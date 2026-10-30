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
//! diario, no debe caducar cada día); `contradicts`, mientras no cambie ninguna de las dos. «Cambiar»
//! es cambiar su título, contenido o tipo, y se compara por reloj lógico (esquema 10): el sello de ese
//! cambio (`memory.content_hlc`) no es posterior al del veredicto (`verdict.hlc`). Antes se comparaban
//! `updated_at` y `at`, relojes de pared de equipos distintos: un equipo con la hora adelantada dejaba
//! sin valor los veredictos de los demás. Los sellos tienen ancho fijo, así que se comparan como texto.
//! De una
//! pareja manda el veredicto más nuevo (por su sello), así que uno nuevo no necesita borrar el
//! anterior, y dos equipos que juzgan la misma pareja acaban de acuerdo al sincronizar.
//!
//! Desde el esquema 9 (0.19.0) cada veredicto lleva su **motivo** (`reason`: el `reason` del
//! `update`, o el título de la memoria nueva en `remember`) y su **autor** (`author`: la persona de
//! la sincronización, `agent:<cliente>` si no hay, `user` desde la vista o la CLI). Salen donde sale
//! el veredicto: en la nota del recall y de `search`, en Revisar y en `omoi review`. Y las
//! sustituciones se encadenan: si A está sustituida por B y B por C, la nota de A lleva a C
//! (`ultima_de_cadena`).

use crate::memoria::AHORA;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

pub const SUSTITUIDA: &str = "superseded";
pub const CONTRADICE: &str = "contradicts";
pub const COMPATIBLE: &str = "compatible";
const DE_PAREJA: [&str; 3] = [SUSTITUIDA, CONTRADICE, COMPATIBLE];

/// Quien lo dice desde la vista web o la CLI («está bien así»).
pub const USUARIO: &str = "user";

/// Hasta dónde llega una cadena de sustituciones (un ciclo se corta antes por el camino).
const TOPE_SALTOS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Veredicto {
    pub memory_id: String,
    pub other_id: String,
    pub relation: String,
    pub at: String,
    pub hlc: String,
    /// Por qué (vacío en los de antes del esquema 9).
    pub reason: String,
    /// Quién: la persona de la sync, `agent:<cliente>`, `cli` o `user`.
    pub author: String,
}

/// La clave de una pareja, sin orden: (a, b) y (b, a) son la misma.
pub fn pareja(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.into(), b.into()) } else { (b.into(), a.into()) }
}

/// Quién firma un veredicto dado aquí: la persona de la sincronización si la hay y, si no, quien
/// llama: `mcp:claude-code` → `agent:claude-code`, `mcp` a secas → `agent`, y lo demás (`cli`,
/// `http`) tal cual.
pub fn autor(db: &Connection, origen: &str) -> String {
    let persona = crate::memoria::autor(db).unwrap_or_default();
    if !persona.is_empty() {
        return persona;
    }
    match origen.trim() {
        "" | "mcp" => "agent".into(),
        o => match o.strip_prefix("mcp:") {
            Some(cliente) => format!("agent:{cliente}"),
            None => o.to_string(),
        },
    }
}

/// Apunta un veredicto de este equipo. `other_id` vacío solo con `ok:<aviso>`. `motivo` y `autor`
/// pueden ir vacíos (un «está bien así» no necesita motivo).
pub fn poner(db: &Connection, memory_id: &str, other_id: &str, relation: &str, motivo: &str, autor: &str) -> Result<()> {
    let de_pareja = DE_PAREJA.contains(&relation);
    anyhow::ensure!(de_pareja || relation.starts_with("ok:"), "unknown verdict «{relation}»");
    let id = crate::memoria::resolver_id(db, memory_id)?;
    let otra = if other_id.is_empty() { String::new() } else { crate::memoria::resolver_id(db, other_id)? };
    anyhow::ensure!(de_pareja != otra.is_empty(), "«{relation}» needs {}", if de_pareja { "another memory" } else { "no other memory" });
    anyhow::ensure!(id != otra, "a memory can't be judged against itself");
    // Posterior al último cambio de texto de las dos: aunque ese cambio venga de un equipo con la hora
    // muy adelantada (que no adelanta el reloj de aquí), el veredicto que se da ahora vale.
    let ultimo: String = db.query_row("SELECT COALESCE(max(content_hlc), '') FROM memory WHERE id IN (?1, ?2)", params![id, otra], |f| f.get(0))?;
    let hlc = crate::reloj::sello_tras(db, &ultimo)?;
    db.execute(
        &format!("INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc, reason, author) VALUES (?1, ?2, ?3, {AHORA}, ?4, ?5, ?6)"),
        params![id, otra, relation, hlc, motivo.trim(), autor.trim()],
    )?;
    Ok(())
}

fn de_fila(f: &rusqlite::Row) -> rusqlite::Result<Veredicto> {
    Ok(Veredicto { memory_id: f.get(0)?, other_id: f.get(1)?, relation: f.get(2)?, at: f.get(3)?, hlc: f.get(4)?, reason: f.get(5)?, author: f.get(6)? })
}

/// Todos los de memorias que existen, para la sincronización.
pub fn todos(db: &Connection) -> Result<Vec<Veredicto>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author FROM verdict v
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
        "INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc, reason, author) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author],
    )?;
    crate::reloj::recibir(db, &v.hlc)?;
    Ok(true)
}

