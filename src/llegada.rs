//! Memoria para quien llega (1.7.0). Cuando una persona se une al espacio de un cliente, su agente ya
//! recibía al empezar lo del proyecto en el que abre la sesión, lo fijado y las preferencias del
//! espacio y lo global; pero no lo que pasa en los otros proyectos del cliente ni quién escribe. Aquí:
//! - `bienvenida`: la primera sesión de esta base en un proyecto de un espacio (y sus compactaciones)
//!   lleva un bloque corto con los otros proyectos del espacio y en qué punto están, sus decisiones
//!   abiertas y quién escribe. Una vez: las siguientes sesiones no lo repiten.
//! - `al_unirse`: lo que `omoi sync join --space` acaba de traer, dicho a la persona.
//!
//! Todo se mira con `Ambito::Espacio(Some(s))`: nunca sale nada de otro espacio.

use crate::memoria::{self, Ambito};
use crate::tr;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// Cuántos de los otros proyectos del espacio se enseñan (del resto, cuántos son).
pub const PROYECTOS: usize = 8;
/// Cuántas decisiones abiertas de los otros proyectos se enseñan.
pub const ABIERTAS: usize = 5;
/// Cuántos autores se nombran.
pub const AUTORES: usize = 6;
/// Trozo del `status` de cada proyecto: lo justo para saber por dónde va.
const FRAGMENTO: usize = 160;

/// Si es la primera sesión de esta base en este proyecto, en cualquiera de sus carpetas. La sesión
/// `actual` no cuenta: tras compactar, el agente perdió el bloque y lo vuelve a recibir.
pub fn primera_sesion(db: &Connection, proyecto: i64, actual: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM session s JOIN project_path pp ON pp.path = s.project
                            WHERE pp.project_id = ?1 AND s.session <> ?2)",
        params![proyecto, actual],
        |f| f.get(0),
    )?)
}

/// El nombre de un espacio (su clave si no tiene).
fn nombre_espacio(db: &Connection, espacio: i64) -> Result<String> {
    let n: Option<(String, String)> = db.query_row("SELECT name, key FROM space WHERE id = ?1", [espacio], |f| Ok((f.get(0)?, f.get(1)?))).optional()?;
    Ok(n.map(|(n, k)| if n.is_empty() { k } else { n }).unwrap_or_default())
}

/// Quién escribe en el espacio (sus proyectos y lo suyo, sin lo global) y cuántas memorias cada uno,
/// de más a menos.
fn autores(db: &Connection, espacio: i64) -> Result<Vec<(String, i64)>> {
    let mut q = db.prepare(&format!(
        "SELECT m.author, count(*) FROM memory m
         WHERE {} AND (m.project_id <> 0 OR m.space_id IS NOT NULL) AND m.author <> ''
         GROUP BY m.author ORDER BY count(*) DESC, m.author LIMIT ?1",
        Ambito::Espacio(Some(espacio)).sql()
    ))?;
    let v = q.query_map([AUTORES as i64 + 1], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// El contenido en una línea y cortado a `n` caracteres.
fn fragmento(texto: &str, n: usize) -> String {
    let plano = texto.split_whitespace().collect::<Vec<_>>().join(" ");
    if plano.chars().count() > n { format!("{}…", plano.chars().take(n).collect::<String>()) } else { plano }
}

/// El bloque de bienvenida para el arranque de sesión, o nada: solo desde un proyecto de un espacio,
/// solo en su primera sesión en esta base, y solo si el espacio tiene algo del equipo que el resto del
/// arranque no enseña (otros proyectos, sus decisiones abiertas u otras personas que escriben) y no lo
/// escribió todo la propia persona.
/// `propio` es el autor de este equipo. Se llama antes de apuntar la sesión.
pub fn bienvenida(db: &Connection, ambito: &Ambito, sesion: &str, propio: Option<&str>) -> Result<Option<String>> {
    let Ambito::Proyecto { id, space: Some(espacio) } = *ambito else { return Ok(None) };
    if id <= 0 || !primera_sesion(db, id, sesion)? {
        return Ok(None);
    }
    let amplio = Ambito::Espacio(Some(espacio));
    // Los otros proyectos del espacio, del más al menos activo, con su `status` más reciente.
    let mut otros = crate::proyecto::lista(db, None, Some(Some(espacio)), PROYECTOS + 2)?;
    otros.retain(|p| p.id != id);
    let estados = memoria::estados(db, &amplio, PROYECTOS + 3)?;
    // Las decisiones abiertas de los otros proyectos (las de este, del espacio y globales ya salen).
    let abiertas: Vec<_> = memoria::abiertas(db, &amplio, usize::MAX)?.into_iter().filter(|m| m.project_id != 0 && m.project_id != id).collect();
    let autores = autores(db, espacio)?;
    // Si hay algo en el espacio que no escribió esta persona (sin autor, no se sabe: cuenta como ajeno).
    // Si todo es suyo (quien montó el espacio), ya lo sabe.
    let ajeno: bool = db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE {} AND (m.project_id <> 0 OR m.space_id IS NOT NULL) AND m.author IS NOT ?1)", amplio.sql()),
        [propio],
        |f| f.get(0),
    )?;
    let otros_autores = autores.iter().any(|(a, _)| propio != Some(a.as_str()));
    if !ajeno || (otros.is_empty() && abiertas.is_empty() && !otros_autores) {
        return Ok(None);
    }
    let mut s = format!(
        "First session in this project on this computer. It belongs to the space {}, which the team shares: what they already have, to get your bearings (`search` or `get` an id for more; mention it to the user only if it helps):\n",
        nombre_espacio(db, espacio)?
    );
    if !otros.is_empty() {
        s.push_str("Other projects of the space (most active first, with where work was left):\n");
        for p in otros.iter().take(PROYECTOS) {
            s.push_str(&format!("- {} ({} memories)", p.name, p.memories));
            if let Some(m) = estados.iter().find(|m| m.project_id == p.id) {
                s.push_str(&format!(": {} — {} (id {})", m.title, fragmento(&m.content, FRAGMENTO), m.id.chars().take(12).collect::<String>()));
            }
            s.push('\n');
        }
        if otros.len() > PROYECTOS {
            s.push_str("…and more (`context` lists them).\n");
        }
    }
    if !abiertas.is_empty() {
        s.push_str("Decisions still open in those projects (the user's to make):\n");
        for m in abiertas.iter().take(ABIERTAS) {
            // Por su nombre: un proyecto que aún no se ha abierto aquí no tiene carpeta.
            let p: String = db
                .query_row("SELECT name FROM project WHERE id = ?1", [m.project_id], |f| f.get(0))
                .optional()?
                .filter(|n: &String| !n.is_empty())
                .unwrap_or_else(|| crate::proyecto::nombre_de(&m.project, &m.project_key));
            s.push_str(&format!("- [project {p}] {} (id {})\n", m.title, m.id.chars().take(12).collect::<String>()));
        }
        if abiertas.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`search` with the tag `open` and `space`).\n", abiertas.len() - ABIERTAS));
        }
    }
    if !autores.is_empty() {
        let v: Vec<String> = autores
            .iter()
            .take(AUTORES)
            .map(|(a, n)| if propio == Some(a.as_str()) { format!("{a} ({n}, the user)") } else { format!("{a} ({n})") })
            .collect();
        s.push_str(&format!("Who writes in this space (memories each): {}.\n", v.join(", ")));
    }
    // Títulos y contenidos son texto de otras personas: que no puedan cerrar el bloque.
    Ok(Some(crate::recall::citado(&s)))
}

