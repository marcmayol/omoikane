//! Memory shared through git: a person's (or a group's) machines pool their memories in a git
//! repository of their own, with no service of its own and at no cost. Omoikane only uses the user's `git`, with
//! their credentials.
//!
//! - **What is pushed:** text, never the SQLite. One line per memory (and per tombstone), sorted and
//!   split by project and by machine: `<project>/<machine>.jsonl`, `global/<machine>.jsonl` and
//!   `olvidos/<machine>.jsonl`, where `<machine>` is an opaque name derived from the key (the machine
//!   name, which carries the system's, only goes inside the encrypted lines). Each machine only
//!   writes its own files, so git never has conflicts; on pull only the files that changed since
//!   last time are read. Commits are signed as «omoikane», with no names.
//! - **Encrypted only on the remote:** each line is encrypted with AES-SIV (RFC 5297), which is deterministic:
//!   a line that doesn't change stays the same byte for byte and the git diff is still one line. Project
//!   folders have an opaque name derived from the key. On the machine everything is in plain text.
//!   The repository carries a check file (`omoikane.check`) encrypted with its key: a machine
//!   with another key knows before reading anything.
//! - **Keys with id and rotation (0.20.0):** each line carries the id of the key that encrypted it (`k2:…`;
//!   without prefix, key 1, like all the earlier ones). `omoikane.check` says which keys exist and which is
//!   current. `omoi sync rotate` creates a new key, re-encrypts with it everything from this
//!   machine and gives the new code: whoever only has the old one stops reading what is written from
//!   then on (and `omoi sync status` tells them). A machine keeps every key it has received,
//!   to read what others haven't re-encrypted yet, and always writes with the newest.
//! - **One repository per space:** besides the general one, one per space (`omoi sync init --space
//!   <name>`), each with its key; `omoi sync` and the background sync go through all of them.
//!   A space's memories (its projects and its own) go only to its repository; the general one carries what
//!   belongs to no space (`goes_to_repo`).
//! - **Who wins:** the highest logical clock stamp (`reloj`). The local version that loses stays
//!   in the history with the reason `sync: <machine>`. Tombstones are applied before memories, so
//!   what was forgotten doesn't come back; what another machine forgets is kept here separately (`omoi sync restore`).
//! - **Projects at different paths:** they are recognized by git's `origin` remote (or, without a remote, by
//!   the person and the folder name), and each machine stores the memory with its own path.
//! - **Automatic:** after each change, `programar` launches `omoi sync --background` separately; it waits
//!   a few seconds to batch changes and only one runs at a time per machine.
//! - **Mid-session:** with each message (and in `omoi serve`, with a timer), every 2 minutes
//!   at most, `traer_si_toca` launches `omoi sync --pull` separately: a `git fetch` and a comparison with what was
//!   last read; the full sync, only if the remote brings something. The time of the last attempt
//!   goes in `meta('sync_traido')`.
//!
//! Everything lives next to the database: `sync.json` (configuration), `sync.key` (keys), `sync/` (the clone),
//! `sync-estado.json`, `sync.pendiente`, `sync.hecho` and `sync.lock`. Each space with its own
//! repository, in `sync-spaces/<space>/`: `sync.json`, `sync.key`, `repo/` (its clone) and `estado.json`
//! (the pending marker and the lock are the database's: one sync goes through all of them). With
//! a database not called `omoikane.db`, the names are prefixed with the database's
//! (`trabajo.sync.json`, `trabajo.sync-spaces/`…), so that two databases in the same folder don't
//! share sync.

use crate::embeber::Modelo;
use crate::memoria::{self, COLUMNAS, Cambios, Memoria, NCOL, Nueva, Origen, Tipo, de_fila};
use crate::{reloj, tr};
use aes_siv::KeyInit;
use aes_siv::siv::Aes256Siv;
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Associated data for the encryption: a line of another format or version is not decrypted by accident.
const ASSOCIATED_DATA: &[u8] = b"omoikane-sync-v1";
const BRANCH: &str = "main";
const CODE_PREFIX: &str = "omoi1.";
/// The key check file, at the repository root, and what it contains (encrypted).
const CHECK_FILE: &str = "omoikane.check";
const CHECK_TEXT: &str = "omoikane-key-check-v1";
/// The header of `omoikane.check` when the repository has more than one key.
const CHECK_HEADER: &str = "omoikane-check-v2";
/// Where the space repositories live, next to the database.
const SPACES_DIR: &str = "sync-spaces";
/// What is needed to understand the repository, at its root and encrypted with the current key (like the
/// lines of `omoikane.check`): `{"min_reader": 1}`. The first sync after `init` writes it
/// (and the next one in older repositories) and it is not changed if it already exists; after a rotation it
/// is re-encrypted with the new key, with the same contents.
const REPO_FORMAT_FILE: &str = "omoikane.format";
/// This version's repository reader. A repository whose `min_reader` goes beyond this was
/// written by a version that changed the meaning of something that already existed: this one doesn't write to it (or read)
/// and warns. It only goes up with such a change; what only adds (a field, a line type) doesn't raise it.
pub const LECTOR_REPO: u64 = 1;
/// The lines this version can read (`meta('sync_lector')`). It goes up every time a version
/// learns to read something earlier ones skipped (a new line type): when syncing with a lower
/// `sync_lector`, where each repository was up to (`sync_commit*`) is forgotten and everything is reread,
/// so what was skipped before updating comes in now.
pub const LECTOR_LINEAS: i64 = 1;
/// How long a git command may take before it is considered hung.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

// --- Paths and configuration ----------------------------------------------------------------------

/// The sync files, next to the database.
pub struct Rutas {
    pub config: PathBuf,
    pub clave: PathBuf,
    pub clon: PathBuf,
    pub estado: PathBuf,
    pub pendiente: PathBuf,
    pub hecho: PathBuf,
    pub cerrojo: PathBuf,
    /// This repository's space (`None`: the general one).
    pub espacio: Option<String>,
    /// The `meta` key where the database records the last commit read from this repository.
    pub(crate) marca_commit: String,
}

/// The paths of the general repository.
pub fn rutas(db: &Path) -> Rutas {
    rutas_de(db, None)
}

/// The paths of the general repository (`None`) or of a space's.
pub fn rutas_de(db: &Path, space: Option<&str>) -> Rutas {
    let dir = db.parent().map(Path::to_path_buf).unwrap_or_default();
    let prefix = match db.file_name().map(|n| n.to_string_lossy().into_owned()) {
        Some(n) if n != "omoikane.db" && !n.is_empty() => {
            format!("{}.", db.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n))
        }
        _ => String::new(),
    };
    let f = |name: &str| dir.join(format!("{prefix}{name}"));
    match space {
        None => Rutas {
            config: f("sync.json"),
            clave: f("sync.key"),
            clon: f("sync"),
            estado: f("sync-estado.json"),
            pendiente: f("sync.pendiente"),
            hecho: f("sync.hecho"),
            cerrojo: f("sync.lock"),
            espacio: None,
            marca_commit: "sync_commit".into(),
        },
        Some(e) => {
            let d = f(SPACES_DIR).join(e);
            Rutas {
                config: d.join("sync.json"),
                clave: d.join("sync.key"),
                clon: d.join("repo"),
                estado: d.join("estado.json"),
                pendiente: f("sync.pendiente"),
                hecho: f("sync.hecho"),
                cerrojo: f("sync.lock"),
                espacio: Some(e.to_string()),
                marca_commit: format!("sync_commit:{e}"),
                }
        }
    }
}

/// The spaces with their own repository on this machine, in name order.
pub fn espacios(db: &Path) -> Vec<String> {
    let folder = rutas_de(db, Some("x")).config.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut v: Vec<String> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("sync.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| espacio_valido(n).is_ok())
        .collect();
    v.sort();
    v
}

/// The repository of the space with this key on this machine, if it has one.
pub fn repo_de_espacio(db: &Path, key: &str) -> Option<Rutas> {
    espacios(db).into_iter().find(|e| crate::proyecto::clave_de_espacio(e) == key).map(|e| rutas_de(db, Some(&e)))
}

/// All the repositories configured on this machine: the general one (if it is) and one per space.
pub fn repos(db: &Path) -> Vec<Rutas> {
    let general = rutas(db);
    let mut v = Vec::new();
    if general.config.exists() {
        v.push(general);
    }
    v.extend(espacios(db).iter().map(|e| rutas_de(db, Some(e))));
    v
}

/// A space name as stored (it is a folder name): lowercase, digits,
/// `-` and `_`, up to 64.
pub fn espacio_valido(name: &str) -> Result<String> {
    let n = name.trim().to_lowercase();
    ensure!(
        !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !n.starts_with(['-', '_']),
        tr!(
            "«{}» is not a valid space name (letters, digits, - and _)",
            "«{}» no vale como nombre de espacio (letras, números, - y _)",
            name
        )
    );
    Ok(n)
}

/// What is shared and how. Decided in `omoi sync init` (or `join`). It applies to what is pushed and to
/// what is accepted from the others.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    /// Kinds that are shared (preferences, only if sharing them was chosen).
    pub kinds: Vec<Tipo>,
    /// Whether global memories are shared.
    pub global: bool,
    /// Projects that are shared, by their key; none = all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<String>>,
    /// Only in the general repository: the spaces (by their key) that also go through it. Without this
    /// list, space memories don't go through the general one: each space has its repository (`space`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<String>>,
    /// The name recorded as the author of each memory; none = not recorded.
    #[serde(default)]
    pub author: Option<String>,
    /// To recognize projects without a git remote: the same person's are merged across their
    /// machines, two people's are not.
    pub person: String,
    /// The space whose memories go to this repository (`None`: the general repository). Older
    /// `sync.json` files don't have it: they are the general one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// Another person's general repository (`join --another-person`, 1.8.6): that person also has its
    /// key, so this person's seal key is not derived from it (`verificado::llaves`).
    /// Older `sync.json` files don't have it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub another_person: bool,
}

impl Config {
    /// Whether a memory of this kind is shared (a kind this version doesn't know counts as `fact`),
    /// of this project (`key`) and of this space (`space`: the memory's or its project's).
    /// Which repository each one goes to is decided by `goes_to_repo`.
    fn accepts(&self, kind: &str, key: &str, space: &str) -> bool {
        let kind = kind.parse::<Tipo>().unwrap_or(Tipo::Fact);
        // A preference saved for a whole space (`remember --space`) belongs to the team, not the
        // person: it always goes through that space's repository, even if preferences are not shared.
        let team_preference = kind == Tipo::Preference && key.is_empty() && !space.is_empty() && self.space.is_some();
        if !team_preference && !self.kinds.contains(&kind) && !(kind == Tipo::Procedure && self.kinds.contains(&Tipo::Fact)) {
            return false;
        }
        if key.is_empty() {
            // Space-level ones go with their space; global ones, if sharing them was chosen.
            return !space.is_empty() || self.global;
        }
        self.projects.as_ref().is_none_or(|p| p.iter().any(|k| k == key))
    }

    /// What this machine pushes and what it accepts from the others: what was chosen and belongs to this repository.
    fn admits(&self, kind: &str, key: &str, space: &str) -> bool {
        self.accepts(kind, key, space) && goes_to_repo(self, space)
    }

    fn shares(&self, m: &Memoria) -> bool {
        // A trade memory (global with `trade:`) belongs to the person or the factory: it never goes through a
        // client's repository (being global it wouldn't anyway; this makes it explicit).
        self.admits(m.tipo_texto(), &m.project_key, &m.space) && !(self.space.is_some() && !crate::oficios::de_memoria(m).is_empty())
    }
}

/// Whether a memory of this space (its key; empty: none) goes to this configuration's
/// repository. A space's (`config.space`) carries only that space's: its projects and its
/// own memories, never global ones or those of projects without a space. The general one, what belongs to no space, and
/// of the spaces only those its `spaces` explicitly lists: so a client's memories don't end up in the
/// personal repository by default. The same applies to what arrives: a client repository can't
/// bring memories into other spaces or global ones.
fn goes_to_repo(config: &Config, space: &str) -> bool {
    match &config.space {
        Some(own) => !space.is_empty() && space == crate::proyecto::clave_de_espacio(own),
        None => space.is_empty() || config.spaces.as_ref().is_some_and(|s| s.iter().any(|k| k == space)),
    }
}

/// The general repository's configuration.
pub fn leer_config(db: &Path) -> Result<Option<Config>> {
    leer_config_de(&rutas(db))
}

/// A repository's configuration (the general one or a space's).
pub fn leer_config_de(r: &Rutas) -> Result<Option<Config>> {
    if !r.config.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&r.config)?;
    Ok(Some(serde_json::from_str(&text).with_context(|| format!("{}", r.config.display()))?))
}

/// The keys this machine has for a repository (`sync.key`): one per line, in hex, with its
/// id in front as in the encrypted lines (`k2:<hex>`; without prefix, key 1). The file from before
/// keys with id, a single key in hex, is key 1 and still works as is.
pub(crate) fn leer_claves(r: &Rutas) -> Result<Cifra> {
    let text = std::fs::read_to_string(&r.clave).with_context(|| tr!("can't read the sync key {}", "no se puede leer la clave de sincronización {}", r.clave.display()))?;
    let damaged = || anyhow!(tr!("the sync key is damaged", "la clave de sincronización está dañada"));
    let mut keys = BTreeMap::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        let (id, hex) = split_key_id(l).ok_or_else(damaged)?;
        let k = from_hex(hex).ok_or_else(damaged)?;
        ensure!(k.len() == 64, damaged());
        keys.insert(id, k);
    }
    ensure!(!keys.is_empty(), damaged());
    Ok(Cifra { keys })
}

fn save_keys(r: &Rutas, cipher: &Cifra) -> Result<()> {
    let text: String = cipher.keys.iter().map(|(id, k)| format!("{}{}\n", key_prefix(*id), to_hex(k))).collect();
    write_private(&r.clave, &text)
}

/// Writes a file only the user can read (on unix, created directly with permissions 0600).
fn write_private(path: &Path, text: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, path)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, text)?;
    Ok(())
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Strips user and password from the URLs in a text (`https://token@host/…` → `https://***@host/…`),
/// and the value of the query parameters that sound like a credential (`?access_token=…`,
/// `&private_token=…`, `?sig=…`): to show URLs and errors without showing credentials.
pub fn sin_credenciales(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (before, after) = rest.split_at(i + 3);
        s.push_str(before);
        let url_end = after.find(|c: char| c.is_whitespace() || c == '\'' || c == '"').unwrap_or(after.len());
        let url = &after[..url_end];
        let end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let host_part = &url[..end];
        match host_part.rfind('@') {
            Some(a) => {
                s.push_str("***@");
                s.push_str(&host_part[a + 1..]);
            }
            None => s.push_str(host_part),
        }
        s.push_str(&redact_query(&url[end..]));
        rest = &after[url_end..];
    }
    s.push_str(rest);
    s
}

/// The path and query of a URL, with `***` in the value of each parameter that sounds like a credential.
fn redact_query(rest: &str) -> String {
    let Some(q) = rest.find('?') else { return rest.to_string() };
    let (path_part, query) = rest.split_at(q + 1);
    let (query, fragment) = match query.find('#') {
        Some(f) => query.split_at(f),
        None => (query, ""),
    };
    let is_secret = |name: &str| {
        let n = name.to_lowercase();
        ["token", "secret", "pass", "pwd", "key", "auth", "sig", "credential", "session"].iter().any(|x| n.contains(x))
    };
    let parts: Vec<String> = query
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((n, v)) if is_secret(n) && !v.is_empty() => format!("{n}=***"),
            _ => p.to_string(),
        })
        .collect();
    format!("{path_part}{}{fragment}", parts.join("&"))
}

// --- Encryption -----------------------------------------------------------------------------------

/// The keys this machine has for a repository, by id: it encrypts with the current one (the one with the
/// highest id) and decrypts with whichever each line says.
///
/// An encrypted line is `k<id>:<base64>`, and without prefix it is key 1's: that way the lines of
/// repositories from before keys with id still work, and those written by a machine with only
/// key 1 still come out the same byte for byte. Base64 never contains `:`, so whatever comes before the first
/// `:` is always a label; one that is not `k<number>` is from a future format and the line is skipped
/// as unreadable. The ciphertext always carries the same associated data (`ASSOCIATED_DATA`).
#[derive(Clone)]
pub(crate) struct Cifra {
    keys: BTreeMap<u32, Vec<u8>>,
}

impl Cifra {
    /// The keys, from oldest to current (to derive from them the person's seal key:
    /// `verificado::llaves`).
    pub(crate) fn por_orden(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.keys.values()
    }
}

/// The id and body of a line with a key id (`k2:…`, or without prefix: key 1). `None` if it carries a
/// label that is not a key's.
fn split_key_id(line: &str) -> Option<(u32, &str)> {
    let l = line.trim();
    match l.split_once(':') {
        None => Some((1, l)),
        Some((label, body)) => {
            let id = label.strip_prefix('k')?.parse::<u32>().ok().filter(|n| *n >= 1)?;
            Some((id, body))
        }
    }
}

/// What goes in front of a line of key `id` (nothing for key 1).
fn key_prefix(id: u32) -> String {
    if id == 1 { String::new() } else { format!("k{id}:") }
}

/// What key `id`'s line in `omoikane.check` contains: the id goes inside (except for key 1,
/// which is the usual one), so that one key's line can't pass for another's.
fn check_text(id: u32) -> String {
    if id == 1 { CHECK_TEXT.to_string() } else { format!("{CHECK_TEXT} k{id}") }
}

/// An opaque name derived from a key.
fn opaque_name(key: &[u8], parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(key);
    for p in parts {
        h.update(p);
    }
    to_hex(&h.finalize())[..16].to_string()
}

impl Cifra {
    /// A single key, with its id.
    pub(crate) fn una(id: u32, key: Vec<u8>) -> Cifra {
        Cifra { keys: BTreeMap::from([(id, key)]) }
    }

    /// These keys plus one more (the new one from a rotation, or the one that arrives in a code).
    fn with_key(&self, id: u32, key: Vec<u8>) -> Cifra {
        let mut c = self.clone();
        c.keys.insert(id, key);
        c
    }

    /// The id of the key used to encrypt.
    pub(crate) fn vigente(&self) -> u32 {
        self.keys.keys().next_back().copied().unwrap_or(1)
    }

    /// The ids of the keys there are.
    pub(crate) fn ids(&self) -> Vec<u32> {
        self.keys.keys().copied().collect()
    }

    fn current_key(&self) -> &[u8] {
        self.keys.values().next_back().map(Vec::as_slice).unwrap_or_default()
    }

    fn siv(key: &[u8]) -> Aes256Siv {
        Aes256Siv::new_from_slice(key).expect("clave de 64 bytes")
    }

    /// Encrypts with the current key, with its id in front.
    pub(crate) fn cifrar(&self, text: &str) -> Result<String> {
        self.encrypt_with(self.vigente(), text)
    }

    fn encrypt_with(&self, id: u32, text: &str) -> Result<String> {
        let key = self.keys.get(&id).context("clave")?;
        let c = Cifra::siv(key).encrypt([ASSOCIATED_DATA], text.as_bytes()).map_err(|_| anyhow!("AES-SIV"))?;
        Ok(format!("{}{}", key_prefix(id), base64::engine::general_purpose::STANDARD_NO_PAD.encode(c)))
    }

