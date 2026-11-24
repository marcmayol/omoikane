//! 0.17.0: verdicts. What the agent says about two memories (supersedes, they contradict, both
//! hold) and the review date for each kind, like engram's `mem_judge`, `mem_compare` and `mem_review`,
//! but without new tools and without asking for a judgement on every save.
//! 0.19.0: each verdict with its reason and its author, and chained supersessions (A → B → C).

mod common;

use omoikane::db;
use omoikane::memory::{self, Scope, Changes, NewMemory, Kind};
use omoikane::review;
use omoikane::verdicts::{self, Verdict};
use rusqlite::Connection;

fn fixture(name: &str) -> (common::TempDir, Connection) {
    let dir = common::TempDir::new(&format!("veredictos-{name}"));
    let db = db::open(&dir.join("o.db")).unwrap();
    (dir, db)
}

fn save(db: &mut Connection, kind: Kind, title: &str, content: &str, tags: &[&str]) -> String {
    let n = NewMemory {
        project: "/p".into(),
        kind: Some(kind),
        title: title.into(),
        content: content.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        ..Default::default()
    };
    memory::save(db, n, None).unwrap().id
}

fn warnings(db: &Connection) -> Vec<(String, String, String)> {
    review::notices(db, &Scope::All).unwrap().into_iter().map(|a| (a.kind, a.memory_id, a.other_id)).collect()
}

fn notes(db: &Connection, id: &str, search: bool) -> Option<String> {
    let m = memory::get(db, id).unwrap();
    let mut n = if search { review::search_notes(db, &Scope::All, &[&m]) } else { review::notes(db, &Scope::All, &[&m]) }.unwrap();
    n.remove(id)
}

