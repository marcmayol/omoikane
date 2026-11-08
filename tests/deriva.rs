//! 1.8.5: deriva por código. Una memoria cuyos ficheros han cambiado mucho desde que se escribió (o se
//! dio por buena) sale en la revisión como `code_drift`; los retoques, no.

mod comun;

use omoikane::db;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::revisar;
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;

/// Las líneas de un fichero de `n` líneas, todas con `marca`.
fn texto(n: usize, marca: &str) -> String {
    (0..n).map(|i| format!("linea {i} {marca}\n")).collect()
}

/// `git` en `dir`, sin depender de la configuración de quien corre los tests.
fn git(dir: &Path, args: &[&str], fecha: Option<&str>) {
    let mut c = Command::new("git");
    c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", "-C"]).arg(dir).args(args);
    c.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    if let Some(f) = fecha {
        c.env("GIT_AUTHOR_DATE", f).env("GIT_COMMITTER_DATE", f);
    }
    let o = c.output().expect("git");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Escribe `fichero` en el repositorio y lo confirma con fecha `hace` (en el formato de `strftime` de
/// SQLite, p. ej. `-8 days`) o ahora.
fn confirmar(db: &Connection, repo: &Path, fichero: &str, contenido: &str, hace: Option<&str>) {
    let ruta = repo.join(fichero);
    std::fs::create_dir_all(ruta.parent().unwrap()).unwrap();
    std::fs::write(&ruta, contenido).unwrap();
    let fecha = hace.map(|h| format!("{} +0000", segundos(db, h)));
    git(repo, &["add", "-A"], None);
    git(repo, &["commit", "-q", "-m", fichero], fecha.as_deref());
}

fn segundos(db: &Connection, hace: &str) -> i64 {
    db.query_row("SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER)", [hace], |f| f.get(0)).unwrap()
}

fn fecha(db: &Connection, hace: &str) -> String {
    db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)", [hace], |f| f.get(0)).unwrap()
}

/// Una memoria del proyecto con esas rutas, escrita hace `hace`.
fn memoria(db: &mut Connection, project: &str, titulo: &str, contenido: &str, rutas: &[&str], hace: &str) -> String {
    let n = Nueva {
        project: project.into(),
        kind: Some(Tipo::Gotcha),
        title: titulo.into(),
        content: contenido.into(),
        paths: rutas.iter().map(|r| r.to_string()).collect(),
        ..Default::default()
    };
    let m = memoria::guardar(db, n, None).unwrap();
    let f = fecha(db, hace);
    db.execute("UPDATE memory SET created_at = ?1, updated_at = ?1 WHERE id = ?2", rusqlite::params![f, m.id]).unwrap();
    m.id
}

fn derivas(db: &Connection, project: &str) -> Vec<(String, String)> {
    let ambito = Ambito::de(db, project).unwrap();
    revisar::avisos(db, &ambito).unwrap().into_iter().filter(|a| a.kind == revisar::DERIVA_CODIGO).map(|a| (a.memory_id, a.detail)).collect()
}

/// Un repositorio de git con `src/a.rs` y `src/b.rs` (40 líneas cada uno) de hace 20 días.
fn repo(nombre: &str) -> (comun::Temporal, Connection, String) {
    let d = comun::Temporal::nueva(&format!("deriva-{nombre}"));
    let raiz = d.join("repo");
    std::fs::create_dir_all(&raiz).unwrap();
    git(&raiz, &["init", "-q"], None);
    let db = db::abrir(&d.join("o.db")).unwrap();
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "uno"), Some("-20 days"));
    confirmar(&db, &raiz, "src/b.rs", &texto(40, "uno"), Some("-20 days"));
    let project = omoikane::proyecto::de(&raiz).unwrap();
    omoikane::proyecto::registrar(&db, &project).unwrap();
    (d, db, project)
}

#[test]
fn avisa_si_sus_ficheros_cambian_mucho_y_no_por_retoques() {
    let (_d, mut db, project) = repo("mucho");
    let raiz = Path::new(&project).to_path_buf();
    let mucho = memoria(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    let poco = memoria(&mut db, &project, "Cómo va b.rs", "Lo de b.rs.", &["src/b.rs"], "-10 days");
    // a.rs, reescrito entero; b.rs, dos líneas de cuarenta.
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "dos"), Some("-2 days"));
    let mut b = texto(40, "uno");
    b.push_str("otra\nmas\n");
    confirmar(&db, &raiz, "src/b.rs", &b, Some("-2 days"));
    let v = derivas(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0].0, mucho);
    assert!(v[0].1.starts_with("`src/a.rs` changed a lot since ") && v[0].1.contains("1 commit, 80 lines changed, 200% of their size") && v[0].1.ends_with("is it still true?"), "{}", v[0].1);
    assert!(!v.iter().any(|(id, _)| *id == poco));
    // El aviso al empezar cuenta lo ya calculado.
    let cifras = revisar::pendientes(&db, &Ambito::de(&db, &project).unwrap()).unwrap();
    assert_eq!(cifras.get(revisar::DERIVA_CODIGO), Some(&1), "{cifras:?}");
}

#[test]
fn los_ficheros_que_nombra_tambien_cuentan() {
    let (_d, mut db, project) = repo("nombra");
    let raiz = Path::new(&project).to_path_buf();
    let id = memoria(&mut db, &project, "El parser", "La función de `src/b.rs` que parte las líneas.", &[], "-10 days");
    confirmar(&db, &raiz, "src/b.rs", &texto(40, "dos"), Some("-2 days"));
    let v = derivas(&db, &project);
    assert_eq!(v.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![id.as_str()], "{v:?}");
}

