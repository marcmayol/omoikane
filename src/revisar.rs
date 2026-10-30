//! Lo que conviene revisar en la memoria (Fase 7.1, y los enlaces rotos de la 7.4): memorias que otra más nueva contradice, estados
//! que se han quedado atrás, rutas que ya no existen, memorias que nunca se usan y notas demasiado
//! largas para el recall. Sin modelo: palabras, fechas y versiones.
//!
//! Las desfasadas se calculan por parejas y se guardan en `review`; se recalculan solas cuando la
//! base cambia (por una huella de `memory`), entre por donde entre el cambio (CLI, MCP, sync,
//! importar). Lo demás depende de la hora o del disco y se mira al leer. «Está bien así» se apunta en
//! la misma tabla y vale hasta que la memoria vuelva a cambiar.
//!
//! Quien sabe qué memorias ha escrito (remember, update, forget, restore, absorb) llama a `al_dia_de`,
//! que solo rehace las parejas y los enlaces en los que entran ellas, con candidatas sacadas del
//! índice de texto; la sincronización, la importación y la lectura, que no lo saben, a `al_dia`, que
//! rehace todo. Con 50.000 memorias, guardar pasó de 2,9 s a unos 15 ms de revisión. Por partes, qué
//! palabra es rara se toma del último recálculo entero (en `meta`), que se repite cuando el número
//! de memorias cambia más de un 5 %.
//!
//! Calibrado con las 137 memorias reales de Marc del 29-oct-2026: sale el caso que lo motivó (la
//! nota de Omoikane que seguía en la 0.2.0 frente a la de estado en la 0.10.0) y ningún falso
//! positivo. Las primeras versiones del detector daban cinco avisos, cuatro falsos: «pendiente» en
//! una y «hecho» en otra, hablando de cosas distintas.

use crate::memoria::{AHORA, Ambito, Memoria};
use crate::recall::LARGO_PARA_RECALL;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const DESFASADA: &str = "outdated";
pub const ESTADO_VIEJO: &str = "stale_status";
pub const RUTA_ROTA: &str = "broken_path";
pub const SIN_USO: &str = "unused";
pub const LARGA: &str = "too_long";
pub const ENLACE_ROTO: &str = "broken_link";
/// Una que el agente dijo que otra sustituye (`supersedes`), o que contradice a otra (`contradicts`).
pub const SUSTITUIDA: &str = crate::veredictos::SUSTITUIDA;
pub const CONTRADICE: &str = crate::veredictos::CONTRADICE;
/// Le ha llegado la fecha de revisión de su tipo (`dias_para_revisar`).
pub const POR_REVISAR: &str = "due";
/// Lleva algo que parece un secreto (`secretos`): guardada antes de la 0.19.0 o con `OMOIKANE_REDACT=0`.
pub const SECRETO: &str = "secret";
const TIPOS: [&str; 10] = [SECRETO, SUSTITUIDA, CONTRADICE, DESFASADA, ENLACE_ROTO, ESTADO_VIEJO, POR_REVISAR, RUTA_ROTA, SIN_USO, LARGA];

/// Días sin tocar a partir de los que una memoria `status` con cambios posteriores se avisa.
pub const DIAS_ESTADO: f64 = 7.0;
/// Días desde que se creó sin recordarse nunca.
pub const DIAS_SIN_USO: f64 = 30.0;

/// Cada cuántos días sin tocar conviene comprobar que una memoria sigue siendo verdad. Una decisión o
/// una receta se quedan viejas antes que un hecho o una trampa; una preferencia, mientras el usuario
/// no diga otra cosa, vale siempre, y una `status` ya tiene su propio aviso. «Está bien así» vuelve a
/// poner el reloj a cero.
pub fn dias_para_revisar(m: &Memoria) -> Option<f64> {
    use crate::memoria::Tipo;
    if m.tags.iter().any(|t| t == crate::memoria::ESTADO) {
        return None;
    }
    match m.kind {
        Tipo::Decision | Tipo::Procedure => Some(120.0),
        Tipo::Fact | Tipo::Gotcha => Some(180.0),
        Tipo::Preference => None,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Aviso {
    pub kind: String,
    pub memory_id: String,
    pub title: String,
    pub project: String,
    /// La otra memoria (la más nueva, en las desfasadas); vacío si no hay.
    pub other_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_title: Option<String>,
    /// Qué se ha visto, en inglés y corto (lo leen el agente y la vista). En un veredicto lleva
    /// también el motivo y «by <autor>».
    pub detail: String,
    /// En un veredicto (`superseded`, `contradicts`): por qué lo dijo quien lo dijo, y quién.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// En una sustituida, cuántos saltos hay hasta `other_id` cuando es el final de una cadena (0 si
    /// es la sustituta directa).
    #[serde(skip_serializing_if = "es_cero")]
    pub via: usize,
}

fn es_cero(n: &usize) -> bool {
    *n == 0
}

/// El detalle de un veredicto para el agente y la CLI: qué relación, el motivo y quién lo dijo.
fn detalle_veredicto(base: &str, v: &crate::veredictos::Veredicto, saltos: usize) -> String {
    let mut d = base.to_string();
    if !v.reason.is_empty() {
        d.push_str(": ");
        d.push_str(&crate::veredictos::motivo_corto(&v.reason));
    }
    if saltos > 1 {
        d.push_str(&format!(" (via {saltos})"));
    }
    if !v.author.is_empty() {
        d.push_str(&format!(" · by {}", v.author));
    }
    d
}

/// Cambia con cualquier escritura: memorias nuevas, cambiadas, movidas u olvidadas.
fn huella(db: &Connection) -> Result<String> {
    Ok(db.query_row(
        "SELECT count(*) || '|' || COALESCE(max(updated_at), '') || '|' || COALESCE(max(hlc), '') || '|' || COALESCE(sum(length(project)), 0) FROM memory",
        [],
        |f| f.get(0),
    )?)
}

/// Lo guardado del último recálculo entero: cuántas memorias había y las palabras que no eran raras.
const CLAVE_COMUNES: &str = "revision_comunes";
/// Cuántas escrituras se han revisado por partes desde el último recálculo entero.
const CLAVE_PARCIALES: &str = "revision_parciales";
/// Por debajo de este número de memorias se recalcula siempre entero: es barato y el tope de las raras
/// aún se mueve con cada memoria (`tope_raras`).
pub const MINIMO_POR_PARTES: usize = 2_000;
/// Cuánto puede cambiar el número de memorias desde el último recálculo entero antes de rehacerlo:
/// hasta ahí, qué palabra es rara se toma de entonces.
const DERIVA: f64 = 0.05;
/// Más memorias cambiadas de una vez que esto (una importación a mano, un `absorb` enorme) se recalcula entero.
const MAXIMO_POR_PARTES: usize = 200;

/// Recalcula las desfasadas si la base cambió desde la última vez, sin saber qué cambió: todo entero.
/// Es lo que corre tras la sincronización y la importación, y al leer (`avisos`, el recall) por si
/// algo escribió sin avisar.
pub fn al_dia(db: &Connection) -> Result<()> {
    let h = huella(db)?;
    let antes: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |f| f.get(0)).optional()?;
    if antes.as_deref() == Some(h.as_str()) {
        return Ok(());
    }
    rehacer(db, &h)
}

/// Si la revisión estaba al día justo antes de escribir. Se toma antes de la escritura y se le pasa
/// a [`al_dia_de`]: solo así se sabe que lo único nuevo son las memorias que se escriben.
pub struct Antes {
    /// La huella guardada, si lo que se ve de ella sin leer la tabla entera es lo de ahora.
    huella: Option<String>,
}

/// Lo que da la huella sin `max(hlc)`: número de memorias, último cambio y proyectos, que salen del
/// índice `memory_project` sin leer la tabla (el `max(hlc)` la lee entera: 30 ms con 50.000 memorias).
fn huella_ligera(db: &Connection) -> Result<(String, String, String)> {
    Ok(db.query_row("SELECT count(*), COALESCE(max(updated_at), ''), COALESCE(sum(length(project)), 0) FROM memory", [], |f| {
        Ok((f.get::<_, i64>(0)?.to_string(), f.get(1)?, f.get::<_, i64>(2)?.to_string()))
    })?)
}

/// Compara con la huella guardada lo que sale del índice, sin el sello. Lo que solo cambia el sello
/// llega por la sincronización, que lo recalcula todo al acabar.
pub fn antes(db: &Connection) -> Result<Antes> {
    let (n, ultimo, proyectos) = huella_ligera(db)?;
    let guardada: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |f| f.get(0)).optional()?;
    let huella = guardada.filter(|g| {
        let p: Vec<&str> = g.split('|').collect();
        p.len() == 4 && p[0] == n && p[1] == ultimo && p[3] == proyectos
    });
    Ok(Antes { huella })
}

