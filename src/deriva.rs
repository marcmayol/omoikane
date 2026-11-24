//! Code drift (1.8.5, the idea of `validate --drift` from OKF Agent Memory): a memory that talks
//! about some files (its `paths` and the ones it names in the text with their folder, `src/sync.rs`) can
//! go stale when those files change a lot after it was written. The review proposes it
//! as `code_drift`; it never changes the memory on its own. «It's fine as is» marks it reviewed today: it warns
//! again only if the files change enough again.
//!
//! What counts as a change, from cheapest to most expensive:
//! 1. The file's modification date: if none has changed since the memory was written or reviewed,
//!    there is nothing to look at (one `stat` per file). On its own it is not enough: on the same day the file
//!    that was just talked about gets touched, and that is not drift.
//! 2. The git history, if the file is in a repository: one `git log --numstat` command per
//!    repository, only with the files that changed, and in parallel. Drift: the lines changed
//!    since then are at least half of what they measure today (`MIN_SHARE`) and at least `MIN_LINES`.
//! 3. Outside git, the session diary (`diario`): drift if they were edited in `MIN_SESSIONS`
//!    different sessions afterwards (not counting the one that saved the memory).
//!
//! What changes in the `GRACE_HOURS` hours after writing it does not count: the normal thing is to save the memory
//! and then commit the work it talks about. A file that no longer exists is already reported by the review
//! (`broken_path`); here only what is still on disk is looked at.
//!
//! Calibrated with a copy of Marc's real memory from 8-Nov-2026 (225 memories; 66 with files
//! that exist, 23 through their `paths`). Counting commits does not separate them: with 5 or more, 9 came out, almost all from
//! touch-ups (7 commits and 27 lines out of 535; a catalogue regenerated in every version). The changed
//! share does: only one comes out, the note saying the stale-memory detector «is still not
//! incremental» (56 %; it has been since); behind it, the history of Omoikane's phases (42 %) and a
//! decision about an experiment (38 %), which still hold, and the rest below 22 %. The
//! diary is still short (one week) and flagged none: that computer's sessions work in other
//! folders, and it stays for those who don't use git.
//!
//! Cost: never in the per-message recall. What is computed is stored in `meta` (`id`, since when it is
//! looked at and the result) and is valid while the memory does not change: with that, the whole review (`omoi
//! review`, the `context` tool, the Review tab) takes one millisecond more, and the notice at
//! session start only reads what is stored, without git or disk. It is recomputed once a day by a
//! separate process (`omoi review --refresh-drift`): with 225 memories, about 0.6-0.9 s, almost all of it
//! git start-up in eight repositories. It is local: it does not travel with sync.

use crate::memoria::{Scope, Memory};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The kind of notice in the review.
pub const CODE_DRIFT: &str = "code_drift";
/// The share of what its files measure today that must have changed (lines added plus removed).
pub const MIN_SHARE: f64 = 0.5;
/// And at least this many lines: in a ten-line file, five lines say nothing.
pub const MIN_LINES: usize = 20;
/// Outside git: diary sessions that edited its files afterwards.
pub const MIN_SESSIONS: usize = 5;
/// Hours after writing (or reviewing) it during which what changes is still that session's work.
pub const GRACE_HOURS: i64 = 6;
/// How many files of a memory are looked at (its own first, then the ones it names).
const MAX_FILES: usize = 20;
/// Larger files are not counted by lines (a model, a binary).
const MAX_BYTES: u64 = 4 << 20;
/// How many git commands at once.
const THREADS: usize = 8;
/// The longest a `git log` is waited for: if it takes longer, its files are looked at as if they were not in git.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Where today's computation is stored: `day` and one `id\tsince\tdetail` line per memory.
const CACHE_KEY: &str = "deriva_codigo";
/// The day the background computation was launched from the session-start notice.
const LAUNCHED_KEY: &str = "deriva_codigo_lanzado";

/// Seconds since 1970 of an ISO date (`2026-10-30T18:45:00.570Z`), in UTC.
fn seconds(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (a, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13).unwrap_or(0), n(14..16).unwrap_or(0), n(17..19).unwrap_or(0));
    // Days since 1970-01-01 (Howard Hinnant).
    let y = if m <= 2 { a - 1 } else { a };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + se)
}

