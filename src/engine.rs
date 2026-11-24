//! EmbeddingGemma's forward pass without candle: the engine the service uses by default.
//!
//! With few tokens (a query is 10-60) the cost is streaming the ~106 M weights through the CPU once
//! per query. candle kept them in f32 (424 MB) and its matmul is tuned for large matrices. Here the
//! weights stay in f16, as they come in `pesos.safetensors`, and are read straight from the
//! memory-mapped file (they take no RAM of the process's own: the disk cache shares it). Each
//! matmul is blocked (2 rows × 6 tokens in registers, with AVX2/FMA/F16C) and the rows are split
//! across the threads of a dedicated pool. Speed review (branch rev-velocidad): the vector of a
//! 24-token query went from 55 to 30 ms, same vector.
//!
//! - **Long texts**: attention is blocked too (6 queries against 2 keys at a time, and the same with
//!   the values). The direct attention of the first version made texts above ~600 tokens slower
//!   than candle.
//! - **Batches**: `vectors` lays the tokens of several texts one after another and runs each matmul
//!   once for all of them (matmuls don't mix tokens; attention runs per text). Same vector as one at
//!   a time, bit for bit: every product is summed in the same order.
//! - **Without AVX2** (old CPUs, ARM): the generic path (`simd = false`) does the same row by row,
//!   with 8-lane sums the compiler vectorizes (SSE2, NEON). It gives the same vector (cosine
//!   ≥0.9999 against candle; test `motor`) but is slower than candle, so `Gemma::load` uses candle
//!   on those CPUs and this path only with `OMOIKANE_GEMMA_RAPIDO=1`.

use rayon::prelude::*;
use std::sync::Arc;

/// Something that keeps alive the data a `Mat` points to (a memory-mapped file or a `Vec`).
type Owner = Arc<dyn Send + Sync>;

/// A run of consecutive rows of an f16 matrix.
struct Part {
    ptr: *const u16,
    rows: usize,
}

/// A weight matrix (outputs × inputs) in f16, row-major as PyTorch stores it. It can be made of
/// several matrices laid one after another (q, k and v; gate and up), each wherever it lives.
pub struct Mat {
    pub rows: usize,
    cols: usize,
    parts: Vec<Part>,
    // Only read on x86_64 (AVX2); on ARM the path is the generic one.
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    simd: bool,
    _owners: Vec<Owner>,
}

// SAFETY: the pointers point to data that never changes and that `_owners` keeps alive.
unsafe impl Send for Mat {}
unsafe impl Sync for Mat {}

impl Mat {
    /// A matrix made of the parts `(data, rows, owner)` one after another. `data` has to hold
    /// `rows · cols` f16 values (their bits) and live as long as `owner`.
    ///
    /// # Safety
    /// Each pointer has to point to `rows · cols` aligned u16 that don't change while its `owner`
    /// is alive.
    pub unsafe fn from_parts(parts: Vec<(*const u16, usize, Owner)>, cols: usize, simd: bool) -> Mat {
        let rows = parts.iter().map(|p| p.1).sum();
        let mut ps = Vec::new();
        let mut owners = Vec::new();
        for (ptr, r, owner) in parts {
            assert!((ptr as usize).is_multiple_of(2));
            ps.push(Part { ptr, rows: r });
            owners.push(owner);
        }
        Mat { rows, cols, parts: ps, simd, _owners: owners }
    }

    /// An owned matrix from f32 values (stored as f16).
    pub fn from_f32(data: &[f32], rows: usize, cols: usize, simd: bool) -> Mat {
        assert_eq!(data.len(), rows * cols);
        let v: Arc<Vec<u16>> = Arc::new(data.iter().map(|x| half::f16::from_f32(*x).to_bits()).collect());
        // SAFETY: `v` holds rows·cols u16 and goes in as the owner.
        unsafe { Mat::from_parts(vec![(v.as_ptr(), rows, v.clone() as Owner)], cols, simd) }
    }

    pub fn bytes(&self) -> usize {
        self.rows * self.cols * 2
    }

