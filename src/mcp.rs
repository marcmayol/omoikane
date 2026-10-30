//! Servidor MCP por stdio: lo que la CLI hace, como herramientas para el agente.
//!
//! Cada llamada abre la base en un hilo aparte (rusqlite es bloqueante) y resuelve el proyecto por
//! la carpeta del proceso, que es la del proyecto en el que el cliente arrancó el servidor. El
//! modelo se abre una vez y se comparte; si no estaba instalado, se vuelve a mirar en cada llamada.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo};
use crate::{db, modelos, proyecto, vectores};
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
If the user asks to continue something and it is unclear what, do at most one `search` (or `recent`); if that doesn't settle it, ask the user instead of exploring further.
When you finish a piece of work or the session winds down, save where it was left (what is done, what is next) with the tag `status`, so the next session can pick it up; keep one per project: if it already has one, `update` it.
When something you remembered changes, `update` it with a reason; `forget` it only when it is wrong or no longer applies.
A memory marked ⚠ may be outdated: before relying on it, look at the newer one it names; then `update` the old one (or `forget` it if the new one replaces it), asking the user if it is not clear.
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
    /// Folder of the project it belongs to, when it is not the current one (for example, the session
    /// runs in the home folder). By default, the current project; from a folder that is not a project,
    /// the repository its absolute paths point to, or global.
    project: Option<String>,
    /// Save it even if a very similar memory exists.
    force: Option<bool>,
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
    /// Search every project, not just this one and the global memories.
    all_projects: Option<bool>,
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
    /// Why it changes; kept with the previous version.
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Recent {
    /// How many (1-50, default 10).
    limit: Option<usize>,
    /// Only this kind: fact, decision, gotcha, preference or procedure.
    kind: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity: Option<f32>,
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
        global: m.project.is_empty(),
        project: (con_proyecto && !m.project.is_empty()).then(|| m.project.clone()),
        id: m.id,
        kind: m.kind,
        title: m.title,
        snippet,
        paths: m.paths,
        tags: m.tags,
        pinned: m.pinned,
        similarity: similarity.map(|s| (s * 1000.0).round() / 1000.0),
    }
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
                let nueva = Nueva {
                    project,
                    kind,
                    title: p.title,
                    content: p.content,
                    paths,
                    tags: p.tags.unwrap_or_default(),
                    pinned: p.pinned.unwrap_or(false),
                    source: origen,
                };
                let m = memoria::guardar(db, nueva, modelo)?;
                let mut r = serde_json::json!({ "saved": true, "id": m.id, "project": if m.project.is_empty() { "(global)" } else { m.project.as_str() } });
                let rel = vectores::relacionadas(db, &busca_en, &m.title, &m.id, 3)?;
                if !rel.is_empty() {
                    r["related"] = serde_json::json!(rel.into_iter().map(|x| compacta(x, false, None)).collect::<Vec<_>>());
                    r["related_hint"] = serde_json::json!("Saved. These memories may be about the same thing: if one already says this, merge them (`update` that one and `forget` the new one); if not, ignore this.");
                }
                // Si deja anticuada a otra (versión más alta, o hecho lo que allí estaba pendiente), se dice ya.
                crate::revisar::al_dia(db)?;
                let viejas = crate::revisar::superadas_por(db, &m.id)?;
                if !viejas.is_empty() {
                    r["supersedes"] = serde_json::json!(viejas.iter().map(|(id, titulo, detalle)| serde_json::json!({ "id": id, "title": titulo, "why": detalle })).collect::<Vec<_>>());
                    r["supersedes_hint"] = serde_json::json!("Saved. It looks newer than these memories, which may now be outdated: update them (or forget them if this one replaces them).");
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

    #[tool(description = "Search the memories of this project and the global ones (or every project with all_projects; from a folder that is not a project, such as the home folder, every project). Returns compact results; use `get` for the whole memory.")]
    async fn search(&self, Parameters(p): Parameters<Search>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, modelo, proyecto| {
                let ambito = if p.all_projects.unwrap_or(false) { Ambito::Todos } else { Ambito::de(proyecto) };
                let todos = matches!(ambito, Ambito::Todos);
                let o = Opciones {
                    ambito,
                    kind: tipo(p.kind)?,
                    tags: p.tags.unwrap_or_default(),
                    limite: limite(p.limit),
                };
                let res = buscar::buscar(db, &p.query, &o, modelo)?;
                Ok(res.into_iter().map(|r| compacta(r.memoria, todos, r.similarity)).collect::<Vec<_>>())
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
    async fn update(&self, Parameters(p): Parameters<Update>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, modelo, _| {
                let c = Cambios {
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
                };
                let m = memoria::actualizar(db, &p.id, c, modelo)?;
                crate::revisar::al_dia(db)?;
                Ok(m)
            })
            .await;
        if r.is_ok() {
            crate::sync::programar(&self.db);
        }
        Ok(r.map(|m| json(&compacta(m, false, None))).unwrap_or_else(fallo))
    }

    #[tool(description = "Delete a memory and its history. Use it only when the memory is wrong or no longer applies; if it just changed, use `update`.")]
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

    #[tool(description = "The user's preferences and the pinned memories for this project (and the global ones), what is worth reviewing in its memory (memories a newer one contradicts, stale status, broken paths…), and what earlier sessions set aside as possibly worth saving. Call it at the start of a session.")]
    async fn context(&self) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(|db, _, proyecto| {
                let v = memoria::contexto(db, proyecto, 30)?;
                let memorias: Vec<_> = v.into_iter().map(|m| compacta(m, false, None)).collect();
                let review = crate::revisar::avisos(db, &Ambito::de(proyecto))?;
                let mut r = serde_json::json!({ "memories": memorias, "review": review });
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

    #[tool(description = "The latest saved or changed memories in this project (and the global ones).")]
    async fn recent(&self, Parameters(p): Parameters<Recent>) -> Result<CallToolResult, ErrorData> {
        let r = self
            .con_base(move |db, _, proyecto| {
                let ambito = Ambito::de(proyecto);
                let todos = matches!(ambito, Ambito::Todos);
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
/// `origen` es el `source` de lo que se guarde con `remember`.
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
            "update" => s.update(Parameters(serde_json::from_value(v)?)).await,
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
