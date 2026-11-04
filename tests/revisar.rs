//! Fase 7.1: lo que conviene revisar. Los casos salen de las memorias reales con las que se calibró
//! (contados con palabras inventadas: el repositorio es público).

use omoikane::db;
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::revisar;
use rusqlite::Connection;
use std::path::PathBuf;

fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-revisar-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (db::abrir(&dir.join("o.db")).unwrap(), dir)
}

fn guardar(db: &mut Connection, project: &str, title: &str, content: &str, tags: &[&str]) -> String {
    let n = Nueva {
        project: project.into(),
        kind: Some(Tipo::Fact),
        title: title.into(),
        content: content.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

fn desfasadas(db: &Connection) -> Vec<(String, String)> {
    revisar::avisos(db, &Ambito::Todos).unwrap().into_iter().filter(|a| a.kind == revisar::DESFASADA).map(|a| (a.memory_id, a.other_id)).collect()
}

/// El caso que lo motivó: la nota del proyecto sigue en la 0.2.0 y la de estado ya va por la 0.9.2.
#[test]
fn avisa_de_la_version_vieja_y_lo_dice_en_el_recall() {
    let (mut db, dir) = base("version");
    let vieja = guardar(&mut db, "/p", "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let nueva = guardar(&mut db, "/p", "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(desfasadas(&db), vec![(vieja.clone(), nueva.clone())]);
    let a = &revisar::avisos(&db, &Ambito::Todos).unwrap()[0];
    assert!(a.detail.contains("0.2.0 → 0.9.2"), "{}", a.detail);
    // Lo que `remember` le dice al agente nada más guardar la nueva.
    let viejas = revisar::superadas_por(&db, &nueva).unwrap();
    assert_eq!(viejas.iter().map(|v| v.0.as_str()).collect::<Vec<_>>(), [vieja.as_str()]);
    // Al empezar la sesión, la vieja sale con su advertencia y el id de la nueva.
    let t = recall::al_empezar(&mut db, "/p", &Entrada { session_id: "s".into(), ..Default::default() }).unwrap().unwrap();
    let linea = t.lines().find(|l| l.contains("memoria para agentes en Rust")).unwrap();
    assert!(linea.contains("⚠ may be outdated") && linea.contains(&nueva[..12]), "{t}");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Falsos positivos que daban las primeras versiones con las notas reales.
#[test]
fn no_avisa_de_cosas_distintas() {
    let (mut db, dir) = base("distintas");
    // Versiones de cosas distintas en el mismo proyecto.
    guardar(&mut db, "/p", "Dependencias de Lumbre", "Usa rusqlite 0.41.0 desde el 27-oct-2026.", &[]);
    guardar(&mut db, "/p", "Lumbre publicada", "omoi 0.9.2 publicado el 29-oct-2026.", &[]);
    // «Pendiente» en una y «publicada» en otra, hablando de cosas distintas.
    guardar(&mut db, "", "Eslogan de Lumbre", "Elegido el 29-oct-2026. Falta ponerlo en la descripción del repositorio.", &[]);
    guardar(&mut db, "", "Lumbre: estado de la versión", "La versión está publicada e instalada en el portátil desde el 30-oct-2026.", &[]);
    assert!(desfasadas(&db).is_empty(), "{:?}", revisar::avisos(&db, &Ambito::Todos).unwrap());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// «Pendiente» en la vieja y «hecho» en la nueva sobre lo mismo sí es un aviso.
#[test]
fn pendiente_y_hecho_de_lo_mismo() {
    let (mut db, dir) = base("pendiente");
    let vieja = guardar(&mut db, "", "Certificado del dominio gaviota", "28-oct-2026: pendiente renovar el certificado wildcard de gaviota.example.", &[]);
    let nueva = guardar(&mut db, "", "Dominio gaviota renovado", "30-oct-2026: hecho, renovado el certificado wildcard de gaviota.example.", &[]);
    assert_eq!(desfasadas(&db), vec![(vieja, nueva)]);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// «Está bien así» vale hasta que la memoria vuelva a cambiar.
#[test]
fn descartar_hasta_que_cambie() {
    let (mut db, dir) = base("descartar");
    let vieja = guardar(&mut db, "/p", "Lumbre: memoria para agentes", "La v0.2.0 publicada el 28-oct-2026.", &[]);
    let nueva = guardar(&mut db, "/p", "Lumbre: estado", "La 0.9.2 está publicada desde el 29-oct-2026.", &["status"]);
    assert_eq!(desfasadas(&db).len(), 1);
    revisar::descartar(&db, &vieja, revisar::DESFASADA, &nueva).unwrap();
    assert!(desfasadas(&db).is_empty());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let c = Cambios { content: Some("La v0.2.0 publicada el 28-oct-2026. Y algo más.".into()), ..Default::default() };
    memoria::actualizar(&mut db, &vieja, c, None).unwrap();
    assert_eq!(desfasadas(&db).len(), 1, "tras cambiar, vuelve a avisar");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Las que dependen del disco y del largo.
#[test]
fn rutas_rotas_y_largas() {
    let (mut db, dir) = base("rutas");
    let proyecto = dir.join("proyecto");
    std::fs::create_dir_all(proyecto.join("src")).unwrap();
    std::fs::write(proyecto.join("src/existe.rs"), "").unwrap();
    let p = proyecto.to_string_lossy().to_string();
    let n = Nueva {
        project: p.clone(),
        kind: Some(Tipo::Gotcha),
        title: "Trampa en el lector".into(),
        content: "Cuidado con el orden.".into(),
        paths: vec!["src/existe.rs".into(), "src/borrado.rs".into()],
        ..Default::default()
    };
    memoria::guardar(&mut db, n, None).unwrap();
    guardar(&mut db, &p, "Nota enorme", &"palabra ".repeat(1000), &[]);
    let v = revisar::avisos(&db, &Ambito::Todos).unwrap();
    let rota = v.iter().find(|a| a.kind == revisar::RUTA_ROTA).expect("ruta rota");
    assert!(rota.detail.contains("src/borrado.rs") && !rota.detail.contains("existe.rs"), "{}", rota.detail);
    assert!(v.iter().any(|a| a.kind == revisar::LARGA && a.title == "Nota enorme"));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

// --- El aviso al empezar la sesión ------------------------------------------------------------------

/// `n` memorias globales demasiado largas para el recall: cada una, un aviso `too_long`.
fn largas(db: &mut Connection, desde: usize, n: usize) {
    for i in desde..desde + n {
        guardar(db, "", &format!("Bitácora larga número {i}"), &format!("Registro {i} del horno. {}", format!("cocción {i} ").repeat(700)), &[]);
    }
}

fn aviso_al_empezar(db: &mut Connection, sesion: &str) -> Option<String> {
    let t = recall::al_empezar(db, "/p", &Entrada { session_id: sesion.into(), ..Default::default() }).unwrap().unwrap();
    t.lines().find(|l| l.starts_with("Memory review:")).map(String::from)
}

/// Por debajo del umbral no dice nada; al llegar, una vez: con las cifras de los tipos que hay y que
/// proponga sin tocar nada.
#[test]
fn avisa_de_la_revision_al_pasar_el_umbral_y_una_vez() {
    let (mut db, dir) = base("aviso");
    largas(&mut db, 0, revisar::AVISO_MINIMO - 1);
    assert_eq!(aviso_al_empezar(&mut db, "s1"), None, "por debajo del umbral");
    largas(&mut db, 100, 1);
    let a = aviso_al_empezar(&mut db, "s2").expect("al llegar al umbral");
    assert!(a.contains("20 items pending (20 too long)") && !a.contains("outdated"), "{a}");
    assert!(a.contains("omoi review") && a.contains("absorb") && a.contains("without the user's OK"), "{a}");
    // El mismo día, no otra vez; tampoco si crece poco.
    assert_eq!(aviso_al_empezar(&mut db, "s3"), None, "dos veces el mismo día");
    largas(&mut db, 200, 4);
    assert_eq!(aviso_al_empezar(&mut db, "s4"), None, "4 más no es bastante");
    // Si crece bastante, sí, con la cifra nueva.
    largas(&mut db, 300, 1);
    let a = aviso_al_empezar(&mut db, "s5").expect("al crecer");
    assert!(a.contains("25 items pending"), "{a}");
    // Otro día, otra vez aunque no haya crecido.
    db.execute("UPDATE meta SET v = '2000-01-01|25' WHERE k = 'revision_aviso'", []).unwrap();
    assert!(aviso_al_empezar(&mut db, "s6").is_some(), "al día siguiente");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// La herramienta `context` (los agentes sin recall al empezar) da el mismo aviso, y comparte con el
/// recall el «ya se dijo hoy».
#[test]
fn el_aviso_tambien_en_context() {
    let (mut db, dir) = base("aviso-context");
    let carpeta = dir.join("proyecto");
    std::fs::create_dir_all(&carpeta).unwrap();
    largas(&mut db, 0, revisar::AVISO_MINIMO);
    let ruta = dir.join("o.db");
    let context = || {
        let (t, error) = omoikane::mcp::herramienta(ruta.clone(), Some(carpeta.clone()), Some("context"), "", "test").unwrap();
        assert!(!error, "{t}");
        serde_json::from_str::<serde_json::Value>(&t).unwrap()
    };
    let r = context();
    let a = r["review_notice"].as_str().expect("el aviso");
    assert!(a.starts_with("Memory review: 20 items pending (20 too long)"), "{a}");
    assert!(context().get("review_notice").is_none(), "una vez al día");
    assert_eq!(aviso_al_empezar(&mut db, "s1"), None, "ya se dijo hoy por context");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// `context` cuenta como al empezar, sin las rutas rotas: tener unas cuantas no hace «crecer» la
/// cuenta ni repite el aviso en la misma sesión.
#[test]
fn las_rutas_rotas_no_repiten_el_aviso_en_context() {
    let (mut db, dir) = base("aviso-rutas");
    let carpeta = dir.join("proyecto");
    std::fs::create_dir_all(&carpeta).unwrap();
    // La ruta como la resuelve Omoikane: en Windows, la carpeta temporal puede llegar con su nombre
    // corto (`RUNNER~1`) y `context` la resuelve con el largo.
    let p = omoikane::proyecto::de(&carpeta).unwrap();
    largas(&mut db, 0, revisar::AVISO_MINIMO);
    assert!(aviso_al_empezar(&mut db, "s1").is_some(), "al empezar");
    for i in 0..6 {
        let n = Nueva {
            project: p.clone(),
            kind: Some(Tipo::Gotcha),
            title: format!("Trampa {i} del lector"),
            content: format!("Cuidado con el orden {i}."),
            paths: vec![format!("src/borrado{i}.rs")],
            ..Default::default()
        };
        memoria::guardar(&mut db, n, None).unwrap();
    }
    let (t, error) = omoikane::mcp::herramienta(dir.join("o.db"), Some(carpeta.clone()), Some("context"), "", "test").unwrap();
    assert!(!error, "{t}");
    let r: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert!(r["review"].as_array().unwrap().iter().filter(|a| a["kind"] == revisar::RUTA_ROTA).count() >= 5, "{r}");
    assert!(r.get("review_notice").is_none(), "{r}");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Al empezar se cuenta sin leer las memorias enteras (`pendientes`): da lo mismo que la revisión
/// entera, salvo las rutas rotas y los secretos, que solo salen en ella.
#[test]
fn las_cifras_rapidas_cuadran_con_la_revision() {
    let (mut db, dir) = base("cifras");
    let vieja = guardar(&mut db, "/p", "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026.", &[]);
    guardar(&mut db, "/p", "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub.", &["status"]);
    guardar(&mut db, "/p", "Horno del taller", "Ver [[manual_que_no_existe]] antes de abrirlo.", &[]);
    largas(&mut db, 0, 2);
    let otra = guardar(&mut db, "/otro", "Cosa de otro proyecto", "Ver [[tampoco_existe]].", &[]);
    // Una vieja y sin uso: `unused`, y lo que la revisión diga de ella por la fecha.
    db.execute("UPDATE memory SET created_at = '2020-01-01T00:00:00.000Z', updated_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [&vieja]).unwrap();
    // Un estado de hace años con cambios después: `stale_status`.
    let estado = guardar(&mut db, "/p", "Horno: en qué punto quedó", "Faltaba calibrar el termopar.", &["status"]);
    db.execute("UPDATE memory SET updated_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [&estado]).unwrap();
    // Un descarte cuenta en las dos.
    revisar::descartar(&db, &otra, revisar::ENLACE_ROTO, "").unwrap();
    for ambito in [Ambito::Todos, Ambito::de(&db, "/p").unwrap()] {
        let rapidas = revisar::pendientes(&db, &ambito).unwrap();
        let enteras = revisar::cifras(&revisar::avisos(&db, &ambito).unwrap());
        assert_eq!(rapidas, enteras);
        assert!(rapidas.len() >= 5 && rapidas.contains_key(revisar::ESTADO_VIEJO), "{rapidas:?}");
    }
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
