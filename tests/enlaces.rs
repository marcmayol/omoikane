//! Phase 7.4: memories connected with `[[name]]`.

mod comun;

use omoikane::db;
use omoikane::enlaces;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::revisar;

#[test]
fn broken_links_and_the_neighbour_in_recall() {
    let dir = comun::Temporal::nueva("enlaces");
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let mut save = |kind, title: &str, content: &str, tag: &str| {
        let n = Nueva { project: String::new(), kind: Some(kind), title: title.into(), content: content.into(), tags: vec![tag.into()], ..Default::default() };
        memoria::guardar(&mut db, n, None).unwrap().id
    };
    let kiln = save(
        Tipo::Fact,
        "Horno de cerámica Ondina en el taller",
        "El horno Ondina del taller cuece a 1240 grados con el programa lento. Antes de abrirlo ver [[pref-horno-ondina]]. Su manual: [[manual_que_no_existe]].",
        "nota_horno_ondina",
    );
    let pref = save(
        Tipo::Preference,
        "Abrir el horno solo por debajo de 80 grados",
        "No abrir la tapa del horno Ondina hasta que marque menos de 80 grados: las piezas se agrietan.",
        "pref_horno_ondina",
    );
    revisar::al_dia(&db).unwrap();
    // The link goes by tag (`-` or `_` makes no difference); the one that leads nowhere shows up in Review.
    let (outgoing, _) = enlaces::vecinas(&db, &kiln).unwrap();
    assert_eq!(outgoing, std::slice::from_ref(&pref));
    let (_, incoming) = enlaces::vecinas(&db, &pref).unwrap();
    assert_eq!(incoming, std::slice::from_ref(&kiln));
    let broken: Vec<_> = revisar::avisos(&db, &Ambito::Todos).unwrap().into_iter().filter(|a| a.kind == revisar::ENLACE_ROTO).collect();
    assert_eq!(broken.len(), 1);
    assert!(broken[0].memory_id == kiln && broken[0].detail.contains("[[manual_que_no_existe]]"), "{:?}", broken[0]);

    // A message about the kiln brings the note and, through its link, the preference not to open it while hot.
    let e = Entrada { session_id: "s1".into(), prompt: "vamos a programar el horno ondina con el programa lento a 1240 grados y luego abrirlo".into(), ..Default::default() };
    let t = recall::al_preguntar(&mut db, "", &e, None).unwrap().expect("algo");
    assert!(t.contains("Horno de cerámica Ondina"), "{t}");
    assert!(t.contains("80 grados"), "la preferencia enlazada debería venir: {t}");
}