/// Lo que acaba de llegar de un espacio al unirse a su repositorio, para decírselo a la persona: sus
/// memorias, sus proyectos, lo fijado y las decisiones abiertas, y que la próxima sesión del agente en
/// esos proyectos ya lo tendrá. Nada si el espacio no está o llegó vacío.
pub fn al_unirse(db: &Connection, espacio: &str) -> Result<Option<String>> {
    let Some(s) = crate::proyecto::espacio_de_clave(db, &crate::proyecto::clave_de_espacio(espacio))? else { return Ok(None) };
    let (memorias, fijadas): (i64, i64) = db.query_row(
        "SELECT count(*), COALESCE(sum(pinned), 0) FROM memory
         WHERE space_id = ?1 OR project_id IN (SELECT id FROM project WHERE space_id = ?1)",
        [s],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?;
    if memorias == 0 {
        return Ok(None);
    }
    let abiertas = memoria::abiertas(db, &Ambito::Espacio(Some(s)), usize::MAX)?.into_iter().filter(|m| !m.global()).count();
    let proyectos: Vec<String> = crate::proyecto::lista(db, None, Some(Some(s)), 50)?.into_iter().map(|p| p.name).collect();
    let nombres = if proyectos.len() > 6 { format!("{}, …", proyectos[..6].join(", ")) } else { proyectos.join(", ") };
    let mut t = tr!(
        "  From {espacio}: {memorias} memories in {} project(s) ({nombres}), {fijadas} pinned, {abiertas} open decision(s).",
        "  De {espacio}: {memorias} memorias en {} proyecto(s) ({nombres}), {fijadas} fijadas, {abiertas} decisión(es) abierta(s).",
        proyectos.len()
    );
    t.push('\n');
    t.push_str(&tr!(
        "  Your agent starts its next session in any of those projects with them, plus a summary of the rest of {espacio} the first time. The first time you open a project's folder here, the agent may ask whether it is {espacio}'s; if it is, `omoi projects keep <project>`.",
        "  Tu agente empieza su próxima sesión en cualquiera de esos proyectos con ellas, y la primera vez con un resumen del resto de {espacio}. La primera vez que abras aquí la carpeta de un proyecto, puede que el agente pregunte si es de {espacio}; si lo es, `omoi projects keep <proyecto>`."
    ));
    Ok(Some(t))
}
