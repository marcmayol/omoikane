//! 1.8: lo que pide Kumi (una central de agentes que usa Omoikane como cerebro): la cola de decisiones
//! con prioridad (`omoi decisions`, la herramienta `decisions`) y el historial por fichero para su
//! puerta de riesgo (`omoi file-history`).

mod comun;

use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::{db, diario, proyecto, riesgo};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    proyecto::de(dir).unwrap()
}

fn decision(db: &mut Connection, project: &str, space_id: Option<i64>, title: &str, tags: &[&str]) -> String {
    let n = Nueva {
        project: project.into(),
        space_id,
        kind: Some(Tipo::Decision),
        title: title.into(),
        content: format!("{title}. Opciones distintas para «{title}»; asumido lo primero mientras tanto."),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    let id = memoria::guardar(db, n, None).unwrap().id;
    // Fechas distintas: el orden por fecha se ve.
    std::thread::sleep(std::time::Duration::from_millis(5));
    id
}

/// `omoi` con su propia base y sin tocar la de verdad; desde `cwd`.
fn omoi(base: &Path, cwd: &Path, args: &[&str], entrada: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(args)
        .current_dir(cwd)
        .env("OMOIKANE_DB", base.join("o.db"))
        .env("OMOIKANE_MODELS", base.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_HOME", base.join("home-omoikane"))
        .env_remove("OMOIKANE_LANG")
        .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut hijo = c.spawn().unwrap();
    if let Some(e) = entrada {
        use std::io::Write;
        hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
    }
    hijo.wait_with_output().unwrap()
}

fn json(o: Output) -> serde_json::Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn titulos(v: &serde_json::Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|d| d["title"].as_str().unwrap().to_string()).collect()
}

/// Dos clientes (Acme y Globex) con decisiones abiertas bloqueantes y para después.
struct Fabrica {
    dir: comun::Temporal,
    acme: String,
    globex: String,
}

fn fabrica(nombre: &str) -> Fabrica {
    let dir = comun::Temporal::nueva(&format!("kumi-{nombre}"));
    let acme = repo(&dir.join("acme-web"));
    let globex = repo(&dir.join("globex-web"));
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let ea = proyecto::crear_espacio(&db, "Acme").unwrap();
    let eg = proyecto::crear_espacio(&db, "Globex").unwrap();
    for (p, s) in [(&acme, ea), (&globex, eg)] {
        let id = proyecto::registrar(&db, p).unwrap();
        proyecto::asignar_espacio(&db, id, Some(s)).unwrap();
    }
    decision(&mut db, &acme, None, "Acme: precio del plan Pro", &["open"]);
    decision(&mut db, &acme, None, "Acme: borrar cuentas inactivas", &["open", "blocking"]);
    decision(&mut db, &acme, None, "Acme: color del botón", &["open"]);
    decision(&mut db, "", Some(ea), "Acme espacio: proveedor de correo", &["open", "Blocking"]);
    decision(&mut db, &acme, None, "Acme: ya decidida", &["blocking"]);
    decision(&mut db, &globex, None, "Globex: migrar la base", &["open", "blocking"]);
    decision(&mut db, "", None, "Global: licencia de las plantillas", &["open"]);
    Fabrica { dir, acme, globex }
}

#[test]
fn las_bloqueantes_primero_y_sin_salir_del_espacio() {
    let f = fabrica("orden");
    let db = db::abrir(&f.dir.join("o.db")).unwrap();
    let desde = Ambito::de(&db, &f.acme).unwrap();
    let v = memoria::decisiones(&db, &desde, false, &[], 50).unwrap();
    let t: Vec<(&str, &str)> = v.iter().map(|d| (d.title.as_str(), d.priority)).collect();
    // Bloqueantes primero (de la más nueva a la más vieja), luego las de después; sin Globex ni la
    // que ya no está abierta.
    assert_eq!(t, [
        ("Acme espacio: proveedor de correo", "blocking"),
        ("Acme: borrar cuentas inactivas", "blocking"),
        ("Global: licencia de las plantillas", "later"),
        ("Acme: color del botón", "later"),
        ("Acme: precio del plan Pro", "later"),
    ]);
    assert!(v[0].project.is_empty() && v[0].space == "acme" && !v[0].global);
    assert!(v[2].global);
    let solo = memoria::decisiones(&db, &desde, true, &[], 50).unwrap();
    assert_eq!(solo.len(), 2);
    assert_eq!(memoria::decisiones(&db, &desde, false, &[], 1).unwrap().len(), 1);
    // Desde la carpeta general, las de todos.
    assert_eq!(memoria::decisiones(&db, &Ambito::Todos, true, &[], 50).unwrap().len(), 3);
}

#[test]
fn al_empezar_las_bloqueantes_marcadas() {
    let f = fabrica("empezar");
    let mut db = db::abrir(&f.dir.join("o.db")).unwrap();
    let e = Entrada { session_id: "s1".into(), ..Default::default() };
    let t = recall::al_empezar(&mut db, &f.acme, &e).unwrap().unwrap();
    let (_, resto) = t.split_once("Decisions waiting for the user").expect("la sección");
    assert!(resto.lines().next().unwrap().contains("the 2 marked BLOCKING first"), "{t}");
    let l: Vec<&str> = resto.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(l.len(), 5, "{t}");
    assert!(l[0].starts_with("- BLOCKING ") && l[0].contains("proveedor de correo"), "{t}");
    assert!(l[1].starts_with("- BLOCKING ") && l[1].contains("borrar cuentas"), "{t}");
    assert!(l[2..].iter().all(|x| !x.contains("BLOCKING")), "{t}");
    assert!(!t.contains("Globex"), "{t}");
    // Sin bloqueantes, la cabecera de siempre.
    let mut db2 = db::abrir(&f.dir.join("o.db")).unwrap();
    let t = recall::al_empezar(&mut db2, &f.globex, &e).unwrap().unwrap();
    assert!(t.contains("BLOCKING") && t.contains("Globex: migrar la base"), "{t}");
    // Las instrucciones dicen cuándo es bloqueante.
    assert!(t.contains("Add the tag `blocking` too only when the work can't go on without the answer"), "{t}");
}

#[test]
fn la_cli_y_la_herramienta_dan_el_mismo_json() {
    let f = fabrica("cli");
    let v = json(omoi(&f.dir, Path::new(&f.acme), &["decisions", "--json"], None));
    assert_eq!(v.as_array().unwrap().len(), 5, "{v}");
    let d = &v[1];
    assert_eq!(d["priority"], "blocking");
    assert_eq!(d["title"], "Acme: borrar cuentas inactivas");
    assert!(d["content"].as_str().unwrap().contains("asumido"));
    assert!(d["id"].as_str().unwrap().len() > 12 && d["created_at"].is_string() && d["updated_at"].is_string());
    assert!(d["project"].as_str().unwrap().ends_with("acme-web"), "{d}");
    let b = json(omoi(&f.dir, Path::new(&f.acme), &["decisions", "--blocking", "--json"], None));
    assert_eq!(titulos(&b), ["Acme espacio: proveedor de correo", "Acme: borrar cuentas inactivas"]);
    // Otro proyecto del mismo cliente, sí; de otro cliente, no.
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions", "--space", "globex", "--json"], None);
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("migrar"));
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions", "--project", "globex-web", "--json"], None);
    assert!(!o.status.success() && !String::from_utf8_lossy(&o.stdout).contains("migrar"));
    // Desde la carpeta general (la central de Kumi, en la carpeta de la factoría) y pidiendo un proyecto.
    let central = f.dir.join("central");
    std::fs::create_dir_all(&central).unwrap();
    let g = json(omoi(&f.dir, &central, &["--dir", &f.globex, "decisions", "--blocking", "--json"], None));
    assert_eq!(titulos(&g), ["Globex: migrar la base"]);
    // Texto: marcadas.
    let o = omoi(&f.dir, Path::new(&f.acme), &["decisions"], None);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.lines().next().unwrap().contains("BLOCKING"), "{t}");

    // La herramienta MCP: lo mismo, y tampoco se sale del espacio.
    let llamar = |args: serde_json::Value| omoikane::mcp::herramienta(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("decisions"), &args.to_string(), "test").unwrap();
    let (t, error) = llamar(serde_json::json!({ "blocking": true }));
    assert!(!error, "{t}");
    let m: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(titulos(&m), titulos(&b));
    let (t, error) = llamar(serde_json::json!({ "space": "globex" }));
    assert!(error && !t.contains("migrar"), "{t}");
    let (t, _) = llamar(serde_json::json!({ "all_projects": true }));
    assert!(!t.contains("Globex"), "{t}");
}

