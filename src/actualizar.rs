//! Omoikane updates itself (Phase 7.6). The first time `omoi` runs each day it launches separately
//! `omoi upgrade --background`, which looks at the latest Release and, if there is a newer one of the
//! same major version, downloads it, checks it is **signed with Omoikane's key** (Ed25519 over
//! `SHA256SUMS`; the public key is in here) and that its checksum matches, replaces the binary and
//! runs the new one so it adds whatever is needed to the agents that already had Omoikane. The next
//! session announces it. It never makes the caller wait, and without network nothing happens.
//!
//! It doesn't update if the user turns it off (`omoi upgrade --off` or `OMOIKANE_AUTO_UPDATE=0`), nor if
//! the binary is managed by something else (cargo, Homebrew, Scoop, Nix, the system): its manager updates it.

use crate::tr;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const REPO: &str = "marcmayol/omoikane";

/// The public key the Releases are signed with (the private one isn't in the repository).
const RELEASE_KEY: [u8; 32] = [
    0x37, 0x9f, 0x18, 0x1c, 0x11, 0x5e, 0x95, 0x8b, 0x78, 0x3c, 0x0c, 0x6d, 0x3c, 0x9f, 0xaa, 0x98, 0x16, 0x33, 0xab, 0xde, 0xf4, 0xd8, 0xc3,
    0x7c, 0x36, 0x1d, 0x4a, 0xb0, 0x1a, 0x6a, 0x09, 0xb9,
];

/// How often to check for a new version.
const CHECK_EVERY: u64 = 24 * 3600;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn home_dir() -> Option<PathBuf> {
    crate::entorno::home()
}

fn file(name: &str) -> Option<PathBuf> {
    home_dir().map(|c| c.join(name))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What the user said (`omoi upgrade --on/--off`, or the question in `omoi setup claude`).
pub fn preference() -> Option<bool> {
    let t = std::fs::read_to_string(file("auto-update")?).ok()?;
    match t.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

pub fn set_preference(on: bool) -> Result<()> {
    let f = file("auto-update").context("no home folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(f, if on { "on\n" } else { "off\n" })?;
    Ok(())
}

/// Only if the user said yes (`omoi upgrade --on` or the question in `omoi setup claude`). Without an
/// answer, off: a binary that changes itself is the first thing someone who cares about security looks
/// at, and it must not happen without anyone asking for it (0.19.0; before, it came on).
pub fn enabled() -> bool {
    !std::env::var("OMOIKANE_AUTO_UPDATE").is_ok_and(|v| v.trim() == "0" || v.trim().eq_ignore_ascii_case("off")) && preference() == Some(true)
}

/// If the binary is managed by another program, which one (then it isn't touched).
pub fn package_manager(exe: &Path) -> Option<&'static str> {
    let p = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    [
        ("/.cargo/bin/", "cargo"),
        ("/cellar/", "Homebrew"),
        ("/homebrew/", "Homebrew"),
        ("/linuxbrew/", "Homebrew"),
        ("/scoop/", "Scoop"),
        ("/nix/store/", "Nix"),
        ("/snap/", "snap"),
        ("/usr/bin/", "el sistema"),
        // Built right here (`cargo build`, the tests): never replaced.
        ("/target/debug/", "cargo build"),
        ("/target/release/", "cargo build"),
    ]
    .into_iter()
    .find(|(fragment, _)| p.contains(fragment))
    .map(|(_, g)| g)
}

/// `X.Y.Z` (with or without `v`).
pub fn version(s: &str) -> Option<(u64, u64, u64)> {
    let mut v = s.trim().trim_start_matches('v').split('.').map(|x| x.parse::<u64>().ok());
    Some((v.next()??, v.next()??, v.next()??))
}

/// Whether it is worth going from `current` to `new`: higher and of the same major version (a major
/// change may break things and is done by hand).
pub fn should_upgrade(current: &str, new: &str) -> bool {
    match (version(current), version(new)) {
        (Some(a), Some(n)) => n > a && n.0 == a.0,
        _ => false,
    }
}

/// The Release package for this machine.
fn package() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("omoi-x86_64-pc-windows-msvc.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("omoi-x86_64-unknown-linux-gnu.tar.gz")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("omoi-aarch64-unknown-linux-gnu.tar.gz")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("omoi-aarch64-apple-darwin.tar.gz")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("omoi-x86_64-apple-darwin.tar.gz")
    } else {
        None
    }
}

/// Checks the Ed25519 signature of `data` with the Releases key (or, in tests, with
/// `OMOIKANE_UPDATE_KEY` in hex, which only works in debug builds).
pub fn signature_valid(data: &[u8], signature: &[u8]) -> bool {
    let key = test_key().unwrap_or(RELEASE_KEY.to_vec());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key).verify(data, signature).is_ok()
}

