//! Agents with no person in front (CI, servers, containers, cloud agents), 1.7.0: `omoi` without
//! a home folder, without questions and configured only through environment variables.
//!
//! - `OMOIKANE_HOME`: the whole Omoikane folder (the database, the models, the meaning model
//!   preference and its service, the updates). `~/.omoikane` by default. `OMOIKANE_DB` and
//!   `OMOIKANE_MODELS` still take precedence for their own part. The sync files (configuration,
//!   keys, clones), the backups and the journal live next to the database, so they go with it.
//! - With no home folder (a container with no `HOME` nor entry in `/etc/passwd`) and with no
//!   `OMOIKANE_HOME` nor `OMOIKANE_DB`, `omoi` refuses with a clear error instead of creating the database in the
//!   folder it runs in (it could end up inside the repository being tested).
//! - `OMOIKANE_NONINTERACTIVE=1`: nobody in front. No questions (whatever needs confirmation asks for
//!   `--yes`), no download nobody asked for (the meaning model), no
//!   automatic update, no meaning service that starts by itself (1.7.1; if there's already
//!   one running, it is used) and no background sync that outlives the process (except with
//!   `OMOIKANE_SYNC_AUTO=1`). Without it, questions are already skipped if the input isn't a terminal; the
//!   rest isn't, because agents' hooks don't have a terminal either and there they are wanted.
//! - `OMOIKANE_AUTHOR`: who signs what gets saved (the author of each memory and each verdict),
//!   over the one chosen when setting up sync.
//! - `OMOIKANE_SYNC_CODE` (one or several codes from `omoi sync code`) and `OMOIKANE_SYNC_URL` (another URL
//!   for the same repository, e.g. with a CI token): `omoi sync`, `omoi sync --pull` and
//!   `omoi sync join` with no code join those repositories by themselves. The code carries the key: it is never
//!   written to the output or to an error.

use std::path::PathBuf;

use crate::tr;

