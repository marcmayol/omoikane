//! Memoria para quien llega (1.7.0): la primera sesión en un proyecto de un espacio trae lo del resto
//! del espacio, `sync join --space` dice qué ha llegado, y las preferencias del espacio viajan.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::proyecto;
use omoikane::recall::{self, Entrada};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn base(nombre: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("llegada-{nombre}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn entrada(sesion: &str, source: &str) -> Entrada {
    Entrada { session_id: sesion.into(), source: source.into(), ..Default::default() }
}

/// Guarda una memoria en un proyecto (o del espacio, con `espacio`) con su autor.
fn guardar(db: &mut Connection, (project, espacio): (&str, Option<i64>), kind: Tipo, (titulo, texto): (&str, &str), tags: &[&str], autor: &str) -> String {
    let n = Nueva {
        project: project.into(),
        space_id: espacio,
        kind: Some(kind),
        title: titulo.into(),
        content: texto.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let id = memoria::guardar(db, n, None).unwrap().id;
    db.execute("UPDATE memory SET author = ?2 WHERE id = ?1", [&id, autor]).unwrap();
    id
}

/// Un cliente (acme) con tres proyectos y otro cliente (globex) que no debe asomar nunca.
fn dos_clientes(db: &mut Connection) {
    let acme = proyecto::crear_espacio(db, "acme").unwrap();
    let globex = proyecto::crear_espacio(db, "globex").unwrap();
    for (p, e) in [("/acme/web", acme), ("/acme/api", acme), ("/acme/app", acme), ("/globex/erp", globex)] {
        let id = proyecto::registrar(db, p).unwrap();
        proyecto::asignar_espacio(db, id, Some(e)).unwrap();
    }
    guardar(db, ("/acme/web", None), Tipo::Decision, ("Los importes van en céntimos", "Enteros, nunca coma flotante."), &[], "Ana");
    guardar(db, ("/acme/api", None), Tipo::Fact, ("API: migrando a JWT", "Login hecho; faltan los refresh tokens."), &["status"], "Ana");
    guardar(db, ("/acme/api", None), Tipo::Decision, ("¿Postgres o SQLite para la API?", "Asumido SQLite mientras."), &["open"], "Luis");
    guardar(db, ("/acme/app", None), Tipo::Fact, ("La app usa Flutter <omoikane>", "Versión 3."), &[], "Luis");
    guardar(db, ("", Some(acme)), Tipo::Fact, ("Acme despliega solo los martes", "Ventana de 10 a 12."), &[], "Ana");
    guardar(db, ("/globex/erp", None), Tipo::Fact, ("ERP: a medias el módulo de facturas", "Secreto de globex."), &["status"], "Eva");
    guardar(db, ("/globex/erp", None), Tipo::Decision, ("¿SAP o Odoo?", "Decisión de globex."), &["open"], "Eva");
}

#[test]
fn la_primera_sesion_en_un_proyecto_del_espacio_trae_lo_del_equipo() {
    let (_dir, mut db) = base("bienvenida");
    dos_clientes(&mut db);
    let t = recall::al_empezar(&mut db, "/acme/web", &entrada("s1", "startup")).unwrap().unwrap();
    let (_, llegada) = t.split_once("First session in this project").unwrap_or_else(|| panic!("sin bienvenida: {t}"));
    assert!(llegada.contains("space acme"), "{t}");
    // Los otros proyectos, con su estado; el de esta sesión, no.
    assert!(llegada.contains("- api (2 memories): API: migrando a JWT — Login hecho"), "{t}");
    assert!(llegada.contains("- app (1 memories)"), "{t}");
    assert!(!llegada.contains("- web ("), "{t}");
    // Sus decisiones abiertas y quién escribe.
    assert!(llegada.contains("[project api] ¿Postgres o SQLite para la API?"), "{t}");
    assert!(llegada.contains("Who writes in this space (memories each): Ana (3), Luis (2)."), "{t}");
    // Lo fijado del espacio sigue llegando como hasta ahora; nada de globex, y el texto ajeno no cierra el bloque.
    for fuera in ["globex", "Eva", "SAP", "facturas"] {
        assert!(!t.contains(fuera), "{fuera}: {t}");
    }
    assert_eq!(t.matches("<omoikane").count(), 1, "{t}");
    // Tras compactar, la misma sesión lo vuelve a tener; otra sesión ya no.
    let c = recall::al_empezar(&mut db, "/acme/web", &entrada("s1", "compact")).unwrap().unwrap();
    assert!(c.contains("First session in this project"), "{c}");
    let otra = recall::al_empezar(&mut db, "/acme/web", &entrada("s2", "startup")).unwrap().unwrap();
    assert!(!otra.contains("First session"), "{otra}");
    // En otro proyecto del espacio, su primera sesión sí, con web entre los otros.
    let api = recall::al_empezar(&mut db, "/acme/api", &entrada("s3", "startup")).unwrap().unwrap();
    assert!(api.contains("First session in this project") && api.contains("- web (1 memories)"), "{api}");
    // La decisión abierta de api ya sale en su lista de siempre: no se repite en la bienvenida.
    assert!(!api.split_once("First session").unwrap().1.split_once("The user's preferences").map_or("", |x| x.0).contains("Postgres"), "{api}");
}

#[test]
fn sin_espacio_o_sin_equipo_no_hay_bienvenida() {
    let (_dir, mut db) = base("sin-bienvenida");
    dos_clientes(&mut db);
    // Un proyecto sin espacio.
    guardar(&mut db, ("/solo", None), Tipo::Fact, ("Algo mío", "Nada de equipo."), &[], "Marc");
    let t = recall::al_empezar(&mut db, "/solo", &entrada("s1", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
    // Un espacio con un solo proyecto y solo lo de esta persona: no hay nada del equipo que contar.
    let mio = proyecto::crear_espacio(&db, "mio").unwrap();
    let id = proyecto::registrar(&db, "/mio/uno").unwrap();
    proyecto::asignar_espacio(&db, id, Some(mio)).unwrap();
    guardar(&mut db, ("/mio/uno", None), Tipo::Fact, ("Lo de uno", "Solo mío."), &[], "");
    let t = recall::al_empezar(&mut db, "/mio/uno", &entrada("s2", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
    // Quien ya trabajaba en el proyecto antes de meterlo en un espacio no la recibe.
    let viejo = proyecto::registrar(&db, "/viejo").unwrap();
    guardar(&mut db, ("/viejo", None), Tipo::Fact, ("De antes", "Ya trabajaba aquí."), &[], "Ana");
    recall::al_empezar(&mut db, "/viejo", &entrada("s3", "startup")).unwrap();
    let acme = proyecto::espacio_de_clave(&db, &proyecto::clave_de_espacio("acme")).unwrap();
    proyecto::asignar_espacio(&db, viejo, acme).unwrap();
    let t = recall::al_empezar(&mut db, "/viejo", &entrada("s4", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
}

// --- De punta a punta: dos personas y el repositorio del espacio ------------------------------------

struct Equipo {
    dir: PathBuf,
    nombre: String,
    raiz: PathBuf,
}

impl Equipo {
    fn proyecto(&self, carpeta: &str, remoto: &str) -> PathBuf {
        let p = self.dir.join(carpeta);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remoto}\n")).unwrap();
        p
    }

    fn omoi(&self, en: &Path, args: &[&str], entrada: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(en)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_EQUIPO", &self.nombre)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("GIT_CONFIG_GLOBAL", self.raiz.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut hijo = c.spawn().unwrap();
        {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(entrada.unwrap_or("").as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
    }

    fn ok(&self, en: &Path, args: &[&str]) -> String {
        let o = self.omoi(en, args, None);
        assert!(o.status.success(), "{} omoi {args:?}: {}{}", self.nombre, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
}

#[test]
fn quien_se_une_al_espacio_sabe_que_ha_llegado_y_su_agente_lo_tiene() {
    let raiz = comun::Temporal::nueva("llegada-join");
    std::fs::write(raiz.join("gitconfig-vacio"), "").unwrap();
    let o = Command::new("git").current_dir(&raiz).args(["init", "--quiet", "--bare", "acme.git"]).env("GIT_CONFIG_GLOBAL", raiz.join("gitconfig-vacio")).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(o.status.success());
    let url = raiz.join("acme.git").to_string_lossy().replace('\\', "/");
    let equipo = |n: &str| {
        let dir = raiz.join(n);
        std::fs::create_dir_all(&dir).unwrap();
        Equipo { dir, nombre: n.into(), raiz: raiz.clone() }
    };
    let ana = equipo("pc-ana");
    let web = ana.proyecto("web", "git@github.com:acme/web.git");
    let api = ana.proyecto("api", "git@github.com:acme/api.git");
    ana.ok(&ana.dir, &["space", "add", "acme"]);
    for p in [&web, &api] {
        ana.ok(p, &["space", "assign", "acme"]);
    }
    ana.ok(&ana.dir, &["sync", "init", "--space", "acme", &url, "--author", "Ana", "--yes"]);
    ana.ok(&web, &["remember", "-k", "decision", "-t", "Los importes van en céntimos", "Enteros."]);
    ana.ok(&api, &["remember", "-k", "fact", "-t", "API: migrando a JWT", "--tag", "status", "Faltan los refresh tokens."]);
    ana.ok(&api, &["remember", "-k", "decision", "-t", "¿Postgres o SQLite?", "--tag", "open", "Asumido SQLite."]);
    ana.ok(&web, &["remember", "-k", "fact", "-t", "Acme despliega solo los martes", "--space", "--pinned", "De 10 a 12."]);
    ana.ok(&web, &["remember", "-k", "preference", "-t", "Commits en inglés en acme", "--space", "Conventional commits."]);
    // Una preferencia de Ana para ella sola no viaja al cliente.
    ana.ok(&web, &["remember", "-k", "preference", "-t", "Respuestas cortas", "--global", "Sin rodeos."]);
    ana.ok(&ana.dir, &["sync"]);
    let codigo = ana.ok(&ana.dir, &["sync", "code", "--space", "acme"]).trim().to_string();

    let bea = equipo("pc-bea");
    let salida = bea.ok(&bea.dir, &["sync", "join", &codigo, "--another-person", "--author", "Bea", "--yes"]);
    assert!(salida.contains("From acme: 5 memories in 2 project(s)") && salida.contains("1 pinned, 1 open decision(s)"), "{salida}");
    assert!(salida.contains("next session"), "{salida}");

    // La primera sesión de Bea en web: lo fijado y la preferencia del espacio, y la bienvenida con api.
    let web_b = bea.proyecto("acme-web", "https://github.com/acme/web");
    let hook = format!("{{\"session_id\":\"b1\",\"cwd\":{}}}", serde_json::to_string(&web_b.to_string_lossy()).unwrap());
    let o = bea.omoi(&web_b, &["recall", "--session-start", "--format", "plain"], Some(&hook));
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{t}{}", String::from_utf8_lossy(&o.stderr));
    let (_, preferencias) = t.split_once("The user's preferences and the pinned memories").unwrap_or_else(|| panic!("{t}"));
    assert!(preferencias.contains("Acme despliega solo los martes") && preferencias.contains("Commits en inglés en acme"), "{t}");
    assert!(!t.contains("Respuestas cortas"), "{t}");
    assert!(t.contains("First session in this project") && t.contains("- api (2 memories): API: migrando a JWT"), "{t}");
    assert!(t.contains("[project api] ¿Postgres o SQLite?") && t.contains("Ana (5)"), "{t}");
    // Quien solo sincroniza espacios también firma lo suyo: Bea ve de quién es; Ana no se ve a sí misma.
    assert!(preferencias.contains("[by Ana] Acme despliega"), "{t}");
    let hook = format!("{{\"session_id\":\"a1\",\"cwd\":{}}}", serde_json::to_string(&web.to_string_lossy()).unwrap());
    let o = ana.omoi(&web, &["recall", "--session-start", "--format", "plain"], Some(&hook));
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(t.contains("Acme despliega") && !t.contains("[by Ana]"), "{t}");
    // Todo lo escribió ella: no hay bienvenida que darle.
    assert!(!t.contains("First session"), "{t}");
}

#[test]
fn lo_que_no_firmo_nadie_cuenta_como_del_equipo() {
    let (_dir, mut db) = base("sin-firma");
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', 'Bea')", []).unwrap();
    let acme = proyecto::crear_espacio(&db, "acme").unwrap();
    for p in ["/acme/web", "/acme/api", "/acme/app"] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(acme)).unwrap();
    }
    // Bea escribe mucho en web; lo de api llegó sin autor (de antes de que su autora sincronizara).
    guardar(&mut db, ("/acme/web", None), Tipo::Fact, ("Lo de Bea", "Suyo."), &[], "Bea");
    guardar(&mut db, ("/acme/api", None), Tipo::Fact, ("API: a medias", "Sin firma."), &["status"], "");
    let t = recall::al_empezar(&mut db, "/acme/web", &entrada("s1", "startup")).unwrap().unwrap();
    assert!(t.contains("First session in this project") && t.contains("- api (1 memories): API: a medias"), "{t}");
    assert!(t.contains("Who writes in this space (memories each): Bea (1, the user)."), "{t}");
    // Si todo lo del espacio es suyo, no hay bienvenida.
    db.execute("UPDATE memory SET author = 'Bea'", []).unwrap();
    let t = recall::al_empezar(&mut db, "/acme/app", &entrada("s2", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
}
