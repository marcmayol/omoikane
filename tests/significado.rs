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
    s::fijar(false).unwrap();
    assert!(!s::activado());
    assert!(s::vectores(&["hola"], Tipo::Pregunta, Duration::from_secs(5)).is_none());
    s::fijar(true).unwrap();
    assert!(s::activado());

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

    // Un token equivocado no recibe vectores.
    let r = s::registro().unwrap();
    let mut tcp = std::net::TcpStream::connect(("127.0.0.1", r.puerto)).unwrap();
    use std::io::{BufRead, Write};
    tcp.write_all(b"{\"token\":\"x\",\"textos\":[\"hola\"]}\n").unwrap();
    let mut l = String::new();
    std::io::BufReader::new(&tcp).read_line(&mut l).unwrap();
    assert!(l.contains("bad token"), "{l}");

    // `--off` lo cierra.
    s::fijar(false).unwrap();
    assert!(esperar(Duration::from_secs(5), || hijo.try_wait().unwrap().is_some()), "--off no cerró el servicio");
    s::fijar(true).unwrap();

    // Y sin uso se cierra solo (aquí, a los 4 s) y borra su registro.
    let mut hijo = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["model", "serve-semantic"]).spawn().unwrap();
    assert!(esperar(Duration::from_secs(60), || s::vivo(Duration::from_millis(200)).is_some()));
    assert!(esperar(Duration::from_secs(15), || hijo.try_wait().unwrap().is_some()), "no se cerró sin uso");
    assert!(s::registro().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
