//! Verdicts (0.17.0): what the agent or the user says about a memory or a pair, and that
//! Omoikane can't know on its own.
//!
//! - `superseded`: `memory_id` is replaced by `other_id` (`remember`/`update` with `supersedes`).
//! - `contradicts`: the two say different things and neither wins yet (`contradicts`).
//! - `compatible`: both hold; removes the warnings for that pair (`compatible`, or "it's fine like this"
//!   on a pair in Review).
//! - `ok:<warning>`: "it's fine like this" on a single-memory warning (`unused`, `due`…).
//!
//! A verdict holds as long as what it judged doesn't change: `superseded`, `compatible` and `ok:*`, as long as
//! the judged memory doesn't change (an "it's fine like this" on a status memory, which changes
//! daily, must not expire every day); `contradicts`, as long as neither of the two changes. "Changing"
//! means changing its title, content or kind, and it is compared by logical clock (schema 10): the stamp of that
//! change (`memory.content_hlc`) is not later than the verdict's (`verdict.hlc`). Before, `updated_at`
//! and `at` were compared, wall clocks of different machines: a machine with its clock ahead left
//! the other machines' verdicts worthless. Stamps have a fixed width, so they are compared as text.
//! For a
//! pair the newest verdict (by its stamp) wins, so a new one doesn't need to delete the
//! previous one, and two machines judging the same pair end up agreeing when they sync.
//!
//! Since schema 9 (0.19.0) each verdict carries its **reason** (`reason`: the `reason` of the
//! `update`, or the title of the new memory in `remember`) and its **author** (`author`: the sync
//! person, `agent:<client>` if there is none, `user` from the view or the CLI). They show wherever
//! the verdict shows: in the recall and `search` note, in Review and in `omoi review`. And
//! supersessions chain: if A is superseded by B and B by C, A's note points to C
//! (`last_in_chain`).

use crate::memoria::NOW;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

pub const SUPERSEDED: &str = "superseded";
pub const CONTRADICTS: &str = "contradicts";
pub const COMPATIBLE: &str = "compatible";
const PAIR_RELATIONS: [&str; 3] = [SUPERSEDED, CONTRADICTS, COMPATIBLE];

/// Who says it from the web view or the CLI ("it's fine like this").
pub const USER: &str = "user";

/// How far a supersession chain goes (a cycle is cut earlier by the path taken).
const MAX_HOPS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub memory_id: String,
    pub other_id: String,
    pub relation: String,
    pub at: String,
    pub hlc: String,
    /// Why (empty in those from before schema 9).
    pub reason: String,
    /// Who: the sync person, `agent:<client>`, `cli` or `user`.
    pub author: String,
    /// The fields this version doesn't know from the line that brought it (`verdict.extra`, as JSON): they
    /// are written again when re-exporting it. Only `all` and `receive` read it; everywhere else it's `{}`.
    pub extra: String,
}

/// The key of a pair, unordered: (a, b) and (b, a) are the same.
pub fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.into(), b.into()) } else { (b.into(), a.into()) }
}

/// Who signs a verdict given here about the memory `memory`: the author that would sign what is
/// saved where it lives (`memoria::author_of`: the one of its space's repository, or the general one) if there
/// is one and, if not, the caller: `mcp:claude-code` → `agent:claude-code`, plain `mcp` → `agent`, and
/// the rest (`cli`, `http`) as is.
pub fn author(db: &Connection, origin: &str, memory: &str) -> String {
    let location: Option<(i64, Option<i64>)> = crate::memoria::resolve_id(db, memory)
        .ok()
        .and_then(|id| db.query_row("SELECT project_id, space_id FROM memory WHERE id = ?1", [id], |row| Ok((row.get(0)?, row.get(1)?))).optional().ok().flatten());
    let person = match location {
        Some((p, s)) => crate::memoria::author_of(db, p, s),
        None => crate::memoria::author_in(db, ""),
    }
    .unwrap_or_default();
    if !person.is_empty() {
        return person;
    }
    match origin.trim() {
        "" | "mcp" => "agent".into(),
        o => match o.strip_prefix("mcp:") {
            Some(client) => format!("agent:{client}"),
            None => o.to_string(),
        },
    }
}

