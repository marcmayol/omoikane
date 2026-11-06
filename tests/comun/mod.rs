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

impl Drop for Temporal {
    fn drop(&mut self) {
        // Un proceso que lanzó el test (`omoi serve`, la sincronización de fondo) puede tardar un
        // momento en soltar sus ficheros: en Windows no se borran hasta entonces.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}
