//! Recall automático: lo que los hooks de Claude Code inyectan sin que el agente lo pida.
//!
//! - Al empezar (o tras compactar): las preferencias y las fijadas del proyecto y globales.
//! - En cada mensaje: las memorias que de verdad tienen que ver con él, o ninguna. Inyectar de más
//!   ensucia el contexto del agente, así que el criterio es estricto y está calibrado con mensajes
//!   reales (PLAN.md §6): el mensaje tiene que tener al menos `TERMINOS_MIN` palabras útiles, la
//!   memoria tiene que compartir al menos `RAROS_MIN` palabras poco frecuentes en la base, y esas
//!   palabras tienen que pesar (por IDF) al menos `COBERTURA_MIN` de lo que pesa el mensaje.
//! - Además, las memorias cuyas rutas se mencionan en el mensaje (los `gotcha` de un fichero).
//!
//! Cada memoria se inyecta una vez por sesión (tabla `recall_log`), salvo tras compactar.

use crate::buscar::{self, Opciones};
use crate::memoria::{self, Ambito, Memoria};
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Deserialize;
use std::collections::BTreeSet;

/// Palabras útiles mínimas en el mensaje. «como ha ido todo?» no debe traer nada.
pub const TERMINOS_MIN: usize = 3;
/// Palabras raras (en ≤ `FRACCION_RARA` de las memorias) que tienen que coincidir.
pub const RAROS_MIN: usize = 2;
pub const FRACCION_RARA: f64 = 0.2;
/// Parte del peso IDF del mensaje que tiene que estar en la memoria.
pub const COBERTURA_MIN: f64 = 0.40;
/// Solo las que se acercan a la mejor: si una destaca, va sola.
pub const RELATIVO_A_LA_MEJOR: f64 = 0.7;
/// Como mucho, tantas memorias por mensaje.
pub const MAXIMO: usize = 3;
/// Desde cuántas letras una palabra cuenta también como prefijo («memoria» → «memorias»). Con menos,
/// «pon» coincidía con «pone», «ponga»…
pub const PREFIJO_MIN: usize = 5;
/// Recordatorio de guardar: a partir de estos mensajes y minutos de sesión sin que se haya guardado
/// ni cambiado ninguna memoria del proyecto, y como mucho una vez cada `MINUTOS_ENTRE_RECORDATORIOS`.
pub const MENSAJES_ANTES_DE_RECORDAR: i64 = 10;
pub const MINUTOS_ANTES_DE_RECORDAR: i64 = 15;
pub const MINUTOS_ENTRE_RECORDATORIOS: i64 = 30;
pub const RECORDATORIO: &str = "Reminder: nothing has been saved to long-term memory (Omoikane) in this session yet. If you have learned something worth knowing next time (a non-obvious trap, a decision and its reasons, how the user wants things done), save it with `remember`. If not, ignore this.";

/// Fragmento de cada memoria inyectada; el agente pide `get` si quiere el resto.
const FRAGMENTO: usize = 200;

/// Muletillas de conversación que no dicen de qué va el mensaje (además de las palabras vacías de
/// la búsqueda). Ya sin tildes.
const CHARLA: &[&str] = &[
    "pues", "dale", "vale", "okey", "ahora", "bueno", "hay", "pon", "mira", "eso", "esto", "esta", "este", "todo", "toda",
    "todos", "algo", "hacer", "hace", "haz", "tengo", "tiene", "puedo", "puede", "quiero", "mejor", "bien", "mal", "asi",
    "aqui", "solo", "tambien", "cosas", "cosa", "otro", "otra", "otros", "otras", "ver", "sea", "ser", "estar", "porque",
    "cuando", "donde", "cual", "igual", "menos", "mas", "nada", "sin", "entonces", "creo", "hecho", "dime", "dame",
    "sigue", "seguimos", "listo", "perfecto", "gracias", "favor", "vamos", "ves", "voy", "van", "ido", "ese", "esa",
    "esos", "esas", "aun", "repite", "repito", "prueba", "mismo", "misma", "cada", "pero", "the", "this", "that", "with",
    "please", "now", "can", "you", "it", "let", "lets", "ok", "sabes", "sabe", "saber", "tengas", "tienes", "tenemos",
    "puedes", "podemos", "quieres", "necesito", "necesitas", "explica", "explicame", "ayuda", "ayudame", "hazme",
    "dimelo", "mirar", "revisa", "revisar", "estoy", "estas", "estamos", "pensando", "parece",
    "know", "want", "need", "tell", "explain", "help", "check", "look", "think",
];

