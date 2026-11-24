//! Provenance (1.8.5, from OntoSmith: every fact tied to its source): with each new memory we
//! record in which agent session and in which project commit it was written, so we can go back to
//! the source (the conversation, the code at that moment).
//!
//! It goes in `memory.extra` under the key [`KEY`], like the data and the verification: without
//! changing the schema, it travels as a `provenance` field of the line and a 1.0–1.8 keeps it without understanding it.
//! It is the one from when it was created: changing it later doesn't touch it (the history says when it changed).
//!
//! Cheap on purpose, because it goes on every `remember`: the session comes from the environment or
//! from the `session` table the hooks already fill, and the commit from reading `.git/HEAD` and its
//! ref (two or three small files), without launching `git`.
//!
//! The session, in order: `OMOIKANE_SESSION` (the caller says it), `CLAUDE_CODE_SESSION_ID` (Claude
//! Code puts it in the environment of what it launches: its shell and its MCP servers), and otherwise
//! the only session the hooks have recorded in that project in the last 12 hours. With several at
//! once in the same project we don't guess: better no session than another conversation's.
//!
//! The trace (1.9): if whoever launched the agent passed it `TRACEPARENT` (W3C Trace Context, the
//! OpenTelemetry standard; an agent hub sets it per step and Claude Code passes it down to its shell
//! and its MCP servers), its `trace_id` and `span_id` are recorded. With that, from the trace of a run
//! you reach the memories it left, and from a memory the exact step that wrote it. They are random
//! ids, with nothing from the project, so they also go on global and space memories.
//!
//! A trade memory carries no provenance: it belongs to every project of its trade and must not carry
//! anything from a client (not even its branch name). A global one, or one from a space without a
//! project, only the session: the commit and branch belong to the folder it was written in, which may
//! be a client's, and those memories are seen from others. Same when moving it later (promoting it to
//! a trade leaves it without provenance; to global, without commit or branch) and when reading or
//! exporting it ([`for_memory`]).

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The key in `memory.extra` (and the `provenance` field of the line that travels).
pub const KEY: &str = "provenance";


/// Where a memory came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The agent session id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// The commit (HEAD) of the project repository.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// The branch of that HEAD (empty with a detached HEAD).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    /// The `trace_id` from `TRACEPARENT` (32 hex), if the agent ran inside a trace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trace_id: String,
    /// The `span_id` from `TRACEPARENT` (16 hex): the step of that trace that launched the agent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub span_id: String,
}

impl Provenance {
    pub fn is_empty(&self) -> bool {
        self.session.is_empty() && self.commit.is_empty() && self.trace_id.is_empty()
    }

    /// On one line: «session 2bd891d2… · commit e564a66 (master)».
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if !self.session.is_empty() {
            parts.push(format!("session {}", self.session));
        }
        if !self.commit.is_empty() {
            let branch = if self.branch.is_empty() { String::new() } else { format!(" ({})", self.branch) };
            parts.push(format!("commit {}{branch}", self.commit.get(..12).unwrap_or(&self.commit)));
        }
        if !self.trace_id.is_empty() {
            parts.push(format!("trace {}/{}", self.trace_id, self.span_id));
        }
        parts.join(" · ")
    }
}

/// The one from its column in `memoria::COLUMNS` (the JSON of `extra.provenance`).
pub(crate) fn from_column(json: Option<&str>) -> Option<Provenance> {
    serde_json::from_str::<Provenance>(json?).ok().filter(|p| !p.is_empty())
}

/// The provenance of what is being saved now in the project folder `project` (the root, as given by
/// `proyecto::of_dir`; empty: global without a folder), or `None` if there is nothing to record.
pub fn current(db: &Connection, project: &str) -> Option<Provenance> {
    let session = session(db, project).unwrap_or_default();
    let (commit, branch) = if project.is_empty() { None } else { head_commit(Path::new(project)) }.unwrap_or_default();
    let (trace_id, span_id) = trace().unwrap_or_default();
    let p = Provenance { session, commit, branch, trace_id, span_id };
    (!p.is_empty()).then_some(p)
}

/// Only the agent session (no commit or branch), for what doesn't belong to the project of the folder
/// `project`: a global memory, one from a space without a project, or a trade one must not carry the
/// branch of the folder it was written in, which may be a client's (`feature/acme-…`) and would be
/// seen from any other.
pub fn session_only(db: &Connection, project: &str) -> Option<Provenance> {
    let (trace_id, span_id) = trace().unwrap_or_default();
    let p = Provenance { session: session(db, project).unwrap_or_default(), trace_id, span_id, ..Default::default() };
    (!p.is_empty()).then_some(p)
}

