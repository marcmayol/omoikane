//! View the memory in Obsidian (1.3.5): `omoi export --obsidian <vault>` writes one Markdown note per
//! memory into `<vault>/omoikane/`, to look at it with Obsidian's graph view. The folder is a
//! mirror rebuilt on every export; since 1.9.0, `omoi import --obsidian <vault>` brings back what was
//! edited there (see [`import`]), and until then the export neither overwrites nor deletes an edited
//! note (see [`pending`]).
//!
//! - One folder per project (`_global` for global memories, `_<space>` for those of a space with no
//!   project), flat: the kind goes in the tags, which is what colours the graph.
//! - Each note links to its project's note (`proyecto-<name>`; global ones, to none: almost everything is
//!   global for someone working from their home folder, and a single note for all of it turned it into
//!   the centre of a star that hid the real links), to the memories it names with
//!   `[[name]]` (resolved with the rules of `links`, but only among what is exported), to the ones it
//!   supersedes or contradicts (current verdicts, `verdicts::pairs`) and to one note per file
//!   cited by two or more memories (`_ficheros/`).
//! - Each note is named after its memory's title (that's what Obsidian shows as the title), with the
//!   end of the id appended only if another would have the same name: retitling a memory renames it, and
//!   the old one is deleted. Only what changes is written and only what carries the `omoikane:` mark in
//!   the header is deleted (whatever someone else put in the folder stays).
//! - Scope: by default the personal ones (projects with no space) and the global ones; with a space, only
//!   that client's, without the global ones. Never two spaces in the same vault.
//! - `.obsidian/graph.json` with one colour per kind, only if the vault has none.
//! - Each memory note carries in its header `base` (the fingerprint of the editable part, as exported) and
//!   `sig` (the signature of this database, with the OKF key: `okf::signature`), to tell on the way back whether
//!   it was edited and whether it really belongs to that memory.

use crate::memory::{Scope, Changes, Memory, Kind};
use crate::tr;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The vault subfolder that belongs to Omoikane.
pub const FOLDER: &str = "omoikane";

/// What an export did.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ExportCount {
    pub memories: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub hubs: usize,
    /// Notes edited in Obsidian not yet imported: they are neither overwritten nor deleted.
    pub kept: usize,
}

/// What gets exported: the personal and global ones, or a space without the global ones.
#[derive(Clone, Copy, Debug)]
pub enum ExportScope {
    Personal,
    Space(i64),
}

/// Exports to the vault and returns what changed.
pub fn export(db: &Connection, vault: &Path, what: ExportScope) -> Result<ExportCount> {
    export_with(db, vault, what, &BTreeSet::new(), &BTreeSet::new())
}

/// Exports; the notes in `overwrite` are rebuilt even if edited and not imported (`--prefer omoikane`), and
/// those in `keep` are neither written nor deleted (what the import couldn't bring in: notes without this
/// database's signature that say something other than their memory, new notes that can't touch theirs).
fn export_with(db: &Connection, vault: &Path, what: ExportScope, overwrite: &BTreeSet<PathBuf>, keep: &BTreeSet<PathBuf>) -> Result<ExportCount> {
    // A network folder is not looked at (see `project::is_local_path`); one with a drive letter is.
    crate::project::require_local(vault)?;
    let p = prepare(db, what)?;
    let root = vault.join(FOLDER);
    std::fs::create_dir_all(&root).with_context(|| tr!("could not create {}", "no se pudo crear {}", root.display()))?;
    let sign_key = crate::okf::signature::key(db, true)?.context("okf_firma")?;
    let scope = scope_of(db, what)?;

    // What was edited in Obsidian and not yet imported is neither overwritten nor deleted (see `pending`).
    let by_id: HashMap<&str, usize> = p.notes.iter().enumerate().map(|(i, n)| (n.m.id.as_str(), i)).collect();
    let mut kept: BTreeSet<PathBuf> = BTreeSet::new();
    for (path, l) in vault_notes(&root, &mut Vec::new()).unwrap_or_default() {
        if !overwrite.contains(&path) && pending(&l, &p, &by_id, &sign_key, &scope) {
            kept.insert(path);
        }
    }
    kept.extend(keep.iter().filter(|r| !overwrite.contains(*r)).cloned());

    let mut written: BTreeSet<PathBuf> = BTreeSet::new();
    let mut c = ExportCount { memories: p.notes.len(), kept: kept.len(), ..ExportCount::default() };
    for n in &p.notes {
        let path = root.join(&n.rel);
        if !kept.contains(&path) {
            write_if_changed(&path, &signed(&n.text, &n.rel, &n.m, &scope, &sign_key), &mut c)?;
        }
        written.insert(path);
    }
    for (rel, text) in &p.hubs {
        let path = root.join(rel);
        write_if_changed(&path, text, &mut c)?;
        written.insert(path);
        c.hubs += 1;
    }
    written.extend(kept);
    c.deleted = clean_up(&root, &written)?;
    configure_graph(vault)?;
    Ok(c)
}

/// A memory note as the export writes it now (without `base` or `sig`).
struct Render {
    m: Memory,
    /// Its path inside `<vault>/omoikane/`.
    rel: PathBuf,
    text: String,
}

/// Everything an export would write, without writing anything.
struct Prepared {
    notes: Vec<Render>,
    /// The group and file notes (path inside `omoikane/`, text).
    hubs: Vec<(PathBuf, String)>,
    /// The groups, by their lowercase folder: which project a new note placed in it goes to.
    groups: BTreeMap<String, Group>,
    /// The (lowercase) name of each memory's note → its id.
    names: HashMap<String, String>,
}

