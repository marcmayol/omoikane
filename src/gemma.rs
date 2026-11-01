//! EmbeddingGemma 300M en Rust puro (candle, CPU), para el canal por significado.
//!
//! Reproduce lo que hace sentence-transformers con `google/embeddinggemma-300m` (validado contra él
//! con coseno 1,00000 en float32): prompt + texto → tokenizer → Gemma 3 con atención bidireccional →
//! media de los tokens → dos capas densas (768 → 3072 → 768) → normalizar.
//!
//! Sin ONNX Runtime a propósito: el ONNX publicado usa operaciones propias de Microsoft
//! (`MultiHeadAttention`, `RotaryEmbedding`…) que solo entiende ONNX Runtime, y Omoikane no depende
//! de nada fuera del binario (PLAN.md, 9.2).
//!
//! Tarda unos 2 s en cargar y ~60 ms por consulta en CPU: no cabe en un hook que arranca en cada
//! mensaje. Lo carga un proceso que se queda abierto (`significado`).
//!
//! Lee dos formatos. El de sentence-transformers (1,2 GB en float32), para probar y para convertir.
//! Y el compacto, el que se instala (`compactar`): la tabla de 262.144 tokens, que son 805 de los
//! 1.200 MB, en int8 con una escala por fila y mapeada del disco (solo se leen las filas de los
//! tokens que salen); el resto de pesos, en float16 (se pasan a float32 al cargar).

use anyhow::{Context, Result, anyhow, ensure};
use candle_core::{D, DType, Device, Tensor};
use std::collections::HashMap;
use std::path::Path;

pub const DIM: usize = 768;
const PROMPT_PREGUNTA: &str = "task: search result | query: ";
const PROMPT_DOCUMENTO: &str = "title: none | text: ";
/// Lo que lee sentence-transformers (`max_seq_length`). Antes eran 512 sin máscara de ventana, y los
/// textos de más de 257 tokens daban otro vector que el de referencia (coseno 0,996 a 0,987 con él;
/// en una memoria real, 0,22 en vez de 0,26 con su pregunta, y otro orden). Con la máscara de
/// `vector`: 0,9992 con 376 tokens y 0,99997 con textos cortos.
const MAX_TOKENS: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tipo {
    Pregunta,
    Documento,
}

/// Ficheros del formato compacto.
const PESOS: &str = "pesos.safetensors";
const TABLA: &str = "tokens.i8";
const ESCALAS: &str = "tokens.escala";

/// La tabla de vectores de entrada de cada token.
enum Tabla {
    F32(Tensor),
    /// Fila `i`: `int8[i·ancho .. (i+1)·ancho] × escala[i]`.
    Int8 { filas: memmap2::Mmap, escalas: Vec<f32>, ancho: usize },
}

impl Tabla {
    fn filas(&self, ids: &[u32]) -> Result<Tensor> {
        match self {
            Tabla::F32(t) => Ok(t.index_select(&Tensor::new(ids, &Device::Cpu)?, 0)?),
            Tabla::Int8 { filas, escalas, ancho } => {
                let mut v = Vec::with_capacity(ids.len() * ancho);
                for &id in ids {
                    let i = id as usize;
                    ensure!(i < escalas.len(), "token {i} out of the table");
                    let e = escalas[i];
                    v.extend(filas[i * ancho..(i + 1) * ancho].iter().map(|&b| f32::from(b as i8) * e));
                }
                Ok(Tensor::from_vec(v, (ids.len(), *ancho), &Device::Cpu)?)
            }
        }
    }
}

struct Capa {
    norma_entrada: Tensor,
    norma_tras_atencion: Tensor,
    norma_antes_mlp: Tensor,
    norma_tras_mlp: Tensor,
    norma_q: Tensor,
    norma_k: Tensor,
    q: Tensor,
    k: Tensor,
    v: Tensor,
    o: Tensor,
    puerta: Tensor,
    subir: Tensor,
    bajar: Tensor,
    /// Capa de ventana deslizante: su RoPE usa la base local.
    local: bool,
}

pub struct Gemma {
    tokens: Tabla,
    capas: Vec<Capa>,
    norma: Tensor,
    densa1: Tensor,
    densa2: Tensor,
    tok: tokenizers::Tokenizer,
    eps: f64,
    cabezas: usize,
    cabezas_kv: usize,
    dim_cabeza: usize,
    escala: f64,
    escala_tokens: f64,
    base_local: f64,
    base_global: f64,
    ventana: usize,
}

