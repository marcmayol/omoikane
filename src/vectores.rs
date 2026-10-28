//! Vectores de las memorias: guardarlos, tenerlos al día con el modelo y buscar los más parecidos.
//!
//! Van en `memory_vec` como BLOB (256 `f32` en little-endian, normalizados) y se comparan con
//! `vec_distance_cosine` de sqlite-vec, con el filtro de proyecto en el mismo SQL.

use crate::embeber::Modelo;
use crate::memoria::{COLUMNAS, Memoria, Tipo, de_fila};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

/// Similitud mínima para que una memoria que solo sale por significado (no por palabras) entre en
/// una búsqueda. Calibrada en la fase 2 con `tests/busquedas.tsv` (`examples/fase2.rs busqueda`):
/// con model2vec cualquier texto se parece algo a cualquier otro, sobre todo en el mismo idioma, y
/// por debajo de ~0,28 las consultas sin respuesta devuelven ruido. Con 0,28: 88 % de acierto en el
/// primer puesto (85 % solo con palabras) y 1 de 5 consultas sin respuesta con un resultado de más.
pub const SIMILITUD_BUSQUEDA: f32 = 0.28;

/// Similitud a partir de la cual guardar propone actualizar la memoria existente. Con los 40 pares
/// de `tests/pares_duplicados.tsv`: 0 falsos positivos y el 80 % de los duplicados detectados
/// (los temas parecidos llegan hasta 0,64; los duplicados tienen mediana 0,73).
pub const SIMILITUD_DUPLICADO: f32 = 0.65;

/// Lo que se convierte en vector: el título y el contenido.
pub fn texto(title: &str, content: &str) -> String {
    format!("{title}\n{content}")
}

fn bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn guardar(tx: &Transaction, n: i64, modelo: &Modelo, title: &str, content: &str) -> Result<()> {
    let v = modelo.vector(&texto(title, content))?;
    tx.execute("INSERT OR REPLACE INTO memory_vec(n, e) VALUES (?1, ?2)", params![n, bytes(&v)])?;
    Ok(())
}

/// Deja los vectores al día con `modelo`: si se calcularon con otro, los rehace todos; si faltan
/// (memorias guardadas sin modelo instalado), los añade. Devuelve cuántos calculó. Con miles de
/// memorias son milisegundos (un vector cuesta ~0,03 ms).
pub fn sincronizar(db: &mut Connection, modelo: &Modelo) -> Result<usize> {
    let guardado: Option<String> =
        db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |f| f.get(0)).optional()?;
    let faltan: i64 = db.query_row(
        "SELECT count(*) FROM memory m WHERE NOT EXISTS (SELECT 1 FROM memory_vec v WHERE v.n = m.n)",
        [],
        |f| f.get(0),
    )?;
    if guardado.as_deref() == Some(modelo.id()) && faltan == 0 {
        return Ok(0);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let guardado: Option<String> =
        tx.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |f| f.get(0)).optional()?;
    if guardado.as_deref() != Some(modelo.id()) {
        tx.execute("DELETE FROM memory_vec", [])?;
        tx.execute(
            "INSERT INTO meta(k, v) VALUES ('modelo', ?1) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            [modelo.id()],
        )?;
    }
    let pendientes: Vec<(i64, String, String)> = {
        let mut q = tx.prepare(
            "SELECT n, title, content FROM memory m WHERE NOT EXISTS (SELECT 1 FROM memory_vec v WHERE v.n = m.n)",
        )?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (n, t, c) in &pendientes {
        guardar(&tx, *n, modelo, t, c)?;
    }
    tx.commit()?;
    Ok(pendientes.len())
}

/// Memorias más parecidas a `texto`, de la más a la menos, con su similitud (coseno). `proyecto`
/// como en `Ambito`: `Some(p)` = las de `p` más las globales; `None` = todas.
pub fn parecidas(
    db: &Connection,
    modelo: &Modelo,
    texto: &str,
    proyecto: Option<&str>,
    kind: Option<Tipo>,
    minimo: f32,
    limite: usize,
) -> Result<Vec<(Memoria, f32)>> {
    let v = bytes(&modelo.vector(texto)?);
    let columnas = COLUMNAS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ");
    let mut q = db.prepare(&format!(
        "SELECT {columnas}, 1.0 - vec_distance_cosine(v.e, ?1) AS sim
         FROM memory_vec v JOIN memory m ON m.n = v.n
         WHERE (?2 IS NULL OR m.project = ?2 OR m.project = '') AND (?3 IS NULL OR m.kind = ?3)
         ORDER BY vec_distance_cosine(v.e, ?1) LIMIT ?4"
    ))?;
    let ncol = COLUMNAS.split(", ").count();
    let filas = q.query_map(params![v, proyecto, kind.map(Tipo::as_str), limite as i64], |f| {
        Ok((de_fila(f)?, f.get::<_, f64>(ncol)? as f32))
    })?;
    let mut res = Vec::new();
    for f in filas {
        let (m, s) = f?;
        if s >= minimo {
            res.push((m, s));
        }
    }
    Ok(res)
}

/// Si ya hay una memoria casi igual en el mismo ámbito (el proyecto y las globales, o solo las
/// globales si la nueva es global), la devuelve con su similitud.
pub fn duplicada(db: &Connection, modelo: &Modelo, project: &str, title: &str, content: &str) -> Result<Option<(Memoria, f32)>> {
    let mut v = parecidas(db, modelo, &texto(title, content), Some(project), None, SIMILITUD_DUPLICADO, 1)?;
    Ok(v.pop())
}

/// Para `doctor`: con qué modelo están calculados los vectores y cuántos hay.
pub fn estado(db: &Connection) -> Result<(Option<String>, i64)> {
    let modelo = db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |f| f.get(0)).optional()?;
    let n = db.query_row("SELECT count(*) FROM memory_vec", [], |f| f.get(0))?;
    Ok((modelo, n))
}