fn prepare(db: &Connection, what: ExportScope) -> Result<Prepared> {
    let scope = match what {
        ExportScope::Personal => Scope::Space(None),
        ExportScope::Space(s) => Scope::Space(Some(s)),
    };
    let mut all = crate::memory::recent(db, &scope, None, usize::MAX >> 1)?;
    if matches!(what, ExportScope::Space(_)) {
        all.retain(|m| !m.global());
    }
    all.sort_by(|a, b| a.id.cmp(&b.id));
    let space = match what {
        ExportScope::Personal => None,
        ExportScope::Space(s) => Some(s),
    };

    // Group (folder and project note) of each memory; two projects with the same name are
    // told apart by the start of their key.
    let mut names: HashMap<String, BTreeSet<String>> = HashMap::new();
    for m in &all {
        if m.project_id != 0 {
            names.entry(crate::project::name_of(&m.project, &m.project_key)).or_default().insert(m.project_key.clone());
        }
    }
    let group = |m: &Memory| -> Group {
        if m.project_id == 0 {
            return if m.space.is_empty() {
                Group { folder: "_global".into(), note: String::new(), title: tr!("Global", "Global").to_string(), project_id: 0, space_id: None }
            } else {
                let s = slug(&m.space, 40);
                Group { folder: format!("_{s}"), note: format!("espacio-{s}"), title: m.space.clone(), project_id: 0, space_id: space }
            };
        }
        let name = crate::project::name_of(&m.project, &m.project_key);
        let mut s = slug(&name, 40);
        if names.get(&name).is_some_and(|k| k.len() > 1) {
            s = format!("{s}-{}", short_id(&m.project_key));
        }
        Group { folder: s.clone(), note: format!("proyecto-{s}"), title: name, project_id: m.project_id, space_id: None }
    };

    let taken: BTreeSet<String> = all.iter().map(|m| group(m).note.to_lowercase()).filter(|n| !n.is_empty()).collect();
    let note_names = note_names_by_id(&all, &taken);
    // The `[[name]]` links, as in `links::recompute`, but only among what is exported.
    let mut by_name: HashMap<String, Vec<&str>> = HashMap::new();
    for m in &all {
        for t in &m.tags {
            by_name.entry(crate::links::link_name(t)).or_default().push(m.id.as_str());
        }
    }
    let targets = |m: &Memory, e: &str| -> Vec<String> {
        match by_name.get(&crate::links::link_name(e)) {
            Some(v) => v.iter().filter(|d| **d != m.id).map(|d| d.to_string()).collect(),
            None => {
                let s = e.trim().to_uppercase();
                let is_id = s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric());
                if is_id { all.iter().find(|x| x.id.starts_with(&s) && x.id != m.id).map(|x| vec![x.id.clone()]).unwrap_or_default() } else { vec![] }
            }
        }
    };

    // Current verdicts among what is exported.
    let mut superseded_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut supersedes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut contradicts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in crate::verdicts::pairs(db)?.into_values() {
        if !note_names.contains_key(&v.memory_id) || !note_names.contains_key(&v.other_id) {
            continue;
        }
        if v.relation == crate::verdicts::SUPERSEDED {
            superseded_by.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            supersedes.entry(v.other_id).or_default().push(v.memory_id);
        } else if v.relation == crate::verdicts::CONTRADICTS {
            contradicts.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            contradicts.entry(v.other_id).or_default().push(v.memory_id);
        }
    }

    // One note per file cited by two or more memories.
    let mut citations: BTreeMap<String, usize> = BTreeMap::new();
    for m in &all {
        for p in m.paths.iter().collect::<BTreeSet<_>>() {
            *citations.entry(p.clone()).or_default() += 1;
        }
    }
    let file_notes: BTreeMap<String, String> = citations.into_iter().filter(|(_, n)| *n >= 2).map(|(p, _)| { let n = file_note(&p); (p, n) }).collect();

    let mut notes = Vec::new();
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for m in &all {
        let g = group(m);
        let rel = Path::new(&g.folder).join(format!("{}.md", note_names[&m.id]));
        let text = render_note(m, &g, &note_names, &targets, &file_notes, &superseded_by, &supersedes, &contradicts);
        notes.push(Render { m: m.clone(), rel, text });
        groups.entry(g.folder.to_lowercase()).or_insert(g);
    }
    let mut hubs = Vec::new();
    for g in groups.values().filter(|g| !g.note.is_empty()) {
        let text = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-hub\"]\n---\n# {}\n\n{}\n",
            g.title,
            tr!("The memories of this group link here. A copy from Omoikane: rewritten on every export.", "Las memorias de este grupo enlazan aquí. Copia de Omoikane: se rehace en cada exportación.")
        );
        hubs.push((Path::new(&g.folder).join(format!("{}.md", g.note)), text));
    }
    for (p, n) in &file_notes {
        let text = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-fichero\"]\n---\n# {}\n\n{}\n",
            p.replace('\\', "/"),
            tr!("The memories that cite this file link here.", "Las memorias que citan este fichero enlazan aquí.")
        );
        hubs.push((Path::new(FILES_DIR).join(format!("{n}.md")), text));
    }
    let names = note_names.into_iter().map(|(id, n)| (n.to_lowercase(), id)).collect();
    Ok(Prepared { notes, hubs, groups, names })
}

/// The folder of the file notes (they aren't memories: the way back skips it).
const FILES_DIR: &str = "_ficheros";

#[derive(Clone, Debug)]
struct Group {
    folder: String,
    note: String,
    title: String,
    /// Its project (0: global, or a space with no project).
    project_id: i64,
    /// The space of `_<space>` (memories of a space with no project).
    space_id: Option<i64>,
}

/// The name of each memory's note: its title, which is what Obsidian shows as the note's
/// title. Obsidian resolves links by name across the whole vault, case-insensitively, so
/// if two memories (or a memory and a group note) would have the same name, they get the end of
/// their id appended, which doesn't change.
fn note_names_by_id(all: &[Memory], taken: &BTreeSet<String>) -> HashMap<String, String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for m in all {
        *counts.entry(file_title(&m.title).to_lowercase()).or_default() += 1;
    }
    all
        .iter()
        .map(|m| {
            let t = file_title(&m.title);
            let k = t.to_lowercase();
            let clash = counts[&k] > 1 || taken.contains(&k) || reserved(&k);
            (m.id.clone(), if clash { format!("{t} ({})", short_id(&m.id)) } else { t })
        })
        .collect()
}

/// A title that works as a file name on Windows, macOS and Linux and as an Obsidian link: without
/// `\ / : * ? " < > |` (Windows) or `# ^ [ ]` (Obsidian), without repeated spaces or a trailing dot or
/// space (Windows), and at most 100 characters.
fn file_title(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_control() || "\\/:*?\"<>|#^[]".contains(c) { ' ' } else { c }).collect();
    let r: String = clean.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
    let r = r.trim_end_matches(['.', ' ']).trim_start_matches(['.', ' ']).to_string();
    if r.is_empty() { "Nota".into() } else { r }
}

/// The names Windows doesn't allow as a file.
fn reserved(k: &str) -> bool {
    matches!(k, "con" | "prn" | "aux" | "nul") || (k.len() == 4 && (k.starts_with("com") || k.starts_with("lpt")) && k.as_bytes()[3].is_ascii_digit())
}

pub(crate) fn short_id(s: &str) -> String {
    let s: Vec<char> = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    s[s.len().saturating_sub(8)..].iter().collect::<String>().to_lowercase()
}

/// A name that works on Windows, macOS, Linux and in an Obsidian link: letters and digits, the rest
/// as `-`, lowercase and at most `max` characters.
pub(crate) fn slug(s: &str, max: usize) -> String {
    let mut r = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            r.extend(ch.to_lowercase());
        } else if !r.ends_with('-') && !r.is_empty() {
            r.push('-');
        }
        if r.chars().count() >= max {
            break;
        }
    }
    let r = r.trim_end_matches('-').to_string();
    if r.is_empty() { "nota".into() } else { r }
}

/// The note of a cited file: its name and a piece of the path's hash (two `main.rs` don't collide).
fn file_note(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let normal = path.replace('\\', "/");
    let name = normal.trim_end_matches('/').rsplit('/').next().unwrap_or(&normal);
    let h = Sha256::digest(normal.to_lowercase().as_bytes());
    format!("fichero-{}-{:02x}{:02x}{:02x}", slug(name, 50), h[0], h[1], h[2])
}

