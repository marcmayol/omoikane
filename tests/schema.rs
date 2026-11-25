//! 0.20.0: schema 10, the one 1.0 freezes. Projects and spaces as tables (and the migration from a
//! schema 9 database like the existing ones), spaces that isolate clients, clones and worktrees
//! that are the same project, moved folders, `lector_minimo`, verdicts by logical clock and kinds
//! this version doesn't know.

mod common;

use omoikane::search::{self, SearchOptions};
use omoikane::memory::{self, Scope, Changes, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::{db, project, verdicts};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

fn temp_dir(name: &str) -> common::TempDir {
    common::TempDir::new(&format!("esquema-{name}"))
}

/// A fake git repository with its `origin` (what `sync::project_key_for` reads).
fn repo(dir: &Path, url: &str) -> String {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n")).unwrap();
    project::of_dir(dir).unwrap()
}

/// A worktree of `main_dir` at `dir` (`.git` is a file pointing to the repository).
fn worktree(main_dir: &Path, dir: &Path) -> String {
    let internal = main_dir.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&internal).unwrap();
    std::fs::write(internal.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".git"), format!("gitdir: {}\n", internal.display())).unwrap();
    project::of_dir(dir).unwrap()
}

fn loose_dir(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    project::of_dir(dir).unwrap()
}

fn save(db: &mut Connection, project: &str, kind: Kind, title: &str, content: &str) -> String {
    let n = NewMemory { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() };
    memory::save(db, n, None).unwrap().id
}

fn titles(v: impl IntoIterator<Item = memory::Memory>) -> Vec<String> {
    let mut t: Vec<String> = v.into_iter().map(|m| m.title).collect();
    t.sort();
    t
}

