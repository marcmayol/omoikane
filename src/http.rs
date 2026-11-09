//! `omoi serve`: la memoria por HTTP en local, para lo que no habla MCP (scripts, otras apps). Las
//! mismas herramientas que el servidor MCP y con las mismas respuestas:
//!
//! - `GET /` → la vista web: la cronología de memorias y los proyectos con su estado (y `/icono.png`,
//!   `/fuentes/*.woff2`, que la página carga del binario)
//! - `GET /api/memories` → las memorias que se ven (ver «Quién ve qué»), enteras, de la más reciente a la más antigua, con
//!   `versions` (cuántas versiones anteriores guarda su historial) y `links` (las que enlaza con `[[…]]`)
//! - `GET /api/projects` → los proyectos (`proyecto::lista`: clave, espacio, carpeta y si ya no está)
//!   y los espacios (`proyecto::espacios`, con su nombre), para agrupar y filtrar por espacio
//! - `GET /api/overview?months=…` → la vista de responsable (`responsable::panorama`): por espacio, sus
//!   proyectos con su último `status`, lo fijado, las decisiones abiertas, quién escribió qué, lo que
//!   dice `revisar`, lo que está en pausa y lo que nadie toca desde hace `months` meses (6). Solo lo que
//!   se ve desde el proyecto de la petición (su espacio); desde la carpeta del usuario, todo
//! - `GET /api/review` → lo que conviene revisar (`revisar::avisos`), y `POST /api/review/dismiss` con
//!   `{memory_id, kind, other_id}` para «está bien así»
//! - `GET /api/candidates` → lo apartado de las conversaciones (`candidatas::todas`), y
//!   `POST /api/candidates/dismiss` con `{id}` para dejarlo
//! - `GET /api/trash` → la papelera (`memoria::papelera`), y `POST /api/restore` con `{id}` para
//!   devolver una memoria
//! - `POST /api/redact` con `{id}` → la memoria sin lo que parece un secreto (`memoria::redactar`), con
//!   el motivo «redacted» en su historial
//! - `GET /health` → `{"ok": true, "version": …}`
//! - `GET /tools` → nombres, descripciones y esquemas de las herramientas
//! - `POST /tools/<nombre>` con los argumentos en JSON → la respuesta de la herramienta (400 si es un error)
//! - `GET /search?q=…&limit=…` → atajo de la herramienta `search`
//!
//! Solo escucha en `127.0.0.1`, y solo atiende peticiones dirigidas a `127.0.0.1` o `localhost` (para
//! que una web cualquiera no llegue a ella cambiando su DNS) y, si escriben, hechas desde la propia
//! vista (una web ajena no puede mandar un POST que olvide memorias). Con `OMOIKANE_HTTP_TOKEN`, cada
//! petición (salvo la página, que no lleva datos) tiene que llevar `Authorization: Bearer <token>`.
//! `POST /api/verify` (el botón «Verificar» de la vista) pide además el token de la vista, uno nuevo en
//! cada `omoi serve` que solo va dentro de la página (`X-Omoikane-View`): frena un `curl` con un
//! `Origin` inventado, no a un agente con shell que se proponga cargar la página para sacarlo.
//!
//! **Quién ve qué** (1.7.0), la misma regla que la CLI y el MCP: lo decide la carpeta en la que arrancó
//! el servidor. Desde un proyecto de un espacio, ese espacio entero (sus proyectos, lo del espacio) y lo
//! global; desde un proyecto sin espacio, los proyectos sin espacio y lo global; desde la carpeta del
//! usuario, todo. Vale para todo lo que lee (memorias, proyectos, revisar, papelera, candidatas,
//! panorama, herramientas) y para lo que escribe: tocar una memoria de fuera da 403. `?dir=<carpeta>`
//! elige el proyecto de esa petición, pero solo dentro de lo que ya se ve (desde un proyecto, otro de su
//! mismo espacio; desde la carpeta del usuario, cualquiera): no amplía nada (403).

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

/// Dónde va, en la página, el token de la vista (ver [`token_de_vista`]).
const MARCA_VISTA: &str = "__OMOIKANE_VIEW__";
/// La cabecera con la que la página lo devuelve en lo que solo puede hacer una persona (`/api/verify`).
const CABECERA_VISTA: &str = "X-Omoikane-View";

