//! `omoi setup <agent>` for the agents that aren't Claude Code: registers the MCP server, the recall
//! hooks where the agent supports them and a paragraph in its instructions file. Each change is
//! computed beforehand (to show it or do a dry run), doesn't repeat what's already there and keeps a copy
//! of the original file (`.antes-de-omoikane`) the first time it touches it.
//!
//! What each one supports (official documentation, October 2026):
//! - Codex: MCP in `~/.codex/config.toml`, hooks like Claude Code's in `~/.codex/hooks.json`
//!   (they have to be approved once with `/hooks`), instructions in `~/.codex/AGENTS.md`.
//! - Gemini CLI: MCP and hooks (`SessionStart`, `BeforeAgent`) in `~/.gemini/settings.json`,
//!   instructions in `~/.gemini/GEMINI.md`.
//! - Cursor: MCP in `~/.cursor/mcp.json`; its `sessionStart` hook can add context, the per-prompt
//!   one can't, so the recall only goes at the start.
//! - OpenCode: MCP in `opencode.json`; it has no command hooks, but TypeScript plugins: a
//!   small one passes each prompt through `omoi recall`. Instructions in `AGENTS.md`.
//! - Qwen Code (derived from Gemini CLI): MCP and hooks like Claude Code's in `~/.qwen/settings.json`,
//!   instructions in `~/.qwen/QWEN.md`.
//! - Kiro: MCP in `~/.kiro/settings/mcp.json`, instructions in `~/.kiro/steering/`, hooks in
//!   `~/.kiro/hooks/` (their output is plain text and the prompt arrives in `USER_PROMPT`).
//! - Copilot (CLI and VS Code, which reads the same files): MCP in `~/.copilot/mcp-config.json`,
//!   instructions in `~/.copilot/copilot-instructions.md`, `SessionStart` hook in
//!   `~/.copilot/hooks/` with `additionalContext` at the very top. The per-prompt one doesn't: its documentation
//!   says that output is discarded.
//! - Antigravity CLI: MCP in `~/.gemini/config/mcp_config.json`, instructions in
//!   `~/.gemini/config/AGENTS.md`, `SessionStart` hook in `~/.gemini/config/hooks.json`
//!   (`injectSteps`); it has no prompt event.
//! - Grok Build (`grok`): MCP in `~/.grok/config.toml`, instructions in `~/.grok/rules/omoikane.md`
//!   (it loads them in every project), commands in `~/.grok/commands/*.md` and hooks in
//!   `~/.grok/hooks/`. Its `SessionStart` and its `UserPromptSubmit` discard the output, so no
//!   automatic recall; `SessionEnd` and `PreCompact` do work for setting aside at the end.
//! - Windsurf (Devin Desktop), Kimi Code, Pi, Amp and Kilo Code: MCP and instructions. Their hooks can't
//!   add context (or it isn't documented), so no automatic recall.
//!
//! When the session closes or compacts (Phase 7.5), `omoi recall --session-end` sets aside candidates from the
//! conversation: Codex (`SessionEnd` with `timeout: 3`, which defaults to 1 s, and `PreCompact`), Gemini
//! (`SessionEnd`, `PreCompress`), Qwen (`SessionEnd`, `PreCompact`), Cursor (`sessionEnd`,
//! `preCompact`), Copilot (`SessionEnd`, with no path: `omoi` finds it by the id), Kiro (`Stop`, only
//! with the id) and Grok (`SessionEnd`, `PreCompact`, also only with the id). In OpenCode, the plugin (with `session.idle` every 10 min and `session.compacted`).
//! Antigravity doesn't: it stores conversations in protobuf.
//!
//! The `/omoikane` (saves what was learned) and `/omoikane-web` (opens the web view) commands, where there are documented user commands: Gemini
//! (`~/.gemini/commands/*.toml`), Qwen (`~/.qwen/commands/*.md`), Codex (`~/.codex/prompts/*.md`,
//! deprecated in favour of skills but they work, as `/prompts:omoikane`), Cursor
//! (`~/.cursor/commands/*.md`), OpenCode (`~/.config/opencode/commands/*.md`), Kiro
//! (`~/.kiro/prompts/*.md`), Windsurf (`~/.codeium/windsurf/global_workflows/*.md`), Pi
//! (`~/.pi/agent/prompts/*.md`), Kilo (`~/.config/kilo/commands/*.md`) and Grok (`~/.grok/commands/*.md`). Without it: Copilot (there is no
//! documented user folder), Kimi (only skills and plugins), Amp (removed commands in favour of
//! skills) and Antigravity (its workflows stopped working). In all of them it is a text for the model,
//! which starts `omoi serve --open` with its terminal.

use crate::tr;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const AGENTES: &[&str] = &["codex", "gemini", "cursor", "opencode", "qwen", "kiro", "copilot", "windsurf", "antigravity", "kimi", "pi", "amp", "kilo", "grok"];