/// La huella tras escribir `ids` sobre la de `antes`, sin leer la tabla: el sello más alto es el de
/// antes o el de una de ellas (un cambio aquí siempre lleva un sello más nuevo). `None` si alguna ya
/// no está (olvidada o absorbida: quizá llevaba el sello más alto) y hay que leerla entera. Si saliera
/// distinta de la de verdad, la siguiente lectura lo recalcularía todo: más lento, pero bien.
fn huella_tras(db: &Connection, antes: &str, ids: &BTreeSet<String>) -> Result<Option<String>> {
    let mut sello = antes.split('|').nth(2).unwrap_or("").to_string();
    for id in ids {
        let Some(s) = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get::<_, String>(0)).optional()? else {
            return Ok(None);
        };
        sello = sello.max(s);
    }
    let (n, ultimo, proyectos) = huella_ligera(db)?;
    Ok(Some(format!("{n}|{ultimo}|{sello}|{proyectos}")))
}

/// Pone al día la revisión tras escribir `ids` (guardadas, cambiadas, olvidadas o recuperadas), sin
/// rehacerla entera: solo las parejas en las que entra una de ellas, contra sus candidatas del índice
/// de búsqueda, y los enlaces que pueden haber cambiado. Qué palabra es rara se toma del último
/// recálculo entero; se rehace entera si la revisión no estaba al día antes de escribir, si hay
/// pocas memorias, si su número ha cambiado más de un 5 % desde entonces o si el índice no sabe
/// buscar alguna palabra.
pub fn al_dia_de(db: &Connection, antes: &Antes, ids: &[String]) -> Result<()> {
    let Some(previa) = antes.huella.as_deref() else {
        return al_dia(db);
    };
    if ids.is_empty() || ids.len() > MAXIMO_POR_PARTES {
        return al_dia(db);
    }
    let ids: BTreeSet<String> = ids.iter().cloned().collect();
    let h = match huella_tras(db, previa, &ids)? {
        Some(h) => h,
        None => huella(db)?,
    };
    let n: usize = h.split('|').next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let Some(comunes) = comunes_vigentes(db, n)? else {
        return al_dia(db);
    };
    let tx = db.unchecked_transaction()?;
    let hecho = desfasadas_de(&tx, &ids, &comunes)? && crate::enlaces::recalcular_de(&tx, &ids)?;
    if !hecho {
        drop(tx);
        return al_dia(db);
    }
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('revision', ?1)", [&h])?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{CLAVE_PARCIALES}', COALESCE((SELECT v FROM meta WHERE k = '{CLAVE_PARCIALES}'), 0) + 1)"),
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// Las palabras comunes del último recálculo entero, si aún valen para `n` memorias.
fn comunes_vigentes(db: &Connection, n: usize) -> Result<Option<HashSet<String>>> {
    let Some(v) = db.query_row(&format!("SELECT v FROM meta WHERE k = '{CLAVE_COMUNES}'"), [], |f| f.get::<_, String>(0)).optional()? else {
        return Ok(None);
    };
    let mut lineas = v.split('\n');
    let Some(n_ref) = lineas.next().and_then(|x| x.parse::<usize>().ok()) else {
        return Ok(None);
    };
    if n < MINIMO_POR_PARTES || n_ref < MINIMO_POR_PARTES || (n as f64 - n_ref as f64).abs() > n_ref as f64 * DERIVA {
        return Ok(None);
    }
    Ok(Some(lineas.filter(|l| !l.is_empty()).map(str::to_string).collect()))
}

