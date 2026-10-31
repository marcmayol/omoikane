//! A qué proyecto pertenece una carpeta: la raíz del repositorio git que la contiene, o la propia
//! carpeta si no hay ninguno. Siempre en forma canónica, para que `C:\Repo`, `c:\repo\.` y un
//! enlace simbólico a la misma carpeta sean el mismo proyecto.

use crate::tr;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Proyecto de la carpeta actual.
pub fn actual() -> Result<String> {
    de(&std::env::current_dir().with_context(|| tr!("can't read the current folder", "no se puede leer la carpeta actual"))?)
}

/// Proyecto de una carpeta.
pub fn de(dir: &Path) -> Result<String> {
    let dir = canonica(dir)?;
    let raiz = dir.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&dir);
    Ok(texto(raiz))
}

/// Si el proyecto es una carpeta general y no un proyecto: la carpeta del usuario, una por encima de
/// ella o la raíz de una unidad, sin repositorio git. Quien abre las sesiones ahí trabaja en muchos
/// proyectos a la vez, así que desde ahí se ven las memorias de todos.
pub fn es_general(project: &str) -> bool {
    match dirs::home_dir().and_then(|h| de(&h).ok()) {
        Some(home) => general_en(project, Path::new(&home)),
        None => false,
    }
}

fn general_en(project: &str, home: &Path) -> bool {
    let p = Path::new(project);
    !project.is_empty() && !p.join(".git").exists() && (home.starts_with(p) || p.parent().is_none())
}

/// Proyecto de una carpeta que alguien da a propósito (`--project`, `project`): una carpeta general
/// no es un proyecto, así que da global.
pub fn de_carpeta(dir: &Path) -> Result<String> {
    let p = de(dir)?;
    Ok(if es_general(&p) { String::new() } else { p })
}

/// Dónde guardar una memoria que no dice su proyecto: el actual si es un proyecto de verdad; si es
/// una carpeta general, el repositorio al que apunten sus rutas absolutas (las de `paths` y las que
/// cite el texto) cuando es uno solo, y si no, ninguno: global. Devuelve también las rutas: si el
/// proyecto salió de ellas, las que caen dentro de ese repositorio pasan a relativas a su raíz.
pub fn para_guardar(actual: &str, paths: &[String], texto: &str) -> (String, Vec<String>) {
    if !es_general(actual) {
        return (actual.to_string(), paths.to_vec());
    }
    let home = dirs::home_dir().and_then(|h| de(&h).ok()).unwrap_or_default();
    let home = Path::new(&home);
    let mut repos: Vec<String> = paths
        .iter()
        .cloned()
        .chain(rutas_en(texto))
        .filter_map(|r| repo_de_ruta(&r, home))
        .collect();
    repos.sort();
    repos.dedup();
    match repos.as_slice() {
        [uno] => (uno.clone(), paths.iter().map(|r| relativa_a(r, uno, home)).collect()),
        _ => (String::new(), paths.to_vec()),
    }
}

/// Palabras con pinta de ruta absoluta que cita un texto. Una ruta con espacios
/// (`C:\Users\marcm\the codex\app\x.kt`) llega partida en palabras: se va alargando con las
/// siguientes (como mucho `PALABRAS_MAX`) y se queda la que llega más hondo en el disco.
fn rutas_en(texto: &str) -> Vec<String> {
    const PALABRAS_MAX: usize = 6;
    let mut v = Vec::new();
    for trozo in texto.split(|c: char| c == '\n' || "`\"'«»()[]<>,;*".contains(c)) {
        let palabras: Vec<&str> = trozo.split(' ').filter(|p| !p.is_empty()).collect();
        for (i, p) in palabras.iter().enumerate() {
            if expandir(p, Path::new("")).is_none() {
                continue;
            }
            let mut mejor = (*p).to_string();
            let mut hondo = profundidad(&mejor);
            let mut candidata = mejor.clone();
            for siguiente in palabras.iter().skip(i + 1).take(PALABRAS_MAX) {
                candidata = format!("{candidata} {siguiente}");
                let h = profundidad(&candidata);
                if h > hondo {
                    (mejor, hondo) = (candidata.clone(), h);
                }
            }
            v.push(mejor.trim_end_matches(['.', ':', '?', '!']).to_string());
        }
    }
    v
}

/// Cuántos componentes de la ruta existen en el disco (la del antepasado existente más hondo).
fn profundidad(ruta: &str) -> usize {
    let r = PathBuf::from(ruta.trim_end_matches(['.', ':', '?', '!']));
    r.ancestors().find(|a| a.exists()).map_or(0, |a| a.components().count())
}

/// Una ruta absoluta (o con `~`), sin la puntuación del final; `None` si no lo es.
fn expandir(ruta: &str, home: &Path) -> Option<PathBuf> {
    let ruta = ruta.trim_end_matches(['.', ':', '?', '!']);
    let b = ruta.as_bytes();
    let absoluta = (b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || (cfg!(unix) && ruta.len() > 1 && ruta.starts_with('/'));
    if let Some(resto) = ruta.strip_prefix("~/").or_else(|| ruta.strip_prefix("~\\")) {
        Some(home.join(resto))
    } else if absoluta {
        Some(PathBuf::from(ruta))
    } else {
        None
    }
}

/// La ruta en forma canónica aunque el final aún no exista: el antepasado existente más hondo,
/// canónico, más el resto tal cual.
fn canonica_parcial(ruta: &Path) -> Option<PathBuf> {
    let existe = ruta.ancestors().find(|a| a.exists())?;
    let resto = ruta.strip_prefix(existe).ok()?;
    Some(PathBuf::from(texto(&canonica(existe).ok()?)).join(resto))
}

/// `ruta` relativa a `repo` si cae dentro; si no (o no es absoluta), tal cual.
fn relativa_a(ruta: &str, repo: &str, home: &Path) -> String {
    let Some(abs) = expandir(ruta, home).and_then(|p| canonica_parcial(&p)) else { return ruta.to_string() };
    match abs.strip_prefix(repo) {
        Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().replace('\\', "/"),
        _ => ruta.to_string(),
    }
}

/// El repositorio git que contiene una ruta absoluta (aunque el fichero ya no exista), si lo hay y no
/// es la carpeta del usuario.
fn repo_de_ruta(ruta: &str, home: &Path) -> Option<String> {
    let ruta = expandir(ruta, home)?;
    let existe = ruta.ancestors().find(|a| a.exists())?;
    let raiz = existe.ancestors().find(|d| d.join(".git").exists())?;
    let raiz = de(raiz).ok()?;
    // Los clones de herramientas (`~/.claude/plugins/…`, `~/.cargo/git/…`, `AppData`) no son proyectos.
    let oculto = Path::new(&raiz).strip_prefix(home).is_ok_and(|r| {
        r.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c.starts_with('.') || c.eq_ignore_ascii_case("AppData")
        })
    });
    (Path::new(&raiz) != home && !oculto).then_some(raiz)
}