const BEGIN_MARK: &str = "<!-- omoikane -->";
const END_MARK: &str = "<!-- /omoikane -->";
const INSTRUCTIONS: &str = "## Long-term memory (Omoikane)\n\nYou have the `omoikane` memory tools (MCP). At the start of a session, call `context`. Before working on something, `search` it. When you learn something worth knowing next time (a non-obvious trap, a decision and its reasons, how the user wants things done), save it with `remember`, short and in the user's language. If `remember` says it's a duplicate, `update` that memory instead. When a memory changes what you do, say so in one line with its id.";

/// A file that changes: which one, why and how it ends up.
#[derive(Debug)]
pub struct Cambio {
    pub fichero: PathBuf,
    pub que: String,
    pub contenido: String,
    /// Delete the file instead of writing it (the command with its old name).
    pub borrar: bool,
}

/// The home folder; `OMOIKANE_SETUP_HOME` changes it (for the tests).
fn home() -> Result<PathBuf> {
    let h = match std::env::var_os("OMOIKANE_SETUP_HOME") {
        Some(h) => PathBuf::from(h),
        None => std::env::home_dir().context(tr!("can't find the home folder", "no se encuentra la carpeta personal"))?,
    };
    // A network home folder isn't looked at (see `proyecto::ruta_local`).
    crate::proyecto::exigir_local(&h)?;
    Ok(h)
}

/// A folder from an environment variable (`XDG_CONFIG_HOME`, `GROK_HOME`, `APPDATA`), if it's set, isn't
/// overridden by `OMOIKANE_SETUP_HOME` (the tests) and is on this computer.
fn from_env_var(v: &str) -> Option<PathBuf> {
    std::env::var_os(v).filter(|x| !x.is_empty() && std::env::var_os("OMOIKANE_SETUP_HOME").is_none()).map(PathBuf::from).and_then(crate::proyecto::local)
}

/// The binary that gets registered: the one running, with forward slashes (valid in Windows
/// and Unix shells) and quoted in the hook commands.
fn binary_path() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().replace('\\', "/"))
}

/// Which shell runs the agent's hooks on Windows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// Gemini CLI and Codex use PowerShell on Windows.
    Powershell,
    /// Not documented (Cursor).
    Unknown,
}

/// The binary at the start of a hook command. Without spaces, as is: valid in any shell.
/// With spaces it has to be quoted, and in PowerShell a quoted string followed by
/// arguments isn't run: it needs the `&` operator in front (which isn't valid in bash).
fn command_prefix(omoi: &str, shell: Shell) -> String {
    if !omoi.contains(' ') {
        omoi.to_string()
    } else if cfg!(windows) && shell == Shell::Powershell {
        format!("& \"{omoi}\"")
    } else {
        format!("\"{omoi}\"")
    }
}

/// For a field only PowerShell uses (Copilot has one for bash and another for PowerShell).
fn powershell_prefix(omoi: &str) -> String {
    if omoi.contains(' ') { format!("& \"{omoi}\"") } else { omoi.to_string() }
}

fn read_json(f: &Path) -> Result<Value> {
    if !f.exists() {
        return Ok(json!({}));
    }
    let t = std::fs::read_to_string(f)?;
    // PowerShell saves UTF-8 with a BOM.
    let t = t.strip_prefix('\u{feff}').unwrap_or(&t);
    if t.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(t).with_context(|| {
        tr!(
            "{} isn't plain JSON (comments aren't supported): fix it or move it away first",
            "{} no es JSON normal (no se admiten comentarios): arréglalo o apártalo antes",
            f.display()
        )
    })
}

fn object_at<'a>(v: &'a mut Value, key: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    if !v.get(key).is_some_and(Value::is_object) {
        v[key] = json!({});
    }
    v[key].as_object_mut().context("objeto")
}

/// Adds a hook entry (`{matcher?, hooks:[{type, command}]}`) if no hook of the event
/// already runs `omoi … recall`.
fn claude_style_hook(root: &mut Value, event: &str, matcher: Option<&str>, command: &str) -> Result<bool> {
    let hooks = object_at(root, "hooks")?;
    let list = hooks.entry(event).or_insert_with(|| json!([]));
    let Some(list) = list.as_array_mut() else { bail!("hooks.{event} isn't a list") };
    if list.iter().any(|e| e.to_string().contains("recall") && e.to_string().contains("omoi")) {
        return Ok(false);
    }
    let mut entry = json!({ "hooks": [{ "type": "command", "command": command }] });
    if let Some(m) = matcher {
        entry["matcher"] = json!(m);
    }
    list.push(entry);
    Ok(true)
}

/// Like `claude_style_hook`, with a timeout in seconds.
fn timed_hook(root: &mut Value, event: &str, matcher: Option<&str>, command: &str, secs: u64) -> Result<bool> {
    let added = claude_style_hook(root, event, matcher, command)?;
    if added && let Some(e) = root["hooks"][event].as_array_mut().and_then(|l| l.last_mut()) {
        e["hooks"][0]["timeout"] = json!(secs);
    }
    Ok(added)
}