/// El recálculo entero: desfasadas y enlaces de toda la base.
fn rehacer(db: &Connection, h: &str) -> Result<()> {
    let (nuevas, n, comunes) = desfasadas(db)?;
    let tx = db.unchecked_transaction()?;
    // Lo que se dio por bueno está en `verdict` (y viaja con la sincronización); aquí solo lo detectado.
    tx.execute("DELETE FROM review WHERE kind IN (?1, ?2)", [DESFASADA, ENLACE_ROTO])?;
    let rotos = crate::enlaces::recalcular(&tx)?;
    apuntar_rotos(&tx, rotos)?;
    for (vieja, nueva, detalle) in nuevas {
        apuntar_desfasada(&tx, &vieja, &nueva, &detalle)?;
    }
    let mut guardadas = n.to_string();
    for w in comunes {
        guardadas.push('\n');
        guardadas.push_str(&w);
    }
    tx.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{CLAVE_COMUNES}', ?1)"), [&guardadas])?;
    tx.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{CLAVE_PARCIALES}', '0')"), [])?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('revision', ?1)", [h])?;
    tx.commit()?;
    Ok(())
}

/// Los enlaces rotos (memoria, nombre tal como está escrito), uno por memoria con todos sus nombres.
pub(crate) fn apuntar_rotos(db: &Connection, rotos: Vec<(String, String)>) -> Result<()> {
    let mut por_memoria: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (m, e) in rotos {
        por_memoria.entry(m).or_default().push(format!("[[{e}]]"));
    }
    for (m, es) in por_memoria {
        db.execute(
            &format!("INSERT OR IGNORE INTO review(memory_id, kind, other_id, detail, found_at) VALUES (?1, ?2, '', ?3, {AHORA})"),
            params![m, ENLACE_ROTO, format!("links to nothing: {}", es.join(", "))],
        )?;
    }
    Ok(())
}

fn apuntar_desfasada(db: &Connection, vieja: &str, nueva: &str, detalle: &str) -> Result<()> {
    db.execute(
        &format!("INSERT OR IGNORE INTO review(memory_id, kind, other_id, detail, found_at) VALUES (?1, ?2, ?3, ?4, {AHORA})"),
        params![vieja, DESFASADA, nueva, detalle],
    )?;
    Ok(())
}

// --- Desfasadas ------------------------------------------------------------------------------------

struct Ficha {
    id: String,
    project: String,
    updated_at: String,
    /// El sello del último cambio (`reloj`): desempata dos memorias del mismo milisegundo.
    hlc: String,
    /// La fecha más reciente que cuenta (o la del último cambio si no menciona ninguna).
    cuando: String,
    titulo: BTreeSet<String>,
    contenido: BTreeSet<String>,
    texto: String,
    versiones: BTreeMap<String, BTreeSet<Vec<u64>>>,
}

/// Lo que no distingue a una memoria de otra.
const VACIAS: &[&str] = &[
    "para", "pero", "como", "esta", "este", "esto", "estos", "estas", "desde", "hasta", "sobre", "entre", "cuando", "donde", "porque",
    "tiene", "tienen", "hace", "hacer", "solo", "todo", "toda", "todos", "todas", "cada", "otro", "otra", "otros", "otras", "mismo",
    "misma", "algo", "nada", "sino", "tras", "with", "that", "this", "from", "have", "when", "what", "into", "your", "they", "them",
    "then", "than", "also", "only", "more", "most", "just", "like", "does", "each", "been", "were", "will", "would", "should", "could",
    "there", "their", "about", "which", "other", "some", "such", "over", "under", "after", "before", "memoria", "memorias", "usuario",
];

fn normal(s: &str) -> String {
    crate::buscar::sin_tildes(&s.to_lowercase())
}

/// Palabras de 4 letras o más (con `.`, `_` y `-` dentro: `centrelaser.es`, `social-publish`).
fn palabras(s: &str) -> BTreeSet<String> {
    let mut r = BTreeSet::new();
    for p in normal(s).split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_' || c == '-')) {
        let p = p.trim_matches(|c: char| c == '.' || c == '-' || c == '_');
        if p.chars().count() >= 4 && !p.chars().all(|c| c.is_ascii_digit() || c == '.') && !VACIAS.contains(&p) {
            r.insert(p.to_string());
        }
    }
    r
}

const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Fechas `29-oct-2026`, `29 oct 2026` y `2026-10-29` del texto, como `AAAA-MM-DD`.
fn fechas(t: &str) -> Vec<String> {
    let mut r = Vec::new();
    let w: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    for i in 0..w.len() {
        // AAAA MM DD (las tres seguidas tras partir por los guiones).
        if i + 2 < w.len()
            && w[i].len() == 4
            && w[i + 1].len() == 2
            && w[i + 2].len() == 2
            && let (Ok(a), Ok(m), Ok(d)) = (w[i].parse::<u32>(), w[i + 1].parse::<u32>(), w[i + 2].parse::<u32>())
            && (1990..2200).contains(&a)
            && (1..=12).contains(&m)
            && (1..=31).contains(&d)
        {
            r.push(format!("{a:04}-{m:02}-{d:02}"));
        }
        // DD mes AAAA.
        if i + 2 < w.len() && w[i].len() <= 2 && w[i + 2].len() == 4 {
            let mes = w[i + 1].get(..3).unwrap_or("");
            let m = MESES.iter().position(|x| *x == mes).or_else(|| MONTHS.iter().position(|x| *x == mes));
            if let (Ok(d), Some(m), Ok(a)) = (w[i].parse::<u32>(), m, w[i + 2].parse::<u32>())
                && (1..=31).contains(&d)
                && (1990..2200).contains(&a)
            {
                r.push(format!("{a:04}-{:02}-{d:02}", m + 1));
            }
        }
    }
    r
}

/// Las palabras que no dicen de qué es una versión: «la 0.9.2», «release v0.10.0».
const GENERICAS: &[&str] = &["la", "el", "lo", "version", "versiones", "release", "tag", "a", "de", "en", "con", "hasta", "desde", "y", "o", "the", "to", "of", "v"];

