//! 1.5: proponer recetas (las de `procedure` guardadas a mano van en `recetas.rs`). Una secuencia de órdenes que funcionaron y se repite en `MIN_SESIONES`
//! sesiones sale como candidata una vez; la que ya cubre una receta guardada, no.

use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::{candidatas, db, recetas};
use serde_json::json;
use std::path::PathBuf;

fn dir(nombre: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("omoi-recetas-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Una conversación de Claude Code con estas órdenes, todas bien.
fn conversacion(d: &std::path::Path, nombre: &str, ordenes: &[&str]) -> PathBuf {
    let mut lineas = vec![json!({"type": "user", "message": {"role": "user", "content": "vamos"}})];
    for (i, o) in ordenes.iter().enumerate() {
        let id = format!("t{i}");
        lineas.push(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": o}}]}}));
        lineas.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}}));
    }
    // Una que falla no cuenta.
    lineas.push(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "f", "name": "Bash", "input": {"command": "cargo bench"}}]}}));
    lineas.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "f", "content": "Exit code 101", "is_error": true}]}}));
    let ruta = d.join(format!("{nombre}.jsonl"));
    std::fs::write(&ruta, lineas.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    ruta
}

#[test]
fn propone_una_vez_lo_que_se_repite_y_no_lo_ya_guardado() {
    let d = dir("propone");
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    let revisar_prs = ["git fetch origin", "git status --short", "gh issue list -R x/y", "gh pr view 3 -R x/y --json state"];
    for i in 0..recetas::MIN_SESIONES - 1 {
        let c = conversacion(&d, &format!("s{i}"), &revisar_prs);
        recetas::apuntar(&db, &format!("s{i}"), &recetas::pasos(&c).unwrap()).unwrap();
        assert!(!recetas::proponer(&db, "/web", &format!("s{i}"), &recetas::pasos(&c).unwrap()).unwrap(), "con {} sesiones aún no", i + 1);
    }
    let c = conversacion(&d, "ultima", &revisar_prs);
    let pasos = recetas::pasos(&c).unwrap();
    assert_eq!(pasos, ["git fetch", "gh issue", "gh pr"], "sin lo que solo mira ni lo que falló");
    recetas::apuntar(&db, "ultima", &pasos).unwrap();
    assert!(recetas::proponer(&db, "/web", "ultima", &pasos).unwrap());
    // Una vez: la siguiente sesión que acaba no la vuelve a proponer.
    recetas::apuntar(&db, "otra", &pasos).unwrap();
    assert!(!recetas::proponer(&db, "/web", "otra", &pasos).unwrap());
    let todas = candidatas::todas(&db).unwrap();
    let r: Vec<_> = todas.iter().filter(|c| c.kind == recetas::RECETA).collect();
    assert_eq!(r.len(), 1, "{todas:?}");
    assert!(r[0].text.contains("`git fetch` → `gh issue` → `gh pr`") && r[0].text.contains(&format!("{} sessions", recetas::MIN_SESIONES)), "{}", r[0].text);
    // Llega a la sesión siguiente, como las demás candidatas.
    let t = recall::al_empezar(&mut db, "/web", &Entrada { session_id: "nueva".into(), ..Default::default() }).unwrap().unwrap();
    assert!(t.contains("[recipe] steps repeated"), "{t}");

    // Otra secuencia, ya cubierta por una receta guardada: no se propone.
    let publicar = ["cargo test -q", "git tag -a v1 -m 1", "git push origin v1", "cargo publish"];
    let mut db2 = db::abrir(&d.join("o2.db")).unwrap();
    let receta = Nueva {
        project: "/web".into(),
        kind: Some(Tipo::Procedure),
        title: "Publicar una versión".into(),
        content: "1. cargo test.\n2. git tag -a y git push del tag.\n3. cargo publish.\n\nwhen: publica".into(),
        ..Default::default()
    };
    memoria::guardar(&mut db2, receta, None).unwrap();
    for i in 0..recetas::MIN_SESIONES {
        let c = conversacion(&d, &format!("p{i}"), &publicar);
        recetas::apuntar(&db2, &format!("p{i}"), &recetas::pasos(&c).unwrap()).unwrap();
    }
    assert!(!recetas::proponer(&db2, "/web", "p", &recetas::pasos(&conversacion(&d, "p0", &publicar)).unwrap()).unwrap(), "la cubre la receta guardada: {:?}", candidatas::todas(&db2).unwrap());
    drop((db, db2));
    let _ = std::fs::remove_dir_all(d);
}
