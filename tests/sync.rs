//! `omoi sync` de punta a punta: varios «equipos» (cada uno con su base, su nombre y sus carpetas)
//! que comparten un repositorio git desnudo local. Git sin identidad ni configuración del usuario,
//! para que la prueba no dependa de la máquina.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Prueba {
    dir: PathBuf,
}

impl Prueba {
    fn nueva(nombre: &str) -> Prueba {
        let dir = std::env::temp_dir().join(format!("omoi-sync-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitconfig-vacio"), "").unwrap();
        let p = Prueba { dir };
        let o = p.git(&p.dir, &["init", "--quiet", "--bare", "compartida.git"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        p
    }

    fn url(&self) -> String {
        self.dir.join("compartida.git").to_string_lossy().replace('\\', "/")
    }

    fn git(&self, en: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(en)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    /// Un equipo con su base y una carpeta de proyecto (con el remoto `origin` que se le dé).
    fn equipo(&self, nombre: &str) -> Equipo {
        let dir = self.dir.join(nombre);
        std::fs::create_dir_all(&dir).unwrap();
        Equipo { dir, nombre: nombre.into(), raiz: self.dir.clone() }
    }

    /// Los ficheros del repositorio compartido, en un clon nuevo.
    fn ficheros_compartidos(&self) -> Vec<(String, String)> {
        let clon = self.dir.join(format!("mirar-{}", ulid::Ulid::generate()));
        let o = self.git(&self.dir, &["clone", "--quiet", &self.url(), clon.to_str().unwrap()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let mut v = Vec::new();
        for e in std::fs::read_dir(&clon).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(e.path()).unwrap().flatten() {
                let nombre = format!("{}/{}", e.file_name().to_string_lossy(), f.file_name().to_string_lossy());
                v.push((nombre, std::fs::read_to_string(f.path()).unwrap()));
            }
        }
        v.sort();
        v
    }

    fn commits(&self) -> usize {
        let o = self.git(&self.dir.join("compartida.git"), &["rev-list", "--count", "--all"]);
        String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0)
    }
}

impl Drop for Prueba {
    fn drop(&mut self) {
        // Un `omoi sync --background` aún vivo puede tener ficheros abiertos un momento.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

struct Equipo {
    dir: PathBuf,
    nombre: String,
    raiz: PathBuf,
}

impl Equipo {
    /// Una carpeta de proyecto de este equipo, con un remoto `origin` (o sin git si es `None`).
    fn proyecto(&self, carpeta: &str, remoto: Option<&str>) -> PathBuf {
        let p = self.dir.join(carpeta);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        match remoto {
            Some(r) => std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {r}\n")).unwrap(),
            None => std::fs::remove_dir_all(p.join(".git")).unwrap(),
        }
        p
    }

    fn omoi_en(&self, en: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(en)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_EQUIPO", &self.nombre)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("GIT_CONFIG_GLOBAL", self.raiz.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok_en(&self, en: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let o = self.omoi_en(en, args, env);
        assert!(o.status.success(), "{} omoi {args:?}: {}{}", self.nombre, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_en(&self.dir, args, &[])
    }

    fn json_en(&self, en: &Path, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok_en(en, &a, &[])).unwrap()
    }

    fn guardar(&self, en: &Path, kind: &str, titulo: &str, texto: &str, extra: &[&str]) -> String {
        let mut a = vec!["--json", "remember", "-k", kind, "-t", titulo];
        a.extend_from_slice(extra);
        a.push(texto);
        let v: serde_json::Value = serde_json::from_str(&self.ok_en(en, &a, &[])).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    fn sync(&self) -> serde_json::Value {
        self.json_en(&self.dir, &["sync"])
    }

    /// Una herramienta MCP (`omoi tool <nombre>`) con sus argumentos JSON, desde una carpeta.
    fn tool_en(&self, en: &Path, nombre: &str, args: &str) -> serde_json::Value {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(["tool", nombre])
            .current_dir(en)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_EQUIPO", &self.nombre)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_SOURCE", "mcp:claude-code")
            .env("GIT_CONFIG_GLOBAL", self.raiz.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut hijo = c.spawn().unwrap();
        {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(args.as_bytes()).unwrap();
        }
        let o = hijo.wait_with_output().unwrap();
        assert!(o.status.success(), "{} omoi tool {nombre}: {}{}", self.nombre, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    fn get(&self, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi_en(&self.dir, &["--json", "get", id], &[]);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }
}

/// A configura el repositorio y B se une con el código.
fn dos(p: &Prueba, opciones_a: &[&str], opciones_b: &[&str]) -> (Equipo, Equipo) {
    let a = p.equipo("pc-a");
    let b = p.equipo("pc-b");
    let url = p.url();
    let mut init = vec!["sync", "init", url.as_str(), "--yes"];
    init.extend_from_slice(opciones_a);
    a.ok(&init);
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    let mut join = vec!["sync", "join", codigo.as_str(), "--yes"];
    join.extend_from_slice(opciones_b);
    b.ok(&join);
    (a, b)
}

#[test]
fn llegan_con_el_mismo_id_y_en_la_ruta_de_cada_equipo() {
    let p = Prueba::nueva("llegan");
    let (a, b) = dos(&p, &["--author", "Marc"], &["--author", "Marc"]);
    let pa = a.proyecto("tienda", Some("git@github.com:marc/tienda.git"));
    let pb = b.proyecto("tienda-web", Some("https://github.com/marc/tienda"));
    let id = a.guardar(&pa, "decision", "Los precios van en céntimos", "Todos los importes en enteros de céntimos.", &[]);
    let global = a.guardar(&a.dir, "fact", "El servidor de pruebas es lento", "Tarda unos 10 s en arrancar.", &["--global"]);
    a.sync();
    // B aún no tiene el proyecto: la memoria llega con la ruta de A y pasa a la suya cuando aparece.
    let r = b.json_en(&pb, &["sync"]);
    assert_eq!(r["nuevas"], 2, "{r}");
    let m = b.get(&id).expect("llegó");
    assert!(m["project"].as_str().unwrap().ends_with("tienda-web"), "{m}");
    assert_eq!(m["author"], "Marc");
    assert_eq!(b.get(&global).unwrap()["project"], "");
    // Lo encuentra desde su carpeta del proyecto.
    let s = b.json_en(&pb, &["search", "céntimos"]);
    assert_eq!(s[0]["id"], id.as_str());
}

#[test]
fn gana_el_cambio_mas_nuevo_aunque_su_reloj_vaya_atrasado() {
    let p = Prueba::nueva("reloj");
    let (a, b) = dos(&p, &[], &[]);
    let id = a.guardar(&a.dir, "fact", "Puerto del servicio de pagos", "Escucha en el 8080.", &["--global"]);
    a.sync();
    b.sync();
    // B cambia con el reloj una hora atrasado: aun así su cambio es posterior al que ya conocía.
    b.ok_en(&b.dir, &["update", &id, "-c", "Escucha en el 9090 desde la migración.", "-r", "migración"], &[("OMOIKANE_RELOJ_DESFASE_MS", "-3600000")]);
    b.sync();
    let r = a.sync();
    assert_eq!(r["actualizadas"], 1, "{r}");
    assert!(a.get(&id).unwrap()["content"].as_str().unwrap().contains("9090"));
    let h = a.json_en(&a.dir, &["history", &id]);
    assert!(h[0]["reason"].as_str().unwrap().starts_with("sync: pc-b-"), "{h}");
    // Y nada rebota: otra vuelta en los dos no cambia nada.
    assert_eq!(b.sync()["actualizadas"], 0);
    assert_eq!(a.sync()["actualizadas"], 0);
}

#[test]
fn lo_olvidado_no_resucita_en_ningun_orden() {
    let p = Prueba::nueva("olvido");
    let (a, b) = dos(&p, &[], &[]);
    let id = a.guardar(&a.dir, "gotcha", "El token caduca antes de tiempo", "Compara la hora en UTC.", &["--global"]);
    a.sync();
    b.sync();
    assert!(b.get(&id).is_some());
    // B olvida; A aún no lo sabe y sincroniza antes (sin cambios), luego B sube y A baja.
    b.ok(&["forget", &id, "--yes"]);
    a.sync();
    b.sync();
    let r = a.sync();
    assert_eq!(r["olvidadas"], 1, "{r}");
    assert!(a.get(&id).is_none());
    for _ in 0..2 {
        a.sync();
        b.sync();
    }
    assert!(a.get(&id).is_none() && b.get(&id).is_none());
}

#[test]
fn cifrado_determinista_y_clave_equivocada() {
    let p = Prueba::nueva("cifrado");
    let (a, _b) = dos(&p, &[], &[]);
    a.guardar(&a.dir, "fact", "Contraseña del wifi de la oficina", "Está en el cajón azul.", &["--global"]);
    a.sync();
    let antes = p.ficheros_compartidos();
    let todo: String = antes.iter().map(|(_, t)| t.as_str()).collect();
    assert!(!todo.contains("wifi") && !todo.contains("cajón"), "en el remoto no hay nada en claro");
    assert!(!antes.iter().any(|(f, _)| f.contains("pc-a") || f.contains("pc-b")), "ni el nombre de los equipos: {antes:?}");
    let autores = p.git(&p.dir.join("compartida.git"), &["log", "--all", "--format=%an %s"]);
    let autores = String::from_utf8_lossy(&autores.stdout);
    assert!(autores.lines().all(|l| l == "omoikane omoikane"), "commits sin nombres: {autores}");
    // Otro cambio no toca la línea de la memoria anterior.
    a.guardar(&a.dir, "fact", "La impresora está en la planta 2", "Junto a la máquina de café.", &["--global"]);
    a.sync();
    let despues: String = p.ficheros_compartidos().iter().map(|(_, t)| t.clone()).collect();
    for l in todo.lines() {
        assert!(despues.lines().any(|x| x == l), "la línea que no cambió sigue igual byte a byte");
    }
    // Un código con otra clave no abre el repositorio, y no deja nada configurado.
    let c = p.equipo("pc-c");
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    let malo = otra_clave(&codigo);
    let o = c.omoi_en(&c.dir, &["sync", "join", &malo, "--yes"], &[]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("key"), "{err}");
    assert!(!c.dir.join("o.sync.json").exists());
}

/// El mismo código con otra clave (cambia el primer byte).
fn otra_clave(codigo: &str) -> String {
    use base64::Engine;
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut v: serde_json::Value = serde_json::from_slice(&b.decode(codigo.strip_prefix("omoi1.").unwrap()).unwrap()).unwrap();
    let k = v["key"].as_str().unwrap().to_string();
    let primero = if &k[..2] == "00" { "01" } else { "00" };
    v["key"] = serde_json::Value::String(format!("{primero}{}", &k[2..]));
    format!("omoi1.{}", b.encode(v.to_string()))
}

#[test]
fn solo_sube_lo_elegido() {
    let p = Prueba::nueva("eleccion");
    let a = p.equipo("pc-a");
    let compartido = a.proyecto("tienda", Some("git@github.com:marc/tienda.git"));
    let privado = a.proyecto("diario", Some("git@github.com:marc/diario.git"));
    // Hace falta que existan antes del init para elegirlos por carpeta.
    a.ok(&["sync", "init", &p.url(), "--yes", "--no-global", "--project", compartido.to_str().unwrap()]);
    let si = a.guardar(&compartido, "decision", "Pagos con Stripe", "Elegido por las comisiones.", &[]);
    let pref = a.guardar(&compartido, "preference", "Respuestas en español", "Siempre en español.", &[]);
    let no = a.guardar(&privado, "fact", "Notas del diario", "Nada que compartir.", &[]);
    let glob = a.guardar(&a.dir, "fact", "Mi portátil se llama tostadora", "Por el ruido.", &["--global"]);
    a.sync();
    let b = p.equipo("pc-b");
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    b.ok(&["sync", "join", &codigo, "--yes"]);
    assert!(b.get(&si).is_some());
    for id in [&pref, &no, &glob] {
        assert!(b.get(id).is_none(), "{id} no se comparte");
    }
}

#[test]
fn autor_si_y_no() {
    let p = Prueba::nueva("autor");
    let (a, b) = dos(&p, &["--author", "Marc Mayol"], &["--no-author"]);
    let de_a = a.guardar(&a.dir, "fact", "La base de pruebas se borra cada noche", "A las 3:00.", &["--global"]);
    let de_b = b.guardar(&b.dir, "fact", "El CI tarda veinte minutos", "Por los tests de macOS.", &["--global"]);
    a.sync();
    b.sync();
    a.sync();
    assert_eq!(b.get(&de_a).unwrap()["author"], "Marc Mayol");
    assert!(a.get(&de_b).unwrap().get("author").is_none(), "B no apunta autor");
}

#[test]
fn proyectos_sin_remoto_se_juntan_por_persona() {
    let p = Prueba::nueva("persona");
    let a = p.equipo("pc-a");
    a.ok(&["sync", "init", &p.url(), "--yes", "--author", "Marc"]);
    let web_a = a.proyecto("web", None);
    let id = a.guardar(&web_a, "fact", "La web usa Astro", "Astro 5.", &[]);
    a.sync();
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    // Otro equipo de Marc con una carpeta `web`: es el mismo proyecto.
    let b = p.equipo("pc-b");
    let web_b = b.proyecto("web", None);
    b.ok_en(&web_b, &["sync", "join", &codigo, "--yes", "--author", "Marc"], &[]);
    let en_b = b.get(&id).unwrap();
    assert!(en_b["project"].as_str().unwrap().ends_with("web") && en_b["project"].as_str().unwrap().contains("pc-b"), "{en_b}");
    // Ana, con su propia carpeta `web`: no es el suyo.
    let c = p.equipo("pc-ana");
    let web_c = c.proyecto("web", None);
    c.ok_en(&web_c, &["sync", "join", &codigo, "--yes", "--author", "Ana", "--another-person"], &[]);
    let en_c = c.get(&id).unwrap();
    assert!(!en_c["project"].as_str().unwrap().contains("pc-ana"), "{en_c}");
}

#[test]
fn dos_sincronizaciones_a_la_vez() {
    let p = Prueba::nueva("cerrojo");
    let (a, _b) = dos(&p, &[], &[]);
    a.guardar(&a.dir, "fact", "Algo que subir", "Para que haya commit.", &["--global"]);
    let lanzar = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(["sync"])
            .current_dir(&a.dir)
            .env("OMOIKANE_DB", a.dir.join("o.db"))
            .env("OMOIKANE_MODELS", a.dir.join("models"))
            .env("OMOIKANE_EQUIPO", &a.nombre)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c.spawn().unwrap()
    };
    let (h1, h2) = (lanzar(), lanzar());
    for h in [h1, h2] {
        let o = h.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    assert!(!a.dir.join("o.sync.lock").exists());
}

#[test]
fn en_segundo_plano_tras_guardar() {
    let p = Prueba::nueva("fondo");
    let (a, _b) = dos(&p, &[], &[]);
    let antes = p.commits();
    // Con la sincronización automática encendida y una espera corta.
    a.ok_en(
        &a.dir,
        &["remember", "-k", "fact", "-t", "Se sube sola", "--global", "Sin esperar a nadie."],
        &[("OMOIKANE_SYNC_AUTO", "1"), ("OMOIKANE_SYNC_ESPERA_MS", "200")],
    );
    let inicio = std::time::Instant::now();
    while p.commits() == antes && inicio.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(p.commits() > antes, "la sincronización en segundo plano subió el cambio");
    let estado = a.ok(&["sync", "status"]);
    assert!(!estado.contains("FAILING"), "{estado}");
}

#[test]
fn unirse_por_otra_url() {
    let p = Prueba::nueva("otraurl");
    let a = p.equipo("pc-a");
    let b = p.equipo("pc-b");
    a.ok(&["sync", "init", &p.url(), "--yes"]);
    let id = a.guardar(&a.dir, "fact", "El servidor de pruebas reinicia los lunes", "A las 6:00.", &["--global"]);
    a.sync();
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    let otra = format!("file:///{}", p.url().trim_start_matches('/'));
    b.ok(&["sync", "join", &codigo, "--url", &otra, "--yes"]);
    assert!(b.get(&id).is_some(), "B llega por la otra URL");
    assert!(b.ok(&["sync", "status"]).contains("file:///"), "y la guarda");
}

#[test]
fn lo_de_otra_persona_va_marcado() {
    let p = Prueba::nueva("marcado");
    let (a, b) = dos(&p, &["--author", "Ana"], &["--author", "Marc"]);
    a.guardar(&a.dir, "gotcha", "Las facturas redondean por línea", "El IVA se redondea en cada línea.", &["--global", "--path", "facturas.rs"]);
    a.sync();
    b.sync();
    let pregunta = [("USER_PROMPT", "revisa facturas.rs, el total no cuadra")];
    let en_b = b.ok_en(&b.dir, &["recall", "--format", "plain"], &pregunta);
    assert!(en_b.contains("[by Ana]"), "{en_b}");
    let en_a = a.ok_en(&a.dir, &["recall", "--format", "plain"], &pregunta);
    assert!(en_a.contains("Las facturas") && !en_a.contains("[by "), "lo propio no se marca: {en_a}");
}

#[test]
fn avisa_si_lleva_tiempo_fallando() {
    let p = Prueba::nueva("aviso");
    let (a, _b) = dos(&p, &[], &[]);
    let hace_dos_horas = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 7200;
    let estado = serde_json::json!({ "ultimo_ok": null, "ultimo_error": "git push: could not read from remote", "error_desde": hace_dos_horas.to_string() });
    std::fs::write(a.dir.join("o.sync-estado.json"), estado.to_string()).unwrap();
    let o = a.ok_en(&a.dir, &["recall", "--session-start", "--format", "plain"], &[]);
    assert!(o.contains("sync has been failing for 2 h"), "{o}");
    assert!(a.ok(&["doctor"]).contains("FAILING"));
}

impl Prueba {
    /// Sube al repositorio compartido un fichero escrito a mano (lo que haría otro programa o un equipo
    /// con otra clave).
    fn subir_a_mano(&self, ruta: &str, texto: &str) {
        let clon = self.dir.join(format!("mano-{}", ulid::Ulid::generate()));
        let o = self.git(&self.dir, &["clone", "--quiet", "--branch", "main", &self.url(), clon.to_str().unwrap()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let f = clon.join(ruta);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, texto).unwrap();
        for args in [vec!["add", "-A"], vec!["-c", "user.name=x", "-c", "user.email=x@x", "commit", "--quiet", "-m", "a mano"], vec!["push", "--quiet", "origin", "HEAD:main"]] {
            let o = self.git(&clon, &args);
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
}

#[test]
fn init_en_un_repositorio_ya_usado_pide_join() {
    let p = Prueba::nueva("yausado");
    let a = p.equipo("pc-a");
    a.ok(&["sync", "init", &p.url(), "--yes"]);
    // Aunque A aún no tenga memorias, el repositorio ya lleva su comprobación de la clave.
    a.sync();
    let c = p.equipo("pc-c");
    let o = c.omoi_en(&c.dir, &["sync", "init", &p.url(), "--yes"], &[]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("join"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!c.dir.join("o.sync.json").exists());
}

#[test]
fn una_linea_ilegible_se_salta_sin_parar() {
    let p = Prueba::nueva("ilegible");
    let (a, b) = dos(&p, &[], &[]);
    p.subir_a_mano("global/intruso.jsonl", "esto-no-es-una-linea-cifrada\n");
    let id = b.guardar(&b.dir, "fact", "El backup corre a las cuatro", "Cada noche.", &["--global"]);
    b.sync();
    let r = a.sync();
    assert_eq!(r["ilegibles"], 1, "{r}");
    assert!(a.get(&id).is_some(), "lo demás llega igual");
    assert!(a.ok(&["sync", "status"]).contains("couldn't be read"));
    // Y A sigue subiendo lo suyo.
    let de_a = a.guardar(&a.dir, "fact", "La VPN cambia de puerto", "Ahora el 51820.", &["--global"]);
    a.sync();
    b.sync();
    assert!(b.get(&de_a).is_some());
}

#[test]
fn un_clon_a_medias_no_atasca() {
    let p = Prueba::nueva("amedias");
    let (a, b) = dos(&p, &[], &[]);
    a.guardar(&a.dir, "fact", "Primero", "Algo.", &["--global"]);
    a.sync();
    // Como si un proceso hubiera muerto entre `add` y `commit`: el índice del clon con cambios.
    let clon = a.dir.join("o.sync");
    std::fs::write(clon.join("global").join("basura.jsonl"), "x\n").unwrap();
    assert!(p.git(&clon, &["add", "-A"]).status.success());
    let id = b.guardar(&b.dir, "fact", "Segundo", "Desde B.", &["--global"]);
    b.sync();
    a.sync();
    assert!(a.get(&id).is_some(), "A sigue sincronizando");
    assert!(!p.ficheros_compartidos().iter().any(|(f, _)| f.contains("basura")), "lo que quedó a medias no se sube");
}

#[test]
fn lo_que_olvida_otro_se_puede_recuperar() {
    let p = Prueba::nueva("recuperar");
    let (a, b) = dos(&p, &["--author", "Ana"], &["--author", "Marc"]);
    let id = b.guardar(&b.dir, "decision", "Los precios van en céntimos", "Para no redondear.", &["--global"]);
    b.ok(&["update", &id, "-c", "Todo en céntimos, IVA al final.", "-r", "aclarar"]);
    b.sync();
    a.sync();
    a.ok(&["forget", &id, "--yes"]);
    a.sync();
    b.sync();
    assert!(b.get(&id).is_none(), "el olvido de A llega a B");
    let lista = b.ok(&["sync", "restore"]);
    assert!(lista.contains("Los precios van en céntimos"), "{lista}");
    b.ok(&["sync", "restore", &id]);
    let m = b.get(&id).unwrap();
    assert_eq!(m["content"], "Todo en céntimos, IVA al final.");
    let h = b.json_en(&b.dir, &["history", &id]);
    assert_eq!(h.as_array().unwrap().len(), 1, "vuelve con su historial: {h}");
    // Y vuelve también a A.
    b.sync();
    a.sync();
    assert!(a.get(&id).is_some(), "la recuperada gana al olvido");
}

#[test]
fn lo_que_aqui_no_se_comparte_tampoco_se_acepta() {
    let p = Prueba::nueva("filtro");
    let (a, b) = dos(&p, &["--share-preferences"], &[]);
    let pref = a.guardar(&a.dir, "preference", "Respuestas cortas", "Sin rodeos.", &["--global"]);
    let hecho = a.guardar(&a.dir, "fact", "El servidor se llama tierra", "En el armario.", &["--global"]);
    a.sync();
    b.sync();
    assert!(b.get(&hecho).is_some());
    assert!(b.get(&pref).is_none(), "B no comparte preferencias: las de A no entran");
}

#[test]
fn el_codigo_por_la_entrada_estandar() {
    let p = Prueba::nueva("entrada");
    let a = p.equipo("pc-a");
    a.ok(&["sync", "init", &p.url(), "--yes"]);
    let id = a.guardar(&a.dir, "fact", "El NAS duerme a las dos", "Y despierta a las siete.", &["--global"]);
    a.sync();
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    let b = p.equipo("pc-b");
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(["sync", "join", "-", "--yes"])
        .current_dir(&b.dir)
        .env("OMOIKANE_DB", b.dir.join("o.db"))
        .env("OMOIKANE_MODELS", b.dir.join("models"))
        .env("OMOIKANE_EQUIPO", &b.nombre)
        .env("OMOIKANE_SYNC_AUTO", "0")
        .env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut h = c.spawn().unwrap();
    {
        use std::io::Write;
        h.stdin.take().unwrap().write_all(format!("{codigo}\n").as_bytes()).unwrap();
    }
    let o = h.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(b.get(&id).is_some());
    // Sin terminal y sin --yes, no se une a ciegas.
    let c2 = p.equipo("pc-c");
    let o = c2.omoi_en(&c2.dir, &["sync", "join", &codigo], &[]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--yes"));
}

#[test]
fn un_cerrojo_de_un_proceso_muerto_no_bloquea() {
    let p = Prueba::nueva("muerto");
    let (a, _b) = dos(&p, &[], &[]);
    let mut h = Command::new("git").arg("--version").stdout(Stdio::null()).spawn().unwrap();
    let pid = h.id();
    h.wait().unwrap();
    let ahora = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(a.dir.join("o.sync.lock"), format!("{pid}\n{ahora}\nficha\n")).unwrap();
    let inicio = std::time::Instant::now();
    a.sync();
    assert!(inicio.elapsed() < std::time::Duration::from_secs(30), "no espera al cerrojo de un proceso que ya no existe");
    assert!(!a.dir.join("o.sync.lock").exists());
}

#[test]
fn ficheros_con_el_nombre_de_la_base() {
    let p = Prueba::nueva("nombres");
    let (a, _b) = dos(&p, &[], &[]);
    for f in ["o.sync.json", "o.sync.key", "o.sync"] {
        assert!(a.dir.join(f).exists(), "{f}");
    }
    assert!(!a.dir.join("sync.json").exists() && !a.dir.join("sync").exists());
}

/// La clave de un código de `omoi sync code`.
fn clave_del_codigo(codigo: &str) -> Vec<u8> {
    use base64::Engine;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(codigo.trim().strip_prefix("omoi1.").unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let h = v["key"].as_str().unwrap();
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

/// Una línea cifrada como las de Omoikane, con lo que se quiera dentro.
fn cifrar_linea(clave: &[u8], json: &serde_json::Value) -> String {
    use aes_siv::KeyInit;
    use base64::Engine;
    let c = aes_siv::siv::Aes256Siv::new_from_slice(clave).unwrap().encrypt([b"omoikane-sync-v1".as_slice()], json.to_string().as_bytes()).unwrap();
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(c)
}

#[test]
fn una_linea_no_valida_se_salta_y_el_aviso_dura_hasta_que_se_arregla() {
    let p = Prueba::nueva("novalida");
    let (a, b) = dos(&p, &[], &[]);
    let clave = clave_del_codigo(&a.ok(&["sync", "code"]));
    let memoria = |id: &str, titulo: &str| {
        serde_json::json!({
            "type": "memory", "id": id, "project_key": "", "project_path": "", "kind": "fact",
            "title": titulo, "content": "algo", "created_at": "2026-10-29T00:00:00.000Z",
            "updated_at": "2026-10-29T00:00:00.000Z", "hlc": "1761696000000-0000000000-x"
        })
    };
    let malas = [cifrar_linea(&clave, &memoria("01M62EWVHRXM0000000000ABCD", "")), cifrar_linea(&clave, &memoria("AB", "Id corto"))];
    p.subir_a_mano("global/intruso.jsonl", &(malas.join("\n") + "\n"));
    let id = b.guardar(&b.dir, "fact", "El backup corre a las cuatro", "Cada noche.", &["--global"]);
    b.sync();
    let r = a.sync();
    assert_eq!(r["ilegibles"], 2, "{r}");
    assert!(a.get(&id).is_some(), "lo demás llega igual");
    // La siguiente no vuelve a leer ese fichero (el punto de lectura avanzó), pero el aviso sigue.
    let r = a.sync();
    assert_eq!(r["ilegibles"], 0, "{r}");
    assert!(a.ok(&["sync", "status"]).contains("couldn't be read"));
    // Cuando el fichero se arregla y se vuelve a leer, el aviso se va.
    p.subir_a_mano("global/intruso.jsonl", "");
    a.sync();
    assert!(!a.ok(&["sync", "status"]).contains("couldn't be read"), "{}", a.ok(&["sync", "status"]));
}

#[test]
fn dos_proyectos_desconocidos_con_la_misma_ruta_no_se_mezclan() {
    let p = Prueba::nueva("misma-ruta");
    let a = p.equipo("pc-a");
    let c = p.equipo("pc-c");
    a.ok(&["sync", "init", &p.url(), "--yes"]);
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    c.ok(&["sync", "join", &codigo, "--yes"]);
    // La misma carpeta (como `/workspace` en dos contenedores) con dos repositorios distintos.
    let ws = p.dir.join("workspace");
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    let remoto = |r: &str| std::fs::write(ws.join(".git").join("config"), format!("[remote \"origin\"]\n\turl = {r}\n")).unwrap();
    remoto("git@github.com:a/uno.git");
    let de_uno = a.guardar(&ws, "fact", "Uno usa Postgres", "Postgres 16.", &[]);
    a.sync();
    remoto("git@github.com:a/dos.git");
    let de_dos = c.guardar(&ws, "fact", "Dos usa SQLite", "SQLite 3.46.", &[]);
    c.sync();
    // B no tiene esa carpeta: las dos claves llegan con la misma ruta de origen.
    std::fs::remove_dir_all(&ws).unwrap();
    let b = p.equipo("pc-b");
    b.ok(&["sync", "join", &codigo, "--yes"]);
    let (pu, pd) = (b.get(&de_uno).unwrap()["project"].clone(), b.get(&de_dos).unwrap()["project"].clone());
    assert_ne!(pu, pd, "cada repositorio en su proyecto");
    // Y al volver a sincronizar (B sube las dos con su clave), siguen separadas.
    b.sync();
    b.sync();
    assert_ne!(b.get(&de_uno).unwrap()["project"], b.get(&de_dos).unwrap()["project"]);
}

#[test]
fn los_equipos_de_una_persona_sin_nombre_juntan_sus_proyectos_sin_remoto() {
    let p = Prueba::nueva("sin-nombre");
    // Sin autor y sin `git user.name` (la configuración global de git de la prueba está vacía).
    let (a, b) = dos(&p, &["--no-author"], &["--no-author"]);
    let web_a = a.proyecto("web", None);
    let id = a.guardar(&web_a, "fact", "La web usa Astro", "Astro 5.", &[]);
    a.sync();
    let web_b = b.proyecto("web", None);
    b.ok_en(&web_b, &["sync"], &[]);
    let en_b = b.get(&id).unwrap();
    assert_eq!(en_b["project"].as_str().unwrap(), web_b.canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\"), "{en_b}");
}

/// 0.17.0: un «está bien así» (un veredicto) que se da en un equipo vale en los demás.
#[test]
fn los_veredictos_viajan() {
    let p = Prueba::nueva("veredictos");
    let (a, b) = dos(&p, &[], &[]);
    let pa = a.proyecto("lumbre", Some("git@github.com:marc/lumbre.git"));
    let pb = b.proyecto("lumbre", Some("git@github.com:marc/lumbre.git"));
    let vieja = a.guardar(&pa, "fact", "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let nueva = a.guardar(&pa, "fact", "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["--tag", "status"]);
    a.sync();
    b.json_en(&pb, &["sync"]);
    let desfasadas = |e: &Equipo, en: &Path| -> usize {
        let v = e.json_en(en, &["review", "--all"]);
        v.as_array().unwrap().iter().filter(|x| x["kind"] == "outdated").count()
    };
    assert_eq!(desfasadas(&b, &pb), 1);
    a.ok_en(&pa, &["review", "--dismiss", &vieja, "--other", &nueva], &[]);
    assert_eq!(desfasadas(&a, &pa), 0);
    a.sync();
    b.json_en(&pb, &["sync"]);
    assert_eq!(desfasadas(&b, &pb), 0);
}

/// 0.19.0: el motivo y el autor del veredicto llegan al otro equipo, y el final de una cadena también
/// se ve allí.
#[test]
fn los_veredictos_viajan_con_motivo_y_autor() {
    let p = Prueba::nueva("motivo");
    let (a, b) = dos(&p, &["--author", "Marc"], &["--author", "Ana"]);
    let pa = a.proyecto("tienda", Some("git@github.com:marc/tienda.git"));
    let pb = b.proyecto("tienda", Some("git@github.com:marc/tienda.git"));
    let cookie = a.guardar(&pa, "decision", "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[]);
    let jwt = a.guardar(&pa, "decision", "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.", &[]);
    // Con la persona de la sync, el autor es ella, no el agente.
    a.tool_en(&pa, "update", &format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}"));
    a.sync();
    b.json_en(&pb, &["sync"]);
    let rev = b.json_en(&pb, &["review", "--all"]);
    let av = rev.as_array().unwrap().iter().find(|x| x["kind"] == "superseded").expect("llega el veredicto");
    assert_eq!((av["memory_id"].as_str(), av["other_id"].as_str()), (Some(cookie.as_str()), Some(jwt.as_str())));
    assert_eq!((av["reason"].as_str(), av["author"].as_str()), (Some("JWT replaced the cookie"), Some("Marc")), "{av}");
    assert_eq!(av["detail"], "replaced by the other one: JWT replaced the cookie · by Marc");
    let s = b.tool_en(&pb, "search", "{\"query\":\"cookie de sesión\"}");
    let nota = s.as_array().unwrap().iter().find(|x| x["id"] == cookie.as_str()).unwrap()["note"].as_str().unwrap_or("").to_string();
    assert!(nota.starts_with("superseded by") && nota.ends_with(": JWT replaced the cookie"), "{nota}");
    // B añade un eslabón (JWT → PASETO) con `remember`: el motivo es el título de la nueva, y en A la
    // nota de la cookie salta hasta el final de la cadena.
    let r = b.tool_en(&pb, "remember", &format!("{{\"kind\":\"decision\",\"title\":\"La autenticación va con PASETO\",\"content\":\"PASETO v4 en vez de JWT, por las firmas.\",\"supersedes\":[\"{jwt}\"],\"force\":true}}"));
    let paseto = r["id"].as_str().unwrap().to_string();
    b.json_en(&pb, &["sync"]);
    a.json_en(&pa, &["sync"]);
    let rev = a.json_en(&pa, &["review", "--all"]);
    let de_cookie = rev.as_array().unwrap().iter().find(|x| x["kind"] == "superseded" && x["memory_id"] == cookie.as_str()).unwrap();
    assert_eq!((de_cookie["other_id"].as_str(), de_cookie["via"].as_u64(), de_cookie["author"].as_str(), de_cookie["reason"].as_str()), (Some(paseto.as_str()), Some(2), Some("Ana"), Some("La autenticación va con PASETO")), "{de_cookie}");
    let s = a.tool_en(&pa, "search", "{\"query\":\"cookie de sesión\"}");
    let nota = s.as_array().unwrap().iter().find(|x| x["id"] == cookie.as_str()).unwrap()["note"].as_str().unwrap_or("").to_string();
    assert_eq!(nota, format!("superseded by {} «La autenticación va con PASETO» (via 2): La autenticación va con PASETO", &paseto[..12]));
}

/// 0.18.0: lo olvidado aquí está en la papelera de aquí, y al devolverlo vuelve también a los demás.
#[test]
fn lo_olvidado_aqui_se_recupera_y_vuelve_a_los_demas() {
    let p = Prueba::nueva("papelera");
    let (a, b) = dos(&p, &[], &[]);
    let id = a.guardar(&a.dir, "fact", "El puerto del servicio de pagos", "Escucha en el 8080.", &["--global"]);
    a.sync();
    b.sync();
    a.ok(&["forget", &id, "--yes"]);
    a.sync();
    b.sync();
    assert!(a.get(&id).is_none() && b.get(&id).is_none());
    let papelera = a.ok(&["trash"]);
    assert!(papelera.contains("pagos") && papelera.contains("pc-a"), "{papelera}");
    a.ok(&["restore", &id]);
    assert!(a.get(&id).is_some());
    a.sync();
    b.sync();
    assert!(b.get(&id).is_some(), "la recuperada gana al olvido también en B");
}

/// 0.19.0, segunda capa: lo que se guardó con un secreto (antes de la 0.19.0, o con
/// `OMOIKANE_REDACT=0`) sale redactado hacia los otros equipos, y en este se queda como está.
#[test]
fn los_secretos_no_salen_del_equipo() {
    let p = Prueba::nueva("secretos");
    let (a, b) = dos(&p, &[], &[]);
    let texto = "export GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let salida = a.ok_en(&a.dir, &["--json", "remember", "-k", "fact", "-t", "Token de CI", "--global", texto], &[("OMOIKANE_REDACT", "0")]);
    let id = serde_json::from_str::<serde_json::Value>(&salida).unwrap()["id"].as_str().unwrap().to_string();
    a.sync();
    b.sync();
    assert_eq!(b.get(&id).expect("llegó")["content"], "export GH_TOKEN=[redacted:github-token]");
    assert_eq!(a.get(&id).unwrap()["content"], texto, "en su equipo se queda (con su aviso en Revisar)");
    for (_, f) in p.ficheros_compartidos() {
        assert!(!f.contains("ghp_"), "{f}");
    }
}
