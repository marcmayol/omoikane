//! Recall comparator, before and after a change: the same questions with two configurations,
//! deterministically and without a judge model, and the list of what the change fixes and what it breaks.
//!
//!   cargo build --release && cargo run --release --example eval -- CASES [options]
//!
//! CASES is a TSV like `tests/busquedas.tsv` (lines starting with `#` are ignored):
//!
//!   m <key> <title> <content> [<kind> [<tags,with,commas> [<project>]]]   a memory for the database
//!                                                                  (global, or of the project of a folder
//!                                                                  `<project>` created in the temp dir)
//!   q <expected> <message> [<cwd>]                                   a case
//!
//! `<expected>`: what recall should inject, separated by `|`: keys of `m` lines, ids (or their
//! start, from 8 characters) or exact titles in the database; `-` if it should inject nothing. In a
//! memory's content, `\n` is a line break (not in the message: Windows paths contain
//! it). Without `<cwd>`, a temporary folder that is not a project; `~`, the user's folder (which sees
//! every project, like someone who opens sessions there).
//!
//! Options:
//!   --base <db>          start from a COPY of this database (opened read-only and copied with
//!                        `VACUUM INTO`); the `m` lines are added to the copy
//!   --a-bin / --b-bin    the `omoi` of each configuration (by default, the one in `target/release`)
//!   --a-env / --b-env    K=V for that configuration (can be repeated)
//!   --a-extra / --b-extra <tsv>  memories (`m` lines) that only that configuration's database has
//!   --rondas <n>         passes per case to measure time (default 1)
//!   --json <file>        what each configuration injected in each case, to look at later
//!
//! Each configuration has its own copy of the database and its own Omoikane folder
//! (`OMOIKANE_HOME`, `OMOIKANE_DB`), without sync, without updates and with the semantic
//! model off (unless `--a-env`/`--b-env` say otherwise); the `OMOIKANE_*` variables of
//! whoever launches it do not reach the processes. Each case is a new `omoi recall`, as the hook launches it,
//! with a new session; the configurations are interleaved (A,B in one round, B,A in the next)
//! so that the disk cache favours neither.
//!
//! A case with expected memories is right if recall brings any of them; one without them (`-`),
//! if it brings nothing. «Fixed»: wrong in A and right in B; «broken»: the other way round. Also, per configuration:
//! precision (of what was injected, what was expected), recall coverage (of what was expected, what was injected) and «correctly
//! nothing» (`-` cases where it injected nothing).

use anyhow::{Context, Result, anyhow, bail};
use omoikane::memoria::{self, NewMemory, Kind};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

struct Mem {
    key: String,
    title: String,
    content: String,
    kind: Kind,
    tags: Vec<String>,
    project: Option<String>,
}

struct Case {
    expected: Vec<String>,
    message: String,
    cwd: Option<String>,
}

#[derive(Default)]
struct Config {
    bin: Option<PathBuf>,
    env: Vec<(String, String)>,
    extra: Option<PathBuf>,
}

fn read_tsv(f: &Path) -> Result<(Vec<Mem>, Vec<Case>)> {
    let text = std::fs::read_to_string(f).with_context(|| f.display().to_string())?;
    let (mut ms, mut qs) = (Vec::new(), Vec::new());
    for (i, l) in text.lines().enumerate() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let c: Vec<&str> = l.split('\t').collect();
        match c.as_slice() {
            ["m", key, title, content, rest @ ..] => ms.push(Mem {
                key: key.to_string(),
                title: title.to_string(),
                content: content.replace("\\n", "\n"),
                kind: rest.first().filter(|t| !t.is_empty()).map(|t| t.parse()).transpose()?.unwrap_or(Kind::Fact),
                tags: rest.get(1).map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
                project: rest.get(2).filter(|p| !p.is_empty()).map(|p| p.to_string()),
            }),
            ["q", expected, message, rest @ ..] => qs.push(Case {
                expected: if *expected == "-" { vec![] } else { expected.split('|').map(|x| x.trim().to_string()).collect() },
                message: message.to_string(),
                cwd: rest.first().filter(|c| !c.is_empty()).map(|c| c.to_string()),
            }),
            _ => bail!("{}:{}: línea que no es `m` ni `q`", f.display(), i + 1),
        }
    }
    Ok((ms, qs))
}

/// Saves the memories in the database and returns key → id. Project ones, in a folder named after
/// the project inside `folders`.
fn save(db: &Path, ms: &[Mem], folders: &Path) -> Result<BTreeMap<String, String>> {
    let mut c = omoikane::db::open(db)?;
    let mut ids = BTreeMap::new();
    for m in ms {
        let project = match &m.project {
            Some(p) => {
                let d = folders.join(p);
                std::fs::create_dir_all(&d)?;
                omoikane::proyecto::of_dir(&d)?
            }
            None => String::new(),
        };
        let n = NewMemory {
            project,
            kind: Some(m.kind),
            title: m.title.clone(),
            content: m.content.clone(),
            tags: m.tags.clone(),
            source: "eval".into(),
            ..Default::default()
        };
        ids.insert(m.key.clone(), memoria::save(&mut c, n, None)?.id);
    }
    // The database will be copied with the file alone: nothing pending in the WAL.
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(ids)
}

