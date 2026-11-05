//! Qué modelo se usa y cómo llega al disco.
//!
//! Cada modelo vive en su carpeta, que no se toca nunca después de crearla:
//! `~/.omoikane/models/<nombre>-<hash>/`. El fichero `actual` dice cuál se usa. Windows no deja
//! sobrescribir ni borrar un fichero mapeado mientras algún proceso lo usa (y en Linux y macOS
//! sobrescribirlo en sitio corrompería en silencio a quien lo tenga abierto), así que instalar uno
//! nuevo es: compilar en una carpeta aparte, renombrarla y cambiar `actual` de forma atómica.

use crate::embeber::{Modelo, compilar};
use crate::tr;
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const NOMBRE: &str = "potion-multilingual-128M-int8";
/// De dónde se descarga la versión int8 de potion-multilingual-128M (The Minish Lab), en orden:
/// Hugging Face y, si falla, la Release de Omoikane en GitHub con los mismos ficheros.
/// `OMOIKANE_MODEL_SOURCE` las sustituye (una o varias separadas por comas: un espejo propio, o
/// para probar una en concreto).
pub const ORIGENES: [&str; 2] = [
    "https://huggingface.co/natzx94/potion-multilingual-128M-int8/resolve/main",
    "https://github.com/marcmayol/omoikane/releases/download/modelo-potion-int8-v1",
];
const FICHEROS: [&str; 3] = ["model.safetensors", "tokenizer.json", "config.json"];
const ACTUAL: &str = "actual";

/// `OMOIKANE_MODELS` o `~/.omoikane/models`.
pub fn carpeta() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_MODELS").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    std::env::home_dir().unwrap_or_default().join(".omoikane").join("models")
}

/// Carpeta del modelo en uso, si hay uno instalado y completo.
pub fn actual() -> Option<PathBuf> {
    let base = carpeta();
    let nombre = std::fs::read_to_string(base.join(ACTUAL)).ok()?;
    let dir = base.join(nombre.trim());
    dir.join("meta.json").exists().then_some(dir)
}

/// El modelo en uso, abierto. `None` si no hay ninguno: todo funciona igual, sin búsqueda por
/// significado.
pub fn abrir_actual() -> Result<Option<Modelo>> {
    actual().map(|d| Modelo::abrir(&d)).transpose()
}

/// FNV-1a de 64 bits: para nombrar la carpeta por su contenido, no para seguridad.
fn huella(ficheros: &[PathBuf]) -> Result<String> {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut buf = vec![0u8; 1 << 20];
    for f in ficheros {
        let mut r = std::fs::File::open(f).with_context(|| f.display().to_string())?;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
    }
    Ok(format!("{:08x}", h >> 32))
}

fn origenes() -> Vec<String> {
    match std::env::var("OMOIKANE_MODEL_SOURCE") {
        Ok(o) if !o.trim().is_empty() => {
            o.split(',').map(|x| x.trim().trim_end_matches('/').to_string()).filter(|x| !x.is_empty()).collect()
        }
        _ => ORIGENES.iter().map(|o| o.to_string()).collect(),
    }
}

/// Descarga los ficheros del modelo int8 a `destino`, probando las fuentes en orden.
fn descargar(destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    let mut fallos = Vec::new();
    for origen in origenes() {
        match descargar_de(&origen, destino, aviso) {
            Ok(()) => return Ok(()),
            Err(e) => {
                aviso(&tr!("failed: {e:#}", "falló: {e:#}"));
                fallos.push(format!("{origen}: {e:#}"));
            }
        }
    }
    bail!(tr!("couldn't download the model from any source:
{}", "no se pudo descargar el modelo de ninguna fuente:
{}", fallos.join("
")))
}

/// `aviso` recibe una línea por fichero.
fn descargar_de(origen: &str, destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    descargar_ficheros(origen, &FICHEROS, destino, aviso)
}

/// El cliente HTTP de todas las descargas (el modelo, las actualizaciones): con los certificados raíz
/// del sistema, y no solo los de Mozilla que trae ureq. En la red de una empresa que inspecciona el
/// HTTPS, su certificado raíz está en el almacén del sistema (por eso Git y PowerShell conectan) y no
/// en la lista de Mozilla. Las actualizaciones se siguen comprobando por su firma Ed25519.
fn cliente() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder().tls_config(tls).build().new_agent()
}

/// GET con los certificados del sistema y, si falla sin que el servidor llegue a responder, otra vez
/// con la lista de Mozilla: un Linux mínimo sin `ca-certificates` no tiene almacén del sistema.
pub fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let ua = format!("omoi/{}", env!("CARGO_PKG_VERSION"));
    match cliente().get(url).header("User-Agent", &ua).call() {
        Err(e) if !matches!(e, ureq::Error::StatusCode(_)) => ureq::get(url).header("User-Agent", &ua).call(),
        r => r,
    }
}

