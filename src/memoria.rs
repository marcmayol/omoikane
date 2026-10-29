//! Memorias: guardar, leer, cambiar (guardando la versión anterior), olvidar y listar.
//!
//! Toda escritura toca `memory`, `memory_fts` y `memory_path` en una sola transacción `IMMEDIATE`:
//! nunca queda una memoria sin indexar ni un índice que apunte a una memoria borrada.

use crate::embeber::Modelo;
use crate::{tr, vectores};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::str::FromStr;

/// Cada tipo cambia cómo se recupera la memoria (ver PLAN.md §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tipo {
    Fact,
    Decision,
    Gotcha,
    Preference,
}

impl Tipo {
    pub const TODOS: [Tipo; 4] = [Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Preference];

    pub fn as_str(self) -> &'static str {
        match self {
            Tipo::Fact => "fact",
            Tipo::Decision => "decision",
            Tipo::Gotcha => "gotcha",
            Tipo::Preference => "preference",
        }
    }
}

impl FromStr for Tipo {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Tipo> {
        Tipo::TODOS
            .into_iter()
            .find(|t| t.as_str() == s.trim().to_lowercase())
            .ok_or_else(|| anyhow!(tr!("unknown kind «{s}»: use fact, decision, gotcha or preference", "tipo «{s}» desconocido: usa fact, decision, gotcha o preference")))
    }
}

impl std::fmt::Display for Tipo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Memoria {
    pub id: String,
    /// Raíz del proyecto; vacío = global.
    pub project: String,
    pub kind: Tipo,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    pub recalled_at: Option<String>,
    pub recall_count: i64,
    /// Quién la escribió (el nombre de git), si la sincronización lo apunta.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
}

/// Lo que decide quien guarda. Lo demás (id, fechas, estadísticas) lo pone Omoikane.
#[derive(Clone, Debug, Default)]
pub struct Nueva {
    pub project: String,
    pub kind: Option<Tipo>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
}

/// Solo cambia lo que viene en `Some`.
#[derive(Clone, Debug, Default)]
pub struct Cambios {
    pub title: Option<String>,
    pub content: Option<String>,
    pub kind: Option<Tipo>,
    pub paths: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    /// Llevarla a otro proyecto (`""` = global). Como fijar, no crea versión.
    pub project: Option<String>,
    /// Por qué cambia: se guarda con la versión anterior.
    pub reason: Option<String>,
}

/// Una versión anterior de una memoria.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub version: i64,
    pub kind: String,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    /// Cuándo dejó de ser la versión viva.
    pub changed_at: String,
    pub reason: Option<String>,
}

/// Qué memorias se ven: las del proyecto más las globales, o todas.
#[derive(Clone, Debug)]
pub enum Ambito {
    Proyecto(String),
    Todos,
}

impl Ambito {
    /// Lo que se ve desde un proyecto: él y las globales; desde una carpeta general (la del
    /// usuario, ver `proyecto::es_general`), todo.
    pub fn de(project: &str) -> Ambito {
        if crate::proyecto::es_general(project) { Ambito::Todos } else { Ambito::Proyecto(project.to_string()) }
    }


    /// Valor para el parámetro `?` de `(? IS NULL OR project = ? OR project = '')`.
    pub(crate) fn filtro(&self) -> Option<&str> {
        match self {
            Ambito::Proyecto(p) => Some(p),
            Ambito::Todos => None,
        }
    }
}

pub(crate) const AHORA: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
pub(crate) const COLUMNAS: &str =
    "id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at, recalled_at, recall_count, author";

