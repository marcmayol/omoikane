//! Preparar la fase 2: medir antes de decidir.
//!
//!   cargo run --release --example fase2 -- duplicados   ¿separa el coseno los duplicados de los temas parecidos?
//!   cargo run --release --example fase2 -- vectores N   vec0 frente a BLOB + vec_distance_cosine, filtrando por proyecto
//!   cargo run --release --example fase2 -- busqueda     acierto de la búsqueda con tests/busquedas.tsv según el umbral
//!   cargo run --release --example fase2 -- dupreal DIR  el aviso de duplicado, con el código de producción y notas reales

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
        Some("centrar") => centrar(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("real") => real(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupprod") => dupprod(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupcob") => dupcob(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
        Some("dupreal") => dupreal(std::path::Path::new(args.get(1).ok_or_else(|| anyhow!("falta la carpeta"))?)),
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
    let configs = configuraciones();
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    for (nombre, con_modelo, u, fusion) in &configs {
        let (mut top1, mut top3, mut mrr, mut con_algo, mut ruido) = (0, 0, 0.0, 0, 0);
        for (clave, q) in &consultas {
            let r = buscar_con(&mut db, q, &o, con_modelo.then_some(&m), *u, *fusion)?;
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

/// Calibración con notas reales (no se suben al repo): una carpeta de memorias de Claude Code, con
/// un `.md` por nota (frontmatter `name`/`description` y cuerpo) y un `MEMORY.md` con una línea por
/// nota: `- [Título](fichero.md) — resumen`. El resumen es la consulta; su nota, la respuesta.
/// También mira todos los pares de notas distintas: cualquier aviso de duplicado es un falso positivo.
fn real(dir: &std::path::Path) -> Result<()> {
    use omoikane::buscar::{Opciones, buscar_con};
    use omoikane::memoria::{self, Ambito, Nueva, Tipo};
    let m = modelo()?;
    let mut notas: Vec<(String, String, String)> = Vec::new(); // (fichero, título, cuerpo)
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let (cabecera, cuerpo) = match t.strip_prefix("---") {
            Some(r) => match r.find("
---") {
                Some(i) => (&r[..i], r[i + 4..].trim()),
                None => ("", t.as_str()),
            },
            None => ("", t.as_str()),
        };
        let titulo = cabecera
            .lines()
            .find_map(|l| l.trim().strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| f.trim_end_matches(".md").replace(['_', '-'], " "));
        let titulo: String = titulo.chars().take(200).collect();
        if !cuerpo.is_empty() {
            notas.push((f, titulo, cuerpo.to_string()));
        }
    }
    let indice = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut consultas: Vec<(String, String)> = Vec::new(); // (fichero esperado, consulta)
    for l in indice.lines() {
        let (Some(i), Some(j)) = (l.find("]("), l.find(".md)")) else { continue };
        let fichero = format!("{}.md", &l[i + 2..j]);
        if let Some(k) = l.find(" — ") {
            let resumen = l[k + " — ".len()..].trim().trim_end_matches('…').to_string();
            if notas.iter().any(|n| n.0 == fichero) && resumen.len() > 10 {
                consultas.push((fichero, resumen));
            }
        }
    }
    println!("{} notas, {} consultas (resúmenes del índice)", notas.len(), consultas.len());

    // Duplicados: todos los pares de notas distintas.
    let vs: Vec<Vec<f32>> = notas.iter().map(|n| m.vector(&omoikane::vectores::texto(&n.1, &n.2))).collect::<Result<_>>()?;
    let mut pares = Vec::new();
    for i in 0..vs.len() {
        for j in i + 1..vs.len() {
            pares.push((coseno(&vs[i], &vs[j]), i, j));
        }
    }
    pares.sort_by(|a, b| b.0.total_cmp(&a.0));
    let total = pares.len();
    for u in [0.60f32, 0.65, 0.70, 0.75, 0.80] {
        let n = pares.iter().filter(|p| p.0 >= u).count();
        println!("pares de notas distintas con coseno ≥ {u:.2}: {n} de {total}");
    }
    println!("los más parecidos (revisar si son duplicados de verdad):");
    for (s, i, j) in pares.iter().take(8) {
        println!("  {s:.3}  {} / {}", notas[*i].0, notas[*j].0);
    }

    // Búsqueda.
    let tmp = std::env::temp_dir().join(format!("omoi-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = std::collections::HashMap::new();
    for (f, t, c) in &notas {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    println!("
{:<18} {:>7} {:>7} {:>6}", "modo", "top-1", "top-3", "MRR");
    let configs = configuraciones();
    for (nombre, con, u, fusion) in &configs {
        let (mut t1, mut t3, mut mrr) = (0, 0, 0.0);
        for (f, q) in &consultas {
            let r = buscar_con(&mut db, q, &o, con.then_some(&m), *u, *fusion)?;
            if let Some(i) = r.iter().position(|r| &fichero_de[&r.memoria.id] == f) {
                t1 += usize::from(i == 0);
                t3 += usize::from(i < 3);
                mrr += 1.0 / (i as f64 + 1.0);
            }
        }
        let n = consultas.len() as f64;
        println!("{nombre:<18} {:>6.0}% {:>6.0}% {:>6.2}", 100.0 * t1 as f64 / n, 100.0 * t3 as f64 / n, mrr / n);
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// Resta el vector medio y renormaliza.
fn centrados(vs: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let d = vs[0].len();
    let mut media = vec![0f32; d];
    for v in vs {
        for (m, x) in media.iter_mut().zip(v) {
            *m += x / vs.len() as f32;
        }
    }
    vs.iter()
        .map(|v| {
            let c: Vec<f32> = v.iter().zip(&media).map(|(x, m)| x - m).collect();
            let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            c.iter().map(|x| x / n).collect()
        })
        .collect()
}

/// ¿Separa mejor los duplicados restar el vector medio de la colección? Con las notas reales de
/// `dir` (todas distintas: lo que pase el umbral es falso positivo) y con los pares de calibración.
fn centrar(dir: &std::path::Path) -> Result<()> {
    let m = modelo()?;
    // Notas reales.
    let mut textos = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if f.ends_with(".md") && f != "MEMORY.md" {
            textos.push(std::fs::read_to_string(e.path())?);
        }
    }
    let vs: Vec<Vec<f32>> = textos.iter().map(|t| m.vector(t)).collect::<Result<_>>()?;
    let cs = centrados(&vs);
    let pares = |v: &[Vec<f32>]| -> Vec<f32> {
        let mut p = Vec::new();
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                p.push(coseno(&v[i], &v[j]));
            }
        }
        p
    };
    let (pb, pc) = (pares(&vs), pares(&cs));
    println!("{} notas reales, {} pares (todos distintos)", textos.len(), pb.len());
    for u in [0.5f32, 0.6, 0.65, 0.7, 0.8] {
        println!(
            "  ≥ {u:.2}: sin centrar {:>5}   centrados {:>5}",
            pb.iter().filter(|x| **x >= u).count(),
            pc.iter().filter(|x| **x >= u).count()
        );
    }
    // Pares de calibración: centrados con la media de sus propias frases.
    let pares_cal: Vec<(String, String, String)> = include_str!("../tests/pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].to_string(), c[1].to_string(), c[2].to_string())
        })
        .collect();
    let frases: Vec<String> = pares_cal.iter().flat_map(|p| [p.1.clone(), p.2.clone()]).collect();
    let fv: Vec<Vec<f32>> = frases.iter().map(|t| m.vector(t)).collect::<Result<_>>()?;
    for (nombre, v) in [("sin centrar", fv.clone()), ("centrados", centrados(&fv))] {
        let r: Vec<(String, f32)> = pares_cal.iter().enumerate().map(|(i, p)| (p.0.clone(), coseno(&v[2 * i], &v[2 * i + 1]))).collect();
        resumen(&format!("pares de calibración, {nombre}"), &r);
    }
    // Notas reales centradas con la media de las notas, pares de calibración centrados con la misma.
    let d = vs[0].len();
    let mut media = vec![0f32; d];
    for v in &vs {
        for (mm, x) in media.iter_mut().zip(v) {
            *mm += x / vs.len() as f32;
        }
    }
    let con_media = |v: &Vec<f32>| -> Vec<f32> {
        let c: Vec<f32> = v.iter().zip(&media).map(|(x, mm)| x - mm).collect();
        let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        c.iter().map(|x| x / n).collect()
    };
    let r: Vec<(String, f32)> =
        pares_cal.iter().enumerate().map(|(i, p)| (p.0.clone(), coseno(&con_media(&fv[2 * i]), &con_media(&fv[2 * i + 1])))).collect();
    resumen("pares de calibración centrados con la media de las notas reales", &r);

    // ¿Y con una media genérica, fija por modelo? La del corpus de los tests (código, docs y frases
    // en muchos idiomas), que no tiene nada que ver con las notas.
    let generico: Vec<Vec<f32>> = include_str!("../tests/corpus.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| m.vector(l))
        .collect::<Result<_>>()?;
    let mut mg = vec![0f32; d];
    for v in &generico {
        for (mm, x) in mg.iter_mut().zip(v) {
            *mm += x / generico.len() as f32;
        }
    }
    let con = |v: &Vec<f32>, med: &[f32]| -> Vec<f32> {
        let c: Vec<f32> = v.iter().zip(med).map(|(x, mm)| x - mm).collect();
        let n = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        c.iter().map(|x| x / n).collect()
    };
    let cg: Vec<Vec<f32>> = vs.iter().map(|v| con(v, &mg)).collect();
    let pg = pares(&cg);
    println!("
notas reales centradas con la media GENÉRICA del corpus de tests:");
    for u in [0.5f32, 0.6, 0.65, 0.7, 0.8] {
        println!("  ≥ {u:.2}: {:>5}", pg.iter().filter(|x| **x >= u).count());
    }
    let r: Vec<(String, f32)> =
        pares_cal.iter().enumerate().map(|(i, p)| (p.0.clone(), coseno(&con(&fv[2 * i], &mg), &con(&fv[2 * i + 1], &mg)))).collect();
    resumen("pares de calibración centrados con la media genérica", &r);
    Ok(())
}

fn configuraciones() -> Vec<(String, bool, f32, omoikane::buscar::Fusion)> {
    use omoikane::buscar::Fusion;
    let mut v = vec![("solo palabras".to_string(), false, 0.0, Fusion::Rescate)];
    for u in [0.15f32, 0.20, 0.25, 0.30, 0.35] {
        v.push((format!("rrf 1.0 >= {u:.2}"), true, u, Fusion::Rrf { peso_vector: 1.0 }));
        v.push((format!("rrf 0.5 >= {u:.2}"), true, u, Fusion::Rrf { peso_vector: 0.5 }));
        v.push((format!("rescate >= {u:.2}"), true, u, Fusion::Rescate));
    }
    v
}

/// El aviso de duplicado tal como lo da `vectores::parecidas` (centrado incluido), con notas reales
/// de `dir` (las de `real`; no se suben). Tres medidas:
/// - distintas: cada nota, antes de guardarla, contra las ya guardadas. Ninguna es duplicada de otra,
///   así que todo aviso es un falso positivo.
/// - reguardadas: cada nota dicha otra vez como lo haría un agente que no la ve (el resumen del índice
///   como título y el principio del cuerpo). Debería avisar, y de su original.
/// - los pares escritos a mano de tests/pares_duplicados.tsv, en la misma base.
fn dupreal(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{parecidas, texto};
    let m = modelo()?;
    let mut notas: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("
---") else { continue };
        let (cabecera, cuerpo) = (&r[..i], r[i + 4..].trim());
        let Some(titulo) = cabecera.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let titulo: String = titulo.trim().trim_matches('"').chars().take(120).collect();
        notas.push((f, titulo, cuerpo.to_string()));
    }
    notas.sort();
    let indice = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut resumen_de = std::collections::HashMap::new();
    for l in indice.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let titulo = &l[l.find('[').unwrap_or(0) + 1..l.find("](").unwrap_or(0)];
        let resumen = l[k + " — ".len()..].trim().trim_end_matches('…');
        resumen_de.insert(format!("{}.md", &l[i + 2..j]), format!("{titulo}: {resumen}"));
    }

    let tmp = std::env::temp_dir().join(format!("omoi-dupreal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut distintas = Vec::new(); // (máxima similitud con las anteriores, fichero, con cuál)
    let mut id_de = std::collections::HashMap::new();
    for (f, t, c) in &notas {
        let p = parecidas(&db, &m, &texto(t, c), Some("/p"), None, -1.0, 1)?;
        if let Some((otra, s)) = p.first() {
            distintas.push((*s, f.clone(), otra.title.chars().take(50).collect::<String>()));
        }
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        id_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let mut reguardadas = Vec::new(); // (similitud con su original, mejor de las otras)
    for (f, _, c) in &notas {
        let Some(titulo) = resumen_de.get(f) else { continue };
        let principio: String = c.chars().take(300).collect();
        let p = parecidas(&db, &m, &texto(titulo, &principio), Some("/p"), None, -1.0, 200)?;
        let propia = p.iter().find(|(x, _)| &id_de[&x.id] == f).map_or(-1.0, |x| x.1);
        let otra = p.iter().find(|(x, _)| &id_de[&x.id] != f).map_or(-1.0, |x| x.1);
        reguardadas.push((propia, otra));
    }
    let mut mano = Vec::new(); // (clase, similitud de b con a)
    for (i, l) in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).enumerate() {
        let c: Vec<&str> = l.split('\t').collect();
        let proyecto = format!("/cal{i}");
        let n = Nueva { project: proyecto.clone(), kind: Some(Tipo::Fact), title: c[1].into(), content: c[1].into(), ..Default::default() };
        let a = memoria::guardar(&mut db, n, Some(&m))?.id;
        let p = parecidas(&db, &m, &texto(c[2], c[2]), Some(&proyecto), None, -1.0, 50)?;
        mano.push((c[0].to_string(), p.iter().find(|(x, _)| x.id == a).map_or(-1.0, |x| x.1)));
    }

    let n_d = distintas.len() as f64;
    let n_r = reguardadas.len() as f64;
    let cuenta = |clase: &str| mano.iter().filter(|x| x.0 == clase).count() as f64;
    println!("{} notas distintas, {} reguardadas, {} pares a mano
", distintas.len(), reguardadas.len(), mano.len());
    println!("umbral  falsos(distintas)  reguardadas: avisa  y de la suya  | a mano: dup  rel  otro");
    for u in [0.60f32, 0.65, 0.70, 0.72, 0.75, 0.78, 0.80, 0.85, 0.90] {
        let fp = distintas.iter().filter(|x| x.0 >= u).count() as f64;
        let avisa = reguardadas.iter().filter(|x| x.0.max(x.1) >= u).count() as f64;
        let bien = reguardadas.iter().filter(|x| x.0 >= u && x.0 >= x.1).count() as f64;
        let de = |clase: &str| mano.iter().filter(|x| x.0 == clase && x.1 >= u).count() as f64 / cuenta(clase);
        println!(
            "{u:.2}    {:>6.1}%            {:>6.1}%  {:>6.1}%       |       {:>4.0}% {:>4.0}% {:>4.0}%",
            100.0 * fp / n_d,
            100.0 * avisa / n_r,
            100.0 * bien / n_r,
            100.0 * de("dup"),
            100.0 * de("rel"),
            100.0 * de("otro")
        );
    }
    distintas.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("
las distintas más parecidas:");
    for (s, f, otra) in distintas.iter().take(10) {
        println!("  {s:.3}  {f}  ~  {otra}");
    }
    let mut r = reguardadas.clone();
    r.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("reguardadas con menos similitud con la suya: {:?}", r.iter().take(8).map(|x| (x.0 * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// Una candidata a duplicada: (coseno centrado, (cobertura IDF, palabras raras compartidas)).
type Candidata = (f32, (f64, usize));

/// Como `dupreal`, pero con una segunda señal además del vector: la cobertura IDF (qué parte del
/// peso de las palabras de la nueva está en la candidata, con el IDF de Lucene sobre las notas, el
/// mismo del recall). Regla: duplicada si coseno ≥ a, o si coseno ≥ b y cobertura ≥ k.
fn dupcob(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{parecidas, texto};
    use std::collections::{BTreeSet, HashMap};
    let m = modelo()?;
    let fichas = |s: &str| -> BTreeSet<String> {
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
    let mut notas: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("\n---") else { continue };
        let (cabecera, cuerpo) = (&r[..i], r[i + 4..].trim());
        let Some(titulo) = cabecera.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let titulo: String = titulo.trim().trim_matches('"').chars().take(120).collect();
        notas.push((f, titulo, cuerpo.to_string()));
    }
    notas.sort();
    let indice = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut resumen_de = HashMap::new();
    for l in indice.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let titulo = &l[l.find('[').unwrap_or(0) + 1..i];
        let resumen = l[k + " — ".len()..].trim().trim_end_matches('…');
        resumen_de.insert(format!("{}.md", &l[i + 2..j]), format!("{titulo}: {resumen}"));
    }
    // df sobre todas las notas.
    let fichas_nota: HashMap<String, BTreeSet<String>> = notas.iter().map(|(f, t, c)| (f.clone(), fichas(&texto(t, c)))).collect();
    let mut df: HashMap<String, f64> = HashMap::new();
    for fs in fichas_nota.values() {
        for t in fs {
            *df.entry(t.clone()).or_default() += 1.0;
        }
    }
    let n = notas.len() as f64;
    let idf = |t: &str| {
        let d = df.get(t).copied().unwrap_or(0.0).max(1.0);
        (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
    };
    // (cobertura, palabras raras compartidas: en como mucho el 20 % de las notas)
    let cobertura = |nueva: &BTreeSet<String>, candidata: &BTreeSet<String>| -> (f64, usize) {
        let total: f64 = nueva.iter().map(|t| idf(t)).sum();
        let dentro: f64 = nueva.iter().filter(|t| candidata.contains(*t)).map(|t| idf(t)).sum();
        let raras = nueva.iter().filter(|t| candidata.contains(*t) && df.get(*t).copied().unwrap_or(0.0) <= 0.2 * n).count();
        (if total > 0.0 { dentro / total } else { 0.0 }, raras)
    };

    let tmp = std::env::temp_dir().join(format!("omoi-dupcob-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    let mut fichas_id: HashMap<String, BTreeSet<String>> = HashMap::new();
    // distintas: candidatas (coseno, cobertura) de cada nota contra las anteriores; todas son falsos.
    let mut distintas: Vec<Vec<Candidata>> = Vec::new();
    for (f, t, c) in &notas {
        let nueva = &fichas_nota[f];
        let p = parecidas(&db, &m, &texto(t, c), Some("/p"), None, -1.0, 5)?;
        distintas.push(p.iter().map(|(x, s)| (*s, cobertura(nueva, &fichas_id[&x.id]))).collect());
        let nn = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        let id = memoria::guardar(&mut db, nn, Some(&m))?.id;
        fichero_de.insert(id.clone(), f.clone());
        fichas_id.insert(id, nueva.clone());
    }
    // reguardadas: (la suya, las otras candidatas).
    let mut reguardadas: Vec<(Option<Candidata>, Vec<Candidata>)> = Vec::new();
    for (f, _, c) in &notas {
        let Some(titulo) = resumen_de.get(f) else { continue };
        let principio: String = c.chars().take(300).collect();
        let txt = texto(titulo, &principio);
        let nueva = fichas(&txt);
        let p = parecidas(&db, &m, &txt, Some("/p"), None, -1.0, 5)?;
        let mut suya = None;
        let mut otras = Vec::new();
        for (x, s) in &p {
            let par = (*s, cobertura(&nueva, &fichas_id[&x.id]));
            if &fichero_de[&x.id] == f { suya = Some(par) } else { otras.push(par) }
        }
        reguardadas.push((suya, otras));
    }
    // a mano: (clase, coseno, cobertura de b sobre a).
    let mut mano: Vec<(String, f32, (f64, usize))> = Vec::new();
    for (i, l) in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).enumerate() {
        let c: Vec<&str> = l.split('\t').collect();
        let proyecto = format!("/cal{i}");
        let nn = Nueva { project: proyecto.clone(), kind: Some(Tipo::Fact), title: c[1].into(), content: c[1].into(), ..Default::default() };
        let a = memoria::guardar(&mut db, nn, Some(&m))?.id;
        let p = parecidas(&db, &m, &texto(c[2], c[2]), Some(&proyecto), None, -1.0, 50)?;
        let s = p.iter().find(|(x, _)| x.id == a).map_or(-1.0, |x| x.1);
        mano.push((c[0].to_string(), s, cobertura(&fichas(c[2]), &fichas(c[1]))));
    }

    // cortas: cada frase de los pares contra las notas (ninguna es duplicada de una nota).
    let mut cortas: Vec<Vec<Candidata>> = Vec::new();
    for l in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        for frase in l.split('\t').skip(1) {
            let nueva = fichas(frase);
            let p = parecidas(&db, &m, &texto(frase, frase), Some("/p"), None, -1.0, 5)?;
            cortas.push(p.iter().filter(|(x, _)| fichas_id.contains_key(&x.id)).map(|(x, s)| (*s, cobertura(&nueva, &fichas_id[&x.id]))).collect());
        }
    }
    let nc = cortas.len() as f64;
    let nd = distintas.len() as f64;
    let nr = reguardadas.len() as f64;
    let total_clase = |k: &str| mano.iter().filter(|x| x.0 == k).count() as f64;
    println!("{} distintas, {} reguardadas, {} a mano", distintas.len(), reguardadas.len(), mano.len());
    println!("  a     b     k  r  | falsos largas cortas | reguardadas: la suya  otra  | a mano: dup  rel  otro");
    let mut filas = Vec::new();
    for a in [0.70f32, 0.75, 0.78, 0.80, 0.85, 2.0] {
        for b in [0.30f32, 0.40, 0.50, 0.60] {
            for k in [0.40f64, 0.50, 0.60, 0.70] {
              for r in [0usize, 2, 3] {
                let dup = |c: &Candidata| c.0 >= a || (c.0 >= b && c.1.0 >= k && c.1.1 >= r);
                let fp = distintas.iter().filter(|v| v.iter().any(dup)).count() as f64 / nd;
                let fc = cortas.iter().filter(|v| v.iter().any(dup)).count() as f64 / nc;
                let suya = reguardadas.iter().filter(|r| r.0.as_ref().is_some_and(dup)).count() as f64 / nr;
                let otra = reguardadas.iter().filter(|r| r.1.iter().any(dup)).count() as f64 / nr;
                let de = |cl: &str| mano.iter().filter(|x| x.0 == cl && dup(&(x.1, x.2))).count() as f64 / total_clase(cl);
                filas.push((a, b, k, r, fp, fc, suya, otra, de("dup"), de("rel"), de("otro")));
              }
            }
        }
    }
    // Las mejores sin falsos en las distintas ni en los rel, por las reguardadas que caza.
    filas.sort_by(|x, y| (x.4 + x.5 + x.7 + x.9).total_cmp(&(y.4 + y.5 + y.7 + y.9)).then(y.6.total_cmp(&x.6)).then(y.8.total_cmp(&x.8)));
    for f in filas.iter().take(25) {
        println!(
            "{:.2}  {:.2}  {:.2}  {}  | {:>5.1}%  {:>5.1}%  |  {:>5.1}%  {:>5.1}%  |  {:>4.0}% {:>4.0}% {:>4.0}%",
            f.0, f.1, f.2, f.3, 100.0 * f.4, 100.0 * f.5, 100.0 * f.6, 100.0 * f.7, 100.0 * f.8, 100.0 * f.9, 100.0 * f.10
        );
    }
    println!("\nla regla actual (solo coseno ≥ 0,70):");
    for f in filas.iter().filter(|f| f.0 == 0.70 && f.1 == 0.60 && f.2 == 0.70 && f.3 == 0) {
        println!("  falsos {:.1}% / cortas {:.1}%, la suya {:.1}%, a mano dup {:.0}% rel {:.0}%", 100.0 * f.4, 100.0 * f.5, 100.0 * f.6, 100.0 * f.8, 100.0 * f.9);
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// El aviso de duplicados de producción (`vectores::duplicada`) con las notas reales de `dir`:
/// distintas (cada nota antes de guardarla: todo aviso es falso), reguardadas (cada nota dicha otra
/// vez: debe avisar, y de la suya) y frases cortas de los pares contra las notas (todo aviso es falso).
fn dupprod(dir: &std::path::Path) -> Result<()> {
    use omoikane::memoria::{self, Nueva, Tipo};
    use omoikane::vectores::{duplicada, relacionadas};
    use std::collections::HashMap;
    use std::time::Instant;
    let m = modelo()?;
    let mut notas: Vec<(String, String, String)> = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let Some(r) = t.strip_prefix("---") else { continue };
        let Some(i) = r.find("\n---") else { continue };
        let (cabecera, cuerpo) = (&r[..i], r[i + 4..].trim());
        let Some(titulo) = cabecera.lines().find_map(|l| l.trim().strip_prefix("description:")) else { continue };
        let titulo: String = titulo.trim().trim_matches('"').chars().take(120).collect();
        notas.push((f, titulo, cuerpo.to_string()));
    }
    notas.sort();
    let indice = std::fs::read_to_string(dir.join("MEMORY.md"))?;
    let mut resumen_de = HashMap::new();
    for l in indice.lines() {
        let (Some(i), Some(j), Some(k)) = (l.find("]("), l.find(".md)"), l.find(" — ")) else { continue };
        let titulo = &l[l.find('[').unwrap_or(0) + 1..i];
        let resumen = l[k + " — ".len()..].trim().trim_end_matches('…');
        resumen_de.insert(format!("{}.md", &l[i + 2..j]), format!("{titulo}: {resumen}"));
    }
    let tmp = std::env::temp_dir().join(format!("omoi-dupprod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    let (mut falsos, mut tiempos, mut distintas_con_rel) = (Vec::new(), Vec::new(), 0);
    for (f, t, c) in &notas {
        let ahora = Instant::now();
        if let Some((otra, s)) = duplicada(&db, &m, "/p", t, c)? {
            falsos.push(format!("{s:.3} {f} ~ {}", otra.title.chars().take(50).collect::<String>()));
        } else if !relacionadas(&db, "/p", t, "", 3)?.is_empty() {
            distintas_con_rel += 1;
        }
        tiempos.push(ahora.elapsed().as_secs_f64() * 1000.0);
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    let (mut suya, mut otra, mut total, mut en_rel) = (0, 0, 0, 0);
    for (f, _, c) in &notas {
        let Some(titulo) = resumen_de.get(f) else { continue };
        total += 1;
        let principio: String = c.chars().take(300).collect();
        match duplicada(&db, &m, "/p", titulo, &principio)? {
            Some((x, _)) if &fichero_de[&x.id] == f => suya += 1,
            Some(_) => otra += 1,
            None => {
                en_rel += usize::from(relacionadas(&db, "/p", titulo, "", 3)?.iter().any(|x| &fichero_de[&x.id] == f));
            }
        }
    }
    let (mut cortas, mut falsas_cortas, mut cortas_con_rel) = (0, Vec::new(), 0);
    for l in include_str!("../tests/pares_duplicados.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        for frase in l.split('\t').skip(1) {
            cortas += 1;
            if let Some((x, s)) = duplicada(&db, &m, "/p", frase, frase)? {
                falsas_cortas.push(format!("{s:.3} «{frase}» ~ {}", x.title.chars().take(40).collect::<String>()));
            } else if !relacionadas(&db, "/p", frase, "", 3)?.is_empty() {
                cortas_con_rel += 1;
            }
        }
    }
    tiempos.sort_by(f64::total_cmp);
    println!("distintas: {} avisos en falso de {}", falsos.len(), notas.len());
    for x in &falsos {
        println!("  {x}");
    }
    println!("reguardadas: {suya} de {total} avisan de la suya ({:.1} %), {otra} de otra", 100.0 * suya as f64 / total as f64);
    println!("  de las que no avisan, la suya sale entre las relacionadas en {en_rel}: cubiertas {} de {total} ({:.1} %)", suya + en_rel, 100.0 * (suya + en_rel) as f64 / total as f64);
    println!("distintas con relacionadas (no bloquea): {distintas_con_rel} de {}", notas.len());
    println!("cortas: {} avisos en falso de {cortas}; con relacionadas (no bloquea): {cortas_con_rel}", falsas_cortas.len());
    for x in &falsas_cortas {
        println!("  {x}");
    }
    println!("tiempo de la comprobación: mediana {:.1} ms, p95 {:.1} ms", tiempos[tiempos.len() / 2], tiempos[tiempos.len() * 95 / 100]);
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}
