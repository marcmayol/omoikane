//! El motor de EmbeddingGemma: el tokenizer compilado da los mismos ids que `tokenizers`, y el
//! motor rápido (con AVX2 y por el camino genérico, el de las CPU sin AVX2 y ARM) el mismo vector que
//! candle, también por lotes.
//!
//! Necesita EmbeddingGemma en `OMOIKANE_SEMANTIC_MODEL` (la carpeta de sentence-transformers o la del
//! formato compacto); sin ella se salta.

mod comun;

use omoikane::gemma::{Gemma, Tipo};
use std::path::PathBuf;

fn coseno(a: &[f32], b: &[f32]) -> f32 {
    let p: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    p / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

/// La carpeta del formato compacto (convertida a un temporal si lo que hay es sentence-transformers).
fn compacto(dir: &std::path::Path) -> Option<PathBuf> {
    let modelo = PathBuf::from(std::env::var_os("OMOIKANE_SEMANTIC_MODEL").filter(|m| !m.is_empty())?);
    std::fs::create_dir_all(dir).unwrap();
    let destino = dir.join("compacto");
    if omoikane::gemma::es_compacto(&modelo) {
        std::fs::create_dir_all(&destino).unwrap();
        for f in omoikane::gemma::FICHEROS_COMPACTO {
            std::fs::copy(modelo.join(f), destino.join(f)).unwrap();
        }
    } else {
        omoikane::gemma::compactar(&modelo, &destino).unwrap();
    }
    Some(destino)
}

const TEXTOS: [&str; 12] = [
    "How do I undo a cargo yank?",
    "Two Gradle daemons at once corrupt the build cache.",
    "",
    "  dos  espacios   y\ttabuladores\t\t",
    "líneas\n\nen blanco\n\n\n\ny más\n",
    "<bos>dentro<eos> <unused12> <start_of_turn>user [multimodal] <table><tr>",
    "▁▁literal y emoji 🙂🚀, 漢字, العربية, café über ñandú",
    "\u{0}\u{1}\u{7f} <0x0A> control",
    "<image_soft_token> no está en la tabla",
    "https://example.com/a?b=c&d=e#f — C:\\Users\\x\\file.rs:12:5",
    "fn main() {\n    println!(\"hola\");\n}\n",
    "palabra ",
];

#[test]
fn motor_y_tokenizer() {
    let dir = comun::Temporal::nueva("motor");
    let Some(m) = compacto(&dir) else {
        eprintln!("motor: falta OMOIKANE_SEMANTIC_MODEL (EmbeddingGemma); se salta");
        return;
    };
    let largo = "La memoria de un agente tiene que crecer sin volverse lenta. ".repeat(80); // ~900 tokens
    let enorme = "x".repeat(9000); // pasa de 2.048 tokens: se recorta
    let mut textos: Vec<&str> = TEXTOS.to_vec();
    textos.push(&largo);
    textos.push(&enorme);

    // Tokenizer compilado = tokenizers (con los prompts, y recortando a 2.048).
    let json = m.join("tokenizer.json");
    let mut hf = tokenizers::Tokenizer::from_file(&json).unwrap();
    hf.with_truncation(Some(tokenizers::TruncationParams { max_length: omoikane::gemma::MAX_TOKENS, ..Default::default() })).unwrap();
    omoikane::bpe::compilar(&json, &m.join(omoikane::bpe::FICHERO)).unwrap();
    let bpe = omoikane::bpe::Bpe::abrir(&json, &m.join(omoikane::bpe::FICHERO)).unwrap();
    for t in &textos {
        for p in ["task: search result | query: ", "title: none | text: ", ""] {
            let s = format!("{p}{t}");
            assert_eq!(bpe.codificar(&s, omoikane::gemma::MAX_TOKENS), hf.encode(s.as_str(), true).unwrap().get_ids(), "{s:?}");
        }
    }

    // Los tres motores, el mismo vector.
    let candle = Gemma::cargar_candle(&m).unwrap();
    let rapido = Gemma::cargar_rapido(&m, 4, omoikane::rapido::simd_disponible()).unwrap();
    let generico = Gemma::cargar_rapido(&m, 4, false).unwrap();
    assert!(rapido.tokenizer_compilado() && rapido.es_rapido() && !candle.es_rapido());
    let tipos = [Tipo::Pregunta, Tipo::Documento];
    let mut solos = Vec::new();
    for (i, t) in textos.iter().enumerate() {
        let tipo = tipos[i % 2];
        let (c, r, g) = (candle.vector(t, tipo).unwrap(), rapido.vector(t, tipo).unwrap(), generico.vector(t, tipo).unwrap());
        assert_eq!(r.len(), omoikane::gemma::DIM);
        assert!(coseno(&c, &r) > 0.9999, "rápido {t:?}: {}", coseno(&c, &r));
        assert!(coseno(&c, &g) > 0.9999, "genérico {t:?}: {}", coseno(&c, &g));
        solos.push(r);
    }
    // Por lotes, lo mismo que de uno en uno (bit a bit con el mismo camino).
    let docs: Vec<&str> = textos.iter().step_by(2).copied().collect();
    let lote = rapido.vectores(&docs, Tipo::Pregunta).unwrap();
    for (l, s) in lote.iter().zip(solos.iter().step_by(2)) {
        assert_eq!(l, s);
    }
    let lote_generico = generico.vectores(&docs, Tipo::Pregunta).unwrap();
    for (l, s) in lote_generico.iter().zip(solos.iter().step_by(2)) {
        assert!(coseno(l, s) > 0.9999);
    }
}
