//! Memory vectors: storing them, keeping them up to date with the model and finding the most similar.
//!
//! They go in `memory_vec` as a BLOB (256 little-endian `f32`, normalized). To compare them they are
//! **centered**: the mean vector of the database's memories is subtracted. Static models
//! share a common direction that makes everything look like everything, especially with long texts
//! (with 127 real notes, 31 % of pairs of distinct notes went over 0.65 without centering; 4 of
//! 8,001 centering). The mean is always that of the current database; with few memories it is mixed
//! with a generic mean so it doesn't come out zero.
//!
//! Reading the whole vectors is costly (with 100,000 memories, 100 MB and 200 ms per query), so a
//! **summary** is kept separately (local tables, see `db::LOCALS`): the sum of the vectors for the
//! mean and an int8 copy of each (a quarter of the size) with how far it is from the real one.
//! With it the similarity of each memory is bounded and only those that may make it are read whole: the
//! result is the same as comparing with all the vectors. It is kept up to date by the triggers
//! of `memory_vec` (they record what changes in `vec_cambio`) and `up_to_date_summary`, which applies it when used.

use crate::embed::Model;
use crate::memory::{Scope, COLUMNS, Memory, Kind, from_row};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// Minimum similarity (centered vectors) for search to add a memory that didn't come up by
/// words. Calibrated in phase 2 with `tests/searches.tsv` and 127 real notes: from 0.25
/// queries without an answer return nothing. With a static model the rescue rarely
/// contributes: it is there for when the similarity is really high.
pub const SEARCH_SIMILARITY: f32 = 0.25;

/// When saving proposes updating the memory that already exists: if the similarity (centered vectors)
/// reaches `DUPLICATE_SIMILARITY`, or if it reaches `DUPLICATE_SIMILARITY_WITH_WORDS` and the words of the
/// new one are in the existing one (`DUPLICATE_COVERAGE` of its IDF weight, as in recall). The vector
/// alone didn't separate: with 0.70, 3.9 % of 127 distinct real notes warned falsely and only
/// 66 % of those notes saved again as an agent would (another title, the start of the text) were
/// detected. With both signals: 0 % false warnings (notes and short sentences) and 89 %
/// detected (`examples/phase2.rs dupcob`). What is lost: short paraphrases with other words.
pub const DUPLICATE_SIMILARITY: f32 = 0.78;
pub const DUPLICATE_SIMILARITY_WITH_WORDS: f32 = 0.40;
pub const DUPLICATE_COVERAGE: f64 = 0.60;

/// What is turned into a vector: the title and the content.
pub fn text(title: &str, content: &str) -> String {
    format!("{title}\n{content}")
}

fn bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn save(tx: &Transaction, n: i64, model: &Model, title: &str, content: &str) -> Result<()> {
    let v = model.vector(&text(title, content))?;
    tx.execute("INSERT OR REPLACE INTO memory_vec(n, e) VALUES (?1, ?2)", params![n, bytes(&v)])?;
    Ok(())
}

/// Brings the vectors up to date with `model`: if they were computed with another, redoes them all; if
/// some are missing (memories saved without a model installed), adds them. Returns how many it computed. With
/// thousands of memories it's milliseconds (a vector costs ~0.03 ms).
pub fn sync(db: &mut Connection, model: &Model) -> Result<usize> {
    let stored: Option<String> =
        db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |row| row.get(0)).optional()?;
    if stored.as_deref() == Some(model.id()) && missing_count(db)? == 0 {
        return Ok(0);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored: Option<String> =
        tx.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |row| row.get(0)).optional()?;
    if stored.as_deref() != Some(model.id()) {
        // Before deleting: without a summary, the triggers don't record each old vector in `vec_cambio`.
        if has_local_tables(&tx)? {
            forget_summary(&tx)?;
        }
        tx.execute("DELETE FROM memory_vec", [])?;
        tx.execute(
            "INSERT INTO meta(k, v) VALUES ('modelo', ?1) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            [model.id()],
        )?;
    }
    let pending: Vec<(i64, String, String)> = {
        let mut q = tx.prepare(
            "SELECT n, title, content FROM memory m WHERE NOT EXISTS (SELECT 1 FROM memory_vec v WHERE v.n = m.n)",
        )?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (n, t, c) in &pending {
        save(&tx, *n, model, t, c)?;
    }
    tx.commit()?;
    Ok(pending.len())
}

/// How much the generic mean weighs against the database's memories (as if it were that many memories).
const GENERIC_MEAN_WEIGHT: f32 = 20.0;

fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

/// Dot product of a stored vector (little-endian bytes) with another, without copying it.
fn dot(b: &[u8], v: &[f32]) -> f32 {
    b.as_chunks::<4>().0.iter().zip(v).map(|(x, y)| f32::from_le_bytes(*x) * y).sum()
}

/// Mean of a generic corpus (the tests' one: code, documentation and sentences in many languages)
/// with this model. Computed once per model (~50 ms) and stored in `meta`.
fn generic_mean(db: &Connection, model: &Model) -> Result<Vec<f32>> {
    let key = format!("media:{}", model.id());
    if let Some(t) = db.query_row("SELECT v FROM meta WHERE k = ?1", [&key], |row| row.get::<_, String>(0)).optional()? {
        return Ok(serde_json::from_str(&t)?);
    }
    let lines: Vec<&str> = include_str!("../tests/corpus.txt").lines().filter(|l| !l.starts_with('#')).collect();
    let mut mean = vec![0f32; model.dim()];
    for l in &lines {
        for (m, x) in mean.iter_mut().zip(model.vector(l)?) {
            *m += x / lines.len() as f32;
        }
    }
    db.execute("INSERT OR IGNORE INTO meta(k, v) VALUES (?1, ?2)", params![key, serde_json::to_string(&mean)?])?;
    Ok(mean)
}

/// The mean used for centering: that of the database's memories, with the generic one as if it were
/// `GENERIC_MEAN_WEIGHT` more memories.
fn centering_mean(db: &Connection, model: &Model) -> Result<Vec<f32>> {
    let mut sum: Vec<f32> = generic_mean(db, model)?.iter().map(|x| x * GENERIC_MEAN_WEIGHT).collect();
    let mut n = GENERIC_MEAN_WEIGHT;
    let mut q = db.prepare("SELECT e FROM memory_vec")?;
    let mut rows = q.query([])?;
    while let Some(row) = rows.next()? {
        for (s, x) in sum.iter_mut().zip(row.get_ref(0)?.as_blob()?.as_chunks::<4>().0) {
            *s += f32::from_le_bytes(*x);
        }
        n += 1.0;
    }
    Ok(sum.iter().map(|s| s / n).collect())
}

