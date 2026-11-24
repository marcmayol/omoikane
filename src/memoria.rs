//! Memories: save, read, change (keeping the previous version), forget and list.
//!
//! Every write touches `memory`, `memory_fts` and `memory_path` in a single `IMMEDIATE` transaction:
//! there is never an unindexed memory nor an index pointing to a deleted memory.

use crate::embeber::Modelo;
use crate::{tr, vectores};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use std::str::FromStr;

/// Each kind changes how the memory is recalled. A kind this version doesn't know
/// (from a future version, via sync) is stored as is in the database and treated as `fact`
/// (see `Memory::kind_text`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Fact,
    Decision,
    Gotcha,
    Preference,
    /// A recipe: steps to follow (publish a version, deploy…), with a `when:` line of
    /// the words that bring it in whole (Phase 7.3).
    Procedure,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Fact, Kind::Decision, Kind::Gotcha, Kind::Preference, Kind::Procedure];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Decision => "decision",
            Kind::Gotcha => "gotcha",
            Kind::Preference => "preference",
            Kind::Procedure => "procedure",
        }
    }
}

impl FromStr for Kind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Kind> {
        Kind::ALL
            .into_iter()
            .find(|t| t.as_str() == s.trim().to_lowercase())
            .ok_or_else(|| anyhow!(tr!("unknown kind «{s}»: use fact, decision, gotcha, preference or procedure", "tipo «{s}» desconocido: usa fact, decision, gotcha, preference o procedure")))
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    /// Main folder of its project (`project.path`); empty = global or space memory.
    pub project: String,
    /// Its project in this database (0 = none). Not in the JSON: between computers the key is what counts.
    #[serde(skip)]
    pub project_id: i64,
    /// The sync key of its project (empty if it has none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_key: String,
    /// The space (its key): the memory's if it is a space memory (no project), or its project's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub kind: Kind,
    /// The kind as stored, when this version doesn't know it (and `kind` says `fact`): so it isn't
    /// lost when re-exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_text: Option<String>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
    /// The stamp of its last text change: only in the copy that goes to the trash (so its
    /// verdicts still hold if it comes back). Empty when read from the database.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hlc: String,
    pub created_at: String,
    pub updated_at: String,
    pub recalled_at: Option<String>,
    pub recall_count: i64,
    /// Who wrote it (the git name), if sync records it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// What `secretos` removed when saving or changing it in this call, one kind per finding (empty if
    /// nothing). Not stored nor in the JSON: it is so the caller can say so.
    #[serde(skip)]
    pub redacted: Vec<&'static str>,
    /// Who verified it and when, if a person verified it and the text hasn't changed since
    /// (`verificado`, in `memory.extra`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<crate::verificado::Verified>,
    /// In which session and at which commit it was written (`procedencia`, in `memory.extra`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::procedencia::Provenance>,
    /// What it is based on (`evidencia`, in `memory.extra`): what the one who saved it saw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<crate::evidencia::Evidence>,
    /// What to check when its review date arrives (`review:YYYY-MM-DD`), in `memory.extra`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    /// The verification removed by this call's text change (`update`). Not stored nor in
    /// the JSON: it is so the caller can say so.
    #[serde(skip)]
    pub unverified: Option<crate::verificado::Verified>,
}

impl Memory {
    /// Of a space, with no project.
    pub fn space_only(&self) -> bool {
        self.project_id == 0 && !self.space.is_empty()
    }

    /// Global: neither a project's nor a space's.
    pub fn global(&self) -> bool {
        self.project_id == 0 && self.space.is_empty()
    }

    /// The kind as stored and sent (an unknown one, as is).
    pub fn kind_str(&self) -> &str {
        self.kind_text.as_deref().unwrap_or(self.kind.as_str())
    }
}

/// What the caller decides. The rest (id, dates, stats) is set by Omoikane.
#[derive(Clone, Debug, Default)]
pub struct NewMemory {
    /// The project folder (empty = global). Ignored if `project_id` or `space_id` is given.
    pub project: String,
    /// The already-resolved project (what comes in through sync).
    pub project_id: Option<i64>,
    /// Memory of this space, with no project.
    pub space_id: Option<i64>,
    pub kind: Option<Kind>,
    /// The kind as is, if this version doesn't know it (what comes in through sync).
    pub kind_text: Option<String>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
    /// Where it comes from (`procedencia::current`), for what is written here: goes to `memory.extra`.
    pub provenance: Option<crate::procedencia::Provenance>,
    /// What it is based on (`evidencia`), already cleaned: goes to `memory.extra`.
    pub evidence: Vec<crate::evidencia::Evidence>,
    /// What to check when its review date arrives: goes to `memory.extra`.
    pub review_reason: Option<String>,
}

/// Only what comes in `Some` changes.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    pub title: Option<String>,
    pub content: Option<String>,
    pub kind: Option<Kind>,
    pub paths: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    /// Move it to another project (`""` = global). Like pinning, it doesn't create a version.
    pub project: Option<String>,
    /// Move it to an already-resolved project (sync). Overrides `project`.
    pub project_id: Option<i64>,
    /// Move it to a space, with no project. Overrides the two above.
    pub space_id: Option<i64>,
    /// The kind as is, if this version doesn't know it (sync).
    pub kind_text: Option<String>,
    /// Why it changes: stored with the previous version.
    pub reason: Option<String>,
    /// Partial edit (0.19.0): literal `(find, replace)` pairs, in order. Each searched piece has to
    /// appear exactly once in the content as it is when applied; otherwise nothing changes.
    pub replace: Vec<(String, String)>,
    /// Text appended at the end of the content (one more step in a recipe), with a blank
    /// line in between.
    pub append: Option<String>,
}

/// A piece that `replace` or `append` changed, with [`CONTEXT`] characters on each side: what the
/// `update` response shows so the agent doesn't have to ask for `get`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppliedChange {
    /// The piece that was searched for (`replace`); nothing if it was `append`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub appended: bool,
    pub context: String,
}

/// Context characters on each side of what changed.
pub const CONTEXT: usize = 80;

/// A previous version of a memory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub version: i64,
    pub kind: String,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    /// When it stopped being the live version.
    pub changed_at: String,
    pub reason: Option<String>,
    /// The fields of a version this one doesn't know (from a newer omoi): stored with it
    /// (`memory_version.extra`) and written back the same, in sync, in `omoi export` and
    /// in the trash (1.9.0). They used to be lost.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Which memories are visible. From a project, its own, its space's and the global ones; never those of
/// another space. From a general folder (the user's, see `proyecto::is_general`), all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// A project (`id`; -1 if it has nothing yet) and its space.
    #[serde(rename = "Proyecto")]
    Project { id: i64, space: Option<i64> },
    /// All the projects of a space, the space's own and the global ones; with `None`, all the projects
    /// with no space and the global ones (what "all projects" sees from a project with no space).
    #[serde(rename = "Espacio")]
    Space(Option<i64>),
    #[serde(rename = "Todos")]
    All,
}

impl Scope {
    /// What is visible from a project's folder (without writing anything to the database).
    pub fn from_project(db: &Connection, project: &str) -> Result<Scope> {
        if crate::proyecto::is_general(project) {
            return Ok(Scope::All);
        }
        let (id, space) = crate::proyecto::resolve(db, project)?.unwrap_or((-1, None));
        Ok(Scope::Project { id, space })
    }

    /// "All projects" as seen from here: from a project, those of its space (or those with no
    /// space); from a general folder, all.
    pub fn widen(self) -> Scope {
        match self {
            Scope::Project { space, .. } => Scope::Space(space),
            other => other,
        }
    }

    /// The space it is viewed from (`None` if there is none, or from a general folder).
    pub fn space(&self) -> Option<i64> {
        match self {
            Scope::Project { space, .. } => *space,
            Scope::Space(s) => *s,
            Scope::All => None,
        }
    }

    /// Whether a memory (by its id) is visible from here: the same condition as `sql`.
    pub fn sees(&self, db: &Connection, id: &str) -> Result<bool> {
        Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE m.id = ?1 AND {})", self.sql()), [id], |f| f.get(0))?)
    }

    /// The project it is viewed from, if it is one.
    pub fn project(&self) -> Option<i64> {
        match self {
            Scope::Project { id, .. } if *id > 0 => Some(*id),
            _ => None,
        }
    }

    /// Whether `other` can be viewed from here: from a general folder, everything; from a project or a
    /// space, only what is in its same space (or, with no space, what has none).
    pub fn allows(&self, other: &Scope) -> bool {
        matches!(self, Scope::All) || self.space() == other.space()
    }

    /// Whether a memory of this project (`project_id`; 0 if none) and, with no project, of this space
    /// (`space_id`) is visible from here: the same condition as `sql`, for what isn't in the
    /// `memory` table (the trash).
    pub fn includes(&self, db: &Connection, project_id: i64, space_id: Option<i64>) -> Result<bool> {
        let in_space = |s: Option<i64>| project_id != 0 || space_id.is_none() || space_id == s;
        Ok(match self {
            Scope::All => true,
            Scope::Project { id, space } => (project_id == *id || project_id == 0) && in_space(*space),
            Scope::Space(s) => (project_id == 0 || crate::proyecto::space_of(db, project_id)? == *s) && in_space(*s),
        })
    }

    /// The SQL condition on memory `m` (with `m.project_id IN (…)`, which goes through the
    /// `memory_scope` index). The ids are database integers: they go in the text, no parameter needed.
    /// Without the trade memories of the trades no project in the scope has (see
    /// `oficios::ocultas`): a global one with `trade:android` doesn't reach a web project.
    pub(crate) fn sql(&self) -> String {
        let base = self.sql_without_trades();
        match crate::oficios::ocultas(self) {
            Some(o) => format!("({base} AND (m.project_id <> 0 OR m.space_id IS NOT NULL OR m.n NOT IN ({o})))"),
            None => base,
        }
    }

    /// `sql` counting trade memories as global: for `memory_cuenta` (which doesn't have the
    /// tags); the caller subtracts `oficios::cuantas_ocultas`.
    fn sql_without_trades(&self) -> String {
        let space_cond = |s: Option<i64>| match s {
            Some(s) => format!("(m.project_id <> 0 OR m.space_id IS NULL OR m.space_id = {s})"),
            None => "(m.project_id <> 0 OR m.space_id IS NULL)".to_string(),
        };
        match self {
            Scope::Project { id, space } => format!("(m.project_id IN ({id}, 0) AND {})", space_cond(*space)),
            Scope::Space(Some(s)) => format!("(m.project_id IN (SELECT 0 UNION ALL SELECT id FROM project WHERE space_id = {s}) AND {})", space_cond(Some(*s))),
            Scope::Space(None) => format!("(m.project_id IN (SELECT id FROM project WHERE space_id IS NULL) AND {})", space_cond(None)),
            Scope::All => "1".to_string(),
        }
    }
}

pub(crate) const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// `INDEXED BY memory_ambito` to join `memory m` by `n` with another table (`memory_fts`,
/// `memory_vec`) and filter by scope without reading each memory's whole row; nothing if the connection
/// doesn't have the index (see `db::LOCALES`). It only changes where SQLite reads from, not the output.
pub(crate) fn scope_index_hint(db: &Connection) -> &'static str {
    let has_index = db
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'memory_ambito')", [], |f| f.get::<_, bool>(0))
        .unwrap_or(false);
    if has_index { "INDEXED BY memory_ambito" } else { "" }
}

/// Whether all memories are visible from `scope` and the word index has one row per memory (the
/// two counts match). Then counting or scoring in `memory_fts` without joining `memory` gives the
/// same as filtering by scope, and costs much less: with 100,000 memories, counting how many memories
/// a word appears in goes from 3 to 0.3 ms. That is the usual case from the user's folder (all are visible) or with
/// a single project. When in doubt (no counts, or they don't match), `false`: it joins, as always.
pub(crate) fn sees_all(db: &Connection, scope: &Scope) -> bool {
    let counts = || -> Option<bool> {
        let all: i64 = db.query_row("SELECT COALESCE(sum(n), 0) FROM memory_cuenta", [], |f| f.get(0)).ok()?;
        let here: i64 = match scope {
            Scope::All => all,
            a => {
                let n: i64 = db.query_row(&format!("SELECT COALESCE(sum(m.n), 0) FROM memory_cuenta m WHERE {}", a.sql_without_trades()), [], |f| f.get(0)).ok()?;
                n - crate::oficios::cuantas_ocultas(db, a).ok()?
            }
        };
        Some(here == all && fts_row_count(db)? == all)
    };
    counts().unwrap_or(false)
}