// --- file-history ---------------------------------------------------------------------------------

fn sesion(db: &Connection, nombre: &str, project_id: i64, carpeta: &str, dia: &str, ficheros: &[&str]) {
    diario::crear_tabla(db).unwrap();
    db.execute(
        "INSERT INTO diary(session, project_id, folder, first_at, last_at, files) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        rusqlite::params![nombre, project_id, carpeta, format!("{dia}T10:00:00.000Z"), serde_json::to_string(ficheros).unwrap()],
    )
    .unwrap();
}

fn gotcha(db: &mut Connection, project: &str, titulo: &str, rutas: &[&str]) -> String {
    let n = Nueva {
        project: project.into(),
        kind: Some(Tipo::Gotcha),
        title: titulo.into(),
        content: format!("{titulo}: cuidado al tocarlo."),
        paths: rutas.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

/// Un repositorio con 5 sesiones en `src/sync.rs` (4 con `tests/sync.rs`, 2 con `CHANGELOG.md`), y
/// otro proyecto que también toca `src/sync.rs`; y una sesión desde la carpeta del usuario con rutas
/// absolutas al repositorio.
fn con_diario(nombre: &str) -> (comun::Temporal, String, Vec<String>) {
    let d = comun::Temporal::nueva(&format!("kumi-{nombre}"));
    let r = repo(&d.join("repo"));
    let otro = repo(&d.join("otro"));
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    let id = proyecto::registrar(&db, &r).unwrap();
    let id2 = proyecto::registrar(&db, &otro).unwrap();
    sesion(&db, "a", id, &r, "2026-10-01", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    sesion(&db, "b", id, &r, "2026-10-08", &["src/sync.rs", "tests/sync.rs"]);
    sesion(&db, "c", id, &r, "2026-10-15", &["src/sync.rs", "tests/sync.rs", "CHANGELOG.md"]);
    sesion(&db, "d", id, &r, "2026-11-04", &["src/sync.rs", "tests/sync.rs"]);
    sesion(&db, "e", id, &r, "2026-10-20", &["src/sync.rs"]);
    sesion(&db, "f", id, &r, "2026-10-21", &["README.md"]);
    for i in 0..3 {
        sesion(&db, &format!("o{i}"), id2, &otro, "2026-11-06", &["src/sync.rs"]);
    }
    // Una sesión abierta en la carpeta del usuario que tocó el repositorio con rutas absolutas (y algo de
    // fuera, que no cuenta), y otra que no lo tocó.
    let abs = |f: &str| format!("{}/{f}", r.replace('\\', "/"));
    let casa = d.join("casa").to_string_lossy().into_owned();
    sesion(&db, "h1", 0, &casa, "2026-11-06", &[&abs("src/sync.rs"), &abs("tests/sync.rs"), "C:/fuera/x.rs"]);
    sesion(&db, "h2", 0, &casa, "2026-11-07", &["C:/fuera/x.rs", &format!("{}-wt/src/sync.rs", r.replace('\\', "/"))]);
    let g1 = gotcha(&mut db, &r, "El push de sync pierde el orden", &["src/sync.rs"]);
    let g2 = gotcha(&mut db, &r, "Sync con la ruta entera", &[&format!("{r}/src/sync.rs")]);
    gotcha(&mut db, &otro, "Gotcha del otro proyecto", &["src/sync.rs"]);
    (d, r, vec![g1, g2])
}

#[test]
fn historia_de_los_ficheros_del_diff() {
    let (d, r, gotchas) = con_diario("historia");
    let db = db::abrir(&d.join("o.db")).unwrap();
    let pedidos: Vec<String> =
        ["src/sync.rs", "./src/sync.rs", &format!("{r}/src/sync.rs"), r"tests\sync.rs", "nuevo.rs", ""].iter().map(|s| s.to_string()).collect();
    let i = riesgo::de_ficheros(&db, &r, &pedidos).unwrap();
    assert_eq!(i.sessions, 7);
    // Sin repetir: relativa, con `./`, absoluta y con `\` son el mismo fichero.
    let nombres: Vec<&str> = i.files.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(nombres, ["src/sync.rs", "tests/sync.rs", "nuevo.rs"]);
    let s = &i.files[0];
    // Solo las sesiones de este proyecto, no las del otro.
    assert_eq!((s.sessions, s.last.as_deref()), (6, Some("2026-11-06")));
    assert_eq!(s.changes_with, [riesgo::Junto { file: "tests/sync.rs".into(), sessions: 5 }, riesgo::Junto { file: "CHANGELOG.md".into(), sessions: 2 }]);
    let mut esperados = gotchas.clone();
    esperados.sort();
    assert_eq!((s.gotchas, &s.gotcha_ids), (2, &esperados));
    let t = &i.files[1];
    assert_eq!((t.sessions, t.changes_with[0].file.as_str(), t.gotchas), (5, "src/sync.rs", 0));
    let n = &i.files[2];
    assert_eq!((n.sessions, n.last.as_deref(), n.changes_with.len(), n.gotchas), (0, None, 0, 0));

    // Sin diario: todo a cero, sin error.
    let d2 = comun::Temporal::nueva("kumi-sin-diario");
    let r2 = repo(&d2.join("repo"));
    let db2 = db::abrir(&d2.join("o.db")).unwrap();
    let i = riesgo::de_ficheros(&db2, &r2, &["src/a.rs".to_string()]).unwrap();
    assert_eq!((i.sessions, i.files[0].sessions), (0, 0));
}

#[test]
fn file_history_por_argumentos_y_por_la_entrada() {
    let (d, r, _) = con_diario("cli-historia");
    let raiz = Path::new(&r);
    let v = json(omoi(&d, raiz, &["file-history", "src/sync.rs", "README.md", "--json"], None));
    assert_eq!(v["sessions"], 7);
    assert_eq!(v["files"][0]["file"], "src/sync.rs");
    assert_eq!(v["files"][0]["sessions"], 6);
    assert_eq!(v["files"][0]["last"], "2026-11-06");
    assert_eq!(v["files"][0]["changes_with"][0]["file"], "tests/sync.rs");
    assert_eq!(v["files"][0]["changes_with"][0]["sessions"], 5);
    assert_eq!(v["files"][0]["gotchas"], 2);
    assert_eq!(v["files"][0]["gotcha_ids"].as_array().unwrap().len(), 2);
    assert_eq!(v["files"][1]["sessions"], 1);
    // Lo que sale de `git diff --name-only`, por la entrada, desde otra carpeta con --dir.
    let fuera = d.join("fuera");
    std::fs::create_dir_all(&fuera).unwrap();
    let v = json(omoi(&d, &fuera, &["--dir", &r, "file-history", "--json"], Some("src/sync.rs\ntests/sync.rs\n\n")));
    let nombres: Vec<&str> = v["files"].as_array().unwrap().iter().map(|f| f["file"].as_str().unwrap()).collect();
    assert_eq!(nombres, ["src/sync.rs", "tests/sync.rs"]);
    // Absoluta.
    let abs = raiz.join("src").join("sync.rs");
    let v = json(omoi(&d, raiz, &["file-history", abs.to_str().unwrap(), "--json"], None));
    assert_eq!((v["files"][0]["file"].as_str(), v["files"][0]["sessions"].as_u64()), (Some("src/sync.rs"), Some(6)));
    // Texto.
    let o = omoi(&d, raiz, &["file-history", "src/sync.rs", "nuevo.rs"], None);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("src/sync.rs: changed in 6 of 7 sessions (last 2026-11-06); with tests/sync.rs (5), CHANGELOG.md (2); 2 gotcha(s)"), "{t}");
    assert!(t.contains("nuevo.rs: not in the session diary"), "{t}");
    // Un diff vacío: ningún fichero, sin error.
    let v = json(omoi(&d, raiz, &["file-history", "--json"], Some("")));
    assert_eq!(v["files"].as_array().map(Vec::len), Some(0));
}

// --- Lo que pide kumi.py: list por etiqueta, datos y el estado de las decisiones -----------------

/// Un proyecto con muchas memorias que se parecen al perfil: `search` lo podía dejar fuera; `list`
/// por etiqueta lo da siempre, con sus datos, y sin salir del espacio.
#[test]
fn list_por_etiqueta_con_datos() {
    let f = fabrica("list");
    let raiz = Path::new(&f.acme);
    let perfil = serde_json::json!({ "ruta": ["web", "seo-geo"], "prod_tests": true, "fase": null });
    let o = omoi(&f.dir, raiz, &["remember", "--json", "-k", "decision", "-t", "Kumi: perfil del proyecto", "--tag", "kumi", "--tag", "Kumi-Perfil", "--data", &perfil.to_string(), "--force", "Decisiones de Marc para este proyecto."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    for i in 0..30 {
        let t = format!("Perfil del proyecto, nota {i}");
        json(omoi(&f.dir, raiz, &["remember", "--json", "-k", "fact", "-t", &t, "--tag", "kumi", "--force", &format!("{t}: perfil del proyecto y su fase {i}.")], None));
    }
    let v = json(omoi(&f.dir, raiz, &["list", "--tag", "kumi-perfil", "--json"], None));
    assert_eq!(v.as_array().unwrap().len(), 1, "{v}");
    assert_eq!((v[0]["id"].as_str(), &v[0]["data"]), (Some(id.as_str()), &perfil));
    assert_eq!(json(omoi(&f.dir, raiz, &["list", "--tag", "kumi", "--json"], None)).as_array().unwrap().len(), 31);
    // Dos etiquetas: las dos; con tipo, solo ese.
    assert_eq!(json(omoi(&f.dir, raiz, &["list", "--tag", "kumi", "--tag", "kumi-perfil", "--json"], None)).as_array().unwrap().len(), 1);
    assert_eq!(json(omoi(&f.dir, raiz, &["list", "--tag", "kumi", "-k", "fact", "-n", "5", "--json"], None)).as_array().unwrap().len(), 5);
    // Las bloqueantes de Acme por etiqueta, también la del espacio; nunca la de Globex.
    let b = json(omoi(&f.dir, raiz, &["list", "--tag", "open", "--tag", "blocking", "--json"], None));
    assert_eq!(titulos(&b), ["Acme espacio: proveedor de correo", "Acme: borrar cuentas inactivas"]);
    assert!(!omoi(&f.dir, raiz, &["list", "--tag", "open", "--space", "globex"], None).status.success());
    // `get` da los datos; la herramienta `recent` con `tags`, igual que `list`.
    let g = json(omoi(&f.dir, raiz, &["get", &id, "--json"], None));
    assert_eq!(g["data"], perfil);
    let (t, error) = omoikane::mcp::herramienta(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("recent"), r#"{"tags":["kumi-perfil"]}"#, "test").unwrap();
    assert!(!error, "{t}");
    let r: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!((r.as_array().unwrap().len(), &r[0]["data"]), (1, &perfil), "{r}");
    // Sin etiqueta, no.
    assert!(!omoi(&f.dir, raiz, &["list", "--json"], None).status.success());
}

/// Los datos no entran en la búsqueda ni en el recall, se cambian sin versión nueva y se quitan.
#[test]
fn los_datos_no_se_buscan_y_se_cambian_solos() {
    let f = fabrica("datos");
    let raiz = Path::new(&f.acme);
    let d = serde_json::json!({ "palabra": "xilofonorara", "clave": "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" });
    let o = omoi(&f.dir, raiz, &["remember", "--json", "-k", "fact", "-t", "Perfil con datos", "--tag", "perfil", "--data", &d.to_string(), "Algo normal."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    let s = json(omoi(&f.dir, raiz, &["search", "xilofonorara", "--json"], None));
    assert_eq!(s.as_array().unwrap().len(), 0, "{s}");
    let mut db = db::abrir(&f.dir.join("o.db")).unwrap();
    let e = Entrada { session_id: "s".into(), prompt: "qué pasa con xilofonorara".into(), ..Default::default() };
    let r = recall::al_preguntar(&mut db, &f.acme, &e, None).unwrap().unwrap_or_default();
    assert!(!r.contains("xilofonorara"), "{r}");
    // Lo que parece un secreto se tapa también en los datos.
    let datos = memoria::datos_de(&db, &[id.as_str()]).unwrap();
    assert!(datos[&id]["clave"].as_str().unwrap().starts_with("[redacted:"), "{datos:?}");
    drop(db);
    // Solo los datos: sin versión en el historial, y el sello cambia (la sync lo lleva).
    let hlc = |db: &Connection| db.query_row("SELECT hlc FROM memory WHERE id = ?1", [&id], |x| x.get::<_, String>(0)).unwrap();
    let antes = hlc(&db::abrir(&f.dir.join("o.db")).unwrap());
    let v = json(omoi(&f.dir, raiz, &["update", &id, "--data", r#"{"fase":"beta"}"#, "--json"], None));
    assert_eq!((v["data"]["fase"].as_str(), v["content"].as_str()), (Some("beta"), Some("Algo normal.")));
    assert!(json(omoi(&f.dir, raiz, &["history", &id, "--json"], None)).as_array().unwrap().is_empty());
    assert_ne!(hlc(&db::abrir(&f.dir.join("o.db")).unwrap()), antes);
    // Por MCP: `data: null` los quita; sin `data`, no se tocan.
    let llamar = |args: serde_json::Value| omoikane::mcp::herramienta(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("update"), &args.to_string(), "test").unwrap();
    llamar(serde_json::json!({ "id": id, "title": "Perfil con datos, retitulado" }));
    assert_eq!(json(omoi(&f.dir, raiz, &["get", &id, "--json"], None))["data"]["fase"], "beta");
    llamar(serde_json::json!({ "id": id, "data": null }));
    assert!(json(omoi(&f.dir, raiz, &["get", &id, "--json"], None)).get("data").is_none());
    // JSON que no lo es: error, y nada guardado.
    assert!(!omoi(&f.dir, raiz, &["remember", "-k", "fact", "-t", "Mal", "--data", "{no", "x"], None).status.success());
    // Viaja con `omoi export` / `omoi import` como un campo `data` de la línea.
    llamar(serde_json::json!({ "id": id, "data": { "fase": "alpha" } }));
    let o = omoi(&f.dir, raiz, &["export", "--all"], None);
    let texto = String::from_utf8_lossy(&o.stdout).to_string();
    let linea: serde_json::Value = texto.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).find(|l| l["id"] == id.as_str()).expect("la línea");
    assert_eq!(linea["data"]["fase"], "alpha", "{linea}");
    let otra = comun::Temporal::nueva("kumi-datos-import");
    let copia = otra.join("copia.jsonl");
    std::fs::write(&copia, &texto).unwrap();
    json(omoi(&otra, raiz, &["import", copia.to_str().unwrap(), "--json"], None));
    let db2 = db::abrir(&otra.join("o.db")).unwrap();
    assert_eq!(memoria::datos_de(&db2, &[id.as_str()]).unwrap()[&id]["fase"], "alpha");
}

/// El ciclo de una decisión de Kumi con el texto limpio: abierta-bloqueante de una tarea, la puerta
/// la ve, se resuelve con la respuesta y deja de salir.
#[test]
fn decision_bloqueante_de_una_tarea_y_resuelta() {
    let f = fabrica("resolver");
    let raiz = Path::new(&f.acme);
    let o = omoi(&f.dir, raiz, &["remember", "--json", "-k", "decision", "-t", "Kumi: tarea parada en la factoría web", "--tag", "kumi", "--tag", "tarea:t42", "--tag", "open", "--tag", "blocking", "--force", "La factoría no sabe qué dominio usar."], None);
    let id = json(o)["id"].as_str().unwrap().to_string();
    json(omoi(&f.dir, raiz, &["remember", "--json", "-k", "decision", "-t", "Kumi: otra tarea", "--tag", "tarea:t7", "--tag", "open", "--tag", "blocking", "--force", "Otra."], None));
    let b = json(omoi(&f.dir, raiz, &["decisions", "--blocking", "--tag", "tarea:t42", "--json"], None));
    assert_eq!(titulos(&b), ["Kumi: tarea parada en la factoría web"]);
    // Resolverla: respuesta al final, sin `open` ni `blocking`, con `resolved`.
    let r = json(omoi(&f.dir, raiz, &["decisions", "--resolve", &id, "--answer", "acme.com", "--json"], None));
    assert!(r["content"].as_str().unwrap().ends_with("acme.com") && r["content"].as_str().unwrap().contains("Answer ("), "{r}");
    let tags: Vec<&str> = r["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert!(tags.contains(&"resolved") && tags.contains(&"tarea:t42") && !tags.contains(&"open") && !tags.contains(&"blocking"), "{tags:?}");
    assert!(json(omoi(&f.dir, raiz, &["decisions", "--blocking", "--tag", "tarea:t42", "--json"], None)).as_array().unwrap().is_empty());
    assert_eq!(json(omoi(&f.dir, raiz, &["list", "--tag", "resolved", "--tag", "tarea:t42", "--json"], None)).as_array().unwrap().len(), 1);
    // Sin respuesta, no; una que no es decisión, no; la de otro cliente, tampoco.
    assert!(!omoi(&f.dir, raiz, &["decisions", "--resolve", &id], None).status.success());
    let db = db::abrir(&f.dir.join("o.db")).unwrap();
    let de_globex = memoria::decisiones(&db, &Ambito::Todos, true, &["".to_string()], 50).unwrap().into_iter().find(|d| d.title.starts_with("Globex")).unwrap().id;
    drop(db);
    let o = omoi(&f.dir, raiz, &["decisions", "--resolve", &de_globex, "--answer", "sí"], None);
    assert!(!o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    // Por MCP: lo mismo, y tampoco cruza de espacio.
    let llamar = |args: serde_json::Value| omoikane::mcp::herramienta(f.dir.join("o.db"), Some(PathBuf::from(&f.acme)), Some("decisions"), &args.to_string(), "test").unwrap();
    let (t, error) = llamar(serde_json::json!({ "resolve": de_globex, "answer": "sí" }));
    assert!(error, "{t}");
    let otra = json(omoi(&f.dir, raiz, &["decisions", "--blocking", "--tag", "tarea:t7", "--json"], None))[0]["id"].as_str().unwrap().to_string();
    let (t, error) = llamar(serde_json::json!({ "resolve": otra, "answer": "más tarde" }));
    assert!(!error && t.contains("\"resolved\": true"), "{t}");
}
