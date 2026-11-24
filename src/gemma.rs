//! EmbeddingGemma 300M in pure Rust, for the meaning (semantic) channel.
//!
//! Reproduces what sentence-transformers does with `google/embeddinggemma-300m` (validated against it
//! with cosine 1.00000 in float32): prompt + text → tokenizer → Gemma 3 with bidirectional attention →
//! mean of the tokens → two dense layers (768 → 3072 → 768) → normalize.
//!
//! No ONNX Runtime on purpose: the published ONNX uses Microsoft's own operators
//! (`MultiHeadAttention`, `RotaryEmbedding`…) that only ONNX Runtime understands, and Omoikane depends
//! on nothing outside the binary.
//!
//! Two engines give the same vector:
//! - `engine` (default): f16 weights read from the mapped file, our own matrix multiplications with
//!   AVX2 (or the generic path without it). Loads in ~0.1 s and ~30 ms per query on 8 threads.
//! - candle (`OMOIKANE_GEMMA_RAPIDO=0`, and the one compared against in tests): f32 weights in RAM.
//!
//! And two tokenizers that give the same ids: the compiled one (`bpe`, mapped in milliseconds) and, if it
//! cannot be compiled, `tokenizers` (1.3-1.6 s to read `tokenizer.json`).
//!
//! Reads two model formats. The sentence-transformers one (1.2 GB in float32), for testing and for
//! converting. And the compact one, the one that gets installed (`compactar`): the 262,144-token table, which is
//! 805 of the 1,200 MB, in int8 with one scale per row and mapped from disk (only the rows of
//! the tokens that appear are read); the rest of the weights, in float16.

use anyhow::{Context, Result, anyhow, ensure};
use candle_core::{D, DType, Device, Tensor};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub const DIM: usize = 768;
const QUERY_PROMPT: &str = "task: search result | query: ";
const DOCUMENT_PROMPT: &str = "title: none | text: ";
/// What sentence-transformers reads (`max_seq_length`). It used to be 512 without a window mask, and
/// texts over 257 tokens gave a different vector from the reference one (cosine 0.996 to 0.987 with it;
/// in a real memory, 0.22 instead of 0.26 with its query, and a different order). With the mask in
/// `vector`: 0.9992 with 376 tokens and 0.99997 with short texts.
pub const MAX_TOKENS: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tipo {
    Pregunta,
    Documento,
}

impl Tipo {
    fn prompt(self) -> &'static str {
        match self {
            Tipo::Pregunta => QUERY_PROMPT,
            Tipo::Documento => DOCUMENT_PROMPT,
        }
    }
}

/// Files of the compact format.
const WEIGHTS_FILE: &str = "pesos.safetensors";
const TABLE_FILE: &str = "tokens.i8";
const SCALES_FILE: &str = "tokens.escala";

/// The table of input vectors for each token.
enum Table {
    F32(Tensor),
    /// Row `i`: `int8[i·width .. (i+1)·width] × scale[i]`.
    Int8 { rows: memmap2::Mmap, scales: Vec<f32>, width: usize },
}

impl Table {
    fn rows(&self, ids: &[u32]) -> Result<Tensor> {
        match self {
            Table::F32(t) => Ok(t.index_select(&Tensor::new(ids, &Device::Cpu)?, 0)?),
            Table::Int8 { width, .. } => Ok(Tensor::from_vec(self.rows_vec(ids)?, (ids.len(), *width), &Device::Cpu)?),
        }
    }

    fn rows_vec(&self, ids: &[u32]) -> Result<Vec<f32>> {
        match self {
            Table::F32(_) => Ok(self.rows(ids)?.flatten_all()?.to_vec1::<f32>()?),
            Table::Int8 { rows, scales, width } => {
                let mut v = Vec::with_capacity(ids.len() * width);
                for &id in ids {
                    // `<image_soft_token>` (262,144) is an added token that is not in the table: written
                    // as is in a memory, sentence-transformers would fail. Here it counts as `<unk>`
                    // (3), so that memory does not stop the indexing of the others.
                    let i = if (id as usize) < scales.len() { id as usize } else { 3 };
                    let e = scales[i];
                    v.extend(rows[i * width..(i + 1) * width].iter().map(|&b| f32::from(b as i8) * e));
                }
                Ok(v)
            }
        }
    }
}

