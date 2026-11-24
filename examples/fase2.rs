//! Preparing phase 2: measure before deciding.
//!
//!   cargo run --release --example fase2 -- duplicados   does cosine separate duplicates from similar topics?
//!   cargo run --release --example fase2 -- vectores N   vec0 versus BLOB + vec_distance_cosine, filtering by project
//!   cargo run --release --example fase2 -- busqueda     search accuracy with tests/busquedas.tsv by threshold
//!   cargo run --release --example fase2 -- dupreal DIR  the duplicate warning, with the production code and real notes

use anyhow::{Result, anyhow};
use omoikane::embeber::Modelo;
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("duplicados") => duplicates(),
        Some("vectores") => vectors(args.get(1).map_or(Ok(5000), |n| n.parse())?),
        Some("busqueda") => search(),
        Some("variantes") => variants(),
        Some("centrar") => center(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("real") => real(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupprod") => dupprod(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupcob") => dupcob(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupreal") => dupreal(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        _ => Err(anyhow!("uso: fase2 duplicados | vectores N | busqueda")),
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Lowercase words without accents, minus the most common stop words.
fn words(s: &str) -> std::collections::BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "al", "con", "de", "del", "el", "en", "es", "la", "las", "lo", "los", "no", "para", "por", "que", "se", "si",
        "su", "un", "una", "y", "the", "of", "and", "to", "in", "is", "it", "for", "on", "be", "a", "an", "or", "from",
    ];
    let s: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            c => c,
        })
        .collect();
    s.split(|c: char| !c.is_alphanumeric()).filter(|p| p.len() > 1 && !STOP_WORDS.contains(p)).map(String::from).collect()
}

fn jaccard(a: &str, b: &str) -> f32 {
    let (a, b) = (words(a), words(b));
    let i = a.intersection(&b).count() as f32;
    let u = a.union(&b).count() as f32;
    if u == 0.0 { 0.0 } else { i / u }
}

/// The best threshold to separate `dup` from `rel` and its accuracy (over dup + rel).
fn best_threshold(v: &[(String, f32)]) -> (f32, f32) {
    let mut best = (0.0, 0.0);
    for i in 0..=200 {
        let u = i as f32 / 200.0;
        let right = v
            .iter()
            .filter(|(c, _)| c != "otro")
            .filter(|(c, s)| (c == "dup") == (*s >= u))
            .count() as f32;
        let total = v.iter().filter(|(c, _)| c != "otro").count() as f32;
        if right / total > best.1 {
            best = (u, right / total);
        }
    }
    best
}

fn summary(name: &str, v: &[(String, f32)]) {
    println!("\n{name}");
    for class in ["dup", "rel", "otro"] {
        let mut x: Vec<f32> = v.iter().filter(|(c, _)| c == class).map(|(_, s)| *s).collect();
        x.sort_by(f32::total_cmp);
        println!(
            "  {class:<5} n={:<3} min {:.3}  mediana {:.3}  max {:.3}",
            x.len(),
            x[0],
            x[x.len() / 2],
            x[x.len() - 1]
        );
    }
    let (u, accuracy) = best_threshold(v);
    println!("  mejor umbral dup/rel: {u:.3} → acierto {:.0} %", accuracy * 100.0);
}

fn duplicates() -> Result<()> {
    let dir = std::env::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi");
    let m = Modelo::abrir(&dir)?;
    let pairs: Vec<(String, String, String)> = include_str!("../tests/pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].to_string(), c[1].to_string(), c[2].to_string())
        })
        .collect();
    let mut cos = Vec::new();
    let mut jac = Vec::new();
    let mut mix = Vec::new();
    for (class, a, b) in &pairs {
        let c = cosine(&m.vector(a)?, &m.vector(b)?);
        let j = jaccard(a, b);
        cos.push((class.clone(), c));
        jac.push((class.clone(), j));
        mix.push((class.clone(), 0.5 * c + 0.5 * j));
    }
    summary("coseno (model2vec int8)", &cos);
    summary("jaccard de palabras", &jac);
    summary("0,5·coseno + 0,5·jaccard", &mix);
    println!("\nlos dup con menos coseno:");
    let mut d: Vec<_> = pairs.iter().zip(&cos).filter(|(p, _)| p.0 == "dup").collect();
    d.sort_by(|a, b| a.1.1.total_cmp(&b.1.1));
    for (p, (_, c)) in d.iter().take(4) {
        println!("  {c:.3}  «{}» / «{}»", p.1, p.2);
    }
    println!("los rel con más coseno:");
    let mut r: Vec<_> = pairs.iter().zip(&cos).filter(|(p, _)| p.0 == "rel").collect();
    r.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
    for (p, (_, c)) in r.iter().take(4) {
        println!("  {c:.3}  «{}» / «{}»", p.1, p.2);
    }
    Ok(())
}

