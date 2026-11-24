//! Trades (stacks): android, web, wordpress, rust… What Kumi, the megafactory, asks for (PLAN §9.7b):
//! each assembly line has its trade memory. «Don't kill the emulator with taskkill» is useful to
//! any Android project, of any client, and doesn't reach the web factory.
//!
//! A level between the project and global, without touching schema 10:
//! - **A memory's trade**: a global memory (no project or space) with the reserved tag
//!   `trade:<trade>` (it may carry several). It travels, is exported and imported like any
//!   tag, so a 1.0 understands it fully (it sees it as one more global; see the CHANGELOG). On a
//!   project or space memory the tag means nothing: a client's memory never becomes a trade
//!   one by carrying it.
//! - **A project's trade**: what is detected in its files (cheap and deterministic: the root and
//!   one level of subfolders), plus what the person corrects by hand (`omoi trades add|remove|set`), in
//!   two local tables that don't travel (`project_trade`, `project_trade_seen`): each machine detects it
//!   in its folder, and a correction belongs to whoever makes it. Like those in `db::LOCALES`, a 1.0 ignores them.
//!
//! From a project you see the memories of the trades it has; from a space, those of the trades
//! of its projects; from the user's folder, all of them (`Scope::sql`, with [`hidden_sql`]).
//!
//! The most important thing is that nothing of a client gets out through here: when saving a trade memory,
//! [`leaks`] warns if it names projects, spaces (clients), absolute paths, repositories or domains
//! that only appear in a client's memories (in the text, paths, tags, key and
//! data), and it isn't saved except for the false positives the `allow_leaks` permission lets through (never a
//! path or a repository). Promoting a client's memory to a trade creates a new memory
//! ([`create_trade_memory`]): its history and verdicts stay with the client. And through sync
//! they only go through the general repository (they are global: `sync::va_al_repo` no longer lets them go to a space one),
//! and a client repository doesn't bring them (`sync`, with [`has_trade`]).

use crate::memoria::{Scope, Memory};
use crate::tr;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// The tag of a trade memory is this prefix and the trade: `trade:android`.
pub const PREFIX: &str = "trade:";

/// The trades detected automatically. The names are those of Kumi's factories (`android`, `web`,
/// `rust`, `plugin-claude`, `python`, `escritorio-node`, `alexa`), so no equivalence table is
/// needed; `seo-geo` can't be reliably inferred from the files and is set by hand, like any
/// other (`kotlin`…).
pub const DETECTABLES: &[&str] = &[
    "3d-print",
    "alexa",
    "android",
    "browser-extension",
    "cpp",
    "dart",
    "deno",
    "docker",
    "dotnet",
    "escritorio-node",
    "flutter",
    "go",
    "godot",
    "ios",
    "jvm",
    "node",
    "php",
    "plugin-claude",
    "python",
    "react-native",
    "ruby",
    "rust",
    "swift",
    "terraform",
    "unity",
    "web",
    "wordpress",
];

const TABLES: &str = "
CREATE TABLE IF NOT EXISTS project_trade (
  project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  trade      TEXT NOT NULL,
  how        TEXT NOT NULL,
  PRIMARY KEY (project_id, trade)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS project_trade_seen (
  project_id INTEGER PRIMARY KEY REFERENCES project(id) ON DELETE CASCADE,
  seen_at    TEXT NOT NULL,
  fixed      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS memory_oficio ON memory(n) WHERE project_id = 0 AND space_id IS NULL AND instr(tags, '\"trade:') > 0;
";

/// `project_trade.how`: detected in the files, added by hand or removed by hand (wins over what is detected).
const AUTO: &str = "auto";
const ADDED: &str = "add";
const REMOVED: &str = "off";

/// Creates the trade tables and the partial index of trade memories (`memory_oficio`: only
/// the globals with the tag, to list them without scanning the table). Normally they already exist.
pub fn create_table(db: &Connection) -> Result<()> {
    let existing: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name IN ('project_trade', 'project_trade_seen', 'memory_oficio')",
        [],
        |row| row.get(0),
    )?;
    if existing < 3 {
        db.execute_batch(TABLES)?;
    }
    Ok(())
}

// --- Trade memories -------------------------------------------------------------------------------

/// The tag of a trade.
pub fn tag(trade: &str) -> String {
    format!("{PREFIX}{trade}")
}

/// The trades of some tags (`trade:<trade>`).
pub fn from_tags(tags: &[String]) -> Vec<String> {
    tags.iter().filter_map(|t| t.strip_prefix(PREFIX)).filter(|t| !t.is_empty()).map(String::from).collect()
}

/// Whether some tags carry any trade.
pub fn has_trade(tags: &[String]) -> bool {
    tags.iter().any(|t| t.starts_with(PREFIX))
}

/// The tags without the trade ones.
pub fn without_trade(tags: Vec<String>) -> Vec<String> {
    tags.into_iter().filter(|t| !t.starts_with(PREFIX) && !t.starts_with(FOR_PREFIX)).collect()
}

/// Who a trade memory is for, if not for everyone: `trade-for:client` (only a client's projects,
/// those in a space) or `trade-for:personal` (only those in none).
/// Without the tag, for both. Nothing new is needed in the project: the space already says it.
pub const FOR_PREFIX: &str = "trade-for:";
pub const CLIENT: &str = "client";
pub const PERSONAL: &str = "personal";

/// `for` of `remember` and `update`: `client`, `personal` or nothing (`""`, `both`: for both).
pub fn parse_for_whom(text: &str) -> Result<Option<String>> {
    let t = text.trim().to_lowercase();
    Ok(match t.as_str() {
        "" | "both" | "all" | "any" => None,
        "client" | "clients" | "cliente" => Some(CLIENT.to_string()),
        "personal" | "own" | "propio" => Some(PERSONAL.to_string()),
        _ => anyhow::bail!(tr!(
            "«{}» is not valid for `for`: client (projects in a space), personal (projects in no space) or both",
            "«{}» no vale para `for`: client (proyectos en un espacio), personal (proyectos sin espacio) o both",
            text.trim()
        )),
    })
}

/// Who a trade memory is for, by its tags (`None`: for everyone).
pub fn for_whom_of(tags: &[String]) -> Option<String> {
    tags.iter().find_map(|t| t.strip_prefix(FOR_PREFIX)).filter(|p| *p == CLIENT || *p == PERSONAL).map(String::from)
}

/// The tags of a trade memory: its own (without the trade ones), its trades and who it is for.
pub fn trade_tags(tags: Vec<String>, trades: &[String], for_whom: Option<&str>) -> Vec<String> {
    without_trade(tags).into_iter().chain(trades.iter().map(|o| tag(o))).chain(for_whom.map(|p| format!("{FOR_PREFIX}{p}"))).collect()
}

/// A memory's trades: those of its tags if it is global; none if it belongs to a project or a
/// space (there the tag doesn't count).
pub fn of_memory(m: &Memory) -> Vec<String> {
    if m.global() { from_tags(&m.tags) } else { Vec::new() }
}

/// A valid trade name: lowercase, letters, digits, `-`, `+`, `.` and `#` (`c#`, `c++`), up to 32.
pub fn valid_name(text: &str) -> Result<String> {
    let n = text.trim().trim_start_matches(PREFIX).trim().to_lowercase();
    ensure!(
        !n.is_empty()
            && n.chars().count() <= 32
            && n.chars().next().is_some_and(|c| c.is_alphanumeric())
            && n.chars().all(|c| c.is_alphanumeric() || "-+.#".contains(c)),
        tr!(
            "«{}» is not a valid trade: letters, digits, -, +, . and #, up to 32 (android, web, wordpress…)",
            "«{}» no vale como oficio: letras, cifras, -, +, . y #, hasta 32 (android, web, wordpress…)",
            text.trim()
        )
    );
    Ok(n)
}

/// The trades of a text like «android, web» (separated by commas or spaces), validated and without repeats.
pub fn list(text: &str) -> Result<Vec<String>> {
    let mut v: Vec<String> = Vec::new();
    for t in text.split([',', ' ']).filter(|t| !t.trim().is_empty()) {
        let n = valid_name(t)?;
        if !v.contains(&n) {
            v.push(n);
        }
    }
    Ok(v)
}

/// The projects whose trades count from a scope, as an SQL list (`None`: from everything, all are seen).
fn projects_sql(a: &Scope) -> Option<String> {
    match a {
        Scope::All => None,
        Scope::Project { id, .. } => Some(id.to_string()),
        Scope::Space(Some(s)) => Some(format!("SELECT id FROM project WHERE space_id = {s}")),
        Scope::Space(None) => Some("SELECT id FROM project WHERE space_id IS NULL AND id <> 0".to_string()),
    }
}

/// The trade memories (by `n`) not seen from `a`: those of trades no project of the scope has
/// (client ones if `trade-for:client`, personal ones if `trade-for:personal`). `None` from everything. It is an uncorrelated subquery: SQLite runs it once
/// per query, through the partial index `memory_oficio` (which `create_table` always creates; without `INDEXED BY`,
/// SQLite preferred `memory_scope` and read all the globals), and then checks each global by its `n`.
pub(crate) fn hidden_sql(a: &Scope) -> Option<String> {
    let ps = projects_sql(a)?;
    Some(format!(
        "SELECT o.n FROM memory o INDEXED BY memory_oficio WHERE o.project_id = 0 AND o.space_id IS NULL AND instr(o.tags, '\"trade:') > 0
           AND NOT EXISTS (SELECT 1 FROM json_each(o.tags) t JOIN project_trade pt ON pt.trade = substr(t.value, 7) JOIN project p ON p.id = pt.project_id
                           WHERE t.value LIKE 'trade:%' AND pt.how <> 'off' AND pt.project_id IN ({ps})
                             AND (instr(o.tags, '\"trade-for:client\"') = 0 OR p.space_id IS NOT NULL)
                             AND (instr(o.tags, '\"trade-for:personal\"') = 0 OR p.space_id IS NULL))"
    ))
}

/// Whether a memory (by its id) is a trade one not seen from `a`.
pub(crate) fn is_hidden(db: &Connection, a: &Scope, id: &str) -> Result<bool> {
    match hidden_sql(a) {
        None => Ok(false),
        Some(q) => Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE m.id = ?1 AND m.n IN ({q}))"), [id], |row| row.get(0))?),
    }
}

