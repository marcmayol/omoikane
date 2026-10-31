//! Vectores de las memorias: guardarlos, tenerlos al día con el modelo y buscar los más parecidos.
//!
//! Van en `memory_vec` como BLOB (256 `f32` en little-endian, normalizados). Para comparar se
//! **centran**: se les resta el vector medio de las memorias de la base. Los modelos estáticos
//! comparten una dirección común que hace que todo se parezca a todo, sobre todo con textos largos
//! (con 127 notas reales, el 31 % de los pares de notas distintas pasaba de 0,65 sin centrar; 4 de
//! 8.001 centrando). La media es siempre la de la base actual; con pocas memorias se mezcla con una
//! media genérica para que no salga cero.
//!
//! Leer los vectores enteros cuesta (con 100.000 memorias, 100 MB y 200 ms por consulta), así que se
//! guarda aparte un **resumen** (tablas locales, ver `db::LOCALES`): la suma de los vectores para la
//! media y una copia en int8 de cada uno (un cuarto de lo que ocupa) con lo que se aleja del de verdad.
//! Con ella se acota el parecido de cada memoria y solo se leen enteras las que pueden entrar: el
//! resultado es el mismo que comparando con todos los vectores. Lo tienen al día los disparadores
//! de `memory_vec` (apuntan qué cambia en `vec_cambio`) y `al_dia`, que lo aplica al usarlo.

use crate::embeber::Modelo;
use crate::memoria::{Ambito, COLUMNAS, Memoria, Tipo, de_fila};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

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
    if guardado.as_deref() == Some(modelo.id()) && faltan(db)? == 0 {
        return Ok(0);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let guardado: Option<String> =
        tx.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |f| f.get(0)).optional()?;
    if guardado.as_deref() != Some(modelo.id()) {
        // Antes de borrar: sin resumen, los disparadores no apuntan cada vector viejo en `vec_cambio`.
        if hay_locales(&tx)? {
            olvidar_resumen(&tx)?;
        }
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

/// La misma media, con la suma del resumen (en `f64`: no depende del orden en que se sumó).
fn media_de(db: &Connection, modelo: &Modelo, r: &Resumen) -> Result<Vec<f32>> {
    let generica = media_generica(db, modelo)?;
    let n = f64::from(PESO_MEDIA_GENERICA) + r.cuantos as f64;
    Ok(generica.iter().zip(&r.suma).map(|(g, s)| ((f64::from(*g) * f64::from(PESO_MEDIA_GENERICA) + s) / n) as f32).collect())
}

// --- El resumen -----------------------------------------------------------------------------------

/// Dónde va en `meta` el estado del resumen.
const CLAVE_RESUMEN: &str = "vec_resumen";
/// Cuántos `n` seguidos van en cada fila de `vec_bloque` (`b = n >> BITS_BLOQUE`).
const BITS_BLOQUE: u32 = 10;
/// Margen que se añade a cada cota por los redondeos de `f32` (los de verdad son del orden de 1e-5).
const HOLGURA: f32 = 1e-3;

/// La parte del resumen que va en `meta`: la dimensión de los vectores, cuántos hay y su suma.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Resumen {
    dim: usize,
    cuantos: i64,
    suma: Vec<f64>,
}

/// Lo que ocupa cada vector en un bloque: escala (`f32`), lo que se aleja del de verdad (`f32`, la
/// norma de la diferencia, redondeada hacia arriba) y un `i8` por dimensión.
fn tam_entrada(dim: usize) -> usize {
    8 + dim
}

fn a_f32(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect()
}

/// Un vector en int8: `e ≈ escala · c`, y `|e - escala · c|`, que acota el error de cualquier producto
/// escalar con él (Cauchy-Schwarz: como mucho eso por la norma del otro vector).
fn cuantizar(e: &[f32], sale: &mut Vec<u8>) {
    let maximo = e.iter().fold(0f32, |m, x| m.max(x.abs()));
    let escala = if maximo > 0.0 { maximo / 127.0 } else { 0.0 };
    let mut codigos = Vec::with_capacity(e.len());
    let mut resto = 0f64;
    for x in e {
        let c = if escala > 0.0 { (x / escala).round().clamp(-127.0, 127.0) } else { 0.0 };
        resto += (f64::from(*x) - f64::from(escala) * f64::from(c)).powi(2);
        codigos.push(c as i8 as u8);
    }
    let resto = (resto.sqrt() * (1.0 + 1e-6) + 1e-9) as f32;
    sale.extend(escala.to_le_bytes());
    sale.extend(resto.to_le_bytes());
    sale.extend(codigos);
}

/// Un bloque de `vec_bloque` en memoria: los `n` en orden y sus entradas.
struct Bloque {
    ns: Vec<i64>,
    datos: Vec<u8>,
}

