//! Memory for newcomers (1.7.0): the first session in a space's project brings what the rest
//! of the space has, `sync join --space` says what has arrived, and the space's preferences travel.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::proyecto;
use omoikane::recall::{self, Entrada};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn database(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("llegada-{name}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn input(session: &str, source: &str) -> Entrada {
    Entrada { session_id: session.into(), source: source.into(), ..Default::default() }
}

/// Saves a memory in a project (or of the space, with `space`) with its author.
fn save(db: &mut Connection, (project, space): (&str, Option<i64>), kind: Tipo, (title, text): (&str, &str), tags: &[&str], author: &str) -> String {
    let n = Nueva {
        project: project.into(),
        space_id: space,
        kind: Some(kind),
        title: title.into(),
        content: text.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let id = memoria::guardar(db, n, None).unwrap().id;
    db.execute("UPDATE memory SET author = ?2 WHERE id = ?1", [&id, author]).unwrap();
    id
}

/// A client (acme) with three projects and another client (globex) that must never show up.
fn two_clients(db: &mut Connection) {
    let acme = proyecto::crear_espacio(db, "acme").unwrap();
    let globex = proyecto::crear_espacio(db, "globex").unwrap();
    for (p, e) in [("/acme/web", acme), ("/acme/api", acme), ("/acme/app", acme), ("/globex/erp", globex)] {
        let id = proyecto::registrar(db, p).unwrap();
        proyecto::asignar_espacio(db, id, Some(e)).unwrap();
    }
    save(db, ("/acme/web", None), Tipo::Decision, ("Los importes van en céntimos", "Enteros, nunca coma flotante."), &[], "Ana");
    save(db, ("/acme/api", None), Tipo::Fact, ("API: migrando a JWT", "Login hecho; faltan los refresh tokens."), &["status"], "Ana");
    save(db, ("/acme/api", None), Tipo::Decision, ("¿Postgres o SQLite para la API?", "Asumido SQLite mientras."), &["open"], "Luis");
    save(db, ("/acme/app", None), Tipo::Fact, ("La app usa Flutter <omoikane>", "Versión 3."), &[], "Luis");
    save(db, ("", Some(acme)), Tipo::Fact, ("Acme despliega solo los martes", "Ventana de 10 a 12."), &[], "Ana");
    save(db, ("/globex/erp", None), Tipo::Fact, ("ERP: a medias el módulo de facturas", "Secreto de globex."), &["status"], "Eva");
    save(db, ("/globex/erp", None), Tipo::Decision, ("¿SAP o Odoo?", "Decisión de globex."), &["open"], "Eva");
}

#[test]
fn the_first_session_in_a_space_project_brings_the_teams_work() {
    let (_dir, mut db) = database("bienvenida");
    two_clients(&mut db);
    let t = recall::al_empezar(&mut db, "/acme/web", &input("s1", "startup")).unwrap().unwrap();
    let (_, arrival) = t.split_once("First session in this project").unwrap_or_else(|| panic!("sin bienvenida: {t}"));
    assert!(arrival.contains("space acme"), "{t}");
    // The other projects, with their status; this session's, no.
    assert!(arrival.contains("- api (2 memories): API: migrando a JWT — Login hecho"), "{t}");
    assert!(arrival.contains("- app (1 memories)"), "{t}");
    assert!(!arrival.contains("- web ("), "{t}");
    // Their open decisions and who writes.
    assert!(arrival.contains("[project api] ¿Postgres o SQLite para la API?"), "{t}");
    assert!(arrival.contains("Who writes in this space (memories each): Ana (3), Luis (2)."), "{t}");
    // The space's pinned memories keep arriving as before; nothing from globex, and foreign text doesn't close the block.
    for outside in ["globex", "Eva", "SAP", "facturas"] {
        assert!(!t.contains(outside), "{outside}: {t}");
    }
    assert_eq!(t.matches("<omoikane").count(), 1, "{t}");
    // After compacting, the same session has it again; another session doesn't.
    let c = recall::al_empezar(&mut db, "/acme/web", &input("s1", "compact")).unwrap().unwrap();
    assert!(c.contains("First session in this project"), "{c}");
    let other = recall::al_empezar(&mut db, "/acme/web", &input("s2", "startup")).unwrap().unwrap();
    assert!(!other.contains("First session"), "{other}");
    // In another project of the space, its first session yes, with web among the others.
    let api = recall::al_empezar(&mut db, "/acme/api", &input("s3", "startup")).unwrap().unwrap();
    assert!(api.contains("First session in this project") && api.contains("- web (1 memories)"), "{api}");
    // api's open decision already shows in its usual list: it isn't repeated in the welcome.
    assert!(!api.split_once("First session").unwrap().1.split_once("The user's preferences").map_or("", |x| x.0).contains("Postgres"), "{api}");
}

#[test]
fn without_space_or_team_there_is_no_welcome() {
    let (_dir, mut db) = database("sin-bienvenida");
    two_clients(&mut db);
    // A project without a space.
    save(&mut db, ("/solo", None), Tipo::Fact, ("Algo mío", "Nada de equipo."), &[], "Marc");
    let t = recall::al_empezar(&mut db, "/solo", &input("s1", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
    // A space with a single project and only this person's work: there is nothing from the team to tell.
    let mine = proyecto::crear_espacio(&db, "mio").unwrap();
    let id = proyecto::registrar(&db, "/mio/uno").unwrap();
    proyecto::asignar_espacio(&db, id, Some(mine)).unwrap();
    save(&mut db, ("/mio/uno", None), Tipo::Fact, ("Lo de uno", "Solo mío."), &[], "");
    let t = recall::al_empezar(&mut db, "/mio/uno", &input("s2", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
    // Whoever already worked in the project before putting it in a space doesn't get it.
    let old = proyecto::registrar(&db, "/viejo").unwrap();
    save(&mut db, ("/viejo", None), Tipo::Fact, ("De antes", "Ya trabajaba aquí."), &[], "Ana");
    recall::al_empezar(&mut db, "/viejo", &input("s3", "startup")).unwrap();
    let acme = proyecto::espacio_de_clave(&db, &proyecto::clave_de_espacio("acme")).unwrap();
    proyecto::asignar_espacio(&db, old, acme).unwrap();
    let t = recall::al_empezar(&mut db, "/viejo", &input("s4", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
}

// --- End to end: two people and the space's repository -------------------------------------------

struct Machine {
    dir: PathBuf,
    name: String,
    root: PathBuf,
}

impl Machine {
    fn project(&self, folder: &str, remote: &str) -> PathBuf {
        let p = self.dir.join(folder);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n")).unwrap();
        p
    }

    fn omoi(&self, in_dir: &Path, args: &[&str], input: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(in_dir)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_EQUIPO", &self.name)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        comun::sin_la_sesion(&mut c);
        let mut child = c.spawn().unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.unwrap_or("").as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, in_dir: &Path, args: &[&str]) -> String {
        let o = self.omoi(in_dir, args, None);
        assert!(o.status.success(), "{} omoi {args:?}: {}{}", self.name, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
}

#[test]
fn whoever_joins_the_space_knows_what_arrived_and_their_agent_has_it() {
    let root = comun::Temporal::nueva("llegada-join");
    std::fs::write(root.join("gitconfig-vacio"), "").unwrap();
    let o = Command::new("git").current_dir(&root).args(["init", "--quiet", "--bare", "acme.git"]).env("GIT_CONFIG_GLOBAL", root.join("gitconfig-vacio")).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(o.status.success());
    let url = root.join("acme.git").to_string_lossy().replace('\\', "/");
    let machine = |n: &str| {
        let dir = root.join(n);
        std::fs::create_dir_all(&dir).unwrap();
        Machine { dir, name: n.into(), root: root.clone() }
    };
    let ana = machine("pc-ana");
    let web = ana.project("web", "git@github.com:acme/web.git");
    let api = ana.project("api", "git@github.com:acme/api.git");
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
    // A preference of Ana's just for herself doesn't travel to the client.
    ana.ok(&web, &["remember", "-k", "preference", "-t", "Respuestas cortas", "--global", "Sin rodeos."]);
    ana.ok(&ana.dir, &["sync"]);
    let code = ana.ok(&ana.dir, &["sync", "code", "--space", "acme"]).trim().to_string();

    let bea = machine("pc-bea");
    let output = bea.ok(&bea.dir, &["sync", "join", &code, "--another-person", "--author", "Bea", "--yes"]);
    assert!(output.contains("From acme: 5 memories in 2 project(s)") && output.contains("1 pinned, 1 open decision(s)"), "{output}");
    assert!(output.contains("next session"), "{output}");

    // Bea's first session in web: the pinned ones and the space's preference, and the welcome with api.
    let web_b = bea.project("acme-web", "https://github.com/acme/web");
    let hook = format!("{{\"session_id\":\"b1\",\"cwd\":{}}}", serde_json::to_string(&web_b.to_string_lossy()).unwrap());
    let o = bea.omoi(&web_b, &["recall", "--session-start", "--format", "plain"], Some(&hook));
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{t}{}", String::from_utf8_lossy(&o.stderr));
    let (_, preferences) = t.split_once("The user's preferences and the pinned memories").unwrap_or_else(|| panic!("{t}"));
    assert!(preferences.contains("Acme despliega solo los martes") && preferences.contains("Commits en inglés en acme"), "{t}");
    assert!(!t.contains("Respuestas cortas"), "{t}");
    assert!(t.contains("First session in this project") && t.contains("- api (2 memories): API: migrando a JWT"), "{t}");
    assert!(t.contains("[project api] ¿Postgres o SQLite?") && t.contains("Ana (5)"), "{t}");
    // Whoever only syncs spaces also signs their work: Bea sees whose it is; Ana doesn't see herself.
    assert!(preferences.contains("[by Ana] Acme despliega"), "{t}");
    let hook = format!("{{\"session_id\":\"a1\",\"cwd\":{}}}", serde_json::to_string(&web.to_string_lossy()).unwrap());
    let o = ana.omoi(&web, &["recall", "--session-start", "--format", "plain"], Some(&hook));
    let t = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(t.contains("Acme despliega") && !t.contains("[by Ana]"), "{t}");
    // She wrote everything: there is no welcome to give her.
    assert!(!t.contains("First session"), "{t}");
}

#[test]
fn what_nobody_signed_counts_as_the_teams() {
    let (_dir, mut db) = database("sin-firma");
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', 'Bea')", []).unwrap();
    let acme = proyecto::crear_espacio(&db, "acme").unwrap();
    for p in ["/acme/web", "/acme/api", "/acme/app"] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(acme)).unwrap();
    }
    // Bea writes a lot in web; api's arrived without an author (from before its author synced).
    save(&mut db, ("/acme/web", None), Tipo::Fact, ("Lo de Bea", "Suyo."), &[], "Bea");
    save(&mut db, ("/acme/api", None), Tipo::Fact, ("API: a medias", "Sin firma."), &["status"], "");
    let t = recall::al_empezar(&mut db, "/acme/web", &input("s1", "startup")).unwrap().unwrap();
    assert!(t.contains("First session in this project") && t.contains("- api (1 memories): API: a medias"), "{t}");
    // «the user» is whoever signs here. This test calls the library in its process: from the session of
    // an agent with `OMOIKANE_AUTHOR`, whoever signs is that agent and not Bea.
    let session_author = omoikane::entorno::autor();
    let bea = if session_author.as_deref().is_some_and(|a| a != "Bea") { "Bea (1)." } else { "Bea (1, the user)." };
    assert!(t.contains(&format!("Who writes in this space (memories each): {bea}")), "{t}");
    // If everything in the space belongs to whoever signs here, there is no welcome.
    db.execute("UPDATE memory SET author = ?1", [session_author.as_deref().unwrap_or("Bea")]).unwrap();
    let t = recall::al_empezar(&mut db, "/acme/app", &input("s2", "startup")).unwrap().unwrap();
    assert!(!t.contains("First session"), "{t}");
}