struct Xs(u64);
impl Xs {
    fn n(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn vector(&mut self) -> Vec<u8> {
        let v: Vec<f32> = (0..256).map(|_| (self.n() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0).collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().flat_map(|x| (x / n).to_le_bytes()).collect()
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// N memories spread over 10 projects plus 10 % global. Finds the 40 nearest of
/// project 3 and the global ones, two ways, interleaved.
fn vectors(n: usize) -> Result<()> {
    omoikane::db::registrar_sqlite_vec();
    let path = std::env::temp_dir().join(format!("omoi-fase2-{}.db", std::process::id()));
    let mut db = rusqlite::Connection::open(&path)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE memory (n INTEGER PRIMARY KEY, project TEXT NOT NULL);
         CREATE INDEX memory_project ON memory(project);
         CREATE VIRTUAL TABLE v0 USING vec0(e float[256] distance_metric=cosine);
         CREATE TABLE vb (n INTEGER PRIMARY KEY, e BLOB NOT NULL);",
    )?;
    let mut r = Xs(0x9E3779B97F4A7C15);
    let tx = db.transaction()?;
    for i in 1..=n as i64 {
        let p = if r.n().is_multiple_of(10) { String::new() } else { format!("/p{}", r.n() % 10) };
        let e = r.vector();
        tx.execute("INSERT INTO memory VALUES (?1, ?2)", rusqlite::params![i, p])?;
        tx.execute("INSERT INTO v0(rowid, e) VALUES (?1, ?2)", rusqlite::params![i, e])?;
        tx.execute("INSERT INTO vb VALUES (?1, ?2)", rusqlite::params![i, e])?;
    }
    tx.commit()?;
    let in_scope: i64 = db.query_row("SELECT count(*) FROM memory WHERE project IN ('/p3', '')", [], |row| row.get(0))?;
    println!("{n} memorias, {in_scope} en el ámbito (/p3 + globales)");

    let queries: Vec<Vec<u8>> = (0..30).map(|_| r.vector()).collect();
    // A: vec0 with no filter (it can't filter by project in the KNN), asking for extra and filtering afterwards.
    let mut a = db.prepare(
        "WITH k AS (SELECT rowid, distance FROM v0 WHERE e MATCH ?1 AND k = 400)
         SELECT k.rowid FROM k JOIN memory m ON m.n = k.rowid WHERE m.project IN ('/p3', '') ORDER BY k.distance LIMIT 40",
    )?;
    // B: plain BLOB and the distance function, with the exact filter in the same SQL.
    let mut b = db.prepare(
        "SELECT m.n FROM vb JOIN memory m ON m.n = vb.n WHERE m.project IN ('/p3', '')
         ORDER BY vec_distance_cosine(vb.e, ?1) LIMIT 40",
    )?;
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    let mut same = 0;
    for q in &queries {
        let t = Instant::now();
        let ra: Vec<i64> = a.query_map([q], |row| row.get(0))?.collect::<Result<_, _>>()?;
        ta.push(ms(t));
        let t = Instant::now();
        let rb: Vec<i64> = b.query_map([q], |row| row.get(0))?.collect::<Result<_, _>>()?;
        tb.push(ms(t));
        if ra == rb {
            same += 1;
        }
    }
    println!("A vec0 k=400 y filtrar después   mediana {:.2} ms", median(ta));
    println!("B BLOB + vec_distance_cosine     mediana {:.2} ms", median(tb));
    println!("mismos 40 resultados en {same} de {} consultas", queries.len());
    drop(a);
    drop(b);
    drop(db);
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
    Ok(())
}

fn model() -> Result<Modelo> {
    Modelo::abrir(&std::env::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi"))
}

/// Saves the memories of tests/busquedas.tsv and measures, for each threshold, how many queries bring their
/// memory first or among the top three, and how much noise the ones with no answer return.
fn search() -> Result<()> {
    use omoikane::buscar::{Opciones, buscar_con};
    use omoikane::memoria::{self, Ambito, Nueva, Tipo};
    let m = model()?;
    let dir = std::env::temp_dir().join(format!("omoi-fase2-busqueda-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = omoikane::db::abrir(&dir.join("o.db"))?;
    let mut keys = std::collections::HashMap::new();
    let mut queries: Vec<(String, String)> = Vec::new();
    for l in include_str!("../tests/busquedas.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        match c[0] {
            "m" => {
                let n = Nueva {
                    project: "/p".into(),
                    kind: Some(Tipo::Fact),
                    title: c[2].into(),
                    content: c[3].into(),
                    ..Default::default()
                };
                let mem = memoria::guardar(&mut db, n, Some(&m))?;
                keys.insert(mem.id, c[1].to_string());
            }
            _ => queries.push((c[1].to_string(), c[2].to_string())),
        }
    }
    let answered = queries.iter().filter(|(k, _)| k != "-").count();
    println!("{} memorias, {} consultas con respuesta, {} sin respuesta
", keys.len(), answered, queries.len() - answered);
    println!("{:<18} {:>7} {:>7} {:>6}   {:>16} {:>14}", "modo", "top-1", "top-3", "MRR", "sin resp. con algo", "ruido medio");
    let configs = configs();
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    for (name, with_model, u, fusion) in &configs {
        let (mut top1, mut top3, mut mrr, mut with_any, mut noise) = (0, 0, 0.0, 0, 0);
        for (key, q) in &queries {
            let r = buscar_con(&mut db, q, &o, with_model.then_some(&m), *u, *fusion)?;
            let ranked: Vec<&String> = r.iter().map(|r| &keys[&r.memoria.id]).collect();
            if key == "-" {
                if std::env::var_os("DETALLE").is_some() && !ranked.is_empty() {
                    println!("    {name}: sin respuesta «{q}» → {:?}", ranked);
                }
                with_any += usize::from(!ranked.is_empty());
                noise += ranked.len();
                continue;
            }
            if std::env::var_os("DETALLE").is_some() && ranked.first().map(|k| *k != key).unwrap_or(true) {
                println!("    {name}: «{q}» → {:?} (esperada {key})", ranked.iter().take(3).collect::<Vec<_>>());
            }
            if let Some(i) = ranked.iter().position(|k| *k == key) {
                top1 += usize::from(i == 0);
                top3 += usize::from(i < 3);
                mrr += 1.0 / (i as f64 + 1.0);
            }
        }
        let unanswered = queries.len() - answered;
        println!(
            "{name:<18} {:>6.0}% {:>6.0}% {:>6.2}   {:>11} de {unanswered} {:>14.1}",
            100.0 * top1 as f64 / answered as f64,
            100.0 * top3 as f64 / answered as f64,
            mrr / answered as f64,
            with_any,
            noise as f64 / unanswered as f64
        );
    }
    // Similarity of the right memory and of the best that comes up in queries with no answer.
    println!("
similitud (coseno) consulta → memoria esperada, y lo más alto en las sin respuesta:");
    let mut expected = Vec::new();
    let mut unrelated = Vec::new();
    for (key, q) in &queries {
        let p = omoikane::vectores::parecidas(&db, &m, q, None, None, -1.0, 50)?;
        if key == "-" {
            unrelated.push(p.first().map_or(0.0, |x| x.1));
        } else if let Some((_, s)) = p.iter().find(|(mem, _)| &keys[&mem.id] == key) {
            expected.push((*s, q.clone()));
        }
    }
    expected.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (s, q) in expected.iter().take(6) {
        println!("  {s:.3}  «{q}»");
    }
    println!("  … mediana {:.3}, máx {:.3}", expected[expected.len() / 2].0, expected.last().unwrap().0);
    unrelated.sort_by(f32::total_cmp);
    println!("  sin respuesta, lo más parecido: {:?}", unrelated.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Which text is best to vectorize? For each query with an answer: the rank of the right memory
/// by similarity with title+content, title only, content only and the maximum.
fn variants() -> Result<()> {
    let m = model()?;
    let mut mems: Vec<(String, String, String)> = Vec::new();
    let mut queries = Vec::new();
    for l in include_str!("../tests/busquedas.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        match c[0] {
            "m" => mems.push((c[1].into(), c[2].into(), c[3].into())),
            _ => queries.push((c[1].to_string(), c[2].to_string())),
        }
    }
    let vt: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&x.1)).collect::<Result<_>>()?;
    let vc: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&x.2)).collect::<Result<_>>()?;
    let va: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&format!("{}
{}", x.1, x.2))).collect::<Result<_>>()?;
    let names = ["título+contenido", "solo título", "solo contenido", "máx(título, t+c)"];
    let mut top1 = [0usize; 4];
    let mut noise = [0f32; 4];
    for (key, q) in &queries {
        let vq = m.vector(q)?;
        let sims: Vec<[f32; 4]> = (0..mems.len())
            .map(|i| {
                let (a, t, c) = (cosine(&vq, &va[i]), cosine(&vq, &vt[i]), cosine(&vq, &vc[i]));
                [a, t, c, a.max(t)]
            })
            .collect();
        for k in 0..4 {
            let best = (0..mems.len()).max_by(|&a, &b| sims[a][k].total_cmp(&sims[b][k])).unwrap();
            if key == "-" {
                noise[k] = noise[k].max(sims[best][k]);
            } else if mems[best].0 == *key {
                top1[k] += 1;
            }
        }
    }
    let n = queries.iter().filter(|(k, _)| k != "-").count();
    for k in 0..4 {
        println!("{:<20} top-1 solo vector {:>3.0}%   máx. similitud en las sin respuesta {:.3}", names[k], 100.0 * top1[k] as f64 / n as f64, noise[k]);
    }
    Ok(())
}

/// Calibration with real notes (not committed to the repo): a folder of Claude Code memories, with
/// one `.md` per note (frontmatter `name`/`description` and body) and a `MEMORY.md` with one line per
/// note: `- [Title](file.md) — summary`. The summary is the query; its note, the answer.
/// It also looks at every pair of different notes: any duplicate warning is a false positive.
fn real(dir: &std::path::Path) -> Result<()> {
    use omoikane::buscar::{Opciones, buscar_con};
    use omoikane::memoria::{self, Ambito, Nueva, Tipo};
    let m = model()?;
    let mut notes: Vec<(String, String, String)> = Vec::new(); // (file, title, body)
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let (header, body) = match t.strip_prefix("---") {
            Some(r) => match r.find("
---") {
                Some(i) => (&r[..i], r[i + 4..].trim()),
                None => ("", t.as_str()),
            },
            None => ("", t.as_str()),
        };
        let title = header
            .lines()
            .find_map(|l| l.trim().strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| f.trim_end_matches(".md").replace(['_', '-'], " "));
        let title: String = title.chars().take(200).collect();
        if !body.is_empty() {
            notes.push((f, title, body.to_string()));
        }
    }
    let index = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut queries: Vec<(String, String)> = Vec::new(); // (expected file, query)
    for l in index.lines() {
        let (Some(i), Some(j)) = (l.find("]("), l.find(".md)")) else { continue };
        let file = format!("{}.md", &l[i + 2..j]);
        if let Some(k) = l.find(" — ") {
            let summary = l[k + " — ".len()..].trim().trim_end_matches('…').to_string();
            if notes.iter().any(|n| n.0 == file) && summary.len() > 10 {
                queries.push((file, summary));
            }
        }
    }
    println!("{} notas, {} consultas (resúmenes del índice)", notes.len(), queries.len());

    // Duplicates: every pair of different notes.
    let vs: Vec<Vec<f32>> = notes.iter().map(|n| m.vector(&omoikane::vectores::texto(&n.1, &n.2))).collect::<Result<_>>()?;
    let mut pairs = Vec::new();
    for i in 0..vs.len() {
        for j in i + 1..vs.len() {
            pairs.push((cosine(&vs[i], &vs[j]), i, j));
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    let total = pairs.len();
    for u in [0.60f32, 0.65, 0.70, 0.75, 0.80] {
        let n = pairs.iter().filter(|p| p.0 >= u).count();
        println!("pares de notas distintas con coseno ≥ {u:.2}: {n} de {total}");
    }
    println!("los más parecidos (revisar si son duplicados de verdad):");
    for (s, i, j) in pairs.iter().take(8) {
        println!("  {s:.3}  {} / {}", notes[*i].0, notes[*j].0);
    }

    // Search.
    let tmp = std::env::temp_dir().join(format!("omoi-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut file_of = std::collections::HashMap::new();
    for (f, t, c) in &notes {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        file_of.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    println!("
{:<18} {:>7} {:>7} {:>6}", "modo", "top-1", "top-3", "MRR");
    let configs = configs();
    for (name, with_model, u, fusion) in &configs {
        let (mut t1, mut t3, mut mrr) = (0, 0, 0.0);
        for (f, q) in &queries {
            let r = buscar_con(&mut db, q, &o, with_model.then_some(&m), *u, *fusion)?;
            if let Some(i) = r.iter().position(|r| &file_of[&r.memoria.id] == f) {
                t1 += usize::from(i == 0);
                t3 += usize::from(i < 3);
                mrr += 1.0 / (i as f64 + 1.0);
            }
        }
        let n = queries.len() as f64;
        println!("{name:<18} {:>6.0}% {:>6.0}% {:>6.2}", 100.0 * t1 as f64 / n, 100.0 * t3 as f64 / n, mrr / n);
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// Subtracts the mean vector and renormalizes.
fn centered(vs: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let d = vs[0].len();
    let mut mean = vec![0f32; d];
    for v in vs {
        for (m, x) in mean.iter_mut().zip(v) {
            *m += x / vs.len() as f32;
        }
    }
    vs.iter()
        .map(|v| {
            let c: Vec<f32> = v.iter().zip(&mean).map(|(x, m)| x - m).collect();
            let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            c.iter().map(|x| x / n).collect()
        })
        .collect()
}

/// Does subtracting the collection's mean vector separate duplicates better? With the real notes of
/// `dir` (all different: whatever passes the threshold is a false positive) and with the calibration pairs.
fn center(dir: &std::path::Path) -> Result<()> {
    let m = model()?;
    // Real notes.
    let mut texts = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if f.ends_with(".md") && f != "MEMORY.md" {
            texts.push(std::fs::read_to_string(e.path())?);
        }
    }
    let vs: Vec<Vec<f32>> = texts.iter().map(|t| m.vector(t)).collect::<Result<_>>()?;
    let cs = centered(&vs);
    let pairs = |v: &[Vec<f32>]| -> Vec<f32> {
        let mut p = Vec::new();
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                p.push(cosine(&v[i], &v[j]));
            }
        }
        p
    };
    let (pb, pc) = (pairs(&vs), pairs(&cs));
    println!("{} notas reales, {} pares (todos distintos)", texts.len(), pb.len());
    for u in [0.5f32, 0.6, 0.65, 0.7, 0.8] {
        println!(
            "  ≥ {u:.2}: sin centrar {:>5}   centrados {:>5}",
            pb.iter().filter(|x| **x >= u).count(),
            pc.iter().filter(|x| **x >= u).count()
        );
    }
    // Calibration pairs: centered with the mean of their own sentences.
    let cal_pairs: Vec<(String, String, String)> = include_str!("../tests/pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].to_string(), c[1].to_string(), c[2].to_string())
        })
        .collect();
    let sentences: Vec<String> = cal_pairs.iter().flat_map(|p| [p.1.clone(), p.2.clone()]).collect();
    let fv: Vec<Vec<f32>> = sentences.iter().map(|t| m.vector(t)).collect::<Result<_>>()?;
    for (name, v) in [("sin centrar", fv.clone()), ("centrados", centered(&fv))] {
        let r: Vec<(String, f32)> = cal_pairs.iter().enumerate().map(|(i, p)| (p.0.clone(), cosine(&v[2 * i], &v[2 * i + 1]))).collect();
        summary(&format!("pares de calibración, {name}"), &r);
    }
    // Real notes centered with the notes' mean, calibration pairs centered with the same one.
    let d = vs[0].len();
    let mut mean = vec![0f32; d];
    for v in &vs {
        for (mm, x) in mean.iter_mut().zip(v) {
            *mm += x / vs.len() as f32;
        }
    }
    let with_mean = |v: &Vec<f32>| -> Vec<f32> {
        let c: Vec<f32> = v.iter().zip(&mean).map(|(x, mm)| x - mm).collect();
        let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        c.iter().map(|x| x / n).collect()
    };
    let r: Vec<(String, f32)> =
        cal_pairs.iter().enumerate().map(|(i, p)| (p.0.clone(), cosine(&with_mean(&fv[2 * i]), &with_mean(&fv[2 * i + 1])))).collect();
    summary("pares de calibración centrados con la media de las notas reales", &r);

    // And with a generic mean, fixed per model? The one from the tests' corpus (code, docs and sentences
    // in many languages), which has nothing to do with the notes.
    let generic: Vec<Vec<f32>> = include_str!("../tests/corpus.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| m.vector(l))
        .collect::<Result<_>>()?;
    let mut mg = vec![0f32; d];
    for v in &generic {
        for (mm, x) in mg.iter_mut().zip(v) {
            *mm += x / generic.len() as f32;
        }
    }
    let centered_by = |v: &Vec<f32>, mean: &[f32]| -> Vec<f32> {
        let c: Vec<f32> = v.iter().zip(mean).map(|(x, mm)| x - mm).collect();
        let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        c.iter().map(|x| x / n).collect()
    };
    let cg: Vec<Vec<f32>> = vs.iter().map(|v| centered_by(v, &mg)).collect();
    let pg = pairs(&cg);
    println!("
notas reales centradas con la media GENÉRICA del corpus de tests:");
    for u in [0.5f32, 0.6, 0.65, 0.7, 0.8] {
        println!("  ≥ {u:.2}: {:>5}", pg.iter().filter(|x| **x >= u).count());
    }
    let r: Vec<(String, f32)> =
        cal_pairs.iter().enumerate().map(|(i, p)| (p.0.clone(), cosine(&centered_by(&fv[2 * i], &mg), &centered_by(&fv[2 * i + 1], &mg)))).collect();
    summary("pares de calibración centrados con la media genérica", &r);
    Ok(())
}

fn configs() -> Vec<(String, bool, f32, omoikane::buscar::Fusion)> {
    use omoikane::buscar::Fusion;
    let mut v = vec![("solo palabras".to_string(), false, 0.0, Fusion::Rescate)];
    for u in [0.15f32, 0.20, 0.25, 0.30, 0.35] {
        v.push((format!("rrf 1.0 >= {u:.2}"), true, u, Fusion::Rrf { peso_vector: 1.0 }));
        v.push((format!("rrf 0.5 >= {u:.2}"), true, u, Fusion::Rrf { peso_vector: 0.5 }));
        v.push((format!("rescate >= {u:.2}"), true, u, Fusion::Rescate));
    }
    v
}

/// The duplicate warning as `vectores::parecidas` gives it (centering included), with real notes
/// from `dir` (those of `real`; not committed). Three measures:
/// - different: each note, before saving it, against those already saved. None is a duplicate of another,
///   so every warning is a false positive.
/// - re-saved: each note said again as an agent that doesn't see it would (the index summary
///   as title and the start of the body). It should warn, and about its original.
/// - the hand-written pairs of tests/pares_duplicados.tsv, in the same database.
fn dupreal(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{parecidas, texto};
    let m = model()?;
    let mut notes: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("
---") else { continue };
        let (header, body) = (&r[..i], r[i + 4..].trim());
        let Some(title) = header.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let title: String = title.trim().trim_matches('"').chars().take(120).collect();
        notes.push((f, title, body.to_string()));
    }
    notes.sort();
    let index = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut summary_of = std::collections::HashMap::new();
    for l in index.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let title = &l[l.find('[').unwrap_or(0) + 1..l.find("](").unwrap_or(0)];
        let gist = l[k + " — ".len()..].trim().trim_end_matches('…');
        summary_of.insert(format!("{}.md", &l[i + 2..j]), format!("{title}: {gist}"));
    }

    let tmp = std::env::temp_dir().join(format!("omoi-dupreal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut different = Vec::new(); // (highest similarity with the previous ones, file, with which)
    let mut id_of = std::collections::HashMap::new();
    for (f, t, c) in &notes {
        let p = parecidas(&db, &m, &texto(t, c), Some("/p"), None, -1.0, 1)?;
        if let Some((other, s)) = p.first() {
            different.push((*s, f.clone(), other.title.chars().take(50).collect::<String>()));
        }
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        id_of.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let mut resaved = Vec::new(); // (similarity with its original, best of the others)
    for (f, _, c) in &notes {
        let Some(title) = summary_of.get(f) else { continue };
        let start: String = c.chars().take(300).collect();
        let p = parecidas(&db, &m, &texto(title, &start), Some("/p"), None, -1.0, 200)?;
        let own = p.iter().find(|(x, _)| &id_of[&x.id] == f).map_or(-1.0, |x| x.1);
        let other = p.iter().find(|(x, _)| &id_of[&x.id] != f).map_or(-1.0, |x| x.1);
        resaved.push((own, other));
    }
    let mut by_hand = Vec::new(); // (class, similarity of b with a)
    for (i, l) in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).enumerate() {
        let c: Vec<&str> = l.split('\t').collect();
        let project = format!("/cal{i}");
        let n = Nueva { project: project.clone(), kind: Some(Tipo::Fact), title: c[1].into(), content: c[1].into(), ..Default::default() };
        let a = memoria::guardar(&mut db, n, Some(&m))?.id;
        let p = parecidas(&db, &m, &texto(c[2], c[2]), Some(&project), None, -1.0, 50)?;
        by_hand.push((c[0].to_string(), p.iter().find(|(x, _)| x.id == a).map_or(-1.0, |x| x.1)));
    }

    let n_d = different.len() as f64;
    let n_r = resaved.len() as f64;
    let count = |class: &str| by_hand.iter().filter(|x| x.0 == class).count() as f64;
    println!("{} notas distintas, {} reguardadas, {} pares a mano
", different.len(), resaved.len(), by_hand.len());
    println!("umbral  falsos(distintas)  reguardadas: avisa  y de la suya  | a mano: dup  rel  otro");
    for u in [0.60f32, 0.65, 0.70, 0.72, 0.75, 0.78, 0.80, 0.85, 0.90] {
        let fp = different.iter().filter(|x| x.0 >= u).count() as f64;
        let warns = resaved.iter().filter(|x| x.0.max(x.1) >= u).count() as f64;
        let right = resaved.iter().filter(|x| x.0 >= u && x.0 >= x.1).count() as f64;
        let rate = |class: &str| by_hand.iter().filter(|x| x.0 == class && x.1 >= u).count() as f64 / count(class);
        println!(
            "{u:.2}    {:>6.1}%            {:>6.1}%  {:>6.1}%       |       {:>4.0}% {:>4.0}% {:>4.0}%",
            100.0 * fp / n_d,
            100.0 * warns / n_r,
            100.0 * right / n_r,
            100.0 * rate("dup"),
            100.0 * rate("rel"),
            100.0 * rate("otro")
        );
    }
    different.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("
las distintas más parecidas:");
    for (s, f, other) in different.iter().take(10) {
        println!("  {s:.3}  {f}  ~  {other}");
    }
    let mut r = resaved.clone();
    r.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("reguardadas con menos similitud con la suya: {:?}", r.iter().take(8).map(|x| (x.0 * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// A duplicate candidate: (centered cosine, (IDF coverage, rare words shared)).
type Candidate = (f32, (f64, usize));

/// Like `dupreal`, but with a second signal besides the vector: IDF coverage (what part of the
/// weight of the new one's words is in the candidate, with Lucene's IDF over the notes, the
/// same as recall's). Rule: duplicate if cosine ≥ a, or if cosine ≥ b and coverage ≥ k.
fn dupcob(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{parecidas, texto};
    use std::collections::{BTreeSet, HashMap};
    let m = model()?;
    let tokens = |s: &str| -> BTreeSet<String> {
        omoikane::buscar::terminos(s)
            .into_iter()
            .map(|t| {
                t.chars()
                    .map(|c| match c {
                        'á' | 'à' | 'ä' | 'â' => 'a',
                        'é' | 'è' | 'ë' | 'ê' => 'e',
                        'í' | 'ì' | 'ï' | 'î' => 'i',
                        'ó' | 'ò' | 'ö' | 'ô' => 'o',
                        'ú' | 'ù' | 'ü' | 'û' => 'u',
                        c => c,
                    })
                    .collect()
            })
            .collect()
    };
    let mut notes: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("\n---") else { continue };
        let (header, body) = (&r[..i], r[i + 4..].trim());
        let Some(title) = header.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let title: String = title.trim().trim_matches('"').chars().take(120).collect();
        notes.push((f, title, body.to_string()));
    }
    notes.sort();
    let index = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut summary_of = HashMap::new();
    for l in index.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let title = &l[l.find('[').unwrap_or(0) + 1..i];
        let gist = l[k + " — ".len()..].trim().trim_end_matches('…');
        summary_of.insert(format!("{}.md", &l[i + 2..j]), format!("{title}: {gist}"));
    }
    // df over all the notes.
    let note_tokens: HashMap<String, BTreeSet<String>> = notes.iter().map(|(f, t, c)| (f.clone(), tokens(&texto(t, c)))).collect();
    let mut df: HashMap<String, f64> = HashMap::new();
    for fs in note_tokens.values() {
        for t in fs {
            *df.entry(t.clone()).or_default() += 1.0;
        }
    }
    let n = notes.len() as f64;
    let idf = |t: &str| {
        let d = df.get(t).copied().unwrap_or(0.0).max(1.0);
        (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
    };
    // (coverage, rare words shared: in at most 20 % of the notes)
    let coverage = |new: &BTreeSet<String>, candidate: &BTreeSet<String>| -> (f64, usize) {
        let total: f64 = new.iter().map(|t| idf(t)).sum();
        let inside: f64 = new.iter().filter(|t| candidate.contains(*t)).map(|t| idf(t)).sum();
        let rare = new.iter().filter(|t| candidate.contains(*t) && df.get(*t).copied().unwrap_or(0.0) <= 0.2 * n).count();
        (if total > 0.0 { inside / total } else { 0.0 }, rare)
    };

    let tmp = std::env::temp_dir().join(format!("omoi-dupcob-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    let mut tokens_of_id: HashMap<String, BTreeSet<String>> = HashMap::new();
    // different: candidates (cosine, coverage) of each note against the previous ones; all are false.
    let mut different: Vec<Vec<Candidate>> = Vec::new();
    for (f, t, c) in &notes {
        let new = &note_tokens[f];
        let p = parecidas(&db, &m, &texto(t, c), Some("/p"), None, -1.0, 5)?;
        different.push(p.iter().map(|(x, s)| (*s, coverage(new, &tokens_of_id[&x.id]))).collect());
        let nn = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        let id = memoria::guardar(&mut db, nn, Some(&m))?.id;
        file_of.insert(id.clone(), f.clone());
        tokens_of_id.insert(id, new.clone());
    }
    // re-saved: (its own, the other candidates).
    let mut resaved: Vec<(Option<Candidate>, Vec<Candidate>)> = Vec::new();
    for (f, _, c) in &notes {
        let Some(title) = summary_of.get(f) else { continue };
        let start: String = c.chars().take(300).collect();
        let txt = texto(title, &start);
        let new = tokens(&txt);
        let p = parecidas(&db, &m, &txt, Some("/p"), None, -1.0, 5)?;
        let mut own = None;
        let mut others = Vec::new();
        for (x, s) in &p {
            let pair = (*s, coverage(&new, &tokens_of_id[&x.id]));
            if &file_of[&x.id] == f { own = Some(pair) } else { others.push(pair) }
        }
        resaved.push((own, others));
    }
    // by hand: (class, cosine, coverage of b over a).
    let mut by_hand: Vec<(String, f32, (f64, usize))> = Vec::new();
    for (i, l) in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).enumerate() {
        let c: Vec<&str> = l.split('\t').collect();
        let project = format!("/cal{i}");
        let nn = Nueva { project: project.clone(), kind: Some(Tipo::Fact), title: c[1].into(), content: c[1].into(), ..Default::default() };
        let a = memoria::guardar(&mut db, nn, Some(&m))?.id;
        let p = parecidas(&db, &m, &texto(c[2], c[2]), Some(&project), None, -1.0, 50)?;
        let s = p.iter().find(|(x, _)| x.id == a).map_or(-1.0, |x| x.1);
        by_hand.push((c[0].to_string(), s, coverage(&tokens(c[2]), &tokens(c[1]))));
    }

    // short: each sentence of the pairs against the notes (none is a duplicate of a note).
    let mut short: Vec<Vec<Candidate>> = Vec::new();
    for l in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        for sentence in l.split('\t').skip(1) {
            let new = tokens(sentence);
            let p = parecidas(&db, &m, &texto(sentence, sentence), Some("/p"), None, -1.0, 5)?;
            short.push(p.iter().filter(|(x, _)| tokens_of_id.contains_key(&x.id)).map(|(x, s)| (*s, coverage(&new, &tokens_of_id[&x.id]))).collect());
        }
    }
    let nc = short.len() as f64;
    let nd = different.len() as f64;
    let nr = resaved.len() as f64;
    let class_total = |k: &str| by_hand.iter().filter(|x| x.0 == k).count() as f64;
    println!("{} distintas, {} reguardadas, {} a mano", different.len(), resaved.len(), by_hand.len());
    println!("  a     b     k  r  | falsos largas cortas | reguardadas: la suya  otra  | a mano: dup  rel  otro");
    let mut rows = Vec::new();
    for a in [0.70f32, 0.75, 0.78, 0.80, 0.85, 2.0] {
        for b in [0.30f32, 0.40, 0.50, 0.60] {
            for k in [0.40f64, 0.50, 0.60, 0.70] {
              for r in [0usize, 2, 3] {
                let dup = |c: &Candidate| c.0 >= a || (c.0 >= b && c.1.0 >= k && c.1.1 >= r);
                let fp = different.iter().filter(|v| v.iter().any(dup)).count() as f64 / nd;
                let fc = short.iter().filter(|v| v.iter().any(dup)).count() as f64 / nc;
                let own = resaved.iter().filter(|r| r.0.as_ref().is_some_and(dup)).count() as f64 / nr;
                let other = resaved.iter().filter(|r| r.1.iter().any(dup)).count() as f64 / nr;
                let rate = |cl: &str| by_hand.iter().filter(|x| x.0 == cl && dup(&(x.1, x.2))).count() as f64 / class_total(cl);
                rows.push((a, b, k, r, fp, fc, own, other, rate("dup"), rate("rel"), rate("otro")));
              }
            }
        }
    }
    // The best ones with no false positives among the different ones or the rel ones, by how many re-saved ones they catch.
    rows.sort_by(|x, y| (x.4 + x.5 + x.7 + x.9).total_cmp(&(y.4 + y.5 + y.7 + y.9)).then(y.6.total_cmp(&x.6)).then(y.8.total_cmp(&x.8)));
    for f in rows.iter().take(25) {
        println!(
            "{:.2}  {:.2}  {:.2}  {}  | {:>5.1}%  {:>5.1}%  |  {:>5.1}%  {:>5.1}%  |  {:>4.0}% {:>4.0}% {:>4.0}%",
            f.0, f.1, f.2, f.3, 100.0 * f.4, 100.0 * f.5, 100.0 * f.6, 100.0 * f.7, 100.0 * f.8, 100.0 * f.9, 100.0 * f.10
        );
    }
    println!("\nla regla actual (solo coseno ≥ 0,70):");
    for f in rows.iter().filter(|f| f.0 == 0.70 && f.1 == 0.60 && f.2 == 0.70 && f.3 == 0) {
        println!("  falsos {:.1}% / cortas {:.1}%, la suya {:.1}%, a mano dup {:.0}% rel {:.0}%", 100.0 * f.4, 100.0 * f.5, 100.0 * f.6, 100.0 * f.8, 100.0 * f.9);
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// The production duplicate warning (`vectores::duplicada`) with the real notes of `dir`:
/// different (each note before saving it: every warning is false), re-saved (each note said again:
/// it must warn, and about its own) and short sentences of the pairs against the notes (every warning is false).
fn dupprod(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{duplicada, relacionadas};
    use std::collections::HashMap;
    use std::time::Instant;
    let m = model()?;
    let mut notes: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("\n---") else { continue };
        let (header, body) = (&r[..i], r[i + 4..].trim());
        let Some(title) = header.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let title: String = title.trim().trim_matches('"').chars().take(120).collect();
        notes.push((f, title, body.to_string()));
    }
    notes.sort();
    let index = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut summary_of = HashMap::new();
    for l in index.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let title = &l[l.find('[').unwrap_or(0) + 1..i];
        let gist = l[k + " — ".len()..].trim().trim_end_matches('…');
        summary_of.insert(format!("{}.md", &l[i + 2..j]), format!("{title}: {gist}"));
    }
    let tmp = std::env::temp_dir().join(format!("omoi-dupprod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    let (mut false_hits, mut times, mut different_with_rel) = (Vec::new(), Vec::new(), 0);
    for (f, t, c) in &notes {
        let now = Instant::now();
        if let Some((other, s)) = duplicada(&db, &m, "/p", t, c)? {
            false_hits.push(format!("{s:.3} {f} ~ {}", other.title.chars().take(50).collect::<String>()));
        } else if !relacionadas(&db, "/p", t, "", 3)?.is_empty() {
            different_with_rel += 1;
        }
        times.push(now.elapsed().as_secs_f64() * 1000.0);
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        file_of.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let (mut own, mut other, mut total, mut in_rel) = (0, 0, 0, 0);
    for (f, _, c) in &notes {
        let Some(title) = summary_of.get(f) else { continue };
        total += 1;
        let start: String = c.chars().take(300).collect();
        match duplicada(&db, &m, "/p", title, &start)? {
            Some((x, _)) if &file_of[&x.id] == f => own += 1,
            Some(_) => other += 1,
            None => {
                in_rel += usize::from(relacionadas(&db, "/p", title, "", 3)?.iter().any(|x| &file_of[&x.id] == f));
            }
        }
    }
    let (mut short, mut false_short, mut short_with_rel) = (0, Vec::new(), 0);
    for l in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        for sentence in l.split('\t').skip(1) {
            short += 1;
            if let Some((x, s)) = duplicada(&db, &m, "/p", sentence, sentence)? {
                false_short.push(format!("{s:.3} «{sentence}» ~ {}", x.title.chars().take(40).collect::<String>()));
            } else if !relacionadas(&db, "/p", sentence, "", 3)?.is_empty() {
                short_with_rel += 1;
            }
        }
    }
    times.sort_by(f64::total_cmp);
    println!("distintas: {} avisos en falso de {}", false_hits.len(), notes.len());
    for x in &false_hits {
        println!("  {x}");
    }
    println!("reguardadas: {own} de {total} avisan de la suya ({:.1} %), {other} de otra", 100.0 * own as f64 / total as f64);
    println!("  de las que no avisan, la suya sale entre las relacionadas en {in_rel}: cubiertas {} de {total} ({:.1} %)", own + in_rel, 100.0 * (own + in_rel) as f64 / total as f64);
    println!("distintas con relacionadas (no bloquea): {different_with_rel} de {}", notes.len());
    println!("cortas: {} avisos en falso de {short}; con relacionadas (no bloquea): {short_with_rel}", false_short.len());
    for x in &false_short {
        println!("  {x}");
    }
    println!("tiempo de la comprobación: mediana {:.1} ms, p95 {:.1} ms", times[times.len() / 2], times[times.len() * 95 / 100]);
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}
