//! 1.9.0: la TUI (`omoi tui`) conoce los espacios y la papelera, con el mismo aislamiento que la web
//! (`tests/aislamiento_web.rs`): desde un proyecto de un cliente, ni la lista, ni «todos», ni el filtro
//! por espacio, ni la papelera enseñan lo de otro cliente, y lo de otro cliente no se recupera ni se
//! borra aunque se tenga su id. Desde la carpeta del usuario se ve todo y se filtra por espacio.

mod comun;

use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::tui::{Estado, Vista, dibujar};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use rusqlite::Connection;

struct Base {
    db: Connection,
    _dir: comun::Temporal,
}

/// Acme (dos proyectos y una memoria del espacio), Globex, un proyecto sin espacio y una preferencia
/// global; una memoria olvidada en Acme y otra en Globex.
fn base(nombre: &str) -> Base {
    let dir = comun::Temporal::nueva(nombre);
    let mut db = omoikane::db::abrir(&dir.join("o.db")).unwrap();
    let guardar = |db: &mut Connection, project: &str, space_id: Option<i64>, kind: Tipo, title: &str| {
        let n = Nueva { project: project.into(), space_id, kind: Some(kind), title: title.into(), content: format!("{title}."), ..Default::default() };
        memoria::guardar(db, n, None).unwrap().id
    };
    guardar(&mut db, "", None, Tipo::Preference, "Respuestas cortas");
    let acme = omoikane::proyecto::crear_espacio(&db, "Acme").unwrap();
    let globex = omoikane::proyecto::crear_espacio(&db, "Globex").unwrap();
    guardar(&mut db, "/acme-api", None, Tipo::Fact, "Acme usa Postgres");
    guardar(&mut db, "/acme-web", None, Tipo::Fact, "Acme web en Astro");
    let tirada_acme = guardar(&mut db, "/acme-web", None, Tipo::Fact, "Acme web: tirar");
    guardar(&mut db, "", Some(acme), Tipo::Decision, "Acme: norma del cliente");
    guardar(&mut db, "/globex", None, Tipo::Fact, "Globex es Flutter");
    let tirada_globex = guardar(&mut db, "/globex", None, Tipo::Fact, "Globex: tirar");
    guardar(&mut db, "/notas", None, Tipo::Fact, "Idea de blog");
    for (p, e) in [("/acme-api", acme), ("/acme-web", acme), ("/globex", globex)] {
        let id = omoikane::proyecto::registrar(&db, p).unwrap();
        omoikane::proyecto::asignar_espacio(&db, id, Some(e)).unwrap();
    }
    memoria::olvidar(&mut db, &tirada_acme).unwrap();
    memoria::olvidar(&mut db, &tirada_globex).unwrap();
    Base { db, _dir: dir }
}

fn id_olvidada(db: &Connection, titulo: &str) -> String {
    memoria::papelera(db).unwrap().into_iter().find(|o| o.title == titulo).unwrap().id
}

fn titulos(e: &Estado) -> Vec<String> {
    e.memorias.iter().map(|m| m.title.clone()).collect()
}

fn tecla(e: &mut Estado, db: &mut Connection, c: KeyCode) {
    if e.tecla(KeyEvent::new(c, KeyModifiers::NONE), db, None).unwrap() {
        e.cargar(db, None).unwrap();
    }
}

