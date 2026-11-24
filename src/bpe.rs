//! EmbeddingGemma's tokenizer compiled to a file that is memory-mapped instead of built.
//!
//! Reading Gemma's `tokenizer.json` (33 MB: 262,144 pieces and 514,906 merges) with `tokenizers`
//! takes 1.3-1.6 s, nearly all of the service's load time. Here that work is done once (`compile`,
//! when the model is installed or the first time it loads) and saved as `tokenizer.omoi`, next to
//! `tokenizer.json`, which is left untouched (the published files and their SHA-256 stay the same).
//! Loading it is mapping it: a few milliseconds.
//!
//! It reproduces what `tokenizers` does with this tokenizer, step by step:
//! 1. Splits the text on the added tokens (`added_tokens`: `<bos>`, `<unused12>`, runs of `\n`,
//!    `\t` and `▁`, HTML tags…), without normalizing and keeping the longest one that starts
//!    leftmost, like its Aho-Corasick `LeftmostLongest`.
//! 2. In each remaining piece replaces spaces with `▁` (the normalizer). The pre-tokenizer splits on
//!    spaces, and after normalizing there are none left: it does nothing.
//! 3. BPE: each character is its own piece or, if it has none, its bytes (`<0x0A>`…), and they are
//!    merged by priority with the same queue (rank and, at equal rank, the leftmost position) and
//!    the same way of skipping stale entries as `Word::merge_all`.
//! 4. `<bos>` + pieces + `<eos>`, cut to `max` like its truncation (the first ones and `<eos>` stay).
//!
//! If `tokenizer.json` isn't shaped like this, `compile` refuses and `gemma` uses `tokenizers`.
//! The equivalence is checked against `tokenizers` (test `bpe_igual_que_tokenizers` and
//! `examples/bpe_igual.rs`, with thousands of real texts).

use anyhow::{Context, Result, bail, ensure};
use memmap2::Mmap;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

/// The compiled file, in the model's folder.
pub const FILE: &str = "tokenizer.omoi";
const MAGIC: &[u8; 8] = b"OMOIBPE\x01";
const EMPTY: u64 = u64::MAX;
const NONE: u32 = u32::MAX;

/// A compiled, memory-mapped tokenizer.
pub struct Bpe {
    map: Mmap,
    bos: u32,
    eos: u32,
    /// `(char, id)` sorted by char: 8 bytes each, starting at `chars.0`.
    chars: (usize, usize),
    bytes: [u32; 256],
    /// Hash table of merges: `(a << 32 | b) → (rank, new)`, 16 bytes per slot from `merges.0`.
    merges: (usize, u32),
    added: HashMap<Box<[u8]>, u32>,
    /// By first byte: the lengths of the added tokens that start with it, longest first.
    lens: Vec<Vec<usize>>,
}

/// What identifies the `tokenizer.json` it came from: size and mtime. If it changes, it's recompiled.
fn fingerprint(json: &Path) -> Result<(u64, u64)> {
    let m = std::fs::metadata(json)?;
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0);
    Ok((m.len(), t))
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

fn hash(key: u64, bits: u32) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - bits)) as usize
}

