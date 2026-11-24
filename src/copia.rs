//! Faithful copies (0.18.0): `omoi export` writes everything needed to rebuild the database
//! (memories with their history, forget marks, verdicts and trash) and `omoi import` puts it back
//! with the same rules as sync (`sync::Aplicador`): the same ids, dates and stamps, the newest wins
//! and what was forgotten doesn't come back. So an import is idempotent and works both to restore a
//! database and to merge two.
//!
//! Format `omoi-export`, version 2: JSONL, one thing per line and streamed (one line in memory at a
//! time, when writing and when reading). The first line is the header and the last the end line, with
//! how many lines go in between and their SHA-256: a cut or tampered file isn't imported. The lines
//! are sync's (`sync::Linea`) and grow just like them: a field this version doesn't know is kept and
//! exported again (`sync::Extra`), and a line of a type it doesn't know goes into the SHA, is counted
//! (`unknown`) and skipped, instead of stopping the import. The format version only goes up with a
//! change an old version can't read properly; adding doesn't bump it.
//! Everything goes: the memories, the forget marks, the trash (each memory with its project and its
//! space by key, like the live ones) and all the verdicts, also those of what is in the trash (after
//! it, so that on import they find their memory).
//! With `--encrypt`, each line is encrypted with sync's current key (AES-SIV) and with its id in
//! front (`k2:…`; with nothing, key 1), as in the repository, to keep the copy anywhere: the key of
//! the repository of the exported scope's space (a client's copy, with that client's key) or, without
//! a space, the general one's. With `--all` (1.9.0), each line with its own space's: acme's stuff with
//! acme's, beta's with beta's and what belongs to no space with the general one's; if some key is
//! missing, error before writing anything (before, everything went with the general one's). The format
//! doesn't change: on import, each line is opened with the first key of the computer that opens it,
//! also in earlier versions, which already did so. A computer missing any of those keys gets nothing
//! applied: the import reads the whole file before applying it and fails at the first line it can't
//! open.
//!
//! And the daily copy: once a day, when the first session starts, `VACUUM INTO` to
//! `<database folder>/backups/omoikane-YYYY-MM-DD.db`, keeping the last `OMOIKANE_BACKUPS`
//! (7; 0 = off), and along the way the trash is emptied of whatever has been there longer than
//! `OMOIKANE_TRASH_DAYS` (30; 0 = never).

use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, COLUMNAS, NCOL, de_fila};
use crate::sync::{self, Aplicador, Cifra, Linea, Resumen};
use crate::tr;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const FORMATO: &str = "omoi-export";
pub const VERSION: u32 = 2;

/// How many lines of each thing (for `--dry-run` and to count what was exported).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Cuenta {
    pub memories: usize,
    pub tombstones: usize,
    pub verdicts: usize,
    pub forgotten: usize,
    /// `project` lines (sync's; an import doesn't apply them).
    #[serde(skip_serializing_if = "is_zero")]
    pub projects: usize,
    /// Lines of a type this version doesn't know (from a newer omoi): they are skipped.
    pub unknown: usize,
    /// Memories with the verified-by-a-person stamp that come in without it (importing from an agent's
    /// shell). Doesn't count in `total`: they are memories of `memories` or `forgotten`.
    #[serde(skip_serializing_if = "is_zero")]
    pub unverified: usize,
    /// Memories with someone else's verified stamp (not signed by whoever imports) that come in without
    /// it, without `--trust-verified`. Doesn't count in `total` either.
    #[serde(skip_serializing_if = "is_zero")]
    pub untrusted_verified: usize,
}

