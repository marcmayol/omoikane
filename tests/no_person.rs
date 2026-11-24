//! 1.7.0, agents with no person in front (CI, servers, containers): `omoi` with no user folder,
//! with everything in `OMOIKANE_HOME`, no questions (`OMOIKANE_NONINTERACTIVE=1`), joined to sync
//! through `OMOIKANE_SYNC_CODE` and signing with `OMOIKANE_AUTHOR`. The code and its key never show up in
//! the output. Everything in temp folders and with a local bare git repository: neither the real user
//! folder nor its `~/.omoikane` are touched (each command gets fake `HOME`/`USERPROFILE` or
//! none, and its own `OMOIKANE_HOME` or `OMOIKANE_DB`).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct TestEnv {
    dir: common::TempDir,
}

impl TestEnv {
    fn new(name: &str) -> TestEnv {
        let dir = common::TempDir::new(&format!("sinpersona-{name}"));
        std::fs::write(dir.join("gitconfig-vacio"), "").unwrap();
        TestEnv { dir }
    }

    /// `omoi` with just enough not to depend on the machine: git without the user's config, without
    /// inherited language, database or folders, and stdin closed. `env` adds or removes (value `None`).
    fn omoi(&self, cwd: &Path, args: &[&str], env: &[(&str, Option<&str>)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(cwd)
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

    /// A project folder with an `origin` remote.
    fn project(&self, name: &str, remote: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n")).unwrap();
        p
    }
}

fn ok(o: &Output, what: &str) -> String {
    assert!(o.status.success(), "{what}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn output(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn is_empty(d: &Path) -> bool {
    std::fs::read_dir(d).map(|mut x| x.next().is_none()).unwrap_or(true)
}

/// The key (in hex) carried by an `omoi sync code` code.
fn key_of(code: &str) -> String {
    use base64::Engine;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(code.trim().strip_prefix("omoi1.").unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    v["key"].as_str().unwrap().to_string()
}

/// `OMOIKANE_HOME` takes everything: the database, the models and the rest go there and not to the user
/// folder (here, a fake one, which must stay empty). And it also works with no user folder.
#[test]
fn everything_in_omoikane_home_and_no_user_folder() {
    let p = TestEnv::new("home");
    let fake = p.dir.join("usuario-falso");
    let home = p.dir.join("casa-omoikane");
    std::fs::create_dir_all(&fake).unwrap();
    let project = p.project("web", "git@github.com:acme/web.git");
    let f = fake.to_str().unwrap();
    let h = home.to_str().unwrap();
    let with_fake = [("HOME", Some(f)), ("USERPROFILE", Some(f)), ("OMOIKANE_HOME", Some(h))];
    ok(&p.omoi(&project, &["remember", "-k", "gotcha", "-t", "El build necesita Node 22", "Con Node 20 falla el bundler."], &with_fake), "remember");
    assert!(home.join("omoikane.db").exists(), "la base va a OMOIKANE_HOME");
    assert!(is_empty(&fake), "nada en la carpeta de usuario: {:?}", std::fs::read_dir(&fake).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
    let models = ok(&p.omoi(&project, &["model", "status"], &with_fake), "model status");
    assert!(Path::new(models.lines().last().unwrap_or_default().trim()).starts_with(&home), "los modelos también: {models}");
    ok(&p.omoi(&project, &["model", "semantic", "--off"], &with_fake), "semantic --off");
    assert!(home.join("semantic").exists() && is_empty(&fake), "y la preferencia del modelo de significado");

    // No HOME nor USERPROFILE: the same database, from OMOIKANE_HOME.
    let no_home = [("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(h))];
    let v: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&project, &["--json", "search", "Node bundler"], &no_home), "search")).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    ok(&p.omoi(&project, &["doctor"], &no_home), "doctor");
}

/// With no usable user folder and no variables, a clear error, without creating the database in the folder
/// it runs in. (A relative `HOME` is the way to test it on any system: Windows and
/// Linux get the user folder from elsewhere if the variable is missing.)
#[test]
fn no_folder_nor_variables_gives_a_clear_error() {
    let p = TestEnv::new("nada");
    let here = p.dir.join("aqui");
    std::fs::create_dir_all(&here).unwrap();
    let o = p.omoi(&here, &["remember", "-k", "fact", "-t", "Algo", "Algo que guardar."], &[("HOME", Some("relativa")), ("USERPROFILE", Some("relativa"))]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("OMOIKANE_HOME"), "{}", output(&o));
    assert!(is_empty(&here), "no deja una base en la carpeta en la que se ejecuta");
}

/// A CI: joins the repository with `OMOIKANE_SYNC_CODE`, pulls what is there, searches, saves with its
/// author and pushes it; the code and its key show up in no output; a network failure exits with an error.
#[test]
fn a_ci_joins_through_variables() {
    let p = TestEnv::new("ci");
    let g = Command::new("git").current_dir(&p.dir).args(["init", "--quiet", "--bare", "memoria.git"]).env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio")).output().unwrap();
    assert!(g.status.success());
    let url = p.dir.join("memoria.git").to_string_lossy().replace('\\', "/");

    // A person's machine: sets up sync and saves something from the project.
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let db_pc = pc.join("o.db");
    let env_pc = [("OMOIKANE_DB", Some(db_pc.to_str().unwrap())), ("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_pc = p.project("web-pc", "git@github.com:acme/web.git");
    ok(&p.omoi(&pc, &["sync", "init", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    ok(&p.omoi(&web_pc, &["remember", "-k", "gotcha", "-t", "Las migraciones van antes del deploy", "Si no, el deploy arranca con el esquema viejo."], &env_pc), "remember pc");
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc");
    let code = ok(&p.omoi(&pc, &["sync", "code"], &env_pc), "code").trim().to_string();
    let key = key_of(&code);

    // The CI: no user folder, no terminal, everything through variables.
    let ci = p.dir.join("ci-home");
    let web_ci = p.project("checkout", "https://github.com/acme/web");
    let env_ci = [
        ("HOME", None),
        ("USERPROFILE", None),
        ("OMOIKANE_HOME", Some(ci.to_str().unwrap())),
        ("OMOIKANE_NONINTERACTIVE", Some("1")),
        ("OMOIKANE_SYNC_CODE", Some(code.as_str())),
        ("OMOIKANE_AUTHOR", Some("ci-bot")),
        ("OMOIKANE_EQUIPO", Some("ci")),
    ];
    // `sync join` without a code: the one in the variable. What it prints carries neither the code nor the key.
    let o = p.omoi(&web_ci, &["sync", "join"], &env_ci);
    let all = output(&o);
    ok(&o, "join");
    assert!(!all.contains(&code) && !all.contains(&key), "el código no sale: {all}");
    assert!(ci.join("omoikane.db").exists() && ci.join("sync.json").exists());
    // Already joined: `--pull` reads the variable again, sees it already has that key and pulls whatever there is.
    ok(&p.omoi(&web_ci, &["sync", "--pull"], &env_ci), "pull");
    // The recall of a message, as a hook would do it (`USER_PROMPT`, text format).
    let mut with_prompt = env_ci.to_vec();
    with_prompt.push(("USER_PROMPT", Some("antes del deploy, hay que correr las migraciones?")));
    let r = ok(&p.omoi(&web_ci, &["recall", "--format", "plain"], &with_prompt), "recall");
    assert!(r.contains("Las migraciones van antes del deploy"), "{r}");
    let s: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&web_ci, &["--json", "search", "migraciones deploy"], &env_ci), "search")).unwrap();
    assert_eq!(s[0]["title"], "Las migraciones van antes del deploy", "{s}");

    // What the CI saves is signed by `OMOIKANE_AUTHOR` and reaches the person's machine.
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
    let on_pc: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&web_pc, &["--json", "get", m["id"].as_str().unwrap()], &env_pc), "get")).unwrap();
    assert_eq!(on_pc["author"], "ci-bot", "{on_pc}");

    // No network (the repository is gone), `--pull` with no one in front exits with an error, without the key.
    std::fs::rename(p.dir.join("memoria.git"), p.dir.join("movida.git")).unwrap();
    let o = p.omoi(&web_ci, &["sync", "--pull"], &env_ci);
    assert!(!o.status.success(), "{}", output(&o));
    assert!(!output(&o).contains(&key));
    // The `--pull` that `omoi` itself launches in the background (`OMOIKANE_SYNC_AUTO=1` on a server) is still
    // the cheap one even with no one in front: with no network it doesn't fail, it will try later.
    let mut in_background = env_ci.to_vec();
    in_background.push(("OMOIKANE_INTERNAL_BACKGROUND", Some("1")));
    let o = p.omoi(&web_ci, &["sync", "--pull"], &in_background);
    assert!(o.status.success(), "{}", output(&o));
    std::fs::rename(p.dir.join("movida.git"), p.dir.join("memoria.git")).unwrap();

    // A bad code or an unreachable URL: clear error, without the code or the key.
    let other = p.dir.join("ci-2");
    let bad = format!("{code}x");
    let o = p.omoi(&web_ci, &["sync"], &[("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(other.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_SYNC_CODE", Some(bad.as_str()))]);
    assert!(!o.status.success() && output(&o).contains("OMOIKANE_SYNC_CODE") && !output(&o).contains(&key), "{}", output(&o));
    let o = p.omoi(
        &web_ci,
        &["sync"],
        &[
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(other.to_str().unwrap())),
            ("OMOIKANE_NONINTERACTIVE", Some("1")),
            ("OMOIKANE_SYNC_CODE", Some(code.as_str())),
            ("OMOIKANE_SYNC_URL", Some("https://ci:s3cr3t-t0ken@127.0.0.1:9/no/existe.git")),
        ],
    );
    let all = output(&o);
    assert!(!o.status.success(), "{all}");
    assert!(!all.contains(&key) && !all.contains("s3cr3t-t0ken"), "ni la clave ni el token: {all}");
}

/// With no one in front it doesn't ask even if confirmation is needed: `--yes` is required.
#[test]
fn with_no_one_in_front_it_does_not_ask() {
    let p = TestEnv::new("preguntas");
    let home = p.dir.join("casa");
    let env = [("OMOIKANE_HOME", Some(home.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1"))];
    let pr = p.project("web", "git@github.com:acme/web.git");
    let m: serde_json::Value = serde_json::from_str(&ok(&p.omoi(&pr, &["--json", "remember", "-k", "fact", "-t", "Algo", "Algo que olvidar."], &env), "remember")).unwrap();
    let o = p.omoi(&pr, &["forget", m["id"].as_str().unwrap()], &env);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--yes"), "{}", output(&o));
}

/// 1.7.1: with no one in front, neither saving nor searching starts the meaning service (it would live half
/// an hour longer than the job). With a person, the same save does leave it starting: the test sees something.
#[test]
fn with_no_one_in_front_the_meaning_service_does_not_start() {
    let p = TestEnv::new("significado");
    // A «model» with the files of the compact format, empty: enough for it to count as installed. The
    // service launched with it fails to load it and exits.
    let model = p.dir.join("modelo");
    std::fs::create_dir_all(&model).unwrap();
    for f in ["config.json", "tokenizer.json", "pesos.safetensors", "tokens.i8", "tokens.escala"] {
        std::fs::write(model.join(f), "").unwrap();
    }
    let pr = p.project("web", "git@github.com:acme/web.git");
    let starting = |home: &Path| home.join("significado.arrancando").exists();
    let home = p.dir.join("casa");
    let env = [
        ("OMOIKANE_HOME", Some(home.to_str().unwrap())),
        ("OMOIKANE_NONINTERACTIVE", Some("1")),
        ("OMOIKANE_SEMANTIC", Some("1")),
        ("OMOIKANE_SEMANTIC_MODEL", Some(model.to_str().unwrap())),
    ];
    ok(&p.omoi(&pr, &["remember", "-k", "fact", "-t", "Despliegue", "Se publica con el script de la CI."], &env), "remember");
    ok(&p.omoi(&pr, &["search", "publicar"], &env), "search");
    assert!(!starting(&home), "sin nadie delante no se arranca el servicio");
    assert!(!home.join("significado.json").exists());

    let other = p.dir.join("otra");
    let env = [
        ("OMOIKANE_HOME", Some(other.to_str().unwrap())),
        ("OMOIKANE_SEMANTIC", Some("1")),
        ("OMOIKANE_SEMANTIC_MODEL", Some(model.to_str().unwrap())),
    ];
    ok(&p.omoi(&pr, &["remember", "-k", "fact", "-t", "Despliegue", "Se publica con el script de la CI."], &env), "remember");
    assert!(starting(&other), "con una persona delante sí se deja arrancando");
}

/// A bare git repository in the test folder; its URL.
fn bare_repo(p: &TestEnv, name: &str) -> String {
    let g = Command::new("git").current_dir(&p.dir).args(["init", "--quiet", "--bare", name]).env("GIT_CONFIG_GLOBAL", p.dir.join("gitconfig-vacio")).output().unwrap();
    assert!(g.status.success());
    p.dir.join(name).to_string_lossy().replace('\\', "/")
}

fn json(o: &Output, what: &str) -> serde_json::Value {
    serde_json::from_str(&ok(o, what)).unwrap_or_else(|e| panic!("{what}: {e}"))
}

/// The variables as `(&str, Option<&str>)`, which is what `TestEnv::omoi` takes.
fn borrowed<'a>(v: &'a [(&'static str, Option<String>)]) -> Vec<(&'static str, Option<&'a str>)> {
    v.iter().map(|(k, x)| (*k, x.as_deref())).collect()
}

/// A CI that joins a client's SPACE through `OMOIKANE_SYNC_CODE` starts from a fresh database: its
/// folder is a project that the space's repository says is its own and that this machine didn't have. With
/// no one in front, the variable is the decision: it enters the space without pausing, pulls what is there, searches,
/// saves and pushes it, and the person's machine receives it. With a person in front (without
/// `OMOIKANE_NONINTERACTIVE`), or if the folder already had memories here, it stays paused.
#[test]
fn a_ci_joins_a_space_through_variables() {
    let p = TestEnv::new("ci-espacio");
    let url = bare_repo(&p, "acme.git");

    // The person's machine: the project in the acme space, with its repository.
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env_pc = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_pc = p.project("web-pc", "git@github.com:acme/web.git");
    ok(&p.omoi(&pc, &["space", "add", "acme"], &env_pc), "space add");
    ok(&p.omoi(&web_pc, &["space", "assign", "acme"], &env_pc), "space assign");
    ok(&p.omoi(&web_pc, &["remember", "-k", "gotcha", "-t", "Las migraciones van antes del deploy", "Si no, el deploy arranca con el esquema viejo."], &env_pc), "remember pc");
    ok(&p.omoi(&pc, &["sync", "init", "--space", "acme", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    let code = ok(&p.omoi(&pc, &["sync", "code", "--space", "acme"], &env_pc), "code").trim().to_string();
    let key = key_of(&code);

    // The CI: no user folder, no terminal, the space's code in the variable.
    let checkout = p.project("checkout", "https://github.com/acme/web");
    let omoi_ci = |home: &Path, unattended: bool, args: &[&str]| -> Output {
        let v: Vec<(&'static str, Option<String>)> = vec![
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(home.to_string_lossy().into_owned())),
            ("OMOIKANE_NONINTERACTIVE", unattended.then(|| "1".to_string())),
            ("OMOIKANE_SYNC_CODE", Some(code.clone())),
            ("OMOIKANE_AUTHOR", Some("ci-bot".to_string())),
            ("OMOIKANE_EQUIPO", Some("ci".to_string())),
        ];
        p.omoi(&checkout, args, &borrowed(&v))
    };
    let ci = p.dir.join("ci-home");
    let o = omoi_ci(&ci, true, &["sync"]);
    let all = output(&o);
    ok(&o, "sync ci");
    assert!(!all.contains("paused") && !all.contains(&code) && !all.contains(&key), "sin pausa y sin el código: {all}");
    // It pulls the space's content: the search finds it.
    let s = json(&omoi_ci(&ci, true, &["--json", "search", "migraciones deploy"]), "search");
    assert_eq!(s[0]["title"], "Las migraciones van antes del deploy", "{s}");
    // What it saves goes to the space and reaches the person's machine.
    let m = json(&omoi_ci(&ci, true, &["--json", "remember", "-k", "fact", "-t", "La CI tarda 9 minutos", "El paso lento es el e2e."]), "remember ci");
    assert_eq!(m["author"], "ci-bot", "{m}");
    let j = json(&omoi_ci(&ci, true, &["--json", "sync"]), "sync ci 2");
    assert!(j["exportadas"].as_u64().or(j["exported"].as_u64()).is_some_and(|n| n >= 1), "{j}");
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc");
    let on_pc = json(&p.omoi(&web_pc, &["--json", "get", m["id"].as_str().unwrap()], &env_pc), "get pc");
    assert_eq!(on_pc["author"], "ci-bot", "{on_pc}");
    let s = json(&p.omoi(&web_pc, &["--json", "search", "CI tarda e2e"], &env_pc), "search pc");
    assert_eq!(s[0]["title"], "La CI tarda 9 minutos", "en el proyecto del espacio: {s}");

    // Another run, again from scratch: the same, and it already pulls what the previous one pushed.
    let ci2 = p.dir.join("ci-home-2");
    ok(&omoi_ci(&ci2, true, &["sync"]), "sync ci2");
    let s = json(&omoi_ci(&ci2, true, &["--json", "search", "CI tarda e2e"]), "search ci2");
    assert_eq!(s[0]["title"], "La CI tarda 9 minutos", "{s}");

    // With a person in front (without OMOIKANE_NONINTERACTIVE), the folder stays paused: it pulls nothing.
    let person = p.dir.join("con-persona");
    let o = omoi_ci(&person, false, &["sync"]);
    assert!(output(&o).contains("paused"), "{}", output(&o));
    let s = json(&omoi_ci(&person, false, &["--json", "search", "migraciones deploy"]), "search persona");
    assert_eq!(s.as_array().map(Vec::len), Some(0), "{s}");

    // A CI whose folder already had memories here before joining (they could be from another repository):
    // paused, and its own content doesn't go up to the space.
    let earlier = p.dir.join("ci-previa");
    let v = [("HOME", None), ("USERPROFILE", None), ("OMOIKANE_HOME", Some(earlier.to_str().unwrap())), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_EQUIPO", Some("ci"))];
    ok(&p.omoi(&checkout, &["remember", "-k", "fact", "-t", "Una nota de antes de unirse", "Guardada sin espacio."], &v), "remember previa");
    let o = omoi_ci(&earlier, true, &["sync"]);
    assert!(output(&o).contains("paused"), "{}", output(&o));
    ok(&p.omoi(&pc, &["sync"], &env_pc), "sync pc 2");
    let s = ok(&p.omoi(&web_pc, &["--json", "search", "nota antes unirse"], &env_pc), "search pc 2");
    assert!(!s.contains("Una nota de antes de unirse"), "lo de antes no sube: {s}");
}

/// Already joined with `OMOIKANE_SYNC_URL`: if the variable brings another URL (a new token, the repository
/// somewhere else), the next sync uses it, in the config and in the clone's `origin`.
#[test]
fn another_url_in_the_variable_is_recorded() {
    let p = TestEnv::new("url");
    let url = bare_repo(&p, "memoria.git");
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env_pc = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    ok(&p.omoi(&pc, &["sync", "init", &url, "--yes", "--author", "Ana"], &env_pc), "init");
    let code = ok(&p.omoi(&pc, &["sync", "code"], &env_pc), "code").trim().to_string();

    let ci = p.dir.join("ci");
    let checkout = p.project("checkout", "https://github.com/acme/web");
    let omoi = |u: &str, args: &[&str]| -> Output {
        let v: Vec<(&'static str, Option<String>)> = vec![
            ("HOME", None),
            ("USERPROFILE", None),
            ("OMOIKANE_HOME", Some(ci.to_string_lossy().into_owned())),
            ("OMOIKANE_NONINTERACTIVE", Some("1".into())),
            ("OMOIKANE_SYNC_CODE", Some(code.clone())),
            ("OMOIKANE_SYNC_URL", Some(u.to_string())),
        ];
        p.omoi(&checkout, args, &borrowed(&v))
    };
    ok(&omoi(&url, &["sync"]), "sync 1");
    // The repository moves: the old URL no longer reaches it.
    std::fs::rename(p.dir.join("memoria.git"), p.dir.join("nueva.git")).unwrap();
    let new_url = p.dir.join("nueva.git").to_string_lossy().replace('\\', "/");
    let o = omoi(&new_url, &["sync"]);
    ok(&o, "sync con la URL nueva");
    assert!(output(&o).contains("URL"), "lo dice: {}", output(&o));
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(ci.join("sync.json")).unwrap()).unwrap();
    assert_eq!(config["url"], new_url.as_str(), "{config}");
    let g = Command::new("git").args(["-C", ci.join("sync").to_str().unwrap(), "remote", "get-url", "origin"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&g.stdout).trim(), new_url, "el origin del clon");
    // No change, nothing to say.
    let o = omoi(&new_url, &["sync"]);
    ok(&o, "sync 3");
    assert!(!output(&o).contains("URL"), "{}", output(&o));
}

/// The author someone joins a client's repository with signs only that client's content: neither
/// another client's (joined without an author) nor what belongs to no space, and removing the first one's
/// sync leaves its name on nothing. No general repository.
#[test]
fn one_clients_author_does_not_sign_anothers() {
    let p = TestEnv::new("autores");
    let (url_a, url_b) = (bare_repo(&p, "acme.git"), bare_repo(&p, "beta.git"));
    let pc = p.dir.join("pc");
    std::fs::create_dir_all(&pc).unwrap();
    let env = [("OMOIKANE_HOME", Some(pc.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("pc"))];
    let web_a = p.project("web-a", "git@github.com:acme/web.git");
    let web_b = p.project("web-b", "git@github.com:beta/web.git");
    let mine = p.project("notas", "git@github.com:ana/notas.git");
    for (e, d) in [("acme", &web_a), ("beta", &web_b)] {
        ok(&p.omoi(&pc, &["space", "add", e], &env), "space add");
        ok(&p.omoi(d, &["space", "assign", e], &env), "space assign");
    }
    ok(&p.omoi(&pc, &["sync", "init", "--space", "acme", &url_a, "--yes", "--author", "Ana (Acme)"], &env), "init acme");
    ok(&p.omoi(&pc, &["sync", "init", "--space", "beta", &url_b, "--yes", "--no-author"], &env), "init beta");
    let author = |d: &Path, t: &str| -> String {
        let m = json(&p.omoi(d, &["--json", "remember", "-k", "fact", "-t", t, "Algo que guardar aquí."], &env), "remember");
        m["author"].as_str().unwrap_or_default().to_string()
    };
    assert_eq!(author(&web_a, "Nota de acme"), "Ana (Acme)");
    assert_eq!(author(&web_b, "Nota de beta"), "", "beta no hereda el nombre de acme");
    assert_eq!(author(&mine, "Nota mía"), "", "lo de ningún espacio tampoco");
    // What travels to beta doesn't carry acme's name: another machine joined only to beta doesn't see it.
    ok(&p.omoi(&pc, &["sync"], &env), "sync");
    let code_b = ok(&p.omoi(&pc, &["sync", "code", "--space", "beta"], &env), "code beta").trim().to_string();
    let other = p.dir.join("otra");
    let env_other = [("OMOIKANE_HOME", Some(other.to_str().unwrap())), ("OMOIKANE_EQUIPO", Some("otra")), ("OMOIKANE_NONINTERACTIVE", Some("1")), ("OMOIKANE_SYNC_CODE", Some(code_b.as_str()))];
    ok(&p.omoi(&p.dir, &["sync"], &env_other), "sync otra");
    let all = ok(&p.omoi(&web_b, &["--json", "recent", "-n", "50"], &env_other), "recent otra");
    assert!(all.contains("Nota de beta") && !all.contains("Acme") && !all.contains("Nota de acme"), "{all}");
    // Without acme's sync, its name no longer signs anything.
    ok(&p.omoi(&pc, &["sync", "off", "--space", "acme", "--yes"], &env), "off acme");
    assert_eq!(author(&web_a, "Otra de acme"), "");
}
