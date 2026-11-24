//! The session diary (1.4, the episodic layer): one line per session and project with when it was, which
//! files were touched, which memories reached it and which were saved or changed in it. It does not store the
//! conversation. It answers «what did I do on Tuesday in X?» or «when did we touch `sync.rs`?», which
//! memories do not cover: the state one only keeps the latest snapshot.
//!
//! It is written when the session closes or compacts (`omoi recall --session-end`, which already reads the conversation
//! for the candidates) and never in the per-message recall. Each write adds to whatever was there:
//! after compacting, the session's `recall_log` is emptied, but what was received before is already in its line.
//!
//! It is local, like `recall_log` or `memory_sig`: it does not travel with sync nor with `omoi export`
//! (file paths belong to this machine and say a lot about what is being done; history belongs to
//! each machine). A 1.0 opens the database and does not see it, and it does not raise `lector_minimo`. The table appears with the
//! first line (`crear_tabla`), not when opening the database. It weighs a few hundred bytes per session; it is
//! kept for a year (`DIAS`).

use crate::memoria::{AHORA, Ambito};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// `files`, `recalled` and `saved` are JSON lists: the files (relative to the repository if they fall
/// inside) and the full ids of the memories. `folder` is the session's folder, to read the line
/// even if the project no longer exists or the session was in a general folder (`project_id` 0).
const TABLE: &str = "
CREATE TABLE IF NOT EXISTS diary (
  session    TEXT NOT NULL,
  project_id INTEGER NOT NULL DEFAULT 0,
  folder     TEXT NOT NULL DEFAULT '',
  first_at   TEXT NOT NULL,
  last_at    TEXT NOT NULL,
  files      TEXT NOT NULL DEFAULT '[]',
  recalled   TEXT NOT NULL DEFAULT '[]',
  saved      TEXT NOT NULL DEFAULT '[]',
  applied    TEXT NOT NULL DEFAULT '[]',
  PRIMARY KEY (session, project_id)
);
CREATE INDEX IF NOT EXISTS diary_last ON diary(last_at);
";

/// How many files are noted per session (the first ones touched).
pub const MAX_FICHEROS: usize = 50;
/// How many days a line is kept. With a few hundred bytes per session, a year of ten sessions a
/// day is a couple of MB: it fits easily, and beyond that almost nobody asks.
pub const DIAS: i64 = 365;
/// From how many of the session's subagents the files and memories are read (the most recent), and up to
/// what size: the close hook has little more than a second and afterwards still sets aside candidates.
pub const MAX_SUBAGENTES: usize = 10;
pub const MAX_BYTES_SUBAGENTE: u64 = 16 << 20;
/// And all the subagents of a session together.
const MAX_BYTES_ALL_SUBAGENTS: u64 = 32 * 1024 * 1024;

/// The table is created when writing the first line, not when opening the database: the per-message recall
/// does not even do that much extra.
pub fn crear_tabla(db: &Connection) -> Result<()> {
    db.execute_batch(TABLE)?;
    // `applied` (1.10): an older table does not have it. An earlier version names its columns when
    // writing and reading, so the extra column does not bother it.
    if !has_applied_column(db)? {
        db.execute_batch("ALTER TABLE diary ADD COLUMN applied TEXT NOT NULL DEFAULT '[]'")?;
    }
    Ok(())
}

/// Whether the table already has the `applied` column (1.10). Reading does not add it: reading does not write.
fn has_applied_column(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('diary') WHERE name = 'applied')", [], |f| f.get(0))?)
}

pub(crate) fn hay_tabla(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'diary')", [], |f| f.get(0))?)
}

/// What is extracted from the conversation.
#[derive(Debug, Default, PartialEq)]
pub struct Lectura {
    /// The files that were edited or written, in the order they were touched, without repeats.
    pub ficheros: Vec<String>,
    /// The ids returned by `remember` (saved) or `update`.
    pub guardadas: Vec<String>,
    /// The ids (or their prefix) passed to an `update` whose response was not found.
    pub pedidas: Vec<String>,
    /// The first time in the conversation, if it says (UTC, like `AHORA`).
    pub primera: Option<String>,
}