impl Bloque {
    fn leer(ns: &[u8], datos: &[u8]) -> Bloque {
        Bloque { ns: ns.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)).collect(), datos: datos.to_vec() }
    }

    fn ns_bytes(&self) -> Vec<u8> {
        self.ns.iter().flat_map(|n| n.to_le_bytes()).collect()
    }

    /// Quita `n` (si está) y, con `e`, lo vuelve a poner en su sitio.
    fn poner(&mut self, n: i64, e: Option<&[f32]>, dim: usize) {
        let t = tam_entrada(dim);
        let i = self.ns.partition_point(|x| *x < n);
        if self.ns.get(i) == Some(&n) {
            self.ns.remove(i);
            self.datos.drain(i * t..(i + 1) * t);
        }
        if let Some(e) = e {
            let mut entrada = Vec::with_capacity(t);
            cuantizar(e, &mut entrada);
            self.ns.insert(i, n);
            self.datos.splice(i * t..i * t, entrada);
        }
    }
}

fn leer_resumen(db: &Connection) -> Result<Option<Resumen>> {
    let t: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [CLAVE_RESUMEN], |f| f.get(0)).optional()?;
    Ok(t.and_then(|t| serde_json::from_str(&t).ok()))
}

fn escribir_resumen(db: &Connection, r: &Resumen) -> Result<()> {
    db.execute(
        "INSERT INTO meta(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![CLAVE_RESUMEN, serde_json::to_string(r)?],
    )?;
    Ok(())
}

/// Borra el resumen: se rehará entero al usarlo. Lo llama `db::locales` al (re)crear sus tablas.
pub(crate) fn olvidar_resumen(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM meta WHERE k = ?1", [CLAVE_RESUMEN])?;
    db.execute_batch("DELETE FROM vec_bloque; DELETE FROM vec_cambio;")?;
    Ok(())
}

/// El resumen hecho de nuevo con todos los vectores. `None` si no tienen todos la misma dimensión
/// (a medias de cambiar de modelo): entonces se compara con todos, como sin resumen.
fn reconstruir(db: &Connection) -> Result<Option<Resumen>> {
    olvidar_resumen(db)?;
    let mut r: Option<Resumen> = None;
    let mut bloque: Option<(i64, Bloque)> = None;
    let mut poner = db.prepare("INSERT INTO vec_bloque(b, ns, datos) VALUES (?1, ?2, ?3)")?;
    let mut q = db.prepare("SELECT n, e FROM memory_vec ORDER BY n")?;
    let mut filas = q.query([])?;
    while let Some(f) = filas.next()? {
        let n: i64 = f.get(0)?;
        let e = a_f32(f.get_ref(1)?.as_blob()?);
        let r = r.get_or_insert_with(|| Resumen { dim: e.len(), cuantos: 0, suma: vec![0.0; e.len()] });
        if e.len() != r.dim || r.dim == 0 {
            olvidar_resumen(db)?;
            return Ok(None);
        }
        r.cuantos += 1;
        r.suma.iter_mut().zip(&e).for_each(|(s, x)| *s += f64::from(*x));
        let b = n >> BITS_BLOQUE;
        if let Some((actual, hecho)) = bloque.take_if(|(actual, _)| *actual != b) {
            poner.execute(params![actual, hecho.ns_bytes(), hecho.datos])?;
        }
        let (_, actual) = bloque.get_or_insert_with(|| (b, Bloque { ns: Vec::new(), datos: Vec::new() }));
        actual.ns.push(n);
        cuantizar(&e, &mut actual.datos);
    }
    if let Some((b, hecho)) = bloque {
        poner.execute(params![b, hecho.ns_bytes(), hecho.datos])?;
    }
    // Sin vectores también hay resumen: vacío, y la dimensión la pone el primero que llegue.
    let r = r.unwrap_or(Resumen { dim: 0, cuantos: 0, suma: Vec::new() });
    escribir_resumen(db, &r)?;
    Ok(Some(r))
}