/// The one a memory may carry depending on where it is now: none if it is a trade one (it belongs to
/// every project of its trade), only the session if it isn't a project's (global or a space's), and
/// all of it if it is a project's. Works for what was saved before trimming it and for what is moved later.
pub(crate) fn for_memory(p: Option<Provenance>, project_id: i64, tags: &[String]) -> Option<Provenance> {
    let mut p = p?;
    if tags.iter().any(|t| t.starts_with(crate::oficios::PREFIX)) {
        return None;
    }
    if project_id == 0 {
        p.commit.clear();
        p.branch.clear();
    }
    (!p.is_empty()).then_some(p)
}

/// [`for_memory`] on an `extra` (the one of `memory.extra` or of an outgoing line): removes the key
/// or trims it. `true` if anything changed.
pub(crate) fn trim_extra(extra: &mut serde_json::Map<String, serde_json::Value>, project_id: i64, tags: &[String]) -> bool {
    // One we don't understand (from a future version with another shape) is left as it is.
    let Some(before) = extra.get(KEY).and_then(|v| serde_json::from_value::<Provenance>(v.clone()).ok()) else { return false };
    match for_memory(Some(before.clone()), project_id, tags) {
        Some(p) if p == before => false,
        Some(p) => {
            extra.insert(KEY.into(), serde_json::to_value(p).unwrap_or_default());
            true
        }
        None => {
            extra.remove(KEY);
            true
        }
    }
}

fn env_var(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty() && v.len() <= 200)
}

/// `trace_id` and `span_id` from `TRACEPARENT`, if valid: `vv-<32 hex>-<16 hex>-<2 hex>`, a version
/// other than `ff` (a future version may add fields after: the first four are read), ids that aren't
/// all zeros. A malformed one is ignored, as the standard says.
fn trace() -> Option<(String, String)> {
    parse_traceparent(&env_var("TRACEPARENT")?)
}

pub(crate) fn parse_traceparent(v: &str) -> Option<(String, String)> {
    let mut parts = v.split('-');
    let (version, trace, span, flags) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let zeros = |s: &str| s.bytes().all(|b| b == b'0');
    (hex(version, 2) && version != "ff" && (version != "00" || parts.next().is_none()) && hex(trace, 32) && hex(span, 16) && hex(flags, 2)
        && !zeros(trace) && !zeros(span))
        .then(|| (trace.to_string(), span.to_string()))
}