impl Cuenta {
    pub fn total(&self) -> usize {
        self.memories + self.tombstones + self.verdicts + self.forgotten + self.projects + self.unknown
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What each line of a copy is encrypted with.
enum Encryption {
    /// Without `--encrypt`.
    None,
    /// Everything with the keys of one repository: the scope's space's, or the general one.
    Single(Cifra),
    /// `--all`: each line with the keys of its space's repository (`""`: the general one). The end
    /// line, which belongs to no space (and carries nothing of any), goes with the general one's or,
    /// without it, with the first space's. A verdict of a memory of which nothing is left here is left out.
    PerSpace(std::collections::BTreeMap<String, Cifra>),
}

impl Encryption {
    /// The keys for a line of the space `space` (its key; `None`: of no particular one).
    fn for_space(&self, space: Option<&str>) -> Option<&Cifra> {
        match self {
            Encryption::None => None,
            Encryption::Single(c) => Some(c),
            Encryption::PerSpace(v) => match space.and_then(|e| v.get(e)) {
                Some(c) => Some(c),
                None => v.get("").or_else(|| v.values().next()),
            },
        }
    }

    fn encrypt(&self, space: Option<&str>, text: &str) -> Result<String> {
        match self.for_space(space) {
            Some(c) => c.cifrar(text),
            None => Ok(text.to_string()),
        }
    }
}

/// The spaces (their key; `""`: none) of everything a copy of everything carries: live memories, those
/// in the trash and forget marks. Verdicts go with their memory.
fn spaces_of_everything(db: &Connection) -> Result<std::collections::BTreeSet<String>> {
    let mut v = std::collections::BTreeSet::new();
    for query in [
        "SELECT DISTINCT COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '') FROM memory m",
        "SELECT DISTINCT space_key FROM tombstone",
    ] {
        let mut q = db.prepare(query)?;
        for e in q.query_map([], |r| r.get::<_, String>(0))? {
            v.insert(e?);
        }
    }
    let mut q = db.prepare("SELECT memory FROM forgotten")?;
    for t in q.query_map([], |r| r.get::<_, String>(0))? {
        v.insert(sync::espacio_de_papelera(db, &t?)?);
    }
    Ok(v)
}

/// `--all --encrypt`: the keys of each space the copy carries (and the general one's for what belongs
/// to none). Error, before writing anything, with the spaces whose repository isn't on this computer:
/// a client's stuff never goes with the personal key or with another client's.
fn keys_per_space(db: &Connection, db_path: &Path) -> Result<Encryption> {
    let mut v = std::collections::BTreeMap::new();
    let general = sync::rutas(db_path);
    if general.clave.exists() {
        v.insert(String::new(), sync::leer_claves(&general)?);
    }
    let mut missing = Vec::new();
    for space in spaces_of_everything(db)? {
        if space.is_empty() {
            if !v.contains_key("") {
                missing.push(tr!("no space (the general repository)", "ninguno (el repositorio general)"));
            }
            continue;
        }
        match sync::repo_de_espacio(db_path, &space).filter(|r| r.clave.exists()) {
            Some(r) => {
                v.insert(space, sync::leer_claves(&r)?);
            }
            None => missing.push(space),
        }
    }
    ensure!(
        missing.is_empty(),
        tr!(
            "encrypting a copy of everything puts each space under the key of its own sync repository, and this computer has none for: {}. Set them up first (`omoi sync init <git url> --space <space>`, or `omoi sync init <git url>` for the general one) or export without --encrypt",
            "una copia cifrada de todo lleva cada espacio con la clave de su propio repositorio de sincronización, y este equipo no tiene la de: {}. Configúralos antes (`omoi sync init <url de git> --space <espacio>`, u `omoi sync init <url de git>` para el general) o exporta sin --encrypt",
            missing.join(", ")
        )
    );
    ensure!(
        !v.is_empty(),
        tr!(
            "encrypting needs the key of the general sync repository: set up `omoi sync init <git url>` (or `omoi sync join <code>`) first",
            "para cifrar hace falta la clave del repositorio general de la sincronización: configura antes `omoi sync init <url de git>` (o `omoi sync join <código>`)"
        )
    );
    Ok(Encryption::PerSpace(v))
}

/// The keys a copy of this scope is encrypted with: those of its space's repository (from an acme
/// project, acme's: a client's copy isn't opened with the personal key) or, from something without a
/// space, the general one's. With `--all`, each space's (`keys_per_space`).
fn keys_for(db: &Connection, db_path: &Path, scope: &Ambito) -> Result<Cifra> {
    let space = crate::proyecto::clave_espacio(db, scope.espacio())?;
    if space.is_empty() {
        let r = sync::rutas(db_path);
        ensure!(
            r.clave.exists(),
            tr!(
                "encrypting needs the key of the general sync repository: set up `omoi sync init <git url>` (or `omoi sync join <code>`) first",
                "para cifrar hace falta la clave del repositorio general de la sincronización: configura antes `omoi sync init <url de git>` (o `omoi sync join <código>`)"
            )
        );
        return sync::leer_claves(&r);
    }
    match sync::repo_de_espacio(db_path, &space).filter(|r| r.clave.exists()) {
        Some(r) => sync::leer_claves(&r),
        None => bail!(tr!(
            "encrypting a copy of the space {space} needs the key of its sync repository: set it up first with `omoi sync init <git url> --space {space}` (or `omoi sync join --space {space} <code>`)",
            "para cifrar una copia del espacio {space} hace falta la clave de su repositorio de sincronización: configúralo antes con `omoi sync init <url de git> --space {space}` (o `omoi sync join --space {space} <código>`)"
        )),
    }
}

/// All of this computer's keys (the general one's and each space's), to read a copy encrypted with
/// any of them.
fn all_keys(db_path: &Path) -> Result<Vec<Cifra>> {
    let v: Vec<Cifra> = sync::repos(db_path).iter().filter(|r| r.clave.exists()).filter_map(|r| sync::leer_claves(r).ok()).collect();
    ensure!(!v.is_empty(), tr!("the copy is encrypted and this computer has no sync key: set up `omoi sync` first", "la copia va cifrada y este equipo no tiene ninguna clave de sincronización: configura antes `omoi sync`"));
    Ok(v)
}

/// Writes the file. Returns how many lines go between the header and the end line.
pub fn exportar(db: &Connection, db_path: &Path, scope: &Ambito, encrypt: bool, out: &mut impl Write) -> Result<Cuenta> {
    let everything = matches!(scope, Ambito::Todos);
    let encryption = match (encrypt, everything) {
        (false, _) => Encryption::None,
        (true, true) => keys_per_space(db, db_path)?,
        (true, false) => Encryption::Single(keys_for(db, db_path, scope)?),
    };
    // The same keys sync exports: those of the `project` table (a path that came from another computer
    // carries the key it came with, not one computed here).
    let machine = crate::reloj::equipo(db)?;
    let header = Linea::Header {
        format: FORMATO.into(),
        version: VERSION,
        schema: crate::db::VERSION_ESQUEMA,
        exported_at: db.query_row(&format!("SELECT {}", memoria::AHORA), [], |r| r.get(0))?,
        machine: machine.clone(),
        encrypted: encrypt,
        person: crate::db::persona_de(db),
    };
    serde_json::to_writer(&mut *out, &header)?;
    out.write_all(b"\n")?;
    let mut hash = Sha256::new();
    let mut count = Cuenta::default();
    // Each line, with the keys of the space `space` (see `Encryption::for_space`).
    let mut write = |l: &Linea, space: Option<&str>, out: &mut dyn Write| -> Result<()> {
        let text = serde_json::to_string(l)?;
        hash.update(text.as_bytes());
        hash.update(b"\n");
        out.write_all(encryption.encrypt(space, &text)?.as_bytes())?;
        out.write_all(b"\n")?;
        Ok(())
    };
    // Without `--all` (a project or a space), of the trash, the forget marks and the verdicts only what
    // belongs to that scope goes: a client's copy carries nothing of another. This scope's trash
    // first: the forget marks and the verdicts depend on it.
    let mut in_trash: HashSet<String> = HashSet::new();
    let mut with_copy: HashSet<String> = HashSet::new();
    {
        let mut q = db.prepare("SELECT id, memory FROM forgotten")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let (id, text): (String, String) = (row.get(0)?, row.get(1)?);
            if everything || trash_in_scope(db, scope, &text)? {
                in_trash.insert(id.clone());
            }
            with_copy.insert(id);
        }
    }
    let scope_space = crate::proyecto::clave_espacio(db, scope.espacio())?;
    // First the forget marks (on import they are applied before the memories), then the memories
    // with their history, the trash and finally the verdicts, which need their memories (live or in
    // the trash).
    {
        let mut q = db.prepare(&format!("SELECT t.id, t.deleted_at, t.hlc, t.space_key, {} FROM tombstone t ORDER BY t.id", sync::EXTRA_OLVIDO))?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let (id, space, extra): (String, String, String) = (row.get(0)?, row.get(3)?, row.get(4)?);
            // Of this scope: what is in its trash or, without a copy in the trash, what is in its space.
            if !everything && !in_trash.contains(&id) && (with_copy.contains(&id) || space != scope_space) {
                continue;
            }
            let l = Linea::olvido(id, row.get(1)?, row.get(2)?, space.clone(), &machine, sync::extra_de(&extra));
            write(&l, Some(&space), out)?;
            count.tombstones += 1;
        }
    }
    // The memories that go (live or in the trash): their verdicts, with them.
    let mut going = in_trash.clone();
    // The history of those that a pre-1.8.0 version promoted from a client with its id was the
    // client's text: it only goes in the copy of everything (as in `memoria::historial_en`).
    let without_history = if everything { Default::default() } else { crate::oficios::historiales_solo_aqui(db)? };
    {
        let mut q = db.prepare(&format!("SELECT {COLUMNAS}, m.hlc, m.content_hlc, m.extra FROM memory m WHERE {} ORDER BY m.id", scope.sql()))?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let m = de_fila(row)?;
            let (hlc, content_hlc, extra): (String, String, String) = (row.get(NCOL)?, row.get(NCOL + 1)?, row.get(NCOL + 2)?);
            let versions = if without_history.contains(&m.id) { Vec::new() } else { memoria::historial(db, &m.id)? };
            going.insert(m.id.clone());
            let space = m.space.clone();
            let l = sync::linea_de(m, hlc, content_hlc, &machine, versions, sync::extra_de(&extra));
            write(&l, Some(&space), out)?;
            count.memories += 1;
        }
    }
    {
        let mut q = db.prepare("SELECT id, memory, versions, hlc, deleted_at, by FROM forgotten ORDER BY id")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            if !in_trash.contains(&id) {
                continue;
            }
            let (memory, versions_json): (String, String) = (row.get(1)?, row.get(2)?);
            let versions = serde_json::from_str(&versions_json).unwrap_or_default();
            let space = if everything { sync::espacio_de_papelera(db, &memory)? } else { String::new() };
            let Some(l) = Linea::papelera(db, id, &memory, versions, row.get(3)?, row.get(4)?, row.get(5)?)? else { continue };
            write(&l, Some(&space), out)?;
            count.forgotten += 1;
        }
    }
    for v in crate::veredictos::todos(db)? {
        // Both memories, of the scope: a verdict isn't taken to a copy that doesn't have the other one.
        if !everything && !(going.contains(&v.memory_id) && (v.other_id.is_empty() || going.contains(&v.other_id))) {
            continue;
        }
        // With the key of its memory's space: the one here, live or in the trash, or that of its forget
        // mark if it was fully deleted. If unknown, it doesn't go encrypted: better that than another
        // client's key or the personal one.
        let space = if everything { verdict_space(db, &v.memory_id)? } else { None };
        if everything && space.is_none() && !matches!(encryption, Encryption::None) {
            continue;
        }
        write(&Linea::veredicto(v, &machine), space.as_deref(), out)?;
        count.verdicts += 1;
    }
    // The end line goes like the other lines (encrypted if they are), but doesn't enter the SHA.
    let end = serde_json::to_string(&Linea::End { count: count.total(), sha256: hex(&hash.finalize()) })?;
    out.write_all(encryption.encrypt(None, &end)?.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(count)
}

