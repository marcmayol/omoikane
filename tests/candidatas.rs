//! Fase 7.2: candidatas sacadas de una conversación de Claude Code (un `.jsonl` inventado con la
//! forma del de verdad).

use omoikane::candidatas;
use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use serde_json::json;
use std::path::PathBuf;

fn dir(nombre: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("omoi-cand-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn asistente(texto: &str) -> serde_json::Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": texto}]}})
}
fn usuario(texto: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": texto}})
}
fn herramienta(id: &str, nombre: &str, entrada: serde_json::Value) -> serde_json::Value {
    json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": nombre, "input": entrada}]}})
}
fn resultado(id: &str, salida: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": salida}]}})
}

fn conversacion(d: &std::path::Path) -> PathBuf {
    let lineas = [
        json!({"type": "file-history-snapshot", "snapshot": {}}),
        usuario("vamos a seguir con la web"),
        asistente("Te he preparado tres diseños.\n\n¿Cuál te gusta más para la portada?"),
        usuario("Me gusta la segunda, pero sin el fondo verde, que no lo veo"),
        asistente("Hecho. ¿Lo subo?"),
        usuario("vale"),
        herramienta("t1", "AskUserQuestion", json!({"questions": []})),
        resultado("t1", "The user answered: \"¿Qué tipografía usamos en los títulos?\"=\"una serif clásica\". Read the answers"),
        asistente("Perfecto. ¿Algo más?"),
        usuario("prefiero que los commits vayan en inglés"),
        asistente("¿Pongo la clave en el .env?"),
        usuario("sí, ponla: ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        herramienta("t2", "Bash", json!({"command": "git commit -q -m \"Portada nueva con la segunda opción\""})),
        resultado("t2", "[master 1a2b3c] Portada nueva"),
        json!({"type": "user", "isMeta": true, "message": {"role": "user", "content": "<system-reminder>no cuenta</system-reminder>"}}),
    ];
    let ruta = d.join("sesion.jsonl");
    std::fs::write(&ruta, lineas.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    ruta
}

