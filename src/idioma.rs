//! Language of what the person sees (help, messages and errors): English by default, Spanish with
//! `OMOIKANE_LANG=es`. The code and its comments are in English.

use std::sync::OnceLock;

/// `true` if we must speak Spanish.
pub fn es() -> bool {
    static ES: OnceLock<bool> = OnceLock::new();
    *ES.get_or_init(|| {
        std::env::var("OMOIKANE_LANG").map(|v| v.trim().to_lowercase().starts_with("es")).unwrap_or(false)
    })
}

/// The text in the active language, like `format!`: `tr!("saved {id}", "guardada {id}")`.
#[macro_export]
macro_rules! tr {
    ($en:literal, $es:literal $(, $($arg:tt)+)?) => {
        if $crate::idioma::es() { format!($es $(, $($arg)+)?) } else { format!($en $(, $($arg)+)?) }
    };
}
