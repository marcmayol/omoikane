//! The model compiled by `embed` gives the same ids as `tokenizers` and the same vectors as
//! model2vec-rs. Otherwise, the vector index of a database saved with one would not work with the other.
//!
//! Needs the int8 model (about 150 MB). It looks for it in `OMOI_MODELO_INT8` or in
//! `~/.omoikane/models/potion-multilingual-128M-int8`; if it is not there, the test warns and checks nothing.
//! Download it from https://huggingface.co/natzx94/potion-multilingual-128M-int8.

mod common;

use omoikane::embed::{Model, compile};
use std::path::PathBuf;

/// Cases that don't fit in one corpus line.
const SPECIAL: &[&str] = &[
    "",
    "   espacios   múltiples\tcon tabulador\nY salto de línea   ",
    "\u{200b}cero\u{200b}ancho\u{feff}",
    "\r\nretorno de carro\r\n",
];

fn int8_model() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

#[test]
fn ids_and_vectors_same_as_reference() {
    let Some(int8) = int8_model() else {
        eprintln!("SALTADO: no está el modelo int8 (ver la cabecera de tests/equivalence.rs)");
        return;
    };
    let dest = common::TempDir::new("equivalencia");
    compile(&int8, &dest).expect("compilar");
    let compiled = Model::open(&dest).expect("abrir");

    let corpus = include_str!("corpus.txt");
    let sentences: Vec<String> = corpus
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(String::from)
        .chain(SPECIAL.iter().map(|s| s.to_string()))
        .collect();
    assert!(sentences.len() > 1800, "corpus demasiado corto: {}", sentences.len());

    let reference = tokenizers::Tokenizer::from_file(int8.join("tokenizer.json")).expect("tokenizer");
    let mut different = Vec::new();
    for s in &sentences {
        let a = reference.encode(s.as_str(), false).expect("encode").get_ids().to_vec();
        let b = compiled.ids(s).expect("ids");
        if a != b {
            different.push(s.clone());
        }
    }
    assert!(different.is_empty(), "{} frases con ids distintos, p. ej. {:?}", different.len(), &different[..different.len().min(3)]);

    let original = model2vec_rs::model::StaticModel::from_pretrained(&int8, None, None, None).expect("model2vec-rs");
    let a = original.encode_with_args(&sentences, None, 512);
    for (s, va) in sentences.iter().zip(a) {
        let vb = compiled.vector(s).expect("vector");
        let is_zero = |v: &[f32]| v.iter().all(|x| *x == 0.0);
        if is_zero(&va) && is_zero(&vb) {
            continue;
        }
        let c: f32 = va.iter().zip(&vb).map(|(x, y)| x * y).sum();
        assert!(c >= 0.9999, "coseno {c} en {s:?}");
    }
}