fn test_key() -> Option<Vec<u8>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let h = std::env::var("OMOIKANE_UPDATE_KEY").ok()?;
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect()
}

/// Where to download from: GitHub, or (only in debug, for tests) `OMOIKANE_UPDATE_URL`.
fn base() -> Option<String> {
    cfg!(debug_assertions).then(|| std::env::var("OMOIKANE_UPDATE_URL").ok()).flatten()
}

/// Downloads all of `url` into memory, with the timeouts of `modelos::get`: a stall is a clear error
/// and leaves nothing written (what was downloaded is written later, only if signature and checksum match).
fn download(url: &str) -> Result<Vec<u8>> {
    use crate::modelos;
    let r = modelos::get(url, modelos::TIEMPOS_PEQUENO).map_err(|e| modelos::error_de_get(e, url))?;
    match r.into_body().with_config().limit(200 * 1024 * 1024).read_to_vec() {
        Ok(v) => Ok(v),
        Err(e) if modelos::es_tiempo_agotado(&e) => {
            let name = url.rsplit('/').next().unwrap_or(url);
            bail!(tr!(
                "the download of {name} stalled or took too long ({e}); nothing was installed",
                "la descarga de {name} se colgó o tardó demasiado ({e}); no se instaló nada"
            ))
        }
        Err(e) => Err(anyhow::Error::new(e).context(url.to_string())),
    }
}

