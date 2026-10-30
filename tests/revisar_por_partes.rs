//! La revisión por partes (`revisar::al_dia_de`) da lo mismo que el recálculo entero: tras escrituras
//! al azar sobre una base de más de 2.000 memorias (las frases de `corpus.txt`, con versiones,
//! pendientes y hechos, etiquetas y enlaces), las mismas parejas desfasadas, los mismos enlaces y
//! los mismos enlaces rotos que si se rehiciera todo.

use omoikane::db;
use omoikane::memoria::{self, Cambios, Nueva, Tipo};
use omoikane::revisar;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Un generador de números de siempre (sin dependencias): el test sale igual en cada pasada.
struct Azar(u64);

impl Azar {
    fn sig(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn hasta(&mut self, n: usize) -> usize {
        (self.sig() % n as u64) as usize
    }
    fn uno<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.hasta(v.len())]
    }
}

fn base(nombre: &str) -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("omoi-por-partes-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = db::abrir(&dir.join("o.db")).unwrap();
    db.execute_batch("PRAGMA synchronous = OFF").unwrap();
    (db, dir)
}

fn frases() -> Vec<String> {
    include_str!("corpus.txt").lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).map(String::from).collect()
}

const PROYECTOS: [&str; 3] = ["/alfa", "/beta", ""];

/// Una memoria al azar: frases del corpus, uno o dos temas (palabras raras: cada una sale en unas
/// pocas memorias), a veces una versión (con nombre o a secas), un pendiente o un hecho, una fecha,
/// etiquetas y enlaces a etiquetas, a principios de id o a nada.
fn inventar(az: &mut Azar, fr: &[String], ids: &[String]) -> (String, String, Vec<String>) {
    let tema = format!("tema{:03}", az.hasta(150));
    let otro = format!("asunto{:03}", az.hasta(150));
    let titulo = format!("{tema} {}", fr[az.hasta(fr.len())].chars().take(60).collect::<String>());
    let mut c = format!("{} {otro}. {}", fr[az.hasta(fr.len())], fr[az.hasta(fr.len())]);
    match az.hasta(6) {
        0 => c.push_str(&format!("\nLa gaviota{} {}.{}.{} quedó publicada.", az.hasta(60), az.hasta(2), az.hasta(9), az.hasta(9))),
        1 => c.push_str(&format!("\nUsa libreria{} 0.{}.0 desde el {}-oct-2026.", az.hasta(60), 30 + az.hasta(12), 1 + az.hasta(28))),
        2 => c.push_str(&format!("\nLa {}.{}.0 de {tema}.", az.hasta(3), az.hasta(20))),
        _ => {}
    }
    match az.hasta(5) {
        0 => c.push_str(&format!("\nPendiente renovar el certificado wildcard de {tema} {otro}.")),
        1 => c.push_str(&format!("\nHecho: renovado el certificado wildcard de {tema} {otro}.")),
        _ => {}
    }
    if az.hasta(3) == 0 {
        c.push_str(&format!("\nEl {}-oct-2026.", 1 + az.hasta(28)));
    }
    match az.hasta(6) {
        0 => c.push_str(&format!(" Ver [[etiqueta-{}]].", az.hasta(300))),
        1 => c.push_str(&format!(" Ver [[Etiqueta {}]].", az.hasta(300))),
        2 if !ids.is_empty() => c.push_str(&format!(" Ver [[{}]].", &az.uno(ids)[..8 + az.hasta(6)])),
        3 => c.push_str(&format!(" Ver [[no-existe-{}]].", az.hasta(10))),
        _ => {}
    }
    let tags = (0..az.hasta(3)).map(|_| format!("etiqueta_{}", az.hasta(300))).collect();
    (titulo, c, tags)
}

fn guardar(db: &mut Connection, az: &mut Azar, fr: &[String], ids: &[String]) -> Option<String> {
    let (title, content, tags) = inventar(az, fr, ids);
    let project = az.uno(&PROYECTOS).to_string();
    let n = Nueva { project, kind: Some(Tipo::Fact), title, content, tags, ..Default::default() };
    memoria::guardar(db, n, None).ok().map(|m| m.id)
}

type Foto = (BTreeSet<(String, String, String)>, BTreeSet<(String, String)>, BTreeSet<(String, String)>);

