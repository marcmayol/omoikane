//! El tokenizer de EmbeddingGemma compilado a un fichero que se mapea y no se construye.
//!
//! Leer el `tokenizer.json` de Gemma (33 MB: 262.144 piezas y 514.906 fusiones) con `tokenizers`
//! cuesta 1,3-1,6 s, casi todo lo que tardaba en cargar el servicio. Aquí ese trabajo se hace una
//! vez (`compilar`, al instalar el modelo o la primera vez que se carga) y se guarda en
//! `tokenizer.omoi`, junto al `tokenizer.json`, que no se toca (los ficheros publicados y sus
//! SHA-256 siguen igual). Cargarlo es mapearlo: unos milisegundos.
//!
//! Reproduce lo que hace `tokenizers` con este tokenizer, paso a paso:
//! 1. Corta el texto por los tokens añadidos (`added_tokens`: `<bos>`, `<unused12>`, rachas de
//!    `\n`, de `\t` y de `▁`, etiquetas HTML…), sin normalizar y quedándose con el más largo que
//!    empiece más a la izquierda, como su Aho-Corasick `LeftmostLongest`.
//! 2. En cada trozo restante cambia los espacios por `▁` (el normalizador). El pre-tokenizer corta
//!    por espacios, y después de normalizar ya no queda ninguno: no hace nada.
//! 3. BPE: cada carácter es su pieza o, si no está, sus bytes (`<0x0A>`…), y se fusionan por orden
//!    de prioridad con la misma cola (rango y, a igual rango, la posición más a la izquierda) y la
//!    misma forma de descartar entradas caducadas que `Word::merge_all`.
//! 4. `<bos>` + piezas + `<eos>`, recortado a `max` como su truncado (se quedan las primeras y el
//!    `<eos>`).
//!
//! Si el `tokenizer.json` no es de esta forma, `compilar` dice que no y `gemma` usa `tokenizers`.
//! La equivalencia se comprueba contra `tokenizers` (test `bpe_igual_que_tokenizers` y
//! `examples/bpe_igual.rs`, con miles de textos reales).

use anyhow::{Context, Result, bail, ensure};
use memmap2::Mmap;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

/// El fichero compilado, en la carpeta del modelo.
pub const FICHERO: &str = "tokenizer.omoi";
const MAGIA: &[u8; 8] = b"OMOIBPE\x01";
const VACIO: u64 = u64::MAX;
const NO: u32 = u32::MAX;

/// Un tokenizer compilado y mapeado.
pub struct Bpe {
    mapa: Mmap,
    bos: u32,
    eos: u32,
    /// `(carácter, id)` ordenados por carácter: 8 bytes cada uno desde `chars`.
    chars: (usize, usize),
    bytes: [u32; 256],
    /// Tabla hash de fusiones: `(a << 32 | b) → (rango, nuevo)`, 16 bytes por hueco desde `fusiones`.
    fusiones: (usize, u32),
    añadidos: HashMap<Box<[u8]>, u32>,
    /// Por primer byte: las longitudes de los tokens añadidos que empiezan por él, de mayor a menor.
    largos: Vec<Vec<usize>>,
}

/// Lo que identifica el `tokenizer.json` del que salió: tamaño y fecha. Si cambia, se recompila.
fn huella(json: &Path) -> Result<(u64, u64)> {
    let m = std::fs::metadata(json)?;
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0);
    Ok((m.len(), t))
}

