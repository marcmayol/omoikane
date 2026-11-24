//! The tests' temporary folder (`common::TempDir`) is deleted even if something is slow to release its files.
//! Windows only: on other systems an open file is deleted anyway.
#![cfg(windows)]

mod common;

/// On Windows an open file that does not share deletion cannot be deleted: if another process (git, the
/// antivirus) releases it a few seconds late, the folder is still deleted as soon as it is released. With a 5 s
/// margin, a batch of `sync` runs on a loaded machine left folders behind in `%TEMP%`.
#[test]
fn deleted_even_if_a_file_is_released_late() {
    use std::os::windows::fs::OpenOptionsExt;
    let t = common::TempDir::new("temporal-tarde");
    let path = t.to_path_buf();
    // Without `FILE_SHARE_DELETE` (Rust's `File::create` does share it, so it could be deleted while open).
    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(path.join("abierto")).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(7));
        drop(file);
    });
    drop(t);
    release.join().unwrap();
    assert!(!path.exists(), "quedó {}", path.display());
}