/// The error for a verdict between memories of different spaces, without saying which or whose.
pub fn different_spaces_error() -> String {
    crate::tr!(
        "these memories are in different spaces: a verdict can't join them",
        "estas memorias son de espacios distintos: un veredicto no puede juntarlas"
    )
}

/// Whether a verdict can join the memory `other` (the full id, alive) with one that is global (`global`)
/// or from `space` (its key; empty, from none): to check it before saving the new memory.
pub fn check_spaces(db: &Connection, other: &str, global: bool, space: &str) -> Result<()> {
    if global {
        return Ok(());
    }
    if let Some((g, e)) = crate::memoria::memory_placement(db, other)?
        && !g
        && e != space
    {
        anyhow::bail!(different_spaces_error());
    }
    Ok(())
}

/// Records a verdict from this machine. Empty `other_id` only with `ok:<warning>`. `reason` and `author`
/// may be empty (an "it's fine like this" needs no reason).
pub fn set(db: &Connection, memory_id: &str, other_id: &str, relation: &str, reason: &str, author: &str) -> Result<()> {
    let is_pair = PAIR_RELATIONS.contains(&relation);
    anyhow::ensure!(is_pair || relation.starts_with("ok:"), "unknown verdict «{relation}»");
    let id = crate::memoria::resolve_id(db, memory_id)?;
    let other = if other_id.is_empty() { String::new() } else { crate::memoria::resolve_id(db, other_id)? };
    anyhow::ensure!(is_pair != other.is_empty(), "«{relation}» needs {}", if is_pair { "another memory" } else { "no other memory" });
    anyhow::ensure!(id != other, "a memory can't be judged against itself");
    // Never between memories of different spaces (a global one goes with any): the "superseded
    // by …" note would carry the title of one client's stuff into another client's sessions.
    if !other.is_empty()
        && let (Some((ga, ea)), Some((gb, eb))) = (crate::memoria::memory_placement(db, &id)?, crate::memoria::memory_placement(db, &other)?)
        && !ga
        && !gb
        && ea != eb
    {
        // No ids or space names: the other one's may belong to another client.
        anyhow::bail!(different_spaces_error());
    }
    // Later than the last text change of both: even if that change comes from a machine with its clock
    // far ahead (which doesn't push this clock forward), the verdict given now holds.
    let last: String = db.query_row("SELECT COALESCE(max(content_hlc), '') FROM memory WHERE id IN (?1, ?2)", params![id, other], |row| row.get(0))?;
    let hlc = crate::reloj::stamp_after(db, &last)?;
    db.execute(
        &format!("INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc, reason, author) VALUES (?1, ?2, ?3, {NOW}, ?4, ?5, ?6)"),
        params![id, other, relation, hlc, reason.trim(), author.trim()],
    )?;
    Ok(())
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Verdict> {
    Ok(Verdict { memory_id: row.get(0)?, other_id: row.get(1)?, relation: row.get(2)?, at: row.get(3)?, hlc: row.get(4)?, reason: row.get(5)?, author: row.get(6)?, extra: "{}".into() })
}

/// All of them, with their unknown fields: also those of memories in the trash (`omoi export`
/// carries them all, so that what gets restored comes back with its `superseded`; sync only
/// sends those of memories that travel).
pub fn all(db: &Connection) -> Result<Vec<Verdict>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author, v.extra FROM verdict v
         ORDER BY v.memory_id, v.other_id, v.relation",
    )?;
    let v = q.query_map([], |row| Ok(Verdict { extra: row.get(7)?, ..from_row(row)? }))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// A verdict arriving from another machine: it stays if it's newer than the existing one. Returns whether it went in.
pub fn receive(db: &Connection, v: &Verdict) -> Result<bool> {
    let local: Option<String> = db
        .query_row(
            "SELECT hlc FROM verdict WHERE memory_id = ?1 AND other_id = ?2 AND relation = ?3",
            params![v.memory_id, v.other_id, v.relation],
            |row| row.get(0),
        )
        .optional()?;
    if local.as_deref().is_some_and(|h| !crate::reloj::is_after(&v.hlc, h)) {
        return Ok(false);
    }
    db.execute(
        "INSERT OR REPLACE INTO verdict(memory_id, other_id, relation, at, hlc, reason, author, extra) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author, v.extra],
    )?;
    crate::reloj::receive(db, &v.hlc)?;
    Ok(true)
}

