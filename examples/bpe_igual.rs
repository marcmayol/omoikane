//! El tokenizer compilado (`bpe`) frente a `tokenizers`: mismos ids en todos los textos, y lo que
//! tarda cada uno en cargar y en tokenizar.
//!
//! `cargo run --release --example bpe_igual -- <carpeta del modelo compacto> <textos.json>`
//! (`textos.json`: un objeto con listas de textos; lo genera `motor/py/textos.py` de la revisión).

use omoikane::bpe::{Bpe, FICHERO, compilar};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::Path::new(&a[0]);
    let json = dir.join("tokenizer.json");
    let grupos: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&std::fs::read(&a[1])?)?;

    let t = Instant::now();
    let mut hf = tokenizers::Tokenizer::from_file(&json).map_err(|e| anyhow::anyhow!("{e}"))?;
    hf.with_truncation(Some(tokenizers::TruncationParams { max_length: 2048, ..Default::default() })).map_err(|e| anyhow::anyhow!("{e}"))?;
    let carga_hf = t.elapsed();
    let destino = dir.join(FICHERO);
    let t = Instant::now();
    compilar(&json, &destino)?;
    let compilar_t = t.elapsed();
    let mut abrir = Vec::new();
    let mut bpe = None;
    for _ in 0..5 {
        let t = Instant::now();
        bpe = Some(Bpe::abrir(&json, &destino)?);
        abrir.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let bpe = bpe.unwrap();
    println!("cargar tokenizer.json: {:.0} ms; compilar: {:.0} ms; abrir el compilado: {:?} ms; {} bytes", carga_hf.as_secs_f64() * 1000.0, compilar_t.as_secs_f64() * 1000.0, abrir, std::fs::metadata(&destino)?.len());

    let (mut total, mut distintos, mut toks) = (0, 0, 0usize);
    let (mut ms_hf, mut ms_bpe) = (0.0, 0.0);
    for (g, v) in &grupos {
        let (mut n, mut mal) = (0, 0);
        for t in v.as_array().unwrap() {
            let t = t.as_str().unwrap();
            for prompt in ["task: search result | query: ", "title: none | text: ", ""] {
                let s = format!("{prompt}{t}");
                let i0 = Instant::now();
                let a = hf.encode(s.as_str(), true).map_err(|e| anyhow::anyhow!("{e}"))?.get_ids().to_vec();
                ms_hf += i0.elapsed().as_secs_f64() * 1000.0;
                let i0 = Instant::now();
                let b = bpe.codificar(&s, 2048);
                ms_bpe += i0.elapsed().as_secs_f64() * 1000.0;
                n += 1;
                toks += a.len();
                if a != b {
                    mal += 1;
                    if mal <= 3 {
                        let p = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                        println!("  DISTINTO en {g}: {:?}… hf {:?} bpe {:?} (desde {p}: {:?} / {:?})", s.chars().take(60).collect::<String>(), a.len(), b.len(), &a[p..(p + 5).min(a.len())], &b[p..(p + 5).min(b.len())]);
                    }
                }
            }
        }
        println!("{g}: {n} textos, {mal} distintos");
        total += n;
        distintos += mal;
    }
    println!("TOTAL {total} textos ({toks} tokens), {distintos} distintos; tokenizar: tokenizers {ms_hf:.0} ms, compilado {ms_bpe:.0} ms");
    Ok(())
}
