//! Compatibility with 1.0.0 (Phase 9, §9.1): a pinned `omoi` 1.0.0 and the one from this commit share a
//! local bare git repository, a database and the `omoi export` backups, and must understand each other in
//! both directions. This is what 1.0 promises: the database, the sync line and the
//! export do not change incompatibly within 1.x; a 1.0 keeps and forwards what it does not understand
//! (`extra`), counts lines of a new type separately, steps away from a repository that asks for a newer
//! reader (`omoikane.format`) and opens a database that only adds (`lector_minimo`).
//!
//! The 1.0.0 goes in `OMOIKANE_COMPAT_BIN` (the path to its `omoi`); without it, each test is skipped with a
//! warning. CI (`.github/workflows/compat.yml`) downloads it from the v1.0.0 Release, checks its SHA-256 and
//! runs `cargo test --test compat` on Windows, Linux and macOS.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The `omoi` from this commit.
const NEW: &str = env!("CARGO_BIN_EXE_omoi");

/// The `omoi` 1.0.0 from `OMOIKANE_COMPAT_BIN`, or `None` (and a warning) if it is not there. If it is, it must be
/// 1.0.0: with another binary the test would prove nothing.
fn binary_10() -> Option<PathBuf> {
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

struct Harness {
    dir: common::TempDir,
}

impl Harness {
    fn new(name: &str) -> Harness {
        let dir = common::TempDir::new(&format!("compat-{name}"));
        std::fs::write(dir.join("gitconfig-vacio"), "").unwrap();
        let p = Harness { dir };
        p.another_repo("compartida");
        p
    }

    fn url(&self) -> String {
        self.dir.join("compartida.git").to_string_lossy().replace('\\', "/")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    /// A bare repository (the general one or a space's). Returns its URL.
    fn another_repo(&self, name: &str) -> String {
        let o = self.git(&self.dir, &["init", "--quiet", "--bare", &format!("{name}.git")]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        self.dir.join(format!("{name}.git")).to_string_lossy().replace('\\', "/")
    }

    fn commits(&self) -> String {
        String::from_utf8_lossy(&self.git(&self.dir.join("compartida.git"), &["rev-list", "--count", "--all"]).stdout).trim().to_string()
    }

    /// A machine with its database, with the given `omoi` and `OMOIKANE_EQUIPO` = `name`.
    fn machine(&self, name: &str, bin: &Path) -> Machine {
        self.machine_in(name, name, bin)
    }

    /// The same, with the folder (and the database) separate from the name: two binaries on the same database, or
    /// two databases with the same machine name.
    fn machine_in(&self, folder: &str, name: &str, bin: &Path) -> Machine {
        let dir = self.dir.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        Machine { dir, name: name.into(), root: self.dir.clone(), bin: bin.to_path_buf() }
    }

    /// A fresh clone of a repository, to look at.
    fn clone_of(&self, url: &str) -> PathBuf {
        let clone = self.dir.join(format!("mirar-{}", ulid::Ulid::generate()));
        let o = self.git(&self.dir, &["clone", "--quiet", "--branch", "main", url, clone.to_str().unwrap()]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        clone
    }

    /// The lines of a repository's memory files.
    fn lines_of(&self, url: &str) -> Vec<String> {
        let clone = self.clone_of(url);
        let mut v = Vec::new();
        for e in std::fs::read_dir(&clone).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                continue;
            }
            for f in std::fs::read_dir(e.path()).unwrap().flatten() {
                v.extend(std::fs::read_to_string(f.path()).unwrap().lines().filter(|l| !l.is_empty()).map(String::from));
            }
        }
        v
    }

    /// Pushes a hand-written file to the general repository (what a newer 1.x would write).
    fn push_by_hand(&self, path: &str, text: &str) {
        let clone = self.clone_of(&self.url());
        let f = clone.join(path);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, text).unwrap();
        for args in [vec!["add", "-A"], vec!["-c", "user.name=x", "-c", "user.email=x@x", "commit", "--quiet", "-m", "a mano"], vec!["push", "--quiet", "origin", "HEAD:main"]] {
            let o = self.git(&clone, &args);
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
}

struct Machine {
    dir: PathBuf,
    name: String,
    root: PathBuf,
    bin: PathBuf,
}

impl Machine {
    fn is_the_commit(&self) -> bool {
        self.bin == Path::new(NEW)
    }

    /// Who it is, for messages: the name and whether it is 1.0.0 or the commit.
    fn who(&self) -> String {
        format!("{} ({})", self.name, if self.is_the_commit() { "commit" } else { "1.0.0" })
    }

    /// A project folder with an `origin` remote.
    fn project(&self, folder: &str, remote: &str) -> PathBuf {
        let p = self.dir.join(folder);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        std::fs::write(p.join(".git").join("config"), format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n")).unwrap();
        p
    }

    /// This machine's `omoi`, with nothing from the host: no self-update, no daily backups, no
    /// background sync.
    fn command(&self, cwd: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.current_dir(cwd)
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_EQUIPO", &self.name)
            .env("OMOIKANE_SYNC_AUTO", "0")
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig-vacio"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_SOURCE");
        common::without_session_vars(&mut c);
        c
    }

    fn omoi_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd).args(args).stdin(Stdio::null()).output().unwrap()
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.omoi_in(cwd, args);
        assert!(o.status.success(), "{} omoi {args:?}: {}{}", self.who(), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.dir, args)
    }

    fn json_in(&self, cwd: &Path, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok_in(cwd, &a)).unwrap()
    }

    fn save(&self, cwd: &Path, kind: &str, title: &str, text: &str, extra: &[&str]) -> String {
        let mut a = vec!["remember", "-k", kind, "-t", title];
        a.extend_from_slice(extra);
        a.push(text);
        self.json_in(cwd, &a)["id"].as_str().unwrap().to_string()
    }

    /// `omoi sync`; between the two versions nothing may be left unreadable.
    fn sync(&self) -> serde_json::Value {
        self.sync_in(&self.dir)
    }

    /// The same from a folder (a project's: that way this machine knows it).
    fn sync_in(&self, cwd: &Path) -> serde_json::Value {
        let r = self.json_in(cwd, &["sync"]);
        assert_eq!(r["ilegibles"].as_u64(), Some(0), "{}: nada ilegible entre versiones: {r}", self.who());
        r
    }

    /// An MCP tool (`omoi tool <name>`) with its JSON arguments.
    fn tool_in(&self, cwd: &Path, name: &str, args: &str) -> serde_json::Value {
        let mut child = self
            .command(cwd)
            .args(["tool", name])
            .env("OMOIKANE_SOURCE", "mcp:claude-code")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(args.as_bytes()).unwrap();
        }
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{} omoi tool {name}: {}{}", self.who(), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    fn get(&self, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi_in(&self.dir, &["--json", "get", id]);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.join("o.db")).unwrap()
    }
}

