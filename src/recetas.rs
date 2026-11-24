//! Proposing recipes (1.5): the command sequences that worked and repeat across several sessions
//! are recipe candidates (a `procedure` memory). When each session closes its steps are noted, in a
//! local table (`session_steps`, like the journal: it doesn't travel nor show up in `omoi export`); if some
//! sequence already shows up in `MIN_SESSIONS` sessions, no saved recipe covers it and it wasn't proposed
//! before, it is set aside as a candidate for the next session. The agent decides with the user:
//! Omoikane doesn't know if it is really a procedure.
//!
//! Calibrated with Marc's conversations (4-nov-2026, 121 sessions with commands): without removing what
//! only looks (`git status`, `git log`…) nor requiring two different programs, the most repeated was
//! `git add → git commit → git push`, which everybody does; with those filters you get, for example,
//! `git fetch → gh issue → gh pr` (checking if they answered on their PRs), which had no recipe.

use crate::memoria::{NOW, Kind};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;
use std::path::Path;

/// In how many different sessions a sequence has to show up.
pub const MIN_SESSIONS: usize = 5;
/// How many days of sessions are looked at (and kept).
const DAYS: i64 = 90;
/// Length of the sequences, in steps.
const MIN_STEPS: usize = 3;
const MAX_STEPS: usize = 6;
/// How many steps of a session are kept, at most.
const MAX_SESSION_STEPS: usize = 300;

/// The kind of the candidates it proposes.
pub const RECIPE: &str = "recipe";

/// Programs that say nothing about a procedure: looking, moving files, the shell itself.
const TRIVIAL: &[&str] = &[
    "ls", "cd", "cat", "grep", "rg", "head", "tail", "echo", "sleep", "pwd", "wc", "sed", "find", "mkdir", "rm", "cp", "mv", "python", "python3", "powershell", "pwsh",
    "cmd", "bash", "sh", "for", "sort", "uniq", "awk", "cut", "tr", "du", "date", "timeout", "export", "test", "true", "set", "printf", "diff", "od", "file", "which",
    "type", "source", "node", "taskkill", "curl", "do", "done", "then", "fi", "else", "if", "while", "until", "break", "continue", "in", "exit", "return", "local", "read",
    "xargs", "tee", "start", "ps", "kill", "jq", "dir", "where", "get-content", "select-string", "write-host", "tasklist", "cygpath", "chmod",
];
/// Steps that only look.
const READ_ONLY: &[&str] = &["git status", "git log", "git diff", "git show", "git branch", "git remote", "gh run", "cargo tree", "cargo metadata"];

