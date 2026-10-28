//! Embeddings estáticos (model2vec) que cargan en milisegundos.
//!
//! `tokenizers` tarda 1,3 s en construir el vocabulario Unigram de 500.000 piezas cada vez que se
//! carga. Aquí ese trabajo se hace una vez (`compilar`) y se guarda en ficheros planos que luego solo
//! se mapean en memoria (`Modelo::abrir`): el trie de doble array, las puntuaciones, la tabla int8 y
//! los pesos por token. Del `tokenizer.json` se queda solo el normalizador y el pre-tokenizer, sin
//! vocabulario, que carga en 3 ms.
//!
//! El troceado (Viterbi de Unigram) y el trie reproducen `fast_unigram.rs` y `trie.rs` de
//! model2vec-rs (MIT, © The Minish Lab), que a su vez reproducen `Unigram::encode_optimized` de
//! `tokenizers`: mismos desempates, misma penalización de desconocidos y misma fusión de rachas
//! desconocidas. La equivalencia se comprueba contra `tokenizers` y contra model2vec-rs.

use crate::tr;
use anyhow::{Context, Result, anyhow, ensure};
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use tokenizers::models::ModelWrapper;
use tokenizers::{Model, OffsetReferential, OffsetType, PreTokenizer, Tokenizer};

// Los ficheros del modelo se escriben y se leen en little-endian (x86-64 y ARM64, Apple Silicon
// incluido). En una máquina big-endian saldrían vectores basura sin avisar: mejor no compilar.
#[cfg(target_endian = "big")]
compile_error!("omoikane only works on little-endian platforms");

const NONE: u32 = u32::MAX;
/// `K_UNK_PENALTY` de `tokenizers`: un carácter desconocido puntúa esto por debajo de la peor pieza.
const PENALIZACION_DESCONOCIDO: f64 = 10.0;
const VERSION_FORMATO: u32 = 1;

// Ficheros de un modelo compilado.
const F_NORMALIZADOR: &str = "normalizador.json";
const F_TRIE: &str = "trie.bin";
const F_PUNTUACIONES: &str = "puntuaciones.bin";
const F_TABLA: &str = "tabla.i8";
const F_PESOS: &str = "pesos.f32";
const F_META: &str = "meta.json";

/// Nodo del trie de doble array. Se guarda tal cual en `trie.bin` (12 bytes, little-endian).
#[repr(C)]
#[derive(Clone, Copy)]
struct Nodo {
    base: u32,
    padre: u32,
    token: u32,
}

const LIBRE: Nodo = Nodo { base: 0, padre: NONE, token: NONE };

#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    formato: u32,
    filas: usize,
    dim: usize,
    unk_id: u32,
    unk_puntuacion: f64,
    byte_fallback: bool,
    normalizar: bool,
}

// --- Compilar (una vez, al instalar el modelo) ---------------------------------------------------

