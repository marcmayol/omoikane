//! 1.10: de memoria a skill del repositorio (`omoi skill`): las dos carpetas, el estado (al día,
//! desfasada, editada, huérfana), `refresh`, y que no pise lo ajeno ni escriba secretos.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Cambios, Nueva, Tipo};
use omoikane::skill::{self, Destino, Estado};
use std::path::Path;

fn preparar(nombre: &str) -> (comun::Temporal, rusqlite::Connection, String) {
    let d = comun::Temporal::nueva(&format!("skill-{nombre}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    (d, db, repo)
}

fn receta(db: &mut rusqlite::Connection, repo: &str) -> String {
    let n = Nueva {
        project: repo.into(),
        kind: Some(Tipo::Procedure),
        title: "Publicar una versión".into(),
        content: "1. Subir la versión en Cargo.toml.\n2. Etiquetar y empujar.\n\nwhen: publica, saca versión".into(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

fn estados(db: &rusqlite::Connection, repo: &str) -> Vec<(String, Estado)> {
    skill::listar(db, repo).unwrap().into_iter().map(|s| (s.path, s.state)).collect()
}

#[test]
fn escribe_las_dos_y_sigue_su_estado() {
    let (_d, mut db, repo) = preparar("estado");
    let id = receta(&mut db, &repo);
    // Simulacro: no toca nada.
    let r = skill::escribir(&db, &repo, &id[..12], Destino::Ambas, false, true).unwrap();
    assert!(r.iter().all(|e| e.action == "created"));
    assert!(estados(&db, &repo).is_empty());
    let r = skill::escribir(&db, &repo, &id, Destino::Ambas, false, false).unwrap();
    let rutas: Vec<&str> = r.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(rutas, [".agents/skills/publicar-una-version/SKILL.md", ".claude/skills/publicar-una-version/SKILL.md"]);
    let texto = std::fs::read_to_string(Path::new(&repo).join(rutas[0])).unwrap();
    assert!(texto.starts_with("---\nname: publicar-una-version\n"), "{texto}");
    assert!(texto.contains("Úsala cuando: publica, saca versión") && texto.contains(&format!("omoikane-id: \"{id}\"")), "{texto}");
    assert!(estados(&db, &repo).iter().all(|(_, e)| *e == Estado::UpToDate));
    // Otra vez: igual.
    assert!(skill::escribir(&db, &repo, &id, Destino::Ambas, false, false).unwrap().iter().all(|e| e.action == "unchanged"));

    // La memoria cambia: desfasadas, y `refresh` las pone al día (con otro título, en el mismo fichero).
    memoria::actualizar(&mut db, &id, Cambios { title: Some("Publicar una versión nueva".into()), ..Default::default() }, None).unwrap();
    assert!(estados(&db, &repo).iter().all(|(_, e)| *e == Estado::Outdated));
    assert_eq!(skill::refrescar(&db, &repo, false).unwrap().len(), 2);
    assert!(estados(&db, &repo).iter().all(|(p, e)| *e == Estado::UpToDate && p.contains("/publicar-una-version/")));

    // Editada a mano (y con los finales de línea de Windows): se respeta, y `refresh` no la toca.
    let a_mano = Path::new(&repo).join(".claude/skills/publicar-una-version/SKILL.md");
    std::fs::write(&a_mano, std::fs::read_to_string(&a_mano).unwrap().replace("Etiquetar", "Etiquetar con -a").replace('\n', "\r\n")).unwrap();
    memoria::actualizar(&mut db, &id, Cambios { append: Some("3. Publicar.".into()), ..Default::default() }, None).unwrap();
    let e: Vec<Estado> = estados(&db, &repo).into_iter().map(|(_, e)| e).collect();
    assert_eq!(e, [Estado::Outdated, Estado::Edited]);
    assert_eq!(skill::refrescar(&db, &repo, false).unwrap().len(), 1);
    let r = skill::escribir(&db, &repo, &id, Destino::Claude, false, false).unwrap();
    assert_eq!(r[0].action, "skipped");
    assert_eq!(skill::escribir(&db, &repo, &id, Destino::Claude, true, false).unwrap()[0].action, "updated");

    // Con los finales de línea de Windows (git en Windows) sigue al día.
    let agentes = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::write(&agentes, std::fs::read_to_string(&agentes).unwrap().replace('\n', "\r\n")).unwrap();
    assert!(estados(&db, &repo).iter().all(|(_, e)| *e == Estado::UpToDate));

    // Olvidada: huérfana.
    memoria::olvidar(&mut db, &id).unwrap();
    assert!(estados(&db, &repo).iter().all(|(_, e)| *e == Estado::Orphan));
}

#[test]
fn no_pisa_lo_ajeno_ni_escribe_secretos() {
    let (_d, mut db, repo) = preparar("ajeno");
    let id = receta(&mut db, &repo);
    let ajena = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::create_dir_all(ajena.parent().unwrap()).unwrap();
    let original = "---\nname: publicar-una-version\ndescription: mía\n---\n\nA mano.\n";
    std::fs::write(&ajena, original).unwrap();
    let r = skill::escribir(&db, &repo, &id, Destino::Agentes, false, false).unwrap();
    // El nombre lo tiene otra: va con el final del id, y la ajena no se toca.
    assert!(r[0].action == "created" && r[0].path.starts_with(".agents/skills/publicar-una-version-"), "{r:?}");
    assert_eq!(std::fs::read_to_string(&ajena).unwrap(), original);
    let nombre = r[0].path.split('/').nth(2).unwrap().to_string();
    let texto = std::fs::read_to_string(Path::new(&repo).join(&r[0].path)).unwrap();
    assert!(texto.starts_with(&format!("---\nname: {nombre}\n")), "{texto}");

    let n = Nueva {
        project: repo.clone(),
        kind: Some(Tipo::Fact),
        title: "Clave del despliegue".into(),
        content: "Se despliega con la clave de servicio.".into(),
        ..Default::default()
    };
    let secreta = memoria::guardar(&mut db, n, None).unwrap().id;
    // Al guardarla ya se quitan los secretos; aquí, una que llegó con uno (de una base antigua).
    db.execute("UPDATE memory SET content = 'token ghp_abcdefghijklmnopqrstuvwxyz0123456789' WHERE id = ?1", [&secreta]).unwrap();
    let e = skill::escribir(&db, &repo, &secreta, Destino::Ambas, false, false).unwrap_err().to_string();
    assert!(e.contains("secret"), "{e}");

    // Fuera de un repositorio, no.
    assert!(skill::listar(&db, "").is_err());
}

/// Sin buscar el id: `sugerencias` propone las recetas y lo que el agente aplicó en 2 sesiones, y
/// `resolver` encuentra la memoria con unas palabras de su título.
#[test]
fn propone_y_encuentra_sin_id() {
    let (d, mut db, repo) = preparar("propone");
    let receta_id = receta(&mut db, &repo);
    let hecho = |db: &mut rusqlite::Connection, titulo: &str| {
        let n = Nueva { project: repo.clone(), kind: Some(Tipo::Fact), title: titulo.into(), content: format!("{titulo}, con detalle."), ..Default::default() };
        memoria::guardar(db, n, None).unwrap().id
    };
    let usada = hecho(&mut db, "El smoke test necesita compilar antes");
    let una_vez = hecho(&mut db, "El puerto del servidor es el 8010");
    // Global: no es del repositorio.
    let n = Nueva { kind: Some(Tipo::Procedure), title: "Receta global".into(), content: "1. Algo.

when: global".into(), ..Default::default() };
    memoria::guardar(&mut db, n, None).unwrap();
    for (sesion, ids) in [("s1", vec![usada.clone(), una_vez.clone()]), ("s2", vec![usada.clone()])] {
        let conv = d.join(format!("{sesion}.jsonl"));
        std::fs::write(&conv, "").unwrap();
        omoikane::diario::apuntar(&db, &repo, sesion, Some(&conv)).unwrap();
        omoikane::diario::apuntar_aplicadas(&db, &repo, sesion, Some(&conv), &ids).unwrap();
    }
    let ids = |db: &rusqlite::Connection| skill::sugerencias(db, &repo).unwrap().into_iter().map(|s| s.id).collect::<Vec<_>>();
    // Primero la más aplicada; la aplicada una sola vez, no.
    assert_eq!(ids(&db), vec![usada.clone(), receta_id.clone()]);

    // Con palabras del título, sin el id.
    assert_eq!(skill::resolver(&mut db, &repo, "publicar versión").unwrap(), receta_id);
    assert_eq!(skill::resolver(&mut db, &repo, &usada[..12]).unwrap(), usada);
    assert!(skill::resolver(&mut db, &repo, "nada parecido zzzz").is_err());

    // Hecha skill, deja de proponerse.
    skill::escribir(&db, &repo, &receta_id, Destino::Ambas, false, false).unwrap();
    assert_eq!(ids(&db), vec![usada]);
}
