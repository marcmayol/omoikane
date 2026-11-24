//! Converts EmbeddingGemma to the compact format and compares its vectors with the original's.
//!
//! `cargo run --release --example gemma_compacto -- <sentence-transformers folder> <destination>`

use omoikane::gemma::{Gemma, Tipo, compactar};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let (source, dest) = (&a[0], &a[1]);
    let t = Instant::now();
    compactar(source, dest)?;
    println!("compactado en {:.1} s", t.elapsed().as_secs_f64());
    let original = Gemma::cargar(source)?;
    let t = Instant::now();
    let compact = Gemma::cargar(dest)?;
    println!("carga del compacto: {:.2} s", t.elapsed().as_secs_f64());
    let texts = [
        ("¿por qué falla el login en Windows?", Tipo::Pregunta),
        ("How does the cat clone guess which language to colorize when reading from a pipe?", Tipo::Pregunta),
        ("El token de la API caduca a los 15 minutos y hay que renovarlo con el refresh token.", Tipo::Documento),
        ("bat detects the syntax from the file name; with stdin, use --language or --file-name.", Tipo::Documento),
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
        compact.vector(texts[1].0, Tipo::Pregunta)?;
    }
    println!("peor coseno {worst:.5}; consulta media {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    Ok(())
}
