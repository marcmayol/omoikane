//! What the tests share (the ones in `tests/` with `mod comun;` and the library's, which include it
//! from `lib.rs`): a temporary folder that deletes itself.

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A test's temporary folder, `<temp>/omoi-<name>-<pid>`, deleted entirely when it goes out of
/// scope, also if the test fails (the `Drop` runs while unwinding the stack after a `panic`). It only deletes
/// that folder: the path is set by `nueva` and can't be changed.
///
/// Declare it before whatever keeps files open inside it (the database): what comes after is dropped
/// first, and on Windows an open file can't be deleted.
pub struct Temporal(PathBuf);

impl Temporal {
    /// Empty: if one from before with the same name and pid was left over, it is deleted first.
    pub fn nueva(name: &str) -> Temporal {
        assert!(!name.is_empty() && !name.contains(['/', '\\', '.']), "nombre de carpeta temporal: {name}");
        let dir = std::env::temp_dir().join(format!("omoi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temporal(dir)
    }
}

impl Deref for Temporal {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Temporal {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Temporal {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

/// A free 127.0.0.1 port for a test's `omoi serve`: the one the system gives when asked for 0. A
/// fixed one per pid (`7600 + pid % 300`) clashed with another `cargo test` running at the same time, like those of other
/// branches in parallel (on Windows pids are multiples of 4: `pid % 300` only gives 75 ports): the test
/// talked to the other process's server, which was sometimes shutting down and answered empty.
#[allow(dead_code)]
pub fn puerto_libre() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

/// The variables an agent session sets that change what `omoi` saves: Claude Code sets
/// `TRACEPARENT` (the provenance carries its trace) and an orchestrator that signs its agents' work,
/// `OMOIKANE_AUTHOR` (the author). In CI they don't exist; if the tests run inside a session, they do.
#[allow(dead_code)]
pub const DE_LA_SESION: &[&str] = &["OMOIKANE_AUTHOR", "TRACEPARENT", "TRACESTATE"];

/// Removes the `DE_LA_SESION` variables from the command, so the test gives the same result from an agent
/// session as in CI. It goes in the command and not in `.cargo/config.toml`: Cargo only reads that file if
/// launched from inside the repo, not with `--manifest-path` from another folder. A test that needs
/// one sets it afterwards.
#[allow(dead_code)]
pub fn sin_la_sesion(c: &mut std::process::Command) -> &mut std::process::Command {
    for v in DE_LA_SESION {
        c.env_remove(v);
    }
    c
}

impl Drop for Temporal {
    fn drop(&mut self) {
        // A process the test launched (`omoi serve`, the background sync, git) or the antivirus
        // can take a while to release their files: on Windows they aren't deleted until then. With the machine
        // loaded by other builds, 5 s weren't enough (8 folders of `tests/sync.rs` were left in
        // a 751 s run); waiting longer only costs in that case.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let error = loop {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(_) if !self.0.exists() => return,
                Err(e) if std::time::Instant::now() >= deadline => break e,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
            }
        };
        // Through `io::stderr` and not `eprintln!`: the `Drop` runs inside the test, with the harness's
        // capture active, and with the macro the warning would only show if the test fails.
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "aviso: no se pudo borrar la carpeta temporal {}: {error}", self.0.display());
    }
}