pub(crate) fn de_fila(f: &Row) -> rusqlite::Result<Memoria> {
    let lista = |i: usize| -> rusqlite::Result<Vec<String>> {
        let s: String = f.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let kind: String = f.get(2)?;
    Ok(Memoria {
        id: f.get(0)?,
        project: f.get(1)?,
        kind: kind.parse().unwrap_or(Tipo::Fact),
        title: f.get(3)?,
        content: f.get(4)?,
        paths: lista(5)?,
        tags: lista(6)?,
        pinned: f.get(7)?,
        source: f.get(8)?,
        created_at: f.get(9)?,
        updated_at: f.get(10)?,
        recalled_at: f.get(11)?,
        recall_count: f.get(12)?,
        author: f.get(13)?,
    })
}

fn escribir_tx(db: &mut Connection) -> Result<Transaction<'_>> {
    Ok(db.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

// --- Normalizar lo que llega -----------------------------------------------------------------------

fn titulo(t: &str) -> Result<String> {
    // Una línea: los saltos se vuelven espacios.
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!t.is_empty(), tr!("the title can't be empty", "el título no puede estar vacío"));
    ensure!(t.chars().count() <= 200, tr!("the title is over 200 characters: put the details in the content", "el título pasa de 200 caracteres: lo largo va en el contenido"));
    Ok(t)
}

fn contenido(c: &str) -> Result<String> {
    let c = c.trim().to_string();
    ensure!(!c.is_empty(), tr!("the content can't be empty", "el contenido no puede estar vacío"));
    Ok(c)
}

/// Rutas relativas a la raíz del proyecto, con `/` y sin `./` delante.
fn rutas(ps: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ps
        .iter()
        .map(|p| p.trim().replace('\\', "/"))
        .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn etiquetas(ts: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ts
        .iter()
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

// --- Índices ---------------------------------------------------------------------------------------

fn indexar(tx: &Transaction, n: i64, m: &Memoria) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
    )?;
    tx.execute("DELETE FROM memory_path WHERE memory_id = ?1", [&m.id])?;
    for p in &m.paths {
        tx.execute("INSERT INTO memory_path(memory_id, path) VALUES (?1, ?2)", params![m.id, p])?;
    }
    Ok(())
}

fn desindexar(tx: &Transaction, n: i64) -> Result<()> {
    tx.execute("DELETE FROM memory_fts WHERE rowid = ?1", [n])?;
    Ok(())
}

// --- Operaciones -----------------------------------------------------------------------------------

/// Guarda sin mirar si ya existe una parecida: eso es `vectores::duplicada`, antes, para que quien
/// llama decida. Con `modelo`, el vector va en la misma transacción.
pub fn guardar(db: &mut Connection, nueva: Nueva, modelo: Option<&Modelo>) -> Result<Memoria> {
    insertar(db, &ulid::Ulid::generate().to_string(), nueva, None, modelo)
}

/// De dónde viene una memoria que llega por la sincronización: su id, fechas, sello y autor de origen.
pub(crate) struct Origen<'a> {
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub hlc: &'a str,
    pub author: &'a str,
}

/// Para la sincronización: la misma memoria, con el mismo id, fechas, sello y autor que en su equipo.
pub(crate) fn guardar_con_id(db: &mut Connection, id: &str, nueva: Nueva, o: &Origen, modelo: Option<&Modelo>) -> Result<Memoria> {
    insertar(db, id, nueva, Some(o), modelo)
}

/// El sello de una memoria (vacío si no existe).
fn hlc_de(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Apunta una marca de olvido, salvo que ya haya una posterior.
pub(crate) fn marcar_olvido(db: &Connection, id: &str, deleted_at: Option<&str>, hlc: &str) -> Result<()> {
    let actual: Option<String> = db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    if actual.is_none_or(|a| crate::reloj::mayor(hlc, &a)) {
        db.execute(
            &format!("INSERT OR REPLACE INTO tombstone(id, deleted_at, hlc) VALUES (?1, COALESCE(?2, {AHORA}), ?3)"),
            params![id, deleted_at, hlc],
        )?;
    }
    Ok(())
}

/// El autor que se apunta en lo que se guarda aquí (lo configura la sincronización; vacío = ninguno).
fn autor(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'autor'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

fn insertar(db: &mut Connection, id: &str, nueva: Nueva, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<Memoria> {
    let tx = escribir_tx(db)?;
    let m = insertar_en(&tx, id, nueva, origen, modelo)?;
    tx.commit()?;
    Ok(m)
}

/// `insertar` dentro de una transacción que ya está abierta (quien llama hace el commit).
fn insertar_en(tx: &Transaction, id: &str, nueva: Nueva, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<Memoria> {
    let kind = nueva.kind.with_context(|| tr!("missing kind: fact, decision, gotcha or preference", "falta el tipo: fact, decision, gotcha o preference"))?;
    let (title, content) = (titulo(&nueva.title)?, contenido(&nueva.content)?);
    let (paths, tags) = (rutas(&nueva.paths), etiquetas(&nueva.tags));
    let (hlc, author) = match origen {
        Some(o) => {
            crate::reloj::recibir(tx, o.hlc)?;
            (o.hlc.to_string(), o.author.to_string())
        }
        None => (crate::reloj::sello(tx)?, autor(tx)?),
    };
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at, hlc, author)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, COALESCE(?10, {AHORA}), COALESCE(?11, {AHORA}), ?12, ?13)"
        ),
        params![
            id,
            nueva.project,
            kind.as_str(),
            title,
            content,
            serde_json::to_string(&paths)?,
            serde_json::to_string(&tags)?,
            nueva.pinned,
            nueva.source,
            origen.map(|o| o.created_at),
            origen.map(|o| o.updated_at),
            hlc,
            author,
        ],
    )?;
    let n = tx.last_insert_rowid();
    let m = leer(tx, id)?;
    indexar(tx, n, &m)?;
    if let Some(modelo) = modelo {
        vectores::guardar(tx, n, modelo, &m.title, &m.content)?;
    }
    Ok(m)
}

