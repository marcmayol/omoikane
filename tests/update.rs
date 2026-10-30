//! 0.19.0: corregir con cabeza. Edición parcial (`replace` exactamente una vez, `append`) y juntar
//! N memorias en una (`absorb`), por la herramienta `update` y por la CLI.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Entorno {
    dir: PathBuf,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = std::env::temp_dir().join(format!("omoi-update-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        Entorno { dir }
    }

    fn omoi(&self, args: &[&str], entrada: Option<&str>) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.dir.join("o.db"))
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

    fn ok(&self, args: &[&str]) -> String {
        let o = self.omoi(args, None);
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    fn id(&self, kind: &str, titulo: &str, texto: &str, extra: &[&str]) -> String {
        let mut a = vec!["remember", "-k", kind, "-t", titulo];
        a.extend_from_slice(extra);
        a.push(texto);
        self.json(&a)["id"].as_str().unwrap().to_string()
    }

    fn get(&self, id: &str) -> Option<serde_json::Value> {
        let o = self.omoi(&["--json", "get", id], None);
        o.status.success().then(|| serde_json::from_slice(&o.stdout).unwrap())
    }

    /// `omoi tool update` con los argumentos en JSON: (salió bien, lo que respondió).
    fn update(&self, args: serde_json::Value) -> (bool, String) {
        let o = self.omoi(&["tool", "update"], Some(&args.to_string()));
        (o.status.success(), String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    fn update_ok(&self, args: serde_json::Value) -> serde_json::Value {
        let (bien, texto) = self.update(args);
        assert!(bien, "{texto}");
        serde_json::from_str(&texto).unwrap()
    }

    fn historial(&self, id: &str) -> Vec<serde_json::Value> {
        self.json(&["history", id]).as_array().unwrap().clone()
    }

    fn rotos(&self) -> Vec<String> {
        self.json(&["review", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == "broken_link")
            .map(|a| a["memory_id"].as_str().unwrap().to_string())
            .collect()
    }
}

impl Drop for Entorno {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const RECETA: &str = "1. git pull\n2. cargo build\n3. cargo test\n4. deploy";

#[test]
fn replace_cambia_el_trozo_que_encaja_una_vez_y_ensena_el_contexto() {
    let e = Entorno::nuevo("replace");
    let id = e.id("procedure", "Publicar", RECETA, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id, "replace": [{ "find": "2. cargo build", "with": "2. cargo build --release" }], "reason": "va en release" }));
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull\n2. cargo build --release\n3. cargo test\n4. deploy");
    let cambiado = &r["changed"][0];
    assert_eq!(cambiado["find"], "2. cargo build");
    let contexto = cambiado["context"].as_str().unwrap();
    assert!(contexto.contains("1. git pull\n2. cargo build --release\n3. cargo test"), "{contexto}");
    // Una sola versión anterior, con el motivo.
    let h = e.historial(&id);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0]["content"], RECETA);
    assert_eq!(h[0]["reason"], "va en release");
    // El contexto se corta a 80 caracteres por lado, con «…».
    let largo = format!("{}\nMARCA\n{}", "a".repeat(200), "b".repeat(200));
    let id2 = e.id("fact", "Larga", &largo, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id2, "replace": [{ "find": "MARCA", "with": "SEÑAL" }] }));
    let contexto = r["changed"][0]["context"].as_str().unwrap();
    assert_eq!(contexto, format!("…{}\nSEÑAL\n{}…", "a".repeat(79), "b".repeat(79)));
}

#[test]
fn replace_que_no_encaja_o_encaja_dos_veces_no_cambia_nada() {
    let e = Entorno::nuevo("replace-mal");
    let id = e.id("procedure", "Publicar", RECETA, &[]);
    let (bien, texto) = e.update(serde_json::json!({ "id": id, "replace": [{ "find": "npm install", "with": "pnpm install" }] }));
    assert!(!bien);
    assert!(texto.contains("appears 0 times"), "{texto}");
    // Dos veces: error con el recuento, y nada cambia aunque hubiese otros campos.
    let (bien, texto) = e.update(serde_json::json!({ "id": id, "title": "Otro título", "replace": [{ "find": "cargo", "with": "CARGO" }] }));
    assert!(!bien);
    assert!(texto.contains("appears 2 times"), "{texto}");
    let m = e.get(&id).unwrap();
    assert_eq!(m["content"], RECETA);
    assert_eq!(m["title"], "Publicar");
    assert!(e.historial(&id).is_empty(), "sin versión fantasma");
    // Dos reemplazos: si el segundo falla, el primero tampoco entra.
    let (bien, _) = e.update(serde_json::json!({ "id": id, "replace": [{ "find": "4. deploy", "with": "4. desplegar" }, { "find": "cargo", "with": "x" }] }));
    assert!(!bien);
    assert_eq!(e.get(&id).unwrap()["content"], RECETA);
}

#[test]
fn append_anade_al_final_con_una_linea_en_blanco() {
    let e = Entorno::nuevo("append");
    let id = e.id("procedure", "Publicar", RECETA, &[]);
    let r = e.update_ok(serde_json::json!({ "id": id, "append": "5. anunciarlo", "reason": "un paso más" }));
    assert_eq!(e.get(&id).unwrap()["content"], format!("{RECETA}\n\n5. anunciarlo"));
    assert_eq!(r["changed"][0]["appended"], true);
    assert!(r["changed"][0]["context"].as_str().unwrap().ends_with("4. deploy\n\n5. anunciarlo"));
    // Con replace a la vez: una sola versión anterior.
    e.update_ok(serde_json::json!({ "id": id, "replace": [{ "find": "1. git pull", "with": "1. git pull --rebase" }], "append": "6. descansar" }));
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull --rebase\n2. cargo build\n3. cargo test\n4. deploy\n\n5. anunciarlo\n\n6. descansar");
    assert_eq!(e.historial(&id).len(), 2);
}