fn canonica(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| tr!("{} doesn't exist", "no existe {}", p.display()))?;
    // En Windows, canonicalize devuelve `\\?\C:\…`: se quita el prefijo para guardar la ruta normal.
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(resto) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{resto}")));
        }
        if let Some(resto) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(resto));
        }
    }
    Ok(c)
}

fn texto(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    // La letra de unidad en mayúscula: así coinciden las rutas que llegan de sitios distintos.
    #[cfg(windows)]
    {
        let mut b = s.into_bytes();
        if b.len() >= 2 && b[1] == b':' {
            b[0] = b[0].to_ascii_uppercase();
        }
        return String::from_utf8(b).expect("ruta UTF-8");
    }
    #[allow(unreachable_code)]
    s
}

// --- Proyectos y espacios en la base (esquema 10) -------------------------------------------------
//
// Un proyecto es una fila de `project` con su clave de sincronización (calculada una vez, al crearlo)
// y todas las carpetas donde vive en `project_path`: un clon, un worktree, la carpeta a la que se
// movió o la ruta que trajo otro equipo (`is_foreign`). Un espacio (`space`) agrupa proyectos de un
// mismo cliente: desde un proyecto se ven las memorias suyas, las de su espacio y las globales, nunca
// las de otro espacio.

/// Cómo se comparan dos rutas: en Windows y macOS (sistemas de ficheros que no distinguen mayúsculas),
/// en minúsculas.
pub fn comparable(ruta: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) { ruta.to_lowercase() } else { ruta.to_string() }
}

/// El nombre corto de un proyecto: el de su carpeta (o, sin ruta, el final de la clave).
pub fn nombre_de(ruta: &str, clave: &str) -> String {
    let desde_ruta = ruta.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").to_string();
    if !desde_ruta.is_empty() {
        return desde_ruta;
    }
    clave.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or(clave).to_string()
}

/// El `origin` de git de una carpeta, normalizado como en las claves (vacío si no tiene).
pub fn origen(ruta: &str) -> String {
    if ruta.is_empty() {
        return String::new();
    }
    crate::sync::origen_git(Path::new(ruta)).map(|u| crate::sync::normalizar_remoto(&u)).unwrap_or_default()
}

fn ahora(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |f| f.get(0))?)
}

/// El proyecto (y su espacio) de una carpeta que ya está apuntada.
fn por_ruta(db: &Connection, ruta: &str) -> Result<Option<(i64, Option<i64>)>> {
    Ok(db
        .query_row(
            "SELECT pp.project_id, p.space_id FROM project_path pp JOIN project p ON p.id = pp.project_id WHERE pp.path_cmp = ?1",
            [comparable(ruta)],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?)
}

/// Qué es una carpeta que aún no está apuntada.
enum Decision {
    /// Otra carpeta del proyecto que ya tiene esa clave (un clon, un worktree, la carpeta movida de un
    /// repositorio con `origin`, o la carpeta local de un proyecto que llegó de otro equipo).
    Unir(i64, Option<i64>),
    /// Un proyecto nuevo, con esta clave.
    Nuevo(String),
}

fn decidir(db: &Connection, ruta: &str, persona: &str) -> Result<Decision> {
    let clave = crate::sync::clave(ruta, persona);
    let existente: Option<(i64, Option<i64>)> =
        db.query_row("SELECT id, space_id FROM project WHERE key = ?1", [&clave], |f| Ok((f.get(0)?, f.get(1)?))).optional()?;
    let Some((id, espacio)) = existente else { return Ok(Decision::Nuevo(clave)) };
    // Con `origin`, la misma clave es el mismo repositorio. Sin él (`dir:`), dos carpetas con el mismo
    // nombre pueden ser cosas distintas: solo se junta con un proyecto que aquí no tiene ninguna
    // carpeta propia (el de otro equipo de la misma persona). Una carpeta movida sin `origin` se
    // arregla con `omoi projects move` (Revisar la enseña como «ya no está»).
    if clave.starts_with("git:") || carpetas_propias(db, id)? == 0 {
        return Ok(Decision::Unir(id, espacio));
    }
    let mut n = 2;
    loop {
        let k = format!("{clave}~{n}");
        let ya: Option<(i64, Option<i64>, i64)> = db
            .query_row(
                "SELECT p.id, p.space_id, (SELECT count(*) FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0) FROM project p WHERE p.key = ?1",
                [&k],
                |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)),
            )
            .optional()?;
        match ya {
            None => return Ok(Decision::Nuevo(k)),
            Some((id, espacio, 0)) => return Ok(Decision::Unir(id, espacio)),
            Some(_) => n += 1,
        }
    }
}

/// Cuántas carpetas propias (de este equipo, no las que trajo otro) tiene un proyecto.
pub fn carpetas_propias(db: &Connection, proyecto: i64) -> Result<i64> {
    Ok(db.query_row("SELECT count(*) FROM project_path WHERE project_id = ?1 AND is_foreign = 0", [proyecto], |f| f.get(0))?)
}

/// Si una carpeta ya está apuntada como carpeta de un proyecto.
pub fn apuntada(db: &Connection, ruta: &str) -> Result<bool> {
    Ok(por_ruta(db, ruta)?.is_some())
}

/// El proyecto de una carpeta, sin escribir nada: el apuntado o, si no lo está, el que tiene su clave
/// (si la carpeta se juntaría con él). `None`: un proyecto que aún no tiene nada. La carpeta vacía es
/// lo global (0).
pub fn resolver(db: &Connection, ruta: &str) -> Result<Option<(i64, Option<i64>)>> {
    if ruta.is_empty() {
        return Ok(Some((0, None)));
    }
    if let Some(r) = por_ruta(db, ruta)? {
        return Ok(Some(r));
    }
    Ok(match decidir(db, ruta, &crate::db::persona_de(db))? {
        Decision::Unir(id, espacio) => Some((id, espacio)),
        Decision::Nuevo(_) => None,
    })
}

