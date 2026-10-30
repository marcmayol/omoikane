//! Omoikane se actualiza solo (Fase 7.6). La primera vez que se ejecuta `omoi` cada día lanza aparte
//! `omoi upgrade --background`, que mira la última Release y, si hay una más nueva de la misma versión
//! mayor, la baja, comprueba que está **firmada con la clave de Omoikane** (Ed25519 sobre
//! `SHA256SUMS`; la clave pública va aquí dentro) y que su suma cuadra, reemplaza el binario y
//! ejecuta el nuevo para que añada lo que haga falta a los agentes que ya tenían Omoikane. La sesión
//! siguiente lo avisa. Nunca hace esperar a quien lo lanza y sin red no pasa nada.
//!
//! No se actualiza si el usuario lo apaga (`omoi upgrade --off` o `OMOIKANE_AUTO_UPDATE=0`), ni si el
//! binario lo gestiona otro (cargo, Homebrew, Scoop, Nix, el sistema): ese lo actualiza su gestor.

use crate::tr;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const REPO: &str = "marcmayol/omoikane";

/// La clave pública con la que se firman las Releases (la privada no está en el repositorio).
const CLAVE: [u8; 32] = [
    0x37, 0x9f, 0x18, 0x1c, 0x11, 0x5e, 0x95, 0x8b, 0x78, 0x3c, 0x0c, 0x6d, 0x3c, 0x9f, 0xaa, 0x98, 0x16, 0x33, 0xab, 0xde, 0xf4, 0xd8, 0xc3,
    0x7c, 0x36, 0x1d, 0x4a, 0xb0, 0x1a, 0x6a, 0x09, 0xb9,
];

/// Cada cuánto se mira si hay versión nueva.
const CADA: u64 = 24 * 3600;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn carpeta() -> Option<PathBuf> {
    std::env::var_os("OMOIKANE_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".omoikane")))
}

fn fichero(nombre: &str) -> Option<PathBuf> {
    carpeta().map(|c| c.join(nombre))
}

fn ahora() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Lo que el usuario dijo (`omoi upgrade --on/--off`, o la pregunta de `omoi setup claude`).
pub fn preferencia() -> Option<bool> {
    let t = std::fs::read_to_string(fichero("auto-update")?).ok()?;
    match t.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

pub fn fijar(on: bool) -> Result<()> {
    let f = fichero("auto-update").context("no home folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(f, if on { "on\n" } else { "off\n" })?;
    Ok(())
}

/// Solo si el usuario dijo que sí (`omoi upgrade --on` o la pregunta de `omoi setup claude`). Sin
/// respuesta, apagada: un binario que se cambia solo es lo primero que mira quien se preocupa por la
/// seguridad, y no debe pasar sin que nadie lo haya pedido (0.19.0; antes venía encendida).
pub fn activada() -> bool {
    !std::env::var("OMOIKANE_AUTO_UPDATE").is_ok_and(|v| v.trim() == "0" || v.trim().eq_ignore_ascii_case("off")) && preferencia() == Some(true)
}

/// Si el binario lo gestiona otro programa, cuál (entonces no se toca).
pub fn gestor(exe: &Path) -> Option<&'static str> {
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
        // Compilado aquí mismo (`cargo build`, los tests): nunca se reemplaza.
        ("/target/debug/", "cargo build"),
        ("/target/release/", "cargo build"),
    ]
    .into_iter()
    .find(|(trozo, _)| p.contains(trozo))
    .map(|(_, g)| g)
}

/// `X.Y.Z` (con o sin `v`).
pub fn version(s: &str) -> Option<(u64, u64, u64)> {
    let mut v = s.trim().trim_start_matches('v').split('.').map(|x| x.parse::<u64>().ok());
    Some((v.next()??, v.next()??, v.next()??))
}

/// Si conviene pasar de `actual` a `nueva`: más alta y de la misma versión mayor (un cambio de
/// mayor puede romper cosas y se hace a mano).
pub fn conviene(actual: &str, nueva: &str) -> bool {
    match (version(actual), version(nueva)) {
        (Some(a), Some(n)) => n > a && n.0 == a.0,
        _ => false,
    }
}

/// El paquete de la Release para este equipo.
fn paquete() -> Option<&'static str> {
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

/// Comprueba la firma Ed25519 de `datos` con la clave de las Releases (o, en las pruebas, con
/// `OMOIKANE_UPDATE_KEY` en hexadecimal, que solo vale en las compilaciones de depuración).
pub fn firma_valida(datos: &[u8], firma: &[u8]) -> bool {
    let clave = clave_de_pruebas().unwrap_or(CLAVE.to_vec());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, clave).verify(datos, firma).is_ok()
}

