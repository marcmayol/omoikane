//! Measure the 0.4 improvements with real notes (not uploaded to the repo) and questions with their answer.
//!
//!   cargo run --release --example mejoras -- busqueda NOTAS TSV...
//!
//! NOTAS: folder of Claude Code notes (one .md per note with `description:` in the front matter).
//! TSV: `tipo<TAB>fichero<TAB>pregunta` (file `-` if it has no answer).

use anyhow::{Result, anyhow};
use omoikane::buscar::{Fusion, Opciones, buscar_con};
use omoikane::embeber::Modelo;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("busqueda") if args.len() >= 3 => search(Path::new(&args[1]), &args[2..]),
        Some("fallos") if args.len() == 3 => failures(Path::new(&args[1]), &args[2]),
        _ => Err(anyhow!("uso: mejoras busqueda NOTAS TSV...")),
    }
}

fn model() -> Result<Modelo> {
    omoikane::modelos::abrir_actual()?.ok_or_else(|| anyhow!("sin modelo: omoi model install"))
}

pub fn notes(dir: &Path) -> Result<Vec<(String, String, String)>> {
    let mut v = Vec::new();
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
        let title: String = title.trim().trim_matches('"').replace("\\\"", "\"").chars().take(120).collect();
        v.push((f, title, body.to_string()));
    }
    v.sort();
    Ok(v)
}

pub fn questions(tsv: &str) -> Result<Vec<(String, String, String)>> {
    Ok(std::fs::read_to_string(tsv)?
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("tipo\t"))
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.len() >= 3).then(|| (c[0].to_string(), c[1].to_string(), c[2].to_string()))
        })
        .collect())
}

fn search(dir: &Path, tsvs: &[String]) -> Result<()> {
    let m = model()?;
    let tmp = std::env::temp_dir().join(format!("omoi-mejoras-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    for (f, t, c) in notes(dir)? {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t, content: c, ..Default::default() };
        file_of.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f);
    }
    let o = Opciones { ambito: Ambito::de(&db, "/p")?, kind: None, tags: vec![], limite: 5 };
    let mut configs: Vec<(String, Fusion)> = vec![("rescate (0.5.0)".into(), Fusion::Rescate)];
    for from in [0.30f32, 0.40, 0.50, 0.60] {
        for slots in [1usize, 2] {
            configs.push((format!("reserva {slots} desde {from}"), Fusion::Reserva { huecos: slots, desde: from }));
        }
    }
    for tsv in tsvs {
        let qs = questions(tsv)?;
        println!("\n=== {tsv}: {} preguntas", qs.len());
        let kinds: Vec<String> = qs.iter().map(|q| q.0.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        print!("{:<26}", "");
        for t in &kinds {
            print!("{:>22}", t);
        }
        println!("{:>14}", "total top1");
        for (name, fusion) in &configs {
            let mut per_kind: BTreeMap<&str, (usize, usize, f64, usize, usize)> = BTreeMap::new(); // top1, top3, mrr, n, top5
            for (kind, f, q) in &qs {
                if f == "-" {
                    continue;
                }
                let r = buscar_con(&mut db, q, &o, Some(&m), omoikane::vectores::SIMILITUD_BUSQUEDA, *fusion)?;
                let pos = r.iter().position(|x| &file_of[&x.memoria.id] == f);
                let e = per_kind.entry(kind).or_default();
                e.0 += usize::from(pos == Some(0));
                e.1 += usize::from(pos.is_some_and(|p| p < 3));
                e.2 += pos.map_or(0.0, |p| 1.0 / (p as f64 + 1.0));
                e.3 += 1;
                e.4 += usize::from(pos.is_some_and(|p| p < 5));
            }
            print!("{name:<26}");
            let (mut t1, mut n) = (0, 0);
            for t in &kinds {
                match per_kind.get(t.as_str()) {
                    Some(e) => {
                        print!(
                            "{:>7.1}% {:>5.1}% {:>5.1}%",
                            100.0 * e.0 as f64 / e.3 as f64,
                            100.0 * e.1 as f64 / e.3 as f64,
                            100.0 * e.4 as f64 / e.3 as f64
                        );
                        t1 += e.0;
                        n += e.3;
                    }
                    None => print!("{:>22}", "-"),
                }
            }
            println!("{:>13.1}%", 100.0 * t1 as f64 / n.max(1) as f64);
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(tmp);
    Ok(())
}

/// The ones that fail (top-1) in a TSV, with what came out first.
pub fn failures(dir: &Path, tsv: &str) -> Result<()> {
    let m = model()?;
    let tmp = std::env::temp_dir().join(format!("omoi-fallos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut file_of = HashMap::new();
    for (f, t, c) in notes(dir)? {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t, content: c, ..Default::default() };
        file_of.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f);
    }
    let o = Opciones { ambito: Ambito::de(&db, "/p")?, kind: None, tags: vec![], limite: 5 };
    for (kind, f, q) in questions(tsv)? {
        if f == "-" {
            continue;
        }
        let r = buscar_con(&mut db, &q, &o, Some(&m), omoikane::vectores::SIMILITUD_BUSQUEDA, Fusion::Rescate)?;
        let pos = r.iter().position(|x| file_of[&x.memoria.id] == f);
        if pos != Some(0) {
            let first = r.first().map_or("(nada)".to_string(), |x| file_of[&x.memoria.id].clone());
            println!("[{kind}] puesto {:?}  «{q}»\n    buena: {f}   primera: {first}", pos.map(|p| p + 1));
        }
    }
    Ok(())
}
