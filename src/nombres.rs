//! Project names that look alike: the same comparison for whoever types a name
//! (`proyecto::buscar`), for the review (duplicate projects) and for recall (a name misheard
//! in dictation in the message).
//!
//! Two levels. **Equal**: the same name ignoring case, accents, hyphens, underscores, spaces
//! and dots (`social-publish` = `Social_Publish` = `social publish`, `Crónicas` = `cronicas`).
//! **One letter apart**: one letter changed, added or missing, with `MIN_LETTERS` letters or more
//! (`Omoikane`/`Omoikae`). The second is less certain: the review only uses it together with another signal, and
//! never a bare resemblance (`Tokimichi` and `tokimichi-web` are different projects).

use crate::memoria::Scope;
use crate::proyecto::{self, Proyecto};
use crate::tr;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

/// From how many letters (of the shorter name, normalized) the one-letter resemblance counts.
pub const MIN_LETTERS: usize = 6;

/// In recall, a word that is in no memory can be a misdictated name from 6
/// letters (the word and the name): not existing already is the other signal. With 5, normal words that did not
/// appear in any memory became projects («tropa» → Troba, «rodeo» and «rodes» → Rodet, «balsa»
/// → Balua): checked with `examples/eval.rs` on `tests/recall_casos.tsv`.
pub const MIN_LETTERS_RECALL: usize = 6;

/// From how many letters the review looks for the name of a folder that no longer exists in the
/// memories of another project (`Narrador` → `Aloud`).
const MIN_LETTERS_CONTINUED: usize = 5;

/// Inflection endings (plural, infinitive, participle…) in Spanish and English: a word that
/// only differs from a name in its last two letters and ends like this is a normal word with its
/// ending, not a misdictated name («faltas» is not Faltis; «tokimicho» is Tokimichi).
const INFLECTIONS: &[&str] = &["s", "ar", "er", "ir", "an", "en", "ed"];

/// Whether `t` (normalized) differs from `n` only in its last two letters and ends in an inflection.
fn is_inflection(t: &str, n: &str) -> bool {
    let common = t.chars().zip(n.chars()).take_while(|(a, b)| a == b).count();
    common + 2 >= t.chars().count() && INFLECTIONS.iter().any(|f| t.ends_with(f))
}

/// The SQL condition on `p` (a project) for the projects visible from a scope: from a
/// project or a space, those of its space (or those without one); from a general folder, all.
fn visible_projects(scope: &Scope) -> String {
    match scope {
        Scope::All => "1".to_string(),
        a => match a.space() {
            Some(s) => format!("p.space_id = {s}"),
            None => "p.space_id IS NULL".to_string(),
        },
    }
}

/// How two names resemble each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Similarity {
    /// The same name, normalized.
    Same,
    /// One letter apart (changed, added or missing).
    OneLetter,
}

/// A letter without its accent (lowercase).
fn strip_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    }
}

/// A text without accents (letter by letter, like [`normal`] but without removing separators): to look for a
/// name as a word in a text.
pub fn fold_accents(text: &str) -> String {
    text.chars().map(|c| c.to_lowercase().next().map_or(c, strip_accent)).collect()
}

/// The name for comparing: lowercase, without accents and without `-`, `_`, spaces or dots.
pub fn normal(name: &str) -> String {
    name.trim().to_lowercase().chars().filter(|c| !matches!(c, '-' | '_' | '.') && !c.is_whitespace()).map(strip_accent).collect()
}

/// Whether two texts (already normalized) are exactly one letter apart: one changed, one added or one
/// missing. Equal ones don't count.
pub fn one_letter_apart_normalized(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (shorter, longer) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    match longer.len() - shorter.len() {
        0 => shorter.iter().zip(longer.iter()).filter(|(x, y)| x != y).count() == 1,
        1 => {
            // The first difference is the extra letter: the rest has to match.
            let i = shorter.iter().zip(longer.iter()).position(|(x, y)| x != y).unwrap_or(shorter.len());
            shorter[i..] == longer[i + 1..]
        }
        _ => false,
    }
}

/// How two project names resemble each other (`None`: not at all).
pub fn similarity(a: &str, b: &str) -> Option<Similarity> {
    let (na, nb) = (normal(a), normal(b));
    if na.is_empty() || nb.is_empty() {
        return None;
    }
    if na == nb {
        return Some(Similarity::Same);
    }
    (na.chars().count().min(nb.chars().count()) >= MIN_LETTERS && one_letter_apart_normalized(&na, &nb)).then_some(Similarity::OneLetter)
}

// --- Recall ----------------------------------------------------------------------------------