/// The change to a JSON file, if `f` changes something.
fn json_change(file: PathBuf, what: String, f: &dyn Fn(&mut Value) -> Result<bool>) -> Result<Option<Cambio>> {
    let mut j = read_json(&file)?;
    Ok(if f(&mut j)? { Some(Cambio { contenido: pretty(&j)?, fichero: file, que: what, borrar: false }) } else { None })
}

fn pretty(v: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)? + "\n")
}

/// The instructions paragraph, between marks so it can be changed or removed.
fn with_instructions(f: &Path) -> Result<Option<String>> {
    let current = if f.exists() { std::fs::read_to_string(f)? } else { String::new() };
    if current.contains(BEGIN_MARK) {
        return Ok(None);
    }
    let sep = if current.is_empty() || current.ends_with("\n\n") { "" } else if current.ends_with('\n') { "\n" } else { "\n\n" };
    Ok(Some(format!("{current}{sep}{BEGIN_MARK}\n{INSTRUCTIONS}\n{END_MARK}\n")))
}

/// What would have to change for `agent` (empty if everything is done).
pub fn plan(agent: &str) -> Result<Vec<Cambio>> {
    let h = home()?;
    let omoi = binary_path()?;
    let mut v = Vec::new();
    match agent {
        "codex" => {
            let d = h.join(".codex");
            if let Some(c) = json_change(d.join("hooks.json"), tr!("recall hooks (approve them once with /hooks)", "hooks del recall (apruébalos una vez con /hooks)"), &|j| {
                let a = claude_style_hook(j, "SessionStart", Some("startup|resume|clear|compact"), &format!("{} recall --session-start", command_prefix(&omoi, Shell::Powershell)))?;
                let b = claude_style_hook(j, "UserPromptSubmit", None, &format!("{} recall", command_prefix(&omoi, Shell::Powershell)))?;
                // On close (1 s by default in Codex: the maximum, 3, is requested) and on compaction (7.5).
                let on_close = format!("{} recall --session-end", command_prefix(&omoi, Shell::Powershell));
                let c = timed_hook(j, "SessionEnd", None, &on_close, 3)?;
                let e = timed_hook(j, "PreCompact", None, &on_close, 10)?;
                Ok(a || b || c || e)
            })? {
                v.push(c);
            }
            mcp_toml(&mut v, d.join("config.toml"), &omoi)?;
            if let Some(c) = with_instructions(&d.join("AGENTS.md"))? {
                v.push(Cambio { fichero: d.join("AGENTS.md"), que: tr!("instructions", "instrucciones"), contenido: c, borrar: false });
            }
            // Codex shows them as `/prompts:omoikane` and `/prompts:omoikane-web`.
            commands_in(&mut v, d.join("prompts"), Format::Md(true), &omoi);
        }
        "gemini" => {
            let d = h.join(".gemini");
            if let Some(c) = json_change(d.join("settings.json"), tr!("MCP server and recall hooks", "servidor MCP y hooks del recall"), &|j| {
                let servers = object_at(j, "mcpServers")?;
                let a = !servers.contains_key("omoikane");
                if a {
                    servers.insert("omoikane".into(), json!({ "command": omoi, "args": ["mcp"] }));
                }
                let b = claude_style_hook(j, "SessionStart", Some("*"), &format!("{} recall --session-start --format gemini", command_prefix(&omoi, Shell::Powershell)))?;
                let c = claude_style_hook(j, "BeforeAgent", Some("*"), &format!("{} recall --format gemini", command_prefix(&omoi, Shell::Powershell)))?;
                let on_close = format!("{} recall --session-end", command_prefix(&omoi, Shell::Powershell));
                let e = claude_style_hook(j, "SessionEnd", Some("*"), &on_close)?;
                let f = claude_style_hook(j, "PreCompress", Some("*"), &on_close)?;
                Ok(a || b || c || e || f)
            })? {
                v.push(c);
            }
            if let Some(c) = with_instructions(&d.join("GEMINI.md"))? {
                v.push(Cambio { fichero: d.join("GEMINI.md"), que: tr!("instructions", "instrucciones"), contenido: c, borrar: false });
            }
            commands_in(&mut v, d.join("commands"), Format::Toml, &omoi);
        }
        "cursor" => {
            let d = h.join(".cursor");
            if let Some(c) = json_change(d.join("mcp.json"), tr!("MCP server", "servidor MCP"), &|j| {
                let servers = object_at(j, "mcpServers")?;
                if servers.contains_key("omoikane") {
                    return Ok(false);
                }
                servers.insert("omoikane".into(), json!({ "type": "stdio", "command": omoi, "args": ["mcp"] }));
                Ok(true)
            })? {
                v.push(c);
            }
            if let Some(c) = json_change(d.join("hooks.json"), tr!("recall at session start (Cursor can't add context per prompt), and setting aside at the end", "recall al empezar la sesión (Cursor no deja añadir contexto en cada mensaje) y apartar al cerrar"), &|j| {
                if j.get("version").is_none() {
                    j["version"] = json!(1);
                }
                let hooks = object_at(j, "hooks")?;
                let o = command_prefix(&omoi, Shell::Unknown);
                let mut changed = false;
                for (event, command) in [
                    ("sessionStart", format!("{o} recall --session-start --format cursor")),
                    ("sessionEnd", format!("{o} recall --session-end")),
                    ("preCompact", format!("{o} recall --session-end")),
                ] {
                    let list = hooks.entry(event).or_insert_with(|| json!([]));
                    let Some(list) = list.as_array_mut() else { bail!("hooks.{event} isn't a list") };
                    if !list.iter().any(|e| e.to_string().contains("recall")) {
                        list.push(json!({ "command": command }));
                        changed = true;
                    }
                }
                Ok(changed)
            })? {
                v.push(c);
            }
            commands_in(&mut v, d.join("commands"), Format::Md(false), &omoi);
        }
        "opencode" => {
            let d = from_env_var("XDG_CONFIG_HOME").map_or_else(|| h.join(".config").join("opencode"), |x| x.join("opencode"));
            if let Some(c) = json_change(d.join("opencode.json"), tr!("MCP server", "servidor MCP"), &|j| {
                let servers = object_at(j, "mcp")?;
                if servers.contains_key("omoikane") {
                    return Ok(false);
                }
                servers.insert("omoikane".into(), json!({ "type": "local", "command": [omoi, "mcp"], "enabled": true }));
                Ok(true)
            })? {
                v.push(c);
            }
            let plugin = d.join("plugins").join("omoikane.ts");
            let new_plugin = opencode_plugin(&omoi);
            let current = std::fs::read_to_string(&plugin).ok();
            // Only if there's none or it's an earlier one from Omoikane (it carries its header): someone else's isn't touched.
            if current.as_deref().is_none_or(|t| t.starts_with("// Omoikane: recall automático en OpenCode") && t != new_plugin) {
                v.push(Cambio { fichero: plugin, que: tr!("plugin: recall on each prompt and setting aside at the end", "plugin: recall en cada mensaje y apartar al cerrar"), contenido: new_plugin, borrar: false });
            }
            if let Some(c) = with_instructions(&d.join("AGENTS.md"))? {
                v.push(Cambio { fichero: d.join("AGENTS.md"), que: tr!("instructions", "instrucciones"), contenido: c, borrar: false });
            }
            commands_in(&mut v, d.join("commands"), Format::Md(true), &omoi);
        }
        "qwen" => {
            let d = h.join(".qwen");
            if let Some(c) = json_change(d.join("settings.json"), tr!("MCP server and recall hooks", "servidor MCP y hooks del recall"), &|j| {
                let servers = object_at(j, "mcpServers")?;
                let a = !servers.contains_key("omoikane");
                if a {
                    servers.insert("omoikane".into(), json!({ "command": omoi, "args": ["mcp"] }));
                }
                let b = claude_style_hook(j, "SessionStart", Some("startup|resume|clear|compact"), &format!("{} recall --session-start", command_prefix(&omoi, Shell::Powershell)))?;
                let c = claude_style_hook(j, "UserPromptSubmit", None, &format!("{} recall", command_prefix(&omoi, Shell::Powershell)))?;
                let on_close = format!("{} recall --session-end", command_prefix(&omoi, Shell::Powershell));
                let e = claude_style_hook(j, "SessionEnd", None, &on_close)?;
                let f = claude_style_hook(j, "PreCompact", None, &on_close)?;
                Ok(a || b || c || e || f)
            })? {
                v.push(c);
            }
            instructions_in(&mut v, d.join("QWEN.md"))?;
            commands_in(&mut v, d.join("commands"), Format::Md(true), &omoi);
        }
        "kiro" => {
            let d = h.join(".kiro");
            mcp_in(&mut v, d.join("settings").join("mcp.json"), "mcpServers", json!({ "command": omoi, "args": ["mcp"] }))?;
            instructions_in(&mut v, d.join("steering").join("omoikane.md"))?;
            let o = command_prefix(&omoi, Shell::Unknown);
            let all_hooks = [
                json!({ "name": "omoikane-session", "trigger": "SessionStart", "action": { "type": "command", "command": format!("{o} recall --session-start --format plain") }, "timeout": 30 }),
                json!({ "name": "omoikane-prompt", "trigger": "UserPromptSubmit", "action": { "type": "command", "command": format!("{o} recall --format plain") }, "timeout": 30 }),
                // Kiro only gives `session_id`: `omoi` looks for the conversation in ~/.kiro/sessions/cli (7.5).
                json!({ "name": "omoikane-end", "trigger": "Stop", "action": { "type": "command", "command": format!("{o} recall --session-end") }, "timeout": 30 }),
            ];
            if let Some(c) = json_change(d.join("hooks").join("omoikane.json"), tr!("recall hooks and setting aside at the end", "hooks del recall y apartar al cerrar"), &|j| {
                if j.get("version").is_none() {
                    j["version"] = json!("v1");
                }
                if !j.get("hooks").is_some_and(Value::is_array) {
                    j["hooks"] = json!([]);
                }
                let list = j["hooks"].as_array_mut().context("hooks")?;
                let mut changed = false;
                for h in &all_hooks {
                    if !list.iter().any(|x| x["name"] == h["name"]) {
                        list.push(h.clone());
                        changed = true;
                    }
                }
                Ok(changed)
            })? {
                v.push(c);
            }
            commands_in(&mut v, d.join("prompts"), Format::Md(false), &omoi);
        }
        "copilot" => {
            let d = h.join(".copilot");
            mcp_in(&mut v, d.join("mcp-config.json"), "mcpServers", json!({ "type": "local", "command": omoi, "args": ["mcp"], "tools": ["*"] }))?;
            instructions_in(&mut v, d.join("copilot-instructions.md"))?;
            let hook = |args: &str| {
                json!([{
                    "type": "command",
                    "bash": format!("{} {args}", command_prefix(&omoi, Shell::Unknown)),
                    "powershell": format!("{} {args}", powershell_prefix(&omoi)),
                    "timeoutSec": 30,
                }])
            };
            let (start, end) = (hook("recall --session-start --format copilot"), hook("recall --session-end"));
            if let Some(c) = json_change(d.join("hooks").join("omoikane.json"), tr!("recall at session start and setting aside at the end", "recall al empezar la sesión y apartar al cerrar"), &|j| {
                if j.get("version").is_none() {
                    j["version"] = json!(1);
                }
                let hooks = object_at(j, "hooks")?;
                let mut changed = false;
                for (event, h) in [("SessionStart", &start), ("SessionEnd", &end)] {
                    if !hooks.contains_key(event) {
                        hooks.insert(event.into(), h.clone());
                        changed = true;
                    }
                }
                Ok(changed)
            })? {
                v.push(c);
            }
        }
        "windsurf" => {
            let entry = json!({ "command": omoi, "args": ["mcp"] });
            // The current documentation (Devin Desktop) and Windsurf's usual path: both.
            mcp_in(&mut v, system_config_dir(&h).join("devin").join("mcp_config.json"), "mcpServers", entry.clone())?;
            mcp_in(&mut v, h.join(".codeium").join("windsurf").join("mcp_config.json"), "mcpServers", entry)?;
            instructions_in(&mut v, h.join(".codeium").join("windsurf").join("memories").join("global_rules.md"))?;
            commands_in(&mut v, h.join(".codeium").join("windsurf").join("global_workflows"), Format::Md(true), &omoi);
        }
        "antigravity" => {
            let d = h.join(".gemini").join("config");
            mcp_in(&mut v, d.join("mcp_config.json"), "mcpServers", json!({ "command": omoi, "args": ["mcp"] }))?;
            instructions_in(&mut v, d.join("AGENTS.md"))?;
            if let Some(c) = json_change(d.join("hooks.json"), tr!("recall at session start", "recall al empezar la sesión"), &|j| {
                if j.get("omoikane").is_some() {
                    return Ok(false);
                }
                j["omoikane"] = json!({ "enabled": true, "SessionStart": [{
                    "command": format!("{} recall --session-start --format antigravity", command_prefix(&omoi, Shell::Unknown)),
                    "timeout": 30,
                }]});
                Ok(true)
            })? {
                v.push(c);
            }
        }
        "kimi" => {
            mcp_in(&mut v, h.join(".kimi-code").join("mcp.json"), "mcpServers", json!({ "command": omoi, "args": ["mcp"] }))?;
            instructions_in(&mut v, h.join(".kimi-code").join("AGENTS.md"))?;
        }
        "pi" => {
            let d = h.join(".pi").join("agent");
            mcp_in(&mut v, d.join("mcp.json"), "mcpServers", json!({ "command": omoi, "args": ["mcp"] }))?;
            instructions_in(&mut v, d.join("AGENTS.md"))?;
            commands_in(&mut v, d.join("prompts"), Format::Md(true), &omoi);
        }
        "amp" => {
            let d = h.join(".config").join("amp");
            mcp_in(&mut v, d.join("settings.json"), "amp.mcpServers", json!({ "command": omoi, "args": ["mcp"] }))?;
            instructions_in(&mut v, d.join("AGENTS.md"))?;
        }
        "kilo" => {
            let d = h.join(".config").join("kilo");
            let rules = d.join("omoikane.md");
            let rules_path = rules.to_string_lossy().replace('\\', "/");
            if let Some(c) = json_change(d.join("kilo.jsonc"), tr!("MCP server and instructions", "servidor MCP e instrucciones"), &|j| {
                let servers = object_at(j, "mcp")?;
                let a = !servers.contains_key("omoikane");
                if a {
                    servers.insert("omoikane".into(), json!({ "type": "local", "command": [omoi, "mcp"], "enabled": true }));
                }
                if !j.get("instructions").is_some_and(Value::is_array) {
                    j["instructions"] = json!([]);
                }
                let list = j["instructions"].as_array_mut().context("instructions")?;
                let b = !list.iter().any(|x| x.as_str() == Some(rules_path.as_str()));
                if b {
                    list.push(json!(rules_path));
                }
                Ok(a || b)
            })? {
                v.push(c);
            }
            instructions_in(&mut v, rules)?;
            commands_in(&mut v, d.join("commands"), Format::Md(true), &omoi);
        }
        "grok" => {
            let d = grok_folder(&h);
            mcp_toml(&mut v, d.join("config.toml"), &omoi)?;
            instructions_in(&mut v, d.join("rules").join("omoikane.md"))?;
            // On close (1.5 s by default in Grok) and on compaction (7.5); on Windows it runs them with PowerShell.
            let on_close = format!("{} recall --session-end", command_prefix(&omoi, Shell::Powershell));
            if let Some(c) = json_change(d.join("hooks").join("omoikane.json"), tr!("setting aside at the end of the session", "apartar al cerrar la sesión"), &|j| {
                let a = timed_hook(j, "SessionEnd", None, &on_close, 3)?;
                let b = timed_hook(j, "PreCompact", None, &on_close, 10)?;
                Ok(a || b)
            })? {
                v.push(c);
            }
            commands_in(&mut v, d.join("commands"), Format::Md(true), &omoi);
        }
        other => bail!(tr!(
            "unknown agent «{other}»: claude, {}",
            "agente «{other}» desconocido: claude, {}",
            AGENTES.join(", ")
        )),
    }
    Ok(v)
}

