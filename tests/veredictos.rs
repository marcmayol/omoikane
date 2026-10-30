//! 0.17.0: veredictos. Lo que el agente dice de dos memorias (sustituye, se contradicen, valen las
//! dos) y la fecha de revisión de cada tipo, como `mem_judge`, `mem_compare` y `mem_review` de
//! engram, pero sin herramientas nuevas y sin pedir juicio en cada guardado.
//! 0.19.0: cada veredicto con su motivo y su autor, y las sustituciones encadenadas (A → B → C).

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
    veredictos::poner(&db, &vieja, &nueva, veredictos::SUSTITUIDA, "", "").unwrap();
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
    veredictos::poner(&db, &a, &b, veredictos::CONTRADICE, "", "").unwrap();
    assert_eq!(avisos(&db), vec![(revisar::CONTRADICE.to_string(), a.clone(), b.clone())]);
    // Las dos llevan la nota, cada una nombrando a la otra.
    assert!(notas(&db, &a, false).unwrap().contains(&b[..12]));
    assert!(notas(&db, &b, false).unwrap().contains(&a[..12]));
    // El veredicto más nuevo de la pareja manda, en cualquier orden.
    veredictos::poner(&db, &b, &a, veredictos::COMPATIBLE, "", "").unwrap();
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
    // Si el agente la recibió hace poco y nadie la corrigió, está repasada: no toca.
    db.execute("UPDATE memory SET recalled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 days'), recall_count = 3 WHERE id = ?1", [&decision]).unwrap();
    assert!(!avisos(&db).iter().any(|a| a.0 == revisar::POR_REVISAR));
    db.execute("UPDATE memory SET recalled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-140 days') WHERE id = ?1", [&decision]).unwrap();
    assert!(avisos(&db).iter().any(|a| a.0 == revisar::POR_REVISAR), "recordada hace 140 días: sí toca");
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
    veredictos::poner(&db, &a, &b, veredictos::CONTRADICE, "", "").unwrap();
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

/// Una memoria escrita a mano en una base de un esquema viejo (con `project` como ruta).
fn memoria_vieja(db: &Connection, n: i64, id: &str, title: &str, content: &str, cuando: &str) {
    db.execute(
        "INSERT INTO memory(n, id, project, kind, title, content, created_at, updated_at) VALUES (?1, ?2, '/p', 'fact', ?3, ?4, ?5, ?5)",
        rusqlite::params![n, id, title, content, cuando],
    )
    .unwrap();
    db.execute("INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, '', '')", rusqlite::params![n, title, content]).unwrap();
}

#[test]
fn los_descartes_de_antes_pasan_a_veredictos() {
    let dir = std::env::temp_dir().join(format!("omoi-veredictos-migrar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = "01JAAAAAAAAAAAAAAAAAAAAAAA";
    {
        // Una base del esquema 7 con un «está bien así» en `review`.
        let db = db::abrir_en_esquema(&dir.join("o.db"), 7).unwrap();
        memoria_vieja(&db, 1, a, "Uno", "Algo que nadie usa.", "2026-10-28T00:00:00.000Z");
        db.execute(
            "INSERT INTO review(memory_id, kind, other_id, detail, found_at, dismissed_at) VALUES (?1, 'unused', '', '', '2026-10-29T00:00:00.000Z', '2026-10-29T00:01:00.000Z')",
            [a],
        )
        .unwrap();
    }
    let db = db::abrir(&dir.join("o.db")).unwrap();
    let v = veredictos::todos(&db).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].memory_id.as_str(), v[0].relation.as_str()), (a, "ok:unused"));
    // El esquema 10 deja `review` como caché: sin `dismissed_at`.
    assert!(db.query_row("SELECT count(*) FROM review WHERE dismissed_at IS NOT NULL", [], |f| f.get::<_, i64>(0)).is_err());
    // Y el «está bien así» sigue valiendo (la memoria no ha cambiado desde entonces).
    assert!(veredictos::oks(&db, true).unwrap().contains_key(&(a.to_string(), "unused".to_string())));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Los falsos positivos de la base real de Marc (30-oct-2026): dentro de un proyecto, dos memorias
