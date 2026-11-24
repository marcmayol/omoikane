//! 1.10: from memory to repository skill (`omoi skill`): the two folders, the state (up to date,
//! outdated, edited, orphan), `refresh`, and that it doesn't overwrite others' files or write secrets.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Changes, NewMemory, Kind};
use omoikane::skill::{self, Target, State};
use std::path::Path;

fn setup(name: &str) -> (comun::Temporal, rusqlite::Connection, String) {
    let d = comun::Temporal::nueva(&format!("skill-{name}"));
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    let repo = omoikane::proyecto::of_dir(&d.join("repo")).unwrap();
    let db = db::open(&d.join("o.db")).unwrap();
    omoikane::proyecto::register(&db, &repo).unwrap();
    (d, db, repo)
}

fn recipe(db: &mut rusqlite::Connection, repo: &str) -> String {
    let n = NewMemory {
        project: repo.into(),
        kind: Some(Kind::Procedure),
        title: "Publicar una versión".into(),
        content: "1. Subir la versión en Cargo.toml.\n2. Etiquetar y empujar.\n\nwhen: publica, saca versión".into(),
        ..Default::default()
    };
    memoria::save(db, n, None).unwrap().id
}

fn states(db: &rusqlite::Connection, repo: &str) -> Vec<(String, State)> {
    skill::list(db, repo).unwrap().into_iter().map(|s| (s.path, s.state)).collect()
}

#[test]
fn writes_both_and_tracks_their_state() {
    let (_d, mut db, repo) = setup("estado");
    let id = recipe(&mut db, &repo);
    // Dry run: touches nothing.
    let r = skill::write(&db, &repo, &id[..12], Target::Both, false, true).unwrap();
    assert!(r.iter().all(|e| e.action == "created"));
    assert!(states(&db, &repo).is_empty());
    let r = skill::write(&db, &repo, &id, Target::Both, false, false).unwrap();
    let paths: Vec<&str> = r.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, [".agents/skills/publicar-una-version/SKILL.md", ".claude/skills/publicar-una-version/SKILL.md"]);
    let text = std::fs::read_to_string(Path::new(&repo).join(paths[0])).unwrap();
    assert!(text.starts_with("---\nname: publicar-una-version\n"), "{text}");
    assert!(text.contains("Úsala cuando: publica, saca versión") && text.contains(&format!("omoikane-id: \"{id}\"")), "{text}");
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == State::UpToDate));
    // Again: the same.
    assert!(skill::write(&db, &repo, &id, Target::Both, false, false).unwrap().iter().all(|e| e.action == "unchanged"));

    // The memory changes: outdated, and `refresh` brings them up to date (with another title, in the same file).
    memoria::update(&mut db, &id, Changes { title: Some("Publicar una versión nueva".into()), ..Default::default() }, None).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == State::Outdated));
    assert_eq!(skill::refresh(&db, &repo, false).unwrap().len(), 2);
    assert!(states(&db, &repo).iter().all(|(p, e)| *e == State::UpToDate && p.contains("/publicar-una-version/")));

    // Edited by hand (and with Windows line endings): it is respected, and `refresh` doesn't touch it.
    let by_hand = Path::new(&repo).join(".claude/skills/publicar-una-version/SKILL.md");
    std::fs::write(&by_hand, std::fs::read_to_string(&by_hand).unwrap().replace("Etiquetar", "Etiquetar con -a").replace('\n', "\r\n")).unwrap();
    memoria::update(&mut db, &id, Changes { append: Some("3. Publicar.".into()), ..Default::default() }, None).unwrap();
    let e: Vec<State> = states(&db, &repo).into_iter().map(|(_, e)| e).collect();
    assert_eq!(e, [State::Outdated, State::Edited]);
    assert_eq!(skill::refresh(&db, &repo, false).unwrap().len(), 1);
    let r = skill::write(&db, &repo, &id, Target::Claude, false, false).unwrap();
    assert_eq!(r[0].action, "skipped");
    assert_eq!(skill::write(&db, &repo, &id, Target::Claude, true, false).unwrap()[0].action, "updated");

    // With Windows line endings (git on Windows) it is still up to date.
    let agents_file = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::write(&agents_file, std::fs::read_to_string(&agents_file).unwrap().replace('\n', "\r\n")).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == State::UpToDate));

    // Forgotten: orphan.
    memoria::forget(&mut db, &id).unwrap();
    assert!(states(&db, &repo).iter().all(|(_, e)| *e == State::Orphan));
}

