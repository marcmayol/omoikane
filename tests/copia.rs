//! 0.18.0: nothing is lost. The trash (`forget` → `trash` → `restore`) and the faithful copy
//! (`export` → `import` rebuilds the exact database and can be repeated without changing anything).

mod comun;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Env {
    dir: comun::Temporal,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = comun::Temporal::nueva(&format!("copia-{name}"));
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        Env { dir }
    }

    /// `omoi` against database `db` (a name inside the test folder), always with the same machine
    /// name (so two databases export identical lines).
    fn omoi(&self, db: &str, args: &[&str], input: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.dir.join(db))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_EQUIPO", "pc-pruebas")
            .env_remove("OMOIKANE_LANG")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        comun::sin_la_sesion(&mut c);
        let mut child = c.spawn().unwrap();
        if let Some(e) = input {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, db: &str, args: &[&str]) -> String {
        let o = self.omoi(db, args, None);
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, db: &str, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(db, &a)).unwrap()
    }

    fn id(&self, db: &str, kind: &str, title: &str, text: &str) -> String {
        self.json(db, &["remember", "-k", kind, "-t", title, text])["id"].as_str().unwrap().to_string()
    }

    fn get(&self, db: &str, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi(db, &["--json", "get", id], None);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// The lines of an export without the header (which carries the time) and without the exporting
/// machine's name (`machine`, and the closing SHA, which includes it): what must be identical between
/// a database and its copy.
fn body(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.remove("machine");
                o.remove("sha256");
            }
            v.to_string()
        })
        .collect()
}

#[test]
fn trash_returns_the_forgotten_with_its_history() {
    let e = Env::new("papelera");
    let id = e.id("o.db", "decision", "Los precios van en céntimos", "Todos los importes en enteros.");
    e.ok("o.db", &["update", &id, "-c", "Todos los importes en enteros de céntimos.", "-r", "más claro"]);
    let output = e.ok("o.db", &["forget", &id, "--yes"]);
    assert!(output.contains("omoi restore"), "{output}");
    assert!(e.get("o.db", &id).is_none());
    let trash = e.json("o.db", &["trash"]);
    assert_eq!(trash[0]["id"], id.as_str());
    assert!(trash[0]["by"].as_str().unwrap().starts_with("pc-pruebas"), "{trash}");
    // `restore` without an id shows the same.
    assert!(e.ok("o.db", &["restore"]).contains("Los precios van en céntimos"));
    e.ok("o.db", &["restore", &id[..12]]);
    let m = e.get("o.db", &id).expect("vuelve");
    assert_eq!(m["content"], "Todos los importes en enteros de céntimos.");
    assert_eq!(e.json("o.db", &["history", &id]).as_array().unwrap().len(), 1, "con su historial");
    assert!(e.json("o.db", &["trash"]).as_array().unwrap().is_empty());
    // Through MCP, with no new tool: `update` with restore.
    e.ok("o.db", &["forget", &id, "--yes"]);
    let o = e.omoi("o.db", &["tool", "update"], Some(&format!("{{\"id\":\"{id}\",\"restore\":true}}")));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(e.get("o.db", &id).is_some());
    // `--purge` skips the trash, and emptying it deletes for good.
    let other = e.id("o.db", "fact", "Una que sobra", "Nada.");
    e.ok("o.db", &["forget", &other, "--yes", "--purge"]);
    assert!(e.json("o.db", &["trash"]).as_array().unwrap().is_empty());
    e.ok("o.db", &["forget", &id, "--yes"]);
    assert_eq!(e.json("o.db", &["trash", "--empty", "--yes"])["emptied"], 1);
    assert!(!e.omoi("o.db", &["restore", &id], None).status.success());
}