/// The tokenizer: the compiled one (`bpe`) or, if that failed, the `tokenizers` one. There is one per loaded
/// model: the size of the large variant does not matter.
#[allow(clippy::large_enum_variant)]
enum Tok {
    Bpe(crate::bpe::Bpe),
    Hf(Box<tokenizers::Tokenizer>),
}

impl Tok {
    /// The compiled one if the folder is in the compact format (it compiles it the first time and leaves it
    /// alongside; `OMOIKANE_GEMMA_BPE=0` does not use it). In a sentence-transformers folder it writes nothing.
    fn load(dir: &Path, compact: bool) -> Result<Tok> {
        let json = dir.join("tokenizer.json");
        if compact && std::env::var("OMOIKANE_GEMMA_BPE").as_deref() != Ok("0") {
            match crate::bpe::Bpe::open_or_compile(&json, &dir.join(crate::bpe::FILE)) {
                Ok(b) => return Ok(Tok::Bpe(b)),
                Err(e) => eprintln!("omoi: {}: {e:#}; using tokenizer.json", crate::bpe::FILE),
            }
        }
        Self::hf(&json)
    }

    fn hf(json: &Path) -> Result<Tok> {
        let mut tok = tokenizers::Tokenizer::from_file(json).map_err(|e| anyhow!("tokenizer.json: {e}"))?;
        tok.with_truncation(Some(tokenizers::TruncationParams { max_length: MAX_TOKENS, ..Default::default() })).map_err(|e| anyhow!("{e}"))?;
        tok.with_padding(None);
        Ok(Tok::Hf(Box::new(tok)))
    }

    fn ids(&self, text: &str) -> Result<Vec<u32>> {
        match self {
            Tok::Bpe(b) => Ok(b.encode(text, MAX_TOKENS)),
            Tok::Hf(t) => Ok(t.encode(text, true).map_err(|e| anyhow!("{e}"))?.get_ids().to_vec()),
        }
    }
}

struct CandleLayer {
    norm_in: Tensor,
    norm_post_attn: Tensor,
    norm_pre_mlp: Tensor,
    norm_post_mlp: Tensor,
    norm_q: Tensor,
    norm_k: Tensor,
    q: Tensor,
    k: Tensor,
    v: Tensor,
    o: Tensor,
    gate: Tensor,
    up: Tensor,
    down: Tensor,
    /// Sliding-window layer: its RoPE uses the local base.
    local: bool,
}

/// The model in candle, in f32.
struct Candle {
    layers: Vec<CandleLayer>,
    norm: Tensor,
    dense1: Tensor,
    dense2: Tensor,
}

/// One per loaded model: the size of the large variant does not matter.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Fast(crate::engine::Engine, rayon::ThreadPool),
    Candle(Candle),
}

pub struct Gemma {
    /// `pesos.safetensors` mapped, if the fast engine reads from it (for `precalentar`).
    map: Option<Arc<memmap2::Mmap>>,
    tokens: Table,
    tok: Tok,
    backend: Backend,
    eps: f64,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
    scale: f64,
    token_scale: f64,
    local_base: f64,
    global_base: f64,
    window: usize,
}

/// Does `dir` have EmbeddingGemma, in the sentence-transformers format or in the compact one?
pub fn es_modelo(dir: &Path) -> bool {
    es_sentence_transformers(dir) || es_compacto(dir)
}

pub fn es_sentence_transformers(dir: &Path) -> bool {
    ["config.json", "model.safetensors", "tokenizer.json", "2_Dense/model.safetensors", "3_Dense/model.safetensors"].iter().all(|f| dir.join(f).is_file())
}