/// Whether a path is absolute on any system (`C:/…`, `C:\…`, `/…`).
fn is_absolute(r: &str) -> bool {
    let b = r.as_bytes();
    r.starts_with('/') || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
}

/// Where a memory's path is on this computer: absolute, `~/…` or relative to its project. Never a
/// network one (`\\server\…`, `//cdn…`): looking at it would open a connection (see `proyecto::ruta_de_red`).
fn on_disk(m: &Memory, path: &str) -> Option<PathBuf> {
    if crate::proyecto::ruta_de_red(path) {
        return None;
    }
    let d = if let Some(r) = path.strip_prefix("~/") {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|c| PathBuf::from(c).join(r))?
    } else if is_absolute(path) {
        PathBuf::from(path)
    } else if !m.project.is_empty() && !m.project.starts_with('[') {
        Path::new(&m.project).join(path)
    } else {
        return None;
    };
    crate::proyecto::ruta_local(&d).then_some(d)
}

/// The file paths a text names: words with a slash and an extension at the end
/// (`src/sync.rs`, `D:\repo\a.py`), without web addresses. Those without a folder (`sync.rs`) don't count:
/// a repository has many `main.rs`.
pub fn named_paths(text: &str) -> Vec<String> {
    let mut r: Vec<String> = Vec::new();
    for p in text.split(|c: char| c.is_whitespace() || "`'\"«»()[]{}<>,;|*".contains(c)) {
        let p = p.trim_end_matches(['.', ':', '!', '?']);
        // Neither web addresses nor network paths (`//cdn.example.com/x.js`, `\\server\share\a.py`).
        if crate::proyecto::ruta_de_red(p) || p.contains("://") || p.starts_with("www.") || !(p.contains('/') || p.contains('\\')) || p.ends_with(['/', '\\']) {
            continue;
        }
        let name = p.rsplit(['/', '\\']).next().unwrap_or("");
        let Some((base, ext)) = name.rsplit_once('.') else { continue };
        let ext_ok = (1..=6).contains(&ext.len()) && ext.starts_with(|c: char| c.is_ascii_alphabetic()) && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if !base.is_empty() && ext_ok && !r.iter().any(|x| x == p) {
            r.push(p.to_string());
        }
    }
    r
}

/// A memory's files that exist on this computer: its paths and the ones it names, without repeats.
/// (what it shows, path on disk)
fn files(m: &Memory) -> Vec<(String, PathBuf)> {
    let mut r: Vec<(String, PathBuf)> = Vec::new();
    let mut seen = BTreeSet::new();
    let named = named_paths(&m.content);
    for path in m.paths.iter().chain(&named) {
        if r.len() >= MAX_FILES {
            break;
        }
        if let Some(d) = on_disk(m, path)
            && d.is_file()
            && seen.insert(crate::proyecto::comparable(&d.to_string_lossy().replace('\\', "/")))
        {
            r.push((path.replace('\\', "/"), d));
        }
    }
    r
}