    /// Where row `r` starts.
    #[inline]
    fn row_ptr(&self, mut r: usize) -> *const u16 {
        for p in &self.parts {
            if r < p.rows {
                // SAFETY: r < the part's rows.
                return unsafe { p.ptr.add(r * self.cols) };
            }
            r -= p.rows;
        }
        panic!("row out of the matrix");
    }

    /// Do all parts have an even number of rows? (the AVX2 block reads two consecutive rows).
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    fn even_parts(&self) -> bool {
        self.parts.iter().all(|p| p.rows.is_multiple_of(2))
    }

    /// `x · Wᵀ`: `x` is n × cols, the result n × rows.
    pub fn mul(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        debug_assert_eq!(x.len(), n * self.cols);
        #[cfg(target_arch = "x86_64")]
        if self.simd && self.cols.is_multiple_of(8) && self.even_parts() {
            return self.mul_blocked(x, n, pool);
        }
        self.mul_rows(x, n, pool)
    }

    /// Blocked: 2 rows × 6 tokens at a time (12 sums in registers), the 6 tokens in L1 and each
    /// thread's block of rows in L2.
    #[cfg(target_arch = "x86_64")]
    fn mul_blocked(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        const T: usize = x86::T;
        let k = self.cols;
        let np = n.div_ceil(T) * T;
        // The tokens missing to fill the last block, as zeros (their outputs are dropped).
        let padded;
        let x = if np == n {
            x
        } else {
            let mut v = Vec::with_capacity(np * k);
            v.extend_from_slice(x);
            v.resize(np * k, 0.0);
            padded = v;
            &padded[..]
        };
        // The output goes straight token by token (n × rows): each task writes its block of rows for
        // its span of tokens, never overlapping another one.
        let mut y = vec![0f32; np * self.rows];
        let threads = pool.current_num_threads().max(1);
        // Blocks of rows that fit in L2 (~128 KB of weights), at least two per thread, and even; and
        // spans of tokens, so that big batches give work to every thread.
        let block = (self.rows.div_ceil(threads * 2).min((128 << 10) / (k * 2)).max(2) + 1) & !1;
        let span = 16 * T;
        let (nb, ns) = (self.rows.div_ceil(block), np.div_ceil(span));
        let out = SendPtr(y.as_mut_ptr());
        let total_rows = self.rows;
        let run = |task: usize| {
            let (b, c) = (task % nb, task / nb);
            let (r0, r1) = (b * block, ((b + 1) * block).min(total_rows));
            let (t0, t1) = (c * span, ((c + 1) * span).min(np));
            let out = &out;
            for t in (t0..t1).step_by(T) {
                for r in (r0..r1).step_by(2) {
                    // SAFETY: AVX2/FMA/F16C checked at load time; r is even and its part has an even
                    // number of rows, so the next row is right behind it; t + T <= np; each task
                    // writes only its own rows of its own tokens.
                    unsafe {
                        let w = self.row_ptr(r);
                        x86::block_f16(w, k, x.as_ptr().add(t * k), k, k, out.0.add(t * total_rows + r), 1, total_rows);
                    }
                }
            }
        };
        if threads == 1 || np * self.rows < 4096 {
            (0..nb * ns).for_each(run);
        } else {
            pool.install(|| (0..nb * ns).into_par_iter().for_each(run));
        }
        y.truncate(n * self.rows);
        y
    }

    /// Row by row (the path without AVX2): each row is converted to f32 and multiplied by each token.
    pub fn mul_rows(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        let k = self.cols;
        let mut yt = vec![0f32; self.rows * n];
        let threads = pool.current_num_threads().max(1);
        let block = self.rows.div_ceil(threads * 4).max(8);
        let run = |(b, chunk): (usize, &mut [f32])| {
            let mut buf = vec![0f32; k];
            for (i, out) in chunk.chunks_mut(n).enumerate() {
                let p = self.row_ptr(b * block + i);
                for (j, v) in buf.iter_mut().enumerate() {
                    // SAFETY: j < cols, inside the row.
                    *v = half::f16::from_bits(unsafe { *p.add(j) }).to_f32();
                }
                for (t, o) in out.iter_mut().enumerate() {
                    *o = dot_generic(&buf, &x[t * k..(t + 1) * k]);
                }
            }
        };
        if threads == 1 || n * self.rows < 4096 {
            yt.chunks_mut(block * n).enumerate().for_each(run);
        } else {
            pool.install(|| yt.par_chunks_mut(block * n).enumerate().for_each(run));
        }
        transpose(&yt, self.rows, n, n)
    }
}