#[test]
fn export_and_import_rebuild_the_exact_db_and_can_be_repeated() {
    let e = Env::new("fiel");
    let a = e.id("o.db", "decision", "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.");
    let b = e.id("o.db", "fact", "El servidor de pruebas es lento", "Tarda unos 10 s en arrancar.");
    let c = e.id("o.db", "decision", "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.");
    e.ok("o.db", &["update", &a, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    e.ok("o.db", &["forget", &b, "--yes"]);
    let o = e.omoi("o.db", &["tool", "update"], Some(&format!("{{\"id\":\"{c}\",\"supersedes\":[\"{a}\"],\"reason\":\"JWT replaced the cookie\"}}")));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let f1 = e.file("copia1.jsonl");
    let output = e.omoi("o.db", &["export", "--all"], None);
    assert!(output.status.success());
    std::fs::write(&f1, &output.stdout).unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["type"], "header");
    assert_eq!(lines[0]["format"], "omoi-export");
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    // Verdicts after the trash: on import, the one for a forgotten memory finds its memory.
    assert_eq!(types, ["header", "tombstone", "memory", "memory", "forgotten", "verdict", "end"], "{types:?}");
    assert_eq!(lines[6]["count"], 5);
    assert_eq!(lines[2]["versions"].as_array().unwrap().len(), 1, "el historial viaja");
    assert_eq!((lines[5]["reason"].as_str(), lines[5]["author"].as_str()), (Some("JWT replaced the cookie"), Some("agent")), "el veredicto con su motivo y su autor: {}", lines[5]);
    // Count only: the whole file is checked.
    let d = e.json("o2.db", &["import", f1.to_str().unwrap(), "--dry-run"]);
    assert_eq!((d["file"]["memories"].as_u64(), d["file"]["tombstones"].as_u64(), d["file"]["verdicts"].as_u64(), d["file"]["forgotten"].as_u64()), (Some(2), Some(1), Some(1), Some(1)));
    assert!(e.get("o2.db", &a).is_none(), "--dry-run no importa nada");
    // Importing into an empty database: the same database.
    let r = e.json("o2.db", &["import", f1.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["updated"].as_u64(), r["skipped_lines"].as_u64()), (Some(2), Some(0), Some(0)), "{r}");
    let m = e.get("o2.db", &a).expect("llega con el mismo id");
    assert_eq!(m["content"], "La autenticación usaba una cookie de sesión; ver JWT.");
    assert_eq!(m["created_at"], e.get("o.db", &a).unwrap()["created_at"], "las fechas son las de origen");
    assert_eq!(e.json("o2.db", &["history", &a]).as_array().unwrap().len(), 1);
    assert_eq!(e.json("o2.db", &["trash"])[0]["id"], b.as_str(), "la papelera también");
    assert!(e.get("o2.db", &b).is_none(), "lo olvidado sigue olvidado");
    let o = e.omoi("o2.db", &["tool", "search"], Some("{\"query\":\"cookie de sesión\",\"all_projects\":true}"));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let note = v.as_array().unwrap().iter().find(|x| x["id"] == a.as_str()).unwrap()["note"].as_str().unwrap_or("").to_string();
    assert!(note.starts_with("superseded by") && note.ends_with(": JWT replaced the cookie"), "el veredicto también, con su motivo: {note}");
    let rev = e.json("o2.db", &["review", "--all"]);
    let av = rev.as_array().unwrap().iter().find(|x| x["kind"] == "superseded").expect("sale en review");
    assert_eq!((av["reason"].as_str(), av["author"].as_str()), (Some("JWT replaced the cookie"), Some("agent")), "{av}");
    // Exporting the copy gives the same file (minus the header, which carries the time).
    let f2 = e.file("copia2.jsonl");
    let output = e.omoi("o2.db", &["export", "--all"], None);
    std::fs::write(&f2, &output.stdout).unwrap();
    assert_eq!(body(&f1), body(&f2));
    // Repeating the import changes nothing.
    let r = e.json("o2.db", &["import", f1.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["updated"].as_u64(), r["forgotten"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
    // A tampered file is not imported.
    let broken = text.replacen("cookie de sesión", "cookie de sesion", 1);
    std::fs::write(e.file("roto.jsonl"), broken).unwrap();
    let o = e.omoi("o3.db", &["import", e.file("roto.jsonl").to_str().unwrap()], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("damaged"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.get("o3.db", &a).is_none());
    // Nor a truncated one.
    let truncated: String = text.lines().take(4).collect::<Vec<_>>().join("\n");
    std::fs::write(e.file("cortado.jsonl"), truncated).unwrap();
    let o = e.omoi("o3.db", &["import", e.file("cortado.jsonl").to_str().unwrap()], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("incomplete"));
}

#[test]
fn encrypted_copy_is_only_read_with_the_key() {
    let e = Env::new("cifrada");
    // A sync key: a local bare git repository is enough.
    std::fs::write(e.dir.join("gitconfig-vacio"), "").unwrap();
    let o = Command::new("git").current_dir(&e.dir).args(["init", "--quiet", "--bare", "compartida.git"]).env("GIT_CONFIG_GLOBAL", e.dir.join("gitconfig-vacio")).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let url = e.dir.join("compartida.git").to_string_lossy().replace('\\', "/");
    let o = e.omoi("o.db", &["sync", "init", &url, "--yes"], None);
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let id = e.id("o.db", "gotcha", "El token caduca antes de tiempo", "Compara la hora en UTC.");
    let output = e.omoi("o.db", &["export", "--all", "--encrypt"], None);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("\"encrypted\":true"));
    assert!(!lines[1].contains("token") && serde_json::from_str::<serde_json::Value>(lines[1]).is_err(), "la memoria va cifrada");
    let f = e.file("cifrada.jsonl");
    std::fs::write(&f, &text).unwrap();
    // With the key it reads in full; without it, it doesn't.
    let d = e.json("o.db", &["import", f.to_str().unwrap(), "--dry-run"]);
    assert_eq!(d["file"]["memories"], 1);
    let o = e.omoi("otra.db", &["import", f.to_str().unwrap(), "--dry-run"], None);
    assert!(!o.status.success());
    assert!(e.get("o.db", &id).is_some());
}

#[test]
fn old_export_and_engram_export_still_import() {
    let e = Env::new("antigua");
    let old = r#"{"id":"01HXXXXXXXXXXXXXXXXXXXXXXX","project":"","kind":"fact","title":"Una de antes","content":"Del formato 1.","paths":[],"tags":[],"pinned":false,"source":"cli","created_at":"2026-10-01T10:00:00.000Z","updated_at":"2026-10-01T10:00:00.000Z","recalled_at":null,"recall_count":0,"versions":[]}"#;
    std::fs::write(e.file("v1.jsonl"), format!("{old}\n")).unwrap();
    let output = e.ok("o.db", &["import", e.file("v1.jsonl").to_str().unwrap()]);
    assert!(output.contains("imported 1"), "{output}");
    let o = e.omoi("o.db", &["tool", "search"], Some("{\"query\":\"formato 1\",\"all_projects\":true}"));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Una de antes"));
}

#[test]
fn backup_copies_the_db_and_keeps_the_latest() {
    let e = Env::new("backup");
    e.id("o.db", "fact", "Algo", "Para que haya base.");
    let output = e.ok("o.db", &["backup"]);
    assert!(output.contains("backups"), "{output}");
    let list = e.json("o.db", &["backup", "--list"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let path = PathBuf::from(list[0]["path"].as_str().unwrap());
    assert!(path.file_name().unwrap().to_string_lossy().starts_with("omoikane-"));
    // The copy is a database that opens and has the memory.
    let o = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["--db", path.to_str().unwrap(), "--json", "recent", "--all"]).env("OMOIKANE_AUTO_UPDATE", "0").env("OMOIKANE_HOME", e.dir.join("home-omoikane")).stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Algo"));
    // Repeating on the same day replaces today's copy instead of adding another.
    e.ok("o.db", &["backup"]);
    assert_eq!(e.json("o.db", &["backup", "--list"]).as_array().unwrap().len(), 1);
    let doctor = e.ok("o.db", &["doctor"]);
    assert!(doctor.contains("backups    omoikane-"), "{doctor}");
}

/// 0.19.1: a memory of a project that arrived from another machine is exported with the key it
/// arrived with, not one computed here. Since schema 10 each project's key is in the `project`
/// table (computed once, when created): export and sync use that one.
#[test]
fn export_uses_the_key_of_projects_that_came_from_elsewhere() {
    let e = Env::new("clave-remota");
    let id = e.id("o.db", "fact", "La caché se invalida al publicar", "Con el hook de publicación.");
    let path = e.get("o.db", &id).unwrap()["project"].as_str().unwrap().to_string();
    assert!(!path.is_empty());
    {
        let db = rusqlite::Connection::open(e.file("o.db")).unwrap();
        db.execute("UPDATE project SET key = 'git:github.com/marcmayol/omoikane' WHERE id = (SELECT project_id FROM memory WHERE id = ?1)", [&id]).unwrap();
    }
    let output = e.omoi("o.db", &["export", "--all"], None);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let memory = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|l| l["id"] == id.as_str())
        .unwrap();
    assert_eq!(memory["project_key"], "git:github.com/marcmayol/omoikane", "{memory}");
    assert_eq!(memory["project_path"], path.as_str());
}

/// 0.19.1: an old database that has in its trash a memory that is already alive (another machine
/// restored it): `omoi restore` doesn't fail with `UNIQUE`, it warns and removes it from the trash.
#[test]
fn restoring_a_live_memory_removes_it_from_the_trash() {
    let e = Env::new("viva-en-papelera");
    let id = e.id("o.db", "fact", "Los informes salen los viernes", "Antes de las 14:00.");
    e.ok("o.db", &["forget", &id, "--yes"]);
    let row: (String, String, String, String, String) = {
        let db = rusqlite::Connection::open(e.file("o.db")).unwrap();
        db.query_row("SELECT memory, versions, hlc, deleted_at, by FROM forgotten WHERE id = ?1", [&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?))).unwrap()
    };
    e.ok("o.db", &["restore", &id]);
    {
        let db = rusqlite::Connection::open(e.file("o.db")).unwrap();
        db.execute("INSERT INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", rusqlite::params![id, row.0, row.1, row.2, row.3, row.4]).unwrap();
    }
    let o = e.omoi("o.db", &["restore", &id], None);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && !err.contains("UNIQUE") && (err.contains("trash") || err.contains("papelera")), "{err}");
    assert!(e.get("o.db", &id).is_some(), "sigue viva");
    assert!(!e.ok("o.db", &["trash"]).contains("informes"), "y ya no está en la papelera");
}

// --- What comes after 1.0 (A3 and A6) -----------------------------------------------------------------

/// A hand-written `omoi export` file (what a newer omoi would write): the header, the lines and the
/// closing line with its count and its SHA-256.
fn hand_written_export(lines: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};
    let header = serde_json::json!({ "type": "header", "format": "omoi-export", "version": 2, "schema": 11, "exported_at": "2026-10-31T00:00:00.000Z", "machine": "pc-futuro" });
    let mut hash = Sha256::new();
    let mut t = format!("{header}\n");
    for l in lines {
        let s = l.to_string();
        hash.update(s.as_bytes());
        hash.update(b"\n");
        t.push_str(&s);
        t.push('\n');
    }
    let sha: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    t.push_str(&serde_json::json!({ "type": "end", "count": lines.len(), "sha256": sha }).to_string());
    t.push('\n');
    t
}

/// The lines of an export, parsed.
fn parse_lines(text: &str) -> Vec<serde_json::Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The line with a given type and id (the memory's, or `memory_id` in verdicts).
fn line_of<'a>(ls: &'a [serde_json::Value], ty: &str, id: &str) -> &'a serde_json::Value {
    ls.iter().find(|l| l["type"] == ty && (l["id"] == id || l["memory_id"] == id)).unwrap_or_else(|| panic!("falta {ty} {id}"))
}

