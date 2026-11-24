//! 1.4: the session diary. One line per session and project, taken from the conversation (made-up `.jsonl`
//! files shaped like real ones) and from what the database knows (`recall_log`).

mod comun;

use omoikane::db;
use omoikane::diario::{self, Filtro};
use omoikane::memoria::{self, Scope, NewMemory, Kind};
use serde_json::json;
use std::path::Path;

fn dir(name: &str) -> comun::Temporal {
    let d = comun::Temporal::nueva(&format!("diario-{name}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    std::fs::create_dir_all(d.join("repo").join("src")).unwrap();
    d
}

fn write(path: &Path, lines: &[serde_json::Value]) {
    std::fs::write(path, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
}

fn tool(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
    json!({"parentUuid": null, "type": "assistant", "timestamp": "2026-11-03T09:00:00.000Z", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
}
fn tool_result(id: &str, output: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": [{"type": "text", "text": output}]}]}})
}

fn save(db: &mut rusqlite::Connection, project: &str, title: &str, pinned: bool) -> String {
    let n = NewMemory { project: project.into(), kind: Some(Kind::Fact), title: title.into(), content: format!("{title}, con detalle."), pinned, ..Default::default() };
    memoria::save(db, n, None).unwrap().id
}

fn received(db: &rusqlite::Connection, session: &str, id: &str) {
    db.execute("INSERT INTO recall_log(session, memory_id, at, project_id) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), 0)", [session, id]).unwrap();
}

#[test]
fn one_line_per_session_with_files_and_memories() {
    let d = dir("claude");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let new = save(&mut db, &repo, "El puerto es el 8010", false);
    let changed = save(&mut db, &repo, "Sync por git", false);
    let used = save(&mut db, &repo, "La caché vive en D:", false);
    let pinned = save(&mut db, &repo, "Siempre en español", true);
    let duplicate = save(&mut db, &repo, "Ya estaba", false);
    received(&db, "s1", &used);
    received(&db, "s1", &pinned);
    let outside = d.join("otro").join("notas.md");
    let lines = vec![
        json!({"type": "permission-mode", "permissionMode": "default"}),
        json!({"type": "user", "timestamp": "2026-11-03T08:59:00.000Z", "message": {"role": "user", "content": "vamos"}}),
        tool("t1", "Edit", json!({"file_path": format!("{repo}/src/sync.rs"), "old_string": "a", "new_string": "b"})),
        tool_result("t1", "ok"),
        tool("t2", "Write", json!({"file_path": outside.to_string_lossy()})),
        tool("t3", "MultiEdit", json!({"file_path": format!("{repo}/src/db.rs"), "edits": []})),
        tool("t4", "NotebookEdit", json!({"notebook_path": format!("{repo}/notas.ipynb")})),
        tool("t5", "Read", json!({"file_path": format!("{repo}/src/leido.rs")})),
        tool("t6", "Edit", json!({"file_path": format!("{repo}/src/sync.rs")})),
        // What the agent saves and changes with Omoikane's tools (with its id in the response).
        tool("t7", "mcp__omoikane__remember", json!({"kind": "fact", "title": "El puerto es el 8010", "content": "…"})),
        tool_result("t7", &serde_json::to_string_pretty(&json!({"saved": true, "id": new, "project": repo, "related": [{"id": used, "title": "x"}]})).unwrap()),
        // 16 letters: the first 10 are the millisecond, which they may share with another one here.
        tool("t8", "mcp__omoikane__update", json!({"id": &changed[..16], "append": "y más"})),
        tool("t9", "mcp__omoikane__remember", json!({"kind": "fact", "title": "Ya estaba", "content": "…"})),
        tool_result("t9", &json!({"saved": false, "duplicate_of": {"id": duplicate}}).to_string()),
        // Another tool called `update` that is not Omoikane's, and an id that does not exist here.
        tool("t10", "mcp__claude_ai_Docs__update", json!({"id": used})),
        tool_result("t10", &json!({"id": used}).to_string()),
        tool("t11", "remember", json!({})),
        tool_result("t11", &json!({"saved": true, "id": "01AAAAAAAAAAAAAAAAAAAAAAAA"}).to_string()),
        // An `update` that failed: its response does not carry the memory, so it does not count.
        tool("t13", "mcp__omoikane__update", json!({"id": duplicate, "title": "x"})),
        tool_result("t13", "that is in another space"),
    ];
    let conv = d.join("s1.jsonl");
    write(&conv, &lines);
    assert!(diario::apuntar(&db, &repo, "s1", Some(&conv)).unwrap());
    let v = diario::lineas(&db, &Scope::from_project(&db, &repo).unwrap(), &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert_eq!(v.len(), 1, "{v:?}");
    let l = &v[0];
    let outside = outside.to_string_lossy().replace('\\', "/");
    assert_eq!(l.files, vec!["src/sync.rs".to_string(), outside, "src/db.rs".into(), "notas.ipynb".into()]);
    assert_eq!(l.first_at, "2026-11-03T08:59:00.000Z");
    // Received: the one that came through the message; the pinned one comes in every session and does not count.
    assert_eq!(l.recalled.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![used.as_str()]);
    assert_eq!(l.recalled[0].title.as_deref(), Some("La caché vive en D:"));
    // Saved: the new one and the changed one (by the start of its id); not the duplicate nor anything foreign.
    assert_eq!(l.saved.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![new.as_str(), changed.as_str()]);

    // After compacting, the session's `recall_log` is emptied and more comes in: the line is completed, not lost.
    db.execute("DELETE FROM recall_log WHERE session = 's1'", []).unwrap();
    let another = save(&mut db, &repo, "Otra recibida", false);
    received(&db, "s1", &another);
    let mut more = lines.clone();
    more.push(tool("t12", "Write", json!({"file_path": format!("{repo}/README.md")})));
    write(&conv, &more);
    diario::apuntar(&db, &repo, "s1", Some(&conv)).unwrap();
    let l = &diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap()[0];
    assert_eq!(l.files.last().map(String::as_str), Some("README.md"));
    assert_eq!(l.files.len(), 5);
    assert_eq!(l.recalled.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![used.as_str(), another.as_str()]);
    assert_eq!(l.saved.len(), 2);
    assert_eq!(db.query_row("SELECT count(*) FROM diary", [], |f| f.get::<_, i64>(0)).unwrap(), 1);
    // Without a conversation (the agent does not give it): what the database knows remains.
    assert!(diario::apuntar(&db, &repo, "s2", None).unwrap());
    assert!(!diario::apuntar(&db, &repo, "", None).unwrap());
}

#[test]
fn files_from_other_agents() {
    let d = dir("agentes");
    let root = "/w/repo";
    let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch";
    let formats: Vec<(&str, Vec<serde_json::Value>, Vec<&str>)> = vec![
        ("codex", vec![
            json!({"timestamp": "2026-11-03T10:00:00.000Z", "type": "session_meta", "payload": {"id": "s", "cwd": root}}),
            json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "call_id": "c1", "input": patch}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "call_id": "c2", "arguments": json!({"command": ["apply_patch", "*** Begin Patch\n*** Delete File: viejo.txt\n*** End Patch"]}).to_string()}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "mcp__omoikane__remember", "call_id": "c3", "arguments": "{\"title\":\"t\"}"}}),
            json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": "c3", "output": json!({"content": [{"type": "text", "text": "{\"saved\": true, \"id\": \"01JCODEXCODEXCODEXCODEXCOD\"}"}]}).to_string()}}),
        ], vec!["src/a.rs", "src/b.rs", "viejo.txt"]),
        ("gemini", vec![
            json!({"$set": {"messages": [{"id": "m1", "type": "gemini", "content": "", "toolCalls": [
                {"id": "g1", "name": "write_file", "args": {"file_path": "/w/repo/docs/x.md", "content": "…"}},
                {"id": "g2", "name": "replace", "args": {"file_path": "/w/repo/src/c.rs"}},
                {"id": "g3", "name": "read_file", "args": {"file_path": "/w/repo/src/leido.rs"}},
                {"id": "g4", "name": "remember", "args": {}, "result": [{"functionResponse": {"id": "g4", "name": "remember", "response": {"output": "{\"saved\": true, \"id\": \"01JGEMINIGEMINIGEMINIGEMIN\"}"}}}]}
            ]}]}}),
        ], vec!["docs/x.md", "src/c.rs"]),
        ("qwen", vec![
            json!({"type": "assistant", "message": {"role": "model", "parts": [{"functionCall": {"id": "q1", "name": "write_file", "args": {"file_path": "/w/repo/q.txt"}}}]}}),
        ], vec!["q.txt"]),
        ("copilot", vec![
            json!({"type": "tool.execution_start", "data": {"toolCallId": "p1", "toolName": "str_replace_editor", "arguments": {"command": "view", "path": "/w/repo/visto.rs"}}}),
            json!({"type": "tool.execution_start", "data": {"toolCallId": "p2", "toolName": "edit", "arguments": {"path": "/w/repo/src/e.rs"}}}),
            json!({"type": "tool.execution_start", "data": {"toolCallId": "p3", "toolName": "create", "arguments": {"path": "/w/repo/nuevo.rs"}}}),
        ], vec!["src/e.rs", "nuevo.rs"]),
        ("cursor", vec![
            json!({"role": "assistant", "message": {"content": [{"type": "tool_use", "name": "edit_file", "input": {"target_file": "/w/repo/f.ts"}}]}}),
        ], vec!["f.ts"]),
        ("grok", vec![
            json!({"type": "assistant", "content": "", "tool_calls": [{"id": "k1", "name": "edit_file", "arguments": "{\"path\":\"/w/repo/g.py\"}"}]}),
        ], vec!["g.py"]),
    ];
    for (name, lines, expected) in formats {
        let path = d.join(format!("{name}.jsonl"));
        write(&path, &lines);
        let l = diario::leer(&path, root).unwrap();
        assert_eq!(l.ficheros, expected, "{name}");
        match name {
            "codex" => {
                assert_eq!(l.guardadas, vec!["01JCODEXCODEXCODEXCODEXCOD"]);
                assert_eq!(l.primera.as_deref(), Some("2026-11-03T10:00:00.000Z"));
            }
            "gemini" => assert_eq!(l.guardadas, vec!["01JGEMINIGEMINIGEMINIGEMIN"]),
            _ => assert!(l.guardadas.is_empty(), "{name}: {l:?}"),
        }
    }
    // Gemini before June 2026: a whole JSON with `messages`.
    let path = d.join("gemini-viejo.json");
    std::fs::write(&path, json!({"sessionId": "s", "startTime": "2026-11-01T07:00:00.000Z", "messages": [{"type": "gemini", "toolCalls": [{"name": "write_file", "args": {"file_path": "/w/repo/v.md"}}]}]}).to_string()).unwrap();
    let l = diario::leer(&path, root).unwrap();
    assert_eq!(l.ficheros, vec!["v.md"]);
    assert_eq!(l.primera.as_deref(), Some("2026-11-01T07:00:00.000Z"));
}