/// The names of the projects with memories visible from `scope`, normalized, with the words
/// of each one (lowercase and without accents, like the recall terms).
fn names_with_memories(db: &Connection, scope: &Scope) -> Result<Vec<(String, Vec<String>)>> {
    let mut q = db.prepare(&format!(
        "SELECT name FROM project p WHERE p.id != 0 AND {} AND EXISTS (SELECT 1 FROM memory m WHERE m.project_id = p.id)",
        visible_projects(scope)
    ))?;
    let names: Vec<String> = q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut v: Vec<(String, Vec<String>)> = names
        .iter()
        .map(|n| {
            let parts = n
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|p| !p.is_empty())
                .map(|p| p.chars().map(strip_accent).collect())
                .collect();
            (normal(n), parts)
        })
        .filter(|(n, _)| !n.is_empty())
        .collect();
    v.sort();
    v.dedup();
    Ok(v)
}

/// A term in the message that is a misspelled project name: the words of the right name.
/// Only terms that are in no memory are looked at (`in_none[i]`), of `MIN_LETTERS_RECALL`
/// letters or more, one letter apart from a single project of also `MIN_LETTERS_RECALL` letters visible from
/// `scope` (never another client's), without being an inflection ([`is_inflection`]); or equal when normalized to
/// one written with separators («socialpublish» → «social publish»). Returns (index of the
/// term, words).
pub fn misspelled(db: &Connection, scope: &Scope, terms: &[String], in_none: &[bool]) -> Result<Vec<(usize, Vec<String>)>> {
    let doubtful: Vec<usize> = (0..terms.len()).filter(|&i| in_none.get(i).copied().unwrap_or(false) && terms[i].chars().count() >= MIN_LETTERS_RECALL).collect();
    if doubtful.is_empty() {
        return Ok(Vec::new());
    }
    let names = names_with_memories(db, scope)?;
    let mut r = Vec::new();
    for i in doubtful {
        let t = normal(&terms[i]);
        let matching: Vec<&(String, Vec<String>)> = names
            .iter()
            .filter(|(n, parts)| {
                (*n == t && parts.len() > 1) || (n.chars().count() >= MIN_LETTERS_RECALL && one_letter_apart_normalized(n, &t) && !is_inflection(&t, n))
            })
            .collect();
        if let [(_, parts)] = matching.as_slice() {
            r.push((i, parts.clone()));
        }
    }
    Ok(r)
}

// --- Review: duplicate projects ----------------------------------------------------------

/// The same normalized name.
pub const SAME_NAME: &str = "same_name";
/// One letter apart, with another signal.
pub const ONE_LETTER: &str = "one_letter";
/// A folder that no longer exists and another project that continues it.
pub const CONTINUED: &str = "continued";
/// A «project» in a temporary or uploads folder.
pub const TEMP_FOLDER: &str = "temp_folder";

/// A project that has «very few» memories.
const FEW: i64 = 3;

/// A review proposal about projects: merge two, or take the memories out of a folder
/// that is not a project. It is never done on its own: the agent proposes it with the command already written.
#[derive(Clone, Debug, Serialize)]
pub struct Proposal {
    pub kind: &'static str,
    /// The project that would disappear when merging (or the one in the temporary folder).
    pub project: String,
    pub path: String,
    pub memories: i64,
    /// The one that would remain (empty for a temporary folder).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub other: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub other_path: String,
    /// Why, in English and short.
    pub detail: String,
    /// The command that would do it (if the user says yes).
    pub command: String,
    /// The command so it is not proposed again.
    pub dismiss: String,
}

/// Where the pairs the user said are different are recorded (`omoi projects distinct`):
/// a JSON with «keyA|keyB» (sorted; keyB empty: the temporary folder is a real project).
const DISTINCT_KEY: &str = "proyectos_distintos";

fn pair_key(a: &str, b: &str) -> String {
    if b.is_empty() || a <= b { format!("{a}|{b}") } else { format!("{b}|{a}") }
}

