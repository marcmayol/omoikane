//! Servidor MCP por stdio: lo que la CLI hace, como herramientas para el agente.
//!
//! Cada llamada abre la base en un hilo aparte (rusqlite es bloqueante) y resuelve el proyecto por
//! la carpeta del proceso, que es la del proyecto en el que el cliente arrancó el servidor. El
//! modelo se abre una vez y se comparte; si no estaba instalado, se vuelve a mirar en cada llamada.

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

const INSTRUCCIONES: &str = "Omoikane is this user's long-term memory across sessions, scoped to the current project (plus global memories).
Save a memory with `remember` when you learn something worth knowing next time: a non-obvious bug or trap (kind gotcha, with the file paths it affects), a decision and its reasons (decision), how something in the project works (fact), how the user wants things done (preference), or a procedure that gets repeated, like publishing a release (procedure: numbered steps, plus a line `when: words, that, trigger it`; it comes back whole when the user asks for it).
When you follow a procedure and a step fails or has changed, `update` the procedure with what you learned (and why), so it is right next time.
Keep each memory short: one-line title, then what, why and where. Don't save what is obvious from the code or already in the repo docs.
Write each memory in the language the user writes to you in (not in English if they write in Spanish): automatic recall matches the user's own words, so a memory in another language won't come back when it should.
Before working on something, `search` for it. If `remember` answers duplicate_of, update that memory instead (or set force if it really is different).
If the session runs in a folder that is not a project (such as the home folder), a memory about one project goes to that project: pass its folder as `project`, or its absolute paths in `paths`. Otherwise it is saved as global.
Projects can belong to a space (a client, a product with several repositories): what applies to every project of the space but not to other clients goes with `space: true` instead of `global`. From a project you see its memories, its space's and the global ones, never another space's. `search` and `recent` take `project` (a folder or part of a project's name) and `space`; `context` says where you are and lists the projects.
If the user asks to continue something and it is unclear what, do at most one `search` (or `recent`); if that doesn't settle it, ask the user instead of exploring further.
When you finish a piece of work or the session winds down, save where it was left (what is done, what is next) with the tag `status`, so the next session can pick it up; keep one per project: if it already has one, `update` it.
When something you remembered changes, `update` it with a reason; `forget` it only when it is wrong or no longer applies (it goes to the trash for 30 days; `update` with restore=true brings it back).
To fix one part of a memory use `update` with `replace` ([{find, with}], each `find` literal and found exactly once); to add a step, `append`; to merge several memories into one, `update` the one that stays with the merged text in `content` and the others' ids in `absorb` (they go to the trash, superseded by it, and their tags, paths and history move to it).
A memory marked ⚠ may be outdated: before relying on it, look at the newer one it names; then `update` the old one (or `forget` it if the new one replaces it), asking the user if it is not clear.
When one memory replaces another, say so: `supersedes` (in `remember` or `update`) with the old one's id. When two disagree and you can't tell which holds, `contradicts`; when a warning pairs two that both hold, `compatible`. That judgment is kept, travels with sync and shows wherever the memories show up.
Call `context` at the start of a session to get the user's preferences and pinned memories for this project.";

#[derive(Clone)]
pub struct Servidor {
    db: PathBuf,
    dir: Option<PathBuf>,
    modelo: Arc<Mutex<Option<Arc<Modelo>>>>,
    tool_router: ToolRouter<Self>,
}

// --- Parámetros ------------------------------------------------------------------------------------

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
    /// Free tags.
    tags: Option<Vec<String>>,
    /// Always shown in `context` for this project.
    pinned: Option<bool>,
    /// Applies to every project, not just this one (typical for user preferences).
    global: Option<bool>,
    /// Applies to every project of this project's space (true), or of the space with this name: for
    /// what is common to one client's repositories and must not reach other clients.
    space: Option<EspacioParam>,
    /// Folder of the project it belongs to, when it is not the current one (for example, the session
    /// runs in the home folder). By default, the current project; from a folder that is not a project,
    /// the repository its absolute paths point to, or global.
    project: Option<String>,
    /// Save it even if a very similar memory exists.
    force: Option<bool>,
    /// Ids of memories this one replaces: they will show up as superseded by it.
    supersedes: Option<Vec<String>>,
    /// Ids of memories this one disagrees with, when it's not clear which holds.
    contradicts: Option<Vec<String>>,
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

/// `space` en `remember`: `true` (el espacio del proyecto) o el nombre de un espacio.
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
    /// Why it changes; kept with the previous version.
    reason: Option<String>,
    /// Ids of memories this one replaces: they will show up as superseded by it.
    supersedes: Option<Vec<String>>,
    /// Ids of memories this one disagrees with, when it's not clear which holds.
    contradicts: Option<Vec<String>>,
    /// Ids of memories flagged against this one that both hold: clears the warning.
    compatible: Option<Vec<String>>,
    /// Bring a forgotten memory back from the trash (with its history, on every synced computer). The other fields are ignored.
    restore: Option<bool>,
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
}

