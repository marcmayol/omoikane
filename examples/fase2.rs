//! Preparar la fase 2: medir antes de decidir.
//!
//!   cargo run --release --example fase2 -- duplicados   ¿separa el coseno los duplicados de los temas parecidos?
//!   cargo run --release --example fase2 -- vectores N   vec0 frente a BLOB + vec_distance_cosine, filtrando por proyecto
//!   cargo run --release --example fase2 -- busqueda     acierto de la búsqueda con tests/busquedas.tsv según el umbral

use anyhow::{Result, anyhow};
use omoikane::embeber::Modelo;
use std::time::Instant;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("duplicados") => duplicados(),
        Some("vectores") => vectores(args.get(1).map_or(Ok(5000), |n| n.parse())?),
        Some("busqueda") => busqueda(),
        Some("variantes") => variantes(),
        _ => Err(anyhow!("uso: fase2 duplicados | vectores N | busqueda")),
    }
}

fn coseno(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Palabras en minúsculas y sin tildes, sin las vacías más comunes.
fn palabras(s: &str) -> std::collections::BTreeSet<String> {
    const VACIAS: &[&str] = &[
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
    s.split(|c: char| !c.is_alphanumeric()).filter(|p| p.len() > 1 && !VACIAS.contains(p)).map(String::from).collect()
}

fn jaccard(a: &str, b: &str) -> f32 {
    let (a, b) = (palabras(a), palabras(b));
    let i = a.intersection(&b).count() as f32;
    let u = a.union(&b).count() as f32;
    if u == 0.0 { 0.0 } else { i / u }
}

/// El mejor umbral para separar `dup` de `rel` y su acierto (sobre dup + rel).
fn mejor_umbral(v: &[(String, f32)]) -> (f32, f32) {
    let mut mejor = (0.0, 0.0);
    for i in 0..=200 {
        let u = i as f32 / 200.0;
        let bien = v
            .iter()
            .filter(|(c, _)| c != "otro")
            .filter(|(c, s)| (c == "dup") == (*s >= u))
            .count() as f32;
        let total = v.iter().filter(|(c, _)| c != "otro").count() as f32;
        if bien / total > mejor.1 {
            mejor = (u, bien / total);
        }
    }
    mejor
}

fn resumen(nombre: &str, v: &[(String, f32)]) {
    println!("\n{nombre}");
    for clase in ["dup", "rel", "otro"] {
        let mut x: Vec<f32> = v.iter().filter(|(c, _)| c == clase).map(|(_, s)| *s).collect();
        x.sort_by(f32::total_cmp);
        println!(
            "  {clase:<5} n={:<3} min {:.3}  mediana {:.3}  max {:.3}",
            x.len(),
            x[0],
            x[x.len() / 2],
            x[x.len() - 1]
        );
    }
    let (u, acierto) = mejor_umbral(v);
    println!("  mejor umbral dup/rel: {u:.3} → acierto {:.0} %", acierto * 100.0);
}

fn duplicados() -> Result<()> {
    let dir = dirs::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi");
    let m = Modelo::abrir(&dir)?;
    let pares: Vec<(String, String, String)> = include_str!("../tests/pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].to_string(), c[1].to_string(), c[2].to_string())
        })
        .collect();
    let mut cos = Vec::new();
    let mut jac = Vec::new();
    let mut mezcla = Vec::new();
    for (clase, a, b) in &pares {
        let c = coseno(&m.vector(a)?, &m.vector(b)?);
        let j = jaccard(a, b);
        cos.push((clase.clone(), c));
        jac.push((clase.clone(), j));
        mezcla.push((clase.clone(), 0.5 * c + 0.5 * j));
    }
    resumen("coseno (model2vec int8)", &cos);
    resumen("jaccard de palabras", &jac);
    resumen("0,5·coseno + 0,5·jaccard", &mezcla);
    println!("\nlos dup con menos coseno:");
    let mut d: Vec<_> = pares.iter().zip(&cos).filter(|(p, _)| p.0 == "dup").collect();
    d.sort_by(|a, b| a.1.1.total_cmp(&b.1.1));
    for (p, (_, c)) in d.iter().take(4) {
        println!("  {c:.3}  «{}» / «{}»", p.1, p.2);
    }
    println!("los rel con más coseno:");
    let mut r: Vec<_> = pares.iter().zip(&cos).filter(|(p, _)| p.0 == "rel").collect();
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

fn mediana(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// N memorias repartidas en 10 proyectos más un 10 % globales. Busca las 40 más cercanas del
/// proyecto 3 y las globales, por dos caminos, intercalados.
fn vectores(n: usize) -> Result<()> {
    omoikane::db::registrar_sqlite_vec();
    let ruta = std::env::temp_dir().join(format!("omoi-fase2-{}.db", std::process::id()));
    let mut db = rusqlite::Connection::open(&ruta)?;
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
    let en_ambito: i64 = db.query_row("SELECT count(*) FROM memory WHERE project IN ('/p3', '')", [], |f| f.get(0))?;
    println!("{n} memorias, {en_ambito} en el ámbito (/p3 + globales)");

    let consultas: Vec<Vec<u8>> = (0..30).map(|_| r.vector()).collect();
    // A: vec0 sin filtro (no sabe filtrar por proyecto en el KNN), pidiendo de más y filtrando después.
    let mut a = db.prepare(
        "WITH k AS (SELECT rowid, distance FROM v0 WHERE e MATCH ?1 AND k = 400)
         SELECT k.rowid FROM k JOIN memory m ON m.n = k.rowid WHERE m.project IN ('/p3', '') ORDER BY k.distance LIMIT 40",
    )?;
    // B: BLOB normal y la función de distancia, con el filtro exacto en el mismo SQL.
    let mut b = db.prepare(
        "SELECT m.n FROM vb JOIN memory m ON m.n = vb.n WHERE m.project IN ('/p3', '')
         ORDER BY vec_distance_cosine(vb.e, ?1) LIMIT 40",
    )?;
    let (mut ta, mut tb) = (Vec::new(), Vec::new());
    let mut iguales = 0;
    for q in &consultas {
        let t = Instant::now();
        let ra: Vec<i64> = a.query_map([q], |f| f.get(0))?.collect::<Result<_, _>>()?;
        ta.push(ms(t));
        let t = Instant::now();
        let rb: Vec<i64> = b.query_map([q], |f| f.get(0))?.collect::<Result<_, _>>()?;
        tb.push(ms(t));
        if ra == rb {
            iguales += 1;
        }
    }
    println!("A vec0 k=400 y filtrar después   mediana {:.2} ms", mediana(ta));
    println!("B BLOB + vec_distance_cosine     mediana {:.2} ms", mediana(tb));
    println!("mismos 40 resultados en {iguales} de {} consultas", consultas.len());
    drop(a);
    drop(b);
    drop(db);
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", ruta.display()));
    }
    Ok(())
}

fn modelo() -> Result<Modelo> {
    Modelo::abrir(&dirs::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi"))
}

/// Guarda las memorias de tests/busquedas.tsv y mide, para cada umbral, cuántas consultas sacan su
/// memoria la primera o entre las tres primeras, y cuánto ruido devuelven las que no tienen respuesta.
fn busqueda() -> Result<()> {
    use omoikane::buscar::{Opciones, buscar_con};
    use omoikane::memoria::{self, Ambito, Nueva, Tipo};
    let m = modelo()?;
    let dir = std::env::temp_dir().join(format!("omoi-fase2-busqueda-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = omoikane::db::abrir(&dir.join("o.db"))?;
    let mut claves = std::collections::HashMap::new();
    let mut consultas: Vec<(String, String)> = Vec::new();
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
                claves.insert(mem.id, c[1].to_string());
            }
            _ => consultas.push((c[1].to_string(), c[2].to_string())),
        }
    }
    let con_respuesta = consultas.iter().filter(|(k, _)| k != "-").count();
    println!("{} memorias, {} consultas con respuesta, {} sin respuesta
", claves.len(), con_respuesta, consultas.len() - con_respuesta);
    println!("{:<18} {:>7} {:>7} {:>6}   {:>16} {:>14}", "modo", "top-1", "top-3", "MRR", "sin resp. con algo", "ruido medio");
    let configs: Vec<(String, bool, f32)> = std::iter::once(("solo palabras".to_string(), false, 0.0))
        .chain([0.20f32, 0.25, 0.28, 0.30, 0.33, 0.35, 0.40].into_iter().map(|u| (format!("híbrida ≥ {u:.2}"), true, u)))
        .collect();
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    for (nombre, con_modelo, u) in &configs {
        let (mut top1, mut top3, mut mrr, mut con_algo, mut ruido) = (0, 0, 0.0, 0, 0);
        for (clave, q) in &consultas {
            let r = buscar_con(&mut db, q, &o, con_modelo.then_some(&m), *u)?;
            let puestos: Vec<&String> = r.iter().map(|r| &claves[&r.memoria.id]).collect();
            if clave == "-" {
                if std::env::var_os("DETALLE").is_some() && !puestos.is_empty() {
                    println!("    {nombre}: sin respuesta «{q}» → {:?}", puestos);
                }
                con_algo += usize::from(!puestos.is_empty());
                ruido += puestos.len();
                continue;
            }
            if std::env::var_os("DETALLE").is_some() && puestos.first().map(|k| *k != clave).unwrap_or(true) {
                println!("    {nombre}: «{q}» → {:?} (esperada {clave})", puestos.iter().take(3).collect::<Vec<_>>());
            }
            if let Some(i) = puestos.iter().position(|k| *k == clave) {
                top1 += usize::from(i == 0);
                top3 += usize::from(i < 3);
                mrr += 1.0 / (i as f64 + 1.0);
            }
        }
        let sin = consultas.len() - con_respuesta;
        println!(
            "{nombre:<18} {:>6.0}% {:>6.0}% {:>6.2}   {:>11} de {sin} {:>14.1}",
            100.0 * top1 as f64 / con_respuesta as f64,
            100.0 * top3 as f64 / con_respuesta as f64,
            mrr / con_respuesta as f64,
            con_algo,
            ruido as f64 / sin as f64
        );
    }
    // Similitud de la memoria correcta y de lo mejor que sale en las consultas sin respuesta.
    println!("
similitud (coseno) consulta → memoria esperada, y lo más alto en las sin respuesta:");
    let mut esperadas = Vec::new();
    let mut ajenas = Vec::new();
    for (clave, q) in &consultas {
        let p = omoikane::vectores::parecidas(&db, &m, q, None, None, -1.0, 50)?;
        if clave == "-" {
            ajenas.push(p.first().map_or(0.0, |x| x.1));
        } else if let Some((_, s)) = p.iter().find(|(mem, _)| &claves[&mem.id] == clave) {
            esperadas.push((*s, q.clone()));
        }
    }
    esperadas.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (s, q) in esperadas.iter().take(6) {
        println!("  {s:.3}  «{q}»");
    }
    println!("  … mediana {:.3}, máx {:.3}", esperadas[esperadas.len() / 2].0, esperadas.last().unwrap().0);
    ajenas.sort_by(f32::total_cmp);
    println!("  sin respuesta, lo más parecido: {:?}", ajenas.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// ¿Qué texto conviene vectorizar? Para cada consulta con respuesta: el puesto de la memoria
/// correcta según la similitud con título+contenido, solo título, solo contenido y el máximo.
fn variantes() -> Result<()> {
    let m = modelo()?;
    let mut mems: Vec<(String, String, String)> = Vec::new();
    let mut consultas = Vec::new();
    for l in include_str!("../tests/busquedas.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        match c[0] {
            "m" => mems.push((c[1].into(), c[2].into(), c[3].into())),
            _ => consultas.push((c[1].to_string(), c[2].to_string())),
        }
    }
    let vt: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&x.1)).collect::<Result<_>>()?;
    let vc: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&x.2)).collect::<Result<_>>()?;
    let va: Vec<Vec<f32>> = mems.iter().map(|x| m.vector(&format!("{}
{}", x.1, x.2))).collect::<Result<_>>()?;
    let nombres = ["título+contenido", "solo título", "solo contenido", "máx(título, t+c)"];
    let mut top1 = [0usize; 4];
    let mut ruido = [0f32; 4];
    for (clave, q) in &consultas {
        let vq = m.vector(q)?;
        let sims: Vec<[f32; 4]> = (0..mems.len())
            .map(|i| {
                let (a, t, c) = (coseno(&vq, &va[i]), coseno(&vq, &vt[i]), coseno(&vq, &vc[i]));
                [a, t, c, a.max(t)]
            })
            .collect();
        for k in 0..4 {
            let mejor = (0..mems.len()).max_by(|&a, &b| sims[a][k].total_cmp(&sims[b][k])).unwrap();
            if clave == "-" {
                ruido[k] = ruido[k].max(sims[mejor][k]);
            } else if mems[mejor].0 == *clave {
                top1[k] += 1;
            }
        }
    }
    let n = consultas.iter().filter(|(k, _)| k != "-").count();
    for k in 0..4 {
        println!("{:<20} top-1 solo vector {:>3.0}%   máx. similitud en las sin respuesta {:.3}", nombres[k], 100.0 * top1[k] as f64 / n as f64, ruido[k]);
    }
    Ok(())
}
