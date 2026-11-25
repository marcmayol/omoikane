//! Verified by a person versus written by an agent (1.8.5, from OKF Agent Memory: its
//! `generated`/`verified` levels with `verified.by`).
//!
//! Almost everything in memory is written by an agent. A person saying "this is how it is" is worth
//! more, and the agent that receives it must be able to tell. Verification is a seal in
//! `memory.extra` under the key [`VERIFIED_KEY`]: who, when and the fingerprint of the text they saw (kind,
//! title and content). No schema change and the same path as the data (`data`): it travels as a
//! `verified` field of the line, and a 1.0–1.8 keeps it and forwards it without understanding it.
//!
//! **An agent cannot verify itself.** No MCP tool does it or reads it from what it receives
//! (`remember`, `update` and `data` never reach this key, and `update` on the text removes it);
//! the `/tools/` endpoint of `omoi serve` is the MCP, so neither does it. The person verifies in
//! the CLI (`omoi verify`, which asks in a terminal and outside one requires `--yes`, and refuses
//! inside an agent's shell) and in the web view (a button, `POST /api/verify`, which requires the
//! view's `Origin` and the token `omoi serve` puts in the page). Seals also arrive on import,
//! never inside an agent's shell: `omoi import --okf` (a person's `verified` from a bundle signed
//! by this database with the same text, or from another with `--trust-verified`; see `okf`) and
//! `omoi import <file>` (the file's seals). Sync brings those from the person's other machines,
//! through their encrypted repository: they are their trust. An agent with a shell that wants to
//! bypass it on purpose can (just as it can edit the database or load the page to read the
//! token): this is not a security boundary, just making sure no normal agent path ends up as
//! "verified".
//!
//! **Changing the text un-verifies it.** The person verified that text, not whatever comes after:
//! if an agent changes it, the memory goes back to "written by an agent" until someone looks at it
//! again. Changing tags, paths, pinning or moving it doesn't touch it (it doesn't change what it
//! says). This version removes the seal when the text changes; and since the seal carries the
//! text's fingerprint, a change made by an older version (which keeps the seal without
//! understanding it) also leaves it unverified: when read, the fingerprint no longer matches.

use crate::memory::{NOW, Memory};
use crate::tr;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The seal's key in `memory.extra` (and the `verified` field of the line that travels).
pub const VERIFIED_KEY: &str = "verified";

/// The local mark (in `meta`, which doesn't travel) of another person's verified seal that arrived
/// through a space's repository (`sync::foreign_seal`): `sello_ajeno:<id>` with its `by` and `at` on
/// two lines. While the memory carries that same seal, here it doesn't count as verified
/// (`memory::COLUMNS`); the seal stays in the memory, as is, for the others.
pub const FOREIGN_SEAL_PREFIX: &str = "sello_ajeno:";

/// When the person last removed the verification (`memory.extra`), so that reimporting an old
/// bundle doesn't verify it again.
pub const UNVERIFIED_AT_KEY: &str = "unverified_at";

/// What is shown: who verified it and when. Only if the verification still holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub by: String,
    pub at: String,
}

/// What is stored: the above, the fingerprint of the verified text and the signature of whoever put it ([`SealKeys`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Seal {
    by: String,
    at: String,
    #[serde(default)]
    sha: String,
    /// HMAC-SHA256 of the seal (memory id, `by`, `at` and `sha`) with the seal key of the person
    /// who put it: says it is theirs without trusting `by`, which anyone can write.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    mac: String,
}