/// How many trade memories aren't seen from `a`.
pub(crate) fn hidden_count(db: &Connection, a: &Scope) -> Result<i64> {
    match hidden_sql(a) {
        None => Ok(0),
        Some(q) => Ok(db.query_row(&format!("SELECT count(*) FROM ({q})"), [], |row| row.get(0))?),
    }
}

// --- A project's trades ---------------------------------------------------------------------------

/// A project's trades (detected or set by hand, without the removed ones), in alphabetical order.
pub fn of_project(db: &Connection, project: i64) -> Result<Vec<String>> {
    let mut q = db.prepare_cached("SELECT trade FROM project_trade WHERE project_id = ?1 AND how <> 'off' ORDER BY trade")?;
    Ok(q.query_map([project], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// The trades of all projects, by id.
pub fn of_projects(db: &Connection) -> Result<HashMap<i64, Vec<String>>> {
    let mut q = db.prepare("SELECT project_id, trade FROM project_trade WHERE how <> 'off' ORDER BY project_id, trade")?;
    let mut m: HashMap<i64, Vec<String>> = HashMap::new();
    for row in q.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))? {
        let (p, t) = row?;
        m.entry(p).or_default().push(t);
    }
    Ok(m)
}

/// What is known about a project's trades, for `omoi trades`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TradeStatus {
    /// The ones that count.
    pub trades: Vec<String>,
    /// Detected in its files.
    pub detected: Vec<String>,
    /// Set by hand.
    pub added: Vec<String>,
    /// Removed by hand (even if detected).
    pub removed: Vec<String>,
    /// With `omoi trades set`: detection no longer adds anything.
    pub fixed: bool,
    /// When its files were last looked at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
}

pub fn status(db: &Connection, project: i64) -> Result<TradeStatus> {
    let mut e = TradeStatus::default();
    let mut q = db.prepare("SELECT trade, how FROM project_trade WHERE project_id = ?1 ORDER BY trade")?;
    for row in q.query_map([project], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (t, how) = row?;
        match how.as_str() {
            AUTO => e.detected.push(t),
            REMOVED => e.removed.push(t),
            _ => e.added.push(t),
        }
    }
    let seen: Option<(String, bool)> =
        db.query_row("SELECT seen_at, fixed FROM project_trade_seen WHERE project_id = ?1", [project], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    if let Some((s, fixed)) = seen {
        e.seen_at = Some(s);
        e.fixed = fixed;
    }
    e.trades = e.detected.iter().chain(&e.added).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    Ok(e)
}

/// Writes inside a `SAVEPOINT` (works inside and outside a transaction).
fn in_savepoint<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    db.execute_batch("SAVEPOINT oficios")?;
    match f() {
        Ok(v) => {
            db.execute_batch("RELEASE oficios")?;
            Ok(v)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK TO oficios; RELEASE oficios");
            Err(e)
        }
    }
}

fn mark_seen(db: &Connection, project: i64, fixed: Option<bool>) -> Result<()> {
    db.execute(
        "INSERT INTO project_trade_seen(project_id, seen_at, fixed) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), COALESCE(?2, 0))
         ON CONFLICT(project_id) DO UPDATE SET seen_at = excluded.seen_at, fixed = COALESCE(?2, fixed)",
        params![project, fixed],
    )?;
    Ok(())
}

/// Looks at the project's files (in `path`) and records what it detects, unless they were fixed by
/// hand (`set`). Only writes if something changes or it is the first time. Returns the trades that count.
/// A folder that doesn't exist here changes nothing.
pub fn refresh(db: &Connection, project: i64, path: &str) -> Result<Vec<String>> {
    if project <= 0 || path.is_empty() || !crate::proyecto::is_dir(Path::new(path)) {
        return of_project(db, project);
    }
    let e = status(db, project)?;
    if e.fixed {
        return Ok(e.trades);
    }
    let detected = detect_cached(db, project, Path::new(path));
    if e.seen_at.is_none() || detected != e.detected {
        in_savepoint(db, || {
            db.execute("DELETE FROM project_trade WHERE project_id = ?1 AND how = 'auto'", [project])?;
            for t in &detected {
                db.execute("INSERT OR IGNORE INTO project_trade(project_id, trade, how) VALUES (?1, ?2, 'auto')", params![project, t])?;
            }
            mark_seen(db, project, None)
        })?;
    }
    of_project(db, project)
}

/// Like `refresh`, but only if it was never looked at (for places that mustn't look at the disk every time).
pub fn refresh_if_missing(db: &Connection, project: i64, path: &str) -> Result<Vec<String>> {
    let seen: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM project_trade_seen WHERE project_id = ?1)", [project], |row| row.get(0))?;
    if seen { of_project(db, project) } else { refresh(db, project, path) }
}

/// Before looking from a folder (MCP, CLI): its trades, if never looked at. A project that has
/// nothing yet gets recorded if it has any (the memories of its trade already serve it), and one without
/// trades isn't recorded. From a general folder, nothing.
pub fn prepare(db: &Connection, path: &str) -> Result<()> {
    if path.is_empty() || crate::proyecto::is_general(path) {
        return Ok(());
    }
    match crate::proyecto::resolve(db, path)? {
        Some((id, _)) if id > 0 => {
            refresh_if_missing(db, id, path)?;
        }
        Some(_) => {}
        None => {
            if !detect(Path::new(path)).is_empty() {
                let id = crate::proyecto::register(db, path)?;
                refresh(db, id, path)?;
            }
        }
    }
    Ok(())
}

/// `omoi trades add`: the project has this trade even if not detected.
pub fn add(db: &Connection, project: i64, trades: &[String]) -> Result<()> {
    in_savepoint(db, || {
        for t in trades {
            db.execute(
                "INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3) ON CONFLICT(project_id, trade) DO UPDATE SET how = ?3",
                params![project, t, ADDED],
            )?;
        }
        mark_seen(db, project, None)
    })
}

