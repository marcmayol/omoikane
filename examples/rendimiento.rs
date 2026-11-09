//! Guardia de rendimiento: el recall y la búsqueda de este `omoi` contra los de la última Release, en
//! la misma máquina y con la misma base. Falla (sale con 1) si alguno va claramente más lento.
//!
//!   cargo build --release && cargo run --release --example rendimiento -- [opciones]
//!
//! Opciones:
//!   --contra <versión>   la Release con la que se compara (por defecto, la última publicada); se baja
//!                        de GitHub y se comprueba como al actualizarse: firma de `SHA256SUMS` con la
//!                        clave de Omoikane y SHA-256 del paquete
//!   --a-bin <omoi>       en vez de bajarla, este binario como el de antes
//!   --b-bin <omoi>       el nuevo (por defecto, el de `target/release`)
//!   --memorias <n>       tamaño de la base sintética (por defecto 10.000)
//!   --rondas <n>         medidas por prueba y binario (por defecto 30)
//!   --margen <x>         cuánto más lento se tolera, en proporción de la mediana (por defecto 0,25)
//!   --minimo-ms <ms>     y en milisegundos: por debajo no cuenta como más lento (por defecto 5)
//!
//! Cómo evita los falsos positivos de una máquina ruidosa (los runners de GitHub lo son):
//! - Los dos binarios se miden en la misma máquina, en el mismo momento y sobre copias de la misma
//!   base, intercalados (A,B en una ronda, B,A en la siguiente): lo que la máquina tenga de lenta, lo
//!   tiene para los dos. Nunca contra un número guardado de otra máquina.
//! - Cuenta la mediana, no la media ni el P95: un proceso que se cruza con otro no la mueve.
//! - Más lento es pasar a la vez el margen relativo y el absoluto.
//! - Una prueba que sale más lenta se vuelve a medir entera; solo falla si lo es las dos veces.
//!
//! Cada llamada es un proceso nuevo, como lo lanzan el hook y el agente: el arranque cuenta. Cada
//! binario tiene su propia copia de la base y su propia carpeta de Omoikane, sin modelos, sin
//! sincronización y sin actualizaciones; las variables `OMOIKANE_*` de quien lo lanza no llegan.
//! Todo va a una carpeta temporal que se borra al terminar.
//!
//! La base es sintética y determinista: las memorias de `tests/busquedas.tsv` más relleno con las
//! palabras de `tests/busquedas.tsv` y `tests/recall_casos.tsv` y palabras inventadas, repartido en 8
//! proyectos y las globales. Las consultas son las de `tests/busquedas.tsv`.

use anyhow::{Context, Result, anyhow, bail};
use omoikane::memoria::{self, Nueva, Tipo};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const BUSQUEDAS: &str = include_str!("../tests/busquedas.tsv");
const RECALL: &str = include_str!("../tests/recall_casos.tsv");
const PROYECTOS: usize = 8;

/// SplitMix64: la misma base en todas las máquinas, sin depender de nada.
struct Azar(u64);