/// A pointer that can be shared between threads that write to different places.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
struct SendPtr(*mut f32);
// SAFETY: each task writes to positions of its own (see `mul_blocked`).
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

/// `yt` is rows × stride (per row, its tokens); returns the first `n` tokens × rows.
fn transpose(yt: &[f32], rows: usize, stride: usize, n: usize) -> Vec<f32> {
    if n == 1 {
        return (0..rows).map(|r| yt[r * stride]).collect();
    }
    let mut y = vec![0f32; n * rows];
    for r in 0..rows {
        for t in 0..n {
            y[t * rows + r] = yt[r * stride + t];
        }
    }
    y
}

/// Does this CPU have AVX2, FMA and F16C? `OMOIKANE_GEMMA_SIMD=0` pretends it doesn't (to test the
/// generic path on one that does).
pub fn simd_available() -> bool {
    if std::env::var("OMOIKANE_GEMMA_SIMD").as_deref() == Ok("0") {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") && std::is_x86_feature_detected!("f16c")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Dot product with 8 partial sums (the compiler vectorizes it with whatever the base CPU has).
fn dot_generic(a: &[f32], b: &[f32]) -> f32 {
    let mut s = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            s[i] += x[i] * y[i];
        }
    }
    s.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// Horizontal sum of the 8 lanes.
    #[target_feature(enable = "avx2,fma")]
    fn hsum(v: __m256) -> f32 {
        let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        _mm_cvtss_f32(_mm_add_ss(s, _mm_shuffle_ps(s, s, 1)))
    }

    /// Tokens per block.
    pub const T: usize = 6;

    macro_rules! block {
        ($name:ident, $t:ty, $feat:literal, $load:expr) => {
            /// `out[i·si + t·st] = w[i] · x[t]` for 2 rows of `w` (the second `sw` after the first)
            /// and `T` rows of `x` (`sx` apart), of `k` values (a multiple of 8).
            #[target_feature(enable = $feat)]
            #[allow(clippy::too_many_arguments)]
            pub unsafe fn $name(w: *const $t, sw: usize, x: *const f32, sx: usize, k: usize, out: *mut f32, si: usize, st: usize) {
                unsafe {
                    let load = $load;
                    let mut a = [[_mm256_setzero_ps(); T]; 2];
                    let mut j = 0;
                    while j < k {
                        let w0 = load(w, j);
                        let w1 = load(w.add(sw), j);
                        for t in 0..T {
                            let xv = _mm256_loadu_ps(x.add(t * sx + j));
                            a[0][t] = _mm256_fmadd_ps(w0, xv, a[0][t]);
                            a[1][t] = _mm256_fmadd_ps(w1, xv, a[1][t]);
                        }
                        j += 8;
                    }
                    for i in 0..2 {
                        for t in 0..T {
                            *out.add(i * si + t * st) = hsum(a[i][t]);
                        }
                    }
                }
            }
        };
    }

    block!(block_f32, f32, "avx2,fma", |w: *const f32, j: usize| _mm256_loadu_ps(w.add(j)));
    block!(block_f16, u16, "avx2,fma,f16c", |w: *const u16, j: usize| _mm256_cvtph_ps(_mm_loadu_si128(w.add(j) as *const __m128i)));
}

/// One transformer layer.
pub struct Layer {
    /// Norm weights, with Gemma's `1 +` already added: before attention, after attention, before
    /// the MLP, after the MLP, and the per-head norms of q and k.
    pub norm_in: Vec<f32>,
    pub norm_post_attn: Vec<f32>,
    pub norm_pre_mlp: Vec<f32>,
    pub norm_post_mlp: Vec<f32>,
    pub norm_q: Vec<f32>,
    pub norm_k: Vec<f32>,
    /// q, k and v one after another (768 + 256 + 256 rows).
    pub qkv: Mat,
    pub o: Mat,
    /// Gate and up one after another (1152 + 1152 rows).
    pub gate_up: Mat,
    pub down: Mat,
    /// Sliding-window layer (local RoPE base, limited attention).
    pub local: bool,
}

