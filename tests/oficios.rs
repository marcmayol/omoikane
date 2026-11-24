//! 1.8.0: trades (stacks). A trade memory (`trade: "android"`) is seen by every project of that
//! trade, of any client, and by no other; each project's trade comes from its files and is
//! corrected with `omoi trades`; and when saving one, anything that looks like a project's or a client's is flagged.
//! Sync is in `tests/sync.rs` (`las_memorias_de_oficio_no_pasan_por_un_cliente`).

mod comun;

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Harness {
    dir: comun::Temporal,
}

impl Harness {
    fn new(name: &str) -> Harness {
        let p = Harness { dir: comun::Temporal::nueva(name) };
        std::fs::create_dir_all(p.home()).unwrap();
        p
    }

    fn home(&self) -> PathBuf {
        self.dir.join("casa")
    }

    /// A project (a git repository, with the given `origin`) with these files.
    fn project(&self, name: &str, origin: Option<&str>, files: &[(&str, &str)]) -> PathBuf {
        let p = self.home().join(name);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        let config = match origin {
            Some(o) => format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {o}\n"),
            None => "[core]\n\tbare = false\n".to_string(),
        };
        std::fs::write(p.join(".git").join("config"), config).unwrap();
        for (f, text) in files {
            let r = p.join(f);
            std::fs::create_dir_all(r.parent().unwrap()).unwrap();
            std::fs::write(r, text).unwrap();
        }
        p
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        let home = self.home();
        c.current_dir(cwd)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_NO_MODEL", "1")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env_remove("OMOIKANE_LANG");
        c
    }

