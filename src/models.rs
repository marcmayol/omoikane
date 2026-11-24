//! Which model is used and how it gets to disk.
//!
//! Each model lives in its own folder, which is never touched after it is created:
//! `~/.omoikane/models/<name>-<hash>/`. The file `actual` says which one is used. Windows doesn't let
//! you overwrite or delete a mapped file while some process uses it (and on Linux and macOS
//! overwriting it in place would silently corrupt whoever has it open), so installing a new one is:
//! compile into a separate folder, rename it and switch `actual` atomically.

use crate::embed::{Model, compile};
use crate::tr;
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const NAME: &str = "potion-multilingual-128M-int8";
/// Where the int8 version of potion-multilingual-128M (The Minish Lab) is downloaded from, in order:
/// Hugging Face and, if that fails, Omoikane's GitHub Release with the same files.
/// `OMOIKANE_MODEL_SOURCE` replaces them (one or several separated by commas: your own mirror, or
/// to try a specific one).
pub const SOURCES: [&str; 2] = [
    "https://huggingface.co/natzx94/potion-multilingual-128M-int8/resolve/main",
    "https://github.com/marcmayol/omoikane/releases/download/modelo-potion-int8-v1",
];
const FILES: [&str; 3] = ["model.safetensors", "tokenizer.json", "config.json"];
/// SHA-256 of each published file of the light model: what is downloaded must be exactly this,
/// whatever mirror it comes from (like `meaning::CHECKSUMS`). Checked on 8-Nov-2026 by downloading the
/// Hugging Face ones, against the `lfs.oid` of its API and the `digest` of the GitHub Release.
const CHECKSUMS: [(&str, &str); 3] = [
    ("model.safetensors", "8cadee28bbc4c52ec36c33af1e6ee8ffcb0e40beb195d29d1b8304d74872df96"),
    ("tokenizer.json", "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1"),
    ("config.json", "595e4cab2093732efd5dbe084fd5c1826b5eea693b73b4c1fd971672867d2e54"),
];
const CURRENT_FILE: &str = "actual";

/// `OMOIKANE_MODELS`, or `models` in Omoikane's folder (`OMOIKANE_HOME` or `~/.omoikane`).
pub fn dir() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_MODELS").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    crate::environment::home().unwrap_or_else(|| PathBuf::from(".omoikane")).join("models")
}

/// Folder of the model in use, if one is installed and complete.
pub fn current() -> Option<PathBuf> {
    let base = dir();
    let name = std::fs::read_to_string(base.join(CURRENT_FILE)).ok()?;
    let dir = base.join(name.trim());
    dir.join("meta.json").exists().then_some(dir)
}

/// The model in use, opened. `None` if there is none: everything works the same, without search by
/// meaning.
pub fn open_current() -> Result<Option<Model>> {
    current().map(|d| Model::open(&d)).transpose()
}

