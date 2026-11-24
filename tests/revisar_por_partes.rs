//! The partial review (`revisar::refresh_for`) gives the same as the full recompute: after random
//! writes on a database of over 2,000 memories (the sentences of `corpus.txt`, with versions,
//! to-dos and dones, tags and links), the same outdated pairs, the same links and
//! the same broken links as if everything were redone.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Changes, NewMemory, Kind};
use omoikane::revisar;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;

/// A plain old number generator (no dependencies): the test comes out the same on every run.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

fn base(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("por-partes-{name}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    db.execute_batch("PRAGMA synchronous = OFF").unwrap();
    (dir, db)
}

fn sentences() -> Vec<String> {
    include_str!("corpus.txt").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).map(String::from).collect()
}

const PROJECTS: [&str; 3] = ["/alfa", "/beta", ""];

/// A random memory: corpus sentences, one or two topics (rare words: each one shows up in a
/// few memories), sometimes a version (named or bare), a to-do or a done, a date,
/// tags and links to tags, to id prefixes or to nothing.
fn invent(rng: &mut Rng, fr: &[String], ids: &[String]) -> (String, String, Vec<String>) {
    let topic = format!("tema{:03}", rng.below(150));
    let other = format!("asunto{:03}", rng.below(150));
    let title = format!("{topic} {}", fr[rng.below(fr.len())].chars().take(60).collect::<String>());
    let mut c = format!("{} {other}. {}", fr[rng.below(fr.len())], fr[rng.below(fr.len())]);
    match rng.below(6) {
        0 => c.push_str(&format!("\nLa gaviota{} {}.{}.{} quedó publicada.", rng.below(60), rng.below(2), rng.below(9), rng.below(9))),
        1 => c.push_str(&format!("\nUsa libreria{} 0.{}.0 desde el {}-oct-2026.", rng.below(60), 30 + rng.below(12), 1 + rng.below(28))),
        2 => c.push_str(&format!("\nLa {}.{}.0 de {topic}.", rng.below(3), rng.below(20))),
        _ => {}
    }
    match rng.below(5) {
        0 => c.push_str(&format!("\nPendiente renovar el certificado wildcard de {topic} {other}.")),
        1 => c.push_str(&format!("\nHecho: renovado el certificado wildcard de {topic} {other}.")),
        _ => {}
    }
    if rng.below(3) == 0 {
        c.push_str(&format!("\nEl {}-oct-2026.", 1 + rng.below(28)));
    }
    match rng.below(6) {
        0 => c.push_str(&format!(" Ver [[etiqueta-{}]].", rng.below(300))),
        1 => c.push_str(&format!(" Ver [[Etiqueta {}]].", rng.below(300))),
        2 if !ids.is_empty() => c.push_str(&format!(" Ver [[{}]].", &rng.pick(ids)[..8 + rng.below(6)])),
        3 => c.push_str(&format!(" Ver [[no-existe-{}]].", rng.below(10))),
        _ => {}
    }
    let tags = (0..rng.below(3)).map(|_| format!("etiqueta_{}", rng.below(300))).collect();
    (title, c, tags)
}

fn save(db: &mut Connection, rng: &mut Rng, fr: &[String], ids: &[String]) -> Option<String> {
    let (title, content, tags) = invent(rng, fr, ids);
    let project = rng.pick(&PROJECTS).to_string();
    let n = NewMemory { project, kind: Some(Kind::Fact), title, content, tags, ..Default::default() };
    memoria::save(db, n, None).ok().map(|m| m.id)
}

type Snapshot = (BTreeSet<(String, String, String)>, BTreeSet<(String, String)>, BTreeSet<(String, String)>);