pub fn es_compacto(dir: &Path) -> bool {
    ["config.json", "tokenizer.json", WEIGHTS_FILE, TABLE_FILE, SCALES_FILE].iter().all(|f| dir.join(f).is_file())
}

/// The files of the compact format, in download order.
pub const FICHEROS_COMPACTO: [&str; 5] = ["config.json", "tokenizer.json", SCALES_FILE, WEIGHTS_FILE, TABLE_FILE];

/// The sentence-transformers weights with the compact format's names: the model's as they are
/// and the two dense layers as `densa1.weight` and `densa2.weight`.
fn sentence_transformers_weights(dir: &Path) -> Result<HashMap<String, Tensor>> {
    let dev = Device::Cpu;
    let mut weights = candle_core::safetensors::load(dir.join("model.safetensors"), &dev)?;
    for (d, name) in [("2_Dense", "densa1.weight"), ("3_Dense", "densa2.weight")] {
        let mut t = candle_core::safetensors::load(dir.join(d).join("model.safetensors"), &dev)?;
        weights.insert(name.into(), t.remove("linear.weight").ok_or_else(|| anyhow!("{d}: missing linear.weight"))?);
    }
    Ok(weights)
}

/// Writes the compact format into `dest` from the sentence-transformers folder `source`:
/// `config.json` and `tokenizer.json` as they are, the token table in int8 (per-row, symmetric scale)
/// and the rest in float16. Each file is written as `.part` and renamed when done.
pub fn compactar(source: &Path, dest: &Path) -> Result<()> {
    ensure!(es_sentence_transformers(source), "{}: not a sentence-transformers EmbeddingGemma folder", source.display());
    std::fs::create_dir_all(dest)?;
    let mut weights = sentence_transformers_weights(source)?;
    let table = weights.remove("embed_tokens.weight").ok_or_else(|| anyhow!("model.safetensors: missing embed_tokens.weight"))?.to_dtype(DType::F32)?;
    let (n, width) = table.dims2()?;
    let data = table.flatten_all()?.to_vec1::<f32>()?;
    drop(table);
    let mut rows = Vec::with_capacity(n * width);
    let mut scales = Vec::with_capacity(n * 4);
    for row in data.chunks_exact(width) {
        let max = row.iter().fold(0f32, |m, x| m.max(x.abs()));
        let e = if max > 0.0 { max / 127.0 } else { 1.0 };
        rows.extend(row.iter().map(|x| (x / e).round().clamp(-127.0, 127.0) as i8 as u8));
        scales.extend_from_slice(&e.to_le_bytes());
    }
    drop(data);
    let rest: HashMap<String, Tensor> = weights.into_iter().map(|(k, v)| Ok((k, v.to_dtype(DType::F16)?))).collect::<Result<_>>()?;
    let write = |name: &str, f: &dyn Fn(&Path) -> Result<()>| -> Result<()> {
        let part = dest.join(format!("{name}.part"));
        f(&part)?;
        std::fs::rename(&part, dest.join(name))?;
        Ok(())
    };
    write(TABLE_FILE, &|p| Ok(std::fs::write(p, &rows)?))?;
    write(SCALES_FILE, &|p| Ok(std::fs::write(p, &scales)?))?;
    write(WEIGHTS_FILE, &|p| Ok(candle_core::safetensors::save(&rest, p)?))?;
    for f in ["config.json", "tokenizer.json"] {
        write(f, &|p| Ok(std::fs::copy(source.join(f), p).map(|_| ())?))?;
    }
    Ok(())
}

/// `x / rms(x) * (1 + w)`: Gemma's RMSNorm stores the weight shifted by 1.
fn rms(x: &Tensor, w: &Tensor, eps: f64) -> Result<Tensor> {
    let v = x.sqr()?.mean_keepdim(D::Minus1)?;
    Ok(x.broadcast_div(&(v + eps)?.sqrt()?)?.broadcast_mul(&(w + 1.0)?)?)
}

