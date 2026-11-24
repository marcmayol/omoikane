//! 1.10: from memory to repository skill (`omoi skill`): the two folders, the state (up to date,
//! outdated, edited, orphan), `refresh`, and that it doesn't overwrite others' files or write secrets.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Cambios, Nueva, Tipo};
use omoikane::skill::{self, Destino, Estado};
use std::path::Path;

fn setup(name: &str) -> (comun::Temporal, rusqlite::Connection, String) {
    let d = comun::Temporal::nueva(&format!("skill-{name}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    let repo = omoikane::proyecto::de(&d.join("repo")).unwrap();
    let db = db::abrir(&d.join("o.db")).unwrap();
    omoikane::proyecto::registrar(&db, &repo).unwrap();
    (d, db, repo)
}

fn recipe(db: &mut rusqlite::Connection, repo: &str) -> String {
    let n = Nueva {
        project: repo.into(),
        kind: Some(Tipo::Procedure),
        title: "Publicar una versión".into(),
        content: "1. Subir la versión en Cargo.toml.\n2. Etiquetar y empujar.\n\nwhen: publica, saca versión".into(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

fn states(db: &rusqlite::Connection, repo: &str) -> Vec<(String, Estado)> {
    skill::listar(db, repo).unwrap().into_iter().map(|s| (s.path, s.state)).collect()
}

#[test]
fn writes_both_and_tracks_their_state() {
    let (_d, mut db, repo) = setup("estado");
    let id = recipe(&mut db, &repo);
    // Dry run: touches nothing.
    let r = skill::escribir(&db, &repo, &id[..12], Destino::Ambas, false, true).unwrap();
    assert!(r.iter().all(|e| e.action == "created"));
    assert!(states(&db, &repo).is_empty());
    let r = skill::escribir(&db, &repo, &id, Destino::Ambas, false, false).unwrap();
    let paths: Vec<&str> = r.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, [".agents/skills/publicar-una-version/SKILL.md", ".claude/skills/publicar-una-version/SKILL.md"]);
    let text = std::fs::read_to_string(Path::new(&repo).join(paths[0])).unwrap();
    assert!(text.starts_with("---\nname: publicar-una-version\n"), "{text}");
    assert!(text.contains("Úsala cuando: publica, saca versión") && text.contains(&format!("omoikane-id: \"{id}\"")), "{text}");
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == Estado::UpToDate));
    // Again: the same.
    assert!(skill::escribir(&db, &repo, &id, Destino::Ambas, false, false).unwrap().iter().all(|e| e.action == "unchanged"));

    // The memory changes: outdated, and `refresh` brings them up to date (with another title, in the same file).
    memoria::actualizar(&mut db, &id, Cambios { title: Some("Publicar una versión nueva".into()), ..Default::default() }, None).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == Estado::Outdated));
    assert_eq!(skill::refrescar(&db, &repo, false).unwrap().len(), 2);
    assert!(states(&db, &repo).iter().all(|(p, e)| *e == Estado::UpToDate && p.contains("/publicar-una-version/")));

    // Edited by hand (and with Windows line endings): it is respected, and `refresh` doesn't touch it.
    let by_hand = Path::new(&repo).join(".claude/skills/publicar-una-version/SKILL.md");
    std::fs::write(&by_hand, std::fs::read_to_string(&by_hand).unwrap().replace("Etiquetar", "Etiquetar con -a").replace('\n', "\r\n")).unwrap();
    memoria::actualizar(&mut db, &id, Cambios { append: Some("3. Publicar.".into()), ..Default::default() }, None).unwrap();
    let e: Vec<Estado> = states(&db, &repo).into_iter().map(|(_, e)| e).collect();
    assert_eq!(e, [Estado::Outdated, Estado::Edited]);
    assert_eq!(skill::refrescar(&db, &repo, false).unwrap().len(), 1);
    let r = skill::escribir(&db, &repo, &id, Destino::Claude, false, false).unwrap();
    assert_eq!(r[0].action, "skipped");
    assert_eq!(skill::escribir(&db, &repo, &id, Destino::Claude, true, false).unwrap()[0].action, "updated");

    // With Windows line endings (git on Windows) it is still up to date.
    let agents_file = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::write(&agents_file, std::fs::read_to_string(&agents_file).unwrap().replace('\n', "\r\n")).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == Estado::UpToDate));

    // Forgotten: orphan.
    memoria::olvidar(&mut db, &id).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == Estado::Orphan));
}

