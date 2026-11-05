//! 1.6: el recall sobre la documentación del proyecto, en solo lectura (`omoi docs`).

use omoikane::db;
use omoikane::documentos as d;
use omoikane::recall::{self, Entrada};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn dir(nombre: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("omoi-docs-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("repo").join(".git")).unwrap();
    std::fs::create_dir_all(d.join("repo").join("docs").join("guias")).unwrap();
    std::fs::create_dir_all(d.join("repo").join("docs").join(".obsidian")).unwrap();
    d
}

fn escribir(ruta: &Path, texto: &str) {
    std::fs::write(ruta, texto).unwrap();
}

/// Una documentación pequeña: de qué va cada apartado se ve en sus palabras raras.
fn documentacion(docs: &Path) {
    escribir(
        &docs.join("despliegue.md"),
        "---\ntags: [ops]\n---\n# Despliegue\n\nCómo se publica una versión.\n\n## Rollback de Kubernetes\n\nSi el despliegue falla, `kubectl rollout undo deployment/api` vuelve a la réplica anterior. El rollback tarda unos 40 segundos y no toca la base de datos.\n\n## Certificados\n\nLos certificados TLS se renuevan solos con cert-manager cada 60 días.\n",
    );
    escribir(
        &docs.join("guias").join("pagos.md"),
        "# Pagos\n\n## Pasarela Stripe\n\nLos webhooks de Stripe llegan a /hooks/stripe y se verifican con la firma del secreto `STRIPE_WEBHOOK_SECRET`. Un webhook sin firma válida se rechaza con 400.\n\n```bash\n# Esto no es un título\ncurl -X POST /hooks/stripe\n```\n",
    );
    escribir(&docs.join("notas.txt"), "Stripe webhooks rollback kubernetes: no es Markdown, no se indexa.");
    escribir(&docs.join(".obsidian").join("oculto.md"), "# Oculto\n\nStripe webhooks firma: la carpeta oculta no se indexa.");
}

fn preparar(nombre: &str) -> (PathBuf, String, rusqlite::Connection) {
    let base = dir(nombre);
    documentacion(&base.join("repo").join("docs"));
    let project = omoikane::proyecto::de(&base.join("repo")).unwrap();
    let db = db::abrir(&base.join("o.db")).unwrap();
    (base, project, db)
}

fn hay_tabla(db: &rusqlite::Connection, t: &str) -> bool {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)", [t], |f| f.get(0)).unwrap()
}

#[test]
fn trocear_por_titulos() {
    let t = d::trocear(
        "guia",
        "---\ntitle: x\n---\nIntro antes de nada.\n# Uno\ntexto uno\n## Dos\ntexto dos\n```\n# no es título\n```\n### Tres\ntexto tres\n## Cuatro\ntexto cuatro\n#### Cinco sigue en Cuatro\n",
    );
    let titulos: Vec<&str> = t.iter().map(|x| x.titulo.as_str()).collect();
    assert_eq!(titulos, ["guia", "Uno", "Uno › Dos", "Uno › Dos › Tres", "Uno › Cuatro"]);
    assert!(t[2].texto.contains("# no es título"), "{:?}", t[2]);
    assert!(t[4].texto.contains("#### Cinco"));
    assert!(!t.iter().any(|x| x.texto.contains("title: x")), "la cabecera YAML no se indexa");
    // Un apartado largo se parte por párrafos, con el mismo título.
    let largo = (0..40).map(|i| format!("Párrafo {i} con bastantes palabras para ocupar sitio en el trozo.")).collect::<Vec<_>>().join("\n\n");
    let t = d::trocear("x", &format!("# Largo\n{largo}\n"));
    assert!(t.len() > 1);
    assert!(t.iter().all(|x| x.titulo == "Largo" && x.texto.chars().count() <= d::TROZO_MAX));
}

#[test]
fn sin_documentacion_no_crea_nada() {
    let (_base, project, mut db) = preparar("nada");
    let e = Entrada { session_id: "s1".into(), prompt: "cómo hago el rollback de kubernetes si falla el despliegue".into(), ..Default::default() };
    assert_eq!(recall::al_preguntar(&mut db, &project, &e, None).unwrap(), None);
    recall::al_empezar(&mut db, &project, &e).unwrap();
    assert!(!hay_tabla(&db, "docs_folder"), "las tablas aparecen con el primer `omoi docs add`");
    assert!(d::listar(&db, &project).unwrap().is_empty());
}

