//! Export and import in OKF v0.2 (1.8.5): Google's Open Knowledge Format
//! (<https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md>), the one used by
//! okf-agent-memory. A bundle is a folder of Markdown with a YAML header: one file per concept
//! (`type` is the only required field), `index.md` in the folders (the root one with
//! `okf_version: "0.2"`) and a `log.md` with the changes by date.
//!
//! - `omoi export --okf <folder>`: one concept per memory in `<group>/<kind>/<title>.md`, with the
//!   groups and the scope of the Obsidian export (the personal and the global, or a space without the global;
//!   never two spaces in the same bundle). It's a mirror: it only writes what changes and only deletes
//!   its own (the concepts with `omoikane:` in the header and the indexes with the end marker). A
//!   folder with someone else's bundle (an `index.md` or `log.md` without the marker) is left alone.
//! - `omoi import --okf <folder>`: each concept, a memory saved by key (`remember --key`):
//!   the one the concept carries if it came from Omoikane, `okf:<its id>` if not. Importing twice doesn't
//!   duplicate. It goes to the folder's project (or `--dir`; this database's global, to the global), with
//!   `--space` to that space and with `--global` to the global. Only what this database signed (`signature`)
//!   comes in global, pinned or verified; what was forgotten here doesn't come back. Title, content and data go through secret redaction, like any `remember`.
//!
//! How the fields map (both ways):
//!
//! | Omoikane | OKF |
//! |---|---|
//! | `kind` | `type`: `Fact`, `Decision`, `Gotcha`, `Preference`, `Procedure` (on import, other people's by their name: `Runbook` → procedure, `Convention` → preference…; anything else, fact, and the original `type` is kept) |
//! | title and content | `title`, the body; `description` is the first sentence |
//! | `tags` | `tags` |
//! | `paths` | `code_refs` |
//! | `review:YYYY-MM-DD` | `stale_after: YYYY-MM-DDT00:00:00Z` |
//! | verified by a person (the stamp of `omoi verify`, `verified`) | `verified: [{ by: human:<who>, at }]`, with the fingerprint of the text in `omoikane.verified_sha` (on import, a person's `verified` sets the stamp with its date if the concept carries this database's signature and the text is the verified one, or with `--trust-verified`; never inside an agent's shell; a `verified` tag is just another tag) |
//! | superseded by another (current verdict) | `status: deprecated` and the link to the one replacing it; on import, the verdict comes back |
//! | tags `draft` and `deprecated` | `status: draft` / `status: deprecated` |
//! | `source` | `generated.by` (`<agent>/omoikane`), with `generated.at` = its last change |
//! | the provenance (commit, branch, session, trace) | `sources` (1.10.1, ids `omoikane-*`, rebuilt on every export) and exact in `omoikane.provenance` |
//! | what it is based on (`evidence`, 1.13) | `omoikane.evidence` |
//! | what to check when the review date arrives (1.14) | `omoikane.review_reason` |
//! | the rest (id, key, pinned, dates, author, data) | `omoikane:` (one more key, which OKF says to keep) |
//!
//! What someone else's concept carries and Omoikane doesn't have (`sources`, `resource`, `generated`, the
//! process `verified` entries, `governance`…) is kept in the memory's data (`data.okf`) and comes
//! out the same on export.

use crate::embed::Model;
use crate::memory::{self, Scope, Changes, Memory, NewMemory, Kind};
use crate::obsidian::{ExportScope, short_id};
use crate::{tr, verdicts};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The OKF version that is written.
pub const VERSION: &str = "0.2";
const DRAFT: &str = "draft";
const DEPRECATED: &str = "deprecated";
/// Where the OKF fields Omoikane doesn't have go, inside a memory's data.
const OKF_DATA_KEY: &str = "okf";
/// The marker of the indexes and the log Omoikane writes (what doesn't carry it is left alone).
const MARKER: &str = "<!-- omoikane: rewritten on every export -->";
/// What goes after a concept's content in its body (the links to others) and is not its content.
const FOOTER: &str = "<!-- omoikane: links -->";
/// The overview concept of each group (`<group>/overview.md`).
const OVERVIEW: &str = "overview";
/// The header keys that become memory fields.
const MAPPED_KEYS: [&str; 8] = ["type", "title", "description", "tags", "status", "stale_after", "code_refs", "omoikane"];

// --- Export ------------------------------------------------------------------------------------------

/// What an export did (counting only concepts; the indexes and the log are separate).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ExportCount {
    pub memories: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    /// The overview concepts, one per group.
    pub hubs: usize,
}

/// "3 memories", "1 memory".
fn memory_count(n: usize) -> String {
    if n == 1 { tr!("1 memory", "1 memoria").to_string() } else { tr!("{n} memories", "{n} memorias") }
}

/// What a folder's `index.md` lists: its subfolders and its concepts (file, title,
/// description).
type FolderIndex = (BTreeSet<String>, Vec<(String, String, String)>);

struct Group {
    folder: String,
    title: String,
}

/// Exports to the bundle `dir` and returns what changed.
pub fn export(db: &Connection, dir: &Path, what: ExportScope) -> Result<ExportCount> {
    crate::project::require_local(dir)?;
    for f in ["index.md", "log.md"] {
        let p = dir.join(f);
        if p.exists() && !is_our_index(&p) {
            bail!(tr!(
                "{} is not from an Omoikane export: choose an empty folder (to bring that bundle in, use `omoi import --okf`)",
                "{} no es de una exportación de Omoikane: elige una carpeta vacía (para traer ese bundle, `omoi import --okf`)",
                p.display()
            ));
        }
    }
    let scope = match what {
        ExportScope::Personal => Scope::Space(None),
        ExportScope::Space(s) => Scope::Space(Some(s)),
    };
    let mut all_memories = memory::recent(db, &scope, None, usize::MAX >> 1)?;
    if matches!(what, ExportScope::Space(_)) {
        all_memories.retain(|m| !m.global());
    }
    all_memories.sort_by(|a, b| a.id.cmp(&b.id));
    std::fs::create_dir_all(dir).with_context(|| tr!("could not create {}", "no se pudo crear {}", dir.display()))?;
    let data = memory::data_of(db, &all_memories.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    // This database's signature: when imported here, what was signed is recognised as our own (see `signature`).
    let key = signature::key(db, true)?;

    // Each memory's group, as in the Obsidian export.
    let mut names: HashMap<String, BTreeSet<String>> = HashMap::new();
    for m in &all_memories {
        if m.project_id != 0 {
            names.entry(crate::project::name_of(&m.project, &m.project_key)).or_default().insert(m.project_key.clone());
        }
    }
    let group_of = |m: &Memory| -> Group {
        if m.project_id == 0 {
            return if m.space.is_empty() {
                Group { folder: "_global".into(), title: tr!("Global", "Global").to_string() }
            } else {
                Group { folder: format!("_{}", slug(&m.space, 40)), title: m.space.clone() }
            };
        }
        let name = crate::project::name_of(&m.project, &m.project_key);
        let mut s = slug(&name, 40);
        if names.get(&name).is_some_and(|k| k.len() > 1) {
            s = format!("{s}-{}", short_id(&m.project_key));
        }
        Group { folder: s, title: name }
    };

    // The path of each concept (its OKF id: without `.md`), unique ignoring case.
    // Each group has its overview concept (`<group>/overview`), which links to all its memories.
    let mut group_titles: BTreeMap<String, String> = BTreeMap::new();
    for m in &all_memories {
        let g = group_of(m);
        group_titles.insert(g.folder, g.title);
    }
    let mut taken: BTreeSet<String> = group_titles.keys().map(|g| format!("{g}/{OVERVIEW}").to_lowercase()).collect();
    let mut path: HashMap<String, String> = HashMap::new();
    for m in &all_memories {
        let g = group_of(m);
        let sub = memory::key_of(m).and_then(path_from_key).unwrap_or_else(|| format!("{}/{}", slug(m.kind_str(), 30), slug(&m.title, 60)));
        let mut r = format!("{}/{sub}", g.folder);
        let last = r.rsplit('/').next().unwrap_or("").to_string();
        if taken.contains(&r.to_lowercase()) || matches!(last.as_str(), "index" | "log") || is_reserved(&last) {
            r = format!("{r}-{}", short_id(&m.id));
        }
        taken.insert(r.to_lowercase());
        path.insert(m.id.clone(), r);
    }

    // Current verdicts among what is exported.
    let mut superseded_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut supersedes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut contradicts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in verdicts::pairs(db)?.into_values() {
        if !path.contains_key(&v.memory_id) || !path.contains_key(&v.other_id) {
            continue;
        }
        if v.relation == verdicts::SUPERSEDED {
            superseded_by.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            supersedes.entry(v.other_id).or_default().push(v.memory_id);
        } else if v.relation == verdicts::CONTRADICTS {
            contradicts.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            contradicts.entry(v.other_id).or_default().push(v.memory_id);
        }
    }
    for v in [&mut superseded_by, &mut supersedes, &mut contradicts] {
        v.values_mut().for_each(|x| {
            x.sort();
            x.dedup();
        });
    }

    let title_of: HashMap<&str, &str> = all_memories.iter().map(|m| (m.id.as_str(), m.title.as_str())).collect();
    let mut c = ExportCount { memories: all_memories.len(), ..ExportCount::default() };
    let mut written: BTreeSet<PathBuf> = BTreeSet::new();
    // What goes in each index: folder → its subfolders and its concepts.
    let mut folders: BTreeMap<String, FolderIndex> = BTreeMap::new();
    let mut log: Vec<(String, String, bool, String)> = Vec::new();
    // Each group's items, for its overview: (path, title, description, last change).
    let mut by_group: BTreeMap<String, Vec<(String, String, String, String)>> = BTreeMap::new();
    for m in &all_memories {
        let r = &path[&m.id];
        let link = |id: &String| format!("[{}](/{}.md)", link_text(title_of[id.as_str()]), path[id]);
        let paths_of = |ids: Option<&Vec<String>>| -> Vec<String> { ids.map(|v| v.iter().map(|id| format!("/{}.md", path[id])).collect()).unwrap_or_default() };
        let mut footer = Vec::new();
        for (label, ids) in [("Superseded by", superseded_by.get(&m.id)), ("Supersedes", supersedes.get(&m.id)), ("Contradicts", contradicts.get(&m.id))] {
            if let Some(v) = ids.filter(|v| !v.is_empty()) {
                footer.push(format!("- {label}: {}", v.iter().map(link).collect::<Vec<_>>().join(", ")));
            }
        }
        let rel = Relations { superseded_by: paths_of(superseded_by.get(&m.id)), supersedes: paths_of(supersedes.get(&m.id)), contradicts: paths_of(contradicts.get(&m.id)) };
        let description = description_of(&m.content, &m.title);
        let text = render_concept(m, r, &group_of(m).title, data.get(&m.id), &rel, &description, &footer, key.as_deref());
        let file = dir.join(format!("{r}.md"));
        write_if_changed(&file, &text, Some(&mut c))?;
        written.insert(file);
        // Into its folder's index, and each folder into the one above.
        let (parent, name) = r.rsplit_once('/').unwrap_or(("", r));
        folders.entry(parent.to_string()).or_default().1.push((format!("{name}.md"), m.title.clone(), description.clone()));
        by_group.entry(group_of(m).folder).or_default().push((r.clone(), m.title.clone(), description, m.updated_at.clone()));
        let mut d = parent.to_string();
        while !d.is_empty() {
            let (up, this_one) = match d.rsplit_once('/') {
                Some((a, e)) => (a.to_string(), e.to_string()),
                None => (String::new(), d.clone()),
            };
            folders.entry(up.clone()).or_default().0.insert(this_one);
            d = up;
        }
        log.push((m.updated_at.clone(), r.clone(), m.created_at.get(..10) == m.updated_at.get(..10), m.title.clone()));
    }

    // Each group's overview: without it, each memory would be a loose concept (okf-agent-memory fails
    // `okf validate --strict` if a concept neither links to nor is linked from another).
    for (folder, v) in &mut by_group {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        let title = group_titles.get(folder).cloned().unwrap_or_else(|| folder.clone());
        let description = tr!("The Omoikane memories of {title}.", "Las memorias de Omoikane de {title}.");
        let when = v.iter().map(|x| x.3.as_str()).max().unwrap_or("").to_string();
        let mut f = Map::new();
        f.insert("type".into(), json!("Overview"));
        f.insert("title".into(), json!(title));
        f.insert("description".into(), json!(description));
        f.insert("generated".into(), json!({ "by": format!("omoikane/{}", env!("CARGO_PKG_VERSION")), "at": when }));
        f.insert("omoikane".into(), json!({ "hub": true }));
        let mut s = String::from("---\n");
        for (k, x) in &f {
            yaml::block(&mut s, k, x, 0);
        }
        s += &format!("---\n\n# {}\n\n", link_text(&title));
        for (r, t, d, _) in v.iter() {
            s += &format!("* [{}](/{r}.md) - {d}\n", link_text(t));
        }
        let file = dir.join(folder).join(format!("{OVERVIEW}.md"));
        write_if_changed(&file, &s, None)?;
        written.insert(file);
        folders.entry(folder.clone()).or_default().1.push((format!("{OVERVIEW}.md"), title.clone(), description));
        folders.entry(String::new()).or_default().0.insert(folder.clone());
        c.hubs += 1;
    }

    // The indexes: the root one with `okf_version`, the rest without a header.
    let count_in = |folder: &str| path.values().filter(|r| r.starts_with(&format!("{folder}/"))).count();
    folders.entry(String::new()).or_default();
    for (folder, (subs, concepts)) in &folders {
        let mut s = String::new();
        if folder.is_empty() {
            s += &format!("---\nokf_version: \"{VERSION}\"\n---\n\n# {}\n\n", tr!("Omoikane memories", "Memorias de Omoikane"));
        } else {
            let name = folder.rsplit('/').next().unwrap_or(folder);
            let title = if folder.contains('/') { name.to_string() } else { group_titles.get(folder).cloned().unwrap_or_else(|| name.to_string()) };
            s += &format!("# {}\n\n", link_text(&title));
        }
        for sub in subs {
            let full = if folder.is_empty() { sub.clone() } else { format!("{folder}/{sub}") };
            let title = if folder.is_empty() { group_titles.get(sub).cloned().unwrap_or_else(|| sub.clone()) } else { sub.clone() };
            s += &format!("* [{}]({sub}/) - {}\n", link_text(&title), memory_count(count_in(&full)));
        }
        let mut concepts = concepts.clone();
        concepts.sort_by(|a, b| a.0.cmp(&b.0));
        for (f, t, d) in &concepts {
            s += &format!("* [{}]({f}) - {d}\n", link_text(t));
        }
        if subs.is_empty() && concepts.is_empty() {
            s += &format!("{}\n", tr!("Nothing exported yet.", "Todavía no hay nada exportado."));
        }
        s += &format!("\n{MARKER}\n");
        let p = if folder.is_empty() { dir.join("index.md") } else { dir.join(folder).join("index.md") };
        write_if_changed(&p, &s, None)?;
        written.insert(p);
    }

    // The log: by day of the last change, newest first.
    log.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut s = format!("# {}\n", tr!("Omoikane memory log", "Registro de las memorias de Omoikane"));
    let mut day = String::new();
    for (when, r, is_new, title) in &log {
        let d = when.get(..10).unwrap_or(when);
        if d != day {
            s += &format!("\n## {d}\n");
            day = d.to_string();
        }
        let what = if *is_new { "**Creation**" } else { "**Update**" };
        s += &format!("* {what}: [{}](/{r}.md)\n", link_text(title));
    }
    s += &format!("\n{MARKER}\n");
    let p = dir.join("log.md");
    write_if_changed(&p, &s, None)?;
    written.insert(p);

    c.deleted = prune(dir, &written)?;
    Ok(c)
}

/// A text in NFC (composed letters: "é" as a single character). macOS (HFS+) stores file
/// names in NFD ("e" and the accent apart) and returns them that way when listing: comparing or signing paths without
/// normalizing gave another name for the same concept (a duplicate on import, or a deletion when
/// re-exporting as a mirror).
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// Obsidian's `slug` over the text in NFC: in NFD, the loose accent isn't a letter and "café"
/// came out as `cafe`.
fn slug(s: &str, max: usize) -> String {
    crate::obsidian::slug(&nfc(s), max)
}

/// A path as NFC text, to compare the written ones with the ones the folder listing gives.
fn path_nfc(p: &Path) -> String {
    nfc(&p.to_string_lossy())
}

/// Whether two paths are the same file on disk: on macOS, the NFC name it was written with and the
/// NFD one HFS+ lists it with. Outside Unix (NTFS doesn't normalize), two different names are two files.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_: &Path, _: &Path) -> bool {
    false
}

