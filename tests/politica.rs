//! When the meaning model (EmbeddingGemma) is used and how much: light below the memory threshold,
//! only to veto in the recall (policy D), never against a preference, without silently losing the
//! veto if the service doesn't answer, and a search that can come back empty.
//!
//! The model isn't needed: a fake service in a thread answers what the test wants (and counts how
//! many times it's asked), and a folder with the compact format's files, empty, acts as an
//! installed model. It's all in a single test because it changes the process's environment variables.

mod comun;

use omoikane::memoria::{self, Scope, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::significado::{self as s, Porque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the fake service answers to each search, and how long it takes.
#[derive(Default)]
struct Fake {
    top: Mutex<Vec<(String, f32)>>,
    delay_ms: AtomicU64,
    questions: AtomicUsize,
}

/// Listens on 127.0.0.1 and registers in `home`'s `significado.json`, like the real service.
fn fake_service(home: &Path) -> Arc<Fake> {
    let f = Arc::new(Fake::default());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let record = serde_json::json!({ "puerto": listener.local_addr().unwrap().port(), "token": "t", "pid": std::process::id(), "modelo": "falso" });
    std::fs::write(home.join("significado.json"), record.to_string()).unwrap();
    let g = f.clone();
    std::thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let g = g.clone();
            std::thread::spawn(move || {
                let mut l = String::new();
                while BufReader::new(&c).read_line(&mut l).unwrap_or(0) > 0 {
                    let p: serde_json::Value = serde_json::from_str(&l).unwrap();
                    let r = if p["ping"].as_bool() == Some(true) {
                        serde_json::json!({ "pid": std::process::id(), "modelo": "falso" })
                    } else {
                        g.questions.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(g.delay_ms.load(Ordering::SeqCst)));
                        serde_json::json!({ "listo": true, "top": *g.top.lock().unwrap(), "pedidas": [] })
                    };
                    let _ = (&c).write_all(format!("{r}\n").as_bytes());
                    l.clear();
                }
            });
        }
    });
    f
}

fn save(db: &mut rusqlite::Connection, project: &str, kind: Kind, title: &str, content: &str) -> String {
    memoria::save(db, NewMemory { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() }, None).unwrap().id
}

/// A vector (any) for each memory: without it, the veto doesn't judge a memory and keeps it.
fn with_vectors(db: &rusqlite::Connection) {
    let rows: Vec<(String, String)> = db.prepare("SELECT id, content_hlc FROM memory").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    let v = vec![0.1f32; omoikane::gemma::DIM];
    for (id, hlc) in rows {
        s::guardar(db, &id, &hlc, &v).unwrap();
    }
}

fn e(session: &str, prompt: &str) -> HookInput {
    HookInput { session_id: session.into(), prompt: prompt.into(), ..Default::default() }
}