/// The agents that already have Omoikane on this computer: their MCP file names it. The automatic
/// update adds to them whatever the new version brings (7.6).
pub fn configurados() -> Vec<&'static str> {
    let Ok(h) = home() else { return Vec::new() };
    let opencode = from_env_var("XDG_CONFIG_HOME").map_or_else(|| h.join(".config").join("opencode"), |x| x.join("opencode"));
    let files: [(&str, Vec<PathBuf>); 14] = [
        ("codex", vec![h.join(".codex").join("config.toml")]),
        ("gemini", vec![h.join(".gemini").join("settings.json")]),
        ("cursor", vec![h.join(".cursor").join("mcp.json")]),
        ("opencode", vec![opencode.join("opencode.json")]),
        ("qwen", vec![h.join(".qwen").join("settings.json")]),
        ("kiro", vec![h.join(".kiro").join("settings").join("mcp.json")]),
        ("copilot", vec![h.join(".copilot").join("mcp-config.json")]),
        ("windsurf", vec![h.join(".codeium").join("windsurf").join("mcp_config.json"), system_config_dir(&h).join("devin").join("mcp_config.json")]),
        ("antigravity", vec![h.join(".gemini").join("config").join("mcp_config.json")]),
        ("kimi", vec![h.join(".kimi-code").join("mcp.json")]),
        ("pi", vec![h.join(".pi").join("agent").join("mcp.json")]),
        ("amp", vec![h.join(".config").join("amp").join("settings.json")]),
        ("kilo", vec![h.join(".config").join("kilo").join("kilo.jsonc")]),
        ("grok", vec![grok_folder(&h).join("config.toml")]),
    ];
    files
        .into_iter()
        .filter(|(_, fs)| fs.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|t| t.contains("omoikane"))))
        .map(|(a, _)| a)
        .collect()
}