/// La pantalla como texto, una línea por fila.
fn pantalla(e: &mut Estado, ancho: u16, alto: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(ancho, alto)).unwrap();
    t.draw(|f| dibujar(f, e)).unwrap();
    let celdas: Vec<String> = t.backend().buffer().content().iter().map(|c| c.symbol().to_string()).collect();
    celdas.chunks(ancho as usize).map(|f| f.concat().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

fn sin_otros(que: &str, texto: &str) {
    for x in ["Globex", "globex", "Idea de blog"] {
        assert!(!texto.contains(x), "{que} enseña «{x}» desde acme:\n{texto}");
    }
}

fn casa() -> String {
    omoikane::proyecto::de(&omoikane::proyecto::casa_local().unwrap()).unwrap()
}

#[test]
fn desde_un_cliente_no_se_ve_otro_cliente_en_la_tui() {
    let Base { mut db, _dir } = base("tuiaisla");
    let mut e = Estado::nuevo("/acme-api".into());
    e.cargar(&mut db, None).unwrap();
    let t = titulos(&e);
    assert!(t.contains(&"Acme usa Postgres".into()) && t.contains(&"Respuestas cortas".into()) && t.contains(&"Acme: norma del cliente".into()), "{t:?}");
    assert!(!t.contains(&"Acme web en Astro".into()), "solo este proyecto: {t:?}");
    sin_otros("la lista", &t.join("\n"));

    // «Todos» desde un proyecto de Acme es Acme entero, nunca todo (antes de la 1.9 era `Ambito::Todos`).
    tecla(&mut e, &mut db, KeyCode::Char('a'));
    let t = titulos(&e);
    assert!(t.contains(&"Acme web en Astro".into()), "{t:?}");
    sin_otros("«todos»", &t.join("\n"));
    let p = pantalla(&mut e, 120, 40);
    assert!(p.contains("Acme"), "{p}");
    sin_otros("la pantalla", &p);
    // Lo mismo si se arranca con `--all`.
    let mut todos = Estado::nuevo("/acme-api".into());
    todos.todos = true;
    todos.cargar(&mut db, None).unwrap();
    assert_eq!(titulos(&todos), titulos(&e));

    // El filtro por espacio solo ofrece el suyo.
    assert_eq!(e.espacios.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert_eq!(e.filtros(&db).unwrap().len(), 1);
    tecla(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.espacio.is_some());
    sin_otros("el filtro", &titulos(&e).join("\n"));
    tecla(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.espacio.is_none(), "tras el único filtro, sin filtro");

    // La papelera: solo lo olvidado en Acme.
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.vista, Vista::Papelera);
    let t: Vec<&str> = e.papelera.iter().map(|o| o.olvidada.title.as_str()).collect();
    assert_eq!(t, ["Acme web: tirar"]);
    sin_otros("la papelera", &pantalla(&mut e, 120, 40));

    // Lo de Globex no se recupera ni se borra aunque se tenga su id.
    let globex = id_olvidada(&db, "Globex: tirar");
    assert!(!e.restaurar(&mut db, &globex, None).unwrap());
    assert!(e.aviso.contains("espacio") || e.aviso.contains("space"), "{}", e.aviso);
    assert_eq!(e.borrar_de_papelera(&mut db, std::slice::from_ref(&globex)).unwrap(), 0);
    assert!(memoria::papelera(&db).unwrap().iter().any(|o| o.id == globex));

    // Vaciar desde Acme vacía lo de Acme y deja lo de Globex.
    tecla(&mut e, &mut db, KeyCode::Char('E'));
    assert_eq!(e.vista, Vista::PapeleraVaciar);
    tecla(&mut e, &mut db, KeyCode::Char('s'));
    assert_eq!(e.vista, Vista::Papelera);
    assert!(e.papelera.is_empty());
    let quedan: Vec<String> = memoria::papelera(&db).unwrap().into_iter().map(|o| o.title).collect();
    assert_eq!(quedan, ["Globex: tirar"]);
}

#[test]
fn un_proyecto_sin_espacio_no_ve_ningun_cliente() {
    let Base { mut db, _dir } = base("tuisinesp");
    let mut e = Estado::nuevo("/notas".into());
    e.todos = true;
    e.cargar(&mut db, None).unwrap();
    let t = titulos(&e).join("\n");
    assert!(t.contains("Idea de blog") && t.contains("Respuestas cortas"), "{t}");
    assert!(!t.contains("Acme") && !t.contains("Globex"), "{t}");
    assert!(e.espacios.is_empty());
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    assert!(e.papelera.is_empty(), "lo olvidado de los clientes no sale");
}

#[test]
fn desde_la_carpeta_del_usuario_se_filtra_por_espacio() {
    let Base { mut db, _dir } = base("tuifiltro");
    let mut e = Estado::nuevo(casa());
    e.todos = true;
    e.cargar(&mut db, None).unwrap();
    assert!(e.ve_todo);
    assert_eq!(e.memorias.len(), 6, "{:?}", titulos(&e));
    assert_eq!(e.filtros(&db).unwrap().len(), 3, "acme, globex y sin espacio");

    tecla(&mut e, &mut db, KeyCode::Char('e'));
    let t = titulos(&e).join("\n");
    assert!(t.contains("Acme usa Postgres") && t.contains("Acme web en Astro") && t.contains("Respuestas cortas"), "{t}");
    assert!(!t.contains("Globex") && !t.contains("Idea de blog"), "{t}");
    let p = pantalla(&mut e, 120, 40);
    assert!(p.contains("◇Acme"), "la memoria del espacio lleva su marca:\n{p}");

    tecla(&mut e, &mut db, KeyCode::Char('e'));
    let t = titulos(&e).join("\n");
    assert!(t.contains("Globex es Flutter") && !t.contains("Acme") && !t.contains("Idea de blog"), "{t}");
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    let t: Vec<&str> = e.papelera.iter().map(|o| o.olvidada.title.as_str()).collect();
    assert_eq!(t, ["Globex: tirar"], "la papelera con el filtro de Globex");
    tecla(&mut e, &mut db, KeyCode::Esc);

    tecla(&mut e, &mut db, KeyCode::Char('e'));
    let t = titulos(&e).join("\n");
    assert!(t.contains("Idea de blog") && t.contains("Respuestas cortas") && !t.contains("Acme") && !t.contains("Globex"), "{t}");
    tecla(&mut e, &mut db, KeyCode::Char('e'));
    assert!(e.espacio.is_none());
    assert_eq!(e.memorias.len(), 6);
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.papelera.len(), 2, "sin filtro, toda la papelera");
}

