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
    // Sin nada de nadie, se guarda; y una que de verdad es un falso positivo, con allow_leaks.
    assert_eq!(guardar("Taskkill cuelga el emulador", "taskkill /F deja colgado adb: usar adb emu kill.")["saved"], true);
    let r = p.tool_ok(&app, "remember", json!({ "title": "Globex Gradle Plugin", "content": "La versión 2 rompe el emulador.", "kind": "gotcha", "trade": "android", "allow_leaks": true }));
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
    let limpia = r["id"].as_str().unwrap().to_string();
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

/// Cuántos veredictos tiene una memoria (de un lado o del otro).
fn veredictos(p: &Prueba, id: &str) -> i64 {
    rusqlite::Connection::open(p.dir.join("o.db")).unwrap().query_row("SELECT count(*) FROM verdict WHERE memory_id = ?1 OR other_id = ?1", [id], |f| f.get(0)).unwrap()
}

/// Los tipos de las fugas de una respuesta.
fn tipos(v: &Value) -> Vec<String> {
    v["trade_leaks"].as_array().map(|a| a.iter().map(|f| f["type"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

/// Dos clientes con un proyecto Android cada uno: `acme-app` en acme y `beta-app` en beta.
fn dos_clientes(p: &Prueba) -> (PathBuf, PathBuf) {
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "acme"]);
    p.ok(&casa, &["space", "add", "beta"]);
    let app = p.proyecto("acme-app", None, &[ANDROID]);
    let beta = p.proyecto("beta-app", None, &[ANDROID]);
    p.ok(&app, &["space", "assign", "acme"]);
    p.ok(&beta, &["space", "assign", "beta"]);
    (app, beta)
}

/// Promover a un oficio una memoria de un cliente crea una memoria de oficio NUEVA, sin historial ni
/// veredictos (eran del cliente), y la del cliente se queda donde estaba; desde otro cliente ni el
/// historial, ni la original, ni las notas de sus veredictos. Una personal se promueve con su id y su
/// historial. Y el historial que una versión previa dejó «solo aquí» solo sale desde la carpeta general.
#[test]
fn promover_lo_de_un_cliente_crea_una_memoria_nueva() {
    let p = Prueba::nueva("oficios-promover");
    let (app, beta) = dos_clientes(&p);
    let id = p.tool_ok(&app, "remember", json!({ "title": "En acme-app el emulador se cuelga", "content": "Pasa al cerrarlo con taskkill.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": id, "append": "Con adb emu kill no pasa.", "reason": "probado" }));
    p.tool_ok(&app, "remember", json!({ "title": "Emulador colgado tras taskkill en acme", "content": "Reiniciar adb.", "kind": "gotcha", "supersedes": [id] }));
    assert_eq!(veredictos(&p, &id), 1);
    // Tal cual nombra el proyecto del cliente: no.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android" }));
    assert_eq!(r["updated"], false, "{r}");
    // Reescrita: una memoria nueva, sin historial ni veredictos.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "trade": "android", "title": "El emulador se cuelga al cerrarlo con taskkill", "content": "Cerrarlo con adb emu kill.", "reason": "es del oficio" }));
    let nueva = r["id"].as_str().unwrap().to_string();
    assert!(nueva != id && r["promoted_from"] == id.as_str() && r["trade"] == json!(["android"]), "{r}");
    assert_eq!(veredictos(&p, &nueva), 0);
    assert_eq!(veredictos(&p, &id), 1);
    // La del cliente, intacta en su proyecto.
    let o = p.tool_ok(&app, "get", json!({ "id": id }));
    assert!(o["title"] == "En acme-app el emulador se cuelga" && !o["tags"].to_string().contains("trade:") && o["project"].as_str().unwrap().ends_with("acme-app"), "{o}");
    // Desde beta: la nueva, sin historial; la original, ni su historial.
    assert_eq!(p.tool_ok(&beta, "history", json!({ "id": nueva })), json!([]));
    assert!(p.tool(&beta, "history", json!({ "id": id })).1);
    assert_eq!(p.ok(&beta, &["--json", "history", &nueva]).trim(), "[]");
    assert!(!p.omoi(&beta, &["--json", "history", &id], "").status.success(), "por la CLI tampoco");
    let ids = Prueba::ids(&p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" })));
    assert!(ids.contains(&nueva) && !ids.contains(&id), "{ids:?}");
    // Un veredicto de acme sobre la de oficio: desde beta, la nota no dice nada de acme.
    p.tool_ok(&app, "remember", json!({ "title": "En acme-app el AVD se cierra con su botón", "content": "Así lo pide Acme.", "kind": "gotcha", "supersedes": [nueva] }));
    let r = p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == nueva.as_str()).expect("la de oficio").clone();
    assert!(m["note"].as_str().is_some_and(|n| n.contains("not in this scope")) && !m.to_string().to_lowercase().contains("acme"), "{m}");
    let t = p.recall(&beta, "s1", "el emulador se cuelga al cerrarlo con taskkill");
    assert!(t.contains("Cerrarlo con adb emu kill") && !t.to_lowercase().contains("acme"), "{t}");
    // Desde acme, la nota sí la nombra.
    let r = p.tool_ok(&app, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == nueva.as_str()).expect("la de oficio").clone();
    assert!(m["note"].as_str().is_some_and(|n| n.contains("AVD se cierra")), "{m}");
    // Por la CLI, igual: `--trade` con el texto del cliente sale con código 2; reescrita, otra memoria.
    let id2 = p.tool_ok(&app, "remember", json!({ "title": "Acme firma con su keystore", "content": "Gradle lo lee de keystore.properties.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    let o = p.omoi(&app, &["update", &id2, "--trade", "android"], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let r: Value = serde_json::from_str(&p.ok(&app, &["--json", "update", &id2, "--trade", "android", "-t", "La firma se lee de keystore.properties", "-c", "Gradle lo lee de keystore.properties."])).unwrap();
    let nueva2 = r["id"].as_str().unwrap().to_string();
    assert!(nueva2 != id2 && r["promoted_from"] == id2.as_str(), "{r}");
    assert!(Prueba::ids(&p.tool_ok(&beta, "search", json!({ "query": "keystore properties firma" }))).contains(&nueva2));
    assert!(p.tool_ok(&app, "get", json!({ "id": id2 }))["title"] == "Acme firma con su keystore");
    // Una personal (un proyecto sin espacio): el mismo id y su historial.
    let mia = p.proyecto("mi-app", None, &[ANDROID]);
    let pid = p.tool_ok(&mia, "remember", json!({ "title": "Gradle 9 rompe el plugin de Android 8.5", "content": "Usar la 8.7.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&mia, "update", json!({ "id": pid, "append": "O la 8.8.", "reason": "probado" }));
    let r = p.tool_ok(&mia, "update", json!({ "id": pid, "trade": "android" }));
    assert!(r["id"] == pid.as_str() && r.get("promoted_from").is_none(), "{r}");
    assert!(p.tool_ok(&beta, "history", json!({ "id": pid })).as_array().is_some_and(|v| !v.is_empty()), "su historial viene con ella");
    // Lo que una versión previa promovió con el mismo id desde un cliente (`historial_local`): su
    // historial no sale desde un proyecto, sí desde la carpeta general.
    rusqlite::Connection::open(p.dir.join("o.db")).unwrap().execute("INSERT INTO meta(k, v) VALUES ('historial_local:' || ?1, '1')", [&pid]).unwrap();
    assert_eq!(p.tool_ok(&beta, "history", json!({ "id": pid })), json!([]));
    assert!(p.tool_ok(&p.casa(), "history", json!({ "id": pid })).as_array().is_some_and(|v| !v.is_empty()));
}

/// Las fugas no miran solo el título y el contenido: también las etiquetas (salvo las reservadas), la
/// clave y los datos, al guardar, al cambiar una de oficio (aunque solo cambien eso) y al promover. Y
/// comparan sin acentos («balua» es Balùa) y con nombres de espacio de 2 letras.
#[test]
fn las_fugas_miran_etiquetas_clave_y_datos() {
    let p = Prueba::nueva("oficios-fugas-extra");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "Globex"]);
    p.ok(&casa, &["space", "add", "QA"]);
    let app = p.proyecto("globex-tienda", None, &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    let balua = p.proyecto("Balùa", None, &[ANDROID]);
    p.tool_ok(&balua, "remember", json!({ "title": "Nota", "content": "Una nota.", "kind": "fact" }));
    let base = |extra: Value| {
        let mut v = json!({ "title": "Taskkill cuelga el emulador", "content": "Usar adb emu kill.", "kind": "gotcha", "trade": "android" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        p.tool_ok(&app, "remember", v)
    };
    // Cada fuga dice en qué campo está (la clave que nombra un cliente se arregla renombrándola).
    for (extra, tipo, campo) in [
        (json!({ "tags": ["globex-tienda"] }), "project", "tag"),
        (json!({ "key": "globex-perfil" }), "space", "key"),
        (json!({ "data": { "cliente": "Globex" } }), "space", "data"),
        (json!({ "data": { "globex-tienda": 1 } }), "project", "data"),
        (json!({ "content": "En balua pasa siempre." }), "project", "content"),
        (json!({ "content": "Lo vimos en QA ayer." }), "space", "content"),
    ] {
        let r = base(extra.clone());
        assert!(r["saved"] == false && tipos(&r).contains(&tipo.to_string()), "{extra}: {r}");
        let campos: Vec<&str> = r["trade_leaks"].as_array().unwrap().iter().filter(|f| f["type"] == tipo).filter_map(|f| f["field"].as_str()).collect();
        assert!(campos.contains(&campo), "{extra}: se esperaba el campo {campo}: {r}");
    }
    // Las reservadas no cuentan.
    let r = base(json!({ "tags": ["review:2026-12-01", "status", "blocking", "open"] }));
    assert_eq!(r["saved"], true, "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // `update` de una de oficio que solo toca las etiquetas o los datos.
    let r = p.tool_ok(&app, "update", json!({ "id": id, "tags": ["globex"] }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": id, "data": { "visto": "en globex-tienda" } }));
    assert_eq!(r["updated"], false, "{r}");
    let o = p.omoi(&app, &["update", &id, "--data", r#"{"cliente":"Globex"}"#], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    // Por la CLI, una etiqueta.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador y taskkill", "--trade", "android", "--tag", "globex-tienda", "Usar adb."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    // Al promover: la etiqueta y los datos de la memoria del proyecto cuentan.
    let del_proyecto = p.tool_ok(&app, "remember", json!({ "title": "El emulador necesita la API 35", "content": "Con la 34 no arranca la cámara.", "kind": "fact", "tags": ["globex-tienda"] }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": del_proyecto, "trade": "android" }));
    assert!(r["updated"] == false && tipos(&r).contains(&"project".to_string()), "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": del_proyecto, "trade": "android", "tags": [], "data": { "app": "Globex" } }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": del_proyecto, "trade": "android", "tags": [] }));
    assert_eq!(r["trade"], json!(["android"]), "{r}");
}

/// Resolver una decisión de oficio pasa la respuesta por las fugas (por MCP y por la CLI), y una
/// decisión ya resuelta no se resuelve otra vez.
#[test]
fn resolver_una_decision_de_oficio_pasa_por_las_fugas() {
    let p = Prueba::nueva("oficios-decision");
    let (app, beta) = dos_clientes(&p);
    let id = p.tool_ok(&app, "remember", json!({ "title": "¿Cerrar el emulador con taskkill o con adb?", "content": "Asumo adb emu kill.", "kind": "decision", "trade": "android", "tags": ["open"] }))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&beta, "decisions", json!({ "resolve": id, "answer": "En acme-app con adb." }));
    assert!(r["resolved"] == false && tipos(&r).contains(&"project".to_string()), "{r}");
    let o = p.omoi(&beta, &["decisions", "--resolve", &id, "--answer", "Como en acme."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(p.tool_ok(&beta, "get", json!({ "id": id }))["tags"].to_string().contains("open"), "nada se guardó");
    let r = p.tool_ok(&beta, "decisions", json!({ "resolve": id, "answer": "Siempre con adb emu kill." }));
    assert_eq!(r["resolved"], true, "{r}");
    // Otra vez, no.
    let (r, error) = p.tool(&beta, "decisions", json!({ "resolve": id, "answer": "Con adb." }));
    assert!(error && r.to_string().contains("already resolved"), "{r}");
    let o = p.omoi(&beta, &["decisions", "--resolve", &id, "--answer", "Con adb."], "");
    assert!(!o.status.success());
}

/// `force` solo salta las duplicadas: para las fugas hace falta `allow_leaks` (`--allow-leaks`), que
/// nunca deja pasar una ruta absoluta ni la URL de un repositorio.
#[test]
fn force_no_salta_las_fugas() {
    let p = Prueba::nueva("oficios-force");
    let casa = p.casa();
    p.ok(&casa, &["space", "add", "Globex"]);
    let app = p.proyecto("globex-tienda", Some("git@github.com:globex/tienda.git"), &[ANDROID]);
    p.ok(&app, &["space", "assign", "globex"]);
    let guardar = |texto: &str, extra: Value| {
        let mut v = json!({ "title": "Taskkill cuelga el emulador", "content": texto, "kind": "gotcha", "trade": "android" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        p.tool_ok(&app, "remember", v)
    };
    let r = guardar("En globex-tienda pasa siempre.", json!({ "force": true }));
    assert_eq!(r["saved"], false, "force no salta las fugas: {r}");
    let r = guardar("El plugin globex-tienda de Gradle lo rompe.", json!({ "allow_leaks": true }));
    assert_eq!(r["saved"], true, "{r}");
    for texto in [r"El AVD vive en C:\Users\ana\.android\avd.", "Ver https://github.com/globex/tienda/issues/1."] {
        let r = guardar(texto, json!({ "allow_leaks": true, "force": true }));
        assert_eq!(r["saved"], false, "{texto}: {r}");
    }
    // Por la CLI.
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador", "--trade", "android", "--force", "En globex-tienda pasa."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    let o = p.omoi(&app, &["remember", "-k", "gotcha", "-t", "Emulador", "--trade", "android", "--allow-leaks", r"Está en D:\clientes\avd."], "");
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stdout));
    p.ok(&app, &["remember", "-k", "gotcha", "-t", "Emulador y el plugin", "--trade", "android", "--allow-leaks", "El plugin globex-tienda de Gradle."]);
    // `update`: `force` ya no vale para esto.
    let id = guardar("Usar adb emu kill.", json!({}))["id"].as_str().unwrap().to_string();
    let r = p.tool_ok(&app, "update", json!({ "id": id, "append": "En globex-tienda también.", "force": true }));
    assert_eq!(r["updated"], false, "{r}");
    let r = p.tool_ok(&app, "update", json!({ "id": id, "append": "Con el plugin globex-tienda también.", "allow_leaks": true }));
    assert!(r.get("updated").is_none(), "{r}");
}

/// Una etiqueta `review:` mal escrita que llegó por la sincronización (de otra versión) no impide
/// cambiar la memoria (`update`, `remember --key`, `decisions --resolve`): solo se comprueban las nuevas.
#[test]
fn una_fecha_mal_escrita_que_llega_no_bloquea() {
    let p = Prueba::nueva("oficios-fecha-vieja");
    let app = p.proyecto("mi-app", None, &[ANDROID]);
    let id = p.tool_ok(&app, "remember", json!({ "title": "¿SQLite o Room?", "content": "Asumo Room.", "kind": "decision", "tags": ["open"], "key": "almacen" }))["id"].as_str().unwrap().to_string();
    // Como si llegara así de otro equipo.
    rusqlite::Connection::open(p.dir.join("o.db"))
        .unwrap()
        .execute("UPDATE memory SET tags = '[\"open\",\"review:pronto\",\"topic:almacen\"]' WHERE id = ?1", [&id])
        .unwrap();
    p.ok(&app, &["update", &id, "--tag", "open", "--tag", "review:pronto", "--tag", "topic:almacen", "--tag", "datos"]);
    let o = p.omoi(&app, &["update", &id, "--tag", "review:luego"], "");
    assert!(!o.status.success(), "una nueva mal escrita sí se rechaza");
    p.ok(&app, &["remember", "-k", "decision", "-t", "¿SQLite o Room?", "--key", "almacen", "--tag", "open", "Asumo Room, con migraciones."]);
    p.ok(&app, &["decisions", "--resolve", &id, "--answer", "Room."]);
    assert!(p.tool_ok(&app, "get", json!({ "id": id }))["tags"].to_string().contains("resolved"));
}

/// Las notas de los veredictos (en `search` y en el recall) no nombran una memoria de otro cliente:
/// una de oficio que ven acme y beta, sustituida por una de acme, sale en beta sin su título.
#[test]
fn las_notas_no_nombran_otro_cliente() {
    let p = Prueba::nueva("oficios-notas");
    let (app, beta) = dos_clientes(&p);
    let t = p.tool_ok(&beta, "remember", json!({ "title": "El emulador se cuelga con taskkill", "content": "Cerrarlo con adb emu kill.", "kind": "gotcha", "trade": "android" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "remember", json!({ "title": "En acme-app el AVD se cierra con su botón", "content": "Así lo pide Acme.", "kind": "gotcha", "supersedes": [t] }));
    let r = p.tool_ok(&beta, "search", json!({ "query": "emulador taskkill adb" }));
    let m = r.as_array().unwrap().iter().find(|m| m["id"] == t.as_str()).expect("la de oficio").clone();
    assert!(!m.to_string().to_lowercase().contains("acme"), "{m}");
    let texto = p.recall(&beta, "s1", "el emulador se cuelga al cerrarlo con taskkill");
    assert!(texto.contains("adb emu kill") && !texto.to_lowercase().contains("acme"), "{texto}");
}

/// El recall no cambia palabras normales por nombres de proyecto: «faltas» no es Faltis, «rodeo» no es
/// Rodet, «balsa» no es Balùa, ni «tropa» o «trobar» Troba (ver `nombres::mal_escritos`).
#[test]
fn el_recall_no_cambia_palabras_normales_por_proyectos() {
    let p = Prueba::nueva("oficios-recall-nombres");
    let casa = p.casa();
    for (nombre, titulo, texto) in [
        ("Faltis", "Faltis compara tickets del súper", "Lee el ticket y compara precios entre tiendas."),
        ("Rodet", "Rodet pesa las bobinas", "Calcula el filamento que queda en cada bobina."),
        ("Balùa", "Balùa cuenta calorías", "Saca las calorías de la foto de un plato."),
        ("Troba", "Troba indexa el disco", "Busca en los ficheros sin red."),
    ] {
        let d = p.proyecto(nombre, None, &[]);
        p.tool_ok(&d, "remember", json!({ "title": titulo, "content": texto, "kind": "fact" }));
    }
    // Con la traza se ve si el recall cambió alguna palabra por un nombre (`TRAZA nombres`).
    let nombres = |prompt: &str| -> String {
        let mut h = p
            .orden(&casa)
            .args(["recall", "--format", "plain"])
            .env("OMOIKANE_TRAZA", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        h.stdin.take().unwrap().write_all(json!({ "session_id": prompt, "prompt": prompt }).to_string().as_bytes()).unwrap();
        let o = h.wait_with_output().unwrap();
        String::from_utf8_lossy(&o.stderr).lines().filter(|l| l.starts_with("TRAZA nombres")).collect::<Vec<_>>().join(" | ")
    };
    for prompt in ["tengo muchas faltas de ortografía en el informe", "el rodeo de la feria empieza a las seis", "alquilamos una balsa para bajar el río", "la tropa del cole sale de excursión", "no consigo trobar las llaves del coche"] {
        assert_eq!(nombres(prompt), "", "{prompt}");
    }
    // Un nombre mal dictado de verdad, sí.
    assert!(nombres("en faltiz se cuelga la lista").contains("faltis"));
}

/// Absorber una memoria de un cliente en una de oficio se llevaría su historial, sus etiquetas y sus
/// rutas a todos los clientes: no se puede. En una del mismo cliente, sí.
#[test]
fn no_se_absorbe_lo_de_un_cliente_en_una_de_oficio() {
    let p = Prueba::nueva("oficios-absorber");
    let (app, _beta) = dos_clientes(&p);
    let t = p.tool_ok(&app, "remember", json!({ "title": "El emulador se cuelga con taskkill", "content": "Cerrarlo con adb emu kill.", "kind": "gotcha", "trade": "android" }))["id"].as_str().unwrap().to_string();
    let a = p.tool_ok(&app, "remember", json!({ "title": "En acme-app el emulador se cuelga", "content": "Lo vimos en el portátil de Acme.", "kind": "gotcha" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": a, "append": "Y en el de Ana.", "reason": "visto" }));
    let (r, error) = p.tool(&app, "update", json!({ "id": t, "content": "Cerrarlo con adb emu kill; taskkill lo cuelga.", "absorb": [a] }));
    assert!(error && r.to_string().contains("client"), "{r}");
    assert!(p.tool_ok(&app, "get", json!({ "id": a }))["title"] == "En acme-app el emulador se cuelga", "sigue en su sitio");
    // En una del mismo cliente, sí.
    let b = p.tool_ok(&app, "remember", json!({ "title": "El emulador de acme-app necesita la API 35", "content": "Con la 34 no arranca.", "kind": "fact" }))["id"].as_str().unwrap().to_string();
    p.tool_ok(&app, "update", json!({ "id": b, "content": "Con la 34 no arranca y taskkill lo cuelga.", "absorb": [a] }));
}