/// The space (its key; empty: none) of the verdict of the memory `id`: the memory's, live or in the
/// trash, or that of its forget mark (`tombstone.space_key`) if it was fully deleted. `None` if
/// nothing of it is left here.
fn verdict_space(db: &Connection, id: &str) -> Result<Option<String>> {
    if let Some(e) = sync::espacio_aqui(db, id)? {
        return Ok(Some(e));
    }
    Ok(db.query_row("SELECT space_key FROM tombstone WHERE id = ?1", [id], |r| r.get(0)).optional()?)
}

/// Whether a memory in the trash (`text`, what `forgotten.memory` stores) belongs to this scope: its
/// project by key (or, in those from before schema 10, by path) or its space. One whose project or
/// space no longer exists here belongs to no scope.
fn trash_in_scope(db: &Connection, scope: &Ambito, text: &str) -> Result<bool> {
    let Some((m, _, _)) = sync::de_papelera(text) else { return Ok(false) };
    let project = if !m.project_key.is_empty() {
        db.query_row("SELECT id FROM project WHERE key = ?1", [&m.project_key], |r| r.get::<_, i64>(0)).optional()?
    } else if !m.project.is_empty() {
        crate::proyecto::resolver(db, &m.project)?.map(|(id, _)| id)
    } else {
        Some(0)
    };
    let Some(project) = project else { return Ok(false) };
    let space = match (project, m.space.is_empty()) {
        (0, false) => match db.query_row("SELECT id FROM space WHERE key = ?1", [&m.space], |r| r.get::<_, i64>(0)).optional()? {
            Some(s) => Some(s),
            None => return Ok(false),
        },
        _ => None,
    };
    scope.incluye(db, project, space)
}