struct Relations {
    superseded_by: Vec<String>,
    supersedes: Vec<String>,
    contradicts: Vec<String>,
}

/// The path of a concept that came from another bundle (`okf:<its id>`), so that it comes out where
/// it was again; Omoikane's keys (`okf:<memory id>`) and the rest, no.
fn path_from_key(key: &str) -> Option<String> {
    let id = key.strip_prefix("okf:")?;
    if is_ulid(id) {
        return None;
    }
    let parts: Vec<&str> = id.split('/').collect();
    if parts.iter().any(|p| !p.chars().any(char::is_alphanumeric)) {
        return None;
    }
    Some(parts.iter().map(|p| slug(p, 60)).collect::<Vec<_>>().join("/"))
}

/// A memory id coming from outside, in the form the database stores it (`None` if it isn't a ULID).
pub fn normalize_id(s: &str) -> Option<String> {
    ulid::Ulid::from_string(s.trim()).ok().map(|u| u.to_string())
}

fn is_ulid(s: &str) -> bool {
    s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The names Windows doesn't allow as a file.
fn is_reserved(k: &str) -> bool {
    let k = k.to_lowercase();
    matches!(k.as_str(), "con" | "prn" | "aux" | "nul") || (k.len() == 4 && (k.starts_with("com") || k.starts_with("lpt")) && k.as_bytes()[3].is_ascii_digit())
}

/// The text of a Markdown link: on one line and without loose brackets.
fn link_text(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ").replace('[', "\\[").replace(']', "\\]")
}

/// OKF's `description`: one sentence, on one line. The first line of the content with text, without
/// heading marks, up to the first period (at most 200 characters); if there is none, the title.
fn description_of(content: &str, title: &str) -> String {
    let line = content
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim_start_matches(['-', '*', '>']).trim())
        .find(|l| !l.is_empty() && !l.starts_with("```"))
        .unwrap_or(title);
    let mut sentence = line.to_string();
    for end in [". ", "? ", "! "] {
        if let Some(i) = sentence.find(end) {
            sentence.truncate(i + 1);
        }
    }
    let sentence: String = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
    if sentence.chars().count() > 200 { format!("{}…", sentence.chars().take(199).collect::<String>().trim_end()) } else { sentence }
}

/// The canonical `type` of each kind.
fn okf_type_name(t: &str) -> String {
    let mut c = t.chars();
    match c.next() {
        Some(p) => p.to_uppercase().chain(c).collect(),
        None => "Fact".into(),
    }
}

/// The Omoikane kind of an OKF `type` (anyone's: there is no closed list).
pub fn kind_from_okf(t: &str) -> Kind {
    let t: String = t.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let has = |ps: &[&str]| ps.iter().any(|p| t == *p || t.split(' ').any(|w| w == *p));
    if has(&["decision", "adr", "decisions"]) {
        Kind::Decision
    } else if has(&["gotcha", "pitfall", "caveat", "warning", "bug", "bugfix", "issue", "trap", "antipattern"]) {
        Kind::Gotcha
    } else if has(&["preference", "convention", "guideline", "style", "rule", "policy", "standard"]) {
        Kind::Preference
    } else if has(&["procedure", "playbook", "runbook", "process", "howto", "how to", "recipe", "workflow", "guide", "steps"]) {
        Kind::Procedure
    } else {
        Kind::Fact
    }
}

/// The actor of `generated.by` (`<producer>/<version>`): the agent that saved it, through Omoikane.
fn actor(source: &str) -> String {
    let f = source.trim();
    let f = f.strip_prefix("mcp:").unwrap_or(f);
    let producer = slug(f.split(':').next().unwrap_or(""), 40);
    format!("{}/omoikane", if f.is_empty() { "omoikane".to_string() } else { producer })
}

/// The entries of an OKF `verified` (a list or a single map).
fn verified_entries(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    }
}

fn by_of(x: &Value) -> &str {
    x.get("by").and_then(Value::as_str).unwrap_or("")
}

/// The verifications: a person's only if the memory carries the stamp and it holds (`m.verified`, the one from
/// `omoi verify`, the web or an import; a tag doesn't count), first; and the process ones it
/// brought from OKF (`data.okf.verified`), as is.
fn verifications(m: &Memory, stored: Option<&Value>) -> Vec<Value> {
    let mut v: Vec<Value> = Vec::new();
    if let Some(s) = &m.verified {
        v.push(json!({ "by": format!("human:{}", actor_id(&s.by)), "at": s.at }));
    }
    v.extend(verified_entries(stored).into_iter().filter(|x| !is_human(by_of(x))));
    v
}

/// An OKF actor id has no spaces (`human:\S+`): the name of whoever verified, with
/// spaces as `%20` (and `%` as `%25`), so it comes back the same when imported.
fn actor_id(name: &str) -> String {
    let mut r = String::new();
    for c in name.chars() {
        match c {
            '%' => r.push_str("%25"),
            c if c.is_whitespace() => r.push_str("%20"),
            c => r.push(c),
        }
    }
    r
}

/// The reverse of [`actor_id`]: `%20` is a space and `%25`, `%`; the rest, as is.
fn actor_name(id: &str) -> String {
    let mut r = String::new();
    let mut rest = id;
    while let Some(i) = rest.find('%') {
        r.push_str(&rest[..i]);
        let after = &rest[i..];
        let (c, n) = if after.starts_with("%20") {
            (" ", 3)
        } else if after.starts_with("%25") {
            ("%", 3)
        } else {
            ("%", 1)
        };
        r.push_str(c);
        rest = &after[n..];
    }
    r.push_str(rest);
    r
}

/// Who verified, from a person actor (`human:<id>`, `human/<id>`; bare `human`, nobody).
fn person_of(by: &str) -> Option<String> {
    let b = by.trim();
    let rest = b.get(5..).filter(|_| b.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("human")))?;
    let q = actor_name(rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?);
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    // The stamp allows up to 100 characters.
    (!q.is_empty()).then(|| q.chars().take(100).collect::<String>().trim_end().to_string())
}

/// A person actor, in OKF form (`human:<id>`) or in okf-agent-memory form (`human/…`).
fn is_human(by: &str) -> bool {
    let b = by.trim().to_lowercase();
    b == "human" || b.starts_with("human:") || b.starts_with("human/")
}

#[allow(clippy::too_many_arguments)]
fn render_concept(m: &Memory, concept_id: &str, project: &str, data: Option<&Value>, rel: &Relations, description: &str, footer: &[String], key: Option<&[u8]>) -> String {
    // What it brought from OKF that Omoikane doesn't have, and the rest of its data.
    let (okf, rest): (Map<String, Value>, Option<Value>) = match data {
        Some(Value::Object(o)) => {
            let mut o = o.clone();
            let okf = match o.remove(OKF_DATA_KEY) {
                Some(Value::Object(x)) => x,
                _ => Map::new(),
            };
            (okf, (!o.is_empty()).then_some(Value::Object(o)))
        }
        Some(other) => (Map::new(), Some(other.clone())),
        None => (Map::new(), None),
    };
    let okf_type = okf.get("type").and_then(Value::as_str).filter(|t| kind_from_okf(t).as_str() == m.kind_str()).map(str::to_string).unwrap_or_else(|| okf_type_name(m.kind_str()));
    let mut f = Map::new();
    f.insert("type".into(), json!(okf_type));
    f.insert("title".into(), json!(m.title));
    f.insert("description".into(), json!(description));
    let is_status = |t: &str| t == DRAFT || t == DEPRECATED;
    let tags: Vec<&String> = m
        .tags
        .iter()
        .filter(|t| !t.starts_with(crate::review::REVIEW_DATE_TAG) && !t.starts_with(memory::TOPIC_TAG) && !is_status(t))
        .collect();
    if !tags.is_empty() {
        f.insert("tags".into(), json!(tags));
    }
    let status = if !rel.superseded_by.is_empty() || m.tags.iter().any(|t| t == DEPRECATED) {
        Some(DEPRECATED)
    } else if m.tags.iter().any(|t| t == DRAFT) {
        Some(DRAFT)
    } else {
        None
    };
    if let Some(e) = status {
        f.insert("status".into(), json!(e));
    }
    if let Some(d) = crate::review::review_date_of(&m.tags) {
        f.insert("stale_after".into(), json!(format!("{d}T00:00:00Z")));
    }
    // `code_refs` are repository paths: the absolute ones (and those going out with `..`) go separately.
    let (relative, others): (Vec<&String>, Vec<&String>) = m.paths.iter().partition(|p| is_relative(p));
    if !relative.is_empty() {
        f.insert("code_refs".into(), json!(relative));
    }
    let by = okf.get("generated").and_then(|g| g.get("by")).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| actor(&m.source));
    // With the stamp (which only holds if the text is the one the person saw), the text hasn't changed since
    // it was verified even if tags or paths changed afterwards: `generated.at` doesn't go past that date
    // (OKF considers a verification older than `generated.at` superseded). The real date goes in
    // `omoikane.updated`.
    let generated = match &m.verified {
        Some(v) if v.at < m.updated_at => &v.at,
        _ => &m.updated_at,
    };
    f.insert("generated".into(), json!({ "by": by, "at": generated }));
    let verified = verifications(m, okf.get("verified"));
    if !verified.is_empty() {
        f.insert("verified".into(), Value::Array(verified));
    }
    // What it came from (OKF «sources»): the provenance, after the `sources` it brought from another bundle.
    let own_sources = sources_of(m);
    if !own_sources.is_empty() {
        let mut v: Vec<Value> = match okf.get("sources") {
            Some(Value::Array(a)) => a.iter().filter(|s| !is_our_source(s)).cloned().collect(),
            Some(other) => vec![other.clone()],
            None => Vec::new(),
        };
        v.extend(own_sources);
        f.insert("sources".into(), Value::Array(v));
    }
    let mut stored = Vec::new();
    for (k, v) in &okf {
        if !f.contains_key(k) && k != "omoikane" {
            f.insert(k.clone(), v.clone());
        }
        stored.push(k.clone());
    }
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    if let Some(k) = memory::key_of(m) {
        o.insert("key".into(), json!(k));
    }
    o.insert("kind".into(), json!(m.kind_str()));
    if m.global() {
        o.insert("scope".into(), json!("global"));
    } else if m.project_id != 0 {
        o.insert("project".into(), json!(project));
        if !m.project_key.is_empty() {
            o.insert("project_key".into(), json!(m.project_key));
        }
    }
    if !m.space.is_empty() {
        o.insert("space".into(), json!(m.space));
    }
    if !others.is_empty() {
        o.insert("paths".into(), json!(others));
    }
    if m.pinned {
        o.insert("pinned".into(), json!(true));
    }
    if !m.source.is_empty() {
        o.insert("source".into(), json!(m.source));
    }
    if !m.author.is_empty() {
        o.insert("author".into(), json!(m.author));
    }
    o.insert("created".into(), json!(m.created_at));
    o.insert("updated".into(), json!(m.updated_at));
    if let Some(p) = &m.provenance {
        o.insert(crate::provenance::KEY.into(), json!(p));
    }
    if !m.evidence.is_empty() {
        o.insert(crate::evidence::KEY.into(), json!(m.evidence));
    }
    if let Some(r) = &m.review_reason {
        o.insert(memory::REVIEW_REASON_KEY.into(), json!(r));
    }
    for (k, v) in [("superseded_by", &rel.superseded_by), ("supersedes", &rel.supersedes), ("contradicts", &rel.contradicts)] {
        if !v.is_empty() {
            o.insert(k.into(), json!(v));
        }
    }
    if !stored.is_empty() {
        o.insert("okf_keys".into(), json!(stored));
    }
    if let Some(d) = rest {
        o.insert("data".into(), d);
    }
    // The fingerprint of the verified text: on import, the stamp only comes back if the text is still that one
    // (re-editing the Markdown doesn't keep the verification).
    let sha = m.verified.as_ref().map(|_| crate::verified::fingerprint(m.kind_str(), &m.title, &m.content)).unwrap_or_default();
    if !sha.is_empty() {
        o.insert("verified_sha".into(), json!(sha));
    }
    f.insert("omoikane".into(), Value::Object(o.clone()));
    let render = |f: &Map<String, Value>| {
        let mut s = String::from("---\n");
        for (k, v) in f {
            yaml::block(&mut s, k, v, 0);
        }
        s += "---\n\n";
        s += m.content.trim_end();
        s += "\n";
        if !footer.is_empty() {
            s += &format!("\n{FOOTER}\n{}\n", footer.join("\n"));
        }
        s
    };
    let s = render(&f);
    // The signature, of what the import will extract from this same text (see `signature`): it is read as it will be read
    // on import and that is signed. If it couldn't be read (doesn't happen), it goes unsigned: it would come in as someone else's.
    if let Some(k) = key
        && let Some((header, body)) = split_front_matter(&s)
        && let Ok(header) = yaml::parse(header)
        && let Ok(e) = to_entry(&Concept { id: concept_id.to_string(), header, body: body.to_string() })
    {
        o.insert("sig".into(), json!(signature::sign(k, &signed_text(&e))));
        f.insert("omoikane".into(), Value::Object(o));
        return render(&f);
    }
    s
}