/// Compiles `json` (a `tokenizer.json`) to `dest`. Fails if it isn't a BPE like Gemma's.
pub fn compile(json: &Path, dest: &Path) -> Result<()> {
    let d: serde_json::Value = serde_json::from_slice(&std::fs::read(json)?)?;
    let m = &d["model"];
    ensure!(m["type"] == "BPE", "not a BPE tokenizer");
    ensure!(m["byte_fallback"] == true, "BPE without byte_fallback");
    for k in ["dropout", "continuing_subword_prefix", "end_of_word_suffix"] {
        ensure!(m[k].is_null(), "BPE with {k}");
    }
    ensure!(m["ignore_merges"] != true, "BPE with ignore_merges");
    ensure!(d["truncation"].is_null() && d["padding"].is_null(), "tokenizer with truncation or padding");
    // Normalizer: replace " " with "▁"; pre-tokenizer: split on " " (a no-op after normalizing).
    let n = &d["normalizer"];
    ensure!(n["type"] == "Replace" && n["pattern"]["String"] == " " && n["content"] == "\u{2581}", "unexpected normalizer");
    let p = &d["pre_tokenizer"];
    ensure!(p.is_null() || (p["type"] == "Split" && p["pattern"]["String"] == " "), "unexpected pre-tokenizer");
    // Template: <bos> A <eos>.
    let pp = &d["post_processor"];
    ensure!(pp["type"] == "TemplateProcessing", "unexpected post-processor");
    let single = pp["single"].as_array().context("post-processor without single")?;
    let special = |v: &serde_json::Value| -> Option<String> { v["SpecialToken"]["id"].as_str().map(str::to_string) };
    ensure!(single.len() == 3 && single[1]["Sequence"]["id"] == "A", "unexpected template");
    let (bos_s, eos_s) = (special(&single[0]).context("template")?, special(&single[2]).context("template")?);
    let special_id = |s: &str| pp["special_tokens"][s]["ids"][0].as_u64().map(|x| x as u32).with_context(|| format!("special token {s}"));
    let (bos, eos) = (special_id(&bos_s)?, special_id(&eos_s)?);

    let vocab = m["vocab"].as_object().context("BPE without vocab")?;
    let mut chars: Vec<(u32, u32)> = Vec::new();
    for (piece, id) in vocab {
        let mut it = piece.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            chars.push((c as u32, id.as_u64().context("vocab id")? as u32));
        }
    }
    chars.sort_unstable();
    let mut bytes = [NONE; 256];
    for (b, id) in bytes.iter_mut().enumerate() {
        *id = vocab.get(&format!("<0x{b:02X}>")).and_then(|v| v.as_u64()).context("BPE without all 256 byte tokens")? as u32;
    }

    let merges = m["merges"].as_array().context("BPE without merges")?;
    let bits = (merges.len() * 2).next_power_of_two().trailing_zeros().max(4);
    let mut table = vec![(EMPTY, 0u32, 0u32); 1 << bits];
    let id_of = |s: &str| vocab.get(s).and_then(|v| v.as_u64()).map(|x| x as u32);
    for (rank, f) in merges.iter().enumerate() {
        let (a, b) = match f {
            serde_json::Value::Array(pair) if pair.len() == 2 => (pair[0].as_str().context("merge")?.to_string(), pair[1].as_str().context("merge")?.to_string()),
            serde_json::Value::String(s) => {
                let (a, b) = s.split_once(' ').context("merge")?;
                (a.to_string(), b.to_string())
            }
            _ => bail!("unexpected merge {f}"),
        };
        let (ia, ib) = (id_of(&a).context("merge piece not in vocab")?, id_of(&b).context("merge piece not in vocab")?);
        let new = id_of(&format!("{a}{b}")).context("merge result not in vocab")?;
        let key = (u64::from(ia) << 32) | u64::from(ib);
        let mut h = hash(key, bits);
        // Like the HashMap in `tokenizers`: a repeated merge keeps the last one.
        while table[h].0 != EMPTY && table[h].0 != key {
            h = (h + 1) & ((1 << bits) - 1);
        }
        table[h] = (key, rank as u32, new);
    }

    let mut added: Vec<(u32, &str)> = Vec::new();
    for a in d["added_tokens"].as_array().context("no added_tokens")? {
        ensure!(a["normalized"] != true && a["lstrip"] != true && a["rstrip"] != true && a["single_word"] != true, "added token with options");
        added.push((a["id"].as_u64().context("added id")? as u32, a["content"].as_str().context("added content")?));
    }

    let (size, mtime) = fingerprint(json)?;
    let mut o: Vec<u8> = Vec::with_capacity(16 << 20);
    o.extend_from_slice(MAGIC);
    o.extend_from_slice(&size.to_le_bytes());
    o.extend_from_slice(&mtime.to_le_bytes());
    for x in [bos, eos, bits, chars.len() as u32, added.len() as u32] {
        o.extend_from_slice(&x.to_le_bytes());
    }
    for b in bytes {
        o.extend_from_slice(&b.to_le_bytes());
    }
    for (c, id) in &chars {
        o.extend_from_slice(&c.to_le_bytes());
        o.extend_from_slice(&id.to_le_bytes());
    }
    for (key, rank, new) in &table {
        o.extend_from_slice(&key.to_le_bytes());
        o.extend_from_slice(&rank.to_le_bytes());
        o.extend_from_slice(&new.to_le_bytes());
    }
    for (id, s) in &added {
        o.extend_from_slice(&id.to_le_bytes());
        o.extend_from_slice(&(s.len() as u32).to_le_bytes());
        o.extend_from_slice(s.as_bytes());
    }
    // To a temporary file and rename: whoever opens it at the same time never sees half of it.
    let tmp = dest.with_extension(format!("part{}", std::process::id()));
    std::fs::write(&tmp, &o)?;
    std::fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