/// `omoi trades remove`: the project doesn't have this trade even if detected.
pub fn remove(db: &Connection, project: i64, trades: &[String]) -> Result<()> {
    in_savepoint(db, || {
        for t in trades {
            db.execute(
                "INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3) ON CONFLICT(project_id, trade) DO UPDATE SET how = ?3",
                params![project, t, REMOVED],
            )?;
        }
        mark_seen(db, project, None)
    })
}

/// `omoi trades set`: exactly these, and detection stops adding.
pub fn set(db: &Connection, project: i64, trades: &[String]) -> Result<()> {
    in_savepoint(db, || {
        db.execute("DELETE FROM project_trade WHERE project_id = ?1", [project])?;
        for t in trades {
            db.execute("INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3)", params![project, t, ADDED])?;
        }
        mark_seen(db, project, Some(true))
    })
}

/// `omoi trades auto`: what was set by hand is forgotten and detection runs again.
pub fn auto(db: &Connection, project: i64, path: &str) -> Result<Vec<String>> {
    in_savepoint(db, || {
        db.execute("DELETE FROM project_trade WHERE project_id = ?1", [project])?;
        db.execute("DELETE FROM project_trade_seen WHERE project_id = ?1", [project])?;
        Ok(())
    })?;
    refresh(db, project, path)
}

/// How many memories of each trade there are.
pub fn memories_per_trade(db: &Connection) -> Result<HashMap<String, i64>> {
    let mut q = db.prepare(
        "SELECT substr(t.value, 7), count(*) FROM memory o, json_each(o.tags) t
         WHERE o.project_id = 0 AND o.space_id IS NULL AND instr(o.tags, '\"trade:') > 0 AND t.value LIKE 'trade:%'
         GROUP BY 1",
    )?;
    Ok(q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
}

// --- Detection ------------------------------------------------------------------------------------

/// Root subfolders looked at at most (a monorepo: `app/`, `web/`, `android/`…): the
/// first ones in alphabetical order, so with more than these the same ones are always looked at.
const MAX_SUBFOLDERS: usize = 60;
/// Entries of a folder looked at at most (the first ones in alphabetical order).
const MAX_ENTRIES: usize = 3000;
/// Entries of a folder that get listed to sort them. Only a folder with more than these
/// depends on the order the system gives them.
const MAX_LISTED: usize = 20_000;
/// Bytes read from a file to look at its content.
const MAX_BYTES: u64 = 256 * 1024;
/// `.php` files of a folder whose start is looked at (the «Plugin Name:» header).
const MAX_PHP: usize = 10;
/// Where what was detected in a project is stored in `meta` (local, doesn't travel): `<signature>\t<trades as JSON>`.
const DETECTED_KEY: &str = "oficios_detectados:";

/// Subfolders that say nothing about the trade (dependencies, build output, caches).
fn ignored(name: &str) -> bool {
    let l = name.to_lowercase();
    l.starts_with('.')
        || matches!(
            l.as_str(),
            "node_modules"
                | "target"
                | "build"
                | "dist"
                | "out"
                | "vendor"
                | "venv"
                | "env"
                | "__pycache__"
                | "bin"
                | "obj"
                | "pods"
                | "deriveddata"
                | "coverage"
                | "tmp"
                | "temp"
                | "cache"
                | "logs"
                | "site-packages"
        )
}

/// The files (lowercase) whose content is looked at besides their name (and the `.php` ones).
fn is_read(l: &str) -> bool {
    matches!(l, "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" | "package.json" | "style.css" | "pubspec.yaml" | "manifest.json")
}

/// An entry of a folder. For the files that are read, their fingerprint: size, last modification
/// (in ns) and whether it is cloud-only.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    is_dir: bool,
    fingerprint: Option<(u64, u128, bool)>,
}

impl Entry {
    /// Whether it can't be read without downloading it (OneDrive, Dropbox… with «files on demand»).
    fn is_cloud_only(&self) -> bool {
        self.fingerprint.is_some_and(|h| h.2)
    }
}

/// Whether some Windows attributes are those of a cloud-only file: reading it (or, with
/// `RECALL_ON_OPEN`, opening it) downloads it. `OFFLINE` is the long-standing one for archived files.
#[cfg(any(windows, test))]
fn cloud_attributes(a: u32) -> bool {
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    a & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(windows)]
fn metadata_is_cloud_only(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    cloud_attributes(m.file_attributes())
}

#[cfg(not(windows))]
fn metadata_is_cloud_only(_: &std::fs::Metadata) -> bool {
    false
}

/// The entries of a folder, in alphabetical order (case-insensitive) and up to `MAX_ENTRIES`.
/// What is known about each file comes from the listing (on Windows, an entry's `metadata` comes with
/// it and doesn't open the file), so a cloud-only file isn't downloaded by listing it.
fn read_folder(dir: &Path) -> Vec<Entry> {
    let Ok(it) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(String, std::fs::DirEntry)> = it.flatten().take(MAX_LISTED).map(|e| (e.file_name().to_string_lossy().into_owned(), e)).collect();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    v.truncate(MAX_ENTRIES);
    let mut php = 0;
    v.into_iter()
        .map(|(name, e)| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let l = name.to_lowercase();
            let reads = !is_dir
                && (is_read(&l)
                    || (l.ends_with(".php") && {
                        php += 1;
                        php <= MAX_PHP
                    }));
            let fingerprint = reads.then(|| match e.metadata() {
                Ok(m) => {
                    let when = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
                    (m.len(), when, metadata_is_cloud_only(&m))
                }
                Err(_) => (0, 0, false),
            });
            Entry { name, is_dir, fingerprint }
        })
        .collect()
}

/// What is seen of a folder to detect its trades, without reading any file.
#[derive(Debug)]
struct Folder {
    /// Its name inside the root (empty: the root).
    rel: String,
    entries: Vec<Entry>,
    /// Has `hooks/hooks.json` (a Claude Code mod).
    hooks: bool,
    /// Has `src/main/AndroidManifest.xml` (an Android module).
    manifest: bool,
}

/// A project's root and one level of subfolders, as seen in their listings.
#[derive(Debug)]
struct Walk {
    root: std::path::PathBuf,
    folders: Vec<Folder>,
}

/// The root and its subfolders (without dependencies, build output or hidden ones), the first `MAX_SUBFOLDERS`
/// in alphabetical order. Lists folders and nothing else: doesn't read any file.
fn walk(root: &Path) -> Walk {
    let folder = |rel: String, dir: &Path| {
        let entries = read_folder(dir);
        let has_dir = |n: &str| entries.iter().any(|e| e.is_dir && e.name.eq_ignore_ascii_case(n));
        let hooks = has_dir("hooks") && dir.join("hooks").join("hooks.json").is_file();
        let manifest = has_dir("src") && dir.join("src").join("main").join("AndroidManifest.xml").is_file();
        Folder { rel, entries, hooks, manifest }
    };
    let first = folder(String::new(), root);
    let subs: Vec<String> = first.entries.iter().filter(|e| e.is_dir && !ignored(&e.name)).take(MAX_SUBFOLDERS).map(|e| e.name.clone()).collect();
    let mut folders = vec![first];
    folders.extend(subs.into_iter().map(|n| {
        let d = root.join(&n);
        folder(n, &d)
    }));
    Walk { root: root.to_path_buf(), folders }
}

