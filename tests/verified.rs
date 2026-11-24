//! 1.8.5: verified by a person versus written by an agent, and the provenance (session and commit)
//! of each memory. With the real `omoi` binary, its own database and a test repository.

mod common;

use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

/// The variables of an agent's shell (Claude Code, Codex…) and of its session: removed, unless the
/// test sets them, so nothing changes depending on where the tests are launched from.
fn agent_vars() -> impl Iterator<Item = &'static str> {
    omoikane::verified::AGENT_ENV_VARS.iter().copied().chain(["OMOIKANE_SESSION"])
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct Env {
    dir: common::TempDir,
}

impl Env {
    /// With a repository `repo` whose HEAD is the `main` branch at [`SHA`].
    fn new(name: &str) -> Env {
        let dir = common::TempDir::new(&format!("verif-{name}"));
        let git = dir.join("repo").join(".git");
        std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        std::fs::create_dir_all(dir.join("repo").join("src")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs").join("heads").join("main"), format!("{SHA}\n")).unwrap();
        Env { dir }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.current_dir(self.dir.join("repo").join("src"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("USERPROFILE", self.dir.join("casa"))
            .env("HOME", self.dir.join("casa"))
            .env_remove("OMOIKANE_LANG");
        common::without_session_vars(&mut c);
        for v in agent_vars() {
            c.env_remove(v);
        }
        c
    }

    fn omoi_with(&self, args: &[&str], input: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = self.command();
        c.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        if let Some(e) = input {
            child.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let o = self.omoi_with(args, None, env);
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        o
    }

    fn ok(&self, args: &[&str]) -> String {
        String::from_utf8(self.ok_with(args, &[]).stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    fn save(&self, kind: &str, title: &str, text: &str) -> String {
        self.json(&["remember", "-k", kind, "-t", title, "--force", text])["id"].as_str().unwrap().to_string()
    }

    /// An MCP tool, as an agent calls it (`omoi tool`).
    fn tool(&self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let o = self.omoi_with(&["tool", name], Some(&args.to_string()), &[("OMOIKANE_SOURCE", "mcp:prueba")]);
        assert!(o.status.success(), "tool {name}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }
}

#[test]
fn a_person_verifies_and_it_shows_in_get_search_list_and_recall() {
    let e = Env::new("ver");
    let id = e.save("decision", "Pagos en céntimos enteros", "Decidido: el importe se guarda en céntimos, nunca en f32.");
    e.ok(&["update", &id, "--tag", "pagos", "--pin"]);
    assert!(e.json(&["get", &id]).get("verified").is_none(), "nadie la ha verificado");

    // Without a terminal and without --yes, no; inside an agent's shell, not even with --yes.
    let o = e.omoi_with(&["verify", &id, "--by", "Marc"], None, &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--yes"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_with(&["verify", &id, "--by", "Marc", "--yes"], None, &[("CLAUDECODE", "1")]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("CLAUDECODE"), "{}", String::from_utf8_lossy(&o.stderr));
    // The ones other agents really set (and Claude Code without `CLAUDECODE`), too.
    for (k, v) in [("CLAUDE_CODE_ENTRYPOINT", "cli"), ("CLAUDE_CODE_SESSION_ID", "s"), ("OPENCODE", "1"), ("CLINE_ACTIVE", "true"), ("COPILOT_AGENT", "1"), ("WINDSURF_CASCADE_TERMINAL", "1"), ("CODEX_THREAD_ID", "t")] {
        let o = e.omoi_with(&["verify", &id, "--by", "Marc", "--yes"], None, &[(k, v)]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(k), "{k}: {}", String::from_utf8_lossy(&o.stderr));
    }
    assert!(e.json(&["get", &id]).get("verified").is_none());
    // And the configuration the person has in their terminal doesn't get mistaken for an agent.
    let o = e.omoi_with(&["verify", &id, "--by", "Marc", "--yes", "--json"], None, &[("CLAUDE_CODE_USE_BEDROCK", "1"), ("AIDER_MODEL", "x"), ("CURSOR_TRACE_ID", "x")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    e.ok(&["verify", &id, "--unverify", "--yes"]);

    let output = e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    assert!(output.contains("verified") && output.contains("Marc"), "{output}");
    let m = e.json(&["get", &id]);
    assert_eq!(m["verified"]["by"], "Marc", "{m}");
    assert!(m["verified"]["at"].as_str().unwrap().starts_with("20"));
    // No new version: verifying doesn't change the text.
    assert_eq!(e.json(&["history", &id]).as_array().unwrap().len(), 1, "la del --tag/--pin, y nada más");
    assert!(e.ok(&["get", &id]).contains("verified   by Marc"));
    // Search, list and recent.
    let r = e.json(&["search", "importe en céntimos"]);
    assert_eq!(r[0]["verified"]["by"], "Marc", "{r}");
    assert!(e.ok(&["search", "importe en céntimos"]).contains("(verified by Marc)"));
    assert_eq!(e.json(&["list", "--tag", "pagos"])[0]["verified"]["by"], "Marc");
    assert_eq!(e.json(&["recent"])[0]["verified"]["by"], "Marc");
    // Recall: a short mark after the kind.
    let o = e.omoi_with(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"s1","source":"startup"}"#), &[]);
    let t = String::from_utf8(o.stdout).unwrap();
    assert!(t.contains("[decision, verified]") && t.contains("Pagos en céntimos"), "{t}");
    // And what the agent reads via MCP: `context`, `search` and `get`.
    let c = e.tool("context", serde_json::json!({}));
    let in_context = c["memories"].as_array().unwrap().iter().find(|x| x["id"] == id.as_str()).unwrap_or_else(|| panic!("{c}"));
    assert!(in_context["verified"].as_str().unwrap().starts_with("by Marc, 20"), "{in_context}");
    let s = e.tool("search", serde_json::json!({ "query": "importe en céntimos" }));
    assert!(s.to_string().contains("by Marc"), "{s}");
    assert_eq!(e.tool("get", serde_json::json!({ "id": id }))["verified"]["by"], "Marc");

    // Removing it.
    assert!(e.ok(&["verify", &id, "--unverify"]).contains("no longer verified"));
    assert!(e.json(&["get", &id]).get("verified").is_none());
    assert!(e.ok(&["verify", &id, "--unverify"]).contains("was not verified"));
}

/// Changing the text unverifies it (and says so); tags, paths or pinning it don't. A change made by
/// a version that doesn't understand it (the seal stays in `extra`) doesn't leave it verified either.
#[test]
fn changing_the_text_unverifies_it() {
    let e = Env::new("cambio");
    let id = e.save("gotcha", "El emulador no se mata con taskkill", "Usar adb emu kill.");
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    e.ok(&["update", &id, "--tag", "android", "--path", "src/x.rs", "--pin"]);
    assert_eq!(e.json(&["get", &id])["verified"]["by"], "Marc", "etiquetas, rutas y fijarla no cambian lo que dice");

    let o = e.ok_with(&["update", &id, "--append", "Y esperar a que se cierre.", "-r", "más"], &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no longer verified"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.json(&["get", &id]).get("verified").is_none());
    let extra: String = e.database().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |row| row.get(0)).unwrap();
    let extra: serde_json::Value = serde_json::from_str(&extra).unwrap();
    assert!(extra.get("verified").is_none(), "el sello se va: {extra}");
    assert!(extra.get("unverified_at").is_some(), "y se apunta cuándo: {extra}");

    // Via MCP: the agent changes the text and the response tells it.
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    let r = e.tool("update", serde_json::json!({ "id": id, "content": "Usar adb emu kill y esperar.", "reason": "más corto" }));
    assert!(r["unverified"].as_str().unwrap().contains("verified by Marc"), "{r}");
    assert!(e.json(&["get", &id]).get("verified").is_none());

    // An older version that keeps the seal without understanding it and changes the content: the fingerprint no
    // longer matches.
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    e.database().execute("UPDATE memory SET content = 'Cambiado por una 1.0.' WHERE id = ?1", [&id]).unwrap();
    let extra: String = e.database().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |row| row.get(0)).unwrap();
    assert!(extra.contains("\"verified\""), "{extra}");
    assert!(e.json(&["get", &id]).get("verified").is_none());
    assert!(e.ok(&["get", &id]).contains("verified   no"));
}

/// Changing the text records when the seal was removed (`extra.unverified_at`, like `verify --unverify`):
/// a newer line that brings the old text with its old seal (another machine of the person that hadn't
/// seen the change yet, a copy from `import <file>`) doesn't leave it verified again. The seal
/// carries the text's fingerprint, but with the old text the fingerprint matches again.
#[test]
fn an_old_seal_does_not_come_back_after_changing_the_text() {
    let e = Env::new("sello-viejo");
    let id = e.save("decision", "Sello viejo", "Texto que la persona vio.");
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    let output = e.ok(&["export"]);
    std::thread::sleep(std::time::Duration::from_millis(20));
    e.ok(&["update", &id, "--append", "Y lo que añadió un agente.", "-r", "más"]);
    let (extra, hlc): (String, String) = e.database().query_row("SELECT extra, hlc FROM memory WHERE id = ?1", [&id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let extra: serde_json::Value = serde_json::from_str(&extra).unwrap();
    assert!(extra.get("verified").is_none() && extra["unverified_at"].as_str().is_some_and(|d| d.starts_with("20")), "{extra}");

    // The line exported before the change, with a later timestamp (from another machine).
    let mut line: serde_json::Value = output.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|l| l["id"] == id.as_str()).unwrap();
    assert_eq!(line["verified"]["by"], "Marc", "{line}");
    let ms: u64 = hlc[..13].parse().unwrap();
    let newer = format!("{:013}-0000000000-otro-equipo", ms + 10_000);
    line["hlc"] = serde_json::json!(newer);
    if line.get("content_hlc").is_some() {
        line["content_hlc"] = serde_json::json!(newer);
    }
    let text = line.to_string();
    let mut footer: serde_json::Value = serde_json::from_str(output.lines().last().unwrap()).unwrap();
    footer["count"] = serde_json::json!(1);
    use sha2::Digest;
    footer["sha256"] = serde_json::json!(sha2::Sha256::digest(format!("{text}\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>());
    let f = e.dir.join("vieja.jsonl");
    std::fs::write(&f, format!("{}\n{text}\n{footer}\n", output.lines().next().unwrap())).unwrap();
    e.ok(&["import", f.to_str().unwrap()]);
    let m = e.json(&["get", &id]);
    assert_eq!(m["content"], "Texto que la persona vio.", "la línea más nueva gana: {m}");
    assert!(m.get("verified").is_none(), "el sello es de antes de quitárselo: {m}");
    // And the date is kept, for the next time it arrives.
    let extra: String = e.database().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |row| row.get(0)).unwrap();
    assert!(extra.contains("unverified_at"), "{extra}");
}

/// No agent path ends in «verified»: there is no tool, `remember` and `update` ignore
/// a `verified` the agent sets, and whatever `data` carries goes separately.
#[test]
fn an_agent_cannot_verify_itself() {
    let e = Env::new("agente");
    let list = e.tool_list();
    let names: Vec<&str> = list["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!names.iter().any(|n| n.contains("verif")), "{names:?}");
    assert!(list["instructions"].as_str().unwrap().contains("never do it yourself"));
    let seal = serde_json::json!({ "by": "Marc", "at": "2026-11-08T00:00:00.000Z", "sha": "x" });
    let r = e.tool(
        "remember",
        serde_json::json!({ "kind": "decision", "title": "Me verifico yo", "content": "Un agente que se pone el sello.", "verified": seal, "data": { "verified": seal } }),
    );
    let id = r["id"].as_str().unwrap().to_string();
    assert!(e.json(&["get", &id]).get("verified").is_none());
    e.tool("update", serde_json::json!({ "id": id, "tags": ["x"], "verified": seal }));
    assert!(e.json(&["get", &id]).get("verified").is_none());
}

impl Env {
    fn tool_list(&self) -> serde_json::Value {
        let o = self.omoi_with(&["tool"], None, &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
}

/// Provenance: the session (from the environment or the hooks) and the repository's commit, without `git`.
#[test]
fn saves_the_session_and_the_commit() {
    let e = Env::new("proc");
    let o = e.ok_with(&["remember", "-k", "fact", "-t", "Con sesión", "--json", "Algo."], &[("OMOIKANE_SESSION", "sesion-1")]);
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let id = m["id"].as_str().unwrap();
    assert_eq!(m["provenance"], serde_json::json!({ "session": "sesion-1", "commit": SHA, "branch": "main" }), "{m}");
    assert_eq!(e.json(&["get", id])["provenance"]["commit"], SHA);
    assert!(e.ok(&["get", id]).contains("written in session sesion-1 · commit 0123456789ab (main)"));
    assert!(e.ok(&["history", id]).contains("written in session sesion-1"));
    // Claude Code puts it in the environment of what it launches.
    let o = e.ok_with(&["remember", "-k", "fact", "-t", "De Claude", "--json", "Otra."], &[("CLAUDE_CODE_SESSION_ID", "cc-2")]);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["provenance"]["session"], "cc-2");
    // Without the variable: the only session the hooks recorded in this project.
    let o = e.omoi_with(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"hook-3","source":"startup"}"#), &[]);
    assert!(o.status.success());
    assert_eq!(e.json(&["remember", "-k", "fact", "-t", "Del hook", "Una más."])["provenance"]["session"], "hook-3");
    // With two at once in the project it doesn't guess: only the commit.
    e.omoi_with(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"hook-4","source":"startup"}"#), &[]);
    let m = e.json(&["remember", "-k", "fact", "-t", "Dos sesiones", "Y otra."]);
    assert_eq!(m["provenance"], serde_json::json!({ "commit": SHA, "branch": "main" }), "{m}");
    // Via MCP too.
    let r = e.tool("remember", serde_json::json!({ "kind": "fact", "title": "Por MCP", "content": "Desde el agente.", "force": true }));
    assert_eq!(e.json(&["get", r["id"].as_str().unwrap()])["provenance"]["commit"], SHA);
    // Changing it doesn't touch it: it is the one from when it was written.
    e.ok_with(&["update", id, "-c", "Cambiada.", "-r", "x"], &[("OMOIKANE_SESSION", "otra")]);
    assert_eq!(e.json(&["get", id])["provenance"]["session"], "sesion-1");
    // A trade one carries nothing from the project.
    let m = e.ok_with(&["remember", "-k", "gotcha", "-t", "No matar el emulador", "--trade", "android", "--json", "Usar adb emu kill."], &[("OMOIKANE_SESSION", "s")]);
    assert!(serde_json::from_slice::<serde_json::Value>(&m.stdout).unwrap().get("provenance").is_none());
    // Without repository or session: nothing (not even `extra`).
    let loose = e.dir.join("suelta");
    std::fs::create_dir_all(&loose).unwrap();
    let m = e.json(&["--dir", loose.to_str().unwrap(), "remember", "-k", "fact", "-t", "Sin nada", "Nada."]);
    assert!(m.get("provenance").is_none(), "{m}");
}

/// Exporting and importing (the same as sync: fields of the line) keeps the seal and the
/// provenance, and the seal is still valid on the other side.
#[test]
fn they_travel_with_the_memory() {
    let e = Env::new("viaje");
    let id = e.json(&["remember", "-k", "decision", "-t", "Viaja", "Con sello."])["id"].as_str().unwrap().to_string();
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    let output = e.ok(&["export"]);
    let line: serde_json::Value = output.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|l| l["id"] == id.as_str()).unwrap();
    assert_eq!(line["verified"]["by"], "Marc", "{line}");
    assert_eq!(line["provenance"]["commit"], SHA, "{line}");
    // In another database (another person, or a machine without the person's general repository: it doesn't have their
    // seal key), the seal isn't trusted without --trust-verified, as in `import --okf`: it says so and comes in without it.
    let other = Env::new("viaje2");
    let f = other.dir.join("copia.jsonl");
    std::fs::write(&f, &output).unwrap();
    let o = other.ok_with(&["import", f.to_str().unwrap()], &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("--trust-verified"), "{}", String::from_utf8_lossy(&o.stderr));
    let m = other.json(&["get", &id]);
    assert!(m.get("verified").is_none(), "{m}");
    assert_eq!(m["provenance"]["commit"], SHA, "{m}");
    // With --trust-verified, yes (and from then on it is a seal of this person).
    let other = Env::new("viaje2b");
    let f = other.dir.join("copia.jsonl");
    std::fs::write(&f, &output).unwrap();
    other.ok(&["import", "--trust-verified", f.to_str().unwrap()]);
    let m = other.json(&["get", &id]);
    assert_eq!((&m["verified"]["by"], &m["provenance"]["commit"]), (&serde_json::json!("Marc"), &serde_json::json!(SHA)), "{m}");
    // In the same database, its own seal copied to another memory (another id, with its fingerprint recomputed) is not
    // valid: the signature covers the id and the fingerprint. It is removed and counted.
    let new_id = "01KZZZZZZZZZZZZZZZZZZZZZZZ";
    let mut l = line.clone();
    l["id"] = serde_json::json!(new_id);
    let f = e.dir.join("falsa.jsonl");
    let header = output.lines().next().unwrap();
    let text = l.to_string();
    let mut footer: serde_json::Value = serde_json::from_str(output.lines().last().unwrap()).unwrap();
    footer["count"] = serde_json::json!(1);
    use sha2::Digest;
    footer["sha256"] = serde_json::json!(sha2::Sha256::digest(format!("{text}\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>());
    std::fs::write(&f, format!("{header}\n{text}\n{footer}\n")).unwrap();
    let r: serde_json::Value = serde_json::from_slice(&e.ok_with(&["--json", "import", f.to_str().unwrap()], &[]).stdout).unwrap();
    assert_eq!(r["file"]["untrusted_verified"], 1, "{r}");
    assert!(e.json(&["get", new_id]).get("verified").is_none());

    // From an agent's shell, the copy comes in without the seal (the fingerprint is computed from the text: otherwise
    // anyone writing the file would verify themselves) and says so.
    let third = Env::new("viaje3");
    let f = third.dir.join("copia.jsonl");
    std::fs::write(&f, &output).unwrap();
    let o = third.ok_with(&["import", f.to_str().unwrap()], &[("CLAUDECODE", "1")]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("they come in unverified"), "{}", String::from_utf8_lossy(&o.stderr));
    let m = third.json(&["get", &id]);
    assert!(m.get("verified").is_none(), "{m}");
    assert_eq!(m["provenance"]["commit"], SHA, "lo demás llega: {m}");
}

/// The provenance of a global memory (or a space one, or a trade one) doesn't carry the commit or the branch
/// of the folder it was written in: it may be a client's and those memories are seen from others.
#[test]
fn a_global_does_not_carry_the_clients_branch() {
    let e = Env::new("rama");
    let git = e.dir.join("repo").join(".git");
    std::fs::create_dir_all(git.join("refs").join("heads").join("feature")).unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/acme-secret-merger\n").unwrap();
    std::fs::write(git.join("refs").join("heads").join("feature").join("acme-secret-merger"), format!("{SHA}\n")).unwrap();
    let session = [("OMOIKANE_SESSION", "s-1")];
    let global: serde_json::Value = serde_json::from_slice(&e.ok_with(&["remember", "-k", "fact", "-t", "Global", "--global", "--json", "De todos."], &session).stdout).unwrap();
    assert_eq!(global["provenance"], serde_json::json!({ "session": "s-1" }), "{global}");
    let gid = global["id"].as_str().unwrap();
    assert_eq!(e.json(&["get", gid])["provenance"], serde_json::json!({ "session": "s-1" }));
    // The project's, complete.
    let own: serde_json::Value = serde_json::from_slice(&e.ok_with(&["remember", "-k", "fact", "-t", "Del proyecto", "--json", "Mía."], &session).stdout).unwrap();
    assert_eq!(own["provenance"]["branch"], "feature/acme-secret-merger");
    // Not via MCP, nor in the copy, nor on the web.
    let r = e.tool("remember", serde_json::json!({ "kind": "fact", "title": "Global por MCP", "content": "Para todos.", "global": true, "force": true }));
    assert!(e.json(&["get", r["id"].as_str().unwrap()]).get("provenance").is_none_or(|p| p.get("branch").is_none()));
    let output = e.ok(&["export"]);
    for l in output.lines().filter(|l| l.contains("Global")) {
        assert!(!l.contains("acme-secret-merger"), "{l}");
    }
    // Promoted to a trade, none; moved to global, without commit or branch.
    let sid = own["id"].as_str().unwrap();
    e.ok(&["update", sid, "--trade", "android", "-r", "x"]);
    assert!(e.json(&["get", sid]).get("provenance").is_none(), "{}", e.ok(&["get", sid]));
    let other = e.json(&["remember", "-k", "fact", "-t", "Se mueve", "--force", "Luego global."]);
    let oid = other["id"].as_str().unwrap();
    e.ok(&["update", oid, "--global", "-r", "x"]);
    assert!(!e.ok(&["export"]).lines().any(|l| l.contains(oid) && l.contains("acme-secret-merger")));
}

/// The review puts first, in each kind of warning, the unverified decisions and gotchas, and what is
/// verified at the end.
#[test]
fn the_review_prioritizes_the_unverified() {
    let e = Env::new("revision");
    let broken = ["--path", "src/no-existe.rs"];
    let g = e.json(&[&["remember", "-k", "gotcha", "-t", "Trampa verificada", "--force"][..], &broken, &["Una."]].concat())["id"].as_str().unwrap().to_string();
    let f = e.json(&[&["remember", "-k", "fact", "-t", "Hecho sin verificar", "--force"][..], &broken, &["Dos."]].concat())["id"].as_str().unwrap().to_string();
    let d = e.json(&[&["remember", "-k", "decision", "-t", "Decisión sin verificar", "--force"][..], &broken, &["Tres."]].concat())["id"].as_str().unwrap().to_string();
    e.ok(&["verify", &g, "--by", "Marc", "--yes"]);
    let r = e.json(&["review"]);
    let order: Vec<&str> =
        r.as_array().unwrap_or_else(|| panic!("{r}")).iter().filter(|a| a["kind"] == "broken_path").map(|a| a["memory_id"].as_str().unwrap()).collect();
    assert_eq!(order, [d.as_str(), f.as_str(), g.as_str()], "{r}");
}

/// The web view: the button goes through `POST /api/verify`, which only accepts what the view's page
/// sends (its `Origin` and the token inside the page), not a `curl` with a made-up `Origin`.
#[test]
fn the_web_verifies_only_from_the_browser() {
    let e = Env::new("web");
    let id = e.save("decision", "Desde la web", "Lo verifica la persona en la vista.");
    let port = common::free_port();
    let log = e.dir.join("serve.log");
    let child = e.command().args(["serve", "--port", &port.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(&log).unwrap()).spawn().unwrap();
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = KillOnDrop(child);
    let request = |method: &str, path: &str, body: &str, headers: &[(&str, &str)]| -> (u16, String) {
        let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                s.write_all(req.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                let r = String::from_utf8_lossy(&v).into_owned();
                let status = r.get(9..12).and_then(|x| x.parse().ok()).unwrap_or(0);
                return (status, r.find("\r\n\r\n").map(|i| r[i + 4..].to_string()).unwrap_or_default());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó: {}", std::fs::read_to_string(&log).unwrap_or_default());
    };
    let origin = format!("http://127.0.0.1:{port}");
    // The view's token, from the page (what the button does).
    let (_, page) = request("GET", "/", "", &[]);
    let marker = "name=\"omoikane-view\" content=\"";
    let i = page.find(marker).expect("la página lleva el token") + marker.len();
    let token = page[i..].split('"').next().unwrap().to_string();
    assert!(token.len() >= 32 && !token.contains("OMOIKANE"), "{token}");
    let verify = |body: &str, headers: &[(&str, &str)]| request("POST", "/api/verify", body, headers);
    let body = serde_json::json!({ "id": id, "by": "Marc" }).to_string();
    // Without Origin (curl, an agent): no.
    assert_eq!(verify(&body, &[]).0, 403);
    // From another website: no.
    assert_eq!(verify(&body, &[("Origin", "http://malo.example"), ("X-Omoikane-View", &token)]).0, 403);
    // A `curl -H "Origin: …"` with the view's Origin but without its token (or with another): not either.
    assert_eq!(verify(&body, &[("Origin", &origin)]).0, 403);
    assert_eq!(verify(&body, &[("Origin", &origin), ("X-Omoikane-View", "0123456789abcdef0123456789abcdef")]).0, 403);
    assert!(e.json(&["get", &id]).get("verified").is_none());
    let (status, r) = verify(&body, &[("Origin", &origin), ("X-Omoikane-View", &token)]);
    assert_eq!(status, 200, "{r}");
    assert_eq!(e.json(&["get", &id])["verified"]["by"], "Marc");
    let (status, _) = verify(&serde_json::json!({ "id": id, "unverify": true }).to_string(), &[("Origin", &origin), ("X-Omoikane-View", &token)]);
    assert_eq!(status, 200);
    assert!(e.json(&["get", &id]).get("verified").is_none());

    // «Make skill» (1.13): writes in the repository, so it requires the same as «Verify».
    let skill = |headers: &[(&str, &str)]| request("POST", "/api/skill", &serde_json::json!({ "id": id }).to_string(), headers);
    assert_eq!(skill(&[("Origin", &origin)]).0, 403);
    let (status, r) = skill(&[("Origin", &origin), ("X-Omoikane-View", &token)]);
    assert_eq!(status, 200, "{r}");
    let made: serde_json::Value = serde_json::from_str(&r).unwrap();
    assert_eq!(made.as_array().map(Vec::len), Some(2), "{made}");
    for h in made.as_array().unwrap() {
        assert_eq!(h["action"], "created", "{h}");
        let f = e.dir.join("repo").join(h["path"].as_str().unwrap());
        assert!(std::fs::read_to_string(&f).unwrap().contains("Lo verifica la persona en la vista."), "{}", f.display());
    }

    // Contradictions (1.14): «This one holds» leaves the other superseded; a `keep` that isn't of the pair, no.
    let a = e.save("fact", "El servidor de pruebas está en Frankfurt", "El servidor de pruebas está en Frankfurt.");
    let b = e.save("fact", "El servidor de pruebas está en Madrid", "El servidor de pruebas está en Madrid desde octubre.");
    e.tool("update", serde_json::json!({ "id": b, "contradicts": [a] }));
    let decide = |keep: &str| request("POST", "/api/contradiction", &serde_json::json!({ "a": b, "b": a, "keep": keep, "reason": "se mudó en octubre" }).to_string(), &[("Origin", &origin)]);
    assert_eq!(decide(&id).0, 400);
    let (status, r) = decide(&b);
    assert_eq!(status, 200, "{r}");
    let v: serde_json::Value = serde_json::from_str(&r).unwrap();
    assert_eq!(v["superseded"], a.as_str(), "{v}");
    let (_, rev) = request("GET", "/api/review", "", &[]);
    assert!(!rev.contains("\"contradicts\"") && rev.contains("se mudó en octubre"), "{rev}");
}

/// The link to a memory (1.8.6): `omoi serve --memory <id>` accepts its prefix, prints the link
/// (`#/memoria/<full id>`) and the page knows how to open it; an id that doesn't exist is an error before starting
/// the server. Kumi uses it so the person verifies from its notice.
#[test]
fn the_link_to_a_memory() {
    let e = Env::new("enlace");
    let id = e.save("decision", "Para verificar desde un aviso", "Kumi manda el enlace.");
    // An id that doesn't exist: error right away, without a server.
    let o = e.omoi_with(&["serve", "--port", &common::free_port().to_string(), "--memory", "ZZZZZZZZ"], None, &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("ZZZZZZZZ"), "{}", String::from_utf8_lossy(&o.stderr));
    let port = common::free_port();
    let log = e.dir.join("serve-enlace.log");
    let prefix = id[..10].to_lowercase();
    let child = e
        .command()
        .args(["serve", "--port", &port.to_string(), "--memory", &prefix])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _server = KillOnDrop(child);
    let link = format!("http://127.0.0.1:{port}/#/memoria/{id}");
    let mut page = String::new();
    for _ in 0..100 {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            s.write_all(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
            s.read_to_string(&mut page).unwrap();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let output = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(output.contains(&link), "{output}");
    assert!(page.contains("startsWith('memoria/')") && page.contains("function renderMemory"), "la página abre #/memoria/<id>");
}
