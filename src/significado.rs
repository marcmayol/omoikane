//! The service that keeps EmbeddingGemma (`gemma`) loaded for whoever needs a vector.
//!
//! Loading the model takes about 2 s and recall starts a process on every message, so the model
//! lives in a separate process, one per user: `omoi model serve-semantic` (hidden). It listens only on
//! 127.0.0.1, on a free port, and requires a token; both, with its pid, go in
//! `~/.omoikane/significado.json` once the model is loaded. It shuts itself down after a while without
//! requests (`OMOIKANE_SEMANTIC_IDLE` seconds, 30 min by default).
//!
//! The caller (`vectores`) has a time budget: if there is no service, or it takes longer, it carries on
//! without meaning, as before, and leaves the service starting for next time. It never waits for
//! it to load.
//!
//! Protocol: one JSON line per request and another per response.
//! `{"token":…,"tipo":"pregunta"|"documento","textos":[…]}` → `{"vectores":[[…]…]}` or `{"error":…}`;
//! `{"token":…,"ping":true}` → `{"pid":…,"modelo":…}`.

use crate::gemma::{Gemma, Tipo};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REGISTRY_FILE: &str = "significado.json";
const STARTING_FILE: &str = "significado.arrancando";
/// Model folder inside `~/.omoikane/models`.
pub const NOMBRE_MODELO: &str = "embeddinggemma-300m";
const DEFAULT_IDLE_SECS: u64 = 30 * 60;
/// After launching the service, it is not launched again until this has passed (more than its load time).
const START_RETRY_SECS: u64 = 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registro {
    pub puerto: u16,
    pub token: String,
    pub pid: u32,
    pub modelo: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Request {
    token: String,
    #[serde(default)]
    tipo: Option<String>,
    #[serde(default)]
    textos: Vec<String>,
    #[serde(default)]
    ping: bool,
    /// The caller's database: the service computes its missing vectors in the background.
    #[serde(default)]
    db: Option<String>,
    /// Search in the service (`parecidas`) instead of asking for vectors.
    #[serde(default)]
    buscar: Option<SearchRequest>,
}

fn home() -> Option<PathBuf> {
    crate::entorno::home()
}

const PREFERENCE_FILE: &str = "semantic";

/// What the user chose with `omoi model semantic --on/--off` (`None`: chose nothing).
pub fn preferencia() -> Option<bool> {
    match std::fs::read_to_string(home()?.join(PREFERENCE_FILE)).ok()?.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Saves the choice: `Some(true)` (`--on`), `Some(false)` (`--off`) or `None` (`--auto`: memory size
/// decides). Turning it off also closes the service if it is running: the watchdog sees that the
/// registry is no longer its own and exits. With it off everything works as before 1.2: only words and potion.
pub fn fijar(choice: Option<bool>) -> Result<()> {
    let c = home().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    match choice {
        Some(on) => std::fs::write(c.join(PREFERENCE_FILE), if on { "on\n" } else { "off\n" })?,
        None => {
            let _ = std::fs::remove_file(c.join(PREFERENCE_FILE));
        }
    }
    if choice == Some(false) {
        let _ = std::fs::remove_file(c.join(REGISTRY_FILE));
    }
    Ok(())
}

/// From how many memories the meaning model turns itself on, if nobody chose anything.
///
/// With few memories it does not pay off: the recall veto barely removes noise below about 1,000
/// (`omoi-revision-12/2-cuando.md`: with 150-1,000 the noise is the same with and without the model; with 3,000
/// it drops from 40 % to 7.5 %) and it costs ~450 MB of disk, a resident service and indexing every memory.
/// 1,000, calibrated on 2-Nov-2026: with 1,000 memories the model already pays (search: paraphrase on the
/// first try 0.08 → 0.36; recall: noise on questions with no answer 30 % → 22.5 %; `2-cuando.md`), and since
/// the in-house engine (`engine`) it costs little more than the disk: loads in ~26 ms, 13 MB of its own RAM,
/// indexing ~76-640 ms per memory, once. Below 1,000 search would also gain paraphrases,
/// but recall would not, and the download does not pay off. `OMOIKANE_SEMANTIC_THRESHOLD` changes it.
///
/// All memories in the database count, not those in the session's scope. The model, its download, the
/// service and the vectors belong to the whole database (not to a project), and the decision has to be the
/// same from any folder: if it depended on the scope, it would turn on and off when switching
/// projects, and the database would be half indexed. Also, from a general folder recall and search
/// see all of them, and in a large database word noise also rises inside each project (global
/// memories drag it in). It costs one read of `memory_cuenta` (microseconds).
pub const UMBRAL_MEMORIAS: usize = 1000;

pub fn umbral() -> usize {
    std::env::var("OMOIKANE_SEMANTIC_THRESHOLD").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(UMBRAL_MEMORIAS)
}

/// Why the model is on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Porque {
    /// `OMOIKANE_SEMANTIC=0`/`1`.
    Env,
    /// `omoi model semantic --on/--off`.
    Chosen,
    /// Nobody chose anything: memory size decides (`UMBRAL_MEMORIAS`).
    Size,
}

/// What state the meaning model is in for a database, and why.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Estado {
    pub on: bool,
    pub reason: Porque,
    pub memories: usize,
    pub threshold: usize,
}

/// Has someone forbidden it? `OMOIKANE_SEMANTIC=0` or `--off`. It does not say whether it is on (that also
/// depends on the size, see `estado`): the service and whoever starts it only look at this.
pub fn permitido() -> bool {
    match std::env::var("OMOIKANE_SEMANTIC") {
        Ok(v) if !v.is_empty() => v != "0",
        _ => preferencia() != Some(false),
    }
}

/// `OMOIKANE_SEMANTIC` rules; otherwise, what the user chose; and if nothing was chosen, the size: from
/// `umbral()` memories in the database on, it is on.
pub fn estado(db: &rusqlite::Connection) -> Estado {
    let memories = crate::memoria::count(db, &crate::memoria::Scope::All).unwrap_or(0).max(0) as usize;
    let threshold = umbral();
    let (on, reason) = match std::env::var("OMOIKANE_SEMANTIC") {
        Ok(v) if !v.is_empty() => (v != "0", Porque::Env),
        _ => match preferencia() {
            Some(p) => (p, Porque::Chosen),
            None => (memories >= threshold, Porque::Size),
        },
    };
    Estado { on, reason, memories, threshold }
}

/// Is the model used with this database? On (`estado`) and installed.
pub fn en_uso(db: &rusqlite::Connection) -> bool {
    permitido() && modelo().is_some() && estado(db).on
}

/// `en_uso` with the database path (opens a connection only if the model is installed and allowed).
pub fn en_uso_en(path: &std::path::Path) -> bool {
    if !permitido() || modelo().is_none() || !path.exists() {
        return false;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    rusqlite::Connection::open_with_flags(path, flags).is_ok_and(|c| estado(&c).on)
}

const DOWNLOADING_FILE: &str = "significado.descargando";
const NOTICE_SENT_FILE: &str = "significado.avisado";
/// After launching a download, no other is launched until this has passed (if it failed, it is retried then).
const DOWNLOAD_RETRY_SECS: u64 = 3600;

/// Is a model download in progress (launched less than `DOWNLOAD_RETRY_SECS` ago)?
pub fn descargando() -> bool {
    home()
        .and_then(|c| std::fs::read_to_string(c.join(DOWNLOADING_FILE)).ok())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .is_some_and(|t| now_secs().saturating_sub(t) < DOWNLOAD_RETRY_SECS)
}

/// Launches `omoi model install --semantic` separately for `db` (which, when done, leaves the service indexing
/// that database), unless a download is already in progress. Does not wait.
pub fn descargar_aparte(db: &std::path::Path) {
    // With nobody at the keyboard (or `OMOIKANE_NO_MODEL=1`), no 450 MB nobody asked for: the model is
    // installed by hand (`omoi model install --semantic`) and, meanwhile, everything goes by words.
    if !permitido() || modelo().is_some() || descargando() || crate::entorno::no_downloads() {
        return;
    }
    let Some(c) = home() else { return };
    let _ = std::fs::create_dir_all(&c);
    if std::fs::write(c.join(DOWNLOADING_FILE), now_secs().to_string()).is_ok() {
        let db = db.to_string_lossy();
        crate::sync::spawn_detached(&["--db", &db, "model", "install", "--semantic"]);
    }
}

/// The download finished (successfully): removes the marker.
pub fn descarga_hecha() {
    if let Some(c) = home() {
        let _ = std::fs::remove_file(c.join(DOWNLOADING_FILE));
    }
}

/// At session start: if the model is on for this database and missing, downloads it in the
/// background; if it is there and the database does not have its vectors yet, leaves the service indexing it. Returns the notice
/// for the agent the first time it turns itself on because of size (once per user).
pub fn al_empezar(db: &rusqlite::Connection) -> Option<String> {
    if !permitido() {
        return None;
    }
    let e = estado(db);
    let path = db.path().filter(|p| !p.is_empty()).map(PathBuf::from)?;
    if !e.on {
        return None;
    }
    if modelo().is_none() {
        if crate::entorno::no_downloads() {
            return None;
        }
        descargar_aparte(&path);
        let c = home()?;
        if e.reason == Porque::Size && !c.join(NOTICE_SENT_FILE).exists() && std::fs::write(c.join(NOTICE_SENT_FILE), now_secs().to_string()).is_ok() {
            return Some(format!(
                "Omoikane now holds {} memories (from {} on, words alone bring more noise): it is downloading its meaning model (EmbeddingGemma, about 450 MB) in the background and will then index the memories, so searches find what is asked with other words and recall injects less noise. Until it finishes everything works as before. `omoi model semantic --off` stops it for good. Tell the user once.",
                e.memories, e.threshold
            ));
        }
        return None;
    }
    if !listo(db) {
        avisar(&path);
    }
    None
}

/// The EmbeddingGemma folder if installed: `OMOIKANE_SEMANTIC_MODEL`, or
/// `~/.omoikane/models/embeddinggemma-300m`.
pub fn modelo() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOIKANE_SEMANTIC_MODEL")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::modelos::carpeta().join(NOMBRE_MODELO));
    crate::gemma::es_modelo(&dir).then_some(dir)
}

/// Where the compact format (`gemma::compactar`) is downloaded from, in order. `OMOIKANE_SEMANTIC_SOURCE`
/// replaces them (one or several, comma-separated).
pub const ORIGENES: [&str; 2] = [
    "https://huggingface.co/natzx94/embeddinggemma-300m-omoikane/resolve/main",
    "https://github.com/marcmayol/omoikane/releases/download/modelo-embeddinggemma-300m-v1",
];

/// SHA-256 of each file of the published compact format: what is downloaded must be
/// exactly this, whichever mirror it comes from.
const SUMAS: [(&str, &str); 5] = [
    ("config.json", "8f863f76e2d9c710cc833dc92efa898c9adfd41031c786507cc6b0e49c2e3e68"),
    ("tokenizer.json", "6852f8d561078cc0cebe70ca03c5bfdd0d60a45f9d2e0e1e4cc05b68e9ec329e"),
    ("tokens.escala", "ffba8e813e1ce38f96a360df3f70f7c2dd14b30c462c5a869da5def52abd5e1a"),
    ("pesos.safetensors", "29c6f8b54070cc8889f1f0f335dbf9d2e5a6957f2c01830ab9e5284f832b7aec"),
    ("tokens.i8", "02c0fd1158bf4195f15f5db815f67f1de120e0852d010a0f2a74917167fc2465"),
];

/// The notice the Gemma license requires from whoever redistributes it; it goes in the model folder.
const LICENSE_NOTICE: &str = "EmbeddingGemma 300M (google/embeddinggemma-300m), converted by Omoikane to a compact format\n\
(token table in int8, the rest in float16).\n\n\
Gemma is provided under and subject to the Gemma Terms of Use found at ai.google.dev/gemma/terms\n\
Its use is restricted by the Gemma Prohibited Use Policy: ai.google.dev/gemma/prohibited_use_policy\n";

/// Installs EmbeddingGemma in `~/.omoikane/models/embeddinggemma-300m`. Without `source`, downloads the
/// compact format (about 450 MB) and checks its SHA-256; with `source`, takes it from a folder, in
/// compact format or sentence-transformers (which it converts). If already installed, does nothing.
/// Returns the folder.
pub fn instalar(source: Option<&std::path::Path>, notify: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    use crate::gemma;
    use crate::tr;
    let base = crate::modelos::carpeta();
    let dest = base.join(NOMBRE_MODELO);
    if gemma::es_modelo(&dest) {
        return Ok(dest);
    }
    std::fs::create_dir_all(&base)?;
    let tmp = base.join(format!(".instalando-significado-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = (|| -> Result<()> {
        match source {
            Some(d) if gemma::es_compacto(d) => {
                std::fs::create_dir_all(&tmp)?;
                for f in gemma::FICHEROS_COMPACTO {
                    std::fs::copy(d.join(f), tmp.join(f))?;
                }
            }
            Some(d) if gemma::es_sentence_transformers(d) => {
                notify(&tr!("converting the model to the compact format (a few seconds)", "convirtiendo el modelo al formato compacto (unos segundos)"));
                gemma::compactar(d, &tmp)?;
            }
            Some(d) => bail!(tr!("{}: no EmbeddingGemma there", "{}: ahí no hay EmbeddingGemma", d.display())),
            None => {
                let sources: Vec<String> = match std::env::var("OMOIKANE_SEMANTIC_SOURCE") {
                    Ok(o) if !o.trim().is_empty() => o.split(',').map(|x| x.trim().trim_end_matches('/').to_string()).filter(|x| !x.is_empty()).collect(),
                    _ => ORIGENES.iter().map(|o| o.to_string()).collect(),
                };
                let mut failures = Vec::new();
                for o in sources {
                    let r = crate::modelos::descargar_ficheros(&o, &gemma::FICHEROS_COMPACTO, &tmp, notify)
                        .and_then(|()| crate::modelos::comprobar_sumas(&tmp, &SUMAS));
                    match r {
                        Ok(()) => {
                            failures.clear();
                            break;
                        }
                        Err(e) => {
                            notify(&tr!("failed: {e:#}", "falló: {e:#}"));
                            failures.push(format!("{o}: {e:#}"));
                        }
                    }
                }
                if !failures.is_empty() {
                    bail!(tr!("couldn't download the meaning model:\n{}", "no se pudo descargar el modelo de significado:\n{}", failures.join("\n")));
                }
            }
        }
        std::fs::write(tmp.join("NOTICE"), LICENSE_NOTICE)?;
        // The compiled tokenizer (`bpe`), here and not in the download: the published files and their
        // SHA-256 do not change. If it failed, it is compiled the first time the model is loaded.
        let _ = crate::bpe::compile(&tmp.join("tokenizer.json"), &tmp.join(crate::bpe::FILE));
        // If another process installed it at the same time, theirs is fine.
        if std::fs::rename(&tmp, &dest).is_err() && !gemma::es_modelo(&dest) {
            bail!(tr!("can't move the model into {}", "no se puede mover el modelo a {}", dest.display()));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result.map(|()| dest)
}

// ------------------------------------------------------------------ vectors in the database

/// The EmbeddingGemma vectors of each memory: a local table, like `vec_bloque` (not
/// synced, a 1.0 ignores it and it does not raise `lector_minimo`). By `id` and not by `n`: a migration that
/// rebuilds `memory` may renumber. `hlc` is the memory's `content_hlc` when it was computed: if
/// the text changes, it stops matching and is recomputed. `v`: scale (f32) and 768 int8.
const TABLE: &str = "CREATE TABLE IF NOT EXISTS memory_sig (id TEXT PRIMARY KEY, hlc TEXT NOT NULL, v BLOB NOT NULL)";

/// Which model and which computation made the stored vectors: if it changes, they are deleted and recomputed.
const VECTORS_VERSION: &str = "embeddinggemma-300m/2048";

pub fn crear_tabla(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(TABLE)?;
    let done: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'sig_version'", [], |f| f.get(0)).ok();
    if done.as_deref() != Some(VECTORS_VERSION) {
        db.execute("DELETE FROM memory_sig", [])?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sig_version', ?1)", [VECTORS_VERSION])?;
    }
    Ok(())
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let e = if max > 0.0 { max / 127.0 } else { 1.0 };
    let mut b = Vec::with_capacity(4 + v.len());
    b.extend_from_slice(&e.to_le_bytes());
    b.extend(v.iter().map(|x| (x / e).round().clamp(-127.0, 127.0) as i8 as u8));
    b
}

/// Cosine between the query (normalized) and a stored vector (normalized when stored).
fn cosine(q: &[f32], b: &[u8]) -> f32 {
    if b.len() != 4 + q.len() {
        return 0.0;
    }
    let e = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    // 8 at a time with partial sums: that way the compiler vectorizes it (with a single sum it cannot
    // reorder it). With 10,000 memories, comparing went from 8.2 to 2.0 ms (speed review).
    let mut s = [0f32; 8];
    let ((cq, rq), (cb, rb)) = (q.as_chunks::<8>(), b[4..].as_chunks::<8>());
    for (x, y) in cq.iter().zip(cb) {
        for i in 0..8 {
            s[i] += x[i] * f32::from(y[i] as i8);
        }
    }
    let rest: f32 = rq.iter().zip(rb).map(|(x, &y)| x * f32::from(y as i8)).sum();
    e * (s.iter().sum::<f32>() + rest)
}

/// The text that becomes a vector: title and content, as in the benchmark that measured it.
pub fn texto_de(title: &str, content: &str) -> String {
    format!("{title}\n{content}")
}

/// Up to `max` memories without a vector (or whose text changed since it was computed): `(id, hlc, text)`.
pub fn pendientes(db: &rusqlite::Connection, max: usize) -> Result<Vec<(String, String, String)>> {
    let mut q = db.prepare(
        "SELECT m.id, m.content_hlc, m.title, m.content FROM memory m LEFT JOIN memory_sig s ON s.id = m.id
         WHERE s.id IS NULL OR s.hlc <> m.content_hlc LIMIT ?1",
    )?;
    let rows = q.query_map([max as i64], |f| Ok((f.get(0)?, f.get(1)?, texto_de(&f.get::<_, String>(2)?, &f.get::<_, String>(3)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn guardar(db: &rusqlite::Connection, id: &str, hlc: &str, v: &[f32]) -> Result<()> {
    db.execute("INSERT OR REPLACE INTO memory_sig(id, hlc, v) VALUES (?1, ?2, ?3)", rusqlite::params![id, hlc, to_blob(v)])?;
    Ok(())
}

/// How many memories have an up-to-date vector, and how many there are. Along the way it deletes those of memories that
/// no longer exist (forgotten or purged).
pub fn cobertura(db: &rusqlite::Connection) -> Result<(usize, usize)> {
    let _ = db.execute("DELETE FROM memory_sig WHERE id NOT IN (SELECT id FROM memory)", []);
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0))?;
    let done: i64 = db.query_row("SELECT count(*) FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc", [], |f| f.get(0))?;
    Ok((done as usize, total as usize))
}

/// Are there vectors for almost everything? While indexing (the first minutes after installing the model),
/// mixing meaning with memories without a vector would sink them: it carries on as before.
pub fn listo(db: &rusqlite::Connection) -> bool {
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0)).unwrap_or(0);
    if total == 0 {
        return false;
    }
    let done: i64 = db.query_row("SELECT count(*) FROM memory_sig", [], |f| f.get(0)).unwrap_or(0);
    done as f64 >= 0.95 * total as f64
}

/// The cosine of `query` with each memory in the scope that has a vector, highest first.
pub fn cosenos(db: &rusqlite::Connection, query: &[f32], scope: &crate::memoria::Scope, kind: Option<crate::memoria::Kind>) -> Result<Vec<(String, f32)>> {
    let mut q = db.prepare(&format!(
        "SELECT s.id, s.v FROM memory_sig s JOIN memory m ON m.id = s.id WHERE {} AND (?1 IS NULL OR m.kind = ?1)",
        scope.sql()
    ))?;
    let mut rows = q.query(rusqlite::params![kind.map(crate::memoria::Kind::as_str)])?;
    let mut v = Vec::new();
    while let Some(f) = rows.next()? {
        let c = cosine(query, f.get_ref(1)?.as_blob()?);
        v.push((f.get::<_, String>(0)?, c));
    }
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(v)
}

// ------------------------------------------------------------------ RAM cap while indexing

/// RAM taken by each token of a batch while it is computed (the intermediate vectors of the
/// layers; measured: 35-41 KB, see the review's `fix-motor.md`). The weights do not count: they are mapped from
/// disk and are the same whether it indexes or not.
const KB_PER_TOKEN: u64 = 40;
/// Default cap on indexing RAM: batches of about 3,300 tokens (25-30 short notes, or 2-5
/// long memories). More is no faster (measured with batches of 1,024 to 4,096 tokens).
pub const RAM_POR_DEFECTO_MB: u64 = 128;
/// The minimum: what the longest memory (2,048 tokens) needs to be computed alone. With less, some
/// could not be indexed.
pub const RAM_MINIMA_MB: u64 = (crate::gemma::MAX_TOKENS as u64 * KB_PER_TOKEN).div_ceil(1024);
const RAM_FILE: &str = "semantic-ram";

/// Where the RAM cap in use comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum OrigenRam {
    /// `OMOIKANE_SEMANTIC_RAM`.
    #[serde(rename = "env")]
    Entorno,
    /// `omoi model semantic --ram`.
    #[serde(rename = "chosen")]
    Elegido,
    #[serde(rename = "default")]
    PorDefecto,
}

/// The indexing RAM cap in MB and where it comes from: `OMOIKANE_SEMANTIC_RAM` rules; otherwise, what
/// was chosen with `omoi model semantic --ram`; otherwise, `RAM_POR_DEFECTO_MB`. Never below
/// `RAM_MINIMA_MB`.
pub fn tope_ram() -> (u64, OrigenRam) {
    let parse = |s: &str| s.trim().parse::<u64>().ok().filter(|&n| n > 0);
    let (mb, origin) = if let Some(n) = std::env::var("OMOIKANE_SEMANTIC_RAM").ok().as_deref().and_then(parse) {
        (n, OrigenRam::Entorno)
    } else if let Some(n) = home().and_then(|c| std::fs::read_to_string(c.join(RAM_FILE)).ok()).as_deref().and_then(parse) {
        (n, OrigenRam::Elegido)
    } else {
        (RAM_POR_DEFECTO_MB, OrigenRam::PorDefecto)
    };
    (mb.max(RAM_MINIMA_MB), origin)
}

/// Saves the cap (`None`: back to the default). Indexing reads it before each round: no
/// need to restart the service. Returns the one left in use (raised to the minimum if needed).
pub fn fijar_ram(mb: Option<u64>) -> Result<u64> {
    let c = home().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    match mb {
        Some(mb) => std::fs::write(c.join(RAM_FILE), format!("{}\n", mb.max(RAM_MINIMA_MB)))?,
        None => {
            let _ = std::fs::remove_file(c.join(RAM_FILE));
        }
    }
    Ok(tope_ram().0)
}

/// Tokens per batch when indexing with the RAM cap in use. Several memories in a single pass of the
/// model: each multiplication is done at once for all of them (faster than one by one, and the same
/// vector). The cap bounds the RAM and how long a query arriving mid-batch waits (at
/// most one step of the batch: see `yield_to_queries`). A memory longer than the batch goes alone.
pub fn tokens_por_lote() -> usize {
    (tope_ram().0 * 1024 / KB_PER_TOKEN) as usize
}

/// Queries the service is serving right now: while there is any, indexing pauses
/// between steps (`yield_to_queries`) so as not to take the CPU from them.
static ACTIVE_QUERIES: AtomicU64 = AtomicU64::new(0);

/// What the model calls between steps while indexing: waits until there are no queries.
fn yield_to_queries() {
    while ACTIVE_QUERIES.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Computes the vectors missing in `db`, reading `batch` memories at a time and in batches of up to
/// `tokens_por_lote` tokens, until done or until `keep_going` says no. `progress` receives how many
/// so far. Returns how many it computed.
pub fn indexar(db: &rusqlite::Connection, gemma: &Gemma, batch: usize, keep_going: &mut dyn FnMut() -> bool, progress: &mut dyn FnMut(usize)) -> Result<usize> {
    let mut done = 0;
    loop {
        // Each round checks the cap again: `omoi model semantic --ram` applies without restarting.
        let cap = tokens_por_lote();
        let p = pendientes(db, batch)?;
        if p.is_empty() {
            return Ok(done);
        }
        let ids = p.iter().map(|(_, _, t)| gemma.ids(t, Tipo::Documento)).collect::<Result<Vec<_>>>()?;
        let mut i = 0;
        while i < p.len() {
            if !keep_going() {
                return Ok(done);
            }
            let (mut j, mut sum) = (i, 0);
            while j < p.len() && (j == i || sum + ids[j].len() <= cap) {
                sum += ids[j].len();
                j += 1;
            }
            for ((id, hlc, _), v) in p[i..j].iter().zip(gemma.vectores_de_ids(&ids[i..j], &yield_to_queries)?) {
                guardar(db, id, hlc, &v)?;
                done += 1;
                progress(done);
            }
            i = j;
        }
    }
}

// ------------------------------------------------------------------ searching inside the service

/// Memories with their cosine, highest first.
pub type Cosenos = Vec<(String, f32)>;

/// What the service keeps in memory for a database to compare without reading it on every message: each
/// recall is a new process, and reading the vectors of all memories (77 MB with 100,000) cost it
/// more than the model itself.
struct Index {
    fingerprint: (i64, i64, String, String, i64),
    rows: Vec<Row>,
    /// The space of each project (for `Scope::Space`).
    space_of: std::collections::HashMap<i64, Option<i64>>,
    memories: i64,
}

struct Row {
    id: String,
    project_id: i64,
    space_id: Option<i64>,
    kind: String,
    v: Vec<u8>,
}

/// Changes if any vector, any memory (text, scope or kind) or any project changes.
fn fingerprint(db: &rusqlite::Connection) -> Result<(i64, i64, String, String, i64)> {
    Ok(db.query_row(
        "SELECT (SELECT count(*) FROM memory_sig), (SELECT COALESCE(max(rowid), 0) FROM memory_sig),
                (SELECT COALESCE(max(hlc), '') FROM memory), (SELECT COALESCE(max(hlc), '') FROM project),
                (SELECT count(*) FROM memory)",
        [],
        |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)),
    )?)
}

impl Index {
    fn load(db: &rusqlite::Connection) -> Result<Index> {
        let fingerprint = fingerprint(db)?;
        let mut q = db.prepare(
            "SELECT s.id, m.project_id, m.space_id, m.kind, s.v FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc",
        )?;
        let rows = q
            .query_map([], |f| Ok(Row { id: f.get(0)?, project_id: f.get(1)?, space_id: f.get(2)?, kind: f.get(3)?, v: f.get(4)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut p = db.prepare("SELECT id, space_id FROM project")?;
        let space_of = p.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(Index { memories: fingerprint.4, fingerprint, rows, space_of })
    }

    /// `Scope::sql`, in memory (a test checks that both give the same).
    fn sees(&self, a: &crate::memoria::Scope, f: &Row) -> bool {
        use crate::memoria::Scope;
        let space = |s: Option<i64>| f.project_id != 0 || f.space_id.is_none() || (s.is_some() && f.space_id == s);
        match *a {
            Scope::Project { id, space: sp } => (f.project_id == id || f.project_id == 0) && space(sp),
            Scope::Space(Some(s)) => (f.project_id == 0 || self.space_of.get(&f.project_id) == Some(&Some(s))) && space(Some(s)),
            Scope::Space(None) => self.space_of.get(&f.project_id) == Some(&None) && space(None),
            Scope::All => true,
        }
    }

    /// The `k` closest to `q` in the scope (highest first) and the cosine of `requested`.
    fn search(&self, q: &[f32], a: &crate::memoria::Scope, kind: Option<&str>, k: usize, requested: &[String]) -> (Cosenos, Cosenos) {
        let mut all: Vec<(usize, f32)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, f)| kind.is_none_or(|k| f.kind == k) && self.sees(a, f))
            .map(|(i, f)| (i, cosine(q, &f.v)))
            .collect();
        let wanted: std::collections::HashSet<&str> = requested.iter().map(|s| s.as_str()).collect();
        let of_requested = all.iter().filter(|(i, _)| wanted.contains(self.rows[*i].id.as_str())).map(|(i, c)| (self.rows[*i].id.clone(), *c)).collect();
        let k = k.min(all.len());
        if k > 0 {
            all.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
            all.truncate(k);
        }
        all.sort_by(|a, b| b.1.total_cmp(&a.1));
        (all.into_iter().map(|(i, c)| (self.rows[i].id.clone(), c)).collect(), of_requested)
    }

    fn ready(&self) -> bool {
        self.memories > 0 && self.rows.len() as f64 >= 0.95 * self.memories as f64
    }
}

/// What the service would answer for vector `q`, without the service: loads the index of `db` and searches
/// (for the tests, which compare with `cosenos`, which filters the scope in SQL).
pub fn parecidas_en_base(db: &rusqlite::Connection, q: &[f32], scope: &crate::memoria::Scope, kind: Option<crate::memoria::Kind>, k: usize) -> Result<Vec<(String, f32)>> {
    let i = Index::load(db)?;
    Ok(i.search(q, scope, kind.map(|k| k.as_str()), k, &[]).0)
}

/// The service's indexes, per database, with their connection.
fn indexes() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, (rusqlite::Connection, Index)>> {
    static I: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, (rusqlite::Connection, Index)>>> = std::sync::OnceLock::new();
    I.get_or_init(Default::default)
}

/// What the service answers to a search: if the database has vectors for almost everything, the `k`
/// closest and the cosine of the requested memories.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Parecidas {
    pub listo: bool,
    #[serde(default)]
    pub top: Vec<(String, f32)>,
    #[serde(default)]
    pub pedidas: Vec<(String, f32)>,
}

fn search_in_service(gemma: &Gemma, b: &SearchRequest) -> Result<Parecidas> {
    let path = PathBuf::from(&b.db);
    // The query vector first, without the lock: it is what takes longest.
    let q = gemma.vector(&b.texto, Tipo::Pregunta)?;
    let mut ix = indexes().lock().map_err(|_| anyhow!("index lock"))?;
    if !ix.contains_key(&path) {
        let c = crate::db::open(&path)?;
        let i = Index::load(&c)?;
        ix.insert(path.clone(), (c, i));
    }
    let (c, i) = ix.get_mut(&path).expect("recién puesto");
    if fingerprint(c)? != i.fingerprint {
        *i = Index::load(c)?;
    }
    if !i.ready() {
        return Ok(Parecidas::default());
    }
    let (top, pedidas) = i.search(&q, &b.ambito, b.kind.as_deref(), b.k, &b.ids);
    Ok(Parecidas { listo: true, top, pedidas })
}

#[derive(Serialize, Deserialize, Debug)]
struct SearchRequest {
    db: String,
    texto: String,
    ambito: crate::memoria::Scope,
    #[serde(default)]
    kind: Option<String>,
    k: usize,
    #[serde(default)]
    ids: Vec<String>,
}

/// Why the service did not give the closest memories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallo {
    /// Off (`--off`, `OMOIKANE_SEMANTIC=0`) or not installed.
    Apagado,
    /// No service (or not listening): it is left starting for next time.
    SinServicio,
    /// It did not answer within the budget (busy CPU, long text).
    Tarde,
    /// The database does not have vectors for almost everything yet (indexing).
    Indexando,
    /// Any other error (broken answer, token…).
    Error,
}

impl Fallo {
    pub fn como_texto(self) -> &'static str {
        match self {
            Fallo::Apagado => "off",
            Fallo::SinServicio => "no_service",
            Fallo::Tarde => "timeout",
            Fallo::Indexando => "indexing",
            Fallo::Error => "error",
        }
    }
}

/// The memories closest to `text` in `scope`, computed by the service (which has the model
/// and the vectors in memory), and the cosine of `ids`. `None` if the model is off or not
/// installed, the service is not there or does not answer within `budget`, or the database does not have vectors
/// for almost everything yet: then it carries on by words. The caller decides beforehand whether the model is
/// on for this database (`en_uso`): here it only checks it is not forbidden.
pub fn parecidas(
    db: &std::path::Path,
    text: &str,
    scope: &crate::memoria::Scope,
    kind: Option<crate::memoria::Kind>,
    k: usize,
    ids: &[String],
    budget: Duration,
) -> Option<Parecidas> {
    parecidas_o_fallo(db, text, scope, kind, k, ids, budget).ok()
}

/// `parecidas`, saying why they did not arrive.
pub fn parecidas_o_fallo(
    db: &std::path::Path,
    text: &str,
    scope: &crate::memoria::Scope,
    kind: Option<crate::memoria::Kind>,
    k: usize,
    ids: &[String],
    budget: Duration,
) -> std::result::Result<Parecidas, Fallo> {
    if !permitido() || modelo().is_none() {
        return Err(Fallo::Apagado);
    }
    let Some(r) = registro() else {
        arrancar(Some(db));
        return Err(Fallo::SinServicio);
    };
    let p = Request {
        token: r.token.clone(),
        db: Some(db.to_string_lossy().into_owned()),
        buscar: Some(SearchRequest {
            db: db.to_string_lossy().into_owned(),
            texto: text.to_string(),
            ambito: *scope,
            kind: kind.map(|k| k.as_str().to_string()),
            k,
            ids: ids.to_vec(),
        }),
        ..Default::default()
    };
    match send_request(&r, &p, budget) {
        Ok(v) => match serde_json::from_value::<Parecidas>(v) {
            Ok(p) if p.listo => Ok(p),
            Ok(_) => Err(Fallo::Indexando),
            Err(_) => Err(Fallo::Error),
        },
        Err(e) => match e.downcast_ref::<std::io::Error>().map(|e| e.kind()) {
            Some(std::io::ErrorKind::ConnectionRefused) => {
                arrancar(Some(db));
                Err(Fallo::SinServicio)
            }
            Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => Err(Fallo::Tarde),
            _ => Err(Fallo::Error),
        },
    }
}

/// The databases the service indexes: those that have asked it something.
fn requested_dbs() -> &'static std::sync::Mutex<std::collections::BTreeSet<PathBuf>> {
    static B: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<PathBuf>>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// The vector of a query to search in `db` (which the service notes down to index it).
pub fn vector_consulta(db: &std::path::Path, text: &str, budget: Duration) -> Option<Vec<f32>> {
    vectors_in(Some(db), &[text], Tipo::Pregunta, budget)?.into_iter().next()
}

/// Tells the service that `db` has changed so it computes the missing vectors, without waiting
/// (starts it if it is not running). For `remember`, `update`, `import` and sync.
pub fn avisar(db: &std::path::Path) {
    // With nobody at the keyboard, nothing left indexing after the process (`arrancar`).
    if crate::entorno::unattended() || !en_uso_en(db) {
        return;
    }
    let _ = vectors_in(Some(db), &[], Tipo::Documento, Duration::from_millis(100));
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn registro() -> Option<Registro> {
    serde_json::from_str(&std::fs::read_to_string(home()?.join(REGISTRY_FILE)).ok()?).ok()
}

/// Sends `p` and reads one line of response, without exceeding `budget` in total.
fn send_request(r: &Registro, p: &Request, budget: Duration) -> Result<serde_json::Value> {
    let start = Instant::now();
    let addr = SocketAddr::from(([127, 0, 0, 1], r.puerto));
    let s = TcpStream::connect_timeout(&addr, budget)?;
    let remaining = || budget.saturating_sub(start.elapsed()).max(Duration::from_millis(1));
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(remaining()))?;
    let mut line = serde_json::to_string(p)?;
    line.push('\n');
    (&s).write_all(line.as_bytes())?;
    s.set_read_timeout(Some(remaining()))?;
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply)?;
    let v: serde_json::Value = serde_json::from_str(&reply).context("semantic service: bad answer")?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("semantic service: {e}");
    }
    Ok(v)
}