fn one(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

// --- Migration 9 → 10 ------------------------------------------------------------------------------

/// A memory written the way 0.19 wrote it (schema 9): with `project` as a path, its FTS index and
/// its vector.
#[allow(clippy::too_many_arguments)]
fn memory_9(db: &Connection, n: i64, id: &str, project: &str, kind: &str, title: &str, content: &str, hlc: &str) {
    db.execute(
        "INSERT INTO memory(n, id, project, kind, title, content, tags, created_at, updated_at, hlc, author)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[\"t\"]', '2026-10-20T10:00:00.000Z', '2026-10-20T10:00:00.000Z', ?7, 'ana')",
        params![n, id, project, kind, title, content, hlc],
    )
    .unwrap();
    db.execute("INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, 't', '')", params![n, title, content]).unwrap();
    let v: Vec<u8> = (0..8u8).map(|x| x.wrapping_mul(n as u8)).collect();
    db.execute("INSERT INTO memory_vec(n, e) VALUES (?1, ?2)", params![n, v]).unwrap();
}

#[test]
fn migrates_a_schema_9_database_without_losing_anything() {
    let dir = temp_dir("migra9");
    let web_a = repo(&dir.join("web"), "git@github.com:acme/web.git");
    let web_b = repo(&dir.join("web-clon"), "https://github.com/acme/web");
    let notes = loose_dir(&dir.join("notas"));
    let gone_path = dir.join("ya-no-existe").display().to_string();
    let foreign = "/home/ana/app";
    let path = dir.join("o.db");
    let ids: Vec<String> = (1..=8).map(|i| format!("01J{i:0>23}")).collect();
    {
        let db = db::open_at_schema(&path, 9).unwrap();
        memory_9(&db, 1, &ids[0], &web_a, "fact", "Despliegue de la web", "Se publica con el script zafiro.", "1793181600000-0000-");
        memory_9(&db, 2, &ids[1], &web_b, "gotcha", "El clon tiene otra caché", "La caché zafiro del clon.", "1793181600001-0002-pc-a");
        memory_9(&db, 3, &ids[2], &notes, "decision", "Notas en markdown", "Todo en markdown.", "1793181600002-0000000000-pc-a");
        memory_9(&db, 4, &ids[3], &gone_path, "fact", "Proyecto que se movió", "Su carpeta ya no está.", "1793181600003-0000000000-pc-a");
        memory_9(&db, 5, &ids[4], foreign, "fact", "La app de Ana", "Viene de otro equipo.", "1793181600004-0000000000-pc-b");
        memory_9(&db, 6, &ids[5], "", "preference", "Respuestas cortas", "Sin rodeos.", "1793181600005-0000000000-pc-a");
        memory_9(&db, 7, &ids[6], "", "procedure", "Publicar", "when: publicar\n1. tag", "1793181600006-0000000000-pc-a");
        memory_9(&db, 8, &ids[7], &web_a, "fact", "Otra de la web", "Más del despliegue zafiro.", "1793181600007-0000000000-pc-a");
        db.execute_batch(&format!(
            "INSERT INTO remote_project(path, key) VALUES ('{foreign}', 'git:github.com/ana/app');
             INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason)
               VALUES ('{a}', 1, 'Antes', 'Lo de antes.', 'fact', '[]', '[]', '2026-10-19T00:00:00.000Z', 'cambio');
             INSERT INTO memory_path(memory_id, path) VALUES ('{a}', 'deploy.sh');
             INSERT INTO verdict(memory_id, other_id, relation, at, hlc, reason, author)
               VALUES ('{h}', '{a}', 'superseded', '2026-10-21T00:00:00.000Z', '1793260800000-0001-pc-a', 'la nueva', 'ana');
             INSERT INTO tombstone(id, deleted_at, hlc) VALUES ('01JZZZZZZZZZZZZZZZZZZZZZZZ', '2026-10-21T00:00:00.000Z', '1793260800000-0005-pc-a');
             INSERT INTO forgotten(id, memory, versions, hlc, deleted_at, by)
               VALUES ('01JZZZZZZZZZZZZZZZZZZZZZZZ', '{{\"id\":\"01JZZZZZZZZZZZZZZZZZZZZZZZ\",\"project\":\"{web}\",\"kind\":\"fact\",\"title\":\"Olvidada\",\"content\":\"En la papelera.\",\"paths\":[],\"tags\":[],\"pinned\":false,\"source\":\"\",\"created_at\":\"2026-10-01T00:00:00.000Z\",\"updated_at\":\"2026-10-01T00:00:00.000Z\",\"recalled_at\":null,\"recall_count\":0}}', '[]', '1793260800000-0004-pc-a', '2026-10-21T00:00:00.000Z', 'pc-a');
             INSERT INTO session(session, project, started_at) VALUES ('s1', '{web_a}', '2026-10-30T00:00:00.000Z');
             INSERT INTO recall_log(session, memory_id, at) VALUES ('s1', '{a}', '2026-10-30T00:00:00.000Z');
             INSERT INTO meta(k, v) VALUES ('hlc', '1793260800000-0005-pc-a');",
            a = ids[0],
            h = ids[7],
            // In the JSON, each backslash is doubled.
            web = web_a.replace('\\', "\\\\"),
        ))
        .unwrap();
    }
    let db = db::open(&path).unwrap();
    assert!(Path::new(&format!("{}.antes-del-esquema-10", path.display())).exists(), "copia antes de migrar");
    assert_eq!(one(&db, "PRAGMA user_version"), db::SCHEMA_VERSION);
    let ok: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    assert_eq!(ok, "ok");
    assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    // The same memories, with the same id and the same `n` (so FTS and vectors stay valid).
    assert_eq!(one(&db, "SELECT count(*) FROM memory"), 8);
    for (i, id) in ids.iter().enumerate() {
        let n: i64 = db.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
        assert_eq!(n, i as i64 + 1);
    }
    assert_eq!(one(&db, "SELECT count(*) FROM memory_vec v JOIN memory m ON m.n = v.n"), 8);
    assert_eq!(one(&db, "SELECT count(*) FROM memory_version"), 1);
    assert_eq!(one(&db, "SELECT count(*) FROM memory_path"), 1);
    assert_eq!(one(&db, "SELECT count(*) FROM forgotten"), 1);
    assert_eq!(one(&db, "SELECT count(*) FROM tombstone"), 1);
    assert_eq!(memory::history(&db, &ids[0]).unwrap()[0].title, "Antes");
    // Search by words, the same.
    let o = SearchOptions { scope: Scope::All, kind: None, tags: vec![], limit: 10 };
    let mut db = db;
    assert_eq!(search::search(&mut db, "zafiro", &o, None).unwrap().len(), 3);
    // Projects: the two clones with the same `origin`, one; the folder without git, its own; the one
    // that's gone, marked; the one that came from another computer, with its key and as a foreign path.
    fn pid_of(db: &Connection, id: &str) -> i64 {
        db.query_row("SELECT project_id FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap()
    }
    let pid = |id: &str| -> i64 { pid_of(&db, id) };
    assert_eq!(pid(&ids[0]), pid(&ids[1]), "clones del mismo repositorio");
    assert_eq!(pid(&ids[0]), pid(&ids[7]));
    assert_ne!(pid(&ids[0]), pid(&ids[2]));
    assert_eq!(pid(&ids[5]), 0, "global");
    let key: String = db.query_row("SELECT key FROM project WHERE id = ?1", [pid(&ids[0])], |f| f.get(0)).unwrap();
    assert_eq!(key, "git:github.com/acme/web");
    let (foreign_key, is_foreign): (String, bool) = db
        .query_row("SELECT p.key, pp.is_foreign FROM project p JOIN project_path pp ON pp.project_id = p.id WHERE p.id = ?1", [pid(&ids[4])], |f| Ok((f.get(0)?, f.get(1)?)))
        .unwrap();
    assert_eq!((foreign_key.as_str(), is_foreign), ("git:github.com/ana/app", true));
    let list = project::list(&db, None, None, 50).unwrap();
    assert!(list.iter().any(|p| p.path == gone_path && p.gone), "{list:?}");
    assert!(!list.iter().any(|p| p.path == foreign && p.gone), "lo de otro equipo no «ya no está»");
    assert_eq!(memory::get(&db, &ids[0]).unwrap().project, web_a);
    // Verdict, still valid; stamps in today's format (comparing as text = comparing stamps).
    assert_eq!(verdicts::pairs(&db).unwrap().len(), 1);
    assert_eq!(one(&db, "SELECT count(*) FROM memory WHERE hlc != '' AND length(hlc) - length(replace(hlc, '-', '')) >= 2 AND substr(hlc, 15, 10) GLOB '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]'"), 8);
    let stamp: String = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [&ids[1]], |f| f.get(0)).unwrap();
    assert_eq!(stamp, "1793181600001-0000000002-pc-a");
    let stamp: String = db.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).unwrap();
    assert_eq!(stamp, "1793260800000-0000000005-pc-a");
    // `remote_project` is gone; `review` without `dismissed_at`; `lector_minimo`.
    assert!(db.query_row("SELECT count(*) FROM remote_project", [], |f| f.get::<_, i64>(0)).is_err());
    assert!(db.query_row("SELECT count(*) FROM review WHERE dismissed_at IS NULL", [], |f| f.get::<_, i64>(0)).is_err());
    let minimum: String = db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |f| f.get(0)).unwrap();
    assert_eq!(minimum, "10");
    // `recall_log` with its session's project.
    assert_eq!(one(&db, "SELECT project_id FROM recall_log WHERE session = 's1'"), pid(&ids[0]));
    // The trash returns the forgotten one to its project (by path: it's from before schema 10).
    let web = pid(&ids[0]);
    let m = memory::restore(&mut db, "01JZZZZZZZZZZZZZZZZZZZZZZZ", None).unwrap();
    assert_eq!(m.project_id, web);
    // And writing goes on as always.
    let new_id = save(&mut db, &web_b, Kind::Fact, "Nueva tras migrar", "Funciona.");
    assert_eq!(pid_of(&db, &new_id), web);
}

