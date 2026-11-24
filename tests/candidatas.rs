//! Phase 7.2: candidates taken from a Claude Code conversation (a made-up `.jsonl` with the
//! shape of the real one).

mod comun;

use omoikane::candidatas;
use omoikane::db;
use omoikane::memoria::{self, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use serde_json::json;
use std::path::PathBuf;

fn dir(name: &str) -> comun::Temporal {
    comun::Temporal::nueva(&format!("cand-{name}"))
}

fn assistant(text: &str) -> serde_json::Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}})
}
fn user(text: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": text}})
}
fn tool(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
}
fn tool_result(id: &str, output: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": output}]}})
}

fn conversation(d: &std::path::Path) -> PathBuf {
    let lines = [
        json!({"type": "file-history-snapshot", "snapshot": {}}),
        user("vamos a seguir con la web"),
        assistant("Te he preparado tres diseños.\n\n¿Cuál te gusta más para la portada?"),
        user("Me gusta la segunda, pero sin el fondo verde, que no lo veo"),
        assistant("Hecho. ¿Lo subo?"),
        user("vale"),
        tool("t1", "AskUserQuestion", json!({"questions": []})),
        tool_result("t1", "The user answered: \"¿Qué tipografía usamos en los títulos?\"=\"una serif clásica\". Read the answers"),
        assistant("Perfecto. ¿Algo más?"),
        user("prefiero que los commits vayan en inglés"),
        assistant("¿Pongo la clave en el .env?"),
        user("sí, ponla: ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        tool("t2", "Bash", json!({"command": "git commit -q -m \"Portada nueva con la segunda opción\""})),
        tool_result("t2", "[master 1a2b3c] Portada nueva"),
        json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "<system-reminder>no cuenta</system-reminder>"}}),
    ];
    let path = d.join("sesion.jsonl");
    std::fs::write(&path, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    path
}

/// 7.5: the same conversation in each agent's shape gives the same decision.
#[test]
fn reads_each_agents_conversation() {
    let d = dir("agentes");
    let question = "Tengo dos colores para la portada. ¿Cuál te gusta?";
    let answer = "el azul, pero más oscuro que el de ahora";
    let expected = format!("¿Cuál te gusta? → {answer}");
    let formats: Vec<(&str, Vec<serde_json::Value>)> = vec![
        ("codex", vec![
            json!({"type": "session_meta", "payload": {"id": "s", "cwd": "/w"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>cwd /w</environment_context>"}]}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": question}]}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": answer}]}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": "{\"command\":[\"bash\",\"-lc\",\"git commit -m \\\"Portada azul\\\"\"]}"}}),
        ]),
        ("gemini", vec![
            json!({"sessionId": "s", "projectHash": "h", "startTime": "2026-10-29T10:00:00Z", "kind": "main"}),
            json!({"id": "m1", "type": "gemini", "content": "Tengo dos col"}),
            json!({"id": "m1", "type": "gemini", "content": question}),
            json!({"id": "m2", "type": "user", "content": answer}),
            json!({"$set": {"lastUpdated": "2026-10-29T10:01:00Z"}}),
        ]),
        ("qwen", vec![
            json!({"type": "assistant", "sessionId": "s", "message": {"role": "model", "parts": [{"text": "pensando…", "thought": true}, {"text": question}]}}),
            json!({"type": "user", "sessionId": "s", "message": {"role": "user", "parts": [{"text": answer}]}}),
        ]),
        ("copilot", vec![
            json!({"type": "session.start", "data": {"sessionId": "s"}}),
            json!({"type": "user.message", "data": {"content": "contexto de una skill", "source": "skill-docs"}}),
            json!({"type": "assistant.message", "data": {"content": question}}),
            json!({"type": "user.message", "data": {"content": answer}}),
        ]),
        ("cursor", vec![
            json!({"role": "user", "message": {"content": [{"type": "text", "text": "<git_status>limpio</git_status>"}]}}),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": question}]}}),
            json!({"role": "user", "message": {"content": [{"type": "text", "text": format!("jueves, 29 oct\n<user_query>\n{answer}\n</user_query>")}]}}),
            json!({"type": "turn_ended"}),
        ]),
        // Grok Build (`chat_history.jsonl`): what Grok injects carries `synthetic_reason`.
        ("grok", vec![
            json!({"type": "system", "content": "You are Grok", "synthetic_reason": "primary"}),
            json!({"type": "user", "content": [{"type": "text", "text": "# AGENTS.md del proyecto con skills"}], "synthetic_reason": "project_instructions"}),
            json!({"type": "assistant", "content": question, "model_id": "grok-4.6"}),
            json!({"type": "user", "content": [{"type": "text", "text": answer}], "prompt_index": 1}),
            json!({"type": "user", "content": [{"type": "text", "text": "<system-reminder>git_status limpio</system-reminder>"}], "synthetic_reason": "system_reminder"}),
            json!({"type": "assistant", "content": "", "tool_calls": [{"id": "c1", "name": "run_terminal_command", "arguments": "{\"command\":\"git commit -m \\\"Portada azul\\\"\"}"}]}),
            json!({"type": "tool_result", "tool_call_id": "c1", "content": "[main 1a2b3c] Portada azul"}),
        ]),
        ("generico", vec![
            json!({"role": "assistant", "text": question}),
            json!({"role": "user", "text": answer}),
            json!({"role": "tool", "command": "git tag -a v2.0.0 -m 2.0.0"}),
        ]),
    ];
    for (name, lines) in formats {
        let path = d.join(format!("{name}.jsonl"));
        std::fs::write(&path, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let cs = candidatas::de_conversacion(&path).unwrap();
        let all: Vec<String> = cs.iter().map(|c| format!("[{}] {}", c.kind, c.text)).collect();
        assert!(all.iter().any(|t| t.ends_with(&expected)), "{name}: {all:?}");
        // What the agent injects (context, skills, git status) doesn't come out as a decision.
        assert!(!all.iter().any(|t| t.contains("environment_context") || t.contains("skill") || t.contains("git_status") || t.contains("Tengo dos col →")), "{name}: {all:?}");
        if name == "codex" || name == "grok" {
            assert!(all.iter().any(|t| t == "[milestone] last commit: Portada azul"), "{all:?}");
        }
        if name == "generico" {
            assert!(all.iter().any(|t| t == "[milestone] published: tag v2.0.0"), "{all:?}");
        }
    }
    // Gemini before June 2026: a whole JSON with `messages`.
    let path = d.join("gemini-viejo.json");
    std::fs::write(&path, json!({"sessionId": "s", "messages": [{"type": "gemini", "content": question}, {"type": "user", "content": answer}]}).to_string()).unwrap();
    let cs = candidatas::de_conversacion(&path).unwrap();
    assert!(cs.iter().any(|c| c.text.ends_with(&expected)), "{cs:?}");
}