/// Does the service answer? Returns its registry if so.
pub fn vivo(budget: Duration) -> Option<Registro> {
    let r = registro()?;
    let v = send_request(&r, &Request { token: r.token.clone(), ping: true, ..Default::default() }, budget).ok()?;
    (v.get("pid").and_then(|p| p.as_u64()) == Some(r.pid as u64)).then_some(r)
}

/// Launches the service separately if there is a model and it was not launched recently, with the database asking for it (`db`) so
/// that it starts indexing it as soon as it loads. Does not wait for it to load.
///
/// With nobody at the keyboard (`OMOIKANE_NONINTERACTIVE=1`) it does not launch it: it would outlive the process by half an hour
/// (in CI, stretching the job or dying with it). If one is already running, it is used; if not, everything goes
/// by words.
pub fn arrancar(db: Option<&std::path::Path>) {
    if !permitido() || modelo().is_none() || crate::entorno::unattended() {
        return;
    }
    let Some(c) = home() else { return };
    let marker = c.join(STARTING_FILE);
    let recent = std::fs::read_to_string(&marker).ok().and_then(|t| t.trim().parse::<u64>().ok()).is_some_and(|t| now_secs().saturating_sub(t) < START_RETRY_SECS);
    if recent {
        return;
    }
    let _ = std::fs::create_dir_all(&c);
    if std::fs::write(&marker, now_secs().to_string()).is_ok() {
        match db {
            Some(d) => crate::sync::spawn_detached(&["--db", &d.to_string_lossy(), "model", "serve-semantic"]),
            None => crate::sync::spawn_detached(&["model", "serve-semantic"]),
        }
    }
}

