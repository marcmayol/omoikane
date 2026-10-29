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
//! Solo Claude Code por ahora: su conversación es un `.jsonl` que el hook da en `transcript_path`.

use crate::memoria::{AHORA, Ambito};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::io::BufRead;
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
    let n = t.to_lowercase();
    let prefijos = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "sk-", "akia", "xoxb-", "xoxp-", "private key"];
    if prefijos.iter().any(|p| n.contains(p)) {
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
    // Un token es un trozo largo sin separadores; «potion-multilingual-128M-int8» son palabras con guiones.
    t.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| {
        w.len() >= 20 && w.chars().any(|c| c.is_ascii_lowercase()) && w.chars().any(|c| c.is_ascii_uppercase()) && w.chars().any(|c| c.is_ascii_digit())
    })
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

fn texto_de(contenido: &serde_json::Value) -> Vec<String> {
    match contenido {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(v) => v.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// Lo que el sistema mete como si fuera del usuario (recordatorios, órdenes, resultados de comandos).
fn es_ruido(t: &str) -> bool {
    let t = t.trim();
    t.is_empty() || t.starts_with('<') || t.starts_with('[') || t.contains("system-reminder") || t.contains("local-command") || t.starts_with("Caveat:") || t.chars().count() > 1200
}

fn una_linea(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Las candidatas de una conversación de Claude Code, de la más a la menos prometedora.
pub fn de_conversacion(ruta: &Path) -> Result<Vec<Candidata>> {
    let f = std::io::BufReader::new(std::fs::File::open(ruta)?);
    let mut r: Vec<Candidata> = Vec::new();
    let mut herramientas: std::collections::HashMap<String, (String, serde_json::Value)> = std::collections::HashMap::new();
    let mut ultimo_texto = String::new();
    let mut commit = None;
    let mut publicado = None;
    for linea in f.lines() {
        let Ok(linea) = linea else { continue };
        // Solo los mensajes (la conversación trae también instantáneas, peticiones, títulos…).
        if !(linea.contains("\"type\":\"user\"") || linea.contains("\"type\":\"assistant\"")) {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(&linea) else { continue };
        if d["isSidechain"] == true || d["isMeta"] == true {
            continue;
        }
        let m = &d["message"];
        if d["type"] == "assistant" {
            for b in m["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    herramientas.insert(b["id"].as_str().unwrap_or_default().to_string(), (b["name"].as_str().unwrap_or_default().to_string(), b["input"].clone()));
                } else if b["type"] == "text" && b["text"].as_str().is_some_and(|t| !t.trim().is_empty()) {
                    ultimo_texto = b["text"].as_str().unwrap_or_default().to_string();
                }
            }
            continue;
        }
        if let Some(bloques) = m["content"].as_array()
            && bloques.iter().any(|b| b["type"] == "tool_result")
        {
            for b in bloques.iter().filter(|b| b["type"] == "tool_result") {
                let Some((nombre, entrada)) = herramientas.get(b["tool_use_id"].as_str().unwrap_or_default()) else { continue };
                let salida = match &b["content"] {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(v) => v.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(" "),
                    _ => String::new(),
                };
                if nombre == "AskUserQuestion" {
                    for (p, resp) in respuestas(&salida) {
                        r.push(Candidata { kind: DECISION, text: una_linea(&format!("{p} → {resp}")), puntos: 2.5 });
                    }
                }
                let fallo = b["is_error"] == true || salida.contains("Exit code ") && !salida.contains("Exit code 0");
                if nombre == "Bash" && !fallo {
                    let cmd = entrada["command"].as_str().unwrap_or_default();
                    if let Some(i) = cmd.find("git commit") {
                        let resto = &cmd[i..];
                        if let Some(j) = resto.find("-m \"") {
                            let msg: String = resto[j + 4..].split(['"', '\n']).next().unwrap_or_default().to_string();
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
            }
            continue;
        }
        for t in texto_de(&m["content"]) {
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
