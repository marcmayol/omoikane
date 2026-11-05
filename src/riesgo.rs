//! Historial de riesgo por fichero (1.6): cuando el mensaje nombra un fichero que sale en el diario
//! del proyecto (`diario`), el recall de ese mensaje añade una línea con cuánto se mueve y qué suele
//! cambiar con él, sacada de las sesiones de este equipo y no de lo que opine un modelo:
//!
//! «History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04); it usually changes together
//! with `tests/sync.rs` (9 of 14) and `CHANGELOG.md` (7); 3 gotchas mention it.»
//!
//! Solo con al menos `SESIONES_MIN` sesiones (con menos no dice nada), una vez por fichero y sesión
//! (tabla local `diary_hint`, aparte de `recall_log`: no es una memoria y no debe contar como
//! recibida) y como mucho `MAX_POR_MENSAJE` ficheros por mensaje. Los gotcha mismos ya llegan por
//! `recall::por_rutas`; aquí solo se cuentan.
//!
//! Coste: lo normal es que el mensaje no nombre ningún fichero, y eso se decide sin tocar la base
//! (`parece_nombrar`). Solo si lo parece se mira si hay diario y se leen sus líneas del proyecto.

use crate::memoria::{AHORA, Ambito};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};

/// Sesiones mínimas con el fichero para decir algo: con menos, la línea es ruido.
pub const SESIONES_MIN: usize = 3;
/// Un fichero «cambia a la vez» si coincide en al menos tantas sesiones…
pub const JUNTOS_MIN: usize = 3;
/// …y en al menos esta parte de las del fichero.
pub const JUNTOS_PARTE: f64 = 0.4;
/// Cuántos de los que cambian a la vez se nombran.
pub const JUNTOS_MAX: usize = 3;
/// De cuántos ficheros se habla en un mismo mensaje.
pub const MAX_POR_MENSAJE: usize = 2;
/// Cuántos días se recuerda que ya se dijo (tras compactar se olvida antes: `al_empezar`).
const DIAS_AVISO: i64 = 7;

/// La tabla con lo que ya se dijo en cada sesión. Local como `diary`, y como ella se crea con la
/// primera línea, no al abrir la base.
const TABLA: &str = "
CREATE TABLE IF NOT EXISTS diary_hint (
  session TEXT NOT NULL,
  file    TEXT NOT NULL,
  at      TEXT NOT NULL,
  PRIMARY KEY (session, file)
);
";

/// Si `ruta` sale en `texto` (ya en minúsculas y con `/`): la ruta entera, o el nombre del fichero si
/// tiene al menos 5 caracteres. El mismo criterio que `recall::por_rutas`.
pub fn nombra(texto: &str, ruta: &str) -> bool {
    let ruta = ruta.to_lowercase().replace('\\', "/");
    let nombre = ruta.rsplit('/').next().unwrap_or(&ruta);
    texto.contains(&ruta) || (nombre.chars().count() >= 5 && texto.contains(nombre))
}

/// Si el mensaje puede nombrar un fichero: alguna palabra con una barra o con un punto entre letras o
/// cifras (`sync.rs`, `src/x`, `a\b`). Sin mirar la base: así el mensaje normal no cuesta nada. Deja
/// fuera los nombres sin extensión escritos solos (`Makefile`); con su carpeta (`ci/Makefile`) entran.
pub fn parece_nombrar(prompt: &str) -> bool {
    let b = prompt.as_bytes();
    b.iter().enumerate().any(|(i, c)| match c {
        b'/' | b'\\' => true,
        b'.' => i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_alphanumeric() && b[i + 1].is_ascii_alphabetic(),
        _ => false,
    })
}

/// Al empezar o compactar la sesión: lo dicho antes ya no está en su contexto, se puede repetir. Y lo
/// de hace más de `DIAS_AVISO` días se borra.
pub fn al_empezar(db: &Connection, sesion: &str) -> Result<()> {
    if !hay_tabla(db)? {
        return Ok(());
    }
    db.execute(&format!("DELETE FROM diary_hint WHERE session = ?1 OR julianday({AHORA}) - julianday(at) > {DIAS_AVISO}"), [sesion])?;
    Ok(())
}

fn hay_tabla(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'diary_hint')", [], |f| f.get(0))?)
}

/// Lo que sabe el diario de un fichero.
#[derive(Debug, PartialEq)]
pub struct Historia {
    pub fichero: String,
    pub sesiones: usize,
    /// El último día (UTC) de la sesión más reciente que lo tocó.
    pub ultima: String,
    /// Los que cambian a la vez, con en cuántas de sus sesiones.
    pub juntos: Vec<(String, usize)>,
    pub gotchas: usize,
}

