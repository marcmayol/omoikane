//! Guardar aunque el agente no lo haga (Fase 7.2): al cerrar o compactar una sesión, Omoikane lee la
//! conversación y aparta **candidatas** a memoria; al empezar la siguiente en ese proyecto se las
//! pasa al agente, que guarda las que valen (con `remember`, en las palabras del usuario) y deja el
//! resto. Omoikane no tiene modelo: detecta dónde puede haber algo; decidir y redactar lo hace el agente.
//!
//! Calibrado con las conversaciones reales de Marc (29-oct-2026). Lo que no funcionó: buscar palabras
//! de decisión («me quedo con…») perdía casi todas las decisiones de verdad, que son respuestas
//! cortas a una pregunta del agente («Me gusta mucho la 1C, pero no la veo como portada»), y lo que
//! pillaba no se entendía sin la pregunta; los errores seguidos de un arreglo eran casi todos fallos
//! de scripts de un solo uso. Lo que queda: cada respuesta a una pregunta, emparejada con ella; las
//! respuestas a `AskUserQuestion`; las frases de preferencia o corrección; y los hitos (commit, tag,
//! publicación). Nada que parezca un secreto, nada que ya esté guardado, como mucho 5 (más los hitos).
//!
//! Lee la conversación de Claude Code, Codex, Gemini CLI, Qwen Code, Copilot CLI y Cursor, y un JSONL
//! genérico (Kiro, el plugin de OpenCode): ver `turnos` y la Fase 7.5 del PLAN.

use crate::memoria::{AHORA, Ambito};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

pub const DECISION: &str = "decision";
pub const PREFERENCIA: &str = "preference";
pub const HITO: &str = "milestone";

/// Cuántas se proponen por sesión (sin contar los hitos).
pub const MAXIMO: usize = 5;
/// Días que esperan a la sesión siguiente antes de caducar.
pub const DIAS: i64 = 7;
/// A partir de esta cobertura por una memoria que ya existe, ya está guardada.
const YA_GUARDADA: f64 = 0.6;

#[derive(Clone, Debug, Serialize)]
pub struct Candidata {
    pub kind: &'static str,
    pub text: String,
    #[serde(skip)]
    puntos: f64,
}

fn normal(s: &str) -> String {
    crate::buscar::sin_tildes(&s.to_lowercase())
}