fn u32_en(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_en(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

fn hash(clave: u64, bits: u32) -> usize {
    (clave.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - bits)) as usize
}

/// Compila `json` (un `tokenizer.json`) a `destino`. Falla si no es un BPE como el de Gemma.
pub fn compilar(json: &Path, destino: &Path) -> Result<()> {
    let d: serde_json::Value = serde_json::from_slice(&std::fs::read(json)?)?;
    let m = &d["model"];
    ensure!(m["type"] == "BPE", "not a BPE tokenizer");
    ensure!(m["byte_fallback"] == true, "BPE without byte_fallback");
    for k in ["dropout", "continuing_subword_prefix", "end_of_word_suffix"] {
        ensure!(m[k].is_null(), "BPE with {k}");
    }
    ensure!(m["ignore_merges"] != true, "BPE with ignore_merges");
    ensure!(d["truncation"].is_null() && d["padding"].is_null(), "tokenizer with truncation or padding");
    // Normalizador: cambiar " " por "▁"; pre-tokenizer: cortar por " " (no hace nada tras normalizar).
    let n = &d["normalizer"];
    ensure!(n["type"] == "Replace" && n["pattern"]["String"] == " " && n["content"] == "\u{2581}", "unexpected normalizer");
    let p = &d["pre_tokenizer"];
    ensure!(p.is_null() || (p["type"] == "Split" && p["pattern"]["String"] == " "), "unexpected pre-tokenizer");
    // Plantilla: <bos> A <eos>.
    let pp = &d["post_processor"];
    ensure!(pp["type"] == "TemplateProcessing", "unexpected post-processor");
    let single = pp["single"].as_array().context("post-processor without single")?;
    let especial = |v: &serde_json::Value| -> Option<String> { v["SpecialToken"]["id"].as_str().map(str::to_string) };
    ensure!(single.len() == 3 && single[1]["Sequence"]["id"] == "A", "unexpected template");
    let (bos_s, eos_s) = (especial(&single[0]).context("template")?, especial(&single[2]).context("template")?);
    let id_especial = |s: &str| pp["special_tokens"][s]["ids"][0].as_u64().map(|x| x as u32).with_context(|| format!("special token {s}"));
    let (bos, eos) = (id_especial(&bos_s)?, id_especial(&eos_s)?);

    let vocab = m["vocab"].as_object().context("BPE without vocab")?;
    let mut chars: Vec<(u32, u32)> = Vec::new();
    for (pieza, id) in vocab {
        let mut it = pieza.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            chars.push((c as u32, id.as_u64().context("vocab id")? as u32));
        }
    }
    chars.sort_unstable();
    let mut bytes = [NO; 256];
    for (b, id) in bytes.iter_mut().enumerate() {
        *id = vocab.get(&format!("<0x{b:02X}>")).and_then(|v| v.as_u64()).context("BPE without all 256 byte tokens")? as u32;
    }

    let fusiones = m["merges"].as_array().context("BPE without merges")?;
    let bits = (fusiones.len() * 2).next_power_of_two().trailing_zeros().max(4);
    let mut tabla = vec![(VACIO, 0u32, 0u32); 1 << bits];
    let id_de = |s: &str| vocab.get(s).and_then(|v| v.as_u64()).map(|x| x as u32);
    for (rango, f) in fusiones.iter().enumerate() {
        let (a, b) = match f {
            serde_json::Value::Array(par) if par.len() == 2 => (par[0].as_str().context("merge")?.to_string(), par[1].as_str().context("merge")?.to_string()),
            serde_json::Value::String(s) => {
                let (a, b) = s.split_once(' ').context("merge")?;
                (a.to_string(), b.to_string())
            }
            _ => bail!("unexpected merge {f}"),
        };
        let (ia, ib) = (id_de(&a).context("merge piece not in vocab")?, id_de(&b).context("merge piece not in vocab")?);
        let nuevo = id_de(&format!("{a}{b}")).context("merge result not in vocab")?;
        let clave = (u64::from(ia) << 32) | u64::from(ib);
        let mut h = hash(clave, bits);
        // Como el HashMap de `tokenizers`: una fusión repetida se queda con la última.
        while tabla[h].0 != VACIO && tabla[h].0 != clave {
            h = (h + 1) & ((1 << bits) - 1);
        }
        tabla[h] = (clave, rango as u32, nuevo);
    }

    let mut añadidos: Vec<(u32, &str)> = Vec::new();
    for a in d["added_tokens"].as_array().context("no added_tokens")? {
        ensure!(a["normalized"] != true && a["lstrip"] != true && a["rstrip"] != true && a["single_word"] != true, "added token with options");
        añadidos.push((a["id"].as_u64().context("added id")? as u32, a["content"].as_str().context("added content")?));
    }

    let (tam, fecha) = huella(json)?;
    let mut o: Vec<u8> = Vec::with_capacity(16 << 20);
    o.extend_from_slice(MAGIA);
    o.extend_from_slice(&tam.to_le_bytes());
    o.extend_from_slice(&fecha.to_le_bytes());
    for x in [bos, eos, bits, chars.len() as u32, añadidos.len() as u32] {
        o.extend_from_slice(&x.to_le_bytes());
    }
    for b in bytes {
        o.extend_from_slice(&b.to_le_bytes());
    }
    for (c, id) in &chars {
        o.extend_from_slice(&c.to_le_bytes());
        o.extend_from_slice(&id.to_le_bytes());
    }
    for (clave, rango, nuevo) in &tabla {
        o.extend_from_slice(&clave.to_le_bytes());
        o.extend_from_slice(&rango.to_le_bytes());
        o.extend_from_slice(&nuevo.to_le_bytes());
    }
    for (id, s) in &añadidos {
        o.extend_from_slice(&id.to_le_bytes());
        o.extend_from_slice(&(s.len() as u32).to_le_bytes());
        o.extend_from_slice(s.as_bytes());
    }
    // A un temporal y renombrar: quien lo abra a la vez nunca ve uno a medias.
    let tmp = destino.with_extension(format!("part{}", std::process::id()));
    std::fs::write(&tmp, &o)?;
    std::fs::rename(&tmp, destino).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

impl Bpe {
    /// Abre `destino` si está compilado a partir de este `json`; si no, lo compila (y lo deja escrito
    /// si puede; si la carpeta es de solo lectura, lo compila en un temporal).
    pub fn abrir_o_compilar(json: &Path, destino: &Path) -> Result<Bpe> {
        if let Ok(b) = Self::abrir(json, destino) {
            return Ok(b);
        }
        if compilar(json, destino).is_ok() {
            return Self::abrir(json, destino);
        }
        let tmp = std::env::temp_dir().join(format!("omoi-{}-{FICHERO}", std::process::id()));
        compilar(json, &tmp)?;
        let b = Self::abrir(json, &tmp);
        let _ = std::fs::remove_file(&tmp);
        b
    }

    /// Abre `destino`, comprobando que salió de `json` tal como está.
    pub fn abrir(json: &Path, destino: &Path) -> Result<Bpe> {
        let f = std::fs::File::open(destino)?;
        // SAFETY: se escribe entero a un temporal y se renombra; nadie lo modifica en su sitio.
        let mapa = unsafe { Mmap::map(&f)? };
        let b = &mapa[..];
        ensure!(b.len() >= 44 + 1024 && &b[..8] == MAGIA, "{FICHERO}: not a compiled tokenizer");
        ensure!((u64_en(b, 8), u64_en(b, 16)) == huella(json)?, "{FICHERO}: compiled from another tokenizer.json");
        let (bos, eos, bits, n_chars, n_añadidos) = (u32_en(b, 24), u32_en(b, 28), u32_en(b, 32), u32_en(b, 36) as usize, u32_en(b, 40) as usize);
        let mut bytes = [NO; 256];
        for (i, x) in bytes.iter_mut().enumerate() {
            *x = u32_en(b, 44 + 4 * i);
        }
        let ini_chars = 44 + 1024;
        let ini_fusiones = ini_chars + 8 * n_chars;
        let mut i = ini_fusiones + 16 * (1usize << bits);
        ensure!(i <= b.len(), "{FICHERO}: truncated");
        let mut añadidos = HashMap::with_capacity(n_añadidos);
        let mut largos = vec![Vec::new(); 256];
        for _ in 0..n_añadidos {
            ensure!(i + 8 <= b.len(), "{FICHERO}: truncated");
            let (id, l) = (u32_en(b, i), u32_en(b, i + 4) as usize);
            ensure!(l > 0 && i + 8 + l <= b.len(), "{FICHERO}: truncated");
            let s: Box<[u8]> = b[i + 8..i + 8 + l].into();
            let ls: &mut Vec<usize> = &mut largos[s[0] as usize];
            if !ls.contains(&l) {
                ls.push(l);
            }
            añadidos.insert(s, id);
            i += 8 + l;
        }
        for l in &mut largos {
            l.sort_unstable_by(|a, b| b.cmp(a));
        }
        Ok(Bpe { bos, eos, chars: (ini_chars, n_chars), bytes, fusiones: (ini_fusiones, bits), añadidos, largos, mapa })
    }

    fn char_id(&self, c: char) -> Option<u32> {
        let (ini, n) = self.chars;
        let b = &self.mapa[..];
        let c = c as u32;
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let m = (lo + hi) / 2;
            let k = u32_en(b, ini + 8 * m);
            match k.cmp(&c) {
                std::cmp::Ordering::Less => lo = m + 1,
                std::cmp::Ordering::Greater => hi = m,
                std::cmp::Ordering::Equal => return Some(u32_en(b, ini + 8 * m + 4)),
            }
        }
        None
    }

    /// `(rango, nuevo)` de la fusión de `a` con `b`.
    fn fusion(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (ini, bits) = self.fusiones;
        let m = &self.mapa[..];
        let clave = (u64::from(a) << 32) | u64::from(b);
        let mut h = hash(clave, bits);
        loop {
            let p = ini + 16 * h;
            let k = u64_en(m, p);
            if k == clave {
                return Some((u32_en(m, p + 8), u32_en(m, p + 12)));
            }
            if k == VACIO {
                return None;
            }
            h = (h + 1) & ((1 << bits) - 1);
        }
    }

    /// El token añadido más largo que empieza en `t[0]`, con su longitud en bytes.
    fn añadido(&self, t: &[u8]) -> Option<(usize, u32)> {
        for &l in &self.largos[t[0] as usize] {
            if l <= t.len()
                && let Some(&id) = self.añadidos.get(&t[..l])
            {
                return Some((l, id));
            }
        }
        None
    }

    /// Los ids de `texto` con `<bos>` y `<eos>`, como mucho `max` (contándolos).
    pub fn codificar(&self, texto: &str, max: usize) -> Vec<u32> {
        let mut ids = vec![self.bos];
        let b = texto.as_bytes();
        let (mut i, mut inicio) = (0, 0);
        while i < b.len() {
            // Todos los tokens añadidos son UTF-8 válido: solo pueden empezar donde empieza un carácter.
            if !self.largos[b[i] as usize].is_empty()
                && let Some((l, id)) = self.añadido(&b[i..])
            {
                self.trozo(&texto[inicio..i], &mut ids);
                ids.push(id);
                i += l;
                inicio = i;
                continue;
            }
            i += 1;
        }
        self.trozo(&texto[inicio..], &mut ids);
        if ids.len() + 1 > max {
            ids.truncate(max.max(2) - 1);
        }
        ids.push(self.eos);
        ids
    }

    /// BPE de un trozo sin tokens añadidos (`Word::merge_all` de `tokenizers`).
    fn trozo(&self, t: &str, ids: &mut Vec<u32>) {
        if t.is_empty() {
            return;
        }
        #[derive(Clone, Copy)]
        struct S {
            c: u32,
            prev: isize,
            next: isize,
            vivo: bool,
        }
        let mut s: Vec<S> = Vec::with_capacity(t.len());
        let añadir = |c: u32, s: &mut Vec<S>| {
            let i = s.len() as isize;
            if let Some(u) = s.last_mut() {
                u.next = i;
            }
            s.push(S { c, prev: i - 1, next: -1, vivo: true });
        };
        let mut buf = [0u8; 4];
        for c in t.chars() {
            let c = if c == ' ' { '\u{2581}' } else { c };
            match self.char_id(c) {
                Some(id) => añadir(id, &mut s),
                None => {
                    for &byte in c.encode_utf8(&mut buf).as_bytes() {
                        añadir(self.bytes[byte as usize], &mut s);
                    }
                }
            }
        }
        // Montículo de mínimos por (rango, posición).
        #[derive(PartialEq, Eq)]
        struct F {
            rango: u32,
            pos: usize,
            nuevo: u32,
        }
        impl Ord for F {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.rango.cmp(&self.rango).then(o.pos.cmp(&self.pos))
            }
        }
        impl PartialOrd for F {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        let mut cola = BinaryHeap::with_capacity(s.len());
        for i in 0..s.len().saturating_sub(1) {
            if let Some((rango, nuevo)) = self.fusion(s[i].c, s[i + 1].c) {
                cola.push(F { rango, pos: i, nuevo });
            }
        }
        while let Some(f) = cola.pop() {
            let actual = s[f.pos];
            if !actual.vivo || actual.next == -1 {
                continue;
            }
            let sig = actual.next as usize;
            let derecha = s[sig];
            // Una entrada caducada: el par ya no es el que era.
            if self.fusion(actual.c, derecha.c).is_none_or(|(_, n)| n != f.nuevo) {
                continue;
            }
            s[f.pos].c = f.nuevo;
            s[f.pos].next = derecha.next;
            s[sig].vivo = false;
            if derecha.next > -1 && (derecha.next as usize) < s.len() {
                s[derecha.next as usize].prev = f.pos as isize;
            }
            let actual = s[f.pos];
            if actual.prev >= 0
                && let Some((rango, nuevo)) = self.fusion(s[actual.prev as usize].c, actual.c)
            {
                cola.push(F { rango, pos: actual.prev as usize, nuevo });
            }
            let n = actual.next;
            if n >= 0
                && (n as usize) < s.len()
                && let Some((rango, nuevo)) = self.fusion(actual.c, s[n as usize].c)
            {
                cola.push(F { rango, pos: f.pos, nuevo });
            }
        }
        ids.extend(s.iter().filter(|x| x.vivo).map(|x| x.c));
    }
}

