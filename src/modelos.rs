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
use std::time::Duration;

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
/// SHA-256 de cada fichero publicado del modelo ligero: lo que se descarga tiene que ser exactamente
/// esto, venga del espejo que venga (como `significado::SUMAS`). Comprobados el 8-nov-2026 bajando los
/// de Hugging Face, contra los `lfs.oid` de su API y los `digest` de la Release de GitHub.
const SUMAS: [(&str, &str); 3] = [
    ("model.safetensors", "8cadee28bbc4c52ec36c33af1e6ee8ffcb0e40beb195d29d1b8304d74872df96"),
    ("tokenizer.json", "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1"),
    ("config.json", "595e4cab2093732efd5dbe084fd5c1826b5eea693b73b4c1fd971672867d2e54"),
];
const ACTUAL: &str = "actual";

/// `OMOIKANE_MODELS`, o `models` en la carpeta de Omoikane (`OMOIKANE_HOME` o `~/.omoikane`).
pub fn carpeta() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_MODELS").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    crate::entorno::casa().unwrap_or_else(|| PathBuf::from(".omoikane")).join("models")
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

/// `aviso` recibe una línea por fichero. Lo bajado tiene que cuadrar con `SUMAS`.
fn descargar_de(origen: &str, destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    descargar_ficheros(origen, &FICHEROS, destino, aviso)?;
    comprobar_sumas(destino, &SUMAS)
}

/// Que cada fichero de `destino` tenga su SHA-256. Si uno no cuadra se borran todos: lo que venga del
/// espejo siguiente no se mezcla con esto.
pub(crate) fn comprobar_sumas(destino: &Path, sumas: &[(&str, &str)]) -> Result<()> {
    for (f, esperada) in sumas {
        let s = suma(&destino.join(f))?;
        if s != *esperada {
            for (f, _) in sumas {
                let _ = std::fs::remove_file(destino.join(f));
            }
            bail!(tr!("{f}: wrong SHA-256 ({s})", "{f}: SHA-256 equivocado ({s})"));
        }
    }
    Ok(())
}