/// A path inside the repository: neither absolute (`/x`, `C:/x`, `\\server`) nor with `..`.
/// The `id` prefix of the `sources` that come from a memory's provenance.
const SOURCE_ID_PREFIX: &str = "omoikane-";

/// A memory's provenance as OKF `sources` (1.10.1): the commit (a link to the commit if the
/// project is on GitHub, GitLab or Codeberg; otherwise `git:<sha>`), the agent session and the
/// OpenTelemetry trace. The exact data travels in `omoikane.provenance`; this is for whoever reads the bundle.
fn sources_of(m: &Memory) -> Vec<Value> {
    let Some(p) = &m.provenance else { return Vec::new() };
    let mut v = Vec::new();
    if !p.commit.is_empty() {
        let repo = m.project_key.strip_prefix("git:").filter(|r| ["github.com/", "gitlab.com/", "codeberg.org/"].iter().any(|h| r.starts_with(h)));
        let resource = match repo {
            Some(r) => format!("https://{r}/commit/{}", p.commit),
            None => format!("git:{}", p.commit),
        };
        let branch = if p.branch.is_empty() { String::new() } else { format!(" ({})", p.branch) };
        v.push(json!({ "id": format!("{SOURCE_ID_PREFIX}commit"), "resource": resource, "title": format!("commit {}{branch}", p.commit.get(..12).unwrap_or(&p.commit)) }));
    }
    if !p.session.is_empty() {
        let who = if m.source.is_empty() { String::new() } else { format!(" ({})", m.source) };
        v.push(json!({ "id": format!("{SOURCE_ID_PREFIX}session"), "resource": format!("session:{}", p.session), "title": format!("agent session{who}") }));
    }
    if !p.trace_id.is_empty() {
        v.push(json!({ "id": format!("{SOURCE_ID_PREFIX}trace"), "resource": format!("trace:{}/{}", p.trace_id, p.span_id), "title": "OpenTelemetry trace" }));
    }
    v
}

/// Whether a `sources` entry is one of those that come from the provenance (rebuilt on export).
fn is_our_source(s: &Value) -> bool {
    s.get("id").and_then(Value::as_str).is_some_and(|i| i.starts_with(SOURCE_ID_PREFIX))
}

fn is_relative(p: &str) -> bool {
    let p = p.replace('\\', "/");
    !p.starts_with('/') && !p.starts_with('~') && !p.contains(':') && !p.split('/').any(|x| x == "..")
}

/// Writes only if it changes (and counts it, if it's a concept).
fn write_if_changed(path: &Path, text: &str, c: Option<&mut ExportCount>) -> Result<()> {
    let existing = std::fs::read(path).ok();
    if existing.as_deref() == Some(text.as_bytes()) {
        if let Some(c) = c {
            c.unchanged += 1;
        }
        return Ok(());
    }
    if let Some(c) = c {
        if existing.is_some() {
            c.updated += 1;
        } else {
            c.created += 1;
        }
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, text).with_context(|| tr!("could not write {}", "no se pudo escribir {}", path.display()))
}

/// Deletes Omoikane's files that no longer come out (and the folders left empty): the concepts with
/// `omoikane:` in the header and the indexes with the marker. Folders starting with `.` are not looked at.
fn prune(root: &Path, written: &BTreeSet<PathBuf>) -> Result<usize> {
    let mut removed = 0;
    // What was written, by its NFC name: the HFS+ listing returns it in NFD.
    let by_nfc: HashMap<String, &PathBuf> = written.iter().map(|p| (path_nfc(p), p)).collect();
    // Written with this name, or the same file with the name in another form (HFS+). A separate file
    // with the same name in NFD (a bundle from a Mac copied to Linux or Windows) is an old copy of the
    // concept: it is deleted like what no longer comes out.
    let is_written = |p: &Path| written.contains(p) || by_nfc.get(&path_nfc(p)).is_some_and(|e| same_file(p, e));
    let mut folders = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if !e.file_name().to_string_lossy().starts_with('.') {
                    folders.push(p.clone());
                    pending.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "md") && !is_written(&p) {
                let reserved_name = matches!(e.file_name().to_str(), Some("index.md" | "log.md"));
                if (reserved_name && is_our_index(&p)) || (!reserved_name && is_our_concept(&p)) {
                    std::fs::remove_file(&p)?;
                    removed += 1;
                }
            }
        }
    }
    folders.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in folders {
        let _ = std::fs::remove_dir(&d); // only if empty
    }
    Ok(removed)
}

fn is_our_index(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|t| t.contains(MARKER))
}

fn is_our_concept(p: &Path) -> bool {
    let Ok(t) = std::fs::read_to_string(p) else { return false };
    let Some((header, _)) = split_front_matter(&t) else { return false };
    header.lines().any(|l| l.starts_with("omoikane:"))
}

// --- This database's signature ----------------------------------------------------------------------------

/// How we know a concept came from THIS database: each one carries in `omoikane.sig` an HMAC-SHA256,
/// with a random key of this database (`meta('okf_firma')`, which travels neither with sync nor with
/// `omoi export`), of EVERYTHING the import applies from it ([`signed_text`]): its path, its id, its
/// key, its project, whether it's global or pinned, its kind, title and content, its paths, its tags
/// (also those from `status` and `stale_after`), its data, its dates, its origin, its relations
/// (`superseded_by`, `contradicts`) and its verification. On export it is computed by reading the concept already
/// written just as on import, so what is signed is exactly what will be applied. Someone else's bundle
/// (another person, another database, one written by hand or touched up in any field) can't
/// compute it: none of that is trusted (the text is imported, like any bundle's, into a new
/// memory). The ids existing here is not enough: anyone who has seen a bundle of yours has them.
pub(crate) mod signature {
    use anyhow::{Result, anyhow};
    use rusqlite::{Connection, OptionalExtension, params};

    const META: &str = "okf_firma";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn from_hex(s: &str) -> Option<Vec<u8>> {
        let s = s.trim();
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return None;
        }
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
    }

    /// This database's key; with `create`, it creates it if there is none yet (on export). On import without a key,
    /// nothing is our own.
    pub fn key(db: &Connection, create: bool) -> Result<Option<Vec<u8>>> {
        let existing: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META], |f| f.get(0)).optional()?;
        if let Some(k) = existing.as_deref().and_then(from_hex).filter(|k| k.len() == 32) {
            return Ok(Some(k));
        }
        if !create {
            return Ok(None);
        }
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META, hex(&b)])?;
        Ok(Some(b.to_vec()))
    }

    pub fn sign(key: &[u8], text: &str) -> String {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
        hex(ring::hmac::sign(&k, text.as_bytes()).as_ref())
    }

    pub fn is_valid(key: &[u8], text: &str, signature: &str) -> bool {
        let Some(f) = from_hex(signature) else { return false };
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
        ring::hmac::verify(&k, text.as_bytes(), &f).is_ok()
    }
}

/// What is signed of a concept: everything the import extracts from it and applies (see `signature`), as
/// it comes out of [`to_entry`], as JSON (maps in the order they come: another order is another signature).
/// Except the signature itself and what isn't applied (the links footer, the `description` of our own concept).
fn signed_text(e: &Entry) -> String {
    let n = &e.memory;
    let mut v = json!({
        "concept": e.concept,
        "id": e.id,
        "key": e.own_key,
        "match_key": e.key,
        "global": e.global,
        "project_key": e.project_key,
        "kind": n.kind.map(|k| k.as_str()),
        "kind_text": n.kind_text,
        "title": n.title,
        "content": n.content,
        "paths": n.paths,
        "tags": n.tags,
        "pinned": n.pinned,
        "source": n.source,
        "data": e.data,
        "created": e.created,
        "updated": e.updated,
        "generated": e.generated,
        "superseded_by": e.superseded_by,
        "contradicts": e.contradicts,
        "verified": e.verified,
        "verified_sha": e.verified_sha,
    });
    // Only if there is one (1.10.1): what a 1.8.5–1.10.0 signed, without provenance, still holds.
    if let Some(p) = &n.provenance {
        v["provenance"] = json!(p);
    }
    // Same with the evidence (1.13).
    if !n.evidence.is_empty() {
        v["evidence"] = json!(n.evidence);
    }
    format!("omoikane-okf-3\n{v}")
}

// --- Reading a bundle --------------------------------------------------------------------------------

/// A concept that was read: its id (the path without `.md`), its header and its body.
#[derive(Clone, Debug)]
pub struct Concept {
    pub id: String,
    pub header: Map<String, Value>,
    pub body: String,
}

/// A bundle read and checked.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub root: PathBuf,
    /// The `okf_version` of the root `index.md`, if it says it.
    pub version: Option<String>,
    pub concepts: Vec<Concept>,
    /// What was skipped while reading it (symbolic links, files too big), to report it.
    pub warnings: Vec<String>,
}

/// A bigger concept isn't read (it's skipped with a warning): a header and a note, not a dump.
pub const MAX_CONCEPT: u64 = 1 << 20;

/// A file whose header starts with `---\r\n` was turned into `\r\n` throughout by whoever copied it (git with
/// `core.autocrlf=true`, the default in Git for Windows; an editor): Omoikane always writes `\n`.
/// Back to `\n` throughout, so a concept of this database still holds with its signature (it was signed with
/// `\n`) and doesn't come in as a new one, duplicated and without its verification. Content with real `\r\n`
/// goes in a file with the header in `\n`, and is left alone.
pub(crate) fn strip_git_crlf(text: String) -> String {
    let t = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if t.starts_with("---\r\n") { text.replace("\r\n", "\n") } else { text }
}

/// The header and the body of a Markdown file, if it starts with a closed `---` header.
pub(crate) fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = t.strip_prefix("---\r\n").or_else(|| t.strip_prefix("---\n"))?;
    let mut i = 0;
    for l in rest.split_inclusive('\n') {
        let trimmed = l.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            return Some((&rest[..i], &rest[i + l.len()..]));
        }
        i += l.len();
    }
    // A header that ends at the last `---` without a line break.
    if rest.ends_with("\n---") {
        return Some((&rest[..rest.len() - 3], ""));
    }
    None
}

/// Reads an OKF bundle and checks that it is one (OKF §11): every `.md` other than `index.md` and `log.md`
/// has a readable YAML header with a non-empty `type`. If the folder has no `index.md` but does have
/// `knowledge/`, it reads that one (a repository's folder with its bundle).
pub fn read(dir: &Path) -> Result<Bundle> {
    read_with_limits(dir, &Limits::default())
}

/// How many entries (files and folders) are walked at most when reading a bundle.
pub const MAX_FILES: usize = 20_000;
/// How many bytes of concepts are read at most when reading a bundle. A real bundle is far
/// less: the one of a real database of 230 memories takes 0.4 MB, and 20,000 concepts of 2 KB are 40 MB.
pub const MAX_BYTES: u64 = 50 << 20;
/// The most a concept's YAML header (or `index.md`'s) can measure. A real one doesn't go past a
/// few KB (the one of a concept exported from Omoikane, with its signature, ~1 KB); the rest goes in the body.
pub const MAX_HEADER: usize = 256 << 10;
/// The most all the headers together can measure: 20,000 concepts of 800 bytes.
pub const MAX_HEADERS: usize = 16 << 20;
/// How many YAML values (each text, number, list or map) are read at most across all the
/// headers. Once read, a header takes in memory about 40 times its size if it's all short
/// lists (`[a, a, a, …]`): 192 MB of such headers were 60 s and 7.5 GB of RAM. With this limit, about
/// 200 MB at most; a real header has 10 to 40 values.
pub const MAX_NODES: usize = 2_000_000;

/// The limits when reading a bundle (see the constants; the tests lower them).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub files: usize,
    pub bytes: u64,
    pub header: usize,
    pub headers: usize,
    pub nodes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { files: MAX_FILES, bytes: MAX_BYTES, header: MAX_HEADER, headers: MAX_HEADERS, nodes: MAX_NODES }
    }
}

/// How many values a read value has: itself and the ones inside.
fn node_count(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(node_count).sum::<usize>(),
        Value::Object(o) => 1 + o.values().map(node_count).sum::<usize>(),
        _ => 1,
    }
}

/// The error for a bundle that goes past a limit, so it's clear it isn't a failure of one concept.
fn too_big(root: &Path, what: String) -> anyhow::Error {
    anyhow!(tr!(
        "{} {what}: too much for a knowledge bundle (is it the right folder?); nothing was imported",
        "{} {what}: demasiado para un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
        root.display()
    ))
}

/// Whether an entry can be read without following a link: neither a symbolic link nor a Windows junction.
pub(crate) fn not_a_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| !m.file_type().is_symlink()) && !is_junction(p)
}

