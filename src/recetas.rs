//! Proponer recetas (1.5): las secuencias de órdenes que funcionaron y se repiten en varias sesiones
//! son candidatas a receta (memoria `procedure`). Al cerrar cada sesión se apuntan sus pasos, en una
//! tabla local (`session_steps`, como el diario: no viaja ni sale en `omoi export`); si alguna
//! secuencia ya sale en `MIN_SESIONES` sesiones, ninguna receta guardada la cubre y no se propuso
//! antes, queda apartada como candidata para la sesión siguiente. Decide el agente con el usuario:
//! Omoikane no sabe si es un procedimiento de verdad.
//!
//! Calibrado con las conversaciones de Marc (4-nov-2026, 121 sesiones con órdenes): sin quitar lo
//! que solo mira (`git status`, `git log`…) ni pedir dos programas distintos, lo más repetido era
//! `git add → git commit → git push`, que hace todo el mundo; con esos filtros sale, por ejemplo,
//! `git fetch → gh issue → gh pr` (revisar si contestaron en sus PR), que no tenía receta.

use crate::memoria::{AHORA, Tipo};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// En cuántas sesiones distintas tiene que salir una secuencia.
pub const MIN_SESIONES: usize = 5;
/// Cuántos días de sesiones se miran (y se guardan).
const DIAS: i64 = 90;
/// Largo de las secuencias, en pasos.
const MIN_PASOS: usize = 3;
const MAX_PASOS: usize = 6;
/// Cuántos pasos de una sesión se guardan, como mucho.
const MAX_PASOS_SESION: usize = 300;

/// El tipo de las candidatas que propone.
pub const RECETA: &str = "recipe";

/// Programas que no dicen nada de un procedimiento: mirar, mover ficheros, la propia shell.
const TRIVIALES: &[&str] = &[
    "ls", "cd", "cat", "grep", "rg", "head", "tail", "echo", "sleep", "pwd", "wc", "sed", "find", "mkdir", "rm", "cp", "mv", "python", "python3", "powershell", "pwsh",
    "cmd", "bash", "sh", "for", "sort", "uniq", "awk", "cut", "tr", "du", "date", "timeout", "export", "test", "true", "set", "printf", "diff", "od", "file", "which",
    "type", "source", "node", "taskkill", "curl", "do", "done", "then", "fi", "else", "if", "while", "until", "break", "continue", "in", "exit", "return", "local", "read",
    "xargs", "tee", "start", "ps", "kill", "jq", "dir", "where", "get-content", "select-string", "write-host", "tasklist", "cygpath", "chmod",
];
/// Pasos que solo miran.
const SOLO_MIRAN: &[&str] = &["git status", "git log", "git diff", "git show", "git branch", "git remote", "gh run", "cargo tree", "cargo metadata"];

