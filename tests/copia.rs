//! 0.18.0: nada se pierde. La papelera (`forget` → `trash` → `restore`) y la copia fiel
//! (`export` → `import` reconstruye la base exacta y se puede repetir sin cambiar nada).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Entorno {
    dir: PathBuf,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = std::env::temp_dir().join(format!("omoi-copia-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        Entorno { dir }
    }

    /// `omoi` contra la base `base` (un nombre dentro de la carpeta de pruebas), con el mismo nombre
    /// de equipo siempre (para que dos bases exporten líneas iguales).
    fn omoi(&self, base: &str, args: &[&str], entrada: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.dir.join(base))
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env("OMOIKANE_EQUIPO", "pc-pruebas")
            .env_remove("OMOIKANE_LANG")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut hijo = c.spawn().unwrap();
        if let Some(e) = entrada {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        hijo.wait_with_output().unwrap()
    }

    fn ok(&self, base: &str, args: &[&str]) -> String {
        let o = self.omoi(base, args, None);
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, base: &str, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(base, &a)).unwrap()
    }

    fn id(&self, base: &str, kind: &str, titulo: &str, texto: &str) -> String {
        self.json(base, &["remember", "-k", kind, "-t", titulo, texto])["id"].as_str().unwrap().to_string()
    }

    fn get(&self, base: &str, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi(base, &["--json", "get", id], None);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    fn fichero(&self, nombre: &str) -> PathBuf {
        self.dir.join(nombre)
    }
}

