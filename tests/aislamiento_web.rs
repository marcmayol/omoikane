//! 1.7.0: the web view (`omoi serve`) respects space isolation like the CLI and the MCP. What it
//! shows is decided by the folder it started in: from a client's project, only that client (and the
//! global ones); from a project without a space, only what has no space; from the user's folder,
//! everything. Nor can it write outside, and `?dir=` only narrows.

mod comun;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fixture {
    dir: comun::Temporal,
}

impl Fixture {
    fn home(&self) -> PathBuf {
        self.dir.join("casa")
    }

    fn project(&self, name: &str) -> PathBuf {
        self.home().join(name)
    }

    fn command(&self, folder: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        let home = self.home();
        c.current_dir(folder)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env_remove("OMOIKANE_LANG");
        c
    }

    fn ok(&self, folder: &Path, args: &[&str]) {
        let o = self.command(folder).args(args).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }

    fn id(&self, title: &str) -> String {
        self.db().query_row("SELECT id FROM memory WHERE title = ?1", [title], |f| f.get(0)).unwrap()
    }

    fn count(&self, query: &str, p: &str) -> i64 {
        self.db().query_row(query, [p], |f| f.get(0)).unwrap()
    }
}

/// The server is shut down even if the test fails: an orphaned one keeps the binary locked on Windows. What
/// it writes to stderr goes to a file, to say why it didn't answer.
struct Server(std::process::Child, u16, PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Server {
    /// (status, body) of a request with the token.
    fn request(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer secreto\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.1,
            body.len()
        );
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.1)) {
                s.write_all(req.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                let r = String::from_utf8_lossy(&v).into_owned();
                let status = r.get(9..12).and_then(|e| e.parse().ok()).unwrap_or(0);
                let body = r.find("\r\n\r\n").map(|i| r[i + 4..].to_string()).unwrap_or_default();
                return (status, body);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    }

    fn get(&self, path: &str) -> String {
        let (e, c) = self.request("GET", path, "");
        assert_eq!(e, 200, "GET {path}: {c}
stderr del servidor: {}", std::fs::read_to_string(&self.2).unwrap_or_default());
        c
    }

    fn post(&self, path: &str, body: serde_json::Value) -> (u16, String) {
        self.request("POST", path, &body.to_string())
    }
}

fn serve(p: &Fixture, folder: &Path) -> Server {
    let port = comun::puerto_libre();
    let log = p.dir.join(format!("serve-{port}.log"));
    let child = p
        .command(folder)
        .args(["serve", "--port", &port.to_string()])
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    Server(child, port, log)
}

/// `?dir=` with the folder escaped for the URL.
fn with_dir(path: &str, folder: &Path) -> String {
    let d: String = folder
        .to_string_lossy()
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("{path}{}dir={d}", if path.contains('?') { '&' } else { '?' })
}

/// What must not show from acme: nothing from globex nor the personal stuff without a space.
fn no_others(what: &str, text: &str) {
    for x in ["Globex", "globex", "Idea de blog", "notas"] {
        assert!(!text.contains(x), "{what} enseña «{x}» desde acme: {text}");
    }
}

#[test]
fn web_only_shows_the_space_it_started_in() {
    let p = Fixture { dir: comun::Temporal::nueva("aislweb") };
    for n in ["acme-api", "acme-web", "globex", "notas"] {
        std::fs::create_dir_all(p.project(n).join(".git")).unwrap();
    }
    let home = p.home();
    let save = |project: &str, args: &[&str]| {
        let mut a = vec!["remember", "-f"];
        a.extend_from_slice(args);
        p.ok(&p.project(project), &a);
    };
    p.ok(&home, &["remember", "-k", "preference", "-t", "Respuestas cortas", "--global", "Prefiero respuestas cortas."]);
    p.ok(&home, &["space", "add", "Acme"]);
    p.ok(&home, &["space", "add", "globex"]);
    save("acme-api", &["-k", "fact", "-t", "Acme usa Postgres", "Postgres 16 en RDS."]);
    save("acme-api", &["-k", "fact", "-t", "Acme API: despliegue", "La v0.2.0 publicada el 28-oct-2026 en staging.\nFalta publicar en producción."]);
    save("acme-api", &["-k", "fact", "-t", "Acme API: estado", "--tag", "status", "La 0.9.2 está publicada en producción."]);
    save("acme-web", &["-k", "fact", "-t", "Acme web en Astro", "Astro 5."]);
    save("acme-web", &["-k", "fact", "-t", "Acme web: tirar", "Se olvida."]);
    save("globex", &["-k", "fact", "-t", "Globex es Flutter", "App en Flutter 3.24 con token sk-test."]);
    // Texts different from acme's: if repeated, their words would stop being rare and Review would see nothing.
    save("globex", &["-k", "fact", "-t", "Globex: tienda", "Pendiente de la revisión de Apple."]);
    save("globex", &["-k", "fact", "-t", "Globex: estado", "--tag", "status", "Esperando a Google Play."]);
    save("globex", &["-k", "fact", "-t", "Globex: tirar", "Se olvida."]);
    save("notas", &["-k", "fact", "-t", "Idea de blog", "Escribir sobre memoria."]);
    for (e, n) in [("acme", "acme-api"), ("acme", "acme-web"), ("globex", "globex")] {
        let path = p.project(n);
        p.ok(&home, &["space", "assign", e, path.to_str().unwrap()]);
    }
    let forgotten_acme = p.id("Acme web: tirar");
    let forgotten_globex = p.id("Globex: tirar");
    p.ok(&home, &["forget", &forgotten_acme, "--yes"]);
    p.ok(&home, &["forget", &forgotten_globex, "--yes"]);
    // What was set aside from two sessions, one in each client (the project folder, as the hook stores it).
    {
        let db = p.db();
        for (n, text) in [("acme-api", "prefiero tabs en acme"), ("globex", "prefiero spaces en globex")] {
            let path: String = db.query_row("SELECT path FROM project WHERE name = ?1", [n], |f| f.get(0)).unwrap();
            db.execute(
                "INSERT INTO candidate(project, session, kind, text, created_at) VALUES (?1, 's', 'preference', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [&path, &text.to_string()],
            )
            .unwrap();
        }
    }
    let globex = p.id("Globex es Flutter");
    let globex_old = p.id("Globex: tienda");
    let globex_new = p.id("Globex: estado");
    let cand_globex: i64 = p.db().query_row("SELECT id FROM candidate WHERE text LIKE '%globex%'", [], |f| f.get(0)).unwrap();

    // From an acme project: only acme (both projects) and the global ones, in every view.
    let s = serve(&p, &p.project("acme-api"));
    let memories = s.get("/api/memories");
    assert!(memories.contains("Acme usa Postgres") && memories.contains("Acme web en Astro"), "todo el espacio: {memories}");
    assert!(memories.contains("Respuestas cortas"), "y lo global: {memories}");
    no_others("/api/memories", &memories);
    let projects: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(projects["scope"]["scope"], "space", "{projects}");
    assert_eq!(projects["scope"]["space"], "acme", "{projects}");
    assert_eq!(projects["scope"]["name"], "Acme", "{projects}");
    let names: Vec<&str> = projects["projects"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 2, "{projects}");
    assert!(names.iter().all(|n| n.starts_with("acme")), "{projects}");
    no_others("/api/projects", &projects.to_string());
    let review = s.get("/api/review");
    assert!(review.contains("Acme API: despliegue"), "lo de acme sí: {review}");
    no_others("/api/review", &review);
    let trash = s.get("/api/trash");
    assert!(trash.contains(&forgotten_acme), "{trash}");
    assert!(!trash.contains(&forgotten_globex), "{trash}");
    no_others("/api/trash", &trash);
    let candidates = s.get("/api/candidates");
    assert!(candidates.contains("tabs en acme"), "{candidates}");
    no_others("/api/candidates", &candidates);
    no_others("/api/overview", &s.get("/api/overview"));
    no_others("/search", &s.get("/search?q=Flutter"));
    let (e, c) = s.post("/tools/search", serde_json::json!({ "query": "Flutter", "all_projects": true }));
    assert!(e == 200 || e == 400, "{c}");
    no_others("/tools/search", &c);
    let (e, c) = s.post("/tools/recent", serde_json::json!({ "all_projects": true, "limit": 50 }));
    assert_eq!(e, 200, "{c}");
    no_others("/tools/recent", &c);
    let (e, c) = s.post("/tools/get", serde_json::json!({ "id": globex }));
    assert_eq!(e, 400, "get de otro cliente: {c}");
    no_others("/tools/get", &c);

    // Writing outside: nothing changes.
    for (path, body) in [
        ("/tools/forget", serde_json::json!({ "id": globex })),
        ("/tools/update", serde_json::json!({ "id": globex, "pinned": true, "reason": "x" })),
        ("/tools/update", serde_json::json!({ "id": globex, "content": "cambiado", "reason": "x" })),
    ] {
        let (e, c) = s.post(path, body);
        assert_eq!(e, 400, "{path}: {c}");
    }
    for (path, body) in [
        ("/api/redact", serde_json::json!({ "id": globex })),
        ("/api/restore", serde_json::json!({ "id": forgotten_globex })),
        ("/api/review/dismiss", serde_json::json!({ "memory_id": globex_old, "kind": "outdated", "other_id": globex_new })),
        ("/api/candidates/dismiss", serde_json::json!({ "id": cand_globex })),
        // The id in lowercase, or just its prefix, is the same id: not allowed either (before, `Scope::sees` didn't
        // find it as is, treated it as visible, and `revisar::dismiss` did find it).
        ("/api/review/dismiss", serde_json::json!({ "memory_id": globex_old.to_lowercase(), "kind": "outdated", "other_id": globex_new.to_lowercase() })),
        ("/api/review/dismiss", serde_json::json!({ "memory_id": &globex_old[..20], "kind": "outdated", "other_id": &globex_new[..20] })),
        ("/api/review/dismiss", serde_json::json!({ "memory_id": globex.to_lowercase(), "kind": "secret" })),
    ] {
        let (e, c) = s.post(path, body);
        assert_eq!(e, 403, "{path}: {c}");
    }
    // `?dir=` with a network path: not looked at (any website can request it with an `<img>`; looking at it
    // opened an SMB connection, took ~21 s and blocked the server).
    for d in [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\??\UNC\10.255.255.1\x", r"\\?\UNC\10.255.255.1\x"] {
        let t = std::time::Instant::now();
        let (e, c) = s.request("GET", &with_dir("/api/memories", Path::new(d)), "");
        assert_eq!(e, 400, "{d}: {c}");
        assert!(t.elapsed() < std::time::Duration::from_secs(3), "{d}: {:?}", t.elapsed());
    }
    assert_eq!(p.count("SELECT count(*) FROM memory WHERE id = ?1 AND pinned = 0 AND content LIKE '%sk-test%'", &globex), 1, "globex sigue igual");
    assert_eq!(p.count("SELECT count(*) FROM forgotten WHERE id = ?1", &forgotten_globex), 1, "sigue en la papelera");
    assert_eq!(p.count("SELECT count(*) FROM verdict WHERE memory_id = ?1 OR other_id = ?1", &globex_old), 0, "sin veredicto");
    assert_eq!(p.count("SELECT count(*) FROM candidate WHERE text LIKE ?1", "%globex%"), 1, "la candidata sigue");
    // Inside, yes.
    let (e, c) = s.post("/api/restore", serde_json::json!({ "id": forgotten_acme }));
    assert_eq!(e, 200, "{c}");
    let (e, c) = s.post("/tools/update", serde_json::json!({ "id": p.id("Acme usa Postgres"), "pinned": true, "reason": "x" }));
    assert_eq!(e, 200, "{c}");

    // `?dir=` doesn't widen: not to the user's folder, nor to another client, nor to what has no space.
    for d in [home.clone(), p.project("globex"), p.project("notas")] {
        for path in ["/api/memories", "/api/projects", "/api/overview", "/search?q=Flutter", "/tools"] {
            let (e, c) = s.request("GET", &with_dir(path, &d), "");
            assert_eq!(e, 403, "{path} con dir={}: {c}", d.display());
        }
        let (e, c) = s.request("POST", &with_dir("/tools/forget", &d), &serde_json::json!({ "id": globex }).to_string());
        assert_eq!(e, 403, "{c}");
    }
    // But it does pick another project in the same space.
    let other = s.get(&with_dir("/api/memories", &p.project("acme-web")));
    assert!(other.contains("Acme usa Postgres"), "{other}");
    no_others("?dir=acme-web", &other);
    drop(s);

    // From a project without a space: what has no space and the global ones, nothing from the clients.
    let s = serve(&p, &p.project("notas"));
    let memories = s.get("/api/memories");
    assert!(memories.contains("Idea de blog") && memories.contains("Respuestas cortas"), "{memories}");
    assert!(!memories.contains("Acme") && !memories.contains("Globex"), "{memories}");
    let projects: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(projects["scope"]["scope"], "space", "{projects}");
    assert_eq!(projects["scope"]["space"], "", "{projects}");
    assert_eq!(projects["spaces"].as_array().unwrap().len(), 0, "{projects}");
    let text = projects.to_string();
    assert!(!text.contains("acme") && !text.contains("globex"), "{text}");
    for path in ["/api/review", "/api/trash", "/api/candidates", "/api/overview", "/search?q=Flutter"] {
        let c = s.get(path);
        assert!(!c.contains("Acme") && !c.contains("Globex") && !c.contains("acme") && !c.contains("globex"), "{path}: {c}");
    }
    let (e, c) = s.post("/api/redact", serde_json::json!({ "id": globex }));
    assert_eq!(e, 403, "{c}");
    let (e, c) = s.request("GET", &with_dir("/api/memories", &home), "");
    assert_eq!(e, 403, "{c}");
    let (e, c) = s.request("GET", &with_dir("/api/memories", &p.project("acme-api")), "");
    assert_eq!(e, 403, "{c}");
    drop(s);

    // From the user's folder: everything, and `?dir=` narrows to one client.
    let s = serve(&p, &home);
    let memories = s.get("/api/memories");
    for t in ["Acme usa Postgres", "Globex es Flutter", "Idea de blog", "Respuestas cortas"] {
        assert!(memories.contains(t), "{t}: {memories}");
    }
    let projects: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(projects["scope"]["scope"], "all", "{projects}");
    assert_eq!(projects["projects"].as_array().unwrap().len(), 4, "{projects}");
    assert!(s.get("/api/trash").contains(&forgotten_globex));
    assert!(s.get("/api/candidates").contains("globex"));
    let only_acme = s.get(&with_dir("/api/memories", &p.project("acme-api")));
    assert!(only_acme.contains("Acme usa Postgres"), "{only_acme}");
    no_others("?dir=acme-api desde casa", &only_acme);
    let (e, c) = s.post("/api/redact", serde_json::json!({ "id": globex }));
    assert_eq!(e, 200, "desde casa sí: {c}");
}

/// 1.7.1: `/api/sync` from a client only shows that space's repository, never the URL or the
/// author of the general one (the personal one) nor those of another client.
#[test]
fn web_sync_only_shows_the_space_repository() {
    let p = Fixture { dir: comun::Temporal::nueva("aislweb-sync") };
    for n in ["acme-api", "globex", "notas"] {
        std::fs::create_dir_all(p.project(n).join(".git")).unwrap();
    }
    let home = p.home();
    p.ok(&home, &["space", "add", "Acme"]);
    p.ok(&home, &["space", "add", "globex"]);
    for n in ["acme-api", "globex", "notas"] {
        p.ok(&p.project(n), &["remember", "-f", "-k", "fact", "-t", &format!("Nota de {n}"), "Algo."]);
    }
    for (e, n) in [("acme", "acme-api"), ("globex", "globex")] {
        let path = p.project(n);
        p.ok(&home, &["space", "assign", e, path.to_str().unwrap()]);
    }
    // The configurations as `sync init` leaves them: the general one next to the database (`o.sync.json`) and
    // one per space in `o.sync-spaces/<space>/`.
    let config = |path: PathBuf, url: &str, author: &str, space: Option<&str>| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = serde_json::json!({ "url": url, "kinds": ["fact"], "global": true, "author": author, "person": "p" });
        if let Some(e) = space {
            c["space"] = e.into();
        }
        std::fs::write(path, c.to_string()).unwrap();
    };
    config(p.dir.join("o.sync.json"), "https://git.example/marc/personal.git", "Marc Personal", None);
    let spaces = p.dir.join("o.sync-spaces");
    config(spaces.join("acme").join("sync.json"), "https://git.example/acme/memoria.git", "Marc en Acme", Some("acme"));
    config(spaces.join("globex").join("sync.json"), "https://git.example/globex/memoria.git", "Marc en Globex", Some("globex"));
    let sync = |s: &Server| -> serde_json::Value { serde_json::from_str(&s.get("/api/sync")).unwrap() };

    // From acme: its repository and its author, nothing from the general one or globex.
    let s = serve(&p, &p.project("acme-api"));
    let v = sync(&s);
    assert_eq!(v["configured"], true, "{v}");
    assert_eq!(v["url"], "https://git.example/acme/memoria.git", "{v}");
    assert_eq!(v["author"], "Marc en Acme", "{v}");
    let text = v.to_string();
    for x in ["personal", "Personal", "globex", "Globex"] {
        assert!(!text.contains(x), "desde acme enseña «{x}»: {text}");
    }
    drop(s);

    // From a project without a space, and from the user's folder: the general one.
    for folder in [p.project("notas"), home.clone()].iter() {
        let s = serve(&p, folder);
        let v = sync(&s);
        assert_eq!(v["url"], "https://git.example/marc/personal.git", "{v}");
        assert_eq!(v["author"], "Marc Personal", "{v}");
        let text = v.to_string();
        assert!(!text.contains("acme") && !text.contains("globex"), "{text}");
    }

    // A client without its own repository doesn't fall back to the general one: there's nothing to show.
    std::fs::remove_dir_all(spaces.join("acme")).unwrap();
    let s = serve(&p, &p.project("acme-api"));
    let v = sync(&s);
    assert_eq!(v["configured"], false, "{v}");
    assert!(v["url"].is_null() && v["author"].is_null(), "{v}");
}
