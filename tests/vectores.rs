//! Phase 2: up-to-date vectors, hybrid search and duplicates. They need the int8 model (see the
//! header of tests/equivalencia.rs); without it, they warn and check nothing.

mod comun;

use omoikane::buscar::{self, SearchOptions};
use omoikane::embeber::{Model, compile};
use omoikane::memoria::{self, Scope, Changes, NewMemory, Kind};
use omoikane::{db, vectores};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

/// The model compiled once for this file's tests that run at the same time. Its temporary folder
/// is deleted when the last one releases it (mapped files can't be deleted earlier on Windows); if
/// another one asks for it later, it is compiled again.
struct WithModel(Option<Model>);

static COMPILED: Mutex<Option<(comun::Temporal, usize)>> = Mutex::new(None);

impl std::ops::Deref for WithModel {
    type Target = Model;

    fn deref(&self) -> &Model {
        self.0.as_ref().unwrap()
    }
}

impl Drop for WithModel {
    fn drop(&mut self) {
        let mut c = COMPILED.lock().unwrap_or_else(|e| e.into_inner());
        // First the model is released and then, if it was the last one, its folder.
        self.0 = None;
        if let Some((_, uses)) = c.as_mut() {
            *uses -= 1;
            if *uses == 0 {
                *c = None;
            }
        }
    }
}

fn model() -> Option<WithModel> {
    let int8 = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))
        .filter(|d| d.join("model.safetensors").exists());
    let Some(int8) = int8 else {
        eprintln!("SALTADO: no está el modelo int8 (ver la cabecera de tests/equivalencia.rs)");
        return None;
    };
    let mut c = COMPILED.lock().unwrap_or_else(|e| e.into_inner());
    if c.is_none() {
        let dir = comun::Temporal::nueva("test-modelo");
        compile(&int8, &dir.join("modelo-test")).expect("compilar");
        *c = Some((dir, 0));
    }
    let (dir, uses) = c.as_mut().unwrap();
    let m = Model::open(&dir.join("modelo-test")).expect("abrir");
    *uses += 1;
    Some(WithModel(Some(m)))
}

fn fresh_db(name: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("vectores-{name}"));
    let db = db::open(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn new_memory(project: &str, title: &str, content: &str) -> NewMemory {
    NewMemory { project: project.into(), kind: Some(Kind::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

#[test]
fn vectors_follow_the_memories() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("al-dia");
    // Two saved without a model (as before installing it) and one with it.
    let a = memoria::save(&mut db, new_memory("/p", "Uno", "primero"), None).unwrap();
    memoria::save(&mut db, new_memory("/p", "Dos", "segundo"), None).unwrap();
    memoria::save(&mut db, new_memory("/p", "Tres", "tercero"), Some(&m)).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), 1);
    // `sync` fills in the missing ones and records the model; the second time it does nothing.
    assert_eq!(vectores::sync(&mut db, &m).unwrap(), 3);
    assert_eq!(vectores::sync(&mut db, &m).unwrap(), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), 3);
    // Changing the text without a model invalidates its vector; `sync` rebuilds it.
    memoria::update(&mut db, &a.id, Changes { content: Some("otro".into()), ..Default::default() }, None).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), 2);
    assert_eq!(vectores::sync(&mut db, &m).unwrap(), 1);
    // Pinning doesn't change the text: the vector stays.
    memoria::update(&mut db, &a.id, Changes { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), 3);
    // If the vectors are from another model, they are all rebuilt.
    db.execute("UPDATE meta SET v = 'otro-modelo' WHERE k = 'modelo'", []).unwrap();
    assert_eq!(vectores::sync(&mut db, &m).unwrap(), 3);
    // Forgetting takes the vector with it.
    memoria::forget(&mut db, &a.id).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), 2);
}

#[test]
fn the_vector_is_the_new_texts() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("texto-nuevo");
    let a = memoria::save(&mut db, new_memory("/p", "Puerto", "El servidor escucha en el 8765."), Some(&m)).unwrap();
    let c = Changes { content: Some("La tortilla lleva cebolla.".into()), ..Default::default() };
    memoria::update(&mut db, &a.id, c, Some(&m)).unwrap();
    let p = vectores::similar(&db, &m, &vectores::text("Puerto", "La tortilla lleva cebolla."), None, None, -1.0, 1).unwrap();
    assert!(p[0].1 > 0.999, "similitud {}", p[0].1);
}