/// Lo que deja la revisión: desfasadas (vieja, nueva, detalle), enlaces rotos y enlaces.
fn foto(db: &Connection) -> Foto {
    let filas = |sql: &str| -> Vec<Vec<String>> {
        let mut q = db.prepare(sql).unwrap();
        let n = q.column_count();
        q.query_map([], |f| (0..n).map(|i| f.get::<_, String>(i)).collect()).unwrap().map(Result::unwrap).collect()
    };
    let des = filas("SELECT memory_id, other_id, detail FROM review WHERE kind = 'outdated'").into_iter().map(|v| (v[0].clone(), v[1].clone(), v[2].clone())).collect();
    let rotos = filas("SELECT memory_id, detail FROM review WHERE kind = 'broken_link'").into_iter().map(|v| (v[0].clone(), v[1].clone())).collect();
    let enl = filas("SELECT from_id, to_id FROM memory_link").into_iter().map(|v| (v[0].clone(), v[1].clone())).collect();
    (des, rotos, enl)
}

/// La revisión rehecha entera, en una copia de la base (la original sigue por partes).
fn foto_entera(db: &Connection, dir: &Path, k: usize) -> Foto {
    let copia = dir.join(format!("entera-{k}.db"));
    db.execute("VACUUM INTO ?1", [copia.to_string_lossy()]).unwrap();
    let c = db::abrir(&copia).unwrap();
    c.execute("DELETE FROM meta WHERE k = 'revision'", []).unwrap();
    revisar::al_dia(&c).unwrap();
    foto(&c)
}

/// La huella que guardó la revisión es la de la base: si no, la siguiente lectura lo rehace todo.
fn huella_al_dia(db: &Connection) -> bool {
    let guardada: String = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |f| f.get(0)).unwrap();
    let real: String = db
        .query_row(
            "SELECT count(*) || '|' || COALESCE(max(updated_at), '') || '|' || COALESCE(max(hlc), '') || '|' || COALESCE(sum(length(project)), 0) FROM memory",
            [],
            |f| f.get(0),
        )
        .unwrap();
    guardada == real
}

fn parciales(db: &Connection) -> i64 {
    db.query_row("SELECT CAST(v AS INTEGER) FROM meta WHERE k = 'revision_parciales'", [], |f| f.get(0)).unwrap_or(0)
}