/// `x · Wᵀ` for `x` with 2 or 3 dimensions.
fn linear(x: &Tensor, w: &Tensor) -> Result<Tensor> {
    Ok(x.broadcast_matmul(&w.t()?)?)
}

/// What is read from `config.json`.
struct Cfg {
    eps: f64,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
    scale: f64,
    token_scale: f64,
    local_base: f64,
    global_base: f64,
    window: usize,
    width: usize,
    /// Per layer: is it sliding-window?
    locals: Vec<bool>,
}

impl Cfg {
    fn read(dir: &Path) -> Result<Cfg> {
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let num = |k: &str| cfg[k].as_f64().ok_or_else(|| anyhow!("config.json: missing {k}"));
        let kinds = cfg["layer_types"].as_array().context("config.json: missing layer_types")?;
        let width = num("hidden_size")?;
        Ok(Cfg {
            eps: num("rms_norm_eps")?,
            heads: num("num_attention_heads")? as usize,
            kv_heads: num("num_key_value_heads")? as usize,
            head_dim: num("head_dim")? as usize,
            scale: num("query_pre_attn_scalar")?.powf(-0.5),
            token_scale: width.sqrt(),
            local_base: num("rope_local_base_freq")?,
            global_base: num("rope_theta")?,
            window: num("sliding_window")? as usize,
            width: width as usize,
            locals: kinds.iter().map(|t| t.as_str() == Some("sliding_attention")).collect(),
        })
    }
}

/// Where `engine` gets the weights from: `pesos.safetensors` mapped (the compact format, in f16: the
/// matrices are used right there, without copying them) or already loaded tensors (sentence-transformers).
enum Source {
    Mapped { map: Arc<memmap2::Mmap>, tensors: HashMap<String, (usize, Vec<usize>, safetensors::Dtype)> },
    Tensors(HashMap<String, Tensor>),
}

impl Source {
    fn mapped(path: &Path) -> Result<Source> {
        // SAFETY: the file is not touched after it is installed (`modelos` installs into another folder).
        let map = Arc::new(unsafe { memmap2::Mmap::map(&std::fs::File::open(path)?)? });
        let st = safetensors::SafeTensors::deserialize(&map[..])?;
        let base = map.as_ptr() as usize;
        let tensors = st.tensors().into_iter().map(|(n, v)| (n, (v.data().as_ptr() as usize - base, v.shape().to_vec(), v.dtype()))).collect();
        Ok(Source::Mapped { map, tensors })
    }

    fn f32s(&mut self, k: &str) -> Result<(Vec<f32>, Vec<usize>)> {
        match self {
            Source::Mapped { map, tensors } => {
                let (start, shape, dtype) = tensors.get(k).with_context(|| format!("{WEIGHTS_FILE}: missing {k}"))?;
                let n: usize = shape.iter().product();
                let b = &map[*start..];
                let v = match dtype {
                    safetensors::Dtype::F16 => b[..2 * n].as_chunks::<2>().0.iter().map(|x| half::f16::from_le_bytes(*x).to_f32()).collect(),
                    safetensors::Dtype::F32 => b[..4 * n].as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect(),
                    t => anyhow::bail!("{WEIGHTS_FILE}: {k} is {t:?}"),
                };
                Ok((v, shape.clone()))
            }
            Source::Tensors(t) => {
                let t = t.remove(k).with_context(|| format!("model.safetensors: missing {k}"))?.to_dtype(DType::F32)?;
                Ok((t.flatten_all()?.to_vec1::<f32>()?, t.dims().to_vec()))
            }
        }
    }

    /// A Gemma norm with the `1 +` already added.
    fn norm(&mut self, k: &str) -> Result<Vec<f32>> {
        Ok(self.f32s(k)?.0.into_iter().map(|x| x + 1.0).collect())
    }