fn tiene(t: &str, frases: &[&str]) -> bool {
    frases.iter().any(|w| {
        t.match_indices(w).any(|(i, _)| {
            let antes = t[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let despues = t[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
            antes && despues
        })
    })
}

/// Lo que dice el usuario cuando marca cómo quiere las cosas.
const PREFERENCIAS: &[&str] = &[
    "prefiero", "no quiero", "no me gusta", "me gusta mas", "nunca hagas", "nunca uses", "nunca pongas", "no hagas", "no uses", "no pongas",
    "no toques", "no cambies", "no sugieras", "no propongas", "te he dicho", "te lo he dicho", "deja de", "a partir de ahora",
    "de ahora en adelante", "recuerda que", "acuerdate de", "no vuelvas a", "quiero que siempre", "quiero que nunca", "i prefer",
    "from now on", "stop doing", "never do", "don't ever",
];
/// Lo que, en una respuesta, suena a opinión o elección (sube su puntuación).
const OPINION: &[&str] = &["gusta", "prefiero", "mejor", "veo", "quiero", "elijo", "me quedo", "vamos con", "no", "pero", "like", "prefer", "rather", "choose"];
/// Encargos («dame…», «explícame…», «publica…»): piden algo, no deciden nada; van detrás.
const ENCARGOS: &[&str] = &[
    "dame", "haz", "hazme", "hazlo", "explicame", "explica", "publica", "ponte", "busca", "buscame", "investiga", "mira", "prepara",
    "sube", "subelo", "crea", "anade", "pon", "ponla", "ponlo", "quita", "borra", "borralos", "arregla", "prueba", "abre", "lee", "escribe",
    "give", "make", "explain", "publish", "search", "look", "write", "fix", "open", "show",
];

/// Respuestas que no dicen nada sin más contexto.
const TRIVIALES: &[&str] = &["si", "sí", "vale", "ok", "okay", "claro", "perfecto", "genial", "gracias", "adelante", "dale", "continua", "sigue", "hazlo", "venga", "bien", "no", "yes", "go", "sure", "thanks"];

/// Formatos de secreto conocidos, o una «palabra» larga que mezcla minúsculas, mayúsculas y cifras.
pub fn parece_secreto(t: &str) -> bool {
    // Lo que `secretos` redactaría (tokens con prefijo, claves PEM, JWT, cadenas que parecen aleatorias…).
    if !crate::secretos::hallazgos(t).is_empty() {
        return true;
    }
    // Y, para apartar, también lo que habla de una clave aunque no se vea la forma del token.
    let n = t.to_lowercase();
    if n.contains("private key") {
        return true;
    }
    for clave in ["password", "passwd", "contraseña", "contrasena", "token", "secret", "api_key", "apikey", "api-key"] {
        if let Some(i) = n.find(clave) {
            let resto = n[i + clave.len()..].trim_start();
            if resto.starts_with(':') || resto.starts_with('=') {
                return true;
            }
        }
    }
    false
}

/// La última pregunta de los últimos párrafos de un mensaje del agente (sin marcas de Markdown).
fn ultima_pregunta(texto: &str) -> Option<String> {
    let limpio: String = texto.chars().filter(|c| !"*_`#>".contains(*c)).collect();
    let parrafos: Vec<&str> = limpio.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
    for p in parrafos.iter().rev().take(3) {
        if let Some(fin) = p.rfind('?') {
            let antes = &p[..fin];
            let ini = antes.rfind(['.', '!', '?', '\n']).map(|i| i + 1).unwrap_or(0);
            let q: String = p[ini..=fin].trim().chars().take(240).collect();
            return Some(q);
        }
    }
    None
}

/// `"pregunta?"="respuesta"` del resultado de `AskUserQuestion`.
fn respuestas(salida: &str) -> Vec<(String, String)> {
    let mut r = Vec::new();
    let mut resto = salida;
    while let Some(i) = resto.find("\"=\"") {
        let antes = &resto[..i];
        let despues = &resto[i + 3..];
        let Some(ini) = antes.rfind('"') else { break };
        let Some(fin) = despues.find('"') else { break };
        let p = &antes[ini + 1..];
        if p.ends_with('?') && p.len() >= 8 {
            r.push((p.to_string(), despues[..fin].trim().to_string()));
        }
        resto = &despues[fin..];
    }
    r
}

/// Lo que el sistema mete como si fuera del usuario (recordatorios, órdenes, resultados de comandos).
fn es_ruido(t: &str) -> bool {
    let t = t.trim();
    t.is_empty() || t.starts_with('<') || t.starts_with('[') || t.contains("system-reminder") || t.contains("local-command") || t.starts_with("Caveat:") || t.chars().count() > 1200
}

fn una_linea(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Un turno de la conversación, sea del agente que sea.
#[derive(Debug, Clone, PartialEq)]
pub enum Turno {
    Usuario(String),
    Agente(String),
    /// Una pregunta con opciones y lo que respondió el usuario (`AskUserQuestion` de Claude Code).
    Respuesta(String, String),
    /// Un comando que se ejecutó bien (para los hitos: commit, tag, publicación).
    Comando(String),
}

fn texto_partes(v: &serde_json::Value) -> String {
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

/// El comando de una llamada a herramienta: `command` como texto o como lista, o dentro de `arguments`
/// (que en Codex es JSON en un texto).
fn comando_de(v: &serde_json::Value) -> Option<String> {
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

fn de_rol(rol: &str) -> Option<bool> {
    match rol {
        "user" | "human" => Some(true),
        "assistant" | "gemini" | "model" | "qwen" | "agent" | "ai" => Some(false),
        _ => None,
    }
}

/// Lo que dice el usuario dentro de `<user_query>` (Cursor); sin ella, la línea la metió el agente.
fn consulta_de_cursor(t: &str) -> Option<String> {
    let i = t.find("<user_query>")? + "<user_query>".len();
    let f = t[i..].find("</user_query>").map(|f| i + f).unwrap_or(t.len());
    Some(t[i..f].trim().to_string())
}

/// Los turnos de una conversación de cualquiera de los agentes que se conocen (Claude Code, Codex,
/// Gemini CLI, Qwen Code, Copilot CLI, Cursor), o de un JSONL genérico con `role`/`type` y
/// `text`/`content`/`parts` (Kiro, el que escribe el plugin de OpenCode, formatos nuevos).
pub fn turnos(ruta: &Path) -> Result<Vec<Turno>> {
    let texto = std::fs::read_to_string(ruta)?;
    // Gemini CLI antes de junio de 2026: un JSON entero con `messages`.
    if let Ok(todo) = serde_json::from_str::<serde_json::Value>(&texto)
        && let Some(ms) = todo["messages"].as_array()
    {
        let mut r = Vec::new();
        for m in ms {
            turno_generico(m, &mut r);
        }
        return Ok(r);
    }
    // (id, turno): Gemini repite el mismo mensaje con el mismo id mientras crece; vale el último.
    let mut r: Vec<(Option<String>, Turno)> = Vec::new();
    let mut herramientas: std::collections::HashMap<String, (String, serde_json::Value)> = std::collections::HashMap::new();
    let poner = |r: &mut Vec<(Option<String>, Turno)>, id: Option<String>, t: Turno| {
        if let Some(id) = &id
            && let Some(sitio) = r.iter_mut().find(|(x, _)| x.as_deref() == Some(id.as_str()))
        {
            sitio.1 = t;
            return;
        }
        r.push((id, t));
    };
    for linea in texto.lines() {
        let Ok(d) = serde_json::from_str::<serde_json::Value>(linea) else { continue };
        let tipo = d["type"].as_str().unwrap_or_default();
        if d["isSidechain"] == true || d["isMeta"] == true {
            continue;
        }
        // Claude Code.
        if (tipo == "user" || tipo == "assistant") && d["message"]["content"].is_array() || (tipo == "user" && d["message"]["content"].is_string()) {
            let m = &d["message"];
            if tipo == "assistant" {
                for b in m["content"].as_array().into_iter().flatten() {
                    if b["type"] == "tool_use" {
                        herramientas.insert(b["id"].as_str().unwrap_or_default().to_string(), (b["name"].as_str().unwrap_or_default().to_string(), b["input"].clone()));
                    } else if let Some(t) = b["text"].as_str().filter(|t| b["type"] == "text" && !t.trim().is_empty()) {
                        poner(&mut r, None, Turno::Agente(t.to_string()));
                    }
                }
                continue;
            }
            if let Some(bloques) = m["content"].as_array()
                && bloques.iter().any(|b| b["type"] == "tool_result")
            {
                for b in bloques.iter().filter(|b| b["type"] == "tool_result") {
                    let Some((nombre, entrada)) = herramientas.get(b["tool_use_id"].as_str().unwrap_or_default()) else { continue };
                    let salida = texto_partes(&b["content"]);
                    if nombre == "AskUserQuestion" {
                        for (p, resp) in respuestas(&salida) {
                            poner(&mut r, None, Turno::Respuesta(p, resp));
                        }
                    }
                    let fallo = b["is_error"] == true || salida.contains("Exit code ") && !salida.contains("Exit code 0");
                    if !fallo && let Some(c) = comando_de(entrada) {
                        poner(&mut r, None, Turno::Comando(c));
                    }
                }
                continue;
            }
            poner(&mut r, None, Turno::Usuario(texto_partes(&m["content"])));
            continue;
        }
        // Codex: `response_item` con el mensaje o la llamada dentro de `payload`.
        if tipo == "response_item" {
            let p = &d["payload"];
            match p["type"].as_str().unwrap_or_default() {
                "message" => match p["role"].as_str() {
                    Some("user") => poner(&mut r, None, Turno::Usuario(texto_partes(&p["content"]))),
                    Some("assistant") => poner(&mut r, None, Turno::Agente(texto_partes(&p["content"]))),
                    _ => {}
                },
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    if let Some(c) = comando_de(&p["arguments"]).or_else(|| comando_de(&p["input"])).or_else(|| comando_de(&p["action"])) {
                        poner(&mut r, None, Turno::Comando(c));
                    }
                }
                _ => {}
            }
            continue;
        }
        // Copilot CLI: `events.jsonl`.
        if tipo == "user.message" {
            if !d["data"]["source"].as_str().is_some_and(|s| s.starts_with("skill-")) {
                poner(&mut r, None, Turno::Usuario(d["data"]["content"].as_str().unwrap_or_default().to_string()));
            }
            continue;
        }
        if tipo == "assistant.message" {
            if d["data"]["parentToolCallId"].is_null() {
                poner(&mut r, None, Turno::Agente(d["data"]["content"].as_str().unwrap_or_default().to_string()));
            }
            continue;
        }
        if tipo == "tool.execution_start" {
            if let Some(c) = comando_de(&d["data"]["arguments"]) {
                poner(&mut r, None, Turno::Comando(c));
            }
            continue;
        }
        // Gemini CLI (JSONL desde junio de 2026): `$set` rehace la lista; cada mensaje lleva su id.
        if let Some(ms) = d["$set"]["messages"].as_array() {
            for m in ms {
                let mut t = Vec::new();
                turno_generico(m, &mut t);
                for x in t {
                    poner(&mut r, m["id"].as_str().map(String::from), x);
                }
            }
            continue;
        }
        // Cursor: estilo Anthropic, y lo que escribe el usuario va dentro de `<user_query>`.
        if let Some(rol) = d["role"].as_str()
            && d["message"]["content"].is_array()
        {
            for b in d["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    if let Some(c) = comando_de(&b["input"]) {
                        poner(&mut r, None, Turno::Comando(c));
                    }
                } else if let Some(t) = b["text"].as_str() {
                    match rol {
                        "user" => {
                            if let Some(q) = consulta_de_cursor(t) {
                                poner(&mut r, None, Turno::Usuario(q));
                            }
                        }
                        "assistant" => poner(&mut r, None, Turno::Agente(t.to_string())),
                        _ => {}
                    }
                }
            }
            continue;
        }
        // Grok Build: un `ConversationItem` por línea, sin `id` ni `message`. Lo que mete Grok por su
        // cuenta (instrucciones, recordatorios) lleva `synthetic_reason`; lo que escribe el usuario no
        // (o `interjection`, si lo mandó con el agente trabajando).
        if d.get("id").is_none() && d.get("message").is_none() {
            if tipo == "user" && d["content"][0]["type"] == "text" {
                if d["synthetic_reason"].as_str().is_none_or(|s| s == "interjection") {
                    poner(&mut r, None, Turno::Usuario(texto_partes(&d["content"])));
                }
                continue;
            }
            if tipo == "assistant" && d["content"].is_string() {
                if let Some(t) = d["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    poner(&mut r, None, Turno::Agente(t.to_string()));
                }
                for c in d["tool_calls"].as_array().into_iter().flatten() {
                    if let Some(c) = comando_de(&c["arguments"]) {
                        poner(&mut r, None, Turno::Comando(c));
                    }
                }
                continue;
            }
        }
        // Gemini, Qwen y el genérico (Kiro, OpenCode, otros).
        let mut t = Vec::new();
        turno_generico(&d, &mut t);
        for x in t {
            poner(&mut r, d["id"].as_str().filter(|_| d["type"] == "gemini" || d["type"] == "user").map(String::from), x);
        }
    }
    Ok(r.into_iter().map(|(_, t)| t).collect())
}

/// Un registro con rol (`role`, `type`, `author` o `sender`) y texto (`text`, `content`, `message`
/// o `parts[].text`), o un comando (`command`).
fn turno_generico(d: &serde_json::Value, r: &mut Vec<Turno>) {
    let rol = ["role", "type", "author", "sender"].iter().find_map(|k| d[k].as_str()).unwrap_or_default();
    if let Some(c) = comando_de(d).filter(|_| matches!(rol, "tool" | "command" | "shell")) {
        r.push(Turno::Comando(c));
        return;
    }
    let Some(es_usuario) = de_rol(rol) else { return };
    let mut t = String::new();
    for v in [&d["text"], &d["content"], &d["message"]["parts"], &d["parts"], &d["message"]["content"], &d["message"]] {
        t = texto_partes(v);
        if !t.trim().is_empty() {
            break;
        }
    }
    // Qwen: las llamadas a herramientas van en `message.parts[].functionCall`.
    for p in d["message"]["parts"].as_array().into_iter().flatten() {
        if let Some(c) = comando_de(&p["functionCall"]["args"]) {
            r.push(Turno::Comando(c));
        }
    }
    if !t.trim().is_empty() {
        r.push(if es_usuario { Turno::Usuario(t) } else { Turno::Agente(t) });
    }
}

/// Las candidatas de una conversación, de la más a la menos prometedora.
pub fn de_conversacion(ruta: &Path) -> Result<Vec<Candidata>> {
    let mut r: Vec<Candidata> = Vec::new();
    let mut ultimo_texto = String::new();
    let mut commit = None;
    let mut publicado = None;
    for turno in turnos(ruta)? {
        match turno {
            Turno::Agente(t) => {
                if !t.trim().is_empty() {
                    ultimo_texto = t;
                }
            }
            Turno::Respuesta(p, resp) => r.push(Candidata { kind: DECISION, text: una_linea(&format!("{p} → {resp}")), puntos: 2.5 }),
            Turno::Comando(cmd) => {
                if let Some(i) = cmd.find("git commit") {
                    let resto = &cmd[i..];
                    if let Some(j) = resto.find("-m \"").or_else(|| resto.find("-m '")) {
                        let msg: String = resto[j + 4..].split(['"', '\'', '\n']).next().unwrap_or_default().to_string();
                        if !msg.is_empty() {
                            commit = Some(msg);
                        }
                    }
                }
                if let Some(i) = cmd.find("git tag -a ") {
                    publicado = cmd[i + 11..].split_whitespace().next().map(|t| format!("tag {t}"));
                } else if cmd.contains("cargo publish") && publicado.is_none() {
                    publicado = Some("cargo publish".into());
                }
            }
            Turno::Usuario(t) => {
                if es_ruido(&t) {
                    continue;
                }
                let t = t.trim();
                let n = normal(t);
                let pregunta = (!ultimo_texto.is_empty()).then(|| ultima_pregunta(&ultimo_texto)).flatten();
                let con = |p: &Option<String>| p.as_ref().map(|q| format!("{q} → {t}")).unwrap_or_else(|| t.to_string());
                if tiene(&n, PREFERENCIAS) {
                    r.push(Candidata { kind: PREFERENCIA, text: una_linea(&con(&pregunta)), puntos: 3.0 });
                } else if let Some(q) = &pregunta {
                    let trivial = TRIVIALES.contains(&n.trim_matches(|c: char| !c.is_alphanumeric()));
                    if !trivial && t.chars().count() < 600 {
                        let primera = n.split(|c: char| !c.is_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("");
                        let encargo = if ENCARGOS.contains(&primera) { -1.5 } else { 0.0 };
                        let opinion = if tiene(&n, OPINION) { 1.0 } else { 0.0 } + encargo;
                        let largo = (t.chars().count() as f64 / 200.0).min(1.0);
                        r.push(Candidata { kind: DECISION, text: una_linea(&format!("{q} → {t}")), puntos: 1.0 + opinion + largo });
                    }
                }
                ultimo_texto.clear();
            }
        }
    }
    let mut vistas = std::collections::BTreeSet::new();
    let mut r: Vec<Candidata> = r.into_iter().filter(|c| !parece_secreto(&c.text) && vistas.insert(c.text.clone())).collect();
    // Las mejores primero, y a igualdad, la más reciente (lo último que se decidió suele ser lo que vale).

    let mut con_orden: Vec<(usize, Candidata)> = r.drain(..).enumerate().collect();
    con_orden.sort_by(|(ia, a), (ib, b)| b.puntos.total_cmp(&a.puntos).then(ib.cmp(ia)));
    let mut salida: Vec<Candidata> = con_orden.into_iter().map(|(_, c)| c).collect();
    salida.truncate(MAXIMO);
    for h in [commit.map(|c| format!("last commit: {c}")), publicado.map(|p| format!("published: {p}"))].into_iter().flatten() {
        if !parece_secreto(&h) {
            salida.push(Candidata { kind: HITO, text: h, puntos: 0.0 });
        }
    }
    Ok(salida)
}

/// Dónde está la conversación de una sesión cuando el hook no da la ruta (Copilot en `sessionEnd`,
/// Kiro en `Stop`): por su id, en las carpetas que usa cada uno.
pub fn buscar_conversacion(session_id: &str) -> Option<std::path::PathBuf> {
    let id = session_id.trim();
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    let casa = dirs::home_dir()?;
    let copilot = std::env::var_os("COPILOT_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| casa.join(".copilot"));
    [
        copilot.join("session-state").join(id).join("events.jsonl"),
        copilot.join("session-state").join(format!("{id}.jsonl")),
        casa.join(".kiro").join("sessions").join("cli").join(format!("{id}.jsonl")),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .or_else(|| {
        // Grok: `~/.grok/sessions/<carpeta codificada>/<id>/chat_history.jsonl`.
        let grok = std::env::var_os("GROK_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| casa.join(".grok"));
        std::fs::read_dir(grok.join("sessions")).ok()?.flatten().map(|d| d.path().join(id).join("chat_history.jsonl")).find(|p| p.is_file())
    })
}

/// Al cerrar o compactar: aparta las candidatas de la conversación que no estén ya guardadas.
pub fn apartar(db: &Connection, project: &str, session: &str, ruta: &Path) -> Result<usize> {
    let cs = de_conversacion(ruta)?;
    let memorias = crate::memoria::recientes(db, &Ambito::de(project), None, usize::MAX >> 1)?;
    let mut n = 0;
    for c in cs {
        let respuesta = c.text.rsplit(" → ").next().unwrap_or(&c.text);
        let cubierta = crate::recall::cobertura(db, project, respuesta, memorias.iter())?.into_iter().fold(0.0, f64::max);
        if c.kind != HITO && cubierta >= YA_GUARDADA {
            continue;
        }
        n += db.execute(
            &format!("INSERT OR IGNORE INTO candidate(project, session, kind, text, created_at) VALUES (?1, ?2, ?3, ?4, {AHORA})"),
            params![project, session, c.kind, c.text],
        )?;
    }
    // Las que nadie miró caducan.
    db.execute(&format!("DELETE FROM candidate WHERE julianday({AHORA}) - julianday(created_at) > {DIAS}"), [])?;
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

fn de_fila(f: &rusqlite::Row) -> rusqlite::Result<Apartada> {
    Ok(Apartada { id: f.get(0)?, project: f.get(1)?, session: f.get(2)?, kind: f.get(3)?, text: f.get(4)?, created_at: f.get(5)?, shown_at: f.get(6)? })
}

/// Al empezar una sesión en `project`: las de sesiones anteriores que aún no se enseñaron (y se
/// apuntan como enseñadas: no vuelven a salir al agente, solo en la vista).
pub fn para_empezar(db: &Connection, project: &str, session: &str) -> Result<Vec<Apartada>> {
    let mut q = db.prepare(&format!(
        "SELECT id, project, session, kind, text, created_at, shown_at FROM candidate
         WHERE project = ?1 AND session != ?2 AND shown_at IS NULL AND julianday({AHORA}) - julianday(created_at) <= {DIAS}
         ORDER BY CASE kind WHEN 'preference' THEN 0 WHEN 'decision' THEN 1 ELSE 2 END, id DESC LIMIT ?3"
    ))?;
    let v: Vec<Apartada> = q.query_map(params![project, session, (MAXIMO + 2) as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    for c in &v {
        db.execute(&format!("UPDATE candidate SET shown_at = {AHORA} WHERE id = ?1"), [c.id])?;
    }
    Ok(v)
}

/// Todas las que esperan (para la vista y la CLI).
pub fn todas(db: &Connection) -> Result<Vec<Apartada>> {
    let mut q = db.prepare(&format!(
        "SELECT id, project, session, kind, text, created_at, shown_at FROM candidate
         WHERE julianday({AHORA}) - julianday(created_at) <= {DIAS} ORDER BY id DESC"
    ))?;
    let v = q.query_map([], de_fila)?.collect::<rusqlite::Result<_>>()?;
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
    fn secretos() {
        assert!(parece_secreto("export GH=ghp_abcdefghijklmnopqrstuvwxyz0123"));
        assert!(parece_secreto("echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s"));
        assert!(parece_secreto("password: hunter2"));
        assert!(!parece_secreto("Me gusta mucho la 1C, pero no la veo como portada"));
        assert!(!parece_secreto("D:\\Omoikane\\src\\recall.rs y potion-multilingual-128M-int8"));
    }

    #[test]
    fn pregunta_y_respuestas() {
        assert_eq!(ultima_pregunta("Hecho.\n\nTe propongo tres. ¿Cuál te gusta? Dímelo."), Some("¿Cuál te gusta?".into()));
        assert_eq!(ultima_pregunta("Todo listo, sin preguntas."), None);
        let r = respuestas("The user answered: \"¿Qué estilo quieres?\"=\"el actual mejorado\", \"¿Y el alcance?\"=\"todo\". Read the answers");
        assert_eq!(r, vec![("¿Qué estilo quieres?".into(), "el actual mejorado".into()), ("¿Y el alcance?".into(), "todo".into())]);
    }
}