/// How many rows `memory_fts` has: the first number of the FTS5 averages record (`id = 1` of
/// `memory_fts_data`, a varint like SQLite's), the one it uses for BM25. Without reading the index.
fn fts_row_count(db: &Connection) -> Option<i64> {
    let b: Vec<u8> = db.query_row("SELECT block FROM memory_fts_data WHERE id = 1", [], |f| f.get(0)).ok()?;
    let mut v: u64 = 0;
    for (i, x) in b.iter().enumerate().take(9) {
        if i == 8 {
            return Some(((v << 8) | u64::from(*x)) as i64);
        }
        v = (v << 7) | u64::from(x & 0x7f);
        if x & 0x80 == 0 {
            return Some(v as i64);
        }
    }
    None
}
/// The columns of a memory, over `memory m`. The project and space ones, with subqueries by
/// primary key: only evaluated for the rows returned (after the `LIMIT`). The last ones,
/// the verification (`verificado`) and the provenance (`procedencia`) from `memory.extra` (or NULL): the
/// `instr` avoids reading the JSON of those without it, which is almost all of them.
pub(crate) const COLUMNS: &str = "m.id, COALESCE((SELECT path FROM project WHERE id = m.project_id), ''), m.kind, m.title, m.content, m.paths, m.tags, m.pinned, m.source, m.created_at, m.updated_at, m.recalled_at, m.recall_count, m.author, m.project_id, COALESCE((SELECT key FROM project WHERE id = m.project_id), ''), COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), ''), CASE WHEN instr(m.extra, 'verified') > 0 AND json_valid(m.extra) AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'sello_ajeno:' || m.id AND v = json_extract(m.extra, '$.verified.by') || char(10) || json_extract(m.extra, '$.verified.at')) THEN json_extract(m.extra, '$.verified') END, CASE WHEN instr(m.extra, 'provenance') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.provenance') END, CASE WHEN instr(m.extra, 'evidence') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.evidence') END, CASE WHEN instr(m.extra, 'review_reason') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.review_reason') END";
/// How many columns there are: whatever is requested after `COLUMNS` starts at this index.
pub(crate) const NCOL: usize = 21;

pub(crate) fn from_row(f: &Row) -> rusqlite::Result<Memory> {
    let list = |i: usize| -> rusqlite::Result<Vec<String>> {
        let s: String = f.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let kind: String = f.get(2)?;
    let parsed = kind.parse::<Kind>().ok();
    let (title, content): (String, String) = (f.get(3)?, f.get(4)?);
    let verified = crate::verificado::from_column(f.get::<_, Option<String>>(17)?.as_deref(), &kind, &title, &content);
    let tags = list(6)?;
    let project_id: i64 = f.get(14)?;
    // What it may carry depending on where it is now (a global one doesn't show a client's branch).
    let provenance = crate::procedencia::for_memory(crate::procedencia::from_column(f.get::<_, Option<String>>(18)?.as_deref()), project_id, &tags);
    Ok(Memory {
        id: f.get(0)?,
        project: f.get(1)?,
        project_id,
        project_key: f.get(15)?,
        space: f.get(16)?,
        kind: parsed.unwrap_or(Kind::Fact),
        kind_text: if parsed.is_none() { Some(kind) } else { None },
        title,
        content,
        paths: list(5)?,
        tags,
        pinned: f.get(7)?,
        source: f.get(8)?,
        created_at: f.get(9)?,
        updated_at: f.get(10)?,
        recalled_at: f.get(11)?,
        recall_count: f.get(12)?,
        author: f.get(13)?,
        content_hlc: String::new(),
        redacted: Vec::new(),
        verified,
        provenance,
        evidence: crate::evidencia::from_column(f.get::<_, Option<String>>(19)?.as_deref()),
        review_reason: f.get::<_, Option<String>>(20)?.filter(|r| !r.trim().is_empty()),
        unverified: None,
    })
}

pub(crate) fn write_tx(db: &mut Connection) -> Result<Transaction<'_>> {
    Ok(db.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

// --- Normalizing what comes in ---------------------------------------------------------------------

fn clean_title(t: &str) -> Result<String> {
    // One line: line breaks become spaces.
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!t.is_empty(), tr!("the title can't be empty", "el título no puede estar vacío"));
    ensure!(t.chars().count() <= 200, tr!("the title is over 200 characters: put the details in the content", "el título pasa de 200 caracteres: lo largo va en el contenido"));
    Ok(t)
}

/// What is written here (not what comes from sync nor what comes back from the trash) goes
/// through `secretos::redactar`, unless `OMOIKANE_REDACT=0`. Records the kind of each one in `found`.
fn strip_secrets(text: &str, found: &mut Vec<&'static str>) -> String {
    if !crate::secretos::activo() {
        return text.to_string();
    }
    let (s, hs) = crate::secretos::redactar(text);
    found.extend(hs.iter().map(|h| h.tipo));
    s
}

fn clean_content(c: &str) -> Result<String> {
    let c = c.trim().to_string();
    ensure!(!c.is_empty(), tr!("the content can't be empty", "el contenido no puede estar vacío"));
    Ok(c)
}

/// How title, content, paths and tags would end up when changed here (with secret
/// redaction), without writing anything: to know whether an incoming change is already applied (the round trip from
/// Obsidian, `obsidian::importar`). What gets masked goes to `redacted_kinds`.
pub(crate) fn as_stored(title: &str, content: &str, paths: &[String], tags: &[String], redacted_kinds: &mut Vec<&'static str>) -> Result<(String, String, Vec<String>, Vec<String>)> {
    let t = clean_title(&strip_secrets(title, redacted_kinds))?;
    let c = clean_content(&strip_secrets(&clean_content(content)?, redacted_kinds))?;
    Ok((t, c, norm_paths(paths), norm_tags(tags)))
}

/// Paths relative to the project root, with `/` and no leading `./`.
fn norm_paths(ps: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ps
        .iter()
        .map(|p| p.trim().replace('\\', "/"))
        .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn norm_tags(ts: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ts
        .iter()
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// What goes from `from` to `to` in `text` with [`CONTEXT`] characters on each side, and «…» where
/// it is cut.
fn around(text: &str, from: usize, to: usize) -> String {
    let start = text[..from].char_indices().rev().nth(CONTEXT - 1).map(|(i, _)| i).unwrap_or(0);
    let end = text[to..].char_indices().nth(CONTEXT).map(|(i, _)| to + i).unwrap_or(text.len());
    let (before, after) = (if start > 0 { "…" } else { "" }, if end < text.len() { "…" } else { "" });
    format!("{before}{}{after}", &text[start..end])
}

/// Applies to `content` the `replace`s (each one exactly once, in order) and the `append` of
/// some `Changes`. Returns the changed pieces with their context; with a `find` that appears 0 or
/// several times, an error with the count, and the content is left untouched.
fn apply_edit(content: &mut String, c: &Changes) -> Result<Vec<AppliedChange>> {
    let mut new_text = content.clone();
    let mut changed = Vec::new();
    for (find, with) in &c.replace {
        ensure!(!find.is_empty(), tr!("replace: `find` can't be empty", "replace: `find` no puede estar vacío"));
        let count = new_text.matches(find.as_str()).count();
        ensure!(
            count == 1,
            tr!(
                "replace: «{find}» appears {count} times in the content (it has to appear exactly once); nothing changed",
                "replace: «{find}» aparece {count} veces en el contenido (tiene que aparecer exactamente una); no se ha cambiado nada"
            )
        );
        let from = new_text.find(find.as_str()).unwrap_or(0);
        new_text.replace_range(from..from + find.len(), with);
        changed.push(AppliedChange { find: Some(find.clone()), appended: false, context: around(&new_text, from, from + with.len()) });
    }
    if let Some(a) = c.append.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        let base = new_text.trim_end().to_string();
        let from = base.len() + 2;
        new_text = format!("{base}\n\n{a}");
        changed.push(AppliedChange { find: None, appended: true, context: around(&new_text, from, new_text.len()) });
    }
    *content = new_text;
    Ok(changed)
}

/// The title and content `m` would have with these changes (`title`, `content`, `replace`, `append`),
/// without writing anything: to look at the final text before saving it (`oficios::al_cambiar`) or to
/// save it in another memory (`oficios::crear_de_oficio`).
pub fn text_after(m: &Memory, c: &Changes) -> Result<(String, String)> {
    let new_title = match &c.title {
        Some(t) => clean_title(t)?,
        None => m.title.clone(),
    };
    let mut content = match &c.content {
        Some(t) => clean_content(t)?,
        None => m.content.clone(),
    };
    apply_edit(&mut content, c)?;
    Ok((new_title, clean_content(&content)?))
}

// --- Indexes ---------------------------------------------------------------------------------------

fn index_memory(tx: &Transaction, n: i64, m: &Memory) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
    )?;
    tx.execute("DELETE FROM memory_path WHERE memory_id = ?1", [&m.id])?;
    for p in &m.paths {
        tx.execute("INSERT INTO memory_path(memory_id, path) VALUES (?1, ?2)", params![m.id, p])?;
    }
    Ok(())
}

fn unindex_memory(tx: &Transaction, n: i64) -> Result<()> {
    tx.execute("DELETE FROM memory_fts WHERE rowid = ?1", [n])?;
    Ok(())
}

// --- Operations ------------------------------------------------------------------------------------

/// Saves without checking whether a similar one already exists: that is `vectores::duplicada`, beforehand, so the
/// caller decides. With `model`, the vector goes in the same transaction.
pub fn save(db: &mut Connection, new: NewMemory, model: Option<&Modelo>) -> Result<Memory> {
    insert(db, &ulid::Ulid::generate().to_string(), new, None, model)
}

/// Like [`save`], but with the id it brings (a memory coming back from an OKF bundle exported from
/// Omoikane, `okf::importar`): goes through secret redaction and gets a stamp from here.
pub(crate) fn save_with_its_id(db: &mut Connection, id: &str, new: NewMemory, model: Option<&Modelo>) -> Result<Memory> {
    let id = ulid::Ulid::from_string(id).map_err(|e| anyhow!("{id}: {e}"))?.to_string();
    insert(db, &id, new, None, model)
}

/// The topic key tag: `topic:<key>`. One memory per key and per scope (project,
/// space or global): `remember` with a key updates the one that already has it instead of creating another.
pub const TOPIC_TAG: &str = "topic:";

/// What [`save_by_key`] did.
#[derive(Clone, Debug)]
pub struct SavedByKey {
    pub memory: Memory,
    /// `true` if there was none with that key in the scope and it was created.
    pub created: bool,
    /// When created: other keys in the scope that look like the new one (maybe one of them was meant).
    pub similar_keys: Vec<String>,
    /// What was masked in the data for looking like a secret (their kinds).
    pub redacted: Vec<&'static str>,
}

/// The key as stored: lowercase, no surrounding spaces, and only letters, digits and `-_.:/`
/// (fits in a tag and is written the same in the CLI and in the MCP).
pub fn topic_key(key: &str) -> Result<String> {
    let c = key.trim().to_lowercase();
    ensure!(
        !c.is_empty() && c.chars().count() <= 80 && c.chars().all(|x| x.is_alphanumeric() || "-_.:/".contains(x)),
        tr!(
            "key «{key}»: use letters, digits and - _ . : / (up to 80), e.g. kumi-profile",
            "clave «{key}»: usa letras, cifras y - _ . : / (hasta 80), p. ej. kumi-perfil"
        )
    );
    Ok(c)
}

/// The topic key of a memory, if it has one.
pub fn key_of(m: &Memory) -> Option<&str> {
    m.tags.iter().find_map(|t| t.strip_prefix(TOPIC_TAG))
}

