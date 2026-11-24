//! 1.5: proposing recipes (the `procedure` ones saved by hand are in `recetas.rs`). A sequence of commands that worked and repeats in `MIN_SESSIONS`
//! sessions comes out as a candidate once; one already covered by a saved recipe, no.

mod comun;

use omoikane::memoria::{self, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::{candidatas, db, recetas};
use serde_json::json;
use std::path::PathBuf;

fn dir(name: &str) -> comun::Temporal {
    comun::Temporal::nueva(&format!("recetas-{name}"))
}

/// A Claude Code conversation with these commands, all successful.
fn conversation(d: &std::path::Path, name: &str, commands: &[&str]) -> PathBuf {
    let mut lines = vec![json!({"type": "user", "message": {"role": "user", "content": "vamos"}})];
    for (i, o) in commands.iter().enumerate() {
        let id = format!("t{i}");
        lines.push(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": o}}]}}));
        lines.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}}));
    }
    // One that fails doesn't count.
    lines.push(json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "f", "name": "Bash", "input": {"command": "cargo bench"}}]}}));
    lines.push(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "f", "content": "Exit code 101", "is_error": true}]}}));
    let path = d.join(format!("{name}.jsonl"));
    std::fs::write(&path, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    path
}

#[test]
fn proposes_once_what_repeats_and_not_what_is_already_saved() {
    let d = dir("propone");
    let mut db = db::open(&d.join("o.db")).unwrap();
    let review_prs = ["git fetch origin", "git status --short", "gh issue list -R x/y", "gh pr view 3 -R x/y --json state"];
    for i in 0..recetas::MIN_SESSIONS - 1 {
        let c = conversation(&d, &format!("s{i}"), &review_prs);
        recetas::record(&db, &format!("s{i}"), &recetas::steps(&c).unwrap()).unwrap();
        assert!(!recetas::propose(&db, "/web", &format!("s{i}"), &recetas::steps(&c).unwrap()).unwrap(), "con {} sesiones aún no", i + 1);
    }
    let c = conversation(&d, "ultima", &review_prs);
    let steps = recetas::steps(&c).unwrap();
    assert_eq!(steps, ["git fetch", "gh issue", "gh pr"], "sin lo que solo mira ni lo que falló");
    recetas::record(&db, "ultima", &steps).unwrap();
    assert!(recetas::propose(&db, "/web", "ultima", &steps).unwrap());
    // Once: the next session that ends doesn't propose it again.
    recetas::record(&db, "otra", &steps).unwrap();
    assert!(!recetas::propose(&db, "/web", "otra", &steps).unwrap());
    let all = candidatas::all(&db).unwrap();
    let r: Vec<_> = all.iter().filter(|c| c.kind == recetas::RECIPE).collect();
    assert_eq!(r.len(), 1, "{all:?}");
    assert!(r[0].text.contains("`git fetch` → `gh issue` → `gh pr`") && r[0].text.contains(&format!("{} sessions", recetas::MIN_SESSIONS)), "{}", r[0].text);
    // It reaches the next session, like the other candidates.
    let t = recall::on_session_start(&mut db, "/web", &HookInput { session_id: "nueva".into(), ..Default::default() }).unwrap().unwrap();
    assert!(t.contains("[recipe] steps repeated"), "{t}");

    // Another sequence, already covered by a saved recipe: not proposed.
    let publish = ["cargo test -q", "git tag -a v1 -m 1", "git push origin v1", "cargo publish"];
    let mut db2 = db::open(&d.join("o2.db")).unwrap();
    let recipe = NewMemory {
        project: "/web".into(),
        kind: Some(Kind::Procedure),
        title: "Publicar una versión".into(),
        content: "1. cargo test.\n2. git tag -a y git push del tag.\n3. cargo publish.\n\nwhen: publica".into(),
        ..Default::default()
    };
    memoria::save(&mut db2, recipe, None).unwrap();
    for i in 0..recetas::MIN_SESSIONS {
        let c = conversation(&d, &format!("p{i}"), &publish);
        recetas::record(&db2, &format!("p{i}"), &recetas::steps(&c).unwrap()).unwrap();
    }
    assert!(!recetas::propose(&db2, "/web", "p", &recetas::steps(&conversation(&d, "p0", &publish)).unwrap()).unwrap(), "la cubre la receta guardada: {:?}", candidatas::all(&db2).unwrap());
}