fn clave_de_pruebas() -> Option<Vec<u8>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let h = std::env::var("OMOIKANE_UPDATE_KEY").ok()?;
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect()
}

/// De dónde se baja: GitHub, o (solo en depuración, para las pruebas) `OMOIKANE_UPDATE_URL`.
fn base() -> Option<String> {
    cfg!(debug_assertions).then(|| std::env::var("OMOIKANE_UPDATE_URL").ok()).flatten()
}

fn bajar(url: &str) -> Result<Vec<u8>> {
    let r = ureq::get(url).header("User-Agent", &format!("omoi/{VERSION}")).call().with_context(|| url.to_string())?;
    Ok(r.into_body().with_config().limit(200 * 1024 * 1024).read_to_vec()?)
}

/// La última versión publicada.
pub fn ultima() -> Result<String> {
    let url = match base() {
        Some(b) => format!("{b}/latest.json"),
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let v: serde_json::Value = serde_json::from_slice(&bajar(&url)?)?;
    v["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_string()).context("tag_name")
}

fn url_de(etiqueta: &str, nombre: &str) -> String {
    match base() {
        Some(b) => format!("{b}/download/v{etiqueta}/{nombre}"),
        None => format!("https://github.com/{REPO}/releases/download/v{etiqueta}/{nombre}"),
    }
}

/// Lo que pasó al comprobar.
#[derive(Debug, PartialEq)]
pub enum Resultado {
    AlDia(String),
    /// Hay una nueva pero no se instala (otra versión mayor, otro gestor, apagado…), y por qué.
    NoSeInstala(String, String),
    Actualizado(String),
}

/// Que `SHA256SUMS` esté firmado con la clave de Omoikane y que `datos` sea el `nombre` que dice.
pub fn verificar(sumas: &[u8], firma: &[u8], nombre: &str, datos: &[u8]) -> Result<()> {
    if !firma_valida(sumas, firma) {
        bail!(tr!("the release signature doesn't match Omoikane's key: not installed", "la firma de la Release no es de la clave de Omoikane: no se instala"));
    }
    let esperada = String::from_utf8_lossy(sumas)
        .lines()
        .find_map(|l| l.split_once(char::is_whitespace).filter(|(_, n)| n.trim().trim_start_matches('*') == nombre).map(|(h, _)| h.to_lowercase()))
        .context("SHA256SUMS")?;
    let suma: String = Sha256::digest(datos).iter().map(|b| format!("{b:02x}")).collect();
    if suma != esperada {
        bail!(tr!("the download doesn't match SHA256SUMS: not installed", "lo bajado no cuadra con SHA256SUMS: no se instala"));
    }
    Ok(())
}

/// Baja, comprueba e instala `nueva` en lugar de `exe`.
fn instalar(exe: &Path, nueva: &str) -> Result<()> {
    let nombre = paquete().context(tr!("no prebuilt binary for this system", "no hay binario para este sistema"))?;
    let sumas = bajar(&url_de(nueva, "SHA256SUMS"))?;
    let firma = bajar(&url_de(nueva, "SHA256SUMS.sig")).context(tr!("the release isn't signed", "la Release no está firmada"))?;
    let datos = bajar(&url_de(nueva, nombre))?;
    verificar(&sumas, &firma, nombre, &datos)?;
    // Se desempaqueta en una carpeta propia con el tar del sistema (en Windows, el de System32, que
    // también abre zip; el de Git que pueda haber antes en el PATH no).
    let dir = carpeta().context("home")?.join("actualizacion");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let archivo = dir.join(nombre);
    std::fs::write(&archivo, &datos)?;
    let tar = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let o = std::process::Command::new(&tar).arg("-xf").arg(&archivo).arg("-C").arg(&dir).output()?;
    if !o.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    let nuevo = dir.join(if cfg!(windows) { "omoi.exe" } else { "omoi" });
    let v = std::process::Command::new(&nuevo).arg("--version").output()?;
    if !String::from_utf8_lossy(&v.stdout).contains(nueva) {
        bail!(tr!("the new binary doesn't say it is {nueva}", "el binario nuevo no dice ser la {nueva}"));
    }
    reemplazar(exe, &nuevo)
}

/// Pone `nuevo` en el sitio de `exe`. En Windows un exe en uso no se puede sobrescribir pero sí
/// renombrar: el viejo queda como `omoi.viejo-<versión>.exe` y se borra en una ejecución posterior.
pub fn reemplazar(exe: &Path, nuevo: &Path) -> Result<()> {
    let dir = exe.parent().context("dir")?;
    if cfg!(windows) {
        let viejo = dir.join(format!("omoi.viejo-{VERSION}-{}.exe", ahora()));
        std::fs::rename(exe, &viejo).context(tr!("can't move the running binary aside", "no se puede apartar el binario en uso"))?;
        if let Err(e) = std::fs::copy(nuevo, exe) {
            let _ = std::fs::rename(&viejo, exe);
            return Err(e.into());
        }
    } else {
        let tmp = dir.join(".omoi.nuevo");
        std::fs::copy(nuevo, &tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, exe)?;
    }
    Ok(())
}

/// Borra los `omoi.viejo-*` que dejó una actualización (si ya no están en uso).
pub fn limpiar_viejos() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let Ok(lista) = std::fs::read_dir(dir) else { return };
    for e in lista.flatten() {
        if e.file_name().to_string_lossy().starts_with("omoi.viejo-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Mira y, si toca, actualiza. `exe` es el binario que se reemplaza.
pub fn comprobar(exe: &Path, instalar_si_hay: bool) -> Result<Resultado> {
    let nueva = ultima()?;
    if let Some(f) = fichero("actualizacion.json") {
        let _ = std::fs::write(f, serde_json::json!({ "checked_at": ahora(), "latest": nueva }).to_string());
    }
    if !conviene(VERSION, &nueva) {
        let otra_mayor = version(&nueva).zip(version(VERSION)).is_some_and(|(n, a)| n.0 != a.0 && n > a);
        return Ok(if otra_mayor {
            Resultado::NoSeInstala(nueva, tr!("it's a new major version: update by hand", "es otra versión mayor: se actualiza a mano"))
        } else {
            Resultado::AlDia(VERSION.into())
        });
    }
    if let Some(g) = gestor(exe) {
        return Ok(Resultado::NoSeInstala(nueva, tr!("this omoi is managed by {g}: update it there", "este omoi lo gestiona {g}: actualízalo ahí")));
    }
    if !instalar_si_hay {
        return Ok(Resultado::NoSeInstala(nueva, String::new()));
    }
    instalar(exe, &nueva)?;
    // El nuevo añade lo suyo a los agentes que ya tenían Omoikane y deja el aviso para la sesión siguiente.
    let _ = std::process::Command::new(exe).args(["upgrade", "--after", VERSION]).stdin(std::process::Stdio::null()).output();
    Ok(Resultado::Actualizado(nueva))
}

/// Al empezar cualquier orden: si toca (una vez al día, encendida, binario propio), lanza aparte la
/// comprobación. Nunca espera ni falla.
pub fn quizas_en_segundo_plano() {
    limpiar_viejos();
    if !activada() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    // En depuración, solo contra el servidor de las pruebas.
    if gestor(&exe).is_some() || paquete().is_none() || (cfg!(debug_assertions) && base().is_none()) {
        return;
    }
    let Some(f) = fichero("actualizacion.json") else { return };
    let ultima_vez = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["checked_at"].as_u64()).unwrap_or(0);
    if ahora().saturating_sub(ultima_vez) < CADA {
        return;
    }
    // Se apunta ya, para que varias órdenes a la vez no lancen varias comprobaciones.
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::write(&f, serde_json::json!({ "checked_at": ahora() }).to_string()).is_err() {
        return;
    }
    crate::sync::lanzar_aparte(&["upgrade", "--background"]);
}

/// Lo que hace el binario recién instalado (`omoi upgrade --after <anterior>`): añadir lo nuevo a los
/// agentes que ya tenían Omoikane y dejar el aviso para la sesión siguiente.
pub fn despues(anterior: &str) -> Result<Vec<String>> {
    let mut hecho = Vec::new();
    for a in crate::agentes::configurados() {
        let cambios = crate::agentes::plan(a)?;
        if !cambios.is_empty() {
            crate::agentes::aplicar(&cambios)?;
            hecho.push(a.to_string());
        }
    }
    // Los ajustes de Claude Code solo si el usuario dijo que sí (la pregunta de `omoi setup claude`).
    if preferencia() == Some(true) && crate::setup::instalado() && crate::setup::auto_update(true).unwrap_or(false) {
        hecho.push("claude".into());
    }
    if let Some(f) = fichero("actualizado.json") {
        std::fs::write(f, serde_json::json!({ "from": anterior, "to": VERSION, "agents": hecho }).to_string())?;
    }
    Ok(hecho)
}

/// El aviso de la última actualización, una sola vez (lo borra al leerlo).
pub fn aviso() -> Option<String> {
    let f = fichero("actualizado.json")?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).ok()?).ok()?;
    let _ = std::fs::remove_file(&f);
    let agentes: Vec<&str> = v["agents"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
    let mas = if agentes.is_empty() { String::new() } else { format!(" and updated its setup for {}", agentes.join(", ")) };
    Some(format!(
        "Omoikane updated itself from {} to {}{mas} (automatic daily update; `omoi upgrade --off` turns it off). Mention it to the user once.",
        v["from"].as_str().unwrap_or("?"),
        v["to"].as_str().unwrap_or("?")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versiones() {
        assert!(conviene("0.13.0", "0.14.0"));
        assert!(conviene("0.13.0", "v0.13.1"));
        assert!(!conviene("0.13.0", "0.13.0"));
        assert!(!conviene("0.14.0", "0.13.9"));
        assert!(!conviene("0.13.0", "1.0.0"), "otra versión mayor, a mano");
        assert!(!conviene("0.13.0", "basura"));
    }

    #[test]
    fn gestores() {
        assert_eq!(gestor(Path::new("C:\\Users\\a\\.cargo\\bin\\omoi.exe")), Some("cargo"));
        assert_eq!(gestor(Path::new("/opt/homebrew/bin/omoi")), Some("Homebrew"));
        assert_eq!(gestor(Path::new("/usr/bin/omoi")), Some("el sistema"));
        assert_eq!(gestor(Path::new("C:\\Users\\a\\AppData\\Local\\Programs\\omoikane\\omoi.exe")), None);
        assert_eq!(gestor(Path::new("/home/a/.local/bin/omoi")), None);
        assert_eq!(gestor(Path::new("/home/a/.omoikane/bin/omoi")), None);
        assert_eq!(gestor(Path::new("D:\\Omoikane\\target\\debug\\omoi.exe")), Some("cargo build"));
    }

    #[test]
    fn verifica_firma_y_suma() {
        use ring::signature::KeyPair;
        let par = ring::signature::Ed25519KeyPair::from_pkcs8(ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap().as_ref()).unwrap();
        let hex: String = par.public_key().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        // SAFETY (de los tests): las variables de entorno solo las toca este test.
        unsafe { std::env::set_var("OMOIKANE_UPDATE_KEY", &hex) };
        let datos = b"binario nuevo";
        let suma: String = Sha256::digest(datos).iter().map(|b| format!("{b:02x}")).collect();
        let sumas = format!("{suma}  omoi-x.zip\n0000  otro.tar.gz\n");
        let firma = par.sign(sumas.as_bytes());
        assert!(verificar(sumas.as_bytes(), firma.as_ref(), "omoi-x.zip", datos).is_ok());
        assert!(verificar(sumas.as_bytes(), firma.as_ref(), "omoi-x.zip", b"otro binario").is_err(), "suma distinta");
        assert!(verificar(sumas.replace("otro", "Otro").as_bytes(), firma.as_ref(), "omoi-x.zip", datos).is_err(), "sumas cambiadas tras firmar");
        assert!(verificar(sumas.as_bytes(), firma.as_ref(), "no-esta.zip", datos).is_err());
        unsafe { std::env::remove_var("OMOIKANE_UPDATE_KEY") };
    }

    #[test]
    fn reemplaza_y_deja_el_viejo_aparte() {
        let d = std::env::temp_dir().join(format!("omoi-reemplazar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join(if cfg!(windows) { "omoi.exe" } else { "omoi" });
        let nuevo = d.join("nuevo");
        std::fs::write(&exe, "viejo").unwrap();
        std::fs::write(&nuevo, "nuevo").unwrap();
        reemplazar(&exe, &nuevo).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "nuevo");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn firmas() {
        use ring::signature::KeyPair;
        // La clave de las Releases no acepta una firma cualquiera.
        assert!(!firma_valida(b"sumas", &[0u8; 64]));
        // Una firma de verdad con una clave cualquiera tampoco (solo vale la de Omoikane).
        let par = ring::signature::Ed25519KeyPair::from_pkcs8(ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap().as_ref()).unwrap();
        let firma = par.sign(b"sumas");
        assert!(!firma_valida(b"sumas", firma.as_ref()));
        let publica = par.public_key().as_ref().to_vec();
        assert!(ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &publica).verify(b"sumas", firma.as_ref()).is_ok());
    }
}
