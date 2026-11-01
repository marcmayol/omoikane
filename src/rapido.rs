//! El cálculo de EmbeddingGemma sin candle: el que usa el servicio por defecto.
//!
//! Con pocos tokens (una pregunta son 10-60) lo que cuesta es pasar los ~106 M de pesos por la CPU
//! una vez por pregunta. candle los tenía en f32 (424 MB) y su multiplicación está pensada para
//! matrices grandes. Aquí los pesos se quedan en f16, tal como vienen en `pesos.safetensors`, y se
//! leen directamente del fichero mapeado (no ocupan RAM propia del proceso: la comparte la caché de
//! disco). Cada multiplicación va por bloques (2 filas × 6 tokens en registros, con AVX2/FMA/F16C) y
//! las filas se reparten entre los hilos de un pool propio. Revisión de velocidad (rama
//! rev-velocidad): vector de una pregunta de 24 tokens de 55 a 30 ms, mismo vector.
//!
//! - **Textos largos**: la atención también va por bloques (6 consultas contra 2 claves a la vez, y
//!   lo mismo con los valores). La atención directa de la primera versión hacía que por encima de
//!   ~600 tokens fuese más lenta que candle.
//! - **Por lotes**: `vectores` pone los tokens de varios textos uno tras otro y hace cada
//!   multiplicación de una vez para todos (las multiplicaciones no mezclan tokens; la atención va por
//!   texto). Mismo vector que de uno en uno, bit a bit: cada producto se suma en el mismo orden.
//! - **Sin AVX2** (CPU antiguas, ARM): el camino genérico (`simd = false`) hace lo mismo fila a fila
//!   con sumas de 8 en 8 que el compilador vectoriza (SSE2, NEON). Da el mismo vector (coseno
//!   ≥0,9999 con candle; test `motor`), pero es más lento que candle, así que `Gemma::cargar` usa
//!   candle en esas CPU y este camino solo con `OMOIKANE_GEMMA_RAPIDO=1`.

use rayon::prelude::*;
use std::sync::Arc;

/// Algo que mantiene vivos los datos a los que apunta una `Mat` (un fichero mapeado o un `Vec`).
type Dueño = Arc<dyn Send + Sync>;

/// Un trozo de filas seguidas de una matriz en f16.
struct Parte {
    ptr: *const u16,
    filas: usize,
}

/// Una matriz de pesos (salidas × entradas) en f16, fila a fila como la guarda PyTorch. Puede estar
/// hecha de varias matrices puestas una tras otra (q, k y v; puerta y subir), cada una donde esté.
pub struct Mat {
    pub filas: usize,
    cols: usize,
    partes: Vec<Parte>,
    simd: bool,
    _dueños: Vec<Dueño>,
}

// SAFETY: los punteros apuntan a datos que no cambian nunca y que `_dueños` mantiene vivos.
unsafe impl Send for Mat {}
unsafe impl Sync for Mat {}

impl Mat {
    /// Una matriz con las partes `(datos, filas)` una tras otra. `datos` tiene que tener `filas · cols`
    /// valores f16 (sus bits) y vivir lo que viva `dueño`.
    ///
    /// # Safety
    /// Cada puntero tiene que apuntar a `filas · cols` u16 alineados que no cambien mientras viva
    /// el `dueño` correspondiente.
    pub unsafe fn de_partes(partes: Vec<(*const u16, usize, Dueño)>, cols: usize, simd: bool) -> Mat {
        let filas = partes.iter().map(|p| p.1).sum();
        let mut ps = Vec::new();
        let mut dueños = Vec::new();
        for (ptr, f, d) in partes {
            assert!((ptr as usize).is_multiple_of(2));
            ps.push(Parte { ptr, filas: f });
            dueños.push(d);
        }
        Mat { filas, cols, partes: ps, simd, _dueños: dueños }
    }

    /// Una matriz propia a partir de valores f32 (se guardan en f16).
    pub fn de_f32(datos: &[f32], filas: usize, cols: usize, simd: bool) -> Mat {
        assert_eq!(datos.len(), filas * cols);
        let v: Arc<Vec<u16>> = Arc::new(datos.iter().map(|x| half::f16::from_f32(*x).to_bits()).collect());
        // SAFETY: `v` tiene filas·cols u16 y va como dueño.
        unsafe { Mat::de_partes(vec![(v.as_ptr(), filas, v.clone() as Dueño)], cols, simd) }
    }

