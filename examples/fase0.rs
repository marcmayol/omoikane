//! Fase 0 del PLAN.md: medir antes de decidir.
//!
//!   cargo run --release --example fase0 -- cuantizar   descarga el modelo f32, escribe el int8 propio y compara
//!   cargo run --release --example fase0 -- cargar DIR  carga un modelo y codifica una frase (para medir en frío)
//!   cargo run --release --example fase0 -- buscar N    vec0 y FTS5 con N memorias sintéticas

use anyhow::{Context, Result, anyhow};
use model2vec_rs::model::StaticModel;
use safetensors::{Dtype, SafeTensors, tensor::TensorView};
use std::path::{Path, PathBuf};
use std::time::Instant;

const REPO: &str = "minishlab/potion-multilingual-128M";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("cuantizar") => cuantizar(),
        Some("cargar") => cargar(Path::new(args.get(1).context("falta DIR")?)),
        Some("buscar") => buscar(args.get(1).map_or(Ok(50_000), |n| n.parse())?),
        _ => Err(anyhow!("uso: fase0 cuantizar | cargar DIR | buscar N")),
    }
}

fn dir_int8() -> PathBuf {
    dirs::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-int8")
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn coseno(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

const FRASES: &[&str] = &[
    "fallo de login en la pantalla de acceso",
    "bug en auth: el token caduca antes de tiempo",
    "el middleware de autenticación no se ejecuta en /health",
    "usamos ULID en vez de UUID para que los ids se ordenen por fecha",
    "prefiero commits cortos y en inglés",
    "la impresora 3D necesita brim para las piezas finas",
    "receta de tortilla de patatas con cebolla",
    "login failure on the sign-in screen",
];

fn cuantizar() -> Result<()> {
    // Descarga (o reutiliza de la caché de Hugging Face) el modelo oficial en f32.
    let t = Instant::now();
    let api = hf_hub_dir()?;
    println!("descarga/caché       {:>8.0} ms  {}", ms(t), api.display());

    let t = Instant::now();
    let f32m = StaticModel::from_pretrained(&api, None, None, None)?;
    println!("carga f32            {:>8.0} ms", ms(t));

    // int8 propio: una escala por fila, plegada en `weights` para que el fichero la lleve consigo.
    let bytes = std::fs::read(api.join("model.safetensors"))?;
    let st = SafeTensors::deserialize(&bytes)?;
    let emb = st.tensor("embeddings").or_else(|_| st.tensor("0")).context("sin embeddings")?;
    let [filas, dim]: [usize; 2] = emb.shape().try_into()?;
    anyhow::ensure!(emb.dtype() == Dtype::F32, "se esperaba f32, es {:?}", emb.dtype());
    let tabla: Vec<f32> = emb.data().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let mapeo: Option<Vec<usize>> = st.tensor("mapping").ok().map(|t| match t.dtype() {
        Dtype::I64 => t.data().chunks_exact(8).map(|b| i64::from_le_bytes(b.try_into().unwrap()) as usize).collect(),
        _ => t.data().chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()) as usize).collect(),
    });
    let pesos: Option<Vec<f32>> = st.tensor("weights").ok().map(|t| match t.dtype() {
        Dtype::F64 => t.data().chunks_exact(8).map(|b| f64::from_le_bytes(b.try_into().unwrap()) as f32).collect(),
        _ => t.data().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect(),
    });
    println!(
        "tensores             filas {filas}, mapping {}, weights {}",
        mapeo.as_ref().map_or("no".into(), |m| m.len().to_string()),
        pesos.as_ref().map_or("no".into(), |w| w.len().to_string())
    );

    let mut datos = Vec::with_capacity(tabla.len());
    let mut escalas = Vec::with_capacity(filas);
    for fila in tabla.chunks_exact(dim) {
        let max = fila.iter().fold(0.0_f32, |m, &v| m.max(v.abs()));
        let e = max / 127.0;
        datos.extend(fila.iter().map(|&v| if e > 0.0 { (v / e).round().clamp(-127.0, 127.0) as i8 as u8 } else { 0 }));
        escalas.push(e);
    }
    let tokens = mapeo.as_ref().map_or(filas, Vec::len).max(pesos.as_ref().map_or(0, Vec::len));
    let nuevos: Vec<f32> = (0..tokens)
        .map(|tok| {
            let fila = mapeo.as_ref().map_or(tok, |m| m[tok]);
            pesos.as_ref().and_then(|w| w.get(tok)).copied().unwrap_or(1.0) * escalas[fila]
        })
        .collect();
    let nuevos_b: Vec<u8> = nuevos.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mapeo_b: Option<Vec<u8>> = mapeo.as_ref().map(|m| m.iter().flat_map(|&v| (v as i64).to_le_bytes()).collect());

    let salida = dir_int8();
    std::fs::create_dir_all(&salida)?;
    let mut vistas = vec![
        ("embeddings".to_string(), TensorView::new(Dtype::I8, vec![filas, dim], &datos)?),
        ("weights".to_string(), TensorView::new(Dtype::F32, vec![tokens], &nuevos_b)?),
    ];
    if let Some(m) = &mapeo_b {
        vistas.push(("mapping".to_string(), TensorView::new(Dtype::I64, vec![tokens], m)?));
    }
    safetensors::serialize_to_file(vistas, &None, &salida.join("model.safetensors"))?;
    for f in ["tokenizer.json", "config.json"] {
        std::fs::copy(api.join(f), salida.join(f))?;
    }
    let mb = |p: &Path| std::fs::metadata(p).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
    println!(
        "int8 escrito         {:.1} MB modelo + {:.1} MB tokenizer  {}",
        mb(&salida.join("model.safetensors")),
        mb(&salida.join("tokenizer.json")),
        salida.display()
    );

    let t = Instant::now();
    let i8m = StaticModel::from_pretrained(&salida, None, None, None)?;
    println!("carga int8           {:>8.0} ms", ms(t));

    let frases: Vec<String> = FRASES.iter().map(|s| s.to_string()).collect();
    let a = f32m.encode(&frases);
    let b = i8m.encode(&frases);
    let fidelidad = a.iter().zip(&b).map(|(x, y)| coseno(x, y)).fold(1.0_f32, f32::min);
    println!("fidelidad int8 vs f32  coseno mínimo {fidelidad:.5}");

    println!("\nsimilitud con «{}» (int8):", FRASES[0]);
    for (i, f) in FRASES.iter().enumerate().skip(1) {
        println!("  {:.3}  {f}", coseno(&b[0], &b[i]));
    }

    let una = vec!["el middleware de autenticación no se ejecuta en las rutas de salud".to_string()];
    let t = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(i8m.encode(&una));
    }
    println!("\nencode 1 frase       {:>8.3} ms", ms(t) / 1000.0);
    Ok(())
}