/// The vectors of `texts`, if the service gives them within `budget`. If there is no service, it
/// leaves it starting and returns `None`: the caller carries on without meaning.
pub fn vectores(texts: &[&str], kind: Tipo, budget: Duration) -> Option<Vec<Vec<f32>>> {
    vectors_in(None, texts, kind, budget)
}

fn vectors_in(db: Option<&std::path::Path>, texts: &[&str], kind: Tipo, budget: Duration) -> Option<Vec<Vec<f32>>> {
    if !permitido() || modelo().is_none() {
        return None;
    }
    let Some(r) = registro() else {
        arrancar(db);
        return None;
    };
    let p = Request {
        token: r.token.clone(),
        tipo: Some(if kind == Tipo::Pregunta { "pregunta" } else { "documento" }.into()),
        textos: texts.iter().map(|t| t.to_string()).collect(),
        ping: false,
        db: db.map(|d| d.to_string_lossy().into_owned()),
        buscar: None,
    };
    match send_request(&r, &p, budget) {
        Ok(v) => serde_json::from_value(v.get("vectores")?.clone()).ok(),
        Err(e) => {
            // Refused or nobody listening: the service died (or was closed) and has to be launched
            // again. A timeout is just slowness: no other one is launched.
            let down = e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
            if down {
                arrancar(db);
            }
            None
        }
    }
}

