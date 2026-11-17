//! Un vocabulario de etiquetas cerrado (1.10.1). Los agentes inventan variantes de la misma etiqueta
//! (`idea` e `ideas`, `test` y `tests`, `Release` y `release`, `a-b` y `a_b`) y cada una parte la
//! búsqueda por etiquetas en dos. Al guardar una memoria nueva (`remember`, por MCP o CLI), cada
//! etiqueta que no existe aún pero es una variante de una que ya está en el ámbito de la memoria se
//! cambia por esa, y la respuesta lo dice. Nada más: lo importado o sincronizado entra tal cual, y una
//! etiqueta del sistema (`open`, `blocking`, `status`, `draft`, las que llevan `:`) no se cambia ni se
//! toma como destino.

use crate::memoria::Ambito;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

/// Las etiquetas con significado propio para Omoikane: ni se cambian ni otra se cambia por ellas.
const DEL_SISTEMA: [&str; 8] = [
    crate::memoria::ESTADO,
    crate::memoria::ABIERTA,
    crate::memoria::BLOQUEANTE,
    "draft",
    "deprecated",
    "verified",
    "pinned",
    "review-after",
];

fn del_sistema(t: &str) -> bool {
    t.contains(':') || DEL_SISTEMA.contains(&t)
}

/// La base de una etiqueta: en minúsculas, sin acentos y sin separadores (`-`, `_`, espacios, `.`).
fn base(t: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    t.to_lowercase().nfkd().filter(|c| c.is_alphanumeric()).filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect()
}

/// Las formas con las que una etiqueta puede coincidir con otra: su base, y sin el plural (`-s`; `-es`
/// tras n, r, l, d, z, j o y, como «versiones» → «version»). Ninguna de menos de 3 letras.
fn formas(t: &str) -> Vec<String> {
    let b = base(t);
    let mut v = vec![b.clone()];
    if let Some(s) = b.strip_suffix('s') {
        v.push(s.to_string());
        if let Some(es) = s.strip_suffix('e')
            && es.ends_with(['n', 'r', 'l', 'd', 'z', 'j', 'y'])
        {
            v.push(es.to_string());
        }
    }
    v.retain(|f| f.chars().count() >= 3);
    v
}

/// Si dos etiquetas son variantes una de otra.
pub fn variantes(a: &str, b: &str) -> bool {
    let fb = formas(b);
    formas(a).iter().any(|f| fb.contains(f))
}

/// Un cambio hecho: la etiqueta pedida y la que ya existía y queda en su lugar.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Cambio {
    pub from: String,
    pub to: String,
}

/// Dónde se mira el vocabulario de una memoria que va al proyecto de la carpeta `project` (vacía: sin
/// proyecto) o al espacio `space_id`: lo que se ve desde ese proyecto; de un espacio, lo del espacio;
/// global, lo que no es de ningún espacio (nunca las etiquetas de un cliente).
pub fn ambito_de(db: &Connection, project: &str, space_id: Option<i64>) -> Result<Ambito> {
    Ok(match space_id {
        _ if !project.is_empty() => Ambito::de(db, project)?,
        Some(s) => Ambito::Espacio(Some(s)),
        None => Ambito::Espacio(None),
    })
}

/// Cuántas memorias del ámbito llevan cada etiqueta.
fn usadas(db: &Connection, ambito: &Ambito) -> Result<HashMap<String, usize>> {
    let mut q = db.prepare(&format!("SELECT j.value FROM memory m, json_each(m.tags) j WHERE {}", ambito.sql()))?;
    let mut n = HashMap::new();
    for t in q.query_map([], |f| f.get::<_, String>(0))? {
        *n.entry(t?).or_insert(0usize) += 1;
    }
    Ok(n)
}

/// Las etiquetas de una memoria nueva con cada variante cambiada por la etiqueta que ya existe en
/// `ambito` (la más usada, si son varias), sin repetidas, y los cambios hechos.
pub fn cerrar(db: &Connection, ambito: &Ambito, tags: Vec<String>) -> Result<(Vec<String>, Vec<Cambio>)> {
    if tags.iter().all(|t| del_sistema(t)) {
        return Ok((tags, Vec::new()));
    }
    let usadas = usadas(db, ambito)?;
    Ok(cerrar_con(&usadas, tags))
}

