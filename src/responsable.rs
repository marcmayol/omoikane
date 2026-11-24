//! The lead's view (1.7.0): whoever runs a client, or the whole factory, sees per space what is
//! there (projects with their latest `status`, what is pinned and the open decisions), who wrote it
//! (memories and activity per author and per agent) and what is stale or paused (what `revisar`
//! says, projects paused by sync and what nobody has touched for months).
//!
//! Read-only, and only what is visible from where you look (`Scope`): from a project, its space; from
//! the user's folder, all of them. Global memories belong to no client and don't show up here.

use crate::memoria::{self, Scope, Memory};
use crate::{proyecto, revisar};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Months untouched after which a memory shows up as forgotten, unless asked otherwise.
pub const MESES: u32 = 6;
/// How many rows are sent from each list (the counts are complete).
const PER_LIST: usize = 12;
/// Days that count as recent activity for each author.
const RECENT_DAYS: u32 = 30;

/// A group in the view: a space, or the projects without a space (empty key).
#[derive(Default)]
struct Group<'a> {
    memories: Vec<&'a Memory>,
    projects: Vec<Value>,
    pauses: Vec<Value>,
    warnings: Vec<&'a revisar::Notice>,
}

/// An author's figures within a group.
#[derive(Default)]
struct Author<'a> {
    memories: usize,
    last: &'a str,
    recent: usize,
    /// Per agent (`source`), how many.
    sources: BTreeMap<&'a str, usize>,
}

/// The memory in a few words, for lists.
fn brief(m: &Memory) -> Value {
    json!({
        "id": m.id,
        "title": m.title,
        "kind": m.kind.as_str(),
        "project": m.project,
        "author": m.author,
        "source": m.source,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
    })
}

