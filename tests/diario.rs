//! 1.4: el diario de sesiones. Una línea por sesión y proyecto, sacada de la conversación (`.jsonl`
//! inventados con la forma de los de verdad) y de lo que la base sabe (`recall_log`).

use omoikane::db;
use omoikane::diario::{self, Filtro};
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use serde_json::json;
use std::path::{Path, PathBuf};

fn dir(nombre: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("omoi-diario-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    std::fs::create_dir_all(d.join("repo").join("src")).unwrap();
    d
}

fn escribir(ruta: &Path, lineas: &[serde_json::Value]) {
    std::fs::write(ruta, lineas.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
}

fn herramienta(id: &str, nombre: &str, entrada: serde_json::Value) -> serde_json::Value {
    json!({"parentUuid": null, "type": "assistant", "timestamp": "2026-11-03T09:00:00.000Z", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": nombre, "input": entrada}]}})
}
fn resultado(id: &str, salida: &str) -> serde_json::Value {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": [{"type": "text", "text": salida}]}]}})
}

fn guardar(db: &mut rusqlite::Connection, project: &str, titulo: &str, pinned: bool) -> String {
    let n = Nueva { project: project.into(), kind: Some(Tipo::Fact), title: titulo.into(), content: format!("{titulo}, con detalle."), pinned, ..Default::default() };
    memoria::guardar(db, n, None).unwrap().id
}

fn recibida(db: &rusqlite::Connection, sesion: &str, id: &str) {
    db.execute("INSERT INTO recall_log(session, memory_id, at, project_id) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), 0)", [sesion, id]).unwrap();
}

#[test]
fn una_linea_por_sesion_con_ficheros_y_memorias() {
    let d = dir("claude");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let nueva = guardar(&mut db, &repo, "El puerto es el 8010", false);
    let cambiada = guardar(&mut db, &repo, "Sync por git", false);
    let usada = guardar(&mut db, &repo, "La caché vive en D:", false);
    let fijada = guardar(&mut db, &repo, "Siempre en español", true);
    let duplicada = guardar(&mut db, &repo, "Ya estaba", false);
    recibida(&db, "s1", &usada);
    recibida(&db, "s1", &fijada);
    let fuera = d.join("otro").join("notas.md");
    let lineas = vec![
        json!({"type": "permission-mode", "permissionMode": "default"}),
        json!({"type": "user", "timestamp": "2026-11-03T08:59:00.000Z", "message": {"role": "user", "content": "vamos"}}),
        herramienta("t1", "Edit", json!({"file_path": format!("{repo}/src/sync.rs"), "old_string": "a", "new_string": "b"})),
        resultado("t1", "ok"),
        herramienta("t2", "Write", json!({"file_path": fuera.to_string_lossy()})),
        herramienta("t3", "MultiEdit", json!({"file_path": format!("{repo}/src/db.rs"), "edits": []})),
        herramienta("t4", "NotebookEdit", json!({"notebook_path": format!("{repo}/notas.ipynb")})),
        herramienta("t5", "Read", json!({"file_path": format!("{repo}/src/leido.rs")})),
        herramienta("t6", "Edit", json!({"file_path": format!("{repo}/src/sync.rs")})),
        // Lo que guarda y cambia el agente con las herramientas de Omoikane (con su id en la respuesta).
        herramienta("t7", "mcp__omoikane__remember", json!({"kind": "fact", "title": "El puerto es el 8010", "content": "…"})),
        resultado("t7", &serde_json::to_string_pretty(&json!({"saved": true, "id": nueva, "project": repo, "related": [{"id": usada, "title": "x"}]})).unwrap()),
        herramienta("t8", "mcp__omoikane__update", json!({"id": &cambiada[..10], "append": "y más"})),
        herramienta("t9", "mcp__omoikane__remember", json!({"kind": "fact", "title": "Ya estaba", "content": "…"})),
        resultado("t9", &json!({"saved": false, "duplicate_of": {"id": duplicada}}).to_string()),
        // Otra herramienta que se llama `update` pero no es de Omoikane, y un id que no existe aquí.
        herramienta("t10", "mcp__claude_ai_Docs__update", json!({"id": usada})),
        resultado("t10", &json!({"id": usada}).to_string()),
        herramienta("t11", "remember", json!({})),
        resultado("t11", &json!({"saved": true, "id": "01AAAAAAAAAAAAAAAAAAAAAAAA"}).to_string()),
        // Un `update` que falló: su respuesta no trae la memoria, así que no cuenta.
        herramienta("t13", "mcp__omoikane__update", json!({"id": duplicada, "title": "x"})),
        resultado("t13", "that is in another space"),
    ];
    let conv = d.join("s1.jsonl");
    escribir(&conv, &lineas);
    assert!(diario::apuntar(&db, &repo, "s1", Some(&conv)).unwrap());
    let v = diario::lineas(&db, &Ambito::de(&db, &repo).unwrap(), &Filtro { limite: 10, ..Default::default() }).unwrap();
    assert_eq!(v.len(), 1, "{v:?}");
    let l = &v[0];
    let fuera = fuera.to_string_lossy().replace('\\', "/");
    assert_eq!(l.files, vec!["src/sync.rs".to_string(), fuera, "src/db.rs".into(), "notas.ipynb".into()]);
    assert_eq!(l.first_at, "2026-11-03T08:59:00.000Z");
    // Recibida: la que llegó por el mensaje; la fijada llega en todas las sesiones y no cuenta.
    assert_eq!(l.recalled.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![usada.as_str()]);
    assert_eq!(l.recalled[0].title.as_deref(), Some("La caché vive en D:"));
    // Guardadas: la nueva y la cambiada (por el principio de su id); no la duplicada ni lo ajeno.
    assert_eq!(l.saved.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![nueva.as_str(), cambiada.as_str()]);

    // Tras compactar, `recall_log` de la sesión se vacía y llega más: la línea se completa, no se pierde.
    db.execute("DELETE FROM recall_log WHERE session = 's1'", []).unwrap();
    let otra = guardar(&mut db, &repo, "Otra recibida", false);
    recibida(&db, "s1", &otra);
    let mut mas = lineas.clone();
    mas.push(herramienta("t12", "Write", json!({"file_path": format!("{repo}/README.md")})));
    escribir(&conv, &mas);
    diario::apuntar(&db, &repo, "s1", Some(&conv)).unwrap();
    let l = &diario::lineas(&db, &Ambito::Todos, &Filtro { limite: 10, ..Default::default() }).unwrap()[0];
    assert_eq!(l.files.last().map(String::as_str), Some("README.md"));
    assert_eq!(l.files.len(), 5);
    assert_eq!(l.recalled.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![usada.as_str(), otra.as_str()]);
    assert_eq!(l.saved.len(), 2);
    assert_eq!(db.query_row("SELECT count(*) FROM diary", [], |f| f.get::<_, i64>(0)).unwrap(), 1);
    // Sin conversación (el agente no la da): queda lo que sabe la base.
    assert!(diario::apuntar(&db, &repo, "s2", None).unwrap());
    assert!(!diario::apuntar(&db, &repo, "", None).unwrap());
    drop(db);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn ficheros_de_otros_agentes() {
    let d = dir("agentes");
    let raiz = "/w/repo";
    let parche = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch";
    let formatos: Vec<(&str, Vec<serde_json::Value>, Vec<&str>)> = vec![
        ("codex", vec![
            json!({"timestamp": "2026-11-03T10:00:00.000Z", "type": "session_meta", "payload": {"id": "s", "cwd": raiz}}),
            json!({"type": "response_item", "payload": {"type": "custom_tool_call", "name": "apply_patch", "call_id": "c1", "input": parche}}),
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
    for (nombre, lineas, esperados) in formatos {
        let ruta = d.join(format!("{nombre}.jsonl"));
        escribir(&ruta, &lineas);
        let l = diario::leer(&ruta, raiz).unwrap();
        assert_eq!(l.ficheros, esperados, "{nombre}");
        match nombre {
            "codex" => {
                assert_eq!(l.guardadas, vec!["01JCODEXCODEXCODEXCODEXCOD"]);
                assert_eq!(l.primera.as_deref(), Some("2026-11-03T10:00:00.000Z"));
            }
            "gemini" => assert_eq!(l.guardadas, vec!["01JGEMINIGEMINIGEMINIGEMIN"]),
            _ => assert!(l.guardadas.is_empty(), "{nombre}: {l:?}"),
        }
    }
    // Gemini antes de junio de 2026: un JSON entero con `messages`.
    let ruta = d.join("gemini-viejo.json");
    std::fs::write(&ruta, json!({"sessionId": "s", "startTime": "2026-11-01T07:00:00.000Z", "messages": [{"type": "gemini", "toolCalls": [{"name": "write_file", "args": {"file_path": "/w/repo/v.md"}}]}]}).to_string()).unwrap();
    let l = diario::leer(&ruta, raiz).unwrap();
    assert_eq!(l.ficheros, vec!["v.md"]);
    assert_eq!(l.primera.as_deref(), Some("2026-11-01T07:00:00.000Z"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn filtros_y_caducidad() {
    let d = dir("filtros");
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    let id = omoikane::proyecto::registrar(&db, &repo).unwrap();
    // Sin ninguna línea todavía la tabla no existe (no la crea abrir la base), y leer no falla.
    assert!(diario::lineas(&db, &Ambito::Todos, &Filtro { limite: 10, ..Default::default() }).unwrap().is_empty());
    let conv = d.join("c.jsonl");
    escribir(&conv, &[herramienta("t1", "Edit", json!({"file_path": format!("{repo}/src/sync.rs")}))]);
    diario::apuntar(&db, &repo, "hoy", Some(&conv)).unwrap();
    escribir(&conv, &[herramienta("t1", "Edit", json!({"file_path": format!("{repo}/src/db.rs")}))]);
    diario::apuntar(&db, &repo, "martes", Some(&conv)).unwrap();
    db.execute("UPDATE diary SET first_at = '2026-11-03T09:00:00.000Z', last_at = '2026-11-03T11:00:00.000Z' WHERE session = 'martes'", []).unwrap();
    // `hoy` empezó hoy (la hora de la conversación inventada es la del martes).
    db.execute("UPDATE diary SET first_at = last_at WHERE session = 'hoy'", []).unwrap();
    // En otra carpeta (otro proyecto sin registrar): solo se ve desde todos.
    diario::apuntar(&db, &d.join("otro").to_string_lossy(), "fuera", None).unwrap();
    let sesiones = |a: &Ambito, f: Filtro| diario::lineas(&db, a, &f).unwrap().into_iter().map(|l| l.session).collect::<Vec<_>>();
    let proyecto = Ambito::Proyecto { id, space: None };
    assert_eq!(sesiones(&proyecto, Filtro { limite: 10, ..Default::default() }), vec!["hoy", "martes"]);
    assert_eq!(sesiones(&Ambito::Todos, Filtro { limite: 10, ..Default::default() }).len(), 3);
    assert_eq!(sesiones(&proyecto, Filtro { limite: 1, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sesiones(&proyecto, Filtro { fichero: Some("SYNC.rs".into()), limite: 10, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sesiones(&proyecto, Filtro { fichero: Some("src\\db".into()), limite: 10, ..Default::default() }), vec!["martes"]);
    // El fragmento no vale a caballo entre dos ficheros.
    assert!(sesiones(&proyecto, Filtro { fichero: Some("rs\",\"".into()), limite: 10, ..Default::default() }).is_empty());
    let dia = |t: &str| Some(diario::fecha(&db, t).unwrap());
    assert_eq!(sesiones(&proyecto, Filtro { desde: dia("2026-11-03"), hasta: dia("2026-11-03"), limite: 10, ..Default::default() }), vec!["martes"]);
    assert_eq!(sesiones(&proyecto, Filtro { desde: dia("0d"), limite: 10, ..Default::default() }), vec!["hoy"]);
    assert_eq!(sesiones(&proyecto, Filtro { hasta: dia("2026-11-02"), limite: 10, ..Default::default() }), Vec::<String>::new());
    assert_eq!(sesiones(&proyecto, Filtro { hasta: dia("2026-11-03"), limite: 10, ..Default::default() }), vec!["martes"]);
    // Lo de hace más de un año se borra al escribir la siguiente.
    db.execute("UPDATE diary SET last_at = '2020-01-01T00:00:00.000Z' WHERE session = 'martes'", []).unwrap();
    diario::apuntar(&db, &repo, "hoy", None).unwrap();
    assert_eq!(sesiones(&proyecto, Filtro { limite: 10, ..Default::default() }), vec!["hoy"]);
    drop(db);
    let _ = std::fs::remove_dir_all(d);
}