/// Saves by key: if in the same scope (the project, the space or global, exactly the right one)
/// there is already a memory with `topic:<key>`, updates it (title, content, kind; merges tags and
/// paths); otherwise creates it. All in one `BEGIN IMMEDIATE` transaction that searches inside: two processes
/// at once with the same key leave a single memory (the second one waits and finds it). Without checking
/// duplicates: the key already says which one it is. The `data`, if given, go to `memory.extra` like those of
/// [`set_data`] and in the same transaction (without them, whatever it had stays); as there, changing
/// only the data doesn't create a version, and the same thing again (text and data) changes nothing.
pub fn save_by_key(db: &mut Connection, key: &str, mut new: NewMemory, data: Option<serde_json::Value>, model: Option<&Modelo>) -> Result<SavedByKey> {
    let key = topic_key(key)?;
    let mut redacted_kinds = Vec::new();
    let data = ready_data(data, &mut redacted_kinds)?;
    let tag = format!("{TOPIC_TAG}{key}");
    new.tags.retain(|t| !t.trim().to_lowercase().starts_with(TOPIC_TAG));
    new.tags.push(tag.clone());
    let tx = write_tx(db)?;
    let (project_id, space_id) = placement(&tx, &new.project, new.project_id, new.space_id)?;
    // A global one with `trade:<trade>` belongs to that trade's scope (the default path of the web
    // factory); with no trade, to the person. Each combination of trades (and of who for, `trade-for:`) is its
    // own scope.
    let trades = serde_json::to_string(&trade_scope(&norm_tags(&new.tags)))?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM memory WHERE project_id = ?1 AND space_id IS ?2 AND tags LIKE ?3
               AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?4)
               AND (SELECT COALESCE(json_group_array(value), '[]') FROM (SELECT value FROM json_each(memory.tags)
                    WHERE value LIKE 'trade:%' OR value LIKE 'trade-for:%' ORDER BY value)) = ?5
             ORDER BY updated_at DESC, id DESC LIMIT 1",
            params![project_id, space_id, format!("%\"{tag}\"%"), tag, trades],
            |f| f.get(0),
        )
        .optional()?;
    let r = match existing {
        Some(id) => {
            let before = read(&tx, &id)?;
            let mut tags = before.tags.clone();
            tags.extend(new.tags.iter().cloned());
            let mut paths = before.paths.clone();
            paths.extend(new.paths.iter().cloned());
            let c = Changes {
                title: Some(new.title),
                content: Some(new.content),
                kind: new.kind,
                paths: Some(paths),
                tags: Some(tags),
                pinned: new.pinned.then_some(true),
                reason: Some(format!("key {key}")),
                ..Default::default()
            };
            let (m, _) = update_in(&tx, &id, c, None, model)?;
            SavedByKey { memory: m, created: false, similar_keys: Vec::new(), redacted: Vec::new() }
        }
        None => {
            new.project_id = Some(project_id);
            new.space_id = space_id;
            let m = insert_in(&tx, &ulid::Ulid::generate().to_string(), new, None, model)?;
            let similar = similar_keys(&tx, project_id, space_id, &key)?;
            SavedByKey { memory: m, created: true, similar_keys: similar, redacted: Vec::new() }
        }
    };
    let data_changed = match data {
        Some(d) => set_data_in(&tx, &r.memory.id, Some(d))?,
        None => false,
    };
    let r = if data_changed { SavedByKey { memory: read(&tx, &r.memory.id)?, redacted: redacted_kinds, ..r } } else { SavedByKey { redacted: redacted_kinds, ..r } };
    tx.commit()?;
    Ok(r)
}

/// The keys in the same scope that look like `key` without being it: equal ignoring `-_.` and
/// case, or one or two letters away (changed, extra or missing) with 6 letters or more.
fn similar_keys(db: &Connection, project_id: i64, space_id: Option<i64>, key: &str) -> Result<Vec<String>> {
    let mut q = db.prepare(
        "SELECT DISTINCT substr(t.value, ?3) FROM memory m, json_each(m.tags) t
         WHERE m.project_id = ?1 AND m.space_id IS ?2 AND m.tags LIKE '%\"topic:%' AND t.value LIKE 'topic:%'",
    )?;
    let keys: Vec<String> = q.query_map(params![project_id, space_id, TOPIC_TAG.len() as i64 + 1], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    let flat = |s: &str| s.chars().filter(|c| !"-_.".contains(*c)).collect::<String>();
    let (pc, n) = (flat(key), key.chars().count());
    Ok(keys
        .into_iter()
        .filter(|k| k != key && (flat(k) == pc || (n >= 6 && edit_distance(k, key) <= if n >= 12 { 2 } else { 1 })))
        .collect())
}

/// Edit distance (Levenshtein) by characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == *cb { prev } else { 1 + prev.min(row[j]).min(row[j + 1]) };
            prev = cur;
        }
    }
    row[b.len()]
}

/// The tags that give the trade scope of a keyed memory, sorted: its trades
/// (`trade:web`, …) and who for (`trade-for:client`).
fn trade_scope(tags: &[String]) -> Vec<String> {
    let mut v: Vec<String> = tags.iter().filter(|t| t.starts_with(crate::oficios::PREFIJO) || t.starts_with(crate::oficios::PARA)).cloned().collect();
    v.sort();
    v
}

/// The trade tags of a list of tags, sorted (`trade:web`, …).
fn trade_tags(tags: &[String]) -> Vec<String> {
    let mut v: Vec<String> = tags.iter().filter(|t| t.starts_with(crate::oficios::PREFIJO)).cloned().collect();
    v.sort();
    v
}

/// Which level a keyed memory belongs to, from most specific to most general.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Project,
    Space,
    Trade,
    Global,
}

/// The level of a memory: a project's, a space's, a trade's (global with `trade:`) or global.
pub fn level_of(m: &Memory) -> Level {
    if m.project_id != 0 {
        Level::Project
    } else if m.space_only() {
        Level::Space
    } else if crate::oficios::lleva_oficio(&m.tags) {
        Level::Trade
    } else {
        Level::Global
    }
}

/// All the memories with this key the scope sees, from most specific to most general: the
/// project's, its space's, its trades' and the global one. Deterministic, no search: the caller
/// combines them (for example, a project's path through Kumi's megafactory). What the scope doesn't
/// see (another client, another project) doesn't come out.
pub fn resolve_key(db: &Connection, scope: &Scope, key: &str) -> Result<Vec<(Level, Memory)>> {
    let key = topic_key(key)?;
    let mut v: Vec<(Level, Memory)> = with_tag(db, scope, &format!("{TOPIC_TAG}{key}"))?.into_iter().map(|m| (level_of(&m), m)).collect();
    // Two computers that created the same key without seeing each other leave, after syncing, two memories in the same
    // scope: one wins, the same one on every computer ([`key_winner`]), and the other doesn't come out.
    v.sort_by(|a, b| key_winner(&b.1, &a.1));
    let mut seen = std::collections::HashSet::new();
    v.retain(|(_, m)| seen.insert((m.project_id, m.space.clone(), trade_scope(&m.tags))));
    v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| trade_scope(&a.1.tags).cmp(&trade_scope(&b.1.tags))).then_with(|| a.1.created_at.cmp(&b.1.created_at)));
    Ok(v)
}

/// Between two memories with the same key in the same scope, which one wins: the one changed later and, if
/// equal, the one with the larger id (`Greater`: `a`). Same as the `ORDER BY` of [`save_by_key`], so
/// `remember --key` changes the same one `get --key` returns.
fn key_winner(a: &Memory, b: &Memory) -> std::cmp::Ordering {
    a.updated_at.cmp(&b.updated_at).then_with(|| a.id.cmp(&b.id))
}

/// The start of the reason of the verdict that [`resolve_repeated_keys`] sets.
const REPEATED_KEY_REASON: &str = "same key ";

/// After syncing or importing: the keys repeated in the same scope (two computers that did
/// `remember --key` with the same key without seeing each other; on each one, the key is atomic). The one
/// [`key_winner`] picks wins and the others are superseded by it (`superseded`, a verdict that travels and that
/// every computer sets the same way), so that Review proposes forgetting them. Nothing is deleted or changed:
/// `get --key` already returns only the winner. Returns how many were superseded now.
pub fn resolve_repeated_keys(db: &Connection) -> Result<usize> {
    let mut q = db.prepare("SELECT id, project_id, space_id, tags, updated_at FROM memory WHERE tags LIKE '%\"topic:%'")?;
    let rows: Vec<(String, i64, Option<i64>, String, String)> =
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    type Group = (i64, Option<i64>, String, Vec<String>);
    let mut groups: std::collections::BTreeMap<Group, Vec<Memory>> = std::collections::BTreeMap::new();
    for (id, project_id, space_id, tags, updated_at) in rows {
        let tags: Vec<String> = serde_json::from_str(&tags).unwrap_or_default();
        let m = Memory { id, project_id, tags, updated_at, ..Default::default() };
        if let Some(key) = key_of(&m).map(str::to_string) {
            groups.entry((project_id, space_id, key, trade_scope(&m.tags))).or_default().push(m);
        }
    }
    let active = crate::veredictos::pairs(db)?;
    let mut n = 0;
    for ((_, _, key, _), mut v) in groups.into_iter().filter(|(_, v)| v.len() > 1) {
        v.sort_by(|a, b| key_winner(b, a));
        let winner = &v[0];
        for other in &v[1..] {
            // With any active verdict the pair is already judged, as in `revisar::trade_duplicates`:
            // after "it's fine as is" (`compatible`) the next sync or import set it as superseded again.
            // Only the one that this same code set the other way round is flipped (the losing one was changed later
            // and now it is the winner).
            let reversed = |x: &crate::veredictos::Verdict| x.relation == crate::veredictos::SUPERSEDED && x.memory_id == winner.id && x.reason.starts_with(REPEATED_KEY_REASON);
            if active.get(&crate::veredictos::pair_key(&other.id, &winner.id)).is_some_and(|x| !reversed(x)) {
                continue;
            }
            let author = crate::veredictos::author(db, "sync", &other.id);
            crate::veredictos::set(db, &other.id, &winner.id, crate::veredictos::SUPERSEDED, &format!("{REPEATED_KEY_REASON}{key} written on two computers"), &author)?;
            n += 1;
        }
    }
    Ok(n)
}

/// A layer of [`resolve_key`] as it comes out in `get --key … --resolve --json`: its scope
/// (`project`, `space`, `trade:<trade>` or `global`), the memory, its text and its data (those of
/// `memory.extra`, [`set_data`]), without merging anything with the other layers.
#[derive(Clone, Debug, Serialize)]
pub struct Layer {
    pub scope: String,
    pub id: String,
    pub title: String,
    pub text: String,
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub updated_at: String,
}

/// The layers of a key for the scope, from most specific to most general.
pub fn key_layers(db: &Connection, scope: &Scope, key: &str) -> Result<Vec<Layer>> {
    let layers = resolve_key(db, scope, key)?;
    let mut data = data_of(db, &layers.iter().map(|(_, m)| m.id.as_str()).collect::<Vec<_>>())?;
    Ok(layers
        .into_iter()
        .map(|(level, m)| {
            let scope = match level {
                Level::Project => "project".to_string(),
                Level::Space => "space".to_string(),
                Level::Global => "global".to_string(),
                Level::Trade => format!(
                    "trade:{}",
                    crate::oficios::de_etiquetas(&trade_tags(&m.tags)).join(",")
                ),
            };
            Layer {
                scope,
                data: data.remove(&m.id),
                id: m.id,
                title: m.title,
                text: m.content,
                project: if level == Level::Project { m.project } else { String::new() },
                space: if level == Level::Space { m.space } else { String::new() },
                updated_at: m.updated_at,
            }
        })
        .collect())
}

/// The memory with this key that wins in the scope: the most specific one from [`resolve_key`].
pub fn by_key(db: &Connection, scope: &Scope, key: &str) -> Result<Option<Memory>> {
    Ok(resolve_key(db, scope, key)?.into_iter().next().map(|(_, m)| m))
}

/// Where a memory coming in through sync comes from: its id, dates, stamps and author of origin.
pub(crate) struct Origin<'a> {
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub hlc: &'a str,
    /// The stamp of its last text change on the origin computer (empty: the `hlc` one, if the text
    /// changes; sent by versions before schema 10 and by lines where they match).
    pub content_hlc: &'a str,
    pub author: &'a str,
    /// The fields of the line this version doesn't know, in JSON (`memory.extra`): the memory
    /// stores them and sync and `omoi export` write them back.
    pub extra: &'a str,
}

/// Where a memory goes: (project, space). A space goes with no project; a folder is registered as a
/// project if it isn't one yet.
fn placement(tx: &Connection, project: &str, project_id: Option<i64>, space_id: Option<i64>) -> Result<(i64, Option<i64>)> {
    Ok(match (space_id, project_id) {
        (Some(s), _) => (0, Some(s)),
        (None, Some(p)) => (p, None),
        (None, None) => (crate::proyecto::register(tx, project)?, None),
    })
}

/// For sync: the same memory, with the same id, dates, stamp and author as on its computer.
pub(crate) fn save_with_id(db: &mut Connection, id: &str, new: NewMemory, o: &Origin, model: Option<&Modelo>) -> Result<Memory> {
    insert(db, id, new, Some(o), model)
}