/// Lo que vale de cada pareja: el veredicto más nuevo, si lo que juzgó no ha cambiado desde entonces.
pub fn parejas(db: &Connection) -> Result<HashMap<(String, String), Veredicto>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author, a.content_hlc, b.content_hlc FROM verdict v
         JOIN memory a ON a.id = v.memory_id JOIN memory b ON b.id = v.other_id
         WHERE v.other_id != ''",
    )?;
    let filas: Vec<(Veredicto, String, String)> = q.query_map([], |f| Ok((de_fila(f)?, f.get(7)?, f.get(8)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut r: HashMap<(String, String), Veredicto> = HashMap::new();
    let mut ultimo: HashMap<(String, String), String> = HashMap::new();
    for (v, cambio_a, cambio_b) in filas {
        let k = pareja(&v.memory_id, &v.other_id);
        // El más nuevo manda aunque ya no valga: un veredicto viejo no vuelve porque caduque el nuevo.
        if ultimo.get(&k).is_some_and(|h| !crate::reloj::mayor(&v.hlc, h)) {
            continue;
        }
        ultimo.insert(k.clone(), v.hlc.clone());
        let vale = cambio_a <= v.hlc && (v.relation != CONTRADICE || cambio_b <= v.hlc);
        if vale {
            r.insert(k, v);
        } else {
            r.remove(&k);
        }
    }
    Ok(r)
}

/// El final de una cadena de sustituciones: (id, saltos, motivo, autor) del último eslabón.
#[derive(Clone, Debug, PartialEq)]
pub struct Final {
    pub id: String,
    pub saltos: usize,
    pub reason: String,
    pub author: String,
}

/// Si `id` está sustituida por B y B por C, la última es C (con cuántos saltos y el motivo del último
/// eslabón). Solo cuenta un `superseded` vigente, con la misma regla que `parejas`: la juzgada no ha
/// cambiado después y no hay un veredicto más nuevo sobre la misma pareja (los sellos comparan bien
/// como texto: milisegundos y contador van a ancho fijo). Un ciclo se corta por el camino recorrido y
/// nada pasa de `TOPE_SALTOS`. `None` si no está sustituida.
pub fn cadena(db: &Connection, id: &str) -> Result<Option<Final>> {
    let r = db
        .query_row(
            "WITH RECURSIVE vigente AS (
               SELECT v.memory_id, v.other_id, v.reason, v.author FROM verdict v
               JOIN memory a ON a.id = v.memory_id JOIN memory b ON b.id = v.other_id
               WHERE v.relation = ?2 AND a.content_hlc <= v.hlc
                 AND NOT EXISTS (SELECT 1 FROM verdict w
                                 WHERE ((w.memory_id = v.memory_id AND w.other_id = v.other_id) OR (w.memory_id = v.other_id AND w.other_id = v.memory_id))
                                   AND w.hlc > v.hlc)
             ),
             paso(id, saltos, camino, reason, author) AS (
               SELECT ?1, 0, ',' || ?1 || ',', '', ''
               UNION
               SELECT g.other_id, p.saltos + 1, p.camino || g.other_id || ',', g.reason, g.author
               FROM paso p JOIN vigente g ON g.memory_id = p.id
               WHERE p.saltos < ?3 AND instr(p.camino, ',' || g.other_id || ',') = 0
             )
             SELECT id, saltos, reason, author FROM paso WHERE saltos > 0 ORDER BY saltos DESC, id LIMIT 1",
            params![id, SUSTITUIDA, TOPE_SALTOS as i64],
            |f| Ok(Final { id: f.get(0)?, saltos: f.get::<_, i64>(1)? as usize, reason: f.get(2)?, author: f.get(3)? }),
        )
        .optional()?;
    Ok(r)
}

/// La última memoria de la cadena de sustituciones de `id` y cuántos saltos hay hasta ella.
pub fn ultima_de_cadena(db: &Connection, id: &str) -> Result<Option<(String, usize)>> {
    Ok(cadena(db, id)?.map(|f| (f.id, f.saltos)))
}

/// Los `ok:<aviso>` de una sola memoria: (memoria, aviso) → cuándo. Con `vigentes`, solo los que
/// siguen valiendo (la memoria no ha cambiado después).
pub fn oks(db: &Connection, vigentes: bool) -> Result<HashMap<(String, String), String>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, substr(v.relation, 4), v.at FROM verdict v JOIN memory m ON m.id = v.memory_id
         WHERE v.other_id = '' AND v.relation LIKE 'ok:%' AND (?1 = 0 OR m.content_hlc <= v.hlc)",
    )?;
    let filas: Vec<(String, String, String)> = q.query_map([vigentes], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(filas.into_iter().map(|(id, aviso, at)| ((id, aviso), at)).collect())
}

/// El motivo como va en una nota: una línea, cortado a 120 caracteres.
pub fn motivo_corto(motivo: &str) -> String {
    let plano = motivo.split_whitespace().collect::<Vec<_>>().join(" ");
    if plano.chars().count() > 120 { format!("{}…", plano.chars().take(120).collect::<String>()) } else { plano }
}