/// Grok Build's folder: `GROK_HOME` or `~/.grok`.
fn grok_folder(h: &Path) -> PathBuf {
    from_env_var("GROK_HOME").unwrap_or_else(|| h.join(".grok"))
}

/// The MCP server in a `config.toml` with `[mcp_servers.<name>]` (Codex and Grok).
fn mcp_toml(v: &mut Vec<Cambio>, toml: PathBuf, omoi: &str) -> Result<()> {
    let current = if toml.exists() { std::fs::read_to_string(&toml)? } else { String::new() };
    // Also if it's already there as an inline table (`omoikane = {…}` inside `[mcp_servers]`):
    // repeating it would leave the file unreadable.
    let already = current.contains("[mcp_servers.omoikane]") || current.lines().any(|l| l.trim_start().starts_with("omoikane") && l.contains('='));
    if !already {
        let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
        let content = format!("{current}{sep}\n[mcp_servers.omoikane]\ncommand = {}\nargs = [\"mcp\"]\n", toml_string(omoi));
        v.push(Cambio { fichero: toml, que: tr!("MCP server", "servidor MCP"), contenido: content, borrar: false });
    }
    Ok(())
}

/// Registers the MCP server as `entry` under `root` (`mcpServers`, `mcp`, `amp.mcpServers`…).
fn mcp_in(v: &mut Vec<Cambio>, file: PathBuf, root: &str, entry: Value) -> Result<()> {
    let root = root.to_string();
    if let Some(c) = json_change(file, tr!("MCP server", "servidor MCP"), &move |j| {
        let servers = object_at(j, &root)?;
        if servers.contains_key("omoikane") {
            return Ok(false);
        }
        servers.insert("omoikane".into(), entry.clone());
        Ok(true)
    })? {
        v.push(c);
    }
    Ok(())
}