/// The stamp of a memory (empty if it doesn't exist).
pub(crate) fn hlc_of(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Records a forget mark, unless there is already a later one, with the space (its key; empty: of
/// none) of what was forgotten: the forget goes only to that space's repository.
pub(crate) fn mark_forgotten(db: &Connection, id: &str, deleted_at: Option<&str>, hlc: &str, space_key: &str) -> Result<()> {
    let cur: Option<String> = db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    if cur.is_none_or(|a| crate::reloj::is_after(hlc, &a)) {
        db.execute(
            &format!("INSERT OR REPLACE INTO tombstone(id, deleted_at, hlc, space_key) VALUES (?1, COALESCE(?2, {NOW}), ?3, ?4)"),
            params![id, deleted_at, hlc, space_key],
        )?;
    }
    Ok(())
}

/// The author recorded on what is saved here in the space `space_key` (its key; empty: in
/// none): `OMOIKANE_AUTHOR` (an agent with no person in front says who it is); otherwise, the one of the repository
/// of that space if this computer has it (`meta` `autor:<space>`, empty if it joined without an author), and if
/// not, the one of the general repository (`meta` `autor`). So the name someone joined a
/// client with never signs another client's things nor their own; empty = none.
pub(crate) fn author_in(db: &Connection, space_key: &str) -> Result<String> {
    if let Some(a) = crate::entorno::author() {
        return Ok(a);
    }
    if !space_key.is_empty()
        && let Some(a) = db.query_row("SELECT v FROM meta WHERE k = ?1", [format!("autor:{space_key}")], |f| f.get(0)).optional()?
    {
        return Ok(a);
    }
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'autor'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// The author of what is saved in this project or space (`author_in` with the space it belongs to).
pub(crate) fn author_of(db: &Connection, project_id: i64, space_id: Option<i64>) -> Result<String> {
    if crate::entorno::author().is_some() {
        return author_in(db, "");
    }
    let space = match space_id {
        Some(s) => Some(s),
        None => crate::proyecto::space_of(db, project_id)?,
    };
    author_in(db, &crate::proyecto::space_key(db, space)?)
}

fn insert(db: &mut Connection, id: &str, new: NewMemory, origin: Option<&Origin>, model: Option<&Modelo>) -> Result<Memory> {
    let tx = write_tx(db)?;
    let m = insert_in(&tx, id, new, origin, model)?;
    tx.commit()?;
    Ok(m)
}

/// `insert` inside an already open transaction (the caller commits).
fn insert_in(tx: &Transaction, id: &str, new: NewMemory, origin: Option<&Origin>, model: Option<&Modelo>) -> Result<Memory> {
    let kind = new.kind.with_context(|| tr!("missing kind: fact, decision, gotcha, preference or procedure", "falta el tipo: fact, decision, gotcha, preference o procedure"))?;
    // Redacted before validating: what counts for the title's 200 characters is what is stored.
    let mut redacted = Vec::new();
    let (title, content) = match origin {
        None => (clean_title(&strip_secrets(&new.title, &mut redacted))?, clean_content(&strip_secrets(&new.content, &mut redacted))?),
        Some(_) => (clean_title(&new.title)?, clean_content(&new.content)?),
    };
    let (paths, tags) = (norm_paths(&new.paths), norm_tags(&new.tags));
    if origin.is_none() {
        crate::revisar::check_review_date(&tags)?;
    }
    let (project_id, space_id) = placement(tx, &new.project, new.project_id, new.space_id)?;
    let (hlc, author) = match origin {
        Some(o) => {
            crate::reloj::receive(tx, o.hlc)?;
            (o.hlc.to_string(), o.author.to_string())
        }
        None => (crate::reloj::stamp(tx)?, author_of(tx, project_id, space_id)?),
    };
    let content_hlc = match origin {
        Some(o) if !o.content_hlc.is_empty() => o.content_hlc.to_string(),
        _ => hlc.clone(),
    };
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project_id, space_id, kind, title, content, paths, tags, pinned, source, created_at, updated_at, hlc, author, content_hlc, extra)
             VALUES (?1, ?2, ?14, ?3, ?4, ?5, ?6, ?7, ?8, ?9, COALESCE(?10, {NOW}), COALESCE(?11, {NOW}), ?12, ?13, ?15, ?16)"
        ),
        params![
            id,
            project_id,
            new.kind_text.as_deref().unwrap_or(kind.as_str()),
            title,
            content,
            serde_json::to_string(&paths)?,
            serde_json::to_string(&tags)?,
            new.pinned,
            new.source,
            origin.map(|o| o.created_at),
            origin.map(|o| o.updated_at),
            hlc,
            author,
            space_id,
            content_hlc,
            // The provenance, only what applies depending on where it lands (see `procedencia::for_memory`).
            match (origin, crate::procedencia::for_memory(new.provenance.clone(), project_id, &tags)) {
                (Some(o), _) => o.extra.to_string(),
                (None, p) => {
                    let mut extra = serde_json::Map::new();
                    if let Some(p) = p {
                        extra.insert(crate::procedencia::KEY.into(), serde_json::to_value(p)?);
                    }
                    crate::evidencia::add(&mut extra, &new.evidence);
                    if let Some(r) = new.review_reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                        extra.insert(REVIEW_REASON_KEY.into(), r.into());
                    }
                    serde_json::Value::Object(extra).to_string()
                }
            },
        ],
    )?;
    let n = tx.last_insert_rowid();
    let m = read(tx, id)?;
    index_memory(tx, n, &m)?;
    if let Some(model) = model {
        vectores::guardar(tx, n, model, &m.title, &m.content)?;
    }
    Ok(Memory { redacted, ..m })
}

pub(crate) fn read(db: &Connection, id: &str) -> Result<Memory> {
    db.query_row(&format!("SELECT {COLUMNS} FROM memory m WHERE m.id = ?1"), [id], from_row)
        .optional()?
        .ok_or_else(|| anyhow!(tr!("no memory with id {id}", "no existe la memoria {id}")))
}

/// Where a memory from here is, live or in the trash: whether it is global (neither project nor space) and the
/// key of its space (its own or its project's; empty if none). `None` if it isn't here.
pub(crate) fn memory_placement(db: &Connection, id: &str) -> Result<Option<(bool, String)>> {
    let live: Option<(bool, String)> = db
        .query_row(
            "SELECT m.project_id = 0 AND m.space_id IS NULL,
                    COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '')
             FROM memory m WHERE m.id = ?1",
            [id],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?;
    if live.is_some() {
        return Ok(live);
    }
    let Some(text) = db.query_row("SELECT memory FROM forgotten WHERE id = ?1", [id], |f| f.get::<_, String>(0)).optional()? else { return Ok(None) };
    let global = crate::sync::from_trash_json(&text).is_some_and(|(m, _, _)| m.project_key.is_empty() && m.project.is_empty() && m.space.is_empty());
    Ok(Some((global, crate::sync::trash_space(db, &text)?)))
}

/// Full id from the id or its start (what the CLI shows is enough).
pub fn resolve_id(db: &Connection, id: &str) -> Result<String> {
    let id = id.trim().to_uppercase();
    ensure!(id.len() >= 4, tr!("id too short: «{id}»", "id demasiado corto: «{id}»"));
    ensure!(id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let mut q = db.prepare("SELECT id FROM memory WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
    let ids: Vec<String> = q.query_map([&id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    match ids.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!(tr!("no memory with id {id}", "no existe ninguna memoria con id {id}")),
        _ => bail!(tr!("several memories start with {id}: type more characters", "hay varias memorias que empiezan por {id}: escribe más caracteres")),
    }
}

pub fn get(db: &Connection, id: &str) -> Result<Memory> {
    read(db, &resolve_id(db, id)?)
}

/// Error if memory `id` (the full id, live) isn't visible from `base`: from a project, only those of
/// its space (or, with no space, those without one), the global ones and the trade ones of a trade that
/// some project of that space has; from a general folder, all. For what is requested by id (`get`,
/// `history`, `update`, `forget`): so nobody pulls out another client's things even with their id.
pub fn in_scope(db: &Connection, base: &Scope, id: &str) -> Result<()> {
    if matches!(base, Scope::All) {
        return Ok(());
    }
    if crate::oficios::oculta(db, &base.widen(), id)? {
        bail!(tr!(
            "that memory is a trade memory of a trade no project here has (see `context`): it is not in this scope",
            "esa memoria es de un oficio que no tiene ningún proyecto de aquí (ver `context`): no es de este ámbito"
        ));
    }
    let own = crate::proyecto::space_key(db, base.space())?;
    match memory_placement(db, id)? {
        Some((global, space_key)) if !global && space_key != own => bail!(tr!(
            "that memory is not in this scope: it belongs to another space, and from this project you only reach its own space and the global memories; open the session in that project, or in the home folder",
            "esa memoria no es de este ámbito: es de otro espacio, y desde este proyecto solo se llega a su espacio y a las globales; abre la sesión en ese proyecto, o en tu carpeta de usuario"
        )),
        _ => Ok(()),
    }
}

/// The history of a memory seen from `base`: error if the memory isn't visible from there
/// ([`in_scope`]); empty if it is one of those that a pre-1.8.0 version promoted from a client
/// with the same id (its history was client text, `oficios::historiales_solo_aqui`) and it isn't
/// viewed from a general folder.
pub fn history_in(db: &Connection, base: &Scope, id: &str) -> Result<Vec<Version>> {
    let id = resolve_id(db, id)?;
    in_scope(db, base, &id)?;
    if !matches!(base, Scope::All) && crate::oficios::historial_solo_aqui(db, &id)? {
        return Ok(Vec::new());
    }
    history(db, &id)
}

pub fn update(db: &mut Connection, id: &str, c: Changes, model: Option<&Modelo>) -> Result<Memory> {
    Ok(update_with_changes(db, id, c, model)?.0)
}

/// Like `update`, and also returns what `replace` and `append` changed, with their context.
pub fn update_with_changes(db: &mut Connection, id: &str, c: Changes, model: Option<&Modelo>) -> Result<(Memory, Vec<AppliedChange>)> {
    update_from(db, id, c, None, model)
}

/// For sync: the change coming from another computer, with its dates, stamp and author, all in
/// the same transaction (the local version stays in the history).
pub(crate) fn update_with_origin(db: &mut Connection, id: &str, c: Changes, o: &Origin, model: Option<&Modelo>) -> Result<Memory> {
    Ok(update_from(db, id, c, Some(o), model)?.0)
}

fn update_from(db: &mut Connection, id: &str, c: Changes, origin: Option<&Origin>, model: Option<&Modelo>) -> Result<(Memory, Vec<AppliedChange>)> {
    let id = resolve_id(db, id)?;
    let tx = write_tx(db)?;
    let r = update_in(&tx, &id, c, origin, model)?;
    tx.commit()?;
    Ok(r)
}

/// `update` inside an already open transaction (the caller commits), with the
/// id already resolved.
fn update_in(tx: &Transaction, id: &str, c: Changes, origin: Option<&Origin>, model: Option<&Modelo>) -> Result<(Memory, Vec<AppliedChange>)> {
    let before = read(tx, id)?;
    let before_hlc = hlc_of(tx, id)?;
    // The change's stamp: the origin's, or a local one later than that of the version being changed.
    let hlc = match origin {
        Some(o) => {
            crate::reloj::receive(tx, o.hlc)?;
            o.hlc.to_string()
        }
        None => crate::reloj::stamp_after(tx, &before_hlc)?,
    };
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    // What comes from another computer brings the whole memory, including the fields this version doesn't
    // know; a local change leaves them as they were.
    if let Some(o) = origin {
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, o.extra])?;
    }
    // Only what the change brings, and only if it is written here: what was already there is flagged by Review.
    let mut redacted = Vec::new();
    let mut clean = |t: &str| if origin.is_none() { strip_secrets(t, &mut redacted) } else { t.to_string() };
    // The new content, and on top of it the partial edit (`replace`, `append`).
    let mut content = match &c.content {
        Some(t) => clean_content(t)?,
        None => before.content.clone(),
    };
    let changed = apply_edit(&mut content, &c)?;
    // What is new (the whole content, or what the edit brought) goes through redaction.
    if c.content.is_some() || !changed.is_empty() {
        content = clean(&content);
    }
    let after = Memory {
        title: match &c.title {
            Some(t) => clean_title(&clean(t))?,
            None => before.title.clone(),
        },
        content: clean_content(&content)?,
        kind: c.kind.unwrap_or(before.kind),
        // A new known kind clears the unknown one; an incoming unknown one brings it.
        kind_text: match (&c.kind_text, c.kind) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(_)) => None,
            (None, None) => before.kind_text.clone(),
        },
        paths: c.paths.as_deref().map(norm_paths).unwrap_or_else(|| before.paths.clone()),
        tags: c.tags.as_deref().map(norm_tags).unwrap_or_else(|| before.tags.clone()),
        pinned: c.pinned.unwrap_or(before.pinned),
        ..before.clone()
    };
    // Only the new `review:` tags: a misspelled one that came through sync (from another
    // version) must not prevent changing the memory.
    if origin.is_none() && c.tags.is_some() {
        let new_ones: Vec<String> = after.tags.iter().filter(|t| !before.tags.contains(t)).cloned().collect();
        crate::revisar::check_review_date(&new_ones)?;
    }
    // Where it ends up: what the change says (a space, an already-resolved project, a folder or global) or
    // where it was.
    let before_space: Option<i64> = tx.query_row("SELECT space_id FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let (project_id, space_id): (i64, Option<i64>) = if c.space_id.is_some() || c.project_id.is_some() || c.project.is_some() {
        placement(tx, c.project.as_deref().unwrap_or(""), c.project_id, c.space_id)?
    } else {
        (before.project_id, before_space)
    };
    let moved = project_id != before.project_id || space_id != before_space;
    // What expires verdicts: title, content or kind (pinning, tagging or moving don't).
    let judged_changed = after.title != before.title || after.content != before.content || after.kind_str() != before.kind_str();
    let text_changed = judged_changed || after.paths != before.paths || after.tags != before.tags;
    let content_hlc: Option<String> = match origin {
        Some(o) if judged_changed => Some(if o.content_hlc.is_empty() { o.hlc.to_string() } else { o.content_hlc.to_string() }),
        None if judged_changed => Some(hlc.clone()),
        _ => None,
    };
    if !text_changed {
        // Pinning or unpinning is not a content change: no new version and updated_at untouched
        // (it gives the recency in search).
        if after.pinned != before.pinned || moved || origin.is_some() {
            tx.execute(
                "UPDATE memory SET pinned = ?2, project_id = ?3, space_id = ?4, hlc = ?5 WHERE id = ?1",
                params![id, after.pinned, project_id, space_id, hlc],
            )?;
            if let Some(o) = origin {
                tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
            }
        }
        if origin.is_none() && moved {
            trim_provenance(tx, id)?;
        }
        return Ok((Memory { redacted, ..read(tx, id)? }, changed));
    }
    // The previous snapshot goes to the history.
    save_version(tx, id, &before, None, c.reason.as_deref())?;
    // The person verified the previous text, not this one: changing it from here removes the verification
    // (see `verificado`). What comes from another computer brings its own in `extra`.
    let unverified = if origin.is_none() && judged_changed { crate::verificado::remove_on_change(tx, id, &before)? } else { None };
    tx.execute(
        &format!(
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, project_id = ?8, space_id = ?11,
             updated_at = {NOW}, hlc = ?9, author = CASE WHEN author = '' THEN ?10 ELSE author END,
             content_hlc = COALESCE(?12, content_hlc) WHERE id = ?1"
        ),
        params![
            id,
            after.title,
            after.content,
            after.kind_str(),
            serde_json::to_string(&after.paths)?,
            serde_json::to_string(&after.tags)?,
            after.pinned,
            project_id,
            hlc,
            author_of(tx, project_id, space_id)?,
            space_id,
            content_hlc,
        ],
    )?;
    if let Some(o) = origin {
        tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
    }
    if origin.is_none() && (moved || after.tags != before.tags) {
        trim_provenance(tx, id)?;
    }
    unindex_memory(tx, n)?;
    let m = read(tx, id)?;
    index_memory(tx, n, &m)?;
    if after.title != before.title || after.content != before.content {
        match model {
            Some(model) => vectores::guardar(tx, n, model, &m.title, &m.content)?,
            // Without a model, the old vector is no longer valid: it is deleted and `sincronizar` will redo it.
            None => {
                tx.execute("DELETE FROM memory_vec WHERE n = ?1", [n])?;
            }
        }
    }
    Ok((Memory { redacted, unverified, ..m }, changed))
}