impl Walk {
    /// Changes if anything detection depends on changes: a name, a folder, or the size, the
    /// date or the cloud state of a file that is read.
    fn signature(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for c in &self.folders {
            h.update(format!("{}\0{}{}\n", c.rel, u8::from(c.hooks), u8::from(c.manifest)).as_bytes());
            for e in &c.entries {
                h.update(format!("{}\0{}\0{:?}\n", e.name, u8::from(e.is_dir), e.fingerprint).as_bytes());
            }
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The trades, reading the start of the files that are read (never that of a cloud-only one).
    fn trades(&self) -> Vec<String> {
        let mut v = BTreeSet::new();
        let mut gradle_without_android = false;
        for c in &self.folders {
            inspect(&self.root.join(&c.rel), c, c.rel.is_empty(), &mut v, &mut gradle_without_android);
        }
        // A Gradle without Android (anywhere) is JVM: in an Android project, the root's `settings.gradle`
        // doesn't always name it.
        if gradle_without_android && !v.contains("android") {
            v.insert("jvm".into());
        }
        // A desktop app built with web technology is not a website (its `vite.config` doesn't say so either).
        if v.contains("escritorio-node") {
            v.remove("web");
        }
        v.into_iter().collect()
    }
}

/// The start of a file, lowercase (empty if it can't be read). `None` if it is cloud-only:
/// reading it would download it (in a OneDrive folder, on every session start).
fn file_start(path: &Path, e: &Entry) -> Option<String> {
    use std::io::Read;
    if e.is_cloud_only() {
        return None;
    }
    let mut s = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(MAX_BYTES).read_to_end(&mut s);
    }
    Some(String::from_utf8_lossy(&s).to_lowercase())
}

/// What a `package.json` says (its text; `None` if it couldn't be read without downloading it): its dependencies.
fn from_package_json(text: Option<String>, v: &mut BTreeSet<String>) {
    v.insert("node".into());
    // `file_start` gives it lowercase: the keys come as `devdependencies`.
    let Some(Ok(j)) = text.map(|t| serde_json::from_str::<serde_json::Value>(&t)) else { return };
    let deps: HashSet<String> = ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|k| j.get(k.to_lowercase()).and_then(|d| d.as_object()))
        .flat_map(|d| d.keys().cloned())
        .collect();
    let mobile = deps.contains("react-native") || deps.contains("expo");
    if mobile {
        v.insert("react-native".into());
    }
    // A desktop app with Electron or Tauri (its interface is web, but it isn't a website).
    if ["electron", "@tauri-apps/api", "@tauri-apps/cli", "nw", "neutralinojs"].iter().any(|d| deps.contains(*d)) {
        v.insert("escritorio-node".into());
    }
    const WEB: &[&str] = &[
        "astro",
        "next",
        "nuxt",
        "vue",
        "svelte",
        "@sveltejs/kit",
        "@angular/core",
        "solid-js",
        "preact",
        "lit",
        "gatsby",
        "@remix-run/react",
        "vite",
        "vitepress",
        "@docusaurus/core",
        "@11ty/eleventy",
        "ember-source",
        "jquery",
        "alpinejs",
        "htmx.org",
        "@builder.io/qwik",
        "webpack",
        "parcel",
    ];
    // Plain React is web except in a React Native app (which also carries it).
    if WEB.iter().any(|w| deps.contains(*w)) || (!mobile && (deps.contains("react") || deps.contains("react-dom"))) {
        v.insert("web".into());
    }
}

/// A project's trades by its files: the root and one level of subfolders (without dependencies,
/// build output or hidden ones), without walking the tree. Deterministic: the same files give the same result.
pub fn detect(root: &Path) -> Vec<String> {
    walk(root).trades()
}

/// Like [`detect`], with what was detected last time in this project if its folders haven't changed
/// ([`Walk::signature`]): this way only folders are listed, and the start of their files is read when
/// one changes (on OneDrive or a network folder, reading them on every start is slow).
fn detect_cached(db: &Connection, project: i64, root: &Path) -> Vec<String> {
    let r = walk(root);
    let signature = r.signature();
    let key = format!("{DETECTED_KEY}{project}");
    let stored: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [&key], |row| row.get(0)).optional().ok().flatten();
    if let Some((f, json)) = stored.as_deref().and_then(|g| g.split_once('\t'))
        && f == signature
        && let Ok(v) = serde_json::from_str::<Vec<String>>(json)
    {
        return v;
    }
    let v = r.trades();
    if let Ok(json) = serde_json::to_string(&v) {
        // It's only a shortcut: if it can't be saved, next time it is read again.
        let _ = db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![key, format!("{signature}\t{json}")]);
    }
    v
}

fn inspect(dir: &Path, c: &Folder, is_root: bool, v: &mut BTreeSet<String>, gradle_without_android: &mut bool) {
    let mut here: BTreeSet<String> = BTreeSet::new();
    let mut packages: BTreeSet<String> = BTreeSet::new();
    let mut put = |t: &str| {
        here.insert(t.to_string());
    };
    let names: HashSet<String> = c.entries.iter().map(|e| e.name.to_lowercase()).collect();
    for e in &c.entries {
        let l = e.name.to_lowercase();
        let ext = l.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if e.is_dir {
            match l.as_str() {
                "wp-content" | "wp-includes" => put("wordpress"),
                "skill-package" => put("alexa"),
                ".claude-plugin" => put("plugin-claude"),
                "src-tauri" => put("escritorio-node"),
                "projectsettings" if names.contains("assets") => put("unity"),
                _ if ext == "xcodeproj" || ext == "xcworkspace" => put("ios"),
                _ => {}
            }
            continue;
        }
        let path = dir.join(&e.name);
        let read = || file_start(&path, e);
        match l.as_str() {
            "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
                // A cloud-only one says nothing (reading it would download it).
                match read() {
                    Some(t) if t.contains("android") => put("android"),
                    Some(_) => *gradle_without_android = true,
                    None => {}
                }
            }
            "androidmanifest.xml" => put("android"),
            "pom.xml" => put("jvm"),
            "cargo.toml" => put("rust"),
            "go.mod" => put("go"),
            "pyproject.toml" | "setup.py" | "setup.cfg" | "pipfile" | "uv.lock" | "poetry.lock" => put("python"),
            "package.json" => from_package_json(read(), &mut packages),
            "angular.json" => put("web"),
            "index.html" | "_config.yml" | "hugo.toml" if is_root => put("web"),
            "wp-config.php" => put("wordpress"),
            "style.css" => {
                if read().is_some_and(|t| t.contains("theme name:")) {
                    put("wordpress");
                }
            }
            "composer.json" => put("php"),
            "gemfile" => put("ruby"),
            "pubspec.yaml" => {
                put("dart");
                if read().is_some_and(|t| t.contains("flutter")) {
                    put("flutter");
                }
            }
            "podfile" => put("ios"),
            "package.swift" => put("swift"),
            "cmakelists.txt" => put("cpp"),
            "dockerfile" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => put("docker"),
            "project.godot" => put("godot"),
            "manifest.json" => {
                if read().is_some_and(|t| t.contains("\"manifest_version\"")) {
                    put("browser-extension");
                }
            }
            "deno.json" | "deno.jsonc" => put("deno"),
            "tauri.conf.json" => put("escritorio-node"),
            "ask-resources.json" => put("alexa"),
            _ => {}
        }
        if l.starts_with("requirements") && ext == "txt" {
            put("python");
        }
        if l.starts_with("dockerfile.") {
            put("docker");
        }
        if ["astro.config.", "next.config.", "vite.config.", "nuxt.config.", "svelte.config.", "gatsby-config.", "remix.config."].iter().any(|p| l.starts_with(p)) {
            put("web");
        }
        match ext {
            "py" | "ipynb" if is_root => put("python"),
            "csproj" | "fsproj" | "vbproj" | "sln" => put("dotnet"),
            "tf" => put("terraform"),
            "stl" | "3mf" | "gcode" => put("3d-print"),
            "php" => {
                put("php");
                // A WordPress plugin: its main file carries the «Plugin Name:» header (the
                // first `MAX_PHP` are looked at, the ones with a fingerprint).
                if e.fingerprint.is_some() && read().is_some_and(|t| t.contains("plugin name:")) {
                    put("wordpress");
                }
            }
            _ => {}
        }
    }
    // A Claude Code mod: `hooks/hooks.json` (at the root or in a module).
    if c.hooks {
        put("plugin-claude");
    }
    // The Android manifest lives in `<module>/src/main/`.
    if c.manifest {
        put("android");
    }
    v.extend(here);
    v.extend(packages);
}

// --- Leaks: what of a client must not go in a trade memory ----------------------------------------

