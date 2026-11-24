//! Phase 4: when to inject memories into each message? With real data that is not uploaded to the repo.
//!
//!   cargo run --release --example phase4 -- recall NOTAS TRANSCRIPCIONES
//!
//! NOTAS: Claude Code memory folder (one .md per note + MEMORY.md with «- [Title](f.md) — summary»).
//! TRANSCRIPCIONES: folder with Claude Code session .jsonl files; their user messages are the
//! real messages used to measure how much gets injected (and whether it makes sense, by looking at a sample).

use anyhow::{Result, anyhow};
use omoikane::search::{self, SearchOptions, SearchResult};
use omoikane::embed::Model;
use omoikane::memory::{self, Scope, NewMemory, Kind};
use std::collections::HashMap;
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match (args.first().map(String::as_str), args.get(1), args.get(2)) {
        (Some("recall"), Some(n), Some(t)) => recall(Path::new(n), Path::new(t)),
        (Some("produccion"), Some(n), Some(t)) => production(Path::new(n), Path::new(t)),
        _ => Err(anyhow!("uso: fase4 recall NOTAS TRANSCRIPCIONES")),
    }
}

fn notes(dir: &Path) -> Result<Vec<(String, String, String)>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if !f.ends_with(".md") || f == "MEMORY.md" {
            continue;
        }
        let t = std::fs::read_to_string(e.path())?;
        let (header, body) = match t.strip_prefix("---").and_then(|r| r.find("\n---").map(|i| (&r[..i], r[i + 4..].trim()))) {
            Some(x) => x,
            None => ("", t.as_str()),
        };
        let title = header
            .lines()
            .find_map(|l| l.trim().strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').chars().take(200).collect())
            .unwrap_or_else(|| f.trim_end_matches(".md").to_string());
        if !body.is_empty() {
            v.push((f, title, body.to_string()));
        }
    }
    Ok(v)
}

