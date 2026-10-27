//! El modelo compilado de `embeber` da los mismos ids que `tokenizers` y los mismos vectores que
//! model2vec-rs. Si no, el índice vectorial de una base guardada con uno no serviría con el otro.
//!
//! Necesita el modelo int8 (unos 150 MB). Lo busca en `OMOI_MODELO_INT8` o en
//! `~/.omoikane/models/potion-multilingual-128M-int8`; si no está, el test avisa y no comprueba nada.
//! Se baja de https://huggingface.co/natzx94/potion-multilingual-128M-int8.

use omoikane::embeber::{Modelo, compilar};
use std::path::PathBuf;

/// Casos que no caben en una línea del corpus.
const ESPECIALES: &[&str] = &[
    "",
    "   espacios   múltiples\tcon tabulador\nY salto de línea   ",
    "\u{200b}cero\u{200b}ancho\u{feff}",
    "\r\nretorno de carro\r\n",
];

fn modelo_int8() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

#[test]
fn ids_y_vectores_iguales_que_la_referencia() {
    let Some(int8) = modelo_int8() else {
        eprintln!("SALTADO: no está el modelo int8 (ver la cabecera de tests/equivalencia.rs)");
        return;
    };
    let destino = std::env::temp_dir().join(format!("omoikane-test-equivalencia-{}", std::process::id()));
    compilar(&int8, &destino).expect("compilar");
    let nuevo = Modelo::abrir(&destino).expect("abrir");

    let corpus = include_str!("corpus.txt");
    let frases: Vec<String> = corpus
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(String::from)
        .chain(ESPECIALES.iter().map(|s| s.to_string()))
        .collect();
    assert!(frases.len() > 1800, "corpus demasiado corto: {}", frases.len());

    let referencia = tokenizers::Tokenizer::from_file(int8.join("tokenizer.json")).expect("tokenizer");
    let mut distintas = Vec::new();
    for f in &frases {
        let a = referencia.encode(f.as_str(), false).expect("encode").get_ids().to_vec();
        let b = nuevo.ids(f).expect("ids");
        if a != b {
            distintas.push(f.clone());
        }
    }
    assert!(distintas.is_empty(), "{} frases con ids distintos, p. ej. {:?}", distintas.len(), &distintas[..distintas.len().min(3)]);

    let original = model2vec_rs::model::StaticModel::from_pretrained(&int8, None, None, None).expect("model2vec-rs");
    let a = original.encode_with_args(&frases, None, 512);
    for (f, va) in frases.iter().zip(a) {
        let vb = nuevo.vector(f).expect("vector");
        let cero = |v: &[f32]| v.iter().all(|x| *x == 0.0);
        if cero(&va) && cero(&vb) {
            continue;
        }
        let c: f32 = va.iter().zip(&vb).map(|(x, y)| x * y).sum();
        assert!(c >= 0.9999, "coseno {c} en {f:?}");
    }
    let _ = std::fs::remove_dir_all(&destino);
}
