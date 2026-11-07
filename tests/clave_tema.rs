//! 1.8.0: la clave de tema (`remember --key`, etiqueta `topic:<clave>`). Una memoria por clave y por
//! ámbito: guardar con una clave que ya está la actualiza, también con varios procesos a la vez (lo
//! que hace Kumi con el perfil de cada proyecto, con dos factorías en el mismo repositorio).

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use rusqlite::Connection;
use std::path::Path;
use std::process::{Command, Stdio};

fn base(nombre: &str) -> (comun::Temporal, Connection) {
    let dir = comun::Temporal::nueva(&format!("clave-{nombre}"));
    let db = db::abrir(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn nueva(project: &str, title: &str, content: &str) -> Nueva {
    Nueva { project: project.into(), kind: Some(Tipo::Fact), title: title.into(), content: content.into(), ..Default::default() }
}

fn con_clave(db: &Connection, clave: &str) -> i64 {
    db.query_row(
        "SELECT count(*) FROM memory m WHERE EXISTS (SELECT 1 FROM json_each(m.tags) WHERE value = ?1)",
        [format!("topic:{clave}")],
        |f| f.get(0),
    )
    .unwrap()
}

#[test]
fn la_clave_crea_una_vez_y_luego_actualiza() {
    let (_dir, mut db) = base("crea");
    let a = memoria::guardar_por_clave(&mut db, "Kumi-Perfil", nueva("/p", "Perfil de p", "Factoría web, fase 1."), None).unwrap();
    assert!(a.creada);
    assert_eq!(memoria::clave_de(&a.memoria), Some("kumi-perfil"));
    let mut n = nueva("/p", "Perfil de p", "Factoría web, fase 2.");
    n.tags = vec!["kumi".into()];
    n.paths = vec!["src/main.rs".into()];
    let b = memoria::guardar_por_clave(&mut db, "kumi-perfil", n, None).unwrap();
    assert!(!b.creada);
    assert_eq!(b.memoria.id, a.memoria.id);
    assert_eq!(b.memoria.content, "Factoría web, fase 2.");
    assert!(b.memoria.tags.contains(&"kumi".to_string()) && b.memoria.tags.contains(&"topic:kumi-perfil".to_string()));
    assert_eq!(con_clave(&db, "kumi-perfil"), 1);
    // La versión anterior queda en el historial, con la clave como motivo.
    let h = memoria::historial(&db, &a.memoria.id).unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].content, "Factoría web, fase 1.");
    assert_eq!(h[0].reason.as_deref(), Some("key kumi-perfil"));
    // La misma clave en otro proyecto es otra memoria.
    let c = memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("/q", "Perfil de q", "Factoría android."), None).unwrap();
    assert!(c.creada && c.memoria.id != a.memoria.id);
    // `get --key` desde cada proyecto da la suya; la global solo si el proyecto no tiene.
    memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("", "Perfil por defecto", "Sin factoría."), None).unwrap();
    let ve = |p: &str| memoria::por_clave(&db, &Ambito::de(&db, p).unwrap(), "KUMI-PERFIL").unwrap().map(|m| m.title);
    assert_eq!(ve("/p").as_deref(), Some("Perfil de p"));
    assert_eq!(ve("/q").as_deref(), Some("Perfil de q"));
    assert_eq!(ve("/r").as_deref(), Some("Perfil por defecto"));
    assert!(memoria::por_clave(&db, &Ambito::de(&db, "/p").unwrap(), "otra").unwrap().is_none());
}

#[test]
fn avisa_de_una_clave_parecida_y_rechaza_las_raras() {
    let (_dir, mut db) = base("parecida");
    memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("/p", "Perfil", "uno"), None).unwrap();
    let r = memoria::guardar_por_clave(&mut db, "kumi_perfil", nueva("/p", "Perfil", "dos"), None).unwrap();
    assert!(r.creada);
    assert_eq!(r.parecidas, vec!["kumi-perfil".to_string()]);
    let r = memoria::guardar_por_clave(&mut db, "kumi-perfl", nueva("/p", "Perfil", "tres"), None).unwrap();
    assert!(r.parecidas.contains(&"kumi-perfil".to_string()), "{:?}", r.parecidas);
    // Distintas de verdad: nada.
    let r = memoria::guardar_por_clave(&mut db, "deploy-recipe", nueva("/p", "Receta", "cuatro"), None).unwrap();
    assert!(r.parecidas.is_empty(), "{:?}", r.parecidas);
    // En otro proyecto no cuenta.
    let r = memoria::guardar_por_clave(&mut db, "kumi-perfi", nueva("/q", "Perfil", "cinco"), None).unwrap();
    assert!(r.parecidas.is_empty());
    for mala in ["", "  ", "dos palabras", "¿qué?"] {
        assert!(memoria::guardar_por_clave(&mut db, mala, nueva("/p", "x", "y"), None).is_err(), "{mala:?}");
    }
}