fn instructions_in(v: &mut Vec<Cambio>, file: PathBuf) -> Result<()> {
    if let Some(c) = with_instructions(&file)? {
        v.push(Cambio { fichero: file, que: tr!("instructions", "instrucciones"), contenido: c, borrar: false });
    }
    Ok(())
}

/// The name of the command that opens the web view. Up to 0.9 it was `/omoikane`, which sounded like
/// «Omoikane, note this down»: since 0.15 that name is the command that saves.
pub const ORDEN: &str = "omoikane-web";
/// The command that makes the agent save what it learned in the session.
pub const ORDEN_GUARDAR: &str = "omoikane";

/// What the `/omoikane-web` command does: in these agents a command is a text for the model, so it
/// asks it to start the web view with its terminal.
fn web_command_text(omoi: &str) -> String {
    let o = if omoi.contains(' ') { format!("\"{omoi}\"") } else { omoi.to_string() };
    format!(
        "Open the Omoikane memory view in the browser: run `{o} serve --open` in the background with your shell tool. It keeps serving while the session lasts, so don't wait for it to finish; if a server is already running, it just opens the browser. Then tell the user the view is at http://127.0.0.1:7438/ (all their memories: search, filters, history, pin, move, forget)."
    )
}

const WEB_COMMAND_DESCRIPTION: &str = "Open the Omoikane memory view in the browser";

