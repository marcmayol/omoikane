//! Static embeddings (model2vec) that load in milliseconds.
//!
//! `tokenizers` takes 1.3 s to build the 500,000-piece Unigram vocabulary every time it is
//! loaded. Here that work is done once (`compile`) and saved in flat files that are then only
//! memory-mapped (`Model::open`): the double-array trie, the scores, the int8 table and
//! the per-token weights. From `tokenizer.json` only the normalizer and the pre-tokenizer are kept, without
//! the vocabulary, which loads in 3 ms.
//!
//! The segmentation (Unigram Viterbi) and the trie reproduce `fast_unigram.rs` and `trie.rs` from
//! model2vec-rs (MIT, © The Minish Lab), which in turn reproduce `Unigram::encode_optimized` from
//! `tokenizers`: same tie-breaks, same unknown penalty and same merging of unknown
//! runs. Equivalence is checked against `tokenizers` and against model2vec-rs.

use crate::tr;
use anyhow::{Context, Result, anyhow, ensure};
use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use tokenizers::models::ModelWrapper;
use tokenizers::{Model as _, OffsetReferential, OffsetType, PreTokenizer, Tokenizer};

// The model files are written and read in little-endian (x86-64 and ARM64, Apple Silicon
// included). On a big-endian machine they would silently give garbage vectors: better not to compile.
#[cfg(target_endian = "big")]
compile_error!("omoikane only works on little-endian platforms");

const NONE: u32 = u32::MAX;
/// `K_UNK_PENALTY` from `tokenizers`: an unknown character scores this much below the worst piece.
const UNK_PENALTY: f64 = 10.0;
const FORMAT_VERSION: u32 = 1;

// Files of a compiled model.
const F_NORMALIZER: &str = "normalizador.json";
const F_TRIE: &str = "trie.bin";
const F_SCORES: &str = "puntuaciones.bin";
const F_TABLE: &str = "tabla.i8";
const F_WEIGHTS: &str = "pesos.f32";
const F_META: &str = "meta.json";

/// Node of the double-array trie. Stored as is in `trie.bin` (12 bytes, little-endian).
#[repr(C)]
#[derive(Clone, Copy)]
struct Node {
    base: u32,
    parent: u32,
    token: u32,
}

const FREE: Node = Node { base: 0, parent: NONE, token: NONE };

#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    #[serde(rename = "formato")]
    format: u32,
    #[serde(rename = "filas")]
    rows: usize,
    dim: usize,
    unk_id: u32,
    #[serde(rename = "unk_puntuacion")]
    unk_score: f64,
    byte_fallback: bool,
    #[serde(rename = "normalizar")]
    normalize: bool,
}

// --- Compile (once, when installing the model) ---------------------------------------------------

