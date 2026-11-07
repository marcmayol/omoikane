//! 1.8.0: oficios (stacks). Una memoria de oficio (`trade: "android"`) la ven todos los proyectos de
//! ese oficio, de cualquier cliente, y ninguno más; el oficio de cada proyecto sale de sus ficheros y se
//! corrige con `omoi trades`; y al guardarla se avisa de lo que parece de un proyecto o de un cliente.
//! La sincronización está en `tests/sync.rs` (`las_memorias_de_oficio_no_pasan_por_un_cliente`).

mod comun;

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Prueba {
    dir: comun::Temporal,
}

impl Prueba {
    fn nueva(nombre: &str) -> Prueba {
        let p = Prueba { dir: comun::Temporal::nueva(nombre) };
        std::fs::create_dir_all(p.casa()).unwrap();
        p
    }

    fn casa(&self) -> PathBuf {
        self.dir.join("casa")
    }

    /// Un proyecto (un repositorio git, con el `origin` que se dé) con estos ficheros.
    fn proyecto(&self, nombre: &str, origin: Option<&str>, ficheros: &[(&str, &str)]) -> PathBuf {
        let p = self.casa().join(nombre);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        let config = match origin {
            Some(o) => format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {o}\n"),
            None => "[core]\n\tbare = false\n".to_string(),
        };
        std::fs::write(p.join(".git").join("config"), config).unwrap();
        for (f, texto) in ficheros {
            let r = p.join(f);
            std::fs::create_dir_all(r.parent().unwrap()).unwrap();
            std::fs::write(r, texto).unwrap();
        }
        p
    }

    fn orden(&self, en: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        let casa = self.casa();
        c.current_dir(en)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_NO_MODEL", "1")
            .env("USERPROFILE", &casa)
            .env("HOME", &casa)
            .env_remove("OMOIKANE_LANG");
        c
    }

    fn omoi(&self, en: &Path, args: &[&str], entrada: &str) -> Output {
        let mut h = self.orden(en).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        h.stdin.take().unwrap().write_all(entrada.as_bytes()).unwrap();
        h.wait_with_output().unwrap()
    }