#[test]
fn does_not_overwrite_others_nor_write_secrets() {
    let (_d, mut db, repo) = setup("ajeno");
    let id = recipe(&mut db, &repo);
    let foreign = Path::new(&repo).join(".agents/skills/publicar-una-version/SKILL.md");
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    let original = "---\nname: publicar-una-version\ndescription: mía\n---\n\nA mano.\n";
    std::fs::write(&foreign, original).unwrap();
    let r = skill::write(&db, &repo, &id, Target::Agents, false, false).unwrap();
    // Another one has the name: it gets the end of the id, and the other one is not touched.
    assert!(r[0].action == "created" && r[0].path.starts_with(".agents/skills/publicar-una-version-"), "{r:?}");
    assert_eq!(std::fs::read_to_string(&foreign).unwrap(), original);
    let name = r[0].path.split('/').nth(2).unwrap().to_string();
    let text = std::fs::read_to_string(Path::new(&repo).join(&r[0].path)).unwrap();
    assert!(text.starts_with(&format!("---\nname: {name}\n")), "{text}");

    let n = NewMemory {
        project: repo.clone(),
        kind: Some(Kind::Fact),
        title: "Clave del despliegue".into(),
        content: "Se despliega con la clave de servicio.".into(),
        ..Default::default()
    };
    let secret = memoria::save(&mut db, n, None).unwrap().id;
    // Saving it already strips secrets; here, one that arrived with one (from an old database).
    db.execute("UPDATE memory SET content = 'token ghp_abcdefghijklmnopqrstuvwxyz0123456789' WHERE id = ?1", [&secret]).unwrap();
    let e = skill::write(&db, &repo, &secret, Target::Both, false, false).unwrap_err().to_string();
    assert!(e.contains("secret"), "{e}");

    // Outside a repository, no.
    assert!(skill::list(&db, "").is_err());
}

/// Without looking up the id: `suggestions` suggests the recipes and what the agent applied in 2 sessions, and
/// `resolve` finds the memory with a few words from its title.
#[test]
fn suggests_and_finds_without_id() {
    let (d, mut db, repo) = setup("propone");
    let recipe_id = recipe(&mut db, &repo);
    let fact = |db: &mut rusqlite::Connection, title: &str| {
        let n = NewMemory { project: repo.clone(), kind: Some(Kind::Fact), title: title.into(), content: format!("{title}, con detalle."), ..Default::default() };
        memoria::save(db, n, None).unwrap().id
    };
    let used = fact(&mut db, "El smoke test necesita compilar antes");
    let once = fact(&mut db, "El puerto del servidor es el 8010");
    // Global: not from the repository.
    let n = NewMemory { kind: Some(Kind::Procedure), title: "Receta global".into(), content: "1. Algo.

when: global".into(), ..Default::default() };
    memoria::save(&mut db, n, None).unwrap();
    for (session, ids) in [("s1", vec![used.clone(), once.clone()]), ("s2", vec![used.clone()])] {
        let conv = d.join(format!("{session}.jsonl"));
        std::fs::write(&conv, "").unwrap();
        omoikane::diario::record(&db, &repo, session, Some(&conv)).unwrap();
        omoikane::diario::record_applied(&db, &repo, session, Some(&conv), &ids).unwrap();
    }
    let ids = |db: &rusqlite::Connection| skill::suggestions(db, &repo).unwrap().into_iter().map(|s| s.id).collect::<Vec<_>>();
    // Most applied first; the one applied only once, not.
    assert_eq!(ids(&db), vec![used.clone(), recipe_id.clone()]);

    // With words from the title, without the id.
    assert_eq!(skill::resolve(&mut db, &repo, "publicar versión").unwrap(), recipe_id);
    assert_eq!(skill::resolve(&mut db, &repo, &used[..12]).unwrap(), used);
    assert!(skill::resolve(&mut db, &repo, "nada parecido zzzz").is_err());

    // Made a skill, it is no longer suggested.
    skill::write(&db, &repo, &recipe_id, Target::Both, false, false).unwrap();
    assert_eq!(ids(&db), vec![used]);
}