/// Something in the text of a trade memory that looks like a project's or a client's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Leak {
    /// `project` (a project's name), `space` (a space: a client), `path` (absolute path),
    /// `repository` (URL of a known repository or via SSH) or `domain` (a domain that only appears in
    /// a client's memories, or one's git server).
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
    /// Where it is: `title`, `content`, `paths`, `tag`, `key`, `data` or `answer` (empty if unknown).
    /// A key that names a project («kumi-ruta») is fixed in a moment if it is known to be the key.
    #[serde(rename = "field", skip_serializing_if = "str::is_empty")]
    pub field: &'static str,
}

/// Project names so generic that naming them doesn't say whose it is (and the trade names).
const GENERIC: &[&str] = &[
    "app", "apps", "web", "site", "website", "server", "client", "backend", "frontend", "api", "docs", "test", "tests", "demo", "main", "core", "common",
    "shared", "utils", "tools", "scripts", "plugin", "plugins", "mobile", "scratchpad", "temp", "project", "projects", "proyecto", "build", "release",
    "android", "kotlin", "java", "gradle", "react", "python", "node", "docker", "wordpress", "rust", "cargo", "astro", "next", "vite", "flutter", "swift",
    "theme", "themes", "library", "lib", "sdk", "cli", "bot", "service", "worker", "landing", "blog", "shop", "store", "admin", "dashboard", "src",
];

/// File extensions that, at the end of a dotted word, are not a domain (`README.md`).
const EXTENSIONS: &[&str] = &[
    "rs", "kt", "kts", "java", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "json", "jsonc", "md", "mdx", "toml", "yaml", "yml", "xml", "gradle", "txt", "html",
    "htm", "css", "scss", "php", "lock", "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "sh", "ps1", "bat", "cmd", "exe", "dll", "so", "dylib", "apk",
    "aab", "db", "sql", "log", "cfg", "ini", "conf", "properties", "vue", "svelte", "astro", "go", "rb", "cs", "swift", "c", "h", "cpp", "hpp", "zip",
    "gz", "tar", "pdf", "mp4", "mp3", "wav", "wasm", "onnx", "bin", "stl", "3mf", "gcode", "env", "pem", "key", "crt", "lnk", "csv", "tsv", "jar", "aar",
    "pro", "plist", "storyboard", "xib", "dart", "lua", "vim", "el", "ex", "exs", "erl", "hs", "ml", "fs", "r", "jl", "nim", "zig", "v", "d", "m", "mm",
    "patch", "diff", "orig", "bak", "tmp", "old", "dat", "pak", "ttf", "otf", "woff", "woff2", "map", "min", "spec", "test", "local", "example", "sample",
];

/// Public forges: their domain belongs to nobody (a repository on them does, if it belongs to a known project).
const FORGES: &[&str] = &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org", "dev.azure.com", "ssh.dev.azure.com", "sr.ht", "git.sr.ht", "huggingface.co"];

/// Whether `name` (lowercase) appears in `text` (lowercase) as a whole word.
fn appears(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        if boundary(text[..start].chars().next_back()) && boundary(text[end..].chars().next()) {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The words of the text, split by spaces and by what usually surrounds a path or a URL.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text
        .split(|c: char| c.is_whitespace() || "`\"'«»()[]<>,;|*".contains(c))
        .map(|p| p.trim_end_matches(['.', ':', '?', '!']))
        .filter(|p| !p.is_empty())
}

/// Whether a word is an absolute path of the kind that says whose it is: on a Windows drive (except
/// `Program Files` and `Windows`), a shared folder, or a Unix home or web root.
fn personal_path(p: &str) -> bool {
    let b = p.as_bytes();
    let l = p.to_lowercase().replace('\\', "/");
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        return !["program files", "windows"].iter().any(|s| l[3..].starts_with(s));
    }
    if p.starts_with("\\\\") && p.len() > 3 {
        return true;
    }
    ["/home/", "/users/", "/var/www/", "/srv/", "/root/"].iter().any(|s| l.starts_with(s) && l.len() > s.len())
}

/// The domain of a word (`https://a.b.com/x` or `a.b.com`), lowercase, if it looks like one.
fn domain(p: &str) -> Option<String> {
    let l = p.to_lowercase();
    let rest = l.split_once("://").map_or(l.as_str(), |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?.split(':').next()?.trim_end_matches('.');
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last()?;
    let valid = labels.len() >= 2
        && labels.iter().all(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && (l.contains("://") || !EXTENSIONS.contains(tld));
    valid.then(|| host.to_string())
}

/// The domains that appear in a text.
fn domains_in(text: &str) -> HashSet<String> {
    words(text).filter_map(domain).collect()
}

/// The last two labels of a domain (`gitlab.acme.com` → `acme.com`).
fn registrable(host: &str) -> String {
    let v: Vec<&str> = host.split('.').collect();
    v[v.len().saturating_sub(2)..].join(".")
}

/// The forms of a name (already lowercase and without accents) searched for in the text: as is, with
/// each separator swapped for the others and without separators («social-publish», «social publish»,
/// «social_publish», «socialpublish»).
fn variants(n: &str) -> Vec<String> {
    let mut v = vec![n.to_string(), n.replace(['-', '_'], " "), n.replace(['-', ' '], "_"), n.replace(['_', ' '], "-"), n.replace(['-', '_', ' ', '.'], "")];
    v.retain(|x| !x.is_empty());
    v.sort();
    v.dedup();
    v
}

/// Reserved tags say nothing about anyone: the trade ones (`trade:`, `trade-for:`), the review
/// date (`review:`) and the status ones (`open`, `blocking`, `resolved`, `status`). Of the key one
/// (`topic:<key>`) the key counts. Returns what must be looked at of a tag.
fn free_tag(t: &str) -> Option<&str> {
    use crate::memoria::{OPEN, BLOCKING, STATUS, TOPIC_TAG, RESOLVED};
    if t.starts_with(PREFIX) || t.starts_with(FOR_PREFIX) || t.starts_with(crate::revisar::REVIEW_DATE_TAG) || [OPEN, BLOCKING, RESOLVED, STATUS].contains(&t) {
        return None;
    }
    Some(t.strip_prefix(TOPIC_TAG).unwrap_or(t))
}

/// The keys and texts of some data (`data`), flattened, to look at them as text.
fn flatten(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| flatten(x, out)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                out.push(k.clone());
                flatten(x, out);
            }
        }
        _ => {}
    }
}