impl Bpe {
    /// Opens `dest` if it was compiled from this `json`; otherwise compiles it (and leaves it written
    /// if it can; if the folder is read-only, compiles it to a temporary file).
    pub fn open_or_compile(json: &Path, dest: &Path) -> Result<Bpe> {
        if let Ok(b) = Self::open(json, dest) {
            return Ok(b);
        }
        if compile(json, dest).is_ok() {
            return Self::open(json, dest);
        }
        let tmp = std::env::temp_dir().join(format!("omoi-{}-{FILE}", std::process::id()));
        compile(json, &tmp)?;
        let b = Self::open(json, &tmp);
        let _ = std::fs::remove_file(&tmp);
        b
    }

    /// Opens `dest`, checking it came from `json` as it is now.
    pub fn open(json: &Path, dest: &Path) -> Result<Bpe> {
        let f = std::fs::File::open(dest)?;
        // SAFETY: it's written whole to a temporary file and renamed; nobody modifies it in place.
        let map = unsafe { Mmap::map(&f)? };
        let b = &map[..];
        ensure!(b.len() >= 44 + 1024 && &b[..8] == MAGIC, "{FILE}: not a compiled tokenizer");
        ensure!((u64_at(b, 8), u64_at(b, 16)) == fingerprint(json)?, "{FILE}: compiled from another tokenizer.json");
        let (bos, eos, bits, n_chars, n_added) = (u32_at(b, 24), u32_at(b, 28), u32_at(b, 32), u32_at(b, 36) as usize, u32_at(b, 40) as usize);
        let mut bytes = [NONE; 256];
        for (i, x) in bytes.iter_mut().enumerate() {
            *x = u32_at(b, 44 + 4 * i);
        }
        let chars_start = 44 + 1024;
        let merges_start = chars_start + 8 * n_chars;
        let mut i = merges_start + 16 * (1usize << bits);
        ensure!(i <= b.len(), "{FILE}: truncated");
        let mut added = HashMap::with_capacity(n_added);
        let mut lens = vec![Vec::new(); 256];
        for _ in 0..n_added {
            ensure!(i + 8 <= b.len(), "{FILE}: truncated");
            let (id, l) = (u32_at(b, i), u32_at(b, i + 4) as usize);
            ensure!(l > 0 && i + 8 + l <= b.len(), "{FILE}: truncated");
            let s: Box<[u8]> = b[i + 8..i + 8 + l].into();
            let ls: &mut Vec<usize> = &mut lens[s[0] as usize];
            if !ls.contains(&l) {
                ls.push(l);
            }
            added.insert(s, id);
            i += 8 + l;
        }
        for l in &mut lens {
            l.sort_unstable_by(|a, b| b.cmp(a));
        }
        Ok(Bpe { bos, eos, chars: (chars_start, n_chars), bytes, merges: (merges_start, bits), added, lens, map })
    }