#[test]
fn duplicates_in_the_same_scope() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("duplicados");
    let n = new_memory("/p", "Bug en auth: el token caduca antes de tiempo", "El middleware compara la hora local con la caducidad en UTC.");
    memoria::save(&mut db, n, Some(&m)).unwrap();
    // A few more memories, as in a real database (the mean comes from them).
    for (t, c) in [("Puerto", "El servidor escucha en el 8765."), ("Commits", "Una línea, en inglés."), ("Node", "Hace falta Node 20.")] {
        memoria::save(&mut db, new_memory("/p", t, c), Some(&m)).unwrap();
    }
    // The same memory said another way, in the same project: it is detected.
    let d = vectores::duplicate(&db, &m, "/p", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap();
    assert!(d.is_some());
    // Same topic, different thing: no.
    let d = vectores::duplicate(&db, &m, "/p", "Los tokens se firman con RS256", "La clave pública está en el servicio de auth.").unwrap();
    assert!(d.is_none(), "{d:?}");
    // In another project it doesn't count, and being global it is only compared with the global ones.
    assert!(vectores::duplicate(&db, &m, "/otro", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap().is_none());
    assert!(vectores::duplicate(&db, &m, "", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap().is_none());
}

fn pairs() -> Vec<(String, String, String)> {
    include_str!("pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].into(), c[1].into(), c[2].into())
        })
        .collect()
}

/// The duplicate warning, through the real path (`vectores::duplicate`: centered vector and
/// words): no false warning with the calibration pairs (neither from its pair nor from another sentence in
/// the database) and at least a third of the duplicates detected, pointing to their pair. These pairs are
/// short paraphrases with other words, the hardest for the rule: only the vector catches them (from
/// 0.78). With 0.70 a few more were caught, but on real notes it warned falsely 3.9 % of the time
/// and a third of the notes saved twice slipped through (see vectores::DUPLICATE_SIMILARITY).
#[test]
fn duplicate_threshold() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("umbral");
    let pairs = pairs();
    let mut seen = std::collections::HashSet::new();
    for (_, a, _) in &pairs {
        if seen.insert(a.clone()) {
            memoria::save(&mut db, new_memory("/p", a, a), Some(&m)).unwrap();
        }
    }
    let (mut hits, mut duplicates, mut false_positives) = (0, 0, Vec::new());
    for (class, a, b) in &pairs {
        let d = vectores::duplicate(&db, &m, "/p", b, b).unwrap();
        if class == "dup" {
            duplicates += 1;
            hits += usize::from(d.as_ref().is_some_and(|(mem, _)| &mem.title == a));
        } else if let Some((mem, s)) = d
            // Some B is another pair's A: if that sentence is already saved, warning is right.
            && &mem.title != b
        {
            false_positives.push(format!("{s:.3} «{}» / «{b}»", mem.title));
        }
    }
    eprintln!("duplicados detectados: {hits} de {duplicates}");
    assert!(false_positives.is_empty(), "falsos positivos: {false_positives:?}");
    assert!(hits * 3 >= duplicates, "solo {hits} de {duplicates} duplicados");
}

/// Search with tests/busquedas.tsv doesn't get worse: with a model at least as well as without it, and
/// queries with no answer return almost nothing.
#[test]
fn search_quality() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("calidad");
    let mut keys = std::collections::HashMap::new();
    let mut queries = Vec::new();
    for l in include_str!("busquedas.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        if c[0] == "m" {
            let mem = memoria::save(&mut db, new_memory("/p", c[2], c[3]), Some(&m)).unwrap();
            keys.insert(mem.id, c[1].to_string());
        } else {
            queries.push((c[1].to_string(), c[2].to_string()));
        }
    }
    let o = SearchOptions { scope: Scope::All, kind: None, tags: vec![], limit: 5 };
    let mut measure = |with: Option<&Model>| {
        let (mut top1, mut noise) = (0, 0);
        for (key, q) in &queries {
            let r = buscar::search(&mut db, q, &o, with).unwrap();
            match (key.as_str(), r.first()) {
                ("-", Some(_)) => noise += 1,
                (k, Some(p)) if keys[&p.memory.id] == k => top1 += 1,
                _ => {}
            }
        }
        (top1, noise)
    };
    let (without, noise_without) = measure(None);
    let (with, noise_with) = measure(Some(&m));
    // The vector only rescues: it can never take hits away from the words nor add noise.
    assert!(with >= without, "con modelo {with}, sin modelo {without}");
    assert_eq!(noise_without, 0);
    assert_eq!(noise_with, 0, "{noise_with} consultas sin respuesta devuelven algo");
    // What comes up by words also carries its similarity.
    let r = buscar::search(&mut db, "el token caduca antes de tiempo", &o, Some(&m)).unwrap();
    assert_eq!(keys[&r[0].memory.id], "auth");
    assert!(r[0].similarity.is_some());
}

#[test]
fn consistency_with_vectors() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("coherencia");
    let mut ids = Vec::new();
    for i in 0..60 {
        let uses = if i % 3 == 0 { None } else { Some(&*m) };
        ids.push(memoria::save(&mut db, new_memory("/p", &format!("nota {i}"), &format!("contenido {i}")), uses).unwrap().id);
    }
    for (i, id) in ids.iter().enumerate() {
        match i % 4 {
            0 => {
                memoria::forget(&mut db, id).unwrap();
            }
            1 => {
                let c = Changes { content: Some(format!("cambiado {i}")), ..Default::default() };
                memoria::update(&mut db, id, c, if i % 2 == 0 { Some(&m) } else { None }).unwrap();
            }
            _ => {}
        }
    }
    vectores::sync(&mut db, &m).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec"), count(&db, "SELECT count(*) FROM memory"));
    assert_eq!(count(&db, "SELECT count(*) FROM memory_vec WHERE n NOT IN (SELECT n FROM memory)"), 0);
}