fn modified(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// The folder of the git repository that contains `p` (the one with `.git`, a folder or a worktree's
/// file), looking at each folder once.
fn repo_of(p: &Path, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    let mut walked = Vec::new();
    let mut d = p.parent();
    let mut found = None;
    while let Some(x) = d {
        if let Some(r) = cache.get(x) {
            found = r.clone();
            break;
        }
        walked.push(x.to_path_buf());
        if x.join(".git").exists() {
            found = Some(x.to_path_buf());
            break;
        }
        d = x.parent();
    }
    for c in walked {
        cache.insert(c, found.clone());
    }
    found
}

/// A git change to a file: (commit, when, lines added plus removed).
type Change = (String, i64, usize);
/// A repository's changes per file (comparable path).
type Log = HashMap<String, Vec<Change>>;
/// What is stored: `id -> (since, detail)`.
type Saved = HashMap<String, (String, String)>;
/// From the diary, per file: (session, last moment, ids saved in it).
type ByFile = HashMap<String, Vec<(String, String, Vec<String>)>>;

/// The changes of `rel` (paths relative to the repository, with `/`) since `since` (seconds), by comparable
/// path. `None` if git is not there or fails: then those files are looked at as if they were not in git.
fn git_log(repo: &Path, since: i64, rel: &[String]) -> Option<Log> {
    let mut c = std::process::Command::new("git");
    // One day of margin in `--since` (which interprets dates its own way); the exact cut comes later.
    c.args(["-c", "core.quotepath=off", "-C"])
        .arg(repo)
        .args(["log", "--no-renames", "--no-merges", "--format=%x00%H %ct", "--numstat"])
        .arg(format!("--since={}", since - 86400))
        .arg("--");
    let magic = if cfg!(any(windows, target_os = "macos")) { ":(literal,icase)" } else { ":(literal)" };
    for r in rel {
        c.arg(format!("{magic}{r}"));
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let output = with_timeout(c, GIT_TIMEOUT)?;
    let mut r: Log = HashMap::new();
    for block in String::from_utf8_lossy(&output).split('\0').skip(1) {
        let mut lines = block.lines();
        let Some((hash, when)) = lines.next().and_then(|l| l.split_once(' ')) else { continue };
        let Ok(when) = when.trim().parse::<i64>() else { continue };
        for l in lines {
            let mut p = l.splitn(3, '\t');
            let (Some(a), Some(q), Some(path)) = (p.next(), p.next(), p.next()) else { continue };
            let n = a.parse::<usize>().unwrap_or(0) + q.parse::<usize>().unwrap_or(0);
            r.entry(crate::proyecto::comparable(path)).or_default().push((hash.to_string(), when, n));
        }
    }
    Some(r)
}

/// What a command writes (with its output already redirected to a pipe) if it finishes successfully before `limit`;
/// if it takes longer, it is stopped (that process, ours, through its handle) and `None`: a huge repository, a
/// network drive or a hung `git` do not leave the review waiting.
fn with_timeout(mut c: std::process::Command, limit: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = c.spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    // The output is read separately: if nobody reads it, git keeps waiting with the pipe full.
    let reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = pipe.read_to_end(&mut v);
        v
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(e)) => break Some(e),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // Without success in time, the reader thread is let go without waiting for it: a grandchild that inherited the pipe (the
    // real `git` under Git for Windows' launcher) keeps it open until it finishes, and
    // `join` waited for that grandchild even though the child had already been stopped (15 s with a 0.5 limit).
    // The thread ends on its own when the grandchild closes the pipe.
    if !status.is_some_and(|e| e.success()) {
        return None;
    }
    // And even if the child finishes successfully, the output is only waited for up to the limit, for the same reason.
    while !reader.is_finished() {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    reader.join().ok()
}

/// How many lines a file has today (0 if it is very large or cannot be read).
fn line_count(p: &Path) -> usize {
    if std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return 0;
    }
    match std::fs::read(p) {
        Ok(b) if !b.is_empty() => b.iter().filter(|c| **c == b'\n').count() + usize::from(*b.last().unwrap_or(&b'\n') != b'\n'),
        _ => 0,
    }
}

/// What is needed from a memory: its id, since when it is looked at and, if it has to be computed, the memory itself.
struct Pending {
    id: String,
    since: String,
    m: Memory,
    files: Vec<(String, PathBuf)>,
}

/// The memory whose paths are looked at and since when: its last change or the last «it's fine as is» of this
/// notice, whichever is newer.
fn since(m_updated: &str, ok: Option<&String>) -> String {
    match ok {
        Some(at) if at.as_str() > m_updated => at.clone(),
        _ => m_updated.to_string(),
    }
}

/// What is stored (`raw`) as the day and `id → (since, detail)`.
fn parse_saved(v: Option<&str>) -> (String, Saved) {
    let mut r = HashMap::new();
    let Some(v) = v else { return (String::new(), r) };
    let mut ls = v.lines();
    let day = ls.next().unwrap_or("").to_string();
    for l in ls {
        let mut p = l.splitn(3, '\t');
        if let (Some(id), Some(d), Some(det)) = (p.next(), p.next(), p.next()) {
            r.insert(id.to_string(), (d.to_string(), det.to_string()));
        }
    }
    (day, r)
}

fn save_cache(db: &Connection, today: &str, c: &Saved) -> Result<()> {
    let mut v = today.to_string();
    let mut ids: Vec<&String> = c.keys().collect();
    ids.sort();
    for id in ids {
        let (d, det) = &c[id];
        v.push('\n');
        v.push_str(&format!("{id}\t{d}\t{}", det.replace(['\n', '\t'], " ")));
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [CACHE_KEY, &v])?;
    Ok(())
}

fn today(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT date('now', 'localtime')", [], |f| f.get(0))?)
}

/// For the session-start notice, which has to be fast: only what is already computed for the memories
/// that have not changed since then (without reading their content, without git or disk). If what is stored is from
/// another day, everything is recomputed in the background and the next session will see it. If it has never been computed,
/// nothing: the first review (`omoi review`, `context`, the Review tab) computes it.
pub fn for_scope(db: &Connection, scope: &Scope) -> Result<HashMap<String, String>> {
    let mut q = db.prepare(&format!("SELECT m.id, m.updated_at FROM memory m WHERE {}", scope.sql()))?;
    let rows: Vec<(String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let (r, day) = from_saved(db, raw(db)?.as_deref(), rows.iter().map(|(id, u)| (id.as_str(), u.as_str())), |_| {})?;
    refresh_in_background(db, &day)?;
    Ok(r)
}

/// What is stored (`saved`, from [`raw`]) for `rows` (`id`, last change) whose «since» has not
/// changed, and which day it is from. Those with nothing stored go to `missing`. Without copying anything that
/// is not returned: with 50,000 memories, copying each id and each date to look them up was almost all the time.
fn from_saved<'a>(
    db: &Connection,
    saved: Option<&str>,
    rows: impl Iterator<Item = (&'a str, &'a str)>,
    mut missing: impl FnMut(&'a str),
) -> Result<(HashMap<String, String>, String)> {
    let (day, cache) = parse_saved_borrowed(saved);
    let oks = crate::veredictos::oks(db, false)?;
    // The «it's fine as is» of this notice for each memory (what `since` looks at).
    let ok: HashMap<&str, &str> = oks.iter().filter(|((_, notice), _)| notice == CODE_DRIFT).map(|((id, _), at)| (id.as_str(), at.as_str())).collect();
    let mut r = HashMap::new();
    for (id, updated) in rows {
        let d = match ok.get(id) {
            Some(at) if *at > updated => *at,
            _ => updated,
        };
        match cache.get(id) {
            Some((dc, det)) if *dc == d => {
                if !det.is_empty() {
                    r.insert(id.to_string(), det.to_string());
                }
            }
            _ => missing(id),
        }
    }
    Ok((r, day))
}

/// [`parse_saved`] without copying: `id → (since, detail)` borrowed from `v`.
fn parse_saved_borrowed(v: Option<&str>) -> (String, HashMap<&str, (&str, &str)>) {
    let mut r = HashMap::new();
    let Some(v) = v else { return (String::new(), r) };
    let mut ls = v.lines();
    let day = ls.next().unwrap_or("").to_string();
    for l in ls {
        let mut p = l.splitn(3, '\t');
        if let (Some(id), Some(d), Some(det)) = (p.next(), p.next(), p.next()) {
            r.insert(id, (d, det));
        }
    }
    (day, r)
}

/// If what is stored is from another day (`day`), launches `omoi review --refresh-drift` on this database, once
/// a day and only if the running program is `omoi` (from a test, the executable is the test one and
/// would understand something else).
fn refresh_in_background(db: &Connection, day: &str) -> Result<()> {
    let today = today(db)?;
    if day.is_empty() || day == today {
        return Ok(());
    }
    launch_today(db, &today)
}

/// Launches `omoi review --refresh-drift` separately, if it has not already been launched today.
fn launch_today(db: &Connection, today: &str) -> Result<()> {
    let launched: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [LAUNCHED_KEY], |f| f.get(0)).optional()?;
    if launched.as_deref() == Some(today) {
        return Ok(());
    }
    let is_omoi = std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| s.to_string_lossy().to_lowercase())).is_some_and(|s| s == "omoi");
    if let (true, Some(path)) = (is_omoi, db.path().filter(|p| !p.is_empty())) {
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [LAUNCHED_KEY, today])?;
        crate::sync::lanzar(&["review", "--refresh-drift"], Some(Path::new(path)));
    }
    Ok(())
}

