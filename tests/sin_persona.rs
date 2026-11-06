//! 1.7.0, agentes sin persona delante (CI, servidores, contenedores): `omoi` sin carpeta de usuario,
//! con todo en `OMOIKANE_HOME`, sin preguntas (`OMOIKANE_NONINTERACTIVE=1`), unido a la sincronización
//! por `OMOIKANE_SYNC_CODE` y firmando con `OMOIKANE_AUTHOR`. El código y su clave no salen nunca en
//! la salida. Todo en carpetas temporales y con un repositorio git desnudo local: ni la carpeta de
//! usuario de verdad ni su `~/.omoikane` se tocan (cada orden lleva `HOME`/`USERPROFILE` falsos o
//! ninguno, y `OMOIKANE_HOME` o `OMOIKANE_DB` propios).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Prueba {
    dir: PathBuf,
}

impl Prueba {
    fn nueva(nombre: &str) -> Prueba {
        let dir = std::env::temp_dir().join(format!("omoi-sinpersona-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gitconfig-vacio"), "").unwrap();
        Prueba { dir }
    }

    /// `omoi` con lo justo para que no dependa de la máquina: git sin configuración del usuario, sin
    /// idioma ni base ni carpetas heredadas, y la entrada cerrada. `env` añade o quita (valor `None`).
    fn omoi(&self, en: &Path, args: &[&str], env: &[(&str, Option<&str>)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(en)
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_SYNC_AUTO", "0");
        for k in [
            "OMOIKANE_LANG",
            "OMOIKANE_DB",
            "OMOIKANE_HOME",
            "OMOIKANE_MODELS",
            "OMOIKANE_SYNC_CODE",
            "OMOIKANE_SYNC_URL",
            "OMOIKANE_AUTHOR",
            "OMOIKANE_NONINTERACTIVE",
            "OMOIKANE_SEMANTIC_MODEL",
        ] {
            c.env_remove(k);
        }
        for (k, v) in env {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        c.stdin(Stdio::null()).output().unwrap()
    }

    /// Una carpeta de proyecto con remoto `origin`.
    fn proyecto(&self, nombre: &str, remoto: &str) -> PathBuf {
        let p = self.dir.join(nombre);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remoto}\n")).unwrap();
        p
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

fn ok(o: &Output, que: &str) -> String {
    assert!(o.status.success(), "{que}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn salida(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn vacia(d: &Path) -> bool {
    std::fs::read_dir(d).map(|mut x| x.next().is_none()).unwrap_or(true)
}

/// La clave (en hexadecimal) que lleva un código de `omoi sync code`.
fn clave_de(codigo: &str) -> String {
    use base64::Engine;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(codigo.trim().strip_prefix("omoi1.").unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    v["key"].as_str().unwrap().to_string()
}

/// `OMOIKANE_HOME` se lo lleva todo: la base, los modelos y lo demás van ahí y no a la carpeta de
/// usuario (aquí, una falsa, que tiene que quedar vacía). Y sin carpeta de usuario también funciona.
#[test]
fn todo_en_omoikane_home_y_sin_carpeta_de_usuario() {
    let p = Prueba::nueva("home");
    let falsa = p.dir.join("usuario-falso");
    let casa = p.dir.join("casa-omoikane");
    std::fs::create_dir_all(&falsa).unwrap();
    let proyecto = p.proyecto("web", "git@github.com:acme/web.git");
    let f = falsa.to_str().unwrap();
    let h = casa.to_str().unwrap();
    let con_falsa = [("HOME", Some(f)), ("USERPROFILE", Some(f)), ("OMOIKANE_HOME", Some(h))];
    ok(&p.omoi(&proyecto, &["remember", "-k", "gotcha", "-t", "El build necesita Node 22", "Con Node 20 falla el bundler."], &con_falsa), "remember");
    assert!(casa.join("omoikane.db").exists(), "la base va a OMOIKANE_HOME");
    assert!(vacia(&falsa), "nada en la carpeta de usuario: {:?}", std::fs::read_dir(&falsa).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
    let modelos = ok(&p.omoi(&proyecto, &["model", "status"], &con_falsa), "model status");
    assert!(Path::new(modelos.lines().last().unwrap_or_default().trim()).starts_with(&casa), "los modelos también: {modelos}");
    ok(&p.omoi(&proyecto, &["model", "semantic", "--off"], &con_falsa), "semantic --off");
    assert!(casa.join("semantic").exists() && vacia(&falsa), "y la preferencia del modelo de significado");

    // Sin HOME ni USERPROFILE: la misma base, desde OMOIKANE_HOME.
    let sin_casa = [("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(h))];
    let v: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&proyecto, &["--json", "search", "Node bundler"], &sin_casa), "search")).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    ok(&p.omoi(&proyecto, &["doctor"], &sin_casa), "doctor");
}

/// Sin carpeta de usuario que valga y sin variables, un error claro, sin crear la base en la carpeta
/// en la que se ejecuta. (Un `HOME` relativo es la forma de probarlo en cualquier sistema: Windows y
/// Linux sacan la carpeta de usuario de otro sitio si falta la variable.)
#[test]
fn sin_carpeta_ni_variables_un_error_claro() {
    let p = Prueba::nueva("nada");
    let aqui = p.dir.join("aqui");
    std::fs::create_dir_all(&aqui).unwrap();
    let o = p.omoi(&aqui, &["remember", "-k", "fact", "-t", "Algo", "Algo que guardar."], &[("HOME", Some("relativa")), ("USERPROFILE", Some("relativa"))]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("OMOIKANE_HOME"), "{}", salida(&o));
    assert!(vacia(&aqui), "no deja una base en la carpeta en la que se ejecuta");
}

/// Una CI: se une al repositorio con `OMOIKANE_SYNC_CODE`, trae lo que hay, busca, guarda con su
/// autor y lo sube; el código y su clave no salen en ninguna salida; un fallo de red sale con error.
#[test]
fn una_ci_se_une_por_variables() {
    let p = Prueba::nueva("ci");
    let g = Command::new("git").current_dir(&p.dir).args(["init", "--quiet", "--bare", "memoria.git"]).env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio")).output().unwrap();
    assert!(g.status.success());
    let url = p.dir.join("memoria.git").to_string_lossy().replace('\\', "/");

    // El equipo de una persona: configura la sincronización y guarda algo del proyecto.
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let db_pc = pc.join("o.db");
    let env_pc = [("OMOIKANE_DB", Some(db_pc.to_str().unwrap())), ("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_pc = p.proyecto("web-pc", "git@github.com:acme/web.git");
    ok(&p.omoi(&pc, &["sync", "init", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    ok(&p.omoi(&web_pc, &["remember", "-k", "gotcha", "-t", "Las migraciones van antes del deploy", "Si no, el deploy arranca con el esquema viejo."], &env_pc), "remember pc");
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc");
    let codigo = ok(&p.omoi(&pc, &["sync", "code"], &env_pc), "code").trim().to_string();
    let clave = clave_de(&codigo);

    // La CI: sin carpeta de usuario, sin terminal, todo por variables.
    let ci = p.dir.join("ci-home");
    let web_ci = p.proyecto("checkout", "https://github.com/acme/web");
    let env_ci = [
        ("HOME", None),
        ("USERPROFILE", None),
        ("OMOIKANE_HOME", Some(ci.to_str().unwrap())),
        ("OMOIKANE_NONINTERACTIVE", Some("1")),
        ("OMOIKANE_SYNC_CODE", Some(codigo.as_str())),
        ("OMOIKANE_AUTHOR", Some("ci-bot")),
        ("OMOIKANE_EQUIPO", Some("ci")),
    ];
    // `sync join` sin código: el de la variable. Lo que escribe no lleva ni el código ni la clave.
    let o = p.omoi(&web_ci, &["sync", "join"], &env_ci);
    let todo = salida(&o);
    ok(&o, "join");
    assert!(!todo.contains(&codigo) && !todo.contains(&clave), "el código no sale: {todo}");
    assert!(ci.join("omoikane.db").exists() && ci.join("sync.json").exists());
    // Ya unida: `--pull` vuelve a leer la variable, ve que ya tiene esa clave y trae lo que haya.
    ok(&p.omoi(&web_ci, &["sync", "--pull"], &env_ci), "pull");
    // El recall de un mensaje, como lo haría un hook (`USER_PROMPT`, formato de texto).
    let mut con_prompt = env_ci.to_vec();
    con_prompt.push(("USER_PROMPT", Some("antes del deploy, hay que correr las migraciones?")));
    let r = ok(&p.omoi(&web_ci, &["recall", "--format", "plain"], &con_prompt), "recall");
    assert!(r.contains("Las migraciones van antes del deploy"), "{r}");
    let s: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&web_ci, &["--json", "search", "migraciones deploy"], &env_ci), "search")).unwrap();
    assert_eq!(s[0]["title"], "Las migraciones van antes del deploy", "{s}");

    // Lo que guarda la CI lo firma `OMOIKANE_AUTHOR` y llega al equipo de la persona.
    let m: serde_json::Value = serde_json::from_str(&ok(
        &p.omoi(&web_ci, &["--json", "remember", "-k", "fact", "-t", "La CI tarda 9 minutos", "El paso lento es el e2e."], &env_ci),
        "remember ci",
    ))
    .unwrap();
    assert_eq!(m["author"], "ci-bot", "{m}");
    let o = p.omoi(&web_ci, &["--json", "sync"], &env_ci);
    let j: serde_json::Value = serde_json::from_str(&ok(&o, "sync ci")).expect("solo JSON en la salida");
    assert!(j["exportadas"].as_u64().or(j["exported"].as_u64()).is_some_and(|n| n >= 1), "{j}");
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc 2");
    let en_pc: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&web_pc, &["--json", "get", m["id"].as_str().unwrap()], &env_pc), "get")).unwrap();
    assert_eq!(en_pc["author"], "ci-bot", "{en_pc}");

    // Sin red (el repositorio ya no está), `--pull` sin nadie delante sale con error, sin la clave.
    std::fs::rename(p.dir.join("memoria.git"), p.dir.join("movida.git")).unwrap();
    let o = p.omoi(&web_ci, &["sync", "--pull"], &env_ci);
    assert!(!o.status.success(), "{}", salida(&o));
    assert!(!salida(&o).contains(&clave));
    // El `--pull` que lanza `omoi` mismo en segundo plano (`OMOIKANE_SYNC_AUTO=1` en un servidor) sigue
    // siendo el barato aunque no haya nadie delante: sin red no falla, lo intentará más tarde.
    let mut de_fondo = env_ci.to_vec();
    de_fondo.push(("OMOIKANE_INTERNAL_BACKGROUND", Some("1")));
    let o = p.omoi(&web_ci, &["sync", "--pull"], &de_fondo);
    assert!(o.status.success(), "{}", salida(&o));
    std::fs::rename(p.dir.join("movida.git"), p.dir.join("memoria.git")).unwrap();

    // Un código malo o una URL a la que no se llega: error claro, sin el código ni la clave.
    let otra = p.dir.join("ci-2");
    let mal = format!("{codigo}x");
    let o = p.omoi(&web_ci, &["sync"], &[("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(otra.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_SYNC_CODE", Some(mal.as_str()))]);
    assert!(!o.status.success() && salida(&o).contains("OMOIKANE_SYNC_CODE") && !salida(&o).contains(&clave), "{}", salida(&o));
    let o = p.omoi(
        &web_ci,
        &["sync"],
        &[
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(otra.to_str().unwrap())),
            ("OMOIKANE_NONINTERACTIVE", Some("1")),
            ("OMOIKANE_SYNC_CODE", Some(codigo.as_str())),
            ("OMOIKANE_SYNC_URL", Some("https://ci:s3cr3t-t0ken@127.0.0.1:9/no/existe.git")),
        ],
    );
    let todo = salida(&o);
    assert!(!o.status.success(), "{todo}");
    assert!(!todo.contains(&clave) && !todo.contains("s3cr3t-t0ken"), "ni la clave ni el token: {todo}");
}

/// Sin nadie delante no se pregunta aunque haya que confirmar: hace falta `--yes`.
#[test]
fn sin_nadie_delante_no_pregunta() {
    let p = Prueba::nueva("preguntas");
    let casa = p.dir.join("casa");
    let env = [("OMOIKANE_HOME", Some(casa.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1"))];
    let pr = p.proyecto("web", "git@github.com:acme/web.git");
    let m: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&pr, &["--json", "remember", "-k", "fact", "-t", "Algo", "Algo que olvidar."], &env), "remember")).unwrap();
    let o = p.omoi(&pr, &["forget", m["id"].as_str().unwrap()], &env);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--yes"), "{}", salida(&o));
}

/// Un repositorio git desnudo en la carpeta de la prueba; su URL.
fn repo_desnudo(p: &Prueba, nombre: &str) -> String {
    let g = Command::new("git").current_dir(&p.dir).args(["init", "--quiet", "--bare", nombre]).env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio")).output().unwrap();
    assert!(g.status.success());
    p.dir.join(nombre).to_string_lossy().replace('\\', "/")
}

fn json(o: &Output, que: &str) -> serde_json::Value {
    serde_json::from_str(&ok(o, que)).unwrap_or_else(|e| panic!("{que}: {e}"))
}

/// Las variables como `(&str, Option<&str>)`, que es lo que toma `Prueba::omoi`.
fn prestadas<'a>(v: &'a [(&'static str, Option<String>)]) -> Vec<(&'static str, Option<&'a str>)> {
    v.iter().map(|(k, x)| (*k, x.as_deref())).collect()
}

/// Una CI que se une al ESPACIO de un cliente por `OMOIKANE_SYNC_CODE` parte de una base nueva: su
/// carpeta es un proyecto que el repositorio del espacio dice suyo y que este equipo no tenía. Sin
/// nadie delante, la variable es la decisión: entra en el espacio sin pausa, trae lo que hay, busca,
/// guarda y lo sube, y el equipo de la persona lo recibe. Con una persona delante (sin
/// `OMOIKANE_NONINTERACTIVE`), o si la carpeta ya tenía memorias aquí, sigue en pausa.
#[test]
fn una_ci_se_une_a_un_espacio_por_variables() {
    let p = Prueba::nueva("ci-espacio");
    let url = repo_desnudo(&p, "acme.git");

    // El equipo de la persona: el proyecto en el espacio acme, con su repositorio.
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env_pc = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_pc = p.proyecto("web-pc", "git@github.com:acme/web.git");
    ok(&p.omoi(&pc, &["space", "add", "acme"], &env_pc), "space add");
    ok(&p.omoi(&web_pc, &["space", "assign", "acme"], &env_pc), "space assign");
    ok(&p.omoi(&web_pc, &["remember", "-k", "gotcha", "-t", "Las migraciones van antes del deploy", "Si no, el deploy arranca con el esquema viejo."], &env_pc), "remember pc");
    ok(&p.omoi(&pc, &["sync", "init", "--space", "acme", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    let codigo = ok(&p.omoi(&pc, &["sync", "code", "--space", "acme"], &env_pc), "code").trim().to_string();
    let clave = clave_de(&codigo);

    // La CI: sin carpeta de usuario, sin terminal, el código del espacio en la variable.
    let checkout = p.proyecto("checkout", "https://github.com/acme/web");
    let omoi_ci = |casa: &Path, nadie: bool, args: &[&str]| -> Output {
        let v: Vec<(&'static str, Option<String>)> = vec![
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(casa.to_string_lossy().into_owned())),
            ("OMOIKANE_NONINTERACTIVE", nadie.then(|| "1".to_string())),
            ("OMOIKANE_SYNC_CODE", Some(codigo.clone())),
            ("OMOIKANE_AUTHOR", Some("ci-bot".to_string())),
            ("OMOIKANE_EQUIPO", Some("ci".to_string())),
        ];
        p.omoi(&checkout, args, &prestadas(&v))
    };
    let ci = p.dir.join("ci-home");
    let o = omoi_ci(&ci, true, &["sync"]);
    let todo = salida(&o);
    ok(&o, "sync ci");
    assert!(!todo.contains("paused") && !todo.contains(&codigo) && !todo.contains(&clave), "sin pausa y sin el código: {todo}");
    // Trae lo del espacio: lo encuentra la búsqueda.
    let s = json(&omoi_ci(&ci, true, &["--json", "search", "migraciones deploy"]), "search");
    assert_eq!(s[0]["title"], "Las migraciones van antes del deploy", "{s}");
    // Lo que guarda va al espacio y llega al equipo de la persona.
    let m = json(&omoi_ci(&ci, true, &["--json", "remember", "-k", "fact", "-t", "La CI tarda 9 minutos", "El paso lento es el e2e."]), "remember ci");
    assert_eq!(m["author"], "ci-bot", "{m}");
    let j = json(&omoi_ci(&ci, true, &["--json", "sync"]), "sync ci 2");
    assert!(j["exportadas"].as_u64().or(j["exported"].as_u64()).is_some_and(|n| n >= 1), "{j}");
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc");
    let en_pc = json(&p.omoi(&web_pc, &["--json", "get", m["id"].as_str().unwrap()], &env_pc), "get pc");
    assert_eq!(en_pc["author"], "ci-bot", "{en_pc}");
    let s = json(&p.omoi(&web_pc, &["--json", "search", "CI tarda e2e"], &env_pc), "search pc");
    assert_eq!(s[0]["title"], "La CI tarda 9 minutos", "en el proyecto del espacio: {s}");

    // Otra ejecución, otra vez desde cero: igual, y ya trae lo que subió la anterior.
    let ci2 = p.dir.join("ci-home-2");
    ok(&omoi_ci(&ci2, true, &["sync"]), "sync ci2");
    let s = json(&omoi_ci(&ci2, true, &["--json", "search", "CI tarda e2e"]), "search ci2");
    assert_eq!(s[0]["title"], "La CI tarda 9 minutos", "{s}");

    // Con una persona delante (sin OMOIKANE_NONINTERACTIVE), la carpeta se queda en pausa: no trae nada.
    let persona = p.dir.join("con-persona");
    let o = omoi_ci(&persona, false, &["sync"]);
    assert!(salida(&o).contains("paused"), "{}", salida(&o));
    let s = json(&omoi_ci(&persona, false, &["--json", "search", "migraciones deploy"]), "search persona");
    assert_eq!(s.as_array().map(Vec::len), Some(0), "{s}");

    // Una CI cuya carpeta ya tenía memorias aquí antes de unirse (podrían ser de otro repositorio): en
    // pausa, y lo suyo no sube al espacio.
    let previa = p.dir.join("ci-previa");
    let v = [("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(previa.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_EQUIPO", Some("ci"))];
    ok(&p.omoi(&checkout, &["remember", "-k", "fact", "-t", "Una nota de antes de unirse", "Guardada sin espacio."], &v), "remember previa");
    let o = omoi_ci(&previa, true, &["sync"]);
    assert!(salida(&o).contains("paused"), "{}", salida(&o));
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc 2");
    let s = ok(&p.omoi(&web_pc, &["--json", "search", "nota antes unirse"], &env_pc), "search pc 2");
    assert!(!s.contains("Una nota de antes de unirse"), "lo de antes no sube: {s}");
}

/// Ya unida con `OMOIKANE_SYNC_URL`: si la variable trae otra URL (un token nuevo, el repositorio en
/// otro sitio), la siguiente sincronización la usa, en la configuración y en el `origin` del clon.
#[test]
fn otra_url_en_la_variable_se_apunta() {
    let p = Prueba::nueva("url");
    let url = repo_desnudo(&p, "memoria.git");
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env_pc = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    ok(&p.omoi(&pc, &["sync", "init", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    let codigo = ok(&p.omoi(&pc, &["sync", "code"], &env_pc), "code").trim().to_string();

    let ci = p.dir.join("ci");
    let checkout = p.proyecto("checkout", "https://github.com/acme/web");
    let omoi = |u: &str, args: &[&str]| -> Output {
        let v: Vec<(&'static str, Option<String>)> = vec![
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(ci.to_string_lossy().into_owned())),
            ("OMOIKANE_NONINTERACTIVE", Some("1".into())),
            ("OMOIKANE_SYNC_CODE", Some(codigo.clone())),
            ("OMOIKANE_SYNC_URL", Some(u.to_string())),
        ];
        p.omoi(&checkout, args, &prestadas(&v))
    };
    ok(&omoi(&url, &["sync"]), "sync 1");
    // El repositorio cambia de sitio: con la URL vieja ya no se llega.
    std::fs::rename(p.dir.join("memoria.git"), p.dir.join("nueva.git")).unwrap();
    let nueva = p.dir.join("nueva.git").to_string_lossy().replace('\\', "/");
    let o = omoi(&nueva, &["sync"]);
    ok(&o, "sync con la URL nueva");
    assert!(salida(&o).contains("URL"), "lo dice: {}", salida(&o));
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(ci.join("sync.json")).unwrap()).unwrap();
    assert_eq!(config["url"], nueva.as_str(), "{config}");
    let g = Command::new("git").args(["-C", ci.join("sync").to_str().unwrap(), "remote", "get-url", "origin"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&g.stdout).trim(), nueva, "el origin del clon");
    // Sin cambio, nada que decir.
    let o = omoi(&nueva, &["sync"]);
    ok(&o, "sync 3");
    assert!(!salida(&o).contains("URL"), "{}", salida(&o));
}

/// El autor con el que alguien se une al repositorio de un cliente firma solo lo de ese cliente: ni lo
/// de otro cliente (unido sin autor) ni lo que no es de ningún espacio, y quitar la sincronización del
/// primero no deja su nombre en nada. Sin repositorio general.
#[test]
fn el_autor_de_un_cliente_no_firma_lo_de_otro() {
    let p = Prueba::nueva("autores");
    let (url_a, url_b) = (repo_desnudo(&p, "acme.git"), repo_desnudo(&p, "beta.git"));
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_a = p.proyecto("web-a", "git@github.com:acme/web.git");
    let web_b = p.proyecto("web-b", "git@github.com:beta/web.git");
    let mio = p.proyecto("notas", "git@github.com:ana/notas.git");
    for (e, d) in [("acme", &web_a), ("beta", &web_b)] {
        ok(&p.omoi(&pc, &["space", "add", e], &env), "space add");
        ok(&p.omoi(d, &["space", "assign", e], &env), "space assign");
    }
    ok(&p.omoi(&pc, &["sync", "init", "--space", "acme", &url_a, "--yes", "--author", "Ana (Acme)"], &env), "init acme");
    ok(&p.omoi(&pc, &["sync", "init", "--space", "beta", &url_b, "--yes", "--no-author"], &env), "init beta");
    let autor = |d: &Path, t: &str| -> String {
        let m = json(&p.omoi(d, &["--json", "remember", "-k", "fact", "-t", t, "Algo que guardar aquí."], &env), "remember");
        m["author"].as_str().unwrap_or_default().to_string()
    };
    assert_eq!(autor(&web_a, "Nota de acme"), "Ana (Acme)");
    assert_eq!(autor(&web_b, "Nota de beta"), "", "beta no hereda el nombre de acme");
    assert_eq!(autor(&mio, "Nota mía"), "", "lo de ningún espacio tampoco");
    // Lo que viaja a beta no lleva el nombre de acme: otro equipo unido solo a beta no lo ve.
    ok(&p.omoi(&pc, &["sync"], &env), "sync");
    let codigo_b = ok(&p.omoi(&pc, &["sync", "code", "--space", "beta"], &env), "code beta").trim().to_string();
    let otra = p.dir.join("otra");
    let env_otra = [("OMOIKANE_HOME", Some(otra.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("otra")), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_SYNC_CODE", Some(codigo_b.as_str()))];
    ok(&p.omoi(&p.dir, &["sync"], &env_otra), "sync otra");
    let todo = ok(&p.omoi(&web_b, &["--json", "recent", "-n", "50"], &env_otra), "recent otra");
    assert!(todo.contains("Nota de beta") && !todo.contains("Acme") && !todo.contains("Nota de acme"), "{todo}");
    // Sin la sincronización de acme, su nombre ya no firma nada.
    ok(&p.omoi(&pc, &["sync", "off", "--space", "acme", "--yes"], &env), "off acme");
    assert_eq!(autor(&web_a, "Otra de acme"), "");
}
