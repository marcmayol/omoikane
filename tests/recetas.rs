//! Fase 7.3: recetas (`procedure`) que vuelven enteras cuando el mensaje las pide.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};

const PASOS: &str = "1. Subir la versión en Cargo.toml y plugin.json.\n2. `git tag -a vX.Y.Z` y push del tag.\n3. `cargo publish`.\n4. En Windows, renombrar omoi.exe antes de instalar: está bloqueado.\n5. Actualizar el plugin.\n\nwhen: publica, publicar, release, saca versión";

#[test]
fn la_receta_llega_entera_cuando_se_pide_y_solo_entonces() {
    let dir = comun::Temporal::nueva("recetas");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let n = Nueva { project: "/p".into(), kind: Some(Tipo::Procedure), title: "Publicar una versión de Lumbre".into(), content: PASOS.into(), ..Default::default() };
    let receta = memoria::guardar(&mut db, n, None).unwrap();
    assert_eq!(recall::disparadores(&receta), ["publica", "publicar", "release", "saca version"]);
    let e = |s: &str, p: &str| Entrada { session_id: s.into(), prompt: p.into(), ..Default::default() };

    // La pide: llega entera, con todos los pasos, aunque el mensaje no comparta palabras raras.
    let t = recall::al_preguntar(&mut db, "/p", &e("s1", "publica la 0.10.1"), None).unwrap().expect("la receta");
    assert!(t.contains("A saved procedure for this") && t.contains("5. Actualizar el plugin") && t.contains("renombrar omoi.exe"), "{t}");
    // En la misma sesión no se repite.
    assert!(recall::al_preguntar(&mut db, "/p", &e("s1", "publica también la 0.10.2"), None).unwrap().is_none_or(|t| !t.contains("A saved procedure")));

    // Hablar de una publicación no es pedirla («publicación» no es «publica»).
    let t = recall::al_preguntar(&mut db, "/p", &e("s2", "qué tal fue la publicación de ayer"), None).unwrap();
    assert!(t.is_none_or(|t| !t.contains("A saved procedure")), "no debería traer la receta");
    // Dos palabras: «saca versión» con tilde también vale.
    let t = recall::al_preguntar(&mut db, "/p", &e("s3", "saca versión nueva cuando puedas"), None).unwrap().expect("la receta");
    assert!(t.contains("A saved procedure"), "{t}");
    // Otro proyecto no la ve.
    assert!(recall::al_preguntar(&mut db, "/otro", &e("s4", "publica la web"), None).unwrap().is_none_or(|t| !t.contains("A saved procedure")));
}

/// Una palabra suelta de `when:` («publica») vale para muchas cosas: con una receta global (o de otro
/// proyecto, vista desde una carpeta general), hace falta algo más que diga que es esa receta.
#[test]
fn una_palabra_suelta_no_basta_fuera_de_su_proyecto() {
    let dir = comun::Temporal::nueva("recetas-sueltas");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let n = Nueva { project: String::new(), kind: Some(Tipo::Procedure), title: "Publicar una versión de Lumbre".into(), content: PASOS.into(), ..Default::default() };
    memoria::guardar(&mut db, n, None).unwrap();
    let pide = |db: &mut rusqlite::Connection, sesion: &str, p: &str| {
        let e = Entrada { session_id: sesion.into(), prompt: p.into(), ..Default::default() };
        recall::al_preguntar(db, "/q", &e, None).unwrap().is_some_and(|t| t.contains("A saved procedure"))
    };
    // Lo que no es publicar una versión: el ruido que traía «publica» en los mensajes reales.
    for (i, p) in ["publica un post en linkedin sobre el paper", "pues publica", "vale me encanta como esta publica", "Publícalo tú dentro de 10 minutos"].iter().enumerate() {
        assert!(!pide(&mut db, &format!("n{i}"), p), "no debería traer la receta: {p}");
    }
    // Lo que sí: una palabra de su título o su proyecto, una frase de `when:` o dos de sus palabras.
    for (i, p) in ["publica la versión nueva", "publica lumbre ya", "saca versión cuando puedas", "publica la release de hoy"].iter().enumerate() {
        assert!(pide(&mut db, &format!("s{i}"), p), "debería traer la receta: {p}");
    }
}