/// The whole model, ready to turn input vectors into an embedding.
pub struct Engine {
    pub layers: Vec<Layer>,
    pub norm: Vec<f32>,
    pub dense1: Mat,
    pub dense2: Mat,
    pub eps: f32,
    pub heads: usize,
    pub kv_heads: usize,
    pub head_dim: usize,
    pub scale: f32,
    /// Half the window of the local layers: each token sees those at this distance or less.
    pub half_window: usize,
    /// RoPE cos and sin per position (MAX × head_dim/2), local and global.
    pub rope_local: (Vec<f32>, Vec<f32>),
    pub rope_global: (Vec<f32>, Vec<f32>),
    /// AVX2/FMA/F16C (otherwise the generic path).
    pub simd: bool,
}

/// Like `Gemma::rope`, in f32 the same as candle.
pub fn rope_table(max: usize, d: usize, base: f64) -> (Vec<f32>, Vec<f32>) {
    let inv: Vec<f32> = (0..d / 2).map(|i| (1.0 / base.powf(2.0 * i as f64 / d as f64)) as f32).collect();
    let (mut c, mut s) = (Vec::with_capacity(max * d / 2), Vec::with_capacity(max * d / 2));
    for p in 0..max {
        for f in &inv {
            let a = p as f32 * f;
            c.push(a.cos());
            s.push(a.sin());
        }
    }
    (c, s)
}

fn rms_in_place(x: &mut [f32], w: &[f32], eps: f32) {
    let m = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let r = 1.0 / (m + eps).sqrt();
    x.iter_mut().zip(w).for_each(|(v, w)| *v = *v * r * w);
}

fn rms_rows(x: &[f32], w: &[f32], eps: f32, pool: &rayon::ThreadPool) -> Vec<f32> {
    let mut y = x.to_vec();
    if x.len() / w.len() < 64 {
        y.chunks_mut(w.len()).for_each(|r| rms_in_place(r, w, eps));
    } else {
        pool.install(|| y.par_chunks_mut(w.len()).for_each(|r| rms_in_place(r, w, eps)));
    }
    y
}

/// `x += rms(o)` row by row (the residual after attention or the MLP).
fn add_rms(x: &mut [f32], o: &mut [f32], w: &[f32], eps: f32, pool: &rayon::ThreadPool) {
    let d = w.len();
    let f = |(x, o): (&mut [f32], &mut [f32])| {
        rms_in_place(o, w, eps);
        x.iter_mut().zip(o.iter()).for_each(|(x, o)| *x += o);
    };
    if x.len() / d < 64 {
        x.chunks_mut(d).zip(o.chunks_mut(d)).for_each(f);
    } else {
        pool.install(|| x.par_chunks_mut(d).zip(o.par_chunks_mut(d)).for_each(f));
    }
}

fn apply_rope(v: &mut [f32], cos: &[f32], sin: &[f32]) {
    let h = v.len() / 2;
    for i in 0..h {
        let (a, b) = (v[i], v[i + h]);
        v[i] = a * cos[i] - b * sin[i];
        v[i + h] = b * cos[i] + a * sin[i];
    }
}

fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + ((2.0f32 / std::f32::consts::PI).sqrt() * (x + 0.044715 * x * x * x)).tanh())
}

/// What attention needs to know about a text: where it starts in the batch and how many tokens it has.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    n: usize,
}

impl Engine {
    /// The (normalized) vector of a text from the input vectors of its `n` tokens.
    pub fn vector(&self, x: Vec<f32>, n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        self.vectors(x, &[n], pool, &|| {}).pop().unwrap_or_default()
    }