/// Tools that write files, in lowercase: Claude Code (Edit, Write, MultiEdit,
/// NotebookEdit), Gemini and Qwen (write_file, replace), Copilot (edit, create, str_replace_editor),
/// Cursor (edit_file, search_replace, delete_file), Kiro (fs_write), Codex (apply_patch).
const EDIT_TOOLS: &[&str] = &[
    "edit", "write", "multiedit", "notebookedit", "write_file", "replace", "edit_file", "create_file", "create", "str_replace",
    "str_replace_editor", "str_replace_based_edit_tool", "strreplace", "search_replace", "fs_write", "apply_patch", "delete_file", "delete",
];
/// Whether a line names a tool of interest (`"name":"Edit"`, `"toolName": "edit"`…) or carries
/// a Codex patch. Of the lines with a call, only those are parsed as JSON: in a conversation
/// of 73 MB, parsing every call (Bash, Read…) cost 130 ms, and searching for each name separately,
/// 40 more.
fn names_one(line: &str) -> bool {
    if line.contains("Begin Patch") {
        return true;
    }
    line.match_indices("ame\":").any(|(i, _)| {
        let rest = line[i + "ame\":".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else { return false };
        let name = &rest[..rest.find('"').unwrap_or(0)];
        !name.is_empty() && (is_edit(name) || omoikane_tool(name).is_some())
    })
}

/// Where those tools put the file.
const PATH_KEYS: &[&str] = &["file_path", "notebook_path", "path", "filePath", "absolute_path", "target_file", "file", "filename"];
/// Where a call's arguments go.
const ARGS_KEYS: &[&str] = &["input", "args", "arguments", "parameters", "params"];
/// A call's id, and the one its result uses to say which call it belongs to.
const ID_KEYS: &[&str] = &["id", "call_id", "toolCallId", "tool_call_id", "callId"];
const REF_KEYS: &[&str] = &["tool_use_id", "call_id", "toolCallId", "tool_call_id", "callId"];
/// Where a tool's response goes.
const RESULT_KEYS: &[&str] = &["content", "output", "result", "response", "functionResponse"];
/// The lines that may carry a tool call: the others are not parsed as JSON.
/// `"tool_use"` (Claude Code, Cursor), with the quotes so it does not match `"tool_use_id"`: that way the
/// results (which may be whole files) are only parsed if they belong to a pending call; `_call`
/// (`function_call`, `custom_tool_call`, `local_shell_call` from Codex, `tool_calls` from Grok), `Call`
/// (`functionCall` from Qwen, `toolCalls` from Gemini) and Copilot's. Few and short: each one is a
/// pass over the line.
const MARKERS: &[&str] = &["\"tool_use\"", "_call", "Call", "tool.execution_start", "Begin Patch"];

#[derive(Clone, Copy, PartialEq)]
enum Omoi {
    Remember,
    Update,
}

fn name_of(o: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    ["name", "toolName", "tool_name"].iter().find_map(|k| o.get(*k).and_then(|v| v.as_str()))
}

/// Omoikane's `remember` or `update`: bare, or from its MCP server (`mcp__omoikane__remember`,
/// `omoikane.update`…).
fn omoikane_tool(name: &str) -> Option<Omoi> {
    let n = name.to_lowercase();
    let (server, short) = match n.rfind("__").map(|i| (i, 2)).or_else(|| n.rfind(['.', '/', ':']).map(|i| (i, 1))) {
        Some((i, l)) => (&n[..i], &n[i + l..]),
        None => ("omoikane", n.as_str()),
    };
    if !server.contains("omoikane") {
        return None;
    }
    match short {
        "remember" => Some(Omoi::Remember),
        "update" => Some(Omoi::Update),
        _ => None,
    }
}

/// Whether the tool writes files. Those of an MCP server (`mcp__x__create`) do not: they are not the agent's.
fn is_edit(name: &str) -> bool {
    !name.contains("__") && EDIT_TOOLS.contains(&name.to_lowercase().as_str())
}

/// A call's arguments: an object, or a text that is JSON (Codex) or the patch itself.
fn arguments(o: &serde_json::Map<String, serde_json::Value>) -> Option<serde_json::Value> {
    let v = ARGS_KEYS.iter().find_map(|k| o.get(*k))?;
    Some(match v.as_str() {
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string())),
        None => v.clone(),
    })
}

/// The files of a Codex patch (`*** Update File: x`, `*** Add File:`, `*** Delete File:`,
/// `*** Move to:`).
fn from_patch(text: &str, r: &mut Vec<String>) {
    for l in text.lines() {
        let l = l.trim_start();
        for marker in ["*** Update File: ", "*** Add File: ", "*** Delete File: ", "*** Move to: "] {
            if let Some(p) = l.strip_prefix(marker) {
                r.push(p.trim().to_string());
            }
        }
    }
}

