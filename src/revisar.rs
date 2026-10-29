//! Lo que conviene revisar en la memoria (Fase 7.1): memorias que otra más nueva contradice, estados
//! que se han quedado atrás, rutas que ya no existen, memorias que nunca se usan y notas demasiado
//! largas para el recall. Sin modelo: palabras, fechas y versiones.
//!
//! Las desfasadas se calculan por parejas y se guardan en `review`; se recalculan solas cuando la
//! base cambia (por una huella de `memory`), entre por donde entre el cambio (CLI, MCP, sync,
//! importar). Lo demás depende de la hora o del disco y se mira al leer. «Está bien así» se apunta en
//! la misma tabla y vale hasta que la memoria vuelva a cambiar.
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
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const DESFASADA: &str = "outdated";
pub const ESTADO_VIEJO: &str = "stale_status";
pub const RUTA_ROTA: &str = "broken_path";
pub const SIN_USO: &str = "unused";
pub const LARGA: &str = "too_long";

/// Días sin tocar a partir de los que una memoria `status` con cambios posteriores se avisa.
pub const DIAS_ESTADO: f64 = 7.0;
/// Días desde que se creó sin recordarse nunca.
pub const DIAS_SIN_USO: f64 = 30.0;

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
    /// Qué se ha visto, en inglés y corto (lo leen el agente y la vista).
    pub detail: String,
}

/// Cambia con cualquier escritura: memorias nuevas, cambiadas, movidas u olvidadas.
fn huella(db: &Connection) -> Result<String> {
    Ok(db.query_row(
        "SELECT count(*) || '|' || COALESCE(max(updated_at), '') || '|' || COALESCE(max(hlc), '') || '|' || COALESCE(sum(length(project)), 0) FROM memory",
        [],
        |f| f.get(0),
    )?)
}

/// Recalcula las desfasadas si la base cambió desde la última vez.
pub fn al_dia(db: &Connection) -> Result<()> {
    let h = huella(db)?;
    let antes: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |f| f.get(0)).optional()?;
    if antes.as_deref() == Some(h.as_str()) {
        return Ok(());
    }
    let nuevas = desfasadas(db)?;
    let tx = db.unchecked_transaction()?;
    // Lo descartado se queda: si el aviso vuelve a salir, sigue descartado mientras nada cambie.
    tx.execute("DELETE FROM review WHERE kind = ?1 AND dismissed_at IS NULL", [DESFASADA])?;
    // Pero solo mientras ninguna de las dos cambie: si una cambia, el aviso puede volver.
    tx.execute(
        "DELETE FROM review WHERE kind = ?1 AND dismissed_at IS NOT NULL AND (
           EXISTS (SELECT 1 FROM memory m WHERE m.id = review.memory_id AND m.updated_at > review.dismissed_at)
           OR EXISTS (SELECT 1 FROM memory m WHERE m.id = review.other_id AND m.updated_at > review.dismissed_at))",
        [DESFASADA],
    )?;
    for (vieja, nueva, detalle) in nuevas {
        tx.execute(
            &format!("INSERT OR IGNORE INTO review(memory_id, kind, other_id, detail, found_at) VALUES (?1, ?2, ?3, ?4, {AHORA})"),
            params![vieja, DESFASADA, nueva, detalle],
        )?;
    }
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('revision', ?1)", [&h])?;
    tx.commit()?;
    Ok(())
}

// --- Desfasadas ------------------------------------------------------------------------------------