/// Options followed by a value (`git -C <folder> diff`, `gh -R <repo> pr list`): the value is not
/// the subcommand.
const TAKES_VALUE: &[&str] = &["-C", "-R", "--repo", "--prefix", "--manifest-path", "--cwd", "--dir", "-f", "--file"];
/// What goes in front of the real program: interpreters and wrappers.
const WRAPPERS: &[&str] = &["bash", "sh", "zsh", "pwsh", "powershell", "cmd", "sudo", "doas", "runuser", "env", "time", "nohup", "exec", "call"];
/// PowerShell's approved verbs (`Get-Verb`): `Verb-Noun` is a cmdlet, a command like
/// any other and not a stray name. Linux ones with the same shape (`update-grub`, `start-stop-daemon`)
/// are lowercase; a cmdlet is recognized by the capital letter (`Remove-Item`) or, all lowercase
/// (`get-childitem`, which PowerShell also accepts), if its verb is in `LOWERCASE_CMDLET_VERBS`.
const POWERSHELL_VERBS: &[&str] = &[
    "add", "approve", "assert", "backup", "block", "build", "checkpoint", "clear", "close", "compare", "complete", "compress", "confirm", "connect",
    "convert", "convertfrom", "convertto", "copy", "debug", "deny", "deploy", "disable", "disconnect", "dismount", "edit", "enable", "enter", "exit",
    "expand", "export", "find", "foreach", "format", "get", "grant", "group", "hide", "import", "initialize", "install", "invoke", "join", "limit",
    "lock", "measure", "merge", "mount", "move", "new", "open", "optimize", "out", "ping", "pop", "protect", "publish", "push", "read", "receive",
    "redo", "register", "remove", "rename", "repair", "request", "reset", "resize", "resolve", "restart", "restore", "resume", "revoke", "save",
    "search", "select", "send", "set", "show", "skip", "sort", "split", "start", "step", "stop", "submit", "suspend", "switch", "sync", "tee", "test",
    "trace", "unblock", "undo", "uninstall", "unlock", "unprotect", "unpublish", "unregister", "update", "use", "wait", "watch", "where", "write",
];
/// The verbs that count as a cmdlet even in lowercase: the usual ones, which no Linux program
/// uses as a prefix.
const LOWERCASE_CMDLET_VERBS: &[&str] = &[
    "get", "set", "select", "where", "foreach", "out", "format", "measure", "sort", "test", "new", "remove", "copy", "move", "write", "invoke",
    "convertto", "convertfrom", "import", "export", "rename", "wait", "expand", "compress", "register", "unregister", "enable", "disable",
    "restart", "resolve", "publish", "uninstall", "unblock",
];
/// Cmdlets that only look, move files or shape the output (PowerShell's `ls`, `cat`, `rm`, `curl`…),
/// by their verb: they say nothing about a procedure.
const TRIVIAL_CMDLET_VERBS: &[&str] = &[
    "get", "select", "where", "foreach", "out", "format", "measure", "sort", "test", "write", "read", "tee", "group", "compare", "join", "split",
    "resolve", "convertto", "convertfrom", "convert", "wait", "clear", "show", "hide", "new", "remove", "copy", "move", "rename", "set", "add", "push",
    "pop", "find", "search", "trace", "debug", "enter", "exit", "skip", "step", "limit", "use", "open", "close",
];
/// And the ones that are trivial even though their verb acts.
const TRIVIAL_CMDLETS: &[&str] = &["invoke-webrequest", "invoke-restmethod", "invoke-expression", "invoke-item", "start-process", "start-sleep", "stop-process"];

