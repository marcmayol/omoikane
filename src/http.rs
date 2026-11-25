//! `omoi serve`: the memory over local HTTP, for whatever doesn't speak MCP (scripts, other apps). The
//! same tools as the MCP server and with the same responses:
//!
//! - `GET /` → the web view: the memory timeline and the projects with their state (and `/icono.png`,
//!   `/fuentes/*.woff2`, which the page loads from the binary)
//! - `GET /api/memories` → the visible memories (see «Who sees what»), whole, newest to oldest, with
//!   `versions` (how many previous versions its history keeps) and `links` (the ones it links with `[[…]]`)
//! - `GET /api/projects` → the projects (`project::list`: key, space, folder and whether it is gone)
//!   and the spaces (`project::spaces`, with their name), to group and filter by space
//! - `GET /api/overview?months=…` → the owner view (`owner_view::overview`): per space, its
//!   projects with their latest `status`, the pinned ones, the open decisions, who wrote what, what
//!   `review` says, what is paused and what nobody has touched for `months` months (6). Only what
//!   is visible from the request's project (its space); from the user's folder, everything
//! - `GET /api/review` → what is worth reviewing (`review::notices`), and `POST /api/review/dismiss` with
//!   `{memory_id, kind, other_id}` for «fine as is»
//! - `POST /api/contradiction` with `{a, b, keep, reason}` → which of two contradicting ones holds (1.14):
//!   `keep` is the id of the one that holds (the other becomes superseded) or `both`
//! - `GET /api/candidates` → what was set aside from conversations (`candidates::all`), and
//!   `POST /api/candidates/dismiss` with `{id}` to drop it
//! - `GET /api/trash` → the trash (`memory::trash`), and `POST /api/restore` with `{id}` to
//!   bring a memory back
//! - `POST /api/run` with `{command}` → Review's «Run» button: one of the commands the view itself
//!   proposes (merge or separate projects, pause one where it is…), from a closed list
//!   (`allowed_command`). Runs the same binary on the same database, never a shell; asks for the view
//!   token, like «Verify»
//! - `POST /api/split` with `{id, parts: [{title, content}], reason}` → Review's «Split» button:
//!   the first part stays in the memory and the rest are new memories in its same project or
//!   space (`memory::split`)
//! - `POST /api/redact` with `{id}` → the memory without whatever looks like a secret (`memory::redact`), with
//!   the reason «redacted» in its history
//! - `GET /health` → `{"ok": true, "version": …}`
//! - `GET /tools` → names, descriptions and schemas of the tools
//! - `POST /tools/<name>` with the arguments as JSON → the tool's response (400 if it is an error)
//! - `GET /search?q=…&limit=…` → shortcut for the `search` tool
//!
//! It only listens on `127.0.0.1`, and only serves requests addressed to `127.0.0.1` or `localhost` (so
//! that an arbitrary website can't reach it by changing its DNS) and, if they write, made from the view
//! itself (a foreign website can't send a POST that forgets memories). With `OMOIKANE_HTTP_TOKEN`, every
//! request (except the page, which carries no data) must carry `Authorization: Bearer <token>`.
//! `POST /api/verify` and `POST /api/skill` (the view's «Verify» and «Make skill» buttons) also ask for the view token, a new one on
//! each `omoi serve` that only lives inside the page (`X-Omoikane-View`): it stops a `curl` with a
//! made-up `Origin`, not an agent with a shell set on loading the page to extract it.
//!
//! **Who sees what** (1.7.0), the same rule as the CLI and MCP: it is decided by the folder the server
//! started in. From a project in a space, that whole space (its projects, the space's memories) and the
//! global ones; from a project without a space, the projects without a space and the global ones; from the
//! user's folder, everything. It applies to everything it reads (memories, projects, review, trash, candidates,
//! overview, tools) and to what it writes: touching a memory from outside gives 403. `?dir=<folder>`
//! picks the project for that request, but only within what is already visible (from a project, another in its
//! same space; from the user's folder, any): it never widens anything (403).

use crate::memory::{self, Scope};
use crate::{db, mcp, tr};
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use tiny_http::{Header, Method, Response, Server};

pub const PORT: u16 = 7438;

fn json(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let header = Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("cabecera");
    Response::from_string(body).with_status_code(status).with_header(header)
}

const PAGE: &str = include_str!("web.html");

/// What the page loads separately: the icon and its fonts (OFL, in `assets/fonts`), inside the binary
/// because the view has to work without internet.
const ASSETS: &[(&str, &str, &[u8])] = &[
    ("/icono.png", "image/png", include_bytes!("../assets/icon-64.png")),
    ("/fuentes/plex-sans.woff2", "font/woff2", include_bytes!("../assets/fonts/plex-sans.woff2")),
    ("/fuentes/plex-mono-400.woff2", "font/woff2", include_bytes!("../assets/fonts/plex-mono-400.woff2")),
    ("/fuentes/plex-mono-600.woff2", "font/woff2", include_bytes!("../assets/fonts/plex-mono-600.woff2")),
    ("/fuentes/shippori-600.woff2", "font/woff2", include_bytes!("../assets/fonts/shippori-600.woff2")),
    ("/fuentes/shippori-600-omoikane.woff2", "font/woff2", include_bytes!("../assets/fonts/shippori-600-omoikane.woff2")),
];