fn new_token() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn handle(s: TcpStream, model: &Gemma, token: &str, model_name: &str, last_use: &AtomicU64) -> Result<()> {
    s.set_nodelay(true)?;
    let mut reader = BufReader::new(&s);
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        last_use.store(now_secs(), Ordering::Relaxed);
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(e) => serde_json::json!({ "error": e.to_string() }),
            Ok(p) if p.token != token => serde_json::json!({ "error": "bad token" }),
            Ok(p) if p.ping => serde_json::json!({ "pid": std::process::id(), "modelo": model_name }),
            Ok(p) => {
                // While serving, indexing waits (`yield_to_queries`).
                ACTIVE_QUERIES.fetch_add(1, Ordering::Relaxed);
                struct Done;
                impl Drop for Done {
                    fn drop(&mut self) {
                        ACTIVE_QUERIES.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                let _done = Done;
                if let Some(db) = &p.db {
                    requested_dbs().lock().map(|mut b| b.insert(PathBuf::from(db))).ok();
                }
                if let Some(b) = &p.buscar {
                    let r = match search_in_service(model, b) {
                        Ok(r) => serde_json::to_value(r).unwrap_or_default(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                    };
                    let mut out = r.to_string();
                    out.push('\n');
                    (&s).write_all(out.as_bytes())?;
                    line.clear();
                    continue;
                }
                let kind = if p.tipo.as_deref() == Some("documento") { Tipo::Documento } else { Tipo::Pregunta };
                match p.textos.iter().map(|t| model.vector(t, kind)).collect::<Result<Vec<_>>>() {
                    Ok(v) => serde_json::json!({ "vectores": v }),
                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                }
            }
        };
        let mut out = reply.to_string();
        out.push('\n');
        (&s).write_all(out.as_bytes())?;
        line.clear();
    }
    Ok(())
}

/// The service (`omoi model serve-semantic`). If one is already answering, it exits without doing anything. `db`:
/// the database that started it, to index it as soon as the model loads.
pub fn servir(db: Option<&std::path::Path>) -> Result<()> {
    if !permitido() {
        bail!("the meaning model is off (`omoi model semantic --on` turns it on)");
    }
    if vivo(Duration::from_millis(500)).is_some() {
        return Ok(());
    }
    let dir = modelo().context("EmbeddingGemma is not installed")?;
    let c = home().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    let gemma = Arc::new(Gemma::cargar(&dir)?);
    {
        let g = gemma.clone();
        std::thread::spawn(move || g.precalentar());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let me = Registro {
        puerto: listener.local_addr()?.port(),
        token: new_token()?,
        pid: std::process::id(),
        modelo: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    // First to a temp file and then rename: a reader never sees a half-written registry.
    let tmp = c.join(format!("{REGISTRY_FILE}.{}", me.pid));
    std::fs::write(&tmp, serde_json::to_string(&me)?)?;
    std::fs::rename(&tmp, c.join(REGISTRY_FILE))?;
    let _ = std::fs::remove_file(c.join(STARTING_FILE));
    if let Some(d) = db {
        requested_dbs().lock().map(|mut b| b.insert(d.to_path_buf())).ok();
    }
    let last_use = Arc::new(AtomicU64::new(now_secs()));
    let idle = std::env::var("OMOIKANE_SEMANTIC_IDLE").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_IDLE_SECS);
    {
        // Watchdog: shuts down after `idle` without requests, or if another service started at the same time and
        // took over the registry (the last one to write it wins).
        let (last_use, me, c) = (last_use.clone(), me.clone(), c.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let still_mine = registro().is_some_and(|r| r.pid == me.pid && r.puerto == me.puerto);
                if !still_mine {
                    std::process::exit(0);
                }
                if now_secs().saturating_sub(last_use.load(Ordering::Relaxed)) >= idle {
                    let _ = std::fs::remove_file(c.join(REGISTRY_FILE));
                    std::process::exit(0);
                }
            }
        });
    }
    {
        // Indexer: computes the missing vectors in the databases that have asked something, only when
        // a second has passed without requests (they share the CPU), in batches (`indexar`), and pausing between
        // steps if a query comes in (`yield_to_queries`). While it has work left, it counts as use: the
        // service does not shut down halfway.
        let (gemma, last_use) = (gemma.clone(), last_use.clone());
        std::thread::spawn(move || {
            let mut open: std::collections::BTreeMap<PathBuf, rusqlite::Connection> = Default::default();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let dbs: Vec<PathBuf> = requested_dbs().lock().map(|b| b.iter().cloned().collect()).unwrap_or_default();
                for path in dbs {
                    if now_secs().saturating_sub(last_use.load(Ordering::Relaxed)) < 1 {
                        break;
                    }
                    if !open.contains_key(&path) {
                        match crate::db::open(&path) {
                            Ok(c) => {
                                open.insert(path.clone(), c);
                            }
                            Err(_) => {
                                requested_dbs().lock().map(|mut b| b.remove(&path)).ok();
                                continue;
                            }
                        }
                    }
                    let db = &open[&path];
                    let mut keep_going = || now_secs().saturating_sub(last_use.load(Ordering::Relaxed)) >= 1;
                    // Each memory done counts as use (one second back, so as not to hold itself back).
                    let mut on_done = |_| {
                        last_use.fetch_max(now_secs().saturating_sub(1), Ordering::Relaxed);
                    };
                    match indexar(db, &gemma, 64, &mut keep_going, &mut on_done) {
                        Ok(0) => {
                            // Up to date: removed from the list until someone asks something again.
                            requested_dbs().lock().map(|mut b| b.remove(&path)).ok();
                            open.remove(&path);
                        }
                        Ok(_) => {}
                        Err(_) => {
                            requested_dbs().lock().map(|mut b| b.remove(&path)).ok();
                            open.remove(&path);
                        }
                    }
                }
            }
        });
    }
    for s in listener.incoming().flatten() {
        let (gemma, token, model_name, last_use) = (gemma.clone(), me.token.clone(), me.modelo.clone(), last_use.clone());
        std::thread::spawn(move || {
            let _ = handle(s, &gemma, &token, &model_name, &last_use);
        });
    }
    Ok(())
}