/// Lo que Claude Code manda al hook por la entrada estándar.
#[derive(Debug, Default, Deserialize)]
pub struct Entrada {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub source: String,
}

fn sin_tildes(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            c => c,
        })
        .collect()
}

/// Las palabras del mensaje que dicen de qué va: sin vacías ni muletillas, sin cifras sueltas y con
/// al menos 3 letras.
pub fn utiles(texto: &str) -> Vec<String> {
    buscar::terminos(texto)
        .iter()
        .map(|t| sin_tildes(t))
        .filter(|t| t.chars().count() >= 3 && !t.chars().all(|c| c.is_ascii_digit()) && !CHARLA.contains(&t.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn fichas(m: &Memoria) -> BTreeSet<String> {
    let texto = format!("{} {} {} {}", m.title, m.content, m.tags.join(" "), m.paths.join(" "));
    sin_tildes(&texto.to_lowercase()).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
}

fn esta(t: &str, fichas: &BTreeSet<String>) -> bool {
    if t.chars().count() >= PREFIJO_MIN { fichas.iter().any(|p| p.starts_with(t)) } else { fichas.contains(t) }
}

/// Memorias del ámbito (el proyecto y las globales) y en cuántas aparece cada término.
fn frecuencias(db: &Connection, project: &str, terminos: &[String]) -> Result<(f64, Vec<f64>)> {
    let n: i64 = db.query_row("SELECT count(*) FROM memory WHERE project = ?1 OR project = ''", [project], |f| f.get(0))?;
    let mut q = db.prepare(
        "SELECT count(*) FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND (m.project = ?2 OR m.project = '')",
    )?;
    let mut df = Vec::with_capacity(terminos.len());
    for t in terminos {
        let consulta = if t.chars().count() >= PREFIJO_MIN { format!("\"{t}\"*") } else { format!("\"{t}\"") };
        df.push(q.query_row(params![consulta, project], |f| f.get::<_, i64>(0)).unwrap_or(0) as f64);
    }
    Ok((n as f64, df))
}

/// Las memorias que tienen que ver con `prompt`, de la más a la menos, con su cobertura.
pub fn relevantes(db: &mut Connection, project: &str, prompt: &str) -> Result<Vec<(Memoria, f64)>> {
    let terminos = utiles(prompt);
    if terminos.len() < TERMINOS_MIN {
        return Ok(Vec::new());
    }
    let (n, df) = frecuencias(db, project, &terminos)?;
    // IDF de Lucene, siempre positivo: con la de BM25 clásica, en una base de una o dos memorias todo
    // pesa cero y no se inyectaría nada justo cuando alguien empieza.
    // Una palabra que no está en ninguna memoria pesa como una que estuviera en una sola, no más: con
    // el IDF entero, en una base pequeña hundía la cobertura de la memoria que sí es del tema («estoy
    // pensando en cambiar la pasarela de pago a Stripe» no traía la decisión sobre la pasarela). Sin
    // contarlas en absoluto, los mensajes largos traían algo casi siempre (53 % de los reales).
    let idf: Vec<f64> = df.iter().map(|d| (1.0 + (n - d.max(1.0) + 0.5) / (d.max(1.0) + 0.5)).ln()).collect();
    let total: f64 = idf.iter().sum();
    if total <= 0.0 {
        return Ok(Vec::new());
    }
    let raro = (FRACCION_RARA * n).max(1.0);
    // Solo para calibrar (`examples/fase4.rs`): sin estas variables valen las constantes.
    let leer = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let cobertura_min = leer("OMOIKANE_CALIBRAR_COBERTURA").unwrap_or(COBERTURA_MIN);
    let raros_min = leer("OMOIKANE_CALIBRAR_RAROS").map_or(RAROS_MIN, |v| v as usize);
    // 30 candidatas: una nota larga que es justo la del tema puede quedar lejos en BM25.
    let o = Opciones { ambito: Ambito::Proyecto(project.to_string()), kind: None, tags: vec![], limite: 30 };
    // Solo palabras: el vector no mejoró la búsqueda de forma medible (PLAN.md §5) y aquí el ruido
    // cuesta más. Así el hook tampoco abre el modelo.
    let candidatos = buscar::buscar_sin_apuntar(db, &terminos.join(" "), &o)?;
    let mut v: Vec<(Memoria, f64, usize)> = candidatos
        .into_iter()
        .filter_map(|r| {
            let f = fichas(&r.memoria);
            let titulo = fichas(&Memoria { content: String::new(), paths: vec![], tags: vec![], ..r.memoria.clone() });
            let (mut peso, mut raros) = (0.0, 0);
            for (i, t) in terminos.iter().enumerate() {
                if esta(t, &f) {
                    peso += idf[i];
                    raros += usize::from(df[i] <= raro);
                }
            }
            let cobertura = peso / total;
            // Cuántas palabras del mensaje están en el título: desempata a favor de la memoria que es
            // justo del tema («building my future» → la nota de Building My Future).
            let en_titulo = terminos.iter().filter(|t| esta(t, &titulo)).count();
            (raros >= raros_min && cobertura >= cobertura_min).then_some((r.memoria, cobertura, en_titulo))
        })
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
    let mejor = v.first().map(|x| x.1).unwrap_or(0.0);
    v.retain(|x| x.1 >= RELATIVO_A_LA_MEJOR * mejor);
    v.truncate(MAXIMO);
    Ok(v.into_iter().map(|(m, c, _)| (m, c)).collect())
}

/// Memorias del ámbito con alguna ruta que aparece en el mensaje (la ruta entera, o el nombre del
/// fichero si tiene al menos 5 caracteres).
pub fn por_rutas(db: &Connection, project: &str, prompt: &str) -> Result<Vec<Memoria>> {
    let texto = prompt.to_lowercase().replace('\\', "/");
    let mut q = db.prepare(
        "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id
         WHERE m.project = ?1 OR m.project = ''",
    )?;
    let rutas: Vec<(String, String)> = q.query_map([project], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut ids: Vec<String> = Vec::new();
    for (id, ruta) in rutas {
        let ruta = ruta.to_lowercase();
        let nombre = ruta.rsplit('/').next().unwrap_or(&ruta);
        let sale = texto.contains(&ruta) || (nombre.chars().count() >= 5 && texto.contains(nombre));
        if sale && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.iter().take(MAXIMO).map(|id| memoria::obtener(db, id)).collect()
}

fn ya_inyectadas(db: &Connection, sesion: &str) -> Result<BTreeSet<String>> {
    let mut q = db.prepare("SELECT memory_id FROM recall_log WHERE session = ?1")?;
    let v = q.query_map([sesion], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

fn apuntar(db: &mut Connection, sesion: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for id in ids {
        tx.execute(
            &format!("INSERT OR IGNORE INTO recall_log(session, memory_id, at) VALUES (?1, ?2, {})", memoria::AHORA),
            params![sesion, id],
        )?;
    }
    tx.commit()?;
    buscar::apuntar_recuperadas(db, ids.iter().map(String::as_str))
}

fn linea(m: &Memoria) -> String {
    let plano = m.content.split_whitespace().collect::<Vec<_>>().join(" ");
    let fragmento = if plano.chars().count() > FRAGMENTO {
        format!("{}…", plano.chars().take(FRAGMENTO).collect::<String>())
    } else {
        plano
    };
    let rutas = if m.paths.is_empty() { String::new() } else { format!(" ({})", m.paths.join(", ")) };
    let global = if m.project.is_empty() { " [global]" } else { "" };
    format!("- [{}]{global}{rutas} {}: {fragmento} (id {})", m.kind, m.title, &m.id[..12])
}

/// Al empezar la sesión (o tras compactar): el contexto que el agente debe tener siempre.
pub fn al_empezar(db: &mut Connection, project: &str, e: &Entrada) -> Result<Option<String>> {
    // Una sesión que empieza o se compacta ya no tiene en su contexto lo inyectado antes.
    db.execute(
        "DELETE FROM recall_log WHERE session = ?1 OR at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')",
        [&e.session_id],
    )?;
    let total: i64 =
        db.query_row("SELECT count(*) FROM memory WHERE project = ?1 OR project = ''", [project], |f| f.get(0))?;
    let contexto = memoria::contexto(db, project, 30)?;
    // Tras compactar sigue siendo la misma sesión: no se reinicia el reloj del recordatorio.
    db.execute(
        &format!(
            "INSERT INTO session(session, project, started_at) VALUES (?1, ?2, {ahora})
             ON CONFLICT(session) DO UPDATE SET project = excluded.project,
               started_at = CASE WHEN ?3 = 'compact' THEN session.started_at ELSE excluded.started_at END,
               prompts = CASE WHEN ?3 = 'compact' THEN session.prompts ELSE 0 END,
               reminded_at = CASE WHEN ?3 = 'compact' THEN session.reminded_at ELSE NULL END",
            ahora = memoria::AHORA
        ),
        params![e.session_id, project, e.source],
    )?;
    let mut s = String::from("<omoikane>\n");
    s.push_str(&format!(
        "Long-term memory (Omoikane) holds {total} memories for this project and globally. Use the omoikane tools: `search` before working on something, `remember` what is worth knowing next time.\n"
    ));
    if !contexto.is_empty() {
        s.push_str("The user's preferences and the pinned memories:\n");
        for m in &contexto {
            s.push_str(&linea(m));
            s.push('\n');
        }
    }
    s.push_str("</omoikane>");
    let ids: Vec<String> = contexto.iter().map(|m| m.id.clone()).collect();
    apuntar(db, &e.session_id, &ids)?;
    Ok(Some(s))
}

/// En cada mensaje: lo que tiene que ver con él y no se ha inyectado ya en esta sesión, o nada.
pub fn al_preguntar(db: &mut Connection, project: &str, e: &Entrada) -> Result<Option<String>> {
    let vistas = ya_inyectadas(db, &e.session_id)?;
    let mut elegidas: Vec<Memoria> = Vec::new();
    for m in por_rutas(db, project, &e.prompt)?.into_iter().chain(relevantes(db, project, &e.prompt)?.into_iter().map(|x| x.0)) {
        if !vistas.contains(&m.id) && !elegidas.iter().any(|x| x.id == m.id) && elegidas.len() < MAXIMO {
            elegidas.push(m);
        }
    }
    let recordar = toca_recordar(db, project, &e.session_id)?;
    if elegidas.is_empty() && !recordar {
        return Ok(None);
    }
    let mut s = String::from("<omoikane>\n");
    if !elegidas.is_empty() {
        s.push_str("Memories from previous sessions that may be relevant (`get` an id for the whole memory):\n");
        for m in &elegidas {
            s.push_str(&linea(m));
            s.push('\n');
        }
    }
    if recordar {
        s.push_str(RECORDATORIO);
        s.push('\n');
    }
    s.push_str("</omoikane>");
    let ids: Vec<String> = elegidas.iter().map(|m| m.id.clone()).collect();
    apuntar(db, &e.session_id, &ids)?;
    Ok(Some(s))
}

/// Cuenta el mensaje y dice si toca recordar al agente que guarde: sesión de al menos
/// `MINUTOS_ANTES_DE_RECORDAR` y `MENSAJES_ANTES_DE_RECORDAR` mensajes, ninguna memoria del proyecto
/// (ni global) guardada o cambiada desde que empezó, y sin otro recordatorio en los últimos
/// `MINUTOS_ENTRE_RECORDATORIOS`. Una sesión que no pasó por `SessionStart` (hooks añadidos a
/// medias) empieza a contar aquí.
fn toca_recordar(db: &Connection, project: &str, sesion: &str) -> Result<bool> {
    if sesion.is_empty() {
        return Ok(false);
    }
    let ahora = memoria::AHORA;
    db.execute(
        &format!(
            "INSERT INTO session(session, project, started_at, prompts) VALUES (?1, ?2, {ahora}, 1)
             ON CONFLICT(session) DO UPDATE SET prompts = session.prompts + 1"
        ),
        params![sesion, project],
    )?;
    let toca: bool = db.query_row(
        "SELECT s.prompts >= ?2
                AND s.started_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3)
                AND (s.reminded_at IS NULL OR s.reminded_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))
                AND NOT EXISTS (SELECT 1 FROM memory m
                                WHERE (m.project = s.project OR m.project = '') AND m.updated_at >= s.started_at)
             FROM session s WHERE s.session = ?1",
        params![
            sesion,
            MENSAJES_ANTES_DE_RECORDAR,
            format!("-{MINUTOS_ANTES_DE_RECORDAR} minutes"),
            format!("-{MINUTOS_ENTRE_RECORDATORIOS} minutes"),
        ],
        |f| f.get(0),
    )?;
    if toca {
        db.execute(&format!("UPDATE session SET reminded_at = {ahora} WHERE session = ?1"), [sesion])?;
    }
    Ok(toca)
}

/// La respuesta del hook para Claude Code.
pub fn salida(evento: &str, contexto: &str) -> String {
    serde_json::json!({ "hookSpecificOutput": { "hookEventName": evento, "additionalContext": contexto } }).to_string()
}

/// Cuántas memorias se han inyectado ya en una sesión.
pub fn inyectadas(db: &Connection, sesion: &str) -> Result<usize> {
    Ok(db.query_row("SELECT count(*) FROM recall_log WHERE session = ?1", [sesion], |f| f.get::<_, i64>(0))? as usize)
}
