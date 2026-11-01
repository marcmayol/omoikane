//! Convierte EmbeddingGemma al formato compacto y compara sus vectores con los del original.
//!
//! `cargo run --release --example gemma_compacto -- <carpeta sentence-transformers> <destino>`

use omoikane::gemma::{Gemma, Tipo, compactar};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let (origen, destino) = (&a[0], &a[1]);
    let t = Instant::now();
    compactar(origen, destino)?;
    println!("compactado en {:.1} s", t.elapsed().as_secs_f64());
    let original = Gemma::cargar(origen)?;
    let t = Instant::now();
    let compacto = Gemma::cargar(destino)?;
    println!("carga del compacto: {:.2} s", t.elapsed().as_secs_f64());
    let textos = [
        ("¿por qué falla el login en Windows?", Tipo::Pregunta),
        ("How does the cat clone guess which language to colorize when reading from a pipe?", Tipo::Pregunta),
        ("El token de la API caduca a los 15 minutos y hay que renovarlo con el refresh token.", Tipo::Documento),
        ("bat detects the syntax from the file name; with stdin, use --language or --file-name.", Tipo::Documento),
    ];
    let mut peor = 1f32;
    for (texto, tipo) in textos {
        let x = original.vector(texto, tipo)?;
        let y = compacto.vector(texto, tipo)?;
        let c: f32 = x.iter().zip(&y).map(|(a, b)| a * b).sum();
        peor = peor.min(c);
        println!("coseno {c:.5}  {texto}");
    }
    let t = Instant::now();
    for _ in 0..20 {
        compacto.vector(textos[1].0, Tipo::Pregunta)?;
    }
    println!("peor coseno {peor:.5}; consulta media {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
    Ok(())
}
