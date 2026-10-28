//! Fase 2: vectores al día, búsqueda híbrida y duplicados. Necesitan el modelo int8 (ver la
//! cabecera de tests/equivalencia.rs); sin él, avisan y no comprueban nada.

use omoikane::buscar::{self, Opciones};
use omoikane::embeber::{Modelo, compilar};
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::{db, vectores};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::OnceLock;

/// El modelo compilado una sola vez para todos los tests de este fichero.
fn modelo() -> Option<Modelo> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let int8 = std::env::var_os("OMOI_MODELO_INT8")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
        if !int8.join("model.safetensors").exists() {
            return None;
        }
        let destino = std::env::temp_dir().join(format!("omoi-test-modelo-{}", std::process::id())).join("modelo-test");
        compilar(&int8, &destino).expect("compilar");
        Some(destino)
    });
    match dir {
        Some(d) => Some(Modelo::abrir(d).expect("abrir")),
        None => {
            eprintln!("SALTADO: no está el modelo int8 (ver la cabecera de tests/equivalencia.rs)");
            None
        }
    }
}

fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-vectores-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (db::abrir(&dir.join("o.db")).unwrap(), dir)
}

fn nueva(project: &str, title: &str, content: &str) -> Nueva {
    Nueva { project: project.into(), kind: Some(Tipo::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

fn cuenta(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |f| f.get(0)).unwrap()
}

#[test]
fn los_vectores_siguen_a_las_memorias() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("al-dia");
    // Dos guardadas sin modelo (como antes de instalarlo) y una con él.
    let a = memoria::guardar(&mut db, nueva("/p", "Uno", "primero"), None).unwrap();
    memoria::guardar(&mut db, nueva("/p", "Dos", "segundo"), None).unwrap();
    memoria::guardar(&mut db, nueva("/p", "Tres", "tercero"), Some(&m)).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), 1);
    // sincronizar completa las que faltan y apunta el modelo; la segunda vez no hace nada.
    assert_eq!(vectores::sincronizar(&mut db, &m).unwrap(), 3);
    assert_eq!(vectores::sincronizar(&mut db, &m).unwrap(), 0);
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), 3);
    // Cambiar el texto sin modelo invalida su vector; sincronizar lo rehace.
    memoria::actualizar(&mut db, &a.id, Cambios { content: Some("otro".into()), ..Default::default() }, None).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), 2);
    assert_eq!(vectores::sincronizar(&mut db, &m).unwrap(), 1);
    // Fijar no cambia el texto: el vector se queda.
    memoria::actualizar(&mut db, &a.id, Cambios { pinned: Some(true), ..Default::default() }, None).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), 3);
    // Si los vectores son de otro modelo, se rehacen todos.
    db.execute("UPDATE meta SET v = 'otro-modelo' WHERE k = 'modelo'", []).unwrap();
    assert_eq!(vectores::sincronizar(&mut db, &m).unwrap(), 3);
    // Olvidar se lleva el vector.
    memoria::olvidar(&mut db, &a.id).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), 2);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn el_vector_es_el_del_texto_nuevo() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("texto-nuevo");
    let a = memoria::guardar(&mut db, nueva("/p", "Puerto", "El servidor escucha en el 8765."), Some(&m)).unwrap();
    let c = Cambios { content: Some("La tortilla lleva cebolla.".into()), ..Default::default() };
    memoria::actualizar(&mut db, &a.id, c, Some(&m)).unwrap();
    let p = vectores::parecidas(&db, &m, &vectores::texto("Puerto", "La tortilla lleva cebolla."), None, None, -1.0, 1).unwrap();
    assert!(p[0].1 > 0.999, "similitud {}", p[0].1);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn duplicados_del_mismo_ambito() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("duplicados");
    let n = nueva("/p", "Bug en auth: el token caduca antes de tiempo", "El middleware compara la hora local con la caducidad en UTC.");
    memoria::guardar(&mut db, n, Some(&m)).unwrap();
    // Unas cuantas memorias más, como en una base de verdad (la media sale de ellas).
    for (t, c) in [("Puerto", "El servidor escucha en el 8765."), ("Commits", "Una línea, en inglés."), ("Node", "Hace falta Node 20.")] {
        memoria::guardar(&mut db, nueva("/p", t, c), Some(&m)).unwrap();
    }
    // La misma memoria dicha de otra forma, en el mismo proyecto: se detecta.
    let d = vectores::duplicada(&db, &m, "/p", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap();
    assert!(d.is_some());
    // Mismo tema, otra cosa: no.
    let d = vectores::duplicada(&db, &m, "/p", "Los tokens se firman con RS256", "La clave pública está en el servicio de auth.").unwrap();
    assert!(d.is_none(), "{d:?}");
    // En otro proyecto no cuenta, y siendo global solo se compara con las globales.
    assert!(vectores::duplicada(&db, &m, "/otro", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap().is_none());
    assert!(vectores::duplicada(&db, &m, "", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token, que va en UTC.").unwrap().is_none());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

fn pares() -> Vec<(String, String, String)> {
    include_str!("pares_duplicados.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0].into(), c[1].into(), c[2].into())
        })
        .collect()
}

/// El umbral de duplicados, por el camino de verdad (vectores centrados con la media de la base):
/// ningún falso positivo en los pares de calibración y al menos 4 de cada 10 duplicados detectados.
/// El umbral está puesto para no molestar con notas reales (ver vectores::SIMILITUD_DUPLICADO), así
/// que con frases cortas deja pasar duplicados: es el precio de no avisar en falso.
#[test]
fn umbral_de_duplicados() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("umbral");
    let pares = pares();
    let mut vistas = std::collections::HashSet::new();
    for (_, a, _) in &pares {
        if vistas.insert(a.clone()) {
            memoria::guardar(&mut db, nueva("/p", a, a), Some(&m)).unwrap();
        }
    }
    let (mut acertados, mut duplicados, mut falsos) = (0, 0, Vec::new());
    for (clase, a, b) in &pares {
        let p = vectores::parecidas(&db, &m, b, Some("/p"), None, -1.0, 100).unwrap();
        let s = p.iter().find(|(mem, _)| &mem.title == a).map(|x| x.1).unwrap();
        let detectado = s >= vectores::SIMILITUD_DUPLICADO;
        if clase == "dup" {
            duplicados += 1;
            acertados += usize::from(detectado);
        } else if detectado {
            falsos.push(format!("{s:.3} «{a}» / «{b}»"));
        }
    }
    eprintln!("duplicados detectados: {acertados} de {duplicados}");
    assert!(falsos.is_empty(), "falsos positivos: {falsos:?}");
    assert!(acertados * 10 >= duplicados * 4, "solo {acertados} de {duplicados} duplicados");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// La búsqueda con tests/busquedas.tsv no empeora: con modelo al menos tan bien como sin él, y las
/// consultas sin respuesta casi no devuelven nada.
#[test]
fn calidad_de_la_busqueda() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("calidad");
    let mut claves = std::collections::HashMap::new();
    let mut consultas = Vec::new();
    for l in include_str!("busquedas.tsv").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let c: Vec<&str> = l.split('\t').collect();
        if c[0] == "m" {
            let mem = memoria::guardar(&mut db, nueva("/p", c[2], c[3]), Some(&m)).unwrap();
            claves.insert(mem.id, c[1].to_string());
        } else {
            consultas.push((c[1].to_string(), c[2].to_string()));
        }
    }
    let o = Opciones { ambito: Ambito::Todos, kind: None, tags: vec![], limite: 5 };
    let mut medir = |con: Option<&Modelo>| {
        let (mut top1, mut ruido) = (0, 0);
        for (clave, q) in &consultas {
            let r = buscar::buscar(&mut db, q, &o, con).unwrap();
            match (clave.as_str(), r.first()) {
                ("-", Some(_)) => ruido += 1,
                (k, Some(p)) if claves[&p.memoria.id] == k => top1 += 1,
                _ => {}
            }
        }
        (top1, ruido)
    };
    let (sin, ruido_sin) = medir(None);
    let (con, ruido_con) = medir(Some(&m));
    // El vector solo rescata: nunca puede quitarle aciertos a las palabras ni meter ruido.
    assert!(con >= sin, "con modelo {con}, sin modelo {sin}");
    assert_eq!(ruido_sin, 0);
    assert_eq!(ruido_con, 0, "{ruido_con} consultas sin respuesta devuelven algo");
    // Lo que sale por palabras lleva también su similitud.
    let r = buscar::buscar(&mut db, "el token caduca antes de tiempo", &o, Some(&m)).unwrap();
    assert_eq!(claves[&r[0].memoria.id], "auth");
    assert!(r[0].similarity.is_some());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn coherencia_con_vectores() {
    let Some(m) = modelo() else { return };
    let (mut db, dir) = base("coherencia");
    let mut ids = Vec::new();
    for i in 0..60 {
        let usa = if i % 3 == 0 { None } else { Some(&m) };
        ids.push(memoria::guardar(&mut db, nueva("/p", &format!("nota {i}"), &format!("contenido {i}")), usa).unwrap().id);
    }
    for (i, id) in ids.iter().enumerate() {
        match i % 4 {
            0 => {
                memoria::olvidar(&mut db, id).unwrap();
            }
            1 => {
                let c = Cambios { content: Some(format!("cambiado {i}")), ..Default::default() };
                memoria::actualizar(&mut db, id, c, if i % 2 == 0 { Some(&m) } else { None }).unwrap();
            }
            _ => {}
        }
    }
    vectores::sincronizar(&mut db, &m).unwrap();
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec"), cuenta(&db, "SELECT count(*) FROM memory"));
    assert_eq!(cuenta(&db, "SELECT count(*) FROM memory_vec WHERE n NOT IN (SELECT n FROM memory)"), 0);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
