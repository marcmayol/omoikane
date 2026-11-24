//! 1.8: what Kumi asks for (an agent hub that uses Omoikane as its brain): the decision queue with
//! priority (`omoi decisions`, the `decisions` tool) and per-file history for its risk gate
//! (`omoi file-history`).

mod common;

use omoikane::memory::{self, Scope, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::{db, diary, project, risk};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    project::of_dir(dir).unwrap()
}

fn decision(db: &mut Connection, project: &str, space_id: Option<i64>, title: &str, tags: &[&str]) -> String {
    let n = NewMemory {
        project: project.into(),
        space_id,
        kind: Some(Kind::Decision),
        title: title.into(),
        content: format!("{title}. Opciones distintas para «{title}»; asumido lo primero mientras tanto."),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let id = memory::save(db, n, None).unwrap().id;
    // Different dates: the date ordering shows.
    std::thread::sleep(std::time::Duration::from_millis(5));
    id
}

/// `omoi` with its own database and without touching the real one; from `cwd`.
fn omoi(data_dir: &Path, cwd: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(args)
        .current_dir(cwd)
        .env("OMOIKANE_DB", data_dir.join("o.db"))
        .env("OMOIKANE_MODELS", data_dir.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_HOME", data_dir.join("home-omoikane"))
        .env_remove("OMOIKANE_LANG")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(e) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn json(o: Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn titles(v: &serde_json::Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|d| d["title"].as_str().unwrap().to_string()).collect()
}

/// Two clients (Acme and Globex) with open decisions, blocking and for later.
struct Factory {
    dir: common::TempDir,
    acme: String,
    globex: String,
}

fn factory(name: &str) -> Factory {
    let dir = common::TempDir::new(&format!("kumi-{name}"));
    let acme = repo(&dir.join("acme-web"));
    let globex = repo(&dir.join("globex-web"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let acme_space = project::create_space(&db, "Acme").unwrap();
    let globex_space = project::create_space(&db, "Globex").unwrap();
    for (p, s) in [(&acme, acme_space), (&globex, globex_space)] {
        let id = project::register(&db, p).unwrap();
        project::assign_space(&db, id, Some(s)).unwrap();
    }
    decision(&mut db, &acme, None, "Acme: precio del plan Pro", &["open"]);
    decision(&mut db, &acme, None, "Acme: borrar cuentas inactivas", &["open", "blocking"]);
    decision(&mut db, &acme, None, "Acme: color del botón", &["open"]);
    decision(&mut db, "", Some(acme_space), "Acme espacio: proveedor de correo", &["open", "Blocking"]);
    decision(&mut db, &acme, None, "Acme: ya decidida", &["blocking"]);
    decision(&mut db, &globex, None, "Globex: migrar la base", &["open", "blocking"]);
    decision(&mut db, "", None, "Global: licencia de las plantillas", &["open"]);
    Factory { dir, acme, globex }
}

#[test]
fn blocking_first_and_without_leaving_the_space() {
    let f = factory("orden");
    let db = db::open(&f.dir.join("o.db")).unwrap();
    let scope = Scope::from_project(&db, &f.acme).unwrap();
    let v = memory::decisions(&db, &scope, false, &[], 50).unwrap();
    let t: Vec<(&str, &str)> = v.iter().map(|d| (d.title.as_str(), d.priority)).collect();
    // Blocking first (newest to oldest), then the later ones; no Globex and not the one that's no
    // longer open.
    assert_eq!(t, [
        ("Acme espacio: proveedor de correo", "blocking"),
        ("Acme: borrar cuentas inactivas", "blocking"),
        ("Global: licencia de las plantillas", "later"),
        ("Acme: color del botón", "later"),
        ("Acme: precio del plan Pro", "later"),
    ]);
    assert!(v[0].project.is_empty() && v[0].space == "acme" && !v[0].global);
    assert!(v[2].global);
    let blocking_only = memory::decisions(&db, &scope, true, &[], 50).unwrap();
    assert_eq!(blocking_only.len(), 2);
    assert_eq!(memory::decisions(&db, &scope, false, &[], 1).unwrap().len(), 1);
    // From the general folder, everyone's.
    assert_eq!(memory::decisions(&db, &Scope::All, true, &[], 50).unwrap().len(), 3);
}

#[test]
fn on_start_blocking_ones_are_marked() {
    let f = factory("empezar");
    let mut db = db::open(&f.dir.join("o.db")).unwrap();
    let e = HookInput { session_id: "s1".into(), ..Default::default() };
    let t = recall::on_session_start(&mut db, &f.acme, &e).unwrap().unwrap();
    let (_, rest) = t.split_once("Decisions waiting for the user").expect("la sección");
    assert!(rest.lines().next().unwrap().contains("the 2 marked BLOCKING first"), "{t}");
    let l: Vec<&str> = rest.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(l.len(), 5, "{t}");
    assert!(l[0].starts_with("- BLOCKING ") && l[0].contains("proveedor de correo"), "{t}");
    assert!(l[1].starts_with("- BLOCKING ") && l[1].contains("borrar cuentas"), "{t}");
    assert!(l[2..].iter().all(|x| !x.contains("BLOCKING")), "{t}");
    assert!(!t.contains("Globex"), "{t}");
    // Without blocking ones, the usual header.
    let mut db2 = db::open(&f.dir.join("o.db")).unwrap();
    let t = recall::on_session_start(&mut db2, &f.globex, &e).unwrap().unwrap();
    assert!(t.contains("BLOCKING") && t.contains("Globex: migrar la base"), "{t}");
    // The instructions say when it's blocking.
    assert!(t.contains("Add the tag `blocking` too only when the work can't go on without the answer"), "{t}");
}

#[test]
fn cli_and_tool_give_the_same_json() {
    let f = factory("cli");
    let v = json(omoi(&f.dir, Path::new(&f.acme), &["decisions", "--json"], None));
    assert_eq!(v.as_array().unwrap().len(), 5, "{v}");
    let d = &v[1];
    assert_eq!(d["priority"], "blocking");
    assert_eq!(d["title"], "Acme: borrar cuentas inactivas");
    assert!(d["content"].as_str().unwrap().contains("asumido"));
    assert!(d["id"].as_str().unwrap().len() > 12 && d["created_at"].is_string() && d["updated_at"].is_string());
    assert!(d["project"].as_str().unwrap().ends_with("acme-web"), "{d}");
    let b = json(omoi(&f.dir, Path::new(&f.acme), &["decisions", "--blocking", "--json"], None));
    assert_eq!(titles(&b), ["Acme espacio: proveedor de correo", "Acme: borrar cuentas inactivas"]);
    // Another project of the same client, yes; of another client, no.
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions", "--space", "globex", "--json"], None);
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("migrar"));
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions", "--project", "globex-web", "--json"], None);
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("migrar"));
    // From the general folder (Kumi's hub, in the factory folder) and asking for a project.
    let hub = f.dir.join("central");
    std::fs::create_dir_all(&hub).unwrap();
    let g = json(omoi(&f.dir, &hub, &["--dir", &f.globex, "decisions", "--blocking", "--json"], None));
    assert_eq!(titles(&g), ["Globex: migrar la base"]);
    // Text: marked.
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions"], None);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.lines().next().unwrap().contains("BLOCKING"), "{t}");

    // The MCP tool: the same, and it doesn't leave the space either.
    let call = |args: serde_json::Value| omoikane::mcp::run_tool(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("decisions"), &args.to_string(), "test").unwrap();
    let (t, error) = call(serde_json::json!({ "blocking": true }));
    assert!(!error, "{t}");
    let m: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(titles(&m), titles(&b));
    let (t, error) = call(serde_json::json!({ "space": "globex" }));
    assert!(error && !t.contains("migrar"), "{t}");
    let (t, _) = call(serde_json::json!({ "all_projects": true }));
    assert!(!t.contains("Globex"), "{t}");
}

// --- file-history ---------------------------------------------------------------------------------

fn session(db: &Connection, name: &str, project_id: i64, folder: &str, day: &str, files: &[&str]) {
    diary::create_table(db).unwrap();
    db.execute(
        "INSERT INTO diary(session, project_id, folder, first_at, last_at, files) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        rusqlite::params![name, project_id, folder, format!("{day}T10:00:00.000Z"), serde_json::to_string(files).unwrap()],
    )
    .unwrap();
}

fn gotcha(db: &mut Connection, project: &str, title: &str, paths: &[&str]) -> String {
    let n = NewMemory {
        project: project.into(),
        kind: Some(Kind::Gotcha),
        title: title.into(),
        content: format!("{title}: cuidado al tocarlo."),
        paths: paths.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    memory::save(db, n, None).unwrap().id
}

/// A repository with 5 sessions on `src/sync.rs` (4 with `tests/sync.rs`, 2 with `CHANGELOG.md`), and
/// another project that also touches `src/sync.rs`; and a session from the user's folder with
/// absolute paths into the repository.
fn with_diary(name: &str) -> (common::TempDir, String, Vec<String>) {
    let d = common::TempDir::new(&format!("kumi-{name}"));
    let r = repo(&d.join("repo"));
    let other = repo(&d.join("otro"));
    let mut db = db::open(&d.join("o.db")).unwrap();
    let id = project::register(&db, &r).unwrap();
    let id2 = project::register(&db, &other).unwrap();
    session(&db, "a", id, &r, "2026-10-01", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    session(&db, "b", id, &r, "2026-10-08", &["src/sync.rs", "tests/sync.rs"]);
    session(&db, "c", id, &r, "2026-10-15", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    session(&db, "d", id, &r, "2026-11-04", &["src/sync.rs", "tests/sync.rs"]);
    session(&db, "e", id, &r, "2026-10-20", &["src/sync.rs"]);
    session(&db, "f", id, &r, "2026-10-21", &["README.md"]);
    for i in 0..3 {
        session(&db, &format!("o{i}"), id2, &other, "2026-11-06", &["src/sync.rs"]);
    }
    // A session opened in the user's folder that touched the repository with absolute paths (and
    // something outside, which doesn't count), and another that didn't touch it.
    let abs = |f: &str| format!("{}/{f}", r.replace('\\', "/"));
    let home = d.join("casa").to_string_lossy().into_owned();
    session(&db, "h1", 0, &home, "2026-11-06", &[&abs("src/sync.rs"), &abs("tests/sync.rs"), "C:/fuera/x.rs"]);
    session(&db, "h2", 0, &home, "2026-11-07", &["C:/fuera/x.rs", &format!("{}-wt/src/sync.rs", r.replace('\\', "/"))]);
    let g1 = gotcha(&mut db, &r, "El push de sync pierde el orden", &["src/sync.rs"]);
    let g2 = gotcha(&mut db, &r, "Sync con la ruta entera", &[&format!("{r}/src/sync.rs")]);
    gotcha(&mut db, &other, "Gotcha del otro proyecto", &["src/sync.rs"]);
    (d, r, vec![g1, g2])
}

#[test]
fn history_of_the_diff_files() {
    let (d, r, gotchas) = with_diary("historia");
    let db = db::open(&d.join("o.db")).unwrap();
    let requested: Vec<String> =
        ["src/sync.rs", "./src/sync.rs", &format!("{r}/src/sync.rs"), r"tests\sync.rs", "nuevo.rs", ""].iter().map(|s| s.to_string()).collect();
    let i = risk::for_files(&db, &r, &requested).unwrap();
    assert_eq!(i.sessions, 7);
    // No repeats: relative, with `./`, absolute and with `\` are the same file.
    let names: Vec<&str> = i.files.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(names, ["src/sync.rs", "tests/sync.rs", "nuevo.rs"]);
    let s = &i.files[0];
    // Only this project's sessions, not the other's.
    assert_eq!((s.sessions, s.last.as_deref()), (6, Some("2026-11-06")));
    assert_eq!(s.changes_with, [risk::CoChange { file: "tests/sync.rs".into(), sessions: 5 }, risk::CoChange { file: "CHANGELOG.md".into(), sessions: 2 }]);
    let mut expected = gotchas.clone();
    expected.sort();
    assert_eq!((s.gotchas, &s.gotcha_ids), (2, &expected));
    let t = &i.files[1];
    assert_eq!((t.sessions, t.changes_with[0].file.as_str(), t.gotchas), (5, "src/sync.rs", 0));
    let n = &i.files[2];
    assert_eq!((n.sessions, n.last.as_deref(), n.changes_with.len(), n.gotchas), (0, None, 0, 0));

    // Without a diary: everything at zero, no error.
    let d2 = common::TempDir::new("kumi-sin-diario");
    let r2 = repo(&d2.join("repo"));
    let db2 = db::open(&d2.join("o.db")).unwrap();
    let i = risk::for_files(&db2, &r2, &["src/a.rs".to_string()]).unwrap();
    assert_eq!((i.sessions, i.files[0].sessions), (0, 0));
}

#[test]
fn file_history_from_args_and_from_stdin() {
    let (d, r, _) = with_diary("cli-historia");
    let root = Path::new(&r);
    let v = json(omoi(&d, root, &["file-history", "src/sync.rs", "README.md", "--json"], None));
    assert_eq!(v["sessions"], 7);
    assert_eq!(v["files"][0]["file"], "src/sync.rs");
    assert_eq!(v["files"][0]["sessions"], 6);
    assert_eq!(v["files"][0]["last"], "2026-11-06");
    assert_eq!(v["files"][0]["changes_with"][0]["file"], "tests/sync.rs");
    assert_eq!(v["files"][0]["changes_with"][0]["sessions"], 5);
    assert_eq!(v["files"][0]["gotchas"], 2);
    assert_eq!(v["files"][0]["gotcha_ids"].as_array().unwrap().len(), 2);
    assert_eq!(v["files"][1]["sessions"], 1);
    // What `git diff --name-only` outputs, through stdin, from another folder with --dir.
    let outside = d.join("fuera");
    std::fs::create_dir_all(&outside).unwrap();
    let v = json(omoi(&d, &outside, &["--dir", &r, "file-history", "--json"], Some("src/sync.rs\ntests/sync.rs\n\n")));
    let names: Vec<&str> = v["files"].as_array().unwrap().iter().map(|f| f["file"].as_str().unwrap()).collect();
    assert_eq!(names, ["src/sync.rs", "tests/sync.rs"]);
    // Absolute.
    let abs = root.join("src").join("sync.rs");
    let v = json(omoi(&d, root, &["file-history", abs.to_str().unwrap(), "--json"], None));
    assert_eq!((v["files"][0]["file"].as_str(), v["files"][0]["sessions"].as_u64()), (Some("src/sync.rs"), Some(6)));
    // Text.
    let o = omoi(&d, root, &["file-history", "src/sync.rs", "nuevo.rs"], None);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("src/sync.rs: changed in 6 of 7 sessions (last 2026-11-06); with tests/sync.rs (5), CHANGELOG.md (2); 2 gotcha(s)"), "{t}");
    assert!(t.contains("nuevo.rs: not in the session diary"), "{t}");
    // An empty diff: no files, no error.
    let v = json(omoi(&d, root, &["file-history", "--json"], Some("")));
    assert_eq!(v["files"].as_array().map(Vec::len), Some(0));
}

// --- What kumi.py asks for: list by tag, data and the state of decisions ---------------------------

/// A project with many memories that look like the profile: `search` could leave it out; `list`
/// by tag always returns it, with its data, and without leaving the space.
#[test]
fn list_by_tag_with_data() {
    let f = factory("list");
    let root = Path::new(&f.acme);
    let profile = serde_json::json!({ "ruta": ["web", "seo-geo"], "prod_tests": true, "fase": null });
    let o = omoi(&f.dir, root, &["remember", "--json", "-k", "decision", "-t", "Kumi: perfil del proyecto", "--tag", "kumi", "--tag", "Kumi-Perfil", "--data", &profile.to_string(), "--force", "Decisiones de Marc para este proyecto."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    for i in 0..30 {
        let t = format!("Perfil del proyecto, nota {i}");
        json(omoi(&f.dir, root, &["remember", "--json", "-k", "fact", "-t", &t, "--tag", "kumi", "--force", &format!("{t}: perfil del proyecto y su fase {i}.")], None));
    }
    let v = json(omoi(&f.dir, root, &["list", "--tag", "kumi-perfil", "--json"], None));
    assert_eq!(v.as_array().unwrap().len(), 1, "{v}");
    assert_eq!((v[0]["id"].as_str(), &v[0]["data"]), (Some(id.as_str()), &profile));
    assert_eq!(json(omoi(&f.dir, root, &["list", "--tag", "kumi", "--json"], None)).as_array().unwrap().len(), 31);
    // Two tags: both; with a kind, only that one.
    assert_eq!(json(omoi(&f.dir, root, &["list", "--tag", "kumi", "--tag", "kumi-perfil", "--json"], None)).as_array().unwrap().len(), 1);
    assert_eq!(json(omoi(&f.dir, root, &["list", "--tag", "kumi", "-k", "fact", "-n", "5", "--json"], None)).as_array().unwrap().len(), 5);
    // Acme's blocking ones by tag, including the space's; never Globex's.
    let b = json(omoi(&f.dir, root, &["list", "--tag", "open", "--tag", "blocking", "--json"], None));
    assert_eq!(titles(&b), ["Acme espacio: proveedor de correo", "Acme: borrar cuentas inactivas"]);
    assert!(!omoi(&f.dir, root, &["list", "--tag", "open", "--space", "globex"], None).status.success());
    // `get` gives the data; the `recent` tool with `tags`, same as `list`.
    let g = json(omoi(&f.dir, root, &["get", &id, "--json"], None));
    assert_eq!(g["data"], profile);
    let (t, error) = omoikane::mcp::run_tool(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("recent"), r#"{"tags":["kumi-perfil"]}"#, "test").unwrap();
    assert!(!error, "{t}");
    let r: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!((r.as_array().unwrap().len(), &r[0]["data"]), (1, &profile), "{r}");
    // Without a tag, no.
    assert!(!omoi(&f.dir, root, &["list", "--json"], None).status.success());
}

/// Data is not part of search or recall, changes without a new version and can be removed.
#[test]
fn data_is_not_searched_and_changes_on_its_own() {
    let f = factory("datos");
    let root = Path::new(&f.acme);
    let d = serde_json::json!({ "palabra": "xilofonorara", "clave": "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" });
    let o = omoi(&f.dir, root, &["remember", "--json", "-k", "fact", "-t", "Perfil con datos", "--tag", "perfil", "--data", &d.to_string(), "Algo normal."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    let s = json(omoi(&f.dir, root, &["search", "xilofonorara", "--json"], None));
    assert_eq!(s.as_array().unwrap().len(), 0, "{s}");
    let mut db = db::open(&f.dir.join("o.db")).unwrap();
    let e = HookInput { session_id: "s".into(), prompt: "qué pasa con xilofonorara".into(), ..Default::default() };
    let r = recall::on_prompt(&mut db, &f.acme, &e, None).unwrap().unwrap_or_default();
    assert!(!r.contains("xilofonorara"), "{r}");
    // What looks like a secret is redacted in the data too.
    let data = memory::data_of(&db, &[id.as_str()]).unwrap();
    assert!(data[&id]["clave"].as_str().unwrap().starts_with("[redacted:"), "{data:?}");
    drop(db);
    // Only the data: no version in the history, and the stamp changes (sync carries it).
    let hlc = |db: &Connection| db.query_row("SELECT hlc FROM memory WHERE id = ?1", [&id], |x| x.get::<_, String>(0)).unwrap();
    let before = hlc(&db::open(&f.dir.join("o.db")).unwrap());
    let v = json(omoi(&f.dir, root, &["update", &id, "--data", r#"{"fase":"beta"}"#, "--json"], None));
    assert_eq!((v["data"]["fase"].as_str(), v["content"].as_str()), (Some("beta"), Some("Algo normal.")));
    assert!(json(omoi(&f.dir, root, &["history", &id, "--json"], None)).as_array().unwrap().is_empty());
    assert_ne!(hlc(&db::open(&f.dir.join("o.db")).unwrap()), before);
    // Through MCP: `data: null` removes it; without `data`, it's left alone.
    let call = |args: serde_json::Value| omoikane::mcp::run_tool(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("update"), &args.to_string(), "test").unwrap();
    call(serde_json::json!({ "id": id, "title": "Perfil con datos, retitulado" }));
    assert_eq!(json(omoi(&f.dir, root, &["get", &id, "--json"], None))["data"]["fase"], "beta");
    call(serde_json::json!({ "id": id, "data": null }));
    assert!(json(omoi(&f.dir, root, &["get", &id, "--json"], None)).get("data").is_none());
    // JSON that isn't: error, and nothing saved.
    assert!(!omoi(&f.dir, root, &["remember", "-k", "fact", "-t", "Mal", "--data", "{no", "x"], None).status.success());
    // It travels with `omoi export` / `omoi import` as a `data` field of the line.
    call(serde_json::json!({ "id": id, "data": { "fase": "alpha" } }));
    let o = omoi(&f.dir, root, &["export", "--all"], None);
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let line: serde_json::Value = text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|l| l["id"] == id.as_str()).expect("la línea");
    assert_eq!(line["data"]["fase"], "alpha", "{line}");
    let other_dir = common::TempDir::new("kumi-datos-import");
    let copy = other_dir.join("copia.jsonl");
    std::fs::write(&copy, &text).unwrap();
    json(omoi(&other_dir, root, &["import", copy.to_str().unwrap(), "--json"], None));
    let db2 = db::open(&other_dir.join("o.db")).unwrap();
    assert_eq!(memory::data_of(&db2, &[id.as_str()]).unwrap()[&id]["fase"], "alpha");
    // The import launches `omoi review --refresh-trade-duplicates` separately on this database. If the
    // test deletes the folder before that process opens it, it recreates it and it stays in temp: wait
    // until it saves its computation (then, if it still has it open, `TempDir` retries).
    let computed = || db2.query_row("SELECT 1 FROM meta WHERE k = 'revision_duplicadas_oficio'", [], |_| Ok(())).is_ok();
    let start = std::time::Instant::now();
    while !computed() && start.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The lifecycle of a Kumi decision with clean text: open-blocking for a task, the gate sees it, it
/// is resolved with the answer and stops showing up.
#[test]
fn blocking_task_decision_then_resolved() {
    let f = factory("resolver");
    let root = Path::new(&f.acme);
    let o = omoi(&f.dir, root, &["remember", "--json", "-k", "decision", "-t", "Kumi: tarea parada en la factoría web", "--tag", "kumi", "--tag", "tarea:t42", "--tag", "open", "--tag", "blocking", "--force", "La factoría no sabe qué dominio usar."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    json(omoi(&f.dir, root, &["remember", "--json", "-k", "decision", "-t", "Kumi: otra tarea", "--tag", "tarea:t7", "--tag", "open", "--tag", "blocking", "--force", "Otra."], None));
    let b = json(omoi(&f.dir, root, &["decisions", "--blocking", "--tag", "tarea:t42", "--json"], None));
    assert_eq!(titles(&b), ["Kumi: tarea parada en la factoría web"]);
    // Resolve it: answer at the end, without `open` or `blocking`, with `resolved`.
    let r = json(omoi(&f.dir, root, &["decisions", "--resolve", &id, "--answer", "acme.com", "--json"], None));
    assert!(r["content"].as_str().unwrap().ends_with("acme.com") && r["content"].as_str().unwrap().contains("Answer ("), "{r}");
    let tags: Vec<&str> = r["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert!(tags.contains(&"resolved") && tags.contains(&"tarea:t42") && !tags.contains(&"open") && !tags.contains(&"blocking"), "{tags:?}");
    assert!(json(omoi(&f.dir, root, &["decisions", "--blocking", "--tag", "tarea:t42", "--json"], None)).as_array().unwrap().is_empty());
    assert_eq!(json(omoi(&f.dir, root, &["list", "--tag", "resolved", "--tag", "tarea:t42", "--json"], None)).as_array().unwrap().len(), 1);
    // Without an answer, no; one that isn't a decision, no; another client's, neither.
    assert!(!omoi(&f.dir, root, &["decisions", "--resolve", &id], None).status.success());
    let db = db::open(&f.dir.join("o.db")).unwrap();
    let globex_id = memory::decisions(&db, &Scope::All, true, &["".to_string()], 50).unwrap().into_iter().find(|d| d.title.starts_with("Globex")).unwrap().id;
    drop(db);
    let o = omoi(&f.dir, root, &["decisions", "--resolve", &globex_id, "--answer", "sí"], None);
    assert!(!o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    // Through MCP: the same, and it doesn't cross spaces either.
    let call = |args: serde_json::Value| omoikane::mcp::run_tool(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("decisions"), &args.to_string(), "test").unwrap();
    let (t, error) = call(serde_json::json!({ "resolve": globex_id, "answer": "sí" }));
    assert!(error, "{t}");
    let other = json(omoi(&f.dir, root, &["decisions", "--blocking", "--tag", "tarea:t7", "--json"], None))[0]["id"].as_str().unwrap().to_string();
    let (t, error) = call(serde_json::json!({ "resolve": other, "answer": "más tarde" }));
    assert!(!error && t.contains("\"resolved\": true"), "{t}");
}