/// 1.8.6: duplicates among trade memories. From the global scope trade ones aren't visible, so
/// `duplicate` didn't find them: the same thing promoted or saved from two clients ended up twice.
/// `trade_duplicate` compares with those of its trades, and the review proposes merging the ones already there.
#[test]
fn duplicates_among_trade_memories() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("duplicados-oficio");
    let trade_memory = |trade: &str, t: &str, c: &str| NewMemory { tags: vec![format!("trade:{trade}")], ..new_memory("", t, c) };
    let old = memoria::save(&mut db, trade_memory("android", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill."), Some(&m)).unwrap().id;
    for (t, c) in [("Puerto", "El servidor escucha en el 8765."), ("Commits", "Una línea, en inglés."), ("Node", "Hace falta Node 20.")] {
        memoria::save(&mut db, new_memory("/p", t, c), Some(&m)).unwrap();
    }
    // A project's memory isn't a trade one even if it says the same: it doesn't count.
    memoria::save(&mut db, new_memory("/cliente", "Cerrar el emulador", "Con taskkill el AVD queda bloqueado."), Some(&m)).unwrap();
    let (t, c) = ("El emulador no se mata con taskkill", "Con taskkill el AVD se queda bloqueado: cerrarlo con adb emu kill.");
    // Before: from the global scope, nothing.
    assert!(vectores::duplicate(&db, &m, "", t, c).unwrap().is_none());
    let d = vectores::trade_duplicate(&db, &m, &["android".to_string()], t, c, None).unwrap();
    assert_eq!(d.map(|x| x.0.id), Some(old.clone()));
    // Another trade doesn't see it; and when promoting, the one it comes from doesn't count.
    assert!(vectores::trade_duplicate(&db, &m, &["web".to_string()], t, c, None).unwrap().is_none());
    assert!(vectores::trade_duplicate(&db, &m, &["android".to_string()], t, c, Some(&old)).unwrap().is_none());

    // If there are already two (saved with `force`, or from before 1.8.6), the review proposes merging them.
    let new_id = memoria::save(&mut db, trade_memory("android", t, c), Some(&m)).unwrap().id;
    memoria::save(&mut db, trade_memory("web", t, c), Some(&m)).unwrap();
    let pairs = omoikane::revisar::trade_duplicates(&db, &Scope::All).unwrap();
    assert_eq!(pairs.len(), 1, "solo las del mismo oficio: {pairs:?}");
    assert_eq!((pairs[0].0.as_str(), pairs[0].1.as_str()), (new_id.as_str(), old.as_str()));
    let warnings = omoikane::revisar::notices(&db, &Scope::All).unwrap();
    let a: Vec<_> = warnings.iter().filter(|a| a.kind == omoikane::revisar::TRADE_DUPLICATE).collect();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].memory_id.as_str(), a[0].other_id.as_str()), (old.as_str(), new_id.as_str()));
    assert_eq!(omoikane::revisar::pending(&db, &Scope::All).unwrap().get(omoikane::revisar::TRADE_DUPLICATE), Some(&1));
    // «It's fine like this»: they stop showing up.
    omoikane::revisar::dismiss(&db, &old, omoikane::revisar::TRADE_DUPLICATE, &new_id).unwrap();
    assert!(omoikane::revisar::trade_duplicates(&db, &Scope::All).unwrap().is_empty());
}

/// 1.8.6: `trade_duplicate` took the 50 most similar of the whole database and then kept the
/// global ones and those of its trades. With many similar project memories, the truly duplicated trade one
/// was left out of those 50. Now it searches only among the ones that count.
#[test]
fn the_trade_duplicate_is_not_hidden_by_project_ones() {
    let Some(m) = model() else { return };
    let (_dir, mut db) = fresh_db("duplicada-tapada");
    let (t, c) = ("El emulador no se mata con taskkill", "Con taskkill el AVD se queda bloqueado: cerrarlo con adb emu kill.");
    // A real database has everything (otherwise, the mean the vectors are centered with would be this
    // same topic): the sentences of `corpus.txt`, in another project.
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    for (i, l) in corpus.lines().filter(|l| !l.starts_with('#') && l.chars().filter(|c| c.is_alphabetic()).count() > 30).take(600).enumerate() {
        memoria::save(&mut db, new_memory("/otro", &format!("Nota {i}"), l), Some(&m)).unwrap();
    }
    // 60 from projects that say exactly the same: the most similar of the whole database.
    for i in 0..60 {
        memoria::save(&mut db, new_memory(&format!("/p{i}"), t, c), Some(&m)).unwrap();
    }
    let n = NewMemory { tags: vec!["trade:android".into()], ..new_memory("", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill.") };
    let old = memoria::save(&mut db, n, Some(&m)).unwrap().id;
    let d = vectores::trade_duplicate(&db, &m, &["android".to_string()], t, c, None).unwrap();
    assert_eq!(d.map(|x| x.0.id), Some(old));
}