/// `{cosa: {versiones}}`: las `X.Y.Z` del texto, por la palabra de delante (vacía si es genérica: la
/// del propio proyecto). Así «rusqlite 0.41.0» no se compara con «omoi 0.9.2».
fn versiones(t: &str) -> BTreeMap<String, BTreeSet<Vec<u64>>> {
    let mut r: BTreeMap<String, BTreeSet<Vec<u64>>> = BTreeMap::new();
    // La palabra de delante, dentro de la misma frase: un salto de línea o un final de frase la borran.
    let mut anterior = String::new();
    for p in t.split(|c: char| c == ' ' || c == '\t' || "()[]{},;*`\"'«»|".contains(c)).filter(|p| !p.is_empty()) {
        let p = match p.rsplit_once(['\n', '\r']) {
            Some((_, resto)) => {
                anterior.clear();
                resto
            }
            None => p,
        };
        if p.is_empty() {
            continue;
        }
        let limpio = p.trim_end_matches(['.', ')']).trim_start_matches('v');
        let partes: Vec<&str> = limpio.split('.').collect();
        if partes.len() == 3 && partes.iter().all(|x| !x.is_empty() && x.len() <= 4 && x.chars().all(|c| c.is_ascii_digit())) {
            let clave = if GENERICAS.contains(&anterior.as_str()) { String::new() } else { anterior.clone() };
            r.entry(clave).or_default().insert(partes.iter().map(|x| x.parse().unwrap_or(0)).collect());
        }
        anterior = if p.ends_with(['.', ':', '!', '?']) && !p.chars().any(|c| c.is_ascii_digit()) {
            String::new()
        } else {
            p.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').to_lowercase()
        };
    }
    r
}

fn ficha(m: &Memoria, hlc: String) -> Ficha {
    let contenido: String = m.content.chars().take(LARGO_PARA_RECALL).collect();
    let texto = normal(&contenido);
    let todo = format!("{} {texto}", normal(&m.title));
    Ficha {
        id: m.id.clone(),
        project: m.project.clone(),
        updated_at: m.updated_at.clone(),
        hlc,
        cuando: fechas(&todo).into_iter().max().unwrap_or_else(|| m.updated_at.get(..10).unwrap_or("").to_string()),
        titulo: palabras(&m.title),
        contenido: palabras(&contenido),
        versiones: versiones(&todo),
        texto,
    }
}

/// Frases (por puntuación de final o salto de línea).
fn frases(t: &str) -> Vec<&str> {
    let mut r = Vec::new();
    let mut ini = 0;
    let b = t.as_bytes();
    for (i, c) in t.char_indices() {
        let corta = c == '\n' || (".;:!?".contains(c) && b.get(i + 1).is_none_or(|x| x.is_ascii_whitespace()));
        if corta {
            if t[ini..i].trim().len() > 1 {
                r.push(&t[ini..i]);
            }
            ini = i + c.len_utf8();
        }
    }
    if t[ini..].trim().len() > 1 {
        r.push(&t[ini..]);
    }
    r
}