    /// Decrypts with the key the line says.
    pub(crate) fn descifrar(&self, line: &str) -> Result<String> {
        let bad = || anyhow!(wrong_key_msg());
        let (id, body) = split_key_id(line).ok_or_else(bad)?;
        let Some(key) = self.keys.get(&id) else {
            bail!(tr!(
                "this was encrypted with key k{} and this computer doesn't have it: ask for the current code (`omoi sync code` on a computer that has it) and run `omoi sync join <code>` here",
                "esto va cifrado con la clave k{} y este equipo no la tiene: pide el código vigente (`omoi sync code` en un equipo que la tenga) y aquí `omoi sync join <código>`",
                id
            ));
        };
        let c = base64::engine::general_purpose::STANDARD_NO_PAD.decode(body).map_err(|_| bad())?;
        let t = Cifra::siv(key).decrypt([ASSOCIATED_DATA], &c).map_err(|_| bad())?;
        String::from_utf8(t).map_err(|_| bad())
    }

    /// Opaque name of a project's folder: it doesn't say which project it is to whoever lacks the key.
    fn folder(&self, project_key_: &str) -> String {
        format!("p-{}", opaque_name(self.current_key(), &[project_key_.as_bytes()]))
    }

    /// Opaque name of a machine's file: the machine name (with the system's) is not
    /// visible in the repository.
    fn file(&self, machine: &str) -> String {
        format!("m-{}.jsonl", opaque_name(self.current_key(), &[b"equipo:", machine.as_bytes()]))
    }

    /// The names this machine's files had with its old keys.
    fn old_files(&self, machine: &str) -> Vec<String> {
        let vigente = self.vigente();
        self.keys.iter().filter(|(id, _)| **id != vigente).map(|(_, k)| format!("m-{}.jsonl", opaque_name(k, &[b"equipo:", machine.as_bytes()]))).collect()
    }

    /// Whether key `id`'s line in `omoikane.check` opens with the local key `id`.
    fn opens(&self, id: u32, line: &str) -> bool {
        self.keys.contains_key(&id) && split_key_id(line).is_some_and(|(i, _)| i == id) && self.descifrar(line).is_ok_and(|t| t == check_text(id))
    }
}

/// `omoikane.check`: which keys the repository has and which is current.
///
/// One line per key, encrypted with it and with its id in front like the others (`k2:…`, key 1 with nothing),
/// with `check_text(id)` inside. With more than one key the header
/// `omoikane-check-v2 current=<id>` goes first; with only key 1, the file is the usual one (one line), and
/// older versions still understand it. That way a machine that lacks a key still knows it
/// exists, even if it can't open its line. Whatever isn't understood in the header is ignored (so it
/// can grow), and so is a line with a label that is not a key's.
#[derive(Debug, Default)]
struct KeyCheck {
    lines: BTreeMap<u32, String>,
    /// The current one according to the header (0 if it has none).
    current: u32,
}

impl KeyCheck {
    fn parse(text: &str) -> KeyCheck {
        let mut c = KeyCheck::default();
        for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(rest) = l.strip_prefix("omoikane-check-") {
                for field in rest.split_whitespace() {
                    if let Some(n) = field.strip_prefix("current=").and_then(|n| n.trim_start_matches('k').parse().ok()) {
                        c.current = n;
                    }
                }
            } else if let Some((id, _)) = split_key_id(l) {
                c.lines.insert(id, l.to_string());
            }
        }
        c
    }

    /// The clone's one; `None` if the repository doesn't have it.
    fn read(clone_dir: &Path) -> Option<KeyCheck> {
        std::fs::read_to_string(clone_dir.join(CHECK_FILE)).ok().map(|t| KeyCheck::parse(&t))
    }

    /// The id of the repository's current key.
    fn vigente(&self) -> u32 {
        self.current.max(self.lines.keys().next_back().copied().unwrap_or(0))
    }

    fn texto(&self) -> String {
        let mut t = String::new();
        if self.lines.len() > 1 || self.vigente() > 1 {
            t.push_str(&format!("{CHECK_HEADER} current={}\n", self.vigente()));
        }
        for l in self.lines.values() {
            t.push_str(l);
            t.push('\n');
        }
        t
    }

    /// How many local keys open their line, and which have a line and don't open it (another key with the
    /// same id).
    fn verify(&self, cipher: &Cifra) -> (usize, Vec<u32>) {
        let (mut opened, mut failed) = (0, Vec::new());
        for id in cipher.ids() {
            match self.lines.get(&id) {
                Some(l) if cipher.opens(id, l) => opened += 1,
                Some(_) => failed.push(id),
                None => {}
            }
        }
        (opened, failed)
    }
}

fn wrong_key_msg() -> String {
    tr!(
        "this computer's key is not the repository's: join it with `omoi sync join <code>` (from `omoi sync code` on a computer that already syncs)",
        "la clave de este equipo no es la del repositorio: únete con `omoi sync join <código>` (de `omoi sync code` en un equipo que ya sincroniza)"
    )
}

// --- Line format ----------------------------------------------------------------------------------

/// The fields of a line this version doesn't know (a newer version added them). They are stored
/// with what they come with (`memory.extra`, `verdict.extra`; a tombstone's, in `meta`; those of a
/// memory in the trash, inside it) and written back as is when this machine
/// re-exports that thing, even if it changed it: so a new field isn't lost by going through a
/// machine with an old version. Only those that are not fields this version already knows are re-exported.
pub(crate) type Extra = serde_json::Map<String, serde_json::Value>;

/// The fields of each line type this version knows (plus `type`): anything else goes to `extra`.
const MEMORY_FIELDS: &[&str] = &[
    "type", "id", "project_key", "project_path", "space_key", "kind", "title", "content", "paths", "tags", "pinned", "created_at", "updated_at", "hlc",
    "content_hlc", "author", "machine", "versions",
];
const TOMBSTONE_FIELDS: &[&str] = &["type", "id", "deleted_at", "hlc", "space_key", "machine"];
const VERDICT_FIELDS: &[&str] = &["type", "memory_id", "other_id", "relation", "at", "hlc", "reason", "author", "machine"];
const FORGOTTEN_FIELDS: &[&str] = &["type", "id", "memory", "versions", "hlc", "deleted_at", "by"];
const PROJECT_FIELDS: &[&str] = &["type", "key", "name", "space_key", "hlc", "moved", "machine"];

/// `extra` without what this version already knows (an earlier version that didn't know it stored it).
fn only_unknown(mut extra: Extra, known_fields: &[&str]) -> Extra {
    extra.retain(|k, _| !known_fields.contains(&k.as_str()));
    extra
}

/// `extra` from the JSON stored in the database (`'{}'` or anything not understood: empty).
pub(crate) fn extra_de(text: &str) -> Extra {
    serde_json::from_str(text).unwrap_or_default()
}

/// `extra` as stored in the database.
pub(crate) fn extra_a_texto(extra: &Extra) -> String {
    serde_json::to_string(extra).unwrap_or_else(|_| "{}".into())
}

/// A tombstone's unknown fields go in `meta` (`olvido_extra:<id>`): the `tombstone` table has no
/// column for them and schema 10 is the one 1.0 freezes; only a newer version brings them,
/// so there are almost never any. This is the column with those of tombstone `t` (`'{}'` if none).
pub(crate) const EXTRA_OLVIDO: &str = "COALESCE((SELECT v FROM meta WHERE k = 'olvido_extra:' || t.id), '{}')";

/// Records (or deletes, if there are none) the unknown fields of `id`'s tombstone.
pub(crate) fn guardar_extra_olvido(db: &Connection, id: &str, extra: &Extra) -> Result<()> {
    let k = format!("olvido_extra:{id}");
    if extra.is_empty() {
        db.execute("DELETE FROM meta WHERE k = ?1", [&k])?;
    } else {
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![k, extra_a_texto(extra)])?;
    }
    Ok(())
}

/// The unknown fields of a project's `project` line, in `meta` (`proyecto_extra:<key>`),
/// like the tombstones': the `project` table has no column for them.
fn project_extra(db: &Connection, key: &str) -> Result<Extra> {
    let t: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [format!("proyecto_extra:{key}")], |f| f.get(0)).optional()?;
    Ok(t.map(|t| extra_de(&t)).unwrap_or_default())
}

fn save_project_extra(db: &Connection, key: &str, extra: &Extra) -> Result<()> {
    let k = format!("proyecto_extra:{key}");
    if extra.is_empty() {
        db.execute("DELETE FROM meta WHERE k = ?1", [&k])?;
    } else {
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![k, extra_a_texto(extra)])?;
    }
    Ok(())
}

/// A memory as it travels: in `memory` lines (sync and `omoi export`) and inside
/// `forgotten` ones (the trash, in `omoi export`), so that a memory restored on another machine lands in
/// its project and its space.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CuerpoMemoria {
    pub id: String,
    /// How the project is recognized on another machine (empty: global or space-level).
    pub project_key: String,
    /// The path on the source machine, in case there is no project with that key here yet.
    pub project_path: String,
    /// The space (its key): the memory's if it is space-level (without `project_key`), or its
    /// project's. Versions before 0.20.0 don't send it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space_key: String,
    /// Text: a kind from a future version arrives as is (here it counts as `fact`) and is re-exported the same.
    pub kind: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    pub hlc: String,
    /// The stamp of the last change of title, content or kind (with which verdicts expire),
    /// only if it is not `hlc` (pinning or moving change `hlc` and not this one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hlc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// The machine that wrote the file (for the history: «sync: <machine>»).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
    /// The previous versions (`omoi export` and, since 0.20.0, sync). Older versions
    /// ignore them; whatever already exists with that number stays.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<crate::memoria::Version>,
    /// Fields from a newer version (`Extra`).
    #[serde(flatten)]
    pub extra: Extra,
}

impl CuerpoMemoria {
    /// A database memory as it travels: the project key comes from the `project` table (computed
    /// once), so it is the same through sync and through `omoi export`.
    pub(crate) fn de(m: Memoria, hlc: String, content_hlc: String, machine: &str, versions: Vec<crate::memoria::Version>, mut extra: Extra) -> CuerpoMemoria {
        // The provenance, only what applies to where it is (a global one doesn't carry a client's branch).
        // (The trash one has no `project_id`: it counts as a project's if it has its key or its folder.)
        let in_project = m.project_id != 0 || !m.project_key.is_empty() || !m.project.is_empty();
        crate::procedencia::recortar_extra(&mut extra, i64::from(in_project), &m.tags);
        CuerpoMemoria {
            kind: m.tipo_texto().to_string(),
            id: m.id,
            project_key: m.project_key,
            project_path: m.project,
            space_key: m.space,
            title: m.title,
            content: m.content,
            paths: m.paths,
            tags: m.tags,
            pinned: m.pinned,
            created_at: m.created_at,
            updated_at: m.updated_at,
            content_hlc: if content_hlc == hlc { String::new() } else { content_hlc },
            hlc,
            author: m.author,
            machine: machine.to_string(),
            versions,
            extra: only_unknown(extra, MEMORY_FIELDS),
        }
    }

    /// As stored in the trash (`forgotten.memory`, the usual `Memoria`): whatever has no
    /// place in `Memoria` (the unknown fields) is added by `a_papelera`.
    fn to_memory(&self, source: &str) -> Memoria {
        let parsed_kind = self.kind.parse::<Tipo>().ok();
        Memoria {
            id: self.id.clone(),
            project: self.project_path.clone(),
            project_key: self.project_key.clone(),
            space: self.space_key.clone(),
            kind: parsed_kind.unwrap_or(Tipo::Fact),
            kind_text: if parsed_kind.is_none() { Some(self.kind.clone()) } else { None },
            title: self.title.clone(),
            content: self.content.clone(),
            paths: self.paths.clone(),
            tags: self.tags.clone(),
            pinned: self.pinned,
            source: source.to_string(),
            content_hlc: self.content_hlc.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            author: self.author.clone(),
            ..Default::default()
        }
    }
}

/// The keys the trash adds to the JSON of `Memoria` (which ignores them when reading it): the
/// memory's unknown fields and those of the `forgotten` line.
pub(crate) const PAPELERA_EXTRA: &str = "extra";
pub(crate) const PAPELERA_EXTRA_LINEA: &str = "extra_forgotten";

/// The JSON stored in `forgotten.memory`: the usual `Memoria` (what the trash and
/// `restaurar` read) and, if there are any, the unknown fields.
pub(crate) fn a_papelera(m: &Memoria, extra: &Extra, line_extra: &Extra) -> Result<String> {
    let mut v = serde_json::to_value(m)?;
    if !extra.is_empty() {
        v[PAPELERA_EXTRA] = serde_json::Value::Object(extra.clone());
    }
    if !line_extra.is_empty() {
        v[PAPELERA_EXTRA_LINEA] = serde_json::Value::Object(line_extra.clone());
    }
    Ok(v.to_string())
}

/// What `forgotten.memory` stores: the memory and its unknown fields (the memory's and the
/// line's). `None` if it isn't understood.
pub(crate) fn de_papelera(text: &str) -> Option<(Memoria, Extra, Extra)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = |k: &str| v.get(k).and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let (extra, line_extra) = (object(PAPELERA_EXTRA), object(PAPELERA_EXTRA_LINEA));
    Some((serde_json::from_value(v).ok()?, extra, line_extra))
}

/// The memory of a `forgotten` line: the shape of `CuerpoMemoria` (since 0.20.0) or the `Memoria`
/// of the JSON API that 0.18.0 and 0.19.0 wrote (without `project_path`; with `project`, the path).
/// Those from back then don't carry their stamp inside: it stays empty and the line's counts.
fn cuerpo_olvidado<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<CuerpoMemoria, D::Error> {
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    if v.get("project_path").is_some() {
        return serde_json::from_value(v).map_err(D::Error::custom);
    }
    let m: Memoria = serde_json::from_value(v).map_err(D::Error::custom)?;
    Ok(CuerpoMemoria::de(m, String::new(), String::new(), "", Vec::new(), Extra::new()))
}

/// Each line of sync and of `omoi export`, in JSON with its type in `type`.
///
/// Rules so that what comes after 1.0 doesn't break whoever stays on it: a new field is
/// optional and kept by whoever doesn't understand it (`Extra`); a new type is skipped by whoever doesn't understand it,
/// who counts it as «from a newer version» and rereads it on updating (`LECTOR`); whatever
/// changes the meaning of what already exists raises `min_reader` in `omoikane.format` (or the version of the
/// `omoi export` format), and then old versions don't write to that repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Linea {
    Memory(CuerpoMemoria),
    Tombstone {
        id: String,
        deleted_at: String,
        hlc: String,
        /// The space of what was forgotten (its key), with which the tombstone goes only to its repository. Empty:
        /// none, or from a version that doesn't send it (through a space's repository, that one's).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        space_key: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// A verdict (`veredictos`), in the judged memory's folder. Versions before
    /// 0.17.0 don't understand it and skip it; those before 0.19.0 ignore `reason` and `author`.
    Verdict {
        memory_id: String,
        #[serde(default)]
        other_id: String,
        relation: String,
        at: String,
        hlc: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        author: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// A memory in the trash, with its history: only in `omoi export` (a copy without the trash
    /// wouldn't be faithful). `memory` has the shape of a `memory` line (`CuerpoMemoria`), with its project
    /// and its space by key.
    Forgotten {
        id: String,
        #[serde(deserialize_with = "cuerpo_olvidado")]
        memory: CuerpoMemoria,
        #[serde(default)]
        versions: Vec<crate::memoria::Version>,
        hlc: String,
        deleted_at: String,
        #[serde(default)]
        by: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// A project's space (since 1.0): each machine writes one per project whose space has
    /// ever changed (`project.hlc`), only in the repository that carries that space (the
    /// general one, those that belong to none). In the general one, the line of a project that is in a space doesn't
    /// name it: it goes with `moved` and, as `key`, the project's opaque name (`Cifra::folder`), without
    /// name or space; it only says «another machine moved it to a space».
    ///
    /// It never changes the space of anything: whoever has a repository's key can write it (the
    /// person and the machine are in plain text). If it says a different space than the local one and is newer than what
    /// this machine knew about the project, the project is paused (`proyecto::Pausa`): nothing of it is pushed or
    /// accepted until the person decides here. The worst it can do is pause, never
    /// move a client's memories to another repository.
    Project {
        key: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
        /// The space (its key); empty: none.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        space_key: String,
        hlc: String,
        /// Only in the general one: the project is in a space the general one doesn't carry (`key` is opaque).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        moved: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// First line of an `omoi export` file.
    Header {
        format: String,
        version: u32,
        schema: i64,
        exported_at: String,
        machine: String,
        #[serde(default)]
        encrypted: bool,
        /// The local person of the exporting database (the one in its `dir:` keys): an import into a
        /// freshly created database adopts it, like someone restoring their database on a new machine.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        person: String,
    },
    /// Last line of an `omoi export` file: how many lines are in between and their SHA-256.
    End {
        count: usize,
        sha256: String,
    },
}

/// The line types this version understands.
const LINE_TYPES: &[&str] = &["memory", "tombstone", "verdict", "forgotten", "project", "header", "end"];

/// Whether a text is a line of a type this version doesn't know: JSON with a `type` that is not one of
/// its own (a newer version wrote it). The others that aren't understood are damaged.
pub(crate) fn de_tipo_nuevo(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(|t| !LINE_TYPES.contains(&t)))
        .unwrap_or(false)
}

impl Linea {
    /// The id the line affects (empty in the header and the end).
    pub(crate) fn id(&self) -> &str {
        match self {
            Linea::Memory(c) => &c.id,
            Linea::Tombstone { id, .. } | Linea::Forgotten { id, .. } => id,
            Linea::Verdict { memory_id, .. } => memory_id,
            Linea::Project { key, .. } => key,
            Linea::Header { .. } | Linea::End { .. } => "",
        }
    }

    /// Whether the ids it carries look like the ones Omoikane writes.
    pub(crate) fn valida(&self) -> bool {
        match self {
            Linea::Header { .. } | Linea::End { .. } => true,
            Linea::Verdict { memory_id, other_id, .. } => valid_id(memory_id) && (other_id.is_empty() || valid_id(other_id)),
            Linea::Forgotten { id, memory, .. } => valid_id(id) && memory.id == *id,
            Linea::Project { key, hlc, .. } => !key.trim().is_empty() && key.len() <= 1024 && !hlc.is_empty(),
            other => valid_id(other.id()),
        }
    }

    pub(crate) fn machine(&self) -> &str {
        match self {
            Linea::Memory(c) => &c.machine,
            Linea::Tombstone { machine, .. } | Linea::Verdict { machine, .. } | Linea::Project { machine, .. } => machine,
            _ => "",
        }
    }

    /// A tombstone as it travels, with its space and its unknown fields.
    pub(crate) fn olvido(id: String, deleted_at: String, hlc: String, space_key: String, machine: &str, extra: Extra) -> Linea {
        Linea::Tombstone { id, deleted_at, hlc, space_key, machine: machine.to_string(), extra: only_unknown(extra, TOMBSTONE_FIELDS) }
    }

    /// A verdict as it travels, with its unknown fields.
    pub(crate) fn veredicto(v: crate::veredictos::Veredicto, machine: &str) -> Linea {
        Linea::Verdict {
            memory_id: v.memory_id,
            other_id: v.other_id,
            relation: v.relation,
            at: v.at,
            hlc: v.hlc,
            reason: v.reason,
            author: v.author,
            machine: machine.to_string(),
            extra: only_unknown(extra_de(&v.extra), VERDICT_FIELDS),
        }
    }

    /// A memory in the trash as it goes in `omoi export`: `text` is what `forgotten.memory` stores.
    /// Without its project's key (forgotten before schema 10), it is looked up by its path in the
    /// `project` table. `None` if it isn't understood.
    pub(crate) fn papelera(db: &Connection, id: String, text: &str, versions: Vec<crate::memoria::Version>, hlc: String, deleted_at: String, by: String) -> Result<Option<Linea>> {
        let Some((mut m, extra, line_extra)) = de_papelera(text) else { return Ok(None) };
        if m.project_key.is_empty()
            && !m.project.is_empty()
            && let Some((p, _)) = crate::proyecto::resolver(db, &m.project)?
        {
            m.project_key = crate::proyecto::clave_de(db, p)?;
            if m.space.is_empty() {
                m.space = crate::proyecto::clave_espacio(db, crate::proyecto::espacio_de(db, p)?)?;
            }
        }
        let content_hlc = if m.content_hlc.is_empty() { hlc.clone() } else { m.content_hlc.clone() };
        // Without `machine` inside: who forgot it is told by `by`, and the exporting machine, by the header.
        let memory = CuerpoMemoria::de(m, hlc.clone(), content_hlc, "", Vec::new(), extra);
        Ok(Some(Linea::Forgotten { id, memory, versions, hlc, deleted_at, by, extra: only_unknown(line_extra, FORGOTTEN_FIELDS) }))
    }
}

/// An id like the ones Omoikane writes (ULID: 26 uppercase letters and digits). A line with another
/// id is not accepted.
fn valid_id(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

// --- Projects -------------------------------------------------------------------------------------

/// A project's key to recognize it on another machine: git's `origin` remote, normalized
/// (`git:github.com/owner/repo`), or, without a remote, the person and the folder name
/// (`dir:<person>/<folder>`), so two people with a `web` folder don't get mixed up.
pub fn clave(project: &str, person: &str) -> String {
    if project.is_empty() {
        return String::new();
    }
    if let Some(url) = origen_git(Path::new(project)) {
        return format!("git:{}", normalizar_remoto(&url));
    }
    let path = Path::new(project);
    // A worktree without `origin` takes its repository's name, not its folder's (1.8.0): so its
    // worktrees (and other machines') give the same key. Keys already stored don't change.
    let name = crate::proyecto::nombre_de_worktree(path)
        .unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_else(|| project.to_lowercase()));
    format!("dir:{person}/{name}")
}

/// The URL of the `origin` remote, reading `.git/config` (fast; in a worktree, the repository's).
pub(crate) fn origen_git(root: &Path) -> Option<String> {
    // Local, or on the network share of the folder being worked in (`proyecto::legible`).
    if !crate::proyecto::legible(root) {
        return None;
    }
    let dot_git = root.join(".git");
    let config = if dot_git.is_dir() {
        dot_git.join("config")
    } else {
        // Worktree: `.git` is a file «gitdir: <repo>/.git/worktrees/<name>» (never on another network
        // share).
        let t = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = crate::proyecto::si_legible(root.join(t.trim().strip_prefix("gitdir:")?.trim()))?;
        let common_dir = std::fs::read_to_string(gitdir.join("commondir")).ok().map(|c| gitdir.join(c.trim())).unwrap_or(gitdir);
        crate::proyecto::si_legible(common_dir)?.join("config")
    };
    let text = std::fs::read_to_string(config).ok()?;
    let mut in_origin = false;
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_origin = l == "[remote \"origin\"]";
        } else if in_origin && let Some(rest) = l.strip_prefix("url") {
            return Some(rest.trim_start_matches([' ', '\t', '=']).trim().to_string());
        }
    }
    None
}