/// El SHA-256 de un fichero, en hexadecimal.
pub(crate) fn suma(f: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut r = std::fs::File::open(f).with_context(|| f.display().to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// El cliente HTTP de todas las descargas (el modelo, las actualizaciones): con los certificados raíz
/// del sistema, y no solo los de Mozilla que trae ureq. En la red de una empresa que inspecciona el
/// HTTPS, su certificado raíz está en el almacén del sistema (por eso Git y PowerShell conectan) y no
/// en la lista de Mozilla. Las actualizaciones se siguen comprobando por su firma Ed25519.
fn cliente(sistema: bool, t: Tiempos) -> ureq::Agent {
    let mut c = ureq::Agent::config_builder()
        .timeout_resolve(Some(t.conexion))
        .timeout_connect(Some(t.conexion))
        .timeout_send_request(Some(t.respuesta))
        .timeout_recv_response(Some(t.respuesta))
        .timeout_recv_body(Some(t.cuerpo));
    if sistema {
        c = c.tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build());
    }
    c.build().new_agent()
}

/// Los tiempos máximos de una descarga. Sin ellos, una red que se queda a medias (un proxy que no
/// contesta, una WiFi que se cae sin cerrar la conexión) dejaba `omoi model install` o la
/// actualización en segundo plano esperando para siempre.
#[derive(Clone, Copy, Debug)]
pub struct Tiempos {
    /// Para conectar: DNS, TCP y TLS, cada uno.
    pub conexion: Duration,
    /// Para mandar la petición y que llegue la cabecera de la respuesta.
    pub respuesta: Duration,
    /// Para el cuerpo entero (ureq lo cuenta de principio a fin, no por lectura).
    pub cuerpo: Duration,
    /// Lo más que puede pasar sin llegar ningún byte del cuerpo (`descargar_ficheros`): un cuelgue a
    /// medias se corta por aquí, no por el total.
    pub inactividad: Duration,
}

/// Los de los modelos. Un fichero entero puede tardar mucho en una conexión lenta (el de significado
/// son cientos de MB: a 100 KB/s, más de una hora; con el límite total de 60 minutos de antes no
/// acababa nunca), así que lo que corta un cuelgue es la inactividad: 120 s sin recibir nada. El total
/// (24 h) solo está para que el hilo que lee no se quede para siempre si nadie lo espera ya.
pub(crate) const TIEMPOS_MODELO: Tiempos =
    Tiempos { conexion: Duration::from_secs(30), respuesta: Duration::from_secs(60), cuerpo: Duration::from_secs(24 * 60 * 60), inactividad: Duration::from_secs(120) };
/// Los de las actualizaciones (el binario, `SHA256SUMS`, la última versión): pocos MB, con el total basta.
pub(crate) const TIEMPOS_PEQUENO: Tiempos = Tiempos { cuerpo: Duration::from_secs(10 * 60), ..TIEMPOS_MODELO };

/// Copia `lector` en `salida` y falla (`TimedOut`) si pasa `inactividad` sin llegar ningún byte. ureq no
/// tiene un tiempo máximo por lectura (el del cuerpo es para el cuerpo entero), así que lee un hilo
/// aparte y aquí se espera cada trozo con su tiempo. Si se corta por inactividad, ese hilo se queda
/// esperando hasta que ureq lo corte por el total, sin nada abierto de quien llama: lo escribe este.
fn copiar_con_inactividad(lector: impl std::io::Read + Send + 'static, salida: &mut impl Write, inactividad: Duration) -> std::io::Result<u64> {
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};
    let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(8);
    std::thread::spawn(move || {
        let mut lector = lector;
        let mut b = vec![0u8; 64 * 1024];
        loop {
            let r = match lector.read(&mut b) {
                Ok(n) => Ok(b[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            };
            let fin = !matches!(&r, Ok(v) if !v.is_empty());
            if tx.send(r).is_err() || fin {
                return;
            }
        }
    });
    let mut total = 0u64;
    loop {
        match rx.recv_timeout(inactividad) {
            Ok(Ok(v)) if v.is_empty() => return Ok(total),
            Ok(Ok(v)) => {
                salida.write_all(&v)?;
                total += v.len() as u64;
            }
            Ok(Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("no data for {} s", inactividad.as_secs_f32())));
            }
            Err(RecvTimeoutError::Disconnected) => return Err(std::io::Error::other("the download reader ended")),
        }
    }
}

/// GET con los certificados del sistema y, si falla sin que el servidor llegue a responder, otra vez
/// con la lista de Mozilla: un Linux mínimo sin `ca-certificates` no tiene almacén del sistema. Un
/// tiempo agotado no se reintenta (sería esperar otro tanto): da su error.
pub fn get(url: &str, t: Tiempos) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let ua = format!("omoi/{}", env!("CARGO_PKG_VERSION"));
    match cliente(true, t).get(url).header("User-Agent", &ua).call() {
        Err(e) if !matches!(e, ureq::Error::StatusCode(_)) && !es_tiempo_agotado(&e) => cliente(false, t).get(url).header("User-Agent", &ua).call(),
        r => r,
    }
}

/// Si un error de E/S de una descarga es un tiempo agotado (ureq lo envuelve en `io::Error`).
pub(crate) fn es_cuelgue(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut || e.get_ref().and_then(|x| x.downcast_ref::<ureq::Error>()).is_some_and(|x| matches!(x, ureq::Error::Timeout(_)))
}

/// Lo mismo con un error de ureq.
pub(crate) fn es_tiempo_agotado(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => es_cuelgue(io),
        _ => false,
    }
}