/// What the `/omoikane` command does. Whatever the user writes after it reaches the model with the text
/// (agents append it at the end when the command doesn't place it), so `$ARGUMENTS` isn't needed.
/// It's the same text the Claude Code mod sends (`hooks/omoikane.ts`).
pub const TEXTO_GUARDAR: &str = "Save to Omoikane now, with its `remember` and `update` tools. If the user wrote something after the command, that is what to save: one memory, in their words. Otherwise go over this session and save what is worth knowing next time and is not saved yet: decisions and their reasons (kind decision), non-obvious traps with the files they affect (gotcha), how the user wants things done (preference), steps they repeat (procedure), and how something works (fact); a decision that is the user's to make and still unanswered goes as kind decision with the tag `open` (the question, the options, what you assumed meanwhile), plus the tag `blocking` if the work can't go on without the answer; a trap of the stack itself that would help any project of the same trade (android, web, wordpress…), with nothing of this project or its client in it, goes with `trade` instead of in the project. `search` first, and `update` a memory instead of saving a near copy. Then update the project's memory tagged `status` (or save one) with where the work was left: what is done and what is next. Write each memory in the language the user writes to you in. Finish with one short line per memory saved or updated, or say that nothing new was worth saving.";

const SAVE_COMMAND_DESCRIPTION: &str = "Save to Omoikane what this session learned (or what you write after it)";

/// The format each agent's commands are written in: Markdown (with `description` at the top, if supported)
/// or Gemini's and Qwen's TOML.
enum Format {
    Md(bool),
    Toml,
}

fn command_file(fmt: &Format, description: &str, body: &str) -> String {
    match fmt {
        Format::Md(true) => format!("---\ndescription: {description}\n---\n\n{body}\n"),
        Format::Md(false) => format!("{body}\n"),
        Format::Toml => format!("description = {}\nprompt = {}\n", toml_string(description), toml_string(body)),
    }
}

/// The two commands in `folder`, if they don't exist yet: `/omoikane` saves and `/omoikane-web` opens the web
/// view. A `/omoikane` from before 0.15, which opened the web view and was written by Omoikane, is replaced
/// by the one that saves; one the user wrote isn't touched.
fn commands_in(v: &mut Vec<Cambio>, folder: PathBuf, fmt: Format, omoi: &str) {
    let ext = if matches!(fmt, Format::Toml) { "toml" } else { "md" };
    let web = command_file(&fmt, WEB_COMMAND_DESCRIPTION, &web_command_text(omoi));
    own_file(v, folder.join(format!("{ORDEN}.{ext}")), tr!("/omoikane-web command (opens the web view)", "orden /omoikane-web (abre la vista web)"), web);
    let file = folder.join(format!("{ORDEN_GUARDAR}.{ext}"));
    let save = command_file(&fmt, SAVE_COMMAND_DESCRIPTION, TEXTO_GUARDAR);
    if std::fs::read_to_string(&file).is_ok_and(|t| t.contains(WEB_COMMAND_DESCRIPTION)) {
        let what = tr!("/omoikane command: now it saves (it used to open the web view)", "orden /omoikane: ahora guarda (antes abría la vista web)");
        v.push(Cambio { fichero: file, que: what, contenido: save, borrar: false });
    } else {
        own_file(v, file, tr!("/omoikane command (saves what the session learned)", "orden /omoikane (guarda lo aprendido en la sesión)"), save);
    }
}

/// A whole file that belongs only to Omoikane (if it doesn't exist yet).
fn own_file(v: &mut Vec<Cambio>, file: PathBuf, what: String, content: String) {
    if !file.exists() {
        v.push(Cambio { fichero: file, que: what, contenido: content, borrar: false });
    }
}

