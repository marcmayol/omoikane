//! Recall automático: lo que los hooks de Claude Code inyectan sin que el agente lo pida.
//!
//! - Al empezar (o tras compactar): las preferencias y las fijadas del proyecto y globales.
//! - En cada mensaje: las memorias que de verdad tienen que ver con él, o ninguna. Inyectar de más
//!   ensucia el contexto del agente, así que el criterio es estricto y está calibrado con mensajes
//!   reales: el mensaje tiene que tener al menos `TERMINOS_MIN` palabras útiles, la
//!   memoria tiene que compartir al menos `RAROS_MIN` palabras poco frecuentes en la base, y esas
//!   palabras tienen que pesar (por IDF) al menos `COBERTURA_MIN` de lo que pesa el mensaje.
//! - Además, las memorias cuyas rutas se mencionan en el mensaje (los `gotcha` de un fichero).
//!
//! Cada memoria se inyecta una vez por sesión (tabla `recall_log`), salvo tras compactar.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
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
/// Parte del peso IDF del mensaje que tiene que estar en la memoria. Era 0,40; recalibrado el
/// 28-oct-2026 con 80 preguntas escritas a ciegas sobre 127 notas reales: con 0,40 se inyectaba algo
/// en 8 de 20 preguntas sin respuesta, con 0,50 en 2 (y 36 de 60 con la nota buena frente a 40). Con
/// los mensajes reales, del 21 % al 11 % con algo inyectado, y los positivos del 99 % al 98 %.
pub const COBERTURA_MIN: f64 = 0.50;
/// Solo las que se acercan a la mejor: si una destaca, va sola. Con 0,85 y como mucho 2 (antes 0,7 y
/// 3), las mismas notas buenas con un 39 % menos de memorias de más; validado con un tercer conjunto
/// de preguntas a ciegas y otra IA de juez: precisión del 62 % al 68 %.
pub const RELATIVO_A_LA_MEJOR: f64 = 0.85;
/// Canal por significado (paráfrasis que no comparten palabras raras): una memoria entra también si
/// su parecido (vectores centrados) llega a `SIMILITUD_RECALL` y sus palabras cubren al menos
/// `COBERTURA_CON_VECTOR` del mensaje. Las dos cosas a la vez: el vector solo mete ruido. Solo en
/// mensajes de hasta `LARGO_MAX_CON_VECTOR` caracteres: el vector de un texto largo (un resumen de
/// sesión, un encargo automático) es la media de todo y se parece a cualquier cosa.
/// Calibrado el 28-oct-2026 con 80 preguntas a ciegas: paráfrasis con la nota buena de 3 a 7 de 15,
/// el mismo ruido en las preguntas sin respuesta (2 de 20); con mensajes reales, de 7 % a 12 % con
/// algo inyectado, casi todo útil al revisarlo a mano.
pub const SIMILITUD_RECALL: f32 = 0.35;
pub const COBERTURA_CON_VECTOR: f64 = 0.25;
pub const LARGO_MAX_CON_VECTOR: usize = 500;
/// Con EmbeddingGemma (`significado`), en los mensajes que miran el significado (`mira_significado`):
/// lo que traen las palabras o el modelo ligero se queda fuera si no está entre las `VETO_PUESTO` más
/// parecidas al mensaje, y la más parecida entra si llega a `ANADIR_GEMMA` y le saca `VENTAJA_GEMMA`
/// a la segunda. Los dos, relativos: por puesto y por ventaja, no por un coseno fijo, que cambia con
/// el idioma y la forma de escribir (en el benchmark, inglés de documentación, la nota buena ronda
/// 0,7; en los mensajes reales de Marc, español con erratas, 0,35, y un veto en 0,5 quitaba lo útil).
/// Lo que sale por una ruta del mensaje no se toca. Elegido con las 250 preguntas a ciegas de
/// `omoikane-bench` (3.000 notas), ajustando con la mitad y comprobando con la otra, y revisado a
/// mano con los 300 últimos mensajes reales: la nota buena en preguntas pasa del 93 % al 94 %, en
/// paráfrasis del 5 % al 29 %, y en las preguntas sin respuesta mete algo el 8 % de las veces en vez
/// del 40 %.
pub const VETO_PUESTO: usize = 3;
pub const ANADIR_GEMMA: f32 = 0.6;
pub const VENTAJA_GEMMA: f32 = 0.03;
/// Cuánto se espera al servicio de significado en cada mensaje. Si tarda más (CPU ocupada), el
/// recall sigue solo con palabras, como en la 1.1: esperar más haría lento cada mensaje para quitar,
/// en el mejor caso, una memoria de ruido. No es en silencio: se apunta (`apuntar_consulta`) y lo
/// enseñan `omoi doctor` y la traza (`OMOIKANE_TRAZA`).
const PRESUPUESTO_GEMMA: std::time::Duration = std::time::Duration::from_millis(250);
/// Política D (`omoi-revision-12/2-cuando.md`): el modelo grande solo se consulta para **vetar** lo
/// que ya trajeron las palabras, en mensajes de más de `buscar::CORTA` palabras y de hasta
/// `LARGO_MAX_GEMMA` caracteres. Con los 300 mensajes reales de Marc, de 237 consultas a 20 con los
/// mismos 6 cambios; en el benchmark (3.000) el mismo ruido (7,5 %), a cambio del rescate de
/// paráfrasis en el recall (0,29 → 0,09), que sigue en la búsqueda. Por encima de ~300 caracteres el
/// vector no llega dentro de `PRESUPUESTO_GEMMA` (37 de 237 mensajes reales esperaban 250 ms para nada).
pub const LARGO_MAX_GEMMA: usize = 300;
/// Como mucho, tantas memorias por mensaje.
pub const MAXIMO: usize = 2;
/// Desde cuántas letras una palabra cuenta también como prefijo («memoria» → «memorias»). Con menos,
/// «pon» coincidía con «pone», «ponga»…
pub const PREFIJO_MIN: usize = 5;
/// Recordatorio de guardar: a partir de estos mensajes y minutos de sesión sin que se haya guardado
/// ni cambiado ninguna memoria del proyecto, y como mucho una vez cada `MINUTOS_ENTRE_RECORDATORIOS`.
pub const MENSAJES_ANTES_DE_RECORDAR: i64 = 10;
pub const MINUTOS_ANTES_DE_RECORDAR: i64 = 15;
pub const MINUTOS_ENTRE_RECORDATORIOS: i64 = 30;
pub const RECORDATORIO: &str = "Reminder: nothing has been saved to long-term memory (Omoikane) in this session yet. If you have learned something worth knowing next time (a non-obvious trap, a decision and its reasons, how the user wants things done), save it with `remember`, including where unfinished work was left (what is done, what is next; tag `status`, and `update` the project's existing one). If not, ignore this.";

/// Con `OMOIKANE_TRAZA` puesta, el recall escribe en stderr una línea `TRAZA <paso> <json>` por paso:
/// lo que vio, cómo lo puntuó y qué hizo con el modelo de significado (y por qué no lo consultó, o
/// por qué no llegó su respuesta). Para medir y para entender un caso; sin la variable no cuesta nada.
fn trazando() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()))
}