    pub fn bytes(&self) -> usize {
        self.filas * self.cols * 2
    }

    /// Dónde empieza la fila `r`.
    #[inline]
    fn fila_ptr(&self, mut r: usize) -> *const u16 {
        for p in &self.partes {
            if r < p.filas {
                // SAFETY: r < filas de la parte.
                return unsafe { p.ptr.add(r * self.cols) };
            }
            r -= p.filas;
        }
        panic!("row out of the matrix");
    }

    /// ¿Van todas las partes por pares de filas? (el bloque de AVX2 lee dos filas seguidas).
    fn pares(&self) -> bool {
        self.partes.iter().all(|p| p.filas.is_multiple_of(2))
    }

    /// `x · Wᵀ`: `x` es n × cols, el resultado n × filas.
    pub fn por(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        debug_assert_eq!(x.len(), n * self.cols);
        #[cfg(target_arch = "x86_64")]
        if self.simd && self.cols.is_multiple_of(8) && self.pares() {
            return self.por_bloques(x, n, pool);
        }
        self.por_filas(x, n, pool)
    }

    /// Por bloques: 2 filas × 6 tokens a la vez (12 sumas en registros), los 6 tokens en la L1 y el
    /// bloque de filas de cada hilo en la L2.
    #[cfg(target_arch = "x86_64")]
    fn por_bloques(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        const T: usize = x86::T;
        let k = self.cols;
        let np = n.div_ceil(T) * T;
        // Los tokens que faltan para llenar el último bloque, a cero (sus salidas se tiran).
        let relleno;
        let x = if np == n {
            x
        } else {
            let mut v = Vec::with_capacity(np * k);
            v.extend_from_slice(x);
            v.resize(np * k, 0.0);
            relleno = v;
            &relleno[..]
        };
        // La salida va directamente token a token (n × filas): cada tarea escribe su bloque de filas
        // de su tramo de tokens, sin solaparse con ninguna otra.
        let mut y = vec![0f32; np * self.filas];
        let hilos = pool.current_num_threads().max(1);
        // Bloques de filas que quepan en la L2 (~128 KB de pesos), al menos dos por hilo, y pares; y
        // tramos de tokens, para que con lotes grandes haya trabajo para todos los hilos.
        let bloque = (self.filas.div_ceil(hilos * 2).min((128 << 10) / (k * 2)).max(2) + 1) & !1;
        let tramo = 16 * T;
        let (nb, nt) = (self.filas.div_ceil(bloque), np.div_ceil(tramo));
        let salida = Puntero(y.as_mut_ptr());
        let filas_total = self.filas;
        let hacer = |tarea: usize| {
            let (b, c) = (tarea % nb, tarea / nb);
            let (r0, r1) = (b * bloque, ((b + 1) * bloque).min(filas_total));
            let (t0, t1) = (c * tramo, ((c + 1) * tramo).min(np));
            let salida = &salida;
            for t in (t0..t1).step_by(T) {
                for r in (r0..r1).step_by(2) {
                    // SAFETY: AVX2/FMA/F16C comprobados al cargar; r es par y su parte tiene filas
                    // pares, así que la fila siguiente está justo detrás; t + T <= np; cada tarea
                    // escribe solo sus filas de sus tokens.
                    unsafe {
                        let w = self.fila_ptr(r);
                        x86::bloque_f16(w, k, x.as_ptr().add(t * k), k, k, salida.0.add(t * filas_total + r), 1, filas_total);
                    }
                }
            }
        };
        if hilos == 1 || np * self.filas < 4096 {
            (0..nb * nt).for_each(hacer);
        } else {
            pool.install(|| (0..nb * nt).into_par_iter().for_each(hacer));
        }
        y.truncate(n * self.filas);
        y
    }