/// Varios procesos `omoi` a la vez con la misma clave en el mismo repositorio, por la CLI y por
/// `omoi tool remember` (el plugin): una sola memoria al final, con el contenido de alguno de ellos.
#[test]
fn varios_procesos_a_la_vez_dejan_una_sola() {
    let dir = comun::Temporal::nueva("clave-procesos");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let casa = dir.join("casa");
    std::fs::create_dir_all(&casa).unwrap();
    let db_ruta = dir.join("o.db");
    assert!(!db_ruta.to_string_lossy().contains(".omoikane"));
    let lanzar = |i: usize, repo: &Path| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.current_dir(repo)
            .env("OMOIKANE_DB", &db_ruta)
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SEMANTIC", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("USERPROFILE", &casa)
            .env("HOME", &casa)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if i.is_multiple_of(2) {
            c.args(["remember", "-k", "fact", "-t", "Perfil del proyecto", "--key", "kumi-perfil", &format!("Factoría {i}.")]).stdin(Stdio::null());
            c.spawn().unwrap()
        } else {
            c.args(["tool", "remember"]).stdin(Stdio::piped());
            let mut h = c.spawn().unwrap();
            let p = serde_json::json!({ "title": "Perfil del proyecto", "content": format!("Factoría {i}."), "kind": "fact", "key": "kumi-perfil" });
            use std::io::Write;
            h.stdin.take().unwrap().write_all(p.to_string().as_bytes()).unwrap();
            h
        }
    };
    for ronda in 0..3 {
        let hijos: Vec<_> = (0..8).map(|i| lanzar(ronda * 8 + i, &repo)).collect();
        for h in hijos {
            let o = h.wait_with_output().unwrap();
            assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        }
    }
    let db = db::abrir(&db_ruta).unwrap();
    assert_eq!(con_clave(&db, "kumi-perfil"), 1, "una sola memoria con la clave");
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0)).unwrap();
    assert_eq!(total, 1);
    let versiones: i64 = db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get(0)).unwrap();
    assert_eq!(versiones, 23, "24 escrituras: una crea y 23 actualizan");
    // Y se lee por la clave.
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .current_dir(&repo)
        .args(["get", "--key", "kumi-perfil"])
        .env("OMOIKANE_DB", &db_ruta)
        .env("OMOIKANE_HOME", dir.join("home-omoikane"))
        .env("OMOIKANE_MODELS", dir.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_SYNC_AUTO", "0")
        .env("OMOIKANE_LANG", "en")
        .env("USERPROFILE", &casa)
        .env("HOME", &casa)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && s.contains("Perfil del proyecto") && s.contains("Factoría"), "{s}{}", String::from_utf8_lossy(&o.stderr));
    // `--data` y `get --key --resolve --json`, por la CLI: global con oficio y del proyecto.
    let correr = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
            .current_dir(&repo)
            .args(args)
            .env("OMOIKANE_DB", &db_ruta)
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_LANG", "en")
            .env("USERPROFILE", &casa)
            .env("HOME", &casa)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    correr(&["remember", "-k", "fact", "-t", "Ruta web", "--global", "--tag", "trade:web", "--key", "kumi-ruta", "--data", r#"{"ruta":["web"]}"#]);
    correr(&["remember", "-k", "fact", "-t", "Ruta de este repo", "--key", "kumi-ruta", "--data", r#"{"ruta":["web","seo-geo"],"fase":"alpha"}"#, "Sin cyber."]);
    let capas: serde_json::Value = serde_json::from_str(&correr(&["--json", "get", "--key", "kumi-ruta", "--resolve"])).unwrap();
    let capas = capas.as_array().unwrap();
    assert_eq!(capas.iter().map(|c| c["scope"].as_str().unwrap()).collect::<Vec<_>>(), ["project", "trade:web"]);
    assert_eq!(capas[0]["data"], serde_json::json!({"fase": "alpha", "ruta": ["web", "seo-geo"]}));
    assert_eq!(capas[0]["text"], "Sin cyber.");
    assert_eq!(capas[1]["data"], serde_json::json!({"ruta": ["web"]}));
}

/// La ruta de un proyecto por la megafactoría de Kumi: oficio (la ruta por defecto de la factoría
/// web), cliente (tras web, siempre seo-geo) y proyecto (sin SEO). `--resolve` las da todas, la más
/// concreta primero, cada una con su ámbito y sus datos, sin juntarlas; otro cliente no ve la del
/// cliente X; y regenerar lo mismo no crea versiones.
#[test]
fn la_ruta_de_kumi_en_tres_niveles() {
    use omoikane::proyecto;
    let (_dir, mut db) = base("niveles");
    let (disney, otro_x, de_y) = ("/clientes/x/disney-countdown", "/clientes/x/tienda", "/clientes/y/web");
    let x = proyecto::crear_espacio(&db, "Cliente X").unwrap();
    let y = proyecto::crear_espacio(&db, "Cliente Y").unwrap();
    for (p, s) in [(disney, x), (otro_x, x), (de_y, y)] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(s)).unwrap();
    }
    let datos = |v: serde_json::Value| memoria::con_datos("", &v).unwrap();
    let guardar = |db: &mut Connection, mut n: Nueva, tags: &[&str]| {
        n.tags = tags.iter().map(|t| t.to_string()).collect();
        memoria::guardar_por_clave(db, "kumi-ruta", n, None).unwrap()
    };
    // Oficio: una global con `trade:web` (lo que guarda `--trade web`).
    let oficio = Nueva { kind: Some(Tipo::Fact), title: "Ruta de la factoría web".into(), content: datos(serde_json::json!({"ruta": ["web"], "tests": ["web", "cyber"]})), ..Default::default() };
    guardar(&mut db, oficio.clone(), &["trade:web"]);
    // Cliente X (su espacio) y cliente Y.
    let cliente = |s, ruta: &str| Nueva { space_id: Some(s), kind: Some(Tipo::Fact), title: format!("Ruta del cliente {ruta}"), content: datos(serde_json::json!({"ruta": ["web", ruta]})), ..Default::default() };
    guardar(&mut db, cliente(x, "seo-geo"), &[]);
    guardar(&mut db, cliente(y, "cyber"), &[]);
    // Proyecto: disney-countdown sin SEO. Y una global sin oficio.
    let proyecto_n = Nueva { project: disney.into(), kind: Some(Tipo::Fact), title: "Ruta de disney-countdown".into(), content: memoria::con_datos("Sin SEO: es una cuenta atrás familiar.", &serde_json::json!({"ruta": ["web"], "fase": "alpha"})).unwrap(), ..Default::default() };
    guardar(&mut db, proyecto_n, &[]);
    guardar(&mut db, Nueva { kind: Some(Tipo::Fact), title: "Ruta por defecto".into(), content: datos(serde_json::json!({"ruta": []})), ..Default::default() }, &[]);

    let capas = |p: &str| memoria::capas_de_clave(&db, &Ambito::de(&db, p).unwrap(), "kumi-ruta").unwrap();
    let ambitos = |p: &str| capas(p).into_iter().map(|c| c.scope).collect::<Vec<_>>();
    assert_eq!(ambitos(disney), ["project", "space", "trade:web", "global"]);
    let c = capas(disney);
    assert_eq!(c[0].text, "Sin SEO: es una cuenta atrás familiar.");
    assert_eq!(c[0].data, Some(serde_json::json!({"fase": "alpha", "ruta": ["web"]})));
    assert_eq!(c[1].data, Some(serde_json::json!({"ruta": ["web", "seo-geo"]})));
    assert_eq!(c[1].title, "Ruta del cliente seo-geo");
    // Otro proyecto del cliente X: sin la de disney, con la del cliente X.
    assert_eq!(ambitos(otro_x), ["space", "trade:web", "global"]);
    // Un proyecto del cliente Y nunca ve la del cliente X.
    let de_y_capas = capas(de_y);
    assert_eq!(de_y_capas.iter().map(|c| c.scope.as_str()).collect::<Vec<_>>(), ["space", "trade:web", "global"]);
    assert_eq!(de_y_capas[0].data, Some(serde_json::json!({"ruta": ["web", "cyber"]})));
    // `get --key` a secas: la más concreta.
    assert_eq!(memoria::por_clave(&db, &Ambito::de(&db, disney).unwrap(), "kumi-ruta").unwrap().unwrap().title, "Ruta de disney-countdown");

    // El oficio y la global sin oficio son ámbitos distintos: una por cada uno.
    assert_eq!(con_clave(&db, "kumi-ruta"), 5);
    // Regenerar el oficio con lo mismo (las claves en otro orden) no crea versión; con otro dato, sí.
    let antes: i64 = db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get(0)).unwrap();
    let mismo = Nueva { content: datos(serde_json::json!({"tests": ["web", "cyber"], "ruta": ["web"]})), ..oficio.clone() };
    let r = guardar(&mut db, mismo, &["trade:web"]);
    assert!(!r.creada);
    let despues: i64 = db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get(0)).unwrap();
    assert_eq!(despues, antes, "lo mismo no deja versión");
    let otro = Nueva { content: datos(serde_json::json!({"ruta": ["web"], "tests": ["web"]})), ..oficio };
    guardar(&mut db, otro, &["trade:web"]);
    let despues: i64 = db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get(0)).unwrap();
    assert_eq!(despues, antes + 1);
    assert_eq!(con_clave(&db, "kumi-ruta"), 5);
}

#[test]
fn los_datos_van_y_vuelven() {
    let v = serde_json::json!({"b": 1, "a": {"d": [1, 2], "c": null}});
    let t = memoria::con_datos("Texto.\n", &v).unwrap();
    assert_eq!(t, "Texto.\n\n```json omoikane-data\n{\"a\":{\"c\":null,\"d\":[1,2]},\"b\":1}\n```");
    assert_eq!(memoria::datos_de(&t), ("Texto.".to_string(), Some(v.clone())));
    // Volver a poner datos cambia el bloque, no lo duplica.
    let t2 = memoria::con_datos(&t, &serde_json::json!([1])).unwrap();
    assert_eq!(memoria::datos_de(&t2), ("Texto.".to_string(), Some(serde_json::json!([1]))));
    // Un bloque `json` corriente no son datos.
    assert_eq!(memoria::datos_de("Mira:\n```json\n{}\n```").1, None);
}