fn traza(etiqueta: &str, valor: impl FnOnce() -> serde_json::Value) {
    if trazando() {
        eprintln!("TRAZA {etiqueta} {}", valor());
    }
}

/// `MAXIMO` y `RELATIVO_A_LA_MEJOR`, salvo al calibrar (`examples`, bancos de pruebas).
fn maximo() -> usize {
    std::env::var("OMOIKANE_CALIBRAR_MAXIMO").ok().and_then(|v| v.parse().ok()).unwrap_or(MAXIMO)
}

fn relativo() -> f64 {
    std::env::var("OMOIKANE_CALIBRAR_RELATIVO").ok().and_then(|v| v.parse().ok()).unwrap_or(RELATIVO_A_LA_MEJOR)
}

/// Fragmento de cada memoria inyectada; el agente pide `get` si quiere el resto.
const FRAGMENTO: usize = 200;
/// Fragmento de cada memoria de estado al empezar sesión: más largo, para que retomar no pida `get`.
const FRAGMENTO_ESTADO: usize = 400;
/// Cuántos proyectos con estado se enseñan al empezar sesión (desde una carpeta general salen varios).
pub const ESTADOS: usize = 5;

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

/// Lo que el agente manda al hook por la entrada estándar. Claude Code, Codex y Gemini CLI usan
/// estos nombres; Cursor llama `conversation_id` a la sesión y no manda `cwd` sino `workspace_roots`.
#[derive(Debug, Default, Deserialize)]
pub struct Entrada {
    #[serde(default, alias = "conversation_id", alias = "sessionId", alias = "conversationId")]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, alias = "workspacePaths")]
    pub workspace_roots: Vec<String>,
    /// La conversación (Claude Code y la mayoría: un `.jsonl`), para sacar candidatas al cerrar.
    #[serde(default, alias = "transcriptPath")]
    pub transcript_path: String,
}

/// En qué formato responde el hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Formato {
    /// Claude Code y Codex: `hookSpecificOutput.additionalContext`.
    Claude,
    /// Gemini CLI: lo mismo, con `BeforeAgent` como evento del mensaje.
    Gemini,
    /// Cursor: `additional_context` (solo al empezar la sesión).
    Cursor,
    /// Copilot (CLI y VS Code): `additionalContext` arriba del todo.
    Copilot,
    /// Antigravity: un paso de sistema en `injectSteps`.
    Antigravity,
    /// Solo el texto (Kiro, Kimi Code).
    Plain,
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

/// Cuánto del contenido cuenta para el recall. Una memoria muy larga contiene casi cualquier palabra
/// y «cubre» cualquier mensaje: con 127 notas reales, una de 87 KB salía en la mitad de las
/// inyecciones de ruido. Con 6.000 caracteres, los mensajes reales con algo inyectado bajan del 12 %
/// al 7 % sin perder ningún acierto con preguntas escritas a ciegas. Se probó 3.000, mejor con las
/// preguntas de ajuste, pero con las de comprobación (que preguntan por detalles del medio y del
/// final de las notas) perdía 7 de 35 naturales: estaba sobreajustado. Guardar avisa de que una
/// memoria más larga conviene partirla.
pub const LARGO_PARA_RECALL: usize = 6000;

fn fichas(m: &Memoria) -> BTreeSet<String> {
    let largo = std::env::var("OMOIKANE_CALIBRAR_LARGO").ok().and_then(|v| v.parse().ok()).unwrap_or(LARGO_PARA_RECALL);
    let contenido: String = m.content.chars().take(largo).collect();
    let texto = format!("{} {} {} {}", m.title, contenido, m.tags.join(" "), m.paths.join(" "));
    sin_tildes(&texto.to_lowercase()).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
}

fn esta(t: &str, fichas: &BTreeSet<String>) -> bool {
    if t.chars().count() >= PREFIJO_MIN { fichas.iter().any(|p| p.starts_with(t)) } else { fichas.contains(t) }
}

/// Memorias del ámbito (el proyecto, su espacio y las globales; todas desde una carpeta general) y en
/// cuántas aparece cada término.
fn frecuencias(db: &Connection, project: &str, terminos: &[String]) -> Result<(f64, Vec<f64>)> {
    let ambito = Ambito::de(db, project)?;
    let n = memoria::cuantas(db, &ambito)?;
    let mut q = if memoria::ve_todas(db, &ambito) {
        db.prepare("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH ?1")?
    } else {
        db.prepare(&format!(
            "SELECT count(*) FROM memory_fts JOIN memory m {} ON m.n = memory_fts.rowid
             WHERE memory_fts MATCH ?1 AND {}",
            memoria::por_ambito(db),
            ambito.sql()
        ))?
    };
    let mut df = Vec::with_capacity(terminos.len());
    for t in terminos {
        let consulta = if t.chars().count() >= PREFIJO_MIN { format!("\"{t}\"*") } else { format!("\"{t}\"") };
        df.push(q.query_row(params![consulta], |f| f.get::<_, i64>(0)).unwrap_or(0) as f64);
    }
    Ok((n as f64, df))
}

/// Qué parte del peso IDF de las palabras de `texto` está en cada memoria (IDF del ámbito, como en
/// `relevantes`). La usa también el aviso de duplicados.
pub fn cobertura<'a>(db: &Connection, project: &str, texto: &str, memorias: impl Iterator<Item = &'a Memoria>) -> Result<Vec<f64>> {
    let terminos = utiles(texto);
    let frecuencias = frecuencias(db, project, &terminos)?;
    Ok(cobertura_con(&terminos, &frecuencias, memorias))
}

/// `cobertura` con las frecuencias de `terminos` (las de `utiles(texto)`) ya calculadas.
fn cobertura_con<'a>(terminos: &[String], (n, df): &(f64, Vec<f64>), memorias: impl Iterator<Item = &'a Memoria>) -> Vec<f64> {
    let n = *n;
    let idf: Vec<f64> = df.iter().map(|d| (1.0 + (n - d.max(1.0) + 0.5) / (d.max(1.0) + 0.5)).ln()).collect();
    let total: f64 = idf.iter().sum();
    memorias
        .map(|m| {
            let f = fichas(m);
            let dentro: f64 = terminos.iter().zip(&idf).filter(|(t, _)| esta(t, &f)).map(|(_, i)| i).sum();
            if total > 0.0 { dentro / total } else { 0.0 }
        })
        .collect()
}

/// Las memorias que tienen que ver con `prompt`, de la más a la menos, con su cobertura.
pub fn relevantes(db: &mut Connection, project: &str, prompt: &str) -> Result<Vec<(Memoria, f64)>> {
    let terminos = utiles(prompt);
    if terminos.len() < TERMINOS_MIN {
        return Ok(Vec::new());
    }
    let frecuencias = frecuencias(db, project, &terminos)?;
    relevantes_con(db, project, &terminos, &frecuencias)
}