#[test]
fn por_partes_igual_que_entera() {
    let (mut db, dir) = base("igual");
    let fr = frases();
    let mut az = Azar(0x9E37_79B9_7F4A_7C15);
    let mut ids: Vec<String> = Vec::new();
    while ids.len() < revisar::MINIMO_POR_PARTES + 150 {
        if let Some(id) = guardar(&mut db, &mut az, &fr, &ids) {
            ids.push(id);
        }
    }
    revisar::al_dia(&db).unwrap();
    let inicial = foto(&db);
    assert!(inicial.0.len() > 20 && inicial.1.len() > 20 && inicial.2.len() > 20, "corpus pobre: {} {} {}", inicial.0.len(), inicial.1.len(), inicial.2.len());

    let mut olvidadas: Vec<String> = Vec::new();
    for paso in 1..=80 {
        let antes = revisar::antes(&db).unwrap();
        let tocadas: Vec<String> = match az.hasta(10) {
            0..=3 => guardar(&mut db, &mut az, &fr, &ids).into_iter().collect(),
            4..=6 => {
                let id = az.uno(&ids).clone();
                let (title, content, tags) = inventar(&mut az, &fr, &ids);
                let c = match az.hasta(4) {
                    0 => Cambios { content: Some(content), tags: Some(tags), ..Default::default() },
                    1 => Cambios { title: Some(title), ..Default::default() },
                    2 => Cambios { project: Some(az.uno(&PROYECTOS).to_string()), ..Default::default() },
                    _ => Cambios { content: Some(content), ..Default::default() },
                };
                memoria::actualizar(&mut db, &id, c, None).ok().map(|m| m.id).into_iter().collect()
            }
            7 => {
                let id = ids.swap_remove(az.hasta(ids.len()));
                memoria::olvidar(&mut db, &id).unwrap();
                olvidadas.push(id.clone());
                vec![id]
            }
            8 if !olvidadas.is_empty() => {
                let id = olvidadas.swap_remove(az.hasta(olvidadas.len()));
                memoria::restaurar(&mut db, &id, None).unwrap();
                ids.push(id.clone());
                vec![id]
            }
            _ => {
                let a = az.uno(&ids).clone();
                let b = az.uno(&ids).clone();
                if a == b {
                    continue;
                }
                let (_, content, _) = inventar(&mut az, &fr, &ids);
                memoria::absorber(&mut db, &a, std::slice::from_ref(&b), Cambios { content: Some(content), ..Default::default() }, None).unwrap();
                ids.retain(|x| *x != b);
                vec![a, b]
            }
        };
        revisar::al_dia_de(&db, &antes, &tocadas).unwrap();
        assert!(huella_al_dia(&db), "huella, paso {paso}");
        if paso % 20 == 0 {
            let (por_partes, entera) = (foto(&db), foto_entera(&db, &dir, paso));
            let distintas = por_partes.0.symmetric_difference(&entera.0).count();
            eprintln!("paso {paso}: {} desfasadas, {} rotos, {} enlaces; {distintas} desfasadas distintas", entera.0.len(), entera.1.len(), entera.2.len());
            assert_eq!(por_partes.2, entera.2, "enlaces, paso {paso}");
            assert_eq!(por_partes.1, entera.1, "enlaces rotos, paso {paso}");
            assert_eq!(por_partes.0, entera.0, "desfasadas, paso {paso}");
        }
    }
    // Se ha revisado por partes de verdad, no rehaciendo todo en cada paso.
    assert!(parciales(&db) >= 70, "solo {} por partes", parciales(&db));
    assert_ne!(foto(&db), inicial, "las escrituras no han cambiado nada");
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

/// Lo que se aproxima: qué palabra es rara se toma del último recálculo entero. Una palabra que
/// estaba justo en el tope y pasa a salir en más memorias sigue contando como rara hasta que el
/// número de memorias cambia más de un 5 %; entonces se rehace todo y vuelve a cuadrar.
#[test]
fn la_rareza_se_fija_hasta_el_siguiente_entero() {
    let (mut db, dir) = base("rareza");
    let fr = frases();
    let mut az = Azar(0x2545_F491_4F6C_DD1D);
    let mut ids = Vec::new();
    while ids.len() < revisar::MINIMO_POR_PARTES {
        if let Some(id) = guardar(&mut db, &mut az, &fr, &ids) {
            ids.push(id);
        }
    }
    // «frontera» en 99 memorias de otro proyecto: rara (el tope es 100).
    for i in 0..99 {
        let n = Nueva { project: "/otro".into(), kind: Some(Tipo::Fact), title: format!("Nota {i} de la frontera"), content: "Algo sin más.".into(), ..Default::default() };
        memoria::guardar(&mut db, n, None).unwrap();
    }
    revisar::al_dia(&db).unwrap();
    let guardar_par = |db: &mut Connection, titulo: &str, version: &str| {
        let antes = revisar::antes(db).unwrap();
        let n = Nueva {
            project: "/frontera".into(),
            kind: Some(Tipo::Fact),
            title: titulo.into(),
            content: format!("La frontera en la {version}."),
            ..Default::default()
        };
        let id = memoria::guardar(db, n, None).unwrap().id;
        revisar::al_dia_de(db, &antes, std::slice::from_ref(&id)).unwrap();
    };
    // La 100.ª y la 101.ª: entera ya no es rara y las dos no comparten nada; por partes aún lo es,
    // hablan de lo mismo y la 0.2.0 deja anticuada a la 0.1.0.
    guardar_par(&mut db, "Frontera norte", "0.1.0");
    guardar_par(&mut db, "Frontera oeste", "0.2.0");
    let por_partes = foto(&db).0.len();
    let entera = foto_entera(&db, &dir, 0).0.len();
    eprintln!("por partes {por_partes} desfasadas, entera {entera}");
    assert_eq!(por_partes, entera + 1, "la diferencia esperada es justo esa pareja");
    // Más de un 5 % de memorias nuevas: se rehace entera y cuadra.
    for _ in 0..(ids.len() + 99) / 20 + 1 {
        let antes = revisar::antes(&db).unwrap();
        let id = guardar(&mut db, &mut az, &fr, &ids).unwrap();
        revisar::al_dia_de(&db, &antes, &[id]).unwrap();
    }
    assert_eq!(foto(&db), foto_entera(&db, &dir, 1));
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}
