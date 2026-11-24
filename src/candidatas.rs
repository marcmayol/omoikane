//! Saving even if the agent doesn't (Phase 7.2): when a session closes or compacts, Omoikane reads the
//! transcript and sets aside memory **candidates**; when the next one starts in that project they are
//! handed to the agent, which saves the worthwhile ones (with `remember`, in the user's words) and drops
//! the rest. Omoikane has no model: it detects where something may be; deciding and writing is the agent's job.
//!
//! Calibrated with Marc's real conversations (29-oct-2026). What didn't work: looking for decision
//! words («me quedo con…») missed almost all the real decisions, which are short answers
//! to a question from the agent («Me gusta mucho la 1C, pero no la veo como portada»), and what it
//! caught made no sense without the question; errors followed by a fix were almost all failures
//! of one-off scripts. What remains: each answer to a question, paired with it; the
//! answers to `AskUserQuestion`; preference or correction phrases; and milestones (commit, tag,
//! release). Nothing that looks like a secret, nothing already saved, at most 5 (plus milestones).
//!
//! Reads the transcripts of Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI and Cursor, and a generic
//! JSONL (Kiro, the OpenCode plugin): see `turnos`.

use crate::memoria::{NOW, Scope};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

pub const DECISION: &str = "decision";
pub const PREFERENCIA: &str = "preference";
pub const HITO: &str = "milestone";
/// A subagent's final report (1.4).
pub const SUBAGENTE: &str = "subagent";
/// Up to how many characters of a subagent's report go into the candidate.
const REPORT_MAX: usize = 700;

/// How many are proposed per session (not counting milestones).
pub const MAXIMO: usize = 5;
/// Days they wait for the next session before expiring.
pub const DIAS: i64 = 7;
/// From this coverage by an existing memory on, it is already saved.
const ALREADY_SAVED: f64 = 0.6;

#[derive(Clone, Debug, Serialize)]
pub struct Candidata {
    pub kind: &'static str,
    pub text: String,
    #[serde(skip)]
    puntos: f64,
}

fn normal(s: &str) -> String {
    crate::buscar::strip_accents(&s.to_lowercase())
}

