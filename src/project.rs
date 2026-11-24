//! Which project a folder belongs to: the root of the git repository that contains it, or the folder
//! itself if there is none. Always in canonical form, so that `C:\Repo`, `c:\repo\.` and a
//! symbolic link to the same folder are the same project.

use crate::tr;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether a path text is a network or device path: it starts with two slashes (`\\server\share`,
/// `//cdn.example.com/x.js`, `\\?\…`, `\\.\…`, mixed or not). What a memory says (its `paths`, the
/// paths it names) is written by an agent or comes from someone else: on Windows, just checking whether
/// such a path exists opens an SMB connection (which can take a while to fail and hands the person's
/// NTLM hash to whatever server it names). Those are never looked at.
pub fn is_network_path(path: &str) -> bool {
    let b = path.trim_start().as_bytes();
    b.len() >= 2 && matches!(b[0], b'\\' | b'/') && matches!(b[1], b'\\' | b'/') && !is_wsl(path.trim_start())
}

// --- WSL ----------------------------------------------------------------------------------------------
//
// `\\wsl$\<distro>\…` and `\\wsl.localhost\<distro>\…` look like network paths, but they are not SMB to
// another machine: they are served by P9NP, the WSL network provider, which talks to this computer's
// WSL virtual machine over a Hyper-V socket (9P), with no NTLM and without going out to the network.
// That is why they count as local even when they come from outside (a path in a memory, `project` over
// MCP, `?dir=`).
//
// Only if P9NP is installed and comes before the SMB client (`LanmanWorkstation`) in the provider
// order (`HKLM\SYSTEM\CurrentControlSet\Control\NetworkProvider\Order`), which is how it is left after
// installing WSL: Windows asks each provider in order and keeps the first one that recognises the
// path. Without WSL (or with SMB in front), `wsl$` and `wsl.localhost` would be machine names like any
// other and the SMB client would look them up on the network (LLMNR, NetBIOS, DNS): someone on the
// network could answer and receive the NTLM hash. Then they are network paths, like the rest. Measured
// on a computer with WSL: a distribution that doesn't exist fails in 3-80 ms and leaves no DNS query for
// `wsl.localhost`. The cost: looking at a path of a stopped distribution can start WSL (a few seconds).

/// Whether a path text is a WSL one (`\\wsl$\<distro>\…`, `\\wsl.localhost\<distro>\…`, also with
/// `\\?\UNC\` in front or with `/`) and WSL serves those paths on this computer (see above).
fn is_wsl(path: &str) -> bool {
    wsl_shape(path) && wsl_serves()
}

/// Only the shape: two slashes, `wsl$` or `wsl.localhost` and a real distribution name (not empty
/// nor `.` nor `..`, which could escape it when the path is normalized).
fn wsl_shape(path: &str) -> bool {
    let slash = |c: char| c == '\\' || c == '/';
    let Some(rest) = path.strip_prefix(['\\', '/']).and_then(|r| r.strip_prefix(['\\', '/'])) else { return false };
    let rest = match rest.get(..6) {
        Some(p) if p.eq_ignore_ascii_case("?\\UNC\\") || p.eq_ignore_ascii_case("?/UNC/") => &rest[6..],
        _ => rest,
    };
    let mut parts = rest.split(slash);
    let server = parts.next().unwrap_or_default();
    let distro = parts.next().unwrap_or_default();
    (server.eq_ignore_ascii_case("wsl$") || server.eq_ignore_ascii_case("wsl.localhost")) && !matches!(distro, "" | "." | "..")
}

/// Whether WSL serves `\\wsl$` paths on this computer: P9NP in the network provider order before
/// `LanmanWorkstation`. Asked once per process.
#[cfg(windows)]
fn wsl_serves() -> bool {
    static SERVES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SERVES.get_or_init(|| provider_order().as_deref().is_some_and(p9np_before_smb))
}

#[cfg(not(windows))]
fn wsl_serves() -> bool {
    false
}

/// `ProviderOrder` from the registry (`P9NP,RDPNP,LanmanWorkstation,webclient`), read with `RegGetValueW`.
#[cfg(windows)]
fn provider_order() -> Option<String> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(key: isize, subkey: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut u8, size: *mut u32) -> i32;
    }
    // `HKEY_LOCAL_MACHINE` is `(HKEY)(ULONG_PTR)(LONG)0x80000002`: sign-extended.
    const HKLM: isize = 0x8000_0002_u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let subkey = wide(r"SYSTEM\CurrentControlSet\Control\NetworkProvider\Order");
    let value = wide("ProviderOrder");
    let mut data = vec![0u16; 2048];
    let mut size = (data.len() * 2) as u32;
    // SAFETY: 0-terminated strings that live until the end of the call, and a buffer of `size` bytes.
    let r = unsafe { RegGetValueW(HKLM, subkey.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), data.as_mut_ptr().cast(), &mut size) };
    if r != 0 {
        return None;
    }
    let n = (size as usize / 2).min(data.len());
    Some(String::from_utf16_lossy(&data[..n]).trim_end_matches('\0').to_string())
}

/// Whether in a provider order P9NP comes before the SMB client (or there is no SMB client). Only
/// exists on Windows (and in the tests, which check it on every system).
#[cfg(any(windows, test))]
fn p9np_before_smb(order: &str) -> bool {
    let v: Vec<String> = order.split(',').map(|x| x.trim().to_ascii_lowercase()).collect();
    match (v.iter().position(|x| x == "p9np"), v.iter().position(|x| x == "lanmanworkstation")) {
        (Some(p9), Some(smb)) => p9 < smb,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Whether a path is on this computer and can be looked at on disk without going out to the network:
/// not [`is_network_path`] and, on Windows, no prefix other than a drive one (`C:`): UNC, `\\?\` and `\\.\`
/// are out. A network drive with a letter (`Z:`) can't be told apart without asking the system and is
/// looked at. WSL ones (`\\wsl$\…`, `\\wsl.localhost\…`) are on this computer if WSL serves them (see
/// [`is_wsl`]).
///
/// It is the filter for everything that comes from outside. The folder the person works in can be in
/// a network folder and is read all the same: see [`readable`].
pub fn is_local_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if is_wsl(s.trim_start()) {
        return true;
    }
    #[cfg(windows)]
    if extended_disk(&s) {
        return true;
    }
    if is_network_path(&s) {
        return false;
    }
    // `\??\…` (the NT path prefix): Windows passes it through as is, so `\??\UNC\host\x` is a network
    // share even though it starts with a single slash (for `Path`, the root of the current drive).
    let b = s.trim_start().as_bytes();
    let nt = b.len() >= 4 && matches!(b[0], b'\\' | b'/') && b[1] == b'?' && b[2] == b'?' && matches!(b[3], b'\\' | b'/');
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !nt && let Some(Component::Prefix(x)) = p.components().next() {
            return matches!(x.kind(), Prefix::Disk(_));
        }
    }
    !nt
}

/// Whether a path text is a drive with the Windows extended-path prefix (`\\?\C:\…`, the form
/// `canonicalize` gives and that some programs use to go past 260 characters): it is on this computer
/// like `C:\…` (and like `Z:`, a network drive with a letter can't be told apart without asking the
/// system; see [`is_local_path`]). Only with backslashes, which is how Windows recognises it (`//?/C:/` is
/// not this), one letter, `:` and nothing or a slash after it, and no `..`: with the prefix, Windows
/// doesn't normalize the path and passes it as is. `\\?\UNC\…` (network), `\\?\Volume{…}`,
/// `\\?\GLOBALROOT\…`, `\\.\…` and `\??\…` are not.
#[cfg(any(windows, test))]
fn extended_disk(s: &str) -> bool {
    let Some(rest) = s.strip_prefix(r"\\?\") else { return false };
    let b = rest.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\') && !rest.split(['\\', '/']).any(|c| c == "..")
}

/// The path if it is local ([`is_local_path`]); `None` if not. For folders that come from an environment
/// variable (`HOME`, `USERPROFILE`, `OMOIKANE_HOME`, `COPILOT_HOME`…) before looking at anything inside.
pub fn local(p: PathBuf) -> Option<PathBuf> {
    is_local_path(&p).then_some(p)
}

/// The user's folder if it is local (see [`local`]).
pub fn local_home() -> Option<PathBuf> {
    std::env::home_dir().filter(|h| h.is_absolute()).and_then(local)
}

/// `is_dir` of a path that may come from outside (from a sync line, from a file, from an agent): only
/// if it is local ([`is_local_path`]); a network one isn't even looked at.
pub fn is_dir(p: &Path) -> bool {
    is_local_path(p) && p.is_dir()
}

/// `exists`, with the same condition as [`is_dir`].
pub fn exists(p: &Path) -> bool {
    is_local_path(p) && p.exists()
}

/// The error for a path that is not on this computer.
fn not_local(p: &Path) -> anyhow::Error {
    anyhow::anyhow!(tr!(
        "{} is not a folder on this computer: Omoikane doesn't open network paths (\\\\server\\share, //host/x, \\\\?\\UNC\\…, \\??\\…); map it to a drive letter if you need it",
        "{} no es una carpeta de este equipo: Omoikane no abre rutas de red (\\\\servidor\\recurso, //host/x, \\\\?\\UNC\\…, \\??\\…); si la necesitas, asígnale una letra de unidad",
        p.display()
    ))
}

/// Error if the path is not local ([`is_local_path`]), before touching the disk.
pub fn require_local(p: &Path) -> Result<()> {
    if is_local_path(p) { Ok(()) } else { Err(not_local(p)) }
}

// --- The folder being worked in -------------------------------------------------------------------
//
// There are two kinds of paths. Those that come from OUTSIDE (the text and the `paths` of a memory,
// `project` or `dir` over MCP, `?dir=` from the web, `--dir`, what the sync brings, a bundle, an
// environment variable) can be written by an agent, another person or any website: they go through
// [`is_local_path`] and a network one is never opened (on Windows, just looking at it opens an SMB
// connection to whatever server it names, which hands over the person's NTLM hash and can take ~21 s
// to fail).
//
// The folder the person WORKS in doesn't come from outside: it is the process's (`omoi` launched
// there, the agent's MCP server) or the `cwd` sent by the agent's own hook, which is the folder of its
// session. That one can be in a network folder (`\\server\share`, a shared drive, WSL) and there the
// repository is read as in 1.8.0: its `origin` (the project's `git:` key), `.git/HEAD` (the provenance
// commit), its common git folder (worktrees) and whether it exists (`last_seen`). Otherwise, a new
// project there got `dir:person/name`, was never seen and the hook failed.
//
// What is trusted is that folder's whole share (`\\server\share`), not just the folder: the
// repository can start higher up (`\\srv\team\repo` while working in `\\srv\team\repo\src`) and a
// worktree's git folder can be next to it. Reading another folder of the same share opens no new
// connection and talks to no other machine: the system is already connected there because the person
// works there. A different share, even on the same server, is still outside. And this only applies to
// the repository reads in the list above ([`readable`]): `of_dir`, `canonical` and everything else that
// receives paths from outside still go through [`is_local_path`].

/// The network shares (`server\share`, lowercase) of the work folders this process received from a
/// hook ([`of_trusted_dir`]). The process's own one is checked separately, each time ([`is_work_share`]).
static WORK_SHARES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The network share of a UNC path (`\\server\share\…` or `\\?\UNC\server\share\…`) in lowercase;
/// `None` if it is not UNC (a drive, a relative path, `\\.\…`, `\??\…`, or outside Windows).
fn network_share(p: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(x)) = p.components().next()
            && let Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) = x.kind()
        {
            return Some(format!("{}\\{}", server.to_string_lossy(), share.to_string_lossy()).to_lowercase());
        }
    }
    let _ = p;
    None
}

