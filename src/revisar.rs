//! What is worth reviewing in memory (Phase 7.1, and the broken links of 7.4): memories that a newer
//! one contradicts, statuses that have fallen behind, paths that no longer exist, memories never used
//! and notes too long for recall. No model: words, dates and versions.
//!
//! Outdated ones are computed in pairs and stored in `review`; they recompute themselves when the
//! database changes (via a fingerprint of `memory`), whichever way the change comes in (CLI, MCP, sync,
//! import). The rest depends on the time or the disk and is checked on read. "It's fine as it is" is
//! recorded in the same table and holds until the memory changes again.
//!
//! Whoever knows which memories it wrote (remember, update, forget, restore, absorb) calls `refresh_for`,
//! which only redoes the pairs and links those memories take part in, with candidates taken from the
//! text index; sync, import and reads, which don't know, call `refresh`, which redoes everything. With
//! 50,000 memories, saving went from 2.9 s to about 15 ms of review. When working incrementally, which
//! word is rare is taken from the last full recomputation (in `meta`), which is repeated when the number
//! of memories changes by more than 5 %.
//!
//! Calibrated with Marc's 137 real memories of 29-oct-2026: the case that motivated it shows up (the
//! Omoikane note still on 0.2.0 versus the status one on 0.10.0) and no false positives. The first
//! versions of the detector gave five warnings, four of them false: "pendiente" in one and "hecho" in
//! another, talking about different things.

use crate::memoria::{NOW, Scope, Memory};
use crate::recall::RECALL_LEN;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const OUTDATED: &str = "outdated";
pub const STALE_STATUS: &str = "stale_status";
pub const BROKEN_PATH: &str = "broken_path";
pub const UNUSED: &str = "unused";
pub const TOO_LONG: &str = "too_long";
pub const BROKEN_LINK: &str = "broken_link";
/// One the agent said another replaces (`supersedes`), or that contradicts another (`contradicts`).
pub const SUPERSEDED: &str = crate::veredictos::SUPERSEDED;
pub const CONTRADICTS: &str = crate::veredictos::CONTRADICTS;
/// The review date for its kind has arrived (`review_period_days`).
pub const DUE: &str = "due";
/// Contains something that looks like a secret (`secretos`): saved before 0.19.0 or with `OMOIKANE_REDACT=0`.
pub const SECRET: &str = "secret";
/// The date someone set when saving it has arrived (`review:YYYY-MM-DD`): a token that expires,
/// an offer, a plan with a day. Always warns at session start, like a secret.
pub const REVIEW_DATE: &str = "review_date";
/// The review date tag: `review:2026-11-25`.
pub const REVIEW_DATE_TAG: &str = "review:";
/// Its files have changed a lot since it was written or reviewed (`deriva`).
pub const CODE_DRIFT: &str = crate::deriva::CODE_DRIFT;
/// Two memories of the same trade that say the same thing (the same thing promoted from two clients,
/// or saved with `force`): to be merged. Computed on read, with the stored vectors
/// ([`trade_duplicates`]).
pub const TRADE_DUPLICATE: &str = "trade_duplicate";
const KINDS: [&str; 13] = [SECRET, REVIEW_DATE, SUPERSEDED, CONTRADICTS, OUTDATED, TRADE_DUPLICATE, BROKEN_LINK, STALE_STATUS, CODE_DRIFT, DUE, BROKEN_PATH, UNUSED, TOO_LONG];

/// Days untouched after which a `status` memory with later changes is flagged.
pub const STATUS_DAYS: f64 = 7.0;
/// Days untouched after which an old status goes to the trash by itself, if its project already has
/// a newer one (`cleanup`): by then it has warned for two weeks and nobody reads it, because session
/// start shows the latest.
pub const STATUS_DAYS_TO_TRASH: f64 = 21.0;
/// A global status belongs to no project: what changes in the other global ones doesn't leave it
/// behind. It only warns after being untouched this long.
pub const GLOBAL_STATUS_DAYS: f64 = 30.0;

/// Whether a status tells of work already closed ("hecho", "publicada", "descartado") and leaves nothing
/// pending: it doesn't go stale, because what it says won't change. Looks at the title and the start.
pub fn is_closed_status(m: &Memory) -> bool {
    let start: String = m.content.chars().take(240).collect();
    let t = normalize(&format!("{}\n{}", m.title, sentences(&start).first().copied().unwrap_or("")));
    (contains_word(&t, DONE) || contains_word(&t, CLOSED)) && !contains_word(&t, PENDING)
}

/// What, besides [`DONE`], says a piece of work was closed.
const CLOSED: &[&str] = &["descartado", "descartada", "cerrado", "cerrada", "completado", "completada", "finished", "closed", "discarded"];

/// `cleanup` without failing the caller (an error is only reported on stderr) and, if something
/// changed, with the warnings up to date. Returns whether something changed, to schedule the sync.
pub fn cleanup_quietly(db: &mut Connection) -> bool {
    match cleanup(db) {
        Ok(cleanup) => {
            let changed = !(cleanup.statuses_trashed.is_empty() && cleanup.decisions_closed.is_empty() && cleanup.blockers_downgraded.is_empty());
            if changed && let Err(e) = refresh(db) {
                eprintln!("omoikane: review: {e:#}");
            }
            changed
        }
        Err(e) => {
            eprintln!("omoikane: cleanup: {e:#}");
            false
        }
    }
}

/// What `cleanup` changed by itself.
#[derive(Debug, Default, Serialize)]
pub struct Cleanup {
    /// Old statuses that went to the trash (their project already has a newer one).
    #[serde(rename = "estados_a_papelera")]
    pub statuses_trashed: Vec<String>,
    /// Open decisions that were closed: a later one of the same task is already resolved.
    #[serde(rename = "decisiones_cerradas")]
    pub decisions_closed: Vec<String>,
    /// Blocking ones that become "for later": a later task has already been resolved in their project.
    #[serde(rename = "bloqueantes_rebajadas")]
    pub blockers_downgraded: Vec<String>,
}

/// Whether a memory is written in Spanish (what `cleanup` adds to it goes in its language, not in that
/// of whoever cleans it): more standalone Spanish words than English ones, or some letter only Spanish has.
fn is_spanish(t: &str) -> bool {
    let t = format!(" {} ", t.to_lowercase());
    let count = |ws: &[&str]| ws.iter().map(|w| t.matches(&format!(" {w} ")).count()).sum::<usize>();
    let es = count(&["el", "la", "los", "las", "de", "que", "en", "y", "con", "para", "por", "una", "no", "se"]);
    let en = count(&["the", "of", "and", "to", "in", "is", "for", "with", "that", "it", "not", "on"]);
    es > en || (es == en && t.contains(['ñ', 'á', 'é', 'í', 'ó', 'ú']))
}

/// Whether it was left by the Kumi task agent: by the tag or by the title ("Kumi: …"), since some of its
/// approvals come without the tag.
fn from_kumi(m: &Memory) -> bool {
    m.tags.iter().any(|t| t == "kumi") || m.title.starts_with("Kumi:")
}

/// The tags that tie a memory to an agent's task (`tarea:…`, `trabajo:…`).
fn task_tags(m: &Memory) -> Vec<&str> {
    m.tags.iter().map(String::as_str).filter(|t| t.starts_with("tarea:") || t.starts_with("trabajo:") || t.starts_with("task:")).collect()
}

/// What fixes itself, without asking anyone, and always reversibly (the trash or the history). Runs at
/// session start, when opening the web view and in `omoi review` / `omoi decisions`:
///
/// - A live status untouched for [`STATUS_DAYS_TO_TRASH`], when its project already has a newer one,
///   goes to the trash: it had been warning for two weeks and session start only reads the latest.
/// - An open decision of a task (`tarea:X`) is closed when a later memory of that task is already
///   resolved: the answer says which one.
/// - An agent's blocking decision (Kumi: `kumi` + `blocking`) becomes "for later" when a decision of a
///   later task has already been resolved in its project: the work went on without it.
pub fn cleanup(db: &mut Connection) -> Result<Cleanup> {
    use crate::memoria::{self, OPEN, BLOCKING, STATUS, RESOLVED};
    let mut r = Cleanup::default();
    let now = db_now(db)?;

    // Statuses: in each project the newest stays; the others, old and live, go to the trash.
    let statuses = memoria::with_tag(db, &Scope::All, STATUS)?;
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    for m in &statuses {
        if m.project_id == 0 {
            continue;
        }
        if !seen.insert((m.project_id, m.space.clone())) && days_between(&m.updated_at, &now) > STATUS_DAYS_TO_TRASH && !is_closed_status(m) && !m.pinned {
            memoria::forget(db, &m.id)?;
            r.statuses_trashed.push(m.id.clone());
        }
    }

    // Open decisions.
    let open: Vec<Memory> = memoria::with_tag(db, &Scope::All, OPEN)?.into_iter().filter(|m| m.kind == memoria::Kind::Decision).collect();
    if open.is_empty() {
        return Ok(r);
    }
    let resolved = memoria::with_tag(db, &Scope::All, RESOLVED)?;
    for d in &open {
        let tasks = task_tags(d);
        let proof = resolved.iter().find(|x| x.id != d.id && x.updated_at > d.updated_at && task_tags(x).iter().any(|t| tasks.contains(t)));
        if let Some(x) = proof {
            let title: String = x.title.chars().take(100).collect();
            let short = &x.id[..12.min(x.id.len())];
            let answer = if is_spanish(&d.content) {
                format!("Cerrada sola: otra memoria posterior de la misma tarea ya está resuelta ({short} «{title}»).")
            } else {
                format!("Closed by itself: a later memory of the same task is already resolved ({short} «{title}»).")
            };
            if let Ok(Ok(_)) = memoria::resolve_decision(db, &d.id, &answer, Some("closed by Omoikane: same task resolved later".into()), false) {
                r.decisions_closed.push(d.id.clone());
            }
            continue;
        }
        let by_agent = from_kumi(d);
        if d.is_blocking() && by_agent && d.project_id != 0 {
            let later = resolved.iter().find(|x| {
                x.project_id == d.project_id && x.kind == memoria::Kind::Decision && x.created_at > d.created_at && from_kumi(x) && !task_tags(x).iter().any(|t| tasks.contains(t))
            });
            if let Some(x) = later {
                let tags: Vec<String> = d.tags.iter().filter(|t| *t != BLOCKING).cloned().collect();
                let short = &x.id[..12.min(x.id.len())];
                let note = if is_spanish(&d.content) {
                    format!("Ya no bloquea: después se resolvió otra tarea de este proyecto ({short}). Decídela cuando puedas.")
                } else {
                    format!("No longer blocking: a later task of this project was resolved after it ({short}). Decide it when you can.")
                };
                let c = memoria::Changes { tags: Some(tags), append: Some(note), reason: Some("no longer blocking: the project moved on".into()), ..Default::default() };
                memoria::update(db, &d.id, c, None)?;
                r.blockers_downgraded.push(d.id.clone());
            }
        }
    }
    Ok(r)
}

/// Whether a status may warn that it has fallen behind: it has the tag, doesn't tell of something
/// closed and, if global, has been untouched for [`GLOBAL_STATUS_DAYS`] (otherwise [`STATUS_DAYS`]).
fn status_warns(m: &Memory, days: f64) -> bool {
    let global = m.project_id == 0 && m.space.is_empty();
    m.tags.iter().any(|t| t == crate::memoria::STATUS) && days > if global { GLOBAL_STATUS_DAYS } else { STATUS_DAYS } && !is_closed_status(m)
}
/// Days since it was created without ever being recalled.
pub const UNUSED_DAYS: f64 = 30.0;

/// Every how many untouched days it is worth checking that a memory is still true. A decision or a
/// procedure goes stale sooner than a fact or a gotcha; a preference holds forever until the user
/// says otherwise, and a `status` already has its own warning. "It's fine as it is" resets the
/// clock to zero.
pub fn review_period_days(m: &Memory) -> Option<f64> {
    use crate::memoria::Kind;
    if m.tags.iter().any(|t| t == crate::memoria::STATUS) {
        return None;
    }
    match m.kind {
        Kind::Decision | Kind::Procedure => Some(120.0),
        Kind::Fact | Kind::Gotcha => Some(180.0),
        Kind::Preference => None,
    }
}