/// El id del proyecto de una carpeta, apuntándola si hace falta (y creando el proyecto si es nuevo).
/// De paso apunta cuándo se vio y su `origin`: si cambió, la clave no cambia (las memorias de los
/// demás equipos la siguen usando) y Revisar lo avisa.
pub fn registrar(db: &Connection, ruta: &str) -> Result<i64> {
    Ok(registrar_avisando(db, ruta)?.0)
}

/// Como `registrar`, y además la carpeta donde estaba el proyecto si esta es la que la sustituye
/// (aquella ya no existe: la carpeta se movió y el `origin` lo dice). Pasa una sola vez: después, la
/// principal ya es esta.
pub fn registrar_avisando(db: &Connection, ruta: &str) -> Result<(i64, Option<String>)> {
    if ruta.is_empty() {
        return Ok((0, None));
    }
    let ahora = ahora(db)?;
    let existe = Path::new(ruta).is_dir();
    if let Some((id, espacio)) = por_ruta(db, ruta)? {
        if existe {
            // La ruta que trajo otro equipo existe ahora aquí: es la primera carpeta de este equipo, y
            // como al unirse por la clave, entrar así en un espacio pide confirmarlo.
            let reclamado = espacio.is_some() && carpetas_propias(db, id)? == 0;
            db.execute(
                "UPDATE project_path SET is_foreign = 0, origin = ?2, last_seen = ?3
                 WHERE path_cmp = ?1 AND (is_foreign = 1 OR origin != ?2 OR last_seen IS NULL OR last_seen < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day'))",
                params![comparable(ruta), origen(ruta), ahora],
            )?;
            if reclamado {
                pausar_por_reclamo(db, id)?;
            }
            return Ok((id, principal_si_hace_falta(db, id, ruta)?));
        }
        return Ok((id, None));
    }
    let (id, reclamado) = match decidir(db, ruta, &crate::db::persona_de(db))? {
        // Un proyecto que está en un espacio sin tener aquí ninguna carpeta propia está en él porque lo
        // dice el repositorio de ese espacio (una línea `project`, o una memoria con su clave): esta
        // carpeta no entra en él sin que la persona lo confirme.
        Decision::Unir(id, espacio) => (id, espacio.is_some() && carpetas_propias(db, id)? == 0),
        Decision::Nuevo(clave) => {
            db.execute(
                "INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![clave, nombre_de(ruta, &clave), ruta, ahora],
            )?;
            (db.last_insert_rowid(), false)
        }
    };
    db.execute(
        "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![ruta, comparable(ruta), id, origen(ruta), existe.then_some(&ahora)],
    )?;
    if reclamado {
        pausar_por_reclamo(db, id)?;
    }
    Ok((id, principal_si_hace_falta(db, id, ruta)?))
}

/// La ruta principal de un proyecto (la que enseñan las memorias) pasa a `ruta` si la de ahora es de
/// otro equipo o ya no existe aquí (la carpeta se movió). Devuelve la de antes si era de aquí y se
/// movió (para avisar).
fn principal_si_hace_falta(db: &Connection, id: i64, ruta: &str) -> Result<Option<String>> {
    let (actual, ajena): (String, bool) = db.query_row(
        "SELECT p.path, COALESCE((SELECT is_foreign FROM project_path WHERE path = p.path), 1) FROM project p WHERE p.id = ?1",
        [id],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?;
    if actual != ruta && Path::new(ruta).is_dir() && (ajena || actual.is_empty() || !Path::new(&actual).is_dir()) {
        db.execute("UPDATE project SET path = ?2, name = ?3 WHERE id = ?1", params![id, ruta, nombre_de(ruta, "")])?;
        return Ok((!ajena && !actual.is_empty()).then_some(actual));
    }
    Ok(None)
}

/// `omoi projects prune`: los proyectos sin memorias (y sus carpetas apuntadas) se borran. El de una
/// carpeta en la que se vuelva a trabajar se crea otra vez solo. Devuelve cuántos.
pub fn podar(db: &Connection) -> Result<usize> {
    db.execute("DELETE FROM project_path WHERE project_id != 0 AND project_id NOT IN (SELECT DISTINCT project_id FROM memory)", [])?;
    Ok(db.execute("DELETE FROM project WHERE id != 0 AND id NOT IN (SELECT DISTINCT project_id FROM memory)", [])?)
}

/// El proyecto de una clave que llega de otro equipo (o de un fichero de `omoi export`), creándolo con
/// la ruta de origen si aquí no hay ninguno. Si esa ruta existe aquí con la misma clave, es la carpeta
/// local del proyecto; si existe con otra clave o ya es de otro proyecto, la ruta ajena lleva la
/// clave detrás (`/workspace [git:…]`) para no confundirse con él.
///
/// Con `de_copia` (un `omoi import` o una restauración de la papelera, no la sincronización), una
/// carpeta sin apuntar que existe aquí en la ruta de origen y que tendría esa misma clave con la
/// persona de la clave (`dir:<persona>/<carpeta>`) es esa carpeta: la copia es de esta misma carpeta,
/// quizá de otra base con otra persona local. Por la sincronización no: dos personas pueden tener la
/// misma ruta en sus equipos.
pub fn de_clave(db: &Connection, clave: &str, ruta_origen: &str, persona: &str, de_copia: bool) -> Result<i64> {
    if clave.is_empty() {
        return Ok(0);
    }
    if let Some(id) = db.query_row("SELECT id FROM project WHERE key = ?1", [clave], |f| f.get(0)).optional()? {
        return Ok(id);
    }
    let existe = !ruta_origen.is_empty() && Path::new(ruta_origen).is_dir();
    if existe && por_ruta(db, ruta_origen)?.is_none() {
        if crate::sync::clave(ruta_origen, persona) == clave {
            return registrar(db, ruta_origen);
        }
        let suya = clave.strip_prefix("dir:").and_then(|r| r.split_once('/')).map(|(p, _)| p);
        if de_copia && suya.is_some_and(|p| crate::sync::clave(ruta_origen, p) == clave) {
            let ahora = ahora(db)?;
            db.execute("INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)", params![clave, nombre_de(ruta_origen, clave), ruta_origen, ahora])?;
            let id = db.last_insert_rowid();
            db.execute(
                "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, '', 0, ?4)",
                params![ruta_origen, comparable(ruta_origen), id, ahora],
            )?;
            return Ok(id);
        }
    }
    let libre = |r: &str| -> Result<bool> { Ok(por_ruta(db, r)?.is_none() && !Path::new(r).exists()) };
    let mut ruta = if ruta_origen.is_empty() { format!("[{clave}]") } else { ruta_origen.to_string() };
    if !libre(&ruta)? {
        ruta = format!("{ruta_origen} [{clave}]");
    }
    let base = ruta.clone();
    let mut n = 2;
    while !libre(&ruta)? {
        ruta = format!("{base} ({n})");
        n += 1;
    }
    let ahora = ahora(db)?;
    db.execute("INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)", params![clave, nombre_de(ruta_origen, clave), ruta, ahora])?;
    let id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, '', 1, NULL)",
        params![ruta, comparable(&ruta), id],
    )?;
    Ok(id)
}

