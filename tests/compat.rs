//! Compatibilidad con la 1.0.0 (Fase 9, §9.1): un `omoi` 1.0.0 fijo y el de este commit comparten un
//! repositorio git desnudo local, una base y las copias de `omoi export`, y se tienen que entender en
//! los dos sentidos. Es lo que promete la 1.0 (PLAN.md, «### 1.0.0»): la base, la línea de sync y el
//! export no cambian de forma incompatible en la 1.x; una 1.0 guarda y reenvía lo que no entiende
//! (`extra`), cuenta aparte las líneas de un tipo nuevo, se aparta de un repositorio que pide un lector
//! más nuevo (`omoikane.format`) y abre una base que solo añade (`lector_minimo`).
//!
//! El 1.0.0 va en `OMOIKANE_COMPAT_BIN` (la ruta a su `omoi`); sin ella, cada test se salta con un
//! aviso. La CI (`.github/workflows/compat.yml`) lo baja de la Release v1.0.0, comprueba su SHA-256 y
//! corre `cargo test --test compat` en Windows, Linux y macOS.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// El `omoi` de este commit.
const NUEVO: &str = env!("CARGO_BIN_EXE_omoi");

/// El `omoi` 1.0.0 de `OMOIKANE_COMPAT_BIN`, o `None` (y un aviso) si no está. Si está, tiene que ser
/// la 1.0.0: con otro binario la prueba no diría nada.
fn binario_10() -> Option<PathBuf> {
    let Some(r) = std::env::var_os("OMOIKANE_COMPAT_BIN").filter(|r| !r.is_empty()) else {
        eprintln!("compat: falta OMOIKANE_COMPAT_BIN (la ruta a un omoi 1.0.0); se salta");
        return None;
    };
    let r = PathBuf::from(r);
    let o = Command::new(&r).arg("--version").output().unwrap_or_else(|e| panic!("OMOIKANE_COMPAT_BIN={}: {e}", r.display()));
    let v = String::from_utf8_lossy(&o.stdout);
    assert_eq!(v.trim(), "omoi 1.0.0", "OMOIKANE_COMPAT_BIN={} no es la 1.0.0", r.display());
    Some(r)
}

struct Prueba {
    dir: PathBuf,
}

