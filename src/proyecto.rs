//! A qué proyecto pertenece una carpeta: la raíz del repositorio git que la contiene, o la propia
//! carpeta si no hay ninguno. Siempre en forma canónica, para que `C:\Repo`, `c:\repo\.` y un
//! enlace simbólico a la misma carpeta sean el mismo proyecto.

use crate::tr;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Proyecto de la carpeta actual.
pub fn actual() -> Result<String> {
    de(&std::env::current_dir().with_context(|| tr!("can't read the current folder", "no se puede leer la carpeta actual"))?)
}

/// Proyecto de una carpeta.
pub fn de(dir: &Path) -> Result<String> {
    let dir = canonica(dir)?;
    let raiz = dir.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&dir);
    Ok(texto(raiz))
}

fn canonica(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| tr!("{} doesn't exist", "no existe {}", p.display()))?;
    // En Windows, canonicalize devuelve `\\?\C:\…`: se quita el prefijo para guardar la ruta normal.
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(resto) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{resto}")));
        }
        if let Some(resto) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(resto));
        }
    }
    Ok(c)
}

fn texto(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    // La letra de unidad en mayúscula: así coinciden las rutas que llegan de sitios distintos.
    #[cfg(windows)]
    {
        let mut b = s.into_bytes();
        if b.len() >= 2 && b[1] == b':' {
            b[0] = b[0].to_ascii_uppercase();
        }
        return String::from_utf8(b).expect("ruta UTF-8");
    }
    #[allow(unreachable_code)]
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raiz_git_desde_una_subcarpeta() {
        let base = std::env::temp_dir().join(format!("omoi-proyecto-{}", std::process::id()));
        let sub = base.join("repo").join("src").join("dentro");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo").join(".git")).unwrap();
        let desde_sub = de(&sub).unwrap();
        let desde_raiz = de(&base.join("repo")).unwrap();
        assert_eq!(desde_sub, desde_raiz);
        assert!(desde_raiz.ends_with("repo"));
        // Sin git, la propia carpeta.
        let suelta = base.join("suelta");
        std::fs::create_dir_all(&suelta).unwrap();
        assert!(de(&suelta).unwrap().ends_with("suelta"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn worktree_con_fichero_git() {
        // En un worktree, `.git` es un fichero, no una carpeta.
        let base = std::env::temp_dir().join(format!("omoi-worktree-{}", std::process::id()));
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join(".git"), "gitdir: /otro/sitio").unwrap();
        assert_eq!(de(&base.join("a")).unwrap(), de(&base).unwrap());
        let _ = std::fs::remove_dir_all(base);
    }
}