fn distinct_pairs(db: &Connection) -> Result<BTreeSet<String>> {
    let v: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [DISTINCT_KEY], |f| f.get(0)).optional()?;
    Ok(v.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// `omoi projects distinct A [B]`: the review stops proposing to merge A and B (or, without B, treating
/// A's temporary folder as if it were not a project). It belongs to this computer: it does not travel.
pub fn mark_distinct(db: &Connection, a: i64, b: Option<i64>) -> Result<()> {
    let ka = proyecto::clave_de(db, a)?;
    anyhow::ensure!(!ka.is_empty(), tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    let kb = match b {
        Some(b) => {
            anyhow::ensure!(b != a, tr!("it's the same project", "es el mismo proyecto"));
            proyecto::clave_de(db, b)?
        }
        None => String::new(),
    };
    let mut v = distinct_pairs(db)?;
    v.insert(pair_key(&ka, &kb));
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![DISTINCT_KEY, serde_json::to_string(&v)?])?;
    Ok(())
}

/// Whether the folder is temporary or an uploads folder: `.claude/uploads/<…>`, the system temp folder
/// (`%TEMP%`, `$TMPDIR`), `AppData\Local\Temp` or `/tmp`.
pub fn is_temp_folder(path: &str) -> bool {
    if path.is_empty() || path.starts_with('[') {
        return false;
    }
    let r = path.replace('\\', "/").to_lowercase();
    if r.contains("/.claude/uploads/") || r.contains("/appdata/local/temp/") || r.starts_with("/tmp/") || r.starts_with("/var/tmp/") {
        return true;
    }
    // The temp folder as is and in canonical form (with `TEMP` as a short name, `CLAUDE~1`, project
    // paths arrive in the long form).
    let tmp = std::env::temp_dir();
    let canonical = proyecto::canonica(&tmp).map(|p| proyecto::texto(&p)).unwrap_or_default();
    [tmp.to_string_lossy().into_owned(), canonical].iter().any(|t| {
        let t = t.replace('\\', "/").to_lowercase();
        let t = t.trim_end_matches('/');
        !t.is_empty() && r.starts_with(&format!("{t}/"))
    })
}

/// How a project is named in a command: its name if `proyecto::buscar` finds only it,
/// and otherwise its folder (in quotes if it has spaces).
fn in_command(db: &Connection, p: &Proyecto) -> String {
    let t = if proyecto::buscar(db, &p.name).ok() == Some(p.id) { p.name.clone() } else { p.path.clone() };
    if t.contains([' ', '\'']) || t.is_empty() { format!("\"{t}\"") } else { t }
}

/// A project's git `origin`: the one in its `git:` key, or the one of any of its local folders.
fn origin_of(db: &Connection, p: &Proyecto) -> Result<String> {
    if let Some(o) = p.key.strip_prefix("git:") {
        return Ok(o.to_string());
    }
    Ok(db
        .query_row("SELECT origin FROM project_path WHERE project_id = ?1 AND is_foreign = 0 AND origin != '' LIMIT 1", [p.id], |f| f.get(0))
        .optional()?
        .unwrap_or_default())
}

/// When each project was created.
fn creation_dates(db: &Connection) -> Result<HashMap<i64, String>> {
    let mut q = db.prepare("SELECT id, created_at FROM project WHERE id != 0")?;
    Ok(q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
}

/// How many memories of each project mention the word (via the text index).
fn mentions(db: &Connection, word: &str) -> Result<HashMap<i64, i64>> {
    let mut q = db.prepare("SELECT m.project_id, count(*) FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid WHERE memory_fts MATCH ?1 GROUP BY m.project_id")?;
    let query = format!("\"{}\"", word.replace('"', ""));
    Ok(q.query_map([query], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>().unwrap_or_default())
}

/// What the review proposes about the projects visible from a scope (those of its space; from
/// a general folder, all): merge duplicates and take memories out of temporary folders.
/// Signals, from most to least certain:
/// - the same normalized name, always (in the same space);
/// - one letter apart, only with another one: one of the folders no longer exists, one has very few memories and is
///   newer, or both have the same git `origin`;
/// - a folder that no longer exists and another, newer project whose memories name it (`Narrador` →
///   `Aloud`);
/// - a «project» in a temporary or uploads folder (`.claude\uploads\<uuid>`, `%TEMP%`).
///
/// No bare name resemblances. Only projects with memories; as temporary, only those that
/// have no `origin` (a `git:` key is a real repository, wherever it is).
pub fn proposals(db: &Connection, scope: &Scope) -> Result<Vec<Proposal>> {
    proposals_with(db, scope, &is_temp_folder)
}

/// `proposals` with the projects the caller already has: `proyecto::lista(db, None, space_filter(scope),
/// usize::MAX)` (`omoi review` lists them first, and each list walks the memory table).
pub fn proposals_for(db: &Connection, all: &[Proyecto]) -> Result<Vec<Proposal>> {
    proposals_among(db, all, &is_temp_folder)
}

/// The space filter with which `proposals` lists the projects of `scope`.
pub fn space_filter(scope: &Scope) -> Option<Option<i64>> {
    if matches!(scope, Scope::All) { None } else { Some(scope.space()) }
}

/// `proposals`, with another way of telling which folder is temporary (tests live in `%TEMP%`).
pub fn proposals_with(db: &Connection, scope: &Scope, is_temp: &dyn Fn(&str) -> bool) -> Result<Vec<Proposal>> {
    proposals_among(db, &proyecto::lista(db, None, space_filter(scope), usize::MAX)?, is_temp)
}

fn proposals_among(db: &Connection, all: &[Proyecto], is_temp: &dyn Fn(&str) -> bool) -> Result<Vec<Proposal>> {
    let dismissed = distinct_pairs(db)?;
    let created = creation_dates(db)?;
    let space: HashMap<i64, Option<i64>> = all.iter().map(|p| Ok((p.id, proyecto::espacio_de(db, p.id)?))).collect::<Result<_>>()?;
    let mut r: Vec<Proposal> = Vec::new();
    let mut seen: BTreeSet<(i64, i64)> = BTreeSet::new();
    let temporary: BTreeSet<i64> = all.iter().filter(|p| p.key.starts_with("dir:") && is_temp(&p.path)).map(|p| p.id).collect();
    let mut propose = |db: &Connection, kind: &'static str, from: &Proyecto, into: &Proyecto, detail: String, r: &mut Vec<Proposal>| {
        let pair = (from.id.min(into.id), from.id.max(into.id));
        if !seen.insert(pair) || dismissed.contains(&pair_key(&from.key, &into.key)) {
            return;
        }
        let (a, b) = (in_command(db, from), in_command(db, into));
        r.push(Proposal {
            kind,
            project: from.name.clone(),
            path: from.path.clone(),
            memories: from.memories,
            other: into.name.clone(),
            other_path: into.path.clone(),
            detail,
            command: format!("omoi projects merge {a} {b}"),
            dismiss: format!("omoi projects distinct {a} {b}"),
        });
    };
    // Which one disappears when merging: the one that is gone, otherwise the one with fewer memories, otherwise the newer one.
    let order = |a: &Proyecto, b: &Proyecto| -> bool {
        let key = |p: &Proyecto| (!p.gone, p.memories, std::cmp::Reverse(created.get(&p.id).cloned().unwrap_or_default()));
        key(a) < key(b)
    };
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            if temporary.contains(&a.id) || temporary.contains(&b.id) || space.get(&a.id) != space.get(&b.id) {
                continue;
            }
            let (from, into) = if order(a, b) { (a, b) } else { (b, a) };
            match similarity(&a.name, &b.name) {
                Some(Similarity::Same) => {
                    propose(db, SAME_NAME, from, into, "same name (ignoring case, accents and separators)".into(), &mut r);
                }
                Some(Similarity::OneLetter) => {
                    let (oa, ob) = (origin_of(db, a)?, origin_of(db, b)?);
                    let new_and_small = |x: &Proyecto, y: &Proyecto| x.memories <= FEW && x.memories < y.memories && created.get(&x.id) > created.get(&y.id);
                    let signal = if a.gone || b.gone {
                        Some("one letter apart, and one folder no longer exists".to_string())
                    } else if !oa.is_empty() && oa == ob {
                        Some(format!("one letter apart, and the same git origin ({oa})"))
                    } else if new_and_small(a, b) || new_and_small(b, a) {
                        Some("one letter apart, and the newer one has very few memories".to_string())
                    } else {
                        None
                    };
                    if let Some(s) = signal {
                        propose(db, ONE_LETTER, from, into, s, &mut r);
                    }
                }
                None => {}
            }
        }
    }
    // A folder that no longer exists, continued by another, newer project that names it.
    for old in all.iter().filter(|p| p.gone && !temporary.contains(&p.id)) {
        let word = normal(&old.name);
        if word.chars().count() < MIN_LETTERS_CONTINUED || !word.chars().all(char::is_alphanumeric) {
            continue;
        }
        let naming = mentions(db, &word)?;
        let candidates: Vec<&Proyecto> = all
            .iter()
            .filter(|p| {
                p.id != old.id
                    && !p.gone
                    && !temporary.contains(&p.id)
                    && space.get(&p.id) == space.get(&old.id)
                    && naming.get(&p.id).copied().unwrap_or(0) > 0
                    && created.get(&p.id) > created.get(&old.id)
            })
            .collect();
        if let [new] = candidates.as_slice() {
            propose(db, CONTINUED, old, new, format!("its folder no longer exists, and {} (newer) mentions it", new.name), &mut r);
        }
    }
    // Temporary or uploads folders.
    for p in all.iter().filter(|p| temporary.contains(&p.id)) {
        if dismissed.contains(&pair_key(&p.key, "")) {
            continue;
        }
        let a = in_command(db, p);
        r.push(Proposal {
            kind: TEMP_FOLDER,
            project: p.name.clone(),
            path: p.path.clone(),
            memories: p.memories,
            other: String::new(),
            other_path: String::new(),
            detail: format!(
                "a temporary or uploads folder, not a project: make its {} global (`omoi update <id> --global`) or move them to their project (`omoi projects merge {a} <project>`)",
                if p.memories == 1 { "memory".to_string() } else { format!("{} memories", p.memories) }
            ),
            command: format!("omoi recent --project {a}"),
            dismiss: format!("omoi projects distinct {a}"),
        });
    }
    Ok(r)
}