    /// The vectors of several texts at once: `x` holds the input vectors of all their tokens, one
    /// after another, and `lens` how many tokens each text has. `yield_now` is called before each
    /// step (about 100 times per vector): indexing uses it to let a query through.
    pub fn vectors(&self, mut x: Vec<f32>, lens: &[usize], pool: &rayon::ThreadPool, yield_now: &dyn Fn()) -> Vec<Vec<f32>> {
        let d = self.norm.len();
        let total: usize = lens.iter().sum();
        debug_assert_eq!(x.len(), total * d);
        if total == 0 {
            return lens.iter().map(|_| vec![0.0; d]).collect();
        }
        let (h, kv, hd) = (self.heads, self.kv_heads, self.head_dim);
        let (dq, dk) = (h * hd, kv * hd);
        let width = dq + 2 * dk;
        let half = hd / 2;
        let mut spans = Vec::with_capacity(lens.len());
        let mut pos = Vec::with_capacity(total);
        for &n in lens {
            spans.push(Span { start: pos.len(), n });
            pos.extend(0..n);
        }
        let mut attn = vec![0f32; total * dq];
        for l in &self.layers {
            yield_now();
            let e = rms_rows(&x, &l.norm_in, self.eps, pool);
            let mut qkv = l.qkv.mul(&e, total, pool);
            let (cos, sin) = if l.local { &self.rope_local } else { &self.rope_global };
            let rope_row = |(t, r): (usize, &mut [f32])| {
                let p = pos[t];
                let (cs, sn) = (&cos[p * half..(p + 1) * half], &sin[p * half..(p + 1) * half]);
                for j in 0..h + kv {
                    let v = &mut r[j * hd..(j + 1) * hd];
                    rms_in_place(v, if j < h { &l.norm_q } else { &l.norm_k }, self.eps);
                    apply_rope(v, cs, sn);
                }
            };
            if total < 64 {
                qkv.chunks_mut(width).enumerate().for_each(rope_row);
            } else {
                pool.install(|| qkv.par_chunks_mut(width).enumerate().for_each(rope_row));
            }
            yield_now();
            self.attention(&qkv, &spans, l.local, &mut attn, pool);
            yield_now();
            let mut o = l.o.mul(&attn, total, pool);
            add_rms(&mut x, &mut o, &l.norm_post_attn, self.eps, pool);
            let e = rms_rows(&x, &l.norm_pre_mlp, self.eps, pool);
            yield_now();
            let gu = l.gate_up.mul(&e, total, pool);
            let inter = l.gate_up.rows / 2;
            let mut m = vec![0f32; total * inter];
            // GELU's tanh is expensive: split across the threads in chunks of 256.
            let gelu_chunk = |(c, chunk): (usize, &mut [f32])| {
                for (j, o) in chunk.iter_mut().enumerate() {
                    let (t, i) = ((c * 256 + j) / inter, (c * 256 + j) % inter);
                    let r = &gu[t * 2 * inter..];
                    *o = gelu(r[i]) * r[inter + i];
                }
            };
            pool.install(|| m.par_chunks_mut(256).enumerate().for_each(gelu_chunk));
            yield_now();
            let mut b = l.down.mul(&m, total, pool);
            add_rms(&mut x, &mut b, &l.norm_post_mlp, self.eps, pool);
        }
        let x = rms_rows(&x, &self.norm, self.eps, pool);
        let mut means = vec![0f32; lens.len() * d];
        for (sp, mean) in spans.iter().zip(means.chunks_mut(d)) {
            for r in x[sp.start * d..(sp.start + sp.n) * d].chunks(d) {
                mean.iter_mut().zip(r).for_each(|(m, v)| *m += v);
            }
            mean.iter_mut().for_each(|m| *m /= sp.n.max(1) as f32);
        }
        let y = self.dense1.mul(&means, lens.len(), pool);
        let y = self.dense2.mul(&y, lens.len(), pool);
        y.chunks(self.dense2.rows)
            .map(|v| {
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.iter().map(|x| x / norm).collect()
            })
            .collect()
    }

    /// Bidirectional attention for each text of the batch; in the local layers each token only sees
    /// those at `half_window` or less.
    fn attention(&self, qkv: &[f32], spans: &[Span], local: bool, attn: &mut [f32], pool: &rayon::ThreadPool) {
        #[cfg(target_arch = "x86_64")]
        if self.simd && self.head_dim.is_multiple_of(8) {
            return self.attention_blocked(qkv, spans, local, attn, pool);
        }
        self.attention_direct(qkv, spans, local, attn, pool);
    }

