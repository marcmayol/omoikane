//! Muchos agentes contra la misma base: procesos `omoi` de verdad, a la vez, sobre un solo fichero
//! (lo que pasa en una software factory con subagentes, varias sesiones y la CI).
//!
//! Cada agente es un hilo que lanza un `omoi` tras otro, como lo harían los hooks y el plugin de un
//! agente: arranque de sesión (`recall --session-start`), el recall de cada mensaje, `remember`,
//! `omoi tool remember`, `update`, `search`, `omoi tool search` y el cierre (`recall --session-end`,
//! con su conversación). Uno de ellos, si se pide, hace además `omoi sync` contra un repositorio
//! `--bare` local. Al final se comprueba que no se ha perdido nada de lo que `omoi` dio por guardado y
//! que `memory`, `memory_fts`, `memory_path` y `memory_vec` cuadran.
//!
//! - `concurrencia_corta` (CI): 6 agentes durante 5 s con el binario de la compilación de los tests.
//! - `concurrencia_medida` (a mano, ignorado): la medida de verdad, con el binario release.
//!   `cargo test --release --test concurrencia -- --ignored --nocapture`
//!   Variables: `OMOI_CONC_AGENTES` (20), `OMOI_CONC_SEGUNDOS` (30), `OMOI_CONC_SYNC` (1),
//!   `OMOI_CONC_MEMORIAS` (3000 de partida),
//!   `OMOI_CONC_BIN` (otro `omoi`, por ejemplo el de otra versión para comparar) y `OMOI_MODELO_INT8`
//!   (la carpeta del modelo int8; sin él se busca en `~/.omoikane/models`, solo para copiarlo).
//!
//! Nada toca la base de verdad: cada proceso lleva `OMOIKANE_DB`, `OMOIKANE_HOME`, `OMOIKANE_MODELS`
//! y la carpeta personal (`USERPROFILE`/`HOME`) en una carpeta temporal, y se comprueba antes de
//! lanzar nada.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Lo más que puede tardar un `omoi`; más es un cuelgue y se corta (por su PID, que es nuestro).
const LIMITE: Duration = Duration::from_secs(60);

