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
    let a = memoria::guardar_por_clave(&mut db, "Kumi-Perfil", nueva("/p", "Perfil de p", "Factoría web, fase 1."), None, None).unwrap();
    assert!(a.creada);
    assert_eq!(memoria::clave_de(&a.memoria), Some("kumi-perfil"));
    let mut n = nueva("/p", "Perfil de p", "Factoría web, fase 2.");
    n.tags = vec!["kumi".into()];
    n.paths = vec!["src/main.rs".into()];
    let b = memoria::guardar_por_clave(&mut db, "kumi-perfil", n, None, None).unwrap();
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
    let c = memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("/q", "Perfil de q", "Factoría android."), None, None).unwrap();
    assert!(c.creada && c.memoria.id != a.memoria.id);
    // `get --key` desde cada proyecto da la suya; la global solo si el proyecto no tiene.
    memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("", "Perfil por defecto", "Sin factoría."), None, None).unwrap();
    let ve = |p: &str| memoria::por_clave(&db, &Ambito::de(&db, p).unwrap(), "KUMI-PERFIL").unwrap().map(|m| m.title);
    assert_eq!(ve("/p").as_deref(), Some("Perfil de p"));
    assert_eq!(ve("/q").as_deref(), Some("Perfil de q"));
    assert_eq!(ve("/r").as_deref(), Some("Perfil por defecto"));
    assert!(memoria::por_clave(&db, &Ambito::de(&db, "/p").unwrap(), "otra").unwrap().is_none());
}