// --- Whose seal it is -------------------------------------------------------------------------------
//
// Through a space's repository (shared with other people) arrive seals put by another person: their
// trust, not that of whoever uses this database (`sync::foreign_seal`). The line's `by` can't tell:
// anyone sharing the space can write `"by": "Marc"`. What decides is a signature only this person can
// compute: an HMAC of the seal with their **seal key**, derived from the key of their general
// repository (the personal, encrypted one, held only by their machines: it never travels through a
// space's) and therefore the same on all their machines. Without a general repository, a random key
// of this database (`meta('sello_firma')`, which doesn't travel): its seals count here and on no other
// machine. A seal without a signature, with another person's, or copied from another memory (the
// signature covers the id) or from another text (it covers the fingerprint) is not this person's.
//
// **When the keys change** (1.8.6). Rotating the general's key, joining one (`join`), creating it
// (`init`) or turning it off (`sync off`) changes the seal key. So that the person's verifications
// don't stop counting:
// - Seal keys derived from a general repository of this person are recorded in
//   `meta('sello_firma_antes')` (local, like its own): they still count for checking seals even when
//   their key is gone (`sync off`, another general). They are never used to sign: only the current one.
// - After the change ([`after_key_change`]), the seals this database considers its own (their
//   signature holds with one of its keys and they are not marked as another person's) are re-signed
//   with the general's current key, so that machines that only have that one accept them (one that
//   joins after rotating it). And those marked as another person's (`FOREIGN_SEAL_PREFIX`) are checked again
//   with the current keys: one signed with a key this machine just received (the new one from a
//   rotation done on another machine) was theirs. None becomes theirs without a valid signature.
// - Another person's general (`join --another-person`) gives no seal key: the other person also has
//   its key, and whatever they signed with it would count here as one's own. This database keeps its
//   own (its own and the recorded ones).
// - A machine that joined another person's general with 1.8.5 doesn't have the mark, and nothing in
//   the database distinguishes it from a machine of the same person (with `--another-person` the
//   configured person was already the local one): that general's keys are recorded like anyone's.
//   Joining again with `--another-person` forgets the ones it recognizes ([`forget_general_keys`])
//   and `omoi sync forget-seal-keys` forgets them all ([`forget_recorded_keys`]).

/// This database's key to sign seals without a general repository (hex, 32 bytes).
const META_OWN_KEY: &str = "sello_firma";
/// Seal keys of the general repositories this database had (hex, one per line, from oldest to
/// newest): they still count for checking.
const META_PREVIOUS: &str = "sello_firma_antes";
/// What is signed with the general repository's key to derive the seal key (that key is never used
/// as is: it's the encryption one).
const DERIVE: &[u8] = b"omoikane-verified-seal-key-1";

/// This person's seal keys on this machine: which one signs and which ones are valid.
pub(crate) struct SealKeys {
    sign: Option<Vec<u8>>,
    valid: Vec<Vec<u8>>,
    /// Whether `sign` comes from the general repository (all the person's machines have it).
    from_general: bool,
}

fn hmac(key: &[u8], text: &[u8]) -> Vec<u8> {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), text).as_ref().to_vec()
}

/// The seal keys derived from this database's general repository, from oldest to current
/// (none without a general, or if it is another person's).
fn general_keys(db: &Connection) -> Vec<Vec<u8>> {
    let Some(path) = db.path().filter(|p| !p.is_empty()) else { return Vec::new() };
    if crate::sync::read_config_from(&crate::sync::paths(std::path::Path::new(path))).ok().flatten().is_some_and(|c| c.another_person) {
        return Vec::new();
    }
    general_repo_keys(db)
}

/// The seal keys that would come from this database's general repository, whoever it belongs to.
fn general_repo_keys(db: &Connection) -> Vec<Vec<u8>> {
    let Some(path) = db.path().filter(|p| !p.is_empty()) else { return Vec::new() };
    let r = crate::sync::paths(std::path::Path::new(path));
    if !crate::project::exists(&r.key_file) {
        return Vec::new();
    }
    match crate::sync::read_keys(&r) {
        Ok(c) => c.in_order().map(|k| hmac(k, DERIVE)).collect(),
        Err(_) => Vec::new(),
    }
}

