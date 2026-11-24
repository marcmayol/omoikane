//! Phase 0.5: load the model in milliseconds without changing a single vector.
//!
//!   cargo run --release --example phase05 -- compilar        int8 → mappable format
//!   cargo run --release --example phase05 -- abrir           open + first vector (measure cold)
//!   cargo run --release --example phase05 -- equivalencia D… ids and vectors against tokenizers and model2vec-rs

use anyhow::{Result, anyhow};
use omoikane::embed::{Model, compile};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn models_dir() -> PathBuf {
    std::env::home_dir().unwrap().join(".omoikane/models")
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let int8 = models_dir().join("potion-multilingual-128M-int8");
    let fast = models_dir().join("potion-multilingual-128M-omoi");
    match args.first().map(String::as_str) {
        Some("compilar") => {
            let t = Instant::now();
            compile(&int8, &fast)?;
            println!("compilado en {:.0} ms → {}", ms(t), fast.display());
            for f in std::fs::read_dir(&fast)? {
                let f = f?;
                println!("  {:<20} {:>8.1} MB", f.file_name().to_string_lossy(), f.metadata()?.len() as f64 / 1e6);
            }
            Ok(())
        }
        Some("abrir") => {
            let dir = args.get(1).map_or(fast, PathBuf::from);
            let t = Instant::now();
            let m = Model::open(&dir)?;
            let open_ms = ms(t);
            let t = Instant::now();
            let v = m.vector("el middleware de autenticación no se ejecuta en /health")?;
            let first_ms = ms(t);
            // Ten more different sentences: with a cold cache, each one may touch new pages of the trie.
            let t = Instant::now();
            for f in HARD.iter().take(10) {
                std::hint::black_box(m.vector(f)?);
            }
            println!("abrir {open_ms:.1} ms · primer vector {first_ms:.2} ms · 10 más {:.2} ms · dim {}", ms(t), v.len());
            Ok(())
        }
        Some("residente") => {
            // Opens, encodes a corpus and stays alive so its memory can be measured from outside.
            let m = Model::open(&fast)?;
            let sentences = corpus(&args[2..]);
            let t = Instant::now();
            for f in &sentences {
                std::hint::black_box(m.vector(f)?);
            }
            println!("{} frases en {:.0} ms; esperando", sentences.len(), ms(t));
            std::thread::sleep(std::time::Duration::from_secs(args.get(1).map_or(Ok(20), |s| s.parse())?));
            Ok(())
        }
        Some("residente-m2v") => {
            // The same with model2vec-rs, to compare.
            let m = model2vec_rs::model::StaticModel::from_pretrained(&int8, None, None, None)?;
            let sentences = corpus(&args[2..]);
            for batch in sentences.chunks(512) {
                std::hint::black_box(m.encode_with_args(batch, None, 512));
            }
            println!("{} frases; esperando", sentences.len());
            std::thread::sleep(std::time::Duration::from_secs(args.get(1).map_or(Ok(20), |s| s.parse())?));
            Ok(())
        }
        Some("equivalencia") => equivalence(&int8, &fast, &args[1..]),
        _ => Err(anyhow!("uso: fase05 compilar | abrir [DIR] | residente[-m2v] SEG DIR… | equivalencia DIR…")),
    }
}

/// Sentences in several scripts and odd cases that don't show up in normal text.
const HARD: &[&str] = &[
    "Les élèves ont étudié l’histoire de la Révolution française.",
    "Größere Äpfel schmecken süßer als kleinere, sagt Jürgen.",
    "Москва — столица России, крупнейший город Европы.",
    "Η Αθήνα είναι η πρωτεύουσα της Ελλάδας.",
    "東京は日本の首都です。人口は約1400万人。",
    "서울은 대한민국의 수도입니다.",
    "القاهرة هي عاصمة مصر وأكبر مدنها.",
    "हिंदी भारत की एक प्रमुख भाषा है।",
    "ภาษาไทยไม่มีช่องว่างระหว่างคำ",
    "L'Ajuntament d'Artà recull el vidre dimecres.",
    "½ café, 2³ = 8, ﬁn, Ⅻ, ｆｕｌｌｗｉｄｔｈ, ℃",
    "emoji 🤖🧠 y combinados 👩‍💻 🇪🇸",
    "   espacios   múltiples\tcon tabulador\nY salto de línea   ",
    "",
    "a",
    "<private>secreto</private> https://example.com/a?b=c&d=e#f",
    "fn main() { println!(\"{:?}\", vec![1, 2, 3]); } // código",
    "SELECT * FROM memory WHERE kind = 'gotcha' AND pinned = 1;",
    "\u{200b}cero\u{200b}ancho\u{feff}",
    "ꙮ 𓀀 𝔘𝔫𝔦𝔠𝔬𝔡𝔢 ﷽",
];

/// Lines of the text files under the given folders.
fn corpus(dirs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = HARD.iter().map(|s| s.to_string()).collect();
    let mut stack: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if p.is_dir() {
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    stack.push(p);
                }
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("md" | "rs" | "go" | "txt" | "py" | "json"))
                && let Ok(t) = std::fs::read_to_string(&p)
            {
                v.extend(t.lines().map(str::trim).filter(|l| !l.is_empty() && l.len() < 2000).map(String::from));
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

fn equivalence(int8: &Path, fast: &Path, dirs: &[String]) -> Result<()> {
    let sentences = corpus(dirs);
    println!("{} frases", sentences.len());

    let reference = tokenizers::Tokenizer::from_file(int8.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
    let original = model2vec_rs::model::StaticModel::from_pretrained(int8, None, None, None)?;
    let new_model = Model::open(fast)?;

    let mut differing_ids = 0;
    let mut worst = 1.0f32;
    let mut worst_sentence = String::new();
    let mut tokens = 0usize;
    for f in &sentences {
        let a = reference.encode(f.as_str(), false).map_err(|e| anyhow!("{e}"))?.get_ids().to_vec();
        let b = new_model.ids(f)?;
        tokens += a.len();
        if a != b {
            differing_ids += 1;
            if differing_ids <= 5 {
                println!("ids distintos: {f:?}\n  tokenizers {a:?}\n  omoi       {b:?}");
            }
        }
    }
    // Vectors in batches, without truncating, to compare the same thing.
    for batch in sentences.chunks(512) {
        let a = original.encode_with_args(batch, None, 512);
        for (f, va) in batch.iter().zip(a) {
            let vb = new_model.vector(f)?;
            let c: f32 = va.iter().zip(&vb).map(|(x, y)| x * y).sum();
            // A sentence with no useful tokens gives the zero vector in both: counts as equal.
            let c = if va.iter().all(|x| *x == 0.0) && vb.iter().all(|x| *x == 0.0) { 1.0 } else { c };
            if c < worst {
                worst = c;
                worst_sentence = f.clone();
            }
        }
    }
    println!("tokens comparados      {tokens}");
    println!("frases con ids distintos  {differing_ids} de {}", sentences.len());
    println!("coseno mínimo con model2vec-rs  {worst:.6}  ({worst_sentence:.80?})");

    let sample: Vec<&String> = sentences.iter().take(2000).collect();
    let t = Instant::now();
    for f in &sample {
        std::hint::black_box(new_model.vector(f)?);
    }
    println!("vector medio            {:.3} ms", ms(t) / sample.len() as f64);
    anyhow::ensure!(differing_ids == 0 && worst >= 0.9999, "NO equivalente");
    println!("EQUIVALENTE");
    Ok(())
}