/// Carpeta del snapshot en la caché de Hugging Face (lo descarga si hace falta).
fn hf_hub_dir() -> Result<PathBuf> {
    // from_pretrained con el id del repo descarga a la caché; luego buscamos el snapshot para leer los ficheros.
    StaticModel::from_pretrained(REPO, None, None, None)?;
    let base = dirs::home_dir().unwrap().join(".cache/huggingface/hub/models--minishlab--potion-multilingual-128M/snapshots");
    let snap = std::fs::read_dir(&base)?.next().context("sin snapshot")??.path();
    Ok(snap)
}

fn cargar(dir: &Path) -> Result<()> {
    let t = Instant::now();
    let m = StaticModel::from_pretrained(dir, None, None, None)?;
    let carga = ms(t);
    let t = Instant::now();
    let v = m.encode(&["fallo de login".to_string()]);
    println!("carga {carga:.0} ms · primer encode {:.2} ms · dim {}", ms(t), v[0].len());
    Ok(())
}

/// Generador determinista, para no añadir dependencias.
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

fn buscar(n: usize) -> Result<()> {
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
    let ruta = std::env::temp_dir().join("omoikane-fase0.db");
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", ruta.display()));
    }
    let mut db = rusqlite::Connection::open(&ruta)?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE VIRTUAL TABLE v USING vec0(e float[256] distance_metric=cosine);
         CREATE VIRTUAL TABLE f USING fts5(title, content, tokenize='unicode61 remove_diacritics 2');",
    )?;

    // Vocabulario sintético de 3000 palabras para que BM25 tenga algo realista que puntuar.
    let mut r = Xs(0x9E3779B97F4A7C15);
    let silabas = ["ca", "lo", "ser", "vi", "dor", "men", "to", "ra", "pre", "con", "fi", "gu", "ción", "es", "tá", "bu", "ño", "au", "te", "di"];
    let palabras: Vec<String> = (0..3000)
        .map(|_| (0..2 + r.n() % 3).map(|_| silabas[(r.n() % 20) as usize]).collect())
        .collect();
    let frase = |r: &mut Xs, k: u64| -> String {
        (0..k).map(|_| palabras[(r.n() % 3000) as usize].as_str()).collect::<Vec<_>>().join(" ")
    };

    let t = Instant::now();
    let tx = db.transaction()?;
    {
        let mut iv = tx.prepare("INSERT INTO v(rowid, e) VALUES (?, ?)")?;
        let mut if_ = tx.prepare("INSERT INTO f(rowid, title, content) VALUES (?, ?, ?)")?;
        for i in 1..=n as i64 {
            iv.execute(rusqlite::params![i, vector(&mut r)])?;
            let (ti, co) = (frase(&mut r, 6), frase(&mut r, 60));
            if_.execute(rusqlite::params![i, ti, co])?;
        }
    }
    tx.commit()?;
    println!("insertar {n} memorias   {:>8.0} ms", ms(t));
    println!("tamaño de la base        {:>8.1} MB", std::fs::metadata(&ruta)?.len() as f64 / 1e6);

    let consultas: Vec<Vec<u8>> = (0..50).map(|_| vector(&mut r)).collect();
    {
        let mut q = db.prepare("SELECT rowid, distance FROM v WHERE e MATCH ? AND k = 40")?;
        let t = Instant::now();
        for c in &consultas {
            let filas: Vec<i64> = q.query_map([c], |f| f.get(0))?.collect::<Result<_, _>>()?;
            std::hint::black_box(filas);
        }
        println!("KNN vec0 k=40            {:>8.2} ms por consulta", ms(t) / consultas.len() as f64);
    }

    let textos: Vec<String> = (0..50).map(|_| frase(&mut r, 4)).collect();
    {
        let mut q = db.prepare("SELECT rowid FROM f WHERE f MATCH ? ORDER BY bm25(f, 3.0, 1.0) LIMIT 40")?;
        let t = Instant::now();
        for tx in &textos {
            let consulta = tx.split(' ').map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(" OR ");
            let filas: Vec<i64> = q.query_map([consulta], |f| f.get(0))?.collect::<Result<_, _>>()?;
            std::hint::black_box(filas);
        }
        println!("FTS5 bm25 top 40         {:>8.2} ms por consulta", ms(t) / textos.len() as f64);
    }
    drop(db);
    let t = Instant::now();
    let db = rusqlite::Connection::open(&ruta)?;
    let _: i64 = db.query_row("SELECT count(*) FROM f WHERE f MATCH '\"cador\"'", [], |f| f.get(0))?;
    println!("abrir base + 1 consulta  {:>8.2} ms (lo que paga el hook)", ms(t));
    Ok(())
}
