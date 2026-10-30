//! Memorias conectadas (Fase 7.4): los `[[nombre]]` del contenido, resueltos contra las etiquetas de
//! las demás (las notas importadas usan su nombre de fichero como etiqueta) o contra un id. Se
//! recalculan con la revisión (`revisar::al_dia`, cuando cambia la base); los que no llevan a ninguna
//! memoria salen en Revisar como enlaces rotos.
//!
//! Con las 137 memorias reales de Marc: 229 enlaces en 113 memorias, 220 resueltos por etiqueta
//! (cambiando `-` por `_`) y 9 rotos.

use crate::memoria::{Ambito, Memoria};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};

/// Cómo se compara un nombre: minúsculas, sin tildes y con `_` en lugar de `-` y espacios.
pub fn nombre(s: &str) -> String {
    crate::buscar::sin_tildes(&s.trim().to_lowercase()).replace(['-', ' '], "_")
}

/// Los `[[nombre]]` (o `[[nombre|texto]]`) de un texto.
pub fn enlaces_de(texto: &str) -> Vec<String> {
    let mut r = Vec::new();
    let mut resto = texto;
    while let Some(i) = resto.find("[[") {
        let tras = &resto[i + 2..];
        let Some(fin) = tras.find("]]") else { break };
        let dentro = tras[..fin].split('|').next().unwrap_or("").trim();
        if !dentro.is_empty() && dentro.len() <= 120 && !dentro.contains('\n') {
            r.push(dentro.to_string());
        }
        resto = &tras[fin + 2..];
    }
    r
}

/// Rehace `memory_link` y devuelve los rotos: (memoria, nombre tal como está escrito).
pub fn recalcular(db: &Connection) -> Result<Vec<(String, String)>> {
    let todas = crate::memoria::recientes(db, &Ambito::Todos, None, usize::MAX >> 1)?;
    let mut por_nombre: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for m in &todas {
        for t in &m.tags {
            por_nombre.entry(nombre(t)).or_default().insert(m.id.as_str());
        }
    }
    let por_id = |s: &str| -> Option<&str> { es_id(s).and_then(|s| todas.iter().find(|m| m.id.starts_with(&s)).map(|m| m.id.as_str())) };
    let mut enlaces: BTreeSet<(String, String)> = BTreeSet::new();
    let mut rotos = Vec::new();
    for m in &todas {
        for e in enlaces_de(&m.content) {
            let destinos: Vec<&str> = match por_nombre.get(&nombre(&e)) {
                Some(v) => v.iter().copied().filter(|d| *d != m.id).collect(),
                None => por_id(&e).into_iter().filter(|d| *d != m.id).collect(),
            };
            if destinos.is_empty() {
                // Un enlace a una etiqueta que solo lleva ella misma no está roto: es su propio nombre.
                if !m.tags.iter().any(|t| nombre(t) == nombre(&e)) {
                    rotos.push((m.id.clone(), e));
                }
            }
            for d in destinos {
                enlaces.insert((m.id.clone(), d.to_string()));
            }
        }
    }
    db.execute("DELETE FROM memory_link", [])?;
    let mut q = db.prepare("INSERT OR IGNORE INTO memory_link(from_id, to_id) VALUES (?1, ?2)")?;
    for (a, b) in &enlaces {
        q.execute(params![a, b])?;
    }
    Ok(rotos)
}

/// Lo mismo que [`recalcular`], pero solo para lo que puede cambiar al escribir `ids`: sus propios
/// enlaces, los de las memorias que las enlazaban y los de las que nombran una de sus etiquetas o el
/// principio de su id (que ahora pueden llevar a ellas). Las busca en el índice de texto, que da de
/// más (luego se rehace cada una con las mismas reglas), y apunta sus rotos en `review`. `false` si
/// un nombre no tiene nada que buscar en el índice: entonces hay que recalcularlo todo.
pub fn recalcular_de(db: &Connection, ids: &BTreeSet<String>) -> Result<bool> {
    let mut rehacer: BTreeSet<String> = ids.clone();
    for id in ids {
        let mut q = db.prepare_cached("SELECT from_id FROM memory_link WHERE to_id = ?1")?;
        for f in q.query_map([id], |f| f.get::<_, String>(0))? {
            rehacer.insert(f?);
        }
        let Some(m) = crate::memoria::obtener(db, id).ok() else { continue };
        for t in &m.tags {
            let nt = nombre(t);
            let Some(frase) = crate::revisar::frase_fts(&nt, false) else { return Ok(false) };
            for (otra, contenido) in por_fts(db, &format!("content : {frase}"))? {
                if enlaces_de(&contenido).iter().any(|e| nombre(e) == nt) {
                    rehacer.insert(otra);
                }
            }
        }
        let prefijos: Vec<String> = (8..=m.id.len()).map(|k| format!("\"{}\"", m.id[..k].to_lowercase())).collect();
        if !prefijos.is_empty() {
            for (otra, contenido) in por_fts(db, &format!("content : ({})", prefijos.join(" OR ")))? {
                if enlaces_de(&contenido).iter().any(|e| es_id(e).is_some_and(|s| m.id.starts_with(&s))) {
                    rehacer.insert(otra);
                }
            }
        }
    }
    let mut rotos = Vec::new();
    for id in &rehacer {
        db.execute("DELETE FROM memory_link WHERE from_id = ?1", [id])?;
        db.execute("DELETE FROM review WHERE memory_id = ?1 AND kind = ?2", params![id, crate::revisar::ENLACE_ROTO])?;
        let Some(m) = crate::memoria::obtener(db, id).ok() else { continue };
        for e in enlaces_de(&m.content) {
            let ne = nombre(&e);
            let Some(frase) = crate::revisar::frase_fts(&ne, false) else { return Ok(false) };
            let con_etiqueta: Vec<String> = por_fts_etiquetas(db, &format!("tags : {frase}"))?
                .into_iter()
                .filter(|(_, tags)| tags.iter().any(|t| nombre(t) == ne))
                .map(|(d, _)| d)
                .collect();
            let destinos: Vec<String> = if con_etiqueta.is_empty() {
                let primera = match es_id(&e) {
                    // La primera en el orden de `recientes`, como en el recálculo entero.
                    Some(s) => db
                        .query_row("SELECT id FROM memory WHERE id GLOB ?1 ORDER BY updated_at DESC, id DESC LIMIT 1", [format!("{s}*")], |f| f.get(0))
                        .optional()?,
                    None => None,
                };
                primera.into_iter().filter(|d| *d != m.id).collect()
            } else {
                con_etiqueta.into_iter().filter(|d| *d != m.id).collect()
            };
            if destinos.is_empty() && !m.tags.iter().any(|t| nombre(t) == ne) {
                rotos.push((m.id.clone(), e));
            }
            for d in destinos {
                db.execute("INSERT OR IGNORE INTO memory_link(from_id, to_id) VALUES (?1, ?2)", params![m.id, d])?;
            }
        }
    }
    crate::revisar::apuntar_rotos(db, rotos)?;
    Ok(true)
}