/// Un proyecto, como lo enseñan `context`, `omoi projects` y Revisar.
#[derive(Clone, Debug, Serialize)]
pub struct Proyecto {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub path: String,
    pub key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub memories: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_update: String,
    /// La memoria `status` más reciente, en una línea.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// Su carpeta ya no existe en este equipo (y no es de otro equipo).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gone: bool,
    /// El `origin` de git de alguna de sus carpetas ya no es el de su clave.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub origin_changed: String,
}

/// Los proyectos con memorias (o el que se pida, aunque no tenga), del más al menos activo. Sin lo
/// global. Con `espacio`, solo los de ese espacio (`Some(None)`: los que no tienen).
pub fn lista(db: &Connection, solo: Option<i64>, espacio: Option<Option<i64>>, limite: usize) -> Result<Vec<Proyecto>> {
    // Cuántas y la última, de todos los proyectos en una pasada por el índice `memory_scope` (con
    // 50.000 memorias, una subconsulta por proyecto recorría la tabla entera una vez por cada uno).
    let por_proyecto: std::collections::HashMap<i64, (i64, String)> = {
        let mut q = db.prepare("SELECT project_id, count(*), max(updated_at) FROM memory WHERE project_id != 0 GROUP BY project_id")?;
        q.query_map([], |f| Ok((f.get(0)?, (f.get(1)?, f.get(2)?))))?.collect::<rusqlite::Result<_>>()?
    };
    let mut q = db.prepare(
        "SELECT p.id, p.name, p.path, p.key, COALESCE(s.key, ''),
                COALESCE((SELECT is_foreign FROM project_path WHERE path = p.path), 0),
                COALESCE((SELECT origin FROM project_path WHERE project_id = p.id AND is_foreign = 0 AND origin != ''
                            AND p.key LIKE 'git:%' AND 'git:' || origin != p.key LIMIT 1), ''),
                p.space_id
         FROM project p LEFT JOIN space s ON s.id = p.space_id
         WHERE p.id != 0 AND (?1 IS NULL OR p.id = ?1)",
    )?;
    // (proyecto, su ruta principal es de otro equipo, su espacio)
    let mut v: Vec<(Proyecto, bool, Option<i64>)> = q
        .query_map(params![solo], |f| {
            let id: i64 = f.get(0)?;
            let (memories, last_update) = por_proyecto.get(&id).cloned().unwrap_or_default();
            Ok((
                Proyecto {
                    id,
                    name: f.get(1)?,
                    path: f.get(2)?,
                    key: f.get(3)?,
                    space: f.get(4)?,
                    memories,
                    last_update,
                    status: String::new(),
                    gone: false,
                    origin_changed: f.get(6)?,
                },
                f.get(5)?,
                f.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    v.retain(|(p, _, e)| (p.memories > 0 || solo.is_some()) && espacio.is_none_or(|x| x == *e));
    v.sort_by(|a, b| b.0.last_update.cmp(&a.0.last_update).then(a.0.name.cmp(&b.0.name)));
    v.truncate(limite);
    // El `status` más reciente de cada proyecto, en una pasada por la tabla: el `LIKE` descarta sin leer
    // el JSON lo que no lleva la etiqueta, y con `max`, SQLite da el título de esa misma fila. `NOT
    // INDEXED`: por el índice de proyecto, SQLite iba fila a fila a la tabla (210 ms con 50.000
    // memorias, frente a 30 recorriéndola de seguido).
    let estados: std::collections::HashMap<i64, String> = {
        let mut q = db.prepare(
            "SELECT project_id, title, max(updated_at) FROM memory NOT INDEXED
             WHERE project_id != 0 AND tags LIKE ?1 AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?2)
             GROUP BY project_id",
        )?;
        let etiqueta = crate::memoria::ESTADO;
        q.query_map(params![format!("%\"{etiqueta}\"%"), etiqueta], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    Ok(v.into_iter()
        .map(|(mut p, ajena, _)| {
            p.gone = !ajena && !p.path.is_empty() && !Path::new(&p.path).is_dir();
            p.status = estados.get(&p.id).cloned().unwrap_or_default();
            p
        })
        .collect())
}

/// El proyecto que dice una persona o un agente: una carpeta, o un nombre o trozo de nombre de los
/// proyectos conocidos («balua»). Error si no encaja con ninguno o encaja con varios (con la lista).
pub fn buscar(db: &Connection, texto: &str) -> Result<i64> {
    let t = texto.trim();
    anyhow::ensure!(!t.is_empty(), tr!("which project? give its folder or its name", "¿qué proyecto? di su carpeta o su nombre"));
    let parece_ruta = t.contains(['/', '\\']) || t == "." || t == "..";
    if (parece_ruta || Path::new(t).is_dir())
        && let Ok(ruta) = de(Path::new(t))
    {
        if let Some((id, _)) = resolver(db, &ruta)? {
            return Ok(id);
        }
        anyhow::ensure!(!parece_ruta, tr!("no memories in the project of {ruta}", "el proyecto de {ruta} no tiene memorias"));
    }
    if parece_ruta && let Some((id, _)) = por_ruta(db, t)? {
        return Ok(id);
    }
    let todos: Vec<(i64, String, String)> = {
        let mut q = db.prepare("SELECT id, name, path FROM project WHERE id != 0 ORDER BY name")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let n = t.to_lowercase();
    let exactos: Vec<&(i64, String, String)> = todos.iter().filter(|(_, nombre, _)| nombre.to_lowercase() == n).collect();
    let trozos: Vec<&(i64, String, String)> = todos.iter().filter(|(_, nombre, _)| nombre.to_lowercase().contains(&n)).collect();
    let elegidos = if exactos.is_empty() { trozos } else { exactos };
    match elegidos.as_slice() {
        [uno] => Ok(uno.0),
        [] => anyhow::bail!(tr!(
            "no project matches «{t}» (known: {})",
            "ningún proyecto encaja con «{t}» (hay: {})",
            todos.iter().map(|x| x.1.as_str()).collect::<Vec<_>>().join(", ")
        )),
        varios => anyhow::bail!(tr!(
            "«{t}» matches several projects, say which: {}",
            "«{t}» encaja con varios proyectos, di cuál: {}",
            varios.iter().map(|x| format!("{} ({})", x.1, x.2)).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// La clave de un proyecto por su id (vacía si no existe o es lo global).
pub fn clave_de(db: &Connection, proyecto: i64) -> Result<String> {
    Ok(db.query_row("SELECT key FROM project WHERE id = ?1", [proyecto], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// El espacio de un proyecto (`None` si no tiene, o si el proyecto no existe).
pub fn espacio_de(db: &Connection, proyecto: i64) -> Result<Option<i64>> {
    Ok(db.query_row("SELECT space_id FROM project WHERE id = ?1", [proyecto], |f| f.get(0)).optional()?.flatten())
}

/// Un espacio, como lo enseña `omoi space list`.
#[derive(Clone, Debug, Serialize)]
pub struct Espacio {
    #[serde(skip)]
    pub id: i64,
    pub key: String,
    pub name: String,
    pub projects: Vec<String>,
    pub memories: i64,
}

/// La clave de un espacio a partir de su nombre: minúsculas, letras, números y guiones.
pub fn clave_de_espacio(nombre: &str) -> String {
    let k: String = nombre.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    k.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-")
}

/// Crea un espacio. Error si ya hay uno con la misma clave.
pub fn crear_espacio(db: &Connection, nombre: &str) -> Result<i64> {
    let clave = clave_de_espacio(nombre);
    anyhow::ensure!(!clave.is_empty(), tr!("the space needs a name", "el espacio necesita un nombre"));
    let ya: Option<i64> = db.query_row("SELECT id FROM space WHERE key = ?1", [&clave], |f| f.get(0)).optional()?;
    anyhow::ensure!(ya.is_none(), tr!("the space «{clave}» already exists", "el espacio «{clave}» ya existe"));
    db.execute("INSERT INTO space(key, name, created_at) VALUES (?1, ?2, ?3)", params![clave, nombre.trim(), ahora(db)?])?;
    Ok(db.last_insert_rowid())
}

/// El espacio de una clave que llega de otro equipo, creándolo si aquí no está.
pub fn espacio_de_clave(db: &Connection, clave: &str) -> Result<Option<i64>> {
    if clave.is_empty() {
        return Ok(None);
    }
    if let Some(id) = db.query_row("SELECT id FROM space WHERE key = ?1", [clave], |f| f.get(0)).optional()? {
        return Ok(Some(id));
    }
    db.execute("INSERT INTO space(key, name, created_at) VALUES (?1, ?1, ?2)", params![clave, ahora(db)?])?;
    Ok(Some(db.last_insert_rowid()))
}

/// El espacio que dice una persona o un agente, por su clave o su nombre. Error (con la lista) si no hay.
pub fn buscar_espacio(db: &Connection, texto: &str) -> Result<i64> {
    let t = texto.trim();
    let id: Option<i64> = db
        .query_row(
            "SELECT id FROM space WHERE key = ?1 OR lower(name) = lower(?2) ORDER BY key = ?1 DESC LIMIT 1",
            params![clave_de_espacio(t), t],
            |f| f.get(0),
        )
        .optional()?;
    id.ok_or_else(|| {
        let hay: Vec<String> = db
            .prepare("SELECT key FROM space ORDER BY key")
            .and_then(|mut q| q.query_map([], |f| f.get(0))?.collect())
            .unwrap_or_default();
        anyhow::anyhow!(tr!(
            "no space «{t}» (there are: {}); create it with `omoi space add {t}`",
            "no hay ningún espacio «{t}» (hay: {}); créalo con `omoi space add {t}`",
            if hay.is_empty() { "-".to_string() } else { hay.join(", ") }
        ))
    })
}

/// La clave de un espacio por su id (vacía si no hay).
pub fn clave_espacio(db: &Connection, id: Option<i64>) -> Result<String> {
    let Some(id) = id else { return Ok(String::new()) };
    Ok(db.query_row("SELECT key FROM space WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Los espacios, con sus proyectos y cuántas memorias tienen (las de sus proyectos y las suyas).
pub fn espacios(db: &Connection) -> Result<Vec<Espacio>> {
    let mut q = db.prepare(
        "SELECT s.id, s.key, s.name,
                (SELECT count(*) FROM memory m WHERE m.space_id = s.id OR m.project_id IN (SELECT id FROM project WHERE space_id = s.id))
         FROM space s ORDER BY s.key",
    )?;
    let v: Vec<Espacio> = q
        .query_map([], |f| Ok(Espacio { id: f.get(0)?, key: f.get(1)?, name: f.get(2)?, projects: Vec::new(), memories: f.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let mut p = db.prepare("SELECT name FROM project WHERE space_id = ?1 ORDER BY name")?;
    v.into_iter()
        .map(|mut e| {
            e.projects = p.query_map([e.id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(e)
        })
        .collect()
}

/// Mete un proyecto en un espacio (o lo saca, con `None`). El proyecto se lleva un sello nuevo
/// (`project.hlc`), con el que la sincronización lleva su espacio a los demás equipos en una línea
/// `project`; sus memorias no cambian de sello (si cambiaran, una versión vieja de aquí ganaría a
/// una más nueva de otro equipo solo por haberse movido). Si estaba en pausa (`Pausa`), es la decisión
/// de la persona: sale de ella, con un sello posterior al de la línea que lo pausó (así esa línea ya
/// no vuelve a pausarlo). Devuelve cuántas memorias tiene.
pub fn asignar_espacio(db: &Connection, proyecto: i64, espacio: Option<i64>) -> Result<usize> {
    anyhow::ensure!(proyecto != 0, tr!("global memories have no project to put in a space", "las memorias globales no tienen proyecto que meter en un espacio"));
    let (antes, previo, pausa): (Option<i64>, String, String) =
        db.query_row("SELECT space_id, hlc, paused FROM project WHERE id = ?1", [proyecto], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    let pausada = !pausa.is_empty();
    if antes != espacio || previo.is_empty() || pausada {
        let previo = match Pausa::de(&pausa) {
            Some(p) if crate::reloj::mayor(&p.hlc, &previo) => p.hlc,
            _ => previo,
        };
        let hlc = crate::reloj::sello_tras(db, &previo)?;
        db.execute("UPDATE project SET space_id = ?2, hlc = ?3, paused = '' WHERE id = ?1", params![proyecto, espacio, hlc])?;
    }
    Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [proyecto], |f| f.get::<_, i64>(0))? as usize)
}

/// Un proyecto en pausa: una línea `project` (de otro equipo, o escrita a mano por quien tenga la clave
/// de un repositorio) dice que está en otro espacio. Esa línea no cambia nada: hasta que la persona
/// decida aquí (`omoi projects move` o `omoi projects keep`), sus memorias no se suben a ningún
/// repositorio ni se aceptan las que lleguen de él. Es local (`project.paused`, en JSON): no viaja.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pausa {
    /// El espacio que dice la línea (su clave; vacío: ninguno).
    #[serde(default)]
    pub space: String,
    /// La línea solo dice que otro equipo lo pasó a un espacio, sin decir a cuál (la que va por el
    /// repositorio general, que no nombra clientes).
    #[serde(default)]
    pub unknown: bool,
    /// No la puso una línea `project`: una carpeta de aquí se unió a un proyecto que estaba en un
    /// espacio (`space`) solo porque lo dice el repositorio de ese espacio (aquí no tenía ninguna carpeta
    /// propia). Sale de ella confirmándolo (`omoi projects keep`) o sacándolo del espacio.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claimed: bool,
    /// El sello de esa línea.
    #[serde(default)]
    pub hlc: String,
    /// El equipo de su sello (el que lo movió).
    #[serde(default)]
    pub machine: String,
    /// Cuándo se pausó aquí.
    #[serde(default)]
    pub at: String,
}

impl Pausa {
    /// La pausa guardada en `project.paused` (`None` si no hay, o no se entiende).
    pub fn de(texto: &str) -> Option<Pausa> {
        if texto.is_empty() { None } else { serde_json::from_str(texto).ok() }
    }
}

/// Deja el proyecto en pausa (sin tocar su espacio ni su sello).
pub fn pausar(db: &Connection, proyecto: i64, pausa: &Pausa) -> Result<()> {
    let p = Pausa { at: ahora(db)?, ..pausa.clone() };
    db.execute("UPDATE project SET paused = ?2 WHERE id = ?1", params![proyecto, serde_json::to_string(&p)?])?;
    Ok(())
}

/// Deja en pausa (`Pausa::claimed`) un proyecto de un espacio al que acaba de unirse una carpeta de
/// aquí sin que este equipo tuviera ninguna: el repositorio de ese espacio dice que es suyo, pero
/// puede no serlo (una librería personal que un compañero usa en el cliente, o una línea escrita a
/// propósito para quedarse con lo que se guarde en esa carpeta). Si ya estaba en pausa, se queda la que
/// tenía.
pub fn pausar_por_reclamo(db: &Connection, proyecto: i64) -> Result<()> {
    let (espacio, hlc, pausa): (Option<i64>, String, String) =
        db.query_row("SELECT space_id, hlc, paused FROM project WHERE id = ?1", [proyecto], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    if espacio.is_none() || !pausa.is_empty() {
        return Ok(());
    }
    let machine = crate::reloj::partes(&hlc).2.to_string();
    pausar(db, proyecto, &Pausa { space: clave_espacio(db, espacio)?, claimed: true, hlc, machine, ..Default::default() })
}

/// Un proyecto en pausa, como lo enseñan `omoi sync`, `doctor`, `review` y Revisar.
#[derive(Clone, Debug, Serialize)]
pub struct EnPausa {
    pub key: String,
    pub name: String,
    /// Su espacio aquí (su clave; vacío: ninguno).
    pub here: String,
    #[serde(flatten)]
    pub pausa: Pausa,
}

/// Los proyectos en pausa, por nombre.
pub fn en_pausa(db: &Connection) -> Result<Vec<EnPausa>> {
    let mut q = db.prepare(
        "SELECT p.key, p.name, COALESCE((SELECT key FROM space WHERE id = p.space_id), ''), p.paused FROM project p WHERE p.paused != '' ORDER BY p.name, p.key",
    )?;
    let filas: Vec<(String, String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(filas
        .into_iter()
        .map(|(key, name, here, t)| EnPausa { key, name, here, pausa: Pausa::de(&t).unwrap_or_default() })
        .collect())
}

/// Si el proyecto de esta clave está en pausa.
pub fn pausado(db: &Connection, clave: &str) -> Result<bool> {
    Ok(db.query_row("SELECT paused != '' FROM project WHERE key = ?1", [clave], |f| f.get(0)).optional()?.unwrap_or(false))
}

/// `omoi projects keep`: el proyecto se queda en su espacio y sale de la pausa; su sello pasa por
/// delante del de la línea que lo pausó, para que no vuelva a pausarlo (y los demás equipos sepan que
/// aquí se quedó donde estaba). `None` si no estaba en pausa; si no, su espacio (su clave).
pub fn quedarse(db: &Connection, proyecto: i64) -> Result<Option<String>> {
    let pausa: Option<String> = db.query_row("SELECT paused FROM project WHERE id = ?1", [proyecto], |f| f.get(0)).optional()?;
    if pausa.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }
    let espacio = espacio_de(db, proyecto)?;
    asignar_espacio(db, proyecto, espacio)?;
    Ok(Some(clave_espacio(db, espacio)?))
}

/// Un sello nuevo para cada memoria del proyecto (sin tocar su contenido ni `content_hlc`): así viaja
/// lo que cambió de su clave (al cambiar de persona). El espacio no: viaja en la línea `project`.
fn resellar(db: &Connection, proyecto: i64) -> Result<usize> {
    let ids: Vec<String> = {
        let mut q = db.prepare("SELECT id FROM memory WHERE project_id = ?1")?;
        q.query_map([proyecto], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    resellar_estas(db, &ids)?;
    Ok(ids.len())
}

/// Un sello nuevo para cada una de estas memorias.
fn resellar_estas(db: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        let hlc: String = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let nuevo = crate::reloj::sello_tras(db, &hlc)?;
        db.execute("UPDATE memory SET hlc = ?2 WHERE id = ?1", params![id, nuevo])?;
    }
    Ok(())
}

/// Las claves `dir:` de los proyectos de aquí (los que tienen alguna carpeta propia) pasan de la
/// persona `vieja` a la `nueva` (`dir:<vieja>/x` → `dir:<nueva>/x`; con `vieja` vacía, las `dir:/x`
/// que se quedaron sin persona), y sus memorias cambian de sello para que viajen con la clave buena.
/// Si la clave nueva ya es de un proyecto que aquí solo existe por otro equipo y está en el mismo
/// espacio, es el mismo proyecto y se juntan; si no, la nueva lleva sufijo (`~2`). Devuelve cuántos
/// proyectos cambiaron.
pub fn cambiar_persona(db: &Connection, vieja: &str, nueva: &str) -> Result<usize> {
    if vieja == nueva {
        return Ok(0);
    }
    let prefijo = format!("dir:{vieja}/");
    let proyectos: Vec<(i64, String, Option<i64>)> = {
        let mut q = db.prepare(
            "SELECT p.id, p.key, p.space_id FROM project p
             WHERE substr(p.key, 1, length(?1)) = ?1 AND EXISTS (SELECT 1 FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0)",
        )?;
        q.query_map([&prefijo], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, clave, espacio) in &proyectos {
        let base = format!("dir:{nueva}/{}", &clave[prefijo.len()..]);
        let mut k = base.clone();
        let mut n = 2;
        loop {
            let otro: Option<(i64, Option<i64>, i64)> = db
                .query_row(
                    "SELECT p.id, p.space_id, (SELECT count(*) FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0) FROM project p WHERE p.key = ?1",
                    [&k],
                    |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)),
                )
                .optional()?;
            match otro {
                None => break,
                Some((o, e, 0)) if e == *espacio => {
                    juntar(db, o, *id)?;
                    break;
                }
                Some(_) => {
                    k = format!("{base}~{n}");
                    n += 1;
                }
            }
        }
        db.execute("UPDATE project SET key = ?2 WHERE id = ?1", params![id, k])?;
        resellar(db, *id)?;
    }
    Ok(proyectos.len())
}

/// Las memorias, carpetas y usos del proyecto `de` pasan a `en`, y `de` desaparece (sin tocar el
/// espacio de `en` ni los sellos: eso lo decide quien llama).
fn juntar(db: &Connection, de: i64, en: i64) -> Result<usize> {
    let n = db.execute("UPDATE memory SET project_id = ?2 WHERE project_id = ?1", params![de, en])?;
    db.execute("UPDATE project_path SET project_id = ?2 WHERE project_id = ?1", params![de, en])?;
    db.execute("UPDATE recall_log SET project_id = ?2 WHERE project_id = ?1", params![de, en])?;
    db.execute(
        "INSERT INTO memory_use(memory_id, project_id, veces, ultima) SELECT memory_id, ?2, veces, ultima FROM memory_use WHERE project_id = ?1
         ON CONFLICT(memory_id, project_id) DO UPDATE SET veces = veces + excluded.veces, ultima = max(ultima, excluded.ultima)",
        params![de, en],
    )?;
    db.execute("DELETE FROM memory_use WHERE project_id = ?1", [de])?;
    db.execute("DELETE FROM project WHERE id = ?1", [de])?;
    Ok(n)
}

/// Error si juntar `de` en `en` cambiaría de espacio memorias de alguno de los dos: las de `de` (si
/// tiene) acabarían en el espacio de `en`, y las de `en`, si no tiene espacio, en el de `de`. Así lo de
/// un cliente no pasa al repositorio de otro (ni al personal) por un `merge` o un `move`.
fn mismos_espacios(db: &Connection, de: i64, en: i64) -> Result<()> {
    let (ed, ee) = (espacio_de(db, de)?, espacio_de(db, en)?);
    if ed == ee {
        return Ok(());
    }
    let cuantas = |p: i64| -> Result<i64> { Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [p], |f| f.get(0))?) };
    if cuantas(de)? == 0 && (ee.is_some() || cuantas(en)? == 0) {
        return Ok(());
    }
    let nombre = |e: Option<i64>| -> Result<String> {
        Ok(match e {
            Some(_) => format!("«{}»", clave_espacio(db, e)?),
            None => tr!("no space", "ninguno"),
        })
    };
    anyhow::bail!(tr!(
        "the projects are in different spaces ({} → {}): their memories would go to another space's sync repository. Add --force if that is what you want",
        "los proyectos están en espacios distintos ({} → {}): sus memorias irían al repositorio de otro espacio. Añade --force si es lo que quieres",
        nombre(ed)?,
        nombre(ee)?
    ))
}

/// Junta el proyecto `de` en `en`: sus memorias (con su id, historial y veredictos) y sus carpetas pasan
/// a `en`, y `de` desaparece. Si los dos tenían memoria `status`, la más nueva se queda y la otra queda
/// sustituida por ella. Entre proyectos de espacios distintos, solo con `forzar`. Devuelve cuántas
/// memorias se movieron.
pub fn fundir(db: &mut Connection, de: i64, en: i64, forzar: bool) -> Result<usize> {
    anyhow::ensure!(de != 0 && en != 0, tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    anyhow::ensure!(de != en, tr!("it's the same project", "es el mismo proyecto"));
    if !forzar {
        mismos_espacios(db, de, en)?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let estado = |p: i64| -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT id FROM memory WHERE project_id = ?1 AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?2) ORDER BY updated_at DESC LIMIT 1",
                params![p, crate::memoria::ESTADO],
                |f| f.get(0),
            )
            .optional()?)
    };
    let (viejo, nuevo) = (estado(de)?, estado(en)?);
    // `en` toma el espacio de `de` si no tenía (con sello nuevo: viaja en su línea `project`).
    if espacio_de(&tx, en)?.is_none()
        && let Some(e) = espacio_de(&tx, de)?
    {
        asignar_espacio(&tx, en, Some(e))?;
    }
    // Las memorias de `de` viajan ahora con la clave de `en`: sello nuevo (las de `en` no cambian).
    let movidas: Vec<String> = {
        let mut q = tx.prepare("SELECT id FROM memory WHERE project_id = ?1")?;
        q.query_map([de], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let n = juntar(&tx, de, en)?;
    resellar_estas(&tx, &movidas)?;
    if let (Some(a), Some(b)) = (viejo, nuevo) {
        let (vieja, nueva): (String, String) = tx.query_row(
            "SELECT CASE WHEN a.updated_at <= b.updated_at THEN a.id ELSE b.id END, CASE WHEN a.updated_at <= b.updated_at THEN b.id ELSE a.id END
             FROM memory a, memory b WHERE a.id = ?1 AND b.id = ?2",
            params![a, b],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )?;
        crate::veredictos::poner(&tx, &vieja, &nueva, crate::veredictos::SUSTITUIDA, "projects merged: the newer status stays", "omoikane")?;
    }
    tx.commit()?;
    Ok(n)
}

/// `omoi projects move`: el proyecto vive ahora en `carpeta`. Se queda con su clave (los demás equipos
/// la conocen) y suma la carpeta; si la carpeta ya era otro proyecto, ese se funde en este (si es de
/// otro espacio, solo con `forzar`).
pub fn mover(db: &mut Connection, proyecto: i64, carpeta: &str, forzar: bool) -> Result<usize> {
    anyhow::ensure!(proyecto != 0, tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    let n = match por_ruta(db, carpeta)? {
        Some((otro, _)) if otro != proyecto => fundir(db, otro, proyecto, forzar)?,
        _ => 0,
    };
    db.execute(
        "INSERT OR REPLACE INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![carpeta, comparable(carpeta), proyecto, origen(carpeta), ahora(db)?],
    )?;
    db.execute("UPDATE project SET path = ?2, name = ?3 WHERE id = ?1", params![proyecto, carpeta, nombre_de(carpeta, "")])?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raiz_git_desde_una_subcarpeta() {
        let base = std::env::temp_dir().join(format!("omoi-proyecto-{}", std::process::id()));
        let sub = base.join("repo").join("src").join("dentro");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo").join(".git")).unwrap();
        let desde_sub = de(&sub).unwrap();
        let desde_raiz = de(&base.join("repo")).unwrap();
        assert_eq!(desde_sub, desde_raiz);
        assert!(desde_raiz.ends_with("repo"));
        // Sin git, la propia carpeta.
        let suelta = base.join("suelta");
        std::fs::create_dir_all(&suelta).unwrap();
        assert!(de(&suelta).unwrap().ends_with("suelta"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn carpeta_general() {
        let base = std::env::temp_dir().join(format!("omoi-general-{}", std::process::id()));
        let home = base.join("usuario");
        std::fs::create_dir_all(home.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(home.join("suelta")).unwrap();
        let t = |p: &Path| de(p).unwrap();
        let home_c = PathBuf::from(t(&home));
        assert!(general_en(&t(&home), &home_c));
        assert!(general_en(&t(&base), &home_c), "una carpeta por encima del usuario también");
        assert!(!general_en(&t(&home.join("repo")), &home_c));
        assert!(!general_en(&t(&home.join("suelta")), &home_c), "una carpeta suelta sigue siendo su proyecto");
        assert!(!general_en("", &home_c));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn repo_de_una_ruta() {
        let base = std::env::temp_dir().join(format!("omoi-rutas-{}", std::process::id()));
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let home = PathBuf::from(de(&base).unwrap());
        let esperado = de(&repo).unwrap();
        let r = |s: &str| repo_de_ruta(s, &home);
        let dentro = repo.join("src").join("no-existe-aun.rs");
        assert_eq!(r(&format!("{}.", dentro.display())), Some(esperado.clone()));
        assert_eq!(r(&repo.display().to_string()), Some(esperado.clone()));
        assert_eq!(r("~/web/src"), Some(esperado));
        assert_eq!(r(&base.display().to_string()), None, "fuera de un repositorio");
        assert_eq!(r("src/main.rs"), None, "las relativas no dicen el proyecto");
        assert_eq!(r(&format!("{}?", repo.display())), Some(de(&repo).unwrap()));
        let clon = base.join(".claude").join("plugins").join("x");
        std::fs::create_dir_all(clon.join(".git")).unwrap();
        assert_eq!(r(&clon.display().to_string()), None, "un clon en una carpeta oculta no es un proyecto");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rutas_con_espacios() {
        let base = std::env::temp_dir().join(format!("omoi-espacios-{}", std::process::id()));
        let repo = base.join("the codex");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("app")).unwrap();
        let fichero = repo.join("app").join("x.kt");
        let texto = format!("El fallo está en `{}` y en {}. Lo vi ayer.", fichero.display(), repo.join("app").display());
        let v = rutas_en(&texto);
        assert!(v.contains(&fichero.display().to_string()), "{v:?}");
        assert!(v.contains(&repo.join("app").display().to_string()), "{v:?}");
        let home = PathBuf::from(de(&base).unwrap());
        let esperado = de(&repo).unwrap();
        assert!(v.iter().all(|r| repo_de_ruta(r, &home) == Some(esperado.clone())), "{v:?}");
        // Sin el repositorio, la palabra suelta no basta.
        assert_eq!(repo_de_ruta(&base.join("the").display().to_string(), &home), None);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rutas_relativas_al_repo() {
        let base = std::env::temp_dir().join(format!("omoi-relativas-{}", std::process::id()));
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(base.join("fuera")).unwrap();
        let home = PathBuf::from(de(&base).unwrap());
        let raiz = de(&repo).unwrap();
        let r = |s: &Path| relativa_a(&s.display().to_string(), &raiz, &home);
        assert_eq!(r(&repo.join("src").join("pages").join("index.astro")), "src/pages/index.astro", "aunque aún no exista");
        assert_eq!(r(&repo.join("src")), "src");
        let fuera = base.join("fuera").join("a.txt").display().to_string();
        assert_eq!(relativa_a(&fuera, &raiz, &home), fuera, "fuera del repo, tal cual");
        assert_eq!(relativa_a("src/main.rs", &raiz, &home), "src/main.rs");
        assert_eq!(relativa_a("~/web/src/a.rs", &raiz, &home), "src/a.rs");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn worktree_con_fichero_git() {
        // En un worktree, `.git` es un fichero, no una carpeta.
        let base = std::env::temp_dir().join(format!("omoi-worktree-{}", std::process::id()));
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join(".git"), "gitdir: /otro/sitio").unwrap();
        assert_eq!(de(&base.join("a")).unwrap(), de(&base).unwrap());
        let _ = std::fs::remove_dir_all(base);
    }
}
