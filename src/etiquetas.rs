//! A closed tag vocabulary (1.10.1). Agents invent variants of the same tag
//! (`idea` and `ideas`, `test` and `tests`, `Release` and `release`, `a-b` and `a_b`) and each one splits
//! tag search in two. When a new memory is saved (`remember`, via MCP or CLI), each
//! tag that does not exist yet but is a variant of one already in the memory's scope is
//! replaced by that one, and the response says so. Nothing else: imported or synced data goes in as is, and a
//! system tag (`open`, `blocking`, `status`, `draft`, the ones with `:`) is neither changed nor
//! used as a target.

use crate::memoria::Scope;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

/// Tags with their own meaning for Omoikane: they are never changed and no other tag is changed into them.
const SYSTEM_TAGS: [&str; 8] = [
    crate::memoria::STATUS,
    crate::memoria::OPEN,
    crate::memoria::BLOCKING,
    "draft",
    "deprecated",
    "verified",
    "pinned",
    "review-after",
];

fn is_system(t: &str) -> bool {
    t.contains(':') || SYSTEM_TAGS.contains(&t)
}

/// The base of a tag: lowercase, without accents and without separators (`-`, `_`, spaces, `.`).
fn base(t: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    t.to_lowercase().nfkd().filter(|c| c.is_alphanumeric()).filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect()
}

/// The forms by which a tag can match another: its base, and without the plural (`-s`; `-es`
/// after n, r, l, d, z, j or y, as in «versiones» → «version»). None shorter than 3 letters.
fn forms(t: &str) -> Vec<String> {
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

/// Whether two tags are variants of each other.
pub fn are_variants(a: &str, b: &str) -> bool {
    let fb = forms(b);
    forms(a).iter().any(|f| fb.contains(f))
}

/// A change made: the requested tag and the one that already existed and takes its place.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TagChange {
    pub from: String,
    pub to: String,
}

/// Where the vocabulary is looked up for a memory going to the project of folder `project` (empty: no
/// project) or to space `space_id`: what is visible from that project; for a space, the space's;
/// global, what belongs to no space (never a client's tags).
pub fn scope_for(db: &Connection, project: &str, space_id: Option<i64>) -> Result<Scope> {
    Ok(match space_id {
        _ if !project.is_empty() => Scope::from_project(db, project)?,
        Some(s) => Scope::Space(Some(s)),
        None => Scope::Space(None),
    })
}

/// How many memories in the scope carry each tag.
fn tag_counts(db: &Connection, scope: &Scope) -> Result<HashMap<String, usize>> {
    let mut q = db.prepare(&format!("SELECT j.value FROM memory m, json_each(m.tags) j WHERE {}", scope.sql()))?;
    let mut n = HashMap::new();
    for t in q.query_map([], |f| f.get::<_, String>(0))? {
        *n.entry(t?).or_insert(0usize) += 1;
    }
    Ok(n)
}

/// The tags of a new memory with each variant replaced by the tag that already exists in
/// `scope` (the most used, if there are several), without duplicates, and the changes made.
pub fn normalize_tags(db: &Connection, scope: &Scope, tags: Vec<String>) -> Result<(Vec<String>, Vec<TagChange>)> {
    if tags.iter().all(|t| is_system(t)) {
        return Ok((tags, Vec::new()));
    }
    let counts = tag_counts(db, scope)?;
    Ok(close_with(&counts, tags))
}

fn close_with(counts: &HashMap<String, usize>, tags: Vec<String>) -> (Vec<String>, Vec<TagChange>) {
    let mut out = Vec::new();
    let mut changes = Vec::new();
    for t in tags {
        let tag = if is_system(&t) || counts.contains_key(&t) {
            t
        } else {
            let best = counts
                .iter()
                .filter(|(e, _)| !is_system(e) && are_variants(&t, e))
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(e, _)| e.clone());
            match best {
                Some(e) => {
                    changes.push(TagChange { from: t, to: e.clone() });
                    e
                }
                None => t,
            }
        };
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    (out, changes)
}