fn asset(content_type: &str, data: &'static [u8]) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(data.to_vec())
        .with_header(Header::from_bytes("Content-Type", content_type).expect("cabecera"))
        .with_header(Header::from_bytes("Cache-Control", "max-age=86400").expect("cabecera"))
}

/// Where the view token goes in the page (see [`new_view_token`]).
const VIEW_MARKER: &str = "__OMOIKANE_VIEW__";
/// The header with which the page sends it back on what only a person can do (`/api/verify`).
const VIEW_HEADER: &str = "X-Omoikane-View";

/// A new token for each `omoi serve`, which only lives inside the page it serves: what `/api/verify`
/// really asks for besides the browser's `Origin` (which any `curl -H "Origin: …"` sets).
/// Reading it requires loading the page: a foreign website can't (the browser doesn't let it read the
/// response, and the `Host` check stops DNS rebinding). An agent with a shell could request the page and
/// extract it: this is not a boundary against someone set on it, but makes sure no normal agent path
/// (its tools, `/tools/`, a plain `curl`) verifies.
fn new_view_token() -> Result<String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn html(view_token: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let header = Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("cabecera");
    // The page must not be stored in any cache: it carries this run's token.
    let no_cache = Header::from_bytes("Cache-Control", "no-store").expect("cabecera");
    Response::from_string(PAGE.replace(VIEW_MARKER, view_token)).with_header(header).with_header(no_cache)
}

/// Opens the view in the system browser.
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if r.is_err() {
        eprintln!("{}", tr!("open {url} in your browser", "abre {url} en el navegador"));
    }
}

/// Whether there is already an `omoi serve` on that port.
fn already_serving(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    if s.write_all(format!("GET /health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    r.contains("\"ok\"")
}

/// Whether two secrets (the view token, the `OMOIKANE_HTTP_TOKEN` one) are equal, in constant
/// time: comparing byte by byte and stopping at the first difference lets one measure how much was guessed.
fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn error(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json(status, serde_json::json!({ "error": message }).to_string())
}

/// Decodes `%xx` and `+` in a URL parameter.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => v.push(b' '),
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(x) => {
                    v.push(x);
                    i += 2;
                }
                None => v.push(b'%'),
            },
            x => v.push(x),
        }
        i += 1;
    }
    String::from_utf8_lossy(&v).into_owned()
}

fn parse_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect()
}

/// Where a request looks from: the project (for the tools) and what is visible from it. The TUI
/// (`omoi tui`) looks with the same one, so it shows exactly what the web shows.
#[derive(Clone)]
pub(crate) struct Viewpoint {
    /// The folder passed to the tools (`?dir=` or the server's).
    dir: Option<PathBuf>,
    /// The project, as the database stores it.
    project: String,
    /// What is visible from the project (for the owner view, which widens it itself).
    pub(crate) base: Scope,
    /// «All projects» from here: its whole space, those without a space, or everything.
    pub(crate) view: Scope,
    /// Whether everything is visible (user's folder).
    pub(crate) sees_all: bool,
    /// The key of the space it looks from (empty: none, or everything).
    pub(crate) space: String,
}

impl Viewpoint {
    /// Whether this space's memories are visible (its key; empty: no space), or a global memory.
    pub(crate) fn sees_space(&self, key: &str, global: bool) -> bool {
        self.sees_all || global || key == self.space
    }

    /// Whether memory `id` is visible, live or in the trash. One that doesn't exist, yes: whoever looks it up will say so.
    pub(crate) fn sees(&self, db: &rusqlite::Connection, id: &str) -> Result<bool> {
        // A trade memory, only if some visible project has that trade.
        Ok(self.sees_all
            || (memory::memory_placement(db, id)?.is_none_or(|(global, key)| self.sees_space(&key, global)) && !crate::trades::is_hidden(db, &self.view, id)?))
    }

    /// Whether a requested scope is visible: from everything, any; otherwise, one that isn't everything and is in its same space.
    pub(crate) fn covers(&self, other: &Scope) -> bool {
        self.sees_all || (!matches!(other, Scope::All) && self.base.space() == other.space())
    }
}