#[test]
fn recuperar_y_borrar_del_todo_desde_la_papelera() {
    let Base { mut db, _dir } = base("tuipapel");
    let mut e = Estado::nuevo("/acme-web".into());
    e.cargar(&mut db, None).unwrap();
    assert!(!titulos(&e).contains(&"Acme web: tirar".into()));
    // Recuperar: vuelve a la lista y sale de la papelera.
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    tecla(&mut e, &mut db, KeyCode::Enter);
    assert!(e.aviso.contains("Acme web: tirar"), "{}", e.aviso);
    assert!(e.papelera.is_empty());
    assert!(titulos(&e).contains(&"Acme web: tirar".into()));
    // Olvidarla otra vez (va a la papelera) y borrarla del todo: «n» no borra, «s» sí.
    tecla(&mut e, &mut db, KeyCode::Esc);
    let i = titulos(&e).iter().position(|t| t == "Acme web: tirar").unwrap();
    e.lista.select(Some(i));
    tecla(&mut e, &mut db, KeyCode::Char('d'));
    tecla(&mut e, &mut db, KeyCode::Char('s'));
    tecla(&mut e, &mut db, KeyCode::Char('t'));
    assert_eq!(e.papelera.len(), 1);
    tecla(&mut e, &mut db, KeyCode::Char('x'));
    assert_eq!(e.vista, Vista::PapeleraBorrar);
    tecla(&mut e, &mut db, KeyCode::Char('n'));
    assert_eq!(e.vista, Vista::Papelera);
    assert_eq!(e.papelera.len(), 1);
    tecla(&mut e, &mut db, KeyCode::Char('x'));
    tecla(&mut e, &mut db, KeyCode::Char('s'));
    assert!(e.papelera.is_empty());
    assert!(memoria::papelera(&db).unwrap().iter().all(|o| o.title != "Acme web: tirar"));
    assert!(memoria::papelera(&db).unwrap().iter().any(|o| o.title == "Globex: tirar"), "lo de Globex sigue");
}

/// Las pantallas nuevas caben en 80×24, en 120×40 y en una terminal pequeña. Con `--nocapture` se ven.
#[test]
fn las_pantallas_caben_en_terminales_pequenas() {
    let Base { mut db, _dir } = base("tuipantallas");
    let mut e = Estado::nuevo(casa());
    e.todos = true;
    e.cargar(&mut db, None).unwrap();
    for (ancho, alto) in [(80, 24), (120, 40), (50, 16), (40, 12), (20, 6)] {
        e.vista = Vista::Lista;
        let lista = pantalla(&mut e, ancho, alto);
        e.vista = Vista::Papelera;
        let papelera = pantalla(&mut e, ancho, alto);
        println!("--- lista {ancho}x{alto} ---\n{lista}\n--- papelera {ancho}x{alto} ---\n{papelera}");
        if ancho >= 80 {
            // La ayuda entera, aunque ocupe dos filas, y la columna del espacio.
            assert!(lista.contains("q salir") || lista.contains("q quit"), "{lista}");
            assert!(lista.contains("Globex"), "{lista}");
            assert!(papelera.contains("E vaciar") || papelera.contains("E empty"), "{papelera}");
        }
        if ancho >= 40 {
            assert!(papelera.contains("tirar"), "{papelera}");
        }
    }
    e.vista = Vista::Papelera;
    e.lista_papelera.select(Some(0));
    tecla(&mut e, &mut db, KeyCode::Char('x'));
    let confirmar = pantalla(&mut e, 80, 24);
    println!("--- borrar del todo 80x24 ---\n{confirmar}");
    assert!(confirmar.contains("s / n") || confirmar.contains("y / n"), "{confirmar}");
}
