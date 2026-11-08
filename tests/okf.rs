//! 1.8.5: exportar e importar en OKF v0.2 (el Open Knowledge Format de Google, el de
//! okf-agent-memory). Ida y vuelta a una base vacía sin perder lo importante, importar dos veces sin
//! duplicar, un bundle de otro con lo que OKF tiene y Omoikane no, los espacios aparte, el espejo y los
//! errores de lo que no es un bundle.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Ambito, Memoria, Nueva, Tipo};
use omoikane::obsidian::Que;
use omoikane::okf::{self, Destino};
use omoikane::{proyecto, veredictos};
use rusqlite::Connection;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

fn guardar(db: &mut Connection, project: &str, kind: Tipo, title: &str, content: &str, tags: &[&str], paths: &[&str]) -> String {
    let n = Nueva {
        project: project.into(),
        kind: Some(kind),
        title: title.into(),
        content: content.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        paths: paths.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

fn todas(db: &Connection) -> Vec<Memoria> {
    let mut v = memoria::recientes(db, &Ambito::Todos, None, 10_000).unwrap();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

fn leer(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Todos los `.md` de una carpeta, con su ruta relativa.
fn ficheros(dir: &Path) -> BTreeSet<String> {
    let mut r = BTreeSet::new();
    let mut pila = vec![dir.to_path_buf()];
    while let Some(d) = pila.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.path().is_dir() {
                pila.push(e.path());
            } else {
                r.insert(e.path().strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    r
}

#[test]
fn ida_y_vuelta_a_una_base_vacia() {
    let dir = comun::Temporal::nueva("okf-ida");
    let mut a = db::abrir(&dir.join("a.db")).unwrap();
    let vieja = guardar(&mut a, "/p", Tipo::Decision, "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[], &["src/auth.rs"]);
    let nueva = guardar(&mut a, "/p", Tipo::Decision, "La autenticación va con JWT", "Se pasó a JWT. Ver `src/auth.rs`.", &["verified", "auth"], &["src/auth.rs", "/etc/hosts"]);
    veredictos::poner(&a, &vieja, &nueva, veredictos::SUSTITUIDA, "", "").unwrap();
    let trampa = guardar(&mut a, "/p", Tipo::Gotcha, "Cuidado: \"comillas\" y #almohadilla", "Línea 1.\n\n## Detalle\n\n```\ncódigo: [1, 2]\n```", &["review:2026-12-01", "draft"], &[]);
    let clave = memoria::guardar_por_clave(&mut a, "kumi-perfil", Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: "Perfil".into(), content: "Datos del perfil.".into(), ..Default::default() }, Some(json!({ "edad": 3 })), None).unwrap().memoria.id;
    let global = guardar(&mut a, "", Tipo::Preference, "Commits en inglés", "Siempre.", &[], &[]);
    memoria::actualizar(&mut a, &global, memoria::Cambios { pinned: Some(true), ..Default::default() }, None).unwrap();
    let receta = guardar(&mut a, "/p", Tipo::Procedure, "Publicar", "1. Compilar.\n2. Subir.", &["index"], &[]);

    let bundle = dir.join("knowledge");
    let n = okf::exportar(&a, &bundle, Que::Personal).unwrap();
    assert_eq!((n.memories, n.created, n.hubs), (6, 6, 2), "{n:?}");
    let raiz = leer(&bundle.join("index.md"));
    assert!(raiz.starts_with("---\nokf_version: \"0.2\"\n---\n"), "{raiz}");
    assert!(leer(&bundle.join("log.md")).contains("\n## 20"), "registro por días");
    let fs = ficheros(&bundle);
    assert!(fs.contains("p/decision/la-autenticación-va-con-jwt.md") && fs.contains("_global/preference/commits-en-inglés.md"), "{fs:?}");
    assert!(fs.contains("p/overview.md") && fs.contains("p/decision/index.md"), "{fs:?}");
    let jwt = leer(&bundle.join("p/decision/la-autenticación-va-con-jwt.md"));
    assert!(jwt.starts_with("---\ntype: Decision\n"), "{jwt}");
    assert!(jwt.contains("code_refs: [src/auth.rs]") && jwt.contains("paths: [\"/etc/hosts\"]"), "las absolutas, fuera de code_refs: {jwt}");
    assert!(jwt.contains("{ by: human:user, at: "), "{jwt}");
    let cookie = leer(&bundle.join("p/decision/las-sesiones-van-por-cookie.md"));
    assert!(cookie.contains("status: deprecated") && cookie.contains("[La autenticación va con JWT](/p/decision/la-autenticación-va-con-jwt.md)"), "{cookie}");
    let t = leer(&bundle.join("p/gotcha/cuidado-comillas-y-almohadilla.md"));
    assert!(t.contains("stale_after: 2026-12-01T00:00:00Z") && t.contains("status: draft"), "{t}");
    // Cada concepto se lee como OKF.
    let b = okf::leer(&bundle).unwrap();
    assert_eq!((b.version.as_deref(), b.conceptos.len()), (Some("0.2"), 8));

    // A una base vacía: lo mismo, con los mismos ids.
    let mut z = db::abrir(&dir.join("z.db")).unwrap();
    let r = okf::importar(&mut z, None, &b, &Destino::Proyecto("/q".into()), false).unwrap();
    assert_eq!((r.concepts, r.created, r.updated, r.failed, r.verdicts), (6, 6, 0, 0, 1), "{r:?}");
    let (va, vz) = (todas(&a), todas(&z));
    assert_eq!(va.len(), vz.len());
    for (x, y) in va.iter().zip(&vz) {
        assert_eq!((&x.id, &x.title, &x.content, x.kind, &x.tags, &x.paths, x.pinned, x.global()), (&y.id, &y.title, &y.content, y.kind, &y.tags, &y.paths, y.pinned, y.global()));
        assert_eq!((&x.created_at, &x.updated_at), (&y.created_at, &y.updated_at), "{}", x.title);
        if !x.global() {
            assert_eq!(y.project, "/q");
        }
    }
    let d = memoria::datos_de(&z, &[clave.as_str()]).unwrap();
    assert_eq!(d.get(&clave), Some(&json!({ "edad": 3 })));
    assert_eq!(memoria::por_clave(&z, &Ambito::Todos, "kumi-perfil").unwrap().map(|m| m.id), Some(clave.clone()));
    let v = veredictos::parejas(&z).unwrap();
    assert!(v.values().any(|v| v.memory_id == vieja && v.other_id == nueva && v.relation == veredictos::SUSTITUIDA), "{v:?}");
    let _ = (trampa, receta);

    // Otra vez: nada nuevo ni cambiado.
    let r = okf::importar(&mut z, None, &b, &Destino::Proyecto("/q".into()), false).unwrap();
    assert_eq!((r.created, r.updated, r.unchanged, r.verdicts), (0, 0, 6, 0), "{r:?}");
    assert_eq!(todas(&z).len(), 6);

    // Y la exportación de la base nueva da los mismos conceptos (salvo el nombre del proyecto).
    let otra = dir.join("otra");
    okf::exportar(&z, &otra, Que::Personal).unwrap();
    let quitar = |s: String| s.replace("/q/", "/p/").lines().filter(|l| !l.starts_with("  project: ") && !l.starts_with("  project_key: ")).collect::<Vec<_>>().join("\n");
    assert_eq!(quitar(leer(&otra.join("q/decision/la-autenticación-va-con-jwt.md"))), quitar(jwt));
}

#[test]
fn un_bundle_de_otro() {
    let dir = comun::Temporal::nueva("okf-ajeno");
    let raiz = dir.join("repo").join("knowledge");
    std::fs::create_dir_all(raiz.join("architecture")).unwrap();
    std::fs::create_dir_all(raiz.join("convention")).unwrap();
    std::fs::write(raiz.join("index.md"), "---\nokf_version: \"0.2\"\nscopes: [project]\n---\n\n# KB\n\n* [Architecture](architecture/) - x\n").unwrap();
    std::fs::write(raiz.join("log.md"), "## 2026-11-05\n* **Update**: algo.\n").unwrap();
    std::fs::write(
        raiz.join("architecture").join("governance-model.md"),
        "---\r\ntype: Decision\r\ntitle: Governance vs. Execution Context\r\ndescription: \"3-tier governance model.\"\r\ntags: [governance, code-refs]\r\n\
         generated: { by: agent/cli, at: \"2026-09-11T15:37:43Z\" }\r\ngovernance: constraint\r\ncode_refs: [pkg/okf/types.go, cmd/okf/main.go]\r\n\
         verified:\r\n  - { by: human:ahormati, at: 2026-06-25T09:00:00Z }\r\n  - by: process:nightly\r\n    at: 2026-06-26T02:00:00Z\r\n\
         stale_after: 2026-09-23\r\nsources:\r\n  - id: adr\r\n    resource: https://example.com/adr\r\n---\r\n\r\n# Governance\r\n\r\nSee [layers](layers.md).[^adr]\r\n\r\n[^adr]: ADR\r\n",
    )
    .unwrap();
    std::fs::write(raiz.join("architecture").join("layers.md"), "---\ntype: Architecture\nstatus: deprecated\n---\nFive layers. The token is ghp_abcdefghijklmnopqrstuvwxyz0123456789.\n").unwrap();
    std::fs::write(raiz.join("convention").join("style.md"), "---\ntype: Convention\ntitle: Go style\n---\n").unwrap();

    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    // La carpeta del repositorio vale: se lee su knowledge/.
    let b = okf::leer(&dir.join("repo")).unwrap();
    assert_eq!(b.conceptos.len(), 3);
    let en_seco = okf::importar(&mut db, None, &b, &Destino::Proyecto("/r".into()), true).unwrap();
    assert_eq!((en_seco.created, todas(&db).len()), (3, 0), "en seco no escribe");
    let r = okf::importar(&mut db, None, &b, &Destino::Proyecto("/r".into()), false).unwrap();
    assert_eq!((r.created, r.failed), (3, 0), "{r:?}");
    assert_eq!(r.redacted.len(), 1, "el token se tapa: {r:?}");
    let g = memoria::por_clave(&db, &Ambito::Todos, "okf:architecture/governance-model").unwrap().unwrap();
    assert_eq!(g.kind, Tipo::Decision);
    assert_eq!(g.paths, ["cmd/okf/main.go", "pkg/okf/types.go"]);
    for t in ["governance", "code-refs", "verified", "review:2026-09-23"] {
        assert!(g.tags.contains(&t.to_string()), "{t}: {:?}", g.tags);
    }
    assert!(g.content.starts_with("3-tier governance model.\n\n# Governance") && !g.content.contains('\r'), "la descripción, delante: {}", g.content);
    let d = &memoria::datos_de(&db, &[g.id.as_str()]).unwrap()[&g.id];
    assert_eq!(d["okf"]["governance"], "constraint");
    assert_eq!(d["okf"]["sources"][0]["resource"], "https://example.com/adr");
    assert_eq!(d["okf"]["verified"][1]["by"], "process:nightly");
    let capas = memoria::por_clave(&db, &Ambito::Todos, "okf:architecture/layers").unwrap().unwrap();
    assert_eq!((capas.kind, capas.title.as_str()), (Tipo::Fact, "layers"));
    assert!(capas.tags.contains(&"deprecated".to_string()) && capas.content.contains("[redacted:github-token]"), "{capas:?}");
    let estilo = memoria::por_clave(&db, &Ambito::Todos, "okf:convention/style").unwrap().unwrap();
    assert_eq!((estilo.kind, estilo.content.as_str()), (Tipo::Preference, "Go style"));

    // Dos veces: sin duplicar.
    let r = okf::importar(&mut db, None, &b, &Destino::Proyecto("/r".into()), false).unwrap();
    assert_eq!((r.created, r.unchanged), (0, 3), "{r:?}");
    assert_eq!(todas(&db).len(), 3);

    // Al exportar vuelve lo que OKF tiene y Omoikane no, y cada concepto, en su sitio.
    let fuera = dir.join("fuera");
    okf::exportar(&db, &fuera, Que::Personal).unwrap();
    let t = leer(&fuera.join("r/architecture/governance-model.md"));
    for x in ["governance: constraint", "  by: agent/cli\n", "resource: https://example.com/adr", "by: process:nightly", "by: human:ahormati", "type: Decision"] {
        assert!(t.contains(x), "{x}: {t}");
    }
    let c = leer(&fuera.join("r/architecture/layers.md"));
    assert!(c.contains("type: Architecture") && c.contains("status: deprecated"), "el tipo original vuelve: {c}");
    // Sin la etiqueta, el `verified` de la persona ya no sale (el del proceso, sí).
    let mut cambios = memoria::Cambios { tags: Some(g.tags.iter().filter(|t| *t != "verified").cloned().collect()), ..Default::default() };
    cambios.reason = Some("x".into());
    memoria::actualizar(&mut db, &g.id, cambios, None).unwrap();
    okf::exportar(&db, &fuera, Que::Personal).unwrap();
    let t = leer(&fuera.join("r/architecture/governance-model.md"));
    assert!(!t.contains("human:ahormati") && t.contains("process:nightly"), "{t}");
}

#[test]
fn los_espacios_van_aparte() {
    let dir = comun::Temporal::nueva("okf-espacios");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let acme = proyecto::crear_espacio(&db, "acme").unwrap();
    guardar(&mut db, "/p", Tipo::Fact, "Personal", "Mío.", &[], &[]);
    guardar(&mut db, "", Tipo::Fact, "Global", "De todos.", &[], &[]);
    memoria::guardar(&mut db, Nueva { space_id: Some(acme), kind: Some(Tipo::Fact), title: "Del cliente".into(), content: "Solo de acme.".into(), ..Default::default() }, None).unwrap();
    let personal = dir.join("personal");
    okf::exportar(&db, &personal, Que::Personal).unwrap();
    let todo: String = ficheros(&personal).iter().map(|f| leer(&personal.join(f))).collect();
    assert!(todo.contains("Mío.") && todo.contains("De todos.") && !todo.contains("Solo de acme"));
    let cliente = dir.join("acme");
    okf::exportar(&db, &cliente, Que::Espacio(acme)).unwrap();
    let todo: String = ficheros(&cliente).iter().map(|f| leer(&cliente.join(f))).collect();
    assert!(todo.contains("Solo de acme") && !todo.contains("Mío.") && !todo.contains("De todos."));

    // Lo de un espacio no entra sin decir en qué espacio.
    let mut z = db::abrir(&dir.join("z.db")).unwrap();
    let b = okf::leer(&cliente).unwrap();
    let e = okf::importar(&mut z, None, &b, &Destino::Proyecto("/q".into()), false).unwrap_err();
    assert!(format!("{e}").contains("--space"), "{e}");
    let otro = proyecto::crear_espacio(&z, "acme").unwrap();
    let r = okf::importar(&mut z, None, &b, &Destino::Espacio(otro), false).unwrap();
    assert_eq!(r.created, 1);
    let m = todas(&z).remove(0);
    assert!(m.de_espacio() && m.space == "acme", "{m:?}");

    // Y lo personal no entra en el proyecto de un cliente.
    let mut w = db::abrir(&dir.join("w.db")).unwrap();
    let id = proyecto::registrar(&w, "/cliente").unwrap();
    let s = proyecto::crear_espacio(&w, "acme").unwrap();
    proyecto::asignar_espacio(&w, id, Some(s)).unwrap();
    let b = okf::leer(&personal).unwrap();
    assert!(okf::importar(&mut w, None, &b, &Destino::Proyecto("/cliente".into()), false).is_err());
}

#[test]
fn espejo_solo_toca_lo_suyo() {
    let dir = comun::Temporal::nueva("okf-espejo");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let a = guardar(&mut db, "/p", Tipo::Fact, "Uno", "Primero.", &[], &[]);
    guardar(&mut db, "/p", Tipo::Fact, "Dos", "Segundo.", &[], &[]);
    let bundle = dir.join("b");
    okf::exportar(&db, &bundle, Que::Personal).unwrap();
    std::fs::write(bundle.join("p").join("fact").join("mia.md"), "---\ntype: Note\n---\nMía.\n").unwrap();
    let n = okf::exportar(&db, &bundle, Que::Personal).unwrap();
    assert_eq!((n.created, n.updated, n.unchanged, n.deleted), (0, 0, 2, 0), "{n:?}");
    memoria::olvidar(&mut db, &a).unwrap();
    let n = okf::exportar(&db, &bundle, Que::Personal).unwrap();
    assert_eq!(n.deleted, 1, "{n:?}");
    assert!(!bundle.join("p/fact/uno.md").exists() && bundle.join("p/fact/mia.md").exists());
    assert!(!leer(&bundle.join("p/overview.md")).contains("Uno"));

    // Un bundle de otro no se pisa.
    let ajeno = dir.join("ajeno");
    std::fs::create_dir_all(&ajeno).unwrap();
    std::fs::write(ajeno.join("index.md"), "---\nokf_version: \"0.2\"\n---\n# Suyo\n").unwrap();
    assert!(okf::exportar(&db, &ajeno, Que::Personal).is_err());
    assert_eq!(leer(&ajeno.join("index.md")), "---\nokf_version: \"0.2\"\n---\n# Suyo\n");
}

#[test]
fn lo_que_no_es_un_bundle() {
    let dir = comun::Temporal::nueva("okf-malo");
    assert!(format!("{}", okf::leer(&dir.join("no-existe")).unwrap_err()).contains("not a folder"));
    let vacio = dir.join("vacio");
    std::fs::create_dir_all(&vacio).unwrap();
    assert!(format!("{}", okf::leer(&vacio).unwrap_err()).contains("no OKF concepts"));
    let malo = dir.join("malo");
    std::fs::create_dir_all(&malo).unwrap();
    std::fs::write(malo.join("a.md"), "Sin cabecera.\n").unwrap();
    std::fs::write(malo.join("b.md"), "---\ntitle: sin tipo\n---\n").unwrap();
    std::fs::write(malo.join("c.md"), "---\ntype: [roto\n---\n").unwrap();
    std::fs::write(malo.join("index.md"), "# Índice sin cabecera\n").unwrap();
    let e = format!("{}", okf::leer(&malo).unwrap_err());
    assert!(e.contains("3 files wrong") && e.contains("a.md: no YAML header") && e.contains("b.md: the header has no `type`") && e.contains("c.md: the YAML header can't be read"), "{e}");
}

#[test]
fn por_la_cli() {
    let dir = comun::Temporal::nueva("okf-cli");
    std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
    let omoi = |args: &[&str]| {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_omoi"))
            .args(args)
            .current_dir(dir.join("repo"))
            .env("OMOIKANE_DB", dir.join("o.db"))
            .env("OMOIKANE_MODELS", dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_HOME", dir.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .output()
            .unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    let (ok, s) = omoi(&["remember", "-k", "gotcha", "-t", "Cuidado", "--path", "src/x.rs", "Con X."]);
    assert!(ok, "{s}");
    let bundle = dir.join("bundle");
    let b = bundle.to_str().unwrap();
    let (ok, s) = omoi(&["export", "--okf", b, "--json"]);
    assert!(ok && s.contains("\"created\": 1"), "{s}");
    let (ok, s) = omoi(&["import", "--okf", b, "--dry-run"]);
    assert!(ok && s.contains("0 new and 1 already here"), "{s}");
    let (ok, s) = omoi(&["import", "--okf", b]);
    assert!(ok && s.contains("1 unchanged"), "{s}");
    let (ok, s) = omoi(&["export", "--okf", b, "--obsidian", b]);
    assert!(!ok, "--okf y --obsidian a la vez: {s}");
    let (ok, s) = omoi(&["import", "--space", "x"]);
    assert!(!ok, "--space sin --okf: {s}");
}