/// Where the request looks from: the folder the server started in or, with `?dir=`, another one within
/// what is already visible from it. `Err` with the response if `?dir=` is not valid.
fn look_from(
    db: &rusqlite::Connection,
    server: &Viewpoint,
    requested: Option<String>,
) -> Result<std::result::Result<Viewpoint, Response<std::io::Cursor<Vec<u8>>>>> {
    let Some(d) = requested.filter(|d| !d.trim().is_empty()) else {
        return Ok(Ok(server.clone()));
    };
    // A network path is never looked at (`project::of_dir` rejects it before touching the disk): any website
    // can request this URL with an `<img>`, and looking at it would open an SMB connection with the person's
    // NTLM hash.
    if !crate::project::is_local_path(std::path::Path::new(&d)) {
        return Ok(Err(error(400, "dir: not a folder on this computer (network paths are never opened)")));
    }
    let Ok(project) = crate::project::of_dir(std::path::Path::new(&d)) else { return Ok(Err(error(400, "dir: no such folder"))) };
    let base = Scope::from_project(db, &project)?;
    if !server.covers(&base) {
        return Ok(Err(error(
            403,
            "dir is outside what this server shows: it can only narrow the view to a project of the same space; start `omoi serve` in that project, or in the home folder",
        )));
    }
    Ok(Ok(Viewpoint::new(db, Some(PathBuf::from(d)), project)?))
}

/// The trash as seen from project `project` (`""` or the user's folder: everything), with the same
/// criterion as the web and the TUI: from a client, its space and the global ones; never another client's.
/// For `omoi trash` and `omoi restore`.
pub fn trash_from(db: &rusqlite::Connection, project: &str) -> Result<(Vec<memory::Forgotten>, bool)> {
    let m = Viewpoint::new(db, None, project.to_string())?;
    let mut v = Vec::new();
    for o in memory::trash(db)? {
        if m.sees(db, &o.id)? {
            v.push(o);
        }
    }
    Ok((v, m.sees_all))
}

impl Viewpoint {
    pub(crate) fn new(db: &rusqlite::Connection, dir: Option<PathBuf>, project: String) -> Result<Viewpoint> {
        let base = Scope::from_project(db, &project)?;
        let sees_all = matches!(base, Scope::All);
        let space = crate::project::space_key(db, base.space())?;
        Ok(Viewpoint { dir, project, base, view: base.widen(), sees_all, space })
    }

    /// Where it looks from, for the view: everything, or a space (empty: the projects without a space).
    fn json(&self, db: &rusqlite::Connection) -> Result<serde_json::Value> {
        let name = if self.space.is_empty() {
            String::new()
        } else {
            db.query_row("SELECT name FROM space WHERE key = ?1", [&self.space], |f| f.get(0)).unwrap_or_default()
        };
        Ok(serde_json::json!({
            "scope": if self.sees_all { "all" } else { "space" },
            "space": if self.sees_all { serde_json::Value::Null } else { self.space.clone().into() },
            "name": name,
            "project": self.project,
        }))
    }
}

/// The JSON body of a POST.
fn json_body(request: &mut tiny_http::Request) -> Result<serde_json::Value> {
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body)?;
    Ok(serde_json::from_str(&body)?)
}

fn out_of_scope() -> Response<std::io::Cursor<Vec<u8>>> {
    error(403, "that memory is not in this scope: it belongs to another space; start `omoi serve` in that project, or in the home folder, to change it")
}

/// While serving, what other machines push arrives as in a session: every `pull_interval` (2
/// minutes) at most, with the same marker as the messages (so two don't pile up). It checks every
/// quarter of the interval; without sync the database isn't even opened.
fn spawn_periodic_pull(path: PathBuf) {
    std::thread::spawn(move || {
        loop {
            if crate::sync::is_automatic(&path)
                && let Ok(db) = db::open(&path)
            {
                crate::sync::pull_if_due(&db, &path);
            }
            std::thread::sleep((crate::sync::pull_interval() / 4).max(std::time::Duration::from_secs(1)));
        }
    });
}

/// The view's address: with the `OMOIKANE_HTTP_TOKEN` token (`#token=…`, which the page stores and
/// removes) and, with `memory`, the link to that memory (`#/memoria/<id>`, or `&to=/memoria/<id>` after the
/// token).
pub fn view_url(port: u16, token: Option<&str>, memory: Option<&str>) -> String {
    let base = format!("http://127.0.0.1:{port}/");
    match (token, memory) {
        (Some(t), Some(id)) => format!("{base}#token={t}&to=/memoria/{id}"),
        (Some(t), None) => format!("{base}#token={t}"),
        (None, Some(id)) => format!("{base}#/memoria/{id}"),
        (None, None) => base,
    }
}

/// The full id of the memory `omoi serve --memory` asks for (its prefix is enough, as in the other
/// commands), if visible from `project` (the folder the server starts in): otherwise the view wouldn't
/// have it and the link would open nothing.
pub fn linked_memory_id(db: &rusqlite::Connection, project: &str, id: &str) -> Result<String> {
    let id = memory::resolve_id(db, id)?;
    memory::in_scope(db, &Scope::from_project(db, project)?, &id).map_err(|e| {
        anyhow!(tr!(
            "--memory {id}: the web view started from this folder doesn't show it ({e}); run `omoi serve` in that memory's project or in your home folder",
            "--memory {id}: la vista que arranca desde esta carpeta no la enseña ({e}); lanza `omoi serve` en el proyecto de esa memoria o en tu carpeta personal"
        ))
    })?;
    Ok(id)
}