#[test]
fn superseded_shows_in_review_recall_and_search() {
    let (_dir, mut db) = fixture("sustituye");
    let old_id = save(&mut db, Kind::Decision, "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[]);
    let new_id = save(&mut db, Kind::Decision, "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.", &[]);
    assert!(warnings(&db).is_empty());
    verdicts::set(&db, &old_id, &new_id, verdicts::SUPERSEDED, "", "").unwrap();
    assert_eq!(warnings(&db), vec![(review::SUPERSEDED.to_string(), old_id.clone(), new_id.clone())]);
    let n = notes(&db, &old_id, false).unwrap();
    assert!(n.starts_with("superseded by") && n.contains(&new_id[..12]), "{n}");
    // The new one carries no note: it is the good one.
    assert_eq!(notes(&db, &new_id, true), None);
    // If the old one is corrected afterwards, the verdict no longer holds.
    let c = Changes { content: Some("La cookie sigue para la web; la API va con JWT.".into()), reason: Some("aclarado".into()), ..Default::default() };
    memory::update(&mut db, &old_id, c, None).unwrap();
    assert!(warnings(&db).is_empty());
}

#[test]
fn contradict_until_one_changes_and_compatible_clears_warning() {
    let (_dir, mut db) = fixture("contradicen");
    let a = save(&mut db, Kind::Fact, "El backup se hace de noche", "La copia corre a las 3:00.", &[]);
    let b = save(&mut db, Kind::Fact, "El backup se hace al mediodía", "La copia corre a las 12:00.", &[]);
    verdicts::set(&db, &a, &b, verdicts::CONTRADICTS, "", "").unwrap();
    assert_eq!(warnings(&db), vec![(review::CONTRADICTS.to_string(), a.clone(), b.clone())]);
    // Both carry the note, each naming the other.
    assert!(notes(&db, &a, false).unwrap().contains(&b[..12]));
    assert!(notes(&db, &b, false).unwrap().contains(&a[..12]));
    // The pair's newest verdict rules, in either order.
    verdicts::set(&db, &b, &a, verdicts::COMPATIBLE, "", "").unwrap();
    assert!(warnings(&db).is_empty());
    assert_eq!(notes(&db, &a, true), None);
}

#[test]
fn fine_as_is_on_outdated_is_compatible() {
    let (_dir, mut db) = fixture("desfasada");
    let old_id = save(&mut db, Kind::Fact, "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let new_id = save(&mut db, Kind::Fact, "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(warnings(&db), vec![(review::OUTDATED.to_string(), old_id.clone(), new_id.clone())]);
    review::dismiss(&db, &old_id, review::OUTDATED, &new_id).unwrap();
    assert!(warnings(&db).is_empty());
    assert_eq!(notes(&db, &old_id, false), None);
}

#[test]
fn review_date_by_kind() {
    let (_dir, mut db) = fixture("revision");
    let decision = save(&mut db, Kind::Decision, "Los precios van en céntimos", "Todos los importes en enteros de céntimos.", &[]);
    let fact = save(&mut db, Kind::Fact, "El servidor de pruebas es lento", "Tarda unos 10 s en arrancar.", &[]);
    let pref = save(&mut db, Kind::Preference, "Commits cortos y en inglés", "Una línea, en inglés.", &[]);
    // 150 days ago: the decision is due (120), the fact is not (180), the preference never.
    db.execute("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-150 days'), created_at = updated_at", []).unwrap();
    let due: Vec<String> = warnings(&db).into_iter().filter(|a| a.0 == review::DUE).map(|a| a.1).collect();
    assert_eq!(due, vec![decision.clone()]);
    // It shows in search but not in recall, which only warns about what may be wrong.
    assert!(notes(&db, &decision, true).unwrap().starts_with("not reviewed in 150 days"));
    assert_eq!(notes(&db, &decision, false), None);
    assert_eq!(notes(&db, &fact, true), None);
    assert_eq!(notes(&db, &pref, true), None);
    // If the agent received it recently and nobody corrected it, it is reviewed: not due.
    db.execute("UPDATE memory SET recalled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 days'), recall_count = 3 WHERE id = ?1", [&decision]).unwrap();
    assert!(!warnings(&db).iter().any(|a| a.0 == review::DUE));
    db.execute("UPDATE memory SET recalled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-140 days') WHERE id = ?1", [&decision]).unwrap();
    assert!(warnings(&db).iter().any(|a| a.0 == review::DUE), "recordada hace 140 días: sí toca");
    // «Fine as is» resets the clock.
    review::dismiss(&db, &decision, review::DUE, "").unwrap();
    assert!(!warnings(&db).iter().any(|a| a.0 == review::DUE));
}

/// A date set when saving (`review:YYYY-MM-DD`): it warns on that day even if the memory is new and of
/// a kind that doesn't expire, also in recall, and stops warning once touched or accepted.
#[test]
fn review_date_set_by_hand() {
    let (_dir, mut db) = fixture("fecha");
    let today: String = db.query_row("SELECT date('now', 'localtime')", [], |f| f.get(0)).unwrap();
    let tomorrow: String = db.query_row("SELECT date('now', 'localtime', '+1 day')", [], |f| f.get(0)).unwrap();
    let token = save(&mut db, Kind::Gotcha, "El token del vigilante caduca", "Hay que renovarlo en el panel.", &[&format!("review:{today}")]);
    let offer = save(&mut db, Kind::Preference, "Usar la oferta del hosting", "Precio rebajado.", &[&format!("review:{tomorrow}")]);
    // Saved a month ago with today's date.
    db.execute("UPDATE memory SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days'), created_at = updated_at", []).unwrap();
    let dates: Vec<String> = warnings(&db).into_iter().filter(|a| a.0 == review::REVIEW_DATE).map(|a| a.1).collect();
    assert_eq!(dates, vec![token.clone()], "la de mañana aún no");
    let a = review::notices(&db, &Scope::All).unwrap().into_iter().find(|a| a.kind == review::REVIEW_DATE).unwrap();
    assert_eq!(a.detail, format!("review date {today} reached: is it still true?"));
    // It shows in recall (it may no longer hold) and in search.
    assert_eq!(notes(&db, &token, false).unwrap(), format!("review date {today} reached: check it still holds"));
    assert!(notes(&db, &token, true).is_some());
    assert_eq!(notes(&db, &offer, false), None);
    // A single reached date is enough for the startup warning, far below the threshold.
    let counts = review::pending(&db, &Scope::All).unwrap();
    assert_eq!(counts.get(review::REVIEW_DATE), Some(&1));
    let warning = review::notice(&db, &counts, 2).unwrap().expect("avisa con una sola");
    assert!(warning.contains("1 review dates reached"), "{warning}");
    // «Fine as is» marks it as reviewed.
    review::dismiss(&db, &token, review::REVIEW_DATE, "").unwrap();
    assert!(!warnings(&db).iter().any(|a| a.0 == review::REVIEW_DATE));
    assert_eq!(notes(&db, &token, false), None);
    // A tag that is not a date is not saved.
    let n = NewMemory { project: "/p".into(), kind: Some(Kind::Fact), title: "Mal".into(), content: "x".into(), tags: vec!["review:25-nov".into()], ..Default::default() };
    let e = memory::save(&mut db, n, None).unwrap_err().to_string();
    assert!(e.contains("review:YYYY-MM-DD"), "{e}");
    let c = Changes { tags: Some(vec!["review:2026-13-01".into()]), ..Default::default() };
    assert!(memory::update(&mut db, &offer, c, None).is_err());
}

#[test]
fn from_another_machine_newest_wins() {
    let (_dir, mut db) = fixture("recibir");
    let a = save(&mut db, Kind::Fact, "Uno", "El puerto es el 8080.", &[]);
    let b = save(&mut db, Kind::Fact, "Dos", "El puerto es el 9090.", &[]);
    verdicts::set(&db, &a, &b, verdicts::CONTRADICTS, "", "").unwrap();
    let mine = verdicts::all(&db).unwrap().remove(0);
    // An older one of the same verdict doesn't get in; a newer one for the pair rules.
    let older = Verdict { hlc: "0000000000001-0000000000-otro".into(), ..mine.clone() };
    assert!(!verdicts::receive(&db, &older).unwrap());
    let future = format!("{}-0000000000-otro", mine.hlc[..13].parse::<u64>().unwrap().saturating_add(1000));
    let newer = Verdict { memory_id: b.clone(), other_id: a.clone(), relation: verdicts::COMPATIBLE.into(), hlc: future, ..mine };
    assert!(verdicts::receive(&db, &newer).unwrap());
    assert!(warnings(&db).is_empty());
}

/// A memory written by hand into a database with an old schema (with `project` as a path).
fn insert_old_memory(db: &Connection, n: i64, id: &str, title: &str, content: &str, when: &str) {
    db.execute(
        "INSERT INTO memory(n, id, project, kind, title, content, created_at, updated_at) VALUES (?1, ?2, '/p', 'fact', ?3, ?4, ?5, ?5)",
        rusqlite::params![n, id, title, content, when],
    )
    .unwrap();
    db.execute("INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, '', '')", rusqlite::params![n, title, content]).unwrap();
}

#[test]
fn old_dismissals_become_verdicts() {
    let dir = common::TempDir::new("veredictos-migrar");
    let a = "01JAAAAAAAAAAAAAAAAAAAAAAA";
    {
        // A schema 7 database with a «fine as is» in `review`.
        let db = db::open_at_schema(&dir.join("o.db"), 7).unwrap();
        insert_old_memory(&db, 1, a, "Uno", "Algo que nadie usa.", "2026-10-28T00:00:00.000Z");
        db.execute(
            "INSERT INTO review(memory_id, kind, other_id, detail, found_at, dismissed_at) VALUES (?1, 'unused', '', '', '2026-10-29T00:00:00.000Z', '2026-10-29T00:01:00.000Z')",
            [a],
        )
        .unwrap();
    }
    let db = db::open(&dir.join("o.db")).unwrap();
    let v = verdicts::all(&db).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!((v[0].memory_id.as_str(), v[0].relation.as_str()), (a, "ok:unused"));
    // Schema 10 leaves `review` as a cache: without `dismissed_at`.
    assert!(db.query_row("SELECT count(*) FROM review WHERE dismissed_at IS NOT NULL", [], |f| f.get::<_, i64>(0)).is_err());
    // And the «fine as is» still holds (the memory hasn't changed since then).
    assert!(verdicts::oks(&db, true).unwrap().contains_key(&(a.to_string(), "unused".to_string())));
}

/// The false positives from Marc's real database (30-oct-2026): within a project, two unrelated
/// memories were marked as outdated just for naming different versions.
#[test]
fn bare_version_does_not_contradict_other_topic() {
    let (_dir, mut db) = fixture("temas");
    save(&mut db, Kind::Fact, "Clave de firma de las Releases", "Desde la 0.14.0 el flujo firma SHA256SUMS con una clave Ed25519 guardada en los secretos del repositorio.", &[]);
    save(&mut db, Kind::Decision, "Eslogan del proyecto", "Desde la 0.14.1 el eslogan es «Memoria eterna, viva y ultrarrápida».", &[]);
    assert!(warnings(&db).is_empty(), "{:?}", warnings(&db));
    // With a name, yes: the dependency is the same thing even if the memories don't look alike.
    save(&mut db, Kind::Fact, "Compilación en Linux", "Con rusqlite 0.41.0 hace falta glibc 2.35.", &[]);
    save(&mut db, Kind::Gotcha, "Pruebas en macOS", "Desde rusqlite 0.42.0 el bundled ya no falla en Intel.", &[]);
    assert_eq!(warnings(&db).iter().filter(|a| a.0 == review::OUTDATED).count(), 1);
}

/// A «fine as is» against the status memory doesn't expire when the status changes.
#[test]
fn compatible_survives_changes_to_the_other() {
    let (_dir, mut db) = fixture("sobrevive");
    let old_id = save(&mut db, Kind::Fact, "Lumbre: memoria para agentes en Rust", "La v0.2.0 publicada el 28-oct-2026 con el instalador.\nFalta publicar en crates.io.", &[]);
    let status_id = save(&mut db, Kind::Fact, "Lumbre: estado al 29-oct-2026", "La 0.9.2 está publicada en GitHub y crates.io.", &["status"]);
    assert_eq!(warnings(&db).len(), 1);
    review::dismiss(&db, &old_id, review::OUTDATED, &status_id).unwrap();
    let c = Changes { content: Some("La 0.10.0 está publicada en GitHub y crates.io.".into()), reason: Some("otra versión".into()), ..Default::default() };
    memory::update(&mut db, &status_id, c, None).unwrap();
    assert!(warnings(&db).is_empty(), "{:?}", warnings(&db));
    // But if the judged one changes, the warning can come back.
    let c = Changes { content: Some("La v0.2.0 publicada el 28-oct-2026.\nFalta publicar en crates.io y en Homebrew.".into()), reason: Some("más".into()), ..Default::default() };
    memory::update(&mut db, &old_id, c, None).unwrap();
    assert_eq!(warnings(&db).len(), 1);
}

/// 0.19.0: the reason and the author are saved and shown wherever the verdict is.
#[test]
fn reason_and_author_saved_and_shown_in_note() {
    let (_dir, mut db) = fixture("motivo");
    let old_id = save(&mut db, Kind::Decision, "Las sesiones van por cookie", "La autenticación usa una cookie de sesión.", &[]);
    let new_id = save(&mut db, Kind::Decision, "La autenticación va con JWT", "Se pasó a JWT en la cabecera Authorization.", &[]);
    verdicts::set(&db, &old_id, &new_id, verdicts::SUPERSEDED, "  JWT replaced the cookie\n in 0.19 ", "agent:claude-code").unwrap();
    let v = verdicts::all(&db).unwrap();
    assert_eq!((v[0].reason.as_str(), v[0].author.as_str()), ("JWT replaced the cookie\n in 0.19", "agent:claude-code"));
    // In the recall and search note, the reason on one line.
    let n = notes(&db, &old_id, false).unwrap();
    assert_eq!(n, format!("superseded by {} «La autenticación va con JWT»: JWT replaced the cookie in 0.19", &new_id[..12]), "{n}");
    assert_eq!(notes(&db, &old_id, true).unwrap(), n);
    // In Review, with who said it.
    let a = review::notices(&db, &Scope::All).unwrap().remove(0);
    assert_eq!((a.kind.as_str(), a.other_id.as_str(), a.via), (review::SUPERSEDED, new_id.as_str(), 0));
    assert_eq!((a.reason.as_str(), a.author.as_str()), ("JWT replaced the cookie\n in 0.19", "agent:claude-code"));
    assert_eq!(a.detail, "replaced by the other one: JWT replaced the cookie in 0.19 · by agent:claude-code");
    // A long reason is cut to 120 characters in the note; without a reason, the usual note.
    let long = "x".repeat(200);
    verdicts::set(&db, &old_id, &new_id, verdicts::CONTRADICTS, &long, "Marc").unwrap();
    let n = notes(&db, &old_id, false).unwrap();
    let head = format!("contradicts {} «La autenticación va con JWT» (on hold until the user says which holds): ", &new_id[..12]);
    assert_eq!(n, format!("{head}{}…", "x".repeat(120)), "{n}");
    let a = review::notices(&db, &Scope::All).unwrap().remove(0);
    assert!(a.detail.ends_with("… · by Marc"), "{}", a.detail);
    verdicts::set(&db, &old_id, &new_id, verdicts::SUPERSEDED, "", "").unwrap();
    assert!(notes(&db, &old_id, false).unwrap().ends_with(": use that one"));
    assert_eq!(review::notices(&db, &Scope::All).unwrap()[0].detail, "replaced by the other one");
    // «Fine as is» is signed by the user.
    review::dismiss(&db, &old_id, review::SUPERSEDED, &new_id).unwrap();
    let v = verdicts::all(&db).unwrap();
    let ok = v.iter().find(|x| x.relation == verdicts::COMPATIBLE).unwrap();
    assert_eq!((ok.author.as_str(), ok.reason.as_str()), ("user", ""));
}

/// Who signs: the sync person, or the calling client.
#[test]
fn author_is_sync_person_or_client() {
    let (_dir, db) = fixture("autor");
    // The library is called in this process, which inherits the environment of whoever runs `cargo test`: if
    // it is an agent session with `OMOIKANE_AUTHOR`, that author signs above everything (and removing it
    // here would mean touching the environment of a process with several test threads).
    let from_session = omoikane::environment::author();
    let expected = |fallback: &str| from_session.clone().unwrap_or_else(|| fallback.to_string());
    assert_eq!(verdicts::author(&db, "mcp:claude-code", ""), expected("agent:claude-code"));
    assert_eq!(verdicts::author(&db, "mcp", ""), expected("agent"));
    assert_eq!(verdicts::author(&db, "", ""), expected("agent"));
    assert_eq!(verdicts::author(&db, "cli", ""), expected("cli"));
    db.execute("INSERT INTO meta(k, v) VALUES ('autor', 'Marc')", []).unwrap();
    assert_eq!(verdicts::author(&db, "mcp:claude-code", ""), expected("Marc"));
}

/// A superseded by B and B by C: A's note leads to C, with the reason of the last link.
#[test]
fn supersession_chain_leads_to_last() {
    let (_dir, mut db) = fixture("cadena");
    let a = save(&mut db, Kind::Fact, "El puerto es el 8080", "Escucha en el 8080.", &[]);
    let b = save(&mut db, Kind::Fact, "El puerto es el 9090", "Escucha en el 9090 desde la migración.", &[]);
    let c = save(&mut db, Kind::Fact, "El puerto es el 7070", "Escucha en el 7070 detrás de Caddy.", &[]);
    verdicts::set(&db, &a, &b, verdicts::SUPERSEDED, "migración", "Marc").unwrap();
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap(), Some((b.clone(), 1)));
    verdicts::set(&db, &b, &c, verdicts::SUPERSEDED, "detrás de Caddy", "Ana").unwrap();
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap(), Some((c.clone(), 2)));
    assert_eq!(verdicts::last_in_chain(&db, &b).unwrap(), Some((c.clone(), 1)));
    assert_eq!(verdicts::last_in_chain(&db, &c).unwrap(), None);
    let n = notes(&db, &a, false).unwrap();
    assert_eq!(n, format!("superseded by {} «El puerto es el 7070» (via 2): detrás de Caddy", &c[..12]), "{n}");
    assert_eq!(notes(&db, &b, false).unwrap(), format!("superseded by {} «El puerto es el 7070»: detrás de Caddy", &c[..12]));
    assert_eq!(notes(&db, &c, true), None);
    // In Review, A points to the end of the chain.
    let warns = review::notices(&db, &Scope::All).unwrap();
    let of_a = warns.iter().find(|x| x.memory_id == a).unwrap();
    assert_eq!((of_a.other_id.as_str(), of_a.via, of_a.reason.as_str(), of_a.author.as_str()), (c.as_str(), 2, "detrás de Caddy", "Ana"));
    assert_eq!(of_a.detail, "replaced by the other one: detrás de Caddy (via 2) · by Ana");
    let of_b = warns.iter().find(|x| x.memory_id == b).unwrap();
    assert_eq!((of_b.other_id.as_str(), of_b.via), (c.as_str(), 0));
    // If B changes, its verdict expires and A's chain stops at B.
    let change = Changes { content: Some("Escucha en el 9090, y el 7070 es solo el proxy.".into()), reason: Some("aclarado".into()), ..Default::default() };
    memory::update(&mut db, &b, change, None).unwrap();
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap(), Some((b.clone(), 1)));
    assert!(notes(&db, &a, false).unwrap().contains(&b[..12]));
}

