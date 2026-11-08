//! Agentes sin persona delante (CI, servidores, contenedores, agentes en la nube), 1.7.0: `omoi` sin
//! carpeta de usuario, sin preguntas y configurado solo por variables de entorno.
//!
//! - `OMOIKANE_HOME`: la carpeta de Omoikane entera (la base, los modelos, la preferencia del modelo
//!   de significado y su servicio, las actualizaciones). Por defecto `~/.omoikane`. `OMOIKANE_DB` y
//!   `OMOIKANE_MODELS` siguen mandando por encima para lo suyo. Lo de la sincronización (configuración,
//!   claves, clones), las copias y el diario viven junto a la base, así que van con ella.
//! - Sin carpeta de usuario (un contenedor sin `HOME` ni entrada en `/etc/passwd`) y sin
//!   `OMOIKANE_HOME` ni `OMOIKANE_DB`, `omoi` se niega con un error claro en vez de crear la base en la
//!   carpeta en la que se ejecuta (podría acabar dentro del repositorio que se está probando).
//! - `OMOIKANE_NONINTERACTIVE=1`: nadie delante. Ninguna pregunta (lo que necesite confirmación pide
//!   `--yes`), ninguna descarga que no se haya pedido (el modelo de significado), ninguna
//!   actualización automática, ningún servicio de significado que se arranque solo (1.7.1; si ya hay
//!   uno en marcha, se usa) y ninguna sincronización de fondo que sobreviva al proceso (salvo
//!   `OMOIKANE_SYNC_AUTO=1`). Sin ella, las preguntas ya se saltan si la entrada no es una terminal; lo
//!   demás no, porque los hooks de los agentes tampoco tienen terminal y ahí sí se quieren.
//! - `OMOIKANE_AUTHOR`: quién firma lo que se guarda (el autor de cada memoria y de cada veredicto),
//!   por encima del que se eligió al configurar la sincronización.
//! - `OMOIKANE_SYNC_CODE` (uno o varios códigos de `omoi sync code`) y `OMOIKANE_SYNC_URL` (otra URL
//!   para el mismo repositorio, p. ej. con un token de la CI): `omoi sync`, `omoi sync --pull` y
//!   `omoi sync join` sin código se unen solos a esos repositorios. El código lleva la clave: nunca
//!   se escribe en la salida ni en un error.

use std::path::PathBuf;

use crate::tr;