/// 64-bit FNV-1a: to name the folder by its content, not for security.
fn fingerprint(files: &[PathBuf]) -> Result<String> {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut buf = vec![0u8; 1 << 20];
    for f in files {
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

fn sources() -> Vec<String> {
    match std::env::var("OMOIKANE_MODEL_SOURCE") {
        Ok(o) if !o.trim().is_empty() => {
            o.split(',').map(|x| x.trim().trim_end_matches('/').to_string()).filter(|x| !x.is_empty()).collect()
        }
        _ => SOURCES.iter().map(|o| o.to_string()).collect(),
    }
}

/// Downloads the int8 model's files to `dest`, trying the sources in order.
fn download(dest: &Path, notify: &mut dyn FnMut(&str)) -> Result<()> {
    let mut failures = Vec::new();
    for source in sources() {
        match download_from(&source, dest, notify) {
            Ok(()) => return Ok(()),
            Err(e) => {
                notify(&tr!("failed: {e:#}", "falló: {e:#}"));
                failures.push(format!("{source}: {e:#}"));
            }
        }
    }
    bail!(tr!("couldn't download the model from any source:
{}", "no se pudo descargar el modelo de ninguna fuente:
{}", failures.join("
")))
}

/// `notify` gets one line per file. What is downloaded must match `CHECKSUMS`.
fn download_from(source: &str, dest: &Path, notify: &mut dyn FnMut(&str)) -> Result<()> {
    download_files(source, &FILES, dest, notify)?;
    check_sums(dest, &CHECKSUMS)
}

/// That each file in `dest` has its SHA-256. If one doesn't match, all are deleted: whatever comes
/// from the next mirror isn't mixed with this.
pub(crate) fn check_sums(dest: &Path, sums: &[(&str, &str)]) -> Result<()> {
    for (f, expected) in sums {
        let s = sha256_of(&dest.join(f))?;
        if s != *expected {
            for (f, _) in sums {
                let _ = std::fs::remove_file(dest.join(f));
            }
            bail!(tr!("{f}: wrong SHA-256 ({s})", "{f}: SHA-256 equivocado ({s})"));
        }
    }
    Ok(())
}

/// A file's SHA-256, in hexadecimal.
pub(crate) fn sha256_of(f: &Path) -> Result<String> {
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

/// The HTTP client for every download (the model, the updates): with the system's root certificates,
/// and not only the Mozilla ones ureq ships. On a company network that inspects HTTPS, its root
/// certificate is in the system store (that's why Git and PowerShell connect) and not in Mozilla's
/// list. Updates are still checked by their Ed25519 signature.
fn client(system: bool, t: Timeouts) -> ureq::Agent {
    let mut c = ureq::Agent::config_builder()
        .timeout_resolve(Some(t.connect))
        .timeout_connect(Some(t.connect))
        .timeout_send_request(Some(t.response))
        .timeout_recv_response(Some(t.response))
        .timeout_recv_body(Some(t.body));
    if system {
        c = c.tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build());
    }
    c.build().new_agent()
}

/// A download's maximum times. Without them, a network that hangs halfway (a proxy that doesn't
/// answer, a WiFi that drops without closing the connection) left `omoi model install` or the
/// background update waiting forever.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// To connect: DNS, TCP and TLS, each.
    pub connect: Duration,
    /// To send the request and get the response header.
    pub response: Duration,
    /// For the whole body (ureq counts it from start to end, not per read).
    pub body: Duration,
    /// The longest that can pass without any body byte arriving (`download_files`): a hang
    /// halfway is cut here, not by the total.
    pub idle: Duration,
}

/// The models' ones. A whole file can take long on a slow connection (the meaning one is hundreds of
/// MB: at 100 KB/s, over an hour; with the previous 60-minute total limit it never finished), so what
/// cuts a hang is inactivity: 120 s without receiving anything. The total (24 h) is only there so the
/// reading thread doesn't stay forever if nobody is waiting for it anymore.
pub(crate) const MODEL_TIMEOUTS: Timeouts =
    Timeouts { connect: Duration::from_secs(30), response: Duration::from_secs(60), body: Duration::from_secs(24 * 60 * 60), idle: Duration::from_secs(120) };
/// The updates' ones (the binary, `SHA256SUMS`, the latest version): a few MB, the total is enough.
pub(crate) const SMALL_TIMEOUTS: Timeouts = Timeouts { body: Duration::from_secs(10 * 60), ..MODEL_TIMEOUTS };

/// Copies `reader` into `out` and fails (`TimedOut`) if `idle` passes without any byte arriving. ureq
/// has no maximum time per read (the body one is for the whole body), so a separate thread reads and
/// here each chunk is awaited with its timeout. If it's cut for inactivity, that thread keeps waiting
/// until ureq cuts it by the total, holding nothing of the caller's open: this one does the writing.
fn copy_with_idle_timeout(reader: impl std::io::Read + Send + 'static, out: &mut impl Write, idle: Duration) -> std::io::Result<u64> {
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};
    let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(8);
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut b = vec![0u8; 64 * 1024];
        loop {
            let r = match reader.read(&mut b) {
                Ok(n) => Ok(b[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            };
            let end = !matches!(&r, Ok(v) if !v.is_empty());
            if tx.send(r).is_err() || end {
                return;
            }
        }
    });
    let mut total = 0u64;
    loop {
        match rx.recv_timeout(idle) {
            Ok(Ok(v)) if v.is_empty() => return Ok(total),
            Ok(Ok(v)) => {
                out.write_all(&v)?;
                total += v.len() as u64;
            }
            Ok(Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("no data for {} s", idle.as_secs_f32())));
            }
            Err(RecvTimeoutError::Disconnected) => return Err(std::io::Error::other("the download reader ended")),
        }
    }
}

/// GET with the system's certificates and, if it fails without the server ever answering, again
/// with Mozilla's list: a minimal Linux without `ca-certificates` has no system store. A timeout
/// isn't retried (it would mean waiting as long again): it gives its error.
pub fn get(url: &str, t: Timeouts) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let ua = format!("omoi/{}", env!("CARGO_PKG_VERSION"));
    match client(true, t).get(url).header("User-Agent", &ua).call() {
        Err(e) if !matches!(e, ureq::Error::StatusCode(_)) && !is_timeout(&e) => client(false, t).get(url).header("User-Agent", &ua).call(),
        r => r,
    }
}

/// Whether an I/O error from a download is a timeout (ureq wraps it in `io::Error`).
pub(crate) fn is_stall(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut || e.get_ref().and_then(|x| x.downcast_ref::<ureq::Error>()).is_some_and(|x| matches!(x, ureq::Error::Timeout(_)))
}

