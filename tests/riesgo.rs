//! 1.6: each file's history, taken from the session diary, in the recall of the message that
//! names it.

mod comun;

use omoikane::db;
use omoikane::diario;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::riesgo;
use rusqlite::Connection;

fn dir(name: &str) -> comun::Temporal {
    let d = comun::Temporal::nueva(&format!("riesgo-{name}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    d
}

/// One diary line, by hand: what `diario::apuntar` would leave when the session closes.
fn session(db: &Connection, name: &str, project_id: i64, folder: &str, day: &str, files: &[&str]) {
    diario::crear_tabla(db).unwrap();
    db.execute(
        "INSERT INTO diary(session, project_id, folder, first_at, last_at, files) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        rusqlite::params![name, project_id, folder, format!("{day}T10:00:00.000Z"), serde_json::to_string(files).unwrap()],
    )
    .unwrap();
}

fn entry(session: &str, prompt: &str) -> Entrada {
    Entrada { session_id: session.into(), prompt: prompt.into(), ..Default::default() }
}

fn gotcha(db: &mut Connection, project: &str, title: &str, paths: &[&str]) {
    let n = Nueva {
        project: project.into(),
        kind: Some(Tipo::Gotcha),
        title: title.into(),
        content: format!("{title}: cuidado al tocarlo."),
        paths: paths.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap();
}

/// A project with 5 sessions on `src/sync.rs`: 4 with `tests/sync.rs`, 3 with `CHANGELOG.md`, 2 with
/// `src/db.rs` (short of 3) and a lone one with `README.md`.
fn project(name: &str) -> (comun::Temporal, Connection, String, i64) {
    let d = dir(name);
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let id = omoikane::proyecto::resolver(&db, &repo).unwrap().unwrap().0;
    session(&db, "a", id, &repo, "2026-10-01", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    session(&db, "b", id, &repo, "2026-10-08", &["src/sync.rs", "tests/sync.rs", "src/db.rs"]);
    session(&db, "c", id, &repo, "2026-10-15", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md", "src/db.rs"]);
    session(&db, "d", id, &repo, "2026-11-04", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    session(&db, "e", id, &repo, "2026-10-20", &["src/sync.rs", "README.md"]);
    session(&db, "f", id, &repo, "2026-10-21", &["README.md", "src/db.rs"]);
    gotcha(&mut db, &repo, "El push de sync pierde el orden", &["src/sync.rs"]);
    gotcha(&mut db, &repo, "Sync con la ruta entera", &[&format!("{repo}/src/sync.rs")]);
    gotcha(&mut db, &repo, "Otro fichero", &["src/db.rs"]);
    (d, db, repo, id)
}

#[test]
fn one_line_with_what_changes_together() {
    let (_d, mut db, repo, _) = project("linea");
    let t = recall::al_preguntar(&mut db, &repo, &entry("s1", "arregla el fallo en sync.rs"), None).unwrap().expect("la historia");
    assert!(t.starts_with("<omoikane>") && t.ends_with("</omoikane>"), "{t}");
    assert!(
        t.contains(
            "History of `src/sync.rs`: changed in 5 sessions (last 2026-11-04); it usually changes together with `tests/sync.rs` (4 of 5) and `CHANGELOG.md` (3); 2 gotchas mention it."
        ),
        "{t}"
    );
    // The gotchas themselves arrive through the paths, as always.
    assert!(t.contains("El push de sync pierde el orden"), "{t}");
    // Once per file and session.
    let other = recall::al_preguntar(&mut db, &repo, &entry("s1", "y ahora el test de sync.rs"), None).unwrap().unwrap_or_default();
    assert!(!other.contains("History of"), "{other}");
    // Another session sees it again, and so does one after compacting (it's no longer in its context).
    let t2 = recall::al_preguntar(&mut db, &repo, &entry("s2", "mira src/sync.rs"), None).unwrap().unwrap_or_default();
    assert!(t2.contains("History of `src/sync.rs`"), "{t2}");
    recall::al_empezar(&mut db, &repo, &Entrada { session_id: "s1".into(), source: "compact".into(), ..Default::default() }).unwrap();
    let t3 = recall::al_preguntar(&mut db, &repo, &entry("s1", "otra vez sync.rs"), None).unwrap().unwrap_or_default();
    assert!(t3.contains("History of `src/sync.rs`"), "{t3}");
    // It doesn't count as a received memory: `recall_log` only holds memories.
    let log: i64 = db.query_row("SELECT count(*) FROM recall_log WHERE memory_id NOT IN (SELECT id FROM memory)", [], |f| f.get(0)).unwrap();
    assert_eq!(log, 0);
    let hinted: i64 = db.query_row("SELECT count(*) FROM diary_hint", [], |f| f.get(0)).unwrap();
    assert_eq!(hinted, 2);
}

#[test]
fn not_enough_sessions_says_nothing() {
    let (_d, db, repo, _) = project("umbral");
    // `README.md`: 2 sessions, below 3.
    assert!(riesgo::historias(&db, &repo, "cambia README.md").unwrap().is_empty());
    // `src/db.rs`: 3 sessions, but nothing changes with it in 3; its gotcha does count.
    let h = riesgo::historias(&db, &repo, "cambia src/db.rs").unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].sesiones, h[0].juntos.len(), h[0].gotchas), (3, 0, 1));
    assert_eq!(h[0].linea(), "History of `src/db.rs`: changed in 3 sessions (last 2026-10-21); 1 gotcha mentions it.");
}

#[test]
fn no_file_in_message_skips_the_diary() {
    let (_d, mut db, repo, _) = project("nada");
    assert!(!riesgo::parece_nombrar("qué tal ha ido la sincronización de ayer"));
    let t = recall::al_preguntar(&mut db, &repo, &entry("s1", "qué tal ha ido la sincronización de ayer"), None).unwrap().unwrap_or_default();
    assert!(!t.contains("History of"), "{t}");
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'diary_hint')", [], |f| f.get(0)).unwrap();
    assert!(!exists, "sin nada que decir no se crea la tabla");
    // With no diary (new database), nothing either: not even an error.
    let d2 = dir("vacia");
    let repo2 = omoikane::proyecto::de(&d2.join("repo")).unwrap();
    let db2 = db::abrir(&d2.join("o.db")).unwrap();
    assert!(riesgo::al_preguntar(&db2, &repo2, "s", "mira src/sync.rs").unwrap().is_none());
}

#[test]
fn only_this_projects_sessions() {
    let (d, db, repo, id) = project("ambito");
    // Another project with the same file, and sessions with no project: from this folder they count,
    // from another they don't.
    let other_dir = d.join("otro").to_string_lossy().into_owned();
    for (i, day) in ["2026-11-01", "2026-11-02", "2026-11-03"].iter().enumerate() {
        session(&db, &format!("o{i}"), id + 1000, &other_dir, day, &["src/sync.rs"]);
        session(&db, &format!("h{i}"), 0, &other_dir, day, &["src/sync.rs"]);
    }
    session(&db, "suelta", 0, &repo, "2026-11-05", &["src/sync.rs"]);
    let h = riesgo::historias(&db, &repo, "src/sync.rs").unwrap();
    assert_eq!((h[0].sesiones, h[0].ultima.as_str()), (6, "2026-11-05"));
}

#[test]
fn by_name_only_the_busiest() {
    let (_d, db, repo, id) = project("nombre");
    for (i, day) in ["2026-11-01", "2026-11-02", "2026-11-03"].iter().enumerate() {
        session(&db, &format!("t{i}"), id, &repo, day, &["tools/sync.rs"]);
    }
    // By name, only `src/sync.rs` (5 sessions versus 3).
    let h = riesgo::historias(&db, &repo, "mira sync.rs").unwrap();
    assert_eq!(h.iter().map(|x| x.fichero.as_str()).collect::<Vec<_>>(), ["src/sync.rs"]);
    // With the full path, that one; and both if both are named.
    let h = riesgo::historias(&db, &repo, "mira tools/sync.rs").unwrap();
    assert_eq!(h.iter().map(|x| x.fichero.as_str()).collect::<Vec<_>>(), ["tools/sync.rs"]);
    let h = riesgo::historias(&db, &repo, "compara tools/sync.rs con src/sync.rs").unwrap();
    assert_eq!(h.len(), 2);
}

/// The history is optional: if it fails (here, a `diary_hint` that isn't its own), the message recall
/// and the session start go on without it.
#[test]
fn if_history_fails_recall_goes_on() {
    let (_d, mut db, repo, _) = project("falla");
    db.execute_batch("CREATE TABLE diary_hint (otra TEXT)").unwrap();
    assert!(riesgo::al_preguntar(&db, &repo, "s1", "mira src/sync.rs").is_err(), "la tabla rota hace fallar el historial");
    let t = recall::al_preguntar(&mut db, &repo, &entry("s1", "mira src/sync.rs"), None).unwrap().expect("las memorias siguen llegando");
    assert!(t.contains("El push de sync pierde el orden"), "{t}");
    assert!(!t.contains("History of"), "{t}");
    assert!(recall::al_empezar(&mut db, &repo, &entry("s2", "")).unwrap().is_some());
}