/// Las órdenes de una línea de shell: corta por `&&`, `||`, `;`, `|` y saltos de línea fuera de las
/// comillas (dentro van patrones de grep, mensajes de commit…).
fn trozos(cmd: &str) -> Vec<String> {
    let mut r = Vec::new();
    let mut actual = String::new();
    let mut comilla: Option<char> = None;
    for c in cmd.chars() {
        match comilla {
            Some(q) if c == q => {
                comilla = None;
                continue;
            }
            Some(_) => continue,
            None if c == '"' || c == '\'' => {
                comilla = Some(c);
                continue;
            }
            None if matches!(c, '&' | ';' | '|' | '\n') => {
                r.push(std::mem::take(&mut actual));
                continue;
            }
            None => actual.push(c),
        }
    }
    r.push(actual);
    r.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Los pasos de una orden: el programa y su subcomando (`cargo build --release && git tag -a v1` →
/// `cargo build`, `git tag`), sin lo trivial ni lo que solo mira.
pub fn pasos_de_orden(cmd: &str) -> Vec<String> {
    // Un script de varias líneas suele ser de un solo uso.
    if cmd.lines().count() > 3 {
        return Vec::new();
    }
    let mut r = Vec::new();
    // Lo de dentro de `$(…)` es otra orden: `id=$(gh run list …)` es `gh run`, no `run list`.
    for parte in trozos(&cmd.replace("$(", ";")) {
        let ts: Vec<&str> = parte.split_whitespace().filter(|t| !t.starts_with('-') && !t.contains('=') && !t.contains('>') && !t.contains('<')).collect();
        let Some(prog) = ts.first() else { continue };
        let prog = prog.rsplit(['/', '\\']).next().unwrap_or(prog).trim_end_matches(".exe").trim_matches(['(', ')', '{', '}']).to_lowercase();
        // Un nombre de programa: letras, cifras, `-` y `.` (no `insert_content`, que es la acción de una
        // herramienta de edición con un campo `command`).
        if prog.is_empty() || TRIVIALES.contains(&prog.as_str()) || !prog.starts_with(|c: char| c.is_ascii_alphabetic()) || !prog.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            continue;
        }
        let sub = ts.get(1).filter(|s| s.len() > 1 && s.chars().all(|c| c.is_ascii_lowercase() || c == '-')).copied().unwrap_or("");
        let paso = if sub.is_empty() { prog } else { format!("{prog} {sub}") };
        if !SOLO_MIRAN.contains(&paso.as_str()) {
            r.push(paso);
        }
    }
    r
}

/// Los pasos de una conversación, en orden, sin repetir el mismo seguido.
pub fn pasos(ruta: &Path) -> Result<Vec<String>> {
    Ok(pasos_de_turnos(&crate::candidatas::turnos(ruta)?))
}

/// `pasos` con los turnos ya leídos.
pub fn pasos_de_turnos(turnos: &[crate::candidatas::Turno]) -> Vec<String> {
    let mut r: Vec<String> = Vec::new();
    for t in turnos {
        if let crate::candidatas::Turno::Comando(c) = t {
            for p in pasos_de_orden(&c) {
                if r.last() != Some(&p) {
                    r.push(p);
                }
            }
        }
    }
    r.truncate(MAX_PASOS_SESION);
    r
}

fn crear(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS session_steps (session TEXT PRIMARY KEY, at TEXT NOT NULL, steps TEXT NOT NULL);")?;
    Ok(())
}

/// Al cerrar o compactar: apunta los pasos de la sesión (si tiene) y borra los de hace más de `DIAS`.
pub fn apuntar(db: &Connection, session: &str, pasos: &[String]) -> Result<()> {
    if session.is_empty() || pasos.is_empty() {
        return Ok(());
    }
    crear(db)?;
    db.execute(&format!("INSERT OR REPLACE INTO session_steps(session, at, steps) VALUES (?1, {AHORA}, ?2)"), params![session, serde_json::to_string(pasos)?])?;
    db.execute(&format!("DELETE FROM session_steps WHERE julianday({AHORA}) - julianday(at) > {DIAS}"), [])?;
    Ok(())
}

/// La secuencia que más se repite (en al menos `MIN_SESIONES` sesiones, con dos programas distintos
/// o más), que ninguna receta guardada cubre y que no se propuso antes, con en cuántas sesiones sale.
pub fn propuesta(db: &Connection) -> Result<Option<(Vec<String>, usize)>> {
    let hay: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_steps')", [], |f| f.get(0))?;
    if !hay {
        return Ok(None);
    }
    let sesiones: Vec<(String, String)> = {
        let mut q = db.prepare("SELECT session, steps FROM session_steps")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut cuenta: BTreeMap<Vec<String>, BTreeSet<&str>> = BTreeMap::new();
    for (s, texto) in &sesiones {
        let pasos: Vec<String> = serde_json::from_str(texto).unwrap_or_default();
        let mut vistas = BTreeSet::new();
        for n in MIN_PASOS..=MAX_PASOS {
            for w in pasos.windows(n) {
                let programas: BTreeSet<&str> = w.iter().map(|p| p.split(' ').next().unwrap_or("")).collect();
                if w.iter().collect::<BTreeSet<_>>().len() == n && programas.len() >= 2 && vistas.insert(w.to_vec()) {
                    cuenta.entry(w.to_vec()).or_default().insert(s.as_str());
                }
            }
        }
    }
    let mut v: Vec<(Vec<String>, usize, BTreeSet<&str>)> = cuenta.into_iter().filter(|(_, s)| s.len() >= MIN_SESIONES).map(|(w, s)| (w, s.len(), s)).collect();
    // Fuera las que están dentro de otra más larga que sale en las mismas sesiones.
    let todas = v.clone();
    v.retain(|(w, _, s)| !todas.iter().any(|(o, _, so)| o.len() > w.len() && so == s && o.windows(w.len()).any(|x| x == w.as_slice())));
    v.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(&b.0)));
    let recetas: Vec<String> = {
        let mut q = db.prepare("SELECT lower(title || ' ' || content) FROM memory WHERE kind = ?1")?;
        q.query_map([Tipo::Procedure.as_str()], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for (w, n, _) in v {
        // Cubierta si una receta nombra cada paso: su programa y su subcomando, aunque no vayan juntos
        // («git add -A, commit y push» cubre `git commit`).
        let palabras = |r: &str| -> BTreeSet<String> { r.split(|c: char| !(c.is_alphanumeric() || c == '-')).map(String::from).collect() };
        let cubierta = recetas.iter().any(|r| {
            let ps = palabras(r);
            w.iter().all(|p| p.split(' ').all(|x| ps.contains(x)))
        });
        let clave = format!("receta_propuesta:{}", w.join(" > "));
        let propuesta: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE k = ?1)", [&clave], |f| f.get(0))?;
        if !cubierta && !propuesta {
            return Ok(Some((w, n)));
        }
    }
    Ok(None)
}

/// Al cerrar: si hay una secuencia para proponer, la aparta como candidata para la sesión siguiente
/// en `project` (y no la vuelve a proponer). Devuelve si apartó una.
pub fn proponer(db: &Connection, project: &str, session: &str) -> Result<bool> {
    let Some((w, n)) = propuesta(db)? else { return Ok(false) };
    let texto = format!(
        "steps repeated in {n} sessions: `{}`. If it is a procedure the user follows, ask them and save it as a recipe (kind procedure, numbered steps and a `when:` line); if not, ignore it",
        w.join("` → `")
    );
    db.execute(&format!("INSERT OR IGNORE INTO candidate(project, session, kind, text, created_at) VALUES (?1, ?2, ?3, ?4, {AHORA})"), params![project, session, RECETA, texto])?;
    let clave = format!("receta_propuesta:{}", w.join(" > "));
    db.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, {AHORA})"), [&clave])?;
    Ok(true)
}