/// The same with a ureq error.
pub(crate) fn is_timeout(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => is_stall(io),
        _ => false,
    }
}

/// The error from `get`, made clear if it was a timeout.
pub(crate) fn get_error(e: ureq::Error, url: &str) -> anyhow::Error {
    let host = url.split('/').nth(2).unwrap_or(url);
    match e {
        e if is_timeout(&e) => anyhow::anyhow!(tr!(
            "{host} didn't answer in time ({e}): check the network or the proxy and try again",
            "{host} no contestó a tiempo ({e}): mira la red o el proxy y vuelve a probar"
        )),
        e => anyhow::Error::new(e).context(tr!("can't download {url}", "no se puede descargar {url}")),
    }
}

/// Downloads `files` from `source` to `dest` (each one to `.part` and renamed once it's complete; if
/// it fails or hangs, the `.part` is deleted and nothing half-done is left with the good name).
pub(crate) fn download_files(source: &str, files: &[&str], dest: &Path, notify: &mut dyn FnMut(&str)) -> Result<()> {
    download_files_with(source, files, dest, notify, MODEL_TIMEOUTS)
}

/// `download_files` with other maximum times (for the tests).
fn download_files_with(source: &str, files: &[&str], dest: &Path, notify: &mut dyn FnMut(&str), times: Timeouts) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let host = source.split('/').nth(2).unwrap_or(source);
    for &f in files {
        let url = format!("{source}/{f}");
        let resp = get(&url, times).map_err(|e| get_error(e, &url))?;
        let total: Option<u64> =
            resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        notify(&match total {
            Some(t) => tr!("downloading {f} ({:.1} MB) from {host}", "descargando {f} ({:.1} MB) de {host}", t as f64 / 1e6),
            None => tr!("downloading {f} from {host}", "descargando {f} de {host}"),
        });
        let part = dest.join(format!("{f}.part"));
        let r = (|| -> Result<()> {
            let mut out = std::fs::File::create(&part)?;
            let copied = copy_with_idle_timeout(resp.into_body().into_reader(), &mut out, times.idle).map_err(|e| {
                if is_stall(&e) {
                    anyhow::anyhow!(tr!(
                        "the download of {f} from {host} stalled or took too long ({e}); nothing was left half-written",
                        "la descarga de {f} de {host} se colgó o tardó demasiado ({e}); no queda nada a medias"
                    ))
                } else {
                    anyhow::Error::new(e).context(tr!("the download of {f} was cut", "la descarga de {f} se cortó"))
                }
            })?;
            out.flush()?;
            drop(out);
            if let Some(t) = total
                && copied != t
            {
                bail!(tr!("{f} arrived incomplete ({copied} of {t} bytes)", "{f} llegó incompleto ({copied} de {t} bytes)"));
            }
            std::fs::rename(&part, dest.join(f))?;
            Ok(())
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        r?;
    }
    Ok(())
}

/// Installs the model and makes it `actual`. Without `from`, downloads it from Hugging Face; with
/// `from`, takes it from a folder with the int8 (no network). Returns the folder's name.
pub fn install(from: Option<&Path>, notify: &mut dyn FnMut(&str)) -> Result<String> {
    let base = dir();
    std::fs::create_dir_all(&base).with_context(|| tr!("can't create {}", "no se puede crear {}", base.display()))?;
    let staging = base.join(format!(".instalando-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| -> Result<String> {
        let source = match from {
            Some(d) => d.to_path_buf(),
            None => {
                let d = staging.join("int8");
                download(&d, notify)?;
                d
            }
        };
        let id = format!("{NAME}-{}", fingerprint(&FILES.map(|f| source.join(f)))?);
        let dest = base.join(&id);
        if !dest.join("meta.json").exists() {
            notify(&tr!("compiling the model (a few seconds, only once)", "compilando el modelo (unos segundos, solo una vez)"));
            let compiled = staging.join("compilado");
            compile(&source, &compiled)?;
            // If another process installed it at the same time, theirs is just as good: same content, same name.
            if std::fs::rename(&compiled, &dest).is_err() && !dest.join("meta.json").exists() {
                bail!(tr!("can't move the model into {}", "no se puede mover el modelo a {}", dest.display()));
            }
        }
        // `actual` changes in one go: whoever reads it sees the old one or the new one, never a half-done one.
        let tmp = base.join(format!("{CURRENT_FILE}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, &id)?;
        std::fs::rename(&tmp, base.join(CURRENT_FILE))?;
        Ok(id)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Installed models: (name, is it the current one?).
pub fn installed() -> Vec<(String, bool)> {
    let current = current().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    let mut v: Vec<(String, bool)> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("meta.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .map(|n| {
            let is_current = current.as_deref() == Some(n.as_str());
            (n, is_current)
        })
        .collect();
    v.sort();
    v
}

/// Deletes the models that aren't the current one and the leftovers of interrupted installs. Whatever
/// is still open by another session (Windows doesn't let you delete it) stays for next time.
pub fn clean() -> Vec<String> {
    let current = current().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    let mut deleted = Vec::new();
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !e.path().is_dir() || Some(&name) == current.as_ref() {
            continue;
        }
        // It may be another process's install in progress: left alone (each install deletes its own
        // when it finishes, also if it fails).
        if name.starts_with(".instalando-") {
            continue;
        }
        if std::fs::remove_dir_all(e.path()).is_ok() {
            deleted.push(name);
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// What the test server answers on a connection.
    type Reply = Box<dyn FnOnce(&mut std::net::TcpStream) + Send>;

    /// An HTTP server on 127.0.0.1 that serves one connection per function in `replies`, answers
    /// whatever that function writes and then goes quiet. Returns the base URL.
    fn server(replies: Vec<Reply>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for respond in replies {
                let Ok((mut s, _)) = l.accept() else { return };
                // The whole request (without body) before answering.
                let mut read = Vec::new();
                let mut b = [0u8; 1024];
                while !read.windows(4).any(|w| w == b"\r\n\r\n") {
                    match std::io::Read::read(&mut s, &mut b) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => read.extend_from_slice(&b[..n]),
                    }
                }
                respond(&mut s);
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        base
    }

    /// The body total, more than enough: what cuts a hang is inactivity.
    fn fast() -> Timeouts {
        Timeouts { connect: Duration::from_secs(2), response: Duration::from_millis(800), body: Duration::from_secs(60), idle: Duration::from_millis(800) }
    }

    fn test_folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omoi-modelos-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_stalled_body_errors_and_leaves_nothing() {
        // Says 1000 bytes, sends 10 and goes quiet.
        let base = server(vec![Box::new(|s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n0123456789");
            let _ = s.flush();
        })]);
        let d = test_folder("cuerpo");
        let start = std::time::Instant::now();
        let e = download_files_with(&base, &["config.json"], &d, &mut |_| {}, fast()).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(4), "tardó {:?}", start.elapsed());
        let e = format!("{e:#}");
        assert!(e.contains("config.json") && (e.contains("stalled") || e.contains("colgó")), "{e}");
        assert!(!d.join("config.json").exists() && !d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 1.8.6: the body limit was for the whole file (60 minutes), and the meaning model at about
    /// 100 KB/s never finished. Now inactivity counts: a slow download that doesn't stop arrives
    /// complete even if it takes much longer than the allowed inactivity.
    #[test]
    fn a_slow_download_that_keeps_going_arrives_complete() {
        // 100 bytes in 10 chunks, one every 300 ms: 3 s in total, with 800 ms of allowed inactivity.
        let base = server(vec![Box::new(|s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n");
            for i in 0..10u8 {
                let _ = s.write_all(&[b'0' + i; 10]);
                let _ = s.flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        })]);
        let d = test_folder("lenta");
        let start = std::time::Instant::now();
        download_files_with(&base, &["config.json"], &d, &mut |_| {}, fast()).unwrap();
        assert!(start.elapsed() >= Duration::from_secs(2), "{:?}", start.elapsed());
        assert_eq!(std::fs::read(d.join("config.json")).unwrap().len(), 100);
        assert!(!d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_server_that_never_answers_gives_a_clear_error() {
        // Accepts the connection and never answers.
        let base = server(vec![Box::new(|_| {})]);
        let d = test_folder("respuesta");
        let start = std::time::Instant::now();
        let e = download_files_with(&base, &["config.json"], &d, &mut |_| {}, fast()).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(4), "tardó {:?}", start.elapsed());
        let e = format!("{e:#}");
        assert!(e.contains("127.0.0.1") && (e.contains("in time") || e.contains("a tiempo")), "{e}");
        assert!(!d.join("config.json").exists() && !d.join("config.json.part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn light_model_checksums() {
        let d = test_folder("sumas");
        std::fs::create_dir_all(&d).unwrap();
        for (f, _) in CHECKSUMS {
            std::fs::write(d.join(f), b"no es el modelo").unwrap();
        }
        let e = format!("{:#}", check_sums(&d, &CHECKSUMS).unwrap_err());
        assert!(e.contains("SHA-256"), "{e}");
        // Nothing is left of what didn't match.
        assert!(CHECKSUMS.iter().all(|(f, _)| !d.join(f).exists()));
        // And what matches passes: `abc` has a known SHA-256.
        std::fs::write(d.join("abc"), b"abc").unwrap();
        check_sums(&d, &[("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")]).unwrap();
        assert!(CHECKSUMS.iter().all(|(f, s)| FILES.contains(f) && s.len() == 64));
        let _ = std::fs::remove_dir_all(&d);
    }
}