/// After moving it or changing its tags from here: removes from `memory.extra` the provenance that no longer
/// applies (`procedencia::for_memory`): promoted to a trade, none; global or of a space, only the
/// session. Goes in the same transaction as the change, which has already given it a new stamp.
fn trim_provenance(tx: &Transaction, id: &str) -> Result<()> {
    let (extra, project_id, tags): (String, i64, String) = tx.query_row("SELECT extra, project_id, tags FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    if !extra.contains(crate::procedencia::KEY) {
        return Ok(());
    }
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&tags).unwrap_or_default();
    if crate::procedencia::trim_extra(&mut o, project_id, &tags) {
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    }
    Ok(())
}

/// Hangs a snapshot of `m` on the history of `id`, with the number after its last one.
/// `changed_at` is when it stopped being the live version (now, if not given).
fn save_version(tx: &Transaction, id: &str, m: &Memory, changed_at: Option<&str>, reason: Option<&str>) -> Result<()> {
    let version: i64 =
        tx.query_row("SELECT COALESCE(MAX(version), 0) + 1 FROM memory_version WHERE memory_id = ?1", [id], |f| f.get(0))?;
    tx.execute(
        &format!(
            "INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, COALESCE(?8, {NOW}), ?9)"
        ),
        params![
            id,
            version,
            m.title,
            m.content,
            m.kind_str(),
            serde_json::to_string(&m.paths)?,
            serde_json::to_string(&m.tags)?,
            changed_at,
            reason.map(str::trim).filter(|r| !r.is_empty()),
        ],
    )?;
    Ok(())
}

/// Merges N memories into one (0.19.0): `id` keeps the merged content that `c` brings (written by
/// the agent or the person; Omoikane doesn't write it) and the `ids` go to the trash with a
/// `superseded` verdict towards it. Their tags and paths are added to the one that stays (so the `[[links]]`
/// that reached them by tag now resolve to this one) and their history hangs from that of the
/// one that stays, with the reason «absorbed from <id>». All in one transaction. Returns the one that stays,
/// what `replace`/`append` changed and the absorbed ids.
pub fn absorb(db: &mut Connection, id: &str, ids: &[String], mut c: Changes, model: Option<&Modelo>) -> Result<(Memory, Vec<AppliedChange>, Vec<String>)> {
    ensure!(
        c.content.is_some(),
        tr!(
            "absorb needs the merged text: write the content that joins the memories and pass it as `content` (Omoikane doesn't write it)",
            "absorb necesita el texto fundido: escribe el contenido que junta las memorias y pásalo en `content` (Omoikane no lo redacta)"
        )
    );
    let id = resolve_id(db, id)?;
    let tx = write_tx(db)?;
    let live = read(&tx, &id)?;
    let mut absorbed: Vec<Memory> = Vec::new();
    for other in ids {
        let other = resolve_id(&tx, other)?;
        ensure!(other != id, tr!("a memory can't absorb itself", "una memoria no puede absorberse a sí misma"));
        if !absorbed.iter().any(|a| a.id == other) {
            absorbed.push(read(&tx, &other)?);
        }
    }
    ensure!(!absorbed.is_empty(), tr!("absorb: no memory to absorb", "absorb: ninguna memoria que absorber"));
    // Tags and paths: those in the changes (or the live one's) plus those of the absorbed ones.
    let mut tags = c.tags.take().unwrap_or_else(|| live.tags.clone());
    let mut paths = c.paths.take().unwrap_or_else(|| live.paths.clone());
    for a in &absorbed {
        tags.extend(a.tags.iter().cloned());
        paths.extend(a.paths.iter().cloned());
    }
    c.tags = Some(tags);
    c.paths = Some(paths);
    let list = absorbed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ");
    if c.reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
        c.reason = Some(format!("absorbed {list}"));
    }
    let (m, changed) = update_in(&tx, &id, c, None, model)?;
    for a in &absorbed {
        // The verdict before deleting it: it is resolved against the live memories, and holds again if
        // someone restores it from the trash.
        crate::veredictos::set(&tx, &a.id, &id, crate::veredictos::SUPERSEDED, &format!("absorbed into {id}"), "omoikane")?;
        // Its history, from the oldest to the last live one, after that of the one that stays.
        let reason = format!("absorbed from {}", a.id);
        for v in history(&tx, &a.id)?.into_iter().rev() {
            let snapshot = Memory { title: v.title, content: v.content, kind: v.kind.parse().unwrap_or(a.kind), paths: v.paths, tags: v.tags, ..a.clone() };
            save_version(&tx, &id, &snapshot, Some(&v.changed_at), Some(&reason))?;
        }
        save_version(&tx, &id, a, None, Some(&reason))?;
        delete_in(&tx, &a.id, None)?;
    }
    tx.commit()?;
    Ok((m, changed, absorbed.into_iter().map(|a| a.id).collect()))
}

