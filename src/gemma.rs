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

use anyhow::{Context, Result, anyhow, ensure};
use candle_core::{D, DType, Device, Tensor};
use std::path::Path;

pub const DIM: usize = 768;
const PROMPT_PREGUNTA: &str = "task: search result | query: ";
const PROMPT_DOCUMENTO: &str = "title: none | text: ";
/// Hasta aquí, la ventana deslizante (512) de las capas locales cubre todo el texto y todas las capas
/// atienden a todo: no hace falta la máscara de ventana.
const MAX_TOKENS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tipo {
    Pregunta,
    Documento,
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
    tokens: Tensor,
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
}

/// ¿Tiene `dir` los ficheros de EmbeddingGemma tal como los deja sentence-transformers?
pub fn es_modelo(dir: &Path) -> bool {
    ["config.json", "model.safetensors", "tokenizer.json", "2_Dense/model.safetensors", "3_Dense/model.safetensors"].iter().all(|f| dir.join(f).is_file())
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
        let mut pesos = candle_core::safetensors::load(dir.join("model.safetensors"), &dev)?;
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
        let tokens = sacar("embed_tokens.weight".into())?;
        let norma = sacar("norm.weight".into())?;
        let densa = |d: &str| -> Result<Tensor> {
            let mut t = candle_core::safetensors::load(dir.join(d).join("model.safetensors"), &dev)?;
            Ok(t.remove("linear.weight").ok_or_else(|| anyhow!("{d}: missing linear.weight"))?.to_dtype(DType::F32)?)
        };
        let mut tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("tokenizer.json: {e}"))?;
        tok.with_truncation(Some(tokenizers::TruncationParams { max_length: MAX_TOKENS, ..Default::default() })).map_err(|e| anyhow!("{e}"))?;
        tok.with_padding(None);
        let ancho = num("hidden_size")?;
        Ok(Gemma {
            tokens,
            capas,
            norma,
            densa1: densa("2_Dense")?,
            densa2: densa("3_Dense")?,
            tok,
            eps: num("rms_norm_eps")?,
            cabezas: num("num_attention_heads")? as usize,
            cabezas_kv: num("num_key_value_heads")? as usize,
            dim_cabeza: num("head_dim")? as usize,
            escala: num("query_pre_attn_scalar")?.powf(-0.5),
            escala_tokens: ancho.sqrt(),
            base_local: num("rope_local_base_freq")?,
            base_global: num("rope_theta")?,
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
        let ids = Tensor::new(enc.get_ids(), &Device::Cpu)?;
        let n = enc.get_ids().len();
        let (h, hd, kv) = (self.cabezas, self.dim_cabeza, self.cabezas_kv);
        let mut x = (self.tokens.index_select(&ids, 0)? * self.escala_tokens)?.unsqueeze(0)?;
        let local = self.rope(n, self.base_local)?;
        let global = self.rope(n, self.base_global)?;
        for c in &self.capas {
            let e = rms(&x, &c.norma_entrada, self.eps)?;
            let q = rms(&lineal(&e, &c.q)?.reshape((1, n, h, hd))?, &c.norma_q, self.eps)?.transpose(1, 2)?.contiguous()?;
            let k = rms(&lineal(&e, &c.k)?.reshape((1, n, kv, hd))?, &c.norma_k, self.eps)?.transpose(1, 2)?.contiguous()?;
            let v = lineal(&e, &c.v)?.reshape((1, n, kv, hd))?.transpose(1, 2)?.contiguous()?;
            let (cos, sin) = if c.local { &local } else { &global };
            let q = candle_nn::rotary_emb::rope(&q, cos, sin)?;
            let k = candle_nn::rotary_emb::rope(&k, cos, sin)?.repeat((1, h / kv, 1, 1))?;
            let v = v.repeat((1, h / kv, 1, 1))?;
            // Bidireccional y sin relleno (un texto por llamada): sin máscara.
            let a = candle_nn::ops::softmax_last_dim(&(q.matmul(&k.t()?)? * self.escala)?)?;
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
