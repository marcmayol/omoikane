//! Fase 4: ¿cuándo inyectar memorias en cada mensaje? Con datos reales que no se suben al repo.
//!
//!   cargo run --release --example fase4 -- recall NOTAS TRANSCRIPCIONES
//!
//! NOTAS: carpeta de memorias de Claude Code (un .md por nota + MEMORY.md con «- [Título](f.md) — resumen»).
//! TRANSCRIPCIONES: carpeta con .jsonl de sesiones de Claude Code; sus mensajes del usuario son los
//! mensajes reales con los que medir cuánto se inyecta (y si tiene sentido, mirando una muestra).

use anyhow::{Result, anyhow};
use omoikane::buscar::{self, Opciones, Resultado};
use omoikane::embeber::Modelo;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use std::collections::HashMap;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match (args.first().map(String::as_str), args.get(1), args.get(2)) {
        (Some("recall"), Some(n), Some(t)) => recall(Path::new(n), Path::new(t)),
        (Some("produccion"), Some(n), Some(t)) => produccion(Path::new(n), Path::new(t)),
        _ => Err(anyhow!("uso: fase4 recall NOTAS TRANSCRIPCIONES")),
    }
}

fn notas(dir: &Path) -> Result<Vec<(String, String, String)>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let (cab, cuerpo) = match t.strip_prefix("---").and_then(|r| r.find("\n---").map(|i| (&r[..i], r[i + 4..].trim()))) {
            Some(x) => x,
            None => ("", t.as_str()),
        };
        let titulo = cab
            .lines()
            .find_map(|l| l.trim().strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').chars().take(200).collect())
            .unwrap_or_else(|| f.trim_end_matches(".md").to_string());
        if !cuerpo.is_empty() {
            v.push((f, titulo, cuerpo.to_string()));
        }
    }
    Ok(v)
}

/// Mensajes que el usuario escribió de verdad (no resultados de herramientas ni avisos del sistema).
fn mensajes(dir: &Path) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let mut pila = vec![dir.to_path_buf()];
    while let Some(p) = pila.pop() {
        for e in std::fs::read_dir(&p)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                pila.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                for l in std::fs::read_to_string(&p).unwrap_or_default().lines() {
                    let Ok(j) = serde_json::from_str::<serde_json::Value>(l) else { continue };
                    if j["type"] != "user" || j["message"]["role"] != "user" || j["isMeta"] == true {
                        continue;
                    }
                    let texto = match &j["message"]["content"] {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => a
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => continue,
                    };
                    let t = texto.trim();
                    if t.len() >= 15 && !t.starts_with('<') && !t.starts_with('[') && !t.starts_with("Caveat") {
                        v.push(t.to_string());
                    }
                }
            }
        }
    }
    v.sort();
    v.dedup();
    Ok(v)
}

/// Lo que se inyectaría con un criterio: las primeras `max` que tienen al menos `minimo` palabras
/// de la consulta, o que solo salieron por significado con similitud ≥ `sim`.
fn elegir(r: &[Resultado], minimo: usize, sim: f32, max: usize) -> Vec<&Resultado> {
    r.iter()
        .filter(|x| x.matched_words >= minimo || (x.matched_words == 0 && x.similarity.unwrap_or(-1.0) >= sim))
        .take(max)
        .collect()
}