/// Redacts a memory that is already stored (the «Redactar» / Redact button in Review): title and content without
/// their secrets, with the reason «redacted» on the previous version, and its history too (otherwise the
/// secret would remain in the earlier version). The person asks for it, so it is done even with
/// `OMOIKANE_REDACT=0`. With nothing to remove, it leaves it as it is (`redacted` empty).
pub fn redact(db: &mut Connection, id: &str, model: Option<&Modelo>) -> Result<Memory> {
    let m = get(db, id)?;
    let (title, ht) = crate::secretos::redactar(&m.title);
    let (content, hc) = crate::secretos::redactar(&m.content);
    if ht.is_empty() && hc.is_empty() {
        return Ok(m);
    }
    let c = Changes {
        title: (!ht.is_empty()).then_some(title),
        content: (!hc.is_empty()).then_some(content),
        reason: Some("redacted".into()),
        ..Changes::default()
    };
    let (r, _) = update_from(db, &m.id, c, None, model)?;
    let tx = write_tx(db)?;
    let versions: Vec<(i64, String, String)> = {
        let mut q = tx.prepare("SELECT version, title, content FROM memory_version WHERE memory_id = ?1")?;
        q.query_map([&m.id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (version, v_title, v_content) in versions {
        let (t, c) = (crate::secretos::redactar(&v_title).0, crate::secretos::redactar(&v_content).0);
        if t != v_title || c != v_content {
            tx.execute("UPDATE memory_version SET title = ?3, content = ?4 WHERE memory_id = ?1 AND version = ?2", params![m.id, version, t, c])?;
        }
    }
    tx.commit()?;
    Ok(Memory { redacted: ht.iter().chain(&hc).map(|h| h.tipo).collect(), ..r })
}

/// Rebuilds the word index from the table. Returns how many memories it indexed.
pub fn reindex_words(db: &mut Connection) -> Result<usize> {
    let tx = write_tx(db)?;
    tx.execute("DELETE FROM memory_fts", [])?;
    let ids: Vec<(i64, String)> = {
        let mut q = tx.prepare("SELECT n, id FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let all: Vec<(i64, Memory)> = ids.into_iter().map(|(n, id)| Ok((n, read(&tx, &id)?))).collect::<Result<_>>()?;
    for (n, m) in &all {
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
        )?;
    }
    tx.commit()?;
    Ok(all.len())
}

/// Forgets the memory: it leaves the database and the indexes, leaves a mark so sync doesn't
/// bring it back from another computer, and stays in the trash (`forgotten`) with its history, from where
/// `restore` brings it back. Returns what was forgotten.
pub fn forget(db: &mut Connection, id: &str) -> Result<Memory> {
    delete(db, id, None)
}

/// Forgets and also removes it from the trash: there is no going back.
pub fn forget_for_good(db: &mut Connection, id: &str) -> Result<Memory> {
    let m = delete(db, id, None)?;
    db.execute("DELETE FROM forgotten WHERE id = ?1", [&m.id])?;
    Ok(m)
}

/// Days a forgotten memory waits in the trash before being deleted for good
/// (`OMOIKANE_TRASH_DAYS`; 0 = never).
pub fn trash_days() -> f64 {
    std::env::var("OMOIKANE_TRASH_DAYS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(30.0)
}

/// Deletes from the trash what was forgotten more than `days` ago (0 = nothing). Returns how many.
pub fn purge(db: &Connection, days: f64) -> Result<usize> {
    if days <= 0.0 {
        return Ok(0);
    }
    let n = db.execute(
        "DELETE FROM forgotten WHERE deleted_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
        [format!("-{} seconds", (days * 86400.0) as i64)],
    )?;
    Ok(n)
}

/// Empties the whole trash. Returns how many.
pub fn empty_trash(db: &Connection) -> Result<usize> {
    Ok(db.execute("DELETE FROM forgotten", [])?)
}

/// Deletes these memories (full ids) from the trash for good, and only those: what the TUI empties from a
/// project, which doesn't see another space's trash. Returns how many there were.
pub fn empty_trash_of(db: &mut Connection, ids: &[String]) -> Result<usize> {
    let tx = write_tx(db)?;
    let mut n = 0;
    for id in ids {
        n += tx.execute("DELETE FROM forgotten WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// For sync: forget with the mark (stamp and date) coming from another computer. The memory is
/// kept aside in case it has to be brought back (`restore`).
pub(crate) fn forget_with(db: &mut Connection, id: &str, hlc: &str, deleted_at: &str, machine: &str) -> Result<Memory> {
    delete(db, id, Some((hlc, deleted_at, machine)))
}

/// A memory in the trash.
#[derive(Clone, Debug, Serialize)]
pub struct Forgotten {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub project: String,
    /// Its space (the key): its own if it was a space memory, or its project's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    /// Who forgot it: the computer's name (and `[by …]` already says the person, if sync records it).
    pub by: String,
    pub deleted_at: String,
}

/// The trash: what was forgotten here or on another computer, from most recent to oldest.
pub fn trash(db: &Connection) -> Result<Vec<Forgotten>> {
    let mut q = db.prepare("SELECT id, memory, by, deleted_at FROM forgotten ORDER BY deleted_at DESC")?;
    let v = q
        .query_map([], |f| {
            let m: String = f.get(1)?;
            let m: Memory = serde_json::from_str(&m).unwrap_or_default();
            Ok(Forgotten { id: f.get(0)?, title: m.title, kind: m.kind, project: m.project, space: m.space, by: f.get(2)?, deleted_at: f.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Brings a memory back from the trash (forgotten here or on another computer), with a stamp later than the
/// forget: so it also comes back to the other computers on the next sync. Its previous history
/// is kept.
pub fn restore(db: &mut Connection, id: &str, model: Option<&Modelo>) -> Result<Memory> {
    // The id as in the other commands: uppercase, and a prefix that matches several is an error.
    let id = id.trim().to_uppercase();
    // At least 4 characters, like `get` and `forget` (`resolve_id`): with just one you could pull out of
    // the trash, blindly, what the list doesn't show from this folder.
    ensure!(id.len() >= 4, tr!("id too short: «{id}»", "id demasiado corto: «{id}»"));
    ensure!(id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let tx = write_tx(db)?;
    let rows: Vec<(String, String, String, String)> = {
        let mut q = tx.prepare("SELECT id, memory, versions, hlc FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
        q.query_map([&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let (id, m, versions, forgotten_hlc) = match <[_; 1]>::try_from(rows) {
        Ok([one]) => one,
        Err(v) if v.is_empty() => bail!(tr!("nothing in the trash with id {id}", "no hay nada en la papelera con id {id}")),
        Err(_) => bail!(tr!("several memories in the trash start with {id}: type more characters", "hay varias memorias en la papelera que empiezan por {id}: escribe más caracteres")),
    };
    // Already live (another computer restored it and it came through sync, from a database before
    // 0.19.1): it was just left over in the trash.
    if tx.query_row("SELECT count(*) FROM memory WHERE id = ?1", [&id], |f| f.get::<_, i64>(0))? > 0 {
        tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
        tx.commit()?;
        bail!(tr!("{id} is not forgotten here (it came back from another computer); removed it from the trash", "{id} no está olvidada aquí (volvió desde otro equipo); la quito de la papelera"));
    }
    let (m, extra, _) = crate::sync::from_trash_json(&m).ok_or_else(|| anyhow!(tr!("{id} can't be read from the trash", "{id} no se entiende en la papelera")))?;
    let extra = crate::sync::extra_to_text(&extra);
    let forgotten_hlc = if m.content_hlc.is_empty() { forgotten_hlc } else { m.content_hlc.clone() };
    let versions: Vec<Version> = serde_json::from_str(&versions).unwrap_or_default();
    let mark: String = tx.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?.unwrap_or_default();
    let hlc = crate::reloj::stamp_after(&tx, &mark)?;
    let (project_id, space_id) = stored_placement(&tx, &m)?;
    let new = NewMemory {
        project: m.project.clone(),
        project_id: Some(project_id),
        space_id,
        kind: Some(m.kind),
        kind_text: m.kind_text.clone(),
        title: m.title.clone(),
        content: m.content.clone(),
        paths: m.paths.clone(),
        tags: m.tags.clone(),
        pinned: m.pinned,
        source: m.source.clone(),
        provenance: None,
        evidence: Vec::new(),
        review_reason: None,
    };
    // It comes back with its usual text: the content stamp is the one from when it was forgotten (the verdicts it
    // had still hold).
    let o = Origin { created_at: &m.created_at, updated_at: &m.updated_at, hlc: &hlc, content_hlc: &forgotten_hlc, author: &m.author, extra: &extra };
    tx.execute("DELETE FROM tombstone WHERE id = ?1", [&id])?;
    crate::sync::save_tombstone_extra(&tx, &id, &Default::default())?;
    let r = insert_in(&tx, &id, new, Some(&o), model)?;
    for v in &versions {
        put_version(&tx, &id, v)?;
    }
    tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
    tx.commit()?;
    Ok(r)
}

/// Where a memory stored outside the database (trash, backup) comes back to: its project by key (or,
/// in backups from before schema 10, by path), its space, or global.
pub(crate) fn stored_placement(tx: &Connection, m: &Memory) -> Result<(i64, Option<i64>)> {
    if !m.project.is_empty() || !m.project_key.is_empty() {
        let id = match crate::proyecto::resolve(tx, &m.project)? {
            Some((id, _)) if m.project_key.is_empty() || crate::proyecto::key_of(tx, id)? == m.project_key => id,
            _ if !m.project_key.is_empty() => crate::proyecto::of_key(tx, &m.project_key, &m.project, &crate::db::person_of(tx), true)?,
            _ => crate::proyecto::register(tx, &m.project)?,
        };
        // As when receiving it through sync: a project with no space here takes the one it brings; one
        // with a space keeps its own.
        if !m.space.is_empty() && crate::proyecto::space_of(tx, id)?.is_none() {
            let s = crate::proyecto::space_of_key(tx, &m.space)?;
            tx.execute("UPDATE project SET space_id = ?2 WHERE id = ?1", params![id, s])?;
        }
        return Ok((id, None));
    }
    Ok((0, crate::proyecto::space_of_key(tx, &m.space)?))
}

fn delete(db: &mut Connection, id: &str, mark: Option<(&str, &str, &str)>) -> Result<Memory> {
    let id = resolve_id(db, id)?;
    let tx = write_tx(db)?;
    let m = delete_in(&tx, &id, mark)?;
    tx.commit()?;
    Ok(m)
}

/// `delete` inside an already open transaction (the caller commits), with the id
/// already resolved.
fn delete_in(tx: &Transaction, id: &str, mark: Option<(&str, &str, &str)>) -> Result<Memory> {
    let (hlc, deleted_at): (String, Option<&str>) = match mark {
        Some((h, d, _)) => {
            crate::reloj::receive(tx, h)?;
            (h.to_string(), Some(d))
        }
        None => (crate::reloj::stamp_after(tx, &hlc_of(tx, id)?)?, None),
    };
    let m = read(tx, id)?;
    mark_forgotten(tx, id, deleted_at, &hlc, &m.space)?;
    // To the trash, with its history: `restore` brings it back from any computer. `by` is the
    // computer that forgot it: another one (maybe another person) or this one.
    let who = match mark {
        Some((_, _, machine)) => machine.to_string(),
        None => crate::reloj::machine_name(tx)?,
    };
    let versions = history(tx, id)?;
    let mem_hlc = hlc_of(tx, id)?;
    let (content_hlc, extra): (String, String) = tx.query_row("SELECT content_hlc, extra FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?)))?;
    let m = Memory { content_hlc, ..m };
    // It goes to the trash with its unknown fields: if it comes back (here or on another computer), they come back with it.
    let text = crate::sync::to_trash_json(&m, &crate::sync::extra_from(&extra), &Default::default())?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, COALESCE(?5, {NOW}), ?6)"),
        params![id, text, serde_json::to_string(&versions)?, mem_hlc, deleted_at, who],
    )?;
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    unindex_memory(tx, n)?;
    // memory_version and memory_path go away with ON DELETE CASCADE.
    tx.execute("DELETE FROM memory WHERE id = ?1", [id])?;
    Ok(m)
}

/// Records a version that comes from outside (another computer, a backup, the trash). The one already there with that
/// number stays; if it had no unknown fields and this one brings them (an omoi version that didn't
/// keep them saw it first), it takes them.
pub(crate) fn put_version(db: &Connection, id: &str, v: &Version) -> Result<()> {
    let extra = if v.extra.is_empty() { "{}".to_string() } else { serde_json::to_string(&v.extra)? };
    db.execute(
        "INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason, extra) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(memory_id, version) DO UPDATE SET extra = excluded.extra WHERE memory_version.extra = '{}' AND excluded.extra != '{}'",
        params![id, v.version, v.title, v.content, v.kind, serde_json::to_string(&v.paths)?, serde_json::to_string(&v.tags)?, v.changed_at, v.reason, extra],
    )?;
    Ok(())
}

/// A `memory_version` version from the columns `version, kind, title, content, paths, tags,
/// changed_at, reason, extra` (starting at column `start`).
fn version_from_row(f: &Row, start: usize) -> rusqlite::Result<Version> {
    let list = |i: usize| -> rusqlite::Result<Vec<String>> { Ok(serde_json::from_str(&f.get::<_, String>(start + i)?).unwrap_or_default()) };
    Ok(Version {
        version: f.get(start)?,
        kind: f.get(start + 1)?,
        title: f.get(start + 2)?,
        content: f.get(start + 3)?,
        paths: list(4)?,
        tags: list(5)?,
        changed_at: f.get(start + 6)?,
        reason: f.get(start + 7)?,
        extra: serde_json::from_str(&f.get::<_, String>(start + 8)?).unwrap_or_default(),
    })
}

/// Previous versions, from most recent to oldest.
pub fn history(db: &Connection, id: &str) -> Result<Vec<Version>> {
    let id = resolve_id(db, id)?;
    let mut q = db.prepare(
        "SELECT version, kind, title, content, paths, tags, changed_at, reason, extra
         FROM memory_version WHERE memory_id = ?1 ORDER BY version DESC",
    )?;
    let v = q.query_map([&id], |f| version_from_row(f, 0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The history of every memory that has one, in a single pass (for sync: one
/// query per memory was the slowest part of `omoi sync` with a large database).
pub(crate) fn histories(db: &Connection) -> Result<std::collections::HashMap<String, Vec<Version>>> {
    let mut q = db.prepare(
        "SELECT memory_id, version, kind, title, content, paths, tags, changed_at, reason, extra
         FROM memory_version ORDER BY memory_id, version DESC",
    )?;
    let mut v: std::collections::HashMap<String, Vec<Version>> = std::collections::HashMap::new();
    let mut rows = q.query([])?;
    while let Some(f) = rows.next()? {
        v.entry(f.get(0)?).or_default().push(version_from_row(f, 1)?);
    }
    Ok(v)
}

/// A `limit` for [`recent`] from which all are requested (whoever wants them all passes `usize::MAX >> 1`).
const ALL: usize = 1 << 20;
/// From how many rows (from lowest to highest `n`) they are read in parallel (see [`in_parallel`]).
const PARALLEL_FROM: i64 = 10_000;
/// Connections at once, at most.
const PARALLEL_THREADS: usize = 8;

/// The rows of `sql` (`SELECT {COLUMNS} FROM memory m WHERE …`, with the kind in `?1`) read in chunks
/// of `n` at a time, each chunk with its own read-only connection: with 50,000 memories, reading them on a single
/// connection takes half a second, almost all of it SQLite and `from_row`. `None` (and the caller reads them as
/// usual) if it isn't worth it or it can't be guaranteed to give the same: a database with no file, few
/// rows, a transaction open in `db` (the other connections wouldn't see what it has written) or
/// any error. The new connections see the last committed state, like `db` outside a transaction;
/// if another process commits something meanwhile (`data_version` changes), the chunks could come from
/// two different moments and that isn't valid either.
fn in_parallel(db: &Connection, sql: &str, kind: Option<Kind>) -> Option<Vec<Memory>> {
    let path = db.path().filter(|r| !r.is_empty())?.to_string();
    if !db.is_autocommit() {
        return None;
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(PARALLEL_THREADS) as i64;
    let (start, end): (i64, i64) = db.query_row("SELECT COALESCE(min(n), 0), COALESCE(max(n), -1) FROM memory", [], |f| Ok((f.get(0)?, f.get(1)?))).ok()?;
    if threads < 2 || end - start + 1 < PARALLEL_FROM {
        return None;
    }
    let version = || db.query_row("PRAGMA data_version", [], |f| f.get::<_, i64>(0)).ok();
    let before = version()?;
    let step = (end - start + threads) / threads;
    let sql = format!("{sql} AND m.n BETWEEN ?2 AND ?3");
    let chunks: Vec<Option<Vec<Memory>>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let (sql, path) = (&sql, &path);
                let (a, b) = (start + i * step, (start + (i + 1) * step - 1).min(end));
                s.spawn(move || -> Option<Vec<Memory>> {
                    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
                    let c = Connection::open_with_flags(path, flags).ok()?;
                    let mut q = c.prepare(sql).ok()?;
                    q.query_map(params![kind.map(Kind::as_str), a, b], from_row).ok()?.collect::<rusqlite::Result<_>>().ok()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    if version()? != before {
        return None;
    }
    chunks.into_iter().try_fold(Vec::new(), |mut v, t| {
        v.extend(t?);
        Some(v)
    })
}

/// The latest saved or changed ones.
pub fn recent(db: &Connection, scope: &Scope, kind: Option<Kind>, limit: usize) -> Result<Vec<Memory>> {
    // With a kind, plain `m.kind = ?1`: with the `IS NULL OR`, SQLite doesn't use the kind of the
    // `memory_scope` index and reads all of the scope's memories (the recall's recipes, on every message).
    let kind_filter = if kind.is_some() { "m.kind = ?1" } else { "?1 IS NULL" };
    // All of the scope's (review, drift, links, exports to Obsidian and OKF): in one pass over the
    // table, in the order they are on disk, and sorted here. Through the index, each row was read
    // with its own query and jumping around the file; with `ORDER BY`, SQLite sorted the whole rows in
    // a temporary file. With 50,000 memories: 0.85 s through the index, 3.5 s with `ORDER BY`, 0.5 s in
    // one pass and 0.2 s in parallel chunks. The order is the same: `updated_at` and `id` are compared
    // byte by byte, as SQLite compares them (`BINARY`), and the `id` is unique.
    if limit >= ALL {
        let sql = format!("SELECT {COLUMNS} FROM memory m WHERE {} AND {kind_filter}", scope.sql());
        let mut v = match in_parallel(db, &sql, kind) {
            Some(v) => v,
            None => db.prepare(&sql)?.query_map([kind.map(Kind::as_str)], from_row)?.collect::<rusqlite::Result<_>>()?,
        };
        v.sort_unstable_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| b.id.cmp(&a.id)));
        v.truncate(limit);
        return Ok(v);
    }
    // Without a kind, through `memory_reciente` (see `db::LOCALES`), which is already in the requested order: it stops at
    // the first `limit` of the scope and only those are read whole.
    let has_index = db
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'memory_reciente')", [], |f| f.get::<_, bool>(0))
        .unwrap_or(false);
    if kind.is_none() && has_index {
        let mut q = db.prepare(&format!(
            "SELECT m.n FROM memory m INDEXED BY memory_reciente WHERE {} ORDER BY m.updated_at DESC, m.id DESC LIMIT ?1",
            scope.sql()
        ))?;
        let ns: Vec<i64> = q.query_map([limit as i64], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut row = db.prepare(&format!("SELECT {COLUMNS} FROM memory m WHERE m.n = ?1"))?;
        return Ok(ns.into_iter().map(|n| row.query_row([n], from_row)).collect::<rusqlite::Result<_>>()?);
    }
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNS} FROM memory m
         WHERE {} AND {kind_filter}
         ORDER BY m.updated_at DESC, m.id DESC LIMIT ?2",
        scope.sql()
    ))?;
    let v = q.query_map(params![kind.map(Kind::as_str), limit as i64], from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The tag of the memories that say where a project's work was left.
pub const STATUS: &str = "status";

/// Where each project was left: the most recent memory with the [`STATUS`] tag of each
/// one (the global ones count as one more project), from newest to oldest.
pub fn statuses(db: &Connection, scope: &Scope, limit: usize) -> Result<Vec<Memory>> {
    let mut v: Vec<Memory> = Vec::new();
    for m in with_tag(db, scope, STATUS)? {
        // One per project; space ones, one per space, and global ones, one.
        if v.len() < limit && !v.iter().any(|x| x.project_id == m.project_id && (m.project_id != 0 || x.space == m.space)) {
            v.push(m);
        }
    }
    Ok(v)
}

/// The tag of the decisions that are up to the person and still unanswered: the agent left the
/// question, the options and what it assumed in the meantime, and kept working.
pub const OPEN: &str = "open";

/// The tag that, together with [`OPEN`], says the decision is blocking: without the answer, what
/// depends on it can't go on. Without it, the decision is "to review later" (the agent
/// went on with what it assumed). A tag and not a field: it doesn't change the schema or the sync.
pub const BLOCKING: &str = "blocking";

impl Memory {
    /// Whether it is a blocking open decision (with the [`BLOCKING`] tag).
    pub fn is_blocking(&self) -> bool {
        self.tags.iter().any(|t| t == BLOCKING)
    }
}

/// The open decisions of the scope (of kind decision and with the [`OPEN`] tag): blocking ones first
/// and, within each group, from newest to oldest; at most `limit`.
pub fn open_decisions(db: &Connection, scope: &Scope, limit: usize) -> Result<Vec<Memory>> {
    let mut v: Vec<Memory> = with_tag(db, scope, OPEN)?.into_iter().filter(|m| m.kind == Kind::Decision).collect();
    // Stable: keeps the date order within each group.
    v.sort_by_key(|m| !m.is_blocking());
    v.truncate(limit);
    Ok(v)
}

/// An open decision for other programs (`omoi decisions --json`, the `decisions` tool, Kumi's
/// hub): whole, with its priority and where it belongs.
#[derive(Debug, Serialize)]
pub struct Decision {
    pub id: String,
    pub title: String,
    pub content: String,
    /// `blocking` (can't go on without the answer) or `later` (to review later).
    pub priority: &'static str,
    /// Its project's folder; empty if it is a space or global one.
    pub project: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub global: bool,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    /// Its structured data, if it has any ([`set_data`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl From<Memory> for Decision {
    fn from(m: Memory) -> Decision {
        Decision {
            priority: if m.is_blocking() { "blocking" } else { "later" },
            global: m.global(),
            id: m.id,
            title: m.title,
            content: m.content,
            project: m.project,
            project_key: m.project_key,
            space: m.space,
            tags: m.tags,
            paths: m.paths,
            author: m.author,
            source: m.source,
            created_at: m.created_at,
            updated_at: m.updated_at,
            data: None,
        }
    }
}

/// The open decisions of the scope for other programs; with `blocking_only`, only those, and with
/// `wanted_tags`, only those that carry all of them (`tarea:123`).
pub fn decisions(db: &Connection, scope: &Scope, blocking_only: bool, wanted_tags: &[String], limit: usize) -> Result<Vec<Decision>> {
    let wanted = norm_tags(wanted_tags);
    let v: Vec<Memory> = open_decisions(db, scope, usize::MAX)?
        .into_iter()
        .filter(|m| (!blocking_only || m.is_blocking()) && wanted.iter().all(|t| m.tags.contains(t)))
        .take(limit)
        .collect();
    let mut data = data_of(db, &v.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    Ok(v.into_iter()
        .map(|m| {
            let data = data.remove(&m.id);
            Decision { data, ..Decision::from(m) }
        })
        .collect())
}

/// The tag left on a decision resolved with [`resolve_decision`] (or from the web view).
pub const RESOLVED: &str = "resolved";

/// Resolves an open decision: appends the answer at the end of the content («Answer (date): …»),
/// removes [`OPEN`] and [`BLOCKING`] and adds [`RESOLVED`]. With a version and reason, like `update`.
/// One already resolved isn't resolved again (error). In a trade decision the answer goes through
/// `oficios::fugas` like any new text (except what `allow_leaks` lets through): if it names
/// something of a project or a client it isn't saved and the leaks come back (`Ok(Err(…))`).
/// Splits a memory that is too long into several (the «Partir» / Split button in Review, 1.12.1): the first part
/// stays in the original memory (its id, history, links and pinning), and each of the others is
/// a new memory of the same project or space, kind, paths and tags (minus `status`: the project's
/// status is still a single one). The new ones are saved before changing the original, so if something
/// fails halfway no text is lost. Returns the changed original and the new ones, in order.
pub fn split(db: &mut Connection, id: &str, parts: &[(String, String)], reason: &str) -> Result<Vec<Memory>> {
    ensure!(parts.len() >= 2, tr!("splitting needs at least two parts", "para partirla hacen falta al menos dos partes"));
    ensure!(
        parts.iter().all(|(t, c)| !t.trim().is_empty() && !c.trim().is_empty()),
        tr!("every part needs a title and some text", "cada parte necesita un título y algo de texto")
    );
    let m = get(db, id)?;
    let space_id: Option<i64> = if m.project_id == 0 && !m.space.is_empty() {
        Some(db.query_row("SELECT id FROM space WHERE key = ?1", [&m.space], |f| f.get(0))?)
    } else {
        None
    };
    let tags: Vec<String> = m.tags.iter().filter(|t| *t != STATUS).cloned().collect();
    let mut new_ones = Vec::new();
    for (title, content) in &parts[1..] {
        let n = NewMemory {
            project_id: (m.project_id != 0).then_some(m.project_id),
            space_id,
            kind: Some(m.kind),
            title: title.trim().to_string(),
            content: content.trim().to_string(),
            paths: m.paths.clone(),
            tags: tags.clone(),
            source: "web".into(),
            ..Default::default()
        };
        new_ones.push(save(db, n, None)?);
    }
    let (title, content) = &parts[0];
    let c = Changes { title: Some(title.trim().to_string()), content: Some(content.trim().to_string()), reason: Some(reason.to_string()), ..Default::default() };
    let first = update(db, &m.id, c, None)?;
    Ok(std::iter::once(first).chain(new_ones).collect())
}

pub fn resolve_decision(
    db: &mut Connection,
    id: &str,
    answer: &str,
    reason: Option<String>,
    allow_leaks: bool,
) -> Result<std::result::Result<Memory, Vec<crate::oficios::Fuga>>> {
    let m = get(db, id)?;
    ensure!(m.kind == Kind::Decision, tr!("{} is not a decision", "{} no es una decisión", m.id));
    ensure!(
        !m.tags.iter().any(|t| t == RESOLVED),
        tr!(
            "{} is already resolved: to change the answer, `update` it",
            "{} ya está resuelta: para cambiar la respuesta, cámbiala con `update`",
            m.id
        )
    );
    let answer = answer.trim();
    ensure!(!answer.is_empty(), tr!("the answer can't be empty", "la respuesta no puede estar vacía"));
    if !crate::oficios::de_memoria(&m).is_empty() {
        let f = crate::oficios::bloquean(crate::oficios::fugas_en(db, &[("answer", answer.to_string())])?, allow_leaks);
        if !f.is_empty() {
            return Ok(Err(f));
        }
    }
    let day: String = db.query_row(&format!("SELECT substr({NOW}, 1, 10)"), [], |f| f.get(0))?;
    let mut tags: Vec<String> = m.tags.iter().filter(|t| *t != OPEN && *t != BLOCKING).cloned().collect();
    if !tags.iter().any(|t| t == RESOLVED) {
        tags.push(RESOLVED.into());
    }
    let c = Changes {
        append: Some(format!("Answer ({day}): {answer}")),
        tags: Some(tags),
        reason: Some(reason.unwrap_or_else(|| "decision resolved".into())),
        ..Default::default()
    };
    Ok(Ok(update(db, &m.id, c, None)?))
}

/// The memories of the scope that carry all these tags (and, if asked, of this kind), from newest
/// to oldest, with no ranking or threshold: for programs that save something with a tag and
/// always have to find it again (`omoi list`). At most `limit`.
pub fn with_tags(db: &Connection, scope: &Scope, wanted_tags: &[String], kind: Option<Kind>, limit: usize) -> Result<Vec<Memory>> {
    let wanted = norm_tags(wanted_tags);
    let Some((first, rest)) = wanted.split_first() else {
        bail!(tr!("which tag? Pass at least one with --tag", "¿qué etiqueta? Pasa al menos una con --tag"));
    };
    Ok(with_tag(db, scope, first)?
        .into_iter()
        .filter(|m| kind.is_none_or(|k| m.kind == k) && rest.iter().all(|t| m.tags.contains(t)))
        .take(limit)
        .collect())
}

// --- Structured data --------------------------------------------------------------------------------
//
// A JSON of the memory's own for programs (a project's profile in Kumi, for example), apart
// from the text: it doesn't go into the search index nor into recall, which only read title, content,
// tags and paths. It lives in `memory.extra` under the key [`DATA`], which is where sync and
// `omoi export` keep the fields of the line they don't know: so it travels as a `data` field of the
// line, and an older version (since 1.0) stores it and resends it as is without understanding it. Without
// changing the schema.

/// The key of the data in `memory.extra` (and the `data` field of the line that travels).
pub const DATA: &str = "data";
/// Maximum size of the data, in JSON.
pub const DATA_MAX: usize = 64 * 1024;

/// The data of some memories (those that have any).
pub fn data_of(db: &Connection, ids: &[&str]) -> Result<std::collections::HashMap<String, serde_json::Value>> {
    let mut r = std::collections::HashMap::new();
    let mut q = db.prepare("SELECT extra FROM memory WHERE id = ?1")?;
    for id in ids {
        if let Some(t) = q.query_row([id], |f| f.get::<_, String>(0)).optional()?
            && let Ok(serde_json::Value::Object(mut o)) = serde_json::from_str::<serde_json::Value>(&t)
            && let Some(d) = o.remove(DATA)
        {
            r.insert(id.to_string(), d);
        }
    }
    Ok(r)
}

/// Reads data that comes as text (`--data '{…}'`): valid JSON and at most [`DATA_MAX`].
pub fn data_from_text(t: &str) -> Result<serde_json::Value> {
    ensure!(t.len() <= DATA_MAX, tr!("data over {DATA_MAX} bytes", "datos de más de {DATA_MAX} bytes"));
    serde_json::from_str(t).map_err(|e| anyhow!(tr!("data is not valid JSON: {e}", "los datos no son JSON válido: {e}")))
}

/// Sets (or removes, with `None` or `null`) a memory's data. Changes its stamp and its date, so
/// sync carries it, and doesn't create a version (like pinning). Whatever looks like a secret in the
/// texts is masked, as in the content; returns the masked kinds.
pub fn set_data(db: &mut Connection, id: &str, data: Option<serde_json::Value>) -> Result<Vec<&'static str>> {
    let mut redacted_kinds = Vec::new();
    let data = ready_data(data, &mut redacted_kinds)?;
    let tx = write_tx(db)?;
    let id = resolve_id(&tx, id)?;
    if set_data_in(&tx, &id, data)? {
        tx.commit()?;
    }
    Ok(redacted_kinds)
}

/// The data as stored: without `null`, with whatever looks like a secret masked (records the kinds
/// in `redacted_kinds`) and at most [`DATA_MAX`].
fn ready_data(data: Option<serde_json::Value>, redacted_kinds: &mut Vec<&'static str>) -> Result<Option<serde_json::Value>> {
    let data = data.filter(|d| !d.is_null()).map(|d| redact_json(d, redacted_kinds));
    if let Some(d) = &data {
        ensure!(serde_json::to_string(d)?.len() <= DATA_MAX, tr!("data over {DATA_MAX} bytes", "datos de más de {DATA_MAX} bytes"));
    }
    Ok(data)
}

/// Whether [`set_data`] would accept this data (after masking whatever looks like a secret), without writing:
/// to check it before creating the memory it goes to.
pub(crate) fn check_data(data: &serde_json::Value) -> Result<()> {
    ready_data(Some(data.clone()), &mut Vec::new()).map(|_| ())
}

/// [`set_data`] inside a transaction, with the data already prepared and the full id. `false` if it already
/// had that same data (and then it touches nothing).
fn set_data_in(tx: &Transaction, id: &str, data: Option<serde_json::Value>) -> Result<bool> {
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.get(DATA) == data.as_ref() {
        return Ok(false);
    }
    match data {
        Some(d) => o.insert(DATA.into(), d),
        None => o.remove(DATA),
    };
    let hlc = crate::reloj::stamp_after(tx, &hlc_of(tx, id)?)?;
    tx.execute(&format!("UPDATE memory SET extra = ?2, hlc = ?3, updated_at = {NOW} WHERE id = ?1"), params![id, serde_json::to_string(&o)?, hlc])?;
    Ok(true)
}

/// The key in `memory.extra` of what to check when the review date arrives.
pub const REVIEW_REASON_KEY: &str = "review_reason";
/// The maximum length of that reason, in characters.
pub const MAX_REVIEW_REASON: usize = 300;

/// The reason for a review date, cleaned: one line, no secrets, truncated. `None` if it is empty.
pub fn clean_review_reason(t: &str) -> Option<String> {
    let mut t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return None;
    }
    if crate::secretos::activo() {
        t = crate::secretos::redactar(&t).0;
    }
    if t.chars().count() > MAX_REVIEW_REASON {
        t = t.chars().take(MAX_REVIEW_REASON - 1).collect::<String>() + "…";
    }
    Some(t)
}

/// Sets (or changes) what to check when the review date of `id` arrives, like evidence:
/// no new version and `updated_at` untouched, with a new stamp so it travels. `false` if it already had it.
pub fn set_review_reason(db: &mut Connection, id: &str, reason: &str) -> Result<bool> {
    let id = resolve_id(db, id)?;
    let Some(reason) = clean_review_reason(reason) else { return Ok(false) };
    let tx = write_tx(db)?;
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.get(REVIEW_REASON_KEY).and_then(|v| v.as_str()) == Some(reason.as_str()) {
        return Ok(false);
    }
    o.insert(REVIEW_REASON_KEY.into(), reason.into());
    let hlc = crate::reloj::stamp_after(&tx, &hlc_of(&tx, &id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    tx.commit()?;
    Ok(true)
}

/// Adds evidence (`evidencia`) to memory `id`, with no new version and without changing `updated_at` (it doesn't
/// change what it says, but what it is based on), but with a new stamp so it travels with sync.
/// `false` if it already had all of it.
pub fn add_evidence(db: &mut Connection, id: &str, new_ones: &[crate::evidencia::Evidence]) -> Result<bool> {
    let id = resolve_id(db, id)?;
    let tx = write_tx(db)?;
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if !crate::evidencia::add(&mut o, new_ones) {
        return Ok(false);
    }
    let hlc = crate::reloj::stamp_after(&tx, &hlc_of(&tx, &id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    tx.commit()?;
    Ok(true)
}

/// The texts of a JSON without whatever looks like a secret.
fn redact_json(v: serde_json::Value, redacted_kinds: &mut Vec<&'static str>) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(strip_secrets(&s, redacted_kinds)),
        Value::Array(a) => Value::Array(a.into_iter().map(|x| redact_json(x, redacted_kinds)).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, redact_json(x, redacted_kinds))).collect()),
        other => other,
    }
}

/// A memory in JSON for programs, with its data (`data`) if it has any.
pub fn with_data(db: &Connection, ms: Vec<Memory>) -> Result<Vec<serde_json::Value>> {
    let data = data_of(db, &ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    ms.into_iter()
        .map(|m| {
            let d = data.get(&m.id).cloned();
            let mut v = serde_json::to_value(m)?;
            if let Some(d) = d {
                v[DATA] = d;
            }
            Ok(v)
        })
        .collect()
}

/// All the memories of the scope with this tag (already lowercase, as stored), from newest
/// to oldest.
pub(crate) fn with_tag(db: &Connection, scope: &Scope, tag: &str) -> Result<Vec<Memory>> {
    // Only those that have the word in their tags according to the word index (all those that carry
    // the tag, and maybe a few more, which the rest of the condition removes): without it, checking the tags
    // of each memory read the whole table, content included (500 ms with 100,000 memories).
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNS} FROM (SELECT rowid AS n FROM memory_fts WHERE memory_fts MATCH ?3) e CROSS JOIN memory m ON m.n = e.n
         WHERE {} AND m.tags LIKE ?2 AND EXISTS (SELECT 1 FROM json_each(m.tags) WHERE value = ?1)
         ORDER BY m.updated_at DESC, m.id DESC",
        scope.sql()
    ))?;
    // The `LIKE` discards, without reading the JSON, what doesn't carry the tag.
    let v = q
        .query_map(params![tag, format!("%\"{tag}\"%"), format!("tags : \"{}\"", tag.replace('"', "\"\""))], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// What the agent should always know when starting in a project: the preferences and the pinned ones,
/// of the project, of its space and global (from a general folder, only the global ones). Pinned ones
/// first, then the most recent.
pub fn context(db: &Connection, scope: &Scope, limit: usize) -> Result<Vec<Memory>> {
    let cond = match scope {
        // Not the trade ones: they belong to the projects of that trade, not to the person.
        Scope::All => "(m.project_id = 0 AND m.space_id IS NULL AND instr(m.tags, '\"trade:') = 0)".to_string(),
        a => a.sql(),
    };
    // The condition, the same as that of the partial index `memory_always`, so it uses it.
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNS} FROM memory m
         WHERE {cond} AND (m.pinned = 1 OR m.kind = 'preference')
         ORDER BY m.pinned DESC, m.updated_at DESC, m.id DESC LIMIT ?1"
    ))?;
    let v = q.query_map(params![limit as i64], from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// How many memories are visible from a scope. With the per-project-and-space count kept by the
/// triggers (`db::LOCALES`), without scanning the table; by counting, on a connection without it.
pub fn count(db: &Connection, scope: &Scope) -> Result<i64> {
    match db.query_row(&format!("SELECT COALESCE(sum(m.n), 0) FROM memory_cuenta m WHERE {}", scope.sql_without_trades()), [], |f| f.get::<_, i64>(0)) {
        Ok(n) => Ok(n - crate::oficios::cuantas_ocultas(db, scope)?),
        Err(_) => Ok(db.query_row(&format!("SELECT count(*) FROM memory m WHERE {}", scope.sql()), [], |f| f.get(0))?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading them all in one pass, in parallel chunks or inside a transaction gives the same result, in
    /// the same order, as reading them one by one through the index.
    #[test]
    fn all_same_as_through_the_index() {
        let dir = crate::comun::Temporal::nueva("todas-paralelo");
        let mut db = crate::db::open(&dir.join("o.db")).unwrap();
        db.execute_batch("PRAGMA synchronous = OFF").unwrap();
        let p = dir.join("p");
        std::fs::create_dir_all(&p).unwrap();
        let key = crate::proyecto::of_dir(&p).unwrap();
        for i in 0..300 {
            let project = if i % 3 == 0 { String::new() } else { key.clone() };
            let kind = Some(if i % 4 == 0 { Kind::Decision } else { Kind::Fact });
            save(&mut db, NewMemory { project, kind, title: format!("memoria {i}"), content: format!("lo que dice la {i}"), ..Default::default() }, None).unwrap();
        }
        // Date ties (the id decides) and an `n` far from the others, so it is read in chunks.
        db.execute_batch(
            "UPDATE memory SET updated_at = '2026-01-01T00:00:00.000Z' WHERE n % 7 = 0;
             UPDATE memory SET n = n + 50000 WHERE n = (SELECT max(n) FROM memory);",
        )
        .unwrap();
        let json = |v: Vec<Memory>| v.iter().map(|m| serde_json::to_string(m).unwrap()).collect::<Vec<_>>();
        for scope in [Scope::All, Scope::from_project(&db, &key).unwrap()] {
            for kind in [None, Some(Kind::Decision)] {
                let kind_filter = if kind.is_some() { "m.kind = ?1" } else { "?1 IS NULL" };
                let sql = format!("SELECT {COLUMNS} FROM memory m WHERE {} AND {kind_filter}", scope.sql());
                assert!(in_parallel(&db, &sql, kind).is_some(), "por trozos");
                let by_index = json(recent(&db, &scope, kind, 1_000).unwrap());
                assert!(by_index.len() > 50);
                assert_eq!(json(recent(&db, &scope, kind, usize::MAX >> 1).unwrap()), by_index);
                db.execute_batch("BEGIN").unwrap();
                assert!(in_parallel(&db, &sql, kind).is_none(), "en una transacción, no");
                assert_eq!(json(recent(&db, &scope, kind, usize::MAX >> 1).unwrap()), by_index);
                db.execute_batch("ROLLBACK").unwrap();
            }
        }
    }

    #[test]
    fn normalizes_paths_and_tags() {
        assert_eq!(norm_paths(&["./src\\auth.rs".into(), "src/auth.rs".into(), " docs/ ".into(), "".into()]), [
            "docs",
            "src/auth.rs"
        ]);
        assert_eq!(norm_tags(&["#Auth".into(), "auth".into(), " Login ".into()]), ["auth", "login"]);
    }

    #[test]
    fn title_on_one_line() {
        assert_eq!(clean_title("  dos\nlíneas  ").unwrap(), "dos líneas");
        assert!(clean_title("   ").is_err());
        assert!(clean_title(&"x".repeat(201)).is_err());
    }

    #[test]
    fn kinds() {
        assert_eq!("Gotcha".parse::<Kind>().unwrap(), Kind::Gotcha);
        assert!("bugfix".parse::<Kind>().is_err());
    }
}