impl Azar {
    fn siguiente(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Entre 0 y 1.
    fn real(&mut self) -> f64 {
        (self.siguiente() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn hasta(&mut self, n: usize) -> usize {
        (self.siguiente() % n as u64) as usize
    }
}

/// Las palabras de los ficheros de casos (las que buscan las consultas) y, detrás, palabras inventadas.
/// Se eligen con sesgo hacia el principio, como en un texto real: unas pocas salen en muchas memorias
/// y la mayoría en pocas.
fn vocabulario() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in BUSQUEDAS.lines().chain(RECALL.lines()).filter(|l| !l.starts_with('#')) {
        for p in l.split(|c: char| !c.is_alphanumeric()).filter(|p| p.chars().count() > 1) {
            let p = p.to_lowercase();
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }
    let silabas = ["ka", "to", "ri", "mu", "sen", "lo", "ve", "da", "pi", "nor", "gu", "fe", "ras", "zo", "te", "bil"];
    for a in silabas {
        for b in silabas {
            v.push(format!("{a}{b}"));
            for c in silabas {
                v.push(format!("{a}{b}{c}"));
            }
        }
    }
    v
}

fn frase(azar: &mut Azar, voc: &[String], palabras: usize) -> String {
    (0..palabras).map(|_| voc[((azar.real().powf(2.5)) * voc.len() as f64) as usize].as_str()).collect::<Vec<_>>().join(" ")
}

/// Crea la base en `db` con `n` memorias; devuelve la carpeta del primer proyecto (la de las consultas).
fn crear_base(db: &Path, carpetas: &Path, n: usize) -> Result<PathBuf> {
    let mut c = omoikane::db::abrir(db)?;
    // Una base de usar y tirar: sin esperar al disco en cada memoria (en un disco duro, minutos).
    c.execute_batch("PRAGMA synchronous=OFF;")?;
    let mut claves = Vec::new();
    for p in 0..PROYECTOS {
        let d = carpetas.join(format!("p{p}"));
        std::fs::create_dir_all(&d)?;
        claves.push(omoikane::proyecto::de(&d)?);
    }
    let tipos = [Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Fact, Tipo::Preference, Tipo::Fact];
    let guardar = |c: &mut rusqlite::Connection, project: String, kind: Tipo, title: String, content: String| -> Result<()> {
        memoria::guardar(c, Nueva { project, kind: Some(kind), title, content, source: "rendimiento".into(), ..Default::default() }, None)?;
        Ok(())
    };
    // Las de verdad, para que las consultas encuentren lo suyo entre el relleno.
    for l in BUSQUEDAS.lines().filter(|l| l.starts_with("m\t")) {
        let c4: Vec<&str> = l.split('\t').collect();
        if let [_, _, titulo, contenido, ..] = c4.as_slice() {
            guardar(&mut c, claves[0].clone(), Tipo::Fact, titulo.to_string(), contenido.to_string())?;
        }
    }
    let voc = vocabulario();
    let mut azar = Azar(20261109);
    for i in 0..n {
        let project = if i % 10 == 0 { String::new() } else { claves[i % PROYECTOS].clone() };
        let (largo_titulo, largo_contenido) = (4 + azar.hasta(6), 25 + azar.hasta(65));
        let titulo = frase(&mut azar, &voc, largo_titulo);
        let contenido = frase(&mut azar, &voc, largo_contenido);
        guardar(&mut c, project, tipos[i % tipos.len()], titulo, contenido)?;
    }
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(carpetas.join("p0"))
}

struct Lado {
    nombre: String,
    bin: PathBuf,
    casa: PathBuf,
    db: PathBuf,
    sin_modelos: PathBuf,
}

impl Lado {
    /// Corre `omoi <args>` con `entrada` por stdin; devuelve (ms, stdout).
    fn correr(&self, args: &[&str], cwd: &Path, entrada: Option<&str>) -> Result<(f64, String)> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, _) in std::env::vars() {
            if k.starts_with("OMOIKANE_") || k == "USER_PROMPT" {
                cmd.env_remove(k);
            }
        }
        cmd.env("OMOIKANE_HOME", &self.casa)
            .env("OMOIKANE_DB", &self.db)
            .env("OMOIKANE_MODELS", &self.sin_modelos)
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_LANG", "en");
        let t = Instant::now();
        let mut hijo = cmd.spawn().with_context(|| self.bin.display().to_string())?;
        let mut stdin = hijo.stdin.take().context("stdin")?;
        if let Some(e) = entrada {
            stdin.write_all(e.as_bytes())?;
        }
        drop(stdin);
        let out = hijo.wait_with_output()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if !out.status.success() {
            bail!("{} {}: {}", self.nombre, args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok((ms, String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Prueba {
    Recall,
    BuscarProyecto,
    BuscarTodo,
}

impl Prueba {
    fn nombre(self) -> &'static str {
        match self {
            Prueba::Recall => "recall",
            Prueba::BuscarProyecto => "search (project)",
            Prueba::BuscarTodo => "search --all",
        }
    }

    /// Una llamada; devuelve (ms, ids que devolvió, en orden).
    fn medir(self, l: &Lado, cwd: &Path, consulta: &str, sesion: &str) -> Result<(f64, Vec<String>)> {
        Ok(match self {
            Prueba::Recall => {
                let e = serde_json::json!({ "session_id": sesion, "cwd": cwd, "prompt": consulta, "hook_event_name": "UserPromptSubmit" });
                let (ms, out) = l.correr(&["recall"], cwd, Some(&e.to_string()))?;
                (ms, ids_recall(&out))
            }
            Prueba::BuscarProyecto | Prueba::BuscarTodo => {
                let mut args = vec!["--json", "search"];
                if self == Prueba::BuscarTodo {
                    args.push("--all");
                }
                args.push(consulta);
                let (ms, out) = l.correr(&args, cwd, None)?;
                let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
                (ms, v.as_array().map(|a| a.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()).unwrap_or_default())
            }
        })
    }
}

/// Lo que inyecta el recall: el principio de cada `(id …)` del bloque.
fn ids_recall(salida: &str) -> Vec<String> {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(salida.trim()) else { return Vec::new() };
    let texto = j["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    texto.match_indices("(id ").map(|(i, _)| texto[i + 4..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect()).collect()
}

fn percentil(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(((v.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(0.0)
}

struct Medida {
    a: Vec<f64>,
    b: Vec<f64>,
    /// Consultas en las que los dos devolvieron lo mismo, de cuántas.
    iguales: usize,
    consultas: usize,
}

fn medir(prueba: Prueba, lados: &[Lado; 2], cwd: &Path, consultas: &[String], rondas: usize, pasada: usize) -> Result<Medida> {
    // Calentar: la primera llamada paga la caché fría del disco y no cuenta.
    for l in lados {
        for _ in 0..3 {
            prueba.medir(l, cwd, &consultas[0], &format!("calentar-{pasada}"))?;
        }
    }
    let mut m = Medida { a: Vec::new(), b: Vec::new(), iguales: 0, consultas: 0 };
    for r in 0..rondas {
        let q = &consultas[r % consultas.len()];
        // La misma sesión para los dos (cada uno con su base): el recall no repite lo ya inyectado.
        let sesion = format!("rendimiento-{pasada}-{r}");
        let orden = if r % 2 == 0 { [0, 1] } else { [1, 0] };
        let mut ids: [Vec<String>; 2] = Default::default();
        for k in orden {
            let (ms, got) = prueba.medir(&lados[k], cwd, q, &sesion)?;
            if k == 0 { m.a.push(ms) } else { m.b.push(ms) }
            ids[k] = got;
        }
        if r < consultas.len() {
            m.consultas += 1;
            m.iguales += usize::from(ids[0] == ids[1]);
        }
    }
    Ok(m)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut contra, mut a_bin, mut b_bin): (Option<String>, Option<PathBuf>, Option<PathBuf>) = (None, None, None);
    let (mut n, mut rondas, mut margen, mut minimo) = (10_000usize, 30usize, 0.25f64, 5.0f64);
    let mut i = 0;
    while i < args.len() {
        let valor = args.get(i + 1).cloned().ok_or_else(|| anyhow!("falta el valor de {}", args[i]))?;
        match args[i].as_str() {
            "--contra" => contra = Some(valor.trim_start_matches('v').to_string()),
            "--a-bin" => a_bin = Some(valor.into()),
            "--b-bin" => b_bin = Some(valor.into()),
            "--memorias" => n = valor.parse()?,
            "--rondas" => rondas = valor.parse()?,
            "--margen" => margen = valor.replace(',', ".").parse()?,
            "--minimo-ms" => minimo = valor.replace(',', ".").parse()?,
            x => bail!("opción desconocida: {x} (mira el principio de examples/rendimiento.rs)"),
        }
        i += 2;
    }
    anyhow::ensure!(rondas >= 5, "al menos 5 rondas");

    let tmp = std::env::temp_dir().join(format!("omoi-rendimiento-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tmp = tmp.canonicalize()?;
    if let Some(casa) = std::env::home_dir().map(|h| h.join(".omoikane"))
        && let Ok(casa) = casa.canonicalize()
        && tmp.starts_with(&casa)
    {
        bail!("la carpeta temporal está dentro de {}", casa.display());
    }
    let limpiar = Limpiar(tmp.clone());

    let release = std::env::current_exe()?.parent().and_then(Path::parent).map(|d| d.join(format!("omoi{}", std::env::consts::EXE_SUFFIX)));
    let b_bin = b_bin.or(release).filter(|b| b.exists()).ok_or_else(|| anyhow!("no está el omoi nuevo: cargo build --release (o --b-bin)"))?;
    let (a_bin, a_origen) = match a_bin {
        Some(b) => {
            let origen = b.display().to_string();
            (b, origen)
        }
        None => {
            let v = match contra {
                Some(v) => v,
                None => omoikane::actualizar::ultima().context("¿cuál es la última Release? (o --contra <versión>)")?,
            };
            eprintln!("bajando omoi {v} y comprobando su firma y su SHA-256…");
            (omoikane::actualizar::bajar_version(&v, &tmp.join("antes"))?, format!("Release v{v}, signature and SHA-256 checked"))
        }
    };
    let version = |b: &Path| -> String {
        Command::new(b).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    };

    eprintln!("creando una base de {n} memorias…");
    let t = Instant::now();
    let base = tmp.join("base.db");
    let cwd = crear_base(&base, &tmp.join("proyectos"), n)?;
    eprintln!("  en {:.1} s", t.elapsed().as_secs_f64());
    let sin_modelos = tmp.join("sin-modelos");
    std::fs::create_dir_all(&sin_modelos)?;
    let lado = |nombre: &str, bin: PathBuf| -> Result<Lado> {
        let casa = tmp.join(nombre);
        std::fs::create_dir_all(&casa)?;
        let db = casa.join("omoikane.db");
        std::fs::copy(&base, &db)?;
        Ok(Lado { nombre: nombre.into(), bin, casa, db, sin_modelos: sin_modelos.clone() })
    };
    let lados = [lado("A", a_bin)?, lado("B", b_bin)?];
    let consultas: Vec<String> = BUSQUEDAS.lines().filter(|l| l.starts_with("q\t")).filter_map(|l| l.split('\t').nth(2).map(str::to_string)).collect();

    let mut informe = format!(
        "## Recall and search against the last release\n\nA: {} ({a_origen})  \nB: {} ({})  \n{n} memories, {rondas} interleaved rounds per test, one process per call; slower means a median over {:.0} % and {minimo} ms above A, twice in a row.\n\n| | A P50 / P95 (ms) | B P50 / P95 (ms) | B / A | same results | |\n|---|---|---|---|---|---|\n",
        version(&lados[0].bin),
        version(&lados[1].bin),
        lados[1].bin.display(),
        margen * 100.0
    );
    let mut mal = Vec::new();
    for prueba in [Prueba::Recall, Prueba::BuscarProyecto, Prueba::BuscarTodo] {
        eprintln!("midiendo {}…", prueba.nombre());
        let lenta = |m: &Medida| {
            let (a, b) = (percentil(&m.a, 0.5), percentil(&m.b, 0.5));
            b > a * (1.0 + margen) && b - a > minimo
        };
        let mut m = medir(prueba, &lados, &cwd, &consultas, rondas, 0)?;
        let mut veredicto = "ok";
        if lenta(&m) {
            eprintln!("  más lenta: se mide otra vez");
            let otra = medir(prueba, &lados, &cwd, &consultas, rondas, 1)?;
            veredicto = if lenta(&otra) { "**slower**" } else { "ok (slower once, not twice)" };
            if lenta(&otra) {
                mal.push(prueba.nombre());
            }
            m = otra;
        }
        let (a50, b50) = (percentil(&m.a, 0.5), percentil(&m.b, 0.5));
        informe.push_str(&format!(
            "| {} | {a50:.1} / {:.1} | {b50:.1} / {:.1} | {:.2} | {} of {} | {veredicto} |\n",
            prueba.nombre(),
            percentil(&m.a, 0.95),
            percentil(&m.b, 0.95),
            b50 / a50.max(1e-9),
            m.iguales,
            m.consultas
        ));
    }
    println!("{informe}");
    // En GitHub Actions, también en el resumen de la ejecución.
    if let Some(f) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let _ = std::fs::OpenOptions::new().append(true).create(true).open(f).and_then(|mut f| f.write_all(informe.as_bytes()));
    }
    if !mal.is_empty() {
        eprintln!("más lento que la versión anterior: {}", mal.join(", "));
        // `exit` no pasa por los `Drop`: la carpeta temporal se borra antes.
        drop(limpiar);
        std::process::exit(1);
    }
    Ok(())
}

struct Limpiar(PathBuf);

impl Drop for Limpiar {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
