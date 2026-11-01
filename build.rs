//! En Windows, `user32.dll` se carga solo si se usa (la vista `omoi tui`, por crossterm): cargarla al
//! arrancar costaba ~2 ms en cada `omoi`, también en el hook de cada mensaje.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:user32.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
}
