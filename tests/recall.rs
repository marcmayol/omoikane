//! Fase 4: el recall que inyectan los hooks.

use omoikane::db;
use omoikane::memoria::{self, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use rusqlite::Connection;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-recall-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (db::abrir(&dir.join("o.db")).unwrap(), dir)
}

/// Las memorias de tests/busquedas.tsv en el proyecto `/p`, más una preferencia y una fijada.
fn con_memorias(db: &mut Connection) {
    for l in include_str!("busquedas.tsv").lines().filter(|l| l.starts_with("m\t")) {
        let c: Vec<&str> = l.split('\t').collect();
        let mut n = Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: c[2].into(), content: c[3].into(), ..Default::default() };
        if c[1] == "auth" {
            n.kind = Some(Tipo::Gotcha);
            n.paths = vec!["src/middleware/auth.rs".into()];
        }
        memoria::guardar(db, n, None).unwrap();
    }
    let pref = Nueva { project: String::new(), kind: Some(Tipo::Preference), title: "Respuestas en español".into(), content: "El usuario quiere las respuestas en español y sin rodeos.".into(), ..Default::default() };
    memoria::guardar(db, pref, None).unwrap();
    let fijada = Nueva { project: "/p".into(), kind: Some(Tipo::Decision), title: "Base de datos única".into(), content: "Todo va en un solo fichero SQLite.".into(), pinned: true, ..Default::default() };
    memoria::guardar(db, fijada, None).unwrap();
}

fn entrada(sesion: &str, prompt: &str) -> Entrada {
    Entrada { session_id: sesion.into(), prompt: prompt.into(), ..Default::default() }
}