/// A variable's value, with no surrounding spaces; `None` if it isn't set or ends up empty.
fn value(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// `1`, `true`, `yes` or `on` (case-insensitive).
fn is_on(name: &str) -> bool {
    value(name).is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// The Omoikane folder: `OMOIKANE_HOME` or `~/.omoikane`. `None` if there is neither (no
/// home folder, or an empty or relative `HOME`).
///
/// `OMOIKANE_HOME` must be absolute: a relative one would depend on the folder each
/// command runs in (each hook, each CI step would have its own database). `~` and `~/…` are expanded with the home
/// folder; any other relative one is not valid (`None`, and `bad_home` explains it).
pub fn home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("OMOIKANE_HOME").filter(|h| !h.is_empty()) {
        return home_from(&h.to_string_lossy());
    }
    crate::project::local_home().map(|h| h.join(".omoikane"))
}

/// `OMOIKANE_HOME` as a path: absolute as is, `~` or `~/…` from the home folder; anything else, `None`.
/// Nor a network one (`\\servidor\recurso`, `//host/x`, `~/\\host\x`…): every command would open the database
/// and the sync files there (see `project::is_local_path`).
fn home_from(h: &str) -> Option<PathBuf> {
    let user_home = crate::project::local_home;
    if h == "~" {
        return user_home();
    }
    let p = if let Some(rest) = h.strip_prefix("~/").or_else(|| h.strip_prefix("~\\")) {
        user_home()?.join(rest)
    } else {
        PathBuf::from(h)
    };
    (p.is_absolute() && crate::project::is_local_path(&p)).then_some(p)
}

/// The error for an `OMOIKANE_HOME` that isn't valid (relative, or with `~` and no home folder); `None` if
/// it isn't set or is valid.
pub fn bad_home() -> Option<String> {
    let h = std::env::var_os("OMOIKANE_HOME").filter(|h| !h.is_empty())?;
    let h = h.to_string_lossy();
    home_from(&h).is_none().then(|| {
        let after_tilde = h.strip_prefix("~/").or_else(|| h.strip_prefix("~\\")).unwrap_or(&h);
        let local = |r: &str| crate::project::is_local_path(std::path::Path::new(r));
        if !local(&h) || !local(after_tilde) {
            tr!(
                "OMOIKANE_HOME must be a folder on this computer (it is «{h}»): Omoikane doesn't open network paths (\\\\server\\share, //host/x, \\\\?\\UNC\\…); map the share to a drive letter if you need it",
                "OMOIKANE_HOME tiene que ser una carpeta de este equipo (es «{h}»): Omoikane no abre rutas de red (\\\\servidor\\recurso, //host/x, \\\\?\\UNC\\…); si la necesitas, asígnale una letra de unidad"
            )
        } else {
            tr!(
                "OMOIKANE_HOME must be an absolute path (it is «{h}»): a relative one would change with the folder each command runs in",
                "OMOIKANE_HOME tiene que ser una ruta absoluta (es «{h}»): una relativa cambiaría con la carpeta en la que se ejecuta cada orden"
            )
        }
    })
}

/// The error for a database (`--db`, `OMOIKANE_DB`) or a models folder (`OMOIKANE_MODELS`) that isn't
/// on this computer; `None` if it's valid. Checked before opening anything.
pub fn bad_path(what: &str, p: &std::path::Path) -> Option<String> {
    (!crate::project::is_local_path(p)).then(|| {
        tr!(
            "{what} must be on this computer (it is «{}»): Omoikane doesn't open network paths (\\\\server\\share, //host/x, \\\\?\\UNC\\…); map the share to a drive letter if you need it",
            "{what} tiene que estar en este equipo (es «{}»): Omoikane no abre rutas de red (\\\\servidor\\recurso, //host/x, \\\\?\\UNC\\…); si la necesitas, asígnale una letra de unidad",
            p.display()
        )
    })
}

/// Whether there is somewhere to keep the database: `OMOIKANE_DB` or an Omoikane folder.
pub fn has_place_to_save() -> bool {
    std::env::var_os("OMOIKANE_DB").is_some_and(|d| !d.is_empty()) || home().is_some()
}

/// The error for when there is no home folder nor variables saying where to save.
pub fn no_home_error() -> String {
    tr!(
        "there is no home folder: set OMOIKANE_HOME to the folder Omoikane should use (or OMOIKANE_DB, or pass --db)",
        "no hay carpeta personal: pon en OMOIKANE_HOME la carpeta que debe usar Omoikane (u OMOIKANE_DB, o pasa --db)"
    )
}

/// `OMOIKANE_NONINTERACTIVE=1`: there is nobody in front.
pub fn unattended() -> bool {
    is_on("OMOIKANE_NONINTERACTIVE")
}

/// The variable `omoi` uses to mark the processes it launches itself in the background (the
/// background sync, the hooks' `--pull`): it's not for anyone outside.
pub const BACKGROUND_VAR: &str = "OMOIKANE_INTERNAL_BACKGROUND";

/// Whether this process was launched by `omoi` in the background (`BACKGROUND_VAR`): even if nobody is in front, nobody
/// is waiting for its result.
pub fn in_background() -> bool {
    is_on(BACKGROUND_VAR)
}

/// Whether we can ask: there is a terminal on the input and nobody said there's no one there.
pub fn can_ask() -> bool {
    use std::io::IsTerminal;
    !unattended() && std::io::stdin().is_terminal()
}

/// No downloads nobody asked for (the meaning model when the memory grows):
/// `OMOIKANE_NONINTERACTIVE=1` or `OMOIKANE_NO_MODEL=1`.
pub fn no_downloads() -> bool {
    unattended() || is_on("OMOIKANE_NO_MODEL")
}

/// The background sync after each change and every few minutes: `OMOIKANE_SYNC_AUTO=0` turns it off
/// and `=1` turns it on; if unset, on unless nobody is in front (in a CI the background
/// process would die with the job, or make it longer: there you sync with `omoi sync` at the end).
pub fn auto_sync() -> bool {
    match value("OMOIKANE_SYNC_AUTO").as_deref() {
        Some("0") => false,
        Some("1") => true,
        _ => !unattended(),
    }
}

/// `OMOIKANE_AUTHOR`: who signs what is saved here.
pub fn author() -> Option<String> {
    value("OMOIKANE_AUTHOR")
}

/// The codes in `OMOIKANE_SYNC_CODE` (separated by spaces, commas or lines: one per repository,
/// the general one and the spaces' ones).
pub fn sync_codes() -> Vec<String> {
    value("OMOIKANE_SYNC_CODE")
        .map(|v| v.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// `OMOIKANE_SYNC_URL`: another URL for the code's repository (only with one code).
pub fn sync_url() -> Option<String> {
    value("OMOIKANE_SYNC_URL")
}

#[cfg(test)]
mod tests {
    use super::home_from;

    /// A relative `OMOIKANE_HOME` is not valid; an absolute one, as is; `~/…`, from the home folder.
    #[test]
    fn home_absolute_or_with_tilde() {
        assert_eq!(home_from("relativa/omoikane"), None);
        assert_eq!(home_from(".omoikane"), None);
        let abs = std::env::temp_dir().join("omoi-casa");
        assert_eq!(home_from(&abs.to_string_lossy()), Some(abs));
        if let Some(u) = std::env::home_dir().filter(|u| u.is_absolute()) {
            assert_eq!(home_from("~/omoi"), Some(u.join("omoi")));
            assert_eq!(home_from("~"), Some(u));
        }
    }

    /// A network `OMOIKANE_HOME` (also after `~/`) is not valid: every command would open the database there. Only on
    /// Windows: on Linux and macOS the backslash is just another letter of the name and `//servidor` is a
    /// local folder, so those paths open no connection.
    #[cfg(windows)]
    #[test]
    fn network_home_is_not_valid() {
        for h in [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\\?\UNC\10.255.255.1\x", r"\??\UNC\10.255.255.1\x", r"~/\\10.255.255.1\x", r"~\//10.255.255.1/x"] {
            assert_eq!(home_from(h), None, "{h}");
        }
    }

    /// An `OMOIKANE_HOME` or a database with a drive's extended-path prefix (`\\?\C:\…`) is on
    /// this computer and is valid; `\\?\UNC\…` isn't.
    #[cfg(windows)]
    #[test]
    fn extended_path_of_a_drive_is_valid() {
        let h = r"\\?\C:\Users\alguien\.omoikane";
        assert_eq!(home_from(h), Some(std::path::PathBuf::from(h)));
        assert_eq!(super::bad_path("--db", std::path::Path::new(r"\\?\C:\x\o.db")), None);
        assert!(super::bad_path("--db", std::path::Path::new(r"\\?\UNC\10.255.255.1\x\o.db")).is_some());
    }
}