fn contains_phrase(t: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|w| {
        t.match_indices(w).any(|(i, _)| {
            let before = t[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let after = t[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
            before && after
        })
    })
}

/// What the user says when stating how they want things.
const PREFERENCE_PHRASES: &[&str] = &[
    "prefiero", "no quiero", "no me gusta", "me gusta mas", "nunca hagas", "nunca uses", "nunca pongas", "no hagas", "no uses", "no pongas",
    "no toques", "no cambies", "no sugieras", "no propongas", "te he dicho", "te lo he dicho", "deja de", "a partir de ahora",
    "de ahora en adelante", "recuerda que", "acuerdate de", "no vuelvas a", "quiero que siempre", "quiero que nunca", "i prefer",
    "from now on", "stop doing", "never do", "don't ever",
];
/// What, in an answer, sounds like an opinion or a choice (raises its score).
const OPINION: &[&str] = &["gusta", "prefiero", "mejor", "veo", "quiero", "elijo", "me quedo", "vamos con", "no", "pero", "like", "prefer", "rather", "choose"];
/// Requests («dame…», «explícame…», «publica…»): they ask for something, they decide nothing; they go last.
const REQUESTS: &[&str] = &[
    "dame", "haz", "hazme", "hazlo", "explicame", "explica", "publica", "ponte", "busca", "buscame", "investiga", "mira", "prepara",
    "sube", "subelo", "crea", "anade", "pon", "ponla", "ponlo", "quita", "borra", "borralos", "arregla", "prueba", "abre", "lee", "escribe",
    "give", "make", "explain", "publish", "search", "look", "write", "fix", "open", "show",
];

/// Answers that say nothing without more context.
const TRIVIAL_REPLIES: &[&str] = &["si", "sí", "vale", "ok", "okay", "claro", "perfecto", "genial", "gracias", "adelante", "dale", "continua", "sigue", "hazlo", "venga", "bien", "no", "yes", "go", "sure", "thanks"];

/// Known secret formats, or a long «word» mixing lowercase, uppercase and digits.
pub fn parece_secreto(t: &str) -> bool {
    // What `secretos` would redact (prefixed tokens, PEM keys, JWT, strings that look random…).
    if !crate::secretos::hallazgos(t).is_empty() {
        return true;
    }
    // And, for setting aside, also whatever talks about a key even if the token's shape isn't visible.
    let n = t.to_lowercase();
    if n.contains("private key") {
        return true;
    }
    for key in ["password", "passwd", "contraseña", "contrasena", "token", "secret", "api_key", "apikey", "api-key"] {
        if let Some(i) = n.find(key) {
            let rest = n[i + key.len()..].trim_start();
            if rest.starts_with(':') || rest.starts_with('=') {
                return true;
            }
        }
    }
    false
}

/// The last question in the last paragraphs of an agent message (without Markdown marks).
fn last_question(text: &str) -> Option<String> {
    let clean: String = text.chars().filter(|c| !"*_`#>".contains(*c)).collect();
    let paragraphs: Vec<&str> = clean.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
    for p in paragraphs.iter().rev().take(3) {
        if let Some(end) = p.rfind('?') {
            let before = &p[..end];
            let start = before.rfind(['.', '!', '?', '\n']).map(|i| i + 1).unwrap_or(0);
            let q: String = p[start..=end].trim().chars().take(240).collect();
            return Some(q);
        }
    }
    None
}

/// `"question?"="answer"` from the result of `AskUserQuestion`.
fn parse_answers(output: &str) -> Vec<(String, String)> {
    let mut r = Vec::new();
    let mut rest = output;
    while let Some(i) = rest.find("\"=\"") {
        let before = &rest[..i];
        let after = &rest[i + 3..];
        let Some(start) = before.rfind('"') else { break };
        let Some(end) = after.find('"') else { break };
        let p = &before[start + 1..];
        if p.ends_with('?') && p.len() >= 8 {
            r.push((p.to_string(), after[..end].trim().to_string()));
        }
        rest = &after[end..];
    }
    r
}

/// What the system injects as if it came from the user (reminders, commands, command output).
fn is_noise(t: &str) -> bool {
    let t = t.trim();
    t.is_empty() || t.starts_with('<') || t.starts_with('[') || t.contains("system-reminder") || t.contains("local-command") || t.starts_with("Caveat:") || t.chars().count() > 1200
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A turn of the conversation, whichever agent it comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Turno {
    Usuario(String),
    Agente(String),
    /// A question with options and what the user answered (Claude Code's `AskUserQuestion`).
    Respuesta(String, String),
    /// A command that ran fine (for milestones: commit, tag, release).
    Comando(String),
}

fn parts_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(xs) => xs
            .iter()
            .filter(|b| b["thought"] != true && (b["type"].is_null() || b["type"] == "text" || b["type"] == "input_text" || b["type"] == "output_text"))
            .filter_map(|b| b["text"].as_str().or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The command of a tool call: `command` as text or as a list, or inside `arguments`
/// (which in Codex is JSON inside a string).
fn command_of(v: &serde_json::Value) -> Option<String> {
    let v = match v.as_str() {
        Some(s) => serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::String(s.to_string())),
        None => v.clone(),
    };
    for k in ["command", "cmd"] {
        match &v[k] {
            serde_json::Value::String(s) => return Some(s.clone()),
            serde_json::Value::Array(xs) => return Some(xs.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")),
            _ => {}
        }
    }
    None
}

fn is_user_role(role: &str) -> Option<bool> {
    match role {
        "user" | "human" => Some(true),
        "assistant" | "gemini" | "model" | "qwen" | "agent" | "ai" => Some(false),
        _ => None,
    }
}

/// What the user says inside `<user_query>` (Cursor); without it, the line was added by the agent.
fn cursor_query(t: &str) -> Option<String> {
    let i = t.find("<user_query>")? + "<user_query>".len();
    let f = t[i..].find("</user_query>").map(|f| i + f).unwrap_or(t.len());
    Some(t[i..f].trim().to_string())
}

/// The turns of a conversation from any of the known agents (Claude Code, Codex,
/// Gemini CLI, Qwen Code, Copilot CLI, Cursor), or from a generic JSONL with `role`/`type` and
/// `text`/`content`/`parts` (Kiro, the one the OpenCode plugin writes, new formats).
pub fn turnos(path: &Path) -> Result<Vec<Turno>> {
    // As bytes: when compacting, the transcript may be cut in the middle of a character.
    let text = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
    // Gemini CLI before June 2026: a whole JSON with `messages`.
    if let Ok(whole) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(ms) = whole["messages"].as_array()
    {
        let mut r = Vec::new();
        for m in ms {
            generic_turn(m, &mut r);
        }
        return Ok(r);
    }
    // (id, turn): Gemini repeats the same message with the same id while it grows; the last one counts.
    let mut r: Vec<(Option<String>, Turno)> = Vec::new();
    let mut tools: std::collections::HashMap<String, (String, serde_json::Value)> = std::collections::HashMap::new();
    let put = |r: &mut Vec<(Option<String>, Turno)>, id: Option<String>, t: Turno| {
        if let Some(id) = &id
            && let Some(slot) = r.iter_mut().find(|(x, _)| x.as_deref() == Some(id.as_str()))
        {
            slot.1 = t;
            return;
        }
        r.push((id, t));
    };
    for line in text.lines() {
        let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let kind = d["type"].as_str().unwrap_or_default();
        if d["isSidechain"] == true || d["isMeta"] == true {
            continue;
        }
        // Claude Code.
        if (kind == "user" || kind == "assistant") && d["message"]["content"].is_array() || (kind == "user" && d["message"]["content"].is_string()) {
            let m = &d["message"];
            if kind == "assistant" {
                for b in m["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_use" {
                        tools.insert(b["id"].as_str().unwrap_or_default().to_string(), (b["name"].as_str().unwrap_or_default().to_string(), b["input"].clone()));
                    } else if let Some(t) = b["text"].as_str().filter(|t| b["type"] == "text" && !t.trim().is_empty()) {
                        put(&mut r, None, Turno::Agente(t.to_string()));
                    }
                }
                continue;
            }
            if let Some(blocks) = m["content"].as_array()
                && blocks.iter().any(|b| b["type"] == "tool_result")
            {
                for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                    let Some((name, input)) = tools.get(b["tool_use_id"].as_str().unwrap_or_default()) else { continue };
                    let output = parts_text(&b["content"]);
                    if name == "AskUserQuestion" {
                        for (p, answer) in parse_answers(&output) {
                            put(&mut r, None, Turno::Respuesta(p, answer));
                        }
                    }
                    let failed = b["is_error"] == true || output.contains("Exit code ") && !output.contains("Exit code 0");
                    if !failed && let Some(c) = command_of(input) {
                        put(&mut r, None, Turno::Comando(c));
                    }
                }
                continue;
            }
            put(&mut r, None, Turno::Usuario(parts_text(&m["content"])));
            continue;
        }
        // Codex: `response_item` with the message or the call inside `payload`.
        if kind == "response_item" {
            let p = &d["payload"];
            match p["type"].as_str().unwrap_or_default() {
                "message" => match p["role"].as_str() {
                    Some("user") => put(&mut r, None, Turno::Usuario(parts_text(&p["content"]))),
                    Some("assistant") => put(&mut r, None, Turno::Agente(parts_text(&p["content"]))),
                    _ => {}
                },
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    if let Some(c) = command_of(&p["arguments"]).or_else(|| command_of(&p["input"])).or_else(|| command_of(&p["action"])) {
                        put(&mut r, None, Turno::Comando(c));
                    }
                }
                _ => {}
            }
            continue;
        }
        // Copilot CLI: `events.jsonl`.
        if kind == "user.message" {
            if !d["data"]["source"].as_str().is_some_and(|s| s.starts_with("skill-")) {
                put(&mut r, None, Turno::Usuario(d["data"]["content"].as_str().unwrap_or_default().to_string()));
            }
            continue;
        }
        if kind == "assistant.message" {
            if d["data"]["parentToolCallId"].is_null() {
                put(&mut r, None, Turno::Agente(d["data"]["content"].as_str().unwrap_or_default().to_string()));
            }
            continue;
        }
        if kind == "tool.execution_start" {
            if let Some(c) = command_of(&d["data"]["arguments"]) {
                put(&mut r, None, Turno::Comando(c));
            }
            continue;
        }
        // Gemini CLI (JSONL since June 2026): `$set` rebuilds the list; each message carries its id.
        if let Some(ms) = d["$set"]["messages"].as_array() {
            for m in ms {
                let mut t = Vec::new();
                generic_turn(m, &mut t);
                for x in t {
                    put(&mut r, m["id"].as_str().map(String::from), x);
                }
            }
            continue;
        }
        // Cursor: Anthropic style, and what the user writes goes inside `<user_query>`.
        if let Some(role) = d["role"].as_str()
            && d["message"]["content"].is_array()
        {
            for b in d["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    if let Some(c) = command_of(&b["input"]) {
                        put(&mut r, None, Turno::Comando(c));
                    }
                } else if let Some(t) = b["text"].as_str() {
                    match role {
                        "user" => {
                            if let Some(q) = cursor_query(t) {
                                put(&mut r, None, Turno::Usuario(q));
                            }
                        }
                        "assistant" => put(&mut r, None, Turno::Agente(t.to_string())),
                        _ => {}
                    }
                }
            }
            continue;
        }
        // Grok Build: one `ConversationItem` per line, without `id` or `message`. What Grok adds on its
        // own (instructions, reminders) carries `synthetic_reason`; what the user writes doesn't
        // (or `interjection`, if sent while the agent was working).
        if d.get("id").is_none() && d.get("message").is_none() {
            if kind == "user" && d["content"][0]["type"] == "text" {
                if d["synthetic_reason"].as_str().is_none_or(|s| s == "interjection") {
                    put(&mut r, None, Turno::Usuario(parts_text(&d["content"])));
                }
                continue;
            }
            if kind == "assistant" && d["content"].is_string() {
                if let Some(t) = d["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    put(&mut r, None, Turno::Agente(t.to_string()));
                }
                for c in d["tool_calls"].as_array().into_iter().flatten() {
                    if let Some(c) = command_of(&c["arguments"]) {
                        put(&mut r, None, Turno::Comando(c));
                    }
                }
                continue;
            }
        }
        // Gemini, Qwen and the generic one (Kiro, OpenCode, others).
        let mut t = Vec::new();
        generic_turn(&d, &mut t);
        for x in t {
            put(&mut r, d["id"].as_str().filter(|_| d["type"] == "gemini" || d["type"] == "user").map(String::from), x);
        }
    }
    Ok(r.into_iter().map(|(_, t)| t).collect())
}