/// Serves requests until the process exits. With `open`, opens the view in the browser (on
/// memory `memory`, if given: its full id, see [`linked_memory_id`]); if there is already a server on that
/// port, it only opens it.
pub fn serve(db: PathBuf, dir: Option<PathBuf>, port: u16, open: bool, memory: Option<String>) -> Result<()> {
    let token = std::env::var("OMOIKANE_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    let url = view_url(port, token.as_deref(), memory.as_deref());
    if let Some(id) = &memory {
        // The link without the token: the one that can be sent in a notice (the page asks for the token separately).
        eprintln!("{}", tr!("omoikane: memory {id}: {}", "omoikane: memoria {id}: {}", view_url(port, None, Some(id))));
    }
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(_) if open && already_serving(port) => {
            open_browser(&url);
            eprintln!("{}", tr!("omoikane: already running on http://127.0.0.1:{port}", "omoikane: ya está en marcha en http://127.0.0.1:{port}"));
            return Ok(());
        }
        Err(e) => return Err(anyhow!("127.0.0.1:{port}: {e}")),
    };
    eprintln!("{}", tr!("omoikane: listening on http://127.0.0.1:{port}", "omoikane: escuchando en http://127.0.0.1:{port}"));
    if open {
        open_browser(&url);
    }
    spawn_periodic_pull(db.clone());
    // The project it started in: what each request sees depends on it (see «Who sees what»).
    let server_project = match &dir {
        Some(d) => crate::project::of_dir(d)?,
        None => crate::project::current()?,
    };
    let view_token = new_view_token()?;
    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let origins = hosts.iter().map(|a| format!("http://{a}")).collect::<Vec<_>>();
    for mut request in server.incoming_requests() {
        let response = (|| -> Result<Response<std::io::Cursor<Vec<u8>>>> {
            let header = |k: &'static str| request.headers().iter().find(|h| h.field.equiv(k)).map(|h| h.value.as_str().to_string());
            // Against «DNS rebinding»: a website that points its domain at 127.0.0.1 arrives with its own Host.
            if header("Host").is_some_and(|h| !hosts.contains(&h)) {
                return Ok(error(403, "wrong Host"));
            }
            // Against foreign websites sending a POST (a form needs no CORS permission to be submitted).
            if *request.method() != Method::Get && header("Origin").is_some_and(|o| !origins.contains(&o)) {
                return Ok(error(403, "requests from other sites are not allowed"));
            }
            let (path, _) = request.url().split_once('?').unwrap_or((request.url(), ""));
            if *request.method() == Method::Get && path == "/" {
                return Ok(html(&view_token));
            }
            if *request.method() == Method::Get
                && let Some((_, content_type, data)) = ASSETS.iter().find(|(r, _, _)| *r == path)
            {
                return Ok(asset(content_type, data));
            }
            if let Some(t) = &token {
                let expected = format!("Bearer {t}");
                let authorized = request.headers().iter().any(|h| h.field.equiv("Authorization") && same_secret(h.value.as_str(), &expected));
                if !authorized {
                    return Ok(error(401, "missing or wrong Authorization: Bearer token"));
                }
            }
            let url = request.url().to_string();
            let (path, query) = url.split_once('?').unwrap_or((&url, ""));
            let params = parse_params(query);
            let param = |k: &str| params.iter().find(|(c, _)| c == k).map(|(_, v)| v.clone());
            let method = request.method().clone();
            if method == Method::Get && path == "/health" {
                return Ok(json(200, serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }).to_string()));
            }
            // Where it looks from: computed on every request, because a project can change
            // space while the server is running.
            let db_path = db.clone();
            let mut base = db::open(&db)?;
            let server = Viewpoint::new(&base, dir.clone(), server_project.clone())?;
            let m = match look_from(&base, &server, param("dir"))? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            match (method, path) {
                (Method::Get, "/api/memories") => {
                    // The view asks for this first: what fixes itself, before showing anything.
                    if crate::review::cleanup_quietly(&mut base) {
                        crate::sync::schedule(&db_path);
                    }
                    let v = memory::recent(&base, &m.view, None, usize::MAX >> 1)?;
                    // How many previous versions each one has, for the view's timeline.
                    let mut q = base.prepare("SELECT memory_id, count(*) FROM memory_version GROUP BY memory_id")?;
                    let versions: std::collections::HashMap<String, i64> =
                        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
                    crate::review::refresh(&base)?;
                    let all_links = crate::links::all(&base)?;
                    // Links only to the ones also visible: the id of another client's one doesn't come out either.
                    let visible_ids: std::collections::HashSet<&str> = v.iter().map(|m| m.id.as_str()).collect();
                    let v: Vec<serde_json::Value> = v
                        .iter()
                        .map(|m| {
                            let n = versions.get(&m.id).copied().unwrap_or(0);
                            let links: Vec<&String> =
                                all_links.get(&m.id).map(|l| l.iter().filter(|x| visible_ids.contains(x.as_str())).collect()).unwrap_or_default();
                            let mut j = serde_json::to_value(m).unwrap_or_default();
                            j["versions"] = n.into();
                            j["links"] = serde_json::json!(links);
                            j
                        })
                        .collect();
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Get, "/api/projects") => {
                    let space = if m.sees_all { None } else { Some(m.view.space()) };
                    let body = serde_json::json!({
                        // Where it looks from, so the view can say so.
                        "scope": m.json(&base)?,
                        "projects": crate::project::list(&base, None, space, usize::MAX)?,
                        // Duplicates or in temp folders: they are proposed, never merged on their own.
                        "duplicates": crate::names::proposals(&base, &m.view)?,
                        "spaces": crate::project::spaces(&base)?.into_iter().filter(|e| m.sees_space(&e.key, false)).collect::<Vec<_>>(),
                        // The projects another machine has in another space, with what
                        // `sync status` says and the command that fixes it (only those that here belong to what is visible).
                        "space_conflicts": crate::sync::clashes(&db_path).iter().filter(|x| m.sees_space(&x.here, false)).map(|x| {
                            let mut v = serde_json::to_value(x).unwrap_or_default();
                            v["text"] = x.text().into();
                            v["command"] = x.command().into();
                            v
                        }).collect::<Vec<_>>(),
                        // The paused ones (a `project` line says another space, or a local folder
                        // joined a space only because its repository says so), with the two
                        // commands that decide it.
                        "paused": crate::project::paused_projects(&base)?.iter().filter(|x| m.sees_space(&x.here, false)).map(|x| {
                            let mut v = serde_json::to_value(x).unwrap_or_default();
                            v["text"] = crate::sync::pause_text(&db_path, x).into();
                            v["accept"] = crate::sync::accept_command(x).into();
                            v["keep"] = crate::sync::keep_command(x).into();
                            v
                        }).collect::<Vec<_>>(),
                    });
                    Ok(json(200, body.to_string()))
                }
                (Method::Get, "/api/overview") => {
                    // The owner view, only with what is visible from the request's project
                    // (its space) or, from the user's folder, everything.
                    let months = param("months").and_then(|n| n.parse().ok()).unwrap_or(crate::owner_view::MONTHS);
                    Ok(json(200, crate::owner_view::overview(&base, &db_path, &m.base, months)?.to_string()))
                }
                (Method::Get, "/api/review") => Ok(json(200, serde_json::to_string(&crate::review::notices(&base, &m.view)?)?)),
                (Method::Post, "/api/review/dismiss") => {
                    let v = json_body(&mut request)?;
                    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                    // The full ids in their canonical form (`resolve_id`) before checking visibility: a lowercase
                    // one or a prefix is not in the database as is, `Scope::sees` took it as visible and
                    // `review::dismiss` did find it (another client's included).
                    let resolved = |k: &str| -> Result<std::result::Result<String, String>> {
                        let t = field(k);
                        if t.trim().is_empty() {
                            return Ok(Ok(String::new()));
                        }
                        Ok(memory::resolve_id(&base, &t).map_err(|e| format!("{e:#}")))
                    };
                    let (one, other_id) = match (resolved("memory_id")?, resolved("other_id")?) {
                        (Ok(u), Ok(o)) => (u, o),
                        (Err(e), _) | (_, Err(e)) => return Ok(error(404, &e)),
                    };
                    if !m.sees(&base, &one)? || (!other_id.is_empty() && !m.sees(&base, &other_id)?) {
                        return Ok(out_of_scope());
                    }
                    match crate::review::dismiss(&base, &one, &field("kind"), &other_id) {
                        Ok(()) => Ok(json(200, serde_json::json!({ "dismissed": true }).to_string())),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Post, "/api/contradiction") => {
                    // Which of two contradicting memories holds (1.14): «This one holds» (`keep`, its id) or
                    // «Both hold» (`keep: "both"`), with whatever the person wants to say in `reason`.
                    let v = json_body(&mut request)?;
                    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                    let (a, b) = match (memory::resolve_id(&base, &field("a")), memory::resolve_id(&base, &field("b"))) {
                        (Ok(a), Ok(b)) => (a, b),
                        (Err(e), _) | (_, Err(e)) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.sees(&base, &a)? || !m.sees(&base, &b)? {
                        return Ok(out_of_scope());
                    }
                    let keep = match crate::verdicts::Keep::parse(&base, &field("keep")) {
                        Ok(q) => q,
                        Err(e) => return Ok(error(400, &format!("{e:#}"))),
                    };
                    let before = crate::review::before(&base)?;
                    match crate::verdicts::resolve(&base, &a, &b, &keep, &field("reason"), crate::verdicts::USER) {
                        Ok(superseded) => {
                            crate::review::refresh_for(&base, &before, &[])?;
                            crate::sync::schedule(&db_path);
                            Ok(json(200, serde_json::json!({ "resolved": true, "superseded": superseded }).to_string()))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Get, "/api/trash") => {
                    let mut v = Vec::new();
                    for o in memory::trash(&base)? {
                        if m.sees(&base, &o.id)? {
                            v.push(o);
                        }
                    }
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Post, "/api/restore") => {
                    let v = json_body(&mut request)?;
                    let id = v["id"].as_str().unwrap_or_default().trim().to_uppercase();
                    // By its prefix, like `memory::restore`: if it matches a single one, that is the one checked.
                    let ids: Vec<String> = base
                        .prepare("SELECT id FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?
                        .query_map([&id], |f| f.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    if let [one] = ids.as_slice()
                        && !id.is_empty()
                        && !m.sees(&base, one)?
                    {
                        return Ok(out_of_scope());
                    }
                    let before = crate::review::before(&base)?;
                    match memory::restore(&mut base, &id, None) {
                        Ok(r) => {
                            crate::review::refresh_for(&base, &before, std::slice::from_ref(&r.id))?;
                            crate::sync::schedule(&db_path);
                            Ok(json(200, serde_json::to_string(&r)?))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Post, "/api/split") => {
                    let v = json_body(&mut request)?;
                    let id = match memory::resolve_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.sees(&base, &id)? {
                        return Ok(out_of_scope());
                    }
                    let parts: Vec<(String, String)> = v["parts"]
                        .as_array()
                        .map(|a| a.iter().map(|p| (p["title"].as_str().unwrap_or_default().to_string(), p["content"].as_str().unwrap_or_default().to_string())).collect())
                        .unwrap_or_default();
                    let before = crate::review::before(&base)?;
                    let reason = v["reason"].as_str().unwrap_or("split in the web view");
                    match memory::split(&mut base, &id, &parts, reason) {
                        Ok(r) => {
                            let ids: Vec<String> = r.iter().map(|x| x.id.clone()).collect();
                            crate::review::refresh_for(&base, &before, &ids)?;
                            crate::sync::schedule(&db_path);
                            Ok(json(200, serde_json::to_string(&r)?))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Post, "/api/redact") => {
                    let v = json_body(&mut request)?;
                    let id = match memory::resolve_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.sees(&base, &id)? {
                        return Ok(out_of_scope());
                    }
                    let before = crate::review::before(&base)?;
                    match memory::redact(&mut base, &id, None) {
                        Ok(r) => {
                            if !r.redacted.is_empty() {
                                crate::review::refresh_for(&base, &before, std::slice::from_ref(&r.id))?;
                                crate::sync::schedule(&db_path);
                            }
                            let mut j = serde_json::to_value(&r)?;
                            j["redacted"] = r.redacted.len().into();
                            Ok(json(200, j.to_string()))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                // The view's «Verify» button: a person's mark (`verified`). It is not a
                // tool (`/tools/`, what agents use over HTTP), and it asks for what this view's page
                // sends: `Origin` (from here: other websites were already cut off above) and the token that
                // lives inside the page (`new_view_token`); a `curl` with a made-up `Origin` is not enough.
                (Method::Post, "/api/run") => {
                    if header("Origin").is_none() || !header(VIEW_HEADER).is_some_and(|t| same_secret(&t, &view_token)) {
                        return Ok(error(403, "this runs from the button in the web view only"));
                    }
                    let v = json_body(&mut request)?;
                    let args = match allowed_command(v["command"].as_str().unwrap_or_default()) {
                        Ok(a) => a,
                        Err(e) => return Ok(error(400, &e)),
                    };
                    let output = std::process::Command::new(std::env::current_exe()?)
                        .args(&args)
                        .arg("--db")
                        .arg(&db_path)
                        .stdin(std::process::Stdio::null())
                        .output()?;
                    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
                    let (out, err) = (text(&output.stdout), text(&output.stderr));
                    if !output.status.success() {
                        return Ok(error(400, if err.is_empty() { &out } else { &err }));
                    }
                    crate::sync::schedule(&db_path);
                    Ok(json(200, serde_json::json!({ "ok": true, "output": if out.is_empty() { err } else { out } }).to_string()))
                }
                // The view's «Make skill» button (`skill`): it writes files into the repository of the
                // memory's project, so it asks for the same as «Verify»: this view's page.
                (Method::Post, "/api/skill") => {
                    if header("Origin").is_none() || !header(VIEW_HEADER).is_some_and(|t| same_secret(&t, &view_token)) {
                        return Ok(error(403, "this runs from the button in the web view only (agents: the `skill` tool)"));
                    }
                    let v = json_body(&mut request)?;
                    let id = match memory::resolve_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.sees(&base, &id)? {
                        return Ok(out_of_scope());
                    }
                    let mem = memory::get(&base, &id)?;
                    if mem.project.is_empty() || !crate::trades::of_memory(&mem).is_empty() {
                        return Ok(error(400, "a skill belongs to a repository: only a project's memories become skills"));
                    }
                    match crate::skill::write(&base, &mem.project, &id, crate::skill::Target::Both, v["force"].as_bool().unwrap_or(false), false) {
                        Ok(r) => Ok(json(200, serde_json::to_string(&r)?)),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Post, "/api/verify") => {
                    if header("Origin").is_none() || !header(VIEW_HEADER).is_some_and(|t| same_secret(&t, &view_token)) {
                        return Ok(error(403, "verifying is for a person: use the button in the web view, or `omoi verify` in a terminal"));
                    }
                    let v = json_body(&mut request)?;
                    let id = match memory::resolve_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.sees(&base, &id)? {
                        return Ok(out_of_scope());
                    }
                    let r = if v["unverify"].as_bool().unwrap_or(false) {
                        crate::verified::unverify(&mut base, &id)?;
                        memory::get(&base, &id)?
                    } else {
                        let current = memory::get(&base, &id)?;
                        let by = match v["by"].as_str().map(str::trim).filter(|b| !b.is_empty()) {
                            Some(b) => b.to_string(),
                            None => crate::verified::default_verifier(&base, &current)?,
                        };
                        match crate::verified::verify(&mut base, &id, &by) {
                            Ok(r) => r,
                            Err(e) => return Ok(error(400, &format!("{e:#}"))),
                        }
                    };
                    crate::sync::schedule(&db_path);
                    Ok(json(200, serde_json::to_string(&r)?))
                }
                (Method::Get, "/api/candidates") => {
                    // What was set aside carries its session's folder: only those of visible projects.
                    let mut scopes: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
                    let mut v = Vec::new();
                    for c in crate::candidates::all(&base)? {
                        let shown = match scopes.get(&c.project) {
                            Some(s) => *s,
                            None => {
                                let s = m.covers(&Scope::from_project(&base, &c.project)?);
                                scopes.insert(c.project.clone(), s);
                                s
                            }
                        };
                        if shown {
                            v.push(c);
                        }
                    }
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Post, "/api/candidates/dismiss") => {
                    let v = json_body(&mut request)?;
                    let id = v["id"].as_i64().unwrap_or_default();
                    let project: Option<String> = rusqlite::OptionalExtension::optional(base.query_row("SELECT project FROM candidate WHERE id = ?1", [id], |f| f.get(0)))?;
                    let Some(project) = project else { return Ok(error(404, "no such candidate")) };
                    if !m.covers(&Scope::from_project(&base, &project)?) {
                        return Ok(error(403, "that candidate is from a project outside this scope"));
                    }
                    crate::candidates::dismiss(&base, id)?;
                    Ok(json(200, serde_json::json!({ "dismissed": true }).to_string()))
                }
                (Method::Get, "/api/sync") => {
                    // From a client (space), only that space's repository: neither the URL nor the
                    // author of the general one (the personal one) nor those of other spaces. From the user's
                    // folder or a project without a space, the general one.
                    let repo = if m.sees_all || m.space.is_empty() {
                        Some(crate::sync::paths(&db_path))
                    } else {
                        crate::sync::space_repo(&db_path, &m.space)
                    };
                    let config = match &repo {
                        Some(r) => crate::sync::read_config_from(r)?,
                        None => None,
                    };
                    let e = repo.as_ref().map(crate::sync::read_status_from).unwrap_or_default();
                    Ok(json(
                        200,
                        serde_json::json!({
                            "configured": config.is_some(),
                            "url": config.as_ref().map(|c| crate::sync::strip_credentials(&c.url)),
                            "author": config.and_then(|c| c.author),
                            "last_ok": e.last_ok,
                            "last_error": e.last_error.as_deref().map(crate::sync::strip_credentials),
                            "warning": e.notice,
                            "failing_since": e.failing_since,
                        })
                        .to_string(),
                    ))
                }
                // The tools look from the request's folder, with the same checks
                // as over MCP (`search`, `get`, `update`, `forget`… don't leave the space).
                (Method::Get, "/tools") => {
                    drop(base);
                    let (text, _) = mcp::run_tool(db_path, m.dir, None, "", "http")?;
                    Ok(json(200, text))
                }
                (Method::Get, "/search") => {
                    drop(base);
                    let Some(q) = param("q") else { return Ok(error(400, "missing q")) };
                    let mut args = serde_json::json!({ "query": q });
                    if let Some(n) = param("limit").and_then(|n| n.parse::<u64>().ok()) {
                        args["limit"] = n.into();
                    }
                    let (text, is_error) = mcp::run_tool(db_path, m.dir, Some("search"), &args.to_string(), "http")?;
                    Ok(json(if is_error { 400 } else { 200 }, text))
                }
                (Method::Post, r) if r.starts_with("/tools/") => {
                    drop(base);
                    let name = &r["/tools/".len()..];
                    let mut body = String::new();
                    request.as_reader().read_to_string(&mut body)?;
                    match mcp::run_tool(db_path, m.dir, Some(name), &body, "http") {
                        Ok((text, is_error)) => Ok(json(if is_error { 400 } else { 200 }, text)),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                _ => Ok(error(404, "not found: GET /, GET /api/memories, GET /api/projects, GET /api/overview, GET /api/review, POST /api/review/dismiss, POST /api/contradiction, GET /api/trash, POST /api/restore, POST /api/redact, POST /api/skill, POST /api/verify, GET /api/candidates, POST /api/candidates/dismiss, GET /api/sync, GET /health, GET /tools, GET /search?q=, POST /tools/<name>")),
            }
        })()
        .unwrap_or_else(|e| error(500, &format!("{e:#}")));
        let _ = request.respond(response);
    }
    Ok(())
}

/// The commands the view's «Run» button can launch: the ones the view itself proposes in
/// Review, and no others. Returns their arguments without the leading `omoi` (with `"…"` or
/// `'…'` quotes for names with spaces, as the view writes them), or why it can't. No
/// placeholders to fill in (`<carpeta>`) nor options that change the database (`--db`, `--dir`).
pub(crate) fn allowed_command(command: &str) -> std::result::Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    let (mut current, mut quote, mut pending) = (String::new(), None::<char>, false);
    for c in command.trim().chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) if c == '\\' => {}
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                pending = true;
            }
            None if c.is_whitespace() => {
                if pending || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
                pending = false;
            }
            None => current.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if pending || !current.is_empty() {
        args.push(current);
    }
    let no = || Err(format!("not one of the commands the web view can run: {command}"));
    if args.first().map(String::as_str) != Some("omoi") {
        return no();
    }
    args.remove(0);
    let action: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    let allowed = matches!(action.as_slice(), ["projects", "merge" | "distinct" | "keep"] | ["space", "unassign"])
        || (action.as_slice() == ["projects", "move"] && args.iter().any(|a| a == "--space"));
    let odd = args.iter().skip(2).any(|a| {
        a.is_empty() || a.contains(['<', '>']) || matches!(a.as_str(), "--db" | "--dir") || a.starts_with("--db=") || a.starts_with("--dir=")
    });
    if !allowed || odd || args.len() < 3 {
        return no();
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_button_only_runs_what_the_view_proposes() {
        use super::allowed_command as o;
        assert_eq!(o("omoi projects merge tokimichi Tokimichi").unwrap(), ["projects", "merge", "tokimichi", "Tokimichi"]);
        assert_eq!(o(r#"omoi projects distinct "Mi app" 'otra cosa'"#).unwrap(), ["projects", "distinct", "Mi app", "otra cosa"]);
        assert_eq!(o("omoi projects move kumi --space acme --force").unwrap(), ["projects", "move", "kumi", "--space", "acme", "--force"]);
        assert!(o("omoi space unassign kumi --force").is_ok());
        assert!(o("omoi projects keep kumi").is_ok());
        // What the view doesn't propose, what has a placeholder or what changes the database: no.
        assert!(o("omoi forget 01M616P33818").is_err());
        assert!(o("omoi projects move kumi <nueva-carpeta>").is_err());
        assert!(o("omoi projects move kumi /otra/carpeta").is_err());
        assert!(o("omoi projects merge a b --db /tmp/otra.db").is_err());
        assert!(o("omoi projects merge").is_err());
        assert!(o("ls; omoi projects merge a b").is_err());
        assert!(o("omoi recent --project a").is_err());
        assert!(o(r#"omoi projects merge "a b"#).is_err());
    }

    use super::view_url;

    /// The link to a memory, with and without the `OMOIKANE_HTTP_TOKEN` token (which the page removes from the
    /// address and replaces with `#/memoria/<id>`).
    #[test]
    fn view_url_with_and_without_token() {
        assert_eq!(view_url(7438, None, None), "http://127.0.0.1:7438/");
        assert_eq!(view_url(7438, None, Some("01ABC")), "http://127.0.0.1:7438/#/memoria/01ABC");
        assert_eq!(view_url(1, Some("t"), None), "http://127.0.0.1:1/#token=t");
        assert_eq!(view_url(1, Some("t"), Some("01ABC")), "http://127.0.0.1:1/#token=t&to=/memoria/01ABC");
    }

    /// 1.8.6: an unescaped apostrophe in a single-quoted string («It's fine as it is») gave a
    /// SyntaxError that left the whole view without JavaScript. Each `<script>` in the page goes through
    /// `node --check`; without node on the machine the test is skipped (CI has it, and checks it itself too).
    #[test]
    fn view_javascript_compiles() {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("sin node: no se comprueba el JavaScript de la vista");
            return;
        }
        let dir = crate::common::TempDir::new("webjs");
        let mut rest = super::PAGE;
        let mut n = 0;
        while let Some(i) = rest.find("<script") {
            let after = &rest[i..];
            let open_end = after.find('>').expect("<script sin cerrar") + 1;
            let close_start = after.find("</script>").expect("falta </script>");
            n += 1;
            let asset = dir.join(format!("web-{n}.js"));
            std::fs::write(&asset, &after[open_end..close_start]).unwrap();
            let output = std::process::Command::new("node").arg("--check").arg(&asset).output().unwrap();
            assert!(output.status.success(), "el <script> {n} de web.html no compila:\n{}", String::from_utf8_lossy(&output.stderr));
            rest = &after[close_start..];
        }
        assert!(n > 0, "web.html sin <script>");
    }
}
