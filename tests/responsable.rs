//! 1.7.0: the owner view (`GET /api/overview` of `omoi serve`): per space, what there is, who
//! wrote it and what is outdated or paused; from a project, only its space, and with the same
//! protection as the rest of the API.

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

    fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        let home = self.home();
        c.current_dir(cwd)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env_remove("OMOIKANE_LANG");
        comun::sin_la_sesion(&mut c);
        c
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let o = self.cmd(cwd).args(args).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn sql(&self, query: &str, p: &[&dyn rusqlite::ToSql]) {
        let db = rusqlite::Connection::open(self.dir.join("o.db")).unwrap();
        db.execute(query, p).unwrap();
    }

    fn set_author(&self, name: &str) {
        self.sql("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', ?1)", &[&name]);
    }
}

/// The server is shut down even if the test fails: an orphaned one keeps the binary locked on Windows.
struct Server(std::process::Child, u16);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Server {
    fn request(&self, req: &str) -> String {
        for _ in 0..50 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.1)) {
                s.write_all(req.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                return String::from_utf8_lossy(&v).into_owned();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    }

    /// The JSON of `/api/overview`, with the token.
    fn overview(&self, query: &str) -> serde_json::Value {
        let r = self.request(&format!(
            "GET /api/overview{query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n",
            self.1
        ));
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        let body = &r[r.find("\r\n\r\n").unwrap() + 4..];
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }
}

fn serve(p: &Fixture, cwd: &Path) -> Server {
    let port = comun::puerto_libre();
    let child = p
        .cmd(cwd)
        .args(["serve", "--port", &port.to_string()])
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Server(child, port)
}

fn find_space<'a>(v: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    v["spaces"].as_array().unwrap().iter().find(|s| s["key"] == key).unwrap_or_else(|| panic!("sin el espacio {key:?}: {v}"))
}

