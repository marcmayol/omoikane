//! 1.8.5: code drift. A memory whose files have changed a lot since it was written (or was
//! confirmed as still right) shows up in the review as `code_drift`; small touch-ups don't.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Scope, NewMemory, Kind};
use omoikane::revisar;
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;

/// The lines of an `n`-line file, all with `mark`.
fn text(n: usize, mark: &str) -> String {
    (0..n).map(|i| format!("linea {i} {mark}\n")).collect()
}

/// `git` in `dir`, independent of the configuration of whoever runs the tests.
fn git(dir: &Path, args: &[&str], when: Option<&str>) {
    let mut c = Command::new("git");
    c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", "-C"]).arg(dir).args(args);
    c.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    if let Some(f) = when {
        c.env("GIT_AUTHOR_DATE", f).env("GIT_COMMITTER_DATE", f);
    }
    let o = c.output().expect("git");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Writes `file` into the repository and commits it dated `ago` (in SQLite's `strftime` format,
/// e.g. `-8 days`) or now.
fn commit(db: &Connection, repo: &Path, file: &str, content: &str, ago: Option<&str>) {
    let path = repo.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    let when = ago.map(|h| format!("{} +0000", seconds(db, h)));
    git(repo, &["add", "-A"], None);
    git(repo, &["commit", "-q", "-m", file], when.as_deref());
}

fn seconds(db: &Connection, ago: &str) -> i64 {
    db.query_row("SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER)", [ago], |f| f.get(0)).unwrap()
}

fn date(db: &Connection, ago: &str) -> String {
    db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)", [ago], |f| f.get(0)).unwrap()
}

/// A project memory with those paths, written `ago`.
fn memory(db: &mut Connection, project: &str, title: &str, content: &str, paths: &[&str], ago: &str) -> String {
    let n = NewMemory {
        project: project.into(),
        kind: Some(Kind::Gotcha),
        title: title.into(),
        content: content.into(),
        paths: paths.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    let m = memoria::save(db, n, None).unwrap();
    let f = date(db, ago);
    db.execute("UPDATE memory SET created_at = ?1, updated_at = ?1 WHERE id = ?2", rusqlite::params![f, m.id]).unwrap();
    m.id
}

fn drifts(db: &Connection, project: &str) -> Vec<(String, String)> {
    let scope = Scope::from_project(db, project).unwrap();
    revisar::notices(db, &scope).unwrap().into_iter().filter(|a| a.kind == revisar::CODE_DRIFT).map(|a| (a.memory_id, a.detail)).collect()
}

/// A git repository with `src/a.rs` and `src/b.rs` (40 lines each) from 20 days ago.
fn repo(name: &str) -> (comun::Temporal, Connection, String) {
    let d = comun::Temporal::nueva(&format!("deriva-{name}"));
    let root = d.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"], None);
    let db = db::open(&d.join("o.db")).unwrap();
    commit(&db, &root, "src/a.rs", &text(40, "uno"), Some("-20 days"));
    commit(&db, &root, "src/b.rs", &text(40, "uno"), Some("-20 days"));
    let project = omoikane::proyecto::of_dir(&root).unwrap();
    omoikane::proyecto::register(&db, &project).unwrap();
    (d, db, project)
}

#[test]
fn warns_if_its_files_change_a_lot_and_not_for_touch_ups() {
    let (_d, mut db, project) = repo("mucho");
    let root = Path::new(&project).to_path_buf();
    let big = memory(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    let small = memory(&mut db, &project, "Cómo va b.rs", "Lo de b.rs.", &["src/b.rs"], "-10 days");
    // a.rs, fully rewritten; b.rs, two lines out of forty.
    commit(&db, &root, "src/a.rs", &text(40, "dos"), Some("-2 days"));
    let mut b = text(40, "uno");
    b.push_str("otra\nmas\n");
    commit(&db, &root, "src/b.rs", &b, Some("-2 days"));
    let v = drifts(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].0, big);
    assert!(v[0].1.starts_with("`src/a.rs` changed a lot since ") && v[0].1.contains("1 commit, 80 lines changed, 200% of their size") && v[0].1.ends_with("is it still true?"), "{}", v[0].1);
    assert!(!v.iter().any(|(id, _)| *id == small));
    // The warning at start counts what was already computed.
    let counts = revisar::pending(&db, &Scope::from_project(&db, &project).unwrap()).unwrap();
    assert_eq!(counts.get(revisar::CODE_DRIFT), Some(&1), "{counts:?}");
}

#[test]
fn files_it_mentions_also_count() {
    let (_d, mut db, project) = repo("nombra");
    let root = Path::new(&project).to_path_buf();
    let id = memory(&mut db, &project, "El parser", "La función de `src/b.rs` que parte las líneas.", &[], "-10 days");
    commit(&db, &root, "src/b.rs", &text(40, "dos"), Some("-2 days"));
    let v = drifts(&db, &project);
    assert_eq!(v.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![id.as_str()], "{v:?}");
}

#[test]
fn same_session_changes_dont_count() {
    // Saving the memory and then committing the work it talks about is normal: it isn't drift.
    let (_d, mut db, project) = repo("gracia");
    let root = Path::new(&project).to_path_buf();
    memory(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-3 hours");
    commit(&db, &root, "src/a.rs", &text(40, "dos"), Some("-1 hours"));
    assert!(drifts(&db, &project).is_empty());
}

#[test]
fn fine_as_is_until_it_changes_again() {
    let (_d, mut db, project) = repo("ok");
    let root = Path::new(&project).to_path_buf();
    let id = memory(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    commit(&db, &root, "src/a.rs", &text(40, "dos"), Some("-8 days"));
    assert_eq!(drifts(&db, &project).len(), 1);
    revisar::dismiss(&db, &id, revisar::CODE_DRIFT, "").unwrap();
    assert!(drifts(&db, &project).is_empty());
    // Confirmed 5 days ago: the change from 8 days ago no longer counts…
    let f = date(&db, "-5 days");
    db.execute("UPDATE verdict SET at = ?1 WHERE memory_id = ?2", rusqlite::params![f, id]).unwrap();
    assert!(drifts(&db, &project).is_empty());
    // …and a big change afterwards warns again, once recomputed (once a day, separately: today's
    // result is already stored).
    commit(&db, &root, "src/a.rs", &text(40, "tres"), Some("-1 days"));
    assert!(drifts(&db, &project).is_empty());
    assert_eq!(omoikane::deriva::refresh(&db).unwrap(), 1);
    let v = drifts(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].1.contains("since ") && v[0].1.contains("1 commit,"), "{}", v[0].1);
    // The memory has not been touched.
    assert_eq!(memoria::get(&db, &id).unwrap().content, "Lo de a.rs.");
}

#[test]
fn outside_git_the_diary_counts() {
    let d = comun::Temporal::nueva("deriva-diario");
    let root = d.join("carpeta");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notas.md"), "hola\n").unwrap();
    let mut db = db::open(&d.join("o.db")).unwrap();
    let project = omoikane::proyecto::of_dir(&root).unwrap();
    omoikane::proyecto::register(&db, &project).unwrap();
    let pid = omoikane::proyecto::resolve(&db, &project).unwrap().unwrap().0;
    let id = memory(&mut db, &project, "Las notas", "Lo de las notas.", &["notas.md"], "-10 days");
    omoikane::diario::create_table(&db).unwrap();
    let session = |db: &Connection, n: usize, saved: &str| {
        let f = date(db, &format!("-{} days", n + 1));
        db.execute(
            "INSERT INTO diary(session, project_id, folder, first_at, last_at, files, saved) VALUES (?1, ?2, ?3, ?4, ?4, '[\"notas.md\"]', ?5)",
            rusqlite::params![format!("s{n}"), pid, project, f, saved],
        )
        .unwrap();
    };
    // Four sessions afterwards, plus the one that saved it (doesn't count): not yet.
    for n in 0..4 {
        session(&db, n, "[]");
    }
    session(&db, 4, &format!("[\"{id}\"]"));
    assert!(drifts(&db, &project).is_empty());
    // The fifth, yes. (Today's result is stored: so it sees it, the memory "changes" date.)
    session(&db, 5, "[]");
    let f = date(&db, "-9 days");
    db.execute("UPDATE memory SET updated_at = ?1 WHERE id = ?2", rusqlite::params![f, id]).unwrap();
    let v = drifts(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].1.contains("`notas.md`") && v[0].1.contains("edited in 5 sessions"), "{}", v[0].1);
}

/// `omoi` with the test's database and nothing from outside.
fn omoi(d: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(args)
        .env("OMOIKANE_DB", d.join("o.db"))
        .env("OMOIKANE_MODELS", d.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_HOME", d.join("home-omoikane"))
        .env("OMOIKANE_LANG", "en")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn cli_recomputes_and_confirms() {
    let (d, mut db, project) = repo("cli");
    let root = Path::new(&project).to_path_buf();
    let id = memory(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    commit(&db, &root, "src/a.rs", &text(40, "dos"), Some("-2 days"));
    // What the background process does: leaves it computed for today.
    omoi(&d, &["review", "--refresh-drift"]);
    let stored: String = db.query_row("SELECT v FROM meta WHERE k = 'deriva_codigo'", [], |f| f.get(0)).unwrap();
    assert!(stored.contains(&id) && stored.contains("changed a lot"), "{stored}");
    let counts = revisar::pending(&db, &Scope::from_project(&db, &project).unwrap()).unwrap();
    assert_eq!(counts.get(revisar::CODE_DRIFT), Some(&1), "{counts:?}");
    let output = omoi(&d, &["review", "--dismiss", &id, "--kind", "code_drift"]);
    assert!(output.contains("until its files change a lot again"), "{output}");
    assert!(drifts(&db, &project).is_empty());
}