/// The same mean, with the summary's sum (in `f64`: it doesn't depend on the order it was summed in).
fn mean_from_summary(db: &Connection, model: &Model, r: &Summary) -> Result<Vec<f32>> {
    let generic = generic_mean(db, model)?;
    let n = f64::from(GENERIC_MEAN_WEIGHT) + r.count as f64;
    Ok(generic.iter().zip(&r.sum).map(|(g, s)| ((f64::from(*g) * f64::from(GENERIC_MEAN_WEIGHT) + s) / n) as f32).collect())
}

// --- The summary ----------------------------------------------------------------------------------

/// Where the summary's state goes in `meta`.
const SUMMARY_KEY: &str = "vec_resumen";
/// How many consecutive `n` go in each `vec_bloque` row (`b = n >> BLOCK_BITS`).
const BLOCK_BITS: u32 = 10;
/// Margin added to each bound for `f32` rounding (the real ones are on the order of 1e-5).
const SLACK: f32 = 1e-3;

/// The part of the summary that goes in `meta`: the dimension of the vectors, how many there are and their sum.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Summary {
    dim: usize,
    #[serde(rename = "cuantos")]
    count: i64,
    #[serde(rename = "suma")]
    sum: Vec<f64>,
}

/// What each vector takes in a block: scale (`f32`), how far it is from the real one (`f32`, the
/// norm of the difference, rounded up) and one `i8` per dimension.
fn entry_size(dim: usize) -> usize {
    8 + dim
}

fn to_f32(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect()
}

/// A vector in int8: `e ≈ scale · c`, and `|e - scale · c|`, which bounds the error of any dot
/// product with it (Cauchy-Schwarz: at most that times the norm of the other vector).
fn quantize(e: &[f32], out: &mut Vec<u8>) {
    let max = e.iter().fold(0f32, |m, x| m.max(x.abs()));
    let scale = if max > 0.0 { max / 127.0 } else { 0.0 };
    let mut codes = Vec::with_capacity(e.len());
    let mut residual = 0f64;
    for x in e {
        let c = if scale > 0.0 { (x / scale).round().clamp(-127.0, 127.0) } else { 0.0 };
        residual += (f64::from(*x) - f64::from(scale) * f64::from(c)).powi(2);
        codes.push(c as i8 as u8);
    }
    let residual = (residual.sqrt() * (1.0 + 1e-6) + 1e-9) as f32;
    out.extend(scale.to_le_bytes());
    out.extend(residual.to_le_bytes());
    out.extend(codes);
}

/// A `vec_bloque` block in memory: the `n` in order and their entries.
struct Block {
    ns: Vec<i64>,
    data: Vec<u8>,
}

impl Block {
    fn read(ns: &[u8], data: &[u8]) -> Block {
        Block { ns: ns.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)).collect(), data: data.to_vec() }
    }

    fn ns_bytes(&self) -> Vec<u8> {
        self.ns.iter().flat_map(|n| n.to_le_bytes()).collect()
    }

    /// Removes `n` (if present) and, with `e`, puts it back in its place.
    fn set(&mut self, n: i64, e: Option<&[f32]>, dim: usize) {
        let t = entry_size(dim);
        let i = self.ns.partition_point(|x| *x < n);
        if self.ns.get(i) == Some(&n) {
            self.ns.remove(i);
            self.data.drain(i * t..(i + 1) * t);
        }
        if let Some(e) = e {
            let mut entry = Vec::with_capacity(t);
            quantize(e, &mut entry);
            self.ns.insert(i, n);
            self.data.splice(i * t..i * t, entry);
        }
    }
}

fn read_summary(db: &Connection) -> Result<Option<Summary>> {
    let t: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [SUMMARY_KEY], |row| row.get(0)).optional()?;
    Ok(t.and_then(|t| serde_json::from_str(&t).ok()))
}

fn write_summary(db: &Connection, r: &Summary) -> Result<()> {
    db.execute(
        "INSERT INTO meta(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![SUMMARY_KEY, serde_json::to_string(r)?],
    )?;
    Ok(())
}

/// Deletes the summary: it will be fully rebuilt when used. Called by `db::ensure_locals` when (re)creating its tables.
pub(crate) fn forget_summary(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM meta WHERE k = ?1", [SUMMARY_KEY])?;
    db.execute_batch("DELETE FROM vec_bloque; DELETE FROM vec_cambio;")?;
    Ok(())
}

/// The summary built again with all the vectors. `None` if they don't all have the same dimension
/// (halfway through a model change): then it compares with all, as without a summary.
fn rebuild(db: &Connection) -> Result<Option<Summary>> {
    forget_summary(db)?;
    let mut r: Option<Summary> = None;
    let mut block: Option<(i64, Block)> = None;
    let mut insert = db.prepare("INSERT INTO vec_bloque(b, ns, datos) VALUES (?1, ?2, ?3)")?;
    let mut q = db.prepare("SELECT n, e FROM memory_vec ORDER BY n")?;
    let mut rows = q.query([])?;
    while let Some(row) = rows.next()? {
        let n: i64 = row.get(0)?;
        let e = to_f32(row.get_ref(1)?.as_blob()?);
        let r = r.get_or_insert_with(|| Summary { dim: e.len(), count: 0, sum: vec![0.0; e.len()] });
        if e.len() != r.dim || r.dim == 0 {
            forget_summary(db)?;
            return Ok(None);
        }
        r.count += 1;
        r.sum.iter_mut().zip(&e).for_each(|(s, x)| *s += f64::from(*x));
        let b = n >> BLOCK_BITS;
        if let Some((current, done)) = block.take_if(|(current, _)| *current != b) {
            insert.execute(params![current, done.ns_bytes(), done.data])?;
        }
        let (_, current) = block.get_or_insert_with(|| (b, Block { ns: Vec::new(), data: Vec::new() }));
        current.ns.push(n);
        quantize(&e, &mut current.data);
    }
    if let Some((b, done)) = block {
        insert.execute(params![b, done.ns_bytes(), done.data])?;
    }
    // Without vectors there is also a summary: empty, and the dimension is set by the first one to arrive.
    let r = r.unwrap_or(Summary { dim: 0, count: 0, sum: Vec::new() });
    write_summary(db, &r)?;
    Ok(Some(r))
}

