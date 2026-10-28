//! Vectores de las memorias: guardarlos, tenerlos al día con el modelo y buscar los más parecidos.
//!
//! Van en `memory_vec` como BLOB (256 `f32` en little-endian, normalizados). Para comparar se
//! **centran**: se les resta el vector medio de las memorias de la base. Los modelos estáticos
//! comparten una dirección común que hace que todo se parezca a todo, sobre todo con textos largos
//! (con 127 notas reales, el 31 % de los pares de notas distintas pasaba de 0,65 sin centrar; 4 de
//! 8.001 centrando). La media se calcula al comparar, así que siempre es la de la base actual; con
//! pocas memorias se mezcla con una media genérica para que no salga cero.

use crate::embeber::Modelo;
use crate::memoria::{COLUMNAS, Memoria, Tipo, de_fila};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

/// Similitud mínima (vectores centrados) para que la búsqueda añada una memoria que no salió por
/// palabras. Calibrada en la fase 2 con `tests/busquedas.tsv` y con 127 notas reales: desde 0,25
/// las consultas sin respuesta no devuelven nada. Con un modelo estático el rescate rara vez
/// aporta: está para cuando la similitud es alta de verdad (ver PLAN.md §5).
pub const SIMILITUD_BUSQUEDA: f32 = 0.25;

/// Cuándo guardar propone actualizar la memoria que ya existe: si el parecido (vectores centrados)
/// llega a `SIMILITUD_DUPLICADO`, o si llega a `SIMILITUD_DUPLICADO_CON_PALABRAS` y las palabras de la
/// nueva están en la existente (`COBERTURA_DUPLICADO` de su peso IDF, como en el recall). El vector
/// solo no separaba: con 0,70, el 3,9 % de 127 notas reales distintas avisaba en falso y solo se
/// detectaba el 66 % de esas notas guardadas otra vez como lo haría un agente (otro título, el
/// principio del texto). Con las dos señales: 0 % de avisos en falso (notas y frases cortas) y 89 %
/// detectadas (`examples/fase2.rs dupcob`). Lo que se pierde: paráfrasis cortas con otras palabras.
pub const SIMILITUD_DUPLICADO: f32 = 0.78;
pub const SIMILITUD_DUPLICADO_CON_PALABRAS: f32 = 0.40;
pub const COBERTURA_DUPLICADO: f64 = 0.60;

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

/// Cuánto pesa la media genérica frente a las memorias de la base (como si fueran tantas memorias).
const PESO_MEDIA_GENERICA: f32 = 20.0;

