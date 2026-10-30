//! `omoi serve`: la memoria por HTTP en local, para lo que no habla MCP (scripts, otras apps). Las
//! mismas herramientas que el servidor MCP y con las mismas respuestas:
//!
//! - `GET /` → la vista web: la cronología de memorias y los proyectos con su estado (y `/icono.png`,
//!   `/fuentes/*.woff2`, que la página carga del binario)
//! - `GET /api/memories` → todas las memorias, enteras, de la más reciente a la más antigua, con
//!   `versions` (cuántas versiones anteriores guarda su historial) y `links` (las que enlaza con `[[…]]`)
//! - `GET /api/review` → lo que conviene revisar (`revisar::avisos`), y `POST /api/review/dismiss` con
//!   `{memory_id, kind, other_id}` para «está bien así»
//! - `GET /api/candidates` → lo apartado de las conversaciones (`candidatas::todas`), y
//!   `POST /api/candidates/dismiss` con `{id}` para dejarlo
//! - `GET /api/trash` → la papelera (`memoria::papelera`), y `POST /api/restore` con `{id}` para
//!   devolver una memoria
//! - `GET /health` → `{"ok": true, "version": …}`
//! - `GET /tools` → nombres, descripciones y esquemas de las herramientas
//! - `POST /tools/<nombre>` con los argumentos en JSON → la respuesta de la herramienta (400 si es un error)
//! - `GET /search?q=…&limit=…` → atajo de la herramienta `search`
//!
//! Solo escucha en `127.0.0.1`, y solo atiende peticiones dirigidas a `127.0.0.1` o `localhost` (para
//! que una web cualquiera no llegue a ella cambiando su DNS) y, si escriben, hechas desde la propia
//! vista (una web ajena no puede mandar un POST que olvide memorias). Con `OMOIKANE_HTTP_TOKEN`, cada
//! petición (salvo la página, que no lleva datos) tiene que llevar `Authorization: Bearer <token>`. `?dir=<carpeta>` elige el proyecto de esa petición (por defecto,
//! el de la carpeta en la que arrancó el servidor).

use crate::memoria::{self, Ambito};
use crate::{db, mcp, tr};
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use tiny_http::{Header, Method, Response, Server};

pub const PUERTO: u16 = 7438;

fn json(estado: u16, cuerpo: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let cabecera = Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("cabecera");
    Response::from_string(cuerpo).with_status_code(estado).with_header(cabecera)
}

const PAGINA: &str = include_str!("web.html");

/// Lo que la página carga aparte: el icono y sus fuentes (OFL, en `assets/fuentes`), dentro del binario
/// porque la vista tiene que funcionar sin internet.
const FICHEROS: &[(&str, &str, &[u8])] = &[
    ("/icono.png", "image/png", include_bytes!("../assets/icon-64.png")),
    ("/fuentes/plex-sans.woff2", "font/woff2", include_bytes!("../assets/fuentes/plex-sans.woff2")),
    ("/fuentes/plex-mono-400.woff2", "font/woff2", include_bytes!("../assets/fuentes/plex-mono-400.woff2")),
    ("/fuentes/plex-mono-600.woff2", "font/woff2", include_bytes!("../assets/fuentes/plex-mono-600.woff2")),
    ("/fuentes/shippori-600.woff2", "font/woff2", include_bytes!("../assets/fuentes/shippori-600.woff2")),
    ("/fuentes/shippori-600-omoikane.woff2", "font/woff2", include_bytes!("../assets/fuentes/shippori-600-omoikane.woff2")),
];

fn fichero(tipo: &str, datos: &'static [u8]) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(datos.to_vec())
        .with_header(Header::from_bytes("Content-Type", tipo).expect("cabecera"))
        .with_header(Header::from_bytes("Cache-Control", "max-age=86400").expect("cabecera"))
}

fn html() -> Response<std::io::Cursor<Vec<u8>>> {
    let cabecera = Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("cabecera");
    Response::from_string(PAGINA).with_header(cabecera)
}

/// Abre la vista en el navegador del sistema.
pub fn abrir_navegador(url: &str) {
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

/// Si ya hay un `omoi serve` en ese puerto.
fn ya_sirve(puerto: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], puerto).into(), std::time::Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    if s.write_all(format!("GET /health HTTP/1.0\r\nHost: 127.0.0.1:{puerto}\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    r.contains("\"ok\"")
}

