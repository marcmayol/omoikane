//! Lo que comparten los tests (los de `tests/` con `mod comun;` y los de la biblioteca, que lo incluyen
//! desde `lib.rs`): una carpeta temporal que se borra sola.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// La carpeta temporal de un test, `<temporal>/omoi-<nombre>-<pid>`, que se borra entera al salir de su
/// ámbito, también si el test falla (el `Drop` corre al deshacer la pila tras un `panic`). Solo borra
/// esa carpeta: la ruta la pone `nueva` y no se puede cambiar.
///
/// Se declara antes que lo que tenga ficheros abiertos dentro (la base): lo de después se suelta
/// antes, y en Windows no se puede borrar un fichero abierto.
pub struct Temporal(PathBuf);

impl Temporal {
    /// Vacía: si quedó una de antes con el mismo nombre y pid, se borra primero.
    pub fn nueva(nombre: &str) -> Temporal {
        assert!(!nombre.is_empty() && !nombre.contains(['/', '\\', '.']), "nombre de carpeta temporal: {nombre}");
        let dir = std::env::temp_dir().join(format!("omoi-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temporal(dir)
    }
}

impl Deref for Temporal {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Temporal {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Temporal {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

/// Un puerto libre de 127.0.0.1 para un `omoi serve` del test: el que da el sistema al pedir el 0. Uno
/// fijo por pid (`7600 + pid % 300`) chocaba con el de otro `cargo test` a la vez, como los de otras
/// ramas en paralelo (en Windows los pid son múltiplos de 4: `pid % 300` solo da 75 puertos): el test
/// le hablaba al servidor del otro proceso, que a veces se estaba cerrando y contestaba vacío.
#[allow(dead_code)]
pub fn puerto_libre() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

/// Las variables que pone la sesión de un agente y que cambian lo que guarda `omoi`: Claude Code pone
/// `TRACEPARENT` (la procedencia lleva su traza) y un orquestador que firma lo de sus agentes,
/// `OMOIKANE_AUTHOR` (el autor). En la CI no existen; si los tests corren dentro de una sesión, sí.
#[allow(dead_code)]
pub const DE_LA_SESION: &[&str] = &["OMOIKANE_AUTHOR", "TRACEPARENT", "TRACESTATE"];

/// Quita de la orden las variables de `DE_LA_SESION`, para que el test dé lo mismo desde una sesión de
/// agente que en la CI. Va en la orden y no en `.cargo/config.toml`: ese fichero solo lo lee Cargo si
/// se lanza desde dentro del repo, no con `--manifest-path` desde otra carpeta. Un test que necesita
/// una la pone después.
#[allow(dead_code)]
pub fn sin_la_sesion(c: &mut std::process::Command) -> &mut std::process::Command {
    for v in DE_LA_SESION {
        c.env_remove(v);
    }
    c
}

impl Drop for Temporal {
    fn drop(&mut self) {
        // Un proceso que lanzó el test (`omoi serve`, la sincronización de fondo, git) o el antivirus
        // pueden tardar en soltar sus ficheros: en Windows no se borran hasta entonces. Con la máquina
        // cargada por otras compilaciones, 5 s no bastaron (quedaron 8 carpetas de `tests/sync.rs` en
        // una batería de 751 s); esperar más solo cuesta en ese caso.
        let limite = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let error = loop {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(_) if !self.0.exists() => return,
                Err(e) if std::time::Instant::now() >= limite => break e,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
            }
        };
        // Por `io::stderr` y no `eprintln!`: el `Drop` corre dentro del test, con la captura del arnés
        // activa, y con la macro el aviso solo se vería si el test falla.
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "aviso: no se pudo borrar la carpeta temporal {}: {error}", self.0.display());
    }
}