/// ¿Tiene `dir` EmbeddingGemma, en el formato de sentence-transformers o en el compacto?
pub fn es_modelo(dir: &Path) -> bool {
    es_sentence_transformers(dir) || es_compacto(dir)
}

pub fn es_sentence_transformers(dir: &Path) -> bool {
    ["config.json", "model.safetensors", "tokenizer.json", "2_Dense/model.safetensors", "3_Dense/model.safetensors"].iter().all(|f| dir.join(f).is_file())
}

pub fn es_compacto(dir: &Path) -> bool {
    ["config.json", "tokenizer.json", PESOS, TABLA, ESCALAS].iter().all(|f| dir.join(f).is_file())
}

/// Los ficheros del formato compacto, en el orden en que se descargan.
pub const FICHEROS_COMPACTO: [&str; 5] = ["config.json", "tokenizer.json", ESCALAS, PESOS, TABLA];

/// Los pesos de sentence-transformers con los nombres del formato compacto: los del modelo tal cual
/// y las dos capas densas como `densa1.weight` y `densa2.weight`.
fn pesos_sentence_transformers(dir: &Path) -> Result<HashMap<String, Tensor>> {
    let dev = Device::Cpu;
    let mut pesos = candle_core::safetensors::load(dir.join("model.safetensors"), &dev)?;
    for (d, nombre) in [("2_Dense", "densa1.weight"), ("3_Dense", "densa2.weight")] {
        let mut t = candle_core::safetensors::load(dir.join(d).join("model.safetensors"), &dev)?;
        pesos.insert(nombre.into(), t.remove("linear.weight").ok_or_else(|| anyhow!("{d}: missing linear.weight"))?);
    }
    Ok(pesos)
}

/// Escribe en `destino` el formato compacto a partir de la carpeta de sentence-transformers `origen`:
/// `config.json` y `tokenizer.json` tal cual, la tabla de tokens en int8 (escala por fila, simétrica)
/// y el resto en float16. Cada fichero se escribe como `.part` y se renombra al terminar.
pub fn compactar(origen: &Path, destino: &Path) -> Result<()> {
    ensure!(es_sentence_transformers(origen), "{}: not a sentence-transformers EmbeddingGemma folder", origen.display());
    std::fs::create_dir_all(destino)?;
    let mut pesos = pesos_sentence_transformers(origen)?;
    let tabla = pesos.remove("embed_tokens.weight").ok_or_else(|| anyhow!("model.safetensors: missing embed_tokens.weight"))?.to_dtype(DType::F32)?;
    let (n, ancho) = tabla.dims2()?;
    let datos = tabla.flatten_all()?.to_vec1::<f32>()?;
    drop(tabla);
    let mut filas = Vec::with_capacity(n * ancho);
    let mut escalas = Vec::with_capacity(n * 4);
    for fila in datos.chunks_exact(ancho) {
        let max = fila.iter().fold(0f32, |m, x| m.max(x.abs()));
        let e = if max > 0.0 { max / 127.0 } else { 1.0 };
        filas.extend(fila.iter().map(|x| (x / e).round().clamp(-127.0, 127.0) as i8 as u8));
        escalas.extend_from_slice(&e.to_le_bytes());
    }
    drop(datos);
    let resto: HashMap<String, Tensor> = pesos.into_iter().map(|(k, v)| Ok((k, v.to_dtype(DType::F16)?))).collect::<Result<_>>()?;
    let escribir = |nombre: &str, f: &dyn Fn(&Path) -> Result<()>| -> Result<()> {
        let parte = destino.join(format!("{nombre}.part"));
        f(&parte)?;
        std::fs::rename(&parte, destino.join(nombre))?;
        Ok(())
    };
    escribir(TABLA, &|p| Ok(std::fs::write(p, &filas)?))?;
    escribir(ESCALAS, &|p| Ok(std::fs::write(p, &escalas)?))?;
    escribir(PESOS, &|p| Ok(candle_core::safetensors::save(&resto, p)?))?;
    for f in ["config.json", "tokenizer.json"] {
        escribir(f, &|p| Ok(std::fs::copy(origen.join(f), p).map(|_| ())?))?;
    }
    Ok(())
}

