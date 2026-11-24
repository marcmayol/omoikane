//! The EmbeddingGemma engine: the compiled tokenizer gives the same ids as `tokenizers`, and the
//! fast engine (with AVX2 and through the generic path, the one for CPUs without AVX2 and ARM) the same vector as
//! candle, also in batches.
//!
//! Needs EmbeddingGemma in `OMOIKANE_SEMANTIC_MODEL` (the sentence-transformers folder or the
//! compact-format one); without it, it is skipped.

mod comun;

use omoikane::gemma::{Gemma, TextKind};
use std::path::PathBuf;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let p: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    p / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

/// The compact-format folder (converted into a temporary one if what is there is sentence-transformers).
fn compact(dir: &std::path::Path) -> Option<PathBuf> {
    let model = PathBuf::from(std::env::var_os("OMOIKANE_SEMANTIC_MODEL").filter(|m| !m.is_empty())?);
    std::fs::create_dir_all(dir).unwrap();
    let dest = dir.join("compacto");
    if omoikane::gemma::is_compact(&model) {
        std::fs::create_dir_all(&dest).unwrap();
        for f in omoikane::gemma::COMPACT_FILES {
            std::fs::copy(model.join(f), dest.join(f)).unwrap();
        }
    } else {
        omoikane::gemma::compact(&model, &dest).unwrap();
    }
    Some(dest)
}

const TEXTS: [&str; 12] = [
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
fn engine_and_tokenizer() {
    let dir = comun::Temporal::nueva("motor");
    let Some(m) = compact(&dir) else {
        eprintln!("motor: falta OMOIKANE_SEMANTIC_MODEL (EmbeddingGemma); se salta");
        return;
    };
    let long = "La memoria de un agente tiene que crecer sin volverse lenta. ".repeat(80); // ~900 tokens
    let huge = "x".repeat(9000); // over 2,048 tokens: gets truncated
    let mut texts: Vec<&str> = TEXTS.to_vec();
    texts.push(&long);
    texts.push(&huge);

    // Compiled tokenizer = tokenizers (with the prompts, and truncating to 2,048).
    let json = m.join("tokenizer.json");
    let mut hf = tokenizers::Tokenizer::from_file(&json).unwrap();
    hf.with_truncation(Some(tokenizers::TruncationParams { max_length: omoikane::gemma::MAX_TOKENS, ..Default::default() })).unwrap();
    omoikane::bpe::compile(&json, &m.join(omoikane::bpe::FILE)).unwrap();
    let bpe = omoikane::bpe::Bpe::open(&json, &m.join(omoikane::bpe::FILE)).unwrap();
    for t in &texts {
        for p in ["task: search result | query: ", "title: none | text: ", ""] {
            let s = format!("{p}{t}");
            assert_eq!(bpe.encode(&s, omoikane::gemma::MAX_TOKENS), hf.encode(s.as_str(), true).unwrap().get_ids(), "{s:?}");
        }
    }

    // The three engines, the same vector.
    let candle = Gemma::load_candle(&m).unwrap();
    let fast = Gemma::load_fast(&m, 4, omoikane::engine::simd_available()).unwrap();
    let generic = Gemma::load_fast(&m, 4, false).unwrap();
    assert!(fast.compiled_tokenizer() && fast.is_fast() && !candle.is_fast());
    let kinds = [TextKind::Query, TextKind::Document];
    let mut singles = Vec::new();
    for (i, t) in texts.iter().enumerate() {
        let kind = kinds[i % 2];
        let (c, r, g) = (candle.vector(t, kind).unwrap(), fast.vector(t, kind).unwrap(), generic.vector(t, kind).unwrap());
        assert_eq!(r.len(), omoikane::gemma::DIM);
        assert!(cosine(&c, &r) > 0.9999, "rápido {t:?}: {}", cosine(&c, &r));
        assert!(cosine(&c, &g) > 0.9999, "genérico {t:?}: {}", cosine(&c, &g));
        singles.push(r);
    }
    // In batches, the same as one at a time (bit for bit on the same path).
    let docs: Vec<&str> = texts.iter().step_by(2).copied().collect();
    let batch = fast.vectors(&docs, TextKind::Query).unwrap();
    for (l, s) in batch.iter().zip(singles.iter().step_by(2)) {
        assert_eq!(l, s);
    }
    let generic_batch = generic.vectors(&docs, TextKind::Query).unwrap();
    for (l, s) in generic_batch.iter().zip(singles.iter().step_by(2)) {
        assert!(cosine(l, s) > 0.9999);
    }
}
