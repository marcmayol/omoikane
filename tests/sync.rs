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
    assert!(!c.dir.join("sync.json").exists());
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
    c.ok_en(&web_c, &["sync", "join", &codigo, "--yes", "--author", "Ana"], &[]);
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
    assert!(!a.dir.join("sync.lock").exists());
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
fn avisa_si_lleva_tiempo_fallando() {
    let p = Prueba::nueva("aviso");
    let (a, _b) = dos(&p, &[], &[]);
    let hace_dos_horas = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 7200;
    let estado = serde_json::json!({ "ultimo_ok": null, "ultimo_error": "git push: could not read from remote", "error_desde": hace_dos_horas.to_string() });
    std::fs::write(a.dir.join("sync-estado.json"), estado.to_string()).unwrap();
    let o = a.ok_en(&a.dir, &["recall", "--session-start", "--format", "plain"], &[]);
    assert!(o.contains("sync has been failing for 2 h"), "{o}");
    assert!(a.ok(&["doctor"]).contains("FAILING"));
}
