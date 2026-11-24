//! 1.9.0: the TUI (`omoi tui`) knows about spaces and the trash, with the same isolation as the web
//! (`tests/web_isolation.rs`): from a client's project, neither the list, nor «all», nor the space
//! filter, nor the trash show another client's memories, and another client's ones cannot be restored or
//! deleted even with their id. From the user's folder everything is visible and filtered by space.

mod common;

use omoikane::memory::{self, NewMemory, Kind};
use omoikane::tui::{State, View, draw};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use rusqlite::Connection;

/// When destructuring it, `let Fixture { _dir, mut db }` with `_dir` first: a pattern's variables are
/// dropped in reverse declaration order, and the database must be closed before deleting its folder (on
/// Windows an open file can't be deleted and the folder stayed in the temp dir).
struct Fixture {
    db: Connection,
    _dir: common::TempDir,
}

/// Acme (two projects and a space memory), Globex, a project without a space and a global
/// preference; one forgotten memory in Acme and another in Globex.
fn fixture(name: &str) -> Fixture {
    let dir = common::TempDir::new(name);
    let mut db = omoikane::db::open(&dir.join("o.db")).unwrap();
    let save = |db: &mut Connection, project: &str, space_id: Option<i64>, kind: Kind, title: &str| {
        let n = NewMemory { project: project.into(), space_id, kind: Some(kind), title: title.into(), content: format!("{title}."), ..Default::default() };
        memory::save(db, n, None).unwrap().id
    };
    save(&mut db, "", None, Kind::Preference, "Respuestas cortas");
    let acme = omoikane::project::create_space(&db, "Acme").unwrap();
    let globex = omoikane::project::create_space(&db, "Globex").unwrap();
    save(&mut db, "/acme-api", None, Kind::Fact, "Acme usa Postgres");
    save(&mut db, "/acme-web", None, Kind::Fact, "Acme web en Astro");
    let trashed_acme = save(&mut db, "/acme-web", None, Kind::Fact, "Acme web: tirar");
    save(&mut db, "", Some(acme), Kind::Decision, "Acme: norma del cliente");
    save(&mut db, "/globex", None, Kind::Fact, "Globex es Flutter");
    let trashed_globex = save(&mut db, "/globex", None, Kind::Fact, "Globex: tirar");
    save(&mut db, "/notas", None, Kind::Fact, "Idea de blog");
    for (p, e) in [("/acme-api", acme), ("/acme-web", acme), ("/globex", globex)] {
        let id = omoikane::project::register(&db, p).unwrap();
        omoikane::project::assign_space(&db, id, Some(e)).unwrap();
    }
    memory::forget(&mut db, &trashed_acme).unwrap();
    memory::forget(&mut db, &trashed_globex).unwrap();
    Fixture { db, _dir: dir }
}

fn forgotten_id(db: &Connection, title: &str) -> String {
    memory::trash(db).unwrap().into_iter().find(|o| o.title == title).unwrap().id
}

fn titles(e: &State) -> Vec<String> {
    e.memories.iter().map(|m| m.title.clone()).collect()
}

fn press(e: &mut State, db: &mut Connection, c: KeyCode) {
    if e.key(KeyEvent::new(c, KeyModifiers::NONE), db, None).unwrap() {
        e.load(db, None).unwrap();
    }
}

