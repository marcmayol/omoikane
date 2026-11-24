//! The EmbeddingGemma vector of a text, as JSON, to compare it with sentence-transformers.
//!
//! `cargo run --release --example gemma_vector -- <model folder> <documento|pregunta> <file>`

use omoikane::gemma::{Gemma, TextKind};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let g = Gemma::load(std::path::Path::new(&args[0]))?;
    let kind = if args[1] == "pregunta" { TextKind::Query } else { TextKind::Document };
    let text = std::fs::read_to_string(&args[2])?;
    println!("{}", serde_json::to_string(&g.vector(&text, kind)?)?);
    Ok(())
}