fn read_with_limits(dir: &Path, limits: &Limits) -> Result<Bundle> {
    let (max_files, max_bytes) = (limits.files, limits.bytes);
    crate::project::require_local(dir)?;
    if !dir.is_dir() {
        bail!(tr!("{} is not a folder", "{} no es una carpeta", dir.display()));
    }
    let knowledge = dir.join("knowledge");
    let root = if !dir.join("index.md").exists() && not_a_link(&knowledge) && knowledge.join("index.md").exists() { knowledge } else { dir.to_path_buf() };
    let mut version = None;
    let mut warnings = Vec::new();
    let index = root.join("index.md");
    // The `index.md`, with the same limits as a concept: without following links and 1 MB at most.
    let index_ok = index.exists() && {
        if !not_a_link(&index) {
            warnings.push(tr!("index.md: a symbolic link, skipped", "index.md: un enlace simbólico, saltado"));
            false
        } else if std::fs::metadata(&index).is_ok_and(|m| m.len() > MAX_CONCEPT) {
            warnings.push(tr!("index.md: over 1 MB, skipped", "index.md: de más de 1 MB, saltado"));
            false
        } else {
            true
        }
    };
    if index_ok {
        let t = std::fs::read_to_string(&index).with_context(|| index.display().to_string())?;
        if let Some((header, _)) = split_front_matter(&t).filter(|(c, _)| {
            let fits = c.len() <= limits.header;
            if !fits {
                warnings.push(tr!("index.md: a YAML header over {} KB, skipped", "index.md: una cabecera YAML de más de {} KB, saltado", limits.header >> 10));
            }
            fits
        }) {
            let c = yaml::parse(header).map_err(|e| anyhow!(tr!("{}: the YAML header can't be read: {e}", "{}: no se puede leer la cabecera YAML: {e}", index.display())))?;
            version = c.get("okf_version").map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
    }
    let mut files = Vec::new();
    let (mut seen, mut bytes) = (0usize, 0u64);
    let mut pending = vec![root.clone()];
    while let Some(d) = pending.pop() {
        let rd = std::fs::read_dir(&d).with_context(|| d.display().to_string())?;
        for e in rd.flatten() {
            seen += 1;
            if seen > max_files {
                bail!(tr!(
                    "{} has over {max_files} files and folders: that's not a knowledge bundle (is it the right folder?); nothing was imported",
                    "{} tiene más de {max_files} ficheros y carpetas: eso no es un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
                    root.display()
                ));
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            // The type of the entry itself, without following links: a symbolic link (or a Windows
            // junction) can point to a folder above and never end, or take the bundle outside.
            let Ok(t) = e.file_type() else { continue };
            let rel = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if t.is_symlink() || is_junction(&p) {
                warnings.push(tr!("{rel}: a symbolic link, skipped", "{rel}: un enlace simbólico, saltado"));
                continue;
            }
            if t.is_dir() {
                pending.push(p);
            } else if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")) && !matches!(name.as_str(), "index.md" | "log.md") {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                if size > MAX_CONCEPT {
                    warnings.push(tr!("{rel}: over 1 MB, skipped", "{rel}: de más de 1 MB, saltado"));
                    continue;
                }
                bytes += size;
                if bytes > max_bytes {
                    bail!(tr!(
                        "{} holds over {} MB of concepts: too much for a knowledge bundle (is it the right folder?); nothing was imported",
                        "{} tiene más de {} MB de conceptos: demasiado para un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
                        root.display(),
                        max_bytes >> 20
                    ));
                }
                files.push(p);
            }
        }
    }
    files.sort();
    let mut concepts = Vec::new();
    let mut bad = Vec::new();
    // First, the texts and the size of their headers (without parsing the YAML): a huge header is never
    // parsed, and all of them together have their limit before any is parsed.
    let mut read_files: Vec<(String, String, String)> = Vec::new();
    let mut header_bytes = 0usize;
    for p in &files {
        let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
        // In NFC: a bundle that went through a Mac (HFS+) lists its names in NFD, and the concept's id
        // (its path) goes into its key and its signature.
        let id = nfc(rel.strip_suffix(".md").or_else(|| rel.strip_suffix(".MD")).unwrap_or(&rel));
        let text = match std::fs::read_to_string(p) {
            Ok(t) => strip_git_crlf(t),
            Err(e) => {
                bad.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let Some((header, _)) = split_front_matter(&text) else {
            bad.push(tr!("{rel}: no YAML header (--- … ---)", "{rel}: sin cabecera YAML (--- … ---)"));
            continue;
        };
        if header.len() > limits.header {
            bad.push(tr!(
                "{rel}: a YAML header over {} KB (a concept's header holds a few fields; the rest goes in the body)",
                "{rel}: una cabecera YAML de más de {} KB (la de un concepto lleva unos pocos campos; lo demás va en el cuerpo)",
                limits.header >> 10
            ));
            continue;
        }
        header_bytes += header.len();
        if header_bytes > limits.headers {
            return Err(too_big(&root, tr!("holds over {} MB of YAML headers", "tiene más de {} MB de cabeceras YAML", limits.headers >> 20)));
        }
        read_files.push((rel, id, text));
    }
    let mut node_total = 0usize;
    for (rel, id, text) in read_files {
        let Some((header, body)) = split_front_matter(&text) else { continue };
        let c = match yaml::parse(header) {
            Ok(c) => c,
            Err(e) => {
                bad.push(tr!("{rel}: the YAML header can't be read: {e}", "{rel}: no se puede leer la cabecera YAML: {e}"));
                continue;
            }
        };
        node_total += c.values().map(node_count).sum::<usize>();
        if node_total > limits.nodes {
            return Err(too_big(&root, tr!("holds over {} values in its YAML headers", "tiene más de {} valores en sus cabeceras YAML", limits.nodes)));
        }
        if !c.get("type").is_some_and(|t| !scalar_text(t).trim().is_empty()) {
            bad.push(tr!("{rel}: the header has no `type`", "{rel}: la cabecera no tiene `type`"));
            continue;
        }
        concepts.push(Concept { id, header: c, body: body.to_string() });
    }
    if !bad.is_empty() {
        let n = bad.len();
        bad.truncate(5);
        bail!(tr!(
            "{} is not a valid OKF bundle ({n} files wrong; every .md other than index.md and log.md needs a YAML header with `type`):\n  {}",
            "{} no es un bundle OKF válido ({n} ficheros mal; cada .md que no sea index.md ni log.md necesita una cabecera YAML con `type`):\n  {}",
            root.display(),
            bad.join("\n  ")
        ));
    }
    if concepts.is_empty() {
        bail!(tr!(
            "{} has no OKF concepts (Markdown files with a YAML header that says their `type`)",
            "{} no tiene conceptos OKF (ficheros Markdown con una cabecera YAML que diga su `type`)",
            root.display()
        ));
    }
    Ok(Bundle { root, version, concepts, warnings })
}

/// A Windows junction: `file_type` doesn't report it as a symbolic link, but it's a mount
/// point that can lead to any folder (also one above).
#[cfg(windows)]
pub(crate) fn is_junction(p: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & REPARSE_POINT != 0)
}

#[cfg(not(windows))]
pub(crate) fn is_junction(_: &Path) -> bool {
    false
}

pub(crate) fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn text_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(scalar_text).filter(|s| !s.trim().is_empty()).collect(),
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![scalar_text(other)],
    }
}

// --- Import ----------------------------------------------------------------------------------------

/// Where the imported memories go.
#[derive(Clone, Debug)]
pub enum Destination {
    /// To the project of this folder (empty: to the global); what was exported as global, to the global.
    Project(String),
    /// To this space, without a project.
    Space(i64),
}

/// What an import did (or would do, dry run).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportSummary {
    pub concepts: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub verdicts: usize,
    /// The memories that got (or in a dry run, would get) the verified-by-a-person stamp.
    pub verified: usize,
    /// Those that carried a person's `verified` and were imported without a stamp (inside an
    /// agent's shell).
    pub not_verified: usize,
    /// Those that carried a person's `verified` in a bundle that isn't from this database and were imported
    /// without a stamp (without `--trust-verified`).
    pub untrusted_verified: usize,
    /// Those that carried a person's `verified` that no longer holds: the text changed since then (it isn't
    /// the one of `verified_sha`), the person unverified it here afterwards, or the date is in the future.
    pub stale_verified: usize,
    /// Those that come in (or would come in) as global.
    pub global: usize,
    /// Those that come in (or would come in) pinned.
    pub pinned: usize,
    /// The concepts that didn't come from this database (without this database's signature, see `signature`).
    pub foreign: usize,
    /// Of those, the ones that asked to be global and go to the destination.
    pub ignored_global: usize,
    /// And the ones that asked to be pinned and come in unpinned.
    pub ignored_pinned: usize,
    /// Those of a memory forgotten here (in the trash or with its forget mark): they don't come back.
    pub skipped_forgotten: usize,
    /// Those of this database older than their memory here (exported before it changed): they don't touch it.
    pub skipped_older: usize,
    /// The titles of the memories where something that looked like a secret was covered.
    pub redacted: Vec<String>,
}

/// A memory ready to save, taken from a concept.
struct Entry {
    concept: String,
    /// The key it is saved by if it doesn't come back with its id: its own or the concept's.
    key: String,
    /// The key it had in Omoikane (`remember --key`), if it had one.
    own_key: Option<String>,
    /// The memory id it carries, if it came from Omoikane.
    id: Option<String>,
    global: bool,
    /// The sync key of its project, if it came from an Omoikane one.
    project_key: Option<String>,
    memory: NewMemory,
    data: Option<Value>,
    created: Option<String>,
    updated: Option<String>,
    superseded_by: Vec<String>,
    contradicts: Vec<String>,
    /// Who verified it and when, if it carries a person's `verified`.
    verified: Option<(String, Option<String>)>,
    /// The concept's `generated.at`: an earlier verification is superseded (OKF) and gives no stamp.
    generated: Option<String>,
    /// The fingerprint of the text that was verified (`omoikane.verified_sha`), if it came from Omoikane with a stamp.
    verified_sha: Option<String>,
    /// The signature of the database it came from (`omoikane.sig`, see `signature`).
    signature: Option<String>,
}

/// The key a concept is saved by: the one it brings from Omoikane, `okf:<memory id>` if it came
/// from Omoikane without a key, and `okf:<concept id>` if it comes from elsewhere (lowercase, with what
/// isn't valid in a key changed to `-`, and with a piece of its hash if it goes past 80).
fn concept_key(id: &str, om: &Map<String, Value>) -> String {
    if let Some(k) = om.get("key").and_then(Value::as_str)
        && let Ok(k) = memory::topic_key(k)
    {
        return k;
    }
    if let Some(i) = om.get("id").and_then(Value::as_str).filter(|i| is_ulid(i)) {
        return format!("okf:{}", i.to_lowercase());
    }
    let clean: String = id.to_lowercase().chars().map(|c| if c.is_alphanumeric() || "-_./".contains(c) { c } else { '-' }).collect();
    let k = format!("okf:{clean}");
    if k.chars().count() <= 80 {
        return k;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(id.as_bytes());
    let short: String = k.chars().take(71).collect();
    format!("{short}-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}

/// An ISO 8601 instant in the form SQLite's date functions understand.
fn sql_instant(s: &str) -> String {
    s.trim().trim_end_matches('Z').replace('T', " ")
}

/// A valid `YYYY-MM-DD` date at the start of an ISO 8601 instant.
fn day_of(instant: &str) -> Option<String> {
    let d = instant.trim().get(..10)?;
    let b = d.as_bytes();
    let ok = b.len() == 10 && b[4] == b'-' && b[7] == b'-' && d.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    let (month, day): (u32, u32) = (d[5..7].parse().ok()?, d[8..10].parse().ok()?);
    (ok && (1..=12).contains(&month) && (1..=31).contains(&day)).then(|| d.to_string())
}

fn to_entry(c: &Concept) -> Result<Entry> {
    let f = &c.header;
    let om = match f.get("omoikane") {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    let ours = !om.is_empty();
    let okf_type = scalar_text(&f["type"]).trim().to_string();
    let kind = om.get("kind").and_then(Value::as_str).and_then(|k| k.parse::<Kind>().ok()).unwrap_or_else(|| kind_from_okf(&okf_type));
    let title = f.get("title").map(scalar_text).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
        let name = c.id.rsplit('/').next().unwrap_or(&c.id);
        name.replace(['-', '_'], " ")
    });
    let mut body = c.body.as_str();
    if ours && let Some(i) = body.find(FOOTER) {
        body = &body[..i];
    }
    // The body of one of our concepts, as is (the content may carry `\r\n`); someone else's, with
    // Omoikane's line breaks.
    let mut content = if ours { body.trim().to_string() } else { body.replace("\r\n", "\n").trim().to_string() };
    if let Some(d) = f.get("description").map(scalar_text).map(|d| d.trim().to_string()).filter(|d| !d.is_empty())
        && !ours
        && !content.contains(&d)
    {
        content = if content.is_empty() { d } else { format!("{d}\n\n{content}") };
    }
    let mut title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() > 200 {
        content = format!("{title}\n\n{content}");
        title = format!("{}…", title.chars().take(199).collect::<String>());
    }
    if content.trim().is_empty() {
        content = title.clone();
    }

    let mut tags: Vec<String> = text_list(f.get("tags")).into_iter().filter(|t| !t.starts_with(crate::review::REVIEW_DATE_TAG)).collect();
    if let Some(d) = f.get("stale_after").map(scalar_text).as_deref().and_then(day_of) {
        tags.push(format!("{}{d}", crate::review::REVIEW_DATE_TAG));
    }
    // A person's verification (the most recent, if there are several): it becomes the stamp when
    // saving it, not data nor a tag.
    let verified = verified_entries(f.get("verified"))
        .iter()
        .filter_map(|v| Some((person_of(by_of(v))?, v.get("at").map(scalar_text).filter(|a| !a.trim().is_empty()))))
        .max_by(|a, b| a.1.cmp(&b.1));
    let superseded_by = text_list(om.get("superseded_by"));
    match f.get("status").map(scalar_text).map(|s| s.trim().to_lowercase()).as_deref() {
        Some(DEPRECATED) if superseded_by.is_empty() => tags.push(DEPRECATED.into()),
        Some(DRAFT) => tags.push(DRAFT.into()),
        _ => {}
    }

    // What OKF has and Omoikane doesn't, into the data: from one of our concepts, only what already came from
    // outside (`okf_keys`); from someone else's, everything that doesn't become a field.
    let mut okf = Map::new();
    if ours {
        for k in text_list(om.get("okf_keys")) {
            if let Some(v) = f.get(&k) {
                okf.insert(k, v.clone());
            }
        }
        // `sources` is written by Omoikane (the provenance): whatever someone adds there also stays.
        if let Some(v) = f.get("sources") {
            okf.insert("sources".into(), v.clone());
        }
    } else {
        for (k, v) in f {
            if !MAPPED_KEYS.contains(&k.as_str()) {
                okf.insert(k.clone(), v.clone());
            }
        }
        if okf_type != okf_type_of(kind) {
            okf.insert("type".into(), json!(okf_type));
        }
    }
    // From `verified`, only the process ones go to the data: a person's are the stamp (or nothing).
    if let Some(v) = okf.remove("verified") {
        let processes: Vec<Value> = verified_entries(Some(&v)).into_iter().filter(|x| !is_human(by_of(x))).collect();
        if !processes.is_empty() {
            okf.insert("verified".into(), Value::Array(processes));
        }
    }
    // From `sources`, those that come from the provenance aren't data: they come back in `omoikane.provenance`.
    if let Some(Value::Array(a)) = okf.get_mut("sources") {
        a.retain(|s| !is_our_source(s));
        if a.is_empty() {
            okf.remove("sources");
        }
    }
    let data = match (om.get("data").cloned(), okf.is_empty()) {
        (d, true) => d,
        (None, false) => Some(json!({ OKF_DATA_KEY: okf })),
        (Some(Value::Object(mut o)), false) => {
            o.insert(OKF_DATA_KEY.into(), Value::Object(okf));
            Some(Value::Object(o))
        }
        (Some(other), false) => Some(json!({ "value": other, OKF_DATA_KEY: okf })),
    };
    // The date of the text: `generated.at`; without it, the most recent verification (the text already existed
    // then, and so it doesn't come out as superseded when exported again).
    let generated_at = f.get("generated").and_then(|g| g.get("at")).map(scalar_text).or_else(|| {
        verified_entries(f.get("verified")).iter().filter_map(|v| v.get("at").map(scalar_text)).filter(|a| !a.trim().is_empty()).max()
    });
    let text = |k: &str| om.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Entry {
        concept: c.id.clone(),
        key: concept_key(&c.id, &om),
        own_key: om.get("key").and_then(Value::as_str).and_then(|k| memory::topic_key(k).ok()),
        // In its form (uppercase): `Ulid::from_string` also accepts lowercase, and the database stores and
        // compares ids as is (a forget with the id in lowercase didn't match and the memory came back).
        id: text("id").and_then(|i| normalize_id(&i)),
        global: text("scope").as_deref() == Some("global"),
        project_key: text("project_key").filter(|k| !k.is_empty()),
        memory: NewMemory {
            kind: Some(kind),
            // A kind from a future Omoikane version, as is (as in sync).
            kind_text: om.get("kind").and_then(Value::as_str).filter(|k| !k.is_empty() && k.parse::<Kind>().is_err()).map(str::to_string),
            title,
            content,
            paths: text_list(f.get("code_refs")).into_iter().chain(text_list(om.get("paths"))).collect(),
            tags,
            pinned: om.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            source: text("source").unwrap_or_else(|| if ours { String::new() } else { "import:okf".into() }),
            provenance: om.get(crate::provenance::KEY).and_then(|p| serde_json::from_value::<crate::provenance::Provenance>(p.clone()).ok()).filter(|p| !p.is_empty()),
            evidence: om
                .get(crate::evidence::KEY)
                .and_then(|v| serde_json::from_value::<Vec<crate::evidence::Evidence>>(v.clone()).ok())
                .and_then(|v| crate::evidence::clean(v).ok())
                .unwrap_or_default(),
            review_reason: om.get(memory::REVIEW_REASON_KEY).and_then(Value::as_str).and_then(memory::clean_review_reason),
            ..Default::default()
        },
        data,
        created: text("created").or_else(|| generated_at.clone()),
        updated: text("updated").or(generated_at),
        superseded_by,
        contradicts: text_list(om.get("contradicts")),
        verified,
        generated: f.get("generated").and_then(|g| g.get("at")).map(scalar_text).filter(|a| !a.trim().is_empty()),
        verified_sha: text("verified_sha").filter(|s| !s.is_empty()),
        signature: text("sig").filter(|s| !s.is_empty()),
    })
}

fn okf_type_of(t: Kind) -> String {
    okf_type_name(t.as_str())
}

fn is_overview(c: &Concept) -> bool {
    c.header.get("omoikane").and_then(|o| o.get("hub")).and_then(Value::as_bool) == Some(true)
}

/// How many concepts of a bundle came from Omoikane (carry `omoikane:`) outside the space with key
/// `space`: personal memories (without a space) or another client's. Imported with `--space`, they would go to
/// that space's repository, which other people share.
pub fn outside_space_count(b: &Bundle, space: &str) -> usize {
    b.concepts
        .iter()
        .filter(|c| !is_overview(c))
        .filter_map(|c| c.header.get("omoikane").and_then(Value::as_object))
        .filter(|o| o.get("space").and_then(Value::as_str).unwrap_or("") != space)
        .count()
}

/// The space of a bundle exported from a space (its concepts' `omoikane.space`).
pub fn space_of(b: &Bundle) -> Option<String> {
    b.concepts.iter().find_map(|c| c.header.get("omoikane").and_then(|o| o.get("space")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
}

/// Where an entry lands: (project folder, project id, space) for `NewMemory`. What came
/// from a project this database also has (the same sync key, in the same space
/// or without a space) goes back to it; the rest, to the destination.
fn placement(db: &Connection, destination: &Destination, e: &Entry) -> Result<(String, Option<i64>, Option<i64>)> {
    let own_project = |space: Option<i64>| -> Result<Option<i64>> {
        let Some(k) = &e.project_key else { return Ok(None) };
        Ok(db.query_row("SELECT id FROM project WHERE key = ?1 AND space_id IS ?2 AND id != 0 LIMIT 1", params![k, space], |f| f.get(0)).optional()?)
    };
    Ok(match destination {
        Destination::Space(s) => match own_project(Some(*s))? {
            Some(p) => (String::new(), Some(p), None),
            None => (String::new(), None, Some(*s)),
        },
        Destination::Project(_) if e.global => (String::new(), None, None),
        Destination::Project(p) => match own_project(None)? {
            Some(id) => (String::new(), Some(id), None),
            None => (p.clone(), None, None),
        },
    })
}

/// The memory that already has that key where the entry would land, with its stamp (to know whether it changed).
fn find_by_key(db: &Connection, project: &str, project_id: Option<i64>, space: Option<i64>, key: &str) -> Result<Option<(String, String)>> {
    let project_id = match (space, project_id) {
        (Some(_), _) => 0,
        (None, Some(p)) => p,
        (None, None) => match crate::project::resolve(db, project)? {
            Some((id, _)) => id,
            None => return Ok(None),
        },
    };
    Ok(db
        .query_row(
            "SELECT id, hlc FROM memory WHERE project_id = ?1 AND space_id IS ?2
               AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?3) ORDER BY created_at, id LIMIT 1",
            params![project_id, space, format!("{}{key}", memory::TOPIC_TAG)],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?)
}

fn current_hlc(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0))?)
}

