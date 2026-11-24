//! Connected memories (Phase 7.4): the `[[name]]` in the content, resolved against the tags of
//! the others (imported notes use their file name as a tag) or against an id. They are
//! recomputed with the review (`revisar::al_dia`, when the database changes); the ones that lead to no
//! memory show up in Review as broken links.
//!
//! With Marc's 137 real memories: 229 links in 113 memories, 220 resolved by tag
//! (replacing `-` with `_`) and 9 broken.

use crate::memoria::{Ambito, Memoria};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeSet, HashMap};

/// How a name is compared: lowercase, without accents and with `_` instead of `-` and spaces.
pub fn nombre(s: &str) -> String {
    crate::buscar::sin_tildes(&s.trim().to_lowercase()).replace(['-', ' '], "_")
}

/// The `[[name]]` (or `[[name|text]]`) in a text.
pub fn enlaces_de(text: &str) -> Vec<String> {
    let mut r = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        let Some(end) = after.find("]]") else { break };
        let inside = after[..end].split('|').next().unwrap_or("").trim();
        if !inside.is_empty() && inside.len() <= 120 && !inside.contains('\n') {
            r.push(inside.to_string());
        }
        rest = &after[end + 2..];
    }
    r
}

/// Rebuilds `memory_link` and returns the broken ones: (memory, name as written).
pub fn recalcular(db: &Connection) -> Result<Vec<(String, String)>> {
    let all = crate::memoria::recientes(db, &Ambito::Todos, None, usize::MAX >> 1)?;
    let mut by_name: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for m in &all {
        for t in &m.tags {
            by_name.entry(nombre(t)).or_default().insert(m.id.as_str());
        }
    }
    let by_id = |s: &str| -> Option<&str> { as_id(s).and_then(|s| all.iter().find(|m| m.id.starts_with(&s)).map(|m| m.id.as_str())) };
    let mut links: BTreeSet<(String, String)> = BTreeSet::new();
    let mut broken = Vec::new();
    for m in &all {
        for e in enlaces_de(&m.content) {
            let targets: Vec<&str> = match by_name.get(&nombre(&e)) {
                Some(v) => v.iter().copied().filter(|d| *d != m.id).collect(),
                None => by_id(&e).into_iter().filter(|d| *d != m.id).collect(),
            };
            if targets.is_empty() {
                // A link to a tag that only the memory itself carries is not broken: it is its own name.
                if !m.tags.iter().any(|t| nombre(t) == nombre(&e)) {
                    broken.push((m.id.clone(), e));
                }
            }
            for d in targets {
                links.insert((m.id.clone(), d.to_string()));
            }
        }
    }
    db.execute("DELETE FROM memory_link", [])?;
    let mut q = db.prepare("INSERT OR IGNORE INTO memory_link(from_id, to_id) VALUES (?1, ?2)")?;
    for (a, b) in &links {
        q.execute(params![a, b])?;
    }
    Ok(broken)
}

/// The same as [`recalcular`], but only for what can change when writing `ids`: their own
/// links, those of the memories that linked to them and those of the ones that name one of their tags or the
/// start of their id (which may now lead to them). It looks for them in the text index, which returns
/// too many (then each one is rebuilt with the same rules), and records their broken ones in `review`. `false` if
/// a name has nothing to search for in the index: then everything has to be recomputed.
pub fn recalcular_de(db: &Connection, ids: &BTreeSet<String>) -> Result<bool> {
    let mut redo: BTreeSet<String> = ids.clone();
    for id in ids {
        let mut q = db.prepare_cached("SELECT from_id FROM memory_link WHERE to_id = ?1")?;
        for f in q.query_map([id], |f| f.get::<_, String>(0))? {
            redo.insert(f?);
        }
        let Some(m) = crate::memoria::obtener(db, id).ok() else { continue };
        for t in &m.tags {
            let nt = nombre(t);
            let Some(phrase) = crate::revisar::frase_fts(&nt, false) else { return Ok(false) };
            for (other, content) in by_fts(db, &format!("content : {phrase}"))? {
                if enlaces_de(&content).iter().any(|e| nombre(e) == nt) {
                    redo.insert(other);
                }
            }
        }
        let prefixes: Vec<String> = (8..=m.id.len()).map(|k| format!("\"{}\"", m.id[..k].to_lowercase())).collect();
        if !prefixes.is_empty() {
            for (other, content) in by_fts(db, &format!("content : ({})", prefixes.join(" OR ")))? {
                if enlaces_de(&content).iter().any(|e| as_id(e).is_some_and(|s| m.id.starts_with(&s))) {
                    redo.insert(other);
                }
            }
        }
    }
    let mut broken = Vec::new();
    for id in &redo {
        db.execute("DELETE FROM memory_link WHERE from_id = ?1", [id])?;
        db.execute("DELETE FROM review WHERE memory_id = ?1 AND kind = ?2", params![id, crate::revisar::ENLACE_ROTO])?;
        let Some(m) = crate::memoria::obtener(db, id).ok() else { continue };
        for e in enlaces_de(&m.content) {
            let ne = nombre(&e);
            let Some(phrase) = crate::revisar::frase_fts(&ne, false) else { return Ok(false) };
            let with_tag: Vec<String> = by_fts_tags(db, &format!("tags : {phrase}"))?
                .into_iter()
                .filter(|(_, tags)| tags.iter().any(|t| nombre(t) == ne))
                .map(|(d, _)| d)
                .collect();
            let targets: Vec<String> = if with_tag.is_empty() {
                let first = match as_id(&e) {
                    // The first one in `recientes` order, as in the full recomputation.
                    Some(s) => db
                        .query_row("SELECT id FROM memory WHERE id GLOB ?1 ORDER BY updated_at DESC, id DESC LIMIT 1", [format!("{s}*")], |f| f.get(0))
                        .optional()?,
                    None => None,
                };
                first.into_iter().filter(|d| *d != m.id).collect()
            } else {
                with_tag.into_iter().filter(|d| *d != m.id).collect()
            };
            if targets.is_empty() && !m.tags.iter().any(|t| nombre(t) == ne) {
                broken.push((m.id.clone(), e));
            }
            for d in targets {
                db.execute("INSERT OR IGNORE INTO memory_link(from_id, to_id) VALUES (?1, ?2)", params![m.id, d])?;
            }
        }
    }
    crate::revisar::apuntar_rotos(db, broken)?;
    Ok(true)
}

