//! 0.20.0: el esquema 10, el que congela la 1.0. Proyectos y espacios como tablas (y la migración
//! desde una base del esquema 9 como las que hay), espacios que aíslan clientes, clones y worktrees
//! que son un mismo proyecto, carpetas movidas, `lector_minimo`, veredictos por reloj lógico y tipos
//! que esta versión no conoce.

use omoikane::buscar::{self, Opciones};
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::{db, proyecto, veredictos};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

fn carpeta(nombre: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omoi-esquema-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Un repositorio git de mentira con su `origin` (lo que lee `sync::clave`).
fn repo(dir: &Path, url: &str) -> String {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n")).unwrap();
    proyecto::de(dir).unwrap()
}

/// Un worktree de `principal` en `dir` (`.git` es un fichero que apunta al repositorio).
fn worktree(principal: &Path, dir: &Path) -> String {
    let interno = principal.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&interno).unwrap();
    std::fs::write(interno.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(".git"), format!("gitdir: {}\n", interno.display())).unwrap();
    proyecto::de(dir).unwrap()
}

fn suelta(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    proyecto::de(dir).unwrap()
}

fn guardar(db: &mut Connection, project: &str, kind: Tipo, title: &str, content: &str) -> String {
    let n = Nueva { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() };
    memoria::guardar(db, n, None).unwrap().id
}

fn titulos(v: impl IntoIterator<Item = memoria::Memoria>) -> Vec<String> {
    let mut t: Vec<String> = v.into_iter().map(|m| m.title).collect();
    t.sort();
    t
}

