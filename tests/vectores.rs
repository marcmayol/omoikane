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

/// 1.8.6: duplicados entre memorias de oficio. Desde lo global no se ven las de oficio, así que
/// `duplicada` no las encontraba: lo mismo promovido o guardado desde dos clientes acababa dos veces.
/// `duplicada_de_oficio` compara con las de sus oficios, y la revisión propone juntar las que ya hay.
#[test]
fn duplicados_entre_memorias_de_oficio() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("duplicados-oficio");
    let de_oficio = |oficio: &str, t: &str, c: &str| Nueva { tags: vec![format!("trade:{oficio}")], ..nueva("", t, c) };
    let vieja = memoria::guardar(&mut db, de_oficio("android", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill."), Some(&m)).unwrap().id;
    for (t, c) in [("Puerto", "El servidor escucha en el 8765."), ("Commits", "Una línea, en inglés."), ("Node", "Hace falta Node 20.")] {
        memoria::guardar(&mut db, nueva("/p", t, c), Some(&m)).unwrap();
    }
    // La de un proyecto no es de oficio aunque diga lo mismo: no cuenta.
    memoria::guardar(&mut db, nueva("/cliente", "Cerrar el emulador", "Con taskkill el AVD queda bloqueado."), Some(&m)).unwrap();
    let (t, c) = ("El emulador no se mata con taskkill", "Con taskkill el AVD se queda bloqueado: cerrarlo con adb emu kill.");
    // Antes: desde lo global, nada.
    assert!(vectores::duplicada(&db, &m, "", t, c).unwrap().is_none());
    let d = vectores::duplicada_de_oficio(&db, &m, &["android".to_string()], t, c, None).unwrap();
    assert_eq!(d.map(|x| x.0.id), Some(vieja.clone()));
    // Otro oficio no la ve; y al promover, la de la que sale no cuenta.
    assert!(vectores::duplicada_de_oficio(&db, &m, &["web".to_string()], t, c, None).unwrap().is_none());
    assert!(vectores::duplicada_de_oficio(&db, &m, &["android".to_string()], t, c, Some(&vieja)).unwrap().is_none());

    // Si ya hay dos (guardada con `force`, o de antes de la 1.8.6), la revisión las propone para juntar.
    let nueva_id = memoria::guardar(&mut db, de_oficio("android", t, c), Some(&m)).unwrap().id;
    memoria::guardar(&mut db, de_oficio("web", t, c), Some(&m)).unwrap();
    let pares = omoikane::revisar::duplicadas_de_oficio(&db, &Ambito::Todos).unwrap();
    assert_eq!(pares.len(), 1, "solo las del mismo oficio: {pares:?}");
    assert_eq!((pares[0].0.as_str(), pares[0].1.as_str()), (nueva_id.as_str(), vieja.as_str()));
    let avisos = omoikane::revisar::avisos(&db, &Ambito::Todos).unwrap();
    let a: Vec<_> = avisos.iter().filter(|a| a.kind == omoikane::revisar::DUPLICADA_OFICIO).collect();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].memory_id.as_str(), a[0].other_id.as_str()), (vieja.as_str(), nueva_id.as_str()));
    assert_eq!(omoikane::revisar::pendientes(&db, &Ambito::Todos).unwrap().get(omoikane::revisar::DUPLICADA_OFICIO), Some(&1));
    // «Está bien así»: dejan de salir.
    omoikane::revisar::descartar(&db, &vieja, omoikane::revisar::DUPLICADA_OFICIO, &nueva_id).unwrap();
    assert!(omoikane::revisar::duplicadas_de_oficio(&db, &Ambito::Todos).unwrap().is_empty());
}

/// 1.8.6: `duplicada_de_oficio` tomaba las 50 más parecidas de toda la base y luego se quedaba con las
/// globales y las de sus oficios. Con muchas memorias de proyecto parecidas, la de oficio duplicada de
/// verdad quedaba fuera de esas 50. Ahora busca solo entre las que cuentan.
#[test]
fn la_duplicada_de_oficio_no_la_tapan_las_de_proyecto() {
    let Some(m) = modelo() else { return };
    let (_dir, mut db) = base("duplicada-tapada");
    let (t, c) = ("El emulador no se mata con taskkill", "Con taskkill el AVD se queda bloqueado: cerrarlo con adb emu kill.");
    // Una base de verdad tiene de todo (si no, la media con la que se centran los vectores sería este
    // mismo tema): las frases de `corpus.txt`, en otro proyecto.
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    for (i, l) in corpus.lines().filter(|l| !l.starts_with('#') && l.chars().filter(|c| c.is_alphabetic()).count() > 30).take(600).enumerate() {
        memoria::guardar(&mut db, nueva("/otro", &format!("Nota {i}"), l), Some(&m)).unwrap();
    }
    // 60 de proyectos que dicen justo lo mismo: las más parecidas de toda la base.
    for i in 0..60 {
        memoria::guardar(&mut db, nueva(&format!("/p{i}"), t, c), Some(&m)).unwrap();
    }
    let n = Nueva { tags: vec!["trade:android".into()], ..nueva("", "No mates el emulador con taskkill", "Cerrarlo con taskkill deja el AVD bloqueado; usa adb emu kill.") };
    let vieja = memoria::guardar(&mut db, n, Some(&m)).unwrap().id;
    let d = vectores::duplicada_de_oficio(&db, &m, &["android".to_string()], t, c, None).unwrap();
    assert_eq!(d.map(|x| x.0.id), Some(vieja));
}