/// A sets up the general repository and B joins with the code, each with its own person.
fn two(p: &Harness, bin_a: &Path, bin_b: &Path) -> (Machine, Machine) {
    let a = p.machine("pc-a", bin_a);
    let b = p.machine("pc-b", bin_b);
    a.ok(&["sync", "init", &p.url(), "--yes", "--author", "Ana"]);
    let code = a.ok(&["sync", "code"]).trim().to_string();
    b.ok(&["sync", "join", &code, "--yes", "--author", "Marc"]);
    (a, b)
}

/// The sync person of each machine from `two`.
fn person(e: &Machine) -> &'static str {
    if e.name == "pc-a" { "Ana" } else { "Marc" }
}

// --- Encryption, the way Omoikane does it -----------------------------------------------------------

/// The key of a code from `omoi sync code`.
fn key_from_code(code: &str) -> Vec<u8> {
    use base64::Engine;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(code.trim().strip_prefix("omoi1.").unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let h = v["key"].as_str().unwrap();
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
}

/// A line encrypted with key 1 (no prefix), with whatever you want inside.
fn encrypt_line(key: &[u8], json: &serde_json::Value) -> String {
    use aes_siv::KeyInit;
    use base64::Engine;
    let c = aes_siv::siv::Aes256Siv::new_from_slice(key).unwrap().encrypt([b"omoikane-sync-v1".as_slice()], json.to_string().as_bytes()).unwrap();
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(c)
}

/// What a line encrypted with key 1 carries inside, or `None` if it is not from that key (another
/// label, another key).
fn decrypt_line(key: &[u8], line: &str) -> Option<serde_json::Value> {
    use aes_siv::KeyInit;
    use base64::Engine;
    let c = base64::engine::general_purpose::STANDARD_NO_PAD.decode(line.trim()).ok()?;
    let t = aes_siv::siv::Aes256Siv::new_from_slice(key).unwrap().decrypt([b"omoikane-sync-v1".as_slice()], &c).ok()?;
    serde_json::from_slice(&t).ok()
}

/// The `min_reader` from the general repository's `omoikane.format` (1 if it is not there yet).
fn min_reader(p: &Harness, key: &[u8]) -> u64 {
    std::fs::read_to_string(p.clone_of(&p.url()).join("omoikane.format"))
        .ok()
        .and_then(|t| decrypt_line(key, &t))
        .and_then(|v| v["min_reader"].as_u64())
        .unwrap_or(1)
}

/// If the commit has raised the repository's `min_reader`, 1.0 cannot write to it (and that is
/// expected): it checks that it steps away with a clear message and without touching anything, and the test of
/// that repository ends there. Returns `true` if that is the case.
fn v10_steps_away(p: &Harness, old: &Machine, key: &[u8]) -> bool {
    let m = min_reader(p, key);
    if m <= 1 {
        return false;
    }
    eprintln!("compat: el commit pide min_reader {m}: la 1.0 no debe tocar el repositorio");
    let commits = p.commits();
    old.save(&old.dir, "fact", "No debe subir", "Nada.", &["--global"]);
    let o = old.omoi_in(&old.dir, &["sync"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs a newer version of omoi"), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(p.commits(), commits, "no escribe nada en él");
    true
}

// --- Syncing through the same repository -------------------------------------------------------------

/// `from` writes (memories with history, a verdict with a reason, a forget) and `to` receives it all; what
/// `to` takes out of the trash comes back to `from`.
fn from_one_machine_to_another(from: &Machine, to: &Machine) {
    let remote = "git@github.com:marc/tienda.git";
    let (pd, pa) = (from.project("tienda", remote), to.project("tienda", remote));
    let q = from.who();
    let cookie = from.save(&pd, "decision", &format!("Sesiones por cookie, de {q}"), "La autenticación usa una cookie de sesión.", &[]);
    from.ok_in(&pd, &["update", &cookie, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    let jwt = from.save(&pd, "decision", &format!("Autenticación con JWT, de {q}"), "Se pasó a JWT en la cabecera Authorization.", &[]);
    from.tool_in(&pd, "update", &format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}"));
    let forgotten = from.save(&from.dir, "fact", &format!("El puerto de pagos, de {q}"), "Escucha en el 8080.", &["--global"]);
    from.sync_in(&pd);
    to.sync_in(&pa);
    assert!(to.get(&forgotten).is_some(), "{}: llega de {q}", to.who());
    from.ok(&["forget", &forgotten, "--yes"]);
    from.sync_in(&pd);
    to.sync_in(&pa);

    // The memories, in this machine's project folder and with their history.
    let m = to.get(&cookie).unwrap_or_else(|| panic!("{}: falta la memoria de {q}", to.who()));
    assert_eq!(m["content"], "La autenticación usaba una cookie de sesión; ver JWT.");
    let path_a = pa.canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\").to_string();
    assert_eq!(m["project"].as_str(), Some(path_a.as_str()), "en la carpeta de {}: {m}", to.who());
    let h = to.json_in(&pa, &["history", &cookie]);
    assert_eq!(h.as_array().unwrap().len(), 1, "{}: el historial viaja: {h}", to.who());
    // The verdict, with its reason and its author.
    let rev = to.json_in(&pa, &["review", "--all"]);
    let av = rev
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "superseded" && x["memory_id"] == cookie.as_str())
        .unwrap_or_else(|| panic!("{}: falta el veredicto de {q}: {rev}", to.who()));
    assert_eq!((av["other_id"].as_str(), av["reason"].as_str(), av["author"].as_str()), (Some(jwt.as_str()), Some("JWT replaced the cookie"), Some(person(from))), "{av}");
    // The forget arrives; what was forgotten is recovered from the other machine and comes back.
    assert!(to.get(&forgotten).is_none(), "{}: el olvido de {q} llega", to.who());
    assert!(to.ok(&["sync", "restore"]).contains("El puerto de pagos"), "{}: en la papelera de la sync", to.who());
    to.ok(&["sync", "restore", &forgotten]);
    assert_eq!(to.get(&forgotten).unwrap()["content"], "Escucha en el 8080.");
    to.sync_in(&pa);
    from.sync_in(&pd);
    assert!(from.get(&forgotten).is_some(), "{q}: la recuperada en {} vuelve", to.who());
    assert!(!from.ok(&["trash"]).contains("El puerto de pagos"), "{q}: y sale de su papelera");
}

fn round_trip(bin_a: &Path, bin_b: &Path, name: &str) {
    let p = Harness::new(name);
    let (a, b) = two(&p, bin_a, bin_b);
    let key = key_from_code(&a.ok(&["sync", "code"]));
    a.save(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    let old = if a.is_the_commit() { &b } else { &a };
    if v10_steps_away(&p, old, &key) {
        return;
    }
    from_one_machine_to_another(&a, &b);
    from_one_machine_to_another(&b, &a);
}

#[test]
fn the_commit_sets_up_and_10_joins() {
    let Some(v10) = binary_10() else { return };
    round_trip(Path::new(NEW), &v10, "commit-10");
}

#[test]
fn v10_sets_up_and_the_commit_joins() {
    let Some(v10) = binary_10() else { return };
    round_trip(&v10, Path::new(NEW), "10-commit");
}

/// A repository for a space (with its key) and key rotation, with each binary on one side.
fn spaces_and_rotation(bin_a: &Path, bin_b: &Path, name: &str) {
    let p = Harness::new(name);
    let (a, b) = two(&p, bin_a, bin_b);
    let acme = p.another_repo("acme");
    a.ok(&["sync", "init", "--space", "acme", &acme, "--yes"]);
    let code = a.ok(&["sync", "code", "--space", "acme"]).trim().to_string();
    b.ok(&["sync", "join", &code, "--yes"]);
    a.ok(&["space", "add", "acme"]);
    let web = a.project("acme-web", "git@github.com:acme/web.git");
    a.ok_in(&web, &["space", "assign", "acme"]);
    let acme_one = a.save(&web, "decision", "Acme factura en euros", "Siempre con IVA.", &["--space=acme"]);
    let project_one = a.save(&web, "fact", "El staging de Acme está en Frankfurt", "Región eu-central-1.", &[]);
    let general = a.save(&a.dir, "fact", "Mi editor es Helix", "Con el tema por defecto.", &["--global"]);
    a.sync();
    b.sync();
    assert_eq!(b.get(&acme_one).unwrap_or_else(|| panic!("{}: falta la de acme", b.who()))["space"], "acme");
    assert_eq!(b.get(&project_one).unwrap_or_else(|| panic!("{}: falta la del proyecto de acme", b.who()))["space"], "acme");
    assert!(b.get(&general).is_some());
    // In acme's repository, only acme's stuff.
    let acme_key = key_from_code(&code);
    let lines = p.lines_of(&acme);
    assert!(!lines.is_empty());
    for l in lines {
        let j = decrypt_line(&acme_key, &l).unwrap_or_else(|| panic!("una línea de acme que no abre su clave: {l}"));
        assert_eq!(j["space_key"], "acme", "{j}");
    }
    // What B writes in acme comes back to A through acme's.
    let from_b = b.save(&b.dir, "gotcha", "La VPN de Acme corta a la hora", "Volver a entrar.", &["--space=acme"]);
    b.sync();
    a.sync();
    assert_eq!(a.get(&from_b).unwrap_or_else(|| panic!("{}: falta la de {}", a.who(), b.who()))["space"], "acme");

    // A rotates the general key: B, with the old one, does not read the new stuff until it joins with the new code.
    let r = a.json_in(&a.dir, &["sync", "rotate", "--yes"]);
    assert_eq!(r["key"], 2, "{r}");
    let new_code = a.ok(&["sync", "code"]).trim().to_string();
    let after_rotating = a.save(&a.dir, "decision", "Las copias van a Atlas", "Cada noche a las tres.", &["--global"]);
    a.sync();
    let r = b.json_in(&b.dir, &["sync"]);
    assert!(b.get(&after_rotating).is_none() && r["ilegibles"].as_u64().unwrap() > 0, "{}: {r}", b.who());
    assert!(b.ok(&["sync", "status"]).contains("key has changed"));
    b.ok(&["sync", "join", &new_code, "--yes"]);
    assert!(b.get(&after_rotating).is_some(), "{}: con la clave nueva", b.who());
    let from_b = b.save(&b.dir, "fact", "Atlas se duerme a los 20 minutos", "Wake-on-lan.", &["--global"]);
    b.sync();
    a.sync();
    assert!(a.get(&from_b).is_some(), "{}: lo de {} tras rotar", a.who(), b.who());

    // B rotates acme's, and A does what B did.
    let r = b.json_in(&b.dir, &["sync", "rotate", "--space", "acme", "--yes"]);
    assert_eq!(r["key"], 2, "{r}");
    let new_acme_code = r["code"].as_str().unwrap().to_string();
    let after_rotating = b.save(&b.dir, "fact", "Acme cierra en agosto", "Todo el mes.", &["--space=acme"]);
    b.sync();
    let r = a.json_in(&a.dir, &["sync"]);
    assert!(a.get(&after_rotating).is_none(), "{}: {r}", a.who());
    a.ok(&["sync", "join", &new_acme_code, "--yes"]);
    assert_eq!(a.get(&after_rotating).unwrap_or_else(|| panic!("{}: con la clave nueva de acme", a.who()))["space"], "acme");
    a.sync();
    b.sync();
}

#[test]
fn spaces_and_rotation_the_commit_sets_up() {
    let Some(v10) = binary_10() else { return };
    spaces_and_rotation(Path::new(NEW), &v10, "esp-commit-10");
}

#[test]
fn spaces_and_rotation_10_sets_up() {
    let Some(v10) = binary_10() else { return };
    spaces_and_rotation(&v10, Path::new(NEW), "esp-10-commit");
}

const HLC_FUTURE: &str = "1761696000000-0000000000-pc-futuro";

/// A newer 1.x writes to the repository a memory, a verdict and a forget with a field that
/// 1.0 does not know, and lines of a new type. 1.0 counts them separately (not as unreadable), changes
/// the memory and pushes it again with the field, and does not touch the other one's file; the commit receives it
/// with the field.
#[test]
fn what_a_1x_writes_passes_through_10_without_loss() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("futuro");
    let (a, old) = two(&p, Path::new(NEW), &v10);
    let c = p.machine("pc-c", Path::new(NEW));
    c.ok(&["sync", "join", a.ok(&["sync", "code"]).trim(), "--yes", "--author", "Ana"]);
    let key = key_from_code(&a.ok(&["sync", "code"]));
    a.save(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    if v10_steps_away(&p, &old, &key) {
        return;
    }
    let id = "01M62EWVHRXM0000000000FUT1";
    let forgotten = "01M62EWVHRXM0000000000FUT2";
    let future = serde_json::json!({ "x": 1, "y": ["dos"] });
    let lines = [
        serde_json::json!({
            "type": "memory", "id": id, "project_key": "", "project_path": "", "kind": "fact",
            "title": "Una memoria de una 1.x", "content": "Con un campo que la 1.0 no conoce.",
            "created_at": "2026-10-29T00:00:00.000Z", "updated_at": "2026-10-29T00:00:00.000Z",
            "hlc": HLC_FUTURE, "machine": "pc-futuro", "futuro": future
        }),
        serde_json::json!({
            "type": "verdict", "memory_id": id, "relation": "ok:unused", "at": "2026-10-29T00:00:00.000Z",
            "hlc": "1761696000001-0000000000-pc-futuro", "machine": "pc-futuro", "futuro": future
        }),
        serde_json::json!({
            "type": "tombstone", "id": forgotten, "deleted_at": "2026-10-29T00:00:00.000Z",
            "hlc": "1761696000002-0000000000-pc-futuro", "machine": "pc-futuro", "futuro": future
        }),
    ];
    let comment = serde_json::json!({ "type": "comment", "memory_id": id, "text": "una línea de un tipo nuevo", "hlc": HLC_FUTURE });
    let mut text: Vec<String> = lines.iter().map(|l| encrypt_line(&key, l)).collect();
    text.push(encrypt_line(&key, &comment));
    text.push(format!("z2:{}", encrypt_line(&key, &comment)));
    let file = text.join("\n") + "\n";
    p.push_by_hand("global/m-futuro.jsonl", &file);

    let r = old.sync();
    assert_eq!((r["nuevas"].as_u64(), r["de_version_nueva"].as_u64()), (Some(2), Some(2)), "la de la 1.x y la de A: {r}");
    assert!(old.get(id).is_some());
    let old_machine = r["equipo"].as_str().unwrap().to_string();
    assert!(old.ok(&["sync", "status"]).contains("from a newer version"));
    let stored: String = old.db().query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stored).unwrap(), serde_json::json!({ "futuro": future }));

    // 1.0 changes it and pushes it again: the field goes with the memory, the verdict and the forget.
    old.ok(&["update", id, "-c", "Cambiada por la 1.0.", "-r", "aquí"]);
    old.ok(&["update", id, "--pin"]);
    old.sync();
    let from_old: Vec<serde_json::Value> = p.lines_of(&p.url()).iter().filter_map(|l| decrypt_line(&key, l)).filter(|l| l["machine"] == old_machine.as_str()).collect();
    let memory = from_old.iter().find(|l| l["type"] == "memory" && l["id"] == id).unwrap_or_else(|| panic!("{from_old:?}"));
    assert_eq!((&memory["content"], &memory["pinned"], &memory["futuro"]), (&serde_json::json!("Cambiada por la 1.0."), &serde_json::json!(true), &future), "{memory}");
    assert_ne!(memory["hlc"], HLC_FUTURE);
    for kind in ["verdict", "tombstone"] {
        let l = from_old.iter().find(|l| l["type"] == kind).unwrap_or_else(|| panic!("{kind}: {from_old:?}"));
        assert_eq!(l["futuro"], future, "{l}");
    }
    // The 1.x file stays as it was, with the lines 1.0 does not understand.
    let in_repo = std::fs::read_to_string(p.clone_of(&p.url()).join("global").join("m-futuro.jsonl")).unwrap();
    assert_eq!(in_repo, file);

    // The commit receives it changed and with the field.
    c.sync();
    assert_eq!(c.get(id).expect("llega al commit")["content"], "Cambiada por la 1.0.");
    let stored: String = c.db().query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stored).unwrap()["futuro"], future);
}

/// 1.8: a memory's structured data (`--data`) travels as a `data` field of the line that
/// 1.0 does not know: it keeps it, changes the memory and forwards it without losing it.
#[test]
fn data_passes_through_10_without_loss() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("datos");
    let (a, old) = two(&p, Path::new(NEW), &v10);
    let key = key_from_code(&a.ok(&["sync", "code"]));
    a.save(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    if v10_steps_away(&p, &old, &key) {
        return;
    }
    let data = serde_json::json!({ "ruta": ["web", "seo-geo"], "prod_tests": true });
    let id = a.save(&a.dir, "decision", "Kumi: perfil del proyecto", "Lo que decidió Marc.", &["--global", "--data", &data.to_string()]);
    a.sync();
    old.sync();
    let stored: String = old.db().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stored).unwrap()["data"], data);
    old.ok(&["update", &id, "-c", "Cambiado por la 1.0.", "-r", "aquí"]);
    old.sync();
    a.sync();
    let m = a.get(&id).unwrap();
    assert_eq!((&m["content"], &m["data"]), (&serde_json::json!("Cambiado por la 1.0."), &data), "{m}");
}