fn recall_cli(db: &Path, start: bool) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.arg("recall").env("OMOIKANE_DB", db).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if start {
        c.arg("--session-start");
    }
    let mut h = c.spawn().unwrap();
    let cwd = db.parent().unwrap().to_string_lossy().into_owned();
    h.stdin.take().unwrap().write_all(serde_json::json!({ "session_id": "cli", "cwd": cwd }).to_string().as_bytes()).unwrap();
    String::from_utf8(h.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn when_and_how_much_the_model_is_used() {
    let dir = comun::Temporal::nueva("politica");
    let (home, model) = (dir.join("home"), dir.join("modelo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&model).unwrap();
    for f in omoikane::gemma::FICHEROS_COMPACTO {
        std::fs::write(model.join(f), b"").unwrap();
    }
    // SAFETY: it's the only test in this file (a separate process) and nobody else reads the environment at the same time.
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &home);
        std::env::set_var("OMOIKANE_MODELS", dir.join("models"));
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", &model);
        std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "1000");
        std::env::set_var("OMOIKANE_SYNC_AUTO", "0");
        std::env::set_var("OMOIKANE_AUTO_UPDATE", "0");
        std::env::set_var("OMOIKANE_BACKUPS", "0");
        std::env::set_var("OMOIKANE_LANG", "en");
        std::env::remove_var("OMOIKANE_SEMANTIC");
    }
    let path = dir.join("o.db");
    let mut db = omoikane::db::abrir(&path).unwrap();
    let others = [
        ("Kubernetes probes", "Liveness probes restart the pod when the health endpoint hangs for thirty seconds."),
        ("Postgres vacuum", "Autovacuum falls behind on the events table; run vacuum analyze weekly."),
        ("Docker layers", "Copy the lockfile before the sources so the dependency layer stays cached."),
        ("Terraform state", "The remote state lives in the bucket with locking through DynamoDB."),
        ("Nginx timeouts", "Proxy read timeout is sixty seconds; long exports need the async endpoint."),
        ("Redis eviction", "The cache uses allkeys-lru eviction with a two gigabyte ceiling."),
        ("Python venv", "The tooling expects the virtual environment in the dot venv folder of the repo."),
        ("Node versions", "The frontend pins Node twenty through the volta field in package json."),
        ("Release notes", "Release notes are written from the merged pull request titles every Friday."),
        ("Log rotation", "Application logs rotate daily and keep fourteen days compressed."),
    ];
    let mut ids = Vec::new();
    for (t, c) in others {
        ids.push(save(&mut db, "/p", Kind::Fact, t, c));
    }
    let fact = save(&mut db, "/p", Kind::Fact, "Gradle daemons", "Two Gradle daemons at once corrupt the build cache on the CI runner; stop them before building.");
    let pref = save(&mut db, "/p", Kind::Preference, "Offline builds", "With two gradle daemons the build cache gets corrupt on the CI runner: always build offline with one daemon.");
    with_vectors(&db);
    let fake = fake_service(&home);
    // The most similar according to the «model»: three memories that have nothing to do with it.
    *fake.top.lock().unwrap() = ids.iter().take(5).enumerate().map(|(i, id)| (id.clone(), 0.5 - i as f32 * 0.01)).collect();
    let question = "why do two gradle daemons corrupt the build cache on our CI runner";

    // 1. Small (12 memories, threshold 1,000) and nothing chosen: light, like 1.1. The service isn't
    // asked, neither in the recall nor in the search.
    let e0 = s::estado(&db);
    assert!(!e0.on && e0.reason == Porque::Size && e0.memories == 12 && e0.threshold == 1000, "{e0:?}");
    assert!(!s::en_uso(&db));
    let t = recall::on_prompt(&mut db, "/p", &e("s1", question), None).unwrap().expect("las palabras traen algo");
    assert!(t.contains("Gradle daemons") && t.contains("Offline builds"), "{t}");
    let o = omoikane::buscar::SearchOptions { scope: Scope::from_project(&db, "/p").unwrap(), kind: None, tags: vec![], limit: 5 };
    let r = omoikane::buscar::search(&mut db, question, &o, None).unwrap();
    assert!(r.iter().all(|x| x.similarity.is_none()), "sin modelo: {r:?}");
    assert_eq!(fake.questions.load(Ordering::SeqCst), 0, "por debajo del umbral no se consulta el servicio");

    // 2. Over the threshold: it's used. The veto removes the fact (it isn't among the 3 most similar)
    // and keeps the preference, which applies without resembling the message.
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "10") };
    let e1 = s::estado(&db);
    assert!(e1.on && e1.reason == Porque::Size, "{e1:?}");
    assert!(s::en_uso(&db));
    let t = recall::on_prompt(&mut db, "/p", &e("s2", question), None).unwrap().expect("la preferencia");
    assert!(!t.contains("Gradle daemons") && t.contains("Offline builds"), "{t}");
    assert_eq!(fake.questions.load(Ordering::SeqCst), 1);
    let _ = (&fact, &pref);

    // 3. Policy D: it isn't consulted on messages of 5 words or fewer, of more than 300 characters, nor
    // when the words brought nothing to veto.
    let short = "gradle daemons corrupt build cache";
    let t = recall::on_prompt(&mut db, "/p", &e("s3", short), None).unwrap().unwrap_or_default();
    assert!(t.contains("Gradle daemons"), "corto, sin veto: {t}");
    let long = format!("{question}. {}", "I have been looking at the logs of the last builds and the timings look odd to me. ".repeat(4));
    assert!(long.chars().count() > recall::MAX_LEN_GEMMA);
    let _ = recall::on_prompt(&mut db, "/p", &e("s4", &long), None).unwrap();
    let _ = recall::on_prompt(&mut db, "/p", &e("s5", "tell me something nice about the weather today please friend"), None).unwrap();
    assert_eq!(fake.questions.load(Ordering::SeqCst), 1, "D: ni corto, ni largo, ni sin nada que vetar");
    assert_eq!(recall::model_queries(&db), (1, 0, None));

    // 4. If the service doesn't answer in time: words only, like in 1.1, and it's recorded.
    fake.delay_ms.store(600, Ordering::SeqCst);
    let t = recall::on_prompt(&mut db, "/p", &e("s6", question), None).unwrap().unwrap();
    assert!(t.contains("Gradle daemons"), "sin respuesta no se veta: {t}");
    let (n, without, last) = recall::model_queries(&db);
    assert_eq!((n, without), (2, 1));
    assert!(last.is_some_and(|u| u.ends_with(" timeout")));
    fake.delay_ms.store(0, Ordering::SeqCst);

    // 5. Search with the model can come back empty: what only meaning brings needs a minimum of
    // similarity. What is very similar does get in even if it shares no words.
    let nothing = "quarterly marketing budget meeting agenda";
    *fake.top.lock().unwrap() = vec![(ids[0].clone(), 0.12), (ids[1].clone(), 0.11)];
    let r = omoikane::buscar::search(&mut db, nothing, &o, None).unwrap();
    assert!(r.is_empty(), "sin palabras y sin parecido: vacía, {r:?}");
    *fake.top.lock().unwrap() = vec![(ids[0].clone(), 0.8), (ids[1].clone(), 0.11)];
    let r = omoikane::buscar::search(&mut db, nothing, &o, None).unwrap();
    assert_eq!(r.iter().map(|x| x.memory.id.as_str()).collect::<Vec<_>>(), [ids[0].as_str()], "{r:?}");

    // 6. What the user chooses overrides the size, and `--auto` hands it back to the size.
    s::fijar(Some(false)).unwrap();
    assert!(!s::estado(&db).on && s::estado(&db).reason == Porque::Chosen && !s::permitido());
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "1000") };
    s::fijar(Some(true)).unwrap();
    assert!(s::estado(&db).on && s::estado(&db).reason == Porque::Chosen);
    s::fijar(None).unwrap();
    assert!(!s::estado(&db).on && s::estado(&db).reason == Porque::Size);
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC", "1") };
    assert!(s::estado(&db).on && s::estado(&db).reason == Porque::Env);
    unsafe { std::env::remove_var("OMOIKANE_SEMANTIC") };
    drop(db);
    let _ = std::fs::remove_file(home.join("significado.json"));

    // 7. At session start, without the model installed: below the threshold it's neither downloaded
    // nor announced; once over it, it downloads in the background and is announced only once. `--off` prevents it.
    unsafe {
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", dir.join("no-hay"));
        std::env::set_var("OMOIKANE_SEMANTIC_SOURCE", "http://127.0.0.1:9/no-hay");
    }
    let marker = home.join("significado.descargando");
    let t = recall_cli(&path, true);
    assert!(!t.contains("meaning model") && !marker.exists(), "{t}");
    let j: serde_json::Value = serde_json::from_slice(&Command::new(env!("CARGO_BIN_EXE_omoi")).args(["--json", "model", "semantic"]).env("OMOIKANE_DB", &path).output().unwrap().stdout).unwrap();
    assert_eq!((j["on"].as_bool(), j["reason"].as_str(), j["memories"].as_u64(), j["threshold"].as_u64()), (Some(false), Some("size"), Some(12), Some(1000)), "{j}");
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "10") };
    let t = recall_cli(&path, true);
    assert!(t.contains("Omoikane now holds 12 memories") && t.contains("Tell the user once"), "{t}");
    assert!(marker.exists(), "lanza la descarga");
    let t = recall_cli(&path, true);
    assert!(!t.contains("Omoikane now holds"), "solo una vez: {t}");
    let _ = std::fs::remove_file(&marker);
    let off = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "semantic", "--off"]).env("OMOIKANE_DB", &path).output().unwrap();
    assert!(String::from_utf8_lossy(&off.stdout).contains("you turned it off"), "{off:?}");
    let _ = recall_cli(&path, true);
    assert!(!marker.exists(), "con --off no descarga");
    let doctor = Command::new(env!("CARGO_BIN_EXE_omoi")).arg("doctor").env("OMOIKANE_DB", &path).output().unwrap();
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("meaning    off: you turned it off"), "{doctor:?}");
}