/// `relevantes` con los términos del mensaje (al menos `TERMINOS_MIN`) y sus frecuencias ya calculados.
fn relevantes_con(db: &mut Connection, project: &str, terminos: &[String], (n, df): &(f64, Vec<f64>)) -> Result<Vec<(Memoria, f64)>> {
    let n = *n;
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
    traza("terminos", || serde_json::json!({"n": n, "terminos": terminos, "df": df, "idf": idf, "raros_msg": df.iter().filter(|d| **d <= raro).count()}));
    // Solo para calibrar (`examples/fase4.rs`): sin estas variables valen las constantes.
    let leer = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let cobertura_min = leer("OMOIKANE_CALIBRAR_COBERTURA").unwrap_or(COBERTURA_MIN);
    let raros_min = leer("OMOIKANE_CALIBRAR_RAROS").map_or(RAROS_MIN, |v| v as usize);
    // Si el mensaje no tiene tantas palabras raras, ninguna memoria puede compartirlas: no hace falta
    // buscar candidatas.
    if df.iter().filter(|d| **d <= raro).count() < raros_min {
        return Ok(Vec::new());
    }
    // 30 candidatas: una nota larga que es justo la del tema puede quedar lejos en BM25.
    let o = Opciones { ambito: Ambito::de(db, project)?, kind: None, tags: vec![], limite: 30 };
    // Solo palabras: el vector no mejoró la búsqueda de forma medible y aquí el ruido
    // cuesta más. Así el hook tampoco abre el modelo.
    let candidatos = buscar::buscar_sin_apuntar(db, &terminos.join(" "), &o)?;
    let mut traza_c: Vec<serde_json::Value> = Vec::new();
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
            if trazando() {
                traza_c.push(serde_json::json!({"id": r.memoria.id, "bm25": r.score, "pal": r.matched_words, "cob": cobertura, "raros": raros, "tit": en_titulo, "largo": r.memoria.content.chars().count()}));
            }
            (raros >= raros_min && cobertura >= cobertura_min).then_some((r.memoria, cobertura, en_titulo))
        })
        .collect();
    traza("candidatas", || serde_json::Value::Array(traza_c));
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
    let mejor = v.first().map(|x| x.1).unwrap_or(0.0);
    v.retain(|x| x.1 >= relativo() * mejor);
    v.truncate(maximo());
    Ok(v.into_iter().map(|(m, c, _)| (m, c)).collect())
}

/// Si el canal por significado mira este mensaje (ver `SIMILITUD_RECALL`): si no, ni hace falta abrir
/// el modelo.
fn mira_significado(prompt: &str) -> bool {
    let sim_min = std::env::var("OMOIKANE_CALIBRAR_SIM_RECALL").ok().and_then(|v| v.parse::<f64>().ok()).map_or(SIMILITUD_RECALL, |v| v as f32);
    sim_min <= 1.0 && prompt.chars().count() <= LARGO_MAX_CON_VECTOR && utiles(prompt).len() >= TERMINOS_MIN
}

/// Las que se parecen al mensaje por significado y comparten parte de sus palabras (ver
/// `SIMILITUD_RECALL`), de la más a la menos parecida.
pub fn por_significado(db: &Connection, project: &str, prompt: &str, modelo: &Modelo) -> Result<Vec<Memoria>> {
    por_significado_con(db, project, prompt, modelo, None)
}

/// `por_significado`, con las frecuencias de `utiles(prompt)` si ya se calcularon.
fn por_significado_con(db: &Connection, project: &str, prompt: &str, modelo: &Modelo, frecuencias: Option<&(f64, Vec<f64>)>) -> Result<Vec<Memoria>> {
    if !mira_significado(prompt) {
        return Ok(Vec::new());
    }
    let leer = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let sim_min = leer("OMOIKANE_CALIBRAR_SIM_RECALL").map_or(SIMILITUD_RECALL, |v| v as f32);
    let cob_min = leer("OMOIKANE_CALIBRAR_COB_VECTOR").unwrap_or(COBERTURA_CON_VECTOR);
    let v = crate::vectores::parecidas_en(db, modelo, prompt, &Ambito::de(db, project)?, None, sim_min, MAXIMO)?;
    if v.is_empty() {
        return Ok(Vec::new());
    }
    let c = match frecuencias {
        Some(f) => cobertura_con(&utiles(prompt), f, v.iter().map(|x| &x.0)),
        None => cobertura(db, project, prompt, v.iter().map(|x| &x.0))?,
    };
    Ok(v.into_iter().zip(c).filter(|(_, c)| *c >= cob_min).map(|((m, _), _)| m).collect())
}