#[test]
fn trae_el_apartado_que_toca_una_vez_por_sesion() {
    let (base, project, mut db) = preparar("trae");
    let (folder, r) = d::anadir(&db, &project, &base.join("repo").join("docs")).unwrap();
    assert_eq!(r.indexed, 2, "solo los .md, sin las carpetas ocultas: {r:?}");
    assert!(r.complete);
    let l = d::listar(&db, &project).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].folder, folder);
    assert_eq!(l[0].files, 2);

    let e = Entrada { session_id: "s1".into(), prompt: "cómo hago el rollback de kubernetes si falla el despliegue".into(), ..Default::default() };
    let t = recall::al_preguntar(&mut db, &project, &e, None).unwrap().expect("trae la documentación");
    assert!(t.contains("docs/despliegue.md › Despliegue › Rollback de Kubernetes"), "{t}");
    assert!(t.contains("kubectl rollout undo"), "{t}");
    assert!(!t.contains("cert-manager"), "solo el apartado que toca: {t}");
    // Una vez por sesión.
    assert_eq!(recall::al_preguntar(&mut db, &project, &e, None).unwrap(), None);
    // Otra sesión, sí.
    let e2 = Entrada { session_id: "s2".into(), ..e };
    assert!(recall::al_preguntar(&mut db, &project, &e2, None).unwrap().is_some());

    // Otro apartado, otro fichero, y el bloque de código no cuenta como título.
    let e3 = Entrada { session_id: "s1".into(), prompt: "los webhooks de stripe fallan con la firma".into(), ..Default::default() };
    let t = recall::al_preguntar(&mut db, &project, &e3, None).unwrap().expect("trae pagos");
    assert!(t.contains("docs/guias/pagos.md › Pagos › Pasarela Stripe"), "{t}");
    assert!(t.contains("STRIPE_WEBHOOK_SECRET"), "{t}");

    // Lo que no va de la documentación no trae nada.
    for p in ["pon el modo oscuro en la pantalla de ajustes", "vale, sigue", "arregla el test de la caché que falla en windows"] {
        let e = Entrada { session_id: "s3".into(), prompt: p.into(), ..Default::default() };
        assert_eq!(recall::al_preguntar(&mut db, &project, &e, None).unwrap(), None, "{p}");
    }

    // Tras compactar (o en una sesión nueva con el mismo id) puede volver.
    recall::al_empezar(&mut db, &project, &Entrada { session_id: "s1".into(), source: "compact".into(), ..Default::default() }).unwrap();
    assert!(recall::al_preguntar(&mut db, &project, &e3, None).unwrap().is_some());

    // Desde otro proyecto, nada: las carpetas son de cada proyecto.
    std::fs::create_dir_all(base.join("otro").join(".git")).unwrap();
    let otro = omoikane::proyecto::de(&base.join("otro")).unwrap();
    let e4 = Entrada { session_id: "s9".into(), prompt: "cómo hago el rollback de kubernetes si falla el despliegue".into(), ..Default::default() };
    assert_eq!(recall::al_preguntar(&mut db, &otro, &e4, None).unwrap(), None);
}