/// Recomputes the drift of every memory and stores it for today (what runs in the background).
pub fn refresh(db: &Connection) -> Result<usize> {
    let all = crate::memoria::recent(db, &Scope::All, None, usize::MAX >> 1)?;
    let oks = crate::veredictos::oks(db, false)?;
    let r = compute(db, all.iter().map(|m| Pending::new(m, &oks)).collect())?;
    save_cache(db, &today(db)?, &r)?;
    Ok(r.values().filter(|(_, det)| !det.is_empty()).count())
}

/// The memories with code drift among `memories`: `id → detail`. It is what the whole review
/// uses. What is stored is valid while the memory does not change, even if it is from another day: then it is left
/// recomputing in the background. Only what has nothing stored (new memories, changed ones or
/// ones marked as fine, or all of them the first time) is computed here, and almost always the date of their
/// files is enough.
pub fn for_memories(db: &Connection, memories: &[Memory]) -> Result<HashMap<String, String>> {
    let by_id: HashMap<&str, &Memory> = memories.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut missing = Vec::new();
    // What is stored, exactly as read: it is overwritten only if nobody has changed it meanwhile (see below).
    let raw_before = raw(db)?;
    let (mut r, day) = from_saved(db, raw_before.as_deref(), memories.iter().map(|m| (m.id.as_str(), m.updated_at.as_str())), |id| missing.push(id.to_string()))?;
    refresh_in_background(db, &day)?;
    if missing.is_empty() {
        return Ok(r);
    }
    let oks = crate::veredictos::oks(db, false)?;
    let pending = missing.iter().filter_map(|id| by_id.get(id.as_str())).map(|m| Pending::new(m, &oks)).collect();
    let fresh = compute(db, pending)?;
    let (_, mut cache) = parse_saved(raw_before.as_deref());
    for (id, (d, det)) in fresh {
        if !det.is_empty() {
            r.insert(id.clone(), det.clone());
        }
        cache.insert(id, (d, det));
    }
    // The new results are recorded with the day of what was there (or today, if there was nothing): that way the old ones keep
    // counting as old until everything is recomputed.
    // Compare-and-set: while computing, `refresh` (in another process) may have stored a newer full
    // result; overwriting it with the old one plus this would lose it. If what is stored is no longer
    // what was read, nothing is written (what was computed is valid for this response, and the next one will
    // recompute it if needed). It is a cache: if it cannot be written, nothing happens.
    let today = today(db)?;
    let _ = save_if_unchanged(db, raw_before.as_deref(), if day.is_empty() { &today } else { &day }, &cache);
    Ok(r)
}