/// What the review leaves: outdated (old, new, detail), broken links and links.
fn snapshot(db: &Connection) -> Snapshot {
    let rows = |sql: &str| -> Vec<Vec<String>> {
        let mut q = db.prepare(sql).unwrap();
        let n = q.column_count();
        q.query_map([], |f| (0..n).map(|i| f.get::<_, String>(i)).collect()).unwrap().map(Result::unwrap).collect()
    };
    let outdated = rows("SELECT memory_id, other_id, detail FROM review WHERE kind = 'outdated'").into_iter().map(|v| (v[0].clone(), v[1].clone(), v[2].clone())).collect();
    let broken = rows("SELECT memory_id, detail FROM review WHERE kind = 'broken_link'").into_iter().map(|v| (v[0].clone(), v[1].clone())).collect();
    let links = rows("SELECT from_id, to_id FROM memory_link").into_iter().map(|v| (v[0].clone(), v[1].clone())).collect();
    (outdated, broken, links)
}

/// The review redone in full, on a copy of the database (the original stays partial).
fn full_snapshot(db: &Connection, dir: &Path, k: usize) -> Snapshot {
    let copy = dir.join(format!("entera-{k}.db"));
    db.execute("VACUUM INTO ?1", [copy.to_string_lossy()]).unwrap();
    let c = db::abrir(&copy).unwrap();
    c.execute("DELETE FROM meta WHERE k = 'revision'", []).unwrap();
    revisar::refresh(&c).unwrap();
    snapshot(&c)
}

/// The fingerprint the review saved is the database's: otherwise the next read redoes everything.
fn fingerprint_up_to_date(db: &Connection) -> bool {
    let saved: String = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |f| f.get(0)).unwrap();
    let real: String = db
        .query_row(
            "SELECT count(*) || '|' || COALESCE(max(updated_at), '') || '|' || COALESCE(max(hlc), '') || '|' || total(project_id) || '|'
                 || (SELECT total(space_id) FROM memory WHERE space_id IS NOT NULL) FROM memory",
            [],
            |f| f.get(0),
        )
        .unwrap();
    // 1.12.0: the fingerprint ends with the binary's version, to recompute on upgrade.
    saved == format!("{real}|{}", env!("CARGO_PKG_VERSION"))
}

fn partials(db: &Connection) -> i64 {
    db.query_row("SELECT CAST(v AS INTEGER) FROM meta WHERE k = 'revision_parciales'", [], |f| f.get(0)).unwrap_or(0)
}

#[test]
fn partial_equals_full() {
    let (dir, mut db) = base("igual");
    let fr = sentences();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ids: Vec<String> = Vec::new();
    while ids.len() < revisar::MIN_FOR_INCREMENTAL + 150 {
        if let Some(id) = save(&mut db, &mut rng, &fr, &ids) {
            ids.push(id);
        }
    }
    revisar::refresh(&db).unwrap();
    let initial = snapshot(&db);
    assert!(initial.0.len() > 20 && initial.1.len() > 20 && initial.2.len() > 20, "corpus pobre: {} {} {}", initial.0.len(), initial.1.len(), initial.2.len());

    let mut forgotten: Vec<String> = Vec::new();
    for step in 1..=80 {
        let before = revisar::before(&db).unwrap();
        let touched: Vec<String> = match rng.below(10) {
            0..=3 => save(&mut db, &mut rng, &fr, &ids).into_iter().collect(),
            4..=6 => {
                let id = rng.pick(&ids).clone();
                let (title, content, tags) = invent(&mut rng, &fr, &ids);
                let c = match rng.below(4) {
                    0 => Changes { content: Some(content), tags: Some(tags), ..Default::default() },
                    1 => Changes { title: Some(title), ..Default::default() },
                    2 => Changes { project: Some(rng.pick(&PROJECTS).to_string()), ..Default::default() },
                    _ => Changes { content: Some(content), ..Default::default() },
                };
                memoria::update(&mut db, &id, c, None).ok().map(|m| m.id).into_iter().collect()
            }
            7 => {
                let id = ids.swap_remove(rng.below(ids.len()));
                memoria::forget(&mut db, &id).unwrap();
                forgotten.push(id.clone());
                vec![id]
            }
            8 if !forgotten.is_empty() => {
                let id = forgotten.swap_remove(rng.below(forgotten.len()));
                memoria::restore(&mut db, &id, None).unwrap();
                ids.push(id.clone());
                vec![id]
            }
            _ => {
                let a = rng.pick(&ids).clone();
                let b = rng.pick(&ids).clone();
                if a == b {
                    continue;
                }
                let (_, content, _) = invent(&mut rng, &fr, &ids);
                memoria::absorb(&mut db, &a, std::slice::from_ref(&b), Changes { content: Some(content), ..Default::default() }, None).unwrap();
                ids.retain(|x| *x != b);
                vec![a, b]
            }
        };
        revisar::refresh_for(&db, &before, &touched).unwrap();
        assert!(fingerprint_up_to_date(&db), "huella, paso {step}");
        if step % 20 == 0 {
            let (partial, full) = (snapshot(&db), full_snapshot(&db, &dir, step));
            let different = partial.0.symmetric_difference(&full.0).count();
            eprintln!("paso {step}: {} desfasadas, {} rotos, {} enlaces; {different} desfasadas distintas", full.0.len(), full.1.len(), full.2.len());
            assert_eq!(partial.2, full.2, "enlaces, paso {step}");
            assert_eq!(partial.1, full.1, "enlaces rotos, paso {step}");
            assert_eq!(partial.0, full.0, "desfasadas, paso {step}");
        }
    }
    // It really was reviewed in parts, not by redoing everything at each step.
    assert!(partials(&db) >= 70, "solo {} por partes", partials(&db));
    assert_ne!(snapshot(&db), initial, "las escrituras no han cambiado nada");
}

