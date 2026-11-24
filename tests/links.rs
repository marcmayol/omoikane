//! Phase 7.4: memories connected with `[[name]]`.

mod common;

use omoikane::db;
use omoikane::links;
use omoikane::memory::{self, Scope, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::review;

#[test]
fn broken_links_and_the_neighbour_in_recall() {
    let dir = common::TempDir::new("enlaces");
    let mut db = db::open(&dir.join("o.db")).unwrap();
    let mut save = |kind, title: &str, content: &str, tag: &str| {
        let n = NewMemory { project: String::new(), kind: Some(kind), title: title.into(), content: content.into(), tags: vec![tag.into()], ..Default::default() };
        memory::save(&mut db, n, None).unwrap().id
    };
    let kiln = save(
        Kind::Fact,
        "Horno de cerámica Ondina en el taller",
        "El horno Ondina del taller cuece a 1240 grados con el programa lento. Antes de abrirlo ver [[pref-horno-ondina]]. Su manual: [[manual_que_no_existe]].",
        "nota_horno_ondina",
    );
    let pref = save(
        Kind::Preference,
        "Abrir el horno solo por debajo de 80 grados",
        "No abrir la tapa del horno Ondina hasta que marque menos de 80 grados: las piezas se agrietan.",
        "pref_horno_ondina",
    );
    review::refresh(&db).unwrap();
    // The link goes by tag (`-` or `_` makes no difference); the one that leads nowhere shows up in Review.
    let (outgoing, _) = links::neighbours(&db, &kiln).unwrap();
    assert_eq!(outgoing, std::slice::from_ref(&pref));
    let (_, incoming) = links::neighbours(&db, &pref).unwrap();
    assert_eq!(incoming, std::slice::from_ref(&kiln));
    let broken: Vec<_> = review::notices(&db, &Scope::All).unwrap().into_iter().filter(|a| a.kind == review::BROKEN_LINK).collect();
    assert_eq!(broken.len(), 1);
    assert!(broken[0].memory_id == kiln && broken[0].detail.contains("[[manual_que_no_existe]]"), "{:?}", broken[0]);

    // A message about the kiln brings the note and, through its link, the preference not to open it while hot.
    let e = HookInput { session_id: "s1".into(), prompt: "vamos a programar el horno ondina con el programa lento a 1240 grados y luego abrirlo".into(), ..Default::default() };
    let t = recall::on_prompt(&mut db, "", &e, None).unwrap().expect("algo");
    assert!(t.contains("Horno de cerámica Ondina"), "{t}");
    assert!(t.contains("80 grados"), "la preferencia enlazada debería venir: {t}");
}
