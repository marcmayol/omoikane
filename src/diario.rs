//! El diario de sesiones (1.4, la capa episódica): una línea por sesión y proyecto con cuándo fue, qué
//! ficheros se tocaron, qué memorias le llegaron y cuáles se guardaron o cambiaron en ella. No guarda la
//! conversación. Responde a «¿qué hice el martes en X?» o «¿cuándo tocamos `sync.rs`?», que las
//! memorias no cubren: la de estado solo guarda la última foto.
//!
//! Se escribe al cerrar o compactar la sesión (`omoi recall --session-end`, que ya lee la conversación
//! para las candidatas) y nunca en el recall de cada mensaje. Cada escritura se suma a la que hubiera:
//! tras compactar, `recall_log` de la sesión se vacía, pero lo recibido antes ya está en su línea.
//!
//! Es local, como `recall_log` o `memory_sig`: no viaja con la sincronización ni con `omoi export`
//! (las rutas de los ficheros son de este equipo y dicen mucho de lo que se hace; el historial es de
//! cada equipo). Una 1.0 abre la base y no la ve, y no sube `lector_minimo`. La tabla aparece con la
//! primera línea (`crear_tabla`), no al abrir la base. Pesa unos cientos de bytes por sesión; se
//! guarda un año (`DIAS`).

use crate::memoria::{AHORA, Ambito};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// `files`, `recalled` y `saved` son listas JSON: los ficheros (relativos al repositorio si caen
/// dentro) y los ids enteros de las memorias. `folder` es la carpeta de la sesión, para leer la línea
/// aunque el proyecto ya no exista o la sesión fuera en una carpeta general (`project_id` 0).
const TABLA: &str = "
CREATE TABLE IF NOT EXISTS diary (
  session    TEXT NOT NULL,
  project_id INTEGER NOT NULL DEFAULT 0,
  folder     TEXT NOT NULL DEFAULT '',
  first_at   TEXT NOT NULL,
  last_at    TEXT NOT NULL,
  files      TEXT NOT NULL DEFAULT '[]',
  recalled   TEXT NOT NULL DEFAULT '[]',
  saved      TEXT NOT NULL DEFAULT '[]',
  PRIMARY KEY (session, project_id)
);
CREATE INDEX IF NOT EXISTS diary_last ON diary(last_at);
";

/// Cuántos ficheros se apuntan por sesión (los primeros que se tocaron).
pub const MAX_FICHEROS: usize = 50;
/// Cuántos días se guarda una línea. Con unos cientos de bytes por sesión, un año de diez sesiones al
/// día son un par de MB: cabe de sobra, y más allá casi nadie pregunta.
pub const DIAS: i64 = 365;
/// De cuántos subagentes de la sesión se leen los ficheros y las memorias (los más recientes), y hasta
/// qué tamaño: el hook de cierre tiene poco más de un segundo y después aún aparta candidatas.
pub const MAX_SUBAGENTES: usize = 10;
pub const MAX_BYTES_SUBAGENTE: u64 = 16 << 20;

/// La tabla se crea al escribir la primera línea, no al abrir la base: el recall de cada mensaje no
/// hace ni eso de más.
pub fn crear_tabla(db: &Connection) -> Result<()> {
    db.execute_batch(TABLA)?;
    Ok(())
}

fn hay_tabla(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'diary')", [], |f| f.get(0))?)
}

/// Lo que se saca de la conversación.
#[derive(Debug, Default, PartialEq)]
pub struct Lectura {
    /// Los ficheros que se editaron o escribieron, en el orden en que se tocaron, sin repetir.
    pub ficheros: Vec<String>,
    /// Los ids que devolvió `remember` (guardada) o `update`.
    pub guardadas: Vec<String>,
    /// Los ids (o su principio) que se pasaron a un `update` cuya respuesta no se encontró.
    pub pedidas: Vec<String>,
    /// La primera hora de la conversación, si la dice (UTC, como `AHORA`).
    pub primera: Option<String>,
}