/// Memorias del ámbito con alguna ruta que aparece en el mensaje (la ruta entera, o el nombre del
/// fichero si tiene al menos 5 caracteres).
pub fn por_rutas(db: &Connection, project: &str, prompt: &str) -> Result<Vec<Memoria>> {
    let texto = prompt.to_lowercase().replace('\\', "/");
    let sale = |ruta: &str| {
        let ruta = ruta.to_lowercase();
        let nombre = ruta.rsplit('/').next().unwrap_or(&ruta);
        texto.contains(&ruta) || (nombre.chars().count() >= 5 && texto.contains(nombre))
    };
    // Lo normal es que ninguna ruta salga en el mensaje: se mira solo en `memory_path`, sin juntarla con
    // `memory` (que recorre las memorias del ámbito: 90 ms con 100.000).
    let mut todas = db.prepare("SELECT DISTINCT path FROM memory_path")?;
    let alguna = todas.query_map([], |f| f.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|r| sale(r));
    if !alguna {
        return Ok(Vec::new());
    }
    let mut q = db.prepare(&format!(
        "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id
         WHERE {}",
        Ambito::de(db, project)?.sql()
    ))?;
    let rutas: Vec<(String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut ids: Vec<String> = Vec::new();
    for (id, ruta) in rutas {
        if sale(&ruta) && !ids.contains(&id) {
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

/// Apunta lo inyectado en la sesión, con el proyecto desde el que se pidió (0 desde una carpeta
/// general o un proyecto sin memorias aún).
fn apuntar(db: &mut Connection, sesion: &str, ids: &[String], proyecto: i64) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for id in ids {
        tx.execute(
            &format!("INSERT OR IGNORE INTO recall_log(session, memory_id, at, project_id) VALUES (?1, ?2, {}, ?3)", memoria::AHORA),
            params![sesion, id, proyecto],
        )?;
    }
    buscar::apuntar_recuperadas_en(&tx, &ids.iter().map(String::as_str).collect::<Vec<_>>(), None)?;
    tx.commit()?;
    Ok(())
}

/// Dónde está una memoria, si no es del proyecto de la sesión (`actual`): `[global]`, `[space x]` o
/// `[project x]` (vista desde una carpeta general o desde otro proyecto del espacio).
fn donde(m: &Memoria, actual: i64) -> String {
    let oficios = crate::oficios::de_memoria(m);
    if !oficios.is_empty() {
        let para = crate::oficios::para_de(&m.tags).map(|p| format!(", {p} projects")).unwrap_or_default();
        format!(" [trade {}{para}]", oficios.join(", "))
    } else if m.global() {
        " [global]".to_string()
    } else if m.de_espacio() {
        format!(" [space {}]", m.space)
    } else if m.project_id != actual {
        format!(" [project {}]", crate::proyecto::nombre_de(&m.project, &m.project_key))
    } else {
        String::new()
    }
}

/// El id del proyecto de la sesión (-1 si aún no tiene nada; 0 nunca: lo global no es una sesión).
fn proyecto_actual(db: &Connection, project: &str) -> i64 {
    match crate::proyecto::resolver(db, project) {
        Ok(Some((id, _))) if !project.is_empty() => id,
        _ => -1,
    }
}

/// `actual` es el proyecto de la sesión: una memoria de otro proyecto (vista desde una carpeta general)
/// dice de cuál es. `propio` es el autor de este equipo: una memoria de otra persona dice de quién es,
/// para que el agente la trate con cuidado.
fn linea(m: &Memoria, actual: i64, propio: Option<&str>) -> String {
    let fragmento = fragmento(&m.content, FRAGMENTO);
    let rutas = if m.paths.is_empty() { String::new() } else { format!(" ({})", m.paths.join(", ")) };
    let global = donde(m, actual);
    // Sin autor propio (este equipo no apunta el suyo), lo que lleva autor viene de otro equipo.
    let de = match propio {
        _ if m.author.is_empty() => String::new(),
        Some(yo) if m.author == yo => String::new(),
        _ => format!(" [by {}]", m.author),
    };
    format!("- [{}{}]{global}{de}{rutas} {}: {fragmento} (id {})", m.kind, marca(m), m.title, m.id.chars().take(12).collect::<String>())
}

/// La marca de una memoria que verificó una persona (`verificado`), detrás del tipo: `[decision, verified]`.
fn marca(m: &Memoria) -> &'static str {
    if m.verified.is_some() { ", verified" } else { "" }
}

/// La advertencia de `revisar` sobre una memoria, para el final de su línea (o nada).
fn nota(notas: &std::collections::HashMap<String, String>, m: &Memoria) -> String {
    notas.get(&m.id).map(|n| format!(" ⚠ {n}")).unwrap_or_default()
}

/// El contenido en una línea y cortado a `n` caracteres.
fn fragmento(texto: &str, n: usize) -> String {
    let plano = texto.split_whitespace().collect::<Vec<_>>().join(" ");
    if plano.chars().count() > n {
        format!("{}…", plano.chars().take(n).collect::<String>())
    } else {
        plano
    }
}

/// Al empezar la sesión (o tras compactar): el contexto que el agente debe tener siempre.
/// El autor de este equipo, si la sincronización lo apunta: con él se distingue lo de otras personas.
/// `actual`: el proyecto de la sesión (-1 o 0, ninguno): en uno de un espacio, el autor con el que este
/// equipo firma en ese espacio.
fn autor_propio(db: &Connection, actual: i64) -> Option<String> {
    // El que se apunta en cada memoria guardada aquí (el del repositorio de su espacio o el general).
    let autor = if actual > 0 { memoria::autor_de(db, actual, None) } else { memoria::autor_en(db, "") };
    if let Some(a) = autor.ok().filter(|a| !a.is_empty()) {
        return Some(a);
    }
    let ruta = db.path().filter(|p| !p.is_empty())?;
    crate::sync::leer_config(std::path::Path::new(ruta)).ok().flatten().and_then(|c| c.author)
}

pub fn al_empezar(db: &mut Connection, project: &str, e: &Entrada) -> Result<Option<String>> {
    // Una vez por sesión: los avisos y los enlaces al día (tras actualizar, o lo que trajo la sync).
    crate::revisar::al_dia(db)?;
    let actualizado = crate::actualizar::aviso();
    // El proyecto de la sesión queda apuntado (con su clave, calculada una vez) y su `origin` al día. Si
    // es la carpeta nueva de un proyecto que se movió (el mismo `origin`), se dice una vez.
    let movido = if crate::proyecto::es_general(project) { None } else { crate::proyecto::registrar_avisando(db, project)?.1 };
    let ambito = Ambito::de(db, project)?;
    // Sus oficios, mirando sus ficheros (raíz y un nivel): con ellos llegan las memorias de oficio.
    // Opcional: si falla, la sesión empieza sin ellos.
    let oficios = match ambito.proyecto() {
        Some(id) => crate::oficios::al_dia(db, id, project).unwrap_or_else(|x| {
            eprintln!("omoikane: trades: {x:#}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    let actual = proyecto_actual(db, project);
    let propio = autor_propio(db, actual);
    // Una sesión que empieza o se compacta ya no tiene en su contexto lo inyectado antes. Lo que se
    // borra queda resumido en `memory_use` (cuántas veces y desde qué proyecto se recibió cada memoria).
    {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let caduca = "session = ?1 OR at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')";
        tx.execute(
            &format!(
                "INSERT INTO memory_use(memory_id, project_id, veces, ultima)
                 SELECT memory_id, project_id, count(*), max(at) FROM recall_log WHERE {caduca} GROUP BY memory_id, project_id
                 ON CONFLICT(memory_id, project_id) DO UPDATE SET veces = veces + excluded.veces, ultima = max(ultima, excluded.ultima)"
            ),
            [&e.session_id],
        )?;
        tx.execute(&format!("DELETE FROM recall_log WHERE {caduca}"), [&e.session_id])?;
        tx.commit()?;
    }
    // El historial de ficheros es opcional: si falla, la sesión empieza igual.
    if let Err(x) = crate::riesgo::al_empezar(db, &e.session_id) {
        eprintln!("omoikane: history: {x:#}");
    }
    let total = memoria::cuantas(db, &ambito)?;
    let contexto = memoria::contexto(db, &ambito, 30)?;
    // La primera sesión aquí en un proyecto de un espacio: lo que tiene el equipo (antes de apuntar la
    // sesión, que es lo que dice si es la primera). Opcional: un error solo deja la sesión sin él.
    let llegada = crate::llegada::bienvenida(db, &ambito, &e.session_id, propio.as_deref()).unwrap_or_else(|x| {
        eprintln!("omoikane: welcome: {x:#}");
        None
    });
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
    // Un proyecto en pausa no se sincroniza hasta que la persona decida: que no se olvide.
    if let Some(p) = crate::sync::aviso_de_pausa_agente(db) {
        s.push_str(&p);
        s.push('\n');
    }
    if let Some(vieja) = &movido {
        s.push_str(&format!("This project used to be in {vieja}, which no longer exists: its memories follow this folder now (same git origin). Tell the user once.\n"));
    }
    if crate::proyecto::es_general(project) {
        s.push_str(&format!(
            "Long-term memory (Omoikane) holds {total} memories across all projects. Use the omoikane tools: `search` before working on something, `remember` what is worth knowing next time.\nThis folder is not a project, so search covers every project. When a memory is about one project, pass its folder as `project` (or put its absolute paths in `paths`); otherwise it is saved as global.\n"
        ));
    } else {
        s.push_str(&format!(
            "Long-term memory (Omoikane) holds {total} memories for this project and globally. Use the omoikane tools: `search` before working on something, `remember` what is worth knowing next time.\n"
        ));
    }
    if !oficios.is_empty() {
        s.push_str(&format!(
            "This project's trades (its stack, from its files): {}. A trap or know-how about the stack itself, useful to any project of that trade and with nothing of this project or its client in it, goes with `trade` (e.g. `trade: \"{}\"`) and reaches every {} project; anything about this project stays in the project.\n",
            oficios.join(", "),
            oficios[0],
            oficios[0]
        ));
    }
    if let Some(l) = &llegada {
        s.push_str(l);
    }
    if !contexto.is_empty() {
        let notas = crate::revisar::notas(db, &ambito, &contexto.iter().collect::<Vec<_>>())?;
        s.push_str("The user's preferences and the pinned memories:\n");
        for m in &contexto {
            s.push_str(&linea(m, actual, propio.as_deref()));
            s.push_str(&nota(&notas, m));
            s.push('\n');
        }
    }
    // En qué punto se quedó cada proyecto (la memoria `status` más reciente de cada uno), con un buen
    // trozo del contenido: «sigamos con X» se responde sin buscar. Tampoco cuentan como inyectadas.
    let estados: Vec<Memoria> = memoria::estados(db, &ambito, ESTADOS)?
        .into_iter()
        .filter(|m| !contexto.iter().any(|c| c.id == m.id))
        .collect();
    if !estados.is_empty() {
        let notas = crate::revisar::notas(db, &ambito, &estados.iter().collect::<Vec<_>>())?;
        s.push_str("Where work was left (the latest `status` memory of each project, newest first):\n");
        for m in &estados {
            let linea = linea_corta(m, actual, propio.as_deref());
            s.push_str(&format!("{linea}{}: {}\n", nota(&notas, m), fragmento(&m.content, FRAGMENTO_ESTADO)));
        }
    }
    // Las decisiones que esperan a la persona (etiqueta `open`): salen en cada sesión hasta que decida.
    // Tampoco cuentan como inyectadas: el recall aún puede traerlas enteras cuando toque.
    // Opcional como el historial: un error solo deja la sesión sin la lista.
    let abiertas: Vec<Memoria> = memoria::abiertas(db, &ambito, usize::MAX)
        .unwrap_or_else(|x| {
            eprintln!("omoikane: open decisions: {x:#}");
            Vec::new()
        })
        .into_iter()
        .filter(|m| !contexto.iter().any(|c| c.id == m.id))
        .collect();
    if !abiertas.is_empty() {
        // Las bloqueantes van primero (`memoria::abiertas`) y marcadas: el trabajo que depende de ellas
        // espera a la persona; las demás son para revisar después.
        let bloqueantes = abiertas.iter().filter(|m| m.bloqueante()).count();
        if bloqueantes > 0 {
            s.push_str(&format!("Decisions waiting for the user (decisions tagged `open`; the {bloqueantes} marked BLOCKING first, then the rest, newest first. BLOCKING: don't go on with the work that depends on it, ask the user now; the rest are for later: mention them at a natural moment. When they decide, `update` the memory with the answer and drop the `open` and `blocking` tags):\n"));
        } else {
            s.push_str("Decisions waiting for the user (decisions tagged `open`, newest first; for later: mention them to the user at a natural moment; when they decide, `update` the memory with the answer and drop the `open` tag):\n");
        }
        for m in abiertas.iter().take(ABIERTAS) {
            let linea = linea_corta(m, actual, propio.as_deref());
            let linea = if m.bloqueante() { linea.replacen("- ", "- BLOCKING ", 1) } else { linea };
            s.push_str(&citado(&format!("{linea}: {}\n", fragmento(&m.content, FRAGMENTO_ABIERTA))));
        }
        if abiertas.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`search` with the tag `open` for the rest).\n", abiertas.len() - ABIERTAS));
        }
    }
    // Lo último en lo que se trabajó: solo los títulos, para que «sigamos con lo de X» no obligue al
    // agente a explorar. No se apuntan como inyectadas: su contenido aún puede llegar con el recall.
    // Un estado que ya no es el último de su proyecto tampoco sale: llevaría a retomar desde lo viejo.
    let ultimas: Vec<Memoria> = memoria::recientes(db, &ambito, None, 50)?
        .into_iter()
        .filter(|m| !contexto.iter().any(|c| c.id == m.id) && !m.tags.iter().any(|t| t == memoria::ESTADO))
        .filter(|m| !abiertas.iter().any(|a| a.id == m.id))
        .take(RECIENTES)
        .collect();
    if !ultimas.is_empty() {
        s.push_str("Last worked on (newest first; `get` an id for the whole memory):\n");
        let notas = crate::revisar::notas(db, &ambito, &ultimas.iter().collect::<Vec<_>>())?;
        for m in &ultimas {
            s.push_str(&linea_corta(m, actual, propio.as_deref()));
            s.push_str(&nota(&notas, m));
            s.push('\n');
        }
    }
    // Lo que quizá valía la pena guardar de sesiones anteriores (7.2): el agente decide.
    let cs = crate::candidatas::para_empezar(db, project, &e.session_id)?;
    if !cs.is_empty() {
        s.push_str("From your previous sessions here, possibly worth keeping (Omoikane picked them without understanding them: save the ones that are worth it with `remember`, in the user's own words and language, merging into existing memories where they fit; ask the user if unsure; ignore the rest, they won't come back):\n");
        for c in &cs {
            s.push_str(&format!("- [{}] {} ({})\n", c.kind, c.text.chars().take(900).collect::<String>(), c.created_at.get(..10).unwrap_or("")));
        }
    }
    // La revisión, si se ha acumulado (una vez al día): el agente propone, el usuario decide. Es
    // opcional: si falla (la base ocupada por una sync larga), solo se pierde el aviso, no el bloque.
    if let Some(a) = crate::revisar::aviso_al_empezar(db, &ambito, total as usize).ok().flatten() {
        s.push_str(&a);
        s.push('\n');
    }
    if let Some(a) = &actualizado {
        s.push_str(a);
        s.push('\n');
    }
    // El modelo de significado, si la memoria ya lo necesita: lo descarga o lo deja indexando, y la
    // primera vez que se enciende solo lo dice.
    if let Some(a) = crate::significado::al_empezar(db) {
        s.push_str(&a);
        s.push('\n');
    }
    s.push_str(CONTINUAR);
    s.push_str("</omoikane>");
    crate::documentos::al_empezar(db, project, &e.session_id);
    let ids: Vec<String> = contexto.iter().map(|m| m.id.clone()).collect();
    apuntar(db, &e.session_id, &ids, ambito.proyecto().unwrap_or(0))?;
    Ok(Some(s))
}

/// Texto que no escribe Omoikane (un fichero de la documentación, una memoria) dentro del bloque
/// `<omoikane>`: sus `<omoikane` y `</omoikane`, en mayúsculas o minúsculas, pasan a `‹omoikane`, para
/// que no puedan cerrar el bloque ni abrir otro.
pub fn citado(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    let mut resto = t;
    while let Some(i) = resto.find('<') {
        s.push_str(&resto[..i]);
        let tras = &resto[i + 1..];
        let tras = tras.strip_prefix('/').unwrap_or(tras);
        let etiqueta = tras.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("omoikane"));
        s.push(if etiqueta { '‹' } else { '<' });
        resto = &resto[i + 1..];
    }
    s.push_str(resto);
    s
}

/// Cuántas memorias recientes se enseñan al empezar la sesión.
pub const RECIENTES: usize = 5;

/// Cuántas decisiones abiertas se enseñan al empezar la sesión; del resto solo se dice cuántas son.
pub const ABIERTAS: usize = 10;
/// Fragmento de cada decisión abierta: lo justo para recordar la pregunta.
const FRAGMENTO_ABIERTA: usize = 160;

/// Qué hacer con una petición ambigua y cuándo dejar dicho en qué punto se quedó algo.
const CONTINUAR: &str = "If the user asks to continue something and it is unclear what, check the list above and do at most one `search`; if that doesn't settle it, ask the user right away instead of exploring files. When you finish a piece of work or the session winds down, save where it was left (what is done, what is next) with `remember` and the tag `status`: one per project, so if the project already has one (listed above), `update` it instead of saving another. When a product or business decision is the user's and you can't ask now, save it as kind decision with the tag `open` (question, options, what you assumed meanwhile), keep working, and when you finish a long piece of work tell the user which ones you left. Add the tag `blocking` too only when the work can't go on without the answer (no safe assumption, or a wrong guess would be costly to undo): then stop that part and ask. Without it, the decision is for later.\n";

/// Una memoria en una línea, sin contenido: fecha, tipo, proyecto si no es el de la sesión, título e id.
fn linea_corta(m: &Memoria, actual: i64, propio: Option<&str>) -> String {
    let donde = donde(m, actual);
    let de = match propio {
        Some(yo) if !m.author.is_empty() && m.author != yo => format!(" [by {}]", m.author),
        _ => String::new(),
    };
    let fecha = m.updated_at.get(..10).unwrap_or(&m.updated_at);
    format!("- {fecha} [{}{}]{donde}{de} {} (id {})", m.kind, marca(m), m.title, m.id.chars().take(12).collect::<String>())
}

/// En cada mensaje: lo que tiene que ver con él y no se ha inyectado ya en esta sesión, o nada.
/// Lo que tiene que cubrir del mensaje una memoria enlazada para traerla con la que la enlaza: la mitad
/// del listón del recall, porque el enlace ya dice que tiene que ver.
pub const LISTON_VECINA: f64 = 0.25;
/// Y si es la memoria inyectada la que la enlaza: quien la escribió ya dijo que van juntas.
pub const LISTON_ENLAZADA: f64 = 0.10;

/// Cuánto de una receta se inyecta (los pasos enteros, no un fragmento).
const LARGO_RECETA: usize = 4000;

/// Las palabras de la línea `when:` de una receta (separadas por comas), en minúsculas y sin tildes.
pub fn disparadores(m: &Memoria) -> Vec<String> {
    m.content
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            let n = sin_tildes(&l.to_lowercase());
            n.strip_prefix("when:").or_else(|| n.strip_prefix("cuando:")).map(|r| r.to_string())
        })
        .flat_map(|r| r.split(',').map(|w| w.trim().trim_matches(['"', '\'', '«', '»', '`', '.']).to_string()).collect::<Vec<_>>())
        .filter(|w| w.chars().count() >= 3)
        .collect()
}

/// La receta que pide el mensaje: una `procedure` del ámbito con una de sus palabras `when:` en él
/// (palabra entera: «publica» no es «publicación») y algo más que diga que es esa receta. Una palabra
/// suelta vale para muchas cosas: en los mensajes reales de Marc, «publica un post en LinkedIn» o
/// «pues publica» traían «Publicar una versión de Omoikane» (9 de los 19 ruidos que quedaban con el
/// modelo, `4-valor.md`). Basta con una de estas:
/// - una frase de su `when:` («saca versión») o dos palabras distintas de la lista;
/// - que la receta sea del proyecto de la sesión (en la carpeta de Omoikane, «publica» es publicarlo);
/// - que el mensaje diga además algo de su título, sus etiquetas o su proyecto («publica la versión»,
///   «publica omoikane»), sin contar las propias palabras `when:`.
fn receta(db: &Connection, project: &str, prompt: &str) -> Result<Option<Memoria>> {
    let n = sin_tildes(&prompt.to_lowercase());
    let tiene = |w: &str| {
        n.match_indices(w).any(|(i, _)| {
            n[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) && n[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric())
        })
    };
    let recetas = memoria::recientes(db, &Ambito::de(db, project)?, Some(memoria::Tipo::Procedure), 200)?;
    if recetas.is_empty() {
        return Ok(None);
    }
    let actual = proyecto_actual(db, project);
    let del_mensaje = utiles(prompt);
    Ok(recetas.into_iter().find(|m| {
        let lista = disparadores(m);
        let dichas: Vec<&String> = lista.iter().filter(|w| tiene(w)).collect();
        if dichas.is_empty() {
            return false;
        }
        // Cada palabra por su raíz (5 letras): «publica» y «publicar» son la misma.
        let raiz = |w: &str| w.chars().take(PREFIJO_MIN).collect::<String>();
        let distintas: BTreeSet<String> = dichas.iter().map(|w| raiz(w)).collect();
        if dichas.iter().any(|w| w.contains(' ')) || distintas.len() >= 2 || (actual > 0 && m.project_id == actual) {
            return true;
        }
        let raices: BTreeSet<String> = lista.iter().filter(|w| !w.contains(' ')).map(|w| raiz(w)).collect();
        let es_disparador = |p: &str| raices.iter().any(|r| p.starts_with(r.as_str()));
        let de_que_es: BTreeSet<String> = {
            let nombre = if m.project_id > 0 { crate::proyecto::nombre_de(&m.project, &m.project_key) } else { String::new() };
            let solo = Memoria { content: nombre, paths: vec![], ..m.clone() };
            fichas(&solo).into_iter().filter(|p| !es_disparador(p)).collect()
        };
        del_mensaje.iter().any(|t| !es_disparador(t) && esta(t, &de_que_es))
    }))
}

/// Cuántas memorias más parecidas pide el recall al servicio: las 3 del veto, la 1.ª y la 2.ª para
/// añadir, y el coseno de casi cualquiera que traigan las palabras (las demás cuentan como 0).
const PARECIDAS_RECALL: usize = 40;

/// Por qué no se consulta el modelo grande en este mensaje (política D, ver `LARGO_MAX_GEMMA`), o
/// `None` si se consulta: solo cuando hay algo que vetar.
fn sin_gemma(prompt: &str, elegidas: &[Memoria], de_rutas: &BTreeSet<String>) -> Option<&'static str> {
    if prompt.chars().count() > LARGO_MAX_GEMMA {
        Some("long")
    } else if prompt.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count() <= buscar::CORTA {
        // Un mensaje corto son palabras clave: ahí el significado se queda corto y el veto quitaba la
        // nota buena (del 94 % al 88 % en el benchmark) sin quitar ruido.
        Some("short")
    } else if !elegidas.iter().any(|m| vetable(m, de_rutas)) {
        Some("nothing_to_veto")
    } else {
        None
    }
}

