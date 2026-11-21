//! Contradicciones en espera y motivo de la fecha de revisión (1.14), por las herramientas del agente y
//! lo que recibe al empezar la sesión y en cada mensaje.

mod comun;

use omoikane::recall::{self, Entrada};
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

    fn id(&self, args: Value) -> String {
        let r = self.ok("remember", args);
        assert_eq!(r["saved"], true, "{r}");
        r["id"].as_str().unwrap().to_string()
    }

    fn proyecto(&self) -> String {
        omoikane::proyecto::de(&self.proyecto).unwrap()
    }

    fn al_empezar(&self, sesion: &str) -> String {
        let mut db = omoikane::db::abrir(&self.db).unwrap();
        recall::al_empezar(&mut db, &self.proyecto(), &Entrada { session_id: sesion.into(), ..Default::default() }).unwrap().unwrap()
    }

    fn al_preguntar(&self, sesion: &str, prompt: &str) -> String {
        let mut db = omoikane::db::abrir(&self.db).unwrap();
        let e = Entrada { session_id: sesion.into(), prompt: prompt.into(), ..Default::default() };
        recall::al_preguntar(&mut db, &self.proyecto(), &e, None).unwrap().unwrap_or_default()
    }
}

/// La contradicha espera: no llega sola, sale más abajo y con su nota, y sale al empezar la sesión
/// hasta que la persona dice cuál vale; entonces deja de esperar.
#[test]
fn la_contradicha_espera_a_la_persona() {
    let p = Prueba::nueva("contradiccion-espera");
    // Otras memorias, para que las palabras de la pareja sean raras como en una base de verdad.
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
    let vieja = p.id(json!({ "kind": "fact", "title": "La API de pagos escucha en el puerto 8080", "content": "La API de pagos escucha en el puerto 8080 del servidor de pagos." }));
    let nueva = p.id(json!({ "kind": "fact", "title": "La API de pagos escucha en el puerto 9000", "content": "La API de pagos escucha en el puerto 9000 del servidor de pagos.", "force": true, "contradicts": [vieja] }));

    // En `search`: la que contradice primero; la contradicha, con la nota de que espera.
    let r = p.ok("search", json!({ "query": "puerto de la API de pagos" }));
    let lista = r.as_array().unwrap();
    assert_eq!(lista[0]["id"].as_str(), Some(nueva.as_str()), "{r}");
    let v = lista.iter().find(|m| m["id"] == vieja.as_str()).expect("la contradicha sigue saliendo");
    assert!(v["note"].as_str().unwrap_or_default().starts_with("ON HOLD"), "{v}");

    // El recall de cada mensaje no la trae sola; la que la contradice sí, con su nota.
    let t = p.al_preguntar("s1", "en qué puerto escucha la API de pagos del servidor");
    assert!(t.contains(&nueva[..12]) && !t.contains(&format!("(id {})", &vieja[..12])), "{t}");
    assert!(t.contains("on hold until the user says which holds"), "{t}");

    // Al empezar: la pareja, para preguntar.
    let t = p.al_empezar("s2");
    assert!(t.contains("Contradictions waiting for the user"), "{t}");
    assert!(t.contains(&format!("vs ON HOLD «La API de pagos escucha en el puerto 8080» (id {})", &vieja[..12])), "{t}");

    // Sin las palabras de la persona no se decide; con un id que no es de la pareja, tampoco.
    let (e, error) = p.llamar("decisions", json!({ "contradiction": [nueva, vieja], "keep": nueva }));
    assert!(error, "{e}");
    let otra = p.id(json!({ "kind": "fact", "title": "Algo que no tiene que ver", "content": "El tema del logo va en rojo.", "force": true }));
    let (e, error) = p.llamar("decisions", json!({ "contradiction": [nueva, vieja], "keep": otra, "answer": "la del 9000" }));
    assert!(error, "{e}");

    let r = p.ok("decisions", json!({ "contradiction": [nueva, vieja], "keep": nueva, "answer": "Marc: «es el 9000 desde la migración»" }));
    assert_eq!((r["resolved"].as_bool(), r["superseded"].as_str()), (Some(true), Some(vieja.as_str())), "{r}");

    // Ya no espera: sale sustituida (con las palabras de la persona) y no al empezar.
    let r = p.ok("search", json!({ "query": "puerto de la API de pagos" }));
    let v = r.as_array().unwrap().iter().find(|m| m["id"] == vieja.as_str()).expect("sigue viva");
    let nota = v["note"].as_str().unwrap_or_default();
    assert!(nota.starts_with("superseded by") && nota.contains("es el 9000 desde la migración"), "{v}");
    let t = p.al_empezar("s3");
    assert!(!t.contains("Contradictions waiting for the user"), "{t}");
    let t = p.al_preguntar("s4", "en qué puerto escucha la API de pagos del servidor");
    assert!(!t.contains("ON HOLD"), "{t}");
}