/// Herramientas que escriben ficheros, en minúsculas: Claude Code (Edit, Write, MultiEdit,
/// NotebookEdit), Gemini y Qwen (write_file, replace), Copilot (edit, create, str_replace_editor),
/// Cursor (edit_file, search_replace, delete_file), Kiro (fs_write), Codex (apply_patch).
const EDICION: &[&str] = &[
    "edit", "write", "multiedit", "notebookedit", "write_file", "replace", "edit_file", "create_file", "create", "str_replace",
    "str_replace_editor", "str_replace_based_edit_tool", "strreplace", "search_replace", "fs_write", "apply_patch", "delete_file", "delete",
];
/// Si una línea nombra una herramienta que interesa (`"name":"Edit"`, `"toolName": "edit"`…) o lleva
/// un parche de Codex. De las líneas con una llamada, solo esas se leen como JSON: en una conversación
/// de 73 MB, leer todas las llamadas (Bash, Read…) costaba 130 ms, y buscar cada nombre por separado,
/// 40 más.
fn nombra_una(linea: &str) -> bool {
    if linea.contains("Begin Patch") {
        return true;
    }
    linea.match_indices("ame\":").any(|(i, _)| {
        let resto = linea[i + "ame\":".len()..].trim_start();
        let Some(resto) = resto.strip_prefix('"') else { return false };
        let nombre = &resto[..resto.find('"').unwrap_or(0)];
        !nombre.is_empty() && (es_edicion(nombre) || de_omoikane(nombre).is_some())
    })
}

/// Dónde dicen el fichero esas herramientas.
const CLAVES_RUTA: &[&str] = &["file_path", "notebook_path", "path", "filePath", "absolute_path", "target_file", "file", "filename"];
/// Dónde van los argumentos de una llamada.
const CLAVES_ARGS: &[&str] = &["input", "args", "arguments", "parameters", "params"];
/// El id de una llamada, y con el que su resultado dice de qué llamada es.
const CLAVES_ID: &[&str] = &["id", "call_id", "toolCallId", "tool_call_id", "callId"];
const CLAVES_REF: &[&str] = &["tool_use_id", "call_id", "toolCallId", "tool_call_id", "callId"];
/// Dónde va la respuesta de una herramienta.
const CLAVES_RESULTADO: &[&str] = &["content", "output", "result", "response", "functionResponse"];
/// Las líneas que pueden llevar una llamada a una herramienta: las demás no se leen como JSON.
/// `"tool_use"` (Claude Code, Cursor), con las comillas para que no case con `"tool_use_id"`: así los
/// resultados (que pueden ser ficheros enteros) solo se leen si son de una llamada pendiente; `_call`
/// (`function_call`, `custom_tool_call`, `local_shell_call` de Codex, `tool_calls` de Grok), `Call`
/// (`functionCall` de Qwen, `toolCalls` de Gemini) y lo de Copilot. Pocas y cortas: cada una es una
/// pasada por la línea.
const MARCAS: &[&str] = &["\"tool_use\"", "_call", "Call", "tool.execution_start", "Begin Patch"];

#[derive(Clone, Copy, PartialEq)]
enum Omoi {
    Remember,
    Update,
}

fn nombre_de(o: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    ["name", "toolName", "tool_name"].iter().find_map(|k| o.get(*k).and_then(|v| v.as_str()))
}

/// `remember` o `update` de Omoikane: a secas, o de su servidor MCP (`mcp__omoikane__remember`,
/// `omoikane.update`…).
fn de_omoikane(nombre: &str) -> Option<Omoi> {
    let n = nombre.to_lowercase();
    let (servidor, corto) = match n.rfind("__").map(|i| (i, 2)).or_else(|| n.rfind(['.', '/', ':']).map(|i| (i, 1))) {
        Some((i, l)) => (&n[..i], &n[i + l..]),
        None => ("omoikane", n.as_str()),
    };
    if !servidor.contains("omoikane") {
        return None;
    }
    match corto {
        "remember" => Some(Omoi::Remember),
        "update" => Some(Omoi::Update),
        _ => None,
    }
}

/// Si la herramienta escribe ficheros. Las de un servidor MCP (`mcp__x__create`) no: no son del agente.
fn es_edicion(nombre: &str) -> bool {
    !nombre.contains("__") && EDICION.contains(&nombre.to_lowercase().as_str())
}