/// The Windows configuration folder (`%APPDATA%`), or `~/.config` on the others.
fn system_config_dir(h: &Path) -> PathBuf {
    if cfg!(windows)
        && let Some(a) = from_env_var("APPDATA")
    {
        return a;
    }
    h.join(".config")
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// OpenCode plugin: with each prompt, `omoi recall` with the user's text; whatever it returns goes
/// into that turn's system prompt (`experimental.chat.system.transform`). Tested with
/// OpenCode 1.19.5: adding it as a new part of the message makes the server fail (its part ids
/// have their own format) and changing the text in `chat.message` no longer reaches the model.
fn opencode_plugin(omoi: &str) -> String {
    format!(
        r#"// Omoikane: recall automático en OpenCode. Lo creó `omoi setup opencode`; bórralo para quitarlo.
const OMOI = {omoi:?}
const pendiente = new Map()

// 7.5: al quedarse la sesión parada (como mucho una vez cada 10 min) y al compactar, la conversación
// pasa a `omoi recall --session-end` como JSONL neutro ({{role, text}}), para apartar lo que valga.
const apartadas = new Map()
async function apartar($, client, directory, id) {{
  try {{
    const r = await client.session.messages({{ path: {{ id }} }})
    const mensajes = Array.isArray(r) ? r : (r?.data ?? [])
    const lineas = mensajes.map((m) => JSON.stringify({{
      role: m.info?.role,
      text: (m.parts ?? []).filter((p) => p.type === "text" && !p.synthetic).map((p) => p.text).join("\n"),
    }}))
    const fichero = `${{process.env.TEMP || process.env.TMPDIR || "/tmp"}}/omoikane-opencode-${{id}}.jsonl`
    await Bun.write(fichero, lineas.join("\n"))
    const evento = JSON.stringify({{ session_id: id, cwd: directory, transcript_path: fichero }})
    await $`${{OMOI}} recall --session-end < ${{new Response(evento)}}`.quiet().nothrow()
  }} catch {{
    // Lo peor es no apartar nada esta vez.
  }}
}}

export const Omoikane = async ({{ $, client, directory }}) => ({{
  event: async ({{ event }}) => {{
    const id = event?.properties?.sessionID ?? event?.properties?.info?.id
    if (!id) return
    const ahora = Date.now()
    if (event.type === "session.compacted" || (event.type === "session.idle" && ahora - (apartadas.get(id) ?? 0) > 10 * 60 * 1000)) {{
      apartadas.set(id, ahora)
      await apartar($, client, directory, id)
    }}
  }},
  "chat.message": async (input, output) => {{
    const prompt = output.parts.filter((p) => p.type === "text" && !p.synthetic).map((p) => p.text).join(" ")
    if (!prompt.trim()) return
    const evento = JSON.stringify({{ session_id: input.sessionID, cwd: directory, prompt }})
    try {{
      const salida = await $`${{OMOI}} recall < ${{new Response(evento)}}`.quiet().nothrow().text()
      const contexto = salida.trim() ? JSON.parse(salida)?.hookSpecificOutput?.additionalContext : undefined
      if (contexto) pendiente.set(input.sessionID, contexto)
      else pendiente.delete(input.sessionID)
    }} catch {{
      // Un recall que falla no debe estorbar al mensaje.
    }}
  }},
  "experimental.chat.system.transform": async (input, output) => {{
    const contexto = input.sessionID && pendiente.get(input.sessionID)
    // Vale para todas las peticiones del turno (OpenCode pide también el título de la sesión, y
    // las vueltas con herramientas); el mensaje siguiente lo sustituye.
    // Se junta al último bloque: con algunos proveedores, un bloque de sistema añadido aparte no le
    // llegaba al modelo.
    if (contexto && output.system.length) output.system[output.system.length - 1] += "\n\n" + contexto
    else if (contexto) output.system.push(contexto)
  }},
}})
"#
    )
}

/// Writes the changes, with a copy of the original the first time.
pub fn aplicar(changes: &[Cambio]) -> Result<()> {
    for c in changes {
        if c.borrar {
            std::fs::remove_file(&c.fichero).with_context(|| format!("{}", c.fichero.display()))?;
            continue;
        }
        if let Some(d) = c.fichero.parent() {
            std::fs::create_dir_all(d)?;
        }
        if c.fichero.exists() {
            let backup = c.fichero.with_file_name(format!("{}.antes-de-omoikane", c.fichero.file_name().unwrap_or_default().to_string_lossy()));
            if !backup.exists() {
                std::fs::copy(&c.fichero, &backup)?;
            }
        }
        std::fs::write(&c.fichero, &c.contenido).with_context(|| format!("{}", c.fichero.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_prefix_per_shell() {
        assert_eq!(command_prefix("C:/x/omoi.exe", Shell::Powershell), "C:/x/omoi.exe");
        assert_eq!(command_prefix("/usr/bin/omoi", Shell::Unknown), "/usr/bin/omoi");
        let with_spaces = command_prefix("C:/Mis cosas/omoi.exe", Shell::Powershell);
        if cfg!(windows) {
            assert_eq!(with_spaces, "& \"C:/Mis cosas/omoi.exe\"");
        } else {
            assert_eq!(with_spaces, "\"C:/Mis cosas/omoi.exe\"");
        }
        assert_eq!(command_prefix("C:/Mis cosas/omoi.exe", Shell::Unknown), "\"C:/Mis cosas/omoi.exe\"");
    }
}