/// Aplica al resumen lo apuntado en `vec_cambio`. `None` si sale más a cuenta rehacerlo (o no cuadra).
fn aplicar_cambios(db: &Connection, mut r: Resumen) -> Result<Option<Resumen>> {
    let cambios: Vec<(i64, Option<Vec<u8>>)> = {
        let mut q = db.prepare("SELECT n, viejo FROM vec_cambio ORDER BY n")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    // Muchos cambios (otro modelo, una importación grande): mejor rehacerlo.
    if cambios.len() as i64 > (r.cuantos / 4).max(1000) {
        return Ok(None);
    }
    let mut actual = db.prepare("SELECT e FROM memory_vec WHERE n = ?1")?;
    let mut leer_bloque = db.prepare("SELECT ns, datos FROM vec_bloque WHERE b = ?1")?;
    let mut bloques: std::collections::BTreeMap<i64, Bloque> = std::collections::BTreeMap::new();
    for (n, viejo) in cambios {
        if let Some(v) = viejo {
            let v = a_f32(&v);
            if v.len() != r.dim {
                return Ok(None);
            }
            r.cuantos -= 1;
            r.suma.iter_mut().zip(&v).for_each(|(s, x)| *s -= f64::from(*x));
        }
        let ahora: Option<Vec<f32>> = actual.query_row([n], |f| Ok(a_f32(f.get_ref(0)?.as_blob()?))).optional()?;
        if let Some(e) = &ahora {
            if r.cuantos == 0 && r.dim == 0 {
                r.dim = e.len();
                r.suma = vec![0.0; e.len()];
            }
            if e.len() != r.dim || r.dim == 0 {
                return Ok(None);
            }
            r.cuantos += 1;
            r.suma.iter_mut().zip(e).for_each(|(s, x)| *s += f64::from(*x));
        }
        let b = n >> BITS_BLOQUE;
        let bloque = match bloques.entry(b) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => v.insert(
                leer_bloque
                    .query_row([b], |f| Ok(Bloque::leer(f.get_ref(0)?.as_blob()?, f.get_ref(1)?.as_blob()?)))
                    .optional()?
                    .unwrap_or(Bloque { ns: Vec::new(), datos: Vec::new() }),
            ),
        };
        bloque.poner(n, ahora.as_deref(), r.dim);
    }
    if r.cuantos < 0 {
        return Ok(None);
    }
    for (b, bloque) in &bloques {
        if bloque.ns.is_empty() {
            db.execute("DELETE FROM vec_bloque WHERE b = ?1", [b])?;
        } else {
            db.execute(
                "INSERT INTO vec_bloque(b, ns, datos) VALUES (?1, ?2, ?3) ON CONFLICT(b) DO UPDATE SET ns = excluded.ns, datos = excluded.datos",
                params![b, bloque.ns_bytes(), bloque.datos],
            )?;
        }
    }
    db.execute("DELETE FROM vec_cambio", [])?;
    escribir_resumen(db, &r)?;
    Ok(Some(r))
}

/// Si la conexión tiene lo que usa el resumen (ver `db::LOCALES`).
fn hay_locales(db: &Connection) -> Result<bool> {
    let tablas: i64 =
        db.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('vec_bloque', 'vec_cambio', 'memory_ambito', 'memory_cuenta')", [], |f| f.get(0))?;
    Ok(tablas == 4)
}

/// El resumen al día con `memory_vec`: si hay cambios apuntados, se aplican (o se rehace entero), y
/// se guarda. `None` si no se puede usar (vectores de dimensiones distintas, o una conexión sin las
/// tablas locales): entonces se compara con todos los vectores.
fn al_dia(db: &Connection) -> Result<Option<Resumen>> {
    if !hay_locales(db)? {
        return Ok(None);
    }
    let r = leer_resumen(db)?;
    let hay_cambios: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM vec_cambio)", [], |f| f.get(0))?;
    if r.is_some() && !hay_cambios {
        return Ok(r);
    }
    // Dentro de la transacción de quien llama, lo que se escribe va con ella; si no, en una propia,
    // IMMEDIATE, y se mira otra vez lo que hay: otro proceso pudo ponerlo al día mientras se esperaba.
    let tx = if db.is_autocommit() { Some(Transaction::new_unchecked(db, TransactionBehavior::Immediate)?) } else { None };
    let r = match leer_resumen(db)? {
        Some(r) => match aplicar_cambios(db, r)? {
            Some(r) => Some(r),
            None => reconstruir(db)?,
        },
        None => reconstruir(db)?,
    };
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(r)
}

/// Cuántas memorias no tienen vector. Con el resumen, cruzando sus `n` con los de `memory` por el
/// índice `memory_ambito`, sin leer `memory_vec` (con 100.000 memorias eran 135 ms en cada búsqueda).
fn faltan(db: &Connection) -> Result<i64> {
    if al_dia(db)?.is_none() {
        return Ok(db.query_row(
            "SELECT count(*) FROM memory m WHERE NOT EXISTS (SELECT 1 FROM memory_vec v WHERE v.n = m.n)",
            [],
            |f| f.get(0),
        )?);
    }
    let mut hay: Vec<i64> = Vec::new();
    {
        let mut q = db.prepare("SELECT ns FROM vec_bloque ORDER BY b")?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            hay.extend(f.get_ref(0)?.as_blob()?.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)));
        }
    }
    let mut q = db.prepare("SELECT m.n FROM memory m INDEXED BY memory_ambito ORDER BY m.n")?;
    let mut filas = q.query([])?;
    let (mut i, mut faltan) = (0, 0);
    while let Some(f) = filas.next()? {
        let n: i64 = f.get(0)?;
        while i < hay.len() && hay[i] < n {
            i += 1;
        }
        if hay.get(i) != Some(&n) {
            faltan += 1;
        }
    }
    Ok(faltan)
}

