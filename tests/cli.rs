//! La CLI de punta a punta, como la usará una persona o un hook: el binario `omoi` de verdad, con
//! su propia base y dentro de un repositorio de prueba.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Entorno {
    dir: PathBuf,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = std::env::temp_dir().join(format!("omoi-cli-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("repo").join("src")).unwrap();
        Entorno { dir }
    }

    fn omoi(&self, args: &[&str]) -> Output {
        self.omoi_con(args, None, &[])
    }

    fn omoi_con(&self, args: &[&str], entrada: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo").join("src"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env_remove("OMOIKANE_LANG")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut hijo = c.spawn().unwrap();
        if let Some(e) = entrada {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.omoi(args);
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&self.ok(&a)).unwrap()
    }
}

impl Drop for Entorno {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn existe(p: &Path) -> bool {
    p.exists()
}

/// El criterio de «hecho» de la fase 1 (PLAN.md §11).
#[test]
fn remember_y_search_encuentran_bug_en_auth() {
    let e = Entorno::nuevo("hecho");
    assert!(!existe(&e.dir.join("o.db")));
    e.ok(&[
        "remember",
        "-k",
        "gotcha",
        "-t",
        "Bug en auth",
        "--path",
        "src/auth.rs",
        "Fallo de autenticación: el token caduca antes de tiempo.",
    ]);
    e.ok(&["remember", "-k", "fact", "-t", "Receta", "Tortilla de patatas."]);
    let r = e.json(&["search", "fallo de autenticación"]);
    assert_eq!(r.as_array().unwrap().len(), 1);
    assert_eq!(r[0]["title"], "Bug en auth");
    assert_eq!(r[0]["paths"][0], "src/auth.rs");
    // El proyecto es la raíz del repo aunque se ejecute desde src/.
    assert!(r[0]["project"].as_str().unwrap().ends_with("repo"));
    assert!(e.ok(&["search", "fallo", "de", "autenticacion"]).contains("Bug en auth"));
}

#[test]
fn contenido_por_la_entrada_estandar() {
    let e = Entorno::nuevo("stdin");
    let o = e.omoi_con(&["remember", "-k", "decision", "-t", "Por stdin", "--json"], Some("Llega por la tubería.\n"), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(m["content"], "Llega por la tubería.");
    // Sin contenido ni entrada: error claro, nada guardado.
    let o = e.omoi(&["remember", "-k", "fact", "-t", "Vacía"]);
    assert!(!o.status.success());
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
}

#[test]
fn update_history_y_forget() {
    let e = Entorno::nuevo("ciclo");
    let m = e.json(&["remember", "-k", "decision", "-t", "Usamos UUID", "Por compatibilidad."]);
    let id = m["id"].as_str().unwrap();
    let corto = &id[..12];
    e.ok(&["update", corto, "-t", "Usamos ULID", "-c", "Se ordenan por fecha.", "-r", "los UUID no se ordenan"]);
    let h = e.json(&["history", corto]);
    assert_eq!(h[0]["title"], "Usamos UUID");
    assert_eq!(h[0]["reason"], "los UUID no se ordenan");
    assert!(e.ok(&["get", corto]).contains("Se ordenan por fecha."));
    e.ok(&["update", corto, "--pin"]);
    assert_eq!(e.json(&["get", corto])["pinned"], true);
    // Sin --yes y fuera de una terminal, se niega y no borra nada.
    let o = e.omoi(&["forget", corto]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    assert!(e.omoi(&["get", corto]).status.success());
    assert!(e.ok(&["forget", corto, "--yes"]).contains("Usamos ULID"));
    assert!(!e.omoi(&["get", corto]).status.success());
}

#[test]
fn global_se_ve_desde_cualquier_proyecto() {
    let e = Entorno::nuevo("global");
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea, en inglés."]);
    let otro = e.dir.join("otro");
    std::fs::create_dir_all(&otro).unwrap();
    let r = e.json(&["--dir", otro.to_str().unwrap(), "search", "commits"]);
    assert_eq!(r[0]["project"], "");
    let p = e.json(&["projects"]);
    assert_eq!(p.as_array().unwrap().len(), 1);
}

#[test]
fn export_jsonl() {
    let e = Entorno::nuevo("export");
    e.ok(&["remember", "-k", "fact", "-t", "Una", "uno"]);
    e.ok(&["remember", "-k", "fact", "-t", "Dos", "dos"]);
    let salida = e.ok(&["export"]);
    let lineas: Vec<serde_json::Value> = salida.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lineas.len(), 2);
    assert!(lineas.iter().all(|l| l["versions"].as_array().unwrap().is_empty()));
}

#[test]
fn ayuda_en_ingles_y_en_espanol() {
    let e = Entorno::nuevo("idioma");
    let en = String::from_utf8(e.omoi(&["remember", "--help"]).stdout).unwrap();
    assert!(en.contains("Save a memory"), "{en}");
    let o = e.omoi_con(&["remember", "--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Guarda una memoria"), "{es}");
    assert!(es.contains("Uso:") && es.contains("Opciones:") && !es.contains("Options:"), "{es}");
    let o = e.omoi_con(&["--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Comandos:") && !es.contains("Commands:") && es.contains("Muestra la versión"), "{es}");
    // Las opciones globales salen en los subcomandos, también en español.
    let o = e.omoi_con(&["search", "-h"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Base de datos") && es.contains("Argumentos:"), "{es}");
    // Los errores también.
    let o = e.omoi_con(&["get", "ZZZZZZZZ"], None, &[("OMOIKANE_LANG", "es")]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no existe"));
    let o = e.omoi(&["get", "ZZZZZZZZ"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no memory"));
}

#[test]
fn tipo_desconocido_falla_sin_guardar() {
    let e = Entorno::nuevo("tipo");
    let o = e.omoi(&["remember", "-k", "bugfix", "-t", "X", "y"]);
    assert!(!o.status.success());
    assert!(!e.dir.join("o.db").exists() || e.json(&["recent"]).as_array().unwrap().is_empty());
}

fn modelo_int8() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

/// Fase 2 de punta a punta: sin modelo funciona por palabras; con él, avisa de duplicados y
/// encuentra por significado.
#[test]
fn modelo_duplicados_y_significado() {
    let Some(int8) = modelo_int8() else {
        eprintln!("SALTADO: no está el modelo int8");
        return;
    };
    let e = Entorno::nuevo("modelo");
    assert!(e.ok(&["model"]).contains("no model installed"));
    e.ok(&["remember", "-k", "gotcha", "-t", "Bug en auth: el token caduca antes de tiempo", "El middleware compara la hora local con la caducidad del token, que va en UTC."]);

    let instalado = e.ok(&["model", "install", "--from", int8.to_str().unwrap()]);
    assert!(instalado.contains("model ready") && instalado.contains("1 vectors"), "{instalado}");
    assert!(e.ok(&["model"]).contains("* potion-multilingual-128M-int8-"));
    let doctor = e.ok(&["doctor"]);
    assert!(doctor.contains("vectors    1"), "{doctor}");

    // Con modelo, lo que sale por palabras lleva su similitud.
    let r = e.json(&["search", "el token caduca"]);
    assert_eq!(r[0]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert!(r[0]["similarity"].as_f64().is_some());

    // Casi lo mismo: no se guarda, código 2, y dice cuál es la parecida.
    let parecida = ["remember", "-k", "gotcha", "-t", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token en UTC."];
    let o = e.omoi(&parecida);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stdout).contains("not saved"));
    let mut con_json = parecida.to_vec();
    con_json.push("--json");
    let o = e.omoi(&con_json);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["duplicate_of"]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
    // Con --force, sí.
    let mut forzada = parecida.to_vec();
    forzada.push("--force");
    e.ok(&forzada);
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 2);

    assert!(e.ok(&["reindex"]).contains("2 memories, 2 vectors"));
    assert!(e.ok(&["model", "clean"]).contains("0 models deleted"));
}

/// `omoi tool`, lo que usa el mod del plugin: la lista de herramientas y una llamada de cada tipo,
/// con las mismas respuestas que el servidor MCP (y exit 1 cuando la herramienta responde error).
#[test]
fn herramientas_sin_mcp() {
    let e = Entorno::nuevo("tool");
    let lista: serde_json::Value = serde_json::from_str(&e.ok(&["tool"])).unwrap();
    let nombres: Vec<&str> = lista["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["remember", "search", "get", "update", "forget", "history", "context", "recent"] {
        assert!(nombres.contains(&n), "falta {n} en {nombres:?}");
    }
    assert!(lista["instructions"].as_str().unwrap().contains("long-term memory"));

    let o = e.omoi_con(&["tool", "remember"], Some(r#"{"title":"Puerto del servidor","content":"Escucha en el 8010","kind":"fact"}"#), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let guardada: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(guardada["saved"], true);

    let o = e.omoi_con(&["tool", "search"], Some(r#"{"query":"puerto servidor"}"#), &[]);
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r[0]["id"], guardada["id"]);

    let o = e.omoi_con(&["tool", "get"], Some(r#"{"id":"ZZZZZZ"}"#), &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stdout).contains("ZZZZZZ"));
}

/// `omoi import` con una exportación de engram: tipos, ámbito personal → global, borradas fuera,
/// fechas conservadas, y una segunda importación no duplica.
#[test]
fn importar_de_engram() {
    let e = Entorno::nuevo("import");
    let fichero = e.dir.join("engram.json");
    std::fs::write(
        &fichero,
        r#"{"version":"0.2.0","sessions":[{"id":"s1","project":"tienda","directory":"/no/existe"}],
           "observations":[
             {"id":1,"session_id":"s1","type":"bugfix","title":"El importe en f32 pierde céntimos","content":"Guardar el dinero en céntimos enteros.","project":"tienda","scope":"project","topic_key":"pagos/importe","pinned":false,"created_at":"2025-03-01 10:00:00","updated_at":"2025-03-02 11:00:00"},
             {"id":2,"session_id":"s1","type":"preference","title":"Commits en inglés","content":"Una sola línea.","project":"tienda","scope":"personal","pinned":"true","created_at":"2025-03-01 10:00:00","updated_at":"2025-03-01 10:00:00"},
             {"id":3,"session_id":"s1","type":"architecture","title":"Hexagonal","content":"Puertos y adaptadores.","project":"tienda","scope":"project","created_at":"2025-03-01 10:00:00","updated_at":"2025-03-01 10:00:00"},
             {"id":4,"session_id":"s1","type":"decision","title":"Borrada","content":"x","project":"tienda","scope":"project","deleted_at":"2025-04-01 00:00:00"}
           ]}"#,
    )
    .unwrap();
    let f = fichero.to_str().unwrap();
    let salida = e.ok(&["import", f]);
    assert!(salida.contains("imported 3") && salida.contains("1 deleted"), "{salida}");
    let todas = e.json(&["recent", "--all", "-n", "10"]);
    let por_titulo = |t: &str| todas.as_array().unwrap().iter().find(|m| m["title"] == t).unwrap().clone();
    let bug = por_titulo("El importe en f32 pierde céntimos");
    assert_eq!(bug["kind"], "gotcha");
    assert!(bug["created_at"].as_str().unwrap().starts_with("2025-03-01T10:00:00"));
    assert!(bug["tags"].as_array().unwrap().iter().any(|t| t == "topic:pagos/importe"));
    let pref = por_titulo("Commits en inglés");
    assert_eq!(pref["project"], "");
    assert_eq!(pref["pinned"], true);
    assert!(por_titulo("Hexagonal")["tags"].as_array().unwrap().iter().any(|t| t == "architecture"));
    assert!(todas.as_array().unwrap().iter().all(|m| m["title"] != "Borrada"));
}

/// `omoi setup <agente>` en una carpeta personal de prueba: escribe lo que toca, respeta lo que ya
/// había, guarda copia del original y una segunda vez no cambia nada.
#[test]
fn setup_de_otros_agentes() {
    let e = Entorno::nuevo("setup");
    let casa = e.dir.join("casa");
    std::fs::create_dir_all(casa.join(".gemini")).unwrap();
    std::fs::write(casa.join(".gemini/settings.json"), r#"{"theme":"dark","mcpServers":{"otro":{"command":"x"}}}"#).unwrap();
    let h = casa.to_str().unwrap();
    for a in ["codex", "gemini", "cursor", "opencode"] {
        let o = e.omoi_con(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(o.status.success(), "{a}: {}", String::from_utf8_lossy(&o.stderr));
        let o = e.omoi_con(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("nothing to do"), "{a} la segunda vez");
    }
    let leer = |f: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(casa.join(f)).unwrap()).unwrap();
    let g = leer(".gemini/settings.json");
    assert_eq!(g["theme"], "dark");
    assert_eq!(g["mcpServers"]["otro"]["command"], "x");
    assert_eq!(g["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(g["hooks"]["BeforeAgent"].to_string().contains("--format gemini"));
    assert!(casa.join(".gemini/settings.json.antes-de-omoikane").exists());
    assert!(leer(".codex/hooks.json")["hooks"]["UserPromptSubmit"].to_string().contains("recall"));
    assert!(std::fs::read_to_string(casa.join(".codex/config.toml")).unwrap().contains("[mcp_servers.omoikane]"));
    assert!(leer(".cursor/hooks.json")["hooks"]["sessionStart"].to_string().contains("--format cursor"));
    assert_eq!(leer(".config/opencode/opencode.json")["mcp"]["omoikane"]["type"], "local");
    let plugin = std::fs::read_to_string(casa.join(".config/opencode/plugins/omoikane.ts")).unwrap();
    // Ningún salto de línea de verdad dentro de una cadena de JS (rompía el plugin sin avisar).
    assert!(plugin.contains("experimental.chat.system.transform") && !plugin.contains("+= \"\n"));
    for f in [".codex/AGENTS.md", ".gemini/GEMINI.md", ".config/opencode/AGENTS.md"] {
        assert_eq!(std::fs::read_to_string(casa.join(f)).unwrap().matches("<!-- omoikane -->").count(), 1, "{f}");
    }
}

/// `omoi recall --format`: Gemini cambia el nombre del evento y Cursor usa `additional_context`;
/// la entrada de Cursor trae `conversation_id` y `workspace_roots` en vez de `session_id` y `cwd`.
#[test]
fn recall_en_formato_de_otros_agentes() {
    let e = Entorno::nuevo("formatos");
    e.ok(&["remember", "-k", "preference", "--global", "-t", "Commits en inglés", "Una sola línea."]);
    let raiz = e.dir.join("repo").to_string_lossy().replace('\\', "/");
    let cursor = format!(r#"{{"conversation_id":"c1","workspace_roots":["{raiz}"],"hook_event_name":"sessionStart"}}"#);
    let o = e.omoi_con(&["recall", "--session-start", "--format", "cursor"], Some(&cursor), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["additional_context"].as_str().unwrap().contains("Commits en inglés"));
    let o = e.omoi_con(&["recall", "--session-start", "--format", "gemini"], Some(r#"{"session_id":"g1","source":"startup"}"#), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
}
