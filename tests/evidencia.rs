//! What a memory is based on (`evidencia`, 1.13), through the tools the agent uses.

mod comun;

use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    _dir: comun::Temporal,
    db: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = comun::Temporal::nueva(name);
        let project = dir.join("proy");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let db = dir.join("o.db");
        Fixture { _dir: dir, db, project }
    }

    fn call(&self, name: &str, args: Value) -> (Value, bool) {
        // An unknown kind doesn't even get past the arguments: that is an error too.
        let (t, error) = match omoikane::mcp::herramienta(self.db.clone(), Some(self.project.clone()), Some(name), &args.to_string(), "test") {
            Ok(r) => r,
            Err(e) => (format!("{e:#}"), true),
        };
        (serde_json::from_str(&t).unwrap_or(Value::String(t)), error)
    }

    fn ok(&self, name: &str, args: Value) -> Value {
        let (v, error) = self.call(name, args);
        assert!(!error, "{name}: {v}");
        v
    }
}

/// What the agent only inferred comes out marked and behind the same thing verified; `update` adds
/// evidence to it without a new version, and then it is no longer marked.
#[test]
fn inferred_weighs_less_until_verified() {
    let p = Fixture::new("evidencia-mcp");
    let inferred = p.ok(
        "remember",
        json!({ "kind": "fact", "title": "El índice de pagos se recalcula de noche", "content": "El índice de pagos se recalcula cada noche a las tres.", "evidence": [{ "kind": "inferred", "detail": "por el nombre del cron, recalcular_nocturno" }] }),
    );
    assert_eq!(inferred["based_on"], "inferred", "{inferred}");
    let tested = p.ok(
        "remember",
        json!({ "kind": "fact", "title": "El índice de pedidos se recalcula de noche", "content": "El índice de pedidos se recalcula cada noche a las tres.", "force": true,
                "evidence": [{ "kind": "tested", "detail": "omoi-cron --list: recalcular a las 03:00" }, { "kind": "user-said", "detail": "«eso corre de noche»" }] }),
    );
    assert_eq!(tested["based_on"], "tested, user-said", "{tested}");

    let r = p.ok("search", json!({ "query": "índice se recalcula cada noche" }));
    let ids: Vec<&str> = r.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids.first().copied(), tested["id"].as_str(), "la comprobada primero: {r}");

    let g = p.ok("get", json!({ "id": inferred["id"] }));
    assert_eq!(g["evidence"][0]["kind"], "inferred", "{g}");

    // Verified later: it is added, without a new version.
    let u = p.ok("update", json!({ "id": inferred["id"], "evidence": [{ "kind": "tested", "detail": "lo vi en el log de las 03:00" }] }));
    assert_eq!(u["based_on"], "inferred, tested", "{u}");
    let h = p.ok("history", json!({ "id": inferred["id"] }));
    assert_eq!(h.as_array().map(Vec::len), Some(0), "sin versión nueva: {h}");

    // Without detail it is not evidence.
    let (e, error) = p.call("remember", json!({ "kind": "fact", "title": "Otra", "content": "Algo distinto.", "evidence": [{ "kind": "doc", "detail": " " }] }));
    assert!(error, "{e}");
    let (e, error) = p.call("remember", json!({ "kind": "fact", "title": "Otra", "content": "Algo distinto.", "evidence": [{ "kind": "guess", "detail": "x" }] }));
    assert!(error, "{e}");
}

/// A trade memory carries no evidence: its detail (a file, a quote) would belong to the project.
#[test]
fn trade_memories_without_evidence() {
    let p = Fixture::new("evidencia-oficio");
    let m = p.ok(
        "remember",
        json!({ "kind": "gotcha", "title": "No matar el emulador con taskkill", "content": "Se corrompe la imagen del AVD; usa adb emu kill.", "trade": "android",
                "evidence": [{ "kind": "tested", "detail": "en C:/clientes/acme/app pasó" }] }),
    );
    assert!(m.get("based_on").is_none(), "{m}");
}