/// A cycle (A → B → A) or a very long chain doesn't hang: the walk cuts the cycle and there is a cap.
#[test]
fn supersession_cycle_does_not_hang() {
    let (_dir, mut db) = fixture("ciclo");
    let a = save(&mut db, Kind::Fact, "Uno", "El backup corre a las 3:00.", &[]);
    let b = save(&mut db, Kind::Fact, "Dos", "El backup corre a las 12:00.", &[]);
    verdicts::set(&db, &a, &b, verdicts::SUPERSEDED, "", "").unwrap();
    verdicts::set(&db, &b, &a, verdicts::SUPERSEDED, "", "").unwrap();
    // The pair's newest one rules (B superseded by A), so A is no longer superseded.
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap(), None);
    assert_eq!(verdicts::last_in_chain(&db, &b).unwrap(), Some((a.clone(), 1)));
    // A cycle of three (A → B → C → A): from A it reaches C and stops there; from C, B.
    let c = save(&mut db, Kind::Fact, "Tres", "El backup corre a las 18:00.", &[]);
    verdicts::set(&db, &a, &b, verdicts::SUPERSEDED, "", "").unwrap();
    verdicts::set(&db, &b, &c, verdicts::SUPERSEDED, "", "").unwrap();
    verdicts::set(&db, &c, &a, verdicts::SUPERSEDED, "", "").unwrap();
    assert_eq!(verdicts::last_in_chain(&db, &a).unwrap(), Some((c.clone(), 2)));
    assert_eq!(verdicts::last_in_chain(&db, &c).unwrap(), Some((b.clone(), 2)));
    assert!(notes(&db, &a, false).unwrap().contains("(via 2)"));
    // Thirty links: it stops at the cap (20), without hanging.
    let mut ids = vec![save(&mut db, Kind::Fact, "Eslabón 0", "Primero.", &[])];
    for i in 1..30 {
        let id = save(&mut db, Kind::Fact, &format!("Eslabón {i}"), &format!("Número {i}."), &[]);
        verdicts::set(&db, &ids[i - 1], &id, verdicts::SUPERSEDED, "", "").unwrap();
        ids.push(id);
    }
    assert_eq!(verdicts::last_in_chain(&db, &ids[0]).unwrap(), Some((ids[20].clone(), 20)));
    assert_eq!(verdicts::last_in_chain(&db, &ids[27]).unwrap(), Some((ids[29].clone(), 2)));
}