#[allow(clippy::too_many_arguments)]
fn render_note(
    m: &Memory,
    g: &Group,
    note_names: &HashMap<String, String>,
    targets: &dyn Fn(&Memory, &str) -> Vec<String>,
    file_notes: &BTreeMap<String, String>,
    superseded_by: &BTreeMap<String, Vec<String>>,
    supersedes: &BTreeMap<String, Vec<String>>,
    contradicts: &BTreeMap<String, Vec<String>>,
) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let list = |v: &[String]| format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "));
    let mut tags = vec![m.kind.as_str().to_string()];
    for t in &m.tags {
        let e = tag(t);
        if !e.is_empty() && !tags.contains(&e) {
            tags.push(e);
        }
    }
    let mut s = String::from("---\n");
    s += &format!("omoikane: {}\n", q(&m.id));
    s += &format!("kind: {}\n", m.kind.as_str());
    s += &format!("aliases: [{}]\n", q(&m.title));
    s += &format!("tags: {}\n", list(&tags));
    s += &format!("project: {}\n", q(&g.title));
    if !m.space.is_empty() {
        s += &format!("space: {}\n", q(&m.space));
    }
    if !m.paths.is_empty() {
        s += &format!("paths: {}\n", list(&m.paths));
    }
    if m.pinned {
        s += "pinned: true\n";
    }
    if !m.author.is_empty() {
        s += &format!("author: {}\n", q(&m.author));
    }
    // No title as a heading: Obsidian already shows the file name, which is the title.
    s += &format!("created: {}\nupdated: {}\n---\n\n", q(&m.created_at), q(&m.updated_at));

    // The content, with each `[[name]]` pointed at the right note (or left as text if it leads to nothing
    // exported: that way no ghost nodes appear) and without a `#d62828` turning into a tag.
    let mut linked: BTreeSet<String> = BTreeSet::new();
    let mut extra: BTreeSet<String> = BTreeSet::new();
    let content = replace_links(&m.content, |e| {
        let d = targets(m, e);
        let Some(first) = d.first() else { return e.to_string() };
        linked.insert(first.clone());
        extra.extend(d.iter().skip(1).cloned());
        format!("[[{}|{}]]", note_names[first], e)
    });
    s += &escape_tags(&content);
    s += &format!("\n\n{FOOTER}\n\n---\n");
    let link = |id: &String| format!("[[{}]]", note_names[id]);
    let line = |s: &mut String, label: &str, ids: Option<&Vec<String>>| {
        if let Some(v) = ids.filter(|v| !v.is_empty()) {
            *s += &format!("- {label}: {}\n", v.iter().map(link).collect::<Vec<_>>().join(", "));
        }
    };
    if !g.note.is_empty() {
        s += &format!("- {}: [[{}]]\n", tr!("Group", "Grupo"), g.note);
    }
    line(&mut s, &tr!("Superseded by", "Sustituida por"), superseded_by.get(&m.id));
    line(&mut s, &tr!("Supersedes", "Sustituye a"), supersedes.get(&m.id));
    line(&mut s, &tr!("Contradicts", "Contradice a"), contradicts.get(&m.id));
    let others: Vec<String> = extra.into_iter().filter(|d| !linked.contains(d)).collect();
    line(&mut s, &tr!("Also links to", "También enlaza a"), Some(&others));
    if !m.paths.is_empty() {
        let v: Vec<String> = m.paths.iter().map(|p| match file_notes.get(p) { Some(n) => format!("[[{n}|{}]]", p.replace('\\', "/")), None => format!("`{}`", p.replace('\\', "/")) }).collect();
        s += &format!("- {}: {}\n", tr!("Files", "Ficheros"), v.join(", "));
    }
    s += &format!(
        "\n*{}*\n",
        tr!(
            "A copy from Omoikane: edit the text above and run `omoi import --obsidian` to bring the change back; what's below the line is rewritten on every export.",
            "Copia de Omoikane: edita el texto de arriba y lanza `omoi import --obsidian` para llevar el cambio de vuelta; lo de debajo de la raya se rehace en cada exportación."
        )
    );
    s
}

/// An Obsidian tag: no spaces or symbols (only letters, digits, `_`, `-` and `/`), and not digits only.
fn tag(t: &str) -> String {
    let r: String = t.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '-' | '/') { c } else { '-' }).collect();
    let r = r.trim_matches('-').to_string();
    if r.chars().all(|c| c.is_ascii_digit()) { String::new() } else { r }
}

/// Replaces each `[[name]]` (or `[[name|text]]`) with what `f` says for the name.
fn replace_links(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut r = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = after[..end].split('|').next().unwrap_or("").trim();
        r += &rest[..i];
        if inner.is_empty() || inner.len() > 120 || inner.contains('\n') {
            r += &rest[i..i + 2 + end + 2];
        } else {
            r += &f(inner);
        }
        rest = &after[end + 2..];
    }
    r + rest
}

/// Escapes a `#` stuck to a word (in Obsidian it would be a tag: `#d62828`, `#3673`) outside
/// code; headings (`## Something`) stay.
fn escape_tags(text: &str) -> String {
    let mut r = String::new();
    let mut in_block = false;
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            r.push('\n');
        }
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            r += line;
            continue;
        }
        if in_block {
            r += line;
            continue;
        }
        let mut in_code = false;
        let mut prev = ' ';
        let cs: Vec<char> = line.chars().collect();
        for (i, &c) in cs.iter().enumerate() {
            if c == '`' {
                in_code = !in_code;
            }
            let next = cs.get(i + 1).copied().unwrap_or(' ');
            if c == '#' && !in_code && !prev.is_alphanumeric() && prev != '#' && prev != '\\' && (next.is_alphanumeric() || next == '_') {
                r.push('\\');
            }
            r.push(c);
            prev = c;
        }
    }
    r
}

/// Writes only if it changes.
fn write_if_changed(path: &Path, text: &str, c: &mut ExportCount) -> Result<()> {
    match std::fs::read(path) {
        Ok(existing) if existing == text.as_bytes() => {
            c.unchanged += 1;
            return Ok(());
        }
        Ok(_) => c.updated += 1,
        Err(_) => c.created += 1,
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, text).with_context(|| tr!("could not write {}", "no se pudo escribir {}", path.display()))?;
    Ok(())
}

/// Deletes the Omoikane notes that are no longer exported (and the folders left empty). Only those
/// carrying the `omoikane:` mark in the header.
fn clean_up(root: &Path, written: &BTreeSet<PathBuf>) -> Result<usize> {
    let mut deleted = 0;
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                dirs.push(p.clone());
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") && !written.contains(&p) && is_ours(&p) {
                std::fs::remove_file(&p)?;
                deleted += 1;
            }
        }
    }
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // only if it's empty
    }
    Ok(deleted)
}

fn is_ours(p: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(p) else { return false };
    let mut l = BufReader::new(f).lines();
    matches!(l.next(), Some(Ok(a)) if a == "---") && matches!(l.next(), Some(Ok(b)) if b.starts_with("omoikane: "))
}