/// The commands in a shell line: splits on `&&`, `||`, `;`, `|`, newlines, `$(` and backticks
/// outside quotes. Quoted text stays as a single word marked with `\u{1}`
/// and only its last path segment (`"C:/…/gh.exe" pr list` → `\u{1}gh.exe pr list`): as an argument it is
/// discarded, and in the program's place its name counts.
fn split_commands(cmd: &str) -> Vec<String> {
    let mut r = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut quoted = String::new();
    let mut escaped = false;
    let mut cs = cmd.chars().peekable();
    while let Some(c) = cs.next() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
                quoted.push(c);
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
                let name = quoted.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").replace(char::is_whitespace, "_");
                current.push_str(&format!(" \u{1}{name} "));
                quoted.clear();
            } else {
                quoted.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '$' if cs.peek() == Some(&'(') => {
                cs.next();
                r.push(std::mem::take(&mut current));
            }
            '&' | ';' | '|' | '\n' | '`' | '(' | ')' => r.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    r.push(current);
    r.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// A program name: letters, digits, `-`, no `_` (`insert_content` is the action of an editing
/// tool with a `command` field), and `.` (`python3.12`). Lowercase and without `.exe`.
fn program_name(t: &str) -> Option<String> {
    let t = t.strip_prefix('\u{1}').unwrap_or(t);
    let p = t.rsplit(['/', '\\']).next().unwrap_or(t).to_lowercase();
    let p = p.strip_suffix(".exe").unwrap_or(&p).trim_matches(['{', '}', '[', ']']).to_string();
    let valid = p.starts_with(|c: char| c.is_ascii_alphabetic()) && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    valid.then_some(p)
}

/// If `t` (as written) is a PowerShell cmdlet (`Remove-Item`, `Invoke-Pester`,
/// `get-childitem`): its verb, lowercase.
fn cmdlet_verb(t: &str) -> Option<String> {
    let t = t.strip_prefix('\u{1}').unwrap_or(t);
    let t = t.rsplit(['/', '\\']).next().unwrap_or(t);
    let (verb, noun) = t.split_once('-')?;
    if noun.is_empty() || !noun.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let v = verb.to_ascii_lowercase();
    let capitalized = t.starts_with(|c: char| c.is_ascii_uppercase()) || noun.starts_with(|c: char| c.is_ascii_uppercase());
    (POWERSHELL_VERBS.contains(&v.as_str()) && (capitalized || LOWERCASE_CMDLET_VERBS.contains(&v.as_str()))).then_some(v)
}

/// Options of `sudo` (and `doas`, `runuser`) that take a value: `sudo -u postgres psql` is `psql`,
/// not `postgres`.
const SUDO_TAKES_VALUE: &[&str] = &["-u", "-g", "-U", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "--user", "--group", "--other-user", "--chdir", "--host", "--prompt", "--role", "--type", "--close-from", "--command-timeout"];
/// And of `env`: `env -u VAR -C folder cargo test`.
const ENV_TAKES_VALUE: &[&str] = &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"];

/// The steps of a command: the program and its subcommand (`cargo build --release && git tag -a v1` →
/// `cargo build`, `git tag`), without the trivial ones nor those that only look.
pub fn command_steps(cmd: &str) -> Vec<String> {
    // A multi-line script is usually single-use; of a heredoc, only its first
    // line counts (the rest is what gets passed to it).
    let cmd = if cmd.contains("<<") { cmd.lines().next().unwrap_or("") } else { cmd };
    if cmd.lines().count() > 3 {
        return Vec::new();
    }
    let mut r = Vec::new();
    for part in split_commands(cmd) {
        // Drop redirections, assignments and options (with their value if they take one).
        let mut ts: Vec<&str> = Vec::new();
        let mut skip = false;
        for t in part.split_whitespace() {
            if skip {
                skip = false;
                continue;
            }
            if TAKES_VALUE.contains(&t) {
                skip = true;
                continue;
            }
            // The options with a value of the wrapper right before (`sudo -u <user> <command>`).
            let wrapper = ts.last().and_then(|p| program_name(p));
            if matches!(wrapper.as_deref(), Some("sudo" | "doas" | "runuser")) && SUDO_TAKES_VALUE.contains(&t)
                || wrapper.as_deref() == Some("env") && ENV_TAKES_VALUE.contains(&t)
            {
                skip = true;
                continue;
            }
            if t.starts_with('-') || t.contains('=') || t.contains('>') || t.contains('<') || (t.starts_with('/') && t.len() <= 3) {
                continue;
            }
            ts.push(t);
        }
        // What goes in front (`bash -lc …`, `cmd /c …`, `sudo …`) isn't the program.
        let mut wrapped = false;
        while ts.first().and_then(|t| program_name(t)).is_some_and(|p| WRAPPERS.contains(&p.as_str())) {
            ts.remove(0);
            wrapped = true;
        }
        // `powershell -Command "…"`: what's inside the quotes can't be seen, so it isn't a step.
        if wrapped && ts.first().is_some_and(|t| t.starts_with('\u{1}')) {
            continue;
        }
        let Some(prog) = ts.first().and_then(|t| program_name(t)) else { continue };
        if TRIVIAL.contains(&prog.as_str()) {
            continue;
        }
        // A cmdlet is a whole command: what follows it are arguments, not a subcommand
        // (`Restart-Service omoikane` is `restart-service`). Drop the ones that only look or move files.
        if let Some(verb) = ts.first().and_then(|t| cmdlet_verb(t)) {
            if !TRIVIAL_CMDLET_VERBS.contains(&verb.as_str()) && !TRIVIAL_CMDLETS.contains(&prog.as_str()) {
                r.push(prog);
            }
            continue;
        }
        let sub = ts.get(1).filter(|s| !s.starts_with('\u{1}') && s.len() > 1 && s.chars().all(|c| c.is_ascii_lowercase() || c == '-')).copied().unwrap_or("");
        let step = if sub.is_empty() { prog } else { format!("{prog} {sub}") };
        if !READ_ONLY.contains(&step.as_str()) {
            r.push(step);
        }
    }
    r
}

/// The steps of a conversation, in order, without repeating the same one in a row.
pub fn steps(path: &Path) -> Result<Vec<String>> {
    Ok(steps_from_turns(&crate::candidatas::turns(path)?))
}

/// `steps` with the turns already read.
pub fn steps_from_turns(turns: &[crate::candidatas::Turn]) -> Vec<String> {
    let mut r: Vec<String> = Vec::new();
    for t in turns {
        if let crate::candidatas::Turn::Command(c) = t {
            for p in command_steps(c) {
                if r.last() != Some(&p) {
                    r.push(p);
                }
            }
        }
    }
    r.truncate(MAX_SESSION_STEPS);
    r
}

fn create_table(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS session_steps (session TEXT PRIMARY KEY, at TEXT NOT NULL, steps TEXT NOT NULL);")?;
    Ok(())
}

/// On close or compaction: notes the session's steps (if it has any) and deletes those older than `DAYS`.
pub fn record(db: &Connection, session: &str, steps: &[String]) -> Result<()> {
    if session.is_empty() || steps.is_empty() {
        return Ok(());
    }
    create_table(db)?;
    db.execute(&format!("INSERT OR REPLACE INTO session_steps(session, at, steps) VALUES (?1, {NOW}, ?2)"), params![session, serde_json::to_string(steps)?])?;
    db.execute(&format!("DELETE FROM session_steps WHERE julianday({NOW}) - julianday(at) > {DAYS}"), [])?;
    Ok(())
}

/// How many sessions, the most recent ones, are looked at for counting.
const MAX_SESSIONS: usize = 500;

/// The words of a text, to see whether a recipe names a step: without surrounding punctuation, lowercase.
fn words(t: &str) -> std::collections::HashSet<String> {
    t.to_lowercase().split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '.')).map(|w| w.trim_matches('.').to_string()).filter(|w| !w.is_empty()).collect()
}

