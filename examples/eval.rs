//! Comparador del recall, antes y después de un cambio: las mismas preguntas con dos configuraciones,
//! de forma determinista y sin modelo de juez, y la lista de lo que el cambio arregla y lo que rompe.
//!
//!   cargo build --release && cargo run --release --example eval -- CASOS [opciones]
//!
//! CASOS es un TSV como `tests/busquedas.tsv` (líneas que empiezan por `#`, ignoradas):
//!
//!   m <clave> <título> <contenido> [<tipo> [<etiquetas,con,comas> [<proyecto>]]]   una memoria para la base
//!                                                                  (global, o del proyecto de una carpeta
//!                                                                  `<proyecto>` creada en la temporal)
//!   q <esperadas> <mensaje> [<cwd>]                                   un caso
//!
//! `<esperadas>`: lo que debería inyectar el recall, separado por `|`: claves de líneas `m`, ids (o su
//! principio, desde 8 caracteres) o títulos exactos de la base; `-` si no debería inyectar nada. En el
//! contenido de una memoria, `\n` es un salto de línea (en el mensaje no: las rutas de Windows lo
//! llevan). Sin `<cwd>`, una carpeta temporal que no es un proyecto; `~`, la carpeta del usuario (que ve
//! todos los proyectos, como quien abre las sesiones ahí).
//!
//! Opciones:
//!   --base <db>          parte de una COPIA de esta base (se abre en solo lectura y se copia con
//!                        `VACUUM INTO`); las líneas `m` se añaden a la copia
//!   --a-bin / --b-bin    el `omoi` de cada configuración (por defecto, el de `target/release`)
//!   --a-env / --b-env    K=V para esa configuración (se puede repetir)
//!   --a-extra / --b-extra <tsv>  memorias (líneas `m`) que solo tiene la base de esa configuración
//!   --rondas <n>         pasadas por caso para medir el tiempo (por defecto 1)
//!   --json <fichero>     lo que inyectó cada configuración en cada caso, para mirarlo después
//!
//! Cada configuración tiene su propia copia de la base y su propia carpeta de Omoikane
//! (`OMOIKANE_HOME`, `OMOIKANE_DB`), sin sincronización, sin actualizaciones y con el modelo de
//! significado apagado (salvo que `--a-env`/`--b-env` digan otra cosa); las variables `OMOIKANE_*` de
//! quien lo lanza no llegan a los procesos. Cada caso es un `omoi recall` nuevo, como lo lanza el hook,
//! con una sesión nueva; las configuraciones se intercalan (A,B en una ronda, B,A en la siguiente)
//! para que la caché del disco no favorezca a ninguna.
//!
//! Un caso con memorias esperadas está bien si el recall trae alguna de ellas; uno sin ellas (`-`),
//! si no trae nada. «Arreglados»: mal en A y bien en B; «rotos»: al revés. Además, por configuración:
//! precisión (de lo inyectado, lo esperado), cobertura (de lo esperado, lo inyectado) y «nada
//! correcto» (casos `-` en los que no inyectó nada).

use anyhow::{Context, Result, anyhow, bail};
use omoikane::memoria::{self, Nueva, Tipo};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

struct Mem {
    clave: String,
    titulo: String,
    contenido: String,
    tipo: Tipo,
    etiquetas: Vec<String>,
    proyecto: Option<String>,
}

struct Caso {
    esperadas: Vec<String>,
    mensaje: String,
    cwd: Option<String>,
}

#[derive(Default)]
struct Config {
    bin: Option<PathBuf>,
    env: Vec<(String, String)>,
    extra: Option<PathBuf>,
}

