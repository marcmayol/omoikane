//! Fase 1: guardar, buscar, cambiar con historial, olvidar y que la tabla y los índices no se
//! separen nunca.

use omoikane::buscar::{self, Opciones};
use omoikane::db;
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use rusqlite::Connection;
use std::path::PathBuf;

/// Base nueva en un fichero temporal (no en memoria: así se prueba también WAL).
fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-nucleo-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ruta = dir.join("o.db");
    (db::abrir(&ruta).unwrap(), dir)
}

fn nueva(project: &str, kind: Tipo, title: &str, content: &str) -> Nueva {
    Nueva { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() }
}

fn buscar(db: &mut Connection, ambito: Ambito, q: &str) -> Vec<String> {
    let o = Opciones { ambito, kind: None, tags: vec![], limite: 50 };
    buscar::buscar(db, q, &o, None).unwrap().into_iter().map(|r| r.memoria.title).collect()
}

fn cuenta(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

#[test]
fn encuentra_con_y_sin_tildes() {
    let (mut db, dir) = base("tildes");
    memoria::guardar(
        &mut db,
        nueva("/p", Tipo::Gotcha, "Bug en auth", "Fallo de autenticación: el token caduca antes de tiempo."), None,
    )
    .unwrap();
    memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "Receta", "Tortilla de patatas con cebolla."), None).unwrap();
    let p = || Ambito::Proyecto("/p".into());
    assert_eq!(buscar(&mut db, p(), "fallo de autenticación"), ["Bug en auth"]);
    assert_eq!(buscar(&mut db, p(), "AUTENTICACION"), ["Bug en auth"]);
    // Prefijo: «tokens» no está, «token» sí; «autentica» encuentra «autenticación».
    assert_eq!(buscar(&mut db, p(), "autentica"), ["Bug en auth"]);
    assert!(buscar(&mut db, p(), "tokens").is_empty());
    assert!(buscar(&mut db, p(), "kubernetes").is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ordena_por_cuantas_palabras_coinciden() {
    let (mut db, dir) = base("orden");
    for (t, c) in [
        ("Uno", "fallo en el login"),
        ("Dos", "fallo de autenticación en el login"),
        ("Tres", "el login va lento"),
    ] {
        memoria::guardar(&mut db, nueva("/p", Tipo::Fact, t, c), None).unwrap();
    }
    let r = buscar(&mut db, Ambito::Todos, "fallo autenticación login");
    assert_eq!(r[0], "Dos");
    assert_eq!(r.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn update_guarda_la_version_anterior_y_reindexa() {
    let (mut db, dir) = base("update");
    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Decision, "Usamos UUID", "Por compatibilidad."), None).unwrap();
    let c = Cambios {
        title: Some("Usamos ULID".into()),
        content: Some("Se ordenan por fecha.".into()),
        reason: Some("los UUID no se ordenan".into()),
        ..Default::default()
    };
    let m2 = memoria::actualizar(&mut db, &m.id, c, None).unwrap();
    assert_eq!(m2.id, m.id);
    assert_eq!(m2.title, "Usamos ULID");
    assert_eq!(m2.created_at, m.created_at);
    let h = memoria::historial(&db, &m.id).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].version, h[0].title.as_str()), (1, "Usamos UUID"));
    assert_eq!(h[0].reason.as_deref(), Some("los UUID no se ordenan"));
    // El índice tiene el texto nuevo y no el viejo.
    assert_eq!(buscar(&mut db, Ambito::Todos, "ordenan fecha"), ["Usamos ULID"]);
    assert!(buscar(&mut db, Ambito::Todos, "compatibilidad").is_empty());
    // Una segunda versión va delante en el historial.
    memoria::actualizar(&mut db, &m.id, Cambios { kind: Some(Tipo::Fact), ..Default::default() }, None).unwrap();
    let h = memoria::historial(&db, &m.id).unwrap();
    assert_eq!(h.iter().map(|v| v.version).collect::<Vec<_>>(), [2, 1]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fijar_no_crea_version_ni_cambia_la_fecha() {
    let (mut db, dir) = base("fijar");
    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Preference, "Tabs", "Cuatro espacios."), None).unwrap();
    let m2 = memoria::actualizar(&mut db, &m.id, Cambios { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert!(m2.pinned);
    assert_eq!(m2.updated_at, m.updated_at);
    assert!(memoria::historial(&db, &m.id).unwrap().is_empty());
    // Sin cambios reales, tampoco.
    memoria::actualizar(&mut db, &m.id, Cambios { title: Some("Tabs".into()), ..Default::default() }, None).unwrap();
    assert!(memoria::historial(&db, &m.id).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn forget_borra_todo() {
    let (mut db, dir) = base("forget");
    let mut n = nueva("/p", Tipo::Gotcha, "Trampa", "No borres la caché a mano.");
    n.paths = vec!["src/cache.rs".into()];
    let m = memoria::guardar(&mut db, n, None).unwrap();
    memoria::actualizar(&mut db, &m.id, Cambios { content: Some("Nunca.".into()), ..Default::default() }, None).unwrap();
    memoria::olvidar(&mut db, &m.id).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory"), 0);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_version"), 0);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_path"), 0);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_fts_docsize"), 0);
    assert!(buscar(&mut db, Ambito::Todos, "caché nunca trampa").is_empty());
    assert!(memoria::obtener(&db, &m.id).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ambitos_proyecto_global_y_todos() {
    let (mut db, dir) = base("ambitos");
    memoria::guardar(&mut db, nueva("/a", Tipo::Fact, "De A", "compartida palabra"), None).unwrap();
    memoria::guardar(&mut db, nueva("/b", Tipo::Fact, "De B", "compartida palabra"), None).unwrap();
    memoria::guardar(&mut db, nueva("", Tipo::Preference, "Global", "compartida palabra"), None).unwrap();
    let mut a = buscar(&mut db, Ambito::Proyecto("/a".into()), "compartida");
    a.sort();
    assert_eq!(a, ["De A", "Global"]);
    assert_eq!(buscar(&mut db, Ambito::Todos, "compartida").len(), 3);
    let r = memoria::recientes(&db, &Ambito::Proyecto("/b".into()), None, 10).unwrap();
    assert_eq!(r.len(), 2);
    let p = memoria::proyectos(&db).unwrap();
    assert_eq!(p.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn filtros_de_tipo_y_etiquetas() {
    let (mut db, dir) = base("filtros");
    let mut n = nueva("/p", Tipo::Gotcha, "Con etiqueta", "despliegue roto");
    n.tags = vec!["#Deploy".into(), "ci".into()];
    memoria::guardar(&mut db, n, None).unwrap();
    memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "Sin etiqueta", "despliegue lento"), None).unwrap();
    let o = |kind, tags: &[&str]| Opciones {
        ambito: Ambito::Todos,
        kind,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        limite: 10,
    };
    let titulos = |db: &mut Connection, o: Opciones| -> Vec<String> {
        buscar::buscar(db, "despliegue", &o, None).unwrap().into_iter().map(|r| r.memoria.title).collect()
    };
    assert_eq!(titulos(&mut db, o(None, &["deploy"])), ["Con etiqueta"]);
    assert_eq!(titulos(&mut db, o(None, &["deploy", "otra"])), Vec::<String>::new());
    assert_eq!(titulos(&mut db, o(Some(Tipo::Fact), &[])), ["Sin etiqueta"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ids_por_prefijo() {
    let (mut db, dir) = base("ids");
    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "Una", "uno"), None).unwrap();
    memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "Otra", "dos"), None).unwrap();
    assert_eq!(memoria::obtener(&db, &m.id).unwrap().title, "Una");
    assert_eq!(memoria::obtener(&db, &m.id.to_lowercase()).unwrap().title, "Una");
    // Las dos se crearon en el mismo milisegundo o casi: el principio de tiempo no basta.
    assert!(memoria::obtener(&db, &m.id[..4]).is_err());
    assert!(memoria::obtener(&db, "ZZZZZZZZ").is_err());
    assert!(memoria::obtener(&db, "a'b--").is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn la_busqueda_cuenta_como_recuperada() {
    let (mut db, dir) = base("recuperada");
    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "Puerto", "El servidor escucha en el 8765."), None).unwrap();
    assert_eq!(m.recall_count, 0);
    buscar(&mut db, Ambito::Todos, "servidor puerto");
    buscar(&mut db, Ambito::Todos, "servidor");
    let m = memoria::obtener(&db, &m.id).unwrap();
    assert_eq!(m.recall_count, 2);
    assert!(m.recalled_at.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn exporta_con_historial() {
    let (mut db, dir) = base("export");
    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Fact, "A", "uno"), None).unwrap();
    memoria::actualizar(&mut db, &m.id, Cambios { content: Some("dos".into()), ..Default::default() }, None).unwrap();
    memoria::guardar(&mut db, nueva("/otro", Tipo::Fact, "B", "tres"), None).unwrap();
    let mut salida = Vec::new();
    assert_eq!(memoria::exportar(&db, &Ambito::Proyecto("/p".into()), &mut salida).unwrap(), 1);
    let lineas: Vec<serde_json::Value> =
        String::from_utf8(salida).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lineas.len(), 1);
    assert_eq!(lineas[0]["content"], "dos");
    assert_eq!(lineas[0]["versions"][0]["content"], "uno");
    let mut todo = Vec::new();
    assert_eq!(memoria::exportar(&db, &Ambito::Todos, &mut todo).unwrap(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

/// Tras cientos de operaciones al azar, la tabla, el índice FTS y las rutas siguen cuadrando, y
/// cada memoria se encuentra por su palabra única.
#[test]
fn coherencia_tras_operaciones_al_azar() {
    let (mut db, dir) = base("coherencia");
    let mut r = 0x2545F4914F6CDD1Du64;
    let mut azar = move |n: u64| {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r % n
    };
    let mut vivas: Vec<(String, u64)> = Vec::new();
    for i in 0..400u64 {
        match azar(10) {
            0..=4 => {
                let mut n = nueva("/p", Tipo::TODOS[azar(4) as usize], &format!("marca{i} título"), "contenido común");
                n.paths = (0..azar(3)).map(|k| format!("src/f{k}.rs")).collect();
                n.tags = (0..azar(3)).map(|k| format!("t{k}")).collect();
                vivas.push((memoria::guardar(&mut db, n, None).unwrap().id, i));
            }
            5..=7 if !vivas.is_empty() => {
                let k = azar(vivas.len() as u64) as usize;
                let marca = vivas[k].1;
                let c = Cambios {
                    content: Some(format!("cambiado {i}")),
                    paths: Some((0..azar(3)).map(|k| format!("lib/g{k}.rs")).collect()),
                    title: Some(format!("marca{marca} retitulada")),
                    ..Default::default()
                };
                memoria::actualizar(&mut db, &vivas[k].0, c, None).unwrap();
            }
            8..=9 if !vivas.is_empty() => {
                let k = azar(vivas.len() as u64) as usize;
                memoria::olvidar(&mut db, &vivas.remove(k).0).unwrap();
            }
            _ => {}
        }
    }
    let n = cuenta(&db, "SELECT count(*) FROM memory");
    assert_eq!(n as usize, vivas.len());
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_fts_docsize"), n);
    assert_eq!(
        cuenta(&db, "SELECT count(*) FROM memory_path"),
        cuenta(&db, "SELECT COALESCE(sum(json_array_length(paths)), 0) FROM memory")
    );
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory)"), 0);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_fts_docsize WHERE id NOT IN (SELECT n FROM memory)"), 0);
    for (id, marca) in &vivas {
        let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
        let r = buscar::buscar(&mut db, &format!("marca{marca}"), &o, None).unwrap();
        assert_eq!(r.first().map(|r| r.memoria.id.as_str()), Some(id.as_str()), "marca{marca}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Cuatro conexiones (como cuatro sesiones de agente) escribiendo a la vez en el mismo fichero:
/// ningún SQLITE_BUSY tiene que llegar hasta aquí.
#[test]
fn escrituras_concurrentes() {
    let (db0, dir) = base("concurrencia");
    drop(db0);
    let ruta = dir.join("o.db");
    let hilos: Vec<_> = (0..4)
        .map(|h| {
            let ruta = ruta.clone();
            std::thread::spawn(move || {
                let mut db = db::abrir(&ruta).unwrap();
                for i in 0..50 {
                    let m = memoria::guardar(&mut db, nueva("/p", Tipo::Fact, &format!("h{h} n{i}"), "texto"), None).unwrap();
                    if i % 5 == 0 {
                        let c = Cambios { content: Some(format!("otro {i}")), ..Default::default() };
                        memoria::actualizar(&mut db, &m.id, c, None).unwrap();
                    }
                }
            })
        })
        .collect();
    for h in hilos {
        h.join().unwrap();
    }
    let db = db::abrir(&ruta).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory"), 200);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_fts_docsize"), 200);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_version"), 40);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// El visor (`omoi tui`) dibujado en memoria: lista, búsqueda, fijar, historial y olvidar.
#[test]
fn visor() {
    use omoikane::tui::{Estado, Vista, dibujar};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    let (mut db, dir) = base_visor();
    for (t, c) in [("Puerto del servidor", "Escucha en el 8010"), ("Pasarela de pago", "Redsys y no Stripe")] {
        omoikane::memoria::guardar(&mut db, omoikane::memoria::Nueva { project: "/p".into(), kind: Some(omoikane::memoria::Tipo::Fact), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
    }
    let mut e = Estado::nuevo("/p".into());
    e.cargar(&mut db, None).unwrap();
    assert_eq!(e.memorias.len(), 2);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    let pantalla = |t: &mut Terminal<TestBackend>, e: &mut Estado| {
        t.draw(|f| dibujar(f, e)).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    assert!(pantalla(&mut t, &mut e).contains("Pasarela de pago"));
    let tecla = |e: &mut Estado, db: &mut rusqlite::Connection, c: KeyCode| {
        if e.tecla(KeyEvent::new(c, KeyModifiers::NONE), db).unwrap() {
            e.cargar(db, None).unwrap();
        }
    };
    // Buscar: solo queda la del puerto.
    tecla(&mut e, &mut db, KeyCode::Char('/'));
    for c in "puerto".chars() {
        tecla(&mut e, &mut db, KeyCode::Char(c));
    }
    assert_eq!(e.memorias.len(), 1);
    assert_eq!(e.memorias[0].title, "Puerto del servidor");
    // Fijar y ver que queda fijada. Fijar no es un cambio de contenido: no deja versión anterior.
    tecla(&mut e, &mut db, KeyCode::Enter);
    tecla(&mut e, &mut db, KeyCode::Char('p'));
    assert!(e.memorias[0].pinned);
    tecla(&mut e, &mut db, KeyCode::Char('h'));
    assert_eq!(e.vista, Vista::Historial);
    assert!(e.historial.is_empty());
    assert!(pantalla(&mut t, &mut e).contains("Sin versiones anteriores") || pantalla(&mut t, &mut e).contains("No previous versions"));
    tecla(&mut e, &mut db, KeyCode::Esc);
    // Olvidar pide confirmación: «n» no borra, «s» sí.
    tecla(&mut e, &mut db, KeyCode::Char('d'));
    tecla(&mut e, &mut db, KeyCode::Char('n'));
    assert_eq!(e.memorias.len(), 1);
    tecla(&mut e, &mut db, KeyCode::Char('d'));
    tecla(&mut e, &mut db, KeyCode::Char('s'));
    assert!(e.memorias.is_empty());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

fn base_visor() -> (rusqlite::Connection, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-visor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (omoikane::db::abrir(&dir.join("o.db")).unwrap(), dir)
}