/// Las secuencias que se propusieron (para la vista y para comprobar).
pub fn propuestas(db: &Connection) -> Result<Vec<String>> {
    let mut q = db.prepare("SELECT substr(k, 18) FROM meta WHERE k LIKE 'receta_propuesta:%' ORDER BY v DESC")?;
    let v = q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Lo apuntado de una sesión (para los tests).
pub fn de_sesion(db: &Connection, session: &str) -> Result<Option<Vec<String>>> {
    let hay: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_steps')", [], |f| f.get(0))?;
    if !hay {
        return Ok(None);
    }
    let t: Option<String> = db.query_row("SELECT steps FROM session_steps WHERE session = ?1", [session], |f| f.get(0)).optional()?;
    Ok(t.and_then(|t| serde_json::from_str(&t).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasos_de_una_orden() {
        assert_eq!(pasos_de_orden("cargo build --release 2>&1 && git tag -a v1.4.1 -m \"1.4.1\" && git push origin v1.4.1"), ["cargo build", "git tag", "git push"]);
        assert_eq!(pasos_de_orden("git status --short | head; cargo test -q 2>&1 | grep -E \"FAILED|panicked\""), ["cargo test"]);
        assert_eq!(pasos_de_orden("cd D:/x && ls"), Vec::<String>::new());
        assert_eq!(pasos_de_orden("C:/Users/m/.cargo/bin/omoi.exe recall --session-end"), ["omoi recall"]);
        assert_eq!(pasos_de_orden("id=$(gh run list -L 5) && gh release view v1"), ["gh release"]);
        assert_eq!(pasos_de_orden("insert_content"), Vec::<String>::new());
    }
}
