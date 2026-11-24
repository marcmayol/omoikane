//! Converts EmbeddingGemma to the compact format and compares its vectors with the original's.
//!
//! `cargo run --release --example gemma_compacto -- <sentence-transformers folder> <destination>`

use omoikane::gemma::{Gemma, TextKind, compact};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let (source, dest) = (&a[0], &a[1]);
    let t = Instant::now();
    compact(source, dest)?;
    println!("compactado en {:.1} s", t.elapsed().as_secs_f64());
    let original = Gemma::load(source)?;
    let t = Instant::now();
    let compact = Gemma::load(dest)?;
    println!("carga del compacto: {:.2} s", t.elapsed().as_secs_f64());
    let texts = [
        ("¿por qué falla el login en Windows?", TextKind::Query),
        ("How does the cat clone guess which language to colorize when reading from a pipe?", TextKind::Query),
        ("El token de la API caduca a los 15 minutos y hay que renovarlo con el refresh token.", TextKind::Document),
        ("bat detects the syntax from the file name; with stdin, use --language or --file-name.", TextKind::Document),
    ];
    let mut worst = 1f32;
    for (text, kind) in texts {
        let x = original.vector(text, kind)?;
        let y = compact.vector(text, kind)?;
        let c: f32 = x.iter().zip(&y).map(|(a, b)| a * b).sum();
        worst = worst.min(c);
        println!("coseno {c:.5}  {text}");
    }
    let t = Instant::now();
    for _ in 0..20 {
        compact.vector(texts[1].0, TextKind::Query)?;
    }
    println!("peor coseno {worst:.5}; consulta media {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    Ok(())
}