    /// The matrices `ks` one after another. From the mapped file in f16, without copying them.
    fn mat(&mut self, ks: &[String], simd: bool) -> Result<crate::engine::Mat> {
        use crate::engine::Mat;
        if let Source::Mapped { map, tensors } = self
            && ks.iter().all(|k| tensors.get(k).is_some_and(|t| t.2 == safetensors::Dtype::F16 && t.1.len() == 2 && t.0 % 2 == 0))
        {
            let cols = tensors[&ks[0]].1[1];
            let mut parts = Vec::new();
            for k in ks {
                let (start, shape, _) = &tensors[k];
                ensure!(shape[1] == cols, "{WEIGHTS_FILE}: {k} has {} columns, not {cols}", shape[1]);
                // SAFETY: inside the file (checked by `deserialize`), even and in f16.
                parts.push((unsafe { map.as_ptr().add(*start) } as *const u16, shape[0], map.clone() as Arc<dyn Send + Sync>));
            }
            // SAFETY: each part is rows·cols f16 of the map, which goes along as owner and does not change.
            return Ok(unsafe { Mat::from_parts(parts, cols, simd) });
        }
        let mut data = Vec::new();
        let (mut rows, mut cols) = (0, 0);
        for k in ks {
            let (v, shape) = self.f32s(k)?;
            ensure!(shape.len() == 2, "{k}: not a matrix");
            rows += shape[0];
            cols = shape[1];
            data.extend(v);
        }
        Ok(Mat::from_f32(&data, rows, cols, simd))
    }
}

impl Gemma {
    /// With the fast engine (`engine`) if the CPU has AVX2/FMA/F16C; otherwise (old CPUs, ARM), with
    /// candle: the fast engine's generic path gives the same vector but is slower than candle (measured:
    /// 56 versus 42 ms with 16 tokens, twice that with long texts). `OMOIKANE_GEMMA_RAPIDO=0` forces
    /// candle and `=1` the fast one (generic, if there is no AVX2).
    pub fn cargar(dir: &Path) -> Result<Self> {
        let simd = crate::engine::simd_available();
        match std::env::var("OMOIKANE_GEMMA_RAPIDO").as_deref() {
            Ok("0") => Self::cargar_candle(dir),
            Ok("1") => Self::cargar_rapido(dir, Self::hilos_por_defecto(), simd),
            _ if simd => Self::cargar_rapido(dir, Self::hilos_por_defecto(), true),
            _ => Self::cargar_candle(dir),
        }
    }

    /// Reads the mapped weights once, so the first query does not pay for the page faults
    /// (~30 ms with the disk in cache). The service calls it on a separate thread right after loading.
    pub fn precalentar(&self) {
        if let Some(m) = &self.map {
            let mut s = 0u8;
            for i in (0..m.len()).step_by(4096) {
                s = s.wrapping_add(m[i]);
            }
            std::hint::black_box(s);
        }
    }