/// An export from a newer omoi: fields this version doesn't know in every line type, and a line of
/// a type it doesn't know. It gets imported (the new line is counted and skipped), the memory is
/// changed here, and on exporting again the fields are still there; and after another import and export.
#[test]
fn a_new_field_survives_export_and_import() {
    let e = Env::new("campo-nuevo");
    let (id, forgotten, in_trash) = ("01M62EWVHRXM0000000000FUT1", "01M62EWVHRXM0000000000FUT2", "01M62EWVHRXM0000000000FUT3");
    let future = serde_json::json!({ "x": 1 });
    let hlc = |n: u32| format!("17616960000{n:02}-0000000000-pc-futuro");
    let memory = |id: &str, title: &str, stamp: &str| {
        serde_json::json!({
            "id": id, "project_key": "", "project_path": "", "kind": "fact", "title": title, "content": "Algo.",
            "created_at": "2026-10-29T00:00:00.000Z", "updated_at": "2026-10-29T00:00:00.000Z", "hlc": stamp, "futuro": future
        })
    };
    let mut live = memory(id, "Una memoria de una versión nueva", &hlc(1));
    live["type"] = "memory".into();
    let text = hand_written_export(&[
        serde_json::json!({ "type": "tombstone", "id": forgotten, "deleted_at": "2026-10-29T00:00:00.000Z", "hlc": hlc(2), "futuro": future }),
        live,
        serde_json::json!({ "type": "comment", "memory_id": id, "text": "de una versión más nueva" }),
        serde_json::json!({
            "type": "forgotten", "id": in_trash, "memory": memory(in_trash, "Una de la papelera", &hlc(3)), "versions": [],
            "hlc": hlc(3), "deleted_at": "2026-10-29T00:00:00.000Z", "by": "pc-futuro", "futuro": future
        }),
        serde_json::json!({ "type": "verdict", "memory_id": id, "relation": "ok:unused", "at": "2026-10-29T00:00:00.000Z", "hlc": hlc(4), "futuro": future }),
    ]);
    let f1 = e.file("futuro.jsonl");
    std::fs::write(&f1, &text).unwrap();
    let d = e.json("o.db", &["import", f1.to_str().unwrap(), "--dry-run"]);
    assert_eq!((d["file"]["memories"].as_u64(), d["file"]["unknown"].as_u64()), (Some(1), Some(1)), "{d}");
    let r = e.json("o.db", &["import", f1.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["skipped_lines"].as_u64(), r["file"]["unknown"].as_u64()), (Some(1), Some(0), Some(1)), "{r}");
    assert!(e.ok("o.db", &["import", f1.to_str().unwrap()]).contains("1 line(s) from a newer version"));

    // Changed here and exported: the fields are still in all four.
    e.ok("o.db", &["update", id, "-c", "Algo, cambiado aquí.", "-r", "aquí"]);
    let output = e.ok("o.db", &["export", "--all"]);
    let ls = parse_lines(&output);
    assert_eq!(line_of(&ls, "memory", id)["content"], "Algo, cambiado aquí.");
    for (ty, id) in [("memory", id), ("tombstone", forgotten), ("verdict", id), ("forgotten", in_trash)] {
        assert_eq!(line_of(&ls, ty, id)["futuro"], future, "{ty}: {}", line_of(&ls, ty, id));
    }
    assert_eq!(line_of(&ls, "forgotten", in_trash)["memory"]["futuro"], future, "y dentro de la memoria de la papelera");

    // Another round, in another database: the same.
    let f2 = e.file("vuelta.jsonl");
    std::fs::write(&f2, &output).unwrap();
    e.ok("o2.db", &["import", f2.to_str().unwrap()]);
    let ls2 = parse_lines(&e.ok("o2.db", &["export", "--all"]));
    for (ty, id) in [("memory", id), ("tombstone", forgotten), ("verdict", id), ("forgotten", in_trash)] {
        assert_eq!(line_of(&ls2, ty, id)["futuro"], future, "{ty} tras la segunda vuelta");
    }
    // And the one in the trash, once restored, comes back with its own.
    e.ok("o2.db", &["restore", in_trash]);
    let ls3 = parse_lines(&e.ok("o2.db", &["export", "--all"]));
    assert_eq!(line_of(&ls3, "memory", in_trash)["futuro"], future);
}

