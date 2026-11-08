//! Verificada por una persona frente a escrita por un agente (1.8.5, de OKF Agent Memory: sus
//! niveles `generated`/`verified` con `verified.by`).
//!
//! Casi todo lo que hay en la memoria lo escribe un agente. Que una persona diga «esto es así» vale
//! más, y el agente que la recibe tiene que poder distinguirlo. La verificación es un sello en
//! `memory.extra` con la clave [`CLAVE`]: quién, cuándo y la huella del texto que vio (tipo, título y
//! contenido). Sin cambiar el esquema y con el mismo camino que los datos (`data`): viaja como un
//! campo `verified` de la línea, y una 1.0–1.8 lo guarda y lo reenvía sin entenderlo.
//!
//! **Un agente no puede verificarse a sí mismo.** Ninguna herramienta MCP lo hace ni lo lee de lo
//! que recibe (`remember`, `update` y `data` no llegan a esta clave, y `update` sobre el texto la
//! quita); el endpoint `/tools/` de `omoi serve` es el MCP, así que tampoco. Verifican la persona en
//! la CLI (`omoi verify`, que en una terminal pregunta y fuera de ella pide `--yes`, y que se niega
//! dentro de la shell de un agente) y en la vista web (un botón, `POST /api/verify`, que pide el
//! `Origin` de la vista y el token que `omoi serve` mete en la página). Además llegan sellos al
//! importar, nunca dentro de la shell de un agente: `omoi import --okf` (el `verified` de una persona
//! de un bundle firmado por esta base y con el mismo texto, o de otro con `--trust-verified`; ver
//! `okf`) y `omoi import <fichero>` (los sellos del fichero). La sincronización trae los de los otros
//! equipos de la persona, por su repositorio cifrado: son su confianza. Un agente con shell que
//! quiera saltárselo a propósito puede (como puede editar la base o cargar la página para leer el
//! token): esto no es una frontera de seguridad sino que ningún camino normal del agente acabe en
//! «verificada».
//!
//! **Cambiar el texto la des-verifica.** La persona verificó ese texto, no el que venga después: si
//! un agente lo cambia, la memoria vuelve a ser «escrita por un agente» hasta que alguien la mire
//! otra vez. Cambiar etiquetas, rutas, fijarla o moverla no la toca (no cambia lo que dice). Esta
//! versión quita el sello al cambiar el texto; y como el sello lleva la huella del texto, un cambio
//! hecho por una versión anterior (que conserva el sello sin entenderlo) también la deja sin
//! verificar: al leerla, la huella ya no coincide.

use crate::memoria::{AHORA, Memoria};
use crate::tr;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// La clave del sello en `memory.extra` (y el campo `verified` de la línea que viaja).
pub const CLAVE: &str = "verified";
/// Cuándo la persona le quitó la verificación por última vez (`memory.extra`), para que reimportar un
/// bundle viejo no la vuelva a verificar.
pub const DESVERIFICADA: &str = "unverified_at";

/// Lo que se enseña: quién la verificó y cuándo. Solo si la verificación sigue valiendo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verificada {
    pub by: String,
    pub at: String,
}

/// Lo que se guarda: lo anterior y la huella del texto verificado.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Sello {
    by: String,
    at: String,
    #[serde(default)]
    sha: String,
}

