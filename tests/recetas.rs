//! Phase 7.3: recipes (`procedure`) that come back whole when the message asks for them.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, NewMemory, Kind};
use omoikane::recall::{self, HookInput};

const STEPS: &str = "1. Subir la versión en Cargo.toml y plugin.json.\n2. `git tag -a vX.Y.Z` y push del tag.\n3. `cargo publish`.\n4. En Windows, renombrar omoi.exe antes de instalar: está bloqueado.\n5. Actualizar el plugin.\n\nwhen: publica, publicar, release, saca versión";

#[test]
fn recipe_arrives_whole_when_asked_and_only_then() {
    let dir = comun::Temporal::nueva("recetas");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let n = NewMemory { project: "/p".into(), kind: Some(Kind::Procedure), title: "Publicar una versión de Lumbre".into(), content: STEPS.into(), ..Default::default() };
    let recipe = memoria::save(&mut db, n, None).unwrap();
    assert_eq!(recall::triggers(&recipe), ["publica", "publicar", "release", "saca version"]);
    let e = |s: &str, p: &str| HookInput { session_id: s.into(), prompt: p.into(), ..Default::default() };

    // Asked for: it arrives whole, with every step, even if the message shares no rare words.
    let t = recall::on_prompt(&mut db, "/p", &e("s1", "publica la 0.10.1"), None).unwrap().expect("la receta");
    assert!(t.contains("A saved procedure for this") && t.contains("5. Actualizar el plugin") && t.contains("renombrar omoi.exe"), "{t}");
    // Not repeated in the same session.
    assert!(recall::on_prompt(&mut db, "/p", &e("s1", "publica también la 0.10.2"), None).unwrap().is_none_or(|t| !t.contains("A saved procedure")));

    // Talking about a release is not asking for one («publicación» is not «publica»).
    let t = recall::on_prompt(&mut db, "/p", &e("s2", "qué tal fue la publicación de ayer"), None).unwrap();
    assert!(t.is_none_or(|t| !t.contains("A saved procedure")), "no debería traer la receta");
    // Two words: «saca versión» with the accent also works.
    let t = recall::on_prompt(&mut db, "/p", &e("s3", "saca versión nueva cuando puedas"), None).unwrap().expect("la receta");
    assert!(t.contains("A saved procedure"), "{t}");
    // Another project doesn't see it.
    assert!(recall::on_prompt(&mut db, "/otro", &e("s4", "publica la web"), None).unwrap().is_none_or(|t| !t.contains("A saved procedure")));
}

/// A lone word from `when:` («publica») fits many things: with a global recipe (or one from another
/// project, seen from a general folder), something more is needed to say it is that recipe.
#[test]
fn a_lone_word_is_not_enough_outside_its_project() {
    let dir = comun::Temporal::nueva("recetas-sueltas");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let n = NewMemory { project: String::new(), kind: Some(Kind::Procedure), title: "Publicar una versión de Lumbre".into(), content: STEPS.into(), ..Default::default() };
    memoria::save(&mut db, n, None).unwrap();
    let asks = |db: &mut rusqlite::Connection, session: &str, p: &str| {
        let e = HookInput { session_id: session.into(), prompt: p.into(), ..Default::default() };
        recall::on_prompt(db, "/q", &e, None).unwrap().is_some_and(|t| t.contains("A saved procedure"))
    };
    // What is not releasing a version: the noise «publica» brought in real messages.
    for (i, p) in ["publica un post en linkedin sobre el paper", "pues publica", "vale me encanta como esta publica", "Publícalo tú dentro de 10 minutos"].iter().enumerate() {
        assert!(!asks(&mut db, &format!("n{i}"), p), "no debería traer la receta: {p}");
    }
    // What is: a word from its title or its project, a phrase from `when:` or two of its words.
    for (i, p) in ["publica la versión nueva", "publica lumbre ya", "saca versión cuando puedas", "publica la release de hoy"].iter().enumerate() {
        assert!(asks(&mut db, &format!("s{i}"), p), "debería traer la receta: {p}");
    }
}