/// Whether the file starts with this format's header (anything else is the old export or engram's,
/// which `importar` reads).
pub fn es_propio(path: &Path) -> Result<bool> {
    let f = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut first = String::new();
    BufReader::new(f).read_line(&mut first)?;
    let first = first.trim_start_matches('\u{feff}');
    Ok(matches!(serde_json::from_str::<Linea>(first), Ok(Linea::Header { format, .. }) if format == FORMATO))
}

/// Reads the file line by line: the header, each line decrypted if needed, and checks the end line.
/// `f` gets each line in between already parsed.
fn read(db_path: &Path, path: &Path, mut f: impl FnMut(Linea) -> Result<()>) -> Result<Cuenta> {
    let file = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let first = lines.next().transpose()?.unwrap_or_default();
    let header: Linea = serde_json::from_str(first.trim_start_matches('\u{feff}')).with_context(|| tr!("{} is not an omoi export", "{} no es una exportación de omoi", path.display()))?;
    let Linea::Header { format, version, encrypted, .. } = header else {
        bail!(tr!("{} is not an omoi export", "{} no es una exportación de omoi", path.display()));
    };
    ensure!(format == FORMATO && version <= VERSION, tr!("{} comes from a newer omoi (export format {version}; this one reads up to {VERSION})", "{} viene de un omoi más nuevo (formato {version}, este entiende hasta el {VERSION})", path.display()));
    let keys = if encrypted { all_keys(db_path)? } else { Vec::new() };
    let mut hash = Sha256::new();
    let mut count = Cuenta::default();
    let mut end: Option<(usize, String)> = None;
    for (i, l) in lines.enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let plain = if encrypted {
            // With the first key that opens it; if none does, the first one's error.
            let mut failure = None;
            let plain = keys.iter().find_map(|c| c.descifrar(&l).map_err(|e| failure.get_or_insert(e)).ok());
            match plain {
                Some(t) => t,
                // (A copy of everything carries each space under the key of its repository: normally
                // this computer is missing one of them.)
                None => {
                    return Err(failure.unwrap_or_else(|| anyhow::anyhow!("?"))).with_context(|| {
                        tr!(
                            "line {} doesn't open with any sync key of this computer, so nothing was imported. A copy of everything carries each space under the key of its own sync repository: join the repositories of the spaces it has (`omoi sync join --space <space> <code>`) and import it again",
                            "la línea {} no se abre con ninguna clave de sincronización de este equipo, así que no se ha importado nada. Una copia de todo lleva cada espacio con la clave de su propio repositorio: únete a los repositorios de los espacios que lleva (`omoi sync join --space <espacio> <código>`) e impórtala otra vez",
                            i + 2
                        )
                    });
                }
            }
        } else {
            l
        };
        // A line of a type this version doesn't know (from a newer omoi) counts for the end line and
        // is skipped; any other that can't be parsed is damaged.
        if sync::de_tipo_nuevo(&plain) {
            hash.update(plain.as_bytes());
            hash.update(b"\n");
            count.unknown += 1;
            continue;
        }
        let x: Linea = serde_json::from_str(&plain).with_context(|| tr!("line {} can't be read", "la línea {} no se entiende", i + 2))?;
        if let Linea::End { count, sha256 } = x {
            end = Some((count, sha256));
            break;
        }
        ensure!(x.valida(), tr!("line {} has an invalid id", "la línea {} lleva un id no válido", i + 2));
        hash.update(plain.as_bytes());
        hash.update(b"\n");
        match &x {
            Linea::Memory(_) => count.memories += 1,
            Linea::Tombstone { .. } => count.tombstones += 1,
            Linea::Verdict { .. } => count.verdicts += 1,
            Linea::Forgotten { .. } => count.forgotten += 1,
            Linea::Project { .. } => count.projects += 1,
            Linea::Header { .. } | Linea::End { .. } => {}
        }
        f(x)?;
    }
    let Some((end_count, sha256)) = end else {
        bail!(tr!("{} is incomplete: it has no end line", "{} está incompleto: no tiene la línea de cierre", path.display()));
    };
    ensure!(end_count == count.total() && sha256 == hex(&hash.finalize()), tr!("{} is damaged: its contents don't match its end line", "{} está dañado: su contenido no cuadra con la línea de cierre", path.display()));
    Ok(count)
}

