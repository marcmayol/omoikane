//! Memorias: guardar, leer, cambiar (guardando la versión anterior), olvidar y listar.
//!
//! Toda escritura toca `memory`, `memory_fts` y `memory_path` en una sola transacción `IMMEDIATE`:
//! nunca queda una memoria sin indexar ni un índice que apunte a una memoria borrada.

use crate::tr;
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

#[derive(Clone, Debug, Serialize)]
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
    /// Por qué cambia: se guarda con la versión anterior.
    pub reason: Option<String>,
}

/// Una versión anterior de una memoria.
#[derive(Clone, Debug, Serialize)]
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
    "id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at, recalled_at, recall_count";

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
        "INSERT INTO memory_fts(rowid, title, content, tags) VALUES (?1, ?2, ?3, ?4)",
        params![n, m.title, m.content, m.tags.join(" ")],
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

pub fn guardar(db: &mut Connection, nueva: Nueva) -> Result<Memoria> {
    let kind = nueva.kind.with_context(|| tr!("missing kind: fact, decision, gotcha or preference", "falta el tipo: fact, decision, gotcha o preference"))?;
    let (title, content) = (titulo(&nueva.title)?, contenido(&nueva.content)?);
    let (paths, tags) = (rutas(&nueva.paths), etiquetas(&nueva.tags));
    let id = ulid::Ulid::generate().to_string();
    let tx = escribir_tx(db)?;
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, {AHORA}, {AHORA})"
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
        ],
    )?;
    let n = tx.last_insert_rowid();
    let m = leer(&tx, &id)?;
    indexar(&tx, n, &m)?;
    tx.commit()?;
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

pub fn actualizar(db: &mut Connection, id: &str, c: Cambios) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let antes = leer(&tx, &id)?;
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
        if despues.pinned != antes.pinned {
            tx.execute("UPDATE memory SET pinned = ?2 WHERE id = ?1", params![id, despues.pinned])?;
            tx.commit()?;
        }
        return Ok(Memoria { pinned: despues.pinned, ..antes });
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
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, updated_at = {AHORA}
             WHERE id = ?1"
        ),
        params![
            id,
            despues.title,
            despues.content,
            despues.kind.as_str(),
            serde_json::to_string(&despues.paths)?,
            serde_json::to_string(&despues.tags)?,
            despues.pinned,
        ],
    )?;
    desindexar(&tx, n)?;
    let m = leer(&tx, &id)?;
    indexar(&tx, n, &m)?;
    tx.commit()?;
    Ok(m)
}

/// Borra la memoria con su historial y sus índices. Devuelve lo que se borró.
pub fn olvidar(db: &mut Connection, id: &str) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let m = leer(&tx, &id)?;
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
