//! Fase 0.5 del PLAN.md: cargar el modelo en milisegundos sin cambiar ni un vector.
//!
//!   cargo run --release --example fase05 -- compilar        int8 → formato mapeable
//!   cargo run --release --example fase05 -- abrir           abrir + primer vector (medir en frío)
//!   cargo run --release --example fase05 -- equivalencia D… ids y vectores frente a tokenizers y model2vec-rs

use anyhow::{Result, anyhow};
use omoikane::embeber::{Modelo, compilar};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn modelos() -> PathBuf {
    dirs::home_dir().unwrap().join(".omoikane/models")
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let int8 = modelos().join("potion-multilingual-128M-int8");
    let rapido = modelos().join("potion-multilingual-128M-omoi");
    match args.first().map(String::as_str) {
        Some("compilar") => {
            let t = Instant::now();
            compilar(&int8, &rapido)?;
            println!("compilado en {:.0} ms → {}", ms(t), rapido.display());
            for f in std::fs::read_dir(&rapido)? {
                let f = f?;
                println!("  {:<20} {:>8.1} MB", f.file_name().to_string_lossy(), f.metadata()?.len() as f64 / 1e6);
            }
            Ok(())
        }
        Some("abrir") => {
            let dir = args.get(1).map_or(rapido, PathBuf::from);
            let t = Instant::now();
            let m = Modelo::abrir(&dir)?;
            let abrir = ms(t);
            let t = Instant::now();
            let v = m.vector("el middleware de autenticación no se ejecuta en /health")?;
            let primero = ms(t);
            // Diez frases distintas más: con la caché fría, cada una puede tocar páginas nuevas del trie.
            let t = Instant::now();
            for f in DIFICILES.iter().take(10) {
                std::hint::black_box(m.vector(f)?);
            }
            println!("abrir {abrir:.1} ms · primer vector {primero:.2} ms · 10 más {:.2} ms · dim {}", ms(t), v.len());
            Ok(())
        }
        Some("residente") => {
            // Abre, codifica un corpus y se queda vivo para medir su memoria desde fuera.
            let m = Modelo::abrir(&rapido)?;
            let frases = corpus(&args[2..]);
            let t = Instant::now();
            for f in &frases {
                std::hint::black_box(m.vector(f)?);
            }
            println!("{} frases en {:.0} ms; esperando", frases.len(), ms(t));
            std::thread::sleep(std::time::Duration::from_secs(args.get(1).map_or(Ok(20), |s| s.parse())?));
            Ok(())
        }
        Some("residente-m2v") => {
            // Lo mismo con model2vec-rs, para comparar.
            let m = model2vec_rs::model::StaticModel::from_pretrained(&int8, None, None, None)?;
            let frases = corpus(&args[2..]);
            for lote in frases.chunks(512) {
                std::hint::black_box(m.encode_with_args(lote, None, 512));
            }
            println!("{} frases; esperando", frases.len());
            std::thread::sleep(std::time::Duration::from_secs(args.get(1).map_or(Ok(20), |s| s.parse())?));
            Ok(())
        }
        Some("equivalencia") => equivalencia(&int8, &rapido, &args[1..]),
        _ => Err(anyhow!("uso: fase05 compilar | abrir [DIR] | residente[-m2v] SEG DIR… | equivalencia DIR…")),
    }
}

/// Frases de varios alfabetos y casos raros que no salen en textos normales.
const DIFICILES: &[&str] = &[
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

/// Líneas de los ficheros de texto bajo las carpetas dadas.
fn corpus(dirs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = DIFICILES.iter().map(|s| s.to_string()).collect();
    let mut pila: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
    while let Some(p) = pila.pop() {
        let Ok(es) = std::fs::read_dir(&p) else { continue };
        for e in es.flatten() {
            let p = e.path();
            let nombre = p.file_name().unwrap_or_default().to_string_lossy();
            if p.is_dir() {
                if !nombre.starts_with('.') && nombre != "target" && nombre != "node_modules" {
                    pila.push(p);
                }
            } else if matches!(p.extension().and_then(|e| e.to_str()), Some("md" | "rs" | "go" | "txt" | "py" | "json")) {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    v.extend(t.lines().map(str::trim).filter(|l| !l.is_empty() && l.len() < 2000).map(String::from));
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

fn equivalencia(int8: &Path, rapido: &Path, dirs: &[String]) -> Result<()> {
    let frases = corpus(dirs);
    println!("{} frases", frases.len());

    let referencia = tokenizers::Tokenizer::from_file(int8.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
    let original = model2vec_rs::model::StaticModel::from_pretrained(int8, None, None, None)?;
    let nuevo = Modelo::abrir(rapido)?;

    let mut ids_distintos = 0;
    let mut peor = 1.0f32;
    let mut peor_frase = String::new();
    let mut tokens = 0usize;
    for f in &frases {
        let a = referencia.encode(f.as_str(), false).map_err(|e| anyhow!("{e}"))?.get_ids().to_vec();
        let b = nuevo.ids(f)?;
        tokens += a.len();
        if a != b {
            ids_distintos += 1;
            if ids_distintos <= 5 {
                println!("ids distintos: {f:?}\n  tokenizers {a:?}\n  omoi       {b:?}");
            }
        }
    }
    // Vectores por lotes, sin truncar, para comparar lo mismo.
    for lote in frases.chunks(512) {
        let a = original.encode_with_args(lote, None, 512);
        for (f, va) in lote.iter().zip(a) {
            let vb = nuevo.vector(f)?;
            let c: f32 = va.iter().zip(&vb).map(|(x, y)| x * y).sum();
            // Una frase sin tokens útiles da el vector cero en los dos: cuenta como igual.
            let c = if va.iter().all(|x| *x == 0.0) && vb.iter().all(|x| *x == 0.0) { 1.0 } else { c };
            if c < peor {
                peor = c;
                peor_frase = f.clone();
            }
        }
    }
    println!("tokens comparados      {tokens}");
    println!("frases con ids distintos  {ids_distintos} de {}", frases.len());
    println!("coseno mínimo con model2vec-rs  {peor:.6}  ({peor_frase:.80?})");

    let muestra: Vec<&String> = frases.iter().take(2000).collect();
    let t = Instant::now();
    for f in &muestra {
        std::hint::black_box(nuevo.vector(f)?);
    }
    println!("vector medio            {:.3} ms", ms(t) / muestra.len() as f64);
    anyhow::ensure!(ids_distintos == 0 && peor >= 0.9999, "NO equivalente");
    println!("EQUIVALENTE");
    Ok(())
}
