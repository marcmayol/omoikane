//! Procedencia (1.8.5, de OntoSmith: cada hecho atado a su fuente): con cada memoria nueva se
//! apunta en qué sesión del agente y en qué commit del proyecto se escribió, para poder volver a la
//! fuente (la conversación, el código de ese momento).
//!
//! Va en `memory.extra` con la clave [`CLAVE`], como los datos y la verificación: sin cambiar el
//! esquema, viaja como un campo `provenance` de la línea y una 1.0–1.8 lo conserva sin entenderlo.
//! Es la de cuando se creó: cambiarla después no la toca (el historial dice cuándo cambió).
//!
//! Barato a propósito, porque va en cada `remember`: la sesión sale del entorno o de la tabla
//! `session` que ya llenan los hooks, y el commit de leer `.git/HEAD` y su referencia (dos o tres
//! ficheros pequeños), sin lanzar `git`.
//!
//! La sesión, por orden: `OMOIKANE_SESSION` (quien llame la dice), `CLAUDE_CODE_SESSION_ID` (Claude
//! Code la pone en el entorno de lo que lanza: su shell y sus servidores MCP), y si no, la única
//! sesión que los hooks hayan apuntado en ese proyecto en las últimas 12 horas. Con varias a la vez
//! en el mismo proyecto no se adivina: mejor sin sesión que con la de otra conversación.
//!
//! Una memoria de oficio no lleva procedencia: es de todos los proyectos de su oficio y no debe
//! llevar nada de un cliente (ni el nombre de su rama). Una global o de un espacio sin proyecto, solo
//! la sesión: el commit y la rama son de la carpeta en la que se escribió, que puede ser la de un
//! cliente, y esas memorias se ven desde otros. Lo mismo al moverla después (promoverla a un oficio la
//! deja sin procedencia; a global, sin commit ni rama) y al leerla o exportarla ([`para_memoria`]).

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// La clave en `memory.extra` (y el campo `provenance` de la línea que viaja).
pub const CLAVE: &str = "provenance";


/// De dónde salió una memoria.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Procedencia {
    /// El id de la sesión del agente.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// El commit (HEAD) del repositorio del proyecto.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    /// La rama de ese HEAD (vacía con un HEAD suelto).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
}

impl Procedencia {
    pub fn vacia(&self) -> bool {
        self.session.is_empty() && self.commit.is_empty()
    }

    /// En una línea: «session 2bd891d2… · commit e564a66 (master)».
    pub fn texto(&self) -> String {
        let mut v = Vec::new();
        if !self.session.is_empty() {
            v.push(format!("session {}", self.session));
        }
        if !self.commit.is_empty() {
            let rama = if self.branch.is_empty() { String::new() } else { format!(" ({})", self.branch) };
            v.push(format!("commit {}{rama}", self.commit.get(..12).unwrap_or(&self.commit)));
        }
        v.join(" · ")
    }
}

/// La de su columna en `memoria::COLUMNAS` (el JSON de `extra.provenance`).
pub(crate) fn de_columna(json: Option<&str>) -> Option<Procedencia> {
    serde_json::from_str::<Procedencia>(json?).ok().filter(|p| !p.vacia())
}

/// La procedencia de lo que se guarda ahora en la carpeta de proyecto `proyecto` (la raíz, como la da
/// `proyecto::de`; vacía: global sin carpeta), o `None` si no hay nada que apuntar.
pub fn actual(db: &Connection, proyecto: &str) -> Option<Procedencia> {
    let session = sesion(db, proyecto).unwrap_or_default();
    let (commit, branch) = if proyecto.is_empty() { None } else { commit_de(Path::new(proyecto)) }.unwrap_or_default();
    let p = Procedencia { session, commit, branch };
    (!p.vacia()).then_some(p)
}

/// Solo la sesión del agente (sin commit ni rama), para lo que no es del proyecto de la carpeta
/// `proyecto`: una memoria global, de un espacio sin proyecto o de oficio no debe llevar la rama de la
/// carpeta en la que se escribió, que puede ser de un cliente (`feature/acme-…`) y se vería desde
/// cualquier otro.
pub fn solo_sesion(db: &Connection, proyecto: &str) -> Option<Procedencia> {
    let p = Procedencia { session: sesion(db, proyecto).unwrap_or_default(), ..Default::default() };
    (!p.vacia()).then_some(p)
}

/// La que puede llevar una memoria según dónde está ahora: ninguna si es de oficio (es de todos los
/// proyectos de su oficio), solo la sesión si no es de un proyecto (global o de un espacio), y entera si
/// es de un proyecto. Vale para lo que se guardó antes de recortarla y para lo que se mueve después.
pub(crate) fn para_memoria(p: Option<Procedencia>, project_id: i64, tags: &[String]) -> Option<Procedencia> {
    let mut p = p?;
    if tags.iter().any(|t| t.starts_with(crate::oficios::PREFIJO)) {
        return None;
    }
    if project_id == 0 {
        p.commit.clear();
        p.branch.clear();
    }
    (!p.vacia()).then_some(p)
}

/// [`para_memoria`] sobre un `extra` (el de `memory.extra` o el de una línea que sale): quita la clave
/// o la recorta. `true` si ha cambiado algo.
pub(crate) fn recortar_extra(extra: &mut serde_json::Map<String, serde_json::Value>, project_id: i64, tags: &[String]) -> bool {
    // Una que no se entiende (de una versión futura con otra forma) se deja como está.
    let Some(antes) = extra.get(CLAVE).and_then(|v| serde_json::from_value::<Procedencia>(v.clone()).ok()) else { return false };
    match para_memoria(Some(antes.clone()), project_id, tags) {
        Some(p) if p == antes => false,
        Some(p) => {
            extra.insert(CLAVE.into(), serde_json::to_value(p).unwrap_or_default());
            true
        }
        None => {
            extra.remove(CLAVE);
            true
        }
    }
}