/// Converts a model2vec model (int8 with `weights`, like the one in `examples/phase0.rs`) to the format
/// that `Model::open` loads without building anything.
pub fn compile(src: &Path, dest: &Path) -> Result<()> {
    let tok_b = std::fs::read(src.join("tokenizer.json"))?;
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

    // Scores by id and pieces for the trie. A repeated piece keeps its last id,
    // like the `token_to_ids` map in `tokenizers`.
    let mut scores = Vec::with_capacity(uni.get_vocab_size());
    let mut pieces = Vec::with_capacity(uni.get_vocab_size());
    for (piece, p) in uni.iter() {
        scores.push(*p);
        pieces.push((piece.as_bytes().to_vec(), uni.token_to_id(piece).with_context(|| tr!("piece without id: {piece}", "pieza sin id: {piece}"))?));
    }
    let trie = build_trie(pieces);

    // Normalizer and pre-tokenizer without vocabulary: keeping the pieces up to the unk is enough.
    let vocab = json["model"]["vocab"].as_array_mut().with_context(|| tr!("tokenizer.json has no vocab", "tokenizer.json no tiene vocab"))?;
    vocab.truncate((unk_id as usize + 1).max(3));
    std::fs::create_dir_all(dest)?;
    std::fs::write(dest.join(F_NORMALIZER), serde_json::to_vec(&json)?)?;

    // Int8 table and weights (each row's scale is already folded into `weights`).
    let st_b = std::fs::read(src.join("model.safetensors"))?;
    let st = safetensors::SafeTensors::deserialize(&st_b)?;
    let emb = st.tensor("embeddings").with_context(|| tr!("the model has no embeddings tensor", "el modelo no tiene el tensor embeddings"))?;
    let dtype = emb.dtype();
    ensure!(dtype == safetensors::Dtype::I8, tr!("the table must be int8, it is {dtype:?}", "la tabla tiene que ser int8, es {dtype:?}"));
    ensure!(st.tensor("mapping").is_err(), tr!("models with a `mapping` tensor are not supported", "los modelos con `mapping` no están soportados"));
    let [rows, dim]: [usize; 2] = emb.shape().try_into()?;
    let weights = st.tensor("weights").with_context(|| tr!("the model has no weights tensor", "el modelo no tiene el tensor weights"))?;
    ensure!(
        weights.dtype() == safetensors::Dtype::F32 && weights.shape() == [rows],
        tr!("unexpected weights tensor", "tensor weights inesperado")
    );

    let config: serde_json::Value = serde_json::from_slice(&std::fs::read(src.join("config.json"))?)?;
    let meta = Meta {
        format: FORMAT_VERSION,
        rows,
        dim,
        unk_id,
        unk_score: uni.min_score - UNK_PENALTY,
        byte_fallback: uni.byte_fallback(),
        normalize: config.get("normalize").and_then(|v| v.as_bool()).unwrap_or(true),
    };

    let trie_b: Vec<u8> = trie.iter().flat_map(|n| [n.base, n.parent, n.token]).flat_map(u32::to_le_bytes).collect();
    let scores_b: Vec<u8> = scores.iter().flat_map(|p| p.to_le_bytes()).collect();
    std::fs::write(dest.join(F_TRIE), trie_b)?;
    std::fs::write(dest.join(F_SCORES), scores_b)?;
    std::fs::write(dest.join(F_TABLE), emb.data())?;
    std::fs::write(dest.join(F_WEIGHTS), weights.data())?;
    // meta.json goes last: if it exists, the rest is complete.
    std::fs::write(dest.join(F_META), serde_json::to_vec_pretty(&meta)?)?;
    Ok(())
}

/// Double-array trie, the same as model2vec-rs's `DoubleArrayTrie::build`.
fn build_trie(mut pieces: Vec<(Vec<u8>, u32)>) -> Vec<Node> {
    const MAX_FAILS: u8 = 16;
    pieces.retain(|(k, _)| !k.is_empty());
    pieces.sort_unstable();
    pieces.dedup_by(|a, b| a.0 == b.0);

    // Next free gap with path compression.
    struct Gaps(Vec<usize>);
    impl Gaps {
        fn lookup(&mut self, mut i: usize) -> usize {
            while i < self.0.len() && self.0[i] != i {
                let up = self.0[i];
                if up < self.0.len() {
                    self.0[i] = self.0[up];
                }
                i = up;
            }
            i
        }
        fn remove(&mut self, i: usize) {
            if i >= self.0.len() {
                let n = self.0.len();
                self.0.extend(n..=i);
            }
            self.0[i] = i + 1;
        }
    }

    let mut nodes = vec![FREE];
    let mut used = vec![true];
    let mut gaps = Gaps(Vec::new());
    gaps.remove(0);
    let mut fails: Vec<u8> = Vec::new();
    let mut queue = std::collections::VecDeque::from([(0u32, 0usize, 0usize, pieces.len())]);
    let mut children: Vec<(u8, usize, usize)> = Vec::new();
    while let Some((node, depth, mut lo, hi)) = queue.pop_front() {
        if lo < hi && pieces[lo].0.len() == depth {
            nodes[node as usize].token = pieces[lo].1;
            lo += 1;
        }
        children.clear();
        while lo < hi {
            let b = pieces[lo].0[depth];
            let mut end = lo + 1;
            while end < hi && pieces[end].0[depth] == b {
                end += 1;
            }
            children.push((b, lo, end));
            lo = end;
        }
        if children.is_empty() {
            continue;
        }
        let min = children[0].0 as usize;
        let mut gap = gaps.lookup(min + 1);
        let base = loop {
            let base = gap - min;
            if children.iter().all(|&(b, _, _)| !used.get(base + b as usize).copied().unwrap_or(false)) {
                break base;
            }
            if gap >= fails.len() {
                fails.resize(gap + 1, 0);
            }
            fails[gap] += 1;
            if fails[gap] >= MAX_FAILS {
                gaps.remove(gap);
            }
            gap = gaps.lookup(gap + 1);
        };
        let top = base + children.last().unwrap().0 as usize;
        if top >= nodes.len() {
            nodes.resize(top + 1, FREE);
            used.resize(top + 1, false);
        }
        nodes[node as usize].base = base as u32;
        for &(b, lo, hi) in &children {
            let h = base + b as usize;
            used[h] = true;
            gaps.remove(h);
            nodes[h].parent = node;
            queue.push_back((h as u32, depth + 1, lo, hi));
        }
    }
    assert!(nodes.len() < NONE as usize, "el trie no cabe en 32 bits");
    // Margin so that `base + byte` never goes past the array.
    nodes.resize(nodes.len() + 256, FREE);
    nodes
}