// --- Respuestas ------------------------------------------------------------------------------------

/// Lo que se devuelve en listas: lo justo para decidir si pedir la memoria entera con `get`.
#[derive(Serialize)]
struct Compacta {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// Su espacio, si es de espacio (sin proyecto) o si se enseña el proyecto.
    #[serde(skip_serializing_if = "Option::is_none")]
    space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f32>,
    /// Por qué tomarla con cuidado (sustituida, contradicha, desfasada, sin revisar).
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

const LARGO_FRAGMENTO: usize = 200;

fn compacta(m: Memoria, con_proyecto: bool, similarity: Option<f32>) -> Compacta {
    let plano = m.content.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = if plano.chars().count() > LARGO_FRAGMENTO {
        format!("{}…", plano.chars().take(LARGO_FRAGMENTO).collect::<String>())
    } else {
        plano
    };
    Compacta {
        global: m.global(),
        project: (con_proyecto && !m.project.is_empty()).then(|| m.project.clone()),
        space: ((m.de_espacio() || con_proyecto) && !m.space.is_empty()).then(|| m.space.clone()),
        id: m.id,
        kind: m.kind,
        title: m.title,
        snippet,
        paths: m.paths,
        tags: m.tags,
        pinned: m.pinned,
        similarity: similarity.map(|s| (s * 1000.0).round() / 1000.0),
        note: None,
    }
}

/// Los veredictos que el agente da con `supersedes`, `contradicts` y `compatible` sobre `id`, con su
/// motivo (el `reason` del `update`, o el título de la memoria nueva) y quién los firma (`origen`:
/// la persona de la sync o `agent:<cliente>`, ver `veredictos::autor`).
fn juzgar(db: &Connection, id: &str, supersedes: Option<Vec<String>>, contradicts: Option<Vec<String>>, compatible: Option<Vec<String>>, motivo: &str, origen: &str) -> Result<usize> {
    let autor = veredictos::autor(db, origen);
    let mut n = 0;
    for viejo in supersedes.unwrap_or_default() {
        veredictos::poner(db, &viejo, id, veredictos::SUSTITUIDA, motivo, &autor)?;
        n += 1;
    }
    for (lista, relacion) in [(contradicts, veredictos::CONTRADICE), (compatible, veredictos::COMPATIBLE)] {
        for otra in lista.unwrap_or_default() {
            veredictos::poner(db, id, &otra, relacion, motivo, &autor)?;
            n += 1;
        }
    }
    Ok(n)
}

/// `redacted: N` y los tipos, si al guardar se quitó algo que parecía un secreto: el agente tiene que
/// saberlo (y decírselo a la persona), porque lo que guardó ya no es lo que mandó.
fn con_redactados(r: &mut serde_json::Value, m: &Memoria) {
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

/// Un fallo de la herramienta (no existe, tipo mal escrito…) vuelve como resultado con isError,
/// para que el agente lo lea y lo corrija, en vez de como error de protocolo.
fn fallo(e: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])
}