/// A full memory id (ULID: 26 uppercase letters and digits).
fn is_id(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// The id of what `remember` (only if it saved: `saved: true`) or `update` answered, searching the
/// result (which may come as JSON text inside another JSON).
fn id_from_result(v: &serde_json::Value, which: Omoi, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match v {
        serde_json::Value::Object(o) => {
            if let Some(id) = o.get("id").and_then(|x| x.as_str()).filter(|x| is_id(x))
                && (which == Omoi::Update || o.get("saved") == Some(&serde_json::Value::Bool(true)))
            {
                return Some(id.to_string());
            }
            if o.get("saved") == Some(&serde_json::Value::Bool(false)) {
                return None;
            }
            o.values().find_map(|x| id_from_result(x, which, depth + 1))
        }
        serde_json::Value::Array(xs) => xs.iter().find_map(|x| id_from_result(x, which, depth + 1)),
        serde_json::Value::String(s) if s.trim_start().starts_with(['{', '[']) => {
            serde_json::from_str::<serde_json::Value>(s).ok().and_then(|x| id_from_result(&x, which, depth + 1))
        }
        _ => None,
    }
}

struct Reader {
    l: Lectura,
    /// Calls to `remember`/`update` with no response yet, by their id, with the id passed to
    /// `update` (for when its response does not show up).
    pending: HashMap<String, (Omoi, Option<String>)>,
}

impl Reader {
    fn visit(&mut self, v: &serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                if let Some(name) = name_of(o) {
                    // What is inside belongs to this call (its response, in Gemini): it is not another one.
                    self.call(name, o);
                    return;
                } else if let Some(r) = REF_KEYS.iter().find_map(|k| o.get(*k).and_then(|x| x.as_str()))
                    && let Some((which, _)) = self.pending.remove(r)
                {
                    // No id in the response (an error, a duplicate): nothing was saved.
                    let id = RESULT_KEYS.iter().filter_map(|k| o.get(*k)).find_map(|x| id_from_result(x, which, 0));
                    self.l.guardadas.extend(id);
                    return;
                }
                for x in o.values() {
                    self.visit(x);
                }
            }
            serde_json::Value::Array(xs) => xs.iter().for_each(|x| self.visit(x)),
            _ => {}
        }
    }

    fn call(&mut self, name: &str, o: &serde_json::Map<String, serde_json::Value>) {
        let args = arguments(o);
        if let Some(which) = omoikane_tool(name) {
            // The response may come inside the same call (Gemini) or in another line, by its id.
            if let Some(id) = ["result", "response", "output"].iter().filter_map(|k| o.get(*k)).find_map(|x| id_from_result(x, which, 0)) {
                self.l.guardadas.push(id);
                return;
            }
            let requested = args.as_ref().and_then(|a| a["id"].as_str()).filter(|_| which == Omoi::Update).map(String::from);
            match ID_KEYS.iter().find_map(|k| o.get(*k).and_then(|x| x.as_str())) {
                Some(id) => {
                    self.pending.insert(id.to_string(), (which, requested));
                }
                None => self.l.pedidas.extend(requested),
            }
            return;
        }
        let Some(args) = args else { return };
        // The Codex patch, as a tool or inside a command.
        let text = match &args {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.contains("*** Begin Patch") {
            let mut r = Vec::new();
            match &args {
                serde_json::Value::String(s) => from_patch(s, &mut r),
                other => {
                    // Inside a JSON: the strings inside, already unescaped.
                    fn strings(v: &serde_json::Value, f: &mut dyn FnMut(&str)) {
                        match v {
                            serde_json::Value::String(s) => f(s),
                            serde_json::Value::Array(xs) => xs.iter().for_each(|x| strings(x, f)),
                            serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, f)),
                            _ => {}
                        }
                    }
                    strings(other, &mut |s| from_patch(s, &mut r));
                }
            }
            self.l.ficheros.extend(r);
            return;
        }
        if !is_edit(name) || args["command"].as_str() == Some("view") {
            return;
        }
        if let Some(p) = PATH_KEYS.iter().find_map(|k| args[*k].as_str()).filter(|p| !p.trim().is_empty()) {
            self.l.ficheros.push(p.trim().to_string());
        }
    }
}

/// The time of a record (`"timestamp":"2026-11-04T10:00:00.123Z"`), in the format of `AHORA`.
fn time_of(line: &str) -> Option<String> {
    for key in ["\"timestamp\":\"", "\"startTime\":\"", "\"timestamp\": \"", "\"startTime\": \""] {
        if let Some(i) = line.find(key) {
            let rest = &line[i + key.len()..];
            let v = &rest[..rest.find('"')?];
            let b = v.as_bytes();
            let shape = b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && v.ends_with('Z');
            if !shape {
                continue;
            }
            match v.len() {
                // With `get`: a multi-byte character in the middle is not a time, and must not break the hook.
                20 => return v.get(..19).map(|h| format!("{h}.000Z")),
                n if n >= 24 && b[19] == b'.' => return v.get(..23).map(|h| format!("{h}Z")),
                _ => continue,
            }
        }
    }
    None
}

