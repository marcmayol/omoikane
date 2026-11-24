//! The service that keeps EmbeddingGemma open (`significado`): the caller never waits for it to
//! load, the vector is the sentence-transformers one, `--off` shuts it down and it shuts itself down
//! when unused.
//!
//! The part with the model needs EmbeddingGemma in `OMOIKANE_SEMANTIC_MODEL` (the
//! sentence-transformers folder of `google/embeddinggemma-300m`, ~1.2 GB); without it, it is skipped. Everything
//! is in a single test because it changes the process's environment variables.

mod comun;

use omoikane::gemma::TextKind;
use omoikane::significado as s;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn wait_until(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    f()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
fn semantic_service() {
    let model = std::env::var_os("OMOIKANE_SEMANTIC_MODEL").filter(|m| !m.is_empty()).map(PathBuf::from);
    let dir = comun::Temporal::nueva("significado");
    // SAFETY: it is the only test in this file (a separate process) and nobody else reads the environment at the same time.
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &dir);
        std::env::set_var("OMOIKANE_MODELS", dir.join("models"));
        std::env::remove_var("OMOIKANE_SEMANTIC");
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", dir.join("no-hay"));
    }

    // No model: no vector, no waiting and nothing launched.
    let t = Instant::now();
    assert!(s::vectors(&["hola"], TextKind::Query, Duration::from_secs(5)).is_none());
    assert!(t.elapsed() < Duration::from_millis(200));
    assert!(!dir.join("significado.arrancando").exists());

    let Some(model) = model else {
        eprintln!("significado: falta OMOIKANE_SEMANTIC_MODEL (EmbeddingGemma); se salta la parte con el modelo");
        return;
    };
    unsafe {
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", &model);
        std::env::set_var("OMOIKANE_SEMANTIC_IDLE", "4");
    }

    // Off: no vector either, even though the model is there.
    s::set_preference(Some(false)).unwrap();
    assert!(!s::allowed());
    assert!(s::vectors(&["hola"], TextKind::Query, Duration::from_secs(5)).is_none());
    s::set_preference(Some(true)).unwrap();
    assert!(s::allowed());

    // On and no service: the caller does not wait for it to load (2 s); the service is started by the
    // binary (in tests `current_exe` is the test, so it is launched by hand as `start` would).
    let t = Instant::now();
    assert!(s::vectors(&["hola"], TextKind::Query, Duration::from_secs(5)).is_none());
    assert!(t.elapsed() < Duration::from_millis(200), "no debe esperar a que cargue el modelo");
    assert!(dir.join("significado.arrancando").exists(), "deja el servicio arrancando");
    let mut child = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).spawn().unwrap();
    assert!(wait_until(Duration::from_secs(60), || s::alive(Duration::from_millis(200)).is_some()), "el servicio no arrancó");
    assert!(!dir.join("significado.arrancando").exists());

    // A second service sees the first one and exits without loading anything.
    let second = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).status().unwrap();
    assert!(second.success());

    // The vector is the sentence-transformers one: query-document cosine 0.6904 (computed with
    // `SentenceTransformer("google/embeddinggemma-300m")`, encode_query / encode_document).
    let q = s::vectors(&["How do I undo a cargo yank?"], TextKind::Query, Duration::from_secs(5)).expect("vector de la pregunta");
    let d = s::vectors(&["Cargo yank --undo restores a yanked version so it can be used again."], TextKind::Document, Duration::from_secs(5)).expect("vector del documento");
    assert_eq!(q[0].len(), omoikane::gemma::DIM);
    let c = cosine(&q[0], &d[0]);
    assert!((c - 0.6904).abs() < 0.002, "coseno {c}");

    // The service computes in the background the vectors of the database of whoever asks it something.
    use omoikane::memoria::{self, Scope, NewMemory, Kind as Kind};
    let path = dir.join("o.db");
    {
        let mut db = omoikane::db::open(&path).unwrap();
        for (t, c) in [("Cargo yank", "Cargo yank --undo restores a yanked version so it can be used again."), ("Gradle", "Two Gradle daemons at once corrupt the build cache.")] {
            memoria::save(&mut db, NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
        }
    }
    let _ = s::query_vector(&path, "hola", Duration::from_secs(5));
    assert!(
        wait_until(Duration::from_secs(120), || s::coverage(&omoikane::db::open(&path).unwrap()).unwrap() == (2, 2)),
        "el servicio no calculó los vectores de la base"
    );

    // And search finds what is asked in other words, which a word search does not find.
    let mut db = omoikane::db::open(&path).unwrap();
    let o = omoikane::buscar::SearchOptions { scope: Scope::from_project(&db, "/p").unwrap(), kind: None, tags: vec![], limit: 5 };
    let question = "how do I bring back a release I pulled from the registry?";
    let r = omoikane::buscar::search(&mut db, question, &o, None).unwrap();
    assert_eq!(r.first().map(|x| x.memory.title.as_str()), Some("Cargo yank"), "{r:?}");
    assert!(r[0].similarity.is_some());
    // Off, as in 1.1: words only, and by words there is nothing.
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC", "0") };
    assert!(omoikane::buscar::search(&mut db, question, &o, None).unwrap().iter().all(|x| x.memory.title != "Cargo yank" || x.similarity.is_none()));
    unsafe { std::env::remove_var("OMOIKANE_SEMANTIC") };
    drop(db);

    // A wrong token gets no vectors.
    let r = s::registry().unwrap();
    let mut tcp = std::net::TcpStream::connect(("127.0.0.1", r.port)).unwrap();
    use std::io::{BufRead, Write};
    tcp.write_all(b"{\"token\":\"x\",\"textos\":[\"hola\"]}\n").unwrap();
    let mut l = String::new();
    std::io::BufReader::new(&tcp).read_line(&mut l).unwrap();
    assert!(l.contains("bad token"), "{l}");

    // `--off` shuts it down.
    s::set_preference(Some(false)).unwrap();
    assert!(wait_until(Duration::from_secs(5), || child.try_wait().unwrap().is_some()), "--off no cerró el servicio");
    s::set_preference(Some(true)).unwrap();

    // And when unused it shuts itself down (here, after 4 s) and deletes its registration.
    let mut child = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).spawn().unwrap();
    assert!(wait_until(Duration::from_secs(60), || s::alive(Duration::from_millis(200)).is_some()));
    assert!(wait_until(Duration::from_secs(15), || child.try_wait().unwrap().is_some()), "no se cerró sin uso");
    assert!(s::registry().is_none());
    // The compact format (the one that gets installed) gives the same vector as the sentence-transformers one.
    let compact = dir.join("compacto");
    omoikane::gemma::compact(&model, &compact).unwrap();
    assert!(omoikane::gemma::is_compact(&compact));
    let (g1, g2) = (omoikane::gemma::Gemma::load(&model).unwrap(), omoikane::gemma::Gemma::load(&compact).unwrap());
    for (t, kind) in [("How do I undo a cargo yank?", TextKind::Query), ("Two Gradle daemons at once corrupt the build cache.", TextKind::Document)] {
        let c = cosine(&g1.vector(t, kind).unwrap(), &g2.vector(t, kind).unwrap());
        assert!(c > 0.9999, "compacto: coseno {c}");
    }
}