struct Ficha {
    id: String,
    project: String,
    updated_at: String,
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

fn ficha(m: &Memoria) -> Ficha {
    let contenido: String = m.content.chars().take(LARGO_PARA_RECALL).collect();
    let texto = normal(&contenido);
    let todo = format!("{} {texto}", normal(&m.title));
    Ficha {
        id: m.id.clone(),
        project: m.project.clone(),
        updated_at: m.updated_at.clone(),
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

/// Por qué la vieja parece superada por la nueva, o nada.
fn motivos(vieja: &Ficha, nueva: &Ficha, raras: &dyn Fn(&BTreeSet<String>) -> BTreeSet<String>) -> Vec<String> {
    let mut r = Vec::new();
    for (cosa, va) in &vieja.versiones {
        if let Some(vb) = nueva.versiones.get(cosa)
            && let (Some(a), Some(b)) = (va.last(), vb.last())
            && b > a
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

/// Parejas (vieja, nueva, detalle) del mismo proyecto que hablan de lo mismo y se contradicen.
fn desfasadas(db: &Connection) -> Result<Vec<(String, String, String)>> {
    let todas = crate::memoria::recientes(db, &Ambito::Todos, None, usize::MAX >> 1)?;
    let fichas: Vec<Ficha> = todas.iter().map(ficha).collect();
    let n = fichas.len().max(1);
    let mut df: HashMap<&str, usize> = HashMap::new();
    for f in &fichas {
        for w in f.titulo.union(&f.contenido) {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let tope = 2usize.max(n / 20);
    let es_rara = |w: &str| df.get(w).is_some_and(|d| *d <= tope);
    let raras = |ws: &BTreeSet<String>| -> BTreeSet<String> { ws.iter().filter(|w| es_rara(w)).cloned().collect() };
    let titulos: Vec<BTreeSet<String>> = fichas.iter().map(|f| raras(&f.titulo)).collect();
    let contenidos: Vec<BTreeSet<String>> = fichas.iter().map(|f| raras(&f.contenido)).collect();

    // Candidatas: dentro de un proyecto, todas las parejas (son pocas y casi todo trata de él); entre
    // las globales, las que comparten alguna palabra rara (índice invertido, para no comparar todas).
    let mut parejas: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut por_proyecto: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in fichas.iter().enumerate() {
        por_proyecto.entry(f.project.as_str()).or_default().push(i);
    }
    for (p, v) in &por_proyecto {
        if p.is_empty() {
            let mut indice: HashMap<&str, Vec<usize>> = HashMap::new();
            for &i in v {
                for w in titulos[i].union(&contenidos[i]) {
                    indice.entry(w.as_str()).or_default().push(i);
                }
            }
            for l in indice.values() {
                for (a, &i) in l.iter().enumerate() {
                    for &j in &l[a + 1..] {
                        parejas.insert((i.min(j), i.max(j)));
                    }
                }
            }
        } else {
            for (a, &i) in v.iter().enumerate() {
                for &j in &v[a + 1..] {
                    parejas.insert((i.min(j), i.max(j)));
                }
            }
        }
    }

    let mut r = Vec::new();
    for (i, j) in parejas {
        let (a, b) = (&fichas[i], &fichas[j]);
        let (mut ca, mut cb) = (a.cuando.as_str(), b.cuando.as_str());
        if ca == cb {
            (ca, cb) = (a.updated_at.as_str(), b.updated_at.as_str());
        }
        if ca == cb {
            continue;
        }
        let ((vieja, iv), (nueva, inu)) = if ca < cb { ((a, i), (b, j)) } else { ((b, j), (a, i)) };
        let comunes_t = titulos[iv].intersection(&titulos[inu]).count();
        let union = contenidos[iv].union(&contenidos[inu]).count().max(1);
        let jaccard = contenidos[iv].intersection(&contenidos[inu]).count() as f64 / union as f64;
        let relacionadas = !vieja.project.is_empty() || comunes_t >= 2 || (comunes_t >= 1 && jaccard >= 0.12) || jaccard >= 0.22;
        if !relacionadas {
            continue;
        }
        let m = motivos(vieja, nueva, &raras);
        if !m.is_empty() {
            r.push((vieja.id.clone(), nueva.id.clone(), m.join("; ")));
        }
    }
    Ok(r)
}

// --- Lectura ---------------------------------------------------------------------------------------

/// Si se dijo «está bien así» y la memoria no ha cambiado desde entonces.
fn descartados(db: &Connection) -> Result<BTreeSet<(String, String, String)>> {
    let mut q = db.prepare(
        "SELECT r.memory_id, r.kind, r.other_id FROM review r JOIN memory m ON m.id = r.memory_id
         WHERE r.dismissed_at IS NOT NULL AND m.updated_at <= r.dismissed_at",
    )?;
    let v = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
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
    let ahora = chrono_ahora(db)?;
    let dias = |desde: &str| -> f64 { dias_entre(desde, &ahora) };
    for m in &todas {
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
        let largo = m.content.chars().count();
        if largo > LARGO_PARA_RECALL && !descartado(LARGA) {
            r.push(aviso(m, LARGA, None, format!("{largo} characters; recall only reads the first {LARGO_PARA_RECALL}: consider splitting it")));
        }
    }
    let orden = |k: &str| [DESFASADA, ESTADO_VIEJO, RUTA_ROTA, SIN_USO, LARGA].iter().position(|x| *x == k).unwrap_or(9);
    r.sort_by_key(|a| orden(&a.kind));
    Ok(r)
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

/// «Está bien así»: no vuelve a avisar de esto hasta que la memoria cambie.
pub fn descartar(db: &Connection, memory_id: &str, kind: &str, other_id: &str) -> Result<()> {
    let id = crate::memoria::resolver_id(db, memory_id)?;
    anyhow::ensure!([DESFASADA, ESTADO_VIEJO, RUTA_ROTA, SIN_USO, LARGA].contains(&kind), "unknown review kind «{kind}»");
    db.execute(
        &format!(
            "INSERT INTO review(memory_id, kind, other_id, detail, found_at, dismissed_at) VALUES (?1, ?2, ?3, '', {AHORA}, {AHORA})
             ON CONFLICT(memory_id, kind, other_id) DO UPDATE SET dismissed_at = {AHORA}"
        ),
        params![id, kind, other_id],
    )?;
    Ok(())
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

/// Para el recall: una nota corta por memoria que conviene tomar con cuidado (desfasada o estado viejo).
pub fn notas(db: &Connection, memorias: &[&Memoria]) -> Result<HashMap<String, String>> {
    let mut r = HashMap::new();
    if memorias.is_empty() {
        return Ok(r);
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