/// `x / rms(x) * (1 + w)`: la RMSNorm de Gemma guarda el peso desplazado en 1.
fn rms(x: &Tensor, w: &Tensor, eps: f64) -> Result<Tensor> {
    let v = x.sqr()?.mean_keepdim(D::Minus1)?;
    Ok(x.broadcast_div(&(v + eps)?.sqrt()?)?.broadcast_mul(&(w + 1.0)?)?)
}

/// `x · Wᵀ` para `x` de 2 o 3 dimensiones.
fn lineal(x: &Tensor, w: &Tensor) -> Result<Tensor> {
    Ok(x.broadcast_matmul(&w.t()?)?)
}

impl Gemma {
    pub fn cargar(dir: &Path) -> Result<Self> {
        ensure!(es_modelo(dir), "{}: not an EmbeddingGemma folder", dir.display());
        let dev = Device::Cpu;
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let num = |k: &str| cfg[k].as_f64().ok_or_else(|| anyhow!("config.json: missing {k}"));
        let compacto = !es_sentence_transformers(dir);
        let mut pesos = if compacto { candle_core::safetensors::load(dir.join(PESOS), &dev)? } else { pesos_sentence_transformers(dir)? };
        let mut sacar = |k: String| -> Result<Tensor> { Ok(pesos.remove(&k).ok_or_else(|| anyhow!("model.safetensors: missing {k}"))?.to_dtype(DType::F32)?) };
        let tipos = cfg["layer_types"].as_array().context("config.json: missing layer_types")?;
        let mut capas = Vec::with_capacity(tipos.len());
        for (i, tipo) in tipos.iter().enumerate() {
            let p = |s: &str| format!("layers.{i}.{s}.weight");
            capas.push(Capa {
                norma_entrada: sacar(p("input_layernorm"))?,
                norma_tras_atencion: sacar(p("post_attention_layernorm"))?,
                norma_antes_mlp: sacar(p("pre_feedforward_layernorm"))?,
                norma_tras_mlp: sacar(p("post_feedforward_layernorm"))?,
                norma_q: sacar(p("self_attn.q_norm"))?,
                norma_k: sacar(p("self_attn.k_norm"))?,
                q: sacar(p("self_attn.q_proj"))?,
                k: sacar(p("self_attn.k_proj"))?,
                v: sacar(p("self_attn.v_proj"))?,
                o: sacar(p("self_attn.o_proj"))?,
                puerta: sacar(p("mlp.gate_proj"))?,
                subir: sacar(p("mlp.up_proj"))?,
                bajar: sacar(p("mlp.down_proj"))?,
                local: tipo.as_str() == Some("sliding_attention"),
            });
        }
        let tokens = if compacto {
            let ancho = num("hidden_size")? as usize;
            // SAFETY: el fichero no se toca después de instalarlo (`modelos` instala en otra carpeta).
            let filas = unsafe { memmap2::Mmap::map(&std::fs::File::open(dir.join(TABLA))?)? };
            let escalas: Vec<f32> = std::fs::read(dir.join(ESCALAS))?.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            ensure!(filas.len() == escalas.len() * ancho, "{TABLA}: {} bytes for {} rows of {ancho}", filas.len(), escalas.len());
            Tabla::Int8 { filas, escalas, ancho }
        } else {
            Tabla::F32(sacar("embed_tokens.weight".into())?)
        };
        let norma = sacar("norm.weight".into())?;
        let densa1 = sacar("densa1.weight".into())?;
        let densa2 = sacar("densa2.weight".into())?;
        let mut tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("tokenizer.json: {e}"))?;
        tok.with_truncation(Some(tokenizers::TruncationParams { max_length: MAX_TOKENS, ..Default::default() })).map_err(|e| anyhow!("{e}"))?;
        tok.with_padding(None);
        let ancho = num("hidden_size")?;
        Ok(Gemma {
            tokens,
            capas,
            norma,
            densa1,
            densa2,
            tok,
            eps: num("rms_norm_eps")?,
            cabezas: num("num_attention_heads")? as usize,
            cabezas_kv: num("num_key_value_heads")? as usize,
            dim_cabeza: num("head_dim")? as usize,
            escala: num("query_pre_attn_scalar")?.powf(-0.5),
            escala_tokens: ancho.sqrt(),
            base_local: num("rope_local_base_freq")?,
            base_global: num("rope_theta")?,
            ventana: num("sliding_window")? as usize,
        })
    }

    /// Cosenos y senos de RoPE para `n` posiciones con base `base`.
    fn rope(&self, n: usize, base: f64) -> Result<(Tensor, Tensor)> {
        let d = self.dim_cabeza;
        let inv: Vec<f32> = (0..d / 2).map(|i| (1.0 / base.powf(2.0 * i as f64 / d as f64)) as f32).collect();
        let inv = Tensor::new(inv, &Device::Cpu)?.unsqueeze(0)?;
        let pos = Tensor::arange(0u32, n as u32, &Device::Cpu)?.to_dtype(DType::F32)?.unsqueeze(1)?;
        let f = pos.broadcast_mul(&inv)?;
        Ok((f.cos()?, f.sin()?))
    }

    /// El vector (768, normalizado) de `texto`, con el prompt de su tipo.
    pub fn vector(&self, texto: &str, tipo: Tipo) -> Result<Vec<f32>> {
        let prompt = match tipo {
            Tipo::Pregunta => PROMPT_PREGUNTA,
            Tipo::Documento => PROMPT_DOCUMENTO,
        };
        let enc = self.tok.encode(format!("{prompt}{texto}"), true).map_err(|e| anyhow!("{e}"))?;
        let n = enc.get_ids().len();
        let (h, hd, kv) = (self.cabezas, self.dim_cabeza, self.cabezas_kv);
        let mut x = (self.tokens.filas(enc.get_ids())? * self.escala_tokens)?.unsqueeze(0)?;
        let local = self.rope(n, self.base_local)?;
        let global = self.rope(n, self.base_global)?;
        // Máscara de las capas locales: cada token ve a los que están a media ventana o menos, a cada
        // lado (lo que hace transformers, comprobado con sus pesos de atención: con ventana 512, el
        // token 300 ve del 44 al 556). Solo hace falta si el texto pasa de esa distancia.
        let medio = self.ventana / 2;
        let mascara = if n > medio + 1 {
            let w = medio as i64;
            let m: Vec<f32> = (0..n as i64).flat_map(|i| (0..n as i64).map(move |j| if (i - j).abs() <= w { 0.0 } else { f32::NEG_INFINITY })).collect();
            Some(Tensor::from_vec(m, (1, 1, n, n), &Device::Cpu)?)
        } else {
            None
        };
        for c in &self.capas {
            let e = rms(&x, &c.norma_entrada, self.eps)?;
            let q = rms(&lineal(&e, &c.q)?.reshape((1, n, h, hd))?, &c.norma_q, self.eps)?.transpose(1, 2)?.contiguous()?;
            let k = rms(&lineal(&e, &c.k)?.reshape((1, n, kv, hd))?, &c.norma_k, self.eps)?.transpose(1, 2)?.contiguous()?;
            let v = lineal(&e, &c.v)?.reshape((1, n, kv, hd))?.transpose(1, 2)?.contiguous()?;
            let (cos, sin) = if c.local { &local } else { &global };
            let q = candle_nn::rotary_emb::rope(&q, cos, sin)?;
            let k = candle_nn::rotary_emb::rope(&k, cos, sin)?.repeat((1, h / kv, 1, 1))?;
            let v = v.repeat((1, h / kv, 1, 1))?;
            // Bidireccional y sin relleno (un texto por llamada): solo la máscara de ventana.
            let mut puntos = (q.matmul(&k.t()?)? * self.escala)?;
            if c.local
                && let Some(m) = &mascara
            {
                puntos = puntos.broadcast_add(m)?;
            }
            let a = candle_nn::ops::softmax_last_dim(&puntos)?;
            let a = a.matmul(&v)?.transpose(1, 2)?.reshape((1, n, h * hd))?;
            x = (x + rms(&lineal(&a, &c.o)?, &c.norma_tras_atencion, self.eps)?)?;
            let e = rms(&x, &c.norma_antes_mlp, self.eps)?;
            let m = lineal(&(lineal(&e, &c.puerta)?.gelu()? * lineal(&e, &c.subir)?)?, &c.bajar)?;
            x = (x + rms(&m, &c.norma_tras_mlp, self.eps)?)?;
        }
        let x = rms(&x, &self.norma, self.eps)?.mean(1)?;
        let x = lineal(&lineal(&x, &self.densa1)?, &self.densa2)?;
        let x = x.broadcast_div(&x.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
        Ok(x.squeeze(0)?.to_vec1()?)
    }
}