/// The pairs of variant tags already in the scope (the most used first), for review.
pub fn variant_pairs(db: &Connection, scope: &Scope) -> Result<Vec<(String, usize, String, usize)>> {
    let counts = tag_counts(db, scope)?;
    let mut v: Vec<(&String, &usize)> = counts.iter().filter(|(t, _)| !is_system(t)).collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut out = Vec::new();
    for (i, (a, na)) in v.iter().enumerate() {
        for (b, nb) in &v[i + 1..] {
            if are_variants(a, b) {
                out.push(((*a).clone(), **na, (*b).clone(), **nb));
            }
        }
    }
    Ok(out)
}

/// Replaces tag `from` with `to` in every memory of the scope that carries it, each one with its
/// version in the history. Returns how many changed. System tags are not merged.
pub fn merge(db: &mut Connection, scope: &Scope, from: &str, to: &str) -> Result<usize> {
    let (from, to) = (from.trim().trim_start_matches('#').to_lowercase(), to.trim().trim_start_matches('#').to_lowercase());
    anyhow::ensure!(!from.is_empty() && !to.is_empty() && from != to, "{}", crate::tr!("give two different tags: --merge FROM TO", "di dos etiquetas distintas: --merge DE A"));
    anyhow::ensure!(
        !is_system(&from) && !is_system(&to),
        "{}",
        crate::tr!("«{}» and «{}»: tags with a meaning for Omoikane (open, blocking, status, draft, the ones with `:`…) are not merged", "«{}» y «{}»: las etiquetas con significado para Omoikane (open, blocking, status, draft, las que llevan `:`…) no se unen", from, to)
    );
    let ids: Vec<String> = {
        let mut q = db.prepare(&format!("SELECT m.id FROM memory m WHERE EXISTS (SELECT 1 FROM json_each(m.tags) j WHERE j.value = ?1) AND {}", scope.sql()))?;
        q.query_map([&from], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        let m = crate::memoria::get(db, id)?;
        let mut tags: Vec<String> = Vec::new();
        for t in m.tags {
            let t = if t == from { to.clone() } else { t };
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        let changes = crate::memoria::Changes { tags: Some(tags), reason: Some(format!("tag «{from}» merged into «{to}»")), ..Default::default() };
        crate::memoria::update(db, id, changes, None)?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_variants() {
        for (a, b) in [("idea", "ideas"), ("test", "tests"), ("Release", "release"), ("code-review", "code_review"), ("versión", "version"), ("versiones", "version"), ("errores", "error"), ("doc", "docs")] {
            assert!(are_variants(a, b), "{a} ~ {b}");
        }
        for (a, b) in [("ide", "ideas"), ("not", "notes"), ("io", "ios"), ("rust", "rest"), ("bus", "bu"), ("android", "kotlin")] {
            assert!(!are_variants(a, b), "{a} !~ {b}");
        }
    }

    #[test]
    fn replaces_with_the_most_used_existing() {
        let counts: HashMap<String, usize> = [("tests".to_string(), 5), ("test".to_string(), 2), ("open".to_string(), 3)].into();
        let (t, c) = close_with(&counts, vec!["Tests".into(), "nueva".into(), "tests".into(), "opens".into(), "review:2027-01-01".into()]);
        assert_eq!(t, ["tests", "nueva", "opens", "review:2027-01-01"]);
        assert_eq!(c, [TagChange { from: "Tests".into(), to: "tests".into() }]);
    }

    #[test]
    fn system_tags_are_untouched() {
        let counts: HashMap<String, usize> = [("opens".to_string(), 9), ("statuses".to_string(), 9)].into();
        let (t, c) = close_with(&counts, vec!["open".into(), "status".into()]);
        assert_eq!(t, ["open", "status"]);
        assert!(c.is_empty());
    }
}