#[test]
fn owner_view_by_space() {
    let p = Fixture { dir: comun::Temporal::nueva("responsable") };
    for n in ["acme-api", "acme-web", "globex", "notas"] {
        std::fs::create_dir_all(p.project(n).join(".git")).unwrap();
    }
    let home = p.home();
    let remember = |project: &str, args: &[&str]| {
        let mut a = vec!["remember", "-f"];
        a.extend_from_slice(args);
        p.ok(&p.project(project), &a);
    };
    p.ok(&home, &["remember", "-k", "preference", "-t", "Respuestas cortas", "--global", "Prefiero respuestas cortas."]);
    p.ok(&home, &["space", "add", "acme"]);
    p.ok(&home, &["space", "add", "globex"]);
    p.set_author("Ana");
    remember("acme-api", &["-k", "fact", "-t", "Acme usa Postgres 16", "--pinned", "Postgres 16 en RDS."]);
    remember("acme-api", &["-k", "fact", "-t", "Acme API: despliegue", "La v0.2.0 publicada el 28-oct-2026 en staging.\nFalta publicar en producción."]);
    remember("acme-api", &["-k", "fact", "-t", "Acme API: estado", "--tag", "status", "La 0.9.2 está publicada en producción."]);
    p.set_author("Carlos");
    remember("acme-web", &["-k", "decision", "-t", "Precios con IVA", "--tag", "open", "--source", "mcp:codex", "Asumo que con IVA hasta que se decida."]);
    remember("acme-web", &["-k", "fact", "-t", "Acme web: límites viejos", "100 peticiones por minuto."]);
    p.set_author("Lucía");
    remember("globex", &["-k", "fact", "-t", "Globex es Flutter", "App en Flutter 3.24."]);
    p.set_author("");
    remember("notas", &["-k", "fact", "-t", "Idea de blog", "Escribir sobre memoria."]);
    for (e, n) in [("acme", "acme-api"), ("acme", "acme-web"), ("globex", "globex")] {
        let path = p.project(n);
        p.ok(&home, &["space", "assign", e, path.to_str().unwrap()]);
    }
    // One nobody has touched for a year and a paused project (another machine says it belongs to acme).
    p.sql("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-12 months') WHERE title = 'Acme web: límites viejos'", &[]);
    p.sql(
        r#"UPDATE project SET paused = '{"space":"acme","machine":"portatil-lucia","hlc":"0","at":"2026-11-05T09:00:00.000Z"}' WHERE name = 'globex'"#,
        &[],
    );

    // From the user's folder: every space, and at the end the projects without a space.
    let s = serve(&p, &home);
    let port = s.1;
    let no_token = s.request(&format!("GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"));
    assert!(no_token.starts_with("HTTP/1.1 401"), "sin token no: {no_token}");
    let foreign_host = s.request(&format!("GET /api/overview HTTP/1.1\r\nHost: malo.example:{port}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(foreign_host.starts_with("HTTP/1.1 403"), "otro Host no: {foreign_host}");
    let v = s.overview("");
    assert_eq!(v["scope"], "all", "{v}");
    let keys: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["acme", "globex", ""], "{v}");
    assert_eq!(v["global_memories"], 1, "lo global no es de ningún cliente: {v}");

    let acme = find_space(&v, "acme");
    assert_eq!(acme["memories"], 5, "{acme}");
    // What there is: the projects with their latest status, the pinned ones and the open decisions.
    let api = acme["projects"].as_array().unwrap().iter().find(|x| x["name"] == "acme-api").unwrap();
    assert_eq!(api["status"]["title"], "Acme API: estado", "{api}");
    assert_eq!(api["status"]["author"], "Ana", "{api}");
    assert_eq!(api["pinned"], 1, "{api}");
    assert_eq!(acme["pinned"]["count"], 1, "{acme}");
    assert_eq!(acme["open_decisions"]["items"][0]["title"], "Precios con IVA", "{acme}");
    // Who wrote it: per author, with their agents.
    let authors = acme["authors"].as_array().unwrap();
    let ana = authors.iter().find(|a| a["author"] == "Ana").unwrap();
    assert_eq!(ana["memories"], 3, "{acme}");
    let carlos = authors.iter().find(|a| a["author"] == "Carlos").unwrap();
    assert_eq!(carlos["memories"], 2, "{acme}");
    assert_eq!(carlos["recent"], 1, "la del año pasado no cuenta como reciente: {carlos}");
    assert!(carlos["sources"].as_array().unwrap().iter().any(|f| f["source"] == "mcp:codex"), "{carlos}");
    assert!(acme["recent"].as_array().unwrap().iter().all(|m| m["title"] != "Globex es Flutter"), "nada de otro espacio: {acme}");
    // What is outdated: what Review shows and what nobody touches.
    assert_eq!(acme["review"]["kinds"]["outdated"], 1, "{acme}");
    assert!(acme["review"]["items"].as_array().unwrap().iter().any(|a| a["title"] == "Acme API: despliegue"), "{acme}");
    assert_eq!(acme["untouched"]["count"], 1, "{acme}");
    assert_eq!(acme["untouched"]["items"][0]["title"], "Acme web: límites viejos", "{acme}");
    // And what is paused, with both commands.
    let globex = find_space(&v, "globex");
    assert_eq!(globex["paused"][0]["keep"], "omoi projects keep globex", "{globex}");
    assert_eq!(globex["projects"][0]["paused"], true, "{globex}");
    assert_eq!(globex["authors"][0]["author"], "Lucía", "{globex}");
    let personal = find_space(&v, "");
    assert_eq!(personal["authors"][0]["author"], "", "sin autor: {personal}");
    // With more months, last year's one no longer counts as forgotten.
    assert_eq!(find_space(&s.overview("?months=24"), "acme")["untouched"]["count"], 0);
    drop(s);

    // From an acme project: only acme (the whole space, not just that project).
    let s = serve(&p, &p.project("acme-api"));
    let v = s.overview("");
    assert_eq!(v["scope"], "space", "{v}");
    assert_eq!(v["space"], "acme", "{v}");
    let keys: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["acme"], "nada de globex ni de lo personal: {v}");
    assert_eq!(find_space(&v, "acme")["projects"].as_array().unwrap().len(), 2, "{v}");
    let text = v.to_string();
    assert!(!text.contains("Globex") && !text.contains("Idea de blog") && !text.contains("globex"), "{text}");
    drop(s);

    // From a project without a space: only the projects without a space.
    let s = serve(&p, &p.project("notas"));
    let v = s.overview("");
    let keys: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, [""], "{v}");
    assert!(!v.to_string().contains("Acme"), "{v}");
}