/// What is done with a file's verified stamps (see [`importar`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Sellos {
    /// Whoever imports is in an agent's shell (`verificado::shell_de_agente`): none is valid.
    pub agente: bool,
    /// `--trust-verified`: also those this person didn't set (they are signed as theirs).
    pub confiar: bool,
}

/// What happened to a line's stamp.
#[derive(PartialEq, Eq)]
enum StampOutcome {
    /// It had none, or it stays.
    Kept,
    /// Removed by an agent's shell.
    Agent,
    /// Removed for not being this person's (without `--trust-verified`).
    Foreign,
}

/// A line's verified stamp (`extra.verified`, the memory's or that of the one going to the trash):
/// it stays if this person set it (their signature is valid with their stamp key, see
/// `verificado::Llaves`: their backup, what was exported on another of their computers with their
/// general repository) or with `--trust-verified` (and then it's signed as theirs); otherwise, or in
/// an agent's shell, it is removed.
fn filter_stamp(l: &mut Linea, keys: &crate::verificado::Llaves, s: Sellos) -> StampOutcome {
    let (id, extra) = match l {
        Linea::Memory(c) => (c.id.clone(), &mut c.extra),
        Linea::Forgotten { memory, .. } => (memory.id.clone(), &mut memory.extra),
        _ => return StampOutcome::Kept,
    };
    let Some(v) = extra.get_mut(crate::verificado::CLAVE) else { return StampOutcome::Kept };
    if s.agente {
        extra.remove(crate::verificado::CLAVE);
        return StampOutcome::Agent;
    }
    if crate::verificado::sello_propio(keys, &id, v) {
        return StampOutcome::Kept;
    }
    if s.confiar {
        // (In a dry run the key to sign it with isn't created: it stays as is, and isn't written.)
        crate::verificado::resellar(keys, &id, v);
        return StampOutcome::Kept;
    }
    extra.remove(crate::verificado::CLAVE);
    StampOutcome::Foreign
}