/// Lo que el modelo puede quitar: lo que no sale por una ruta del mensaje ni es una preferencia. Una
/// preferencia se aplica sin parecerse al mensaje («lamina en Creality Print, no en Orca» con «ábrelo
/// en creality»): en los mensajes reales de Marc, el 22-27 % de las preferencias vetadas eran útiles,
/// frente al 6-7 % de los hechos (`4-valor.md`).
fn vetable(m: &Memoria, de_rutas: &BTreeSet<String>) -> bool {
    !de_rutas.contains(&m.id) && m.kind != memoria::Tipo::Preference
}

type Respuesta = std::result::Result<crate::significado::Cosenos, crate::significado::Fallo>;

/// Pide al servicio de significado las memorias del ámbito más parecidas al mensaje (y el coseno de
/// `ids`), dentro de `PRESUPUESTO_GEMMA`.
fn pedir_gemma(db: &Connection, project: &str, prompt: &str, ids: &[String]) -> Result<Respuesta> {
    use crate::significado as s;
    let Some(ruta) = db.path().filter(|r| !r.is_empty()).map(std::path::PathBuf::from) else { return Ok(Err(s::Fallo::Apagado)) };
    let ambito = Ambito::de(db, project)?;
    Ok(s::parecidas_o_fallo(&ruta, prompt, &ambito, None, PARECIDAS_RECALL, ids, PRESUPUESTO_GEMMA).map(|p| {
        let mut v = p.top;
        for x in p.pedidas {
            if !v.iter().any(|y| y.0 == x.0) {
                v.push(x);
            }
        }
        v
    }))
}