/// The screen as text, one line per row.
fn screen(e: &mut State, width: u16, height: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(|f| draw(f, e)).unwrap();
    let cells: Vec<String> = t.backend().buffer().content().iter().map(|c| c.symbol().to_string()).collect();
    cells.chunks(width as usize).map(|f| f.concat().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

fn assert_no_others(what: &str, text: &str) {
    for x in ["Globex", "globex", "Idea de blog"] {
        assert!(!text.contains(x), "{what} enseña «{x}» desde acme:\n{text}");
    }
}

fn home() -> String {
    omoikane::project::of_dir(&omoikane::project::local_home().unwrap()).unwrap()
}

#[test]
fn other_client_not_visible_from_a_client_in_tui() {
    let Fixture { _dir, mut db } = fixture("tuiaisla");
    let mut e = State::new("/acme-api".into());
    e.load(&mut db, None).unwrap();
    let t = titles(&e);
    assert!(t.contains(&"Acme usa Postgres".into()) && t.contains(&"Respuestas cortas".into()) && t.contains(&"Acme: norma del cliente".into()), "{t:?}");
    assert!(!t.contains(&"Acme web en Astro".into()), "solo este proyecto: {t:?}");
    assert_no_others("la lista", &t.join("\n"));

    // «All» from an Acme project is all of Acme, never everything (before 1.9 it was `Scope::All`).
    press(&mut e, &mut db, KeyCode::Char('a'));
    let t = titles(&e);
    assert!(t.contains(&"Acme web en Astro".into()), "{t:?}");
    assert_no_others("«todos»", &t.join("\n"));
    let p = screen(&mut e, 120, 40);
    assert!(p.contains("Acme"), "{p}");
    assert_no_others("la pantalla", &p);
    // The same when started with `--all`.
    let mut all_view = State::new("/acme-api".into());
    all_view.all = true;
    all_view.load(&mut db, None).unwrap();
    assert_eq!(titles(&all_view), titles(&e));

    // The space filter only offers its own.
    assert_eq!(e.spaces.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert_eq!(e.space_filters(&db).unwrap().len(), 1);
    press(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.space.is_some());
    assert_no_others("el filtro", &titles(&e).join("\n"));
    press(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.space.is_none(), "tras el único filtro, sin filtro");

    // The trash: only what was forgotten in Acme.
    press(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.view, View::Trash);
    let t: Vec<&str> = e.trash.iter().map(|o| o.forgotten.title.as_str()).collect();
    assert_eq!(t, ["Acme web: tirar"]);
    assert_no_others("la papelera", &screen(&mut e, 120, 40));

    // Globex's ones are not restored or deleted even with their id.
    let globex = forgotten_id(&db, "Globex: tirar");
    assert!(!e.restore(&mut db, &globex, None).unwrap());
    assert!(e.notice.contains("espacio") || e.notice.contains("space"), "{}", e.notice);
    assert_eq!(e.delete_from_trash(&mut db, std::slice::from_ref(&globex)).unwrap(), 0);
    assert!(memory::trash(&db).unwrap().iter().any(|o| o.id == globex));

    // Emptying from Acme empties Acme's and leaves Globex's.
    press(&mut e, &mut db, KeyCode::Char('E'));
    assert_eq!(e.view, View::TrashEmpty);
    press(&mut e, &mut db, KeyCode::Char('s'));
    assert_eq!(e.view, View::Trash);
    assert!(e.trash.is_empty());
    let remaining: Vec<String> = memory::trash(&db).unwrap().into_iter().map(|o| o.title).collect();
    assert_eq!(remaining, ["Globex: tirar"]);
}

#[test]
fn project_without_space_sees_no_client() {
    let Fixture { _dir, mut db } = fixture("tuisinesp");
    let mut e = State::new("/notas".into());
    e.all = true;
    e.load(&mut db, None).unwrap();
    let t = titles(&e).join("\n");
    assert!(t.contains("Idea de blog") && t.contains("Respuestas cortas"), "{t}");
    assert!(!t.contains("Acme") && !t.contains("Globex"), "{t}");
    assert!(e.spaces.is_empty());
    press(&mut e, &mut db, KeyCode::Char('t'));
    assert!(e.trash.is_empty(), "lo olvidado de los clientes no sale");
}

#[test]
fn user_folder_filters_by_space() {
    let Fixture { _dir, mut db } = fixture("tuifiltro");
    let mut e = State::new(home());
    e.all = true;
    e.load(&mut db, None).unwrap();
    assert!(e.sees_all);
    assert_eq!(e.memories.len(), 6, "{:?}", titles(&e));
    assert_eq!(e.space_filters(&db).unwrap().len(), 3, "acme, globex y sin espacio");

    press(&mut e, &mut db, KeyCode::Char('e'));
    let t = titles(&e).join("\n");
    assert!(t.contains("Acme usa Postgres") && t.contains("Acme web en Astro") && t.contains("Respuestas cortas"), "{t}");
    assert!(!t.contains("Globex") && !t.contains("Idea de blog"), "{t}");
    let p = screen(&mut e, 120, 40);
    assert!(p.contains("◇Acme"), "la memoria del espacio lleva su marca:\n{p}");

    press(&mut e, &mut db, KeyCode::Char('e'));
    let t = titles(&e).join("\n");
    assert!(t.contains("Globex es Flutter") && !t.contains("Acme") && !t.contains("Idea de blog"), "{t}");
    press(&mut e, &mut db, KeyCode::Char('t'));
    let t: Vec<&str> = e.trash.iter().map(|o| o.forgotten.title.as_str()).collect();
    assert_eq!(t, ["Globex: tirar"], "la papelera con el filtro de Globex");
    press(&mut e, &mut db, KeyCode::Esc);

    press(&mut e, &mut db, KeyCode::Char('e'));
    let t = titles(&e).join("\n");
    assert!(t.contains("Idea de blog") && t.contains("Respuestas cortas") && !t.contains("Acme") && !t.contains("Globex"), "{t}");
    press(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.space.is_none());
    assert_eq!(e.memories.len(), 6);
    press(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.trash.len(), 2, "sin filtro, toda la papelera");
}

#[test]
fn restore_and_purge_from_trash() {
    let Fixture { _dir, mut db } = fixture("tuipapel");
    let mut e = State::new("/acme-web".into());
    e.load(&mut db, None).unwrap();
    assert!(!titles(&e).contains(&"Acme web: tirar".into()));
    // Restore: it goes back to the list and leaves the trash.
    press(&mut e, &mut db, KeyCode::Char('t'));
    press(&mut e, &mut db, KeyCode::Enter);
    assert!(e.notice.contains("Acme web: tirar"), "{}", e.notice);
    assert!(e.trash.is_empty());
    assert!(titles(&e).contains(&"Acme web: tirar".into()));
    // Forget it again (it goes to the trash) and delete it for good: «n» doesn't delete, «s» does.
    press(&mut e, &mut db, KeyCode::Esc);
    let i = titles(&e).iter().position(|t| t == "Acme web: tirar").unwrap();
    e.list.select(Some(i));
    press(&mut e, &mut db, KeyCode::Char('d'));
    press(&mut e, &mut db, KeyCode::Char('s'));
    press(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.trash.len(), 1);
    press(&mut e, &mut db, KeyCode::Char('x'));
    assert_eq!(e.view, View::TrashDelete);
    press(&mut e, &mut db, KeyCode::Char('n'));
    assert_eq!(e.view, View::Trash);
    assert_eq!(e.trash.len(), 1);
    press(&mut e, &mut db, KeyCode::Char('x'));
    press(&mut e, &mut db, KeyCode::Char('s'));
    assert!(e.trash.is_empty());
    assert!(memory::trash(&db).unwrap().iter().all(|o| o.title != "Acme web: tirar"));
    assert!(memory::trash(&db).unwrap().iter().any(|o| o.title == "Globex: tirar"), "lo de Globex sigue");
}

/// The new screens fit in 80×24, in 120×40 and in a small terminal. With `--nocapture` they are shown.
#[test]
fn screens_fit_small_terminals() {
    let Fixture { _dir, mut db } = fixture("tuipantallas");
    let mut e = State::new(home());
    e.all = true;
    e.load(&mut db, None).unwrap();
    for (width, height) in [(80, 24), (120, 40), (50, 16), (40, 12), (20, 6)] {
        e.view = View::List;
        let list = screen(&mut e, width, height);
        e.view = View::Trash;
        let trash = screen(&mut e, width, height);
        println!("--- lista {width}x{height} ---\n{list}\n--- papelera {width}x{height} ---\n{trash}");
        if width >= 80 {
            // The whole help, even if it takes two rows, and the space column.
            assert!(list.contains("q salir") || list.contains("q quit"), "{list}");
            assert!(list.contains("Globex"), "{list}");
            assert!(trash.contains("E vaciar") || trash.contains("E empty"), "{trash}");
        }
        if width >= 40 {
            assert!(trash.contains("tirar"), "{trash}");
        }
    }
    e.view = View::Trash;
    e.trash_list.select(Some(0));
    press(&mut e, &mut db, KeyCode::Char('x'));
    let confirm = screen(&mut e, 80, 24);
    println!("--- borrar del todo 80x24 ---\n{confirm}");
    assert!(confirm.contains("s / n") || confirm.contains("y / n"), "{confirm}");
}

/// `omoi trash` and `omoi restore` look at the trash with `http::trash_from`: from a client, another
/// client's forgotten memories are neither listed nor restored.
#[test]
fn cli_trash_does_not_show_other_client() {
    let b = fixture("papelera-cli");
    let (from_acme, sees_all) = omoikane::http::trash_from(&b.db, "/acme-web").unwrap();
    assert!(!sees_all);
    let t: Vec<&str> = from_acme.iter().map(|o| o.title.as_str()).collect();
    assert_eq!(t, vec!["Acme web: tirar"], "desde acme: {t:?}");
    let (from_home, sees_all) = omoikane::http::trash_from(&b.db, &home()).unwrap();
    assert!(sees_all);
    assert_eq!(from_home.len(), 2);
    let _ = forgotten_id(&b.db, "Globex: tirar");
}

/// `omoi restore` asks for at least 4 characters of the id, like `get` and `forget`.
#[test]
fn restore_needs_at_least_four_chars() {
    let mut b = fixture("restore-corto");
    let id = forgotten_id(&b.db, "Globex: tirar");
    assert!(memory::restore(&mut b.db, &id[..1], None).is_err());
    assert!(memory::restore(&mut b.db, &id[..3], None).is_err());
    assert!(memory::restore(&mut b.db, &id, None).is_ok());
}
