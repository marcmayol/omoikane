//! Phase 1: save, search, change with history, forget, and the table and the indexes never
//! drift apart.

mod comun;

use omoikane::buscar::{self, Opciones};
use omoikane::db;
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use rusqlite::Connection;

/// Fresh database in a temp file (not in memory: that way WAL gets tested too).
fn open_db(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("nucleo-{name}"));
    let path = dir.join("o.db");
    let db = db::abrir(&path).unwrap();
    (dir, db)
}

fn new_memory(project: &str, kind: Tipo, title: &str, content: &str) -> Nueva {
    Nueva { project: project.into(), kind: Some(kind), title: title.into(), content: content.into(), ..Default::default() }
}

fn search(db: &mut Connection, scope: Ambito, q: &str) -> Vec<String> {
    let o = Opciones { ambito: scope, kind: None, tags: vec![], limite: 50 };
    buscar::buscar(db, q, &o, None).unwrap().into_iter().map(|r| r.memoria.title).collect()
}

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

#[test]
fn finds_with_and_without_accents() {
    let (_dir, mut db) = open_db("tildes");
    memoria::guardar(
        &mut db,
        new_memory("/p", Tipo::Gotcha, "Bug en auth", "Fallo de autenticación: el token caduca antes de tiempo."), None,
    )
    .unwrap();
    memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "Receta", "Tortilla de patatas con cebolla."), None).unwrap();
    let p0 = Ambito::de(&db, "/p").unwrap();
    let p = || p0;
    assert_eq!(search(&mut db, p(), "fallo de autenticación"), ["Bug en auth"]);
    assert_eq!(search(&mut db, p(), "AUTENTICACION"), ["Bug en auth"]);
    // Prefix: «autentica» finds «autenticación». Stem (from 6 letters): «tokens» finds «token».
    assert_eq!(search(&mut db, p(), "autentica"), ["Bug en auth"]);
    assert_eq!(search(&mut db, p(), "tokens"), ["Bug en auth"]);
    assert!(search(&mut db, p(), "kubernetes").is_empty());
}

#[test]
fn orders_by_how_many_words_match() {
    let (_dir, mut db) = open_db("orden");
    for (t, c) in [
        ("Uno", "fallo en el login"),
        ("Dos", "fallo de autenticación en el login"),
        ("Tres", "el login va lento"),
    ] {
        memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, t, c), None).unwrap();
    }
    let r = search(&mut db, Ambito::Todos, "fallo autenticación login");
    assert_eq!(r[0], "Dos");
    assert_eq!(r.len(), 3);
}

#[test]
fn update_keeps_previous_version_and_reindexes() {
    let (_dir, mut db) = open_db("update");
    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Decision, "Usamos UUID", "Por compatibilidad."), None).unwrap();
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
    // The index has the new text and not the old one.
    assert_eq!(search(&mut db, Ambito::Todos, "ordenan fecha"), ["Usamos ULID"]);
    assert!(search(&mut db, Ambito::Todos, "compatibilidad").is_empty());
    // A second version goes first in the history.
    memoria::actualizar(&mut db, &m.id, Cambios { kind: Some(Tipo::Fact), ..Default::default() }, None).unwrap();
    let h = memoria::historial(&db, &m.id).unwrap();
    assert_eq!(h.iter().map(|v| v.version).collect::<Vec<_>>(), [2, 1]);
}

#[test]
fn pinning_creates_no_version_nor_changes_the_date() {
    let (_dir, mut db) = open_db("fijar");
    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Preference, "Tabs", "Cuatro espacios."), None).unwrap();
    let m2 = memoria::actualizar(&mut db, &m.id, Cambios { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert!(m2.pinned);
    assert_eq!(m2.updated_at, m.updated_at);
    assert!(memoria::historial(&db, &m.id).unwrap().is_empty());
    // Without real changes, neither.
    memoria::actualizar(&mut db, &m.id, Cambios { title: Some("Tabs".into()), ..Default::default() }, None).unwrap();
    assert!(memoria::historial(&db, &m.id).unwrap().is_empty());
}

#[test]
fn forget_deletes_everything() {
    let (_dir, mut db) = open_db("forget");
    let mut n = new_memory("/p", Tipo::Gotcha, "Trampa", "No borres la caché a mano.");
    n.paths = vec!["src/cache.rs".into()];
    let m = memoria::guardar(&mut db, n, None).unwrap();
    memoria::actualizar(&mut db, &m.id, Cambios { content: Some("Nunca.".into()), ..Default::default() }, None).unwrap();
    memoria::olvidar(&mut db, &m.id).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory"), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_version"), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_path"), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts_docsize"), 0);
    assert!(search(&mut db, Ambito::Todos, "caché nunca trampa").is_empty());
    assert!(memoria::obtener(&db, &m.id).is_err());
}