/// Applies to the summary what was recorded in `vec_cambio`. `None` if rebuilding it pays off more (or it doesn't add up).
fn apply_changes(db: &Connection, mut r: Summary) -> Result<Option<Summary>> {
    let changes: Vec<(i64, Option<Vec<u8>>)> = {
        let mut q = db.prepare("SELECT n, viejo FROM vec_cambio ORDER BY n")?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    // Many changes (another model, a large import): better to rebuild it.
    if changes.len() as i64 > (r.count / 4).max(1000) {
        return Ok(None);
    }
    let mut current = db.prepare("SELECT e FROM memory_vec WHERE n = ?1")?;
    let mut read_block = db.prepare("SELECT ns, datos FROM vec_bloque WHERE b = ?1")?;
    let mut blocks: std::collections::BTreeMap<i64, Block> = std::collections::BTreeMap::new();
    for (n, old) in changes {
        if let Some(v) = old {
            let v = to_f32(&v);
            if v.len() != r.dim {
                return Ok(None);
            }
            r.count -= 1;
            r.sum.iter_mut().zip(&v).for_each(|(s, x)| *s -= f64::from(*x));
        }
        let now: Option<Vec<f32>> = current.query_row([n], |row| Ok(to_f32(row.get_ref(0)?.as_blob()?))).optional()?;
        if let Some(e) = &now {
            if r.count == 0 && r.dim == 0 {
                r.dim = e.len();
                r.sum = vec![0.0; e.len()];
            }
            if e.len() != r.dim || r.dim == 0 {
                return Ok(None);
            }
            r.count += 1;
            r.sum.iter_mut().zip(e).for_each(|(s, x)| *s += f64::from(*x));
        }
        let b = n >> BLOCK_BITS;
        let block = match blocks.entry(b) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => v.insert(
                read_block
                    .query_row([b], |row| Ok(Block::read(row.get_ref(0)?.as_blob()?, row.get_ref(1)?.as_blob()?)))
                    .optional()?
                    .unwrap_or(Block { ns: Vec::new(), data: Vec::new() }),
            ),
        };
        block.set(n, now.as_deref(), r.dim);
    }
    if r.count < 0 {
        return Ok(None);
    }
    for (b, block) in &blocks {
        if block.ns.is_empty() {
            db.execute("DELETE FROM vec_bloque WHERE b = ?1", [b])?;
        } else {
            db.execute(
                "INSERT INTO vec_bloque(b, ns, datos) VALUES (?1, ?2, ?3) ON CONFLICT(b) DO UPDATE SET ns = excluded.ns, datos = excluded.datos",
                params![b, block.ns_bytes(), block.data],
            )?;
        }
    }
    db.execute("DELETE FROM vec_cambio", [])?;
    write_summary(db, &r)?;
    Ok(Some(r))
}

/// Whether the connection has what the summary uses (see `db::LOCALS`).
fn has_local_tables(db: &Connection) -> Result<bool> {
    let tables: i64 =
        db.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('vec_bloque', 'vec_cambio', 'memory_ambito', 'memory_cuenta')", [], |row| row.get(0))?;
    Ok(tables == 4)
}

/// The summary up to date with `memory_vec`: if there are recorded changes, they are applied (or it is fully
/// rebuilt), and it is saved. `None` if it can't be used (vectors of different dimensions, or a connection
/// without the local tables): then it compares with all the vectors.
fn up_to_date_summary(db: &Connection) -> Result<Option<Summary>> {
    if !has_local_tables(db)? {
        return Ok(None);
    }
    let r = read_summary(db)?;
    let has_changes: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM vec_cambio)", [], |row| row.get(0))?;
    if r.is_some() && !has_changes {
        return Ok(r);
    }
    // Inside the caller's transaction, what is written goes with it; otherwise, in its own,
    // IMMEDIATE, and what's there is checked again: another process may have updated it while waiting.
    let tx = if db.is_autocommit() { Some(Transaction::new_unchecked(db, TransactionBehavior::Immediate)?) } else { None };
    let r = match read_summary(db)? {
        Some(r) => match apply_changes(db, r)? {
            Some(r) => Some(r),
            None => rebuild(db)?,
        },
        None => rebuild(db)?,
    };
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(r)
}

/// How many memories have no vector. With the summary, crossing its `n` with those of `memory` through the
/// `memory_ambito` index, without reading `memory_vec` (with 100,000 memories it was 135 ms on each search).
fn missing_count(db: &Connection) -> Result<i64> {
    if up_to_date_summary(db)?.is_none() {
        return Ok(db.query_row(
            "SELECT count(*) FROM memory m WHERE NOT EXISTS (SELECT 1 FROM memory_vec v WHERE v.n = m.n)",
            [],
            |row| row.get(0),
        )?);
    }
    let mut present: Vec<i64> = Vec::new();
    {
        let mut q = db.prepare("SELECT ns FROM vec_bloque ORDER BY b")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            present.extend(row.get_ref(0)?.as_blob()?.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)));
        }
    }
    let mut q = db.prepare("SELECT m.n FROM memory m INDEXED BY memory_ambito ORDER BY m.n")?;
    let mut rows = q.query([])?;
    let (mut i, mut missing) = (0, 0);
    while let Some(row) = rows.next()? {
        let n: i64 = row.get(0)?;
        while i < present.len() && present[i] < n {
            i += 1;
        }
        if present.get(i) != Some(&n) {
            missing += 1;
        }
    }
    Ok(missing)
}