    fn char_id(&self, c: char) -> Option<u32> {
        let (start, n) = self.chars;
        let b = &self.map[..];
        let c = c as u32;
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let m = (lo + hi) / 2;
            let k = u32_at(b, start + 8 * m);
            match k.cmp(&c) {
                std::cmp::Ordering::Less => lo = m + 1,
                std::cmp::Ordering::Greater => hi = m,
                std::cmp::Ordering::Equal => return Some(u32_at(b, start + 8 * m + 4)),
            }
        }
        None
    }

    /// `(rank, new)` of the merge of `a` with `b`.
    fn merge(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (start, bits) = self.merges;
        let m = &self.map[..];
        let key = (u64::from(a) << 32) | u64::from(b);
        let mut h = hash(key, bits);
        loop {
            let p = start + 16 * h;
            let k = u64_at(m, p);
            if k == key {
                return Some((u32_at(m, p + 8), u32_at(m, p + 12)));
            }
            if k == EMPTY {
                return None;
            }
            h = (h + 1) & ((1 << bits) - 1);
        }
    }

    /// The longest added token that starts at `t[0]`, with its length in bytes.
    fn added_token(&self, t: &[u8]) -> Option<(usize, u32)> {
        for &l in &self.lens[t[0] as usize] {
            if l <= t.len()
                && let Some(&id) = self.added.get(&t[..l])
            {
                return Some((l, id));
            }
        }
        None
    }

    /// The ids of `text` with `<bos>` and `<eos>`, at most `max` (counting them).
    pub fn encode(&self, text: &str, max: usize) -> Vec<u32> {
        let mut ids = vec![self.bos];
        let b = text.as_bytes();
        let (mut i, mut start) = (0, 0);
        while i < b.len() {
            // Every added token is valid UTF-8: it can only start where a character starts.
            if !self.lens[b[i] as usize].is_empty()
                && let Some((l, id)) = self.added_token(&b[i..])
            {
                self.piece(&text[start..i], &mut ids);
                ids.push(id);
                i += l;
                start = i;
                continue;
            }
            i += 1;
        }
        self.piece(&text[start..], &mut ids);
        if ids.len() + 1 > max {
            ids.truncate(max.max(2) - 1);
        }
        ids.push(self.eos);
        ids
    }

    /// BPE of a piece with no added tokens (`Word::merge_all` in `tokenizers`).
    fn piece(&self, t: &str, ids: &mut Vec<u32>) {
        if t.is_empty() {
            return;
        }
        #[derive(Clone, Copy)]
        struct S {
            c: u32,
            prev: isize,
            next: isize,
            alive: bool,
        }
        let mut s: Vec<S> = Vec::with_capacity(t.len());
        let push = |c: u32, s: &mut Vec<S>| {
            let i = s.len() as isize;
            if let Some(u) = s.last_mut() {
                u.next = i;
            }
            s.push(S { c, prev: i - 1, next: -1, alive: true });
        };
        let mut buf = [0u8; 4];
        for c in t.chars() {
            let c = if c == ' ' { '\u{2581}' } else { c };
            match self.char_id(c) {
                Some(id) => push(id, &mut s),
                None => {
                    for &byte in c.encode_utf8(&mut buf).as_bytes() {
                        push(self.bytes[byte as usize], &mut s);
                    }
                }
            }
        }
        // Min-heap by (rank, position).
        #[derive(PartialEq, Eq)]
        struct F {
            rank: u32,
            pos: usize,
            new: u32,
        }
        impl Ord for F {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.rank.cmp(&self.rank).then(o.pos.cmp(&self.pos))
            }
        }
        impl PartialOrd for F {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        let mut queue = BinaryHeap::with_capacity(s.len());
        for i in 0..s.len().saturating_sub(1) {
            if let Some((rank, new)) = self.merge(s[i].c, s[i + 1].c) {
                queue.push(F { rank, pos: i, new });
            }
        }
        while let Some(f) = queue.pop() {
            let cur = s[f.pos];
            if !cur.alive || cur.next == -1 {
                continue;
            }
            let nx = cur.next as usize;
            let right = s[nx];
            // A stale entry: the pair is no longer what it was.
            if self.merge(cur.c, right.c).is_none_or(|(_, n)| n != f.new) {
                continue;
            }
            s[f.pos].c = f.new;
            s[f.pos].next = right.next;
            s[nx].alive = false;
            if right.next > -1 && (right.next as usize) < s.len() {
                s[right.next as usize].prev = f.pos as isize;
            }
            let cur = s[f.pos];
            if cur.prev >= 0
                && let Some((rank, new)) = self.merge(s[cur.prev as usize].c, cur.c)
            {
                queue.push(F { rank, pos: cur.prev as usize, new });
            }
            let n = cur.next;
            if n >= 0
                && (n as usize) < s.len()
                && let Some((rank, new)) = self.merge(cur.c, s[n as usize].c)
            {
                queue.push(F { rank, pos: f.pos, new });
            }
        }
        ids.extend(s.iter().filter(|x| x.alive).map(|x| x.c));
    }
}
