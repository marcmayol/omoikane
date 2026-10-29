//! Fase 7.4: memorias conectadas con `[[nombre]]`.

use omoikane::db;
use omoikane::enlaces;
use omoikane::memoria::{self, Ambito, Nueva, Tipo};
use omoikane::recall::{self, Entrada};
use omoikane::revisar;

#[test]
fn enlaces_rotos_y_la_vecina_en_el_recall() {
    let dir = std::env::temp_dir().join(format!("omoi-enlaces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut db = db::abrir(&dir.join("o.db")).unwrap();
    let mut guardar = |kind, title: &str, content: &str, tag: &str| {
        let n = Nueva { project: String::new(), kind: Some(kind), title: title.into(), content: content.into(), tags: vec![tag.into()], ..Default::default() };
        memoria::guardar(&mut db, n, None).unwrap().id
    };
    let horno = guardar(
        Tipo::Fact,
        "Horno de cerámica Ondina en el taller",
        "El horno Ondina del taller cuece a 1240 grados con el programa lento. Antes de abrirlo ver [[pref-horno-ondina]]. Su manual: [[manual_que_no_existe]].",
        "nota_horno_ondina",
    );
    let pref = guardar(
        Tipo::Preference,
        "Abrir el horno solo por debajo de 80 grados",
        "No abrir la tapa del horno Ondina hasta que marque menos de 80 grados: las piezas se agrietan.",
        "pref_horno_ondina",
    );
    revisar::al_dia(&db).unwrap();
    // El enlace va por etiqueta (con `-` o `_` da igual); el que no lleva a nada sale en Revisar.
    let (salen, _) = enlaces::vecinas(&db, &horno).unwrap();
    assert_eq!(salen, std::slice::from_ref(&pref));
    let (_, entran) = enlaces::vecinas(&db, &pref).unwrap();
    assert_eq!(entran, std::slice::from_ref(&horno));
    let rotos: Vec<_> = revisar::avisos(&db, &Ambito::Todos).unwrap().into_iter().filter(|a| a.kind == revisar::ENLACE_ROTO).collect();
    assert_eq!(rotos.len(), 1);
    assert!(rotos[0].memory_id == horno && rotos[0].detail.contains("[[manual_que_no_existe]]"), "{:?}", rotos[0]);

    // Un mensaje sobre el horno trae la nota y, por su enlace, la preferencia de no abrirlo en caliente.
    let e = Entrada { session_id: "s1".into(), prompt: "vamos a programar el horno ondina con el programa lento a 1240 grados y luego abrirlo".into(), ..Default::default() };
    let t = recall::al_preguntar(&mut db, "", &e, None).unwrap().expect("algo");
    assert!(t.contains("Horno de cerámica Ondina"), "{t}");
    assert!(t.contains("80 grados"), "la preferencia enlazada debería venir: {t}");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