/// 7.5: la misma conversación con la forma de cada agente da la misma decisión.
#[test]
fn lee_la_conversacion_de_cada_agente() {
    let d = dir("agentes");
    let pregunta = "Tengo dos colores para la portada. ¿Cuál te gusta?";
    let respuesta = "el azul, pero más oscuro que el de ahora";
    let esperada = format!("¿Cuál te gusta? → {respuesta}");
    let formatos: Vec<(&str, Vec<serde_json::Value>)> = vec![
        ("codex", vec![
            json!({"type": "session_meta", "payload": {"id": "s", "cwd": "/w"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>cwd /w</environment_context>"}]}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": pregunta}]}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": respuesta}]}}),
            json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": "{\"command\":[\"bash\",\"-lc\",\"git commit -m \\\"Portada azul\\\"\"]}"}}),
        ]),
        ("gemini", vec![
            json!({"sessionId": "s", "projectHash": "h", "startTime": "2026-10-29T10:00:00Z", "kind": "main"}),
            json!({"id": "m1", "type": "gemini", "content": "Tengo dos col"}),
            json!({"id": "m1", "type": "gemini", "content": pregunta}),
            json!({"id": "m2", "type": "user", "content": respuesta}),
            json!({"$set": {"lastUpdated": "2026-10-29T10:01:00Z"}}),
        ]),
        ("qwen", vec![
            json!({"type": "assistant", "sessionId": "s", "message": {"role": "model", "parts": [{"text": "pensando…", "thought": true}, {"text": pregunta}]}}),
            json!({"type": "user", "sessionId": "s", "message": {"role": "user", "parts": [{"text": respuesta}]}}),
        ]),
        ("copilot", vec![
            json!({"type": "session.start", "data": {"sessionId": "s"}}),
            json!({"type": "user.message", "data": {"content": "contexto de una skill", "source": "skill-docs"}}),
            json!({"type": "assistant.message", "data": {"content": pregunta}}),
            json!({"type": "user.message", "data": {"content": respuesta}}),
        ]),
        ("cursor", vec![
            json!({"role": "user", "message": {"content": [{"type": "text", "text": "<git_status>limpio</git_status>"}]}}),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": pregunta}]}}),
            json!({"role": "user", "message": {"content": [{"type": "text", "text": format!("jueves, 29 oct\n<user_query>\n{respuesta}\n</user_query>")}]}}),
            json!({"type": "turn_ended"}),
        ]),
        ("generico", vec![
            json!({"role": "assistant", "text": pregunta}),
            json!({"role": "user", "text": respuesta}),
            json!({"role": "tool", "command": "git tag -a v2.0.0 -m 2.0.0"}),
        ]),
    ];
    for (nombre, lineas) in formatos {
        let ruta = d.join(format!("{nombre}.jsonl"));
        std::fs::write(&ruta, lineas.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        let cs = candidatas::de_conversacion(&ruta).unwrap();
        let todo: Vec<String> = cs.iter().map(|c| format!("[{}] {}", c.kind, c.text)).collect();
        assert!(todo.iter().any(|t| t.ends_with(&esperada)), "{nombre}: {todo:?}");
        // Lo que mete el agente (contexto, skills, estado de git) no sale como decisión.
        assert!(!todo.iter().any(|t| t.contains("environment_context") || t.contains("skill") || t.contains("git_status") || t.contains("Tengo dos col →")), "{nombre}: {todo:?}");
        if nombre == "codex" {
            assert!(todo.iter().any(|t| t == "[milestone] last commit: Portada azul"), "{todo:?}");
        }
        if nombre == "generico" {
            assert!(todo.iter().any(|t| t == "[milestone] published: tag v2.0.0"), "{todo:?}");
        }
    }
    // Gemini antes de junio de 2026: un JSON entero con `messages`.
    let ruta = d.join("gemini-viejo.json");
    std::fs::write(&ruta, json!({"sessionId": "s", "messages": [{"type": "gemini", "content": pregunta}, {"type": "user", "content": respuesta}]}).to_string()).unwrap();
    let cs = candidatas::de_conversacion(&ruta).unwrap();
    assert!(cs.iter().any(|c| c.text.ends_with(&esperada)), "{cs:?}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn saca_decisiones_preferencias_e_hitos_sin_secretos() {
    let d = dir("saca");
    let cs = candidatas::de_conversacion(&conversacion(&d)).unwrap();
    let textos: Vec<String> = cs.iter().map(|c| format!("[{}] {}", c.kind, c.text)).collect();
    let todo = textos.join("\n");
    // La preferencia primero; la decisión va con su pregunta; la respuesta de AskUserQuestion también.
    assert!(textos[0].starts_with("[preference]") && textos[0].contains("commits vayan en inglés"), "{todo}");
    assert!(todo.contains("¿Cuál te gusta más para la portada? → Me gusta la segunda, pero sin el fondo verde"), "{todo}");
    assert!(todo.contains("¿Qué tipografía usamos en los títulos? → una serif clásica"), "{todo}");
    assert!(todo.contains("[milestone] last commit: Portada nueva con la segunda opción"), "{todo}");
    // Nada trivial, nada con secretos, nada de lo que mete el sistema.
    assert!(!todo.contains("→ vale") && !todo.contains("ghp_") && !todo.contains("no cuenta"), "{todo}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn se_proponen_una_vez_en_la_sesion_siguiente_y_no_lo_ya_guardado() {
    let d = dir("propone");
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    // Ya guardado: no se vuelve a proponer.
    let ya = Nueva { project: "/web".into(), kind: Some(Tipo::Preference), title: "Commits en inglés".into(), content: "Prefiere que los commits vayan en inglés.".into(), ..Default::default() };
    memoria::guardar(&mut db, ya, None).unwrap();
    let n = candidatas::apartar(&db, "/web", "s1", &conversacion(&d)).unwrap();
    let todas = candidatas::todas(&db).unwrap();
    assert_eq!(n, todas.len());
    assert!(!todas.iter().any(|c| c.text.contains("commits vayan en inglés")), "{todas:?}");
    // Repetir (PreCompact y luego SessionEnd) no duplica.
    candidatas::apartar(&db, "/web", "s1", &conversacion(&d)).unwrap();
    assert_eq!(candidatas::todas(&db).unwrap().len(), n);
    // En la misma sesión no salen; en la siguiente, sí, y una sola vez.
    let e = |s: &str| Entrada { session_id: s.into(), ..Default::default() };
    let t = recall::al_empezar(&mut db, "/web", &e("s1")).unwrap().unwrap();
    assert!(!t.contains("possibly worth keeping"), "{t}");
    let t = recall::al_empezar(&mut db, "/web", &e("s2")).unwrap().unwrap();
    assert!(t.contains("possibly worth keeping") && t.contains("sin el fondo verde"), "{t}");
    let t = recall::al_empezar(&mut db, "/web", &e("s3")).unwrap().unwrap();
    assert!(!t.contains("possibly worth keeping"), "{t}");
    // Otro proyecto no las ve.
    drop(db);
    let _ = std::fs::remove_dir_all(d);
}
