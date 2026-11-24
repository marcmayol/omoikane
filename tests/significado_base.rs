//! The meaning vectors in the database (`memory_sig`), without the model: what is left to compute, that
//! a text change asks for it again, that the scope is respected when comparing and that changing the
//! version of the vectors deletes them.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Scope, Changes, NewMemory, Kind};
use omoikane::significado as s;

fn open_db(name: &str) -> (comun::Temporal, rusqlite::Connection) {
    let dir = comun::Temporal::nueva(&format!("sigbase-{name}"));
    let path = dir.join("o.db");
    let db = db::open(&path).unwrap();
    (dir, db)
}

fn new_fact(project: &str, title: &str, content: &str) -> NewMemory {
    NewMemory { project: project.into(), kind: Some(Kind::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

/// A unit vector in direction `i` (plus a bit of `j`).
fn axis(i: usize, j: usize) -> Vec<f32> {
    let mut v = vec![0f32; omoikane::gemma::DIM];
    v[i] = 0.9;
    v[j] = 0.1;
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / n).collect()
}

#[test]
fn vectors_in_the_db() {
    let (dir, mut db) = open_db("vectores");
    let a = memoria::save(&mut db, new_fact("/p", "Tokens", "El token caduca a los 15 minutos."), None).unwrap();
    let b = memoria::save(&mut db, new_fact("/p", "Gradle", "La caché de Gradle se rompe con dos daemons."), None).unwrap();
    let c = memoria::save(&mut db, new_fact("/otro", "Otro proyecto", "Esto es de otro proyecto."), None).unwrap();

    // At first there are none.
    assert_eq!(s::pending(&db, 10).unwrap().len(), 3);
    assert_eq!(s::coverage(&db).unwrap(), (0, 3));
    assert!(!s::ready(&db));
    let p = s::pending(&db, 10).unwrap();
    let text_a = p.iter().find(|x| x.0 == a.id).unwrap();
    assert_eq!(text_a.2, s::text_of("Tokens", "El token caduca a los 15 minutos."));

    for (m, v) in [(&a, axis(0, 1)), (&b, axis(1, 0)), (&c, axis(0, 2))] {
        let hlc = p.iter().find(|x| x.0 == m.id).unwrap().1.clone();
        s::save(&db, &m.id, &hlc, &v).unwrap();
    }
    assert!(s::pending(&db, 10).unwrap().is_empty());
    assert_eq!(s::coverage(&db).unwrap(), (3, 3));
    assert!(s::ready(&db));

    // Ordered by cosine, and from /p the one from /otro is not seen.
    let scope = Scope::from_project(&db, "/p").unwrap();
    let cos = s::cosines(&db, &axis(0, 1), &scope, None).unwrap();
    assert_eq!(cos.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![a.id.as_str(), b.id.as_str()]);
    assert!((cos[0].1 - 1.0).abs() < 0.01, "{:?}", cos);
    assert_eq!(s::cosines(&db, &axis(0, 1), &Scope::All, None).unwrap().len(), 3);

    // Changing the text asks for it again; pinning it doesn't.
    memoria::update(&mut db, &b.id, Changes { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert!(s::pending(&db, 10).unwrap().is_empty());
    memoria::update(&mut db, &b.id, Changes { content: Some("La caché de Gradle se rompe con tres daemons.".into()), ..Default::default() }, None).unwrap();
    let p = s::pending(&db, 10).unwrap();
    assert_eq!(p.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![b.id.as_str()]);
    assert_eq!(s::coverage(&db).unwrap(), (2, 3));

    // Forgotten for good, its vector goes away when counting.
    memoria::forget_for_good(&mut db, &c.id).unwrap();
    s::coverage(&db).unwrap();
    let n: i64 = db.query_row("SELECT count(*) FROM memory_sig WHERE id = ?1", [&c.id], |f| f.get(0)).unwrap();
    assert_eq!(n, 0);

    // Another version of the vectors (another model, another computation): deleted on open.
    db.execute("UPDATE meta SET v = 'otro' WHERE k = 'sig_version'", []).unwrap();
    drop(db);
    let db = db::open(&dir.join("o.db")).unwrap();
    assert_eq!(s::coverage(&db).unwrap(), (0, 2));
}

/// The service filters the scope in memory (`similar_in_db`); it must see the same as the SQL of
/// `Scope::sql` (`cosines`), with projects with and without a space, space memories and global ones.
#[test]
fn in_memory_scope_matches_sql() {
    let (_dir, mut db) = open_db("ambito");
    let acme = omoikane::proyecto::create_space(&db, "acme").unwrap();
    let globex = omoikane::proyecto::create_space(&db, "globex").unwrap();
    let mut ids = Vec::new();
    for (i, p) in ["/a1", "/a2", "/g1", "/suelto", ""].iter().enumerate() {
        let m = memoria::save(&mut db, new_fact(p, &format!("m{i}"), &format!("contenido {i}")), None).unwrap();
        ids.push(m.id);
    }
    for (p, s) in [("/a1", acme), ("/a2", acme), ("/g1", globex)] {
        let id = omoikane::proyecto::resolve(&db, p).unwrap().unwrap().0;
        omoikane::proyecto::assign_space(&db, id, Some(s)).unwrap();
    }
    for (s, t) in [(acme, "de acme"), (globex, "de globex")] {
        let m = memoria::save(&mut db, NewMemory { space_id: Some(s), kind: Some(Kind::Fact), title: t.into(), content: t.into(), ..Default::default() }, None).unwrap();
        ids.push(m.id);
    }
    for (k, (id, hlc, _)) in s::pending(&db, 100).unwrap().into_iter().enumerate() {
        s::save(&db, &id, &hlc, &axis(k % 700, 700 + k % 60)).unwrap();
    }
    let q = axis(3, 701);
    let mut scopes = vec![Scope::All, Scope::Space(None), Scope::Space(Some(acme)), Scope::Space(Some(globex))];
    for p in ["/a1", "/a2", "/g1", "/suelto", "/nuevo"] {
        scopes.push(Scope::from_project(&db, p).unwrap());
    }
    for a in scopes {
        let mut sql: Vec<String> = s::cosines(&db, &q, &a, None).unwrap().into_iter().map(|x| x.0).collect();
        let mut mem: Vec<String> = s::similar_in_db(&db, &q, &a, None, 100).unwrap().into_iter().map(|x| x.0).collect();
        sql.sort();
        mem.sort();
        assert_eq!(sql, mem, "{a:?}");
        assert!(!sql.is_empty(), "{a:?}");
    }
}