#[test]
fn la_cli_tambien_reemplaza_y_anade() {
    let e = Entorno::nuevo("cli");
    let id = e.id("procedure", "Publicar", RECETA, &[]);
    let salida = e.ok(&["update", &id, "--replace", "3. cargo test", "--with", "3. cargo test --all", "--append", "5. tag", "-r", "cli"]);
    assert!(salida.contains("changed:"), "{salida}");
    assert_eq!(e.get(&id).unwrap()["content"], "1. git pull\n2. cargo build\n3. cargo test --all\n4. deploy\n\n5. tag");
    // Un --replace sin su --with es un error, y el contenido no se toca.
    let o = e.omoi(&["update", &id, "--replace", "4. deploy"], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--with"));
    let o = e.omoi(&["update", &id, "--replace", "nada", "--with", "algo"], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("appears 0 times"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn absorb_junta_etiquetas_rutas_historial_y_enlaces_y_deja_la_vieja_en_la_papelera() {
    let e = Entorno::nuevo("absorb");
    let vieja = e.id("fact", "El token caduca en UTC", "El middleware compara la hora local con la caducidad UTC.", &["--tag", "auth-token", "--path", "src/auth.rs"]);
    e.ok(&["update", &vieja, "-c", "El middleware compara la hora local con la caducidad UTC del token.", "-r", "más claro"]);
    let nueva = e.id("fact", "Autenticación", "Las sesiones van por cookie firmada.", &["--tag", "auth", "--path", "src/session.rs"]);
    e.ok(&["update", &nueva, "-c", "Las sesiones van por cookie firmada; la cookie dura 7 días.", "-r", "duración"]);
    e.id("gotcha", "Antes de tocar la auth", "Mira primero [[auth-token]].", &[]);
    assert!(e.rotos().is_empty(), "el enlace resuelve a la vieja");
    // Sin el texto fundido no hay fusión: Omoikane no redacta.
    let (bien, texto) = e.update(serde_json::json!({ "id": nueva, "absorb": [&vieja[..10]] }));
    assert!(!bien);
    assert!(texto.contains("merged text"), "{texto}");
    assert!(e.get(&vieja).is_some());
    let fundido = "Las sesiones van por cookie firmada; la cookie dura 7 días.\n\nEl middleware compara la hora local con la caducidad UTC del token.";
    let r = e.update_ok(serde_json::json!({ "id": nueva, "content": fundido, "absorb": [&vieja[..10]], "reason": "juntadas" }));
    assert_eq!(r["absorbed"], serde_json::json!([vieja]));
    let m = e.get(&nueva).unwrap();
    assert_eq!(m["content"], fundido);
    assert_eq!(m["tags"], serde_json::json!(["auth", "auth-token"]));
    assert_eq!(m["paths"], serde_json::json!(["src/auth.rs", "src/session.rs"]));
    // Historial: la versión propia (motivo «juntadas») y las dos de la vieja, numeradas a continuación.
    let h = e.historial(&nueva);
    let resumen: Vec<(i64, String, String)> = h.iter().map(|v| (v["version"].as_i64().unwrap(), v["content"].as_str().unwrap().to_string(), v["reason"].as_str().unwrap_or("").to_string())).collect();
    assert_eq!(resumen, [
        (4, "El middleware compara la hora local con la caducidad UTC del token.".to_string(), format!("absorbed from {vieja}")),
        (3, "El middleware compara la hora local con la caducidad UTC.".to_string(), format!("absorbed from {vieja}")),
        (2, "Las sesiones van por cookie firmada; la cookie dura 7 días.".to_string(), "juntadas".to_string()),
        (1, "Las sesiones van por cookie firmada.".to_string(), "duración".to_string()),
    ]);
    // La vieja está en la papelera, y el enlace de la tercera sigue resolviendo (ahora a la nueva).
    assert!(e.get(&vieja).is_none());
    let papelera = e.json(&["trash"]);
    assert_eq!(papelera[0]["id"], vieja.as_str());
    assert!(e.rotos().is_empty(), "[[auth-token]] lleva a la que queda");
    // Control: un enlace a una etiqueta que de verdad desaparece sí sale roto.
    let suelta = e.id("fact", "Suelta", "Nada.", &["--tag", "suelta"]);
    let enlaza = e.id("fact", "Enlaza a la suelta", "Ver [[suelta]].", &[]);
    e.ok(&["forget", &suelta, "--yes"]);
    assert_eq!(e.rotos(), std::slice::from_ref(&enlaza));
    // El veredicto: si alguien restaura la vieja, aparece como sustituida por la que queda.
    e.ok(&["restore", &vieja]);
    let o = e.omoi(&["tool", "search"], Some("{\"query\":\"caducidad UTC\",\"all_projects\":true}"));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let nota = v.as_array().unwrap().iter().find(|x| x["id"] == vieja.as_str()).expect("vuelve")["note"].as_str().unwrap_or("").to_string();
    assert!(nota.starts_with("superseded by"), "{nota}");
}