/// Like [`for_memories`], for whoever cannot wait (the `context` tool, at session start): if it has
/// never been computed yet (all of it, with git, can take seconds), it is not computed here: it is launched
/// separately and the next one will see it. If something is already stored, whatever is missing (new or changed memories,
/// almost always just the date of their files), like [`for_memories`].
pub fn for_memories_no_wait(db: &Connection, memories: &[Memory]) -> Result<HashMap<String, String>> {
    if raw(db)?.is_none() {
        launch_today(db, &today(db)?)?;
        return Ok(HashMap::new());
    }
    for_memories(db, memories)
}

/// What is stored in `meta`, as is.
fn raw(db: &Connection) -> Result<Option<String>> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = ?1", [CACHE_KEY], |f| f.get(0)).optional()?)
}

/// Saves the cache only if what is stored is still `before`, in an `IMMEDIATE` transaction (nobody
/// writes between checking and writing). `false` if it had changed.
fn save_if_unchanged(db: &Connection, before: Option<&str>, day: &str, c: &Saved) -> Result<bool> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    if raw(&tx)?.as_deref() != before {
        return Ok(false);
    }
    save_cache(&tx, day, c)?;
    tx.commit()?;
    Ok(true)
}

impl Pending {
    fn new(m: &Memory, oks: &HashMap<(String, String), String>) -> Pending {
        Pending { id: m.id.clone(), since: since(&m.updated_at, oks.get(&(m.id.clone(), CODE_DRIFT.to_string()))), m: m.clone(), files: Vec::new() }
    }
}

