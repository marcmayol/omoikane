//! Fase 7.3: recetas (`procedure`) que vuelven enteras cuando el mensaje las pide.

use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};

const PASOS: &str = "1. Subir la versión en Cargo.toml y plugin.json.\n2. `git tag -a vX.Y.Z` y push del tag.\n3. `cargo publish`.\n4. En Windows, renombrar omoi.exe antes de instalar: está bloqueado.\n5. Actualizar el plugin.\n\nwhen: publica, publicar, release, saca versión";

#[test]
fn la_receta_llega_entera_cuando_se_pide_y_solo_entonces() {
    let dir = std::env::temp_dir().join(format!("omoi-recetas-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
