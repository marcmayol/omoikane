//! 1.6: el historial de cada fichero, sacado del diario de sesiones, en el recall del mensaje que lo
//! nombra.

mod comun;

use omoikane::db;
use omoikane::diario;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::riesgo;
use rusqlite::Connection;

fn dir(nombre: &str) -> comun::Temporal {
    let d = comun::Temporal::nueva(&format!("riesgo-{nombre}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    d
}

/// Una línea del diario, a mano: lo que dejaría `diario::apuntar` al cerrar la sesión.
fn sesion(db: &Connection, nombre: &str, project_id: i64, carpeta: &str, dia: &str, ficheros: &[&str]) {
    diario::crear_tabla(db).unwrap();
    db.execute(
        "INSERT INTO diary(session, project_id, folder, first_at, last_at, files) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        rusqlite::params![nombre, project_id, carpeta, format!("{dia}T10:00:00.000Z"), serde_json::to_string(ficheros).unwrap()],
    )
    .unwrap();
}

fn entrada(sesion: &str, prompt: &str) -> Entrada {
    Entrada { session_id: sesion.into(), prompt: prompt.into(), ..Default::default() }
}

fn gotcha(db: &mut Connection, project: &str, titulo: &str, rutas: &[&str]) {
    let n = Nueva {
        project: project.into(),
        kind: Some(Tipo::Gotcha),
        title: titulo.into(),
        content: format!("{titulo}: cuidado al tocarlo."),
        paths: rutas.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap();
}

/// Un proyecto con 5 sesiones en `src/sync.rs`: 4 con `tests/sync.rs`, 3 con `CHANGELOG.md`, 2 con
/// `src/db.rs` (no llega a 3) y una suelta con `README.md`.
fn proyecto(nombre: &str) -> (comun::Temporal, Connection, String, i64) {
    let d = dir(nombre);
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    let id = omoikane::proyecto::resolver(&db, &repo).unwrap().unwrap().0;
    sesion(&db, "a", id, &repo, "2026-10-01", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    sesion(&db, "b", id, &repo, "2026-10-08", &["src/sync.rs", "tests/sync.rs", "src/db.rs"]);
    sesion(&db, "c", id, &repo, "2026-10-15", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md", "src/db.rs"]);
    sesion(&db, "d", id, &repo, "2026-11-04", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    sesion(&db, "e", id, &repo, "2026-10-20", &["src/sync.rs", "README.md"]);
    sesion(&db, "f", id, &repo, "2026-10-21", &["README.md", "src/db.rs"]);
    gotcha(&mut db, &repo, "El push de sync pierde el orden", &["src/sync.rs"]);
    gotcha(&mut db, &repo, "Sync con la ruta entera", &[&format!("{repo}/src/sync.rs")]);
    gotcha(&mut db, &repo, "Otro fichero", &["src/db.rs"]);
    (d, db, repo, id)
}

#[test]
fn una_linea_con_lo_que_cambia_a_la_vez() {
    let (_d, mut db, repo, _) = proyecto("linea");
    let t = recall::al_preguntar(&mut db, &repo, &entrada("s1", "arregla el fallo en sync.rs"), None).unwrap().expect("la historia");
    assert!(t.starts_with("<omoikane>") && t.ends_with("</omoikane>"), "{t}");
    assert!(
        t.contains(
            "History of `src/sync.rs`: changed in 5 sessions (last 2026-11-04); it usually changes together with `tests/sync.rs` (4 of 5) and `CHANGELOG.md` (3); 2 gotchas mention it."
        ),
        "{t}"
    );
    // Los gotcha mismos llegan por las rutas, como siempre.
    assert!(t.contains("El push de sync pierde el orden"), "{t}");
    // Una vez por fichero y sesión.
    let otra = recall::al_preguntar(&mut db, &repo, &entrada("s1", "y ahora el test de sync.rs"), None).unwrap().unwrap_or_default();
    assert!(!otra.contains("History of"), "{otra}");
    // Otra sesión lo vuelve a ver, y tras compactar también (ya no está en su contexto).
    let t2 = recall::al_preguntar(&mut db, &repo, &entrada("s2", "mira src/sync.rs"), None).unwrap().unwrap_or_default();
    assert!(t2.contains("History of `src/sync.rs`"), "{t2}");
    recall::al_empezar(&mut db, &repo, &Entrada { session_id: "s1".into(), source: "compact".into(), ..Default::default() }).unwrap();
    let t3 = recall::al_preguntar(&mut db, &repo, &entrada("s1", "otra vez sync.rs"), None).unwrap().unwrap_or_default();
    assert!(t3.contains("History of `src/sync.rs`"), "{t3}");
    // No cuenta como memoria recibida: en `recall_log` solo hay memorias.
    let log: i64 = db.query_row("SELECT count(*) FROM recall_log WHERE memory_id NOT IN (SELECT id FROM memory)", [], |f| f.get(0)).unwrap();
    assert_eq!(log, 0);
    let dichas: i64 = db.query_row("SELECT count(*) FROM diary_hint", [], |f| f.get(0)).unwrap();
    assert_eq!(dichas, 2);
}

#[test]
fn sin_bastantes_sesiones_no_dice_nada() {
    let (_d, db, repo, _) = proyecto("umbral");
    // `README.md`: 2 sesiones, por debajo de 3.
    assert!(riesgo::historias(&db, &repo, "cambia README.md").unwrap().is_empty());
    // `src/db.rs`: 3 sesiones, pero ninguno cambia con él en 3; su gotcha sí cuenta.
    let h = riesgo::historias(&db, &repo, "cambia src/db.rs").unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].sesiones, h[0].juntos.len(), h[0].gotchas), (3, 0, 1));
    assert_eq!(h[0].linea(), "History of `src/db.rs`: changed in 3 sessions (last 2026-10-21); 1 gotcha mentions it.");
}

#[test]
fn sin_fichero_en_el_mensaje_no_mira_el_diario() {
    let (_d, mut db, repo, _) = proyecto("nada");
    assert!(!riesgo::parece_nombrar("qué tal ha ido la sincronización de ayer"));
    let t = recall::al_preguntar(&mut db, &repo, &entrada("s1", "qué tal ha ido la sincronización de ayer"), None).unwrap().unwrap_or_default();
    assert!(!t.contains("History of"), "{t}");
    let hay: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'diary_hint')", [], |f| f.get(0)).unwrap();
    assert!(!hay, "sin nada que decir no se crea la tabla");
    // Sin diario (base nueva), tampoco: ni error.
    let d2 = dir("vacia");
    let repo2 = omoikane::proyecto::de(&d2.join("repo")).unwrap();
    let db2 = db::abrir(&d2.join("o.db")).unwrap();
    assert!(riesgo::al_preguntar(&db2, &repo2, "s", "mira src/sync.rs").unwrap().is_none());
}

#[test]
fn solo_las_sesiones_de_este_proyecto() {
    let (d, db, repo, id) = proyecto("ambito");
    // Otro proyecto con el mismo fichero, y sesiones sin proyecto: desde esta carpeta cuentan, desde
    // otra no.
    let otro = d.join("otro").to_string_lossy().into_owned();
    for (i, dia) in ["2026-11-01", "2026-11-02", "2026-11-03"].iter().enumerate() {
        sesion(&db, &format!("o{i}"), id + 1000, &otro, dia, &["src/sync.rs"]);
        sesion(&db, &format!("h{i}"), 0, &otro, dia, &["src/sync.rs"]);
    }
    sesion(&db, "suelta", 0, &repo, "2026-11-05", &["src/sync.rs"]);
    let h = riesgo::historias(&db, &repo, "src/sync.rs").unwrap();
    assert_eq!((h[0].sesiones, h[0].ultima.as_str()), (6, "2026-11-05"));
}

#[test]
fn con_el_nombre_solo_el_que_mas_se_mueve() {
    let (_d, db, repo, id) = proyecto("nombre");
    for (i, dia) in ["2026-11-01", "2026-11-02", "2026-11-03"].iter().enumerate() {
        sesion(&db, &format!("t{i}"), id, &repo, dia, &["tools/sync.rs"]);
    }
    // Por el nombre, solo `src/sync.rs` (5 sesiones frente a 3).
    let h = riesgo::historias(&db, &repo, "mira sync.rs").unwrap();
    assert_eq!(h.iter().map(|x| x.fichero.as_str()).collect::<Vec<_>>(), ["src/sync.rs"]);
    // Con la ruta entera, ese; y los dos si se nombran los dos.
    let h = riesgo::historias(&db, &repo, "mira tools/sync.rs").unwrap();
    assert_eq!(h.iter().map(|x| x.fichero.as_str()).collect::<Vec<_>>(), ["tools/sync.rs"]);
    let h = riesgo::historias(&db, &repo, "compara tools/sync.rs con src/sync.rs").unwrap();
    assert_eq!(h.len(), 2);
}

/// El historial es opcional: si falla (aquí, una `diary_hint` que no es la suya), el recall del mensaje
/// y el arranque de la sesión siguen, sin él.
#[test]
fn si_falla_el_historial_el_recall_sigue() {
    let (_d, mut db, repo, _) = proyecto("falla");
    db.execute_batch("CREATE TABLE diary_hint (otra TEXT)").unwrap();
    assert!(riesgo::al_preguntar(&db, &repo, "s1", "mira src/sync.rs").is_err(), "la tabla rota hace fallar el historial");
    let t = recall::al_preguntar(&mut db, &repo, &entrada("s1", "mira src/sync.rs"), None).unwrap().expect("las memorias siguen llegando");
    assert!(t.contains("El push de sync pierde el orden"), "{t}");
    assert!(!t.contains("History of"), "{t}");
    assert!(recall::al_empezar(&mut db, &repo, &entrada("s2", "")).unwrap().is_some());
}