    /// Row by row: each query against each key (the path without AVX2).
    fn attention_direct(&self, qkv: &[f32], spans: &[Span], local: bool, attn: &mut [f32], pool: &rayon::ThreadPool) {
        let (h, kv, hd) = (self.heads, self.kv_heads, self.head_dim);
        let (dq, dk) = (h * hd, kv * hd);
        let width = dq + 2 * dk;
        let group = h / kv;
        let total: usize = spans.iter().map(|s| s.n).sum();
        // For each row of `attn`, the span it belongs to.
        let span_of: Vec<Span> = spans.iter().flat_map(|s| std::iter::repeat_n(*s, s.n)).collect();
        let attn_row = |(g, row): (usize, &mut [f32])| {
            let sp = span_of[g];
            let (i, qkv) = (g - sp.start, &qkv[sp.start * width..(sp.start + sp.n) * width]);
            let n = sp.n;
            let windowed = local && n > self.half_window + 1;
            let mut weights = vec![0f32; n];
            row.iter_mut().for_each(|a| *a = 0.0);
            let (j0, j1) = if windowed { (i.saturating_sub(self.half_window), (i + self.half_window + 1).min(n)) } else { (0, n) };
            for head in 0..h {
                let q = &qkv[i * width + head * hd..i * width + (head + 1) * hd];
                let ck = dq + (head / group) * hd;
                let cv = dq + dk + (head / group) * hd;
                let mut max = f32::NEG_INFINITY;
                for j in j0..j1 {
                    let p = dot_generic(q, &qkv[j * width + ck..j * width + ck + hd]) * self.scale;
                    weights[j] = p;
                    max = max.max(p);
                }
                let mut s = 0.0;
                for p in &mut weights[j0..j1] {
                    *p = (*p - max).exp();
                    s += *p;
                }
                let o = &mut row[head * hd..(head + 1) * hd];
                for j in j0..j1 {
                    let a = weights[j] / s;
                    let v = &qkv[j * width + cv..j * width + cv + hd];
                    o.iter_mut().zip(v).for_each(|(o, v)| *o += a * v);
                }
            }
        };
        // With few tokens, splitting it isn't worth it.
        if total < 64 {
            attn.chunks_mut(dq).enumerate().for_each(attn_row);
        } else {
            pool.install(|| attn.par_chunks_mut(dq).enumerate().for_each(attn_row));
        }
    }