/// 1.8.5: a person's verification (`verified`) and the provenance (`provenance`) travel as
/// fields of the line that 1.0 does not know: it keeps and forwards them. If 1.0 only pins it, the
/// verification still holds; if it changes the text, it no longer holds (its fingerprint no longer matches), and the
/// provenance stays.
#[test]
fn verification_and_provenance_pass_through_10() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("verificada");
    let (a, old) = two(&p, Path::new(NEW), &v10);
    let key = key_from_code(&a.ok(&["sync", "code"]));
    a.save(&a.dir, "fact", "Para que se escriba el formato", "Algo.", &["--global"]);
    a.sync();
    if v10_steps_away(&p, &old, &key) {
        return;
    }
    // Without an agent shell's variables (if the tests run inside one, `verify` refuses).
    let without_agent = |args: &[&str], env: &[(&str, &str)]| -> serde_json::Value {
        let mut c = a.command(&a.dir);
        for v in omoikane::verified::AGENT_ENV_VARS {
            c.env_remove(v);
        }
        for (k, x) in env {
            c.env(k, x);
        }
        let o = c.arg("--json").args(args).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    };
    let id = without_agent(&["remember", "-k", "decision", "-t", "Verificada por Marc", "--global", "Lo decidió Marc."], &[("OMOIKANE_SESSION", "s-compat")])["id"]
        .as_str()
        .unwrap()
        .to_string();
    without_agent(&["verify", &id, "--by", "Marc", "--yes"], &[]);
    a.sync();
    old.sync();
    let stored: String = old.db().query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0)).unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!((&stored["verified"]["by"], &stored["provenance"]["session"]), (&serde_json::json!("Marc"), &serde_json::json!("s-compat")), "{stored}");
    old.ok(&["update", &id, "--pin"]);
    old.sync();
    a.sync();
    let m = a.get(&id).unwrap();
    assert_eq!((&m["pinned"], &m["verified"]["by"], &m["provenance"]["session"]), (&serde_json::json!(true), &serde_json::json!("Marc"), &serde_json::json!("s-compat")), "{m}");
    old.ok(&["update", &id, "-c", "Cambiado por la 1.0.", "-r", "aquí"]);
    old.sync();
    a.sync();
    let m = a.get(&id).unwrap();
    assert_eq!(m["content"], "Cambiado por la 1.0.");
    assert!(m.get("verified").is_none(), "la 1.0 cambió el texto que se verificó: {m}");
    assert_eq!(m["provenance"]["session"], "s-compat", "{m}");
}

