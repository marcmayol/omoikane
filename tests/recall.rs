//! Phase 4: the recall the hooks inject.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use rusqlite::Connection;
use std::io::Write;
use std::process::{Command, Stdio};

fn base(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("recall-{name}"));
    let db = db::open(&dir.join("o.db")).unwrap();
    (dir, db)
}

/// The memories of tests/busquedas.tsv in project `/p`, plus a preference and a pinned one.
fn with_memories(db: &mut Connection) {
    for l in include_str!("busquedas.tsv").lines().filter(|l| l.starts_with("m\t")) {
        let c: Vec<&str> = l.split('\t').collect();
        let mut n = NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: c[2].into(), content: c[3].into(), ..Default::default() };
        if c[1] == "auth" {
            n.kind = Some(Kind::Gotcha);
            n.paths = vec!["src/middleware/auth.rs".into()];
        }
        memoria::save(db, n, None).unwrap();
    }
    let pref = NewMemory { project: String::new(), kind: Some(Kind::Preference), title: "Respuestas en español".into(), content: "El usuario quiere las respuestas en español y sin rodeos.".into(), ..Default::default() };
    memoria::save(db, pref, None).unwrap();
    let pinned_mem = NewMemory { project: "/p".into(), kind: Some(Kind::Decision), title: "Base de datos única".into(), content: "Todo va en un solo fichero SQLite.".into(), pinned: true, ..Default::default() };
    memoria::save(db, pinned_mem, None).unwrap();
}

fn entry(session: &str, prompt: &str) -> HookInput {
    HookInput { session_id: session.into(), prompt: prompt.into(), ..Default::default() }
}

#[test]
fn session_start_gives_preferences_and_pinned() {
    let (_dir, mut db) = base("empezar");
    with_memories(&mut db);
    let t = recall::on_session_start(&mut db, "/p", &entry("s1", "")).unwrap().unwrap();
    assert!(t.starts_with("<omoikane>") && t.ends_with("</omoikane>"));
    assert!(t.contains("20 memories"), "{t}");
    assert!(t.contains("Respuestas en español") && t.contains("[global]"));
    assert!(t.contains("Base de datos única"));
    // The last things worked on: 5 dated titles, without their content, and not repeating the pinned ones.
    let (_, recent) = t.split_once("Last worked on").expect("lista de lo último");
    let lines: Vec<&str> = recent.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(lines.len(), recall::MAX_RECENT, "{t}");
    assert!(lines.iter().all(|l| l.starts_with("- 20") && l.contains("(id ") && !l.contains("Base de datos única")), "{t}");
    assert!(!t.contains("/var/log/app:"), "sin el contenido: {t}");
    assert!(t.contains("ask the user right away"), "{t}");
    // Only preferences and pinned ones count as injected: recent ones can still arrive in full.
    assert_eq!(recall::injected_count(&db, "s1").unwrap(), 2);
}

#[test]
fn session_start_says_where_each_project_was_left() {
    let (_dir, mut db) = base("estado");
    with_memories(&mut db);
    let status = |project: &str, title: &str, content: &str| NewMemory {
        project: project.into(),
        kind: Some(Kind::Fact),
        title: title.into(),
        content: content.into(),
        tags: vec!["Status".into()],
        ..Default::default()
    };
    memoria::save(&mut db, status("/p", "Estado viejo de p", "Fase 1 a medias."), None).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let long = format!("Fase 2 hecha; siguiente: la web. {}", "detalle ".repeat(100));
    let newest = memoria::save(&mut db, status("/p", "Estado de p", &long), None).unwrap();
    memoria::save(&mut db, status("/otro", "Estado de otro", "No debería salir desde /p."), None).unwrap();
    let t = recall::on_session_start(&mut db, "/p", &entry("s1", "")).unwrap().unwrap();
    let (_, statuses) = t.split_once("Where work was left").expect("sección de estado");
    let lines: Vec<&str> = statuses.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    // Only the most recent of each project in scope, with a long but cut piece of the content.
    assert_eq!(lines.len(), 1, "{t}");
    assert!(lines[0].contains("Estado de p") && lines[0].contains("siguiente: la web") && lines[0].ends_with('…'), "{t}");
    assert!(!t.contains("Estado viejo de p") && !t.contains("Estado de otro"), "{t}");
    // It isn't repeated in «Last worked on», which keeps its 5, nor counts as injected.
    let (_, recent) = t.split_once("Last worked on").unwrap();
    let recent: Vec<&str> = recent.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(recent.len(), recall::MAX_RECENT, "{t}");
    assert!(!recent.iter().any(|l| l.contains(&newest.id[..12])), "{t}");
    assert_eq!(recall::injected_count(&db, "s1").unwrap(), 2);
    assert!(t.contains("tag `status`"), "{t}");
}