/// The trash travels with its project and its space by key (the shape of a `memory` line): restored
/// on another machine, it lands where it was. And the verdicts of what is in the trash travel too, so
/// that restoring it brings back its `superseded`.
#[test]
fn trash_travels_with_its_project_space_and_verdicts() {
    let e = Env::new("papelera-espacio");
    e.ok("o.db", &["space", "add", "Acme"]);
    e.ok("o.db", &["space", "assign", "acme"]);
    let space_mem = e.json("o.db", &["remember", "-k", "fact", "-t", "Acme factura en euros", "--space=acme", "Siempre con IVA."])["id"].as_str().unwrap().to_string();
    let cookie = e.id("o.db", "decision", "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.");
    let jwt = e.id("o.db", "decision", "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.");
    let o = e.omoi("o.db", &["tool", "update"], Some(&format!("{{\"id\":\"{jwt}\",\"supersedes\":[\"{cookie}\"],\"reason\":\"JWT replaced the cookie\"}}")));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let key = e.get("o.db", &cookie).unwrap()["project_key"].as_str().unwrap().to_string();
    // Everything to the trash: on the other machine nothing else will say which space the project is in.
    for id in [&space_mem, &cookie, &jwt] {
        e.ok("o.db", &["forget", id, "--yes"]);
    }
    let text = e.ok("o.db", &["export", "--all"]);
    let ls = parse_lines(&text);
    let p = line_of(&ls, "forgotten", &cookie);
    assert_eq!((p["memory"]["project_key"].as_str(), p["memory"]["space_key"].as_str()), (Some(key.as_str()), Some("acme")), "{p}");
    assert!(p["memory"]["project_path"].is_string() && p["memory"].get("project").is_none(), "{p}");
    let s = line_of(&ls, "forgotten", &space_mem);
    assert_eq!((s["memory"]["project_key"].as_str(), s["memory"]["space_key"].as_str()), (Some(""), Some("acme")), "{s}");
    assert_eq!(line_of(&ls, "verdict", &cookie)["relation"], "superseded", "el veredicto de la que está en la papelera");

    // Another machine with nothing: on restore, each in its place, and the superseded one with its verdict.
    let f = e.file("papelera.jsonl");
    std::fs::write(&f, &text).unwrap();
    e.ok("o2.db", &["import", f.to_str().unwrap()]);
    for id in [&space_mem, &cookie, &jwt] {
        e.ok("o2.db", &["restore", id]);
    }
    let m = e.get("o2.db", &space_mem).unwrap();
    assert_eq!((m["space"].as_str(), m["project"].as_str()), (Some("acme"), Some("")), "de espacio, no global: {m}");
    let m = e.get("o2.db", &cookie).unwrap();
    assert_eq!((m["project_key"].as_str(), m["space"].as_str()), (Some(key.as_str()), Some("acme")), "{m}");
    let rev = e.json("o2.db", &["review", "--all"]);
    let av = rev.as_array().unwrap().iter().find(|x| x["kind"] == "superseded" && x["memory_id"] == cookie.as_str()).unwrap_or_else(|| panic!("{rev}"));
    assert_eq!(av["reason"], "JWT replaced the cookie");
}