/// `c · q` and `c · m` at once, with 8 sums in parallel so the compiler uses vector
/// instructions.
fn two_dots(c: &[u8], q: &[f32], m: &[f32]) -> (f32, f32) {
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

/// The `limit`-th best similarity among those computed, if there are already that many.
fn worst_of(sims: &[(i64, f32)], limit: usize) -> Option<f32> {
    (sims.len() >= limit).then(|| {
        let mut s: Vec<f32> = sims.iter().map(|x| x.1).collect();
        s.sort_by(|a, b| b.total_cmp(a));
        s[limit - 1]
    })
}

/// The most similar using the summary: the similarity of each vector is bounded with its int8 copy and the
/// real one (with the whole vector and the same computation as `linear`) is only computed for those that may end up
/// among the first `limit`. What is discarded reaches neither `min` nor the worst of those that
/// remain, so the result is that of `linear` (with equal similarity, the lower `n` first).
#[allow(clippy::too_many_arguments)]
fn with_summary(
    db: &Connection,
    query: &[f32],
    mean: &[f32],
    scope: &Scope,
    kind: Option<Kind>,
    min: f32,
    limit: usize,
    dim: usize,
) -> Result<Vec<(i64, f32)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mq: f32 = mean.iter().zip(query).map(|(a, b)| a * b).sum();
    let mm: f32 = mean.iter().map(|x| x * x).sum();
    // If the scope doesn't see them all (or a kind is requested), only the `n` it sees count, in order like the
    // summary. If it sees them all, there is no need to look (what doesn't exist is discarded when checking).
    let all: i64 = db.query_row("SELECT COALESCE(sum(n), 0) FROM memory_cuenta", [], |row| row.get(0))?;
    let allowed: Option<Vec<i64>> = if kind.is_none() && crate::memory::count(db, scope)? == all {
        None
    } else {
        let mut q = db.prepare(&format!(
            "SELECT m.n FROM memory m INDEXED BY memory_ambito WHERE {} AND (?1 IS NULL OR m.kind = ?1) ORDER BY m.n",
            scope.sql()
        ))?;
        let v = q.query_map(params![kind.map(Kind::as_str)], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        Some(v)
    };
    let mut bounds = bounds_of(db, query, mean, allowed.as_deref(), min, Some(limit), dim)?;
    // First cut: the `limit`-th best lower bound (there are at least `limit` that reach it).
    // If any of those wasn't in the scope or no longer exists, all are gone over down to `min`.
    let mut cut = if bounds.len() >= limit {
        let mut lower: Vec<f32> = bounds.iter().map(|c| c.1).collect();
        let (_, k, _) = lower.select_nth_unstable_by(limit - 1, |a, b| b.total_cmp(a));
        k.max(min)
    } else {
        min
    };
    bounds.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
    let mut exact = db.prepare(&format!(
        "SELECT v.e FROM memory_vec v JOIN memory m INDEXED BY memory_ambito ON m.n = v.n
         WHERE v.n = ?1 AND {} AND (?2 IS NULL OR m.kind = ?2)",
        scope.sql()
    ))?;
    let mut sims: Vec<(i64, f32)> = Vec::new();
    let mut i = 0;
    loop {
        let mut worst = worst_of(&sims, limit);
        while let Some((n, _, upper)) = bounds.get(i) {
            if *upper < cut || worst.is_some_and(|p| *upper < p) {
                break;
            }
            i += 1;
            let e: Option<Vec<u8>> = exact.query_row(params![n, kind.map(Kind::as_str)], |row| row.get(0)).optional()?;
            if let Some(e) = e {
                let norm = (1.0 - 2.0 * dot(&e, mean) + mm).max(1e-12).sqrt();
                let s = (dot(&e, query) - mq) / norm;
                if s >= min {
                    sims.push((*n, s));
                    worst = worst_of(&sims, limit);
                }
            }
        }
        if cut <= min || worst.is_some_and(|p| p >= cut) {
            break;
        }
        // The ones that set the cut weren't there: the bounds are redone without pruning (the pruned ones could
        // make it) and it continues with those not yet looked at.
        let looked: std::collections::BTreeSet<i64> = bounds[..i].iter().map(|c| c.0).collect();
        bounds = bounds_of(db, query, mean, allowed.as_deref(), min, None, dim)?;
        bounds.retain(|c| !looked.contains(&c.0));
        bounds.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        i = 0;
        cut = min;
    }
    sims.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    sims.truncate(limit);
    Ok(sims)
}

/// An orderable `f32`, for the heap in `bounds_of`.
#[derive(PartialEq)]
struct Ordered(f32);

impl Eq for Ordered {}

impl PartialOrd for Ordered {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ordered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// `(n, lower bound, upper bound)` of the similarity of each summary vector (of the `n` in
/// `allowed`, if given) that may reach `min`. With `prune = Some(k)`, also, what doesn't reach the
/// `k`-th best lower bound seen so far is left out (it can't end up among the first
/// `k`): with a search that asks for everything (`min` = -1) 100,000 bounds aren't kept to
/// sort them. The `k` best lower bounds are always kept.
fn bounds_of(
    db: &Connection,
    query: &[f32],
    mean: &[f32],
    allowed: Option<&[i64]>,
    min: f32,
    prune: Option<usize>,
    dim: usize,
) -> Result<Vec<(i64, f32, f32)>> {
    let mq: f32 = mean.iter().zip(query).map(|(a, b)| a * b).sum();
    let mm: f32 = mean.iter().map(|x| x * x).sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt() as f32;
    let (nq, nm) = (norm(query), norm(mean));
    let mut best: std::collections::BinaryHeap<std::cmp::Reverse<Ordered>> = std::collections::BinaryHeap::new();
    let mut threshold = min;
    let mut bounds: Vec<(i64, f32, f32)> = Vec::new();
    let t = entry_size(dim);
    let mut p = 0;
    let mut q = db.prepare("SELECT ns, datos FROM vec_bloque ORDER BY b")?;
    let mut rows = q.query([])?;
    while let Some(row) = rows.next()? {
        let ns = row.get_ref(0)?.as_blob()?;
        let data = row.get_ref(1)?.as_blob()?;
        for (i, n) in ns.as_chunks::<8>().0.iter().map(|x| i64::from_le_bytes(*x)).enumerate() {
            if let Some(v) = &allowed {
                while p < v.len() && v[p] < n {
                    p += 1;
                }
                if v.get(p) != Some(&n) {
                    continue;
                }
            }
            let entry = &data[i * t..(i + 1) * t];
            let scale = f32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
            let residual = f32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
            let (cq, cm) = two_dots(&entry[8..], query, mean);
            let (eq, em) = (scale * cq, scale * cm);
            let (dq, dm) = (residual * nq + SLACK, residual * nm + SLACK);
            // s = (e·q - μ·q) / √(1 - 2·e·μ + μ·μ), with e·q and e·μ within ± their error.
            let root_low = (1.0 - 2.0 * (em + dm) + mm).max(1e-12).sqrt();
            let root_high = (1.0 - 2.0 * (em - dm) + mm).max(1e-12).sqrt();
            let (num_min, num_max) = (eq - dq - mq, eq + dq - mq);
            let upper = if num_max >= 0.0 { num_max / root_low } else { num_max / root_high };
            if upper < threshold {
                continue;
            }
            let lower = if num_min >= 0.0 { num_min / root_high } else { num_min / root_low };
            bounds.push((n, lower, upper));
            if let Some(k) = prune {
                best.push(std::cmp::Reverse(Ordered(lower)));
                if best.len() > k {
                    best.pop();
                }
                if best.len() == k
                    && let Some(std::cmp::Reverse(Ordered(worst))) = best.peek()
                {
                    threshold = threshold.max(*worst);
                }
            }
        }
    }
    // What got in before the threshold rose and no longer reaches it.
    bounds.retain(|c| c.2 >= threshold);
    Ok(bounds)
}

/// Memories most similar to `text`, from most to least, with their similarity (cosine of the centered
/// vectors). `project` as in `Scope`: `Some(p)` = those of `p` plus the globals; `None` = all.
pub fn similar(
    db: &Connection,
    model: &Model,
    text: &str,
    project: Option<&str>,
    kind: Option<Kind>,
    min: f32,
    limit: usize,
) -> Result<Vec<(Memory, f32)>> {
    let scope = match project {
        Some(p) => Scope::from_project(db, p)?,
        None => Scope::All,
    };
    similar_in(db, model, text, &scope, kind, min, limit)
}

/// Like `similar`, in an already resolved scope.
pub fn similar_in(
    db: &Connection,
    model: &Model,
    text: &str,
    scope: &Scope,
    kind: Option<Kind>,
    min: f32,
    limit: usize,
) -> Result<Vec<(Memory, f32)>> {
    let summary = up_to_date_summary(db)?.filter(|r| r.dim == model.dim() || r.count == 0);
    let mean = match &summary {
        Some(r) if r.count > 0 => mean_from_summary(db, model, r)?,
        Some(_) => generic_mean(db, model)?,
        None => centering_mean(db, model)?,
    };
    let query = normalized(model.vector(text)?.iter().zip(&mean).map(|(x, m)| x - m).collect());
    let sims = match &summary {
        Some(r) if r.count == 0 => Vec::new(),
        Some(r) => with_summary(db, &query, &mean, scope, kind, min, limit, r.dim)?,
        None => linear(db, &query, &mean, scope, kind, min, limit)?,
    };
    // Only those returned are read whole.
    let mut q = db.prepare(&format!("SELECT {COLUMNS} FROM memory m WHERE m.n = ?1"))?;
    sims.into_iter().map(|(n, s)| Ok((q.query_row([n], from_row)?, s))).collect()
}

/// The most similar comparing with all the vectors of the scope: without a summary.
fn linear(
    db: &Connection,
    query: &[f32],
    mean: &[f32],
    scope: &Scope,
    kind: Option<Kind>,
    min: f32,
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    linear_where(db, query, mean, &scope.sql(), kind, min, limit)
}

/// `linear` among the memories that meet `condition` (SQL over `memory m`).
fn linear_where(
    db: &Connection,
    query: &[f32],
    mean: &[f32],
    condition: &str,
    kind: Option<Kind>,
    min: f32,
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    // With `a` normalized: cos(a - μ, q) = (a·q - μ·q) / |a - μ|, and |a - μ|² = 1 - 2·a·μ + μ·μ.
    // This way there is no need to center (or copy) each stored vector: two dot products suffice.
    let mq: f32 = mean.iter().zip(query).map(|(a, b)| a * b).sum();
    let mm: f32 = mean.iter().map(|x| x * x).sum();
    let mut sims: Vec<(i64, f32)> = Vec::new();
    {
        let mut q = db.prepare(&format!(
            "SELECT v.n, v.e FROM memory_vec v JOIN memory m ON m.n = v.n
             WHERE {condition} AND (?1 IS NULL OR m.kind = ?1)"
        ))?;
        let mut rows = q.query(params![kind.map(Kind::as_str)])?;
        while let Some(row) = rows.next()? {
            let e = row.get_ref(1)?.as_blob()?;
            let norm = (1.0 - 2.0 * dot(e, mean) + mm).max(1e-12).sqrt();
            let s = (dot(e, query) - mq) / norm;
            if s >= min {
                sims.push((row.get(0)?, s));
            }
        }
    }
    sims.sort_by(|a, b| b.1.total_cmp(&a.1));
    sims.truncate(limit);
    Ok(sims)
}

/// If there is already an almost identical memory in the same scope (the project and the globals, or only the
/// globals if the new one is global; all if `project` is a general folder), returns it with its
/// similarity.
pub fn duplicate(db: &Connection, model: &Model, project: &str, title: &str, content: &str) -> Result<Option<(Memory, f32)>> {
    let new = text(title, content);
    let scope = Scope::from_project(db, project)?;
    let v = similar_in(db, model, &new, &scope, None, DUPLICATE_SIMILARITY_WITH_WORDS, 5)?;
    the_duplicate(db, &scope, &new, v)
}

/// Like [`duplicate`], for a trade memory (`trades`, without the prefix): from global the trade ones
/// aren't seen (no project of the scope has trades), so it compares with the globals and with
/// the trade ones that share any of its trades, whoever they are for (`trade-for:`). That's what
/// happens when promoting the same thing from two clients. `exclude`: the memory it comes from (when promoting).
///
/// It only searches among those: before, the 50 most similar of the whole database were taken and then
/// filtered, and with many similar project memories the real duplicate was left out. The
/// words weigh with the IDF of the whole database, as in [`duplicates_among`].
pub fn trade_duplicate(db: &Connection, model: &Model, trades: &[String], title: &str, content: &str, exclude: Option<&str>) -> Result<Option<(Memory, f32)>> {
    let new = text(title, content);
    let mut candidates: Vec<i64> = Vec::new();
    {
        let mut q = db.prepare("SELECT m.n, m.id, m.tags FROM memory m WHERE m.project_id = 0 AND m.space_id IS NULL")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(1)?;
            let tags: Vec<String> = serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default();
            let its_trades = crate::trades::from_tags(&tags);
            if Some(id.as_str()) != exclude && (its_trades.is_empty() || its_trades.iter().any(|o| trades.contains(o))) {
                candidates.push(row.get(0)?);
            }
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let list = candidates.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let v = similar_where(db, model, &new, &format!("m.n IN ({list})"), DUPLICATE_SIMILARITY_WITH_WORDS, 5)?;
    the_duplicate(db, &Scope::All, &new, v)
}

/// The most similar to `text` among the memories that meet `condition` (SQL over `memory m`),
/// comparing with all their vectors: for a set that isn't a scope.
fn similar_where(db: &Connection, model: &Model, text: &str, condition: &str, min: f32, limit: usize) -> Result<Vec<(Memory, f32)>> {
    let summary = up_to_date_summary(db)?.filter(|r| r.dim == model.dim() || r.count == 0);
    let mean = match &summary {
        Some(r) if r.count == 0 => return Ok(Vec::new()),
        Some(r) => mean_from_summary(db, model, r)?,
        None => centering_mean(db, model)?,
    };
    let query = normalized(model.vector(text)?.iter().zip(&mean).map(|(x, m)| x - m).collect());
    let sims = linear_where(db, &query, &mean, condition, None, min, limit)?;
    let mut q = db.prepare(&format!("SELECT {COLUMNS} FROM memory m WHERE m.n = ?1"))?;
    sims.into_iter().map(|(n, s)| Ok((q.query_row([n], from_row)?, s))).collect()
}

/// Of the similar ones (from most to least), the duplicate: the first if it reaches `DUPLICATE_SIMILARITY`
/// and, if not, the one that best covers the new one's words (at least `DUPLICATE_COVERAGE`, with the IDF of
/// `scope`).
fn the_duplicate(db: &Connection, scope: &Scope, new: &str, mut v: Vec<(Memory, f32)>) -> Result<Option<(Memory, f32)>> {
    if v.first().is_some_and(|x| x.1 >= DUPLICATE_SIMILARITY) {
        return Ok(Some(v.swap_remove(0)));
    }
    if v.is_empty() {
        return Ok(None);
    }
    let coverages = crate::recall::coverage_in(db, scope, new, v.iter().map(|x| &x.0))?;
    let best = coverages.iter().enumerate().filter(|(_, c)| **c >= DUPLICATE_COVERAGE).max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i);
    Ok(best.map(|i| v.swap_remove(i)))
}

/// The mean the vectors are centered with, without the model: the database's (from the summary if there is one) with
/// the generic one stored in `meta` when the model was used. `None` without vectors.
fn stored_mean(db: &Connection) -> Result<Option<Vec<f32>>> {
    let (mut sum, mut n): (Vec<f64>, f64) = match read_summary(db)? {
        Some(r) if r.count > 0 => (r.sum, r.count as f64),
        _ => {
            let mut sum: Vec<f64> = Vec::new();
            let mut n = 0.0;
            let mut q = db.prepare("SELECT e FROM memory_vec")?;
            let mut rows = q.query([])?;
            while let Some(row) = rows.next()? {
                let e = to_f32(row.get_ref(0)?.as_blob()?);
                if sum.is_empty() {
                    sum = vec![0.0; e.len()];
                }
                if e.len() == sum.len() {
                    sum.iter_mut().zip(&e).for_each(|(s, x)| *s += f64::from(*x));
                    n += 1.0;
                }
            }
            (sum, n)
        }
    };
    if n == 0.0 {
        return Ok(None);
    }
    let model: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |row| row.get(0)).optional()?;
    let generic: Option<Vec<f32>> = match model {
        Some(m) => db
            .query_row("SELECT v FROM meta WHERE k = ?1", [format!("media:{m}")], |row| row.get::<_, String>(0))
            .optional()?
            .and_then(|t| serde_json::from_str(&t).ok()),
        None => None,
    };
    if let Some(g) = generic.filter(|g| g.len() == sum.len()) {
        sum.iter_mut().zip(&g).for_each(|(s, x)| *s += f64::from(*x) * f64::from(GENERIC_MEAN_WEIGHT));
        n += f64::from(GENERIC_MEAN_WEIGHT);
    }
    Ok(Some(sum.iter().map(|s| (s / n) as f32).collect()))
}

/// The pairs of `memories` (by position, `i < j`) that [`duplicate`] would have taken as duplicates
/// when saving the second, with their similarity: with the vectors already stored, without loading the model (for
/// the review). Only those of the same group in `groups` are compared (positions in `memories`: those of
/// each trade), and each pair comes out once even if they share several. The newer one has the say
/// against the older, as when saving. A memory without a vector (without a model) is compared only by
/// words, and then both have to cover each other (`DUPLICATE_COVERAGE` in both
/// directions).
///
/// Words weigh with the IDF of the whole database (`Scope::All`): trade ones aren't seen from
/// global, and with that scope (what was done before) a database with only trade memories gave negative
/// IDF and none covered any. The words, their weights and each memory's centered vector
/// are computed once, not once per pair: with 42 trade memories without a model, the review took
/// 16.6 s counting the same words in the index for each pair.
///
/// With `only`, only the pairs involving one of those (the ones that changed since the last
/// computation): the others were already judged then.
pub fn duplicates_among(db: &Connection, memories: &[&Memory], groups: &[Vec<usize>], only: Option<&std::collections::HashSet<usize>>) -> Result<Vec<(usize, usize, f32)>> {
    use std::collections::{BTreeSet, HashMap};
    let counts = |i: &usize| only.is_none_or(|s| s.contains(i));
    let groups: Vec<&Vec<usize>> = groups.iter().filter(|g| g.len() >= 2 && g.iter().any(counts)).collect();
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let mean = stored_mean(db)?;
    let mut q = db.prepare("SELECT v.e FROM memory_vec v JOIN memory m ON m.n = v.n WHERE m.id = ?1")?;
    let mut frequencies = crate::recall::Frequencies::new(db, &Scope::All)?;
    // Each memory's data, computed the first time it's needed.
    struct PerMemory {
        centered: Option<Vec<f32>>,
        weights: crate::recall::Weights,
        tokens: BTreeSet<String>,
    }
    let mut per_memory: HashMap<usize, PerMemory> = HashMap::new();
    let mut r = Vec::new();
    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    for g in groups {
        for &i in g {
            if per_memory.contains_key(&i) {
                continue;
            }
            let m = memories[i];
            let e: Option<Vec<u8>> = q.query_row([&m.id], |row| row.get(0)).optional()?;
            let centered = match (e, &mean) {
                (Some(e), Some(mu)) => {
                    let e = to_f32(&e);
                    (e.len() == mu.len()).then(|| normalized(e.iter().zip(mu).map(|(x, m)| x - m).collect()))
                }
                _ => None,
            };
            let weights = crate::recall::Weights::of(&text(&m.title, &m.content), &mut frequencies)?;
            per_memory.insert(i, PerMemory { centered, weights, tokens: crate::recall::words_of(m) });
        }
        for (x, &i) in g.iter().enumerate() {
            for &j in &g[x + 1..] {
                if i == j || !(counts(&i) || counts(&j)) || !seen.insert((i.min(j), i.max(j))) {
                    continue;
                }
                // The new one against the old one.
                let (new, old) = if (memories[i].created_at.as_str(), memories[i].id.as_str()) >= (memories[j].created_at.as_str(), memories[j].id.as_str()) { (i, j) } else { (j, i) };
                let covers = |from: usize, to: usize| per_memory[&from].weights.coverage(&per_memory[&to].tokens);
                let is_dup = match (&per_memory[&i].centered, &per_memory[&j].centered) {
                    (Some(a), Some(b)) => {
                        let sim: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
                        let dup = sim >= DUPLICATE_SIMILARITY || (sim >= DUPLICATE_SIMILARITY_WITH_WORDS && covers(new, old) >= DUPLICATE_COVERAGE);
                        dup.then_some(sim)
                    }
                    _ => {
                        // The second only if the first reaches it.
                        let c = covers(new, old);
                        let c = if c >= DUPLICATE_COVERAGE { c.min(covers(old, new)) } else { c };
                        (c >= DUPLICATE_COVERAGE).then_some(c as f32)
                    }
                };
                if let Some(sim) = is_dup {
                    r.push((i.min(j), i.max(j), sim));
                }
            }
        }
    }
    Ok(r)
}

/// Memories that may be about the same as a just-saved one without being duplicates: those that
/// score best by BM25 with any word of the title and share at least two title words
/// with it (or the only one, if both titles have one). It blocks nothing: they are returned so
/// the agent decides whether to merge them. This also catches the 10 % of duplicates that `duplicate` lets
/// through, without false warnings upfront.
pub fn related(db: &Connection, project: &str, title: &str, exclude: &str, limit: usize) -> Result<Vec<Memory>> {
    let Some(query) = crate::search::fts_query(title) else { return Ok(Vec::new()) };
    let own: std::collections::BTreeSet<String> = crate::recall::useful_terms(title).into_iter().collect();
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNS} FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1 AND {} AND m.id != ?2
         ORDER BY bm25(memory_fts) LIMIT 30",
        Scope::from_project(db, project)?.sql()
    ))?;
    let candidates: Vec<Memory> = q.query_map(params![query, exclude], from_row)?.collect::<rusqlite::Result<_>>()?;
    // Also tried requiring a minimum vector similarity: each point less of noise cost
    // uncovered duplicates (with 0.15, from 100 % to 98 %), so it isn't required.
    Ok(candidates
        .into_iter()
        .filter(|m| {
            let theirs: std::collections::BTreeSet<String> = crate::recall::useful_terms(&m.title).into_iter().collect();
            let common = own.intersection(&theirs).count();
            common >= 2 || (common == 1 && own.len() == 1 && theirs.len() == 1)
        })
        .take(limit)
        .collect())
}

