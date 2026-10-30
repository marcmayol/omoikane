//! 0.17.0: veredictos. Lo que el agente dice de dos memorias (sustituye, se contradicen, valen las
//! dos) y la fecha de revisión de cada tipo, como `mem_judge`, `mem_compare` y `mem_review` de
//! engram, pero sin herramientas nuevas y sin pedir juicio en cada guardado.

use omoikane::db;
use omoikane::memoria::{self, Ambito, Cambios, Nueva, Tipo};
use omoikane::revisar;
use omoikane::veredictos::{self, Veredicto};
use rusqlite::Connection;
use std::path::PathBuf;

fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-veredictos-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (db::abrir(&dir.join("o.db")).unwrap(), dir)
}

fn guardar(db: &mut Connection, kind: Tipo, title: &str, content: &str, tags: &[&str]) -> String {
    let n = Nueva {
        project: "/p".into(),
        kind: Some(kind),
        title: title.into(),
        content: content.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    memoria::guardar(db, n, None).unwrap().id
}

fn avisos(db: &Connection) -> Vec<(String, String, String)> {
    revisar::avisos(db, &Ambito::Todos).unwrap().into_iter().map(|a| (a.kind, a.memory_id, a.other_id)).collect()
}

fn notas(db: &Connection, id: &str, busqueda: bool) -> Option<String> {
    let m = memoria::obtener(db, id).unwrap();
    let mut n = if busqueda { revisar::notas_busqueda(db, &[&m]) } else { revisar::notas(db, &[&m]) }.unwrap();
    n.remove(id)
}

#[test]
fn sustituida_sale_en_revisar_en_el_recall_y_en_la_busqueda() {
    let (mut db, dir) = base("sustituye");
    let vieja = guardar(&mut db, Tipo::Decision, "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[]);
    let nueva = guardar(&mut db, Tipo::Decision, "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.", &[]);
    assert!(avisos(&db).is_empty());
    veredictos::poner(&db, &vieja, &nueva, veredictos::SUSTITUIDA).unwrap();
    assert_eq!(avisos(&db), vec![(revisar::SUSTITUIDA.to_string(), vieja.clone(), nueva.clone())]);
    let n = notas(&db, &vieja, false).unwrap();
    assert!(n.starts_with("superseded by") && n.contains(&nueva[..12]), "{n}");
    // La nueva no lleva nota: es la buena.
    assert_eq!(notas(&db, &nueva, true), None);
    // Si la vieja se corrige después, el veredicto deja de valer.
    let c = Cambios { content: Some("La cookie sigue para la web; la API va con JWT.".into()), reason: Some("aclarado".into()), ..Default::default() };
    memoria::actualizar(&mut db, &vieja, c, None).unwrap();
    assert!(avisos(&db).is_empty());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn se_contradicen_hasta_que_cambia_una_y_compatible_quita_el_aviso() {
    let (mut db, dir) = base("contradicen");
    let a = guardar(&mut db, Tipo::Fact, "El backup se hace de noche", "La copia corre a las 3:00.", &[]);
    let b = guardar(&mut db, Tipo::Fact, "El backup se hace al mediodía", "La copia corre a las 12:00.", &[]);
    veredictos::poner(&db, &a, &b, veredictos::CONTRADICE).unwrap();
    assert_eq!(avisos(&db), vec![(revisar::CONTRADICE.to_string(), a.clone(), b.clone())]);
    // Las dos llevan la nota, cada una nombrando a la otra.
    assert!(notas(&db, &a, false).unwrap().contains(&b[..12]));
    assert!(notas(&db, &b, false).unwrap().contains(&a[..12]));
    // El veredicto más nuevo de la pareja manda, en cualquier orden.
    veredictos::poner(&db, &b, &a, veredictos::COMPATIBLE).unwrap();
    assert!(avisos(&db).is_empty());
    assert_eq!(notas(&db, &a, true), None);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn esta_bien_asi_en_una_desfasada_es_compatible() {
    let (mut db, dir) = base("desfasada");
    let vieja = guardar(&mut db, Tipo::Fact, "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let nueva = guardar(&mut db, Tipo::Fact, "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(avisos(&db), vec![(revisar::DESFASADA.to_string(), vieja.clone(), nueva.clone())]);
    revisar::descartar(&db, &vieja, revisar::DESFASADA, &nueva).unwrap();
    assert!(avisos(&db).is_empty());
    assert_eq!(notas(&db, &vieja, false), None);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fecha_de_revision_por_tipo() {
    let (mut db, dir) = base("revision");
    let decision = guardar(&mut db, Tipo::Decision, "Los precios van en céntimos", "Todos los importes en enteros de céntimos.", &[]);
    let hecho = guardar(&mut db, Tipo::Fact, "El servidor de pruebas es lento", "Tarda unos 10 s en arrancar.", &[]);
    let pref = guardar(&mut db, Tipo::Preference, "Commits cortos y en inglés", "Una línea, en inglés.", &[]);
    // Hace 150 días: a la decisión ya le toca (120), al hecho no (180), a la preferencia nunca.
    db.execute("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-150 days'), created_at = updated_at", []).unwrap();
    let due: Vec<String> = avisos(&db).into_iter().filter(|a| a.0 == revisar::POR_REVISAR).map(|a| a.1).collect();
    assert_eq!(due, vec![decision.clone()]);
    // Sale en la búsqueda pero no en el recall, que solo avisa de lo que puede estar mal.
    assert!(notas(&db, &decision, true).unwrap().starts_with("not reviewed in 150 days"));
    assert_eq!(notas(&db, &decision, false), None);
    assert_eq!(notas(&db, &hecho, true), None);
    assert_eq!(notas(&db, &pref, true), None);
    // «Está bien así» pone el reloj a cero.
    revisar::descartar(&db, &decision, revisar::POR_REVISAR, "").unwrap();
    assert!(!avisos(&db).iter().any(|a| a.0 == revisar::POR_REVISAR));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn de_otro_equipo_gana_el_mas_nuevo() {
    let (mut db, dir) = base("recibir");
    let a = guardar(&mut db, Tipo::Fact, "Uno", "El puerto es el 8080.", &[]);
    let b = guardar(&mut db, Tipo::Fact, "Dos", "El puerto es el 9090.", &[]);
    veredictos::poner(&db, &a, &b, veredictos::CONTRADICE).unwrap();
    let mio = veredictos::todos(&db).unwrap().remove(0);
    // Uno más viejo del mismo veredicto no entra; uno más nuevo de la pareja manda.
    let viejo = Veredicto { hlc: "0000000000001-0000000000-otro".into(), ..mio.clone() };
    assert!(!veredictos::recibir(&db, &viejo).unwrap());
    let futuro = format!("{}-0000000000-otro", mio.hlc[..13].parse::<u64>().unwrap().saturating_add(1000));
    let nuevo = Veredicto { memory_id: b.clone(), other_id: a.clone(), relation: veredictos::COMPATIBLE.into(), hlc: futuro, ..mio };
    assert!(veredictos::recibir(&db, &nuevo).unwrap());
    assert!(avisos(&db).is_empty());
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn los_descartes_de_antes_pasan_a_veredictos() {
    let (mut db, dir) = base("migrar");
    let a = guardar(&mut db, Tipo::Fact, "Uno", "Algo que nadie usa.", &[]);
    // Una base del esquema 7 con un «está bien así» en `review`.
    db.execute_batch("DROP TABLE verdict; PRAGMA user_version = 7;").unwrap();
    db.execute(
        "INSERT INTO review(memory_id, kind, other_id, detail, found_at, dismissed_at) VALUES (?1, 'unused', '', '', '2026-10-29T00:00:00.000Z', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 minute'))",
        [&a],
    )
    .unwrap();
    drop(db);
    let db = db::abrir(&dir.join("o.db")).unwrap();
    let v = veredictos::todos(&db).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].memory_id.as_str(), v[0].relation.as_str()), (a.as_str(), "ok:unused"));
    let quedan: i64 = db.query_row("SELECT count(*) FROM review WHERE dismissed_at IS NOT NULL", [], |f| f.get(0)).unwrap();
    assert_eq!(quedan, 0);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
