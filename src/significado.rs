//! El servicio que mantiene abierto EmbeddingGemma (`gemma`) para quien necesite un vector.
//!
//! Cargar el modelo cuesta unos 2 s y el recall arranca un proceso en cada mensaje, así que el modelo
//! vive en un proceso aparte, uno por usuario: `omoi model serve-semantic` (oculto). Escucha solo en
//! 127.0.0.1, en un puerto libre, y exige un token; los dos, con su pid, van en
//! `~/.omoikane/significado.json` cuando el modelo ya está cargado. Se cierra solo tras un rato sin
//! peticiones (`OMOIKANE_SEMANTIC_IDLE` segundos, 30 min por defecto).
//!
//! Quien pide (`vectores`) tiene un presupuesto de tiempo: si no hay servicio, o tarda más, sigue sin
//! significado, como hasta ahora, y deja el servicio arrancando para la próxima vez. Nunca espera a
//! que cargue.
//!
//! Protocolo: una línea JSON por petición y otra por respuesta.
//! `{"token":…,"tipo":"pregunta"|"documento","textos":[…]}` → `{"vectores":[[…]…]}` o `{"error":…}`;
//! `{"token":…,"ping":true}` → `{"pid":…,"modelo":…}`.

use crate::gemma::{Gemma, Tipo};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REGISTRO: &str = "significado.json";
const ARRANCANDO: &str = "significado.arrancando";
/// Carpeta del modelo dentro de `~/.omoikane/models`.
pub const NOMBRE_MODELO: &str = "embeddinggemma-300m";
const OCIOSO_POR_DEFECTO: u64 = 30 * 60;
/// Tras lanzar el servicio, no se vuelve a lanzar hasta pasado esto (lo que tarda en cargar, de sobra).
const REINTENTO_ARRANQUE: u64 = 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registro {
    pub puerto: u16,
    pub token: String,
    pub pid: u32,
    pub modelo: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Peticion {
    token: String,
    #[serde(default)]
    tipo: Option<String>,
    #[serde(default)]
    textos: Vec<String>,
    #[serde(default)]
    ping: bool,
}

fn carpeta() -> Option<PathBuf> {
    std::env::var_os("OMOIKANE_HOME").filter(|r| !r.is_empty()).map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".omoikane")))
}

const PREFERENCIA: &str = "semantic";

/// Lo que eligió el usuario con `omoi model semantic --on/--off` (`None`: no eligió nada).
pub fn preferencia() -> Option<bool> {
    match std::fs::read_to_string(carpeta()?.join(PREFERENCIA)).ok()?.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Guarda la elección. Apagarlo cierra también el servicio si está abierto: el vigía ve que el
/// registro ya no es suyo y sale. Con él apagado todo va como antes de la 1.2: solo palabras y potion.
pub fn fijar(on: bool) -> Result<()> {
    let c = carpeta().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    std::fs::write(c.join(PREFERENCIA), if on { "on
" } else { "off
" })?;
    if !on {
        let _ = std::fs::remove_file(c.join(REGISTRO));
    }
    Ok(())
}

/// ¿Se usa el canal por significado? `OMOIKANE_SEMANTIC=0`/`1` manda; si no, la preferencia; y si no
/// hay ninguna, encendido (solo hace algo si el modelo está instalado).
pub fn activado() -> bool {
    match std::env::var("OMOIKANE_SEMANTIC") {
        Ok(v) if !v.is_empty() => v != "0",
        _ => preferencia().unwrap_or(true),
    }
}

/// La carpeta de EmbeddingGemma si está instalado: `OMOIKANE_SEMANTIC_MODEL`, o
/// `~/.omoikane/models/embeddinggemma-300m`.
pub fn modelo() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOIKANE_SEMANTIC_MODEL")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::modelos::carpeta().join(NOMBRE_MODELO));
    crate::gemma::es_modelo(&dir).then_some(dir)
}

fn ahora() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn registro() -> Option<Registro> {
    serde_json::from_str(&std::fs::read_to_string(carpeta()?.join(REGISTRO)).ok()?).ok()
}

/// Manda `p` y lee una línea de respuesta, sin pasarse de `presupuesto` en total.
fn pedir(r: &Registro, p: &Peticion, presupuesto: Duration) -> Result<serde_json::Value> {
    let inicio = Instant::now();
    let dir = SocketAddr::from(([127, 0, 0, 1], r.puerto));
    let s = TcpStream::connect_timeout(&dir, presupuesto)?;
    let resto = || presupuesto.saturating_sub(inicio.elapsed()).max(Duration::from_millis(1));
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(resto()))?;
    let mut linea = serde_json::to_string(p)?;
    linea.push('\n');
    (&s).write_all(linea.as_bytes())?;
    s.set_read_timeout(Some(resto()))?;
    let mut respuesta = String::new();
    BufReader::new(&s).read_line(&mut respuesta)?;
    let v: serde_json::Value = serde_json::from_str(&respuesta).context("semantic service: bad answer")?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("semantic service: {e}");
    }
    Ok(v)
}

/// ¿Responde el servicio? Devuelve su registro si sí.
pub fn vivo(presupuesto: Duration) -> Option<Registro> {
    let r = registro()?;
    let v = pedir(&r, &Peticion { token: r.token.clone(), ping: true, ..Default::default() }, presupuesto).ok()?;
    (v.get("pid").and_then(|p| p.as_u64()) == Some(r.pid as u64)).then_some(r)
}

