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

/// Si el proyecto es una carpeta general y no un proyecto: la carpeta del usuario, una por encima de
/// ella o la raíz de una unidad, sin repositorio git. Quien abre las sesiones ahí trabaja en muchos
/// proyectos a la vez, así que desde ahí se ven las memorias de todos.
pub fn es_general(project: &str) -> bool {
    match dirs::home_dir().and_then(|h| de(&h).ok()) {
        Some(home) => general_en(project, Path::new(&home)),
        None => false,
    }
}

fn general_en(project: &str, home: &Path) -> bool {
    let p = Path::new(project);
    !project.is_empty() && !p.join(".git").exists() && (home.starts_with(p) || p.parent().is_none())
}

/// Proyecto de una carpeta que alguien da a propósito (`--project`, `project`): una carpeta general
/// no es un proyecto, así que da global.
pub fn de_carpeta(dir: &Path) -> Result<String> {
    let p = de(dir)?;
    Ok(if es_general(&p) { String::new() } else { p })
}

/// Dónde guardar una memoria que no dice su proyecto: el actual si es un proyecto de verdad; si es
/// una carpeta general, el repositorio al que apunten sus rutas absolutas (las de `paths` y las que
/// cite el texto) cuando es uno solo, y si no, ninguno: global.
pub fn para_guardar(actual: &str, paths: &[String], texto: &str) -> String {
    if !es_general(actual) {
        return actual.to_string();
    }
    let home = dirs::home_dir().and_then(|h| de(&h).ok()).unwrap_or_default();
    let mut repos: Vec<String> = paths
        .iter()
        .map(String::as_str)
        .chain(texto.split(|c: char| c.is_whitespace() || "`\"'«»()[]<>,;*".contains(c)))
        .filter_map(|r| repo_de_ruta(r, Path::new(&home)))
        .collect();
    repos.sort();
    repos.dedup();
    match repos.as_slice() {
        [uno] => uno.clone(),
        _ => String::new(),
    }
}

/// El repositorio git que contiene una ruta absoluta (aunque el fichero ya no exista), si lo hay y no
/// es la carpeta del usuario.
fn repo_de_ruta(ruta: &str, home: &Path) -> Option<String> {
    let ruta = ruta.trim_end_matches(['.', ':', '?', '!']);
    let b = ruta.as_bytes();
    let absoluta = (b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || (cfg!(unix) && ruta.len() > 1 && ruta.starts_with('/'));
    let ruta = if let Some(resto) = ruta.strip_prefix("~/").or_else(|| ruta.strip_prefix("~\\")) {
        home.join(resto)
    } else if absoluta {
        PathBuf::from(ruta)
    } else {
        return None;
    };
    let existe = ruta.ancestors().find(|a| a.exists())?;
    let raiz = existe.ancestors().find(|d| d.join(".git").exists())?;
    let raiz = de(raiz).ok()?;
    // Los clones de herramientas (`~/.claude/plugins/…`, `~/.cargo/git/…`, `AppData`) no son proyectos.
    let oculto = Path::new(&raiz).strip_prefix(home).is_ok_and(|r| {
        r.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c.starts_with('.') || c.eq_ignore_ascii_case("AppData")
        })
    });
    (Path::new(&raiz) != home && !oculto).then_some(raiz)
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
    fn carpeta_general() {
        let base = std::env::temp_dir().join(format!("omoi-general-{}", std::process::id()));
        let home = base.join("usuario");
        std::fs::create_dir_all(home.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(home.join("suelta")).unwrap();
        let t = |p: &Path| de(p).unwrap();
        let home_c = PathBuf::from(t(&home));
        assert!(general_en(&t(&home), &home_c));
        assert!(general_en(&t(&base), &home_c), "una carpeta por encima del usuario también");
        assert!(!general_en(&t(&home.join("repo")), &home_c));
        assert!(!general_en(&t(&home.join("suelta")), &home_c), "una carpeta suelta sigue siendo su proyecto");
        assert!(!general_en("", &home_c));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn repo_de_una_ruta() {
        let base = std::env::temp_dir().join(format!("omoi-rutas-{}", std::process::id()));
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let home = PathBuf::from(de(&base).unwrap());
        let esperado = de(&repo).unwrap();
        let r = |s: &str| repo_de_ruta(s, &home);
        let dentro = repo.join("src").join("no-existe-aun.rs");
        assert_eq!(r(&format!("{}.", dentro.display())), Some(esperado.clone()));
        assert_eq!(r(&repo.display().to_string()), Some(esperado.clone()));
        assert_eq!(r("~/web/src"), Some(esperado));
        assert_eq!(r(&base.display().to_string()), None, "fuera de un repositorio");
        assert_eq!(r("src/main.rs"), None, "las relativas no dicen el proyecto");
        assert_eq!(r(&format!("{}?", repo.display())), Some(de(&repo).unwrap()));
        let clon = base.join(".claude").join("plugins").join("x");
        std::fs::create_dir_all(clon.join(".git")).unwrap();
        assert_eq!(r(&clon.display().to_string()), None, "un clon en una carpeta oculta no es un proyecto");
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
