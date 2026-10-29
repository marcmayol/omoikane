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
