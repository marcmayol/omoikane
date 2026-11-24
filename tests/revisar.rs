//! Phase 7.1: what is worth reviewing. The cases come from the real memories it was calibrated with
//! (told with made-up words: the repository is public).

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Scope, Changes, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::revisar;
use rusqlite::Connection;

fn base(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("revisar-{name}"));
    let db = db::open(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn save(db: &mut Connection, project: &str, title: &str, content: &str, tags: &[&str]) -> String {
    let n = NewMemory {
        project: project.into(),
        kind: Some(Kind::Fact),
        title: title.into(),
        content: content.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    memoria::save(db, n, None).unwrap().id
}

fn outdated(db: &Connection) -> Vec<(String, String)> {
    revisar::notices(db, &Scope::All).unwrap().into_iter().filter(|a| a.kind == revisar::OUTDATED).map(|a| (a.memory_id, a.other_id)).collect()
}

/// The case that motivated it: the project's note is still at 0.2.0 and the status one is already at 0.9.2.
#[test]
fn warns_about_the_old_version_and_says_so_in_the_recall() {
    let (_dir, mut db) = base("version");
    let old = save(&mut db, "/p", "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let new = save(&mut db, "/p", "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(outdated(&db), vec![(old.clone(), new.clone())]);
    let a = &revisar::notices(&db, &Scope::All).unwrap()[0];
    assert!(a.detail.contains("0.2.0 → 0.9.2"), "{}", a.detail);
    // What `remember` tells the agent right after saving the new one.
    let olds = revisar::superseded_by(&db, &new).unwrap();
    assert_eq!(olds.iter().map(|v| v.0.as_str()).collect::<Vec<_>>(), [old.as_str()]);
    // At session start, the old one shows up with its warning and the new one's id.
    let t = recall::on_session_start(&mut db, "/p", &HookInput { session_id: "s".into(), ..Default::default() }).unwrap().unwrap();
    let line = t.lines().find(|l| l.contains("memoria para agentes en Rust")).unwrap();
    assert!(line.contains("⚠ may be outdated") && line.contains(&new[..12]), "{t}");
}

/// False positives the first versions gave with the real notes.
#[test]
fn does_not_warn_about_different_things() {
    let (_dir, mut db) = base("distintas");
    // Versions of different things in the same project.
    save(&mut db, "/p", "Dependencias de Lumbre", "Usa rusqlite 0.41.0 desde el 27-oct-2026.", &[]);
    save(&mut db, "/p", "Lumbre publicada", "omoi 0.9.2 publicado el 29-oct-2026.", &[]);
    // «Pendiente» in one and «publicada» in another, talking about different things.
    save(&mut db, "", "Eslogan de Lumbre", "Elegido el 29-oct-2026. Falta ponerlo en la descripción del repositorio.", &[]);
    save(&mut db, "", "Lumbre: estado de la versión", "La versión está publicada e instalada en el portátil desde el 30-oct-2026.", &[]);
    assert!(outdated(&db).is_empty(), "{:?}", revisar::notices(&db, &Scope::All).unwrap());
}

/// «Pendiente» in the old one and «hecho» in the new one about the same thing is a warning.
#[test]
fn pending_and_done_about_the_same_thing() {
    let (_dir, mut db) = base("pendiente");
    let old = save(&mut db, "", "Certificado del dominio gaviota", "28-oct-2026: pendiente renovar el certificado wildcard de gaviota.example.", &[]);
    let new = save(&mut db, "", "Dominio gaviota renovado", "30-oct-2026: hecho, renovado el certificado wildcard de gaviota.example.", &[]);
    assert_eq!(outdated(&db), vec![(old, new)]);
}

/// «It's fine as is» holds until the memory changes again.
#[test]
fn dismiss_until_it_changes() {
    let (_dir, mut db) = base("descartar");
    let old = save(&mut db, "/p", "Lumbre: memoria para agentes", "La v0.2.0 publicada el 28-oct-2026.", &[]);
    let new = save(&mut db, "/p", "Lumbre: estado", "La 0.9.2 está publicada desde el 29-oct-2026.", &["status"]);
    assert_eq!(outdated(&db).len(), 1);
    revisar::dismiss(&db, &old, revisar::OUTDATED, &new).unwrap();
    assert!(outdated(&db).is_empty());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let c = Changes { content: Some("La v0.2.0 publicada el 28-oct-2026. Y algo más.".into()), ..Default::default() };
    memoria::update(&mut db, &old, c, None).unwrap();
    assert_eq!(outdated(&db).len(), 1, "tras cambiar, vuelve a avisar");
}

/// The ones that depend on the disk and on length.
#[test]
fn broken_and_long_paths() {
    let (dir, mut db) = base("rutas");
    let project = dir.join("proyecto");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/existe.rs"), "").unwrap();
    let p = project.to_string_lossy().to_string();
    let n = NewMemory {
        project: p.clone(),
        kind: Some(Kind::Gotcha),
        title: "Trampa en el lector".into(),
        content: "Cuidado con el orden.".into(),
        paths: vec!["src/existe.rs".into(), "src/borrado.rs".into()],
        ..Default::default()
    };
    memoria::save(&mut db, n, None).unwrap();
    save(&mut db, &p, "Nota enorme", &"palabra ".repeat(1000), &[]);
    let v = revisar::notices(&db, &Scope::All).unwrap();
    let broken = v.iter().find(|a| a.kind == revisar::BROKEN_PATH).expect("ruta rota");
    assert!(broken.detail.contains("src/borrado.rs") && !broken.detail.contains("existe.rs"), "{}", broken.detail);
    assert!(v.iter().any(|a| a.kind == revisar::TOO_LONG && a.title == "Nota enorme"));
}

// --- The notice at session start --------------------------------------------------------------------

/// `n` global memories too long for the recall: each one, a `too_long` warning.
fn long_ones(db: &mut Connection, from: usize, n: usize) {
    for i in from..from + n {
        save(db, "", &format!("Bitácora larga número {i}"), &format!("Registro {i} del horno. {}", format!("cocción {i} ").repeat(700)), &[]);
    }
}

fn notice_at_start(db: &mut Connection, session: &str) -> Option<String> {
    let t = recall::on_session_start(db, "/p", &HookInput { session_id: session.into(), ..Default::default() }).unwrap().unwrap();
    t.lines().find(|l| l.starts_with("Memory review:")).map(String::from)
}

/// Below the threshold it says nothing; on reaching it, once: with the numbers of the kinds there are
/// and asking to propose without touching anything.
#[test]
fn announces_the_review_on_passing_the_threshold_and_once() {
    let (_dir, mut db) = base("aviso");
    long_ones(&mut db, 0, revisar::NOTICE_MIN - 1);
    assert_eq!(notice_at_start(&mut db, "s1"), None, "por debajo del umbral");
    long_ones(&mut db, 100, 1);
    let a = notice_at_start(&mut db, "s2").expect("al llegar al umbral");
    assert!(a.contains("20 items pending (20 too long)") && !a.contains("outdated"), "{a}");
    assert!(a.contains("omoi review") && a.contains("absorb") && a.contains("without the user's OK"), "{a}");
    // The same day, not again; nor if it grows a little.
    assert_eq!(notice_at_start(&mut db, "s3"), None, "dos veces el mismo día");
    long_ones(&mut db, 200, 4);
    assert_eq!(notice_at_start(&mut db, "s4"), None, "4 más no es bastante");
    // If it grows enough, yes, with the new number.
    long_ones(&mut db, 300, 1);
    let a = notice_at_start(&mut db, "s5").expect("al crecer");
    assert!(a.contains("25 items pending"), "{a}");
    // Another day, again even if it hasn't grown.
    db.execute("UPDATE meta SET v = '2000-01-01|25' WHERE k = 'revision_aviso'", []).unwrap();
    assert!(notice_at_start(&mut db, "s6").is_some(), "al día siguiente");
}

/// The `context` tool (agents without recall at start) gives the same notice, and shares with the
/// recall the «already said today».
#[test]
fn the_notice_also_in_context() {
    let (dir, mut db) = base("aviso-context");
    let folder = dir.join("proyecto");
    std::fs::create_dir_all(&folder).unwrap();
    long_ones(&mut db, 0, revisar::NOTICE_MIN);
    let path = dir.join("o.db");
    let context = || {
        let (t, error) = omoikane::mcp::herramienta(path.clone(), Some(folder.clone()), Some("context"), "", "test").unwrap();
        assert!(!error, "{t}");
        serde_json::from_str::<serde_json::Value>(&t).unwrap()
    };
    let r = context();
    let a = r["review_notice"].as_str().expect("el aviso");
    assert!(a.starts_with("Memory review: 20 items pending (20 too long)"), "{a}");
    assert!(context().get("review_notice").is_none(), "una vez al día");
    assert_eq!(notice_at_start(&mut db, "s1"), None, "ya se dijo hoy por context");
}

/// `context` counts like the start, without the broken paths: having a few doesn't make the count
/// «grow» nor repeat the notice in the same session.
#[test]
fn broken_paths_do_not_repeat_the_notice_in_context() {
    let (dir, mut db) = base("aviso-rutas");
    let folder = dir.join("proyecto");
    std::fs::create_dir_all(&folder).unwrap();
    // The path as Omoikane resolves it: on Windows, the temporary folder can arrive with its short
    // name (`RUNNER~1`) and `context` resolves it with the long one.
    let p = omoikane::proyecto::of_dir(&folder).unwrap();
    long_ones(&mut db, 0, revisar::NOTICE_MIN);
    assert!(notice_at_start(&mut db, "s1").is_some(), "al empezar");
    for i in 0..6 {
        let n = NewMemory {
            project: p.clone(),
            kind: Some(Kind::Gotcha),
            title: format!("Trampa {i} del lector"),
            content: format!("Cuidado con el orden {i}."),
            paths: vec![format!("src/borrado{i}.rs")],
            ..Default::default()
        };
        memoria::save(&mut db, n, None).unwrap();
    }
    let (t, error) = omoikane::mcp::herramienta(dir.join("o.db"), Some(folder.clone()), Some("context"), "", "test").unwrap();
    assert!(!error, "{t}");
    let r: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert!(r["review"].as_array().unwrap().iter().filter(|a| a["kind"] == revisar::BROKEN_PATH).count() >= 5, "{r}");
    assert!(r.get("review_notice").is_none(), "{r}");
}

/// At the start it counts without reading the whole memories (`pending`): it gives the same as the
/// full review, except for broken paths and secrets, which only show up in the review.
#[test]
fn the_quick_numbers_match_the_review() {
    let (_dir, mut db) = base("cifras");
    let old = save(&mut db, "/p", "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026.", &[]);
    save(&mut db, "/p", "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub.", &["status"]);
    save(&mut db, "/p", "Horno del taller", "Ver [[manual_que_no_existe]] antes de abrirlo.", &[]);
    long_ones(&mut db, 0, 2);
    let other = save(&mut db, "/otro", "Cosa de otro proyecto", "Ver [[tampoco_existe]].", &[]);
    // An old, unused one: `unused`, and whatever the review says about it by its date.
    db.execute("UPDATE memory SET created_at = '2020-01-01T00:00:00.000Z', updated_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [&old]).unwrap();
    // A years-old status with changes after it: `stale_status`.
    let status = save(&mut db, "/p", "Horno: en qué punto quedó", "Faltaba calibrar el termopar.", &["status"]);
    db.execute("UPDATE memory SET updated_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [&status]).unwrap();
    // A dismissal counts in both.
    revisar::dismiss(&db, &other, revisar::BROKEN_LINK, "").unwrap();
    for scope in [Scope::All, Scope::from_project(&db, "/p").unwrap()] {
        let quick = revisar::pending(&db, &scope).unwrap();
        let full = revisar::counts(&revisar::notices(&db, &scope).unwrap());
        assert_eq!(quick, full);
        assert!(quick.len() >= 5 && quick.contains_key(revisar::STALE_STATUS), "{quick:?}");
    }
}

/// Made-up, stable text for `n` different memories: three-syllable words drawn with a linear
/// generator (no randomness: the test always gives the same).
fn made_up_text(seed: u64, words: usize) -> String {
    const SYLLABLES: [&str; 16] = ["ka", "lo", "mi", "ra", "te", "su", "po", "ne", "di", "fa", "gu", "be", "zo", "chi", "ve", "tra"];
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut v = Vec::with_capacity(words);
    for _ in 0..words {
        let mut p = String::new();
        for _ in 0..3 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            p.push_str(SYLLABLES[(x >> 33) as usize % SYLLABLES.len()]);
        }
        v.push(p);
    }
    v.join(" ")
}

fn trade_duplicates(db: &Connection) -> Vec<(String, String)> {
    revisar::notices(db, &Scope::All).unwrap().into_iter().filter(|a| a.kind == revisar::TRADE_DUPLICATE).map(|a| (a.memory_id, a.other_id)).collect()
}

/// 1.8.6: without a model (without vectors) trade duplicates are compared by words, with the IDF of
/// the whole database. Before, it was computed in the global scope, where trade memories aren't
/// visible: with only trade memories the IDF came out negative and none was ever found. And nothing
/// that serves a request computes them (the session start, `pending`, `context`, `review`, the web):
/// they read what the background process left (`trade_duplicates`).
#[test]
fn trade_duplicates_without_a_model() {
    let (_dir, mut db) = base("oficio-sin-modelo");
    let old = save(&mut db, "", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill.", &["trade:android"]);
    for i in 0..6 {
        save(&mut db, "", &made_up_text(i, 4), &made_up_text(100 + i, 30), &["trade:android"]);
    }
    let new = save(&mut db, "", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado: usa adb emu kill.", &["trade:android"]);
    // Same thing, from another trade: it pairs with none.
    save(&mut db, "", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill.", &["trade:web"]);
    // Before the background process, nothing: reading doesn't compute.
    assert!(trade_duplicates(&db).is_empty());
    assert_eq!(revisar::pending(&db, &Scope::All).unwrap().get(revisar::TRADE_DUPLICATE), None);
    // The background process: it finds it without vectors.
    let pairs = revisar::trade_duplicates(&db, &Scope::All).unwrap();
    assert_eq!(pairs.iter().map(|p| (p.0.clone(), p.1.clone())).collect::<Vec<_>>(), vec![(new.clone(), old.clone())]);
    // And from then on the review and the start read it.
    assert_eq!(trade_duplicates(&db), vec![(old.clone(), new.clone())]);
    assert_eq!(revisar::pending(&db, &Scope::All).unwrap().get(revisar::TRADE_DUPLICATE), Some(&1));
    // «It's fine as is»: it stops showing even if what's saved doesn't change.
    revisar::dismiss(&db, &old, revisar::TRADE_DUPLICATE, &new).unwrap();
    assert!(trade_duplicates(&db).is_empty());
}

/// 1.8.6: with 300 trade memories without a model, the computation (the background process's part)
/// counted the same words in the index once per pair (42 memories: 16.6 s). Now each memory counts its
/// words once; and the session start computes nothing.
#[test]
fn trade_duplicates_with_300_without_a_model_is_fast() {
    let (_dir, mut db) = base("oficio-300");
    for i in 0..300 {
        save(&mut db, "", &made_up_text(i, 4), &made_up_text(1000 + i, 40), &["trade:android"]);
    }
    let t = std::time::Instant::now();
    let pairs = revisar::trade_duplicates(&db, &Scope::All).unwrap();
    let elapsed = t.elapsed();
    assert!(pairs.is_empty(), "{pairs:?}");
    // Generous for a slow CI in debug; in release, locally, a few tens of ms (see the CHANGELOG).
    assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");
    let t = std::time::Instant::now();
    revisar::pending(&db, &Scope::All).unwrap();
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
}

/// 1.12.1: «Split» a memory that is too long. The first part stays in the original (its id) and the
/// rest are new, with the same project, kind, paths and tags, except `status`.
#[test]
fn split_one_that_is_too_long() {
    let (_dir, mut db) = base("partir");
    let n = NewMemory {
        project: "/p".into(),
        kind: Some(Kind::Gotcha),
        title: "Lumbre: todo lo de la impresora".into(),
        content: format!("{}\n\n{}", "a".repeat(5000), "b".repeat(5000)),
        tags: vec!["impresora".into(), "status".into()],
        paths: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let m = memoria::save(&mut db, n, None).unwrap();
    assert!(revisar::notices(&db, &Scope::All).unwrap().iter().any(|a| a.kind == revisar::TOO_LONG && a.memory_id == m.id));
    let parts = [("Lumbre: impresora (1/2)".to_string(), "a".repeat(5000)), ("Lumbre: impresora (2/2)".to_string(), "b".repeat(5000))];
    let r = memoria::split(&mut db, &m.id, &parts, "split in a test").unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].id, m.id);
    assert_eq!(r[0].title, "Lumbre: impresora (1/2)");
    assert_eq!(r[1].project_id, m.project_id);
    assert_eq!(r[1].kind, Kind::Gotcha);
    assert_eq!(r[1].paths, ["src/lib.rs"]);
    assert_eq!(r[1].tags, ["impresora"], "el estado del proyecto sigue siendo uno");
    revisar::refresh(&db).unwrap();
    assert!(!revisar::notices(&db, &Scope::All).unwrap().iter().any(|a| a.kind == revisar::TOO_LONG));
    // Without two parts, or with an empty one, nothing is touched.
    assert!(memoria::split(&mut db, &m.id, &parts[..1], "x").is_err());
    assert!(memoria::split(&mut db, &m.id, &[parts[0].clone(), ("t".into(), " ".into())], "x").is_err());
    assert_eq!(memoria::get(&db, &m.id).unwrap().title, "Lumbre: impresora (1/2)");
}