fn recall(dir_notas: &Path, dir_trans: &Path) -> Result<()> {
    let m = Modelo::abrir(&dirs::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi"))?;
    let notas = notas(dir_notas)?;
    let tmp = std::env::temp_dir().join(format!("omoi-fase4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    for (f, t, c) in &notas {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f.clone());
    }
    omoikane::vectores::sincronizar(&mut db, &m)?;
    let indice = std::fs::read_to_string(dir_notas.join("MEMORY.md"))?;
    let positivos: Vec<(String, String)> = indice
        .lines()
        .filter_map(|l| {
            let (i, j, k) = (l.find("](")?, l.find(".md)")?, l.find(" — ")?);
            Some((format!("{}.md", &l[i + 2..j]), l[k + " — ".len()..].trim().trim_end_matches('…').to_string()))
        })
        .filter(|(f, q)| fichero_de.values().any(|x| x == f) && q.len() > 10)
        .collect();
    let mut reales = mensajes(dir_trans)?;
    // Muestra fija de 300, repartida por todo el conjunto.
    if reales.len() > 300 {
        let paso = reales.len() as f64 / 300.0;
        reales = (0..300).map(|i| reales[(i as f64 * paso) as usize].clone()).collect();
    }
    println!("{} notas · {} positivos (resúmenes de MEMORY.md) · {} mensajes reales\n", notas.len(), positivos.len(), reales.len());

    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 10 };
    let res_pos: Vec<(String, Vec<Resultado>)> =
        positivos.iter().map(|(f, q)| Ok((f.clone(), buscar::buscar(&mut db, q, &o, Some(&m))?))).collect::<Result<_>>()?;
    let res_real: Vec<(String, Vec<Resultado>)> =
        reales.iter().map(|q| Ok((q.clone(), buscar::buscar(&mut db, q, &o, Some(&m))?))).collect::<Result<_>>()?;

    // Cobertura ponderada por IDF: la parte del «peso» de la consulta que está en la memoria.
    let toks = |t: &str| -> std::collections::BTreeSet<String> {
        sin_tildes(&t.to_lowercase()).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
    };
    let docs: Vec<std::collections::BTreeSet<String>> = notas.iter().map(|(_, t, c)| toks(&format!("{t} {c}"))).collect();
    let n_docs = docs.len() as f64;
    let pref: usize = std::env::var("PREFIJO").ok().and_then(|x| x.parse().ok()).unwrap_or(3);
    let esta = |t: &str, d: &std::collections::BTreeSet<String>| {
        if t.chars().count() >= pref { d.iter().any(|p| p.starts_with(t)) } else { d.contains(t) }
    };
    let idf = |t: &str| -> f64 {
        let df = docs.iter().filter(|d| esta(t, d)).count() as f64;
        ((n_docs - df + 0.5) / (df + 0.5)).ln().max(0.0)
    };
    let wc = |q: &str, r: &Resultado| -> f64 {
        let ts: Vec<String> = buscar::terminos(q).iter().map(|t| sin_tildes(t)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let d = toks(&format!("{} {}", r.memoria.title, r.memoria.content));
        let (mut a, mut b) = (0.0, 0.0);
        for t in &ts {
            let w = idf(t);
            b += w;
            if esta(t, &d) {
                a += w;
            }
        }
        if b == 0.0 { 0.0 } else { a / b }
    };
    println!("{:<34} {:>12} {:>10}   {:>16} {:>11}", "criterio (cobertura IDF)", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for umbral in [0.15f64, 0.25, 0.35, 0.45] {
        for relativo in [0.0f64, 0.7] {
            for max in [1usize, 3] {
                let eleg = |q: &str, r: &[Resultado]| -> Vec<String> {
                    let mut v: Vec<(f64, &Resultado)> = r.iter().map(|x| (wc(q, x), x)).filter(|(w, _)| *w >= umbral).collect();
                    v.sort_by(|a, b| b.0.total_cmp(&a.0));
                    let mejor = v.first().map(|x| x.0).unwrap_or(0.0);
                    v.into_iter().filter(|(w, _)| *w >= relativo * mejor).take(max).map(|(_, x)| x.memoria.id.clone()).collect()
                };
                let (mut ok, mut iny) = (0, 0);
                for ((f, r), (_, q)) in res_pos.iter().zip(&positivos) {
                    let e = eleg(q, r);
                    ok += usize::from(e.iter().any(|id| &fichero_de[id] == f));
                    iny += e.len();
                }
                let (mut con, mut iny_r) = (0, 0);
                for (q, r) in &res_real {
                    let e = eleg(q, r);
                    con += usize::from(!e.is_empty());
                    iny_r += e.len();
                }
                println!(
                    "≥{umbral:.2}, ≥{relativo:.1}×mejor, máx {max}            {:>11.0}% {:>10.1}   {:>15.0}% {:>11.1}",
                    100.0 * ok as f64 / res_pos.len() as f64,
                    iny as f64 / res_pos.len() as f64,
                    100.0 * con as f64 / res_real.len() as f64,
                    iny_r as f64 / res_real.len() as f64
                );
            }
        }
    }
    // Criterio completo: términos útiles (≥ 3 letras y no solo cifras), al menos `min_terminos` en la
    // consulta, al menos `min_raros` en común que estén en ≤ 20 % de las notas, y cobertura IDF ≥ u.
    let raro_max: f64 = std::env::var("RARO").ok().and_then(|x| x.parse().ok()).unwrap_or(0.2);

    let charla = std::env::var("CHARLA").is_ok();
    const CHARLA: &[&str] = &[
        "pues", "dale", "vale", "okey", "ahora", "bueno", "hay", "pon", "mira", "eso", "esto", "esta", "este", "todo",
        "toda", "todos", "algo", "hacer", "hace", "haz", "tengo", "tiene", "puedo", "puede", "quiero", "mejor", "bien",
        "mal", "asi", "aqui", "solo", "tambien", "cosas", "cosa", "otro", "otra", "otros", "otras", "ver", "sea", "ser",
        "estar", "porque", "cuando", "donde", "cual", "igual", "menos", "mas", "nada", "sin", "entonces", "creo", "hecho",
        "dime", "dame", "sigue", "seguimos", "listo", "perfecto", "gracias", "favor", "vamos", "ves", "voy", "van",
        "ido", "ese", "esa", "esos", "esas", "aun", "ya", "repite", "repito", "prueba", "mismo", "misma", "cada", "pero",
        "the", "this", "that", "with", "please", "now", "can", "you", "it",
    ];
    let utiles = |q: &str| -> Vec<String> {
        buscar::terminos(q)
            .iter()
            .map(|t| sin_tildes(t))
            .filter(|t| t.chars().count() >= 3 && !t.chars().all(|c| c.is_ascii_digit()))
            .filter(|t| !charla || !CHARLA.contains(&t.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let df = |t: &str| docs.iter().filter(|d| esta(t, d)).count() as f64;
    let completo = |q: &str, r: &[Resultado], u: f64, min_terminos: usize, min_raros: usize| -> Vec<(f64, String)> {
        let ts = utiles(q);
        if ts.len() < min_terminos {
            return vec![];
        }
        let total: f64 = ts.iter().map(|t| idf(t)).sum();
        if total == 0.0 {
            return vec![];
        }
        let mut v: Vec<(f64, String)> = r
            .iter()
            .filter_map(|x| {
                let d = toks(&format!("{} {}", x.memoria.title, x.memoria.content));
                let comunes: Vec<&String> = ts.iter().filter(|t| esta(t, &d)).collect();
                let raros = comunes.iter().filter(|t| df(t) <= raro_max * n_docs).count();
                let w = comunes.iter().map(|t| idf(t)).sum::<f64>() / total;
                (raros >= min_raros && w >= u).then(|| (w, x.memoria.id.clone()))
            })
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mejor = v.first().map(|x| x.0).unwrap_or(0.0);
        v.into_iter().filter(|(w, _)| *w >= 0.7 * mejor).take(3).collect()
    };
    println!("
{:<40} {:>12} {:>10}   {:>16} {:>11}", "criterio completo", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for (u, mt, mr) in [(0.35, 1, 1), (0.35, 3, 1), (0.35, 3, 2), (0.30, 3, 2), (0.25, 3, 2), (0.40, 3, 2), (0.35, 4, 2), (0.35, 3, 3)] {
        let (mut ok, mut iny) = (0, 0);
        for ((f, r), (_, q)) in res_pos.iter().zip(&positivos) {
            let e = completo(q, r, u, mt, mr);
            ok += usize::from(e.iter().any(|(_, id)| &fichero_de[id] == f));
            iny += e.len();
        }
        let (mut con, mut iny_r) = (0, 0);
        for (q, r) in &res_real {
            let e = completo(q, r, u, mt, mr);
            con += usize::from(!e.is_empty());
            iny_r += e.len();
        }
        println!(
            "IDF≥{u:.2}, ≥{mt} términos, ≥{mr} raros{:>16} {:>11.0}% {:>10.1}   {:>15.0}% {:>11.1}",
            "",
            100.0 * ok as f64 / res_pos.len() as f64,
            iny as f64 / res_pos.len() as f64,
            100.0 * con as f64 / res_real.len() as f64,
            iny_r as f64 / res_real.len() as f64
        );
    }
    if let Ok(c) = std::env::var("COMPLETO") {
        let x: Vec<f64> = c.split(',').map(|v| v.parse().unwrap()).collect();
        println!("
muestra con el criterio completo {c}:");
        let mut n = 0;
        for (q, r) in &res_real {
            let e = completo(q, r, x[0], x[1] as usize, x[2] as usize);
            if e.is_empty() {
                continue;
            }
            n += 1;
            if n > 40 {
                break;
            }
            let corto: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
            println!("· «{corto}»");
            for (w, id) in e {
                println!("      {:.2} {}", w, fichero_de[&id]);
            }
        }
        return Ok(());
    }
    if let Ok(u) = std::env::var("UMBRAL") {
        let u: f64 = u.parse()?;
        println!("
muestra con cobertura IDF ≥ {u}, ≥0.7×mejor, máx 3:");
        let mut n = 0;
        for (q, r) in &res_real {
            let mut v: Vec<(f64, &Resultado)> = r.iter().map(|x| (wc(q, x), x)).filter(|(w, _)| *w >= u).collect();
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mejor = v.first().map(|x| x.0).unwrap_or(0.0);
            let v: Vec<_> = v.into_iter().filter(|(w, _)| *w >= 0.7 * mejor).take(3).collect();
            if v.is_empty() {
                continue;
            }
            n += 1;
            if n > 30 {
                break;
            }
            let corto: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
            println!("· «{corto}»");
            for (w, x) in v {
                println!("      {:.2} {}", w, fichero_de[&x.memoria.id]);
            }
        }
        return Ok(());
    }

    println!("
{:<26} {:>12} {:>10}   {:>18} {:>12}", "criterio", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for minimo in [1usize, 2, 3, 4] {
        for sim in [0.30f32, 0.40, 2.0] {
            let nombre = format!("≥{minimo} palabras, vector≥{}", if sim > 1.0 { "no".into() } else { format!("{sim:.2}") });
            let (mut ok, mut iny) = (0, 0);
            for (f, r) in &res_pos {
                let e = elegir(r, minimo, sim, 3);
                ok += usize::from(e.iter().any(|x| &fichero_de[&x.memoria.id] == f));
                iny += e.len();
            }
            let (mut con, mut iny_r) = (0, 0);
            for (_, r) in &res_real {
                let e = elegir(r, minimo, sim, 3);
                con += usize::from(!e.is_empty());
                iny_r += e.len();
            }
            println!(
                "{nombre:<26} {:>11.0}% {:>10.1}   {:>17.0}% {:>12.1}",
                100.0 * ok as f64 / res_pos.len() as f64,
                iny as f64 / res_pos.len() as f64,
                100.0 * con as f64 / res_real.len() as f64,
                iny_r as f64 / res_real.len() as f64
            );
        }
    }
    let (minimo, sim) = (
        std::env::var("MINIMO").ok().and_then(|x| x.parse().ok()).unwrap_or(3),
        std::env::var("SIM").ok().and_then(|x| x.parse().ok()).unwrap_or(2.0f32),
    );
    println!("\nmuestra con ≥{minimo} palabras (vector ≥ {sim}): mensaje → lo que se inyectaría");
    for (q, r) in res_real.iter().filter(|(_, r)| !elegir(r, minimo, sim, 3).is_empty()).take(25) {
        let corto: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
        println!("· «{corto}»");
        for x in elegir(r, minimo, sim, 3) {
            println!("      {} ({} pal.)", fichero_de[&x.memoria.id], x.matched_words);
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

fn sin_tildes(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            c => c,
        })
        .collect()
}

/// El criterio de verdad (`recall::relevantes`, con sus constantes) sobre los mismos datos, rápido.
fn produccion(dir_notas: &Path, dir_trans: &Path) -> Result<()> {
    let notas = notas(dir_notas)?;
    let tmp = std::env::temp_dir().join(format!("omoi-fase4p-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    // Con modelo, también el canal por significado del recall (como en el hook).
    let modelo = omoikane::modelos::abrir_actual()?;
    for (f, t, c) in &notas {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, modelo.as_ref())?.id, f.clone());
    }
    let todas = |db: &mut rusqlite::Connection, q: &str| -> Result<Vec<(omoikane::memoria::Memoria, f64)>> {
        let mut r = omoikane::recall::relevantes(db, "/p", q)?;
        if let Some(m) = &modelo {
            for x in omoikane::recall::por_significado(db, "/p", q, m)? {
                if !r.iter().any(|(y, _)| y.id == x.id) && r.len() < omoikane::recall::MAXIMO {
                    r.push((x, 0.0));
                }
            }
        }
        Ok(r)
    };
    let indice = std::fs::read_to_string(dir_notas.join("MEMORY.md"))?;
    let positivos: Vec<(String, String)> = indice
        .lines()
        .filter_map(|l| {
            let (i, j, k) = (l.find("](")?, l.find(".md)")?, l.find(" — ")?);
            Some((format!("{}.md", &l[i + 2..j]), l[k + " — ".len()..].trim().trim_end_matches('…').to_string()))
        })
        .filter(|(f, q)| fichero_de.values().any(|x| x == f) && q.len() > 10)
        .collect();
    let mut reales = mensajes(dir_trans)?;
    if reales.len() > 300 {
        let paso = reales.len() as f64 / 300.0;
        reales = (0..300).map(|i| reales[(i as f64 * paso) as usize].clone()).collect();
    }
    let t = std::time::Instant::now();
    let (mut ok, mut iny) = (0, 0);
    for (f, q) in &positivos {
        let r = todas(&mut db, q)?;
        ok += usize::from(r.iter().any(|(m, _)| &fichero_de[&m.id] == f));
        iny += r.len();
    }
    let (mut con, mut iny_r) = (0, 0);
    let mut muestra = Vec::new();
    for q in &reales {
        let r = todas(&mut db, q)?;
        con += usize::from(!r.is_empty());
        iny_r += r.len();
        if !r.is_empty() {
            muestra.push((q.clone(), r.iter().map(|(m, c)| format!("{c:.2} {}", fichero_de[&m.id])).collect::<Vec<_>>()));
        }
    }
    let n = (positivos.len() + reales.len()) as f64;
    println!(
        "positivos: {:.0}% con su nota ({:.1} inyectadas de media) · reales: {:.0}% con algo ({:.2} de media) · {:.1} ms por mensaje",
        100.0 * ok as f64 / positivos.len() as f64,
        iny as f64 / positivos.len() as f64,
        100.0 * con as f64 / reales.len() as f64,
        iny_r as f64 / reales.len() as f64,
        t.elapsed().as_secs_f64() * 1000.0 / n
    );
    for (q, r) in muestra.iter().take(60) {
        let corto: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(95).collect();
        println!("· «{corto}»  →  {}", r.join(" | "));
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}