fn uno(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

// --- Migración 9 → 10 ------------------------------------------------------------------------------

/// Una memoria escrita como la escribía la 0.19 (esquema 9): con `project` como ruta, su índice FTS y
/// su vector.
#[allow(clippy::too_many_arguments)]
fn memoria_9(db: &Connection, n: i64, id: &str, project: &str, kind: &str, title: &str, content: &str, hlc: &str) {
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
fn migra_una_base_del_esquema_9_sin_perder_nada() {
    let dir = carpeta("migra9");
    let web_a = repo(&dir.join("web"), "git@github.com:acme/web.git");
    let web_b = repo(&dir.join("web-clon"), "https://github.com/acme/web");
    let notas = suelta(&dir.join("notas"));
    let ido = dir.join("ya-no-existe").display().to_string();
    let ajeno = "/home/ana/app";
    let ruta = dir.join("o.db");
    let ids: Vec<String> = (1..=8).map(|i| format!("01J{i:0>23}")).collect();
    {
        let db = db::abrir_en_esquema(&ruta, 9).unwrap();
        memoria_9(&db, 1, &ids[0], &web_a, "fact", "Despliegue de la web", "Se publica con el script zafiro.", "1793181600000-0000-");
        memoria_9(&db, 2, &ids[1], &web_b, "gotcha", "El clon tiene otra caché", "La caché zafiro del clon.", "1793181600001-0002-pc-a");
        memoria_9(&db, 3, &ids[2], &notas, "decision", "Notas en markdown", "Todo en markdown.", "1793181600002-0000000000-pc-a");
        memoria_9(&db, 4, &ids[3], &ido, "fact", "Proyecto que se movió", "Su carpeta ya no está.", "1793181600003-0000000000-pc-a");
        memoria_9(&db, 5, &ids[4], ajeno, "fact", "La app de Ana", "Viene de otro equipo.", "1793181600004-0000000000-pc-b");
        memoria_9(&db, 6, &ids[5], "", "preference", "Respuestas cortas", "Sin rodeos.", "1793181600005-0000000000-pc-a");
        memoria_9(&db, 7, &ids[6], "", "procedure", "Publicar", "when: publicar\n1. tag", "1793181600006-0000000000-pc-a");
        memoria_9(&db, 8, &ids[7], &web_a, "fact", "Otra de la web", "Más del despliegue zafiro.", "1793181600007-0000000000-pc-a");
        db.execute_batch(&format!(
            "INSERT INTO remote_project(path, key) VALUES ('{ajeno}', 'git:github.com/ana/app');
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
            // En el JSON, cada barra invertida va doble.
            web = web_a.replace('\\', "\\\\"),
        ))
        .unwrap();
    }
    let db = db::abrir(&ruta).unwrap();
    assert!(Path::new(&format!("{}.antes-del-esquema-10", ruta.display())).exists(), "copia antes de migrar");
    assert_eq!(uno(&db, "PRAGMA user_version"), db::VERSION_ESQUEMA);
    let ok: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    assert_eq!(ok, "ok");
    assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    // Las mismas memorias, con el mismo id y el mismo `n` (así FTS y vectores siguen valiendo).
    assert_eq!(uno(&db, "SELECT count(*) FROM memory"), 8);
    for (i, id) in ids.iter().enumerate() {
        let n: i64 = db.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
        assert_eq!(n, i as i64 + 1);
    }
    assert_eq!(uno(&db, "SELECT count(*) FROM memory_vec v JOIN memory m ON m.n = v.n"), 8);
    assert_eq!(uno(&db, "SELECT count(*) FROM memory_version"), 1);
    assert_eq!(uno(&db, "SELECT count(*) FROM memory_path"), 1);
    assert_eq!(uno(&db, "SELECT count(*) FROM forgotten"), 1);
    assert_eq!(uno(&db, "SELECT count(*) FROM tombstone"), 1);
    assert_eq!(memoria::historial(&db, &ids[0]).unwrap()[0].title, "Antes");
    // La búsqueda por palabras, igual.
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 10 };
    let mut db = db;
    assert_eq!(buscar::buscar(&mut db, "zafiro", &o, None).unwrap().len(), 3);
    // Proyectos: los dos clones con el mismo `origin`, uno; la carpeta sin git, el suyo; la que ya no
    // está, marcada; la que llegó de otro equipo, con su clave y como ruta ajena.
    fn pid_de(db: &Connection, id: &str) -> i64 {
        db.query_row("SELECT project_id FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap()
    }
    let pid = |id: &str| -> i64 { pid_de(&db, id) };
    assert_eq!(pid(&ids[0]), pid(&ids[1]), "clones del mismo repositorio");
    assert_eq!(pid(&ids[0]), pid(&ids[7]));
    assert_ne!(pid(&ids[0]), pid(&ids[2]));
    assert_eq!(pid(&ids[5]), 0, "global");
    let clave: String = db.query_row("SELECT key FROM project WHERE id = ?1", [pid(&ids[0])], |f| f.get(0)).unwrap();
    assert_eq!(clave, "git:github.com/acme/web");
    let (clave_ajena, ajena): (String, bool) = db
        .query_row("SELECT p.key, pp.is_foreign FROM project p JOIN project_path pp ON pp.project_id = p.id WHERE p.id = ?1", [pid(&ids[4])], |f| Ok((f.get(0)?, f.get(1)?)))
        .unwrap();
    assert_eq!((clave_ajena.as_str(), ajena), ("git:github.com/ana/app", true));
    let lista = proyecto::lista(&db, None, None, 50).unwrap();
    assert!(lista.iter().any(|p| p.path == ido && p.gone), "{lista:?}");
    assert!(!lista.iter().any(|p| p.path == ajeno && p.gone), "lo de otro equipo no «ya no está»");
    assert_eq!(memoria::obtener(&db, &ids[0]).unwrap().project, web_a);
    // Veredicto, que sigue valiendo; sellos al formato de ahora (comparar como texto = comparar sellos).
    assert_eq!(veredictos::parejas(&db).unwrap().len(), 1);
    assert_eq!(uno(&db, "SELECT count(*) FROM memory WHERE hlc != '' AND length(hlc) - length(replace(hlc, '-', '')) >= 2 AND substr(hlc, 15, 10) GLOB '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]'"), 8);
    let sello: String = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [&ids[1]], |f| f.get(0)).unwrap();
    assert_eq!(sello, "1793181600001-0000000002-pc-a");
    let sello: String = db.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).unwrap();
    assert_eq!(sello, "1793260800000-0000000005-pc-a");
    // `remote_project` ya no está; `review` sin `dismissed_at`; `lector_minimo`.
    assert!(db.query_row("SELECT count(*) FROM remote_project", [], |f| f.get::<_, i64>(0)).is_err());
    assert!(db.query_row("SELECT count(*) FROM review WHERE dismissed_at IS NULL", [], |f| f.get::<_, i64>(0)).is_err());
    let minimo: String = db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |f| f.get(0)).unwrap();
    assert_eq!(minimo, "10");
    // `recall_log` con el proyecto de su sesión.
    assert_eq!(uno(&db, "SELECT project_id FROM recall_log WHERE session = 's1'"), pid(&ids[0]));
    // La papelera devuelve la olvidada a su proyecto (por la ruta: es de antes del esquema 10).
    let web = pid(&ids[0]);
    let m = memoria::restaurar(&mut db, "01JZZZZZZZZZZZZZZZZZZZZZZZ", None).unwrap();
    assert_eq!(m.project_id, web);
    // Y se sigue escribiendo como siempre.
    let nueva = guardar(&mut db, &web_b, Tipo::Fact, "Nueva tras migrar", "Funciona.");
    assert_eq!(pid_de(&db, &nueva), web);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn una_base_de_un_esquema_mayor_se_abre_si_su_lector_minimo_lo_permite() {
    let dir = carpeta("lector");
    let ruta = dir.join("o.db");
    {
        let mut db = db::abrir(&ruta).unwrap();
        guardar(&mut db, "", Tipo::Fact, "Una", "Algo.");
        // Una versión futura que solo añade: esquema 11, legible desde el 10.
        db.execute_batch("PRAGMA user_version = 11; CREATE TABLE futura (x); INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', '10');").unwrap();
    }
    {
        let db = db::abrir(&ruta).unwrap();
        assert_eq!(memoria::recientes(&db, &Ambito::Todos, None, 10).unwrap().len(), 1);
        assert_eq!(uno(&db, "PRAGMA user_version"), 11, "no la baja ni la toca");
    }
    {
        // Una que rompe: legible solo desde el 12.
        let db = Connection::open(&ruta).unwrap();
        db.execute_batch("PRAGMA user_version = 12; UPDATE meta SET v = '12' WHERE k = 'lector_minimo';").unwrap();
    }
    let e = db::abrir(&ruta).expect_err("no se abre");
    assert!(format!("{e:#}").contains("12"), "{e:#}");
    {
        // Una de antes del esquema 10 con un esquema mayor (sin `lector_minimo`): tampoco.
        let db = Connection::open(&ruta).unwrap();
        db.execute_batch("DELETE FROM meta WHERE k = 'lector_minimo'; PRAGMA user_version = 11;").unwrap();
    }
    assert!(db::abrir(&ruta).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

// --- Espacios ------------------------------------------------------------------------------------

struct Clientes {
    dir: PathBuf,
    db: Connection,
    a_web: String,
    a_app: String,
    b_web: String,
    suelto: String,
}

/// Dos clientes: Acme (web y app) y Globex (web), un proyecto sin espacio, y memorias de cada uno, de
/// cada espacio y globales, todas con la palabra «zafiro».
fn clientes(nombre: &str) -> Clientes {
    let dir = carpeta(nombre);
    let a_web = suelta(&dir.join("acme-web"));
    let a_app = suelta(&dir.join("acme-app"));
    let b_web = suelta(&dir.join("globex-web"));
    let suelto = suelta(&dir.join("personal"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    guardar(&mut db, &a_web, Tipo::Fact, "Acme web: puerto zafiro", "El servidor zafiro de Acme escucha en el 7741.");
    guardar(&mut db, &a_app, Tipo::Fact, "Acme app: build zafiro", "La app de Acme compila con el perfil zafiro.");
    guardar(&mut db, &b_web, Tipo::Fact, "Globex web: puerto zafiro", "El servidor zafiro de Globex escucha en el 9931.");
    guardar(&mut db, &suelto, Tipo::Fact, "Personal: zafiro", "Mi script zafiro de copias.");
    guardar(&mut db, "", Tipo::Preference, "Global: respuestas zafiro", "Respuestas cortas, también con zafiro.");
    let acme = proyecto::crear_espacio(&db, "Acme").unwrap();
    let globex = proyecto::crear_espacio(&db, "Globex").unwrap();
    for (p, s) in [(&a_web, acme), (&a_app, acme), (&b_web, globex)] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(s)).unwrap();
    }
    for (s, t) in [(acme, "Acme espacio: VPN zafiro"), (globex, "Globex espacio: VPN zafiro")] {
        let n = Nueva { space_id: Some(s), kind: Some(Tipo::Gotcha), title: t.into(), content: "La VPN zafiro del cliente.".into(), ..Default::default() };
        memoria::guardar(&mut db, n, None).unwrap();
    }
    Clientes { dir, db, a_web, a_app, b_web, suelto }
}

#[test]
fn desde_un_cliente_no_se_ve_nada_de_otro() {
    let Clientes { dir, mut db, a_web, a_app, b_web, suelto } = clientes("aislar");
    let desde_a = Ambito::de(&db, &a_web).unwrap();
    let o = Opciones { ambito: desde_a, kind: None, tags: vec![], limite: 50 };
    let vistas = titulos(buscar::buscar(&mut db, "zafiro", &o, None).unwrap().into_iter().map(|r| r.memoria));
    assert_eq!(vistas, ["Acme espacio: VPN zafiro", "Acme web: puerto zafiro", "Global: respuestas zafiro"]);
    assert_eq!(titulos(memoria::recientes(&db, &desde_a, None, 50).unwrap()), vistas);
    // «Todos los proyectos» desde un proyecto de Acme: los de Acme, nunca los de Globex.
    let amplio = desde_a.amplio();
    let o = Opciones { ambito: amplio, kind: None, tags: vec![], limite: 50 };
    let vistas = titulos(buscar::buscar(&mut db, "zafiro", &o, None).unwrap().into_iter().map(|r| r.memoria));
    assert_eq!(vistas, ["Acme app: build zafiro", "Acme espacio: VPN zafiro", "Acme web: puerto zafiro", "Global: respuestas zafiro"]);
    // El contexto (preferencias y fijadas) y la lista de proyectos.
    assert!(memoria::contexto(&db, &desde_a, 30).unwrap().iter().all(|m| !m.title.contains("Globex")));
    // Desde un proyecto sin espacio: ni Acme ni Globex (solo lo suyo, lo de otros sin espacio y lo global).
    let desde_suelto = Ambito::de(&db, &suelto).unwrap().amplio();
    assert_eq!(titulos(memoria::recientes(&db, &desde_suelto, None, 50).unwrap()), ["Global: respuestas zafiro", "Personal: zafiro"]);
    // Desde la carpeta general (la del usuario), todo.
    assert_eq!(memoria::recientes(&db, &Ambito::Todos, None, 50).unwrap().len(), 7);
    // El recall: aunque la memoria de Globex encaja mejor con el mensaje, no llega.
    let e = Entrada { session_id: "s".into(), prompt: "¿en qué puerto escucha el servidor zafiro de Globex?".into(), ..Default::default() };
    let r = recall::al_preguntar(&mut db, &a_web, &e, None).unwrap().unwrap_or_default();
    assert!(!r.contains("Globex web") && !r.contains("9931"), "{r}");
    let e = Entrada { session_id: "s2".into(), ..Default::default() };
    let r = recall::al_empezar(&mut db, &a_app, &e).unwrap().unwrap_or_default();
    assert!(!r.contains("Globex"), "{r}");
    assert!(r.contains("Acme"), "{r}");
    // Y lo de Globex sí se ve desde Globex.
    let desde_b = Ambito::de(&db, &b_web).unwrap();
    assert!(titulos(memoria::recientes(&db, &desde_b, None, 50).unwrap()).iter().any(|t| t.starts_with("Globex web")));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Lo mismo por las herramientas MCP (`search`, `recent`, `context`), que es lo que ve el agente,
/// también pidiendo otro proyecto u otro espacio a propósito.
#[test]
fn por_mcp_tampoco_se_cruza_de_espacio() {
    let Clientes { dir, db, a_web, b_web, .. } = clientes("aislar-mcp");
    drop(db);
    let ruta = dir.join("o.db");
    let llamar = |desde: &str, nombre: &str, args: serde_json::Value| -> (String, bool) {
        omoikane::mcp::herramienta(ruta.clone(), Some(PathBuf::from(desde)), Some(nombre), &args.to_string(), "test").unwrap()
    };
    let (t, error) = llamar(&a_web, "search", serde_json::json!({ "query": "zafiro", "all_projects": true }));
    assert!(!error && t.contains("Acme app") && !t.contains("Globex"), "{t}");
    let (t, _) = llamar(&a_web, "recent", serde_json::json!({ "limit": 50 }));
    assert!(t.contains("Acme web") && !t.contains("Globex"), "{t}");
    let (t, error) = llamar(&a_web, "search", serde_json::json!({ "query": "zafiro", "project": "globex-web" }));
    assert!(error && !t.contains("9931"), "{t}");
    let (t, error) = llamar(&a_web, "recent", serde_json::json!({ "space": "globex" }));
    assert!(error && !t.contains("9931"), "{t}");
    let (t, _) = llamar(&a_web, "context", serde_json::json!({}));
    let c: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(c["project"]["space"], "acme", "{c}");
    assert!(c["project"]["key"].as_str().unwrap().starts_with("dir:"), "{c}");
    let nombres: Vec<&str> = c["projects"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(nombres.contains(&"acme-app") && !nombres.contains(&"globex-web"), "{nombres:?}");
    assert!(!t.contains("Globex"), "{t}");
    // Desde Acme, otro proyecto de Acme por su nombre (o un trozo) sí.
    let (t, error) = llamar(&a_web, "search", serde_json::json!({ "query": "zafiro", "project": "app" }));
    assert!(!error && t.contains("Acme app"), "{t}");
    // Guardar a nivel de espacio con `space: true`, y verlo desde el otro proyecto del espacio.
    let (t, error) = llamar(&a_web, "remember", serde_json::json!({ "title": "Acme: el staging es lento", "content": "Lleva dos réplicas.", "kind": "fact", "space": true, "force": true }));
    assert!(!error && t.contains("space acme"), "{t}");
    let (t, _) = llamar(&dir.join("acme-app").display().to_string(), "recent", serde_json::json!({}));
    assert!(t.contains("staging"), "{t}");
    let (t, _) = llamar(&b_web, "recent", serde_json::json!({ "limit": 50 }));
    assert!(!t.contains("staging") && !t.contains("Acme"), "{t}");
    // Un nombre que encaja con varios: error con la lista.
    let (t, error) = llamar(&dir.display().to_string(), "search", serde_json::json!({ "query": "zafiro", "project": "web" }));
    assert!(error && t.contains("acme-web") && t.contains("globex-web"), "{t}");
    let _ = std::fs::remove_dir_all(dir);
}

// --- Clones, worktrees y carpetas movidas ---------------------------------------------------------

#[test]
fn dos_clones_y_un_worktree_son_un_proyecto() {
    let dir = carpeta("clones");
    let a = repo(&dir.join("web"), "git@github.com:acme/web.git");
    let b = repo(&dir.join("web-2"), "https://github.com/acme/web.git");
    let w = worktree(&dir.join("web"), &dir.join("web-rama"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    guardar(&mut db, &a, Tipo::Gotcha, "La caché del build", "Borrar target al cambiar de rama.");
    let (pa, pb, pw) = (proyecto::registrar(&db, &a).unwrap(), proyecto::registrar(&db, &b).unwrap(), proyecto::registrar(&db, &w).unwrap());
    assert_eq!((pa, pa), (pb, pw));
    assert_eq!(uno(&db, "SELECT count(*) FROM project_path"), 3);
    // Desde cualquiera de las tres se ve, y no sale como «de otro proyecto».
    for desde in [&a, &b, &w] {
        assert_eq!(memoria::recientes(&db, &Ambito::de(&db, desde).unwrap(), None, 10).unwrap().len(), 1);
    }
    let e = Entrada { session_id: "s".into(), ..Default::default() };
    let r = recall::al_empezar(&mut db, &w, &e).unwrap().unwrap();
    assert!(r.contains("La caché del build") && !r.contains("[project"), "{r}");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn una_carpeta_movida_sigue_siendo_su_proyecto() {
    let dir = carpeta("movida");
    let vieja = repo(&dir.join("app"), "git@github.com:acme/app.git");
    let sin_git = suelta(&dir.join("notas"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let id = guardar(&mut db, &vieja, Tipo::Fact, "La app usa Kotlin", "Y Compose.");
    let id_notas = guardar(&mut db, &sin_git, Tipo::Fact, "Notas en markdown", "Todo en .md.");
    let clave: String = db.query_row("SELECT key FROM project", [], |f| f.get(0)).unwrap_or_default();
    let _ = clave;
    // Con `origin`: se mueve la carpeta y el proyecto la sigue solo (misma clave, la ruta principal cambia).
    std::fs::rename(dir.join("app"), dir.join("app-nueva")).unwrap();
    let nueva = proyecto::de(&dir.join("app-nueva")).unwrap();
    assert_eq!(memoria::recientes(&db, &Ambito::de(&db, &nueva).unwrap(), None, 10).unwrap().len(), 1, "ya se ve antes de apuntarla");
    proyecto::registrar(&db, &nueva).unwrap();
    assert_eq!(memoria::obtener(&db, &id).unwrap().project, nueva);
    // Sin `origin`: una carpeta nueva con el mismo nombre no se junta sola (podría ser otra cosa); la
    // vieja sale como «ya no está», y `projects move` la lleva.
    std::fs::rename(dir.join("notas"), dir.join("notas-2026")).unwrap();
    std::fs::create_dir_all(dir.join("otra").join("notas")).unwrap();
    let otra = proyecto::de(&dir.join("otra").join("notas")).unwrap();
    let id_otra = proyecto::registrar(&db, &otra).unwrap();
    assert_ne!(id_otra, memoria::obtener(&db, &id_notas).unwrap().project_id, "mismo nombre, otra carpeta");
    assert!(proyecto::lista(&db, None, None, 50).unwrap().iter().any(|p| p.path == sin_git && p.gone));
    let movida = proyecto::de(&dir.join("notas-2026")).unwrap();
    let pid = memoria::obtener(&db, &id_notas).unwrap().project_id;
    proyecto::mover(&mut db, pid, &movida).unwrap();
    let m = memoria::obtener(&db, &id_notas).unwrap();
    assert_eq!((m.project_id, m.project.as_str()), (pid, movida.as_str()));
    assert!(!proyecto::lista(&db, None, None, 50).unwrap().iter().any(|p| p.gone));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fundir_dos_proyectos_conserva_ids_y_deja_el_estado_mas_nuevo() {
    let dir = carpeta("fundir");
    let a = suelta(&dir.join("viejo"));
    let b = suelta(&dir.join("nuevo"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let ea = memoria::guardar(&mut db, Nueva { project: a.clone(), kind: Some(Tipo::Fact), title: "Estado viejo".into(), content: "Iba por la 1.0.".into(), tags: vec!["status".into()], ..Default::default() }, None).unwrap().id;
    let otra = guardar(&mut db, &a, Tipo::Fact, "Una del viejo", "Algo.");
    memoria::actualizar(&mut db, &otra, Cambios { content: Some("Algo más.".into()), ..Default::default() }, None).unwrap();
    let eb = memoria::guardar(&mut db, Nueva { project: b.clone(), kind: Some(Tipo::Fact), title: "Estado nuevo".into(), content: "Voy por la 2.0.".into(), tags: vec!["status".into()], ..Default::default() }, None).unwrap().id;
    let (pa, pb) = (proyecto::registrar(&db, &a).unwrap(), proyecto::registrar(&db, &b).unwrap());
    assert_eq!(proyecto::fundir(&mut db, pa, pb).unwrap(), 2);
    for id in [&ea, &otra, &eb] {
        assert_eq!(memoria::obtener(&db, id).unwrap().project_id, pb);
    }
    assert_eq!(memoria::historial(&db, &otra).unwrap().len(), 1, "con su historial");
    assert_eq!(veredictos::ultima_de_cadena(&db, &ea).unwrap().map(|x| x.0), Some(eb.clone()), "el estado viejo, sustituido por el nuevo");
    assert_eq!(proyecto::resolver(&db, &a).unwrap().map(|x| x.0), Some(pb), "la carpeta vieja ya es del nuevo");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

// --- Veredictos por reloj lógico y tipos desconocidos ----------------------------------------------

/// Un equipo con la hora 30 minutos adelantada cambia una memoria; aquí se juzga después. Comparando
/// relojes de pared (`updated_at` frente a `at`), el veredicto no valía hasta pasada media hora.
#[test]
fn un_veredicto_vale_aunque_los_relojes_de_pared_esten_descuadrados() {
    let dir = carpeta("relojes");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let a = guardar(&mut db, "/p", Tipo::Fact, "El puerto es el 8080", "Del servidor.");
    let b = guardar(&mut db, "/p", Tipo::Fact, "El puerto es el 9090", "Del servidor.");
    // Lo que deja la sincronización: el cambio del otro equipo con su hora (adelantada) y su sello.
    let futuro_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 30 * 60 * 1000;
    let sello = omoikane::reloj::formato(futuro_ms, 0, "pc-adelantado");
    db.execute(
        "UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+30 minutes'), hlc = ?2, content_hlc = ?2 WHERE id = ?1",
        params![a, sello],
    )
    .unwrap();
    omoikane::reloj::recibir(&db, &sello).unwrap();
    veredictos::poner(&db, &a, &b, veredictos::SUSTITUIDA, "la nueva", "user").unwrap();
    assert!(veredictos::parejas(&db).unwrap().contains_key(&veredictos::pareja(&a, &b)), "vale ya");
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap().map(|x| x.0), Some(b.clone()));
    let (at, cambio): (String, String) = db.query_row("SELECT v.at, m.updated_at FROM verdict v JOIN memory m ON m.id = v.memory_id", [], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
    assert!(cambio > at, "con la regla de antes no valdría: {cambio} > {at}");
    // Fijarla o etiquetarla no lo caduca; cambiar el texto, sí.
    memoria::actualizar(&mut db, &a, Cambios { pinned: Some(true), tags: Some(vec!["puertos".into()]), ..Default::default() }, None).unwrap();
    assert!(veredictos::parejas(&db).unwrap().contains_key(&veredictos::pareja(&a, &b)));
    memoria::actualizar(&mut db, &a, Cambios { content: Some("Del servidor de pruebas.".into()), ..Default::default() }, None).unwrap();
    assert!(!veredictos::parejas(&db).unwrap().contains_key(&veredictos::pareja(&a, &b)));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Un tipo de una versión futura llega por un export: se guarda tal cual, se trata como `fact` y vuelve
/// a salir igual.
#[test]
fn un_tipo_desconocido_se_conserva() {
    let dir = carpeta("tipo");
    let ruta = dir.join("o.db");
    let mut db = db::abrir(&ruta).unwrap();
    let id = guardar(&mut db, "", Tipo::Fact, "Una idea", "Para más adelante.");
    db.execute("UPDATE memory SET kind = 'insight' WHERE id = ?1", [&id]).unwrap();
    let m = memoria::obtener(&db, &id).unwrap();
    assert_eq!((m.kind, m.kind_text.as_deref()), (Tipo::Fact, Some("insight")));
    // Cambiar otra cosa no lo pierde.
    memoria::actualizar(&mut db, &id, Cambios { content: Some("Para la 2.0.".into()), ..Default::default() }, None).unwrap();
    let kind: String = db.query_row("SELECT kind FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(kind, "insight");
    let mut salida = Vec::new();
    omoikane::copia::exportar(&db, &ruta, &Ambito::Todos, false, &mut salida).unwrap();
    let texto = String::from_utf8(salida).unwrap();
    assert!(texto.contains("\"kind\":\"insight\""), "{texto}");
    // Y entra en otra base igual.
    let fichero = dir.join("copia.jsonl");
    std::fs::write(&fichero, &texto).unwrap();
    let ruta2 = dir.join("otra.db");
    let mut otra = db::abrir(&ruta2).unwrap();
    omoikane::copia::importar(&mut otra, &ruta2, &fichero, "", None, false).unwrap();
    let kind: String = otra.query_row("SELECT kind FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(kind, "insight");
    drop((db, otra));
    let _ = std::fs::remove_dir_all(dir);
}

/// Para aprender entre proyectos (0.21.0): desde qué proyecto se recibió cada memoria. El recall lo
/// apunta en `recall_log` (con su proyecto) y, cuando caduca, queda resumido en `memory_use`; una
/// búsqueda desde un proyecto se suma directamente.
#[test]
fn el_uso_desde_cada_proyecto_queda_resumido() {
    let dir = carpeta("uso");
    let a = suelta(&dir.join("balua"));
    let b = suelta(&dir.join("lumbre"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let id = guardar(&mut db, "", Tipo::Gotcha, "bitsandbytes 4-bit rompe la visión en la RTX 5070 Ti", "Cuantizar a 8 bits.");
    let (pa, pb) = (proyecto::registrar(&db, &a).unwrap(), proyecto::registrar(&db, &b).unwrap());
    let e = Entrada { session_id: "s1".into(), prompt: "la visión con bitsandbytes 4-bit en la RTX 5070 Ti falla".into(), ..Default::default() };
    let r = recall::al_preguntar(&mut db, &a, &e, None).unwrap().unwrap_or_default();
    assert!(r.contains("bitsandbytes"), "{r}");
    let en_log: i64 = db.query_row("SELECT project_id FROM recall_log WHERE session = 's1' AND memory_id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(en_log, pa);
    // La sesión se compacta (o empieza otra): lo inyectado se borra del registro y queda resumido.
    recall::al_empezar(&mut db, &a, &Entrada { session_id: "s1".into(), source: "compact".into(), ..Default::default() }).unwrap();
    assert_eq!(uno(&db, "SELECT count(*) FROM recall_log WHERE session = 's1'"), 0);
    let (veces, proyecto_id): (i64, i64) = db.query_row("SELECT veces, project_id FROM memory_use WHERE memory_id = ?1", [&id], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
    assert_eq!((veces, proyecto_id), (1, pa));
    // Buscada desde otro proyecto.
    let o = Opciones { ambito: Ambito::de(&db, &b).unwrap(), kind: None, tags: vec![], limite: 5 };
    assert!(!buscar::buscar(&mut db, "bitsandbytes", &o, None).unwrap().is_empty());
    assert_eq!(uno(&db, &format!("SELECT count(DISTINCT project_id) FROM memory_use WHERE memory_id = '{id}'")), 2);
    assert_eq!(uno(&db, &format!("SELECT veces FROM memory_use WHERE memory_id = '{id}' AND project_id = {pb}")), 1);
    // Olvidarla del todo (y que no quede nada colgando).
    memoria::olvidar_del_todo(&mut db, &id).unwrap();
    assert_eq!(uno(&db, "SELECT count(*) FROM memory_use"), 0);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