/// Imports a file of this format. First it reads it whole to check it (without applying anything),
/// and only then applies it line by line. With `dry_run`, it only counts.
///
/// Verified-by-a-person stamps, with the same rule as `import --okf`: anyone may have written the
/// file (and the stamp's fingerprint is computed from the text), so only those this person set
/// (signed with their stamp key) stay and, with `--trust-verified`, the rest. From an agent's shell
/// (`stamps.agente`), none: that would be an agent verifying itself. The removed ones are counted in
/// `Cuenta::unverified` (agent) and `Cuenta::untrusted_verified` (someone else's) to warn. (Sync does
/// bring the stamps of the person's other computers, through their encrypted repository: see
/// `sync::sello_de_otro`.)
pub fn importar(db: &mut Connection, db_path: &Path, path: &Path, current: &str, model: Option<&Modelo>, dry_run: bool, stamps: Sellos) -> Result<(Cuenta, Resumen)> {
    let keys = crate::verificado::llaves(db, stamps.confiar && !stamps.agente && !dry_run)?;
    let (mut from_agent, mut foreign) = (0, 0);
    let mut count = read(db_path, path, |mut l| {
        match filter_stamp(&mut l, &keys, stamps) {
            StampOutcome::Agent => from_agent += 1,
            StampOutcome::Foreign => foreign += 1,
            StampOutcome::Kept => {}
        }
        Ok(())
    })?;
    count.unverified = from_agent;
    count.untrusted_verified = foreign;
    let mut r = Resumen::default();
    if dry_run {
        return Ok((count, r));
    }
    // In a freshly created database (no memories, no trash and no sync), the copy's person: it's the
    // same person restoring their database, and their projects without a remote go back to their folders.
    if let Some(person) = copy_person(path)?
        && sync::repos(db_path).is_empty()
        && db.query_row("SELECT NOT EXISTS(SELECT 1 FROM memory) AND NOT EXISTS(SELECT 1 FROM forgotten)", [], |r| r.get::<_, bool>(0))?
    {
        sync::adoptar_persona(db, db_path, &person)?;
    }
    let mut applier = Aplicador::nuevo(db, None, Some(current))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    read(db_path, path, |mut l| {
        filter_stamp(&mut l, &keys, stamps);
        let origin = match l.machine() {
            "" => format!("import:{name}"),
            m => m.to_string(),
        };
        if applier.aplicar(db, &origin, l, model, &mut r).is_err() {
            r.ilegibles += 1;
        }
        Ok(())
    })?;
    // The same key written on another computer without seeing this one: one wins (`memoria::resolver_claves_repetidas`).
    crate::memoria::resolver_claves_repetidas(db)?;
    crate::revisar::al_dia(db)?;
    Ok((count, r))
}

