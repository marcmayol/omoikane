//! El servicio que mantiene abierto EmbeddingGemma (`significado`): que quien pide no espere nunca a
//! que cargue, que el vector sea el de sentence-transformers, que `--off` lo cierre y que se cierre
//! solo sin uso.
//!
//! La parte con el modelo necesita EmbeddingGemma en `OMOIKANE_SEMANTIC_MODEL` (la carpeta de
//! sentence-transformers de `google/embeddinggemma-300m`, ~1,2 GB); sin ella se salta. Todo va en
//! un solo test porque cambia variables de entorno del proceso.

use omoikane::gemma::Tipo;
use omoikane::significado as s;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn esperar(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < max {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    f()
}

fn coseno(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
fn servicio_de_significado() {
    let modelo = std::env::var_os("OMOIKANE_SEMANTIC_MODEL").filter(|m| !m.is_empty()).map(PathBuf::from);
    let dir = std::env::temp_dir().join(format!("omoi-significado-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: es el único test de este fichero (un proceso aparte) y nadie más lee el entorno a la vez.
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &dir);
        std::env::set_var("OMOIKANE_MODELS", dir.join("models"));
        std::env::remove_var("OMOIKANE_SEMANTIC");
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", dir.join("no-hay"));
    }

    // Sin modelo: no hay vector, no espera y no lanza nada.
    let t = Instant::now();
    assert!(s::vectores(&["hola"], Tipo::Pregunta, Duration::from_secs(5)).is_none());
    assert!(t.elapsed() < Duration::from_millis(200));
    assert!(!dir.join("significado.arrancando").exists());

    let Some(modelo) = modelo else {
        eprintln!("significado: falta OMOIKANE_SEMANTIC_MODEL (EmbeddingGemma); se salta la parte con el modelo");
        return;
    };
    unsafe {
        std::env::set_var("OMOIKANE_SEMANTIC_MODEL", &modelo);
        std::env::set_var("OMOIKANE_SEMANTIC_IDLE", "4");
    }

    // Apagado: tampoco hay vector, aunque el modelo esté.
    s::fijar(Some(false)).unwrap();
    assert!(!s::permitido());
    assert!(s::vectores(&["hola"], Tipo::Pregunta, Duration::from_secs(5)).is_none());
    s::fijar(Some(true)).unwrap();
    assert!(s::permitido());

    // Encendido y sin servicio: quien pide no espera a que cargue (2 s); el servicio lo arranca el
    // binario (en los tests `current_exe` es el test, así que se lanza a mano como haría `arrancar`).
    let t = Instant::now();
    assert!(s::vectores(&["hola"], Tipo::Pregunta, Duration::from_secs(5)).is_none());
    assert!(t.elapsed() < Duration::from_millis(200), "no debe esperar a que cargue el modelo");
    assert!(dir.join("significado.arrancando").exists(), "deja el servicio arrancando");
    let mut hijo = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).spawn().unwrap();
    assert!(esperar(Duration::from_secs(60), || s::vivo(Duration::from_millis(200)).is_some()), "el servicio no arrancó");
    assert!(!dir.join("significado.arrancando").exists());

    // Un segundo servicio ve al primero y sale sin cargar nada.
    let segundo = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).status().unwrap();
    assert!(segundo.success());

    // El vector es el de sentence-transformers: coseno pregunta-documento 0,6904 (calculado con
    // `SentenceTransformer("google/embeddinggemma-300m")`, encode_query / encode_document).
    let q = s::vectores(&["How do I undo a cargo yank?"], Tipo::Pregunta, Duration::from_secs(5)).expect("vector de la pregunta");
    let d = s::vectores(&["Cargo yank --undo restores a yanked version so it can be used again."], Tipo::Documento, Duration::from_secs(5)).expect("vector del documento");
    assert_eq!(q[0].len(), omoikane::gemma::DIM);
    let c = coseno(&q[0], &d[0]);
    assert!((c - 0.6904).abs() < 0.002, "coseno {c}");

    // El servicio calcula en segundo plano los vectores de la base de quien le pide algo.
    use omoikane::memoria::{self, Ambito, Nueva, Tipo as Kind};
    let ruta = dir.join("o.db");
    {
        let mut db = omoikane::db::abrir(&ruta).unwrap();
        for (t, c) in [("Cargo yank", "Cargo yank --undo restores a yanked version so it can be used again."), ("Gradle", "Two Gradle daemons at once corrupt the build cache.")] {
            memoria::guardar(&mut db, Nueva { project: "/p".into(), kind: Some(Kind::Fact), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
        }
    }
    let _ = s::vector_consulta(&ruta, "hola", Duration::from_secs(5));
    assert!(
        esperar(Duration::from_secs(120), || s::cobertura(&omoikane::db::abrir(&ruta).unwrap()).unwrap() == (2, 2)),
        "el servicio no calculó los vectores de la base"
    );

    // Y la búsqueda encuentra lo que se pregunta con otras palabras, que por palabras no sale.
    let mut db = omoikane::db::abrir(&ruta).unwrap();
    let o = omoikane::buscar::Opciones { ambito: Ambito::de(&db, "/p").unwrap(), kind: None, tags: vec![], limite: 5 };
    let pregunta = "how do I bring back a release I pulled from the registry?";
    let r = omoikane::buscar::buscar(&mut db, pregunta, &o, None).unwrap();
    assert_eq!(r.first().map(|x| x.memoria.title.as_str()), Some("Cargo yank"), "{r:?}");
    assert!(r[0].similarity.is_some());
    // Apagado, como en la 1.1: solo palabras, y por palabras no hay nada.
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC", "0") };
    assert!(omoikane::buscar::buscar(&mut db, pregunta, &o, None).unwrap().iter().all(|x| x.memoria.title != "Cargo yank" || x.similarity.is_none()));
    unsafe { std::env::remove_var("OMOIKANE_SEMANTIC") };
    drop(db);

    // Un token equivocado no recibe vectores.
    let r = s::registro().unwrap();
    let mut tcp = std::net::TcpStream::connect(("127.0.0.1", r.puerto)).unwrap();
    use std::io::{BufRead, Write};
    tcp.write_all(b"{\"token\":\"x\",\"textos\":[\"hola\"]}\n").unwrap();
    let mut l = String::new();
    std::io::BufReader::new(&tcp).read_line(&mut l).unwrap();
    assert!(l.contains("bad token"), "{l}");

    // `--off` lo cierra.
    s::fijar(Some(false)).unwrap();
    assert!(esperar(Duration::from_secs(5), || hijo.try_wait().unwrap().is_some()), "--off no cerró el servicio");
    s::fijar(Some(true)).unwrap();

    // Y sin uso se cierra solo (aquí, a los 4 s) y borra su registro.
    let mut hijo = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).spawn().unwrap();
    assert!(esperar(Duration::from_secs(60), || s::vivo(Duration::from_millis(200)).is_some()));
    assert!(esperar(Duration::from_secs(15), || hijo.try_wait().unwrap().is_some()), "no se cerró sin uso");
    assert!(s::registro().is_none());
    // El formato compacto (el que se instala) da el mismo vector que el de sentence-transformers.
    let compacto = dir.join("compacto");
    omoikane::gemma::compactar(&modelo, &compacto).unwrap();
    assert!(omoikane::gemma::es_compacto(&compacto));
    let (g1, g2) = (omoikane::gemma::Gemma::cargar(&modelo).unwrap(), omoikane::gemma::Gemma::cargar(&compacto).unwrap());
    for (t, tipo) in [("How do I undo a cargo yank?", Tipo::Pregunta), ("Two Gradle daemons at once corrupt the build cache.", Tipo::Documento)] {
        let c = coseno(&g1.vector(t, tipo).unwrap(), &g2.vector(t, tipo).unwrap());
        assert!(c > 0.9999, "compacto: coseno {c}");
    }
    drop((g1, g2));

    let _ = std::fs::remove_dir_all(&dir);
}
