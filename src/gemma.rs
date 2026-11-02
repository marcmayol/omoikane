//! EmbeddingGemma 300M en Rust puro, para el canal por significado.
//!
//! Reproduce lo que hace sentence-transformers con `google/embeddinggemma-300m` (validado contra él
//! con coseno 1,00000 en float32): prompt + texto → tokenizer → Gemma 3 con atención bidireccional →
//! media de los tokens → dos capas densas (768 → 3072 → 768) → normalizar.
//!
//! Sin ONNX Runtime a propósito: el ONNX publicado usa operaciones propias de Microsoft
//! (`MultiHeadAttention`, `RotaryEmbedding`…) que solo entiende ONNX Runtime, y Omoikane no depende
//! de nada fuera del binario.
//!
//! Dos motores dan el mismo vector:
//! - `rapido` (por defecto): pesos en f16 leídos del fichero mapeado, multiplicaciones propias con
//!   AVX2 (o el camino genérico sin él). Carga en ~0,1 s y ~30 ms por pregunta en 8 hilos.
//! - candle (`OMOIKANE_GEMMA_RAPIDO=0`, y el que se compara en los tests): pesos en f32 en RAM.
//!
//! Y dos tokenizers que dan los mismos ids: el compilado (`bpe`, se mapea en milisegundos) y, si no
//! se puede compilar, `tokenizers` (1,3-1,6 s en leer el `tokenizer.json`).
//!
//! Lee dos formatos de modelo. El de sentence-transformers (1,2 GB en float32), para probar y para
//! convertir. Y el compacto, el que se instala (`compactar`): la tabla de 262.144 tokens, que son
//! 805 de los 1.200 MB, en int8 con una escala por fila y mapeada del disco (solo se leen las filas de
//! los tokens que salen); el resto de pesos, en float16.

use anyhow::{Context, Result, anyhow, ensure};
use candle_core::{D, DType, Device, Tensor};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub const DIM: usize = 768;
const PROMPT_PREGUNTA: &str = "task: search result | query: ";
const PROMPT_DOCUMENTO: &str = "title: none | text: ";
/// Lo que lee sentence-transformers (`max_seq_length`). Antes eran 512 sin máscara de ventana, y los
/// textos de más de 257 tokens daban otro vector que el de referencia (coseno 0,996 a 0,987 con él;
/// en una memoria real, 0,22 en vez de 0,26 con su pregunta, y otro orden). Con la máscara de
/// `vector`: 0,9992 con 376 tokens y 0,99997 con textos cortos.
pub const MAX_TOKENS: usize = 2048;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tipo {
    Pregunta,
    Documento,
}

impl Tipo {
    fn prompt(self) -> &'static str {
        match self {
            Tipo::Pregunta => PROMPT_PREGUNTA,
            Tipo::Documento => PROMPT_DOCUMENTO,
        }
    }
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
            Tabla::Int8 { ancho, .. } => Ok(Tensor::from_vec(self.filas_vec(ids)?, (ids.len(), *ancho), &Device::Cpu)?),
        }
    }

    fn filas_vec(&self, ids: &[u32]) -> Result<Vec<f32>> {
        match self {
            Tabla::F32(_) => Ok(self.filas(ids)?.flatten_all()?.to_vec1::<f32>()?),
            Tabla::Int8 { filas, escalas, ancho } => {
                let mut v = Vec::with_capacity(ids.len() * ancho);
                for &id in ids {
                    // `<image_soft_token>` (262.144) es un token añadido que no está en la tabla: escrito
                    // tal cual en una memoria, sentence-transformers fallaría. Aquí cuenta como `<unk>`
                    // (3), para que esa memoria no pare el indexado de las demás.
                    let i = if (id as usize) < escalas.len() { id as usize } else { 3 };
                    let e = escalas[i];
                    v.extend(filas[i * ancho..(i + 1) * ancho].iter().map(|&b| f32::from(b as i8) * e));
                }
                Ok(v)
            }
        }
    }
}

/// El tokenizer: el compilado (`bpe`) o, si no se pudo, el de `tokenizers`. Hay uno por modelo
/// cargado: lo que ocupe la variante grande da igual.
#[allow(clippy::large_enum_variant)]
enum Tok {
    Bpe(crate::bpe::Bpe),
    Hf(Box<tokenizers::Tokenizer>),
}