/// Lanza el servicio aparte si hay modelo y no se lanzó hace poco. No espera a que cargue.
pub fn arrancar() {
    if !activado() || modelo().is_none() {
        return;
    }
    let Some(c) = carpeta() else { return };
    let marca = c.join(ARRANCANDO);
    let reciente = std::fs::read_to_string(&marca).ok().and_then(|t| t.trim().parse::<u64>().ok()).is_some_and(|t| ahora().saturating_sub(t) < REINTENTO_ARRANQUE);
    if reciente {
        return;
    }
    let _ = std::fs::create_dir_all(&c);
    if std::fs::write(&marca, ahora().to_string()).is_ok() {
        crate::sync::lanzar_aparte(&["model", "serve-semantic"]);
    }
}

/// Los vectores de `textos`, si el servicio los da dentro de `presupuesto`. Si no hay servicio, lo
/// deja arrancando y devuelve `None`: quien llama sigue sin significado.
pub fn vectores(textos: &[&str], tipo: Tipo, presupuesto: Duration) -> Option<Vec<Vec<f32>>> {
    if !activado() || modelo().is_none() {
        return None;
    }
    let Some(r) = registro() else {
        arrancar();
        return None;
    };
    let p = Peticion {
        token: r.token.clone(),
        tipo: Some(if tipo == Tipo::Pregunta { "pregunta" } else { "documento" }.into()),
        textos: textos.iter().map(|t| t.to_string()).collect(),
        ping: false,
    };
    match pedir(&r, &p, presupuesto) {
        Ok(v) => serde_json::from_value(v.get("vectores")?.clone()).ok(),
        Err(e) => {
            // Rechazada o sin nadie escuchando: el servicio murió (o lo cerraron) y hay que lanzarlo
            // otra vez. Un plazo agotado es solo lentitud: no se lanza otro.
            let caido = e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
            if caido {
                arrancar();
            }
            None
        }
    }
}

fn token_nuevo() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn atender(s: TcpStream, modelo: &Gemma, token: &str, nombre: &str, uso: &AtomicU64) -> Result<()> {
    s.set_nodelay(true)?;
    let mut lector = BufReader::new(&s);
    let mut linea = String::new();
    while lector.read_line(&mut linea)? > 0 {
        uso.store(ahora(), Ordering::Relaxed);
        let respuesta = match serde_json::from_str::<Peticion>(&linea) {
            Err(e) => serde_json::json!({ "error": e.to_string() }),
            Ok(p) if p.token != token => serde_json::json!({ "error": "bad token" }),
            Ok(p) if p.ping => serde_json::json!({ "pid": std::process::id(), "modelo": nombre }),
            Ok(p) => {
                let tipo = if p.tipo.as_deref() == Some("documento") { Tipo::Documento } else { Tipo::Pregunta };
                match p.textos.iter().map(|t| modelo.vector(t, tipo)).collect::<Result<Vec<_>>>() {
                    Ok(v) => serde_json::json!({ "vectores": v }),
                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                }
            }
        };
        let mut salida = respuesta.to_string();
        salida.push('\n');
        (&s).write_all(salida.as_bytes())?;
        linea.clear();
    }
    Ok(())
}

/// El servicio (`omoi model serve-semantic`). Si ya hay uno respondiendo, sale sin hacer nada.
pub fn servir() -> Result<()> {
    if !activado() {
        bail!("the meaning model is off (`omoi model semantic --on` turns it on)");
    }
    if vivo(Duration::from_millis(500)).is_some() {
        return Ok(());
    }
    let dir = modelo().context("EmbeddingGemma is not installed")?;
    let c = carpeta().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    let gemma = Arc::new(Gemma::cargar(&dir)?);
    let escucha = TcpListener::bind(("127.0.0.1", 0))?;
    let yo = Registro {
        puerto: escucha.local_addr()?.port(),
        token: token_nuevo()?,
        pid: std::process::id(),
        modelo: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    // Primero a un temporal y luego renombrar: quien lee nunca ve un registro a medias.
    let tmp = c.join(format!("{REGISTRO}.{}", yo.pid));
    std::fs::write(&tmp, serde_json::to_string(&yo)?)?;
    std::fs::rename(&tmp, c.join(REGISTRO))?;
    let _ = std::fs::remove_file(c.join(ARRANCANDO));
    let uso = Arc::new(AtomicU64::new(ahora()));
    let ocioso = std::env::var("OMOIKANE_SEMANTIC_IDLE").ok().and_then(|s| s.parse().ok()).unwrap_or(OCIOSO_POR_DEFECTO);
    {
        // Vigía: se cierra tras `ocioso` sin peticiones, o si otro servicio arrancó a la vez y se
        // quedó con el registro (gana el último que lo escribió).
        let (uso, yo, c) = (uso.clone(), yo.clone(), c.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let mio = registro().is_some_and(|r| r.pid == yo.pid && r.puerto == yo.puerto);
                if !mio {
                    std::process::exit(0);
                }
                if ahora().saturating_sub(uso.load(Ordering::Relaxed)) >= ocioso {
                    let _ = std::fs::remove_file(c.join(REGISTRO));
                    std::process::exit(0);
                }
            }
        });
    }
    for s in escucha.incoming().flatten() {
        let (gemma, token, nombre, uso) = (gemma.clone(), yo.token.clone(), yo.modelo.clone(), uso.clone());
        std::thread::spawn(move || {
            let _ = atender(s, &gemma, &token, &nombre, &uso);
        });
    }
    Ok(())
}
