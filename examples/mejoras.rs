//! Medir las mejoras de la 0.4 con notas reales (no se suben al repo) y preguntas con su respuesta.
//!
//!   cargo run --release --example mejoras -- busqueda NOTAS TSV...
//!
//! NOTAS: carpeta de notas de Claude Code (un .md por nota con `description:` en el frontmatter).
//! TSV: `tipo<TAB>fichero<TAB>pregunta` (fichero `-` si no tiene respuesta).

use anyhow::{Result, anyhow};
use omoikane::buscar::{Fusion, Opciones, buscar_con};
use omoikane::embeber::Modelo;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("busqueda") if args.len() >= 3 => busqueda(Path::new(&args[1]), &args[2..]),
        Some("fallos") if args.len() == 3 => fallos(Path::new(&args[1]), &args[2]),
        _ => Err(anyhow!("uso: mejoras busqueda NOTAS TSV...")),
    }
}

fn modelo() -> Result<Modelo> {
    omoikane::modelos::abrir_actual()?.ok_or_else(|| anyhow!("sin modelo: omoi model install"))
}

pub fn notas(dir: &Path) -> Result<Vec<(String, String, String)>> {
    let mut v = Vec::new();
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
        let titulo: String = titulo.trim().trim_matches('"').replace("\\\"", "\"").chars().take(120).collect();
        v.push((f, titulo, cuerpo.to_string()));
    }
    v.sort();
    Ok(v)
}

pub fn preguntas(tsv: &str) -> Result<Vec<(String, String, String)>> {
    Ok(std::fs::read_to_string(tsv)?
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("tipo\t"))
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.len() >= 3).then(|| (c[0].to_string(), c[1].to_string(), c[2].to_string()))
        })
        .collect())
}

fn busqueda(dir: &Path, tsvs: &[String]) -> Result<()> {
    let m = modelo()?;
    let tmp = std::env::temp_dir().join(format!("omoi-mejoras-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    for (f, t, c) in notas(dir)? {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t, content: c, ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f);
    }
    let o = Opciones { ambito: Ambito::Proyecto("/p".into()), kind: None, tags: vec![], limite: 5 };
    let mut configs: Vec<(String, Fusion)> = vec![("rescate (0.3.1)".into(), Fusion::Rescate)];
    for desde in [0.20f32, 0.30, 0.40] {
        for peso in [0.5f64, 1.0, 2.0, 4.0] {
            configs.push((format!("mezcla {peso} desde {desde}"), Fusion::Mezcla { peso, desde }));
        }
    }
    for tsv in tsvs {
        let qs = preguntas(tsv)?;
        println!("\n=== {tsv}: {} preguntas", qs.len());
        let tipos: Vec<String> = qs.iter().map(|q| q.0.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        print!("{:<26}", "");
        for t in &tipos {
            print!("{:>22}", t);
        }
        println!("{:>14}", "total top1");
        for (nombre, fusion) in &configs {
            let mut por_tipo: BTreeMap<&str, (usize, usize, f64, usize)> = BTreeMap::new(); // top1, top3, mrr, n
            for (tipo, f, q) in &qs {
                if f == "-" {
                    continue;
                }
                let r = buscar_con(&mut db, q, &o, Some(&m), omoikane::vectores::SIMILITUD_BUSQUEDA, *fusion)?;
                let pos = r.iter().position(|x| &fichero_de[&x.memoria.id] == f);
                let e = por_tipo.entry(tipo).or_default();
                e.0 += usize::from(pos == Some(0));
                e.1 += usize::from(pos.is_some_and(|p| p < 3));
                e.2 += pos.map_or(0.0, |p| 1.0 / (p as f64 + 1.0));
                e.3 += 1;
            }
            print!("{nombre:<26}");
            let (mut t1, mut n) = (0, 0);
            for t in &tipos {
                match por_tipo.get(t.as_str()) {
                    Some(e) => {
                        print!("{:>9.1}% {:>5.1}% {:.2}", 100.0 * e.0 as f64 / e.3 as f64, 100.0 * e.1 as f64 / e.3 as f64, e.2 / e.3 as f64);
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

/// Las que fallan (top-1) en un TSV, con lo que salió primero.
pub fn fallos(dir: &Path, tsv: &str) -> Result<()> {
    let m = modelo()?;
    let tmp = std::env::temp_dir().join(format!("omoi-fallos-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut db = omoikane::db::abrir(&tmp.join("o.db"))?;
    let mut fichero_de = HashMap::new();
    for (f, t, c) in notas(dir)? {
        let n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: t, content: c, ..Default::default() };
        fichero_de.insert(memoria::guardar(&mut db, n, Some(&m))?.id, f);
    }
    let o = Opciones { ambito: Ambito::Proyecto("/p".into()), kind: None, tags: vec![], limite: 5 };
    for (tipo, f, q) in preguntas(tsv)? {
        if f == "-" {
            continue;
        }
        let r = buscar_con(&mut db, &q, &o, Some(&m), omoikane::vectores::SIMILITUD_BUSQUEDA, Fusion::Rescate)?;
        let pos = r.iter().position(|x| fichero_de[&x.memoria.id] == f);
        if pos != Some(0) {
            let primero = r.first().map_or("(nada)".to_string(), |x| fichero_de[&x.memoria.id].clone());
            println!("[{tipo}] puesto {:?}  «{q}»\n    buena: {f}   primera: {primero}", pos.map(|p| p + 1));
        }
    }
    Ok(())
}