/// Of the sequences in `current` (the steps of the session being closed), the one most repeated in the
/// saved sessions (in at least `MIN_SESSIONS`, with two or more different programs), that no saved
/// recipe covers and that wasn't proposed before, with how many sessions it shows up in. Only this session's
/// are looked at: that way the cost doesn't grow with the history and the proposal lands in the project where it happens.
pub fn proposal(db: &Connection, current: &[String]) -> Result<Option<(Vec<String>, usize)>> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_steps')", [], |f| f.get(0))?;
    if !exists || current.len() < MIN_STEPS {
        return Ok(None);
    }
    // The candidates: this session's sequences.
    let mut counts: std::collections::HashMap<Vec<String>, BTreeSet<usize>> = std::collections::HashMap::new();
    for n in MIN_STEPS..=MAX_STEPS {
        for w in current.windows(n) {
            let programs: BTreeSet<&str> = w.iter().map(|p| p.split(' ').next().unwrap_or("")).collect();
            if w.iter().collect::<BTreeSet<_>>().len() == n && programs.len() >= 2 {
                counts.entry(w.to_vec()).or_default();
            }
        }
    }
    if counts.is_empty() {
        return Ok(None);
    }
    // Which saved sessions each one shows up in (without copying: looked up by the slice).
    let mut q = db.prepare("SELECT steps FROM session_steps ORDER BY at DESC LIMIT ?1")?;
    let rows = q.query_map([MAX_SESSIONS as i64], |f| f.get::<_, String>(0))?;
    for (i, text) in rows.enumerate() {
        let steps: Vec<String> = serde_json::from_str(&text?).unwrap_or_default();
        for n in MIN_STEPS..=MAX_STEPS {
            for w in steps.windows(n) {
                if let Some(s) = counts.get_mut(w) {
                    s.insert(i);
                }
            }
        }
    }
    let mut v: Vec<(Vec<String>, BTreeSet<usize>)> = counts.into_iter().filter(|(_, s)| s.len() >= MIN_SESSIONS).collect();
    // Drop the ones contained in a longer one that shows up in the same sessions.
    let all = v.clone();
    v.retain(|(w, s)| !all.iter().any(|(o, so)| o.len() > w.len() && so == s && o.windows(w.len()).any(|x| x == w.as_slice())));
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(&b.0)));
    let recipes: Vec<std::collections::HashSet<String>> = {
        let mut q = db.prepare("SELECT title || ' ' || content FROM memory WHERE kind = ?1")?;
        q.query_map([Kind::Procedure.as_str()], |f| f.get::<_, String>(0))?.filter_map(|t| t.ok()).map(|t| words(&t)).collect()
    };
    let already: std::collections::HashSet<String> = {
        let mut q = db.prepare("SELECT k FROM meta WHERE k LIKE 'receta_propuesta:%'")?;
        q.query_map([], |f| f.get::<_, String>(0))?.filter_map(|k| k.ok()).collect()
    };
    for (w, s) in v {
        // Covered if a recipe names every step: its program and its subcommand, even if not together
        // («git add -A, commit y push» covers `git commit`).
        let covered = recipes.iter().any(|ps| w.iter().all(|p| p.split(' ').all(|x| ps.contains(x))));
        if !covered && !already.contains(&format!("receta_propuesta:{}", w.join(" > "))) {
            return Ok(Some((w, s.len())));
        }
    }
    Ok(None)
}