/// A record with a role (`role`, `type`, `author` or `sender`) and text (`text`, `content`, `message`
/// or `parts[].text`), or a command (`command`).
fn generic_turn(d: &serde_json::Value, r: &mut Vec<Turno>) {
    let role = ["role", "type", "author", "sender"].iter().find_map(|k| d[k].as_str()).unwrap_or_default();
    if let Some(c) = command_of(d).filter(|_| matches!(role, "tool" | "command" | "shell")) {
        r.push(Turno::Comando(c));
        return;
    }
    let Some(is_user) = is_user_role(role) else { return };
    let mut t = String::new();
    for v in [&d["text"], &d["content"], &d["message"]["parts"], &d["parts"], &d["message"]["content"], &d["message"]] {
        t = parts_text(v);
        if !t.trim().is_empty() {
            break;
        }
    }
    // Qwen: tool calls go in `message.parts[].functionCall`.
    for p in d["message"]["parts"].as_array().into_iter().flatten() {
        if let Some(c) = command_of(&p["functionCall"]["args"]) {
            r.push(Turno::Comando(c));
        }
    }
    if !t.trim().is_empty() {
        r.push(if is_user { Turno::Usuario(t) } else { Turno::Agente(t) });
    }
}

/// The candidates of a conversation, from most to least promising.
pub fn de_conversacion(path: &Path) -> Result<Vec<Candidata>> {
    Ok(de_turnos(turnos(path)?))
}