fn leer_tsv(f: &Path) -> Result<(Vec<Mem>, Vec<Caso>)> {
    let texto = std::fs::read_to_string(f).with_context(|| f.display().to_string())?;
    let (mut ms, mut qs) = (Vec::new(), Vec::new());
    for (i, l) in texto.lines().enumerate() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let c: Vec<&str> = l.split('\t').collect();
        match c.as_slice() {
            ["m", clave, titulo, contenido, resto @ ..] => ms.push(Mem {
                clave: clave.to_string(),
                titulo: titulo.to_string(),
                contenido: contenido.replace("\\n", "\n"),
                tipo: resto.first().filter(|t| !t.is_empty()).map(|t| t.parse()).transpose()?.unwrap_or(Tipo::Fact),
                etiquetas: resto.get(1).map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
                proyecto: resto.get(2).filter(|p| !p.is_empty()).map(|p| p.to_string()),
            }),
            ["q", esperadas, mensaje, resto @ ..] => qs.push(Caso {
                esperadas: if *esperadas == "-" { vec![] } else { esperadas.split('|').map(|x| x.trim().to_string()).collect() },
                mensaje: mensaje.to_string(),
                cwd: resto.first().filter(|c| !c.is_empty()).map(|c| c.to_string()),
            }),
            _ => bail!("{}:{}: línea que no es `m` ni `q`", f.display(), i + 1),
        }
    }
    Ok((ms, qs))
}

/// Guarda las memorias en la base y devuelve clave → id. Las de un proyecto, en una carpeta con su
/// nombre dentro de `carpetas`.
fn guardar(db: &Path, ms: &[Mem], carpetas: &Path) -> Result<BTreeMap<String, String>> {
    let mut c = omoikane::db::abrir(db)?;
    let mut ids = BTreeMap::new();
    for m in ms {
        let project = match &m.proyecto {
            Some(p) => {
                let d = carpetas.join(p);
                std::fs::create_dir_all(&d)?;
                omoikane::proyecto::de(&d)?
            }
            None => String::new(),
        };
        let n = Nueva {
            project,
            kind: Some(m.tipo),
            title: m.titulo.clone(),
            content: m.contenido.clone(),
            tags: m.etiquetas.clone(),
            source: "eval".into(),
            ..Default::default()
        };
        ids.insert(m.clave.clone(), memoria::guardar(&mut c, n, None)?.id);
    }
    // La base va a copiarse con el fichero solo: nada pendiente en el WAL.
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(ids)
}

