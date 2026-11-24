//! Risk history per file (1.6): when the message names a file that shows up in the project's diary
//! (`diario`), the recall for that message adds a line with how much it moves and what usually
//! changes with it, taken from this computer's sessions and not from what a model thinks:
//!
//! «History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04); it usually changes together
//! with `tests/sync.rs` (9 of 14) and `CHANGELOG.md` (7); 3 gotchas mention it.»
//!
//! Only with at least `MIN_SESSIONS` sessions (with fewer it says nothing), once per file and session
//! (local table `diary_hint`, separate from `recall_log`: it's not a memory and must not count as
//! received) and at most `MAX_POR_MENSAJE` files per message. The gotchas themselves already arrive
//! through `recall::by_paths`; here they are only counted.
//!
//! Cost: normally the message names no file, and that is decided without touching the database
//! (`parece_nombrar`). Only if it seems to does it check for a diary and read the project's lines.

use crate::memoria::{NOW, Scope};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};

/// Minimum sessions with the file to say anything: with fewer, the line is noise.
pub const SESIONES_MIN: usize = 3;
/// A file "changes together" if it matches in at least this many sessions…
pub const JUNTOS_MIN: usize = 3;
/// …and in at least this share of the file's sessions.
pub const JUNTOS_PARTE: f64 = 0.4;
/// How many of those that change together are named.
pub const JUNTOS_MAX: usize = 3;
/// How many files are talked about in a single message.
pub const MAX_POR_MENSAJE: usize = 2;
/// How many days it remembers it already said it (after compacting it forgets sooner: `on_session_start`).
const HINT_DAYS: i64 = 7;

/// The table with what was already said in each session. Local like `diary`, and like it, created with
/// the first line, not when opening the database.
const TABLE: &str = "
CREATE TABLE IF NOT EXISTS diary_hint (
  session TEXT NOT NULL,
  file    TEXT NOT NULL,
  at      TEXT NOT NULL,
  PRIMARY KEY (session, file)
);
";

/// Whether `path` shows up in `text` (already lowercase and with `/`): the whole path, or the file
/// name if it has at least 5 characters. The same rule as `recall::by_paths`.
pub fn nombra(text: &str, path: &str) -> bool {
    let path = path.to_lowercase().replace('\\', "/");
    let name = path.rsplit('/').next().unwrap_or(&path);
    text.contains(&path) || (name.chars().count() >= 5 && text.contains(name))
}

/// Whether the message may name a file: some word with a slash or with a dot between letters or
/// digits (`sync.rs`, `src/x`, `a\b`). Without looking at the database: so a normal message costs
/// nothing. Leaves out names without extension written alone (`Makefile`); with their folder
/// (`ci/Makefile`) they count.
pub fn parece_nombrar(prompt: &str) -> bool {
    let b = prompt.as_bytes();
    b.iter().enumerate().any(|(i, c)| match c {
        b'/' | b'\\' => true,
        b'.' => i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_alphanumeric() && b[i + 1].is_ascii_alphabetic(),
        _ => false,
    })
}

/// When the session starts or compacts: what was said before is no longer in its context, it can be
/// repeated. And whatever is older than `HINT_DAYS` days is deleted.
pub fn al_empezar(db: &Connection, session: &str) -> Result<()> {
    if !has_table(db)? {
        return Ok(());
    }
    db.execute(&format!("DELETE FROM diary_hint WHERE session = ?1 OR julianday({NOW}) - julianday(at) > {HINT_DAYS}"), [session])?;
    Ok(())
}

fn has_table(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'diary_hint')", [], |r| r.get(0))?)
}

/// What the diary knows about a file.
#[derive(Debug, PartialEq)]
pub struct Historia {
    pub fichero: String,
    pub sesiones: usize,
    /// The last day (UTC) of the most recent session that touched it.
    pub ultima: String,
    /// The ones that change together, with in how many of its sessions.
    pub juntos: Vec<(String, usize)>,
    pub gotchas: usize,
}