/// Where the last projects notice of each scope is recorded: `day`.
fn notice_key(scope: &Scope) -> String {
    let e = match scope {
        Scope::All => "all".to_string(),
        a => format!("{:?}", a.space()),
    };
    format!("revision_proyectos:{e}")
}

/// At session start, once a day per scope: the paragraph for the agent with what is proposed
/// about projects (`None` if there is nothing or it already came out today). It is computed only once a day.
pub fn start_notice(db: &Connection, scope: &Scope) -> Result<Option<String>> {
    let today: String = db.query_row("SELECT date('now', 'localtime')", [], |f| f.get(0))?;
    let k = notice_key(scope);
    let before: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [&k], |f| f.get(0)).optional()?;
    if before.as_deref() == Some(today.as_str()) {
        return Ok(None);
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![k, today])?;
    let v = proposals(db, scope)?;
    Ok((!v.is_empty()).then(|| notice_text(&v)))
}

/// The paragraph for the agent, in English like the rest of recall.
pub fn notice_text(v: &[Proposal]) -> String {
    let mut s = String::from(
        "Projects review: these look like duplicates or not real projects. Ask the user, one by one, with the command already written; run it only if they say yes (a merge reaches every synced computer). If they say no, run the dismiss command.\n",
    );
    for p in v {
        let who = if p.other.is_empty() { format!("{} ({})", p.project, p.path) } else { format!("{} ({}) → {} ({})", p.project, p.path, p.other, p.other_path) };
        let (yes, no) = if p.other.is_empty() { ("its memories", "if it is a real project") } else { ("merge", "if not") };
        s.push_str(&format!("- {who}: {}. {yes}: `{}` · {no}: `{}`\n", p.detail, p.command, p.dismiss));
    }
    s.pop();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_normalized_names() {
        assert_eq!(normal("Social Publish"), "socialpublish");
        assert_eq!(similarity("social-publish", "Social_Publish"), Some(Similarity::Same));
        assert_eq!(similarity("Crónicas", "cronicas"), Some(Similarity::Same));
        assert_eq!(similarity("marcmayol.github.io", "marcmayol-github-io"), Some(Similarity::Same));
        assert_eq!(similarity("Omoikane", "omoikane"), Some(Similarity::Same));
    }

    #[test]
    fn one_letter_apart() {
        assert_eq!(similarity("Omoikane", "Omoikae"), Some(Similarity::OneLetter), "una de menos");
        assert_eq!(similarity("Omoikane", "Onmoikane"), Some(Similarity::OneLetter), "una de más");
        assert_eq!(similarity("Tokimichi", "tokimicho"), Some(Similarity::OneLetter), "una cambiada");
        assert_eq!(similarity("Troba", "trova"), None, "con menos de 6 letras, no");
        assert_eq!(similarity("Tokimichi", "tokimichi-web"), None, "un nombre dentro de otro no es parecido");
        assert_eq!(similarity("Omoikane", "Omokiane"), None, "dos letras cambiadas de sitio son dos");
        assert!(!one_letter_apart_normalized("abc", "abc"));
        assert!(one_letter_apart_normalized("abcdef", "abcdefg"));
        assert!(one_letter_apart_normalized("xabcdef", "abcdef"));
    }

    #[test]
    fn temporary_folders() {
        assert!(is_temp_folder(r"C:\Users\x\.claude\uploads\d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39"));
        assert!(is_temp_folder(r"C:\Users\x\AppData\Local\Temp\tmp.abc"));
        assert!(is_temp_folder("/tmp/x"));
        assert!(!is_temp_folder(r"D:\Omoikane"));
        assert!(!is_temp_folder(""));
        let t = std::env::temp_dir().join("algo");
        assert!(is_temp_folder(&t.to_string_lossy()));
        // And as a project's path arrives: canonical (on Windows, with the long name).
        let d = crate::comun::Temporal::nueva("temporal");
        assert!(is_temp_folder(&proyecto::de(&d).unwrap()));
    }
}
