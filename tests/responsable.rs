//! 1.7.0: la vista de responsable (`GET /api/overview` de `omoi serve`): por espacio, qué hay, quién
//! lo escribió y qué está desfasado o en pausa; desde un proyecto, solo su espacio, y con la misma
//! protección que el resto de la API.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Prueba {
    dir: PathBuf,
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

    fn sql(&self, consulta: &str, p: &[&dyn rusqlite::ToSql]) {
        let db = rusqlite::Connection::open(self.dir.join("o.db")).unwrap();
        db.execute(consulta, p).unwrap();
    }

    fn autor(&self, nombre: &str) {
        self.sql("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', ?1)", &[&nombre]);
    }
}

impl Drop for Prueba {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// El servidor se cierra aunque el test falle: uno huérfano deja bloqueado el binario en Windows.
struct Servidor(std::process::Child, u16);

impl Drop for Servidor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Servidor {
    fn pedir(&self, peticion: &str) -> String {
        for _ in 0..50 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.1)) {
                s.write_all(peticion.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                return String::from_utf8_lossy(&v).into_owned();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    }

    /// El JSON de `/api/overview`, con el token.
    fn panorama(&self, consulta: &str) -> serde_json::Value {
        let r = self.pedir(&format!(
            "GET /api/overview{consulta} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n",
            self.1
        ));
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
        let cuerpo = &r[r.find("\r\n\r\n").unwrap() + 4..];
        serde_json::from_str(cuerpo).unwrap_or_else(|e| panic!("{e}: {cuerpo}"))
    }
}

fn servir(p: &Prueba, carpeta: &Path, puerto: u16) -> Servidor {
    let hijo = p
        .orden(carpeta)
        .args(["serve", "--port", &puerto.to_string()])
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Servidor(hijo, puerto)
}

fn espacio<'a>(v: &'a serde_json::Value, clave: &str) -> &'a serde_json::Value {
    v["spaces"].as_array().unwrap().iter().find(|s| s["key"] == clave).unwrap_or_else(|| panic!("sin el espacio {clave:?}: {v}"))
}