#[test]
fn filters_and_expiry() {
    let d = dir("filtros");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    let id = omoikane::proyecto::registrar(&db, &repo).unwrap();
    // With no line yet the table does not exist (opening the database does not create it), and reading does not fail.
    assert!(diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap().is_empty());
    let conv = d.join("c.jsonl");
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/sync.rs")}))]);
    diario::apuntar(&db, &repo, "hoy", Some(&conv)).unwrap();
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/db.rs")}))]);
    diario::apuntar(&db, &repo, "martes", Some(&conv)).unwrap();
    db.execute("UPDATE diary SET first_at = '2026-11-03T09:00:00.000Z', last_at = '2026-11-03T11:00:00.000Z' WHERE session = 'martes'", []).unwrap();
    // `hoy` started today (the time of the made-up conversation is Tuesday's).
    db.execute("UPDATE diary SET first_at = last_at WHERE session = 'hoy'", []).unwrap();
    // In another folder (another unregistered project): only visible from all.
    diario::apuntar(&db, &d.join("otro").to_string_lossy(), "fuera", None).unwrap();
    let sessions = |a: &Scope, f: Filtro| diario::lineas(&db, a, &f).unwrap().into_iter().map(|l| l.session).collect::<Vec<_>>();
    let project = Scope::Project { id, space: None };
    assert_eq!(sessions(&project, Filtro { limite: 10, ..Default::default() }), vec!["hoy", "martes"]);
    assert_eq!(sessions(&Scope::All, Filtro { limite: 10, ..Default::default() }).len(), 3);
    assert_eq!(sessions(&project, Filtro { limite: 1, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sessions(&project, Filtro { fichero: Some("SYNC.rs".into()), limite: 10, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sessions(&project, Filtro { fichero: Some("src\\db".into()), limite: 10, ..Default::default() }), vec!["martes"]);
    // The fragment does not match across two files.
    assert!(sessions(&project, Filtro { fichero: Some("rs\",\"".into()), limite: 10, ..Default::default() }).is_empty());
    let day = |t: &str| Some(diario::fecha(&db, t).unwrap());
    assert_eq!(sessions(&project, Filtro { desde: day("2026-11-03"), hasta: day("2026-11-03"), limite: 10, ..Default::default() }), vec!["martes"]);
    assert_eq!(sessions(&project, Filtro { desde: day("0d"), limite: 10, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sessions(&project, Filtro { hasta: day("2026-11-02"), limite: 10, ..Default::default() }), Vec::<String>::new());
    assert_eq!(sessions(&project, Filtro { hasta: day("2026-11-03"), limite: 10, ..Default::default() }), vec!["martes"]);
    // Anything older than a year is deleted when the next one is written.
    db.execute("UPDATE diary SET last_at = '2020-01-01T00:00:00.000Z' WHERE session = 'martes'", []).unwrap();
    diario::apuntar(&db, &repo, "hoy", None).unwrap();
    assert_eq!(sessions(&project, Filtro { limite: 10, ..Default::default() }), vec!["hoy"]);
}

/// What subagents did (`<session>/subagents/*.jsonl`, with `isSidechain`) also belongs to the
/// session; one that is too big is skipped.
#[test]
fn counts_what_subagents_did() {
    let d = dir("subagentes");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let theirs = save(&mut db, &repo, "Lo guardó el subagente", false);
    let conv = d.join("s1.jsonl");
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/main.rs")}))]);
    let subs = d.join("s1").join("subagents");
    std::fs::create_dir_all(&subs).unwrap();
    let sidechain = |mut v: serde_json::Value| {
        v["isSidechain"] = json!(true);
        v
    };
    write(
        &subs.join("agent-a1.jsonl"),
        &[
            sidechain(tool("u1", "Write", json!({"file_path": format!("{repo}/src/sub.rs")}))),
            sidechain(tool("u2", "Edit", json!({"file_path": format!("{repo}/src/main.rs")}))),
            sidechain(tool("u3", "mcp__omoikane__remember", json!({"title": "Lo guardó el subagente"}))),
            sidechain(tool_result("u3", &json!({"saved": true, "id": theirs}).to_string())),
        ],
    );
    // Over 16 MB: it is not read (the closing hook has little time).
    let big = tool("g1", "Write", json!({"file_path": format!("{repo}/src/grande.rs")})).to_string();
    std::fs::write(subs.join("agent-grande.jsonl"), format!("{big}\n{}", " ".repeat(17 << 20))).unwrap();
    diario::apuntar(&db, &repo, "s1", Some(&conv)).unwrap();
    let v = diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert_eq!(v[0].files, vec!["src/main.rs", "src/sub.rs"], "{v:?}");
    assert_eq!(v[0].saved.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![theirs.as_str()]);
}

/// A session in a folder that is not a project yet (an agent that only has the closing hook)
/// is left without a project, and is visible from that folder: before and after it becomes one. When a project is pruned,
/// its lines are left without a project: whoever reuses its id does not inherit them.
#[test]
fn lines_without_project_and_pruned() {
    let d = dir("podados");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    let conv = d.join("c.jsonl");
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/a.rs")}))]);
    diario::apuntar(&db, &repo, "antes", Some(&conv)).unwrap();
    diario::apuntar(&db, &d.join("otra").to_string_lossy(), "otra", None).unwrap();
    let from = |folder: &str| {
        let f = Filtro { carpeta: Some(folder.into()), limite: 10, ..Default::default() };
        diario::lineas(&db, &Scope::from_project(&db, folder).unwrap(), &f).unwrap().into_iter().map(|l| l.session).collect::<Vec<_>>()
    };
    assert_eq!(from(&repo), vec!["antes"]);
    // Now it is a project: it is visible through its folders, even if where it is viewed from is not given.
    let id = omoikane::proyecto::registrar(&db, &repo).unwrap();
    diario::apuntar(&db, &repo, "despues", None).unwrap();
    db.execute("UPDATE diary SET last_at = '2026-11-01T10:00:00.000Z' WHERE session = 'antes'", []).unwrap();
    let only = |id: i64| {
        diario::lineas(&db, &Scope::Project { id, space: None }, &Filtro { limite: 10, ..Default::default() }).unwrap().into_iter().map(|l| l.session).collect::<Vec<_>>()
    };
    assert_eq!(only(id), vec!["despues", "antes"]);
    // It is pruned (it has no memories) and another folder takes its id.
    assert_eq!(omoikane::proyecto::podar(&db).unwrap(), 1);
    assert_eq!(db.query_row("SELECT count(*) FROM diary WHERE project_id != 0", [], |f| f.get::<_, i64>(0)).unwrap(), 0);
    let new_dir = d.join("nueva");
    std::fs::create_dir_all(new_dir.join(".git")).unwrap();
    let other = omoikane::proyecto::registrar(&db, &omoikane::proyecto::de(&new_dir).unwrap()).unwrap();
    assert_eq!(other, id, "SQLite reutiliza el id");
    assert!(only(other).is_empty());
    // And from its folder they are still there, with the folder as the project.
    assert_eq!(from(&repo), vec!["despues", "antes"]);
    let v = diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert!(v.iter().all(|l| !l.project.is_empty()), "{v:?}");
    // A client's project that gets pruned takes its lines with it: without a project they would be visible from the personal space.
    let client = d.join("cliente");
    std::fs::create_dir_all(client.join(".git")).unwrap();
    let client_path = omoikane::proyecto::de(&client).unwrap();
    let client_project = omoikane::proyecto::registrar(&db, &client_path).unwrap();
    db.execute("INSERT INTO space(key, name, created_at) VALUES ('acme', 'acme', '2026-11-04T00:00:00.000Z')", []).unwrap();
    db.execute("UPDATE project SET space_id = (SELECT id FROM space WHERE key = 'acme') WHERE id = ?1", [client_project]).unwrap();
    diario::apuntar(&db, &client_path, "del-cliente", None).unwrap();
    omoikane::proyecto::podar(&db).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM diary WHERE session = 'del-cliente'", [], |f| f.get::<_, i64>(0)).unwrap(), 0);
}

/// A conversation cut in the middle of a character is read just the same.
#[test]
fn truncated_conversation() {
    let d = dir("cortada");
    let path = d.join("c.jsonl");
    let mut bytes = tool("t1", "Edit", json!({"file_path": "/w/repo/src/ñ.rs"})).to_string().into_bytes();
    bytes.extend_from_slice(b"\n{\"type\":\"user\",\"message\":\"caf\xC3");
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(diario::leer(&path, "/w/repo").unwrap().ficheros, vec!["src/ñ.rs"]);
}

/// `--file` also lowercases non-ASCII text when comparing, and `-n` counts after filtering.
#[test]
fn unicode_file_and_limit() {
    let d = dir("unicode");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    let id = omoikane::proyecto::registrar(&db, &repo).unwrap();
    let conv = d.join("c.jsonl");
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/Ñandú.rs")}))]);
    diario::apuntar(&db, &repo, "uno", Some(&conv)).unwrap();
    write(&conv, &[tool("t1", "Edit", json!({"file_path": format!("{repo}/src/b.rs")}))]);
    diario::apuntar(&db, &repo, "dos", Some(&conv)).unwrap();
    db.execute("UPDATE diary SET last_at = '2026-11-01T10:00:00.000Z' WHERE session = 'uno'", []).unwrap();
    let f = Filtro { fichero: Some("ñandú".into()), limite: 1, ..Default::default() };
    let v = diario::lineas(&db, &Scope::Project { id, space: None }, &f).unwrap();
    assert_eq!(v.iter().map(|l| l.session.as_str()).collect::<Vec<_>>(), vec!["uno"]);
}

/// 1.10: the memories the agent says it follows, quoting their id, are recorded as applied in the
/// session's line; the ones it just saved do not count, nor do uppercase words or ids that do not exist.
#[test]
fn applied_by_citation() {
    let d = dir("aplicadas");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let followed = save(&mut db, &repo, "Compilar antes del smoke test", false);
    let new = save(&mut db, &repo, "El puerto es el 8010", false);
    let conv = d.join("s9.jsonl");
    write(
        &conv,
        &[
            tool("t1", "mcp__omoikane__remember", json!({"kind": "fact", "title": "El puerto es el 8010", "content": "…"})),
            tool_result("t1", &json!({"saved": true, "id": new}).to_string()),
        ],
    );
    assert!(diario::apuntar(&db, &repo, "s9", Some(&conv)).unwrap());
    let short = &followed[..12];
    let texts = [
        format!("Sigo {short}: compilo antes del smoke test."),
        format!("Guardada como {new}. README, HTTP2 y 01AAAAAAAAAAAAAAAAAAAAAAAA no son memorias."),
    ];
    let ids = diario::citadas(&db, texts.iter().map(String::as_str)).unwrap();
    assert_eq!(ids, vec![followed.clone(), new.clone()]);
    assert_eq!(diario::apuntar_aplicadas(&db, &repo, "s9", Some(&conv), &ids).unwrap(), 1);
    // Again (after compacting it is reread in full): it is not duplicated.
    assert_eq!(diario::apuntar_aplicadas(&db, &repo, "s9", Some(&conv), &ids).unwrap(), 1);
    let l = diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert_eq!(l[0].applied.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), vec![followed.clone()]);
    let (n, last) = diario::aplicada(&db, &followed).unwrap();
    assert!(n == 1 && last.is_some());
    assert_eq!(diario::aplicada(&db, &new).unwrap().0, 0);
    // Without a line for the session, nothing is recorded.
    assert_eq!(diario::apuntar_aplicadas(&db, &repo, "otra", None, &ids).unwrap(), 0);
}

/// A diary created by an earlier version (without the `applied` column) is read just the same and gains the
/// column when the first line is written.
#[test]
fn old_diary_without_applied() {
    let d = dir("antes");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE diary (session TEXT NOT NULL, project_id INTEGER NOT NULL DEFAULT 0, folder TEXT NOT NULL DEFAULT '',
           first_at TEXT NOT NULL, last_at TEXT NOT NULL, files TEXT NOT NULL DEFAULT '[]', recalled TEXT NOT NULL DEFAULT '[]',
           saved TEXT NOT NULL DEFAULT '[]', PRIMARY KEY (session, project_id));
         INSERT INTO diary(session, folder, first_at, last_at) VALUES ('vieja', '', '2026-11-01T10:00:00Z', strftime('%Y-%m-%dT%H:%M:%fZ','now'));",
    )
    .unwrap();
    let l = diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert!(l.len() == 1 && l[0].applied.is_empty());
    assert_eq!(diario::aplicada(&db, "01M").unwrap().0, 0);
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    assert!(diario::apuntar(&db, &repo, "nueva", None).unwrap());
    assert_eq!(diario::lineas(&db, &Scope::All, &Filtro { limite: 10, ..Default::default() }).unwrap().len(), 2);
}