/// Lo que se pone en `memory.extra` al crear una memoria con esta procedencia.
pub(crate) fn extra(p: &Procedencia) -> String {
    serde_json::json!({ CLAVE: p }).to_string()
}

fn variable(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty() && v.len() <= 200)
}

/// La sesión del agente que guarda (ver el principio del módulo).
fn sesion(db: &Connection, proyecto: &str) -> Option<String> {
    if let Some(s) = variable("OMOIKANE_SESSION").or_else(|| variable("CLAUDE_CODE_SESSION_ID")) {
        return Some(s);
    }
    if proyecto.is_empty() {
        return None;
    }
    let mut q = db
        .prepare_cached(
            "SELECT session FROM session WHERE project = ?1 AND session != ''
               AND started_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-12 hours') LIMIT 2",
        )
        .ok()?;
    let v: Vec<String> = q.query_map(params![proyecto], |f| f.get(0)).ok()?.filter_map(|x| x.ok()).collect();
    match v.as_slice() {
        [una] => Some(una.clone()),
        _ => None,
    }
}

/// El commit de HEAD y su rama en el repositorio con raíz `raiz`, leyendo los ficheros de git: `.git`
/// (o, en un worktree, el fichero `gitdir:` y su `commondir`), `HEAD`, la referencia suelta o
/// `packed-refs`. `None` si no es un repositorio, aún no tiene commits o no se entiende (reftable).
pub fn commit_de(raiz: &Path) -> Option<(String, String)> {
    // Cada fichero que se abre cuesta (en Windows, ~0,1 ms): los mínimos. Un repositorio normal: HEAD
    // y su rama; un worktree: además `.git` y `commondir`.
    // Local, o en el recurso de red de la carpeta en la que se trabaja (`proyecto::legible`).
    if !crate::proyecto::legible(raiz) {
        return None;
    }
    let punto = raiz.join(".git");
    let (gitdir, comun) = match std::fs::read_to_string(&punto) {
        // `.git` es un fichero: un worktree (o un submódulo, sin `commondir`). La carpeta común (la del
        // repositorio principal) es donde están las ramas. Ni una ni otra pueden ser de otro recurso de red.
        Ok(t) => {
            let gitdir = crate::proyecto::si_legible(raiz.join(t.trim().strip_prefix("gitdir:")?.trim()))?;
            let comun = std::fs::read_to_string(gitdir.join("commondir")).map(|c| gitdir.join(c.trim())).unwrap_or_else(|_| gitdir.clone());
            let comun = crate::proyecto::si_legible(comun)?;
            (gitdir, comun)
        }
        Err(_) => (punto.clone(), punto),
    };
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(referencia) = head.strip_prefix("ref:").map(str::trim) else {
        return es_sha(head).then(|| (head.to_string(), String::new()));
    };
    let rama = referencia.strip_prefix("refs/heads/").unwrap_or(referencia).to_string();
    // Las ramas, en la común; lo propio de un worktree (`refs/bisect`…), en la suya.
    let dirs = if gitdir == comun { vec![&comun] } else { vec![&comun, &gitdir] };
    for dir in dirs {
        if let Ok(s) = std::fs::read_to_string(dir.join(referencia)) {
            let s = s.trim();
            if es_sha(s) {
                return Some((s.to_string(), rama));
            }
        }
    }
    let empaquetadas = std::fs::read_to_string(comun.join("packed-refs")).ok()?;
    empaquetadas
        .lines()
        .filter_map(|l| l.split_once(' '))
        .find(|(sha, r)| r.trim() == referencia && es_sha(sha))
        .map(|(sha, _)| (sha.to_string(), rama))
}

/// SHA-1 (40) o SHA-256 (64) en hex.
fn es_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_head_suelto_rama_y_empaquetada() {
        let t = crate::comun::Temporal::nueva("procedencia");
        let raiz = t.join("repo");
        let git = raiz.join(".git");
        std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        assert_eq!(commit_de(&raiz), None, "sin HEAD");
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        // Rama con su fichero.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs").join("heads").join("main"), format!("{a}\n")).unwrap();
        assert_eq!(commit_de(&raiz), Some((a.clone(), "main".into())));
        // Rama solo en packed-refs.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/vieja\n").unwrap();
        std::fs::write(git.join("packed-refs"), format!("# pack-refs with: peeled fully-peeled sorted\n{b} refs/heads/vieja\n")).unwrap();
        assert_eq!(commit_de(&raiz), Some((b.clone(), "vieja".into())));
        // Rama sin commits.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/nueva\n").unwrap();
        assert_eq!(commit_de(&raiz), None);
        // HEAD suelto.
        std::fs::write(git.join("HEAD"), format!("{a}\n")).unwrap();
        assert_eq!(commit_de(&raiz), Some((a.clone(), String::new())));
        // Un worktree: `.git` es un fichero, HEAD propio y las ramas en la carpeta común.
        let wt = t.join("wt");
        let gitdir = git.join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(commit_de(&wt), Some((a, "main".into())));
        assert_eq!(commit_de(&t.join("no-existe")), None);
    }

    #[test]
    fn texto_corto() {
        let p = Procedencia { session: "s1".into(), commit: "0123456789abcdef0123456789abcdef01234567".into(), branch: "main".into() };
        assert_eq!(p.texto(), "session s1 · commit 0123456789ab (main)");
        assert!(de_columna(Some("{}")).is_none());
    }
}