/// Un token nuevo para cada `omoi serve`, que solo está dentro de la página que sirve: lo que de verdad
/// pide `/api/verify` además del `Origin` del navegador (que cualquier `curl -H "Origin: …"` pone).
/// Para leerlo hay que cargar la página: una web ajena no puede (el navegador no le deja leer la
/// respuesta, y el `Host` corta el DNS rebinding). Un agente con shell podría pedir la página y
/// sacarlo: no es una frontera contra quien se lo proponga, sino que ningún camino normal del agente
/// (sus herramientas, `/tools/`, un `curl` sin más) verifique.
fn token_de_vista() -> Result<String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn html(vista: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let cabecera = Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("cabecera");
    // Que la página no se guarde en ninguna caché: lleva el token de esta ejecución.
    let sin_cache = Header::from_bytes("Cache-Control", "no-store").expect("cabecera");
    Response::from_string(PAGINA.replace(MARCA_VISTA, vista)).with_header(cabecera).with_header(sin_cache)
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

/// Si dos secretos (el token de la vista, el de `OMOIKANE_HTTP_TOKEN`) son iguales, en tiempo
/// constante: comparar byte a byte y parar en el primero distinto deja medir cuánto se ha acertado.
fn mismo_secreto(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
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

/// Desde dónde mira una petición: el proyecto (para las herramientas) y lo que se ve desde él. La TUI
/// (`omoi tui`) mira con el mismo, para que enseñe exactamente lo que enseña la web.
#[derive(Clone)]
pub(crate) struct Mirador {
    /// La carpeta que se pasa a las herramientas (`?dir=` o la del servidor).
    dir: Option<PathBuf>,
    /// El proyecto, como lo guarda la base.
    proyecto: String,
    /// Lo que se ve desde el proyecto (para la vista de responsable, que lo amplía ella).
    pub(crate) base: Ambito,
    /// «Todos los proyectos» desde aquí: su espacio entero, los que no tienen espacio, o todo.
    pub(crate) vista: Ambito,
    /// Si se ve todo (carpeta del usuario).
    pub(crate) todo: bool,
    /// La clave del espacio desde el que se mira (vacía: ninguno, o todo).
    pub(crate) espacio: String,
}

impl Mirador {
    /// Si se ve lo de este espacio (su clave; vacía: sin espacio), o una memoria global.
    pub(crate) fn ve_espacio(&self, clave: &str, global: bool) -> bool {
        self.todo || global || clave == self.espacio
    }

    /// Si se ve la memoria `id`, viva o en la papelera. Una que no existe, sí: ya lo dirá quien la busque.
    pub(crate) fn ve(&self, db: &rusqlite::Connection, id: &str) -> Result<bool> {
        // Una memoria de oficio, solo si algún proyecto de lo que se ve tiene ese oficio.
        Ok(self.todo
            || (memoria::alcance_de_memoria(db, id)?.is_none_or(|(global, clave)| self.ve_espacio(&clave, global)) && !crate::oficios::oculta(db, &self.vista, id)?))
    }

    /// Si se ve un ámbito pedido: desde todo, cualquiera; si no, uno que no sea todo y de su mismo espacio.
    pub(crate) fn abarca(&self, otro: &Ambito) -> bool {
        self.todo || (!matches!(otro, Ambito::Todos) && self.base.espacio() == otro.espacio())
    }
}

/// Desde dónde mira la petición: la carpeta en la que arrancó el servidor o, con `?dir=`, otra dentro de
/// lo que ya se ve desde esa. `Err` con la respuesta si `?dir=` no vale.
fn mirar(
    db: &rusqlite::Connection,
    servidor: &Mirador,
    pedida: Option<String>,
) -> Result<std::result::Result<Mirador, Response<std::io::Cursor<Vec<u8>>>>> {
    let Some(d) = pedida.filter(|d| !d.trim().is_empty()) else {
        return Ok(Ok(servidor.clone()));
    };
    // Una ruta de red no se mira (`proyecto::de` la rechaza antes de tocar el disco): cualquier web
    // puede pedir esta URL con un `<img>`, y mirarla abriría una conexión SMB con el hash NTLM de la
    // persona.
    if !crate::proyecto::ruta_local(std::path::Path::new(&d)) {
        return Ok(Err(error(400, "dir: not a folder on this computer (network paths are never opened)")));
    }
    let Ok(proyecto) = crate::proyecto::de(std::path::Path::new(&d)) else { return Ok(Err(error(400, "dir: no such folder"))) };
    let base = Ambito::de(db, &proyecto)?;
    if !servidor.abarca(&base) {
        return Ok(Err(error(
            403,
            "dir is outside what this server shows: it can only narrow the view to a project of the same space; start `omoi serve` in that project, or in the home folder",
        )));
    }
    Ok(Ok(Mirador::nuevo(db, Some(PathBuf::from(d)), proyecto)?))
}

impl Mirador {
    pub(crate) fn nuevo(db: &rusqlite::Connection, dir: Option<PathBuf>, proyecto: String) -> Result<Mirador> {
        let base = Ambito::de(db, &proyecto)?;
        let todo = matches!(base, Ambito::Todos);
        let espacio = crate::proyecto::clave_espacio(db, base.espacio())?;
        Ok(Mirador { dir, proyecto, base, vista: base.amplio(), todo, espacio })
    }

    /// Desde dónde se mira, para la vista: todo, o un espacio (vacío: los proyectos sin espacio).
    fn json(&self, db: &rusqlite::Connection) -> Result<serde_json::Value> {
        let nombre = if self.espacio.is_empty() {
            String::new()
        } else {
            db.query_row("SELECT name FROM space WHERE key = ?1", [&self.espacio], |f| f.get(0)).unwrap_or_default()
        };
        Ok(serde_json::json!({
            "scope": if self.todo { "all" } else { "space" },
            "space": if self.todo { serde_json::Value::Null } else { self.espacio.clone().into() },
            "name": nombre,
            "project": self.proyecto,
        }))
    }
}

/// El cuerpo JSON de un POST.
fn cuerpo_json(peticion: &mut tiny_http::Request) -> Result<serde_json::Value> {
    let mut cuerpo = String::new();
    peticion.as_reader().read_to_string(&mut cuerpo)?;
    Ok(serde_json::from_str(&cuerpo)?)
}

fn fuera() -> Response<std::io::Cursor<Vec<u8>>> {
    error(403, "that memory is not in this scope: it belongs to another space; start `omoi serve` in that project, or in the home folder, to change it")
}

/// Mientras sirve, lo que otros equipos suben llega como en una sesión: cada `cada_cuanto` (2
/// minutos) como mucho, con la misma marca que los mensajes (así no se juntan dos). Se mira cada
/// cuarto de intervalo; sin sincronización no se abre ni la base.
fn traer_cada_poco(ruta: PathBuf) {
    std::thread::spawn(move || {
        loop {
            if crate::sync::automatica(&ruta)
                && let Ok(db) = db::abrir(&ruta)
            {
                crate::sync::traer_si_toca(&db, &ruta);
            }
            std::thread::sleep((crate::sync::cada_cuanto() / 4).max(std::time::Duration::from_secs(1)));
        }
    });
}

/// La dirección de la vista: con el token de `OMOIKANE_HTTP_TOKEN` (`#token=…`, que la página guarda y
/// quita) y, con `memoria`, el enlace a esa memoria (`#/memoria/<id>`, o `&to=/memoria/<id>` detrás del
/// token).
pub fn url_vista(puerto: u16, token: Option<&str>, memoria: Option<&str>) -> String {
    let base = format!("http://127.0.0.1:{puerto}/");
    match (token, memoria) {
        (Some(t), Some(id)) => format!("{base}#token={t}&to=/memoria/{id}"),
        (Some(t), None) => format!("{base}#token={t}"),
        (None, Some(id)) => format!("{base}#/memoria/{id}"),
        (None, None) => base,
    }
}

/// El id entero de la memoria que pide `omoi serve --memory` (vale su principio, como en el resto de
/// órdenes), si se ve desde `proyecto` (la carpeta en la que arranca el servidor): si no, la vista no
/// la tendría y el enlace no abriría nada.
pub fn memoria_del_enlace(db: &rusqlite::Connection, proyecto: &str, id: &str) -> Result<String> {
    let id = memoria::resolver_id(db, id)?;
    memoria::en_ambito(db, &Ambito::de(db, proyecto)?, &id).map_err(|e| {
        anyhow!(tr!(
            "--memory {id}: the web view started from this folder doesn't show it ({e}); run `omoi serve` in that memory's project or in your home folder",
            "--memory {id}: la vista que arranca desde esta carpeta no la enseña ({e}); lanza `omoi serve` en el proyecto de esa memoria o en tu carpeta personal"
        ))
    })?;
    Ok(id)
}

/// Atiende peticiones hasta que se cierre el proceso. Con `abrir`, abre la vista en el navegador (en la
/// memoria `memoria`, si se da: su id entero, ver [`memoria_del_enlace`]); si ya hay un servidor en ese
/// puerto, solo la abre.
pub fn servir(db: PathBuf, dir: Option<PathBuf>, puerto: u16, abrir: bool, memoria: Option<String>) -> Result<()> {
    let token = std::env::var("OMOIKANE_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    let url = url_vista(puerto, token.as_deref(), memoria.as_deref());
    if let Some(id) = &memoria {
        // El enlace sin el token: es el que se puede mandar en un aviso (la página pide el token aparte).
        eprintln!("{}", tr!("omoikane: memory {id}: {}", "omoikane: memoria {id}: {}", url_vista(puerto, None, Some(id))));
    }
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
    traer_cada_poco(db.clone());
    // El proyecto en el que arrancó: de él depende lo que ve cada petición (ver «Quién ve qué»).
    let proyecto_servidor = match &dir {
        Some(d) => crate::proyecto::de(d)?,
        None => crate::proyecto::actual()?,
    };
    let vista = token_de_vista()?;
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
                return Ok(html(&vista));
            }
            if *peticion.method() == Method::Get
                && let Some((_, tipo, datos)) = FICHEROS.iter().find(|(r, _, _)| *r == ruta)
            {
                return Ok(fichero(tipo, datos));
            }
            if let Some(t) = &token {
                let esperado = format!("Bearer {t}");
                let vale = peticion.headers().iter().any(|h| h.field.equiv("Authorization") && mismo_secreto(h.value.as_str(), &esperado));
                if !vale {
                    return Ok(error(401, "missing or wrong Authorization: Bearer token"));
                }
            }
            let url = peticion.url().to_string();
            let (ruta, consulta) = url.split_once('?').unwrap_or((&url, ""));
            let params = parametros(consulta);
            let param = |k: &str| params.iter().find(|(c, _)| c == k).map(|(_, v)| v.clone());
            let metodo = peticion.method().clone();
            if metodo == Method::Get && ruta == "/health" {
                return Ok(json(200, serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }).to_string()));
            }
            // Desde dónde se mira: se calcula en cada petición, porque un proyecto puede cambiar de
            // espacio mientras el servidor está en marcha.
            let ruta_db = db.clone();
            let mut base = db::abrir(&db)?;
            let servidor = Mirador::nuevo(&base, dir.clone(), proyecto_servidor.clone())?;
            let m = match mirar(&base, &servidor, param("dir"))? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            match (metodo, ruta) {
                (Method::Get, "/api/memories") => {
                    let v = memoria::recientes(&base, &m.vista, None, usize::MAX >> 1)?;
                    // Con cuántas versiones anteriores tiene cada una, para la cronología de la vista.
                    let mut q = base.prepare("SELECT memory_id, count(*) FROM memory_version GROUP BY memory_id")?;
                    let versiones: std::collections::HashMap<String, i64> =
                        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
                    crate::revisar::al_dia(&base)?;
                    let enlaces = crate::enlaces::todos(&base)?;
                    // Los enlaces, solo a las que también se ven: el id de una de otro cliente tampoco sale.
                    let visibles: std::collections::HashSet<&str> = v.iter().map(|m| m.id.as_str()).collect();
                    let v: Vec<serde_json::Value> = v
                        .iter()
                        .map(|m| {
                            let n = versiones.get(&m.id).copied().unwrap_or(0);
                            let links: Vec<&String> =
                                enlaces.get(&m.id).map(|l| l.iter().filter(|x| visibles.contains(x.as_str())).collect()).unwrap_or_default();
                            let mut j = serde_json::to_value(m).unwrap_or_default();
                            j["versions"] = n.into();
                            j["links"] = serde_json::json!(links);
                            j
                        })
                        .collect();
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Get, "/api/projects") => {
                    let espacio = if m.todo { None } else { Some(m.vista.espacio()) };
                    let cuerpo = serde_json::json!({
                        // Desde dónde se mira, para que la vista lo diga.
                        "scope": m.json(&base)?,
                        "projects": crate::proyecto::lista(&base, None, espacio, usize::MAX)?,
                        // Repetidos o en carpetas temporales: se proponen, nunca se juntan solos.
                        "duplicates": crate::nombres::propuestas(&base, &m.vista)?,
                        "spaces": crate::proyecto::espacios(&base)?.into_iter().filter(|e| m.ve_espacio(&e.key, false)).collect::<Vec<_>>(),
                        // Los proyectos que otro equipo tiene en otro espacio, con lo que dice
                        // `sync status` y la orden que lo arregla (solo los que aquí son de lo que se ve).
                        "space_conflicts": crate::sync::choques(&ruta_db).iter().filter(|x| m.ve_espacio(&x.here, false)).map(|x| {
                            let mut v = serde_json::to_value(x).unwrap_or_default();
                            v["text"] = x.texto().into();
                            v["command"] = x.orden().into();
                            v
                        }).collect::<Vec<_>>(),
                        // Los que están en pausa (una línea `project` dice otro espacio, o una carpeta
                        // de aquí entró en un espacio solo porque lo dice su repositorio), con las dos
                        // órdenes que la deciden.
                        "paused": crate::proyecto::en_pausa(&base)?.iter().filter(|x| m.ve_espacio(&x.here, false)).map(|x| {
                            let mut v = serde_json::to_value(x).unwrap_or_default();
                            v["text"] = crate::sync::texto_pausa(&ruta_db, x).into();
                            v["accept"] = crate::sync::orden_aceptar(x).into();
                            v["keep"] = crate::sync::orden_quedarse(x).into();
                            v
                        }).collect::<Vec<_>>(),
                    });
                    Ok(json(200, cuerpo.to_string()))
                }
                (Method::Get, "/api/overview") => {
                    // La vista de responsable, solo con lo que se ve desde el proyecto de la petición
                    // (su espacio) o, desde la carpeta del usuario, todo.
                    let meses = param("months").and_then(|n| n.parse().ok()).unwrap_or(crate::responsable::MESES);
                    Ok(json(200, crate::responsable::panorama(&base, &ruta_db, &m.base, meses)?.to_string()))
                }
                (Method::Get, "/api/review") => Ok(json(200, serde_json::to_string(&crate::revisar::avisos(&base, &m.vista)?)?)),
                (Method::Post, "/api/review/dismiss") => {
                    let v = cuerpo_json(&mut peticion)?;
                    let campo = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                    // Los ids enteros y en su forma (`resolver_id`) antes de mirar si se ven: uno en
                    // minúsculas o un trozo no está tal cual en la base, `ve` lo daba por visible y
                    // `descartar` sí lo encontraba (el de otro cliente incluido).
                    let resuelto = |k: &str| -> Result<std::result::Result<String, String>> {
                        let t = campo(k);
                        if t.trim().is_empty() {
                            return Ok(Ok(String::new()));
                        }
                        Ok(memoria::resolver_id(&base, &t).map_err(|e| format!("{e:#}")))
                    };
                    let (una, otra) = match (resuelto("memory_id")?, resuelto("other_id")?) {
                        (Ok(u), Ok(o)) => (u, o),
                        (Err(e), _) | (_, Err(e)) => return Ok(error(404, &e)),
                    };
                    if !m.ve(&base, &una)? || (!otra.is_empty() && !m.ve(&base, &otra)?) {
                        return Ok(fuera());
                    }
                    match crate::revisar::descartar(&base, &una, &campo("kind"), &otra) {
                        Ok(()) => Ok(json(200, serde_json::json!({ "dismissed": true }).to_string())),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Get, "/api/trash") => {
                    let mut v = Vec::new();
                    for o in memoria::papelera(&base)? {
                        if m.ve(&base, &o.id)? {
                            v.push(o);
                        }
                    }
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Post, "/api/restore") => {
                    let v = cuerpo_json(&mut peticion)?;
                    let id = v["id"].as_str().unwrap_or_default().trim().to_uppercase();
                    // Por su principio, como `restaurar`: si encaja con una sola, esa es la que se mira.
                    let ids: Vec<String> = base
                        .prepare("SELECT id FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?
                        .query_map([&id], |f| f.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    if let [una] = ids.as_slice()
                        && !id.is_empty()
                        && !m.ve(&base, una)?
                    {
                        return Ok(fuera());
                    }
                    let antes = crate::revisar::antes(&base)?;
                    match memoria::restaurar(&mut base, &id, None) {
                        Ok(r) => {
                            crate::revisar::al_dia_de(&base, &antes, std::slice::from_ref(&r.id))?;
                            crate::sync::programar(&ruta_db);
                            Ok(json(200, serde_json::to_string(&r)?))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                (Method::Post, "/api/redact") => {
                    let v = cuerpo_json(&mut peticion)?;
                    let id = match memoria::resolver_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.ve(&base, &id)? {
                        return Ok(fuera());
                    }
                    let antes = crate::revisar::antes(&base)?;
                    match memoria::redactar(&mut base, &id, None) {
                        Ok(r) => {
                            if !r.redacted.is_empty() {
                                crate::revisar::al_dia_de(&base, &antes, std::slice::from_ref(&r.id))?;
                                crate::sync::programar(&ruta_db);
                            }
                            let mut j = serde_json::to_value(&r)?;
                            j["redacted"] = r.redacted.len().into();
                            Ok(json(200, j.to_string()))
                        }
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                // El botón «Verificar» de la vista: la marca de una persona (`verificado`). No es una
                // herramienta (`/tools/`, lo que usan los agentes por HTTP), y pide lo que manda la página
                // de esta vista: `Origin` (de aquí: lo de otra web ya se ha cortado arriba) y el token que
                // va dentro de la página (`token_de_vista`); un `curl` con un `Origin` inventado no basta.
                (Method::Post, "/api/verify") => {
                    if cabecera("Origin").is_none() || !cabecera(CABECERA_VISTA).is_some_and(|t| mismo_secreto(&t, &vista)) {
                        return Ok(error(403, "verifying is for a person: use the button in the web view, or `omoi verify` in a terminal"));
                    }
                    let v = cuerpo_json(&mut peticion)?;
                    let id = match memoria::resolver_id(&base, v["id"].as_str().unwrap_or_default()) {
                        Ok(id) => id,
                        Err(e) => return Ok(error(404, &format!("{e:#}"))),
                    };
                    if !m.ve(&base, &id)? {
                        return Ok(fuera());
                    }
                    let r = if v["unverify"].as_bool().unwrap_or(false) {
                        crate::verificado::desverificar(&mut base, &id)?;
                        memoria::obtener(&base, &id)?
                    } else {
                        let actual = memoria::obtener(&base, &id)?;
                        let by = match v["by"].as_str().map(str::trim).filter(|b| !b.is_empty()) {
                            Some(b) => b.to_string(),
                            None => crate::verificado::quien_por_defecto(&base, &actual)?,
                        };
                        match crate::verificado::verificar(&mut base, &id, &by) {
                            Ok(r) => r,
                            Err(e) => return Ok(error(400, &format!("{e:#}"))),
                        }
                    };
                    crate::sync::programar(&ruta_db);
                    Ok(json(200, serde_json::to_string(&r)?))
                }
                (Method::Get, "/api/candidates") => {
                    // Lo apartado lleva la carpeta de su sesión: solo las de proyectos que se ven.
                    let mut ambitos: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
                    let mut v = Vec::new();
                    for c in crate::candidatas::todas(&base)? {
                        let se_ve = match ambitos.get(&c.project) {
                            Some(s) => *s,
                            None => {
                                let s = m.abarca(&Ambito::de(&base, &c.project)?);
                                ambitos.insert(c.project.clone(), s);
                                s
                            }
                        };
                        if se_ve {
                            v.push(c);
                        }
                    }
                    Ok(json(200, serde_json::to_string(&v)?))
                }
                (Method::Post, "/api/candidates/dismiss") => {
                    let v = cuerpo_json(&mut peticion)?;
                    let id = v["id"].as_i64().unwrap_or_default();
                    let proyecto: Option<String> = rusqlite::OptionalExtension::optional(base.query_row("SELECT project FROM candidate WHERE id = ?1", [id], |f| f.get(0)))?;
                    let Some(proyecto) = proyecto else { return Ok(error(404, "no such candidate")) };
                    if !m.abarca(&Ambito::de(&base, &proyecto)?) {
                        return Ok(error(403, "that candidate is from a project outside this scope"));
                    }
                    crate::candidatas::descartar(&base, id)?;
                    Ok(json(200, serde_json::json!({ "dismissed": true }).to_string()))
                }
                (Method::Get, "/api/sync") => {
                    // Desde un cliente (espacio), solo el repositorio de ese espacio: ni la URL ni el
                    // autor del general (el personal) ni los de otros espacios. Desde la carpeta del
                    // usuario o un proyecto sin espacio, el general.
                    let repo = if m.todo || m.espacio.is_empty() {
                        Some(crate::sync::rutas(&ruta_db))
                    } else {
                        crate::sync::repo_de_espacio(&ruta_db, &m.espacio)
                    };
                    let config = match &repo {
                        Some(r) => crate::sync::leer_config_de(r)?,
                        None => None,
                    };
                    let e = repo.as_ref().map(crate::sync::leer_estado_de).unwrap_or_default();
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
                // Las herramientas miran desde la carpeta de la petición, con las mismas comprobaciones
                // que por MCP (`search`, `get`, `update`, `forget`… no salen del espacio).
                (Method::Get, "/tools") => {
                    drop(base);
                    let (texto, _) = mcp::herramienta(ruta_db, m.dir, None, "", "http")?;
                    Ok(json(200, texto))
                }
                (Method::Get, "/search") => {
                    drop(base);
                    let Some(q) = param("q") else { return Ok(error(400, "missing q")) };
                    let mut args = serde_json::json!({ "query": q });
                    if let Some(n) = param("limit").and_then(|n| n.parse::<u64>().ok()) {
                        args["limit"] = n.into();
                    }
                    let (texto, es_error) = mcp::herramienta(ruta_db, m.dir, Some("search"), &args.to_string(), "http")?;
                    Ok(json(if es_error { 400 } else { 200 }, texto))
                }
                (Method::Post, r) if r.starts_with("/tools/") => {
                    drop(base);
                    let nombre = &r["/tools/".len()..];
                    let mut cuerpo = String::new();
                    peticion.as_reader().read_to_string(&mut cuerpo)?;
                    match mcp::herramienta(ruta_db, m.dir, Some(nombre), &cuerpo, "http") {
                        Ok((texto, es_error)) => Ok(json(if es_error { 400 } else { 200 }, texto)),
                        Err(e) => Ok(error(400, &format!("{e:#}"))),
                    }
                }
                _ => Ok(error(404, "not found: GET /, GET /api/memories, GET /api/projects, GET /api/overview, GET /api/review, POST /api/review/dismiss, GET /api/trash, POST /api/restore, POST /api/redact, POST /api/verify, GET /api/candidates, POST /api/candidates/dismiss, GET /api/sync, GET /health, GET /tools, GET /search?q=, POST /tools/<name>")),
            }
        })()
        .unwrap_or_else(|e| error(500, &format!("{e:#}")));
        let _ = peticion.respond(respuesta);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::url_vista;

    /// El enlace a una memoria, con y sin el token de `OMOIKANE_HTTP_TOKEN` (que la página quita de la
    /// dirección y cambia por `#/memoria/<id>`).
    #[test]
    fn la_url_de_la_vista() {
        assert_eq!(url_vista(7438, None, None), "http://127.0.0.1:7438/");
        assert_eq!(url_vista(7438, None, Some("01ABC")), "http://127.0.0.1:7438/#/memoria/01ABC");
        assert_eq!(url_vista(1, Some("t"), None), "http://127.0.0.1:1/#token=t");
        assert_eq!(url_vista(1, Some("t"), Some("01ABC")), "http://127.0.0.1:1/#token=t&to=/memoria/01ABC");
    }

    /// 1.8.6: un apóstrofo sin escapar en un texto entre comillas simples («It's fine as it is») daba un
    /// SyntaxError que dejaba la vista entera sin JavaScript. Cada `<script>` de la página pasa por
    /// `node --check`; sin node en el equipo el test se salta (en la CI hay, y además lo comprueba ella).
    #[test]
    fn el_javascript_de_la_vista_compila() {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("sin node: no se comprueba el JavaScript de la vista");
            return;
        }
        let dir = crate::comun::Temporal::nueva("webjs");
        let mut resto = super::PAGINA;
        let mut n = 0;
        while let Some(i) = resto.find("<script") {
            let tras = &resto[i..];
            let abre = tras.find('>').expect("<script sin cerrar") + 1;
            let cierra = tras.find("</script>").expect("falta </script>");
            n += 1;
            let fichero = dir.join(format!("web-{n}.js"));
            std::fs::write(&fichero, &tras[abre..cierra]).unwrap();
            let salida = std::process::Command::new("node").arg("--check").arg(&fichero).output().unwrap();
            assert!(salida.status.success(), "el <script> {n} de web.html no compila:\n{}", String::from_utf8_lossy(&salida.stderr));
            resto = &tras[cierra..];
        }
        assert!(n > 0, "web.html sin <script>");
    }
}
