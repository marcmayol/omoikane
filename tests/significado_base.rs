//! Los vectores de significado en la base (`memory_sig`), sin el modelo: qué falta por calcular, que
//! un cambio de texto lo vuelva a pedir, que el ámbito se respete al comparar y que cambiar de
//! versión de los vectores los borre.

use omoikane::db;
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::significado as s;

fn base(nombre: &str) -> (rusqlite::Connection, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-sigbase-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ruta = dir.join("o.db");
    (db::abrir(&ruta).unwrap(), dir)
}

fn nueva(project: &str, title: &str, content: &str) -> Nueva {
    Nueva { project: project.into(), kind: Some(Tipo::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

/// Un vector unitario en la dirección `i` (más un poco de la `j`).
fn eje(i: usize, j: usize) -> Vec<f32> {
    let mut v = vec![0f32; omoikane::gemma::DIM];
    v[i] = 0.9;
    v[j] = 0.1;
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / n).collect()
}

#[test]
fn vectores_en_la_base() {
    let (mut db, dir) = base("vectores");
    let a = memoria::guardar(&mut db, nueva("/p", "Tokens", "El token caduca a los 15 minutos."), None).unwrap();
    let b = memoria::guardar(&mut db, nueva("/p", "Gradle", "La caché de Gradle se rompe con dos daemons."), None).unwrap();
    let c = memoria::guardar(&mut db, nueva("/otro", "Otro proyecto", "Esto es de otro proyecto."), None).unwrap();

    // Al principio no hay ninguno.
    assert_eq!(s::pendientes(&db, 10).unwrap().len(), 3);
    assert_eq!(s::cobertura(&db).unwrap(), (0, 3));
    assert!(!s::listo(&db));
    let p = s::pendientes(&db, 10).unwrap();
    let texto_a = p.iter().find(|x| x.0 == a.id).unwrap();
    assert_eq!(texto_a.2, s::texto_de("Tokens", "El token caduca a los 15 minutos."));

    for (m, v) in [(&a, eje(0, 1)), (&b, eje(1, 0)), (&c, eje(0, 2))] {
        let hlc = p.iter().find(|x| x.0 == m.id).unwrap().1.clone();
        s::guardar(&db, &m.id, &hlc, &v).unwrap();
    }
    assert!(s::pendientes(&db, 10).unwrap().is_empty());
    assert_eq!(s::cobertura(&db).unwrap(), (3, 3));
    assert!(s::listo(&db));

    // El orden es por coseno, y desde /p no se ve la de /otro.
    let ambito = Ambito::de(&db, "/p").unwrap();
    let cos = s::cosenos(&db, &eje(0, 1), &ambito, None).unwrap();
    assert_eq!(cos.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![a.id.as_str(), b.id.as_str()]);
    assert!((cos[0].1 - 1.0).abs() < 0.01, "{:?}", cos);
    assert_eq!(s::cosenos(&db, &eje(0, 1), &Ambito::Todos, None).unwrap().len(), 3);

    // Cambiar el texto lo vuelve a pedir; fijarla no.
    memoria::actualizar(&mut db, &b.id, Cambios { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert!(s::pendientes(&db, 10).unwrap().is_empty());
    memoria::actualizar(&mut db, &b.id, Cambios { content: Some("La caché de Gradle se rompe con tres daemons.".into()), ..Default::default() }, None).unwrap();
    let p = s::pendientes(&db, 10).unwrap();
    assert_eq!(p.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![b.id.as_str()]);
    assert_eq!(s::cobertura(&db).unwrap(), (2, 3));

    // Olvidada del todo, su vector se va al contar.
    memoria::olvidar_del_todo(&mut db, &c.id).unwrap();
    s::cobertura(&db).unwrap();
    let n: i64 = db.query_row("SELECT count(*) FROM memory_sig WHERE id = ?1", [&c.id], |f| f.get(0)).unwrap();
    assert_eq!(n, 0);

    // Otra versión de los vectores (otro modelo, otro cálculo): se borran al abrir.
    db.execute("UPDATE meta SET v = 'otro' WHERE k = 'sig_version'", []).unwrap();
    drop(db);
    let db = db::abrir(&dir.join("o.db")).unwrap();
    assert_eq!(s::cobertura(&db).unwrap(), (0, 2));
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// El servicio filtra el ámbito en memoria (`parecidas_en_base`); tiene que ver lo mismo que el SQL de
/// `Ambito::sql` (`cosenos`), con proyectos con y sin espacio, memorias de espacio y globales.
#[test]
fn el_ambito_en_memoria_es_el_de_sql() {
    let (mut db, dir) = base("ambito");
    let acme = omoikane::proyecto::crear_espacio(&db, "acme").unwrap();
    let globex = omoikane::proyecto::crear_espacio(&db, "globex").unwrap();
    let mut ids = Vec::new();
    for (i, p) in ["/a1", "/a2", "/g1", "/suelto", ""].iter().enumerate() {
        let m = memoria::guardar(&mut db, nueva(p, &format!("m{i}"), &format!("contenido {i}")), None).unwrap();
        ids.push(m.id);
    }
    for (p, s) in [("/a1", acme), ("/a2", acme), ("/g1", globex)] {
        let id = omoikane::proyecto::resolver(&db, p).unwrap().unwrap().0;
        omoikane::proyecto::asignar_espacio(&db, id, Some(s)).unwrap();
    }
    for (s, t) in [(acme, "de acme"), (globex, "de globex")] {
        let m = memoria::guardar(&mut db, Nueva { space_id: Some(s), kind: Some(Tipo::Fact), title: t.into(), content: t.into(), ..Default::default() }, None).unwrap();
        ids.push(m.id);
    }
    for (k, (id, hlc, _)) in s::pendientes(&db, 100).unwrap().into_iter().enumerate() {
        s::guardar(&db, &id, &hlc, &eje(k % 700, 700 + k % 60)).unwrap();
    }
    let q = eje(3, 701);
    let mut ambitos = vec![Ambito::Todos, Ambito::Espacio(None), Ambito::Espacio(Some(acme)), Ambito::Espacio(Some(globex))];
    for p in ["/a1", "/a2", "/g1", "/suelto", "/nuevo"] {
        ambitos.push(Ambito::de(&db, p).unwrap());
    }
    for a in ambitos {
        let mut sql: Vec<String> = s::cosenos(&db, &q, &a, None).unwrap().into_iter().map(|x| x.0).collect();
        let mut mem: Vec<String> = s::parecidas_en_base(&db, &q, &a, None, 100).unwrap().into_iter().map(|x| x.0).collect();
        sql.sort();
        mem.sort();
        assert_eq!(sql, mem, "{a:?}");
        assert!(!sql.is_empty(), "{a:?}");
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
