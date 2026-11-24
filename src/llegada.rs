//! Memory for newcomers (1.7.0). When someone joins a client's space, their agent already got, at
//! start, what belongs to the project the session opens in, what is pinned, the space's preferences
//! and the global stuff; but not what happens in the client's other projects nor who writes. Here:
//! - `bienvenida`: the first session of this database in a project of a space (and its compactions)
//!   carries a short block with the space's other projects and where they stand, their open
//!   decisions and who writes. Once: later sessions don't repeat it.
//! - `al_unirse`: what `omoi sync join --space` just brought, told to the person.
//!
//! Everything is looked up with `Ambito::Espacio(Some(s))`: nothing from another space ever leaks.

use crate::memoria::{self, Ambito};
use crate::tr;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

/// How many of the space's other projects are shown (of the rest, how many there are).
pub const PROYECTOS: usize = 8;
/// How many open decisions from the other projects are shown.
pub const ABIERTAS: usize = 5;
/// How many authors are named.
pub const AUTORES: usize = 6;
/// Slice of each project's `status`: just enough to know where it stands.
const SNIPPET: usize = 160;

/// Whether this is the first session of this database in this project, in any of its folders. The
/// `current` session doesn't count: after compacting, the agent lost the block and gets it again.
pub fn primera_sesion(db: &Connection, project: i64, current: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM session s JOIN project_path pp ON pp.path = s.project
                            WHERE pp.project_id = ?1 AND s.session <> ?2)",
        params![project, current],
        |r| r.get(0),
    )?)
}

/// A space's name (its key if it has none).
fn space_name(db: &Connection, space: i64) -> Result<String> {
    let n: Option<(String, String)> = db.query_row("SELECT name, key FROM space WHERE id = ?1", [space], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    Ok(n.map(|(n, k)| if n.is_empty() { k } else { n }).unwrap_or_default())
}

/// Who writes in the space (its projects and its own, without the global ones) and how many memories
/// each, most first.
fn authors(db: &Connection, space: i64) -> Result<Vec<(String, i64)>> {
    let mut q = db.prepare(&format!(
        "SELECT m.author, count(*) FROM memory m
         WHERE {} AND (m.project_id <> 0 OR m.space_id IS NOT NULL) AND m.author <> ''
         GROUP BY m.author ORDER BY count(*) DESC, m.author LIMIT ?1",
        Ambito::Espacio(Some(space)).sql()
    ))?;
    let v = q.query_map([AUTORES as i64 + 1], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The content on one line, cut to `n` characters.
fn snippet(text: &str, n: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n { format!("{}…", flat.chars().take(n).collect::<String>()) } else { flat }
}

/// The welcome block for the session start, or nothing: only from a project of a space, only on its
/// first session in this database, and only if the space has something from the team that the rest
/// of the start doesn't show (other projects, their open decisions or other people who write) and the
/// person didn't write all of it themselves.
/// `own` is this computer's author. Called before recording the session.
pub fn bienvenida(db: &Connection, scope: &Ambito, session: &str, own: Option<&str>) -> Result<Option<String>> {
    let Ambito::Proyecto { id, space: Some(space) } = *scope else { return Ok(None) };
    if id <= 0 || !primera_sesion(db, id, session)? {
        return Ok(None);
    }
    let wide = Ambito::Espacio(Some(space));
    // The space's other projects, most active first, with their latest `status`.
    let mut others = crate::proyecto::lista(db, None, Some(Some(space)), PROYECTOS + 2)?;
    others.retain(|p| p.id != id);
    let statuses = memoria::estados(db, &wide, PROYECTOS + 3)?;
    // The open decisions of the other projects (this one's, the space's and the global ones already show).
    let open: Vec<_> = memoria::abiertas(db, &wide, usize::MAX)?.into_iter().filter(|m| m.project_id != 0 && m.project_id != id).collect();
    let authors = authors(db, space)?;
    // Whether the space has something this person didn't write (without an author, unknown: counts as someone else's).
    // If it's all theirs (whoever set up the space), they already know it.
    let foreign: bool = db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE {} AND (m.project_id <> 0 OR m.space_id IS NOT NULL) AND m.author IS NOT ?1)", wide.sql()),
        [own],
        |r| r.get(0),
    )?;
    let other_authors = authors.iter().any(|(a, _)| own != Some(a.as_str()));
    if !foreign || (others.is_empty() && open.is_empty() && !other_authors) {
        return Ok(None);
    }
    let mut s = format!(
        "First session in this project on this computer. It belongs to the space {}, which the team shares: what they already have, to get your bearings (`search` or `get` an id for more; mention it to the user only if it helps):\n",
        space_name(db, space)?
    );
    if !others.is_empty() {
        s.push_str("Other projects of the space (most active first, with where work was left):\n");
        for p in others.iter().take(PROYECTOS) {
            s.push_str(&format!("- {} ({} memories)", p.name, p.memories));
            if let Some(m) = statuses.iter().find(|m| m.project_id == p.id) {
                s.push_str(&format!(": {} — {} (id {})", m.title, snippet(&m.content, SNIPPET), m.id.chars().take(12).collect::<String>()));
            }
            s.push('\n');
        }
        if others.len() > PROYECTOS {
            s.push_str("…and more (`context` lists them).\n");
        }
    }
    if !open.is_empty() {
        s.push_str("Decisions still open in those projects (the user's to make):\n");
        for m in open.iter().take(ABIERTAS) {
            // By its name: a project that hasn't been opened here yet has no folder.
            let p: String = db
                .query_row("SELECT name FROM project WHERE id = ?1", [m.project_id], |r| r.get(0))
                .optional()?
                .filter(|n: &String| !n.is_empty())
                .unwrap_or_else(|| crate::proyecto::nombre_de(&m.project, &m.project_key));
            let mark = if m.bloqueante() { "BLOCKING " } else { "" };
            s.push_str(&format!("- {mark}[project {p}] {} (id {})\n", m.title, m.id.chars().take(12).collect::<String>()));
        }
        if open.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`search` with the tag `open` and `space`).\n", open.len() - ABIERTAS));
        }
    }
    if !authors.is_empty() {
        let v: Vec<String> = authors
            .iter()
            .take(AUTORES)
            .map(|(a, n)| if own == Some(a.as_str()) { format!("{a} ({n}, the user)") } else { format!("{a} ({n})") })
            .collect();
        s.push_str(&format!("Who writes in this space (memories each): {}.\n", v.join(", ")));
    }
    // Titles and contents are other people's text: they must not be able to close the block.
    Ok(Some(crate::recall::citado(&s)))
}