fn normalizado(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

/// Producto escalar de un vector guardado (bytes little-endian) con otro, sin copiarlo.
fn punto(b: &[u8], v: &[f32]) -> f32 {
    b.as_chunks::<4>().0.iter().zip(v).map(|(x, y)| f32::from_le_bytes(*x) * y).sum()
}

/// Media de un corpus genérico (el de los tests: código, documentación y frases en muchos idiomas)
/// con este modelo. Se calcula una vez por modelo (~50 ms) y se guarda en `meta`.
fn media_generica(db: &Connection, modelo: &Modelo) -> Result<Vec<f32>> {
    let clave = format!("media:{}", modelo.id());
    if let Some(t) = db.query_row("SELECT v FROM meta WHERE k = ?1", [&clave], |f| f.get::<_, String>(0)).optional()? {
        return Ok(serde_json::from_str(&t)?);
    }
    let lineas: Vec<&str> = include_str!("../tests/corpus.txt").lines().filter(|l| !l.starts_with('#')).collect();
    let mut media = vec![0f32; modelo.dim()];
    for l in &lineas {
        for (m, x) in media.iter_mut().zip(modelo.vector(l)?) {
            *m += x / lineas.len() as f32;
        }
    }
    db.execute("INSERT OR IGNORE INTO meta(k, v) VALUES (?1, ?2)", params![clave, serde_json::to_string(&media)?])?;
    Ok(media)
}

/// La media con la que se centra: la de las memorias de la base, con la genérica como si fuera
/// `PESO_MEDIA_GENERICA` memorias más.
fn media(db: &Connection, modelo: &Modelo) -> Result<Vec<f32>> {
    let mut suma: Vec<f32> = media_generica(db, modelo)?.iter().map(|x| x * PESO_MEDIA_GENERICA).collect();
    let mut n = PESO_MEDIA_GENERICA;
    let mut q = db.prepare("SELECT e FROM memory_vec")?;
    let mut filas = q.query([])?;
    while let Some(f) = filas.next()? {
        for (s, x) in suma.iter_mut().zip(f.get_ref(0)?.as_blob()?.as_chunks::<4>().0) {
            *s += f32::from_le_bytes(*x);
        }
        n += 1.0;
    }
    Ok(suma.iter().map(|s| s / n).collect())
}

/// Memorias más parecidas a `texto`, de la más a la menos, con su similitud (coseno de los vectores
/// centrados). `proyecto` como en `Ambito`: `Some(p)` = las de `p` más las globales; `None` = todas.
pub fn parecidas(
    db: &Connection,
    modelo: &Modelo,
    texto: &str,
    proyecto: Option<&str>,
    kind: Option<Tipo>,
    minimo: f32,
    limite: usize,
) -> Result<Vec<(Memoria, f32)>> {
    let media = media(db, modelo)?;
    let consulta = normalizado(modelo.vector(texto)?.iter().zip(&media).map(|(x, m)| x - m).collect());
    // Con `a` normalizado: cos(a - μ, q) = (a·q - μ·q) / |a - μ|, y |a - μ|² = 1 - 2·a·μ + μ·μ.
    // Así no hace falta centrar (ni copiar) cada vector guardado: bastan dos productos escalares.
    let mq: f32 = media.iter().zip(&consulta).map(|(a, b)| a * b).sum();
    let mm: f32 = media.iter().map(|x| x * x).sum();
    let mut sims: Vec<(i64, f32)> = Vec::new();
    {
        let mut q = db.prepare(
            "SELECT v.n, v.e FROM memory_vec v JOIN memory m ON m.n = v.n
             WHERE (?1 IS NULL OR m.project = ?1 OR m.project = '') AND (?2 IS NULL OR m.kind = ?2)",
        )?;
        let mut filas = q.query(params![proyecto, kind.map(Tipo::as_str)])?;
        while let Some(f) = filas.next()? {
            let e = f.get_ref(1)?.as_blob()?;
            let norma = (1.0 - 2.0 * punto(e, &media) + mm).max(1e-12).sqrt();
            let s = (punto(e, &consulta) - mq) / norma;
            if s >= minimo {
                sims.push((f.get(0)?, s));
            }
        }
    }
    sims.sort_by(|a, b| b.1.total_cmp(&a.1));
    sims.truncate(limite);
    // Solo se leen enteras las que se devuelven.
    let mut q = db.prepare(&format!("SELECT {COLUMNAS} FROM memory WHERE n = ?1"))?;
    sims.into_iter().map(|(n, s)| Ok((q.query_row([n], de_fila)?, s))).collect()
}

/// Si ya hay una memoria casi igual en el mismo ámbito (el proyecto y las globales, o solo las
/// globales si la nueva es global), la devuelve con su similitud.
pub fn duplicada(db: &Connection, modelo: &Modelo, project: &str, title: &str, content: &str) -> Result<Option<(Memoria, f32)>> {
    let nueva = texto(title, content);
    let mut v = parecidas(db, modelo, &nueva, Some(project), None, SIMILITUD_DUPLICADO_CON_PALABRAS, 5)?;
    if v.first().is_some_and(|x| x.1 >= SIMILITUD_DUPLICADO) {
        return Ok(Some(v.swap_remove(0)));
    }
    if v.is_empty() {
        return Ok(None);
    }
    let coberturas = crate::recall::cobertura(db, project, &nueva, v.iter().map(|x| &x.0))?;
    let mejor = coberturas.iter().enumerate().filter(|(_, c)| **c >= COBERTURA_DUPLICADO).max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i);
    Ok(mejor.map(|i| v.swap_remove(i)))
}

/// Memorias que pueden tratar de lo mismo que una recién guardada sin llegar a duplicadas: las que
/// mejor puntúan por BM25 con cualquier palabra del título y comparten al menos dos palabras de
/// título con ella (o la única, si los dos títulos tienen una). No bloquea nada: se devuelven para
/// que el agente decida si fusionarlas. Así cae también el 10 % de duplicadas que `duplicada` deja
/// pasar, sin avisar en falso de entrada.
pub fn relacionadas(db: &Connection, project: &str, title: &str, excluir: &str, limite: usize) -> Result<Vec<Memoria>> {
    let Some(consulta) = crate::buscar::consulta_fts(title) else { return Ok(Vec::new()) };
    let propias: std::collections::BTreeSet<String> = crate::recall::utiles(title).into_iter().collect();
    let mut q = db.prepare(&format!(
        "SELECT {} FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND (m.project = ?2 OR m.project = '') AND m.id != ?3
         ORDER BY bm25(memory_fts) LIMIT 30",
        COLUMNAS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ")
    ))?;
    let candidatas: Vec<Memoria> = q.query_map(params![consulta, project, excluir], de_fila)?.collect::<rusqlite::Result<_>>()?;
    // Probado también exigir un parecido mínimo por vector: cada punto de ruido menos costaba
    // duplicadas sin cubrir (con 0,15, del 100 % al 98 %), así que no se exige.
    Ok(candidatas
        .into_iter()
        .filter(|m| {
            let suyas: std::collections::BTreeSet<String> = crate::recall::utiles(&m.title).into_iter().collect();
            let comunes = propias.intersection(&suyas).count();
            comunes >= 2 || (comunes == 1 && propias.len() == 1 && suyas.len() == 1)
        })
        .take(limite)
        .collect())
}

/// Para `doctor`: con qué modelo están calculados los vectores y cuántos hay.
pub fn estado(db: &Connection) -> Result<(Option<String>, i64)> {
    let modelo = db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |f| f.get(0)).optional()?;
    let n = db.query_row("SELECT count(*) FROM memory_vec", [], |f| f.get(0))?;
    Ok((modelo, n))
}

