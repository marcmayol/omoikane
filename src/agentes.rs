//! `omoi setup <agente>` para los agentes que no son Claude Code: registra el servidor MCP, los hooks
//! del recall donde el agente los admite y un párrafo en su fichero de instrucciones. Cada cambio se
//! calcula antes (para enseñarlo o hacer un simulacro), no repite lo que ya está y guarda una copia
//! del fichero original (`.antes-de-omoikane`) la primera vez que lo toca.
//!
//! Lo que admite cada uno (documentación oficial, octubre de 2026):
//! - Codex: MCP en `~/.codex/config.toml`, hooks como los de Claude Code en `~/.codex/hooks.json`
//!   (hay que aprobarlos una vez con `/hooks`), instrucciones en `~/.codex/AGENTS.md`.
//! - Gemini CLI: MCP y hooks (`SessionStart`, `BeforeAgent`) en `~/.gemini/settings.json`,
//!   instrucciones en `~/.gemini/GEMINI.md`.
//! - Cursor: MCP en `~/.cursor/mcp.json`; su hook `sessionStart` puede añadir contexto, el de cada
//!   mensaje no, así que el recall solo va al empezar.
//! - OpenCode: MCP en `opencode.json`; no tiene hooks de comandos, sino plugins en TypeScript: uno
//!   pequeño pasa cada mensaje por `omoi recall`. Instrucciones en `AGENTS.md`.

use crate::tr;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const AGENTES: &[&str] = &["codex", "gemini", "cursor", "opencode"];

const INICIO: &str = "<!-- omoikane -->";
const FIN: &str = "<!-- /omoikane -->";
const INSTRUCCIONES: &str = "## Long-term memory (Omoikane)\n\nYou have the `omoikane` memory tools (MCP). Before working on something, `search` it. When you learn something worth knowing next time (a non-obvious trap, a decision and its reasons, how the user wants things done), save it with `remember`, short and in the user's language. If `remember` says it's a duplicate, `update` that memory instead.";

/// Un fichero que cambia: cuál, por qué y cómo queda.
#[derive(Debug)]
pub struct Cambio {
    pub fichero: PathBuf,
    pub que: String,
    pub contenido: String,
}

/// La carpeta personal; `OMOIKANE_SETUP_HOME` la cambia (para los tests).
fn casa() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("OMOIKANE_SETUP_HOME") {
        return Ok(PathBuf::from(h));
    }
    dirs::home_dir().context(tr!("can't find the home folder", "no se encuentra la carpeta personal"))
}

/// El binario que se registra: el que se está ejecutando, con barras normales (vale en las shells de
/// Windows y de Unix) y entre comillas en los comandos de los hooks.
fn binario() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().replace('\\', "/"))
}

/// Qué shell ejecuta los hooks del agente en Windows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// Gemini CLI y Codex usan PowerShell en Windows.
    Powershell,
    /// No documentado (Cursor).
    Desconocida,
}

/// El binario al principio de un comando de hook. Sin espacios, tal cual: vale en cualquier shell.
/// Con espacios hay que entrecomillarlo, y en PowerShell una cadena entre comillas seguida de
/// argumentos no se ejecuta: hace falta el operador `&` delante (que en bash no vale).
fn orden(omoi: &str, shell: Shell) -> String {
    if !omoi.contains(' ') {
        omoi.to_string()
    } else if cfg!(windows) && shell == Shell::Powershell {
        format!("& \"{omoi}\"")
    } else {
        format!("\"{omoi}\"")
    }
}

fn leer_json(f: &Path) -> Result<Value> {
    if !f.exists() {
        return Ok(json!({}));
    }
    let t = std::fs::read_to_string(f)?;
    // PowerShell guarda UTF-8 con BOM.
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

fn objeto<'a>(v: &'a mut Value, clave: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    if !v.get(clave).is_some_and(Value::is_object) {
        v[clave] = json!({});
    }
    v[clave].as_object_mut().context("objeto")
}

/// Añade una entrada de hook (`{matcher?, hooks:[{type, command}]}`) si ningún hook del evento
/// ejecuta ya `omoi … recall`.
fn hook_estilo_claude(raiz: &mut Value, evento: &str, matcher: Option<&str>, comando: &str) -> Result<bool> {
    let hooks = objeto(raiz, "hooks")?;
    let lista = hooks.entry(evento).or_insert_with(|| json!([]));
    let Some(lista) = lista.as_array_mut() else { bail!("hooks.{evento} isn't a list") };
    if lista.iter().any(|e| e.to_string().contains("recall") && e.to_string().contains("omoi")) {
        return Ok(false);
    }
    let mut entrada = json!({ "hooks": [{ "type": "command", "command": comando }] });
    if let Some(m) = matcher {
        entrada["matcher"] = json!(m);
    }
    lista.push(entrada);
    Ok(true)
}