/// «Valen las dos» quita el aviso sin sustituir ninguna.
#[test]
fn valen_las_dos() {
    let p = Prueba::nueva("contradiccion-dos");
    let a = p.id(json!({ "kind": "decision", "title": "Los informes se mandan los lunes", "content": "Los informes semanales se mandan los lunes por la mañana." }));
    let b = p.id(json!({ "kind": "decision", "title": "Los informes se mandan los viernes", "content": "Los informes mensuales se mandan los viernes.", "force": true, "contradicts": [a] }));
    let r = p.ok("decisions", json!({ "contradiction": [a, b], "keep": "both", "answer": "uno es el semanal y otro el mensual" }));
    assert_eq!((r["kept"].as_str(), r["superseded"].is_null()), (Some("both"), true), "{r}");
    let r = p.ok("search", json!({ "query": "cuándo se mandan los informes" }));
    assert!(r.as_array().unwrap().iter().all(|m| m["note"].is_null()), "{r}");
    let t = p.al_empezar("s1");
    assert!(!t.contains("Contradictions waiting"), "{t}");
}

/// Una fecha de revisión pide qué mirar ese día; cuando llega, sale al empezar con ese motivo.
#[test]
fn la_fecha_de_revision_pide_motivo_y_sale_al_empezar() {
    let p = Prueba::nueva("revision-motivo");
    let (e, error) = p.llamar("remember", json!({ "kind": "fact", "title": "Token de la API de mapas", "content": "El token de la API de mapas es del plan de prueba.", "tags": ["review:2020-01-15"] }));
    assert!(error && e.as_str().unwrap_or_default().contains("review_reason"), "{e}");
    let (e, error) = p.llamar("remember", json!({ "kind": "fact", "title": "Sin fecha", "content": "Algo sin fecha.", "review_reason": "nada" }));
    assert!(!error, "sin fecha, el motivo no se guarda pero no estorba: {e}");

    let id = p.id(json!({ "kind": "fact", "title": "Token de la API de mapas", "content": "El token de la API de mapas es del plan de prueba.", "tags": ["review:2020-01-15"], "review_reason": "el plan de prueba caduca: mira si ya hay que pagarlo" }));
    // Guardada antes de su fecha (una memoria tocada después de la fecha ya cuenta como revisada).
    let db = omoikane::db::abrir(&p.db).unwrap();
    db.execute("UPDATE memory SET created_at = '2019-12-01T10:00:00.000Z', updated_at = '2019-12-01T10:00:00.000Z' WHERE id = ?1", [&id]).unwrap();
    drop(db);
    let g = p.ok("get", json!({ "id": id }));
    assert_eq!(g["review_reason"], "el plan de prueba caduca: mira si ya hay que pagarlo", "{g}");

    let t = p.al_empezar("s1");
    assert!(t.contains("Review dates reached"), "{t}");
    assert!(t.contains(&format!("Token de la API de mapas (id {}): 2020-01-15, el plan de prueba caduca", &id[..12])), "{t}");

    // Cambiar la fecha pide motivo otra vez; cambiar solo el motivo no crea versión.
    let (e, error) = p.llamar("update", json!({ "id": id, "tags": ["review:2099-01-01"] }));
    assert!(error, "{e}");
    let u = p.ok("update", json!({ "id": id, "review_reason": "si el proveedor sigue dando plan gratis" }));
    assert_eq!(u["id"], id.as_str(), "{u}");
    let h = p.ok("history", json!({ "id": id }));
    assert_eq!(h.as_array().map(Vec::len), Some(0), "sin versión nueva: {h}");
    let g = p.ok("get", json!({ "id": id }));
    assert_eq!(g["review_reason"], "si el proveedor sigue dando plan gratis", "{g}");
    p.ok("update", json!({ "id": id, "tags": ["review:2099-01-01"], "review_reason": "la renovación anual" }));
    let t = p.al_empezar("s2");
    assert!(!t.contains("Review dates reached"), "la fecha nueva aún no ha llegado: {t}");

    // Un motivo sin fecha no tiene sentido.
    let sin = p.id(json!({ "kind": "fact", "title": "Otra sin fecha", "content": "Otra cosa sin fecha distinta." , "force": true }));
    let (e, error) = p.llamar("update", json!({ "id": sin, "review_reason": "x" }));
    assert!(error, "{e}");
}
