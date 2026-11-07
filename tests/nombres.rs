//! Nombres de proyecto tolerantes (1.8.0): `proyecto::buscar` con acentos, separadores y una letra
//! de más o de menos; la revisión de proyectos duplicados; el recall con un nombre mal dictado; y los
//! worktrees de un repositorio sin `origin`.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::nombres::{self, Propuesta};
use omoikane::proyecto;
use omoikane::recall::{self, Entrada};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn base(nombre: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("nombres-{nombre}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}

/// Una carpeta de proyecto (sin git) dentro de la temporal, en forma canónica.
fn carpeta(dir: &Path, nombre: &str) -> String {
    let c = dir.join("p").join(nombre);
    std::fs::create_dir_all(&c).unwrap();
    proyecto::de(&c).unwrap()
}

fn guardar(db: &mut Connection, project: &str, title: &str, content: &str) -> String {
    let n = Nueva { project: project.into(), kind: Some(Tipo::Fact), title: title.into(), content: content.into(), ..Default::default() };
    memoria::guardar(db, n, None).unwrap().id
}

fn id_de(db: &Connection, ruta: &str) -> i64 {
    db.query_row("SELECT project_id FROM project_path WHERE path = ?1", [ruta], |f| f.get(0)).unwrap()
}

/// Las propuestas, sin tomar por temporales las carpetas de los tests (viven en `%TEMP%`): solo las que
/// tienen `subidas` en la ruta.
fn propuestas(db: &Connection) -> Vec<Propuesta> {
    nombres::propuestas_con(db, &Ambito::Todos, &|r: &str| r.contains("subidas")).unwrap()
}

fn parejas(v: &[Propuesta]) -> Vec<(&'static str, String, String)> {
    v.iter().map(|p| (p.kind, p.project.clone(), p.other.clone())).collect()
}

#[test]
fn buscar_ignora_acentos_separadores_y_una_letra() {
    let (dir, mut db) = base("buscar");
    let cronicas = carpeta(&dir, "Crónicas");
    let social = carpeta(&dir, "social-publish");
    let omoikane = carpeta(&dir, "Omoikane");
    guardar(&mut db, &cronicas, "Nota", "Una nota de crónicas.");
    guardar(&mut db, &social, "Nota", "Una nota de social.");
    guardar(&mut db, &omoikane, "Nota", "Una nota de omoikane.");
    let (c, s, o) = (id_de(&db, &cronicas), id_de(&db, &social), id_de(&db, &omoikane));
    assert_eq!(proyecto::buscar(&db, "cronicas").unwrap(), c, "sin acento");
    assert_eq!(proyecto::buscar(&db, "Social Publish").unwrap(), s, "con espacio");
    assert_eq!(proyecto::buscar(&db, "social_publish").unwrap(), s, "con guion bajo");
    assert_eq!(proyecto::buscar(&db, "Omoikae").unwrap(), o, "a una letra, si nada más encaja");
    assert_eq!(proyecto::buscar(&db, "omoik").unwrap(), o, "un trozo sigue valiendo");
    assert!(proyecto::buscar(&db, "Omoi-kamo").is_err(), "a dos letras, no");
}

#[test]
fn buscar_sigue_preguntando_si_hay_varios() {
    let (dir, mut db) = base("varios");
    let a = carpeta(&dir, "social-publish");
    let b = carpeta(&dir, "social_publish");
    guardar(&mut db, &a, "Nota", "Una.");
    guardar(&mut db, &b, "Nota", "Otra.");
    // Escrito tal cual, elige ese (la regla de la 1.7.2).
    assert_eq!(proyecto::buscar(&db, "social_publish").unwrap(), id_de(&db, &b));
    assert_eq!(proyecto::buscar(&db, "social-publish").unwrap(), id_de(&db, &a));
    // Escrito de otra forma que encaja con los dos, pregunta.
    let e = proyecto::buscar(&db, "Social Publish").unwrap_err().to_string();
    assert!(e.contains("several") || e.contains("varios"), "{e}");
    // Dos a una letra, también.
    let (dir2, mut db2) = base("varios-letra");
    let x = carpeta(&dir2, "tokimichi");
    let y = carpeta(&dir2, "tokimicha");
    guardar(&mut db2, &x, "Nota", "Una.");
    guardar(&mut db2, &y, "Nota", "Otra.");
    assert!(proyecto::buscar(&db2, "tokimichu").is_err());
}

#[test]
fn revision_propone_el_mismo_nombre_y_nunca_junta() {
    let (dir, mut db) = base("mismo");
    let grande = carpeta(&dir, "Omoikane");
    let pequeno = dir.join("q").join("omoikane");
    std::fs::create_dir_all(&pequeno).unwrap();
    let pequeno = proyecto::de(&pequeno).unwrap();
    for i in 0..4 {
        guardar(&mut db, &grande, &format!("Nota {i}"), "Del repositorio.");
    }
    guardar(&mut db, &pequeno, "Campaña", "De la campaña.");
    let v = propuestas(&db);
    assert_eq!(parejas(&v), vec![(nombres::MISMO_NOMBRE, "omoikane".to_string(), "Omoikane".to_string())], "{v:?}");
    // La orden ya escrita, con cada proyecto nombrado de forma que `buscar` lo encuentre a él.
    assert_eq!(v[0].command, "omoi projects merge omoikane Omoikane");
    assert_eq!(v[0].dismiss, "omoi projects distinct omoikane Omoikane");
    // Nada se ha juntado.
    assert_ne!(id_de(&db, &grande), id_de(&db, &pequeno));
    // «No son el mismo»: no vuelve a salir.
    nombres::marcar_distintos(&db, id_de(&db, &pequeno), Some(id_de(&db, &grande))).unwrap();
    assert!(propuestas(&db).is_empty());
}

#[test]
fn a_una_letra_solo_con_otra_senal() {
    let (dir, mut db) = base("letra");
    // Sin otra señal: dos proyectos vivos, con memorias y de la misma época, no se proponen.
    let a = carpeta(&dir, "Tokimichi");
    let b = carpeta(&dir, "Tokimicha");
    for i in 0..5 {
        guardar(&mut db, &a, &format!("A {i}"), "Uno.");
        guardar(&mut db, &b, &format!("B {i}"), "Otro.");
    }
    // Un nombre dentro de otro no es parecido: nunca.
    let web = carpeta(&dir, "tokimichi-web");
    guardar(&mut db, &web, "Web", "La web.");
    assert!(propuestas(&db).is_empty(), "{:?}", propuestas(&db));
    // Con una carpeta que ya no existe, sí.
    let c = carpeta(&dir, "Omoikane");
    for i in 0..5 {
        guardar(&mut db, &c, &format!("C {i}"), "Tres.");
    }
    let ido = carpeta(&dir, "Omoikae");
    guardar(&mut db, &ido, "Mal escrito", "Una.");
    std::fs::remove_dir_all(&ido).unwrap();
    let v = propuestas(&db);
    assert_eq!(parejas(&v), vec![(nombres::UNA_LETRA, "Omoikae".to_string(), "Omoikane".to_string())], "{v:?}");
    assert!(v[0].detail.contains("no longer exists"), "{}", v[0].detail);
}

#[test]
fn a_una_letra_con_un_nuevo_casi_vacio() {
    let (dir, mut db) = base("nuevo");
    let viejo = carpeta(&dir, "Narrador");
    for i in 0..6 {
        guardar(&mut db, &viejo, &format!("N {i}"), "Del narrador.");
    }
    // El nuevo se crea después (con otra marca de tiempo) y tiene una sola memoria.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let nuevo = carpeta(&dir, "Narradr");
    guardar(&mut db, &nuevo, "Una", "Sola.");
    let v = propuestas(&db);
    assert_eq!(parejas(&v), vec![(nombres::UNA_LETRA, "Narradr".to_string(), "Narrador".to_string())], "{v:?}");
}

#[test]
fn carpeta_ida_continuada_por_otro() {
    let (dir, mut db) = base("continua");
    let viejo = carpeta(&dir, "Narrador");
    guardar(&mut db, &viejo, "Narrador: lector de textos", "La app lee en voz alta.");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let nuevo = carpeta(&dir, "Aloud");
    guardar(&mut db, &nuevo, "Aloud", "Es la app que antes se llamaba Narrador, con otro nombre.");
    // Otro proyecto vivo que no lo nombra no cuenta.
    let otro = carpeta(&dir, "Balua");
    guardar(&mut db, &otro, "Balua", "Fotos de platos.");
    assert!(propuestas(&db).is_empty(), "mientras la carpeta existe, nada");
    std::fs::remove_dir_all(&viejo).unwrap();
    let v = propuestas(&db);
    assert_eq!(parejas(&v), vec![(nombres::CONTINUA, "Narrador".to_string(), "Aloud".to_string())], "{v:?}");
    assert_eq!(v[0].command, "omoi projects merge Narrador Aloud");
}

#[test]
fn carpeta_de_subidas_no_es_un_proyecto() {
    let (dir, mut db) = base("subidas");
    let up = dir.join("subidas").join("d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39");
    std::fs::create_dir_all(&up).unwrap();
    let up = proyecto::de(&up).unwrap();
    guardar(&mut db, &up, "Algo subido", "Una nota que no es de esa carpeta.");
    let v = propuestas(&db);
    assert_eq!(parejas(&v), vec![(nombres::TEMPORAL, "d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39".to_string(), String::new())], "{v:?}");
    assert!(v[0].detail.contains("--global"), "{}", v[0].detail);
    // «Es un proyecto de verdad».
    nombres::marcar_distintos(&db, id_de(&db, &up), None).unwrap();
    assert!(propuestas(&db).is_empty());
}

#[test]
fn el_aviso_sale_al_empezar_una_vez_al_dia() {
    let (dir, mut db) = base("aviso");
    // Una carpeta temporal de verdad (`%TEMP%`): el aviso usa `es_temporal`.
    let up = carpeta(&dir, "d35c0488");
    guardar(&mut db, &up, "Algo subido", "Una nota.");
    let a = nombres::aviso_al_empezar(&db, &Ambito::Todos).unwrap().expect("aviso");
    assert!(a.contains("omoi projects distinct") && a.contains("d35c0488"), "{a}");
    assert!(nombres::aviso_al_empezar(&db, &Ambito::Todos).unwrap().is_none(), "una vez al día");
    // Y en el bloque del inicio de sesión, por la revisión.
    let (dir2, mut db2) = base("aviso-sesion");
    let up2 = carpeta(&dir2, "subida");
    guardar(&mut db2, &up2, "Algo subido", "Una nota.");
    let home = std::env::home_dir().unwrap();
    let t = recall::al_empezar(&mut db2, &proyecto::de(&home).unwrap(), &Entrada { session_id: "s".into(), ..Default::default() }).unwrap().unwrap_or_default();
    assert!(t.contains("Projects review") && t.contains("omoi projects distinct"), "{t}");
}

#[test]
fn el_recall_entiende_un_nombre_mal_dictado() {
    let (dir, mut db) = base("recall");
    let troba = carpeta(&dir, "Troba");
    let toki = carpeta(&dir, "Tokimichi");
    guardar(&mut db, &troba, "Troba publica sus binarios en troba-releases", "El instalador de Troba baja la versión de escritorio del repositorio de releases.");
    guardar(&mut db, &toki, "Tokimichi: el calendario va en la hoja de config", "Las fechas de Tokimichi se leen de la hoja de configuración.");
    for i in 0..8 {
        guardar(&mut db, "", &format!("Relleno {i}"), &format!("Una nota global cualquiera sobre el tema {i} y otras cosas."));
    }
    let home = proyecto::de(&std::env::home_dir().unwrap()).unwrap();
    let pedir = |db: &mut Connection, sesion: &str, prompt: &str| {
        recall::al_preguntar(db, &home, &Entrada { session_id: sesion.into(), prompt: prompt.into(), ..Default::default() }, None).unwrap().unwrap_or_default()
    };
    let t = pedir(&mut db, "a", "dónde publica trova los binarios del instalador");
    assert!(t.contains("troba-releases"), "trova → Troba: {t}");
    let t = pedir(&mut db, "b", "en tokimicho de dónde salen las fechas del calendario");
    assert!(t.contains("hoja de config"), "tokimicho → Tokimichi: {t}");
    // Una palabra que sí está en alguna memoria no se toca, aunque esté a una letra de un nombre.
    let terminos: Vec<String> = ["trova", "tokimicho", "onmoikane", "socialpublish"].iter().map(|t| t.to_string()).collect();
    let arreglos = nombres::mal_escritos(&db, &terminos, &[false, true, true, true]).unwrap();
    assert_eq!(arreglos, vec![(1, vec!["tokimichi".to_string()])], "trova está en alguna memoria; onmoikane y socialpublish no son proyectos de esta base");
    // Un nombre con separadores, escrito junto o a una letra, da sus palabras.
    let sp = carpeta(&dir, "social-publish");
    guardar(&mut db, &sp, "Social", "Publica en redes.");
    let arreglos = nombres::mal_escritos(&db, &terminos, &[false, false, false, true]).unwrap();
    assert_eq!(arreglos, vec![(3, vec!["social".to_string(), "publish".to_string()])]);
    // A una letra de dos proyectos a la vez, ninguno.
    let toka = carpeta(&dir, "Tokimicha");
    guardar(&mut db, &toka, "Otro", "Otro.");
    assert!(nombres::mal_escritos(&db, &terminos, &[false, true, false, false]).unwrap().is_empty());
}

/// Dos worktrees de un repositorio sin `origin`, en carpetas con nombres distintos, son el mismo
/// proyecto (por la carpeta git común), y con la clave del repositorio.
#[test]
fn worktrees_sin_remoto_son_un_proyecto() {
    let (dir, mut db) = base("worktrees");
    let principal = dir.join("Kumi");
    std::fs::create_dir_all(principal.join(".git").join("worktrees").join("rama-a")).unwrap();
    std::fs::create_dir_all(principal.join(".git").join("worktrees").join("rama-b")).unwrap();
    let worktree = |nombre: &str, rama: &str| -> String {
        let w = dir.join(nombre);
        std::fs::create_dir_all(&w).unwrap();
        let gitdir: PathBuf = principal.join(".git").join("worktrees").join(rama);
        std::fs::write(w.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        proyecto::de(&w).unwrap()
    };
    let a = worktree("kumi-feature", "rama-a");
    let b = worktree("otra-cosa", "rama-b");
    // Primero los dos worktrees, sin la principal apuntada: el mismo proyecto, con la clave del repositorio.
    guardar(&mut db, &a, "Desde A", "Una.");
    guardar(&mut db, &b, "Desde B", "Otra.");
    assert_eq!(id_de(&db, &a), id_de(&db, &b));
    let persona = db::persona_de(&db);
    assert_eq!(proyecto::clave_de(&db, id_de(&db, &a)).unwrap(), format!("dir:{persona}/kumi"));
    // Después la carpeta principal: también.
    let p = proyecto::de(&principal).unwrap();
    guardar(&mut db, &p, "Desde la principal", "Tres.");
    assert_eq!(id_de(&db, &p), id_de(&db, &a));
    // Otra carpeta que se llama igual pero es otro repositorio sigue siendo otro proyecto.
    let otra = dir.join("x").join("Kumi");
    std::fs::create_dir_all(otra.join(".git")).unwrap();
    let otra = proyecto::de(&otra).unwrap();
    guardar(&mut db, &otra, "Otro Kumi", "Cuatro.");
    assert_ne!(id_de(&db, &otra), id_de(&db, &p));
}

/// La principal apuntada antes (como en las bases de antes de la 1.8): un worktree nuevo se une a ella,
/// y la clave ya guardada no cambia.
#[test]
fn worktree_nuevo_de_una_principal_ya_apuntada() {
    let (dir, mut db) = base("worktree-viejo");
    let principal = dir.join("repo");
    let gitdir = principal.join(".git").join("worktrees").join("w");
    std::fs::create_dir_all(&gitdir).unwrap();
    let p = proyecto::de(&principal).unwrap();
    guardar(&mut db, &p, "Principal", "Una.");
    let clave = proyecto::clave_de(&db, id_de(&db, &p)).unwrap();
    let w = dir.join("nombre-distinto");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(w.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    let w = proyecto::de(&w).unwrap();
    guardar(&mut db, &w, "Worktree", "Dos.");
    assert_eq!(id_de(&db, &w), id_de(&db, &p));
    assert_eq!(proyecto::clave_de(&db, id_de(&db, &p)).unwrap(), clave);
}