/// Apunta en la base (`meta`, local) cada consulta del recall al modelo y las que no tuvieron
/// respuesta, con la última y su motivo, para que `omoi doctor` lo enseñe: sin respuesta a tiempo el
/// recall va solo por palabras, y eso no debe pasar en silencio. Nunca falla el recall por esto.
fn apuntar_consulta(db: &Connection, fallo: Option<crate::significado::Fallo>) {
    let sumar = "INSERT INTO meta(k, v) VALUES (?1, '1') ON CONFLICT(k) DO UPDATE SET v = CAST(v AS INTEGER) + 1";
    let _ = db.execute(sumar, ["sig_recall_consultas"]);
    if let Some(f) = fallo {
        let _ = db.execute(sumar, ["sig_recall_sin_respuesta"]);
        let _ = db.execute(
            &format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('sig_recall_ultimo_fallo', {} || ' ' || ?1)", memoria::AHORA),
            [f.como_texto()],
        );
    }
}

/// Cuántas veces consultó el recall el modelo en esta base, cuántas no tuvo respuesta y la última que
/// no (fecha y motivo).
pub fn consultas_al_modelo(db: &Connection) -> (i64, i64, Option<String>) {
    let leer = |k: &str| db.query_row("SELECT v FROM meta WHERE k = ?1", [k], |f| f.get::<_, String>(0)).ok();
    let n = |k: &str| leer(k).and_then(|v| v.parse().ok()).unwrap_or(0);
    (n("sig_recall_consultas"), n("sig_recall_sin_respuesta"), leer("sig_recall_ultimo_fallo"))
}

