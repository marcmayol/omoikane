//! En qué se basa una memoria (`evidencia`, 1.13), por las herramientas que usa el agente.

mod comun;

use serde_json::{Value, json};
use std::path::PathBuf;

struct Prueba {
    _dir: comun::Temporal,
    db: PathBuf,
    proyecto: PathBuf,
}

impl Prueba {
    fn nueva(nombre: &str) -> Prueba {
        let dir = comun::Temporal::nueva(nombre);
        let proyecto = dir.join("proy");
        std::fs::create_dir_all(proyecto.join(".git")).unwrap();
        let db = dir.join("o.db");
        Prueba { _dir: dir, db, proyecto }
    }

    fn llamar(&self, nombre: &str, args: Value) -> (Value, bool) {
        // Un tipo desconocido no pasa ni de los argumentos: también es un error.
        let (t, error) = match omoikane::mcp::herramienta(self.db.clone(), Some(self.proyecto.clone()), Some(nombre), &args.to_string(), "test") {
            Ok(r) => r,
            Err(e) => (format!("{e:#}"), true),
        };
        (serde_json::from_str(&t).unwrap_or(Value::String(t)), error)
    }

    fn ok(&self, nombre: &str, args: Value) -> Value {
        let (v, error) = self.llamar(nombre, args);
        assert!(!error, "{nombre}: {v}");
        v
    }
}

/// Lo que solo dedujo el agente sale marcado y por detrás de lo mismo comprobado; `update` le añade
/// evidencia sin versión nueva, y entonces deja de estar marcada.
#[test]
fn lo_deducido_pesa_menos_hasta_que_se_comprueba() {
    let p = Prueba::nueva("evidencia-mcp");
    let deducida = p.ok(
        "remember",
        json!({ "kind": "fact", "title": "El índice de pagos se recalcula de noche", "content": "El índice de pagos se recalcula cada noche a las tres.", "evidence": [{ "kind": "inferred", "detail": "por el nombre del cron, recalcular_nocturno" }] }),
    );
    assert_eq!(deducida["based_on"], "inferred", "{deducida}");
    let probada = p.ok(
        "remember",
        json!({ "kind": "fact", "title": "El índice de pedidos se recalcula de noche", "content": "El índice de pedidos se recalcula cada noche a las tres.", "force": true,
                "evidence": [{ "kind": "tested", "detail": "omoi-cron --list: recalcular a las 03:00" }, { "kind": "user-said", "detail": "«eso corre de noche»" }] }),
    );
    assert_eq!(probada["based_on"], "tested, user-said", "{probada}");

    let r = p.ok("search", json!({ "query": "índice se recalcula cada noche" }));
    let ids: Vec<&str> = r.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids.first().copied(), probada["id"].as_str(), "la comprobada primero: {r}");

    let g = p.ok("get", json!({ "id": deducida["id"] }));
    assert_eq!(g["evidence"][0]["kind"], "inferred", "{g}");

    // Comprobada después: se añade, sin versión nueva.
    let u = p.ok("update", json!({ "id": deducida["id"], "evidence": [{ "kind": "tested", "detail": "lo vi en el log de las 03:00" }] }));
    assert_eq!(u["based_on"], "inferred, tested", "{u}");
    let h = p.ok("history", json!({ "id": deducida["id"] }));
    assert_eq!(h.as_array().map(Vec::len), Some(0), "sin versión nueva: {h}");

    // Sin detalle no es evidencia.
    let (e, error) = p.llamar("remember", json!({ "kind": "fact", "title": "Otra", "content": "Algo distinto.", "evidence": [{ "kind": "doc", "detail": " " }] }));
    assert!(error, "{e}");
    let (e, error) = p.llamar("remember", json!({ "kind": "fact", "title": "Otra", "content": "Algo distinto.", "evidence": [{ "kind": "guess", "detail": "x" }] }));
    assert!(error, "{e}");
}

/// Una memoria de oficio no lleva evidencia: su detalle (un fichero, una cita) sería del proyecto.
#[test]
fn las_de_oficio_sin_evidencia() {
    let p = Prueba::nueva("evidencia-oficio");
    let m = p.ok(
        "remember",
        json!({ "kind": "gotcha", "title": "No matar el emulador con taskkill", "content": "Se corrompe la imagen del AVD; usa adb emu kill.", "trade": "android",
                "evidence": [{ "kind": "tested", "detail": "en C:/clientes/acme/app pasó" }] }),
    );
    assert!(m.get("based_on").is_none(), "{m}");
}