impl Tok {
    /// El compilado si la carpeta es del formato compacto (lo compila la primera vez y lo deja al
    /// lado; `OMOIKANE_GEMMA_BPE=0` no lo usa). En una de sentence-transformers no escribe nada.
    fn cargar(dir: &Path, compacto: bool) -> Result<Tok> {
        let json = dir.join("tokenizer.json");
        if compacto && std::env::var("OMOIKANE_GEMMA_BPE").as_deref() != Ok("0") {
            match crate::bpe::Bpe::abrir_o_compilar(&json, &dir.join(crate::bpe::FICHERO)) {
                Ok(b) => return Ok(Tok::Bpe(b)),
                Err(e) => eprintln!("omoi: {}: {e:#}; using tokenizer.json", crate::bpe::FICHERO),
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

    fn ids(&self, texto: &str) -> Result<Vec<u32>> {
        match self {
            Tok::Bpe(b) => Ok(b.codificar(texto, MAX_TOKENS)),
            Tok::Hf(t) => Ok(t.encode(texto, true).map_err(|e| anyhow!("{e}"))?.get_ids().to_vec()),
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

/// El modelo en candle, en f32.
struct Candle {
    capas: Vec<Capa>,
    norma: Tensor,
    densa1: Tensor,
    densa2: Tensor,
}

/// Uno por modelo cargado: lo que ocupe la variante grande da igual.
#[allow(clippy::large_enum_variant)]
enum Motor {
    Rapido(crate::rapido::Rapido, rayon::ThreadPool),
    Candle(Candle),
}

pub struct Gemma {
    /// `pesos.safetensors` mapeado, si el motor rápido lee de él (para `precalentar`).
    mapa: Option<Arc<memmap2::Mmap>>,
    tokens: Tabla,
    tok: Tok,
    motor: Motor,
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

/// Lo que se lee de `config.json`.
struct Cfg {
    eps: f64,
    cabezas: usize,
    cabezas_kv: usize,
    dim_cabeza: usize,
    escala: f64,
    escala_tokens: f64,
    base_local: f64,
    base_global: f64,
    ventana: usize,
    ancho: usize,
    /// Por capa: ¿es de ventana deslizante?
    locales: Vec<bool>,
}

impl Cfg {
    fn leer(dir: &Path) -> Result<Cfg> {
        let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let num = |k: &str| cfg[k].as_f64().ok_or_else(|| anyhow!("config.json: missing {k}"));
        let tipos = cfg["layer_types"].as_array().context("config.json: missing layer_types")?;
        let ancho = num("hidden_size")?;
        Ok(Cfg {
            eps: num("rms_norm_eps")?,
            cabezas: num("num_attention_heads")? as usize,
            cabezas_kv: num("num_key_value_heads")? as usize,
            dim_cabeza: num("head_dim")? as usize,
            escala: num("query_pre_attn_scalar")?.powf(-0.5),
            escala_tokens: ancho.sqrt(),
            base_local: num("rope_local_base_freq")?,
            base_global: num("rope_theta")?,
            ventana: num("sliding_window")? as usize,
            ancho: ancho as usize,
            locales: tipos.iter().map(|t| t.as_str() == Some("sliding_attention")).collect(),
        })
    }
}

/// De dónde saca `rapido` los pesos: `pesos.safetensors` mapeado (el formato compacto, en f16: las
/// matrices se usan ahí mismo, sin copiarlas) o tensores ya cargados (sentence-transformers).
enum Fuente {
    Mapa { mapa: Arc<memmap2::Mmap>, tensores: HashMap<String, (usize, Vec<usize>, safetensors::Dtype)> },
    Tensores(HashMap<String, Tensor>),
}

impl Fuente {
    fn mapa(ruta: &Path) -> Result<Fuente> {
        // SAFETY: el fichero no se toca después de instalarlo (`modelos` instala en otra carpeta).
        let mapa = Arc::new(unsafe { memmap2::Mmap::map(&std::fs::File::open(ruta)?)? });
        let st = safetensors::SafeTensors::deserialize(&mapa[..])?;
        let base = mapa.as_ptr() as usize;
        let tensores = st.tensors().into_iter().map(|(n, v)| (n, (v.data().as_ptr() as usize - base, v.shape().to_vec(), v.dtype()))).collect();
        Ok(Fuente::Mapa { mapa, tensores })
    }

    fn f32s(&mut self, k: &str) -> Result<(Vec<f32>, Vec<usize>)> {
        match self {
            Fuente::Mapa { mapa, tensores } => {
                let (ini, forma, tipo) = tensores.get(k).with_context(|| format!("{PESOS}: missing {k}"))?;
                let n: usize = forma.iter().product();
                let b = &mapa[*ini..];
                let v = match tipo {
                    safetensors::Dtype::F16 => b[..2 * n].as_chunks::<2>().0.iter().map(|x| half::f16::from_le_bytes(*x).to_f32()).collect(),
                    safetensors::Dtype::F32 => b[..4 * n].as_chunks::<4>().0.iter().map(|x| f32::from_le_bytes(*x)).collect(),
                    t => anyhow::bail!("{PESOS}: {k} is {t:?}"),
                };
                Ok((v, forma.clone()))
            }
            Fuente::Tensores(t) => {
                let t = t.remove(k).with_context(|| format!("model.safetensors: missing {k}"))?.to_dtype(DType::F32)?;
                Ok((t.flatten_all()?.to_vec1::<f32>()?, t.dims().to_vec()))
            }
        }
    }

    /// Una norma de Gemma con el `1 +` ya sumado.
    fn norma(&mut self, k: &str) -> Result<Vec<f32>> {
        Ok(self.f32s(k)?.0.into_iter().map(|x| x + 1.0).collect())
    }

    /// Las matrices `ks` una tras otra. Del fichero mapeado en f16, sin copiarlas.
    fn mat(&mut self, ks: &[String], simd: bool) -> Result<crate::rapido::Mat> {
        use crate::rapido::Mat;
        if let Fuente::Mapa { mapa, tensores } = self
            && ks.iter().all(|k| tensores.get(k).is_some_and(|t| t.2 == safetensors::Dtype::F16 && t.1.len() == 2 && t.0 % 2 == 0))
        {
            let cols = tensores[&ks[0]].1[1];
            let mut partes = Vec::new();
            for k in ks {
                let (ini, forma, _) = &tensores[k];
                ensure!(forma[1] == cols, "{PESOS}: {k} has {} columns, not {cols}", forma[1]);
                // SAFETY: dentro del fichero (lo comprobó `deserialize`), par y en f16.
                partes.push((unsafe { mapa.as_ptr().add(*ini) } as *const u16, forma[0], mapa.clone() as Arc<dyn Send + Sync>));
            }
            // SAFETY: cada parte son filas·cols f16 del mapa, que va como dueño y no cambia.
            return Ok(unsafe { Mat::de_partes(partes, cols, simd) });
        }
        let mut datos = Vec::new();
        let (mut filas, mut cols) = (0, 0);
        for k in ks {
            let (v, forma) = self.f32s(k)?;
            ensure!(forma.len() == 2, "{k}: not a matrix");
            filas += forma[0];
            cols = forma[1];
            datos.extend(v);
        }
        Ok(Mat::de_f32(&datos, filas, cols, simd))
    }
}

impl Gemma {
    /// Con el motor rápido (`rapido`) si la CPU tiene AVX2/FMA/F16C; si no (CPU antiguas, ARM), con
    /// candle: el camino genérico del rápido da el mismo vector pero es más lento que candle (medido:
    /// 56 frente a 42 ms con 16 tokens, el doble con textos largos). `OMOIKANE_GEMMA_RAPIDO=0` fuerza
    /// candle y `=1` el rápido (genérico, si no hay AVX2).
    pub fn cargar(dir: &Path) -> Result<Self> {
        let simd = crate::rapido::simd_disponible();
        match std::env::var("OMOIKANE_GEMMA_RAPIDO").as_deref() {
            Ok("0") => Self::cargar_candle(dir),
            Ok("1") => Self::cargar_rapido(dir, Self::hilos_por_defecto(), simd),
            _ if simd => Self::cargar_rapido(dir, Self::hilos_por_defecto(), true),
            _ => Self::cargar_candle(dir),
        }
    }

    /// Lee una vez los pesos mapeados, para que la primera consulta no pague los fallos de página
    /// (~30 ms con el disco en caché). El servicio lo llama en un hilo aparte nada más cargar.
    pub fn precalentar(&self) {
        if let Some(m) = &self.mapa {
            let mut s = 0u8;
            for i in (0..m.len()).step_by(4096) {
                s = s.wrapping_add(m[i]);
            }
            std::hint::black_box(s);
        }
    }

    /// Hilos del motor rápido: `OMOIKANE_GEMMA_HILOS` o los de la CPU.
    pub fn hilos_por_defecto() -> usize {
        std::env::var("OMOIKANE_GEMMA_HILOS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &usize| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
    }

    /// ¿Va con el motor rápido?
    pub fn es_rapido(&self) -> bool {
        matches!(self.motor, Motor::Rapido(..))
    }

    /// ¿Usa el tokenizer compilado?
    pub fn tokenizer_compilado(&self) -> bool {
        matches!(self.tok, Tok::Bpe(_))
    }

    fn con_motor(dir: &Path, cfg: Cfg, tokens: Tabla, compacto: bool, motor: Motor) -> Result<Self> {
        Ok(Gemma {
            mapa: None,
            tokens,
            tok: Tok::cargar(dir, compacto)?,
            motor,
            eps: cfg.eps,
            cabezas: cfg.cabezas,
            cabezas_kv: cfg.cabezas_kv,
            dim_cabeza: cfg.dim_cabeza,
            escala: cfg.escala,
            escala_tokens: cfg.escala_tokens,
            base_local: cfg.base_local,
            base_global: cfg.base_global,
            ventana: cfg.ventana,
        })
    }

    fn tabla_int8(dir: &Path, ancho: usize) -> Result<Tabla> {
        // SAFETY: el fichero no se toca después de instalarlo (`modelos` instala en otra carpeta).
        let filas = unsafe { memmap2::Mmap::map(&std::fs::File::open(dir.join(TABLA))?)? };
        let escalas: Vec<f32> = std::fs::read(dir.join(ESCALAS))?.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        ensure!(filas.len() == escalas.len() * ancho, "{TABLA}: {} bytes for {} rows of {ancho}", filas.len(), escalas.len());
        Ok(Tabla::Int8 { filas, escalas, ancho })
    }

    /// El motor rápido con `hilos` hilos; `simd` con AVX2/FMA/F16C (`rapido::simd_disponible`).
    pub fn cargar_rapido(dir: &Path, hilos: usize, simd: bool) -> Result<Self> {
        use crate::rapido::{Capa as C, Rapido, tabla_rope};
        ensure!(es_modelo(dir), "{}: not an EmbeddingGemma folder", dir.display());
        let cfg = Cfg::leer(dir)?;
        let compacto = !es_sentence_transformers(dir);
        let mut f = if compacto { Fuente::mapa(&dir.join(PESOS))? } else { Fuente::Tensores(pesos_sentence_transformers(dir)?) };
        let mut capas = Vec::with_capacity(cfg.locales.len());
        for (i, &local) in cfg.locales.iter().enumerate() {
            let p = |s: &str| format!("layers.{i}.{s}.weight");
            capas.push(C {
                ne: f.norma(&p("input_layernorm"))?,
                nta: f.norma(&p("post_attention_layernorm"))?,
                nam: f.norma(&p("pre_feedforward_layernorm"))?,
                ntm: f.norma(&p("post_feedforward_layernorm"))?,
                nq: f.norma(&p("self_attn.q_norm"))?,
                nk: f.norma(&p("self_attn.k_norm"))?,
                qkv: f.mat(&[p("self_attn.q_proj"), p("self_attn.k_proj"), p("self_attn.v_proj")], simd)?,
                o: f.mat(&[p("self_attn.o_proj")], simd)?,
                gu: f.mat(&[p("mlp.gate_proj"), p("mlp.up_proj")], simd)?,
                bajar: f.mat(&[p("mlp.down_proj")], simd)?,
                local,
            });
        }
        let tokens = match &mut f {
            Fuente::Mapa { .. } => Self::tabla_int8(dir, cfg.ancho)?,
            Fuente::Tensores(t) => Tabla::F32(t.remove("embed_tokens.weight").context("model.safetensors: missing embed_tokens.weight")?.to_dtype(DType::F32)?),
        };
        let r = Rapido {
            capas,
            norma: f.norma("norm.weight")?,
            densa1: f.mat(&["densa1.weight".into()], simd)?,
            densa2: f.mat(&["densa2.weight".into()], simd)?,
            eps: cfg.eps as f32,
            cabezas: cfg.cabezas,
            cabezas_kv: cfg.cabezas_kv,
            dim_cabeza: cfg.dim_cabeza,
            escala: cfg.escala as f32,
            medio: cfg.ventana / 2,
            rope_local: tabla_rope(MAX_TOKENS, cfg.dim_cabeza, cfg.base_local),
            rope_global: tabla_rope(MAX_TOKENS, cfg.dim_cabeza, cfg.base_global),
            simd,
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(hilos.max(1)).thread_name(|i| format!("gemma-{i}")).build()?;
        let mapa = match f {
            Fuente::Mapa { mapa, .. } => Some(mapa),
            Fuente::Tensores(_) => None,
        };
        let mut g = Self::con_motor(dir, cfg, tokens, compacto, Motor::Rapido(r, pool))?;
        g.mapa = mapa;
        Ok(g)
    }

    /// Con candle, en f32 (como hasta la 1.2.0).
    pub fn cargar_candle(dir: &Path) -> Result<Self> {
        ensure!(es_modelo(dir), "{}: not an EmbeddingGemma folder", dir.display());
        let dev = Device::Cpu;
        let cfg = Cfg::leer(dir)?;
        let compacto = !es_sentence_transformers(dir);
        let mut pesos = if compacto { candle_core::safetensors::load(dir.join(PESOS), &dev)? } else { pesos_sentence_transformers(dir)? };
        let mut sacar = |k: String| -> Result<Tensor> { Ok(pesos.remove(&k).ok_or_else(|| anyhow!("model.safetensors: missing {k}"))?.to_dtype(DType::F32)?) };
        let mut capas = Vec::with_capacity(cfg.locales.len());
        for (i, &local) in cfg.locales.iter().enumerate() {
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
                local,
            });
        }
        let tokens = if compacto { Self::tabla_int8(dir, cfg.ancho)? } else { Tabla::F32(sacar("embed_tokens.weight".into())?) };
        let c = Candle { capas, norma: sacar("norm.weight".into())?, densa1: sacar("densa1.weight".into())?, densa2: sacar("densa2.weight".into())? };
        Self::con_motor(dir, cfg, tokens, compacto, Motor::Candle(c))
    }

    /// Los ids de `texto` con el prompt de su tipo (con `<bos>` y `<eos>`, hasta `MAX_TOKENS`).
    pub fn ids(&self, texto: &str, tipo: Tipo) -> Result<Vec<u32>> {
        self.tok.ids(&format!("{}{texto}", tipo.prompt()))
    }

    /// El vector (768, normalizado) de `texto`, con el prompt de su tipo.
    pub fn vector(&self, texto: &str, tipo: Tipo) -> Result<Vec<f32>> {
        let ids = self.ids(texto, tipo)?;
        Ok(self.vectores_de_ids(&[ids], &|| {})?.pop().unwrap_or_default())
    }

    /// Los vectores de varios textos (con el motor rápido, de una vez: el mismo resultado que de
    /// uno en uno, más rápido).
    pub fn vectores(&self, textos: &[&str], tipo: Tipo) -> Result<Vec<Vec<f32>>> {
        let ids = textos.iter().map(|t| self.ids(t, tipo)).collect::<Result<Vec<_>>>()?;
        self.vectores_de_ids(&ids, &|| {})
    }

    /// Los vectores de textos ya tokenizados (`ids`). `ceder` se llama entre paso y paso: quien
    /// indexa lo usa para dejar pasar antes una consulta.
    pub fn vectores_de_ids(&self, ids: &[Vec<u32>], ceder: &dyn Fn()) -> Result<Vec<Vec<f32>>> {
        match &self.motor {
            Motor::Rapido(r, pool) => {
                let todos: Vec<u32> = ids.iter().flatten().copied().collect();
                let e = self.escala_tokens as f32;
                let x = self.tokens.filas_vec(&todos)?.into_iter().map(|v| v * e).collect();
                let largos: Vec<usize> = ids.iter().map(Vec::len).collect();
                Ok(r.vectores(x, &largos, pool, ceder))
            }
            Motor::Candle(c) => ids
                .iter()
                .map(|i| {
                    ceder();
                    self.vector_candle(c, i)
                })
                .collect(),
        }
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

    fn vector_candle(&self, c: &Candle, ids: &[u32]) -> Result<Vec<f32>> {
        let n = ids.len();
        let (h, hd, kv) = (self.cabezas, self.dim_cabeza, self.cabezas_kv);
        let mut x = (self.tokens.filas(ids)? * self.escala_tokens)?.unsqueeze(0)?;
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
        for c in &c.capas {
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
        let x = rms(&x, &c.norma, self.eps)?.mean(1)?;
        let x = lineal(&lineal(&x, &c.densa1)?, &c.densa2)?;
        let x = x.broadcast_div(&x.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)?;
        Ok(x.squeeze(0)?.to_vec1()?)
    }
}
