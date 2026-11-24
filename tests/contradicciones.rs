//! Contradictions on hold and the reason for a review date (1.14), through the agent's tools and
//! what it receives at session start and on each message.

mod comun;

use omoikane::recall::{self, HookInput};
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

    fn id(&self, args: Value) -> String {
        let r = self.ok("remember", args);
        assert_eq!(r["saved"], true, "{r}");
        r["id"].as_str().unwrap().to_string()
    }

    fn project_id(&self) -> String {
        omoikane::proyecto::de(&self.project).unwrap()
    }

    fn on_start(&self, session: &str) -> String {
        let mut db = omoikane::db::abrir(&self.db).unwrap();
        recall::on_session_start(&mut db, &self.project_id(), &HookInput { session_id: session.into(), ..Default::default() }).unwrap().unwrap()
    }

    fn on_prompt(&self, session: &str, prompt: &str) -> String {
        let mut db = omoikane::db::abrir(&self.db).unwrap();
        let e = HookInput { session_id: session.into(), prompt: prompt.into(), ..Default::default() };
        recall::on_prompt(&mut db, &self.project_id(), &e, None).unwrap().unwrap_or_default()
    }
}

/// The contradicted one waits: it doesn't come on its own, it shows further down with its note, and it shows at
/// session start until the person says which one holds; then it stops waiting.
#[test]
fn the_contradicted_one_waits_for_the_person() {
    let p = Fixture::new("contradiccion-espera");
    // Other memories, so the pair's words are rare as in a real database.
    for (t, c) in [
        ("El logo va en rojo", "El logo de la tienda va en rojo corporativo."),
        ("Los tests usan TEMP corto", "Los tests necesitan una carpeta temporal de ruta corta en Windows."),
        ("Las copias se hacen de noche", "Las copias de seguridad se hacen cada noche."),
        ("El móvil de pruebas es un Pixel", "Para probar la app se usa un Pixel 7."),
        ("Las facturas van en PDF", "Las facturas se envían siempre en PDF firmado."),
        ("El blog usa Astro", "El blog se genera con Astro y se publica en Netlify."),
        ("La newsletter sale los martes", "La newsletter se manda los martes a primera hora."),
        ("Las fotos se guardan en WebP", "Las fotos del catálogo se guardan en WebP."),
        ("El dominio se renueva en marzo", "El dominio principal se renueva cada marzo."),
    ] {
        p.id(json!({ "kind": "fact", "title": t, "content": c, "force": true }));
    }
    let old = p.id(json!({ "kind": "fact", "title": "La API de pagos escucha en el puerto 8080", "content": "La API de pagos escucha en el puerto 8080 del servidor de pagos." }));
    let new = p.id(json!({ "kind": "fact", "title": "La API de pagos escucha en el puerto 9000", "content": "La API de pagos escucha en el puerto 9000 del servidor de pagos.", "force": true, "contradicts": [old] }));

    // In `search`: the contradicting one first; the contradicted one, with the note saying it is on hold.
    let r = p.ok("search", json!({ "query": "puerto de la API de pagos" }));
    let list = r.as_array().unwrap();
    assert_eq!(list[0]["id"].as_str(), Some(new.as_str()), "{r}");
    let v = list.iter().find(|m| m["id"] == old.as_str()).expect("la contradicha sigue saliendo");
    assert!(v["note"].as_str().unwrap_or_default().starts_with("ON HOLD"), "{v}");

    // The per-message recall doesn't bring it on its own; the one contradicting it does, with its note.
    let t = p.on_prompt("s1", "en qué puerto escucha la API de pagos del servidor");
    assert!(t.contains(&new[..12]) && !t.contains(&format!("(id {})", &old[..12])), "{t}");
    assert!(t.contains("on hold until the user says which holds"), "{t}");

    // At start: the pair, to ask about.
    let t = p.on_start("s2");
    assert!(t.contains("Contradictions waiting for the user"), "{t}");
    assert!(t.contains(&format!("vs ON HOLD «La API de pagos escucha en el puerto 8080» (id {})", &old[..12])), "{t}");

    // Without the person's words nothing is decided; with an id that is not from the pair, neither.
    let (e, error) = p.call("decisions", json!({ "contradiction": [new, old], "keep": new }));
    assert!(error, "{e}");
    let other = p.id(json!({ "kind": "fact", "title": "Algo que no tiene que ver", "content": "El tema del logo va en rojo.", "force": true }));
    let (e, error) = p.call("decisions", json!({ "contradiction": [new, old], "keep": other, "answer": "la del 9000" }));
    assert!(error, "{e}");

    let r = p.ok("decisions", json!({ "contradiction": [new, old], "keep": new, "answer": "Marc: «es el 9000 desde la migración»" }));
    assert_eq!((r["resolved"].as_bool(), r["superseded"].as_str()), (Some(true), Some(old.as_str())), "{r}");

    // No longer waiting: it shows as superseded (with the person's words) and not at start.
    let r = p.ok("search", json!({ "query": "puerto de la API de pagos" }));
    let v = r.as_array().unwrap().iter().find(|m| m["id"] == old.as_str()).expect("sigue viva");
    let note = v["note"].as_str().unwrap_or_default();
    assert!(note.starts_with("superseded by") && note.contains("es el 9000 desde la migración"), "{v}");
    let t = p.on_start("s3");
    assert!(!t.contains("Contradictions waiting for the user"), "{t}");
    let t = p.on_prompt("s4", "en qué puerto escucha la API de pagos del servidor");
    assert!(!t.contains("ON HOLD"), "{t}");
}