#[test]
fn a_newer_schema_database_opens_if_its_min_reader_allows_it() {
    let dir = temp_dir("lector");
    let path = dir.join("o.db");
    {
        let mut db = db::open(&path).unwrap();
        save(&mut db, "", Kind::Fact, "Una", "Algo.");
        // A future version that only adds: schema 11, readable from 10.
        db.execute_batch("PRAGMA user_version = 11; CREATE TABLE futura (x); INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', '10');").unwrap();
    }
    {
        let db = db::open(&path).unwrap();
        assert_eq!(memory::recent(&db, &Scope::All, None, 10).unwrap().len(), 1);
        assert_eq!(one(&db, "PRAGMA user_version"), 11, "no la baja ni la toca");
    }
    {
        // One that breaks: readable only from 12.
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA user_version = 12; UPDATE meta SET v = '12' WHERE k = 'lector_minimo';").unwrap();
    }
    let e = db::open(&path).expect_err("no se abre");
    assert!(format!("{e:#}").contains("12"), "{e:#}");
    {
        // One from before schema 10 with a newer schema (without `lector_minimo`): not either.
        let db = Connection::open(&path).unwrap();
        db.execute_batch("DELETE FROM meta WHERE k = 'lector_minimo'; PRAGMA user_version = 11;").unwrap();
    }
    assert!(db::open(&path).is_err());
}

// --- Spaces --------------------------------------------------------------------------------------

struct Clients {
    /// Goes before the database: when destructuring the struct into variables, the database is released before deleting it.
    dir: common::TempDir,
    db: Connection,
    a_web: String,
    a_app: String,
    b_web: String,
    loose: String,
}

