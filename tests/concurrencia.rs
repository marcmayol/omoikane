//! Many agents against the same database: real `omoi` processes, at the same time, on a single file
//! (what happens in a software factory with subagents, several sessions and CI).
//!
//! Each agent is a thread that launches one `omoi` after another, as an agent's hooks and plugin
//! would: session start (`recall --session-start`), the recall for each message, `remember`,
//! `omoi tool remember`, `update`, `search`, `omoi tool search` and the close (`recall --session-end`,
//! with its transcript). One of them, if asked, also runs `omoi sync` against a local `--bare`
//! repository. At the end it checks that nothing `omoi` reported as saved was lost and
//! that `memory`, `memory_fts`, `memory_path` and `memory_vec` add up.
//!
//! - `concurrency_short` (CI): 6 agents for 5 s with the binary from the test build.
//! - `concurrency_measured` (by hand, ignored): the real measurement, with the release binary.
//!   `cargo test --release --test concurrencia -- --ignored --nocapture`
//!   Variables: `OMOI_CONC_AGENTES` (20), `OMOI_CONC_SEGUNDOS` (30), `OMOI_CONC_SYNC` (1),
//!   `OMOI_CONC_MEMORIAS` (3000 to start with),
//!   `OMOI_CONC_BIN` (another `omoi`, for example another version's to compare) and `OMOI_MODELO_INT8`
//!   (the int8 model folder; without it, `~/.omoikane/models` is searched, only to copy it).
//!
//! Nothing touches the real database: each process gets `OMOIKANE_DB`, `OMOIKANE_HOME`, `OMOIKANE_MODELS`
//! and the home folder (`USERPROFILE`/`HOME`) in a temporary folder, and that is checked before
//! launching anything.

mod comun;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The longest an `omoi` may take; longer is a hang and it is killed (by its PID, which is ours).
const TIMEOUT: Duration = Duration::from_secs(60);

struct Conf {
    agents: usize,
    seconds: u64,
    bin: PathBuf,
    sync: bool,
    model: Option<PathBuf>,
    /// Starting memories, spread across the three projects (with 2,000 or more, the review after
    /// each write goes in parts, `revisar::al_dia_de`; with fewer, it is redone whole).
    memories: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum OpKind {
    Start,
    Recall,
    Remember,
    ToolRemember,
    Update,
    Search,
    ToolSearch,
    End,
    Sync,
    /// `omoi --version`: what it costs to launch a process with the machine this loaded, without a database.
    Version,
    /// `remember` from everyone at once on a database that doesn't exist yet.
    Cold,
}

impl OpKind {
    fn name(self) -> &'static str {
        match self {
            OpKind::Start => "recall --session-start",
            OpKind::Recall => "recall (mensaje)",
            OpKind::Remember => "remember",
            OpKind::ToolRemember => "tool remember",
            OpKind::Update => "update",
            OpKind::Search => "search",
            OpKind::ToolSearch => "tool search",
            OpKind::End => "recall --session-end",
            OpKind::Sync => "sync",
            OpKind::Version => "--version (sin base)",
            OpKind::Cold => "remember en frío (base nueva)",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Failure {
    /// `database is locked`, SQLITE_BUSY or BUSY_SNAPSHOT.
    Locked,
    Other,
    Timeout,
}

struct Op {
    kind: OpKind,
    ms: f64,
    /// What the process waited for another connection (`TRAZA espera_base`; `omoi` from before 1.7
    /// doesn't report it).
    wait: Option<f64>,
    /// Times it had to reopen the database (`TRAZA abrir_reintento`).
    retries: usize,
    failure: Option<(Failure, String)>,
}

/// What `omoi` reported as saved: the memory and its latest content.
#[derive(Clone)]
struct Written {
    id: String,
    title: String,
    content: String,
}

struct RunOutput {
    ok: bool,
    stdout: String,
    stderr: String,
    ms: f64,
    timed_out: bool,
}

struct Env {
    dir: PathBuf,
    bin: PathBuf,
}

impl Env {
    fn db(&self) -> PathBuf {
        self.dir.join("o.db")
    }

    fn repo(&self, i: usize) -> PathBuf {
        self.dir.join(format!("repo{i}"))
    }

    /// An `omoi` with everything pointing at the temporary folder.
    fn omoi(&self, args: &[&str], cwd: &Path, input: Option<&str>) -> RunOutput {
        let home = self.dir.join("casa");
        let mut c = Command::new(&self.bin);
        c.args(args)
            .current_dir(cwd)
            .env("OMOIKANE_DB", self.db())
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SEMANTIC", "0")
            // Sync only by hand (one agent does it): no stray background processes.
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("OMOIKANE_TRAZA", "1")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let t = Instant::now();
        let mut child = c.spawn().expect("no se puede lanzar omoi");
        if let Some(e) = input {
            let mut s = child.stdin.take().unwrap();
            let _ = s.write_all(e.as_bytes());
        }
        let read_pipe = |mut r: Box<dyn Read + Send>| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                let _ = tx.send(s);
            });
            rx
        };
        let out = read_pipe(Box::new(child.stdout.take().unwrap()));
        let err = read_pipe(Box::new(child.stderr.take().unwrap()));
        // When its output closes, the process has finished (or closed it, and `wait` won't take long).
        let stdout = out.recv_timeout(TIMEOUT);
        let Ok(stdout) = stdout else {
            let _ = child.kill();
            let _ = child.wait();
            return RunOutput { ok: false, stdout: String::new(), stderr: String::new(), ms: t.elapsed().as_secs_f64() * 1000.0, timed_out: true };
        };
        let status = child.wait().expect("wait");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let stderr = err.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        RunOutput { ok: status.success(), stdout, stderr, ms, timed_out: false }
    }
}