impl Prueba {
    fn nueva(nombre: &str) -> Prueba {
        let dir = std::env::temp_dir().join(format!("omoi-compat-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitconfig-vacio"), "").unwrap();
        let p = Prueba { dir };
        p.otro_repo("compartida");
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

    /// Un repositorio desnudo (el general o el de un espacio). Devuelve su URL.
    fn otro_repo(&self, nombre: &str) -> String {
        let o = self.git(&self.dir, &["init", "--quiet", "--bare", &format!("{nombre}.git")]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        self.dir.join(format!("{nombre}.git")).to_string_lossy().replace('\\', "/")
    }

    fn commits(&self) -> String {
        String::from_utf8_lossy(&self.git(&self.dir.join("compartida.git"), &["rev-list", "--count", "--all"]).stdout).trim().to_string()
    }

    /// Un equipo con su base, con el `omoi` que se le dé y `OMOIKANE_EQUIPO` = `nombre`.
    fn equipo(&self, nombre: &str, bin: &Path) -> Equipo {
        self.equipo_en(nombre, nombre, bin)
    }

    /// Lo mismo, con la carpeta (y la base) aparte del nombre: dos binarios sobre la misma base, o
    /// dos bases con el mismo nombre de equipo.
    fn equipo_en(&self, carpeta: &str, nombre: &str, bin: &Path) -> Equipo {
        let dir = self.dir.join(carpeta);
        std::fs::create_dir_all(&dir).unwrap();
        Equipo { dir, nombre: nombre.into(), raiz: self.dir.clone(), bin: bin.to_path_buf() }
    }

    /// Un clon nuevo de un repositorio, para mirar.
    fn clon_de(&self, url: &str) -> PathBuf {
        let clon = self.dir.join(format!("mirar-{}", ulid::Ulid::generate()));
        let o = self.git(&self.dir, &["clone", "--quiet", "--branch", "main", url, clon.to_str().unwrap()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        clon
    }

    /// Las líneas de los ficheros de memorias de un repositorio.
    fn lineas_de(&self, url: &str) -> Vec<String> {
        let clon = self.clon_de(url);
        let mut v = Vec::new();
        for e in std::fs::read_dir(&clon).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(e.path()).unwrap().flatten() {
                v.extend(std::fs::read_to_string(f.path()).unwrap().lines().filter(|l| !l.is_empty()).map(String::from));
            }
        }
        v
    }

    /// Sube al repositorio general un fichero escrito a mano (lo que escribiría una 1.x más nueva).
    fn subir_a_mano(&self, ruta: &str, texto: &str) {
        let clon = self.clon_de(&self.url());
        let f = clon.join(ruta);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, texto).unwrap();
        for args in [vec!["add", "-A"], vec!["-c", "user.name=x", "-c", "user.email=x@x", "commit", "--quiet", "-m", "a mano"], vec!["push", "--quiet", "origin", "HEAD:main"]] {
            let o = self.git(&clon, &args);
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
}

impl Drop for Prueba {
    fn drop(&mut self) {
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
    bin: PathBuf,
}

impl Equipo {
    fn es_el_commit(&self) -> bool {
        self.bin == Path::new(NUEVO)
    }

    /// Quién es, para los mensajes: el nombre y si es la 1.0.0 o el commit.
    fn quien(&self) -> String {
        format!("{} ({})", self.nombre, if self.es_el_commit() { "commit" } else { "1.0.0" })
    }

    /// Una carpeta de proyecto con un remoto `origin`.
    fn proyecto(&self, carpeta: &str, remoto: &str) -> PathBuf {
        let p = self.dir.join(carpeta);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remoto}\n")).unwrap();
        p
    }

    /// `omoi` de este equipo, sin nada de la máquina: ni actualizarse solo, ni copias diarias, ni
    /// sincronizar en segundo plano.
    fn comando(&self, en: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.current_dir(en)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_EQUIPO", &self.nombre)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("GIT_CONFIG_GLOBAL", self.raiz.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_SOURCE");
        c
    }

    fn omoi_en(&self, en: &Path, args: &[&str]) -> Output {
        self.comando(en).args(args).stdin(Stdio::null()).output().unwrap()
    }

    fn ok_en(&self, en: &Path, args: &[&str]) -> String {
        let o = self.omoi_en(en, args);
        assert!(o.status.success(), "{} omoi {args:?}: {}{}", self.quien(), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_en(&self.dir, args)
    }

    fn json_en(&self, en: &Path, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok_en(en, &a)).unwrap()
    }

    fn guardar(&self, en: &Path, kind: &str, titulo: &str, texto: &str, extra: &[&str]) -> String {
        let mut a = vec!["remember", "-k", kind, "-t", titulo];
        a.extend_from_slice(extra);
        a.push(texto);
        self.json_en(en, &a)["id"].as_str().unwrap().to_string()
    }

    /// `omoi sync`; entre las dos versiones no puede quedar nada ilegible.
    fn sync(&self) -> serde_json::Value {
        self.sync_en(&self.dir)
    }

    /// Lo mismo desde una carpeta (la de un proyecto: así este equipo la conoce).
    fn sync_en(&self, en: &Path) -> serde_json::Value {
        let r = self.json_en(en, &["sync"]);
        assert_eq!(r["ilegibles"].as_u64(), Some(0), "{}: nada ilegible entre versiones: {r}", self.quien());
        r
    }

    /// Una herramienta MCP (`omoi tool <nombre>`) con sus argumentos JSON.
    fn tool_en(&self, en: &Path, nombre: &str, args: &str) -> serde_json::Value {
        let mut hijo = self
            .comando(en)
            .args(["tool", nombre])
            .env("OMOIKANE_SOURCE", "mcp:claude-code")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(args.as_bytes()).unwrap();
        }
        let o = hijo.wait_with_output().unwrap();
        assert!(o.status.success(), "{} omoi tool {nombre}: {}{}", self.quien(), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    fn get(&self, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi_en(&self.dir, &["--json", "get", id]);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    fn base(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }
}

/// A configura el repositorio general y B se une con el código, cada uno con su persona.
fn dos(p: &Prueba, bin_a: &Path, bin_b: &Path) -> (Equipo, Equipo) {
    let a = p.equipo("pc-a", bin_a);
    let b = p.equipo("pc-b", bin_b);
    a.ok(&["sync", "init", &p.url(), "--yes", "--author", "Ana"]);
    let codigo = a.ok(&["sync", "code"]).trim().to_string();
    b.ok(&["sync", "join", &codigo, "--yes", "--author", "Marc"]);
    (a, b)
}

/// La persona de la sync de cada equipo de `dos`.
fn persona(e: &Equipo) -> &'static str {
    if e.nombre == "pc-a" { "Ana" } else { "Marc" }
}

// --- Cifrado, como lo hace Omoikane ------------------------------------------------------------------

/// La clave de un código de `omoi sync code`.
fn clave_del_codigo(codigo: &str) -> Vec<u8> {
    use base64::Engine;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(codigo.trim().strip_prefix("omoi1.").unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let h = v["key"].as_str().unwrap();
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

/// Una línea cifrada con la clave 1 (sin prefijo), con lo que se quiera dentro.
fn cifrar_linea(clave: &[u8], json: &serde_json::Value) -> String {
    use aes_siv::KeyInit;
    use base64::Engine;
    let c = aes_siv::siv::Aes256Siv::new_from_slice(clave).unwrap().encrypt([b"omoikane-sync-v1".as_slice()], json.to_string().as_bytes()).unwrap();
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(c)
}

/// Lo que lleva dentro una línea cifrada con la clave 1, o `None` si no es de esa clave (otra
/// etiqueta, otra clave).
fn descifrar_linea(clave: &[u8], linea: &str) -> Option<serde_json::Value> {
    use aes_siv::KeyInit;
    use base64::Engine;
    let c = base64::engine::general_purpose::STANDARD_NO_PAD.decode(linea.trim()).ok()?;
    let t = aes_siv::siv::Aes256Siv::new_from_slice(clave).unwrap().decrypt([b"omoikane-sync-v1".as_slice()], &c).ok()?;
    serde_json::from_slice(&t).ok()
}

/// El `min_reader` de `omoikane.format` del repositorio general (1 si aún no está).
fn min_reader(p: &Prueba, clave: &[u8]) -> u64 {
    std::fs::read_to_string(p.clon_de(&p.url()).join("omoikane.format"))
        .ok()
        .and_then(|t| descifrar_linea(clave, &t))
        .and_then(|v| v["min_reader"].as_u64())
        .unwrap_or(1)
}

/// Si el commit ha subido el `min_reader` del repositorio, la 1.0 no puede escribir en él (y es lo
/// que se espera): se comprueba que se aparta con un mensaje claro y sin tocar nada, y la prueba de
/// ese repositorio acaba ahí. Devuelve `true` si es el caso.
fn la_10_se_aparta(p: &Prueba, viejo: &Equipo, clave: &[u8]) -> bool {
    let m = min_reader(p, clave);
    if m <= 1 {
        return false;
    }
    eprintln!("compat: el commit pide min_reader {m}: la 1.0 no debe tocar el repositorio");
    let commits = p.commits();
    viejo.guardar(&viejo.dir, "fact", "No debe subir", "Nada.", &["--global"]);
    let o = viejo.omoi_en(&viejo.dir, &["sync"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs a newer version of omoi"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(p.commits(), commits, "no escribe nada en él");
    true
}

// --- Sincronizar por el mismo repositorio ------------------------------------------------------------

/// `de` escribe (memorias con historial, un veredicto con motivo, un olvido) y `a` lo recibe todo; lo
/// que `a` saca de la papelera vuelve a `de`.
fn de_un_equipo_a_otro(de: &Equipo, a: &Equipo) {
    let remoto = "git@github.com:marc/tienda.git";
    let (pd, pa) = (de.proyecto("tienda", remoto), a.proyecto("tienda", remoto));
    let q = de.quien();
    let cookie = de.guardar(&pd, "decision", &format!("Sesiones por cookie, de {q}"), "La autenticación usa una cookie de sesión.", &[]);
    de.ok_en(&pd, &["update", &cookie, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    let jwt = de.guardar(&pd, "decision", &format!("Autenticación con JWT, de {q}"), "Se pasó a JWT en la cabecera Authorization.", &[]);
    de.tool_en(&pd, "update", &format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}"));
    let olvidada = de.guardar(&de.dir, "fact", &format!("El puerto de pagos, de {q}"), "Escucha en el 8080.", &["--global"]);
    de.sync_en(&pd);
    a.sync_en(&pa);
    assert!(a.get(&olvidada).is_some(), "{}: llega de {q}", a.quien());
    de.ok(&["forget", &olvidada, "--yes"]);
    de.sync_en(&pd);
    a.sync_en(&pa);

    // Las memorias, en la carpeta del proyecto de este equipo y con su historial.
    let m = a.get(&cookie).unwrap_or_else(|| panic!("{}: falta la memoria de {q}", a.quien()));
    assert_eq!(m["content"], "La autenticación usaba una cookie de sesión; ver JWT.");
    let ruta_a = pa.canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\").to_string();
    assert_eq!(m["project"].as_str(), Some(ruta_a.as_str()), "en la carpeta de {}: {m}", a.quien());
    let h = a.json_en(&pa, &["history", &cookie]);
    assert_eq!(h.as_array().unwrap().len(), 1, "{}: el historial viaja: {h}", a.quien());
    // El veredicto, con su motivo y su autor.
    let rev = a.json_en(&pa, &["review", "--all"]);
    let av = rev
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "superseded" && x["memory_id"] == cookie.as_str())
        .unwrap_or_else(|| panic!("{}: falta el veredicto de {q}: {rev}", a.quien()));
    assert_eq!((av["other_id"].as_str(), av["reason"].as_str(), av["author"].as_str()), (Some(jwt.as_str()), Some("JWT replaced the cookie"), Some(persona(de))), "{av}");
    // El olvido llega; lo olvidado se recupera desde el otro equipo y vuelve.
    assert!(a.get(&olvidada).is_none(), "{}: el olvido de {q} llega", a.quien());
    assert!(a.ok(&["sync", "restore"]).contains("El puerto de pagos"), "{}: en la papelera de la sync", a.quien());
    a.ok(&["sync", "restore", &olvidada]);
    assert_eq!(a.get(&olvidada).unwrap()["content"], "Escucha en el 8080.");
    a.sync_en(&pa);
    de.sync_en(&pd);
    assert!(de.get(&olvidada).is_some(), "{q}: la recuperada en {} vuelve", a.quien());
    assert!(!de.ok(&["trash"]).contains("El puerto de pagos"), "{q}: y sale de su papelera");
}

fn ida_y_vuelta(bin_a: &Path, bin_b: &Path, nombre: &str) {
    let p = Prueba::nueva(nombre);
    let (a, b) = dos(&p, bin_a, bin_b);
    let clave = clave_del_codigo(&a.ok(&["sync", "code"]));
    a.guardar(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    let viejo = if a.es_el_commit() { &b } else { &a };
    if la_10_se_aparta(&p, viejo, &clave) {
        return;
    }
    de_un_equipo_a_otro(&a, &b);
    de_un_equipo_a_otro(&b, &a);
}

#[test]
fn el_commit_configura_y_la_10_se_une() {
    let Some(v10) = binario_10() else { return };
    ida_y_vuelta(Path::new(NUEVO), &v10, "commit-10");
}

#[test]
fn la_10_configura_y_el_commit_se_une() {
    let Some(v10) = binario_10() else { return };
    ida_y_vuelta(&v10, Path::new(NUEVO), "10-commit");
}

/// Un repositorio para un espacio (con su clave) y la rotación de claves, con cada binario a un lado.
fn espacios_y_rotacion(bin_a: &Path, bin_b: &Path, nombre: &str) {
    let p = Prueba::nueva(nombre);
    let (a, b) = dos(&p, bin_a, bin_b);
    let acme = p.otro_repo("acme");
    a.ok(&["sync", "init", "--space", "acme", &acme, "--yes"]);
    let codigo = a.ok(&["sync", "code", "--space", "acme"]).trim().to_string();
    b.ok(&["sync", "join", &codigo, "--yes"]);
    a.ok(&["space", "add", "acme"]);
    let web = a.proyecto("acme-web", "git@github.com:acme/web.git");
    a.ok_en(&web, &["space", "assign", "acme"]);
    let de_acme = a.guardar(&web, "decision", "Acme factura en euros", "Siempre con IVA.", &["--space=acme"]);
    let del_proyecto = a.guardar(&web, "fact", "El staging de Acme está en Frankfurt", "Región eu-central-1.", &[]);
    let general = a.guardar(&a.dir, "fact", "Mi editor es Helix", "Con el tema por defecto.", &["--global"]);
    a.sync();
    b.sync();
    assert_eq!(b.get(&de_acme).unwrap_or_else(|| panic!("{}: falta la de acme", b.quien()))["space"], "acme");
    assert_eq!(b.get(&del_proyecto).unwrap_or_else(|| panic!("{}: falta la del proyecto de acme", b.quien()))["space"], "acme");
    assert!(b.get(&general).is_some());
    // En el repositorio de acme, solo lo de acme.
    let clave_acme = clave_del_codigo(&codigo);
    let lineas = p.lineas_de(&acme);
    assert!(!lineas.is_empty());
    for l in lineas {
        let j = descifrar_linea(&clave_acme, &l).unwrap_or_else(|| panic!("una línea de acme que no abre su clave: {l}"));
        assert_eq!(j["space_key"], "acme", "{j}");
    }
    // Lo que B escribe en acme vuelve a A por el de acme.
    let de_b = b.guardar(&b.dir, "gotcha", "La VPN de Acme corta a la hora", "Volver a entrar.", &["--space=acme"]);
    b.sync();
    a.sync();
    assert_eq!(a.get(&de_b).unwrap_or_else(|| panic!("{}: falta la de {}", a.quien(), b.quien()))["space"], "acme");

    // A rota la clave del general: B, con la vieja, no lee lo nuevo hasta unirse con el código nuevo.
    let r = a.json_en(&a.dir, &["sync", "rotate", "--yes"]);
    assert_eq!(r["key"], 2, "{r}");
    let nuevo = a.ok(&["sync", "code"]).trim().to_string();
    let tras_rotar = a.guardar(&a.dir, "decision", "Las copias van a Atlas", "Cada noche a las tres.", &["--global"]);
    a.sync();
    let r = b.json_en(&b.dir, &["sync"]);
    assert!(b.get(&tras_rotar).is_none() && r["ilegibles"].as_u64().unwrap() > 0, "{}: {r}", b.quien());
    assert!(b.ok(&["sync", "status"]).contains("key has changed"));
    b.ok(&["sync", "join", &nuevo, "--yes"]);
    assert!(b.get(&tras_rotar).is_some(), "{}: con la clave nueva", b.quien());
    let de_b = b.guardar(&b.dir, "fact", "Atlas se duerme a los 20 minutos", "Wake-on-lan.", &["--global"]);
    b.sync();
    a.sync();
    assert!(a.get(&de_b).is_some(), "{}: lo de {} tras rotar", a.quien(), b.quien());

    // B rota la de acme, y A hace lo que hizo B.
    let r = b.json_en(&b.dir, &["sync", "rotate", "--space", "acme", "--yes"]);
    assert_eq!(r["key"], 2, "{r}");
    let nuevo_acme = r["code"].as_str().unwrap().to_string();
    let tras_rotar = b.guardar(&b.dir, "fact", "Acme cierra en agosto", "Todo el mes.", &["--space=acme"]);
    b.sync();
    let r = a.json_en(&a.dir, &["sync"]);
    assert!(a.get(&tras_rotar).is_none(), "{}: {r}", a.quien());
    a.ok(&["sync", "join", &nuevo_acme, "--yes"]);
    assert_eq!(a.get(&tras_rotar).unwrap_or_else(|| panic!("{}: con la clave nueva de acme", a.quien()))["space"], "acme");
    a.sync();
    b.sync();
}

#[test]
fn espacios_y_rotacion_el_commit_configura() {
    let Some(v10) = binario_10() else { return };
    espacios_y_rotacion(Path::new(NUEVO), &v10, "esp-commit-10");
}

#[test]
fn espacios_y_rotacion_la_10_configura() {
    let Some(v10) = binario_10() else { return };
    espacios_y_rotacion(&v10, Path::new(NUEVO), "esp-10-commit");
}

const HLC_FUTURO: &str = "1761696000000-0000000000-pc-futuro";

/// Una 1.x más nueva escribe en el repositorio una memoria, un veredicto y un olvido con un campo que
/// la 1.0 no conoce, y líneas de un tipo nuevo. La 1.0 las cuenta aparte (no como ilegibles), cambia
/// la memoria y la vuelve a subir con el campo, y no toca el fichero de la otra; el commit la recibe
/// con el campo.
#[test]
fn lo_que_escribe_una_1x_pasa_por_la_10_sin_perderse() {
    let Some(v10) = binario_10() else { return };
    let p = Prueba::nueva("futuro");
    let (a, viejo) = dos(&p, Path::new(NUEVO), &v10);
    let c = p.equipo("pc-c", Path::new(NUEVO));
    c.ok(&["sync", "join", a.ok(&["sync", "code"]).trim(), "--yes", "--author", "Ana"]);
    let clave = clave_del_codigo(&a.ok(&["sync", "code"]));
    a.guardar(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    if la_10_se_aparta(&p, &viejo, &clave) {
        return;
    }
    let id = "01M62EWVHRXM0000000000FUT1";
    let olvidada = "01M62EWVHRXM0000000000FUT2";
    let futuro = serde_json::json!({ "x": 1, "y": ["dos"] });
    let lineas = [
        serde_json::json!({
            "type": "memory", "id": id, "project_key": "", "project_path": "", "kind": "fact",
            "title": "Una memoria de una 1.x", "content": "Con un campo que la 1.0 no conoce.",
            "created_at": "2026-10-29T00:00:00.000Z", "updated_at": "2026-10-29T00:00:00.000Z",
            "hlc": HLC_FUTURO, "machine": "pc-futuro", "futuro": futuro
        }),
        serde_json::json!({
            "type": "verdict", "memory_id": id, "relation": "ok:unused", "at": "2026-10-29T00:00:00.000Z",
            "hlc": "1761696000001-0000000000-pc-futuro", "machine": "pc-futuro", "futuro": futuro
        }),
        serde_json::json!({
            "type": "tombstone", "id": olvidada, "deleted_at": "2026-10-29T00:00:00.000Z",
            "hlc": "1761696000002-0000000000-pc-futuro", "machine": "pc-futuro", "futuro": futuro
        }),
    ];
    let comentario = serde_json::json!({ "type": "comment", "memory_id": id, "text": "una línea de un tipo nuevo", "hlc": HLC_FUTURO });
    let mut texto: Vec<String> = lineas.iter().map(|l| cifrar_linea(&clave, l)).collect();
    texto.push(cifrar_linea(&clave, &comentario));
    texto.push(format!("z2:{}", cifrar_linea(&clave, &comentario)));
    let fichero = texto.join("\n") + "\n";
    p.subir_a_mano("global/m-futuro.jsonl", &fichero);

    let r = viejo.sync();
    assert_eq!((r["nuevas"].as_u64(), r["de_version_nueva"].as_u64()), (Some(2), Some(2)), "la de la 1.x y la de A: {r}");
    assert!(viejo.get(id).is_some());
    let equipo_viejo = r["equipo"].as_str().unwrap().to_string();
    assert!(viejo.ok(&["sync", "status"]).contains("from a newer version"));
    let guardado: String = viejo.base().query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&guardado).unwrap(), serde_json::json!({ "futuro": futuro }));

    // La 1.0 la cambia y la vuelve a subir: el campo va con la memoria, el veredicto y el olvido.
    viejo.ok(&["update", id, "-c", "Cambiada por la 1.0.", "-r", "aquí"]);
    viejo.ok(&["update", id, "--pin"]);
    viejo.sync();
    let del_viejo: Vec<serde_json::Value> = p.lineas_de(&p.url()).iter().filter_map(|l| descifrar_linea(&clave, l)).filter(|l| l["machine"] == equipo_viejo.as_str()).collect();
    let memoria = del_viejo.iter().find(|l| l["type"] == "memory" && l["id"] == id).unwrap_or_else(|| panic!("{del_viejo:?}"));
    assert_eq!((&memoria["content"], &memoria["pinned"], &memoria["futuro"]), (&serde_json::json!("Cambiada por la 1.0."), &serde_json::json!(true), &futuro), "{memoria}");
    assert_ne!(memoria["hlc"], HLC_FUTURO);
    for tipo in ["verdict", "tombstone"] {
        let l = del_viejo.iter().find(|l| l["type"] == tipo).unwrap_or_else(|| panic!("{tipo}: {del_viejo:?}"));
        assert_eq!(l["futuro"], futuro, "{l}");
    }
    // El fichero de la 1.x sigue tal cual, con las líneas que la 1.0 no entiende.
    let en_repo = std::fs::read_to_string(p.clon_de(&p.url()).join("global").join("m-futuro.jsonl")).unwrap();
    assert_eq!(en_repo, fichero);

    // El commit la recibe cambiada y con el campo.
    c.sync();
    assert_eq!(c.get(id).expect("llega al commit")["content"], "Cambiada por la 1.0.");
    let guardado: String = c.base().query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&guardado).unwrap()["futuro"], futuro);
}

// --- La base ------------------------------------------------------------------------------------------

/// Los ficheros de una base (`o.db`, `o.db-wal`…) con su contenido, para ver que nadie los toca.
fn ficheros_de_la_base(dir: &Path, base: &str) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().starts_with(base))
        .map(|f| (f.file_name().to_string_lossy().into_owned(), std::fs::read(f.path()).unwrap()))
        .collect();
    v.sort();
    v
}

fn version_de(db: &rusqlite::Connection) -> i64 {
    db.query_row("PRAGMA user_version", [], |f| f.get(0)).unwrap()
}

/// La 1.0 abre una base del commit, la usa, y el commit sigue viendo lo que escribió la 1.0; o, si el
/// commit ha subido `lector_minimo` por encima del esquema 10, se niega con un mensaje claro y sin
/// tocarla. Además, contra la 1.0 de verdad: una base de un esquema futuro que solo añade se abre, y
/// una que pide un lector más nuevo no se toca.
#[test]
fn la_10_abre_la_base_del_commit() {
    let Some(v10) = binario_10() else { return };
    let p = Prueba::nueva("base");
    let nuevo = p.equipo("pc", Path::new(NUEVO));
    let viejo = p.equipo("pc", &v10);
    let proyecto = nuevo.proyecto("tienda", "git@github.com:marc/tienda.git");
    let id = nuevo.guardar(&proyecto, "decision", "Los precios van en céntimos", "Todos los importes en enteros.", &[]);
    nuevo.ok_en(&proyecto, &["update", &id, "-c", "Todos los importes en enteros de céntimos.", "-r", "más claro"]);
    let global = nuevo.guardar(&nuevo.dir, "preference", "Respuestas cortas", "Sin rodeos.", &["--global"]);
    let olvidada = nuevo.guardar(&nuevo.dir, "fact", "Una que sobra", "Nada.", &["--global"]);
    nuevo.ok(&["forget", &olvidada, "--yes"]);

    let (version, lector) = {
        let db = nuevo.base();
        let v = version_de(&db);
        let l: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |f| f.get(0)).ok();
        (v, l.and_then(|l| l.trim().parse::<i64>().ok()).unwrap_or(v))
    };
    if lector > 10 {
        eprintln!("compat: el commit pide lector_minimo {lector}: la 1.0 no debe abrir la base");
        let antes = ficheros_de_la_base(&nuevo.dir, "o.db");
        let o = viejo.omoi_en(&viejo.dir, &["get", &id]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("newer omoi"), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(ficheros_de_la_base(&nuevo.dir, "o.db"), antes, "sin tocarla");
    } else {
        let m = viejo.get(&id).expect("la 1.0 la lee");
        assert_eq!(m["content"], "Todos los importes en enteros de céntimos.");
        assert_eq!(viejo.json_en(&proyecto, &["history", &id]).as_array().unwrap().len(), 1);
        let s = viejo.json_en(&proyecto, &["search", "céntimos"]);
        assert_eq!(s[0]["id"], id.as_str(), "{s}");
        assert!(viejo.get(&global).is_some());
        assert!(viejo.ok(&["trash"]).contains("Una que sobra"));
        let de_10 = viejo.guardar(&proyecto, "fact", "Escrita por la 1.0", "En la base del commit.", &[]);
        viejo.ok_en(&proyecto, &["update", &id, "--pin"]);
        assert_eq!(version_de(&nuevo.base()), version, "la 1.0 no le baja el esquema");
        // Y el commit sigue con ella.
        assert_eq!(nuevo.get(&de_10).expect("el commit ve lo de la 1.0")["content"], "En la base del commit.");
        assert_eq!(nuevo.get(&id).unwrap()["pinned"], true);
        nuevo.ok(&["restore", &olvidada]);
    }

    // Una base de un esquema futuro que solo añade (una tabla y una columna con valor por defecto,
    // legible desde el 10): la 1.0 la abre, escribe en ella y no la baja.
    let futura = p.dir.join("pc").join("futura.db");
    nuevo.base().execute("VACUUM INTO ?1", [futura.to_str().unwrap()]).unwrap();
    let esquema_futuro = version.max(10) + 1;
    {
        let db = rusqlite::Connection::open(&futura).unwrap();
        db.execute_batch(&format!(
            "PRAGMA user_version = {esquema_futuro}; CREATE TABLE futura (x); INSERT INTO futura VALUES (1);
             ALTER TABLE memory ADD COLUMN futura TEXT NOT NULL DEFAULT 'x';
             INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', '10');"
        ))
        .unwrap();
    }
    let con_futura = |args: &[&str]| viejo.comando(&viejo.dir).env("OMOIKANE_DB", &futura).args(args).stdin(Stdio::null()).output().unwrap();
    let o = con_futura(&["--json", "remember", "-k", "fact", "-t", "En una base futura", "--global", "La 1.0 escribe en ella."]);
    assert!(o.status.success(), "la 1.0 abre una base que solo añade: {}", String::from_utf8_lossy(&o.stderr));
    let o = con_futura(&["--json", "get", &global]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    {
        let db = rusqlite::Connection::open(&futura).unwrap();
        assert_eq!(version_de(&db), esquema_futuro, "no le baja el esquema");
        assert_eq!(db.query_row("SELECT count(*) FROM futura", [], |f| f.get::<_, i64>(0)).unwrap(), 1);
        let (todas, con_valor): (i64, i64) = db.query_row("SELECT count(*), count(*) FILTER (WHERE futura = 'x') FROM memory", [], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
        assert_eq!(todas, con_valor, "la columna nueva sigue, con su valor por defecto");
        // Y otra que rompe: legible solo desde un esquema que la 1.0 no conoce.
        db.execute_batch(&format!("PRAGMA user_version = {0}; UPDATE meta SET v = '{0}' WHERE k = 'lector_minimo';", esquema_futuro + 1)).unwrap();
    }
    let antes = ficheros_de_la_base(&p.dir.join("pc"), "futura.db");
    let o = con_futura(&["get", &global]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("newer omoi") && err.contains("update omoi"), "{err}");
    assert_eq!(ficheros_de_la_base(&p.dir.join("pc"), "futura.db"), antes, "sin tocarla");
}

// --- omoi export / omoi import ------------------------------------------------------------------------

/// Una base con de todo: historial, un veredicto con motivo y la papelera.
fn llenar(e: &Equipo, en: &Path, quien: &str) {
    let cookie = e.guardar(en, "decision", &format!("Sesiones por cookie ({quien})"), "La autenticación usa una cookie de sesión.", &[]);
    e.ok_en(en, &["update", &cookie, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    let jwt = e.guardar(en, "decision", &format!("Autenticación con JWT ({quien})"), "Se pasó a JWT en la cabecera Authorization.", &[]);
    e.tool_en(en, "update", &format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}"));
    e.guardar(en, "preference", &format!("Respuestas cortas ({quien})"), "Sin rodeos.", &["--global"]);
    let sobra = e.guardar(en, "fact", &format!("Una que sobra ({quien})"), "Nada.", &["--global"]);
    e.ok_en(en, &["update", &sobra, "-c", "Nada de nada.", "-r", "más claro"]);
    e.ok_en(en, &["forget", &sobra, "--yes"]);
}

/// Las líneas de una copia, sin la cabecera (lleva la hora) ni el cierre, y sin lo que depende del
/// equipo que exporta (`machine`).
fn cuerpo(texto: &str) -> Vec<serde_json::Value> {
    texto
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.remove("machine");
            }
            v
        })
        .filter(|v| v["type"] != "header" && v["type"] != "end")
        .collect()
}

/// `a` está dentro de `b`: todo lo que dice `a` lo dice `b` igual (en los objetos, `b` puede tener
/// más campos).
fn contenido(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => x.iter().all(|(k, v)| y.get(k).is_some_and(|w| contenido(v, w))),
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| contenido(v, w)),
        _ => a == b,
    }
}

/// Exporta desde `de`, importa en `a` (una base vacía) y vuelve a exportar desde `a`: no se pierde
/// nada de lo que llevaba la copia de `de`, y repetir el import no cambia nada.
fn copia_de_uno_a_otro(p: &Prueba, de: &Equipo, a: &Equipo, en: &Path) {
    let o = de.omoi_en(en, &["export", "--all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let texto = String::from_utf8(o.stdout).unwrap();
    let fichero = p.dir.join(format!("copia-{}.jsonl", ulid::Ulid::generate()));
    std::fs::write(&fichero, &texto).unwrap();
    let cabecera: serde_json::Value = serde_json::from_str(texto.lines().next().unwrap()).unwrap();
    let version = cabecera["version"].as_u64().unwrap();
    if version > 2 && !a.es_el_commit() {
        // El commit ha subido el formato: la 1.0 tiene que negarse y no meter nada.
        eprintln!("compat: el commit exporta el formato {version}: la 1.0 no debe importarlo");
        let o = a.omoi_en(en, &["import", fichero.to_str().unwrap()]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("comes from a newer omoi"), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(cuerpo(&a.ok_en(en, &["export", "--all"])).is_empty(), "{}: no mete nada", a.quien());
        return;
    }
    let r = a.json_en(en, &["import", fichero.to_str().unwrap()]);
    assert_eq!((r["skipped_lines"].as_u64(), r["updated"].as_u64()), (Some(0), Some(0)), "{}: {r}", a.quien());
    let o = a.omoi_en(en, &["export", "--all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (antes, despues) = (cuerpo(&texto), cuerpo(&String::from_utf8(o.stdout).unwrap()));
    assert_eq!(antes.len(), despues.len(), "{} → {}: {antes:#?}\n{despues:#?}", de.quien(), a.quien());
    for l in &antes {
        assert!(despues.iter().any(|d| contenido(l, d)), "{} → {}: se pierde {l:#}\nen {despues:#?}", de.quien(), a.quien());
    }
    let r = a.json_en(en, &["import", fichero.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["updated"].as_u64(), r["forgotten"].as_u64()), (Some(0), Some(0), Some(0)), "{}: {r}", a.quien());
}

#[test]
fn export_del_commit_lo_importa_la_10_y_al_reves() {
    let Some(v10) = binario_10() else { return };
    let p = Prueba::nueva("export");
    // El mismo nombre de equipo y la misma carpeta de proyecto en las cuatro bases.
    let nuevo = p.equipo_en("nuevo", "pc-pruebas", Path::new(NUEVO));
    let viejo = p.equipo_en("viejo", "pc-pruebas", &v10);
    let proyecto = nuevo.proyecto("tienda", "git@github.com:marc/tienda.git");
    llenar(&nuevo, &proyecto, "commit");
    copia_de_uno_a_otro(&p, &nuevo, &viejo, &proyecto);
    let nuevo2 = p.equipo_en("nuevo2", "pc-pruebas", Path::new(NUEVO));
    let viejo2 = p.equipo_en("viejo2", "pc-pruebas", &v10);
    llenar(&viejo2, &proyecto, "1.0");
    copia_de_uno_a_otro(&p, &viejo2, &nuevo2, &proyecto);
}