fn copiar_base(origen: &Path, destino: &Path) -> Result<()> {
    let ruta = origen.canonicalize().with_context(|| origen.display().to_string())?;
    let c = Connection::open_with_flags(&ruta, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.execute("VACUUM INTO ?1", [destino.to_string_lossy()])?;
    Ok(())
}

/// Lo que inyecta el recall: los 12 primeros caracteres de cada `(id …)` del bloque.
fn ids_inyectados(salida: &str) -> Vec<String> {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(salida.trim()) else { return Vec::new() };
    let texto = j["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    let mut v = Vec::new();
    for (i, _) in texto.match_indices("(id ") {
        let id: String = texto[i + 4..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if !id.is_empty() && !v.contains(&id) {
            v.push(id);
        }
    }
    v
}

fn percentil(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn corto(t: &str, n: usize) -> String {
    let plano = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if plano.chars().count() > n { format!("{}…", plano.chars().take(n).collect::<String>()) } else { plano }
}

struct Lado {
    nombre: &'static str,
    bin: PathBuf,
    env: Vec<(String, String)>,
    casa: PathBuf,
    db: PathBuf,
    /// Lo inyectado en cada caso (de la primera ronda) y los tiempos de todas.
    salida: Vec<Vec<String>>,
    tiempos: Vec<f64>,
    titulos: BTreeMap<String, String>,
}

impl Lado {
    fn recall(&self, caso: usize, ronda: usize, mensaje: &str, cwd: &str) -> Result<(Vec<String>, f64)> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("recall").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Nada de la carpeta ni de la configuración de quien lo lanza.
        for (k, _) in std::env::vars() {
            if k.starts_with("OMOIKANE_") || k == "USER_PROMPT" {
                cmd.env_remove(k);
            }
        }
        cmd.env("OMOIKANE_HOME", &self.casa)
            .env("OMOIKANE_DB", &self.db)
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let entrada = serde_json::json!({
            "session_id": format!("eval-{}-{caso}-{ronda}", self.nombre),
            "cwd": cwd,
            "prompt": mensaje,
            "hook_event_name": "UserPromptSubmit",
        });
        let t = Instant::now();
        let mut hijo = cmd.spawn().with_context(|| self.bin.display().to_string())?;
        hijo.stdin.take().unwrap().write_all(entrada.to_string().as_bytes())?;
        let out = hijo.wait_with_output()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let err = String::from_utf8_lossy(&out.stderr);
        // Con `OMOIKANE_TRAZA` en la configuración, la traza entera; si no, el principio de un error.
        if !err.trim().is_empty() && ronda == 0 {
            let traza = self.env.iter().any(|(k, _)| k == "OMOIKANE_TRAZA");
            eprintln!("[{}] caso {}: {}", self.nombre, caso + 1, if traza { err.to_string() } else { corto(&err, 200) });
        }
        Ok((ids_inyectados(&String::from_utf8_lossy(&out.stdout)), ms))
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut casos_f: Option<PathBuf> = None;
    let (mut base, mut rondas, mut json): (Option<PathBuf>, usize, Option<PathBuf>) = (None, 1, None);
    let (mut a, mut b) = (Config::default(), Config::default());
    let mut i = 0;
    while i < args.len() {
        let valor = || args.get(i + 1).cloned().ok_or_else(|| anyhow!("falta el valor de {}", args[i]));
        let par = |v: String| -> Result<(String, String)> { v.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| anyhow!("{v}: se esperaba K=V")) };
        match args[i].as_str() {
            "--base" => base = Some(valor()?.into()),
            "--a-bin" => a.bin = Some(valor()?.into()),
            "--b-bin" => b.bin = Some(valor()?.into()),
            "--a-env" => a.env.push(par(valor()?)?),
            "--b-env" => b.env.push(par(valor()?)?),
            "--a-extra" => a.extra = Some(valor()?.into()),
            "--b-extra" => b.extra = Some(valor()?.into()),
            "--rondas" => rondas = valor()?.parse()?,
            "--json" => json = Some(valor()?.into()),
            x if !x.starts_with("--") && casos_f.is_none() => {
                casos_f = Some(x.into());
                i += 1;
                continue;
            }
            x => bail!("opción desconocida: {x}"),
        }
        i += 2;
    }
    let casos_f = casos_f.ok_or_else(|| anyhow!("uso: eval CASOS [--base db] [--a-bin|--b-bin omoi] [--a-env|--b-env K=V] [--a-extra|--b-extra tsv] [--rondas n] [--json f]"))?;
    let (mems, casos) = leer_tsv(&casos_f)?;

    // Todo en una carpeta temporal propia, que nunca es la de Omoikane del usuario.
    let tmp = std::env::temp_dir().join(format!("omoi-eval-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tmp = tmp.canonicalize()?;
    if let Some(casa) = std::env::home_dir().map(|h| h.join(".omoikane"))
        && let Ok(casa) = casa.canonicalize()
        && tmp.starts_with(&casa)
    {
        bail!("la carpeta temporal está dentro de {}", casa.display());
    }
    // Se borra al terminar, también si algo falla: puede llevar la copia de una base real.
    let _limpiar = Limpiar(tmp.clone());
    let neutra = tmp.join("cwd");
    std::fs::create_dir_all(&neutra)?;

    let base_db = tmp.join("base.db");
    if let Some(b) = &base {
        copiar_base(b, &base_db)?;
    }
    let carpetas = tmp.join("proyectos");
    let mut claves = guardar(&base_db, &mems, &carpetas)?;

    let release = std::env::current_exe()?.parent().and_then(Path::parent).map(|d| d.join(format!("omoi{}", std::env::consts::EXE_SUFFIX)));
    let mut lados = Vec::new();
    for (nombre, cfg) in [("A", a), ("B", b)] {
        let casa = tmp.join(nombre);
        std::fs::create_dir_all(&casa)?;
        let db = casa.join("omoikane.db");
        std::fs::copy(&base_db, &db)?;
        if let Some(extra) = &cfg.extra {
            let (ms, qs) = leer_tsv(extra)?;
            if !qs.is_empty() {
                bail!("{}: un fichero extra solo lleva memorias (`m`)", extra.display());
            }
            claves.extend(guardar(&db, &ms, &carpetas)?);
        }
        let bin = cfg.bin.or(release.clone()).ok_or_else(|| anyhow!("¿qué omoi? usa --{}-bin", nombre.to_lowercase()))?;
        if !bin.exists() {
            bail!("{} no existe (cargo build --release)", bin.display());
        }
        lados.push(Lado { nombre, bin, env: cfg.env, casa, db, salida: Vec::new(), tiempos: Vec::new(), titulos: BTreeMap::new() });
    }

    // Lo esperado de cada caso, como ids completos (de las claves `m`, de un id o de un título).
    let todas: Vec<(String, String)> = {
        let c = Connection::open_with_flags(&lados[1].db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut q = c.prepare("SELECT id, title FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for l in &mut lados {
        let c = Connection::open_with_flags(&l.db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut q = c.prepare("SELECT id, title FROM memory")?;
        l.titulos = q.query_map([], |f| Ok((f.get::<_, String>(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    }
    let mut esperadas: Vec<BTreeSet<String>> = Vec::new();
    for (n, c) in casos.iter().enumerate() {
        let mut s = BTreeSet::new();
        for e in &c.esperadas {
            let id = claves.get(e).cloned().or_else(|| {
                let por_id: Vec<&String> = todas.iter().map(|x| &x.0).filter(|id| e.len() >= 8 && id.starts_with(e.as_str())).collect();
                let por_titulo: Vec<&String> = todas.iter().filter(|x| x.1.eq_ignore_ascii_case(e)).map(|x| &x.0).collect();
                if por_id.len() == 1 { Some(por_id[0].clone()) } else if por_titulo.len() == 1 { Some(por_titulo[0].clone()) } else { None }
            });
            s.insert(id.ok_or_else(|| anyhow!("caso {}: «{e}» no es una clave, un id ni un título único", n + 1))?);
        }
        esperadas.push(s);
    }

    // Las rondas, intercaladas.
    for ronda in 0..rondas {
        for (n, c) in casos.iter().enumerate() {
            // Una carpeta que ya no existe (temporales de sesiones viejas): la más cercana que siga,
            // que da el mismo proyecto (o ninguno).
            let cwd = match &c.cwd {
                Some(d) if d == "~" => std::env::home_dir().unwrap_or(neutra.clone()).to_string_lossy().into_owned(),
                Some(d) => Path::new(d).ancestors().find(|p| p.exists()).unwrap_or(&neutra).to_string_lossy().into_owned(),
                None => neutra.to_string_lossy().into_owned(),
            };
            let orden: [usize; 2] = if (ronda + n) % 2 == 0 { [0, 1] } else { [1, 0] };
            for k in orden {
                let (ids, ms) = lados[k].recall(n, ronda, &c.mensaje, &cwd)?;
                lados[k].tiempos.push(ms);
                if ronda == 0 {
                    lados[k].salida.push(ids);
                }
            }
        }
    }

    // Puntuación.
    let dentro = |got: &str, e: &BTreeSet<String>| e.iter().any(|id| id.starts_with(got));
    let bien = |n: usize, got: &[String]| -> bool {
        if esperadas[n].is_empty() { got.is_empty() } else { got.iter().any(|g| dentro(g, &esperadas[n])) }
    };
    let positivos = esperadas.iter().filter(|e| !e.is_empty()).count();
    let negativos = casos.len() - positivos;
    println!("# Recall: {} casos ({positivos} con memoria esperada, {negativos} sin ninguna) · {}", casos.len(), casos_f.display());
    if let Some(b) = &base {
        println!("base: copia de {} ({} memorias)", b.display(), todas.len());
    }
    for l in &lados {
        println!("{}: {} {:?}", l.nombre, l.bin.display(), l.env);
    }
    println!();
    println!("| | A | B |\n|---|---|---|");
    let mut filas: Vec<(String, [String; 2])> = Vec::new();
    let mut fila = |nombre: &str, f: &dyn Fn(&Lado) -> String| filas.push((nombre.to_string(), [f(&lados[0]), f(&lados[1])]));
    fila("casos bien", &|l| {
        let n = (0..casos.len()).filter(|&n| bien(n, &l.salida[n])).count();
        format!("{n} de {} ({:.0} %)", casos.len(), 100.0 * n as f64 / casos.len().max(1) as f64)
    });
    fila("con memoria esperada: trae alguna", &|l| {
        let n = (0..casos.len()).filter(|&n| !esperadas[n].is_empty() && bien(n, &l.salida[n])).count();
        format!("{n} de {positivos}")
    });
    fila("sin memoria esperada: nada (correcto)", &|l| {
        let n = (0..casos.len()).filter(|&n| esperadas[n].is_empty() && l.salida[n].is_empty()).count();
        format!("{n} de {negativos}")
    });
    fila("precisión (inyectadas que tocaban)", &|l| {
        let total: usize = l.salida.iter().map(Vec::len).sum();
        let ok: usize = (0..casos.len()).map(|n| l.salida[n].iter().filter(|g| dentro(g, &esperadas[n])).count()).sum();
        format!("{ok} de {total} ({:.0} %)", 100.0 * ok as f64 / total.max(1) as f64)
    });
    fila("cobertura (esperadas que llegan)", &|l| {
        let total: usize = esperadas.iter().map(BTreeSet::len).sum();
        let ok: usize = (0..casos.len()).map(|n| esperadas[n].iter().filter(|e| l.salida[n].iter().any(|g| e.starts_with(g.as_str()))).count()).sum();
        format!("{ok} de {total} ({:.0} %)", 100.0 * ok as f64 / total.max(1) as f64)
    });
    fila("memorias inyectadas por caso", &|l| format!("{:.2}", l.salida.iter().map(Vec::len).sum::<usize>() as f64 / casos.len().max(1) as f64));
    fila("tiempo P50 / P95 (ms)", &|l| format!("{:.1} / {:.1}", percentil(&l.tiempos, 0.5), percentil(&l.tiempos, 0.95)));
    for (nombre, [x, y]) in &filas {
        println!("| {nombre} | {x} | {y} |");
    }

    let titulo = |l: &Lado, g: &str| l.titulos.iter().find(|(id, _)| id.starts_with(g)).map(|(_, t)| corto(t, 60)).unwrap_or_else(|| g.to_string());
    let lista = |l: &Lado, n: usize| -> String {
        if l.salida[n].is_empty() {
            "nada".into()
        } else {
            l.salida[n].iter().map(|g| format!("{}{}", if dentro(g, &esperadas[n]) { "✓ " } else { "✗ " }, titulo(l, g))).collect::<Vec<_>>().join("; ")
        }
    };
    let esperado = |n: usize| -> String {
        if esperadas[n].is_empty() { "nada".into() } else { esperadas[n].iter().map(|e| titulo(&lados[1], &e[..12.min(e.len())])).collect::<Vec<_>>().join("; ") }
    };
    let (mut arreglados, mut rotos, mut otros) = (Vec::new(), Vec::new(), Vec::new());
    for n in 0..casos.len() {
        let (x, y) = (bien(n, &lados[0].salida[n]), bien(n, &lados[1].salida[n]));
        if !x && y {
            arreglados.push(n);
        } else if x && !y {
            rotos.push(n);
        } else if lados[0].salida[n] != lados[1].salida[n] {
            otros.push(n);
        }
    }
    for (cabecera, v) in [("Arreglados (mal en A, bien en B)", &arreglados), ("Rotos (bien en A, mal en B)", &rotos), ("Otros cambios (igual de bien o de mal)", &otros)] {
        println!("\n## {cabecera}: {}", v.len());
        for &n in v.iter() {
            println!("- #{} «{}»\n  esperado: {}\n  A: {}\n  B: {}", n + 1, corto(&casos[n].mensaje, 110), esperado(n), lista(&lados[0], n), lista(&lados[1], n));
        }
    }

    if let Some(f) = json {
        let v: Vec<serde_json::Value> = (0..casos.len())
            .map(|n| {
                serde_json::json!({
                    "caso": n + 1, "mensaje": casos[n].mensaje, "esperadas": esperadas[n],
                    "a": lados[0].salida[n], "b": lados[1].salida[n],
                    "a_titulos": lados[0].salida[n].iter().map(|g| titulo(&lados[0], g)).collect::<Vec<_>>(),
                    "b_titulos": lados[1].salida[n].iter().map(|g| titulo(&lados[1], g)).collect::<Vec<_>>(),
                })
            })
            .collect();
        std::fs::write(&f, serde_json::to_string_pretty(&v)?)?;
    }
    Ok(())
}

struct Limpiar(PathBuf);

impl Drop for Limpiar {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