/// One colour per kind in the graph view, only if the vault doesn't have its own yet.
fn configure_graph(vault: &Path) -> Result<()> {
    let path = vault.join(".obsidian").join("graph.json");
    if path.exists() {
        return Ok(());
    }
    let group = |q: &str, rgb: u32| serde_json::json!({ "query": q, "color": { "a": 1, "rgb": rgb } });
    let v = serde_json::json!({
        "colorGroups": [
            group("tag:#gotcha", 0xd62828),
            group("tag:#decision", 0xe9a23b),
            group("tag:#preference", 0x2a9d8f),
            group("tag:#procedure", 0x8e7dbe),
            group("tag:#fact", 0x5b8bd6),
            group("tag:#omoikane-hub", 0xeeeeee),
            group("tag:#omoikane-fichero", 0x7a7a7a),
        ],
        "showArrow": false,
        "showTags": false,
    });
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

/// Exports every `interval` while the database changes (the first time, always). Never returns.
pub fn watch(db: &Connection, vault: &Path, what: ExportScope, interval: std::time::Duration, mut notify: impl FnMut(&ExportCount)) -> Result<()> {
    let mut seen: Option<i64> = None;
    loop {
        // `data_version` changes when another connection writes: if it hasn't changed, there's nothing to look at.
        let v: i64 = db.query_row("PRAGMA data_version", [], |f| f.get(0))?;
        if seen != Some(v) {
            notify(&export(db, vault, what)?);
            seen = Some(v);
        }
        std::thread::sleep(interval);
    }
}

// --- The way back: from Obsidian to Omoikane (1.9.0) ------------------------------------------------

/// What separates a note's text (the editable part) from the links the export rebuilds
/// below it. It's an Obsidian comment: it isn't shown when reading the note.
const FOOTER: &str = "%% omoikane: below this line, rewritten on every export %%";
/// The domain of a note's signature (see [`signed_text`]).
const SIGNATURE_DOMAIN: &str = "omoikane-obsidian-1";

/// A note read from the vault.
#[derive(Clone, Debug)]
struct ReadNote {
    /// Its name without `.md` (what Obsidian shows as the title), in NFC.
    stem: String,
    header: Map<String, Value>,
    /// The text: without the header and, in an Omoikane note, without what's below [`FOOTER`].
    body: String,
}

impl ReadNote {
    fn field(&self, k: &str) -> Option<String> {
        self.header.get(k).map(crate::okf::scalar_text).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn list(&self, k: &str) -> Vec<String> {
        crate::okf::text_list(self.header.get(k)).into_iter().map(|s| s.trim().to_string()).collect()
    }

    /// The id of its memory, if it's the note of a memory exported by Omoikane.
    fn id(&self) -> Option<String> {
        self.field("omoikane").filter(|i| i != "hub")
    }

    fn is_hub(&self) -> bool {
        self.field("omoikane").as_deref() == Some("hub")
    }
}

fn stem_of(rel: &Path) -> String {
    crate::okf::nfc(&rel.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
}

/// Reads a note: its YAML header (if it has one; `None` if it can't be read) and its text.
fn parse_note(stem: &str, text: &str) -> Option<ReadNote> {
    let text = crate::okf::strip_git_crlf(text.to_string());
    let (header, rest) = match crate::okf::split_front_matter(&text) {
        Some((c, r)) => (crate::okf::yaml::parse(c).ok()?, r),
        None => (Map::new(), text.trim_start_matches('\u{feff}')),
    };
    let mut body = rest;
    if header.contains_key("omoikane")
        && let Some(i) = body.rfind(FOOTER)
    {
        body = &body[..i];
    }
    // In `\n`: content saved with `\r\n` comes out that way in the note, and Obsidian (or the editor) may
    // save it with `\n`; that is not an edit. Untouched lines come back from the memory, with their `\r`.
    let body = body.replace("\r\n", "\n");
    Some(ReadNote { stem: stem.to_string(), header, body: body.trim_start_matches('\n').trim_end().to_string() })
}

/// The fingerprint of a note's editable part (name, alias, kind, tags, paths and text),
/// over the parsed values and not the header's text: Obsidian rewrites it its own way
/// (block lists, no quotes) when a property is touched, and that is not a change.
fn fingerprint(l: &ReadNote) -> String {
    use sha2::{Digest, Sha256};
    let v = serde_json::json!([l.stem, l.list("aliases"), l.field("kind"), l.list("tags"), l.list("paths"), l.body]);
    Sha256::digest(v.to_string().as_bytes())[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// What gets signed in an exported note: which memory it belongs to, from which scope it was exported (`""`
/// the personal one, or the space key), which version of the memory it had (`updated`) and the fingerprint
/// of the editable part as exported (`base`). Not the text: that's what gets edited.
fn signed_text(id: &str, scope: &str, updated: &str, base: &str) -> String {
    format!("{SIGNATURE_DOMAIN}\n{}", serde_json::json!({ "id": id, "scope": scope, "updated": updated, "base": base }))
}

/// The note with its `base` and `sig` in the header.
fn signed(text: &str, rel: &Path, m: &Memory, scope: &str, key: &[u8]) -> String {
    let Some(l) = parse_note(&stem_of(rel), text) else { return text.to_string() };
    let base = fingerprint(&l);
    let sig = crate::okf::signature::sign(key, &signed_text(&m.id, scope, &m.updated_at, &base));
    let i = text.get(3..).and_then(|t| t.find("\n---\n")).map_or(0, |i| i + 4);
    format!("{}base: \"{base}\"\nsig: \"{sig}\"\n{}", &text[..i], &text[i..])
}

/// Whether the note was exported by this database, from this scope, for the memory it claims.
fn signature_valid(l: &ReadNote, key: &[u8], scope: &str) -> bool {
    let (Some(id), Some(base), Some(sig)) = (l.id(), l.field("base"), l.field("sig")) else { return false };
    crate::okf::signature::is_valid(key, &signed_text(&id, scope, &l.field("updated").unwrap_or_default(), &base), &sig)
}

/// Whether it has been edited since it was exported.
fn edited(l: &ReadNote) -> bool {
    l.field("base").as_deref() != Some(fingerprint(l).as_str())
}

fn scope_of(db: &Connection, what: ExportScope) -> Result<String> {
    match what {
        ExportScope::Personal => Ok(String::new()),
        ExportScope::Space(s) => crate::project::space_key(db, Some(s)),
    }
}

/// The notes in `<vault>/omoikane/` (except `_ficheros/`, hidden ones and symbolic links), with the
/// limits of an OKF bundle. What gets skipped goes to `notices`.
fn vault_notes(root: &Path, notices: &mut Vec<String>) -> Result<Vec<(PathBuf, ReadNote)>> {
    let mut r = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            anyhow::ensure!(
                seen <= crate::okf::MAX_FILES,
                tr!(
                    "{} has over {} files and folders: is it the right vault? Nothing was imported",
                    "{} tiene más de {} ficheros y carpetas: ¿es el vault bueno? No se ha importado nada",
                    root.display(),
                    crate::okf::MAX_FILES
                )
            );
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(t) = e.file_type() else { continue };
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if t.is_symlink() || crate::okf::is_junction(&p) {
                notices.push(tr!("{rel}: a symbolic link, skipped", "{rel}: un enlace simbólico, saltado"));
                continue;
            }
            if t.is_dir() {
                if !(d == root && name == FILES_DIR) {
                    stack.push(p);
                }
                continue;
            }
            if !name.to_lowercase().ends_with(".md") {
                continue;
            }
            if std::fs::metadata(&p).is_ok_and(|m| m.len() > crate::okf::MAX_CONCEPT) {
                notices.push(tr!("{rel}: over 1 MB, skipped", "{rel}: de más de 1 MB, saltada"));
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                notices.push(tr!("{rel}: not UTF-8 text, skipped", "{rel}: no es texto UTF-8, saltada"));
                continue;
            };
            match parse_note(&stem_of(Path::new(&name)), &text) {
                Some(l) => r.push((p, l)),
                None => notices.push(tr!("{rel}: its YAML header can't be read, skipped", "{rel}: no se puede leer su cabecera YAML, saltada")),
            }
        }
    }
    r.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(r)
}

/// What an edited note would leave in its memory: everything normalized as when saving, to compare it.
#[derive(Debug)]
struct Proposal {
    title: String,
    content: String,
    /// The kind, if the note changes it.
    kind: Option<Kind>,
    tags: Vec<String>,
    paths: Vec<String>,
    /// Whether saving it hides something that looks like a secret (it's no longer visible in the memory:
    /// `memory::update` won't find it again, so it's counted here).
    redacted: bool,
}

impl Proposal {
    /// Whether the memory is already like this (imported again, or a change that changes nothing).
    fn matches(&self, m: &Memory) -> bool {
        let as_set = |v: &[String]| v.iter().cloned().collect::<BTreeSet<String>>();
        self.title == m.title
            && self.content == m.content
            && self.kind.is_none_or(|k| k == m.kind && m.kind_text.is_none())
            && as_set(&self.tags) == as_set(&m.tags)
            && as_set(&self.paths) == as_set(&m.paths)
    }
}

/// What an edited note (`l`) changes in its memory (`m`), comparing with the note the export
/// would write for it now (`b`). Only title, text, kind, tags and paths: never which project
/// or space it belongs to, whether it's global, pinned or verified. Of the tags, only the plain ones: those
/// with `:` (`trade:`, `topic:`, `review:`…) carry scope or meaning and are neither removed nor added.
fn propose(l: &ReadNote, m: &Memory, b: &ReadNote, names: &HashMap<String, String>) -> Result<Proposal> {
    // The title: on export, the file name is the alias turned into a file name. If they still
    // match, the alias wins (edited or not); if not, whatever was changed: the name (Obsidian
    // renames the file when the note's title changes) if the alias is the memory's, otherwise the alias.
    let alias = |x: &ReadNote| x.list("aliases").into_iter().next().unwrap_or_default();
    let (al, ab) = (alias(l), alias(b));
    let suffix = format!(" ({})", short_id(&m.id));
    let name = l.stem.strip_suffix(&suffix).unwrap_or(&l.stem).to_string();
    let title = if al.is_empty() {
        name
    } else if file_title(&al) == name || al != ab {
        al
    } else {
        name
    };
    let kind = match (l.field("kind"), b.field("kind")) {
        (Some(k), before) if Some(&k) != before.as_ref() => Some(k.parse::<Kind>()?),
        _ => None,
    };
    let plain = |v: Vec<String>| -> BTreeSet<String> {
        v.into_iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty() && t.parse::<Kind>().is_err()).collect()
    };
    let (in_note, before) = (plain(l.list("tags")), plain(b.list("tags")));
    let removed: BTreeSet<&String> = before.difference(&in_note).collect();
    let mut tags = m.tags.clone();
    tags.retain(|t| t.contains(':') || !removed.contains(&tag(t).to_lowercase()));
    tags.extend(in_note.difference(&before).filter(|t| !t.contains(':')).cloned());
    let paths = if l.list("paths") != b.list("paths") { l.list("paths") } else { m.paths.clone() };
    let content = if l.body == b.body { m.content.clone() } else { merge(&m.content, &b.body, &l.body, names) };
    anyhow::ensure!(
        !content.trim().is_empty(),
        tr!("the note's text is empty: to forget the memory, use `omoi forget`", "el texto de la nota está vacío: para olvidar la memoria, `omoi forget`")
    );
    let mut redacted = Vec::new();
    let (title, content, paths, tags) = crate::memory::as_stored(&title, &content, &paths, &tags, &mut redacted)?;
    Ok(Proposal { title, content, kind, tags, paths, redacted: !redacted.is_empty() })
}

/// The new text of a memory: each line of the edited note (`e`) that is still the same as in the
/// exported one (`b`) goes back to being the original line of the content (`c`), and the changed ones go
/// through [`unexport_line`]. Exporting changes each line separately (links and `#`), so `c` and `b` have
/// the same lines; if not (or the note is huge), all of them go through `unexport_line`.
fn merge(c: &str, b: &str, e: &str, names: &HashMap<String, String>) -> String {
    let (cl, bl, el): (Vec<&str>, Vec<&str>, Vec<&str>) = (c.split('\n').collect(), b.split('\n').collect(), e.split('\n').collect());
    if cl.len() != bl.len() || bl.len().saturating_mul(el.len()) > 4_000_000 {
        return el.iter().map(|x| unexport_line(x, names)).collect::<Vec<_>>().join("\n");
    }
    // The longest common subsequence between `bl` and `el`.
    let (n, m) = (bl.len(), el.len());
    let width = m + 1;
    let mut t = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * width + j] = if bl[i] == el[j] { t[(i + 1) * width + j + 1] + 1 } else { t[(i + 1) * width + j].max(t[i * width + j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut r: Vec<String> = Vec::with_capacity(m);
    while j < m {
        if i < n && bl[i] == el[j] && t[i * width + j] == t[(i + 1) * width + j + 1] + 1 {
            r.push(cl[i].to_string());
            i += 1;
            j += 1;
        } else if i < n && t[(i + 1) * width + j] >= t[i * width + j + 1] {
            i += 1;
        } else {
            r.push(unexport_line(el[j], names));
            j += 1;
        }
    }
    r.join("\n")
}

/// A line written in Obsidian, turned into how Omoikane stores it: `[[note|name]]` (the way links are
/// exported) goes back to `[[name]]`, a `[[note]]` to another memory becomes `[[<its id>]]` (that's how it
/// resolves in Omoikane) and the `\#` added by the export goes back to `#`. Other links, as they are.
fn unexport_line(line: &str, names: &HashMap<String, String>) -> String {
    let mut r = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        r += &rest[..i];
        let (target, text) = match inner.split_once('|') {
            Some((a, t)) => (a.trim(), t.trim()),
            None => (inner.trim(), ""),
        };
        let target = target.rsplit('/').next().unwrap_or(target).trim_end_matches(".md");
        match names.get(&target.to_lowercase()) {
            Some(_) if !text.is_empty() => r += &format!("[[{text}]]"),
            Some(id) => r += &format!("[[{id}]]"),
            None => r += &rest[i..i + 2 + end + 2],
        }
        rest = &after[end + 2..];
    }
    r += rest;
    let cs: Vec<char> = r.chars().collect();
    let mut s = String::with_capacity(r.len());
    let mut in_code = false;
    for (i, &c) in cs.iter().enumerate() {
        if c == '`' {
            in_code = !in_code;
        }
        if c == '\\' && !in_code && cs.get(i + 1) == Some(&'#') && cs.get(i + 2).is_some_and(|n| n.is_alphanumeric() || *n == '_') {
            continue;
        }
        s.push(c);
    }
    s
}

/// Whether an Omoikane note is edited and its change is not yet in its memory (not imported, or in
/// conflict): the export neither overwrites nor deletes it. Also the note of a memory no longer exported
/// (forgotten or moved elsewhere): it's the person's text, and they delete it.
fn pending(l: &ReadNote, p: &Prepared, by_id: &HashMap<&str, usize>, key: &[u8], scope: &str) -> bool {
    let Some(id) = l.id() else { return false };
    if !signature_valid(l, key, scope) || !edited(l) {
        return false;
    }
    let Some(&i) = by_id.get(id.as_str()) else { return true };
    let n = &p.notes[i];
    let Some(b) = parse_note(&stem_of(&n.rel), &n.text) else { return true };
    propose(l, &n.m, &b, &p.names).map(|x| !x.matches(&n.m)).unwrap_or(true)
}

/// Whether a note without this database's signature (from a version before 1.9.0, from another database or with a
/// broken signature) says the same as the one the export would write now for its memory: rebuilding it
/// loses nothing. One from before 1.9.0 has no [`FOOTER`], so its text still carries the links from
/// below the line: they count as intact if they are the current ones (the last line, the notice, changed).
/// When in doubt, no: the import leaves it as it is.
fn foreign_intact(l: &ReadNote, p: &Prepared, by_id: &HashMap<&str, usize>) -> bool {
    let Some(i) = l.id().and_then(|id| by_id.get(id.as_str()).copied()) else { return false };
    let n = &p.notes[i];
    let Some(b) = parse_note(&stem_of(&n.rel), &n.text) else { return false };
    if fingerprint(l) == fingerprint(&b) {
        return true;
    }
    let strip_notice = |t: &str| t.rsplit_once("\n\n*").map_or(t, |(a, _)| a).trim_end().to_string();
    let Some((_, below)) = n.text.rsplit_once(FOOTER) else { return false };
    if strip_notice(&l.body) != format!("{}\n\n{}", b.body, strip_notice(below.trim())) {
        return false;
    }
    let mut l = l.clone();
    l.body = b.body.clone();
    fingerprint(&l) == fingerprint(&b)
}

/// What to do with an edited note whose memory has also changed in Omoikane since the export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Prefer {
    /// Nothing: it's reported and left as it is (the note isn't overwritten).
    #[default]
    Neither,
    /// Obsidian's: the note is applied (Omoikane's version stays in the history).
    Obsidian,
    /// Omoikane's: the note is rebuilt from the memory and its change is lost.
    Omoikane,
}

/// A note that didn't get in, and why.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SkippedNote {
    pub note: String,
    pub reason: String,
}

/// What an import from Obsidian did (or would do, in a dry run).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportSummary {
    /// The memory notes (one per memory) and the new ones that were looked at.
    pub notes: usize,
    pub updated: usize,
    pub created: usize,
    pub unchanged: usize,
    /// Edited in Obsidian and also changed in Omoikane after exporting them: left untouched.
    pub conflicts: Vec<String>,
    /// Conflicts resolved with `--prefer obsidian` (applied) or `--prefer omoikane` (discarded).
    pub conflicts_applied: usize,
    pub conflicts_discarded: usize,
    /// With `omoikane:` but without the signature of this database and this scope (another database, another version, or
    /// touched in those fields) and with a different text from their memory: they touch no memory, and the
    /// following export neither overwrites nor deletes them (those saying the same count in `unchanged`
    /// and are rebuilt signed).
    pub foreign: Vec<String>,
    /// Edited, but their memory is no longer exported to this vault (forgotten, or moved elsewhere).
    pub gone: Vec<String>,
    /// New notes that can't get in (no project or no kind that can be inferred, global, empty).
    pub skipped: Vec<SkippedNote>,
    pub failed: Vec<SkippedNote>,
    /// The titles of the memories in which something that looked like a secret was hidden.
    pub redacted: Vec<String>,
    /// Memories verified by a person whose text changed: they lose the verification.
    pub unverified: usize,
    /// What was skipped while reading the vault.
    pub notices: Vec<String>,
    /// The export that brings the vault up to date after importing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportCount>,
}