/// Two clients: Acme (web and app) and Globex (web), a project without a space, and memories of each
/// one, of each space and global, all with the word «zafiro».
fn clients(name: &str) -> Clients {
    let dir = temp_dir(name);
    let a_web = loose_dir(&dir.join("acme-web"));
    let a_app = loose_dir(&dir.join("acme-app"));
    let b_web = loose_dir(&dir.join("globex-web"));
    let loose = loose_dir(&dir.join("personal"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    save(&mut db, &a_web, Kind::Fact, "Acme web: puerto zafiro", "El servidor zafiro de Acme escucha en el 7741.");
    save(&mut db, &a_app, Kind::Fact, "Acme app: build zafiro", "La app de Acme compila con el perfil zafiro.");
    save(&mut db, &b_web, Kind::Fact, "Globex web: puerto zafiro", "El servidor zafiro de Globex escucha en el 9931.");
    save(&mut db, &loose, Kind::Fact, "Personal: zafiro", "Mi script zafiro de copias.");
    save(&mut db, "", Kind::Preference, "Global: respuestas zafiro", "Respuestas cortas, también con zafiro.");
    let acme = project::create_space(&db, "Acme").unwrap();
    let globex = project::create_space(&db, "Globex").unwrap();
    for (p, s) in [(&a_web, acme), (&a_app, acme), (&b_web, globex)] {
        let id = project::register(&db, p).unwrap();
        project::assign_space(&db, id, Some(s)).unwrap();
    }
    for (s, t) in [(acme, "Acme espacio: VPN zafiro"), (globex, "Globex espacio: VPN zafiro")] {
        let n = NewMemory { space_id: Some(s), kind: Some(Kind::Gotcha), title: t.into(), content: "La VPN zafiro del cliente.".into(), ..Default::default() };
        memory::save(&mut db, n, None).unwrap();
    }
    Clients { dir, db, a_web, a_app, b_web, loose }
}

#[test]
fn from_one_client_nothing_of_another_is_seen() {
    let Clients { dir: _dir, mut db, a_web, a_app, b_web, loose } = clients("aislar");
    let from_a = Scope::from_project(&db, &a_web).unwrap();
    let o = SearchOptions { scope: from_a, kind: None, tags: vec![], limit: 50 };
    let seen = titles(search::search(&mut db, "zafiro", &o, None).unwrap().into_iter().map(|r| r.memory));
    assert_eq!(seen, ["Acme espacio: VPN zafiro", "Acme web: puerto zafiro", "Global: respuestas zafiro"]);
    assert_eq!(titles(memory::recent(&db, &from_a, None, 50).unwrap()), seen);
    // «All projects» from an Acme project: Acme's, never Globex's.
    let wide = from_a.widen();
    let o = SearchOptions { scope: wide, kind: None, tags: vec![], limit: 50 };
    let seen = titles(search::search(&mut db, "zafiro", &o, None).unwrap().into_iter().map(|r| r.memory));
    assert_eq!(seen, ["Acme app: build zafiro", "Acme espacio: VPN zafiro", "Acme web: puerto zafiro", "Global: respuestas zafiro"]);
    // The context (preferences and pinned) and the project list.
    assert!(memory::context(&db, &from_a, 30).unwrap().iter().all(|m| !m.title.contains("Globex")));
    // From a project without a space: neither Acme nor Globex (only its own, other space-less ones' and the global ones).
    let from_loose = Scope::from_project(&db, &loose).unwrap().widen();
    assert_eq!(titles(memory::recent(&db, &from_loose, None, 50).unwrap()), ["Global: respuestas zafiro", "Personal: zafiro"]);
    // From the general folder (the user's), everything.
    assert_eq!(memory::recent(&db, &Scope::All, None, 50).unwrap().len(), 7);
    // The recall: even though the Globex memory matches the message better, it doesn't arrive.
    let e = HookInput { session_id: "s".into(), prompt: "¿en qué puerto escucha el servidor zafiro de Globex?".into(), ..Default::default() };
    let r = recall::on_prompt(&mut db, &a_web, &e, None).unwrap().unwrap_or_default();
    assert!(!r.contains("Globex web") && !r.contains("9931"), "{r}");
    let e = HookInput { session_id: "s2".into(), ..Default::default() };
    let r = recall::on_session_start(&mut db, &a_app, &e).unwrap().unwrap_or_default();
    assert!(!r.contains("Globex"), "{r}");
    assert!(r.contains("Acme"), "{r}");
    // And Globex's stuff is visible from Globex.
    let from_b = Scope::from_project(&db, &b_web).unwrap();
    assert!(titles(memory::recent(&db, &from_b, None, 50).unwrap()).iter().any(|t| t.starts_with("Globex web")));
}

/// The same through the MCP tools (`search`, `recent`, `context`), which is what the agent sees,
/// also asking for another project or another space on purpose.
#[test]
fn through_mcp_spaces_do_not_cross_either() {
    let Clients { dir, db, a_web, b_web, .. } = clients("aislar-mcp");
    drop(db);
    let path = dir.join("o.db");
    let call = |from: &str, name: &str, args: serde_json::Value| -> (String, bool) {
        omoikane::mcp::run_tool(path.clone(), Some(PathBuf::from(from)), Some(name), &args.to_string(), "test").unwrap()
    };
    let (t, error) = call(&a_web, "search", serde_json::json!({ "query": "zafiro", "all_projects": true }));
    assert!(!error && t.contains("Acme app") && !t.contains("Globex"), "{t}");
    let (t, _) = call(&a_web, "recent", serde_json::json!({ "limit": 50 }));
    assert!(t.contains("Acme web") && !t.contains("Globex"), "{t}");
    let (t, error) = call(&a_web, "search", serde_json::json!({ "query": "zafiro", "project": "globex-web" }));
    assert!(error && !t.contains("9931"), "{t}");
    let (t, error) = call(&a_web, "recent", serde_json::json!({ "space": "globex" }));
    assert!(error && !t.contains("9931"), "{t}");
    let (t, _) = call(&a_web, "context", serde_json::json!({}));
    let c: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(c["project"]["space"], "acme", "{c}");
    assert!(c["project"]["key"].as_str().unwrap().starts_with("dir:"), "{c}");
    let names: Vec<&str> = c["projects"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"acme-app") && !names.contains(&"globex-web"), "{names:?}");
    assert!(!t.contains("Globex"), "{t}");
    // From Acme, another Acme project by its name (or a part of it) works.
    let (t, error) = call(&a_web, "search", serde_json::json!({ "query": "zafiro", "project": "app" }));
    assert!(!error && t.contains("Acme app"), "{t}");
    // Save at space level with `space: true`, and see it from the space's other project.
    let (t, error) = call(&a_web, "remember", serde_json::json!({ "title": "Acme: el staging es lento", "content": "Lleva dos réplicas.", "kind": "fact", "space": true, "force": true }));
    assert!(!error && t.contains("space acme"), "{t}");
    let (t, _) = call(&dir.join("acme-app").display().to_string(), "recent", serde_json::json!({}));
    assert!(t.contains("staging"), "{t}");
    let (t, _) = call(&b_web, "recent", serde_json::json!({ "limit": 50 }));
    assert!(!t.contains("staging") && !t.contains("Acme"), "{t}");
    // A preference, even if saved from a client's project, belongs to the person: global.
    let (t, error) = call(&a_web, "remember", serde_json::json!({ "title": "Commits en inglés", "content": "Siempre.", "kind": "preference" }));
    assert!(!error && t.contains("(global)"), "{t}");
    let (t, _) = call(&b_web, "context", serde_json::json!({}));
    assert!(t.contains("Commits en inglés"), "{t}");
    // From something without a space, a name is only looked up among space-less projects: the error
    // doesn't name the clients' ones.
    let (t, error) = call(&dir.display().to_string(), "search", serde_json::json!({ "query": "zafiro", "project": "web" }));
    assert!(error && !t.contains("acme-web") && !t.contains("globex-web"), "{t}");
}

#[test]
fn an_exact_name_picks_between_two_that_differ_only_in_case() {
    let dir = temp_dir("mayusculas");
    let repo_path = repo(&dir.join("codigo").join("Omoikane"), "git@github.com:acme/omoikane.git");
    let campaign = dir.join("campana").join("omoikane");
    std::fs::create_dir_all(&campaign).unwrap();
    let db = db::open(&dir.join("o.db")).unwrap();
    let (a, b) = (project::register(&db, &repo_path).unwrap(), project::register(&db, &campaign.display().to_string()).unwrap());
    assert_ne!(a, b);
    assert_eq!(project::find(&db, "Omoikane").unwrap(), a);
    assert_eq!(project::find(&db, "omoikane").unwrap(), b);
    // With other casing there's no way to know which: it keeps asking to be told.
    let e = project::find(&db, "OMOIKANE").unwrap_err().to_string();
    assert!(e.contains("Omoikane") && e.contains("omoikane"), "{e}");
}

// --- Clones, worktrees and moved folders -----------------------------------------------------------

#[test]
fn two_clones_and_a_worktree_are_one_project() {
    let dir = temp_dir("clones");
    let a = repo(&dir.join("web"), "git@github.com:acme/web.git");
    let b = repo(&dir.join("web-2"), "https://github.com/acme/web.git");
    let w = worktree(&dir.join("web"), &dir.join("web-rama"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    save(&mut db, &a, Kind::Gotcha, "La caché del build", "Borrar target al cambiar de rama.");
    let (pa, pb, pw) = (project::register(&db, &a).unwrap(), project::register(&db, &b).unwrap(), project::register(&db, &w).unwrap());
    assert_eq!((pa, pa), (pb, pw));
    assert_eq!(one(&db, "SELECT count(*) FROM project_path"), 3);
    // It's visible from any of the three, and doesn't show up as «from another project».
    for from in [&a, &b, &w] {
        assert_eq!(memory::recent(&db, &Scope::from_project(&db, from).unwrap(), None, 10).unwrap().len(), 1);
    }
    let e = HookInput { session_id: "s".into(), ..Default::default() };
    let r = recall::on_session_start(&mut db, &w, &e).unwrap().unwrap();
    assert!(r.contains("La caché del build") && !r.contains("[project"), "{r}");
}

#[test]
fn a_moved_folder_is_still_its_project() {
    let dir = temp_dir("movida");
    let old_path = repo(&dir.join("app"), "git@github.com:acme/app.git");
    let no_git = loose_dir(&dir.join("notas"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let id = save(&mut db, &old_path, Kind::Fact, "La app usa Kotlin", "Y Compose.");
    let notes_id = save(&mut db, &no_git, Kind::Fact, "Notas en markdown", "Todo en .md.");
    let key: String = db.query_row("SELECT key FROM project", [], |f| f.get(0)).unwrap_or_default();
    let _ = key;
    // With `origin`: the folder moves and the project follows it by itself (same key, the main path changes).
    std::fs::rename(dir.join("app"), dir.join("app-nueva")).unwrap();
    let new_id = project::of_dir(&dir.join("app-nueva")).unwrap();
    assert_eq!(memory::recent(&db, &Scope::from_project(&db, &new_id).unwrap(), None, 10).unwrap().len(), 1, "ya se ve antes de apuntarla");
    // When the session starts there, it's recorded and announced once.
    let e = HookInput { session_id: "s".into(), ..Default::default() };
    let r = recall::on_session_start(&mut db, &new_id, &e).unwrap().unwrap();
    assert!(r.contains("used to be in") && r.contains("La app usa Kotlin"), "{r}");
    assert_eq!(memory::get(&db, &id).unwrap().project, new_id);
    let r = recall::on_session_start(&mut db, &new_id, &e).unwrap().unwrap();
    assert!(!r.contains("used to be in"), "solo una vez");
    // Without `origin`: a new folder with the same name doesn't join by itself (it could be something
    // else); the old one shows up as «gone», and `projects move` takes it.
    std::fs::rename(dir.join("notas"), dir.join("notas-2026")).unwrap();
    std::fs::create_dir_all(dir.join("otra").join("notas")).unwrap();
    let other = project::of_dir(&dir.join("otra").join("notas")).unwrap();
    let other_pid = project::register(&db, &other).unwrap();
    assert_ne!(other_pid, memory::get(&db, &notes_id).unwrap().project_id, "mismo nombre, otra carpeta");
    assert!(project::list(&db, None, None, 50).unwrap().iter().any(|p| p.path == no_git && p.gone));
    let moved = project::of_dir(&dir.join("notas-2026")).unwrap();
    let pid = memory::get(&db, &notes_id).unwrap().project_id;
    project::move_to(&mut db, pid, &moved, false).unwrap();
    let m = memory::get(&db, &notes_id).unwrap();
    assert_eq!((m.project_id, m.project.as_str()), (pid, moved.as_str()));
    assert!(!project::list(&db, None, None, 50).unwrap().iter().any(|p| p.gone));
    // `projects prune`: the other «notas» folder's project, without memories, goes away.
    assert_eq!(project::prune(&db).unwrap(), 1);
    assert_eq!(project::resolve(&db, &other).unwrap(), None);
}

#[test]
fn merging_two_projects_keeps_ids_and_leaves_the_newest_status() {
    let dir = temp_dir("fundir");
    let a = loose_dir(&dir.join("viejo"));
    let b = loose_dir(&dir.join("nuevo"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let ea = memory::save(&mut db, NewMemory { project: a.clone(), kind: Some(Kind::Fact), title: "Estado viejo".into(), content: "Iba por la 1.0.".into(), tags: vec!["status".into()], ..Default::default() }, None).unwrap().id;
    let other = save(&mut db, &a, Kind::Fact, "Una del viejo", "Algo.");
    memory::update(&mut db, &other, Changes { content: Some("Algo más.".into()), ..Default::default() }, None).unwrap();
    let eb = memory::save(&mut db, NewMemory { project: b.clone(), kind: Some(Kind::Fact), title: "Estado nuevo".into(), content: "Voy por la 2.0.".into(), tags: vec!["status".into()], ..Default::default() }, None).unwrap().id;
    let (pa, pb) = (project::register(&db, &a).unwrap(), project::register(&db, &b).unwrap());
    assert_eq!(project::merge(&mut db, pa, pb, false).unwrap(), 2);
    for id in [&ea, &other, &eb] {
        assert_eq!(memory::get(&db, id).unwrap().project_id, pb);
    }
    assert_eq!(memory::history(&db, &other).unwrap().len(), 1, "con su historial");
    assert_eq!(verdicts::last_in_chain(&db, &ea).unwrap().map(|x| x.0), Some(eb.clone()), "el estado viejo, sustituido por el nuevo");
    assert_eq!(project::resolve(&db, &a).unwrap().map(|x| x.0), Some(pb), "la carpeta vieja ya es del nuevo");
}

// --- Verdicts by logical clock and unknown kinds ---------------------------------------------------

/// A computer with its clock 30 minutes ahead changes a memory; here it's judged afterwards. Comparing
/// wall clocks (`updated_at` against `at`), the verdict wasn't valid until half an hour had passed.
#[test]
fn a_verdict_holds_even_with_skewed_wall_clocks() {
    let dir = temp_dir("relojes");
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let a = save(&mut db, "/p", Kind::Fact, "El puerto es el 8080", "Del servidor.");
    let b = save(&mut db, "/p", Kind::Fact, "El puerto es el 9090", "Del servidor.");
    // What sync leaves: the other computer's change with its (ahead) time and its stamp.
    let future_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 30 * 60 * 1000;
    let stamp = omoikane::clock::format_stamp(future_ms, 0, "pc-adelantado");
    db.execute(
        "UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+30 minutes'), hlc = ?2, content_hlc = ?2 WHERE id = ?1",
        params![a, stamp],
    )
    .unwrap();
    omoikane::clock::receive(&db, &stamp).unwrap();
    verdicts::set(&db, &a, &b, verdicts::SUPERSEDED, "la nueva", "user").unwrap();
    assert!(verdicts::pairs(&db).unwrap().contains_key(&verdicts::pair_key(&a, &b)), "vale ya");
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap().map(|x| x.0), Some(b.clone()));
    let (at, changed_at): (String, String) = db.query_row("SELECT v.at, m.updated_at FROM verdict v JOIN memory m ON m.id = v.memory_id", [], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
    assert!(changed_at > at, "con la regla de antes no valdría: {changed_at} > {at}");
    // Pinning or tagging it doesn't expire it; changing the text does.
    memory::update(&mut db, &a, Changes { pinned: Some(true), tags: Some(vec!["puertos".into()]), ..Default::default() }, None).unwrap();
    assert!(verdicts::pairs(&db).unwrap().contains_key(&verdicts::pair_key(&a, &b)));
    memory::update(&mut db, &a, Changes { content: Some("Del servidor de pruebas.".into()), ..Default::default() }, None).unwrap();
    assert!(!verdicts::pairs(&db).unwrap().contains_key(&verdicts::pair_key(&a, &b)));
}

/// A kind from a future version arrives through an export: it's stored as is, treated as `fact` and
/// comes out the same again.
#[test]
fn an_unknown_kind_is_kept() {
    let dir = temp_dir("tipo");
    let path = dir.join("o.db");
    let mut db = db::open(&path).unwrap();
    let id = save(&mut db, "", Kind::Fact, "Una idea", "Para más adelante.");
    db.execute("UPDATE memory SET kind = 'insight' WHERE id = ?1", [&id]).unwrap();
    let m = memory::get(&db, &id).unwrap();
    assert_eq!((m.kind, m.kind_text.as_deref()), (Kind::Fact, Some("insight")));
    // Changing something else doesn't lose it.
    memory::update(&mut db, &id, Changes { content: Some("Para la 2.0.".into()), ..Default::default() }, None).unwrap();
    let kind: String = db.query_row("SELECT kind FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(kind, "insight");
    let mut out = Vec::new();
    omoikane::backup::export(&db, &path, &Scope::All, false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("\"kind\":\"insight\""), "{text}");
    // And it goes into another database the same.
    let file = dir.join("copia.jsonl");
    std::fs::write(&file, &text).unwrap();
    let path2 = dir.join("otra.db");
    let mut other = db::open(&path2).unwrap();
    omoikane::backup::import(&mut other, &path2, &file, "", None, false, Default::default()).unwrap();
    let kind: String = other.query_row("SELECT kind FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(kind, "insight");
}

/// To learn across projects (0.21.0): from which project each memory was received. The recall
/// records it in `recall_log` (with its project) and, when it expires, it's summarized in
/// `memory_use`; a search from a project is added directly.
#[test]
fn usage_from_each_project_is_summarized() {
    let dir = temp_dir("uso");
    let a = loose_dir(&dir.join("balua"));
    let b = loose_dir(&dir.join("lumbre"));
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let id = save(&mut db, "", Kind::Gotcha, "bitsandbytes 4-bit rompe la visión en la RTX 5070 Ti", "Cuantizar a 8 bits.");
    let (pa, pb) = (project::register(&db, &a).unwrap(), project::register(&db, &b).unwrap());
    let e = HookInput { session_id: "s1".into(), prompt: "la visión con bitsandbytes 4-bit en la RTX 5070 Ti falla".into(), ..Default::default() };
    let r = recall::on_prompt(&mut db, &a, &e, None).unwrap().unwrap_or_default();
    assert!(r.contains("bitsandbytes"), "{r}");
    let in_log: i64 = db.query_row("SELECT project_id FROM recall_log WHERE session = 's1' AND memory_id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(in_log, pa);
    // The session compacts (or another starts): what was injected is deleted from the log and summarized.
    recall::on_session_start(&mut db, &a, &HookInput { session_id: "s1".into(), source: "compact".into(), ..Default::default() }).unwrap();
    assert_eq!(one(&db, "SELECT count(*) FROM recall_log WHERE session = 's1'"), 0);
    let (times, used_pid): (i64, i64) = db.query_row("SELECT veces, project_id FROM memory_use WHERE memory_id = ?1", [&id], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
    assert_eq!((times, used_pid), (1, pa));
    // Searched from another project.
    let o = SearchOptions { scope: Scope::from_project(&db, &b).unwrap(), kind: None, tags: vec![], limit: 5 };
    assert!(!search::search(&mut db, "bitsandbytes", &o, None).unwrap().is_empty());
    assert_eq!(one(&db, &format!("SELECT count(DISTINCT project_id) FROM memory_use WHERE memory_id = '{id}'")), 2);
    assert_eq!(one(&db, &format!("SELECT veces FROM memory_use WHERE memory_id = '{id}' AND project_id = {pb}")), 1);
    // Forget it completely (and nothing is left dangling).
    memory::forget_for_good(&mut db, &id).unwrap();
    assert_eq!(one(&db, "SELECT count(*) FROM memory_use"), 0);
}

// --- Local person and isolation by id (1.0 review) ---------------------------------------------------

/// The key of a database's only (non-global) project.
fn the_key(db: &Connection) -> String {
    db.query_row("SELECT key FROM project WHERE id != 0", [], |f| f.get(0)).unwrap()
}

/// The 9 → 10 migration never leaves a `dir:/<folder>` key: without sync, the local person (a new and
/// stable one); with it, the one in its `sync.json`.
#[test]
fn the_migration_puts_a_person_in_dir_keys() {
    for (name, sync) in [("persona-nueva", None), ("persona-sync", Some("ana"))] {
        let dir = temp_dir(name);
        let notes = loose_dir(&dir.join("notas"));
        let path = dir.join("o.db");
        {
            let db = db::open_at_schema(&path, 9).unwrap();
            memory_9(&db, 1, "01J00000000000000000000001", &notes, "fact", "Notas", "En markdown.", "1793181600000-0000000000-pc-a");
        }
        if let Some(p) = sync {
            std::fs::write(dir.join("o.sync.json"), serde_json::json!({ "url": "x", "kinds": [], "global": true, "person": p }).to_string()).unwrap();
        }
        let db = db::open(&path).unwrap();
        let person = db::person_of(&db);
        match sync {
            Some(p) => assert_eq!(person, p),
            None => assert!(person.starts_with("u-") && person.len() == 10, "{person}"),
        }
        assert_eq!(the_key(&db), format!("dir:{person}/notas"));
        drop(db);
        assert_eq!(db::person_of(&db::open(&path).unwrap()), person, "estable");
    }
}

/// A schema 10 database from before the local person (from an unreleased version): on opening it,
/// its `dir:/<folder>` keys get a person, the forget marks their space column and the projects their
/// stamp column (`project.hlc`).
#[test]
fn a_schema_10_database_without_a_person_is_brought_up_to_date() {
    let dir = temp_dir("diez-sin-persona");
    let notes = loose_dir(&dir.join("notas"));
    let path = dir.join("o.db");
    {
        let mut db = db::open(&path).unwrap();
        save(&mut db, &notes, Kind::Fact, "Notas", "En markdown.");
        let other = save(&mut db, &notes, Kind::Fact, "Otra", "Para olvidar.");
        memory::forget(&mut db, &other).unwrap();
        db.execute_batch("DELETE FROM meta WHERE k = 'persona'; UPDATE project SET key = 'dir:/notas' WHERE id != 0; ALTER TABLE tombstone DROP COLUMN space_key; ALTER TABLE project DROP COLUMN hlc;").unwrap();
    }
    let db = db::open(&path).unwrap();
    let person = db::person_of(&db);
    assert!(person.starts_with("u-"), "{person}");
    assert_eq!(the_key(&db), format!("dir:{person}/notas"));
    assert_eq!(one(&db, "SELECT count(*) FROM tombstone WHERE space_key = ''"), 1);
    assert_eq!(one(&db, "SELECT count(*) FROM project WHERE hlc = ''"), 2, "la columna, vacía");
}

/// The same old database opened by several processes (here, threads) at once: updating it checks and
/// writes in a single transaction, so none fails with «duplicate column» and all see the same
/// person.
#[test]
fn an_old_schema_10_database_opened_concurrently() {
    for round in 0..5 {
        let dir = temp_dir(&format!("diez-a-la-vez-{round}"));
        let notes = loose_dir(&dir.join("notas"));
        let path = dir.join("o.db");
        {
            let mut db = db::open(&path).unwrap();
            save(&mut db, &notes, Kind::Fact, "Notas", "En markdown.");
            db.execute_batch("DELETE FROM meta WHERE k = 'persona'; ALTER TABLE tombstone DROP COLUMN space_key; ALTER TABLE project DROP COLUMN hlc; ALTER TABLE project DROP COLUMN paused;").unwrap();
        }
        let out = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (path, out) = (path.clone(), out.clone());
                std::thread::spawn(move || {
                    out.wait();
                    db::open(&path).map(|db| db::person_of(&db)).map_err(|e| format!("{e:#}"))
                })
            })
            .collect();
        let persons: Vec<String> = threads.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
        assert!(persons.iter().all(|p| *p == persons[0]), "{persons:?}");
        let db = db::open(&path).unwrap();
        assert_eq!(one(&db, "SELECT count(*) FROM project WHERE hlc = '' AND paused = ''"), 2);
    }
}

/// By id it doesn't cross either: `get`, `history`, `update` (and its verdicts and `absorb`) and
/// `forget` of a memory of another space (or of a space-less project) error from a client's project;
/// from the user's folder everything is allowed, except a verdict between memories of different
/// spaces, which never is.
#[test]
fn by_id_spaces_do_not_cross_either() {
    let Clients { dir, db, a_web, b_web, .. } = clients("por-id");
    let id_of = |t: &str| -> String { db.query_row("SELECT id FROM memory WHERE title = ?1", [t], |f| f.get(0)).unwrap() };
    let (globex, acme, acme_space, global, personal) = (
        id_of("Globex web: puerto zafiro"),
        id_of("Acme web: puerto zafiro"),
        id_of("Acme espacio: VPN zafiro"),
        id_of("Global: respuestas zafiro"),
        id_of("Personal: zafiro"),
    );
    drop(db);
    let path = dir.join("o.db");
    let call = |from: &str, name: &str, args: serde_json::Value| -> (String, bool) {
        omoikane::mcp::run_tool(path.clone(), Some(PathBuf::from(from)), Some(name), &args.to_string(), "test").unwrap()
    };
    for (name, args) in [
        ("get", serde_json::json!({ "id": globex })),
        // 20 characters: the first 10 of a ULID are the time, and on a fast machine several memories of
        // the test fall in the same millisecond (the prefix would be ambiguous, not «from another space»).
        ("get", serde_json::json!({ "id": &globex[..20] })),
        ("history", serde_json::json!({ "id": globex })),
        ("forget", serde_json::json!({ "id": globex })),
        ("update", serde_json::json!({ "id": globex, "content": "Cambiada desde Acme." })),
        ("update", serde_json::json!({ "id": acme, "supersedes": [globex] })),
        ("update", serde_json::json!({ "id": acme, "contradicts": [globex] })),
        ("update", serde_json::json!({ "id": acme, "compatible": [globex] })),
        ("update", serde_json::json!({ "id": acme, "absorb": [globex], "content": "Juntas." })),
        ("get", serde_json::json!({ "id": personal })),
    ] {
        let (t, error) = call(&a_web, name, args.clone());
        assert!(error && t.contains("not in this scope") && !t.contains("9931"), "{name} {args}: {t}");
    }
    // Its own, its space's and the global ones, yes.
    for id in [&acme, &acme_space, &global] {
        let (t, error) = call(&a_web, "get", serde_json::json!({ "id": id }));
        assert!(!error, "{t}");
    }
    // Globex is still intact and, from Globex, visible.
    let (t, error) = call(&b_web, "get", serde_json::json!({ "id": globex }));
    assert!(!error && t.contains("9931") && !t.contains("Cambiada"), "{t}");
    // From the user's folder, everything; but a verdict (or an absorb) across different spaces, never.
    let home = std::env::home_dir().unwrap().display().to_string();
    let (t, error) = call(&home, "get", serde_json::json!({ "id": globex }));
    assert!(!error && t.contains("9931"), "{t}");
    let (t, error) = call(&home, "update", serde_json::json!({ "id": acme, "supersedes": [globex], "reason": "cruzado" }));
    assert!(error && t.contains("different spaces"), "{t}");
    let (t, error) = call(&home, "update", serde_json::json!({ "id": acme, "absorb": [globex], "content": "Juntas." }));
    assert!(error && t.contains("different spaces"), "{t}");
    let db = db::open(&path).unwrap();
    assert_eq!(one(&db, "SELECT count(*) FROM verdict"), 0);
    assert!(memory::get(&db, &globex).is_ok(), "no se absorbió");
    drop(db);
    // With a global one, yes: it's visible from both.
    let (t, error) = call(&a_web, "update", serde_json::json!({ "id": acme, "compatible": [global] }));
    assert!(!error, "{t}");
}

/// `remember` with `supersedes`/`contradicts` towards a memory of another space: it used to save the
/// memory and then the verdict failed with an error stating the whole id and the NAME of the other
/// space («… are in different spaces («acme» and «globex»)»). Now it's checked before saving (as in
/// `update`): an error without ids or names, and nothing saved. Also with part of the id, from the
/// user's folder towards a project (`project`) of another space, and as a space (`space`).
#[test]
fn remember_neither_judges_nor_names_another_space() {
    let Clients { dir, db, a_web, .. } = clients("remember-juzga");
    let id_of = |t: &str| -> String { db.query_row("SELECT id FROM memory WHERE title = ?1", [t], |f| f.get(0)).unwrap() };
    let (globex, acme) = (id_of("Globex web: puerto zafiro"), id_of("Acme web: puerto zafiro"));
    let before = one(&db, "SELECT count(*) FROM memory");
    drop(db);
    let path = dir.join("o.db");
    let call = |from: &str, args: serde_json::Value| -> (String, bool) {
        omoikane::mcp::run_tool(path.clone(), Some(PathBuf::from(from)), Some("remember"), &args.to_string(), "test").unwrap()
    };
    let home = std::env::home_dir().unwrap().display().to_string();
    let cases = [
        (a_web.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "supersedes": [globex] })),
        (a_web.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "contradicts": [&globex[..20]] })),
        (a_web.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "supersedes": [globex.to_lowercase()], "force": true })),
        (home.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "project": a_web, "supersedes": [globex] })),
        (home.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "space": "acme", "contradicts": [globex] })),
        (home.clone(), serde_json::json!({ "title": "Puerto nuevo", "content": "Ahora el 8080.", "kind": "fact", "project": a_web, "contradicts": [acme, globex] })),
    ];
    for (from, args) in cases {
        let (t, error) = call(&from, args.clone());
        assert!(error, "{args}: {t}");
        let l = t.to_lowercase();
        assert!(!l.contains(&globex.to_lowercase()) && !l.contains(&globex[..10].to_lowercase()) && !l.contains("globex") && !l.contains("acme"), "{args}: {t}");
        let db = db::open(&path).unwrap();
        assert_eq!(one(&db, "SELECT count(*) FROM memory"), before, "nada guardado: {args}");
        assert_eq!(one(&db, "SELECT count(*) FROM verdict"), 0);
    }
    // From the user's folder, the `update` error across spaces doesn't state ids or names either.
    let (t, error) = omoikane::mcp::run_tool(path.clone(), Some(PathBuf::from(&home)), Some("update"), &serde_json::json!({ "id": acme, "supersedes": [globex], "reason": "x" }).to_string(), "test").unwrap();
    assert!(error && t.contains("different spaces") && !t.contains(&globex) && !t.contains(&acme) && !t.contains("globex") && !t.contains("acme"), "{t}");
    // Within its space, it does judge.
    let (t, error) = call(&a_web, serde_json::json!({ "title": "Puerto nuevo de Acme", "content": "Ahora el 8443.", "kind": "fact", "supersedes": [acme], "force": true }));
    assert!(!error && t.contains("\"judged\": 1"), "{t}");
}