/// Everything looked at in a trade memory, as one text: the pieces given (title,
/// content, paths…), its tags without the reserved ones ([`free_tag`]), the key and the data
/// (its keys and its texts).
pub fn leak_text(parts: &[&str], tags: &[String], key: Option<&str>, data: Option<&serde_json::Value>) -> String {
    let fields: Vec<(&'static str, &str)> = parts.iter().map(|p| ("", *p)).collect();
    leak_fields(&fields, tags, key, data).into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n")
}

/// The same as [`leak_text`], field by field (`title`, `content`, `paths`, `tag`, `key`,
/// `data`…), to be able to say where each leak is.
pub fn leak_fields(parts: &[(&'static str, &str)], tags: &[String], key: Option<&str>, data: Option<&serde_json::Value>) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = parts.iter().map(|(c, p)| (*c, p.to_string())).collect();
    v.extend(tags.iter().filter_map(|t| free_tag(t)).map(|t| ("tag", t.to_string())));
    v.extend(key.map(|k| ("key", k.to_string())));
    if let Some(d) = data {
        let mut texts = Vec::new();
        flatten(d, &mut texts);
        v.extend(texts.into_iter().map(|t| ("data", t)));
    }
    v.retain(|(_, x)| !x.trim().is_empty());
    v
}

/// [`leaks`] of some fields, each leak with the field it appears in (the first one containing it).
pub fn leaks_in(db: &Connection, fields: &[(&'static str, String)]) -> Result<Vec<Leak>> {
    let text = fields.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n");
    let normalize = |s: &str| crate::nombres::fold_accents(&s.to_lowercase());
    let normalized: Vec<(&'static str, String)> = fields.iter().map(|(c, t)| (*c, normalize(t))).collect();
    let mut v = leaks(db, &text)?;
    for f in &mut v {
        let wanted = normalize(&f.text);
        // As a word (with boundaries, as `leaks` detects it): «acme» isn't from the field that says «acmeco»;
        // if it doesn't appear that way in any (a variant detected it), the first one containing it.
        f.field = normalized
            .iter()
            .find(|(_, t)| appears(t, &wanted))
            .or_else(|| normalized.iter().find(|(_, t)| t.contains(&wanted)))
            .map(|(c, _)| *c)
            .unwrap_or("");
    }
    Ok(v)
}

/// The leaks that prevent saving. With the leaks permission (`allow_leaks`), only those that are never a
/// false positive: a personal absolute path or a repository URL.
pub fn blocking_leaks(leaks: Vec<Leak>, allow: bool) -> Vec<Leak> {
    if allow { leaks.into_iter().filter(|f| f.kind == "path" || f.kind == "repository").collect() } else { leaks }
}

/// What in the text of a trade memory looks like a project's or a client's: names of
/// projects and spaces, personal absolute paths, URLs of known repositories (or via SSH) and
/// domains that only appear in a client's memories (or its git server's). Without repeats. Names
/// are compared case- and accent-insensitively («balua» is «Balùa»).
pub fn leaks(db: &Connection, text: &str) -> Result<Vec<Leak>> {
    let lower = crate::nombres::fold_accents(&text.to_lowercase());
    let mut v: Vec<Leak> = Vec::new();
    let mut put = |kind: &'static str, text: String| {
        if !v.iter().any(|f| f.kind == kind && f.text.eq_ignore_ascii_case(&text)) {
            v.push(Leak { kind, text, field: "" });
        }
    };
    // Spaces: a client, whatever its name (from 2 letters: «QA», «HP»).
    let spaces: Vec<(String, String)> = {
        let mut q = db.prepare("SELECT key, name FROM space")?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (key, name) in &spaces {
        for n in [key, name] {
            let n = crate::nombres::fold_accents(&n.trim().to_lowercase());
            if n.chars().count() >= 2 && variants(&n).iter().any(|x| x.chars().count() >= 2 && appears(&lower, x)) {
                put("space", name.clone());
            }
        }
    }
    // Projects, except those with a generic or very short name.
    let projects: Vec<(String, String)> = {
        let mut q = db.prepare("SELECT name, key FROM project WHERE id != 0")?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (name, _) in &projects {
        let n = crate::nombres::fold_accents(&name.trim().to_lowercase());
        if n.chars().count() < 4 || GENERIC.contains(&n.as_str()) || DETECTABLES.contains(&n.as_str()) || n.starts_with("tmp.") {
            continue;
        }
        if variants(&n).iter().any(|x| x.chars().count() >= 4 && appears(&lower, x)) {
            put("project", name.clone());
        }
    }
    // Absolute paths.
    for p in words(text) {
        if personal_path(p) {
            put("path", p.to_string());
        }
    }
    // Repositories: those of known projects (by their key or the `origin` of their folders).
    let mut origins: Vec<String> = projects.iter().filter_map(|(_, k)| k.strip_prefix("git:").map(String::from)).collect();
    {
        let mut q = db.prepare("SELECT DISTINCT origin FROM project_path WHERE origin != ''")?;
        origins.extend(q.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    origins.retain(|o| o.contains('/'));
    // Git servers that aren't a public forge belong to someone (`gitlab.acme.com`).
    let servers: HashSet<String> = origins
        .iter()
        .filter_map(|o| o.split('/').next())
        .filter(|h| h.contains('.') && !FORGES.contains(h))
        .map(registrable)
        .collect();
    for p in words(text) {
        let l = p.to_lowercase();
        if l.starts_with("git@") || l.starts_with("ssh://") {
            put("repository", p.to_string());
            continue;
        }
        if !l.contains("://") && !l.contains('/') {
            continue;
        }
        let n = crate::sync::normalize_remote(&l);
        if origins.iter().any(|o| n == *o || n.starts_with(&format!("{o}/"))) {
            put("repository", p.to_string());
        }
    }
    // Domains: that of someone's git server, or one that appears in a client's memories (of a
    // space or its projects) and in no global one.
    let here = domains_in(text);
    if here.is_empty() {
        return Ok(v);
    }
    let mut from_clients: HashSet<String> = HashSet::new();
    if !spaces.is_empty() {
        let mut q = db.prepare(
            "SELECT m.title || ' ' || m.content FROM memory m
             WHERE m.space_id IS NOT NULL OR m.project_id IN (SELECT id FROM project WHERE space_id IS NOT NULL)",
        )?;
        for t in q.query_map([], |row| row.get::<_, String>(0))? {
            from_clients.extend(domains_in(&t?).into_iter().filter(|d| here.contains(d)));
        }
        if !from_clients.is_empty() {
            let mut q = db.prepare("SELECT m.title || ' ' || m.content FROM memory m WHERE m.project_id = 0 AND m.space_id IS NULL")?;
            for t in q.query_map([], |row| row.get::<_, String>(0))? {
                for d in domains_in(&t?) {
                    from_clients.remove(&d);
                }
            }
        }
    }
    let mut here: Vec<String> = here.into_iter().collect();
    here.sort();
    for d in here {
        if from_clients.contains(&d) || servers.contains(&registrable(&d)) {
            put("domain", d);
        }
    }
    Ok(v)
}

/// The memories whose history doesn't travel: those a pre-1.8.0 version (unreleased) promoted
/// from a client to a trade with the same id and recorded in `meta('historial_local:<id>')`. Since
/// promoting a client's memory creates a new memory ([`create_trade_memory`]) none is recorded any more; those
/// that exist still don't travel and their history is only shown from a general folder
/// (`memoria::history_in`).
pub(crate) fn local_only_histories(db: &Connection) -> Result<HashSet<String>> {
    let mut q = db.prepare("SELECT substr(k, 17) FROM meta WHERE k LIKE 'historial_local:%'")?;
    Ok(q.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Whether this memory's history stayed on this machine (see [`local_only_histories`]).
pub(crate) fn history_local_only(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE k = 'historial_local:' || ?1)", [id], |row| row.get(0))?)
}

/// What an `update` touching the trades asks for (MCP and CLI).
pub struct ChangeRequest<'a> {
    /// `trade`: the trades («android, web»); `Some("")` stops being a trade one.
    pub trade: Option<&'a str>,
    /// `for`: `client`, `personal` or `both`.
    pub for_whom: Option<&'a str>,
    /// What changes (title, content, `replace`, `append`, paths, tags, where it goes).
    pub changes: &'a crate::memoria::Changes,
    /// The new data (`Some(None)`: removed), if changed.
    pub data: Option<Option<&'a serde_json::Value>>,
    /// The leaks permission (`allow_leaks`): lets through what looks like a false positive, never a
    /// path or a repository.
    pub allow_leaks: bool,
}

/// What to do with the trades in an `update`.
#[derive(Debug, Default)]
pub struct Plan {
    /// The tags that remain (`None`: unchanged).
    pub tags: Option<Vec<String>>,
    /// It has to be made global (it was a project's or a space's and becomes a trade one).
    pub make_global: bool,
    /// Goes from a client (a space, or a project in one) to a trade: it isn't moved, a new trade
    /// memory is created ([`create_trade_memory`]) and the client's stays where it was.
    pub create_new: bool,
}

/// What an `update` does with the trades: make it a trade one (promote it), stop being one, change who
/// it is for, or take it out of its trade when moving it to a project or a space. What is new in a trade
/// memory goes through `leaks` (everything that will remain if it enters a trade: text, paths, tags, key and
/// data), except what the leaks permission lets through: `Err` with them in `Ok(Err(…))`.
///
/// Promoting a personal memory (of a project without a space) moves it with its id, its history and its
/// verdicts: all of that belongs to the person. Promoting a client's creates a new memory (`create_new`):
/// its history and verdicts belong to the client and must not reach the others.
pub fn on_update(db: &Connection, current: &Memory, p: ChangeRequest) -> Result<std::result::Result<Plan, Vec<Leak>>> {
    let normalize = |v: &[String]| -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty()).collect();
        v.sort();
        v.dedup();
        v
    };
    let c = p.changes;
    let base = normalize(&c.tags.clone().unwrap_or_else(|| current.tags.clone()));
    let to_project = c.space_id.is_some() || c.project.as_deref().is_some_and(|d| !d.is_empty());
    let to_global = c.project.as_deref() == Some("");
    let was = of_memory(current);
    let drop_trade = p.trade.is_some_and(|t| t.trim().is_empty());
    let mut requested = match p.trade.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => list(t)?,
        None => Vec::new(),
    };
    for o in from_tags(&base) {
        let o = valid_name(&o)?;
        if !requested.contains(&o) {
            requested.push(o);
        }
    }
    let new_for = p.for_whom.map(parse_for_whom).transpose()?;
    let changed = |tags: Vec<String>| -> Option<Vec<String>> {
        let tags = normalize(&tags);
        (tags != normalize(&current.tags)).then_some(tags)
    };
    if drop_trade || to_project {
        return Ok(Ok(Plan { tags: changed(without_trade(base)), ..Plan::default() }));
    }
    let enters = !requested.is_empty() && (p.trade.is_some() || current.global() || to_global);
    if !enters && was.is_empty() {
        ensure!(new_for.is_none(), tr!("`for` only applies to a trade memory: give `trade` too", "`for` solo vale en una memoria de oficio: di también `trade`"));
        return Ok(Ok(Plan { tags: c.tags.as_ref().and_then(|_| changed(without_trade(base))), ..Plan::default() }));
    }
    let trades = if enters { requested } else { was.clone() };
    let final_for = new_for.unwrap_or_else(|| for_whom_of(&current.tags));
    let new_trade = was.is_empty() || trades.iter().any(|o| !was.contains(o));
    let text = if new_trade {
        // Enters a trade (or another one): everything that will remain, as it will remain (after `replace` and `append`).
        let (title, content) = crate::memoria::text_after(current, c)?;
        let paths = c.paths.clone().unwrap_or_else(|| current.paths.clone());
        let data = match p.data {
            Some(d) => d.cloned(),
            None => crate::memoria::data_of(db, &[current.id.as_str()])?.remove(&current.id),
        };
        leak_fields(&[("title", &title), ("content", &content), ("paths", &paths.join(" "))], &base, None, data.as_ref())
    } else {
        // It was already of this trade: what's new (what `replace` and `append` put, the title, the content and
        // the paths that come, the tags it didn't have and the new data).
        let mut parts: Vec<(&'static str, String)> = c.replace.iter().map(|(_, with)| ("content", with.clone())).collect();
        parts.extend(c.append.clone().map(|a| ("content", a)));
        parts.extend(c.title.clone().map(|t| ("title", t)));
        parts.extend(c.content.clone().map(|t| ("content", t)));
        parts.extend(c.paths.iter().flatten().cloned().map(|r| ("paths", r)));
        let had = normalize(&current.tags);
        let new_tags: Vec<String> = base.iter().filter(|t| !had.contains(t)).cloned().collect();
        leak_fields(&parts.iter().map(|(c, t)| (*c, t.as_str())).collect::<Vec<_>>(), &new_tags, None, p.data.flatten())
    };
    let f = blocking_leaks(leaks_in(db, &text)?, p.allow_leaks);
    if !f.is_empty() {
        return Ok(Err(f));
    }
    Ok(Ok(Plan {
        tags: changed(trade_tags(base, &trades, final_for.as_deref())),
        make_global: !current.global(),
        create_new: !current.global() && !current.space.is_empty(),
    }))
}

/// Promoting a client's memory to a trade: a NEW trade memory (another id, no history
/// or verdicts) with the text as it remains after the changes (`c`, which already went through `leaks`), its
/// paths, its kind, its data and the plan's tags (`tags`, with the trade). The client's isn't touched:
/// it stays in its project or its space, with its history, and sync keeps carrying it through
/// its client's repository (moving it to the general one left the copy on the client's other machines in
/// conflict, and its history was client text).
pub fn create_trade_memory(
    db: &mut Connection,
    current: &Memory,
    c: &crate::memoria::Changes,
    tags: Vec<String>,
    source: &str,
    model: Option<&crate::embeber::Model>,
) -> Result<Memory> {
    let (title, content) = crate::memoria::text_after(current, c)?;
    let data = crate::memoria::data_of(db, &[current.id.as_str()])?.remove(&current.id);
    let new = crate::memoria::NewMemory {
        project_id: Some(0),
        kind: Some(c.kind.unwrap_or(current.kind)),
        kind_text: if c.kind.is_none() { current.kind_text.clone() } else { None },
        title,
        content,
        paths: c.paths.clone().unwrap_or_else(|| current.paths.clone()),
        tags,
        pinned: c.pinned.unwrap_or(current.pinned),
        source: source.to_string(),
        ..Default::default()
    };
    let m = crate::memoria::save(db, new, model)?;
    if let Some(d) = data {
        crate::memoria::set_data(db, &m.id, Some(d))?;
    }
    Ok(m)
}

/// What the agent is told about the leaks.
pub const LEAKS_NOTICE: &str = "A trade memory reaches every project of that trade, of every client: it must hold only knowledge about the stack itself. This memory (its text, tags, key or data) names what looks like a project or a client (see trade_leaks: each says its `field`, so a key like `acme-route` is renamed, not the text rewritten). Rewrite it without them (generic names, no absolute paths, repository URLs or client domains), or save it in the project instead (without `trade`). Only if they are false positives (a library's name, public documentation), call again with allow_leaks=true; that never lets through an absolute path or a repository URL. (`force` only skips the duplicate check.)";

/// What the agent is told when the trade memory it just promoted says nearly the same
/// as another of its trade (`duplicate_of`).
pub const DUPLICATE_NOTICE: &str = "This trade memory says nearly the same as another one of its trade (duplicate_of), maybe promoted from another client: propose merging them to the user (`update` the one to keep with `absorb` and the other's id), or `forget` this one.";

/// What the agent is told when promoting a client's memory.
pub const PROMOTED_NOTICE: &str = "It came from a client's project, so a NEW trade memory was created (this id, with no history or verdicts): the client's memory stays where it was, with its history. If it now only repeats this one, `forget` it.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trade_names() {
        assert_eq!(valid_name(" Android ").unwrap(), "android");
        assert_eq!(valid_name("trade:web").unwrap(), "web");
        assert_eq!(valid_name("c++").unwrap(), "c++");
        assert!(valid_name("").is_err());
        assert!(valid_name("a b").is_err());
        assert!(valid_name("-x").is_err());
        assert_eq!(list("android, web android").unwrap(), ["android", "web"]);
    }

    /// A leak's field is the one that has it as a word: «QA» appears in the content, not in the title
    /// that says «qatar» (before, the first one containing it was taken).
    #[test]
    fn a_leaks_field_is_by_words() {
        let t = crate::comun::Temporal::nueva("oficios-campo");
        let db = crate::db::open(&t.join("o.db")).unwrap();
        crate::proyecto::create_space(&db, "QA").unwrap();
        let fields = vec![("title", "Vuelos a Qatar".to_string()), ("content", "Lo vimos en QA ayer.".to_string())];
        let f = leaks_in(&db, &fields).unwrap();
        let qa = f.iter().find(|x| x.kind == "space").unwrap();
        assert_eq!(qa.field, "content", "{f:?}");
    }

    #[test]
    fn whole_words() {
        assert!(appears("en balua el emulador", "balua"));
        assert!(!appears("en baluarte", "balua"));
        assert!(appears("ver social-publish-plugin/x", "social-publish-plugin"));
        assert!(appears("ñandú y acme", "acme"));
    }

    #[test]
    fn paths_and_domains() {
        assert!(personal_path(r"C:\Users\ana\acme\app"));
        assert!(personal_path("D:/Clientes/acme"));
        assert!(!personal_path(r"C:\Program Files\Android\Android Studio"));
        assert!(personal_path("/home/ana/acme"));
        assert!(!personal_path("/system/bin/sh"));
        assert!(!personal_path("app/src/main"));
        assert_eq!(domain("https://staging.acme-shop.com/login").as_deref(), Some("staging.acme-shop.com"));
        assert_eq!(domain("acme-shop.com").as_deref(), Some("acme-shop.com"));
        assert_eq!(domain("README.md"), None);
        assert_eq!(domain("build.gradle.kts"), None);
        assert_eq!(domain("v1.2.3"), None);
        assert_eq!(registrable("gitlab.acme.com"), "acme.com");
    }

    #[test]
    fn detects_by_files() {
        let t = crate::comun::Temporal::nueva("oficios-detecta");
        let root = t.to_path_buf();
        let write = |r: &str, c: &str| {
            let p = root.join(r);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        // A monorepo: Android in `app/`, an Astro website in `web/`, and what doesn't count.
        write("settings.gradle.kts", "rootProject.name = \"x\"\ninclude(\":app\")");
        write("app/build.gradle.kts", "plugins { id(\"com.android.application\") }");
        write("web/package.json", r#"{"dependencies":{"astro":"5.0.0"}}"#);
        write("node_modules/x/Cargo.toml", "");
        write(".hidden/go.mod", "");
        write("web/sub/deep/Cargo.toml", "");
        assert_eq!(detect(&root), ["android", "node", "web"]);
        // Gradle without Android: JVM.
        let t2 = crate::comun::Temporal::nueva("oficios-jvm");
        std::fs::write(t2.join("build.gradle"), "plugins { id 'java' }").unwrap();
        assert_eq!(detect(&t2), ["jvm"]);
        // A WordPress theme and a plugin.
        let t3 = crate::comun::Temporal::nueva("oficios-wp");
        std::fs::write(t3.join("style.css"), "/*\nTheme Name: Acme\n*/").unwrap();
        std::fs::write(t3.join("functions.php"), "<?php").unwrap();
        assert_eq!(detect(&t3), ["php", "wordpress"]);
        let t4 = crate::comun::Temporal::nueva("oficios-wp-plugin");
        std::fs::write(t4.join("mi-plugin.php"), "<?php\n/**\n * Plugin Name: Mi plugin\n */").unwrap();
        assert_eq!(detect(&t4), ["php", "wordpress"]);
        // React Native is not web.
        let t5 = crate::comun::Temporal::nueva("oficios-rn");
        std::fs::write(t5.join("package.json"), r#"{"dependencies":{"react":"18","react-native":"0.76"}}"#).unwrap();
        assert_eq!(detect(&t5), ["node", "react-native"]);
        // Electron with React: desktop, not web.
        let t7 = crate::comun::Temporal::nueva("oficios-electron");
        std::fs::write(t7.join("package.json"), r#"{"dependencies":{"react":"18"},"devDependencies":{"Electron":"30"}}"#).unwrap();
        std::fs::write(t7.join("vite.config.ts"), "").unwrap();
        assert_eq!(detect(&t7), ["escritorio-node", "node"]);
        // A Claude Code plugin in Rust (Omoikane), and an Alexa skill.
        let t8 = crate::comun::Temporal::nueva("oficios-plugin");
        std::fs::create_dir_all(t8.join(".claude-plugin")).unwrap();
        std::fs::write(t8.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
        std::fs::write(t8.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect(&t8), ["plugin-claude", "rust"]);
        let t9 = crate::comun::Temporal::nueva("oficios-alexa");
        std::fs::create_dir_all(t9.join("skill-package")).unwrap();
        std::fs::create_dir_all(t9.join("hooks")).unwrap();
        std::fs::write(t9.join("hooks").join("hooks.json"), "{}").unwrap();
        assert_eq!(detect(&t9), ["alexa", "plugin-claude"]);
        // Nothing to detect.
        let t6 = crate::comun::Temporal::nueva("oficios-nada");
        std::fs::write(t6.join("notas.md"), "x").unwrap();
        assert!(detect(&t6).is_empty());
    }

    #[test]
    fn with_many_subfolders_always_looks_at_the_same() {
        let t = crate::comun::Temporal::nueva("oficios-subcarpetas");
        // 70 subfolders: the first 60 in alphabetical order are looked at, created in whatever order.
        for i in (0..70).rev() {
            std::fs::create_dir_all(t.join(format!("m{i:02}"))).unwrap();
        }
        std::fs::write(t.join("m05").join("go.mod"), "").unwrap();
        std::fs::write(t.join("m65").join("Cargo.toml"), "").unwrap();
        for _ in 0..3 {
            assert_eq!(detect(&t), ["go"]);
        }
        let r = walk(&t);
        assert_eq!(r.folders.len(), 1 + MAX_SUBFOLDERS);
        assert_eq!(r.folders.last().unwrap().rel, "m59");
        assert_eq!(r.signature(), walk(&t).signature());
    }

    #[test]
    fn detected_is_cached_until_the_files_change() {
        let t = crate::comun::Temporal::nueva("oficios-guardado");
        // The database, outside the folder being looked at (its files change).
        let tb = crate::comun::Temporal::nueva("oficios-guardado-db");
        let db = crate::db::open(&tb.join("o.db")).unwrap();
        let gradle = t.join("build.gradle");
        std::fs::write(&gradle, "plugins { id 'com.android.application' }").unwrap();
        assert_eq!(detect_cached(&db, 7, &t), ["android"]);
        // What is stored rules as long as the folders and the files that are read don't change.
        let key = format!("{DETECTED_KEY}7");
        let v: String = db.query_row("SELECT v FROM meta WHERE k = ?1", [&key], |row| row.get(0)).unwrap();
        let (signature, _) = v.split_once('\t').unwrap();
        db.execute("UPDATE meta SET v = ?2 WHERE k = ?1", params![key, format!("{signature}\t[\"guardado\"]")]).unwrap();
        assert_eq!(detect_cached(&db, 7, &t), ["guardado"]);
        // The file changes (its size or date): it is read again.
        std::fs::write(&gradle, "plugins { id 'java' } // sin el otro").unwrap();
        assert_eq!(detect_cached(&db, 7, &t), ["jvm"]);
        // A new file too.
        std::fs::write(t.join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_cached(&db, 7, &t), ["jvm", "rust"]);
    }

    #[test]
    fn attributes_of_a_cloud_file() {
        assert!(cloud_attributes(0x0040_0000)); // RECALL_ON_DATA_ACCESS (OneDrive)
        assert!(cloud_attributes(0x0004_0000)); // RECALL_ON_OPEN
        assert!(cloud_attributes(0x0000_1000 | 0x20)); // OFFLINE
        assert!(!cloud_attributes(0x20 | 0x0040_0000 >> 1)); // ARCHIVE, PINNED (downloaded)
        assert!(!cloud_attributes(0));
    }

    /// A file marked as cloud-only isn't read (reading it would download it), and when it is downloaded
    /// the signature changes and it is read.
    #[cfg(windows)]
    #[test]
    fn does_not_read_what_is_in_the_cloud() {
        let t = crate::comun::Temporal::nueva("oficios-nube");
        // The database, outside the folder being looked at (its files change).
        let tb = crate::comun::Temporal::nueva("oficios-nube-db");
        let db = crate::db::open(&tb.join("o.db")).unwrap();
        let gradle = t.join("build.gradle");
        std::fs::write(&gradle, "plugins { id 'com.android.application' }").unwrap();
        std::fs::write(t.join("package.json"), r#"{"dependencies":{"astro":"5"}}"#).unwrap();
        let attrib = |sign: &str, p: &Path| {
            let ok = std::process::Command::new("attrib").arg(format!("{sign}O")).arg(p).status().unwrap().success();
            assert!(ok, "attrib {sign}O");
        };
        attrib("+", &gradle);
        attrib("+", &t.join("package.json"));
        // By name yes: `package.json` is Node; what it says inside, no.
        assert_eq!(detect_cached(&db, 3, &t), ["node"]);
        attrib("-", &gradle);
        attrib("-", &t.join("package.json"));
        assert_eq!(detect_cached(&db, 3, &t), ["android", "node", "web"]);
    }
}