/// The latest published version.
pub fn latest() -> Result<String> {
    let url = match base() {
        Some(b) => format!("{b}/latest.json"),
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let v: serde_json::Value = serde_json::from_slice(&download(&url)?)?;
    v["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_string()).context("tag_name")
}

fn url_for(tag: &str, name: &str) -> String {
    match base() {
        Some(b) => format!("{b}/download/v{tag}/{name}"),
        None => format!("https://github.com/{REPO}/releases/download/v{tag}/{name}"),
    }
}

/// What happened when checking.
#[derive(Debug, PartialEq)]
pub enum CheckResult {
    UpToDate(String),
    /// There is a new one but it isn't installed (another major version, another manager, turned off…), and why.
    NotInstalled(String, String),
    Updated(String),
}

/// That `SHA256SUMS` is signed with Omoikane's key and that `data` is the `name` it says.
pub fn verify(sums: &[u8], signature: &[u8], name: &str, data: &[u8]) -> Result<()> {
    if !signature_valid(sums, signature) {
        bail!(tr!("the release signature doesn't match Omoikane's key: not installed", "la firma de la Release no es de la clave de Omoikane: no se instala"));
    }
    let expected = String::from_utf8_lossy(sums)
        .lines()
        .find_map(|l| l.split_once(char::is_whitespace).filter(|(_, n)| n.trim().trim_start_matches('*') == name).map(|(h, _)| h.to_lowercase()))
        .context("SHA256SUMS")?;
    let sum: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    if sum != expected {
        bail!(tr!("the download doesn't match SHA256SUMS: not installed", "lo bajado no cuadra con SHA256SUMS: no se instala"));
    }
    Ok(())
}

/// Downloads, checks and installs `new` in place of `exe`.
fn install(exe: &Path, new: &str) -> Result<()> {
    let new_bin = download_version(new, &home_dir().context("home")?.join("actualizacion"))?;
    replace(exe, &new_bin)
}

/// Downloads the `omoi` of Release `new` for this system into `dir` (emptying it first), checks the
/// signature of `SHA256SUMS` and the package checksum, unpacks it and returns the path of the binary,
/// which says it is that version. Used by the update and by the performance guard (`examples/rendimiento.rs`).
pub fn download_version(new: &str, dir: &Path) -> Result<PathBuf> {
    let name = package().context(tr!("no prebuilt binary for this system", "no hay binario para este sistema"))?;
    let sums = download(&url_for(new, "SHA256SUMS"))?;
    let signature = download(&url_for(new, "SHA256SUMS.sig")).context(tr!("the release isn't signed", "la Release no está firmada"))?;
    let data = download(&url_for(new, name))?;
    verify(&sums, &signature, name, &data)?;
    // It's unpacked in its own folder with the system's tar (on Windows, the one in System32, which
    // also opens zip; not Git's, which may come earlier in the PATH).
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let archive = dir.join(name);
    std::fs::write(&archive, &data)?;
    let tar = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let o = std::process::Command::new(&tar).arg("-xf").arg(&archive).arg("-C").arg(dir).output()?;
    if !o.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let new_bin = dir.join(if cfg!(windows) { "omoi.exe" } else { "omoi" });
    let v = std::process::Command::new(&new_bin).arg("--version").output()?;
    if !String::from_utf8_lossy(&v.stdout).contains(new) {
        bail!(tr!("the new binary doesn't say it is {new}", "el binario nuevo no dice ser la {new}"));
    }
    Ok(new_bin)
}

/// Puts `new` in place of `exe`. On Windows an exe in use can't be overwritten but can be
/// renamed: the old one stays as `omoi.viejo-<version>.exe` and is deleted in a later run.
pub fn replace(exe: &Path, new: &Path) -> Result<()> {
    let dir = exe.parent().context("dir")?;
    if cfg!(windows) {
        let old = dir.join(format!("omoi.viejo-{VERSION}-{}.exe", now()));
        std::fs::rename(exe, &old).context(tr!("can't move the running binary aside", "no se puede apartar el binario en uso"))?;
        if let Err(e) = std::fs::copy(new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e.into());
        }
    } else {
        let tmp = dir.join(".omoi.nuevo");
        std::fs::copy(new, &tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, exe)?;
    }
    Ok(())
}

/// Deletes the `omoi.viejo-*` an update left behind (if no longer in use).
pub fn clean_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("omoi.viejo-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Checks and, if due, updates. `exe` is the binary being replaced.
pub fn check(exe: &Path, install_if_any: bool) -> Result<CheckResult> {
    let new = latest()?;
    if let Some(f) = file("actualizacion.json") {
        let _ = std::fs::write(f, serde_json::json!({ "checked_at": now(), "latest": new }).to_string());
    }
    if !should_upgrade(VERSION, &new) {
        let other_major = version(&new).zip(version(VERSION)).is_some_and(|(n, a)| n.0 != a.0 && n > a);
        return Ok(if other_major {
            CheckResult::NotInstalled(new, tr!("it's a new major version: update by hand", "es otra versión mayor: se actualiza a mano"))
        } else {
            CheckResult::UpToDate(VERSION.into())
        });
    }
    if let Some(g) = package_manager(exe) {
        return Ok(CheckResult::NotInstalled(new, tr!("this omoi is managed by {g}: update it there", "este omoi lo gestiona {g}: actualízalo ahí")));
    }
    if !install_if_any {
        return Ok(CheckResult::NotInstalled(new, String::new()));
    }
    install(exe, &new)?;
    // The new one adds its part to the agents that already had Omoikane and leaves the notice for the next session.
    let _ = std::process::Command::new(exe).args(["upgrade", "--after", VERSION]).stdin(std::process::Stdio::null()).output();
    Ok(CheckResult::Updated(new))
}

/// At the start of any command: if due (once a day, turned on, own binary), launches the check
/// separately. Never waits or fails.
pub fn maybe_in_background() {
    clean_old();
    // With nobody in front (a CI, a server), the binary doesn't change itself even if it was turned on.
    if !enabled() || crate::entorno::unattended() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    // In debug, only against the test server.
    if package_manager(&exe).is_some() || package().is_none() || (cfg!(debug_assertions) && base().is_none()) {
        return;
    }
    let Some(f) = file("actualizacion.json") else { return };
    let last_time = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["checked_at"].as_u64()).unwrap_or(0);
    if now().saturating_sub(last_time) < CHECK_EVERY {
        return;
    }
    // Recorded right away, so several commands at once don't launch several checks.
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::write(&f, serde_json::json!({ "checked_at": now() }).to_string()).is_err() {
        return;
    }
    crate::sync::spawn_detached(&["upgrade", "--background"]);
}

/// What the freshly installed binary does (`omoi upgrade --after <previous>`): add what's new to the
/// agents that already had Omoikane and leave the notice for the next session.
pub fn after_upgrade(previous: &str) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for a in crate::agentes::configured() {
        let changes = crate::agentes::plan(a)?;
        if !changes.is_empty() {
            crate::agentes::apply(&changes)?;
            done.push(a.to_string());
        }
    }
    // Claude Code's settings only if the user said yes (the question in `omoi setup claude`).
    if preference() == Some(true) && crate::setup::installed() && crate::setup::auto_update(true).unwrap_or(false) {
        done.push("claude".into());
    }
    if let Some(f) = file("actualizado.json") {
        std::fs::write(f, serde_json::json!({ "from": previous, "to": VERSION, "agents": done }).to_string())?;
    }
    Ok(done)
}