impl Historia {
    /// The line for the agent.
    pub fn linea(&self) -> String {
        let mut s = format!("History of `{}`: changed in {} sessions (last {})", self.fichero, self.sesiones, self.ultima);
        if !self.juntos.is_empty() {
            let parts: Vec<String> = self
                .juntos
                .iter()
                .enumerate()
                .map(|(i, (f, n))| if i == 0 { format!("`{f}` ({n} of {})", self.sesiones) } else { format!("`{f}` ({n})") })
                .collect();
            let list = match parts.as_slice() {
                [a] => a.clone(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
                [] => String::new(),
            };
            s.push_str(&format!("; it usually changes together with {list}"));
        }
        match self.gotchas {
            0 => {}
            1 => s.push_str("; 1 gotcha mentions it"),
            n => s.push_str(&format!("; {n} gotchas mention it")),
        }
        s.push('.');
        s
    }
}

/// The diary lines that belong to the folder `project`: those of its project and those without a
/// project recorded from one of its folders (or from this one, if it isn't a project yet or is a
/// general folder). Each one, its last moment and its files.
fn sessions(db: &Connection, project: &str) -> Result<Vec<(String, Vec<String>)>> {
    let general = crate::proyecto::es_general(project);
    let id = if general { 0 } else { crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0) };
    let mut folders: BTreeSet<String> = BTreeSet::from([crate::diario::carpeta_cmp(project)]);
    if id != 0 {
        let mut q = db.prepare("SELECT path FROM project_path WHERE project_id = ?1")?;
        for p in q.query_map([id], |r| r.get::<_, String>(0))? {
            folders.insert(crate::diario::carpeta_cmp(&p?));
        }
    }
    let mut q = db.prepare("SELECT project_id, folder, last_at, files FROM diary WHERE project_id IN (?1, 0)")?;
    let mut rows = q.query([id])?;
    let mut r = Vec::new();
    while let Some(row) = rows.next()? {
        let (pid, folder): (i64, String) = (row.get(0)?, row.get(1)?);
        if pid == 0 && !folders.contains(&crate::diario::carpeta_cmp(&folder)) {
            continue;
        }
        let files: Vec<String> = serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default();
        if !files.is_empty() {
            r.push((row.get(2)?, files));
        }
    }
    Ok(r)
}

/// Whether two paths are the same file: equal (ignoring case and slashes), or one ends in the other
/// at a folder boundary (`D:/repo/src/a.rs` and `src/a.rs`: memories keep the path as the agent gave it).
pub(crate) fn misma(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase().replace('\\', "/"), b.to_lowercase().replace('\\', "/"));
    let (long, short) = if a.len() >= b.len() { (&a, &b) } else { (&b, &a) };
    long == short || (!short.is_empty() && long.ends_with(short.as_str()) && long.as_bytes()[long.len() - short.len() - 1] == b'/')
}