// --- The database -------------------------------------------------------------------------------------

/// The files of a database (`o.db`, `o.db-wal`…) with their contents, to see that nobody touches them.
fn db_files(dir: &Path, db: &str) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().starts_with(db))
        .map(|f| (f.file_name().to_string_lossy().into_owned(), std::fs::read(f.path()).unwrap()))
        .collect();
    v.sort();
    v
}

fn version_of(db: &rusqlite::Connection) -> i64 {
    db.query_row("PRAGMA user_version", [], |f| f.get(0)).unwrap()
}

/// 1.0 opens a database from the commit, uses it, and the commit still sees what 1.0 wrote; or, if the
/// commit has raised `lector_minimo` above schema 10, it refuses with a clear message and without
/// touching it. Also, against the real 1.0: a database from a future schema that only adds is opened, and
/// one that asks for a newer reader is not touched.
#[test]
fn v10_opens_the_commits_database() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("base");
    let new = p.machine("pc", Path::new(NEW));
    let old = p.machine("pc", &v10);
    let project = new.project("tienda", "git@github.com:marc/tienda.git");
    let id = new.save(&project, "decision", "Los precios van en céntimos", "Todos los importes en enteros.", &[]);
    new.ok_in(&project, &["update", &id, "-c", "Todos los importes en enteros de céntimos.", "-r", "más claro"]);
    let global = new.save(&new.dir, "preference", "Respuestas cortas", "Sin rodeos.", &["--global"]);
    let forgotten = new.save(&new.dir, "fact", "Una que sobra", "Nada.", &["--global"]);
    new.ok(&["forget", &forgotten, "--yes"]);

    let (version, reader) = {
        let db = new.db();
        let v = version_of(&db);
        let l: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |f| f.get(0)).ok();
        (v, l.and_then(|l| l.trim().parse::<i64>().ok()).unwrap_or(v))
    };
    if reader > 10 {
        eprintln!("compat: el commit pide lector_minimo {reader}: la 1.0 no debe abrir la base");
        let before = db_files(&new.dir, "o.db");
        let o = old.omoi_in(&old.dir, &["get", &id]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("newer omoi"), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(db_files(&new.dir, "o.db"), before, "sin tocarla");
    } else {
        let m = old.get(&id).expect("la 1.0 la lee");
        assert_eq!(m["content"], "Todos los importes en enteros de céntimos.");
        assert_eq!(old.json_in(&project, &["history", &id]).as_array().unwrap().len(), 1);
        let s = old.json_in(&project, &["search", "céntimos"]);
        assert_eq!(s[0]["id"], id.as_str(), "{s}");
        assert!(old.get(&global).is_some());
        assert!(old.ok(&["trash"]).contains("Una que sobra"));
        let from_10 = old.save(&project, "fact", "Escrita por la 1.0", "En la base del commit.", &[]);
        old.ok_in(&project, &["update", &id, "--pin"]);
        // 1.9.0: the history has `extra` (a column with a default value): 1.0 writes to it
        // naming its columns, and the commit reads it.
        old.ok_in(&project, &["update", &id, "-c", "Todos los importes en enteros de céntimos, sin decimales.", "-r", "1.0"]);
        assert_eq!(new.json_in(&project, &["history", &id]).as_array().unwrap().len(), 2);
        let extras: Vec<String> = {
            let db = new.db();
            let mut q = db.prepare("SELECT extra FROM memory_version WHERE memory_id = ?1").unwrap();
            q.query_map([&id], |f| f.get(0)).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(extras, vec!["{}".to_string(), "{}".to_string()]);
        assert_eq!(version_of(&new.db()), version, "la 1.0 no le baja el esquema");
        // And the commit keeps working with it.
        assert_eq!(new.get(&from_10).expect("el commit ve lo de la 1.0")["content"], "En la base del commit.");
        assert_eq!(new.get(&id).unwrap()["pinned"], true);
        new.ok(&["restore", &forgotten]);
    }

    // A database from a future schema that only adds (a table and a column with a default value,
    // readable from 10): 1.0 opens it, writes to it and does not downgrade it.
    let future = p.dir.join("pc").join("futura.db");
    new.db().execute("VACUUM INTO ?1", [future.to_str().unwrap()]).unwrap();
    let future_schema = version.max(10) + 1;
    {
        let db = rusqlite::Connection::open(&future).unwrap();
        db.execute_batch(&format!(
            "PRAGMA user_version = {future_schema}; CREATE TABLE futura (x); INSERT INTO futura VALUES (1);
             ALTER TABLE memory ADD COLUMN futura TEXT NOT NULL DEFAULT 'x';
             INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', '10');"
        ))
        .unwrap();
    }
    let with_future = |args: &[&str]| old.command(&old.dir).env("OMOIKANE_DB", &future).args(args).stdin(Stdio::null()).output().unwrap();
    let o = with_future(&["--json", "remember", "-k", "fact", "-t", "En una base futura", "--global", "La 1.0 escribe en ella."]);
    assert!(o.status.success(), "la 1.0 abre una base que solo añade: {}", String::from_utf8_lossy(&o.stderr));
    let o = with_future(&["--json", "get", &global]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    {
        let db = rusqlite::Connection::open(&future).unwrap();
        assert_eq!(version_of(&db), future_schema, "no le baja el esquema");
        assert_eq!(db.query_row("SELECT count(*) FROM futura", [], |f| f.get::<_, i64>(0)).unwrap(), 1);
        let (all, with_value): (i64, i64) = db.query_row("SELECT count(*), count(*) FILTER (WHERE futura = 'x') FROM memory", [], |f| Ok((f.get(0)?, f.get(1)?))).unwrap();
        assert_eq!(all, with_value, "la columna nueva sigue, con su valor por defecto");
        // And another that breaks: readable only from a schema 1.0 does not know.
        db.execute_batch(&format!("PRAGMA user_version = {0}; UPDATE meta SET v = '{0}' WHERE k = 'lector_minimo';", future_schema + 1)).unwrap();
    }
    let before = db_files(&p.dir.join("pc"), "futura.db");
    let o = with_future(&["get", &global]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("newer omoi") && err.contains("update omoi"), "{err}");
    assert_eq!(db_files(&p.dir.join("pc"), "futura.db"), before, "sin tocarla");
}

// --- omoi export / omoi import ------------------------------------------------------------------------

/// A database with a bit of everything: history, a verdict with a reason and the trash.
fn fill(e: &Machine, cwd: &Path, who: &str) {
    let cookie = e.save(cwd, "decision", &format!("Sesiones por cookie ({who})"), "La autenticación usa una cookie de sesión.", &[]);
    e.ok_in(cwd, &["update", &cookie, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    let jwt = e.save(cwd, "decision", &format!("Autenticación con JWT ({who})"), "Se pasó a JWT en la cabecera Authorization.", &[]);
    e.tool_in(cwd, "update", &format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}"));
    e.save(cwd, "preference", &format!("Respuestas cortas ({who})"), "Sin rodeos.", &["--global"]);
    let spare = e.save(cwd, "fact", &format!("Una que sobra ({who})"), "Nada.", &["--global"]);
    e.ok_in(cwd, &["update", &spare, "-c", "Nada de nada.", "-r", "más claro"]);
    e.ok_in(cwd, &["forget", &spare, "--yes"]);
}

/// The lines of a backup, without the header (it carries the time) or the end line, and without what depends on the
/// exporting machine (`machine`).
fn body(text: &str) -> Vec<serde_json::Value> {
    text.lines()
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

/// `a` is contained in `b`: everything `a` says, `b` says the same (in objects, `b` may have
/// more fields).
fn contained(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => x.iter().all(|(k, v)| y.get(k).is_some_and(|w| contained(v, w))),
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| contained(v, w)),
        _ => a == b,
    }
}

/// Exports from `from`, imports into `to` (an empty database) and exports again from `to`: nothing
/// carried by `from`'s backup is lost, and repeating the import changes nothing.
fn backup_from_one_to_another(p: &Harness, from: &Machine, to: &Machine, cwd: &Path) {
    let o = from.omoi_in(cwd, &["export", "--all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8(o.stdout).unwrap();
    let file = p.dir.join(format!("copia-{}.jsonl", ulid::Ulid::generate()));
    std::fs::write(&file, &text).unwrap();
    let header: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let version = header["version"].as_u64().unwrap();
    if version > 2 && !to.is_the_commit() {
        // The commit has raised the format: 1.0 must refuse and import nothing.
        eprintln!("compat: el commit exporta el formato {version}: la 1.0 no debe importarlo");
        let o = to.omoi_in(cwd, &["import", file.to_str().unwrap()]);
        assert!(!o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("comes from a newer omoi"), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(body(&to.ok_in(cwd, &["export", "--all"])).is_empty(), "{}: no mete nada", to.who());
        return;
    }
    let r = to.json_in(cwd, &["import", file.to_str().unwrap()]);
    assert_eq!((r["skipped_lines"].as_u64(), r["updated"].as_u64()), (Some(0), Some(0)), "{}: {r}", to.who());
    let o = to.omoi_in(cwd, &["export", "--all"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (before, after) = (body(&text), body(&String::from_utf8(o.stdout).unwrap()));
    assert_eq!(before.len(), after.len(), "{} → {}: {before:#?}\n{after:#?}", from.who(), to.who());
    for l in &before {
        assert!(after.iter().any(|d| contained(l, d)), "{} → {}: se pierde {l:#}\nen {after:#?}", from.who(), to.who());
    }
    let r = to.json_in(cwd, &["import", file.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["updated"].as_u64(), r["forgotten"].as_u64()), (Some(0), Some(0), Some(0)), "{}: {r}", to.who());
}

#[test]
fn commit_export_is_imported_by_10_and_vice_versa() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("export");
    // The same machine name and the same project folder in all four databases.
    let new = p.machine_in("nuevo", "pc-pruebas", Path::new(NEW));
    let old = p.machine_in("viejo", "pc-pruebas", &v10);
    let project = new.project("tienda", "git@github.com:marc/tienda.git");
    fill(&new, &project, "commit");
    backup_from_one_to_another(&p, &new, &old, &project);
    let new2 = p.machine_in("nuevo2", "pc-pruebas", Path::new(NEW));
    let old2 = p.machine_in("viejo2", "pc-pruebas", &v10);
    fill(&old2, &project, "1.0");
    backup_from_one_to_another(&p, &old2, &new2, &project);
}

/// 1.4: the session diary is a new local table. A 1.0 opens the database that has it, saves and
/// searches as always, and the row is still there for the commit.
#[test]
fn v10_opens_a_database_with_a_diary() {
    let Some(v10) = binary_10() else { return };
    let p = Harness::new("diario");
    let new = p.machine_in("base", "pc-pruebas", Path::new(NEW));
    let old = p.machine_in("base", "pc-pruebas", &v10);
    let project = new.project("tienda", "git@github.com:marc/tienda.git");
    new.save(&project, "fact", "Puerto", "Escucha en el 8010", &[]);
    let transcript = new.dir.join("sesion.jsonl");
    let line = serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": project.join("src").join("main.rs")}}]}});
    std::fs::write(&transcript, line.to_string()).unwrap();
    let input = serde_json::json!({"session_id": "s1", "transcript_path": transcript, "cwd": project}).to_string();
    let mut child = new.command(&project).args(["recall", "--session-end"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    assert_eq!(new.json_in(&project, &["diary"])[0]["files"][0], "src/main.rs");
    // 1.0, on the same database.
    old.save(&project, "fact", "Otra", "Guardada por la 1.0", &[]);
    assert_eq!(old.json_in(&project, &["recent"]).as_array().map(Vec::len), Some(2));
    assert!(old.ok_in(&project, &["search", "8010"]).contains("Puerto"));
    assert_eq!(new.json_in(&project, &["diary"])[0]["files"][0], "src/main.rs");
}