/// On close: if among this session's steps there is a sequence to propose, sets it aside as a
/// candidate for the next session in `project` (and doesn't propose it again). Returns whether it set one aside.
pub fn propose(db: &Connection, project: &str, session: &str, current: &[String]) -> Result<bool> {
    let Some((w, n)) = proposal(db, current)? else { return Ok(false) };
    let text = format!(
        "steps repeated in {n} sessions: `{}`. If it is a procedure the user follows, ask them and save it as a recipe (kind procedure, numbered steps and a `when:` line); if not, ignore it",
        w.join("` → `")
    );
    db.execute(&format!("INSERT OR IGNORE INTO candidate(project, session, kind, text, created_at) VALUES (?1, ?2, ?3, ?4, {NOW})"), params![project, session, RECIPE, text])?;
    let key = format!("receta_propuesta:{}", w.join(" > "));
    db.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, {NOW})"), [&key])?;
    Ok(true)
}

/// The sequences that were proposed (for the view and for checking).
pub fn proposals(db: &Connection) -> Result<Vec<String>> {
    let mut q = db.prepare("SELECT substr(k, 18) FROM meta WHERE k LIKE 'receta_propuesta:%' ORDER BY v DESC")?;
    let v = q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// What was noted for a session (for the tests).
pub fn for_session(db: &Connection, session: &str) -> Result<Option<Vec<String>>> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_steps')", [], |f| f.get(0))?;
    if !exists {
        return Ok(None);
    }
    let t: Option<String> = db.query_row("SELECT steps FROM session_steps WHERE session = ?1", [session], |f| f.get(0)).optional()?;
    Ok(t.and_then(|t| serde_json::from_str(&t).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_of_a_command() {
        assert_eq!(command_steps("cargo build --release 2>&1 && git tag -a v1.4.1 -m \"1.4.1\" && git push origin v1.4.1"), ["cargo build", "git tag", "git push"]);
        assert_eq!(command_steps("git status --short | head; cargo test -q 2>&1 | grep -E \"FAILED|panicked\""), ["cargo test"]);
        assert_eq!(command_steps("cd D:/x && ls"), Vec::<String>::new());
        assert_eq!(command_steps("C:/Users/m/.cargo/bin/omoi.exe recall --session-end"), ["omoi recall"]);
        assert_eq!(command_steps("id=$(gh run list -L 5) && gh release view v1"), ["gh release"]);
        assert_eq!(command_steps("insert_content"), Vec::<String>::new());
        // What the 1.5.0 review found.
        assert_eq!(command_steps("cargo test 2>&1 | Select-Object -Last 20"), ["cargo test"]);
        assert_eq!(command_steps("bash -lc cargo test && git push"), ["cargo test", "git push"]);
        assert_eq!(command_steps("cmd /c npm run build"), ["npm run"]);
        assert!(command_steps(r#"powershell -Command "Remove-Item -Recurse D:\tmp\build""#).is_empty());
        assert!(command_steps(r#"bash -c "cargo test --manifest-path a/Cargo.toml""#).is_empty());
        assert_eq!(command_steps("git -C D:/Omoikane diff && gh -R o/r pr list"), ["gh pr"]);
        assert_eq!(command_steps("python - <<'EOF'\nimport json\nEOF"), Vec::<String>::new());
        assert_eq!(command_steps("\"/c/Program Files/GitHub CLI/gh.exe\" pr list"), ["gh pr"]);
        assert_eq!(command_steps("git commit -m \"say \\\" ; docker push x\""), ["git commit"]);
        assert_eq!(command_steps("x=`git rev-parse HEAD` && cargo build"), ["git rev-parse", "cargo build"]);
        assert_eq!(command_steps("GIT.EXE tag v1"), ["git tag"]);
    }

    /// 1.8.6: PowerShell cmdlets are commands (with their whole name, no subcommand), and the ones that
    /// only look or move files are dropped like `ls` or `rm`.
    #[test]
    fn powershell_cmdlets() {
        assert_eq!(command_steps("Invoke-Pester -Path tests; git push"), ["invoke-pester", "git push"]);
        assert_eq!(command_steps("Restart-Service omoikane"), ["restart-service"]);
        assert_eq!(command_steps("Install-Module PSReadLine -Scope CurrentUser"), ["install-module"]);
        assert_eq!(command_steps("Publish-Module -Name Omoi -NuGetApiKey $k"), ["publish-module"]);
        assert_eq!(command_steps("cargo build --release; Compress-Archive -Path target/release/omoi.exe -DestinationPath omoi.zip"), ["cargo build", "compress-archive"]);
        assert_eq!(command_steps("Register-ScheduledTask -TaskName vigilante -Xml (Get-Content x.xml | Out-String)"), ["register-scheduledtask"]);
        assert_eq!(command_steps("Update-Help"), ["update-help"]);
        assert_eq!(command_steps(r"unblock-file .\omoi.exe"), ["unblock-file"]);
        // What only looks, moves files or shapes the output, out.
        assert!(command_steps("Get-ChildItem -Recurse src | Select-Object -First 5").is_empty());
        assert!(command_steps(r"Remove-Item -Recurse -Force D:\tmp\x; Set-Location D:\x; New-Item -ItemType Directory y").is_empty());
        assert!(command_steps("Invoke-WebRequest -Uri https://x -OutFile y; Start-Sleep 2; Start-Process notepad").is_empty());
        assert!(command_steps("get-content a.txt | where-object { $_ -match 'x' }").is_empty());
        assert_eq!(command_steps("cargo test 2>&1 | Select-Object -Last 20"), ["cargo test"]);
        // Linux ones shaped like a cmdlet, in lowercase, are still programs.
        assert_eq!(command_steps("update-grub"), ["update-grub"]);
        assert_eq!(command_steps("docker-compose up -d"), ["docker-compose up"]);
        assert_eq!(command_steps("pre-commit run --all-files"), ["pre-commit run"]);
    }

    /// 1.8.6: `sudo -u <user> <command>` is `<command>`, not the user.
    #[test]
    fn sudo_with_user() {
        assert_eq!(command_steps("sudo -u postgres psql -c 'select 1'"), ["psql"]);
        assert_eq!(command_steps("sudo -u www-data git -C /srv/app pull"), ["git pull"]);
        assert_eq!(command_steps("sudo -E -u marc docker compose up -d"), ["docker compose"]);
        assert_eq!(command_steps("sudo --user=marc systemctl restart caddy"), ["systemctl restart"]);
        assert_eq!(command_steps("sudo -g docker -u marc cargo build"), ["cargo build"]);
        assert_eq!(command_steps("sudo systemctl restart caddy"), ["systemctl restart"]);
        assert_eq!(command_steps("doas -u marc make install"), ["make install"]);
        assert_eq!(command_steps("env -u RUST_LOG cargo test"), ["cargo test"]);
        // Another command's `-u` swallows nothing: `git push -u origin main` is still `git push`.
        assert_eq!(command_steps("git push -u origin main"), ["git push"]);
    }
}