    /// Fila a fila (el camino sin AVX2): cada fila se pasa a f32 y se multiplica por cada token.
    pub fn por_filas(&self, x: &[f32], n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        let k = self.cols;
        let mut yt = vec![0f32; self.filas * n];
        let hilos = pool.current_num_threads().max(1);
        let bloque = self.filas.div_ceil(hilos * 4).max(8);
        let hacer = |(b, trozo): (usize, &mut [f32])| {
            let mut buf = vec![0f32; k];
            for (i, out) in trozo.chunks_mut(n).enumerate() {
                let p = self.fila_ptr(b * bloque + i);
                for (j, v) in buf.iter_mut().enumerate() {
                    // SAFETY: j < cols, dentro de la fila.
                    *v = half::f16::from_bits(unsafe { *p.add(j) }).to_f32();
                }
                for (t, o) in out.iter_mut().enumerate() {
                    *o = punto_generico(&buf, &x[t * k..(t + 1) * k]);
                }
            }
        };
        if hilos == 1 || n * self.filas < 4096 {
            yt.chunks_mut(bloque * n).enumerate().for_each(hacer);
        } else {
            pool.install(|| yt.par_chunks_mut(bloque * n).enumerate().for_each(hacer));
        }
        trasponer(&yt, self.filas, n, n)
    }
}

/// Un puntero que se puede compartir entre hilos que escriben en sitios distintos.
struct Puntero(*mut f32);
// SAFETY: cada tarea escribe en posiciones suyas (ver `por_bloques`).
unsafe impl Send for Puntero {}
unsafe impl Sync for Puntero {}

/// `yt` es filas × paso (por fila, sus tokens); devuelve los `n` primeros tokens × filas.
fn trasponer(yt: &[f32], filas: usize, paso: usize, n: usize) -> Vec<f32> {
    if n == 1 {
        return (0..filas).map(|r| yt[r * paso]).collect();
    }
    let mut y = vec![0f32; n * filas];
    for r in 0..filas {
        for t in 0..n {
            y[t * filas + r] = yt[r * paso + t];
        }
    }
    y
}

/// ¿Tiene esta CPU AVX2, FMA y F16C? `OMOIKANE_GEMMA_SIMD=0` hace como si no (para probar el camino
/// genérico en una que sí).
pub fn simd_disponible() -> bool {
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

/// Producto escalar con 8 sumas parciales (el compilador lo vectoriza con lo que tenga la CPU base).
fn punto_generico(a: &[f32], b: &[f32]) -> f32 {
    let mut s = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            s[i] += x[i] * y[i];
        }
    }
    s.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2,fma")]
    fn suma(v: __m256) -> f32 {
        let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        _mm_cvtss_f32(_mm_add_ss(s, _mm_shuffle_ps(s, s, 1)))
    }

    /// Tokens por bloque.
    pub const T: usize = 6;

    macro_rules! bloque {
        ($nombre:ident, $t:ty, $feat:literal, $carga:expr) => {
            /// `out[i·si + t·st] = w[i] · x[t]` para 2 filas de `w` (la segunda `sw` detrás de la
            /// primera) y `T` filas de `x` (separadas `sx`), de `k` valores (múltiplo de 8).
            #[target_feature(enable = $feat)]
            #[allow(clippy::too_many_arguments)]
            pub unsafe fn $nombre(w: *const $t, sw: usize, x: *const f32, sx: usize, k: usize, out: *mut f32, si: usize, st: usize) {
                unsafe {
                    let carga = $carga;
                    let mut a = [[_mm256_setzero_ps(); T]; 2];
                    let mut j = 0;
                    while j < k {
                        let w0 = carga(w, j);
                        let w1 = carga(w.add(sw), j);
                        for t in 0..T {
                            let xv = _mm256_loadu_ps(x.add(t * sx + j));
                            a[0][t] = _mm256_fmadd_ps(w0, xv, a[0][t]);
                            a[1][t] = _mm256_fmadd_ps(w1, xv, a[1][t]);
                        }
                        j += 8;
                    }
                    for i in 0..2 {
                        for t in 0..T {
                            *out.add(i * si + t * st) = suma(a[i][t]);
                        }
                    }
                }
            }
        };
    }

    bloque!(bloque_f32, f32, "avx2,fma", |w: *const f32, j: usize| _mm256_loadu_ps(w.add(j)));
    bloque!(bloque_f16, u16, "avx2,fma,f16c", |w: *const u16, j: usize| _mm256_cvtph_ps(_mm_loadu_si128(w.add(j) as *const __m128i)));
}