/// What happened to a concept when importing it.
enum Outcome {
    Created,
    Changed,
    Same,
    /// From a memory forgotten here: it doesn't come back.
    Forgotten,
}

/// How a bundle is imported.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImportOptions {
    /// Only count what it would do, without writing (`created`: what would be new; `updated`: what is already there
    /// and would be brought up to date).
    pub dry_run: bool,
    /// Outside an agent's shell: a person's `verified` can set the verified stamp
    /// (importing is the person's action). Without this, those memories come in without a stamp and are counted in
    /// `not_verified`.
    pub verify: bool,
    /// `--trust-verified`: a person's `verified` from a bundle that didn't come from this database also
    /// sets the stamp. Without this, only that of what was signed here (see `signature`).
    pub trust_verified: bool,
}

/// Imports a bundle already read, without trusting the `verified` of someone else's bundle (see [`import_with`]).
pub fn import(db: &mut Connection, model: Option<&Model>, b: &Bundle, destination: &Destination, dry_run: bool, verify: bool) -> Result<ImportSummary> {
    import_with(db, model, b, destination, ImportOptions { dry_run, verify, trust_verified: false })
}

/// Imports a bundle already read.
///
/// What gives a concept power (`omoikane.scope: global`, `pinned`, a person's `verified`, its
/// id and its key —which memory here it joins— and its `project_key`) is only trusted if it came from THIS
/// database: it carries its signature, which also covers its text (see `signature`). From someone else's bundle (or from one
/// of our own touched up, also in the text) it goes to the requested destination as a new memory, by the concept's
/// key, unpinned and without a stamp (the stamp, with `trust_verified`, yes). What ends up as a
/// trade memory goes through `trades::leaks`. And even with the signature, the stamp only comes back if the text is the one that was
/// verified (`omoikane.verified_sha`), the person didn't unverify it here afterwards and the date isn't in the
/// future. What was forgotten here (in the trash or with its forget mark) doesn't come back: it is skipped and
/// counted in `skipped_forgotten` (as in sync, where a forget stops the incoming memory;
/// to bring it back, `omoi restore`).
pub fn import_with(db: &mut Connection, model: Option<&Model>, b: &Bundle, destination: &Destination, op: ImportOptions) -> Result<ImportSummary> {
    if let (Destination::Project(_), Some(s)) = (destination, space_of(b)) {
        bail!(tr!(
            "this bundle was exported from the space «{s}»: import it with --space <name>, so it doesn't mix with personal memories",
            "este bundle se exportó del espacio «{s}»: impórtalo con --space <nombre>, para que no se mezcle con lo personal"
        ));
    }
    // Personal memories exported from Omoikane don't go into a client's project (one in a space); someone
    // else's bundle (the `knowledge/` of their repository) does.
    if let Destination::Project(p) = destination
        && b.concepts.iter().any(|c| c.header.contains_key("omoikane"))
        && let Some((_, Some(_))) = crate::project::resolve(db, p)?
    {
        bail!(tr!(
            "these are personal memories exported from Omoikane and this folder's project is in a space (a client's): import them from a personal project, or into a space with --space <name>",
            "son memorias personales exportadas de Omoikane y el proyecto de esta carpeta está en un espacio (de un cliente): impórtalas desde un proyecto personal, o a un espacio con --space <nombre>"
        ));
    }
    let key = signature::key(db, false)?;
    // The group overviews of an Omoikane export aren't memories.
    let concepts: Vec<&Concept> = b.concepts.iter().filter(|c| !is_overview(c)).collect();
    let mut r = ImportSummary { concepts: concepts.len(), ..ImportSummary::default() };
    // Concept id (lowercase) → memory id, whether it's from this database and whether it was skipped as older than
    // its memory here (`None`: two concepts with the same path except for case, so it's unknown which one
    // it is), and the relations each one carries.
    let mut ids: HashMap<String, Option<(String, bool, bool)>> = HashMap::new();
    // The memories this import has already touched.
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut relations: Vec<(String, bool, Vec<String>, Vec<String>)> = Vec::new();
    for c in concepts {
        match import_one(db, model, c, destination, op, key.as_deref(), &mut touched) {
            Ok((u, e)) => {
                match u.outcome {
                    Outcome::Created => r.created += 1,
                    Outcome::Changed => r.updated += 1,
                    Outcome::Same => r.unchanged += 1,
                    Outcome::Forgotten => {
                        r.skipped_forgotten += 1;
                        continue;
                    }
                }
                match u.stamp {
                    Stamp::Set => r.verified += 1,
                    Stamp::Agent => r.not_verified += 1,
                    Stamp::Foreign => r.untrusted_verified += 1,
                    Stamp::Stale => r.stale_verified += 1,
                    Stamp::Absent | Stamp::Already => {}
                }
                r.foreign += usize::from(u.foreign);
                r.ignored_global += usize::from(u.ignored_global);
                r.ignored_pinned += usize::from(u.ignored_pinned);
                r.global += usize::from(u.global);
                r.pinned += usize::from(e.memory.pinned);
                if u.redacted {
                    r.redacted.push(e.memory.title.clone());
                }
                r.skipped_older += usize::from(u.older);
                if let Some(id) = u.id {
                    ids.entry(e.concept.to_lowercase()).and_modify(|x| *x = None).or_insert_with(|| Some((id.clone(), u.own, u.older)));
                    // The relations of one that wasn't applied (older than the memory here), no.
                    if !u.older {
                        relations.push((id, u.own, e.superseded_by, e.contradicts));
                    }
                }
            }
            Err(err) => {
                eprintln!("omoikane: {}: {err:#}", c.id);
                r.failed += 1;
            }
        }
    }
    if op.dry_run {
        return Ok(r);
    }
    // The verdicts among what was imported: `superseded_by` and `contradicts` point to other concepts of the
    // bundle (`/path.md`). One that already holds isn't set again.
    let current = verdicts::pairs(db)?;
    let id_of = |path: &str| -> Option<&(String, bool, bool)> {
        let r = path.trim().trim_start_matches('/');
        let r = r.strip_suffix(".md").unwrap_or(r);
        ids.get(&nfc(r).to_lowercase()).and_then(Option::as_ref)
    };
    for (id, own, superseded_by, contradicts) in &relations {
        for (relation, others) in [(verdicts::SUPERSEDED, superseded_by), (verdicts::CONTRADICTS, contradicts)] {
            for (other, other_own, other_older) in others.iter().filter_map(|o| id_of(o)) {
                // Never between a concept of this database and one without its signature: a foreign one can't mark
                // a memory from here as superseded (nor contradict it), nor the other way round with what one of our own concepts
                // copied along with others from outside says. Nor towards one that wasn't applied for being old.
                if other == id || other_own != own || *other_older {
                    continue;
                }
                let already = current.get(&verdicts::pair_key(id, other)).is_some_and(|v| v.relation == relation && (relation == verdicts::CONTRADICTS || v.memory_id == *id));
                if already {
                    continue;
                }
                match verdicts::set(db, id, other, relation, "okf import", "import:okf") {
                    Ok(()) => r.verdicts += 1,
                    Err(err) => eprintln!("omoikane: {id}: {err:#}"),
                }
            }
        }
    }
    Ok(r)
}

/// What happened to a concept's person `verified`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    /// It didn't carry one.
    Absent,
    /// It was set (dry run: it would be set).
    Set,
    /// It already had that same one.
    Already,
    /// No, from an agent's shell.
    Agent,
    /// No, from a bundle that isn't from this database (without `trust_verified`).
    Foreign,
    /// No, because it no longer holds (another text, unverified afterwards, date in the future).
    Stale,
}

/// What happened to a concept when importing it.
struct ConceptResult {
    /// Its memory's id (dry run: the one already there, if there is one).
    id: Option<String>,
    outcome: Outcome,
    /// Whether something that looked like a secret was covered.
    redacted: bool,
    stamp: Stamp,
    /// Without this database's signature.
    foreign: bool,
    /// It asked to be global (or pinned) and was ignored for being someone else's.
    ignored_global: bool,
    ignored_pinned: bool,
    /// It comes in as global.
    global: bool,
    /// It carries this database's signature.
    own: bool,
    /// It's from this database but older than the memory here (its `updated` is earlier): it doesn't touch it.
    older: bool,
}

/// Error if the entry leaves (or creates) a trade memory —global, with `trade:`— with something from a
/// project or a client (`trades::leaks`): in its text, its paths, its tags, its key or its
/// data. `global` is whether the memory stays global; `before`, the one being updated (its tags and paths
/// are merged with the new ones). What that one already had while being a trade memory (saved maybe with `allow_leaks`)
/// doesn't stop it again: only what's new.
fn check_trade(db: &Connection, e: &Entry, global: bool, before: Option<&str>, key: &str) -> Result<()> {
    if !global {
        return Ok(());
    }
    let old = match before {
        Some(id) => Some(memory::get(db, id)?),
        None => None,
    };
    let mut tags: Vec<String> = e.memory.tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).collect();
    let mut paths = e.memory.paths.clone();
    if let Some(v) = &old {
        tags.extend(v.tags.iter().cloned());
        paths.extend(v.paths.iter().cloned());
    }
    if !crate::trades::has_trade(&tags) {
        return Ok(());
    }
    let joined_paths = paths.join(" ");
    let fields = crate::trades::leak_fields(&[("title", &e.memory.title), ("content", &e.memory.content), ("paths", &joined_paths)], &tags, Some(key), e.data.as_ref());
    let mut leaks = crate::trades::leaks_in(db, &fields)?;
    if let Some(v) = old.filter(|v| v.global() && crate::trades::has_trade(&v.tags)) {
        let data = memory::data_of(db, &[v.id.as_str()])?.remove(&v.id);
        let joined_paths = v.paths.join(" ");
        let already = crate::trades::leak_fields(&[("title", &v.title), ("content", &v.content), ("paths", &joined_paths)], &v.tags, memory::key_of(&v), data.as_ref());
        let already: BTreeSet<String> = crate::trades::leaks_in(db, &already)?.into_iter().map(|f| f.text.to_lowercase()).collect();
        leaks.retain(|f| !already.contains(&f.text.to_lowercase()));
    }
    if leaks.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = leaks.iter().map(|f| if f.field.is_empty() { format!("{} «{}»", f.kind, f.text) } else { format!("{} «{}» ({})", f.kind, f.text, f.field) }).collect();
    bail!(tr!(
        "it would be a trade memory (global with trade:), which reaches every project of that trade of every client, and it names what looks like a project or a client: {}. Remove it from the concept, or import it into a project instead of --global",
        "sería una memoria de oficio (global con trade:), que llega a todos los proyectos de ese oficio de todos los clientes, y nombra algo que parece de un proyecto o de un cliente: {}. Quítalo del concepto, o impórtalo en un proyecto en vez de con --global",
        list.join(", ")
    ))
}