/// A database date minus `offset` (e.g. `-6 months`), in the same format as `updated_at`, to
/// compare as text.
fn ago(db: &Connection, offset: &str) -> Result<String> {
    Ok(db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now',?1)", [offset], |f| f.get(0))?)
}

/// What the lead sees from `base`, grouped by space. `months`: since when an untouched memory
/// counts as forgotten.
pub fn panorama(db: &Connection, db_path: &Path, base: &Scope, months: u32) -> Result<Value> {
    let months = months.clamp(1, 120);
    // From a project, its whole space (the client's other projects too), not just its own.
    let base = &base.widen();
    let all = memoria::recent(db, base, None, usize::MAX >> 1)?;
    let warnings = revisar::notices(db, base)?;
    // The space being looked from (its key; empty: none) and whether everything is visible.
    let everything = matches!(base, Scope::All);
    let own = proyecto::space_key(db, base.space())?;
    let visible = |key: &str| everything || key == own;
    let cutoff = ago(db, &format!("-{months} months"))?;
    let recent_cutoff = ago(db, &format!("-{RECENT_DAYS} days"))?;

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    if !everything {
        groups.entry(own.clone()).or_default();
    }
    for e in proyecto::spaces(db)? {
        if visible(&e.key) {
            groups.entry(e.key).or_default();
        }
    }
    let mut globals = 0usize;
    for m in &all {
        if m.project_id == 0 && m.space.is_empty() {
            globals += 1;
        } else if visible(&m.space) {
            groups.entry(m.space.clone()).or_default().memories.push(m);
        }
    }
    let by_id: HashMap<&str, &Memory> = all.iter().map(|m| (m.id.as_str(), m)).collect();
    for a in &warnings {
        if let Some(m) = by_id.get(a.memory_id.as_str())
            && !(m.project_id == 0 && m.space.is_empty())
            && let Some(g) = groups.get_mut(&m.space)
        {
            g.warnings.push(a);
        }
    }
    // Paused projects, by key: they show in their card and in their space's list.
    let pauses: HashMap<String, Value> = proyecto::paused_projects(db)?
        .iter()
        .filter(|p| visible(&p.here))
        .map(|p| {
            let v = json!({
                "name": p.name,
                "key": p.key,
                "machine": p.pause.machine,
                "space": p.pause.space,
                "text": crate::sync::pause_text(db_path, p),
                "keep": crate::sync::keep_command(p),
                "accept": crate::sync::accept_command(p),
            });
            if let Some(g) = groups.get_mut(&p.here) {
                g.pauses.push(v.clone());
            }
            (p.key.clone(), v)
        })
        .collect();
    // Each project's latest `status` (the list goes from newest to oldest).
    let mut statuses: HashMap<i64, &Memory> = HashMap::new();
    for m in &all {
        if m.project_id != 0 && m.tags.iter().any(|t| t == memoria::STATUS) {
            statuses.entry(m.project_id).or_insert(m);
        }
    }
    for p in proyecto::list(db, None, None, usize::MAX)? {
        if !visible(&p.space) {
            continue;
        }
        let status = statuses.get(&p.id).map(|m| {
            json!({ "id": m.id, "title": m.title, "content": m.content, "updated_at": m.updated_at, "author": m.author })
        });
        let pinned = all.iter().filter(|m| m.project_id == p.id && m.pinned).count();
        let v = json!({
            "name": p.name,
            "path": p.path,
            "key": p.key,
            "memories": p.memories,
            "last_update": p.last_update,
            "pinned": pinned,
            "status": status,
            "gone": p.gone,
            "paused": pauses.contains_key(&p.key),
        });
        groups.entry(p.space.clone()).or_default().projects.push(v);
    }
    let names: HashMap<String, String> = proyecto::spaces(db)?.into_iter().map(|e| (e.key, e.name)).collect();

    // Spaces by name and, at the end, the projects without a space.
    let mut groups: Vec<(String, Group)> = groups.into_iter().collect();
    groups.sort_by_cached_key(|(k, _)| (k.is_empty(), names.get(k).map(|n| n.to_lowercase()).unwrap_or_default(), k.clone()));
    let spaces: Vec<Value> = groups
        .into_iter()
        // With no space and nothing in it, the group says nothing.
        .filter(|(k, g)| !k.is_empty() || !g.memories.is_empty() || !g.projects.is_empty() || !g.pauses.is_empty())
        .map(|(key, g)| {
            let ms = &g.memories;
            // Per author: how many, the latest, how many in recent days and with which agents.
            let mut authors: BTreeMap<&str, Author> = BTreeMap::new();
            for m in ms {
                let a = authors.entry(m.author.as_str()).or_default();
                a.memories += 1;
                if m.updated_at.as_str() > a.last {
                    a.last = m.updated_at.as_str();
                }
                if m.updated_at >= recent_cutoff {
                    a.recent += 1;
                }
                *a.sources.entry(m.source.as_str()).or_default() += 1;
            }
            let mut authors: Vec<Value> = authors
                .into_iter()
                .map(|(name, a)| {
                    let mut sources: Vec<(&str, usize)> = a.sources.into_iter().collect();
                    sources.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                    json!({
                        "author": name,
                        "memories": a.memories,
                        "last": a.last,
                        "recent": a.recent,
                        "sources": sources.iter().map(|(s, n)| json!({ "source": s, "memories": n })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            authors.sort_by(|a, b| b["last"].as_str().cmp(&a["last"].as_str()));
            let mut open: Vec<&&Memory> =
                ms.iter().filter(|m| m.kind == memoria::Kind::Decision && m.tags.iter().any(|t| t == memoria::OPEN)).collect();
            // Blocking ones first, as at session start.
            open.sort_by_key(|m| !m.is_blocking());
            let blocking = open.iter().filter(|m| m.is_blocking()).count();
            let pinned_mems: Vec<&&Memory> = ms.iter().filter(|m| m.pinned).collect();
            // Those nobody has touched since the cutoff, oldest to newest.
            let mut untouched: Vec<&&Memory> = ms.iter().filter(|m| m.updated_at < cutoff).collect();
            untouched.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
            for a in &g.warnings {
                *by_kind.entry(a.kind.as_str()).or_default() += 1;
            }
            let list = |v: &[&&Memory]| v.iter().take(PER_LIST).map(|m| brief(m)).collect::<Vec<_>>();
            json!({
                "key": key,
                "name": names.get(&key).cloned().unwrap_or_default(),
                "memories": ms.len(),
                // The space's own, without a project.
                "space_memories": ms.iter().filter(|m| m.project_id == 0).count(),
                "projects": g.projects,
                "pinned": { "count": pinned_mems.len(), "items": list(&pinned_mems) },
                "open_decisions": {
                    "count": open.len(),
                    "blocking": blocking,
                    "items": open.iter().take(PER_LIST).map(|m| {
                        let mut b = brief(m);
                        b["blocking"] = json!(m.is_blocking());
                        b
                    }).collect::<Vec<_>>(),
                },
                "authors": authors,
                "recent": ms.iter().take(PER_LIST).map(|m| brief(m)).collect::<Vec<_>>(),
                "review": {
                    "count": g.warnings.len(),
                    "kinds": by_kind,
                    "items": g.warnings.iter().take(PER_LIST * 2).map(|a| json!({
                        "kind": a.kind,
                        "memory_id": a.memory_id,
                        "title": a.title,
                        "project": a.project,
                        "other_id": a.other_id,
                        "other_title": a.other_title,
                        "detail": a.detail,
                    })).collect::<Vec<_>>(),
                },
                "paused": g.pauses,
                "untouched": { "count": untouched.len(), "items": list(&untouched) },
            })
        })
        .collect();
    Ok(json!({
        // Where it's looked from: everything (the user's folder) or a space (empty: projects without a space).
        "scope": if everything { "all" } else { "space" },
        "space": if everything { Value::Null } else { own.into() },
        "months": months,
        "recent_days": RECENT_DAYS,
        "global_memories": globals,
        "spaces": spaces,
    }))
}
