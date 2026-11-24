//! Performance guard: recall and search of this `omoi` against those of the latest Release, on
//! the same machine and with the same database. Fails (exits with 1) if any is clearly slower.
//!
//!   cargo build --release && cargo run --release --example performance -- [options]
//!
//! Options:
//!   --contra <version>   the Release to compare against (default: the latest published); it is downloaded
//!                        from GitHub and checked as on an update: signature of `SHA256SUMS` with the
//!                        Omoikane key and SHA-256 of the package
//!   --a-bin <omoi>       instead of downloading it, this binary as the old one
//!   --b-bin <omoi>       the new one (default: the one in `target/release`)
//!   --memorias <n>       size of the synthetic database (default 10,000)
//!   --rondas <n>         measurements per test and binary (default 30)
//!   --margen <x>         how much slower is tolerated, as a proportion of the median (default 0.25)
//!   --minimo-ms <ms>     and in milliseconds: below this it doesn't count as slower (default 5)
//!
//! How it avoids the false positives of a noisy machine (GitHub runners are):
//! - Both binaries are measured on the same machine, at the same moment and on copies of the same
//!   database, interleaved (A,B in one round, B,A in the next): whatever slowness the machine has, it
//!   has for both. Never against a stored number from another machine.
//! - The median counts, not the mean nor the P95: a process that collides with another doesn't move it.
//! - Slower means exceeding both the relative and the absolute margin at once.
//! - A test that comes out slower is measured again in full; it only fails if it is slower both times.
//!
//! Each call is a new process, as the hook and the agent launch it: startup counts. Each
//! binary has its own copy of the database and its own Omoikane folder, with no models, no
//! sync and no updates; the `OMOIKANE_*` variables of whoever launches it don't get through.
//! Everything goes to a temp folder that is deleted at the end.
//!
//! The database is synthetic and deterministic: the memories of `tests/searches.tsv` plus filler with the
//! words of `tests/searches.tsv` and `tests/recall_cases.tsv` and made-up words, spread over 8
//! projects and the global ones. The queries are those of `tests/searches.tsv`.

use anyhow::{Context, Result, anyhow, bail};
use omoikane::memory::{self, NewMemory, Kind};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const SEARCHES: &str = include_str!("../tests/searches.tsv");
const RECALL: &str = include_str!("../tests/recall_cases.tsv");
const PROJECTS: usize = 8;