/// `c · q` y `c · m` de una vez, con 8 sumas a la par para que el compilador use instrucciones
/// vectoriales.
fn dos_puntos(c: &[u8], q: &[f32], m: &[f32]) -> (f32, f32) {
    let (mut aq, mut am) = ([0f32; 8], [0f32; 8]);
    let (cs, cr) = c.as_chunks::<8>();
    let (qs, qr) = q.as_chunks::<8>();
    let (ms, mr) = m.as_chunks::<8>();
    for ((cc, qq), mm) in cs.iter().zip(qs).zip(ms) {
        for i in 0..8 {
            let x = f32::from(cc[i] as i8);
            aq[i] += x * qq[i];
            am[i] += x * mm[i];
        }
    }
    let (mut sq, mut sm) = (aq.iter().sum::<f32>(), am.iter().sum::<f32>());
    for ((c, q), m) in cr.iter().zip(qr).zip(mr) {
        let x = f32::from(*c as i8);
        sq += x * q;
        sm += x * m;
    }
    (sq, sm)
}

/// La `limite`-ésima mejor similitud de las calculadas, si ya hay tantas.
fn peor_de(sims: &[(i64, f32)], limite: usize) -> Option<f32> {
    (sims.len() >= limite).then(|| {
        let mut s: Vec<f32> = sims.iter().map(|x| x.1).collect();
        s.sort_by(|a, b| b.total_cmp(a));
        s[limite - 1]
    })
}

/// Las más parecidas con el resumen: se acota el parecido de cada vector con su copia int8 y solo se
/// calcula el de verdad (con el vector entero y la misma cuenta que `lineal`) de las que pueden quedar
/// entre las `limite` primeras. Lo que se descarta no llega ni a `minimo` ni a la peor de las que
/// quedan, así que el resultado es el de `lineal` (a igualdad de parecido, primero el `n` menor).
#[allow(clippy::too_many_arguments)]
fn con_resumen(
    db: &Connection,
    consulta: &[f32],
    media: &[f32],
    ambito: &Ambito,
    kind: Option<Tipo>,
    minimo: f32,
    limite: usize,
    dim: usize,
) -> Result<Vec<(i64, f32)>> {
    if limite == 0 {
        return Ok(Vec::new());
    }
    let mq: f32 = media.iter().zip(consulta).map(|(a, b)| a * b).sum();
    let mm: f32 = media.iter().map(|x| x * x).sum();
    // Si el ámbito no las ve todas (o se pide un tipo), solo cuentan los `n` que ve, en orden como el
    // resumen. Si las ve todas, no hace falta mirarlo (lo que no exista se descarta al comprobar).
    let todas: i64 = db.query_row("SELECT COALESCE(sum(n), 0) FROM memory_cuenta", [], |f| f.get(0))?;
    let permitidas: Option<Vec<i64>> = if kind.is_none() && crate::memoria::cuantas(db, ambito)? == todas {
        None
    } else {
        let mut q = db.prepare(&format!(
            "SELECT m.n FROM memory m INDEXED BY memory_ambito WHERE {} AND (?1 IS NULL OR m.kind = ?1) ORDER BY m.n",
            ambito.sql()
        ))?;
        let v = q.query_map(params![kind.map(Tipo::as_str)], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        Some(v)
    };
    let mut cotas = cotas_de(db, consulta, media, permitidas.as_deref(), minimo, Some(limite), dim)?;
    // Primer corte: la `limite`-ésima mejor cota inferior (hay al menos `limite` que llegan a ella).
    // Si alguna de esas no estaba en el ámbito o ya no existe, se repasan todas hasta `minimo`.
    let mut corte = if cotas.len() >= limite {
        let mut inf: Vec<f32> = cotas.iter().map(|c| c.1).collect();
        let (_, k, _) = inf.select_nth_unstable_by(limite - 1, |a, b| b.total_cmp(a));
        k.max(minimo)
    } else {
        minimo
    };
    cotas.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
    let mut exacto = db.prepare(&format!(
        "SELECT v.e FROM memory_vec v JOIN memory m INDEXED BY memory_ambito ON m.n = v.n
         WHERE v.n = ?1 AND {} AND (?2 IS NULL OR m.kind = ?2)",
        ambito.sql()
    ))?;
    let mut sims: Vec<(i64, f32)> = Vec::new();
    let mut i = 0;
    loop {
        let mut peor = peor_de(&sims, limite);
        while let Some((n, _, sup)) = cotas.get(i) {
            if *sup < corte || peor.is_some_and(|p| *sup < p) {
                break;
            }
            i += 1;
            let e: Option<Vec<u8>> = exacto.query_row(params![n, kind.map(Tipo::as_str)], |f| f.get(0)).optional()?;
            if let Some(e) = e {
                let norma = (1.0 - 2.0 * punto(&e, media) + mm).max(1e-12).sqrt();
                let s = (punto(&e, consulta) - mq) / norma;
                if s >= minimo {
                    sims.push((*n, s));
                    peor = peor_de(&sims, limite);
                }
            }
        }
        if corte <= minimo || peor.is_some_and(|p| p >= corte) {
            break;
        }
        // Las que fijaban el corte no estaban: se rehacen las cotas sin podar (las podadas podrían
        // entrar) y se sigue con las que aún no se han mirado.
        let miradas: std::collections::BTreeSet<i64> = cotas[..i].iter().map(|c| c.0).collect();
        cotas = cotas_de(db, consulta, media, permitidas.as_deref(), minimo, None, dim)?;
        cotas.retain(|c| !miradas.contains(&c.0));
        cotas.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        i = 0;
        corte = minimo;
    }
    sims.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    sims.truncate(limite);
    Ok(sims)
}

/// Un `f32` ordenable, para el montón de `cotas_de`.
#[derive(PartialEq)]
struct Orden(f32);

impl Eq for Orden {}

impl PartialOrd for Orden {
    fn partial_cmp(&self, otro: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(otro))
    }
}