/// The lines of the open decisions section at session start (empty if it isn't there).
fn open_lines(t: &str) -> Vec<String> {
    match t.split_once("Decisions waiting for the user") {
        Some((_, r)) => r.lines().skip(1).take_while(|l| l.starts_with("- ") || l.starts_with('…')).map(str::to_string).collect(),
        None => Vec::new(),
    }
}

#[test]
fn session_start_recalls_open_decisions() {
    let (_dir, mut db) = base("abiertas");
    with_memories(&mut db);
    let decision = |project: &str, title: &str, kind: Kind, tags: &[&str]| NewMemory {
        project: project.into(),
        kind: Some(kind),
        title: title.into(),
        content: format!("{title}: ¿cobramos por usuario o por equipo? Opciones: A, B. Asumido: por equipo hasta que decida. {}", "más ".repeat(80)),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let old = memoria::save(&mut db, decision("/p", "Precio del plan Pro", Kind::Decision, &["Open", "precios"]), None).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    memoria::save(&mut db, decision("", "Licencia de las plantillas", Kind::Decision, &["open"]), None).unwrap();
    // Not a fact with the tag, nor a decision without it, nor another project's.
    memoria::save(&mut db, decision("/p", "Hecho etiquetado open", Kind::Fact, &["open"]), None).unwrap();
    memoria::save(&mut db, decision("/p", "Decisión ya tomada", Kind::Decision, &[]), None).unwrap();
    memoria::save(&mut db, decision("/otro", "Decisión de otro proyecto", Kind::Decision, &["open"]), None).unwrap();

    let t = recall::on_session_start(&mut db, "/p", &entry("s1", "")).unwrap().unwrap();
    let l = open_lines(&t);
    // From newest to oldest (the global one counts), with a short snippet of the content.
    assert_eq!(l.len(), 2, "{t}");
    assert!(l[0].contains("Licencia de las plantillas") && l[1].contains("Precio del plan Pro"), "{t}");
    assert!(l[1].contains("Asumido: por equipo") && l[1].ends_with('…') && l[1].chars().count() < 300, "{t}");
    assert!(!l.iter().any(|x| x.contains("Hecho") || x.contains("Decisión")), "{t}");
    assert!(t.contains("drop the `open` tag"), "{t}");
    // They aren't repeated in «Last worked on», which keeps its 5.
    let (_, recent) = t.split_once("Last worked on").unwrap();
    let recent: Vec<&str> = recent.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(recent.len(), recall::MAX_RECENT, "{t}");
    assert!(!recent.iter().any(|l| l.contains("Precio del plan Pro") || l.contains("Licencia de las plantillas")), "{t}");
    // They don't count as injected: still only the preference and the pinned one.
    assert_eq!(recall::injected_count(&db, "s1").unwrap(), 2);

    // When the person decides: the answer is appended and the tag removed, and it stops showing.
    let c = memoria::Changes { append: Some("Decidido: por usuario.".into()), tags: Some(vec!["precios".into()]), ..Default::default() };
    memoria::update(&mut db, &old.id, c, None).unwrap();
    let t = recall::on_session_start(&mut db, "/p", &entry("s2", "")).unwrap().unwrap();
    let l = open_lines(&t);
    assert_eq!(l.len(), 1, "{t}");
    assert!(l[0].contains("Licencia de las plantillas"), "{t}");

    // With none open, there's no section.
    let remaining = memoria::open_decisions(&db, &memoria::Scope::All, 10).unwrap();
    assert_eq!(remaining.len(), 2, "la global y la de /otro");
    for m in remaining {
        memoria::update(&mut db, &m.id, memoria::Changes { tags: Some(Vec::new()), ..Default::default() }, None).unwrap();
    }
    let t = recall::on_session_start(&mut db, "/p", &entry("s3", "")).unwrap().unwrap();
    assert!(!t.contains("Decisions waiting for the user"), "{t}");
}

#[test]
fn session_start_counts_open_decisions_that_dont_fit() {
    let (_dir, mut db) = base("abiertas-muchas");
    let topics = ["precios", "licencia", "logotipo", "dominio", "idiomas", "soporte", "pagos", "correo", "nube", "móvil", "datos", "marca", "equipo"];
    for topic in topics {
        let n = NewMemory {
            project: "/p".into(),
            kind: Some(Kind::Decision),
            title: format!("Qué hacer con {topic}"),
            content: format!("Pregunta abierta sobre {topic}; asumido lo de siempre."),
            tags: vec!["open".into()],
            ..Default::default()
        };
        memoria::save(&mut db, n, None).unwrap();
    }
    let t = recall::on_session_start(&mut db, "/p", &entry("s1", "")).unwrap().unwrap();
    let l = open_lines(&t);
    assert_eq!(l.iter().filter(|x| x.starts_with("- ")).count(), recall::MAX_OPEN_DECISIONS, "{t}");
    assert!(l.last().unwrap().contains(&format!("{} more", topics.len() - recall::MAX_OPEN_DECISIONS)), "{t}");
}

#[test]
fn each_message_brings_what_fits_or_nothing() {
    let (_dir, mut db) = base("mensaje");
    with_memories(&mut db);
    // Relevant: shares infrequent words with a memory.
    let t = recall::on_prompt(&mut db, "/p", &entry("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap();
    let t = t.expect("debería inyectar la del token");
    assert!(t.contains("Bug en auth"), "{t}");
    // Chit-chat and short messages: nothing.
    for p in ["vale, sigue", "como ha ido todo?", "1 ok 2 ok 3 ok", "perfecto gracias, dale"] {
        assert!(recall::on_prompt(&mut db, "/p", &entry("s1", p), None).unwrap().is_none(), "«{p}» no debería inyectar nada");
    }
    // Unrelated to any memory: nothing.
    assert!(recall::on_prompt(&mut db, "/p", &entry("s1", "receta de paella valenciana con conejo y garrofó"), None).unwrap().is_none());
    // The same memory isn't repeated in the same session…
    assert!(recall::on_prompt(&mut db, "/p", &entry("s1", "otra vez el middleware de autenticación y la caducidad del token UTC"), None).unwrap().is_none());
    // …but it is in another session, and in the same one after compacting.
    assert!(recall::on_prompt(&mut db, "/p", &entry("s2", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_some());
    recall::on_session_start(&mut db, "/p", &entry("s1", "")).unwrap();
    assert!(recall::on_prompt(&mut db, "/p", &entry("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_some());
}

/// With very few memories, the message words that are in none of them must not sink the
/// coverage of the one that is on topic (a real case from the test with Claude Code).
#[test]
fn small_base_and_words_that_are_not_there() {
    let (_dir, mut db) = base("pequena");
    for (k, t, c, p) in [
        (Kind::Preference, "Respuestas cortas y en español", "Como mucho tres frases.", ""),
        (Kind::Gotcha, "El cobro con tarjeta redondea mal los céntimos", "En src/pago.rs el importe va en f32 y pierde céntimos.", "/p"),
        (Kind::Decision, "Pasarela de pago: Redsys", "Se eligió Redsys y no Stripe porque el banco lo exige y la comisión es menor.", "/p"),
    ] {
        memoria::save(&mut db, NewMemory { project: p.into(), kind: Some(k), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
    }
    let t = recall::on_prompt(&mut db, "/p", &entry("s", "Estoy pensando en cambiar la pasarela de pago a Stripe"), None).unwrap();
    assert!(t.expect("debería traer la decisión").contains("Redsys"));
}

/// The save reminder: only in long sessions with nothing saved, and not on every message.
#[test]
fn save_reminder() {
    let (_dir, mut db) = base("recordatorio");
    with_memories(&mut db);
    recall::on_session_start(&mut db, "/p", &HookInput { session_id: "s".into(), source: "startup".into(), ..Default::default() }).unwrap();
    let chat = |db: &mut Connection| recall::on_prompt(db, "/p", &entry("s", "vale, sigue"), None).unwrap();
    // Just started: nothing, even with many messages.
    for _ in 0..12 {
        assert!(chat(&mut db).is_none());
    }
    // A session from 20 minutes ago with nothing saved: it's due, just once.
    // (the sample memories were created «now»: they're moved back so they predate the session)
    db.execute("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours')", []).unwrap();
    db.execute("UPDATE session SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-20 minutes')", []).unwrap();
    let t = chat(&mut db).expect("debería recordar");
    assert!(t.contains("nothing has been saved"), "{t}");
    assert!(chat(&mut db).is_none(), "solo una vez cada 30 minutos");
    // With something saved during the session, no longer.
    db.execute("UPDATE session SET reminded_at = NULL", []).unwrap();
    memoria::save(&mut db, NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: "Nueva".into(), content: "algo aprendido".into(), ..Default::default() }, None).unwrap();
    assert!(chat(&mut db).is_none());
    // Compacting doesn't reset the clock; a new session does.
    let before: String = db.query_row("SELECT started_at FROM session WHERE session = 's'", [], |f| f.get(0)).unwrap();
    recall::on_session_start(&mut db, "/p", &HookInput { session_id: "s".into(), source: "compact".into(), ..Default::default() }).unwrap();
    let after: String = db.query_row("SELECT started_at FROM session WHERE session = 's'", [], |f| f.get(0)).unwrap();
    assert_eq!(before, after);
}

#[test]
fn naming_a_file_brings_its_memories() {
    let (_dir, mut db) = base("rutas");
    with_memories(&mut db);
    let t = recall::on_prompt(&mut db, "/p", &entry("s1", "revisa src/middleware/auth.rs por favor"), None).unwrap().unwrap();
    assert!(t.contains("Bug en auth") && t.contains("src/middleware/auth.rs"), "{t}");
    // The file name is enough, and with backslashes on Windows too.
    let t = recall::on_prompt(&mut db, "/p", &entry("s2", r"mira src\middleware\auth.rs"), None).unwrap().unwrap();
    assert!(t.contains("Bug en auth"));
}

#[test]
fn another_project_does_not_see_the_memories() {
    let (_dir, mut db) = base("proyecto");
    with_memories(&mut db);
    assert!(recall::on_prompt(&mut db, "/otro", &entry("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_none());
    let t = recall::on_session_start(&mut db, "/otro", &entry("s1", "")).unwrap().unwrap();
    assert!(t.contains("Respuestas en español") && !t.contains("Base de datos única"), "{t}");
}

/// The real hook: `omoi recall` with the JSON Claude Code sends on stdin.
fn hook(dir: &std::path::Path, args: &[&str], json: &str) -> (bool, String, String) {
    let mut h = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .arg("recall")
        .args(args)
        .env("OMOIKANE_DB", dir.join("o.db"))
        .env_remove("OMOIKANE_LANG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    h.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
    let o = h.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn the_claude_code_hook() {
    let dir = comun::Temporal::nueva("recall-hook");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let cwd = repo.to_string_lossy().replace('\\', "\\\\");
    // No database yet: it stays out of the way.
    let (ok, out, _) = hook(&dir, &[], &format!(r#"{{"session_id":"s","cwd":"{cwd}","prompt":"el token caduca","hook_event_name":"UserPromptSubmit"}}"#));
    assert!(ok && out.is_empty());
    // With a memory of the project (the one from `cwd`, not from the process folder).
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(["--dir", repo.to_str().unwrap(), "remember", "-k", "gotcha", "-t", "Bug en auth: el token caduca antes de tiempo"])
        .arg("El middleware de autenticación compara la hora local con la caducidad del token, que va en UTC.")
        .env("OMOIKANE_DB", dir.join("o.db"))
        .output()
        .unwrap();
    assert!(o.status.success());
    let (ok, out, _) = hook(&dir, &["--session-start"], &format!(r#"{{"session_id":"s","cwd":"{cwd}","source":"startup","hook_event_name":"SessionStart"}}"#));
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("1 memories"));
    let (ok, out, _) = hook(
        &dir,
        &[],
        &format!(r#"{{"session_id":"s","cwd":"{cwd}","prompt":"el middleware de autenticación y la caducidad del token en UTC","hook_event_name":"UserPromptSubmit"}}"#),
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("Bug en auth"));
    // Broken input: no error and no output (a hook must never block the user).
    let (ok, out, _) = hook(&dir, &[], "esto no es json");
    assert!(ok && out.is_empty());
}

/// 1.11: opening a file (`PreToolUse` of Read, Edit, Write) brings the memories that have it in
/// their paths even if the message doesn't name it, the gotcha first; once per session, never another file
/// whose name merely ends the same way, and nothing with a tool that doesn't open files.
#[test]
fn opening_a_file_brings_its_memories() {
    let (_dir, mut db) = base("abrir");
    let save = |db: &mut Connection, kind: Kind, title: &str, paths: &[&str]| {
        let n = NewMemory { project: "/p".into(), kind: Some(kind), title: title.into(), content: format!("{title}."), paths: paths.iter().map(|r| r.to_string()).collect(), ..Default::default() };
        memoria::save(db, n, None).unwrap().id
    };
    let fact = save(&mut db, Kind::Fact, "Cómo se firma el bundle", &["src/okf.rs"]);
    let trap = save(&mut db, Kind::Gotcha, "El YAML entrecomilla distinto", &["/p/src/okf.rs", "tests/okf.rs"]);
    let other = save(&mut db, Kind::Gotcha, "Otro fichero que acaba igual", &["src/xokf.rs"]);
    let open = |session: &str, tool: &str, f: &str| HookInput { session_id: session.into(), tool_name: tool.into(), tool_input: serde_json::json!({ "file_path": f }), ..Default::default() };

    let t = recall::on_file_open(&mut db, "/p", &open("s1", "Read", "/p/src/okf.rs")).unwrap().expect("sus memorias");
    assert!(t.contains("Memories about `src/okf.rs`"), "{t}");
    let (i_trap, i_fact) = (t.find(&trap[..12]).expect("la trampa"), t.find(&fact[..12]).expect("el hecho"));
    assert!(i_trap < i_fact, "la trampa primero: {t}");
    assert!(!t.contains(&other[..12]), "`xokf.rs` no es `okf.rs`: {t}");

    // Again in the same session: it already has them. In another session, they come back.
    assert_eq!(recall::on_file_open(&mut db, "/p", &open("s1", "Edit", "src/okf.rs")).unwrap(), None);
    assert!(recall::on_file_open(&mut db, "/p", &open("s2", "Edit", "src/okf.rs")).unwrap().is_some());
    // What doesn't open files, or a file without memories: nothing.
    assert_eq!(recall::on_file_open(&mut db, "/p", &open("s3", "Bash", "src/okf.rs")).unwrap(), None);
    assert_eq!(recall::on_file_open(&mut db, "/p", &open("s3", "Read", "README.md")).unwrap(), None);
    // And what already arrived with a message isn't repeated on open.
    let t = recall::on_file_open(&mut db, "/p", &open("s4", "Read", "tests/okf.rs")).unwrap().unwrap();
    assert!(t.contains(&trap[..12]));
    assert_eq!(recall::on_file_open(&mut db, "/p", &open("s4", "Read", "src/okf.rs")).unwrap().map(|t| t.contains(&trap[..12])), Some(false));
}

/// Through the CLI, as the mod calls it: `omoi recall --file-open` with the hook's JSON.
#[test]
fn recall_file_open_through_the_cli() {
    let (dir, mut db) = base("abrir-cli");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let root = omoikane::proyecto::of_dir(&repo).unwrap();
    let n = NewMemory { project: root.clone(), kind: Some(Kind::Gotcha), title: "Cuidado con el lector".into(), content: "Lee en NFC.".into(), paths: vec!["src/lector.rs".into()], ..Default::default() };
    memoria::save(&mut db, n, None).unwrap();
    drop(db);
    let mut child = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(["--dir", root.as_str(), "recall", "--file-open"])
        .env("OMOIKANE_DB", dir.join("o.db"))
        .env("OMOIKANE_MODELS", dir.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let e = serde_json::json!({ "session_id": "x", "tool_name": "Write", "tool_input": { "file_path": repo.join("src").join("lector.rs") } });
    child.stdin.take().unwrap().write_all(e.to_string().as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stdout)));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("Cuidado con el lector"), "{v}");
}