/// Whether the memory `id` (the full id, in its form) is alive here.
fn is_alive(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM memory WHERE id = ?1)", [id], |f| f.get(0))?)
}

/// Whether `id` was forgotten here: it has its forget mark or is in the trash.
fn is_forgotten(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM tombstone WHERE id = ?1) OR EXISTS(SELECT 1 FROM forgotten WHERE id = ?1)", [id], |f| f.get(0))?)
}

/// Whether the instant `a` is later than `b` (two ISO 8601, with their zone if they say it). `false` if either
/// can't be understood.
fn is_after(db: &Connection, a: &str, b: &str) -> Result<bool> {
    let x: Option<bool> = db.query_row("SELECT julianday(?1) > julianday(?2)", params![sql_instant(a), sql_instant(b)], |f| f.get(0))?;
    Ok(x == Some(true))
}

/// Imports a concept: what happened and its entry.
fn import_one(db: &mut Connection, model: Option<&Model>, c: &Concept, destination: &Destination, op: ImportOptions, key: Option<&[u8]>, touched: &mut BTreeSet<String>) -> Result<(ConceptResult, Entry)> {
    let mut e = to_entry(c)?;
    // Did it come from this database? Its signature covers everything applied from it (see `signature` and `signed_text`):
    // a field changed, removed or added leaves it unsigned.
    let own = match (key, &e.signature, &e.id) {
        (Some(k), Some(f), Some(_)) => signature::is_valid(k, &signed_text(&e), f),
        _ => false,
    };
    let mut u = ConceptResult { id: None, outcome: Outcome::Same, redacted: false, stamp: Stamp::Absent, foreign: !own, ignored_global: false, ignored_pinned: false, global: false, own, older: false };
    if !own {
        // Someone else's: neither global nor pinned; it goes to the requested destination like any concept.
        if e.global {
            u.ignored_global = matches!(destination, Destination::Project(p) if !p.is_empty());
            e.global = false;
        }
        if e.memory.pinned {
            u.ignored_pinned = true;
            e.memory.pinned = false;
        }
        // And nothing its `omoikane:` says decides which memory here it joins or where it goes: not
        // its id (with that of a pinned global, from another project or a trade memory, it rewrote it wherever
        // it was), nor its key, nor its project's key (it sent it to a project other than the
        // requested one). It is saved with a new id by the concept's key (`okf:<path>`) in the destination:
        // importing it again brings it up to date.
        e.id = None;
        e.own_key = None;
        e.project_key = None;
        e.key = concept_key(&c.id, &Map::new());
    }
    // A verification earlier than `generated.at` is of a text that is no longer this one (OKF considers it
    // superseded): it doesn't set the stamp.
    if let Some((_, Some(at))) = &e.verified
        && let Some(gen_at) = &e.generated
        && is_after(db, gen_at, at)?
    {
        e.verified = None;
    }
    // What was forgotten here doesn't come back with its id.
    if let Some(id) = &e.id
        && !is_alive(db, id)?
        && is_forgotten(db, id)?
    {
        u.outcome = Outcome::Forgotten;
        return Ok((u, e));
    }
    // The data, checked before writing anything: if it isn't valid, the concept fails without leaving a
    // half-written memory.
    if let Some(d) = &e.data {
        memory::check_data(d)?;
    }
    let (project, project_id, space_id) = placement(db, destination, &e)?;
    e.memory.project = project.clone();
    e.memory.project_id = project_id;
    e.memory.space_id = space_id;
    // The same memory (only from a concept signed here: someone else's no longer carries an id), if this database still
    // has it in the same space. Wherever it is now: if it was moved after exporting it (a global one that
    // was taken to a project), it is updated there, without moving it again or creating another by its key; what
    // was done here afterwards is newer than the bundle.
    let own_id = match &e.id {
        Some(id) if is_alive(db, id)? => {
            let (_, space) = memory::memory_placement(db, id)?.unwrap_or((false, String::new()));
            let target_space = crate::project::space_key(db, space_id)?;
            (space == target_space).then(|| id.clone())
        }
        _ => None,
    };
    // Global: what asks to be (and is trusted), everything if the destination is the global (`--global`), or the one that is already
    // global and gets updated where it is.
    u.global = match &own_id {
        Some(id) => memory::memory_placement(db, id)?.is_some_and(|(g, _)| g),
        None => space_id.is_none() && project_id.is_none() && project.is_empty(),
    };
    // How it is saved: updating the same memory; by its key, if there's already one with it where it lands;
    // with its same id, if it came from Omoikane and this database doesn't have it; and otherwise, by the concept's key.
    // Two concepts of the same import never go to the same memory (memories of two projects with
    // the same key that land in a single one): the second one doesn't keep the key.
    let mut by_own_key = match &e.own_key {
        Some(k) => find_by_key(db, &project, project_id, space_id, k)?.map(|x| (k.clone(), x)),
        None => None,
    };
    if by_own_key.as_ref().is_some_and(|(_, (id, _))| touched.contains(id)) {
        by_own_key = None;
        e.own_key = None;
        e.key = match &e.id {
            Some(i) => format!("okf:{}", i.to_lowercase()),
            None => concept_key(&c.id, &Map::new()),
        };
    }
    if e.own_key.is_none() && find_by_key(db, &project, project_id, space_id, &e.key)?.is_some_and(|(id, _)| touched.contains(&id)) {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(c.id.as_bytes());
        let base: String = e.key.chars().take(71).collect();
        e.key = format!("{base}-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3]);
    }
    let with_own_id = match e.id.clone() {
        Some(id) if own_id.is_none() && by_own_key.is_none() && !is_alive(db, &id)? => Some(id),
        _ => None,
    };
    let before = match (&own_id, &by_own_key) {
        (Some(id), _) => Some((id.clone(), current_hlc(db, id)?)),
        (None, Some((_, x))) => Some(x.clone()),
        (None, None) if with_own_id.is_some() => None,
        (None, None) => find_by_key(db, &project, project_id, space_id, &e.key)?,
    };
    // Going back: a concept of this database older than the memory here (its `updated`, signed, is
    // earlier than the memory's: a bundle exported before changing it) doesn't touch it. Not its text, nor its
    // tags, paths or data, nor its verdicts, nor the stamp: it would take the memory back to how it was then and
    // would verify it with the date from then.
    if own && let Some((id, _)) = &before {
        let here_updated: String = db.query_row("SELECT updated_at FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let older = match &e.updated {
            Some(c) => is_after(db, &here_updated, c)?,
            None => true,
        };
        if older {
            u.older = true;
            u.outcome = Outcome::Same;
            u.id = Some(id.clone());
            touched.insert(id.clone());
            return Ok((u, e));
        }
    }
    // Whether a person's `verified` can set the stamp.
    u.stamp = match &e.verified {
        None => Stamp::Absent,
        Some(_) if !op.verify => Stamp::Agent,
        Some(_) if !own && !op.trust_verified => Stamp::Foreign,
        Some((_, at)) => {
            let kind = e.memory.kind_text.clone().unwrap_or_else(|| e.memory.kind.unwrap_or(Kind::Fact).as_str().to_string());
            let other_text = e.verified_sha.as_deref().is_some_and(|s| s != crate::verified::fingerprint(&kind, &e.memory.title, &e.memory.content));
            // The person unverified it here after that verification (reimporting an old bundle).
            let unverified_later = match (&before, at) {
                (Some((id, _)), Some(at)) => match crate::verified::unverified_at(db, id)? {
                    Some(d) => is_after(db, &d, at)?,
                    None => false,
                },
                _ => false,
            };
            let future = match at {
                Some(at) => db.query_row("SELECT julianday(?1) > julianday('now', '+1 day')", [sql_instant(at)], |f| f.get::<_, Option<bool>>(0))? == Some(true),
                None => false,
            };
            if other_text || unverified_later || future { Stamp::Stale } else { Stamp::Set }
        }
    };
    // A trade memory created or changed on import goes through `trades::leaks`, as in
    // `remember` and `update` (without `allow_leaks`: whatever it flags is fixed in the bundle or imported into a
    // project). Before writing anything, also in a dry run.
    check_trade(db, &e, u.global, before.as_ref().map(|(id, _)| id.as_str()), by_own_key.as_ref().map_or(e.key.as_str(), |(k, _)| k.as_str()))?;
    if op.dry_run {
        u.outcome = if before.is_some() { Outcome::Changed } else { Outcome::Created };
        u.id = before.map(|(id, _)| id);
        return Ok((u, e));
    }
    // Whatever looks like a secret is covered by `memory` when saving (also in the data); here it's only
    // noted to report it.
    let secret = crate::secrets::enabled() && !crate::secrets::findings(&format!("{}\n{}", e.memory.title, e.memory.content)).is_empty();
    let (id, created, redactions) = match &own_id {
        Some(id) => {
            let m = memory::get(db, id)?;
            let mut tags = m.tags.clone();
            tags.extend(e.memory.tags.iter().cloned());
            let mut paths = m.paths.clone();
            paths.extend(e.memory.paths.iter().cloned());
            let changes = Changes {
                title: Some(e.memory.title.clone()),
                content: Some(e.memory.content.clone()),
                kind: e.memory.kind,
                paths: Some(paths),
                tags: Some(tags),
                pinned: e.memory.pinned.then_some(true),
                reason: Some("okf import".into()),
                ..Default::default()
            };
            let m = memory::update(db, id, changes, model)?;
            let mut redactions = m.redacted.clone();
            if e.data.is_some() {
                redactions.extend(memory::set_data(db, id, e.data.clone())?);
            }
            (m.id, false, redactions)
        }
        None if with_own_id.is_some() => {
            let mut memory = e.memory.clone();
            if let Some(k) = &e.own_key {
                memory.tags.push(format!("{}{k}", memory::TOPIC_TAG));
            }
            let m = memory::save_with_its_id(db, with_own_id.as_deref().unwrap_or_default(), memory, model)?;
            let mut redactions = m.redacted.clone();
            if e.data.is_some() {
                redactions.extend(memory::set_data(db, &m.id, e.data.clone())?);
            }
            (m.id, true, redactions)
        }
        None => {
            let key = by_own_key.as_ref().map_or(e.key.as_str(), |(k, _)| k.as_str());
            let p = memory::save_by_key(db, key, e.memory.clone(), e.data.clone(), model)?;
            let mut redactions = p.memory.redacted.clone();
            redactions.extend(p.redacted);
            (p.memory.id, p.created, redactions)
        }
    };
    if created {
        // The source dates, if they make sense.
        for (column, value) in [("created_at", &e.created), ("updated_at", &e.updated)] {
            if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
                db.execute(
                    &format!("UPDATE memory SET {column} = strftime('%Y-%m-%dT%H:%M:%fZ', ?1) WHERE id = ?2 AND strftime('%Y-%m-%dT%H:%M:%fZ', ?1) IS NOT NULL"),
                    params![sql_instant(v), id],
                )?;
            }
        }
    }
    // The evidence, also on the one that already existed (with its key or its id): it is added to what it had.
    if !e.memory.evidence.is_empty() {
        memory::add_evidence(db, &id, &e.memory.evidence)?;
    }
    // And what to check when its review date arrives (1.14).
    if let Some(r) = &e.memory.review_reason {
        memory::set_review_reason(db, &id, r)?;
    }
    // The stamp only comes back if the text that was verified (`verified_sha`) is the one the memory has NOW,
    // already written (saving it may have covered a secret, or the memory it went to had another text).
    if u.stamp == Stamp::Set
        && let Some(sha) = &e.verified_sha
    {
        let m = memory::get(db, &id)?;
        if crate::verified::fingerprint(m.kind_str(), &m.title, &m.content) != *sha {
            u.stamp = Stamp::Stale;
        }
    }
    // The verified stamp, after the text (changing it removes any existing one) and with the original date.
    // If it already has that same one, it's left alone.
    if u.stamp == Stamp::Set
        && let Some((by, at)) = &e.verified
    {
        u.stamp = match crate::verified::verify_at(db, &id, by, at.as_deref()) {
            Ok((_, true)) => Stamp::Set,
            Ok((_, false)) => Stamp::Already,
            Err(err) => {
                eprintln!("omoikane: {}: {err:#}", c.id);
                Stamp::Stale
            }
        };
    }
    u.outcome = match before {
        None => Outcome::Created,
        Some((_, h)) if current_hlc(db, &id)? != h => Outcome::Changed,
        Some(_) => Outcome::Same,
    };
    touched.insert(id.clone());
    u.id = Some(id);
    u.redacted = secret || !redactions.is_empty();
    Ok((u, e))
}

// --- YAML ------------------------------------------------------------------------------------------

/// The YAML that OKF headers use: maps and lists by indentation, `[…]` and `{…}` (also over
/// several lines), single- or double-quoted strings, `|` and `>` blocks, comments, numbers,
/// `true`/`false` and `null`. No anchors, aliases or `!!` tags: a clear error if they appear. No
/// dependencies: what is read stays in `serde_json::Value`.
pub mod yaml {
    use serde_json::{Map, Number, Value};

    struct Line {
        indent: usize,
        text: String,
        n: usize,
    }

    struct Reader {
        lines: Vec<Line>,
        raw: Vec<String>,
        i: usize,
        /// How many maps and lists are open (see [`MAX_DEPTH`]).
        depth: usize,
    }

    /// How many nested maps and lists are read at most: a real header doesn't go past three or
    /// four, and without a limit one of `x: [[[[…` (or of ever more indented maps) overflows the stack and
    /// takes the process down.
    pub const MAX_DEPTH: usize = 64;

    fn too_deep() -> String {
        format!("nested deeper than {MAX_DEPTH} levels")
    }

    /// Reads a header (what is between the `---`) as a map.
    pub fn parse(text: &str) -> Result<Map<String, Value>, String> {
        let raw: Vec<String> = text.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
        let mut lines = Vec::new();
        for (n, l) in raw.iter().enumerate() {
            let stripped = l.trim_start_matches(' ');
            if stripped.starts_with('\t') {
                return Err(format!("line {}: tabs can't indent YAML", n + 1));
            }
            if stripped.is_empty() || stripped.starts_with('#') {
                continue;
            }
            if n == 0 && stripped.starts_with('%') {
                return Err(format!("line {}: YAML directives are not supported", n + 1));
            }
            lines.push(Line { indent: l.len() - stripped.len(), text: stripped.to_string(), n });
        }
        let mut l = Reader { lines, raw, i: 0, depth: 0 };
        if l.lines.is_empty() {
            return Ok(Map::new());
        }
        let s = l.lines[0].indent;
        if is_item(&l.lines[0].text) {
            return Err("the header is a list, not keys and values".into());
        }
        let m = l.mapping(s)?;
        if let Some(x) = l.lines.get(l.i) {
            return Err(format!("line {}: unexpected indentation", x.n + 1));
        }
        Ok(m)
    }