/// A memory's review date (the `review:YYYY-MM-DD` tag), if it has a valid one.
pub fn review_date_of(tags: &[String]) -> Option<&str> {
    tags.iter().filter_map(|t| t.strip_prefix(REVIEW_DATE_TAG)).find(|f| is_date(f))
}

/// The review date written in the text rather than in a tag: "[review-after] 10-nov-2026"
/// (some agents leave it like that). Without this, the date warning never showed up.
pub fn written_review_date(text: &str) -> Option<String> {
    let i = text.find("[review-after]")?;
    let rest: String = text[i + "[review-after]".len()..].chars().take(40).collect();
    dates(&normalize(&rest)).into_iter().next()
}

/// `YYYY-MM-DD` with a possible month and day (the 31st of any month is accepted: enough for comparing).
fn is_date(f: &str) -> bool {
    let b = f.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !digits(0..4) || !digits(5..7) || !digits(8..10) {
        return false;
    }
    let (month, day) = (f[5..7].parse::<u8>().unwrap_or(0), f[8..10].parse::<u8>().unwrap_or(0));
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// A `review:` tag that isn't a date would never warn: better to say so when saving.
pub fn check_review_date(tags: &[String]) -> Result<()> {
    if let Some(t) = tags.iter().find(|t| t.strip_prefix(REVIEW_DATE_TAG).is_some_and(|f| !is_date(f))) {
        anyhow::bail!(crate::tr!(
            "tag «{t}»: a review date goes as review:YYYY-MM-DD (e.g. review:2026-11-25)",
            "etiqueta «{t}»: la fecha de revisión va como review:AAAA-MM-DD (p. ej. review:2026-11-25)"
        ));
    }
    Ok(())
}

/// The review date if it has arrived (`today`, the local day) and nobody has touched the memory or
/// marked it fine since that day. Recalling it doesn't count: the agent receiving it doesn't say it still holds.
fn date_reached(m: &Memory, reviewed: &HashMap<(String, String), String>, today: &Today) -> Option<String> {
    let f = review_date_of(&m.tags).map(str::to_string).or_else(|| written_review_date(&m.content))?;
    // An already resolved decision waits for nobody, even if its text carries the date.
    if m.tags.iter().any(|t| t == crate::memoria::RESOLVED) {
        return None;
    }
    let f = f.as_str();
    if f > today.day.as_str() {
        return None;
    }
    // Stored dates are UTC and `f` is a local day: "since that day" means since its local midnight
    // converted to UTC. Comparing with plain `f`, whoever marked it fine between midnight and the UTC
    // hour (east of Greenwich) left a stamp from the previous day and the warning never went away.
    let start = utc_start(f, today.offset);
    if m.updated_at.as_str() >= start.as_str() {
        return None;
    }
    if reviewed.get(&(m.id.clone(), REVIEW_DATE.to_string())).is_some_and(|at| at.as_str() >= start.as_str()) {
        return None;
    }
    Some(f.to_string())
}

/// The memories in `scope` whose review date has arrived, with that date, from oldest to newest:
/// they go at session start with what to check (`review_reason`), along with the open decisions.
pub fn reached_review_dates(db: &Connection, scope: &Scope) -> Result<Vec<(Memory, String)>> {
    let sql = format!(
        "SELECT {} FROM memory m WHERE {} AND (instr(m.tags, '{REVIEW_DATE_TAG}') > 0 OR instr(m.content, '[review-after]') > 0)",
        crate::memoria::COLUMNS,
        scope.sql()
    );
    let mut q = db.prepare(&sql)?;
    let ms: Vec<Memory> = q.query_map([], crate::memoria::from_row)?.collect::<rusqlite::Result<_>>()?;
    if ms.is_empty() {
        return Ok(Vec::new());
    }
    let (reviewed, today) = (crate::veredictos::oks(db, false)?, Today::load(db)?);
    let mut r: Vec<(Memory, String)> = ms.into_iter().filter_map(|m| date_reached(&m, &reviewed, &today).map(|f| (m, f))).collect();
    r.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.id.cmp(&b.0.id)));
    Ok(r)
}

/// The local day and how many seconds local time is ahead of UTC.
struct Today {
    day: String,
    offset: i64,
}