/// What holds for each pair: the newest verdict, if what it judged hasn't changed since.
pub fn pairs(db: &Connection) -> Result<HashMap<(String, String), Verdict>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, v.other_id, v.relation, v.at, v.hlc, v.reason, v.author, a.content_hlc, b.content_hlc FROM verdict v
         JOIN memory a ON a.id = v.memory_id JOIN memory b ON b.id = v.other_id
         WHERE v.other_id != ''",
    )?;
    let rows: Vec<(Verdict, String, String)> = q.query_map([], |row| Ok((from_row(row)?, row.get(7)?, row.get(8)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut r: HashMap<(String, String), Verdict> = HashMap::new();
    let mut latest: HashMap<(String, String), String> = HashMap::new();
    for (v, change_a, change_b) in rows {
        let k = pair_key(&v.memory_id, &v.other_id);
        // The newest wins even if it no longer holds: an old verdict doesn't come back because the new one expires.
        if latest.get(&k).is_some_and(|h| !crate::reloj::is_after(&v.hlc, h)) {
            continue;
        }
        latest.insert(k.clone(), v.hlc.clone());
        let holds = change_a <= v.hlc && (v.relation != CONTRADICTS || change_b <= v.hlc);
        if holds {
            r.insert(k, v);
        } else {
            r.remove(&k);
        }
    }
    Ok(r)
}

/// The end of a supersession chain: (id, hops, reason, author) of the last link.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainEnd {
    pub id: String,
    pub hops: usize,
    pub reason: String,
    pub author: String,
}

/// If `id` is superseded by B and B by C, the last one is C (with how many hops and the reason of the last
/// link). Only a current `superseded` counts, with the same rule as `pairs`: the judged one hasn't
/// changed since and there is no newer verdict on the same pair (stamps compare fine
/// as text: milliseconds and counter are fixed width). A cycle is cut by the path taken and
/// nothing goes past `MAX_HOPS`. `None` if it isn't superseded.
pub fn chain(db: &Connection, id: &str) -> Result<Option<ChainEnd>> {
    let r = db
        .query_row(
            "WITH RECURSIVE vigente AS (
               SELECT v.memory_id, v.other_id, v.reason, v.author FROM verdict v
               JOIN memory a ON a.id = v.memory_id JOIN memory b ON b.id = v.other_id
               WHERE v.relation = ?2 AND a.content_hlc <= v.hlc
                 AND NOT EXISTS (SELECT 1 FROM verdict w
                                 WHERE ((w.memory_id = v.memory_id AND w.other_id = v.other_id) OR (w.memory_id = v.other_id AND w.other_id = v.memory_id))
                                   AND w.hlc > v.hlc)
             ),
             paso(id, saltos, camino, reason, author) AS (
               SELECT ?1, 0, ',' || ?1 || ',', '', ''
               UNION
               SELECT g.other_id, p.saltos + 1, p.camino || g.other_id || ',', g.reason, g.author
               FROM paso p JOIN vigente g ON g.memory_id = p.id
               WHERE p.saltos < ?3 AND instr(p.camino, ',' || g.other_id || ',') = 0
             )
             SELECT id, saltos, reason, author FROM paso WHERE saltos > 0 ORDER BY saltos DESC, id LIMIT 1",
            params![id, SUPERSEDED, MAX_HOPS as i64],
            |row| Ok(ChainEnd { id: row.get(0)?, hops: row.get::<_, i64>(1)? as usize, reason: row.get(2)?, author: row.get(3)? }),
        )
        .optional()?;
    Ok(r)
}

/// The last memory in `id`'s supersession chain and how many hops it takes to reach it.
pub fn last_in_chain(db: &Connection, id: &str) -> Result<Option<(String, usize)>> {
    Ok(chain(db, id)?.map(|f| (f.id, f.hops)))
}

