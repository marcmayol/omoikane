//! Phase 0: measure before deciding.
//!
//!   cargo run --release --example phase0 -- cuantizar   downloads the f32 model, writes our own int8 and compares
//!   cargo run --release --example phase0 -- cargar DIR  loads a model and encodes one sentence (to measure cold)
//!   cargo run --release --example phase0 -- buscar N    vec0 and FTS5 with N synthetic memories

use anyhow::{Context, Result, anyhow};
use model2vec_rs::model::StaticModel;
use safetensors::{Dtype, SafeTensors, tensor::TensorView};
use std::path::{Path, PathBuf};
use std::time::Instant;

const REPO: &str = "minishlab/potion-multilingual-128M";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("cuantizar") => quantize(),
        Some("cargar") => load(Path::new(args.get(1).context("falta DIR")?)),
        Some("buscar") => search(args.get(1).map_or(Ok(50_000), |n| n.parse())?),
        _ => Err(anyhow!("uso: fase0 cuantizar | cargar DIR | buscar N")),
    }
}

fn dir_int8() -> PathBuf {
    std::env::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-int8")
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

const SENTENCES: &[&str] = &[
    "fallo de login en la pantalla de acceso",
    "bug en auth: el token caduca antes de tiempo",
    "el middleware de autenticación no se ejecuta en /health",
    "usamos ULID en vez de UUID para que los ids se ordenen por fecha",
    "prefiero commits cortos y en inglés",
    "la impresora 3D necesita brim para las piezas finas",
    "receta de tortilla de patatas con cebolla",
    "login failure on the sign-in screen",
];

fn quantize() -> Result<()> {
    // Downloads (or reuses from the Hugging Face cache) the official f32 model.
    let t = Instant::now();
    let api = hf_hub_dir()?;
    println!("descarga/caché       {:>8.0} ms  {}", ms(t), api.display());

    let t = Instant::now();
    let f32m = StaticModel::from_pretrained(&api, None, None, None)?;
    println!("carga f32            {:>8.0} ms", ms(t));

    // Our own int8: one scale per row, folded into `weights` so the file carries it along.
    let bytes = std::fs::read(api.join("model.safetensors"))?;
    let st = SafeTensors::deserialize(&bytes)?;
    let emb = st.tensor("embeddings").or_else(|_| st.tensor("0")).context("sin embeddings")?;
    let [rows, dim]: [usize; 2] = emb.shape().try_into()?;
    anyhow::ensure!(emb.dtype() == Dtype::F32, "se esperaba f32, es {:?}", emb.dtype());
    let table: Vec<f32> = emb.data().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let mapping: Option<Vec<usize>> = st.tensor("mapping").ok().map(|t| match t.dtype() {
        Dtype::I64 => t.data().as_chunks::<8>().0.iter().map(|b| i64::from_le_bytes(*b) as usize).collect(),
        _ => t.data().as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) as usize).collect(),
    });
    let weights: Option<Vec<f32>> = st.tensor("weights").ok().map(|t| match t.dtype() {
        Dtype::F64 => t.data().as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b) as f32).collect(),
        _ => t.data().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
    });
    println!(
        "tensores             filas {rows}, mapping {}, weights {}",
        mapping.as_ref().map_or("no".into(), |m| m.len().to_string()),
        weights.as_ref().map_or("no".into(), |w| w.len().to_string())
    );

    let mut data = Vec::with_capacity(table.len());
    let mut scales = Vec::with_capacity(rows);
    for row in table.chunks_exact(dim) {
        let max = row.iter().fold(0.0_f32, |m, &v| m.max(v.abs()));
        let e = max / 127.0;
        data.extend(row.iter().map(|&v| if e > 0.0 { (v / e).round().clamp(-127.0, 127.0) as i8 as u8 } else { 0 }));
        scales.push(e);
    }
    let tokens = mapping.as_ref().map_or(rows, Vec::len).max(weights.as_ref().map_or(0, Vec::len));
    let new_weights: Vec<f32> = (0..tokens)
        .map(|tok| {
            let row = mapping.as_ref().map_or(tok, |m| m[tok]);
            weights.as_ref().and_then(|w| w.get(tok)).copied().unwrap_or(1.0) * scales[row]
        })
        .collect();
    let new_weights_b: Vec<u8> = new_weights.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mapping_b: Option<Vec<u8>> = mapping.as_ref().map(|m| m.iter().flat_map(|&v| (v as i64).to_le_bytes()).collect());

    let out_dir = dir_int8();
    std::fs::create_dir_all(&out_dir)?;
    let mut views = vec![
        ("embeddings".to_string(), TensorView::new(Dtype::I8, vec![rows, dim], &data)?),
        ("weights".to_string(), TensorView::new(Dtype::F32, vec![tokens], &new_weights_b)?),
    ];
    if let Some(m) = &mapping_b {
        views.push(("mapping".to_string(), TensorView::new(Dtype::I64, vec![tokens], m)?));
    }
    safetensors::serialize_to_file(views, &None, &out_dir.join("model.safetensors"))?;
    for f in ["tokenizer.json", "config.json"] {
        std::fs::copy(api.join(f), out_dir.join(f))?;
    }
    let mb = |p: &Path| std::fs::metadata(p).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
    println!(
        "int8 escrito         {:.1} MB modelo + {:.1} MB tokenizer  {}",
        mb(&out_dir.join("model.safetensors")),
        mb(&out_dir.join("tokenizer.json")),
        out_dir.display()
    );

    let t = Instant::now();
    let i8m = StaticModel::from_pretrained(&out_dir, None, None, None)?;
    println!("carga int8           {:>8.0} ms", ms(t));

    let sentences: Vec<String> = SENTENCES.iter().map(|s| s.to_string()).collect();
    let a = f32m.encode(&sentences);
    let b = i8m.encode(&sentences);
    let fidelity = a.iter().zip(&b).map(|(x, y)| cosine(x, y)).fold(1.0_f32, f32::min);
    println!("fidelidad int8 vs f32  coseno mínimo {fidelity:.5}");

    println!("\nsimilitud con «{}» (int8):", SENTENCES[0]);
    for (i, f) in SENTENCES.iter().enumerate().skip(1) {
        println!("  {:.3}  {f}", cosine(&b[0], &b[i]));
    }

    let one = vec!["el middleware de autenticación no se ejecuta en las rutas de salud".to_string()];
    let t = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(i8m.encode(&one));
    }
    println!("\nencode 1 frase       {:>8.3} ms", ms(t) / 1000.0);
    Ok(())
}

