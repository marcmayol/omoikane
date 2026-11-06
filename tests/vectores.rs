//! Fase 2: vectores al día, búsqueda híbrida y duplicados. Necesitan el modelo int8 (ver la
//! cabecera de tests/equivalencia.rs); sin él, avisan y no comprueban nada.

mod comun;

use omoikane::buscar::{self, Opciones};
use omoikane::embeber::{Modelo, compilar};
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::{db, vectores};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

/// El modelo compilado una vez para los tests de este fichero que corren a la vez. Su carpeta temporal
/// se borra cuando el último lo suelta (los ficheros mapeados no se pueden borrar antes en Windows); si
/// otro lo pide después, se compila de nuevo.
struct ConModelo(Option<Modelo>);

static COMPILADO: Mutex<Option<(comun::Temporal, usize)>> = Mutex::new(None);

impl std::ops::Deref for ConModelo {
    type Target = Modelo;

    fn deref(&self) -> &Modelo {
        self.0.as_ref().unwrap()
    }
}

impl Drop for ConModelo {
    fn drop(&mut self) {
        let mut c = COMPILADO.lock().unwrap_or_else(|e| e.into_inner());
        // Primero se suelta el modelo y luego, si era el último, su carpeta.
        self.0 = None;
        if let Some((_, usos)) = c.as_mut() {
            *usos -= 1;
            if *usos == 0 {
                *c = None;
            }
        }
    }
}

fn modelo() -> Option<ConModelo> {
    let int8 = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))
        .filter(|d| d.join("model.safetensors").exists());
    let Some(int8) = int8 else {
        eprintln!("SALTADO: no está el modelo int8 (ver la cabecera de tests/equivalencia.rs)");
        return None;
    };
    let mut c = COMPILADO.lock().unwrap_or_else(|e| e.into_inner());
    if c.is_none() {
        let dir = comun::Temporal::nueva("test-modelo");
        compilar(&int8, &dir.join("modelo-test")).expect("compilar");
        *c = Some((dir, 0));
    }
    let (dir, usos) = c.as_mut().unwrap();
    let m = Modelo::abrir(&dir.join("modelo-test")).expect("abrir");
    *usos += 1;
    Some(ConModelo(Some(m)))
}

fn base(nombre: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("vectores-{nombre}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
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
    let (_dir, mut db) = base("al-dia");
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
}

#[test]
fn el_vector_es_el_del_texto_nuevo() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("texto-nuevo");
    let a = memoria::guardar(&mut db, nueva("/p", "Puerto", "El servidor escucha en el 8765."), Some(&m)).unwrap();
    let c = Cambios { content: Some("La tortilla lleva cebolla.".into()), ..Default::default() };
    memoria::actualizar(&mut db, &a.id, c, Some(&m)).unwrap();
    let p = vectores::parecidas(&db, &m, &vectores::texto("Puerto", "La tortilla lleva cebolla."), None, None, -1.0, 1).unwrap();
    assert!(p[0].1 > 0.999, "similitud {}", p[0].1);
}

#[test]
fn duplicados_del_mismo_ambito() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("duplicados");
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

/// El aviso de duplicados, por el camino de verdad (`vectores::duplicada`: vector centrado y
/// palabras): ningún aviso en falso con los pares de calibración (ni de su pareja ni de otra frase de
/// la base) y al menos un tercio de los duplicados detectados, apuntando a su pareja. Estos pares son
/// paráfrasis cortas con otras palabras, lo más difícil para la regla: solo los coge el vector (desde
/// 0,78). Con 0,70 se cogían algunos más, pero en notas reales avisaba en falso el 3,9 % de las veces
/// y se escapaban un tercio de las notas guardadas dos veces (ver vectores::SIMILITUD_DUPLICADO).
#[test]
fn umbral_de_duplicados() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("umbral");
    let pares = pares();
    let mut vistas = std::collections::HashSet::new();
    for (_, a, _) in &pares {
        if vistas.insert(a.clone()) {
            memoria::guardar(&mut db, nueva("/p", a, a), Some(&m)).unwrap();
        }
    }
    let (mut acertados, mut duplicados, mut falsos) = (0, 0, Vec::new());
    for (clase, a, b) in &pares {
        let d = vectores::duplicada(&db, &m, "/p", b, b).unwrap();
        if clase == "dup" {
            duplicados += 1;
            acertados += usize::from(d.as_ref().is_some_and(|(mem, _)| &mem.title == a));
        } else if let Some((mem, s)) = d
            // Alguna B es la A de otro par: si esa frase ya está guardada, avisar es lo correcto.
            && &mem.title != b
        {
            falsos.push(format!("{s:.3} «{}» / «{b}»", mem.title));
        }
    }
    eprintln!("duplicados detectados: {acertados} de {duplicados}");
    assert!(falsos.is_empty(), "falsos positivos: {falsos:?}");
    assert!(acertados * 3 >= duplicados, "solo {acertados} de {duplicados} duplicados");
}

/// La búsqueda con tests/busquedas.tsv no empeora: con modelo al menos tan bien como sin él, y las
/// consultas sin respuesta casi no devuelven nada.
#[test]
fn calidad_de_la_busqueda() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("calidad");
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
}

#[test]
fn coherencia_con_vectores() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("coherencia");
    let mut ids = Vec::new();
    for i in 0..60 {
        let usa = if i % 3 == 0 { None } else { Some(&*m) };
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
}
