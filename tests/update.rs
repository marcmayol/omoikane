//! 0.19.0: correcting with care. Partial edits (`replace` exactly once, `append`) and merging
//! N memories into one (`absorb`), through the `update` tool and through the CLI.

mod common;

use std::process::{Command, Output, Stdio};

struct Env {
    dir: common::TempDir,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = common::TempDir::new(&format!("update-{name}"));
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        Env { dir }
    }

    fn omoi(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_EQUIPO", "pc-pruebas")
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

    fn ok(&self, args: &[&str]) -> String {
        let o = self.omoi(args, None);
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    fn id(&self, kind: &str, title: &str, text: &str, extra: &[&str]) -> String {
        let mut a = vec!["remember", "-k", kind, "-t", title];
        a.extend_from_slice(extra);
        a.push(text);
        self.json(&a)["id"].as_str().unwrap().to_string()
    }

    fn get(&self, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi(&["--json", "get", id], None);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    /// `omoi tool update` with the arguments in JSON: (whether it succeeded, what it answered).
    fn update(&self, args: serde_json::Value) -> (bool, String) {
        let o = self.omoi(&["tool", "update"], Some(&args.to_string()));
        (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn update_ok(&self, args: serde_json::Value) -> serde_json::Value {
        let (success, text) = self.update(args);
        assert!(success, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    fn history(&self, id: &str) -> Vec<serde_json::Value> {
        self.json(&["history", id]).as_array().unwrap().clone()
    }

    fn broken(&self) -> Vec<String> {
        self.json(&["review", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == "broken_link")
            .map(|a| a["memory_id"].as_str().unwrap().to_string())
            .collect()
    }
}

const RECIPE: &str = "1. git pull\n2. cargo build\n3. cargo test\n4. deploy";

#[test]
fn replace_changes_the_piece_that_matches_once_and_shows_the_context() {
    let e = Env::new("replace");
    let id = e.id("procedure", "Publicar", RECIPE, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id, "replace": [{ "find": "2. cargo build", "with": "2. cargo build --release" }], "reason": "va en release" }));
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull\n2. cargo build --release\n3. cargo test\n4. deploy");
    let changed = &r["changed"][0];
    assert_eq!(changed["find"], "2. cargo build");
    let context = changed["context"].as_str().unwrap();
    assert!(context.contains("1. git pull\n2. cargo build --release\n3. cargo test"), "{context}");
    // A single previous version, with the reason.
    let h = e.history(&id);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["content"], RECIPE);
    assert_eq!(h[0]["reason"], "va en release");
    // The context is cut to 80 characters per side, with «…».
    let long = format!("{}\nMARCA\n{}", "a".repeat(200), "b".repeat(200));
    let id2 = e.id("fact", "Larga", &long, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id2, "replace": [{ "find": "MARCA", "with": "SEÑAL" }] }));
    let context = r["changed"][0]["context"].as_str().unwrap();
    assert_eq!(context, format!("…{}\nSEÑAL\n{}…", "a".repeat(79), "b".repeat(79)));
}

#[test]
fn replace_matching_zero_or_two_times_changes_nothing() {
    let e = Env::new("replace-mal");
    let id = e.id("procedure", "Publicar", RECIPE, &[]);
    let (success, text) = e.update(serde_json::json!({ "id": id, "replace": [{ "find": "npm install", "with": "pnpm install" }] }));
    assert!(!success);
    assert!(text.contains("appears 0 times"), "{text}");
    // Twice: an error with the count, and nothing changes even if there were other fields.
    let (success, text) = e.update(serde_json::json!({ "id": id, "title": "Otro título", "replace": [{ "find": "cargo", "with": "CARGO" }] }));
    assert!(!success);
    assert!(text.contains("appears 2 times"), "{text}");
    let m = e.get(&id).unwrap();
    assert_eq!(m["content"], RECIPE);
    assert_eq!(m["title"], "Publicar");
    assert!(e.history(&id).is_empty(), "sin versión fantasma");
    // Two replacements: if the second fails, the first doesn't go in either.
    let (success, _) = e.update(serde_json::json!({ "id": id, "replace": [{ "find": "4. deploy", "with": "4. desplegar" }, { "find": "cargo", "with": "x" }] }));
    assert!(!success);
    assert_eq!(e.get(&id).unwrap()["content"], RECIPE);
}

#[test]
fn append_adds_at_the_end_after_a_blank_line() {
    let e = Env::new("append");
    let id = e.id("procedure", "Publicar", RECIPE, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id, "append": "5. anunciarlo", "reason": "un paso más" }));
    assert_eq!(e.get(&id).unwrap()["content"], format!("{RECIPE}\n\n5. anunciarlo"));
    assert_eq!(r["changed"][0]["appended"], true);
    assert!(r["changed"][0]["context"].as_str().unwrap().ends_with("4. deploy\n\n5. anunciarlo"));
    // With replace at the same time: a single previous version.
    e.update_ok(serde_json::json!({ "id": id, "replace": [{ "find": "1. git pull", "with": "1. git pull --rebase" }], "append": "6. descansar" }));
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull --rebase\n2. cargo build\n3. cargo test\n4. deploy\n\n5. anunciarlo\n\n6. descansar");
    assert_eq!(e.history(&id).len(), 2);
}

