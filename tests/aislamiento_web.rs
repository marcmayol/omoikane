//! 1.7.0: la vista web (`omoi serve`) respeta el aislamiento de espacios como la CLI y el MCP. Lo que
//! enseña lo decide la carpeta en la que arrancó: desde un proyecto de un cliente, solo ese cliente (y lo
//! global); desde un proyecto sin espacio, solo lo que no tiene espacio; desde la carpeta del usuario,
//! todo. Tampoco se puede escribir fuera, y `?dir=` solo restringe.

mod comun;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Prueba {
    dir: comun::Temporal,
}

impl Prueba {
    fn casa(&self) -> PathBuf {
        self.dir.join("casa")
    }

    fn proyecto(&self, nombre: &str) -> PathBuf {
        self.casa().join(nombre)
    }

    fn orden(&self, carpeta: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        let casa = self.casa();
        c.current_dir(carpeta)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("USERPROFILE", &casa)
            .env("HOME", &casa)
            .env_remove("OMOIKANE_LANG");
        c
    }

    fn ok(&self, carpeta: &Path, args: &[&str]) {
        let o = self.orden(carpeta).args(args).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn base(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }

    fn id(&self, titulo: &str) -> String {
        self.base().query_row("SELECT id FROM memory WHERE title = ?1", [titulo], |f| f.get(0)).unwrap()
    }

    fn cuenta(&self, consulta: &str, p: &str) -> i64 {
        self.base().query_row(consulta, [p], |f| f.get(0)).unwrap()
    }
}

/// El servidor se cierra aunque el test falle: uno huérfano deja bloqueado el binario en Windows. Lo
/// que escribe por stderr queda en un fichero, para decir por qué no contestó.
struct Servidor(std::process::Child, u16, PathBuf);

impl Drop for Servidor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Servidor {
    /// (estado, cuerpo) de una petición con el token.
    fn pedir(&self, metodo: &str, ruta: &str, cuerpo: &str) -> (u16, String) {
        let peticion = format!(
            "{metodo} {ruta} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer secreto\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
            self.1,
            cuerpo.len()
        );
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.1)) {
                s.write_all(peticion.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                let r = String::from_utf8_lossy(&v).into_owned();
                let estado = r.get(9..12).and_then(|e| e.parse().ok()).unwrap_or(0);
                let cuerpo = r.find("\r\n\r\n").map(|i| r[i + 4..].to_string()).unwrap_or_default();
                return (estado, cuerpo);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    }

    fn get(&self, ruta: &str) -> String {
        let (e, c) = self.pedir("GET", ruta, "");
        assert_eq!(e, 200, "GET {ruta}: {c}
stderr del servidor: {}", std::fs::read_to_string(&self.2).unwrap_or_default());
        c
    }

    fn post(&self, ruta: &str, cuerpo: serde_json::Value) -> (u16, String) {
        self.pedir("POST", ruta, &cuerpo.to_string())
    }
}

fn servir(p: &Prueba, carpeta: &Path) -> Servidor {
    let puerto = comun::puerto_libre();
    let log = p.dir.join(format!("serve-{puerto}.log"));
    let hijo = p
        .orden(carpeta)
        .args(["serve", "--port", &puerto.to_string()])
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    Servidor(hijo, puerto, log)
}

/// `?dir=` con la carpeta escapada para la URL.
fn con_dir(ruta: &str, carpeta: &Path) -> String {
    let d: String = carpeta
        .to_string_lossy()
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("{ruta}{}dir={d}", if ruta.contains('?') { '&' } else { '?' })
}

/// Lo que no debe salir desde acme: nada de globex ni de lo personal sin espacio.
fn sin_otros(que: &str, texto: &str) {
    for x in ["Globex", "globex", "Idea de blog", "notas"] {
        assert!(!texto.contains(x), "{que} enseña «{x}» desde acme: {texto}");
    }
}

#[test]
fn la_web_solo_ensena_lo_del_espacio_donde_arranco() {
    let p = Prueba { dir: comun::Temporal::nueva("aislweb") };
    for n in ["acme-api", "acme-web", "globex", "notas"] {
        std::fs::create_dir_all(p.proyecto(n).join(".git")).unwrap();
    }
    let casa = p.casa();
    let guardar = |proyecto: &str, args: &[&str]| {
        let mut a = vec!["remember", "-f"];
        a.extend_from_slice(args);
        p.ok(&p.proyecto(proyecto), &a);
    };
    p.ok(&casa, &["remember", "-k", "preference", "-t", "Respuestas cortas", "--global", "Prefiero respuestas cortas."]);
    p.ok(&casa, &["space", "add", "Acme"]);
    p.ok(&casa, &["space", "add", "globex"]);
    guardar("acme-api", &["-k", "fact", "-t", "Acme usa Postgres", "Postgres 16 en RDS."]);
    guardar("acme-api", &["-k", "fact", "-t", "Acme API: despliegue", "La v0.2.0 publicada el 28-oct-2026 en staging.\nFalta publicar en producción."]);
    guardar("acme-api", &["-k", "fact", "-t", "Acme API: estado", "--tag", "status", "La 0.9.2 está publicada en producción."]);
    guardar("acme-web", &["-k", "fact", "-t", "Acme web en Astro", "Astro 5."]);
    guardar("acme-web", &["-k", "fact", "-t", "Acme web: tirar", "Se olvida."]);
    guardar("globex", &["-k", "fact", "-t", "Globex es Flutter", "App en Flutter 3.24 con token sk-test."]);
    // Textos distintos de los de acme: repetidos, sus palabras dejarían de ser raras y Revisar no vería nada.
    guardar("globex", &["-k", "fact", "-t", "Globex: tienda", "Pendiente de la revisión de Apple."]);
    guardar("globex", &["-k", "fact", "-t", "Globex: estado", "--tag", "status", "Esperando a Google Play."]);
    guardar("globex", &["-k", "fact", "-t", "Globex: tirar", "Se olvida."]);
    guardar("notas", &["-k", "fact", "-t", "Idea de blog", "Escribir sobre memoria."]);
    for (e, n) in [("acme", "acme-api"), ("acme", "acme-web"), ("globex", "globex")] {
        let ruta = p.proyecto(n);
        p.ok(&casa, &["space", "assign", e, ruta.to_str().unwrap()]);
    }
    let tirada_acme = p.id("Acme web: tirar");
    let tirada_globex = p.id("Globex: tirar");
    p.ok(&casa, &["forget", &tirada_acme, "--yes"]);
    p.ok(&casa, &["forget", &tirada_globex, "--yes"]);
    // Lo apartado de dos sesiones, una en cada cliente (la carpeta del proyecto, como la guarda el hook).
    {
        let db = p.base();
        for (n, texto) in [("acme-api", "prefiero tabs en acme"), ("globex", "prefiero spaces en globex")] {
            let ruta: String = db.query_row("SELECT path FROM project WHERE name = ?1", [n], |f| f.get(0)).unwrap();
            db.execute(
                "INSERT INTO candidate(project, session, kind, text, created_at) VALUES (?1, 's', 'preference', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                [&ruta, &texto.to_string()],
            )
            .unwrap();
        }
    }
    let globex = p.id("Globex es Flutter");
    let globex_vieja = p.id("Globex: tienda");
    let globex_nueva = p.id("Globex: estado");
    let cand_globex: i64 = p.base().query_row("SELECT id FROM candidate WHERE text LIKE '%globex%'", [], |f| f.get(0)).unwrap();

    // Desde un proyecto de acme: solo acme (los dos proyectos) y lo global, en cada vista.
    let s = servir(&p, &p.proyecto("acme-api"));
    let memorias = s.get("/api/memories");
    assert!(memorias.contains("Acme usa Postgres") && memorias.contains("Acme web en Astro"), "todo el espacio: {memorias}");
    assert!(memorias.contains("Respuestas cortas"), "y lo global: {memorias}");
    sin_otros("/api/memories", &memorias);
    let proyectos: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(proyectos["scope"]["scope"], "space", "{proyectos}");
    assert_eq!(proyectos["scope"]["space"], "acme", "{proyectos}");
    assert_eq!(proyectos["scope"]["name"], "Acme", "{proyectos}");
    let nombres: Vec<&str> = proyectos["projects"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(nombres.len(), 2, "{proyectos}");
    assert!(nombres.iter().all(|n| n.starts_with("acme")), "{proyectos}");
    sin_otros("/api/projects", &proyectos.to_string());
    let revisar = s.get("/api/review");
    assert!(revisar.contains("Acme API: despliegue"), "lo de acme sí: {revisar}");
    sin_otros("/api/review", &revisar);
    let papelera = s.get("/api/trash");
    assert!(papelera.contains(&tirada_acme), "{papelera}");
    assert!(!papelera.contains(&tirada_globex), "{papelera}");
    sin_otros("/api/trash", &papelera);
    let candidatas = s.get("/api/candidates");
    assert!(candidatas.contains("tabs en acme"), "{candidatas}");
    sin_otros("/api/candidates", &candidatas);
    sin_otros("/api/overview", &s.get("/api/overview"));
    sin_otros("/search", &s.get("/search?q=Flutter"));
    let (e, c) = s.post("/tools/search", serde_json::json!({ "query": "Flutter", "all_projects": true }));
    assert!(e == 200 || e == 400, "{c}");
    sin_otros("/tools/search", &c);
    let (e, c) = s.post("/tools/recent", serde_json::json!({ "all_projects": true, "limit": 50 }));
    assert_eq!(e, 200, "{c}");
    sin_otros("/tools/recent", &c);
    let (e, c) = s.post("/tools/get", serde_json::json!({ "id": globex }));
    assert_eq!(e, 400, "get de otro cliente: {c}");
    sin_otros("/tools/get", &c);

    // Escribir fuera: nada cambia.
    for (ruta, cuerpo) in [
        ("/tools/forget", serde_json::json!({ "id": globex })),
        ("/tools/update", serde_json::json!({ "id": globex, "pinned": true, "reason": "x" })),
        ("/tools/update", serde_json::json!({ "id": globex, "content": "cambiado", "reason": "x" })),
    ] {
        let (e, c) = s.post(ruta, cuerpo);
        assert_eq!(e, 400, "{ruta}: {c}");
    }
    for (ruta, cuerpo) in [
        ("/api/redact", serde_json::json!({ "id": globex })),
        ("/api/restore", serde_json::json!({ "id": tirada_globex })),
        ("/api/review/dismiss", serde_json::json!({ "memory_id": globex_vieja, "kind": "outdated", "other_id": globex_nueva })),
        ("/api/candidates/dismiss", serde_json::json!({ "id": cand_globex })),
    ] {
        let (e, c) = s.post(ruta, cuerpo);
        assert_eq!(e, 403, "{ruta}: {c}");
    }
    assert_eq!(p.cuenta("SELECT count(*) FROM memory WHERE id = ?1 AND pinned = 0 AND content LIKE '%sk-test%'", &globex), 1, "globex sigue igual");
    assert_eq!(p.cuenta("SELECT count(*) FROM forgotten WHERE id = ?1", &tirada_globex), 1, "sigue en la papelera");
    assert_eq!(p.cuenta("SELECT count(*) FROM verdict WHERE memory_id = ?1 OR other_id = ?1", &globex_vieja), 0, "sin veredicto");
    assert_eq!(p.cuenta("SELECT count(*) FROM candidate WHERE text LIKE ?1", "%globex%"), 1, "la candidata sigue");
    // Dentro sí.
    let (e, c) = s.post("/api/restore", serde_json::json!({ "id": tirada_acme }));
    assert_eq!(e, 200, "{c}");
    let (e, c) = s.post("/tools/update", serde_json::json!({ "id": p.id("Acme usa Postgres"), "pinned": true, "reason": "x" }));
    assert_eq!(e, 200, "{c}");

    // `?dir=` no amplía: ni a la carpeta del usuario, ni a otro cliente, ni a lo que no tiene espacio.
    for d in [casa.clone(), p.proyecto("globex"), p.proyecto("notas")] {
        for ruta in ["/api/memories", "/api/projects", "/api/overview", "/search?q=Flutter", "/tools"] {
            let (e, c) = s.pedir("GET", &con_dir(ruta, &d), "");
            assert_eq!(e, 403, "{ruta} con dir={}: {c}", d.display());
        }
        let (e, c) = s.pedir("POST", &con_dir("/tools/forget", &d), &serde_json::json!({ "id": globex }).to_string());
        assert_eq!(e, 403, "{c}");
    }
    // Pero sí elige otro proyecto del mismo espacio.
    let otro = s.get(&con_dir("/api/memories", &p.proyecto("acme-web")));
    assert!(otro.contains("Acme usa Postgres"), "{otro}");
    sin_otros("?dir=acme-web", &otro);
    drop(s);

    // Desde un proyecto sin espacio: lo que no tiene espacio y lo global, nada de los clientes.
    let s = servir(&p, &p.proyecto("notas"));
    let memorias = s.get("/api/memories");
    assert!(memorias.contains("Idea de blog") && memorias.contains("Respuestas cortas"), "{memorias}");
    assert!(!memorias.contains("Acme") && !memorias.contains("Globex"), "{memorias}");
    let proyectos: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(proyectos["scope"]["scope"], "space", "{proyectos}");
    assert_eq!(proyectos["scope"]["space"], "", "{proyectos}");
    assert_eq!(proyectos["spaces"].as_array().unwrap().len(), 0, "{proyectos}");
    let texto = proyectos.to_string();
    assert!(!texto.contains("acme") && !texto.contains("globex"), "{texto}");
    for ruta in ["/api/review", "/api/trash", "/api/candidates", "/api/overview", "/search?q=Flutter"] {
        let c = s.get(ruta);
        assert!(!c.contains("Acme") && !c.contains("Globex") && !c.contains("acme") && !c.contains("globex"), "{ruta}: {c}");
    }
    let (e, c) = s.post("/api/redact", serde_json::json!({ "id": globex }));
    assert_eq!(e, 403, "{c}");
    let (e, c) = s.pedir("GET", &con_dir("/api/memories", &casa), "");
    assert_eq!(e, 403, "{c}");
    let (e, c) = s.pedir("GET", &con_dir("/api/memories", &p.proyecto("acme-api")), "");
    assert_eq!(e, 403, "{c}");
    drop(s);

    // Desde la carpeta del usuario: todo, y `?dir=` restringe a un cliente.
    let s = servir(&p, &casa);
    let memorias = s.get("/api/memories");
    for t in ["Acme usa Postgres", "Globex es Flutter", "Idea de blog", "Respuestas cortas"] {
        assert!(memorias.contains(t), "{t}: {memorias}");
    }
    let proyectos: serde_json::Value = serde_json::from_str(&s.get("/api/projects")).unwrap();
    assert_eq!(proyectos["scope"]["scope"], "all", "{proyectos}");
    assert_eq!(proyectos["projects"].as_array().unwrap().len(), 4, "{proyectos}");
    assert!(s.get("/api/trash").contains(&tirada_globex));
    assert!(s.get("/api/candidates").contains("globex"));
    let solo_acme = s.get(&con_dir("/api/memories", &p.proyecto("acme-api")));
    assert!(solo_acme.contains("Acme usa Postgres"), "{solo_acme}");
    sin_otros("?dir=acme-api desde casa", &solo_acme);
    let (e, c) = s.post("/api/redact", serde_json::json!({ "id": globex }));
    assert_eq!(e, 200, "desde casa sí: {c}");
}

/// 1.7.1: `/api/sync` desde un cliente solo enseña el repositorio de ese espacio, nunca la URL ni el
/// autor del general (el personal) ni los de otro cliente.
#[test]
fn la_sincronizacion_de_la_web_solo_ensena_el_repositorio_del_espacio() {
    let p = Prueba { dir: comun::Temporal::nueva("aislweb-sync") };
    for n in ["acme-api", "globex", "notas"] {
        std::fs::create_dir_all(p.proyecto(n).join(".git")).unwrap();
    }
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "Acme"]);
    p.ok(&casa, &["space", "add", "globex"]);
    for n in ["acme-api", "globex", "notas"] {
        p.ok(&p.proyecto(n), &["remember", "-f", "-k", "fact", "-t", &format!("Nota de {n}"), "Algo."]);
    }
    for (e, n) in [("acme", "acme-api"), ("globex", "globex")] {
        let ruta = p.proyecto(n);
        p.ok(&casa, &["space", "assign", e, ruta.to_str().unwrap()]);
    }
    // Las configuraciones tal como las deja `sync init`: la general junto a la base (`o.sync.json`) y
    // una por espacio en `o.sync-spaces/<espacio>/`.
    let config = |ruta: PathBuf, url: &str, autor: &str, espacio: Option<&str>| {
        std::fs::create_dir_all(ruta.parent().unwrap()).unwrap();
        let mut c = serde_json::json!({ "url": url, "kinds": ["fact"], "global": true, "author": autor, "person": "p" });
        if let Some(e) = espacio {
            c["space"] = e.into();
        }
        std::fs::write(ruta, c.to_string()).unwrap();
    };
    config(p.dir.join("o.sync.json"), "https://git.example/marc/personal.git", "Marc Personal", None);
    let espacios = p.dir.join("o.sync-spaces");
    config(espacios.join("acme").join("sync.json"), "https://git.example/acme/memoria.git", "Marc en Acme", Some("acme"));
    config(espacios.join("globex").join("sync.json"), "https://git.example/globex/memoria.git", "Marc en Globex", Some("globex"));
    let sync = |s: &Servidor| -> serde_json::Value { serde_json::from_str(&s.get("/api/sync")).unwrap() };

    // Desde acme: su repositorio y su autor, nada del general ni de globex.
    let s = servir(&p, &p.proyecto("acme-api"));
    let v = sync(&s);
    assert_eq!(v["configured"], true, "{v}");
    assert_eq!(v["url"], "https://git.example/acme/memoria.git", "{v}");
    assert_eq!(v["author"], "Marc en Acme", "{v}");
    let texto = v.to_string();
    for x in ["personal", "Personal", "globex", "Globex"] {
        assert!(!texto.contains(x), "desde acme enseña «{x}»: {texto}");
    }
    drop(s);

    // Desde un proyecto sin espacio, y desde la carpeta del usuario: el general.
    for carpeta in [p.proyecto("notas"), casa.clone()].iter() {
        let s = servir(&p, carpeta);
        let v = sync(&s);
        assert_eq!(v["url"], "https://git.example/marc/personal.git", "{v}");
        assert_eq!(v["author"], "Marc Personal", "{v}");
        let texto = v.to_string();
        assert!(!texto.contains("acme") && !texto.contains("globex"), "{texto}");
    }

    // Un cliente sin repositorio propio no cae al general: no hay nada que enseñar.
    std::fs::remove_dir_all(espacios.join("acme")).unwrap();
    let s = servir(&p, &p.proyecto("acme-api"));
    let v = sync(&s);
    assert_eq!(v["configured"], false, "{v}");
    assert!(v["url"].is_null() && v["author"].is_null(), "{v}");
}