fn error(estado: u16, mensaje: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json(estado, serde_json::json!({ "error": mensaje }).to_string())
}

/// Decodifica `%xx` y `+` de un parámetro de la URL.
fn decodificar(s: &str) -> String {
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

fn parametros(consulta: &str) -> Vec<(String, String)> {
    consulta
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decodificar(k), decodificar(v)),
            None => (decodificar(p), String::new()),
        })
        .collect()
}

/// Atiende peticiones hasta que se cierre el proceso. Con `abrir`, abre la vista en el navegador; si
/// ya hay un servidor en ese puerto, solo la abre.
pub fn servir(db: PathBuf, dir: Option<PathBuf>, puerto: u16, abrir: bool) -> Result<()> {
    let token = std::env::var("OMOIKANE_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    let url = match &token {
        Some(t) => format!("http://127.0.0.1:{puerto}/#token={t}"),
        None => format!("http://127.0.0.1:{puerto}/"),
    };
    let servidor = match Server::http(("127.0.0.1", puerto)) {
        Ok(s) => s,
        Err(_) if abrir && ya_sirve(puerto) => {
            abrir_navegador(&url);
            eprintln!("{}", tr!("omoikane: already running on http://127.0.0.1:{puerto}", "omoikane: ya está en marcha en http://127.0.0.1:{puerto}"));
            return Ok(());
        }
        Err(e) => return Err(anyhow!("127.0.0.1:{puerto}: {e}")),
    };
    eprintln!("{}", tr!("omoikane: listening on http://127.0.0.1:{puerto}", "omoikane: escuchando en http://127.0.0.1:{puerto}"));
    if abrir {
        abrir_navegador(&url);
    }
    let anfitriones = [format!("127.0.0.1:{puerto}"), format!("localhost:{puerto}")];
    let origenes = anfitriones.iter().map(|a| format!("http://{a}")).collect::<Vec<_>>();
    for mut peticion in servidor.incoming_requests() {
        let respuesta = (|| -> Result<Response<std::io::Cursor<Vec<u8>>>> {
            let cabecera = |k: &'static str| peticion.headers().iter().find(|h| h.field.equiv(k)).map(|h| h.value.as_str().to_string());
            // Contra el «DNS rebinding»: una web que haga que su dominio apunte a 127.0.0.1 llega con su Host.
            if cabecera("Host").is_some_and(|h| !anfitriones.contains(&h)) {
                return Ok(error(403, "wrong Host"));
            }
            // Contra webs ajenas que manden un POST (un formulario no necesita permiso de CORS para enviarse).
            if *peticion.method() != Method::Get && cabecera("Origin").is_some_and(|o| !origenes.contains(&o)) {
                return Ok(error(403, "requests from other sites are not allowed"));
            }
            let (ruta, _) = peticion.url().split_once('?').unwrap_or((peticion.url(), ""));
            if *peticion.method() == Method::Get && ruta == "/" {
                return Ok(html());
            }
            if *peticion.method() == Method::Get
                && let Some((_, tipo, datos)) = FICHEROS.iter().find(|(r, _, _)| *r == ruta)
            {
                return Ok(fichero(tipo, datos));
            }
            if let Some(t) = &token {
                let esperado = format!("Bearer {t}");
                let vale = peticion.headers().iter().any(|h| h.field.equiv("Authorization") && h.value.as_str() == esperado);
                if !vale {
                    return Ok(error(401, "missing or wrong Authorization: Bearer token"));
                }
            }
            let url = peticion.url().to_string();
            let (ruta, consulta) = url.split_once('?').unwrap_or((&url, ""));
            let params = parametros(consulta);
            let param = |k: &str| params.iter().find(|(c, _)| c == k).map(|(_, v)| v.clone());
            let dir_peticion = param("dir").map(PathBuf::from).or_else(|| dir.clone());
            let metodo = peticion.method().clone();
            match (metodo, ruta) {
                (Method::Get, "/api/memories") => {
                    let db = db::abrir(&db)?;
                    let v = memoria::recientes(&db, &Ambito::Todos, None, usize::MAX >> 1)?;
                    // Con cuántas versiones anteriores tiene cada una, para la cronología de la vista.
                    let mut q = db.prepare("SELECT memory_id, count(*) FROM memory_version GROUP BY memory_id")?;
                    let versiones: std::collections::HashMap<String, i64> =
                        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
                    crate::revisar::al_dia(&db)?;
                    let enlaces = crate::enlaces::todos(&db)?;
                    let v: Vec<serde_json::Value> = v
                        .into_iter()
                        .map(|m| {
                            let n = versiones.get(&m.id).copied().unwrap_or(0);
                            let links = enlaces.get(&m.id).cloned().unwrap_or_default();
                            let mut j = serde_json::to_value(m).unwrap_or_default();
                            j["versions"] = n.into();
                            j["links"] = links.into();
                            j
                        })
                        .collect();
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Get, "/api/review") => {
                    let db = db::abrir(&db)?;
                    Ok(json(200, serde_json::to_string(&crate::revisar::avisos(&db, &Ambito::Todos)?)?))
                }
                (Method::Post, "/api/review/dismiss") => {
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    let v: serde_json::Value = serde_json::from_str(&cuerpo)?;
                    let campo = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                    let db = db::abrir(&db)?;
                    match crate::revisar::descartar(&db, &campo("memory_id"), &campo("kind"), &campo("other_id")) {
                        Ok(()) => Ok(json(200, serde_json::json!({ "dismissed": true }).to_string())),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Get, "/api/trash") => {
                    let db = db::abrir(&db)?;
                    Ok(json(200, serde_json::to_string(&memoria::papelera(&db)?)?))
                }
                (Method::Post, "/api/restore") => {
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    let v: serde_json::Value = serde_json::from_str(&cuerpo)?;
                    let mut base = db::abrir(&db)?;
                    match memoria::restaurar(&mut base, v["id"].as_str().unwrap_or_default(), None) {
                        Ok(m) => {
                            crate::sync::programar(&db);
                            Ok(json(200, serde_json::to_string(&m)?))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Get, "/api/candidates") => {
                    let db = db::abrir(&db)?;
                    Ok(json(200, serde_json::to_string(&crate::candidatas::todas(&db)?)?))
                }
                (Method::Post, "/api/candidates/dismiss") => {
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    let v: serde_json::Value = serde_json::from_str(&cuerpo)?;
                    let db = db::abrir(&db)?;
                    crate::candidatas::descartar(&db, v["id"].as_i64().unwrap_or_default())?;
                    Ok(json(200, serde_json::json!({ "dismissed": true }).to_string()))
                }
                (Method::Get, "/api/sync") => {
                    let config = crate::sync::leer_config(&db)?;
                    let e = crate::sync::leer_estado(&db);
                    Ok(json(
                        200,
                        serde_json::json!({
                            "configured": config.is_some(),
                            "url": config.as_ref().map(|c| crate::sync::sin_credenciales(&c.url)),
                            "author": config.and_then(|c| c.author),
                            "last_ok": e.ultimo_ok,
                            "last_error": e.ultimo_error.as_deref().map(crate::sync::sin_credenciales),
                            "warning": e.aviso,
                            "failing_since": e.error_desde,
                        })
                        .to_string(),
                    ))
                }
                (Method::Get, "/health") => Ok(json(200, serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }).to_string())),
                (Method::Get, "/tools") => {
                    let (texto, _) = mcp::herramienta(db.clone(), dir_peticion, None, "", "http")?;
                    Ok(json(200, texto))
                }
                (Method::Get, "/search") => {
                    let Some(q) = param("q") else { return Ok(error(400, "missing q")) };
                    let mut args = serde_json::json!({ "query": q });
                    if let Some(n) = param("limit").and_then(|n| n.parse::<u64>().ok()) {
                        args["limit"] = n.into();
                    }
                    let (texto, es_error) = mcp::herramienta(db.clone(), dir_peticion, Some("search"), &args.to_string(), "http")?;
                    Ok(json(if es_error { 400 } else { 200 }, texto))
                }
                (Method::Post, r) if r.starts_with("/tools/") => {
                    let nombre = &r["/tools/".len()..];
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    match mcp::herramienta(db.clone(), dir_peticion, Some(nombre), &cuerpo, "http") {
                        Ok((texto, es_error)) => Ok(json(if es_error { 400 } else { 200 }, texto)),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                _ => Ok(error(404, "not found: GET /, GET /api/memories, GET /api/review, POST /api/review/dismiss, GET /api/candidates, POST /api/candidates/dismiss, GET /api/sync, GET /health, GET /tools, GET /search?q=, POST /tools/<name>")),
            }
        })()
        .unwrap_or_else(|e| error(500, &format!("{e:#}")));
        let _ = peticion.respond(respuesta);
    }
    Ok(())
}