/// La huella del texto que se verifica: tipo, título y contenido (32 cifras hex).
pub(crate) fn huella(kind: &str, title: &str, content: &str) -> String {
    let mut h = Sha256::new();
    for parte in [kind, title, content] {
        h.update(parte.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// La verificación de su columna en `memoria::COLUMNAS` (el JSON del sello), si su huella es la del texto de ahora.
pub(crate) fn de_columna(json: Option<&str>, kind: &str, title: &str, content: &str) -> Option<Verificada> {
    let s: Sello = serde_json::from_str(json?).ok()?;
    (!s.by.trim().is_empty() && s.sha == huella(kind, title, content)).then_some(Verificada { by: s.by, at: s.at })
}

/// Quién verifica si no se dice (`--by`, o la vista web): el autor de este equipo para lo de esa
/// memoria (`OMOIKANE_AUTHOR` o el nombre con el que se unió a la sincronización), y si no hay, el
/// usuario del sistema.
pub fn quien_por_defecto(db: &Connection, m: &Memoria) -> Result<String> {
    let a = crate::memoria::autor_en(db, &m.space)?;
    if !a.trim().is_empty() {
        return Ok(a);
    }
    Ok(["USERNAME", "USER"].iter().filter_map(|v| std::env::var(v).ok()).map(|v| v.trim().to_string()).find(|v| !v.is_empty()).unwrap_or_default())
}

/// Las variables que pone la shell de un agente de programación: dentro de ella, `omoi verify` se
/// niega (la verificación es de la persona). Es un freno para que un agente no la use sin querer o
/// porque una memoria se lo pida, no una barrera contra quien la quiera saltar.
const DE_AGENTE: &[&str] = &["CLAUDECODE", "AI_AGENT", "GEMINI_CLI", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED", "CURSOR_AGENT"];

/// La variable de agente que hay en el entorno, si hay alguna.
pub fn shell_de_agente() -> Option<&'static str> {
    DE_AGENTE.iter().copied().find(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

/// Marca la memoria `id` (entero) como verificada por `by`. No crea versión ni cambia su fecha (como
/// fijarla); cambia su sello para que la sincronización lo lleve. Devuelve la memoria.
pub fn verificar(db: &mut Connection, id: &str, by: &str) -> Result<Memoria> {
    Ok(verificar_en(db, id, by, None)?.0)
}

/// Como [`verificar`], con la fecha en que se verificó si se sabe (un instante ISO 8601, el `at` de un
/// `verified` de OKF al importarlo; si no se entiende, ahora). Si ya tiene ese mismo sello y vale, no
/// lo vuelve a poner (no cambia su sello de tiempo): devuelve la memoria y si lo ha puesto.
pub fn verificar_en(db: &mut Connection, id: &str, by: &str, at: Option<&str>) -> Result<(Memoria, bool)> {
    let by = by.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!by.is_empty(), tr!("who verifies it? Give --by <name>", "¿quién la verifica? Di --by <nombre>"));
    ensure!(by.chars().count() <= 100, tr!("--by: up to 100 characters", "--by: hasta 100 caracteres"));
    let tx = crate::memoria::escribir_tx(db)?;
    let (kind, title, content): (String, String, String) =
        tx.query_row("SELECT kind, title, content FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    // La fecha dada, en la forma de las de Omoikane (`…T…:…:….mmmZ`, en UTC); si no se entiende, ahora.
    let dada: Option<String> = match at.map(|a| a.trim().trim_end_matches('Z').replace('T', " ")).filter(|a| !a.is_empty()) {
        Some(a) => tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1)", [a], |f| f.get(0))?,
        None => None,
    };
    // Una fecha de más de un día en el futuro no es de cuando se verificó (un reloj mal puesto, o un
    // bundle que quiere quedar por delante de cualquier cambio): no se acepta.
    if let Some(a) = &dada {
        let futura: bool = tx.query_row("SELECT ?1 > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day')", [a], |f| f.get(0))?;
        ensure!(!futura, tr!("the verification date {a} is in the future", "la fecha de verificación {a} es del futuro"));
    }
    let at = match dada {
        Some(a) => a,
        None => tx.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?,
    };
    let antes = crate::memoria::leer(&tx, id)?;
    if antes.verified.as_ref().is_some_and(|v| v.by == by && v.at == at) {
        return Ok((antes, false));
    }
    let sello = serde_json::to_value(Sello { by, at, sha: huella(&kind, &title, &content) })?;
    poner(&tx, id, Some(sello))?;
    let m = crate::memoria::leer(&tx, id)?;
    tx.commit()?;
    Ok((m, true))
}

/// Le quita la verificación y apunta cuándo ([`DESVERIFICADA`]): reimportar un bundle exportado antes
/// no la vuelve a verificar. `false` si no tenía.
pub fn desverificar(db: &mut Connection, id: &str) -> Result<bool> {
    let tx = crate::memoria::escribir_tx(db)?;
    let hay = poner(&tx, id, None)?;
    if hay {
        let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
        let ahora: String = tx.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?;
        o.insert(DESVERIFICADA.into(), ahora.into());
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    }
    tx.commit()?;
    Ok(hay)
}

/// Cuándo le quitó la persona la verificación por última vez ([`desverificar`]), si se sabe.
pub(crate) fn desverificada_en(db: &Connection, id: &str) -> Result<Option<String>> {
    let extra: Option<String> = db.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    Ok(extra
        .filter(|e| e.contains(DESVERIFICADA))
        .and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok())
        .and_then(|v| v.get(DESVERIFICADA).and_then(|x| x.as_str()).map(str::to_string)))
}

/// Pone o quita el sello con un sello de tiempo nuevo. `false` si no había nada que quitar.
fn poner(tx: &Transaction, id: &str, sello: Option<serde_json::Value>) -> Result<bool> {
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    let habia = match sello {
        Some(s) => {
            o.insert(CLAVE.into(), s);
            true
        }
        None => o.remove(CLAVE).is_some(),
    };
    if !habia {
        return Ok(false);
    }
    let hlc = crate::reloj::sello_tras(tx, &crate::memoria::hlc_de(tx, id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    Ok(true)
}

/// Al cambiar el texto desde aquí (`memoria::actualizar_en`, que ya pone el sello de tiempo): quita la
/// verificación y devuelve la que había, si valía.
pub(crate) fn quitar_al_cambiar(tx: &Transaction, id: &str, antes: &Memoria) -> Result<Option<Verificada>> {
    let extra: Option<String> = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    let Some(extra) = extra.filter(|e| e.contains("\"verified\"")) else { return Ok(None) };
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.remove(CLAVE).is_none() {
        return Ok(None);
    }
    tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    Ok(antes.verified.clone())
}

/// Lo que se enseña en una línea: «verified by Marc, 2026-11-08».
pub fn texto(v: &Verificada) -> String {
    format!("verified by {}, {}", v.by, v.at.get(..10).unwrap_or(&v.at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_huella_cambia_con_el_texto() {
        let a = huella("decision", "T", "C");
        assert_eq!(a.len(), 32);
        assert_ne!(a, huella("fact", "T", "C"));
        assert_ne!(a, huella("decision", "T", "C."));
        // El separador: «ab»+«c» no es «a»+«bc».
        assert_ne!(huella("x", "ab", "c"), huella("x", "a", "bc"));
        let s = serde_json::json!({ "by": "Marc", "at": "2026-11-08T10:00:00.000Z", "sha": a }).to_string();
        assert_eq!(de_columna(Some(&s), "decision", "T", "C").unwrap().by, "Marc");
        assert!(de_columna(Some(&s), "decision", "T", "otro").is_none());
        assert!(de_columna(Some(r#"{"by":"Marc","at":"x"}"#), "decision", "T", "C").is_none());
        assert!(de_columna(None, "decision", "T", "C").is_none());
    }
}