#[test]
fn avisa_de_una_clave_parecida_y_rechaza_las_raras() {
    let (_dir, mut db) = base("parecida");
    memoria::guardar_por_clave(&mut db, "kumi-perfil", nueva("/p", "Perfil", "uno"), None, None).unwrap();
    let r = memoria::guardar_por_clave(&mut db, "kumi_perfil", nueva("/p", "Perfil", "dos"), None, None).unwrap();
    assert!(r.creada);
    assert_eq!(r.parecidas, vec!["kumi-perfil".to_string()]);
    let r = memoria::guardar_por_clave(&mut db, "kumi-perfl", nueva("/p", "Perfil", "tres"), None, None).unwrap();
    assert!(r.parecidas.contains(&"kumi-perfil".to_string()), "{:?}", r.parecidas);
    // Distintas de verdad: nada.
    let r = memoria::guardar_por_clave(&mut db, "deploy-recipe", nueva("/p", "Receta", "cuatro"), None, None).unwrap();
    assert!(r.parecidas.is_empty(), "{:?}", r.parecidas);
    // En otro proyecto no cuenta.
    let r = memoria::guardar_por_clave(&mut db, "kumi-perfi", nueva("/q", "Perfil", "cinco"), None, None).unwrap();
    assert!(r.parecidas.is_empty());
    for mala in ["", "  ", "dos palabras", "¿qué?"] {
        assert!(memoria::guardar_por_clave(&mut db, mala, nueva("/p", "x", "y"), None, None).is_err(), "{mala:?}");
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
    correr(&["trades", "add", "web"]);
    correr(&["remember", "-k", "fact", "-t", "Ruta web", "--trade", "web", "--key", "kumi-ruta", "--data", r#"{"ruta":["web"]}"#]);
    correr(&["remember", "-k", "fact", "-t", "Ruta de este repo", "--key", "kumi-ruta", "--data", r#"{"ruta":["web","seo-geo"],"fase":"alpha"}"#, "Sin cyber."]);
    let capas: serde_json::Value = serde_json::from_str(&correr(&["--json", "get", "--key", "kumi-ruta", "--resolve"])).unwrap();
    let capas = capas.as_array().unwrap();
    assert_eq!(capas.iter().map(|c| c["scope"].as_str().unwrap()).collect::<Vec<_>>(), ["project", "trade:web"]);
    assert_eq!(capas[0]["data"], serde_json::json!({"fase": "alpha", "ruta": ["web", "seo-geo"]}));
    assert_eq!(capas[0]["text"], "Sin cyber.");
    assert_eq!(capas[1]["data"], serde_json::json!({"ruta": ["web"]}));
    // Los datos no van en el texto: ni en la memoria ni en la búsqueda.
    assert!(!capas[0]["text"].as_str().unwrap().contains("seo-geo"));
    let hallado = correr(&["--json", "search", "seo-geo"]);
    assert!(!hallado.contains("Ruta de este repo"), "{hallado}");
    // `get --key --json` a secas también da los datos.
    let una: serde_json::Value = serde_json::from_str(&correr(&["--json", "get", "--key", "kumi-ruta"])).unwrap();
    assert_eq!(una["data"]["fase"], "alpha");
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
    // Los tres son webs: la capa del oficio solo la ven los proyectos de ese oficio.
    for (p, s) in [(disney, x), (otro_x, x), (de_y, y)] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(s)).unwrap();
        omoikane::oficios::anadir(&db, id, &["web".to_string()]).unwrap();
    }
    let guardar = |db: &mut Connection, mut n: Nueva, tags: &[&str], datos: serde_json::Value| {
        n.tags = tags.iter().map(|t| t.to_string()).collect();
        memoria::guardar_por_clave(db, "kumi-ruta", n, Some(datos), None).unwrap()
    };
    // Oficio: una global con `trade:web` (lo que guarda `--trade web`).
    let oficio = Nueva { kind: Some(Tipo::Fact), title: "Ruta de la factoría web".into(), content: "La ruta por defecto de una web.".into(), ..Default::default() };
    guardar(&mut db, oficio.clone(), &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web", "cyber"]}));
    // Cliente X (su espacio) y cliente Y.
    let cliente = |s, ruta: &str| Nueva { space_id: Some(s), kind: Some(Tipo::Fact), title: format!("Ruta del cliente {ruta}"), content: "La ruta del cliente.".into(), ..Default::default() };
    guardar(&mut db, cliente(x, "seo-geo"), &[], serde_json::json!({"ruta": ["web", "seo-geo"]}));
    guardar(&mut db, cliente(y, "cyber"), &[], serde_json::json!({"ruta": ["web", "cyber"]}));
    // Proyecto: disney-countdown sin SEO. Y una global sin oficio.
    let proyecto_n = Nueva { project: disney.into(), kind: Some(Tipo::Fact), title: "Ruta de disney-countdown".into(), content: "Sin SEO: es una cuenta atrás familiar.".into(), ..Default::default() };
    guardar(&mut db, proyecto_n, &[], serde_json::json!({"ruta": ["web"], "fase": "alpha"}));
    guardar(&mut db, Nueva { kind: Some(Tipo::Fact), title: "Ruta por defecto".into(), content: "Sin factoría.".into(), ..Default::default() }, &[], serde_json::json!({"ruta": []}));

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
    // Regenerar el oficio con lo mismo (las claves en otro orden) no cambia nada; con otros datos
    // cambian los datos, sin versión (como `update --data`); con otro texto, versión.
    let versiones = |db: &Connection| db.query_row("SELECT count(*) FROM memory_version", [], |f| f.get::<_, i64>(0)).unwrap();
    let antes = versiones(&db);
    let r = guardar(&mut db, oficio.clone(), &["trade:web"], serde_json::json!({"tests": ["web", "cyber"], "ruta": ["web"]}));
    assert!(!r.creada);
    let sello = r.memoria.updated_at.clone();
    assert_eq!(versiones(&db), antes, "lo mismo no deja versión");
    let r = guardar(&mut db, oficio.clone(), &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    assert_eq!(versiones(&db), antes, "solo los datos no dejan versión");
    assert!(r.memoria.updated_at >= sello);
    assert_eq!(memoria::datos_de(&db, &[r.memoria.id.as_str()]).unwrap()[&r.memoria.id], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    let otro = Nueva { content: "Otra ruta por defecto.".into(), ..oficio.clone() };
    guardar(&mut db, otro, &["trade:web"], serde_json::json!({"ruta": ["web"], "tests": ["web"]}));
    assert_eq!(versiones(&db), antes + 1);
    assert_eq!(con_clave(&db, "kumi-ruta"), 5);
    // `trade-for:client` es otro nivel del mismo oficio: no pisa la de todos.
    let r = guardar(&mut db, oficio, &["trade:web", "trade-for:client"], serde_json::json!({"ruta": ["web", "seo-geo"]}));
    assert!(r.creada);
    assert_eq!(con_clave(&db, "kumi-ruta"), 6);
    let todas = memoria::capas_de_clave(&db, &Ambito::de(&db, disney).unwrap(), "kumi-ruta").unwrap();
    assert_eq!(todas.iter().map(|c| c.scope.as_str()).collect::<Vec<_>>(), ["project", "space", "trade:web", "trade:web", "global"]);
}

/// Los datos de una memoria con clave van en `memory.extra` (como `--data` en cualquier memoria),
/// nunca en su contenido; guardar la clave sin datos conserva los que tenía, y lo que parezca un
/// secreto se tapa (el de ejemplo lleva una `~` en el prefijo, como en tests/secretos.rs, para que
/// ningún escáner lo tome por uno de verdad).
#[test]
fn los_datos_van_aparte_del_texto() {
    let (_dir, mut db) = base("datos");
    let v = serde_json::json!({"b": 1, "a": {"d": [1, 2], "c": null}});
    let r = memoria::guardar_por_clave(&mut db, "perfil", nueva("/p", "Perfil", "Texto."), Some(v.clone()), None).unwrap();
    assert_eq!(r.memoria.content, "Texto.");
    assert_eq!(memoria::datos_de(&db, &[r.memoria.id.as_str()]).unwrap()[&r.memoria.id], v);
    let r = memoria::guardar_por_clave(&mut db, "perfil", nueva("/p", "Perfil", "Texto nuevo."), None, None).unwrap();
    assert_eq!(r.memoria.content, "Texto nuevo.");
    assert_eq!(memoria::datos_de(&db, &[r.memoria.id.as_str()]).unwrap()[&r.memoria.id], v);
    let r = memoria::guardar_por_clave(&mut db, "perfil", nueva("/p", "Perfil", "Texto nuevo."), Some(serde_json::json!({"token": "export GH=gh~p_abcdefghijklmnopqrstuvwxyz0123456789".replace('~', "")})), None).unwrap();
    assert!(!r.tapados.is_empty());
    let d = memoria::datos_de(&db, &[r.memoria.id.as_str()]).unwrap();
    assert!(!d[&r.memoria.id]["token"].as_str().unwrap().contains("abcdefghijklmnop"));
}

/// 1.8.6: la misma clave en dos bases; al importar una en la otra, la que pierde queda sustituida por la
/// que manda. Si la persona dice «está bien así» (`compatible`), importar otra vez no vuelve a ponerla
/// sustituida: antes solo se saltaba la pareja si su veredicto vigente era `superseded`.
#[test]
fn importar_otra_vez_respeta_esta_bien_asi() {
    let dir = comun::Temporal::nueva("clave-descartada");
    let omoi = |base: &str, args: &[&str]| -> String {
        let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
            .args(args)
            .current_dir(&*dir)
            .env("OMOIKANE_DB", dir.join(base))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    let id_de = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap()["id"].as_str().unwrap().to_string();
    let vieja = id_de(omoi("a.db", &["--json", "remember", "-k", "fact", "-t", "Ruta de la factoría", "--global", "--key", "ruta", "android, test"]));
    let nueva = id_de(omoi("b.db", &["--json", "remember", "-k", "fact", "-t", "Ruta de la factoría", "--global", "--key", "ruta", "android, test, despliegue"]));
    let export = dir.join("a.jsonl");
    std::fs::write(&export, omoi("a.db", &["export", "--all"])).unwrap();
    let export = export.to_str().unwrap();
    let sustituida = || {
        let rev: serde_json::Value = serde_json::from_str(&omoi("b.db", &["--json", "review"])).unwrap();
        rev.as_array().unwrap().iter().any(|a| a["kind"] == "superseded" && a["memory_id"] == vieja.as_str())
    };
    omoi("b.db", &["import", export]);
    assert!(sustituida(), "la que pierde queda sustituida al importar");
    omoi("b.db", &["review", "--dismiss", &vieja, "--kind", "superseded", "--other", &nueva]);
    assert!(!sustituida());
    omoi("b.db", &["import", export]);
    assert!(!sustituida(), "importar otra vez no vuelve a ponerla sustituida");
}