    fn is_item(t: &str) -> bool {
        t == "-" || t.starts_with("- ")
    }

    impl Reader {
        fn mapping(&mut self, indent: usize) -> Result<Map<String, Value>, String> {
            self.depth += 1;
            let r = self.mapping_inner(indent);
            self.depth -= 1;
            r
        }

        fn mapping_inner(&mut self, indent: usize) -> Result<Map<String, Value>, String> {
            if self.depth > MAX_DEPTH {
                return Err(too_deep());
            }
            let mut m = Map::new();
            while let Some(l) = self.lines.get(self.i) {
                if l.indent < indent || (l.indent == indent && is_item(&l.text)) {
                    break;
                }
                if l.indent > indent {
                    return Err(format!("line {}: unexpected indentation", l.n + 1));
                }
                let n = l.n;
                let text = l.text.clone();
                let (key, rest) = split_key(&text).ok_or_else(|| format!("line {}: expected `key: value`", n + 1))?;
                self.i += 1;
                let v = self.value(indent, rest.trim(), n)?;
                m.insert(key, v);
            }
            Ok(m)
        }

        fn list(&mut self, indent: usize) -> Result<Vec<Value>, String> {
            self.depth += 1;
            let r = self.list_inner(indent);
            self.depth -= 1;
            r
        }

        fn list_inner(&mut self, indent: usize) -> Result<Vec<Value>, String> {
            if self.depth > MAX_DEPTH {
                return Err(too_deep());
            }
            let mut v = Vec::new();
            while let Some(l) = self.lines.get(self.i) {
                if l.indent != indent || !is_item(&l.text) {
                    if l.indent > indent {
                        return Err(format!("line {}: unexpected indentation", l.n + 1));
                    }
                    break;
                }
                let n = l.n;
                let rest = l.text[1..].to_string();
                let inner = rest.len() - rest.trim_start().len();
                let item = rest.trim().to_string();
                if !item.is_empty() && (is_item(&item) || (split_key(&item).is_some() && !item.starts_with(['[', '{', '"', '\'']))) {
                    // `- key: value` (or `- - x`): what comes after the dash is the start of a map (or
                    // a list) with the indentation of where it starts.
                    let s = indent + 1 + inner;
                    self.lines[self.i] = Line { indent: s, text: item.clone(), n };
                    v.push(if is_item(&item) { Value::Array(self.list(s)?) } else { Value::Object(self.mapping(s)?) });
                } else {
                    self.i += 1;
                    v.push(self.value(indent, &item, n)?);
                }
            }
            Ok(v)
        }

        /// The value of a key (or of an item) on line `n` with indentation `indent`: whatever comes
        /// after, or whatever comes below.
        fn value(&mut self, indent: usize, rest: &str, n: usize) -> Result<Value, String> {
            let rest = strip_comment(rest);
            if rest.is_empty() {
                return Ok(match self.lines.get(self.i) {
                    Some(l) if l.indent > indent => {
                        let s = l.indent;
                        if is_item(&l.text) { Value::Array(self.list(s)?) } else { Value::Object(self.mapping(s)?) }
                    }
                    Some(l) if l.indent == indent && is_item(&l.text) => Value::Array(self.list(indent)?),
                    _ => Value::Null,
                });
            }
            if rest.starts_with('|') || rest.starts_with('>') {
                return Ok(Value::String(self.block_scalar(indent, rest, n)?));
            }
            if rest.starts_with('[') || rest.starts_with('{') {
                // A `[…]` or `{…}` can continue on the lines below.
                // What is open is tracked line by line (`Closing`): looking again at everything gathered on each
                // line was quadratic (a 400 KB header in a single list took 15 s).
                let mut all_text = rest.to_string();
                let mut closing = Closing::default();
                closing.feed(&all_text);
                while !closing.is_closed() {
                    let Some(l) = self.lines.get(self.i) else { return Err(format!("line {}: unclosed {}", n + 1, &rest[..1])) };
                    let more = strip_comment(&l.text);
                    all_text.push(' ');
                    all_text.push_str(more);
                    closing.feed(" ");
                    closing.feed(more);
                    self.i += 1;
                }
                let mut p = Flow { c: all_text.chars().collect(), i: 0, depth: 0 };
                let v = p.value().map_err(|e| format!("line {}: {e}", n + 1))?;
                p.skip_spaces();
                if p.i < p.c.len() {
                    return Err(format!("line {}: unexpected text after the closing bracket", n + 1));
                }
                return Ok(v);
            }
            if rest.starts_with('"') || rest.starts_with('\'') {
                let mut p = Flow { c: rest.chars().collect(), i: 0, depth: 0 };
                let v = p.quoted().map_err(|e| format!("line {}: {e}", n + 1))?;
                p.skip_spaces();
                if p.i < p.c.len() {
                    return Err(format!("line {}: unexpected text after the closing quote", n + 1));
                }
                return Ok(Value::String(v));
            }
            if rest.starts_with(['&', '*', '!']) {
                return Err(format!("line {}: YAML anchors, aliases and tags are not supported", n + 1));
            }
            // An unquoted string can continue on the lines below, more indented.
            let mut text = rest.to_string();
            while let Some(l) = self.lines.get(self.i) {
                if l.indent <= indent {
                    break;
                }
                if is_item(&l.text) || split_key(&l.text).is_some() {
                    return Err(format!("line {}: unexpected indentation", l.n + 1));
                }
                text.push(' ');
                text.push_str(strip_comment(&l.text));
                self.i += 1;
            }
            Ok(plain(&text))
        }

