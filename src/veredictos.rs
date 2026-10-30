//! Veredictos (0.17.0): lo que el agente o el usuario dicen de una memoria o de una pareja, y que
//! Omoikane no puede saber solo.
//!
//! - `superseded`: `memory_id` queda sustituida por `other_id` (`remember`/`update` con `supersedes`).
//! - `contradicts`: las dos dicen cosas distintas y ninguna gana todavía (`contradicts`).
//! - `compatible`: las dos valen; quita los avisos de esa pareja (`compatible`, o «está bien así»
//!   en una pareja de Revisar).
//! - `ok:<aviso>`: «está bien así» en un aviso de una sola memoria (`unused`, `due`…).
//!
//! Un veredicto vale mientras no cambie lo que juzgó: `superseded`, `compatible` y `ok:*`, mientras
//! la memoria juzgada no cambie (un «está bien así» frente a una memoria de estado, que cambia a
//! diario, no debe caducar cada día); `contradicts`, mientras no cambie ninguna de las dos. De una
//! pareja manda el veredicto más nuevo (por su sello), así que uno nuevo no necesita borrar el
//! anterior, y dos equipos que juzgan la misma pareja acaban de acuerdo al sincronizar.

use crate::memoria::AHORA;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

pub const SUSTITUIDA: &str = "superseded";
pub const CONTRADICE: &str = "contradicts";
pub const COMPATIBLE: &str = "compatible";
const DE_PAREJA: [&str; 3] = [SUSTITUIDA, CONTRADICE, COMPATIBLE];

#[derive(Clone, Debug, PartialEq)]
pub struct Veredicto {
    pub memory_id: String,
    pub other_id: String,
    pub relation: String,
    pub at: String,
    pub hlc: String,
}

/// La clave de una pareja, sin orden: (a, b) y (b, a) son la misma.
pub fn pareja(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.into(), b.into()) } else { (b.into(), a.into()) }
}

/// Apunta un veredicto de este equipo. `other_id` vacío solo con `ok:<aviso>`.
pub fn poner(db: &Connection, memory_id: &str, other_id: &str, relation: &str) -> Result<()> {
    let de_pareja = DE_PAREJA.contains(&relation);
    anyhow::ensure!(de_pareja || relation.starts_with("ok:"), "unknown verdict «{relation}»");
    let id = crate::memoria::resolver_id(db, memory_id)?;
    let otra = if other_id.is_empty() { String::new() } else { crate::memoria::resolver_id(db, other_id)? };
    anyhow::ensure!(de_pareja != otra.is_empty(), "«{relation}» needs {}", if de_pareja { "another memory" } else { "no other memory" });
    anyhow::ensure!(id != otra, "a memory can't be judged against itself");
    let hlc = crate::reloj::sello(db)?;
    db.execute(
        &format!("INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc) VALUES (?1, ?2, ?3, {AHORA}, ?4)"),
        params![id, otra, relation, hlc],
    )?;
    Ok(())
}

fn de_fila(f: &rusqlite::Row) -> rusqlite::Result<Veredicto> {
    Ok(Veredicto { memory_id: f.get(0)?, other_id: f.get(1)?, relation: f.get(2)?, at: f.get(3)?, hlc: f.get(4)? })
}

/// Todos los de memorias que existen, para la sincronización.
pub fn todos(db: &Connection) -> Result<Vec<Veredicto>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc FROM verdict v
         WHERE EXISTS (SELECT 1 FROM memory m WHERE m.id = v.memory_id)
           AND (v.other_id = '' OR EXISTS (SELECT 1 FROM memory m WHERE m.id = v.other_id))
         ORDER BY v.memory_id, v.other_id, v.relation",
    )?;
    let v = q.query_map([], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Un veredicto que llega de otro equipo: se queda si es más nuevo que el que hay. Devuelve si entró.
pub fn recibir(db: &Connection, v: &Veredicto) -> Result<bool> {
    let local: Option<String> = db
        .query_row(
            "SELECT hlc FROM verdict WHERE memory_id = ?1 AND other_id = ?2 AND relation = ?3",
            params![v.memory_id, v.other_id, v.relation],
            |f| f.get(0),
        )
        .optional()?;
    if local.as_deref().is_some_and(|h| !crate::reloj::mayor(&v.hlc, h)) {
        return Ok(false);
    }
    db.execute(
        "INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![v.memory_id, v.other_id, v.relation, v.at, v.hlc],
    )?;
    crate::reloj::recibir(db, &v.hlc)?;
    Ok(true)
}

/// Lo que vale de cada pareja: el veredicto más nuevo, si lo que juzgó no ha cambiado desde entonces.
pub fn parejas(db: &Connection) -> Result<HashMap<(String, String), Veredicto>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc, a.updated_at, b.updated_at FROM verdict v
         JOIN memory a ON a.id = v.memory_id JOIN memory b ON b.id = v.other_id
         WHERE v.other_id != ''",
    )?;
    let filas: Vec<(Veredicto, String, String)> = q.query_map([], |f| Ok((de_fila(f)?, f.get(5)?, f.get(6)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut r: HashMap<(String, String), Veredicto> = HashMap::new();
    let mut ultimo: HashMap<(String, String), String> = HashMap::new();
    for (v, cambio_a, cambio_b) in filas {
        let k = pareja(&v.memory_id, &v.other_id);
        // El más nuevo manda aunque ya no valga: un veredicto viejo no vuelve porque caduque el nuevo.
        if ultimo.get(&k).is_some_and(|h| !crate::reloj::mayor(&v.hlc, h)) {
            continue;
        }
        ultimo.insert(k.clone(), v.hlc.clone());
        let vale = cambio_a <= v.at && (v.relation != CONTRADICE || cambio_b <= v.at);
        if vale {
            r.insert(k, v);
        } else {
            r.remove(&k);
        }
    }
    Ok(r)
}

/// Los `ok:<aviso>` de una sola memoria: (memoria, aviso) → cuándo. Con `vigentes`, solo los que
/// siguen valiendo (la memoria no ha cambiado después).
pub fn oks(db: &Connection, vigentes: bool) -> Result<HashMap<(String, String), String>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, substr(v.relation, 4), v.at FROM verdict v JOIN memory m ON m.id = v.memory_id
         WHERE v.other_id = '' AND v.relation LIKE 'ok:%' AND (?1 = 0 OR m.updated_at <= v.at)",
    )?;
    let filas: Vec<(String, String, String)> = q.query_map([vigentes], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(filas.into_iter().map(|(id, aviso, at)| ((id, aviso), at)).collect())
}