/// Whether `share` is that of a work folder: the process's or one received from a hook.
fn is_work_share(share: &str) -> bool {
    std::env::current_dir().ok().and_then(|d| network_share(&d)).as_deref() == Some(share)
        || WORK_SHARES.lock().is_ok_and(|v| v.iter().any(|r| r == share))
}

/// Whether the repository of a path can be read: it is local ([`is_local_path`]) or it is on the same
/// network share as the folder being worked in (see above). Only for reading a project's repository
/// (`origin`, `HEAD`, the common git folder, whether it exists); never to decide whether a path from
/// outside is accepted.
pub fn readable(p: &Path) -> bool {
    is_local_path(p) || network_share(p).is_some_and(|r| is_work_share(&r))
}

/// The path if it is [`readable`]; `None` if not.
pub fn if_readable(p: PathBuf) -> Option<PathBuf> {
    readable(&p).then_some(p)
}

/// Project of the folder the person works in, which may be on the network: the process's
/// ([`current`]) or the `cwd` sent by the agent's hook (the folder of its session). From here on, its
/// network share counts as a work one for [`readable`]. Never for a path that comes from outside (an
/// argument, `project`, `?dir=`): those go through [`of_dir`].
pub fn of_trusted_dir(dir: &Path) -> Result<String> {
    note_work_folder(dir);
    of_unchecked(dir)
}

/// Notes the network share of a work folder (if it is on the network), without looking at the disk.
fn note_work_folder(dir: &Path) {
    if let Some(r) = network_share(dir) {
        if let Ok(mut v) = WORK_SHARES.lock()
            && !v.contains(&r)
        {
            v.push(r);
        }
        if let Ok(mut v) = WORK_FOLDERS.lock()
            && !v.iter().any(|c| c == dir)
        {
            v.push(dir.to_path_buf());
        }
    }
}

/// The network work folders this process received ([`of_trusted_dir`]), whole: to accept that same
/// folder when it comes as `project` ([`of_requested_dir`]).
static WORK_FOLDERS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Project of a folder that comes from outside (`project` over MCP or via `omoi serve`): like [`of_dir`],
/// plus the session's own network work folder (the process's or the hook's, see [`of_trusted_dir`]) or
/// the root of its repository, written any way (`\\?\UNC\…`, `/`, uppercase, a trailing slash). That
/// one is already read: the agent working in `\\server\team\repo` that passes it as `project` asks for
/// nothing new. Any other network path, even on the same share or a folder inside the work one, is
/// still an error without touching the disk.
pub fn of_requested_dir(dir: &Path) -> Result<String> {
    if is_local_path(dir) {
        return of_dir(dir);
    }
    let requested = comparison_form(dir).ok_or_else(|| not_local(dir))?;
    let mut work: Vec<PathBuf> = std::env::current_dir().into_iter().filter(|d| network_share(d).is_some()).collect();
    if let Ok(v) = WORK_FOLDERS.lock() {
        work.extend(v.iter().cloned());
    }
    for t in work {
        let Some(form) = comparison_form(&t) else { continue };
        if form == requested {
            return of_trusted_dir(&t);
        }
        // The root of its repository (the session's project, the one `context` and the memories
        // show), which is above the work folder: it is computed from that folder, which is already
        // trusted. If the requested one is not above it, it isn't even looked at.
        if form.starts_with(&format!(r"{requested}\"))
            && let Ok(root) = of_trusted_dir(&t)
            && comparison_form(Path::new(&root)).as_ref() == Some(&requested)
        {
            return Ok(root);
        }
    }
    Err(not_local(dir))
}

/// A UNC path written in a single form to compare it without touching the disk: `\\server\…` with
/// backslashes, without `\\?\UNC\`, without extra or trailing slashes and, on Windows and macOS, in
/// lowercase ([`comparable`]). `None` if it is not UNC or contains `.` or `..` (they would lead to
/// another folder).
fn comparison_form(p: &Path) -> Option<String> {
    network_share(p)?;
    let s = p.to_string_lossy().replace('/', r"\");
    let rest = match s.get(..8) {
        Some(x) if x.eq_ignore_ascii_case(r"\\?\UNC\") => &s[8..],
        _ => s.strip_prefix(r"\\")?,
    };
    let parts: Vec<&str> = rest.split('\\').filter(|c| !c.is_empty()).collect();
    if parts.len() < 2 || parts.iter().any(|c| *c == "." || *c == "..") {
        return None;
    }
    Some(comparable(&format!(r"\\{}", parts.join(r"\"))))
}

/// Project of the current folder, which may be on a network share (see [`of_trusted_dir`]).
pub fn current() -> Result<String> {
    of_trusted_dir(&std::env::current_dir().with_context(|| tr!("can't read the current folder", "no se puede leer la carpeta actual"))?)
}

/// Project of a folder. A path that is not local ([`is_local_path`]) gives an error without touching the
/// disk: on Windows, just looking at it opens an SMB connection (which takes a while to fail and hands
/// the person's NTLM hash to whatever server it names), and the path can come from an agent
/// (`project`) or from a website (`?dir=`).
pub fn of_dir(dir: &Path) -> Result<String> {
    require_local(dir)?;
    of_unchecked(dir)
}

fn of_unchecked(dir: &Path) -> Result<String> {
    let dir = canonical_unchecked(dir)?;
    let root = dir.ancestors().find(|d| d.join(".git").exists()).unwrap_or(&dir);
    Ok(path_text(root))
}

/// Whether the project is a general folder and not a project: the user's folder, one above it or the
/// root of a drive, without a git repository. Whoever opens sessions there works on many projects at
/// once, so from there the memories of all of them are visible.
pub fn is_general(project: &str) -> bool {
    match home_project() {
        Some(home) => general_in(project, Path::new(home)),
        None => false,
    }
}

/// The user's folder as a project (`of_dir`), computed once per process: on Windows, resolving it costs
/// a couple of milliseconds, and the recall of every message asks about the scope several times.
fn home_project() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| local_home().and_then(|h| of_dir(&h).ok())).as_deref()
}

fn general_in(project: &str, home: &Path) -> bool {
    let p = Path::new(project);
    !project.is_empty() && is_local_path(p) && !p.join(".git").exists() && (home.starts_with(p) || p.parent().is_none())
}

/// Project of a folder someone gives on purpose (`--project`, `project`): a general folder is not a
/// project, so it gives global.
pub fn of_given_folder(dir: &Path) -> Result<String> {
    let p = of_requested_dir(dir)?;
    Ok(if is_general(&p) { String::new() } else { p })
}

/// Where to save a memory that doesn't say its project: the current one if it is a real project; if
/// it is a general folder, the repository its absolute paths point to (those in `paths` and those the
/// text cites) when there is only one, and otherwise none: global. Also returns the paths: if the
/// project came from them, those that fall inside that repository become relative to its root.
pub fn for_saving(current: &str, paths: &[String], text: &str) -> (String, Vec<String>) {
    if !is_general(current) {
        return (current.to_string(), paths.to_vec());
    }
    let home = home_project().unwrap_or_default();
    let home = Path::new(home);
    let mut repos: Vec<String> = paths
        .iter()
        .cloned()
        .chain(paths_in(text, home))
        .filter_map(|r| repo_of_path(&r, home))
        .collect();
    repos.sort();
    repos.dedup();
    match repos.as_slice() {
        [one] => (one.clone(), paths.iter().map(|r| relative_to(r, one, home)).collect()),
        _ => (String::new(), paths.to_vec()),
    }
}

/// Words that look like an absolute path cited in a text. A path with spaces
/// (`C:\Users\marcm\the codex\app\x.kt`) arrives split into words: it is extended with the following
/// ones (at most `MAX_WORDS`) and the one that goes deepest on disk is kept.
fn paths_in(text: &str, home: &Path) -> Vec<String> {
    const MAX_WORDS: usize = 6;
    let mut v = Vec::new();
    for chunk in text.split(|c: char| c == '\n' || "`\"'«»()[]<>,;*".contains(c)) {
        let words: Vec<&str> = chunk.split(' ').filter(|p| !p.is_empty()).collect();
        for (i, p) in words.iter().enumerate() {
            if expand(p, home).is_none() {
                continue;
            }
            let mut best = (*p).to_string();
            let mut deepest = depth(&best, home);
            let mut candidate = best.clone();
            for next in words.iter().skip(i + 1).take(MAX_WORDS) {
                candidate = format!("{candidate} {next}");
                let h = depth(&candidate, home);
                if h > deepest {
                    (best, deepest) = (candidate.clone(), h);
                }
            }
            v.push(best.trim_end_matches(['.', ':', '?', '!']).to_string());
        }
    }
    v
}

/// How many components of the path exist on disk (those of the deepest existing ancestor). Only for an
/// absolute, local path (see [`expand`]): any other isn't even looked at.
fn depth(path: &str, home: &Path) -> usize {
    let Some(r) = expand(path, home) else { return 0 };
    r.ancestors().find(|a| a.exists()).map_or(0, |a| a.components().count())
}

/// An absolute (or `~`) and local ([`is_local_path`]) path, without trailing punctuation; `None` if it
/// isn't. What follows `~/` can be another whole path (`~/\\host\share`, `~\//host/x`, `~/C:\x`): when
/// joined with the home folder, `join` keeps that one. That's why the already assembled path is checked.
fn expand(path: &str, home: &Path) -> Option<PathBuf> {
    let path = path.trim_end_matches(['.', ':', '?', '!']);
    if is_network_path(path) {
        return None;
    }
    let b = path.as_bytes();
    let absolute = (b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || (cfg!(unix) && path.len() > 1 && path.starts_with('/'));
    let p = if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        // Without a home folder (or with one that isn't local), `~` is nothing.
        if home.as_os_str().is_empty() || !home.is_absolute() {
            return None;
        }
        home.join(rest)
    } else if absolute {
        PathBuf::from(path)
    } else {
        return None;
    };
    (p.is_absolute() && is_local_path(&p)).then_some(p)
}

/// The path in canonical form even if its end doesn't exist yet: the deepest existing ancestor,
/// canonical, plus the rest as is. `None` for a path that is not local (it isn't even looked at).
fn partial_canonical(path: &Path) -> Option<PathBuf> {
    if !is_local_path(path) {
        return None;
    }
    let existing = path.ancestors().find(|a| a.exists())?;
    let rest = path.strip_prefix(existing).ok()?;
    Some(PathBuf::from(path_text(&canonical(existing).ok()?)).join(rest))
}

/// `path` relative to `repo` if it falls inside; if not (or it isn't absolute), as is.
fn relative_to(path: &str, repo: &str, home: &Path) -> String {
    let Some(abs) = expand(path, home).and_then(|p| partial_canonical(&p)) else { return path.to_string() };
    match abs.strip_prefix(repo) {
        Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().replace('\\', "/"),
        _ => path.to_string(),
    }
}

/// The git repository that contains an absolute path (even if the file no longer exists), if there is
/// one and it isn't the user's folder.
fn repo_of_path(path: &str, home: &Path) -> Option<String> {
    let path = expand(path, home)?;
    if !is_local_path(&path) {
        return None;
    }
    let existing = path.ancestors().find(|a| a.exists())?;
    let root = existing.ancestors().find(|d| d.join(".git").exists())?;
    let root = of_dir(root).ok()?;
    // Tool clones (`~/.claude/plugins/…`, `~/.cargo/git/…`, `AppData`) are not projects.
    let hidden = Path::new(&root).strip_prefix(home).is_ok_and(|r| {
        r.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c.starts_with('.') || c.eq_ignore_ascii_case("AppData")
        })
    });
    (Path::new(&root) != home && !hidden).then_some(root)
}

/// The canonical form of a local path; one that isn't ([`is_local_path`]) gives an error without touching
/// the disk.
pub(crate) fn canonical(p: &Path) -> Result<PathBuf> {
    require_local(p)?;
    canonical_unchecked(p)
}

fn canonical_unchecked(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| tr!("{} doesn't exist", "no existe {}", p.display()))?;
    // On Windows, canonicalize returns `\\?\C:\…`: the prefix is removed to store the normal path.
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(c)
}

/// The path as text, with the drive letter in uppercase on Windows: that way paths coming from
/// different places match.
#[cfg(windows)]
pub(crate) fn path_text(p: &Path) -> String {
    let mut b = p.to_string_lossy().into_owned().into_bytes();
    if b.len() >= 2 && b[1] == b':' {
        b[0] = b[0].to_ascii_uppercase();
    }
    String::from_utf8(b).expect("ruta UTF-8")
}

#[cfg(not(windows))]
pub(crate) fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// --- Projects and spaces in the database (schema 10) ----------------------------------------------
//
// A project is a row of `project` with its sync key (computed once, when it is created) and all the
// folders where it lives in `project_path`: a clone, a worktree, the folder it was moved to or the path
// another computer brought (`is_foreign`). A space (`space`) groups projects of the same client: from a
// project you see its own memories, those of its space and the global ones, never those of another
// space.

/// How two paths are compared: on Windows and macOS (case-insensitive file systems), in lowercase.
pub fn comparable(path: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) { path.to_lowercase() } else { path.to_string() }
}

/// The short name of a project: that of its folder (or, without a path, the end of the key).
pub fn name_of(path: &str, key: &str) -> String {
    // `[<key>]` is the path of a project that came through the sync without a folder here (`of_key`).
    let path = if path.starts_with('[') && path.ends_with(']') { "" } else { path };
    let from_path = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").to_string();
    if !from_path.is_empty() {
        return from_path;
    }
    key.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or(key).to_string()
}

/// The git `origin` of a folder, normalized as in the keys (empty if it has none).
pub fn origin(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    crate::sync::git_origin(Path::new(path)).map(|u| crate::sync::normalize_remote(&u)).unwrap_or_default()
}

fn now(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |f| f.get(0))?)
}