/// SplitMix64: the same database on every machine, depending on nothing.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Between 0 and 1.
    fn real(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// The words of the case files (the ones the queries look for) and, after them, made-up words.
/// They are picked with a bias towards the start, as in real text: a few show up in many memories
/// and most in few.
fn vocabulary() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in SEARCHES.lines().chain(RECALL.lines()).filter(|l| !l.starts_with('#')) {
        for p in l.split(|c: char| !c.is_alphanumeric()).filter(|p| p.chars().count() > 1) {
            let p = p.to_lowercase();
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }
    let syllables = ["ka", "to", "ri", "mu", "sen", "lo", "ve", "da", "pi", "nor", "gu", "fe", "ras", "zo", "te", "bil"];
    for a in syllables {
        for b in syllables {
            v.push(format!("{a}{b}"));
            for c in syllables {
                v.push(format!("{a}{b}{c}"));
            }
        }
    }
    v
}

fn sentence(rng: &mut Rng, voc: &[String], words: usize) -> String {
    (0..words).map(|_| voc[((rng.real().powf(2.5)) * voc.len() as f64) as usize].as_str()).collect::<Vec<_>>().join(" ")
}

/// Creates the database in `db` with `n` memories; returns the folder of the first project (the one for the queries).
fn create_db(db: &Path, folders: &Path, n: usize) -> Result<PathBuf> {
    let mut c = omoikane::db::open(db)?;
    // A throwaway database: no waiting for the disk on every memory (on a hard disk, minutes).
    c.execute_batch("PRAGMA synchronous=OFF;")?;
    let mut keys = Vec::new();
    for p in 0..PROJECTS {
        let d = folders.join(format!("p{p}"));
        std::fs::create_dir_all(&d)?;
        keys.push(omoikane::project::of_dir(&d)?);
    }
    let kinds = [Kind::Fact, Kind::Decision, Kind::Gotcha, Kind::Fact, Kind::Preference, Kind::Fact];
    let save = |c: &mut rusqlite::Connection, project: String, kind: Kind, title: String, content: String| -> Result<()> {
        memory::save(c, NewMemory { project, kind: Some(kind), title, content, source: "rendimiento".into(), ..Default::default() }, None)?;
        Ok(())
    };
    // The real ones, so the queries find theirs among the filler.
    for l in SEARCHES.lines().filter(|l| l.starts_with("m\t")) {
        let c4: Vec<&str> = l.split('\t').collect();
        if let [_, _, title, content, ..] = c4.as_slice() {
            save(&mut c, keys[0].clone(), Kind::Fact, title.to_string(), content.to_string())?;
        }
    }
    let voc = vocabulary();
    let mut rng = Rng(20261109);
    for i in 0..n {
        let project = if i % 10 == 0 { String::new() } else { keys[i % PROJECTS].clone() };
        let (title_len, content_len) = (4 + rng.below(6), 25 + rng.below(65));
        let title = sentence(&mut rng, &voc, title_len);
        let content = sentence(&mut rng, &voc, content_len);
        save(&mut c, project, kinds[i % kinds.len()], title, content)?;
    }
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(folders.join("p0"))
}

struct Side {
    name: String,
    bin: PathBuf,
    home: PathBuf,
    db: PathBuf,
    no_models: PathBuf,
}

impl Side {
    /// Runs `omoi <args>` with `input` on stdin; returns (ms, stdout).
    fn run(&self, args: &[&str], cwd: &Path, input: Option<&str>) -> Result<(f64, String)> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, _) in std::env::vars() {
            if k.starts_with("OMOIKANE_") || k == "USER_PROMPT" {
                cmd.env_remove(k);
            }
        }
        cmd.env("OMOIKANE_HOME", &self.home)
            .env("OMOIKANE_DB", &self.db)
            .env("OMOIKANE_MODELS", &self.no_models)
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_LANG", "en");
        let t = Instant::now();
        let mut child = cmd.spawn().with_context(|| self.bin.display().to_string())?;
        let mut stdin = child.stdin.take().context("stdin")?;
        if let Some(e) = input {
            stdin.write_all(e.as_bytes())?;
        }
        drop(stdin);
        let out = child.wait_with_output()?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if !out.status.success() {
            bail!("{} {}: {}", self.name, args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok((ms, String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Test {
    Recall,
    SearchProject,
    SearchAll,
}

impl Test {
    fn name(self) -> &'static str {
        match self {
            Test::Recall => "recall",
            Test::SearchProject => "search (project)",
            Test::SearchAll => "search --all",
        }
    }

    /// One call; returns (ms, ids it returned, in order).
    fn measure(self, l: &Side, cwd: &Path, query: &str, session: &str) -> Result<(f64, Vec<String>)> {
        Ok(match self {
            Test::Recall => {
                let e = serde_json::json!({ "session_id": session, "cwd": cwd, "prompt": query, "hook_event_name": "UserPromptSubmit" });
                let (ms, out) = l.run(&["recall"], cwd, Some(&e.to_string()))?;
                (ms, recall_ids(&out))
            }
            Test::SearchProject | Test::SearchAll => {
                let mut args = vec!["--json", "search"];
                if self == Test::SearchAll {
                    args.push("--all");
                }
                args.push(query);
                let (ms, out) = l.run(&args, cwd, None)?;
                let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_default();
                (ms, v.as_array().map(|a| a.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect()).unwrap_or_default())
            }
        })
    }
}

/// What the recall injects: the start of each `(id …)` in the block.
fn recall_ids(output: &str) -> Vec<String> {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(output.trim()) else { return Vec::new() };
    let text = j["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default();
    text.match_indices("(id ").map(|(i, _)| text[i + 4..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect()).collect()
}

fn percentile(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(((v.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(0.0)
}

struct Measurement {
    a: Vec<f64>,
    b: Vec<f64>,
    /// Queries in which both returned the same, out of how many.
    same: usize,
    queries: usize,
}

fn measure(test: Test, sides: &[Side; 2], cwd: &Path, queries: &[String], rounds: usize, pass: usize) -> Result<Measurement> {
    // Warm up: the first call pays for the cold disk cache and doesn't count.
    for l in sides {
        for _ in 0..3 {
            test.measure(l, cwd, &queries[0], &format!("calentar-{pass}"))?;
        }
    }
    let mut m = Measurement { a: Vec::new(), b: Vec::new(), same: 0, queries: 0 };
    for r in 0..rounds {
        let q = &queries[r % queries.len()];
        // The same session for both (each with its own database): the recall doesn't repeat what was already injected.
        let session = format!("rendimiento-{pass}-{r}");
        let order = if r % 2 == 0 { [0, 1] } else { [1, 0] };
        let mut ids: [Vec<String>; 2] = Default::default();
        for k in order {
            let (ms, got) = test.measure(&sides[k], cwd, q, &session)?;
            if k == 0 { m.a.push(ms) } else { m.b.push(ms) }
            ids[k] = got;
        }
        if r < queries.len() {
            m.queries += 1;
            m.same += usize::from(ids[0] == ids[1]);
        }
    }
    Ok(m)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut against, mut a_bin, mut b_bin): (Option<String>, Option<PathBuf>, Option<PathBuf>) = (None, None, None);
    let (mut n, mut rounds, mut margin, mut min_ms) = (10_000usize, 30usize, 0.25f64, 5.0f64);
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned().ok_or_else(|| anyhow!("falta el valor de {}", args[i]))?;
        match args[i].as_str() {
            "--contra" => against = Some(value.trim_start_matches('v').to_string()),
            "--a-bin" => a_bin = Some(value.into()),
            "--b-bin" => b_bin = Some(value.into()),
            "--memorias" => n = value.parse()?,
            "--rondas" => rounds = value.parse()?,
            "--margen" => margin = value.replace(',', ".").parse()?,
            "--minimo-ms" => min_ms = value.replace(',', ".").parse()?,
            x => bail!("opción desconocida: {x} (mira el principio de examples/performance.rs)"),
        }
        i += 2;
    }
    anyhow::ensure!(rounds >= 5, "al menos 5 rondas");

    let tmp = std::env::temp_dir().join(format!("omoi-rendimiento-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tmp = tmp.canonicalize()?;
    if let Some(home) = std::env::home_dir().map(|h| h.join(".omoikane"))
        && let Ok(home) = home.canonicalize()
        && tmp.starts_with(&home)
    {
        bail!("la carpeta temporal está dentro de {}", home.display());
    }
    let cleanup = Cleanup(tmp.clone());

    let release = std::env::current_exe()?.parent().and_then(Path::parent).map(|d| d.join(format!("omoi{}", std::env::consts::EXE_SUFFIX)));
    let b_bin = b_bin.or(release).filter(|b| b.exists()).ok_or_else(|| anyhow!("no está el omoi nuevo: cargo build --release (o --b-bin)"))?;
    let (a_bin, a_origin) = match a_bin {
        Some(b) => {
            let origin = b.display().to_string();
            (b, origin)
        }
        None => {
            let v = match against {
                Some(v) => v,
                None => omoikane::update::latest().context("¿cuál es la última Release? (o --contra <versión>)")?,
            };
            eprintln!("bajando omoi {v} y comprobando su firma y su SHA-256…");
            (omoikane::update::download_version(&v, &tmp.join("antes"))?, format!("Release v{v}, signature and SHA-256 checked"))
        }
    };
    let version = |b: &Path| -> String {
        Command::new(b).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    };

    eprintln!("creando una base de {n} memorias…");
    let t = Instant::now();
    let base = tmp.join("base.db");
    let cwd = create_db(&base, &tmp.join("proyectos"), n)?;
    eprintln!("  en {:.1} s", t.elapsed().as_secs_f64());
    let no_models = tmp.join("sin-modelos");
    std::fs::create_dir_all(&no_models)?;
    let side = |name: &str, bin: PathBuf| -> Result<Side> {
        let home = tmp.join(name);
        std::fs::create_dir_all(&home)?;
        let db = home.join("omoikane.db");
        std::fs::copy(&base, &db)?;
        Ok(Side { name: name.into(), bin, home, db, no_models: no_models.clone() })
    };
    let sides = [side("A", a_bin)?, side("B", b_bin)?];
    let queries: Vec<String> = SEARCHES.lines().filter(|l| l.starts_with("q\t")).filter_map(|l| l.split('\t').nth(2).map(str::to_string)).collect();

    let mut report = format!(
        "## Recall and search against the last release\n\nA: {} ({a_origin})  \nB: {} ({})  \n{n} memories, {rounds} interleaved rounds per test, one process per call; slower means a median over {:.0} % and {min_ms} ms above A, twice in a row.\n\n| | A P50 / P95 (ms) | B P50 / P95 (ms) | B / A | same results | |\n|---|---|---|---|---|---|\n",
        version(&sides[0].bin),
        version(&sides[1].bin),
        sides[1].bin.display(),
        margin * 100.0
    );
    let mut failed = Vec::new();
    for test in [Test::Recall, Test::SearchProject, Test::SearchAll] {
        eprintln!("midiendo {}…", test.name());
        let is_slow = |m: &Measurement| {
            let (a, b) = (percentile(&m.a, 0.5), percentile(&m.b, 0.5));
            b > a * (1.0 + margin) && b - a > min_ms
        };
        let mut m = measure(test, &sides, &cwd, &queries, rounds, 0)?;
        let mut verdict = "ok";
        if is_slow(&m) {
            eprintln!("  más lenta: se mide otra vez");
            let again = measure(test, &sides, &cwd, &queries, rounds, 1)?;
            verdict = if is_slow(&again) { "**slower**" } else { "ok (slower once, not twice)" };
            if is_slow(&again) {
                failed.push(test.name());
            }
            m = again;
        }
        let (a50, b50) = (percentile(&m.a, 0.5), percentile(&m.b, 0.5));
        report.push_str(&format!(
            "| {} | {a50:.1} / {:.1} | {b50:.1} / {:.1} | {:.2} | {} of {} | {verdict} |\n",
            test.name(),
            percentile(&m.a, 0.95),
            percentile(&m.b, 0.95),
            b50 / a50.max(1e-9),
            m.same,
            m.queries
        ));
    }
    println!("{report}");
    // On GitHub Actions, also in the run summary.
    if let Some(f) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let _ = std::fs::OpenOptions::new().append(true).create(true).open(f).and_then(|mut f| f.write_all(report.as_bytes()));
    }
    if !failed.is_empty() {
        eprintln!("más lento que la versión anterior: {}", failed.join(", "));
        // `exit` doesn't run the `Drop`s: the temp folder is deleted first.
        drop(cleanup);
        std::process::exit(1);
    }
    Ok(())
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