/// The session of the agent that saves (see the start of the module).
fn session(db: &Connection, project: &str) -> Option<String> {
    if let Some(s) = env_var("OMOIKANE_SESSION").or_else(|| env_var("CLAUDE_CODE_SESSION_ID")) {
        return Some(s);
    }
    if project.is_empty() {
        return None;
    }
    let mut q = db
        .prepare_cached(
            "SELECT session FROM session WHERE project = ?1 AND session != ''
               AND started_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-12 hours') LIMIT 2",
        )
        .ok()?;
    let v: Vec<String> = q.query_map(params![project], |row| row.get(0)).ok()?.filter_map(|x| x.ok()).collect();
    match v.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// The HEAD commit and its branch in the repository rooted at `root`, reading git's files: `.git`
/// (or, in a worktree, the `gitdir:` file and its `commondir`), `HEAD`, the loose ref or
/// `packed-refs`. `None` if it isn't a repository, has no commits yet or can't be understood (reftable).
pub fn head_commit(root: &Path) -> Option<(String, String)> {
    // Every file opened costs (on Windows, ~0.1 ms): the minimum. A normal repository: HEAD
    // and its branch; a worktree: also `.git` and `commondir`.
    // Local, or on the network share of the folder being worked in (`proyecto::readable`).
    if !crate::proyecto::readable(root) {
        return None;
    }
    let dot_git = root.join(".git");
    let (gitdir, common) = match std::fs::read_to_string(&dot_git) {
        // `.git` is a file: a worktree (or a submodule, without `commondir`). The common folder (the
        // main repository's) is where the branches are. Neither can be on another network share.
        Ok(t) => {
            let gitdir = crate::proyecto::if_readable(root.join(t.trim().strip_prefix("gitdir:")?.trim()))?;
            let common = std::fs::read_to_string(gitdir.join("commondir")).map(|c| gitdir.join(c.trim())).unwrap_or_else(|_| gitdir.clone());
            let common = crate::proyecto::if_readable(common)?;
            (gitdir, common)
        }
        Err(_) => (dot_git.clone(), dot_git),
    };
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return is_sha(head).then(|| (head.to_string(), String::new()));
    };
    let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference).to_string();
    // Branches, in the common one; a worktree's own refs (`refs/bisect`…), in its own.
    let dirs = if gitdir == common { vec![&common] } else { vec![&common, &gitdir] };
    for dir in dirs {
        if let Ok(s) = std::fs::read_to_string(dir.join(reference)) {
            let s = s.trim();
            if is_sha(s) {
                return Some((s.to_string(), branch));
            }
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter_map(|l| l.split_once(' '))
        .find(|(sha, r)| r.trim() == reference && is_sha(sha))
        .map(|(sha, _)| (sha.to_string(), branch))
}

/// SHA-1 (40) or SHA-256 (64) in hex.
fn is_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_detached_head_branch_and_packed() {
        let t = crate::comun::Temporal::nueva("procedencia");
        let root = t.join("repo");
        let git = root.join(".git");
        std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        assert_eq!(head_commit(&root), None, "sin HEAD");
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        // Branch with its file.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs").join("heads").join("main"), format!("{a}\n")).unwrap();
        assert_eq!(head_commit(&root), Some((a.clone(), "main".into())));
        // Branch only in packed-refs.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/vieja\n").unwrap();
        std::fs::write(git.join("packed-refs"), format!("# pack-refs with: peeled fully-peeled sorted\n{b} refs/heads/vieja\n")).unwrap();
        assert_eq!(head_commit(&root), Some((b.clone(), "vieja".into())));
        // Branch without commits.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/nueva\n").unwrap();
        assert_eq!(head_commit(&root), None);
        // Detached HEAD.
        std::fs::write(git.join("HEAD"), format!("{a}\n")).unwrap();
        assert_eq!(head_commit(&root), Some((a.clone(), String::new())));
        // A worktree: `.git` is a file, its own HEAD and the branches in the common folder.
        let wt = t.join("wt");
        let gitdir = git.join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(head_commit(&wt), Some((a, "main".into())));
        assert_eq!(head_commit(&t.join("no-existe")), None);
    }

    #[test]
    fn traceparent_valid_and_invalid() {
        let t = "4bf92f3577b34da6a3ce929d0e0e4736";
        let s = "00f067aa0ba902b7";
        assert_eq!(parse_traceparent(&format!("00-{t}-{s}-01")), Some((t.into(), s.into())));
        // A future version may bring fields after; version 00, no.
        assert_eq!(parse_traceparent(&format!("01-{t}-{s}-01-extra")), Some((t.into(), s.into())));
        assert_eq!(parse_traceparent(&format!("00-{t}-{s}-01-extra")), None);
        assert_eq!(parse_traceparent(&format!("ff-{t}-{s}-01")), None);
        assert_eq!(parse_traceparent(&format!("00-{}-{s}-01", "0".repeat(32))), None);
        assert_eq!(parse_traceparent(&format!("00-{t}-{}-01", "0".repeat(16))), None);
        assert_eq!(parse_traceparent(&format!("00-{}-{s}-01", t.to_uppercase())), None);
        assert_eq!(parse_traceparent(&format!("00-{t}-{s}")), None);
        assert_eq!(parse_traceparent("basura"), None);
    }

    #[test]
    fn trace_only_is_not_empty_and_shows() {
        let p = Provenance { trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(), span_id: "00f067aa0ba902b7".into(), ..Default::default() };
        assert!(!p.is_empty());
        assert_eq!(p.text(), "trace 4bf92f3577b34da6a3ce929d0e0e4736/00f067aa0ba902b7");
        // A global one keeps it (they are ids with nothing from the project); a trade one, no.
        assert_eq!(for_memory(Some(p.clone()), 0, &[]), Some(p.clone()));
        assert_eq!(for_memory(Some(p), 3, &[format!("{}android", crate::oficios::PREFIX)]), None);
    }

    #[test]
    fn short_text() {
        let p = Provenance { session: "s1".into(), commit: "0123456789abcdef0123456789abcdef01234567".into(), branch: "main".into(), ..Default::default() };
        assert_eq!(p.text(), "session s1 · commit 0123456789ab (main)");
        assert!(from_column(Some("{}")).is_none());
    }
}
