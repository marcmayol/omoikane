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
/// (see `Memoria::kind_text`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tipo {
    #[default]
    Fact,
    Decision,
    Gotcha,
    Preference,
    /// A recipe: steps to follow (publish a version, deploy…), with a `when:` line of
    /// the words that bring it in whole (Phase 7.3).
    Procedure,
}

impl Tipo {
    pub const TODOS: [Tipo; 5] = [Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Preference, Tipo::Procedure];

    pub fn as_str(self) -> &'static str {
        match self {
            Tipo::Fact => "fact",
            Tipo::Decision => "decision",
            Tipo::Gotcha => "gotcha",
            Tipo::Preference => "preference",
            Tipo::Procedure => "procedure",
        }
    }
}

impl FromStr for Tipo {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Tipo> {
        Tipo::TODOS
            .into_iter()
            .find(|t| t.as_str() == s.trim().to_lowercase())
            .ok_or_else(|| anyhow!(tr!("unknown kind «{s}»: use fact, decision, gotcha, preference or procedure", "tipo «{s}» desconocido: usa fact, decision, gotcha, preference o procedure")))
    }
}

impl std::fmt::Display for Tipo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Memoria {
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
    pub kind: Tipo,
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
    pub verified: Option<crate::verificado::Verificada>,
    /// In which session and at which commit it was written (`procedencia`, in `memory.extra`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::procedencia::Procedencia>,
    /// What it is based on (`evidencia`, in `memory.extra`): what the one who saved it saw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<crate::evidencia::Evidencia>,
    /// What to check when its review date arrives (`review:YYYY-MM-DD`), in `memory.extra`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<String>,
    /// The verification removed by this call's text change (`update`). Not stored nor in
    /// the JSON: it is so the caller can say so.
    #[serde(skip)]
    pub unverified: Option<crate::verificado::Verificada>,
}

impl Memoria {
    /// Of a space, with no project.
    pub fn de_espacio(&self) -> bool {
        self.project_id == 0 && !self.space.is_empty()
    }

    /// Global: neither a project's nor a space's.
    pub fn global(&self) -> bool {
        self.project_id == 0 && self.space.is_empty()
    }

    /// The kind as stored and sent (an unknown one, as is).
    pub fn tipo_texto(&self) -> &str {
        self.kind_text.as_deref().unwrap_or(self.kind.as_str())
    }
}

/// What the caller decides. The rest (id, dates, stats) is set by Omoikane.
#[derive(Clone, Debug, Default)]
pub struct Nueva {
    /// The project folder (empty = global). Ignored if `project_id` or `space_id` is given.
    pub project: String,
    /// The already-resolved project (what comes in through sync).
    pub project_id: Option<i64>,
    /// Memory of this space, with no project.
    pub space_id: Option<i64>,
    pub kind: Option<Tipo>,
    /// The kind as is, if this version doesn't know it (what comes in through sync).
    pub kind_text: Option<String>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
    /// Where it comes from (`procedencia::actual`), for what is written here: goes to `memory.extra`.
    pub provenance: Option<crate::procedencia::Procedencia>,
    /// What it is based on (`evidencia`), already cleaned: goes to `memory.extra`.
    pub evidence: Vec<crate::evidencia::Evidencia>,
    /// What to check when its review date arrives: goes to `memory.extra`.
    pub review_reason: Option<String>,
}

/// Only what comes in `Some` changes.
#[derive(Clone, Debug, Default)]
pub struct Cambios {
    pub title: Option<String>,
    pub content: Option<String>,
    pub kind: Option<Tipo>,
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

/// A piece that `replace` or `append` changed, with [`CONTEXTO`] characters on each side: what the
/// `update` response shows so the agent doesn't have to ask for `get`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cambiado {
    /// The piece that was searched for (`replace`); nothing if it was `append`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub appended: bool,
    pub context: String,
}

/// Context characters on each side of what changed.
pub const CONTEXTO: usize = 80;

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
/// another space. From a general folder (the user's, see `proyecto::es_general`), all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ambito {
    /// A project (`id`; -1 if it has nothing yet) and its space.
    Proyecto { id: i64, space: Option<i64> },
    /// All the projects of a space, the space's own and the global ones; with `None`, all the projects
    /// with no space and the global ones (what "all projects" sees from a project with no space).
    Espacio(Option<i64>),
    Todos,
}

impl Ambito {
    /// What is visible from a project's folder (without writing anything to the database).
    pub fn de(db: &Connection, project: &str) -> Result<Ambito> {
        if crate::proyecto::es_general(project) {
            return Ok(Ambito::Todos);
        }
        let (id, space) = crate::proyecto::resolver(db, project)?.unwrap_or((-1, None));
        Ok(Ambito::Proyecto { id, space })
    }

    /// "All projects" as seen from here: from a project, those of its space (or those with no
    /// space); from a general folder, all.
    pub fn amplio(self) -> Ambito {
        match self {
            Ambito::Proyecto { space, .. } => Ambito::Espacio(space),
            other => other,
        }
    }

    /// The space it is viewed from (`None` if there is none, or from a general folder).
    pub fn espacio(&self) -> Option<i64> {
        match self {
            Ambito::Proyecto { space, .. } => *space,
            Ambito::Espacio(s) => *s,
            Ambito::Todos => None,
        }
    }

    /// Whether a memory (by its id) is visible from here: the same condition as `sql`.
    pub fn ve(&self, db: &Connection, id: &str) -> Result<bool> {
        Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE m.id = ?1 AND {})", self.sql()), [id], |f| f.get(0))?)
    }

    /// The project it is viewed from, if it is one.
    pub fn proyecto(&self) -> Option<i64> {
        match self {
            Ambito::Proyecto { id, .. } if *id > 0 => Some(*id),
            _ => None,
        }
    }

    /// Whether `other` can be viewed from here: from a general folder, everything; from a project or a
    /// space, only what is in its same space (or, with no space, what has none).
    pub fn permite(&self, other: &Ambito) -> bool {
        matches!(self, Ambito::Todos) || self.espacio() == other.espacio()
    }

    /// Whether a memory of this project (`project_id`; 0 if none) and, with no project, of this space
    /// (`space_id`) is visible from here: the same condition as `sql`, for what isn't in the
    /// `memory` table (the trash).
    pub fn incluye(&self, db: &Connection, project_id: i64, space_id: Option<i64>) -> Result<bool> {
        let in_space = |s: Option<i64>| project_id != 0 || space_id.is_none() || space_id == s;
        Ok(match self {
            Ambito::Todos => true,
            Ambito::Proyecto { id, space } => (project_id == *id || project_id == 0) && in_space(*space),
            Ambito::Espacio(s) => (project_id == 0 || crate::proyecto::espacio_de(db, project_id)? == *s) && in_space(*s),
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
            Ambito::Proyecto { id, space } => format!("(m.project_id IN ({id}, 0) AND {})", space_cond(*space)),
            Ambito::Espacio(Some(s)) => format!("(m.project_id IN (SELECT 0 UNION ALL SELECT id FROM project WHERE space_id = {s}) AND {})", space_cond(Some(*s))),
            Ambito::Espacio(None) => format!("(m.project_id IN (SELECT id FROM project WHERE space_id IS NULL) AND {})", space_cond(None)),
            Ambito::Todos => "1".to_string(),
        }
    }
}

