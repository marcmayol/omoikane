//! La CLI de punta a punta, como la usará una persona o un hook: el binario `omoi` de verdad, con
//! su propia base y dentro de un repositorio de prueba.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Entorno {
    dir: PathBuf,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = std::env::temp_dir().join(format!("omoi-cli-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("repo").join("src")).unwrap();
        Entorno { dir }
    }

    fn omoi(&self, args: &[&str]) -> Output {
        self.omoi_con(args, None, &[])
    }

    fn omoi_con(&self, args: &[&str], entrada: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo").join("src"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env_remove("OMOIKANE_LANG")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut hijo = c.spawn().unwrap();
        if let Some(e) = entrada {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
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

impl Drop for Entorno {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn existe(p: &Path) -> bool {
    p.exists()
}

/// El criterio de «hecho» de la fase 1 (PLAN.md §11).
#[test]
fn remember_y_search_encuentran_bug_en_auth() {
    let e = Entorno::nuevo("hecho");
    assert!(!existe(&e.dir.join("o.db")));
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
    // El proyecto es la raíz del repo aunque se ejecute desde src/.
    assert!(r[0]["project"].as_str().unwrap().ends_with("repo"));
    assert!(e.ok(&["search", "fallo", "de", "autenticacion"]).contains("Bug en auth"));
}

#[test]
fn contenido_por_la_entrada_estandar() {
    let e = Entorno::nuevo("stdin");
    let o = e.omoi_con(&["remember", "-k", "decision", "-t", "Por stdin", "--json"], Some("Llega por la tubería.\n"), &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let m: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(m["content"], "Llega por la tubería.");
    // Sin contenido ni entrada: error claro, nada guardado.
    let o = e.omoi(&["remember", "-k", "fact", "-t", "Vacía"]);
    assert!(!o.status.success());
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
}

#[test]
fn update_history_y_forget() {
    let e = Entorno::nuevo("ciclo");
    let m = e.json(&["remember", "-k", "decision", "-t", "Usamos UUID", "Por compatibilidad."]);
    let id = m["id"].as_str().unwrap();
    let corto = &id[..12];
    e.ok(&["update", corto, "-t", "Usamos ULID", "-c", "Se ordenan por fecha.", "-r", "los UUID no se ordenan"]);
    let h = e.json(&["history", corto]);
    assert_eq!(h[0]["title"], "Usamos UUID");
    assert_eq!(h[0]["reason"], "los UUID no se ordenan");
    assert!(e.ok(&["get", corto]).contains("Se ordenan por fecha."));
    e.ok(&["update", corto, "--pin"]);
    assert_eq!(e.json(&["get", corto])["pinned"], true);
    // Sin --yes y fuera de una terminal, se niega y no borra nada.
    let o = e.omoi(&["forget", corto]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    assert!(e.omoi(&["get", corto]).status.success());
    assert!(e.ok(&["forget", corto, "--yes"]).contains("Usamos ULID"));
    assert!(!e.omoi(&["get", corto]).status.success());
}

#[test]
fn global_se_ve_desde_cualquier_proyecto() {
    let e = Entorno::nuevo("global");
    e.ok(&["remember", "-k", "preference", "-t", "Commits en inglés", "--global", "Una línea, en inglés."]);
    let otro = e.dir.join("otro");
    std::fs::create_dir_all(&otro).unwrap();
    let r = e.json(&["--dir", otro.to_str().unwrap(), "search", "commits"]);
    assert_eq!(r[0]["project"], "");
    let p = e.json(&["projects"]);
    assert_eq!(p.as_array().unwrap().len(), 1);
}

#[test]
fn export_jsonl() {
    let e = Entorno::nuevo("export");
    e.ok(&["remember", "-k", "fact", "-t", "Una", "uno"]);
    e.ok(&["remember", "-k", "fact", "-t", "Dos", "dos"]);
    let salida = e.ok(&["export"]);
    let lineas: Vec<serde_json::Value> = salida.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lineas.len(), 2);
    assert!(lineas.iter().all(|l| l["versions"].as_array().unwrap().is_empty()));
}

#[test]
fn ayuda_en_ingles_y_en_espanol() {
    let e = Entorno::nuevo("idioma");
    let en = String::from_utf8(e.omoi(&["remember", "--help"]).stdout).unwrap();
    assert!(en.contains("Save a memory"), "{en}");
    let o = e.omoi_con(&["remember", "--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Guarda una memoria"), "{es}");
    assert!(es.contains("Uso:") && es.contains("Opciones:") && !es.contains("Options:"), "{es}");
    let o = e.omoi_con(&["--help"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Comandos:") && !es.contains("Commands:") && es.contains("Muestra la versión"), "{es}");
    // Las opciones globales salen en los subcomandos, también en español.
    let o = e.omoi_con(&["search", "-h"], None, &[("OMOIKANE_LANG", "es")]);
    let es = String::from_utf8(o.stdout).unwrap();
    assert!(es.contains("Base de datos") && es.contains("Argumentos:"), "{es}");
    // Los errores también.
    let o = e.omoi_con(&["get", "ZZZZZZZZ"], None, &[("OMOIKANE_LANG", "es")]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no existe"));
    let o = e.omoi(&["get", "ZZZZZZZZ"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no memory"));
}

#[test]
fn tipo_desconocido_falla_sin_guardar() {
    let e = Entorno::nuevo("tipo");
    let o = e.omoi(&["remember", "-k", "bugfix", "-t", "X", "y"]);
    assert!(!o.status.success());
    assert!(!e.dir.join("o.db").exists() || e.json(&["recent"]).as_array().unwrap().is_empty());
}

fn modelo_int8() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

/// Fase 2 de punta a punta: sin modelo funciona por palabras; con él, avisa de duplicados y
/// encuentra por significado.
#[test]
fn modelo_duplicados_y_significado() {
    let Some(int8) = modelo_int8() else {
        eprintln!("SALTADO: no está el modelo int8");
        return;
    };
    let e = Entorno::nuevo("modelo");
    assert!(e.ok(&["model"]).contains("no model installed"));
    e.ok(&["remember", "-k", "gotcha", "-t", "Bug en auth: el token caduca antes de tiempo", "El middleware compara la hora local con la caducidad del token, que va en UTC."]);
    // Sin modelo, la consulta sin palabras en común no encuentra nada.
    assert!(e.json(&["search", "problema de zona horaria con la autenticación"]).as_array().unwrap().is_empty());

    let instalado = e.ok(&["model", "install", "--from", int8.to_str().unwrap()]);
    assert!(instalado.contains("model ready") && instalado.contains("1 vectors"), "{instalado}");
    assert!(e.ok(&["model"]).contains("* potion-multilingual-128M-int8-"));
    let doctor = e.ok(&["doctor"]);
    assert!(doctor.contains("vectors    1"), "{doctor}");

    // Ahora sí, por significado.
    let r = e.json(&["search", "problema de zona horaria con la autenticación"]);
    assert_eq!(r[0]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert!(r[0]["similarity"].as_f64().unwrap() > 0.28);

    // Casi lo mismo: no se guarda, código 2, y dice cuál es la parecida.
    let parecida = ["remember", "-k", "gotcha", "-t", "El token de auth caduca demasiado pronto", "El middleware compara la hora local con la caducidad del token en UTC."];
    let o = e.omoi(&parecida);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stdout).contains("not saved"));
    let mut con_json = parecida.to_vec();
    con_json.push("--json");
    let o = e.omoi(&con_json);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["duplicate_of"]["title"], "Bug en auth: el token caduca antes de tiempo");
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 1);
    // Con --force, sí.
    let mut forzada = parecida.to_vec();
    forzada.push("--force");
    e.ok(&forzada);
    assert_eq!(e.json(&["recent"]).as_array().unwrap().len(), 2);

    assert!(e.ok(&["reindex"]).contains("2 memories, 2 vectors"));
    assert!(e.ok(&["model", "clean"]).contains("0 models deleted"));
}