#[test]
fn extracts_decisions_preferences_and_milestones_without_secrets() {
    let d = dir("saca");
    let cs = candidatas::de_conversacion(&conversation(&d)).unwrap();
    let texts: Vec<String> = cs.iter().map(|c| format!("[{}] {}", c.kind, c.text)).collect();
    let all = texts.join("\n");
    // The preference first; the decision goes with its question; the AskUserQuestion answer too.
    assert!(texts[0].starts_with("[preference]") && texts[0].contains("commits vayan en inglés"), "{all}");
    assert!(all.contains("¿Cuál te gusta más para la portada? → Me gusta la segunda, pero sin el fondo verde"), "{all}");
    assert!(all.contains("¿Qué tipografía usamos en los títulos? → una serif clásica"), "{all}");
    assert!(all.contains("[milestone] last commit: Portada nueva con la segunda opción"), "{all}");
    // Nothing trivial, nothing with secrets, nothing the system injects.
    assert!(!all.contains("→ vale") && !all.contains("ghp_") && !all.contains("no cuenta"), "{all}");
}

#[test]
fn proposed_once_in_the_next_session_and_not_what_is_already_saved() {
    let d = dir("propone");
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    // Already saved: not proposed again.
    let saved = NewMemory { project: "/web".into(), kind: Some(Kind::Preference), title: "Commits en inglés".into(), content: "Prefiere que los commits vayan en inglés.".into(), ..Default::default() };
    memoria::save(&mut db, saved, None).unwrap();
    let n = candidatas::apartar(&db, "/web", "s1", &conversation(&d)).unwrap();
    let all = candidatas::todas(&db).unwrap();
    assert_eq!(n, all.len());
    assert!(!all.iter().any(|c| c.text.contains("commits vayan en inglés")), "{all:?}");
    // Repeating (PreCompact and then SessionEnd) doesn't duplicate.
    candidatas::apartar(&db, "/web", "s1", &conversation(&d)).unwrap();
    assert_eq!(candidatas::todas(&db).unwrap().len(), n);
    // In the same session they don't show up; in the next one, they do, and only once.
    let e = |s: &str| HookInput { session_id: s.into(), ..Default::default() };
    let t = recall::on_session_start(&mut db, "/web", &e("s1")).unwrap().unwrap();
    assert!(!t.contains("possibly worth keeping"), "{t}");
    let t = recall::on_session_start(&mut db, "/web", &e("s2")).unwrap().unwrap();
    assert!(t.contains("possibly worth keeping") && t.contains("sin el fondo verde"), "{t}");
    let t = recall::on_session_start(&mut db, "/web", &e("s3")).unwrap().unwrap();
    assert!(!t.contains("possibly worth keeping"), "{t}");
    // Another project doesn't see them.
}