impl Historia {
    /// La línea para el agente.
    pub fn linea(&self) -> String {
        let mut s = format!("History of `{}`: changed in {} sessions (last {})", self.fichero, self.sesiones, self.ultima);
        if !self.juntos.is_empty() {
            let partes: Vec<String> = self
                .juntos
                .iter()
                .enumerate()
                .map(|(i, (f, n))| if i == 0 { format!("`{f}` ({n} of {})", self.sesiones) } else { format!("`{f}` ({n})") })
                .collect();
            let lista = match partes.as_slice() {
                [a] => a.clone(),
                [resto @ .., ultimo] => format!("{} and {ultimo}", resto.join(", ")),
                [] => String::new(),
            };
            s.push_str(&format!("; it usually changes together with {lista}"));
        }
        match self.gotchas {
            0 => {}
            1 => s.push_str("; 1 gotcha mentions it"),
            n => s.push_str(&format!("; {n} gotchas mention it")),
        }
        s.push('.');
        s
    }
}

/// Las líneas del diario que son de la carpeta `project`: las de su proyecto y las sin proyecto
/// apuntadas desde una de sus carpetas (o desde esta, si aún no es un proyecto o es una carpeta
/// general). Cada una, su último momento y sus ficheros.
fn sesiones(db: &Connection, project: &str) -> Result<Vec<(String, Vec<String>)>> {
    let general = crate::proyecto::es_general(project);
    let id = if general { 0 } else { crate::proyecto::resolver(db, project)?.map(|(id, _)| id).unwrap_or(0) };
    let mut carpetas: BTreeSet<String> = BTreeSet::from([crate::diario::carpeta_cmp(project)]);
    if id != 0 {
        let mut q = db.prepare("SELECT path FROM project_path WHERE project_id = ?1")?;
        for p in q.query_map([id], |f| f.get::<_, String>(0))? {
            carpetas.insert(crate::diario::carpeta_cmp(&p?));
        }
    }
    let mut q = db.prepare("SELECT project_id, folder, last_at, files FROM diary WHERE project_id IN (?1, 0)")?;
    let mut filas = q.query([id])?;
    let mut r = Vec::new();
    while let Some(f) = filas.next()? {
        let (pid, folder): (i64, String) = (f.get(0)?, f.get(1)?);
        if pid == 0 && !carpetas.contains(&crate::diario::carpeta_cmp(&folder)) {
            continue;
        }
        let files: Vec<String> = serde_json::from_str(&f.get::<_, String>(3)?).unwrap_or_default();
        if !files.is_empty() {
            r.push((f.get(2)?, files));
        }
    }
    Ok(r)
}

/// Si dos rutas son el mismo fichero: iguales (sin mirar mayúsculas ni barras), o una acaba en la otra
/// por una carpeta (`D:/repo/src/a.rs` y `src/a.rs`: las memorias guardan la ruta como la dio el agente).
fn misma(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase().replace('\\', "/"), b.to_lowercase().replace('\\', "/"));
    let (largo, corto) = if a.len() >= b.len() { (&a, &b) } else { (&b, &a) };
    largo == corto || (!corto.is_empty() && largo.ends_with(corto.as_str()) && largo.as_bytes()[largo.len() - corto.len() - 1] == b'/')
}