#[test]
fn vista_de_responsable_por_espacio() {
    let p = Prueba { dir: std::env::temp_dir().join(format!("omoi-responsable-{}", std::process::id())) };
    let _ = std::fs::remove_dir_all(&p.dir);
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
    p.ok(&casa, &["space", "add", "acme"]);
    p.ok(&casa, &["space", "add", "globex"]);
    p.autor("Ana");
    guardar("acme-api", &["-k", "fact", "-t", "Acme usa Postgres 16", "--pinned", "Postgres 16 en RDS."]);
    guardar("acme-api", &["-k", "fact", "-t", "Acme API: despliegue", "La v0.2.0 publicada el 28-oct-2026 en staging.\nFalta publicar en producción."]);
    guardar("acme-api", &["-k", "fact", "-t", "Acme API: estado", "--tag", "status", "La 0.9.2 está publicada en producción."]);
    p.autor("Carlos");
    guardar("acme-web", &["-k", "decision", "-t", "Precios con IVA", "--tag", "open", "--source", "mcp:codex", "Asumo que con IVA hasta que se decida."]);
    guardar("acme-web", &["-k", "fact", "-t", "Acme web: límites viejos", "100 peticiones por minuto."]);
    p.autor("Lucía");
    guardar("globex", &["-k", "fact", "-t", "Globex es Flutter", "App en Flutter 3.24."]);
    p.autor("");
    guardar("notas", &["-k", "fact", "-t", "Idea de blog", "Escribir sobre memoria."]);
    for (e, n) in [("acme", "acme-api"), ("acme", "acme-web"), ("globex", "globex")] {
        let ruta = p.proyecto(n);
        p.ok(&casa, &["space", "assign", e, ruta.to_str().unwrap()]);
    }
    // Una que nadie toca desde hace un año y un proyecto en pausa (otro equipo dice que es de acme).
    p.sql("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-12 months') WHERE title = 'Acme web: límites viejos'", &[]);
    p.sql(
        r#"UPDATE project SET paused = '{"space":"acme","machine":"portatil-lucia","hlc":"0","at":"2026-11-05T09:00:00.000Z"}' WHERE name = 'globex'"#,
        &[],
    );

    // Desde la carpeta del usuario: todos los espacios, y al final los proyectos sin espacio.
    let puerto = 7000 + (std::process::id() % 300) as u16;
    let s = servir(&p, &casa, puerto);
    let sin = s.pedir(&format!("GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1:{puerto}\r\nConnection: close\r\n\r\n"));
    assert!(sin.starts_with("HTTP/1.1 401"), "sin token no: {sin}");
    let ajeno = s.pedir(&format!("GET /api/overview HTTP/1.1\r\nHost: malo.example:{puerto}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(ajeno.starts_with("HTTP/1.1 403"), "otro Host no: {ajeno}");
    let v = s.panorama("");
    assert_eq!(v["scope"], "all", "{v}");
    let claves: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(claves, ["acme", "globex", ""], "{v}");
    assert_eq!(v["global_memories"], 1, "lo global no es de ningún cliente: {v}");

    let acme = espacio(&v, "acme");
    assert_eq!(acme["memories"], 5, "{acme}");
    // Qué hay: los proyectos con su último status, lo fijado y las decisiones abiertas.
    let api = acme["projects"].as_array().unwrap().iter().find(|x| x["name"] == "acme-api").unwrap();
    assert_eq!(api["status"]["title"], "Acme API: estado", "{api}");
    assert_eq!(api["status"]["author"], "Ana", "{api}");
    assert_eq!(api["pinned"], 1, "{api}");
    assert_eq!(acme["pinned"]["count"], 1, "{acme}");
    assert_eq!(acme["open_decisions"]["items"][0]["title"], "Precios con IVA", "{acme}");
    // Quién lo escribió: por autor, con sus agentes.
    let autores = acme["authors"].as_array().unwrap();
    let ana = autores.iter().find(|a| a["author"] == "Ana").unwrap();
    assert_eq!(ana["memories"], 3, "{acme}");
    let carlos = autores.iter().find(|a| a["author"] == "Carlos").unwrap();
    assert_eq!(carlos["memories"], 2, "{acme}");
    assert_eq!(carlos["recent"], 1, "la del año pasado no cuenta como reciente: {carlos}");
    assert!(carlos["sources"].as_array().unwrap().iter().any(|f| f["source"] == "mcp:codex"), "{carlos}");
    assert!(acme["recent"].as_array().unwrap().iter().all(|m| m["title"] != "Globex es Flutter"), "nada de otro espacio: {acme}");
    // Qué está desfasado: lo de Revisar y lo que nadie toca.
    assert_eq!(acme["review"]["kinds"]["outdated"], 1, "{acme}");
    assert!(acme["review"]["items"].as_array().unwrap().iter().any(|a| a["title"] == "Acme API: despliegue"), "{acme}");
    assert_eq!(acme["untouched"]["count"], 1, "{acme}");
    assert_eq!(acme["untouched"]["items"][0]["title"], "Acme web: límites viejos", "{acme}");
    // Y lo que está en pausa, con las dos órdenes.
    let globex = espacio(&v, "globex");
    assert_eq!(globex["paused"][0]["keep"], "omoi projects keep globex", "{globex}");
    assert_eq!(globex["projects"][0]["paused"], true, "{globex}");
    assert_eq!(globex["authors"][0]["author"], "Lucía", "{globex}");
    let personal = espacio(&v, "");
    assert_eq!(personal["authors"][0]["author"], "", "sin autor: {personal}");
    // Con más meses, la del año pasado ya no cuenta como olvidada.
    assert_eq!(espacio(&s.panorama("?months=24"), "acme")["untouched"]["count"], 0);
    drop(s);

    // Desde un proyecto de acme: solo acme (todo el espacio, no solo ese proyecto).
    let s = servir(&p, &p.proyecto("acme-api"), puerto + 1);
    let v = s.panorama("");
    assert_eq!(v["scope"], "space", "{v}");
    assert_eq!(v["space"], "acme", "{v}");
    let claves: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(claves, ["acme"], "nada de globex ni de lo personal: {v}");
    assert_eq!(espacio(&v, "acme")["projects"].as_array().unwrap().len(), 2, "{v}");
    let texto = v.to_string();
    assert!(!texto.contains("Globex") && !texto.contains("Idea de blog") && !texto.contains("globex"), "{texto}");
    drop(s);

    // Desde un proyecto sin espacio: solo los proyectos sin espacio.
    let s = servir(&p, &p.proyecto("notas"), puerto + 2);
    let v = s.panorama("");
    let claves: Vec<&str> = v["spaces"].as_array().unwrap().iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(claves, [""], "{v}");
    assert!(!v.to_string().contains("Acme"), "{v}");
}
