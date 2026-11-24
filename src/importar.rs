//! Import memories from another system: engram's JSON export (`engram export`) or our own
//! `omoi export` (one memory per line). What each format knows is kept (kind, project,
//! pinned, dates) and anything with no equivalent goes in as a tag. What is already (almost identical) in
//! the database is skipped, unless `force`. From Omoikane's export only the current version comes in,
//! without the history.

use crate::embeber::Modelo;
use crate::memoria::{self, Nueva, Tipo};
use crate::{proyecto, vectores};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// A memory as it is going to be saved, with its original dates.
#[derive(Debug, Clone)]
pub struct Entrante {
    pub nueva: Nueva,
    pub creada: Option<String>,
    pub cambiada: Option<String>,
}

#[derive(Debug, Default)]
pub struct Resumen {
    pub guardadas: usize,
    pub duplicadas: usize,
    /// Those that couldn't be saved even after fixing what can be fixed (stderr says which).
    pub fallidas: usize,
}

/// What `memoria::guardar` rejects and can be fixed without losing anything: a title over 200
/// characters is cut (and the whole one moves to the start of the content) and empty content takes the title.
fn fix_up(mut n: Nueva) -> Nueva {
    if n.title.chars().count() > 200 {
        let short: String = n.title.chars().take(199).collect();
        n.content = if n.content.trim().is_empty() { n.title.clone() } else { format!("{}\n\n{}", n.title, n.content) };
        n.title = format!("{short}…");
    }
    if n.content.trim().is_empty() {
        n.content = n.title.clone();
    }
    n
}

/// Is there already a memory with the same title and content in that scope? For when there is no model.
fn identical(db: &Connection, n: &Nueva) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory WHERE project_id = ?1 AND title = ?2 AND content = ?3)",
        params![crate::proyecto::resolver(db, &n.project)?.map_or(-1, |(id, _)| id), n.title, n.content],
        |row| row.get(0),
    )?)
}

#[derive(Deserialize)]
struct EngramExport {
    #[serde(default)]
    sessions: Vec<EngramSession>,
    observations: Vec<EngramObs>,
}

#[derive(Deserialize)]
struct EngramSession {
    id: String,
    #[serde(default)]
    directory: Option<String>,
}

#[derive(Deserialize)]
struct EngramObs {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default, rename = "type")]
    tipo: Option<String>,
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    topic_key: Option<String>,
    #[serde(default, deserialize_with = "boolish")]
    pinned: bool,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

/// engram exports `pinned` as a boolean or as text depending on the version.
fn boolish<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    })
}

#[derive(Deserialize)]
struct OmoikaneLine {
    project: String,
    kind: Tipo,
    title: String,
    content: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    pinned: bool,
    created_at: Option<String>,
    updated_at: Option<String>,
}

/// engram kind → Omoikane kind. Anything with no equivalent is a `fact`.
fn kind_from_engram(t: &str) -> Tipo {
    match t {
        "bugfix" => Tipo::Gotcha,
        "decision" => Tipo::Decision,
        "preference" => Tipo::Preference,
        _ => Tipo::Fact,
    }
}

/// Reads an export file (the format is detected automatically). `dir` forces the project of every
/// memory that isn't global; without it, engram's go to the project of their session's folder
/// if it exists on this machine, and otherwise to `current`'s.
pub fn leer(path: &Path, dir: Option<&Path>, current: &str) -> Result<(Vec<Entrante>, usize)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    let forced = dir.map(proyecto::de).transpose()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // A single JSON object with `observations` is an engram export; if it can't be read,
    // say why (and not "it's neither one nor the other").
    if text.trim_start().starts_with('{') && text.contains("\"observations\"") {
        let e: EngramExport = serde_json::from_str(text).with_context(|| format!("{} looks like an engram export but can't be read", path.display()))?;
        return Ok(from_engram(e, forced.as_deref(), current));
    }
    // One memory per line: Omoikane's export.
    let mut v = Vec::new();
    for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let m: OmoikaneLine = serde_json::from_str(l).with_context(|| format!("line {} isn't an engram export nor an Omoikane export", i + 1))?;
        let project = match (&forced, m.project.is_empty()) {
            (_, true) => String::new(),
            (Some(p), false) => p.clone(),
            (None, false) => m.project,
        };
        v.push(Entrante {
            nueva: Nueva { project, kind: Some(m.kind), title: m.title, content: m.content, paths: m.paths, tags: m.tags, pinned: m.pinned, source: "import:omoikane".into(), ..Default::default() },
            creada: m.created_at,
            cambiada: m.updated_at,
        });
    }
    if v.is_empty() {
        bail!("{} has no memories", path.display());
    }
    Ok((v, 0))
}