pub(crate) fn normalizar_remoto(url: &str) -> String {
    let mut u = url.trim().to_lowercase();
    for p in ["https://", "http://", "ssh://", "git://", "git+ssh://"] {
        if let Some(r) = u.strip_prefix(p) {
            u = r.to_string();
        }
    }
    if let Some((_, r)) = u.split_once('@') {
        u = r.to_string();
    }
    // `host:owner/repo` (scp) and `host/owner/repo` are the same; a port (`host:22/…`) is removed.
    if let Some((host, rest)) = u.split_once(':') {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('/');
        u = format!("{host}/{rest}");
    }
    u.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// A person as part of a key: lowercase, letters and digits (empty if nothing is left).
pub fn persona(name: &str) -> String {
    let p: String = name.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    p.trim_matches('-').to_string()
}

/// A project's folder gets recorded: if its key is that of a project that came from another machine
/// (or that of a clone, a worktree or a moved folder with the same `origin`), its memories are already
/// local, without moving them (they go by the project id). Returns how many memories that project had if the
/// folder wasn't recorded (0 if it already was or it is new).
pub fn adoptar(db: &Connection, project: &str) -> Result<usize> {
    if project.is_empty() || crate::proyecto::es_general(project) || crate::proyecto::apuntada(db, project)? {
        return Ok(0);
    }
    let id = crate::proyecto::registrar(db, project)?;
    Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [id], |f| f.get::<_, i64>(0))? as usize)
}

