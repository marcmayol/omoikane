//! En Windows, `user32.dll` se carga solo si se usa (la vista `omoi tui`, por crossterm): cargarla al
//! arrancar costaba ~2 ms en cada `omoi`, también en el hook de cada mensaje.
//!
//! Y la pila del hilo principal pasa de 1 MB (lo de Windows) a 8 MB (lo de Linux y macOS): en debug, la
//! CLI entera de clap (cada orden con sus opciones) ya no cabía en 1 MB y `omoi --version` se
//! desbordaba. Reservar no cuesta nada: la memoria solo se usa si hace falta.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:user32.dll");
        println!("cargo:rustc-link-lib=delayimp");
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