fn from_engram(e: EngramExport, forced: Option<&str>, current: &str) -> (Vec<Entrante>, usize) {
    let dirs: HashMap<String, String> = e.sessions.into_iter().filter_map(|s| Some((s.id, s.directory?))).collect();
    let mut deleted = 0;
    let mut v = Vec::new();
    for o in e.observations {
        if o.deleted_at.as_deref().is_some_and(|d| !d.is_empty()) {
            deleted += 1;
            continue;
        }
        let project = if o.scope.as_deref() == Some("personal") {
            String::new()
        } else if let Some(p) = forced {
            p.to_string()
        } else {
            o.session_id
                .as_ref()
                .and_then(|s| dirs.get(s))
                .filter(|d| proyecto::es_carpeta(Path::new(d)))
                .and_then(|d| proyecto::de(Path::new(d)).ok())
                .unwrap_or_else(|| current.to_string())
        };
        let kind = o.tipo.unwrap_or_default();
        let mut tags = Vec::new();
        if !kind.is_empty() && !matches!(kind.as_str(), "bugfix" | "decision" | "preference" | "manual") {
            tags.push(kind.clone());
        }
        if let Some(t) = o.topic_key.filter(|t| !t.is_empty()) {
            tags.push(format!("topic:{t}"));
        }
        v.push(Entrante {
            nueva: Nueva {
                project,
                kind: Some(kind_from_engram(&kind)),
                title: o.title,
                content: o.content,
                paths: Vec::new(),
                tags,
                pinned: o.pinned,
                source: "import:engram".into(),
                ..Default::default()
            },
            creada: o.created_at,
            cambiada: o.updated_at,
        });
    }
    (v, deleted)
}

/// Saves the memories read, skipping those already present (unless `force`), with their dates.
pub fn guardar(db: &mut Connection, model: Option<&Modelo>, incoming: Vec<Entrante>, force: bool) -> Result<Resumen> {
    let mut r = Resumen::default();
    for e in incoming {
        let new = fix_up(e.nueva);
        // Without a model, at least the identical ones: reimporting the same file must not duplicate everything.
        let repeated = match model {
            _ if force => false,
            Some(m) => vectores::duplicada(db, m, &new.project, &new.title, &new.content)?.is_some(),
            None => identical(db, &new)?,
        };
        if repeated {
            r.duplicadas += 1;
            continue;
        }
        let title = new.title.clone();
        let id = match memoria::guardar(db, new, model) {
            Ok(m) => m.id,
            Err(err) => {
                // A broken entry doesn't stop the import: count it and carry on.
                eprintln!("omoikane: «{title}»: {err:#}");
                r.fallidas += 1;
                continue;
            }
        };
        // The source dates, if they make sense: «2026-10-28 13:28:16» or already in our own format.
        for (column, value) in [("created_at", e.creada), ("updated_at", e.cambiada)] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                db.execute(
                    &format!(
                        "UPDATE memory SET {column} = strftime('%Y-%m-%dT%H:%M:%fZ', ?1) WHERE id = ?2 AND strftime('%Y-%m-%dT%H:%M:%fZ', ?1) IS NOT NULL"
                    ),
                    params![v.trim_end_matches('Z').replace('T', " "), id],
                )?;
            }
        }
        r.guardadas += 1;
    }
    Ok(r)
}