fn leer(db: &Connection, id: &str) -> Result<Memoria> {
    db.query_row(&format!("SELECT {COLUMNAS} FROM memory WHERE id = ?1"), [id], de_fila)
        .optional()?
        .ok_or_else(|| anyhow!(tr!("no memory with id {id}", "no existe la memoria {id}")))
}

/// Id completo a partir del id o de su principio (lo que enseña la CLI basta).
pub fn resolver_id(db: &Connection, id: &str) -> Result<String> {
    let id = id.trim().to_uppercase();
    ensure!(id.len() >= 4, tr!("id too short: «{id}»", "id demasiado corto: «{id}»"));
    ensure!(id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let mut q = db.prepare("SELECT id FROM memory WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
    let ids: Vec<String> = q.query_map([&id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    match ids.as_slice() {
        [uno] => Ok(uno.clone()),
        [] => bail!(tr!("no memory with id {id}", "no existe ninguna memoria con id {id}")),
        _ => bail!(tr!("several memories start with {id}: type more characters", "hay varias memorias que empiezan por {id}: escribe más caracteres")),
    }
}

pub fn obtener(db: &Connection, id: &str) -> Result<Memoria> {
    leer(db, &resolver_id(db, id)?)
}

pub fn actualizar(db: &mut Connection, id: &str, c: Cambios, modelo: Option<&Modelo>) -> Result<Memoria> {
    actualizar_desde(db, id, c, None, modelo)
}

/// Para la sincronización: el cambio que llega de otro equipo, con sus fechas, sello y autor, todo en
/// la misma transacción (la versión de aquí queda en el historial).
pub(crate) fn actualizar_con_origen(db: &mut Connection, id: &str, c: Cambios, o: &Origen, modelo: Option<&Modelo>) -> Result<Memoria> {
    actualizar_desde(db, id, c, Some(o), modelo)
}

fn actualizar_desde(db: &mut Connection, id: &str, c: Cambios, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let antes = leer(&tx, &id)?;
    let hlc_antes = hlc_de(&tx, &id)?;
    // El sello del cambio: el de origen, o uno de aquí posterior al de la versión que se cambia.
    let hlc = match origen {
        Some(o) => {
            crate::reloj::recibir(&tx, o.hlc)?;
            o.hlc.to_string()
        }
        None => crate::reloj::sello_tras(&tx, &hlc_antes)?,
    };
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    let despues = Memoria {
        title: match &c.title {
            Some(t) => titulo(t)?,
            None => antes.title.clone(),
        },
        content: match &c.content {
            Some(t) => contenido(t)?,
            None => antes.content.clone(),
        },
        kind: c.kind.unwrap_or(antes.kind),
        paths: c.paths.as_deref().map(rutas).unwrap_or_else(|| antes.paths.clone()),
        tags: c.tags.as_deref().map(etiquetas).unwrap_or_else(|| antes.tags.clone()),
        pinned: c.pinned.unwrap_or(antes.pinned),
        project: c.project.clone().unwrap_or_else(|| antes.project.clone()),
        ..antes.clone()
    };
    let cambia_texto = despues.title != antes.title
        || despues.content != antes.content
        || despues.kind != antes.kind
        || despues.paths != antes.paths
        || despues.tags != antes.tags;
    if !cambia_texto {
        // Fijar o desfijar no es un cambio de contenido: sin versión nueva y sin tocar updated_at
        // (que da la recencia en la búsqueda).
        if despues.pinned != antes.pinned || despues.project != antes.project || origen.is_some() {
            tx.execute(
                "UPDATE memory SET pinned = ?2, project = ?3, hlc = ?4 WHERE id = ?1",
                params![id, despues.pinned, despues.project, hlc],
            )?;
            if let Some(o) = origen {
                tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
            }
            tx.commit()?;
        }
        return Ok(Memoria { pinned: despues.pinned, project: despues.project, ..antes });
    }
    {
        // La foto anterior va al historial.
        let version: i64 =
            tx.query_row("SELECT COALESCE(MAX(version), 0) + 1 FROM memory_version WHERE memory_id = ?1", [&id], |f| {
                f.get(0)
            })?;
        tx.execute(
            &format!(
                "INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {AHORA}, ?8)"
            ),
            params![
                id,
                version,
                antes.title,
                antes.content,
                antes.kind.as_str(),
                serde_json::to_string(&antes.paths)?,
                serde_json::to_string(&antes.tags)?,
                c.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()),
            ],
        )?;
    }
    tx.execute(
        &format!(
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, project = ?8,
             updated_at = {AHORA}, hlc = ?9, author = CASE WHEN author = '' THEN ?10 ELSE author END WHERE id = ?1"
        ),
        params![
            id,
            despues.title,
            despues.content,
            despues.kind.as_str(),
            serde_json::to_string(&despues.paths)?,
            serde_json::to_string(&despues.tags)?,
            despues.pinned,
            despues.project,
            hlc,
            autor(&tx)?,
        ],
    )?;
    if let Some(o) = origen {
        tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
    }
    desindexar(&tx, n)?;
    let m = leer(&tx, &id)?;
    indexar(&tx, n, &m)?;
    if despues.title != antes.title || despues.content != antes.content {
        match modelo {
            Some(modelo) => vectores::guardar(&tx, n, modelo, &m.title, &m.content)?,
            // Sin modelo, el vector viejo ya no vale: se borra y `sincronizar` lo rehará.
            None => {
                tx.execute("DELETE FROM memory_vec WHERE n = ?1", [n])?;
            }
        }
    }
    tx.commit()?;
    Ok(m)
}

/// Rehace el índice de palabras desde la tabla. Devuelve cuántas memorias indexó.
pub fn reindexar_palabras(db: &mut Connection) -> Result<usize> {
    let tx = escribir_tx(db)?;
    tx.execute("DELETE FROM memory_fts", [])?;
    let ids: Vec<(i64, String)> = {
        let mut q = tx.prepare("SELECT n, id FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let todas: Vec<(i64, Memoria)> = ids.into_iter().map(|(n, id)| Ok((n, leer(&tx, &id)?))).collect::<Result<_>>()?;
    for (n, m) in &todas {
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
        )?;
    }
    tx.commit()?;
    Ok(todas.len())
}

/// Borra la memoria con su historial y sus índices, y deja una marca para que la sincronización no la
/// devuelva desde otro equipo. Devuelve lo que se borró.
pub fn olvidar(db: &mut Connection, id: &str) -> Result<Memoria> {
    borrar(db, id, None)
}

/// Para la sincronización: olvidar con la marca (sello y fecha) que llega de otro equipo. La memoria se
/// guarda aparte por si hay que devolverla (`restaurar`).
pub(crate) fn olvidar_con(db: &mut Connection, id: &str, hlc: &str, deleted_at: &str, equipo: &str) -> Result<Memoria> {
    borrar(db, id, Some((hlc, deleted_at, equipo)))
}

/// Lo que otros equipos olvidaron y aquí se guardó: (id, título, quién, cuándo).
pub fn olvidadas_fuera(db: &Connection) -> Result<Vec<(String, String, String, String)>> {
    let mut q = db.prepare("SELECT id, memory, by, deleted_at FROM forgotten ORDER BY deleted_at DESC")?;
    let v = q
        .query_map([], |f| {
            let m: String = f.get(1)?;
            let titulo = serde_json::from_str::<serde_json::Value>(&m).ok().and_then(|v| v["title"].as_str().map(String::from)).unwrap_or_default();
            Ok((f.get(0)?, titulo, f.get(2)?, f.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Devuelve una memoria que olvidó otro equipo, con un sello posterior al olvido (así vuelve también a
/// los demás equipos en la próxima sincronización). Su historial anterior se conserva.
pub fn restaurar(db: &mut Connection, id: &str, modelo: Option<&Modelo>) -> Result<Memoria> {
    // El id como en las demás órdenes: en mayúsculas, y un principio que valga para varias es un error.
    let id = id.trim().to_uppercase();
    ensure!(!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let tx = escribir_tx(db)?;
    let filas: Vec<(String, String, String)> = {
        let mut q = tx.prepare("SELECT id, memory, versions FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
        q.query_map([&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let (id, m, versiones) = match <[_; 1]>::try_from(filas) {
        Ok([una]) => una,
        Err(v) if v.is_empty() => bail!(tr!("nothing forgotten elsewhere with id {id}", "no hay nada olvidado en otro equipo con id {id}")),
        Err(_) => bail!(tr!("several forgotten memories start with {id}: type more characters", "hay varias memorias olvidadas que empiezan por {id}: escribe más caracteres")),
    };
    let m: Memoria = serde_json::from_str(&m)?;
    let versiones: Vec<Version> = serde_json::from_str(&versiones).unwrap_or_default();
    let marca: String = tx.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?.unwrap_or_default();
    let hlc = crate::reloj::sello_tras(&tx, &marca)?;
    let nueva = Nueva { project: m.project.clone(), kind: Some(m.kind), title: m.title.clone(), content: m.content.clone(), paths: m.paths.clone(), tags: m.tags.clone(), pinned: m.pinned, source: m.source.clone() };
    let o = Origen { created_at: &m.created_at, updated_at: &m.updated_at, hlc: &hlc, author: &m.author };
    tx.execute("DELETE FROM tombstone WHERE id = ?1", [&id])?;
    let r = insertar_en(&tx, &id, nueva, Some(&o), modelo)?;
    for v in versiones {
        tx.execute(
            "INSERT OR IGNORE INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, v.version, v.title, v.content, v.kind, serde_json::to_string(&v.paths)?, serde_json::to_string(&v.tags)?, v.changed_at, v.reason],
        )?;
    }
    tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
    tx.commit()?;
    Ok(r)
}

fn borrar(db: &mut Connection, id: &str, marca: Option<(&str, &str, &str)>) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let (hlc, deleted_at): (String, Option<&str>) = match marca {
        Some((h, d, _)) => {
            crate::reloj::recibir(&tx, h)?;
            (h.to_string(), Some(d))
        }
        None => (crate::reloj::sello_tras(&tx, &hlc_de(&tx, &id)?)?, None),
    };
    marcar_olvido(&tx, &id, deleted_at, &hlc)?;
    let m = leer(&tx, &id)?;
    if let Some((_, _, equipo)) = marca {
        // Lo olvidó otro equipo (quizá otra persona): aquí queda una copia, con su historial, que
        // `omoi sync restore <id>` devuelve.
        let versiones = historial(&tx, &id)?;
        let hlc_m = hlc_de(&tx, &id)?;
        tx.execute(
            &format!("INSERT OR REPLACE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, COALESCE(?5, {AHORA}), ?6)"),
            params![id, serde_json::to_string(&m)?, serde_json::to_string(&versiones)?, hlc_m, deleted_at, equipo],
        )?;
    }
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    desindexar(&tx, n)?;
    // memory_version y memory_path caen con ON DELETE CASCADE.
    tx.execute("DELETE FROM memory WHERE id = ?1", [&id])?;
    tx.commit()?;
    Ok(m)
}

/// Versiones anteriores, de la más reciente a la más antigua.
pub fn historial(db: &Connection, id: &str) -> Result<Vec<Version>> {
    let id = resolver_id(db, id)?;
    let mut q = db.prepare(
        "SELECT version, kind, title, content, paths, tags, changed_at, reason
         FROM memory_version WHERE memory_id = ?1 ORDER BY version DESC",
    )?;
    let v = q
        .query_map([&id], |f| {
            let lista = |i: usize| -> rusqlite::Result<Vec<String>> {
                Ok(serde_json::from_str(&f.get::<_, String>(i)?).unwrap_or_default())
            };
            Ok(Version {
                version: f.get(0)?,
                kind: f.get(1)?,
                title: f.get(2)?,
                content: f.get(3)?,
                paths: lista(4)?,
                tags: lista(5)?,
                changed_at: f.get(6)?,
                reason: f.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Las últimas guardadas o cambiadas.
pub fn recientes(db: &Connection, ambito: &Ambito, kind: Option<Tipo>, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory
         WHERE (?1 IS NULL OR project = ?1 OR project = '') AND (?2 IS NULL OR kind = ?2)
         ORDER BY updated_at DESC, id DESC LIMIT ?3"
    ))?;
    let v = q
        .query_map(params![ambito.filtro(), kind.map(Tipo::as_str), limite as i64], de_fila)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Lo que el agente debería saber siempre al empezar en un proyecto: las preferencias y las fijadas,
/// del proyecto y globales. Primero las fijadas, luego las más recientes.
pub fn contexto(db: &Connection, project: &str, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory
         WHERE (project = ?1 OR project = '') AND (kind = 'preference' OR pinned = 1)
         ORDER BY pinned DESC, updated_at DESC, id DESC LIMIT ?2"
    ))?;
    let v = q.query_map(params![project, limite as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

#[derive(Clone, Debug, Serialize)]
pub struct Proyecto {
    /// Vacío = memorias globales.
    pub project: String,
    pub memories: i64,
    pub last_update: String,
}

pub fn proyectos(db: &Connection) -> Result<Vec<Proyecto>> {
    let mut q = db.prepare(
        "SELECT project, count(*), max(updated_at) FROM memory GROUP BY project ORDER BY max(updated_at) DESC",
    )?;
    let v = q
        .query_map([], |f| Ok(Proyecto { project: f.get(0)?, memories: f.get(1)?, last_update: f.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Una línea JSON por memoria, con su historial. Para no quedarse atrapado en Omoikane.
pub fn exportar(db: &Connection, ambito: &Ambito, salida: &mut impl Write) -> Result<usize> {
    #[derive(Serialize)]
    struct Linea<'a> {
        #[serde(flatten)]
        memoria: &'a Memoria,
        versions: Vec<Version>,
    }
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory WHERE (?1 IS NULL OR project = ?1 OR project = '') ORDER BY created_at, id"
    ))?;
    let todas: Vec<Memoria> = q.query_map([ambito.filtro()], de_fila)?.collect::<rusqlite::Result<_>>()?;
    for m in &todas {
        let linea = Linea { memoria: m, versions: historial(db, &m.id)? };
        serde_json::to_writer(&mut *salida, &linea)?;
        salida.write_all(b"\n")?;
    }
    Ok(todas.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_rutas_y_etiquetas() {
        assert_eq!(rutas(&["./src\\auth.rs".into(), "src/auth.rs".into(), " docs/ ".into(), "".into()]), [
            "docs",
            "src/auth.rs"
        ]);
        assert_eq!(etiquetas(&["#Auth".into(), "auth".into(), " Login ".into()]), ["auth", "login"]);
    }

    #[test]
    fn titulo_en_una_linea() {
        assert_eq!(titulo("  dos\nlíneas  ").unwrap(), "dos líneas");
        assert!(titulo("   ").is_err());
        assert!(titulo(&"x".repeat(201)).is_err());
    }

    #[test]
    fn tipos() {
        assert_eq!("Gotcha".parse::<Tipo>().unwrap(), Tipo::Gotcha);
        assert!("bugfix".parse::<Tipo>().is_err());
    }
}