/// What just arrived from a space on joining its repository, to tell the person: its memories, its
/// projects, what is pinned and the open decisions, and that the agent's next session in those
/// projects will already have it. Nothing if the space isn't there or arrived empty.
pub fn al_unirse(db: &Connection, space: &str) -> Result<Option<String>> {
    let Some(s) = crate::proyecto::espacio_de_clave(db, &crate::proyecto::clave_de_espacio(space))? else { return Ok(None) };
    let (memories, pinned): (i64, i64) = db.query_row(
        "SELECT count(*), COALESCE(sum(pinned), 0) FROM memory
         WHERE space_id = ?1 OR project_id IN (SELECT id FROM project WHERE space_id = ?1)",
        [s],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if memories == 0 {
        return Ok(None);
    }
    let open = memoria::abiertas(db, &Ambito::Espacio(Some(s)), usize::MAX)?.into_iter().filter(|m| !m.global()).count();
    let projects: Vec<String> = crate::proyecto::lista(db, None, Some(Some(s)), 50)?.into_iter().map(|p| p.name).collect();
    let names = if projects.len() > 6 { format!("{}, …", projects[..6].join(", ")) } else { projects.join(", ") };
    let mut t = tr!(
        "  From {space}: {memories} memories in {} project(s) ({names}), {pinned} pinned, {open} open decision(s).",
        "  De {space}: {memories} memorias en {} proyecto(s) ({names}), {pinned} fijadas, {open} decisión(es) abierta(s).",
        projects.len()
    );
    t.push('\n');
    t.push_str(&tr!(
        "  Your agent starts its next session in any of those projects with them, plus a summary of the rest of {space} the first time. The first time you open a project's folder here, the agent may ask whether it is {space}'s; if it is, `omoi projects keep <project>`.",
        "  Tu agente empieza su próxima sesión en cualquiera de esos proyectos con ellas, y la primera vez con un resumen del resto de {space}. La primera vez que abras aquí la carpeta de un proyecto, puede que el agente pregunte si es de {space}; si lo es, `omoi projects keep <proyecto>`."
    ));
    Ok(Some(t))
}