/// Exports from 0.18.0 and 0.19.0 carry in `forgotten.memory` the JSON API's `Memory` (with
/// `project`, the path, and without its stamp): they still import, and what is restored lands in that folder.
#[test]
fn trash_from_a_0_19_export_still_imports() {
    let e = Env::new("papelera-019");
    let path = e.dir.join("repo").to_string_lossy().into_owned();
    let id = "01M62EWVHRXM0000000000OLD1";
    let memory = serde_json::json!({
        "id": id, "project": path, "kind": "gotcha", "title": "El puerto 8080 está ocupado", "content": "Usar el 8081.",
        "paths": [], "tags": ["red"], "pinned": false, "source": "cli", "created_at": "2026-10-20T10:00:00.000Z",
        "updated_at": "2026-10-20T10:00:00.000Z", "recalled_at": null, "recall_count": 3
    });
    let text = hand_written_export(&[serde_json::json!({
        "type": "forgotten", "id": id, "memory": memory, "versions": [], "hlc": "1760954400000-0000000000-pc-viejo",
        "deleted_at": "2026-10-21T10:00:00.000Z", "by": "pc-viejo"
    })]);
    let f = e.file("v019.jsonl");
    std::fs::write(&f, &text).unwrap();
    let r = e.json("o.db", &["import", f.to_str().unwrap()]);
    assert_eq!(r["file"]["forgotten"], 1, "{r}");
    assert_eq!(e.json("o.db", &["trash"])[0]["id"], id);
    e.ok("o.db", &["restore", id]);
    let m = e.get("o.db", id).unwrap();
    assert_eq!((m["kind"].as_str(), m["tags"][0].as_str()), (Some("gotcha"), Some("red")), "{m}");
    // In that folder: with `TEMP` as a short name (`CLAUDE~1`, the CI) the export's path and the
    // current folder's project path are the same folder written two ways, and since 1.8.0 (the
    // common git folder) they're a single project, with the long path.
    let canonical = |r: &str| std::fs::canonicalize(r).unwrap();
    assert_eq!(canonical(m["project"].as_str().unwrap()), canonical(&path), "{m}");
}