pub struct Capa {
    /// Normas con el `1 +` de Gemma ya sumado.
    pub ne: Vec<f32>,
    pub nta: Vec<f32>,
    pub nam: Vec<f32>,
    pub ntm: Vec<f32>,
    pub nq: Vec<f32>,
    pub nk: Vec<f32>,
    /// q, k y v una tras otra (768 + 256 + 256 filas).
    pub qkv: Mat,
    pub o: Mat,
    /// Puerta y subir una tras otra (1152 + 1152 filas).
    pub gu: Mat,
    pub bajar: Mat,
    pub local: bool,
}

pub struct Rapido {
    pub capas: Vec<Capa>,
    pub norma: Vec<f32>,
    pub densa1: Mat,
    pub densa2: Mat,
    pub eps: f32,
    pub cabezas: usize,
    pub cabezas_kv: usize,
    pub dim_cabeza: usize,
    pub escala: f32,
    /// Media ventana de las capas locales: cada token ve a los que están a esta distancia o menos.
    pub medio: usize,
    /// cos y sin de RoPE por posición (MAX × dim_cabeza/2), local y global.
    pub rope_local: (Vec<f32>, Vec<f32>),
    pub rope_global: (Vec<f32>, Vec<f32>),
    /// AVX2/FMA/F16C (si no, el camino genérico).
    pub simd: bool,
}