/// What the conversation says: the files touched and the memories saved or changed. `root` is
/// the project folder: files inside it are made relative to it.
pub fn leer(path: &Path, root: &str) -> Result<Lectura> {
    // Without requiring UTF-8: a conversation cut in the middle of a character (the agent is still writing) is read anyway.
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut reader = Reader { l: Lectura::default(), pending: HashMap::new() };
    // Gemini CLI before June 2026: a whole JSON with `messages`.
    if let Ok(whole) = serde_json::from_str::<serde_json::Value>(&text)
        && whole["messages"].is_array()
    {
        reader.l.primera = time_of(&text);
        reader.visit(&whole["messages"]);
    } else {
        // Claude Code (and Cursor) only call with `"tool_use"`: one pass per line instead of all of them.
        let cut = (0..=text.len().min(65_536)).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        let claude = text[..cut].contains("\"parentUuid\"");
        let markers = if claude { &MARKERS[..1] } else { MARKERS };
        for (i, line) in text.lines().enumerate() {
            if reader.l.primera.is_none() && i < 50 {
                reader.l.primera = time_of(line);
            }
            let relevant = markers.iter().any(|m| line.contains(m)) && names_one(line)
                || reader.pending.keys().any(|id| line.contains(id.as_str()));
            if !relevant {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                reader.visit(&v);
            }
        }
    }
    let mut l = reader.l;
    l.pedidas.extend(reader.pending.into_values().filter_map(|(_, requested)| requested));
    let mut seen = BTreeSet::new();
    l.ficheros = l.ficheros.iter().map(|f| relativo(f, root)).filter(|f| seen.insert(f.clone())).take(MAX_FICHEROS).collect();
    Ok(l)
}

/// `f` relative to `root` if it falls inside, with `/`; otherwise as is (with `/`). First comparing the
/// text; if it does not match, with the real paths on disk, because the same folder may arrive written in
/// another form: on Windows with its short name (`C:/Users/RUNNER~1/…`), or through a link.
pub(crate) fn relativo(f: &str, root: &str) -> String {
    let r = relativo_texto(f, root);
    if r != f.replace('\\', "/") || root.is_empty() || !std::path::Path::new(f).is_absolute() {
        return r;
    }
    match (real_path(std::path::Path::new(f)), real_path(std::path::Path::new(root))) {
        (Some(rf), Some(rr)) => {
            let x = relativo_texto(&rf, &rr);
            if x != rf.replace('\\', "/") { x } else { r }
        }
        _ => r,
    }
}