/// What is approximated: which word is rare is taken from the last full recompute. A word that
/// was right at the cap and starts showing up in more memories still counts as rare until the
/// number of memories changes by more than 5 %; then everything is redone and it adds up again.
#[test]
fn rarity_is_fixed_until_the_next_full_run() {
    let (dir, mut db) = base("rareza");
    let fr = sentences();
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut ids = Vec::new();
    while ids.len() < revisar::MIN_FOR_INCREMENTAL {
        if let Some(id) = save(&mut db, &mut rng, &fr, &ids) {
            ids.push(id);
        }
    }
    // «frontera» in 99 memories of another project: rare (the cap is 100).
    for i in 0..99 {
        let n = NewMemory { project: "/otro".into(), kind: Some(Kind::Fact), title: format!("Nota {i} de la frontera"), content: "Algo sin más.".into(), ..Default::default() };
        memoria::save(&mut db, n, None).unwrap();
    }
    revisar::refresh(&db).unwrap();
    let save_pair = |db: &mut Connection, title: &str, version: &str| {
        let before = revisar::before(db).unwrap();
        let n = NewMemory {
            project: "/frontera".into(),
            kind: Some(Kind::Fact),
            title: title.into(),
            content: format!("La frontera en la {version}."),
            ..Default::default()
        };
        let id = memoria::save(db, n, None).unwrap().id;
        revisar::refresh_for(db, &before, std::slice::from_ref(&id)).unwrap();
    };
    // The 100th and the 101st: in full it's no longer rare and the two share nothing; in parts it still is,
    // they talk about the same thing and 0.2.0 makes 0.1.0 outdated.
    save_pair(&mut db, "Frontera norte", "0.1.0");
    save_pair(&mut db, "Frontera oeste", "0.2.0");
    let partial = snapshot(&db).0.len();
    let full = full_snapshot(&db, &dir, 0).0.len();
    eprintln!("por partes {partial} desfasadas, entera {full}");
    assert_eq!(partial, full + 1, "la diferencia esperada es justo esa pareja");
    // More than 5 % new memories: it's redone in full and adds up.
    for _ in 0..(ids.len() + 99) / 20 + 1 {
        let before = revisar::before(&db).unwrap();
        let id = save(&mut db, &mut rng, &fr, &ids).unwrap();
        revisar::refresh_for(&db, &before, &[id]).unwrap();
    }
    assert_eq!(snapshot(&db), full_snapshot(&db, &dir, 1));
}