/// Como `Gemma::rope`, en f32 igual que candle.
pub fn tabla_rope(max: usize, d: usize, base: f64) -> (Vec<f32>, Vec<f32>) {
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

fn rms_en(x: &mut [f32], w: &[f32], eps: f32) {
    let m = x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32;
    let r = 1.0 / (m + eps).sqrt();
    x.iter_mut().zip(w).for_each(|(v, w)| *v = *v * r * w);
}

fn rms_filas(x: &[f32], w: &[f32], eps: f32, pool: &rayon::ThreadPool) -> Vec<f32> {
    let mut y = x.to_vec();
    if x.len() / w.len() < 64 {
        y.chunks_mut(w.len()).for_each(|f| rms_en(f, w, eps));
    } else {
        pool.install(|| y.par_chunks_mut(w.len()).for_each(|f| rms_en(f, w, eps)));
    }
    y
}

/// `x += rms(o)` fila a fila (el residuo tras la atención o el MLP).
fn sumar_rms(x: &mut [f32], o: &mut [f32], w: &[f32], eps: f32, pool: &rayon::ThreadPool) {
    let d = w.len();
    let f = |(x, o): (&mut [f32], &mut [f32])| {
        rms_en(o, w, eps);
        x.iter_mut().zip(o.iter()).for_each(|(x, o)| *x += o);
    };
    if x.len() / d < 64 {
        x.chunks_mut(d).zip(o.chunks_mut(d)).for_each(f);
    } else {
        pool.install(|| x.par_chunks_mut(d).zip(o.par_chunks_mut(d)).for_each(f));
    }
}

fn rope_en(v: &mut [f32], cos: &[f32], sin: &[f32]) {
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

/// Lo que necesita la atención de un texto: dónde empieza en el lote y cuántos tokens tiene.
#[derive(Clone, Copy)]
struct Tramo {
    ini: usize,
    n: usize,
}

impl Rapido {
    /// El vector (normalizado) de un texto a partir de los vectores de entrada de sus `n` tokens.
    pub fn vector(&self, x: Vec<f32>, n: usize, pool: &rayon::ThreadPool) -> Vec<f32> {
        self.vectores(x, &[n], pool, &|| {}).pop().unwrap_or_default()
    }

    /// Los vectores de varios textos a la vez: `x` son los vectores de entrada de todos sus tokens,
    /// uno tras otro, y `largos` cuántos tokens tiene cada texto. `ceder` se llama antes de cada paso
    /// (unas 100 veces por vector): el indexado lo usa para dejar pasar una consulta.
    pub fn vectores(&self, mut x: Vec<f32>, largos: &[usize], pool: &rayon::ThreadPool, ceder: &dyn Fn()) -> Vec<Vec<f32>> {
        let d = self.norma.len();
        let total: usize = largos.iter().sum();
        debug_assert_eq!(x.len(), total * d);
        if total == 0 {
            return largos.iter().map(|_| vec![0.0; d]).collect();
        }
        let (h, kv, hd) = (self.cabezas, self.cabezas_kv, self.dim_cabeza);
        let (dq, dk) = (h * hd, kv * hd);
        let ancho = dq + 2 * dk;
        let mitad = hd / 2;
        let mut tramos = Vec::with_capacity(largos.len());
        let mut pos = Vec::with_capacity(total);
        for &n in largos {
            tramos.push(Tramo { ini: pos.len(), n });
            pos.extend(0..n);
        }
        let mut atn = vec![0f32; total * dq];
        for c in &self.capas {
            ceder();
            let e = rms_filas(&x, &c.ne, self.eps, pool);
            let mut qkv = c.qkv.por(&e, total, pool);
            let (cos, sin) = if c.local { &self.rope_local } else { &self.rope_global };
            let rope_fila = |(t, f): (usize, &mut [f32])| {
                let p = pos[t];
                let (cs, sn) = (&cos[p * mitad..(p + 1) * mitad], &sin[p * mitad..(p + 1) * mitad]);
                for j in 0..h + kv {
                    let v = &mut f[j * hd..(j + 1) * hd];
                    rms_en(v, if j < h { &c.nq } else { &c.nk }, self.eps);
                    rope_en(v, cs, sn);
                }
            };
            if total < 64 {
                qkv.chunks_mut(ancho).enumerate().for_each(rope_fila);
            } else {
                pool.install(|| qkv.par_chunks_mut(ancho).enumerate().for_each(rope_fila));
            }
            ceder();
            self.atencion(&qkv, &tramos, c.local, &mut atn, pool);
            ceder();
            let mut o = c.o.por(&atn, total, pool);
            sumar_rms(&mut x, &mut o, &c.nta, self.eps, pool);
            let e = rms_filas(&x, &c.nam, self.eps, pool);
            ceder();
            let gu = c.gu.por(&e, total, pool);
            let inter = c.gu.filas / 2;
            let mut m = vec![0f32; total * inter];
            // La tanh de la GELU cuesta: repartida entre los hilos por trozos de 256.
            let gelu_trozo = |(c, trozo): (usize, &mut [f32])| {
                for (j, o) in trozo.iter_mut().enumerate() {
                    let (t, i) = ((c * 256 + j) / inter, (c * 256 + j) % inter);
                    let f = &gu[t * 2 * inter..];
                    *o = gelu(f[i]) * f[inter + i];
                }
            };
            pool.install(|| m.par_chunks_mut(256).enumerate().for_each(gelu_trozo));
            ceder();
            let mut b = c.bajar.por(&m, total, pool);
            sumar_rms(&mut x, &mut b, &c.ntm, self.eps, pool);
        }
        let x = rms_filas(&x, &self.norma, self.eps, pool);
        let mut medias = vec![0f32; largos.len() * d];
        for (tr, media) in tramos.iter().zip(medias.chunks_mut(d)) {
            for f in x[tr.ini * d..(tr.ini + tr.n) * d].chunks(d) {
                media.iter_mut().zip(f).for_each(|(m, v)| *m += v);
            }
            media.iter_mut().for_each(|m| *m /= tr.n.max(1) as f32);
        }
        let y = self.densa1.por(&medias, largos.len(), pool);
        let y = self.densa2.por(&y, largos.len(), pool);
        y.chunks(self.densa2.filas)
            .map(|v| {
                let norma = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.iter().map(|x| x / norma).collect()
            })
            .collect()
    }

    /// La atención de cada texto del lote, bidireccional; en las capas locales, cada token solo ve
    /// a los que están a `medio` o menos.
    fn atencion(&self, qkv: &[f32], tramos: &[Tramo], local: bool, atn: &mut [f32], pool: &rayon::ThreadPool) {
        #[cfg(target_arch = "x86_64")]
        if self.simd && self.dim_cabeza.is_multiple_of(8) {
            return self.atencion_bloques(qkv, tramos, local, atn, pool);
        }
        self.atencion_directa(qkv, tramos, local, atn, pool);
    }

    /// Fila a fila: cada consulta contra cada clave (el camino sin AVX2).
    fn atencion_directa(&self, qkv: &[f32], tramos: &[Tramo], local: bool, atn: &mut [f32], pool: &rayon::ThreadPool) {
        let (h, kv, hd) = (self.cabezas, self.cabezas_kv, self.dim_cabeza);
        let (dq, dk) = (h * hd, kv * hd);
        let ancho = dq + 2 * dk;
        let grupo = h / kv;
        let total: usize = tramos.iter().map(|t| t.n).sum();
        // (fila de `atn`, tramo al que pertenece)
        let tramo_de: Vec<Tramo> = tramos.iter().flat_map(|t| std::iter::repeat_n(*t, t.n)).collect();
        let fila_atn = |(g, fila): (usize, &mut [f32])| {
            let tr = tramo_de[g];
            let (i, qkv) = (g - tr.ini, &qkv[tr.ini * ancho..(tr.ini + tr.n) * ancho]);
            let n = tr.n;
            let ventana = local && n > self.medio + 1;
            let mut pesos = vec![0f32; n];
            fila.iter_mut().for_each(|a| *a = 0.0);
            let (j0, j1) = if ventana { (i.saturating_sub(self.medio), (i + self.medio + 1).min(n)) } else { (0, n) };
            for cab in 0..h {
                let q = &qkv[i * ancho + cab * hd..i * ancho + (cab + 1) * hd];
                let ck = dq + (cab / grupo) * hd;
                let cv = dq + dk + (cab / grupo) * hd;
                let mut max = f32::NEG_INFINITY;
                for j in j0..j1 {
                    let p = punto_generico(q, &qkv[j * ancho + ck..j * ancho + ck + hd]) * self.escala;
                    pesos[j] = p;
                    max = max.max(p);
                }
                let mut s = 0.0;
                for p in &mut pesos[j0..j1] {
                    *p = (*p - max).exp();
                    s += *p;
                }
                let o = &mut fila[cab * hd..(cab + 1) * hd];
                for j in j0..j1 {
                    let a = pesos[j] / s;
                    let v = &qkv[j * ancho + cv..j * ancho + cv + hd];
                    o.iter_mut().zip(v).for_each(|(o, v)| *o += a * v);
                }
            }
        };
        // Con pocos tokens no compensa repartirla.
        if total < 64 {
            atn.chunks_mut(dq).enumerate().for_each(fila_atn);
        } else {
            pool.install(|| atn.par_chunks_mut(dq).enumerate().for_each(fila_atn));
        }
    }

    /// Por bloques de 6 consultas: los puntos de cada bloque contra sus claves, con el mismo núcleo
    /// que las multiplicaciones (las claves hacen de filas), y lo mismo con los valores traspuestos.
    #[cfg(target_arch = "x86_64")]
    fn atencion_bloques(&self, qkv: &[f32], tramos: &[Tramo], local: bool, atn: &mut [f32], pool: &rayon::ThreadPool) {
        const T: usize = x86::T;
        let (h, kv, hd) = (self.cabezas, self.cabezas_kv, self.dim_cabeza);
        let (dq, dk) = (h * hd, kv * hd);
        let ancho = dq + 2 * dk;
        let grupo = h / kv;
        // Por texto: Q por cabeza (filas hasta múltiplo de T), K por cabeza (filas hasta múltiplo de
        // 8) y Vᵀ por cabeza (columnas hasta múltiplo de 8); lo que sobra, a cero.
        struct Prep {
            n: usize,
            n8: usize,
            nq: usize,
            q: Vec<f32>,
            k: Vec<f32>,
            vt: Vec<f32>,
        }
        let preps: Vec<Prep> = tramos
            .iter()
            .map(|tr| {
                let n = tr.n;
                let (n8, nq) = (n.div_ceil(8) * 8, n.div_ceil(T) * T);
                let mut q = vec![0f32; h * nq * hd];
                let mut k = vec![0f32; kv * n8 * hd];
                let mut vt = vec![0f32; kv * hd * n8];
                for t in 0..n {
                    let f = &qkv[(tr.ini + t) * ancho..(tr.ini + t + 1) * ancho];
                    for c in 0..h {
                        q[(c * nq + t) * hd..(c * nq + t + 1) * hd].copy_from_slice(&f[c * hd..(c + 1) * hd]);
                    }
                    for c in 0..kv {
                        k[(c * n8 + t) * hd..(c * n8 + t + 1) * hd].copy_from_slice(&f[dq + c * hd..dq + (c + 1) * hd]);
                        for (dd, v) in f[dq + dk + c * hd..dq + dk + (c + 1) * hd].iter().enumerate() {
                            vt[(c * hd + dd) * n8 + t] = *v;
                        }
                    }
                }
                Prep { n, n8, nq, q, k, vt }
            })
            .collect();
        // Las tareas: cada bloque de T filas de cada texto, con su trozo de `atn`.
        let mut tareas: Vec<(usize, usize, &mut [f32])> = Vec::new();
        let mut resto = atn;
        for (i, tr) in tramos.iter().enumerate() {
            let (este, otro) = std::mem::take(&mut resto).split_at_mut(tr.n * dq);
            resto = otro;
            for (b, trozo) in este.chunks_mut(T * dq).enumerate() {
                tareas.push((i, b * T, trozo));
            }
        }
        let hacer = |(i, t0, trozo): (usize, usize, &mut [f32])| {
            let p = &preps[i];
            let n = p.n;
            let reales = trozo.len() / dq;
            let ventana = local && n > self.medio + 1;
            let (mut j0, j1) = if ventana { (t0.saturating_sub(self.medio), (t0 + reales + self.medio).min(n)) } else { (0, n) };
            let largo = (j1 - j0).div_ceil(8) * 8;
            if j0 + largo > p.n8 {
                j0 = p.n8 - largo;
            }
            let mut st = vec![0f32; largo * T];
            let mut pr = vec![0f32; T * largo];
            let mut o = vec![0f32; hd * T];
            for cab in 0..h {
                let kc = &p.k[(cab / grupo) * p.n8 * hd..];
                let vt = &p.vt[(cab / grupo) * hd * p.n8..];
                let q = &p.q[(cab * p.nq + t0) * hd..];
                // Puntos: st[j·T + t] = k[j0 + j] · q[t].
                for j in (0..largo).step_by(2) {
                    // SAFETY: AVX2/FMA comprobados; j0 + j + 1 < n8 filas de k; T filas de q desde t0 < nq.
                    unsafe { x86::bloque_f32(kc.as_ptr().add((j0 + j) * hd), hd, q.as_ptr(), hd, hd, st.as_mut_ptr().add(j * T), T, 1) };
                }
                // Softmax de cada consulta sobre las claves que ve; lo demás, a cero.
                for t in 0..T {
                    let fila = &mut pr[t * largo..(t + 1) * largo];
                    fila.iter_mut().for_each(|v| *v = 0.0);
                    if t >= reales {
                        continue;
                    }
                    let i = t0 + t;
                    let (a, b) = if ventana { (i.saturating_sub(self.medio).max(j0), (i + self.medio + 1).min(n)) } else { (0, n) };
                    let mut max = f32::NEG_INFINITY;
                    for j in a..b {
                        max = max.max(st[(j - j0) * T + t] * self.escala);
                    }
                    let mut s = 0.0;
                    for j in a..b {
                        let e = (st[(j - j0) * T + t] * self.escala - max).exp();
                        fila[j - j0] = e;
                        s += e;
                    }
                    fila[a - j0..b - j0].iter_mut().for_each(|v| *v /= s);
                }
                // Salida: o[d·T + t] = vᵀ[d][j0..] · pr[t].
                for dd in (0..hd).step_by(2) {
                    // SAFETY: AVX2/FMA comprobados; dd + 1 < hd filas de vᵀ de n8 columnas, j0 + largo <= n8.
                    unsafe { x86::bloque_f32(vt.as_ptr().add(dd * p.n8 + j0), p.n8, pr.as_ptr(), largo, largo, o.as_mut_ptr().add(dd * T), T, 1) };
                }
                for t in 0..reales {
                    for dd in 0..hd {
                        trozo[t * dq + cab * hd + dd] = o[dd * T + t];
                    }
                }
            }
        };
        let total: usize = tramos.iter().map(|t| t.n).sum();
        if total < 64 {
            tareas.into_iter().for_each(hacer);
        } else {
            pool.install(|| tareas.into_par_iter().for_each(hacer));
        }
    }

    pub fn bytes(&self) -> usize {
        self.capas.iter().map(|c| c.qkv.bytes() + c.o.bytes() + c.gu.bytes() + c.bajar.bytes()).sum::<usize>() + self.densa1.bytes() + self.densa2.bytes()
    }
}