impl Drop for Entorno {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Las líneas de un export sin la cabecera (que lleva la hora) y sin el nombre del equipo que
/// exporta (`machine`, y el SHA del cierre, que lo incluye): lo que tiene que ser idéntico entre una
/// base y su copia.
fn cuerpo(ruta: &Path) -> Vec<String> {
    std::fs::read_to_string(ruta)
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
fn la_papelera_devuelve_lo_olvidado_con_su_historial() {
    let e = Entorno::nuevo("papelera");
    let id = e.id("o.db", "decision", "Los precios van en céntimos", "Todos los importes en enteros.");
    e.ok("o.db", &["update", &id, "-c", "Todos los importes en enteros de céntimos.", "-r", "más claro"]);
    let salida = e.ok("o.db", &["forget", &id, "--yes"]);
    assert!(salida.contains("omoi restore"), "{salida}");
    assert!(e.get("o.db", &id).is_none());
    let papelera = e.json("o.db", &["trash"]);
    assert_eq!(papelera[0]["id"], id.as_str());
    assert!(papelera[0]["by"].as_str().unwrap().starts_with("pc-pruebas"), "{papelera}");
    // `restore` sin id enseña lo mismo.
    assert!(e.ok("o.db", &["restore"]).contains("Los precios van en céntimos"));
    e.ok("o.db", &["restore", &id[..12]]);
    let m = e.get("o.db", &id).expect("vuelve");
    assert_eq!(m["content"], "Todos los importes en enteros de céntimos.");
    assert_eq!(e.json("o.db", &["history", &id]).as_array().unwrap().len(), 1, "con su historial");
    assert!(e.json("o.db", &["trash"]).as_array().unwrap().is_empty());
    // Por MCP, sin herramienta nueva: `update` con restore.
    e.ok("o.db", &["forget", &id, "--yes"]);
    let o = e.omoi("o.db", &["tool", "update"], Some(&format!("{{\"id\":\"{id}\",\"restore\":true}}")));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(e.get("o.db", &id).is_some());
    // `--purge` no pasa por la papelera, y vaciarla borra del todo.
    let otra = e.id("o.db", "fact", "Una que sobra", "Nada.");
    e.ok("o.db", &["forget", &otra, "--yes", "--purge"]);
    assert!(e.json("o.db", &["trash"]).as_array().unwrap().is_empty());
    e.ok("o.db", &["forget", &id, "--yes"]);
    assert_eq!(e.json("o.db", &["trash", "--empty", "--yes"])["emptied"], 1);
    assert!(!e.omoi("o.db", &["restore", &id], None).status.success());
}

#[test]
fn export_e_import_reconstruyen_la_base_exacta_y_se_pueden_repetir() {
    let e = Entorno::nuevo("fiel");
    let a = e.id("o.db", "decision", "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.");
    let b = e.id("o.db", "fact", "El servidor de pruebas es lento", "Tarda unos 10 s en arrancar.");
    let c = e.id("o.db", "decision", "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.");
    e.ok("o.db", &["update", &a, "-c", "La autenticación usaba una cookie de sesión; ver JWT.", "-r", "aclarado"]);
    e.ok("o.db", &["forget", &b, "--yes"]);
    let o = e.omoi("o.db", &["tool", "update"], Some(&format!("{{\"id\":\"{c}\",\"supersedes\":[\"{a}\"],\"reason\":\"JWT replaced the cookie\"}}")));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let f1 = e.fichero("copia1.jsonl");
    let salida = e.omoi("o.db", &["export", "--all"], None);
    assert!(salida.status.success());
    std::fs::write(&f1, &salida.stdout).unwrap();
    let texto = String::from_utf8(salida.stdout).unwrap();
    let lineas: Vec<serde_json::Value> = texto.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lineas[0]["type"], "header");
    assert_eq!(lineas[0]["format"], "omoi-export");
    let tipos: Vec<&str> = lineas.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(tipos, ["header", "tombstone", "memory", "memory", "verdict", "forgotten", "end"], "{tipos:?}");
    assert_eq!(lineas[6]["count"], 5);
    assert_eq!(lineas[2]["versions"].as_array().unwrap().len(), 1, "el historial viaja");
    assert_eq!((lineas[4]["reason"].as_str(), lineas[4]["author"].as_str()), (Some("JWT replaced the cookie"), Some("agent")), "el veredicto con su motivo y su autor: {}", lineas[4]);
    // Solo contar: el fichero se comprueba entero.
    let d = e.json("o2.db", &["import", f1.to_str().unwrap(), "--dry-run"]);
    assert_eq!((d["file"]["memories"].as_u64(), d["file"]["tombstones"].as_u64(), d["file"]["verdicts"].as_u64(), d["file"]["forgotten"].as_u64()), (Some(2), Some(1), Some(1), Some(1)));
    assert!(e.get("o2.db", &a).is_none(), "--dry-run no importa nada");
    // Importar en una base vacía: la misma base.
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
    let nota = v.as_array().unwrap().iter().find(|x| x["id"] == a.as_str()).unwrap()["note"].as_str().unwrap_or("").to_string();
    assert!(nota.starts_with("superseded by") && nota.ends_with(": JWT replaced the cookie"), "el veredicto también, con su motivo: {nota}");
    let rev = e.json("o2.db", &["review", "--all"]);
    let av = rev.as_array().unwrap().iter().find(|x| x["kind"] == "superseded").expect("sale en review");
    assert_eq!((av["reason"].as_str(), av["author"].as_str()), (Some("JWT replaced the cookie"), Some("agent")), "{av}");
    // Exportar la copia da el mismo fichero (menos la cabecera, que lleva la hora).
    let f2 = e.fichero("copia2.jsonl");
    let salida = e.omoi("o2.db", &["export", "--all"], None);
    std::fs::write(&f2, &salida.stdout).unwrap();
    assert_eq!(cuerpo(&f1), cuerpo(&f2));
    // Repetir el import no cambia nada.
    let r = e.json("o2.db", &["import", f1.to_str().unwrap()]);
    assert_eq!((r["new"].as_u64(), r["updated"].as_u64(), r["forgotten"].as_u64()), (Some(0), Some(0), Some(0)), "{r}");
    // Un fichero tocado no se importa.
    let roto = texto.replacen("cookie de sesión", "cookie de sesion", 1);
    std::fs::write(e.fichero("roto.jsonl"), roto).unwrap();
    let o = e.omoi("o3.db", &["import", e.fichero("roto.jsonl").to_str().unwrap()], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("damaged"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.get("o3.db", &a).is_none());
    // Y uno cortado tampoco.
    let cortado: String = texto.lines().take(4).collect::<Vec<_>>().join("\n");
    std::fs::write(e.fichero("cortado.jsonl"), cortado).unwrap();
    let o = e.omoi("o3.db", &["import", e.fichero("cortado.jsonl").to_str().unwrap()], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("incomplete"));
}

#[test]
fn la_copia_cifrada_solo_se_lee_con_la_clave() {
    let e = Entorno::nuevo("cifrada");
    // Una clave de sincronización: un repositorio git desnudo local basta.
    std::fs::write(e.dir.join("gitconfig-vacio"), "").unwrap();
    let o = Command::new("git").current_dir(&e.dir).args(["init", "--quiet", "--bare", "compartida.git"]).env("GIT_CONFIG_GLOBAL", e.dir.join("gitconfig-vacio")).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let url = e.dir.join("compartida.git").to_string_lossy().replace('\\', "/");
    let o = e.omoi("o.db", &["sync", "init", &url, "--yes"], None);
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let id = e.id("o.db", "gotcha", "El token caduca antes de tiempo", "Compara la hora en UTC.");
    let salida = e.omoi("o.db", &["export", "--all", "--encrypt"], None);
    assert!(salida.status.success(), "{}", String::from_utf8_lossy(&salida.stderr));
    let texto = String::from_utf8(salida.stdout).unwrap();
    let lineas: Vec<&str> = texto.lines().collect();
    assert_eq!(lineas.len(), 3);
    assert!(lineas[0].contains("\"encrypted\":true"));
    assert!(!lineas[1].contains("token") && serde_json::from_str::<serde_json::Value>(lineas[1]).is_err(), "la memoria va cifrada");
    let f = e.fichero("cifrada.jsonl");
    std::fs::write(&f, &texto).unwrap();
    // Con la clave se lee entero; sin ella, no.
    let d = e.json("o.db", &["import", f.to_str().unwrap(), "--dry-run"]);
    assert_eq!(d["file"]["memories"], 1);
    let o = e.omoi("otra.db", &["import", f.to_str().unwrap(), "--dry-run"], None);
    assert!(!o.status.success());
    assert!(e.get("o.db", &id).is_some());
}

#[test]
fn la_exportacion_antigua_y_la_de_engram_siguen_entrando() {
    let e = Entorno::nuevo("antigua");
    let vieja = r#"{"id":"01HXXXXXXXXXXXXXXXXXXXXXXX","project":"","kind":"fact","title":"Una de antes","content":"Del formato 1.","paths":[],"tags":[],"pinned":false,"source":"cli","created_at":"2026-10-01T10:00:00.000Z","updated_at":"2026-10-01T10:00:00.000Z","recalled_at":null,"recall_count":0,"versions":[]}"#;
    std::fs::write(e.fichero("v1.jsonl"), format!("{vieja}\n")).unwrap();
    let salida = e.ok("o.db", &["import", e.fichero("v1.jsonl").to_str().unwrap()]);
    assert!(salida.contains("imported 1"), "{salida}");
    let o = e.omoi("o.db", &["tool", "search"], Some("{\"query\":\"formato 1\",\"all_projects\":true}"));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Una de antes"));
}

#[test]
fn backup_copia_la_base_y_conserva_las_ultimas() {
    let e = Entorno::nuevo("backup");
    e.id("o.db", "fact", "Algo", "Para que haya base.");
    let salida = e.ok("o.db", &["backup"]);
    assert!(salida.contains("backups"), "{salida}");
    let lista = e.json("o.db", &["backup", "--list"]);
    assert_eq!(lista.as_array().unwrap().len(), 1);
    let ruta = PathBuf::from(lista[0]["path"].as_str().unwrap());
    assert!(ruta.file_name().unwrap().to_string_lossy().starts_with("omoikane-"));
    // La copia es una base que se abre y tiene la memoria.
    let o = Command::new(env!("CARGO_BIN_EXE_omoi")).args(["--db", ruta.to_str().unwrap(), "--json", "recent", "--all"]).env("OMOIKANE_AUTO_UPDATE", "0").env("OMOIKANE_HOME", e.dir.join("home-omoikane")).stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("Algo"));
    // Repetir el mismo día sustituye la copia de hoy, no añade otra.
    e.ok("o.db", &["backup"]);
    assert_eq!(e.json("o.db", &["backup", "--list"]).as_array().unwrap().len(), 1);
    let doctor = e.ok("o.db", &["doctor"]);
    assert!(doctor.contains("backups    omoikane-"), "{doctor}");
}

/// 0.19.1: una memoria de un proyecto que llegó de otro equipo se exporta con la clave con la que
/// llegó (la de `remote_project`, la misma que usa la sincronización), no con una calculada aquí.
#[test]
fn el_export_usa_la_clave_de_los_proyectos_que_llegaron_de_fuera() {
    let e = Entorno::nuevo("clave-remota");
    let id = e.id("o.db", "fact", "La caché se invalida al publicar", "Con el hook de publicación.");
    let ruta = e.get("o.db", &id).unwrap()["project"].as_str().unwrap().to_string();
    assert!(!ruta.is_empty());
    {
        let db = rusqlite::Connection::open(e.fichero("o.db")).unwrap();
        db.execute("INSERT INTO remote_project(path, key) VALUES (?1, 'git:github.com/marcmayol/omoikane')", [&ruta]).unwrap();
    }
    let salida = e.omoi("o.db", &["export", "--all"], None);
    assert!(salida.status.success(), "{}", String::from_utf8_lossy(&salida.stderr));
    let memoria = String::from_utf8(salida.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|l| l["id"] == id.as_str())
        .unwrap();
    assert_eq!(memoria["project_key"], "git:github.com/marcmayol/omoikane", "{memoria}");
    assert_eq!(memoria["project_path"], ruta.as_str());
}

/// 0.19.1: una base de antes que tiene en la papelera una memoria que ya está viva (otro equipo la
/// restauró): `omoi restore` no falla con `UNIQUE`, avisa y la quita de la papelera.
#[test]
fn restaurar_una_memoria_viva_la_quita_de_la_papelera() {
    let e = Entorno::nuevo("viva-en-papelera");
    let id = e.id("o.db", "fact", "Los informes salen los viernes", "Antes de las 14:00.");
    e.ok("o.db", &["forget", &id, "--yes"]);
    let fila: (String, String, String, String, String) = {
        let db = rusqlite::Connection::open(e.fichero("o.db")).unwrap();
        db.query_row("SELECT memory, versions, hlc, deleted_at, by FROM forgotten WHERE id = ?1", [&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?))).unwrap()
    };
    e.ok("o.db", &["restore", &id]);
    {
        let db = rusqlite::Connection::open(e.fichero("o.db")).unwrap();
        db.execute("INSERT INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", rusqlite::params![id, fila.0, fila.1, fila.2, fila.3, fila.4]).unwrap();
    }
    let o = e.omoi("o.db", &["restore", &id], None);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && !err.contains("UNIQUE") && (err.contains("trash") || err.contains("papelera")), "{err}");
    assert!(e.get("o.db", &id).is_some(), "sigue viva");
    assert!(!e.ok("o.db", &["trash"]).contains("informes"), "y ya no está en la papelera");
}
