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