/// Moving a project from one client to another, or taking it out of its space (its memories would go
/// to the personal repository), only with `--force`, like `projects move --space`.
#[test]
fn changing_space_or_removing_it_requires_force() {
    let e = Env::new("asignar");
    e.id("o.db", "fact", "Algo del proyecto", "Para que exista.");
    e.ok("o.db", &["space", "add", "acme"]);
    e.ok("o.db", &["space", "add", "beta"]);
    e.ok("o.db", &["space", "assign", "acme"]);
    let o = e.omoi("o.db", &["space", "assign", "beta"], None);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--force"), "{}", String::from_utf8_lossy(&o.stderr));
    e.ok("o.db", &["space", "assign", "beta", "--force"]);
    let o = e.omoi("o.db", &["space", "unassign"], None);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--force"), "{}", String::from_utf8_lossy(&o.stderr));
    e.ok("o.db", &["space", "unassign", "--force"]);
}

/// A project made personal on purpose doesn't go back to a space without `--force`, even if a
/// client repository keeps suggesting it (the command the warning suggests carries `--force`).
#[test]
fn made_personal_does_not_return_to_a_space_without_force() {
    let e = Env::new("personal");
    e.id("o.db", "fact", "Algo mío", "Para que exista.");
    e.ok("o.db", &["space", "add", "acme"]);
    e.ok("o.db", &["space", "assign", "acme"]);
    e.ok("o.db", &["space", "unassign", "--force"]);
    for args in [vec!["space", "assign", "acme"], vec!["projects", "move", ".", "--space", "acme"]] {
        let o = e.omoi("o.db", &args, None);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("personal on purpose"), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }
    e.ok("o.db", &["space", "assign", "acme", "--force"]);
    // Once inside again, the mark goes away: taking it out and back in asks the usual.
    e.ok("o.db", &["space", "unassign", "--force"]);
    e.ok("o.db", &["projects", "move", ".", "--space", "acme", "--force"]);
}
