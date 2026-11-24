//! 1.8.0: the topic key (`remember --key`, tag `topic:<key>`). One memory per key and per
//! scope: saving with a key that already exists updates it, also with several processes at once (what
//! Kumi does with each project's profile, with two factories in the same repository).

mod common;

use omoikane::db;
use omoikane::memory::{self, Scope, NewMemory, Kind};
use rusqlite::Connection;
use std::path::Path;
use std::process::{Command, Stdio};

fn base(name: &str) -> (common::TempDir, Connection) {
    let dir = common::TempDir::new(&format!("clave-{name}"));
    let db = db::open(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn new_memory(project: &str, title: &str, content: &str) -> NewMemory {
    NewMemory { project: project.into(), kind: Some(Kind::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

fn with_key(db: &Connection, key: &str) -> i64 {
    db.query_row(
        "SELECT count(*) FROM memory m WHERE EXISTS (SELECT 1 FROM json_each(m.tags) WHERE value = ?1)",
        [format!("topic:{key}")],
        |f| f.get(0),
    )
    .unwrap()
}

#[test]
fn the_key_creates_once_and_then_updates() {
    let (_dir, mut db) = base("crea");
    let a = memory::save_by_key(&mut db, "Kumi-Perfil", new_memory("/p", "Perfil de p", "Factoría web, fase 1."), None, None).unwrap();
    assert!(a.created);
    assert_eq!(memory::key_of(&a.memory), Some("kumi-perfil"));
    let mut n = new_memory("/p", "Perfil de p", "Factoría web, fase 2.");
    n.tags = vec!["kumi".into()];
    n.paths = vec!["src/main.rs".into()];
    let b = memory::save_by_key(&mut db, "kumi-perfil", n, None, None).unwrap();
    assert!(!b.created);
    assert_eq!(b.memory.id, a.memory.id);
    assert_eq!(b.memory.content, "Factoría web, fase 2.");
    assert!(b.memory.tags.contains(&"kumi".to_string()) && b.memory.tags.contains(&"topic:kumi-perfil".to_string()));
    assert_eq!(with_key(&db, "kumi-perfil"), 1);
    // The previous version stays in the history, with the key as the reason.
    let h = memory::history(&db, &a.memory.id).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].content, "Factoría web, fase 1.");
    assert_eq!(h[0].reason.as_deref(), Some("key kumi-perfil"));
    // The same key in another project is another memory.
    let c = memory::save_by_key(&mut db, "kumi-perfil", new_memory("/q", "Perfil de q", "Factoría android."), None, None).unwrap();
    assert!(c.created && c.memory.id != a.memory.id);
    // `get --key` from each project gives its own; the global one only if the project has none.
    memory::save_by_key(&mut db, "kumi-perfil", new_memory("", "Perfil por defecto", "Sin factoría."), None, None).unwrap();
    let sees = |p: &str| memory::by_key(&db, &Scope::from_project(&db, p).unwrap(), "KUMI-PERFIL").unwrap().map(|m| m.title);
    assert_eq!(sees("/p").as_deref(), Some("Perfil de p"));
    assert_eq!(sees("/q").as_deref(), Some("Perfil de q"));
    assert_eq!(sees("/r").as_deref(), Some("Perfil por defecto"));
    assert!(memory::by_key(&db, &Scope::from_project(&db, "/p").unwrap(), "otra").unwrap().is_none());
}

#[test]
fn warns_about_a_similar_key_and_rejects_odd_ones() {
    let (_dir, mut db) = base("parecida");
    memory::save_by_key(&mut db, "kumi-perfil", new_memory("/p", "Perfil", "uno"), None, None).unwrap();
    let r = memory::save_by_key(&mut db, "kumi_perfil", new_memory("/p", "Perfil", "dos"), None, None).unwrap();
    assert!(r.created);
    assert_eq!(r.similar_keys, vec!["kumi-perfil".to_string()]);
    let r = memory::save_by_key(&mut db, "kumi-perfl", new_memory("/p", "Perfil", "tres"), None, None).unwrap();
    assert!(r.similar_keys.contains(&"kumi-perfil".to_string()), "{:?}", r.similar_keys);
    // Really different: nothing.
    let r = memory::save_by_key(&mut db, "deploy-recipe", new_memory("/p", "Receta", "cuatro"), None, None).unwrap();
    assert!(r.similar_keys.is_empty(), "{:?}", r.similar_keys);
    // In another project it does not count.
    let r = memory::save_by_key(&mut db, "kumi-perfi", new_memory("/q", "Perfil", "cinco"), None, None).unwrap();
    assert!(r.similar_keys.is_empty());
    for bad in ["", "  ", "dos palabras", "¿qué?"] {
        assert!(memory::save_by_key(&mut db, bad, new_memory("/p", "x", "y"), None, None).is_err(), "{bad:?}");
    }
}

/// Several `omoi` processes at once with the same key in the same repository, via the CLI and via
/// `omoi tool remember` (the plugin): a single memory at the end, with the content of one of them.
#[test]
fn several_processes_at_once_leave_just_one() {
    let dir = common::TempDir::new("clave-procesos");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let home = dir.join("casa");
    std::fs::create_dir_all(&home).unwrap();
    let db_path = dir.join("o.db");
    assert!(!db_path.to_string_lossy().contains(".omoikane"));
    let launch = |i: usize, repo: &Path| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.current_dir(repo)
            .env("OMOIKANE_DB", &db_path)
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if i.is_multiple_of(2) {
            c.args(["remember", "-k", "fact", "-t", "Perfil del proyecto", "--key", "kumi-perfil", &format!("Factoría {i}.")]).stdin(Stdio::null());
            c.spawn().unwrap()
        } else {
            c.args(["tool", "remember"]).stdin(Stdio::piped());
            let mut h = c.spawn().unwrap();
            let p = serde_json::json!({ "title": "Perfil del proyecto", "content": format!("Factoría {i}."), "kind": "fact", "key": "kumi-perfil" });
            use std::io::Write;
            h.stdin.take().unwrap().write_all(p.to_string().as_bytes()).unwrap();
            h
        }
    };
    for round in 0..3 {
        let children: Vec<_> = (0..8).map(|i| launch(round * 8 + i, &repo)).collect();
        for h in children {
            let o = h.wait_with_output().unwrap();
            assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        }
    }
    let db = db::open(&db_path).unwrap();
    assert_eq!(with_key(&db, "kumi-perfil"), 1, "una sola memoria con la clave");
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0)).unwrap();
    assert_eq!(total, 1);
    let versions: i64 = db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get(0)).unwrap();
    assert_eq!(versions, 23, "24 escrituras: una crea y 23 actualizan");
    // And it is read by the key.
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .current_dir(&repo)
        .args(["get", "--key", "kumi-perfil"])
        .env("OMOIKANE_DB", &db_path)
        .env("OMOIKANE_HOME", dir.join("home-omoikane"))
        .env("OMOIKANE_MODELS", dir.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_SYNC_AUTO", "0")
        .env("OMOIKANE_LANG", "en")
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && s.contains("Perfil del proyecto") && s.contains("Factoría"), "{s}{}", String::from_utf8_lossy(&o.stderr));
    // `--data` and `get --key --resolve --json`, via the CLI: global with a trade and the project's.
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
            .current_dir(&repo)
            .args(args)
            .env("OMOIKANE_DB", &db_path)
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    run(&["trades", "add", "web"]);
    run(&["remember", "-k", "fact", "-t", "Ruta web", "--trade", "web", "--key", "kumi-ruta", "--data", r#"{"ruta":["web"]}"#]);
    run(&["remember", "-k", "fact", "-t", "Ruta de este repo", "--key", "kumi-ruta", "--data", r#"{"ruta":["web","seo-geo"],"fase":"alpha"}"#, "Sin cyber."]);
    let layers: serde_json::Value = serde_json::from_str(&run(&["--json", "get", "--key", "kumi-ruta", "--resolve"])).unwrap();
    let layers = layers.as_array().unwrap();
    assert_eq!(layers.iter().map(|c| c["scope"].as_str().unwrap()).collect::<Vec<_>>(), ["project", "trade:web"]);
    assert_eq!(layers[0]["data"], serde_json::json!({"fase": "alpha", "ruta": ["web", "seo-geo"]}));
    assert_eq!(layers[0]["text"], "Sin cyber.");
    assert_eq!(layers[1]["data"], serde_json::json!({"ruta": ["web"]}));
    // The data does not go into the text: neither in the memory nor in search.
    assert!(!layers[0]["text"].as_str().unwrap().contains("seo-geo"));
    let found = run(&["--json", "search", "seo-geo"]);
    assert!(!found.contains("Ruta de este repo"), "{found}");
    // Plain `get --key --json` also gives the data.
    let one: serde_json::Value = serde_json::from_str(&run(&["--json", "get", "--key", "kumi-ruta"])).unwrap();
    assert_eq!(one["data"]["fase"], "alpha");
}

/// A project's route through Kumi's megafactory: trade (the default route of the web
/// factory), client (after web, always seo-geo) and project (no SEO). `--resolve` gives them all, the most
/// specific first, each with its scope and its data, without merging them; another client does not see
/// client X's; and regenerating the same thing creates no versions.
#[test]
fn kumis_route_on_three_levels() {
    use omoikane::project;
    let (_dir, mut db) = base("niveles");
    let (disney, other_x, of_y) = ("/clientes/x/disney-countdown", "/clientes/x/tienda", "/clientes/y/web");
    let x = project::create_space(&db, "Cliente X").unwrap();
    let y = project::create_space(&db, "Cliente Y").unwrap();
    // All three are websites: the trade layer is only seen by projects of that trade.
    for (p, s) in [(disney, x), (other_x, x), (of_y, y)] {
        let id = project::register(&db, p).unwrap();
        project::assign_space(&db, id, Some(s)).unwrap();
        omoikane::trades::add(&db, id, &["web".to_string()]).unwrap();
    }
    let save = |db: &mut Connection, mut n: NewMemory, tags: &[&str], data: serde_json::Value| {
        n.tags = tags.iter().map(|t| t.to_string()).collect();
        memory::save_by_key(db, "kumi-ruta", n, Some(data), None).unwrap()
    };
    // Trade: a global one with `trade:web` (what `--trade web` saves).
    let trade = NewMemory { kind: Some(Kind::Fact), title: "Ruta de la factoría web".into(), content: "La ruta por defecto de una web.".into(), ..Default::default() };
    save(&mut db, trade.clone(), &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web", "cyber"]}));
    // Client X (its space) and client Y.
    let client = |s, route: &str| NewMemory { space_id: Some(s), kind: Some(Kind::Fact), title: format!("Ruta del cliente {route}"), content: "La ruta del cliente.".into(), ..Default::default() };
    save(&mut db, client(x, "seo-geo"), &[], serde_json::json!({"ruta": ["web", "seo-geo"]}));
    save(&mut db, client(y, "cyber"), &[], serde_json::json!({"ruta": ["web", "cyber"]}));
    // Project: disney-countdown without SEO. And a global one without a trade.
    let project_mem = NewMemory { project: disney.into(), kind: Some(Kind::Fact), title: "Ruta de disney-countdown".into(), content: "Sin SEO: es una cuenta atrás familiar.".into(), ..Default::default() };
    save(&mut db, project_mem, &[], serde_json::json!({"ruta": ["web"], "fase": "alpha"}));
    save(&mut db, NewMemory { kind: Some(Kind::Fact), title: "Ruta por defecto".into(), content: "Sin factoría.".into(), ..Default::default() }, &[], serde_json::json!({"ruta": []}));

    let layers = |p: &str| memory::key_layers(&db, &Scope::from_project(&db, p).unwrap(), "kumi-ruta").unwrap();
    let scopes = |p: &str| layers(p).into_iter().map(|c| c.scope).collect::<Vec<_>>();
    assert_eq!(scopes(disney), ["project", "space", "trade:web", "global"]);
    let c = layers(disney);
    assert_eq!(c[0].text, "Sin SEO: es una cuenta atrás familiar.");
    assert_eq!(c[0].data, Some(serde_json::json!({"fase": "alpha", "ruta": ["web"]})));
    assert_eq!(c[1].data, Some(serde_json::json!({"ruta": ["web", "seo-geo"]})));
    assert_eq!(c[1].title, "Ruta del cliente seo-geo");
    // Another project of client X: without disney's, with client X's.
    assert_eq!(scopes(other_x), ["space", "trade:web", "global"]);
    // A project of client Y never sees client X's.
    let of_y_layers = layers(of_y);
    assert_eq!(of_y_layers.iter().map(|c| c.scope.as_str()).collect::<Vec<_>>(), ["space", "trade:web", "global"]);
    assert_eq!(of_y_layers[0].data, Some(serde_json::json!({"ruta": ["web", "cyber"]})));
    // Plain `get --key`: the most specific one.
    assert_eq!(memory::by_key(&db, &Scope::from_project(&db, disney).unwrap(), "kumi-ruta").unwrap().unwrap().title, "Ruta de disney-countdown");

    // The trade and the global one without a trade are different scopes: one for each.
    assert_eq!(with_key(&db, "kumi-ruta"), 5);
    // Regenerating the trade one with the same thing (keys in another order) changes nothing; with other data
    // the data changes, with no version (like `update --data`); with another text, a version.
    let versions = |db: &Connection| db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get::<_, i64>(0)).unwrap();
    let before = versions(&db);
    let r = save(&mut db, trade.clone(), &["trade:web"], serde_json::json!({"tests": ["web", "cyber"], "ruta": ["web"]}));
    assert!(!r.created);
    let stamp = r.memory.updated_at.clone();
    assert_eq!(versions(&db), before, "lo mismo no deja versión");
    let r = save(&mut db, trade.clone(), &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    assert_eq!(versions(&db), before, "solo los datos no dejan versión");
    assert!(r.memory.updated_at >= stamp);
    assert_eq!(memory::data_of(&db, &[r.memory.id.as_str()]).unwrap()[&r.memory.id], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    let other = NewMemory { content: "Otra ruta por defecto.".into(), ..trade.clone() };
    save(&mut db, other, &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    assert_eq!(versions(&db), before + 1);
    assert_eq!(with_key(&db, "kumi-ruta"), 5);
    // `trade-for:client` is another level of the same trade: it does not overwrite the one for everyone.
    let r = save(&mut db, trade, &["trade:web", "trade-for:client"], serde_json::json!({"ruta": ["web", "seo-geo"]}));
    assert!(r.created);
    assert_eq!(with_key(&db, "kumi-ruta"), 6);
    let all = memory::key_layers(&db, &Scope::from_project(&db, disney).unwrap(), "kumi-ruta").unwrap();
    assert_eq!(all.iter().map(|c| c.scope.as_str()).collect::<Vec<_>>(), ["project", "space", "trade:web", "trade:web", "global"]);
}

/// The data of a keyed memory goes in `memory.extra` (like `--data` on any memory),
/// never in its content; saving the key without data keeps what it had, and anything that looks like a
/// secret is masked (the example one has a `~` in the prefix, as in tests/secrets.rs, so that
/// no scanner takes it for a real one).
#[test]
fn data_goes_apart_from_the_text() {
    let (_dir, mut db) = base("datos");
    let v = serde_json::json!({"b": 1, "a": {"d": [1, 2], "c": null}});
    let r = memory::save_by_key(&mut db, "perfil", new_memory("/p", "Perfil", "Texto."), Some(v.clone()), None).unwrap();
    assert_eq!(r.memory.content, "Texto.");
    assert_eq!(memory::data_of(&db, &[r.memory.id.as_str()]).unwrap()[&r.memory.id], v);
    let r = memory::save_by_key(&mut db, "perfil", new_memory("/p", "Perfil", "Texto nuevo."), None, None).unwrap();
    assert_eq!(r.memory.content, "Texto nuevo.");
    assert_eq!(memory::data_of(&db, &[r.memory.id.as_str()]).unwrap()[&r.memory.id], v);
    let r = memory::save_by_key(&mut db, "perfil", new_memory("/p", "Perfil", "Texto nuevo."), Some(serde_json::json!({"token": "export GH=gh~p_abcdefghijklmnopqrstuvwxyz0123456789".replace('~', "")})), None).unwrap();
    assert!(!r.redacted.is_empty());
    let d = memory::data_of(&db, &[r.memory.id.as_str()]).unwrap();
    assert!(!d[&r.memory.id]["token"].as_str().unwrap().contains("abcdefghijklmnop"));
}

/// 1.8.6: the same key in two databases; when importing one into the other, the losing one is superseded by
/// the winning one. If the person says «it's fine as is» (`compatible`), importing again does not mark it
/// superseded again: before, the pair was only skipped if its current verdict was `superseded`.
#[test]
fn importing_again_respects_its_fine_as_is() {
    let dir = common::TempDir::new("clave-descartada");
    let omoi = |base: &str, args: &[&str]| -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
            .args(args)
            .current_dir(&*dir)
            .env("OMOIKANE_DB", dir.join(base))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let id_of = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap()["id"].as_str().unwrap().to_string();
    let old = id_of(omoi("a.db", &["--json", "remember", "-k", "fact", "-t", "Ruta de la factoría", "--global", "--key", "ruta", "android, test"]));
    let newer = id_of(omoi("b.db", &["--json", "remember", "-k", "fact", "-t", "Ruta de la factoría", "--global", "--key", "ruta", "android, test, despliegue"]));
    let export = dir.join("a.jsonl");
    std::fs::write(&export, omoi("a.db", &["export", "--all"])).unwrap();
    let export = export.to_str().unwrap();
    let superseded = || {
        let rev: serde_json::Value = serde_json::from_str(&omoi("b.db", &["--json", "review"])).unwrap();
        rev.as_array().unwrap().iter().any(|a| a["kind"] == "superseded" && a["memory_id"] == old.as_str())
    };
    omoi("b.db", &["import", export]);
    assert!(superseded(), "la que pierde queda sustituida al importar");
    omoi("b.db", &["review", "--dismiss", &old, "--kind", "superseded", "--other", &newer]);
    assert!(!superseded());
    omoi("b.db", &["import", export]);
    assert!(!superseded(), "importar otra vez no vuelve a ponerla sustituida");
}