pub(crate) const AHORA: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// `INDEXED BY memory_ambito` to join `memory m` by `n` with another table (`memory_fts`,
/// `memory_vec`) and filter by scope without reading each memory's whole row; nothing if the connection
/// doesn't have the index (see `db::LOCALES`). It only changes where SQLite reads from, not the output.
pub(crate) fn por_ambito(db: &Connection) -> &'static str {
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
pub(crate) fn ve_todas(db: &Connection, scope: &Ambito) -> bool {
    let counts = || -> Option<bool> {
        let all: i64 = db.query_row("SELECT COALESCE(sum(n), 0) FROM memory_cuenta", [], |f| f.get(0)).ok()?;
        let here: i64 = match scope {
            Ambito::Todos => all,
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
pub(crate) const COLUMNAS: &str = "m.id, COALESCE((SELECT path FROM project WHERE id = m.project_id), ''), m.kind, m.title, m.content, m.paths, m.tags, m.pinned, m.source, m.created_at, m.updated_at, m.recalled_at, m.recall_count, m.author, m.project_id, COALESCE((SELECT key FROM project WHERE id = m.project_id), ''), COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), ''), CASE WHEN instr(m.extra, 'verified') > 0 AND json_valid(m.extra) AND NOT EXISTS (SELECT 1 FROM meta WHERE k = 'sello_ajeno:' || m.id AND v = json_extract(m.extra, '$.verified.by') || char(10) || json_extract(m.extra, '$.verified.at')) THEN json_extract(m.extra, '$.verified') END, CASE WHEN instr(m.extra, 'provenance') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.provenance') END, CASE WHEN instr(m.extra, 'evidence') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.evidence') END, CASE WHEN instr(m.extra, 'review_reason') > 0 AND json_valid(m.extra) THEN json_extract(m.extra, '$.review_reason') END";
/// How many columns there are: whatever is requested after `COLUMNAS` starts at this index.
pub(crate) const NCOL: usize = 21;

pub(crate) fn de_fila(f: &Row) -> rusqlite::Result<Memoria> {
    let list = |i: usize| -> rusqlite::Result<Vec<String>> {
        let s: String = f.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let kind: String = f.get(2)?;
    let parsed = kind.parse::<Tipo>().ok();
    let (title, content): (String, String) = (f.get(3)?, f.get(4)?);
    let verified = crate::verificado::de_columna(f.get::<_, Option<String>>(17)?.as_deref(), &kind, &title, &content);
    let tags = list(6)?;
    let project_id: i64 = f.get(14)?;
    // What it may carry depending on where it is now (a global one doesn't show a client's branch).
    let provenance = crate::procedencia::para_memoria(crate::procedencia::de_columna(f.get::<_, Option<String>>(18)?.as_deref()), project_id, &tags);
    Ok(Memoria {
        id: f.get(0)?,
        project: f.get(1)?,
        project_id,
        project_key: f.get(15)?,
        space: f.get(16)?,
        kind: parsed.unwrap_or(Tipo::Fact),
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
        evidence: crate::evidencia::de_columna(f.get::<_, Option<String>>(19)?.as_deref()),
        review_reason: f.get::<_, Option<String>>(20)?.filter(|r| !r.trim().is_empty()),
        unverified: None,
    })
}

pub(crate) fn escribir_tx(db: &mut Connection) -> Result<Transaction<'_>> {
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
pub(crate) fn como_se_guarda(title: &str, content: &str, paths: &[String], tags: &[String], redacted_kinds: &mut Vec<&'static str>) -> Result<(String, String, Vec<String>, Vec<String>)> {
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

/// What goes from `from` to `to` in `text` with [`CONTEXTO`] characters on each side, and «…» where
/// it is cut.
fn around(text: &str, from: usize, to: usize) -> String {
    let start = text[..from].char_indices().rev().nth(CONTEXTO - 1).map(|(i, _)| i).unwrap_or(0);
    let end = text[to..].char_indices().nth(CONTEXTO).map(|(i, _)| to + i).unwrap_or(text.len());
    let (before, after) = (if start > 0 { "…" } else { "" }, if end < text.len() { "…" } else { "" });
    format!("{before}{}{after}", &text[start..end])
}

/// Applies to `content` the `replace`s (each one exactly once, in order) and the `append` of
/// some `Cambios`. Returns the changed pieces with their context; with a `find` that appears 0 or
/// several times, an error with the count, and the content is left untouched.
fn apply_edit(content: &mut String, c: &Cambios) -> Result<Vec<Cambiado>> {
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
        changed.push(Cambiado { find: Some(find.clone()), appended: false, context: around(&new_text, from, from + with.len()) });
    }
    if let Some(a) = c.append.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        let base = new_text.trim_end().to_string();
        let from = base.len() + 2;
        new_text = format!("{base}\n\n{a}");
        changed.push(Cambiado { find: None, appended: true, context: around(&new_text, from, new_text.len()) });
    }
    *content = new_text;
    Ok(changed)
}

/// The title and content `m` would have with these changes (`title`, `content`, `replace`, `append`),
/// without writing anything: to look at the final text before saving it (`oficios::al_cambiar`) or to
/// save it in another memory (`oficios::crear_de_oficio`).
pub fn texto_tras(m: &Memoria, c: &Cambios) -> Result<(String, String)> {
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

fn index_memory(tx: &Transaction, n: i64, m: &Memoria) -> Result<()> {
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
pub fn guardar(db: &mut Connection, new: Nueva, model: Option<&Modelo>) -> Result<Memoria> {
    insert(db, &ulid::Ulid::generate().to_string(), new, None, model)
}

/// Like [`guardar`], but with the id it brings (a memory coming back from an OKF bundle exported from
/// Omoikane, `okf::importar`): goes through secret redaction and gets a stamp from here.
pub(crate) fn guardar_con_su_id(db: &mut Connection, id: &str, new: Nueva, model: Option<&Modelo>) -> Result<Memoria> {
    let id = ulid::Ulid::from_string(id).map_err(|e| anyhow!("{id}: {e}"))?.to_string();
    insert(db, &id, new, None, model)
}

/// The topic key tag: `topic:<key>`. One memory per key and per scope (project,
/// space or global): `remember` with a key updates the one that already has it instead of creating another.
pub const ETIQUETA_TEMA: &str = "topic:";

/// What [`guardar_por_clave`] did.
#[derive(Clone, Debug)]
pub struct PorClave {
    pub memoria: Memoria,
    /// `true` if there was none with that key in the scope and it was created.
    pub creada: bool,
    /// When created: other keys in the scope that look like the new one (maybe one of them was meant).
    pub parecidas: Vec<String>,
    /// What was masked in the data for looking like a secret (their kinds).
    pub tapados: Vec<&'static str>,
}

/// The key as stored: lowercase, no surrounding spaces, and only letters, digits and `-_.:/`
/// (fits in a tag and is written the same in the CLI and in the MCP).
pub fn clave_tema(key: &str) -> Result<String> {
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
pub fn clave_de(m: &Memoria) -> Option<&str> {
    m.tags.iter().find_map(|t| t.strip_prefix(ETIQUETA_TEMA))
}

/// Saves by key: if in the same scope (the project, the space or global, exactly the right one)
/// there is already a memory with `topic:<key>`, updates it (title, content, kind; merges tags and
/// paths); otherwise creates it. All in one `BEGIN IMMEDIATE` transaction that searches inside: two processes
/// at once with the same key leave a single memory (the second one waits and finds it). Without checking
/// duplicates: the key already says which one it is. The `data`, if given, go to `memory.extra` like those of
/// [`poner_datos`] and in the same transaction (without them, whatever it had stays); as there, changing
/// only the data doesn't create a version, and the same thing again (text and data) changes nothing.
pub fn guardar_por_clave(db: &mut Connection, key: &str, mut new: Nueva, data: Option<serde_json::Value>, model: Option<&Modelo>) -> Result<PorClave> {
    let key = clave_tema(key)?;
    let mut redacted_kinds = Vec::new();
    let data = ready_data(data, &mut redacted_kinds)?;
    let tag = format!("{ETIQUETA_TEMA}{key}");
    new.tags.retain(|t| !t.trim().to_lowercase().starts_with(ETIQUETA_TEMA));
    new.tags.push(tag.clone());
    let tx = escribir_tx(db)?;
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
            let before = leer(&tx, &id)?;
            let mut tags = before.tags.clone();
            tags.extend(new.tags.iter().cloned());
            let mut paths = before.paths.clone();
            paths.extend(new.paths.iter().cloned());
            let c = Cambios {
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
            PorClave { memoria: m, creada: false, parecidas: Vec::new(), tapados: Vec::new() }
        }
        None => {
            new.project_id = Some(project_id);
            new.space_id = space_id;
            let m = insert_in(&tx, &ulid::Ulid::generate().to_string(), new, None, model)?;
            let similar = similar_keys(&tx, project_id, space_id, &key)?;
            PorClave { memoria: m, creada: true, parecidas: similar, tapados: Vec::new() }
        }
    };
    let data_changed = match data {
        Some(d) => set_data_in(&tx, &r.memoria.id, Some(d))?,
        None => false,
    };
    let r = if data_changed { PorClave { memoria: leer(&tx, &r.memoria.id)?, tapados: redacted_kinds, ..r } } else { PorClave { tapados: redacted_kinds, ..r } };
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
    let keys: Vec<String> = q.query_map(params![project_id, space_id, ETIQUETA_TEMA.len() as i64 + 1], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
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
pub enum Nivel {
    Project,
    Space,
    Trade,
    Global,
}

/// The level of a memory: a project's, a space's, a trade's (global with `trade:`) or global.
pub fn nivel_de(m: &Memoria) -> Nivel {
    if m.project_id != 0 {
        Nivel::Project
    } else if m.de_espacio() {
        Nivel::Space
    } else if crate::oficios::lleva_oficio(&m.tags) {
        Nivel::Trade
    } else {
        Nivel::Global
    }
}

/// All the memories with this key the scope sees, from most specific to most general: the
/// project's, its space's, its trades' and the global one. Deterministic, no search: the caller
/// combines them (for example, a project's path through Kumi's megafactory). What the scope doesn't
/// see (another client, another project) doesn't come out.
pub fn resolver_clave(db: &Connection, scope: &Ambito, key: &str) -> Result<Vec<(Nivel, Memoria)>> {
    let key = clave_tema(key)?;
    let mut v: Vec<(Nivel, Memoria)> = con_etiqueta(db, scope, &format!("{ETIQUETA_TEMA}{key}"))?.into_iter().map(|m| (nivel_de(&m), m)).collect();
    // Two computers that created the same key without seeing each other leave, after syncing, two memories in the same
    // scope: one wins, the same one on every computer ([`key_winner`]), and the other doesn't come out.
    v.sort_by(|a, b| key_winner(&b.1, &a.1));
    let mut seen = std::collections::HashSet::new();
    v.retain(|(_, m)| seen.insert((m.project_id, m.space.clone(), trade_scope(&m.tags))));
    v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| trade_scope(&a.1.tags).cmp(&trade_scope(&b.1.tags))).then_with(|| a.1.created_at.cmp(&b.1.created_at)));
    Ok(v)
}

/// Between two memories with the same key in the same scope, which one wins: the one changed later and, if
/// equal, the one with the larger id (`Greater`: `a`). Same as the `ORDER BY` of [`guardar_por_clave`], so
/// `remember --key` changes the same one `get --key` returns.
fn key_winner(a: &Memoria, b: &Memoria) -> std::cmp::Ordering {
    a.updated_at.cmp(&b.updated_at).then_with(|| a.id.cmp(&b.id))
}

/// The start of the reason of the verdict that [`resolver_claves_repetidas`] sets.
const REPEATED_KEY_REASON: &str = "same key ";

/// After syncing or importing: the keys repeated in the same scope (two computers that did
/// `remember --key` with the same key without seeing each other; on each one, the key is atomic). The one
/// [`key_winner`] picks wins and the others are superseded by it (`superseded`, a verdict that travels and that
/// every computer sets the same way), so that Review proposes forgetting them. Nothing is deleted or changed:
/// `get --key` already returns only the winner. Returns how many were superseded now.
pub fn resolver_claves_repetidas(db: &Connection) -> Result<usize> {
    let mut q = db.prepare("SELECT id, project_id, space_id, tags, updated_at FROM memory WHERE tags LIKE '%\"topic:%'")?;
    let rows: Vec<(String, i64, Option<i64>, String, String)> =
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    type Group = (i64, Option<i64>, String, Vec<String>);
    let mut groups: std::collections::BTreeMap<Group, Vec<Memoria>> = std::collections::BTreeMap::new();
    for (id, project_id, space_id, tags, updated_at) in rows {
        let tags: Vec<String> = serde_json::from_str(&tags).unwrap_or_default();
        let m = Memoria { id, project_id, tags, updated_at, ..Default::default() };
        if let Some(key) = clave_de(&m).map(str::to_string) {
            groups.entry((project_id, space_id, key, trade_scope(&m.tags))).or_default().push(m);
        }
    }
    let active = crate::veredictos::parejas(db)?;
    let mut n = 0;
    for ((_, _, key, _), mut v) in groups.into_iter().filter(|(_, v)| v.len() > 1) {
        v.sort_by(|a, b| key_winner(b, a));
        let winner = &v[0];
        for other in &v[1..] {
            // With any active verdict the pair is already judged, as in `revisar::duplicadas_de_oficio`:
            // after "it's fine as is" (`compatible`) the next sync or import set it as superseded again.
            // Only the one that this same code set the other way round is flipped (the losing one was changed later
            // and now it is the winner).
            let reversed = |x: &crate::veredictos::Veredicto| x.relation == crate::veredictos::SUSTITUIDA && x.memory_id == winner.id && x.reason.starts_with(REPEATED_KEY_REASON);
            if active.get(&crate::veredictos::pareja(&other.id, &winner.id)).is_some_and(|x| !reversed(x)) {
                continue;
            }
            let author = crate::veredictos::autor(db, "sync", &other.id);
            crate::veredictos::poner(db, &other.id, &winner.id, crate::veredictos::SUSTITUIDA, &format!("{REPEATED_KEY_REASON}{key} written on two computers"), &author)?;
            n += 1;
        }
    }
    Ok(n)
}

/// A layer of [`resolver_clave`] as it comes out in `get --key … --resolve --json`: its scope
/// (`project`, `space`, `trade:<trade>` or `global`), the memory, its text and its data (those of
/// `memory.extra`, [`poner_datos`]), without merging anything with the other layers.
#[derive(Clone, Debug, Serialize)]
pub struct Capa {
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
pub fn capas_de_clave(db: &Connection, scope: &Ambito, key: &str) -> Result<Vec<Capa>> {
    let layers = resolver_clave(db, scope, key)?;
    let mut data = datos_de(db, &layers.iter().map(|(_, m)| m.id.as_str()).collect::<Vec<_>>())?;
    Ok(layers
        .into_iter()
        .map(|(level, m)| {
            let scope = match level {
                Nivel::Project => "project".to_string(),
                Nivel::Space => "space".to_string(),
                Nivel::Global => "global".to_string(),
                Nivel::Trade => format!(
                    "trade:{}",
                    crate::oficios::de_etiquetas(&trade_tags(&m.tags)).join(",")
                ),
            };
            Capa {
                scope,
                data: data.remove(&m.id),
                id: m.id,
                title: m.title,
                text: m.content,
                project: if level == Nivel::Project { m.project } else { String::new() },
                space: if level == Nivel::Space { m.space } else { String::new() },
                updated_at: m.updated_at,
            }
        })
        .collect())
}

/// The memory with this key that wins in the scope: the most specific one from [`resolver_clave`].
pub fn por_clave(db: &Connection, scope: &Ambito, key: &str) -> Result<Option<Memoria>> {
    Ok(resolver_clave(db, scope, key)?.into_iter().next().map(|(_, m)| m))
}

/// Where a memory coming in through sync comes from: its id, dates, stamps and author of origin.
pub(crate) struct Origen<'a> {
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
        (None, None) => (crate::proyecto::registrar(tx, project)?, None),
    })
}

/// For sync: the same memory, with the same id, dates, stamp and author as on its computer.
pub(crate) fn guardar_con_id(db: &mut Connection, id: &str, new: Nueva, o: &Origen, model: Option<&Modelo>) -> Result<Memoria> {
    insert(db, id, new, Some(o), model)
}

/// The stamp of a memory (empty if it doesn't exist).
pub(crate) fn hlc_de(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Records a forget mark, unless there is already a later one, with the space (its key; empty: of
/// none) of what was forgotten: the forget goes only to that space's repository.
pub(crate) fn marcar_olvido(db: &Connection, id: &str, deleted_at: Option<&str>, hlc: &str, space_key: &str) -> Result<()> {
    let cur: Option<String> = db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    if cur.is_none_or(|a| crate::reloj::mayor(hlc, &a)) {
        db.execute(
            &format!("INSERT OR REPLACE INTO tombstone(id, deleted_at, hlc, space_key) VALUES (?1, COALESCE(?2, {AHORA}), ?3, ?4)"),
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
pub(crate) fn autor_en(db: &Connection, space_key: &str) -> Result<String> {
    if let Some(a) = crate::entorno::autor() {
        return Ok(a);
    }
    if !space_key.is_empty()
        && let Some(a) = db.query_row("SELECT v FROM meta WHERE k = ?1", [format!("autor:{space_key}")], |f| f.get(0)).optional()?
    {
        return Ok(a);
    }
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'autor'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// The author of what is saved in this project or space (`autor_en` with the space it belongs to).
pub(crate) fn autor_de(db: &Connection, project_id: i64, space_id: Option<i64>) -> Result<String> {
    if crate::entorno::autor().is_some() {
        return autor_en(db, "");
    }
    let space = match space_id {
        Some(s) => Some(s),
        None => crate::proyecto::espacio_de(db, project_id)?,
    };
    autor_en(db, &crate::proyecto::clave_espacio(db, space)?)
}

fn insert(db: &mut Connection, id: &str, new: Nueva, origin: Option<&Origen>, model: Option<&Modelo>) -> Result<Memoria> {
    let tx = escribir_tx(db)?;
    let m = insert_in(&tx, id, new, origin, model)?;
    tx.commit()?;
    Ok(m)
}

/// `insert` inside an already open transaction (the caller commits).
fn insert_in(tx: &Transaction, id: &str, new: Nueva, origin: Option<&Origen>, model: Option<&Modelo>) -> Result<Memoria> {
    let kind = new.kind.with_context(|| tr!("missing kind: fact, decision, gotcha, preference or procedure", "falta el tipo: fact, decision, gotcha, preference o procedure"))?;
    // Redacted before validating: what counts for the title's 200 characters is what is stored.
    let mut redacted = Vec::new();
    let (title, content) = match origin {
        None => (clean_title(&strip_secrets(&new.title, &mut redacted))?, clean_content(&strip_secrets(&new.content, &mut redacted))?),
        Some(_) => (clean_title(&new.title)?, clean_content(&new.content)?),
    };
    let (paths, tags) = (norm_paths(&new.paths), norm_tags(&new.tags));
    if origin.is_none() {
        crate::revisar::comprobar_fecha(&tags)?;
    }
    let (project_id, space_id) = placement(tx, &new.project, new.project_id, new.space_id)?;
    let (hlc, author) = match origin {
        Some(o) => {
            crate::reloj::recibir(tx, o.hlc)?;
            (o.hlc.to_string(), o.author.to_string())
        }
        None => (crate::reloj::sello(tx)?, autor_de(tx, project_id, space_id)?),
    };
    let content_hlc = match origin {
        Some(o) if !o.content_hlc.is_empty() => o.content_hlc.to_string(),
        _ => hlc.clone(),
    };
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project_id, space_id, kind, title, content, paths, tags, pinned, source, created_at, updated_at, hlc, author, content_hlc, extra)
             VALUES (?1, ?2, ?14, ?3, ?4, ?5, ?6, ?7, ?8, ?9, COALESCE(?10, {AHORA}), COALESCE(?11, {AHORA}), ?12, ?13, ?15, ?16)"
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
            // The provenance, only what applies depending on where it lands (see `procedencia::para_memoria`).
            match (origin, crate::procedencia::para_memoria(new.provenance.clone(), project_id, &tags)) {
                (Some(o), _) => o.extra.to_string(),
                (None, p) => {
                    let mut extra = serde_json::Map::new();
                    if let Some(p) = p {
                        extra.insert(crate::procedencia::CLAVE.into(), serde_json::to_value(p)?);
                    }
                    crate::evidencia::anadir(&mut extra, &new.evidence);
                    if let Some(r) = new.review_reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                        extra.insert(CLAVE_MOTIVO_REVISION.into(), r.into());
                    }
                    serde_json::Value::Object(extra).to_string()
                }
            },
        ],
    )?;
    let n = tx.last_insert_rowid();
    let m = leer(tx, id)?;
    index_memory(tx, n, &m)?;
    if let Some(model) = model {
        vectores::guardar(tx, n, model, &m.title, &m.content)?;
    }
    Ok(Memoria { redacted, ..m })
}

pub(crate) fn leer(db: &Connection, id: &str) -> Result<Memoria> {
    db.query_row(&format!("SELECT {COLUMNAS} FROM memory m WHERE m.id = ?1"), [id], de_fila)
        .optional()?
        .ok_or_else(|| anyhow!(tr!("no memory with id {id}", "no existe la memoria {id}")))
}

/// Where a memory from here is, live or in the trash: whether it is global (neither project nor space) and the
/// key of its space (its own or its project's; empty if none). `None` if it isn't here.
pub(crate) fn alcance_de_memoria(db: &Connection, id: &str) -> Result<Option<(bool, String)>> {
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
    let global = crate::sync::de_papelera(&text).is_some_and(|(m, _, _)| m.project_key.is_empty() && m.project.is_empty() && m.space.is_empty());
    Ok(Some((global, crate::sync::espacio_de_papelera(db, &text)?)))
}

/// Full id from the id or its start (what the CLI shows is enough).
pub fn resolver_id(db: &Connection, id: &str) -> Result<String> {
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

pub fn obtener(db: &Connection, id: &str) -> Result<Memoria> {
    leer(db, &resolver_id(db, id)?)
}

/// Error if memory `id` (the full id, live) isn't visible from `base`: from a project, only those of
/// its space (or, with no space, those without one), the global ones and the trade ones of a trade that
/// some project of that space has; from a general folder, all. For what is requested by id (`get`,
/// `history`, `update`, `forget`): so nobody pulls out another client's things even with their id.
pub fn en_ambito(db: &Connection, base: &Ambito, id: &str) -> Result<()> {
    if matches!(base, Ambito::Todos) {
        return Ok(());
    }
    if crate::oficios::oculta(db, &base.amplio(), id)? {
        bail!(tr!(
            "that memory is a trade memory of a trade no project here has (see `context`): it is not in this scope",
            "esa memoria es de un oficio que no tiene ningún proyecto de aquí (ver `context`): no es de este ámbito"
        ));
    }
    let own = crate::proyecto::clave_espacio(db, base.espacio())?;
    match alcance_de_memoria(db, id)? {
        Some((global, space_key)) if !global && space_key != own => bail!(tr!(
            "that memory is not in this scope: it belongs to another space, and from this project you only reach its own space and the global memories; open the session in that project, or in the home folder",
            "esa memoria no es de este ámbito: es de otro espacio, y desde este proyecto solo se llega a su espacio y a las globales; abre la sesión en ese proyecto, o en tu carpeta de usuario"
        )),
        _ => Ok(()),
    }
}

/// The history of a memory seen from `base`: error if the memory isn't visible from there
/// ([`en_ambito`]); empty if it is one of those that a pre-1.8.0 version promoted from a client
/// with the same id (its history was client text, `oficios::historiales_solo_aqui`) and it isn't
/// viewed from a general folder.
pub fn historial_en(db: &Connection, base: &Ambito, id: &str) -> Result<Vec<Version>> {
    let id = resolver_id(db, id)?;
    en_ambito(db, base, &id)?;
    if !matches!(base, Ambito::Todos) && crate::oficios::historial_solo_aqui(db, &id)? {
        return Ok(Vec::new());
    }
    historial(db, &id)
}

pub fn actualizar(db: &mut Connection, id: &str, c: Cambios, model: Option<&Modelo>) -> Result<Memoria> {
    Ok(actualizar_con_cambios(db, id, c, model)?.0)
}

/// Like `actualizar`, and also returns what `replace` and `append` changed, with their context.
pub fn actualizar_con_cambios(db: &mut Connection, id: &str, c: Cambios, model: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    update_from(db, id, c, None, model)
}

/// For sync: the change coming from another computer, with its dates, stamp and author, all in
/// the same transaction (the local version stays in the history).
pub(crate) fn actualizar_con_origen(db: &mut Connection, id: &str, c: Cambios, o: &Origen, model: Option<&Modelo>) -> Result<Memoria> {
    Ok(update_from(db, id, c, Some(o), model)?.0)
}

fn update_from(db: &mut Connection, id: &str, c: Cambios, origin: Option<&Origen>, model: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let r = update_in(&tx, &id, c, origin, model)?;
    tx.commit()?;
    Ok(r)
}

/// `actualizar` inside an already open transaction (the caller commits), with the
/// id already resolved.
fn update_in(tx: &Transaction, id: &str, c: Cambios, origin: Option<&Origen>, model: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    let before = leer(tx, id)?;
    let before_hlc = hlc_de(tx, id)?;
    // The change's stamp: the origin's, or a local one later than that of the version being changed.
    let hlc = match origin {
        Some(o) => {
            crate::reloj::recibir(tx, o.hlc)?;
            o.hlc.to_string()
        }
        None => crate::reloj::sello_tras(tx, &before_hlc)?,
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
    let after = Memoria {
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
        crate::revisar::comprobar_fecha(&new_ones)?;
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
    let judged_changed = after.title != before.title || after.content != before.content || after.tipo_texto() != before.tipo_texto();
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
        return Ok((Memoria { redacted, ..leer(tx, id)? }, changed));
    }
    // The previous snapshot goes to the history.
    save_version(tx, id, &before, None, c.reason.as_deref())?;
    // The person verified the previous text, not this one: changing it from here removes the verification
    // (see `verificado`). What comes from another computer brings its own in `extra`.
    let unverified = if origin.is_none() && judged_changed { crate::verificado::quitar_al_cambiar(tx, id, &before)? } else { None };
    tx.execute(
        &format!(
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, project_id = ?8, space_id = ?11,
             updated_at = {AHORA}, hlc = ?9, author = CASE WHEN author = '' THEN ?10 ELSE author END,
             content_hlc = COALESCE(?12, content_hlc) WHERE id = ?1"
        ),
        params![
            id,
            after.title,
            after.content,
            after.tipo_texto(),
            serde_json::to_string(&after.paths)?,
            serde_json::to_string(&after.tags)?,
            after.pinned,
            project_id,
            hlc,
            autor_de(tx, project_id, space_id)?,
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
    let m = leer(tx, id)?;
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
    Ok((Memoria { redacted, unverified, ..m }, changed))
}

/// After moving it or changing its tags from here: removes from `memory.extra` the provenance that no longer
/// applies (`procedencia::para_memoria`): promoted to a trade, none; global or of a space, only the
/// session. Goes in the same transaction as the change, which has already given it a new stamp.
fn trim_provenance(tx: &Transaction, id: &str) -> Result<()> {
    let (extra, project_id, tags): (String, i64, String) = tx.query_row("SELECT extra, project_id, tags FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    if !extra.contains(crate::procedencia::CLAVE) {
        return Ok(());
    }
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&tags).unwrap_or_default();
    if crate::procedencia::recortar_extra(&mut o, project_id, &tags) {
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    }
    Ok(())
}

/// Hangs a snapshot of `m` on the history of `id`, with the number after its last one.
/// `changed_at` is when it stopped being the live version (now, if not given).
fn save_version(tx: &Transaction, id: &str, m: &Memoria, changed_at: Option<&str>, reason: Option<&str>) -> Result<()> {
    let version: i64 =
        tx.query_row("SELECT COALESCE(MAX(version), 0) + 1 FROM memory_version WHERE memory_id = ?1", [id], |f| f.get(0))?;
    tx.execute(
        &format!(
            "INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, COALESCE(?8, {AHORA}), ?9)"
        ),
        params![
            id,
            version,
            m.title,
            m.content,
            m.tipo_texto(),
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
pub fn absorber(db: &mut Connection, id: &str, ids: &[String], mut c: Cambios, model: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>, Vec<String>)> {
    ensure!(
        c.content.is_some(),
        tr!(
            "absorb needs the merged text: write the content that joins the memories and pass it as `content` (Omoikane doesn't write it)",
            "absorb necesita el texto fundido: escribe el contenido que junta las memorias y pásalo en `content` (Omoikane no lo redacta)"
        )
    );
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let live = leer(&tx, &id)?;
    let mut absorbed: Vec<Memoria> = Vec::new();
    for other in ids {
        let other = resolver_id(&tx, other)?;
        ensure!(other != id, tr!("a memory can't absorb itself", "una memoria no puede absorberse a sí misma"));
        if !absorbed.iter().any(|a| a.id == other) {
            absorbed.push(leer(&tx, &other)?);
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
        crate::veredictos::poner(&tx, &a.id, &id, crate::veredictos::SUSTITUIDA, &format!("absorbed into {id}"), "omoikane")?;
        // Its history, from the oldest to the last live one, after that of the one that stays.
        let reason = format!("absorbed from {}", a.id);
        for v in historial(&tx, &a.id)?.into_iter().rev() {
            let snapshot = Memoria { title: v.title, content: v.content, kind: v.kind.parse().unwrap_or(a.kind), paths: v.paths, tags: v.tags, ..a.clone() };
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
pub fn redactar(db: &mut Connection, id: &str, model: Option<&Modelo>) -> Result<Memoria> {
    let m = obtener(db, id)?;
    let (title, ht) = crate::secretos::redactar(&m.title);
    let (content, hc) = crate::secretos::redactar(&m.content);
    if ht.is_empty() && hc.is_empty() {
        return Ok(m);
    }
    let c = Cambios {
        title: (!ht.is_empty()).then_some(title),
        content: (!hc.is_empty()).then_some(content),
        reason: Some("redacted".into()),
        ..Cambios::default()
    };
    let (r, _) = update_from(db, &m.id, c, None, model)?;
    let tx = escribir_tx(db)?;
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
    Ok(Memoria { redacted: ht.iter().chain(&hc).map(|h| h.tipo).collect(), ..r })
}

/// Rebuilds the word index from the table. Returns how many memories it indexed.
pub fn reindexar_palabras(db: &mut Connection) -> Result<usize> {
    let tx = escribir_tx(db)?;
    tx.execute("DELETE FROM memory_fts", [])?;
    let ids: Vec<(i64, String)> = {
        let mut q = tx.prepare("SELECT n, id FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let all: Vec<(i64, Memoria)> = ids.into_iter().map(|(n, id)| Ok((n, leer(&tx, &id)?))).collect::<Result<_>>()?;
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
/// `restaurar` brings it back. Returns what was forgotten.
pub fn olvidar(db: &mut Connection, id: &str) -> Result<Memoria> {
    delete(db, id, None)
}

/// Forgets and also removes it from the trash: there is no going back.
pub fn olvidar_del_todo(db: &mut Connection, id: &str) -> Result<Memoria> {
    let m = delete(db, id, None)?;
    db.execute("DELETE FROM forgotten WHERE id = ?1", [&m.id])?;
    Ok(m)
}

/// Days a forgotten memory waits in the trash before being deleted for good
/// (`OMOIKANE_TRASH_DAYS`; 0 = never).
pub fn dias_papelera() -> f64 {
    std::env::var("OMOIKANE_TRASH_DAYS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(30.0)
}

/// Deletes from the trash what was forgotten more than `days` ago (0 = nothing). Returns how many.
pub fn purgar(db: &Connection, days: f64) -> Result<usize> {
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
pub fn vaciar_papelera(db: &Connection) -> Result<usize> {
    Ok(db.execute("DELETE FROM forgotten", [])?)
}

/// Deletes these memories (full ids) from the trash for good, and only those: what the TUI empties from a
/// project, which doesn't see another space's trash. Returns how many there were.
pub fn vaciar_papelera_de(db: &mut Connection, ids: &[String]) -> Result<usize> {
    let tx = escribir_tx(db)?;
    let mut n = 0;
    for id in ids {
        n += tx.execute("DELETE FROM forgotten WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// For sync: forget with the mark (stamp and date) coming from another computer. The memory is
/// kept aside in case it has to be brought back (`restaurar`).
pub(crate) fn olvidar_con(db: &mut Connection, id: &str, hlc: &str, deleted_at: &str, machine: &str) -> Result<Memoria> {
    delete(db, id, Some((hlc, deleted_at, machine)))
}

/// A memory in the trash.
#[derive(Clone, Debug, Serialize)]
pub struct Olvidada {
    pub id: String,
    pub title: String,
    pub kind: Tipo,
    pub project: String,
    /// Its space (the key): its own if it was a space memory, or its project's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    /// Who forgot it: the computer's name (and `[by …]` already says the person, if sync records it).
    pub by: String,
    pub deleted_at: String,
}

/// The trash: what was forgotten here or on another computer, from most recent to oldest.
pub fn papelera(db: &Connection) -> Result<Vec<Olvidada>> {
    let mut q = db.prepare("SELECT id, memory, by, deleted_at FROM forgotten ORDER BY deleted_at DESC")?;
    let v = q
        .query_map([], |f| {
            let m: String = f.get(1)?;
            let m: Memoria = serde_json::from_str(&m).unwrap_or_default();
            Ok(Olvidada { id: f.get(0)?, title: m.title, kind: m.kind, project: m.project, space: m.space, by: f.get(2)?, deleted_at: f.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Brings a memory back from the trash (forgotten here or on another computer), with a stamp later than the
/// forget: so it also comes back to the other computers on the next sync. Its previous history
/// is kept.
pub fn restaurar(db: &mut Connection, id: &str, model: Option<&Modelo>) -> Result<Memoria> {
    // The id as in the other commands: uppercase, and a prefix that matches several is an error.
    let id = id.trim().to_uppercase();
    // At least 4 characters, like `get` and `forget` (`resolver_id`): with just one you could pull out of
    // the trash, blindly, what the list doesn't show from this folder.
    ensure!(id.len() >= 4, tr!("id too short: «{id}»", "id demasiado corto: «{id}»"));
    ensure!(id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let tx = escribir_tx(db)?;
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
    let (m, extra, _) = crate::sync::de_papelera(&m).ok_or_else(|| anyhow!(tr!("{id} can't be read from the trash", "{id} no se entiende en la papelera")))?;
    let extra = crate::sync::extra_a_texto(&extra);
    let forgotten_hlc = if m.content_hlc.is_empty() { forgotten_hlc } else { m.content_hlc.clone() };
    let versions: Vec<Version> = serde_json::from_str(&versions).unwrap_or_default();
    let mark: String = tx.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?.unwrap_or_default();
    let hlc = crate::reloj::sello_tras(&tx, &mark)?;
    let (project_id, space_id) = alcance_guardado(&tx, &m)?;
    let new = Nueva {
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
    let o = Origen { created_at: &m.created_at, updated_at: &m.updated_at, hlc: &hlc, content_hlc: &forgotten_hlc, author: &m.author, extra: &extra };
    tx.execute("DELETE FROM tombstone WHERE id = ?1", [&id])?;
    crate::sync::guardar_extra_olvido(&tx, &id, &Default::default())?;
    let r = insert_in(&tx, &id, new, Some(&o), model)?;
    for v in &versions {
        poner_version(&tx, &id, v)?;
    }
    tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
    tx.commit()?;
    Ok(r)
}

/// Where a memory stored outside the database (trash, backup) comes back to: its project by key (or,
/// in backups from before schema 10, by path), its space, or global.
pub(crate) fn alcance_guardado(tx: &Connection, m: &Memoria) -> Result<(i64, Option<i64>)> {
    if !m.project.is_empty() || !m.project_key.is_empty() {
        let id = match crate::proyecto::resolver(tx, &m.project)? {
            Some((id, _)) if m.project_key.is_empty() || crate::proyecto::clave_de(tx, id)? == m.project_key => id,
            _ if !m.project_key.is_empty() => crate::proyecto::de_clave(tx, &m.project_key, &m.project, &crate::db::persona_de(tx), true)?,
            _ => crate::proyecto::registrar(tx, &m.project)?,
        };
        // As when receiving it through sync: a project with no space here takes the one it brings; one
        // with a space keeps its own.
        if !m.space.is_empty() && crate::proyecto::espacio_de(tx, id)?.is_none() {
            let s = crate::proyecto::espacio_de_clave(tx, &m.space)?;
            tx.execute("UPDATE project SET space_id = ?2 WHERE id = ?1", params![id, s])?;
        }
        return Ok((id, None));
    }
    Ok((0, crate::proyecto::espacio_de_clave(tx, &m.space)?))
}

fn delete(db: &mut Connection, id: &str, mark: Option<(&str, &str, &str)>) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let m = delete_in(&tx, &id, mark)?;
    tx.commit()?;
    Ok(m)
}

/// `delete` inside an already open transaction (the caller commits), with the id
/// already resolved.
fn delete_in(tx: &Transaction, id: &str, mark: Option<(&str, &str, &str)>) -> Result<Memoria> {
    let (hlc, deleted_at): (String, Option<&str>) = match mark {
        Some((h, d, _)) => {
            crate::reloj::recibir(tx, h)?;
            (h.to_string(), Some(d))
        }
        None => (crate::reloj::sello_tras(tx, &hlc_de(tx, id)?)?, None),
    };
    let m = leer(tx, id)?;
    marcar_olvido(tx, id, deleted_at, &hlc, &m.space)?;
    // To the trash, with its history: `restaurar` brings it back from any computer. `by` is the
    // computer that forgot it: another one (maybe another person) or this one.
    let who = match mark {
        Some((_, _, machine)) => machine.to_string(),
        None => crate::reloj::equipo(tx)?,
    };
    let versions = historial(tx, id)?;
    let mem_hlc = hlc_de(tx, id)?;
    let (content_hlc, extra): (String, String) = tx.query_row("SELECT content_hlc, extra FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?)))?;
    let m = Memoria { content_hlc, ..m };
    // It goes to the trash with its unknown fields: if it comes back (here or on another computer), they come back with it.
    let text = crate::sync::a_papelera(&m, &crate::sync::extra_de(&extra), &Default::default())?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, COALESCE(?5, {AHORA}), ?6)"),
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
pub(crate) fn poner_version(db: &Connection, id: &str, v: &Version) -> Result<()> {
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
pub fn historial(db: &Connection, id: &str) -> Result<Vec<Version>> {
    let id = resolver_id(db, id)?;
    let mut q = db.prepare(
        "SELECT version, kind, title, content, paths, tags, changed_at, reason, extra
         FROM memory_version WHERE memory_id = ?1 ORDER BY version DESC",
    )?;
    let v = q.query_map([&id], |f| version_from_row(f, 0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The history of every memory that has one, in a single pass (for sync: one
/// query per memory was the slowest part of `omoi sync` with a large database).
pub(crate) fn historiales(db: &Connection) -> Result<std::collections::HashMap<String, Vec<Version>>> {
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

/// A `limit` for [`recientes`] from which all are requested (whoever wants them all passes `usize::MAX >> 1`).
const ALL: usize = 1 << 20;
/// From how many rows (from lowest to highest `n`) they are read in parallel (see [`in_parallel`]).
const PARALLEL_FROM: i64 = 10_000;
/// Connections at once, at most.
const PARALLEL_THREADS: usize = 8;

/// The rows of `sql` (`SELECT {COLUMNAS} FROM memory m WHERE …`, with the kind in `?1`) read in chunks
/// of `n` at a time, each chunk with its own read-only connection: with 50,000 memories, reading them on a single
/// connection takes half a second, almost all of it SQLite and `de_fila`. `None` (and the caller reads them as
/// usual) if it isn't worth it or it can't be guaranteed to give the same: a database with no file, few
/// rows, a transaction open in `db` (the other connections wouldn't see what it has written) or
/// any error. The new connections see the last committed state, like `db` outside a transaction;
/// if another process commits something meanwhile (`data_version` changes), the chunks could come from
/// two different moments and that isn't valid either.
fn in_parallel(db: &Connection, sql: &str, kind: Option<Tipo>) -> Option<Vec<Memoria>> {
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
    let chunks: Vec<Option<Vec<Memoria>>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let (sql, path) = (&sql, &path);
                let (a, b) = (start + i * step, (start + (i + 1) * step - 1).min(end));
                s.spawn(move || -> Option<Vec<Memoria>> {
                    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
                    let c = Connection::open_with_flags(path, flags).ok()?;
                    let mut q = c.prepare(sql).ok()?;
                    q.query_map(params![kind.map(Tipo::as_str), a, b], de_fila).ok()?.collect::<rusqlite::Result<_>>().ok()
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
pub fn recientes(db: &Connection, scope: &Ambito, kind: Option<Tipo>, limit: usize) -> Result<Vec<Memoria>> {
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
        let sql = format!("SELECT {COLUMNAS} FROM memory m WHERE {} AND {kind_filter}", scope.sql());
        let mut v = match in_parallel(db, &sql, kind) {
            Some(v) => v,
            None => db.prepare(&sql)?.query_map([kind.map(Tipo::as_str)], de_fila)?.collect::<rusqlite::Result<_>>()?,
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
        let mut row = db.prepare(&format!("SELECT {COLUMNAS} FROM memory m WHERE m.n = ?1"))?;
        return Ok(ns.into_iter().map(|n| row.query_row([n], de_fila)).collect::<rusqlite::Result<_>>()?);
    }
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory m
         WHERE {} AND {kind_filter}
         ORDER BY m.updated_at DESC, m.id DESC LIMIT ?2",
        scope.sql()
    ))?;
    let v = q.query_map(params![kind.map(Tipo::as_str), limit as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The tag of the memories that say where a project's work was left.
pub const ESTADO: &str = "status";

/// Where each project was left: the most recent memory with the [`ESTADO`] tag of each
/// one (the global ones count as one more project), from newest to oldest.
pub fn estados(db: &Connection, scope: &Ambito, limit: usize) -> Result<Vec<Memoria>> {
    let mut v: Vec<Memoria> = Vec::new();
    for m in con_etiqueta(db, scope, ESTADO)? {
        // One per project; space ones, one per space, and global ones, one.
        if v.len() < limit && !v.iter().any(|x| x.project_id == m.project_id && (m.project_id != 0 || x.space == m.space)) {
            v.push(m);
        }
    }
    Ok(v)
}

/// The tag of the decisions that are up to the person and still unanswered: the agent left the
/// question, the options and what it assumed in the meantime, and kept working.
pub const ABIERTA: &str = "open";

/// The tag that, together with [`ABIERTA`], says the decision is blocking: without the answer, what
/// depends on it can't go on. Without it, the decision is "to review later" (the agent
/// went on with what it assumed). A tag and not a field: it doesn't change the schema or the sync.
pub const BLOQUEANTE: &str = "blocking";

impl Memoria {
    /// Whether it is a blocking open decision (with the [`BLOQUEANTE`] tag).
    pub fn bloqueante(&self) -> bool {
        self.tags.iter().any(|t| t == BLOQUEANTE)
    }
}

/// The open decisions of the scope (of kind decision and with the [`ABIERTA`] tag): blocking ones first
/// and, within each group, from newest to oldest; at most `limit`.
pub fn abiertas(db: &Connection, scope: &Ambito, limit: usize) -> Result<Vec<Memoria>> {
    let mut v: Vec<Memoria> = con_etiqueta(db, scope, ABIERTA)?.into_iter().filter(|m| m.kind == Tipo::Decision).collect();
    // Stable: keeps the date order within each group.
    v.sort_by_key(|m| !m.bloqueante());
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
    /// Its structured data, if it has any ([`poner_datos`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl From<Memoria> for Decision {
    fn from(m: Memoria) -> Decision {
        Decision {
            priority: if m.bloqueante() { "blocking" } else { "later" },
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
pub fn decisiones(db: &Connection, scope: &Ambito, blocking_only: bool, wanted_tags: &[String], limit: usize) -> Result<Vec<Decision>> {
    let wanted = norm_tags(wanted_tags);
    let v: Vec<Memoria> = abiertas(db, scope, usize::MAX)?
        .into_iter()
        .filter(|m| (!blocking_only || m.bloqueante()) && wanted.iter().all(|t| m.tags.contains(t)))
        .take(limit)
        .collect();
    let mut data = datos_de(db, &v.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    Ok(v.into_iter()
        .map(|m| {
            let data = data.remove(&m.id);
            Decision { data, ..Decision::from(m) }
        })
        .collect())
}

/// The tag left on a decision resolved with [`resolver_decision`] (or from the web view).
pub const RESUELTA: &str = "resolved";

/// Resolves an open decision: appends the answer at the end of the content («Answer (date): …»),
/// removes [`ABIERTA`] and [`BLOQUEANTE`] and adds [`RESUELTA`]. With a version and reason, like `update`.
/// One already resolved isn't resolved again (error). In a trade decision the answer goes through
/// `oficios::fugas` like any new text (except what `allow_leaks` lets through): if it names
/// something of a project or a client it isn't saved and the leaks come back (`Ok(Err(…))`).
/// Splits a memory that is too long into several (the «Partir» / Split button in Review, 1.12.1): the first part
/// stays in the original memory (its id, history, links and pinning), and each of the others is
/// a new memory of the same project or space, kind, paths and tags (minus `status`: the project's
/// status is still a single one). The new ones are saved before changing the original, so if something
/// fails halfway no text is lost. Returns the changed original and the new ones, in order.
pub fn partir(db: &mut Connection, id: &str, parts: &[(String, String)], reason: &str) -> Result<Vec<Memoria>> {
    ensure!(parts.len() >= 2, tr!("splitting needs at least two parts", "para partirla hacen falta al menos dos partes"));
    ensure!(
        parts.iter().all(|(t, c)| !t.trim().is_empty() && !c.trim().is_empty()),
        tr!("every part needs a title and some text", "cada parte necesita un título y algo de texto")
    );
    let m = obtener(db, id)?;
    let space_id: Option<i64> = if m.project_id == 0 && !m.space.is_empty() {
        Some(db.query_row("SELECT id FROM space WHERE key = ?1", [&m.space], |f| f.get(0))?)
    } else {
        None
    };
    let tags: Vec<String> = m.tags.iter().filter(|t| *t != ESTADO).cloned().collect();
    let mut new_ones = Vec::new();
    for (title, content) in &parts[1..] {
        let n = Nueva {
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
        new_ones.push(guardar(db, n, None)?);
    }
    let (title, content) = &parts[0];
    let c = Cambios { title: Some(title.trim().to_string()), content: Some(content.trim().to_string()), reason: Some(reason.to_string()), ..Default::default() };
    let first = actualizar(db, &m.id, c, None)?;
    Ok(std::iter::once(first).chain(new_ones).collect())
}

pub fn resolver_decision(
    db: &mut Connection,
    id: &str,
    answer: &str,
    reason: Option<String>,
    allow_leaks: bool,
) -> Result<std::result::Result<Memoria, Vec<crate::oficios::Fuga>>> {
    let m = obtener(db, id)?;
    ensure!(m.kind == Tipo::Decision, tr!("{} is not a decision", "{} no es una decisión", m.id));
    ensure!(
        !m.tags.iter().any(|t| t == RESUELTA),
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
    let day: String = db.query_row(&format!("SELECT substr({AHORA}, 1, 10)"), [], |f| f.get(0))?;
    let mut tags: Vec<String> = m.tags.iter().filter(|t| *t != ABIERTA && *t != BLOQUEANTE).cloned().collect();
    if !tags.iter().any(|t| t == RESUELTA) {
        tags.push(RESUELTA.into());
    }
    let c = Cambios {
        append: Some(format!("Answer ({day}): {answer}")),
        tags: Some(tags),
        reason: Some(reason.unwrap_or_else(|| "decision resolved".into())),
        ..Default::default()
    };
    Ok(Ok(actualizar(db, &m.id, c, None)?))
}

/// The memories of the scope that carry all these tags (and, if asked, of this kind), from newest
/// to oldest, with no ranking or threshold: for programs that save something with a tag and
/// always have to find it again (`omoi list`). At most `limit`.
pub fn con_etiquetas(db: &Connection, scope: &Ambito, wanted_tags: &[String], kind: Option<Tipo>, limit: usize) -> Result<Vec<Memoria>> {
    let wanted = norm_tags(wanted_tags);
    let Some((first, rest)) = wanted.split_first() else {
        bail!(tr!("which tag? Pass at least one with --tag", "¿qué etiqueta? Pasa al menos una con --tag"));
    };
    Ok(con_etiqueta(db, scope, first)?
        .into_iter()
        .filter(|m| kind.is_none_or(|k| m.kind == k) && rest.iter().all(|t| m.tags.contains(t)))
        .take(limit)
        .collect())
}

// --- Structured data --------------------------------------------------------------------------------
//
// A JSON of the memory's own for programs (a project's profile in Kumi, for example), apart
// from the text: it doesn't go into the search index nor into recall, which only read title, content,
// tags and paths. It lives in `memory.extra` under the key [`DATOS`], which is where sync and
// `omoi export` keep the fields of the line they don't know: so it travels as a `data` field of the
// line, and an older version (since 1.0) stores it and resends it as is without understanding it. Without
// changing the schema.

/// The key of the data in `memory.extra` (and the `data` field of the line that travels).
pub const DATOS: &str = "data";
/// Maximum size of the data, in JSON.
pub const DATOS_MAX: usize = 64 * 1024;

/// The data of some memories (those that have any).
pub fn datos_de(db: &Connection, ids: &[&str]) -> Result<std::collections::HashMap<String, serde_json::Value>> {
    let mut r = std::collections::HashMap::new();
    let mut q = db.prepare("SELECT extra FROM memory WHERE id = ?1")?;
    for id in ids {
        if let Some(t) = q.query_row([id], |f| f.get::<_, String>(0)).optional()?
            && let Ok(serde_json::Value::Object(mut o)) = serde_json::from_str::<serde_json::Value>(&t)
            && let Some(d) = o.remove(DATOS)
        {
            r.insert(id.to_string(), d);
        }
    }
    Ok(r)
}

/// Reads data that comes as text (`--data '{…}'`): valid JSON and at most [`DATOS_MAX`].
pub fn datos_de_texto(t: &str) -> Result<serde_json::Value> {
    ensure!(t.len() <= DATOS_MAX, tr!("data over {DATOS_MAX} bytes", "datos de más de {DATOS_MAX} bytes"));
    serde_json::from_str(t).map_err(|e| anyhow!(tr!("data is not valid JSON: {e}", "los datos no son JSON válido: {e}")))
}

/// Sets (or removes, with `None` or `null`) a memory's data. Changes its stamp and its date, so
/// sync carries it, and doesn't create a version (like pinning). Whatever looks like a secret in the
/// texts is masked, as in the content; returns the masked kinds.
pub fn poner_datos(db: &mut Connection, id: &str, data: Option<serde_json::Value>) -> Result<Vec<&'static str>> {
    let mut redacted_kinds = Vec::new();
    let data = ready_data(data, &mut redacted_kinds)?;
    let tx = escribir_tx(db)?;
    let id = resolver_id(&tx, id)?;
    if set_data_in(&tx, &id, data)? {
        tx.commit()?;
    }
    Ok(redacted_kinds)
}

/// The data as stored: without `null`, with whatever looks like a secret masked (records the kinds
/// in `redacted_kinds`) and at most [`DATOS_MAX`].
fn ready_data(data: Option<serde_json::Value>, redacted_kinds: &mut Vec<&'static str>) -> Result<Option<serde_json::Value>> {
    let data = data.filter(|d| !d.is_null()).map(|d| redact_json(d, redacted_kinds));
    if let Some(d) = &data {
        ensure!(serde_json::to_string(d)?.len() <= DATOS_MAX, tr!("data over {DATOS_MAX} bytes", "datos de más de {DATOS_MAX} bytes"));
    }
    Ok(data)
}

/// Whether [`poner_datos`] would accept this data (after masking whatever looks like a secret), without writing:
/// to check it before creating the memory it goes to.
pub(crate) fn comprobar_datos(data: &serde_json::Value) -> Result<()> {
    ready_data(Some(data.clone()), &mut Vec::new()).map(|_| ())
}

/// [`poner_datos`] inside a transaction, with the data already prepared and the full id. `false` if it already
/// had that same data (and then it touches nothing).
fn set_data_in(tx: &Transaction, id: &str, data: Option<serde_json::Value>) -> Result<bool> {
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.get(DATOS) == data.as_ref() {
        return Ok(false);
    }
    match data {
        Some(d) => o.insert(DATOS.into(), d),
        None => o.remove(DATOS),
    };
    let hlc = crate::reloj::sello_tras(tx, &hlc_de(tx, id)?)?;
    tx.execute(&format!("UPDATE memory SET extra = ?2, hlc = ?3, updated_at = {AHORA} WHERE id = ?1"), params![id, serde_json::to_string(&o)?, hlc])?;
    Ok(true)
}

/// The key in `memory.extra` of what to check when the review date arrives.
pub const CLAVE_MOTIVO_REVISION: &str = "review_reason";
/// The maximum length of that reason, in characters.
pub const MAX_MOTIVO_REVISION: usize = 300;

/// The reason for a review date, cleaned: one line, no secrets, truncated. `None` if it is empty.
pub fn limpiar_motivo_revision(t: &str) -> Option<String> {
    let mut t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return None;
    }
    if crate::secretos::activo() {
        t = crate::secretos::redactar(&t).0;
    }
    if t.chars().count() > MAX_MOTIVO_REVISION {
        t = t.chars().take(MAX_MOTIVO_REVISION - 1).collect::<String>() + "…";
    }
    Some(t)
}

/// Sets (or changes) what to check when the review date of `id` arrives, like evidence:
/// no new version and `updated_at` untouched, with a new stamp so it travels. `false` if it already had it.
pub fn poner_motivo_revision(db: &mut Connection, id: &str, reason: &str) -> Result<bool> {
    let id = resolver_id(db, id)?;
    let Some(reason) = limpiar_motivo_revision(reason) else { return Ok(false) };
    let tx = escribir_tx(db)?;
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.get(CLAVE_MOTIVO_REVISION).and_then(|v| v.as_str()) == Some(reason.as_str()) {
        return Ok(false);
    }
    o.insert(CLAVE_MOTIVO_REVISION.into(), reason.into());
    let hlc = crate::reloj::sello_tras(&tx, &hlc_de(&tx, &id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    tx.commit()?;
    Ok(true)
}

/// Adds evidence (`evidencia`) to memory `id`, with no new version and without changing `updated_at` (it doesn't
/// change what it says, but what it is based on), but with a new stamp so it travels with sync.
/// `false` if it already had all of it.
pub fn anadir_evidencia(db: &mut Connection, id: &str, new_ones: &[crate::evidencia::Evidencia]) -> Result<bool> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [&id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if !crate::evidencia::anadir(&mut o, new_ones) {
        return Ok(false);
    }
    let hlc = crate::reloj::sello_tras(&tx, &hlc_de(&tx, &id)?)?;
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
pub fn con_datos(db: &Connection, ms: Vec<Memoria>) -> Result<Vec<serde_json::Value>> {
    let data = datos_de(db, &ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    ms.into_iter()
        .map(|m| {
            let d = data.get(&m.id).cloned();
            let mut v = serde_json::to_value(m)?;
            if let Some(d) = d {
                v[DATOS] = d;
            }
            Ok(v)
        })
        .collect()
}

/// All the memories of the scope with this tag (already lowercase, as stored), from newest
/// to oldest.
pub(crate) fn con_etiqueta(db: &Connection, scope: &Ambito, tag: &str) -> Result<Vec<Memoria>> {
    // Only those that have the word in their tags according to the word index (all those that carry
    // the tag, and maybe a few more, which the rest of the condition removes): without it, checking the tags
    // of each memory read the whole table, content included (500 ms with 100,000 memories).
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM (SELECT rowid AS n FROM memory_fts WHERE memory_fts MATCH ?3) e CROSS JOIN memory m ON m.n = e.n
         WHERE {} AND m.tags LIKE ?2 AND EXISTS (SELECT 1 FROM json_each(m.tags) WHERE value = ?1)
         ORDER BY m.updated_at DESC, m.id DESC",
        scope.sql()
    ))?;
    // The `LIKE` discards, without reading the JSON, what doesn't carry the tag.
    let v = q
        .query_map(params![tag, format!("%\"{tag}\"%"), format!("tags : \"{}\"", tag.replace('"', "\"\""))], de_fila)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// What the agent should always know when starting in a project: the preferences and the pinned ones,
/// of the project, of its space and global (from a general folder, only the global ones). Pinned ones
/// first, then the most recent.
pub fn contexto(db: &Connection, scope: &Ambito, limit: usize) -> Result<Vec<Memoria>> {
    let cond = match scope {
        // Not the trade ones: they belong to the projects of that trade, not to the person.
        Ambito::Todos => "(m.project_id = 0 AND m.space_id IS NULL AND instr(m.tags, '\"trade:') = 0)".to_string(),
        a => a.sql(),
    };
    // The condition, the same as that of the partial index `memory_always`, so it uses it.
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory m
         WHERE {cond} AND (m.pinned = 1 OR m.kind = 'preference')
         ORDER BY m.pinned DESC, m.updated_at DESC, m.id DESC LIMIT ?1"
    ))?;
    let v = q.query_map(params![limit as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// How many memories are visible from a scope. With the per-project-and-space count kept by the
/// triggers (`db::LOCALES`), without scanning the table; by counting, on a connection without it.
pub fn cuantas(db: &Connection, scope: &Ambito) -> Result<i64> {
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
        let mut db = crate::db::abrir(&dir.join("o.db")).unwrap();
        db.execute_batch("PRAGMA synchronous = OFF").unwrap();
        let p = dir.join("p");
        std::fs::create_dir_all(&p).unwrap();
        let key = crate::proyecto::de(&p).unwrap();
        for i in 0..300 {
            let project = if i % 3 == 0 { String::new() } else { key.clone() };
            let kind = Some(if i % 4 == 0 { Tipo::Decision } else { Tipo::Fact });
            guardar(&mut db, Nueva { project, kind, title: format!("memoria {i}"), content: format!("lo que dice la {i}"), ..Default::default() }, None).unwrap();
        }
        // Date ties (the id decides) and an `n` far from the others, so it is read in chunks.
        db.execute_batch(
            "UPDATE memory SET updated_at = '2026-01-01T00:00:00.000Z' WHERE n % 7 = 0;
             UPDATE memory SET n = n + 50000 WHERE n = (SELECT max(n) FROM memory);",
        )
        .unwrap();
        let json = |v: Vec<Memoria>| v.iter().map(|m| serde_json::to_string(m).unwrap()).collect::<Vec<_>>();
        for scope in [Ambito::Todos, Ambito::de(&db, &key).unwrap()] {
            for kind in [None, Some(Tipo::Decision)] {
                let kind_filter = if kind.is_some() { "m.kind = ?1" } else { "?1 IS NULL" };
                let sql = format!("SELECT {COLUMNAS} FROM memory m WHERE {} AND {kind_filter}", scope.sql());
                assert!(in_parallel(&db, &sql, kind).is_some(), "por trozos");
                let by_index = json(recientes(&db, &scope, kind, 1_000).unwrap());
                assert!(by_index.len() > 50);
                assert_eq!(json(recientes(&db, &scope, kind, usize::MAX >> 1).unwrap()), by_index);
                db.execute_batch("BEGIN").unwrap();
                assert!(in_parallel(&db, &sql, kind).is_none(), "en una transacción, no");
                assert_eq!(json(recientes(&db, &scope, kind, usize::MAX >> 1).unwrap()), by_index);
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
        assert_eq!("Gotcha".parse::<Tipo>().unwrap(), Tipo::Gotcha);
        assert!("bugfix".parse::<Tipo>().is_err());
    }
}