/// El cambio de un fichero JSON, si `f` cambia algo.
fn json_cambio(fichero: PathBuf, que: String, f: &dyn Fn(&mut Value) -> Result<bool>) -> Result<Option<Cambio>> {
    let mut j = leer_json(&fichero)?;
    Ok(if f(&mut j)? { Some(Cambio { contenido: bonito(&j)?, fichero, que }) } else { None })
}

fn bonito(v: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(v)? + "\n")
}

/// El párrafo de instrucciones, entre marcas para poder cambiarlo o quitarlo.
fn con_instrucciones(f: &Path) -> Result<Option<String>> {
    let actual = if f.exists() { std::fs::read_to_string(f)? } else { String::new() };
    if actual.contains(INICIO) {
        return Ok(None);
    }
    let sep = if actual.is_empty() || actual.ends_with("\n\n") { "" } else if actual.ends_with('\n') { "\n" } else { "\n\n" };
    Ok(Some(format!("{actual}{sep}{INICIO}\n{INSTRUCCIONES}\n{FIN}\n")))
}

/// Lo que habría que cambiar para `agente` (vacío si ya está todo).
pub fn plan(agente: &str) -> Result<Vec<Cambio>> {
    let h = casa()?;
    let omoi = binario()?;
    let mut v = Vec::new();
    match agente {
        "codex" => {
            let d = h.join(".codex");
            if let Some(c) = json_cambio(d.join("hooks.json"), tr!("recall hooks (approve them once with /hooks)", "hooks del recall (apruébalos una vez con /hooks)"), &|j| {
                let a = hook_estilo_claude(j, "SessionStart", Some("startup|resume|clear|compact"), &format!("{} recall --session-start", orden(&omoi, Shell::Powershell)))?;
                let b = hook_estilo_claude(j, "UserPromptSubmit", None, &format!("{} recall", orden(&omoi, Shell::Powershell)))?;
                Ok(a || b)
            })? {
                v.push(c);
            }
            let toml = d.join("config.toml");
            let actual = if toml.exists() { std::fs::read_to_string(&toml)? } else { String::new() };
            // También si ya está como tabla en línea (`omoikane = {…}` dentro de `[mcp_servers]`):
            // repetirla dejaría el fichero sin poder leerse.
            let ya_esta = actual.contains("[mcp_servers.omoikane]")
                || actual.lines().any(|l| l.trim_start().starts_with("omoikane") && l.contains('='));
            if !ya_esta {
                let sep = if actual.is_empty() || actual.ends_with('\n') { "" } else { "\n" };
                let contenido = format!("{actual}{sep}\n[mcp_servers.omoikane]\ncommand = {}\nargs = [\"mcp\"]\n", toml_texto(&omoi));
                v.push(Cambio { fichero: toml, que: tr!("MCP server", "servidor MCP"), contenido });
            }
            if let Some(c) = con_instrucciones(&d.join("AGENTS.md"))? {
                v.push(Cambio { fichero: d.join("AGENTS.md"), que: tr!("instructions", "instrucciones"), contenido: c });
            }
        }
        "gemini" => {
            let d = h.join(".gemini");
            if let Some(c) = json_cambio(d.join("settings.json"), tr!("MCP server and recall hooks", "servidor MCP y hooks del recall"), &|j| {
                let servidores = objeto(j, "mcpServers")?;
                let a = !servidores.contains_key("omoikane");
                if a {
                    servidores.insert("omoikane".into(), json!({ "command": omoi, "args": ["mcp"] }));
                }
                let b = hook_estilo_claude(j, "SessionStart", Some("*"), &format!("{} recall --session-start --format gemini", orden(&omoi, Shell::Powershell)))?;
                let c = hook_estilo_claude(j, "BeforeAgent", Some("*"), &format!("{} recall --format gemini", orden(&omoi, Shell::Powershell)))?;
                Ok(a || b || c)
            })? {
                v.push(c);
            }
            if let Some(c) = con_instrucciones(&d.join("GEMINI.md"))? {
                v.push(Cambio { fichero: d.join("GEMINI.md"), que: tr!("instructions", "instrucciones"), contenido: c });
            }
        }
        "cursor" => {
            let d = h.join(".cursor");
            if let Some(c) = json_cambio(d.join("mcp.json"), tr!("MCP server", "servidor MCP"), &|j| {
                let servidores = objeto(j, "mcpServers")?;
                if servidores.contains_key("omoikane") {
                    return Ok(false);
                }
                servidores.insert("omoikane".into(), json!({ "type": "stdio", "command": omoi, "args": ["mcp"] }));
                Ok(true)
            })? {
                v.push(c);
            }
            if let Some(c) = json_cambio(d.join("hooks.json"), tr!("recall at session start (Cursor can't add context per prompt)", "recall al empezar la sesión (Cursor no deja añadir contexto en cada mensaje)"), &|j| {
                if j.get("version").is_none() {
                    j["version"] = json!(1);
                }
                let hooks = objeto(j, "hooks")?;
                let lista = hooks.entry("sessionStart").or_insert_with(|| json!([]));
                let Some(lista) = lista.as_array_mut() else { bail!("hooks.sessionStart isn't a list") };
                if lista.iter().any(|e| e.to_string().contains("recall")) {
                    return Ok(false);
                }
                lista.push(json!({ "command": format!("{} recall --session-start --format cursor", orden(&omoi, Shell::Desconocida)) }));
                Ok(true)
            })? {
                v.push(c);
            }
        }
        "opencode" => {
            let d = match std::env::var_os("XDG_CONFIG_HOME") {
                Some(x) if std::env::var_os("OMOIKANE_SETUP_HOME").is_none() => PathBuf::from(x).join("opencode"),
                _ => h.join(".config").join("opencode"),
            };
            if let Some(c) = json_cambio(d.join("opencode.json"), tr!("MCP server", "servidor MCP"), &|j| {
                let servidores = objeto(j, "mcp")?;
                if servidores.contains_key("omoikane") {
                    return Ok(false);
                }
                servidores.insert("omoikane".into(), json!({ "type": "local", "command": [omoi, "mcp"], "enabled": true }));
                Ok(true)
            })? {
                v.push(c);
            }
            let plugin = d.join("plugins").join("omoikane.ts");
            if !plugin.exists() {
                v.push(Cambio { fichero: plugin, que: tr!("recall plugin (each prompt)", "plugin del recall (cada mensaje)"), contenido: plugin_opencode(&omoi) });
            }
            if let Some(c) = con_instrucciones(&d.join("AGENTS.md"))? {
                v.push(Cambio { fichero: d.join("AGENTS.md"), que: tr!("instructions", "instrucciones"), contenido: c });
            }
        }
        otro => bail!(tr!(
            "unknown agent «{otro}»: claude, codex, gemini, cursor or opencode",
            "agente «{otro}» desconocido: claude, codex, gemini, cursor u opencode"
        )),
    }
    Ok(v)
}