/// The history of the files the message names (those above `MIN_SESSIONS`), without checking whether
/// it was already said. Those named with their whole path first; of those named only by name, for each
/// name the one that moved the most (`main.rs` isn't every `main.rs` in the repository).
pub fn historias(db: &Connection, project: &str, prompt: &str) -> Result<Vec<Historia>> {
    if project.is_empty() || !parece_nombrar(prompt) || !crate::diario::hay_tabla(db)? {
        return Ok(Vec::new());
    }
    let text = prompt.to_lowercase().replace('\\', "/");
    let lines = sessions(db, project)?;
    // How many sessions and the last one, per named file (`None`: seen and not named; checked once).
    let mut seen: HashMap<&str, Option<(usize, &str)>> = HashMap::new();
    for (last, files) in &lines {
        for f in files {
            if let Some((n, u)) = seen.entry(f.as_str()).or_insert_with(|| nombra(&text, f).then_some((0, ""))) {
                *n += 1;
                *u = (*u).max(last.as_str());
            }
        }
    }
    let mut chosen: Vec<(&str, usize, &str)> = seen.into_iter().filter_map(|(f, x)| x.filter(|(n, _)| *n >= SESIONES_MIN).map(|(n, u)| (f, n, u))).collect();
    let whole = |f: &str| text.contains(&f.to_lowercase());
    chosen.sort_by(|a, b| whole(b.0).cmp(&whole(a.0)).then(b.1.cmp(&a.1)).then(a.0.cmp(b.0)));
    // Named whole, the name is already taken: `src/recall.rs` doesn't also bring `tests/recall.rs`.
    let name = |f: &str| f.to_lowercase().rsplit('/').next().unwrap_or("").to_string();
    let mut names: BTreeSet<String> = chosen.iter().filter(|x| whole(x.0)).map(|x| name(x.0)).collect();
    chosen.retain(|(f, _, _)| whole(f) || names.insert(name(f)));
    chosen.truncate(MAX_POR_MENSAJE);
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    // The scope's gotchas with paths: few.
    let mut q = db.prepare(&format!(
        "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id WHERE m.kind = 'gotcha' AND {}",
        Scope::from_project(db, project)?.sql()
    ))?;
    let gotchas: Vec<(String, String)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut r = Vec::new();
    for (file, n, last) in chosen {
        let mut together: HashMap<&str, usize> = HashMap::new();
        for (_, files) in lines.iter().filter(|(_, fs)| fs.iter().any(|f| f == file)) {
            for f in files.iter().filter(|f| *f != file) {
                *together.entry(f).or_default() += 1;
            }
        }
        let mut together: Vec<(String, usize)> =
            together.into_iter().filter(|(_, k)| *k >= JUNTOS_MIN && *k as f64 >= JUNTOS_PARTE * n as f64).map(|(f, k)| (f.to_string(), k)).collect();
        together.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        together.truncate(JUNTOS_MAX);
        let ids: BTreeSet<&str> = gotchas.iter().filter(|(_, p)| misma(p, file)).map(|(id, _)| id.as_str()).collect();
        r.push(Historia { fichero: file.to_string(), sesiones: n, ultima: last.chars().take(10).collect(), juntos: together, gotchas: ids.len() });
    }
    Ok(r)
}

/// What the recall for this message adds (one line per file), or nothing. Records what was said so
/// as not to repeat it in the session.
pub fn al_preguntar(db: &Connection, project: &str, session: &str, prompt: &str) -> Result<Option<String>> {
    let mut hs = historias(db, project, prompt)?;
    if hs.is_empty() {
        return Ok(None);
    }
    if !session.is_empty() {
        if has_table(db)? {
            let mut q = db.prepare("SELECT 1 FROM diary_hint WHERE session = ?1 AND file = ?2")?;
            let mut fresh = Vec::new();
            for h in hs {
                if q.query_row(params![session, h.fichero], |_| Ok(())).optional()?.is_none() {
                    fresh.push(h);
                }
            }
            hs = fresh;
        }
        if hs.is_empty() {
            return Ok(None);
        }
        db.execute_batch(TABLE)?;
        for h in &hs {
            db.execute(&format!("INSERT OR IGNORE INTO diary_hint(session, file, at) VALUES (?1, ?2, {NOW})"), params![session, h.fichero])?;
        }
    }
    Ok(Some(hs.iter().map(Historia::linea).collect::<Vec<_>>().join("\n")))
}

// --- For other programs: `omoi file-history` -----------------------------------------------------
//
// The same as the recall line, but in full and for code: the risk gate of an agent hub (Kumi)
// computes its level over the diff with these numbers; no agent decides. No minimums and no file
// limit: the caller sets its own thresholds.

/// How many of those that change together are given per file.
pub const JUNTOS_INFORME: usize = 10;

/// A file that usually changes with another, and in how many sessions it did.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Junto {
    pub file: String,
    pub sessions: usize,
}