#[test]
fn does_not_overwrite_others_nor_write_secrets() {
    let (_d, mut db, repo) = setup("ajeno");
    let id = recipe(&mut db, &repo);
    let foreign = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    let original = "---\nname: publicar-una-version\ndescription: mía\n---\n\nA mano.\n";
    std::fs::write(&foreign, original).unwrap();
    let r = skill::escribir(&db, &repo, &id, Destino::Agentes, false, false).unwrap();
    // Another one has the name: it gets the end of the id, and the other one is not touched.
    assert!(r[0].action == "created" && r[0].path.starts_with(".agents/skills/publicar-una-version-"), "{r:?}");
    assert_eq!(std::fs::read_to_string(&foreign).unwrap(), original);
    let name = r[0].path.split('/').nth(2).unwrap().to_string();
    let text = std::fs::read_to_string(Path::new(&repo).join(&r[0].path)).unwrap();
    assert!(text.starts_with(&format!("---\nname: {name}\n")), "{text}");

    let n = Nueva {
        project: repo.clone(),
        kind: Some(Tipo::Fact),
        title: "Clave del despliegue".into(),
        content: "Se despliega con la clave de servicio.".into(),
        ..Default::default()
    };
    let secret = memoria::guardar(&mut db, n, None).unwrap().id;
    // Saving it already strips secrets; here, one that arrived with one (from an old database).
    db.execute("UPDATE memory SET content = 'token ghp_abcdefghijklmnopqrstuvwxyz0123456789' WHERE id = ?1", [&secret]).unwrap();
    let e = skill::escribir(&db, &repo, &secret, Destino::Ambas, false, false).unwrap_err().to_string();
    assert!(e.contains("secret"), "{e}");

    // Outside a repository, no.
    assert!(skill::listar(&db, "").is_err());
}

/// Without looking up the id: `sugerencias` suggests the recipes and what the agent applied in 2 sessions, and
/// `resolver` finds the memory with a few words from its title.
#[test]
fn suggests_and_finds_without_id() {
    let (d, mut db, repo) = setup("propone");
    let recipe_id = recipe(&mut db, &repo);
    let fact = |db: &mut rusqlite::Connection, title: &str| {
        let n = Nueva { project: repo.clone(), kind: Some(Tipo::Fact), title: title.into(), content: format!("{title}, con detalle."), ..Default::default() };
        memoria::guardar(db, n, None).unwrap().id
    };
    let used = fact(&mut db, "El smoke test necesita compilar antes");
    let once = fact(&mut db, "El puerto del servidor es el 8010");
    // Global: not from the repository.
    let n = Nueva { kind: Some(Tipo::Procedure), title: "Receta global".into(), content: "1. Algo.

when: global".into(), ..Default::default() };
    memoria::guardar(&mut db, n, None).unwrap();
    for (session, ids) in [("s1", vec![used.clone(), once.clone()]), ("s2", vec![used.clone()])] {
        let conv = d.join(format!("{session}.jsonl"));
        std::fs::write(&conv, "").unwrap();
        omoikane::diario::apuntar(&db, &repo, session, Some(&conv)).unwrap();
        omoikane::diario::apuntar_aplicadas(&db, &repo, session, Some(&conv), &ids).unwrap();
    }
    let ids = |db: &rusqlite::Connection| skill::sugerencias(db, &repo).unwrap().into_iter().map(|s| s.id).collect::<Vec<_>>();
    // Most applied first; the one applied only once, not.
    assert_eq!(ids(&db), vec![used.clone(), recipe_id.clone()]);

    // With words from the title, without the id.
    assert_eq!(skill::resolver(&mut db, &repo, "publicar versión").unwrap(), recipe_id);
    assert_eq!(skill::resolver(&mut db, &repo, &used[..12]).unwrap(), used);
    assert!(skill::resolver(&mut db, &repo, "nada parecido zzzz").is_err());

    // Made a skill, it is no longer suggested.
    skill::escribir(&db, &repo, &recipe_id, Destino::Ambas, false, false).unwrap();
    assert_eq!(ids(&db), vec![used]);
}