/// Snapshot folder in the Hugging Face cache (downloads it if needed).
fn hf_hub_dir() -> Result<PathBuf> {
    // from_pretrained with the repo id downloads to the cache; then we look for the snapshot to read the files.
    StaticModel::from_pretrained(REPO, None, None, None)?;
    let base = std::env::home_dir().unwrap().join(".cache/huggingface/hub/models--minishlab--potion-multilingual-128M/snapshots");
    let snap = std::fs::read_dir(&base)?.next().context("sin snapshot")??.path();
    Ok(snap)
}

fn load(dir: &Path) -> Result<()> {
    let t = Instant::now();
    let m = StaticModel::from_pretrained(dir, None, None, None)?;
    let load_ms = ms(t);
    let t = Instant::now();
    let v = m.encode(&["fallo de login".to_string()]);
    println!("carga {load_ms:.0} ms · primer encode {:.2} ms · dim {}", ms(t), v[0].len());
    Ok(())
}

/// Deterministic generator, to avoid adding dependencies.
struct Xs(u64);
impl Xs {
    fn n(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.n() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

fn vector(r: &mut Xs) -> Vec<u8> {
    let v: Vec<f32> = (0..256).map(|_| r.f()).collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().flat_map(|x| (x / n).to_le_bytes()).collect()
}

fn search(n: usize) -> Result<()> {
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let path = std::env::temp_dir().join("omoikane-fase0.db");
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", path.display()));
    }
    let mut db = rusqlite::Connection::open(&path)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE VIRTUAL TABLE v USING vec0(e float[256] distance_metric=cosine);
         CREATE VIRTUAL TABLE f USING fts5(title, content, tokenize='unicode61 remove_diacritics 2');",
    )?;

    // Synthetic 3000-word vocabulary so BM25 has something realistic to score.
    let mut r = Xs(0x9E3779B97F4A7C15);
    let syllables = ["ca", "lo", "ser", "vi", "dor", "men", "to", "ra", "pre", "con", "fi", "gu", "ción", "es", "tá", "bu", "ño", "au", "te", "di"];
    let words: Vec<String> = (0..3000)
        .map(|_| (0..2 + r.n() % 3).map(|_| syllables[(r.n() % 20) as usize]).collect())
        .collect();
    let sentence = |r: &mut Xs, k: u64| -> String {
        (0..k).map(|_| words[(r.n() % 3000) as usize].as_str()).collect::<Vec<_>>().join(" ")
    };

    let t = Instant::now();
    let tx = db.transaction()?;
    {
        let mut iv = tx.prepare("INSERT INTO v(rowid, e) VALUES (?, ?)")?;
        let mut if_ = tx.prepare("INSERT INTO f(rowid, title, content) VALUES (?, ?, ?)")?;
        for i in 1..=n as i64 {
            iv.execute(rusqlite::params![i, vector(&mut r)])?;
            let (ti, co) = (sentence(&mut r, 6), sentence(&mut r, 60));
            if_.execute(rusqlite::params![i, ti, co])?;
        }
    }
    tx.commit()?;
    println!("insertar {n} memorias   {:>8.0} ms", ms(t));
    println!("tamaño de la base        {:>8.1} MB", std::fs::metadata(&path)?.len() as f64 / 1e6);

    let queries: Vec<Vec<u8>> = (0..50).map(|_| vector(&mut r)).collect();
    {
        let mut q = db.prepare("SELECT rowid, distance FROM v WHERE e MATCH ? AND k = 40")?;
        let t = Instant::now();
        for c in &queries {
            let rows: Vec<i64> = q.query_map([c], |f| f.get(0))?.collect::<Result<_, _>>()?;
            std::hint::black_box(rows);
        }
        println!("KNN vec0 k=40            {:>8.2} ms por consulta", ms(t) / queries.len() as f64);
    }

    let texts: Vec<String> = (0..50).map(|_| sentence(&mut r, 4)).collect();
    {
        let mut q = db.prepare("SELECT rowid FROM f WHERE f MATCH ? ORDER BY bm25(f, 3.0, 1.0) LIMIT 40")?;
        let t = Instant::now();
        for tx in &texts {
            let query = tx.split(' ').map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(" OR ");
            let rows: Vec<i64> = q.query_map([query], |f| f.get(0))?.collect::<Result<_, _>>()?;
            std::hint::black_box(rows);
        }
        println!("FTS5 bm25 top 40         {:>8.2} ms por consulta", ms(t) / texts.len() as f64);
    }
    drop(db);
    let t = Instant::now();
    let db = rusqlite::Connection::open(&path)?;
    let _: i64 = db.query_row("SELECT count(*) FROM f WHERE f MATCH '\"cador\"'", [], |f| f.get(0))?;
    println!("abrir base + 1 consulta  {:>8.2} ms (lo que paga el hook)", ms(t));
    Ok(())
}