/// The real path of `p` (that of its nearest existing folder, if `p` is gone), without Windows'
/// `\\?\`.
fn real_path(p: &std::path::Path) -> Option<String> {
    // A network path is not resolved: it would open an SMB connection (see `proyecto::ruta_de_red`).
    if !crate::proyecto::ruta_local(p) {
        return None;
    }
    let mut rest = Vec::new();
    let mut current = p;
    loop {
        if let Ok(mut c) = std::fs::canonicalize(current) {
            for part in rest.iter().rev() {
                c.push(part);
            }
            let t = c.to_string_lossy().into_owned();
            return Some(t.strip_prefix(r"\\?\").map(String::from).unwrap_or(t));
        }
        rest.push(current.file_name()?.to_os_string());
        current = current.parent()?;
    }
}

/// `relativo` without touching the disk: text only.
pub(crate) fn relativo_texto(f: &str, root: &str) -> String {
    let f = f.replace('\\', "/");
    let root = root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return f;
    }
    // The start of `f` is compared as is and not `f` lowercased: lowercasing can change
    // how many bytes a character takes, and cutting there would break.
    let n = root.len();
    match (f.get(..n), f.get(n..)) {
        (Some(head), Some(rest)) if rest.len() > 1 && rest.starts_with('/') && crate::proyecto::comparable(head) == crate::proyecto::comparable(root) => {
            rest[1..].to_string()
        }
        _ => f,
    }
}

fn json_list(db: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>> {
    let text: Option<String> = db.query_row(sql, p, |f| f.get(0)).optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// Appends `new` after `old`, without repeats, at most `max`.
fn merge(old: Vec<String>, new: Vec<String>, max: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    old.into_iter().chain(new).filter(|x| seen.insert(x.clone())).take(max).collect()
}

/// The session's key in the diary: its id or, without one, the conversation file's name.
pub fn nombre_sesion(session: &str, conversation: Option<&Path>) -> String {
    match session.trim() {
        "" => conversation.and_then(|c| c.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        s => s.to_string(),
    }
}

/// On close or compaction: writes (or completes) the session's line in the project. `conversation` is
/// the conversation's path, if the agent gives it; without it, only what the database knows (the memories
/// received). Without a session id, the conversation file's name.
pub fn apuntar(db: &Connection, project: &str, session: &str, conversation: Option<&Path>) -> Result<bool> {
    let session_key = nombre_sesion(session, conversation);
    if session_key.is_empty() {
        return Ok(false);
    }
    crear_tabla(db)?;
    let general = crate::proyecto::es_general(project);
    let project_id = if general || project.is_empty() { 0 } else { crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0) };
    let root = if general { "" } else { project };
    let mut reading = match conversation {
        Some(c) => leer(c, root)?,
        None => Lectura::default(),
    };
    // What its subagents did (Claude Code keeps them separately): their files and memories belong to
    // the session. One that cannot be read does not spoil the line.
    let mut read_bytes = 0u64;
    for sub in conversation.map(crate::candidatas::subagentes_de).unwrap_or_default().into_iter().take(MAX_SUBAGENTES) {
        let len = std::fs::metadata(&sub).map(|m| m.len()).unwrap_or(u64::MAX);
        // Each one with its cap, and all together with another: the close hook has little more than a second.
        if len > MAX_BYTES_SUBAGENTE || read_bytes + len > MAX_BYTES_ALL_SUBAGENTS {
            continue;
        }
        read_bytes += len;
        if let Ok(l) = leer(&sub, root) {
            reading.ficheros = merge(std::mem::take(&mut reading.ficheros), l.ficheros, MAX_FICHEROS);
            reading.guardadas.extend(l.guardadas);
            reading.pedidas.extend(l.pedidas);
        }
    }
    // Those received by relevance: preferences and pinned ones arrive in every session and
    // say nothing about this one.
    let mut q = db.prepare(
        "SELECT r.memory_id FROM recall_log r JOIN memory m ON m.id = r.memory_id
         WHERE r.session = ?1 AND m.pinned = 0 AND m.kind != 'preference' ORDER BY r.at, r.memory_id",
    )?;
    let recalled: Vec<String> = q.query_map([&session_key], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    // Those saved or changed, only if they exist (a response from another database, or already forgotten, does not count).
    let mut saved = Vec::new();
    for id in &reading.guardadas {
        if db.query_row("SELECT 1 FROM memory WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some() {
            saved.push(id.clone());
        }
    }
    for p in &reading.pedidas {
        if let Ok(id) = crate::memoria::resolver_id(db, p) {
            saved.push(id);
        }
    }
    let key = params![session_key, project_id];
    let files = merge(json_list(db, "SELECT files FROM diary WHERE session = ?1 AND project_id = ?2", key)?, reading.ficheros, MAX_FICHEROS);
    let recalled = merge(json_list(db, "SELECT recalled FROM diary WHERE session = ?1 AND project_id = ?2", key)?, recalled, usize::MAX);
    let saved = merge(json_list(db, "SELECT saved FROM diary WHERE session = ?1 AND project_id = ?2", key)?, saved, usize::MAX);
    let started: Option<String> = db.query_row("SELECT started_at FROM session WHERE session = ?1", [&session_key], |f| f.get(0)).optional()?;
    let first = [started, reading.primera].into_iter().flatten().min();
    db.execute(
        &format!(
            "INSERT INTO diary(session, project_id, folder, first_at, last_at, files, recalled, saved)
             VALUES (?1, ?2, ?3, min(COALESCE(?4, {AHORA}), {AHORA}), {AHORA}, ?5, ?6, ?7)
             ON CONFLICT(session, project_id) DO UPDATE SET folder = excluded.folder,
               first_at = min(diary.first_at, excluded.first_at), last_at = excluded.last_at,
               files = excluded.files, recalled = excluded.recalled, saved = excluded.saved"
        ),
        params![session_key, project_id, project, first, serde_json::to_string(&files)?, serde_json::to_string(&recalled)?, serde_json::to_string(&saved)?],
    )?;
    db.execute(&format!("DELETE FROM diary WHERE julianday({AHORA}) - julianday(last_at) > {DIAS}"), [])?;
    Ok(true)
}

/// A memory named in a line, with its title if it still exists.
#[derive(Clone, Debug, Serialize)]
pub struct Nombrada {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A diary line, for reading.
#[derive(Clone, Debug, Serialize)]
pub struct Linea {
    pub session: String,
    /// The project folder (or the session's, if it was not a project).
    pub project: String,
    pub first_at: String,
    pub last_at: String,
    pub files: Vec<String>,
    pub recalled: Vec<Nombrada>,
    pub saved: Vec<Nombrada>,
    /// Those the agent said it followed, citing their id (1.10): which memories the work was done with.
    pub applied: Vec<Nombrada>,
}

/// Which lines to read.
#[derive(Default)]
pub struct Filtro {
    /// From this day or moment (`2026-11-03`, `2026-11-03T10`): those that ended then or later.
    pub desde: Option<String>,
    /// Until this day or moment: those that started then or earlier.
    pub hasta: Option<String>,
    /// Those that touched a file whose path contains this.
    pub fichero: Option<String>,
    /// The folder it is looked at from, if it is that of the requested project: its lines without a project (the
    /// folder was not one yet when the session closed, or the project was pruned) are also its own.
    pub carpeta: Option<String>,
    pub limite: usize,
}

/// `7d`, `2w` or a date (`2026-11-03`, with or without time), as the start of a UTC moment.
pub fn fecha(db: &Connection, text: &str) -> Result<String> {
    let t = text.trim();
    if let Some(n) = t.strip_suffix('d').or_else(|| t.strip_suffix('w')).and_then(|n| n.parse::<i64>().ok()) {
        let days = if t.ends_with('w') { n * 7 } else { n };
        return Ok(db.query_row("SELECT strftime('%Y-%m-%d', 'now', ?1)", [format!("-{days} days")], |f| f.get(0))?);
    }
    let b = t.as_bytes();
    anyhow::ensure!(
        b.len() >= 10 && b[4] == b'-' && b[7] == b'-' && b[..4].iter().chain(&b[5..7]).chain(&b[8..10]).all(u8::is_ascii_digit),
        crate::tr!("not a date: «{t}» (use 2026-11-03, or 7d for the last 7 days)", "no es una fecha: «{t}» (usa 2026-11-03, o 7d para los últimos 7 días)")
    );
    Ok(t.trim_end_matches('Z').replace(' ', "T"))
}

/// The SQL fragment that keeps the lines visible from `scope` (`d` is the table).
fn where_clause(scope: &Ambito) -> String {
    match scope {
        Ambito::Todos => "1".into(),
        // Those with `project_id` 0, only if they belong to one of its folders (checked later, in `lineas`).
        Ambito::Proyecto { id, .. } => format!("d.project_id IN ({id}, 0)"),
        Ambito::Espacio(Some(s)) => format!("d.project_id IN (SELECT id FROM project WHERE space_id = {s})"),
        Ambito::Espacio(None) => "(d.project_id = 0 OR d.project_id IN (SELECT id FROM project WHERE space_id IS NULL))".into(),
    }
}

/// How a line's folder is compared with a project's.
pub(crate) fn carpeta_cmp(path: &str) -> String {
    crate::proyecto::comparable(path.replace('\\', "/").trim_end_matches('/'))
}

/// The diary lines visible from `scope`, from most recent to oldest.
pub fn lineas(db: &Connection, scope: &Ambito, f: &Filtro) -> Result<Vec<Linea>> {
    if !hay_tabla(db)? {
        return Ok(Vec::new());
    }
    // From a project, the lines without a project that belong to its folders (or to the folder it is
    // looked at from, if it is not a project yet).
    let folders: Option<BTreeSet<String>> = match scope {
        Ambito::Proyecto { id, .. } if *id != 0 => {
            let mut c: BTreeSet<String> = column_list(db, "SELECT path FROM project_path WHERE project_id = ?1", [id])?.iter().map(|p| carpeta_cmp(p)).collect();
            c.extend(f.carpeta.as_deref().filter(|x| !x.is_empty()).map(carpeta_cmp));
            Some(c)
        }
        _ => None,
    };
    // Project 0 exists with an empty path: without the `NULLIF`, a line without a project would not give its folder.
    let mut sql = format!(
        "SELECT d.session, d.project_id, d.folder, COALESCE(NULLIF((SELECT path FROM project WHERE id = d.project_id), ''), d.folder),
                d.first_at, d.last_at, d.files, d.recalled, d.saved, {}
         FROM diary d WHERE {}",
        if has_applied_column(db)? { "d.applied" } else { "'[]'" },
        where_clause(scope)
    );
    let mut values: Vec<String> = Vec::new();
    if let Some(d) = &f.desde {
        values.push(d.clone());
        sql.push_str(&format!(" AND substr(d.last_at, 1, length(?{n})) >= ?{n}", n = values.len()));
    }
    if let Some(h) = &f.hasta {
        values.push(h.clone());
        sql.push_str(&format!(" AND substr(d.first_at, 1, length(?{n})) <= ?{n}", n = values.len()));
    }
    // The file and the limit, here and not in SQL: SQLite's `lower()` only knows ASCII, and the limit
    // goes after the filter so that `-n` gives as many as there are.
    sql.push_str(" ORDER BY d.last_at DESC");
    let file = f.fichero.as_deref().map(|x| x.trim().replace('\\', "/").to_lowercase()).filter(|x| !x.is_empty());
    let limit = f.limite.max(1);
    let mut q = db.prepare(&sql)?;
    let mut rows = q.query(rusqlite::params_from_iter(values.iter()))?;
    let mut title = db.prepare("SELECT title FROM memory WHERE id = ?1")?;
    let mut name_all = |text: &str| -> Result<Vec<Nombrada>> {
        let ids: Vec<String> = serde_json::from_str(text).unwrap_or_default();
        ids.into_iter().map(|id| Ok(Nombrada { title: title.query_row([&id], |r| r.get(0)).optional()?, id })).collect()
    };
    let mut r = Vec::new();
    while r.len() < limit
        && let Some(row) = rows.next()?
    {
        let (project_id, folder): (i64, String) = (row.get(1)?, row.get(2)?);
        if project_id == 0
            && let Some(c) = &folders
            && !c.contains(&carpeta_cmp(&folder))
        {
            continue;
        }
        // File by file: the fragment could span two at once in the JSON (`a.rs","b`).
        let files: Vec<String> = serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default();
        if let Some(x) = &file
            && !files.iter().any(|p| p.to_lowercase().contains(x))
        {
            continue;
        }
        r.push(Linea {
            session: row.get(0)?,
            project: row.get(3)?,
            first_at: row.get(4)?,
            last_at: row.get(5)?,
            files,
            recalled: name_all(&row.get::<_, String>(7)?)?,
            saved: name_all(&row.get::<_, String>(8)?)?,
            applied: name_all(&row.get::<_, String>(9)?)?,
        });
    }
    Ok(r)
}

/// At most how many ids cited in a session are checked (the close hook has little time).
const MAX_CITED: usize = 200;

/// The memory ids the agent cites in what it writes (`texts`), resolved against the database: only
/// those of a memory that exists, without repeats. An id is a word of 8 to 26 characters of the
/// ULID alphabet (uppercase without I, L, O or U, and digits), starting with 0-7 and mixing letters and
/// digits: that way uppercase words («README») and numbers do not count. A unique prefix is enough (the
/// recall shows the first 12).
pub fn citadas<'a>(db: &Connection, texts: impl IntoIterator<Item = &'a str>) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut r = Vec::new();
    let mut checked = 0;
    for t in texts {
        for w in t.split(|c: char| !c.is_ascii_alphanumeric()) {
            if !looks_like_id(w) || !seen.insert(w.to_string()) {
                continue;
            }
            checked += 1;
            if checked > MAX_CITED {
                return Ok(r);
            }
            if let Ok(id) = crate::memoria::resolver_id(db, w)
                && !r.contains(&id)
            {
                r.push(id);
            }
        }
    }
    Ok(r)
}

fn looks_like_id(w: &str) -> bool {
    let b = w.as_bytes();
    (8..=26).contains(&b.len())
        && (b'0'..=b'7').contains(&b[0])
        && b.iter().all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !matches!(c, b'I' | b'L' | b'O' | b'U')))
        && b.iter().any(u8::is_ascii_uppercase)
        && b.iter().filter(|c| c.is_ascii_digit()).count() >= 2
}

/// Writes in the session's line the memories the agent said it followed (`ids`, already resolved), without
/// those it saved or changed in it: citing the one it just wrote is not applying it. Adds to whatever
/// was there (after compacting, the conversation continues and is read again in full). Without a line, nothing.
pub fn apuntar_aplicadas(db: &Connection, project: &str, session: &str, conversation: Option<&Path>, ids: &[String]) -> Result<usize> {
    let session = nombre_sesion(session, conversation);
    if ids.is_empty() || session.is_empty() || !hay_tabla(db)? {
        return Ok(0);
    }
    crear_tabla(db)?;
    let general = crate::proyecto::es_general(project);
    let project_id = if general || project.is_empty() { 0 } else { crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0) };
    let key = params![session, project_id];
    let row: Option<(String, String)> =
        db.query_row("SELECT saved, applied FROM diary WHERE session = ?1 AND project_id = ?2", key, |f| Ok((f.get(0)?, f.get(1)?))).optional()?;
    let Some((saved, before)) = row else { return Ok(0) };
    let saved: Vec<String> = serde_json::from_str(&saved).unwrap_or_default();
    let new: Vec<String> = ids.iter().filter(|id| !saved.contains(id)).cloned().collect();
    let all = merge(serde_json::from_str(&before).unwrap_or_default(), new, usize::MAX);
    db.execute("UPDATE diary SET applied = ?3 WHERE session = ?1 AND project_id = ?2", params![session, project_id, serde_json::to_string(&all)?])?;
    Ok(all.len())
}