impl Ord for Orden {
    fn cmp(&self, otro: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&otro.0)
    }
}

/// `(n, cota inferior, cota superior)` del parecido de cada vector del resumen (de los `n` de
/// `permitidas`, si se dan) que puede llegar a `minimo`. Con `podar = Some(k)`, además, se deja fuera
/// lo que no llega a la `k`-ésima mejor cota inferior vista hasta entonces (no puede quedar entre las
/// `k` primeras): con una búsqueda que lo pide todo (`minimo` = -1) no se guardan 100.000 cotas para
/// ordenarlas. Las `k` mejores cotas inferiores siempre se quedan.
fn cotas_de(
    db: &Connection,
    consulta: &[f32],
    media: &[f32],
    permitidas: Option<&[i64]>,
    minimo: f32,
    podar: Option<usize>,
    dim: usize,
) -> Result<Vec<(i64, f32, f32)>> {
    let mq: f32 = media.iter().zip(consulta).map(|(a, b)| a * b).sum();
    let mm: f32 = media.iter().map(|x| x * x).sum();
    let norma = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt() as f32;
    let (nq, nm) = (norma(consulta), norma(media));
    let mut mejores: std::collections::BinaryHeap<std::cmp::Reverse<Orden>> = std::collections::BinaryHeap::new();
    let mut umbral = minimo;
    let mut cotas: Vec<(i64, f32, f32)> = Vec::new();
    let t = tam_entrada(dim);
    let mut p = 0;
    let mut q = db.prepare("SELECT ns, datos FROM vec_bloque ORDER BY b")?;
    let mut filas = q.query([])?;
    while let Some(f) = filas.next()? {
        let ns = f.get_ref(0)?.as_blob()?;
        let datos = f.get_ref(1)?.as_blob()?;
        for (i, n) in ns.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)).enumerate() {
            if let Some(v) = &permitidas {
                while p < v.len() && v[p] < n {
                    p += 1;
                }
                if v.get(p) != Some(&n) {
                    continue;
                }
            }
            let entrada = &datos[i * t..(i + 1) * t];
            let escala = f32::from_le_bytes([entrada[0], entrada[1], entrada[2], entrada[3]]);
            let resto = f32::from_le_bytes([entrada[4], entrada[5], entrada[6], entrada[7]]);
            let (cq, cm) = dos_puntos(&entrada[8..], consulta, media);
            let (eq, em) = (escala * cq, escala * cm);
            let (dq, dm) = (resto * nq + HOLGURA, resto * nm + HOLGURA);
            // s = (e·q - μ·q) / √(1 - 2·e·μ + μ·μ), con e·q y e·μ dentro de ± su error.
            let raiz_menor = (1.0 - 2.0 * (em + dm) + mm).max(1e-12).sqrt();
            let raiz_mayor = (1.0 - 2.0 * (em - dm) + mm).max(1e-12).sqrt();
            let (num_min, num_max) = (eq - dq - mq, eq + dq - mq);
            let sup = if num_max >= 0.0 { num_max / raiz_menor } else { num_max / raiz_mayor };
            if sup < umbral {
                continue;
            }
            let inf = if num_min >= 0.0 { num_min / raiz_mayor } else { num_min / raiz_menor };
            cotas.push((n, inf, sup));
            if let Some(k) = podar {
                mejores.push(std::cmp::Reverse(Orden(inf)));
                if mejores.len() > k {
                    mejores.pop();
                }
                if mejores.len() == k
                    && let Some(std::cmp::Reverse(Orden(peor))) = mejores.peek()
                {
                    umbral = umbral.max(*peor);
                }
            }
        }
    }
    // Lo que entró antes de subir el umbral y ya no llega.
    cotas.retain(|c| c.2 >= umbral);
    Ok(cotas)
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
    let ambito = match proyecto {
        Some(p) => Ambito::de(db, p)?,
        None => Ambito::Todos,
    };
    parecidas_en(db, modelo, texto, &ambito, kind, minimo, limite)
}

