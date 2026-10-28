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
/// La versión int8 de potion-multilingual-128M (The Minish Lab), cuantizada para Omoikane.
pub const ORIGEN: &str = "https://huggingface.co/natzx94/potion-multilingual-128M-int8/resolve/main";
const FICHEROS: [&str; 3] = ["model.safetensors", "tokenizer.json", "config.json"];
const ACTUAL: &str = "actual";

/// `OMOIKANE_MODELS` o `~/.omoikane/models`.
pub fn carpeta() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_MODELS").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    dirs::home_dir().unwrap_or_default().join(".omoikane").join("models")
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

/// Descarga los ficheros del modelo int8 a `destino`. `aviso` recibe una línea por fichero.
fn descargar(destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    std::fs::create_dir_all(destino)?;
    for f in FICHEROS {
        let url = format!("{ORIGEN}/{f}");
        let resp = ureq::get(&url).call().with_context(|| tr!("can't download {url}", "no se puede descargar {url}"))?;
        let total: Option<u64> =
            resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        aviso(&match total {
            Some(t) => tr!("downloading {f} ({:.1} MB)", "descargando {f} ({:.1} MB)", t as f64 / 1e6),
            None => tr!("downloading {f}", "descargando {f}"),
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
