//! Cuándo se usa el modelo de significado (EmbeddingGemma) y cuánto: ligero por debajo del umbral de
//! memorias, solo para vetar en el recall (política D), nunca contra una preferencia, sin perder el
//! veto en silencio si el servicio no responde, y una búsqueda que puede volver vacía.
//!
//! No hace falta el modelo: un servicio falso en un hilo responde lo que el test quiere (y cuenta las
//! veces que se le pregunta), y una carpeta con los ficheros del formato compacto vacíos hace de
//! modelo instalado. Todo va en un solo test porque cambia variables de entorno del proceso.

mod comun;

use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::significado::{self as s, Porque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Lo que responde el servicio falso a cada búsqueda, y cuánto tarda.
#[derive(Default)]
struct Falso {
    top: Mutex<Vec<(String, f32)>>,
    retraso_ms: AtomicU64,
    preguntas: AtomicUsize,
}

/// Escucha en 127.0.0.1 y se apunta en `significado.json` de `home`, como el servicio de verdad.
fn servicio_falso(home: &Path) -> Arc<Falso> {
    let f = Arc::new(Falso::default());
    let escucha = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let registro = serde_json::json!({ "puerto": escucha.local_addr().unwrap().port(), "token": "t", "pid": std::process::id(), "modelo": "falso" });
    std::fs::write(home.join("significado.json"), registro.to_string()).unwrap();
    let g = f.clone();
    std::thread::spawn(move || {
        for c in escucha.incoming().flatten() {
            let g = g.clone();
            std::thread::spawn(move || {
                let mut l = String::new();
                while BufReader::new(&c).read_line(&mut l).unwrap_or(0) > 0 {
                    let p: serde_json::Value = serde_json::from_str(&l).unwrap();
                    let r = if p["ping"].as_bool() == Some(true) {
                        serde_json::json!({ "pid": std::process::id(), "modelo": "falso" })
                    } else {
                        g.preguntas.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(g.retraso_ms.load(Ordering::SeqCst)));
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

fn guardar(db: &mut rusqlite::Connection, project: &str, kind: Tipo, title: &str, content: &str) -> String {
    memoria::guardar(db, Nueva { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() }, None).unwrap().id
}

/// Un vector (cualquiera) para cada memoria: sin él, el veto no juzga una memoria y la deja.
fn con_vectores(db: &rusqlite::Connection) {
    let filas: Vec<(String, String)> = db.prepare("SELECT id, content_hlc FROM memory").unwrap().query_map([], |f| Ok((f.get(0)?, f.get(1)?))).unwrap().map(Result::unwrap).collect();
    let v = vec![0.1f32; omoikane::gemma::DIM];
    for (id, hlc) in filas {
        s::guardar(db, &id, &hlc, &v).unwrap();
    }
}

fn e(sesion: &str, prompt: &str) -> Entrada {
    Entrada { session_id: sesion.into(), prompt: prompt.into(), ..Default::default() }
}

fn recall_cli(db: &Path, inicio: bool) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.arg("recall").env("OMOIKANE_DB", db).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if inicio {
        c.arg("--session-start");
    }
    let mut h = c.spawn().unwrap();
    let cwd = db.parent().unwrap().to_string_lossy().into_owned();
    h.stdin.take().unwrap().write_all(serde_json::json!({ "session_id": "cli", "cwd": cwd }).to_string().as_bytes()).unwrap();
    String::from_utf8(h.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn cuando_y_cuanto_se_usa_el_modelo() {
    let dir = comun::Temporal::nueva("politica");
    let (home, modelo) = (dir.join("home"), dir.join("modelo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&modelo).unwrap();
    for f in omoikane::gemma::FICHEROS_COMPACTO {
        std::fs::write(modelo.join(f), b"").unwrap();
    }
    // SAFETY: es el único test de este fichero (un proceso aparte) y nadie más lee el entorno a la vez.
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &home);
        std::env::set_var("OMOIKANE_MODELS", dir.join("models"));
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", &modelo);
        std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "1000");
        std::env::set_var("OMOIKANE_SYNC_AUTO", "0");
        std::env::set_var("OMOIKANE_AUTO_UPDATE", "0");
        std::env::set_var("OMOIKANE_BACKUPS", "0");
        std::env::set_var("OMOIKANE_LANG", "en");
        std::env::remove_var("OMOIKANE_SEMANTIC");
    }
    let ruta = dir.join("o.db");
    let mut db = omoikane::db::abrir(&ruta).unwrap();
    let otros = [
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
    for (t, c) in otros {
        ids.push(guardar(&mut db, "/p", Tipo::Fact, t, c));
    }
    let hecho = guardar(&mut db, "/p", Tipo::Fact, "Gradle daemons", "Two Gradle daemons at once corrupt the build cache on the CI runner; stop them before building.");
    let pref = guardar(&mut db, "/p", Tipo::Preference, "Offline builds", "With two gradle daemons the build cache gets corrupt on the CI runner: always build offline with one daemon.");
    con_vectores(&db);
    let falso = servicio_falso(&home);
    // Lo más parecido según el «modelo»: tres memorias que no tienen nada que ver.
    *falso.top.lock().unwrap() = ids.iter().take(5).enumerate().map(|(i, id)| (id.clone(), 0.5 - i as f32 * 0.01)).collect();
    let pregunta = "why do two gradle daemons corrupt the build cache on our CI runner";

    // 1. Pequeña (12 memorias, umbral 1.000) y sin elegir nada: ligera, como la 1.1. Ni se pregunta al
    // servicio, ni en el recall ni en la búsqueda.
    let e0 = s::estado(&db);
    assert!(!e0.on && e0.reason == Porque::Size && e0.memories == 12 && e0.threshold == 1000, "{e0:?}");
    assert!(!s::en_uso(&db));
    let t = recall::al_preguntar(&mut db, "/p", &e("s1", pregunta), None).unwrap().expect("las palabras traen algo");
    assert!(t.contains("Gradle daemons") && t.contains("Offline builds"), "{t}");
    let o = omoikane::buscar::Opciones { ambito: Ambito::de(&db, "/p").unwrap(), kind: None, tags: vec![], limite: 5 };
    let r = omoikane::buscar::buscar(&mut db, pregunta, &o, None).unwrap();
    assert!(r.iter().all(|x| x.similarity.is_none()), "sin modelo: {r:?}");
    assert_eq!(falso.preguntas.load(Ordering::SeqCst), 0, "por debajo del umbral no se consulta el servicio");

    // 2. Pasa el umbral: se usa. El veto quita el hecho (no está entre los 3 más parecidos) y deja la
    // preferencia, que se aplica sin parecerse al mensaje.
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "10") };
    let e1 = s::estado(&db);
    assert!(e1.on && e1.reason == Porque::Size, "{e1:?}");
    assert!(s::en_uso(&db));
    let t = recall::al_preguntar(&mut db, "/p", &e("s2", pregunta), None).unwrap().expect("la preferencia");
    assert!(!t.contains("Gradle daemons") && t.contains("Offline builds"), "{t}");
    assert_eq!(falso.preguntas.load(Ordering::SeqCst), 1);
    let _ = (&hecho, &pref);

    // 3. Política D: no se consulta en mensajes de 5 palabras o menos, de más de 300 caracteres, ni
    // cuando las palabras no trajeron nada que vetar.
    let corto = "gradle daemons corrupt build cache";
    let t = recall::al_preguntar(&mut db, "/p", &e("s3", corto), None).unwrap().unwrap_or_default();
    assert!(t.contains("Gradle daemons"), "corto, sin veto: {t}");
    let largo = format!("{pregunta}. {}", "I have been looking at the logs of the last builds and the timings look odd to me. ".repeat(4));
    assert!(largo.chars().count() > recall::LARGO_MAX_GEMMA);
    let _ = recall::al_preguntar(&mut db, "/p", &e("s4", &largo), None).unwrap();
    let _ = recall::al_preguntar(&mut db, "/p", &e("s5", "tell me something nice about the weather today please friend"), None).unwrap();
    assert_eq!(falso.preguntas.load(Ordering::SeqCst), 1, "D: ni corto, ni largo, ni sin nada que vetar");
    assert_eq!(recall::consultas_al_modelo(&db), (1, 0, None));

    // 4. Si el servicio no responde a tiempo: solo palabras, como en la 1.1, y queda apuntado.
    falso.retraso_ms.store(600, Ordering::SeqCst);
    let t = recall::al_preguntar(&mut db, "/p", &e("s6", pregunta), None).unwrap().unwrap();
    assert!(t.contains("Gradle daemons"), "sin respuesta no se veta: {t}");
    let (n, sin, ultimo) = recall::consultas_al_modelo(&db);
    assert_eq!((n, sin), (2, 1));
    assert!(ultimo.is_some_and(|u| u.ends_with(" timeout")));
    falso.retraso_ms.store(0, Ordering::SeqCst);

    // 5. La búsqueda con modelo puede volver vacía: lo que solo trae el significado necesita un mínimo
    // de parecido. Lo muy parecido sí entra aunque no comparta palabras.
    let nada = "quarterly marketing budget meeting agenda";
    *falso.top.lock().unwrap() = vec![(ids[0].clone(), 0.12), (ids[1].clone(), 0.11)];
    let r = omoikane::buscar::buscar(&mut db, nada, &o, None).unwrap();
    assert!(r.is_empty(), "sin palabras y sin parecido: vacía, {r:?}");
    *falso.top.lock().unwrap() = vec![(ids[0].clone(), 0.8), (ids[1].clone(), 0.11)];
    let r = omoikane::buscar::buscar(&mut db, nada, &o, None).unwrap();
    assert_eq!(r.iter().map(|x| x.memoria.id.as_str()).collect::<Vec<_>>(), [ids[0].as_str()], "{r:?}");

    // 6. Lo que elige el usuario manda sobre el tamaño, y `--auto` lo devuelve al tamaño.
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

    // 7. Al empezar sesión, sin el modelo instalado: por debajo del umbral ni se descarga ni se avisa;
    // al pasarlo, se descarga en segundo plano y se avisa una sola vez. `--off` lo impide.
    unsafe {
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", dir.join("no-hay"));
        std::env::set_var("OMOIKANE_SEMANTIC_SOURCE", "http://127.0.0.1:9/no-hay");
    }
    let marca = home.join("significado.descargando");
    let t = recall_cli(&ruta, true);
    assert!(!t.contains("meaning model") && !marca.exists(), "{t}");
    let j: serde_json::Value = serde_json::from_slice(&Command::new(env!("CARGO_BIN_EXE_omoi")).args(["--json", "model", "semantic"]).env("OMOIKANE_DB", &ruta).output().unwrap().stdout).unwrap();
    assert_eq!((j["on"].as_bool(), j["reason"].as_str(), j["memories"].as_u64(), j["threshold"].as_u64()), (Some(false), Some("size"), Some(12), Some(1000)), "{j}");
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_THRESHOLD", "10") };
    let t = recall_cli(&ruta, true);
    assert!(t.contains("Omoikane now holds 12 memories") && t.contains("Tell the user once"), "{t}");
    assert!(marca.exists(), "lanza la descarga");
    let t = recall_cli(&ruta, true);
    assert!(!t.contains("Omoikane now holds"), "solo una vez: {t}");
    let _ = std::fs::remove_file(&marca);
    let off = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "semantic", "--off"]).env("OMOIKANE_DB", &ruta).output().unwrap();
    assert!(String::from_utf8_lossy(&off.stdout).contains("you turned it off"), "{off:?}");
    let _ = recall_cli(&ruta, true);
    assert!(!marca.exists(), "con --off no descarga");
    let doctor = Command::new(env!("CARGO_BIN_EXE_omoi")).arg("doctor").env("OMOIKANE_DB", &ruta).output().unwrap();
    assert!(String::from_utf8_lossy(&doctor.stdout).contains("meaning    off: you turned it off"), "{doctor:?}");
}
