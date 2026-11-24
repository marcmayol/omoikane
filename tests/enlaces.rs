//! Phase 7.4: memories connected with `[[name]]`.

mod comun;

use omoikane::db;
use omoikane::enlaces;
use omoikane::memoria::{self, Scope, NewMemory, Kind};
use omoikane::recall::{self, HookInput};
use omoikane::revisar;

#[test]
fn broken_links_and_the_neighbour_in_recall() {
    let dir = comun::Temporal::nueva("enlaces");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let mut save = |kind, title: &str, content: &str, tag: &str| {
        let n = NewMemory { project: String::new(), kind: Some(kind), title: title.into(), content: content.into(), tags: vec![tag.into()], ..Default::default() };
        memoria::save(&mut db, n, None).unwrap().id
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
    revisar::refresh(&db).unwrap();
    // The link goes by tag (`-` or `_` makes no difference); the one that leads nowhere shows up in Review.
    let (outgoing, _) = enlaces::neighbours(&db, &kiln).unwrap();
    assert_eq!(outgoing, std::slice::from_ref(&pref));
    let (_, incoming) = enlaces::neighbours(&db, &pref).unwrap();
    assert_eq!(incoming, std::slice::from_ref(&kiln));
    let broken: Vec<_> = revisar::notices(&db, &Scope::All).unwrap().into_iter().filter(|a| a.kind == revisar::BROKEN_LINK).collect();
    assert_eq!(broken.len(), 1);
    assert!(broken[0].memory_id == kiln && broken[0].detail.contains("[[manual_que_no_existe]]"), "{:?}", broken[0]);

    // A message about the kiln brings the note and, through its link, the preference not to open it while hot.
    let e = HookInput { session_id: "s1".into(), prompt: "vamos a programar el horno ondina con el programa lento a 1240 grados y luego abrirlo".into(), ..Default::default() };
    let t = recall::on_prompt(&mut db, "", &e, None).unwrap().expect("algo");
    assert!(t.contains("Horno de cerámica Ondina"), "{t}");
    assert!(t.contains("80 grados"), "la preferencia enlazada debería venir: {t}");
}