/// The person stated in the header of an `omoi export` file (older ones don't carry it).
fn copy_person(path: &Path) -> Result<Option<String>> {
    let f = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut first = String::new();
    BufReader::new(f).read_line(&mut first)?;
    Ok(match serde_json::from_str::<Linea>(first.trim_start_matches('\u{feff}')) {
        Ok(Linea::Header { person, .. }) if !person.is_empty() => Some(person),
        _ => None,
    })
}

// --- Daily copy ------------------------------------------------------------------------------------

/// How many copies are kept (`OMOIKANE_BACKUPS`; 0 = none are made).
pub fn copias_a_conservar() -> usize {
    std::env::var("OMOIKANE_BACKUPS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(7)
}

pub fn carpeta_copias(db_path: &Path) -> PathBuf {
    db_path.parent().map(Path::to_path_buf).unwrap_or_default().join("backups")
}

/// The copies there are, newest to oldest: (path, bytes).
pub fn copias(db_path: &Path) -> Vec<(PathBuf, u64)> {
    let mut v: Vec<(PathBuf, u64)> = std::fs::read_dir(carpeta_copias(db_path))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("omoikane-") && e.path().extension().is_some_and(|x| x == "db"))
                .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

/// Makes today's copy (replaces today's if there was one already) and deletes the extra ones. Returns its path.
pub fn copiar(db: &Connection, db_path: &Path) -> Result<PathBuf> {
    let folder = carpeta_copias(db_path);
    std::fs::create_dir_all(&folder)?;
    let today: String = db.query_row("SELECT strftime('%Y-%m-%d', 'now')", [], |r| r.get(0))?;
    let dest = folder.join(format!("omoikane-{today}.db"));
    // `VACUUM INTO` doesn't overwrite: it's written aside and renamed at the end.
    let tmp = folder.join(format!(".omoikane-{today}.db.tmp"));
    let _ = std::fs::remove_file(&tmp);
    db.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])?;
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&tmp, &dest)?;
    for (path, _) in copias(db_path).into_iter().skip(copias_a_conservar().max(1)) {
        let _ = std::fs::remove_file(path);
    }
    Ok(dest)
}

/// When the first session of the day starts: the copy and the trash purge. Never fails nor repeats
/// on the same day (it records it in `meta`).
pub fn diaria(db: &Connection, db_path: &Path) -> Option<PathBuf> {
    let today: String = db.query_row("SELECT strftime('%Y-%m-%d', 'now')", [], |r| r.get(0)).ok()?;
    let last: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'copia_dia'", [], |r| r.get(0)).optional().ok()?;
    if last.as_deref() == Some(today.as_str()) {
        return None;
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('copia_dia', ?1)", [&today]).ok()?;
    let _ = memoria::purgar(db, memoria::dias_papelera());
    if copias_a_conservar() == 0 {
        return None;
    }
    copiar(db, db_path).ok()
}
