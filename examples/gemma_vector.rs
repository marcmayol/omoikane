//! El vector de EmbeddingGemma de un texto, en JSON, para compararlo con sentence-transformers.
//!
//! `cargo run --release --example gemma_vector -- <carpeta del modelo> <documento|pregunta> <fichero>`

use omoikane::gemma::{Gemma, Tipo};

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let g = Gemma::cargar(std::path::Path::new(&a[0]))?;
    let tipo = if a[1] == "pregunta" { Tipo::Pregunta } else { Tipo::Documento };
    let texto = std::fs::read_to_string(&a[2])?;
    println!("{}", serde_json::to_string(&g.vector(&texto, tipo)?)?);
    Ok(())
}