#[test]
fn refresca_lo_que_cambia_y_quita_lo_que_ya_no_esta() {
    let (base, project, mut db) = preparar("refresco");
    let docs = base.join("repo").join("docs");
    let (folder, _) = d::anadir(&db, &project, &docs).unwrap();
    let r = d::refrescar(&db, &folder, None).unwrap();
    assert_eq!((r.indexed, r.removed, r.unchanged), (0, 0, 2), "{r:?}");

    // Cambia uno (otro tamaño), se borra otro y aparece uno nuevo.
    escribir(&docs.join("despliegue.md"), "# Despliegue\n\n## Canary\n\nEl canary con Argo Rollouts manda el 5 % del tráfico a la réplica nueva durante diez minutos.\n");
    std::fs::remove_file(docs.join("guias").join("pagos.md")).unwrap();
    escribir(&docs.join("guias").join("colas.md"), "# Colas\n\nRabbitMQ reintenta cada mensaje tres veces y después lo manda a la cola muerta `dlq.pedidos`.\n");
    let e = Entrada { session_id: "nueva".into(), ..Default::default() };
    recall::al_empezar(&mut db, &project, &e).unwrap();
    let l = d::listar(&db, &project).unwrap();
    assert_eq!(l[0].files, 2);

    let busca = |db: &rusqlite::Connection, p: &str| d::buscar(db, &project, p, &BTreeSet::new()).unwrap();
    assert!(busca(&db, "cómo hago el rollback de kubernetes si falla el despliegue").is_none_or(|e| !e.excerpt.contains("kubectl")), "lo viejo ya no está");
    assert!(busca(&db, "los webhooks de stripe fallan con la firma").is_none(), "el fichero borrado sale del índice");
    let c = busca(&db, "el canary de argo rollouts con el tráfico").expect("lo cambiado se reindexa");
    assert!(c.excerpt.contains("5 %"), "{c:?}");
    let q = busca(&db, "los mensajes de rabbitmq acaban en la cola muerta").expect("lo nuevo entra");
    assert_eq!(q.path, "docs/guias/colas.md");

    // Quitar la carpeta quita su índice.
    assert!(d::quitar(&db, &project, &folder).unwrap());
    assert!(!d::quitar(&db, &project, &folder).unwrap());
    assert!(d::listar(&db, &project).unwrap().is_empty());
    let n: i64 = db.query_row("SELECT count(*) FROM docs_fts", [], |f| f.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn el_fragmento_es_acotado_y_empieza_donde_toca() {
    let (base, project, db) = preparar("fragmento");
    let docs = base.join("repo").join("docs");
    let relleno = (0..12).map(|i| format!("Párrafo de relleno número {i} que habla de cosas generales del proyecto.")).collect::<Vec<_>>().join("\n\n");
    escribir(&docs.join("largo.md"), &format!("# Operaciones\n\n{relleno}\n\nEl volcado nocturno de PostgreSQL lo hace pgbackrest a las 03:00 y se guarda catorce días en el bucket de copias.\n"));
    d::anadir(&db, &project, &docs).unwrap();
    let e = d::buscar(&db, &project, "cuándo hace pgbackrest el volcado de postgresql", &BTreeSet::new()).unwrap().expect("lo encuentra");
    assert!(e.excerpt.chars().count() <= d::FRAGMENTO + 3, "{}", e.excerpt.chars().count());
    assert!(e.excerpt.contains("pgbackrest"), "empieza en el párrafo que toca: {}", e.excerpt);
}

/// `omoi docs` de punta a punta, y el hook del recall con la documentación.
#[test]
fn la_orden_y_el_hook() {
    let base = dir("cli");
    documentacion(&base.join("repo").join("docs"));
    let omoi = |args: &[&str], entrada: Option<&str>, es: bool| {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(base.join("repo"))
            .env("OMOIKANE_DB", base.join("o.db"))
            .env("OMOIKANE_MODELS", base.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_HOME", base.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if es {
            c.env("OMOIKANE_LANG", "es");
        }
        let mut h = c.spawn().unwrap();
        if let Some(e) = entrada {
            h.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        let o = h.wait_with_output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    // Sin base todavía, mirar no la crea.
    assert!(omoi(&["docs"], None, false).contains("no documentation folders yet"));
    assert!(!base.join("o.db").exists());
    assert!(omoi(&["docs", "add", "docs"], None, false).contains("2 files indexed"));
    let l: serde_json::Value = serde_json::from_str(&omoi(&["docs", "list", "--json"], None, false)).unwrap();
    assert_eq!(l[0]["files"], 2);
    let t = omoi(&["docs", "test", "cómo hago el rollback de kubernetes si falla el despliegue"], None, false);
    assert!(t.starts_with("docs/despliegue.md › Despliegue › Rollback de Kubernetes"), "{t}");
    assert!(omoi(&["docs", "test", "vale, sigue con lo de antes"], None, false).contains("nothing"));

    let cwd = base.join("repo").to_string_lossy().into_owned();
    let entrada = serde_json::json!({"session_id": "s1", "cwd": cwd, "prompt": "los webhooks de stripe fallan con la firma"}).to_string();
    let r = omoi(&["recall"], Some(&entrada), false);
    let v: serde_json::Value = serde_json::from_str(&r).unwrap();
    let texto = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(texto.contains("From this project's documentation"), "{texto}");
    assert!(texto.contains("STRIPE_WEBHOOK_SECRET"), "{texto}");
    // Una vez por sesión.
    assert_eq!(omoi(&["recall"], Some(&entrada), false).trim(), "");

    assert!(omoi(&["docs", "--help"], None, true).contains("solo lectura"));
    assert!(omoi(&["docs", "remove", "docs"], None, false).contains("removed"));
    assert!(omoi(&["docs"], None, false).contains("no documentation folders yet"));
}