/// 1.4: each subagent's final report, which Claude Code stores in `<session>/subagents/`, is set aside
/// with its task; one with a secret isn't; and the main session's stuff still comes out the same.
#[test]
fn sets_aside_each_subagents_report() {
    let d = dir("subagentes");
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    let session = conversation(&d);
    let sub = d.join("sesion").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    let side = |mut v: serde_json::Value| {
        v["isSidechain"] = json!(true);
        v
    };
    let report = "El cuello de botella no era el modelo: SQLite hacía el checkpoint del WAL al cerrar cada conexión.";
    let lines = [
        side(user("Eres el agente «velocidad» de una revisión.\n\nMide el recall con la base de prueba.")),
        side(assistant("Empiezo por medir.")),
        side(tool("b1", "Bash", json!({"command": "cargo build --release"}))),
        side(tool_result("b1", "Finished")),
        side(assistant(report)),
        side(json!({"type": "api-request", "message": {}})),
    ];
    std::fs::write(sub.join("agent-a1.jsonl"), lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    let secret = [side(user("Prueba la API")), side(assistant("Funciona con password: hunter2"))];
    std::fs::write(sub.join("agent-a2.jsonl"), secret.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    std::fs::write(sub.join("notas.txt"), "no es una conversación").unwrap();
    // One that's still working (compaction happens while it runs): its last message asks for a tool.
    let mut asks = tool("b2", "Bash", json!({"command": "cargo test"}));
    asks["message"]["content"].as_array_mut().unwrap().insert(0, json!({"type": "text", "text": "Ahora los tests."}));
    asks["message"]["stop_reason"] = json!("tool_use");
    let running = [side(user("Revisa el sync")), side(asks)];
    std::fs::write(sub.join("agent-a3.jsonl"), running.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    assert!(candidatas::de_subagente(&sub.join("agent-a3.jsonl")).unwrap().is_none(), "uno que no ha acabado no se aparta");
    // A big one (here 1 MB of tools in the middle): only the start and the end are read.
    let filler = side(tool_result("b1", &"x".repeat(1000)));
    let mut big = vec![side(user("Revisa el benchmark entero")), side(tool("b1", "Bash", json!({"command": "ls"})))];
    big.extend(std::iter::repeat_n(filler, 1000));
    big.push(side(assistant("Hecho: el benchmark da 89 %.")));
    std::fs::write(sub.join("agent-a4.jsonl"), big.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    let (g, _) = candidatas::de_subagente(&sub.join("agent-a4.jsonl")).unwrap().unwrap();
    assert_eq!(g.text, "Revisa el benchmark entero → Hecho: el benchmark da 89 %.");
    std::fs::remove_file(sub.join("agent-a4.jsonl")).unwrap();

    let (c, only) = candidatas::de_subagente(&sub.join("agent-a1.jsonl")).unwrap().unwrap();
    assert_eq!(c.kind, candidatas::SUBAGENTE);
    assert_eq!(c.text, format!("Eres el agente «velocidad» de una revisión. → {report}"));
    assert_eq!(only, report);
    assert!(candidatas::de_subagente(&sub.join("agent-a2.jsonl")).unwrap().is_none(), "con un secreto no se aparta");

    candidatas::apartar(&db, "/web", "s1", &session).unwrap();
    let all = candidatas::todas(&db).unwrap();
    assert_eq!(all.iter().filter(|c| c.kind == "subagent").count(), 1, "{all:?}");
    assert!(all.iter().any(|c| c.text.contains("sin el fondo verde")), "lo de la sesión principal sigue: {all:?}");
    // PreCompact and then SessionEnd: it isn't duplicated.
    let n = all.len();
    candidatas::apartar(&db, "/web", "s1", &session).unwrap();
    assert_eq!(candidatas::todas(&db).unwrap().len(), n);
    let t = recall::on_session_start(&mut db, "/web", &HookInput { session_id: "s2".into(), ..Default::default() }).unwrap().unwrap();
    assert!(t.contains("[subagent] Eres el agente «velocidad»") && t.contains("checkpoint del WAL"), "{t}");
    // A report already saved as a memory isn't proposed again.
    let mut db2 = db::abrir(&d.join("o2.db")).unwrap();
    let saved = NewMemory { project: "/web".into(), kind: Some(Kind::Gotcha), title: "El recall lento era el checkpoint del WAL".into(), content: report.into(), ..Default::default() };
    memoria::save(&mut db2, saved, None).unwrap();
    candidatas::apartar(&db2, "/web", "s1", &session).unwrap();
    assert!(!candidatas::todas(&db2).unwrap().iter().any(|c| c.kind == "subagent"));
}