fn tipo(t: Option<String>) -> Result<Option<Tipo>> {
    t.filter(|t| !t.trim().is_empty()).map(|t| t.parse()).transpose()
}

fn limite(n: Option<usize>) -> usize {
    n.unwrap_or(10).clamp(1, 50)
}

/// El ámbito de `search` y `recent`: el de la sesión, o el que pidan (`project`, `space`,
/// `all_projects`), sin salir nunca del espacio de la sesión (desde una carpeta general, todo).
fn ambito_pedido(db: &Connection, proyecto: &str, project: Option<&str>, space: Option<&str>, todos: bool) -> Result<Ambito> {
    let base = Ambito::de(db, proyecto)?;
    let pedido = if let Some(s) = space.map(str::trim).filter(|s| !s.is_empty()) {
        Ambito::Espacio(Some(proyecto::buscar_espacio(db, s)?))
    } else if let Some(p) = project.map(str::trim).filter(|p| !p.is_empty()) {
        let id = proyecto::buscar(db, p)?;
        Ambito::Proyecto { id, space: proyecto::espacio_de(db, id)? }
    } else if todos {
        base.amplio()
    } else {
        base
    };
    fuera_de_espacio(&base, &pedido)?;
    Ok(pedido)
}

/// Error si desde `base` no se puede ver (ni escribir en) `pedido`: otro espacio.
fn fuera_de_espacio(base: &Ambito, pedido: &Ambito) -> Result<()> {
    anyhow::ensure!(
        base.permite(pedido),
        "that is in another space: from this project you only see its own space (and the global memories); open the session in that project, or in the home folder, to see it"
    );
    Ok(())
}

/// El espacio de `remember` (`space: true` o su nombre) o de `update` (`space: "<nombre>"`).
fn espacio_pedido(db: &Connection, proyecto: &str, p: Option<EspacioParam>) -> Result<Option<i64>> {
    let base = Ambito::de(db, proyecto)?;
    let id = match p {
        None | Some(EspacioParam::Si(false)) => return Ok(None),
        Some(EspacioParam::Si(true)) => base.espacio().ok_or_else(|| {
            anyhow::anyhow!("this project is not in a space: give the space's name, or put the project in one with `omoi space assign <space>`")
        })?,
        Some(EspacioParam::Nombre(n)) if n.trim().is_empty() => return Ok(None),
        Some(EspacioParam::Nombre(n)) => proyecto::buscar_espacio(db, &n)?,
    };
    fuera_de_espacio(&base, &Ambito::Espacio(Some(id)))?;
    Ok(Some(id))
}

// --- Servidor --------------------------------------------------------------------------------------

impl Servidor {
    pub fn new(db: PathBuf, dir: Option<PathBuf>) -> Servidor {
        Servidor { db, dir, modelo: Arc::new(Mutex::new(None)), tool_router: Self::tool_router() }
    }

    fn modelo(&self) -> Result<Option<Arc<Modelo>>> {
        let mut m = self.modelo.lock().expect("modelo");
        if m.is_none() {
            *m = modelos::abrir_actual()?.map(Arc::new);
        }
        Ok(m.clone())
    }