/// `merge` and `move_to` between projects of different spaces: an error (from which space to which)
/// unless `force`; an empty one does merge (the folder that registered itself on passing through it).
#[test]
fn merging_across_spaces_requires_force() {
    let Clients { dir, mut db, a_web, a_app, b_web, loose } = clients("fundir-espacios");
    let id = |p: &str| project::resolve(&db, p).unwrap().unwrap().0;
    let (pa, papp, pb, ps) = (id(&a_web), id(&a_app), id(&b_web), id(&loose));
    let e = project::merge(&mut db, pb, pa, false).unwrap_err().to_string();
    assert!(e.contains("«globex» → «acme»"), "{e}");
    let e = project::merge(&mut db, ps, pa, false).unwrap_err().to_string();
    assert!(e.contains("no space → «acme»"), "{e}");
    let e = project::move_to(&mut db, pa, &b_web, false).unwrap_err().to_string();
    assert!(e.contains("«globex» → «acme»"), "{e}");
    // Within the same space, just like that.
    assert_eq!(project::merge(&mut db, papp, pa, false).unwrap(), 1);
    // A folder without memories (the one that registered itself on passing through it) merges without forcing.
    let empty_dir = loose_dir(&dir.join("vacia"));
    let pv = project::register(&db, &empty_dir).unwrap();
    assert_eq!(project::merge(&mut db, pv, pa, false).unwrap(), 0);
    assert_eq!(project::merge(&mut db, pb, pa, true).unwrap(), 1);
}