/// Convierte un modelo model2vec (int8 con `weights`, como el de `examples/fase0.rs`) al formato
/// que `Modelo::abrir` carga sin construir nada.
pub fn compilar(origen: &Path, destino: &Path) -> Result<()> {
    let tok_b = std::fs::read(origen.join("tokenizer.json"))?;
    let tok = Tokenizer::from_bytes(&tok_b).map_err(|e| anyhow!("tokenizer.json: {e}"))?;
    let ModelWrapper::Unigram(uni) = tok.get_model() else {
        return Err(anyhow!(tr!("only Unigram tokenizers are supported", "solo se admiten tokenizers Unigram")));
    };
    ensure!(
        tok.get_truncation().is_none() && tok.get_padding().is_none(),
        tr!("the tokenizer has truncation or padding configured", "el tokenizer tiene truncado o relleno configurados")
    );
    let mut json: serde_json::Value = serde_json::from_slice(&tok_b)?;
    let unk_id = json["model"]["unk_id"].as_u64().with_context(|| tr!("tokenizer.json has no unk_id", "tokenizer.json no tiene unk_id"))? as u32;

    // Puntuaciones por id y piezas para el trie. Una pieza repetida se queda con su último id,
    // como el mapa `token_to_ids` de `tokenizers`.
    let mut puntuaciones = Vec::with_capacity(uni.get_vocab_size());
    let mut piezas = Vec::with_capacity(uni.get_vocab_size());
    for (pieza, p) in uni.iter() {
        puntuaciones.push(*p);
        piezas.push((pieza.as_bytes().to_vec(), uni.token_to_id(pieza).with_context(|| tr!("piece without id: {pieza}", "pieza sin id: {pieza}"))?));
    }
    let trie = construir_trie(piezas);

    // Normalizador y pre-tokenizer sin vocabulario: basta con dejar las piezas hasta el unk.
    let vocab = json["model"]["vocab"].as_array_mut().with_context(|| tr!("tokenizer.json has no vocab", "tokenizer.json no tiene vocab"))?;
    vocab.truncate((unk_id as usize + 1).max(3));
    std::fs::create_dir_all(destino)?;
    std::fs::write(destino.join(F_NORMALIZADOR), serde_json::to_vec(&json)?)?;

    // Tabla int8 y pesos (la escala de cada fila ya va plegada en `weights`).
    let st_b = std::fs::read(origen.join("model.safetensors"))?;
    let st = safetensors::SafeTensors::deserialize(&st_b)?;
    let emb = st.tensor("embeddings").with_context(|| tr!("the model has no embeddings tensor", "el modelo no tiene el tensor embeddings"))?;
    let tipo = emb.dtype();
    ensure!(tipo == safetensors::Dtype::I8, tr!("the table must be int8, it is {tipo:?}", "la tabla tiene que ser int8, es {tipo:?}"));
    ensure!(st.tensor("mapping").is_err(), tr!("models with a `mapping` tensor are not supported", "los modelos con `mapping` no están soportados"));
    let [filas, dim]: [usize; 2] = emb.shape().try_into()?;
    let pesos = st.tensor("weights").with_context(|| tr!("the model has no weights tensor", "el modelo no tiene el tensor weights"))?;
    ensure!(
        pesos.dtype() == safetensors::Dtype::F32 && pesos.shape() == [filas],
        tr!("unexpected weights tensor", "tensor weights inesperado")
    );

    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(origen.join("config.json"))?)?;
    let meta = Meta {
        formato: VERSION_FORMATO,
        filas,
        dim,
        unk_id,
        unk_puntuacion: uni.min_score - PENALIZACION_DESCONOCIDO,
        byte_fallback: uni.byte_fallback(),
        normalizar: config.get("normalize").and_then(|v| v.as_bool()).unwrap_or(true),
    };

    let trie_b: Vec<u8> = trie.iter().flat_map(|n| [n.base, n.padre, n.token]).flat_map(u32::to_le_bytes).collect();
    let punt_b: Vec<u8> = puntuaciones.iter().flat_map(|p| p.to_le_bytes()).collect();
    std::fs::write(destino.join(F_TRIE), trie_b)?;
    std::fs::write(destino.join(F_PUNTUACIONES), punt_b)?;
    std::fs::write(destino.join(F_TABLA), emb.data())?;
    std::fs::write(destino.join(F_PESOS), pesos.data())?;
    // meta.json va el último: si existe, el resto está completo.
    std::fs::write(destino.join(F_META), serde_json::to_vec_pretty(&meta)?)?;
    Ok(())
}