    /// Threads of the fast engine: `OMOIKANE_GEMMA_HILOS` or the CPU's.
    pub fn hilos_por_defecto() -> usize {
        std::env::var("OMOIKANE_GEMMA_HILOS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
    }

    /// Is it running with the fast engine?
    pub fn es_rapido(&self) -> bool {
        matches!(self.backend, Backend::Fast(..))
    }

    /// Does it use the compiled tokenizer?
    pub fn tokenizer_compilado(&self) -> bool {
        matches!(self.tok, Tok::Bpe(_))
    }

    fn with_backend(dir: &Path, cfg: Cfg, tokens: Table, compact: bool, backend: Backend) -> Result<Self> {
        Ok(Gemma {
            map: None,
            tokens,
            tok: Tok::load(dir, compact)?,
            backend,
            eps: cfg.eps,
            heads: cfg.heads,
            kv_heads: cfg.kv_heads,
            head_dim: cfg.head_dim,
            scale: cfg.scale,
            token_scale: cfg.token_scale,
            local_base: cfg.local_base,
            global_base: cfg.global_base,
            window: cfg.window,
        })
    }

    fn int8_table(dir: &Path, width: usize) -> Result<Table> {
        // SAFETY: the file is not touched after it is installed (`modelos` installs into another folder).
        let rows = unsafe { memmap2::Mmap::map(&std::fs::File::open(dir.join(TABLE_FILE))?)? };
        let scales: Vec<f32> = std::fs::read(dir.join(SCALES_FILE))?.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        ensure!(rows.len() == scales.len() * width, "{TABLE_FILE}: {} bytes for {} rows of {width}", rows.len(), scales.len());
        Ok(Table::Int8 { rows, scales, width })
    }

    /// The fast engine with `threads` threads; `simd` with AVX2/FMA/F16C (`engine::simd_available`).
    pub fn cargar_rapido(dir: &Path, threads: usize, simd: bool) -> Result<Self> {
        use crate::engine::{Engine, Layer, rope_table};
        ensure!(es_modelo(dir), "{}: not an EmbeddingGemma folder", dir.display());
        let cfg = Cfg::read(dir)?;
        let compact = !es_sentence_transformers(dir);
        let mut f = if compact { Source::mapped(&dir.join(WEIGHTS_FILE))? } else { Source::Tensors(sentence_transformers_weights(dir)?) };
        let mut layers = Vec::with_capacity(cfg.locals.len());
        for (i, &local) in cfg.locals.iter().enumerate() {
            let p = |s: &str| format!("layers.{i}.{s}.weight");
            layers.push(Layer {
                norm_in: f.norm(&p("input_layernorm"))?,
                norm_post_attn: f.norm(&p("post_attention_layernorm"))?,
                norm_pre_mlp: f.norm(&p("pre_feedforward_layernorm"))?,
                norm_post_mlp: f.norm(&p("post_feedforward_layernorm"))?,
                norm_q: f.norm(&p("self_attn.q_norm"))?,
                norm_k: f.norm(&p("self_attn.k_norm"))?,
                qkv: f.mat(&[p("self_attn.q_proj"), p("self_attn.k_proj"), p("self_attn.v_proj")], simd)?,
                o: f.mat(&[p("self_attn.o_proj")], simd)?,
                gate_up: f.mat(&[p("mlp.gate_proj"), p("mlp.up_proj")], simd)?,
                down: f.mat(&[p("mlp.down_proj")], simd)?,
                local,
            });
        }
        let tokens = match &mut f {
            Source::Mapped { .. } => Self::int8_table(dir, cfg.width)?,
            Source::Tensors(t) => Table::F32(t.remove("embed_tokens.weight").context("model.safetensors: missing embed_tokens.weight")?.to_dtype(DType::F32)?),
        };
        let r = Engine {
            layers,
            norm: f.norm("norm.weight")?,
            dense1: f.mat(&["densa1.weight".into()], simd)?,
            dense2: f.mat(&["densa2.weight".into()], simd)?,
            eps: cfg.eps as f32,
            heads: cfg.heads,
            kv_heads: cfg.kv_heads,
            head_dim: cfg.head_dim,
            scale: cfg.scale as f32,
            half_window: cfg.window / 2,
            rope_local: rope_table(MAX_TOKENS, cfg.head_dim, cfg.local_base),
            rope_global: rope_table(MAX_TOKENS, cfg.head_dim, cfg.global_base),
            simd,
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).thread_name(|i| format!("gemma-{i}")).build()?;
        let map = match f {
            Source::Mapped { map, .. } => Some(map),
            Source::Tensors(_) => None,
        };
        let mut g = Self::with_backend(dir, cfg, tokens, compact, Backend::Fast(r, pool))?;
        g.map = map;
        Ok(g)
    }

    /// With candle, in f32 (as up to 1.2.0).
    pub fn cargar_candle(dir: &Path) -> Result<Self> {
        ensure!(es_modelo(dir), "{}: not an EmbeddingGemma folder", dir.display());
        let dev = Device::Cpu;
        let cfg = Cfg::read(dir)?;
        let compact = !es_sentence_transformers(dir);
        let mut weights = if compact { candle_core::safetensors::load(dir.join(WEIGHTS_FILE), &dev)? } else { sentence_transformers_weights(dir)? };
        let mut take = |k: String| -> Result<Tensor> { Ok(weights.remove(&k).ok_or_else(|| anyhow!("model.safetensors: missing {k}"))?.to_dtype(DType::F32)?) };
        let mut layers = Vec::with_capacity(cfg.locals.len());
        for (i, &local) in cfg.locals.iter().enumerate() {
            let p = |s: &str| format!("layers.{i}.{s}.weight");
            layers.push(CandleLayer {
                norm_in: take(p("input_layernorm"))?,
                norm_post_attn: take(p("post_attention_layernorm"))?,
                norm_pre_mlp: take(p("pre_feedforward_layernorm"))?,
                norm_post_mlp: take(p("post_feedforward_layernorm"))?,
                norm_q: take(p("self_attn.q_norm"))?,
                norm_k: take(p("self_attn.k_norm"))?,
                q: take(p("self_attn.q_proj"))?,
                k: take(p("self_attn.k_proj"))?,
                v: take(p("self_attn.v_proj"))?,
                o: take(p("self_attn.o_proj"))?,
                gate: take(p("mlp.gate_proj"))?,
                up: take(p("mlp.up_proj"))?,
                down: take(p("mlp.down_proj"))?,
                local,
            });
        }
        let tokens = if compact { Self::int8_table(dir, cfg.width)? } else { Table::F32(take("embed_tokens.weight".into())?) };
        let c = Candle { layers, norm: take("norm.weight".into())?, dense1: take("densa1.weight".into())?, dense2: take("densa2.weight".into())? };
        Self::with_backend(dir, cfg, tokens, compact, Backend::Candle(c))
    }

    /// The ids of `text` with its kind's prompt (with `<bos>` and `<eos>`, up to `MAX_TOKENS`).
    pub fn ids(&self, text: &str, kind: Tipo) -> Result<Vec<u32>> {
        self.tok.ids(&format!("{}{text}", kind.prompt()))
    }

    /// The vector (768, normalized) of `text`, with its kind's prompt.
    pub fn vector(&self, text: &str, kind: Tipo) -> Result<Vec<f32>> {
        let ids = self.ids(text, kind)?;
        Ok(self.vectores_de_ids(&[ids], &|| {})?.pop().unwrap_or_default())
    }

    /// The vectors of several texts (with the fast engine, all at once: the same result as one
    /// by one, faster).
    pub fn vectores(&self, texts: &[&str], kind: Tipo) -> Result<Vec<Vec<f32>>> {
        let ids = texts.iter().map(|t| self.ids(t, kind)).collect::<Result<Vec<_>>>()?;
        self.vectores_de_ids(&ids, &|| {})
    }

    /// The vectors of already tokenized texts (`ids`). `yield_now` is called between steps: the
    /// indexer uses it to let a query go first.
    pub fn vectores_de_ids(&self, ids: &[Vec<u32>], yield_now: &dyn Fn()) -> Result<Vec<Vec<f32>>> {
        match &self.backend {
            Backend::Fast(r, pool) => {
                let all: Vec<u32> = ids.iter().flatten().copied().collect();
                let e = self.token_scale as f32;
                let x = self.tokens.rows_vec(&all)?.into_iter().map(|v| v * e).collect();
                let lens: Vec<usize> = ids.iter().map(Vec::len).collect();
                Ok(r.vectors(x, &lens, pool, yield_now))
            }
            Backend::Candle(c) => ids
                .iter()
                .map(|i| {
                    yield_now();
                    self.vector_candle(c, i)
                })
                .collect(),
        }
    }

    /// RoPE cosines and sines for `n` positions with base `base`.
    fn rope(&self, n: usize, base: f64) -> Result<(Tensor, Tensor)> {
        let d = self.head_dim;
        let inv: Vec<f32> = (0..d / 2).map(|i| (1.0 / base.powf(2.0 * i as f64 / d as f64)) as f32).collect();
        let inv = Tensor::new(inv, &Device::Cpu)?.unsqueeze(0)?;
        let pos = Tensor::arange(0u32, n as u32, &Device::Cpu)?.to_dtype(DType::F32)?.unsqueeze(1)?;
        let f = pos.broadcast_mul(&inv)?;
        Ok((f.cos()?, f.sin()?))
    }

    fn vector_candle(&self, c: &Candle, ids: &[u32]) -> Result<Vec<f32>> {
        let n = ids.len();
        let (h, hd, kv) = (self.heads, self.head_dim, self.kv_heads);
        let mut x = (self.tokens.rows(ids)? * self.token_scale)?.unsqueeze(0)?;
        let local = self.rope(n, self.local_base)?;
        let global = self.rope(n, self.global_base)?;
        // Mask for the local layers: each token sees those at half a window or less, on each
        // side (what transformers does, checked with its attention weights: with window 512, token
        // 300 sees 44 to 556). Only needed if the text goes beyond that distance.
        let half = self.window / 2;
        let mask = if n > half + 1 {
            let w = half as i64;
            let m: Vec<f32> = (0..n as i64).flat_map(|i| (0..n as i64).map(move |j| if (i - j).abs() <= w { 0.0 } else { f32::NEG_INFINITY })).collect();
            Some(Tensor::from_vec(m, (1, 1, n, n), &Device::Cpu)?)
        } else {
            None
        };
        for c in &c.layers {
            let e = rms(&x, &c.norm_in, self.eps)?;
            let q = rms(&linear(&e, &c.q)?.reshape((1, n, h, hd))?, &c.norm_q, self.eps)?.transpose(1, 2)?.contiguous()?;
            let k = rms(&linear(&e, &c.k)?.reshape((1, n, kv, hd))?, &c.norm_k, self.eps)?.transpose(1, 2)?.contiguous()?;
            let v = linear(&e, &c.v)?.reshape((1, n, kv, hd))?.transpose(1, 2)?.contiguous()?;
            let (cos, sin) = if c.local { &local } else { &global };
            let q = candle_nn::rotary_emb::rope(&q, cos, sin)?;
            let k = candle_nn::rotary_emb::rope(&k, cos, sin)?.repeat((1, h / kv, 1, 1))?;
            let v = v.repeat((1, h / kv, 1, 1))?;
            // Bidirectional and without padding (one text per call): only the window mask.
            let mut scores = (q.matmul(&k.t()?)? * self.scale)?;
            if c.local
                && let Some(m) = &mask
            {
                scores = scores.broadcast_add(m)?;
            }
            let a = candle_nn::ops::softmax_last_dim(&scores)?;
            let a = a.matmul(&v)?.transpose(1, 2)?.reshape((1, n, h * hd))?;
            x = (x + rms(&linear(&a, &c.o)?, &c.norm_post_attn, self.eps)?)?;
            let e = rms(&x, &c.norm_pre_mlp, self.eps)?;
            let m = linear(&(linear(&e, &c.gate)?.gelu()? * linear(&e, &c.up)?)?, &c.down)?;
            x = (x + rms(&m, &c.norm_post_mlp, self.eps)?)?;
        }
        let x = rms(&x, &c.norm, self.eps)?.mean(1)?;
        let x = linear(&linear(&x, &c.dense1)?, &c.dense2)?;
        let x = x.broadcast_div(&x.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
        Ok(x.squeeze(0)?.to_vec1()?)
    }
}