/// Los argumentos de una llamada: un objeto, o un texto que es JSON (Codex) o el parche mismo.
fn argumentos(o: &serde_json::Map<String, serde_json::Value>) -> Option<serde_json::Value> {
    let v = CLAVES_ARGS.iter().find_map(|k| o.get(*k))?;
    Some(match v.as_str() {
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string())),
        None => v.clone(),
    })
}

/// Los ficheros de un parche de Codex (`*** Update File: x`, `*** Add File:`, `*** Delete File:`,
/// `*** Move to:`).
fn de_parche(texto: &str, r: &mut Vec<String>) {
    for l in texto.lines() {
        let l = l.trim_start();
        for marca in ["*** Update File: ", "*** Add File: ", "*** Delete File: ", "*** Move to: "] {
            if let Some(p) = l.strip_prefix(marca) {
                r.push(p.trim().to_string());
            }
        }
    }
}

/// Un id de memoria entero (ULID: 26 letras y cifras en mayúsculas).
fn es_id(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

/// El id de lo que respondió `remember` (solo si guardó: `saved: true`) o `update`, buscando en el
/// resultado (que puede venir como texto JSON dentro de otro JSON).
fn id_del_resultado(v: &serde_json::Value, que: Omoi, hondo: usize) -> Option<String> {
    if hondo > 6 {
        return None;
    }
    match v {
        serde_json::Value::Object(o) => {
            if let Some(id) = o.get("id").and_then(|x| x.as_str()).filter(|x| es_id(x))
                && (que == Omoi::Update || o.get("saved") == Some(&serde_json::Value::Bool(true)))
            {
                return Some(id.to_string());
            }
            if o.get("saved") == Some(&serde_json::Value::Bool(false)) {
                return None;
            }
            o.values().find_map(|x| id_del_resultado(x, que, hondo + 1))
        }
        serde_json::Value::Array(xs) => xs.iter().find_map(|x| id_del_resultado(x, que, hondo + 1)),
        serde_json::Value::String(s) if s.trim_start().starts_with(['{', '[']) => {
            serde_json::from_str::<serde_json::Value>(s).ok().and_then(|x| id_del_resultado(&x, que, hondo + 1))
        }
        _ => None,
    }
}

struct Lector {
    l: Lectura,
    /// Las llamadas a `remember`/`update` sin respuesta todavía, por su id, con el id que se pasó a
    /// `update` (para cuando su respuesta no aparezca).
    pendientes: HashMap<String, (Omoi, Option<String>)>,
}

impl Lector {
    fn mirar(&mut self, v: &serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                if let Some(nombre) = nombre_de(o) {
                    // Lo de dentro es de esta llamada (su respuesta, en Gemini): no es otra.
                    self.llamada(nombre, o);
                    return;
                } else if let Some(r) = CLAVES_REF.iter().find_map(|k| o.get(*k).and_then(|x| x.as_str()))
                    && let Some((que, _)) = self.pendientes.remove(r)
                {
                    // Sin id en la respuesta (un error, una duplicada), no se guardó nada.
                    let id = CLAVES_RESULTADO.iter().filter_map(|k| o.get(*k)).find_map(|x| id_del_resultado(x, que, 0));
                    self.l.guardadas.extend(id);
                    return;
                }
                for x in o.values() {
                    self.mirar(x);
                }
            }
            serde_json::Value::Array(xs) => xs.iter().for_each(|x| self.mirar(x)),
            _ => {}
        }
    }

    fn llamada(&mut self, nombre: &str, o: &serde_json::Map<String, serde_json::Value>) {
        let args = argumentos(o);
        if let Some(que) = de_omoikane(nombre) {
            // La respuesta puede venir dentro de la misma llamada (Gemini) o en otra línea, por su id.
            if let Some(id) = ["result", "response", "output"].iter().filter_map(|k| o.get(*k)).find_map(|x| id_del_resultado(x, que, 0)) {
                self.l.guardadas.push(id);
                return;
            }
            let pedida = args.as_ref().and_then(|a| a["id"].as_str()).filter(|_| que == Omoi::Update).map(String::from);
            match CLAVES_ID.iter().find_map(|k| o.get(*k).and_then(|x| x.as_str())) {
                Some(id) => {
                    self.pendientes.insert(id.to_string(), (que, pedida));
                }
                None => self.l.pedidas.extend(pedida),
            }
            return;
        }
        let Some(args) = args else { return };
        // El parche de Codex, como herramienta o dentro de un comando.
        let texto = match &args {
            serde_json::Value::String(s) => s.clone(),
            otro => otro.to_string(),
        };
        if texto.contains("*** Begin Patch") {
            let mut r = Vec::new();
            match &args {
                serde_json::Value::String(s) => de_parche(s, &mut r),
                otro => {
                    // Dentro de un JSON: los textos de dentro, ya sin escapar.
                    fn textos(v: &serde_json::Value, f: &mut dyn FnMut(&str)) {
                        match v {
                            serde_json::Value::String(s) => f(s),
                            serde_json::Value::Array(xs) => xs.iter().for_each(|x| textos(x, f)),
                            serde_json::Value::Object(o) => o.values().for_each(|x| textos(x, f)),
                            _ => {}
                        }
                    }
                    textos(otro, &mut |s| de_parche(s, &mut r));
                }
            }
            self.l.ficheros.extend(r);
            return;
        }
        if !es_edicion(nombre) || args["command"].as_str() == Some("view") {
            return;
        }
        if let Some(p) = CLAVES_RUTA.iter().find_map(|k| args[*k].as_str()).filter(|p| !p.trim().is_empty()) {
            self.l.ficheros.push(p.trim().to_string());
        }
    }
}