/// How many diary sessions applied memory `id` and when the last one was (empty if none).
pub fn aplicada(db: &Connection, id: &str) -> Result<(i64, Option<String>)> {
    if !hay_tabla(db)? || !has_applied_column(db)? {
        return Ok((0, None));
    }
    // The full id goes in quotes in the JSON list: `LIKE` with it does not confuse one with another.
    Ok(db.query_row(
        "SELECT count(*), max(last_at) FROM diary WHERE applied LIKE '%\"' || ?1 || '\"%'",
        [id],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?)
}

fn column_list(db: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(sql)?;
    let v = q.query_map(p, |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// After `omoi projects prune`: the lines of a project that no longer exists become project-less
/// (and are still visible from their folder). If they kept their id, a new project that
/// reused it (without `AUTOINCREMENT`, SQLite may hand out the id again) would inherit lines that are not
/// its own, perhaps from another space.
pub fn soltar_podados(db: &Connection) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    let orphans = "project_id != 0 AND project_id NOT IN (SELECT id FROM project)";
    // If the same session already had a line without a project, that one stays.
    db.execute(&format!("UPDATE OR IGNORE diary SET project_id = 0 WHERE {orphans}"), [])?;
    db.execute(&format!("DELETE FROM diary WHERE {orphans}"), [])?;
    Ok(())
}

/// Before pruning: the lines of a space's projects that are about to be deleted are deleted with them.
/// Without a project they would be visible from the personal space, and the diary never crosses from one client to another.
pub fn antes_de_podar(db: &Connection) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    db.execute(
        "DELETE FROM diary WHERE project_id IN (SELECT id FROM project WHERE id != 0 AND space_id IS NOT NULL AND id NOT IN (SELECT DISTINCT project_id FROM memory))",
        [],
    )?;
    Ok(())
}

/// When merging two projects (`proyecto::juntar`): the lines of `from` move to `to`.
pub fn mover(db: &Connection, from: i64, to: i64) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    db.execute("UPDATE OR IGNORE diary SET project_id = ?2 WHERE project_id = ?1", params![from, to])?;
    db.execute("DELETE FROM diary WHERE project_id = ?1", [from])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names() {
        assert!(omoikane_tool("mcp__omoikane__remember") == Some(Omoi::Remember));
        assert!(omoikane_tool("remember") == Some(Omoi::Remember));
        assert!(omoikane_tool("mcp__plugin_omoikane_omoikane__update") == Some(Omoi::Update));
        assert!(omoikane_tool("mcp__claude_ai_Claude_Docs__update").is_none());
        assert!(is_edit("Edit") && is_edit("write_file") && !is_edit("mcp__docs__create") && !is_edit("Bash"));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relativo("/w/repo/src/a.rs", "/w/repo"), "src/a.rs");
        assert_eq!(relativo("/w/repo2/a.rs", "/w/repo"), "/w/repo2/a.rs");
        assert_eq!(relativo("src/a.rs", "/w/repo"), "src/a.rs");
        assert_eq!(relativo(r"D:\Omoikane\src\sync.rs", r"D:\Omoikane"), "src/sync.rs");
        // Characters that change size when lowercased (`İ` takes 2 bytes, `i̇` 3): no panic.
        assert_eq!(relativo("/w/İİ/a.rs", "/w/ii"), "/w/İİ/a.rs");
        assert_eq!(relativo("/w/ii/a.rs", "/w/İİ"), "/w/ii/a.rs");
        assert_eq!(relativo("/w/ña/a.rs", "/w/ña"), "a.rs");
    }

    #[test]
    fn times_and_dates() {
        assert_eq!(time_of(r#"{"timestamp":"2026-11-04T10:00:00.123Z","x":1}"#).as_deref(), Some("2026-11-04T10:00:00.123Z"));
        assert_eq!(time_of(r#"{"timestamp":"2026-11-04T10:00:00Z"}"#).as_deref(), Some("2026-11-04T10:00:00.000Z"));
        assert_eq!(time_of(r#"{"timestamp":"ayer"}"#), None);
        // With multi-byte letters where the time goes: not a time, and does not break.
        assert_eq!(time_of(r#"{"timestamp":"2026-11-04T10:00:00.12ñZ"}"#), None);
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(fecha(&db, "2026-11-03").unwrap(), "2026-11-03");
        assert_eq!(fecha(&db, "7d").unwrap().len(), 10);
        assert!(fecha(&db, "martes").is_err());
    }
}