fn toml_texto(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Plugin de OpenCode: con cada mensaje, `omoi recall` con el texto del usuario; lo que devuelva entra
/// en el prompt de sistema de ese turno (`experimental.chat.system.transform`). Probado con
/// OpenCode 1.19.5: añadirlo como parte nueva del mensaje hace fallar al servidor (sus ids de parte
/// tienen formato propio) y cambiar el texto en `chat.message` ya no llega al modelo.
fn plugin_opencode(omoi: &str) -> String {
    format!(
        r#"// Omoikane: recall automático en OpenCode. Lo creó `omoi setup opencode`; bórralo para quitarlo.
const OMOI = {omoi:?}
const pendiente = new Map()

export const Omoikane = async ({{ $, directory }}) => ({{
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

/// Escribe los cambios, con una copia del original la primera vez.
pub fn aplicar(cambios: &[Cambio]) -> Result<()> {
    for c in cambios {
        if let Some(d) = c.fichero.parent() {
            std::fs::create_dir_all(d)?;
        }
        if c.fichero.exists() {
            let copia = c.fichero.with_file_name(format!("{}.antes-de-omoikane", c.fichero.file_name().unwrap_or_default().to_string_lossy()));
            if !copia.exists() {
                std::fs::copy(&c.fichero, &copia)?;
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
    fn orden_segun_la_shell() {
        assert_eq!(orden("C:/x/omoi.exe", Shell::Powershell), "C:/x/omoi.exe");
        assert_eq!(orden("/usr/bin/omoi", Shell::Desconocida), "/usr/bin/omoi");
        let con_espacios = orden("C:/Mis cosas/omoi.exe", Shell::Powershell);
        if cfg!(windows) {
            assert_eq!(con_espacios, "& \"C:/Mis cosas/omoi.exe\"");
        } else {
            assert_eq!(con_espacios, "\"C:/Mis cosas/omoi.exe\"");
        }
        assert_eq!(orden("C:/Mis cosas/omoi.exe", Shell::Desconocida), "\"C:/Mis cosas/omoi.exe\"");
    }
}
