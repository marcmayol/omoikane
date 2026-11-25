//! The CLI end to end, as a person or a hook will use it: the real `omoi` binary, with
//! its own database and inside a test repository.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct TestEnv {
    dir: common::TempDir,
}

impl TestEnv {
    fn new(name: &str) -> TestEnv {
        let dir = common::TempDir::new(&format!("cli-{name}"));
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("repo").join("src")).unwrap();
        TestEnv { dir }
    }

    fn omoi(&self, args: &[&str]) -> Output {
        self.omoi_with(args, None, &[])
    }

    fn omoi_with(&self, args: &[&str], input: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo").join("src"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        if let Some(e) = input {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
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

fn exists(p: &Path) -> bool {
    p.exists()
}

/// The "done" criterion of phase 1.
#[test]
fn remember_and_search_find_auth_bug() {
    let e = TestEnv::new("hecho");
    assert!(!exists(&e.dir.join("o.db")));
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
    // The project is the repo root even when run from src/.
    assert!(r[0]["project"].as_str().unwrap().ends_with("repo"));
    assert!(e.ok(&["search", "fallo", "de", "autenticacion"]).contains("Bug en auth"));
}

#[test]
fn content_from_stdin() {
    let e = TestEnv::new("stdin");
    let o = e.omoi_with(&["remember", "-k", "decision", "-t", "Por stdin", "--json"], Some("Llega por la tubería.\n"), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(m["content"], "Llega por la tubería.");
    // No content and no input: clear error, nothing saved.
    let o = e.omoi(&["remember", "-k", "fact", "-t", "Vacía"]);
    assert!(!o.status.success());
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
}

#[test]
fn update_history_and_forget() {
    let e = TestEnv::new("ciclo");
    let m = e.json(&["remember", "-k", "decision", "-t", "Usamos UUID", "Por compatibilidad."]);
    let id = m["id"].as_str().unwrap();
    let short = &id[..12];
    e.ok(&["update", short, "-t", "Usamos ULID", "-c", "Se ordenan por fecha.", "-r", "los UUID no se ordenan"]);
    let h = e.json(&["history", short]);
    assert_eq!(h[0]["title"], "Usamos UUID");
    assert_eq!(h[0]["reason"], "los UUID no se ordenan");
    assert!(e.ok(&["get", short]).contains("Se ordenan por fecha."));
    e.ok(&["update", short, "--pin"]);
    assert_eq!(e.json(&["get", short])["pinned"], true);
    // Without --yes and outside a terminal, it refuses and deletes nothing.
    let o = e.omoi(&["forget", short]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    assert!(e.omoi(&["get", short]).status.success());
    assert!(e.ok(&["forget", short, "--yes"]).contains("Usamos ULID"));
    assert!(!e.omoi(&["get", short]).status.success());
}

#[test]
fn move_between_projects() {
    let e = TestEnv::new("mover");
    let other = e.dir.join("otro");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let id = e.json(&["remember", "-k", "fact", "-t", "La web usa Astro", "Astro 5 con Tailwind."])["id"].as_str().unwrap().to_string();
    let v = e.json(&["update", &id, "--project", other.join(".").to_str().unwrap()]);
    assert!(v["project"].as_str().unwrap().ends_with("otro"), "{v}");
    assert!(e.json(&["history", &id]).as_array().unwrap().is_empty(), "mover no crea versión");
    // From the previous repo it is no longer seen; from the new one, it is.
    assert!(e.json(&["search", "astro"]).as_array().unwrap().is_empty());
    assert_eq!(e.json(&["--dir", other.to_str().unwrap(), "search", "astro"]).as_array().unwrap().len(), 1);
    let g = e.json(&["update", &id, "--global"]);
    assert_eq!(g["project"], "");
    assert_eq!(e.json(&["search", "astro"]).as_array().unwrap().len(), 1);
}

#[test]
fn global_is_seen_from_any_project() {
    let e = TestEnv::new("global");
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea, en inglés."]);
    let other = e.dir.join("otro");
    std::fs::create_dir_all(&other).unwrap();
    let r = e.json(&["--dir", other.to_str().unwrap(), "search", "commits"]);
    assert_eq!(r[0]["project"], "");
    // Global is not a project: `omoi projects` does not list it.
    let p = e.json(&["projects"]);
    assert!(p.as_array().unwrap().is_empty(), "{p}");
}

#[test]
fn export_jsonl() {
    let e = TestEnv::new("export");
    e.ok(&["remember", "-k", "fact", "-t", "Una", "uno"]);
    e.ok(&["remember", "-k", "fact", "-t", "Dos", "dos"]);
    let output = e.ok(&["export"]);
    let lines: Vec<serde_json::Value> = output.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    // Header, the two memories and the end line (the whole format is tested in `backup.rs`).
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["type"], "header");
    assert!(lines[1..3].iter().all(|l| l["type"] == "memory" && l["versions"].is_null()));
    assert_eq!(lines[3]["count"], 2);
}

#[test]
fn export_to_obsidian() {
    let e = TestEnv::new("obsidian");
    e.ok(&["remember", "-k", "gotcha", "-t", "Cuidado con X", "--tag", "nota-a", "--path", "src/x.rs", "Ver [[nota-b]], [[no-existe]] y el rojo #d62828."]);
    let b = e.json(&["remember", "-k", "decision", "-t", "Usamos Y", "--tag", "nota-b", "--path", "src/x.rs", "Porque sí."]);
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea."]);
    e.ok(&["space", "add", "acme"]);
    e.ok(&["remember", "-k", "fact", "-t", "Del cliente", "--space=acme", "Solo de acme."]);
    let vault = e.dir.join("vault");
    let root = vault.join("omoikane");
    // Two memories with the same title: both carry the end of their id.
    let r1 = e.json(&["remember", "-k", "fact", "-t", "Repetida", "--global", "Una."]);
    let r2 = e.json(&["remember", "-k", "fact", "-t", "Repetida", "--global", "--force", "Otra."]);
    let suffix = |id: &serde_json::Value| id.as_str().unwrap()[18..].to_lowercase();
    let (fa, fb) = ("Cuidado con X".to_string(), "Usamos Y".to_string());
    e.ok(&["export", "--obsidian", vault.to_str().unwrap()]);
    for r in [&r1, &r2] {
        let global = std::fs::read_to_string(root.join("_global").join(format!("Repetida ({}).md", suffix(&r["id"])))).unwrap();
        assert!(!global.contains("[[omoikane-global") && !global.contains("
# "), "lo global no cuelga de ninguna nota de grupo: {global}");
    }
    let note_a = std::fs::read_to_string(root.join("repo").join(format!("{fa}.md"))).unwrap();
    assert!(note_a.starts_with("---\nomoikane: "), "{note_a}");
    assert!(note_a.contains(&format!("[[{fb}|nota-b]]")), "{note_a}");
    assert!(note_a.contains("no-existe") && !note_a.contains("[[no-existe"), "un enlace roto va en texto: {note_a}");
    assert!(note_a.contains("\\#d62828") && note_a.contains("tags: [\"gotcha\", \"nota-a\"]"), "{note_a}");
    assert!(note_a.contains("[[proyecto-repo]]"));
    assert!(root.join("repo").join("proyecto-repo.md").exists() && root.join("_global").exists());
    assert_eq!(std::fs::read_dir(root.join("_ficheros")).unwrap().count(), 1, "src/x.rs, citado dos veces");
    assert!(vault.join(".obsidian").join("graph.json").exists());
    let all_text = |d: &Path| -> String {
        let mut s = String::new();
        let mut stack = vec![d.to_path_buf()];
        while let Some(p) = stack.pop() {
            for x in std::fs::read_dir(&p).unwrap().flatten() {
                if x.path().is_dir() { stack.push(x.path()) } else { s += &std::fs::read_to_string(x.path()).unwrap_or_default() }
            }
        }
        s
    };
    assert!(!all_text(&root).contains("Solo de acme"), "nada de un espacio en el vault personal");

    // A space, in its vault: only its own, without the global ones.
    let other = e.dir.join("vault-acme");
    e.ok(&["export", "--obsidian", other.to_str().unwrap(), "--space", "acme"]);
    let t = all_text(&other.join("omoikane"));
    assert!(t.contains("Solo de acme") && !t.contains("Commits en inglés") && !t.contains("Cuidado con X"));

    // What is forgotten disappears, the link to it stays as text and what is not Omoikane's stays.
    std::fs::write(root.join("repo").join("mia.md"), "---\ntitle: mía\n---\nNota mía.\n").unwrap();
    e.ok(&["forget", b["id"].as_str().unwrap(), "--yes"]);
    let n = e.json(&["export", "--obsidian", vault.to_str().unwrap()]);
    assert!(n["deleted"].as_u64().unwrap() >= 2, "la nota y la del fichero: {n}");
    assert!(!root.join("repo").join(format!("{fb}.md")).exists() && root.join("repo").join("mia.md").exists());
    let note_a = std::fs::read_to_string(root.join("repo").join(format!("{fa}.md"))).unwrap();
    assert!(note_a.contains("nota-b") && !note_a.contains("[[Usamos Y"), "{note_a}");
    // With no changes, nothing is written.
    let n = e.json(&["export", "--obsidian", vault.to_str().unwrap()]);
    assert_eq!((n["created"].as_u64(), n["updated"].as_u64(), n["deleted"].as_u64()), (Some(0), Some(0), Some(0)));
}

/// Reads a note and writes it back changed.
fn edit(path: &Path, f: impl FnOnce(String) -> String) {
    let t = std::fs::read_to_string(path).unwrap();
    let new = f(t.clone());
    assert_ne!(t, new, "la edición no cambia nada en {}", path.display());
    std::fs::write(path, new).unwrap();
}

#[test]
fn import_from_obsidian() {
    let e = TestEnv::new("obsidian-vuelta");
    let a = e.json(&["remember", "-k", "gotcha", "-t", "Cuidado con X", "--tag", "nota-a", "--tag", "review:2099-01-01", "Ver [[nota-b]] y [[no-existe]].\nEl rojo es #d62828.\nTercera línea."]);
    let b = e.json(&["remember", "-k", "decision", "-t", "Usamos Y", "--tag", "nota-b", "Porque sí."]);
    let g = e.json(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "--pinned", "Una línea."]);
    e.ok(&["space", "add", "acme"]);
    let s = e.json(&["remember", "-k", "fact", "-t", "Del cliente", "--space=acme", "Solo de acme."]);
    let id = |m: &serde_json::Value| m["id"].as_str().unwrap().to_string();
    let vault = e.dir.join("vault");
    let v = vault.to_str().unwrap();
    let root = vault.join("omoikane");
    let note_a = root.join("repo").join("Cuidado con X.md");
    let note_b = root.join("repo").join("Usamos Y.md");
    let note_g = root.join("_global").join("Commits en inglés.md");
    e.ok(&["export", "--obsidian", v]);
    let text = std::fs::read_to_string(&note_a).unwrap();
    assert!(text.contains("\nbase: \"") && text.contains("\nsig: \"") && text.contains("%% omoikane:"), "{text}");

    // Exporting and importing without touching anything changes nothing, and neither does repeating.
    let before = e.json(&["get", &id(&a)]);
    for _ in 0..2 {
        let r = e.json(&["import", "--obsidian", v]);
        assert_eq!((r["updated"].as_u64(), r["created"].as_u64(), r["unchanged"].as_u64()), (Some(0), Some(0), Some(3)), "{r}");
        assert_eq!((r["export"]["created"].as_u64(), r["export"]["updated"].as_u64(), r["export"]["deleted"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
    }
    assert_eq!(e.json(&["get", &id(&a)]), before);
    assert_eq!(e.json(&["history", &id(&a)]).as_array().map(Vec::len), Some(0));

    // An edited note updates its memory, with history; untouched lines stay as they were
    // (the links and the `#` in the content), and the changed one goes back to Omoikane's form.
    edit(&note_a, |t| {
        t.replace("Tercera línea.", "Tercera línea, editada en \\#obsidian y con [[Usamos Y]].")
            .replace("aliases: [\"Cuidado con X\"]", "aliases: [\"Cuidado con X y Z\"]")
            .replace("tags: [\"gotcha\", \"nota-a\", \"review-2099-01-01\"]", "tags: [\"gotcha\", \"nota-a\", \"nueva\", \"trade:web\"]")
            .replace("kind: gotcha", "kind: fact")
    });
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!((r["updated"].as_u64(), r["unchanged"].as_u64()), (Some(1), Some(2)), "{r}");
    let m = e.json(&["get", &id(&a)]);
    assert_eq!(m["title"], "Cuidado con X y Z");
    assert_eq!(m["kind"], "fact");
    assert_eq!(m["content"], format!("Ver [[nota-b]] y [[no-existe]].\nEl rojo es #d62828.\nTercera línea, editada en #obsidian y con [[{}]].", id(&b)));
    let tags: Vec<&str> = m["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert!(tags.contains(&"nueva") && tags.contains(&"review:2099-01-01") && !tags.iter().any(|t| t.starts_with("trade")), "las que llevan `:` no se tocan: {tags:?}");
    let h = e.json(&["history", &id(&a)]);
    assert_eq!(h[0]["title"], "Cuidado con X");
    assert_eq!(h[0]["reason"], "obsidian import");
    // The vault is up to date (the note, with its new name) and another import does nothing.
    assert!(!note_a.exists() && root.join("repo").join("Cuidado con X y Z.md").exists());
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!((r["updated"].as_u64(), r["unchanged"].as_u64()), (Some(0), Some(3)), "{r}");

    // Nothing grants privileges: in the pinned global one, what changes the scope is not applied (the text is).
    edit(&note_g, |t| t.replace("pinned: true\n", "").replace("project: \"Global\"", "project: \"repo\"").replace("Una línea.", "Una línea, en Obsidian."));
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["updated"].as_u64(), Some(1), "{r}");
    let m = e.json(&["get", &id(&g)]);
    assert_eq!((m["pinned"].as_bool(), m["project"].as_str(), m["content"].as_str()), (Some(true), Some(""), Some("Una línea, en Obsidian.")), "{m}");
    // A note pointing to another memory (by hand, or to one in a space) does not touch it: it lacks its signature.
    let text_b = std::fs::read_to_string(&note_b).unwrap();
    std::fs::write(root.join("repo").join("Falsa.md"), text_b.replace(&id(&b), &id(&s)).replace("Porque sí.", "Robado.")).unwrap();
    std::fs::write(root.join("repo").join("Falsa 2.md"), text_b.replace(&id(&b), &id(&g)).replace("Porque sí.", "Robado.")).unwrap();
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["foreign"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(e.json(&["get", &id(&s)])["content"], "Solo de acme.");
    assert_eq!(e.json(&["get", &id(&g)])["content"], "Una línea, en Obsidian.");
    // Its text may be someone's: the following export does not overwrite or delete them either.
    assert_eq!(r["export"]["kept"].as_u64(), Some(2), "{r}");
    for f in ["Falsa.md", "Falsa 2.md"] {
        assert!(std::fs::read_to_string(root.join("repo").join(f)).unwrap().contains("Robado."));
        std::fs::remove_file(root.join("repo").join(f)).unwrap();
    }

    // Conflict: changed in Obsidian and in Omoikane. Neither is overwritten, not even on export; then one is chosen.
    edit(&note_b, |t| t.replace("Porque sí.", "Porque lo dijo Obsidian."));
    e.ok(&["update", &id(&b), "-c", "Porque lo dijo Omoikane."]);
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["conflicts"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["export"]["kept"].as_u64(), Some(1), "{r}");
    assert_eq!(e.json(&["get", &id(&b)])["content"], "Porque lo dijo Omoikane.");
    assert!(std::fs::read_to_string(&note_b).unwrap().contains("Porque lo dijo Obsidian."));
    let n = e.json(&["export", "--obsidian", v]);
    assert_eq!(n["kept"].as_u64(), Some(1), "{n}");
    assert!(std::fs::read_to_string(&note_b).unwrap().contains("Porque lo dijo Obsidian."));
    let r = e.json(&["import", "--obsidian", v, "--prefer", "obsidian"]);
    assert_eq!((r["updated"].as_u64(), r["conflicts_applied"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!(e.json(&["get", &id(&b)])["content"], "Porque lo dijo Obsidian.");
    edit(&note_b, |t| t.replace("Porque lo dijo Obsidian.", "Otra vez Obsidian."));
    e.ok(&["update", &id(&b), "-c", "Otra vez Omoikane."]);
    let r = e.json(&["import", "--obsidian", v, "--prefer", "omoikane"]);
    assert_eq!(r["conflicts_discarded"].as_u64(), Some(1), "{r}");
    assert!(std::fs::read_to_string(&note_b).unwrap().contains("Otra vez Omoikane."));
    assert_eq!(e.json(&["get", &id(&b)])["content"], "Otra vez Omoikane.");

    // New notes: in a project's folder and with a kind, they get in; without a kind, in _global or with no
    // project, they do not (and it says why).
    std::fs::write(root.join("repo").join("Idea nueva.md"), "---\nkind: decision\ntags: [\"x\", \"trade:web\"]\npinned: true\n---\nUsar Z.\n").unwrap();
    std::fs::write(root.join("repo").join("Sin tipo.md"), "Algo.\n").unwrap();
    std::fs::write(root.join("_global").join("Global a mano.md"), "---\nkind: fact\n---\nPara todos.\n").unwrap();
    std::fs::create_dir_all(root.join("otra")).unwrap();
    std::fs::write(root.join("otra").join("Suelta.md"), "---\nkind: fact\n---\nDe nadie.\n").unwrap();
    let r = e.json(&["import", "--obsidian", v, "--dry-run"]);
    assert_eq!((r["created"].as_u64(), r["skipped"].as_array().map(Vec::len)), (Some(1), Some(3)), "{r}");
    assert!(root.join("repo").join("Idea nueva.md").exists());
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["created"].as_u64(), Some(1), "{r}");
    let recent = e.json(&["recent"]);
    let new_one = recent.as_array().unwrap().iter().find(|m| m["title"] == "Idea nueva").expect("la nota nueva es una memoria");
    assert_eq!((new_one["kind"].as_str(), new_one["pinned"].as_bool(), new_one["project"].as_str().is_some_and(|p| p.ends_with("repo"))), (Some("decision"), Some(false), true), "{new_one}");
    assert!(!new_one["tags"].as_array().unwrap().iter().any(|t| t.as_str().unwrap().starts_with("trade:")), "{new_one}");
    // Its file becomes Omoikane's copy, and another import does not duplicate it.
    assert!(std::fs::read_to_string(root.join("repo").join("Idea nueva.md")).unwrap().starts_with("---\nomoikane: "));
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!((r["created"].as_u64(), r["updated"].as_u64()), (Some(0), Some(0)), "{r}");

    // A space's vault is imported with its --space; without it, not.
    let other = e.dir.join("vault-acme");
    e.ok(&["export", "--obsidian", other.to_str().unwrap(), "--space", "acme"]);
    let o = e.omoi(&["import", "--obsidian", other.to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--space acme"), "{}", String::from_utf8_lossy(&o.stderr));
    edit(&other.join("omoikane").join("_acme").join("Del cliente.md"), |t| t.replace("Solo de acme.", "Solo de acme, editado."));
    let r = e.json(&["import", "--obsidian", other.to_str().unwrap(), "--space", "acme"]);
    assert_eq!(r["updated"].as_u64(), Some(1), "{r}");
    assert_eq!(e.json(&["get", &id(&s)])["content"], "Solo de acme, editado.");
}

/// What the way back from Obsidian cannot bring is not lost: a new unsigned note does not overwrite the
/// memory that already came from it, the export after importing does not overwrite or delete edited
/// unsigned notes (a 1.8.6 vault), and what is masked as a secret is counted.
#[test]
fn obsidian_what_does_not_get_in_is_not_lost() {
    let e = TestEnv::new("obsidian-sin-firma");
    let a = e.json(&["remember", "-k", "fact", "-t", "Despliegue", "Se despliega a mano."]);
    let b = e.json(&["remember", "-k", "fact", "-t", "Puerto", "Escucha en el 8010."]);
    let id = |m: &serde_json::Value| m["id"].as_str().unwrap().to_string();
    let vault = e.dir.join("vault");
    let v = vault.to_str().unwrap();
    let folder = vault.join("omoikane").join("repo");
    e.ok(&["export", "--obsidian", v]);

    // A new note creates its memory; that memory is changed, pinned and verified in Omoikane.
    let idea = folder.join("idea.md");
    std::fs::write(&idea, "---\nkind: decision\n---\nLa base de datos es Postgres.\n").unwrap();
    assert_eq!(e.json(&["import", "--obsidian", v])["created"].as_u64(), Some(1));
    let m = e.json(&["recent"]).as_array().unwrap().iter().find(|m| m["content"] == "La base de datos es Postgres.").cloned().unwrap();
    e.ok(&["update", &id(&m), "--pin", "-c", "La base de datos es Postgres 16 (revisado).", "-r", "a mano"]);
    let as_person: Vec<(String, &str)> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("CLAUDE") || k == "AI_AGENT").map(|k| (k, "")).collect();
    let as_person: Vec<(&str, &str)> = as_person.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let o = e.omoi_with(&["verify", &id(&m), "--yes"], None, &as_person);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.json(&["get", &id(&m)])["verified"].is_object());
    // Rewriting it without a header (unsigned) does not overwrite it: it is skipped, reported, and the file stays.
    std::fs::write(&idea, "---\nkind: decision\n---\nLa base de datos es MongoDB, sin backups.\n").unwrap();
    for _ in 0..2 {
        let r = e.json(&["import", "--obsidian", v]);
        assert_eq!((r["updated"].as_u64(), r["created"].as_u64(), r["unverified"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
        let s = r["skipped"].as_array().unwrap();
        assert!(s.len() == 1 && s[0]["note"] == "repo/idea.md" && s[0]["reason"].as_str().unwrap().contains(&id(&m)), "{r}");
        let now = e.json(&["get", &id(&m)]);
        assert_eq!((now["content"].as_str(), now["pinned"].as_bool(), now["verified"].is_object()), (Some("La base de datos es Postgres 16 (revisado)."), Some(true), true), "{now}");
        assert!(std::fs::read_to_string(&idea).unwrap().contains("MongoDB"), "la exportación no pisa la nota");
    }
    // An unsigned note that says the same as its memory does nothing (and makes room for its copy).
    std::fs::write(&idea, "---\nkind: decision\n---\nLa base de datos es Postgres 16 (revisado).\n").unwrap();
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!((r["updated"].as_u64(), r["skipped"].as_array().map(Vec::len)), (Some(0), Some(0)), "{r}");
    assert!(std::fs::read_to_string(&idea).unwrap().starts_with("---\nomoikane: "));

    // A 1.8.6 vault (without `base`, `sig` or the invisible rule, with the notice of back then): the untouched
    // note is rebuilt signed; the edited one does not get in and the following export does not overwrite it.
    let a_md = folder.join("Despliegue.md");
    let b_md = folder.join("Puerto.md");
    let to_186 = |t: String| -> String {
        let t: String = t.lines().filter(|l| !l.starts_with("base: ") && !l.starts_with("sig: ")).map(|l| format!("{l}\n")).collect();
        let t = t.replace("\n%% omoikane: below this line, rewritten on every export %%\n", "");
        let (body, _) = t.rsplit_once("\n*").unwrap();
        format!("{body}\n*A copy from Omoikane: rewritten on every export; changes made here are lost.*\n")
    };
    for f in [&a_md, &b_md] {
        edit(f, to_186);
    }
    edit(&a_md, |t| t.replace("Se despliega a mano.", "Se despliega a mano. EDICIÓN IMPORTANTE"));
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["foreign"].as_array().unwrap(), &vec![serde_json::json!("repo/Despliegue.md")], "{r}");
    assert_eq!((r["updated"].as_u64(), r["export"]["kept"].as_u64()), (Some(0), Some(1)), "{r}");
    assert!(std::fs::read_to_string(&a_md).unwrap().contains("EDICIÓN IMPORTANTE"), "la edición sigue en el vault");
    assert_eq!(e.json(&["get", &id(&a)])["content"], "Se despliega a mano.");
    assert!(std::fs::read_to_string(&b_md).unwrap().contains("\nsig: \""), "la intacta se rehace firmada");
    assert_eq!(e.json(&["get", &id(&b)])["content"], "Escucha en el 8010.");
    // What the message says is true: without the `omoikane:` line, it gets in as a new memory.
    edit(&a_md, |t| t.replacen(&format!("omoikane: \"{}\"\n", id(&a)), "", 1));
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!(r["created"].as_u64(), Some(1), "{r}");

    // A secret added in a signed note is masked and counted.
    let token = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
    edit(&b_md, |t| t.replace("Escucha en el 8010.", &format!("Escucha en el 8010. Token {token}")));
    let r = e.json(&["import", "--obsidian", v, "--dry-run"]);
    assert_eq!(r["redacted"].as_array().map(Vec::len), Some(1), "{r}");
    let r = e.json(&["import", "--obsidian", v]);
    assert_eq!((r["updated"].as_u64(), r["redacted"].as_array().map(Vec::len)), (Some(1), Some(1)), "{r}");
    let c = e.json(&["get", &id(&b)])["content"].as_str().unwrap().to_string();
    assert!(c.contains("[redacted:github-token]") && !c.contains(&token), "{c}");
}

#[test]
fn help_in_english_and_spanish() {
    let e = TestEnv::new("idioma");
    let en = String::from_utf8(e.omoi(&["remember", "--help"]).stdout).unwrap();
    assert!(en.contains("Save a memory"), "{en}");
    let o = e.omoi_with(&["remember", "--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Guarda una memoria"), "{es}");
    assert!(es.contains("Uso:") && es.contains("Opciones:") && !es.contains("Options:"), "{es}");
    let o = e.omoi_with(&["--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Comandos:") && !es.contains("Commands:") && es.contains("Muestra la versión"), "{es}");
    // Global options show in subcommands, in Spanish too.
    let o = e.omoi_with(&["search", "-h"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Base de datos") && es.contains("Argumentos:"), "{es}");
    // Errors too.
    let o = e.omoi_with(&["get", "ZZZZZZZZ"], None, &[("OMOIKANE_LANG", "es")]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no existe"));
    let o = e.omoi(&["get", "ZZZZZZZZ"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no memory"));
}

#[test]
fn unknown_kind_fails_without_saving() {
    let e = TestEnv::new("tipo");
    let o = e.omoi(&["remember", "-k", "bugfix", "-t", "X", "y"]);
    assert!(!o.status.success());
    assert!(!e.dir.join("o.db").exists() || e.json(&["recent"]).as_array().unwrap().is_empty());
}

fn int8_model() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

/// Phase 2 end to end: without a model it works by words; with it, it warns about duplicates and
/// finds by meaning.
#[test]
fn model_duplicates_and_meaning() {
    let Some(int8) = int8_model() else {
        eprintln!("SALTADO: no está el modelo int8");
        return;
    };
    let e = TestEnv::new("modelo");
    assert!(e.ok(&["model"]).contains("no model installed"));
    e.ok(&["remember", "-k", "gotcha", "-t", "Bug en auth: el token caduca antes de tiempo", "El middleware compara la hora local con la caducidad del token, que va en UTC."]);

    let installed = e.ok(&["model", "install", "--from", int8.to_str().unwrap()]);
    assert!(installed.contains("model ready") && installed.contains("1 vectors"), "{installed}");
    assert!(e.ok(&["model"]).contains("* potion-multilingual-128M-int8-"));
    let doctor = e.ok(&["doctor"]);
    assert!(doctor.contains("vectors    1"), "{doctor}");

    // With a model, what comes out by words carries its similarity.
    let r = e.json(&["search", "el token caduca"]);
    assert_eq!(r[0]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert!(r[0]["similarity"].as_f64().is_some());

    // Almost the same: not saved, exit code 2, and it says which one is similar.
    let similar = ["remember", "-k", "gotcha", "-t", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token en UTC."];
    let o = e.omoi(&similar);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stdout).contains("not saved"));
    let mut with_json = similar.to_vec();
    with_json.push("--json");
    let o = e.omoi(&with_json);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["duplicate_of"]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
    // With --force, it is.
    let mut forced = similar.to_vec();
    forced.push("--force");
    e.ok(&forced);
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 2);

    assert!(e.ok(&["reindex"]).contains("2 memories, 2 vectors"));
    assert!(e.ok(&["model", "clean"]).contains("0 models deleted"));
}

/// `omoi tool`, what the plugin mod uses: the tool list and one call of each kind,
/// with the same responses as the MCP server (and exit 1 when the tool responds with an error).
#[test]
fn tools_without_mcp() {
    let e = TestEnv::new("tool");
    let list: serde_json::Value = serde_json::from_str(&e.ok(&["tool"])).unwrap();
    let names: Vec<&str> = list["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["remember", "search", "get", "update", "forget", "history", "context", "recent", "decisions"] {
        assert!(names.contains(&n), "falta {n} en {names:?}");
    }
    assert!(list["instructions"].as_str().unwrap().contains("long-term memory"));

    let o = e.omoi_with(
        &["tool", "remember"],
        Some(r#"{"title":"Puerto del servidor","content":"Escucha en el 8010","kind":"fact"}"#),
        &[("OMOIKANE_SOURCE", "mcp:claude-code")],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let saved: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(saved["saved"], true);
    let full = e.json(&["get", saved["id"].as_str().unwrap()]);
    assert_eq!(full["source"], "mcp:claude-code", "quien llama a `omoi tool` dice quién es");

    let o = e.omoi_with(&["tool", "search"], Some(r#"{"query":"puerto servidor"}"#), &[]);
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r[0]["id"], saved["id"]);

    let o = e.omoi_with(&["tool", "get"], Some(r#"{"id":"ZZZZZZ"}"#), &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stdout).contains("ZZZZZZ"));
}

/// `omoi import` with an engram export: kinds, personal scope → global, deleted ones left out,
/// dates kept, and a second import does not duplicate.
#[test]
fn import_from_engram() {
    let e = TestEnv::new("import");
    let file = e.dir.join("engram.json");
    std::fs::write(
        &file,
        r#"{"version":"0.2.0","sessions":[{"id":"s1","project":"tienda","directory":"/no/existe"}],
           "observations":[
             {"id":1,"session_id":"s1","type":"bugfix","title":"El importe en f32 pierde céntimos","content":"Guardar el dinero en céntimos enteros.","project":"tienda","scope":"project","topic_key":"pagos/importe","pinned":false,"created_at":"2025-03-01 10:00:00","updated_at":"2025-03-02 11:00:00"},
             {"id":2,"session_id":"s1","type":"preference","title":"Commits en inglés","content":"Una sola línea.","project":"tienda","scope":"personal","pinned":"true","created_at":"2025-03-01 10:00:00","updated_at":"2025-03-01 10:00:00"},
             {"id":3,"session_id":"s1","type":"architecture","title":"Hexagonal","content":"Puertos y adaptadores.","project":"tienda","scope":"project","created_at":"2025-03-01 10:00:00","updated_at":"2025-03-01 10:00:00"},
             {"id":4,"session_id":"s1","type":"decision","title":"Borrada","content":"x","project":"tienda","scope":"project","deleted_at":"2025-04-01 00:00:00"}
           ]}"#,
    )
    .unwrap();
    let f = file.to_str().unwrap();
    let output = e.ok(&["import", f]);
    assert!(output.contains("imported 3") && output.contains("1 deleted"), "{output}");
    let all = e.json(&["recent", "--all", "-n", "10"]);
    let by_title = |t: &str| all.as_array().unwrap().iter().find(|m| m["title"] == t).unwrap().clone();
    let bug = by_title("El importe en f32 pierde céntimos");
    assert_eq!(bug["kind"], "gotcha");
    assert!(bug["created_at"].as_str().unwrap().starts_with("2025-03-01T10:00:00"));
    assert!(bug["tags"].as_array().unwrap().iter().any(|t| t == "topic:pagos/importe"));
    let pref = by_title("Commits en inglés");
    assert_eq!(pref["project"], "");
    assert_eq!(pref["pinned"], true);
    assert!(by_title("Hexagonal")["tags"].as_array().unwrap().iter().any(|t| t == "architecture"));
    assert!(all.as_array().unwrap().iter().all(|m| m["title"] != "Borrada"));
}

/// `omoi setup <agent>` in a test home folder: writes what it should, respects what was
/// already there, keeps a copy of the original and a second run changes nothing.
#[test]
fn setup_for_other_agents() {
    let e = TestEnv::new("setup");
    let home = e.dir.join("casa");
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::write(home.join(".gemini/settings.json"), r#"{"theme":"dark","mcpServers":{"otro":{"command":"x"}}}"#).unwrap();
    let h = home.to_str().unwrap();
    for a in omoikane::agents::AGENTS {
        let o = e.omoi_with(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(o.status.success(), "{a}: {}", String::from_utf8_lossy(&o.stderr));
        let o = e.omoi_with(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("nothing to do"), "{a} la segunda vez");
    }
    let read = |f: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(home.join(f)).unwrap()).unwrap();
    let g = read(".gemini/settings.json");
    assert_eq!(g["theme"], "dark");
    assert_eq!(g["mcpServers"]["otro"]["command"], "x");
    assert_eq!(g["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(g["hooks"]["BeforeAgent"].to_string().contains("--format gemini"));
    assert!(home.join(".gemini/settings.json.antes-de-omoikane").exists());
    assert!(read(".codex/hooks.json")["hooks"]["UserPromptSubmit"].to_string().contains("recall"));
    assert!(std::fs::read_to_string(home.join(".codex/config.toml")).unwrap().contains("[mcp_servers.omoikane]"));
    assert!(read(".cursor/hooks.json")["hooks"]["sessionStart"].to_string().contains("--format cursor"));
    assert_eq!(read(".config/opencode/opencode.json")["mcp"]["omoikane"]["type"], "local");
    let plugin = std::fs::read_to_string(home.join(".config/opencode/plugins/omoikane.ts")).unwrap();
    // No real line break inside a JS string (it broke the plugin silently).
    assert!(plugin.contains("experimental.chat.system.transform") && !plugin.contains("+= \"\n"));
    for f in [".codex/AGENTS.md", ".gemini/GEMINI.md", ".config/opencode/AGENTS.md", ".qwen/QWEN.md", ".copilot/copilot-instructions.md", ".pi/agent/AGENTS.md"] {
        assert_eq!(std::fs::read_to_string(home.join(f)).unwrap().matches("<!-- omoikane -->").count(), 1, "{f}");
    }
    // Every JSON it writes can be read and has the server where it belongs.
    assert_eq!(read(".qwen/settings.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(read(".qwen/settings.json")["hooks"]["UserPromptSubmit"].to_string().contains("recall"));
    assert_eq!(read(".kiro/settings/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(read(".kiro/hooks/omoikane.json")["hooks"].to_string().contains("--format plain"));
    assert_eq!(read(".copilot/mcp-config.json")["mcpServers"]["omoikane"]["type"], "local");
    assert!(read(".copilot/hooks/omoikane.json")["hooks"]["SessionStart"].to_string().contains("--format copilot"));
    assert_eq!(read(".codeium/windsurf/mcp_config.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert_eq!(read(".gemini/config/mcp_config.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert!(read(".gemini/config/hooks.json")["omoikane"]["SessionStart"].to_string().contains("--format antigravity"));
    assert_eq!(read(".kimi-code/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    assert_eq!(read(".pi/agent/mcp.json")["mcpServers"]["omoikane"]["args"][0], "mcp");
    // Grok: MCP in TOML, its rules, and the closing hooks (no recall: its hooks do not add context).
    assert!(std::fs::read_to_string(home.join(".grok/config.toml")).unwrap().contains("[mcp_servers.omoikane]"));
    assert_eq!(std::fs::read_to_string(home.join(".grok/rules/omoikane.md")).unwrap().matches("<!-- omoikane -->").count(), 1);
    let grok = read(".grok/hooks/omoikane.json");
    assert!(grok["hooks"]["SessionEnd"].to_string().contains("--session-end") && grok["hooks"]["PreCompact"].to_string().contains("--session-end"), "{grok}");
    assert_eq!(grok["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    assert!(grok["hooks"].get("UserPromptSubmit").is_none());
    assert_eq!(read(".config/amp/settings.json")["amp.mcpServers"]["omoikane"]["args"][0], "mcp");
    let kilo = read(".config/kilo/kilo.jsonc");
    assert_eq!(kilo["mcp"]["omoikane"]["type"], "local");
    assert!(kilo["instructions"][0].as_str().unwrap().ends_with("omoikane.md"));
    // The /omoikane-web command where the agent supports user commands: opens the web view.
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
        let t = std::fs::read_to_string(home.join(f)).unwrap_or_else(|_| panic!("falta {f}"));
        assert!(t.contains("serve --open") && t.contains("127.0.0.1:7438"), "{f}: {t}");
        // And next to it `/omoikane`, which makes the agent save.
        let g = f.replace("omoikane-web", "omoikane");
        let t = std::fs::read_to_string(home.join(&g)).unwrap_or_else(|_| panic!("falta {g}"));
        assert!(t.contains("Save to Omoikane now") && t.contains("`status`") && !t.contains("serve --open"), "{g}: {t}");
    }
    let gemini = std::fs::read_to_string(home.join(".gemini/commands/omoikane.toml")).unwrap();
    assert!(gemini.starts_with("description = \"Save to Omoikane") && gemini.contains("
prompt = \""), "{gemini}");
    let gemini = std::fs::read_to_string(home.join(".gemini/commands/omoikane-web.toml")).unwrap();
    assert!(gemini.starts_with("description = \"") && gemini.contains("\nprompt = \""), "{gemini}");
    assert!(std::fs::read_to_string(home.join(".config/opencode/commands/omoikane-web.md")).unwrap().starts_with("---\ndescription:"));
}

/// 7.5: each agent's closing hooks, also over what a previous `setup` left.
#[test]
fn setup_sets_aside_on_close_in_other_agents() {
    let e = TestEnv::new("setup-cierre");
    let home = e.dir.join("casa");
    // What 0.12 left: the Kiro and Copilot hooks without the closing one, and the old OpenCode plugin.
    std::fs::create_dir_all(home.join(".kiro/hooks")).unwrap();
    std::fs::write(home.join(".kiro/hooks/omoikane.json"), r#"{"version":"v1","hooks":[{"name":"omoikane-session","trigger":"SessionStart","action":{"type":"command","command":"omoi recall --session-start --format plain"},"timeout":30}]}"#).unwrap();
    std::fs::create_dir_all(home.join(".copilot/hooks")).unwrap();
    std::fs::write(home.join(".copilot/hooks/omoikane.json"), r#"{"version":1,"hooks":{"SessionStart":[{"type":"command","bash":"omoi recall --session-start --format copilot"}]}}"#).unwrap();
    std::fs::create_dir_all(home.join(".config/opencode/plugins")).unwrap();
    std::fs::write(home.join(".config/opencode/plugins/omoikane.ts"), "// Omoikane: recall automático en OpenCode. Lo creó `omoi setup opencode`; bórralo para quitarlo.\nviejo\n").unwrap();
    let h = home.to_str().unwrap();
    for a in ["codex", "gemini", "qwen", "cursor", "kiro", "copilot", "opencode"] {
        let o = e.omoi_with(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(o.status.success(), "{a}: {}", String::from_utf8_lossy(&o.stderr));
        let o = e.omoi_with(&["setup", a, "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("nothing to do"), "{a} la segunda vez: {}", String::from_utf8_lossy(&o.stdout));
    }
    let read = |f: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(home.join(f)).unwrap()).unwrap();
    let codex = read(".codex/hooks.json");
    assert!(codex["hooks"]["SessionEnd"].to_string().contains("--session-end"), "{codex}");
    assert_eq!(codex["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    assert!(codex["hooks"]["PreCompact"].to_string().contains("--session-end"));
    let gemini = read(".gemini/settings.json");
    assert!(gemini["hooks"]["SessionEnd"].to_string().contains("--session-end") && gemini["hooks"]["PreCompress"].to_string().contains("--session-end"));
    let qwen = read(".qwen/settings.json");
    assert!(qwen["hooks"]["SessionEnd"].to_string().contains("--session-end") && qwen["hooks"]["PreCompact"].to_string().contains("--session-end"));
    let cursor = read(".cursor/hooks.json");
    assert!(cursor["hooks"]["sessionEnd"].to_string().contains("--session-end") && cursor["hooks"]["preCompact"].to_string().contains("--session-end"));
    let kiro = read(".kiro/hooks/omoikane.json");
    assert_eq!(kiro["hooks"].as_array().unwrap().len(), 3, "{kiro}");
    assert!(kiro["hooks"].to_string().contains("\"trigger\":\"Stop\""));
    let copilot = read(".copilot/hooks/omoikane.json");
    assert!(copilot["hooks"]["SessionEnd"].to_string().contains("--session-end") && copilot["hooks"]["SessionStart"].to_string().contains("--session-start"));
    let plugin = std::fs::read_to_string(home.join(".config/opencode/plugins/omoikane.ts")).unwrap();
    assert!(plugin.contains("session.compacted") && plugin.contains("--session-end") && !plugin.contains("viejo"), "el plugin viejo se rehace");
    // A plugin that is not Omoikane's is not touched.
    std::fs::write(home.join(".config/opencode/plugins/omoikane.ts"), "// mío\n").unwrap();
    let o = e.omoi_with(&["setup", "opencode", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(home.join(".config/opencode/plugins/omoikane.ts")).unwrap(), "// mío\n");
}

/// 7.5: Copilot does not give the path in `sessionEnd`: `omoi` finds it by the session id.
#[test]
fn on_close_finds_copilot_conversation_by_id() {
    let e = TestEnv::new("cierre-copilot");
    let copilot = e.dir.join("copilot");
    let session = copilot.join("session-state").join("abc123");
    std::fs::create_dir_all(&session).unwrap();
    let lines = [
        r#"{"type":"session.start","data":{"sessionId":"abc123"}}"#,
        r#"{"type":"user.message","data":{"content":"vamos a montar la web"}}"#,
        r#"{"type":"assistant.message","data":{"content":"Tengo dos opciones de plantilla. ¿Cuál prefieres?"}}"#,
        r#"{"type":"user.message","data":{"content":"la segunda, pero con la letra más grande que la veo pequeña"}}"#,
    ];
    std::fs::write(session.join("events.jsonl"), lines.join("\n")).unwrap();
    // The database must exist (otherwise there is nothing to set aside into).
    assert!(e.omoi_with(&["remember", "--kind", "fact", "--title", "Una", "algo"], None, &[]).status.success());
    let input = format!(r#"{{"sessionId":"abc123","cwd":{},"reason":"user_exit"}}"#, serde_json::to_string(&e.dir.join("repo").to_string_lossy()).unwrap());
    let o = e.omoi_with(&["recall", "--session-end"], Some(&input), &[("COPILOT_HOME", copilot.to_str().unwrap())]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_with(&["review", "--all"], None, &[]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("¿Cuál prefieres? → la segunda, pero con la letra más grande"), "{t}");
}

/// Grok does not give the path either: it is in `~/.grok/sessions/<folder>/<id>/chat_history.jsonl`.
#[test]
fn on_close_finds_grok_conversation_by_id() {
    let e = TestEnv::new("cierre-grok");
    let grok = e.dir.join("grok");
    let session = grok.join("sessions").join("C--repo").join("019a-grok");
    std::fs::create_dir_all(&session).unwrap();
    let lines = [
        r#"{"type":"user","content":[{"type":"text","text":"vamos a montar la web"}],"prompt_index":0}"#,
        r#"{"type":"assistant","content":"Tengo dos opciones de plantilla. ¿Cuál prefieres?"}"#,
        r#"{"type":"user","content":[{"type":"text","text":"la segunda, pero con la letra más grande que la veo pequeña"}],"prompt_index":1}"#,
    ];
    std::fs::write(session.join("chat_history.jsonl"), lines.join("\n")).unwrap();
    assert!(e.omoi_with(&["remember", "--kind", "fact", "--title", "Una", "algo"], None, &[]).status.success());
    let input = format!(r#"{{"hookEventName":"session_end","hook_event_name":"SessionEnd","sessionId":"019a-grok","cwd":{}}}"#, serde_json::to_string(&e.dir.join("repo").to_string_lossy()).unwrap());
    let o = e.omoi_with(&["recall", "--session-end"], Some(&input), &[("GROK_HOME", grok.to_str().unwrap())]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_with(&["review", "--all"], None, &[]);
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(t.contains("¿Cuál prefieres? → la segunda, pero con la letra más grande"), "{t}");
    // Grok has no recall at start: what was set aside reaches it with `context`, once.
    let repo = e.dir.join("repo");
    let context = |e: &TestEnv| {
        let o = e.omoi_with(&["--dir", repo.to_str().unwrap(), "tool", "context"], Some("{}"), &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    let c = context(&e);
    assert!(c["set_aside"].to_string().contains("la letra más grande"), "{c}");
    assert!(context(&e).get("set_aside").is_none(), "solo una vez");
}

/// A user's own `/omoikane-web` command is not touched, nor a `/omoikane` that Omoikane did not write.
#[test]
fn setup_respects_an_existing_command() {
    let e = TestEnv::new("setup-orden");
    let home = e.dir.join("casa");
    std::fs::create_dir_all(home.join(".qwen/commands")).unwrap();
    std::fs::write(home.join(".qwen/commands/omoikane-web.md"), "mía").unwrap();
    std::fs::write(home.join(".qwen/commands/omoikane.md"), "también mía").unwrap();
    let h = home.to_str().unwrap();
    let o = e.omoi_with(&["setup", "qwen", "--dry-run"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert!(!String::from_utf8_lossy(&o.stdout).contains("/omoikane"), "{}", String::from_utf8_lossy(&o.stdout));
    let o = e.omoi_with(&["setup", "qwen", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(home.join(".qwen/commands/omoikane-web.md")).unwrap(), "mía");
    assert_eq!(std::fs::read_to_string(home.join(".qwen/commands/omoikane.md")).unwrap(), "también mía");
}

/// The old `/omoikane`, which opened the web view, becomes the one that saves if Omoikane had written it,
/// and the web view moves to `/omoikane-web`.
#[test]
fn setup_replaces_the_old_command() {
    let e = TestEnv::new("setup-orden-vieja");
    let home = e.dir.join("casa");
    std::fs::create_dir_all(home.join(".gemini/commands")).unwrap();
    let old = home.join(".gemini/commands/omoikane.toml");
    std::fs::write(&old, "description = \"Open the Omoikane memory view in the browser\"\nprompt = \"…\"\n").unwrap();
    let h = home.to_str().unwrap();
    let o = e.omoi_with(&["setup", "gemini", "--yes"], None, &[("OMOIKANE_SETUP_HOME", h)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = std::fs::read_to_string(&old).unwrap();
    assert!(t.contains("Save to Omoikane now") && !t.contains("memory view"), "{t}");
    assert!(std::fs::read_to_string(home.join(".gemini/commands/omoikane-web.toml")).unwrap().contains("serve --open"));
}

/// `omoi recall --format`: Gemini changes the event name and Cursor uses `additional_context`;
/// Cursor's input carries `conversation_id` and `workspace_roots` instead of `session_id` and `cwd`.
#[test]
fn recall_in_other_agents_formats() {
    let e = TestEnv::new("formatos");
    e.ok(&["remember", "-k", "preference", "--global", "-t", "Commits en inglés", "Una sola línea."]);
    let root = e.dir.join("repo").to_string_lossy().replace('\\', "/");
    let cursor = format!(r#"{{"conversation_id":"c1","workspace_roots":["{root}"],"hook_event_name":"sessionStart"}}"#);
    let o = e.omoi_with(&["recall", "--session-start", "--format", "cursor"], Some(&cursor), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["additional_context"].as_str().unwrap().contains("Commits en inglés"));
    let o = e.omoi_with(&["recall", "--session-start", "--format", "gemini"], Some(r#"{"session_id":"g1","source":"startup"}"#), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    // Copilot: `additionalContext` at the top level.
    let o = e.omoi_with(&["recall", "--session-start", "--format", "copilot"], Some(r#"{"sessionId":"cp1","source":"new"}"#), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["additionalContext"].as_str().unwrap().contains("Commits en inglés"));
    // Antigravity: `conversationId` and `workspacePaths`, and the response in `injectSteps`.
    let ag = format!(r#"{{"conversationId":"ag1","workspacePaths":["{root}"]}}"#);
    let o = e.omoi_with(&["recall", "--session-start", "--format", "antigravity"], Some(&ag), &[]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["injectSteps"][0]["systemMessage"]["systemMessage"].as_str().unwrap().contains("Commits en inglés"));
    // Kiro: plain text, and the message in USER_PROMPT.
    e.ok(&["remember", "-k", "gotcha", "-t", "El importe en f32 pierde céntimos", "En src/pago.rs el importe se guarda en f32 y pierde céntimos al redondear."]);
    let o = e.omoi_with(&["recall", "--format", "plain"], Some(r#"{"session_id":"k1"}"#), &[("USER_PROMPT", "el importe en f32 de src/pago.rs pierde céntimos al redondear")]);
    let t = String::from_utf8(o.stdout).unwrap();
    assert!(t.starts_with("<omoikane>") && t.contains("céntimos"), "{t}");
}

/// What the review found: an invalid entry (empty content, very long title) does not stop the
/// import, and without a model re-importing the same file duplicates nothing.
#[test]
fn import_odd_entries_without_model() {
    let e = TestEnv::new("import2");
    let long = "t".repeat(300);
    let file = e.dir.join("engram.json");
    std::fs::write(
        &file,
        format!(
            r#"{{"sessions":[],"observations":[
                {{"id":1,"type":"decision","title":"Primera","content":"Uno.","scope":"project"}},
                {{"id":2,"type":"decision","title":"Sin contenido","content":"","scope":"project"}},
                {{"id":3,"type":"decision","title":"{long}","content":"Tres.","scope":"project"}},
                {{"id":4,"type":"decision","title":"Cuarta","content":"Cuatro.","scope":"project"}}
            ]}}"#
        ),
    )
    .unwrap();
    let f = file.to_str().unwrap();
    let s = e.ok(&["import", f]);
    assert!(s.contains("imported 4") && s.contains("0 failed"), "{s}");
    let s = e.ok(&["import", f]);
    assert!(s.contains("imported 0") && s.contains("4 skipped"), "sin modelo, la segunda vez no debe duplicar: {s}");
    let all = e.json(&["recent", "-n", "10"]);
    let long_one = all.as_array().unwrap().iter().find(|m| m["title"].as_str().unwrap().starts_with("ttt")).unwrap();
    assert!(long_one["title"].as_str().unwrap().chars().count() <= 200);
    assert!(long_one["content"].as_str().unwrap().contains(&long), "el título entero no se pierde");
}

/// `omoi serve`: health without and with a token, and a tool over POST, like the MCP ones.
#[test]
fn http_server() {
    use std::io::{Read, Write};
    let e = TestEnv::new("http");
    e.ok(&["remember", "-k", "fact", "-t", "Puerto del servidor", "Escucha en el 8010"]);
    let port = common::free_port();
    // The server is closed even if the test fails: an orphaned one keeps the binary locked on Windows.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _child = KillOnDrop(Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(["serve", "--port", &port.to_string()])
        .current_dir(e.dir.join("repo").join("src"))
        .env("OMOIKANE_DB", e.dir.join("o.db"))
        .env("OMOIKANE_MODELS", e.dir.join("models"))
        .env("OMOIKANE_HTTP_TOKEN", "secreto")
        .stderr(Stdio::null())
        .spawn()
        .unwrap());
    let request = |req: &str| -> String {
        for _ in 0..50 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                s.write_all(req.as_bytes()).unwrap();
                // As bytes: the page comes in chunks (chunked) and an accented letter can be split
                // between two, with the chunk header in the middle.
                let mut v = Vec::new();
                s.read_to_end(&mut v).unwrap();
                return String::from_utf8_lossy(&v).into_owned();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("el servidor no arrancó");
    };
    let h = format!("Host: 127.0.0.1:{port}");
    let without = request(&format!("GET /health HTTP/1.1\r\n{h}\r\nConnection: close\r\n\r\n"));
    assert!(without.starts_with("HTTP/1.1 401"), "{without}");
    let with = request(&format!("GET /health HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(with.starts_with("HTTP/1.1 200") && with.contains("\"ok\":true"), "{with}");
    let body = r#"{"query":"puerto servidor"}"#;
    let found = request(&format!(
        "POST /tools/search HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    assert!(found.starts_with("HTTP/1.1 200") && found.contains("Puerto del servidor"), "{found}");
    // The web view: the page carries no data (no token); the list does.
    let page = request(&format!("GET / HTTP/1.1\r\n{h}\r\nConnection: close\r\n\r\n"));
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("<title>Omoikane</title>"), "{}", &page[..200.min(page.len())]);
    let list = request(&format!("GET /api/memories HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(list.starts_with("HTTP/1.1 200") && list.contains("Escucha en el 8010"), "{list}");
    let sync = request(&format!("GET /api/sync HTTP/1.1\r\n{h}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(sync.starts_with("HTTP/1.1 200") && sync.contains("\"configured\":false"), "{sync}");
    // A website pointing its domain at 127.0.0.1 arrives with its Host; another website sending a POST, with its Origin.
    let rebinding = request(&format!("GET /api/memories HTTP/1.1\r\nHost: malo.example:{port}\r\nAuthorization: Bearer secreto\r\nConnection: close\r\n\r\n"));
    assert!(rebinding.starts_with("HTTP/1.1 403"), "{rebinding}");
    let foreign = request(&format!(
        "POST /tools/forget HTTP/1.1\r\n{h}\r\nOrigin: https://malo.example\r\nAuthorization: Bearer secreto\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{{\"id\":\"x\"}}"
    ));
    assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
}

/// 1.4: a session in a general folder (the user's) does not belong to a project: the diary names its
/// folder, not an empty one (project 0 exists with the empty path).
#[test]
fn diary_of_a_general_folder_names_the_folder() {
    let e = TestEnv::new("diario-general");
    let home = e.dir.join("casa");
    std::fs::create_dir_all(&home).unwrap();
    let home_s = home.to_string_lossy().into_owned();
    let env = [("USERPROFILE", home_s.as_str()), ("HOME", home_s.as_str())];
    e.ok(&["remember", "--kind", "fact", "--title", "Algo", "Para que exista la base"]);
    let input = serde_json::json!({"session_id": "general-1", "cwd": home, "hook_event_name": "SessionEnd"}).to_string();
    let o = e.omoi_with(&["recall", "--session-end"], Some(&input), &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_with(&["--dir", &home_s, "diary", "--all", "--json"], None, &env);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    let folder = v[0]["project"].as_str().unwrap().replace('\\', "/").to_lowercase();
    assert!(folder.ends_with("/casa"), "{v}");
}

/// 1.4: closing the session leaves a row in the diary, read with `omoi diary` and with the
/// `recent` tool (`sessions: true`), without one more tool.
#[test]
fn diary_records_the_session_on_close() {
    let e = TestEnv::new("diario");
    let repo = e.dir.join("repo");
    e.ok(&["remember", "--kind", "fact", "--title", "Puerto del servidor", "Escucha en el 8010"]);
    let id = e.json(&["recent"])[0]["id"].as_str().unwrap().to_string();
    let path = |f: &str| repo.join(f).to_string_lossy().into_owned();
    let lines = [
        serde_json::json!({"type": "user", "timestamp": "2026-11-03T09:00:00.000Z", "message": {"role": "user", "content": "arregla la sync"}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": path("src/sync.rs")}}]}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "t2", "name": "mcp__omoikane__update", "input": {"id": id, "append": "y en el 8011"}}]}}),
        serde_json::json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": serde_json::json!({"id": id, "kind": "fact"}).to_string()}]}}),
    ];
    let transcript = e.dir.join("sesion.jsonl");
    std::fs::write(&transcript, lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    let input = serde_json::json!({"session_id": "sesion-1", "transcript_path": transcript, "cwd": repo, "hook_event_name": "SessionEnd"}).to_string();
    let o = e.omoi_with(&["recall", "--session-end"], Some(&input), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = e.json(&["diary"]);
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v[0]["session"], "sesion-1");
    assert_eq!(v[0]["files"], serde_json::json!(["src/sync.rs"]));
    assert_eq!(v[0]["saved"][0]["title"], "Puerto del servidor", "{v}");
    assert_eq!(v[0]["first_at"], "2026-11-03T09:00:00.000Z");
    // Filters.
    assert_eq!(e.json(&["diary", "--file", "sync.rs"]).as_array().map(Vec::len), Some(1));
    assert_eq!(e.json(&["diary", "--file", "otro.rs"]).as_array().map(Vec::len), Some(0));
    // A session that started on Tuesday and ended today counts for Tuesday; for Monday, it does not.
    assert_eq!(e.json(&["diary", "--since", "2026-11-03", "--until", "2026-11-03"]).as_array().map(Vec::len), Some(1));
    assert_eq!(e.json(&["diary", "--until", "2026-11-02"]).as_array().map(Vec::len), Some(0));
    assert_eq!(e.json(&["diary", "--since", "7d"]).as_array().map(Vec::len), Some(1));
    assert!(!e.omoi(&["diary", "--since", "martes"]).status.success());
    let t = e.ok(&["diary"]);
    assert!(t.contains("2026-11-03") && t.contains("src/sync.rs") && t.contains("Puerto del servidor"), "{t}");
    // For the agent: `recent` with `sessions`, no new tool.
    let tool = |args: &str| {
        let o = e.omoi_with(&["--dir", repo.to_str().unwrap(), "tool", "recent"], Some(args), &[]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()
    };
    let r = tool(r#"{"sessions":true,"file":"sync"}"#);
    assert_eq!(r[0]["files"][0], "src/sync.rs", "{r}");
    assert_eq!(tool(r#"{"sessions":true,"file":"nada"}"#).as_array().map(Vec::len), Some(0));
    // Without `sessions`, `recent` stays as always.
    assert_eq!(tool("{}")[0]["title"], "Puerto del servidor");
    let list: serde_json::Value = serde_json::from_str(&e.ok(&["tool"])).unwrap();
    assert_eq!(list["tools"].as_array().map(Vec::len), Some(10));
    // Neither sync nor `omoi export` carries it: it belongs to this machine.
    let export = e.ok(&["export", "--all"]);
    assert!(!export.contains("sesion-1") && !export.contains("sync.rs"), "{export}");
}

/// The database, the Omoikane folder and the models, only on this machine: with a network path in `--db`,
/// `OMOIKANE_DB`, `OMOIKANE_HOME` (also after `~/`) or `OMOIKANE_MODELS`, every command opened an
/// SMB connection there (about 21 s against a machine that does not answer, and the person's NTLM hash for it).
/// Now it is a clear error before touching anything; and `import` of a network file, too.
/// Windows only: on Linux and macOS those paths are local names and open no connection.
#[cfg(windows)]
#[test]
fn neither_db_nor_its_folders_on_a_network_path() {
    let e = TestEnv::new("base-red");
    let network = [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\\?\UNC\10.255.255.1\x"];
    for r in network {
        let base = format!("{r}/o.db");
        type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);
        let cases: [Case; 4] = [
            (&["--db", base.as_str(), "recent"], &[]),
            (&["recent"], &[("OMOIKANE_DB", base.as_str())]),
            (&["recent"], &[("OMOIKANE_MODELS", r)]),
            (&["import", base.as_str()], &[]),
        ];
        for (args, env) in cases {
            let t = std::time::Instant::now();
            let o = e.omoi_with(args, None, env);
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(!o.status.success() && err.contains("network paths"), "{args:?} {env:?}: {err}");
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "{args:?} {env:?}: {:?}", t.elapsed());
        }
    }
    // `OMOIKANE_HOME` without `OMOIKANE_DB`.
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

/// A local path with the extended-path prefix (`\\?\C:\…`, the one `canonicalize` gives) belongs to this
/// machine: `--db`, `OMOIKANE_DB` and `OMOIKANE_HOME` accept it (in 1.8.5 it was rejected as if it were
/// a network path). `\\?\UNC\…` is still a network path.
#[cfg(windows)]
#[test]
fn extended_drive_path_is_local() {
    let e = TestEnv::new("base-extendida");
    let folder = std::fs::canonicalize(&e.dir).unwrap();
    let folder = folder.to_string_lossy();
    assert!(folder.starts_with(r"\\?\"), "{folder}");
    let base = format!(r"{folder}\extendida.db");
    let o = e.omoi_with(&["--db", base.as_str(), "remember", "-k", "fact", "-t", "Ruta extendida", "La base vive en una ruta extendida."], None, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.omoi_with(&["recent"], None, &[("OMOIKANE_DB", base.as_str())]);
    assert!(o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Ruta extendida"), "{}", String::from_utf8_lossy(&o.stderr));
    // `OMOIKANE_HOME` without `OMOIKANE_DB`.
    let home = format!(r"{folder}\casa-extendida");
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(["recent"]).current_dir(e.dir.join("repo")).env_remove("OMOIKANE_DB").env("OMOIKANE_HOME", &home).env("OMOIKANE_MODELS", e.dir.join("models")).env("OMOIKANE_AUTO_UPDATE", "0").env_remove("OMOIKANE_LANG");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.dir.join("casa-extendida").join("omoikane.db").exists());
    // With `..` or a network path, no.
    for bad in [format!(r"{folder}\..\..\x.db"), r"\\?\UNC\10.255.255.1\x\o.db".to_string()] {
        let o = e.omoi_with(&["--db", bad.as_str(), "recent"], None, &[]);
        assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("network paths"), "{bad}");
    }
}

/// The folder the person works in can be on a network share (a shared drive,
/// `\\server\share`, WSL): its repository is read there as in 1.8.0. With the network path fixes
/// of 1.8.5 a new project from there got `dir:persona/netrepo` instead of its `git:`, with no
/// commit in the provenance and no `last_seen`, and the hook with that `cwd` failed. What comes from outside
/// (`project` over MCP) is still not opened. It is simulated with `\\localhost\C$\…` (the administrative share
/// of the temp folder's drive); if this machine does not allow opening it, the test says so and checks
/// nothing (the logic is covered by `project::tests::the_work_share_is_trusted`).
#[cfg(windows)]
#[test]
fn working_folder_can_be_on_the_network() {
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    let e = TestEnv::new("trabajo-red");
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
    let omoi = |cwd: &str, args: &[&str], input: Option<&str>| -> Output {
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
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        if let Some(t) = input {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(t.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    };
    let src = format!(r"{unc}\src");
    let o = omoi(&src, &["remember", "-k", "fact", "-t", "Desde la red", "--force", "--json", "Guardada desde una carpeta compartida."], None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    // Its key is the repository's (`origin`), and its folder has been seen here.
    let o = omoi(&src, &["projects", "--json"], None);
    let p: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let net = p.as_array().unwrap().iter().find(|x| x["path"].as_str().is_some_and(|s| s.ends_with("netrepo"))).unwrap_or_else(|| panic!("{p}"));
    assert_eq!(net["key"], "git:github.com/acme/netrepo", "{p}");
    let db = rusqlite::Connection::open(e.dir.join("o.db")).unwrap();
    let seen: Option<String> = db.query_row("SELECT last_seen FROM project_path WHERE path LIKE '%netrepo'", [], |f| f.get(0)).unwrap();
    assert!(seen.is_some(), "last_seen");
    drop(db);
    // The provenance carries its HEAD's commit.
    let o = omoi(&src, &["get", &id, "--json"], None);
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(m["provenance"]["commit"], SHA, "{m}");
    // The agent's hook with a network `cwd` (its session's folder), launched from another folder.
    let outside = e.dir.to_string_lossy().into_owned();
    let input = serde_json::json!({ "session_id": "s-red", "cwd": src, "source": "startup" }).to_string();
    for args in [&["recall", "--session-start", "--format", "plain"][..], &["recall", "--format", "plain"], &["recall", "--session-end"]] {
        let o = omoi(&outside, args, Some(&input));
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }
    // What comes from outside, not: a network `project` over MCP, even from the network folder.
    let t = std::time::Instant::now();
    let o = omoi(&src, &["tool", "remember"], Some(&serde_json::json!({ "kind": "fact", "title": "x", "content": "y", "project": r"\\10.255.255.1\x" }).to_string()));
    let output = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(output.contains("network paths"), "{output}");
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
}

/// 1.10.1: a new tag that is a variant of one that already exists here (`ideas` with `idea`) is saved
/// as the existing one; system tags are not touched; `omoi tags` lists the pairs already there and
/// `--merge` merges one into the other with history.
#[test]
fn tags_without_variants() {
    let e = TestEnv::new("etiquetas");
    e.ok(&["remember", "-k", "fact", "-t", "Primera", "--tag", "idea", "--tag", "opens", "Una idea."]);
    let o = e.omoi(&["remember", "-k", "fact", "-t", "Segunda", "--tag", "Ideas", "--tag", "open", "Otra idea distinta del todo."]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("«Ideas» → «idea»"), "{}", String::from_utf8_lossy(&o.stderr));
    let r = e.json(&["search", "otra idea distinta"]);
    let tags: Vec<&str> = r[0]["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert!(tags.contains(&"idea") && tags.contains(&"open") && !tags.contains(&"ideas"), "{tags:?}");

    // A pair that was already in the database (from before 1.10.1, or imported): lists it and merges it.
    let db = rusqlite::Connection::open(e.dir.join("o.db")).unwrap();
    db.execute("UPDATE memory SET tags = '[\"tests\"]' WHERE title = 'Primera'", []).unwrap();
    db.execute("UPDATE memory SET tags = '[\"test\",\"open\"]' WHERE title = 'Segunda'", []).unwrap();
    drop(db);
    let p = e.json(&["tags"]);
    assert_eq!(p.as_array().unwrap().len(), 1, "{p}");
    assert!(e.ok(&["tags"]).contains("omoi tags --merge"));
    let m = e.json(&["tags", "--merge", "tests", "test"]);
    assert_eq!(m["changed"], 1, "{m}");
    assert_eq!(e.json(&["tags"]).as_array().unwrap().len(), 0);
    assert!(!e.omoi(&["tags", "--merge", "opens", "open"]).status.success(), "las del sistema no se unen");
}

/// `--evidence KIND:DETAIL` (1.13) when saving and when changing, and `get` says what it is based on.
#[test]
fn evidence_from_cli() {
    let e = TestEnv::new("evidencia");
    let m = e.json(&["remember", "-k", "fact", "-t", "Los tests tardan", "--evidence", "tested:cargo test, 255 s", "--evidence", "doc:https://doc.rust-lang.org/cargo dice que paraleliza", "Los tests de integración tardan unos 4 minutos."]);
    assert_eq!(m["evidence"][0]["kind"], "tested", "{m}");
    assert_eq!(m["evidence"][1]["url"], "https://doc.rust-lang.org/cargo", "{m}");
    let id = m["id"].as_str().unwrap();
    e.ok(&["update", id, "--evidence", "user-said:«van lentos»"]);
    let g = e.ok(&["get", id]);
    assert!(g.contains("based on   tested: cargo test, 255 s") && g.contains("user-said: «van lentos»"), "{g}");
    assert!(!e.omoi(&["remember", "-t", "x", "--evidence", "guess:algo", "texto"]).status.success());
}