/// «Both hold» clears the warning without superseding either.
#[test]
fn both_hold() {
    let p = Fixture::new("contradiccion-dos");
    let a = p.id(json!({ "kind": "decision", "title": "Los informes se mandan los lunes", "content": "Los informes semanales se mandan los lunes por la mañana." }));
    let b = p.id(json!({ "kind": "decision", "title": "Los informes se mandan los viernes", "content": "Los informes mensuales se mandan los viernes.", "force": true, "contradicts": [a] }));
    let r = p.ok("decisions", json!({ "contradiction": [a, b], "keep": "both", "answer": "uno es el semanal y otro el mensual" }));
    assert_eq!((r["kept"].as_str(), r["superseded"].is_null()), (Some("both"), true), "{r}");
    let r = p.ok("search", json!({ "query": "cuándo se mandan los informes" }));
    assert!(r.as_array().unwrap().iter().all(|m| m["note"].is_null()), "{r}");
    let t = p.on_start("s1");
    assert!(!t.contains("Contradictions waiting"), "{t}");
}

/// A review date asks what to check that day; when it arrives, it shows at start with that reason.
#[test]
fn review_date_asks_for_reason_and_shows_at_start() {
    let p = Fixture::new("revision-motivo");
    let (e, error) = p.call("remember", json!({ "kind": "fact", "title": "Token de la API de mapas", "content": "El token de la API de mapas es del plan de prueba.", "tags": ["review:2020-01-15"] }));
    assert!(error && e.as_str().unwrap_or_default().contains("review_reason"), "{e}");
    let (e, error) = p.call("remember", json!({ "kind": "fact", "title": "Sin fecha", "content": "Algo sin fecha.", "review_reason": "nada" }));
    assert!(!error, "sin fecha, el motivo no se guarda pero no estorba: {e}");

    let id = p.id(json!({ "kind": "fact", "title": "Token de la API de mapas", "content": "El token de la API de mapas es del plan de prueba.", "tags": ["review:2020-01-15"], "review_reason": "el plan de prueba caduca: mira si ya hay que pagarlo" }));
    // Saved before its date (a memory touched after the date already counts as reviewed).
    let db = omoikane::db::abrir(&p.db).unwrap();
    db.execute("UPDATE memory SET created_at = '2019-12-01T10:00:00.000Z', updated_at = '2019-12-01T10:00:00.000Z' WHERE id = ?1", [&id]).unwrap();
    drop(db);
    let g = p.ok("get", json!({ "id": id }));
    assert_eq!(g["review_reason"], "el plan de prueba caduca: mira si ya hay que pagarlo", "{g}");

    let t = p.on_start("s1");
    assert!(t.contains("Review dates reached"), "{t}");
    assert!(t.contains(&format!("Token de la API de mapas (id {}): 2020-01-15, el plan de prueba caduca", &id[..12])), "{t}");

    // Changing the date asks for a reason again; changing only the reason doesn't create a version.
    let (e, error) = p.call("update", json!({ "id": id, "tags": ["review:2099-01-01"] }));
    assert!(error, "{e}");
    let u = p.ok("update", json!({ "id": id, "review_reason": "si el proveedor sigue dando plan gratis" }));
    assert_eq!(u["id"], id.as_str(), "{u}");
    let h = p.ok("history", json!({ "id": id }));
    assert_eq!(h.as_array().map(Vec::len), Some(0), "sin versión nueva: {h}");
    let g = p.ok("get", json!({ "id": id }));
    assert_eq!(g["review_reason"], "si el proveedor sigue dando plan gratis", "{g}");
    p.ok("update", json!({ "id": id, "tags": ["review:2099-01-01"], "review_reason": "la renovación anual" }));
    let t = p.on_start("s2");
    assert!(!t.contains("Review dates reached"), "la fecha nueva aún no ha llegado: {t}");

    // A reason without a date makes no sense.
    let undated = p.id(json!({ "kind": "fact", "title": "Otra sin fecha", "content": "Otra cosa sin fecha distinta." , "force": true }));
    let (e, error) = p.call("update", json!({ "id": undated, "review_reason": "x" }));
    assert!(error, "{e}");
}