// --- git ------------------------------------------------------------------------------------------

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// No windows on Windows (in the background there is no console; each command would open one).
fn no_window(c: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Whether the user already says how to use ssh (`GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand`: plink, a
/// specific key…). Then it is respected; if not, ssh runs in no-questions mode.
fn user_ssh_config() -> bool {
    static USER_SSH: OnceLock<bool> = OnceLock::new();
    *USER_SSH.get_or_init(|| {
        if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
            return true;
        }
        let mut c = std::process::Command::new("git");
        c.args(["config", "--get", "core.sshCommand"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        no_window(&mut c);
        c.output().is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
    })
}

/// Kills git and whatever it launched (git-remote-https, ssh…), not just the git process.
fn kill_tree(h: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut k = std::process::Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &h.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        no_window(&mut k);
        let _ = k.status();
    }
    #[cfg(unix)]
    {
        // git runs in its own process group: the whole group is killed.
        // SAFETY: kill with a negative group pid only sends the signal to that group.
        unsafe {
            libc::kill(-(h.id() as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = h.kill();
    let _ = h.wait();
}

/// Reads an output in a thread, keeping what arrives as it arrives: if a git grandchild (an ssh
/// with `ControlPersist`) leaves it open, we can carry on without waiting for it to close.
fn spawn_reader(mut r: Box<dyn Read + Send>) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let data = Arc::new(Mutex::new(Vec::new()));
    let end = Arc::new(AtomicBool::new(false));
    let (d, f) = (data.clone(), end.clone());
    std::thread::spawn(move || {
        let mut b = [0u8; 8192];
        loop {
            match r.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(n) => d.lock().map(|mut v| v.extend_from_slice(&b[..n])).unwrap_or(()),
            }
        }
        f.store(true, Ordering::Relaxed);
    });
    (data, end)
}

/// Runs git in the clone. No prompts or windows (in the background there is nobody to answer),
/// without the hooks or the line-ending conversions of the user's configuration (the clone is
/// Omoikane's, and a hook that rejected encrypted text would jam it), and with a time limit for the whole
/// command, also for reading what it writes.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut c = std::process::Command::new("git");
    c.arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .args(["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if !user_ssh_config() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    no_window(&mut c);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut h = c.spawn().with_context(|| tr!("can't run git: is it installed?", "no se puede ejecutar git: ¿está instalado?"))?;
    let (stdout_buf, stdout_done) = spawn_reader(Box::new(h.stdout.take().expect("stdout")));
    let (stderr_buf, stderr_done) = spawn_reader(Box::new(h.stderr.take().expect("stderr")));
    let deadline = Instant::now() + GIT_TIMEOUT;
    let timed_out = || anyhow!("git {}: {}", args.first().unwrap_or(&""), tr!("no answer after {} s", "sin respuesta tras {} s", GIT_TIMEOUT.as_secs()));
    let status = loop {
        if let Some(e) = h.try_wait()? {
            break e;
        }
        if Instant::now() > deadline {
            kill_tree(&mut h);
            return Err(timed_out());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Whatever is left to read: until the outputs close, with a short grace period (a grandchild that
    // leaves them open must not hang the sync) and never beyond the deadline.
    let grace = (Instant::now() + Duration::from_secs(2)).min(deadline);
    while !(stdout_done.load(Ordering::Relaxed) && stderr_done.load(Ordering::Relaxed)) && Instant::now() < grace {
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = stdout_buf.lock().map(|v| v.clone()).unwrap_or_default();
    let err = stderr_buf.lock().map(|v| v.clone()).unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        // The command also without credentials: the `clone` one carries the URL, maybe with a CI token.
        bail!("git {}: {}", sin_credenciales(&args.join(" ")), sin_credenciales(String::from_utf8_lossy(&err).trim()))
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn remote_has_branch(clone_dir: &Path) -> Result<bool> {
    Ok(!git(clone_dir, &["ls-remote", "--heads", "origin", BRANCH])?.trim().is_empty())
}

fn has_head(clone_dir: &Path) -> bool {
    git(clone_dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// Clones the repository (empty or not), leaves the `main` branch and a neutral identity of its own in the clone:
/// commits don't depend on the user having git configured and don't expose their name.
fn clone_repo(url: &str, clone_dir: &Path) -> Result<()> {
    if clone_dir.exists() {
        std::fs::remove_dir_all(clone_dir).with_context(|| format!("{}", clone_dir.display()))?;
    }
    // Git is launched from the parent folder: with a relative destination it would look for it inside it.
    let clone_dir = &std::path::absolute(clone_dir)?;
    let parent = clone_dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let dest = clone_dir.to_string_lossy().into_owned();
    git(parent, &["clone", "--quiet", "--", url, &dest]).with_context(|| tr!("can't clone {}", "no se puede clonar {}", sin_credenciales(url)))?;
    if remote_has_branch(clone_dir)? {
        git(clone_dir, &["checkout", "--quiet", "--force", "-B", BRANCH, &format!("origin/{BRANCH}")])?;
    } else {
        git(clone_dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{BRANCH}")])?;
    }
    git(clone_dir, &["config", "user.name", "omoikane"])?;
    git(clone_dir, &["config", "user.email", "omoikane@localhost"])?;
    git(clone_dir, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

/// Leaves the clone like the remote (or like its last commit): this machine's memories are rewritten from
/// the database on each sync, so a half-done commit or index from an earlier time (a
/// process that died, a commit that failed) doesn't jam anything.
fn reset_clone(clone_dir: &Path) -> Result<()> {
    if has_head(clone_dir) {
        git(clone_dir, &["reset", "--quiet", "--hard", "HEAD"])?;
    }
    git(clone_dir, &["clean", "--quiet", "-fdx"])?;
    if remote_has_branch(clone_dir)? {
        git(clone_dir, &["fetch", "--quiet", "origin", BRANCH])?;
        git(clone_dir, &["checkout", "--quiet", "--force", "-B", BRANCH, &format!("origin/{BRANCH}")])?;
    }
    Ok(())
}

/// The repository's fixed files: no text conversions, and the key check.
fn write_fixed_files(clone_dir: &Path, cipher: &Cifra) -> Result<()> {
    let attributes = clone_dir.join(".gitattributes");
    if !attributes.exists() {
        std::fs::write(attributes, "* -text\n")?;
    }
    let before = std::fs::read_to_string(clone_dir.join(CHECK_FILE)).ok();
    let mut ch = before.as_deref().map(KeyCheck::parse).unwrap_or_default();
    let (opened, failed) = ch.verify(cipher);
    if !failed.is_empty() || (opened == 0 && !ch.lines.is_empty()) {
        bail!(wrong_key_msg());
    }
    // The local keys the repository doesn't list yet: a new repository's, the new one from a
    // rotation, or one whose push was lost.
    for id in cipher.ids() {
        if let std::collections::btree_map::Entry::Vacant(e) = ch.lines.entry(id) {
            e.insert(cipher.encrypt_with(id, &check_text(id))?);
        }
    }
    ch.current = ch.vigente();
    let text = ch.texto();
    if before.as_deref() != Some(text.as_str()) {
        std::fs::write(clone_dir.join(CHECK_FILE), text)?;
    }
    // `omoikane.format`: if it's missing, this version's; if it is encrypted with a key older than the
    // local current one (a rotation), the same contents with the current one. Whatever can't be opened is not
    // touched.
    let format_file = clone_dir.join(REPO_FORMAT_FILE);
    match std::fs::read_to_string(&format_file) {
        Err(_) => std::fs::write(&format_file, cipher.cifrar(&serde_json::json!({ "min_reader": LECTOR_REPO }).to_string())? + "\n")?,
        Ok(t) => {
            if let Some((id, _)) = split_key_id(t.trim())
                && id < cipher.vigente()
                && let Ok(plain) = cipher.descifrar(t.trim())
            {
                std::fs::write(&format_file, cipher.cifrar(&plain)? + "\n")?;
            }
        }
    }
    Ok(())
}

/// The `min_reader` of `omoikane.format` in the clone, if it has one and this machine can open it.
fn required_reader(clone_dir: &Path, cipher: &Cifra) -> Option<u64> {
    let t = std::fs::read_to_string(clone_dir.join(REPO_FORMAT_FILE)).ok()?;
    let plain = cipher.descifrar(t.trim()).ok()?;
    serde_json::from_str::<serde_json::Value>(&plain).ok()?.get("min_reader")?.as_u64()
}

/// Whether the repository asks for a newer version of omoi than this one (according to its clone, offline): the
/// `min_reader` it asks for.
pub fn necesita_version(r: &Rutas) -> Option<u64> {
    let cipher = leer_claves(r).ok()?;
    required_reader(&r.clon, &cipher).filter(|n| *n > LECTOR_REPO)
}

/// The notice for `necesita_version`.
pub fn aviso_version() -> String {
    tr!(
        "this repository needs a newer version of omoi: update it (`omoi upgrade`); until then nothing is written to it",
        "este repositorio necesita una versión más nueva de omoi: actualízalo (`omoi upgrade`); hasta entonces no se escribe nada en él"
    )
}

/// After a rotation, this machine's files with an old key's name: its memories are rewritten
/// with the current one (and with the current one's name), so the old ones are deleted. Otherwise,
/// whoever only has the old key would keep reading what this machine wrote before.
fn remove_old_files(clone_dir: &Path, machine: &str, cipher: &Cifra) -> Result<()> {
    let old_names = cipher.old_files(machine);
    if old_names.is_empty() {
        return Ok(());
    }
    for e in std::fs::read_dir(clone_dir)?.flatten() {
        if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
            continue;
        }
        for n in &old_names {
            let f = e.path().join(n);
            if f.exists() {
                std::fs::remove_file(f)?;
            }
        }
    }
    Ok(())
}

/// Whether the repository already encrypts with a key this machine doesn't have (another rotated it): whatever is written
/// from then on can't be read here. It looks at the clone from the last sync (offline).
pub fn clave_cambiada(r: &Rutas) -> bool {
    let Ok(cipher) = leer_claves(r) else { return false };
    KeyCheck::read(&r.clon).is_some_and(|c| c.vigente() > 0 && !cipher.keys.contains_key(&c.vigente()))
}

/// The notice for `clave_cambiada`.
pub fn aviso_clave_cambiada() -> String {
    tr!(
        "this repository's key has changed: ask for the new code (`omoi sync code` on a computer that has it) and run `omoi sync join <code>` here",
        "la clave de este repositorio ha cambiado: pide el código nuevo (`omoi sync code` en un equipo que la tenga) y aquí `omoi sync join <código>`"
    )
}

/// The ids of the keys this machine has for a repository and that of the repository's current one
/// (according to its clone; 0 if not known yet).
pub fn claves_de(r: &Rutas) -> (Vec<u32>, u32) {
    let ids = leer_claves(r).map(|c| c.ids()).unwrap_or_default();
    (ids, KeyCheck::read(&r.clon).map_or(0, |c| c.vigente()))
}

// --- Export and import ----------------------------------------------------------------------------

/// All memories, with their stamp (`hlc`), that of their last text change (`content_hlc`) and their
/// unknown fields (`extra`).
pub(crate) fn todas(db: &Connection) -> Result<Vec<(Memoria, String, String, Extra)>> {
    let mut q = db.prepare(&format!("SELECT {COLUMNAS}, m.hlc, m.content_hlc, m.extra FROM memory m ORDER BY m.id"))?;
    let v = q
        .query_map([], |f| Ok((de_fila(f)?, f.get::<_, String>(NCOL)?, f.get::<_, String>(NCOL + 1)?, extra_de(&f.get::<_, String>(NCOL + 2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// A memory's line, for sync and `omoi export`.
pub(crate) fn linea_de(m: Memoria, hlc: String, content_hlc: String, machine: &str, versions: Vec<crate::memoria::Version>, extra: Extra) -> Linea {
    Linea::Memory(CuerpoMemoria::de(m, hlc, content_hlc, machine, versions, extra))
}

/// What this machine would push (for the summary of `init` and `join`): global ones, project ones and how many
/// projects.
pub fn resumen_de_subida(db: &Connection, config: &Config) -> Result<(usize, usize, usize)> {
    let (mut globals, mut in_projects, mut projects) = (0, 0, HashSet::new());
    for (m, _, _, _) in todas(db)? {
        if config.shares(&m) {
            if m.project_key.is_empty() {
                globals += 1;
            } else {
                in_projects += 1;
                projects.insert(m.project_key);
            }
        }
    }
    Ok((globals, in_projects, projects.len()))
}

/// Writes this machine's files into the clone and deletes its own that no longer apply. Returns how many
/// memories it exported.
fn export(db: &Connection, clone_dir: &Path, machine: &str, config: &Config, cipher: &Cifra) -> Result<usize> {
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut n = 0;
    // The folder of each shared memory: its verdicts go with it.
    let mut folders: HashMap<String, String> = HashMap::new();
    // Everyone's history, in one query (one per memory was 50,000 with a large database).
    let mut histories = crate::memoria::historiales(db)?;
    // What a version before 1.8.0 moved from a client to a trade with its id doesn't carry its
    // history (it was the client's text). Since that now creates a new memory, none is recorded any more.
    for id in crate::oficios::historiales_solo_aqui(db)? {
        histories.remove(&id);
    }
    // A paused project's memories go to no repository until the person decides.
    let paused_keys: HashSet<String> = {
        let mut q = db.prepare("SELECT key FROM project WHERE paused != ''")?;
        q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for (m, hlc, content_hlc, extra) in todas(db)? {
        if !config.shares(&m) || (!m.project_key.is_empty() && paused_keys.contains(&m.project_key)) {
            continue;
        }
        let folder = if !m.project_key.is_empty() {
            cipher.folder(&m.project_key)
        } else if !m.space.is_empty() {
            cipher.folder(&format!("space:{}", m.space))
        } else {
            "global".to_string()
        };
        folders.insert(m.id.clone(), folder.clone());
        // The history travels with the memory: whoever joins a repository (a colleague, when moving a
        // personal project to a space) gets it with its earlier versions.
        let versions = histories.remove(&m.id).unwrap_or_default();
        // Second layer: what was saved before 0.19.0 (or with `OMOIKANE_REDACT=0` at some other time)
        // doesn't leave the machine with its secret, neither in the memory nor in its versions. Here it stays as
        // it is, with its notice in Review.
        let (m, versions) = if crate::secretos::activo() {
            let redact = |t: &str| crate::secretos::redactar(t).0;
            let versions = versions.into_iter().map(|v| crate::memoria::Version { title: redact(&v.title), content: redact(&v.content), ..v }).collect();
            (Memoria { title: redact(&m.title), content: redact(&m.content), ..m }, versions)
        } else {
            (m, versions)
        };
        let l = linea_de(m, hlc, content_hlc, machine, versions, extra);
        files.entry(folder).or_default().push(cipher.cifrar(&serde_json::to_string(&l)?)?);
        n += 1;
    }
    for v in crate::veredictos::todos(db)? {
        // Only if both memories travel: a verdict about something the other machine doesn't have is useless.
        let (Some(folder), true) = (folders.get(&v.memory_id), v.other_id.is_empty() || folders.contains_key(&v.other_id)) else {
            continue;
        };
        let l = Linea::veredicto(v, machine);
        files.entry(folder.clone()).or_default().push(cipher.cifrar(&serde_json::to_string(&l)?)?);
    }
    // The space of each project that ever changed it (`project.hlc`), in the repository that
    // carries it. In the general one, of those in a space it doesn't carry, only that they moved (without
    // naming them): so the machine that still has it without a space pauses it, without the general one saying which
    // client it belongs to.
    {
        let mut q = db.prepare(
            "SELECT p.key, p.name, COALESCE((SELECT key FROM space WHERE id = p.space_id), ''), p.hlc FROM project p
             WHERE p.id != 0 AND p.key != '' AND p.hlc != '' ORDER BY p.key",
        )?;
        let projects: Vec<(String, String, String, String)> =
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        for (key, name, space, hlc) in projects {
            if config.projects.as_ref().is_some_and(|p| !p.contains(&key)) {
                continue;
            }
            let l = if goes_to_repo(config, &space) {
                let extra = only_unknown(project_extra(db, &key)?, PROJECT_FIELDS);
                Linea::Project { key, name, space_key: space, hlc, moved: false, machine: machine.to_string(), extra }
            } else if config.space.is_none() {
                Linea::Project { key: cipher.folder(&key), name: String::new(), space_key: String::new(), hlc, moved: true, machine: machine.to_string(), extra: Extra::new() }
            } else {
                continue;
            };
            files.entry("proyectos".into()).or_default().push(cipher.cifrar(&serde_json::to_string(&l)?)?);
        }
    }
    {
        let mut q = db.prepare(&format!("SELECT t.id, t.deleted_at, t.hlc, t.space_key, {EXTRA_OLVIDO} FROM tombstone t ORDER BY t.id"))?;
        let tombstones: Vec<(String, String, String, String, String)> =
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, deleted_at, hlc, space, extra) in tombstones {
            // Each tombstone, only to its space's repository: the one the memory had when it was forgotten (or the one
            // of the repository it arrived through). Without a space (or from before schema 10), to the general one.
            if !goes_to_repo(config, &space) {
                continue;
            }
            let l = Linea::olvido(id, deleted_at, hlc, space, machine, extra_de(&extra));
            files.entry("olvidos".into()).or_default().push(cipher.cifrar(&serde_json::to_string(&l)?)?);
        }
    }
    let name = cipher.file(machine);
    // This machine's files that no longer have anything are deleted.
    for e in std::fs::read_dir(clone_dir)?.flatten() {
        let folder = e.file_name().to_string_lossy().into_owned();
        if folder.starts_with('.') || !e.path().is_dir() {
            continue;
        }
        let f = e.path().join(&name);
        if f.exists() && !files.contains_key(&folder) {
            std::fs::remove_file(&f)?;
        }
    }
    for (folder, mut lines) in files {
        // Sorted (by ciphertext, which is deterministic): the same contents give the same file.
        lines.sort();
        let dir = clone_dir.join(&folder);
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(&name);
        let text = lines.join("\n") + "\n";
        if std::fs::read_to_string(&dest).ok().as_deref() != Some(text.as_str()) {
            let tmp = dir.join(format!(".{name}.tmp"));
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &dest)?;
        }
    }
    Ok(n)
}

/// If the database last synced with a line reader older than this version's
/// (`LECTOR_LINEAS`; unrecorded, 0), forgets where it was in every repository to reread
/// everything, and records the current one. Returns whether it did.
fn reread_if_needed(db: &Connection) -> Result<bool> {
    let v: i64 = db
        .query_row("SELECT v FROM meta WHERE k = 'sync_lector'", [], |f| f.get::<_, String>(0))
        .optional()?
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if v >= LECTOR_LINEAS {
        return Ok(false);
    }
    db.execute("DELETE FROM meta WHERE k = 'sync_commit' OR substr(k, 1, 12) = 'sync_commit:'", [])?;
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sync_lector', ?1)", [LECTOR_LINEAS.to_string()])?;
    Ok(true)
}

/// Which files from other machines must be read: those that changed since the last sync, or
/// all of them if it is the first (or if the database doesn't remember where it was).
fn files_to_read(db: &Connection, clone_dir: &Path, machine: &str, cipher: &Cifra, mark: &str) -> Result<Vec<PathBuf>> {
    let own = cipher.file(machine);
    let last: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [mark], |f| f.get(0)).optional()?;
    let changed: Option<Vec<String>> = match last {
        Some(u) => git(clone_dir, &["diff", "--name-only", &u, "HEAD"]).ok().map(|s| s.lines().map(String::from).collect()),
        None => None,
    };
    let mut v: Vec<PathBuf> = match changed {
        Some(list) => list.into_iter().map(|f| clone_dir.join(f)).filter(|p| p.exists()).collect(),
        None => {
            let mut all = Vec::new();
            for e in std::fs::read_dir(clone_dir)?.flatten() {
                if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(e.path())?.flatten() {
                    all.push(f.path());
                }
            }
            all
        }
    };
    v.retain(|p| p.extension().is_some_and(|x| x == "jsonl") && p.file_name().is_some_and(|n| n.to_string_lossy() != own));
    v.sort();
    Ok(v)
}

#[derive(Debug, Default, Serialize)]
pub struct Resumen {
    pub equipo: String,
    pub exportadas: usize,
    pub leidos: usize,
    pub nuevas: usize,
    pub actualizadas: usize,
    pub olvidadas: usize,
    /// Lines from other machines that couldn't be decrypted or are not valid (they are skipped).
    pub ilegibles: usize,
    /// Lines of a type (or with an encryption label) this version doesn't know: a newer omoi
    /// wrote them. They are skipped and reread on updating (`LECTOR_LINEAS`).
    pub de_version_nueva: usize,
    /// Lines rejected because they touch something that here belongs to another space (a project, a memory, those
    /// of a verdict or a tombstone) or arrive through a repository that doesn't carry their space: nothing from one
    /// client gets into another or changes space because of what arrives. They are not applied.
    pub conflictos_de_espacio: usize,
    /// Lines of memories of a project paused here (`proyecto::Pausa`): they are not applied until the
    /// person decides, and then they are reread.
    pub en_pausa: usize,
    /// Memories that arrived through a space's repository with another person's verified
    /// seal (see `foreign_seal`): here they don't count as verified.
    pub sellos_ajenos: usize,
    /// Memories with a key (`--key`) that another machine also created without seeing it: they become superseded by
    /// the one that rules (`memoria::resolver_claves_repetidas`).
    #[serde(rename = "duplicate_keys")]
    pub claves_repetidas: usize,
    /// Per file read in this pass (path inside the repository), how many bad lines it had.
    #[serde(skip)]
    pub malos: BTreeMap<String, usize>,
    /// The same with those from a newer version.
    #[serde(skip)]
    pub futuros: BTreeMap<String, usize>,
    /// The same with those rejected for being from another space.
    #[serde(skip)]
    pub conflictos: BTreeMap<String, usize>,
    /// The same with those of a paused project.
    #[serde(skip)]
    pub pausadas: BTreeMap<String, usize>,
    /// Per file read, the projects that are in another space there (`Choque`).
    #[serde(skip)]
    pub choques: BTreeMap<String, BTreeSet<Choque>>,
    /// Those of the line being applied (`import` moves them to `choques`, with their file).
    #[serde(skip)]
    pendientes: Vec<Choque>,
    /// What is worth saying: per project, those in another space on another machine with the command
    /// that fixes it, and those paused with the two commands that decide it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub avisos: Vec<String>,
}

/// A project that here is in one space and on another machine in another: what arrives from it is not applied.
/// It is reported with the command that fixes it, and disappears as soon as both agree: files with
/// any are reread on every sync.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Choque {
    pub key: String,
    pub name: String,
    /// Its space here (empty: none).
    pub here: String,
    /// The one the line brings (empty: none).
    pub there: String,
    /// The machine that has it that way.
    pub machine: String,
}

/// A project name as a command argument (quoted if needed).
fn shell_arg(n: &str) -> String {
    if !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || "-_./:@".contains(c)) { n.to_string() } else { format!("\"{}\"", n.replace('"', "\\\"")) }
}

impl Choque {
    /// The command that fixes it: the one that leaves the project here where the other machine has it, unless the
    /// other one still has it without a space (through the general one) and here it is in one: then, the one to
    /// run on that machine.
    pub fn orden(&self) -> String {
        let n = shell_arg(&self.name);
        match (self.there.is_empty(), self.here.is_empty()) {
            // Here it is in no space and another machine says it is: putting it in one is this
            // person's decision, not the proposing repository's (maybe they made it personal on purpose).
            (false, true) => format!("omoi projects move {n} --space {} --force", self.there),
            (false, false) => format!("omoi projects move {n} --space {} --force", self.there),
            (true, _) => format!("omoi projects move {n} --space {}", self.here),
        }
    }

    pub fn texto(&self) -> String {
        let (n, m, o) = (&self.name, &self.machine, self.orden());
        let (here_en, here_es) = if self.here.is_empty() {
            ("in no space".to_string(), "en ninguno".to_string())
        } else {
            (format!("in {}", self.here), format!("en {}", self.here))
        };
        match self.there.is_empty() {
            false => tr!(
                "{n} is in the space {} on {m} and {here_en} here, so its changes are not applied; if it is the same project and belongs in {}: `{o}`",
                "{n} está en el espacio {} en {m} y aquí {here_es}, así que sus cambios no se aplican; si es el mismo proyecto y va en {}: `{o}`",
                self.there,
                self.there
            ),
            true => tr!(
                "{n} is in the space {} here but {m} still has it in no space (through the general repository), so its changes are not applied: on {m}, `{o}`",
                "{n} está aquí en el espacio {} pero {m} aún lo tiene sin espacio (por el repositorio general), así que sus cambios no se aplican: en {m}, `{o}`",
                self.here
            ),
        }
    }
}

/// The clashes from several files, one per project and situation (without repeating each machine's).
pub fn sin_repetir<'a>(clashes: impl IntoIterator<Item = &'a Choque>) -> Vec<Choque> {
    let mut seen = HashSet::new();
    clashes.into_iter().filter(|c| seen.insert((c.key.clone(), c.here.clone(), c.there.clone()))).cloned().collect()
}

/// The other command, the one that doesn't leave the project as it is: move it to the space the line that
/// paused it says (always with `--force`: its repository would become that client's), or take it out of its own.
/// If it isn't known which, with `<space>`. For the pause of a claimed folder (`claimed`), take it out of the
/// space: it is personal.
pub fn orden_aceptar(p: &crate::proyecto::EnPausa) -> String {
    let n = shell_arg(&p.name);
    match (p.pausa.claimed, p.pausa.unknown, p.pausa.space.is_empty()) {
        (true, _, _) | (false, false, true) => format!("omoi space unassign {n} --force"),
        (false, true, _) => format!("omoi projects move {n} --space <space> --force"),
        (false, false, false) => format!("omoi projects move {n} --space {} --force", p.pausa.space),
    }
}

/// The command that leaves it where it is (`omoi projects keep`): the first one recommended.
pub fn orden_quedarse(p: &crate::proyecto::EnPausa) -> String {
    format!("omoi projects keep {}", shell_arg(&p.name))
}

/// What is said about a paused project, with the two commands (`omoi sync`, `sync status`, `doctor`,
/// `review` and Review).
pub fn texto_pausa(db_path: &Path, p: &crate::proyecto::EnPausa) -> String {
    let (n, m, accept, keep) = (&p.name, &p.pausa.machine, orden_aceptar(p), orden_quedarse(p));
    let (here_en, here_es) = if p.here.is_empty() { ("in no space".to_string(), "en ninguno".to_string()) } else { (format!("in {}", p.here), format!("en {}", p.here)) };
    let e = &p.pausa.space;
    // What another machine says is not repeated as if it were true: it can be a mistake or a line written on
    // purpose. First, the command that leaves it as it is.
    let mut t = if p.pausa.claimed {
        tr!(
            "{n} is paused: this folder is in the space {e} only because {e}'s repository says so (this computer had no folder of it). What you save in it stays here until you decide: if it is {e}'s, `{keep}`; if it is yours, `{accept}` (it goes to the general repository, never to {e})",
            "{n} está en pausa: esta carpeta es del espacio {e} solo porque lo dice el repositorio de {e} (este equipo no tenía ninguna carpeta suya). Lo que guardes en ella se queda aquí hasta que decidas: si es de {e}, `{keep}`; si es tuya, `{accept}` (va al repositorio general, nunca a {e})"
        )
    } else if p.pausa.unknown {
        tr!(
            "{n} is paused: another computer ({m}) says it moved it to a space (the general repository doesn't say which); here it is {here_en}. Nothing of it is synced until you decide. If you don't know, `{keep}` (it stays as it is). If it does belong to a client, first join that client's repository (`omoi sync join --space <space> <code>`, with the code from a computer that is in it): then this notice names the space, and `{accept}` moves it",
            "{n} está en pausa: otro equipo ({m}) dice que lo pasó a un espacio (el repositorio general no dice a cuál); aquí está {here_es}. No se sincroniza nada suyo hasta que decidas. Si no lo sabes, `{keep}` (se queda como está). Si sí es de un cliente, primero únete al repositorio de ese cliente (`omoi sync join --space <espacio> <código>`, con el código de un equipo que esté en él): entonces este aviso dice el espacio, y `{accept}` lo mueve"
        )
    } else if e.is_empty() {
        tr!(
            "{n} is paused: another computer ({m}) says it is in no space; here it is {here_en}. Nothing of it is synced until you decide. If you don't know, `{keep}` (it stays as it is); to take it out, `{accept}` (its memories would go to the general repository)",
            "{n} está en pausa: otro equipo ({m}) dice que no es de ningún espacio; aquí está {here_es}. No se sincroniza nada suyo hasta que decidas. Si no lo sabes, `{keep}` (se queda como está); para sacarlo, `{accept}` (sus memorias irían al repositorio general)"
        )
    } else {
        tr!(
            "{n} is paused: another computer ({m}) says it belongs to the space {e}; here it is {here_en}. Nothing of it is synced until you decide. If you don't know, `{keep}` (it stays as it is); to move it to {e}, `{accept}`",
            "{n} está en pausa: otro equipo ({m}) dice que es del espacio {e}; aquí está {here_es}. No se sincroniza nada suyo hasta que decidas. Si no lo sabes, `{keep}` (se queda como está); para pasarlo a {e}, `{accept}`"
        )
    };
    // If it goes to a space whose repository this machine doesn't have: how to join.
    if !p.pausa.claimed && !e.is_empty() && repo_de_espacio(db_path, e).is_none() {
        t.push_str(&tr!(
            " (to sync it in {e}, join its repository: `omoi sync join --space {e} <code>`; `omoi sync code --space {e}` on a computer that has it gives the code)",
            " (para sincronizarlo en {e}, únete a su repositorio: `omoi sync join --space {e} <código>`; el código lo da `omoi sync code --space {e}` en un equipo que ya lo tenga)"
        ));
    }
    t
}

/// The notices for this database's paused projects.
pub fn avisos_de_pausa(db: &Connection, db_path: &Path) -> Vec<String> {
    crate::proyecto::en_pausa(db).unwrap_or_default().iter().map(|p| texto_pausa(db_path, p)).collect()
}

/// Whether a project has no memories.
fn has_no_memories(db: &Connection, project: i64) -> Result<bool> {
    Ok(db.query_row("SELECT NOT EXISTS (SELECT 1 FROM memory WHERE project_id = ?1)", [project], |f| f.get(0))?)
}

/// With nobody at the keyboard (`OMOIKANE_NONINTERACTIVE=1`): whether the repository of space `space` (its key)
/// is one of those from `OMOIKANE_SYNC_CODE`, that is, whether one of the keys this machine has for it
/// is that of a code in the variable with that space, and the project key is a git remote's
/// (`git:`; a `dir:` is only a folder name and doesn't say whose it is). The rest that is
/// needed (that the `origin` of a local folder is that key) is checked by `proyecto::adopta_en`.
pub(crate) fn adopta_por_entorno(db: &Connection, key: &str, space: &str) -> bool {
    if !crate::entorno::desatendido() || !key.starts_with("git:") || space.is_empty() {
        return false;
    }
    let codes = crate::entorno::codigos_sync();
    if codes.is_empty() {
        return false;
    }
    let Some(r) = db.path().filter(|p| !p.is_empty()).and_then(|p| repo_de_espacio(Path::new(p), space)) else { return false };
    let Ok(cipher) = leer_claves(&r) else { return false };
    codes.iter().filter_map(|k| leer_codigo(k).ok()).any(|c| {
        c.espacio.as_deref().is_some_and(|e| crate::proyecto::clave_de_espacio(e) == space) && cipher.keys.get(&c.id).is_some_and(|k| *k == c.clave)
    })
}

/// For the agent, at session start: one line per paused project, with what it has to do.
pub fn aviso_de_pausa_agente(db: &Connection) -> Option<String> {
    let v = crate::proyecto::en_pausa(db).ok()?;
    // With nobody at the keyboard there is nobody to ask: it must run neither command (deciding
    // is a person's job) and know that what it saves there doesn't leave this machine.
    let unattended = crate::entorno::desatendido();
    let lines: Vec<String> = v
        .iter()
        .map(|p| {
            let (accept, keep) = (orden_aceptar(p), orden_quedarse(p));
            let e = &p.pausa.space;
            if unattended {
                return format!(
                    "The project {} is paused until a person decides which space it belongs to; nobody can decide it here, so run neither `{keep}` nor `{accept}`. Nothing of it is synced: what you save in it stays on this computer.",
                    p.name
                );
            }
            if p.pausa.claimed {
                return format!(
                    "The project {} is paused: its folder is in the space {e} only because {e}'s repository says so (this computer had no folder of it). What is saved in it stays on this computer until the user decides. Ask the user whether it belongs to {e}: if so, run `{keep}`; if it is theirs, `{accept}`.",
                    p.name
                );
            }
            let says = if p.pausa.unknown {
                "another computer says it moved it to a space (the general repository doesn't say which)".to_string()
            } else if e.is_empty() {
                "another computer says it is in no space".to_string()
            } else {
                format!("another computer says it belongs to the space {e}")
            };
            format!(
                "The project {} is paused: {says}. Nothing of it is synced until the user decides. Ask the user; if they don't know, run `{keep}` (it stays as it is); to accept the change, `{accept}`.",
                p.name
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

impl Resumen {
    /// Adds up another repository's counts (bad files are recorded in each one's status).
    fn add(&mut self, o: Resumen) {
        self.exportadas += o.exportadas;
        self.leidos += o.leidos;
        self.nuevas += o.nuevas;
        self.actualizadas += o.actualizadas;
        self.olvidadas += o.olvidadas;
        self.ilegibles += o.ilegibles;
        self.de_version_nueva += o.de_version_nueva;
        self.conflictos_de_espacio += o.conflictos_de_espacio;
        self.en_pausa += o.en_pausa;
        self.sellos_ajenos += o.sellos_ajenos;
        self.claves_repetidas += o.claves_repetidas;
        self.avisos.extend(o.avisos);
    }
}

/// Whether the line of a memory arriving through a space's repository (shared with other
/// people) carries a verified seal the local person didn't put. It is decided by the seal's signature
/// with this person's seal key (`verificado::sello_propio`; derived from their general repository,
/// which nobody else has), never by its `by`: anyone sharing the space can write
/// `"by": "<your name>"`. That seal doesn't count here: it is another person's trust. Through the general
/// repository (the person's, across their machines) seals count, as always. Returns what
/// identifies that seal (`by` and `at`, for [`crate::verificado::SELLO_AJENO`]).
fn foreign_seal(db: &Connection, c: &Config, id: &str, extra: &Extra) -> Result<Option<String>> {
    if c.space.is_none() {
        return Ok(None);
    }
    let Some(v) = extra.get(crate::verificado::CLAVE) else { return Ok(None) };
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    // One of theirs, but older than when the person removed the verification here: someone else in the space could
    // resend an old seal of the person (it carries their signature) to undo that «no longer».
    let unverified_after = match crate::verificado::desverificada_en(db, id)? {
        Some(d) => d > text("at"),
        None => false,
    };
    if !unverified_after && crate::verificado::sello_propio(&crate::verificado::llaves(db, false)?, id, v) {
        return Ok(None);
    }
    Ok(Some(format!("{}\n{}", text("by"), text("at"))))
}

fn hlc_local(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

fn tombstone_hlc(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

/// The tombstone of `id` in space `space`: the one that can stop a memory of that space. One that
/// arrived through another space's repository doesn't stop it (it can't delete another client's memories).
fn tombstone_hlc_in(db: &Connection, id: &str, space: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1 AND space_key = ?2", [id, space], |f| f.get(0)).optional()?)
}

/// The space (its key; empty if none) of the project with this key, if there is one here.
fn project_space(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT COALESCE((SELECT key FROM space WHERE id = p.space_id), '') FROM project p WHERE p.key = ?1", [key], |f| f.get(0))
        .optional()?)
}

/// The space of a memory in the trash: the one its project has here now (if it is here), or the one
/// recorded when it was forgotten.
pub(crate) fn espacio_de_papelera(db: &Connection, text: &str) -> Result<String> {
    let Some((m, _, _)) = de_papelera(text) else { return Ok(String::new()) };
    if !m.project_key.is_empty()
        && let Some(e) = project_space(db, &m.project_key)?
    {
        return Ok(e);
    }
    Ok(m.space)
}

/// The space (its key; empty if none) of a local memory, live or in the trash;
/// `None` if it is not here.
pub(crate) fn espacio_aqui(db: &Connection, id: &str) -> Result<Option<String>> {
    let live: Option<String> = db
        .query_row(
            "SELECT COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '') FROM memory m WHERE m.id = ?1",
            [id],
            |f| f.get(0),
        )
        .optional()?;
    if live.is_some() {
        return Ok(live);
    }
    let trash: Option<String> = db.query_row("SELECT memory FROM forgotten WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    trash.map(|t| espacio_de_papelera(db, &t)).transpose()
}

/// `a` is equal to or later than `b`.
fn not_older(a: &str, b: &str) -> bool {
    reloj::comparar(a, b) != std::cmp::Ordering::Less
}

/// Applies lines with the sync rules, whether they come from the repository or from an
/// `omoi export` file: the newest stamp wins, what was forgotten doesn't come back, a project is recognized by its
/// key and, if it doesn't exist here, takes the source path. Without `config` (an import) everything is accepted.
pub(crate) struct Aplicador<'a> {
    config: Option<&'a Config>,
    person: String,
    /// The opaque names of the local projects (`Cifra::folder`, with each repository key)
    /// and their key: with them the general repository's `project` line with `moved` is recognized.
    opaque: HashMap<String, String>,
}

impl<'a> Aplicador<'a> {
    /// With `current` (the session's folder), that folder is recorded before applying anything: whatever
    /// arrives with its key is already its own.
    pub(crate) fn nuevo(db: &Connection, config: Option<&'a Config>, current: Option<&str>) -> Result<Self> {
        let person = crate::db::persona_de(db);
        if let Some(a) = current.filter(|a| !a.is_empty() && !crate::proyecto::es_general(a)) {
            crate::proyecto::registrar(db, a)?;
        }
        Ok(Aplicador { config, person, opaque: HashMap::new() })
    }

    /// Records the opaque names of the local projects with this repository's keys.
    fn with_opaque(&mut self, db: &Connection, cipher: &Cifra) -> Result<()> {
        let mut q = db.prepare("SELECT key FROM project WHERE id != 0 AND key != ''")?;
        let keys: Vec<String> = q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        for k in keys {
            for key in cipher.keys.values() {
                self.opaque.insert(format!("p-{}", opaque_name(key, &[k.as_bytes()])), k.clone());
            }
        }
        Ok(())
    }

    /// Applies a line. `Err` is a line that couldn't be applied (it is counted and we carry on).
    /// Tombstones are best applied before memories, and verdicts after.
    pub(crate) fn aplicar(&mut self, db: &mut Connection, source: &str, l: Linea, model: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
        match l {
            Linea::Header { .. } | Linea::End { .. } => Ok(()),
            Linea::Project { key, name, space_key, hlc, moved, extra, .. } => {
                // Not applied in an import: a new project's space comes with its memories, and a local
                // one doesn't change space because of a copy.
                let Some(c) = self.config else { return Ok(()) };
                // One more than an hour in the future is not applied (it counts as invalid): with that
                // stamp, keeping the project (`keep`) would make it deaf to real moves.
                ensure!(!reloj::del_futuro(&hlc), "project: {hlc}");
                // The general one's without a name: the local project with that opaque name (if none, nothing).
                let key = match moved {
                    true => match self.opaque.get(&key) {
                        Some(k) => k.clone(),
                        None => return Ok(()),
                    },
                    false => key,
                };
                if c.projects.as_ref().is_some_and(|p| !p.contains(&key)) {
                    return Ok(());
                }
                let local: Option<(i64, String, String, String)> = db
                    .query_row(
                        "SELECT p.id, COALESCE((SELECT key FROM space WHERE id = p.space_id), ''), p.hlc, p.paused FROM project p WHERE p.key = ?1",
                        [&key],
                        |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)),
                    )
                    .optional()?;
                let Some((id, here, stamp, pause)) = local else {
                    // A project not here yet: it is recorded in its space only if that space's
                    // repository says so (a folder of it that shows up here joins it). Without a
                    // space, or through another repository, nothing to record.
                    if moved || space_key.is_empty() || !goes_to_repo(c, &space_key) {
                        return Ok(());
                    }
                    let id = crate::proyecto::de_clave(db, &key, "", &self.person, false)?;
                    if !name.trim().is_empty() {
                        db.execute("UPDATE project SET name = ?2 WHERE id = ?1", params![id, name.trim()])?;
                    }
                    let s = crate::proyecto::espacio_de_clave(db, &space_key)?;
                    db.execute("UPDATE project SET space_id = ?2, hlc = ?3 WHERE id = ?1", params![id, s, hlc])?;
                    save_project_extra(db, &key, &extra)?;
                    reloj::recibir(db, &hlc)?;
                    return Ok(());
                };
                // What this machine already knew about the project: its stamp, or that of the line that paused it. A
                // line that is not newer changes nothing (unless it says which space the one that was
                // paused without knowing goes to: the same decision, now with a name).
                let pause = crate::proyecto::Pausa::de(&pause);
                let known = match &pause {
                    Some(p) if reloj::mayor(&p.hlc, &stamp) => p.hlc.clone(),
                    _ => stamp,
                };
                let names_space = pause.as_ref().is_some_and(|p| p.unknown && !moved && p.hlc == hlc);
                if not_older(&known, &hlc) && !names_space {
                    return Ok(());
                }
                // It says the same as what is here: nothing to do. Its stamp isn't recorded (a hand-written line
                // doesn't make the project deaf to later moves), nor does it lift the pause (that
                // only happens by deciding here).
                if !moved && space_key == here {
                    return Ok(());
                }
                // «Moved to a space» for one that is already in one here: it may be the same one; we can't know.
                if moved && !here.is_empty() {
                    return Ok(());
                }
                // With nobody at the keyboard and joined to this repository through `OMOIKANE_SYNC_CODE`: a local
                // folder (the CI's) with the same `origin`, without a space and without memories that could belong to
                // another repository, joins the space without a pause (`proyecto::adopta_en`).
                if !moved
                    && here.is_empty()
                    && pause.is_none()
                    && c.space.is_some()
                    && goes_to_repo(c, &space_key)
                    && has_no_memories(db, id)?
                    && crate::proyecto::adopta_en(db, id, &space_key)?
                {
                    let s = crate::proyecto::espacio_de_clave(db, &space_key)?;
                    db.execute("UPDATE project SET space_id = ?2, hlc = ?3 WHERE id = ?1", params![id, s, hlc])?;
                    reloj::recibir(db, &hlc)?;
                    return Ok(());
                }
                // Another space: it neither moves nor keeps syncing; paused until it is decided here.
                let who = reloj::partes(&hlc).2.to_string();
                let p = crate::proyecto::Pausa { space: if moved { String::new() } else { space_key }, unknown: moved, hlc, machine: who, ..Default::default() };
                crate::proyecto::pausar(db, id, &p)?;
                reloj::recibir(db, &p.hlc)?;
                Ok(())
            }
            Linea::Tombstone { id, deleted_at, hlc, space_key, extra, .. } => {
                if tombstone_hlc(db, &id)?.is_some_and(|h| not_older(&h, &hlc)) {
                    return Ok(());
                }
                // Through a space's repository only that space's tombstones arrive (without `space_key`,
                // from a version that doesn't send it: the repository's).
                let space = match (space_key.is_empty(), self.config.and_then(|c| c.space.as_deref())) {
                    (true, Some(e)) => crate::proyecto::clave_de_espacio(e),
                    _ => space_key,
                };
                // What was forgotten for good here (no copy in the trash) belongs to its tombstone's space.
                let mark: Option<String> = db.query_row("SELECT space_key FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?;
                let here = match espacio_aqui(db, &id)? {
                    Some(e) => Some(e),
                    None => mark,
                };
                // A tombstone only deletes what the repository it arrives through carries: acme's doesn't forget
                // a beta memory (nor a personal one), nor changes the space of the tombstone of one of them.
                if let Some(c) = self.config
                    && (!goes_to_repo(c, &space) || here.as_deref().is_some_and(|e| !goes_to_repo(c, e)))
                {
                    r.conflictos_de_espacio += 1;
                    return Ok(());
                }
                let space = here.unwrap_or(space);
                // The incoming tombstone becomes the local one (in any of the three cases): its unknown
                // fields go with it.
                guardar_extra_olvido(db, &id, &extra)?;
                match hlc_local(db, &id)? {
                    Some(local) if reloj::mayor(&local, &hlc) => {
                        // It was changed here after it was forgotten there: it stays, and the old tombstone is recorded.
                        memoria::marcar_olvido(db, &id, Some(&deleted_at), &hlc, &space)?;
                        reloj::recibir(db, &hlc)?;
                    }
                    Some(_) => {
                        memoria::olvidar_con(db, &id, &hlc, &deleted_at, source)?;
                        r.olvidadas += 1;
                    }
                    None => {
                        memoria::marcar_olvido(db, &id, Some(&deleted_at), &hlc, &space)?;
                        reloj::recibir(db, &hlc)?;
                    }
                }
                Ok(())
            }
            Linea::Memory(CuerpoMemoria { id, project_key, project_path, space_key, kind, title, content, paths, tags, pinned, created_at, updated_at, hlc, content_hlc, author, versions, extra, .. }) => {
                if let Some(c) = self.config {
                    // A client repository doesn't bring trade memories (global with `trade:`): the
                    // projects of that trade across all clients would see them.
                    if c.space.is_some() && project_key.is_empty() && space_key.is_empty() && crate::oficios::lleva_oficio(&tags) {
                        r.conflictos_de_espacio += 1;
                        return Ok(());
                    }
                    // Only its space's memories come in through a repository (the general one: those of none, and the
                    // spaces its `spaces` lists): the rest is rejected and counted.
                    if !goes_to_repo(c, &space_key) {
                        r.conflictos_de_espacio += 1;
                        return Ok(());
                    }
                    // What this machine doesn't share isn't accepted either (e.g. another person's
                    // preferences).
                    if !c.accepts(&kind, &project_key, &space_key) {
                        return Ok(());
                    }
                }
                // A paused project's memories don't come in (from any repository) until it is decided: they are counted,
                // and the file is reread then.
                if self.config.is_some() {
                    let project_paused: bool = if project_key.is_empty() { false } else { crate::proyecto::pausado(db, &project_key)? };
                    let own_paused: bool = db
                        .query_row("SELECT p.paused != '' FROM memory m JOIN project p ON p.id = m.project_id WHERE m.id = ?1", [&id], |f| f.get(0))
                        .optional()?
                        .unwrap_or(false);
                    if project_paused || own_paused {
                        r.en_pausa += 1;
                        return Ok(());
                    }
                }
                // What was forgotten doesn't come back; through sync, only a tombstone from its same space stops it.
                let tombstone = if self.config.is_some() { tombstone_hlc_in(db, &id, &space_key)? } else { tombstone_hlc(db, &id)? };
                if tombstone.is_some_and(|h| not_older(&h, &hlc)) {
                    return Ok(());
                }
                let local = hlc_local(db, &id)?;
                if local.as_ref().is_some_and(|h| not_older(h, &hlc)) {
                    return Ok(());
                }
                // Nothing changes space because of what arrives: neither a local memory nor a local project.
                // A line that would land in another space (another client's `web` folder with the same
                // key, or a beta key written in acme's repository) is rejected.
                if self.config.is_some() {
                    let memory_elsewhere = local.is_some() && espacio_aqui(db, &id)?.is_some_and(|e| e != space_key);
                    let project_elsewhere = if project_key.is_empty() { None } else { project_space(db, &project_key)?.filter(|e| *e != space_key) };
                    if memory_elsewhere || project_elsewhere.is_some() {
                        r.conflictos_de_espacio += 1;
                        // Which project, to say which command fixes it.
                        if let Some(here) = project_elsewhere {
                            let name = db.query_row("SELECT name FROM project WHERE key = ?1", [&project_key], |f| f.get(0))?;
                            r.pendientes.push(Choque { key: project_key, name, here, there: space_key, machine: source.to_string() });
                        }
                        return Ok(());
                    }
                }
                // The project, by its key (if there is none here, with the source path, which is adopted
                // as soon as a folder with that key shows up here); or the space; or global.
                let (project_id, space_id) = if !project_key.is_empty() {
                    let p = crate::proyecto::de_clave(db, &project_key, &project_path, &self.person, self.config.is_none())?;
                    // A new project takes the space the line brings (through sync, one that was already
                    // here in another space was rejected above; in an import, one without a space
                    // takes the one it brings).
                    if !space_key.is_empty() && crate::proyecto::espacio_de(db, p)?.is_none() {
                        let s = crate::proyecto::espacio_de_clave(db, &space_key)?;
                        db.execute("UPDATE project SET space_id = ?2 WHERE id = ?1", params![p, s])?;
                        // If its folder was already here (the same path, with the same key), it is a local
                        // folder joining a space because its repository says so: paused until
                        // the person confirms it, as if it were opened afterwards.
                        // With nobody at the keyboard and joined to this repository through the variable, the CI folder
                        // joins without a pause if it had nothing (whatever it had could belong to another repository).
                        if self.config.is_some()
                            && crate::proyecto::carpetas_propias(db, p)? > 0
                            && !(has_no_memories(db, p)? && crate::proyecto::adopta_sin_pausa(db, p)?)
                        {
                            crate::proyecto::pausar_por_reclamo(db, p)?;
                        }
                    }
                    (Some(p), None)
                } else if !space_key.is_empty() {
                    (None, crate::proyecto::espacio_de_clave(db, &space_key)?)
                } else {
                    (Some(0), None)
                };
                let parsed_kind = kind.parse::<Tipo>().ok();
                let kind_text = if parsed_kind.is_none() { Some(kind.clone()) } else { None };
                // Another person's verified seal doesn't count here (see `foreign_seal`). It is stored as
                // is (the line this machine writes again carries it, for the others) and `meta` records
                // that it doesn't count here: `memoria::COLUMNAS` doesn't treat it as verified.
                if let Some(c) = self.config {
                    let mark = format!("{}{id}", crate::verificado::SELLO_AJENO);
                    match foreign_seal(db, c, &id, &extra)? {
                        Some(stamp) => {
                            r.sellos_ajenos += 1;
                            db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![mark, stamp])?;
                        }
                        None => {
                            db.execute("DELETE FROM meta WHERE k = ?1", [&mark])?;
                        }
                    }
                }
                // The verification was removed here (`verify --unverify`, or its text changed): the date
                // stays and an earlier seal doesn't come back, even if the line is newer and brings the text that was
                // verified (another machine that hadn't seen the change yet, a copy). Through a space's
                // repository it stays in the line for the others and doesn't count here (`foreign_seal`).
                let mut extra = extra;
                if local.is_some()
                    && let Some(d) = crate::verificado::desverificada_en(db, &id)?
                {
                    crate::verificado::sin_sello_viejo(&mut extra, &d, self.config.is_none_or(|c| c.space.is_none()));
                }
                // The fields this version doesn't know are stored with it, to write them back.
                let extra = extra_a_texto(&extra);
                let o = Origen { created_at: &created_at, updated_at: &updated_at, hlc: &hlc, content_hlc: &content_hlc, author: &author, extra: &extra };
                // A line that doesn't pass the checks (empty or long title, empty content…) is
                // skipped and counted: it never stops the sync.
                if local.is_some() {
                    let c = Cambios {
                        title: Some(title),
                        content: Some(content),
                        kind: Some(parsed_kind.unwrap_or(Tipo::Fact)),
                        kind_text,
                        paths: Some(paths),
                        tags: Some(tags),
                        pinned: Some(pinned),
                        project_id,
                        space_id,
                        reason: Some(format!("sync: {source}")),
                        ..Default::default()
                    };
                    memoria::actualizar_con_origen(db, &id, c, &o, model)?;
                    r.actualizadas += 1;
                } else {
                    let new_memory = Nueva {
                        project: String::new(),
                        project_id,
                        space_id,
                        kind: Some(parsed_kind.unwrap_or(Tipo::Fact)),
                        kind_text,
                        title,
                        content,
                        paths,
                        tags,
                        pinned,
                        source: format!("sync:{source}"),
                        provenance: None,
                        evidence: Vec::new(),
                        review_reason: None,
                    };
                    memoria::guardar_con_id(db, &id, new_memory, &o, model)?;
                    r.nuevas += 1;
                }
                // It exists again with a stamp later than its tombstone (another machine restored it): it is no
                // longer in the trash.
                db.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
                // Whatever history comes: what already exists with that number stays.
                for v in &versions {
                    memoria::poner_version(db, &id, v)?;
                }
                Ok(())
            }
            Linea::Verdict { memory_id, other_id, relation, at, hlc, reason, author, extra, .. } => {
                // For a memory that isn't here (not shared, or forgotten for good) nothing is
                // recorded. Those in the trash, yes: if it comes back, it comes back with its verdicts (`omoi export`
                // carries them, and they go after the trash).
                let Some(first_space) = espacio_aqui(db, &memory_id)? else { return Ok(()) };
                let other_space = match other_id.is_empty() {
                    true => None,
                    false => match espacio_aqui(db, &other_id)? {
                        Some(e) => Some(e),
                        None => return Ok(()),
                    },
                };
                // A verdict only touches memories of what the repository it arrives through carries.
                if let Some(c) = self.config
                    && (!goes_to_repo(c, &first_space) || other_space.as_deref().is_some_and(|e| !goes_to_repo(c, e)))
                {
                    r.conflictos_de_espacio += 1;
                    return Ok(());
                }
                let extra = extra_a_texto(&extra);
                crate::veredictos::recibir(db, &crate::veredictos::Veredicto { memory_id, other_id, relation, at, hlc, reason, author, extra })?;
                Ok(())
            }
            Linea::Forgotten { id, memory, versions, hlc, deleted_at, by, extra } => {
                // To the trash, if it isn't live here nor already in the trash. It is stored like the local one
                // (`Memoria`, with its project and its space by key) and with its unknown fields.
                if hlc_local(db, &id)?.is_some() {
                    return Ok(());
                }
                let source = if memory.machine.is_empty() { format!("sync:{source}") } else { format!("sync:{}", memory.machine) };
                let text = a_papelera(&memory.to_memory(&source), &memory.extra, &extra)?;
                let n = db.execute(
                    "INSERT OR IGNORE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, text, serde_json::to_string(&versions)?, hlc, deleted_at, by],
                )?;
                // As in a `memory` line: another person's seal arriving in the trash through a
                // space's repository doesn't count here if restored (`memoria::restaurar` keeps
                // it as is).
                if n > 0
                    && let Some(c) = self.config
                {
                    let mark = format!("{}{id}", crate::verificado::SELLO_AJENO);
                    match foreign_seal(db, c, &id, &memory.extra)? {
                        Some(stamp) => {
                            r.sellos_ajenos += 1;
                            db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![mark, stamp])?;
                        }
                        None => {
                            db.execute("DELETE FROM meta WHERE k = ?1", [&mark])?;
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

/// Reads other machines' files and applies their lines. A line that isn't understood (from a future
/// version, another key, or damaged) is skipped and counted, without stopping the sync.
#[allow(clippy::too_many_arguments)]
fn import(db: &mut Connection, clone_dir: &Path, files: &[PathBuf], config: &Config, cipher: &Cifra, current: Option<&str>, model: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
    let mut applier = Aplicador::nuevo(db, Some(config), current)?;
    applier.with_opaque(db, cipher)?;
    // (source machine, file, line)
    let mut lines: Vec<(String, String, Linea)> = Vec::new();
    for f in files {
        let rel_path = f.strip_prefix(clone_dir).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(f).with_context(|| format!("{}", f.display()))?;
        r.malos.insert(rel_path.clone(), 0);
        r.futuros.insert(rel_path.clone(), 0);
        r.conflictos.insert(rel_path.clone(), 0);
        r.pausadas.insert(rel_path.clone(), 0);
        for l in text.lines().filter(|l| !l.trim().is_empty()) {
            // A label that is not a key's (`x1:…`) is from a future format.
            if split_key_id(l).is_none() {
                *r.futuros.entry(rel_path.clone()).or_default() += 1;
                continue;
            }
            let Ok(plain) = cipher.descifrar(l) else {
                *r.malos.entry(rel_path.clone()).or_default() += 1;
                continue;
            };
            let x = match serde_json::from_str::<Linea>(&plain) {
                Ok(x) => x,
                // A line type from a newer version: counted separately, to ask for an
                // update, and it will be reread then.
                Err(_) if de_tipo_nuevo(&plain) => {
                    *r.futuros.entry(rel_path.clone()).or_default() += 1;
                    continue;
                }
                Err(_) => {
                    *r.malos.entry(rel_path.clone()).or_default() += 1;
                    continue;
                }
            };
            if !x.valida() {
                *r.malos.entry(rel_path.clone()).or_default() += 1;
                continue;
            }
            let source = if x.machine().is_empty() { f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { x.machine().to_string() };
            lines.push((source, rel_path.clone(), x));
        }
        r.leidos += 1;
    }
    // Tombstones first (so as not to bring something about to be deleted), then projects (the space
    // their memories land in), then memories and finally verdicts, which need them.
    let rank = |l: &Linea| match l {
        Linea::Tombstone { .. } => 0,
        Linea::Project { .. } => 1,
        Linea::Memory(_) => 2,
        _ => 3,
    };
    // `project` lines, from newest to oldest: the winner is applied first, and the rest no longer
    // change anything or warn about what no longer is.
    let stamp_of = |l: &Linea| match l {
        Linea::Project { hlc, .. } => hlc.clone(),
        _ => String::new(),
    };
    lines.sort_by(|(_, _, x), (_, _, y)| rank(x).cmp(&rank(y)).then_with(|| reloj::comparar(&stamp_of(y), &stamp_of(x))));
    for (source, file, l) in lines {
        let (before, paused_before) = (r.conflictos_de_espacio, r.en_pausa);
        if applier.aplicar(db, &source, l, model, r).is_err() {
            *r.malos.entry(file.clone()).or_default() += 1;
        }
        if r.conflictos_de_espacio > before {
            *r.conflictos.entry(file.clone()).or_default() += r.conflictos_de_espacio - before;
        }
        if r.en_pausa > paused_before {
            *r.pausadas.entry(file.clone()).or_default() += r.en_pausa - paused_before;
        }
        for c in std::mem::take(&mut r.pendientes) {
            r.choques.entry(file.clone()).or_default().insert(c);
        }
    }
    r.ilegibles = r.malos.values().sum();
    r.de_version_nueva = r.futuros.values().sum();
    Ok(())
}

// --- Sync -----------------------------------------------------------------------------------------

fn not_configured() -> String {
    tr!(
        "sync is not set up: `omoi sync init <git url>` (or `omoi sync join <code>`)",
        "la sincronización no está configurada: `omoi sync init <url de git>` (o `omoi sync join <código>`)"
    )
}

/// A full sync of each repository on this machine (the general one and each space's):
/// pull, merge, write this machine's memories, commit and push. Records how each one went in its status;
/// if one fails, the others sync anyway and the first error is returned.
pub fn sincronizar(db: &mut Connection, db_path: &Path, current: Option<&str>, model: Option<&Modelo>) -> Result<Resumen> {
    let repos = repos(db_path);
    ensure!(!repos.is_empty(), not_configured());
    let mut total = Resumen { equipo: reloj::equipo(db)?, ..Default::default() };
    let mut first_error = None;
    for r in &repos {
        let res = leer_config_de(r)
            .and_then(|c| c.context(not_configured()))
            .and_then(|config| sync_repo(db, r, &config, current, model, None))
            .map(|(res, _)| res);
        let res = match &r.espacio {
            Some(e) => res.with_context(|| tr!("space {}", "espacio {}", e)),
            None => res,
        };
        record_status(r, &res);
        match res {
            Ok(x) => total.add(x),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    // The paused projects (of this repository or another): once, with their commands.
    total.avisos.extend(avisos_de_pausa(db, db_path));
    match first_error {
        Some(e) => Err(e),
        None => Ok(total),
    }
}

/// Syncs a repository. With `rotate`, before writing anything it starts using that key: it gives it the id
/// after the highest the repository knows, records it in `omoikane.check` as current and
/// encrypts with it everything from this machine. Returns the keys it used.
fn sync_repo(db: &mut Connection, r: &Rutas, config: &Config, current: Option<&str>, model: Option<&Modelo>, rotar: Option<&[u8]>) -> Result<(Resumen, Cifra)> {
    let base = leer_claves(r)?;
    ensure!(r.clon.join(".git").exists(), tr!("the sync clone is missing: run `omoi sync init` again", "falta el clon de la sincronización: vuelve a hacer `omoi sync init`"));
    if let Some(a) = current {
        adoptar(db, a)?;
    }
    let machine = reloj::equipo(db)?;
    reread_if_needed(db)?;
    for attempt in 0..4 {
        // The counts, for each attempt: a retry rereads the same files.
        let mut res = Resumen { equipo: machine.clone(), ..Default::default() };
        reset_clone(&r.clon)?;
        // The new key's id, for each attempt: if another machine rotated in between, the next one.
        let cipher = match rotar {
            Some(k) => base.with_key(KeyCheck::read(&r.clon).map_or(0, |c| c.vigente()).max(base.vigente()) + 1, k.to_vec()),
            None => base.clone(),
        };
        // A repository that asks for a newer reader: it is neither read nor written.
        if required_reader(&r.clon, &cipher).is_some_and(|n| n > LECTOR_REPO) {
            bail!(aviso_version());
        }
        write_fixed_files(&r.clon, &cipher)?;
        remove_old_files(&r.clon, &machine, &cipher)?;
        let mut files = files_to_read(db, &r.clon, &machine, &cipher, &r.marca_commit)?;
        // Files with lines rejected because their project is in another space (or with a project
        // notice) or paused are always reread, even if they haven't changed: if the project here
        // is already where they say (it was moved by hand, or its pause was decided), their changes come in now, and if
        // not, the notice is recomputed. Those that no longer exist stop being counted.
        let e = leer_estado_de(r);
        let reread = |f: &String| e.ficheros_conflicto.contains_key(f) || e.ficheros_choques.contains_key(f) || e.ficheros_en_pausa.contains_key(f);
        for f in e.ficheros_conflicto.keys().chain(e.ficheros_choques.keys()).chain(e.ficheros_en_pausa.keys()).chain(e.ficheros_malos.keys()).chain(e.ficheros_futuros.keys()) {
            let path = r.clon.join(f);
            if !path.exists() {
                for counts in [&mut res.malos, &mut res.futuros, &mut res.conflictos, &mut res.pausadas] {
                    counts.insert(f.clone(), 0);
                }
            } else if reread(f) && !files.contains(&path) {
                files.push(path);
            }
        }
        files.sort();
        import(db, &r.clon, &files, config, &cipher, current, model, &mut res)?;
        // Two machines that created the same key without seeing each other: one rules and the others become superseded
        // by it, with a verdict that goes out in this same `export`.
        res.claves_repetidas += crate::memoria::resolver_claves_repetidas(db)?;
        res.avisos = sin_repetir(res.choques.values().flatten()).iter().map(Choque::texto).collect();
        // If this version skipped new lines and a newer one used the database before (it went back to an
        // old one), that one will have to reread them when it comes back.
        if res.de_version_nueva > 0 {
            db.execute("UPDATE meta SET v = ?1 WHERE k = 'sync_lector' AND CAST(v AS INTEGER) > ?1", [LECTOR_LINEAS])?;
        }
        res.exportadas = export(db, &r.clon, &machine, config, &cipher)?;
        git(&r.clon, &["add", "-A"])?;
        if !git(&r.clon, &["status", "--porcelain"])?.trim().is_empty() {
            git(&r.clon, &["commit", "--quiet", "--no-verify", "-m", "omoikane"])?;
        }
        let has_commits = has_head(&r.clon);
        let push = if has_commits { git(&r.clon, &["push", "--quiet", "origin", &format!("HEAD:{BRANCH}")]) } else { Ok(String::new()) };
        match push {
            Ok(_) => {
                if has_commits {
                    let head = git(&r.clon, &["rev-parse", "HEAD"])?;
                    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![r.marca_commit, head.trim()])?;
                }
                // What came from other machines may leave local memories outdated (or the other way round).
                let _ = crate::revisar::al_dia(db);
                return Ok((res, cipher));
            }
            // Another machine pushed in between: its changes are pulled and we try again.
            Err(_) if attempt < 3 => std::thread::sleep(Duration::from_millis(200 * (attempt + 1))),
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

// --- Setup: init, join, code ----------------------------------------------------------------------

/// What is chosen when setting up.
#[derive(Clone, Debug)]
pub struct Eleccion {
    pub preferencias: bool,
    pub global: bool,
    /// Project folders; empty = all.
    pub proyectos: Vec<String>,
    /// Spaces (name or key) that also go through the general repository; empty = none.
    pub espacios: Vec<String>,
    pub autor: Option<String>,
}

/// The user's git name (`git config user.name`), if they have one.
pub fn nombre_git() -> Option<String> {
    let o = std::process::Command::new("git").args(["config", "--get", "user.name"]).output().ok()?;
    let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !n.is_empty()).then_some(n)
}

pub fn config_de(url: &str, e: &Eleccion) -> Config {
    config_con_persona(url, e, None, "")
}

/// Like `config_de`, but with the person already decided (`join` uses the inviting machine's: so the
/// projects without a remote of the same person merge across their machines even if each one has a different
/// author name) and, if there is neither an author nor a git name, the database's local person (`local`).
pub fn config_con_persona(url: &str, e: &Eleccion, fixed: Option<&str>, local: &str) -> Config {
    let mut kinds = vec![Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Procedure];
    if e.preferencias {
        kinds.push(Tipo::Preference);
    }
    // The person: the given one; if not, the name's; without a name, a fixed random identifier
    // (stored in the configuration and passed in the code for joining), never one shared by everyone.
    let person = fixed
        .map(persona)
        .filter(|p| !p.is_empty())
        .or_else(|| e.autor.clone().or_else(nombre_git).map(|n| persona(&n)).filter(|p| !p.is_empty()))
        .or_else(|| Some(persona(local)).filter(|p| !p.is_empty()))
        .unwrap_or_else(|| format!("u-{}", ulid::Ulid::generate().to_string()[16..].to_lowercase()));
    let projects = (!e.proyectos.is_empty()).then(|| e.proyectos.iter().map(|p| clave(p, &person)).collect());
    let spaces = (!e.espacios.is_empty()).then(|| e.espacios.iter().map(|s| crate::proyecto::clave_de_espacio(s)).collect());
    Config { url: url.to_string(), kinds, global: e.global, projects, spaces, author: e.autor.clone(), person, space: None, another_person: false }
}

/// After `sync init` or `join`: if the sync person (`new_person`) is not this database's, it becomes
/// it (it is the same person on another machine): the `dir:` keys of the local projects are recomputed
/// with it and their memories are re-stamped, so they travel with the right key, and all
/// this machine's repositories record it. With `--another-person`, the configuration's is already the
/// local one and nothing changes. Returns how many projects changed key.
pub fn adoptar_persona(db: &mut Connection, db_path: &Path, new_person: &str) -> Result<usize> {
    let old_person = crate::db::persona_de(db);
    if new_person.is_empty() || old_person == new_person {
        return Ok(0);
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('persona', ?1)", [new_person])?;
    let n = crate::proyecto::cambiar_persona(&tx, &old_person, new_person)?;
    tx.commit()?;
    let (de, a) = (format!("dir:{old_person}/"), format!("dir:{new_person}/"));
    for r in repos(db_path) {
        let Some(mut c) = leer_config_de(&r)? else { continue };
        c.person = new_person.to_string();
        for k in c.projects.iter_mut().flatten() {
            if let Some(rest) = k.strip_prefix(&de) {
                *k = format!("{a}{rest}");
            }
        }
        write_private(&r.config, &serde_json::to_string_pretty(&c)?)?;
    }
    Ok(n)
}

/// Whether a project's key is already in the general repository, according to this machine's clone (offline):
/// this machine or another pushed it. Without a general repository, no. If its keys can't be read, it is taken
/// as yes (`proyecto::clave_con_espacio` then leaves the key as it was).
pub(crate) fn en_el_general(db: &Connection, key: &str) -> bool {
    let Some(path) = db.path().filter(|p| !p.is_empty()) else { return false };
    let r = rutas(Path::new(path));
    if !r.config.exists() {
        return false;
    }
    let Ok(cipher) = leer_claves(&r) else { return true };
    cipher.keys.values().any(|k| r.clon.join(format!("p-{}", opaque_name(k, &[key.as_bytes()]))).exists())
}

/// A project's key changed (`proyecto::cambiar_clave`): the `projects` list of the
/// repositories that name it switches to the new one.
pub(crate) fn cambiar_clave_en_configs(db_path: &Path, old_key: &str, new_key: &str) -> Result<()> {
    for r in repos(db_path) {
        let Some(mut c) = leer_config_de(&r)? else { continue };
        let Some(p) = c.projects.as_mut().filter(|p| p.iter().any(|k| k == old_key)) else { continue };
        for k in p.iter_mut().filter(|k| *k == old_key) {
            *k = new_key.to_string();
        }
        write_private(&r.config, &serde_json::to_string_pretty(&c)?)?;
    }
    Ok(())
}

fn save_config(db: &Connection, r: &Rutas, config: &Config, cipher: &Cifra) -> Result<()> {
    if let Some(d) = r.config.parent() {
        std::fs::create_dir_all(d)?;
    }
    save_keys(r, cipher)?;
    write_private(&r.config, &serde_json::to_string_pretty(config)?)?;
    // The general repository's author signs what belongs to no space (`meta` `autor`); a
    // space repository's, only that space's (`autor:<space>`, empty if joined without an author):
    // the name someone joins a client with never signs another's (`memoria::autor_en`).
    match &r.espacio {
        None => match &config.author {
            Some(a) => db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', ?1)", [a])?,
            None => db.execute("DELETE FROM meta WHERE k = 'autor'", [])?,
        },
        Some(e) => db.execute(
            "INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)",
            params![format!("autor:{}", crate::proyecto::clave_de_espacio(e)), config.author.as_deref().unwrap_or("")],
        )?,
    };
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    Ok(())
}

fn has_memories_in(clone_dir: &Path) -> bool {
    std::fs::read_dir(clone_dir).into_iter().flatten().flatten().any(|e| {
        e.path().is_dir()
            && !e.file_name().to_string_lossy().starts_with('.')
            && std::fs::read_dir(e.path()).into_iter().flatten().flatten().any(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
    })
}

fn already_in_use() -> String {
    tr!(
        "this repository is already in use by Omoikane: on a computer that already syncs with it run `omoi sync code`, and here `omoi sync join <code>`",
        "Omoikane ya usa este repositorio: en un equipo que ya sincroniza con él ejecuta `omoi sync code` y aquí `omoi sync join <código>`"
    )
}

/// `omoi sync init <url>`: a new repository (empty or without memories), with a new key.
pub fn iniciar(db: &mut Connection, db_path: &Path, config: &Config) -> Result<()> {
    let r = rutas_de(db_path, config.space.as_deref());
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    clone_repo(&config.url, &r.clon)?;
    if r.clon.join(CHECK_FILE).exists() || has_memories_in(&r.clon) {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(already_in_use());
    }
    save_config(db, &r, config, &Cifra::una(1, new_key()?))?;
    after_key_change(db, &r);
    Ok(())
}

/// After changing the general repository's keys (`init`, `join`, a new key from a rotation):
/// the person's verifications keep working with the new seal key
/// (`verificado::tras_cambio_de_claves`). If it fails, it says so and the sync carries on: the worst is that
/// some seal stays as it was.
fn after_key_change(db: &Connection, r: &Rutas) {
    if r.espacio.is_some() {
        return;
    }
    if let Err(e) = crate::verificado::tras_cambio_de_claves(db) {
        eprintln!("omoikane: {}: {e:#}", tr!("verification keys", "claves de verificación"));
    }
}

fn new_key() -> Result<Vec<u8>> {
    let mut key = vec![0u8; 64];
    getrandom::fill(&mut key).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(key)
}

/// The code to join another machine to a repository (the general one or a space's): it carries the URL and
/// the current key with its id (pass it through a private channel!). A repository that never rotated gives the
/// same code as before keys with id (without `id`).
pub fn codigo(db_path: &Path, space: Option<&str>) -> Result<String> {
    let r = rutas_de(db_path, space);
    let config = leer_config_de(&r)?.context(tr!("sync is not set up", "la sincronización no está configurada"))?;
    let cipher = leer_claves(&r)?;
    let mut json = serde_json::json!({ "url": config.url, "key": to_hex(cipher.current_key()), "person": config.person });
    if cipher.vigente() != 1 {
        json["id"] = cipher.vigente().into();
    }
    if let Some(e) = &config.space {
        json["space"] = e.clone().into();
    }
    Ok(format!("{CODE_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())))
}

/// What an `omoi sync code` code carries.
#[derive(Debug, PartialEq)]
pub struct Codigo {
    pub url: String,
    pub clave: Vec<u8>,
    /// The key's id (codes from before keys with id are key 1's).
    pub id: u32,
    /// The inviting machine's person (older codes don't have it).
    pub persona: Option<String>,
    /// The repository's space (`None`: the general one).
    pub espacio: Option<String>,
}

/// URL, key and person of an `omoi sync code` code.
pub fn leer_codigo(code: &str) -> Result<Codigo> {
    let bad = || anyhow!(tr!("that is not an Omoikane sync code", "eso no es un código de sincronización de Omoikane"));
    let rest = code.trim().strip_prefix(CODE_PREFIX).ok_or_else(bad)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(rest).map_err(|_| bad())?;
    let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| bad())?;
    let url = v["url"].as_str().ok_or_else(bad)?.to_string();
    let k = from_hex(v["key"].as_str().ok_or_else(bad)?).ok_or_else(bad)?;
    ensure!(k.len() == 64, bad());
    let id = match v.get("id") {
        None => 1,
        Some(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n >= 1).ok_or_else(bad)?,
    };
    let space = v["space"].as_str().filter(|e| !e.is_empty()).map(espacio_valido).transpose()?;
    Ok(Codigo { url, clave: k, id, persona: v["person"].as_str().filter(|p| !p.is_empty()).map(String::from), espacio: space })
}

/// `omoi sync join <code>`: another machine's repository and key. It checks that the key opens
/// the repository before saving anything.
pub fn unirse(db: &mut Connection, db_path: &Path, config: &Config, id: u32, key: &[u8]) -> Result<()> {
    let r = rutas_de(db_path, config.space.as_deref());
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    clone_repo(&config.url, &r.clon)?;
    let cipher = Cifra::una(id, key.to_vec());
    let vale = match KeyCheck::read(&r.clon) {
        Some(ch) => ch.lines.get(&id).is_some_and(|l| cipher.opens(id, l)),
        // A repository without a check (or still empty): the first line there is.
        None => std::fs::read_dir(&r.clon)?
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
            .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
            .find_map(|f| std::fs::read_to_string(f.path()).ok().and_then(|t| t.lines().next().map(String::from)))
            .is_none_or(|l| cipher.descifrar(&l).is_ok()),
    };
    if !vale {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(wrong_key_msg());
    }
    save_config(db, &r, config, &cipher)?;
    // Another person's general one: whatever was recorded from it before (joined without the flag) is forgotten.
    if config.another_person && r.espacio.is_none() {
        crate::verificado::olvidar_las_del_general(db)?;
    }
    after_key_change(db, &r);
    Ok(())
}

/// `omoi sync join <code>` on a machine that already syncs with that repository (after a rotation):
/// it stores the new key next to the ones it already had, which still serve to read what others haven't
/// re-encrypted yet, and the next sync rereads everything and re-encrypts this
/// machine's memories with it. `false` if it already had it.
pub fn anadir_clave(db: &Connection, db_path: &Path, space: Option<&str>, id: u32, key: &[u8]) -> Result<bool> {
    let r = rutas_de(db_path, space);
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    let other_repo = || {
        anyhow!(tr!(
            "that code is for another repository, and this computer already syncs one here (`omoi sync status`; `omoi sync off` to start over)",
            "ese código es de otro repositorio, y este equipo ya sincroniza uno aquí (`omoi sync status`; `omoi sync off` para empezar de nuevo)"
        ))
    };
    let cipher = leer_claves(&r)?;
    if let Some(k) = cipher.keys.get(&id) {
        ensure!(k.as_slice() == key, other_repo());
        return Ok(false);
    }
    reset_clone(&r.clon)?;
    let new_cipher = Cifra::una(id, key.to_vec());
    ensure!(KeyCheck::read(&r.clon).and_then(|c| c.lines.get(&id).cloned()).is_some_and(|l| new_cipher.opens(id, &l)), other_repo());
    save_keys(&r, &cipher.with_key(id, key.to_vec()))?;
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    after_key_change(db, &r);
    Ok(true)
}

/// Another URL for a repository this machine already syncs with (`OMOIKANE_SYNC_URL` with a new CI
/// token, or `omoi sync join --url`): it records it in its configuration and in its clone's `origin`
/// remote. Returns the one it had (`None` if it already was that one). The URL may carry a token: it doesn't appear in
/// any message.
pub fn cambiar_url(db_path: &Path, space: Option<&str>, url: &str) -> Result<Option<String>> {
    let r = rutas_de(db_path, space);
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    let Some(mut config) = leer_config_de(&r)? else { return Ok(None) };
    if config.url == url {
        return Ok(None);
    }
    if r.clon.join(".git").exists() {
        git(&r.clon, &["remote", "set-url", "origin", url])?;
    }
    let old_url = std::mem::replace(&mut config.url, url.to_string());
    write_private(&r.config, &serde_json::to_string_pretty(&config)?)?;
    Ok(Some(old_url))
}

/// `omoi sync rotate`: a new key for a repository. It records it as current in
/// `omoikane.check`, re-encrypts with it everything from this machine (and deletes its files with the
/// old one), commits and pushes, and stores it here next to the old ones. From then on `omoi sync code` gives
/// the new one's code; whoever only has the old one no longer reads what is written with it. Returns the
/// new key's id.
pub fn rotar(db: &mut Connection, db_path: &Path, space: Option<&str>, current: Option<&str>, model: Option<&Modelo>) -> Result<(u32, Resumen)> {
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    let r = rutas_de(db_path, space);
    let config = leer_config_de(&r)?.context(not_configured())?;
    let new_key = new_key()?;
    let res = sync_repo(db, &r, &config, current, model, Some(&new_key));
    let (res, cipher) = match res {
        Ok((res, cipher)) => (Ok(res), Some(cipher)),
        Err(e) => (Err(e), None),
    };
    // Only once the push is done: otherwise the repository knows nothing about the new key.
    if let Some(c) = &cipher {
        save_keys(&r, c)?;
        after_key_change(db, &r);
    }
    record_status(&r, &res);
    Ok((cipher.map_or(0, |c| c.vigente()), res?))
}

/// Stops syncing a repository (the general one or a space's): deletes its configuration, its
/// keys and its clone (the local memories stay).
pub fn desactivar(db: &Connection, db_path: &Path, space: Option<&str>) -> Result<()> {
    let r = rutas_de(db_path, space);
    let c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    // Before deleting the general one's key: the seal key derived from it stays recorded here, so
    // that the person's verifications made with it keep working (`verificado::recordar`).
    if r.espacio.is_none() {
        crate::verificado::recordar(db)?;
    }
    for f in [&r.config, &r.clave, &r.estado] {
        let _ = std::fs::remove_file(f);
    }
    if r.clon.exists() {
        std::fs::remove_dir_all(&r.clon)?;
    }
    match &r.espacio {
        Some(e) => {
            if let Some(d) = r.config.parent() {
                let _ = std::fs::remove_dir_all(d);
            }
            db.execute("DELETE FROM meta WHERE k = ?1", [format!("autor:{}", crate::proyecto::clave_de_espacio(e))])?;
        }
        None => {
            db.execute("DELETE FROM meta WHERE k = 'autor'", [])?;
        }
    }
    if repos(db_path).is_empty() {
        let _ = std::fs::remove_file(&r.pendiente);
        let _ = std::fs::remove_file(&r.hecho);
    }
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    drop(c);
    Ok(())
}

fn another_running() -> String {
    tr!("another sync is running; try again in a moment", "hay otra sincronización en marcha; prueba dentro de un momento")
}

// --- In the background ----------------------------------------------------------------------------

/// How the last sync went.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Estado {
    pub ultimo_ok: Option<String>,
    pub ultimo_error: Option<String>,
    /// Since when it has been failing (the first time in a row).
    pub error_desde: Option<String>,
    /// Something worth knowing even if it went well (lines that couldn't be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aviso: Option<String>,
    /// Repository files with lines that couldn't be read, and how many: they stay until a
    /// sync rereads the file and it is fine.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_malos: BTreeMap<String, usize>,
    /// The same with lines from a newer version of omoi.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_futuros: BTreeMap<String, usize>,
    /// The same with lines rejected for touching something of another space (`Resumen::conflictos_de_espacio`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_conflicto: BTreeMap<String, usize>,
    /// The same with those of a paused project (`Resumen::en_pausa`): they are reread on every
    /// sync, so they come in as soon as it is decided.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_en_pausa: BTreeMap<String, usize>,
    /// Per file, the projects that are in another space there (`Choque`): they are reread on every
    /// sync until none is left.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_choques: BTreeMap<String, BTreeSet<Choque>>,
}

impl Estado {
    /// How many lines from a newer version are left unread in the repository.
    pub fn de_version_nueva(&self) -> usize {
        self.ficheros_futuros.values().sum()
    }

    /// How many repository lines were rejected for touching something of another space.
    pub fn conflictos_de_espacio(&self) -> usize {
        self.ficheros_conflicto.values().sum()
    }

    /// The projects that are in another space on another machine, one per situation.
    pub fn choques(&self) -> Vec<Choque> {
        sin_repetir(self.ficheros_choques.values().flatten())
    }
}

/// The projects in another space on another machine, from all this machine's repositories (for
/// Review, `omoi review` and `doctor`).
pub fn choques(db_path: &Path) -> Vec<Choque> {
    let all: Vec<Choque> = repos(db_path).iter().flat_map(|r| leer_estado_de(r).choques()).collect();
    sin_repetir(&all)
}

/// How the general repository's last sync went.
pub fn leer_estado(db_path: &Path) -> Estado {
    leer_estado_de(&rutas(db_path))
}

pub fn leer_estado_de(r: &Rutas) -> Estado {
    std::fs::read_to_string(&r.estado).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn now() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    s.to_string()
}

fn seconds(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

fn record_status(r: &Rutas, result: &Result<Resumen>) {
    let mut e = leer_estado_de(r);
    match result {
        Ok(r) => {
            e.ultimo_ok = Some(now());
            e.ultimo_error = None;
            e.error_desde = None;
            for (counts, fresh) in
                [(&mut e.ficheros_malos, &r.malos), (&mut e.ficheros_futuros, &r.futuros), (&mut e.ficheros_conflicto, &r.conflictos), (&mut e.ficheros_en_pausa, &r.pausadas)]
            {
                for (f, n) in fresh {
                    if *n == 0 {
                        counts.remove(f);
                    } else {
                        counts.insert(f.clone(), *n);
                    }
                }
            }
            // Those of each file read (they all go through `malos`), instead of those from the previous time.
            for f in r.malos.keys() {
                match r.choques.get(f).filter(|c| !c.is_empty()) {
                    Some(c) => e.ficheros_choques.insert(f.clone(), c.clone()),
                    None => e.ficheros_choques.remove(f),
                };
            }
            let bad_lines: usize = e.ficheros_malos.values().sum();
            let newer_lines = e.de_version_nueva();
            let mut notices = Vec::new();
            if bad_lines > 0 {
                notices.push(tr!(
                    "{} shared line(s) couldn't be read and were skipped (another key, or not valid)",
                    "{} línea(s) compartidas no se pudieron leer y se saltaron (otra clave, o no válidas)",
                    bad_lines
                ));
            }
            if newer_lines > 0 {
                notices.push(aviso_lineas_nuevas(newer_lines));
            }
            let conflicts = e.conflictos_de_espacio();
            if conflicts > 0 {
                notices.push(tr!(
                    "{} shared line(s) rejected: they touch a project or memory that is in another space here (nothing from one space reaches another)",
                    "{} línea(s) compartidas rechazadas: tocan un proyecto o una memoria que aquí es de otro espacio (nada de un espacio llega a otro)",
                    conflicts
                ));
            }
            let paused: usize = e.ficheros_en_pausa.values().sum();
            if paused > 0 {
                notices.push(tr!(
                    "{} shared line(s) held back: their project is paused here (they come in once you decide)",
                    "{} línea(s) compartidas en espera: su proyecto está aquí en pausa (entran en cuanto decidas)",
                    paused
                ));
            }
            // Which project and which command fixes it.
            notices.extend(e.choques().iter().map(Choque::texto));
            e.aviso = (!notices.is_empty()).then(|| notices.join(" · "));
        }
        Err(err) => {
            e.ultimo_error = Some(sin_credenciales(&format!("{err:#}")));
            e.error_desde.get_or_insert_with(now);
        }
    }
    if let Ok(t) = serde_json::to_string_pretty(&e) {
        let _ = std::fs::write(&r.estado, t);
    }
}

/// There are unpushed changes: the last change marker is not the one already synced.
pub fn pendiente(db_path: &Path) -> bool {
    let r = rutas(db_path);
    match std::fs::read_to_string(&r.pendiente) {
        Ok(m) => std::fs::read_to_string(&r.hecho).ok().as_deref() != Some(m.as_str()),
        Err(_) => false,
    }
}

/// The first line of an error, without credentials or whatever the server adds (`remote: …`), which must
/// not reach the agent's context.
fn short_error(e: &str) -> String {
    let l = e.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("remote:")).unwrap_or_default();
    let l = l.split("remote:").next().unwrap_or_default().trim();
    sin_credenciales(&l.chars().take(200).collect::<String>())
}

/// «N lines from a newer version: update omoi».
pub fn aviso_lineas_nuevas(n: usize) -> String {
    tr!(
        "{} line(s) from a newer version: update omoi (`omoi upgrade`)",
        "{} línea(s) de una versión más nueva: actualiza omoi (`omoi upgrade`)",
        n
    )
}

/// A notice for session start: if some repository asks for a newer version of omoi (as
/// soon as it is known), or if sync has been failing for more than an hour.
pub fn aviso(db_path: &Path) -> Option<String> {
    if repos(db_path).iter().any(|r| necesita_version(r).is_some()) {
        return Some(
            "An Omoikane sync repository needs a newer version of omoi: nothing is written to it until omoi is updated (`omoi upgrade`). Tell the user; `omoi sync status` shows more."
                .into(),
        );
    }
    leer_config(db_path).ok().flatten()?;
    let e = leer_estado(db_path);
    let desde = seconds(e.error_desde.as_deref()?);
    let now = seconds(&now());
    (now.saturating_sub(desde) >= 3600).then(|| {
        format!(
            "Omoikane sync has been failing for {} h (last error: {}). Tell the user; `omoi sync status` shows more.",
            (now - desde) / 3600,
            short_error(e.ultimo_error.as_deref().unwrap_or_default())
        )
    })
}

/// After a change: if sync is set up, leaves a new pending marker and launches
/// `omoi sync --background` separately, without waiting. It never fails (the worst is not syncing now).
pub fn programar(db_path: &Path) {
    // What changed also needs its meaning vector (if the model is there): let the
    // service compute it now, in the background.
    crate::significado::avisar(db_path);
    let r = rutas(db_path);
    // `OMOIKANE_SYNC_AUTO=0` (or nobody at the keyboard without `=1`): only by hand (`omoi sync`).
    if (!r.config.exists() && espacios(db_path).is_empty()) || !crate::entorno::sync_automatica() {
        return;
    }
    // Each change, a different marker: the background process repeats while the marker is not the one it already
    // synced (never deleting it, so one arriving right in between isn't lost).
    let mark = ulid::Ulid::generate().to_string();
    if std::fs::write(&r.pendiente, mark).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(["sync", "--background"])
        .env("OMOIKANE_DB", db_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP: with its own hidden console (so the git
        // commands it launches don't open windows either) and without dying with a Ctrl+C from whoever launches it.
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_inherit_handles();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a Ctrl+C in the terminal of whoever launched it doesn't stop it.
        c.process_group(0);
    }
    if let Ok(mut child) = c.spawn() {
        // A thread waits for the child: in long-lived processes (`omoi mcp`, `omoi serve`) no zombie
        // processes are left; in a short one, on exit, the system adopts it.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Launches `omoi <args>` separately, without a window, without waiting for it and without it holding on to the outputs of
/// the caller (a hook reads them until the end). Used by the daily update (`actualizar`).
pub(crate) fn lanzar_aparte(args: &[&str]) {
    lanzar(args, None);
}

/// Like `lanzar_aparte`, and with `db_path` the child opens that database (`OMOIKANE_DB`), not the default one.
pub(crate) fn lanzar(args: &[&str], db_path: Option<&Path>) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    if let Some(r) = db_path {
        c.env("OMOIKANE_DB", r);
    }
    // The child knows it is a background one: its `--pull` is the cheap one even if nobody is at the keyboard (`traer`).
    c.env(crate::entorno::DE_FONDO, "1");
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_inherit_handles();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    if let Ok(mut child) = c.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// On Windows a child process inherits every inheritable handle, including this process's input and
/// outputs even if the child is given others. If whoever called `omoi` (a hook, an agent) reads
/// those outputs, it would wait for the background sync to finish. This process ends
/// right away, so they are marked as non-inheritable before launching it.
#[cfg(windows)]
fn no_inherit_handles() {
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn SetHandleInformation(h: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE and STD_ERROR_HANDLE (-10, -11, -12).
    for n in [-10i32, -11, -12] {
        // SAFETY: Windows API calls with a handle the system itself returns.
        unsafe {
            let h = GetStdHandle(n as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Whether a process is still alive.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks whether the process exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(h: isize, exit_code: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: Windows API calls; the handle is always closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut exit_code = 0u32;
        let ok = GetExitCodeProcess(h, &mut exit_code);
        CloseHandle(h);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn alive(_pid: u32) -> bool {
    true
}

/// One lock per machine: a file created exclusively with the pid, the time and a token of its own. One
/// from a process that no longer exists, or not renewed in 10 minutes, is considered abandoned. While held,
/// a thread renews it; on release it is only deleted if it is still ours.
pub struct Cerrojo {
    path: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const LOCK_EXPIRY_SECS: u64 = 600;

impl Cerrojo {
    fn contents(token: &str) -> String {
        format!("{}\n{}\n{token}\n", std::process::id(), now())
    }

    fn abandoned(p: &Path) -> bool {
        let Ok(t) = std::fs::read_to_string(p) else { return false };
        let mut l = t.lines();
        let pid = l.next().and_then(|x| x.trim().parse::<u32>().ok());
        let when = l.next().map(|x| seconds(x.trim())).unwrap_or(0);
        let stale = seconds(&now()).saturating_sub(when) > LOCK_EXPIRY_SECS;
        match pid {
            Some(pid) => !alive(pid) || stale,
            // A half-written lock (or from an earlier version, empty): by its date.
            None => std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(LOCK_EXPIRY_SECS)),
        }
    }

    pub fn tomar(db_path: &Path) -> Option<Cerrojo> {
        Cerrojo::tomar_en(rutas(db_path).cerrojo)
    }

    /// Whether someone holds the lock at `p` (and it isn't abandoned).
    pub fn ocupado(p: &Path) -> bool {
        p.exists() && !Cerrojo::abandoned(p)
    }

    /// Like `tomar`, with another lock file (the one for computing trade duplicates).
    pub fn tomar_en(p: PathBuf) -> Option<Cerrojo> {
        let token = ulid::Ulid::generate().to_string();
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(f) => {
                    use std::io::Write;
                    let mut f = f;
                    let _ = f.write_all(Cerrojo::contents(&token).as_bytes());
                    drop(f);
                    let stop = Arc::new(AtomicBool::new(false));
                    let thread = {
                        let (p, token, stop) = (p.clone(), token.clone(), stop.clone());
                        std::thread::spawn(move || {
                            let mut n = 0u32;
                            while !stop.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(200));
                                n += 1;
                                // Every minute, if it is still ours, the time is renewed.
                                if n.is_multiple_of(300) && std::fs::read_to_string(&p).is_ok_and(|t| t.contains(&token)) {
                                    let _ = std::fs::write(&p, Cerrojo::contents(&token));
                                }
                            }
                        })
                    };
                    return Some(Cerrojo { path: p, token, stop, thread: Some(thread) });
                }
                Err(_) => {
                    let seen = std::fs::read_to_string(&p).ok();
                    if !Cerrojo::abandoned(&p) {
                        return None;
                    }
                    // It is moved aside with a name of its own (renaming is atomic: if two see it abandoned,
                    // only one succeeds) and it is checked that what was moved aside is what was judged abandoned.
                    let moved_aside = p.with_extension(format!("lock.{token}"));
                    if std::fs::rename(&p, &moved_aside).is_err() {
                        return None;
                    }
                    if std::fs::read_to_string(&moved_aside).ok() != seen {
                        // Another one had taken it in between: it is given back.
                        let _ = std::fs::rename(&moved_aside, &p);
                        return None;
                    }
                    let _ = std::fs::remove_file(&moved_aside);
                }
            }
        }
        None
    }

    /// Waits up to `max` for it to be free.
    pub fn esperar(db_path: &Path, max: Duration) -> Option<Cerrojo> {
        let start = Instant::now();
        loop {
            if let Some(c) = Cerrojo::tomar(db_path) {
                return Some(c);
            }
            if start.elapsed() >= max {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Cerrojo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t.contains(&self.token)) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn wait_time() -> Duration {
    Duration::from_millis(std::env::var("OMOIKANE_SYNC_ESPERA_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000))
}

/// Records that change marker `mark` is already synced.
fn mark_done(r: &Rutas, mark: Option<&str>) {
    if let Some(m) = mark {
        let _ = std::fs::write(&r.hecho, m);
    }
}

/// `omoi sync --background`: waits a few seconds to batch changes, syncs, and repeats if
/// more arrived meanwhile. If another one is already running, it leaves (that one will see the new marker). `open`
/// doesn't need to load the model: whatever arrives without a vector is completed by the next command that opens it.
pub fn en_segundo_plano(db_path: &Path, abrir: impl Fn() -> Result<(Connection, Option<Modelo>)>) -> Result<()> {
    let Some(_lock) = Cerrojo::tomar(db_path) else { return Ok(()) };
    let r = rutas(db_path);
    for _ in 0..20 {
        std::thread::sleep(wait_time());
        let mark = std::fs::read_to_string(&r.pendiente).ok();
        // `sincronizar` records how each repository went; a failure opening the database, in the general one.
        let result = match abrir() {
            Ok((mut db, model)) => sincronizar(&mut db, db_path, None, model.as_ref()),
            Err(e) => {
                let e = Err(e);
                record_status(&r, &e);
                e
            }
        };
        if result.is_err() {
            break;
        }
        mark_done(&r, mark.as_deref());
        if !pendiente(db_path) {
            break;
        }
    }
    Ok(())
}

/// `omoi sync` by hand: waits for a background one to finish if there is one, and records the result.
pub fn ahora_mismo(db: &mut Connection, db_path: &Path, current: Option<&str>, model: Option<&Modelo>) -> Result<Resumen> {
    let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
    let r = rutas(db_path);
    // A change made meanwhile launches a background sync that leaves right away (this one holds
    // the lock): it repeats here while the pending marker moves forward.
    let mut rounds = 0;
    loop {
        let mark = std::fs::read_to_string(&r.pendiente).ok();
        let res = sincronizar(db, db_path, current, model)?;
        mark_done(&r, mark.as_deref());
        rounds += 1;
        if !pendiente(db_path) || rounds >= 5 {
            return Ok(res);
        }
    }
}

// --- Pulling others' changes mid-session ----------------------------------------------------------

/// The `meta` key with the last time (Unix seconds) a pull was launched from here.
const PULL_MARK: &str = "sync_traido";

/// How often, at most, to check whether the remote brings something (`OMOIKANE_SYNC_EVERY`, in seconds; 120).
pub fn cada_cuanto() -> Duration {
    Duration::from_secs(std::env::var("OMOIKANE_SYNC_EVERY").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(120))
}

/// Whether sync is set up and automatic (`entorno::sync_automatica`). It only checks whether
/// some files exist: without sync it costs nothing.
pub fn automatica(db_path: &Path) -> bool {
    crate::entorno::sync_automatica() && (rutas(db_path).config.exists() || !espacios(db_path).is_empty())
}

/// Records that a pull just happened (or that a full sync was launched, at session start).
pub fn apuntar_traido(db: &Connection) {
    let _ = db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![PULL_MARK, now()]);
}

/// On every message (and in `omoi serve` with a timer): if sync is automatic and it has been
/// more than `cada_cuanto` since the last check, launches `omoi sync --pull` separately and returns right away. The `meta`
/// marker is taken with a conditional write, so with several sessions at once only one
/// launches the process. It never fails: the worst is that the new changes arrive on the next attempt.
pub fn traer_si_toca(db: &Connection, db_path: &Path) -> bool {
    if !automatica(db_path) {
        return false;
    }
    let now = seconds(&now()) as i64;
    let every = cada_cuanto().as_secs() as i64;
    // Usually it isn't due yet: one read and nothing more.
    let last: Option<i64> = db.query_row("SELECT v FROM meta WHERE k = ?1", [PULL_MARK], |f| f.get::<_, String>(0)).optional().ok().flatten().and_then(|v| v.trim().parse().ok());
    // A marker in the future (the clock went back) must not stop pulling forever either.
    if last.is_some_and(|u| u <= now && now - u < every) {
        return false;
    }
    let claimed = db
        .execute(
            "INSERT INTO meta(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v
             WHERE CAST(meta.v AS INTEGER) <= ?3 OR CAST(meta.v AS INTEGER) > ?4",
            params![PULL_MARK, now.to_string(), now - every, now],
        )
        .is_ok_and(|n| n == 1);
    if claimed {
        lanzar(&["sync", "--pull"], Some(db_path));
    }
    claimed
}

/// Whether a repository's remote has commits this database hasn't read yet: a `git fetch` and
/// a comparison with the last commit read. Gives that remote commit, or nothing on any doubt (no
/// clone, offline).
fn remote_news(db: &Connection, r: &Rutas) -> Option<String> {
    if !r.clon.join(".git").exists() || !matches!(leer_config_de(r), Ok(Some(_))) {
        return None;
    }
    git(&r.clon, &["fetch", "--quiet", "origin"]).ok()?;
    let remote = git(&r.clon, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{BRANCH}")]).ok()?.trim().to_string();
    let last_read: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [&r.marca_commit], |f| f.get(0)).optional().ok().flatten();
    (last_read.as_deref().map(str::trim) != Some(remote.as_str())).then_some(remote)
}

/// The `meta` key with the last remote commit for which a full sync was attempted
/// from a pull (`sync_intentado`, `sync_intentado:<space>`).
fn attempted_key(r: &Rutas) -> String {
    r.marca_commit.replacen("sync_commit", "sync_intentado", 1)
}

/// How often the same thing is retried (the same remote commit, the same pending marker) after an
/// attempt that didn't solve it: a passing network outage doesn't leave it stuck until something changes.
const RETRY_SECS: u64 = 30 * 60;

/// Whether `value` was already tried in `key` less than `RETRY_SECS` ago (`meta`: `value@seconds`).
fn recently_tried(db: &Connection, key: &str, value: &str) -> bool {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let v: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [key], |f| f.get(0)).optional().ok().flatten();
    v.and_then(|v| {
        let (before, when) = v.rsplit_once('@')?;
        let when: u64 = when.parse().ok()?;
        Some(before == value && now.saturating_sub(when) < RETRY_SECS && when <= now)
    })
    .unwrap_or(false)
}

fn record_attempt(db: &Connection, key: &str, value: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![key, format!("{value}@{now}")]);
}

/// The `meta` key with the pending marker a pull tried to push.
const PENDING_TRIED: &str = "sync_pendiente_intentado";

/// `omoi sync --pull`: if no other sync is running, checks each repository with a `git
/// fetch`; only if some brings something (or there are unpushed local changes) does it do the full sync.
/// What was already tried and not solved (the same remote commit, the same pending marker: it asks for
/// a newer version, a clash that doesn't solve itself) is not repeated every 2 minutes, but when
/// it changes or after half an hour.
pub fn traer(db_path: &Path, abrir: impl Fn() -> Result<Connection>) -> Result<()> {
    // With nobody at the keyboard (a CI asking for `omoi sync --pull`) «another one will do it» or «offline, on
    // the next attempt» won't do: it waits for the running one, syncs fully and, if it fails, exits with an error.
    // The one `omoi` itself launches in the background (with `OMOIKANE_SYNC_AUTO=1` on a server) is still
    // the cheap one: nobody waits for its result.
    if crate::entorno::desatendido() && !crate::entorno::de_fondo() {
        let _c = Cerrojo::esperar(db_path, Duration::from_secs(60)).context(another_running())?;
        let mut db = abrir()?;
        let r = rutas(db_path);
        let mark = std::fs::read_to_string(&r.pendiente).ok();
        sincronizar(&mut db, db_path, None, None)?;
        mark_done(&r, mark.as_deref());
        return Ok(());
    }
    // If another is running, that one already pulls the new changes.
    let Some(_lock) = Cerrojo::tomar(db_path) else { return Ok(()) };
    let mut db = abrir()?;
    let mut found_new = false;
    for r in repos(db_path) {
        let Some(remote) = remote_news(&db, &r) else { continue };
        let key = attempted_key(&r);
        if !recently_tried(&db, &key, &remote) {
            found_new = true;
            record_attempt(&db, &key, &remote);
        }
    }
    // The pending marker is left by every local change and also by every session start; if
    // sync fails it is never considered done, so it counts as new only once.
    if pendiente(db_path)
        && let Ok(mark) = std::fs::read_to_string(&rutas(db_path).pendiente)
        && !recently_tried(&db, PENDING_TRIED, mark.trim())
    {
        found_new = true;
        record_attempt(&db, PENDING_TRIED, mark.trim());
    }
    if !found_new {
        return Ok(());
    }
    // As in `en_segundo_plano`: a local change arriving meanwhile doesn't launch another one (this one
    // holds the lock), so it repeats while the pending marker moves forward.
    let r = rutas(db_path);
    for _ in 0..5 {
        let mark = std::fs::read_to_string(&r.pendiente).ok();
        if sincronizar(&mut db, db_path, None, None).is_err() {
            break;
        }
        mark_done(&r, mark.as_deref());
        if !pendiente(db_path) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_remotes() {
        let a = normalizar_remoto("git@github.com:Marc/Tienda.git");
        assert_eq!(a, "github.com/marc/tienda");
        assert_eq!(normalizar_remoto("https://github.com/marc/tienda"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com/marc/tienda.git/"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com:22/marc/tienda.git"), a);
    }

    #[test]
    fn deterministic_encryption_and_wrong_key() {
        let c = Cifra::una(1, (0..64).collect());
        let x = c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap();
        assert_eq!(x, c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap(), "la misma línea cifra igual");
        assert_eq!(c.descifrar(&x).unwrap(), "{\"hola\":\"mundo, una línea\"}");
        let other = Cifra::una(1, (1..65).collect());
        assert!(other.descifrar(&x).is_err());
        assert_ne!(c.folder("git:a/b"), other.folder("git:a/b"));
        assert!(c.file("marc-pc-abc").starts_with("m-") && !c.file("marc-pc-abc").contains("marc"));
        assert_ne!(c.file("a"), c.file("b"));
        assert!(c.folder("git:a/b").starts_with("p-"));
    }

    #[test]
    fn code_round_trip() {
        let k: Vec<u8> = (0..64).collect();
        let json = serde_json::json!({ "url": "git@x:y/z.git", "key": to_hex(&k) });
        let c = format!("{CODE_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap(), Codigo { url: "git@x:y/z.git".to_string(), clave: k.clone(), id: 1, persona: None, espacio: None });
        let json = serde_json::json!({ "url": "u", "key": to_hex(&k), "person": "marc" });
        let c = format!("{CODE_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap().persona.as_deref(), Some("marc"));
        assert!(leer_codigo("hola").is_err());
    }

    #[test]
    fn persons() {
        assert_eq!(persona("Marc Mayol"), "marc-mayol");
        assert_eq!(persona("  "), "");
        let e = Eleccion { preferencias: false, global: true, proyectos: vec![], espacios: vec![], autor: Some(" ".into()) };
        let c = config_de("x", &e);
        assert!(c.person.starts_with("u-") || !c.person.is_empty(), "nunca vacía: {}", c.person);
        assert_eq!(config_con_persona("x", &e, Some("Ana López"), "").person, "ana-lópez");
        assert_eq!(config_con_persona("x", &e, None, "u-1234abcd").person, "u-1234abcd", "sin autor, la persona local");
        assert!(valid_id("01M62EWVHRXM0000000000ABCD"));
        assert!(!valid_id("01m62ewvhrxm0000000000abcd") && !valid_id("CORTO") && !valid_id("01M62EWVHRXM0000000000ABCÑ"));
    }

    #[test]
    fn credentials_stripped() {
        assert_eq!(sin_credenciales("https://ghp_x:y@github.com/a/b.git"), "https://***@github.com/a/b.git");
        assert_eq!(sin_credenciales("git clone https://tok@host/r falló; ssh://git@h:22/r"), "git clone https://***@host/r falló; ssh://***@h:22/r");
        assert_eq!(sin_credenciales("git@github.com:a/b.git"), "git@github.com:a/b.git");
        // Tokens in the query too, and only the parameters that sound like a credential.
        assert_eq!(
            sin_credenciales("fallo en https://h.com/a/b.git?access_token=s3cr3t&ref=main#x y más"),
            "fallo en https://h.com/a/b.git?access_token=***&ref=main#x y más"
        );
        assert_eq!(sin_credenciales("https://u:p@h.com?private_token=abc&sig=zz"), "https://***@h.com?private_token=***&sig=***");
        assert_eq!(sin_credenciales("https://h.com/r?token=t1 'https://h.com/q?Token=T2'"), "https://h.com/r?token=*** 'https://h.com/q?Token=***'");
        assert_eq!(sin_credenciales("https://h.com/buscar?q=deploy"), "https://h.com/buscar?q=deploy");
        assert_eq!(short_error("git push: remote: Permission to x denied to ghp\nmás"), "git push:");
    }

    #[test]
    fn paths_per_database() {
        let a = rutas(Path::new("/x/omoikane.db"));
        assert!(a.config.ends_with("sync.json") && a.clon.ends_with("sync"));
        let b = rutas(Path::new("/x/trabajo.db"));
        assert!(b.config.ends_with("trabajo.sync.json") && b.clon.ends_with("trabajo.sync") && b.cerrojo.ends_with("trabajo.sync.lock"));
        let e = rutas_de(Path::new("/x/trabajo.db"), Some("acme"));
        assert!(e.config.ends_with("trabajo.sync-spaces/acme/sync.json") && e.clon.ends_with("trabajo.sync-spaces/acme/repo"), "{}", e.config.display());
        assert_eq!(e.cerrojo, b.cerrojo, "un solo cerrojo por base");
        assert_ne!(e.marca_commit, b.marca_commit);
        assert_eq!(espacio_valido(" Acme-2 ").unwrap(), "acme-2");
        assert!(espacio_valido("../x").is_err() && espacio_valido("").is_err() && espacio_valido("-x").is_err());
    }

    #[test]
    fn lines_with_key_id() {
        let k1: Vec<u8> = (0..64).collect();
        let k2: Vec<u8> = (64..128).collect();
        let old = Cifra::una(1, k1.clone());
        let both = old.with_key(2, k2.clone());
        assert_eq!(both.vigente(), 2);
        // Key 1 without prefix (as before); the others with `k<id>:`.
        let l1 = old.cifrar("hola").unwrap();
        assert!(!l1.contains(':'));
        let l2 = both.cifrar("hola").unwrap();
        assert!(l2.starts_with("k2:"), "{l2}");
        assert_eq!(both.descifrar(&l1).unwrap(), "hola", "con las dos se lee lo viejo");
        assert_eq!(both.descifrar(&l2).unwrap(), "hola");
        assert!(old.descifrar(&l2).unwrap_err().to_string().contains("k2"), "sin la nueva no se lee, y lo dice");
        assert_eq!(old.descifrar(&format!("k1:{l1}")).unwrap(), "hola", "`k1:` también vale");
        assert!(both.descifrar(&format!("x9:{l1}")).is_err(), "una etiqueta desconocida es de un formato futuro");
        assert_eq!(Cifra::una(2, k2.clone()).file("pc"), both.file("pc"));
        assert_eq!(both.old_files("pc"), vec![old.file("pc")]);
    }

    #[test]
    fn old_check_and_multi_key_check() {
        let k1: Vec<u8> = (0..64).collect();
        let old = Cifra::una(1, k1.clone());
        // With only key 1: one line, as always.
        let mut ch = KeyCheck::default();
        ch.lines.insert(1, old.encrypt_with(1, &check_text(1)).unwrap());
        let t = ch.texto();
        assert_eq!(t.lines().count(), 1);
        assert!(KeyCheck::parse(&t).verify(&old) == (1, vec![]));
        // With key 2: the header with the current one, and whoever only has key 1 knows it exists.
        let both = old.with_key(2, (64..128).collect());
        ch.lines.insert(2, both.encrypt_with(2, &check_text(2)).unwrap());
        let t = ch.texto();
        assert!(t.starts_with("omoikane-check-v2 current=2\n"), "{t}");
        let parsed = KeyCheck::parse(&(t.clone() + "omoikane-check-v3 algo=nuevo\nz7:futuro\n"));
        assert_eq!(parsed.vigente(), 2);
        assert_eq!(parsed.verify(&old), (1, vec![]));
        assert_eq!(parsed.verify(&both), (2, vec![]));
        // Another key with the same id doesn't pass, nor does one key's line placed as another's.
        assert_eq!(parsed.verify(&old.with_key(2, (1..65).collect())), (1, vec![2]));
        assert!(!both.opens(2, &ch.lines[&1]));
    }
}
