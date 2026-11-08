//! 1.8.5: verificada por una persona frente a escrita por un agente, y la procedencia (sesión y commit)
//! de cada memoria. Con el binario `omoi` de verdad, su propia base y un repositorio de prueba.

mod comun;

use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

/// Las variables de la shell de un agente (Claude Code, Codex…) y de su sesión: fuera, salvo que el
/// test las ponga, para que no cambie nada según desde dónde se lancen los tests.
fn de_agente() -> impl Iterator<Item = &'static str> {
    omoikane::verificado::DE_AGENTE.iter().copied().chain(["OMOIKANE_SESSION"])
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

struct Entorno {
    dir: comun::Temporal,
}

impl Entorno {
    /// Con un repositorio `repo` cuyo HEAD es la rama `main` en [`SHA`].
    fn nuevo(nombre: &str) -> Entorno {
        let dir = comun::Temporal::nueva(&format!("verif-{nombre}"));
        let git = dir.join("repo").join(".git");
        std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        std::fs::create_dir_all(dir.join("repo").join("src")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs").join("heads").join("main"), format!("{SHA}\n")).unwrap();
        Entorno { dir }
    }

    fn orden(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.current_dir(self.dir.join("repo").join("src"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("USERPROFILE", self.dir.join("casa"))
            .env("HOME", self.dir.join("casa"))
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_AUTHOR");
        for v in de_agente() {
            c.env_remove(v);
        }
        c
    }

    fn omoi_con(&self, args: &[&str], entrada: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = self.orden();
        c.args(args).stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut hijo = c.spawn().unwrap();
        if let Some(e) = entrada {
            hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
    }

    fn ok_con(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let o = self.omoi_con(args, None, env);
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        o
    }

    fn ok(&self, args: &[&str]) -> String {
        String::from_utf8(self.ok_con(args, &[]).stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    fn guardar(&self, kind: &str, titulo: &str, texto: &str) -> String {
        self.json(&["remember", "-k", kind, "-t", titulo, "--force", texto])["id"].as_str().unwrap().to_string()
    }

    /// Una herramienta del MCP, como la llama un agente (`omoi tool`).
    fn herramienta(&self, nombre: &str, args: serde_json::Value) -> serde_json::Value {
        let o = self.omoi_con(&["tool", nombre], Some(&args.to_string()), &[("OMOIKANE_SOURCE", "mcp:prueba")]);
        assert!(o.status.success(), "tool {nombre}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    fn base(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }
}

#[test]
fn una_persona_verifica_y_se_ve_en_get_search_list_y_recall() {
    let e = Entorno::nuevo("ver");
    let id = e.guardar("decision", "Pagos en céntimos enteros", "Decidido: el importe se guarda en céntimos, nunca en f32.");
    e.ok(&["update", &id, "--tag", "pagos", "--pin"]);
    assert!(e.json(&["get", &id]).get("verified").is_none(), "nadie la ha verificado");

    // Sin terminal y sin --yes, no; dentro de la shell de un agente, ni con --yes.
    let o = e.omoi_con(&["verify", &id, "--by", "Marc"], None, &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--yes"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_con(&["verify", &id, "--by", "Marc", "--yes"], None, &[("CLAUDECODE", "1")]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("CLAUDECODE"), "{}", String::from_utf8_lossy(&o.stderr));
    // Las que ponen de verdad otros agentes (y Claude Code sin `CLAUDECODE`), también.
    for (k, v) in [("CLAUDE_CODE_ENTRYPOINT", "cli"), ("CLAUDE_CODE_SESSION_ID", "s"), ("OPENCODE", "1"), ("CLINE_ACTIVE", "true"), ("COPILOT_AGENT", "1"), ("WINDSURF_CASCADE_TERMINAL", "1"), ("CODEX_THREAD_ID", "t")] {
        let o = e.omoi_con(&["verify", &id, "--by", "Marc", "--yes"], None, &[(k, v)]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(k), "{k}: {}", String::from_utf8_lossy(&o.stderr));
    }
    assert!(e.json(&["get", &id]).get("verified").is_none());
    // Y la configuración que la persona tiene en su terminal no la confunde con un agente.
    let o = e.omoi_con(&["verify", &id, "--by", "Marc", "--yes", "--json"], None, &[("CLAUDE_CODE_USE_BEDROCK", "1"), ("AIDER_MODEL", "x"), ("CURSOR_TRACE_ID", "x")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    e.ok(&["verify", &id, "--unverify", "--yes"]);

    let salida = e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    assert!(salida.contains("verified") && salida.contains("Marc"), "{salida}");
    let m = e.json(&["get", &id]);
    assert_eq!(m["verified"]["by"], "Marc", "{m}");
    assert!(m["verified"]["at"].as_str().unwrap().starts_with("20"));
    // Sin versión nueva: verificar no cambia el texto.
    assert_eq!(e.json(&["history", &id]).as_array().unwrap().len(), 1, "la del --tag/--pin, y nada más");
    assert!(e.ok(&["get", &id]).contains("verified   by Marc"));
    // Búsqueda, lista y recientes.
    let r = e.json(&["search", "importe en céntimos"]);
    assert_eq!(r[0]["verified"]["by"], "Marc", "{r}");
    assert!(e.ok(&["search", "importe en céntimos"]).contains("(verified by Marc)"));
    assert_eq!(e.json(&["list", "--tag", "pagos"])[0]["verified"]["by"], "Marc");
    assert_eq!(e.json(&["recent"])[0]["verified"]["by"], "Marc");
    // El recall: una marca corta tras el tipo.
    let o = e.omoi_con(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"s1","source":"startup"}"#), &[]);
    let t = String::from_utf8(o.stdout).unwrap();
    assert!(t.contains("[decision, verified]") && t.contains("Pagos en céntimos"), "{t}");
    // Y lo que el agente lee por MCP: `context`, `search` y `get`.
    let c = e.herramienta("context", serde_json::json!({}));
    let en_contexto = c["memories"].as_array().unwrap().iter().find(|x| x["id"] == id.as_str()).unwrap_or_else(|| panic!("{c}"));
    assert!(en_contexto["verified"].as_str().unwrap().starts_with("by Marc, 20"), "{en_contexto}");
    let s = e.herramienta("search", serde_json::json!({ "query": "importe en céntimos" }));
    assert!(s.to_string().contains("by Marc"), "{s}");
    assert_eq!(e.herramienta("get", serde_json::json!({ "id": id }))["verified"]["by"], "Marc");

    // Quitarla.
    assert!(e.ok(&["verify", &id, "--unverify"]).contains("no longer verified"));
    assert!(e.json(&["get", &id]).get("verified").is_none());
    assert!(e.ok(&["verify", &id, "--unverify"]).contains("was not verified"));
}

/// Cambiar el texto la des-verifica (y lo dice); etiquetas, rutas o fijarla, no. Un cambio hecho por
/// una versión que no la entiende (el sello se queda en `extra`) tampoco la deja verificada.
#[test]
fn cambiar_el_texto_la_desverifica() {
    let e = Entorno::nuevo("cambio");
    let id = e.guardar("gotcha", "El emulador no se mata con taskkill", "Usar adb emu kill.");
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    e.ok(&["update", &id, "--tag", "android", "--path", "src/x.rs", "--pin"]);
    assert_eq!(e.json(&["get", &id])["verified"]["by"], "Marc", "etiquetas, rutas y fijarla no cambian lo que dice");

    let o = e.ok_con(&["update", &id, "--append", "Y esperar a que se cierre.", "-r", "más"], &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no longer verified"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.json(&["get", &id]).get("verified").is_none());
    let extra: String = e.base().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert!(!extra.contains("verified"), "el sello se va: {extra}");

    // Por MCP: el agente cambia el texto y la respuesta se lo dice.
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    let r = e.herramienta("update", serde_json::json!({ "id": id, "content": "Usar adb emu kill y esperar.", "reason": "más corto" }));
    assert!(r["unverified"].as_str().unwrap().contains("verified by Marc"), "{r}");
    assert!(e.json(&["get", &id]).get("verified").is_none());

    // Una versión anterior que conserva el sello sin entenderlo y cambia el contenido: la huella ya no
    // coincide.
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    e.base().execute("UPDATE memory SET content = 'Cambiado por una 1.0.' WHERE id = ?1", [&id]).unwrap();
    let extra: String = e.base().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert!(extra.contains("\"verified\""), "{extra}");
    assert!(e.json(&["get", &id]).get("verified").is_none());
    assert!(e.ok(&["get", &id]).contains("verified   no"));
}

/// Ningún camino del agente acaba en «verificada»: no hay herramienta, `remember` y `update` ignoran
/// un `verified` que el agente ponga, y lo que lleve `data` va aparte.
#[test]
fn un_agente_no_puede_verificarse() {
    let e = Entorno::nuevo("agente");
    let lista = e.herramienta_lista();
    let nombres: Vec<&str> = lista["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!nombres.iter().any(|n| n.contains("verif")), "{nombres:?}");
    assert!(lista["instructions"].as_str().unwrap().contains("never do it yourself"));
    let sello = serde_json::json!({ "by": "Marc", "at": "2026-11-08T00:00:00.000Z", "sha": "x" });
    let r = e.herramienta(
        "remember",
        serde_json::json!({ "kind": "decision", "title": "Me verifico yo", "content": "Un agente que se pone el sello.", "verified": sello, "data": { "verified": sello } }),
    );
    let id = r["id"].as_str().unwrap().to_string();
    assert!(e.json(&["get", &id]).get("verified").is_none());
    e.herramienta("update", serde_json::json!({ "id": id, "tags": ["x"], "verified": sello }));
    assert!(e.json(&["get", &id]).get("verified").is_none());
}

impl Entorno {
    fn herramienta_lista(&self) -> serde_json::Value {
        let o = self.omoi_con(&["tool"], None, &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
}

/// La procedencia: la sesión (del entorno o de los hooks) y el commit del repositorio, sin `git`.
#[test]
fn guarda_la_sesion_y_el_commit() {
    let e = Entorno::nuevo("proc");
    let o = e.ok_con(&["remember", "-k", "fact", "-t", "Con sesión", "--json", "Algo."], &[("OMOIKANE_SESSION", "sesion-1")]);
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let id = m["id"].as_str().unwrap();
    assert_eq!(m["provenance"], serde_json::json!({ "session": "sesion-1", "commit": SHA, "branch": "main" }), "{m}");
    assert_eq!(e.json(&["get", id])["provenance"]["commit"], SHA);
    assert!(e.ok(&["get", id]).contains("written in session sesion-1 · commit 0123456789ab (main)"));
    assert!(e.ok(&["history", id]).contains("written in session sesion-1"));
    // Claude Code la pone en el entorno de lo que lanza.
    let o = e.ok_con(&["remember", "-k", "fact", "-t", "De Claude", "--json", "Otra."], &[("CLAUDE_CODE_SESSION_ID", "cc-2")]);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["provenance"]["session"], "cc-2");
    // Sin variable: la única sesión que los hooks apuntaron en este proyecto.
    let o = e.omoi_con(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"hook-3","source":"startup"}"#), &[]);
    assert!(o.status.success());
    assert_eq!(e.json(&["remember", "-k", "fact", "-t", "Del hook", "Una más."])["provenance"]["session"], "hook-3");
    // Con dos a la vez en el proyecto no adivina: solo el commit.
    e.omoi_con(&["recall", "--session-start", "--format", "plain"], Some(r#"{"session_id":"hook-4","source":"startup"}"#), &[]);
    let m = e.json(&["remember", "-k", "fact", "-t", "Dos sesiones", "Y otra."]);
    assert_eq!(m["provenance"], serde_json::json!({ "commit": SHA, "branch": "main" }), "{m}");
    // Por MCP también.
    let r = e.herramienta("remember", serde_json::json!({ "kind": "fact", "title": "Por MCP", "content": "Desde el agente.", "force": true }));
    assert_eq!(e.json(&["get", r["id"].as_str().unwrap()])["provenance"]["commit"], SHA);
    // Cambiarla no la toca: es la de cuando se escribió.
    e.ok_con(&["update", id, "-c", "Cambiada.", "-r", "x"], &[("OMOIKANE_SESSION", "otra")]);
    assert_eq!(e.json(&["get", id])["provenance"]["session"], "sesion-1");
    // Una de oficio no lleva nada del proyecto.
    let m = e.ok_con(&["remember", "-k", "gotcha", "-t", "No matar el emulador", "--trade", "android", "--json", "Usar adb emu kill."], &[("OMOIKANE_SESSION", "s")]);
    assert!(serde_json::from_slice::<serde_json::Value>(&m.stdout).unwrap().get("provenance").is_none());
    // Sin repositorio ni sesión: nada (ni `extra`).
    let suelta = e.dir.join("suelta");
    std::fs::create_dir_all(&suelta).unwrap();
    let m = e.json(&["--dir", suelta.to_str().unwrap(), "remember", "-k", "fact", "-t", "Sin nada", "Nada."]);
    assert!(m.get("provenance").is_none(), "{m}");
}

/// Exportar e importar (lo mismo que la sincronización: campos de la línea) conserva el sello y la
/// procedencia, y el sello sigue valiendo al otro lado.
#[test]
fn viajan_con_la_memoria() {
    let e = Entorno::nuevo("viaje");
    let id = e.json(&["remember", "-k", "decision", "-t", "Viaja", "Con sello."])["id"].as_str().unwrap().to_string();
    e.ok(&["verify", &id, "--by", "Marc", "--yes"]);
    let salida = e.ok(&["export"]);
    let linea: serde_json::Value = salida.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|l| l["id"] == id.as_str()).unwrap();
    assert_eq!(linea["verified"]["by"], "Marc", "{linea}");
    assert_eq!(linea["provenance"]["commit"], SHA, "{linea}");
    let otra = Entorno::nuevo("viaje2");
    let f = otra.dir.join("copia.jsonl");
    std::fs::write(&f, &salida).unwrap();
    otra.ok(&["import", f.to_str().unwrap()]);
    let m = otra.json(&["get", &id]);
    assert_eq!((&m["verified"]["by"], &m["provenance"]["commit"]), (&serde_json::json!("Marc"), &serde_json::json!(SHA)), "{m}");

    // Desde la shell de un agente, la copia entra sin el sello (la huella se calcula con el texto: si
    // no, cualquiera que escriba el fichero se verificaría a sí mismo) y lo dice.
    let tercera = Entorno::nuevo("viaje3");
    let f = tercera.dir.join("copia.jsonl");
    std::fs::write(&f, &salida).unwrap();
    let o = tercera.ok_con(&["import", f.to_str().unwrap()], &[("CLAUDECODE", "1")]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("they come in unverified"), "{}", String::from_utf8_lossy(&o.stderr));
    let m = tercera.json(&["get", &id]);
    assert!(m.get("verified").is_none(), "{m}");
    assert_eq!(m["provenance"]["commit"], SHA, "lo demás llega: {m}");
}

/// La procedencia de una memoria global (o de un espacio, o de oficio) no lleva el commit ni la rama
/// de la carpeta en la que se escribió: puede ser la de un cliente y esas memorias se ven desde otros.
#[test]
fn una_global_no_lleva_la_rama_del_cliente() {
    let e = Entorno::nuevo("rama");
    let git = e.dir.join("repo").join(".git");
    std::fs::create_dir_all(git.join("refs").join("heads").join("feature")).unwrap();
    std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/acme-secret-merger\n").unwrap();
    std::fs::write(git.join("refs").join("heads").join("feature").join("acme-secret-merger"), format!("{SHA}\n")).unwrap();
    let sesion = [("OMOIKANE_SESSION", "s-1")];
    let global: serde_json::Value = serde_json::from_slice(&e.ok_con(&["remember", "-k", "fact", "-t", "Global", "--global", "--json", "De todos."], &sesion).stdout).unwrap();
    assert_eq!(global["provenance"], serde_json::json!({ "session": "s-1" }), "{global}");
    let gid = global["id"].as_str().unwrap();
    assert_eq!(e.json(&["get", gid])["provenance"], serde_json::json!({ "session": "s-1" }));
    // La del proyecto, entera.
    let suya: serde_json::Value = serde_json::from_slice(&e.ok_con(&["remember", "-k", "fact", "-t", "Del proyecto", "--json", "Mía."], &sesion).stdout).unwrap();
    assert_eq!(suya["provenance"]["branch"], "feature/acme-secret-merger");
    // Ni por MCP, ni en la copia, ni en la web.
    let r = e.herramienta("remember", serde_json::json!({ "kind": "fact", "title": "Global por MCP", "content": "Para todos.", "global": true, "force": true }));
    assert!(e.json(&["get", r["id"].as_str().unwrap()]).get("provenance").is_none_or(|p| p.get("branch").is_none()));
    let salida = e.ok(&["export"]);
    for l in salida.lines().filter(|l| l.contains("Global")) {
        assert!(!l.contains("acme-secret-merger"), "{l}");
    }
    // Promovida a un oficio, ninguna; movida a global, sin commit ni rama.
    let sid = suya["id"].as_str().unwrap();
    e.ok(&["update", sid, "--trade", "android", "-r", "x"]);
    assert!(e.json(&["get", sid]).get("provenance").is_none(), "{}", e.ok(&["get", sid]));
    let otra = e.json(&["remember", "-k", "fact", "-t", "Se mueve", "--force", "Luego global."]);
    let oid = otra["id"].as_str().unwrap();
    e.ok(&["update", oid, "--global", "-r", "x"]);
    assert!(!e.ok(&["export"]).lines().any(|l| l.contains(oid) && l.contains("acme-secret-merger")));
}

/// La revisión pone delante, en cada tipo de aviso, las decisiones y trampas sin verificar, y lo
/// verificado al final.
#[test]
fn la_revision_prioriza_lo_no_verificado() {
    let e = Entorno::nuevo("revision");
    let rota = ["--path", "src/no-existe.rs"];
    let g = e.json(&[&["remember", "-k", "gotcha", "-t", "Trampa verificada", "--force"][..], &rota, &["Una."]].concat())["id"].as_str().unwrap().to_string();
    let f = e.json(&[&["remember", "-k", "fact", "-t", "Hecho sin verificar", "--force"][..], &rota, &["Dos."]].concat())["id"].as_str().unwrap().to_string();
    let d = e.json(&[&["remember", "-k", "decision", "-t", "Decisión sin verificar", "--force"][..], &rota, &["Tres."]].concat())["id"].as_str().unwrap().to_string();
    e.ok(&["verify", &g, "--by", "Marc", "--yes"]);
    let r = e.json(&["review"]);
    let orden: Vec<&str> =
        r.as_array().unwrap_or_else(|| panic!("{r}")).iter().filter(|a| a["kind"] == "broken_path").map(|a| a["memory_id"].as_str().unwrap()).collect();
    assert_eq!(orden, [d.as_str(), f.as_str(), g.as_str()], "{r}");
}

/// La vista web: el botón va por `POST /api/verify`, que solo acepta lo que manda la página de la
/// vista (su `Origin` y el token que va dentro de la página), no un `curl` con un `Origin` inventado.
#[test]
fn la_web_verifica_solo_desde_el_navegador() {
    let e = Entorno::nuevo("web");
    let id = e.guardar("decision", "Desde la web", "Lo verifica la persona en la vista.");
    let puerto = comun::puerto_libre();
    let log = e.dir.join("serve.log");
    let hijo = e.orden().args(["serve", "--port", &puerto.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(&log).unwrap()).spawn().unwrap();
    struct Cerrar(std::process::Child);
    impl Drop for Cerrar {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _servidor = Cerrar(hijo);
    let pedir = |metodo: &str, ruta: &str, cuerpo: &str, cabeceras: &[(&str, &str)]| -> (u16, String) {
        let extra: String = cabeceras.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let peticion = format!(
            "{metodo} {ruta} HTTP/1.1\r\nHost: 127.0.0.1:{puerto}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cuerpo}",
            cuerpo.len()
        );
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", puerto)) {
                s.write_all(peticion.as_bytes()).unwrap();
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                let r = String::from_utf8_lossy(&v).into_owned();
                let estado = r.get(9..12).and_then(|x| x.parse().ok()).unwrap_or(0);
                return (estado, r.find("\r\n\r\n").map(|i| r[i + 4..].to_string()).unwrap_or_default());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó: {}", std::fs::read_to_string(&log).unwrap_or_default());
    };
    let origen = format!("http://127.0.0.1:{puerto}");
    // El token de la vista, de la página (lo que hace el botón).
    let (_, pagina) = pedir("GET", "/", "", &[]);
    let marca = "name=\"omoikane-view\" content=\"";
    let i = pagina.find(marca).expect("la página lleva el token") + marca.len();
    let token = pagina[i..].split('"').next().unwrap().to_string();
    assert!(token.len() >= 32 && !token.contains("OMOIKANE"), "{token}");
    let verificar = |cuerpo: &str, cabeceras: &[(&str, &str)]| pedir("POST", "/api/verify", cuerpo, cabeceras);
    let cuerpo = serde_json::json!({ "id": id, "by": "Marc" }).to_string();
    // Sin Origin (curl, un agente): no.
    assert_eq!(verificar(&cuerpo, &[]).0, 403);
    // De otra web: no.
    assert_eq!(verificar(&cuerpo, &[("Origin", "http://malo.example"), ("X-Omoikane-View", &token)]).0, 403);
    // Un `curl -H "Origin: …"` con el Origin de la vista pero sin su token (o con otro): tampoco.
    assert_eq!(verificar(&cuerpo, &[("Origin", &origen)]).0, 403);
    assert_eq!(verificar(&cuerpo, &[("Origin", &origen), ("X-Omoikane-View", "0123456789abcdef0123456789abcdef")]).0, 403);
    assert!(e.json(&["get", &id]).get("verified").is_none());
    let (estado, r) = verificar(&cuerpo, &[("Origin", &origen), ("X-Omoikane-View", &token)]);
    assert_eq!(estado, 200, "{r}");
    assert_eq!(e.json(&["get", &id])["verified"]["by"], "Marc");
    let (estado, _) = verificar(&serde_json::json!({ "id": id, "unverify": true }).to_string(), &[("Origin", &origen), ("X-Omoikane-View", &token)]);
    assert_eq!(estado, 200);
    assert!(e.json(&["get", &id]).get("verified").is_none());
}
