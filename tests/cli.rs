//! La CLI de punta a punta, como la usará una persona o un hook: el binario `omoi` de verdad, con
//! su propia base y dentro de un repositorio de prueba.

mod comun;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Entorno {
    dir: comun::Temporal,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = comun::Temporal::nueva(&format!("cli-{nombre}"));
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
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
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

fn existe(p: &Path) -> bool {
    p.exists()
}

/// El criterio de «hecho» de la fase 1.
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
fn mover_de_proyecto() {
    let e = Entorno::nuevo("mover");
    let otro = e.dir.join("otro");
    std::fs::create_dir_all(otro.join(".git")).unwrap();
    let id = e.json(&["remember", "-k", "fact", "-t", "La web usa Astro", "Astro 5 con Tailwind."])["id"].as_str().unwrap().to_string();
    let v = e.json(&["update", &id, "--project", otro.join(".").to_str().unwrap()]);
    assert!(v["project"].as_str().unwrap().ends_with("otro"), "{v}");
    assert!(e.json(&["history", &id]).as_array().unwrap().is_empty(), "mover no crea versión");
    // Desde el repo de antes ya no se ve; desde el nuevo, sí.
    assert!(e.json(&["search", "astro"]).as_array().unwrap().is_empty());
    assert_eq!(e.json(&["--dir", otro.to_str().unwrap(), "search", "astro"]).as_array().unwrap().len(), 1);
    let g = e.json(&["update", &id, "--global"]);
    assert_eq!(g["project"], "");
    assert_eq!(e.json(&["search", "astro"]).as_array().unwrap().len(), 1);
}

#[test]
fn global_se_ve_desde_cualquier_proyecto() {
    let e = Entorno::nuevo("global");
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea, en inglés."]);
    let otro = e.dir.join("otro");
    std::fs::create_dir_all(&otro).unwrap();
    let r = e.json(&["--dir", otro.to_str().unwrap(), "search", "commits"]);
    assert_eq!(r[0]["project"], "");
    // Lo global no es un proyecto: `omoi projects` no lo lista.
    let p = e.json(&["projects"]);
    assert!(p.as_array().unwrap().is_empty(), "{p}");
}

#[test]
fn export_jsonl() {
    let e = Entorno::nuevo("export");
    e.ok(&["remember", "-k", "fact", "-t", "Una", "uno"]);
    e.ok(&["remember", "-k", "fact", "-t", "Dos", "dos"]);
    let salida = e.ok(&["export"]);
    let lineas: Vec<serde_json::Value> = salida.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // Cabecera, las dos memorias y el cierre (el formato entero se prueba en `copia.rs`).
    assert_eq!(lineas.len(), 4);
    assert_eq!(lineas[0]["type"], "header");
    assert!(lineas[1..3].iter().all(|l| l["type"] == "memory" && l["versions"].is_null()));
    assert_eq!(lineas[3]["count"], 2);
}

#[test]
fn export_a_obsidian() {
    let e = Entorno::nuevo("obsidian");
    e.ok(&["remember", "-k", "gotcha", "-t", "Cuidado con X", "--tag", "nota-a", "--path", "src/x.rs", "Ver [[nota-b]], [[no-existe]] y el rojo #d62828."]);
    let b = e.json(&["remember", "-k", "decision", "-t", "Usamos Y", "--tag", "nota-b", "--path", "src/x.rs", "Porque sí."]);
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea."]);
    e.ok(&["space", "add", "acme"]);
    e.ok(&["remember", "-k", "fact", "-t", "Del cliente", "--space=acme", "Solo de acme."]);
    let vault = e.dir.join("vault");
    let raiz = vault.join("omoikane");
    // Dos memorias con el mismo título: las dos llevan el final de su id.
    let r1 = e.json(&["remember", "-k", "fact", "-t", "Repetida", "--global", "Una."]);
    let r2 = e.json(&["remember", "-k", "fact", "-t", "Repetida", "--global", "--force", "Otra."]);
    let fin = |id: &serde_json::Value| id.as_str().unwrap()[18..].to_lowercase();
    let (fa, fb) = ("Cuidado con X".to_string(), "Usamos Y".to_string());
    e.ok(&["export", "--obsidian", vault.to_str().unwrap()]);
    for r in [&r1, &r2] {
        let global = std::fs::read_to_string(raiz.join("_global").join(format!("Repetida ({}).md", fin(&r["id"])))).unwrap();
        assert!(!global.contains("[[omoikane-global") && !global.contains("
# "), "lo global no cuelga de ninguna nota de grupo: {global}");
    }
    let nota_a = std::fs::read_to_string(raiz.join("repo").join(format!("{fa}.md"))).unwrap();
    assert!(nota_a.starts_with("---\nomoikane: "), "{nota_a}");
    assert!(nota_a.contains(&format!("[[{fb}|nota-b]]")), "{nota_a}");
    assert!(nota_a.contains("no-existe") && !nota_a.contains("[[no-existe"), "un enlace roto va en texto: {nota_a}");
    assert!(nota_a.contains("\\#d62828") && nota_a.contains("tags: [\"gotcha\", \"nota-a\"]"), "{nota_a}");
    assert!(nota_a.contains("[[proyecto-repo]]"));
    assert!(raiz.join("repo").join("proyecto-repo.md").exists() && raiz.join("_global").exists());
    assert_eq!(std::fs::read_dir(raiz.join("_ficheros")).unwrap().count(), 1, "src/x.rs, citado dos veces");
    assert!(vault.join(".obsidian").join("graph.json").exists());
    let todo = |d: &Path| -> String {
        let mut s = String::new();
        let mut pila = vec![d.to_path_buf()];
        while let Some(p) = pila.pop() {
            for x in std::fs::read_dir(&p).unwrap().flatten() {
                if x.path().is_dir() { pila.push(x.path()) } else { s += &std::fs::read_to_string(x.path()).unwrap_or_default() }
            }
        }
        s
    };
    assert!(!todo(&raiz).contains("Solo de acme"), "nada de un espacio en el vault personal");

    // Un espacio, en su vault: solo lo suyo, sin lo global.
    let otro = e.dir.join("vault-acme");
    e.ok(&["export", "--obsidian", otro.to_str().unwrap(), "--space", "acme"]);
    let t = todo(&otro.join("omoikane"));
    assert!(t.contains("Solo de acme") && !t.contains("Commits en inglés") && !t.contains("Cuidado con X"));

    // Lo olvidado desaparece, el enlace a ello queda en texto y lo que no es de Omoikane se queda.
    std::fs::write(raiz.join("repo").join("mia.md"), "---\ntitle: mía\n---\nNota mía.\n").unwrap();
    e.ok(&["forget", b["id"].as_str().unwrap(), "--yes"]);
    let n = e.json(&["export", "--obsidian", vault.to_str().unwrap()]);
    assert!(n["deleted"].as_u64().unwrap() >= 2, "la nota y la del fichero: {n}");
    assert!(!raiz.join("repo").join(format!("{fb}.md")).exists() && raiz.join("repo").join("mia.md").exists());
    let nota_a = std::fs::read_to_string(raiz.join("repo").join(format!("{fa}.md"))).unwrap();
    assert!(nota_a.contains("nota-b") && !nota_a.contains("[[Usamos Y"), "{nota_a}");
    // Sin cambios, no se escribe nada.
    let n = e.json(&["export", "--obsidian", vault.to_str().unwrap()]);
    assert_eq!((n["created"].as_u64(), n["updated"].as_u64(), n["deleted"].as_u64()), (Some(0), Some(0), Some(0)));
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
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
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
    for n in ["remember", "search", "get", "update", "forget", "history", "context", "recent", "decisions"] {
        assert!(nombres.contains(&n), "falta {n} en {nombres:?}");
    }
    assert!(lista["instructions"].as_str().unwrap().contains("long-term memory"));

    let o = e.omoi_con(
        &["tool", "remember"],
        Some(r#"{"title":"Puerto del servidor","content":"Escucha en el 8010","kind":"fact"}"#),
        &[("OMOIKANE_SOURCE", "mcp:claude-code")],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let guardada: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(guardada["saved"], true);
    let entera = e.json(&["get", guardada["id"].as_str().unwrap()]);
    assert_eq!(entera["source"], "mcp:claude-code", "quien llama a `omoi tool` dice quién es");

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
    for a in omoikane::agentes::AGENTES {
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
    for f in [".codex/AGENTS.md", ".gemini/GEMINI.md", ".config/opencode/AGENTS.md", ".qwen/QWEN.md", ".copilot/copilot-instructions.md", ".pi/agent/AGENTS.md"] {
        assert_eq!(std::fs::read_to_string(casa.join(f)).unwrap().matches("<!-- omoikane -->").count(), 1, "{f}");
    }
    // Todos los JSON que escribe se pueden leer y tienen el servidor donde toca.
    assert_eq!(leer(".qwen/settings.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(leer(".qwen/settings.json")["hooks"]["UserPromptSubmit"].to_string().contains("recall"));
    assert_eq!(leer(".kiro/settings/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(leer(".kiro/hooks/omoikane.json")["hooks"].to_string().contains("--format plain"));
    assert_eq!(leer(".copilot/mcp-config.json")["mcpServers"]["omoikane"]["type"], "local");
    assert!(leer(".copilot/hooks/omoikane.json")["hooks"]["SessionStart"].to_string().contains("--format copilot"));
    assert_eq!(leer(".codeium/windsurf/mcp_config.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert_eq!(leer(".gemini/config/mcp_config.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(leer(".gemini/config/hooks.json")["omoikane"]["SessionStart"].to_string().contains("--format antigravity"));
    assert_eq!(leer(".kimi-code/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert_eq!(leer(".pi/agent/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    // Grok: MCP en TOML, sus reglas, y los hooks de cierre (sin recall: sus hooks no añaden contexto).
    assert!(std::fs::read_to_string(casa.join(".grok/config.toml")).unwrap().contains("[mcp_servers.omoikane]"));
    assert_eq!(std::fs::read_to_string(casa.join(".grok/rules/omoikane.md")).unwrap().matches("<!-- omoikane -->").count(), 1);
    let grok = leer(".grok/hooks/omoikane.json");
    assert!(grok["hooks"]["SessionEnd"].to_string().contains("--session-end") && grok["hooks"]["PreCompact"].to_string().contains("--session-end"), "{grok}");
    assert_eq!(grok["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    assert!(grok["hooks"].get("UserPromptSubmit").is_none());
    assert_eq!(leer(".config/amp/settings.json")["amp.mcpServers"]["omoikane"]["args"][0], "mcp");
    let kilo = leer(".config/kilo/kilo.jsonc");
    assert_eq!(kilo["mcp"]["omoikane"]["type"], "local");
    assert!(kilo["instructions"][0].as_str().unwrap().ends_with("omoikane.md"));
    // La orden /omoikane-web donde el agente admite órdenes de usuario: abre la vista web.
    for f in [
        ".codex/prompts/omoikane-web.md",
        ".gemini/commands/omoikane-web.toml",
        ".cursor/commands/omoikane-web.md",
        ".config/opencode/commands/omoikane-web.md",
        ".qwen/commands/omoikane-web.md",
        ".kiro/prompts/omoikane-web.md",
        ".codeium/windsurf/global_workflows/omoikane-web.md",
        ".pi/agent/prompts/omoikane-web.md",
        ".config/kilo/commands/omoikane-web.md",
        ".grok/commands/omoikane-web.md",
    ] {
        let t = std::fs::read_to_string(casa.join(f)).unwrap_or_else(|_| panic!("falta {f}"));
        assert!(t.contains("serve --open") && t.contains("127.0.0.1:7438"), "{f}: {t}");
        // Y a su lado `/omoikane`, que hace guardar al agente.
        let g = f.replace("omoikane-web", "omoikane");
        let t = std::fs::read_to_string(casa.join(&g)).unwrap_or_else(|_| panic!("falta {g}"));
        assert!(t.contains("Save to Omoikane now") && t.contains("`status`") && !t.contains("serve --open"), "{g}: {t}");
    }
    let gemini = std::fs::read_to_string(casa.join(".gemini/commands/omoikane.toml")).unwrap();
    assert!(gemini.starts_with("description = \"Save to Omoikane") && gemini.contains("
prompt = \""), "{gemini}");
    let gemini = std::fs::read_to_string(casa.join(".gemini/commands/omoikane-web.toml")).unwrap();
    assert!(gemini.starts_with("description = \"") && gemini.contains("\nprompt = \""), "{gemini}");
    assert!(std::fs::read_to_string(casa.join(".config/opencode/commands/omoikane-web.md")).unwrap().starts_with("---\ndescription:"));
}

/// 7.5: los hooks de cierre de cada agente, también sobre lo que dejó un `setup` anterior.
#[test]
fn setup_aparta_al_cerrar_en_los_demas_agentes() {
    let e = Entorno::nuevo("setup-cierre");
    let casa = e.dir.join("casa");
    // Lo que dejaba la 0.12: los hooks de Kiro y Copilot sin el de cierre, y el plugin de OpenCode viejo.
    std::fs::create_dir_all(casa.join(".kiro/hooks")).unwrap();
    std::fs::write(casa.join(".kiro/hooks/omoikane.json"), r#"{"version":"v1","hooks":[{"name":"omoikane-session","trigger":"SessionStart","action":{"type":"command","command":"omoi recall --session-start --format plain"},"timeout":30}]}"#).unwrap();
    std::fs::create_dir_all(casa.join(".copilot/hooks")).unwrap();
    std::fs::write(casa.join(".copilot/hooks/omoikane.json"), r#"{"version":1,"hooks":{"SessionStart":[{"type":"command","bash":"omoi recall --session-start --format copilot"}]}}"#).unwrap();
    std::fs::create_dir_all(casa.join(".config/opencode/plugins")).unwrap();
    std::fs::write(casa.join(".config/opencode/plugins/omoikane.ts"), "// Omoikane: recall automático en OpenCode. Lo creó `omoi setup opencode`; bórralo para quitarlo.\nviejo\n").unwrap();
    let h = casa.to_str().unwrap();
    for a in ["codex", "gemini", "qwen", "cursor", "kiro", "copilot", "opencode"] {
        let o = e.omoi_con(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(o.status.success(), "{a}: {}", String::from_utf8_lossy(&o.stderr));
        let o = e.omoi_con(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("nothing to do"), "{a} la segunda vez: {}", String::from_utf8_lossy(&o.stdout));
    }
    let leer = |f: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(casa.join(f)).unwrap()).unwrap();
    let codex = leer(".codex/hooks.json");
    assert!(codex["hooks"]["SessionEnd"].to_string().contains("--session-end"), "{codex}");
    assert_eq!(codex["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    assert!(codex["hooks"]["PreCompact"].to_string().contains("--session-end"));
    let gemini = leer(".gemini/settings.json");
    assert!(gemini["hooks"]["SessionEnd"].to_string().contains("--session-end") && gemini["hooks"]["PreCompress"].to_string().contains("--session-end"));
    let qwen = leer(".qwen/settings.json");
    assert!(qwen["hooks"]["SessionEnd"].to_string().contains("--session-end") && qwen["hooks"]["PreCompact"].to_string().contains("--session-end"));
    let cursor = leer(".cursor/hooks.json");
    assert!(cursor["hooks"]["sessionEnd"].to_string().contains("--session-end") && cursor["hooks"]["preCompact"].to_string().contains("--session-end"));
    let kiro = leer(".kiro/hooks/omoikane.json");
    assert_eq!(kiro["hooks"].as_array().unwrap().len(), 3, "{kiro}");
    assert!(kiro["hooks"].to_string().contains("\"trigger\":\"Stop\""));
    let copilot = leer(".copilot/hooks/omoikane.json");
    assert!(copilot["hooks"]["SessionEnd"].to_string().contains("--session-end") && copilot["hooks"]["SessionStart"].to_string().contains("--session-start"));
    let plugin = std::fs::read_to_string(casa.join(".config/opencode/plugins/omoikane.ts")).unwrap();
    assert!(plugin.contains("session.compacted") && plugin.contains("--session-end") && !plugin.contains("viejo"), "el plugin viejo se rehace");
    // Un plugin que no es de Omoikane no se toca.
    std::fs::write(casa.join(".config/opencode/plugins/omoikane.ts"), "// mío\n").unwrap();
    let o = e.omoi_con(&["setup", "opencode", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(casa.join(".config/opencode/plugins/omoikane.ts")).unwrap(), "// mío\n");
}

/// 7.5: Copilot no da la ruta en `sessionEnd`: `omoi` la busca por el id de sesión.
#[test]
fn al_cerrar_busca_la_conversacion_de_copilot_por_su_id() {
    let e = Entorno::nuevo("cierre-copilot");
    let copilot = e.dir.join("copilot");
    let sesion = copilot.join("session-state").join("abc123");
    std::fs::create_dir_all(&sesion).unwrap();
    let lineas = [
        r#"{"type":"session.start","data":{"sessionId":"abc123"}}"#,
        r#"{"type":"user.message","data":{"content":"vamos a montar la web"}}"#,
        r#"{"type":"assistant.message","data":{"content":"Tengo dos opciones de plantilla. ¿Cuál prefieres?"}}"#,
        r#"{"type":"user.message","data":{"content":"la segunda, pero con la letra más grande que la veo pequeña"}}"#,
    ];
    std::fs::write(sesion.join("events.jsonl"), lineas.join("\n")).unwrap();
    // La base tiene que existir (si no, no hay nada a lo que apartar).
    assert!(e.omoi_con(&["remember", "--kind", "fact", "--title", "Una", "algo"], None, &[]).status.success());
    let entrada = format!(r#"{{"sessionId":"abc123","cwd":{},"reason":"user_exit"}}"#, serde_json::to_string(&e.dir.join("repo").to_string_lossy()).unwrap());
    let o = e.omoi_con(&["recall", "--session-end"], Some(&entrada), &[("COPILOT_HOME", copilot.to_str().unwrap())]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_con(&["review", "--all"], None, &[]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("¿Cuál prefieres? → la segunda, pero con la letra más grande"), "{t}");
}

/// Grok tampoco da la ruta: está en `~/.grok/sessions/<carpeta>/<id>/chat_history.jsonl`.
#[test]
fn al_cerrar_busca_la_conversacion_de_grok_por_su_id() {
    let e = Entorno::nuevo("cierre-grok");
    let grok = e.dir.join("grok");
    let sesion = grok.join("sessions").join("C--repo").join("019a-grok");
    std::fs::create_dir_all(&sesion).unwrap();
    let lineas = [
        r#"{"type":"user","content":[{"type":"text","text":"vamos a montar la web"}],"prompt_index":0}"#,
        r#"{"type":"assistant","content":"Tengo dos opciones de plantilla. ¿Cuál prefieres?"}"#,
        r#"{"type":"user","content":[{"type":"text","text":"la segunda, pero con la letra más grande que la veo pequeña"}],"prompt_index":1}"#,
    ];
    std::fs::write(sesion.join("chat_history.jsonl"), lineas.join("\n")).unwrap();
    assert!(e.omoi_con(&["remember", "--kind", "fact", "--title", "Una", "algo"], None, &[]).status.success());
    let entrada = format!(r#"{{"hookEventName":"session_end","hook_event_name":"SessionEnd","sessionId":"019a-grok","cwd":{}}}"#, serde_json::to_string(&e.dir.join("repo").to_string_lossy()).unwrap());
    let o = e.omoi_con(&["recall", "--session-end"], Some(&entrada), &[("GROK_HOME", grok.to_str().unwrap())]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_con(&["review", "--all"], None, &[]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("¿Cuál prefieres? → la segunda, pero con la letra más grande"), "{t}");
    // Grok no tiene recall al empezar: lo apartado le llega con `context`, una vez.
    let repo = e.dir.join("repo");
    let contexto = |e: &Entorno| {
        let o = e.omoi_con(&["--dir", repo.to_str().unwrap(), "tool", "context"], Some("{}"), &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    let c = contexto(&e);
    assert!(c["set_aside"].to_string().contains("la letra más grande"), "{c}");
    assert!(contexto(&e).get("set_aside").is_none(), "solo una vez");
}

/// Una orden `/omoikane-web` propia del usuario no se toca, ni una `/omoikane` que no escribió Omoikane.
#[test]
fn setup_respeta_una_orden_existente() {
    let e = Entorno::nuevo("setup-orden");
    let casa = e.dir.join("casa");
    std::fs::create_dir_all(casa.join(".qwen/commands")).unwrap();
    std::fs::write(casa.join(".qwen/commands/omoikane-web.md"), "mía").unwrap();
    std::fs::write(casa.join(".qwen/commands/omoikane.md"), "también mía").unwrap();
    let h = casa.to_str().unwrap();
    let o = e.omoi_con(&["setup", "qwen", "--dry-run"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert!(!String::from_utf8_lossy(&o.stdout).contains("/omoikane"), "{}", String::from_utf8_lossy(&o.stdout));
    let o = e.omoi_con(&["setup", "qwen", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(casa.join(".qwen/commands/omoikane-web.md")).unwrap(), "mía");
    assert_eq!(std::fs::read_to_string(casa.join(".qwen/commands/omoikane.md")).unwrap(), "también mía");
}

/// La `/omoikane` de antes, que abría la vista web, pasa a ser la que guarda si la había escrito Omoikane,
/// y la vista web queda en `/omoikane-web`.
#[test]
fn setup_cambia_la_orden_vieja() {
    let e = Entorno::nuevo("setup-orden-vieja");
    let casa = e.dir.join("casa");
    std::fs::create_dir_all(casa.join(".gemini/commands")).unwrap();
    let vieja = casa.join(".gemini/commands/omoikane.toml");
    std::fs::write(&vieja, "description = \"Open the Omoikane memory view in the browser\"\nprompt = \"…\"\n").unwrap();
    let h = casa.to_str().unwrap();
    let o = e.omoi_con(&["setup", "gemini", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = std::fs::read_to_string(&vieja).unwrap();
    assert!(t.contains("Save to Omoikane now") && !t.contains("memory view"), "{t}");
    assert!(std::fs::read_to_string(casa.join(".gemini/commands/omoikane-web.toml")).unwrap().contains("serve --open"));
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
    // Copilot: `additionalContext` arriba del todo.
    let o = e.omoi_con(&["recall", "--session-start", "--format", "copilot"], Some(r#"{"sessionId":"cp1","source":"new"}"#), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["additionalContext"].as_str().unwrap().contains("Commits en inglés"));
    // Antigravity: `conversationId` y `workspacePaths`, y la respuesta en `injectSteps`.
    let ag = format!(r#"{{"conversationId":"ag1","workspacePaths":["{raiz}"]}}"#);
    let o = e.omoi_con(&["recall", "--session-start", "--format", "antigravity"], Some(&ag), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["injectSteps"][0]["systemMessage"]["systemMessage"].as_str().unwrap().contains("Commits en inglés"));
    // Kiro: texto plano, y el mensaje en USER_PROMPT.
    e.ok(&["remember", "-k", "gotcha", "-t", "El importe en f32 pierde céntimos", "En src/pago.rs el importe se guarda en f32 y pierde céntimos al redondear."]);
    let o = e.omoi_con(&["recall", "--format", "plain"], Some(r#"{"session_id":"k1"}"#), &[("USER_PROMPT", "el importe en f32 de src/pago.rs pierde céntimos al redondear")]);
    let t = String::from_utf8(o.stdout).unwrap();
    assert!(t.starts_with("<omoikane>") && t.contains("céntimos"), "{t}");
}

/// Lo que encontró la revisión: una entrada inválida (contenido vacío, título larguísimo) no para la
/// importación, y sin modelo reimportar el mismo fichero no duplica nada.
#[test]
fn importar_entradas_raras_y_sin_modelo() {
    let e = Entorno::nuevo("import2");
    let largo = "t".repeat(300);
    let fichero = e.dir.join("engram.json");
    std::fs::write(
        &fichero,
        format!(
            r#"{{"sessions":[],"observations":[
                {{"id":1,"type":"decision","title":"Primera","content":"Uno.","scope":"project"}},
                {{"id":2,"type":"decision","title":"Sin contenido","content":"","scope":"project"}},
                {{"id":3,"type":"decision","title":"{largo}","content":"Tres.","scope":"project"}},
                {{"id":4,"type":"decision","title":"Cuarta","content":"Cuatro.","scope":"project"}}
            ]}}"#
        ),
    )
    .unwrap();
    let f = fichero.to_str().unwrap();
    let s = e.ok(&["import", f]);
    assert!(s.contains("imported 4") && s.contains("0 failed"), "{s}");
    let s = e.ok(&["import", f]);
    assert!(s.contains("imported 0") && s.contains("4 skipped"), "sin modelo, la segunda vez no debe duplicar: {s}");
    let todas = e.json(&["recent", "-n", "10"]);
    let larga = todas.as_array().unwrap().iter().find(|m| m["title"].as_str().unwrap().starts_with("ttt")).unwrap();
    assert!(larga["title"].as_str().unwrap().chars().count() <= 200);
    assert!(larga["content"].as_str().unwrap().contains(&largo), "el título entero no se pierde");
}

/// `omoi serve`: health sin y con token, y una herramienta por POST, como las del MCP.
#[test]
fn servidor_http() {
    use std::io::{Read, Write};
    let e = Entorno::nuevo("http");
    e.ok(&["remember", "-k", "fact", "-t", "Puerto del servidor", "Escucha en el 8010"]);
    let puerto = comun::puerto_libre();
    // El servidor se cierra aunque el test falle: uno huérfano deja bloqueado el binario en Windows.
    struct Cerrar(std::process::Child);
    impl Drop for Cerrar {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _hijo = Cerrar(Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(["serve", "--port", &puerto.to_string()])
        .current_dir(e.dir.join("repo").join("src"))
        .env("OMOIKANE_DB", e.dir.join("o.db"))
        .env("OMOIKANE_MODELS", e.dir.join("models"))
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stderr(Stdio::null())
        .spawn()
        .unwrap());
    let pedir = |peticion: &str| -> String {
        for _ in 0..50 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", puerto)) {
                s.write_all(peticion.as_bytes()).unwrap();
                // En bytes: la página va en trozos (chunked) y una letra con tilde puede quedar partida
                // entre dos, con la cabecera del trozo en medio.
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                return String::from_utf8_lossy(&v).into_owned();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    };
    let h = format!("Host: 127.0.0.1:{puerto}");
    let sin = pedir(&format!("GET /health HTTP/1.1\r\n{h}\r\nConnection: close\r\n\r\n"));
    assert!(sin.starts_with("HTTP/1.1 401"), "{sin}");
    let con = pedir(&format!("GET /health HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(con.starts_with("HTTP/1.1 200") && con.contains("\"ok\":true"), "{con}");
    let cuerpo = r#"{"query":"puerto servidor"}"#;
    let busca = pedir(&format!(
        "POST /tools/search HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
        cuerpo.len()
    ));
    assert!(busca.starts_with("HTTP/1.1 200") && busca.contains("Puerto del servidor"), "{busca}");
    // La vista web: la página no lleva datos (sin token); la lista, sí.
    let pagina = pedir(&format!("GET / HTTP/1.1\r\n{h}\r\nConnection: close\r\n\r\n"));
    assert!(pagina.starts_with("HTTP/1.1 200") && pagina.contains("<title>Omoikane</title>"), "{}", &pagina[..200.min(pagina.len())]);
    let lista = pedir(&format!("GET /api/memories HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(lista.starts_with("HTTP/1.1 200") && lista.contains("Escucha en el 8010"), "{lista}");
    let sync = pedir(&format!("GET /api/sync HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(sync.starts_with("HTTP/1.1 200") && sync.contains("\"configured\":false"), "{sync}");
    // Una web que apunta su dominio a 127.0.0.1 llega con su Host; otra web que manda un POST, con su Origin.
    let rebinding = pedir(&format!("GET /api/memories HTTP/1.1\r\nHost: malo.example:{puerto}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(rebinding.starts_with("HTTP/1.1 403"), "{rebinding}");
    let ajena = pedir(&format!(
        "POST /tools/forget HTTP/1.1\r\n{h}\r\nOrigin: https://malo.example\r\nAuthorization: Bearer secreto\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{{\"id\":\"x\"}}"
    ));
    assert!(ajena.starts_with("HTTP/1.1 403"), "{ajena}");
}

/// 1.4: una sesión en una carpeta general (la del usuario) no es de un proyecto: el diario dice su
/// carpeta, no una vacía (el proyecto 0 existe con la ruta vacía).
#[test]
fn el_diario_de_una_carpeta_general_dice_la_carpeta() {
    let e = Entorno::nuevo("diario-general");
    let casa = e.dir.join("casa");
    std::fs::create_dir_all(&casa).unwrap();
    let casa_s = casa.to_string_lossy().into_owned();
    let env = [("USERPROFILE", casa_s.as_str()), ("HOME", casa_s.as_str())];
    e.ok(&["remember", "--kind", "fact", "--title", "Algo", "Para que exista la base"]);
    let entrada = serde_json::json!({"session_id": "general-1", "cwd": casa, "hook_event_name": "SessionEnd"}).to_string();
    let o = e.omoi_con(&["recall", "--session-end"], Some(&entrada), &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_con(&["--dir", &casa_s, "diary", "--all", "--json"], None, &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    let carpeta = v[0]["project"].as_str().unwrap().replace('\\', "/").to_lowercase();
    assert!(carpeta.ends_with("/casa"), "{v}");
}

/// 1.4: al cerrar la sesión queda una línea en el diario, que se lee con `omoi diary` y con la
/// herramienta `recent` (`sessions: true`), sin una herramienta más.
#[test]
fn el_diario_apunta_la_sesion_al_cerrar() {
    let e = Entorno::nuevo("diario");
    let repo = e.dir.join("repo");
    e.ok(&["remember", "--kind", "fact", "--title", "Puerto del servidor", "Escucha en el 8010"]);
    let id = e.json(&["recent"])[0]["id"].as_str().unwrap().to_string();
    let ruta = |f: &str| repo.join(f).to_string_lossy().into_owned();
    let lineas = [
        serde_json::json!({"type": "user", "timestamp": "2026-11-03T09:00:00.000Z", "message": {"role": "user", "content": "arregla la sync"}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": ruta("src/sync.rs")}}]}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t2", "name": "mcp__omoikane__update", "input": {"id": id, "append": "y en el 8011"}}]}}),
        serde_json::json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": serde_json::json!({"id": id, "kind": "fact"}).to_string()}]}}),
    ];
    let conv = e.dir.join("sesion.jsonl");
    std::fs::write(&conv, lineas.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    let entrada = serde_json::json!({"session_id": "sesion-1", "transcript_path": conv, "cwd": repo, "hook_event_name": "SessionEnd"}).to_string();
    let o = e.omoi_con(&["recall", "--session-end"], Some(&entrada), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = e.json(&["diary"]);
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v[0]["session"], "sesion-1");
    assert_eq!(v[0]["files"], serde_json::json!(["src/sync.rs"]));
    assert_eq!(v[0]["saved"][0]["title"], "Puerto del servidor", "{v}");
    assert_eq!(v[0]["first_at"], "2026-11-03T09:00:00.000Z");
    // Filtros.
    assert_eq!(e.json(&["diary", "--file", "sync.rs"]).as_array().map(Vec::len), Some(1));
    assert_eq!(e.json(&["diary", "--file", "otro.rs"]).as_array().map(Vec::len), Some(0));
    // Una sesión que empezó el martes y acabó hoy cuenta para el martes; para el lunes, no.
    assert_eq!(e.json(&["diary", "--since", "2026-11-03", "--until", "2026-11-03"]).as_array().map(Vec::len), Some(1));
    assert_eq!(e.json(&["diary", "--until", "2026-11-02"]).as_array().map(Vec::len), Some(0));
    assert_eq!(e.json(&["diary", "--since", "7d"]).as_array().map(Vec::len), Some(1));
    assert!(!e.omoi(&["diary", "--since", "martes"]).status.success());
    let t = e.ok(&["diary"]);
    assert!(t.contains("2026-11-03") && t.contains("src/sync.rs") && t.contains("Puerto del servidor"), "{t}");
    // Para el agente: `recent` con `sessions`, sin herramienta nueva.
    let tool = |args: &str| {
        let o = e.omoi_con(&["--dir", repo.to_str().unwrap(), "tool", "recent"], Some(args), &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    let r = tool(r#"{"sessions":true,"file":"sync"}"#);
    assert_eq!(r[0]["files"][0], "src/sync.rs", "{r}");
    assert_eq!(tool(r#"{"sessions":true,"file":"nada"}"#).as_array().map(Vec::len), Some(0));
    // Sin `sessions`, `recent` sigue siendo lo de siempre.
    assert_eq!(tool("{}")[0]["title"], "Puerto del servidor");
    let lista: serde_json::Value = serde_json::from_str(&e.ok(&["tool"])).unwrap();
    assert_eq!(lista["tools"].as_array().map(Vec::len), Some(9));
    // Ni la sincronización ni `omoi export` lo llevan: es de este equipo.
    let export = e.ok(&["export", "--all"]);
    assert!(!export.contains("sesion-1") && !export.contains("sync.rs"), "{export}");
}

/// La base, la carpeta de Omoikane y los modelos, solo en este equipo: con una ruta de red en `--db`,
/// `OMOIKANE_DB`, `OMOIKANE_HOME` (también tras `~/`) u `OMOIKANE_MODELS`, cada orden abría ahí una
/// conexión SMB (unos 21 s contra un equipo que no responde, y el hash NTLM de la persona para él).
/// Ahora es un error claro antes de tocar nada; e `import` de un fichero de red, también.
/// Solo en Windows: en Linux y macOS esas rutas son nombres locales y no abren ninguna conexión.
#[cfg(windows)]
#[test]
fn ni_la_base_ni_sus_carpetas_en_una_ruta_de_red() {
    let e = Entorno::nuevo("base-red");
    let red = [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\\?\UNC\10.255.255.1\x"];
    for r in red {
        let base = format!("{r}/o.db");
        type Caso<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);
        let casos: [Caso; 4] = [
            (&["--db", base.as_str(), "recent"], &[]),
            (&["recent"], &[("OMOIKANE_DB", base.as_str())]),
            (&["recent"], &[("OMOIKANE_MODELS", r)]),
            (&["import", base.as_str()], &[]),
        ];
        for (args, env) in casos {
            let t = std::time::Instant::now();
            let o = e.omoi_con(args, None, env);
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(!o.status.success() && err.contains("network paths"), "{args:?} {env:?}: {err}");
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "{args:?} {env:?}: {:?}", t.elapsed());
        }
    }
    // `OMOIKANE_HOME` sin `OMOIKANE_DB`.
    for h in [r"\\10.255.255.1\x", r"~/\\10.255.255.1\x"] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(["recent"]).current_dir(e.dir.join("repo")).env_remove("OMOIKANE_DB").env("OMOIKANE_HOME", h).env("OMOIKANE_MODELS", e.dir.join("models")).env("OMOIKANE_AUTO_UPDATE", "0").env_remove("OMOIKANE_LANG");
        let t = std::time::Instant::now();
        let o = c.output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains("OMOIKANE_HOME") && err.contains("network paths"), "{h}: {err}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{h}: {:?}", t.elapsed());
    }
}

/// Una ruta local con el prefijo de ruta extendida (`\\?\C:\…`, la que da `canonicalize`) es de este
/// equipo: `--db`, `OMOIKANE_DB` y `OMOIKANE_HOME` la aceptan (en la 1.8.5 se rechazaba como si fuera
/// de red). `\\?\UNC\…` sigue siendo de red.
#[cfg(windows)]
#[test]
fn la_ruta_extendida_de_una_unidad_es_local() {
    let e = Entorno::nuevo("base-extendida");
    let carpeta = std::fs::canonicalize(&e.dir).unwrap();
    let carpeta = carpeta.to_string_lossy();
    assert!(carpeta.starts_with(r"\\?\"), "{carpeta}");
    let base = format!(r"{carpeta}\extendida.db");
    let o = e.omoi_con(&["--db", base.as_str(), "remember", "-k", "fact", "-t", "Ruta extendida", "La base vive en una ruta extendida."], None, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_con(&["recent"], None, &[("OMOIKANE_DB", base.as_str())]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Ruta extendida"), "{}", String::from_utf8_lossy(&o.stderr));
    // `OMOIKANE_HOME` sin `OMOIKANE_DB`.
    let casa = format!(r"{carpeta}\casa-extendida");
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(["recent"]).current_dir(e.dir.join("repo")).env_remove("OMOIKANE_DB").env("OMOIKANE_HOME", &casa).env("OMOIKANE_MODELS", e.dir.join("models")).env("OMOIKANE_AUTO_UPDATE", "0").env_remove("OMOIKANE_LANG");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.dir.join("casa-extendida").join("omoikane.db").exists());
    // Con `..` o de red, no.
    for mala in [format!(r"{carpeta}\..\..\x.db"), r"\\?\UNC\10.255.255.1\x\o.db".to_string()] {
        let o = e.omoi_con(&["--db", mala.as_str(), "recent"], None, &[]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("network paths"), "{mala}");
    }
}

/// La carpeta en la que la persona trabaja puede estar en una carpeta de red (una unidad compartida,
/// `\\servidor\recurso`, WSL): ahí se lee su repositorio como en la 1.8.0. Con los arreglos de rutas de
/// red de la 1.8.5 un proyecto nuevo desde ahí recibía `dir:persona/netrepo` en vez de su `git:`, sin
/// commit en la procedencia ni `last_seen`, y el hook con ese `cwd` fallaba. Lo que llega de fuera
/// (`project` por MCP) sigue sin abrirse. Se simula con `\\localhost\C$\…` (el recurso administrativo
/// de la unidad de la carpeta temporal); si este equipo no lo deja abrir, el test lo dice y no comprueba
/// nada (la lógica la cubre `proyecto::tests::el_recurso_de_trabajo_es_de_confianza`).
#[cfg(windows)]
#[test]
fn la_carpeta_de_trabajo_puede_ser_de_red() {
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let e = Entorno::nuevo("trabajo-red");
    let repo = e.dir.join("netrepo");
    let git = repo.join(".git");
    std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(git.join("config"), "[remote \"origin\"]\n\turl = https://github.com/acme/netrepo.git\n").unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(git.join("refs").join("heads").join("main"), format!("{SHA}\n")).unwrap();
    let local = std::fs::canonicalize(&repo).unwrap().to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let unc = format!(r"\\localhost\{}$\{}", &local[..1], &local[3..]);
    if !Path::new(&unc).join("src").is_dir() {
        eprintln!("{unc} no se puede abrir en este equipo: no se comprueba");
        return;
    }
    let omoi = |cwd: &str, args: &[&str], entrada: Option<&str>| -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(cwd)
            .env("OMOIKANE_DB", e.dir.join("o.db"))
            .env("OMOIKANE_MODELS", e.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_HOME", e.dir.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_SESSION")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut hijo = c.spawn().unwrap();
        if let Some(t) = entrada {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(t.as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
    };
    let src = format!(r"{unc}\src");
    let o = omoi(&src, &["remember", "-k", "fact", "-t", "Desde la red", "--force", "--json", "Guardada desde una carpeta compartida."], None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    // Su clave es la del repositorio (`origin`), y su carpeta se ha visto aquí.
    let o = omoi(&src, &["projects", "--json"], None);
    let p: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let net = p.as_array().unwrap().iter().find(|x| x["path"].as_str().is_some_and(|s| s.ends_with("netrepo"))).unwrap_or_else(|| panic!("{p}"));
    assert_eq!(net["key"], "git:github.com/acme/netrepo", "{p}");
    let db = rusqlite::Connection::open(e.dir.join("o.db")).unwrap();
    let visto: Option<String> = db.query_row("SELECT last_seen FROM project_path WHERE path LIKE '%netrepo'", [], |f| f.get(0)).unwrap();
    assert!(visto.is_some(), "last_seen");
    drop(db);
    // La procedencia lleva el commit de su HEAD.
    let o = omoi(&src, &["get", &id, "--json"], None);
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(m["provenance"]["commit"], SHA, "{m}");
    // El hook del agente con `cwd` de red (la carpeta de su sesión), lanzado desde otra carpeta.
    let fuera = e.dir.to_string_lossy().into_owned();
    let entrada = serde_json::json!({ "session_id": "s-red", "cwd": src, "source": "startup" }).to_string();
    for args in [&["recall", "--session-start", "--format", "plain"][..], &["recall", "--format", "plain"], &["recall", "--session-end"]] {
        let o = omoi(&fuera, args, Some(&entrada));
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }
    // Lo que llega de fuera, no: `project` de red por MCP, también desde la carpeta de red.
    let t = std::time::Instant::now();
    let o = omoi(&src, &["tool", "remember"], Some(&serde_json::json!({ "kind": "fact", "title": "x", "content": "y", "project": r"\\10.255.255.1\x" }).to_string()));
    let salida = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(salida.contains("network paths"), "{salida}");
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
}