/// The single-memory `ok:<warning>`s: (memory, warning) → when. With `current_only`, only those that
/// still hold (the memory hasn't changed since).
pub fn oks(db: &Connection, current_only: bool) -> Result<HashMap<(String, String), String>> {
    let mut q = db.prepare(
        "SELECT v.memory_id, substr(v.relation, 4), v.at FROM verdict v JOIN memory m ON m.id = v.memory_id
         WHERE v.other_id = '' AND v.relation LIKE 'ok:%' AND (?1 = 0 OR m.content_hlc <= v.hlc)",
    )?;
    let rows: Vec<(String, String, String)> = q.query_map([current_only], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows.into_iter().map(|(id, warning, at)| ((id, warning), at)).collect())
}

/// The reason as it goes in a note: one line, cut at 120 characters.
pub fn short_reason(reason: &str) -> String {
    let flat = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 { format!("{}…", flat.chars().take(120).collect::<String>()) } else { flat }
}

// --- Contradictions on hold (1.14) ----------------------------------------------------------------
//
// A current `contradicts` leaves the contradicted memory (`other_id`: the one that was already there; the one
// contradicting is `memory_id`, the new one or the one being changed) on hold until the person says which holds: the
// automatic recall doesn't bring it, in `search` it ranks lower ([`ON_HOLD_FACTOR`]) and its note says it's waiting.
// The contradicting one shows normally, with its note. It is resolved with [`resolve`]: one supersedes the other
// (`superseded`) or both hold (`compatible`); since the pair's newest verdict wins, the
// `contradicts` stops holding without deleting it. The person can do it from the web view or the agent with
// their answer (`decisions` with `contradiction`), and the reason keeps their words.

/// The weight of a memory on hold when searching.
pub const ON_HOLD_FACTOR: f64 = 0.6;

/// The current contradictions, newest first.
pub fn contradictions(db: &Connection) -> Result<Vec<Verdict>> {
    let mut v: Vec<Verdict> = pairs(db)?.into_values().filter(|v| v.relation == CONTRADICTS).collect();
    v.sort_by(|a, b| b.hlc.cmp(&a.hlc).then_with(|| a.memory_id.cmp(&b.memory_id)));
    Ok(v)
}

/// The memories on hold: the contradicted ones of the current contradictions.
pub fn on_hold(db: &Connection) -> Result<std::collections::HashSet<String>> {
    // Almost always there are none: without `contradicts` in the table there's no need to look at the pairs.
    let any: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM verdict WHERE relation = ?1)", [CONTRADICTS], |row| row.get(0))?;
    if !any {
        return Ok(Default::default());
    }
    Ok(contradictions(db)?.into_iter().map(|v| v.other_id).collect())
}

/// Which one holds in a contradicting pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keep {
    /// This one holds (its id): the other is superseded by it.
    One(String),
    /// Both hold: they talk about different things or complement each other.
    Both,
}

impl Keep {
    /// `keep` as it arrives: an id (or its prefix), or "both".
    pub fn parse(db: &Connection, t: &str) -> Result<Keep> {
        match t.trim().to_lowercase().as_str() {
            "both" | "las dos" | "ambas" => Ok(Keep::Both),
            "" => anyhow::bail!("`keep`: the id of the one that holds, or \"both\""),
            _ => Ok(Keep::One(crate::memoria::resolve_id(db, t)?)),
        }
    }
}

/// The person's decision on the pair `a`-`b`: `One(x)` leaves the other superseded by `x`;
/// `Both`, compatible. `reason`, what they said (or empty). Returns the one left superseded, if any.
pub fn resolve(db: &Connection, a: &str, b: &str, keep: &Keep, reason: &str, author: &str) -> Result<Option<String>> {
    let (a, b) = (crate::memoria::resolve_id(db, a)?, crate::memoria::resolve_id(db, b)?);
    anyhow::ensure!(a != b, "a memory can't be judged against itself");
    match keep {
        Keep::Both => {
            set(db, &a, &b, COMPATIBLE, reason, author)?;
            Ok(None)
        }
        Keep::One(x) => {
            let other = if *x == a {
                b
            } else if *x == b {
                a
            } else {
                anyhow::bail!("`keep` has to be one of the two memories, or \"both\"")
            };
            set(db, &other, x, SUPERSEDED, reason, author)?;
            Ok(Some(other))
        }
    }
}