/// Schema 8 → 10: the existing verdicts are kept, without reason or author, and still hold.
#[test]
fn schema_8_verdicts_are_kept() {
    let dir = common::TempDir::new("veredictos-esquema8");
    let (a, b) = ("01JAAAAAAAAAAAAAAAAAAAAAAA", "01JBBBBBBBBBBBBBBBBBBBBBBB");
    {
        // A schema 8 database: the `verdict` table without reason or author, and short-counter stamps.
        let db = db::open_at_schema(&dir.join("o.db"), 8).unwrap();
        insert_old_memory(&db, 1, a, "Uno", "El puerto es el 8080.", "2026-10-28T10:00:00.000Z");
        insert_old_memory(&db, 2, b, "Dos", "El puerto es el 9090.", "2026-10-28T11:00:00.000Z");
        db.execute_batch(&format!(
            "INSERT INTO verdict(memory_id, other_id, relation, at, hlc) VALUES ('{a}', '{b}', 'superseded', '2026-10-28T12:00:00.000Z', '1793188800000-0003-pc');
             INSERT INTO verdict(memory_id, other_id, relation, at, hlc) VALUES ('{a}', '', 'ok:unused', '2026-10-28T12:00:00.000Z', '1793188800000-0004-pc');"
        ))
        .unwrap();
    }
    let db = db::open(&dir.join("o.db")).unwrap();
    let v: i64 = db.query_row("PRAGMA user_version", [], |f| f.get(0)).unwrap();
    assert_eq!(v, db::SCHEMA_VERSION);
    // The stamps, in the current format.
    let stamp: String = db.query_row("SELECT hlc FROM verdict WHERE relation = 'superseded'", [], |f| f.get(0)).unwrap();
    assert_eq!(stamp, "1793188800000-0000000003-pc");
    let mut all = verdicts::all(&db).unwrap();
    all.sort_by(|x, y| x.relation.cmp(&y.relation));
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].relation.as_str(), all[0].reason.as_str(), all[0].author.as_str()), ("ok:unused", "", ""));
    assert_eq!((all[1].memory_id.as_str(), all[1].other_id.as_str(), all[1].relation.as_str(), all[1].reason.as_str()), (a, b, "superseded", ""));
    assert!(notes(&db, a, false).unwrap().ends_with(": use that one"));
    let ok: String = db.query_row("PRAGMA integrity_check", [], |f| f.get(0)).unwrap();
    assert_eq!(ok, "ok");
}