/// Messages the user actually wrote (not tool results or system notices).
fn messages(dir: &Path) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p)?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                for l in std::fs::read_to_string(&p).unwrap_or_default().lines() {
                    let Ok(j) = serde_json::from_str::<serde_json::Value>(l) else { continue };
                    if j["type"] != "user" || j["message"]["role"] != "user" || j["isMeta"] == true {
                        continue;
                    }
                    let text = match &j["message"]["content"] {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Array(a) => a
                            .iter()
                            .filter(|b| b["type"] == "text")
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => continue,
                    };
                    let t = text.trim();
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

/// What would be injected with a criterion: the first `max` that have at least `min_words` words
/// from the query, or that only came up by meaning with similarity ≥ `sim`.
fn choose(r: &[SearchResult], min_words: usize, sim: f32, max: usize) -> Vec<&SearchResult> {
    r.iter()
        .filter(|x| x.matched_words >= min_words || (x.matched_words == 0 && x.similarity.unwrap_or(-1.0) >= sim))
        .take(max)
        .collect()
}

fn recall(notes_dir: &Path, transcripts_dir: &Path) -> Result<()> {
    let m = Model::open(&std::env::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-omoi"))?;
    let notes = notes(notes_dir)?;
    let tmp = std::env::temp_dir().join(format!("omoi-fase4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::open(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    for (f, t, c) in &notes {
        let n = NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        file_of.insert(memory::save(&mut db, n, Some(&m))?.id, f.clone());
    }
    omoikane::vectors::sync(&mut db, &m)?;
    let index = std::fs::read_to_string(notes_dir.join("MEMORY.md"))?;
    let positives: Vec<(String, String)> = index
        .lines()
        .filter_map(|l| {
            let (i, j, k) = (l.find("](")?, l.find(".md)")?, l.find(" — ")?);
            Some((format!("{}.md", &l[i + 2..j]), l[k + " — ".len()..].trim().trim_end_matches('…').to_string()))
        })
        .filter(|(f, q)| file_of.values().any(|x| x == f) && q.len() > 10)
        .collect();
    let mut real = messages(transcripts_dir)?;
    // Fixed sample of 300, spread across the whole set.
    if real.len() > 300 {
        let step = real.len() as f64 / 300.0;
        real = (0..300).map(|i| real[(i as f64 * step) as usize].clone()).collect();
    }
    println!("{} notas · {} positivos (resúmenes de MEMORY.md) · {} mensajes reales\n", notes.len(), positives.len(), real.len());

    let o = SearchOptions { scope: Scope::All, kind: None, tags: vec![], limit: 10 };
    let res_pos: Vec<(String, Vec<SearchResult>)> =
        positives.iter().map(|(f, q)| Ok((f.clone(), search::search(&mut db, q, &o, Some(&m))?))).collect::<Result<_>>()?;
    let res_real: Vec<(String, Vec<SearchResult>)> =
        real.iter().map(|q| Ok((q.clone(), search::search(&mut db, q, &o, Some(&m))?))).collect::<Result<_>>()?;

    // IDF-weighted coverage: the part of the query's «weight» that is in the memory.
    let toks = |t: &str| -> std::collections::BTreeSet<String> {
        strip_accents(&t.to_lowercase()).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
    };
    let docs: Vec<std::collections::BTreeSet<String>> = notes.iter().map(|(_, t, c)| toks(&format!("{t} {c}"))).collect();
    let n_docs = docs.len() as f64;
    let pref: usize = std::env::var("PREFIJO").ok().and_then(|x| x.parse().ok()).unwrap_or(3);
    let has = |t: &str, d: &std::collections::BTreeSet<String>| {
        if t.chars().count() >= pref { d.iter().any(|p| p.starts_with(t)) } else { d.contains(t) }
    };
    let idf = |t: &str| -> f64 {
        let df = docs.iter().filter(|d| has(t, d)).count() as f64;
        ((n_docs - df + 0.5) / (df + 0.5)).ln().max(0.0)
    };
    let wc = |q: &str, r: &SearchResult| -> f64 {
        let ts: Vec<String> = search::terms(q).iter().map(|t| strip_accents(t)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let d = toks(&format!("{} {}", r.memory.title, r.memory.content));
        let (mut a, mut b) = (0.0, 0.0);
        for t in &ts {
            let w = idf(t);
            b += w;
            if has(t, &d) {
                a += w;
            }
        }
        if b == 0.0 { 0.0 } else { a / b }
    };
    println!("{:<34} {:>12} {:>10}   {:>16} {:>11}", "criterio (cobertura IDF)", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for threshold in [0.15f64, 0.25, 0.35, 0.45] {
        for relative in [0.0f64, 0.7] {
            for max in [1usize, 3] {
                let pick = |q: &str, r: &[SearchResult]| -> Vec<String> {
                    let mut v: Vec<(f64, &SearchResult)> = r.iter().map(|x| (wc(q, x), x)).filter(|(w, _)| *w >= threshold).collect();
                    v.sort_by(|a, b| b.0.total_cmp(&a.0));
                    let best = v.first().map(|x| x.0).unwrap_or(0.0);
                    v.into_iter().filter(|(w, _)| *w >= relative * best).take(max).map(|(_, x)| x.memory.id.clone()).collect()
                };
                let (mut ok, mut injected) = (0, 0);
                for ((f, r), (_, q)) in res_pos.iter().zip(&positives) {
                    let e = pick(q, r);
                    ok += usize::from(e.iter().any(|id| &file_of[id] == f));
                    injected += e.len();
                }
                let (mut with_any, mut injected_r) = (0, 0);
                for (q, r) in &res_real {
                    let e = pick(q, r);
                    with_any += usize::from(!e.is_empty());
                    injected_r += e.len();
                }
                println!(
                    "≥{threshold:.2}, ≥{relative:.1}×mejor, máx {max}            {:>11.0}% {:>10.1}   {:>15.0}% {:>11.1}",
                    100.0 * ok as f64 / res_pos.len() as f64,
                    injected as f64 / res_pos.len() as f64,
                    100.0 * with_any as f64 / res_real.len() as f64,
                    injected_r as f64 / res_real.len() as f64
                );
            }
        }
    }
    // Full criterion: useful terms (≥ 3 letters and not only digits), at least `min_terms` in the
    // query, at least `min_rare` in common that are in ≤ 20 % of the notes, and IDF coverage ≥ u.
    let rare_max: f64 = std::env::var("RARO").ok().and_then(|x| x.parse().ok()).unwrap_or(0.2);

    let chatter = std::env::var("CHARLA").is_ok();
    const CHATTER: &[&str] = &[
        "pues", "dale", "vale", "okey", "ahora", "bueno", "hay", "pon", "mira", "eso", "esto", "esta", "este", "todo",
        "toda", "todos", "algo", "hacer", "hace", "haz", "tengo", "tiene", "puedo", "puede", "quiero", "mejor", "bien",
        "mal", "asi", "aqui", "solo", "tambien", "cosas", "cosa", "otro", "otra", "otros", "otras", "ver", "sea", "ser",
        "estar", "porque", "cuando", "donde", "cual", "igual", "menos", "mas", "nada", "sin", "entonces", "creo", "hecho",
        "dime", "dame", "sigue", "seguimos", "listo", "perfecto", "gracias", "favor", "vamos", "ves", "voy", "van",
        "ido", "ese", "esa", "esos", "esas", "aun", "ya", "repite", "repito", "prueba", "mismo", "misma", "cada", "pero",
        "the", "this", "that", "with", "please", "now", "can", "you", "it",
    ];
    let useful = |q: &str| -> Vec<String> {
        search::terms(q)
            .iter()
            .map(|t| strip_accents(t))
            .filter(|t| t.chars().count() >= 3 && !t.chars().all(|c| c.is_ascii_digit()))
            .filter(|t| !chatter || !CHATTER.contains(&t.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let df = |t: &str| docs.iter().filter(|d| has(t, d)).count() as f64;
    let full = |q: &str, r: &[SearchResult], u: f64, min_terms: usize, min_rare: usize| -> Vec<(f64, String)> {
        let ts = useful(q);
        if ts.len() < min_terms {
            return vec![];
        }
        let total: f64 = ts.iter().map(|t| idf(t)).sum();
        if total == 0.0 {
            return vec![];
        }
        let mut v: Vec<(f64, String)> = r
            .iter()
            .filter_map(|x| {
                let d = toks(&format!("{} {}", x.memory.title, x.memory.content));
                let common: Vec<&String> = ts.iter().filter(|t| has(t, &d)).collect();
                let rare = common.iter().filter(|t| df(t) <= rare_max * n_docs).count();
                let w = common.iter().map(|t| idf(t)).sum::<f64>() / total;
                (rare >= min_rare && w >= u).then(|| (w, x.memory.id.clone()))
            })
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        let best = v.first().map(|x| x.0).unwrap_or(0.0);
        v.into_iter().filter(|(w, _)| *w >= 0.7 * best).take(3).collect()
    };
    println!("
{:<40} {:>12} {:>10}   {:>16} {:>11}", "criterio completo", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for (u, mt, mr) in [(0.35, 1, 1), (0.35, 3, 1), (0.35, 3, 2), (0.30, 3, 2), (0.25, 3, 2), (0.40, 3, 2), (0.35, 4, 2), (0.35, 3, 3)] {
        let (mut ok, mut injected) = (0, 0);
        for ((f, r), (_, q)) in res_pos.iter().zip(&positives) {
            let e = full(q, r, u, mt, mr);
            ok += usize::from(e.iter().any(|(_, id)| &file_of[id] == f));
            injected += e.len();
        }
        let (mut with_any, mut injected_r) = (0, 0);
        for (q, r) in &res_real {
            let e = full(q, r, u, mt, mr);
            with_any += usize::from(!e.is_empty());
            injected_r += e.len();
        }
        println!(
            "IDF≥{u:.2}, ≥{mt} términos, ≥{mr} raros{:>16} {:>11.0}% {:>10.1}   {:>15.0}% {:>11.1}",
            "",
            100.0 * ok as f64 / res_pos.len() as f64,
            injected as f64 / res_pos.len() as f64,
            100.0 * with_any as f64 / res_real.len() as f64,
            injected_r as f64 / res_real.len() as f64
        );
    }
    if let Ok(c) = std::env::var("COMPLETO") {
        let x: Vec<f64> = c.split(',').map(|v| v.parse().unwrap()).collect();
        println!("
muestra con el criterio completo {c}:");
        let mut n = 0;
        for (q, r) in &res_real {
            let e = full(q, r, x[0], x[1] as usize, x[2] as usize);
            if e.is_empty() {
                continue;
            }
            n += 1;
            if n > 40 {
                break;
            }
            let short: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
            println!("· «{short}»");
            for (w, id) in e {
                println!("      {:.2} {}", w, file_of[&id]);
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
            let mut v: Vec<(f64, &SearchResult)> = r.iter().map(|x| (wc(q, x), x)).filter(|(w, _)| *w >= u).collect();
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            let best = v.first().map(|x| x.0).unwrap_or(0.0);
            let v: Vec<_> = v.into_iter().filter(|(w, _)| *w >= 0.7 * best).take(3).collect();
            if v.is_empty() {
                continue;
            }
            n += 1;
            if n > 30 {
                break;
            }
            let short: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
            println!("· «{short}»");
            for (w, x) in v {
                println!("      {:.2} {}", w, file_of[&x.memory.id]);
            }
        }
        return Ok(());
    }

    println!("
{:<26} {:>12} {:>10}   {:>18} {:>12}", "criterio", "positivos ok", "inyectadas", "reales con algo", "inyectadas");
    for min_words in [1usize, 2, 3, 4] {
        for sim in [0.30f32, 0.40, 2.0] {
            let name = format!("≥{min_words} palabras, vector≥{}", if sim > 1.0 { "no".into() } else { format!("{sim:.2}") });
            let (mut ok, mut injected) = (0, 0);
            for (f, r) in &res_pos {
                let e = choose(r, min_words, sim, 3);
                ok += usize::from(e.iter().any(|x| &file_of[&x.memory.id] == f));
                injected += e.len();
            }
            let (mut with_any, mut injected_r) = (0, 0);
            for (_, r) in &res_real {
                let e = choose(r, min_words, sim, 3);
                with_any += usize::from(!e.is_empty());
                injected_r += e.len();
            }
            println!(
                "{name:<26} {:>11.0}% {:>10.1}   {:>17.0}% {:>12.1}",
                100.0 * ok as f64 / res_pos.len() as f64,
                injected as f64 / res_pos.len() as f64,
                100.0 * with_any as f64 / res_real.len() as f64,
                injected_r as f64 / res_real.len() as f64
            );
        }
    }
    let (min_words, sim) = (
        std::env::var("MINIMO").ok().and_then(|x| x.parse().ok()).unwrap_or(3),
        std::env::var("SIM").ok().and_then(|x| x.parse().ok()).unwrap_or(2.0f32),
    );
    println!("\nmuestra con ≥{min_words} palabras (vector ≥ {sim}): mensaje → lo que se inyectaría");
    for (q, r) in res_real.iter().filter(|(_, r)| !choose(r, min_words, sim, 3).is_empty()).take(25) {
        let short: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
        println!("· «{short}»");
        for x in choose(r, min_words, sim, 3) {
            println!("      {} ({} pal.)", file_of[&x.memory.id], x.matched_words);
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

fn strip_accents(s: &str) -> String {
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

/// The real criterion (`recall::relevant`, with its constants) on the same data, fast.
fn production(notes_dir: &Path, transcripts_dir: &Path) -> Result<()> {
    let notes = notes(notes_dir)?;
    let tmp = std::env::temp_dir().join(format!("omoi-fase4p-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::open(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    // With a model, also the recall's by-meaning channel (as in the hook).
    let model = omoikane::models::open_current()?;
    for (f, t, c) in &notes {
        let n = NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: t.clone(), content: c.clone(), ..Default::default() };
        file_of.insert(memory::save(&mut db, n, model.as_ref())?.id, f.clone());
    }
    let all_for = |db: &mut rusqlite::Connection, q: &str| -> Result<Vec<(omoikane::memory::Memory, f64)>> {
        let mut r = omoikane::recall::relevant(db, "/p", q)?;
        if let Some(m) = &model {
            for x in omoikane::recall::by_meaning(db, "/p", q, m)? {
                if !r.iter().any(|(y, _)| y.id == x.id) && r.len() < omoikane::recall::MAX_PER_PROMPT {
                    r.push((x, 0.0));
                }
            }
        }
        Ok(r)
    };
    let index = std::fs::read_to_string(notes_dir.join("MEMORY.md"))?;
    let positives: Vec<(String, String)> = index
        .lines()
        .filter_map(|l| {
            let (i, j, k) = (l.find("](")?, l.find(".md)")?, l.find(" — ")?);
            Some((format!("{}.md", &l[i + 2..j]), l[k + " — ".len()..].trim().trim_end_matches('…').to_string()))
        })
        .filter(|(f, q)| file_of.values().any(|x| x == f) && q.len() > 10)
        .collect();
    let mut real = messages(transcripts_dir)?;
    if real.len() > 300 {
        let step = real.len() as f64 / 300.0;
        real = (0..300).map(|i| real[(i as f64 * step) as usize].clone()).collect();
    }
    let t = std::time::Instant::now();
    let (mut ok, mut injected) = (0, 0);
    for (f, q) in &positives {
        let r = all_for(&mut db, q)?;
        ok += usize::from(r.iter().any(|(m, _)| &file_of[&m.id] == f));
        injected += r.len();
    }
    let (mut with_any, mut injected_r) = (0, 0);
    let mut sample = Vec::new();
    for q in &real {
        let r = all_for(&mut db, q)?;
        with_any += usize::from(!r.is_empty());
        injected_r += r.len();
        if !r.is_empty() {
            sample.push((q.clone(), r.iter().map(|(m, c)| format!("{c:.2} {}", file_of[&m.id])).collect::<Vec<_>>()));
        }
    }
    let n = (positives.len() + real.len()) as f64;
    println!(
        "positivos: {:.0}% con su nota ({:.1} inyectadas de media) · reales: {:.0}% con algo ({:.2} de media) · {:.1} ms por mensaje",
        100.0 * ok as f64 / positives.len() as f64,
        injected as f64 / positives.len() as f64,
        100.0 * with_any as f64 / real.len() as f64,
        injected_r as f64 / real.len() as f64,
        t.elapsed().as_secs_f64() * 1000.0 / n
    );
    for (q, r) in sample.iter().take(60) {
        let short: String = q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(95).collect();
        println!("· «{short}»  →  {}", r.join(" | "));
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}