/// El valor de una variable, sin espacios alrededor; `None` si no está o queda vacía.
fn valor(nombre: &str) -> Option<String> {
    std::env::var(nombre).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// `1`, `true`, `yes` u `on` (sin mirar mayúsculas).
fn encendida(nombre: &str) -> bool {
    valor(nombre).is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// La carpeta de Omoikane: `OMOIKANE_HOME` o `~/.omoikane`. `None` si no hay ninguna de las dos (sin
/// carpeta de usuario, o con un `HOME` vacío o relativo).
///
/// `OMOIKANE_HOME` tiene que ser absoluta: una relativa dependería de la carpeta en la que se ejecute
/// cada orden (cada hook, cada paso de una CI tendría su base). `~` y `~/…` se expanden con la carpeta
/// de usuario; otra relativa no vale (`None`, y `casa_mala` lo explica).
pub fn casa() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("OMOIKANE_HOME").filter(|h| !h.is_empty()) {
        return casa_de(&h.to_string_lossy());
    }
    crate::proyecto::casa_local().map(|h| h.join(".omoikane"))
}

/// `OMOIKANE_HOME` como ruta: absoluta tal cual, `~` o `~/…` desde la carpeta de usuario; otra, `None`.
/// Tampoco una de red (`\\servidor\recurso`, `//host/x`, `~/\\host\x`…): cada orden abriría la base
/// y los ficheros de la sincronización ahí (ver `proyecto::ruta_local`).
fn casa_de(h: &str) -> Option<PathBuf> {
    let usuario = crate::proyecto::casa_local;
    if h == "~" {
        return usuario();
    }
    let p = if let Some(resto) = h.strip_prefix("~/").or_else(|| h.strip_prefix("~\\")) {
        usuario()?.join(resto)
    } else {
        PathBuf::from(h)
    };
    (p.is_absolute() && crate::proyecto::ruta_local(&p)).then_some(p)
}

/// El error de un `OMOIKANE_HOME` que no vale (relativo, o con `~` sin carpeta de usuario); `None` si
/// no está o vale.
pub fn casa_mala() -> Option<String> {
    let h = std::env::var_os("OMOIKANE_HOME").filter(|h| !h.is_empty())?;
    let h = h.to_string_lossy();
    casa_de(&h).is_none().then(|| {
        let tras_casa = h.strip_prefix("~/").or_else(|| h.strip_prefix("~\\")).unwrap_or(&h);
        let local = |r: &str| crate::proyecto::ruta_local(std::path::Path::new(r));
        if !local(&h) || !local(tras_casa) {
            tr!(
                "OMOIKANE_HOME must be a folder on this computer (it is «{h}»): Omoikane doesn't open network paths (\\\\server\\share, //host/x, \\\\?\\…); map the share to a drive letter if you need it",
                "OMOIKANE_HOME tiene que ser una carpeta de este equipo (es «{h}»): Omoikane no abre rutas de red (\\\\servidor\\recurso, //host/x, \\\\?\\…); si la necesitas, asígnale una letra de unidad"
            )
        } else {
            tr!(
                "OMOIKANE_HOME must be an absolute path (it is «{h}»): a relative one would change with the folder each command runs in",
                "OMOIKANE_HOME tiene que ser una ruta absoluta (es «{h}»): una relativa cambiaría con la carpeta en la que se ejecuta cada orden"
            )
        }
    })
}

/// El error de una base (`--db`, `OMOIKANE_DB`) o una carpeta de modelos (`OMOIKANE_MODELS`) que no es
/// de este equipo; `None` si vale. Se mira antes de abrir nada.
pub fn ruta_mala(que: &str, p: &std::path::Path) -> Option<String> {
    (!crate::proyecto::ruta_local(p)).then(|| {
        tr!(
            "{que} must be on this computer (it is «{}»): Omoikane doesn't open network paths (\\\\server\\share, //host/x, \\\\?\\…); map the share to a drive letter if you need it",
            "{que} tiene que estar en este equipo (es «{}»): Omoikane no abre rutas de red (\\\\servidor\\recurso, //host/x, \\\\?\\…); si la necesitas, asígnale una letra de unidad",
            p.display()
        )
    })
}

/// Si hay dónde guardar la base: `OMOIKANE_DB` o una carpeta de Omoikane.
pub fn hay_donde_guardar() -> bool {
    std::env::var_os("OMOIKANE_DB").is_some_and(|d| !d.is_empty()) || casa().is_some()
}

/// El error de cuando no hay carpeta de usuario ni variables que digan dónde guardar.
pub fn sin_casa() -> String {
    tr!(
        "there is no home folder: set OMOIKANE_HOME to the folder Omoikane should use (or OMOIKANE_DB, or pass --db)",
        "no hay carpeta personal: pon en OMOIKANE_HOME la carpeta que debe usar Omoikane (u OMOIKANE_DB, o pasa --db)"
    )
}

/// `OMOIKANE_NONINTERACTIVE=1`: no hay nadie delante.
pub fn desatendido() -> bool {
    encendida("OMOIKANE_NONINTERACTIVE")
}

/// La variable con la que `omoi` marca los procesos que lanza él mismo en segundo plano (la
/// sincronización de fondo, el `--pull` de los hooks): no es para nadie de fuera.
pub const DE_FONDO: &str = "OMOIKANE_INTERNAL_BACKGROUND";

/// Si este proceso lo lanzó `omoi` en segundo plano (`DE_FONDO`): aunque no haya nadie delante, nadie
/// espera su resultado.
pub fn de_fondo() -> bool {
    encendida(DE_FONDO)
}

/// Si se puede preguntar: hay una terminal en la entrada y no se ha dicho que no hay nadie.
pub fn puede_preguntar() -> bool {
    use std::io::IsTerminal;
    !desatendido() && std::io::stdin().is_terminal()
}

/// Sin descargas que nadie haya pedido (el modelo de significado al crecer la memoria):
/// `OMOIKANE_NONINTERACTIVE=1` u `OMOIKANE_NO_MODEL=1`.
pub fn sin_descargas() -> bool {
    desatendido() || encendida("OMOIKANE_NO_MODEL")
}

/// La sincronización de fondo tras cada cambio y cada pocos minutos: `OMOIKANE_SYNC_AUTO=0` la apaga
/// y `=1` la enciende; sin decir nada, encendida salvo con nadie delante (en una CI el proceso de
/// fondo moriría con el trabajo, o lo alargaría: ahí se sincroniza con `omoi sync` al final).
pub fn sync_automatica() -> bool {
    match valor("OMOIKANE_SYNC_AUTO").as_deref() {
        Some("0") => false,
        Some("1") => true,
        _ => !desatendido(),
    }
}

/// `OMOIKANE_AUTHOR`: quién firma lo que se guarda aquí.
pub fn autor() -> Option<String> {
    valor("OMOIKANE_AUTHOR")
}

/// Los códigos de `OMOIKANE_SYNC_CODE` (separados por espacios, comas o líneas: uno por repositorio,
/// el general y los de los espacios).
pub fn codigos_sync() -> Vec<String> {
    valor("OMOIKANE_SYNC_CODE")
        .map(|v| v.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// `OMOIKANE_SYNC_URL`: otra URL para el repositorio del código (solo con un código).
pub fn url_sync() -> Option<String> {
    valor("OMOIKANE_SYNC_URL")
}

#[cfg(test)]
mod tests {
    use super::casa_de;

    /// `OMOIKANE_HOME` relativa no vale; absoluta, tal cual; `~/…`, desde la carpeta de usuario.
    #[test]
    fn casa_absoluta_o_con_tilde() {
        assert_eq!(casa_de("relativa/omoikane"), None);
        assert_eq!(casa_de(".omoikane"), None);
        let abs = std::env::temp_dir().join("omoi-casa");
        assert_eq!(casa_de(&abs.to_string_lossy()), Some(abs));
        if let Some(u) = std::env::home_dir().filter(|u| u.is_absolute()) {
            assert_eq!(casa_de("~/omoi"), Some(u.join("omoi")));
            assert_eq!(casa_de("~"), Some(u));
        }
    }

    /// Un `OMOIKANE_HOME` de red (también tras `~/`) no vale: cada orden abriría ahí la base.
    #[test]
    fn casa_de_red_no_vale() {
        for h in [r"\\10.255.255.1\x", "//10.255.255.1/x", r"\\?\UNC\10.255.255.1\x", r"\??\UNC\10.255.255.1\x", r"~/\\10.255.255.1\x", r"~\//10.255.255.1/x"] {
            assert_eq!(casa_de(h), None, "{h}");
        }
    }
}