struct Conf {
    agentes: usize,
    segundos: u64,
    bin: PathBuf,
    sync: bool,
    modelo: Option<PathBuf>,
    /// Memorias de partida, repartidas entre los tres proyectos (con 2.000 o más, la revisión tras
    /// cada escritura va por partes, `revisar::al_dia_de`; con menos, se rehace entera).
    memorias: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Tipo {
    Empezar,
    Recall,
    Remember,
    ToolRemember,
    Update,
    Search,
    ToolSearch,
    Cerrar,
    Sync,
    /// `omoi --version`: lo que cuesta lanzar un proceso con la máquina así de cargada, sin base.
    Version,
    /// `remember` de todos a la vez en una base que aún no existe.
    Frio,
}

impl Tipo {
    fn nombre(self) -> &'static str {
        match self {
            Tipo::Empezar => "recall --session-start",
            Tipo::Recall => "recall (mensaje)",
            Tipo::Remember => "remember",
            Tipo::ToolRemember => "tool remember",
            Tipo::Update => "update",
            Tipo::Search => "search",
            Tipo::ToolSearch => "tool search",
            Tipo::Cerrar => "recall --session-end",
            Tipo::Sync => "sync",
            Tipo::Version => "--version (sin base)",
            Tipo::Frio => "remember en frío (base nueva)",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Fallo {
    /// `database is locked`, SQLITE_BUSY o BUSY_SNAPSHOT.
    Bloqueada,
    Otro,
    Tiempo,
}

struct Op {
    tipo: Tipo,
    ms: f64,
    /// Lo que el proceso esperó a otra conexión (`TRAZA espera_base`; los `omoi` de antes de la 1.7
    /// no lo dicen).
    espera: Option<f64>,
    /// Veces que tuvo que volver a abrir la base (`TRAZA abrir_reintento`).
    reintentos: usize,
    fallo: Option<(Fallo, String)>,
}

/// Lo que `omoi` dio por guardado: la memoria y su último contenido.
#[derive(Clone)]
struct Escrita {
    id: String,
    titulo: String,
    contenido: String,
}

struct Salida {
    ok: bool,
    stdout: String,
    stderr: String,
    ms: f64,
    tiempo: bool,
}

struct Entorno {
    dir: PathBuf,
    bin: PathBuf,
}

impl Entorno {
    fn db(&self) -> PathBuf {
        self.dir.join("o.db")
    }

    fn repo(&self, i: usize) -> PathBuf {
        self.dir.join(format!("repo{i}"))
    }

    /// Un `omoi` con todo apuntando a la carpeta temporal.
    fn omoi(&self, args: &[&str], cwd: &Path, entrada: Option<&str>) -> Salida {
        let casa = self.dir.join("casa");
        let mut c = Command::new(&self.bin);
        c.args(args)
            .current_dir(cwd)
            .env("OMOIKANE_DB", self.db())
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SEMANTIC", "0")
            // La sincronización solo a mano (la hace un agente): nada de procesos sueltos de fondo.
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("OMOIKANE_TRAZA", "1")
            .env("USERPROFILE", &casa)
            .env("HOME", &casa)
            .env("GIT_CONFIG_GLOBAL", casa.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let t = Instant::now();
        let mut hijo = c.spawn().expect("no se puede lanzar omoi");
        if let Some(e) = entrada {
            let mut s = hijo.stdin.take().unwrap();
            let _ = s.write_all(e.as_bytes());
        }
        let leer = |mut r: Box<dyn Read + Send>| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                let _ = tx.send(s);
            });
            rx
        };
        let out = leer(Box::new(hijo.stdout.take().unwrap()));
        let err = leer(Box::new(hijo.stderr.take().unwrap()));
        // Cuando se cierra su salida, el proceso ha terminado (o la ha cerrado, y `wait` no tarda).
        let stdout = out.recv_timeout(LIMITE);
        let Ok(stdout) = stdout else {
            let _ = hijo.kill();
            let _ = hijo.wait();
            return Salida { ok: false, stdout: String::new(), stderr: String::new(), ms: t.elapsed().as_secs_f64() * 1000.0, tiempo: true };
        };
        let estado = hijo.wait().expect("wait");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let stderr = err.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        Salida { ok: estado.success(), stdout, stderr, ms, tiempo: false }
    }
}

/// xorshift: el mismo reparto en cada ejecución.
struct Azar(u64);

impl Azar {
    fn n(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

const PALABRAS: [&str; 24] = [
    "caché", "puerto", "token", "migración", "índice", "despliegue", "cola", "reintento", "pasarela", "factura", "usuario", "sesión",
    "registro", "plantilla", "consulta", "permiso", "certificado", "contenedor", "tarea", "alerta", "copia", "esquema", "servidor", "cliente",
];

fn frase(a: &mut Azar, n: usize) -> String {
    (0..n).map(|_| PALABRAS[a.n(PALABRAS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
}

/// El id de una memoria guardada en la salida de `remember` o de `omoi tool remember`.
fn id_de(texto: &str) -> Option<String> {
    texto
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|p| p.len() == 26 && p.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()))
        .map(str::to_string)
}

fn espera_de(s: &Salida) -> Option<f64> {
    let l = s.stderr.lines().find_map(|l| l.strip_prefix("TRAZA espera_base "))?;
    serde_json::from_str::<serde_json::Value>(l).ok()?["ms"].as_f64()
}

fn reintentos_de(s: &Salida) -> usize {
    s.stderr.lines().filter(|l| l.starts_with("TRAZA abrir_reintento ")).count()
}

fn clasificar(s: &Salida, tipo: Tipo) -> Option<(Fallo, String)> {
    if s.tiempo {
        return Some((Fallo::Tiempo, "más de 60 s".into()));
    }
    let err = s.stderr.to_lowercase();
    // El recall nunca falla hacia fuera (un hook no debe molestar): sus errores van a stderr.
    let fallo = !s.ok || (matches!(tipo, Tipo::Empezar | Tipo::Recall | Tipo::Cerrar) && err.contains("omoikane:"));
    if !fallo {
        return None;
    }
    let linea = s.stderr.lines().filter(|l| !l.starts_with("TRAZA ")).chain(s.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(160).collect();
    if err.contains("locked") || err.contains("busy") || s.stdout.to_lowercase().contains("database is locked") {
        Some((Fallo::Bloqueada, linea))
    } else {
        Some((Fallo::Otro, linea))
    }
}

fn hook(sesion: &str, cwd: &Path, evento: &str, extra: serde_json::Value) -> String {
    let mut v = serde_json::json!({ "session_id": sesion, "cwd": cwd.to_string_lossy(), "hook_event_name": evento });
    if let (Some(o), serde_json::Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    v.to_string()
}

/// Un agente: arranca sesión, trabaja hasta `fin` y cierra. Devuelve lo que hizo y lo que guardó.
fn agente(e: &Entorno, k: usize, fin: Instant, sync: bool) -> (Vec<Op>, Vec<Escrita>) {
    let mut a = Azar(0x9E37_79B9_7F4A_7C15 ^ ((k as u64 + 1) * 0x2545_F491));
    let repo = e.repo(k % 3);
    let cwd = repo.join("src");
    let sesion = format!("conc-{k:02}");
    let mut ops = Vec::new();
    let mut escritas: Vec<Escrita> = Vec::new();
    let nota = |ops: &mut Vec<Op>, tipo: Tipo, s: &Salida| {
        let fallo = clasificar(s, tipo);
        ops.push(Op { tipo, ms: s.ms, espera: espera_de(s), reintentos: reintentos_de(s), fallo: fallo.clone() });
        fallo.is_none()
    };
    let s = e.omoi(&["recall", "--session-start"], &cwd, Some(&hook(&sesion, &cwd, "SessionStart", serde_json::json!({ "source": "startup" }))));
    nota(&mut ops, Tipo::Empezar, &s);
    let mut i = 0usize;
    let mut proxima_sync = Instant::now() + Duration::from_secs(2);
    while Instant::now() < fin {
        i += 1;
        if sync && Instant::now() >= proxima_sync {
            let s = e.omoi(&["sync"], &cwd, None);
            nota(&mut ops, Tipo::Sync, &s);
            proxima_sync = Instant::now() + Duration::from_secs(3);
            continue;
        }
        let marca = format!("conc{k:02}x{i:04}");
        match a.n(100) {
            0..5 => {
                let s = e.omoi(&["--version"], &cwd, None);
                nota(&mut ops, Tipo::Version, &s);
            }
            5..40 => {
                let prompt = format!("¿qué sabemos de {}?", frase(&mut a, 3));
                let s = e.omoi(&["recall"], &cwd, Some(&hook(&sesion, &cwd, "UserPromptSubmit", serde_json::json!({ "prompt": prompt }))));
                nota(&mut ops, Tipo::Recall, &s);
            }
            40..55 => {
                let titulo = format!("{marca} {}", frase(&mut a, 3));
                let contenido = format!("{marca}: {}", frase(&mut a, 12));
                let s = e.omoi(&["remember", "-k", "fact", "-t", &titulo, "-f", "--json", "--path", "src/main.rs", &contenido], &cwd, None);
                if nota(&mut ops, Tipo::Remember, &s) {
                    escritas.push(Escrita { id: id_de(&s.stdout).unwrap_or_default(), titulo, contenido });
                }
            }
            55..70 => {
                let titulo = format!("{marca} {}", frase(&mut a, 3));
                let contenido = format!("{marca}: {}", frase(&mut a, 12));
                let args = serde_json::json!({ "kind": "gotcha", "title": titulo, "content": contenido, "force": true, "paths": ["src/lib.rs"], "tags": ["conc"] });
                let s = e.omoi(&["tool", "remember"], &cwd, Some(&args.to_string()));
                if nota(&mut ops, Tipo::ToolRemember, &s) {
                    escritas.push(Escrita { id: id_de(&s.stdout).unwrap_or_default(), titulo, contenido });
                }
            }
            70..80 if !escritas.is_empty() => {
                let j = a.n(escritas.len() as u64) as usize;
                let contenido = format!("{marca} cambiado: {}", frase(&mut a, 10));
                let id = escritas[j].id.clone();
                let s = e.omoi(&["update", &id, "-c", &contenido], &cwd, None);
                if nota(&mut ops, Tipo::Update, &s) {
                    escritas[j].contenido = contenido;
                }
            }
            80..92 => {
                let q = frase(&mut a, 2);
                let s = e.omoi(&["search", &q], &cwd, None);
                nota(&mut ops, Tipo::Search, &s);
            }
            _ => {
                let args = serde_json::json!({ "query": frase(&mut a, 2) });
                let s = e.omoi(&["tool", "search"], &cwd, Some(&args.to_string()));
                nota(&mut ops, Tipo::ToolSearch, &s);
            }
        }
    }
    // El cierre, con una conversación corta en el formato de Claude Code.
    let conversacion = e.dir.join(format!("conversacion-{k:02}.jsonl"));
    let turnos = [
        serde_json::json!({ "type": "user", "sessionId": sesion, "message": { "role": "user", "content": "arregla el reintento de la cola" } }),
        serde_json::json!({ "type": "assistant", "sessionId": sesion, "message": { "role": "assistant", "content": [{ "type": "text", "text": "Hecho: el reintento ahora espera 2 s." }] } }),
    ];
    let texto: String = turnos.iter().map(|t| format!("{t}\n")).collect();
    std::fs::write(&conversacion, texto).unwrap();
    let s = e.omoi(
        &["recall", "--session-end"],
        &cwd,
        Some(&hook(&sesion, &cwd, "SessionEnd", serde_json::json!({ "transcript_path": conversacion.to_string_lossy(), "reason": "exit" }))),
    );
    nota(&mut ops, Tipo::Cerrar, &s);
    (ops, escritas)
}

fn percentil(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i.min(v.len() - 1)]
}

/// Lo que sale de una ejecución, para comprobarlo o enseñarlo.
struct Informe {
    ops: Vec<Op>,
    escritas: usize,
    perdidas: Vec<String>,
    incoherencias: Vec<String>,
    wal_kb: u64,
    texto: String,
}

fn ruta_real() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".omoikane")
}

fn modelo_int8() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8").map(PathBuf::from).or_else(|| Some(ruta_real().join("models").join("potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

fn ejecutar(nombre: &str, c: &Conf) -> Informe {
    let dir = std::env::temp_dir().join(format!("omoi-conc-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for i in 0..3 {
        std::fs::create_dir_all(dir.join(format!("repo{i}")).join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(format!("repo{i}")).join("src")).unwrap();
    }
    std::fs::create_dir_all(dir.join("casa")).unwrap();
    let e = Entorno { dir: dir.clone(), bin: c.bin.clone() };
    // Antes de lanzar nada: la base es la temporal, y no la de verdad.
    let real = ruta_real();
    assert!(e.db().starts_with(std::env::temp_dir()) && !e.db().starts_with(&real), "la base de la prueba no es temporal: {}", e.db().display());
    assert!(!dir.join("home-omoikane").starts_with(&real) && !dir.join("models").starts_with(&real));

    // Arranque en frío: todos los agentes guardan a la vez en una base que aún no existe (la CI que
    // empieza con una base nueva). Tiene que salir una sola base, con una sola persona.
    let cwd0 = e.repo(0).join("src");
    let mut ops = Vec::new();
    let mut escritas = Vec::new();
    let frio: Vec<(Salida, String, String)> = std::thread::scope(|s| {
        let hilos: Vec<_> = (0..c.agentes)
            .map(|k| {
                let e = &e;
                s.spawn(move || {
                    let (titulo, contenido) = (format!("frio{k:02} {}", PALABRAS[k % PALABRAS.len()]), format!("frio{k:02}: base nueva"));
                    let s = e.omoi(&["remember", "-k", "fact", "-t", &titulo, "-f", "--json", &contenido], &e.repo(k % 3).join("src"), None);
                    (s, titulo, contenido)
                })
            })
            .collect();
        hilos.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (s, titulo, contenido) in frio {
        let fallo = clasificar(&s, Tipo::Frio);
        if fallo.is_none() {
            escritas.push(Escrita { id: id_de(&s.stdout).unwrap_or_default(), titulo, contenido });
        }
        ops.push(Op { tipo: Tipo::Frio, ms: s.ms, espera: espera_de(&s), reintentos: reintentos_de(&s), fallo });
    }
    // Las demás memorias de partida, por la biblioteca (es mucho más rápido) y repartidas por proyecto.
    if c.memorias > 0 {
        let mut db = omoikane::db::abrir(&e.db()).unwrap();
        let proyectos: Vec<String> = (0..3).map(|i| omoikane::proyecto::de(&e.repo(i)).unwrap()).collect();
        for j in 0..c.memorias {
            let mut a = Azar(1 + j as u64);
            let n = omoikane::memoria::Nueva {
                project: proyectos[j % 3].clone(),
                kind: Some(omoikane::memoria::Tipo::Decision),
                title: format!("semilla{j} {}", frase(&mut a, 3)),
                content: frase(&mut a, 15),
                source: "semilla".into(),
                ..Default::default()
            };
            omoikane::memoria::guardar(&mut db, n, None).unwrap();
        }
    }
    if let Some(m) = &c.modelo {
        let s = e.omoi(&["model", "install", "--from", &m.to_string_lossy()], &cwd0, None);
        assert!(s.ok, "model install: {}", s.stderr);
    }
    if c.sync {
        let remoto = dir.join("remoto.git");
        let g = Command::new("git").args(["init", "--bare", "-q"]).arg(&remoto).output().unwrap();
        assert!(g.status.success());
        let s = e.omoi(&["sync", "init", &remoto.to_string_lossy(), "--yes"], &cwd0, None);
        assert!(s.ok, "sync init: {}{}", s.stdout, s.stderr);
    }
    // Una sesión antes de medir: la revisión queda al día con las memorias de partida.
    let s = e.omoi(&["recall", "--session-start"], &cwd0, Some(&hook("calentar", &cwd0, "SessionStart", serde_json::json!({}))));
    assert!(s.ok, "calentar: {}", s.stderr);

    let fin = Instant::now() + Duration::from_secs(c.segundos);
    let t0 = Instant::now();
    let resultados: Vec<(Vec<Op>, Vec<Escrita>)> = std::thread::scope(|s| {
        let hilos: Vec<_> = (0..c.agentes).map(|k| {
            let e = &e;
            s.spawn(move || agente(e, k, fin, c.sync && k == 0))
        }).collect();
        hilos.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let total = t0.elapsed().as_secs_f64();

    for (o, w) in resultados {
        ops.extend(o);
        escritas.extend(w);
    }

    // Lo que hay al final, leído por la biblioteca (la misma apertura que `omoi`).
    let wal_kb = std::fs::metadata(dir.join("o.db-wal")).map(|m| m.len() / 1024).unwrap_or(0);
    let db = omoikane::db::abrir(&e.db()).unwrap();
    let cuenta = |sql: &str| -> i64 { db.query_row(sql, [], |f| f.get(0)).unwrap() };
    let mut perdidas = Vec::new();
    for w in &escritas {
        let fila: Option<(String, String)> = db
            .query_row("SELECT title, content FROM memory WHERE id = ?1", [&w.id], |f| Ok((f.get(0)?, f.get(1)?)))
            .ok();
        match fila {
            None => perdidas.push(format!("falta {} ({})", w.id, w.titulo)),
            Some((t, cont)) if t != w.titulo || cont != w.contenido => perdidas.push(format!("{} no tiene lo último: {cont}", w.id)),
            _ => {}
        }
    }
    let mut incoherencias = Vec::new();
    let n = cuenta("SELECT count(*) FROM memory");
    let mut igual = |que: &str, a: i64, b: i64| {
        if a != b {
            incoherencias.push(format!("{que}: {a} frente a {b}"));
        }
    };
    igual("memory_fts", cuenta("SELECT count(*) FROM memory_fts_docsize"), n);
    igual("memory_fts sin memoria", cuenta("SELECT count(*) FROM memory_fts_docsize WHERE id NOT IN (SELECT n FROM memory)"), 0);
    igual("memory_path", cuenta("SELECT count(*) FROM memory_path"), cuenta("SELECT COALESCE(sum(json_array_length(paths)), 0) FROM memory"));
    igual("memory_version huérfanas", cuenta("SELECT count(*) FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory)"), 0);
    igual("proyectos", cuenta("SELECT count(DISTINCT project_id) FROM memory"), 3);
    if c.modelo.is_some() {
        igual("memory_vec", cuenta("SELECT count(*) FROM memory_vec"), n);
    }
    let integridad: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    if integridad != "ok" {
        incoherencias.push(format!("integrity_check: {integridad}"));
    }
    drop(db);
    let doctor = e.omoi(&["doctor"], &cwd0, None);
    if !doctor.ok {
        incoherencias.push(format!("doctor: {}", doctor.stderr.trim()));
    }

    // La tabla.
    let mut texto = format!(
        "{} agentes, {:.1} s, {} procesos omoi ({:.0}/s), {} memorias al final, {} guardadas por los agentes, WAL {} KB{}{}\n",
        c.agentes,
        total,
        ops.len(),
        ops.len() as f64 / total,
        n,
        escritas.len(),
        wal_kb,
        if c.modelo.is_some() { ", con modelo" } else { ", sin modelo" },
        if c.sync { ", con sync" } else { "" },
    );
    texto.push_str("| operación | n | bloqueada | otro error | tiempo | P50 ms | P95 ms | P99 ms | máx ms | espera media ms |\n|---|---|---|---|---|---|---|---|---|---|\n");
    let mut por: HashMap<Tipo, Vec<&Op>> = HashMap::new();
    for o in &ops {
        por.entry(o.tipo).or_default().push(o);
    }
    let mut tipos: Vec<_> = por.keys().copied().collect();
    tipos.sort();
    for t in tipos {
        let v = &por[&t];
        let mut ms: Vec<f64> = v.iter().map(|o| o.ms).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let cuantas = |f: Fallo| v.iter().filter(|o| o.fallo.as_ref().is_some_and(|(x, _)| *x == f)).count();
        let esperas: Vec<f64> = v.iter().filter_map(|o| o.espera).collect();
        let espera = if esperas.is_empty() { "-".to_string() } else { format!("{:.1}", esperas.iter().sum::<f64>() / esperas.len() as f64) };
        texto.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.0} | {:.0} | {espera} |\n",
            t.nombre(),
            v.len(),
            cuantas(Fallo::Bloqueada),
            cuantas(Fallo::Otro),
            cuantas(Fallo::Tiempo),
            percentil(&ms, 0.5),
            percentil(&ms, 0.95),
            percentil(&ms, 0.99),
            ms.last().copied().unwrap_or(0.0)
        ));
    }
    let mut ejemplos: Vec<String> = ops.iter().filter_map(|o| o.fallo.as_ref().map(|(f, l)| format!("{} {f:?}: {l}", o.tipo.nombre()))).collect();
    ejemplos.sort();
    ejemplos.dedup();
    for l in ejemplos.iter().take(12) {
        texto.push_str(&format!("  fallo: {l}\n"));
    }
    texto.push_str(&format!(
        "aperturas repetidas por SQLITE_BUSY: {}; escrituras perdidas: {}; incoherencias: {}\n",
        ops.iter().map(|o| o.reintentos).sum::<usize>(),
        perdidas.len(),
        incoherencias.len()
    ));
    for l in perdidas.iter().take(5).chain(incoherencias.iter()) {
        texto.push_str(&format!("  {l}\n"));
    }
    let informe = Informe { ops, escritas: escritas.len(), perdidas, incoherencias, wal_kb, texto };
    let _ = std::fs::remove_dir_all(&dir);
    informe
}

/// Para CI: corto y sin cifras que dependan de la máquina. Ningún error, nada perdido, todo cuadra.
#[test]
fn concurrencia_corta() {
    let c = Conf { agentes: 6, segundos: 5, bin: PathBuf::from(env!("CARGO_BIN_EXE_omoi")), sync: false, modelo: None, memorias: 0 };
    let r = ejecutar("corta", &c);
    eprintln!("{}", r.texto);
    let fallos: Vec<_> = r.ops.iter().filter(|o| o.fallo.is_some()).collect();
    assert!(fallos.is_empty(), "{} operaciones fallaron\n{}", fallos.len(), r.texto);
    assert!(r.escritas > 0, "{}", r.texto);
    assert!(r.perdidas.is_empty() && r.incoherencias.is_empty(), "{}", r.texto);
    let _ = r.wal_kb;
}

/// La medida: 20 agentes durante 30 s con el binario release (ver la cabecera).
#[test]
#[ignore]
fn concurrencia_medida() {
    let num = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let c = Conf {
        agentes: num("OMOI_CONC_AGENTES", 20) as usize,
        segundos: num("OMOI_CONC_SEGUNDOS", 30),
        bin: std::env::var_os("OMOI_CONC_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_omoi"))),
        sync: num("OMOI_CONC_SYNC", 1) == 1,
        modelo: modelo_int8(),
        memorias: num("OMOI_CONC_MEMORIAS", 3000) as usize,
    };
    let r = ejecutar("medida", &c);
    println!("{}", r.texto);
}