/// que no tienen nada que ver se marcaban como desfasadas solo por nombrar versiones distintas.
#[test]
fn una_version_a_secas_no_contradice_a_otro_tema() {
    let (mut db, dir) = base("temas");
    guardar(&mut db, Tipo::Fact, "Clave de firma de las Releases", "Desde la 0.14.0 el flujo firma SHA256SUMS con una clave Ed25519 guardada en los secretos del repositorio.", &[]);
    guardar(&mut db, Tipo::Decision, "Eslogan del proyecto", "Desde la 0.14.1 el eslogan es «Memoria eterna, viva y ultrarrápida».", &[]);
    assert!(avisos(&db).is_empty(), "{:?}", avisos(&db));
    // Con nombre sí: la dependencia es la misma cosa aunque las memorias no se parezcan.
    guardar(&mut db, Tipo::Fact, "Compilación en Linux", "Con rusqlite 0.41.0 hace falta glibc 2.35.", &[]);
    guardar(&mut db, Tipo::Gotcha, "Pruebas en macOS", "Desde rusqlite 0.42.0 el bundled ya no falla en Intel.", &[]);
    assert_eq!(avisos(&db).iter().filter(|a| a.0 == revisar::DESFASADA).count(), 1);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Un «está bien así» frente a la memoria de estado no caduca cuando el estado cambia.
#[test]
fn compatible_sobrevive_a_los_cambios_de_la_otra() {
    let (mut db, dir) = base("sobrevive");
    let vieja = guardar(&mut db, Tipo::Fact, "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let estado = guardar(&mut db, Tipo::Fact, "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(avisos(&db).len(), 1);
    revisar::descartar(&db, &vieja, revisar::DESFASADA, &estado).unwrap();
    let c = Cambios { content: Some("La 0.10.0 está publicada en GitHub y crates.io.".into()), reason: Some("otra versión".into()), ..Default::default() };
    memoria::actualizar(&mut db, &estado, c, None).unwrap();
    assert!(avisos(&db).is_empty(), "{:?}", avisos(&db));
    // Pero si cambia la juzgada, el aviso puede volver.
    let c = Cambios { content: Some("La v0.2.0 publicada el 28-oct-2026.\nFalta publicar en crates.io y en Homebrew.".into()), reason: Some("más".into()), ..Default::default() };
    memoria::actualizar(&mut db, &vieja, c, None).unwrap();
    assert_eq!(avisos(&db).len(), 1);
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// 0.19.0: el motivo y el autor se guardan y salen donde sale el veredicto.
#[test]
fn motivo_y_autor_se_guardan_y_salen_en_la_nota() {
    let (mut db, dir) = base("motivo");
    let vieja = guardar(&mut db, Tipo::Decision, "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[]);
    let nueva = guardar(&mut db, Tipo::Decision, "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.", &[]);
    veredictos::poner(&db, &vieja, &nueva, veredictos::SUSTITUIDA, "  JWT replaced the cookie\n in 0.19 ", "agent:claude-code").unwrap();
    let v = veredictos::todos(&db).unwrap();
    assert_eq!((v[0].reason.as_str(), v[0].author.as_str()), ("JWT replaced the cookie\n in 0.19", "agent:claude-code"));
    // En la nota del recall y de la búsqueda, el motivo en una línea.
    let n = notas(&db, &vieja, false).unwrap();
    assert_eq!(n, format!("superseded by {} «La autenticación va con JWT»: JWT replaced the cookie in 0.19", &nueva[..12]), "{n}");
    assert_eq!(notas(&db, &vieja, true).unwrap(), n);
    // En Revisar, con quién lo dijo.
    let a = revisar::avisos(&db, &Ambito::Todos).unwrap().remove(0);
    assert_eq!((a.kind.as_str(), a.other_id.as_str(), a.via), (revisar::SUSTITUIDA, nueva.as_str(), 0));
    assert_eq!((a.reason.as_str(), a.author.as_str()), ("JWT replaced the cookie\n in 0.19", "agent:claude-code"));
    assert_eq!(a.detail, "replaced by the other one: JWT replaced the cookie in 0.19 · by agent:claude-code");
    // Un motivo largo se corta a 120 caracteres en la nota; sin motivo, la nota de siempre.
    let largo = "x".repeat(200);
    veredictos::poner(&db, &vieja, &nueva, veredictos::CONTRADICE, &largo, "Marc").unwrap();
    let n = notas(&db, &vieja, false).unwrap();
    let cabeza = format!("contradicts {} «La autenticación va con JWT»: ", &nueva[..12]);
    assert_eq!(n, format!("{cabeza}{}…", "x".repeat(120)), "{n}");
    let a = revisar::avisos(&db, &Ambito::Todos).unwrap().remove(0);
    assert!(a.detail.ends_with("… · by Marc"), "{}", a.detail);
    veredictos::poner(&db, &vieja, &nueva, veredictos::SUSTITUIDA, "", "").unwrap();
    assert!(notas(&db, &vieja, false).unwrap().ends_with(": use that one"));
    assert_eq!(revisar::avisos(&db, &Ambito::Todos).unwrap()[0].detail, "replaced by the other one");
    // «Está bien así» lo firma el usuario.
    revisar::descartar(&db, &vieja, revisar::SUSTITUIDA, &nueva).unwrap();
    let v = veredictos::todos(&db).unwrap();
    let ok = v.iter().find(|x| x.relation == veredictos::COMPATIBLE).unwrap();
    assert_eq!((ok.author.as_str(), ok.reason.as_str()), ("user", ""));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Quién firma: la persona de la sincronización, o el cliente que llama.
#[test]
fn el_autor_es_la_persona_de_la_sync_o_el_cliente() {
    let (db, dir) = base("autor");
    assert_eq!(veredictos::autor(&db, "mcp:claude-code"), "agent:claude-code");
    assert_eq!(veredictos::autor(&db, "mcp"), "agent");
    assert_eq!(veredictos::autor(&db, ""), "agent");
    assert_eq!(veredictos::autor(&db, "cli"), "cli");
    db.execute("INSERT INTO meta(k, v) VALUES ('autor', 'Marc')", []).unwrap();
    assert_eq!(veredictos::autor(&db, "mcp:claude-code"), "Marc");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// A sustituida por B y B por C: la nota de A lleva a C, con el motivo del último eslabón.
#[test]
fn la_cadena_de_sustituciones_lleva_a_la_ultima() {
    let (mut db, dir) = base("cadena");
    let a = guardar(&mut db, Tipo::Fact, "El puerto es el 8080", "Escucha en el 8080.", &[]);
    let b = guardar(&mut db, Tipo::Fact, "El puerto es el 9090", "Escucha en el 9090 desde la migración.", &[]);
    let c = guardar(&mut db, Tipo::Fact, "El puerto es el 7070", "Escucha en el 7070 detrás de Caddy.", &[]);
    veredictos::poner(&db, &a, &b, veredictos::SUSTITUIDA, "migración", "Marc").unwrap();
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap(), Some((b.clone(), 1)));
    veredictos::poner(&db, &b, &c, veredictos::SUSTITUIDA, "detrás de Caddy", "Ana").unwrap();
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap(), Some((c.clone(), 2)));
    assert_eq!(veredictos::ultima_de_cadena(&db, &b).unwrap(), Some((c.clone(), 1)));
    assert_eq!(veredictos::ultima_de_cadena(&db, &c).unwrap(), None);
    let n = notas(&db, &a, false).unwrap();
    assert_eq!(n, format!("superseded by {} «El puerto es el 7070» (via 2): detrás de Caddy", &c[..12]), "{n}");
    assert_eq!(notas(&db, &b, false).unwrap(), format!("superseded by {} «El puerto es el 7070»: detrás de Caddy", &c[..12]));
    assert_eq!(notas(&db, &c, true), None);
    // En Revisar, A apunta al final de la cadena.
    let av = revisar::avisos(&db, &Ambito::Todos).unwrap();
    let de_a = av.iter().find(|x| x.memory_id == a).unwrap();
    assert_eq!((de_a.other_id.as_str(), de_a.via, de_a.reason.as_str(), de_a.author.as_str()), (c.as_str(), 2, "detrás de Caddy", "Ana"));
    assert_eq!(de_a.detail, "replaced by the other one: detrás de Caddy (via 2) · by Ana");
    let de_b = av.iter().find(|x| x.memory_id == b).unwrap();
    assert_eq!((de_b.other_id.as_str(), de_b.via), (c.as_str(), 0));
    // Si B cambia, su veredicto caduca y la cadena de A se queda en B.
    let cambio = Cambios { content: Some("Escucha en el 9090, y el 7070 es solo el proxy.".into()), reason: Some("aclarado".into()), ..Default::default() };
    memoria::actualizar(&mut db, &b, cambio, None).unwrap();
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap(), Some((b.clone(), 1)));
    assert!(notas(&db, &a, false).unwrap().contains(&b[..12]));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Un ciclo (A → B → A) o una cadena larguísima no cuelgan: el camino corta el ciclo y hay tope.
#[test]
fn un_ciclo_de_sustituciones_no_cuelga() {
    let (mut db, dir) = base("ciclo");
    let a = guardar(&mut db, Tipo::Fact, "Uno", "El backup corre a las 3:00.", &[]);
    let b = guardar(&mut db, Tipo::Fact, "Dos", "El backup corre a las 12:00.", &[]);
    veredictos::poner(&db, &a, &b, veredictos::SUSTITUIDA, "", "").unwrap();
    veredictos::poner(&db, &b, &a, veredictos::SUSTITUIDA, "", "").unwrap();
    // De la pareja manda el más nuevo (B sustituida por A), así que A ya no está sustituida.
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap(), None);
    assert_eq!(veredictos::ultima_de_cadena(&db, &b).unwrap(), Some((a.clone(), 1)));
    // Un ciclo de tres (A → B → C → A): desde A se llega a C y ahí se para; desde C, a B.
    let c = guardar(&mut db, Tipo::Fact, "Tres", "El backup corre a las 18:00.", &[]);
    veredictos::poner(&db, &a, &b, veredictos::SUSTITUIDA, "", "").unwrap();
    veredictos::poner(&db, &b, &c, veredictos::SUSTITUIDA, "", "").unwrap();
    veredictos::poner(&db, &c, &a, veredictos::SUSTITUIDA, "", "").unwrap();
    assert_eq!(veredictos::ultima_de_cadena(&db, &a).unwrap(), Some((c.clone(), 2)));
    assert_eq!(veredictos::ultima_de_cadena(&db, &c).unwrap(), Some((b.clone(), 2)));
    assert!(notas(&db, &a, false).unwrap().contains("(via 2)"));
    // Treinta eslabones: se para en el tope (20), sin colgarse.
    let mut ids = vec![guardar(&mut db, Tipo::Fact, "Eslabón 0", "Primero.", &[])];
    for i in 1..30 {
        let id = guardar(&mut db, Tipo::Fact, &format!("Eslabón {i}"), &format!("Número {i}."), &[]);
        veredictos::poner(&db, &ids[i - 1], &id, veredictos::SUSTITUIDA, "", "").unwrap();
        ids.push(id);
    }
    assert_eq!(veredictos::ultima_de_cadena(&db, &ids[0]).unwrap(), Some((ids[20].clone(), 20)));
    assert_eq!(veredictos::ultima_de_cadena(&db, &ids[27]).unwrap(), Some((ids[29].clone(), 2)));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Esquema 8 → 10: los veredictos que ya había se conservan, sin motivo ni autor, y siguen valiendo.
#[test]
fn los_veredictos_del_esquema_8_se_conservan() {
    let dir = std::env::temp_dir().join(format!("omoi-veredictos-esquema8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = ("01JAAAAAAAAAAAAAAAAAAAAAAA", "01JBBBBBBBBBBBBBBBBBBBBBBB");
    {
        // Una base del esquema 8: la tabla `verdict` sin motivo ni autor, y sellos de contador corto.
        let db = db::abrir_en_esquema(&dir.join("o.db"), 8).unwrap();
        memoria_vieja(&db, 1, a, "Uno", "El puerto es el 8080.", "2026-10-28T10:00:00.000Z");
        memoria_vieja(&db, 2, b, "Dos", "El puerto es el 9090.", "2026-10-28T11:00:00.000Z");
        db.execute_batch(&format!(
            "INSERT INTO verdict(memory_id, other_id, relation, at, hlc) VALUES ('{a}', '{b}', 'superseded', '2026-10-28T12:00:00.000Z', '1793188800000-0003-pc');
             INSERT INTO verdict(memory_id, other_id, relation, at, hlc) VALUES ('{a}', '', 'ok:unused', '2026-10-28T12:00:00.000Z', '1793188800000-0004-pc');"
        ))
        .unwrap();
    }
    let db = db::abrir(&dir.join("o.db")).unwrap();
    let v: i64 = db.query_row("PRAGMA user_version", [], |f| f.get(0)).unwrap();
    assert_eq!(v, db::VERSION_ESQUEMA);
    // Los sellos, al formato de ahora.
    let sello: String = db.query_row("SELECT hlc FROM verdict WHERE relation = 'superseded'", [], |f| f.get(0)).unwrap();
    assert_eq!(sello, "1793188800000-0000000003-pc");
    let mut todos = veredictos::todos(&db).unwrap();
    todos.sort_by(|x, y| x.relation.cmp(&y.relation));
    assert_eq!(todos.len(), 2);
    assert_eq!((todos[0].relation.as_str(), todos[0].reason.as_str(), todos[0].author.as_str()), ("ok:unused", "", ""));
    assert_eq!((todos[1].memory_id.as_str(), todos[1].other_id.as_str(), todos[1].relation.as_str(), todos[1].reason.as_str()), (a, b, "superseded", ""));
    assert!(notas(&db, &a, false).unwrap().ends_with(": use that one"));
    let ok: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    assert_eq!(ok, "ok");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
