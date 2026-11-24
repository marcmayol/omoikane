//! On Windows, `user32.dll` is loaded only if used (the `omoi tui` view, through crossterm): loading it at
//! startup cost ~2 ms on every `omoi`, including in the hook for every message.
//!
//! And the main thread's stack goes from 1 MB (the Windows default) to 8 MB (the Linux and macOS one): in debug,
//! the whole clap CLI (every command with its options) no longer fit in 1 MB and `omoi --version`
//! overflowed. Reserving costs nothing: the memory is only used if needed.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:user32.dll");
        println!("cargo:rustc-link-lib=delayimp");
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
