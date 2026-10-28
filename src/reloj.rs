//! Reloj lógico híbrido (HLC): el orden de los cambios entre equipos sin fiarse de la hora de cada uno.
//!
//! Un sello es `<milisegundos>-<contador>-<equipo>`, con los números con ceros delante, así que dos
//! sellos se comparan como texto. Cada cambio local toma un sello mayor que todos los que este equipo
//! ha visto (los suyos y los que llegaron de otros): si el reloj de un equipo va atrasado, sus cambios
//! siguen ganando a lo que ya conocía, y el contador desempata dentro del mismo milisegundo. A igual
//! tiempo y contador decide el nombre del equipo, así que todos eligen la misma versión.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// El nombre de este equipo: el del sistema y un sufijo fijo, guardado en la base.
pub fn equipo(db: &Connection) -> Result<String> {
    if let Some(e) = db.query_row("SELECT v FROM meta WHERE k = 'equipo'", [], |f| f.get::<_, String>(0)).optional()? {
        return Ok(e);
    }
    let sistema = std::env::var("OMOIKANE_EQUIPO")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_else(|| "equipo".into());
    let limpio: String = sistema.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let sufijo = ulid::Ulid::generate().to_string()[20..].to_lowercase();
    let limpio = limpio.trim_matches('-');
    let e = format!("{}-{sufijo}", if limpio.is_empty() { "equipo" } else { limpio });
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo', ?1)", [&e])?;
    Ok(e)
}

fn ahora_ms() -> u64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    // Para probar relojes desfasados.
    let desfase: i64 = std::env::var("OMOIKANE_RELOJ_DESFASE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    (ms + desfase).max(0) as u64
}

/// Milisegundos y contador de un sello (0, 0 si está vacío o mal formado).
pub fn partes(sello: &str) -> (u64, u32) {
    let mut p = sello.splitn(3, '-');
    let ms = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let c = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (ms, c)
}

pub fn formato(ms: u64, contador: u32, equipo: &str) -> String {
    format!("{ms:013}-{contador:04}-{equipo}")
}

fn ultimo(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Un sello nuevo para un cambio de este equipo (dentro de la transacción del cambio).
pub fn sello(db: &Connection) -> Result<String> {
    let equipo = equipo(db)?;
    let (ms_ult, c_ult) = partes(&ultimo(db)?);
    let ahora = ahora_ms();
    let (ms, c) = if ahora > ms_ult { (ahora, 0) } else { (ms_ult, c_ult + 1) };
    let s = formato(ms, c, &equipo);
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![s])?;
    Ok(s)
}

/// Anota un sello que llegó de otro equipo: los cambios de aquí irán después.
pub fn recibir(db: &Connection, remoto: &str) -> Result<()> {
    let (ms_r, c_r) = partes(remoto);
    let (ms_u, c_u) = partes(&ultimo(db)?);
    if (ms_r, c_r) > (ms_u, c_u) {
        let equipo = equipo(db)?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![formato(ms_r, c_r, &equipo)])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn se_comparan_como_texto() {
        assert!(formato(5, 0, "b") < formato(5, 1, "a"));
        assert!(formato(5, 9, "z") < formato(6, 0, "a"));
        assert!(formato(5, 1, "a") < formato(5, 1, "b"), "a igual tiempo decide el equipo");
        assert_eq!(partes(&formato(1234, 7, "pc-x")), (1234, 7));
        assert_eq!(partes(""), (0, 0));
    }
}