fn copy_db(source: &Path, dest: &Path) -> Result<()> {
    let path = source.canonicalize().with_context(|| source.display().to_string())?;
    let c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
    Ok(())
}

/// What recall injects: the first 12 characters of each `(id …)` in the block.
fn injected_ids(output: &str) -> Vec<String> {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(output.trim()) else { return Vec::new() };
    let text = j["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    let mut v = Vec::new();
    for (i, _) in text.match_indices("(id ") {
        let id: String = text[i + 4..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if !id.is_empty() && !v.contains(&id) {
            v.push(id);
        }
    }
    v
}

fn percentile(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn short(t: &str, n: usize) -> String {
    let flat = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n { format!("{}…", flat.chars().take(n).collect::<String>()) } else { flat }
}

struct Side {
    name: &'static str,
    bin: PathBuf,
    env: Vec<(String, String)>,
    home: PathBuf,
    db: PathBuf,
    /// What was injected in each case (from the first round) and the times of all rounds.
    output: Vec<Vec<String>>,
    times: Vec<f64>,
    titles: BTreeMap<String, String>,
}

impl Side {
    fn recall(&self, case: usize, round: usize, message: &str, cwd: &str) -> Result<(Vec<String>, f64)> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("recall").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Nothing from the folder or configuration of whoever launches it.
        for (k, _) in std::env::vars() {
            if k.starts_with("OMOIKANE_") || k == "USER_PROMPT" {
                cmd.env_remove(k);
            }
        }
        cmd.env("OMOIKANE_HOME", &self.home)
            .env("OMOIKANE_DB", &self.db)
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0");
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let input = serde_json::json!({
            "session_id": format!("eval-{}-{case}-{round}", self.name),
            "cwd": cwd,
            "prompt": message,
            "hook_event_name": "UserPromptSubmit",
        });
        let t = Instant::now();
        let mut child = cmd.spawn().with_context(|| self.bin.display().to_string())?;
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes())?;
        let out = child.wait_with_output()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let err = String::from_utf8_lossy(&out.stderr);
        // With `OMOIKANE_TRAZA` in the configuration, the whole trace; otherwise, the start of an error.
        if !err.trim().is_empty() && round == 0 {
            let trace = self.env.iter().any(|(k, _)| k == "OMOIKANE_TRAZA");
            eprintln!("[{}] caso {}: {}", self.name, case + 1, if trace { err.to_string() } else { short(&err, 200) });
        }
        Ok((injected_ids(&String::from_utf8_lossy(&out.stdout)), ms))
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cases_file: Option<PathBuf> = None;
    let (mut base, mut rounds, mut json): (Option<PathBuf>, usize, Option<PathBuf>) = (None, 1, None);
    let (mut a, mut b) = (Config::default(), Config::default());
    let mut i = 0;
    while i < args.len() {
        let value = || args.get(i + 1).cloned().ok_or_else(|| anyhow!("falta el valor de {}", args[i]));
        let pair = |v: String| -> Result<(String, String)> { v.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| anyhow!("{v}: se esperaba K=V")) };
        match args[i].as_str() {
            "--base" => base = Some(value()?.into()),
            "--a-bin" => a.bin = Some(value()?.into()),
            "--b-bin" => b.bin = Some(value()?.into()),
            "--a-env" => a.env.push(pair(value()?)?),
            "--b-env" => b.env.push(pair(value()?)?),
            "--a-extra" => a.extra = Some(value()?.into()),
            "--b-extra" => b.extra = Some(value()?.into()),
            "--rondas" => rounds = value()?.parse()?,
            "--json" => json = Some(value()?.into()),
            x if !x.starts_with("--") && cases_file.is_none() => {
                cases_file = Some(x.into());
                i += 1;
                continue;
            }
            x => bail!("opción desconocida: {x}"),
        }
        i += 2;
    }
    let cases_file = cases_file.ok_or_else(|| anyhow!("uso: eval CASOS [--base db] [--a-bin|--b-bin omoi] [--a-env|--b-env K=V] [--a-extra|--b-extra tsv] [--rondas n] [--json f]"))?;
    let (mems, cases) = read_tsv(&cases_file)?;

    // Everything in its own temporary folder, which is never the user's Omoikane folder.
    let tmp = std::env::temp_dir().join(format!("omoi-eval-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tmp = tmp.canonicalize()?;
    if let Some(home) = std::env::home_dir().map(|h| h.join(".omoikane"))
        && let Ok(home) = home.canonicalize()
        && tmp.starts_with(&home)
    {
        bail!("la carpeta temporal está dentro de {}", home.display());
    }
    // It is deleted at the end, also if something fails: it may hold the copy of a real database.
    let _cleanup = Cleanup(tmp.clone());
    let neutral = tmp.join("cwd");
    std::fs::create_dir_all(&neutral)?;

    let base_db = tmp.join("base.db");
    if let Some(b) = &base {
        copy_db(b, &base_db)?;
    }
    let folders = tmp.join("proyectos");
    let mut keys = save(&base_db, &mems, &folders)?;

    let release = std::env::current_exe()?.parent().and_then(Path::parent).map(|d| d.join(format!("omoi{}", std::env::consts::EXE_SUFFIX)));
    let mut sides = Vec::new();
    for (name, cfg) in [("A", a), ("B", b)] {
        let home = tmp.join(name);
        std::fs::create_dir_all(&home)?;
        let db = home.join("omoikane.db");
        std::fs::copy(&base_db, &db)?;
        if let Some(extra) = &cfg.extra {
            let (ms, qs) = read_tsv(extra)?;
            if !qs.is_empty() {
                bail!("{}: un fichero extra solo lleva memorias (`m`)", extra.display());
            }
            keys.extend(save(&db, &ms, &folders)?);
        }
        let bin = cfg.bin.or(release.clone()).ok_or_else(|| anyhow!("¿qué omoi? usa --{}-bin", name.to_lowercase()))?;
        if !bin.exists() {
            bail!("{} no existe (cargo build --release)", bin.display());
        }
        sides.push(Side { name, bin, env: cfg.env, home, db, output: Vec::new(), times: Vec::new(), titles: BTreeMap::new() });
    }

    // What is expected for each case, as full ids (from the `m` keys, from an id or from a title).
    let all: Vec<(String, String)> = {
        let c = Connection::open_with_flags(&sides[1].db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut q = c.prepare("SELECT id, title FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for l in &mut sides {
        let c = Connection::open_with_flags(&l.db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut q = c.prepare("SELECT id, title FROM memory")?;
        l.titles = q.query_map([], |f| Ok((f.get::<_, String>(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    }
    let mut expected: Vec<BTreeSet<String>> = Vec::new();
    for (n, c) in cases.iter().enumerate() {
        let mut s = BTreeSet::new();
        for e in &c.expected {
            let id = keys.get(e).cloned().or_else(|| {
                let by_id: Vec<&String> = all.iter().map(|x| &x.0).filter(|id| e.len() >= 8 && id.starts_with(e.as_str())).collect();
                let by_title: Vec<&String> = all.iter().filter(|x| x.1.eq_ignore_ascii_case(e)).map(|x| &x.0).collect();
                if by_id.len() == 1 { Some(by_id[0].clone()) } else if by_title.len() == 1 { Some(by_title[0].clone()) } else { None }
            });
            s.insert(id.ok_or_else(|| anyhow!("caso {}: «{e}» no es una clave, un id ni un título único", n + 1))?);
        }
        expected.push(s);
    }

    // The rounds, interleaved.
    for round in 0..rounds {
        for (n, c) in cases.iter().enumerate() {
            // A folder that no longer exists (temp dirs of old sessions): the closest one still there,
            // which gives the same project (or none).
            let cwd = match &c.cwd {
                Some(d) if d == "~" => std::env::home_dir().unwrap_or(neutral.clone()).to_string_lossy().into_owned(),
                Some(d) => Path::new(d).ancestors().find(|p| p.exists()).unwrap_or(&neutral).to_string_lossy().into_owned(),
                None => neutral.to_string_lossy().into_owned(),
            };
            let order: [usize; 2] = if (round + n) % 2 == 0 { [0, 1] } else { [1, 0] };
            for k in order {
                let (ids, ms) = sides[k].recall(n, round, &c.message, &cwd)?;
                sides[k].times.push(ms);
                if round == 0 {
                    sides[k].output.push(ids);
                }
            }
        }
    }

    // Scoring.
    let inside = |got: &str, e: &BTreeSet<String>| e.iter().any(|id| id.starts_with(got));
    let right = |n: usize, got: &[String]| -> bool {
        if expected[n].is_empty() { got.is_empty() } else { got.iter().any(|g| inside(g, &expected[n])) }
    };
    let positives = expected.iter().filter(|e| !e.is_empty()).count();
    let negatives = cases.len() - positives;
    println!("# Recall: {} casos ({positives} con memoria esperada, {negatives} sin ninguna) · {}", cases.len(), cases_file.display());
    if let Some(b) = &base {
        println!("base: copia de {} ({} memorias)", b.display(), all.len());
    }
    for l in &sides {
        println!("{}: {} {:?}", l.name, l.bin.display(), l.env);
    }
    println!();
    println!("| | A | B |\n|---|---|---|");
    let mut rows: Vec<(String, [String; 2])> = Vec::new();
    let mut row = |name: &str, f: &dyn Fn(&Side) -> String| rows.push((name.to_string(), [f(&sides[0]), f(&sides[1])]));
    row("casos bien", &|l| {
        let n = (0..cases.len()).filter(|&n| right(n, &l.output[n])).count();
        format!("{n} de {} ({:.0} %)", cases.len(), 100.0 * n as f64 / cases.len().max(1) as f64)
    });
    row("con memoria esperada: trae alguna", &|l| {
        let n = (0..cases.len()).filter(|&n| !expected[n].is_empty() && right(n, &l.output[n])).count();
        format!("{n} de {positives}")
    });
    row("sin memoria esperada: nada (correcto)", &|l| {
        let n = (0..cases.len()).filter(|&n| expected[n].is_empty() && l.output[n].is_empty()).count();
        format!("{n} de {negatives}")
    });
    row("precisión (inyectadas que tocaban)", &|l| {
        let total: usize = l.output.iter().map(Vec::len).sum();
        let ok: usize = (0..cases.len()).map(|n| l.output[n].iter().filter(|g| inside(g, &expected[n])).count()).sum();
        format!("{ok} de {total} ({:.0} %)", 100.0 * ok as f64 / total.max(1) as f64)
    });
    row("cobertura (esperadas que llegan)", &|l| {
        let total: usize = expected.iter().map(BTreeSet::len).sum();
        let ok: usize = (0..cases.len()).map(|n| expected[n].iter().filter(|e| l.output[n].iter().any(|g| e.starts_with(g.as_str()))).count()).sum();
        format!("{ok} de {total} ({:.0} %)", 100.0 * ok as f64 / total.max(1) as f64)
    });
    row("memorias inyectadas por caso", &|l| format!("{:.2}", l.output.iter().map(Vec::len).sum::<usize>() as f64 / cases.len().max(1) as f64));
    row("tiempo P50 / P95 (ms)", &|l| format!("{:.1} / {:.1}", percentile(&l.times, 0.5), percentile(&l.times, 0.95)));
    for (name, [x, y]) in &rows {
        println!("| {name} | {x} | {y} |");
    }

    let title = |l: &Side, g: &str| l.titles.iter().find(|(id, _)| id.starts_with(g)).map(|(_, t)| short(t, 60)).unwrap_or_else(|| g.to_string());
    let list = |l: &Side, n: usize| -> String {
        if l.output[n].is_empty() {
            "nada".into()
        } else {
            l.output[n].iter().map(|g| format!("{}{}", if inside(g, &expected[n]) { "✓ " } else { "✗ " }, title(l, g))).collect::<Vec<_>>().join("; ")
        }
    };
    let expected_text = |n: usize| -> String {
        if expected[n].is_empty() { "nada".into() } else { expected[n].iter().map(|e| title(&sides[1], &e[..12.min(e.len())])).collect::<Vec<_>>().join("; ") }
    };
    let (mut fixed, mut broken, mut others) = (Vec::new(), Vec::new(), Vec::new());
    for n in 0..cases.len() {
        let (x, y) = (right(n, &sides[0].output[n]), right(n, &sides[1].output[n]));
        if !x && y {
            fixed.push(n);
        } else if x && !y {
            broken.push(n);
        } else if sides[0].output[n] != sides[1].output[n] {
            others.push(n);
        }
    }
    for (header, v) in [("Arreglados (mal en A, bien en B)", &fixed), ("Rotos (bien en A, mal en B)", &broken), ("Otros cambios (igual de bien o de mal)", &others)] {
        println!("\n## {header}: {}", v.len());
        for &n in v.iter() {
            println!("- #{} «{}»\n  esperado: {}\n  A: {}\n  B: {}", n + 1, short(&cases[n].message, 110), expected_text(n), list(&sides[0], n), list(&sides[1], n));
        }
    }

    if let Some(f) = json {
        let v: Vec<serde_json::Value> = (0..cases.len())
            .map(|n| {
                serde_json::json!({
                    "caso": n + 1, "mensaje": cases[n].message, "esperadas": expected[n],
                    "a": sides[0].output[n], "b": sides[1].output[n],
                    "a_titulos": sides[0].output[n].iter().map(|g| title(&sides[0], g)).collect::<Vec<_>>(),
                    "b_titulos": sides[1].output[n].iter().map(|g| title(&sides[1], g)).collect::<Vec<_>>(),
                })
            })
            .collect();
        std::fs::write(&f, serde_json::to_string_pretty(&v)?)?;
    }
    Ok(())
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