fn cerrar_con(usadas: &HashMap<String, usize>, tags: Vec<String>) -> (Vec<String>, Vec<Cambio>) {
    let mut fuera = Vec::new();
    let mut cambios = Vec::new();
    for t in tags {
        let nueva = if del_sistema(&t) || usadas.contains_key(&t) {
            t
        } else {
            let mejor = usadas
                .iter()
                .filter(|(e, _)| !del_sistema(e) && variantes(&t, e))
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(e, _)| e.clone());
            match mejor {
                Some(e) => {
                    cambios.push(Cambio { from: t, to: e.clone() });
                    e
                }
                None => t,
            }
        };
        if !fuera.contains(&nueva) {
            fuera.push(nueva);
        }
    }
    (fuera, cambios)
}

/// Las parejas de etiquetas variantes que ya hay en el ámbito (la más usada primero), para la revisión.
pub fn parejas(db: &Connection, ambito: &Ambito) -> Result<Vec<(String, usize, String, usize)>> {
    let usadas = usadas(db, ambito)?;
    let mut v: Vec<(&String, &usize)> = usadas.iter().filter(|(t, _)| !del_sistema(t)).collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut fuera = Vec::new();
    for (i, (a, na)) in v.iter().enumerate() {
        for (b, nb) in &v[i + 1..] {
            if variantes(a, b) {
                fuera.push(((*a).clone(), **na, (*b).clone(), **nb));
            }
        }
    }
    Ok(fuera)
}

/// Cambia la etiqueta `de` por `a` en todas las memorias del ámbito que la llevan, cada una con su
/// versión en el historial. Devuelve cuántas cambiaron. Las del sistema no se unen.
pub fn unir(db: &mut Connection, ambito: &Ambito, de: &str, a: &str) -> Result<usize> {
    let (de, a) = (de.trim().trim_start_matches('#').to_lowercase(), a.trim().trim_start_matches('#').to_lowercase());
    anyhow::ensure!(!de.is_empty() && !a.is_empty() && de != a, "{}", crate::tr!("give two different tags: --merge FROM TO", "di dos etiquetas distintas: --merge DE A"));
    anyhow::ensure!(
        !del_sistema(&de) && !del_sistema(&a),
        "{}",
        crate::tr!("«{}» and «{}»: tags with a meaning for Omoikane (open, blocking, status, draft, the ones with `:`…) are not merged", "«{}» y «{}»: las etiquetas con significado para Omoikane (open, blocking, status, draft, las que llevan `:`…) no se unen", de, a)
    );
    let ids: Vec<String> = {
        let mut q = db.prepare(&format!("SELECT m.id FROM memory m WHERE EXISTS (SELECT 1 FROM json_each(m.tags) j WHERE j.value = ?1) AND {}", ambito.sql()))?;
        q.query_map([&de], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        let m = crate::memoria::obtener(db, id)?;
        let mut tags: Vec<String> = Vec::new();
        for t in m.tags {
            let t = if t == de { a.clone() } else { t };
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        let cambios = crate::memoria::Cambios { tags: Some(tags), reason: Some(format!("tag «{de}» merged into «{a}»")), ..Default::default() };
        crate::memoria::actualizar(db, id, cambios, None)?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variantes_de_verdad() {
        for (a, b) in [("idea", "ideas"), ("test", "tests"), ("Release", "release"), ("code-review", "code_review"), ("versión", "version"), ("versiones", "version"), ("errores", "error"), ("doc", "docs")] {
            assert!(variantes(a, b), "{a} ~ {b}");
        }
        for (a, b) in [("ide", "ideas"), ("not", "notes"), ("io", "ios"), ("rust", "rest"), ("bus", "bu"), ("android", "kotlin")] {
            assert!(!variantes(a, b), "{a} !~ {b}");
        }
    }

    #[test]
    fn cambia_por_la_existente_mas_usada() {
        let usadas: HashMap<String, usize> = [("tests".to_string(), 5), ("test".to_string(), 2), ("open".to_string(), 3)].into();
        let (t, c) = cerrar_con(&usadas, vec!["Tests".into(), "nueva".into(), "tests".into(), "opens".into(), "review:2027-01-01".into()]);
        assert_eq!(t, ["tests", "nueva", "opens", "review:2027-01-01"]);
        assert_eq!(c, [Cambio { from: "Tests".into(), to: "tests".into() }]);
    }

    #[test]
    fn las_del_sistema_no_se_tocan() {
        let usadas: HashMap<String, usize> = [("opens".to_string(), 9), ("statuses".to_string(), 9)].into();
        let (t, c) = cerrar_con(&usadas, vec!["open".into(), "status".into()]);
        assert_eq!(t, ["open", "status"]);
        assert!(c.is_empty());
    }
}