/// xorshift: the same distribution on every run.
struct Rng(u64);

impl Rng {
    fn n(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

const WORDS: [&str; 24] = [
    "caché", "puerto", "token", "migración", "índice", "despliegue", "cola", "reintento", "pasarela", "factura", "usuario", "sesión",
    "registro", "plantilla", "consulta", "permiso", "certificado", "contenedor", "tarea", "alerta", "copia", "esquema", "servidor", "cliente",
];

fn phrase(a: &mut Rng, n: usize) -> String {
    (0..n).map(|_| WORDS[a.n(WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
}

/// The id of a saved memory in the output of `remember` or `omoi tool remember`.
fn id_of(text: &str) -> Option<String> {
    text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|p| p.len() == 26 && p.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()))
        .map(str::to_string)
}

fn wait_of(s: &RunOutput) -> Option<f64> {
    let l = s.stderr.lines().find_map(|l| l.strip_prefix("TRAZA espera_base "))?;
    serde_json::from_str::<serde_json::Value>(l).ok()?["ms"].as_f64()
}

fn retries_of(s: &RunOutput) -> usize {
    s.stderr.lines().filter(|l| l.starts_with("TRAZA abrir_reintento ")).count()
}

fn classify(s: &RunOutput, kind: OpKind) -> Option<(Failure, String)> {
    if s.timed_out {
        return Some((Failure::Timeout, "más de 60 s".into()));
    }
    let err = s.stderr.to_lowercase();
    // Recall never fails outwards (a hook must not get in the way): its errors go to stderr.
    let failure = !s.ok || (matches!(kind, OpKind::Start | OpKind::Recall | OpKind::End) && err.contains("omoikane:"));
    if !failure {
        return None;
    }
    let line = s.stderr.lines().filter(|l| !l.starts_with("TRAZA ")).chain(s.stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(160).collect();
    if err.contains("locked") || err.contains("busy") || s.stdout.to_lowercase().contains("database is locked") {
        Some((Failure::Locked, line))
    } else {
        Some((Failure::Other, line))
    }
}

fn hook(session: &str, cwd: &Path, event: &str, extra: serde_json::Value) -> String {
    let mut v = serde_json::json!({ "session_id": session, "cwd": cwd.to_string_lossy(), "hook_event_name": event });
    if let (Some(o), serde_json::Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    v.to_string()
}

/// An agent: starts a session, works until `end` and closes. Returns what it did and what it saved.
fn agent(e: &Env, k: usize, end: Instant, sync: bool) -> (Vec<Op>, Vec<Written>) {
    let mut a = Rng(0x9E37_79B9_7F4A_7C15 ^ ((k as u64 + 1) * 0x2545_F491));
    let repo = e.repo(k % 3);
    let cwd = repo.join("src");
    let session = format!("conc-{k:02}");
    let mut ops = Vec::new();
    let mut written: Vec<Written> = Vec::new();
    let record = |ops: &mut Vec<Op>, kind: OpKind, s: &RunOutput| {
        let failure = classify(s, kind);
        ops.push(Op { kind, ms: s.ms, wait: wait_of(s), retries: retries_of(s), failure: failure.clone() });
        failure.is_none()
    };
    let s = e.omoi(&["recall", "--session-start"], &cwd, Some(&hook(&session, &cwd, "SessionStart", serde_json::json!({ "source": "startup" }))));
    record(&mut ops, OpKind::Start, &s);
    let mut i = 0usize;
    let mut next_sync = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        i += 1;
        if sync && Instant::now() >= next_sync {
            let s = e.omoi(&["sync"], &cwd, None);
            record(&mut ops, OpKind::Sync, &s);
            next_sync = Instant::now() + Duration::from_secs(3);
            continue;
        }
        let mark = format!("conc{k:02}x{i:04}");
        match a.n(100) {
            0..5 => {
                let s = e.omoi(&["--version"], &cwd, None);
                record(&mut ops, OpKind::Version, &s);
            }
            5..40 => {
                let prompt = format!("¿qué sabemos de {}?", phrase(&mut a, 3));
                let s = e.omoi(&["recall"], &cwd, Some(&hook(&session, &cwd, "UserPromptSubmit", serde_json::json!({ "prompt": prompt }))));
                record(&mut ops, OpKind::Recall, &s);
            }
            40..55 => {
                let title = format!("{mark} {}", phrase(&mut a, 3));
                let content = format!("{mark}: {}", phrase(&mut a, 12));
                let s = e.omoi(&["remember", "-k", "fact", "-t", &title, "-f", "--json", "--path", "src/main.rs", &content], &cwd, None);
                if record(&mut ops, OpKind::Remember, &s) {
                    written.push(Written { id: id_of(&s.stdout).unwrap_or_default(), title, content });
                }
            }
            55..70 => {
                let title = format!("{mark} {}", phrase(&mut a, 3));
                let content = format!("{mark}: {}", phrase(&mut a, 12));
                let args = serde_json::json!({ "kind": "gotcha", "title": title, "content": content, "force": true, "paths": ["src/lib.rs"], "tags": ["conc"] });
                let s = e.omoi(&["tool", "remember"], &cwd, Some(&args.to_string()));
                if record(&mut ops, OpKind::ToolRemember, &s) {
                    written.push(Written { id: id_of(&s.stdout).unwrap_or_default(), title, content });
                }
            }
            70..80 if !written.is_empty() => {
                let j = a.n(written.len() as u64) as usize;
                let content = format!("{mark} cambiado: {}", phrase(&mut a, 10));
                let id = written[j].id.clone();
                let s = e.omoi(&["update", &id, "-c", &content], &cwd, None);
                if record(&mut ops, OpKind::Update, &s) {
                    written[j].content = content;
                }
            }
            80..92 => {
                let q = phrase(&mut a, 2);
                let s = e.omoi(&["search", &q], &cwd, None);
                record(&mut ops, OpKind::Search, &s);
            }
            _ => {
                let args = serde_json::json!({ "query": phrase(&mut a, 2) });
                let s = e.omoi(&["tool", "search"], &cwd, Some(&args.to_string()));
                record(&mut ops, OpKind::ToolSearch, &s);
            }
        }
    }
    // The close, with a short transcript in Claude Code's format.
    let transcript = e.dir.join(format!("conversacion-{k:02}.jsonl"));
    let turns = [
        serde_json::json!({ "type": "user", "sessionId": session, "message": { "role": "user", "content": "arregla el reintento de la cola" } }),
        serde_json::json!({ "type": "assistant", "sessionId": session, "message": { "role": "assistant", "content": [{ "type": "text", "text": "Hecho: el reintento ahora espera 2 s." }] } }),
    ];
    let text: String = turns.iter().map(|t| format!("{t}\n")).collect();
    std::fs::write(&transcript, text).unwrap();
    let s = e.omoi(
        &["recall", "--session-end"],
        &cwd,
        Some(&hook(&session, &cwd, "SessionEnd", serde_json::json!({ "transcript_path": transcript.to_string_lossy(), "reason": "exit" }))),
    );
    record(&mut ops, OpKind::End, &s);
    (ops, written)
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i.min(v.len() - 1)]
}

/// What a run produces, to check it or show it.
struct Report {
    ops: Vec<Op>,
    written: usize,
    lost: Vec<String>,
    mismatches: Vec<String>,
    wal_kb: u64,
    text: String,
}

fn real_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".omoikane")
}

fn int8_model() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8").map(PathBuf::from).or_else(|| Some(real_dir().join("models").join("potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

fn run(name: &str, c: &Conf) -> Report {
    let dir = comun::Temporal::nueva(&format!("conc-{name}"));
    for i in 0..3 {
        std::fs::create_dir_all(dir.join(format!("repo{i}")).join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(format!("repo{i}")).join("src")).unwrap();
    }
    std::fs::create_dir_all(dir.join("casa")).unwrap();
    let e = Env { dir: dir.clone(), bin: c.bin.clone() };
    // Before launching anything: the database is the temporary one, not the real one.
    let real = real_dir();
    assert!(e.db().starts_with(std::env::temp_dir()) && !e.db().starts_with(&real), "la base de la prueba no es temporal: {}", e.db().display());
    assert!(!dir.join("home-omoikane").starts_with(&real) && !dir.join("models").starts_with(&real));

    // Cold start: every agent saves at once on a database that doesn't exist yet (CI that
    // starts with a new database). A single database must come out, with a single person.
    let cwd0 = e.repo(0).join("src");
    let mut ops = Vec::new();
    let mut written = Vec::new();
    let cold: Vec<(RunOutput, String, String)> = std::thread::scope(|s| {
        let threads: Vec<_> = (0..c.agents)
            .map(|k| {
                let e = &e;
                s.spawn(move || {
                    let (title, content) = (format!("frio{k:02} {}", WORDS[k % WORDS.len()]), format!("frio{k:02}: base nueva"));
                    let s = e.omoi(&["remember", "-k", "fact", "-t", &title, "-f", "--json", &content], &e.repo(k % 3).join("src"), None);
                    (s, title, content)
                })
            })
            .collect();
        threads.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (s, title, content) in cold {
        let failure = classify(&s, OpKind::Cold);
        if failure.is_none() {
            written.push(Written { id: id_of(&s.stdout).unwrap_or_default(), title, content });
        }
        ops.push(Op { kind: OpKind::Cold, ms: s.ms, wait: wait_of(&s), retries: retries_of(&s), failure });
    }
    // The remaining starting memories, through the library (much faster) and spread by project.
    if c.memories > 0 {
        let mut db = omoikane::db::abrir(&e.db()).unwrap();
        let projects: Vec<String> = (0..3).map(|i| omoikane::proyecto::de(&e.repo(i)).unwrap()).collect();
        for j in 0..c.memories {
            let mut a = Rng(1 + j as u64);
            let n = omoikane::memoria::Nueva {
                project: projects[j % 3].clone(),
                kind: Some(omoikane::memoria::Tipo::Decision),
                title: format!("semilla{j} {}", phrase(&mut a, 3)),
                content: phrase(&mut a, 15),
                source: "semilla".into(),
                ..Default::default()
            };
            omoikane::memoria::guardar(&mut db, n, None).unwrap();
        }
    }
    if let Some(m) = &c.model {
        let s = e.omoi(&["model", "install", "--from", &m.to_string_lossy()], &cwd0, None);
        assert!(s.ok, "model install: {}", s.stderr);
    }
    if c.sync {
        let remote = dir.join("remoto.git");
        let g = Command::new("git").args(["init", "--bare", "-q"]).arg(&remote).output().unwrap();
        assert!(g.status.success());
        let s = e.omoi(&["sync", "init", &remote.to_string_lossy(), "--yes"], &cwd0, None);
        assert!(s.ok, "sync init: {}{}", s.stdout, s.stderr);
    }
    // A session before measuring: the review is up to date with the starting memories.
    let s = e.omoi(&["recall", "--session-start"], &cwd0, Some(&hook("calentar", &cwd0, "SessionStart", serde_json::json!({}))));
    assert!(s.ok, "calentar: {}", s.stderr);

    let end = Instant::now() + Duration::from_secs(c.seconds);
    let t0 = Instant::now();
    let results: Vec<(Vec<Op>, Vec<Written>)> = std::thread::scope(|s| {
        let threads: Vec<_> = (0..c.agents).map(|k| {
            let e = &e;
            s.spawn(move || agent(e, k, end, c.sync && k == 0))
        }).collect();
        threads.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let total = t0.elapsed().as_secs_f64();

    for (o, w) in results {
        ops.extend(o);
        written.extend(w);
    }

    // What is there at the end, read through the library (the same open as `omoi`).
    let wal_kb = std::fs::metadata(dir.join("o.db-wal")).map(|m| m.len() / 1024).unwrap_or(0);
    let db = omoikane::db::abrir(&e.db()).unwrap();
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |f| f.get(0)).unwrap() };
    let mut lost = Vec::new();
    for w in &written {
        let row: Option<(String, String)> = db
            .query_row("SELECT title, content FROM memory WHERE id = ?1", [&w.id], |f| Ok((f.get(0)?, f.get(1)?)))
            .ok();
        match row {
            None => lost.push(format!("falta {} ({})", w.id, w.title)),
            Some((t, cont)) if t != w.title || cont != w.content => lost.push(format!("{} no tiene lo último: {cont}", w.id)),
            _ => {}
        }
    }
    let mut mismatches = Vec::new();
    let n = count("SELECT count(*) FROM memory");
    let mut check_eq = |what: &str, a: i64, b: i64| {
        if a != b {
            mismatches.push(format!("{what}: {a} frente a {b}"));
        }
    };
    check_eq("memory_fts", count("SELECT count(*) FROM memory_fts_docsize"), n);
    check_eq("memory_fts sin memoria", count("SELECT count(*) FROM memory_fts_docsize WHERE id NOT IN (SELECT n FROM memory)"), 0);
    check_eq("memory_path", count("SELECT count(*) FROM memory_path"), count("SELECT COALESCE(sum(json_array_length(paths)), 0) FROM memory"));
    check_eq("memory_version huérfanas", count("SELECT count(*) FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory)"), 0);
    check_eq("proyectos", count("SELECT count(DISTINCT project_id) FROM memory"), 3);
    if c.model.is_some() {
        check_eq("memory_vec", count("SELECT count(*) FROM memory_vec"), n);
    }
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    if integrity != "ok" {
        mismatches.push(format!("integrity_check: {integrity}"));
    }
    drop(db);
    let doctor = e.omoi(&["doctor"], &cwd0, None);
    if !doctor.ok {
        mismatches.push(format!("doctor: {}", doctor.stderr.trim()));
    }

    // The table.
    let mut text = format!(
        "{} agentes, {:.1} s, {} procesos omoi ({:.0}/s), {} memorias al final, {} guardadas por los agentes, WAL {} KB{}{}\n",
        c.agents,
        total,
        ops.len(),
        ops.len() as f64 / total,
        n,
        written.len(),
        wal_kb,
        if c.model.is_some() { ", con modelo" } else { ", sin modelo" },
        if c.sync { ", con sync" } else { "" },
    );
    text.push_str("| operación | n | bloqueada | otro error | tiempo | P50 ms | P95 ms | P99 ms | máx ms | espera media ms |\n|---|---|---|---|---|---|---|---|---|---|\n");
    let mut by_kind: HashMap<OpKind, Vec<&Op>> = HashMap::new();
    for o in &ops {
        by_kind.entry(o.kind).or_default().push(o);
    }
    let mut kinds: Vec<_> = by_kind.keys().copied().collect();
    kinds.sort();
    for t in kinds {
        let v = &by_kind[&t];
        let mut ms: Vec<f64> = v.iter().map(|o| o.ms).collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let count_of = |f: Failure| v.iter().filter(|o| o.failure.as_ref().is_some_and(|(x, _)| *x == f)).count();
        let waits: Vec<f64> = v.iter().filter_map(|o| o.wait).collect();
        let wait = if waits.is_empty() { "-".to_string() } else { format!("{:.1}", waits.iter().sum::<f64>() / waits.len() as f64) };
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.0} | {:.0} | {wait} |\n",
            t.name(),
            v.len(),
            count_of(Failure::Locked),
            count_of(Failure::Other),
            count_of(Failure::Timeout),
            percentile(&ms, 0.5),
            percentile(&ms, 0.95),
            percentile(&ms, 0.99),
            ms.last().copied().unwrap_or(0.0)
        ));
    }
    let mut examples: Vec<String> = ops.iter().filter_map(|o| o.failure.as_ref().map(|(f, l)| format!("{} {f:?}: {l}", o.kind.name()))).collect();
    examples.sort();
    examples.dedup();
    for l in examples.iter().take(12) {
        text.push_str(&format!("  fallo: {l}\n"));
    }
    text.push_str(&format!(
        "aperturas repetidas por SQLITE_BUSY: {}; escrituras perdidas: {}; incoherencias: {}\n",
        ops.iter().map(|o| o.retries).sum::<usize>(),
        lost.len(),
        mismatches.len()
    ));
    for l in lost.iter().take(5).chain(mismatches.iter()) {
        text.push_str(&format!("  {l}\n"));
    }
    Report { ops, written: written.len(), lost, mismatches, wal_kb, text }
}