/// El error de `get`, claro si fue un tiempo agotado.
pub(crate) fn error_de_get(e: ureq::Error, url: &str) -> anyhow::Error {
    let host = url.split('/').nth(2).unwrap_or(url);
    match e {
        e if es_tiempo_agotado(&e) => anyhow::anyhow!(tr!(
            "{host} didn't answer in time ({e}): check the network or the proxy and try again",
            "{host} no contestó a tiempo ({e}): mira la red o el proxy y vuelve a probar"
        )),
        e => anyhow::Error::new(e).context(tr!("can't download {url}", "no se puede descargar {url}")),
    }
}

/// Descarga `ficheros` de `origen` a `destino` (cada uno a `.part` y renombrado al llegar entero; si
/// falla o se cuelga, el `.part` se borra y no queda nada a medias con el nombre bueno).
pub(crate) fn descargar_ficheros(origen: &str, ficheros: &[&str], destino: &Path, aviso: &mut dyn FnMut(&str)) -> Result<()> {
    descargar_ficheros_con(origen, ficheros, destino, aviso, TIEMPOS_MODELO)
}

/// `descargar_ficheros` con otros tiempos máximos (para las pruebas).
fn descargar_ficheros_con(origen: &str, ficheros: &[&str], destino: &Path, aviso: &mut dyn FnMut(&str), tiempos: Tiempos) -> Result<()> {
    std::fs::create_dir_all(destino)?;
    let host = origen.split('/').nth(2).unwrap_or(origen);
    for &f in ficheros {
        let url = format!("{origen}/{f}");
        let resp = get(&url, tiempos).map_err(|e| error_de_get(e, &url))?;
        let total: Option<u64> =
            resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        aviso(&match total {
            Some(t) => tr!("downloading {f} ({:.1} MB) from {host}", "descargando {f} ({:.1} MB) de {host}", t as f64 / 1e6),
            None => tr!("downloading {f} from {host}", "descargando {f} de {host}"),
        });
        let parte = destino.join(format!("{f}.part"));
        let r = (|| -> Result<()> {
            let mut salida = std::fs::File::create(&parte)?;
            let copiados = copiar_con_inactividad(resp.into_body().into_reader(), &mut salida, tiempos.inactividad).map_err(|e| {
                if es_cuelgue(&e) {
                    anyhow::anyhow!(tr!(
                        "the download of {f} from {host} stalled or took too long ({e}); nothing was left half-written",
                        "la descarga de {f} de {host} se colgó o tardó demasiado ({e}); no queda nada a medias"
                    ))
                } else {
                    anyhow::Error::new(e).context(tr!("the download of {f} was cut", "la descarga de {f} se cortó"))
                }
            })?;
            salida.flush()?;
            drop(salida);
            if let Some(t) = total
                && copiados != t
            {
                bail!(tr!("{f} arrived incomplete ({copiados} of {t} bytes)", "{f} llegó incompleto ({copiados} de {t} bytes)"));
            }
            std::fs::rename(&parte, destino.join(f))?;
            Ok(())
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&parte);
        }
        r?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Lo que contesta el servidor de prueba en una conexión.
    type Respuesta = Box<dyn FnOnce(&mut std::net::TcpStream) + Send>;

    /// Un servidor HTTP en 127.0.0.1 que atiende una conexión por cada función de `respuestas`, contesta
    /// lo que esa función escriba y luego se queda callado. Devuelve la URL base.
    fn servidor(respuestas: Vec<Respuesta>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for responder in respuestas {
                let Ok((mut s, _)) = l.accept() else { return };
                // La petición entera (sin cuerpo) antes de contestar.
                let mut leido = Vec::new();
                let mut b = [0u8; 1024];
                while !leido.windows(4).any(|w| w == b"\r\n\r\n") {
                    match std::io::Read::read(&mut s, &mut b) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => leido.extend_from_slice(&b[..n]),
                    }
                }
                responder(&mut s);
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        base
    }

    /// El total del cuerpo, de sobra: lo que corta un cuelgue es la inactividad.
    fn rapidos() -> Tiempos {
        Tiempos { conexion: Duration::from_secs(2), respuesta: Duration::from_millis(800), cuerpo: Duration::from_secs(60), inactividad: Duration::from_millis(800) }
    }

    fn carpeta_de_prueba(nombre: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omoi-modelos-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn un_cuerpo_colgado_da_error_y_no_deja_nada() {
        // Dice 1000 bytes, manda 10 y se calla.
        let base = servidor(vec![Box::new(|s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789");
            let _ = s.flush();
        })]);
        let d = carpeta_de_prueba("cuerpo");
        let inicio = std::time::Instant::now();
        let e = descargar_ficheros_con(&base, &["config.json"], &d, &mut |_| {}, rapidos()).unwrap_err();
        assert!(inicio.elapsed() < Duration::from_secs(4), "tardó {:?}", inicio.elapsed());
        let e = format!("{e:#}");
        assert!(e.contains("config.json") && (e.contains("stalled") || e.contains("colgó")), "{e}");
        assert!(!d.join("config.json").exists() && !d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 1.8.6: el límite del cuerpo era para el fichero entero (60 minutos), y el modelo de significado a
    /// unos 100 KB/s no acababa nunca. Ahora cuenta la inactividad: una descarga lenta pero que no para
    /// llega entera aunque tarde mucho más que la inactividad permitida.
    #[test]
    fn una_descarga_lenta_que_no_para_llega_entera() {
        // 100 bytes en 10 trozos, uno cada 300 ms: 3 s en total, con 800 ms de inactividad permitida.
        let base = servidor(vec![Box::new(|s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
            for i in 0..10u8 {
                let _ = s.write_all(&[b'0' + i; 10]);
                let _ = s.flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        })]);
        let d = carpeta_de_prueba("lenta");
        let inicio = std::time::Instant::now();
        descargar_ficheros_con(&base, &["config.json"], &d, &mut |_| {}, rapidos()).unwrap();
        assert!(inicio.elapsed() >= Duration::from_secs(2), "{:?}", inicio.elapsed());
        assert_eq!(std::fs::read(d.join("config.json")).unwrap().len(), 100);
        assert!(!d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn un_servidor_que_no_contesta_da_error_claro() {
        // Acepta la conexión y no contesta nunca.
        let base = servidor(vec![Box::new(|_| {})]);
        let d = carpeta_de_prueba("respuesta");
        let inicio = std::time::Instant::now();
        let e = descargar_ficheros_con(&base, &["config.json"], &d, &mut |_| {}, rapidos()).unwrap_err();
        assert!(inicio.elapsed() < Duration::from_secs(4), "tardó {:?}", inicio.elapsed());
        let e = format!("{e:#}");
        assert!(e.contains("127.0.0.1") && (e.contains("in time") || e.contains("a tiempo")), "{e}");
        assert!(!d.join("config.json").exists() && !d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sumas_del_modelo_ligero() {
        let d = carpeta_de_prueba("sumas");
        std::fs::create_dir_all(&d).unwrap();
        for (f, _) in SUMAS {
            std::fs::write(d.join(f), b"no es el modelo").unwrap();
        }
        let e = format!("{:#}", comprobar_sumas(&d, &SUMAS).unwrap_err());
        assert!(e.contains("SHA-256"), "{e}");
        // No queda nada de lo que no cuadró.
        assert!(SUMAS.iter().all(|(f, _)| !d.join(f).exists()));
        // Y lo que cuadra pasa: `abc` tiene un SHA-256 conocido.
        std::fs::write(d.join("abc"), b"abc").unwrap();
        comprobar_sumas(&d, &[("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")]).unwrap();
        assert!(SUMAS.iter().all(|(f, s)| FICHEROS.contains(f) && s.len() == 64));
        let _ = std::fs::remove_dir_all(&d);
    }
}