    fn omoi(&self, cwd: &Path, args: &[&str], input: &str) -> Output {
        let mut h = self.command(cwd).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        h.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        h.wait_with_output().unwrap()
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.omoi(cwd, args, "");
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    /// An MCP tool (`omoi tool`): its response and whether it was an error.
    fn tool(&self, cwd: &Path, name: &str, args: Value) -> (Value, bool) {
        let o = self.omoi(cwd, &["tool", name], &args.to_string());
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        (serde_json::from_str(&text).unwrap_or(Value::String(text)), !o.status.success())
    }

    fn tool_ok(&self, cwd: &Path, name: &str, args: Value) -> Value {
        let (v, error) = self.tool(cwd, name, args);
        assert!(!error, "{name}: {v}");
        v
    }

    fn ids(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect()
    }

    /// What the recall of a message injects.
    fn recall(&self, cwd: &Path, session: &str, prompt: &str) -> String {
        let o = self.omoi(cwd, &["recall", "--format", "plain"], &json!({ "session_id": session, "prompt": prompt }).to_string());
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn at_start(&self, cwd: &Path, session: &str) -> String {
        let o = self.omoi(cwd, &["recall", "--session-start", "--format", "plain"], &json!({ "session_id": session, "source": "startup" }).to_string());
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
}

const ANDROID: (&str, &str) = ("app/build.gradle.kts", "plugins { id(\"com.android.application\") }");
const WEB: (&str, &str) = ("package.json", r#"{"dependencies":{"astro":"5.0.0"}}"#);

/// The Android gotcha reaches the Android projects of any client (and those with no
/// client) and not a client's website: not in search, nor in `recent`, nor in `context`, nor in
/// recall, nor when asked for by its id. Correcting the trade by hand changes what is seen.
#[test]
fn an_android_gotcha_does_not_reach_a_clients_website() {
    let p = Harness::new("oficios-aislamiento");
    let home = p.home();
    p.ok(&home, &["space", "add", "acme"]);
    p.ok(&home, &["space", "add", "globex"]);
    let web = p.project("acme-web", None, &[WEB]);
    let app = p.project("globex-app", None, &[ANDROID]);
    let mine = p.project("mi-app", None, &[("build.gradle", "apply plugin: 'com.android.application'")]);
    p.ok(&web, &["space", "assign", "acme"]);
    p.ok(&app, &["space", "assign", "globex"]);
    // Each with its own, so there is something to see.
    p.tool_ok(&web, "remember", json!({ "title": "La portada de la web usa islas", "content": "Componentes interactivos solo en la cabecera.", "kind": "fact" }));
    // `context` looks at their files: each one's trade.
    assert_eq!(p.tool_ok(&web, "context", json!({}))["project"]["trades"], json!(["node", "web"]));
    assert_eq!(p.tool_ok(&app, "context", json!({}))["project"]["trades"], json!(["android"]));
    let r = p.tool_ok(
        &app,
        "remember",
        json!({ "title": "No matar el emulador con taskkill", "content": "taskkill /F deja colgado adb y el emulador no vuelve a arrancar: usar adb emu kill.", "kind": "gotcha", "trade": "android" }),
    );
    assert_eq!((r["saved"].as_bool(), r["project"].as_str()), (Some(true), Some("(trade android)")), "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // The acme client's website does not see it.
    let search = |cwd: &Path| Harness::ids(&p.tool_ok(cwd, "search", json!({ "query": "emulador taskkill adb" })));
    assert!(!search(&web).contains(&id));
    assert!(!Harness::ids(&p.tool_ok(&web, "recent", json!({}))).contains(&id));
    assert!(!Harness::ids(&p.tool_ok(&web, "search", json!({ "query": "emulador taskkill adb", "all_projects": true }))).contains(&id));
    let (g, error) = p.tool(&web, "get", json!({ "id": id }));
    assert!(error, "por su id tampoco: {g}");
    assert!(!p.recall(&web, "s-web", "el emulador se queda colgado si lo cierro con taskkill").contains("taskkill /F"));
    // An Android project of another client and one with no client do.
    for cwd in [&app, &mine] {
        assert!(search(cwd).contains(&id), "{}", cwd.display());
        assert!(Harness::ids(&p.tool_ok(cwd, "recent", json!({}))).contains(&id));
        assert_eq!(p.tool_ok(cwd, "get", json!({ "id": id }))["id"], id.as_str());
    }
    let found = &p.tool_ok(&mine, "search", json!({ "query": "emulador taskkill adb" }))[0];
    assert_eq!((found["trade"].clone(), found.get("global")), (json!(["android"]), None), "{found}");
    let injected = p.recall(&mine, "s-mia", "el emulador se queda colgado si lo cierro con taskkill");
    assert!(injected.contains("[trade android]") && injected.contains("No matar el emulador"), "{injected}");
    // From the user's folder, everything.
    assert!(search(&home).contains(&id));
    // At session start, the project states its trades.
    let start = p.at_start(&mine, "s-inicio");
    assert!(start.contains("This project's trades (its stack, from its files): android."), "{start}");
    // Correct it by hand: without Android, mi-app stops seeing it; with `auto`, it comes back.
    p.ok(&mine, &["trades", "remove", "android"]);
    assert!(!search(&mine).contains(&id));
    p.ok(&mine, &["trades", "auto"]);
    assert!(search(&mine).contains(&id));
    p.ok(&mine, &["trades", "set", "web"]);
    assert!(!search(&mine).contains(&id));
    let e: Value = serde_json::from_str(&p.ok(&mine, &["--json", "trades"])).unwrap();
    assert_eq!((e["trades"].clone(), e["fixed"].clone()), (json!(["web"]), json!(true)), "{e}");
    // If acme's website is also Android (a hybrid app), it sees it: it is stack knowledge.
    p.ok(&web, &["trades", "add", "android"]);
    assert!(search(&web).contains(&id));
    let list: Value = serde_json::from_str(&p.ok(&home, &["--json", "trades", "list", "--all"])).unwrap();
    assert_eq!(list["memories"]["android"], 1, "{list}");
    let projects = p.ok(&home, &["projects"]);
    assert!(projects.lines().any(|l| l.contains("acme-web") && l.contains("(android, node, web)")), "{projects}");
}

/// A trade memory that names a project, a client, a path, a repository or a client's
/// domain is not saved without `force` (over MCP, `remember` and `update`; on the CLI, with exit code 2).
#[test]
fn a_trade_memory_takes_nothing_from_a_client() {
    let p = Harness::new("oficios-fugas");
    let home = p.home();
    p.ok(&home, &["space", "add", "Globex"]);
    let app = p.project("globex-tienda", Some("git@github.com:globex/tienda.git"), &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    p.tool_ok(&app, "remember", json!({ "title": "El staging está en pruebas-rojo.es", "content": "https://pruebas-rojo.es/login con el usuario de QA.", "kind": "fact" }));
    let save = |title: &str, text: &str| p.tool_ok(&app, "remember", json!({ "title": title, "content": text, "kind": "gotcha", "trade": "android" }));
    let kinds = |v: &Value| -> Vec<String> { v["trade_leaks"].as_array().map(|a| a.iter().map(|f| f["type"].as_str().unwrap().to_string()).collect()).unwrap_or_default() };
    for (text, kind) in [
        ("En globex-tienda el emulador se cuelga con taskkill.", "project"),
        ("Lo vimos en el móvil de Globex: taskkill cuelga el emulador.", "space"),
        (r"El AVD vive en C:\Users\ana\.android\avd y taskkill lo deja corrupto.", "path"),
        ("Ver https://github.com/globex/tienda/issues/12: taskkill cuelga el emulador.", "repository"),
        ("Tras taskkill, el emulador no entra en pruebas-rojo.es hasta reiniciar adb.", "domain"),
    ] {
        let r = save("Taskkill cuelga el emulador", text);
        assert_eq!(r["saved"], false, "{text}: {r}");
        assert!(kinds(&r).contains(&kind.to_string()), "{text}: {r}");
    }
    // With nothing from anyone, it is saved; and one that really is a false positive, with allow_leaks.
    assert_eq!(save("Taskkill cuelga el emulador", "taskkill /F deja colgado adb: usar adb emu kill.")["saved"], true);
    let r = p.tool_ok(&app, "remember", json!({ "title": "Globex Gradle Plugin", "content": "La versión 2 rompe el emulador.", "kind": "gotcha", "trade": "android", "allow_leaks": true }));
    assert_eq!(r["saved"], true, "{r}");
    // On the CLI: exit code 2 and what it found.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador y taskkill", "--trade", "android", "En globex-tienda pasa siempre."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(String::from_utf8_lossy(&o.stdout).contains("globex-tienda"));
    // `update`: a project memory that becomes a trade memory is checked in full.
    let project_one = p.tool_ok(&app, "remember", json!({ "title": "Globex usa el emulador Pixel 8", "content": "Con la API 35.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": project_one, "trade": "android" }));
    assert_eq!(r["updated"], false, "{r}");
    let clean = p.tool_ok(&app, "remember", json!({ "title": "El emulador necesita la API 35", "content": "Con la 34 no arranca la cámara.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": clean, "trade": "android" }));
    assert_eq!(r["trade"], json!(["android"]), "{r}");
    let clean = r["id"].as_str().unwrap().to_string();
    // What is new in a trade memory also goes through the warning.
    let r = p.tool_ok(&app, "update", json!({ "id": clean, "append": "En globex-tienda, además, la 33." }));
    assert_eq!(r["updated"], false, "{r}");
    // Moving it to a project removes its trade.
    let r = p.tool_ok(&app, "update", json!({ "id": clean, "project": app.to_string_lossy() }));
    assert!(r.get("trade").is_none() && !r["tags"].to_string().contains("trade:"), "{r}");
}

/// Trade preferences are injected at session start in every project of that trade (along with the
/// global ones), and `for` restricts them to client projects (in a space) or personal ones.
#[test]
fn trade_preferences_for_client_or_personal() {
    let p = Harness::new("oficios-preferencias");
    let home = p.home();
    p.ok(&home, &["space", "add", "acme"]);
    let client = p.project("acme-landing", None, &[WEB]);
    let personal = p.project("cuenta-atras", None, &[("index.html", "<html></html>")]);
    let app = p.project("mi-app", None, &[ANDROID]);
    p.ok(&client, &["space", "assign", "acme"]);
    let seo = p.tool_ok(&client, "remember", json!({ "title": "Revisar GEO y SEO de la landing", "content": "En cada web para un cliente, antes de entregar: títulos, descripciones, datos estructurados y llms.txt.", "kind": "preference", "trade": "web", "for": "client" }));
    assert_eq!(seo["project"], "(trade web, client projects only)", "{seo}");
    let seo = seo["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "remember", json!({ "title": "Pantallas con Scaffold y su padding", "content": "Cada pantalla nueva va dentro de un Scaffold y usa su innerPadding.", "kind": "preference", "trade": "android" }));
    assert_eq!(r["saved"], true, "{r}");
    // At start, in the preferences block: the web one only on the client's website; the Android one, in the app.
    let (_, client_prefs) = p.at_start(&client, "s1").split_once("preferences and the pinned memories").map(|(a, b)| (a.to_string(), b.to_string())).expect("bloque");
    assert!(client_prefs.contains("Revisar GEO y SEO") && client_prefs.contains("[trade web, client projects]"), "{client_prefs}");
    assert!(!client_prefs.contains("Scaffold"), "{client_prefs}");
    let personal_start = p.at_start(&personal, "s2");
    assert!(!personal_start.contains("GEO y SEO"), "{personal_start}");
    let app_start = p.at_start(&app, "s3");
    assert!(app_start.contains("Pantallas con Scaffold") && !app_start.contains("GEO y SEO"), "{app_start}");
    // Neither in search nor by its id, from the personal website.
    assert!(!Harness::ids(&p.tool_ok(&personal, "search", json!({ "query": "GEO SEO landing" }))).contains(&seo));
    assert!(p.tool(&personal, "get", json!({ "id": seo })).1);
    let r = &p.tool_ok(&client, "search", json!({ "query": "GEO SEO landing" }))[0];
    assert_eq!((r["id"].as_str(), r["for"].as_str()), (Some(seo.as_str()), Some("client")), "{r}");
    // For both: now the personal one too. And a personal-only one does not reach the client.
    p.tool_ok(&client, "update", json!({ "id": seo, "for": "both" }));
    assert!(p.at_start(&personal, "s4").contains("GEO y SEO"));
    let only_mine = p.ok(&personal, &["--json", "remember", "-k", "preference", "-t", "Sin cookies de terceros", "--trade", "web", "--for", "personal", "Mis webs no cargan nada de terceros."]);
    let only_mine = serde_json::from_str::<Value>(&only_mine).unwrap()["id"].as_str().unwrap().to_string();
    assert!(p.at_start(&personal, "s5").contains("Sin cookies de terceros"));
    assert!(!p.at_start(&client, "s6").contains("Sin cookies de terceros"));
    assert!(p.tool(&client, "get", json!({ "id": only_mine })).1);
}

/// How many verdicts a memory has (on either side).
fn verdicts(p: &Harness, id: &str) -> i64 {
    rusqlite::Connection::open(p.dir.join("o.db")).unwrap().query_row("SELECT count(*) FROM verdict WHERE memory_id = ?1 OR other_id = ?1", [id], |f| f.get(0)).unwrap()
}

/// The leak types of a response.
fn kinds(v: &Value) -> Vec<String> {
    v["trade_leaks"].as_array().map(|a| a.iter().map(|f| f["type"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// Two clients with one Android project each: `acme-app` in acme and `beta-app` in beta.
fn two_clients(p: &Harness) -> (PathBuf, PathBuf) {
    let home = p.home();
    p.ok(&home, &["space", "add", "acme"]);
    p.ok(&home, &["space", "add", "beta"]);
    let app = p.project("acme-app", None, &[ANDROID]);
    let beta = p.project("beta-app", None, &[ANDROID]);
    p.ok(&app, &["space", "assign", "acme"]);
    p.ok(&beta, &["space", "assign", "beta"]);
    (app, beta)
}

/// Promoting a client's memory to a trade creates a NEW trade memory, with no history or
/// verdicts (they belonged to the client), and the client's one stays where it was; from another client, neither the
/// history, nor the original, nor the notes of its verdicts. A personal one is promoted with its id and its
/// history. And the history a previous version left "only here" only shows from the general folder.
#[test]
fn promoting_a_clients_memory_creates_a_new_one() {
    let p = Harness::new("oficios-promover");
    let (app, beta) = two_clients(&p);
    let id = p.tool_ok(&app, "remember", json!({ "title": "En acme-app el emulador se cuelga", "content": "Pasa al cerrarlo con taskkill.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": id, "append": "Con adb emu kill no pasa.", "reason": "probado" }));
    p.tool_ok(&app, "remember", json!({ "title": "Emulador colgado tras taskkill en acme", "content": "Reiniciar adb.", "kind": "gotcha", "supersedes": [id] }));
    assert_eq!(verdicts(&p, &id), 1);
    // As is, it names the client's project: no.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android" }));
    assert_eq!(r["updated"], false, "{r}");
    // Rewritten: a new memory, with no history or verdicts.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android", "title": "El emulador se cuelga al cerrarlo con taskkill", "content": "Cerrarlo con adb emu kill.", "reason": "es del oficio" }));
    let new_id = r["id"].as_str().unwrap().to_string();
    assert!(new_id != id && r["promoted_from"] == id.as_str() && r["trade"] == json!(["android"]), "{r}");
    assert_eq!(verdicts(&p, &new_id), 0);
    assert_eq!(verdicts(&p, &id), 1);
    // The client's one, untouched in its project.
    let o = p.tool_ok(&app, "get", json!({ "id": id }));
    assert!(o["title"] == "En acme-app el emulador se cuelga" && !o["tags"].to_string().contains("trade:") && o["project"].as_str().unwrap().ends_with("acme-app"), "{o}");
    // From beta: the new one, with no history; the original, not even its history.
    assert_eq!(p.tool_ok(&beta, "history", json!({ "id": new_id })), json!([]));
    assert!(p.tool(&beta, "history", json!({ "id": id })).1);
    assert_eq!(p.ok(&beta, &["--json", "history", &new_id]).trim(), "[]");
    assert!(!p.omoi(&beta, &["--json", "history", &id], "").status.success(), "por la CLI tampoco");
    let ids = Harness::ids(&p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" })));
    assert!(ids.contains(&new_id) && !ids.contains(&id), "{ids:?}");
    // A verdict from acme on the trade memory: from beta, the note says nothing about acme.
    p.tool_ok(&app, "remember", json!({ "title": "En acme-app el AVD se cierra con su botón", "content": "Así lo pide Acme.", "kind": "gotcha", "supersedes": [new_id] }));
    let r = p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == new_id.as_str()).expect("la de oficio").clone();
    assert!(m["note"].as_str().is_some_and(|n| n.contains("not in this scope")) && !m.to_string().to_lowercase().contains("acme"), "{m}");
    let t = p.recall(&beta, "s1", "el emulador se cuelga al cerrarlo con taskkill");
    assert!(t.contains("Cerrarlo con adb emu kill") && !t.to_lowercase().contains("acme"), "{t}");
    // From acme, the note does name it.
    let r = p.tool_ok(&app, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == new_id.as_str()).expect("la de oficio").clone();
    assert!(m["note"].as_str().is_some_and(|n| n.contains("AVD se cierra")), "{m}");
    // On the CLI, the same: `--trade` with the client's text exits with code 2; rewritten, another memory.
    let id2 = p.tool_ok(&app, "remember", json!({ "title": "Acme firma con su keystore", "content": "Gradle lo lee de keystore.properties.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let o = p.omoi(&app, &["update", &id2, "--trade", "android"], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let r: Value = serde_json::from_str(&p.ok(&app, &["--json", "update", &id2, "--trade", "android", "-t", "La firma se lee de keystore.properties", "-c", "Gradle lo lee de keystore.properties."])).unwrap();
    let new_id2 = r["id"].as_str().unwrap().to_string();
    assert!(new_id2 != id2 && r["promoted_from"] == id2.as_str(), "{r}");
    assert!(Harness::ids(&p.tool_ok(&beta, "search", json!({ "query": "keystore properties firma" }))).contains(&new_id2));
    assert!(p.tool_ok(&app, "get", json!({ "id": id2 }))["title"] == "Acme firma con su keystore");
    // A personal one (a project with no space): the same id and its history.
    let mine = p.project("mi-app", None, &[ANDROID]);
    let pid = p.tool_ok(&mine, "remember", json!({ "title": "Gradle 9 rompe el plugin de Android 8.5", "content": "Usar la 8.7.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&mine, "update", json!({ "id": pid, "append": "O la 8.8.", "reason": "probado" }));
    let r = p.tool_ok(&mine, "update", json!({ "id": pid, "trade": "android" }));
    assert!(r["id"] == pid.as_str() && r.get("promoted_from").is_none(), "{r}");
    assert!(p.tool_ok(&beta, "history", json!({ "id": pid })).as_array().is_some_and(|v| !v.is_empty()), "su historial viene con ella");
    // What a previous version promoted with the same id from a client (`historial_local`): its
    // history does not show from a project, but does from the general folder.
    rusqlite::Connection::open(p.dir.join("o.db")).unwrap().execute("INSERT INTO meta(k, v) VALUES ('historial_local:' || ?1, '1')", [&pid]).unwrap();
    assert_eq!(p.tool_ok(&beta, "history", json!({ "id": pid })), json!([]));
    assert!(p.tool_ok(&p.home(), "history", json!({ "id": pid })).as_array().is_some_and(|v| !v.is_empty()));
}

/// Leak checks do not look only at the title and the content: also the tags (except the reserved ones), the
/// key and the data, when saving, when changing a trade memory (even if only those change) and when promoting. And
/// they compare without accents ("balua" is Balùa) and with 2-letter space names.
#[test]
fn leak_checks_look_at_tags_key_and_data() {
    let p = Harness::new("oficios-fugas-extra");
    let home = p.home();
    p.ok(&home, &["space", "add", "Globex"]);
    p.ok(&home, &["space", "add", "QA"]);
    let app = p.project("globex-tienda", None, &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    let balua = p.project("Balùa", None, &[ANDROID]);
    p.tool_ok(&balua, "remember", json!({ "title": "Nota", "content": "Una nota.", "kind": "fact" }));
    let base = |extra: Value| {
        let mut v = json!({ "title": "Taskkill cuelga el emulador", "content": "Usar adb emu kill.", "kind": "gotcha", "trade": "android" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        p.tool_ok(&app, "remember", v)
    };
    // Each leak says which field it is in (a key that names a client is fixed by renaming it).
    for (extra, kind, field) in [
        (json!({ "tags": ["globex-tienda"] }), "project", "tag"),
        (json!({ "key": "globex-perfil" }), "space", "key"),
        (json!({ "data": { "cliente": "Globex" } }), "space", "data"),
        (json!({ "data": { "globex-tienda": 1 } }), "project", "data"),
        (json!({ "content": "En balua pasa siempre." }), "project", "content"),
        (json!({ "content": "Lo vimos en QA ayer." }), "space", "content"),
    ] {
        let r = base(extra.clone());
        assert!(r["saved"] == false && kinds(&r).contains(&kind.to_string()), "{extra}: {r}");
        let fields: Vec<&str> = r["trade_leaks"].as_array().unwrap().iter().filter(|f| f["type"] == kind).filter_map(|f| f["field"].as_str()).collect();
        assert!(fields.contains(&field), "{extra}: se esperaba el campo {field}: {r}");
    }
    // Reserved ones do not count.
    let r = base(json!({ "tags": ["review:2026-12-01", "status", "blocking", "open"], "review_reason": "si el SDK sigue en beta" }));
    assert_eq!(r["saved"], true, "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // `update` of a trade memory that only touches the tags or the data.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "tags": ["globex"] }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": id, "data": { "visto": "en globex-tienda" } }));
    assert_eq!(r["updated"], false, "{r}");
    let o = p.omoi(&app, &["update", &id, "--data", r#"{"cliente":"Globex"}"#], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    // On the CLI, a tag.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador y taskkill", "--trade", "android", "--tag", "globex-tienda", "Usar adb."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    // When promoting: the tag and the data of the project memory count.
    let project_one = p.tool_ok(&app, "remember", json!({ "title": "El emulador necesita la API 35", "content": "Con la 34 no arranca la cámara.", "kind": "fact", "tags": ["globex-tienda"] }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": project_one, "trade": "android" }));
    assert!(r["updated"] == false && kinds(&r).contains(&"project".to_string()), "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": project_one, "trade": "android", "tags": [], "data": { "app": "Globex" } }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": project_one, "trade": "android", "tags": [] }));
    assert_eq!(r["trade"], json!(["android"]), "{r}");
}

/// Resolving a trade decision runs the answer through the leak check (over MCP and the CLI), and a
/// decision already resolved is not resolved again.
#[test]
fn resolving_a_trade_decision_goes_through_leak_check() {
    let p = Harness::new("oficios-decision");
    let (app, beta) = two_clients(&p);
    let id = p.tool_ok(&app, "remember", json!({ "title": "¿Cerrar el emulador con taskkill o con adb?", "content": "Asumo adb emu kill.", "kind": "decision", "trade": "android", "tags": ["open"] }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&beta, "decisions", json!({ "resolve": id, "answer": "En acme-app con adb." }));
    assert!(r["resolved"] == false && kinds(&r).contains(&"project".to_string()), "{r}");
    let o = p.omoi(&beta, &["decisions", "--resolve", &id, "--answer", "Como en acme."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(p.tool_ok(&beta, "get", json!({ "id": id }))["tags"].to_string().contains("open"), "nada se guardó");
    let r = p.tool_ok(&beta, "decisions", json!({ "resolve": id, "answer": "Siempre con adb emu kill." }));
    assert_eq!(r["resolved"], true, "{r}");
    // Not again.
    let (r, error) = p.tool(&beta, "decisions", json!({ "resolve": id, "answer": "Con adb." }));
    assert!(error && r.to_string().contains("already resolved"), "{r}");
    let o = p.omoi(&beta, &["decisions", "--resolve", &id, "--answer", "Con adb."], "");
    assert!(!o.status.success());
}

/// `force` only skips duplicates: leaks need `allow_leaks` (`--allow-leaks`), which
/// never lets through an absolute path or a repository URL.
#[test]
fn force_does_not_skip_leaks() {
    let p = Harness::new("oficios-force");
    let home = p.home();
    p.ok(&home, &["space", "add", "Globex"]);
    let app = p.project("globex-tienda", Some("git@github.com:globex/tienda.git"), &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    let save = |text: &str, extra: Value| {
        let mut v = json!({ "title": "Taskkill cuelga el emulador", "content": text, "kind": "gotcha", "trade": "android" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        p.tool_ok(&app, "remember", v)
    };
    let r = save("En globex-tienda pasa siempre.", json!({ "force": true }));
    assert_eq!(r["saved"], false, "force no salta las fugas: {r}");
    let r = save("El plugin globex-tienda de Gradle lo rompe.", json!({ "allow_leaks": true }));
    assert_eq!(r["saved"], true, "{r}");
    for text in [r"El AVD vive en C:\Users\ana\.android\avd.", "Ver https://github.com/globex/tienda/issues/1."] {
        let r = save(text, json!({ "allow_leaks": true, "force": true }));
        assert_eq!(r["saved"], false, "{text}: {r}");
    }
    // On the CLI.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador", "--trade", "android", "--force", "En globex-tienda pasa."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador", "--trade", "android", "--allow-leaks", r"Está en D:\clientes\avd."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    p.ok(&app, &["remember", "-k", "gotcha", "-t", "Emulador y el plugin", "--trade", "android", "--allow-leaks", "El plugin globex-tienda de Gradle."]);
    // `update`: `force` no longer works for this.
    let id = save("Usar adb emu kill.", json!({}))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": id, "append": "En globex-tienda también.", "force": true }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": id, "append": "Con el plugin globex-tienda también.", "allow_leaks": true }));
    assert!(r.get("updated").is_none(), "{r}");
}

/// A malformed `review:` tag that arrived through sync (from another version) does not prevent
/// changing the memory (`update`, `remember --key`, `decisions --resolve`): only new ones are checked.
#[test]
fn a_malformed_incoming_date_does_not_block() {
    let p = Harness::new("oficios-fecha-vieja");
    let app = p.project("mi-app", None, &[ANDROID]);
    let id = p.tool_ok(&app, "remember", json!({ "title": "¿SQLite o Room?", "content": "Asumo Room.", "kind": "decision", "tags": ["open"], "key": "almacen" }))["id"].as_str().unwrap().to_string();
    // As if it arrived like this from another machine.
    rusqlite::Connection::open(p.dir.join("o.db"))
        .unwrap()
        .execute("UPDATE memory SET tags = '[\"open\",\"review:pronto\",\"topic:almacen\"]' WHERE id = ?1", [&id])
        .unwrap();
    p.ok(&app, &["update", &id, "--tag", "open", "--tag", "review:pronto", "--tag", "topic:almacen", "--tag", "datos"]);
    let o = p.omoi(&app, &["update", &id, "--tag", "review:luego"], "");
    assert!(!o.status.success(), "una nueva mal escrita sí se rechaza");
    p.ok(&app, &["remember", "-k", "decision", "-t", "¿SQLite o Room?", "--key", "almacen", "--tag", "open", "Asumo Room, con migraciones."]);
    p.ok(&app, &["decisions", "--resolve", &id, "--answer", "Room."]);
    assert!(p.tool_ok(&app, "get", json!({ "id": id }))["tags"].to_string().contains("resolved"));
}

/// Verdict notes (in `search` and in recall) do not name another client's memory:
/// a trade memory seen by acme and beta, superseded by one from acme, shows in beta without its title.
#[test]
fn notes_do_not_name_another_client() {
    let p = Harness::new("oficios-notas");
    let (app, beta) = two_clients(&p);
    let t = p.tool_ok(&beta, "remember", json!({ "title": "El emulador se cuelga con taskkill", "content": "Cerrarlo con adb emu kill.", "kind": "gotcha", "trade": "android" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "remember", json!({ "title": "En acme-app el AVD se cierra con su botón", "content": "Así lo pide Acme.", "kind": "gotcha", "supersedes": [t] }));
    let r = p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == t.as_str()).expect("la de oficio").clone();
    assert!(!m.to_string().to_lowercase().contains("acme"), "{m}");
    let text = p.recall(&beta, "s1", "el emulador se cuelga al cerrarlo con taskkill");
    assert!(text.contains("adb emu kill") && !text.to_lowercase().contains("acme"), "{text}");
}

/// Recall does not swap ordinary words for project names: "faltas" is not Faltis, "rodeo" is not
/// Rodet, "balsa" is not Balùa, nor are "tropa" or "trobar" Troba (see `nombres::misspelled`).
#[test]
fn recall_does_not_swap_ordinary_words_for_projects() {
    let p = Harness::new("oficios-recall-nombres");
    let home = p.home();
    for (name, title, text) in [
        ("Faltis", "Faltis compara tickets del súper", "Lee el ticket y compara precios entre tiendas."),
        ("Rodet", "Rodet pesa las bobinas", "Calcula el filamento que queda en cada bobina."),
        ("Balùa", "Balùa cuenta calorías", "Saca las calorías de la foto de un plato."),
        ("Troba", "Troba indexa el disco", "Busca en los ficheros sin red."),
    ] {
        let d = p.project(name, None, &[]);
        p.tool_ok(&d, "remember", json!({ "title": title, "content": text, "kind": "fact" }));
    }
    // The trace shows whether recall swapped any word for a name (`TRAZA nombres`).
    let names = |prompt: &str| -> String {
        let mut h = p
            .command(&home)
            .args(["recall", "--format", "plain"])
            .env("OMOIKANE_TRAZA", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        h.stdin.take().unwrap().write_all(json!({ "session_id": prompt, "prompt": prompt }).to_string().as_bytes()).unwrap();
        let o = h.wait_with_output().unwrap();
        String::from_utf8_lossy(&o.stderr).lines().filter(|l| l.starts_with("TRAZA nombres")).collect::<Vec<_>>().join(" | ")
    };
    for prompt in ["tengo muchas faltas de ortografía en el informe", "el rodeo de la feria empieza a las seis", "alquilamos una balsa para bajar el río", "la tropa del cole sale de excursión", "no consigo trobar las llaves del coche"] {
        assert_eq!(names(prompt), "", "{prompt}");
    }
    // A name that really was misdictated does.
    assert!(names("en faltiz se cuelga la lista").contains("faltis"));
}

/// Absorbing a client's memory into a trade memory would carry its history, its tags and its
/// paths to every client: not allowed. Into one from the same client, it is.
#[test]
fn a_clients_memory_is_not_absorbed_into_a_trade_one() {
    let p = Harness::new("oficios-absorber");
    let (app, _beta) = two_clients(&p);
    let t = p.tool_ok(&app, "remember", json!({ "title": "El emulador se cuelga con taskkill", "content": "Cerrarlo con adb emu kill.", "kind": "gotcha", "trade": "android" }))["id"].as_str().unwrap().to_string();
    let a = p.tool_ok(&app, "remember", json!({ "title": "En acme-app el emulador se cuelga", "content": "Lo vimos en el portátil de Acme.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": a, "append": "Y en el de Ana.", "reason": "visto" }));
    let (r, error) = p.tool(&app, "update", json!({ "id": t, "content": "Cerrarlo con adb emu kill; taskkill lo cuelga.", "absorb": [a] }));
    assert!(error && r.to_string().contains("client"), "{r}");
    assert!(p.tool_ok(&app, "get", json!({ "id": a }))["title"] == "En acme-app el emulador se cuelga", "sigue en su sitio");
    // Into one from the same client, it is.
    let b = p.tool_ok(&app, "remember", json!({ "title": "El emulador de acme-app necesita la API 35", "content": "Con la 34 no arranca.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": b, "content": "Con la 34 no arranca y taskkill lo cuelga.", "absorb": [a] }));
}