/// Forgets recorded seal keys ([`META_PREVIOUS`]): those matching `remove`. Returns how many. If it
/// forgets any, the seals of a space's memories that no longer hold with the remaining keys become
/// another person's ([`FOREIGN_SEAL_PREFIX`]), as if they had just arrived through the space's repository:
/// what was accepted with a forgotten key stops counting here.
fn forget_recorded(db: &Connection, remove: impl Fn(&[u8]) -> bool) -> Result<usize> {
    let v = recorded(db)?;
    let remaining: Vec<String> = v.iter().filter(|k| !remove(k)).map(|k| to_hex(k)).collect();
    let n = v.len() - remaining.len();
    if n == 0 {
        return Ok(0);
    }
    let tx = db.unchecked_transaction()?;
    if remaining.is_empty() {
        tx.execute("DELETE FROM meta WHERE k = ?1", [META_PREVIOUS])?;
    } else {
        tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META_PREVIOUS, remaining.join("\n")])?;
    }
    let l = seal_keys(&tx, false)?;
    let rows: Vec<(String, String)> = {
        let mut q = tx.prepare(
            "SELECT m.id, m.extra FROM memory m WHERE instr(m.extra, '\"verified\"') > 0
             AND (m.space_id IS NOT NULL OR m.project_id IN (SELECT id FROM project WHERE space_id IS NOT NULL))",
        )?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, extra) in rows {
        let Some(v) = serde_json::from_str::<serde_json::Value>(&extra).ok().and_then(|mut e| e.get_mut(VERIFIED_KEY).map(serde_json::Value::take)) else { continue };
        if is_own_seal(&l, &id, &v) {
            continue;
        }
        let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![format!("{FOREIGN_SEAL_PREFIX}{id}"), format!("{}\n{}", text("by"), text("at"))])?;
    }
    tx.commit()?;
    Ok(n)
}

/// `omoi sync forget-seal-keys`: forgets all recorded seal keys from previous generals. For a
/// machine that joined another person's general with 1.8.5 (without the `another_person` mark): that
/// general's keys were recorded as this person's (when turning it off or rotating it) and the other
/// person's seals kept counting here as one's own. This person's verifications signed only with one
/// of them stop counting on this machine until verified again. Returns how many it forgot.
pub fn forget_recorded_keys(db: &Connection) -> Result<usize> {
    forget_recorded(db, |_| true)
}

/// After `join --another-person` to the general: the seal keys of that repository recorded earlier
/// (a machine that was joined to it with 1.8.5, without the mark, and turned it off to join again)
/// are forgotten, and are not recorded again ([`general_keys`] gives none with the mark). Those from
/// before a rotation this machine doesn't have are not recognized: that's `forget-seal-keys`.
pub(crate) fn forget_general_keys(db: &Connection) -> Result<usize> {
    let theirs = general_repo_keys(db);
    forget_recorded(db, |k| theirs.iter().any(|s| s == k))
}

/// The recorded seal keys from previous generals ([`META_PREVIOUS`]).
fn recorded(db: &Connection) -> Result<Vec<Vec<u8>>> {
    let v: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META_PREVIOUS], |f| f.get(0)).optional()?;
    Ok(v.unwrap_or_default().lines().filter_map(from_hex).filter(|k| k.len() == 32).collect())
}

/// Records in [`META_PREVIOUS`] the seal keys of the current general repository not yet there: so
/// they still count if its key goes away (`sync off`, another general). Called by `sync off` (before
/// deleting the key) and [`after_key_change`].
pub(crate) fn record_keys(db: &Connection) -> Result<()> {
    let mut v = recorded(db)?;
    let n = v.len();
    for k in general_keys(db) {
        if !v.contains(&k) {
            v.push(k);
        }
    }
    if v.len() != n {
        let text: Vec<String> = v.iter().map(|k| to_hex(k)).collect();
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META_PREVIOUS, text.join("\n")])?;
    }
    Ok(())
}

/// This database's seal keys: one per key of the general repository (the current one signs; seals
/// from before rotating it still count with the old ones), those of previous generals
/// ([`META_PREVIOUS`], only for checking) and the database's own (which signs if there is no
/// general repository). With `create`, the database's own is created if needed to sign.
pub(crate) fn seal_keys(db: &Connection, create: bool) -> Result<SealKeys> {
    let mut l = SealKeys { sign: None, valid: Vec::new(), from_general: false };
    for d in general_keys(db) {
        l.valid.push(d.clone());
        l.sign = Some(d);
        l.from_general = true;
    }
    for k in recorded(db)? {
        if !l.valid.contains(&k) {
            l.valid.push(k);
        }
    }
    let stored: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META_OWN_KEY], |f| f.get(0)).optional()?;
    let mut own = stored.as_deref().and_then(from_hex).filter(|k| k.len() == 32);
    if own.is_none() && create && l.sign.is_none() {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META_OWN_KEY, to_hex(&b)])?;
        own = Some(b.to_vec());
    }
    if let Some(p) = own {
        if l.sign.is_none() {
            l.sign = Some(p.clone());
        }
        l.valid.push(p);
    }
    Ok(l)
}