impl Today {
    fn load(db: &Connection) -> Result<Today> {
        let (day, offset) = db.query_row(
            "SELECT date('now', 'localtime'), CAST(strftime('%s', 'now', 'localtime') AS INTEGER) - CAST(strftime('%s', 'now') AS INTEGER)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Today { day, offset })
    }
}

/// Local midnight of day `f` (`YYYY-MM-DD`) in UTC, as `YYYY-MM-DDTHH:MM:SS`, comparable with the
/// stored dates (`…Z`).
fn utc_start(f: &str, offset: i64) -> String {
    let n = |r: std::ops::Range<usize>| f.get(r).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let s = days_since_1970(n(0..4), n(5..7), n(8..10)) * 86_400 - offset;
    let (days, rest) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Days since 1970-01-01 (proleptic Gregorian calendar; H. Hinnant's algorithm).
fn days_since_1970(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a number of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[derive(Clone, Debug, Serialize)]
pub struct Notice {
    pub kind: String,
    pub memory_id: String,
    pub title: String,
    pub project: String,
    /// The other memory (the newer one, for outdated ones); empty if there is none.
    pub other_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_title: Option<String>,
    /// What was seen, in short English (read by the agent and the view). In a verdict it also
    /// carries the reason and "by <author>".
    pub detail: String,
    /// In a verdict (`superseded`, `contradicts`): why whoever said it said it, and who.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// In a superseded one, how many hops there are to `other_id` when it is the end of a chain (0 if
    /// it is the direct replacement).
    #[serde(skip_serializing_if = "is_zero")]
    pub via: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A verdict's detail for the agent and the CLI: which relation, the reason and who said it.
fn verdict_detail(base: &str, v: &crate::veredictos::Verdict, hops: usize) -> String {
    let mut d = base.to_string();
    if !v.reason.is_empty() {
        d.push_str(": ");
        d.push_str(&crate::veredictos::short_reason(&v.reason));
    }
    if hops > 1 {
        d.push_str(&format!(" (via {hops})"));
    }
    if !v.author.is_empty() {
        d.push_str(&format!(" · by {}", v.author));
    }
    d
}

/// Changes with any write: new, changed, moved or forgotten memories. It is
/// `count|last change|highest stamp|projects|spaces`; everything but the stamp comes from
/// `light_fingerprint`, so the one from `before` and the one from `refresh_for` can't drift apart.
fn fingerprint(db: &Connection) -> Result<String> {
    let (n, last, projects, spaces) = light_fingerprint(db)?;
    let stamp: String = db.query_row("SELECT COALESCE(max(hlc), '') FROM memory", [], |row| row.get(0))?;
    // With the version: if the detector changes (1.12.0: a journal's history no longer counts), what was
    // saved with the previous one is recomputed on upgrade, without waiting for some memory to change.
    Ok(format!("{n}|{last}|{stamp}|{projects}|{spaces}|{}", env!("CARGO_PKG_VERSION")))
}

/// What the last full recomputation saved: how many memories there were and the words that were not rare.
const COMMON_WORDS_KEY: &str = "revision_comunes";
/// How many writes have been reviewed incrementally since the last full recomputation.
const PARTIAL_COUNT_KEY: &str = "revision_parciales";
/// Threads used to check the memories in [`notices`], at most, and memories per thread, at least
/// (with fewer, starting the threads costs more than they save).
const MAX_THREADS: usize = 8;
const MIN_PER_THREAD: usize = 2_000;
/// From how many memories they are dropped on another thread at the end of [`notices`].
const DROP_ELSEWHERE_FROM: usize = 10_000;
/// Below this number of memories it is always fully recomputed: it's cheap and the rare-word cap
/// still moves with every memory (`rare_cap`).
pub const MIN_FOR_INCREMENTAL: usize = 2_000;
/// How much the number of memories may change since the last full recomputation before redoing it:
/// up to there, which word is rare is taken from back then.
const COUNT_DRIFT: f64 = 0.05;
/// More memories changed at once than this (a manual import, a huge `absorb`) is fully recomputed.
const MAX_INCREMENTAL: usize = 200;

/// Recomputes the outdated ones if the database changed since last time, without knowing what changed:
/// all of it. This is what runs after sync and import, and on read (`notices`, the recall) in case
/// something wrote without telling.
pub fn refresh(db: &Connection) -> Result<()> {
    let h = fingerprint(db)?;
    let before: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |row| row.get(0)).optional()?;
    if before.as_deref() == Some(h.as_str()) {
        return Ok(());
    }
    rebuild(db, &h)
}

/// Whether the review was up to date just before writing. Taken before the write and passed to
/// [`refresh_for`]: only that way do we know the only new thing is the memories being written.
pub struct Before {
    /// The saved fingerprint, if what can be seen of it without reading the whole table is the current one.
    fingerprint: Option<String>,
}

/// What the fingerprint gives without `max(hlc)`: number of memories, last change, projects and spaces,
/// which come from the `memory_scope` and `memory_space` indexes without reading the table (`max(hlc)`
/// reads all of it: 30 ms with 50,000 memories). The sums, as SQLite text, equal on both paths.
fn light_fingerprint(db: &Connection) -> Result<(String, String, String, String)> {
    let (n, last, projects) = db.query_row("SELECT count(*), COALESCE(max(updated_at), ''), total(project_id) || '' FROM memory", [], |row| {
        Ok((row.get::<_, i64>(0)?.to_string(), row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let spaces: String = db.query_row("SELECT total(space_id) || '' FROM memory WHERE space_id IS NOT NULL", [], |row| row.get(0))?;
    Ok((n, last, projects, spaces))
}

/// Compares the saved fingerprint with what comes from the index, without the stamp. What only changes
/// the stamp arrives via sync, which recomputes everything when done.
pub fn before(db: &Connection) -> Result<Before> {
    let (n, last, projects, spaces) = light_fingerprint(db)?;
    let saved: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |row| row.get(0)).optional()?;
    let fingerprint = saved.filter(|g| {
        let p: Vec<&str> = g.split('|').collect();
        p.len() == 6 && p[0] == n && p[1] == last && p[3] == projects && p[4] == spaces && p[5] == env!("CARGO_PKG_VERSION")
    });
    Ok(Before { fingerprint })
}

/// The fingerprint after writing `ids` on top of the one from `before`, without reading the table: the
/// highest stamp is the previous one or one of theirs (a change here always carries a newer stamp).
/// `None` if one of them is gone (forgotten or absorbed: maybe it had the highest stamp) and the whole
/// table must be read. If it came out different from the real one, the next read would recompute
/// everything: slower, but correct.
fn fingerprint_after(db: &Connection, before: &str, ids: &BTreeSet<String>) -> Result<Option<String>> {
    let mut stamp = before.split('|').nth(2).unwrap_or("").to_string();
    for id in ids {
        let Some(s) = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |row| row.get::<_, String>(0)).optional()? else {
            return Ok(None);
        };
        stamp = stamp.max(s);
    }
    let (n, last, projects, spaces) = light_fingerprint(db)?;
    Ok(Some(format!("{n}|{last}|{stamp}|{projects}|{spaces}|{}", env!("CARGO_PKG_VERSION"))))
}

/// Brings the review up to date after writing `ids` (saved, changed, forgotten or restored), without
/// redoing it all: only the pairs one of them takes part in, against its candidates from the search
/// index, and the links that may have changed. Which word is rare is taken from the last full
/// recomputation; it is fully redone if the review was not up to date before writing, if there are
/// few memories, if their number has changed by more than 5 % since then or if the index can't
/// search for some word.
pub fn refresh_for(db: &Connection, before: &Before, ids: &[String]) -> Result<()> {
    let Some(previous) = before.fingerprint.as_deref() else {
        return refresh(db);
    };
    if ids.is_empty() || ids.len() > MAX_INCREMENTAL {
        return refresh(db);
    }
    let ids: BTreeSet<String> = ids.iter().cloned().collect();
    let h = match fingerprint_after(db, previous, &ids)? {
        Some(h) => h,
        None => fingerprint(db)?,
    };
    let n: usize = h.split('|').next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let Some(common) = current_common_words(db, n)? else {
        return refresh(db);
    };
    // IMMEDIATE: this transaction reads and then writes. Deferred, if another agent wrote between the
    // read and the write, SQLite returned BUSY_SNAPSHOT at once, without waiting (its snapshot is no
    // longer the latest), and the review was left out of date.
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    let done = outdated_pairs_of(&tx, &ids, &common)? && crate::enlaces::recompute_for(&tx, &ids)?;
    if !done {
        drop(tx);
        return refresh(db);
    }
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('revision', ?1)", [&h])?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{PARTIAL_COUNT_KEY}', COALESCE((SELECT v FROM meta WHERE k = '{PARTIAL_COUNT_KEY}'), 0) + 1)"),
        [],
    )?;
    tx.commit()?;
    // If it touched a trade memory, its duplicates are recomputed separately (if that fails, the daily job will).
    let _ = trade_duplicates_after_write(db);
    Ok(())
}

/// The common words from the last full recomputation, if they still hold for `n` memories.
fn current_common_words(db: &Connection, n: usize) -> Result<Option<HashSet<String>>> {
    let Some(v) = db.query_row(&format!("SELECT v FROM meta WHERE k = '{COMMON_WORDS_KEY}'"), [], |row| row.get::<_, String>(0)).optional()? else {
        return Ok(None);
    };
    let mut lines = v.split('\n');
    let Some(n_ref) = lines.next().and_then(|x| x.parse::<usize>().ok()) else {
        return Ok(None);
    };
    if n < MIN_FOR_INCREMENTAL || n_ref < MIN_FOR_INCREMENTAL || (n as f64 - n_ref as f64).abs() > n_ref as f64 * COUNT_DRIFT {
        return Ok(None);
    }
    Ok(Some(lines.filter(|l| !l.is_empty()).map(str::to_string).collect()))
}

/// The full recomputation: outdated pairs and links of the whole database.
fn rebuild(db: &Connection, h: &str) -> Result<()> {
    let (found, n, common) = outdated_pairs(db)?;
    // IMMEDIATE, like every transaction that writes (see `db`).
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    // What was marked fine is in `verdict` (and travels with sync); here only what was detected.
    tx.execute("DELETE FROM review WHERE kind IN (?1, ?2)", [OUTDATED, BROKEN_LINK])?;
    let broken = crate::enlaces::recompute(&tx)?;
    record_broken_links(&tx, broken)?;
    for (old, new, detail) in found {
        record_outdated(&tx, &old, &new, &detail)?;
    }
    let mut saved = n.to_string();
    for w in common {
        saved.push('\n');
        saved.push_str(&w);
    }
    tx.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{COMMON_WORDS_KEY}', ?1)"), [&saved])?;
    tx.execute(&format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('{PARTIAL_COUNT_KEY}', '0')"), [])?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('revision', ?1)", [h])?;
    tx.commit()?;
    // The database changed (a sync, an import, something that wrote without telling): if some trade
    // memory changed, its duplicates are recomputed separately.
    let _ = trade_duplicates_after_write(db);
    Ok(())
}

/// The broken links (memory, name as written), one per memory with all its names.
pub(crate) fn record_broken_links(db: &Connection, broken: Vec<(String, String)>) -> Result<()> {
    let mut by_memory: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (m, e) in broken {
        by_memory.entry(m).or_default().push(format!("[[{e}]]"));
    }
    for (m, es) in by_memory {
        db.execute(
            &format!("INSERT OR IGNORE INTO review(memory_id, kind, other_id, detail, found_at) VALUES (?1, ?2, '', ?3, {NOW})"),
            params![m, BROKEN_LINK, format!("links to nothing: {}", es.join(", "))],
        )?;
    }
    Ok(())
}

fn record_outdated(db: &Connection, old: &str, new: &str, detail: &str) -> Result<()> {
    db.execute(
        &format!("INSERT OR IGNORE INTO review(memory_id, kind, other_id, detail, found_at) VALUES (?1, ?2, ?3, ?4, {NOW})"),
        params![old, OUTDATED, new, detail],
    )?;
    Ok(())
}

// --- Outdated --------------------------------------------------------------------------------------

struct Card {
    id: String,
    project: String,
    updated_at: String,
    /// The stamp of the last change (`reloj`): breaks ties between two memories of the same millisecond.
    hlc: String,
    /// The most recent date that counts (or that of the last change if it mentions none).
    when: String,
    title: BTreeSet<String>,
    content: BTreeSet<String>,
    text: String,
    versions: BTreeMap<String, BTreeSet<Vec<u64>>>,
    /// The versions it states in the present: without the history lines (with a date, "vA → vB" or
    /// "desde la X"). "19-oct-2026 (v0.6.1 → 0.6.2): …" tells what happened and is still true; it
    /// doesn't say it is on 0.6.2 today. This is what is compared from the old one against the new one.
    current: BTreeMap<String, BTreeSet<Vec<u64>>>,
}

/// What doesn't tell one memory from another.
const STOPWORDS: &[&str] = &[
    "para", "pero", "como", "esta", "este", "esto", "estos", "estas", "desde", "hasta", "sobre", "entre", "cuando", "donde", "porque",
    "tiene", "tienen", "hace", "hacer", "solo", "todo", "toda", "todos", "todas", "cada", "otro", "otra", "otros", "otras", "mismo",
    "misma", "algo", "nada", "sino", "tras", "with", "that", "this", "from", "have", "when", "what", "into", "your", "they", "them",
    "then", "than", "also", "only", "more", "most", "just", "like", "does", "each", "been", "were", "will", "would", "should", "could",
    "there", "their", "about", "which", "other", "some", "such", "over", "under", "after", "before", "memoria", "memorias", "usuario",
];

fn normalize(s: &str) -> String {
    crate::buscar::strip_accents(&s.to_lowercase())
}

/// Words of 4 letters or more (with `.`, `_` and `-` inside: `centrelaser.es`, `social-publish`).
fn words(s: &str) -> BTreeSet<String> {
    let mut r = BTreeSet::new();
    for p in normalize(s).split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_' || c == '-')) {
        let p = p.trim_matches(|c: char| c == '.' || c == '-' || c == '_');
        if p.chars().count() >= 4 && !p.chars().all(|c| c.is_ascii_digit() || c == '.') && !STOPWORDS.contains(&p) {
            r.insert(p.to_string());
        }
    }
    r
}

const SPANISH_MONTHS: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Dates `29-oct-2026`, `29 oct 2026` and `2026-10-29` in the text, as `YYYY-MM-DD`.
fn dates(t: &str) -> Vec<String> {
    let mut r = Vec::new();
    let w: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    for i in 0..w.len() {
        // YYYY MM DD (the three in a row after splitting on the dashes).
        if i + 2 < w.len()
            && w[i].len() == 4
            && w[i + 1].len() == 2
            && w[i + 2].len() == 2
            && let (Ok(y), Ok(m), Ok(d)) = (w[i].parse::<u32>(), w[i + 1].parse::<u32>(), w[i + 2].parse::<u32>())
            && (1990..2200).contains(&y)
            && (1..=12).contains(&m)
            && (1..=31).contains(&d)
        {
            r.push(format!("{y:04}-{m:02}-{d:02}"));
        }
        // DD month YYYY.
        if i + 2 < w.len() && w[i].len() <= 2 && w[i + 2].len() == 4 {
            let month = w[i + 1].get(..3).unwrap_or("");
            let m = SPANISH_MONTHS.iter().position(|x| *x == month).or_else(|| MONTHS.iter().position(|x| *x == month));
            if let (Ok(d), Some(m), Ok(y)) = (w[i].parse::<u32>(), m, w[i + 2].parse::<u32>())
                && (1..=31).contains(&d)
                && (1990..2200).contains(&y)
            {
                r.push(format!("{y:04}-{:02}-{d:02}", m + 1));
            }
        }
    }
    r
}

/// The words that don't say what a version belongs to: "la 0.9.2", "release v0.10.0".
const GENERIC_WORDS: &[&str] = &["la", "el", "lo", "version", "versiones", "release", "tag", "a", "de", "en", "con", "hasta", "desde", "y", "o", "the", "to", "of", "v"];

/// `{thing: {versions}}`: the `X.Y.Z` in the text, by the word in front (empty if it is generic: the
/// project's own). That way "rusqlite 0.41.0" isn't compared with "omoi 0.9.2".
fn versions(t: &str) -> BTreeMap<String, BTreeSet<Vec<u64>>> {
    let mut r: BTreeMap<String, BTreeSet<Vec<u64>>> = BTreeMap::new();
    // The word in front, within the same sentence: a line break or the end of a sentence clears it.
    let mut previous = String::new();
    for p in t.split(|c: char| c == ' ' || c == '\t' || "()[]{},;*`\"'«»|".contains(c)).filter(|p| !p.is_empty()) {
        let p = match p.rsplit_once(['\n', '\r']) {
            Some((_, rest)) => {
                previous.clear();
                rest
            }
            None => p,
        };
        if p.is_empty() {
            continue;
        }
        let clean = p.trim_end_matches(['.', ')']).trim_start_matches('v');
        let parts: Vec<&str> = clean.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|x| !x.is_empty() && x.len() <= 4 && x.chars().all(|c| c.is_ascii_digit())) {
            let key = if GENERIC_WORDS.contains(&previous.as_str()) { String::new() } else { previous.clone() };
            r.entry(key).or_default().insert(parts.iter().map(|x| x.parse().unwrap_or(0)).collect());
        }
        previous = if p.ends_with(['.', ':', '!', '?']) && !p.chars().any(|c| c.is_ascii_digit()) {
            String::new()
        } else {
            p.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').to_lowercase()
        };
    }
    r
}

fn card(m: &Memory, hlc: String) -> Card {
    let content: String = m.content.chars().take(RECALL_LEN).collect();
    let text = normalize(&content);
    let all = format!("{} {text}", normalize(&m.title));
    Card {
        id: m.id.clone(),
        // Space-level ones are compared among themselves, not with the global ones.
        project: if m.space_only() { format!("space:{}", m.space) } else { m.project.clone() },
        updated_at: m.updated_at.clone(),
        hlc,
        when: dates(&all).into_iter().max().unwrap_or_else(|| m.updated_at.get(..10).unwrap_or("").to_string()),
        title: words(&m.title),
        content: words(&content),
        versions: versions(&all),
        current: versions(&format!("{}\n{}", normalize(&m.title), without_history(&text))),
        text,
    }
}

/// The text without its history lines: those starting with a date (a journal entry,
/// "**19-oct-2026 (v0.6.1 → 0.6.2):** …"), those telling of a version change ("0.6.1 → 0.6.2") and the
/// "desde la X" ones. By lines and not by sentences: in a dated entry, the sentences after the date are
/// also from that day. A date in the middle ("la v0.2.0, publicada el 28-oct-2026") doesn't make it
/// history: it says where the project is.
fn without_history(t: &str) -> String {
    let starts_with_date = |l: &str| {
        let start: String = l.trim_start_matches(|c: char| !c.is_alphanumeric()).chars().take(14).collect();
        !dates(&start).is_empty()
    };
    // "desde la 0.4.0", "desde v0.4.0", "since 0.4.0": the word and, after it, a version or its article.
    let since = |l: &str| {
        let ws: Vec<&str> = l.split_whitespace().map(|w| w.trim_start_matches(|c: char| !c.is_alphanumeric())).collect();
        ws.windows(2).any(|p| {
            matches!(p[0], "desde" | "since")
                && (matches!(p[1], "la" | "el" | "version") || p[1].trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit()))
        })
    };
    t.lines()
        .filter(|l| !starts_with_date(l) && !l.contains('→') && !l.contains("->") && !since(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sentences (by end punctuation or line break).
fn sentences(t: &str) -> Vec<&str> {
    let mut r = Vec::new();
    let mut start = 0;
    let b = t.as_bytes();
    for (i, c) in t.char_indices() {
        let cut = c == '\n' || (".;:!?".contains(c) && b.get(i + 1).is_none_or(|x| x.is_ascii_whitespace()));
        if cut {
            if t[start..i].trim().len() > 1 {
                r.push(&t[start..i]);
            }
            start = i + c.len_utf8();
        }
    }
    if t[start..].trim().len() > 1 {
        r.push(&t[start..]);
    }
    r
}

fn contains_word(sentence: &str, ws: &[&str]) -> bool {
    ws.iter().any(|w| {
        sentence.match_indices(w).any(|(i, _)| {
            let before = sentence[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let after = sentence[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
            before && after
        })
    })
}

const PENDING: &[&str] = &["pendiente", "falta", "faltan", "por hacer", "sin hacer", "sin publicar", "todavia no", "aun no", "siguiente", "not yet", "pending"];
const DONE: &[&str] = &["hecho", "hecha", "publicado", "publicada", "publicados", "publicadas", "ya esta", "resuelto", "terminado", "terminada", "instalado", "instalada", "done", "published", "shipped"];

fn version_text(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// Why the old one looks superseded by the new one, or nothing. `same_topic`: they share words; without
/// that, a bare version says nothing (in a project almost everything names its own versions: the slogan
/// "desde la 0.14.1" doesn't contradict the signature "desde la 0.14.0"). A named version
/// ("rusqlite 0.41.0") does, because the name already ties them.
fn reasons(old: &Card, new: &Card, same_topic: bool, rare: &dyn Fn(&BTreeSet<String>) -> BTreeSet<String>) -> Vec<String> {
    let mut r = Vec::new();
    for (thing, va) in &old.current {
        if let Some(vb) = new.versions.get(thing)
            && let (Some(a), Some(b)) = (va.last(), vb.last())
            && b > a
            && (same_topic || !thing.is_empty())
        {
            let name = if thing.is_empty() { "version".to_string() } else { thing.clone() };
            r.push(format!("{name} {} → {}", version_text(a), version_text(b)));
        }
    }
    // A sentence of the old one says something is pending and another of the new one that it is done,
    // and they talk about the same thing (at least two rare words in common).
    let pending: Vec<BTreeSet<String>> = sentences(&old.text).into_iter().filter(|f| contains_word(f, PENDING)).map(|f| rare(&words(f))).collect();
    if !pending.is_empty() {
        let done: Vec<BTreeSet<String>> = sentences(&new.text).into_iter().filter(|f| contains_word(f, DONE)).map(|f| rare(&words(f))).collect();
        let best = pending.iter().flat_map(|p| done.iter().map(move |h| p.intersection(h).count())).max().unwrap_or(0);
        if best >= 2 {
            r.push("pending → done".to_string());
        }
    }
    r
}

/// A memory's rare words, from the title and from the content.
struct RareWords {
    title: BTreeSet<String>,
    content: BTreeSet<String>,
}

fn rare_words_of(f: &Card, is_rare: &dyn Fn(&str) -> bool) -> RareWords {
    let filter = |ws: &BTreeSet<String>| -> BTreeSet<String> { ws.iter().filter(|w| is_rare(w)).cloned().collect() };
    RareWords { title: filter(&f.title), content: filter(&f.content) }
}

/// Rare: in 5 % of the memories at most; from 2,000 memories on, in 100. A word that appears in
/// hundreds of memories doesn't say two of them talk about the same thing.
fn rare_cap(n: usize) -> usize {
    2usize.max(n / 20).min(100)
}

/// What two memories must share to be candidates: a rare word or the version of the same thing.
fn share_something(a: &Card, ra: &RareWords, b: &Card, rb: &RareWords) -> bool {
    let of_a: BTreeSet<&String> = ra.title.union(&ra.content).collect();
    rb.title.iter().chain(&rb.content).any(|w| of_a.contains(w)) || a.versions.keys().any(|c| !c.is_empty() && b.versions.contains_key(c))
}

/// (old, new, detail).
type Outdated = (String, String, String);

/// Whether one of the two makes the other obsolete.
fn judge(a: &Card, ra: &RareWords, b: &Card, rb: &RareWords, is_rare: &dyn Fn(&str) -> bool) -> Option<Outdated> {
    // The newer one: by the date it mentions, then by its last change and, in the same
    // millisecond (several saved in a row, an import), by the clock stamp.
    fn order_key(f: &Card) -> (&str, &str, &str, &str) {
        (&f.when, &f.updated_at, &f.hlc, &f.id)
    }
    let ((old, r_old), (new, r_new)) = if order_key(a) < order_key(b) { ((a, ra), (b, rb)) } else { ((b, rb), (a, ra)) };
    let common_title = r_old.title.intersection(&r_new.title).count();
    let union = r_old.content.union(&r_new.content).count().max(1);
    let jaccard = r_old.content.intersection(&r_new.content).count() as f64 / union as f64;
    let same_topic = common_title >= 2 || (common_title >= 1 && jaccard >= 0.12) || jaccard >= 0.22;
    // What shares no words can only contradict itself through a named version.
    if !same_topic && old.versions.keys().all(|c| c.is_empty()) {
        return None;
    }
    let rare = |ws: &BTreeSet<String>| -> BTreeSet<String> { ws.iter().filter(|w| is_rare(w)).cloned().collect() };
    let m = reasons(old, new, same_topic, &rare);
    (!m.is_empty()).then(|| (old.id.clone(), new.id.clone(), m.join("; ")))
}

/// Pairs (old, new, detail) of the same project that talk about the same thing and contradict each
/// other, across the whole database. Also returns how many memories there were and the words that were
/// not rare, which is what the incremental review (`refresh_for`) needs until the next full recomputation.
fn outdated_pairs(db: &Connection) -> Result<(Vec<Outdated>, usize, Vec<String>)> {
    let all = crate::memoria::recent(db, &Scope::All, None, usize::MAX >> 1)?;
    let stamps: HashMap<String, String> = {
        let mut q = db.prepare("SELECT id, hlc FROM memory")?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let cards: Vec<Card> = all.iter().map(|m| card(m, stamps.get(&m.id).cloned().unwrap_or_default())).collect();
    let n = cards.len().max(1);
    let mut df: HashMap<&str, usize> = HashMap::new();
    for f in &cards {
        for w in f.title.union(&f.content) {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let cap = rare_cap(n);
    let is_rare = |w: &str| df.get(w).is_some_and(|d| *d <= cap);
    let rare: Vec<RareWords> = cards.iter().map(|f| rare_words_of(f, &is_rare)).collect();

    // Candidates: pairs of the same project (the global ones count as one) that share a rare word or
    // name the version of the same thing ("rusqlite 0.41.0"). It is exact: every reason in `reasons`
    // requires one of the two. Comparing every pair of a project was quadratic: with 50,000 memories,
    // 31 million pairs, 108 s and 1.2 GB on every write.
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut index: HashMap<(&str, String), Vec<usize>> = HashMap::new();
    for (i, f) in cards.iter().enumerate() {
        for w in rare[i].title.union(&rare[i].content) {
            index.entry((f.project.as_str(), w.clone())).or_default().push(i);
        }
        for thing in f.versions.keys().filter(|c| !c.is_empty()) {
            index.entry((f.project.as_str(), format!("ver:{thing}"))).or_default().push(i);
        }
    }
    for l in index.values() {
        for (a, &i) in l.iter().enumerate() {
            for &j in &l[a + 1..] {
                pairs.insert((i.min(j), i.max(j)));
            }
        }
    }

    let r = pairs.into_iter().filter_map(|(i, j)| judge(&cards[i], &rare[i], &cards[j], &rare[j], &is_rare)).collect();
    let mut common: Vec<String> = df.iter().filter(|(_, d)| **d > cap).map(|(w, _)| w.to_string()).collect();
    common.sort();
    Ok((r, cards.len(), common))
}

/// The phrase with which the text index (`memory_fts`, unicode61 without accents) finds a word from the
/// cards: its alphanumeric pieces in a row ("centrelaser.es" → `"centrelaser es"`). With `prefix`, the
/// last piece counts as the start of a word: the card cuts the content at `RECALL_LEN` and may
/// keep half a word. `None` if there is nothing to search for.
pub(crate) fn fts_phrase(word: &str, prefix: bool) -> Option<String> {
    let pieces: Vec<&str> = word.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let last = pieces.last()?;
    let star = if prefix && last.chars().count() >= 3 { " *" } else { "" };
    Some(format!("\"{}\"{star}", pieces.join(" ")))
}

/// The memories (with their stamp) an FTS query returns among those compared with `m`: those of its
/// project, those of its space if it is space-level, or the global ones (the same grouping as `card`).
fn cards_by_fts(db: &Connection, query: &str, m: &Memory) -> Result<Vec<Card>> {
    let (filter, value): (&str, rusqlite::types::Value) = if m.project_id != 0 {
        ("m.project_id = ?2", m.project_id.into())
    } else if m.space_only() {
        ("m.project_id = 0 AND m.space_id = (SELECT id FROM space WHERE key = ?2)", m.space.clone().into())
    } else {
        ("m.project_id = 0 AND m.space_id IS NULL AND ?2 IS NULL", rusqlite::types::Value::Null)
    };
    let mut q = db.prepare_cached(&format!(
        "SELECT {}, m.hlc FROM memory m WHERE {filter} AND m.n IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1)",
        crate::memoria::COLUMNS
    ))?;
    let v = q
        .query_map(params![query, value], |row| Ok((crate::memoria::from_row(row)?, row.get::<_, String>(crate::memoria::NCOL)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v.into_iter().map(|(m, hlc)| card(&m, hlc)).collect())
}

/// The outdated pairs one of `ids` takes part in, with the notion of rare from the last full
/// recomputation: deletes the existing ones and records the current ones. Their candidates come from the
/// text index (any memory that shares a rare word or a versioned thing names it) and are checked the same
/// way as in the full recomputation. `false` if a word has nothing to search for in the index.
fn outdated_pairs_of(db: &Connection, ids: &BTreeSet<String>, common: &HashSet<String>) -> Result<bool> {
    let is_rare = |w: &str| !common.contains(w);
    let mut changed = Vec::new();
    for id in ids {
        db.execute("DELETE FROM review WHERE kind = ?1 AND (memory_id = ?2 OR other_id = ?2)", params![OUTDATED, id])?;
        let loaded = db
            .query_row(&format!("SELECT {}, m.hlc FROM memory m WHERE m.id = ?1", crate::memoria::COLUMNS), [id], |row| {
                Ok((crate::memoria::from_row(row)?, row.get::<_, String>(crate::memoria::NCOL)?))
            })
            .optional()?;
        if let Some((m, hlc)) = loaded {
            changed.push((card(&m, hlc), m));
        }
    }
    let mut found: BTreeMap<(String, String), String> = BTreeMap::new();
    for (c, m) in &changed {
        let rc = rare_words_of(c, &is_rare);
        let mut phrases = Vec::new();
        for w in rc.title.union(&rc.content) {
            let Some(f) = fts_phrase(w, true) else { return Ok(false) };
            phrases.push(f);
        }
        for thing in c.versions.keys().filter(|c| !c.is_empty()) {
            let Some(f) = fts_phrase(thing, false) else { return Ok(false) };
            phrases.push(f);
        }
        if phrases.is_empty() {
            continue;
        }
        let query = format!("{{title content}} : ({})", phrases.join(" OR "));
        for b in cards_by_fts(db, &query, m)? {
            if b.id == c.id {
                continue;
            }
            let rb = rare_words_of(&b, &is_rare);
            if !share_something(c, &rc, &b, &rb) {
                continue;
            }
            if let Some((old, new, detail)) = judge(c, &rc, &b, &rb, &is_rare) {
                found.insert((old, new), detail);
            }
        }
    }
    for ((old, new), detail) in found {
        record_outdated(db, &old, &new, &detail)?;
    }
    Ok(true)
}

// --- Reading ---------------------------------------------------------------------------------------

/// Whether "it's fine as it is" was said and the memory hasn't changed since.
/// The warnings a current verdict removes: (memory, warning, other). A pair with a verdict no longer
/// shows as outdated: either it holds (`compatible`) or it has its own warning (`superseded`, `contradicts`).
fn dismissed(db: &Connection) -> Result<BTreeSet<(String, String, String)>> {
    let mut r = BTreeSet::new();
    for ((id, kind), _) in crate::veredictos::oks(db, true)? {
        r.insert((id, kind, String::new()));
    }
    for (a, b) in crate::veredictos::pairs(db)?.into_keys() {
        r.insert((a.clone(), OUTDATED.to_string(), b.clone()));
        r.insert((b, OUTDATED.to_string(), a));
    }
    Ok(r)
}

/// Where a memory's path is on this machine, if it can be known (relative to its project).
fn path_on_disk(m: &Memory, path: &str) -> Option<std::path::PathBuf> {
    // A network path is never checked (it would open an SMB connection: see `proyecto::ruta_de_red`).
    if crate::proyecto::ruta_de_red(path) || from_other_os(path) {
        return None;
    }
    let p = std::path::Path::new(path.trim_start_matches("~/"));
    let d = if path.starts_with("~/") {
        home_dir().map(|h| h.join(p))?
    } else if p.is_absolute() {
        p.to_path_buf()
    } else {
        let root = std::path::Path::new(&m.project);
        if m.project.is_empty() || !crate::proyecto::ruta_local(root) || !root.is_dir() {
            return None;
        }
        root.join(p)
    };
    crate::proyecto::ruta_local(&d).then_some(d)
}

/// A Windows path (`D:/…`, `C:\\…`) seen from macOS or Linux, or a Unix one (`/Users/…`) seen from
/// Windows: it belongs to the person's other machine (it arrives via sync), not a broken path.
fn from_other_os(path: &str) -> bool {
    let b = path.as_bytes();
    let windows_path = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\');
    if cfg!(windows) { path.starts_with('/') } else { windows_path }
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from)
}

/// Everything worth reviewing in the scope, from most to least important.
pub fn notices(db: &Connection, scope: &Scope) -> Result<Vec<Notice>> {
    notices_with(db, scope, true)
}

/// Like [`notices`], for the `context` tool (at session start, with the agent waiting): code drift is
/// not computed here the first time (with git, it can take seconds), it is launched separately and
/// shows up next time (`deriva::for_memories_no_wait`).
pub fn notices_without_waiting(db: &Connection, scope: &Scope) -> Result<Vec<Notice>> {
    notices_with(db, scope, false)
}

fn notices_with(db: &Connection, scope: &Scope, wait: bool) -> Result<Vec<Notice>> {
    refresh(db)?;
    let excluded = dismissed(db)?;
    let all = crate::memoria::recent(db, scope, None, usize::MAX >> 1)?;
    let by_id: HashMap<&str, &Memory> = all.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut r = Vec::new();
    let make_notice = |m: &Memory, kind: &str, other: Option<&Memory>, detail: String| Notice {
        kind: kind.into(),
        memory_id: m.id.clone(),
        title: m.title.clone(),
        project: m.project.clone(),
        other_id: other.map(|o| o.id.clone()).unwrap_or_default(),
        other_title: other.map(|o| o.title.clone()),
        detail,
        reason: String::new(),
        author: String::new(),
        via: 0,
    };
    {
        let mut q = db.prepare("SELECT memory_id, other_id, detail FROM review WHERE kind = ?1 ORDER BY found_at, memory_id")?;
        let rows: Vec<(String, String, String)> = q.query_map([OUTDATED], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (old, new, detail) in rows {
            if let (Some(o), Some(n)) = (by_id.get(old.as_str()), by_id.get(new.as_str()))
                && !excluded.contains(&(old.clone(), OUTDATED.to_string(), new.clone()))
            {
                r.push(make_notice(o, OUTDATED, Some(n), detail));
            }
        }
    }
    for mut v in crate::veredictos::pairs(db)?.into_values() {
        if v.relation == crate::veredictos::COMPATIBLE {
            continue;
        }
        // A superseded one points to the end of its chain: if B supersedes A and C supersedes B, A leads
        // to C, with the reason of the last link.
        let mut hops = 0;
        if v.relation == SUPERSEDED
            && let Some(end) = crate::veredictos::chain(db, &v.memory_id)?
            && end.hops > 1
        {
            hops = end.hops;
            v.other_id = end.id;
            v.reason = end.reason;
            v.author = end.author;
        }
        if let (Some(m), Some(o)) = (by_id.get(v.memory_id.as_str()), by_id.get(v.other_id.as_str())) {
            let base = if v.relation == SUPERSEDED { "replaced by the other one" } else { "they disagree and neither has won yet" };
            let mut a = make_notice(m, &v.relation, Some(o), verdict_detail(base, &v, hops));
            a.reason = v.reason;
            a.author = v.author;
            a.via = hops;
            r.push(a);
        }
    }
    // Comparing trade memories with each other takes time: here we only read what the background
    // process computed (see `saved_trade_duplicates`).
    for (new, old, detail) in saved_trade_duplicates(db, scope)? {
        if let (Some(n), Some(o)) = (by_id.get(new.as_str()), by_id.get(old.as_str())) {
            // Like an outdated one: the old one, and the other is the new one (the web view merges the old into the new).
            r.push(make_notice(o, TRADE_DUPLICATE, Some(n), detail));
        }
    }
    {
        let mut q = db.prepare("SELECT memory_id, detail FROM review WHERE kind = ?1 ORDER BY memory_id")?;
        let rows: Vec<(String, String)> = q.query_map([BROKEN_LINK], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, detail) in rows {
            if let Some(m) = by_id.get(id.as_str())
                && !excluded.contains(&(id.clone(), BROKEN_LINK.to_string(), String::new()))
            {
                r.push(make_notice(m, BROKEN_LINK, None, detail));
            }
        }
    }
    let now = db_now(db)?;
    let days = |since: &str| -> f64 { days_between(since, &now) };
    let reviewed = crate::veredictos::oks(db, false)?;
    let today = Today::load(db)?;
    // Optional, like the warning: if git or the disk fail, only this one is lost.
    let drift = if wait { crate::deriva::for_memories(db, &all) } else { crate::deriva::for_memories_no_wait(db, &all) }.unwrap_or_default();
    // What concerns each memory doesn't depend on the others (except reading them, in `stale_status`):
    // it is split into contiguous chunks across the cores and joined in the same order, so the result is
    // the same as one by one. With 50,000 memories, the secrets alone took half a second.
    let check_one = |m: &Memory, r: &mut Vec<Notice>| {
        if let Some(d) = drift.get(&m.id) {
            r.push(make_notice(m, CODE_DRIFT, None, d.clone()));
        }
        if let Some(f) = date_reached(m, &reviewed, &today) {
            r.push(make_notice(m, REVIEW_DATE, None, format!("review date {f} reached: is it still true?")));
        }
        if let Some(n) = days_unreviewed(m, &reviewed, &now) {
            r.push(make_notice(m, DUE, None, format!("untouched for {n} days: is it still true?")));
        }
        let is_dismissed = |k: &str| excluded.contains(&(m.id.clone(), k.to_string(), String::new()));
        if status_warns(m, days(&m.updated_at)) && !is_dismissed(STALE_STATUS) {
            let later = all.iter().filter(|x| x.project_id == m.project_id && x.space == m.space && x.id != m.id && x.updated_at > m.updated_at).count();
            if later > 0 {
                r.push(make_notice(m, STALE_STATUS, None, format!("{later} memories of this project changed after it; untouched for {} days", days(&m.updated_at) as i64)));
            }
        }
        if !is_dismissed(BROKEN_PATH) {
            let broken: Vec<&str> = m.paths.iter().filter(|p| path_on_disk(m, p).is_some_and(|d| !d.exists())).map(String::as_str).collect();
            if !broken.is_empty() {
                r.push(make_notice(m, BROKEN_PATH, None, format!("no longer exists: {}", broken.join(", "))));
            }
        }
        if m.recall_count == 0 && m.recalled_at.is_none() && days(&m.created_at) > UNUSED_DAYS && !is_dismissed(UNUSED) {
            r.push(make_notice(m, UNUSED, None, format!("never recalled in {} days", days(&m.created_at) as i64)));
        }
        // Checked on read, like `unused`: a few µs per memory, and that way it also covers what arrives via
        // sync or a backup. It never shows the secret, only what it looks like.
        if !is_dismissed(SECRET) {
            let hs = secrets_of(m);
            if !hs.is_empty() {
                r.push(make_notice(m, SECRET, None, format!("looks like a secret: {}; redact it", crate::secretos::resumen(&hs))));
            }
        }
        let len = m.content.chars().count();
        if len > RECALL_LEN && !is_dismissed(TOO_LONG) {
            r.push(make_notice(m, TOO_LONG, None, format!("{len} characters; recall only reads the first {RECALL_LEN}: consider splitting it")));
        }
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(MAX_THREADS);
    let chunk = all.len().div_ceil(threads).max(MIN_PER_THREAD);
    if all.len() <= chunk {
        for m in &all {
            check_one(m, &mut r);
        }
    } else {
        let parts: Vec<Vec<Notice>> = std::thread::scope(|s| {
            let handles: Vec<_> = all
                .chunks(chunk)
                .map(|ms| {
                    let check_one = &check_one;
                    s.spawn(move || {
                        let mut v = Vec::new();
                        for m in ms {
                            check_one(m, &mut v);
                        }
                        v
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
        });
        r.extend(parts.into_iter().flatten());
    }
    let order = |k: &str| KINDS.iter().position(|x| *x == k).unwrap_or(KINDS.len());
    // Within each kind of warning, first what is most wrong if nobody looks at it: decisions and
    // gotchas written by an agent and not verified by any person; verified ones last.
    let priority = |id: &str| match by_id.get(id) {
        Some(m) if m.verified.is_some() => 2,
        Some(m) if matches!(m.kind, crate::memoria::Kind::Decision | crate::memoria::Kind::Gotcha) => 0,
        _ => 1,
    };
    // With each warning's key computed once (with 50,000 memories, computing it on every comparison
    // took more than 100 ms); stable, like `sort_by_key`.
    r.sort_by_cached_key(|a| (order(&a.kind), priority(&a.memory_id)));
    // Dropping many memories is freeing memory nobody waits for (with 50,000, 40 ms): on another thread.
    drop(by_id);
    if all.len() > DROP_ELSEWHERE_FROM {
        std::thread::spawn(move || drop(all));
    }
    Ok(r)
}

/// The last computed trade duplicates: `trade_fingerprint` \t JSON of [`ComputedDuplicates`]. Local.
const TRADE_DUPLICATES_KEY: &str = "revision_duplicadas_oficio";
/// The fingerprint for which the background computation was already launched, and when (`fingerprint \t seconds`).
const TRADE_DUPLICATES_LAUNCHED_KEY: &str = "revision_duplicadas_oficio_lanzado";
/// Seconds between two launches of the computation, even if the fingerprint changes (a burst of writes).
const BETWEEN_LAUNCHES: u64 = 10;
/// After launching the computation for a fingerprint, how long to wait for it to save before declaring
/// it dead and launching it again (seconds).
const SAME_FINGERPRINT: u64 = 600;

/// A pair of trade duplicates: (the newer, the older, detail).
type Pair = (String, String, String);

/// What the trade duplicates computation saves.
#[derive(Default, Serialize, serde::Deserialize)]
struct ComputedDuplicates {
    /// The vector model it was computed with: with another one, everything from scratch.
    #[serde(rename = "modelo")]
    model: String,
    /// How each trade memory was (`updated_at|has vector`): next time only the ones that changed are
    /// compared.
    #[serde(rename = "memorias")]
    memories: BTreeMap<String, String>,
    #[serde(rename = "parejas")]
    pairs: Vec<ComputedPair>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
struct ComputedPair {
    #[serde(rename = "nueva")]
    newer: String,
    #[serde(rename = "vieja")]
    older: String,
    #[serde(rename = "detalle")]
    detail: String,
    /// Each one's `updated_at` when it was computed: if it is no longer the current one, the pair isn't shown.
    #[serde(rename = "cambio_nueva")]
    newer_changed_at: String,
    #[serde(rename = "cambio_vieja")]
    older_changed_at: String,
}

/// The trade memories and how each one is (`updated_at|has vector`), ordered by id.
const TRADE_SQL: &str = "SELECT m.id, m.updated_at || '|' || EXISTS(SELECT 1 FROM memory_vec v WHERE v.n = m.n) FROM memory m
     WHERE m.project_id = 0 AND m.space_id IS NULL AND instr(m.tags, '\"trade:') > 0 ORDER BY m.id";

fn vector_model(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'modelo'", [], |row| row.get(0)).optional()?.unwrap_or_default())
}

/// What changes the trade duplicates result: each trade memory (id, last change and whether it has a
/// vector) and the vector model. What changes in the others only moves the IDF a little, and that waits
/// until a trade one changes. A query over small columns of the global ones.
fn trade_fingerprint(db: &Connection) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut q = db.prepare(TRADE_SQL)?;
    let mut rows = q.query([])?;
    while let Some(row) = rows.next()? {
        h.update(format!("{}|{}\n", row.get::<_, String>(0)?, row.get::<_, String>(1)?));
    }
    h.update(vector_model(db)?);
    Ok(h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect())
}

/// What is saved: (fingerprint, what was computed). Empty if there is nothing or it can't be parsed.
fn read_computed(db: &Connection) -> Result<(String, ComputedDuplicates)> {
    let saved: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [TRADE_DUPLICATES_KEY], |row| row.get(0)).optional()?;
    Ok(match saved.as_deref().and_then(|v| v.split_once('\t')) {
        Some((h, json)) => match serde_json::from_str::<ComputedDuplicates>(json) {
            Ok(c) => (h.to_string(), c),
            Err(_) => (String::new(), ComputedDuplicates::default()),
        },
        None => (String::new(), ComputedDuplicates::default()),
    })
}

/// The fingerprint of what is saved (empty if there is nothing), without reading the pairs.
fn saved_duplicates_fingerprint(db: &Connection) -> Result<String> {
    let h: Option<String> =
        db.query_row("SELECT substr(v, 1, instr(v, char(9)) - 1) FROM meta WHERE k = ?1 AND instr(v, char(9)) > 0", [TRADE_DUPLICATES_KEY], |row| row.get(0)).optional()?;
    Ok(h.unwrap_or_default())
}

/// Computes the trade duplicates and leaves them in `meta` (local): what the background process does
/// (`omoi review --refresh-trade-duplicates` after a write, and the daily `--refresh-drift`).
/// Incremental: only the memories that changed since the last computation (or new ones) are compared
/// with those of their trades; the pairs among the unchanged ones stay as they were. With another vector
/// model, everything from scratch. Returns the pairs visible from `scope`, like
/// [`saved_trade_duplicates`]. Nothing serving a request calls it.
pub fn trade_duplicates(db: &Connection, scope: &Scope) -> Result<Vec<Pair>> {
    trade_duplicates_with(db, scope, false)
}

/// [`trade_duplicates`]; `from_scratch` compares every pair again (the daily job: over time the word
/// weights and the vector mean move, and a pair that fell just short may make it without either of the
/// two changing).
fn trade_duplicates_with(db: &Connection, scope: &Scope, from_scratch: bool) -> Result<Vec<Pair>> {
    let fingerprint = trade_fingerprint(db)?;
    let model = vector_model(db)?;
    let (_, before) = read_computed(db)?;
    let before = if before.model == model && !from_scratch { before } else { ComputedDuplicates::default() };
    let mut q = db.prepare(&format!(
        "SELECT {}, m.updated_at || '|' || EXISTS(SELECT 1 FROM memory_vec v WHERE v.n = m.n) FROM memory m
         WHERE m.project_id = 0 AND m.space_id IS NULL AND instr(m.tags, '\"trade:') > 0 ORDER BY m.id",
        crate::memoria::COLUMNS
    ))?;
    let columns = q.column_count();
    let rows: Vec<(Memory, String)> = q.query_map([], |row| Ok((crate::memoria::from_row(row)?, row.get(columns - 1)?)))?.collect::<rusqlite::Result<_>>()?;
    let all: Vec<Memory> = rows.iter().map(|(m, _)| m.clone()).collect();
    let memories: BTreeMap<String, String> = rows.into_iter().map(|(m, e)| (m.id, e)).collect();
    // The ones that changed since the last computation (or weren't there).
    let changed: HashSet<usize> = all.iter().enumerate().filter(|(_, m)| before.memories.get(&m.id) != memories.get(&m.id)).map(|(i, _)| i).collect();
    let unchanged = |id: &str| memories.get(id).is_some_and(|e| before.memories.get(id) == Some(e));
    // What there was between two that didn't change still holds.
    let mut pairs: Vec<ComputedPair> = before.pairs.into_iter().filter(|p| unchanged(&p.newer) && unchanged(&p.older)).collect();
    let mut by_trade: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, m) in all.iter().enumerate() {
        for o in crate::oficios::de_etiquetas(&m.tags) {
            by_trade.entry(o).or_default().push(i);
        }
    }
    let refs: Vec<&Memory> = all.iter().collect();
    let trades: Vec<&String> = by_trade.keys().collect();
    let groups: Vec<Vec<usize>> = by_trade.values().cloned().collect();
    if !changed.is_empty() {
        for (i, j, sim) in crate::vectores::duplicadas_entre(db, &refs, &groups, Some(&changed))? {
            let (a, b) = (&all[i], &all[j]);
            // The first trade they share, for the detail.
            let trade = groups.iter().position(|g| g.contains(&i) && g.contains(&j)).map(|k| trades[k].as_str()).unwrap_or_default();
            let (new, old) = if (a.created_at.as_str(), a.id.as_str()) >= (b.created_at.as_str(), b.id.as_str()) { (a, b) } else { (b, a) };
            pairs.push(ComputedPair {
                newer: new.id.clone(),
                older: old.id.clone(),
                detail: format!("says the same as the other one (trade:{trade}, similarity {sim:.2}): merge them"),
                newer_changed_at: new.updated_at.clone(),
                older_changed_at: old.updated_at.clone(),
            });
        }
    }
    pairs.sort_by(|a, b| (&a.older, &a.newer).cmp(&(&b.older, &b.newer)));
    let c = ComputedDuplicates { model, memories, pairs };
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [TRADE_DUPLICATES_KEY, &format!("{fingerprint}\t{}", serde_json::to_string(&c)?)])?;
    filter_duplicates(db, scope, c.pairs)
}

/// The same, from what is saved and never computing anything, not even the first time: it is what
/// session start (`pending`), `context`, `omoi review` and the web view read. If the trade ones
/// changed since the last computation (or there is none), it launches the computation in the background
/// (it shows up next time). What is saved may lag behind: a pair only shows if both its memories are
/// still alive, trade ones, visible from `scope`, without a verdict and unchanged since it was computed.
pub fn saved_trade_duplicates(db: &Connection, scope: &Scope) -> Result<Vec<Pair>> {
    let (saved_fingerprint, c) = read_computed(db)?;
    // Launching it is only so that what is saved catches up: if it fails (the database busy with a long
    // sync), the read goes on with what there is.
    let _ = recompute_if_changed(db, &saved_fingerprint);
    filter_duplicates(db, scope, c.pairs)
}

/// After a write that may touch trade memories (`remember`, `update`, `forget`, a sync, an import): if
/// they changed since the last computation, launches it in the background. Never waits for it.
pub fn trade_duplicates_after_write(db: &Connection) -> Result<()> {
    recompute_if_changed(db, &saved_duplicates_fingerprint(db)?)
}

fn recompute_if_changed(db: &Connection, saved_fingerprint: &str) -> Result<()> {
    let fingerprint = trade_fingerprint(db)?;
    if fingerprint != saved_fingerprint {
        recompute_in_background(db, &fingerprint)?;
    }
    Ok(())
}

/// The lock file of the trade duplicates computation, next to the database.
fn duplicates_lock(db_path: &std::path::Path) -> std::path::PathBuf {
    db_path.with_extension("trade-duplicates.lock")
}

/// Launches `omoi review --refresh-trade-duplicates` separately (a detached process nobody waits for;
/// there is no daemon): not if one is already running (it holds the lock, and repeats while the trade ones
/// keep changing), nor if it was already launched for this `fingerprint` less than `SAME_FINGERPRINT` ago,
/// nor if another was launched less than `BETWEEN_LAUNCHES` ago. If the process died without saving,
/// after that window the next read or write launches it again. Only if the running binary is `omoi`
/// (like `deriva`: from a test the executable is the test one).
fn recompute_in_background(db: &Connection, fingerprint: &str) -> Result<()> {
    let Some(path) = db.path().filter(|p| !p.is_empty()).map(std::path::PathBuf::from) else { return Ok(()) };
    let is_omoi = std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| s.to_string_lossy().to_lowercase())).is_some_and(|s| s == "omoi");
    if !is_omoi || crate::sync::Cerrojo::ocupado(&duplicates_lock(&path)) {
        return Ok(());
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let launched: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [TRADE_DUPLICATES_LAUNCHED_KEY], |row| row.get(0)).optional()?;
    if let Some((h, when)) = launched.as_deref().and_then(|v| v.split_once('\t'))
        && let Ok(t) = when.parse::<u64>()
        && (now.saturating_sub(t) < BETWEEN_LAUNCHES || (h == fingerprint && now.saturating_sub(t) < SAME_FINGERPRINT))
    {
        return Ok(());
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [TRADE_DUPLICATES_LAUNCHED_KEY, &format!("{fingerprint}\t{now}")])?;
    crate::sync::lanzar(&["review", "--refresh-trade-duplicates"], Some(&path));
    Ok(())
}

/// Computes the trade duplicates and leaves them in `meta`: the background process. With the lock, so
/// there are never two at once (if there already is one, it does nothing: `None`), and again if the trade
/// ones changed meanwhile (a burst of `remember`), up to 5 times. `from_scratch`: the first round compares
/// every pair (the daily job). How many pairs.
pub fn refresh_trade_duplicates(db: &Connection, from_scratch: bool) -> Result<Option<usize>> {
    let _lock = match db.path().filter(|p| !p.is_empty()) {
        Some(path) => match crate::sync::Cerrojo::tomar_en(duplicates_lock(std::path::Path::new(path))) {
            Some(c) => Some(c),
            None => return Ok(None),
        },
        None => None,
    };
    let mut n = 0;
    for round in 0..5 {
        n = trade_duplicates_with(db, &Scope::All, from_scratch && round == 0)?.len();
        if saved_duplicates_fingerprint(db)? == trade_fingerprint(db)? {
            break;
        }
    }
    Ok(Some(n))
}

/// Of the computed pairs, the ones that hold now: both memories alive, still trade ones, visible from
/// `scope` and unchanged since the computation (their `updated_at`), and the pair without a verdict.
fn filter_duplicates(db: &Connection, scope: &Scope, pairs: Vec<ComputedPair>) -> Result<Vec<Pair>> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let visible: HashMap<String, String> = {
        let mut q = db.prepare(&format!(
            "SELECT m.id, m.updated_at FROM memory m WHERE m.project_id = 0 AND m.space_id IS NULL AND instr(m.tags, '\"trade:') > 0 AND {}",
            scope.sql()
        ))?;
        q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let judged = crate::veredictos::pairs(db)?;
    Ok(pairs
        .into_iter()
        .filter(|p| {
            visible.get(&p.newer) == Some(&p.newer_changed_at)
                && visible.get(&p.older) == Some(&p.older_changed_at)
                && !judged.contains_key(&crate::veredictos::pair_key(&p.newer, &p.older))
        })
        .map(|p| (p.newer, p.older, p.detail))
        .collect())
}

/// The kinds of what looks like a secret in a memory's title and content, one per finding.
pub fn secrets_of(m: &Memory) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = crate::secretos::hallazgos(&m.title).iter().map(|h| h.tipo).collect();
    v.extend(crate::secretos::hallazgos(&m.content).iter().map(|h| h.tipo));
    v
}

fn db_now(db: &Connection) -> Result<String> {
    Ok(db.query_row(&format!("SELECT {NOW}"), [], |row| row.get(0))?)
}

/// Days between two ISO dates (the ones the database stores). No dependencies: civil days by hand.
fn days_between(since: &str, until: &str) -> f64 {
    fn day_number(s: &str) -> Option<f64> {
        let (y, m, d) = (s.get(0..4)?.parse::<i64>().ok()?, s.get(5..7)?.parse::<i64>().ok()?, s.get(8..10)?.parse::<i64>().ok()?);
        let (h, mi) = (s.get(11..13).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0), s.get(14..16).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0));
        // Days since 1970-01-01 (Howard Hinnant's algorithm).
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some((era * 146097 + doe - 719468) as f64 + (h * 60 + mi) as f64 / 1440.0)
    }
    match (day_number(since), day_number(until)) {
        (Some(a), Some(b)) => b - a,
        _ => 0.0,
    }
}

/// Days without review, if it is due: since its last change, the last "it's fine as it is" or the last
/// time the agent received it, whichever is newest. A memory recalled every week that nobody corrects
/// is reviewed; one that has gone 120 days without anyone seeing or touching it is not (spaced repetition).
fn days_unreviewed(m: &Memory, reviewed: &HashMap<(String, String), String>, now: &str) -> Option<i64> {
    let period = review_period_days(m)?;
    let mut since = m.updated_at.as_str();
    if let Some(at) = reviewed.get(&(m.id.clone(), DUE.to_string()))
        && at.as_str() > since
    {
        since = at;
    }
    if let Some(r) = m.recalled_at.as_deref()
        && r > since
    {
        since = r;
    }
    let n = days_between(since, now);
    (n > period).then_some(n as i64)
}

/// "It's fine as it is": doesn't warn about this again until the memory changes. In a pair, both hold
/// (`compatible`); in a single memory, `ok:<kind>`. It is a verdict, so it travels, signed by `user`
/// (it comes from the web view or the CLI, not from the agent).
pub fn dismiss(db: &Connection, memory_id: &str, kind: &str, other_id: &str) -> Result<()> {
    anyhow::ensure!(KINDS.contains(&kind), "unknown review kind «{kind}»");
    if other_id.is_empty() {
        crate::veredictos::set(db, memory_id, "", &format!("ok:{kind}"), "", crate::veredictos::USER)
    } else {
        crate::veredictos::set(db, memory_id, other_id, crate::veredictos::COMPATIBLE, "", crate::veredictos::USER)
    }
}

/// The memories the just-saved one makes obsolete: (id, title, why).
pub fn superseded_by(db: &Connection, id: &str) -> Result<Vec<(String, String, String)>> {
    let mut q = db.prepare(
        "SELECT r.memory_id, m.title, r.detail FROM review r JOIN memory m ON m.id = r.memory_id
         WHERE r.other_id = ?1 AND r.kind = ?2",
    )?;
    let v = q.query_map(params![id, OUTDATED], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// For recall: a short note per memory that should be taken with care (superseded, contradicted,
/// outdated or stale status).
/// The notes name the other memory (its id, its title, the verdict's reason) only if it is visible from
/// `scope`: a trade memory visible in two clients may have a verdict with one from one of them.
pub fn notes(db: &Connection, scope: &Scope, memories: &[&Memory]) -> Result<HashMap<String, String>> {
    notes_with(db, scope, memories, false)
}

/// For `search`: the same and, in addition, the ones already due for review. Not in recall, to avoid
/// noise: there only what says the memory may be wrong goes.
pub fn search_notes(db: &Connection, scope: &Scope, memories: &[&Memory]) -> Result<HashMap<String, String>> {
    notes_with(db, scope, memories, true)
}

fn notes_with(db: &Connection, scope: &Scope, memories: &[&Memory], with_review: bool) -> Result<HashMap<String, String>> {
    let mut r = HashMap::new();
    if memories.is_empty() {
        return Ok(r);
    }
    // What is visible from the scope's space (the other memories of its space, yes).
    let view = scope.widen();
    let visible = |id: &str| -> Result<bool> { if matches!(view, Scope::All) { Ok(true) } else { view.sees(db, id) } };
    let short_ref = |id: &str| -> Result<String> {
        let t: String = db.query_row("SELECT title FROM memory WHERE id = ?1", [id], |row| row.get(0))?;
        Ok(format!("{} «{}»", id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>()))
    };
    // The verdict's reason, or what to do if it doesn't carry one.
    let reason_or = |reason: &str, otherwise: &str| -> String { if reason.trim().is_empty() { otherwise.into() } else { crate::veredictos::short_reason(reason) } };
    for v in crate::veredictos::pairs(db)?.into_values() {
        for (this, other) in [(&v.memory_id, &v.other_id), (&v.other_id, &v.memory_id)] {
            if r.contains_key(this.as_str()) || !memories.iter().any(|m| &m.id == this) {
                continue;
            }
            let note = match v.relation.as_str() {
                SUPERSEDED if this == &v.memory_id => {
                    // If the replacement is in turn superseded, the note leads to the end of the chain.
                    let (last, hops, reason) = match crate::veredictos::chain(db, this)? {
                        Some(end) if end.hops > 1 => (end.id, end.hops, end.reason),
                        _ => (other.clone(), 1, v.reason.clone()),
                    };
                    let via = if hops > 1 { format!(" (via {hops})") } else { String::new() };
                    if visible(&last)? {
                        format!("superseded by {}{via}: {}", short_ref(&last)?, reason_or(&reason, "use that one"))
                    } else {
                        // Neither its title nor the reason (which may be the other one's title).
                        "superseded by a newer memory that is not in this scope".to_string()
                    }
                }
                // The contradicted one (`other_id`) waits for the person (`veredictos::on_hold`).
                CONTRADICTS if visible(other)? && this == &v.other_id => format!(
                    "ON HOLD: {} contradicts it ({}); ask the user which holds and save the answer with `decisions` (contradiction, keep, answer)",
                    short_ref(other)?,
                    reason_or(&v.reason, "no reason given")
                ),
                CONTRADICTS if visible(other)? => format!("contradicts {} (on hold until the user says which holds): {}", short_ref(other)?, reason_or(&v.reason, "ask the user which holds")),
                _ => continue,
            };
            r.insert(this.clone(), note);
        }
    }
    // Without `refresh`: recomputing costs about 50 ms and recall must stay below 30.
    // Writes (MCP, CLI, sync, the view) already leave it up to date.
    let excluded = dismissed(db)?;
    let mut q = db.prepare(
        "SELECT r.other_id, r.detail, m.title FROM review r JOIN memory m ON m.id = r.other_id
         WHERE r.memory_id = ?1 AND r.kind = ?2 LIMIT 1",
    )?;
    let now = db_now(db)?;
    // The review date also goes in recall: it says the memory may have stopped holding (the token
    // expired, the offer ended). Verdicts are only read if some memory carries a date.
    if memories.iter().any(|m| review_date_of(&m.tags).is_some()) {
        let (reviewed, today) = (crate::veredictos::oks(db, false)?, Today::load(db)?);
        for m in memories {
            if !r.contains_key(&m.id)
                && let Some(f) = date_reached(m, &reviewed, &today)
            {
                let why = m.review_reason.as_deref().map(|x| format!(" (why: {x})")).unwrap_or_default();
                r.insert(m.id.clone(), format!("review date {f} reached{why}: check it still holds"));
            }
        }
    }
    for m in memories {
        if r.contains_key(&m.id) {
            continue;
        }
        if let Some((other, detail, title)) =
            q.query_row(params![m.id, OUTDATED], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))).optional()?
            && !excluded.contains(&(m.id.clone(), OUTDATED.to_string(), other.clone()))
            && visible(&other)?
        {
            let short: String = title.chars().take(80).collect();
            r.insert(m.id.clone(), format!("may be outdated: newer {} «{short}» ({detail})", other.chars().take(12).collect::<String>()));
        } else if status_warns(m, days_between(&m.updated_at, &now)) {
            let later: i64 = db.query_row(
                "SELECT count(*) FROM memory WHERE project_id = ?1 AND id != ?2 AND updated_at > ?3",
                params![m.project_id, m.id, m.updated_at],
                |row| row.get(0),
            )?;
            if later > 0 && !excluded.contains(&(m.id.clone(), STALE_STATUS.to_string(), String::new())) {
                r.insert(m.id.clone(), format!("may be stale: {later} memories of this project changed after it"));
            }
        }
    }
    if with_review {
        let reviewed = crate::veredictos::oks(db, false)?;
        for m in memories {
            if !r.contains_key(&m.id)
                && let Some(n) = days_unreviewed(m, &reviewed, &now)
            {
                r.insert(m.id.clone(), format!("not reviewed in {n} days: check it still holds"));
            }
        }
    }
    Ok(r)

}

// --- The warning at session start ------------------------------------------------------------------
//
// The review is only useful if someone opens it. When it piles up, the agent receives at start a short
// paragraph with the numbers and the instruction to propose what to do: it decides with the user,
// Omoikane carries no language model. Once a day per database, or again the same day if the number
// grows enough.
//
// Calibrated with Marc's real memory of 4-nov-2026 (178 memories): from the home folder there are
// 33 without counting paths (21 too long, 9 broken links, 3 outdated), almost one in five memories:
// it warns. From D:\Omoikane, 15 of 135 (9 long, 4 links, 2 outdated): it doesn't. With 10,000
// memories of a team, the ones nobody has recalled in 30 days are already hundreds: a fixed number
// would always warn, so the threshold is also a proportion.
//
// Counting with `notices` costs half a second with 10,000 memories (reading them whole and searching
// for secrets in each one), so at start it counts with `pending`: what is saved in `review`, the
// verdicts and the small columns of `memory`, without the content or the disk. Broken paths and secrets
// only show in the full review (`omoi review`, the `context` tool).

/// Pending items from which it warns, with few memories.
pub const NOTICE_MIN: usize = 20;
/// The share of the scope's memories that has to be pending, with many (10 %).
pub const NOTICE_RATIO: f64 = 0.10;
/// Where the last warning is recorded: `day|count`.
const NOTICE_KEY: &str = "revision_aviso";

/// The threshold for a scope with `memories` memories.
pub fn notice_threshold(memories: usize) -> usize {
    NOTICE_MIN.max((memories as f64 * NOTICE_RATIO).ceil() as usize)
}

/// How much the number has to grow to repeat the warning the same day: 5, or 10 % if that is more.
fn grew(before: usize, now: usize) -> bool {
    now >= before + 5.max(before / 10)
}

/// Where the last counts of `pending` are saved: `key\tJSON`.
const COUNTS_KEY: &str = "revision_aviso_cifras";

/// The reader's day, not the UTC one.
fn today(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT date('now', 'localtime')", [], |row| row.get(0))?)
}

/// How many there are of each kind in the scope, the same that `notices` would count except broken paths
/// and secrets, without reading the memories' content. Saved in `meta` by day, scope, `memory`
/// fingerprint and verdicts: until something is written, the day's next sessions don't count. What
/// changes just by recalling (unused, due) only goes down, and waits until tomorrow.
pub fn pending(db: &Connection, scope: &Scope) -> Result<BTreeMap<&'static str, usize>> {
    refresh(db)?;
    let fingerprint: String = db.query_row("SELECT v FROM meta WHERE k = 'revision'", [], |row| row.get(0)).optional()?.unwrap_or_default();
    let (n, stamp): (i64, String) = db.query_row("SELECT count(*), COALESCE(max(hlc), '') FROM verdict", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let key = format!("{}|{fingerprint}|{n}|{stamp}|{}|{}", today(db)?, scope.sql(), saved_duplicates_fingerprint(db)?);
    let saved: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [COUNTS_KEY], |row| row.get(0)).optional()?;
    if let Some((k, json)) = saved.as_deref().and_then(|v| v.split_once('\t'))
        && k == key
        && let Ok(m) = serde_json::from_str::<BTreeMap<String, usize>>(json)
    {
        return Ok(m.into_iter().filter_map(|(k, n)| KINDS.iter().find(|t| **t == k).map(|t| (*t, n))).collect());
    }
    let r = count_pending(db, scope)?;
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [COUNTS_KEY, &format!("{key}\t{}", serde_json::to_string(&r)?)])?;
    Ok(r)
}

fn count_pending(db: &Connection, scope: &Scope) -> Result<BTreeMap<&'static str, usize>> {
    let excluded = dismissed(db)?;
    // Only the small columns, and the length only of those that exceed it in bytes (a character is at
    // least one byte): `octet_length` doesn't walk the text. The space goes by its number, not its key.
    let mut q = db.prepare(&format!(
        "SELECT m.id, m.kind, m.tags, m.created_at, m.updated_at, m.recalled_at, m.recall_count, m.project_id,
                COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id), 0),
                CASE WHEN octet_length(m.content) > {RECALL_LEN} THEN length(m.content) ELSE 0 END
         FROM memory m WHERE {}",
        scope.sql()
    ))?;
    let all: Vec<(Memory, usize)> = q
        .query_map([], |row| {
            let m = Memory {
                id: row.get(0)?,
                kind: row.get::<_, String>(1)?.parse().unwrap_or(crate::memoria::Kind::Fact),
                tags: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                recalled_at: row.get(5)?,
                recall_count: row.get(6)?,
                project_id: row.get(7)?,
                space: row.get::<_, i64>(8)?.to_string(),
                ..Default::default()
            };
            Ok((m, row.get::<_, i64>(9)? as usize))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let in_scope: HashSet<&str> = all.iter().map(|(m, _)| m.id.as_str()).collect();
    let mut r: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut add = |k: &'static str| *r.entry(k).or_insert(0) += 1;
    {
        let mut q = db.prepare("SELECT memory_id, other_id FROM review WHERE kind = ?1")?;
        let rows: Vec<(String, String)> = q.query_map([OUTDATED], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        for (old, new) in rows {
            if in_scope.contains(old.as_str()) && in_scope.contains(new.as_str()) && !excluded.contains(&(old, OUTDATED.to_string(), new)) {
                add(OUTDATED);
            }
        }
    }
    for v in crate::veredictos::pairs(db)?.into_values() {
        if v.relation == crate::veredictos::COMPATIBLE {
            continue;
        }
        let mut other = v.other_id.clone();
        if v.relation == SUPERSEDED
            && let Some(end) = crate::veredictos::chain(db, &v.memory_id)?
            && end.hops > 1
        {
            other = end.id;
        }
        if in_scope.contains(v.memory_id.as_str()) && in_scope.contains(other.as_str()) {
            add(if v.relation == SUPERSEDED { SUPERSEDED } else { CONTRADICTS });
        }
    }
    // At session start trade memories are not compared: what the background process computed.
    for (new, old, _) in saved_trade_duplicates(db, scope)? {
        if in_scope.contains(new.as_str()) && in_scope.contains(old.as_str()) {
            add(TRADE_DUPLICATE);
        }
    }
    {
        let mut q = db.prepare("SELECT memory_id FROM review WHERE kind = ?1")?;
        let rows: Vec<String> = q.query_map([BROKEN_LINK], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        for id in rows {
            if in_scope.contains(id.as_str()) && !excluded.contains(&(id, BROKEN_LINK.to_string(), String::new())) {
                add(BROKEN_LINK);
            }
        }
    }
    let now = db_now(db)?;
    let reviewed = crate::veredictos::oks(db, false)?;
    // The last change of each project (and space): a status has fallen behind if there is another one after it.
    let mut last: HashMap<(i64, &str), &str> = HashMap::new();
    for (m, _) in &all {
        let u = last.entry((m.project_id, m.space.as_str())).or_insert(m.updated_at.as_str());
        if m.updated_at.as_str() > *u {
            *u = m.updated_at.as_str();
        }
    }
    let today = Today::load(db)?;
    // Only what is already computed: at start git is not launched (`deriva::for_scope`).
    for _ in 0..crate::deriva::for_scope(db, scope).map(|d| d.len()).unwrap_or(0) {
        add(CODE_DRIFT);
    }
    for (m, len) in &all {
        let is_dismissed = |k: &str| excluded.contains(&(m.id.clone(), k.to_string(), String::new()));
        if date_reached(m, &reviewed, &today).is_some() {
            add(REVIEW_DATE);
        }
        if days_unreviewed(m, &reviewed, &now).is_some() {
            add(DUE);
        }
        if status_warns(m, days_between(&m.updated_at, &now))
            && !is_dismissed(STALE_STATUS)
            && last.get(&(m.project_id, m.space.as_str())).is_some_and(|u| *u > m.updated_at.as_str())
        {
            add(STALE_STATUS);
        }
        if m.recall_count == 0 && m.recalled_at.is_none() && days_between(&m.created_at, &now) > UNUSED_DAYS && !is_dismissed(UNUSED) {
            add(UNUSED);
        }
        if *len > RECALL_LEN && !is_dismissed(TOO_LONG) {
            add(TOO_LONG);
        }
    }
    Ok(r)
}

/// The counts of an already done review (that of `notices`), by kind.
pub fn counts(notices: &[Notice]) -> BTreeMap<&'static str, usize> {
    let mut r = BTreeMap::new();
    for a in notices {
        if let Some(k) = KINDS.iter().find(|k| **k == a.kind) {
            *r.entry(*k).or_insert(0) += 1;
        }
    }
    r
}

/// At session start (the recall): the warning, if due.
pub fn start_notice(db: &Connection, scope: &Scope, memories: usize) -> Result<Option<String>> {
    let memories = notice(db, &pending(db, scope)?, memories)?;
    // And duplicated projects or ones that aren't (`nombres`), once a day: if it fails, only that one is lost.
    let projects = crate::nombres::start_notice(db, scope).ok().flatten();
    Ok(match (memories, projects) {
        (Some(a), Some(b)) => Some(format!("{a}
{b}")),
        (a, b) => a.or(b),
    })
}

/// The warning for these counts in a scope of `memories` memories, if they reach the threshold and it
/// wasn't already given today (or they have grown since). If it goes out, it is recorded in `meta`: it
/// belongs to this machine, it doesn't travel.
pub fn notice(db: &Connection, counts: &BTreeMap<&'static str, usize>, memories: usize) -> Result<Option<String>> {
    // Possible secrets don't count in the total that is compared and saved: session start doesn't count
    // them, and `context` would warn again in the same session. But one always warns: it travels with
    // sync and backups.
    let total: usize = counts.iter().filter(|(k, _)| **k != SECRET).map(|(_, n)| n).sum();
    // A review date that arrives also always warns: someone set it to find out that day.
    let always = [SECRET, REVIEW_DATE].iter().any(|k| counts.get(k).is_some_and(|n| *n > 0));
    if total < notice_threshold(memories) && !always {
        return Ok(None);
    }
    let today = today(db)?;
    let before: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [NOTICE_KEY], |row| row.get(0)).optional()?;
    if let Some(n) = before.as_deref().and_then(|v| v.split_once('|')).filter(|(d, _)| *d == today).and_then(|(_, n)| n.parse::<usize>().ok())
        && !grew(n, total)
    {
        return Ok(None);
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [NOTICE_KEY, &format!("{today}|{total}")])?;
    Ok(Some(notice_text(counts)))
}

/// What each kind is called in the warning.
fn notice_name(kind: &str) -> &'static str {
    match kind {
        SECRET => "possible secrets",
        REVIEW_DATE => "review dates reached",
        SUPERSEDED => "superseded",
        CONTRADICTS => "contradictions",
        OUTDATED => "outdated",
        TRADE_DUPLICATE => "duplicated trade memories",
        BROKEN_LINK => "broken [[links]]",
        STALE_STATUS => "stale status",
        CODE_DRIFT => "code changed under them",
        DUE => "due for a check",
        BROKEN_PATH => "broken paths",
        UNUSED => "never recalled",
        TOO_LONG => "too long",
        _ => "other",
    }
}

/// The paragraph for the agent, in English like the rest of the recall: the counts of the kinds present,
/// in review order, and what to do with them.
pub fn notice_text(counts: &BTreeMap<&'static str, usize>) -> String {
    let parts: Vec<String> = KINDS.iter().filter_map(|k| counts.get(k).filter(|n| **n > 0).map(|n| format!("{n} {}", notice_name(k)))).collect();
    format!(
        "Memory review: {} items pending ({}). Look at them (`omoi review`, or `review` in the `context` tool) and propose concrete actions to the user: update, merge with `absorb`, `forget`, mark `supersedes`/`compatible`, fix the links. Change nothing without the user's OK.",
        counts.values().sum::<usize>(),
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_written_in_the_text_counts_as_review_date() {
        assert_eq!(written_review_date("[review-after] 10-nov-2026. Para el servidor"), Some("2026-11-10".into()));
        assert_eq!(written_review_date("[review-after] 2026-12-01"), Some("2026-12-01".into()));
        assert_eq!(written_review_date("sin fecha de revisión, solo 10-nov-2026"), None);
    }

    #[test]
    fn journal_history_does_not_count_as_current_version() {
        let d = normalize("Desde v0.4.0 también Bluesky\nVa en la 0.7.4 ya");
        assert!(!versions(&without_history(&d)).values().flatten().any(|x| x == &vec![0, 4, 0]));
        let t = normalize("Plugin social-publish v0.3.0 en marcha\n**19-oct-2026 (v0.6.1 → 0.6.2):** herramienta humanizar\nVa en la 0.7.4 desde ayer");
        let v = versions(&without_history(&t));
        assert!(v.values().flatten().any(|x| x == &vec![0, 3, 0]), "{v:?} {:?}", without_history(&t));
        assert!(!v.values().flatten().any(|x| x == &vec![0, 6, 2]), "{v:?}");
    }

    #[test]
    fn what_cleanup_adds_goes_in_the_memory_language() {
        assert!(is_spanish("[blocking] android: YA EXISTÍA, no lo trae esta tarea: con letra 1,5 la pantalla"));
        assert!(!is_spanish("The release is blocked until the tests pass on Windows and the tag is pushed"));
    }

    #[test]
    fn paths_from_the_other_os_are_not_broken() {
        assert_eq!(from_other_os("D:/Omoikane/src/okf.rs"), !cfg!(windows));
        assert_eq!(from_other_os("C:\\Users\\marcm\\x.md"), !cfg!(windows));
        assert_eq!(from_other_os("/Users/marc/x.md"), cfg!(windows));
        assert!(!from_other_os("src/okf.rs"));
    }

    #[test]
    fn dates_and_versions() {
        assert_eq!(dates("hecho el 29-oct-2026 y el 2026-11-02"), vec!["2026-10-29", "2026-11-02"]);
        assert_eq!(dates("3 de mayo"), Vec::<String>::new());
        let v = versions("la 0.9.2 usa rusqlite 0.41.0 y la v0.10.0");
        assert_eq!(v[""].iter().map(|x| version_text(x)).collect::<Vec<_>>(), vec!["0.9.2", "0.10.0"]);
        assert_eq!(v["rusqlite"].len(), 1);
        // A word from another line or another sentence doesn't count.
        let v = versions("otras máquinas\n**v0.2.0 publicada. La v0.3.0");
        assert_eq!(v[""].iter().map(|x| version_text(x)).collect::<Vec<_>>(), vec!["0.2.0", "0.3.0"]);
    }

    #[test]
    fn days() {
        assert_eq!(days_between("2026-10-29T00:00:00Z", "2026-11-05T00:00:00Z"), 7.0);
        assert_eq!(days_between("2024-02-28", "2024-03-01"), 2.0);
    }

    #[test]
    fn split_sentences() {
        assert_eq!(sentences("uno. dos: tres\ncuatro v0.9.2 cinco"), vec!["uno", " dos", " tres", "cuatro v0.9.2 cinco"]);
    }

    #[test]
    fn local_midnight_in_utc() {
        // Madrid in winter (+1 h): day 10 starts on the 9th at 23:00 UTC.
        assert_eq!(utc_start("2026-11-10", 3600), "2026-11-09T23:00:00");
        assert_eq!(utc_start("2026-01-01", 3600), "2025-12-31T23:00:00");
        assert_eq!(utc_start("2024-03-01", 7200), "2024-02-29T22:00:00");
        // West of UTC, the same day.
        assert_eq!(utc_start("2026-11-10", -5 * 3600), "2026-11-10T05:00:00");
        assert_eq!(utc_start("2026-11-10", 0), "2026-11-10T00:00:00");
        for days in [-1, 0, 59, 10_000, 20_402] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_since_1970(y, m, d), days);
        }
    }
}