/// For CI: short and without machine-dependent numbers. No error, nothing lost, everything adds up.
#[test]
fn concurrency_short() {
    let c = Conf { agents: 6, seconds: 5, bin: PathBuf::from(env!("CARGO_BIN_EXE_omoi")), sync: false, model: None, memories: 0 };
    let r = run("corta", &c);
    eprintln!("{}", r.text);
    let failures: Vec<_> = r.ops.iter().filter(|o| o.failure.is_some()).collect();
    assert!(failures.is_empty(), "{} operaciones fallaron\n{}", failures.len(), r.text);
    assert!(r.written > 0, "{}", r.text);
    assert!(r.lost.is_empty() && r.mismatches.is_empty(), "{}", r.text);
    let _ = r.wal_kb;
}

/// The measurement: 20 agents for 30 s with the release binary (see the header).
#[test]
#[ignore]
fn concurrency_measured() {
    let num = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let c = Conf {
        agents: num("OMOI_CONC_AGENTES", 20) as usize,
        seconds: num("OMOI_CONC_SEGUNDOS", 30),
        bin: std::env::var_os("OMOI_CONC_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_omoi"))),
        sync: num("OMOI_CONC_SYNC", 1) == 1,
        model: int8_model(),
        memories: num("OMOI_CONC_MEMORIAS", 3000) as usize,
    };
    let r = run("medida", &c);
    println!("{}", r.text);
}
