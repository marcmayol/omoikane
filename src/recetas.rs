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
use std::collections::BTreeSet;
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

/// Opciones que llevan un valor detrás (`git -C <carpeta> diff`, `gh -R <repo> pr list`): el valor no
/// es el subcomando.
const CON_VALOR: &[&str] = &["-C", "-R", "--repo", "--prefix", "--manifest-path", "--cwd", "--dir", "-f", "--file"];
/// Lo que va delante del programa de verdad: intérpretes y envoltorios.
const DELANTE: &[&str] = &["bash", "sh", "zsh", "pwsh", "powershell", "cmd", "sudo", "doas", "runuser", "env", "time", "nohup", "exec", "call"];
/// Los verbos aprobados de PowerShell (`Get-Verb`): `Verbo-Nombre` es un cmdlet, una orden como
/// cualquier otra y no un nombre suelto. Los de Linux con la misma forma (`update-grub`, `start-stop-daemon`)
/// van en minúsculas; un cmdlet se reconoce por la mayúscula (`Remove-Item`) o, todo en minúsculas
/// (`get-childitem`, que PowerShell también acepta), si su verbo está en `VERBOS_CMDLET_MINUSCULAS`.
const VERBOS_POWERSHELL: &[&str] = &[
    "add", "approve", "assert", "backup", "block", "build", "checkpoint", "clear", "close", "compare", "complete", "compress", "confirm", "connect",
    "convert", "convertfrom", "convertto", "copy", "debug", "deny", "deploy", "disable", "disconnect", "dismount", "edit", "enable", "enter", "exit",
    "expand", "export", "find", "foreach", "format", "get", "grant", "group", "hide", "import", "initialize", "install", "invoke", "join", "limit",
    "lock", "measure", "merge", "mount", "move", "new", "open", "optimize", "out", "ping", "pop", "protect", "publish", "push", "read", "receive",
    "redo", "register", "remove", "rename", "repair", "request", "reset", "resize", "resolve", "restart", "restore", "resume", "revoke", "save",
    "search", "select", "send", "set", "show", "skip", "sort", "split", "start", "step", "stop", "submit", "suspend", "switch", "sync", "tee", "test",
    "trace", "unblock", "undo", "uninstall", "unlock", "unprotect", "unpublish", "unregister", "update", "use", "wait", "watch", "where", "write",
];
/// Los verbos que valen como cmdlet aunque vayan en minúsculas: los de siempre, que ningún programa de
/// Linux usa como prefijo.
const VERBOS_CMDLET_MINUSCULAS: &[&str] = &[
    "get", "set", "select", "where", "foreach", "out", "format", "measure", "sort", "test", "new", "remove", "copy", "move", "write", "invoke",
    "convertto", "convertfrom", "import", "export", "rename", "wait", "expand", "compress", "register", "unregister", "enable", "disable",
    "restart", "resolve", "publish", "uninstall", "unblock",
];
/// Cmdlets que solo miran, mueven ficheros o dan forma a la salida (el `ls`, `cat`, `rm`, `curl`…
/// de PowerShell), por su verbo: no dicen nada de un procedimiento.
const VERBOS_CMDLET_TRIVIALES: &[&str] = &[
    "get", "select", "where", "foreach", "out", "format", "measure", "sort", "test", "write", "read", "tee", "group", "compare", "join", "split",
    "resolve", "convertto", "convertfrom", "convert", "wait", "clear", "show", "hide", "new", "remove", "copy", "move", "rename", "set", "add", "push",
    "pop", "find", "search", "trace", "debug", "enter", "exit", "skip", "step", "limit", "use", "open", "close",
];
/// Y los que lo son aunque su verbo actúe.
const CMDLETS_TRIVIALES: &[&str] = &["invoke-webrequest", "invoke-restmethod", "invoke-expression", "invoke-item", "start-process", "start-sleep", "stop-process"];