pub fn al_preguntar(db: &mut Connection, project: &str, e: &Entrada, modelo: Option<&Modelo>) -> Result<Option<String>> {
    al_preguntar_abriendo(db, project, e, &mut || modelo)
}

/// `al_preguntar`, abriendo el modelo solo si hace falta (`modelo` se llama como mucho una vez): la
/// mayoría de los mensajes no llegan al canal por significado, y abrirlo cuesta unos milisegundos.
pub fn al_preguntar_abriendo<'m>(
    db: &mut Connection,
    project: &str,
    e: &Entrada,
    modelo: &mut dyn FnMut() -> Option<&'m Modelo>,
) -> Result<Option<String>> {
    let actual = proyecto_actual(db, project);
    let propio = autor_propio(db, actual);
    let vistas = ya_inyectadas(db, &e.session_id)?;
    let mut elegidas: Vec<Memoria> = Vec::new();
    let la_receta = receta(db, project, &e.prompt)?.filter(|m| !vistas.contains(&m.id));
    let elegir = |m: Memoria, elegidas: &mut Vec<Memoria>| {
        if !vistas.contains(&m.id) && !elegidas.iter().any(|x| x.id == m.id) && la_receta.as_ref().is_none_or(|r| r.id != m.id) && elegidas.len() < maximo() {
            elegidas.push(m);
        }
    };
    // Las frecuencias de las palabras del mensaje, una vez para las palabras y el significado.
    let mut terminos = utiles(&e.prompt);
    let mut frecuencias = if terminos.len() >= TERMINOS_MIN { Some(frecuencias(db, project, &terminos)?) } else { None };
    // Un nombre de proyecto mal dictado («trova» por Troba) no está en ninguna memoria: se busca por el
    // nombre bueno (`nombres::mal_escritos`). Solo se miran los nombres si hay palabras que no están
    // en ninguna memoria.
    if let Some((_, df)) = &frecuencias
        && df.contains(&0.0)
    {
        let en_ninguna: Vec<bool> = df.iter().map(|d| *d == 0.0).collect();
        // Solo los proyectos que se ven desde aquí: el nombre de otro cliente nunca entra en el mensaje.
        let arreglos = crate::nombres::mal_escritos(db, &Ambito::de(db, project)?, &terminos, &en_ninguna)?;
        if !arreglos.is_empty() {
            traza("nombres", || serde_json::json!(arreglos.iter().map(|(i, p)| (terminos[*i].clone(), p.join(" "))).collect::<Vec<_>>()));
            for (i, partes) in arreglos.iter().rev() {
                terminos.splice(*i..*i + 1, partes.iter().filter(|p| p.chars().count() >= 3).cloned());
            }
            terminos.sort();
            terminos.dedup();
            frecuencias = Some(crate::recall::frecuencias(db, project, &terminos)?);
        }
    }
    let por_palabras = match &frecuencias {
        Some(f) => relevantes_con(db, project, &terminos, f)?,
        None => Vec::new(),
    };
    let rutas = por_rutas(db, project, &e.prompt)?;
    let de_rutas: BTreeSet<String> = rutas.iter().map(|m| m.id.clone()).collect();
    for m in rutas.into_iter().chain(por_palabras.into_iter().map(|x| x.0)) {
        elegir(m, &mut elegidas);
    }
    // El canal por significado va detrás: si las rutas y las palabras ya llenan el cupo, lo que traiga
    // no entraría y no hace falta mirarlo (ni leer los vectores).
    if elegidas.len() < maximo()
        && mira_significado(&e.prompt)
        && let Some(modelo) = modelo()
    {
        for m in por_significado_con(db, project, &e.prompt, modelo, frecuencias.as_ref())? {
            elegir(m, &mut elegidas);
        }
    }
    traza("antes_gemma", || serde_json::json!({"elegidas": elegidas.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "rutas": de_rutas, "mira": mira_significado(&e.prompt)}));
    // El modelo grande, solo para vetar (política D) y después de las palabras.
    let cosenos = match sin_gemma(&e.prompt, &elegidas, &de_rutas) {
        Some(motivo) => {
            traza("gemma", || serde_json::json!({"asked": false, "why": motivo}));
            None
        }
        None if !crate::significado::en_uso(db) => {
            traza("gemma", || {
                let e = crate::significado::estado(db);
                serde_json::json!({"asked": false, "why": "not_in_use", "state": e, "installed": crate::significado::modelo().is_some()})
            });
            None
        }
        None => {
            let ids: Vec<String> = elegidas.iter().map(|m| m.id.clone()).collect();
            let r = pedir_gemma(db, project, &e.prompt, &ids)?;
            apuntar_consulta(db, r.as_ref().err().copied());
            match r {
                Ok(c) => {
                    traza("gemma", || serde_json::json!({"asked": true, "cosines": c}));
                    Some(c)
                }
                Err(f) => {
                    // Sin respuesta (CPU ocupada, servicio arrancando o indexando): solo palabras, como
                    // en la 1.1. Queda apuntado para `omoi doctor`.
                    traza("gemma", || serde_json::json!({"asked": true, "failed": f.como_texto()}));
                    None
                }
            }
        }
    };
    if let Some(cosenos) = cosenos {
        let de: std::collections::HashMap<&str, f32> = cosenos.iter().map(|(id, c)| (id.as_str(), *c)).collect();
        let arriba: BTreeSet<&str> = cosenos.iter().take(VETO_PUESTO).map(|(id, _)| id.as_str()).collect();
        // Sin vector al día (recién guardada o cambiada, aún sin calcular) no se puede juzgar: se queda.
        let sin_vector = |id: &str| -> bool {
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc WHERE s.id = ?1)",
                [id],
                |f| f.get::<_, bool>(0),
            )
            .unwrap_or(false)
        };
        elegidas.retain(|m| !vetable(m, &de_rutas) || arriba.contains(m.id.as_str()) || sin_vector(&m.id));
        if let [(id, c1), (_, c2), ..] = cosenos.as_slice()
            && *c1 >= ANADIR_GEMMA
            && c1 - c2 >= VENTAJA_GEMMA
        {
            // Con el cupo lleno, sustituye a la que menos se parece (salvo las de rutas).
            if elegidas.len() >= maximo() && !elegidas.iter().any(|m| m.id == *id) {
                let peor = elegidas
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !de_rutas.contains(&m.id))
                    .map(|(i, m)| (i, de.get(m.id.as_str()).copied().unwrap_or(0.0)))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, c)) = peor
                    && c < *c1
                {
                    elegidas.remove(i);
                }
            }
            elegir(memoria::obtener(db, id)?, &mut elegidas);
        }
    }
    traza("final", || serde_json::json!(elegidas.iter().map(|m| m.id.clone()).collect::<Vec<_>>()));
    // Una memoria enlazada con las elegidas, si es una trampa o una preferencia y tiene que ver (7.4).
    let vecina = if elegidas.is_empty() { None } else { crate::enlaces::vecina_para(db, project, &e.prompt, &elegidas, &vistas, LISTON_ENLAZADA, LISTON_VECINA)? };
    let recordar = toca_recordar(db, project, &e.session_id)?;
    // El historial de los ficheros que nombra el mensaje, sacado del diario (`riesgo`).
    // Opcional, como la documentación: si falla, el recall sigue sin él.
    let historia = crate::riesgo::al_preguntar(db, project, &e.session_id, &e.prompt).unwrap_or_else(|x| {
        eprintln!("omoikane: history: {x:#}");
        None
    });
    let docs = crate::documentos::al_preguntar(db, project, &e.session_id, &e.prompt);
    if elegidas.is_empty() && !recordar && la_receta.is_none() && historia.is_none() && docs.is_none() {
        return Ok(None);
    }
    let mut s = String::from("<omoikane>\n");
    if let Some(r) = &la_receta {
        let pasos: String = r.content.chars().take(LARGO_RECETA).collect();
        s.push_str(&format!(
            "A saved procedure for this (id {}): follow it step by step as a checklist; if a step fails or has changed, `update` the procedure with what you learned.\n### {}\n{pasos}\n",
            r.id.chars().take(12).collect::<String>(),
            r.title
        ));
    }
    if !elegidas.is_empty() {
        let notas = crate::revisar::notas(db, &Ambito::de(db, project)?, &elegidas.iter().collect::<Vec<_>>())?;
        s.push_str("Memories from previous sessions that may be relevant (`get` an id for the whole memory):\n");
        for m in &elegidas {
            s.push_str(&linea(m, actual, propio.as_deref()));
            s.push_str(&nota(&notas, m));
            s.push('\n');
        }
        if let Some((v, de)) = &vecina {
            s.push_str(&format!("{} (linked from {})\n", linea(v, actual, propio.as_deref()), de.chars().take(12).collect::<String>()));
        }
    }
    if let Some(h) = &historia {
        s.push_str(h);
        s.push('\n');
    }
    if let Some(d) = &docs {
        s.push_str(d);
    }
    if recordar {
        s.push_str(RECORDATORIO);
        s.push('\n');
    }
    s.push_str("</omoikane>");
    let ids: Vec<String> = elegidas.iter().chain(&la_receta).chain(vecina.iter().map(|(v, _)| v)).map(|m| m.id.clone()).collect();
    apuntar(db, &e.session_id, &ids, actual.max(0))?;
    Ok(Some(s))
}