fn contiene_palabra(frase: &str, ws: &[&str]) -> bool {
    ws.iter().any(|w| {
        frase.match_indices(w).any(|(i, _)| {
            let antes = frase[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let despues = frase[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
            antes && despues
        })
    })
}

const PENDIENTE: &[&str] = &["pendiente", "falta", "faltan", "por hacer", "sin hacer", "sin publicar", "todavia no", "aun no", "siguiente", "not yet", "pending"];
const HECHO: &[&str] = &["hecho", "hecha", "publicado", "publicada", "publicados", "publicadas", "ya esta", "resuelto", "terminado", "terminada", "instalado", "instalada", "done", "published", "shipped"];

fn texto_version(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// Por qué la vieja parece superada por la nueva, o nada. `mismo_tema`: comparten palabras; sin eso,
/// una versión a secas no dice nada (en un proyecto casi todo nombra versiones suyas: el eslogan
/// «desde la 0.14.1» no contradice a la firma «desde la 0.14.0»). Una versión con nombre
/// («rusqlite 0.41.0») sí, porque el nombre ya las ata.
fn motivos(vieja: &Ficha, nueva: &Ficha, mismo_tema: bool, raras: &dyn Fn(&BTreeSet<String>) -> BTreeSet<String>) -> Vec<String> {
    let mut r = Vec::new();
    for (cosa, va) in &vieja.versiones {
        if let Some(vb) = nueva.versiones.get(cosa)
            && let (Some(a), Some(b)) = (va.last(), vb.last())
            && b > a
            && (mismo_tema || !cosa.is_empty())
        {
            let nombre = if cosa.is_empty() { "version".to_string() } else { cosa.clone() };
            r.push(format!("{nombre} {} → {}", texto_version(a), texto_version(b)));
        }
    }
    // Una frase de la vieja dice que algo está pendiente y otra de la nueva que está hecho, y hablan
    // de lo mismo (al menos dos palabras raras en común).
    let pendientes: Vec<BTreeSet<String>> = frases(&vieja.texto).into_iter().filter(|f| contiene_palabra(f, PENDIENTE)).map(|f| raras(&palabras(f))).collect();
    if !pendientes.is_empty() {
        let hechas: Vec<BTreeSet<String>> = frases(&nueva.texto).into_iter().filter(|f| contiene_palabra(f, HECHO)).map(|f| raras(&palabras(f))).collect();
        let mejor = pendientes.iter().flat_map(|p| hechas.iter().map(move |h| p.intersection(h).count())).max().unwrap_or(0);
        if mejor >= 2 {
            r.push("pending → done".to_string());
        }
    }
    r
}

/// Las palabras raras de una memoria, del título y del contenido.
struct Raras {
    titulo: BTreeSet<String>,
    contenido: BTreeSet<String>,
}

fn raras_de(f: &Ficha, es_rara: &dyn Fn(&str) -> bool) -> Raras {
    let filtra = |ws: &BTreeSet<String>| -> BTreeSet<String> { ws.iter().filter(|w| es_rara(w)).cloned().collect() };
    Raras { titulo: filtra(&f.titulo), contenido: filtra(&f.contenido) }
}

/// Rara: en el 5 % de las memorias como mucho; a partir de 2.000 memorias, en 100. Una palabra que
/// sale en cientos de memorias no dice que dos hablen de lo mismo.
fn tope_raras(n: usize) -> usize {
    2usize.max(n / 20).min(100)
}

/// Lo que comparten dos memorias para ser candidatas: una palabra rara o la versión de la misma cosa.
fn comparten(a: &Ficha, ra: &Raras, b: &Ficha, rb: &Raras) -> bool {
    let de_a: BTreeSet<&String> = ra.titulo.union(&ra.contenido).collect();
    rb.titulo.iter().chain(&rb.contenido).any(|w| de_a.contains(w)) || a.versiones.keys().any(|c| !c.is_empty() && b.versiones.contains_key(c))
}

/// (vieja, nueva, detalle).
type Desfasada = (String, String, String);

/// Si una de las dos deja anticuada a la otra.
fn juzgar(a: &Ficha, ra: &Raras, b: &Ficha, rb: &Raras, es_rara: &dyn Fn(&str) -> bool) -> Option<Desfasada> {
    // La más nueva: por la fecha que menciona, luego por su último cambio y, en el mismo
    // milisegundo (varias guardadas seguidas, una importación), por el sello del reloj.
    fn orden(f: &Ficha) -> (&str, &str, &str, &str) {
        (&f.cuando, &f.updated_at, &f.hlc, &f.id)
    }
    let ((vieja, rv), (nueva, rn)) = if orden(a) < orden(b) { ((a, ra), (b, rb)) } else { ((b, rb), (a, ra)) };
    let comunes_t = rv.titulo.intersection(&rn.titulo).count();
    let union = rv.contenido.union(&rn.contenido).count().max(1);
    let jaccard = rv.contenido.intersection(&rn.contenido).count() as f64 / union as f64;
    let mismo_tema = comunes_t >= 2 || (comunes_t >= 1 && jaccard >= 0.12) || jaccard >= 0.22;
    // Lo que no comparte palabras solo puede contradecirse por una versión con nombre.
    if !mismo_tema && vieja.versiones.keys().all(|c| c.is_empty()) {
        return None;
    }
    let raras = |ws: &BTreeSet<String>| -> BTreeSet<String> { ws.iter().filter(|w| es_rara(w)).cloned().collect() };
    let m = motivos(vieja, nueva, mismo_tema, &raras);
    (!m.is_empty()).then(|| (vieja.id.clone(), nueva.id.clone(), m.join("; ")))
}

/// Parejas (vieja, nueva, detalle) del mismo proyecto que hablan de lo mismo y se contradicen, de toda
/// la base. Devuelve también cuántas memorias había y las palabras que no eran raras, que es lo que
/// necesita la revisión por partes (`al_dia_de`) hasta el siguiente recálculo entero.
fn desfasadas(db: &Connection) -> Result<(Vec<Desfasada>, usize, Vec<String>)> {
    let todas = crate::memoria::recientes(db, &Ambito::Todos, None, usize::MAX >> 1)?;
    let sellos: HashMap<String, String> = {
        let mut q = db.prepare("SELECT id, hlc FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let fichas: Vec<Ficha> = todas.iter().map(|m| ficha(m, sellos.get(&m.id).cloned().unwrap_or_default())).collect();
    let n = fichas.len().max(1);
    let mut df: HashMap<&str, usize> = HashMap::new();
    for f in &fichas {
        for w in f.titulo.union(&f.contenido) {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let tope = tope_raras(n);
    let es_rara = |w: &str| df.get(w).is_some_and(|d| *d <= tope);
    let raras: Vec<Raras> = fichas.iter().map(|f| raras_de(f, &es_rara)).collect();

    // Candidatas: parejas del mismo proyecto (las globales cuentan como uno) que comparten una palabra
    // rara o nombran la versión de la misma cosa («rusqlite 0.41.0»). Es exacto: cualquier motivo de
    // `motivos` exige una de las dos cosas. Comparar todas las parejas de un proyecto era cuadrático:
    // con 50.000 memorias, 31 millones de parejas, 108 s y 1,2 GB en cada escritura.
    let mut parejas: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut indice: HashMap<(&str, String), Vec<usize>> = HashMap::new();
    for (i, f) in fichas.iter().enumerate() {
        for w in raras[i].titulo.union(&raras[i].contenido) {
            indice.entry((f.project.as_str(), w.clone())).or_default().push(i);
        }
        for cosa in f.versiones.keys().filter(|c| !c.is_empty()) {
            indice.entry((f.project.as_str(), format!("ver:{cosa}"))).or_default().push(i);
        }
    }
    for l in indice.values() {
        for (a, &i) in l.iter().enumerate() {
            for &j in &l[a + 1..] {
                parejas.insert((i.min(j), i.max(j)));
            }
        }
    }

    let r = parejas.into_iter().filter_map(|(i, j)| juzgar(&fichas[i], &raras[i], &fichas[j], &raras[j], &es_rara)).collect();
    let mut comunes: Vec<String> = df.iter().filter(|(_, d)| **d > tope).map(|(w, _)| w.to_string()).collect();
    comunes.sort();
    Ok((r, fichas.len(), comunes))
}

/// La frase con la que el índice de texto (`memory_fts`, unicode61 sin tildes) encuentra una palabra
/// de las fichas: sus trozos alfanuméricos seguidos («centrelaser.es» → `"centrelaser es"`). Con
/// `prefijo`, el último trozo vale como principio de palabra: la ficha corta el contenido en
/// `LARGO_PARA_RECALL` y puede quedarse con media palabra. `None` si no tiene nada que buscar.
pub(crate) fn frase_fts(palabra: &str, prefijo: bool) -> Option<String> {
    let trozos: Vec<&str> = palabra.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let ultimo = trozos.last()?;
    let estrella = if prefijo && ultimo.chars().count() >= 3 { " *" } else { "" };
    Some(format!("\"{}\"{estrella}", trozos.join(" ")))
}

/// Las memorias (con su sello) que da una consulta FTS en un proyecto.
fn fichas_por_fts(db: &Connection, consulta: &str, project: &str) -> Result<Vec<Ficha>> {
    let mut q = db.prepare_cached(&format!(
        "SELECT {}, hlc FROM memory WHERE project = ?2 AND n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)",
        crate::memoria::COLUMNAS
    ))?;
    let v = q
        .query_map(params![consulta, project], |f| Ok((crate::memoria::de_fila(f)?, f.get::<_, String>(14)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v.into_iter().map(|(m, hlc)| ficha(&m, hlc)).collect())
}

/// Las desfasadas en las que entra alguna de `ids`, con la idea de rara del último recálculo entero:
/// borra las que había y apunta las de ahora. Sus candidatas salen del índice de texto (cualquier
/// memoria que comparta una palabra rara o una cosa con versión la nombra) y se comprueban igual que
/// en el recálculo entero. `false` si una palabra no tiene nada que buscar en el índice.
fn desfasadas_de(db: &Connection, ids: &BTreeSet<String>, comunes: &HashSet<String>) -> Result<bool> {
    let es_rara = |w: &str| !comunes.contains(w);
    let mut cambiadas = Vec::new();
    for id in ids {
        db.execute("DELETE FROM review WHERE kind = ?1 AND (memory_id = ?2 OR other_id = ?2)", params![DESFASADA, id])?;
        let fila = db
            .query_row(&format!("SELECT {}, hlc FROM memory WHERE id = ?1", crate::memoria::COLUMNAS), [id], |f| {
                Ok((crate::memoria::de_fila(f)?, f.get::<_, String>(14)?))
            })
            .optional()?;
        if let Some((m, hlc)) = fila {
            cambiadas.push(ficha(&m, hlc));
        }
    }
    let mut nuevas: BTreeMap<(String, String), String> = BTreeMap::new();
    for c in &cambiadas {
        let rc = raras_de(c, &es_rara);
        let mut frases = Vec::new();
        for w in rc.titulo.union(&rc.contenido) {
            let Some(f) = frase_fts(w, true) else { return Ok(false) };
            frases.push(f);
        }
        for cosa in c.versiones.keys().filter(|c| !c.is_empty()) {
            let Some(f) = frase_fts(cosa, false) else { return Ok(false) };
            frases.push(f);
        }
        if frases.is_empty() {
            continue;
        }
        let consulta = format!("{{title content}} : ({})", frases.join(" OR "));
        for b in fichas_por_fts(db, &consulta, &c.project)? {
            if b.id == c.id {
                continue;
            }
            let rb = raras_de(&b, &es_rara);
            if !comparten(c, &rc, &b, &rb) {
                continue;
            }
            if let Some((vieja, nueva, detalle)) = juzgar(c, &rc, &b, &rb, &es_rara) {
                nuevas.insert((vieja, nueva), detalle);
            }
        }
    }
    for ((vieja, nueva), detalle) in nuevas {
        apuntar_desfasada(db, &vieja, &nueva, &detalle)?;
    }
    Ok(true)
}

// --- Lectura ---------------------------------------------------------------------------------------

/// Si se dijo «está bien así» y la memoria no ha cambiado desde entonces.
/// Los avisos que un veredicto vigente quita: (memoria, aviso, otra). Una pareja con veredicto ya no
/// sale como desfasada: o vale (`compatible`) o tiene su propio aviso (`superseded`, `contradicts`).
fn descartados(db: &Connection) -> Result<BTreeSet<(String, String, String)>> {
    let mut r = BTreeSet::new();
    for ((id, aviso), _) in crate::veredictos::oks(db, true)? {
        r.insert((id, aviso, String::new()));
    }
    for (a, b) in crate::veredictos::parejas(db)?.into_keys() {
        r.insert((a.clone(), DESFASADA.to_string(), b.clone()));
        r.insert((b, DESFASADA.to_string(), a));
    }
    Ok(r)
}

/// Dónde está una ruta de una memoria en este equipo, si se puede saber (relativa a su proyecto).
fn ruta_en_disco(m: &Memoria, ruta: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(ruta.trim_start_matches("~/"));
    if ruta.starts_with("~/") {
        return dirs_casa().map(|c| c.join(p));
    }
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    let raiz = std::path::Path::new(&m.project);
    (!m.project.is_empty() && raiz.is_dir()).then(|| raiz.join(p))
}

fn dirs_casa() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from)
}

/// Todo lo que conviene revisar en el ámbito, de lo más a lo menos importante.
pub fn avisos(db: &Connection, ambito: &Ambito) -> Result<Vec<Aviso>> {
    al_dia(db)?;
    let fuera = descartados(db)?;
    let todas = crate::memoria::recientes(db, ambito, None, usize::MAX >> 1)?;
    let por_id: HashMap<&str, &Memoria> = todas.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut r = Vec::new();
    let aviso = |m: &Memoria, kind: &str, otra: Option<&Memoria>, detail: String| Aviso {
        kind: kind.into(),
        memory_id: m.id.clone(),
        title: m.title.clone(),
        project: m.project.clone(),
        other_id: otra.map(|o| o.id.clone()).unwrap_or_default(),
        other_title: otra.map(|o| o.title.clone()),
        detail,
        reason: String::new(),
        author: String::new(),
        via: 0,
    };
    {
        let mut q = db.prepare("SELECT memory_id, other_id, detail FROM review WHERE kind = ?1 AND dismissed_at IS NULL ORDER BY found_at, memory_id")?;
        let filas: Vec<(String, String, String)> = q.query_map([DESFASADA], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (vieja, nueva, detalle) in filas {
            if let (Some(v), Some(n)) = (por_id.get(vieja.as_str()), por_id.get(nueva.as_str()))
                && !fuera.contains(&(vieja.clone(), DESFASADA.to_string(), nueva.clone()))
            {
                r.push(aviso(v, DESFASADA, Some(n), detalle));
            }
        }
    }
    for mut v in crate::veredictos::parejas(db)?.into_values() {
        if v.relation == crate::veredictos::COMPATIBLE {
            continue;
        }
        // Una sustituida apunta al final de su cadena: si B sustituye a A y C a B, A lleva a C, con el
        // motivo del último eslabón.
        let mut saltos = 0;
        if v.relation == SUSTITUIDA
            && let Some(fin) = crate::veredictos::cadena(db, &v.memory_id)?
            && fin.saltos > 1
        {
            saltos = fin.saltos;
            v.other_id = fin.id;
            v.reason = fin.reason;
            v.author = fin.author;
        }
        if let (Some(m), Some(o)) = (por_id.get(v.memory_id.as_str()), por_id.get(v.other_id.as_str())) {
            let base = if v.relation == SUSTITUIDA { "replaced by the other one" } else { "they disagree and neither has won yet" };
            let mut a = aviso(m, &v.relation, Some(o), detalle_veredicto(base, &v, saltos));
            a.reason = v.reason;
            a.author = v.author;
            a.via = saltos;
            r.push(a);
        }
    }
    {
        let mut q = db.prepare("SELECT memory_id, detail FROM review WHERE kind = ?1 AND dismissed_at IS NULL ORDER BY memory_id")?;
        let filas: Vec<(String, String)> = q.query_map([ENLACE_ROTO], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, detalle) in filas {
            if let Some(m) = por_id.get(id.as_str())
                && !fuera.contains(&(id.clone(), ENLACE_ROTO.to_string(), String::new()))
            {
                r.push(aviso(m, ENLACE_ROTO, None, detalle));
            }
        }
    }
    let ahora = chrono_ahora(db)?;
    let dias = |desde: &str| -> f64 { dias_entre(desde, &ahora) };
    let revisadas = crate::veredictos::oks(db, false)?;
    for m in &todas {
        if let Some(n) = sin_revisar(m, &revisadas, &ahora) {
            r.push(aviso(m, POR_REVISAR, None, format!("untouched for {n} days: is it still true?")));
        }
        let descartado = |k: &str| fuera.contains(&(m.id.clone(), k.to_string(), String::new()));
        if m.tags.iter().any(|t| t == crate::memoria::ESTADO) && dias(&m.updated_at) > DIAS_ESTADO && !descartado(ESTADO_VIEJO) {
            let despues = todas.iter().filter(|x| x.project == m.project && x.id != m.id && x.updated_at > m.updated_at).count();
            if despues > 0 {
                r.push(aviso(m, ESTADO_VIEJO, None, format!("{despues} memories of this project changed after it; untouched for {} days", dias(&m.updated_at) as i64)));
            }
        }
        if !descartado(RUTA_ROTA) {
            let rotas: Vec<&str> = m.paths.iter().filter(|p| ruta_en_disco(m, p).is_some_and(|d| !d.exists())).map(String::as_str).collect();
            if !rotas.is_empty() {
                r.push(aviso(m, RUTA_ROTA, None, format!("no longer exists: {}", rotas.join(", "))));
            }
        }
        if m.recall_count == 0 && m.recalled_at.is_none() && dias(&m.created_at) > DIAS_SIN_USO && !descartado(SIN_USO) {
            r.push(aviso(m, SIN_USO, None, format!("never recalled in {} days", dias(&m.created_at) as i64)));
        }
        // Se mira al leer, como `unused`: unos µs por memoria, y así vale también para lo que llega por la
        // sincronización o una copia. Nunca enseña el secreto, solo qué parece.
        if !descartado(SECRETO) {
            let hs = secretos_de(m);
            if !hs.is_empty() {
                r.push(aviso(m, SECRETO, None, format!("looks like a secret: {}; redact it", crate::secretos::resumen(&hs))));
            }
        }
        let largo = m.content.chars().count();
        if largo > LARGO_PARA_RECALL && !descartado(LARGA) {
            r.push(aviso(m, LARGA, None, format!("{largo} characters; recall only reads the first {LARGO_PARA_RECALL}: consider splitting it")));
        }
    }
    let orden = |k: &str| TIPOS.iter().position(|x| *x == k).unwrap_or(TIPOS.len());
    r.sort_by_key(|a| orden(&a.kind));
    Ok(r)
}

/// Los tipos de lo que parece un secreto en el título y el contenido de una memoria, uno por hallazgo.
pub fn secretos_de(m: &Memoria) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = crate::secretos::hallazgos(&m.title).iter().map(|h| h.tipo).collect();
    v.extend(crate::secretos::hallazgos(&m.content).iter().map(|h| h.tipo));
    v
}

fn chrono_ahora(db: &Connection) -> Result<String> {
    Ok(db.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?)
}

/// Días entre dos fechas ISO (las que guarda la base). Sin dependencias: días civiles a mano.
fn dias_entre(desde: &str, hasta: &str) -> f64 {
    fn dia(s: &str) -> Option<f64> {
        let (a, m, d) = (s.get(0..4)?.parse::<i64>().ok()?, s.get(5..7)?.parse::<i64>().ok()?, s.get(8..10)?.parse::<i64>().ok()?);
        let (h, mi) = (s.get(11..13).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0), s.get(14..16).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0));
        // Días desde 1970-01-01 (algoritmo de Howard Hinnant).
        let y = if m <= 2 { a - 1 } else { a };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some((era * 146097 + doe - 719468) as f64 + (h * 60 + mi) as f64 / 1440.0)
    }
    match (dia(desde), dia(hasta)) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    }
}

/// Días sin revisar, si ya le toca: desde su último cambio, el último «está bien así» o la última vez
/// que el agente la recibió, lo más nuevo. Una memoria que se recuerda cada semana y nadie corrige
/// está repasada; la que lleva 120 días sin que nadie la vea ni la toque, no (repetición espaciada).
fn sin_revisar(m: &Memoria, revisadas: &HashMap<(String, String), String>, ahora: &str) -> Option<i64> {
    let plazo = dias_para_revisar(m)?;
    let mut desde = m.updated_at.as_str();
    if let Some(at) = revisadas.get(&(m.id.clone(), POR_REVISAR.to_string()))
        && at.as_str() > desde
    {
        desde = at;
    }
    if let Some(r) = m.recalled_at.as_deref()
        && r > desde
    {
        desde = r;
    }
    let n = dias_entre(desde, ahora);
    (n > plazo).then_some(n as i64)
}

/// «Está bien así»: no vuelve a avisar de esto hasta que la memoria cambie. En una pareja, las dos
/// valen (`compatible`); en una memoria sola, `ok:<aviso>`. Es un veredicto, así que viaja, firmado
/// por `user` (viene de la vista web o de la CLI, no del agente).
pub fn descartar(db: &Connection, memory_id: &str, kind: &str, other_id: &str) -> Result<()> {
    anyhow::ensure!(TIPOS.contains(&kind), "unknown review kind «{kind}»");
    if other_id.is_empty() {
        crate::veredictos::poner(db, memory_id, "", &format!("ok:{kind}"), "", crate::veredictos::USUARIO)
    } else {
        crate::veredictos::poner(db, memory_id, other_id, crate::veredictos::COMPATIBLE, "", crate::veredictos::USUARIO)
    }
}

/// Las memorias que la recién guardada deja anticuadas: (id, título, por qué).
pub fn superadas_por(db: &Connection, id: &str) -> Result<Vec<(String, String, String)>> {
    let mut q = db.prepare(
        "SELECT r.memory_id, m.title, r.detail FROM review r JOIN memory m ON m.id = r.memory_id
         WHERE r.other_id = ?1 AND r.kind = ?2 AND r.dismissed_at IS NULL",
    )?;
    let v = q.query_map(params![id, DESFASADA], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Para el recall: una nota corta por memoria que conviene tomar con cuidado (sustituida,
/// contradicha, desfasada o estado viejo).
pub fn notas(db: &Connection, memorias: &[&Memoria]) -> Result<HashMap<String, String>> {
    notas_con(db, memorias, false)
}

/// Para `search`: lo mismo y, además, las que ya toca revisar. En el recall no, para no meter ruido:
/// ahí solo va lo que dice que la memoria puede estar mal.
pub fn notas_busqueda(db: &Connection, memorias: &[&Memoria]) -> Result<HashMap<String, String>> {
    notas_con(db, memorias, true)
}

fn notas_con(db: &Connection, memorias: &[&Memoria], con_revision: bool) -> Result<HashMap<String, String>> {
    let mut r = HashMap::new();
    if memorias.is_empty() {
        return Ok(r);
    }
    let corto = |id: &str| -> Result<String> {
        let t: String = db.query_row("SELECT title FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        Ok(format!("{} «{}»", id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>()))
    };
    // El motivo del veredicto, o lo que hay que hacer si no lo trae.
    let motivo = |reason: &str, si_no: &str| -> String { if reason.trim().is_empty() { si_no.into() } else { crate::veredictos::motivo_corto(reason) } };
    for v in crate::veredictos::parejas(db)?.into_values() {
        for (esta, otra) in [(&v.memory_id, &v.other_id), (&v.other_id, &v.memory_id)] {
            if r.contains_key(esta.as_str()) || !memorias.iter().any(|m| &m.id == esta) {
                continue;
            }
            let nota = match v.relation.as_str() {
                SUSTITUIDA if esta == &v.memory_id => {
                    // Si la sustituta está a su vez sustituida, la nota lleva al final de la cadena.
                    let (ultima, saltos, reason) = match crate::veredictos::cadena(db, esta)? {
                        Some(fin) if fin.saltos > 1 => (fin.id, fin.saltos, fin.reason),
                        _ => (otra.clone(), 1, v.reason.clone()),
                    };
                    let via = if saltos > 1 { format!(" (via {saltos})") } else { String::new() };
                    format!("superseded by {}{via}: {}", corto(&ultima)?, motivo(&reason, "use that one"))
                }
                CONTRADICE => format!("contradicts {}: {}", corto(otra)?, motivo(&v.reason, "check which one holds, asking the user if unsure")),
                _ => continue,
            };
            r.insert(esta.clone(), nota);
        }
    }
    // Sin `al_dia`: recalcular cuesta unos 50 ms y el recall tiene que quedarse por debajo de 30.
    // Las escrituras (MCP, CLI, sync, la vista) ya lo dejan al día.
    let fuera = descartados(db)?;
    let mut q = db.prepare(
        "SELECT r.other_id, r.detail, m.title FROM review r JOIN memory m ON m.id = r.other_id
         WHERE r.memory_id = ?1 AND r.kind = ?2 AND r.dismissed_at IS NULL LIMIT 1",
    )?;
    let ahora = chrono_ahora(db)?;
    for m in memorias {
        if r.contains_key(&m.id) {
            continue;
        }
        if let Some((otra, detalle, titulo)) =
            q.query_row(params![m.id, DESFASADA], |f| Ok((f.get::<_, String>(0)?, f.get::<_, String>(1)?, f.get::<_, String>(2)?))).optional()?
            && !fuera.contains(&(m.id.clone(), DESFASADA.to_string(), otra.clone()))
        {
            let corto: String = titulo.chars().take(80).collect();
            r.insert(m.id.clone(), format!("may be outdated: newer {} «{corto}» ({detalle})", otra.chars().take(12).collect::<String>()));
        } else if m.tags.iter().any(|t| t == crate::memoria::ESTADO) && dias_entre(&m.updated_at, &ahora) > DIAS_ESTADO {
            let despues: i64 = db.query_row(
                "SELECT count(*) FROM memory WHERE project = ?1 AND id != ?2 AND updated_at > ?3",
                params![m.project, m.id, m.updated_at],
                |f| f.get(0),
            )?;
            if despues > 0 && !fuera.contains(&(m.id.clone(), ESTADO_VIEJO.to_string(), String::new())) {
                r.insert(m.id.clone(), format!("may be stale: {despues} memories of this project changed after it"));
            }
        }
    }
    if con_revision {
        let revisadas = crate::veredictos::oks(db, false)?;
        for m in memorias {
            if !r.contains_key(&m.id)
                && let Some(n) = sin_revisar(m, &revisadas, &ahora)
            {
                r.insert(m.id.clone(), format!("not reviewed in {n} days: check it still holds"));
            }
        }
    }
    Ok(r)

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fechas_y_versiones() {
        assert_eq!(fechas("hecho el 29-oct-2026 y el 2026-11-02"), vec!["2026-10-29", "2026-11-02"]);
        assert_eq!(fechas("3 de mayo"), Vec::<String>::new());
        let v = versiones("la 0.9.2 usa rusqlite 0.41.0 y la v0.10.0");
        assert_eq!(v[""].iter().map(|x| texto_version(x)).collect::<Vec<_>>(), vec!["0.9.2", "0.10.0"]);
        assert_eq!(v["rusqlite"].len(), 1);
        // La palabra de otra línea o de otra frase no cuenta.
        let v = versiones("otras máquinas\n**v0.2.0 publicada. La v0.3.0");
        assert_eq!(v[""].iter().map(|x| texto_version(x)).collect::<Vec<_>>(), vec!["0.2.0", "0.3.0"]);
    }

    #[test]
    fn dias() {
        assert_eq!(dias_entre("2026-10-29T00:00:00Z", "2026-11-05T00:00:00Z"), 7.0);
        assert_eq!(dias_entre("2024-02-28", "2024-03-01"), 2.0);
    }

    #[test]
    fn frases_partidas() {
        assert_eq!(frases("uno. dos: tres\ncuatro v0.9.2 cinco"), vec!["uno", " dos", " tres", "cuatro v0.9.2 cinco"]);
    }
}
