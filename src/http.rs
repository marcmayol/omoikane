//! `omoi serve`: la memoria por HTTP en local, para lo que no habla MCP (scripts, otras apps). Las
//! mismas herramientas que el servidor MCP y con las mismas respuestas:
//!
//! - `GET /health` → `{"ok": true, "version": …}`
//! - `GET /tools` → nombres, descripciones y esquemas de las herramientas
//! - `POST /tools/<nombre>` con los argumentos en JSON → la respuesta de la herramienta (400 si es un error)
//! - `GET /search?q=…&limit=…` → atajo de la herramienta `search`
//!
//! Solo escucha en `127.0.0.1`. Con `OMOIKANE_HTTP_TOKEN`, cada petición tiene que llevar
//! `Authorization: Bearer <token>`. `?dir=<carpeta>` elige el proyecto de esa petición (por defecto,
//! el de la carpeta en la que arrancó el servidor).

use crate::mcp;
use crate::tr;
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use tiny_http::{Header, Method, Response, Server};

pub const PUERTO: u16 = 7438;

fn json(estado: u16, cuerpo: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let cabecera = Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("cabecera");
    Response::from_string(cuerpo).with_status_code(estado).with_header(cabecera)
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

/// Atiende peticiones hasta que se cierre el proceso.
pub fn servir(db: PathBuf, dir: Option<PathBuf>, puerto: u16) -> Result<()> {
    let servidor = Server::http(("127.0.0.1", puerto)).map_err(|e| anyhow!("127.0.0.1:{puerto}: {e}"))?;
    let token = std::env::var("OMOIKANE_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    eprintln!("{}", tr!("omoikane: listening on http://127.0.0.1:{puerto}", "omoikane: escuchando en http://127.0.0.1:{puerto}"));
    for mut peticion in servidor.incoming_requests() {
        let respuesta = (|| -> Result<Response<std::io::Cursor<Vec<u8>>>> {
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
                (Method::Get, "/health") => Ok(json(200, serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }).to_string())),
                (Method::Get, "/tools") => {
                    let (texto, _) = mcp::herramienta(db.clone(), dir_peticion, None, "")?;
                    Ok(json(200, texto))
                }
                (Method::Get, "/search") => {
                    let Some(q) = param("q") else { return Ok(error(400, "missing q")) };
                    let mut args = serde_json::json!({ "query": q });
                    if let Some(n) = param("limit").and_then(|n| n.parse::<u64>().ok()) {
                        args["limit"] = n.into();
                    }
                    let (texto, es_error) = mcp::herramienta(db.clone(), dir_peticion, Some("search"), &args.to_string())?;
                    Ok(json(if es_error { 400 } else { 200 }, texto))
                }
                (Method::Post, r) if r.starts_with("/tools/") => {
                    let nombre = &r["/tools/".len()..];
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    match mcp::herramienta(db.clone(), dir_peticion, Some(nombre), &cuerpo) {
                        Ok((texto, es_error)) => Ok(json(if es_error { 400 } else { 200 }, texto)),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                _ => Ok(error(404, "not found: GET /health, GET /tools, GET /search?q=, POST /tools/<name>")),
            }
        })()
        .unwrap_or_else(|e| error(500, &format!("{e:#}")));
        let _ = peticion.respond(respuesta);
    }
    Ok(())
}