/// After changing the general repository's keys (see above): records the seal keys derived from it
/// ([`record_keys`]), removes the other-person mark ([`FOREIGN_SEAL_PREFIX`]) from seals whose signature now
/// holds with one of this database's keys, and re-signs with the general's current key the seals this
/// database considers its own that were signed with another. Re-signing changes the memory's
/// timestamp: the next sync carries it. Returns how many marks it removed and how many seals it signed.
pub fn after_key_change(db: &Connection) -> Result<(usize, usize)> {
    record_keys(db)?;
    let l = seal_keys(db, false)?;
    let tx = db.unchecked_transaction()?;
    let seal_of = |id: &str| -> Result<Option<(serde_json::Value, Seal)>> {
        let extra: Option<String> = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
        let v = extra.and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok()).and_then(|mut e| e.get_mut(VERIFIED_KEY).map(serde_json::Value::take));
        Ok(v.and_then(|v| serde_json::from_value::<Seal>(v.clone()).ok().map(|s| (v, s))))
    };
    // Other-person marks that no longer apply: their signature holds with the current keys.
    let marks: Vec<(String, String)> = {
        let mut q = tx.prepare("SELECT k, v FROM meta WHERE k LIKE 'sello_ajeno:%'")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut removed = 0;
    let mut foreign = std::collections::HashSet::new();
    for (k, mark) in marks {
        let id = k[FOREIGN_SEAL_PREFIX.len()..].to_string();
        let Some((v, s)) = seal_of(&id)? else { continue };
        if mark != format!("{}\n{}", s.by, s.at) {
            continue;
        }
        // Like `sync::foreign_seal`: one older than when the person removed it here doesn't come back.
        let unverified_after = unverified_at(&tx, &id)?.is_some_and(|d| d > s.at);
        if !unverified_after && is_own_seal(&l, &id, &v) {
            tx.execute("DELETE FROM meta WHERE k = ?1", [&k])?;
            removed += 1;
        } else {
            foreign.insert(id);
        }
    }
    // Their own, re-signed with the general's current key. Without a general, nothing: no other
    // machine has the own key, and those signed with a recorded one still count where they did.
    let mut signed = 0;
    if let (true, Some(k)) = (l.from_general, l.sign.as_ref()) {
        let rows: Vec<(String, String, String, String)> = {
            let mut q = tx.prepare("SELECT id, kind, title, content FROM memory WHERE instr(extra, '\"verified\"') > 0")?;
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let current = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, k);
        for (id, kind, title, content) in rows {
            if foreign.contains(&id) {
                continue;
            }
            let Some((v, mut s)) = seal_of(&id)? else { continue };
            // Only one still valid for its text, from this person and not yet with the current key.
            if s.sha != fingerprint(&kind, &title, &content) || !is_own_seal(&l, &id, &v) {
                continue;
            }
            if from_hex(&s.mac).is_some_and(|f| ring::hmac::verify(&current, mac_text(&id, &s).as_bytes(), &f).is_ok()) {
                continue;
            }
            s.mac = to_hex(&hmac(k, mac_text(&id, &s).as_bytes()));
            set_seal(&tx, &id, Some(serde_json::to_value(&s)?))?;
            signed += 1;
        }
    }
    tx.commit()?;
    Ok((removed, signed))
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// What the seal key signs: each field prefixed by its length.
fn mac_text(id: &str, s: &Seal) -> String {
    let mut t = String::from("omoikane-seal-1");
    for c in [id, s.by.as_str(), s.at.as_str(), s.sha.as_str()] {
        t += &format!("\n{}:{c}", c.len());
    }
    t
}

/// Whether the seal (the value of [`VERIFIED_KEY`] in `extra`) of memory `id` was put by this person: its
/// signature holds with one of their seal keys.
pub(crate) fn is_own_seal(l: &SealKeys, id: &str, v: &serde_json::Value) -> bool {
    let Ok(s) = serde_json::from_value::<Seal>(v.clone()) else { return false };
    let Some(f) = from_hex(&s.mac) else { return false };
    let text = mac_text(id, &s);
    l.valid.iter().any(|k| ring::hmac::verify(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, k), text.as_bytes(), &f).is_ok())
}