/// Como `parecidas`, en un ámbito ya resuelto.
pub fn parecidas_en(
    db: &Connection,
    modelo: &Modelo,
    texto: &str,
    ambito: &Ambito,
    kind: Option<Tipo>,
    minimo: f32,
    limite: usize,
) -> Result<Vec<(Memoria, f32)>> {
    let resumen = al_dia(db)?.filter(|r| r.dim == modelo.dim() || r.cuantos == 0);
    let media = match &resumen {
        Some(r) if r.cuantos > 0 => media_de(db, modelo, r)?,
        Some(_) => media_generica(db, modelo)?,
        None => media(db, modelo)?,
    };
    let consulta = normalizado(modelo.vector(texto)?.iter().zip(&media).map(|(x, m)| x - m).collect());
    let sims = match &resumen {
        Some(r) if r.cuantos == 0 => Vec::new(),
        Some(r) => con_resumen(db, &consulta, &media, ambito, kind, minimo, limite, r.dim)?,
        None => lineal(db, &consulta, &media, ambito, kind, minimo, limite)?,
    };
    // Solo se leen enteras las que se devuelven.
    let mut q = db.prepare(&format!("SELECT {COLUMNAS} FROM memory m WHERE m.n = ?1"))?;
    sims.into_iter().map(|(n, s)| Ok((q.query_row([n], de_fila)?, s))).collect()
}

/// Las más parecidas comparando con todos los vectores del ámbito: sin resumen.
fn lineal(
    db: &Connection,
    consulta: &[f32],
    media: &[f32],
    ambito: &Ambito,
    kind: Option<Tipo>,
    minimo: f32,
    limite: usize,
) -> Result<Vec<(i64, f32)>> {
    // Con `a` normalizado: cos(a - μ, q) = (a·q - μ·q) / |a - μ|, y |a - μ|² = 1 - 2·a·μ + μ·μ.
    // Así no hace falta centrar (ni copiar) cada vector guardado: bastan dos productos escalares.
    let mq: f32 = media.iter().zip(consulta).map(|(a, b)| a * b).sum();
    let mm: f32 = media.iter().map(|x| x * x).sum();
    let mut sims: Vec<(i64, f32)> = Vec::new();
    {
        let mut q = db.prepare(&format!(
            "SELECT v.n, v.e FROM memory_vec v JOIN memory m ON m.n = v.n
             WHERE {} AND (?1 IS NULL OR m.kind = ?1)",
            ambito.sql()
        ))?;
        let mut filas = q.query(params![kind.map(Tipo::as_str)])?;
        while let Some(f) = filas.next()? {
            let e = f.get_ref(1)?.as_blob()?;
            let norma = (1.0 - 2.0 * punto(e, media) + mm).max(1e-12).sqrt();
            let s = (punto(e, consulta) - mq) / norma;
            if s >= minimo {
                sims.push((f.get(0)?, s));
            }
        }
    }
    sims.sort_by(|a, b| b.1.total_cmp(&a.1));
    sims.truncate(limite);
    Ok(sims)
}