/// Un enlace que parece el principio de un id (8 letras o cifras o más), en mayúsculas.
fn es_id(e: &str) -> Option<String> {
    let s = e.trim().to_uppercase();
    (s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric())).then_some(s)
}

/// (id, contenido) de las memorias que da una consulta FTS.
fn por_fts(db: &Connection, consulta: &str) -> Result<Vec<(String, String)>> {
    let mut q = db.prepare_cached("SELECT id, content FROM memory WHERE n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)")?;
    let v = q.query_map([consulta], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// (id, etiquetas) de las memorias que da una consulta FTS.
fn por_fts_etiquetas(db: &Connection, consulta: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut q = db.prepare_cached("SELECT id, tags FROM memory WHERE n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)")?;
    let v: Vec<(String, String)> = q.query_map([consulta], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v.into_iter().map(|(id, tags)| (id, serde_json::from_str(&tags).unwrap_or_default())).collect())
}

/// Las que enlaza una memoria y las que la enlazan a ella (ids).
pub fn vecinas(db: &Connection, id: &str) -> Result<(Vec<String>, Vec<String>)> {
    let lista = |sql: &str| -> Result<Vec<String>> {
        let mut q = db.prepare(sql)?;
        let v = q.query_map([id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    };
    Ok((lista("SELECT to_id FROM memory_link WHERE from_id = ?1")?, lista("SELECT from_id FROM memory_link WHERE to_id = ?1")?))
}

/// Todos los enlaces, para la vista: from → [to].
pub fn todos(db: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let mut q = db.prepare("SELECT from_id, to_id FROM memory_link ORDER BY from_id, to_id")?;
    let mut r: HashMap<String, Vec<String>> = HashMap::new();
    for f in q.query_map([], |f| Ok((f.get::<_, String>(0)?, f.get::<_, String>(1)?)))? {
        let (a, b) = f?;
        r.entry(a).or_default().push(b);
    }
    Ok(r)
}

/// Para el recall: una vecina de las que se van a inyectar que conviene traer también (una trampa o
/// una preferencia). Una que la memoria enlaza («antes de abrirlo ver [[…]]») lo dijo quien la
/// escribió: basta `liston_sale` del mensaje; una que la enlaza a ella necesita `liston_entra`.
/// Devuelve la vecina y de cuál viene.
pub fn vecina_para(
    db: &Connection,
    project: &str,
    prompt: &str,
    elegidas: &[Memoria],
    excluir: &BTreeSet<String>,
    liston_sale: f64,
    liston_entra: f64,
) -> Result<Option<(Memoria, String)>> {
    let mut candidatas: Vec<(Memoria, String, f64)> = Vec::new();
    for m in elegidas {
        let (salen, entran) = vecinas(db, &m.id)?;
        let marcadas = salen.into_iter().map(|id| (id, liston_sale)).chain(entran.into_iter().map(|id| (id, liston_entra)));
        for (id, liston) in marcadas {
            if excluir.contains(&id) || elegidas.iter().any(|x| x.id == id) || candidatas.iter().any(|(c, _, _)| c.id == id) {
                continue;
            }
            let Ok(v) = crate::memoria::obtener(db, &id) else { continue };
            let visible = v.project.is_empty() || v.project == project || crate::proyecto::es_general(project);
            if visible && matches!(v.kind, crate::memoria::Tipo::Gotcha | crate::memoria::Tipo::Preference) {
                candidatas.push((v, m.id.clone(), liston));
            }
        }
    }
    if candidatas.is_empty() {
        return Ok(None);
    }
    let coberturas = crate::recall::cobertura(db, project, prompt, candidatas.iter().map(|(m, _, _)| m))?;
    let mejor = coberturas.iter().enumerate().filter(|(i, c)| **c >= candidatas[*i].2).max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i);
    Ok(mejor.map(|i| {
        let (m, de, _) = candidatas.swap_remove(i);
        (m, de)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlaces_y_nombres() {
        assert_eq!(enlaces_de("ver [[feedback_estructura_plana]] y [[project-troba|Troba]]; no [[ ]] ni [roto]"), ["feedback_estructura_plana", "project-troba"]);
        assert_eq!(nombre("Project-Impresora Creality-K2"), "project_impresora_creality_k2");
    }
}