/// Descarga `ficheros` de `origen` a `destino` (cada uno a `.part` y renombrado al llegar entero).
pub(crate) fn descargar_ficheros(origen: &str, ficheros: &[&str], destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    std::fs::create_dir_all(destino)?;
    let host = origen.split('/').nth(2).unwrap_or(origen);
    for &f in ficheros {
        let url = format!("{origen}/{f}");
        let resp = get(&url).with_context(|| tr!("can't download {url}", "no se puede descargar {url}"))?;
        let total: Option<u64> =
            resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        aviso(&match total {
            Some(t) => tr!("downloading {f} ({:.1} MB) from {host}", "descargando {f} ({:.1} MB) de {host}", t as f64 / 1e6),
            None => tr!("downloading {f} from {host}", "descargando {f} de {host}"),
        });
        let parte = destino.join(format!("{f}.part"));
        let mut salida = std::fs::File::create(&parte)?;
        let copiados = std::io::copy(&mut resp.into_body().into_reader(), &mut salida)?;
        salida.flush()?;
        drop(salida);
        if let Some(t) = total
            && copiados != t
        {
            bail!(tr!("{f} arrived incomplete ({copiados} of {t} bytes)", "{f} llegó incompleto ({copiados} de {t} bytes)"));
        }
        std::fs::rename(&parte, destino.join(f))?;
    }
    Ok(())
}

/// Instala el modelo y lo deja como `actual`. Sin `desde`, lo descarga de Hugging Face; con
/// `desde`, lo toma de una carpeta con el int8 (sin red). Devuelve el nombre de la carpeta.
pub fn instalar(desde: Option<&Path>, aviso: &mut dyn FnMut(&str)) -> Result<String> {
    let base = carpeta();
    std::fs::create_dir_all(&base).with_context(|| tr!("can't create {}", "no se puede crear {}", base.display()))?;
    let temporal = base.join(format!(".instalando-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    let resultado = (|| -> Result<String> {
        let origen = match desde {
            Some(d) => d.to_path_buf(),
            None => {
                let d = temporal.join("int8");
                descargar(&d, aviso)?;
                d
            }
        };
        let id = format!("{NOMBRE}-{}", huella(&FICHEROS.map(|f| origen.join(f)))?);
        let destino = base.join(&id);
        if !destino.join("meta.json").exists() {
            aviso(&tr!("compiling the model (a few seconds, only once)", "compilando el modelo (unos segundos, solo una vez)"));
            let compilado = temporal.join("compilado");
            compilar(&origen, &compilado)?;
            // Si otro proceso lo instaló a la vez, el suyo vale igual: mismo contenido, mismo nombre.
            if std::fs::rename(&compilado, &destino).is_err() && !destino.join("meta.json").exists() {
                bail!(tr!("can't move the model into {}", "no se puede mover el modelo a {}", destino.display()));
            }
        }
        // `actual` cambia de golpe: quien lo lea ve el viejo o el nuevo, nunca uno a medias.
        let tmp = base.join(format!("{ACTUAL}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, &id)?;
        std::fs::rename(&tmp, base.join(ACTUAL))?;
        Ok(id)
    })();
    let _ = std::fs::remove_dir_all(&temporal);
    resultado
}

/// Modelos instalados: (nombre, ¿es el actual?).
pub fn instalados() -> Vec<(String, bool)> {
    let actual = actual().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    let mut v: Vec<(String, bool)> = std::fs::read_dir(carpeta())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("meta.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .map(|n| {
            let es = actual.as_deref() == Some(n.as_str());
            (n, es)
        })
        .collect();
    v.sort();
    v
}

/// Borra los modelos que no son el actual y los restos de instalaciones cortadas. Lo que siga
/// abierto por otra sesión (Windows no deja borrarlo) se queda para la próxima vez.
pub fn limpiar() -> Vec<String> {
    let actual = actual().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    let mut borrados = Vec::new();
    for e in std::fs::read_dir(carpeta()).into_iter().flatten().flatten() {
        let nombre = e.file_name().to_string_lossy().into_owned();
        if !e.path().is_dir() || Some(&nombre) == actual.as_ref() {
            continue;
        }
        // Puede ser una instalación en marcha de otro proceso: no se toca (cada instalación borra
        // la suya al terminar, también si falla).
        if nombre.starts_with(".instalando-") {
            continue;
        }
        if std::fs::remove_dir_all(e.path()).is_ok() {
            borrados.push(nombre);
        }
    }
    borrados
}