#[test]
fn scopes_project_global_and_all() {
    let (_dir, mut db) = open_db("ambitos");
    memoria::guardar(&mut db, new_memory("/a", Tipo::Fact, "De A", "compartida palabra"), None).unwrap();
    memoria::guardar(&mut db, new_memory("/b", Tipo::Fact, "De B", "compartida palabra"), None).unwrap();
    memoria::guardar(&mut db, new_memory("", Tipo::Preference, "Global", "compartida palabra"), None).unwrap();
    let of_a = Ambito::de(&db, "/a").unwrap();
    let mut a = search(&mut db, of_a, "compartida");
    a.sort();
    assert_eq!(a, ["De A", "Global"]);
    assert_eq!(search(&mut db, Ambito::Todos, "compartida").len(), 3);
    let r = memoria::recientes(&db, &Ambito::de(&db, "/b").unwrap(), None, 10).unwrap();
    assert_eq!(r.len(), 2);
    // The projects (global is not one).
    let p = omoikane::proyecto::lista(&db, None, None, usize::MAX).unwrap();
    assert_eq!(p.len(), 2);
}

#[test]
fn kind_and_tag_filters() {
    let (_dir, mut db) = open_db("filtros");
    let mut n = new_memory("/p", Tipo::Gotcha, "Con etiqueta", "despliegue roto");
    n.tags = vec!["#Deploy".into(), "ci".into()];
    memoria::guardar(&mut db, n, None).unwrap();
    memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "Sin etiqueta", "despliegue lento"), None).unwrap();
    let o = |kind, tags: &[&str]| Opciones {
        ambito: Ambito::Todos,
        kind,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        limite: 10,
    };
    let titles = |db: &mut Connection, o: Opciones| -> Vec<String> {
        buscar::buscar(db, "despliegue", &o, None).unwrap().into_iter().map(|r| r.memoria.title).collect()
    };
    assert_eq!(titles(&mut db, o(None, &["deploy"])), ["Con etiqueta"]);
    assert_eq!(titles(&mut db, o(None, &["deploy", "otra"])), Vec::<String>::new());
    assert_eq!(titles(&mut db, o(Some(Tipo::Fact), &[])), ["Sin etiqueta"]);
}

#[test]
fn ids_by_prefix() {
    let (_dir, mut db) = open_db("ids");
    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "Una", "uno"), None).unwrap();
    memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "Otra", "dos"), None).unwrap();
    assert_eq!(memoria::obtener(&db, &m.id).unwrap().title, "Una");
    assert_eq!(memoria::obtener(&db, &m.id.to_lowercase()).unwrap().title, "Una");
    // Both were created in the same millisecond or nearly: the time prefix is not enough.
    assert!(memoria::obtener(&db, &m.id[..4]).is_err());
    assert!(memoria::obtener(&db, "ZZZZZZZZ").is_err());
    assert!(memoria::obtener(&db, "a'b--").is_err());
}

#[test]
fn search_counts_as_recalled() {
    let (_dir, mut db) = open_db("recuperada");
    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "Puerto", "El servidor escucha en el 8765."), None).unwrap();
    assert_eq!(m.recall_count, 0);
    search(&mut db, Ambito::Todos, "servidor puerto");
    search(&mut db, Ambito::Todos, "servidor");
    let m = memoria::obtener(&db, &m.id).unwrap();
    assert_eq!(m.recall_count, 2);
    assert!(m.recalled_at.is_some());
}

#[test]
fn exports_with_history() {
    let (dir, mut db) = open_db("export");
    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, "A", "uno"), None).unwrap();
    memoria::actualizar(&mut db, &m.id, Cambios { content: Some("dos".into()), ..Default::default() }, None).unwrap();
    memoria::guardar(&mut db, new_memory("/otro", Tipo::Fact, "B", "tres"), None).unwrap();
    let path = dir.join("o.db");
    let mut out = Vec::new();
    assert_eq!(omoikane::copia::exportar(&db, &path, &Ambito::de(&db, "/p").unwrap(), false, &mut out).unwrap().memories, 1);
    let lines: Vec<serde_json::Value> =
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // Header, the memory and the footer.
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1]["content"], "dos");
    assert_eq!(lines[1]["versions"][0]["content"], "uno");
    let mut all = Vec::new();
    assert_eq!(omoikane::copia::exportar(&db, &path, &Ambito::Todos, false, &mut all).unwrap().memories, 2);
}