#[test]
fn the_cli_also_replaces_and_appends() {
    let e = Env::new("cli");
    let id = e.id("procedure", "Publicar", RECIPE, &[]);
    let out = e.ok(&["update", &id, "--replace", "3. cargo test", "--with", "3. cargo test --all", "--append", "5. tag", "-r", "cli"]);
    assert!(out.contains("changed:"), "{out}");
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull\n2. cargo build\n3. cargo test --all\n4. deploy\n\n5. tag");
    // A --replace without its --with is an error, and the content isn't touched.
    let o = e.omoi(&["update", &id, "--replace", "4. deploy"], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--with"));
    let o = e.omoi(&["update", &id, "--replace", "nada", "--with", "algo"], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("appears 0 times"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn absorb_merges_tags_paths_history_and_links_and_trashes_the_old_one() {
    let e = Env::new("absorb");
    let old = e.id("fact", "El token caduca en UTC", "El middleware compara la hora local con la caducidad UTC.", &["--tag", "auth-token", "--path", "src/auth.rs"]);
    e.ok(&["update", &old, "-c", "El middleware compara la hora local con la caducidad UTC del token.", "-r", "más claro"]);
    let kept = e.id("fact", "Autenticación", "Las sesiones van por cookie firmada.", &["--tag", "auth", "--path", "src/session.rs"]);
    e.ok(&["update", &kept, "-c", "Las sesiones van por cookie firmada; la cookie dura 7 días.", "-r", "duración"]);
    e.id("gotcha", "Antes de tocar la auth", "Mira primero [[auth-token]].", &[]);
    assert!(e.broken().is_empty(), "el enlace resuelve a la vieja");
    // No merged text, no merge: Omoikane doesn't write it.
    let (success, text) = e.update(serde_json::json!({ "id": kept, "absorb": [&old[..10]] }));
    assert!(!success);
    assert!(text.contains("merged text"), "{text}");
    assert!(e.get(&old).is_some());
    let merged = "Las sesiones van por cookie firmada; la cookie dura 7 días.\n\nEl middleware compara la hora local con la caducidad UTC del token.";
    let r = e.update_ok(serde_json::json!({ "id": kept, "content": merged, "absorb": [&old[..10]], "reason": "juntadas" }));
    assert_eq!(r["absorbed"], serde_json::json!([old]));
    let m = e.get(&kept).unwrap();
    assert_eq!(m["content"], merged);
    assert_eq!(m["tags"], serde_json::json!(["auth", "auth-token"]));
    assert_eq!(m["paths"], serde_json::json!(["src/auth.rs", "src/session.rs"]));
    // History: its own version (reason «juntadas») and the old one's two, numbered after it.
    let h = e.history(&kept);
    let summary: Vec<(i64, String, String)> = h.iter().map(|v| (v["version"].as_i64().unwrap(), v["content"].as_str().unwrap().to_string(), v["reason"].as_str().unwrap_or("").to_string())).collect();
    assert_eq!(summary, [
        (4, "El middleware compara la hora local con la caducidad UTC del token.".to_string(), format!("absorbed from {old}")),
        (3, "El middleware compara la hora local con la caducidad UTC.".to_string(), format!("absorbed from {old}")),
        (2, "Las sesiones van por cookie firmada; la cookie dura 7 días.".to_string(), "juntadas".to_string()),
        (1, "Las sesiones van por cookie firmada.".to_string(), "duración".to_string()),
    ]);
    // The old one is in the trash, and the third one's link still resolves (now to the new one).
    assert!(e.get(&old).is_none());
    let trash = e.json(&["trash"]);
    assert_eq!(trash[0]["id"], old.as_str());
    assert!(e.broken().is_empty(), "[[auth-token]] lleva a la que queda");
    // Control: a link to a tag that really disappears does show up broken.
    let loose = e.id("fact", "Suelta", "Nada.", &["--tag", "suelta"]);
    let linker = e.id("fact", "Enlaza a la suelta", "Ver [[suelta]].", &[]);
    e.ok(&["forget", &loose, "--yes"]);
    assert_eq!(e.broken(), std::slice::from_ref(&linker));
    // The verdict: if someone restores the old one, it shows as superseded by the one that remains.
    e.ok(&["restore", &old]);
    let o = e.omoi(&["tool", "search"], Some("{\"query\":\"caducidad UTC\",\"all_projects\":true}"));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let note = v.as_array().unwrap().iter().find(|x| x["id"] == old.as_str()).expect("vuelve")["note"].as_str().unwrap_or("").to_string();
    assert!(note.starts_with("superseded by"), "{note}");
}
