//! Idioma de lo que ve la persona (ayuda, mensajes y errores): inglés por defecto, español con
//! `OMOIKANE_LANG=es`. El código y sus comentarios siguen en español.

use std::sync::OnceLock;

/// `true` si hay que hablar en español.
pub fn es() -> bool {
    static ES: OnceLock<bool> = OnceLock::new();
    *ES.get_or_init(|| {
        std::env::var("OMOIKANE_LANG").map(|v| v.trim().to_lowercase().starts_with("es")).unwrap_or(false)
    })
}

/// El texto en el idioma activo, como `format!`: `tr!("saved {id}", "guardada {id}")`.
#[macro_export]
macro_rules! tr {
    ($en:literal, $es:literal $(, $($arg:tt)+)?) => {
        if $crate::idioma::es() { format!($es $(, $($arg)+)?) } else { format!($en $(, $($arg)+)?) }
    };
}