/// A link that looks like the start of an id (8 letters or digits or more), uppercased.
fn as_id(e: &str) -> Option<String> {
    let s = e.trim().to_uppercase();
    (s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric())).then_some(s)
}

/// (id, content) of the memories returned by an FTS query.
fn by_fts(db: &Connection, query: &str) -> Result<Vec<(String, String)>> {
    let mut q = db.prepare_cached("SELECT id, content FROM memory WHERE n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)")?;
    let v = q.query_map([query], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// (id, tags) of the memories returned by an FTS query.
fn by_fts_tags(db: &Connection, query: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut q = db.prepare_cached("SELECT id, tags FROM memory WHERE n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)")?;
    let v: Vec<(String, String)> = q.query_map([query], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v.into_iter().map(|(id, tags)| (id, serde_json::from_str(&tags).unwrap_or_default())).collect())
}

/// The ones a memory links to and the ones that link to it (ids).
pub fn vecinas(db: &Connection, id: &str) -> Result<(Vec<String>, Vec<String>)> {
    let list = |sql: &str| -> Result<Vec<String>> {
        let mut q = db.prepare(sql)?;
        let v = q.query_map([id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    };
    Ok((list("SELECT to_id FROM memory_link WHERE from_id = ?1")?, list("SELECT from_id FROM memory_link WHERE to_id = ?1")?))
}

/// All links, for the view: from → [to].
pub fn todos(db: &Connection) -> Result<HashMap<String, Vec<String>>> {
    let mut q = db.prepare("SELECT from_id, to_id FROM memory_link ORDER BY from_id, to_id")?;
    let mut r: HashMap<String, Vec<String>> = HashMap::new();
    for f in q.query_map([], |f| Ok((f.get::<_, String>(0)?, f.get::<_, String>(1)?)))? {
        let (a, b) = f?;
        r.entry(a).or_default().push(b);
    }
    Ok(r)
}

/// For recall: a neighbour of the ones about to be injected that is worth bringing too (a gotcha or
/// a preference). One that the memory links to («before opening it see [[…]]») was named by whoever
/// wrote it: `bar_outgoing` from the message is enough; one that links to it needs `bar_incoming`.
/// Returns the neighbour and which one it comes from.
pub fn vecina_para(
    db: &Connection,
    project: &str,
    prompt: &str,
    chosen: &[Memoria],
    exclude: &BTreeSet<String>,
    bar_outgoing: f64,
    bar_incoming: f64,
) -> Result<Option<(Memoria, String)>> {
    let mut candidates: Vec<(Memoria, String, f64)> = Vec::new();
    for m in chosen {
        let (outgoing, incoming) = vecinas(db, &m.id)?;
        let marked = outgoing.into_iter().map(|id| (id, bar_outgoing)).chain(incoming.into_iter().map(|id| (id, bar_incoming)));
        for (id, bar) in marked {
            if exclude.contains(&id) || chosen.iter().any(|x| x.id == id) || candidates.iter().any(|(c, _, _)| c.id == id) {
                continue;
            }
            let Ok(v) = crate::memoria::obtener(db, &id) else { continue };
            let visible = crate::memoria::Ambito::de(db, project)?.ve(db, &v.id)?;
            if visible && matches!(v.kind, crate::memoria::Tipo::Gotcha | crate::memoria::Tipo::Preference) {
                candidates.push((v, m.id.clone(), bar));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    let coverages = crate::recall::cobertura(db, project, prompt, candidates.iter().map(|(m, _, _)| m))?;
    let best = coverages.iter().enumerate().filter(|(i, c)| **c >= candidates[*i].2).max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i);
    Ok(best.map(|i| {
        let (m, from, _) = candidates.swap_remove(i);
        (m, from)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_names() {
        assert_eq!(enlaces_de("ver [[feedback_estructura_plana]] y [[project-troba|Troba]]; no [[ ]] ni [roto]"), ["feedback_estructura_plana", "project-troba"]);
        assert_eq!(nombre("Project-Impresora Creality-K2"), "project_impresora_creality_k2");
    }
}