/// Re-signs a seal with this person's key (when making it theirs: `omoi import <file>
/// --trust-verified`). `false` if it isn't a seal or there is nothing to sign with.
pub(crate) fn reseal(l: &SealKeys, id: &str, v: &mut serde_json::Value) -> bool {
    let (Ok(mut s), Some(k)) = (serde_json::from_value::<Seal>(v.clone()), l.sign.as_ref()) else { return false };
    s.mac = to_hex(&hmac(k, mac_text(id, &s).as_bytes()));
    match serde_json::to_value(&s) {
        Ok(x) => {
            *v = x;
            true
        }
        Err(_) => false,
    }
}

/// The fingerprint of the verified text: kind, title and content (32 hex digits).
pub(crate) fn fingerprint(kind: &str, title: &str, content: &str) -> String {
    let mut h = Sha256::new();
    for part in [kind, title, content] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// The verification from its column in `memory::COLUMNS` (the seal's JSON), if its fingerprint is that of the current text.
pub(crate) fn from_column(json: Option<&str>, kind: &str, title: &str, content: &str) -> Option<Verified> {
    let s: Seal = serde_json::from_str(json?).ok()?;
    (!s.by.trim().is_empty() && s.sha == fingerprint(kind, title, content)).then_some(Verified { by: s.by, at: s.at })
}

/// Who verifies if not given (`--by`, or the web view): this machine's author for that memory's
/// scope (`OMOIKANE_AUTHOR` or the name used to join sync), and failing that, the system user.
pub fn default_verifier(db: &Connection, m: &Memory) -> Result<String> {
    verifier_here(db, &m.space)
}

/// [`default_verifier`] for a space's content (its key; empty, no space).
pub(crate) fn verifier_here(db: &Connection, space: &str) -> Result<String> {
    let a = crate::memory::author_in(db, space)?;
    if !a.trim().is_empty() {
        return Ok(a);
    }
    Ok(["USERNAME", "USER"].iter().filter_map(|v| std::env::var(v).ok()).map(|v| v.trim().to_string()).find(|v| !v.is_empty()).unwrap_or_default())
}

/// The variables a coding agent's shell sets: inside it, `omoi verify` refuses (verification
/// belongs to the person). It's a brake so an agent doesn't use it by accident or because a memory
/// asks it to, not a barrier against someone who wants to bypass it.
///
/// Only those the agent sets when launching its commands (checked in its code or docs, or in a real
/// Claude Code shell; the `detect-coding-agent` table lists them with their source), never the
/// configuration ones the person sets in their own terminal: not `CLAUDE_CODE_*` in general
/// (`CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS`… go in the person's profile), not
/// `AIDER_*` (Aider sets none when running: they're all its configuration), not `CURSOR_TRACE_ID` or
/// `TERM_PROGRAM` (also present in the person's terminal inside the editor). With one of those,
/// `omoi verify` would refuse the person too.
pub const AGENT_ENV_VARS: &[&str] = &[
    // Generic (agents.md): set by Claude Code, Copilot in VS Code, Devin, OpenCode, Crush, Amp,
    // Goose…
    "AI_AGENT",
    "AGENT",
    // Claude Code (and Claude Cowork).
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_IS_COWORK",
    // Gemini CLI, Qwen Code, Antigravity.
    "GEMINI_CLI",
    "QWEN_CODE",
    "ANTIGRAVITY_AGENT",
    // Codex.
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_CI",
    // Cursor (editor agent and CLI).
    "CURSOR_AGENT",
    // OpenCode and Kilo Code; Crush; Amp; Goose.
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    "KILO",
    "CRUSH",
    "AMP_CURRENT_THREAD_ID",
    "GOOSE_TERMINAL",
    // Cline and Roo Code.
    "CLINE_ACTIVE",
    "ROO_ACTIVE",
    // GitHub Copilot: in VS Code, its CLI and its cloud agent.
    "COPILOT_AGENT",
    "COPILOT_CLI",
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_AGENT_JOB_ID",
    // Windsurf (Cascade), Augment, Pi, Junie, Kiro, Trae.
    "WINDSURF_CASCADE_TERMINAL",
    "AUGMENT_AGENT",
    "PI_CODING_AGENT",
    "JUNIE_DATA",
    "KIRO_AGENT_PATH",
    "TRAE_AI_SHELL_ID",
];

/// The agent variable present in the environment, if any.
pub fn agent_shell() -> Option<&'static str> {
    AGENT_ENV_VARS.iter().copied().find(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

/// Marks memory `id` (full) as verified by `by`. Creates no version and doesn't change its date (like
/// pinning it); changes its timestamp so sync carries it. Returns the memory.
pub fn verify(db: &mut Connection, id: &str, by: &str) -> Result<Memory> {
    Ok(verify_at(db, id, by, None)?.0)
}

/// Like [`verify`], with the date it was verified if known (an ISO 8601 instant, the `at` of an
/// OKF `verified` when importing it; if not understood, now). If it already has that same seal and
/// it holds, it doesn't put it again (doesn't change its timestamp): returns the memory and whether it put it.
pub fn verify_at(db: &mut Connection, id: &str, by: &str, at: Option<&str>) -> Result<(Memory, bool)> {
    let by = by.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!by.is_empty(), tr!("who verifies it? Give --by <name>", "¿quién la verifica? Di --by <nombre>"));
    ensure!(by.chars().count() <= 100, tr!("--by: up to 100 characters", "--by: hasta 100 caracteres"));
    let tx = crate::memory::write_tx(db)?;
    let (kind, title, content): (String, String, String) =
        tx.query_row("SELECT kind, title, content FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    // The given date, in Omoikane's form (`…T…:…:….mmmZ`, in UTC); if not understood, now.
    let given: Option<String> = match at.map(|a| a.trim().trim_end_matches('Z').replace('T', " ")).filter(|a| !a.is_empty()) {
        Some(a) => tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1)", [a], |f| f.get(0))?,
        None => None,
    };
    // A date more than a day in the future is not when it was verified (a wrong clock, or a bundle
    // that wants to stay ahead of any change): not accepted.
    if let Some(a) = &given {
        let future: bool = tx.query_row("SELECT ?1 > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day')", [a], |f| f.get(0))?;
        ensure!(!future, tr!("the verification date {a} is in the future", "la fecha de verificación {a} es del futuro"));
    }
    let at = match given {
        Some(a) => a,
        None => tx.query_row(&format!("SELECT {NOW}"), [], |f| f.get(0))?,
    };
    let before = crate::memory::read(&tx, id)?;
    if before.verified.as_ref().is_some_and(|v| v.by == by && v.at == at) {
        return Ok((before, false));
    }
    let mut seal = Seal { by, at, sha: fingerprint(&kind, &title, &content), mac: String::new() };
    // Signed with this person's seal key (see `SealKeys`): so it counts as theirs on their other
    // machines even if it arrives through a space's repository.
    if let Some(k) = seal_keys(&tx, true)?.sign {
        seal.mac = to_hex(&hmac(&k, mac_text(id, &seal).as_bytes()));
    }
    set_seal(&tx, id, Some(serde_json::to_value(seal)?))?;
    // If it was marked as another person's (`FOREIGN_SEAL_PREFIX`), no longer: now it's theirs.
    tx.execute("DELETE FROM meta WHERE k = ?1", [format!("{FOREIGN_SEAL_PREFIX}{id}")])?;
    let m = crate::memory::read(&tx, id)?;
    tx.commit()?;
    Ok((m, true))
}

/// Removes its verification and records when ([`UNVERIFIED_AT_KEY`]): reimporting a bundle exported
/// earlier doesn't verify it again. `false` if it had none.
pub fn unverify(db: &mut Connection, id: &str) -> Result<bool> {
    let tx = crate::memory::write_tx(db)?;
    let had = set_seal(&tx, id, None)?;
    if had {
        let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
        let now: String = tx.query_row(&format!("SELECT {NOW}"), [], |f| f.get(0))?;
        o.insert(UNVERIFIED_AT_KEY.into(), now.into());
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    }
    tx.commit()?;
    Ok(had)
}

/// When the person last removed its verification ([`unverify`]), if known.
pub(crate) fn unverified_at(db: &Connection, id: &str) -> Result<Option<String>> {
    let extra: Option<String> = db.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    Ok(extra
        .filter(|e| e.contains(UNVERIFIED_AT_KEY))
        .and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok())
        .and_then(|v| v.get(UNVERIFIED_AT_KEY).and_then(|x| x.as_str()).map(str::to_string)))
}