/// The key a new note is saved under: `obsidian:<its path>` (importing it again doesn't
/// duplicate it; without a signature, it doesn't change the memory that already came from it either).
fn note_key(rel: &str) -> String {
    let r = rel.to_lowercase();
    let r = r.strip_suffix(".md").unwrap_or(&r);
    let clean: String = r.chars().map(|c| if c.is_alphanumeric() || "-_./".contains(c) { c } else { '-' }).collect();
    let k = format!("obsidian:{clean}");
    if k.chars().count() <= 80 {
        return k;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(rel.as_bytes());
    format!("{}-{:02x}{:02x}{:02x}{:02x}", k.chars().take(71).collect::<String>(), h[0], h[1], h[2], h[3])
}

/// Brings into Omoikane what was edited in a vault exported with [`export`] (`omoi import --obsidian`).
///
/// - An edited memory note (with `omoikane: <id>`) updates its memory, with history (reason
///   `obsidian import`): title (the alias or the file name), text, kind, plain tags and
///   paths. Only if the note carries this database's signature for this scope (`sig`, see
///   [`signed_text`]): an id is not a secret (it travels in sync, in bundles and in other
///   vaults), and without a signature nobody can point a note at a memory. It never changes project or
///   space, nor makes it global, pinned or verified; changing a verified memory's text removes its
///   verification, like `update`. A trade memory goes through `trades::leaks`.
/// - Not edited (its fingerprint is the one in `base`) or already imported: nothing.
/// - Edited and also changed in Omoikane after exporting it (`updated` is no longer the
///   memory's): conflict. Without `prefer`, neither is touched and it's reported; the export
///   doesn't overwrite the note either.
/// - A new note (without `omoikane:`) in a project's folder (or with a `project:` that is one of
///   this vault) and with `kind:` (or a tag that is a kind) comes in as a new memory of that project
///   (or of the space, in `_<space>/`), under the key `obsidian:<path>`, unpinned and with source
///   `import:obsidian`; its file becomes Omoikane's copy. Not in `_global/`: global memories are not
///   created from a file anyone can write. If a memory already came from that path, the note
///   (unsigned) doesn't change it: it's skipped and stays in the vault.
/// - A note with `omoikane:` without this database's signature (from an earlier version, from another database) doesn't
///   touch any memory; if its text isn't its memory's, the following export leaves it.
/// - Group and file notes, whatever is outside `omoikane/` and what was deleted in the vault do nothing.
///
/// Everything that comes in goes through secret redaction, like any write. Afterwards, if it's not a
/// dry run, it exports again to bring the vault up to date.
pub fn import(db: &mut Connection, model: Option<&crate::embed::Model>, vault: &Path, what: ExportScope, prefer: Prefer, dry_run: bool) -> Result<ImportSummary> {
    crate::project::require_local(vault)?;
    let root = vault.join(FOLDER);
    anyhow::ensure!(
        root.is_dir(),
        tr!(
            "{} has no {FOLDER}/ folder: export to it first (omoi export --obsidian <vault>)",
            "{} no tiene la carpeta {FOLDER}/: exporta antes a él (omoi export --obsidian <vault>)",
            vault.display()
        )
    );
    let mut r = ImportSummary::default();
    let notes = vault_notes(&root, &mut r.notices)?;
    let p = prepare(db, what)?;
    let scope = scope_of(db, what)?;
    let sign_key = crate::okf::signature::key(db, false)?;
    let rel_of = |path: &Path| path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace('\\', "/");

    // A vault exported from another scope (the personal one versus a space): say so clearly.
    let memory_notes: Vec<&ReadNote> = notes.iter().map(|(_, l)| l).filter(|l| l.id().is_some()).collect();
    if let Some(other) = memory_notes.first().map(|l| l.field("space").unwrap_or_default())
        && other != scope
        && memory_notes.iter().all(|l| l.field("space").unwrap_or_default() == other)
    {
        if scope.is_empty() {
            bail!(tr!("this vault was exported from the space «{other}»: import it with --space {other}", "este vault se exportó del espacio «{other}»: impórtalo con --space {other}"));
        }
        bail!(tr!(
            "this vault wasn't exported from the space «{scope}»: import it with the --space it was exported from (or without --space if it's the personal one)",
            "este vault no se exportó del espacio «{scope}»: impórtalo con el --space del que se exportó (o sin --space si es el personal)"
        ));
    }
    let by_id: HashMap<&str, usize> = p.notes.iter().enumerate().map(|(i, n)| (n.m.id.as_str(), i)).collect();
    let mut overwrite: BTreeSet<PathBuf> = BTreeSet::new();
    // What the following export neither writes nor deletes: what couldn't get in and is someone's
    // text (see `export_with`).
    let mut keep: BTreeSet<PathBuf> = BTreeSet::new();
    let mut by_memory: BTreeMap<String, Vec<(PathBuf, ReadNote)>> = BTreeMap::new();
    let mut new_notes: Vec<(PathBuf, ReadNote)> = Vec::new();
    for (path, l) in notes {
        if l.is_hub() {
            continue;
        }
        if !l.header.contains_key("omoikane") {
            new_notes.push((path, l));
        } else if let (Some(id), Some(k)) = (l.id(), &sign_key)
            && signature_valid(&l, k, &scope)
        {
            by_memory.entry(id).or_default().push((path, l));
        } else if foreign_intact(&l, &p, &by_id) {
            // Unsigned but the same as its memory (an untouched vault from an earlier version): the
            // export rebuilds it signed and nothing is lost.
            r.notes += 1;
            r.unchanged += 1;
        } else {
            // Unsigned and with a different text: it may be an edit (an edited 1.8 vault), which doesn't
            // go into any memory; the export doesn't overwrite or delete it either.
            r.foreign.push(rel_of(&path));
            keep.insert(path);
        }
    }

    for (id, copies) in by_memory {
        r.notes += 1;
        let edited_copies: Vec<&(PathBuf, ReadNote)> = copies.iter().filter(|(_, l)| edited(l)).collect();
        let (path, l) = match edited_copies.as_slice() {
            [] => {
                r.unchanged += 1;
                continue;
            }
            [one] => (&one.0, &one.1),
            several => {
                // Two edited copies of the same memory (one renamed and another that was exported again): no
                // way to know which one counts.
                r.conflicts.push(several.iter().map(|(p, _)| rel_of(p)).collect::<Vec<_>>().join(" + "));
                continue;
            }
        };
        let rel = rel_of(path);
        let Some(&i) = by_id.get(id.as_str()) else {
            r.gone.push(rel);
            continue;
        };
        let n = &p.notes[i];
        let m = &n.m;
        let proposal = parse_note(&stem_of(&n.rel), &n.text).context("render").and_then(|b| propose(l, m, &b, &p.names));
        let proposal = match proposal {
            Ok(x) => x,
            Err(e) => {
                r.failed.push(SkippedNote { note: rel, reason: format!("{e:#}") });
                continue;
            }
        };
        // What is already in its memory (or is applied now) the following export rebuilds (with the
        // new name if the title changed), even if the note still looks edited.
        if proposal.matches(m) {
            r.unchanged += 1;
            overwrite.insert(path.clone());
            continue;
        }
        if l.field("updated").as_deref() != Some(m.updated_at.as_str()) {
            match prefer {
                Prefer::Neither => {
                    r.conflicts.push(rel);
                    continue;
                }
                Prefer::Omoikane => {
                    r.conflicts_discarded += 1;
                    overwrite.insert(path.clone());
                    continue;
                }
                Prefer::Obsidian => r.conflicts_applied += 1,
            }
        }
        let mut changes = Changes {
            title: (proposal.title != m.title).then(|| proposal.title.clone()),
            content: (proposal.content != m.content).then(|| proposal.content.clone()),
            kind: proposal.kind,
            paths: Some(proposal.paths.clone()),
            tags: Some(proposal.tags.clone()),
            reason: Some("obsidian import".into()),
            ..Default::default()
        };
        // A trade memory: the new content goes through `trades::leaks`, as in `update`.
        if crate::trades::has_trade(&m.tags) {
            let request = crate::trades::ChangeRequest { trade: None, for_whom: None, changes: &changes, data: None, allow_leaks: false };
            match crate::trades::on_update(db, m, request)? {
                Err(leaks) => {
                    let list: Vec<String> = leaks.iter().map(|f| format!("{} «{}»", f.kind, f.text)).collect();
                    r.failed.push(SkippedNote {
                        note: rel,
                        reason: tr!(
                            "a trade memory (reaches every project of its trade) and the change names what looks like a project or a client: {}",
                            "una memoria de oficio (llega a todos los proyectos de su oficio) y el cambio nombra algo que parece de un proyecto o de un cliente: {}",
                            list.join(", ")
                        ),
                    });
                    continue;
                }
                Ok(plan) if plan.make_global || plan.create_new => {
                    r.failed.push(SkippedNote { note: rel, reason: tr!("it would change where the memory belongs", "cambiaría dónde está la memoria") });
                    continue;
                }
                Ok(plan) => {
                    if let Some(t) = plan.tags {
                        changes.tags = Some(t);
                    }
                }
            }
        }
        if dry_run {
            r.updated += 1;
            if proposal.redacted {
                r.redacted.push(proposal.title.clone());
            }
            continue;
        }
        match crate::memory::update(db, &m.id, changes, model) {
            Ok(x) => {
                r.updated += 1;
                overwrite.insert(path.clone());
                // `memory::update` no longer sees the secret (`propose` hid it, to compare): it's counted from
                // the proposal.
                if proposal.redacted || !x.redacted.is_empty() {
                    r.redacted.push(x.title.clone());
                }
                r.unverified += usize::from(x.unverified.is_some());
            }
            Err(e) => r.failed.push(SkippedNote { note: rel, reason: format!("{e:#}") }),
        }
    }

    for (path, l) in new_notes {
        r.notes += 1;
        let rel = rel_of(&path);
        let mut skip = |reason: String| r.skipped.push(SkippedNote { note: rel.clone(), reason });
        let folder = rel.split_once('/').map(|(c, _)| c.to_lowercase());
        let group = match folder.as_ref().and_then(|c| p.groups.get(c)) {
            Some(g) => Some(g),
            None => l.field("project").and_then(|t| p.groups.values().find(|g| g.title.eq_ignore_ascii_case(&t) || g.folder.eq_ignore_ascii_case(&t))),
        };
        let Some(g) = group else {
            skip(tr!(
                "no project can be told: put it in a project's folder of this vault (or give it a `project:` that is one)",
                "no se sabe de qué proyecto es: ponla en la carpeta de un proyecto de este vault (o dale un `project:` que sea uno)"
            ));
            continue;
        };
        if g.project_id == 0 && g.space_id.is_none() {
            skip(tr!(
                "in _global/: a note from the vault doesn't become a global memory (every project sees those); save it with `omoi remember --global`",
                "en _global/: una nota del vault no se convierte en memoria global (las ve cualquier proyecto); guárdala con `omoi remember --global`"
            ));
            continue;
        }
        let kind = match l.field("kind") {
            Some(k) => match k.parse::<Kind>() {
                Ok(t) => t,
                Err(e) => {
                    skip(format!("{e:#}"));
                    continue;
                }
            },
            None => match l.list("tags").iter().find_map(|t| t.trim_start_matches('#').parse::<Kind>().ok()) {
                Some(t) => t,
                None => {
                    skip(tr!(
                        "no kind: add `kind: fact` (or decision, gotcha, preference, procedure) to its properties",
                        "sin tipo: añade `kind: fact` (o decision, gotcha, preference, procedure) a sus propiedades"
                    ));
                    continue;
                }
            },
        };
        if l.body.trim().is_empty() {
            skip(tr!("empty", "vacía").to_string());
            continue;
        }
        let title = l.list("aliases").into_iter().next().filter(|t| !t.is_empty()).or_else(|| l.field("title")).unwrap_or_else(|| l.stem.clone());
        let tags: Vec<String> = l
            .list("tags")
            .into_iter()
            .map(|t| t.trim_start_matches('#').to_lowercase())
            .filter(|t| !t.is_empty() && !t.contains(':') && t.parse::<Kind>().is_err())
            .collect();
        let new_memory = crate::memory::NewMemory {
            project_id: (g.project_id != 0).then_some(g.project_id),
            space_id: if g.project_id == 0 { g.space_id } else { None },
            kind: Some(kind),
            title,
            content: l.body.clone(),
            paths: l.list("paths"),
            tags,
            pinned: false,
            source: "import:obsidian".into(),
            ..Default::default()
        };
        let key = note_key(&rel);
        let topic = format!("{}{key}", crate::memory::TOPIC_TAG);
        let earlier: Option<(String, String)> = db
            .query_row(
                "SELECT id, hlc FROM memory WHERE project_id = ?1 AND space_id IS ?2
                   AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?3) ORDER BY created_at, id LIMIT 1",
                rusqlite::params![g.project_id, new_memory.space_id, topic],
                |f| Ok((f.get(0)?, f.get(1)?)),
            )
            .optional()?;
        // A memory already came from this note. Unsigned, the note doesn't change it: anyone writing to
        // the vault could overwrite what was later changed, pinned or verified in Omoikane (and its exported
        // note is the one that gets edited). If it says the same, there's nothing to do; if not, it stays as
        // it is, the export doesn't overwrite it and the user is told how to bring it over.
        if let Some((id, _)) = &earlier {
            let m = match crate::memory::get(db, id) {
                Ok(m) => m,
                Err(e) => {
                    r.failed.push(SkippedNote { note: rel, reason: format!("{e:#}") });
                    keep.insert(path);
                    continue;
                }
            };
            let mut redacted = Vec::new();
            let same = crate::memory::as_stored(&new_memory.title, &new_memory.content, &new_memory.paths, &new_memory.tags, &mut redacted)
                .is_ok_and(|(t, c, _, _)| t == m.title && c == m.content && Some(m.kind) == new_memory.kind);
            if same {
                r.unchanged += 1;
                if !dry_run {
                    // The same as its memory: the export writes its copy.
                    let _ = std::fs::remove_file(&path);
                }
            } else {
                r.skipped.push(SkippedNote {
                    note: rel,
                    reason: tr!(
                        "the memory {} «{}» already came from this note, and a note without Omoikane's signature doesn't change an existing memory: it was left as it is. Bring the change over with `omoi update {}` (or rename the note to save it as a new memory), then delete it",
                        "de esta nota ya salió la memoria {} «{}», y una nota sin la firma de Omoikane no cambia una memoria que ya existe: se queda como está. Lleva el cambio con `omoi update {}` (o cambia el nombre de la nota para guardarla como memoria nueva) y luego bórrala",
                        m.id,
                        m.title,
                        m.id
                    ),
                });
                keep.insert(path);
            }
            continue;
        }
        if dry_run {
            r.created += 1;
            continue;
        }
        let (title, content) = (new_memory.title.clone(), new_memory.content.clone());
        match crate::memory::save_by_key(db, &key, new_memory, None, model) {
            Ok(x) => {
                let changed = match &earlier {
                    None => true,
                    Some((id, hlc)) => crate::memory::hlc_of(db, id).map(|h| h != *hlc).unwrap_or(true),
                };
                match (x.created, changed) {
                    (true, _) => r.created += 1,
                    (false, true) => r.updated += 1,
                    (false, false) => r.unchanged += 1,
                }
                let was_redacted = !x.memory.redacted.is_empty();
                if was_redacted {
                    r.redacted.push(x.memory.title.clone());
                }
                // The note becomes Omoikane's copy (the following export writes it with its id).
                // If something was hidden, it stays as it is: its text isn't the memory's.
                if !was_redacted && x.memory.title == title.split_whitespace().collect::<Vec<_>>().join(" ") && x.memory.content == content.trim() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(e) => r.failed.push(SkippedNote { note: rel, reason: format!("{e:#}") }),
        }
    }

    if !dry_run {
        r.export = Some(export_with(db, vault, what, &overwrite, &keep)?);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_text() {
        assert_eq!(slug("¿Publicar 1.3.0 en crates.io?", 60), "publicar-1-3-0-en-crates-io");
        assert_eq!(slug("***", 60), "nota");
        assert_eq!(file_title("Su \"Netflix casero\": C:\\Users\\x [beta] #1."), "Su Netflix casero C Users x beta 1");
        assert_eq!(file_title("..."), "Nota");
        assert!(reserved("con") && reserved("com1") && !reserved("comer"));
        assert_eq!(tag("project status"), "project-status");
        assert_eq!(tag("2026"), "");
        assert_eq!(replace_links("ver [[a-b|texto]] y [[c]]", |e| format!("<{e}>")), "ver <a-b> y <c>");
        assert_eq!(escape_tags("## Título\nrojo #d62828, `#no` y #3673\n```\n#dentro\n```"), "## Título\nrojo \\#d62828, `#no` y \\#3673\n```\n#dentro\n```");
        assert_ne!(file_note("src/main.rs"), file_note("tests/main.rs"));
    }

    #[test]
    fn text_round_trip() {
        let names: HashMap<String, String> = [("usamos y".to_string(), "01ABC".to_string())].into();
        // What was exported, untouched, comes back as is; what's new, in Omoikane's form.
        assert_eq!(unexport_line("ver [[Usamos Y|nota-b]], [[Usamos Y]], [[otra]] y \\#rojo `\\#x`", &names), "ver [[nota-b]], [[01ABC]], [[otra]] y #rojo `\\#x`");
        let c = "Ver [[nota-b]].\nEl rojo #d62828.\nFin.";
        let b = "Ver [[Usamos Y|nota-b]].\nEl rojo \\#d62828.\nFin.";
        assert_eq!(merge(c, b, "Ver [[Usamos Y|nota-b]].\nNueva [[Usamos Y]].\nEl rojo \\#d62828.\nFin.", &names), "Ver [[nota-b]].\nNueva [[01ABC]].\nEl rojo #d62828.\nFin.");
        assert_eq!(merge(c, b, "El rojo \\#d62828.", &names), "El rojo #d62828.");
        // Content in `\r\n` coming back in `\n` (as the note is read): only the new line changes.
        let l = parse_note("x", "---\nomoikane: \"A\"\n---\n\nUno.\r\nDos.\r\n\n%% omoikane: below this line, rewritten on every export %%\n").unwrap();
        assert_eq!(l.body, "Uno.\nDos.");
        assert_eq!(merge("Uno.\r\nDos.", &l.body, "Uno.\nDos.\nTres.", &names), "Uno.\r\nDos.\nTres.");
    }
}
