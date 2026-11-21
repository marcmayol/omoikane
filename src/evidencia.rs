//! En qué se basa una memoria (1.13, del registro de evidencias de trycompai/crm). El agente no puntúa
//! su propia certeza («un modelo que puntúa su certeza se equivoca hacia donde parece más útil»): dice
//! qué vio, con un tipo cerrado y un detalle que una persona pueda leer.
//!
//! | Tipo | Qué es | Detalle |
//! |---|---|---|
//! | `user-said` | Lo dijo el usuario | Sus palabras, citadas |
//! | `code-read` | Leído en el código | El fichero (y la línea) |
//! | `tested` | Comprobado al ejecutarlo | Qué se ejecutó y qué salió |
//! | `doc` | Leído en documentación o en una web | Qué dice; la URL en `url` |
//! | `inferred` | Lo dedujo el agente | De qué lo dedujo |
//!
//! Lo que se basa solo en `inferred` pesa menos al buscar ([`FACTOR_DEDUCIDA`]) y sale marcado
//! «inferred» en las listas: el agente que la recibe sabe que nadie la ha comprobado. Una memoria sin
//! evidencia (las de antes de la 1.13, lo importado) no se toca.
//!
//! No es la procedencia (`procedencia`: en qué sesión y en qué commit se escribió), sino la base del
//! dato. Va en `memory.extra` con la clave [`CLAVE`], como la procedencia: sin cambiar el esquema,
//! viaja con la sincronización y una versión anterior la conserva sin entenderla. Se añade con
//! `update` (no se pisa ni crea versión); dos detalles iguales del mismo tipo cuentan como uno.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// La clave en `memory.extra`.
pub const CLAVE: &str = "evidence";
/// Cuántas lleva una memoria como mucho (las primeras se quedan).
pub const MAXIMO: usize = 10;
/// El largo máximo de un detalle, en caracteres.
const MAX_DETALLE: usize = 400;
/// Lo que pesa al buscar una memoria basada solo en lo que dedujo el agente.
pub const FACTOR_DEDUCIDA: f64 = 0.85;

/// El tipo de evidencia: qué vio quien guarda.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Clase {
    /// The user said it (quote their words in `detail`).
    UserSaid,
    /// Read in the code (the file, and line, in `detail`).
    CodeRead,
    /// Checked by running or testing it (what ran and what came out).
    Tested,
    /// Read in documentation or on a web page (the URL in `url`).
    Doc,
    /// The agent worked it out (from what, in `detail`).
    Inferred,
}

impl Clase {
    pub fn as_str(self) -> &'static str {
        match self {
            Clase::UserSaid => "user-said",
            Clase::CodeRead => "code-read",
            Clase::Tested => "tested",
            Clase::Doc => "doc",
            Clase::Inferred => "inferred",
        }
    }

    pub fn de(t: &str) -> Result<Clase> {
        Ok(match t.trim().to_lowercase().replace('_', "-").as_str() {
            "user-said" | "user" | "said" => Clase::UserSaid,
            "code-read" | "code" => Clase::CodeRead,
            "tested" | "test" | "ran" => Clase::Tested,
            "doc" | "docs" | "web" => Clase::Doc,
            "inferred" | "inference" => Clase::Inferred,
            otro => bail!(crate::tr!(
                "unknown evidence kind «{otro}»: use user-said, code-read, tested, doc or inferred",
                "tipo de evidencia desconocido «{otro}»: usa user-said, code-read, tested, doc o inferred"
            )),
        })
    }
}

/// Una evidencia: qué se vio, en palabras de una persona.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Evidencia {
    /// What kind of evidence: user-said, code-read, tested, doc or inferred.
    pub kind: Clase,
    /// What you saw, readable by a person: the user's words, the file and line, the command and its output, what the page says. Not «confirmed».
    pub detail: String,
    /// The page or document, for `doc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Evidencia {
    /// En una línea: «tested: cargo test, 83 passed».
    pub fn texto(&self) -> String {
        match &self.url {
            Some(u) => format!("{}: {} ({u})", self.kind.as_str(), self.detail),
            None => format!("{}: {}", self.kind.as_str(), self.detail),
        }
    }
}

/// Limpia lo que llega (detalle sin espacios de más y recortado, URL solo si es http/https) y quita las
/// repetidas. Un detalle vacío es un error: una evidencia sin decir qué se vio no es evidencia.
pub fn limpiar(v: Vec<Evidencia>) -> Result<Vec<Evidencia>> {
    let mut r: Vec<Evidencia> = Vec::new();
    for mut e in v {
        e.detail = e.detail.split_whitespace().collect::<Vec<_>>().join(" ");
        if e.detail.is_empty() {
            bail!(crate::tr!(
                "evidence «{}» without detail: say what you saw (the user's words, the file, the command and its output…)",
                "evidencia «{}» sin detalle: di qué viste (las palabras del usuario, el fichero, la orden y lo que salió…)",
                e.kind.as_str()
            ));
        }
        // Una cita del usuario o la salida de una orden pueden llevar un secreto, como el contenido.
        if crate::secretos::activo() {
            e.detail = crate::secretos::redactar(&e.detail).0;
        }
        if e.detail.chars().count() > MAX_DETALLE {
            e.detail = e.detail.chars().take(MAX_DETALLE - 1).collect::<String>() + "…";
        }
        e.url = e.url.map(|u| u.trim().to_string()).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
        if !r.iter().any(|x| x.kind == e.kind && x.detail == e.detail) {
            r.push(e);
        }
    }
    Ok(r)
}