/// Puts or removes the seal with a new timestamp. `false` if there was nothing to remove.
fn set_seal(tx: &Connection, id: &str, seal: Option<serde_json::Value>) -> Result<bool> {
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    let changed = match seal {
        Some(s) => {
            o.insert(VERIFIED_KEY.into(), s);
            true
        }
        None => o.remove(VERIFIED_KEY).is_some(),
    };
    if !changed {
        return Ok(false);
    }
    let hlc = crate::clock::stamp_after(tx, &crate::memory::hlc_of(tx, id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    Ok(true)
}

/// When the text is changed from here (`memory::update_in`, which already sets the timestamp):
/// removes the verification, records when ([`UNVERIFIED_AT_KEY`], like [`unverify`]) and returns the
/// one it had, if it held. Without the date, the old seal would come back with a newer line carrying
/// the old text (another machine of the person that hasn't seen the change yet, `import <file>`, an
/// OKF bundle): with the old text, its fingerprint matches again.
pub(crate) fn remove_on_change(tx: &Transaction, id: &str, before: &Memory) -> Result<Option<Verified>> {
    let extra: Option<String> = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    let Some(extra) = extra.filter(|e| e.contains("\"verified\"")) else { return Ok(None) };
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.remove(VERIFIED_KEY).is_none() {
        return Ok(None);
    }
    let now: String = tx.query_row(&format!("SELECT {NOW}"), [], |f| f.get(0))?;
    o.insert(UNVERIFIED_AT_KEY.into(), now.into());
    tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    Ok(before.verified.clone())
}

/// What arrives in a newer line (sync, `import <file>`) for a memory whose verification was removed
/// here at `unverified_at` ([`UNVERIFIED_AT_KEY`]): the date is kept (the more recent of the two, for
/// next time) and, if `remove_seal`, a seal older than that date is removed. (Through a space's
/// repository the seal stays in the line for the others and doesn't count here: see
/// `sync::foreign_seal`.)
pub(crate) fn drop_old_seal(extra: &mut serde_json::Map<String, serde_json::Value>, unverified_at: &str, remove_seal: bool) {
    if extra.get(UNVERIFIED_AT_KEY).and_then(|x| x.as_str()).is_none_or(|d| d < unverified_at) {
        extra.insert(UNVERIFIED_AT_KEY.into(), unverified_at.into());
    }
    let old = extra.get(VERIFIED_KEY).and_then(|v| v.get("at")).and_then(|x| x.as_str()).is_some_and(|at| at < unverified_at);
    if old && remove_seal {
        extra.remove(VERIFIED_KEY);
    }
}

/// What is shown on a line: "verified by Marc, 2026-11-08".
pub fn text(v: &Verified) -> String {
    format!("verified by {}, {}", v.by, v.at.get(..10).unwrap_or(&v.at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_with_text() {
        let a = fingerprint("decision", "T", "C");
        assert_eq!(a.len(), 32);
        assert_ne!(a, fingerprint("fact", "T", "C"));
        assert_ne!(a, fingerprint("decision", "T", "C."));
        // The separator: "ab"+"c" is not "a"+"bc".
        assert_ne!(fingerprint("x", "ab", "c"), fingerprint("x", "a", "bc"));
        let s = serde_json::json!({ "by": "Marc", "at": "2026-11-08T10:00:00.000Z", "sha": a }).to_string();
        assert_eq!(from_column(Some(&s), "decision", "T", "C").unwrap().by, "Marc");
        assert!(from_column(Some(&s), "decision", "T", "otro").is_none());
        assert!(from_column(Some(r#"{"by":"Marc","at":"x"}"#), "decision", "T", "C").is_none());
        assert!(from_column(None, "decision", "T", "C").is_none());
    }
}