/// Las órdenes de una línea de shell: corta por `&&`, `||`, `;`, `|`, saltos de línea, `$(` y comillas
/// invertidas fuera de las comillas. Lo entrecomillado queda como una sola palabra marcada con `\u{1}`
/// y solo su último trozo de ruta (`"C:/…/gh.exe" pr list` → `\u{1}gh.exe pr list`): como argumento se
/// descarta, y en el sitio del programa vale su nombre.
fn trozos(cmd: &str) -> Vec<String> {
    let mut r = Vec::new();
    let mut actual = String::new();
    let mut comilla: Option<char> = None;
    let mut dentro = String::new();
    let mut escapado = false;
    let mut cs = cmd.chars().peekable();
    while let Some(c) = cs.next() {
        if let Some(q) = comilla {
            if escapado {
                escapado = false;
                dentro.push(c);
            } else if c == '\\' && q == '"' {
                escapado = true;
            } else if c == q {
                comilla = None;
                let nombre = dentro.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").replace(char::is_whitespace, "_");
                actual.push_str(&format!(" \u{1}{nombre} "));
                dentro.clear();
            } else {
                dentro.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => comilla = Some(c),
            '$' if cs.peek() == Some(&'(') => {
                cs.next();
                r.push(std::mem::take(&mut actual));
            }
            '&' | ';' | '|' | '\n' | '`' | '(' | ')' => r.push(std::mem::take(&mut actual)),
            _ => actual.push(c),
        }
    }
    r.push(actual);
    r.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Un nombre de programa: letras, cifras, `-`, `_` no (`insert_content` es la acción de una herramienta
/// de edición con un campo `command`), y `.` (`python3.12`). En minúsculas y sin `.exe`.
fn programa(t: &str) -> Option<String> {
    let t = t.strip_prefix('\u{1}').unwrap_or(t);
    let p = t.rsplit(['/', '\\']).next().unwrap_or(t).to_lowercase();
    let p = p.strip_suffix(".exe").unwrap_or(&p).trim_matches(['{', '}', '[', ']']).to_string();
    let valido = p.starts_with(|c: char| c.is_ascii_alphabetic()) && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    valido.then_some(p)
}

/// Si `t` (tal cual se escribió) es un cmdlet de PowerShell (`Remove-Item`, `Invoke-Pester`,
/// `get-childitem`): su verbo, en minúsculas.
fn verbo_de_cmdlet(t: &str) -> Option<String> {
    let t = t.strip_prefix('\u{1}').unwrap_or(t);
    let t = t.rsplit(['/', '\\']).next().unwrap_or(t);
    let (verbo, nombre) = t.split_once('-')?;
    if nombre.is_empty() || !nombre.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let v = verbo.to_ascii_lowercase();
    let con_mayuscula = t.starts_with(|c: char| c.is_ascii_uppercase()) || nombre.starts_with(|c: char| c.is_ascii_uppercase());
    (VERBOS_POWERSHELL.contains(&v.as_str()) && (con_mayuscula || VERBOS_CMDLET_MINUSCULAS.contains(&v.as_str()))).then_some(v)
}

/// Opciones de `sudo` (y `doas`, `runuser`) que llevan un valor: `sudo -u postgres psql` es `psql`,
/// no `postgres`.
const CON_VALOR_SUDO: &[&str] = &["-u", "-g", "-U", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "--user", "--group", "--other-user", "--chdir", "--host", "--prompt", "--role", "--type", "--close-from", "--command-timeout"];
/// Y de `env`: `env -u VAR -C carpeta cargo test`.
const CON_VALOR_ENV: &[&str] = &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"];

/// Los pasos de una orden: el programa y su subcomando (`cargo build --release && git tag -a v1` →
/// `cargo build`, `git tag`), sin lo trivial ni lo que solo mira.
pub fn pasos_de_orden(cmd: &str) -> Vec<String> {
    // Un script de varias líneas suele ser de un solo uso; de un heredoc, solo cuenta su primera
    // línea (el resto es lo que se le pasa).
    let cmd = if cmd.contains("<<") { cmd.lines().next().unwrap_or("") } else { cmd };
    if cmd.lines().count() > 3 {
        return Vec::new();
    }
    let mut r = Vec::new();
    for parte in trozos(cmd) {
        // Fuera las redirecciones, las asignaciones y las opciones (con su valor si lo llevan).
        let mut ts: Vec<&str> = Vec::new();
        let mut saltar = false;
        for t in parte.split_whitespace() {
            if saltar {
                saltar = false;
                continue;
            }
            if CON_VALOR.contains(&t) {
                saltar = true;
                continue;
            }
            // Las opciones con valor del envoltorio que va justo antes (`sudo -u <usuario> <orden>`).
            let envoltorio = ts.last().and_then(|p| programa(p));
            if matches!(envoltorio.as_deref(), Some("sudo" | "doas" | "runuser")) && CON_VALOR_SUDO.contains(&t)
                || envoltorio.as_deref() == Some("env") && CON_VALOR_ENV.contains(&t)
            {
                saltar = true;
                continue;
            }
            if t.starts_with('-') || t.contains('=') || t.contains('>') || t.contains('<') || (t.starts_with('/') && t.len() <= 3) {
                continue;
            }
            ts.push(t);
        }
        // Lo que va delante (`bash -lc …`, `cmd /c …`, `sudo …`) no es el programa.
        let mut delante = false;
        while ts.first().and_then(|t| programa(t)).is_some_and(|p| DELANTE.contains(&p.as_str())) {
            ts.remove(0);
            delante = true;
        }
        // `powershell -Command "…"`: lo de dentro de las comillas no se ve, así que no es un paso.
        if delante && ts.first().is_some_and(|t| t.starts_with('\u{1}')) {
            continue;
        }
        let Some(prog) = ts.first().and_then(|t| programa(t)) else { continue };
        if TRIVIALES.contains(&prog.as_str()) {
            continue;
        }
        // Un cmdlet es una orden entera: lo que lleva detrás son argumentos, no un subcomando
        // (`Restart-Service omoikane` es `restart-service`). Fuera los que solo miran o mueven ficheros.
        if let Some(verbo) = ts.first().and_then(|t| verbo_de_cmdlet(t)) {
            if !VERBOS_CMDLET_TRIVIALES.contains(&verbo.as_str()) && !CMDLETS_TRIVIALES.contains(&prog.as_str()) {
                r.push(prog);
            }
            continue;
        }
        let sub = ts.get(1).filter(|s| !s.starts_with('\u{1}') && s.len() > 1 && s.chars().all(|c| c.is_ascii_lowercase() || c == '-')).copied().unwrap_or("");
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
            for p in pasos_de_orden(c) {
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

/// Cuántas sesiones, de las más recientes, se miran para contar.
const MAX_SESIONES: usize = 500;

/// Las palabras de un texto para ver si una receta nombra un paso: sin signos alrededor, en minúsculas.
fn palabras(t: &str) -> std::collections::HashSet<String> {
    t.to_lowercase().split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.')).map(|w| w.trim_matches('.').to_string()).filter(|w| !w.is_empty()).collect()
}

/// De las secuencias de `actuales` (los pasos de la sesión que se cierra), la que más se repite en las
/// sesiones guardadas (en al menos `MIN_SESIONES`, con dos programas distintos o más), que ninguna
/// receta guardada cubre y que no se propuso antes, con en cuántas sesiones sale. Solo se miran las de
/// esta sesión: así el coste no crece con el historial y la propuesta cae en el proyecto donde pasa.
pub fn propuesta(db: &Connection, actuales: &[String]) -> Result<Option<(Vec<String>, usize)>> {
    let hay: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_steps')", [], |f| f.get(0))?;
    if !hay || actuales.len() < MIN_PASOS {
        return Ok(None);
    }
    // Las candidatas: las secuencias de esta sesión.
    let mut cuenta: std::collections::HashMap<Vec<String>, BTreeSet<usize>> = std::collections::HashMap::new();
    for n in MIN_PASOS..=MAX_PASOS {
        for w in actuales.windows(n) {
            let programas: BTreeSet<&str> = w.iter().map(|p| p.split(' ').next().unwrap_or("")).collect();
            if w.iter().collect::<BTreeSet<_>>().len() == n && programas.len() >= 2 {
                cuenta.entry(w.to_vec()).or_default();
            }
        }
    }
    if cuenta.is_empty() {
        return Ok(None);
    }
    // En qué sesiones guardadas sale cada una (sin copiar: se busca con el trozo).
    let mut q = db.prepare("SELECT steps FROM session_steps ORDER BY at DESC LIMIT ?1")?;
    let filas = q.query_map([MAX_SESIONES as i64], |f| f.get::<_, String>(0))?;
    for (i, texto) in filas.enumerate() {
        let pasos: Vec<String> = serde_json::from_str(&texto?).unwrap_or_default();
        for n in MIN_PASOS..=MAX_PASOS {
            for w in pasos.windows(n) {
                if let Some(s) = cuenta.get_mut(w) {
                    s.insert(i);
                }
            }
        }
    }
    let mut v: Vec<(Vec<String>, BTreeSet<usize>)> = cuenta.into_iter().filter(|(_, s)| s.len() >= MIN_SESIONES).collect();
    // Fuera las que están dentro de otra más larga que sale en las mismas sesiones.
    let todas = v.clone();
    v.retain(|(w, s)| !todas.iter().any(|(o, so)| o.len() > w.len() && so == s && o.windows(w.len()).any(|x| x == w.as_slice())));
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(&b.0)));
    let recetas: Vec<std::collections::HashSet<String>> = {
        let mut q = db.prepare("SELECT title || ' ' || content FROM memory WHERE kind = ?1")?;
        q.query_map([Tipo::Procedure.as_str()], |f| f.get::<_, String>(0))?.filter_map(|t| t.ok()).map(|t| palabras(&t)).collect()
    };
    let ya: std::collections::HashSet<String> = {
        let mut q = db.prepare("SELECT k FROM meta WHERE k LIKE 'receta_propuesta:%'")?;
        q.query_map([], |f| f.get::<_, String>(0))?.filter_map(|k| k.ok()).collect()
    };
    for (w, s) in v {
        // Cubierta si una receta nombra cada paso: su programa y su subcomando, aunque no vayan juntos
        // («git add -A, commit y push» cubre `git commit`).
        let cubierta = recetas.iter().any(|ps| w.iter().all(|p| p.split(' ').all(|x| ps.contains(x))));
        if !cubierta && !ya.contains(&format!("receta_propuesta:{}", w.join(" > "))) {
            return Ok(Some((w, s.len())));
        }
    }
    Ok(None)
}

/// Al cerrar: si entre los pasos de esta sesión hay una secuencia para proponer, la aparta como
/// candidata para la sesión siguiente en `project` (y no la vuelve a proponer). Devuelve si apartó una.
pub fn proponer(db: &Connection, project: &str, session: &str, actuales: &[String]) -> Result<bool> {
    let Some((w, n)) = propuesta(db, actuales)? else { return Ok(false) };
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
        // Lo que encontró la revisión de la 1.5.0.
        assert_eq!(pasos_de_orden("cargo test 2>&1 | Select-Object -Last 20"), ["cargo test"]);
        assert_eq!(pasos_de_orden("bash -lc cargo test && git push"), ["cargo test", "git push"]);
        assert_eq!(pasos_de_orden("cmd /c npm run build"), ["npm run"]);
        assert!(pasos_de_orden(r#"powershell -Command "Remove-Item -Recurse D:\tmp\build""#).is_empty());
        assert!(pasos_de_orden(r#"bash -c "cargo test --manifest-path a/Cargo.toml""#).is_empty());
        assert_eq!(pasos_de_orden("git -C D:/Omoikane diff && gh -R o/r pr list"), ["gh pr"]);
        assert_eq!(pasos_de_orden("python - <<'EOF'\nimport json\nEOF"), Vec::<String>::new());
        assert_eq!(pasos_de_orden("\"/c/Program Files/GitHub CLI/gh.exe\" pr list"), ["gh pr"]);
        assert_eq!(pasos_de_orden("git commit -m \"say \\\" ; docker push x\""), ["git commit"]);
        assert_eq!(pasos_de_orden("x=`git rev-parse HEAD` && cargo build"), ["git rev-parse", "cargo build"]);
        assert_eq!(pasos_de_orden("GIT.EXE tag v1"), ["git tag"]);
    }

    /// 1.8.6: los cmdlets de PowerShell son órdenes (con su nombre entero, sin subcomando), y los que
    /// solo miran o mueven ficheros se quitan como `ls` o `rm`.
    #[test]
    fn cmdlets_de_powershell() {
        assert_eq!(pasos_de_orden("Invoke-Pester -Path tests; git push"), ["invoke-pester", "git push"]);
        assert_eq!(pasos_de_orden("Restart-Service omoikane"), ["restart-service"]);
        assert_eq!(pasos_de_orden("Install-Module PSReadLine -Scope CurrentUser"), ["install-module"]);
        assert_eq!(pasos_de_orden("Publish-Module -Name Omoi -NuGetApiKey $k"), ["publish-module"]);
        assert_eq!(pasos_de_orden("cargo build --release; Compress-Archive -Path target/release/omoi.exe -DestinationPath omoi.zip"), ["cargo build", "compress-archive"]);
        assert_eq!(pasos_de_orden("Register-ScheduledTask -TaskName vigilante -Xml (Get-Content x.xml | Out-String)"), ["register-scheduledtask"]);
        assert_eq!(pasos_de_orden("Update-Help"), ["update-help"]);
        assert_eq!(pasos_de_orden(r"unblock-file .\omoi.exe"), ["unblock-file"]);
        // Lo que solo mira, mueve ficheros o da forma a la salida, fuera.
        assert!(pasos_de_orden("Get-ChildItem -Recurse src | Select-Object -First 5").is_empty());
        assert!(pasos_de_orden(r"Remove-Item -Recurse -Force D:\tmp\x; Set-Location D:\x; New-Item -ItemType Directory y").is_empty());
        assert!(pasos_de_orden("Invoke-WebRequest -Uri https://x -OutFile y; Start-Sleep 2; Start-Process notepad").is_empty());
        assert!(pasos_de_orden("get-content a.txt | where-object { $_ -match 'x' }").is_empty());
        assert_eq!(pasos_de_orden("cargo test 2>&1 | Select-Object -Last 20"), ["cargo test"]);
        // Los de Linux con forma de cmdlet, en minúsculas, siguen siendo programas.
        assert_eq!(pasos_de_orden("update-grub"), ["update-grub"]);
        assert_eq!(pasos_de_orden("docker-compose up -d"), ["docker-compose up"]);
        assert_eq!(pasos_de_orden("pre-commit run --all-files"), ["pre-commit run"]);
    }

    /// 1.8.6: `sudo -u <usuario> <orden>` es `<orden>`, no el usuario.
    #[test]
    fn sudo_con_usuario() {
        assert_eq!(pasos_de_orden("sudo -u postgres psql -c 'select 1'"), ["psql"]);
        assert_eq!(pasos_de_orden("sudo -u www-data git -C /srv/app pull"), ["git pull"]);
        assert_eq!(pasos_de_orden("sudo -E -u marc docker compose up -d"), ["docker compose"]);
        assert_eq!(pasos_de_orden("sudo --user=marc systemctl restart caddy"), ["systemctl restart"]);
        assert_eq!(pasos_de_orden("sudo -g docker -u marc cargo build"), ["cargo build"]);
        assert_eq!(pasos_de_orden("sudo systemctl restart caddy"), ["systemctl restart"]);
        assert_eq!(pasos_de_orden("doas -u marc make install"), ["make install"]);
        assert_eq!(pasos_de_orden("env -u RUST_LOG cargo test"), ["cargo test"]);
        // `-u` de otra orden no se come nada: `git push -u origin main` sigue siendo `git push`.
        assert_eq!(pasos_de_orden("git push -u origin main"), ["git push"]);
    }
}