/// The notice of the last update, only once (deletes it on reading).
pub fn notice() -> Option<String> {
    let f = file("actualizado.json")?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).ok()?).ok()?;
    let _ = std::fs::remove_file(&f);
    let agents: Vec<&str> = v["agents"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
    let more = if agents.is_empty() { String::new() } else { format!(" and updated its setup for {}", agents.join(", ")) };
    Some(format!(
        "Omoikane updated itself from {} to {}{more} (automatic daily update; `omoi upgrade --off` turns it off). Mention it to the user once.",
        v["from"].as_str().unwrap_or("?"),
        v["to"].as_str().unwrap_or("?")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(should_upgrade("0.13.0", "0.14.0"));
        assert!(should_upgrade("0.13.0", "v0.13.1"));
        assert!(!should_upgrade("0.13.0", "0.13.0"));
        assert!(!should_upgrade("0.14.0", "0.13.9"));
        assert!(!should_upgrade("0.13.0", "1.0.0"), "otra versión mayor, a mano");
        assert!(!should_upgrade("0.13.0", "basura"));
    }

    #[test]
    fn managers() {
        assert_eq!(package_manager(Path::new("C:\\Users\\a\\.cargo\\bin\\omoi.exe")), Some("cargo"));
        assert_eq!(package_manager(Path::new("/opt/homebrew/bin/omoi")), Some("Homebrew"));
        assert_eq!(package_manager(Path::new("/usr/bin/omoi")), Some("el sistema"));
        assert_eq!(package_manager(Path::new("C:\\Users\\a\\AppData\\Local\\Programs\\omoikane\\omoi.exe")), None);
        assert_eq!(package_manager(Path::new("/home/a/.local/bin/omoi")), None);
        assert_eq!(package_manager(Path::new("/home/a/.omoikane/bin/omoi")), None);
        assert_eq!(package_manager(Path::new("D:\\Omoikane\\target\\debug\\omoi.exe")), Some("cargo build"));
    }

    // The test key (`OMOIKANE_UPDATE_KEY`) only works in debug: in release only the Releases one
    // works, which `signatures` already tests.
    #[cfg(debug_assertions)]
    #[test]
    fn checks_signature_and_checksum() {
        use ring::signature::KeyPair;
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap().as_ref()).unwrap();
        let hex: String = pair.public_key().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        // SAFETY (tests): only this test touches the environment variables.
        unsafe { std::env::set_var("OMOIKANE_UPDATE_KEY", &hex) };
        let data = b"binario nuevo";
        let sum: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        let sums = format!("{sum}  omoi-x.zip\n0000  otro.tar.gz\n");
        let signature = pair.sign(sums.as_bytes());
        assert!(verify(sums.as_bytes(), signature.as_ref(), "omoi-x.zip", data).is_ok());
        assert!(verify(sums.as_bytes(), signature.as_ref(), "omoi-x.zip", b"otro binario").is_err(), "suma distinta");
        assert!(verify(sums.replace("otro", "Otro").as_bytes(), signature.as_ref(), "omoi-x.zip", data).is_err(), "sumas cambiadas tras firmar");
        assert!(verify(sums.as_bytes(), signature.as_ref(), "no-esta.zip", data).is_err());
        unsafe { std::env::remove_var("OMOIKANE_UPDATE_KEY") };
    }

    #[test]
    fn replaces_and_keeps_the_old_one_aside() {
        let d = crate::comun::Temporal::nueva("reemplazar");
        let exe = d.join(if cfg!(windows) { "omoi.exe" } else { "omoi" });
        let new = d.join("nuevo");
        std::fs::write(&exe, "viejo").unwrap();
        std::fs::write(&new, "nuevo").unwrap();
        replace(&exe, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "nuevo");
    }

    #[test]
    fn signatures() {
        use ring::signature::KeyPair;
        // The Releases key doesn't accept just any signature.
        assert!(!signature_valid(b"sumas", &[0u8; 64]));
        // Nor a real signature with any other key (only Omoikane's works).
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap().as_ref()).unwrap();
        let signature = pair.sign(b"sumas");
        assert!(!signature_valid(b"sumas", signature.as_ref()));
        let public = pair.public_key().as_ref().to_vec();
        assert!(ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public).verify(b"sumas", signature.as_ref()).is_ok());
    }
}