/// La hora de un registro (`"timestamp":"2026-11-04T10:00:00.123Z"`), en el formato de `AHORA`.
fn hora_de(linea: &str) -> Option<String> {
    for clave in ["\"timestamp\":\"", "\"startTime\":\"", "\"timestamp\": \"", "\"startTime\": \""] {
        if let Some(i) = linea.find(clave) {
            let resto = &linea[i + clave.len()..];
            let v = &resto[..resto.find('"')?];
            let b = v.as_bytes();
            let forma = b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && v.ends_with('Z');
            if !forma {
                continue;
            }
            match v.len() {
                // Con `get`: un carácter de varios bytes en medio no es una hora, y no debe romper el hook.
                20 => return v.get(..19).map(|h| format!("{h}.000Z")),
                n if n >= 24 && b[19] == b'.' => return v.get(..23).map(|h| format!("{h}Z")),
                _ => continue,
            }
        }
    }
    None
}

/// Lo que dice la conversación: los ficheros tocados y las memorias guardadas o cambiadas. `raiz` es
/// la carpeta del proyecto: los ficheros de dentro quedan relativos a ella.
pub fn leer(ruta: &Path, raiz: &str) -> Result<Lectura> {
    // Sin exigir UTF-8: una conversación cortada a mitad de un carácter (el agente aún escribe) se lee igual.
    let bytes = std::fs::read(ruta)?;
    let texto = String::from_utf8_lossy(&bytes);
    let mut lector = Lector { l: Lectura::default(), pendientes: HashMap::new() };
    // Gemini CLI antes de junio de 2026: un JSON entero con `messages`.
    if let Ok(todo) = serde_json::from_str::<serde_json::Value>(&texto)
        && todo["messages"].is_array()
    {
        lector.l.primera = hora_de(&texto);
        lector.mirar(&todo["messages"]);
    } else {
        // Claude Code (y Cursor) solo llaman con `"tool_use"`: una pasada por línea en vez de todas.
        let corte = (0..=texto.len().min(65_536)).rev().find(|i| texto.is_char_boundary(*i)).unwrap_or(0);
        let claude = texto[..corte].contains("\"parentUuid\"");
        let marcas = if claude { &MARCAS[..1] } else { MARCAS };
        for (i, linea) in texto.lines().enumerate() {
            if lector.l.primera.is_none() && i < 50 {
                lector.l.primera = hora_de(linea);
            }
            let toca = marcas.iter().any(|m| linea.contains(m)) && nombra_una(linea)
                || lector.pendientes.keys().any(|id| linea.contains(id.as_str()));
            if !toca {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(linea) {
                lector.mirar(&v);
            }
        }
    }
    let mut l = lector.l;
    l.pedidas.extend(lector.pendientes.into_values().filter_map(|(_, pedida)| pedida));
    let mut vistos = BTreeSet::new();
    l.ficheros = l.ficheros.iter().map(|f| relativo(f, raiz)).filter(|f| vistos.insert(f.clone())).take(MAX_FICHEROS).collect();
    Ok(l)
}

/// `f` relativo a `raiz` si cae dentro, con `/`; si no, tal cual (con `/`). Sin tocar el disco.
fn relativo(f: &str, raiz: &str) -> String {
    let f = f.replace('\\', "/");
    let raiz = raiz.replace('\\', "/");
    let raiz = raiz.trim_end_matches('/');
    if raiz.is_empty() {
        return f;
    }
    // Se compara el principio de `f` tal cual y no `f` en minúsculas: pasar a minúsculas puede cambiar
    // cuántos bytes ocupa un carácter, y cortar por ahí rompería.
    let n = raiz.len();
    match (f.get(..n), f.get(n..)) {
        (Some(cabeza), Some(resto)) if resto.len() > 1 && resto.starts_with('/') && crate::proyecto::comparable(cabeza) == crate::proyecto::comparable(raiz) => {
            resto[1..].to_string()
        }
        _ => f,
    }
}

fn lista(db: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>> {
    let texto: Option<String> = db.query_row(sql, p, |f| f.get(0)).optional()?;
    Ok(texto.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// Junta `nuevos` detrás de `viejos`, sin repetir, como mucho `max`.
fn juntar(viejos: Vec<String>, nuevos: Vec<String>, max: usize) -> Vec<String> {
    let mut vistos = BTreeSet::new();
    viejos.into_iter().chain(nuevos).filter(|x| vistos.insert(x.clone())).take(max).collect()
}

/// Al cerrar o compactar: apunta (o completa) la línea de la sesión en el proyecto. `conversacion` es
/// la ruta de la conversación, si el agente la da; sin ella, solo lo que sabe la base (las memorias
/// recibidas). Sin id de sesión, el nombre del fichero de la conversación.
pub fn apuntar(db: &Connection, project: &str, session: &str, conversacion: Option<&Path>) -> Result<bool> {
    let sesion = match session.trim() {
        "" => conversacion.and_then(|c| c.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        s => s.to_string(),
    };
    if sesion.is_empty() {
        return Ok(false);
    }
    crear_tabla(db)?;
    let general = crate::proyecto::es_general(project);
    let project_id = if general || project.is_empty() { 0 } else { crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0) };
    let raiz = if general { "" } else { project };
    let mut lectura = match conversacion {
        Some(c) => leer(c, raiz)?,
        None => Lectura::default(),
    };
    // Lo que hicieron sus subagentes (Claude Code los guarda aparte): sus ficheros y sus memorias son de
    // la sesión. Uno que no se puede leer no estropea la línea.
    for sub in conversacion.map(crate::candidatas::subagentes_de).unwrap_or_default().into_iter().take(MAX_SUBAGENTES) {
        if std::fs::metadata(&sub).map(|m| m.len() > MAX_BYTES_SUBAGENTE).unwrap_or(true) {
            continue;
        }
        if let Ok(l) = leer(&sub, raiz) {
            lectura.ficheros = juntar(std::mem::take(&mut lectura.ficheros), l.ficheros, MAX_FICHEROS);
            lectura.guardadas.extend(l.guardadas);
            lectura.pedidas.extend(l.pedidas);
        }
    }
    // Las recibidas por relevancia: las preferencias y las fijadas llegan en todas las sesiones y no
    // dicen nada de esta.
    let mut q = db.prepare(
        "SELECT r.memory_id FROM recall_log r JOIN memory m ON m.id = r.memory_id
         WHERE r.session = ?1 AND m.pinned = 0 AND m.kind != 'preference' ORDER BY r.at, r.memory_id",
    )?;
    let recibidas: Vec<String> = q.query_map([&sesion], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    // Las guardadas o cambiadas, solo si existen (una respuesta de otra base, o ya olvidada, no).
    let mut guardadas = Vec::new();
    for id in &lectura.guardadas {
        if db.query_row("SELECT 1 FROM memory WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some() {
            guardadas.push(id.clone());
        }
    }
    for p in &lectura.pedidas {
        if let Ok(id) = crate::memoria::resolver_id(db, p) {
            guardadas.push(id);
        }
    }
    let clave = params![sesion, project_id];
    let ficheros = juntar(lista(db, "SELECT files FROM diary WHERE session = ?1 AND project_id = ?2", clave)?, lectura.ficheros, MAX_FICHEROS);
    let recibidas = juntar(lista(db, "SELECT recalled FROM diary WHERE session = ?1 AND project_id = ?2", clave)?, recibidas, usize::MAX);
    let guardadas = juntar(lista(db, "SELECT saved FROM diary WHERE session = ?1 AND project_id = ?2", clave)?, guardadas, usize::MAX);
    let empezo: Option<String> = db.query_row("SELECT started_at FROM session WHERE session = ?1", [&sesion], |f| f.get(0)).optional()?;
    let primera = [empezo, lectura.primera].into_iter().flatten().min();
    db.execute(
        &format!(
            "INSERT INTO diary(session, project_id, folder, first_at, last_at, files, recalled, saved)
             VALUES (?1, ?2, ?3, min(COALESCE(?4, {AHORA}), {AHORA}), {AHORA}, ?5, ?6, ?7)
             ON CONFLICT(session, project_id) DO UPDATE SET folder = excluded.folder,
               first_at = min(diary.first_at, excluded.first_at), last_at = excluded.last_at,
               files = excluded.files, recalled = excluded.recalled, saved = excluded.saved"
        ),
        params![sesion, project_id, project, primera, serde_json::to_string(&ficheros)?, serde_json::to_string(&recibidas)?, serde_json::to_string(&guardadas)?],
    )?;
    db.execute(&format!("DELETE FROM diary WHERE julianday({AHORA}) - julianday(last_at) > {DIAS}"), [])?;
    Ok(true)
}

/// Una memoria nombrada en una línea, con su título si sigue existiendo.
#[derive(Clone, Debug, Serialize)]
pub struct Nombrada {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Una línea del diario, para leerla.
#[derive(Clone, Debug, Serialize)]
pub struct Linea {
    pub session: String,
    /// La carpeta del proyecto (o de la sesión, si no era un proyecto).
    pub project: String,
    pub first_at: String,
    pub last_at: String,
    pub files: Vec<String>,
    pub recalled: Vec<Nombrada>,
    pub saved: Vec<Nombrada>,
}

/// Qué líneas leer.
#[derive(Default)]
pub struct Filtro {
    /// Desde este día o momento (`2026-11-03`, `2026-11-03T10`): las que acabaron entonces o después.
    pub desde: Option<String>,
    /// Hasta este día o momento: las que empezaron entonces o antes.
    pub hasta: Option<String>,
    /// Las que tocaron un fichero cuya ruta contiene esto.
    pub fichero: Option<String>,
    /// La carpeta desde la que se mira, si es la del proyecto que se pide: sus líneas sin proyecto (la
    /// carpeta aún no lo era al cerrar la sesión, o el proyecto se podó) también son suyas.
    pub carpeta: Option<String>,
    pub limite: usize,
}

/// `7d`, `2w` o una fecha (`2026-11-03`, con hora o sin ella), como el principio de un momento UTC.
pub fn fecha(db: &Connection, texto: &str) -> Result<String> {
    let t = texto.trim();
    if let Some(n) = t.strip_suffix('d').or_else(|| t.strip_suffix('w')).and_then(|n| n.parse::<i64>().ok()) {
        let dias = if t.ends_with('w') { n * 7 } else { n };
        return Ok(db.query_row("SELECT strftime('%Y-%m-%d', 'now', ?1)", [format!("-{dias} days")], |f| f.get(0))?);
    }
    let b = t.as_bytes();
    anyhow::ensure!(
        b.len() >= 10 && b[4] == b'-' && b[7] == b'-' && b[..4].iter().chain(&b[5..7]).chain(&b[8..10]).all(u8::is_ascii_digit),
        crate::tr!("not a date: «{t}» (use 2026-11-03, or 7d for the last 7 days)", "no es una fecha: «{t}» (usa 2026-11-03, o 7d para los últimos 7 días)")
    );
    Ok(t.trim_end_matches('Z').replace(' ', "T"))
}

/// El trozo de SQL que deja las líneas que se ven desde `ambito` (`d` es la tabla).
fn donde(ambito: &Ambito) -> String {
    match ambito {
        Ambito::Todos => "1".into(),
        // Las de `project_id` 0, solo si son de una de sus carpetas (se mira después, en `lineas`).
        Ambito::Proyecto { id, .. } => format!("d.project_id IN ({id}, 0)"),
        Ambito::Espacio(Some(s)) => format!("d.project_id IN (SELECT id FROM project WHERE space_id = {s})"),
        Ambito::Espacio(None) => "(d.project_id = 0 OR d.project_id IN (SELECT id FROM project WHERE space_id IS NULL))".into(),
    }
}

/// Cómo se compara la carpeta de una línea con la de un proyecto.
fn carpeta_cmp(ruta: &str) -> String {
    crate::proyecto::comparable(ruta.replace('\\', "/").trim_end_matches('/'))
}

/// Las líneas del diario que se ven desde `ambito`, de la más reciente a la más vieja.
pub fn lineas(db: &Connection, ambito: &Ambito, f: &Filtro) -> Result<Vec<Linea>> {
    if !hay_tabla(db)? {
        return Ok(Vec::new());
    }
    // Desde un proyecto, las líneas sin proyecto que son de sus carpetas (o de la carpeta desde la que se
    // mira, si aún no es un proyecto).
    let carpetas: Option<BTreeSet<String>> = match ambito {
        Ambito::Proyecto { id, .. } if *id != 0 => {
            let mut c: BTreeSet<String> = lista_de(db, "SELECT path FROM project_path WHERE project_id = ?1", [id])?.iter().map(|p| carpeta_cmp(p)).collect();
            c.extend(f.carpeta.as_deref().filter(|x| !x.is_empty()).map(carpeta_cmp));
            Some(c)
        }
        _ => None,
    };
    // El proyecto 0 existe con la ruta vacía: sin el `NULLIF`, una línea sin proyecto no diría su carpeta.
    let mut sql = format!(
        "SELECT d.session, d.project_id, d.folder, COALESCE(NULLIF((SELECT path FROM project WHERE id = d.project_id), ''), d.folder),
                d.first_at, d.last_at, d.files, d.recalled, d.saved
         FROM diary d WHERE {}",
        donde(ambito)
    );
    let mut valores: Vec<String> = Vec::new();
    if let Some(d) = &f.desde {
        valores.push(d.clone());
        sql.push_str(&format!(" AND substr(d.last_at, 1, length(?{n})) >= ?{n}", n = valores.len()));
    }
    if let Some(h) = &f.hasta {
        valores.push(h.clone());
        sql.push_str(&format!(" AND substr(d.first_at, 1, length(?{n})) <= ?{n}", n = valores.len()));
    }
    // El fichero y el límite, aquí y no en SQL: el `lower()` de SQLite solo sabe de ASCII, y el límite
    // va después del filtro para que `-n` dé tantas como haya.
    sql.push_str(" ORDER BY d.last_at DESC");
    let fichero = f.fichero.as_deref().map(|x| x.trim().replace('\\', "/").to_lowercase()).filter(|x| !x.is_empty());
    let limite = f.limite.max(1);
    let mut q = db.prepare(&sql)?;
    let mut filas = q.query(rusqlite::params_from_iter(valores.iter()))?;
    let mut titulo = db.prepare("SELECT title FROM memory WHERE id = ?1")?;
    let mut nombrar = |texto: &str| -> Result<Vec<Nombrada>> {
        let ids: Vec<String> = serde_json::from_str(texto).unwrap_or_default();
        ids.into_iter().map(|id| Ok(Nombrada { title: titulo.query_row([&id], |r| r.get(0)).optional()?, id })).collect()
    };
    let mut r = Vec::new();
    while r.len() < limite
        && let Some(fila) = filas.next()?
    {
        let (project_id, folder): (i64, String) = (fila.get(1)?, fila.get(2)?);
        if project_id == 0
            && let Some(c) = &carpetas
            && !c.contains(&carpeta_cmp(&folder))
        {
            continue;
        }
        // Fichero a fichero: el fragmento puede caer en dos a la vez en el JSON (`a.rs","b`).
        let files: Vec<String> = serde_json::from_str(&fila.get::<_, String>(6)?).unwrap_or_default();
        if let Some(x) = &fichero
            && !files.iter().any(|p| p.to_lowercase().contains(x))
        {
            continue;
        }
        r.push(Linea {
            session: fila.get(0)?,
            project: fila.get(3)?,
            first_at: fila.get(4)?,
            last_at: fila.get(5)?,
            files,
            recalled: nombrar(&fila.get::<_, String>(7)?)?,
            saved: nombrar(&fila.get::<_, String>(8)?)?,
        });
    }
    Ok(r)
}

fn lista_de(db: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(sql)?;
    let v = q.query_map(p, |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Tras `omoi projects prune`: las líneas de un proyecto que ya no existe pasan a no tener proyecto
/// (y siguen viéndose desde su carpeta). Si se quedaran con su id, un proyecto nuevo que lo
/// reutilizara (sin `AUTOINCREMENT`, SQLite puede volver a dar el id) heredaría unas líneas que no
/// son suyas, quizá de otro espacio.
pub fn soltar_podados(db: &Connection) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    let huerfanas = "project_id != 0 AND project_id NOT IN (SELECT id FROM project)";
    // Si la misma sesión ya tenía una línea sin proyecto, se queda esa.
    db.execute(&format!("UPDATE OR IGNORE diary SET project_id = 0 WHERE {huerfanas}"), [])?;
    db.execute(&format!("DELETE FROM diary WHERE {huerfanas}"), [])?;
    Ok(())
}

/// Al juntar dos proyectos (`proyecto::juntar`): las líneas de `de` pasan a `en`.
pub fn mover(db: &Connection, de: i64, en: i64) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    db.execute("UPDATE OR IGNORE diary SET project_id = ?2 WHERE project_id = ?1", params![de, en])?;
    db.execute("DELETE FROM diary WHERE project_id = ?1", [de])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_de_herramientas() {
        assert!(de_omoikane("mcp__omoikane__remember") == Some(Omoi::Remember));
        assert!(de_omoikane("remember") == Some(Omoi::Remember));
        assert!(de_omoikane("mcp__plugin_omoikane_omoikane__update") == Some(Omoi::Update));
        assert!(de_omoikane("mcp__claude_ai_Claude_Docs__update").is_none());
        assert!(es_edicion("Edit") && es_edicion("write_file") && !es_edicion("mcp__docs__create") && !es_edicion("Bash"));
    }

    #[test]
    fn rutas_relativas() {
        assert_eq!(relativo("/w/repo/src/a.rs", "/w/repo"), "src/a.rs");
        assert_eq!(relativo("/w/repo2/a.rs", "/w/repo"), "/w/repo2/a.rs");
        assert_eq!(relativo("src/a.rs", "/w/repo"), "src/a.rs");
        assert_eq!(relativo(r"D:\Omoikane\src\sync.rs", r"D:\Omoikane"), "src/sync.rs");
        // Caracteres que cambian de tamaño al pasar a minúsculas (`İ` ocupa 2 bytes, `i̇` 3): sin pánico.
        assert_eq!(relativo("/w/İİ/a.rs", "/w/ii"), "/w/İİ/a.rs");
        assert_eq!(relativo("/w/ii/a.rs", "/w/İİ"), "/w/ii/a.rs");
        assert_eq!(relativo("/w/ña/a.rs", "/w/ña"), "a.rs");
    }

    #[test]
    fn horas_y_fechas() {
        assert_eq!(hora_de(r#"{"timestamp":"2026-11-04T10:00:00.123Z","x":1}"#).as_deref(), Some("2026-11-04T10:00:00.123Z"));
        assert_eq!(hora_de(r#"{"timestamp":"2026-11-04T10:00:00Z"}"#).as_deref(), Some("2026-11-04T10:00:00.000Z"));
        assert_eq!(hora_de(r#"{"timestamp":"ayer"}"#), None);
        // Con letras de varios bytes donde va la hora: no es una hora, y no rompe.
        assert_eq!(hora_de(r#"{"timestamp":"2026-11-04T10:00:00.12ñZ"}"#), None);
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(fecha(&db, "2026-11-03").unwrap(), "2026-11-03");
        assert_eq!(fecha(&db, "7d").unwrap().len(), 10);
        assert!(fecha(&db, "martes").is_err());
    }
}