/// `id → (since, detail)` for each pending one (empty detail if it does not drift).
fn compute(db: &Connection, mut pending: Vec<Pending>) -> Result<Saved> {
    let details = if pending.is_empty() { HashMap::new() } else { compute_pending(db, &mut pending)? };
    Ok(pending.into_iter().map(|p| (p.id.clone(), (p.since, details.get(&p.id).cloned().unwrap_or_default()))).collect())
}

/// The detail of the ones that drift among `pending`.
fn compute_pending(db: &Connection, pending: &mut [Pending]) -> Result<HashMap<String, String>> {
    // The ones with some file changed since then (plus the grace period); the others have nothing.
    let mut repos: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    // repo → (oldest since, relative paths)
    let mut by_repo: HashMap<PathBuf, (i64, BTreeSet<String>)> = HashMap::new();
    let mut outside_git = false;
    let mut alive: Vec<usize> = Vec::new();
    for (i, p) in pending.iter_mut().enumerate() {
        let Some(cutoff) = seconds(&p.since) else { continue };
        let cutoff = cutoff + GRACE_HOURS * 3600;
        p.files = files(&p.m);
        let changed: Vec<&PathBuf> = p.files.iter().map(|(_, d)| d).filter(|d| modified(d).is_some_and(|t| t > cutoff)).collect();
        if changed.is_empty() {
            continue;
        }
        alive.push(i);
        for d in changed {
            match repo_of(d, &mut repos) {
                Some(repo) => {
                    let rel = d.strip_prefix(&repo).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                    if !rel.is_empty() {
                        let e = by_repo.entry(repo).or_insert((cutoff, BTreeSet::new()));
                        e.0 = e.0.min(cutoff);
                        e.1.insert(rel);
                    }
                }
                None => outside_git = true,
            }
        }
    }
    if alive.is_empty() {
        return Ok(HashMap::new());
    }
    // One command per repository, in parallel.
    let jobs: Vec<(PathBuf, i64, Vec<String>)> = by_repo.into_iter().map(|(r, (d, rel))| (r, d, rel.into_iter().collect())).collect();
    let mut logs: HashMap<PathBuf, Option<Log>> = HashMap::new();
    for batch in jobs.chunks(THREADS) {
        let done: Vec<(PathBuf, Option<Log>)> = std::thread::scope(|s| {
            let hs: Vec<_> = batch.iter().map(|(r, d, rel)| s.spawn(move || (r.clone(), git_log(r, *d, rel)))).collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        logs.extend(done);
    }
    // The diary, only if some changed file is not in git (or git did not respond).
    let outside_git = outside_git || logs.values().any(Option::is_none);
    let diary = if outside_git { sessions(db)? } else { HashMap::new() };

    let mut r = HashMap::new();
    for i in alive {
        let p = &pending[i];
        let cutoff = seconds(&p.since).unwrap_or(0) + GRACE_HOURS * 3600;
        let (mut commits, mut changed, mut total) = (BTreeSet::new(), 0usize, 0usize);
        let mut its_sessions: BTreeSet<&str> = BTreeSet::new();
        let mut names: Vec<&str> = Vec::new();
        for (shown, d) in &p.files {
            // `Some(Some(..))`: its repository responded; `Some(None)`: git failed; `None`: either it is not in git
            // or nothing worth looking at changed in its repository.
            let repo = repo_of(d, &mut repos);
            let log = repo.as_ref().and_then(|repo| {
                let rel = d.strip_prefix(repo).ok()?.to_string_lossy().replace('\\', "/");
                logs.get(repo).map(|l| l.as_ref().map(|l| l.get(&crate::proyecto::comparable(&rel)).cloned().unwrap_or_default()))
            });
            match log {
                Some(Some(changes)) => {
                    total += line_count(d);
                    let mut its_own = false;
                    for (h, _, n) in changes.iter().filter(|(_, t, _)| *t > cutoff) {
                        commits.insert(h.clone());
                        changed += n;
                        its_own = true;
                    }
                    if its_own {
                        names.push(shown);
                    }
                }
                // In git but without changes since then (it was not among the changed ones): it still counts towards the size.
                None if repo.is_some() => total += line_count(d),
                _ => {
                    let key = crate::proyecto::comparable(&d.to_string_lossy().replace('\\', "/"));
                    let mut its_own = false;
                    for (session, last, saved) in diary.get(&key).map(Vec::as_slice).unwrap_or_default() {
                        if seconds(last).is_some_and(|t| t > cutoff) && !saved.contains(&p.id) {
                            its_sessions.insert(session);
                            its_own = true;
                        }
                    }
                    if its_own {
                        names.push(shown);
                    }
                }
            }
        }
        let share = changed as f64 / total.max(1) as f64;
        let by_git = changed >= MIN_LINES && share >= MIN_SHARE;
        let by_diary = its_sessions.len() >= MIN_SESSIONS;
        if !(by_git || by_diary) {
            continue;
        }
        let list = match names.as_slice() {
            [] => String::new(),
            [a] => format!("`{a}`"),
            [a, b] => format!("`{a}` and `{b}`"),
            [a, b, rest @ ..] => format!("`{a}`, `{b}` and {} more", rest.len()),
        };
        let day = p.since.get(..10).unwrap_or("");
        let mut figures = Vec::new();
        if by_git {
            let n = commits.len();
            figures.push(format!("{n} commit{}, {changed} lines changed, {:.0}% of their size", if n == 1 { "" } else { "s" }, share * 100.0));
        }
        if by_diary || !its_sessions.is_empty() {
            figures.push(format!("edited in {} sessions", its_sessions.len()));
        }
        r.insert(p.id.clone(), format!("{list} changed a lot since {day} ({}): is it still true?", figures.join("; ")));
    }
    Ok(r)
}

/// The diary sessions per file (comparable absolute path): (session, last moment, ids
/// saved in it). Relative files are relative to the project folder of the line.
fn sessions(db: &Connection) -> Result<ByFile> {
    let mut r: ByFile = HashMap::new();
    if !crate::diario::hay_tabla(db)? {
        return Ok(r);
    }
    let mut q = db.prepare(
        "SELECT d.session, COALESCE(NULLIF((SELECT path FROM project WHERE id = d.project_id), ''), d.folder), d.last_at, d.files, d.saved
         FROM diary d WHERE d.files <> '[]'",
    )?;
    let mut rows = q.query([])?;
    while let Some(f) = rows.next()? {
        let (session, folder, last): (String, String, String) = (f.get(0)?, f.get(1)?, f.get(2)?);
        let files: Vec<String> = serde_json::from_str(&f.get::<_, String>(3)?).unwrap_or_default();
        let saved: Vec<String> = serde_json::from_str(&f.get::<_, String>(4)?).unwrap_or_default();
        for x in files {
            let abs = if is_absolute(&x) { x.replace('\\', "/") } else { format!("{}/{}", folder.replace('\\', "/").trim_end_matches('/'), x.replace('\\', "/")) };
            r.entry(crate::proyecto::comparable(&abs)).or_default().push((session.clone(), last.clone(), saved.clone()));
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_paths_finds_paths() {
        let t = "Está en `src/sync.rs` y en D:\\repo\\a.py, no en main.rs ni en https://x.com/a.html ni en src/; también (tests/x.rs).";
        assert_eq!(named_paths(t), vec!["src/sync.rs", "D:\\repo\\a.py", "tests/x.rs"]);
    }

    /// Network paths are not taken as files nor looked at on disk (on Windows, looking at them opens an
    /// SMB connection: a hang and the NTLM hash sent to whatever server the memory says).
    #[test]
    fn never_network_paths() {
        let t = "Carga //cdn.example.com/x.js y \\\\host\\share\\a.py, o \\\\?\\C:\\x\\b.rs y \\\\.\\pipe\\c.rs; pero sí src/d.rs.";
        assert_eq!(named_paths(t), vec!["src/d.rs"]);
        let m = Memory { project: "/p".into(), ..Default::default() };
        for r in ["//cdn.example.com/x.js", "\\\\host\\share\\a.py", "\\\\?\\C:\\x\\b.rs", "\\\\.\\pipe\\c.rs", "/\\host\\x.rs"] {
            assert_eq!(on_disk(&m, r), None, "{r}");
        }
        // Relative to a project that is on a network share: not either.
        let remote = Memory { project: "\\\\host\\share\\repo".into(), ..Default::default() };
        assert_eq!(on_disk(&remote, "src/a.rs"), None);
        assert!(on_disk(&m, "src/a.rs").is_some());
    }

    /// A `git log` that does not finish is stopped when it reaches the limit, without waiting for it to end.
    #[test]
    fn git_with_timeout() {
        let mut c = if cfg!(windows) {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 20"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("20");
            c
        };
        c.stdout(std::process::Stdio::piped()).stdin(std::process::Stdio::null());
        let t = std::time::Instant::now();
        assert_eq!(with_timeout(c, std::time::Duration::from_millis(300)), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    /// A child that leaves a grandchild with the pipe open (like Git for Windows' launcher, whose real `git`
    /// inherits it) does not make us wait for that grandchild: neither if the child goes over the limit and is stopped, nor if
    /// it finishes right away.
    #[test]
    fn the_grandchild_does_not_make_us_wait() {
        // (what it hangs on, how long the child takes to exit)
        let commands: Vec<Vec<&str>> = if cfg!(windows) {
            vec![
                vec!["cmd", "/C", "start /b powershell -NoProfile -Command Start-Sleep -Seconds 15 & powershell -NoProfile -Command Start-Sleep -Seconds 20"],
                vec!["cmd", "/C", "start /b powershell -NoProfile -Command Start-Sleep -Seconds 15 & exit 0"],
            ]
        } else {
            vec![vec!["sh", "-c", "sleep 15 & sleep 20"], vec!["sh", "-c", "sleep 15 & exit 0"]]
        };
        for o in commands {
            let mut c = std::process::Command::new(o[0]);
            c.args(&o[1..]).stdout(std::process::Stdio::piped()).stdin(std::process::Stdio::null());
            let t = std::time::Instant::now();
            assert_eq!(with_timeout(c, std::time::Duration::from_millis(2500)), None, "{o:?}");
            assert!(t.elapsed() < std::time::Duration::from_secs(8), "{o:?}: {:?}", t.elapsed());
        }
    }

    /// What is computed does not overwrite what another process stored meanwhile (`refresh`): it is only written if what is
    /// stored is still what was read.
    #[test]
    fn the_cache_does_not_overwrite_newer_data() {
        let t = crate::comun::Temporal::nueva("deriva-cache");
        let db = crate::db::abrir(&t.join("o.db")).unwrap();
        let mut old = Saved::new();
        old.insert("A".into(), ("2026-01-01".into(), String::new()));
        save_cache(&db, "2026-11-01", &old).unwrap();
        let read = raw(&db).unwrap();
        // Meanwhile, `refresh` stores today's.
        let mut new = Saved::new();
        new.insert("A".into(), ("2026-01-01".into(), "cambió mucho".into()));
        save_cache(&db, "2026-11-08", &new).unwrap();
        let now = raw(&db).unwrap();
        assert!(!save_if_unchanged(&db, read.as_deref(), "2026-11-01", &old).unwrap());
        assert_eq!(raw(&db).unwrap(), now, "lo más nuevo se queda");
        assert!(save_if_unchanged(&db, now.as_deref(), "2026-11-08", &old).unwrap());
        // And `context` does not compute it the first time: with nothing stored, it does not wait.
        let empty = crate::db::abrir(&t.join("v.db")).unwrap();
        assert!(for_memories_no_wait(&empty, &[]).unwrap().is_empty());
        assert_eq!(raw(&empty).unwrap(), None);
    }

    #[test]
    fn dates_in_seconds() {
        assert_eq!(seconds("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(seconds("2026-10-30T18:45:00.570Z").unwrap() - seconds("2026-10-30T12:45:00Z").unwrap(), 6 * 3600);
    }
}