/// Trie de doble array, igual que `DoubleArrayTrie::build` de model2vec-rs.
fn construir_trie(mut piezas: Vec<(Vec<u8>, u32)>) -> Vec<Nodo> {
    const MAX_FALLOS: u8 = 16;
    piezas.retain(|(k, _)| !k.is_empty());
    piezas.sort_unstable();
    piezas.dedup_by(|a, b| a.0 == b.0);

    // Siguiente hueco libre con compresión de caminos.
    struct Huecos(Vec<usize>);
    impl Huecos {
        fn buscar(&mut self, mut i: usize) -> usize {
            while i < self.0.len() && self.0[i] != i {
                let arriba = self.0[i];
                if arriba < self.0.len() {
                    self.0[i] = self.0[arriba];
                }
                i = arriba;
            }
            i
        }
        fn quitar(&mut self, i: usize) {
            if i >= self.0.len() {
                let n = self.0.len();
                self.0.extend(n..=i);
            }
            self.0[i] = i + 1;
        }
    }

    let mut nodos = vec![LIBRE];
    let mut usado = vec![true];
    let mut huecos = Huecos(Vec::new());
    huecos.quitar(0);
    let mut fallos: Vec<u8> = Vec::new();
    let mut cola = std::collections::VecDeque::from([(0u32, 0usize, 0usize, piezas.len())]);
    let mut hijos: Vec<(u8, usize, usize)> = Vec::new();
    while let Some((nodo, prof, mut lo, hi)) = cola.pop_front() {
        if lo < hi && piezas[lo].0.len() == prof {
            nodos[nodo as usize].token = piezas[lo].1;
            lo += 1;
        }
        hijos.clear();
        while lo < hi {
            let b = piezas[lo].0[prof];
            let mut fin = lo + 1;
            while fin < hi && piezas[fin].0[prof] == b {
                fin += 1;
            }
            hijos.push((b, lo, fin));
            lo = fin;
        }
        if hijos.is_empty() {
            continue;
        }
        let min = hijos[0].0 as usize;
        let mut hueco = huecos.buscar(min + 1);
        let base = loop {
            let base = hueco - min;
            if hijos.iter().all(|&(b, _, _)| !usado.get(base + b as usize).copied().unwrap_or(false)) {
                break base;
            }
            if hueco >= fallos.len() {
                fallos.resize(hueco + 1, 0);
            }
            fallos[hueco] += 1;
            if fallos[hueco] >= MAX_FALLOS {
                huecos.quitar(hueco);
            }
            hueco = huecos.buscar(hueco + 1);
        };
        let tope = base + hijos.last().unwrap().0 as usize;
        if tope >= nodos.len() {
            nodos.resize(tope + 1, LIBRE);
            usado.resize(tope + 1, false);
        }
        nodos[nodo as usize].base = base as u32;
        for &(b, lo, hi) in &hijos {
            let h = base + b as usize;
            usado[h] = true;
            huecos.quitar(h);
            nodos[h].padre = nodo;
            cola.push_back((h as u32, prof + 1, lo, hi));
        }
    }
    assert!(nodos.len() < NONE as usize, "el trie no cabe en 32 bits");
    // Margen para que `base + byte` nunca se salga del array.
    nodos.resize(nodos.len() + 256, LIBRE);
    nodos
}

// --- Usar (en cada arranque) ---------------------------------------------------------------------

/// Modelo listo para codificar. Todo lo grande está mapeado, no copiado: el sistema solo lee del
/// disco las filas que se usan.
pub struct Modelo {
    normalizador: Tokenizer,
    trie: Mmap,
    puntuaciones: Mmap,
    tabla: Mmap,
    pesos: Mmap,
    meta: Meta,
    bytes: [u32; 256],
}

/// Mejor camino que termina en una posición (en bytes) de un trozo.
#[derive(Clone, Copy)]
struct Camino {
    puntuacion: f64,
    empieza: u32,
    token: u32,
}

const SIN_CAMINO: Camino = Camino { puntuacion: 0.0, empieza: NONE, token: 0 };

fn mapear(ruta: &Path) -> Result<Mmap> {
    let f = File::open(ruta).with_context(|| tr!("can't open {}", "no se puede abrir {}", ruta.display()))?;
    // SAFETY: los ficheros del modelo solo los escribe `compilar`; nadie los cambia mientras se usan.
    Ok(unsafe { Mmap::map(&f)? })
}