/// Cuenta el mensaje y dice si toca recordar al agente que guarde: sesión de al menos
/// `MINUTOS_ANTES_DE_RECORDAR` y `MENSAJES_ANTES_DE_RECORDAR` mensajes, ninguna memoria del proyecto
/// (ni global) guardada o cambiada desde que empezó, y sin otro recordatorio en los últimos
/// `MINUTOS_ENTRE_RECORDATORIOS`. Una sesión que no pasó por `SessionStart` (hooks añadidos a
/// medias) empieza a contar aquí. Por `memory_updated`: las cambiadas desde que empezó la sesión son
/// pocas, y por el índice del ámbito SQLite leía todas las del proyecto (25 ms con 100.000).
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
        &format!(
            "SELECT s.prompts >= ?2
                    AND s.started_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3)
                    AND (s.reminded_at IS NULL OR s.reminded_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))
                    AND NOT EXISTS (SELECT 1 FROM memory m INDEXED BY memory_updated WHERE {} AND m.updated_at >= s.started_at)
             FROM session s WHERE s.session = ?1",
            Ambito::de(db, project)?.sql()
        ),
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
    salida_en(Formato::Claude, evento, contexto)
}

/// La respuesta del hook en el formato del agente. `evento` es el de Claude Code
/// (`SessionStart` o `UserPromptSubmit`).
pub fn salida_en(formato: Formato, evento: &str, contexto: &str) -> String {
    match formato {
        Formato::Claude => serde_json::json!({ "hookSpecificOutput": { "hookEventName": evento, "additionalContext": contexto } }),
        Formato::Gemini => {
            let evento = if evento == "UserPromptSubmit" { "BeforeAgent" } else { evento };
            serde_json::json!({ "hookSpecificOutput": { "hookEventName": evento, "additionalContext": contexto } })
        }
        Formato::Cursor => serde_json::json!({ "additional_context": contexto }),
        Formato::Copilot => serde_json::json!({ "additionalContext": contexto }),
        Formato::Antigravity => serde_json::json!({ "injectSteps": [{ "systemMessage": { "systemMessage": contexto } }] }),
        Formato::Plain => return contexto.to_string(),
    }
    .to_string()
}

/// Cuántas memorias se han inyectado ya en una sesión.
pub fn inyectadas(db: &Connection, sesion: &str) -> Result<usize> {
    Ok(db.query_row("SELECT count(*) FROM recall_log WHERE session = ?1", [sesion], |f| f.get::<_, i64>(0))? as usize)
}