#[test]
fn al_empezar_da_preferencias_y_fijadas() {
    let (mut db, dir) = base("empezar");
    con_memorias(&mut db);
    let t = recall::al_empezar(&mut db, "/p", &entrada("s1", "")).unwrap().unwrap();
    assert!(t.starts_with("<omoikane>") && t.ends_with("</omoikane>"));
    assert!(t.contains("20 memories"), "{t}");
    assert!(t.contains("Respuestas en español") && t.contains("[global]"));
    assert!(t.contains("Base de datos única"));
    // Lo último en lo que se trabajó: 5 títulos con fecha, sin su contenido, y sin repetir lo fijado.
    let (_, ultimas) = t.split_once("Last worked on").expect("lista de lo último");
    let lineas: Vec<&str> = ultimas.lines().skip(1).take_while(|l| l.starts_with("- ")).collect();
    assert_eq!(lineas.len(), recall::RECIENTES, "{t}");
    assert!(lineas.iter().all(|l| l.starts_with("- 20") && l.contains("(id ") && !l.contains("Base de datos única")), "{t}");
    assert!(!t.contains("/var/log/app:"), "sin el contenido: {t}");
    assert!(t.contains("ask the user right away"), "{t}");
    // Solo las preferencias y las fijadas cuentan como inyectadas: lo reciente aún puede llegar entero.
    assert_eq!(recall::inyectadas(&db, "s1").unwrap(), 2);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cada_mensaje_trae_lo_que_toca_o_nada() {
    let (mut db, dir) = base("mensaje");
    con_memorias(&mut db);
    // Relevante: comparte palabras poco frecuentes con una memoria.
    let t = recall::al_preguntar(&mut db, "/p", &entrada("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap();
    let t = t.expect("debería inyectar la del token");
    assert!(t.contains("Bug en auth"), "{t}");
    // Charla y mensajes cortos: nada.
    for p in ["vale, sigue", "como ha ido todo?", "1 ok 2 ok 3 ok", "perfecto gracias, dale"] {
        assert!(recall::al_preguntar(&mut db, "/p", &entrada("s1", p), None).unwrap().is_none(), "«{p}» no debería inyectar nada");
    }
    // Sin relación con ninguna memoria: nada.
    assert!(recall::al_preguntar(&mut db, "/p", &entrada("s1", "receta de paella valenciana con conejo y garrofó"), None).unwrap().is_none());
    // La misma memoria no se repite en la misma sesión…
    assert!(recall::al_preguntar(&mut db, "/p", &entrada("s1", "otra vez el middleware de autenticación y la caducidad del token UTC"), None).unwrap().is_none());
    // …pero sí en otra sesión, y en la misma después de compactar.
    assert!(recall::al_preguntar(&mut db, "/p", &entrada("s2", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_some());
    recall::al_empezar(&mut db, "/p", &entrada("s1", "")).unwrap();
    assert!(recall::al_preguntar(&mut db, "/p", &entrada("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_some());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Con muy pocas memorias, las palabras del mensaje que no están en ninguna no deben hundir la
/// cobertura de la que sí es del tema (caso real de la prueba con Claude Code).
#[test]
fn base_pequena_y_palabras_que_no_estan() {
    let (mut db, dir) = base("pequena");
    for (k, t, c, p) in [
        (Tipo::Preference, "Respuestas cortas y en español", "Como mucho tres frases.", ""),
        (Tipo::Gotcha, "El cobro con tarjeta redondea mal los céntimos", "En src/pago.rs el importe va en f32 y pierde céntimos.", "/p"),
        (Tipo::Decision, "Pasarela de pago: Redsys", "Se eligió Redsys y no Stripe porque el banco lo exige y la comisión es menor.", "/p"),
    ] {
        memoria::guardar(&mut db, Nueva { project: p.into(), kind: Some(k), title: t.into(), content: c.into(), ..Default::default() }, None).unwrap();
    }
    let t = recall::al_preguntar(&mut db, "/p", &entrada("s", "Estoy pensando en cambiar la pasarela de pago a Stripe"), None).unwrap();
    assert!(t.expect("debería traer la decisión").contains("Redsys"));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// El recordatorio de guardar: solo en sesiones largas sin nada guardado, y no en cada mensaje.
#[test]
fn recordatorio_de_guardar() {
    let (mut db, dir) = base("recordatorio");
    con_memorias(&mut db);
    recall::al_empezar(&mut db, "/p", &Entrada { session_id: "s".into(), source: "startup".into(), ..Default::default() }).unwrap();
    let charla = |db: &mut Connection| recall::al_preguntar(db, "/p", &entrada("s", "vale, sigue"), None).unwrap();
    // Recién empezada: nada, aunque haya muchos mensajes.
    for _ in 0..12 {
        assert!(charla(&mut db).is_none());
    }
    // Una sesión de hace 20 minutos sin nada guardado: toca, una sola vez.
    // (las memorias de ejemplo se crearon «ahora»: se retrasan para que sean de antes de la sesión)
    db.execute("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours')", []).unwrap();
    db.execute("UPDATE session SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-20 minutes')", []).unwrap();
    let t = charla(&mut db).expect("debería recordar");
    assert!(t.contains("nothing has been saved"), "{t}");
    assert!(charla(&mut db).is_none(), "solo una vez cada 30 minutos");
    // Con algo guardado durante la sesión, ya no.
    db.execute("UPDATE session SET reminded_at = NULL", []).unwrap();
    memoria::guardar(&mut db, Nueva { project: "/p".into(), kind: Some(Tipo::Fact), title: "Nueva".into(), content: "algo aprendido".into(), ..Default::default() }, None).unwrap();
    assert!(charla(&mut db).is_none());
    // Compactar no reinicia el reloj; una sesión nueva, sí.
    let antes: String = db.query_row("SELECT started_at FROM session WHERE session = 's'", [], |f| f.get(0)).unwrap();
    recall::al_empezar(&mut db, "/p", &Entrada { session_id: "s".into(), source: "compact".into(), ..Default::default() }).unwrap();
    let despues: String = db.query_row("SELECT started_at FROM session WHERE session = 's'", [], |f| f.get(0)).unwrap();
    assert_eq!(antes, despues);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nombrar_un_fichero_trae_sus_memorias() {
    let (mut db, dir) = base("rutas");
    con_memorias(&mut db);
    let t = recall::al_preguntar(&mut db, "/p", &entrada("s1", "revisa src/middleware/auth.rs por favor"), None).unwrap().unwrap();
    assert!(t.contains("Bug en auth") && t.contains("src/middleware/auth.rs"), "{t}");
    // Con el nombre del fichero basta, y en Windows con barras al revés también.
    let t = recall::al_preguntar(&mut db, "/p", &entrada("s2", r"mira src\middleware\auth.rs"), None).unwrap().unwrap();
    assert!(t.contains("Bug en auth"));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn otro_proyecto_no_ve_las_memorias() {
    let (mut db, dir) = base("proyecto");
    con_memorias(&mut db);
    assert!(recall::al_preguntar(&mut db, "/otro", &entrada("s1", "el middleware de autenticación compara mal la caducidad del token en UTC"), None).unwrap().is_none());
    let t = recall::al_empezar(&mut db, "/otro", &entrada("s1", "")).unwrap().unwrap();
    assert!(t.contains("Respuestas en español") && !t.contains("Base de datos única"), "{t}");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// El hook de verdad: `omoi recall` con el JSON que manda Claude Code por stdin.
fn hook(dir: &std::path::Path, args: &[&str], json: &str) -> (bool, String, String) {
    let mut h = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .arg("recall")
        .args(args)
        .env("OMOIKANE_DB", dir.join("o.db"))
        .env_remove("OMOIKANE_LANG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    h.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
    let o = h.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn el_hook_de_claude_code() {
    let dir = std::env::temp_dir().join(format!("omoi-recall-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let cwd = repo.to_string_lossy().replace('\\', "\\\\");
    // Sin base todavía: no molesta.
    let (ok, out, _) = hook(&dir, &[], &format!(r#"{{"session_id":"s","cwd":"{cwd}","prompt":"el token caduca","hook_event_name":"UserPromptSubmit"}}"#));
    assert!(ok && out.is_empty());
    // Con una memoria del proyecto (el que sale de `cwd`, no de la carpeta del proceso).
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(["--dir", repo.to_str().unwrap(), "remember", "-k", "gotcha", "-t", "Bug en auth: el token caduca antes de tiempo"])
        .arg("El middleware de autenticación compara la hora local con la caducidad del token, que va en UTC.")
        .env("OMOIKANE_DB", dir.join("o.db"))
        .output()
        .unwrap();
    assert!(o.status.success());
    let (ok, out, _) = hook(&dir, &["--session-start"], &format!(r#"{{"session_id":"s","cwd":"{cwd}","source":"startup","hook_event_name":"SessionStart"}}"#));
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("1 memories"));
    let (ok, out, _) = hook(
        &dir,
        &[],
        &format!(r#"{{"session_id":"s","cwd":"{cwd}","prompt":"el middleware de autenticación y la caducidad del token en UTC","hook_event_name":"UserPromptSubmit"}}"#),
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("Bug en auth"));
    // Entrada rota: ni error ni salida (un hook nunca debe bloquear al usuario).
    let (ok, out, _) = hook(&dir, &[], "esto no es json");
    assert!(ok && out.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