/// Las de una columna (el JSON de `extra.evidence`); lo que no se entienda, ninguna.
pub(crate) fn de_columna(json: Option<&str>) -> Vec<Evidencia> {
    json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
}

/// Si se basa solo en lo que dedujo el agente (sin evidencia no cuenta: no se sabe).
pub fn solo_deducida(v: &[Evidencia]) -> bool {
    !v.is_empty() && v.iter().all(|e| e.kind == Clase::Inferred)
}

/// Sus tipos, sin repetir y en orden, para las listas: «tested, user-said». `None` sin evidencia.
pub fn tipos(v: &[Evidencia]) -> Option<String> {
    let mut t: Vec<&str> = Vec::new();
    for e in v {
        if !t.contains(&e.kind.as_str()) {
            t.push(e.kind.as_str());
        }
    }
    (!t.is_empty()).then(|| t.join(", "))
}

/// Junta las nuevas con las que ya había en `extra` (las de antes primero, sin repetir, hasta
/// [`MAXIMO`]). `true` si ha cambiado algo.
pub(crate) fn anadir(extra: &mut serde_json::Map<String, serde_json::Value>, nuevas: &[Evidencia]) -> bool {
    let mut v: Vec<Evidencia> = extra.get(CLAVE).and_then(|x| serde_json::from_value(x.clone()).ok()).unwrap_or_default();
    let antes = v.len();
    for e in nuevas {
        if v.len() < MAXIMO && !v.iter().any(|x| x.kind == e.kind && x.detail == e.detail) {
            v.push(e.clone());
        }
    }
    if v.len() == antes {
        return false;
    }
    extra.insert(CLAVE.into(), serde_json::to_value(v).unwrap_or_default());
    true
}

/// `--evidence tipo:detalle` de la CLI (con `doc:https://…` la URL va en `url`).
pub fn de_cli(t: &str) -> Result<Evidencia> {
    let Some((k, d)) = t.split_once(':') else {
        bail!(crate::tr!("--evidence KIND:DETAIL, e.g. --evidence \"tested:cargo test, 83 passed\"", "--evidence TIPO:DETALLE, p. ej. --evidence \"tested:cargo test, 83 passed\""))
    };
    let kind = Clase::de(k)?;
    let d = d.trim();
    let (detail, url) = match kind {
        Clase::Doc if d.starts_with("http://") || d.starts_with("https://") => match d.split_once(char::is_whitespace) {
            Some((u, resto)) => (resto.trim().to_string(), Some(u.to_string())),
            None => (d.to_string(), Some(d.to_string())),
        },
        _ => (d.to_string(), None),
    };
    Ok(Evidencia { kind, detail, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(k: Clase, d: &str) -> Evidencia {
        Evidencia { kind: k, detail: d.into(), url: None }
    }

    #[test]
    fn limpia_y_quita_repetidas() {
        let v = limpiar(vec![ev(Clase::Tested, "  cargo   test "), ev(Clase::Tested, "cargo test"), ev(Clase::Inferred, "x")]).unwrap();
        assert_eq!(v, [ev(Clase::Tested, "cargo test"), ev(Clase::Inferred, "x")]);
        assert!(limpiar(vec![ev(Clase::Doc, "  ")]).is_err());
        let u = limpiar(vec![Evidencia { kind: Clase::Doc, detail: "d".into(), url: Some("javascript:x".into()) }]).unwrap();
        assert_eq!(u[0].url, None);
    }

    #[test]
    fn solo_deducida_sin_evidencia_no() {
        assert!(!solo_deducida(&[]));
        assert!(solo_deducida(&[ev(Clase::Inferred, "a")]));
        assert!(!solo_deducida(&[ev(Clase::Inferred, "a"), ev(Clase::UserSaid, "b")]));
    }

    #[test]
    fn la_cli() {
        assert_eq!(de_cli("tested: cargo test").unwrap(), ev(Clase::Tested, "cargo test"));
        let d = de_cli("doc:https://x.dev/a dice que sí").unwrap();
        assert_eq!((d.url.as_deref(), d.detail.as_str()), (Some("https://x.dev/a"), "dice que sí"));
        assert!(de_cli("guess:algo").is_err());
        assert!(de_cli("sin tipo").is_err());
    }

    #[test]
    fn anadir_sin_repetir_ni_pasar_del_maximo() {
        let mut extra = serde_json::Map::new();
        assert!(anadir(&mut extra, &[ev(Clase::Tested, "a")]));
        assert!(!anadir(&mut extra, &[ev(Clase::Tested, "a")]));
        let muchas: Vec<Evidencia> = (0..20).map(|i| ev(Clase::CodeRead, &format!("f{i}"))).collect();
        anadir(&mut extra, &muchas);
        assert_eq!(de_columna(Some(&extra[CLAVE].to_string())).len(), MAXIMO);
    }
}
