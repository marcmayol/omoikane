//! MCP server over stdio: what the CLI does, as tools for the agent.
//!
//! Each call opens the database in a separate thread (rusqlite is blocking) and resolves the project
//! from the process's folder, which is that of the project where the client started the server. The
//! model is opened once and shared; if it was not installed, it is checked again on every call.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo};
use crate::{db, modelos, proyecto, vectores, veredictos};
use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const INSTRUCTIONS: &str = "Omoikane is this user's long-term memory across sessions, scoped to the current project (plus global memories).
Save a memory with `remember` when you learn something worth knowing next time: a non-obvious bug or trap (kind gotcha, with the file paths it affects), a decision and its reasons (decision), how something in the project works (fact), how the user wants things done (preference), or a procedure that gets repeated, like publishing a release (procedure: numbered steps, plus a line `when: words, that, trigger it`; it comes back whole when the user asks for it).
When you follow a procedure and a step fails or has changed, `update` the procedure with what you learned (and why), so it is right next time.
Keep each memory short: one-line title, then what, why and where. Don't save what is obvious from the code or already in the repo docs.
Say what each memory is based on with `evidence` (what you saw, not how sure you are): `user-said` with the user's words, `code-read` with the file, `tested` with what you ran and what came out, `doc` with the URL, or `inferred` with what you worked it out from. A memory marked inferred has not been checked: check it before relying on it, and when you do, add the new evidence with `update`.
Write each memory in the language the user writes to you in (not in English if they write in Spanish): automatic recall matches the user's own words, so a memory in another language won't come back when it should.
Before working on something, `search` for it. If `remember` answers duplicate_of, update that memory instead (or set force if it really is different).
When a memory changes what you do, say so in one line with its id (\"Following 01M6DSPWW7NZ: build before the smoke test\"): Omoikane counts it as applied in the session diary, so the user can see which memories the work ran under. Don't cite the ones you just saved.
When the user wants a memory to reach the whole team and every agent as a file in the repository (\"make it a skill\"), use `skill` with its id: it writes `.agents/skills/<name>/SKILL.md` and `.claude/skills/<name>/SKILL.md`, which they commit. Procedures and conventions that apply to many tasks fit best; one-off gotchas stay in memory. `skill` with suggest=true proposes candidates with their ids, so the user doesn't have to look for them; with no id it lists the repository's skills and says which are outdated (the memory changed since) or edited by hand; `refresh` brings the outdated ones up to date.
If the session runs in a folder that is not a project (such as the home folder), a memory about one project goes to that project: pass its folder as `project`, or its absolute paths in `paths`. Otherwise it is saved as global.
Projects can belong to a space (a client, a product with several repositories): what applies to every project of the space but not to other clients goes with `space: true` instead of `global`. From a project you see its memories, its space's and the global ones, never another space's. `search` and `recent` take `project` (a folder or part of a project's name) and `space`; `context` says where you are and lists the projects.
Each project has trades (its stack: android, web, wordpress, rust…, detected from its files; `context` lists them). A trap or know-how about the stack itself, that would help any project of that trade of any client (\"don't kill the Android emulator with taskkill\"), goes with `trade: \"android\"`: every project of that trade sees it, and no other. Only knowledge of the stack: never a project's or client's name, paths, repository URLs, domains or data (Omoikane refuses it, see trade_leaks); anything about this project or its client stays in the project. Any kind can be a trade memory: a trade preference (\"new screens go in a Scaffold\") applies from the first session of every project of that trade; `for: \"client\"` keeps it for client projects only (those in a space), `for: \"personal\"` for the user's own. A memory saved in the project that turns out to be of the trade is promoted with `update` and `trade`, rewritten without the client: from a client's project (one in a space) that creates a new trade memory with no history and the client's one stays; from a personal project it keeps its id and history. `force` only skips the duplicate check; if trade_leaks flags something you checked is a false positive (a library's name), pass allow_leaks=true, never by default.
If the user asks to continue something and it is unclear what, do at most one `search` (or `recent`); if that doesn't settle it, ask the user instead of exploring further.
When you finish a piece of work or the session winds down, save where it was left (what is done, what is next) with the tag `status`, so the next session can pick it up; keep one per project: if it already has one, `update` it.
When a product or business decision belongs to the user and you can't ask right now, save it as kind decision with the tag `open` (the question, the options, what you assumed meanwhile) and keep working; when you finish a long piece of work, tell the user which ones you left. Add the tag `blocking` too only when the work can't go on without the answer (no safe assumption, or a wrong guess would be costly to undo): then stop that part and ask; without it the decision is for later. `decisions` lists the open ones, blocking first, and closes one with the user's answer (`resolve` and `answer`).
If a memory stops holding on a known day (a token that expires, an offer, a plan with a date), add the tag `review:YYYY-MM-DD` and say in `review_reason` what to check that day (\"the API token expires\"): that day it shows up at the start of the session with that reason.
When something you remembered changes, `update` it with a reason; `forget` it only when it is wrong or no longer applies (it goes to the trash for 30 days; `update` with restore=true brings it back).
To fix one part of a memory use `update` with `replace` ([{find, with}], each `find` literal and found exactly once); to add a step, `append`; to merge several memories into one, `update` the one that stays with the merged text in `content` and the others' ids in `absorb` (they go to the trash, superseded by it, and their tags, paths and history move to it).
A memory marked ⚠ may be outdated: before relying on it, look at the newer one it names; then `update` the old one (or `forget` it if the new one replaces it), asking the user if it is not clear.
When one memory replaces another, say so: `supersedes` (in `remember` or `update`) with the old one's id. When two disagree and you can't tell which holds, ask the user; if you can't ask now, mark it with `contradicts`: the one it contradicts goes ON HOLD (no automatic recall, lower in search) until the user says which holds. Save their answer with `decisions` (contradiction: both ids, keep: the one that holds or \"both\", answer: their words); the user can also decide in the web view. When a warning pairs two that both hold, `compatible`. That judgment is kept, travels with sync and shows wherever the memories show up.
A memory marked verified was checked by a person: trust it over an unverified one that disagrees. Only the user verifies (`omoi verify` or the web view); never do it yourself, and if you change its text, tell the user it is no longer verified.
Call `context` at the start of a session to get the user's preferences and pinned memories for this project.";

#[derive(Clone)]
pub struct Servidor {
    db: PathBuf,
    dir: Option<PathBuf>,
    modelo: Arc<Mutex<Option<Arc<Modelo>>>>,
    tool_router: ToolRouter<Self>,
}

// --- Parameters ------------------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct Remember {
    /// One line saying what the memory is about.
    title: String,
    /// What, why and where, in a few lines.
    content: String,
    /// fact, decision, gotcha, preference or procedure.
    kind: String,
    /// Project files or folders it refers to (relative to the repo root, or absolute).
    paths: Option<Vec<String>>,
    /// Free tags. `review:YYYY-MM-DD` = flag it for review that day (something that expires).
    tags: Option<Vec<String>>,
    /// Always shown in `context` for this project.
    pinned: Option<bool>,
    /// Applies to every project, not just this one. Preferences are global unless you pass `project` or `space` (or global=false).
    global: Option<bool>,
    /// Applies to every project of this project's space (true), or of the space with this name: for
    /// what is common to one client's repositories and must not reach other clients.
    space: Option<EspacioParam>,
    /// Folder of the project it belongs to, when it is not the current one (for example, the session
    /// runs in the home folder). By default, the current project; from a folder that is not a project,
    /// the repository its absolute paths point to, or global.
    project: Option<String>,
    /// A trade (stack), such as "android", "web" or "wordpress" (several: "android, kotlin"): the memory reaches every project of that trade, of any client, and no other. Only for knowledge of the stack itself, with no project or client names, paths, URLs or data in it.
    trade: Option<String>,
    /// With `trade`: "client" (only for projects in a space, a client's: "when it's for a client, review the landing's SEO"), "personal" (only projects in no space) or "both" (the default).
    #[serde(rename = "for")]
    para: Option<String>,
    /// Save it even if a very similar memory exists. It does not skip the trade leak check (see allow_leaks).
    force: Option<bool>,
    /// With `trade`: save even if trade_leaks flags something you checked is a false positive (a library's name, public docs). Never lets through an absolute path or a repository URL. Don't send it by default.
    allow_leaks: Option<bool>,
    /// A stable key for "the one memory about X" in this project (e.g. `kumi-profile`): if a memory
    /// with this key exists here it is updated, otherwise created. No duplicate check.
    key: Option<String>,
    /// Ids of memories this one replaces: they will show up as superseded by it.
    supersedes: Option<Vec<String>>,
    /// Ids of memories this one disagrees with, when it's not clear which holds and you can't ask the user now: they go on hold until the user says which holds.
    contradicts: Option<Vec<String>>,
    /// With a `review:YYYY-MM-DD` tag (required then): what to check that day, e.g. "the API token expires". It shows up with the memory at the start of the session when the date arrives.
    review_reason: Option<String>,
    /// Structured data for programs (any JSON, up to 64 KB): kept with the memory and returned by `get` (and, with `key`, by `get` with `key` and `resolve`), never searched or recalled.
    data: Option<serde_json::Value>,
    /// What this is based on, so the next agent knows how far to trust it: one or more of {kind, detail, url}. kind: "user-said" (quote the user), "code-read" (the file and line), "tested" (what you ran and what came out), "doc" (what the page says; its URL in url) or "inferred" (you worked it out: from what). Say what you saw, not how sure you are. Memories based only on "inferred" rank lower and show up marked.
    evidence: Option<Vec<crate::evidencia::Evidencia>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Search {
    /// What to look for, in plain words (any language).
    query: String,
    /// Only this kind: fact, decision, gotcha, preference or procedure.
    kind: Option<String>,
    /// Only memories with all these tags.
    tags: Option<Vec<String>>,
    /// How many results (1-50, default 10).
    limit: Option<usize>,
    /// Search every project (of this space, if the project is in one), not just this one and the global memories.
    all_projects: Option<bool>,
    /// Search this project instead: its folder, or its name or part of it ("balua").
    project: Option<String>,
    /// Search this space instead (every project in it, plus its own and the global memories).
    space: Option<String>,
}

/// `space` in `remember`: `true` (the project's space) or a space's name.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EspacioParam {
    Si(bool),
    Nombre(String),
}

#[derive(Deserialize, JsonSchema)]
pub struct Id {
    /// Memory id (its first characters are enough).
    id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Skill {
    /// The memory to turn into a skill: its id (the first characters are enough) or a few words to find it. Without it (and without refresh or suggest), lists the repository's skills from Omoikane with their state.
    id: Option<String>,
    /// Instead, the memories of this project that could be skills and aren't yet (procedures, and memories you applied in 2 sessions or more), with their ids.
    suggest: Option<bool>,
    /// Bring up to date every skill whose memory changed since (not the ones edited by hand).
    refresh: Option<bool>,
    /// Where: "both" (default: `.agents/skills` and `.claude/skills`), "agents" or "claude".
    to: Option<String>,
    /// Replace a skill edited by hand or not from Omoikane, and write it even if it looks like it holds a secret. Only when the user asks.
    force: Option<bool>,
    /// Say what it would write without touching anything.
    dry_run: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Get {
    /// Memory id (its first characters are enough).
    id: Option<String>,
    /// Or the key it was saved with (`remember` with `key`): the one this project sees.
    key: Option<String>,
    /// With `key`: every memory with that key this project sees, most specific first (project, space,
    /// trade, global), each with its scope and data, unmerged.
    resolve: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Update {
    /// Memory id (its first characters are enough).
    id: String,
    title: Option<String>,
    content: Option<String>,
    /// fact, decision, gotcha, preference or procedure.
    kind: Option<String>,
    /// Replaces the paths (an empty list removes them).
    paths: Option<Vec<String>>,
    /// Replaces the tags (an empty list removes them).
    tags: Option<Vec<String>>,
    pinned: Option<bool>,
    /// Move it to the project of this folder ("" makes it global).
    project: Option<String>,
    /// Move it to this space (its name), for every project in it.
    space: Option<String>,
    /// Make it a trade memory ("android", or several: "android, kotlin"), seen by every project of that trade; "" stops it being one (it becomes an ordinary global memory, not back to its project). From a client's project (one in a space) this creates a NEW trade memory and the client's one stays; from a personal one it keeps its id and history.
    trade: Option<String>,
    /// With `trade` (or on a trade memory): save even if trade_leaks flags something you checked is a false positive. Never lets through an absolute path or a repository URL.
    allow_leaks: Option<bool>,
    /// On a trade memory: "client" (only projects in a space, a client's), "personal" (only projects in no space) or "both".
    #[serde(rename = "for")]
    para: Option<String>,
    /// Why it changes; kept with the previous version.
    reason: Option<String>,
    /// Ids of memories this one replaces: they will show up as superseded by it.
    supersedes: Option<Vec<String>>,
    /// Ids of memories this one disagrees with, when it's not clear which holds and you can't ask the user now: they go on hold until the user says which holds.
    contradicts: Option<Vec<String>>,
    /// What to check when its review date (`review:YYYY-MM-DD` tag) arrives. Required when `tags` sets a new review date.
    review_reason: Option<String>,
    /// Ids of memories flagged against this one that both hold: clears the warning.
    compatible: Option<Vec<String>>,
    /// Replace its structured data (any JSON; null removes it). Never searched or recalled.
    #[serde(default, deserialize_with = "double_option")]
    #[schemars(with = "Option<serde_json::Value>")]
    data: Option<Option<serde_json::Value>>,
    /// Bring a forgotten memory back from the trash (with its history, on every synced computer). The other fields are ignored.
    restore: Option<bool>,
    /// More evidence for it (added to what it had, no new version): {kind, detail, url}, kind one of user-said, code-read, tested, doc, inferred. E.g. after you test what an "inferred" memory says.
    evidence: Option<Vec<crate::evidencia::Evidencia>>,
    /// Edit part of the content: each `find` is literal and has to appear exactly once in the current content (otherwise nothing changes and you get the count). Applied in order, before `append`.
    replace: Option<Vec<Reemplazo>>,
    /// Text added at the end of the content (a blank line in between), for one more step in a procedure.
    append: Option<String>,
    /// Ids of memories to merge into this one. Pass the merged text as `content` (you write it; Omoikane doesn't): they go to the trash marked superseded by this one, their tags, paths and history move here, and `[[links]]` to their tags now lead here.
    absorb: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Reemplazo {
    /// Exact text to find (it has to appear exactly once).
    find: String,
    /// What to put in its place (empty removes it).
    with: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Recent {
    /// How many (1-50, default 10).
    limit: Option<usize>,
    /// Only this kind: fact, decision, gotcha, preference or procedure.
    kind: Option<String>,
    /// From every project (of this space, if the project is in one).
    all_projects: Option<bool>,
    /// From this project instead: its folder, or its name or part of it.
    project: Option<String>,
    /// From this space instead.
    space: Option<String>,
    /// Only memories with all these tags: every one in the scope, newest first, with no search or ranking (for programs that save something under a tag and must always find it). Each comes with its `data`.
    tags: Option<Vec<String>>,
    /// Instead of memories, the session diary: one line per past session (when, files touched, memories recalled and saved), newest first. Answers "what did we do on Tuesday?" or "when did we last touch sync.rs?".
    sessions: Option<bool>,
    /// With sessions: only those that ended on or after this date (2026-11-03, or 7d for the last 7 days).
    since: Option<String>,
    /// With sessions: only those that started on or before this date.
    until: Option<String>,
    /// With sessions: only those that touched a file whose path contains this.
    file: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Decisions {
    /// Only the blocking ones (work that can't go on without the user's answer).
    blocking: Option<bool>,
    /// From every project (of this space, if the project is in one).
    all_projects: Option<bool>,
    /// From this project instead: its folder, or its name or part of it.
    project: Option<String>,
    /// From this space instead.
    space: Option<String>,
    /// How many (1-50, default 50).
    limit: Option<usize>,
    /// Only those with all these tags too.
    tags: Option<Vec<String>>,
    /// Resolve this decision instead of listing: `answer` is appended, the `open` and `blocking` tags go and `resolved` is added.
    resolve: Option<String>,
    /// With resolve: the user's answer.
    answer: Option<String>,
    /// With resolve: why (kept with the previous version).
    reason: Option<String>,
    /// With resolve, on a trade decision: save the answer even if trade_leaks flags a false positive (never an absolute path or a repository URL).
    allow_leaks: Option<bool>,
    /// Instead: settle two memories that contradict each other with the user's answer. Both ids; with `keep` and `answer`.
    contradiction: Option<Vec<String>>,
    /// With contradiction: the id of the one that holds (the other becomes superseded by it), or "both" when both hold.
    keep: Option<String>,
}

/// `data` in `update`: without the field, it is left alone; with `null`, the data is removed.
fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<serde_json::Value>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

// --- Responses -------------------------------------------------------------------------------------

/// What is returned in lists: just enough to decide whether to ask for the whole memory with `get`.
#[derive(Serialize)]
struct Compact {
    id: String,
    kind: Tipo,
    title: String,
    snippet: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    global: bool,
    /// Its trades, if it is a trade memory (then it doesn't show up as `global`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    trade: Vec<String>,
    /// Whether a trade memory is only for client or personal projects.
    #[serde(rename = "for", skip_serializing_if = "Option::is_none")]
    para: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// Its space, if it is a space memory (no project) or if the project is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f32>,
    /// Why to take it with care (superseded, contradicted, outdated, unreviewed).
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// «by Marc, 2026-11-08» if a person verified it (`verificado`).
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<String>,
    /// What it is based on (`evidencia`): its kinds, «tested, user-said»; «inferred» if only an agent worked it out.
    #[serde(skip_serializing_if = "Option::is_none")]
    based_on: Option<String>,
}

const SNIPPET_LEN: usize = 200;

fn compact(m: Memoria, with_project: bool, similarity: Option<f32>) -> Compact {
    let flat = m.content.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = if flat.chars().count() > SNIPPET_LEN {
        format!("{}…", flat.chars().take(SNIPPET_LEN).collect::<String>())
    } else {
        flat
    };
    let trade = crate::oficios::de_memoria(&m);
    Compact {
        global: m.global() && trade.is_empty(),
        para: if trade.is_empty() { None } else { crate::oficios::para_de(&m.tags) },
        trade,
        project: (with_project && !m.project.is_empty()).then(|| m.project.clone()),
        space: ((m.de_espacio() || with_project) && !m.space.is_empty()).then(|| m.space.clone()),
        id: m.id,
        kind: m.kind,
        title: m.title,
        snippet,
        paths: m.paths,
        tags: m.tags,
        pinned: m.pinned,
        similarity: similarity.map(|s| (s * 1000.0).round() / 1000.0),
        note: None,
        verified: m.verified.as_ref().map(|v| format!("by {}, {}", v.by, v.at.get(..10).unwrap_or(&v.at))),
        based_on: crate::evidencia::tipos(&m.evidence),
    }
}

/// What to check when the review date arrives, cleaned up. With a date (`has_date`) it is
/// required: a date with no reason means rereading the whole memory to know what to check.
fn review_reason_for(has_date: bool, reason: Option<&str>) -> Result<Option<String>> {
    let reason = reason.and_then(memoria::limpiar_motivo_revision);
    anyhow::ensure!(
        !has_date || reason.is_some(),
        "a review date (tag review:YYYY-MM-DD) needs `review_reason`: what to check that day, e.g. \"the API token expires\""
    );
    Ok(if has_date { reason } else { None })
}

/// The verdicts the agent gives with `supersedes`, `contradicts` and `compatible` about `id`, with their
/// reason (the `update`'s `reason`, or the new memory's title) and who signs them (`source`:
/// the sync person or `agent:<client>`, see `veredictos::autor`).
fn judge(db: &Connection, id: &str, supersedes: Option<Vec<String>>, contradicts: Option<Vec<String>>, compatible: Option<Vec<String>>, reason: &str, source: &str) -> Result<usize> {
    let author = veredictos::autor(db, source, id);
    let mut n = 0;
    for old in supersedes.unwrap_or_default() {
        veredictos::poner(db, &old, id, veredictos::SUSTITUIDA, reason, &author)?;
        n += 1;
    }
    for (list, relation) in [(contradicts, veredictos::CONTRADICE), (compatible, veredictos::COMPATIBLE)] {
        for other in list.unwrap_or_default() {
            veredictos::poner(db, id, &other, relation, reason, &author)?;
            n += 1;
        }
    }
    Ok(n)
}

/// `redacted: N` and the kinds, if something that looked like a secret was removed when saving: the
/// agent has to know (and tell the person), because what it saved is no longer what it sent.
fn with_redacted(r: &mut serde_json::Value, m: &Memoria) {
    if m.redacted.is_empty() {
        return;
    }
    r["redacted"] = serde_json::json!(m.redacted.len());
    r["redacted_types"] = serde_json::json!(crate::secretos::tipos_distintos(&m.redacted));
    r["redacted_hint"] = serde_json::json!("What looked like a secret was replaced by [redacted:<type>] before saving. Tell the user; never save credentials in memory.");
}

fn json(v: &impl Serialize) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(v).unwrap_or_default())])
}

/// A tool failure (it doesn't exist, misspelt kind…) comes back as a result with isError,
/// so the agent reads it and fixes it, instead of as a protocol error.
fn failure(e: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])
}

fn parse_kind(t: Option<String>) -> Result<Option<Tipo>> {
    t.filter(|t| !t.trim().is_empty()).map(|t| t.parse()).transpose()
}

fn limit_of(n: Option<usize>) -> usize {
    n.unwrap_or(10).clamp(1, 50)
}

/// The scope of `search` and `recent`: the session's, or the requested one (`project`, `space`,
/// `all_projects`), never leaving the session's space (from a general folder, everything).
fn requested_scope(db: &Connection, session_project: &str, project: Option<&str>, space: Option<&str>, all: bool) -> Result<Ambito> {
    let base = Ambito::de(db, session_project)?;
    let requested = if let Some(s) = space.map(str::trim).filter(|s| !s.is_empty()) {
        Ambito::Espacio(Some(proyecto::buscar_espacio(db, s)?))
    } else if let Some(p) = project.map(str::trim).filter(|p| !p.is_empty()) {
        let id = proyecto::buscar_en(db, p, &base)?;
        Ambito::Proyecto { id, space: proyecto::espacio_de(db, id)? }
    } else if all {
        base.amplio()
    } else {
        base
    };
    outside_space(&base, &requested)?;
    Ok(requested)
}

/// Error if `requested` can't be seen (or written to) from `base`: another space.
fn outside_space(base: &Ambito, requested: &Ambito) -> Result<()> {
    anyhow::ensure!(
        base.permite(requested),
        "that is in another space: from this project you only see its own space (and the global memories); open the session in that project, or in the home folder, to see it"
    );
    Ok(())
}

/// The full id of a memory requested by id (live, or in the trash with `trash`), with an error if
/// it can't be seen from the session: from a project, only those of its space (or, with no space, those
/// with none) and the global ones; from a general folder, all of them. So `get`, `history`, `update` and
/// `forget` don't leak anything of another client even if the agent has its id.
fn id_in_scope(db: &Connection, session_project: &str, id: &str, trash: bool) -> Result<String> {
    let id = if trash {
        let requested = id.trim().to_uppercase();
        let mut q = db.prepare("SELECT id FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
        let ids: Vec<String> = q.query_map([&requested], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        // Without exactly one match, `restaurar` will say so.
        match <[String; 1]>::try_from(ids) {
            Ok([single]) => single,
            Err(_) => return Ok(requested),
        }
    } else {
        memoria::resolver_id(db, id)?
    };
    let base = Ambito::de(db, session_project)?;
    if trash {
        // In the trash there are no trades to check: only its space.
        let own = proyecto::clave_espacio(db, base.espacio())?;
        return match memoria::alcance_de_memoria(db, &id)? {
            Some((global, space)) if !matches!(base, Ambito::Todos) && !global && space != own => anyhow::bail!(
                "that memory is not in this scope: it belongs to another space, and from this project you only reach its own space and the global memories; open the session in that project, or in the home folder"
            ),
            _ => Ok(id),
        };
    }
    memoria::en_ambito(db, &base, &id)?;
    Ok(id)
}

/// The trades `remember` or `update` ask for: those in `trade` and those in the `trade:` tags the
/// agent sets, validated; and the tags without them.
fn requested_trades(trade: Option<&str>, tags: Vec<String>) -> Result<(Vec<String>, Vec<String>)> {
    let tags: Vec<String> = tags
        .into_iter()
        .map(|t| if t.trim().to_lowercase().starts_with(crate::oficios::PREFIJO) { t.trim().to_lowercase() } else { t })
        .collect();
    let mut v = match trade.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => crate::oficios::lista(t)?,
        None => Vec::new(),
    };
    for o in crate::oficios::de_etiquetas(&tags) {
        let o = crate::oficios::nombre(&o)?;
        if !v.contains(&o) {
            v.push(o);
        }
    }
    Ok((v, crate::oficios::sin_oficio(tags)))
}

/// The space of `remember` (`space: true` or its name) or of `update` (`space: "<name>"`).
fn requested_space(db: &Connection, session_project: &str, p: Option<EspacioParam>) -> Result<Option<i64>> {
    let base = Ambito::de(db, session_project)?;
    let id = match p {
        None | Some(EspacioParam::Si(false)) => return Ok(None),
        Some(EspacioParam::Si(true)) => base.espacio().ok_or_else(|| {
            anyhow::anyhow!("this project is not in a space: give the space's name, or put the project in one with `omoi space assign <space>`")
        })?,
        Some(EspacioParam::Nombre(n)) if n.trim().is_empty() => return Ok(None),
        Some(EspacioParam::Nombre(n)) => proyecto::buscar_espacio(db, &n)?,
    };
    outside_space(&base, &Ambito::Espacio(Some(id)))?;
    Ok(Some(id))
}

// --- Server ----------------------------------------------------------------------------------------

impl Servidor {
    pub fn new(db: PathBuf, dir: Option<PathBuf>) -> Servidor {
        Servidor { db, dir, modelo: Arc::new(Mutex::new(None)), tool_router: Self::tool_router() }
    }

    fn model(&self) -> Result<Option<Arc<Modelo>>> {
        let mut m = self.modelo.lock().expect("modelo");
        if m.is_none() {
            *m = modelos::abrir_actual()?.map(Arc::new);
        }
        Ok(m.clone())
    }

    /// Runs `f` in a blocking thread with the database open, the model (if any) with the vectors up
    /// to date, and the current project.
    async fn with_db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection, Option<&Modelo>, &str) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (path, dir, model) = (self.db.clone(), self.dir.clone(), self.model()?);
        tokio::task::spawn_blocking(move || {
            let mut db = db::abrir(&path)?;
            if let Some(m) = &model {
                vectores::sincronizar(&mut db, m)?;
            }
            let session_project = match &dir {
                Some(d) => proyecto::de(d)?,
                None => proyecto::actual()?,
            };
            // Its trades, if they were never checked: without them it wouldn't see its trade's memories. Optional.
            if let Err(e) = crate::oficios::preparar(&db, &session_project) {
                eprintln!("omoikane: trades: {e:#}");
            }
            f(&mut db, model.as_deref(), &session_project)
        })
        .await?
    }

    /// `remember`, with who saves it (`source`): `mcp:<client>` over MCP, whatever the caller says
    /// over `omoi tool` or HTTP.
    async fn save(&self, p: Remember, source: String) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, model, session_project| {
                let kind = Some(p.kind.parse::<Tipo>()?);
                let space_id = requested_space(db, session_project, p.space)?;
                // Trade memory: `trade`, or the `trade:` tags the agent sets by hand.
                let para = match p.para.as_deref() {
                    Some(x) => crate::oficios::para(x)?,
                    None => crate::oficios::para_de(p.tags.as_deref().unwrap_or_default()),
                };
                let (trades, tags) = requested_trades(p.trade.as_deref(), p.tags.unwrap_or_default())?;
                let review_reason = review_reason_for(crate::revisar::fecha_revision(&tags).is_some(), p.review_reason.as_deref())?;
                let is_trade = !trades.is_empty();
                anyhow::ensure!(is_trade || para.is_none(), "`for` only applies to a trade memory: give `trade` too");
                if is_trade {
                    anyhow::ensure!(
                        space_id.is_none() && p.project.as_deref().is_none_or(|x| x.trim().is_empty()),
                        "a trade memory belongs to no project and no space (every project of the trade sees it): drop `project` and `space`, or drop `trade` to keep it in the project"
                    );
                    // Everything it will carry: text, paths, tags, key and data. `force` doesn't skip it (it is
                    // for duplicates); `allow_leaks`, only what may be a false positive.
                    let paths = p.paths.as_deref().unwrap_or_default().join(" ");
                    let review_text = review_reason.as_deref().unwrap_or_default();
                    let fields = crate::oficios::campos_para_fugas(&[("title", &p.title), ("content", &p.content), ("paths", &paths), ("review_reason", review_text)], &tags, p.key.as_deref(), p.data.as_ref());
                    let leaks = crate::oficios::bloquean(crate::oficios::fugas_en(db, &fields)?, p.allow_leaks.unwrap_or(false));
                    if !leaks.is_empty() {
                        return Ok(serde_json::json!({ "saved": false, "trade_leaks": leaks, "hint": crate::oficios::AVISO_FUGAS }));
                    }
                }
                // Preferences belong to the person, not to a project or a client: global unless
                // told otherwise (`project`, `space`).
                let preference = kind == Some(Tipo::Preference) && p.project.is_none() && space_id.is_none() && p.global.is_none();
                let (project, paths) = match p.project.as_deref().map(str::trim) {
                    _ if is_trade || p.global.unwrap_or(false) || preference => (String::new(), p.paths.unwrap_or_default()),
                    Some("") => (String::new(), p.paths.unwrap_or_default()),
                    Some(d) => (proyecto::de_carpeta(std::path::Path::new(d))?, p.paths.unwrap_or_default()),
                    None => proyecto::para_guardar(session_project, &p.paths.unwrap_or_default(), &format!("{} {}", p.title, p.content)),
                };
                let search_in = if !project.is_empty() && proyecto::es_general(session_project) { session_project.to_string() } else { project.clone() };
                let key = p.key.as_deref().map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
                // With data and no text, the memory is mostly its data: its content is the title.
                let content = if p.content.trim().is_empty() && p.data.as_ref().is_some_and(|d| !d.is_null()) { p.title.clone() } else { p.content.clone() };
                if key.is_none()
                    && !p.force.unwrap_or(false)
                    && let Some(m) = model
                    && let Some((existing, sim)) = if is_trade {
                        // Among those of its trade, which can't be seen from the global ones.
                        vectores::duplicada_de_oficio(db, m, &trades, &p.title, &p.content, None)?
                    } else {
                        vectores::duplicada(db, m, &search_in, &p.title, &p.content)?
                    }
                {
                    return Ok(serde_json::json!({
                        "saved": false,
                        "duplicate_of": compact(existing, false, None),
                        "similarity": (sim * 1000.0).round() / 1000.0,
                        "hint": "A very similar memory exists. Update it with `update` (give a reason), or call `remember` again with force=true if it is a different memory.",
                    }));
                }
                if space_id.is_none() && !project.is_empty() {
                    outside_space(&Ambito::de(db, session_project)?, &Ambito::de(db, &project)?)?;
                }
                // The memories it judges (`supersedes`, `contradicts`), checked BEFORE saving anything,
                // as in `update`: each one can be seen from here and can go together with where the new one
                // will go. If not, an error with no ids or names from another space, and the memory is not saved.
                let global = project.is_empty() && space_id.is_none();
                let its_space = match space_id {
                    Some(s) => proyecto::clave_espacio(db, Some(s))?,
                    None if project.is_empty() => String::new(),
                    None => proyecto::clave_espacio(db, Ambito::de(db, &project)?.espacio())?,
                };
                let mut to_judge: [Vec<String>; 2] = [Vec::new(), Vec::new()];
                for (i, list) in [&p.supersedes, &p.contradicts].into_iter().enumerate() {
                    for other in list.iter().flatten() {
                        let other = id_in_scope(db, session_project, other, false)?;
                        veredictos::comprobar_espacios(db, &other, global, &its_space)?;
                        to_judge[i].push(other);
                    }
                }
                let [supersedes, contradicts] = to_judge;
                // Where it comes from: the session and the commit (see `procedencia`); trade ones, with nothing from the project.
                // Global and space ones, only the session: this folder's branch may be a client's
                // and those memories are seen from others.
                let provenance = if is_trade {
                    None
                } else if project.is_empty() || space_id.is_some() {
                    crate::procedencia::solo_sesion(db, session_project)
                } else {
                    crate::procedencia::actual(db, &project)
                };
                let evidence = if is_trade { Vec::new() } else { crate::evidencia::limpiar(p.evidence.unwrap_or_default())? };
                // A variant of a tag that already exists where it will go (`ideas` with `idea`), replaced by that one.
                let (tags, tags_merged) = if is_trade { (tags, Vec::new()) } else { crate::etiquetas::cerrar(db, &crate::etiquetas::ambito_de(db, &project, space_id)?, tags)? };
                let new = Nueva {
                    project,
                    space_id,
                    kind,
                    title: p.title,
                    content,
                    paths,
                    tags: if is_trade { crate::oficios::etiquetas_de_oficio(tags, &trades, para.as_deref()) } else { tags },
                    pinned: p.pinned.unwrap_or(false),
                    source: source.clone(),
                    provenance,
                    // Trade ones, without evidence: its detail (a file, a quote) would be the project's.
                    evidence: evidence.clone(),
                    ..Default::default()
                };
                let data = p.data.filter(|d| !d.is_null());
                if let Some(d) = &data {
                    anyhow::ensure!(serde_json::to_string(d)?.len() <= memoria::DATOS_MAX, "data over {} bytes", memoria::DATOS_MAX);
                }
                let before = crate::revisar::antes(db)?;
                // With a key: the one with that key in this scope, created or updated in the same transaction
                // (with its data, if given).
                let (mut m, by_key, hidden) = match &key {
                    Some(k) => {
                        let r = memoria::guardar_por_clave(db, k, new, data, model)?;
                        (r.memoria, Some((r.creada, r.parecidas)), r.tapados)
                    }
                    None => {
                        let m = memoria::guardar(db, new, model)?;
                        let hidden = match data {
                            Some(d) => memoria::poner_datos(db, &m.id, Some(d))?,
                            None => Vec::new(),
                        };
                        (m, None, hidden)
                    }
                };
                // The reason for a verdict given when saving is the new memory itself: its title.
                // With a key it may have updated one that already existed: it is added to it.
                if !evidence.is_empty() && memoria::anadir_evidencia(db, &m.id, &evidence)? {
                    m = memoria::obtener(db, &m.id)?;
                }
                if let Some(x) = &review_reason
                    && memoria::poner_motivo_revision(db, &m.id, x)?
                {
                    m = memoria::obtener(db, &m.id)?;
                }
                let judged = judge(db, &m.id, Some(supersedes), Some(contradicts), None, &m.title, &source)?;
                let place = if is_trade {
                    format!("(trade {}{})", trades.join(", "), para.as_deref().map(|x| format!(", {x} projects only")).unwrap_or_default())
                } else if m.de_espacio() {
                    format!("(space {})", m.space)
                } else if m.project.is_empty() {
                    "(global)".to_string()
                } else {
                    m.project.clone()
                };
                let mut r = serde_json::json!({ "saved": true, "id": m.id, "project": place });
                if !tags_merged.is_empty() {
                    r["tags_merged"] = serde_json::json!(tags_merged);
                }
                match crate::evidencia::tipos(&m.evidence) {
                    Some(t) => r["based_on"] = serde_json::json!(t),
                    None if !is_trade => {
                        r["evidence_hint"] = serde_json::json!("No evidence: add what it is based on with `update` and `evidence` (user-said, code-read, tested, doc or inferred, with what you saw).")
                    }
                    None => {}
                }
                if is_trade && let Some(here) = Ambito::de(db, session_project)?.proyecto() {
                    let own = crate::oficios::del_proyecto(db, here)?;
                    let foreign: Vec<&String> = trades.iter().filter(|o| !own.contains(o)).collect();
                    if !foreign.is_empty() {
                        r["trade_hint"] = serde_json::json!(format!(
                            "This project's trades are [{}], so this memory won't show up here, only in projects of {}. If this project is one, `omoi trades add {}` (or the user does).",
                            own.join(", "),
                            foreign.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
                            foreign[0]
                        ));
                    }
                }
                with_redacted(&mut r, &m);
                if !hidden.is_empty() {
                    r["data_redacted"] = serde_json::json!(crate::secretos::tipos_distintos(&hidden));
                }
                if judged > 0 {
                    r["judged"] = serde_json::json!(judged);
                }
                if let Some((created, similar)) = &by_key {
                    r["key"] = serde_json::json!(memoria::clave_de(&m));
                    r["created"] = serde_json::json!(created);
                    if !similar.is_empty() {
                        r["similar_keys"] = serde_json::json!(similar);
                        r["key_hint"] = serde_json::json!("New key, but these keys already exist here: if you meant one of them, save with that key and forget this memory.");
                    }
                }
                let related = if by_key.is_some() { Vec::new() } else { vectores::relacionadas(db, &search_in, &m.title, &m.id, 3)? };
                if !related.is_empty() {
                    r["related"] = serde_json::json!(related.into_iter().map(|x| compact(x, false, None)).collect::<Vec<_>>());
                    r["related_hint"] = serde_json::json!("Saved. These memories may be about the same thing: if one already says this, merge them (`update` that one and `forget` the new one); if not, ignore this.");
                }
                // If it makes another one outdated (higher version, or what was pending there is done), say so now.
                crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                // Only those visible from here: a trade memory may make another client's one outdated.
                let here = Ambito::de(db, session_project)?;
                let olds: Vec<_> = crate::revisar::superadas_por(db, &m.id)?
                    .into_iter()
                    .filter(|(id, _, _)| veredictos::parejas(db).map(|p| !p.contains_key(&veredictos::pareja(id, &m.id))).unwrap_or(true))
                    .filter(|(id, _, _)| here.ve(db, id).unwrap_or(false))
                    .collect();
                if !olds.is_empty() {
                    // One that is already superseded (maybe by a chain) is reported, with the end of the
                    // chain: the agent doesn't have to judge it again.
                    let mut list = Vec::new();
                    let mut chained = false;
                    for (id, title, detail) in &olds {
                        let mut e = serde_json::json!({ "id": id, "title": title, "why": detail });
                        if let Some(end) = veredictos::cadena(db, id)?
                            && here.ve(db, &end.id)?
                        {
                            let t: String = db.query_row("SELECT title FROM memory WHERE id = ?1", [&end.id], |row| row.get(0))?;
                            let via = if end.saltos > 1 { format!(" (via {})", end.saltos) } else { String::new() };
                            e["already_superseded_by"] = serde_json::json!(format!("{} «{}»{via}", end.id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>()));
                            chained = true;
                        }
                        list.push(e);
                    }
                    r["supersedes"] = serde_json::json!(list);
                    let mut hint = "Saved. It looks newer than these memories, which may now be outdated: if this one replaces them, `update` it with `supersedes` (or forget them); if both hold, `compatible`.".to_string();
                    if chained {
                        hint.push_str(" The ones marked already_superseded_by are already replaced (by that memory, at the end of the chain): judge that one instead.");
                    }
                    r["supersedes_hint"] = serde_json::json!(hint);
                }
                if m.content.chars().count() > crate::recall::LARGO_PARA_RECALL {
                    r["length_hint"] = serde_json::json!(format!("Long memory: only its first {} characters count for automatic recall. Consider splitting it into shorter memories, one fact each.", crate::recall::LARGO_PARA_RECALL));
                }
                Ok(r)
            })
            .await;
        if r.as_ref().is_ok_and(|v| v["saved"] == true) {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    /// `update`, with who asks for it (`source`, as in `save`): signs the verdicts it carries.
    async fn change(&self, p: Update, source: String) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, model, session_project| {
                let before = crate::revisar::antes(db)?;
                if p.restore.unwrap_or(false) {
                    let id = id_in_scope(db, session_project, &p.id, true)?;
                    let m = memoria::restaurar(db, &id, model)?;
                    crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                    return Ok(serde_json::to_value(compact(m, false, None))?);
                }
                // The memory and every one named (verdicts, absorb), among those visible from here.
                let base = Ambito::de(db, session_project)?;
                let id = id_in_scope(db, session_project, &p.id, false)?;
                for other in [&p.supersedes, &p.contradicts, &p.compatible, &p.absorb].into_iter().flatten().flatten() {
                    id_in_scope(db, session_project, other, false)?;
                }
                if let Some(d) = p.project.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                    outside_space(&base, &Ambito::de(db, &proyecto::de_carpeta(std::path::Path::new(d))?)?)?;
                }
                // The reason for the change is also that of the verdicts that come with it.
                let reason = p.reason.clone().unwrap_or_default();
                let space_id = requested_space(db, session_project, p.space.map(EspacioParam::Nombre))?;
                // Trades: make it a trade memory (promote it), stop being one, who it is for, or take it out of
                // its trade by moving it to a project or a space. What is new in a trade memory goes
                // through `oficios::fugas`.
                let current = memoria::obtener(db, &id)?;
                // A new review date asks what to check that day; a reason alone, that there is a date.
                let new_date = p.tags.as_deref().and_then(crate::revisar::fecha_revision).is_some_and(|f| crate::revisar::fecha_revision(&current.tags) != Some(f));
                let has_date = crate::revisar::fecha_revision(p.tags.as_deref().unwrap_or(&current.tags)).is_some();
                let gives_reason = p.review_reason.as_deref().is_some_and(|x| !x.trim().is_empty());
                anyhow::ensure!(has_date || !gives_reason, "`review_reason` goes with a review date: add the tag review:YYYY-MM-DD too");
                let review_reason = review_reason_for(new_date, p.review_reason.as_deref())?.or_else(|| p.review_reason.as_deref().and_then(memoria::limpiar_motivo_revision));
                // In a trade memory (or one that will be), the reason goes through the leak check like the text.
                if let Some(x) = &review_reason
                    && (!crate::oficios::de_etiquetas(&current.tags).is_empty() || p.trade.as_deref().is_some_and(|t| !t.trim().is_empty()))
                {
                    let leaks = crate::oficios::bloquean(crate::oficios::fugas_en(db, &crate::oficios::campos_para_fugas(&[("review_reason", x)], &[], None, None))?, p.allow_leaks.unwrap_or(false));
                    if !leaks.is_empty() {
                        return Ok(serde_json::json!({ "updated": false, "trade_leaks": leaks, "hint": crate::oficios::AVISO_FUGAS }));
                    }
                }
                let mut c = Cambios {
                    space_id,
                    title: p.title,
                    content: p.content,
                    kind: parse_kind(p.kind)?,
                    paths: p.paths,
                    tags: p.tags,
                    pinned: p.pinned,
                    project: match p.project.as_deref().map(str::trim) {
                        None => None,
                        Some("") => Some(String::new()),
                        Some(d) => Some(proyecto::de_carpeta(std::path::Path::new(d))?),
                    },
                    reason: p.reason,
                    replace: p.replace.unwrap_or_default().into_iter().map(|r| (r.find, r.with)).collect(),
                    append: p.append,
                    ..Default::default()
                };
                let request = crate::oficios::PedidoCambio {
                    trade: p.trade.as_deref(),
                    para: p.para.as_deref(),
                    cambios: &c,
                    datos: p.data.as_ref().map(|d| d.as_ref().filter(|d| !d.is_null())),
                    permitir_fugas: p.allow_leaks.unwrap_or(false),
                };
                let plan = match crate::oficios::al_cambiar(db, &current, request)? {
                    Ok(plan) => plan,
                    Err(leaks) => return Ok(serde_json::json!({ "updated": false, "trade_leaks": leaks, "hint": crate::oficios::AVISO_FUGAS })),
                };
                c.tags = plan.tags.clone();
                let absorb = p.absorb.filter(|ids| !ids.is_empty());
                // Absorbing takes the history, tags and paths of the absorbed ones: a client's things
                // can't end up in a memory everyone sees (global or trade).
                if let Some(ids) = &absorb {
                    let stays_global = (current.global() && c.project.as_deref().is_none_or(str::is_empty) && c.space_id.is_none()) || plan.hacer_global;
                    for other in ids {
                        let o = memoria::obtener(db, other)?;
                        anyhow::ensure!(
                            !(stays_global && !o.space.is_empty()),
                            "memory {} is a client's: absorbing it would carry its history, tags and paths to a memory every client sees; write what holds into this one and `forget` it instead",
                            o.id
                        );
                    }
                }
                // From a client to a trade: a new memory, and the client's one stays as it was.
                if plan.nueva {
                    anyhow::ensure!(absorb.is_none(), "a client's memory becomes a new trade memory: promote it first, without `absorb`");
                    let tags = c.tags.clone().unwrap_or_else(|| current.tags.clone());
                    let m = crate::oficios::crear_de_oficio(db, &current, &c, tags, &source, model)?;
                    let hidden = match p.data {
                        Some(d) => memoria::poner_datos(db, &m.id, d)?,
                        None => Vec::new(),
                    };
                    judge(db, &m.id, p.supersedes, p.contradicts, p.compatible, &reason, &source)?;
                    crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                    let mut r = serde_json::to_value(compact(m.clone(), false, None))?;
                    with_redacted(&mut r, &m);
                    r["created"] = serde_json::json!(true);
                    r["promoted_from"] = serde_json::json!(current.id);
                    r["promoted_hint"] = serde_json::json!(crate::oficios::AVISO_PROMOVIDA);
                    // The same thing promoted from another client: not prevented (the promotion is done), just reported.
                    if let Some(mo) = model
                        && let Some((other, sim)) = vectores::duplicada_de_oficio(db, mo, &crate::oficios::de_etiquetas(&m.tags), &m.title, &m.content, Some(&m.id))?
                    {
                        r["duplicate_of"] = serde_json::to_value(compact(other, false, None))?;
                        r["similarity"] = serde_json::json!((sim * 1000.0).round() / 1000.0);
                        r["duplicate_hint"] = serde_json::json!(crate::oficios::AVISO_DUPLICADA);
                    }
                    if !hidden.is_empty() {
                        r["data_redacted"] = serde_json::json!(crate::secretos::tipos_distintos(&hidden));
                    }
                    return Ok(r);
                }
                if plan.hacer_global && c.project.is_none() {
                    c.project = Some(String::new());
                }
                // Verdicts only: the memory doesn't change (nor gets an empty version in its history).
                let only_verdicts = c.title.is_none()
                    && c.content.is_none()
                    && c.kind.is_none()
                    && c.paths.is_none()
                    && c.tags.is_none()
                    && c.pinned.is_none()
                    && c.project.is_none()
                    && c.space_id.is_none()
                    && c.replace.is_empty()
                    && c.append.is_none()
                    && absorb.is_none();
                let data = p.data;
                let evidence = crate::evidencia::limpiar(p.evidence.unwrap_or_default())?;
                let (mut m, changed, absorbed) = if only_verdicts {
                    (memoria::obtener(db, &id)?, Vec::new(), Vec::new())
                } else if let Some(ids) = absorb {
                    memoria::absorber(db, &id, &ids, c, model)?
                } else {
                    let (m, changed) = memoria::actualizar_con_cambios(db, &id, c, model)?;
                    (m, changed, Vec::new())
                };
                let hidden = match data {
                    Some(d) => memoria::poner_datos(db, &m.id, d)?,
                    None => Vec::new(),
                };
                if !evidence.is_empty() && crate::oficios::de_etiquetas(&m.tags).is_empty() && memoria::anadir_evidencia(db, &m.id, &evidence)? {
                    m = memoria::obtener(db, &m.id)?;
                }
                if let Some(x) = &review_reason
                    && memoria::poner_motivo_revision(db, &m.id, x)?
                {
                    m = memoria::obtener(db, &m.id)?;
                }
                judge(db, &m.id, p.supersedes, p.contradicts, p.compatible, &reason, &source)?;
                let touched: Vec<String> = std::iter::once(m.id.clone()).chain(absorbed.iter().cloned()).collect();
                crate::revisar::al_dia_de(db, &before, if only_verdicts { &[] } else { &touched })?;
                let mut r = serde_json::to_value(compact(m.clone(), false, None))?;
                with_redacted(&mut r, &m);
                if !hidden.is_empty() {
                    r["data_redacted"] = serde_json::json!(crate::secretos::tipos_distintos(&hidden));
                }
                if !changed.is_empty() {
                    r["changed"] = serde_json::json!(changed);
                }
                if !absorbed.is_empty() {
                    r["absorbed"] = serde_json::json!(absorbed);
                }
                // The person verified the previous text: the agent says so, so they look at it again.
                if let Some(u) = &m.unverified {
                    r["unverified"] = serde_json::json!(format!(
                        "It was verified by {} ({}); changing its text removed that. Tell the user, who can verify it again if it is still right.",
                        u.by,
                        u.at.get(..10).unwrap_or(&u.at)
                    ));
                }
                Ok(r)
            })
            .await;
        if r.is_ok() {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }
}

/// `mcp:<client name>` from what the client said when connecting (`clientInfo`); `mcp` if it
/// didn't say.
fn source_of(peer: &Peer<RoleServer>) -> String {
    let name = peer.peer_info().map(|i| i.client_info.name.trim().to_lowercase()).unwrap_or_default();
    if name.is_empty() { "mcp".into() } else { format!("mcp:{name}") }
}

#[tool_router(router = tool_router)]
impl Servidor {
    #[tool(description = "Save a memory for future sessions. If a very similar memory already exists in this project, nothing is saved and you get duplicate_of: update that one instead, or call again with force=true if it really is different. With `key`, it creates or updates the one memory with that key here.")]
    async fn remember(&self, peer: Peer<RoleServer>, Parameters(p): Parameters<Remember>) -> Result<CallToolResult, ErrorData> {
        self.save(p, source_of(&peer)).await
    }

    #[tool(description = "Search the memories of this project, its space and the global ones (or every project of the space with all_projects, another project with `project`, a space with `space`; from a folder that is not a project, such as the home folder, every project). Returns compact results; use `get` for the whole memory.")]
    async fn search(&self, Parameters(p): Parameters<Search>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, model, session_project| {
                let scope = requested_scope(db, session_project, p.project.as_deref(), p.space.as_deref(), p.all_projects.unwrap_or(false))?;
                let wide = !matches!(scope, Ambito::Proyecto { .. });
                let o = Opciones {
                    ambito: scope,
                    kind: parse_kind(p.kind)?,
                    tags: p.tags.unwrap_or_default(),
                    limite: limit_of(p.limit),
                };
                let res = buscar::buscar(db, &p.query, &o, model)?;
                let mut notes = crate::revisar::notas_busqueda(db, &scope, &res.iter().map(|r| &r.memoria).collect::<Vec<_>>())?;
                Ok(res
                    .into_iter()
                    .map(|r| {
                        let note = notes.remove(&r.memoria.id);
                        Compact { note, ..compact(r.memoria, wide, r.similarity) }
                    })
                    .collect::<Vec<_>>())
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    #[tool(description = "Get a whole memory by id, or by the key it was saved with. It comes with its `data`, if it has any.")]
    async fn get(&self, Parameters(p): Parameters<Get>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, _, session_project| {
                let m = match (p.id.as_deref(), p.key.as_deref()) {
                    (None, Some(k)) if p.resolve.unwrap_or(false) => return Ok(serde_json::to_value(memoria::capas_de_clave(db, &Ambito::de(db, session_project)?, k)?)?),
                    (Some(id), _) => memoria::obtener(db, &id_in_scope(db, session_project, id, false)?)?,
                    (None, Some(k)) => memoria::por_clave(db, &Ambito::de(db, session_project)?, k)?.ok_or_else(|| anyhow::anyhow!("no memory with key «{}» here", k.trim()))?,
                    (None, None) => anyhow::bail!("give id or key"),
                };
                Ok(memoria::con_datos(db, vec![m])?.remove(0))
            })
            .await;
        Ok(r.map(|m| json(&m)).unwrap_or_else(failure))
    }

    #[tool(description = "Change a memory. Only the fields you pass change; the previous version is kept in its history with your reason.")]
    async fn update(&self, peer: Peer<RoleServer>, Parameters(p): Parameters<Update>) -> Result<CallToolResult, ErrorData> {
        self.change(p, source_of(&peer)).await
    }

    #[tool(description = "Forget a memory: it goes to the trash with its history for 30 days, and `update` with restore=true brings it back on every synced computer. Use it only when the memory is wrong or no longer applies; if it just changed, use `update`.")]
    async fn forget(&self, Parameters(p): Parameters<Id>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, _, session_project| {
                let id = id_in_scope(db, session_project, &p.id, false)?;
                let before = crate::revisar::antes(db)?;
                let m = memoria::olvidar(db, &id)?;
                crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                Ok(m)
            })
            .await;
        if r.is_ok() {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|m| json(&serde_json::json!({ "forgotten": m.id, "title": m.title }))).unwrap_or_else(failure))
    }

    #[tool(description = "Turn a memory into a skill of this repository: writes `.agents/skills/<name>/SKILL.md` (Codex, Cursor, Gemini CLI, Copilot, OpenCode…) and `.claude/skills/<name>/SKILL.md` (Claude Code), with the memory's id, so every agent and teammate loads it once committed. `id` can also be a few words; several matches come back as an error listing them. With suggest=true, the project's memories that could be skills, with ids: offer them when the user asks what could be a skill. Without id, lists the repository's skills from Omoikane: up_to_date, outdated (the memory changed: refresh), edited (by hand: left alone) or orphan (its memory is gone). Refuses a memory that looks like it holds a secret. Tell the user to commit the files; never commit them yourself unless asked.")]
    async fn skill(&self, Parameters(p): Parameters<Skill>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, _, session_project| {
                let force = p.force.unwrap_or(false);
                let dry_run = p.dry_run.unwrap_or(false);
                if p.suggest.unwrap_or(false) {
                    return Ok(serde_json::to_value(crate::skill::sugerencias(db, session_project)?)?);
                }
                Ok(match (p.id.as_deref(), p.refresh.unwrap_or(false)) {
                    (Some(wanted), _) => {
                        let id = crate::skill::resolver(db, session_project, wanted)?;
                        let id = id_in_scope(db, session_project, &id, false)?;
                        serde_json::to_value(crate::skill::escribir(db, session_project, &id, crate::skill::Destino::de(p.to.as_deref())?, force, dry_run)?)?
                    }
                    (None, true) => serde_json::to_value(crate::skill::refrescar(db, session_project, dry_run)?)?,
                    (None, false) => serde_json::to_value(crate::skill::listar(db, session_project)?)?,
                })
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    #[tool(description = "Previous versions of a memory, newest first, with the reason for each change.")]
    async fn history(&self, Parameters(p): Parameters<Id>) -> Result<CallToolResult, ErrorData> {
        let r = self.with_db(move |db, _, session_project| memoria::historial_en(db, &Ambito::de(db, session_project)?, &id_in_scope(db, session_project, &p.id, false)?)).await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    #[tool(description = "Where you are (this project: folder, sync key, space, how many memories) and the other projects (latest activity first, with their status), the user's preferences and the pinned memories for this project (its space's and the global ones), what is worth reviewing in its memory (memories a newer one contradicts, stale status, broken paths…), and what earlier sessions set aside as possibly worth saving. Call it at the start of a session.")]
    async fn context(&self) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(|db, _, session_project| {
                // Its trades, looking at its files again (`with_db` only looks the first time).
                if let Some(id) = Ambito::de(db, session_project)?.proyecto() {
                    crate::oficios::al_dia(db, id, session_project)?;
                }
                let scope = Ambito::de(db, session_project)?;
                let v = memoria::contexto(db, &scope, 30)?;
                let memories: Vec<_> = v.into_iter().map(|m| compact(m, false, None)).collect();
                let review = crate::revisar::avisos_sin_esperar(db, &scope)?;
                // Where it is: the project (or the general folder) and, of the others, only those in its space.
                let current = match scope {
                    Ambito::Proyecto { id, .. } if id > 0 => proyecto::lista(db, Some(id), None, 1)?.into_iter().next().map(|mut x| {
                        x.path = session_project.to_string();
                        serde_json::to_value(x).unwrap_or_default()
                    }),
                    Ambito::Todos => Some(serde_json::json!({ "path": session_project, "general": true, "memories": memoria::cuantas(db, &scope)? })),
                    _ => Some(serde_json::json!({ "path": session_project, "memories": 0 })),
                };
                let filter = match scope {
                    Ambito::Todos => None,
                    a => Some(a.espacio()),
                };
                let projects = proyecto::lista(db, None, filter, 50)?;
                let mut r = serde_json::json!({ "project": current, "projects": projects, "memories": memories, "review": review });
                // Duplicated projects or ones in temporary folders: the agent proposes them with the command written out.
                let proposals = crate::nombres::propuestas(db, &scope)?;
                if !proposals.is_empty() {
                    r["projects_review"] = serde_json::json!(proposals);
                    r["projects_review_hint"] = serde_json::json!("These projects look duplicated or are not real projects. Ask the user before running each `command` (a merge reaches every synced computer); if they say no, run its `dismiss`.");
                }
                // The accumulated review notice, as at session start (and not repeated if it already showed today).
                // With the same figures as at the start: without broken paths (they aren't counted there), or
                // having a few would make the count "grow" and repeat the notice in the same session.
                // Secrets do count, they always warn. It is optional: if it fails, only the notice is lost.
                let mut figures = crate::revisar::cifras(&review);
                figures.remove(crate::revisar::RUTA_ROTA);
                if let Some(a) = crate::revisar::aviso(db, &figures, memoria::cuantas(db, &scope)? as usize).ok().flatten() {
                    r["review_notice"] = serde_json::json!(a);
                }
                // What was set aside when other sessions closed: in agents with no recall at start (Grok,
                // Windsurf…) it only arrives here. Shown once, as in recall.
                let set_aside = crate::candidatas::para_empezar(db, session_project, "")?;
                if !set_aside.is_empty() {
                    r["set_aside"] = serde_json::json!(set_aside.iter().map(|c| format!("[{}] {}", c.kind, c.text.chars().take(900).collect::<String>())).collect::<Vec<_>>());
                    r["set_aside_hint"] = serde_json::json!("From earlier sessions here, possibly worth keeping (picked without understanding them): save the ones worth it with `remember`, in the user's words and language, merging into existing memories where they fit; ignore the rest, they won't come back.");
                }
                Ok(r)
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    #[tool(description = "The latest saved or changed memories in this project (its space's and the global ones), or in `project`, `space` or every project of the space (all_projects). With sessions=true, the session diary instead (this computer's past sessions: when, files touched, memories recalled and saved).")]
    async fn recent(&self, Parameters(p): Parameters<Recent>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, _, session_project| {
                let scope = requested_scope(db, session_project, p.project.as_deref(), p.space.as_deref(), p.all_projects.unwrap_or(false))?;
                let wide = !matches!(scope, Ambito::Proyecto { .. });
                if p.sessions.unwrap_or(false) {
                    let filter = crate::diario::Filtro {
                        desde: p.since.as_deref().map(|d| crate::diario::fecha(db, d)).transpose()?,
                        hasta: p.until.as_deref().map(|d| crate::diario::fecha(db, d)).transpose()?,
                        fichero: p.file,
                        carpeta: p.project.is_none().then(|| session_project.to_string()),
                        limite: limit_of(p.limit),
                    };
                    return Ok(serde_json::to_value(crate::diario::lineas(db, &scope, &filter)?)?);
                }
                if let Some(tags) = p.tags.filter(|t| !t.is_empty()) {
                    // By tag: all of them (at most `limit`, 200 if not given), with their data.
                    let v = memoria::con_etiquetas(db, &scope, &tags, parse_kind(p.kind)?, p.limit.unwrap_or(200).clamp(1, 200))?;
                    let data = memoria::datos_de(db, &v.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
                    let mut list = Vec::new();
                    for m in v {
                        let d = data.get(&m.id).cloned();
                        let mut x = serde_json::to_value(compact(m, wide, None))?;
                        if let Some(d) = d {
                            x["data"] = d;
                        }
                        list.push(x);
                    }
                    return Ok(serde_json::Value::Array(list));
                }
                let v = memoria::recientes(db, &scope, parse_kind(p.kind)?, limit_of(p.limit))?;
                Ok(serde_json::to_value(v.into_iter().map(|m| compact(m, wide, None)).collect::<Vec<_>>())?)
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }

    #[tool(description = "The open decisions waiting for the user (kind decision, tag `open`) in this project, its space's and the global ones (or in `project`, `space` or every project of the space with all_projects): blocking first (priority `blocking`: work can't go on without the answer), then the ones for later (priority `later`), newest first within each. With blocking=true, only the blocking ones. With `resolve` and `answer`, closes one; with `contradiction` (two ids), `keep` and `answer`, settles two memories that contradict each other with what the user said.")]
    async fn decisions(&self, Parameters(p): Parameters<Decisions>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .with_db(move |db, _, session_project| {
                if let Some(ids) = p.contradiction {
                    let [a, b] = ids.as_slice() else {
                        anyhow::bail!("`contradiction` takes the two ids that contradict each other")
                    };
                    let (a, b) = (id_in_scope(db, session_project, a, false)?, id_in_scope(db, session_project, b, false)?);
                    let answer = p.answer.unwrap_or_default();
                    anyhow::ensure!(!answer.trim().is_empty(), "`answer`: what the user said (their words); ask them which one holds");
                    let keep = veredictos::Queda::de(db, p.keep.as_deref().unwrap_or_default())?;
                    let before = crate::revisar::antes(db)?;
                    let superseded = veredictos::resolver(db, &a, &b, &keep, &answer, veredictos::USUARIO)?;
                    crate::revisar::al_dia_de(db, &before, &[])?;
                    return Ok(serde_json::json!({
                        "resolved": true,
                        "kept": match &keep { veredictos::Queda::Una(x) => serde_json::json!(x), veredictos::Queda::LasDos => serde_json::json!("both") },
                        "superseded": superseded,
                        "hint": if superseded.is_some() { "The other one is now superseded by the one that holds: offer the user to `forget` it if nothing in it still holds." } else { "Both hold: the warning is gone." },
                    }));
                }
                if let Some(id) = p.resolve {
                    let id = id_in_scope(db, session_project, &id, false)?;
                    let answer = p.answer.unwrap_or_default();
                    let before = crate::revisar::antes(db)?;
                    let m = match memoria::resolver_decision(db, &id, &answer, p.reason, p.allow_leaks.unwrap_or(false))? {
                        Ok(m) => m,
                        Err(leaks) => return Ok(serde_json::json!({ "resolved": false, "trade_leaks": leaks, "hint": crate::oficios::AVISO_FUGAS })),
                    };
                    crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                    return Ok(serde_json::json!({ "resolved": true, "memory": compact(m, false, None) }));
                }
                crate::revisar::limpiar_callado(db);
                let scope = requested_scope(db, session_project, p.project.as_deref(), p.space.as_deref(), p.all_projects.unwrap_or(false))?;
                let v = memoria::decisiones(db, &scope, p.blocking.unwrap_or(false), &p.tags.unwrap_or_default(), p.limit.unwrap_or(50).clamp(1, 50))?;
                Ok(serde_json::to_value(v)?)
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(failure))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Servidor {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omoikane", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// A tool without a server, for clients that don't speak MCP (the Claude Code plugin's mod):
/// the same tools and the same responses. With no name, the list of tools with their schemas
/// and the instructions. Returns the text and whether the tool answered with an error.
/// `source` is the `source` of whatever `remember` saves and signs `update`'s verdicts.
pub fn herramienta(db: PathBuf, dir: Option<PathBuf>, name: Option<&str>, args: &str, source: &str) -> Result<(String, bool)> {
    let s = Servidor::new(db, dir);
    let Some(name) = name else {
        let list = serde_json::json!({ "instructions": INSTRUCTIONS, "tools": s.tool_router.list_all() });
        return Ok((serde_json::to_string(&list)?, false));
    };
    let v: serde_json::Value = if args.trim().is_empty() { serde_json::json!({}) } else { serde_json::from_str(args)? };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let r = rt.block_on(async {
        Ok::<_, anyhow::Error>(match name {
            "remember" => s.save(serde_json::from_value(v)?, source.to_string()).await,
            "search" => s.search(Parameters(serde_json::from_value(v)?)).await,
            "get" => s.get(Parameters(serde_json::from_value(v)?)).await,
            "update" => s.change(serde_json::from_value(v)?, source.to_string()).await,
            "forget" => s.forget(Parameters(serde_json::from_value(v)?)).await,
            "history" => s.history(Parameters(serde_json::from_value(v)?)).await,
            "context" => s.context().await,
            "recent" => s.recent(Parameters(serde_json::from_value(v)?)).await,
            "decisions" => s.decisions(Parameters(serde_json::from_value(v)?)).await,
            "skill" => s.skill(Parameters(serde_json::from_value(v)?)).await,
            other => anyhow::bail!("unknown tool: {other}"),
        })
    })?;
    let r = r.map_err(|e| anyhow::anyhow!("{}", e.message))?;
    let text = r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.as_str())).collect::<Vec<_>>().join("\n");
    Ok((text, r.is_error.unwrap_or(false)))
}

/// Serves over stdin/stdout until the client closes. Nothing else may write to stdout.
pub fn servir(db: PathBuf, dir: Option<PathBuf>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let service = Servidor::new(db, dir).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}