// --- Use (on every start) ------------------------------------------------------------------------

/// Model ready to encode. Everything big is mapped, not copied: the system only reads from
/// disk the rows that are used.
pub struct Model {
    /// Name of its folder: identifies which model some vectors were computed with.
    id: String,
    normalizer: Tokenizer,
    trie: Mmap,
    scores: Mmap,
    table: Mmap,
    weights: Mmap,
    meta: Meta,
    bytes: [u32; 256],
}

/// Best path ending at a position (in bytes) of a chunk.
#[derive(Clone, Copy)]
struct Step {
    score: f64,
    start: u32,
    token: u32,
}

const NO_STEP: Step = Step { score: 0.0, start: NONE, token: 0 };

fn map_file(path: &Path) -> Result<Mmap> {
    let f = File::open(path).with_context(|| tr!("can't open {}", "no se puede abrir {}", path.display()))?;
    // SAFETY: the model files are only written by `compile`; nobody changes them while in use.
    Ok(unsafe { Mmap::map(&f)? })
}

impl Model {
    pub fn open(dir: &Path) -> Result<Model> {
        let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join(F_META)).with_context(|| tr!("the model is not compiled: {}", "el modelo no está compilado: {}", dir.display()))?)?;
        let version = meta.format;
        ensure!(
            version == FORMAT_VERSION,
            tr!("model format {version} (expected {FORMAT_VERSION})", "formato de modelo {version} (se esperaba {FORMAT_VERSION})")
        );
        let normalizer = Tokenizer::from_file(dir.join(F_NORMALIZER)).map_err(|e| anyhow!("normalizador.json: {e}"))?;
        let m = Model {
            id: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            normalizer,
            trie: map_file(&dir.join(F_TRIE))?,
            scores: map_file(&dir.join(F_SCORES))?,
            table: map_file(&dir.join(F_TABLE))?,
            weights: map_file(&dir.join(F_WEIGHTS))?,
            bytes: [NONE; 256],
            meta,
        };
        let corrupt = |f: &str| tr!("{f} doesn't match meta.json: recompile the model", "{f} no cuadra con meta.json: hay que recompilar el modelo");
        ensure!(m.trie.len().is_multiple_of(12), corrupt(F_TRIE));
        ensure!(m.table.len() == m.meta.rows * m.meta.dim, corrupt(F_TABLE));
        ensure!(m.weights.len() == m.meta.rows * 4, corrupt(F_WEIGHTS));
        ensure!(m.scores.len() == m.meta.rows * 8, corrupt(F_SCORES));
        // Each token reads a single 256-byte row of the table: without this hint, Linux reads 128 KB
        // around each page fault. With a cold cache on a SATA SSD, the first vector drops
        // from ~50 to ~44 ms and the next ten from ~170 to ~130 ms (interleaved A/B). Not worth it
        // for the trie: its accesses are clustered.
        #[cfg(unix)]
        for map in [&m.table, &m.weights] {
            let _ = map.advise(memmap2::Advice::Random);
        }
        let mut m = m;
        if m.meta.byte_fallback {
            for b in 0..256 {
                m.bytes[b] = m.lookup(format!("<0x{b:02X}>").as_bytes());
            }
        }
        Ok(m)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dim(&self) -> usize {
        self.meta.dim
    }

    fn node(&self, i: u32) -> Node {
        let o = i as usize * 12;
        let u = |k: usize| u32::from_le_bytes(self.trie[o + k..o + k + 4].try_into().unwrap());
        Node { base: u(0), parent: u(4), token: u(8) }
    }

    #[inline(always)]
    fn child(&self, node: u32, byte: u8) -> u32 {
        let h = self.node(node).base.wrapping_add(byte as u32);
        if (h as usize) * 12 < self.trie.len() && self.node(h).parent == node { h } else { NONE }
    }

    fn lookup(&self, key: &[u8]) -> u32 {
        let mut n = 0;
        for &b in key {
            n = self.child(n, b);
            if n == NONE {
                return NONE;
            }
        }
        self.node(n).token
    }

    fn score(&self, id: u32) -> f64 {
        let o = id as usize * 8;
        f64::from_le_bytes(self.scores[o..o + 8].try_into().unwrap())
    }

    /// Ids of a text, like `Tokenizer::encode(text, false).get_ids()` (unk included).
    pub fn ids(&self, text: &str) -> Result<Vec<u32>> {
        let mut pre = self.normalizer.get_added_vocabulary().extract_and_normalize(self.normalizer.get_normalizer(), text);
        if let Some(p) = self.normalizer.get_pre_tokenizer() {
            p.pre_tokenize(&mut pre).map_err(|e| anyhow!("{e}"))?;
        }
        let mut out = Vec::new();
        let mut best = Vec::new();
        for (chunk, _, tokens) in pre.get_splits(OffsetReferential::Normalized, OffsetType::None) {
            match tokens {
                Some(t) => out.extend(t.iter().map(|t| t.id)),
                None => self.viterbi(chunk, &mut best, &mut out),
            }
        }
        Ok(out)
    }

    fn viterbi(&self, text: &str, best: &mut Vec<Step>, out: &mut Vec<u32>) {
        let b = text.as_bytes();
        let n = b.len();
        if n == 0 {
            return;
        }
        best.clear();
        best.resize(n + 1, NO_STEP);
        best[0].start = 0;

        let mut pos = 0;
        while pos < n {
            let so_far = best[pos].score;
            let char_len = utf8_len(b[pos]);
            let mut has_single_char = false;
            let (mut node, mut end) = (0, pos);
            while end < n {
                node = self.child(node, b[end]);
                if node == NONE {
                    break;
                }
                end += 1;
                let id = self.node(node).token;
                if id != NONE {
                    relax(&mut best[end], self.score(id) + so_far, pos, id);
                    has_single_char |= end - pos == char_len;
                }
            }
            if !has_single_char {
                relax(&mut best[pos + char_len], self.meta.unk_score + so_far, pos, self.meta.unk_id);
            }
            pos += char_len;
        }

        // Backwards, merging unknown runs; reversed at the end.
        let mark = out.len();
        let mut end = n;
        let mut run_end = None;
        while end > 0 {
            let c = best[end];
            if c.token == self.meta.unk_id {
                run_end.get_or_insert(end);
            } else {
                if let Some(r) = run_end.take() {
                    self.unknown_reversed(&text[end..r], out);
                }
                out.push(c.token);
            }
            end = c.start as usize;
        }
        if let Some(r) = run_end {
            self.unknown_reversed(&text[..r], out);
        }
        out[mark..].reverse();
    }

    fn unknown_reversed(&self, run: &str, out: &mut Vec<u32>) {
        let id = self.lookup(run.as_bytes());
        if id != NONE {
            out.push(id);
        } else if self.meta.byte_fallback && run.bytes().all(|b| self.bytes[b as usize] != NONE) {
            out.extend(run.bytes().rev().map(|b| self.bytes[b as usize]));
        } else {
            out.push(self.meta.unk_id);
        }
    }

    /// Vector of a text: weighted mean of its tokens' rows (without the unk), normalized.
    pub fn vector(&self, text: &str) -> Result<Vec<f32>> {
        let dim = self.meta.dim;
        let mut sum = vec![0.0f32; dim];
        let mut n = 0usize;
        for id in self.ids(text)? {
            if id == self.meta.unk_id {
                continue;
            }
            let o = id as usize * 4;
            let weight = f32::from_le_bytes(self.weights[o..o + 4].try_into().unwrap());
            let row = &self.table[id as usize * dim..(id as usize + 1) * dim];
            for (s, &v) in sum.iter_mut().zip(row) {
                *s += (v as i8) as f32 * weight;
            }
            n += 1;
        }
        let d = n.max(1) as f32;
        sum.iter_mut().for_each(|x| *x /= d);
        if self.meta.normalize {
            let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            sum.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(sum)
    }
}

#[inline(always)]
fn relax(dest: &mut Step, candidate: f64, start: usize, token: u32) {
    if dest.start == NONE || candidate > dest.score {
        *dest = Step { score: candidate, start: start as u32, token };
    }
}

#[inline(always)]
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0x80..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}