        /// A `|` block (the lines as is) or `>` block (joined with spaces), with `-` or `+` at the end.
        fn block_scalar(&mut self, indent: usize, head: &str, n: usize) -> Result<String, String> {
            let literal = head.starts_with('|');
            let strip = head.contains('-');
            let keep = head.contains('+');
            // The raw lines up to the next one with indentation <= the key's.
            let end = self.lines.get(self.i).filter(|l| l.indent <= indent).map(|l| l.n).or_else(|| {
                let mut j = self.i;
                while self.lines.get(j).is_some_and(|l| l.indent > indent) {
                    j += 1;
                }
                self.lines.get(j).map(|l| l.n)
            });
            let end = end.unwrap_or(self.raw.len());
            while self.lines.get(self.i).is_some_and(|l| l.n < end) {
                self.i += 1;
            }
            let raw: Vec<&str> = self.raw[n + 1..end].iter().map(String::as_str).collect();
            let inner = raw.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).min().unwrap_or(indent + 2);
            // Only ASCII spaces are removed (each one, one byte): cutting at byte `inner` of a line
            // that starts with another space (`\u{a0}`, multi-byte) would split a character.
            let parts: Vec<String> = raw
                .iter()
                .map(|l| {
                    let spaces = l.len() - l.trim_start_matches(' ').len();
                    match l.get(spaces.min(inner)..) {
                        Some(r) if !l.trim().is_empty() || spaces == l.len() => r.to_string(),
                        _ => String::new(),
                    }
                })
                .collect();
            let mut s = if literal {
                parts.join("\n")
            } else {
                let mut r = String::new();
                for p in &parts {
                    if p.is_empty() {
                        r.push('\n');
                    } else {
                        if !r.is_empty() && !r.ends_with('\n') {
                            r.push(' ');
                        }
                        r.push_str(p);
                    }
                }
                r
            };
            if !keep {
                s = s.trim_end_matches('\n').to_string();
                if !strip {
                    s.push('\n');
                }
            }
            Ok(s)
        }
    }

    /// `key: rest` (or `"key": rest`), if the line starts like that.
    fn split_key(t: &str) -> Option<(String, &str)> {
        if t.starts_with('"') || t.starts_with('\'') {
            let mut p = Flow { c: t.chars().collect(), i: 0, depth: 0 };
            let k = p.quoted().ok()?;
            let used: usize = p.c[..p.i].iter().map(|c| c.len_utf8()).sum();
            let rest = t[used..].trim_start().strip_prefix(':')?;
            return (rest.is_empty() || rest.starts_with(' ')).then_some((k, rest));
        }
        if t.starts_with(['[', '{', '-', '#', '?']) && !t.starts_with("-") || t.starts_with("- ") {
            return None;
        }
        let i = t.find(": ").or_else(|| t.strip_suffix(':').map(|s| s.len()))?;
        let k = t[..i].trim();
        if k.is_empty() || k.contains(" #") {
            return None;
        }
        Some((k.to_string(), &t[i + 1..]))
    }

    /// Removes a comment (` #…`) outside quotes.
    fn strip_comment(t: &str) -> &str {
        let mut quote: Option<char> = None;
        let mut prev = ' ';
        let mut escaped = false;
        for (i, c) in t.char_indices() {
            match quote {
                Some('"') if escaped => escaped = false,
                Some('"') if c == '\\' => escaped = true,
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => {
                    if prev == ' ' || prev == '[' || prev == '{' || prev == ',' || prev == ':' || i == 0 {
                        quote = Some(c)
                    }
                }
                None if c == '#' && (i == 0 || prev == ' ') => return t[..i].trim_end(),
                None => {}
            }
            prev = c;
        }
        t.trim_end()
    }

    /// How many `[` and `{` outside quotes are still open, looking at the text in pieces (each
    /// piece, only once).
    #[derive(Default)]
    struct Closing {
        level: i64,
        quote: Option<char>,
        escaped: bool,
    }

    impl Closing {
        fn feed(&mut self, t: &str) {
            for c in t.chars() {
                match self.quote {
                    Some('"') if self.escaped => self.escaped = false,
                    Some('"') if c == '\\' => self.escaped = true,
                    Some(q) if c == q => self.quote = None,
                    Some(_) => {}
                    None => match c {
                        '"' | '\'' => self.quote = Some(c),
                        '[' | '{' => self.level += 1,
                        ']' | '}' => self.level -= 1,
                        _ => {}
                    },
                }
            }
        }

        /// Whether the `[` and `{` outside quotes are closed.
        fn is_closed(&self) -> bool {
            self.level <= 0 && self.quote.is_none()
        }
    }

    /// An unquoted value: `null`, `true`/`false`, a number or a string.
    fn plain(t: &str) -> Value {
        let t = t.trim();
        match t {
            "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
            "true" | "True" | "TRUE" => return Value::Bool(true),
            "false" | "False" | "FALSE" => return Value::Bool(false),
            _ => {}
        }
        let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
        if !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && !(digits.len() > 1 && digits.starts_with('0'))
            && let Ok(n) = t.parse::<i64>()
        {
            return Value::Number(n.into());
        }
        let is_decimal = {
            let (m, e) = match digits.find(['e', 'E']) {
                Some(i) => (&digits[..i], Some(&digits[i + 1..])),
                None => (digits, None),
            };
            let parts: Vec<&str> = m.split('.').collect();
            parts.len() == 2
                && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
                && parts.iter().any(|p| !p.is_empty())
                && e.is_none_or(|e| {
                    let e = e.strip_prefix(['-', '+']).unwrap_or(e);
                    !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
                })
        };
        if is_decimal && let Some(n) = t.parse::<f64>().ok().and_then(Number::from_f64) {
            return Value::Number(n);
        }
        Value::String(t.to_string())
    }

    struct Flow {
        c: Vec<char>,
        i: usize,
        /// How many `[` and `{` are open (see [`MAX_DEPTH`]).
        depth: usize,
    }

    impl Flow {
        fn skip_spaces(&mut self) {
            while self.c.get(self.i).is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
        }

        fn value(&mut self) -> Result<Value, String> {
            self.depth += 1;
            let r = if self.depth > MAX_DEPTH { Err(too_deep()) } else { self.value_inner() };
            self.depth -= 1;
            r
        }

        fn value_inner(&mut self) -> Result<Value, String> {
            self.skip_spaces();
            match self.c.get(self.i) {
                Some('[') => {
                    self.i += 1;
                    let mut v = Vec::new();
                    loop {
                        self.skip_spaces();
                        match self.c.get(self.i) {
                            Some(']') => {
                                self.i += 1;
                                return Ok(Value::Array(v));
                            }
                            None => return Err("unclosed [".into()),
                            _ => {}
                        }
                        v.push(self.value()?);
                        self.skip_spaces();
                        match self.c.get(self.i) {
                            Some(',') => self.i += 1,
                            Some(']') => {}
                            _ => return Err("expected , or ] in a list".into()),
                        }
                    }
                }
                Some('{') => {
                    self.i += 1;
                    let mut m = Map::new();
                    loop {
                        self.skip_spaces();
                        match self.c.get(self.i) {
                            Some('}') => {
                                self.i += 1;
                                return Ok(Value::Object(m));
                            }
                            None => return Err("unclosed {".into()),
                            _ => {}
                        }
                        let k = match self.c.get(self.i) {
                            Some('"' | '\'') => self.quoted()?,
                            _ => self.plain_until(true),
                        };
                        self.skip_spaces();
                        if self.c.get(self.i) != Some(&':') {
                            return Err(format!("expected : after «{k}»"));
                        }
                        self.i += 1;
                        let v = self.value()?;
                        m.insert(k.trim().to_string(), v);
                        self.skip_spaces();
                        match self.c.get(self.i) {
                            Some(',') => self.i += 1,
                            Some('}') => {}
                            _ => return Err("expected , or } in a map".into()),
                        }
                    }
                }
                Some('"' | '\'') => Ok(Value::String(self.quoted()?)),
                Some('&' | '*' | '!') => Err("YAML anchors, aliases and tags are not supported".into()),
                _ => {
                    let t = self.plain_until(false);
                    Ok(plain(&t))
                }
            }
        }

        /// An unquoted string inside `[…]` or `{…}`: up to `,`, `]` or `}` (a key, also
        /// up to `: `).
        fn plain_until(&mut self, key: bool) -> String {
            let mut s = String::new();
            while let Some(&c) = self.c.get(self.i) {
                if matches!(c, ',' | ']' | '}') {
                    break;
                }
                if key && c == ':' && self.c.get(self.i + 1).is_none_or(|x| x.is_whitespace() || matches!(x, ',' | '}' | ']')) {
                    break;
                }
                s.push(c);
                self.i += 1;
            }
            s.trim().to_string()
        }

        fn quoted(&mut self) -> Result<String, String> {
            let q = self.c[self.i];
            self.i += 1;
            let mut s = String::new();
            loop {
                let Some(&c) = self.c.get(self.i) else { return Err("unclosed quote".into()) };
                self.i += 1;
                if q == '\'' {
                    if c == '\'' {
                        if self.c.get(self.i) == Some(&'\'') {
                            s.push('\'');
                            self.i += 1;
                            continue;
                        }
                        return Ok(s);
                    }
                    s.push(c);
                    continue;
                }
                match c {
                    '"' => return Ok(s),
                    '\\' => {
                        let Some(&e) = self.c.get(self.i) else { return Err("unclosed quote".into()) };
                        self.i += 1;
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            '0' => s.push('\0'),
                            '"' | '\\' | '/' | ' ' => s.push(e),
                            'u' | 'x' | 'U' => {
                                let n = match e {
                                    'x' => 2,
                                    'u' => 4,
                                    _ => 8,
                                };
                                let h: String = self.c.iter().skip(self.i).take(n).collect();
                                self.i += n;
                                let cp = u32::from_str_radix(&h, 16).map_err(|_| format!("bad escape \\{e}{h}"))?;
                                // A JSON surrogate pair (`😀`).
                                if (0xD800..0xDC00).contains(&cp) && self.c.get(self.i) == Some(&'\\') && self.c.get(self.i + 1) == Some(&'u') {
                                    let h2: String = self.c.iter().skip(self.i + 2).take(4).collect();
                                    if let Ok(low) = u32::from_str_radix(&h2, 16)
                                        && (0xDC00..0xE000).contains(&low)
                                    {
                                        self.i += 6;
                                        s.push(char::from_u32(0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00)).unwrap_or('\u{fffd}'));
                                        continue;
                                    }
                                }
                                s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                            other => return Err(format!("unknown escape \\{other}")),
                        }
                    }
                    _ => s.push(c),
                }
            }
        }
    }

    // --- Writing ---

    /// A string as a YAML value: as is if there's no doubt it reads the same, otherwise in double
    /// quotes (with JSON escapes, which YAML understands).
    pub fn scalar(v: &Value) -> String {
        match v {
            Value::String(s) => {
                let safe = !s.is_empty()
                    && s.trim() == s
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || " _./:+@-".contains(c))
                    && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                    && !s.contains(": ")
                    && !s.ends_with(':')
                    && plain(s) == Value::String(s.clone());
                if safe { s.clone() } else { serde_json::to_string(s).unwrap_or_default() }
            }
            Value::Array(_) | Value::Object(_) => flow(v),
            other => other.to_string(),
        }
    }

    /// A value on one line: `[a, b]`, `{ by: x, at: y }`.
    pub fn flow(v: &Value) -> String {
        match v {
            Value::Array(a) => format!("[{}]", a.iter().map(flow).collect::<Vec<_>>().join(", ")),
            Value::Object(o) if o.is_empty() => "{}".into(),
            Value::Object(o) => format!("{{ {} }}", o.iter().map(|(k, v)| format!("{}: {}", yaml_key(k), flow(v))).collect::<Vec<_>>().join(", ")),
            other => scalar(other),
        }
    }

    fn yaml_key(k: &str) -> String {
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !k.starts_with('-') {
            k.to_string()
        } else {
            serde_json::to_string(k).unwrap_or_default()
        }
    }

    /// `key: value` with the right indentation: maps, below; lists of maps, with `- `; the
    /// rest on the same line.
    pub fn block(s: &mut String, k: &str, v: &Value, indent: usize) {
        let pre = " ".repeat(indent);
        match v {
            Value::Object(o) if !o.is_empty() => {
                *s += &format!("{pre}{}:\n", yaml_key(k));
                for (k2, v2) in o {
                    block(s, k2, v2, indent + 2);
                }
            }
            Value::Array(a) if a.iter().any(|x| x.is_object() || x.is_array()) => {
                *s += &format!("{pre}{}:\n", yaml_key(k));
                for x in a {
                    *s += &format!("{pre}  - {}\n", flow(x));
                }
            }
            other => *s += &format!("{pre}{}: {}\n", yaml_key(k), scalar(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_from_okf() {
        let t = "type: Attested Computation\ntitle: \"Incident: freshness\"\ndescription: Steps to triage. # comentario\ntags: [sales, \"a b\", 2026]\n\
                 generated: { by: reference_agent/gemini-2.5-pro, at: 2026-05-28T14:30:00Z }\nverified:\n  - { by: human:ahormati, at: 2026-06-25T09:00:00Z }\n  - by: process:x\n    at: 2026-06-26T02:00:00Z\n\
                 parameters:\n- { name: year, type: integer, required: true }\nexecutor:\n  resource: references/skills/run-on-bq.md\n  receipt: [job_id,\n    executed_sql]\n\
                 nota: |\n  línea 1\n\n  línea 2\nplegado: >-\n  uno\n  dos\nvacio:\nversion: \"0.2\"\nnumero: 0.2\nsi: true\ncita: 'it''s'\nlargo: una frase\n  que sigue\n";
        let m = yaml::parse(t).unwrap();
        assert_eq!(m["type"], "Attested Computation");
        assert_eq!(m["title"], "Incident: freshness");
        assert_eq!(m["description"], "Steps to triage.");
        assert_eq!(m["tags"], json!(["sales", "a b", 2026]));
        assert_eq!(m["generated"]["by"], "reference_agent/gemini-2.5-pro");
        assert_eq!(m["generated"]["at"], "2026-05-28T14:30:00Z");
        assert_eq!(m["verified"][0]["by"], "human:ahormati");
        assert_eq!(m["verified"][1]["at"], "2026-06-26T02:00:00Z");
        assert_eq!(m["parameters"][0]["required"], true);
        assert_eq!(m["executor"]["receipt"], json!(["job_id", "executed_sql"]));
        assert_eq!(m["nota"], "línea 1\n\nlínea 2\n");
        assert_eq!(m["plegado"], "uno dos");
        assert_eq!(m["vacio"], Value::Null);
        assert_eq!(m["version"], "0.2");
        assert_eq!(m["si"], true);
        assert_eq!(m["cita"], "it's");
        assert_eq!(m["largo"], "una frase que sigue");
        assert!(yaml::parse("a: &x 1").is_err());
        assert!(yaml::parse("solo texto").is_err());
        assert!(yaml::parse("a: [1, 2").is_err());
        assert!(yaml::parse("a: 1\n    b: 2").is_err());
    }

    #[test]
    fn yaml_round_trip() {
        let v = json!({
            "type": "Decision",
            "title": "Usar: \"comillas\" y #almohadilla",
            "tags": ["a", "true", "1.5", "x y"],
            "n": 3,
            "verified": [{ "by": "human:marc", "at": "2026-11-08T10:00:00.123Z" }],
            "anidado": { "lista": [1, 2], "texto": "línea\nsegunda", "vacio": {} },
            "raro": "- empieza con guion",
            "nulo": null,
        });
        let mut s = String::new();
        for (k, x) in v.as_object().unwrap() {
            yaml::block(&mut s, k, x, 0);
        }
        let m = yaml::parse(&s).unwrap();
        assert_eq!(Value::Object(m), v, "{s}");
    }

    #[test]
    fn kinds_and_keys() {
        assert_eq!(kind_from_okf("Decision"), Kind::Decision);
        assert_eq!(kind_from_okf("Architecture Decision Record"), Kind::Decision);
        assert_eq!(kind_from_okf("Runbook"), Kind::Procedure);
        assert_eq!(kind_from_okf("Convention"), Kind::Preference);
        assert_eq!(kind_from_okf("BigQuery Table"), Kind::Fact);
        for t in Kind::ALL {
            assert_eq!(kind_from_okf(&okf_type_name(t.as_str())), t);
        }
        assert_eq!(concept_key("Architecture/Governance Model", &Map::new()), "okf:architecture/governance-model");
        let long_id = "a/".repeat(60);
        let k = concept_key(&long_id, &Map::new());
        assert!(k.chars().count() <= 80 && memory::topic_key(&k).is_ok(), "{k}");
        assert_eq!(day_of("2026-09-23T00:00:00Z").as_deref(), Some("2026-09-23"));
        assert_eq!(day_of("mañana"), None);
        assert_eq!(description_of("# Título\n\nPrimera frase. Segunda.", "t"), "Título");
        assert_eq!(description_of("Primera frase. Segunda.", "t"), "Primera frase.");
        assert_eq!(path_from_key("okf:architecture/governance-model").as_deref(), Some("architecture/governance-model"));
        assert_eq!(path_from_key("okf:01jabcdefghjkmnpqrstvwxyz0"), None);
        assert_eq!(actor("mcp:claude-code"), "claude-code/omoikane");
        assert_eq!(actor("cli"), "cli/omoikane");
        assert_eq!(actor_id("Marc Mayol 100%"), "Marc%20Mayol%20100%25");
        for n in ["Marc Mayol 100%", "a%20b", "%", "ana"] {
            assert_eq!(actor_name(&actor_id(n)), n);
        }
        assert_eq!(person_of("human:Marc%20Mayol").as_deref(), Some("Marc Mayol"));
        assert_eq!(person_of("Human/ana").as_deref(), Some("ana"));
        assert_eq!(person_of("human"), None);
        assert_eq!(person_of("human:"), None);
        assert_eq!(person_of("process:nightly"), None);
        assert_eq!(person_of("humano:x"), None);
    }

    #[test]
    fn split_header() {
        assert_eq!(split_front_matter("---\na: 1\n---\ncuerpo\n"), Some(("a: 1\n", "cuerpo\n")));
        assert_eq!(split_front_matter("\u{feff}---\r\na: 1\r\n---\r\nx"), Some(("a: 1\r\n", "x")));
        assert_eq!(split_front_matter("sin cabecera"), None);
    }

    /// A malicious header doesn't take the process down: neither by nesting endlessly (before, it overflowed the stack and
    /// aborted) nor with non-ASCII spaces in a block (before, it cut a character in half).
    #[test]
    fn malicious_yaml_errors_without_crashing() {
        // 200,000 `[` on one line.
        let deep = format!("x: {}{}", "[".repeat(200_000), "]".repeat(200_000));
        assert!(yaml::parse(&deep).unwrap_err().contains("nested deeper"));
        let deep = format!("x: {}1{}", "{a: ".repeat(100_000), "}".repeat(100_000));
        assert!(yaml::parse(&deep).unwrap_err().contains("nested deeper"));
        // Lists nested by indentation (`- - - … x`) and ever more indented maps.
        let lists = format!("x:\n  {}x\n", "- ".repeat(100_000));
        assert!(yaml::parse(&lists).unwrap_err().contains("nested deeper"));
        let mut maps = String::new();
        for i in 0..2_000 {
            maps += &format!("{}k{i}:\n", " ".repeat(i));
        }
        assert!(yaml::parse(&maps).unwrap_err().contains("nested deeper"));
        // The usual still works: 60 levels are fine.
        assert!(yaml::parse(&format!("x: {}1{}", "[".repeat(60), "]".repeat(60))).is_ok());
        // A block with a line of non-ASCII spaces (`\u{a0}`) shorter than its indentation.
        let m = yaml::parse("nota: |\n   uno\n  \u{a0}\n   dos\n").unwrap();
        assert_eq!(m["nota"], "uno\n\ndos\n");
        let m = yaml::parse("nota: |\n  \u{a0}uno\n  dos\n").unwrap();
        assert_eq!(m["nota"], "\u{a0}uno\ndos\n");
    }

    /// A `[…]` split over many lines is read in linear time: before, everything gathered was looked at
    /// again on each line (a 400 KB header in a single list, 15 s).
    #[test]
    fn multiline_yaml_is_not_quadratic() {
        let mut t = String::from("tags: [\n");
        for i in 0..80_000 {
            t += &format!("  \"a{i}\",\n");
        }
        t += "  fin ]\n";
        assert!(t.len() > 900_000);
        let start = std::time::Instant::now();
        let m = yaml::parse(&t).unwrap();
        assert_eq!(m["tags"].as_array().unwrap().len(), 80_001);
        assert!(start.elapsed() < std::time::Duration::from_secs(3), "{:?}", start.elapsed());
        // What is split in the middle of quotes stays open on the next line.
        let m = yaml::parse("x: [\"a,\n  b]\", c]\n").unwrap();
        assert_eq!(m["x"], serde_json::json!(["a, b]", "c"]));
    }

    /// The `index.md` has a concept's limits (neither a symbolic link nor over 1 MB), and a huge bundle
    /// (in files or bytes) is rejected entirely with a clear error.
    #[test]
    fn bundle_index_and_limits() {
        let t = crate::common::TempDir::new("okf-topes");
        let root = t.join("b");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("index.md"), format!("---\nokf_version: \"0.2\"\nx: [{}]\n---\n", "1, ".repeat(400_000))).unwrap();
        for i in 0..5 {
            std::fs::write(root.join("sub").join(format!("c{i}.md")), "---\ntype: Fact\n---\nA.\n").unwrap();
        }
        let b = read(&root).unwrap();
        assert!(b.version.is_none() && b.warnings.iter().any(|a| a.contains("index.md") && a.contains("1 MB")), "{:?}", b.warnings);
        assert_eq!(b.concepts.len(), 5);
        let e = format!("{}", read_with_limits(&root, &Limits { files: 4, ..Limits::default() }).unwrap_err());
        assert!(e.contains("over 4 files and folders") && e.contains("nothing was imported"), "{e}");
        let e = format!("{}", read_with_limits(&root, &Limits { bytes: 40, ..Limits::default() }).unwrap_err());
        assert!(e.contains("MB of concepts"), "{e}");
        // The header limits: each one, all together and the values read.
        let e = format!("{}", read_with_limits(&root, &Limits { headers: 40, ..Limits::default() }).unwrap_err());
        assert!(e.contains("MB of YAML headers") && e.contains("nothing was imported"), "{e}");
        let e = format!("{}", read_with_limits(&root, &Limits { nodes: 3, ..Limits::default() }).unwrap_err());
        assert!(e.contains("values in its YAML headers") && e.contains("nothing was imported"), "{e}");
        assert_eq!(MAX_BYTES, 50 << 20);
        assert_eq!(MAX_HEADER, 256 << 10);
        // A header over 256 KB (here, a `[…]` list of 300 KB): the bundle isn't valid, without parsing it.
        std::fs::write(root.join("sub").join("enorme.md"), format!("---\ntype: Fact\nx: [{}]\n---\nA.\n", "a, ".repeat(100_000))).unwrap();
        let start = std::time::Instant::now();
        let e = format!("{:#}", read(&root).unwrap_err());
        assert!(e.contains("sub/enorme.md") && e.contains("over 256 KB"), "{e}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
        std::fs::remove_file(root.join("sub").join("enorme.md")).unwrap();
        // An `index.md` that is a link (here, to a file outside) isn't read.
        let outside = t.join("fuera.md");
        std::fs::write(&outside, "---\nokf_version: \"9.9\"\n---\n").unwrap();
        std::fs::remove_file(root.join("index.md")).unwrap();
        // On Windows, creating a symbolic link needs developer mode: without it, it isn't checked.
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside, root.join("index.md")).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, root.join("index.md")).is_ok();
        if linked {
            let b = read(&root).unwrap();
            assert!(b.version.is_none() && b.warnings.iter().any(|a| a.contains("index.md") && a.contains("symbolic link")), "{:?}", b.warnings);
        }
    }
}
