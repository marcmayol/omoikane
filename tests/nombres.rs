//! Tolerant project names (1.8.0): `proyecto::buscar` with accents, separators and one letter
//! too many or too few; the review of duplicate projects; the recall with a misdictated name; and the
//! worktrees of a repository without `origin`.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::nombres::{self, Propuesta};
use omoikane::proyecto;
use omoikane::recall::{self, Entrada};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn fresh_db(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("nombres-{name}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}

/// A project folder (without git) inside the temporary one, in canonical form.
fn project_dir(dir: &Path, name: &str) -> String {
    let c = dir.join("p").join(name);
    std::fs::create_dir_all(&c).unwrap();
    proyecto::de(&c).unwrap()
}

fn save(db: &mut Connection, project: &str, title: &str, content: &str) -> String {
    let n = Nueva { project: project.into(), kind: Some(Tipo::Fact), title: title.into(), content: content.into(), ..Default::default() };
    memoria::guardar(db, n, None).unwrap().id
}

fn id_of(db: &Connection, path: &str) -> i64 {
    db.query_row("SELECT project_id FROM project_path WHERE path = ?1", [path], |f| f.get(0)).unwrap()
}

/// The proposals, without taking the tests' folders as temporary (they live in `%TEMP%`): only those that
/// have `subidas` in the path.
fn proposals(db: &Connection) -> Vec<Propuesta> {
    nombres::propuestas_con(db, &Ambito::Todos, &|r: &str| r.contains("subidas")).unwrap()
}

fn pairs(v: &[Propuesta]) -> Vec<(&'static str, String, String)> {
    v.iter().map(|p| (p.kind, p.project.clone(), p.other.clone())).collect()
}

#[test]
fn search_ignores_accents_separators_and_one_letter() {
    let (dir, mut db) = fresh_db("buscar");
    let cronicas = project_dir(&dir, "Crónicas");
    let social = project_dir(&dir, "social-publish");
    let omoikane = project_dir(&dir, "Omoikane");
    save(&mut db, &cronicas, "Nota", "Una nota de crónicas.");
    save(&mut db, &social, "Nota", "Una nota de social.");
    save(&mut db, &omoikane, "Nota", "Una nota de omoikane.");
    let (c, s, o) = (id_of(&db, &cronicas), id_of(&db, &social), id_of(&db, &omoikane));
    assert_eq!(proyecto::buscar(&db, "cronicas").unwrap(), c, "sin acento");
    assert_eq!(proyecto::buscar(&db, "Social Publish").unwrap(), s, "con espacio");
    assert_eq!(proyecto::buscar(&db, "social_publish").unwrap(), s, "con guion bajo");
    assert_eq!(proyecto::buscar(&db, "Omoikae").unwrap(), o, "a una letra, si nada más encaja");
    assert_eq!(proyecto::buscar(&db, "omoik").unwrap(), o, "un trozo sigue valiendo");
    assert!(proyecto::buscar(&db, "Omoi-kamo").is_err(), "a dos letras, no");
}

#[test]
fn search_still_asks_if_there_are_several() {
    let (dir, mut db) = fresh_db("varios");
    let a = project_dir(&dir, "social-publish");
    let b = project_dir(&dir, "social_publish");
    save(&mut db, &a, "Nota", "Una.");
    save(&mut db, &b, "Nota", "Otra.");
    // Written exactly, it picks that one (the 1.7.2 rule).
    assert_eq!(proyecto::buscar(&db, "social_publish").unwrap(), id_of(&db, &b));
    assert_eq!(proyecto::buscar(&db, "social-publish").unwrap(), id_of(&db, &a));
    // Written another way that fits both, it asks.
    let e = proyecto::buscar(&db, "Social Publish").unwrap_err().to_string();
    assert!(e.contains("several") || e.contains("varios"), "{e}");
    // Two at one letter, too.
    let (dir2, mut db2) = fresh_db("varios-letra");
    let x = project_dir(&dir2, "tokimichi");
    let y = project_dir(&dir2, "tokimicha");
    save(&mut db2, &x, "Nota", "Una.");
    save(&mut db2, &y, "Nota", "Otra.");
    assert!(proyecto::buscar(&db2, "tokimichu").is_err());
}

#[test]
fn review_proposes_the_same_name_and_never_merges() {
    let (dir, mut db) = fresh_db("mismo");
    let big = project_dir(&dir, "Omoikane");
    let small = dir.join("q").join("omoikane");
    std::fs::create_dir_all(&small).unwrap();
    let small = proyecto::de(&small).unwrap();
    for i in 0..4 {
        save(&mut db, &big, &format!("Nota {i}"), "Del repositorio.");
    }
    save(&mut db, &small, "Campaña", "De la campaña.");
    let v = proposals(&db);
    assert_eq!(pairs(&v), vec![(nombres::MISMO_NOMBRE, "omoikane".to_string(), "Omoikane".to_string())], "{v:?}");
    // The command already written, with each project named so that `buscar` finds it.
    assert_eq!(v[0].command, "omoi projects merge omoikane Omoikane");
    assert_eq!(v[0].dismiss, "omoi projects distinct omoikane Omoikane");
    // Nothing has been merged.
    assert_ne!(id_of(&db, &big), id_of(&db, &small));
    // «They aren't the same»: it doesn't come up again.
    nombres::marcar_distintos(&db, id_of(&db, &small), Some(id_of(&db, &big))).unwrap();
    assert!(proposals(&db).is_empty());
}

#[test]
fn one_letter_only_with_another_signal() {
    let (dir, mut db) = fresh_db("letra");
    // Without another signal: two live projects, with memories and from the same period, aren't proposed.
    let a = project_dir(&dir, "Tokimichi");
    let b = project_dir(&dir, "Tokimicha");
    for i in 0..5 {
        save(&mut db, &a, &format!("A {i}"), "Uno.");
        save(&mut db, &b, &format!("B {i}"), "Otro.");
    }
    // A name inside another isn't similar: never.
    let web = project_dir(&dir, "tokimichi-web");
    save(&mut db, &web, "Web", "La web.");
    assert!(proposals(&db).is_empty(), "{:?}", proposals(&db));
    // With a folder that no longer exists, yes.
    let c = project_dir(&dir, "Omoikane");
    for i in 0..5 {
        save(&mut db, &c, &format!("C {i}"), "Tres.");
    }
    let gone = project_dir(&dir, "Omoikae");
    save(&mut db, &gone, "Mal escrito", "Una.");
    std::fs::remove_dir_all(&gone).unwrap();
    let v = proposals(&db);
    assert_eq!(pairs(&v), vec![(nombres::UNA_LETRA, "Omoikae".to_string(), "Omoikane".to_string())], "{v:?}");
    assert!(v[0].detail.contains("no longer exists"), "{}", v[0].detail);
}

#[test]
fn one_letter_with_a_new_almost_empty_one() {
    let (dir, mut db) = fresh_db("nuevo");
    let old = project_dir(&dir, "Narrador");
    for i in 0..6 {
        save(&mut db, &old, &format!("N {i}"), "Del narrador.");
    }
    // The new one is created later (with another timestamp) and has a single memory.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let new = project_dir(&dir, "Narradr");
    save(&mut db, &new, "Una", "Sola.");
    let v = proposals(&db);
    assert_eq!(pairs(&v), vec![(nombres::UNA_LETRA, "Narradr".to_string(), "Narrador".to_string())], "{v:?}");
}

#[test]
fn gone_folder_continued_by_another() {
    let (dir, mut db) = fresh_db("continua");
    let old = project_dir(&dir, "Narrador");
    save(&mut db, &old, "Narrador: lector de textos", "La app lee en voz alta.");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let new = project_dir(&dir, "Aloud");
    save(&mut db, &new, "Aloud", "Es la app que antes se llamaba Narrador, con otro nombre.");
    // Another live project that doesn't name it doesn't count.
    let other = project_dir(&dir, "Balua");
    save(&mut db, &other, "Balua", "Fotos de platos.");
    assert!(proposals(&db).is_empty(), "mientras la carpeta existe, nada");
    std::fs::remove_dir_all(&old).unwrap();
    let v = proposals(&db);
    assert_eq!(pairs(&v), vec![(nombres::CONTINUA, "Narrador".to_string(), "Aloud".to_string())], "{v:?}");
    assert_eq!(v[0].command, "omoi projects merge Narrador Aloud");
}

#[test]
fn uploads_folder_is_not_a_project() {
    let (dir, mut db) = fresh_db("subidas");
    let up = dir.join("subidas").join("d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39");
    std::fs::create_dir_all(&up).unwrap();
    let up = proyecto::de(&up).unwrap();
    save(&mut db, &up, "Algo subido", "Una nota que no es de esa carpeta.");
    let v = proposals(&db);
    assert_eq!(pairs(&v), vec![(nombres::TEMPORAL, "d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39".to_string(), String::new())], "{v:?}");
    assert!(v[0].detail.contains("--global"), "{}", v[0].detail);
    // «It's a real project».
    nombres::marcar_distintos(&db, id_of(&db, &up), None).unwrap();
    assert!(proposals(&db).is_empty());
}

#[test]
fn the_notice_shows_at_start_once_a_day() {
    let (dir, mut db) = fresh_db("aviso");
    // A real temporary folder (`%TEMP%`): the notice uses `es_temporal`.
    let up = project_dir(&dir, "d35c0488");
    save(&mut db, &up, "Algo subido", "Una nota.");
    let a = nombres::aviso_al_empezar(&db, &Ambito::Todos).unwrap().expect("aviso");
    assert!(a.contains("omoi projects distinct") && a.contains("d35c0488"), "{a}");
    assert!(nombres::aviso_al_empezar(&db, &Ambito::Todos).unwrap().is_none(), "una vez al día");
    // And in the session-start block, through the review.
    let (dir2, mut db2) = fresh_db("aviso-sesion");
    let up2 = project_dir(&dir2, "subida");
    save(&mut db2, &up2, "Algo subido", "Una nota.");
    let home = std::env::home_dir().unwrap();
    let t = recall::al_empezar(&mut db2, &proyecto::de(&home).unwrap(), &Entrada { session_id: "s".into(), ..Default::default() }).unwrap().unwrap_or_default();
    assert!(t.contains("Projects review") && t.contains("omoi projects distinct"), "{t}");
}

#[test]
fn the_recall_understands_a_misdictated_name() {
    let (dir, mut db) = fresh_db("recall");
    let troba = project_dir(&dir, "Troba");
    let toki = project_dir(&dir, "Tokimichi");
    save(&mut db, &troba, "Troba publica sus binarios en troba-releases", "El instalador de Troba baja la versión de escritorio del repositorio de releases.");
    save(&mut db, &toki, "Tokimichi: el calendario va en la hoja de config", "Las fechas de Tokimichi se leen de la hoja de configuración.");
    for i in 0..8 {
        save(&mut db, "", &format!("Relleno {i}"), &format!("Una nota global cualquiera sobre el tema {i} y otras cosas."));
    }
    let home = proyecto::de(&std::env::home_dir().unwrap()).unwrap();
    let ask = |db: &mut Connection, session: &str, prompt: &str| {
        recall::al_preguntar(db, &home, &Entrada { session_id: session.into(), prompt: prompt.into(), ..Default::default() }, None).unwrap().unwrap_or_default()
    };
    // With 5 letters no longer: «trova» could be a normal word (like «tropa»).
    assert!(nombres::mal_escritos(&db, &Ambito::Todos, &["trova".to_string()], &[true]).unwrap().is_empty(), "trova (5 letras) no es Troba");
    let t = ask(&mut db, "a", "cómo va trova esta semana");
    assert!(!t.contains("troba-releases"), "{t}");
    let t = ask(&mut db, "b", "en tokimicho de dónde salen las fechas del calendario");
    assert!(t.contains("hoja de config"), "tokimicho → Tokimichi: {t}");
    // A word that is in some memory isn't touched, even if it is one letter away from a name.
    let terms: Vec<String> = ["trova", "tokimicho", "onmoikane", "socialpublish"].iter().map(|t| t.to_string()).collect();
    let fixes = nombres::mal_escritos(&db, &Ambito::Todos, &terms, &[false, true, true, true]).unwrap();
    assert_eq!(fixes, vec![(1, vec!["tokimichi".to_string()])], "trova está en alguna memoria; onmoikane y socialpublish no son proyectos de esta base");
    // A name with separators, written together or one letter off, gives its words.
    let sp = project_dir(&dir, "social-publish");
    save(&mut db, &sp, "Social", "Publica en redes.");
    let fixes = nombres::mal_escritos(&db, &Ambito::Todos, &terms, &[false, false, false, true]).unwrap();
    assert_eq!(fixes, vec![(3, vec!["social".to_string(), "publish".to_string()])]);
    // One letter away from two projects at once, neither.
    let toka = project_dir(&dir, "Tokimicha");
    save(&mut db, &toka, "Otro", "Otro.");
    assert!(nombres::mal_escritos(&db, &Ambito::Todos, &terms, &[false, true, false, false]).unwrap().is_empty());
}

/// Normal words that don't show up in any memory don't become projects: with fewer than 6 letters
/// («tropa», «rodeo», «rodes», «balsa»), with a name of fewer than 6 («trobar» → Troba), nor an inflection
/// of the name («faltas» → Faltis). And a misdictated name is only looked up among the visible projects:
/// from a project without a space, never a client's.
#[test]
fn the_recall_doesnt_change_normal_words_nor_look_at_other_clients() {
    let (dir, mut db) = fresh_db("recall-negativos");
    for (name, title) in [("Troba", "Troba indexa"), ("Rodet", "Rodet pesa bobinas"), ("Balùa", "Balùa cuenta calorías"), ("Faltis", "Faltis compara tickets")] {
        let p = project_dir(&dir, name);
        save(&mut db, &p, title, "Una nota del proyecto.");
    }
    let terms: Vec<String> = ["tropa", "trobar", "rodeo", "rodes", "balsa", "faltas"].iter().map(|t| t.to_string()).collect();
    let all = vec![true; terms.len()];
    assert!(nombres::mal_escritos(&db, &Ambito::Todos, &terms, &all).unwrap().is_empty());
    // «faltis» is Faltis, and «faltiz» (not an inflection) too.
    let t2: Vec<String> = ["faltiz".to_string()].to_vec();
    assert_eq!(nombres::mal_escritos(&db, &Ambito::Todos, &t2, &[true]).unwrap(), vec![(0, vec!["faltis".to_string()])]);
    // A client's project: from what has no space its name isn't looked up; from its space or
    // from the general folder, it is.
    let acme = proyecto::crear_espacio(&db, "acme").unwrap();
    let kawa = project_dir(&dir, "Kawaneko");
    save(&mut db, &kawa, "Kawaneko guarda fotos", "En la caché.");
    proyecto::asignar_espacio(&db, id_of(&db, &kawa), Some(acme)).unwrap();
    let t3: Vec<String> = ["kawaneco".to_string()].to_vec();
    let personal = Ambito::Espacio(None);
    assert!(nombres::mal_escritos(&db, &personal, &t3, &[true]).unwrap().is_empty(), "de otro cliente, no");
    assert_eq!(nombres::mal_escritos(&db, &Ambito::Espacio(Some(acme)), &t3, &[true]).unwrap().len(), 1);
    assert_eq!(nombres::mal_escritos(&db, &Ambito::Todos, &t3, &[true]).unwrap().len(), 1);
}

/// `proyecto::buscar_en` only searches among the projects of the scope's space, and its error doesn't name
/// another client's.
#[test]
fn searching_a_project_doesnt_leave_the_space() {
    let (dir, mut db) = fresh_db("buscar-espacio");
    let acme = proyecto::crear_espacio(&db, "acme").unwrap();
    let beta = proyecto::crear_espacio(&db, "beta").unwrap();
    let a = project_dir(&dir, "acme-app");
    let b = project_dir(&dir, "beta-app");
    save(&mut db, &a, "Nota", "De acme.");
    save(&mut db, &b, "Nota", "De beta.");
    proyecto::asignar_espacio(&db, id_of(&db, &a), Some(acme)).unwrap();
    proyecto::asignar_espacio(&db, id_of(&db, &b), Some(beta)).unwrap();
    let from_beta = Ambito::Proyecto { id: id_of(&db, &b), space: Some(beta) };
    assert_eq!(proyecto::buscar_en(&db, "beta", &from_beta).unwrap(), id_of(&db, &b), "«beta» encaja solo con el suyo, aunque «app» también con acme");
    let e = proyecto::buscar_en(&db, "acme-app", &from_beta).unwrap_err().to_string();
    let known = e.split_once("known:").map(|(_, l)| l.to_string()).unwrap_or_default();
    assert!(known.contains("beta-app") && !known.contains("acme"), "el error no nombra lo de otro cliente: {e}");
    assert_eq!(proyecto::buscar_en(&db, "app", &from_beta).unwrap(), id_of(&db, &b), "«app» solo encaja con el suyo");
    // Not by its folder either.
    assert!(proyecto::buscar_en(&db, &a, &from_beta).is_err());
    assert_eq!(proyecto::buscar_en(&db, "acme-app", &Ambito::Todos).unwrap(), id_of(&db, &a));
}

/// Two worktrees of a repository without `origin`, in folders with different names, are the same
/// project (through the common git folder), and with the repository's key.
#[test]
fn worktrees_without_remote_are_one_project() {
    let (dir, mut db) = fresh_db("worktrees");
    let main_dir = dir.join("Kumi");
    std::fs::create_dir_all(main_dir.join(".git").join("worktrees").join("rama-a")).unwrap();
    std::fs::create_dir_all(main_dir.join(".git").join("worktrees").join("rama-b")).unwrap();
    let worktree = |name: &str, branch: &str| -> String {
        let w = dir.join(name);
        std::fs::create_dir_all(&w).unwrap();
        let gitdir: PathBuf = main_dir.join(".git").join("worktrees").join(branch);
        std::fs::write(w.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        proyecto::de(&w).unwrap()
    };
    let a = worktree("kumi-feature", "rama-a");
    let b = worktree("otra-cosa", "rama-b");
    // First the two worktrees, without the main one recorded: the same project, with the repository's key.
    save(&mut db, &a, "Desde A", "Una.");
    save(&mut db, &b, "Desde B", "Otra.");
    assert_eq!(id_of(&db, &a), id_of(&db, &b));
    let person = db::persona_de(&db);
    assert_eq!(proyecto::clave_de(&db, id_of(&db, &a)).unwrap(), format!("dir:{person}/kumi"));
    // Then the main folder: also.
    let p = proyecto::de(&main_dir).unwrap();
    save(&mut db, &p, "Desde la principal", "Tres.");
    assert_eq!(id_of(&db, &p), id_of(&db, &a));
    // Another folder with the same name but a different repository is still another project.
    let other = dir.join("x").join("Kumi");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let other = proyecto::de(&other).unwrap();
    save(&mut db, &other, "Otro Kumi", "Cuatro.");
    assert_ne!(id_of(&db, &other), id_of(&db, &p));
}

/// The main one recorded earlier (as in databases from before 1.8): a new worktree joins it,
/// and the key already saved doesn't change.
#[test]
fn new_worktree_of_an_already_recorded_main() {
    let (dir, mut db) = fresh_db("worktree-viejo");
    let main_dir = dir.join("repo");
    let gitdir = main_dir.join(".git").join("worktrees").join("w");
    std::fs::create_dir_all(&gitdir).unwrap();
    let p = proyecto::de(&main_dir).unwrap();
    save(&mut db, &p, "Principal", "Una.");
    let key = proyecto::clave_de(&db, id_of(&db, &p)).unwrap();
    let w = dir.join("nombre-distinto");
    std::fs::create_dir_all(&w).unwrap();
    std::fs::write(w.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    let w = proyecto::de(&w).unwrap();
    save(&mut db, &w, "Worktree", "Dos.");
    assert_eq!(id_of(&db, &w), id_of(&db, &p));
    assert_eq!(proyecto::clave_de(&db, id_of(&db, &p)).unwrap(), key);
}