/// A file's history according to this computer's diary.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct HistoriaFichero {
    /// Relative to the repository, with `/` (absolute if it falls outside).
    pub file: String,
    /// In how many sessions it changed.
    pub sessions: usize,
    /// The last day (UTC) of the most recent session that touched it; `None` if none.
    pub last: Option<String>,
    /// Those that changed in the same sessions, most first (at most [`JUNTOS_INFORME`]).
    pub changes_with: Vec<Junto>,
    /// How many gotchas (of the project, its space and the global ones) name it in their paths, and which.
    pub gotchas: usize,
    pub gotcha_ids: Vec<String>,
}

/// The history of several files of a project.
#[derive(Debug, serde::Serialize)]
pub struct Informe {
    /// The project's folder.
    pub project: String,
    /// How many sessions with files this project's diary has: to put things in proportion.
    pub sessions: usize,
    pub files: Vec<HistoriaFichero>,
}

/// A path as the diary stores it: relative to the repository if it falls inside (also if it comes
/// absolute), with `/` and without a leading `./`.
fn path_key(f: &str, project: &str) -> String {
    let f = f.trim();
    let r = if std::path::Path::new(f).is_absolute() { crate::diario::relativo(f, project) } else { f.replace('\\', "/") };
    let mut r = r.as_str();
    while let Some(x) = r.strip_prefix("./") {
        r = x;
    }
    r.to_string()
}

/// The project's sessions (`sessions`) and, also, those opened in another folder (the user's, another
/// project) that touched files of this repository: those record them with the absolute path. Of them
/// only the files inside count, made relative; what's outside isn't this project's. Someone working
/// from their personal folder on several repositories would have no history without this.
fn sessions_with_outside(db: &Connection, project: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut r = sessions(db, project)?;
    if crate::proyecto::es_general(project) {
        return Ok(r);
    }
    let id = crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0);
    let folders: BTreeSet<String> = {
        let mut c = BTreeSet::from([crate::diario::carpeta_cmp(project)]);
        if id != 0 {
            let mut q = db.prepare("SELECT path FROM project_path WHERE project_id = ?1")?;
            for p in q.query_map([id], |r| r.get::<_, String>(0))? {
                c.insert(crate::diario::carpeta_cmp(&p?));
            }
        }
        c
    };
    // Those `sessions` already counted: this project's and those without a project from one of its folders.
    let mut q = db.prepare(r"SELECT project_id, folder, last_at, files FROM diary WHERE project_id <> ?1 AND files LIKE ?2 ESCAPE '\'")?;
    // The `LIKE` discards, without reading the JSON, those that don't name the folder (with `/`, as they are stored).
    let folder_path = project.replace('\\', "/");
    let pattern = format!("%{}/%", folder_path.trim_end_matches('/').replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_"));
    let mut rows = q.query(rusqlite::params![id, pattern])?;
    while let Some(row) = rows.next()? {
        let (pid, folder): (i64, String) = (row.get(0)?, row.get(1)?);
        if pid == 0 && folders.contains(&crate::diario::carpeta_cmp(&folder)) {
            continue;
        }
        let files: Vec<String> = serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default();
        let inside: Vec<String> = files
            .iter()
            .filter_map(|x| {
                let x = x.replace('\\', "/");
                let rel = crate::diario::relativo_texto(&x, &folder_path);
                (rel != x).then_some(rel)
            })
            .collect();
        if !inside.is_empty() {
            r.push((row.get(2)?, inside));
        }
    }
    Ok(r)
}