/// The project (and its space) of a folder that is already registered.
fn by_path(db: &Connection, path: &str) -> Result<Option<(i64, Option<i64>)>> {
    Ok(db
        .query_row(
            "SELECT pp.project_id, p.space_id FROM project_path pp JOIN project p ON p.id = pp.project_id WHERE pp.path_cmp = ?1",
            [comparable(path)],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?)
}

/// What a folder that is not registered yet is.
enum Decision {
    /// Another folder of the project that already has that key (a clone, a worktree, the moved folder
    /// of a repository with `origin`, or the local folder of a project that came from another computer).
    Join(i64, Option<i64>),
    /// A new project, with this key.
    New(String),
}

fn decide(db: &Connection, path: &str, person: &str) -> Result<Decision> {
    let key = crate::sync::project_key_for(path, person);
    // Without `origin`, the worktrees of the same repository are recognised by their common git folder:
    // the worktree of a repository whose main folder is already registered is that project, whatever
    // its folder is called.
    let common = if key.starts_with("dir:") { git_common_dir(Path::new(path)) } else { None };
    // (The main one is not the folder itself, not even written another way: `CLAUDE~1` and its long name.)
    if let Some(main) = common.as_deref().and_then(main_of)
        && main != path
        && legible_canonical(Path::new(path)).map(|p| path_text(&p)).as_deref() != Some(main.as_str())
        && let Some((id, space)) = by_path(db, &main)?
    {
        return Ok(Decision::Join(id, space));
    }
    let same_repo = |id: i64| -> Result<bool> {
        let Some(c) = &common else { return Ok(false) };
        let mut q = db.prepare("SELECT path FROM project_path WHERE project_id = ?1 AND is_foreign = 0")?;
        let paths: Vec<String> = q.query_map([id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(paths.iter().any(|r| git_common_dir(Path::new(r)).as_ref() == Some(c)))
    };
    let existing: Option<(i64, Option<i64>)> =
        db.query_row("SELECT id, space_id FROM project WHERE key = ?1", [&key], |f| Ok((f.get(0)?, f.get(1)?))).optional()?;
    let Some((id, space)) = existing else { return Ok(Decision::New(key)) };
    // With `origin`, the same key is the same repository. Without it (`dir:`), two folders with the same
    // name can be different things: it only joins a project that has no folder of its own here (the
    // one from another computer of the same person), or one whose git repository it is a worktree of.
    // A folder moved without `origin` is fixed with `omoi projects move` (Review shows it as
    // «ya no está»).
    if key.starts_with("git:") || own_folders(db, id)? == 0 || same_repo(id)? {
        return Ok(Decision::Join(id, space));
    }
    let mut n = 2;
    loop {
        let k = format!("{key}~{n}");
        let found: Option<(i64, Option<i64>, i64)> = db
            .query_row(
                "SELECT p.id, p.space_id, (SELECT count(*) FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0) FROM project p WHERE p.key = ?1",
                [&k],
                |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)),
            )
            .optional()?;
        match found {
            None => return Ok(Decision::New(k)),
            Some((id, space, 0)) => return Ok(Decision::Join(id, space)),
            Some((id, space, _)) if same_repo(id)? => return Ok(Decision::Join(id, space)),
            Some(_) => n += 1,
        }
    }
}

/// The common git folder of a repository root, canonical: its `.git`, or in a worktree (`.git` is a
/// `gitdir: …` file and that folder has `commondir`) the main repository's. `None` if there is no
/// repository, or if `.git` is a file that doesn't belong to a worktree (a submodule).
pub(crate) fn git_common_dir(root: &Path) -> Option<PathBuf> {
    // Local, or on the network share of the folder being worked in (see `readable`).
    if !readable(root) {
        return None;
    }
    let dot = root.join(".git");
    let dir = if dot.is_dir() {
        dot
    } else {
        let t = std::fs::read_to_string(&dot).ok()?;
        // What the `.git` file (and its `commondir`) says can also be a network path: from another
        // share, it isn't looked at.
        let gitdir = if_readable(root.join(t.trim().strip_prefix("gitdir:")?.trim()))?;
        let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
        gitdir.join(common.trim())
    };
    legible_canonical(&dir)
}

/// The canonical form of a [`readable`] path; `None` if it isn't (without touching the disk) or doesn't
/// exist.
fn legible_canonical(p: &Path) -> Option<PathBuf> {
    if readable(p) { canonical_unchecked(p).ok() } else { None }
}

/// The main work folder of a common git folder (`<main>/.git`); `None` in a bare repository.
fn main_of(common: &Path) -> Option<String> {
    (common.file_name()? == ".git").then(|| common.parent().map(path_text)).flatten()
}

/// The repository name of a worktree without `origin`: that of the main folder (or that of the bare
/// repository without `.git`), in lowercase. `None` if the folder is not a worktree.
pub(crate) fn worktree_name(root: &Path) -> Option<String> {
    if !readable(root) || root.join(".git").is_dir() {
        return None;
    }
    let common = git_common_dir(root)?;
    let name = match main_of(&common) {
        Some(p) => Path::new(&p).file_name()?.to_string_lossy().into_owned(),
        None => {
            let n = common.file_name()?.to_string_lossy().into_owned();
            n.strip_suffix(".git").map(str::to_string).unwrap_or(n)
        }
    };
    (!name.is_empty()).then(|| name.to_lowercase())
}

/// How many folders of its own (from this computer, not those another one brought) a project has.
pub fn own_folders(db: &Connection, project: i64) -> Result<i64> {
    Ok(db.query_row("SELECT count(*) FROM project_path WHERE project_id = ?1 AND is_foreign = 0", [project], |f| f.get(0))?)
}

/// Whether a folder is already registered as a folder of a project.
pub fn is_registered(db: &Connection, path: &str) -> Result<bool> {
    Ok(by_path(db, path)?.is_some())
}

/// The project of a folder, without writing anything: the registered one or, if it isn't, the one
/// that has its key (if the folder would join it). `None`: a project that has nothing yet. The empty
/// folder is global (0).
pub fn resolve(db: &Connection, path: &str) -> Result<Option<(i64, Option<i64>)>> {
    if path.is_empty() {
        return Ok(Some((0, None)));
    }
    if let Some(r) = by_path(db, path)? {
        return Ok(Some(r));
    }
    Ok(match decide(db, path, &crate::db::person_of(db))? {
        Decision::Join(id, space) => Some((id, space)),
        Decision::New(_) => None,
    })
}

/// The id of a folder's project, registering it if needed (and creating the project if it is new).
/// Along the way it records when it was seen and its `origin`: if that changed, the key doesn't change
/// (the memories of the other computers still use it) and Review warns about it.
pub fn register(db: &Connection, path: &str) -> Result<i64> {
    Ok(register_reporting(db, path)?.0)
}

/// Like `register`, plus the folder where the project was if this one replaces it (that one no
/// longer exists: the folder was moved and the `origin` says so). It happens only once: afterwards,
/// the main one is this one.
pub fn register_reporting(db: &Connection, path: &str) -> Result<(i64, Option<String>)> {
    if path.is_empty() {
        return Ok((0, None));
    }
    let now = now(db)?;
    // A project's folder can be on the network share being worked in (see `readable`).
    let exists = readable(Path::new(path)) && Path::new(path).is_dir();
    if let Some((id, space)) = by_path(db, path)? {
        if exists {
            // The path another computer brought now exists here: it is this computer's first folder,
            // and as when joining by key, entering a space this way needs confirming.
            let claimed = space.is_some() && own_folders(db, id)? == 0;
            db.execute(
                "UPDATE project_path SET is_foreign = 0, origin = ?2, last_seen = ?3
                 WHERE path_cmp = ?1 AND (is_foreign = 1 OR origin != ?2 OR last_seen IS NULL OR last_seen < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day'))",
                params![comparable(path), origin(path), now],
            )?;
            if claimed && !adopts_without_pause(db, id)? {
                pause_for_claim(db, id)?;
            }
            return Ok((id, main_if_needed(db, id, path)?));
        }
        return Ok((id, None));
    }
    let (id, claimed) = match decide(db, path, &crate::db::person_of(db))? {
        // A project that is in a space without having any folder of its own here is in it because that
        // space's repository says so (a `project` line, or a memory with its key): this folder doesn't
        // enter it without the person confirming.
        Decision::Join(id, space) => (id, space.is_some() && own_folders(db, id)? == 0),
        Decision::New(key) => {
            db.execute(
                "INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![key, name_of(path, &key), path, now],
            )?;
            (db.last_insert_rowid(), false)
        }
    };
    db.execute(
        "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![path, comparable(path), id, origin(path), exists.then_some(&now)],
    )?;
    if claimed && !adopts_without_pause(db, id)? {
        pause_for_claim(db, id)?;
    }
    Ok((id, main_if_needed(db, id, path)?))
}

/// A project's main path (the one the memories show) becomes `path` if the current one is from another
/// computer or no longer exists here (the folder was moved). Returns the previous one if it was from
/// here and was moved (to warn).
fn main_if_needed(db: &Connection, id: i64, path: &str) -> Result<Option<String>> {
    let (current, foreign): (String, bool) = db.query_row(
        "SELECT p.path, COALESCE((SELECT is_foreign FROM project_path WHERE path = p.path), 1) FROM project p WHERE p.id = ?1",
        [id],
        |f| Ok((f.get(0)?, f.get(1)?)),
    )?;
    if current != path && is_dir(Path::new(path)) && (foreign || current.is_empty() || !is_dir(Path::new(&current))) {
        db.execute("UPDATE project SET path = ?2, name = ?3 WHERE id = ?1", params![id, path, name_of(path, "")])?;
        return Ok((!foreign && !current.is_empty()).then_some(current));
    }
    Ok(None)
}

/// `omoi projects prune`: projects without memories (and their registered folders) are deleted. The
/// one for a folder that is worked in again is created again by itself. Returns how many.
pub fn prune(db: &Connection) -> Result<usize> {
    crate::diary::before_prune(db)?;
    db.execute("DELETE FROM project_path WHERE project_id != 0 AND project_id NOT IN (SELECT DISTINCT project_id FROM memory)", [])?;
    let n = db.execute("DELETE FROM project WHERE id != 0 AND id NOT IN (SELECT DISTINCT project_id FROM memory)", [])?;
    crate::diary::release_pruned(db)?;
    Ok(n)
}

/// The project of a key that comes from another computer (or from an `omoi export` file), creating it
/// with the origin path if there is none here. If that path exists here with the same key, it is the
/// project's local folder; if it exists with another key or already belongs to another project, the
/// foreign path carries the key after it (`/workspace [git:…]`) so it isn't confused with it.
///
/// With `from_copy` (an `omoi import` or a restore from the trash, not the sync), an unregistered
/// folder that exists here at the origin path and that would have that same key with the key's person
/// (`dir:<person>/<folder>`) is that folder: the copy is of this same folder, maybe from another
/// database with another local person. Not through the sync: two people can have the same path on
/// their computers.
pub fn of_key(db: &Connection, key: &str, origin_path: &str, person: &str, from_copy: bool) -> Result<i64> {
    if key.is_empty() {
        return Ok(0);
    }
    if let Some(id) = db.query_row("SELECT id FROM project WHERE key = ?1", [key], |f| f.get(0)).optional()? {
        return Ok(id);
    }
    let exists = !origin_path.is_empty() && is_dir(Path::new(origin_path));
    // In a copy (not through the sync), the 1.9.0 key of a folder that is already registered here with
    // the usual one (`dir:<person>/x` and the copy says `dir:<person>/acme/x`), with no space or in that
    // same one: it is this folder, which in the copy's database entered acme. It takes that key, as it
    // would have taken it when put here into the space (if the usual one hasn't left this computer).
    if from_copy
        && exists
        && let Some(e) = space_in_dir_key(key)
        && let Some((id, space)) = by_path(db, origin_path)?
        && let old = key_of(db, id)?
        && dir_key_in_space(&old, e).as_deref() == Some(key)
        && (space.is_none() || space_key(db, space)? == e)
        && !crate::sync::in_general_repo(db, &old)
    {
        change_key(db, id, &old, key)?;
        return Ok(id);
    }
    if exists && by_path(db, origin_path)?.is_none() {
        if crate::sync::project_key_for(origin_path, person) == key {
            return register(db, origin_path);
        }
        let theirs = key.strip_prefix("dir:").and_then(|r| r.split_once('/')).map(|(p, _)| p);
        // (With the 1.9.0 key, that of this folder in the space the key says.)
        let is_this = |p: &str| {
            let k = crate::sync::project_key_for(origin_path, p);
            k == key || space_in_dir_key(key).and_then(|e| dir_key_in_space(&k, e)).as_deref() == Some(key)
        };
        if from_copy && theirs.is_some_and(is_this) {
            let now = now(db)?;
            db.execute("INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)", params![key, name_of(origin_path, key), origin_path, now])?;
            let id = db.last_insert_rowid();
            db.execute(
                "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, '', 0, ?4)",
                params![origin_path, comparable(origin_path), id, now],
            )?;
            return Ok(id);
        }
    }
    let free = |r: &str| -> Result<bool> { Ok(by_path(db, r)?.is_none() && !crate::project::exists(Path::new(r))) };
    let mut path = if origin_path.is_empty() { format!("[{key}]") } else { origin_path.to_string() };
    if !free(&path)? {
        path = format!("{origin_path} [{key}]");
    }
    let base = path.clone();
    let mut n = 2;
    while !free(&path)? {
        path = format!("{base} ({n})");
        n += 1;
    }
    let now = now(db)?;
    db.execute("INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)", params![key, name_of(origin_path, key), path, now])?;
    let id = db.last_insert_rowid();
    db.execute(
        "INSERT INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, '', 1, NULL)",
        params![path, comparable(&path), id],
    )?;
    Ok(id)
}

/// A project, as shown by `context`, `omoi projects` and Review.
#[derive(Clone, Debug, Serialize)]
pub struct Project {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub path: String,
    pub key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub memories: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_update: String,
    /// The most recent `status` memory, in one line.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status: String,
    /// Its folder no longer exists on this computer (and it isn't from another computer).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gone: bool,
    /// The git `origin` of one of its folders is no longer that of its key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub origin_changed: String,
    /// Its trades (android, web…: `trades`), the detected ones and those set by hand.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trades: Vec<String>,
}

/// The projects with memories (or the one asked for, even if it has none), from most to least active.
/// Without global. With `space`, only those of that space (`Some(None)`: those that have none).
pub fn list(db: &Connection, only: Option<i64>, space: Option<Option<i64>>, limit: usize) -> Result<Vec<Project>> {
    // How many and the latest, for all projects in one pass over the `memory_scope` index (with 50,000
    // memories, one subquery per project scanned the whole table once for each).
    let per_project: std::collections::HashMap<i64, (i64, String)> = {
        let mut q = db.prepare("SELECT project_id, count(*), max(updated_at) FROM memory WHERE project_id != 0 GROUP BY project_id")?;
        q.query_map([], |f| Ok((f.get(0)?, (f.get(1)?, f.get(2)?))))?.collect::<rusqlite::Result<_>>()?
    };
    let mut q = db.prepare(
        "SELECT p.id, p.name, p.path, p.key, COALESCE(s.key, ''),
                COALESCE((SELECT is_foreign FROM project_path WHERE path = p.path), 0),
                COALESCE((SELECT origin FROM project_path WHERE project_id = p.id AND is_foreign = 0 AND origin != ''
                            AND p.key LIKE 'git:%' AND 'git:' || origin != p.key LIMIT 1), ''),
                p.space_id
         FROM project p LEFT JOIN space s ON s.id = p.space_id
         WHERE p.id != 0 AND (?1 IS NULL OR p.id = ?1)",
    )?;
    // (project, its main path is from another computer, its space)
    let mut v: Vec<(Project, bool, Option<i64>)> = q
        .query_map(params![only], |f| {
            let id: i64 = f.get(0)?;
            let (memories, last_update) = per_project.get(&id).cloned().unwrap_or_default();
            Ok((
                Project {
                    id,
                    name: f.get(1)?,
                    path: f.get(2)?,
                    key: f.get(3)?,
                    space: f.get(4)?,
                    memories,
                    last_update,
                    status: String::new(),
                    gone: false,
                    origin_changed: f.get(6)?,
                    trades: Vec::new(),
                },
                f.get(5)?,
                f.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    v.retain(|(p, _, e)| (p.memories > 0 || only.is_some()) && space.is_none_or(|x| x == *e));
    v.sort_by(|a, b| b.0.last_update.cmp(&a.0.last_update).then(a.0.name.cmp(&b.0.name)));
    v.truncate(limit);
    // The most recent `status` of each project, in one pass over the table: the `LIKE` discards without
    // reading the JSON whatever doesn't carry the tag, and with `max`, SQLite gives the title of that same
    // row. `NOT INDEXED`: through the project index, SQLite went row by row to the table (210 ms with
    // 50,000 memories, versus 30 scanning it straight through).
    let statuses: std::collections::HashMap<i64, String> = {
        let mut q = db.prepare(
            "SELECT project_id, title, max(updated_at) FROM memory NOT INDEXED
             WHERE project_id != 0 AND tags LIKE ?1 AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?2)
             GROUP BY project_id",
        )?;
        let tag = crate::memory::STATUS;
        q.query_map(params![format!("%\"{tag}\"%"), tag], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut trades = crate::trades::of_projects(db)?;
    Ok(v.into_iter()
        .map(|(mut p, foreign, _)| {
            p.gone = !foreign && !p.path.is_empty() && readable(Path::new(&p.path)) && !Path::new(&p.path).is_dir();
            p.status = statuses.get(&p.id).cloned().unwrap_or_default();
            p.trades = trades.remove(&p.id).unwrap_or_default();
            p
        })
        .collect())
}

/// The project a person or an agent names: a folder, or a name or part of a name of the known
/// projects («balua»). Error if it matches none or matches several (with the list).
pub fn find(db: &Connection, text: &str) -> Result<i64> {
    find_in(db, text, &crate::memory::Scope::All)
}

/// [`find`] among the projects visible from `scope`: from a project or a space, those of its space
/// (or, without a space, those that have none); from a general folder, all of them. A name is only
/// searched among them (one from another client doesn't match nor appears in the error), and a folder
/// from another space is an error.
pub fn find_in(db: &Connection, text: &str, scope: &crate::memory::Scope) -> Result<i64> {
    let all = matches!(scope, crate::memory::Scope::All);
    let id = search_among(db, text, if all { None } else { Some(scope.space()) })?;
    anyhow::ensure!(
        all || space_of(db, id)? == scope.space(),
        tr!(
            "that project is in another space: from here you only reach the projects of this one",
            "ese proyecto es de otro espacio: desde aquí solo se llega a los proyectos de este"
        )
    );
    Ok(id)
}

/// `find`, with names only from the projects of this space (`Some(None)`: those that have none;
/// `None`: all).
fn search_among(db: &Connection, text: &str, space: Option<Option<i64>>) -> Result<i64> {
    let t = text.trim();
    anyhow::ensure!(!t.is_empty(), tr!("which project? give its folder or its name", "¿qué proyecto? di su carpeta o su nombre"));
    let looks_like_path = t.contains(['/', '\\']) || t == "." || t == "..";
    if (looks_like_path || is_dir(Path::new(t)))
        && let Ok(path) = of_requested_dir(Path::new(t))
    {
        if let Some((id, _)) = resolve(db, &path)? {
            return Ok(id);
        }
        anyhow::ensure!(!looks_like_path, tr!("no memories in the project of {path}", "el proyecto de {path} no tiene memorias"));
    }
    if looks_like_path && let Some((id, _)) = by_path(db, t)? {
        return Ok(id);
    }
    let all: Vec<(i64, String, String)> = {
        let filter = match space {
            None => String::new(),
            Some(Some(s)) => format!(" AND space_id = {s}"),
            Some(None) => " AND space_id IS NULL".to_string(),
        };
        let mut q = db.prepare(&format!("SELECT id, name, path FROM project WHERE id != 0{filter} ORDER BY name"))?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    // From most to least certain (`names`): the same name ignoring case, accents and separators
    // («Crónicas» = «cronicas», «social_publish» = «Social Publish»); a part of the name; and, if
    // nothing matches, one letter off («Omoikae»).
    let n = crate::names::normal(t);
    let exact: Vec<&(i64, String, String)> = all.iter().filter(|(_, name, _)| crate::names::normal(name) == n).collect();
    let partial: Vec<&(i64, String, String)> = all.iter().filter(|(_, name, _)| !n.is_empty() && crate::names::normal(name).contains(&n)).collect();
    // Two projects that only differ in case («Omoikane» and «omoikane»): if one is written exactly,
    // it is that one.
    let identical: Vec<&(i64, String, String)> = exact.iter().copied().filter(|(_, name, _)| name == t).collect();
    let chosen = if identical.len() == 1 {
        identical
    } else if !exact.is_empty() {
        exact
    } else if !partial.is_empty() {
        partial
    } else {
        all.iter().filter(|(_, name, _)| crate::names::similarity(name, t) == Some(crate::names::Similarity::OneLetter)).collect()
    };
    match chosen.as_slice() {
        [one] => Ok(one.0),
        [] => anyhow::bail!(tr!(
            "no project matches «{t}» (known: {})",
            "ningún proyecto encaja con «{t}» (hay: {})",
            all.iter().map(|x| x.1.as_str()).collect::<Vec<_>>().join(", ")
        )),
        several => anyhow::bail!(tr!(
            "«{t}» matches several projects, say which: {}",
            "«{t}» encaja con varios proyectos, di cuál: {}",
            several.iter().map(|x| format!("{} ({})", x.1, x.2)).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The key of a project by its id (empty if it doesn't exist or is global).
pub fn key_of(db: &Connection, project: i64) -> Result<String> {
    Ok(db.query_row("SELECT key FROM project WHERE id = ?1", [project], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// The space of a project (`None` if it has none, or if the project doesn't exist).
pub fn space_of(db: &Connection, project: i64) -> Result<Option<i64>> {
    Ok(db.query_row("SELECT space_id FROM project WHERE id = ?1", [project], |f| f.get(0)).optional()?.flatten())
}

/// A space, as shown by `omoi space list`.
#[derive(Clone, Debug, Serialize)]
pub struct Space {
    #[serde(skip)]
    pub id: i64,
    pub key: String,
    pub name: String,
    pub projects: Vec<String>,
    pub memories: i64,
}

/// The key of a space from its name: lowercase, letters, numbers and hyphens.
pub fn space_key_from_name(name: &str) -> String {
    let k: String = name.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    k.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-")
}

/// Creates a space. Error if there is already one with the same key.
pub fn create_space(db: &Connection, name: &str) -> Result<i64> {
    let key = space_key_from_name(name);
    anyhow::ensure!(!key.is_empty(), tr!("the space needs a name", "el espacio necesita un nombre"));
    let found: Option<i64> = db.query_row("SELECT id FROM space WHERE key = ?1", [&key], |f| f.get(0)).optional()?;
    anyhow::ensure!(found.is_none(), tr!("the space «{key}» already exists", "el espacio «{key}» ya existe"));
    db.execute("INSERT INTO space(key, name, created_at) VALUES (?1, ?2, ?3)", params![key, name.trim(), now(db)?])?;
    Ok(db.last_insert_rowid())
}

/// The space of a key that comes from another computer, creating it if it isn't here.
pub fn space_of_key(db: &Connection, key: &str) -> Result<Option<i64>> {
    if key.is_empty() {
        return Ok(None);
    }
    if let Some(id) = db.query_row("SELECT id FROM space WHERE key = ?1", [key], |f| f.get(0)).optional()? {
        return Ok(Some(id));
    }
    db.execute("INSERT INTO space(key, name, created_at) VALUES (?1, ?1, ?2)", params![key, now(db)?])?;
    Ok(Some(db.last_insert_rowid()))
}

/// The space a person or an agent names, by its key or its name. Error (with the list) if there is none.
pub fn find_space(db: &Connection, text: &str) -> Result<i64> {
    let t = text.trim();
    let id: Option<i64> = db
        .query_row(
            "SELECT id FROM space WHERE key = ?1 OR lower(name) = lower(?2) ORDER BY key = ?1 DESC LIMIT 1",
            params![space_key_from_name(t), t],
            |f| f.get(0),
        )
        .optional()?;
    id.ok_or_else(|| {
        let known: Vec<String> = db
            .prepare("SELECT key FROM space ORDER BY key")
            .and_then(|mut q| q.query_map([], |f| f.get(0))?.collect())
            .unwrap_or_default();
        anyhow::anyhow!(tr!(
            "no space «{t}» (there are: {}); create it with `omoi space add {t}`",
            "no hay ningún espacio «{t}» (hay: {}); créalo con `omoi space add {t}`",
            if known.is_empty() { "-".to_string() } else { known.join(", ") }
        ))
    })
}

/// The key of a space by its id (empty if there is none).
pub fn space_key(db: &Connection, id: Option<i64>) -> Result<String> {
    let Some(id) = id else { return Ok(String::new()) };
    Ok(db.query_row("SELECT key FROM space WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// The spaces, with their projects and how many memories they have (those of their projects and their own).
pub fn spaces(db: &Connection) -> Result<Vec<Space>> {
    let mut q = db.prepare(
        "SELECT s.id, s.key, s.name,
                (SELECT count(*) FROM memory m WHERE m.space_id = s.id OR m.project_id IN (SELECT id FROM project WHERE space_id = s.id))
         FROM space s ORDER BY s.key",
    )?;
    let v: Vec<Space> = q
        .query_map([], |f| Ok(Space { id: f.get(0)?, key: f.get(1)?, name: f.get(2)?, projects: Vec::new(), memories: f.get(3)? }))?
        .collect::<rusqlite::Result<_>>()?;
    let mut p = db.prepare("SELECT name FROM project WHERE space_id = ?1 ORDER BY name")?;
    v.into_iter()
        .map(|mut e| {
            e.projects = p.query_map([e.id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(e)
        })
        .collect()
}

/// Puts a project into a space (or takes it out, with `None`). The project gets a new stamp
/// (`project.hlc`), with which the sync carries its space to the other computers in a `project` line;
/// its memories don't change stamp (if they did, an old version from here would beat a newer one from
/// another computer just for having been moved). If it was paused (`Pause`), this is the person's
/// decision: it leaves the pause, with a stamp later than that of the line that paused it (so that line
/// doesn't pause it again). Returns how many memories it has.
pub fn assign_space(db: &Connection, project: i64, space: Option<i64>) -> Result<usize> {
    anyhow::ensure!(project != 0, tr!("global memories have no project to put in a space", "las memorias globales no tienen proyecto que meter en un espacio"));
    let (before, previous, pause): (Option<i64>, String, String) =
        db.query_row("SELECT space_id, hlc, paused FROM project WHERE id = ?1", [project], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    let paused = !pause.is_empty();
    // The first time a folder without a remote enters a space without its key having left this
    // computer, the key starts carrying the space (`key_with_space`).
    let previous = match (before, space, previous.is_empty() && !paused) {
        (None, Some(e), true) => in_a_transaction(db, || key_with_space(db, project, e))?.unwrap_or(previous),
        _ => previous,
    };
    if before != space || previous.is_empty() || paused {
        let previous = match Pause::parse(&pause) {
            Some(p) if crate::clock::is_after(&p.hlc, &previous) => p.hlc,
            _ => previous,
        };
        let hlc = crate::clock::stamp_after(db, &previous)?;
        db.execute("UPDATE project SET space_id = ?2, hlc = ?3, paused = '' WHERE id = ?1", params![project, space, hlc])?;
    }
    Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [project], |f| f.get::<_, i64>(0))? as usize)
}

/// `f` in a transaction: the caller's if there already is one (with a `SAVEPOINT`), or an `IMMEDIATE`
/// one (a deferred one that reads and then writes can give `BUSY_SNAPSHOT` without waiting: see `db`).
fn in_a_transaction<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let (begin, commit, rollback) = if db.is_autocommit() {
        ("BEGIN IMMEDIATE", "COMMIT", "ROLLBACK")
    } else {
        ("SAVEPOINT clave_con_espacio", "RELEASE clave_con_espacio", "ROLLBACK TO clave_con_espacio; RELEASE clave_con_espacio")
    };
    db.execute_batch(begin)?;
    match f() {
        Ok(v) => {
            db.execute_batch(commit)?;
            Ok(v)
        }
        Err(e) => {
            let _ = db.execute_batch(rollback);
            Err(e)
        }
    }
}

/// The `dir:` key of a folder without a remote that enters the space `space` (its key):
/// `dir:<person>/<space>/<folder>` (1.9.0). `None` if it isn't one of the usual `dir:` keys
/// (`dir:<person>/<folder>`, with or without `~2`): a `git:` one, or one that already carries a space.
pub fn dir_key_in_space(key: &str, space: &str) -> Option<String> {
    let (person, name) = key.strip_prefix("dir:")?.split_once('/')?;
    if name.is_empty() || name.contains('/') || space.is_empty() || space.contains('/') {
        return None;
    }
    // The `~2` of the second folder with that name on this computer: with the space in front it is no
    // longer needed (if it still clashes, `key_with_space` adds another).
    let name = match name.rsplit_once('~') {
        Some((n, d)) if !n.is_empty() && !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => n,
        _ => name,
    };
    Some(format!("dir:{person}/{space}/{name}"))
}

/// The space of a `dir:<person>/<space>/<folder>` key (the 1.9.0 ones); `None` for the rest.
pub fn space_in_dir_key(key: &str) -> Option<&str> {
    let (_, rest) = key.strip_prefix("dir:")?.split_once('/')?;
    let (space, name) = rest.split_once('/')?;
    (!space.is_empty() && !name.is_empty() && !name.contains('/')).then_some(space)
}

/// Two folders without a remote with the same name, from the same person, in two clients (acme's `web`
/// on one computer and beta's on another) had the same key `dir:<person>/web` and each paused the
/// other. Since 1.9.0, when a folder without a remote enters a space for the first time (here it wasn't
/// in any and never was: empty `project.hlc`), and its key hasn't left this computer yet, the key
/// becomes `dir:<person>/<space>/<folder>`: acme's and beta's no longer match. Hasn't left: without a
/// space, only the general repository carries it, so it is enough that it isn't in its clone
/// (`sync::in_general_repo`); if it already is, other computers know it by that key and it stays as it
/// was (the usual, and the pause warning).
///
/// Earlier keys never change because of this, and what comes from outside changes none: it only
/// happens when the person decides it here (`space assign`, `projects move --space`, `merge`). If the
/// new key is already that of a project that has no folder of its own here and is in that same space
/// (it came from that space's repository: it is this same folder on another computer of the person),
/// they are joined (its content moves to this one, without changing space); otherwise, the new one
/// gets a suffix (`~2`). Returns the space stamp of the one that was joined, so the new one goes after it.
fn key_with_space(db: &Connection, project: i64, space: i64) -> Result<Option<String>> {
    let key = key_of(db, project)?;
    let e = space_key(db, Some(space))?;
    let Some(base) = dir_key_in_space(&key, &e) else { return Ok(None) };
    if own_folders(db, project)? == 0 || crate::sync::in_general_repo(db, &key) {
        return Ok(None);
    }
    let mut k = base.clone();
    let mut n = 2;
    let mut stamp = None;
    loop {
        let other: Option<(i64, Option<i64>, i64, String, String)> = db
            .query_row(
                "SELECT p.id, p.space_id, (SELECT count(*) FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0), p.hlc, p.paused FROM project p WHERE p.key = ?1",
                [&k],
                |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)),
            )
            .optional()?;
        match other {
            None => break,
            Some((o, Some(s), 0, hlc, pause)) if s == space && pause.is_empty() => {
                join(db, o, project)?;
                stamp = Some(hlc);
                break;
            }
            Some(_) => {
                k = format!("{base}~{n}");
                n += 1;
            }
        }
    }
    change_key(db, project, &key, &k)?;
    Ok(Some(stamp.unwrap_or_default()))
}

/// The key of project `project` changes from `old` to `new` (`key_with_space`, or a copy's in
/// `of_key`): along with it, what is in its trash (it goes by key: otherwise it would return to a
/// ghost project) and the `projects` list of the repositories that chose it (otherwise it would stop
/// syncing).
fn change_key(db: &Connection, project: i64, old: &str, new: &str) -> Result<()> {
    db.execute("UPDATE project SET key = ?2 WHERE id = ?1", params![project, new])?;
    db.execute(
        "UPDATE forgotten SET memory = json_set(memory, '$.project_key', ?2) WHERE json_valid(memory) AND json_extract(memory, '$.project_key') = ?1",
        params![old, new],
    )?;
    if let Some(path) = db.path().filter(|p| !p.is_empty()) {
        crate::sync::rename_key_in_configs(Path::new(path), old, new)?;
    }
    Ok(())
}

/// A paused project: a `project` line (from another computer, or written by hand by whoever has a
/// repository's key) says it is in another space. That line changes nothing: until the person decides
/// here (`omoi projects move` or `omoi projects keep`), its memories are not pushed to any repository
/// nor are those that come from it accepted. It is local (`project.paused`, in JSON): it doesn't travel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    /// The space the line says (its key; empty: none).
    #[serde(default)]
    pub space: String,
    /// The line only says another computer moved it to a space, without saying which (the one that goes
    /// through the general repository, which doesn't name clients).
    #[serde(default)]
    pub unknown: bool,
    /// Not set by a `project` line: a folder from here joined a project that was in a space (`space`)
    /// only because that space's repository says so (here it had no folder of its own). It leaves the
    /// pause by confirming (`omoi projects keep`) or by taking it out of the space.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claimed: bool,
    /// That line's stamp.
    #[serde(default)]
    pub hlc: String,
    /// The computer of its stamp (the one that moved it).
    #[serde(default)]
    pub machine: String,
    /// When it was paused here.
    #[serde(default)]
    pub at: String,
}

impl Pause {
    /// The pause stored in `project.paused` (`None` if there is none, or it can't be parsed).
    pub fn parse(text: &str) -> Option<Pause> {
        if text.is_empty() { None } else { serde_json::from_str(text).ok() }
    }
}

/// Pauses the project (without touching its space or its stamp).
pub fn pause(db: &Connection, project: i64, pause: &Pause) -> Result<()> {
    let p = Pause { at: now(db)?, ..pause.clone() };
    db.execute("UPDATE project SET paused = ?2 WHERE id = ?1", params![project, serde_json::to_string(&p)?])?;
    Ok(())
}

/// Pauses (`Pause::claimed`) a project of a space that a folder from here has just joined without this
/// computer having any: that space's repository says it is theirs, but it may not be (a personal
/// library a colleague uses at the client, or a line written on purpose to grab whatever is saved in
/// that folder). If it was already paused, it keeps the pause it had.
pub fn pause_for_claim(db: &Connection, project: i64) -> Result<()> {
    let (space, hlc, paused): (Option<i64>, String, String) =
        db.query_row("SELECT space_id, hlc, paused FROM project WHERE id = ?1", [project], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    if space.is_none() || !paused.is_empty() {
        return Ok(());
    }
    let machine = crate::clock::parts(&hlc).2.to_string();
    pause(db, project, &Pause { space: space_key(db, space)?, claimed: true, hlc, machine, ..Default::default() })
}

/// With nobody in front (`OMOIKANE_NONINTERACTIVE=1`), whether a project that its space's repository
/// claims enters it without a pause when a folder of it appears here: only if this computer joined that
/// repository via `OMOIKANE_SYNC_CODE` (the variable is already the decision: a CI starts each time from
/// a new database and there is nobody to say `omoi projects keep`) and if the `origin` of a folder here
/// is exactly that of its `git:` key (see `sync::adopts_from_env`). With a person in front, or
/// without that variable, it is paused as always.
pub fn adopts_without_pause(db: &Connection, project: i64) -> Result<bool> {
    let space = space_key(db, space_of(db, project)?)?;
    adopts_into(db, project, &space)
}

/// Like `adopts_without_pause`, for entering the space `space` (its key) a project that isn't in any here
/// yet.
pub fn adopts_into(db: &Connection, project: i64, space: &str) -> Result<bool> {
    let key = key_of(db, project)?;
    if !crate::sync::adopts_from_env(db, &key, space) {
        return Ok(false);
    }
    let mut q = db.prepare("SELECT origin FROM project_path WHERE project_id = ?1 AND is_foreign = 0")?;
    let origins: Vec<String> = q.query_map([project], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(origins.iter().any(|o| !o.is_empty() && format!("git:{o}") == key))
}

/// A paused project, as shown by `omoi sync`, `doctor`, `review` and Review.
#[derive(Clone, Debug, Serialize)]
pub struct PausedProject {
    pub key: String,
    pub name: String,
    /// Its space here (its key; empty: none).
    pub here: String,
    #[serde(flatten)]
    pub pause: Pause,
}

/// The paused projects, by name.
pub fn paused_projects(db: &Connection) -> Result<Vec<PausedProject>> {
    let mut q = db.prepare(
        "SELECT p.key, p.name, COALESCE((SELECT key FROM space WHERE id = p.space_id), ''), p.paused FROM project p WHERE p.paused != '' ORDER BY p.name, p.key",
    )?;
    let rows: Vec<(String, String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|(key, name, here, t)| PausedProject { key, name, here, pause: Pause::parse(&t).unwrap_or_default() })
        .collect())
}

/// Whether the project with this key is paused.
pub fn is_paused(db: &Connection, key: &str) -> Result<bool> {
    Ok(db.query_row("SELECT paused != '' FROM project WHERE key = ?1", [key], |f| f.get(0)).optional()?.unwrap_or(false))
}

/// `omoi projects keep`: the project stays in its space and leaves the pause; its stamp moves ahead
/// of that of the line that paused it, so it doesn't pause it again (and the other computers know it
/// stayed where it was here). `None` if it wasn't paused; otherwise, its space (its key).
pub fn keep(db: &Connection, project: i64) -> Result<Option<String>> {
    let pause: Option<String> = db.query_row("SELECT paused FROM project WHERE id = ?1", [project], |f| f.get(0)).optional()?;
    if pause.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }
    let space = space_of(db, project)?;
    assign_space(db, project, space)?;
    Ok(Some(space_key(db, space)?))
}

/// A new stamp for each memory of the project (without touching its content or `content_hlc`): that
/// way what changed in its key (when changing person) travels. Not the space: it travels in the
/// `project` line.
fn restamp(db: &Connection, project: i64) -> Result<usize> {
    let ids: Vec<String> = {
        let mut q = db.prepare("SELECT id FROM memory WHERE project_id = ?1")?;
        q.query_map([project], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    restamp_these(db, &ids)?;
    Ok(ids.len())
}

/// A new stamp for each of these memories.
fn restamp_these(db: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        let hlc: String = db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let new = crate::clock::stamp_after(db, &hlc)?;
        db.execute("UPDATE memory SET hlc = ?2 WHERE id = ?1", params![id, new])?;
    }
    Ok(())
}

/// The `dir:` keys of the projects from here (those with some folder of their own) change from person
/// `old` to `new` (`dir:<old>/x` → `dir:<new>/x`; with an empty `old`, the `dir:/x` ones that were left
/// without a person), and their memories change stamp so they travel with the right key. If the new key
/// already belongs to a project that only exists here through another computer and is in the same
/// space, it is the same project and they are joined; otherwise, the new one gets a suffix (`~2`).
/// Returns how many projects changed.
pub fn change_person(db: &Connection, old: &str, new: &str) -> Result<usize> {
    if old == new {
        return Ok(0);
    }
    let prefix = format!("dir:{old}/");
    let projects: Vec<(i64, String, Option<i64>)> = {
        let mut q = db.prepare(
            "SELECT p.id, p.key, p.space_id FROM project p
             WHERE substr(p.key, 1, length(?1)) = ?1 AND EXISTS (SELECT 1 FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0)",
        )?;
        q.query_map([&prefix], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, key, space) in &projects {
        let base = format!("dir:{new}/{}", &key[prefix.len()..]);
        let mut k = base.clone();
        let mut n = 2;
        loop {
            let other: Option<(i64, Option<i64>, i64)> = db
                .query_row(
                    "SELECT p.id, p.space_id, (SELECT count(*) FROM project_path pp WHERE pp.project_id = p.id AND pp.is_foreign = 0) FROM project p WHERE p.key = ?1",
                    [&k],
                    |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)),
                )
                .optional()?;
            match other {
                None => break,
                Some((o, e, 0)) if e == *space => {
                    join(db, o, *id)?;
                    break;
                }
                Some(_) => {
                    k = format!("{base}~{n}");
                    n += 1;
                }
            }
        }
        db.execute("UPDATE project SET key = ?2 WHERE id = ?1", params![id, k])?;
        restamp(db, *id)?;
    }
    Ok(projects.len())
}

/// The memories, folders and uses of project `from` move to `to`, and `from` disappears (without
/// touching the space of `to` or the stamps: the caller decides that).
fn join(db: &Connection, from: i64, to: i64) -> Result<usize> {
    let n = db.execute("UPDATE memory SET project_id = ?2 WHERE project_id = ?1", params![from, to])?;
    db.execute("UPDATE project_path SET project_id = ?2 WHERE project_id = ?1", params![from, to])?;
    db.execute("UPDATE recall_log SET project_id = ?2 WHERE project_id = ?1", params![from, to])?;
    crate::diary::move_project(db, from, to)?;
    db.execute(
        "INSERT INTO memory_use(memory_id, project_id, veces, ultima) SELECT memory_id, ?2, veces, ultima FROM memory_use WHERE project_id = ?1
         ON CONFLICT(memory_id, project_id) DO UPDATE SET veces = veces + excluded.veces, ultima = max(ultima, excluded.ultima)",
        params![from, to],
    )?;
    db.execute("DELETE FROM memory_use WHERE project_id = ?1", [from])?;
    db.execute("DELETE FROM project WHERE id = ?1", [from])?;
    Ok(n)
}

/// Error if joining `from` into `to` would move memories of either one to another space: those of
/// `from` (if it has any) would end up in the space of `to`, and those of `to`, if it has no space, in
/// that of `from`. That way one client's content doesn't go to another's repository (nor to the
/// personal one) through a `merge` or a `move`.
fn same_spaces(db: &Connection, from: i64, to: i64) -> Result<()> {
    let (sf, st) = (space_of(db, from)?, space_of(db, to)?);
    if sf == st {
        return Ok(());
    }
    let count = |p: i64| -> Result<i64> { Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [p], |f| f.get(0))?) };
    if count(from)? == 0 && (st.is_some() || count(to)? == 0) {
        return Ok(());
    }
    let name = |e: Option<i64>| -> Result<String> {
        Ok(match e {
            Some(_) => format!("«{}»", space_key(db, e)?),
            None => tr!("no space", "ninguno"),
        })
    };
    anyhow::bail!(tr!(
        "the projects are in different spaces ({} → {}): their memories would go to another space's sync repository. Add --force if that is what you want",
        "los proyectos están en espacios distintos ({} → {}): sus memorias irían al repositorio de otro espacio. Añade --force si es lo que quieres",
        name(sf)?,
        name(st)?
    ))
}

/// Merges project `from` into `to`: its memories (with their id, history and verdicts) and its folders
/// move to `to`, and `from` disappears. If both had a `status` memory, the newest stays and the other
/// is superseded by it. Between projects of different spaces, only with `force`. Returns how many
/// memories were moved.
pub fn merge(db: &mut Connection, from: i64, to: i64, force: bool) -> Result<usize> {
    anyhow::ensure!(from != 0 && to != 0, tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    anyhow::ensure!(from != to, tr!("it's the same project", "es el mismo proyecto"));
    if !force {
        same_spaces(db, from, to)?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let status = |p: i64| -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT id FROM memory WHERE project_id = ?1 AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?2) ORDER BY updated_at DESC LIMIT 1",
                params![p, crate::memory::STATUS],
                |f| f.get(0),
            )
            .optional()?)
    };
    let (old_status, new_status) = (status(from)?, status(to)?);
    // `to` takes the space of `from` if it had none (with a new stamp: it travels in its `project` line).
    if space_of(&tx, to)?.is_none()
        && let Some(e) = space_of(&tx, from)?
    {
        assign_space(&tx, to, Some(e))?;
    }
    // The memories of `from` now travel with the key of `to`: new stamp (those of `to` don't change).
    let moved: Vec<String> = {
        let mut q = tx.prepare("SELECT id FROM memory WHERE project_id = ?1")?;
        q.query_map([from], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let n = join(&tx, from, to)?;
    restamp_these(&tx, &moved)?;
    if let (Some(a), Some(b)) = (old_status, new_status) {
        let (older, newer): (String, String) = tx.query_row(
            "SELECT CASE WHEN a.updated_at <= b.updated_at THEN a.id ELSE b.id END, CASE WHEN a.updated_at <= b.updated_at THEN b.id ELSE a.id END
             FROM memory a, memory b WHERE a.id = ?1 AND b.id = ?2",
            params![a, b],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )?;
        crate::verdicts::set(&tx, &older, &newer, crate::verdicts::SUPERSEDED, "projects merged: the newer status stays", "omoikane")?;
    }
    tx.commit()?;
    Ok(n)
}

/// `omoi projects move`: the project now lives in `folder`. It keeps its key (the other computers know
/// it) and adds the folder; if the folder was already another project, that one is merged into this
/// one (if it is from another space, only with `force`).
pub fn move_to(db: &mut Connection, project: i64, folder: &str, force: bool) -> Result<usize> {
    anyhow::ensure!(project != 0, tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    let n = match by_path(db, folder)? {
        Some((other, _)) if other != project => merge(db, other, project, force)?,
        _ => 0,
    };
    db.execute(
        "INSERT OR REPLACE INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, 0, ?5)",
        params![folder, comparable(folder), project, origin(folder), now(db)?],
    )?;
    db.execute("UPDATE project SET path = ?2, name = ?3 WHERE id = ?1", params![project, folder, name_of(folder, "")])?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_root_from_a_subfolder() {
        let base = crate::common::TempDir::new("proyecto");
        let sub = base.join("repo").join("src").join("dentro");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo").join(".git")).unwrap();
        let from_sub = of_dir(&sub).unwrap();
        let from_root = of_dir(&base.join("repo")).unwrap();
        assert_eq!(from_sub, from_root);
        assert!(from_root.ends_with("repo"));
        // Without git, the folder itself.
        let loose = base.join("suelta");
        std::fs::create_dir_all(&loose).unwrap();
        assert!(of_dir(&loose).unwrap().ends_with("suelta"));
    }

    #[test]
    fn dir_key_with_space() {
        assert_eq!(dir_key_in_space("dir:ana/web", "acme").as_deref(), Some("dir:ana/acme/web"));
        assert_eq!(dir_key_in_space("dir:ana/web~2", "acme").as_deref(), Some("dir:ana/acme/web"));
        assert_eq!(dir_key_in_space("dir:ana/v~1.2", "acme").as_deref(), Some("dir:ana/acme/v~1.2"));
        assert_eq!(dir_key_in_space("dir:ana/acme/web", "beta"), None, "ya lleva espacio");
        assert_eq!(dir_key_in_space("git:github.com/acme/web", "acme"), None);
        assert_eq!(dir_key_in_space("dir:ana/web", ""), None);
        assert_eq!(space_in_dir_key("dir:ana/acme/web"), Some("acme"));
        assert_eq!(space_in_dir_key("dir:ana/acme/web~2"), Some("acme"));
        assert_eq!(space_in_dir_key("dir:ana/web"), None);
        assert_eq!(space_in_dir_key("git:github.com/acme/web"), None);
    }

    #[test]
    fn general_folder() {
        let base = crate::common::TempDir::new("general");
        let home = base.join("usuario");
        std::fs::create_dir_all(home.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(home.join("suelta")).unwrap();
        let t = |p: &Path| of_dir(p).unwrap();
        let home_c = PathBuf::from(t(&home));
        assert!(general_in(&t(&home), &home_c));
        assert!(general_in(&t(&base), &home_c), "una carpeta por encima del usuario también");
        assert!(!general_in(&t(&home.join("repo")), &home_c));
        assert!(!general_in(&t(&home.join("suelta")), &home_c), "una carpeta suelta sigue siendo su proyecto");
        assert!(!general_in("", &home_c));
    }

    #[test]
    fn repo_of_a_path() {
        let base = crate::common::TempDir::new("rutas");
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let home = PathBuf::from(of_dir(&base).unwrap());
        let expected = of_dir(&repo).unwrap();
        let r = |s: &str| repo_of_path(s, &home);
        let inside = repo.join("src").join("no-existe-aun.rs");
        assert_eq!(r(&format!("{}.", inside.display())), Some(expected.clone()));
        assert_eq!(r(&repo.display().to_string()), Some(expected.clone()));
        assert_eq!(r("~/web/src"), Some(expected));
        assert_eq!(r(&base.display().to_string()), None, "fuera de un repositorio");
        assert_eq!(r("src/main.rs"), None, "las relativas no dicen el proyecto");
        assert_eq!(r(&format!("{}?", repo.display())), Some(of_dir(&repo).unwrap()));
        let clone = base.join(".claude").join("plugins").join("x");
        std::fs::create_dir_all(clone.join(".git")).unwrap();
        assert_eq!(r(&clone.display().to_string()), None, "un clon en una carpeta oculta no es un proyecto");
    }

    #[test]
    fn paths_with_spaces() {
        let base = crate::common::TempDir::new("espacios");
        let repo = base.join("the codex");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("app")).unwrap();
        let file = repo.join("app").join("x.kt");
        let text = format!("El fallo está en `{}` y en {}. Lo vi ayer.", file.display(), repo.join("app").display());
        let home = PathBuf::from(of_dir(&base).unwrap());
        let v = paths_in(&text, &home);
        assert!(v.contains(&file.display().to_string()), "{v:?}");
        assert!(v.contains(&repo.join("app").display().to_string()), "{v:?}");
        let expected = of_dir(&repo).unwrap();
        assert!(v.iter().all(|r| repo_of_path(r, &home) == Some(expected.clone())), "{v:?}");
        // Without the repository, the loose word is not enough.
        assert_eq!(repo_of_path(&base.join("the").display().to_string(), &home), None);
    }

    #[test]
    fn paths_relative_to_the_repo() {
        let base = crate::common::TempDir::new("relativas");
        let repo = base.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(base.join("fuera")).unwrap();
        let home = PathBuf::from(of_dir(&base).unwrap());
        let root = of_dir(&repo).unwrap();
        let r = |s: &Path| relative_to(&s.display().to_string(), &root, &home);
        assert_eq!(r(&repo.join("src").join("pages").join("index.astro")), "src/pages/index.astro", "aunque aún no exista");
        assert_eq!(r(&repo.join("src")), "src");
        let outside = base.join("fuera").join("a.txt").display().to_string();
        assert_eq!(relative_to(&outside, &root, &home), outside, "fuera del repo, tal cual");
        assert_eq!(relative_to("src/main.rs", &root, &home), "src/main.rs");
        assert_eq!(relative_to("~/web/src/a.rs", &root, &home), "src/a.rs");
    }

    #[test]
    fn worktree_with_git_file() {
        // In a worktree, `.git` is a file, not a folder.
        let base = crate::common::TempDir::new("worktree");
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::fs::write(base.join(".git"), "gitdir: /otro/sitio").unwrap();
        assert_eq!(of_dir(&base.join("a")).unwrap(), of_dir(&base).unwrap());
    }

    /// A network path (an agent's `project`, a website's `?dir=`) is not looked at: `of_dir`, `canonical`
    /// and `find` reject it without touching the disk. With 10.255.255.1 (nobody behind it), looking
    /// at it took about 21 s to fail and along the way handed over the person's NTLM hash.
    #[test]
    fn a_network_path_is_not_looked_at() {
        let t = crate::common::TempDir::new("red");
        let db = crate::db::open(&t.join("o.db")).unwrap();
        for r in [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\/10.255.255.1\x", r"\\?\UNC\10.255.255.1\x", r"\??\UNC\10.255.255.1\x", "/??/UNC/10.255.255.1/x", r"\\.\pipe\x"] {
            let start = std::time::Instant::now();
            assert!(!is_local_path(Path::new(r)), "{r}");
            assert!(format!("{}", of_dir(Path::new(r)).unwrap_err()).contains("network"), "{r}");
            assert!(of_given_folder(Path::new(r)).is_err() && canonical(Path::new(r)).is_err(), "{r}");
            assert!(find(&db, r).is_err(), "{r}");
            assert!(!is_dir(Path::new(r)) && !exists(Path::new(r)), "{r}");
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "{r}: {:?}", start.elapsed());
        }
        assert!(is_local_path(Path::new(r"C:\x\??\y")) && is_local_path(Path::new("src/a.rs")));
    }

    /// `project` (MCP) with the session's own network folder, or the root of its repository, is accepted
    /// written any way; another folder of the same share, one inside it, with `..` or from another
    /// server, is not, and without touching the disk (nobody answers at 10.255.255.1). Only on Windows:
    /// elsewhere there are no UNC paths.
    #[cfg(windows)]
    #[test]
    fn project_from_the_sessions_network_folder() {
        let start = std::time::Instant::now();
        note_work_folder(Path::new(r"\\10.255.255.1\sesion-prueba\repo\src"));
        assert_eq!(comparison_form(Path::new(r"\\?\UNC\10.255.255.1\Sesion-Prueba\repo\src\")).as_deref(), Some(r"\\10.255.255.1\sesion-prueba\repo\src"));
        assert_eq!(comparison_form(Path::new("//10.255.255.1/sesion-prueba//repo/src")).as_deref(), Some(r"\\10.255.255.1\sesion-prueba\repo\src"));
        for r in [r"\\10.255.255.1\sesion-prueba\repo\src\..", r"\\10.255.255.1\sesion-prueba\.\repo", r"\\10.255.255.1", r"C:\x", r"\\.\pipe\x"] {
            assert_eq!(comparison_form(Path::new(r)), None, "{r}");
        }
        for r in [
            r"\\10.255.255.1\sesion-prueba\otro",
            r"\\10.255.255.1\sesion-prueba\repo\src\dentro",
            r"\\10.255.255.1\sesion-prueba\repo\src\..\..\otro",
            r"\\10.255.255.2\sesion-prueba\repo\src",
            r"\\10.255.255.1\otro-recurso\repo\src",
            r"\??\UNC\10.255.255.1\sesion-prueba\repo\src",
        ] {
            assert!(of_requested_dir(Path::new(r)).is_err(), "{r}");
            assert!(of_given_folder(Path::new(r)).is_err(), "{r}");
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
        // With a real network folder (the drive's administrative share, if this computer has it): the
        // work one and the root of its repository, written another way, give its project.
        let t = crate::common::TempDir::new("project-red");
        std::fs::create_dir_all(t.join("repo").join(".git")).unwrap();
        std::fs::create_dir_all(t.join("repo").join("src")).unwrap();
        let local = of_dir(&t.join("repo").join("src")).unwrap();
        let (drive, rest) = local.split_at(2);
        let net = PathBuf::from(format!(r"\\localhost\{}$\{}", &drive[..1], rest.trim_start_matches('\\')));
        if net.is_dir() {
            let session = of_trusted_dir(&net.join("src")).unwrap();
            let net_root = format!(r"\\?\UNC\LOCALHOST\{}$\{}/", drive[..1].to_lowercase(), rest.trim_start_matches('\\').replace('\\', "/"));
            assert_eq!(of_requested_dir(Path::new(&net_root)).unwrap(), session, "{net_root}");
            assert_eq!(of_requested_dir(&net.join("src")).unwrap(), session);
            assert!(of_requested_dir(&net.join("src").join("otra")).is_err());
        }
    }

    /// `\\?\C:\…` (extended path of a drive) is local; the other device prefixes are not. The shape is
    /// tested on every system; `is_local_path`, only on Windows (elsewhere, the backslash is just another
    /// character and `\\?\C:` would be an odd name on this computer, which is still rejected).
    #[test]
    fn the_extended_path_of_a_drive() {
        for r in [r"\\?\C:\Users\a\.omoikane", r"\\?\d:\x", r"\\?\C:", r"\\?\C:\", r"\\?\C:\a..b\c"] {
            assert!(extended_disk(r), "{r}");
        }
        for r in [
            r"\\?\UNC\10.255.255.1\x",
            r"\\?\unc\10.255.255.1\x",
            r"\\?\Volume{0b1c2d3e-0000-0000-0000-000000000000}\x",
            r"\\?\GLOBALROOT\Device\Mup\10.255.255.1\x",
            r"\\?\C:\..\..\UNC\10.255.255.1\x",
            r"\\?\C:\a/../../x",
            r"\\?\CC:\x",
            r"\\?\C:x",
            r"\\?\1:\x",
            "//?/C:/x",
            r"\\.\C:\x",
            r"\??\C:\x",
            r"\\?\\C:\x",
            r"C:\x",
        ] {
            assert!(!extended_disk(r), "{r}");
        }
        #[cfg(windows)]
        {
            assert!(is_local_path(Path::new(r"\\?\C:\Users\a\.omoikane")));
            for r in [r"\\?\UNC\10.255.255.1\x", r"\\?\C:\..\..\UNC\10.255.255.1\x", r"\??\UNC\10.255.255.1\x", r"\\.\pipe\x", r"\\?\GLOBALROOT\Device\Mup\10.255.255.1\x"] {
                assert!(!is_local_path(Path::new(r)), "{r}");
            }
            // A real folder, as `canonicalize` gives it: `of_dir` reads it.
            let t = crate::common::TempDir::new("extendida");
            let c = std::fs::canonicalize(&*t).unwrap();
            assert!(c.to_string_lossy().starts_with(r"\\?\"));
            assert_eq!(of_dir(&c).unwrap(), of_dir(&t).unwrap());
        }
    }

    /// The folder being worked in can be on the network: its share (`\\server\share`) is legible for
    /// reading the repository, the one next to it isn't, and what comes from outside is still not looked
    /// at even if it is from that same share. Without touching the disk (nobody answers at 10.255.255.1).
    #[test]
    fn the_work_share_is_trusted() {
        let start = std::time::Instant::now();
        let repo = Path::new(r"\\10.255.255.1\equipo-prueba\repo");
        assert!(!readable(repo), "aún no es de trabajo");
        note_work_folder(&repo.join("src"));
        if cfg!(windows) {
            for r in [r"\\10.255.255.1\equipo-prueba\repo", r"\\10.255.255.1\EQUIPO-PRUEBA\repo\.git\HEAD", r"\\?\UNC\10.255.255.1\equipo-prueba\otro\.git"] {
                assert!(readable(Path::new(r)), "{r}");
                assert!(if_readable(PathBuf::from(r)).is_some(), "{r}");
            }
        }
        // Another share on the same server, another server, `\??\UNC\…` and `\\.\…`: no.
        for r in [r"\\10.255.255.1\otro\repo", r"\\10.255.255.2\equipo-prueba\repo", r"\??\UNC\10.255.255.1\equipo-prueba\repo", r"\\.\pipe\equipo-prueba"] {
            assert!(!readable(Path::new(r)), "{r}");
        }
        // What comes from outside stays the same: `of_dir`, `canonical`, `is_dir`.
        assert!(!is_local_path(repo) && of_dir(repo).is_err() && canonical(repo).is_err() && !is_dir(repo));
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
    }

    /// WSL (`\\wsl$\<distro>`, `\\wsl.localhost\<distro>`) is on this computer only if its provider
    /// serves those paths before SMB; the shape has to be exact.
    #[test]
    fn wsl_only_with_its_provider_in_front() {
        for r in [r"\\wsl$\Ubuntu\home\a.rs", r"\\WSL.localhost\Ubuntu", "//wsl$/Ubuntu/x", r"\\?\UNC\wsl.localhost\Debian\x"] {
            assert!(wsl_shape(r), "{r}");
        }
        for r in [r"\\wsl$", r"\\wsl$\", r"\\wsl$\..\x", r"\\wsl$\.\x", r"\\wsl.localhost.evil.com\x\y", r"\\wsl\x", r"\\10.0.0.1\wsl$\x", r"\??\UNC\wsl$\x\y", r"C:\wsl$\x"] {
            assert!(!wsl_shape(r), "{r}");
        }
        assert!(p9np_before_smb("RDPNP,P9NP,LanmanWorkstation,webclient"));
        assert!(p9np_before_smb("P9NP"));
        assert!(!p9np_before_smb("RDPNP,LanmanWorkstation,P9NP"));
        assert!(!p9np_before_smb("RDPNP,LanmanWorkstation,webclient"));
        assert!(!p9np_before_smb(""));
        // With or without WSL here, a WSL path is local exactly when WSL serves it.
        let w = Path::new(r"\\wsl$\Ubuntu\home");
        assert_eq!(is_local_path(w), wsl_serves(), "{}", w.display());
        assert_eq!(is_network_path(r"\\wsl$\Ubuntu\home"), !wsl_serves());
        assert!(!is_local_path(Path::new(r"\\wsl$\..\x")));
    }

    /// `~/` followed by a network path (`~/\\host\x`, `~\//host/x`, `~/\\?\UNC\…`): when joined with the
    /// home folder, `join` keeps the network one. `remember` from the home folder looks for the
    /// repository of the memory's paths (`for_saving`) and looked at them on disk: an SMB connection to
    /// whatever the memory said. They are neither looked at nor give a project, also when cited in the text.
    #[test]
    // Only on Windows: on Linux and macOS the backslash is just another character of the name and
    // `//server` is a local folder, so those paths open no connection.
    #[cfg(windows)]
    fn tilde_followed_by_a_network_path_is_not_looked_at() {
        let base = crate::common::TempDir::new("tilde-red");
        let home = PathBuf::from(of_dir(&base).unwrap());
        let bad = [
            r"~/\\10.255.255.1\x\a.rs",
            r"~\\\10.255.255.1\x\a.rs",
            "~///10.255.255.1/x/a.rs",
            r"~\//10.255.255.1/x/a.rs",
            r"~/\\?\UNC\10.255.255.1\x\a.rs",
            r"~/\??\UNC\10.255.255.1\x\a.rs",
            r"~\\\.\pipe\x",
            r"~/\\10.255.255.1\x.",
        ];
        for r in bad {
            let start = std::time::Instant::now();
            // `None`, or a path on the home folder's drive (`~/\??\UNC\…` on Windows becomes
            // `C:\??\UNC\…`, a local folder that can't exist): never a network one.
            let e = expand(r, &home);
            assert!(e.as_ref().is_none_or(|p| is_local_path(p)), "{r}");
            assert_eq!(repo_of_path(r, &home), None, "{r}");
            assert_eq!(relative_to(r, &home.to_string_lossy(), &home), r, "{r}");
            if e.is_none() {
                assert_eq!(depth(r, &home), 0, "{r}");
                assert!(paths_in(&format!("mira {r} y ya"), &home).is_empty(), "{r}");
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "{r}: {:?}", start.elapsed());
        }
        for r in [r"\\10.255.255.1\x", r"\\?\UNC\10.255.255.1\x", r"\??\UNC\10.255.255.1\x"] {
            assert_eq!(partial_canonical(Path::new(r)), None, "{r}");
        }
        // Without a home folder, `~` is nothing (it used to be relative to the current folder).
        assert_eq!(expand("~/web/a.rs", Path::new("")), None);
        // From the real home folder, `remember` with those paths and that text: global, right away.
        if let Some(h) = home_project() {
            let start = std::time::Instant::now();
            let paths: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            let (p, out_paths) = for_saving(h, &paths, &bad.join(" "));
            assert_eq!(p, "");
            assert_eq!(out_paths, paths);
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
        }
    }
}