    /// Ejecuta `f` en un hilo de bloqueo con la base abierta, el modelo (si hay) con los vectores al
    /// día y el proyecto actual.
    async fn con_base<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection, Option<&Modelo>, &str) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (ruta, dir, modelo) = (self.db.clone(), self.dir.clone(), self.modelo()?);
        tokio::task::spawn_blocking(move || {
            let mut db = db::abrir(&ruta)?;
            if let Some(m) = &modelo {
                vectores::sincronizar(&mut db, m)?;
            }
            let proyecto = match &dir {
                Some(d) => proyecto::de(d)?,
                None => proyecto::actual()?,
            };
            f(&mut db, modelo.as_deref(), &proyecto)
        })
        .await?
    }

    /// `remember`, con quién la guarda (`source`): `mcp:<cliente>` por MCP, lo que diga quien llama
    /// por `omoi tool` o HTTP.
    async fn guardar(&self, p: Remember, origen: String) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, modelo, proyecto| {
                let kind = Some(p.kind.parse::<Tipo>()?);
                let space_id = espacio_pedido(db, proyecto, p.space)?;
                let (project, paths) = match p.project.as_deref().map(str::trim) {
                    _ if p.global.unwrap_or(false) => (String::new(), p.paths.unwrap_or_default()),
                    Some("") => (String::new(), p.paths.unwrap_or_default()),
                    Some(d) => (proyecto::de_carpeta(std::path::Path::new(d))?, p.paths.unwrap_or_default()),
                    None => proyecto::para_guardar(proyecto, &p.paths.unwrap_or_default(), &format!("{} {}", p.title, p.content)),
                };
                let busca_en = if !project.is_empty() && proyecto::es_general(proyecto) { proyecto.to_string() } else { project.clone() };
                if !p.force.unwrap_or(false)
                    && let Some(m) = modelo
                    && let Some((existente, sim)) = vectores::duplicada(db, m, &busca_en, &p.title, &p.content)?
                {
                    return Ok(serde_json::json!({
                        "saved": false,
                        "duplicate_of": compacta(existente, false, None),
                        "similarity": (sim * 1000.0).round() / 1000.0,
                        "hint": "A very similar memory exists. Update it with `update` (give a reason), or call `remember` again with force=true if it is a different memory.",
                    }));
                }
                if space_id.is_none() && !project.is_empty() {
                    fuera_de_espacio(&Ambito::de(db, proyecto)?, &Ambito::de(db, &project)?)?;
                }
                let nueva = Nueva {
                    project,
                    space_id,
                    kind,
                    title: p.title,
                    content: p.content,
                    paths,
                    tags: p.tags.unwrap_or_default(),
                    pinned: p.pinned.unwrap_or(false),
                    source: origen.clone(),
                    ..Default::default()
                };
                let m = memoria::guardar(db, nueva, modelo)?;
                // El motivo de un veredicto dado al guardar es la propia memoria nueva: su título.
                let juzgadas = juzgar(db, &m.id, p.supersedes, p.contradicts, None, &m.title, &origen)?;
                let donde = if m.de_espacio() { format!("(space {})", m.space) } else if m.project.is_empty() { "(global)".to_string() } else { m.project.clone() };
                let mut r = serde_json::json!({ "saved": true, "id": m.id, "project": donde });
                con_redactados(&mut r, &m);
                if juzgadas > 0 {
                    r["judged"] = serde_json::json!(juzgadas);
                }
                let rel = vectores::relacionadas(db, &busca_en, &m.title, &m.id, 3)?;
                if !rel.is_empty() {
                    r["related"] = serde_json::json!(rel.into_iter().map(|x| compacta(x, false, None)).collect::<Vec<_>>());
                    r["related_hint"] = serde_json::json!("Saved. These memories may be about the same thing: if one already says this, merge them (`update` that one and `forget` the new one); if not, ignore this.");
                }
                // Si deja anticuada a otra (versión más alta, o hecho lo que allí estaba pendiente), se dice ya.
                crate::revisar::al_dia(db)?;
                let viejas: Vec<_> = crate::revisar::superadas_por(db, &m.id)?
                    .into_iter()
                    .filter(|(id, _, _)| veredictos::parejas(db).map(|p| !p.contains_key(&veredictos::pareja(id, &m.id))).unwrap_or(true))
                    .collect();
                if !viejas.is_empty() {
                    // Una que ya está sustituida (quizá por una cadena) se dice, con el final de la
                    // cadena: el agente no tiene que volver a juzgarla.
                    let mut lista = Vec::new();
                    let mut encadenadas = false;
                    for (id, titulo, detalle) in &viejas {
                        let mut e = serde_json::json!({ "id": id, "title": titulo, "why": detalle });
                        if let Some(fin) = veredictos::cadena(db, id)? {
                            let t: String = db.query_row("SELECT title FROM memory WHERE id = ?1", [&fin.id], |f| f.get(0))?;
                            let via = if fin.saltos > 1 { format!(" (via {})", fin.saltos) } else { String::new() };
                            e["already_superseded_by"] = serde_json::json!(format!("{} «{}»{via}", fin.id.chars().take(12).collect::<String>(), t.chars().take(80).collect::<String>()));
                            encadenadas = true;
                        }
                        lista.push(e);
                    }
                    r["supersedes"] = serde_json::json!(lista);
                    let mut hint = "Saved. It looks newer than these memories, which may now be outdated: if this one replaces them, `update` it with `supersedes` (or forget them); if both hold, `compatible`.".to_string();
                    if encadenadas {
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
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }

    /// `update`, con quién lo pide (`origen`, como en `guardar`): firma los veredictos que traiga.
    async fn cambiar(&self, p: Update, origen: String) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, modelo, proyecto| {
                if p.restore.unwrap_or(false) {
                    return Ok(serde_json::to_value(compacta(memoria::restaurar(db, &p.id, modelo)?, false, None))?);
                }
                // El motivo del cambio es también el de los veredictos que vengan con él.
                let motivo = p.reason.clone().unwrap_or_default();
                let space_id = espacio_pedido(db, proyecto, p.space.map(EspacioParam::Nombre))?;
                let c = Cambios {
                    space_id,
                    title: p.title,
                    content: p.content,
                    kind: tipo(p.kind)?,
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
                let absorbe = p.absorb.filter(|ids| !ids.is_empty());
                // Solo veredictos: la memoria no cambia (ni gana una versión vacía en su historial).
                let solo_juicio = c.title.is_none()
                    && c.content.is_none()
                    && c.kind.is_none()
                    && c.paths.is_none()
                    && c.tags.is_none()
                    && c.pinned.is_none()
                    && c.project.is_none()
                    && c.space_id.is_none()
                    && c.replace.is_empty()
                    && c.append.is_none()
                    && absorbe.is_none();
                let (m, cambiados, absorbidas) = if solo_juicio {
                    (memoria::obtener(db, &p.id)?, Vec::new(), Vec::new())
                } else if let Some(ids) = absorbe {
                    memoria::absorber(db, &p.id, &ids, c, modelo)?
                } else {
                    let (m, cambiados) = memoria::actualizar_con_cambios(db, &p.id, c, modelo)?;
                    (m, cambiados, Vec::new())
                };
                juzgar(db, &m.id, p.supersedes, p.contradicts, p.compatible, &motivo, &origen)?;
                crate::revisar::al_dia(db)?;
                let mut r = serde_json::to_value(compacta(m.clone(), false, None))?;
                con_redactados(&mut r, &m);
                if !cambiados.is_empty() {
                    r["changed"] = serde_json::json!(cambiados);
                }
                if !absorbidas.is_empty() {
                    r["absorbed"] = serde_json::json!(absorbidas);
                }
                Ok(r)
            })
            .await;
        if r.is_ok() {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }
}

/// `mcp:<nombre del cliente>` según lo que el cliente dijo al conectarse (`clientInfo`); `mcp` si no
/// lo dijo.
fn origen_de(peer: &Peer<RoleServer>) -> String {
    let nombre = peer.peer_info().map(|i| i.client_info.name.trim().to_lowercase()).unwrap_or_default();
    if nombre.is_empty() { "mcp".into() } else { format!("mcp:{nombre}") }
}

#[tool_router(router = tool_router)]
impl Servidor {
    #[tool(description = "Save a memory for future sessions. If a very similar memory already exists in this project, nothing is saved and you get duplicate_of: update that one instead, or call again with force=true if it really is different.")]
    async fn remember(&self, peer: Peer<RoleServer>, Parameters(p): Parameters<Remember>) -> Result<CallToolResult, ErrorData> {
        self.guardar(p, origen_de(&peer)).await
    }

    #[tool(description = "Search the memories of this project, its space and the global ones (or every project of the space with all_projects, another project with `project`, a space with `space`; from a folder that is not a project, such as the home folder, every project). Returns compact results; use `get` for the whole memory.")]
    async fn search(&self, Parameters(p): Parameters<Search>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, modelo, proyecto| {
                let ambito = ambito_pedido(db, proyecto, p.project.as_deref(), p.space.as_deref(), p.all_projects.unwrap_or(false))?;
                let todos = !matches!(ambito, Ambito::Proyecto { .. });
                let o = Opciones {
                    ambito,
                    kind: tipo(p.kind)?,
                    tags: p.tags.unwrap_or_default(),
                    limite: limite(p.limit),
                };
                let res = buscar::buscar(db, &p.query, &o, modelo)?;
                let mut notas = crate::revisar::notas_busqueda(db, &res.iter().map(|r| &r.memoria).collect::<Vec<_>>())?;
                Ok(res
                    .into_iter()
                    .map(|r| {
                        let note = notas.remove(&r.memoria.id);
                        Compacta { note, ..compacta(r.memoria, todos, r.similarity) }
                    })
                    .collect::<Vec<_>>())
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }

    #[tool(description = "Get a whole memory by id.")]
    async fn get(&self, Parameters(p): Parameters<Id>) -> Result<CallToolResult, ErrorData> {
        let r = self.con_base(move |db, _, _| memoria::obtener(db, &p.id)).await;
        Ok(r.map(|m| json(&m)).unwrap_or_else(fallo))
    }

    #[tool(description = "Change a memory. Only the fields you pass change; the previous version is kept in its history with your reason.")]
    async fn update(&self, peer: Peer<RoleServer>, Parameters(p): Parameters<Update>) -> Result<CallToolResult, ErrorData> {
        self.cambiar(p, origen_de(&peer)).await
    }

    #[tool(description = "Forget a memory: it goes to the trash with its history for 30 days, and `update` with restore=true brings it back on every synced computer. Use it only when the memory is wrong or no longer applies; if it just changed, use `update`.")]
    async fn forget(&self, Parameters(p): Parameters<Id>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, _, _| {
                let m = memoria::olvidar(db, &p.id)?;
                crate::revisar::al_dia(db)?;
                Ok(m)
            })
            .await;
        if r.is_ok() {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|m| json(&serde_json::json!({ "forgotten": m.id, "title": m.title }))).unwrap_or_else(fallo))
    }

    #[tool(description = "Previous versions of a memory, newest first, with the reason for each change.")]
    async fn history(&self, Parameters(p): Parameters<Id>) -> Result<CallToolResult, ErrorData> {
        let r = self.con_base(move |db, _, _| memoria::historial(db, &p.id)).await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }

    #[tool(description = "Where you are (this project: folder, sync key, space, how many memories) and the other projects (latest activity first, with their status), the user's preferences and the pinned memories for this project (its space's and the global ones), what is worth reviewing in its memory (memories a newer one contradicts, stale status, broken paths…), and what earlier sessions set aside as possibly worth saving. Call it at the start of a session.")]
    async fn context(&self) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(|db, _, proyecto| {
                let ambito = Ambito::de(db, proyecto)?;
                let v = memoria::contexto(db, &ambito, 30)?;
                let memorias: Vec<_> = v.into_iter().map(|m| compacta(m, false, None)).collect();
                let review = crate::revisar::avisos(db, &ambito)?;
                // Dónde está: el proyecto (o la carpeta general) y, de los demás, solo los de su espacio.
                let actual = match ambito {
                    Ambito::Proyecto { id, .. } if id > 0 => proyecto::lista(db, Some(id), None, 1)?.into_iter().next().map(|mut x| {
                        x.path = proyecto.to_string();
                        serde_json::to_value(x).unwrap_or_default()
                    }),
                    Ambito::Todos => Some(serde_json::json!({ "path": proyecto, "general": true, "memories": memoria::cuantas(db, &ambito)? })),
                    _ => Some(serde_json::json!({ "path": proyecto, "memories": 0 })),
                };
                let filtro = match ambito {
                    Ambito::Todos => None,
                    a => Some(a.espacio()),
                };
                let proyectos = proyecto::lista(db, None, filtro, 50)?;
                let mut r = serde_json::json!({ "project": actual, "projects": proyectos, "memories": memorias, "review": review });
                // Lo apartado al cerrar otras sesiones: en los agentes sin recall al empezar (Grok,
                // Windsurf…) solo llega por aquí. Se enseña una vez, como en el recall.
                let apartadas = crate::candidatas::para_empezar(db, proyecto, "")?;
                if !apartadas.is_empty() {
                    r["set_aside"] = serde_json::json!(apartadas.iter().map(|c| format!("[{}] {}", c.kind, c.text.chars().take(400).collect::<String>())).collect::<Vec<_>>());
                    r["set_aside_hint"] = serde_json::json!("From earlier sessions here, possibly worth keeping (picked without understanding them): save the ones worth it with `remember`, in the user's words and language, merging into existing memories where they fit; ignore the rest, they won't come back.");
                }
                Ok(r)
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }

    #[tool(description = "The latest saved or changed memories in this project (its space's and the global ones), or in `project`, `space` or every project of the space (all_projects).")]
    async fn recent(&self, Parameters(p): Parameters<Recent>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, _, proyecto| {
                let ambito = ambito_pedido(db, proyecto, p.project.as_deref(), p.space.as_deref(), p.all_projects.unwrap_or(false))?;
                let todos = !matches!(ambito, Ambito::Proyecto { .. });
                let v = memoria::recientes(db, &ambito, tipo(p.kind)?, limite(p.limit))?;
                Ok(v.into_iter().map(|m| compacta(m, todos, None)).collect::<Vec<_>>())
            })
            .await;
        Ok(r.map(|v| json(&v)).unwrap_or_else(fallo))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Servidor {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omoikane", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCCIONES)
    }
}

/// Una herramienta sin servidor, para los clientes que no hablan MCP (el mod del plugin de Claude
/// Code): las mismas herramientas y las mismas respuestas. Sin nombre, la lista de herramientas con
/// sus esquemas y las instrucciones. Devuelve el texto y si la herramienta respondió con error.
/// `origen` es el `source` de lo que se guarde con `remember` y firma los veredictos de `update`.
pub fn herramienta(db: PathBuf, dir: Option<PathBuf>, nombre: Option<&str>, args: &str, origen: &str) -> Result<(String, bool)> {
    let s = Servidor::new(db, dir);
    let Some(nombre) = nombre else {
        let lista = serde_json::json!({ "instructions": INSTRUCCIONES, "tools": s.tool_router.list_all() });
        return Ok((serde_json::to_string(&lista)?, false));
    };
    let v: serde_json::Value = if args.trim().is_empty() { serde_json::json!({}) } else { serde_json::from_str(args)? };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let r = rt.block_on(async {
        Ok::<_, anyhow::Error>(match nombre {
            "remember" => s.guardar(serde_json::from_value(v)?, origen.to_string()).await,
            "search" => s.search(Parameters(serde_json::from_value(v)?)).await,
            "get" => s.get(Parameters(serde_json::from_value(v)?)).await,
            "update" => s.cambiar(serde_json::from_value(v)?, origen.to_string()).await,
            "forget" => s.forget(Parameters(serde_json::from_value(v)?)).await,
            "history" => s.history(Parameters(serde_json::from_value(v)?)).await,
            "context" => s.context().await,
            "recent" => s.recent(Parameters(serde_json::from_value(v)?)).await,
            otro => anyhow::bail!("unknown tool: {otro}"),
        })
    })?;
    let r = r.map_err(|e| anyhow::anyhow!("{}", e.message))?;
    let texto = r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.as_str())).collect::<Vec<_>>().join("\n");
    Ok((texto, r.is_error.unwrap_or(false)))
}

/// Sirve por stdin/stdout hasta que el cliente cierre. Nada más puede escribir en stdout.
pub fn servir(db: PathBuf, dir: Option<PathBuf>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let servicio = Servidor::new(db, dir).serve(rmcp::transport::stdio()).await?;
        servicio.waiting().await?;
        Ok(())
    })
}