/// La historia de los ficheros que nombra el mensaje (los que pasan de `SESIONES_MIN`), sin mirar si
/// ya se dijo. Los nombrados con su ruta entera primero; de los nombrados solo por el nombre, por cada
/// nombre el que más se movió (`main.rs` no son todos los `main.rs` del repositorio).
pub fn historias(db: &Connection, project: &str, prompt: &str) -> Result<Vec<Historia>> {
    if project.is_empty() || !parece_nombrar(prompt) || !crate::diario::hay_tabla(db)? {
        return Ok(Vec::new());
    }
    let texto = prompt.to_lowercase().replace('\\', "/");
    let lineas = sesiones(db, project)?;
    // Cuántas sesiones y la última, por fichero nombrado (`None`: visto y no nombrado; se mira una vez).
    let mut vistos: HashMap<&str, Option<(usize, &str)>> = HashMap::new();
    for (ultima, files) in &lineas {
        for f in files {
            if let Some((n, u)) = vistos.entry(f.as_str()).or_insert_with(|| nombra(&texto, f).then_some((0, ""))) {
                *n += 1;
                *u = (*u).max(ultima.as_str());
            }
        }
    }
    let mut elegidos: Vec<(&str, usize, &str)> = vistos.into_iter().filter_map(|(f, x)| x.filter(|(n, _)| *n >= SESIONES_MIN).map(|(n, u)| (f, n, u))).collect();
    let entera = |f: &str| texto.contains(&f.to_lowercase());
    elegidos.sort_by(|a, b| entera(b.0).cmp(&entera(a.0)).then(b.1.cmp(&a.1)).then(a.0.cmp(b.0)));
    // Nombrado entero, el nombre ya es suyo: `src/recall.rs` no trae también `tests/recall.rs`.
    let nombre = |f: &str| f.to_lowercase().rsplit('/').next().unwrap_or("").to_string();
    let mut nombres: BTreeSet<String> = elegidos.iter().filter(|x| entera(x.0)).map(|x| nombre(x.0)).collect();
    elegidos.retain(|(f, _, _)| entera(f) || nombres.insert(nombre(f)));
    elegidos.truncate(MAX_POR_MENSAJE);
    if elegidos.is_empty() {
        return Ok(Vec::new());
    }
    // Los gotcha del ámbito con rutas: pocos.
    let mut q = db.prepare(&format!(
        "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id WHERE m.kind = 'gotcha' AND {}",
        Ambito::de(db, project)?.sql()
    ))?;
    let gotchas: Vec<(String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut r = Vec::new();
    for (fichero, n, ultima) in elegidos {
        let mut juntos: HashMap<&str, usize> = HashMap::new();
        for (_, files) in lineas.iter().filter(|(_, fs)| fs.iter().any(|f| f == fichero)) {
            for f in files.iter().filter(|f| *f != fichero) {
                *juntos.entry(f).or_default() += 1;
            }
        }
        let mut juntos: Vec<(String, usize)> =
            juntos.into_iter().filter(|(_, k)| *k >= JUNTOS_MIN && *k as f64 >= JUNTOS_PARTE * n as f64).map(|(f, k)| (f.to_string(), k)).collect();
        juntos.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        juntos.truncate(JUNTOS_MAX);
        let ids: BTreeSet<&str> = gotchas.iter().filter(|(_, p)| misma(p, fichero)).map(|(id, _)| id.as_str()).collect();
        r.push(Historia { fichero: fichero.to_string(), sesiones: n, ultima: ultima.chars().take(10).collect(), juntos, gotchas: ids.len() });
    }
    Ok(r)
}

/// Lo que el recall de este mensaje añade (una línea por fichero), o nada. Apunta lo dicho para no
/// repetirlo en la sesión.
pub fn al_preguntar(db: &Connection, project: &str, sesion: &str, prompt: &str) -> Result<Option<String>> {
    let mut hs = historias(db, project, prompt)?;
    if hs.is_empty() {
        return Ok(None);
    }
    if !sesion.is_empty() {
        if hay_tabla(db)? {
            let mut q = db.prepare("SELECT 1 FROM diary_hint WHERE session = ?1 AND file = ?2")?;
            let mut nuevas = Vec::new();
            for h in hs {
                if q.query_row(params![sesion, h.fichero], |_| Ok(())).optional()?.is_none() {
                    nuevas.push(h);
                }
            }
            hs = nuevas;
        }
        if hs.is_empty() {
            return Ok(None);
        }
        db.execute_batch(TABLA)?;
        for h in &hs {
            db.execute(&format!("INSERT OR IGNORE INTO diary_hint(session, file, at) VALUES (?1, ?2, {AHORA})"), params![sesion, h.fichero])?;
        }
    }
    Ok(Some(hs.iter().map(Historia::linea).collect::<Vec<_>>().join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parece_un_fichero() {
        assert!(parece_nombrar("mira sync.rs"));
        assert!(parece_nombrar("el fallo de src/sync"));
        assert!(parece_nombrar(r"en D:\repo"));
        assert!(!parece_nombrar("como ha ido todo? bien. 1.5 o 2"));
        assert!(!parece_nombrar("termina. Ahora sigue"));
        assert!(!parece_nombrar(""));
    }

    #[test]
    fn mismas_rutas() {
        assert!(misma("src/a.rs", "SRC/a.rs"));
        assert!(misma("D:/repo/src/a.rs", "src/a.rs"));
        assert!(misma(r"src\a.rs", "D:/repo/src/a.rs"));
        assert!(!misma("src/data.rs", "a.rs"));
        assert!(!misma("src/a.rs", "tests/a.rs"));
    }

    #[test]
    fn la_linea() {
        let mut h = Historia {
            fichero: "src/sync.rs".into(),
            sesiones: 14,
            ultima: "2026-11-04".into(),
            juntos: vec![("tests/sync.rs".into(), 9), ("CHANGELOG.md".into(), 7)],
            gotchas: 3,
        };
        assert_eq!(
            h.linea(),
            "History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04); it usually changes together with `tests/sync.rs` (9 of 14) and `CHANGELOG.md` (7); 3 gotchas mention it."
        );
        h.juntos.push(("docs/x.md".into(), 6));
        h.gotchas = 1;
        assert!(h.linea().contains("(9 of 14), `CHANGELOG.md` (7) and `docs/x.md` (6); 1 gotcha mentions it."));
        h.juntos.clear();
        h.gotchas = 0;
        assert_eq!(h.linea(), "History of `src/sync.rs`: changed in 14 sessions (last 2026-11-04).");
    }
}