/// For `doctor`: which model the vectors were computed with and how many there are.
pub fn status(db: &Connection) -> Result<(Option<String>, i64)> {
    let model = db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |row| row.get(0)).optional()?;
    let n = db.query_row("SELECT count(*) FROM memory_vec", [], |row| row.get(0))?;
    Ok((model, n))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{self, Changes, NewMemory};

    /// Repeatable pseudo-random numbers (xorshift), so as not to depend on anything.
    struct Rng(u64);

    impl Rng {
        fn next_f32(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }

        /// A norm-1 vector near one of the `centers`: similarities of all kinds, like the
        /// real ones (some very close, most far).
        fn vector(&mut self, centers: &[Vec<f32>]) -> Vec<f32> {
            let c = &centers[(self.next_f32().abs() * centers.len() as f32) as usize % centers.len()];
            normalized(c.iter().map(|x| x + 0.08 * self.next_f32()).collect())
        }
    }

    const DIM: usize = 256;

    /// The database in its temporary folder, deleted at the end (the folder goes first: it is dropped later).
    fn database(name: &str) -> (crate::common::TempDir, Connection) {
        let dir = crate::common::TempDir::new(&format!("resumen-{name}"));
        let db = crate::db::open(&dir.join("o.db")).unwrap();
        (dir, db)
    }

    fn save_memory(db: &mut Connection, project: &str, kind: Kind, i: usize) -> (String, i64) {
        let n = NewMemory { project: project.into(), kind: Some(kind), title: format!("nota {i}"), content: format!("texto {i}"), ..Default::default() };
        let m = memory::save(db, n, None).unwrap();
        let n: i64 = db.query_row("SELECT n FROM memory WHERE id = ?1", [&m.id], |row| row.get(0)).unwrap();
        (m.id, n)
    }

    fn set_vector(db: &Connection, n: i64, v: &[f32]) {
        db.execute("INSERT OR REPLACE INTO memory_vec(n, e) VALUES (?1, ?2)", params![n, bytes(v)]).unwrap();
    }

    fn blocks(db: &Connection) -> Vec<(i64, Vec<u8>, Vec<u8>)> {
        let mut q = db.prepare("SELECT b, ns, datos FROM vec_bloque ORDER BY b").unwrap();
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn count(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// A database with memories of two projects, globals and of two kinds, with random vectors.
    fn with_vectors(name: &str, how_many: usize, rng: &mut Rng) -> (crate::common::TempDir, Connection, Vec<Vec<f32>>) {
        let (dir, mut db) = database(name);
        let centers: Vec<Vec<f32>> = (0..12).map(|_| normalized((0..DIM).map(|_| rng.next_f32()).collect())).collect();
        for i in 0..how_many {
            let project = ["/p1", "/p1", "/p2", ""][i % 4];
            let kind = if i % 3 == 0 { Kind::Gotcha } else { Kind::Fact };
            let (_, n) = save_memory(&mut db, project, kind, i);
            set_vector(&db, n, &rng.vector(&centers));
        }
        (dir, db, centers)
    }

    #[test]
    fn the_summary_gives_the_same_as_comparing_with_all() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let (_dir, db, centers) = with_vectors("igual", 1200, &mut rng);
        let r = up_to_date_summary(&db).unwrap().expect("resumen");
        assert_eq!(r.count, 1200);
        let mean: Vec<f32> = r.sum.iter().map(|s| (*s / r.count as f64) as f32).collect();
        let scopes = [Scope::All, Scope::from_project(&db, "/p1").unwrap(), Scope::from_project(&db, "/p2").unwrap(), Scope::Space(None)];
        let (mut non_empty, mut cut_off) = (0, 0);
        for _ in 0..12 {
            let query = normalized(rng.vector(&centers).iter().zip(&mean).map(|(x, m)| x - m).collect());
            for scope in &scopes {
                for kind in [None, Some(Kind::Gotcha)] {
                    for (min, limit) in [(-1.0, 40), (-1.0, 1), (0.1, 5), (0.35, 2), (0.6, 3), (0.9, 2)] {
                        let a = linear(&db, &query, &mean, scope, kind, min, limit).unwrap();
                        let b = with_summary(&db, &query, &mean, scope, kind, min, limit, DIM).unwrap();
                        assert_eq!(a, b, "{scope:?} {kind:?} {min} {limit}");
                        non_empty += usize::from(!a.is_empty());
                        cut_off += usize::from(a.len() == limit && min > 0.0);
                    }
                }
            }
        }
        // Make sure both cuts are really tested: by `min` and by `limit`.
        assert!(non_empty > 300 && cut_off > 50, "{non_empty} {cut_off}");
    }

    #[test]
    fn the_summary_follows_the_changes() {
        let mut rng = Rng(0x2545f4914f6cdd1d);
        let (_dir, mut db, centers) = with_vectors("cambios", 300, &mut rng);
        up_to_date_summary(&db).unwrap().expect("resumen");
        // A vector changed with `INSERT OR REPLACE` (as when editing), another with `UPDATE`, one deleted by
        // hand, forgotten memories (their vectors go in cascade), a new one and another without a vector.
        let ns: Vec<i64> = {
            let mut q = db.prepare("SELECT n FROM memory_vec ORDER BY n").unwrap();
            q.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        set_vector(&db, ns[3], &rng.vector(&centers));
        set_vector(&db, ns[3], &rng.vector(&centers));
        db.execute("UPDATE memory_vec SET e = ?2 WHERE n = ?1", params![ns[7], bytes(&rng.vector(&centers))]).unwrap();
        db.execute("DELETE FROM memory_vec WHERE n = ?1", [ns[11]]).unwrap();
        for i in [20, 21, 299] {
            let id: String = db.query_row("SELECT id FROM memory WHERE n = ?1", [ns[i]], |row| row.get(0)).unwrap();
            memory::forget(&mut db, &id).unwrap();
        }
        let (_, new) = save_memory(&mut db, "/p2", Kind::Fact, 1000);
        set_vector(&db, new, &rng.vector(&centers));
        save_memory(&mut db, "/p1", Kind::Fact, 1001);
        // The new one reuses the `n` of the last forgotten one: a single record, with the previous vector.
        assert_eq!(count(&db, "SELECT count(*) FROM vec_cambio"), 6);
        let r = up_to_date_summary(&db).unwrap().expect("resumen");
        assert_eq!(count(&db, "SELECT count(*) FROM vec_cambio"), 0);
        assert_eq!(missing_count(&db).unwrap(), 2, "la del vector borrado y la nueva sin vector");
        let incremental = blocks(&db);
        // The same as rebuilding it fully.
        let full = rebuild(&db).unwrap().expect("resumen");
        assert_eq!(r.count, full.count);
        assert_eq!(r.count, count(&db, "SELECT count(*) FROM memory_vec"));
        for (a, b) in r.sum.iter().zip(&full.sum) {
            assert!((a - b).abs() < 1e-9, "{a} {b}");
        }
        assert_eq!(incremental, blocks(&db));
    }

    #[test]
    fn without_the_local_tables_compares_with_all() {
        let mut rng = Rng(7);
        let (_dir, db, _) = with_vectors("sin-locales", 20, &mut rng);
        db.execute_batch("DROP TABLE vec_bloque").unwrap();
        assert!(up_to_date_summary(&db).unwrap().is_none());
        assert_eq!(missing_count(&db).unwrap(), 0);
    }

    #[test]
    fn the_counts_per_scope_add_up() {
        let (_dir, mut db) = database("cuentas");
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(save_memory(&mut db, ["/p1", "/p2", "", "/p3"][i % 4], Kind::Fact, i).0);
        }
        memory::forget(&mut db, &ids[0]).unwrap();
        memory::forget(&mut db, &ids[2]).unwrap();
        let move_to = Changes { project: Some("/p2".into()), ..Default::default() };
        memory::update(&mut db, &ids[5], move_to, None).unwrap();
        let global = Changes { project: Some(String::new()), ..Default::default() };
        memory::update(&mut db, &ids[9], global, None).unwrap();
        let scopes = [Scope::All, Scope::from_project(&db, "/p1").unwrap(), Scope::from_project(&db, "/p2").unwrap(), Scope::from_project(&db, "/p3").unwrap(), Scope::Space(None)];
        for scope in scopes {
            let counted = count(&db, &format!("SELECT count(*) FROM memory m WHERE {}", scope.sql()));
            assert_eq!(memory::count(&db, &scope).unwrap(), counted, "{scope:?}");
        }
        assert!(memory::sees_all(&db, &Scope::All));
        assert!(!memory::sees_all(&db, &Scope::from_project(&db, "/p1").unwrap()));
    }

    /// If a piece is missing (a database another version touched, or a migration that rebuilt `memory`), on
    /// opening all are rebuilt and recounted; the summary will be rebuilt when used.
    #[test]
    fn local_pieces_are_rebuilt_if_any_is_missing() {
        let mut rng = Rng(11);
        let (_dir, mut db, centers) = with_vectors("rehacer", 30, &mut rng);
        up_to_date_summary(&db).unwrap().expect("resumen");
        let path = std::path::PathBuf::from(db.path().unwrap());
        db.execute_batch("DROP TRIGGER memory_cuenta_mas; DROP TRIGGER vec_cambio_pon;").unwrap();
        let (_, n) = save_memory(&mut db, "/p1", Kind::Fact, 99);
        set_vector(&db, n, &rng.vector(&centers));
        drop(db);
        let db = crate::db::open(&path).unwrap();
        assert_eq!(memory::count(&db, &Scope::All).unwrap(), 31);
        assert!(read_summary(&db).unwrap().is_none());
        assert_eq!(up_to_date_summary(&db).unwrap().expect("resumen").count, 31);
    }
}
