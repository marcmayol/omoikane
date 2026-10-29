//! Reloj lógico híbrido (HLC): el orden de los cambios entre equipos sin fiarse de la hora de cada uno.
//!
//! Un sello es `<milisegundos>-<contador>-<equipo>`. Dos sellos se comparan por sus partes (tiempo,
//! contador y equipo), nunca como texto: así los sellos viejos (contador de 4 cifras) y los nuevos (de
//! 10) se ordenan bien. Cada cambio local toma un sello mayor que todos los que este equipo ha visto
//! (los suyos y los que llegaron de otros) y que el de la memoria que cambia: si el reloj de un equipo
//! va atrasado, sus cambios siguen ganando a lo que ya conocía, y el contador desempata dentro del
//! mismo milisegundo. A igual tiempo y contador decide el nombre del equipo, así que todos eligen la
//! misma versión. Un sello de otro equipo que viene de más de una hora en el futuro no adelanta el
//! reloj de aquí (un equipo con la hora mal no deja a todos los demás atascados en su milisegundo).

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::cmp::Ordering;

/// Lo que un sello remoto puede ir por delante de la hora de aquí para adelantar el reloj.
const FUTURO_MAX_MS: u64 = 3_600_000;

/// El nombre del sistema (para el nombre del equipo).
fn sistema() -> String {
    std::env::var("OMOIKANE_EQUIPO")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "equipo".into())
}

/// El nombre de este equipo: el del sistema y un sufijo fijo, guardado en la base. Si la base se copió
/// a otro equipo (el sistema ya no es el que se apuntó), se genera un nombre nuevo: dos equipos con
/// el mismo nombre escribirían el mismo fichero.
pub fn equipo(db: &Connection) -> Result<String> {
    let host = sistema().trim().to_lowercase();
    let guardado: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'equipo'", [], |f| f.get(0)).optional()?;
    let host_guardado: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'equipo_host'", [], |f| f.get(0)).optional()?;
    if let Some(e) = guardado {
        match host_guardado {
            Some(h) if h == host => return Ok(e),
            // Bases de antes de apuntar el sistema: se apunta ahora y se sigue con el nombre.
            None => {
                db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo_host', ?1)", [&host])?;
                return Ok(e);
            }
            Some(_) => {}
        }
    }
    let limpio: String = host.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let sufijo = ulid::Ulid::generate().to_string()[20..].to_lowercase();
    let limpio = limpio.trim_matches('-');
    let e = format!("{}-{sufijo}", if limpio.is_empty() { "equipo" } else { limpio });
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo', ?1)", [&e])?;
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo_host', ?1)", [&host])?;
    Ok(e)
}

fn ahora_ms() -> u64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    // Para probar relojes desfasados.
    let desfase: i64 = std::env::var("OMOIKANE_RELOJ_DESFASE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    (ms + desfase).max(0) as u64
}

/// Milisegundos, contador y equipo de un sello (0, 0, "" si está vacío o mal formado).
pub fn partes(sello: &str) -> (u64, u64, &str) {
    let mut p = sello.splitn(3, '-');
    let ms = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let c = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (ms, c, p.next().unwrap_or(""))
}

/// Compara dos sellos por sus partes.
pub fn comparar(a: &str, b: &str) -> Ordering {
    partes(a).cmp(&partes(b))
}

/// `a` es posterior a `b`.
pub fn mayor(a: &str, b: &str) -> bool {
    comparar(a, b) == Ordering::Greater
}

pub fn formato(ms: u64, contador: u64, equipo: &str) -> String {
    format!("{ms:013}-{contador:010}-{equipo}")
}

fn ultimo(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Un sello nuevo para un cambio de este equipo (dentro de la transacción del cambio).
pub fn sello(db: &Connection) -> Result<String> {
    sello_tras(db, "")
}

/// Un sello nuevo, también posterior a `previo` (el sello de la memoria que se cambia u olvida): un
/// cambio hecho aquí después de ver una versión siempre le gana, aunque esa versión venga de un equipo
/// con la hora adelantada.
pub fn sello_tras(db: &Connection, previo: &str) -> Result<String> {
    let equipo = equipo(db)?;
    let u = ultimo(db)?;
    let (mut ms, mut c, _) = partes(&u);
    let (ms_p, c_p, _) = partes(previo);
    if (ms_p, c_p) > (ms, c) {
        (ms, c) = (ms_p, c_p);
    }
    let ahora = ahora_ms();
    let (ms, c) = if ahora > ms { (ahora, 0) } else { (ms, c + 1) };
    let s = formato(ms, c, &equipo);
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![s])?;
    Ok(s)
}

/// Anota un sello que llegó de otro equipo: los cambios de aquí irán después. Uno de más de una hora en
/// el futuro no se anota (devuelve `false`).
pub fn recibir(db: &Connection, remoto: &str) -> Result<bool> {
    let (ms_r, c_r, _) = partes(remoto);
    if ms_r > ahora_ms().saturating_add(FUTURO_MAX_MS) {
        return Ok(false);
    }
    let (ms_u, c_u, _) = partes(&ultimo(db)?);
    if (ms_r, c_r) > (ms_u, c_u) {
        let equipo = equipo(db)?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![formato(ms_r, c_r, &equipo)])?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn se_comparan_por_partes() {
        assert!(mayor(&formato(5, 1, "a"), &formato(5, 0, "b")));
        assert!(mayor(&formato(6, 0, "a"), &formato(5, 9, "z")));
        assert!(mayor(&formato(5, 1, "b"), &formato(5, 1, "a")), "a igual tiempo decide el equipo");
        assert_eq!(partes(&formato(1234, 7, "pc-x")), (1234, 7, "pc-x"));
        assert_eq!(partes(""), (0, 0, ""));
        // Un sello viejo (contador de 4 cifras) frente a uno nuevo (de 10): manda el número.
        assert!(mayor(&formato(5, 10_000, "a"), "0000000000005-9999-a"));
        assert!(mayor("0000000000005-0002-a", &formato(5, 1, "a")));
        assert!(!mayor("0000000000005-0001-a", &formato(5, 1, "a")));
    }

    #[test]
    fn el_futuro_no_adelanta_el_reloj() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);").unwrap();
        let lejos = formato(ahora_ms() + 10 * FUTURO_MAX_MS, 0, "otro");
        assert!(!recibir(&db, &lejos).unwrap());
        let s = sello(&db).unwrap();
        assert!(partes(&s).0 <= ahora_ms() + 1000, "el reloj sigue en la hora de aquí: {s}");
        // Pero un cambio sobre una memoria con ese sello le gana igual.
        assert!(mayor(&sello_tras(&db, &lejos).unwrap(), &lejos));
        // Uno cercano sí se anota.
        let cerca = formato(ahora_ms() + 60_000, 3, "otro");
        assert!(recibir(&db, &cerca).unwrap());
        assert!(mayor(&sello(&db).unwrap(), &cerca));
    }

    #[test]
    fn base_copiada_a_otro_equipo_cambia_de_nombre() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);").unwrap();
        let a = equipo(&db).unwrap();
        assert_eq!(equipo(&db).unwrap(), a);
        db.execute("UPDATE meta SET v = 'otro-sistema' WHERE k = 'equipo_host'", []).unwrap();
        let b = equipo(&db).unwrap();
        assert_ne!(a, b);
        assert_eq!(equipo(&db).unwrap(), b);
    }
}