    /// In blocks of 6 queries: each block's dot products against its keys, with the same kernel as
    /// the matmuls (the keys act as rows), and the same with the transposed values.
    #[cfg(target_arch = "x86_64")]
    fn attention_blocked(&self, qkv: &[f32], spans: &[Span], local: bool, attn: &mut [f32], pool: &rayon::ThreadPool) {
        const T: usize = x86::T;
        let (h, kv, hd) = (self.heads, self.kv_heads, self.head_dim);
        let (dq, dk) = (h * hd, kv * hd);
        let width = dq + 2 * dk;
        let group = h / kv;
        // Per text: Q per head (rows padded to a multiple of T), K per head (rows padded to a
        // multiple of 8) and Vᵀ per head (columns padded to a multiple of 8); the padding is zeros.
        struct Prep {
            n: usize,
            n8: usize,
            nq: usize,
            q: Vec<f32>,
            k: Vec<f32>,
            vt: Vec<f32>,
        }
        let preps: Vec<Prep> = spans
            .iter()
            .map(|sp| {
                let n = sp.n;
                let (n8, nq) = (n.div_ceil(8) * 8, n.div_ceil(T) * T);
                let mut q = vec![0f32; h * nq * hd];
                let mut k = vec![0f32; kv * n8 * hd];
                let mut vt = vec![0f32; kv * hd * n8];
                for t in 0..n {
                    let r = &qkv[(sp.start + t) * width..(sp.start + t + 1) * width];
                    for c in 0..h {
                        q[(c * nq + t) * hd..(c * nq + t + 1) * hd].copy_from_slice(&r[c * hd..(c + 1) * hd]);
                    }
                    for c in 0..kv {
                        k[(c * n8 + t) * hd..(c * n8 + t + 1) * hd].copy_from_slice(&r[dq + c * hd..dq + (c + 1) * hd]);
                        for (dd, v) in r[dq + dk + c * hd..dq + dk + (c + 1) * hd].iter().enumerate() {
                            vt[(c * hd + dd) * n8 + t] = *v;
                        }
                    }
                }
                Prep { n, n8, nq, q, k, vt }
            })
            .collect();
        // The tasks: each block of T rows of each text, with its slice of `attn`.
        let mut tasks: Vec<(usize, usize, &mut [f32])> = Vec::new();
        let mut rest = attn;
        for (i, sp) in spans.iter().enumerate() {
            let (this, other) = std::mem::take(&mut rest).split_at_mut(sp.n * dq);
            rest = other;
            for (b, chunk) in this.chunks_mut(T * dq).enumerate() {
                tasks.push((i, b * T, chunk));
            }
        }
        let run = |(i, t0, chunk): (usize, usize, &mut [f32])| {
            let p = &preps[i];
            let n = p.n;
            let real = chunk.len() / dq;
            let windowed = local && n > self.half_window + 1;
            let (mut j0, j1) = if windowed { (t0.saturating_sub(self.half_window), (t0 + real + self.half_window).min(n)) } else { (0, n) };
            let len = (j1 - j0).div_ceil(8) * 8;
            if j0 + len > p.n8 {
                j0 = p.n8 - len;
            }
            let mut st = vec![0f32; len * T];
            let mut pr = vec![0f32; T * len];
            let mut o = vec![0f32; hd * T];
            for head in 0..h {
                let kc = &p.k[(head / group) * p.n8 * hd..];
                let vt = &p.vt[(head / group) * hd * p.n8..];
                let q = &p.q[(head * p.nq + t0) * hd..];
                // Dot products: st[j·T + t] = k[j0 + j] · q[t].
                for j in (0..len).step_by(2) {
                    // SAFETY: AVX2/FMA checked; j0 + j + 1 < n8 rows of k; T rows of q from t0 < nq.
                    unsafe { x86::block_f32(kc.as_ptr().add((j0 + j) * hd), hd, q.as_ptr(), hd, hd, st.as_mut_ptr().add(j * T), T, 1) };
                }
                // Softmax of each query over the keys it sees; the rest, zero.
                for t in 0..T {
                    let row = &mut pr[t * len..(t + 1) * len];
                    row.iter_mut().for_each(|v| *v = 0.0);
                    if t >= real {
                        continue;
                    }
                    let i = t0 + t;
                    let (a, b) = if windowed { (i.saturating_sub(self.half_window).max(j0), (i + self.half_window + 1).min(n)) } else { (0, n) };
                    let mut max = f32::NEG_INFINITY;
                    for j in a..b {
                        max = max.max(st[(j - j0) * T + t] * self.scale);
                    }
                    let mut s = 0.0;
                    for j in a..b {
                        let e = (st[(j - j0) * T + t] * self.scale - max).exp();
                        row[j - j0] = e;
                        s += e;
                    }
                    row[a - j0..b - j0].iter_mut().for_each(|v| *v /= s);
                }
                // Output: o[d·T + t] = vᵀ[d][j0..] · pr[t].
                for dd in (0..hd).step_by(2) {
                    // SAFETY: AVX2/FMA checked; dd + 1 < hd rows of vᵀ with n8 columns, j0 + len <= n8.
                    unsafe { x86::block_f32(vt.as_ptr().add(dd * p.n8 + j0), p.n8, pr.as_ptr(), len, len, o.as_mut_ptr().add(dd * T), T, 1) };
                }
                for t in 0..real {
                    for dd in 0..hd {
                        chunk[t * dq + head * hd + dd] = o[dd * T + t];
                    }
                }
            }
        };
        let total: usize = spans.iter().map(|s| s.n).sum();
        if total < 64 {
            tasks.into_iter().for_each(run);
        } else {
            pool.install(|| tasks.into_par_iter().for_each(run));
        }
    }

    pub fn bytes(&self) -> usize {
        self.layers.iter().map(|l| l.qkv.bytes() + l.o.bytes() + l.gate_up.bytes() + l.down.bytes()).sum::<usize>() + self.dense1.bytes() + self.dense2.bytes()
    }
}