/// `de_conversacion` with the turns already read (on close they are read once for everything).
pub fn de_turnos(turns: Vec<Turno>) -> Vec<Candidata> {
    let mut r: Vec<Candidata> = Vec::new();
    let mut last_agent_text = String::new();
    let mut commit = None;
    let mut published = None;
    for turn in turns {
        match turn {
            Turno::Agente(t) => {
                if !t.trim().is_empty() {
                    last_agent_text = t;
                }
            }
            Turno::Respuesta(p, answer) => r.push(Candidata { kind: DECISION, text: one_line(&format!("{p} → {answer}")), puntos: 2.5 }),
            Turno::Comando(cmd) => {
                if let Some(i) = cmd.find("git commit") {
                    let rest = &cmd[i..];
                    if let Some(j) = rest.find("-m \"").or_else(|| rest.find("-m '")) {
                        let msg: String = rest[j + 4..].split(['"', '\'', '\n']).next().unwrap_or_default().to_string();
                        if !msg.is_empty() {
                            commit = Some(msg);
                        }
                    }
                }
                if let Some(i) = cmd.find("git tag -a ") {
                    published = cmd[i + 11..].split_whitespace().next().map(|t| format!("tag {t}"));
                } else if cmd.contains("cargo publish") && published.is_none() {
                    published = Some("cargo publish".into());
                }
            }
            Turno::Usuario(t) => {
                if is_noise(&t) {
                    continue;
                }
                let t = t.trim();
                let n = normal(t);
                let question = (!last_agent_text.is_empty()).then(|| last_question(&last_agent_text)).flatten();
                let with_question = |p: &Option<String>| p.as_ref().map(|q| format!("{q} → {t}")).unwrap_or_else(|| t.to_string());
                if contains_phrase(&n, PREFERENCE_PHRASES) {
                    r.push(Candidata { kind: PREFERENCIA, text: one_line(&with_question(&question)), puntos: 3.0 });
                } else if let Some(q) = &question {
                    let trivial = TRIVIAL_REPLIES.contains(&n.trim_matches(|c: char| !c.is_alphanumeric()));
                    if !trivial && t.chars().count() < 600 {
                        let first_word = n.split(|c: char| !c.is_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("");
                        let task = if REQUESTS.contains(&first_word) { -1.5 } else { 0.0 };
                        let opinion = if contains_phrase(&n, OPINION) { 1.0 } else { 0.0 } + task;
                        let size = (t.chars().count() as f64 / 200.0).min(1.0);
                        r.push(Candidata { kind: DECISION, text: one_line(&format!("{q} → {t}")), puntos: 1.0 + opinion + size });
                    }
                }
                last_agent_text.clear();
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut r: Vec<Candidata> = r.into_iter().filter(|c| !parece_secreto(&c.text) && seen.insert(c.text.clone())).collect();
    // Best first, and on ties, the most recent (the last thing decided is usually what holds).

    let mut ordered: Vec<(usize, Candidata)> = r.drain(..).enumerate().collect();
    ordered.sort_by(|(ia, a), (ib, b)| b.puntos.total_cmp(&a.puntos).then(ib.cmp(ia)));
    let mut output: Vec<Candidata> = ordered.into_iter().map(|(_, c)| c).collect();
    output.truncate(MAXIMO);
    for h in [commit.map(|c| format!("last commit: {c}")), published.map(|p| format!("published: {p}"))].into_iter().flatten() {
        if !parece_secreto(&h) {
            output.push(Candidata { kind: HITO, text: h, puntos: 0.0 });
        }
    }
    output
}

/// Where a session's transcript is when the hook doesn't give the path (Copilot on `sessionEnd`,
/// Kiro on `Stop`): by its id, in the folders each one uses.
pub fn buscar_conversacion(session_id: &str) -> Option<std::path::PathBuf> {
    let id = session_id.trim();
    // No folders or drives (`C:x`): the id comes from the hook and is joined to a folder.
    if id.is_empty() || id.contains(['/', '\\', '.', ':']) {
        return None;
    }
    // The folders from the variables, only if they belong to this machine (see `proyecto::ruta_local`).
    let home = crate::proyecto::casa_local()?;
    let from_var = |v: &str| std::env::var_os(v).map(std::path::PathBuf::from).and_then(crate::proyecto::local);
    let copilot = from_var("COPILOT_HOME").unwrap_or_else(|| home.join(".copilot"));
    [
        copilot.join("session-state").join(id).join("events.jsonl"),
        copilot.join("session-state").join(format!("{id}.jsonl")),
        home.join(".kiro").join("sessions").join("cli").join(format!("{id}.jsonl")),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .or_else(|| {
        // Grok: `~/.grok/sessions/<encoded folder>/<id>/chat_history.jsonl`.
        let grok = from_var("GROK_HOME").unwrap_or_else(|| home.join(".grok"));
        std::fs::read_dir(grok.join("sessions")).ok()?.flatten().map(|d| d.path().join(id).join("chat_history.jsonl")).find(|p| p.is_file())
    })
}

/// The subagents of a Claude Code session, which stores their transcripts next to the session's
/// (`<folder>/<session id>/subagents/*.jsonl`), from newest to oldest.
pub fn subagentes_de(transcript: &Path) -> Vec<std::path::PathBuf> {
    let (Some(dir), Some(id)) = (transcript.parent(), transcript.file_stem()) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(dir.join(id).join("subagents")) else { return Vec::new() };
    let mut v: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    v.sort();
    v.into_iter().rev().map(|(_, p)| p).collect()
}

/// What a subagent was asked to do (the first line of its first message) and its final report,
/// which the main agent reads once and almost never saves. In Marc's 142 subagent conversations
/// (4-nov-2026) the median report is 3,459 characters; the other signal that was considered, «a
/// command fails and then the same one works», didn't show up once (and with the same program and other
/// arguments, twice, both noise), so it is not used. Returns the candidate and what is proposed
/// from the report, to compare with what is already saved.
pub fn de_subagente(path: &Path) -> Result<Option<(Candidata, String)>> {
    // Only its first and last messages are needed: the start and end of the file are read, not
    // the whole thing (some are 50 MB, and SessionEnd has little more than a second).
    let (head, tail) = ends_of(path)?;
    let task = head.lines().find_map(|l| {
        let d: serde_json::Value = serde_json::from_str(l).ok()?;
        (d["type"] == "user").then(|| parts_text(&d["message"]["content"]))
    });
    // Its last message, if it is the final one (`end_turn`): one still working (when compacting the
    // session with subagents running) ends in a tool call and hasn't reported anything yet.
    let last: Option<serde_json::Value> = tail.lines().rev().find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok().filter(|d| d["type"] == "assistant"));
    let report = last.filter(|d| d["message"]["stop_reason"] != "tool_use").and_then(|d| {
        d["message"]["content"].as_array()?.iter().rev().find_map(|b| b["text"].as_str().filter(|t| b["type"] == "text" && !t.trim().is_empty()).map(String::from))
    });
    let Some(report) = report.map(|t| one_line(&t)) else { return Ok(None) };
    let task: String = task.as_deref().unwrap_or("").lines().map(|l| l.trim_matches(|c: char| "*_`#> ".contains(c))).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect();
    if parece_secreto(&report) || parece_secreto(&task) {
        return Ok(None);
    }
    let short: String = if report.chars().count() > REPORT_MAX { format!("{}…", report.chars().take(REPORT_MAX).collect::<String>()) } else { report.clone() };
    // What is already saved is compared with what is proposed (the start of the report), not with the whole
    // report: it is cheaper and a memory summarizing it looks more like that.
    let text = if task.is_empty() { short.clone() } else { format!("{task} → {short}") };
    Ok(Some((Candidata { kind: SUBAGENTE, text, puntos: 0.0 }, short)))
}

/// How much of the end of a subagent's transcript is read to find its last message.
const TAIL_BYTES: u64 = 256 * 1024;

/// The first lines (up to 64 KB) and the end (the last `TAIL_BYTES` bytes, without the first line, which
/// may be cut) of a file; if it is small, the whole file in both.
fn ends_of(path: &Path) -> Result<(String, String)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    if size <= TAIL_BYTES {
        let mut b = Vec::new();
        f.read_to_end(&mut b)?;
        let t = String::from_utf8_lossy(&b).into_owned();
        return Ok((t.clone(), t));
    }
    let mut head = vec![0; 64 * 1024];
    let n = f.read(&mut head)?;
    head.truncate(n);
    f.seek(SeekFrom::Start(size - TAIL_BYTES))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    let tail = String::from_utf8_lossy(&tail);
    let tail = tail.split_once('\n').map(|(_, r)| r).unwrap_or("").to_string();
    Ok((String::from_utf8_lossy(&head).into_owned(), tail))
}

/// On close or compact: sets aside the transcript's candidates and its subagents' reports
/// that are not already saved.
pub fn apartar(db: &Connection, project: &str, session: &str, path: &Path) -> Result<usize> {
    apartar_con(db, project, session, path, turnos(path)?)
}

/// `apartar` with the transcript's turns already read.
pub fn apartar_con(db: &Connection, project: &str, session: &str, path: &Path, turns: Vec<Turno>) -> Result<usize> {
    let mut cs: Vec<(Candidata, String)> = de_turnos(turns)
        .into_iter()
        .map(|c| {
            let answer = c.text.rsplit(" → ").next().unwrap_or(&c.text).to_string();
            (c, answer)
        })
        .collect();
    // The `MAXIMO` most recent reports from subagents that have finished, looking at most at twice as many
    // transcripts (SessionEnd has little more than a second).
    let mut reports = 0;
    for s in subagentes_de(path).into_iter().take(2 * MAXIMO) {
        if reports == MAXIMO {
            break;
        }
        if let Ok(Some(c)) = de_subagente(&s) {
            cs.push(c);
            reports += 1;
        }
    }
    let memories = crate::memoria::recent(db, &Scope::from_project(db, project)?, None, usize::MAX >> 1)?;
    let mut n = 0;
    for (c, answer) in cs {
        let coverage = crate::recall::coverage(db, project, &answer, memories.iter())?.into_iter().fold(0.0, f64::max);
        if c.kind != HITO && coverage >= ALREADY_SAVED {
            continue;
        }
        n += db.execute(
            &format!("INSERT OR IGNORE INTO candidate(project, session, kind, text, created_at) VALUES (?1, ?2, ?3, ?4, {NOW})"),
            params![project, session, c.kind, c.text],
        )?;
    }
    // The ones nobody looked at expire.
    db.execute(&format!("DELETE FROM candidate WHERE julianday({NOW}) - julianday(created_at) > {DIAS}"), [])?;
    Ok(n)
}

#[derive(Clone, Debug, Serialize)]
pub struct Apartada {
    pub id: i64,
    pub project: String,
    pub session: String,
    pub kind: String,
    pub text: String,
    pub created_at: String,
    pub shown_at: Option<String>,
}

fn from_row(f: &rusqlite::Row) -> rusqlite::Result<Apartada> {
    Ok(Apartada { id: f.get(0)?, project: f.get(1)?, session: f.get(2)?, kind: f.get(3)?, text: f.get(4)?, created_at: f.get(5)?, shown_at: f.get(6)? })
}

/// When a session starts in `project`: those from previous sessions not shown yet (and they are
/// marked as shown: they don't come out to the agent again, only in the view).
pub fn para_empezar(db: &Connection, project: &str, session: &str) -> Result<Vec<Apartada>> {
    let mut q = db.prepare(&format!(
        "SELECT id, project, session, kind, text, created_at, shown_at FROM candidate
         WHERE project = ?1 AND session != ?2 AND shown_at IS NULL AND julianday({NOW}) - julianday(created_at) <= {DIAS}
         ORDER BY CASE kind WHEN 'preference' THEN 0 WHEN 'decision' THEN 1 ELSE 2 END, id DESC LIMIT ?3"
    ))?;
    let v: Vec<Apartada> = q.query_map(params![project, session, (MAXIMO + 2) as i64], from_row)?.collect::<rusqlite::Result<_>>()?;
    for c in &v {
        db.execute(&format!("UPDATE candidate SET shown_at = {NOW} WHERE id = ?1"), [c.id])?;
    }
    Ok(v)
}

/// All the waiting ones (for the view and the CLI).
pub fn todas(db: &Connection) -> Result<Vec<Apartada>> {
    let mut q = db.prepare(&format!(
        "SELECT id, project, session, kind, text, created_at, shown_at FROM candidate
         WHERE julianday({NOW}) - julianday(created_at) <= {DIAS} ORDER BY id DESC"
    ))?;
    let v = q.query_map([], from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn descartar(db: &Connection, id: i64) -> Result<()> {
    db.execute("DELETE FROM candidate WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets() {
        assert!(parece_secreto("export GH=ghp_abcdefghijklmnopqrstuvwxyz0123"));
        assert!(parece_secreto("echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s"));
        assert!(parece_secreto("password: hunter2"));
        assert!(!parece_secreto("Me gusta mucho la 1C, pero no la veo como portada"));
        assert!(!parece_secreto("D:\\Omoikane\\src\\recall.rs y potion-multilingual-128M-int8"));
    }

    #[test]
    fn question_and_answers() {
        assert_eq!(last_question("Hecho.\n\nTe propongo tres. ¿Cuál te gusta? Dímelo."), Some("¿Cuál te gusta?".into()));
        assert_eq!(last_question("Todo listo, sin preguntas."), None);
        let r = parse_answers("The user answered: \"¿Qué estilo quieres?\"=\"el actual mejorado\", \"¿Y el alcance?\"=\"todo\". Read the answers");
        assert_eq!(r, vec![("¿Qué estilo quieres?".into(), "el actual mejorado".into()), ("¿Y el alcance?".into(), "todo".into())]);
    }
}