#[test]
fn lo_de_la_misma_sesion_no_cuenta() {
    // Guardar la memoria y confirmar después el trabajo del que habla es lo normal: no es deriva.
    let (_d, mut db, project) = repo("gracia");
    let raiz = Path::new(&project).to_path_buf();
    memoria(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-3 hours");
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "dos"), Some("-1 hours"));
    assert!(derivas(&db, &project).is_empty());
}

#[test]
fn esta_bien_asi_hasta_que_vuelva_a_cambiar() {
    let (_d, mut db, project) = repo("ok");
    let raiz = Path::new(&project).to_path_buf();
    let id = memoria(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "dos"), Some("-8 days"));
    assert_eq!(derivas(&db, &project).len(), 1);
    revisar::descartar(&db, &id, revisar::DERIVA_CODIGO, "").unwrap();
    assert!(derivas(&db, &project).is_empty());
    // Dada por buena hace 5 días: lo de hace 8 ya no cuenta…
    let f = fecha(&db, "-5 days");
    db.execute("UPDATE verdict SET at = ?1 WHERE memory_id = ?2", rusqlite::params![f, id]).unwrap();
    assert!(derivas(&db, &project).is_empty());
    // …y un cambio grande después vuelve a avisar, cuando se recalcula (una vez al día, aparte: lo de
    // hoy ya está guardado).
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "tres"), Some("-1 days"));
    assert!(derivas(&db, &project).is_empty());
    assert_eq!(omoikane::deriva::refrescar(&db).unwrap(), 1);
    let v = derivas(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].1.contains("since ") && v[0].1.contains("1 commit,"), "{}", v[0].1);
    // La memoria no se ha tocado.
    assert_eq!(memoria::obtener(&db, &id).unwrap().content, "Lo de a.rs.");
}

#[test]
fn fuera_de_git_cuenta_el_diario() {
    let d = comun::Temporal::nueva("deriva-diario");
    let raiz = d.join("carpeta");
    std::fs::create_dir_all(&raiz).unwrap();
    std::fs::write(raiz.join("notas.md"), "hola\n").unwrap();
    let mut db = db::abrir(&d.join("o.db")).unwrap();
    let project = omoikane::proyecto::de(&raiz).unwrap();
    omoikane::proyecto::registrar(&db, &project).unwrap();
    let pid = omoikane::proyecto::resolver(&db, &project).unwrap().unwrap().0;
    let id = memoria(&mut db, &project, "Las notas", "Lo de las notas.", &["notas.md"], "-10 days");
    omoikane::diario::crear_tabla(&db).unwrap();
    let sesion = |db: &Connection, n: usize, saved: &str| {
        let f = fecha(db, &format!("-{} days", n + 1));
        db.execute(
            "INSERT INTO diary(session, project_id, folder, first_at, last_at, files, saved) VALUES (?1, ?2, ?3, ?4, ?4, '[\"notas.md\"]', ?5)",
            rusqlite::params![format!("s{n}"), pid, project, f, saved],
        )
        .unwrap();
    };
    // Cuatro sesiones después, y la que la guardó (no cuenta): aún no.
    for n in 0..4 {
        sesion(&db, n, "[]");
    }
    sesion(&db, 4, &format!("[\"{id}\"]"));
    assert!(derivas(&db, &project).is_empty());
    // La quinta, sí. (Lo de hoy se guarda: para que lo vea, la memoria «cambia» de fecha.)
    sesion(&db, 5, "[]");
    let f = fecha(&db, "-9 days");
    db.execute("UPDATE memory SET updated_at = ?1 WHERE id = ?2", rusqlite::params![f, id]).unwrap();
    let v = derivas(&db, &project);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].1.contains("`notas.md`") && v[0].1.contains("edited in 5 sessions"), "{}", v[0].1);
}

/// `omoi` con la base del test y nada de fuera.
fn omoi(d: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_omoi"))
        .args(args)
        .env("OMOIKANE_DB", d.join("o.db"))
        .env("OMOIKANE_MODELS", d.join("models"))
        .env("OMOIKANE_AUTO_UPDATE", "0")
        .env("OMOIKANE_HOME", d.join("home-omoikane"))
        .env("OMOIKANE_LANG", "en")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "omoi {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn la_cli_recalcula_y_da_por_buena() {
    let (d, mut db, project) = repo("cli");
    let raiz = Path::new(&project).to_path_buf();
    let id = memoria(&mut db, &project, "Cómo va a.rs", "Lo de a.rs.", &["src/a.rs"], "-10 days");
    confirmar(&db, &raiz, "src/a.rs", &texto(40, "dos"), Some("-2 days"));
    // Lo que hace el proceso de fondo: lo deja calculado para hoy.
    omoi(&d, &["review", "--refresh-drift"]);
    let guardado: String = db.query_row("SELECT v FROM meta WHERE k = 'deriva_codigo'", [], |f| f.get(0)).unwrap();
    assert!(guardado.contains(&id) && guardado.contains("changed a lot"), "{guardado}");
    let cifras = revisar::pendientes(&db, &Ambito::de(&db, &project).unwrap()).unwrap();
    assert_eq!(cifras.get(revisar::DERIVA_CODIGO), Some(&1), "{cifras:?}");
    let salida = omoi(&d, &["review", "--dismiss", &id, "--kind", "code_drift"]);
    assert!(salida.contains("until its files change a lot again"), "{salida}");
    assert!(derivas(&db, &project).is_empty());
}