/// After hundreds of random operations, the table, the FTS index and the paths still add up, and
/// each memory is found by its unique word.
#[test]
fn consistency_after_random_operations() {
    let (_dir, mut db) = open_db("coherencia");
    let mut r = 0x2545F4914F6CDD1Du64;
    let mut rand = move |n: u64| {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r % n
    };
    let mut alive: Vec<(String, u64)> = Vec::new();
    for i in 0..400u64 {
        match rand(10) {
            0..=4 => {
                let mut n = new_memory("/p", Tipo::TODOS[rand(4) as usize], &format!("marca{i} título"), "contenido común");
                n.paths = (0..rand(3)).map(|k| format!("src/f{k}.rs")).collect();
                n.tags = (0..rand(3)).map(|k| format!("t{k}")).collect();
                alive.push((memoria::guardar(&mut db, n, None).unwrap().id, i));
            }
            5..=7 if !alive.is_empty() => {
                let k = rand(alive.len() as u64) as usize;
                let mark = alive[k].1;
                let c = Cambios {
                    content: Some(format!("cambiado {i}")),
                    paths: Some((0..rand(3)).map(|k| format!("lib/g{k}.rs")).collect()),
                    title: Some(format!("marca{mark} retitulada")),
                    ..Default::default()
                };
                memoria::actualizar(&mut db, &alive[k].0, c, None).unwrap();
            }
            8..=9 if !alive.is_empty() => {
                let k = rand(alive.len() as u64) as usize;
                memoria::olvidar(&mut db, &alive.remove(k).0).unwrap();
            }
            _ => {}
        }
    }
    let n = count(&db, "SELECT count(*) FROM memory");
    assert_eq!(n as usize, alive.len());
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts_docsize"), n);
    assert_eq!(
        count(&db, "SELECT count(*) FROM memory_path"),
        count(&db, "SELECT COALESCE(sum(json_array_length(paths)), 0) FROM memory")
    );
    assert_eq!(count(&db, "SELECT count(*) FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory)"), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts_docsize WHERE id NOT IN (SELECT n FROM memory)"), 0);
    for (id, mark) in &alive {
        let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
        let r = buscar::buscar(&mut db, &format!("marca{mark}"), &o, None).unwrap();
        assert_eq!(r.first().map(|r| r.memoria.id.as_str()), Some(id.as_str()), "marca{mark}");
    }
}

/// Four connections (like four agent sessions) writing at once to the same file:
/// no SQLITE_BUSY must reach this far.
#[test]
fn concurrent_writes() {
    let (dir, db0) = open_db("concurrencia");
    drop(db0);
    let path = dir.join("o.db");
    let threads: Vec<_> = (0..4)
        .map(|h| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut db = db::abrir(&path).unwrap();
                for i in 0..50 {
                    let m = memoria::guardar(&mut db, new_memory("/p", Tipo::Fact, &format!("h{h} n{i}"), "texto"), None).unwrap();
                    if i % 5 == 0 {
                        let c = Cambios { content: Some(format!("otro {i}")), ..Default::default() };
                        memoria::actualizar(&mut db, &m.id, c, None).unwrap();
                    }
                }
            })
        })
        .collect();
    for h in threads {
        h.join().unwrap();
    }
    let db = db::abrir(&path).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory"), 200);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_fts_docsize"), 200);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_version"), 40);
}

/// The viewer (`omoi tui`) drawn in memory: list, search, pin, history and forget.
#[test]
fn viewer() {
    use omoikane::tui::{Estado, Vista, dibujar};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut db) = open_viewer_db();
    for (t, c) in [("Puerto del servidor", "Escucha en el 8010"), ("Pasarela de pago", "Redsys y no Stripe")] {
        omoikane::memoria::guardar(&mut db, omoikane::memoria::Nueva { project: "/p".into(), kind: Some(omoikane::memoria::Tipo::Fact), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
    }
    let mut e = Estado::nuevo("/p".into());
    e.cargar(&mut db, None).unwrap();
    assert_eq!(e.memorias.len(), 2);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    let screen = |t: &mut Terminal<TestBackend>, e: &mut Estado| {
        t.draw(|f| dibujar(f, e)).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    assert!(screen(&mut t, &mut e).contains("Pasarela de pago"));
    let key = |e: &mut Estado, db: &mut rusqlite::Connection, c: KeyCode| {
        if e.tecla(KeyEvent::new(c, KeyModifiers::NONE), db, None).unwrap() {
            e.cargar(db, None).unwrap();
        }
    };
    // Search: only the port one is left.
    key(&mut e, &mut db, KeyCode::Char('/'));
    for c in "puerto".chars() {
        key(&mut e, &mut db, KeyCode::Char(c));
    }
    assert_eq!(e.memorias.len(), 1);
    assert_eq!(e.memorias[0].title, "Puerto del servidor");
    // Pin and see that it stays pinned. Pinning is not a content change: it leaves no previous version.
    key(&mut e, &mut db, KeyCode::Enter);
    key(&mut e, &mut db, KeyCode::Char('p'));
    assert!(e.memorias[0].pinned);
    key(&mut e, &mut db, KeyCode::Char('h'));
    assert_eq!(e.vista, Vista::Historial);
    assert!(e.historial.is_empty());
    assert!(screen(&mut t, &mut e).contains("Sin versiones anteriores") || screen(&mut t, &mut e).contains("No previous versions"));
    key(&mut e, &mut db, KeyCode::Esc);
    // Forgetting asks for confirmation: «n» doesn't delete, «s» does.
    key(&mut e, &mut db, KeyCode::Char('d'));
    key(&mut e, &mut db, KeyCode::Char('n'));
    assert_eq!(e.memorias.len(), 1);
    key(&mut e, &mut db, KeyCode::Char('d'));
    key(&mut e, &mut db, KeyCode::Char('s'));
    assert!(e.memorias.is_empty());
}

fn open_viewer_db() -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva("visor");
    let db = omoikane::db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}