    fn ok(&self, en: &Path, args: &[&str]) -> String {
        let o = self.omoi(en, args, "");
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    /// Una herramienta MCP (`omoi tool`): su respuesta y si fue un error.
    fn tool(&self, en: &Path, nombre: &str, args: Value) -> (Value, bool) {
        let o = self.omoi(en, &["tool", nombre], &args.to_string());
        let texto = String::from_utf8_lossy(&o.stdout).to_string();
        (serde_json::from_str(&texto).unwrap_or(Value::String(texto)), !o.status.success())
    }

    fn tool_ok(&self, en: &Path, nombre: &str, args: Value) -> Value {
        let (v, error) = self.tool(en, nombre, args);
        assert!(!error, "{nombre}: {v}");
        v
    }

    fn ids(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect()
    }

    /// Lo que inyecta el recall de un mensaje.
    fn recall(&self, en: &Path, sesion: &str, prompt: &str) -> String {
        let o = self.omoi(en, &["recall", "--format", "plain"], &json!({ "session_id": sesion, "prompt": prompt }).to_string());
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn al_empezar(&self, en: &Path, sesion: &str) -> String {
        let o = self.omoi(en, &["recall", "--session-start", "--format", "plain"], &json!({ "session_id": sesion, "source": "startup" }).to_string());
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
}

const ANDROID: (&str, &str) = ("app/build.gradle.kts", "plugins { id(\"com.android.application\") }");
const WEB: (&str, &str) = ("package.json", r#"{"dependencies":{"astro":"5.0.0"}}"#);

/// La trampa de Android llega a los proyectos Android de cualquier cliente (y a los que no tienen
/// cliente) y no a la web de un cliente: ni en la búsqueda, ni en `recent`, ni en `context`, ni en el
/// recall, ni pidiéndola por su id. Corregir el oficio a mano cambia lo que se ve.
#[test]
fn una_trampa_de_android_no_llega_a_la_web_de_un_cliente() {
    let p = Prueba::nueva("oficios-aislamiento");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "acme"]);
    p.ok(&casa, &["space", "add", "globex"]);
    let web = p.proyecto("acme-web", None, &[WEB]);
    let app = p.proyecto("globex-app", None, &[ANDROID]);
    let mia = p.proyecto("mi-app", None, &[("build.gradle", "apply plugin: 'com.android.application'")]);
    p.ok(&web, &["space", "assign", "acme"]);
    p.ok(&app, &["space", "assign", "globex"]);
    // Cada uno con lo suyo, para que haya algo que ver.
    p.tool_ok(&web, "remember", json!({ "title": "La portada de la web usa islas", "content": "Componentes interactivos solo en la cabecera.", "kind": "fact" }));
    // `context` mira sus ficheros: el oficio de cada uno.
    assert_eq!(p.tool_ok(&web, "context", json!({}))["project"]["trades"], json!(["node", "web"]));
    assert_eq!(p.tool_ok(&app, "context", json!({}))["project"]["trades"], json!(["android"]));
    let r = p.tool_ok(
        &app,
        "remember",
        json!({ "title": "No matar el emulador con taskkill", "content": "taskkill /F deja colgado adb y el emulador no vuelve a arrancar: usar adb emu kill.", "kind": "gotcha", "trade": "android" }),
    );
    assert_eq!((r["saved"].as_bool(), r["project"].as_str()), (Some(true), Some("(trade android)")), "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // La web del cliente acme no la ve.
    let buscar = |en: &Path| Prueba::ids(&p.tool_ok(en, "search", json!({ "query": "emulador taskkill adb" })));
    assert!(!buscar(&web).contains(&id));
    assert!(!Prueba::ids(&p.tool_ok(&web, "recent", json!({}))).contains(&id));
    assert!(!Prueba::ids(&p.tool_ok(&web, "search", json!({ "query": "emulador taskkill adb", "all_projects": true }))).contains(&id));
    let (g, error) = p.tool(&web, "get", json!({ "id": id }));
    assert!(error, "por su id tampoco: {g}");
    assert!(!p.recall(&web, "s-web", "el emulador se queda colgado si lo cierro con taskkill").contains("taskkill /F"));
    // Un Android de otro cliente y uno sin cliente, sí.
    for en in [&app, &mia] {
        assert!(buscar(en).contains(&id), "{}", en.display());
        assert!(Prueba::ids(&p.tool_ok(en, "recent", json!({}))).contains(&id));
        assert_eq!(p.tool_ok(en, "get", json!({ "id": id }))["id"], id.as_str());
    }
    let encontrada = &p.tool_ok(&mia, "search", json!({ "query": "emulador taskkill adb" }))[0];
    assert_eq!((encontrada["trade"].clone(), encontrada.get("global")), (json!(["android"]), None), "{encontrada}");
    let inyectado = p.recall(&mia, "s-mia", "el emulador se queda colgado si lo cierro con taskkill");
    assert!(inyectado.contains("[trade android]") && inyectado.contains("No matar el emulador"), "{inyectado}");
    // Desde la carpeta del usuario, todo.
    assert!(buscar(&casa).contains(&id));
    // Al empezar sesión, el proyecto dice sus oficios.
    let inicio = p.al_empezar(&mia, "s-inicio");
    assert!(inicio.contains("This project's trades (its stack, from its files): android."), "{inicio}");
    // Corregirlo a mano: sin Android, mi-app deja de verla; con `auto`, vuelve.
    p.ok(&mia, &["trades", "remove", "android"]);
    assert!(!buscar(&mia).contains(&id));
    p.ok(&mia, &["trades", "auto"]);
    assert!(buscar(&mia).contains(&id));
    p.ok(&mia, &["trades", "set", "web"]);
    assert!(!buscar(&mia).contains(&id));
    let e: Value = serde_json::from_str(&p.ok(&mia, &["--json", "trades"])).unwrap();
    assert_eq!((e["trades"].clone(), e["fixed"].clone()), (json!(["web"]), json!(true)), "{e}");
    // Si la web de acme también es Android (una app híbrida), la ve: es conocimiento del stack.
    p.ok(&web, &["trades", "add", "android"]);
    assert!(buscar(&web).contains(&id));
    let lista: Value = serde_json::from_str(&p.ok(&casa, &["--json", "trades", "list", "--all"])).unwrap();
    assert_eq!(lista["memories"]["android"], 1, "{lista}");
    let proyectos = p.ok(&casa, &["projects"]);
    assert!(proyectos.lines().any(|l| l.contains("acme-web") && l.contains("(android, node, web)")), "{proyectos}");
}

/// Una memoria de oficio que nombra un proyecto, un cliente, una ruta, un repositorio o un dominio de un
/// cliente no se guarda sin `force` (por MCP, `remember` y `update`; por la CLI, con código 2).
#[test]
fn una_memoria_de_oficio_no_se_lleva_nada_de_un_cliente() {
    let p = Prueba::nueva("oficios-fugas");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "Globex"]);
    let app = p.proyecto("globex-tienda", Some("git@github.com:globex/tienda.git"), &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    p.tool_ok(&app, "remember", json!({ "title": "El staging está en pruebas-rojo.es", "content": "https://pruebas-rojo.es/login con el usuario de QA.", "kind": "fact" }));
    let guardar = |titulo: &str, texto: &str| p.tool_ok(&app, "remember", json!({ "title": titulo, "content": texto, "kind": "gotcha", "trade": "android" }));
    let tipos = |v: &Value| -> Vec<String> { v["trade_leaks"].as_array().map(|a| a.iter().map(|f| f["type"].as_str().unwrap().to_string()).collect()).unwrap_or_default() };
    for (texto, tipo) in [
        ("En globex-tienda el emulador se cuelga con taskkill.", "project"),
        ("Lo vimos en el móvil de Globex: taskkill cuelga el emulador.", "space"),
        (r"El AVD vive en C:\Users\ana\.android\avd y taskkill lo deja corrupto.", "path"),
        ("Ver https://github.com/globex/tienda/issues/12: taskkill cuelga el emulador.", "repository"),
        ("Tras taskkill, el emulador no entra en pruebas-rojo.es hasta reiniciar adb.", "domain"),
    ] {
        let r = guardar("Taskkill cuelga el emulador", texto);
        assert_eq!(r["saved"], false, "{texto}: {r}");
        assert!(tipos(&r).contains(&tipo.to_string()), "{texto}: {r}");
    }
    // Sin nada de nadie, se guarda; y una que de verdad es un falso positivo, con force.
    assert_eq!(guardar("Taskkill cuelga el emulador", "taskkill /F deja colgado adb: usar adb emu kill.")["saved"], true);
    let r = p.tool_ok(&app, "remember", json!({ "title": "Globex Gradle Plugin", "content": "La versión 2 rompe el emulador.", "kind": "gotcha", "trade": "android", "force": true }));
    assert_eq!(r["saved"], true, "{r}");
    // Por la CLI: código 2 y lo que encontró.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador y taskkill", "--trade", "android", "En globex-tienda pasa siempre."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(String::from_utf8_lossy(&o.stdout).contains("globex-tienda"));
    // `update`: una del proyecto que pasa a ser de oficio se mira entera.
    let del_proyecto = p.tool_ok(&app, "remember", json!({ "title": "Globex usa el emulador Pixel 8", "content": "Con la API 35.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": del_proyecto, "trade": "android" }));
    assert_eq!(r["updated"], false, "{r}");
    let limpia = p.tool_ok(&app, "remember", json!({ "title": "El emulador necesita la API 35", "content": "Con la 34 no arranca la cámara.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": limpia, "trade": "android" }));
    assert_eq!(r["trade"], json!(["android"]), "{r}");
    // Lo nuevo de una de oficio también pasa por el aviso.
    let r = p.tool_ok(&app, "update", json!({ "id": limpia, "append": "En globex-tienda, además, la 33." }));
    assert_eq!(r["updated"], false, "{r}");
    // Llevarla a un proyecto le quita el oficio.
    let r = p.tool_ok(&app, "update", json!({ "id": limpia, "project": app.to_string_lossy() }));
    assert!(r.get("trade").is_none() && !r["tags"].to_string().contains("trade:"), "{r}");
}

/// Las preferencias de oficio se inyectan al empezar sesión en todo proyecto de ese oficio (con las
/// globales), y `for` las deja solo para los proyectos de cliente (en un espacio) o los personales.
#[test]
fn preferencias_de_oficio_para_cliente_o_personal() {
    let p = Prueba::nueva("oficios-preferencias");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "acme"]);
    let cliente = p.proyecto("acme-landing", None, &[WEB]);
    let personal = p.proyecto("cuenta-atras", None, &[("index.html", "<html></html>")]);
    let app = p.proyecto("mi-app", None, &[ANDROID]);
    p.ok(&cliente, &["space", "assign", "acme"]);
    let seo = p.tool_ok(&cliente, "remember", json!({ "title": "Revisar GEO y SEO de la landing", "content": "En cada web para un cliente, antes de entregar: títulos, descripciones, datos estructurados y llms.txt.", "kind": "preference", "trade": "web", "for": "client" }));
    assert_eq!(seo["project"], "(trade web, client projects only)", "{seo}");
    let seo = seo["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "remember", json!({ "title": "Pantallas con Scaffold y su padding", "content": "Cada pantalla nueva va dentro de un Scaffold y usa su innerPadding.", "kind": "preference", "trade": "android" }));
    assert_eq!(r["saved"], true, "{r}");
    // Al empezar, en el bloque de preferencias: la de web solo en la web del cliente; la de Android, en la app.
    let (_, pref_cliente) = p.al_empezar(&cliente, "s1").split_once("preferences and the pinned memories").map(|(a, b)| (a.to_string(), b.to_string())).expect("bloque");
    assert!(pref_cliente.contains("Revisar GEO y SEO") && pref_cliente.contains("[trade web, client projects]"), "{pref_cliente}");
    assert!(!pref_cliente.contains("Scaffold"), "{pref_cliente}");
    let inicio_personal = p.al_empezar(&personal, "s2");
    assert!(!inicio_personal.contains("GEO y SEO"), "{inicio_personal}");
    let inicio_app = p.al_empezar(&app, "s3");
    assert!(inicio_app.contains("Pantallas con Scaffold") && !inicio_app.contains("GEO y SEO"), "{inicio_app}");
    // Ni en la búsqueda ni por su id, desde la web personal.
    assert!(!Prueba::ids(&p.tool_ok(&personal, "search", json!({ "query": "GEO SEO landing" }))).contains(&seo));
    assert!(p.tool(&personal, "get", json!({ "id": seo })).1);
    let r = &p.tool_ok(&cliente, "search", json!({ "query": "GEO SEO landing" }))[0];
    assert_eq!((r["id"].as_str(), r["for"].as_str()), (Some(seo.as_str()), Some("client")), "{r}");
    // Para las dos: ahora también la personal. Y una solo personal no llega al cliente.
    p.tool_ok(&cliente, "update", json!({ "id": seo, "for": "both" }));
    assert!(p.al_empezar(&personal, "s4").contains("GEO y SEO"));
    let solo_mia = p.ok(&personal, &["--json", "remember", "-k", "preference", "-t", "Sin cookies de terceros", "--trade", "web", "--for", "personal", "Mis webs no cargan nada de terceros."]);
    let solo_mia = serde_json::from_str::<Value>(&solo_mia).unwrap()["id"].as_str().unwrap().to_string();
    assert!(p.al_empezar(&personal, "s5").contains("Sin cookies de terceros"));
    assert!(!p.al_empezar(&cliente, "s6").contains("Sin cookies de terceros"));
    assert!(p.tool(&cliente, "get", json!({ "id": solo_mia })).1);
}

/// Promover a un oficio una memoria de un proyecto de cliente conserva su id, su historial y sus
/// veredictos; con el texto del cliente no se puede, y su historial (que lo lleva) deja de viajar.
#[test]
fn promover_a_oficio_conserva_la_historia_sin_sacar_al_cliente() {
    let p = Prueba::nueva("oficios-promover");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "acme"]);
    let app = p.proyecto("acme-app", None, &[ANDROID]);
    p.ok(&app, &["space", "assign", "acme"]);
    let id = p.tool_ok(&app, "remember", json!({ "title": "En acme-app el emulador se cuelga", "content": "Pasa al cerrarlo con taskkill.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": id, "append": "Con adb emu kill no pasa.", "reason": "probado" }));
    let otra = p.tool_ok(&app, "remember", json!({ "title": "Emulador colgado tras taskkill", "content": "Reiniciar adb.", "kind": "gotcha", "supersedes": [id] }))["id"].as_str().unwrap().to_string();
    let veredictos = |id: &str| -> i64 {
        rusqlite::Connection::open(p.dir.join("o.db")).unwrap().query_row("SELECT count(*) FROM verdict WHERE memory_id = ?1 OR other_id = ?1", [id], |f| f.get(0)).unwrap()
    };
    assert_eq!(veredictos(&id), 1);
    // Tal cual nombra el proyecto del cliente: no.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android" }));
    assert_eq!(r["updated"], false, "{r}");
    // Reescrita, sí: el mismo id, su historial y su veredicto.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android", "title": "El emulador se cuelga al cerrarlo con taskkill", "content": "Cerrarlo con adb emu kill.", "reason": "es del oficio" }));
    assert_eq!((r["id"].as_str(), r["trade"].clone(), r["history_kept_local"].as_bool()), (Some(id.as_str()), json!(["android"]), Some(true)), "{r}");
    let h = p.tool_ok(&app, "history", json!({ "id": id }));
    assert!(h.as_array().unwrap().len() >= 2 && h.to_string().contains("acme-app"), "el historial sigue aquí: {h}");
    assert_eq!(veredictos(&id), 1);
    assert!(otra != id);
    let base = rusqlite::Connection::open(p.dir.join("o.db")).unwrap();
    let marca: i64 = base.query_row("SELECT count(*) FROM meta WHERE k = 'historial_local:' || ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(marca, 1);
    // Ya es de oficio: la ve otro Android sin cliente.
    let mia = p.proyecto("mi-app", None, &[ANDROID]);
    assert!(Prueba::ids(&p.tool_ok(&mia, "search", json!({ "query": "emulador taskkill" }))).contains(&id));
    // Por la CLI, igual: `--trade` con el texto del cliente sale con código 2.
    let id2 = p.tool_ok(&app, "remember", json!({ "title": "Acme firma con su keystore", "content": "Gradle lo lee de keystore.properties.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let o = p.omoi(&app, &["update", &id2, "--trade", "android"], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    p.ok(&app, &["update", &id2, "--trade", "android", "-t", "La firma se lee de keystore.properties", "-c", "Gradle lo lee de keystore.properties."]);
    assert!(Prueba::ids(&p.tool_ok(&mia, "search", json!({ "query": "keystore properties firma" }))).contains(&id2));
}