/// Si ya hay una memoria casi igual en el mismo ámbito (el proyecto y las globales, o solo las
/// globales si la nueva es global; todas si `project` es una carpeta general), la devuelve con su
/// similitud.
pub fn duplicada(db: &Connection, modelo: &Modelo, project: &str, title: &str, content: &str) -> Result<Option<(Memoria, f32)>> {
    let nueva = texto(title, content);
    let mut v = parecidas_en(db, modelo, &nueva, &Ambito::de(db, project)?, None, SIMILITUD_DUPLICADO_CON_PALABRAS, 5)?;
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
        "SELECT {COLUMNAS} FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND {} AND m.id != ?2
         ORDER BY bm25(memory_fts) LIMIT 30",
        Ambito::de(db, project)?.sql()
    ))?;
    let candidatas: Vec<Memoria> = q.query_map(params![consulta, excluir], de_fila)?.collect::<rusqlite::Result<_>>()?;
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::memoria::{self, Cambios, Nueva};

    /// Números pseudoaleatorios repetibles (xorshift), para no depender de nada.
    struct Azar(u64);

    impl Azar {
        fn siguiente(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }

        /// Un vector de norma 1 cerca de uno de los `centros`: parecidos de todo tipo, como los de
        /// verdad (unos muy cerca, la mayoría lejos).
        fn vector(&mut self, centros: &[Vec<f32>]) -> Vec<f32> {
            let c = &centros[(self.siguiente().abs() * centros.len() as f32) as usize % centros.len()];
            normalizado(c.iter().map(|x| x + 0.08 * self.siguiente()).collect())
        }
    }

    const DIM: usize = 256;

    fn base(nombre: &str) -> Connection {
        let dir = std::env::temp_dir().join(format!("omoi-resumen-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::db::abrir(&dir.join("o.db")).unwrap()
    }

    fn guardar(db: &mut Connection, project: &str, kind: Tipo, i: usize) -> (String, i64) {
        let n = Nueva { project: project.into(), kind: Some(kind), title: format!("nota {i}"), content: format!("texto {i}"), ..Default::default() };
        let m = memoria::guardar(db, n, None).unwrap();
        let n: i64 = db.query_row("SELECT n FROM memory WHERE id = ?1", [&m.id], |f| f.get(0)).unwrap();
        (m.id, n)
    }

    fn poner_vector(db: &Connection, n: i64, v: &[f32]) {
        db.execute("INSERT OR REPLACE INTO memory_vec(n, e) VALUES (?1, ?2)", params![n, bytes(v)]).unwrap();
    }

    fn bloques(db: &Connection) -> Vec<(i64, Vec<u8>, Vec<u8>)> {
        let mut q = db.prepare("SELECT b, ns, datos FROM vec_bloque ORDER BY b").unwrap();
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn cuenta(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |f| f.get(0)).unwrap()
    }

    /// Una base con memorias de dos proyectos, globales y de dos tipos, con vectores al azar.
    fn con_vectores(nombre: &str, cuantas: usize, azar: &mut Azar) -> (Connection, Vec<Vec<f32>>) {
        let mut db = base(nombre);
        let centros: Vec<Vec<f32>> = (0..12).map(|_| normalizado((0..DIM).map(|_| azar.siguiente()).collect())).collect();
        for i in 0..cuantas {
            let project = ["/p1", "/p1", "/p2", ""][i % 4];
            let kind = if i % 3 == 0 { Tipo::Gotcha } else { Tipo::Fact };
            let (_, n) = guardar(&mut db, project, kind, i);
            poner_vector(&db, n, &azar.vector(&centros));
        }
        (db, centros)
    }

    #[test]
    fn el_resumen_da_lo_mismo_que_comparar_con_todos() {
        let mut azar = Azar(0x9e3779b97f4a7c15);
        let (db, centros) = con_vectores("igual", 1200, &mut azar);
        let r = al_dia(&db).unwrap().expect("resumen");
        assert_eq!(r.cuantos, 1200);
        let media: Vec<f32> = r.suma.iter().map(|s| (*s / r.cuantos as f64) as f32).collect();
        let ambitos = [Ambito::Todos, Ambito::de(&db, "/p1").unwrap(), Ambito::de(&db, "/p2").unwrap(), Ambito::Espacio(None)];
        let (mut con_algo, mut cortadas) = (0, 0);
        for _ in 0..12 {
            let consulta = normalizado(azar.vector(&centros).iter().zip(&media).map(|(x, m)| x - m).collect());
            for ambito in &ambitos {
                for kind in [None, Some(Tipo::Gotcha)] {
                    for (minimo, limite) in [(-1.0, 40), (-1.0, 1), (0.1, 5), (0.35, 2), (0.6, 3), (0.9, 2)] {
                        let a = lineal(&db, &consulta, &media, ambito, kind, minimo, limite).unwrap();
                        let b = con_resumen(&db, &consulta, &media, ambito, kind, minimo, limite, DIM).unwrap();
                        assert_eq!(a, b, "{ambito:?} {kind:?} {minimo} {limite}");
                        con_algo += usize::from(!a.is_empty());
                        cortadas += usize::from(a.len() == limite && minimo > 0.0);
                    }
                }
            }
        }
        // Que se prueben de verdad los dos cortes: por `minimo` y por `limite`.
        assert!(con_algo > 300 && cortadas > 50, "{con_algo} {cortadas}");
    }

    #[test]
    fn el_resumen_sigue_a_los_cambios() {
        let mut azar = Azar(0x2545f4914f6cdd1d);
        let (mut db, centros) = con_vectores("cambios", 300, &mut azar);
        al_dia(&db).unwrap().expect("resumen");
        // Un vector cambiado con `INSERT OR REPLACE` (como al editar), otro con `UPDATE`, uno borrado a
        // mano, memorias olvidadas (sus vectores se van en cascada), una nueva y otra sin vector.
        let ns: Vec<i64> = {
            let mut q = db.prepare("SELECT n FROM memory_vec ORDER BY n").unwrap();
            q.query_map([], |f| f.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        poner_vector(&db, ns[3], &azar.vector(&centros));
        poner_vector(&db, ns[3], &azar.vector(&centros));
        db.execute("UPDATE memory_vec SET e = ?2 WHERE n = ?1", params![ns[7], bytes(&azar.vector(&centros))]).unwrap();
        db.execute("DELETE FROM memory_vec WHERE n = ?1", [ns[11]]).unwrap();
        for i in [20, 21, 299] {
            let id: String = db.query_row("SELECT id FROM memory WHERE n = ?1", [ns[i]], |f| f.get(0)).unwrap();
            memoria::olvidar(&mut db, &id).unwrap();
        }
        let (_, nueva) = guardar(&mut db, "/p2", Tipo::Fact, 1000);
        poner_vector(&db, nueva, &azar.vector(&centros));
        guardar(&mut db, "/p1", Tipo::Fact, 1001);
        // La nueva reusa el `n` de la última olvidada: un solo apunte, con el vector de antes.
        assert_eq!(cuenta(&db, "SELECT count(*) FROM vec_cambio"), 6);
        let r = al_dia(&db).unwrap().expect("resumen");
        assert_eq!(cuenta(&db, "SELECT count(*) FROM vec_cambio"), 0);
        assert_eq!(faltan(&db).unwrap(), 2, "la del vector borrado y la nueva sin vector");
        let poco_a_poco = bloques(&db);
        // Lo mismo que rehaciéndolo entero.
        let entero = reconstruir(&db).unwrap().expect("resumen");
        assert_eq!(r.cuantos, entero.cuantos);
        assert_eq!(r.cuantos, cuenta(&db, "SELECT count(*) FROM memory_vec"));
        for (a, b) in r.suma.iter().zip(&entero.suma) {
            assert!((a - b).abs() < 1e-9, "{a} {b}");
        }
        assert_eq!(poco_a_poco, bloques(&db));
    }

    #[test]
    fn sin_las_tablas_locales_se_compara_con_todos() {
        let mut azar = Azar(7);
        let (db, _) = con_vectores("sin-locales", 20, &mut azar);
        db.execute_batch("DROP TABLE vec_bloque").unwrap();
        assert!(al_dia(&db).unwrap().is_none());
        assert_eq!(faltan(&db).unwrap(), 0);
    }

    #[test]
    fn las_cuentas_por_ambito_cuadran() {
        let mut db = base("cuentas");
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(guardar(&mut db, ["/p1", "/p2", "", "/p3"][i % 4], Tipo::Fact, i).0);
        }
        memoria::olvidar(&mut db, &ids[0]).unwrap();
        memoria::olvidar(&mut db, &ids[2]).unwrap();
        let mover = Cambios { project: Some("/p2".into()), ..Default::default() };
        memoria::actualizar(&mut db, &ids[5], mover, None).unwrap();
        let global = Cambios { project: Some(String::new()), ..Default::default() };
        memoria::actualizar(&mut db, &ids[9], global, None).unwrap();
        let ambitos = [Ambito::Todos, Ambito::de(&db, "/p1").unwrap(), Ambito::de(&db, "/p2").unwrap(), Ambito::de(&db, "/p3").unwrap(), Ambito::Espacio(None)];
        for ambito in ambitos {
            let contadas = cuenta(&db, &format!("SELECT count(*) FROM memory m WHERE {}", ambito.sql()));
            assert_eq!(memoria::cuantas(&db, &ambito).unwrap(), contadas, "{ambito:?}");
        }
        assert!(memoria::ve_todas(&db, &Ambito::Todos));
        assert!(!memoria::ve_todas(&db, &Ambito::de(&db, "/p1").unwrap()));
    }

    /// Si falta una pieza (una base que tocó otra versión, o una migración que rehízo `memory`), al
    /// abrir se rehacen todas y se recuenta; el resumen se rehará al usarlo.
    #[test]
    fn las_piezas_locales_se_rehacen_si_falta_alguna() {
        let mut azar = Azar(11);
        let (mut db, centros) = con_vectores("rehacer", 30, &mut azar);
        al_dia(&db).unwrap().expect("resumen");
        let ruta = std::path::PathBuf::from(db.path().unwrap());
        db.execute_batch("DROP TRIGGER memory_cuenta_mas; DROP TRIGGER vec_cambio_pon;").unwrap();
        let (_, n) = guardar(&mut db, "/p1", Tipo::Fact, 99);
        poner_vector(&db, n, &azar.vector(&centros));
        drop(db);
        let db = crate::db::abrir(&ruta).unwrap();
        assert_eq!(memoria::cuantas(&db, &Ambito::Todos).unwrap(), 31);
        assert!(leer_resumen(&db).unwrap().is_none());
        assert_eq!(al_dia(&db).unwrap().expect("resumen").cuantos, 31);
    }
}