impl Modelo {
    pub fn abrir(dir: &Path) -> Result<Modelo> {
        let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join(F_META)).with_context(|| tr!("the model is not compiled: {}", "el modelo no está compilado: {}", dir.display()))?)?;
        let formato = meta.formato;
        ensure!(
            formato == VERSION_FORMATO,
            tr!("model format {formato} (expected {VERSION_FORMATO})", "formato de modelo {formato} (se esperaba {VERSION_FORMATO})")
        );
        let normalizador = Tokenizer::from_file(dir.join(F_NORMALIZADOR)).map_err(|e| anyhow!("normalizador.json: {e}"))?;
        let m = Modelo {
            normalizador,
            trie: mapear(&dir.join(F_TRIE))?,
            puntuaciones: mapear(&dir.join(F_PUNTUACIONES))?,
            tabla: mapear(&dir.join(F_TABLA))?,
            pesos: mapear(&dir.join(F_PESOS))?,
            bytes: [NONE; 256],
            meta,
        };
        let corrupto = |f: &str| tr!("{f} doesn't match meta.json: recompile the model", "{f} no cuadra con meta.json: hay que recompilar el modelo");
        ensure!(m.trie.len().is_multiple_of(12), corrupto(F_TRIE));
        ensure!(m.tabla.len() == m.meta.filas * m.meta.dim, corrupto(F_TABLA));
        ensure!(m.pesos.len() == m.meta.filas * 4, corrupto(F_PESOS));
        ensure!(m.puntuaciones.len() == m.meta.filas * 8, corrupto(F_PUNTUACIONES));
        // Cada token lee una fila suelta de 256 bytes de la tabla: sin este aviso, Linux lee 128 KB
        // alrededor de cada fallo de página. Con la caché fría en un SSD SATA, el primer vector baja
        // de ~50 a ~44 ms y los diez siguientes de ~170 a ~130 ms (A/B intercalado). En el trie no
        // conviene: sus accesos sí están agrupados.
        #[cfg(unix)]
        for mapa in [&m.tabla, &m.pesos] {
            let _ = mapa.advise(memmap2::Advice::Random);
        }
        let mut m = m;
        if m.meta.byte_fallback {
            for b in 0..256 {
                m.bytes[b] = m.buscar(format!("<0x{b:02X}>").as_bytes());
            }
        }
        Ok(m)
    }

    pub fn dim(&self) -> usize {
        self.meta.dim
    }

    fn nodo(&self, i: u32) -> Nodo {
        let o = i as usize * 12;
        let u = |k: usize| u32::from_le_bytes(self.trie[o + k..o + k + 4].try_into().unwrap());
        Nodo { base: u(0), padre: u(4), token: u(8) }
    }

    #[inline(always)]
    fn hijo(&self, nodo: u32, byte: u8) -> u32 {
        let h = self.nodo(nodo).base.wrapping_add(byte as u32);
        if (h as usize) * 12 < self.trie.len() && self.nodo(h).padre == nodo { h } else { NONE }
    }

    fn buscar(&self, clave: &[u8]) -> u32 {
        let mut n = 0;
        for &b in clave {
            n = self.hijo(n, b);
            if n == NONE {
                return NONE;
            }
        }
        self.nodo(n).token
    }

    fn puntuacion(&self, id: u32) -> f64 {
        let o = id as usize * 8;
        f64::from_le_bytes(self.puntuaciones[o..o + 8].try_into().unwrap())
    }

    /// Ids de un texto, como `Tokenizer::encode(texto, false).get_ids()` (incluido el unk).
    pub fn ids(&self, texto: &str) -> Result<Vec<u32>> {
        let mut pre = self.normalizador.get_added_vocabulary().extract_and_normalize(self.normalizador.get_normalizer(), texto);
        if let Some(p) = self.normalizador.get_pre_tokenizer() {
            p.pre_tokenize(&mut pre).map_err(|e| anyhow!("{e}"))?;
        }
        let mut salida = Vec::new();
        let mut mejor = Vec::new();
        for (trozo, _, tokens) in pre.get_splits(OffsetReferential::Normalized, OffsetType::None) {
            match tokens {
                Some(t) => salida.extend(t.iter().map(|t| t.id)),
                None => self.viterbi(trozo, &mut mejor, &mut salida),
            }
        }
        Ok(salida)
    }

    fn viterbi(&self, texto: &str, mejor: &mut Vec<Camino>, salida: &mut Vec<u32>) {
        let b = texto.as_bytes();
        let n = b.len();
        if n == 0 {
            return;
        }
        mejor.clear();
        mejor.resize(n + 1, SIN_CAMINO);
        mejor[0].empieza = 0;

        let mut ini = 0;
        while ini < n {
            let hasta_aqui = mejor[ini].puntuacion;
            let largo_char = largo_utf8(b[ini]);
            let mut tiene_un_char = false;
            let (mut nodo, mut fin) = (0, ini);
            while fin < n {
                nodo = self.hijo(nodo, b[fin]);
                if nodo == NONE {
                    break;
                }
                fin += 1;
                let id = self.nodo(nodo).token;
                if id != NONE {
                    relajar(&mut mejor[fin], self.puntuacion(id) + hasta_aqui, ini, id);
                    tiene_un_char |= fin - ini == largo_char;
                }
            }
            if !tiene_un_char {
                relajar(&mut mejor[ini + largo_char], self.meta.unk_puntuacion + hasta_aqui, ini, self.meta.unk_id);
            }
            ini += largo_char;
        }

        // Hacia atrás, fusionando rachas de desconocidos; se invierte al final.
        let marca = salida.len();
        let mut fin = n;
        let mut fin_racha = None;
        while fin > 0 {
            let c = mejor[fin];
            if c.token == self.meta.unk_id {
                fin_racha.get_or_insert(fin);
            } else {
                if let Some(r) = fin_racha.take() {
                    self.desconocido_al_reves(&texto[fin..r], salida);
                }
                salida.push(c.token);
            }
            fin = c.empieza as usize;
        }
        if let Some(r) = fin_racha {
            self.desconocido_al_reves(&texto[..r], salida);
        }
        salida[marca..].reverse();
    }

    fn desconocido_al_reves(&self, racha: &str, salida: &mut Vec<u32>) {
        let id = self.buscar(racha.as_bytes());
        if id != NONE {
            salida.push(id);
        } else if self.meta.byte_fallback && racha.bytes().all(|b| self.bytes[b as usize] != NONE) {
            salida.extend(racha.bytes().rev().map(|b| self.bytes[b as usize]));
        } else {
            salida.push(self.meta.unk_id);
        }
    }

    /// Vector de un texto: media de las filas de sus tokens (sin el unk), ponderada, y normalizada.
    pub fn vector(&self, texto: &str) -> Result<Vec<f32>> {
        let dim = self.meta.dim;
        let mut suma = vec![0.0f32; dim];
        let mut n = 0usize;
        for id in self.ids(texto)? {
            if id == self.meta.unk_id {
                continue;
            }
            let o = id as usize * 4;
            let peso = f32::from_le_bytes(self.pesos[o..o + 4].try_into().unwrap());
            let fila = &self.tabla[id as usize * dim..(id as usize + 1) * dim];
            for (s, &v) in suma.iter_mut().zip(fila) {
                *s += (v as i8) as f32 * peso;
            }
            n += 1;
        }
        let d = n.max(1) as f32;
        suma.iter_mut().for_each(|x| *x /= d);
        if self.meta.normalizar {
            let norma = suma.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            suma.iter_mut().for_each(|x| *x /= norma);
        }
        Ok(suma)
    }
}

#[inline(always)]
fn relajar(destino: &mut Camino, candidato: f64, empieza: usize, token: u32) {
    if destino.empieza == NONE || candidato > destino.puntuacion {
        *destino = Camino { puntuacion: candidato, empieza: empieza as u32, token };
    }
}

#[inline(always)]
fn largo_utf8(primero: u8) -> usize {
    match primero {
        0x00..=0x7f => 1,
        0x80..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}