/// The history of `files` (paths relative to the repository or absolute) in the project of the folder
/// `project`, in the order asked and without repeats. Read-only: the diary is local, this computer's.
pub fn de_ficheros(db: &Connection, project: &str, files: &[String]) -> Result<Informe> {
    let mut keys: Vec<String> = Vec::new();
    for f in files.iter().filter(|f| !f.trim().is_empty()) {
        let k = path_key(f, project);
        if !keys.iter().any(|c| crate::proyecto::comparable(c) == crate::proyecto::comparable(&k)) {
            keys.push(k);
        }
    }
    let lines = if crate::diario::hay_tabla(db)? { sessions_with_outside(db, project)? } else { Vec::new() };
    // Each session with its files already comparable, once.
    let lines: Vec<(&str, Vec<(String, &str)>)> = lines
        .iter()
        .map(|(u, fs)| (u.as_str(), fs.iter().map(|f| (crate::proyecto::comparable(&f.replace('\\', "/")), f.as_str())).collect()))
        .collect();
    let gotchas: Vec<(String, String)> = if keys.is_empty() {
        Vec::new()
    } else {
        let mut q = db.prepare(&format!(
            "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id WHERE m.kind = 'gotcha' AND {}",
            Scope::from_project(db, project)?.sql()
        ))?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for k in keys {
        let kc = crate::proyecto::comparable(&k);
        let (mut n, mut last) = (0, "");
        let mut together: HashMap<&str, usize> = HashMap::new();
        for (u, fs) in lines.iter().filter(|(_, fs)| fs.iter().any(|(c, _)| *c == kc)) {
            n += 1;
            last = last.max(*u);
            for (_, f) in fs.iter().filter(|(c, _)| *c != kc) {
                *together.entry(*f).or_default() += 1;
            }
        }
        let mut together: Vec<Junto> = together.into_iter().map(|(f, s)| Junto { file: f.replace('\\', "/"), sessions: s }).collect();
        together.sort_by(|a, b| b.sessions.cmp(&a.sessions).then(a.file.cmp(&b.file)));
        together.truncate(JUNTOS_INFORME);
        let ids: BTreeSet<&str> = gotchas.iter().filter(|(_, p)| misma(p, &k)).map(|(id, _)| id.as_str()).collect();
        out.push(HistoriaFichero {
            sessions: n,
            last: (n > 0).then(|| last.chars().take(10).collect()),
            changes_with: together,
            gotchas: ids.len(),
            gotcha_ids: ids.into_iter().map(String::from).collect(),
            file: k,
        });
    }
    Ok(Informe { project: project.to_string(), sessions: lines.len(), files: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_a_file() {
        assert!(parece_nombrar("mira sync.rs"));
        assert!(parece_nombrar("el fallo de src/sync"));
        assert!(parece_nombrar(r"en D:\repo"));
        assert!(!parece_nombrar("como ha ido todo? bien. 1.5 o 2"));
        assert!(!parece_nombrar("termina. Ahora sigue"));
        assert!(!parece_nombrar(""));
    }

    #[test]
    fn same_paths() {
        assert!(misma("src/a.rs", "SRC/a.rs"));
        assert!(misma("D:/repo/src/a.rs", "src/a.rs"));
        assert!(misma(r"src\a.rs", "D:/repo/src/a.rs"));
        assert!(!misma("src/data.rs", "a.rs"));
        assert!(!misma("src/a.rs", "tests/a.rs"));
    }

    #[test]
    fn the_line() {
        let mut h = Historia {
            fichero: "src/sync.rs".into(),
            sesiones: 14,
            ultima: "2026-11-04".into(),
            juntos: vec![("tests/sync.rs".into(), 9), ("CHANGELOG.md".into(), 7)],
            gotchas: 3,
        };
        assert_eq!(
            h.linea(),
            "History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04); it usually changes together with `tests/sync.rs` (9 of 14) and `CHANGELOG.md` (7); 3 gotchas mention it."
        );
        h.juntos.push(("docs/x.md".into(), 6));
        h.gotchas = 1;
        assert!(h.linea().contains("(9 of 14), `CHANGELOG.md` (7) and `docs/x.md` (6); 1 gotcha mentions it."));
        h.juntos.clear();
        h.gotchas = 0;
        assert_eq!(h.linea(), "History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04).");
    }
}
