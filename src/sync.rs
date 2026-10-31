//! Memoria compartida por git: los equipos de una persona (o un grupo) juntan sus memorias en un
//! repositorio git suyo, sin servicio propio y sin coste. Omoikane solo usa el `git` del usuario, con
//! sus credenciales.
//!
//! - **Qué se sube:** texto, nunca el SQLite. Una línea por memoria (y por cada olvido), ordenadas y
//!   partidas por proyecto y por equipo: `<proyecto>/<equipo>.jsonl`, `global/<equipo>.jsonl` y
//!   `olvidos/<equipo>.jsonl`, donde `<equipo>` es un nombre opaco derivado de la clave (el nombre del
//!   equipo, que lleva el del sistema, solo va dentro de las líneas cifradas). Cada equipo solo
//!   escribe sus ficheros, así que git nunca tiene conflictos; al bajar se leen solo los ficheros que
//!   cambiaron desde la última vez. Los commits van firmados como «omoikane», sin nombres.
//! - **Cifrado solo en el remoto:** cada línea va cifrada con AES-SIV (RFC 5297), que es determinista:
//!   una línea que no cambia queda igual byte a byte y el diff de git sigue siendo de una línea. Las
//!   carpetas de proyecto tienen un nombre opaco derivado de la clave. En el equipo todo está en claro.
//!   El repositorio lleva un fichero de comprobación (`omoikane.check`) cifrado con su clave: un equipo
//!   con otra clave lo sabe antes de leer nada.
//! - **Claves con id y rotación (0.20.0):** cada línea lleva el id de la clave que la cifró (`k2:…`;
//!   sin prefijo, la clave 1, como todas las de antes). `omoikane.check` dice qué claves hay y cuál es
//!   la vigente. `omoi sync rotate` crea una clave nueva, vuelve a cifrar con ella todo lo de este
//!   equipo y da el código nuevo: quien solo tiene la vieja deja de leer lo que se escriba desde
//!   entonces (y `omoi sync status` se lo dice). Un equipo guarda todas las claves que ha recibido,
//!   para leer lo que otros aún no han vuelto a cifrar, y escribe siempre con la más nueva.
//! - **Un repositorio por espacio:** además del general, uno por espacio (`omoi sync init --space
//!   <nombre>`), cada uno con su clave; `omoi sync` y la sincronización de fondo los recorren todos.
//!   Lo de un espacio (sus proyectos y lo suyo) va solo a su repositorio; el general lleva lo que no
//!   es de ningún espacio (`va_al_repo`).
//! - **Quién gana:** el sello del reloj lógico (`reloj`) más alto. La versión de aquí que pierde queda
//!   en el historial con el motivo `sync: <equipo>`. Los olvidos se aplican antes que las memorias, así
//!   que lo olvidado no resucita; lo que olvida otro equipo se guarda aquí aparte (`omoi sync restore`).
//! - **Proyectos en rutas distintas:** se reconocen por el remoto `origin` de git (o, sin remoto, por
//!   la persona y el nombre de la carpeta), y cada equipo guarda la memoria con su propia ruta.
//! - **Automático:** después de cada cambio, `programar` lanza `omoi sync --background` aparte; espera
//!   unos segundos para juntar cambios y solo corre uno a la vez por equipo.
//!
//! Todo vive junto a la base: `sync.json` (configuración), `sync.key` (claves), `sync/` (el clon),
//! `sync-estado.json`, `sync.pendiente`, `sync.hecho` y `sync.lock`. Cada espacio con repositorio
//! propio, en `sync-spaces/<espacio>/`: `sync.json`, `sync.key`, `repo/` (su clon) y `estado.json`
//! (la marca de pendiente y el cerrojo son los de la base: una sincronización los recorre todos). Con
//! una base que no se llama `omoikane.db`, los nombres llevan delante el de la base
//! (`trabajo.sync.json`, `trabajo.sync-spaces/`…), para que dos bases en la misma carpeta no
//! compartan sincronización.

use crate::embeber::Modelo;
use crate::memoria::{self, COLUMNAS, Cambios, Memoria, NCOL, Nueva, Origen, Tipo, de_fila};
use crate::{reloj, tr};
use aes_siv::KeyInit;
use aes_siv::siv::Aes256Siv;
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Cabecera asociada al cifrado: una línea de otro formato o versión no se descifra por accidente.
const CABECERA: &[u8] = b"omoikane-sync-v1";
const RAMA: &str = "main";
const PREFIJO_CODIGO: &str = "omoi1.";
/// El fichero de comprobación de la clave, en la raíz del repositorio, y lo que lleva dentro (cifrado).
const CHEQUEO: &str = "omoikane.check";
const TEXTO_CHEQUEO: &str = "omoikane-key-check-v1";
/// La cabecera de `omoikane.check` cuando el repositorio tiene más de una clave.
const CABECERA_CHEQUEO: &str = "omoikane-check-v2";
/// Dónde viven los repositorios de los espacios, junto a la base.
const CARPETA_ESPACIOS: &str = "sync-spaces";
/// Lo que hace falta para entender el repositorio, en su raíz y cifrado con la clave vigente (como las
/// líneas de `omoikane.check`): `{"min_reader": 1}`. Lo escribe la primera sincronización tras `init`
/// (y la siguiente en los repositorios de antes) y no se cambia si ya existe; tras una rotación se
/// vuelve a cifrar con la clave nueva, con el mismo contenido.
const FORMATO_REPO: &str = "omoikane.format";
/// El lector de repositorios de esta versión. Un repositorio cuyo `min_reader` pase de aquí lo ha
/// escrito una versión que cambió el significado de algo que ya había: esta no escribe en él (ni lee)
/// y avisa. Solo sube con un cambio así; lo que solo añade (un campo, un tipo de línea) no lo sube.
pub const LECTOR_REPO: u64 = 1;
/// Las líneas que sabe leer esta versión (`meta('sync_lector')`). Sube cada vez que una versión
/// aprende a leer algo que las anteriores se saltaban (un tipo de línea nuevo): al sincronizar con un
/// `sync_lector` menor, se olvida por dónde iba cada repositorio (`sync_commit*`) y se relee todo,
/// así que lo que se saltó antes de actualizar entra ahora.
pub const LECTOR_LINEAS: i64 = 1;
/// Lo que puede tardar una orden de git antes de darla por colgada.
const LIMITE_GIT: Duration = Duration::from_secs(120);

// --- Rutas y configuración -------------------------------------------------------------------------

/// Los ficheros de la sincronización, junto a la base.
pub struct Rutas {
    pub config: PathBuf,
    pub clave: PathBuf,
    pub clon: PathBuf,
    pub estado: PathBuf,
    pub pendiente: PathBuf,
    pub hecho: PathBuf,
    pub cerrojo: PathBuf,
    /// El espacio de este repositorio (`None`: el general).
    pub espacio: Option<String>,
    /// La clave de `meta` donde la base apunta el último commit leído de este repositorio.
    pub(crate) marca_commit: String,
}

/// Las rutas del repositorio general.
pub fn rutas(db: &Path) -> Rutas {
    rutas_de(db, None)
}

/// Las rutas del repositorio general (`None`) o del de un espacio.
pub fn rutas_de(db: &Path, espacio: Option<&str>) -> Rutas {
    let dir = db.parent().map(Path::to_path_buf).unwrap_or_default();
    let delante = match db.file_name().map(|n| n.to_string_lossy().into_owned()) {
        Some(n) if n != "omoikane.db" && !n.is_empty() => {
            format!("{}.", db.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n))
        }
        _ => String::new(),
    };
    let f = |nombre: &str| dir.join(format!("{delante}{nombre}"));
    match espacio {
        None => Rutas {
            config: f("sync.json"),
            clave: f("sync.key"),
            clon: f("sync"),
            estado: f("sync-estado.json"),
            pendiente: f("sync.pendiente"),
            hecho: f("sync.hecho"),
            cerrojo: f("sync.lock"),
            espacio: None,
            marca_commit: "sync_commit".into(),
        },
        Some(e) => {
            let d = f(CARPETA_ESPACIOS).join(e);
            Rutas {
                config: d.join("sync.json"),
                clave: d.join("sync.key"),
                clon: d.join("repo"),
                estado: d.join("estado.json"),
                pendiente: f("sync.pendiente"),
                hecho: f("sync.hecho"),
                cerrojo: f("sync.lock"),
                espacio: Some(e.to_string()),
                marca_commit: format!("sync_commit:{e}"),
            }
        }
    }
}

/// Los espacios con repositorio propio en este equipo, por orden de nombre.
pub fn espacios(db: &Path) -> Vec<String> {
    let carpeta = rutas_de(db, Some("x")).config.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut v: Vec<String> = std::fs::read_dir(carpeta)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("sync.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| espacio_valido(n).is_ok())
        .collect();
    v.sort();
    v
}

/// Todos los repositorios configurados en este equipo: el general (si lo está) y uno por espacio.
pub fn repos(db: &Path) -> Vec<Rutas> {
    let general = rutas(db);
    let mut v = Vec::new();
    if general.config.exists() {
        v.push(general);
    }
    v.extend(espacios(db).iter().map(|e| rutas_de(db, Some(e))));
    v
}

/// El nombre de un espacio tal como se guarda (es el nombre de una carpeta): minúsculas, números,
/// `-` y `_`, hasta 64.
pub fn espacio_valido(nombre: &str) -> Result<String> {
    let n = nombre.trim().to_lowercase();
    ensure!(
        !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !n.starts_with(['-', '_']),
        tr!(
            "«{}» is not a valid space name (letters, digits, - and _)",
            "«{}» no vale como nombre de espacio (letras, números, - y _)",
            nombre
        )
    );
    Ok(n)
}

/// Qué se comparte y cómo. Se decide en `omoi sync init` (o `join`). Vale para lo que se sube y para
/// lo que se acepta de los demás.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    /// Tipos que se comparten (las preferencias, solo si se eligió compartirlas).
    pub kinds: Vec<Tipo>,
    /// Si se comparten las memorias globales.
    pub global: bool,
    /// Proyectos que se comparten, por su clave; ninguno = todos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<String>>,
    /// Solo en el repositorio general: los espacios (por su clave) que también van por él. Sin esta
    /// lista, lo de los espacios no va por el general: cada espacio tiene su repositorio (`space`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<Vec<String>>,
    /// El nombre que se apunta como autor de cada memoria; ninguno = no se apunta.
    #[serde(default)]
    pub author: Option<String>,
    /// Para reconocer los proyectos sin remoto git: los de la misma persona se juntan entre sus
    /// equipos, los de dos personas no.
    pub person: String,
    /// El espacio cuyas memorias van a este repositorio (`None`: el repositorio general). Los
    /// `sync.json` de antes no lo llevan: son el general.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

impl Config {
    /// Si se comparte una memoria de este tipo (un tipo que esta versión no conoce cuenta como `fact`),
    /// de este proyecto (`clave`) y de este espacio (`espacio`: el de la memoria o el de su proyecto).
    /// A qué repositorio va cada una lo decide `va_al_repo`.
    fn acepta(&self, kind: &str, clave: &str, espacio: &str) -> bool {
        let kind = kind.parse::<Tipo>().unwrap_or(Tipo::Fact);
        if !self.kinds.contains(&kind) && !(kind == Tipo::Procedure && self.kinds.contains(&Tipo::Fact)) {
            return false;
        }
        if clave.is_empty() {
            // Las de nivel espacio van con su espacio; las globales, si se eligió compartirlas.
            return !espacio.is_empty() || self.global;
        }
        self.projects.as_ref().is_none_or(|p| p.iter().any(|k| k == clave))
    }

    /// Lo que se sube de este equipo y lo que se acepta de los demás: lo elegido y de este repositorio.
    fn admite(&self, kind: &str, clave: &str, espacio: &str) -> bool {
        self.acepta(kind, clave, espacio) && va_al_repo(self, espacio)
    }

    fn comparte(&self, m: &Memoria) -> bool {
        self.admite(m.tipo_texto(), &m.project_key, &m.space)
    }
}

/// Si una memoria de este espacio (su clave; vacío: de ninguno) va al repositorio de esta
/// configuración. El de un espacio (`config.space`) lleva solo lo de ese espacio: sus proyectos y lo
/// suyo, nunca lo global ni lo de proyectos sin espacio. El general, lo que no es de ningún espacio, y
/// de los espacios solo los que diga expresamente su `spaces`: así lo de un cliente no acaba en el
/// repositorio personal por defecto. Vale igual para lo que llega: un repositorio de cliente no puede
/// meter memorias en otros espacios ni globales.
fn va_al_repo(config: &Config, espacio: &str) -> bool {
    match &config.space {
        Some(propio) => !espacio.is_empty() && espacio == crate::proyecto::clave_de_espacio(propio),
        None => espacio.is_empty() || config.spaces.as_ref().is_some_and(|s| s.iter().any(|k| k == espacio)),
    }
}

/// La configuración del repositorio general.
pub fn leer_config(db: &Path) -> Result<Option<Config>> {
    leer_config_de(&rutas(db))
}

/// La configuración de un repositorio (el general o el de un espacio).
pub fn leer_config_de(r: &Rutas) -> Result<Option<Config>> {
    if !r.config.exists() {
        return Ok(None);
    }
    let texto = std::fs::read_to_string(&r.config)?;
    Ok(Some(serde_json::from_str(&texto).with_context(|| format!("{}", r.config.display()))?))
}

/// Las claves que tiene este equipo para un repositorio (`sync.key`): una por línea, en hex, con su
/// id delante como en las líneas cifradas (`k2:<hex>`; sin prefijo, la 1). El fichero de antes de las
/// claves con id, una sola clave en hex, es la clave 1 y sigue valiendo tal cual.
pub(crate) fn leer_claves(r: &Rutas) -> Result<Cifra> {
    let texto = std::fs::read_to_string(&r.clave).with_context(|| tr!("can't read the sync key {}", "no se puede leer la clave de sincronización {}", r.clave.display()))?;
    let danada = || anyhow!(tr!("the sync key is damaged", "la clave de sincronización está dañada"));
    let mut claves = BTreeMap::new();
    for l in texto.lines().filter(|l| !l.trim().is_empty()) {
        let (id, hex) = partir(l).ok_or_else(danada)?;
        let k = desde_hex(hex).ok_or_else(danada)?;
        ensure!(k.len() == 64, danada());
        claves.insert(id, k);
    }
    ensure!(!claves.is_empty(), danada());
    Ok(Cifra { claves })
}

fn guardar_claves(r: &Rutas, cifra: &Cifra) -> Result<()> {
    let texto: String = cifra.claves.iter().map(|(id, k)| format!("{}{}\n", prefijo(*id), a_hex(k))).collect();
    escribir_privado(&r.clave, &texto)
}

/// Escribe un fichero que solo puede leer el usuario (en unix, creado ya con permisos 0600).
fn escribir_privado(ruta: &Path, texto: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = ruta.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(texto.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, ruta)?;
    }
    #[cfg(not(unix))]
    std::fs::write(ruta, texto)?;
    Ok(())
}

fn a_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn desde_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Quita usuario y contraseña de las URL de un texto (`https://token@host/…` → `https://***@host/…`):
/// para enseñar URLs y errores sin enseñar credenciales.
pub fn sin_credenciales(texto: &str) -> String {
    let mut s = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(i) = resto.find("://") {
        let (antes, despues) = resto.split_at(i + 3);
        s.push_str(antes);
        let fin = despues.find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"').unwrap_or(despues.len());
        let anfitrion = &despues[..fin];
        match anfitrion.rfind('@') {
            Some(a) => {
                s.push_str("***@");
                s.push_str(&anfitrion[a + 1..]);
            }
            None => s.push_str(anfitrion),
        }
        resto = &despues[fin..];
    }
    s.push_str(resto);
    s
}

// --- Cifrado ---------------------------------------------------------------------------------------

/// Las claves de un repositorio que tiene este equipo, por su id: se cifra con la vigente (la de id
/// más alto) y se descifra con la que diga cada línea.
///
/// Una línea cifrada es `k<id>:<base64>`, y sin prefijo es de la clave 1: así las líneas de los
/// repositorios de antes de las claves con id siguen valiendo, y las que escribe un equipo con solo la
/// clave 1 siguen saliendo igual byte a byte. El base64 nunca lleva `:`, así que lo de antes del primer
/// `:` es siempre una etiqueta; una que no sea `k<número>` es de un formato futuro y la línea se salta
/// como ilegible. El texto cifrado lleva siempre la misma cabecera asociada (`CABECERA`).
#[derive(Clone)]
pub(crate) struct Cifra {
    claves: BTreeMap<u32, Vec<u8>>,
}

/// El id y el cuerpo de una línea con id de clave (`k2:…`, o sin prefijo: la 1). `None` si lleva una
/// etiqueta que no es de clave.
fn partir(linea: &str) -> Option<(u32, &str)> {
    let l = linea.trim();
    match l.split_once(':') {
        None => Some((1, l)),
        Some((etiqueta, cuerpo)) => {
            let id = etiqueta.strip_prefix('k')?.parse::<u32>().ok().filter(|n| *n >= 1)?;
            Some((id, cuerpo))
        }
    }
}

/// Lo que va delante de una línea de la clave `id` (nada para la 1).
fn prefijo(id: u32) -> String {
    if id == 1 { String::new() } else { format!("k{id}:") }
}

/// Lo que lleva dentro la línea de la clave `id` en `omoikane.check`: el id va dentro (salvo en la 1,
/// que es la de siempre), para que la línea de una clave no pase por la de otra.
fn texto_chequeo(id: u32) -> String {
    if id == 1 { TEXTO_CHEQUEO.to_string() } else { format!("{TEXTO_CHEQUEO} k{id}") }
}

/// Un nombre opaco derivado de una clave.
fn opaco(clave: &[u8], partes: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(clave);
    for p in partes {
        h.update(p);
    }
    a_hex(&h.finalize())[..16].to_string()
}

impl Cifra {
    /// Una sola clave, con su id.
    pub(crate) fn una(id: u32, clave: Vec<u8>) -> Cifra {
        Cifra { claves: BTreeMap::from([(id, clave)]) }
    }

    /// Estas claves y una más (la nueva de una rotación, o la que llega en un código).
    fn con(&self, id: u32, clave: Vec<u8>) -> Cifra {
        let mut c = self.clone();
        c.claves.insert(id, clave);
        c
    }

    /// El id de la clave con la que se cifra.
    pub(crate) fn vigente(&self) -> u32 {
        self.claves.keys().next_back().copied().unwrap_or(1)
    }

    /// Los ids de las claves que hay.
    pub(crate) fn ids(&self) -> Vec<u32> {
        self.claves.keys().copied().collect()
    }

    fn clave_vigente(&self) -> &[u8] {
        self.claves.values().next_back().map(Vec::as_slice).unwrap_or_default()
    }

    fn siv(clave: &[u8]) -> Aes256Siv {
        Aes256Siv::new_from_slice(clave).expect("clave de 64 bytes")
    }

    /// Cifra con la clave vigente, con su id delante.
    pub(crate) fn cifrar(&self, texto: &str) -> Result<String> {
        self.cifrar_con(self.vigente(), texto)
    }

    fn cifrar_con(&self, id: u32, texto: &str) -> Result<String> {
        let clave = self.claves.get(&id).context("clave")?;
        let c = Cifra::siv(clave).encrypt([CABECERA], texto.as_bytes()).map_err(|_| anyhow!("AES-SIV"))?;
        Ok(format!("{}{}", prefijo(id), base64::engine::general_purpose::STANDARD_NO_PAD.encode(c)))
    }

    /// Descifra con la clave que diga la línea.
    pub(crate) fn descifrar(&self, linea: &str) -> Result<String> {
        let mal = || anyhow!(clave_ajena());
        let (id, cuerpo) = partir(linea).ok_or_else(mal)?;
        let Some(clave) = self.claves.get(&id) else {
            bail!(tr!(
                "this was encrypted with key k{} and this computer doesn't have it: ask for the current code (`omoi sync code` on a computer that has it) and run `omoi sync join <code>` here",
                "esto va cifrado con la clave k{} y este equipo no la tiene: pide el código vigente (`omoi sync code` en un equipo que la tenga) y aquí `omoi sync join <código>`",
                id
            ));
        };
        let c = base64::engine::general_purpose::STANDARD_NO_PAD.decode(cuerpo).map_err(|_| mal())?;
        let t = Cifra::siv(clave).decrypt([CABECERA], &c).map_err(|_| mal())?;
        String::from_utf8(t).map_err(|_| mal())
    }

    /// Nombre opaco de la carpeta de un proyecto: no dice qué proyecto es a quien no tiene la clave.
    fn carpeta(&self, clave_proyecto: &str) -> String {
        format!("p-{}", opaco(self.clave_vigente(), &[clave_proyecto.as_bytes()]))
    }

    /// Nombre opaco del fichero de un equipo: el nombre del equipo (con el del sistema) no queda a la
    /// vista en el repositorio.
    fn fichero(&self, equipo: &str) -> String {
        format!("m-{}.jsonl", opaco(self.clave_vigente(), &[b"equipo:", equipo.as_bytes()]))
    }

    /// Los nombres que tenían los ficheros de este equipo con sus claves viejas.
    fn ficheros_viejos(&self, equipo: &str) -> Vec<String> {
        let vigente = self.vigente();
        self.claves.iter().filter(|(id, _)| **id != vigente).map(|(_, k)| format!("m-{}.jsonl", opaco(k, &[b"equipo:", equipo.as_bytes()]))).collect()
    }

    /// Si la línea de la clave `id` de `omoikane.check` se abre con la clave `id` de aquí.
    fn abre(&self, id: u32, linea: &str) -> bool {
        self.claves.contains_key(&id) && partir(linea).is_some_and(|(i, _)| i == id) && self.descifrar(linea).is_ok_and(|t| t == texto_chequeo(id))
    }
}

/// `omoikane.check`: qué claves tiene el repositorio y cuál es la vigente.
///
/// Una línea por clave, cifrada con ella y con su id delante como las demás (`k2:…`, la 1 sin nada),
/// con `texto_chequeo(id)` dentro. Con más de una clave va delante la cabecera
/// `omoikane-check-v2 current=<id>`; con solo la 1, el fichero es el de siempre (una línea), y las
/// versiones de antes lo siguen entendiendo. Así un equipo que no tiene una clave sabe igual que
/// existe, aunque no pueda abrir su línea. De la cabecera se ignora lo que no se entienda (para que
/// pueda crecer), y una línea con una etiqueta que no es de clave también.
#[derive(Debug, Default)]
struct Chequeo {
    lineas: BTreeMap<u32, String>,
    /// La vigente según la cabecera (0 si no la lleva).
    actual: u32,
}

impl Chequeo {
    fn de_texto(texto: &str) -> Chequeo {
        let mut c = Chequeo::default();
        for l in texto.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(resto) = l.strip_prefix("omoikane-check-") {
                for campo in resto.split_whitespace() {
                    if let Some(n) = campo.strip_prefix("current=").and_then(|n| n.trim_start_matches('k').parse().ok()) {
                        c.actual = n;
                    }
                }
            } else if let Some((id, _)) = partir(l) {
                c.lineas.insert(id, l.to_string());
            }
        }
        c
    }

    /// El del clon; `None` si el repositorio no lo tiene.
    fn leer(clon: &Path) -> Option<Chequeo> {
        std::fs::read_to_string(clon.join(CHEQUEO)).ok().map(|t| Chequeo::de_texto(&t))
    }

    /// El id de la clave vigente del repositorio.
    fn vigente(&self) -> u32 {
        self.actual.max(self.lineas.keys().next_back().copied().unwrap_or(0))
    }

    fn texto(&self) -> String {
        let mut t = String::new();
        if self.lineas.len() > 1 || self.vigente() > 1 {
            t.push_str(&format!("{CABECERA_CHEQUEO} current={}\n", self.vigente()));
        }
        for l in self.lineas.values() {
            t.push_str(l);
            t.push('\n');
        }
        t
    }

    /// Cuántas claves de aquí abren su línea, y cuáles tienen línea y no la abren (otra clave con el
    /// mismo id).
    fn comprobar(&self, cifra: &Cifra) -> (usize, Vec<u32>) {
        let (mut abren, mut fallan) = (0, Vec::new());
        for id in cifra.ids() {
            match self.lineas.get(&id) {
                Some(l) if cifra.abre(id, l) => abren += 1,
                Some(_) => fallan.push(id),
                None => {}
            }
        }
        (abren, fallan)
    }
}

fn clave_ajena() -> String {
    tr!(
        "this computer's key is not the repository's: join it with `omoi sync join <code>` (from `omoi sync code` on a computer that already syncs)",
        "la clave de este equipo no es la del repositorio: únete con `omoi sync join <código>` (de `omoi sync code` en un equipo que ya sincroniza)"
    )
}

// --- Formato de las líneas -------------------------------------------------------------------------

/// Los campos de una línea que esta versión no conoce (los añadió una versión más nueva). Se guardan
/// con lo que acompañan (`memory.extra`, `verdict.extra`; los de un olvido, en `meta`; los de una
/// memoria de la papelera, dentro de ella) y se vuelven a escribir tal cual cuando este equipo
/// reexporta esa cosa, aunque la haya cambiado: así un campo nuevo no se pierde por pasar por un
/// equipo con una versión vieja. Se reexportan solo los que no son campos que esta versión ya conoce.
pub(crate) type Extra = serde_json::Map<String, serde_json::Value>;

/// Los campos de cada tipo de línea que conoce esta versión (más `type`): lo que no sea esto va a `extra`.
const CAMPOS_MEMORIA: &[&str] = &[
    "type", "id", "project_key", "project_path", "space_key", "kind", "title", "content", "paths", "tags", "pinned", "created_at", "updated_at", "hlc",
    "content_hlc", "author", "machine", "versions",
];
const CAMPOS_OLVIDO: &[&str] = &["type", "id", "deleted_at", "hlc", "space_key", "machine"];
const CAMPOS_VEREDICTO: &[&str] = &["type", "memory_id", "other_id", "relation", "at", "hlc", "reason", "author", "machine"];
const CAMPOS_PAPELERA: &[&str] = &["type", "id", "memory", "versions", "hlc", "deleted_at", "by"];

/// `extra` sin lo que esta versión ya conoce (lo guardó una versión anterior que no lo conocía).
fn solo_desconocidos(mut extra: Extra, conocidos: &[&str]) -> Extra {
    extra.retain(|k, _| !conocidos.contains(&k.as_str()));
    extra
}

/// `extra` desde el JSON guardado en la base (`'{}'` o lo que sea que no se entienda: vacío).
pub(crate) fn extra_de(texto: &str) -> Extra {
    serde_json::from_str(texto).unwrap_or_default()
}

/// `extra` como se guarda en la base.
pub(crate) fn extra_a_texto(extra: &Extra) -> String {
    serde_json::to_string(extra).unwrap_or_else(|_| "{}".into())
}

/// Los campos desconocidos de un olvido van en `meta` (`olvido_extra:<id>`): la tabla `tombstone` no
/// tiene columna para ellos y el esquema 10 es el que congela la 1.0; solo los trae una versión más
/// nueva, así que casi nunca hay ninguno. Esta es la columna con los del olvido `t` (`'{}'` si no hay).
pub(crate) const EXTRA_OLVIDO: &str = "COALESCE((SELECT v FROM meta WHERE k = 'olvido_extra:' || t.id), '{}')";

/// Apunta (o borra, si no hay) los campos desconocidos del olvido de `id`.
pub(crate) fn guardar_extra_olvido(db: &Connection, id: &str, extra: &Extra) -> Result<()> {
    let k = format!("olvido_extra:{id}");
    if extra.is_empty() {
        db.execute("DELETE FROM meta WHERE k = ?1", [&k])?;
    } else {
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![k, extra_a_texto(extra)])?;
    }
    Ok(())
}

/// Una memoria tal como viaja: en las líneas `memory` (sincronización y `omoi export`) y dentro de las
/// `forgotten` (la papelera, en `omoi export`), para que una memoria restaurada en otro equipo caiga en
/// su proyecto y su espacio.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CuerpoMemoria {
    pub id: String,
    /// Cómo se reconoce el proyecto en otro equipo (vacío: global o de espacio).
    pub project_key: String,
    /// La ruta en el equipo de origen, por si aquí aún no hay ningún proyecto con esa clave.
    pub project_path: String,
    /// El espacio (su clave): el de la memoria si es de espacio (sin `project_key`), o el de su
    /// proyecto. Las versiones de antes de la 0.20.0 no lo mandan.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space_key: String,
    /// Texto: un tipo de una versión futura llega tal cual (aquí cuenta como `fact`) y se reexporta igual.
    pub kind: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
    pub hlc: String,
    /// El sello del último cambio de título, contenido o tipo (con el que caducan los veredictos),
    /// solo si no es `hlc` (fijar o mover cambian `hlc` y no este).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hlc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// El equipo que escribió el fichero (para el historial: «sync: <equipo>»).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
    /// Las versiones anteriores (`omoi export` y, desde la 0.20.0, la sincronización). Las versiones
    /// de antes las ignoran; lo que ya hay con ese número se queda.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<crate::memoria::Version>,
    /// Campos de una versión más nueva (`Extra`).
    #[serde(flatten)]
    pub extra: Extra,
}

impl CuerpoMemoria {
    /// Una memoria de la base como viaja: la clave del proyecto sale de la tabla `project` (calculada
    /// una vez), así que es la misma por la sincronización y por `omoi export`.
    pub(crate) fn de(m: Memoria, hlc: String, content_hlc: String, equipo: &str, versions: Vec<crate::memoria::Version>, extra: Extra) -> CuerpoMemoria {
        CuerpoMemoria {
            kind: m.tipo_texto().to_string(),
            id: m.id,
            project_key: m.project_key,
            project_path: m.project,
            space_key: m.space,
            title: m.title,
            content: m.content,
            paths: m.paths,
            tags: m.tags,
            pinned: m.pinned,
            created_at: m.created_at,
            updated_at: m.updated_at,
            content_hlc: if content_hlc == hlc { String::new() } else { content_hlc },
            hlc,
            author: m.author,
            machine: equipo.to_string(),
            versions,
            extra: solo_desconocidos(extra, CAMPOS_MEMORIA),
        }
    }

    /// Como se guarda en la papelera (`forgotten.memory`, el `Memoria` de siempre): lo que no tiene
    /// sitio en `Memoria` (los campos desconocidos) lo añade `a_papelera`.
    fn a_memoria(&self, origen: &str) -> Memoria {
        let tipo = self.kind.parse::<Tipo>().ok();
        Memoria {
            id: self.id.clone(),
            project: self.project_path.clone(),
            project_key: self.project_key.clone(),
            space: self.space_key.clone(),
            kind: tipo.unwrap_or(Tipo::Fact),
            kind_text: if tipo.is_none() { Some(self.kind.clone()) } else { None },
            title: self.title.clone(),
            content: self.content.clone(),
            paths: self.paths.clone(),
            tags: self.tags.clone(),
            pinned: self.pinned,
            source: origen.to_string(),
            content_hlc: self.content_hlc.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            author: self.author.clone(),
            ..Default::default()
        }
    }
}

/// Las claves que la papelera añade al JSON de `Memoria` (que las ignora al leerlo): los campos
/// desconocidos de la memoria y los de la línea `forgotten`.
pub(crate) const PAPELERA_EXTRA: &str = "extra";
pub(crate) const PAPELERA_EXTRA_LINEA: &str = "extra_forgotten";

/// El JSON que se guarda en `forgotten.memory`: el `Memoria` de siempre (lo que leen la papelera y
/// `restaurar`) y, si los hay, los campos desconocidos.
pub(crate) fn a_papelera(m: &Memoria, extra: &Extra, extra_linea: &Extra) -> Result<String> {
    let mut v = serde_json::to_value(m)?;
    if !extra.is_empty() {
        v[PAPELERA_EXTRA] = serde_json::Value::Object(extra.clone());
    }
    if !extra_linea.is_empty() {
        v[PAPELERA_EXTRA_LINEA] = serde_json::Value::Object(extra_linea.clone());
    }
    Ok(v.to_string())
}

/// Lo que guarda `forgotten.memory`: la memoria y sus campos desconocidos (los de la memoria y los de
/// la línea). `None` si no se entiende.
pub(crate) fn de_papelera(texto: &str) -> Option<(Memoria, Extra, Extra)> {
    let v: serde_json::Value = serde_json::from_str(texto).ok()?;
    let objeto = |k: &str| v.get(k).and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let (extra, extra_linea) = (objeto(PAPELERA_EXTRA), objeto(PAPELERA_EXTRA_LINEA));
    Some((serde_json::from_value(v).ok()?, extra, extra_linea))
}

/// La memoria de una línea `forgotten`: la forma de `CuerpoMemoria` (desde la 0.20.0) o el `Memoria`
/// de la API JSON que escribían la 0.18.0 y la 0.19.0 (sin `project_path`; con `project`, la ruta).
/// Las de entonces no llevan su sello dentro: queda vacío y vale el de la línea.
fn cuerpo_olvidado<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<CuerpoMemoria, D::Error> {
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    if v.get("project_path").is_some() {
        return serde_json::from_value(v).map_err(D::Error::custom);
    }
    let m: Memoria = serde_json::from_value(v).map_err(D::Error::custom)?;
    Ok(CuerpoMemoria::de(m, String::new(), String::new(), "", Vec::new(), Extra::new()))
}

/// Cada línea de la sincronización y de `omoi export`, en JSON con su tipo en `type`.
///
/// Reglas para que lo que venga después de la 1.0 no rompa a quien siga en ella: un campo nuevo es
/// opcional y lo conserva quien no lo entiende (`Extra`); un tipo nuevo lo salta quien no lo entiende,
/// que lo cuenta como «de una versión más nueva» y lo vuelve a leer al actualizarse (`LECTOR`); lo que
/// cambie el significado de lo que ya hay sube `min_reader` en `omoikane.format` (o la versión del
/// formato de `omoi export`), y entonces las versiones viejas no escriben en ese repositorio.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Linea {
    Memory(CuerpoMemoria),
    Tombstone {
        id: String,
        deleted_at: String,
        hlc: String,
        /// El espacio de lo olvidado (su clave), con el que el olvido va solo a su repositorio. Vacío:
        /// de ninguno, o de una versión que no lo manda (por el repositorio de un espacio, de ese).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        space_key: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// Un veredicto (`veredictos`), en la carpeta de la memoria juzgada. Las versiones de antes de la
    /// 0.17.0 no lo entienden y se lo saltan; las de antes de la 0.19.0 ignoran `reason` y `author`.
    Verdict {
        memory_id: String,
        #[serde(default)]
        other_id: String,
        relation: String,
        at: String,
        hlc: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        author: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// Una memoria de la papelera, con su historial: solo en `omoi export` (una copia sin la papelera
    /// no sería fiel). `memory` tiene la forma de una línea `memory` (`CuerpoMemoria`), con su proyecto
    /// y su espacio por clave.
    Forgotten {
        id: String,
        #[serde(deserialize_with = "cuerpo_olvidado")]
        memory: CuerpoMemoria,
        #[serde(default)]
        versions: Vec<crate::memoria::Version>,
        hlc: String,
        deleted_at: String,
        #[serde(default)]
        by: String,
        #[serde(flatten)]
        extra: Extra,
    },
    /// Primera línea de un fichero de `omoi export`.
    Header {
        format: String,
        version: u32,
        schema: i64,
        exported_at: String,
        machine: String,
        #[serde(default)]
        encrypted: bool,
        /// La persona local de la base que exporta (la de sus claves `dir:`): un import en una base
        /// recién creada la adopta, como quien restaura su base en un equipo nuevo.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        person: String,
    },
    /// Última línea de un fichero de `omoi export`: cuántas líneas van entre medias y su SHA-256.
    End {
        count: usize,
        sha256: String,
    },
}

/// Los tipos de línea que entiende esta versión.
const TIPOS_DE_LINEA: &[&str] = &["memory", "tombstone", "verdict", "forgotten", "header", "end"];

/// Si un texto es una línea de un tipo que esta versión no conoce: JSON con un `type` que no es de los
/// suyos (lo escribió una versión más nueva). Las demás que no se entienden están dañadas.
pub(crate) fn de_tipo_nuevo(texto: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(texto)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(|t| !TIPOS_DE_LINEA.contains(&t)))
        .unwrap_or(false)
}

impl Linea {
    /// El id que la línea afecta (vacío en la cabecera y el final).
    pub(crate) fn id(&self) -> &str {
        match self {
            Linea::Memory(c) => &c.id,
            Linea::Tombstone { id, .. } | Linea::Forgotten { id, .. } => id,
            Linea::Verdict { memory_id, .. } => memory_id,
            Linea::Header { .. } | Linea::End { .. } => "",
        }
    }

    /// Si los ids que lleva son como los que escribe Omoikane.
    pub(crate) fn valida(&self) -> bool {
        match self {
            Linea::Header { .. } | Linea::End { .. } => true,
            Linea::Verdict { memory_id, other_id, .. } => id_valido(memory_id) && (other_id.is_empty() || id_valido(other_id)),
            Linea::Forgotten { id, memory, .. } => id_valido(id) && memory.id == *id,
            otra => id_valido(otra.id()),
        }
    }

    pub(crate) fn machine(&self) -> &str {
        match self {
            Linea::Memory(c) => &c.machine,
            Linea::Tombstone { machine, .. } | Linea::Verdict { machine, .. } => machine,
            _ => "",
        }
    }

    /// Un olvido como viaja, con su espacio y sus campos desconocidos.
    pub(crate) fn olvido(id: String, deleted_at: String, hlc: String, space_key: String, equipo: &str, extra: Extra) -> Linea {
        Linea::Tombstone { id, deleted_at, hlc, space_key, machine: equipo.to_string(), extra: solo_desconocidos(extra, CAMPOS_OLVIDO) }
    }

    /// Un veredicto como viaja, con sus campos desconocidos.
    pub(crate) fn veredicto(v: crate::veredictos::Veredicto, equipo: &str) -> Linea {
        Linea::Verdict {
            memory_id: v.memory_id,
            other_id: v.other_id,
            relation: v.relation,
            at: v.at,
            hlc: v.hlc,
            reason: v.reason,
            author: v.author,
            machine: equipo.to_string(),
            extra: solo_desconocidos(extra_de(&v.extra), CAMPOS_VEREDICTO),
        }
    }

    /// Una memoria de la papelera como va en `omoi export`: `texto` es lo que guarda `forgotten.memory`.
    /// Sin la clave de su proyecto (lo olvidado antes del esquema 10), se busca por su ruta en la tabla
    /// `project`. `None` si no se entiende.
    pub(crate) fn papelera(db: &Connection, id: String, texto: &str, versions: Vec<crate::memoria::Version>, hlc: String, deleted_at: String, by: String) -> Result<Option<Linea>> {
        let Some((mut m, extra, extra_linea)) = de_papelera(texto) else { return Ok(None) };
        if m.project_key.is_empty()
            && !m.project.is_empty()
            && let Some((p, _)) = crate::proyecto::resolver(db, &m.project)?
        {
            m.project_key = crate::proyecto::clave_de(db, p)?;
            if m.space.is_empty() {
                m.space = crate::proyecto::clave_espacio(db, crate::proyecto::espacio_de(db, p)?)?;
            }
        }
        let content_hlc = if m.content_hlc.is_empty() { hlc.clone() } else { m.content_hlc.clone() };
        // Sin `machine` dentro: quién la olvidó lo dice `by`, y el equipo que exporta, la cabecera.
        let memory = CuerpoMemoria::de(m, hlc.clone(), content_hlc, "", Vec::new(), extra);
        Ok(Some(Linea::Forgotten { id, memory, versions, hlc, deleted_at, by, extra: solo_desconocidos(extra_linea, CAMPOS_PAPELERA) }))
    }
}

/// Un id como los que escribe Omoikane (ULID: 26 letras y números en mayúsculas). Una línea con otro
/// id no se acepta.
fn id_valido(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
}

// --- Proyectos ---------------------------------------------------------------------------------------

/// La clave de un proyecto para reconocerlo en otro equipo: el remoto `origin` de git, normalizado
/// (`git:github.com/dueño/repo`), o, sin remoto, la persona y el nombre de la carpeta
/// (`dir:<persona>/<carpeta>`), para que dos personas con una carpeta `web` no se mezclen.
pub fn clave(project: &str, persona: &str) -> String {
    if project.is_empty() {
        return String::new();
    }
    if let Some(url) = origen_git(Path::new(project)) {
        return format!("git:{}", normalizar_remoto(&url));
    }
    let ruta = Path::new(project);
    let nombre = ruta.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_else(|| project.to_lowercase());
    format!("dir:{persona}/{nombre}")
}

/// La URL del remoto `origin`, leyendo `.git/config` (rápido; en un worktree, el del repositorio).
pub(crate) fn origen_git(raiz: &Path) -> Option<String> {
    let punto_git = raiz.join(".git");
    let config = if punto_git.is_dir() {
        punto_git.join("config")
    } else {
        // Worktree: `.git` es un fichero «gitdir: <repo>/.git/worktrees/<nombre>».
        let t = std::fs::read_to_string(&punto_git).ok()?;
        let gitdir = PathBuf::from(t.trim().strip_prefix("gitdir:")?.trim());
        let comun = std::fs::read_to_string(gitdir.join("commondir")).ok().map(|c| gitdir.join(c.trim())).unwrap_or(gitdir);
        comun.join("config")
    };
    let texto = std::fs::read_to_string(config).ok()?;
    let mut en_origin = false;
    for l in texto.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            en_origin = l == "[remote \"origin\"]";
        } else if en_origin && let Some(resto) = l.strip_prefix("url") {
            return Some(resto.trim_start_matches([' ', '\t', '=']).trim().to_string());
        }
    }
    None
}

pub(crate) fn normalizar_remoto(url: &str) -> String {
    let mut u = url.trim().to_lowercase();
    for p in ["https://", "http://", "ssh://", "git://", "git+ssh://"] {
        if let Some(r) = u.strip_prefix(p) {
            u = r.to_string();
        }
    }
    if let Some((_, r)) = u.split_once('@') {
        u = r.to_string();
    }
    // `host:dueño/repo` (scp) y `host/dueño/repo` son lo mismo; un puerto (`host:22/…`) se quita.
    if let Some((host, resto)) = u.split_once(':') {
        let resto = resto.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('/');
        u = format!("{host}/{resto}");
    }
    u.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// Una persona como parte de una clave: minúsculas, letras y números (vacío si no queda nada).
pub fn persona(nombre: &str) -> String {
    let p: String = nombre.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    p.trim_matches('-').to_string()
}

/// La carpeta de un proyecto queda apuntada: si su clave es la de un proyecto que llegó de otro equipo
/// (o la de un clon, un worktree o una carpeta movida con el mismo `origin`), sus memorias ya son de
/// aquí, sin moverlas (van por el id del proyecto). Devuelve cuántas memorias tenía ese proyecto si la
/// carpeta no estaba apuntada (0 si ya lo estaba o es nuevo).
pub fn adoptar(db: &Connection, project: &str) -> Result<usize> {
    if project.is_empty() || crate::proyecto::es_general(project) || crate::proyecto::apuntada(db, project)? {
        return Ok(0);
    }
    let id = crate::proyecto::registrar(db, project)?;
    Ok(db.query_row("SELECT count(*) FROM memory WHERE project_id = ?1", [id], |f| f.get::<_, i64>(0))? as usize)
}

// --- git ---------------------------------------------------------------------------------------------

#[cfg(windows)]
const NULO: &str = "NUL";
#[cfg(not(windows))]
const NULO: &str = "/dev/null";

/// Sin ventanas en Windows (en segundo plano no hay consola; cada orden abriría una).
fn sin_ventana(c: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Si el usuario ya dice cómo usar ssh (`GIT_SSH_COMMAND`, `GIT_SSH` o `core.sshCommand`: plink, una
/// clave concreta…). Entonces se respeta; si no, ssh va en modo sin preguntas.
fn ssh_propio() -> bool {
    static PROPIO: OnceLock<bool> = OnceLock::new();
    *PROPIO.get_or_init(|| {
        if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
            return true;
        }
        let mut c = std::process::Command::new("git");
        c.args(["config", "--get", "core.sshCommand"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        sin_ventana(&mut c);
        c.output().is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
    })
}

/// Mata git y lo que haya lanzado (git-remote-https, ssh…), no solo el proceso git.
fn matar_arbol(h: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut k = std::process::Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &h.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        sin_ventana(&mut k);
        let _ = k.status();
    }
    #[cfg(unix)]
    {
        // git va en su propio grupo de procesos: se mata el grupo entero.
        // SAFETY: kill con un pid de grupo negativo solo manda la señal a ese grupo.
        unsafe {
            libc::kill(-(h.id() as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = h.kill();
    let _ = h.wait();
}

/// Lee una salida en un hilo, guardando lo que llega a medida que llega: si un nieto de git (un ssh
/// con `ControlPersist`) la deja abierta, se puede seguir sin esperar a que la cierre.
fn lector(mut r: Box<dyn Read + Send>) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let datos = Arc::new(Mutex::new(Vec::new()));
    let fin = Arc::new(AtomicBool::new(false));
    let (d, f) = (datos.clone(), fin.clone());
    std::thread::spawn(move || {
        let mut b = [0u8; 8192];
        loop {
            match r.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(n) => d.lock().map(|mut v| v.extend_from_slice(&b[..n])).unwrap_or(()),
            }
        }
        f.store(true, Ordering::Relaxed);
    });
    (datos, fin)
}

/// Ejecuta git en el clon. Sin preguntas ni ventanas (en segundo plano no hay nadie para contestar),
/// sin los hooks ni las conversiones de fin de línea de la configuración del usuario (el clon es de
/// Omoikane, y un hook que rechace texto cifrado lo atascaría), y con un límite de tiempo para toda la
/// orden, también para leer lo que escribe.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut c = std::process::Command::new("git");
    c.arg("-c")
        .arg(format!("core.hooksPath={NULO}"))
        .args(["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if !ssh_propio() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    sin_ventana(&mut c);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut h = c.spawn().with_context(|| tr!("can't run git: is it installed?", "no se puede ejecutar git: ¿está instalado?"))?;
    let (salida, fin_salida) = lector(Box::new(h.stdout.take().expect("stdout")));
    let (errores, fin_errores) = lector(Box::new(h.stderr.take().expect("stderr")));
    let limite = Instant::now() + LIMITE_GIT;
    let colgado = || anyhow!("git {}: {}", args.first().unwrap_or(&""), tr!("no answer after {} s", "sin respuesta tras {} s", LIMITE_GIT.as_secs()));
    let estado = loop {
        if let Some(e) = h.try_wait()? {
            break e;
        }
        if Instant::now() > limite {
            matar_arbol(&mut h);
            return Err(colgado());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Lo que quede por leer: hasta que se cierren las salidas, con un margen corto (un nieto que las
    // deja abiertas no debe colgar la sincronización) y nunca más allá del límite.
    let margen = (Instant::now() + Duration::from_secs(2)).min(limite);
    while !(fin_salida.load(Ordering::Relaxed) && fin_errores.load(Ordering::Relaxed)) && Instant::now() < margen {
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = salida.lock().map(|v| v.clone()).unwrap_or_default();
    let err = errores.lock().map(|v| v.clone()).unwrap_or_default();
    if estado.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        bail!("git {}: {}", args.join(" "), sin_credenciales(String::from_utf8_lossy(&err).trim()))
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn remoto_tiene_rama(clon: &Path) -> Result<bool> {
    Ok(!git(clon, &["ls-remote", "--heads", "origin", RAMA])?.trim().is_empty())
}

fn hay_head(clon: &Path) -> bool {
    git(clon, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// Clona el repositorio (vacío o no), deja la rama `main` y una identidad propia y neutra en el clon:
/// los commits no dependen de que el usuario tenga git configurado ni dejan su nombre a la vista.
fn clonar(url: &str, clon: &Path) -> Result<()> {
    if clon.exists() {
        std::fs::remove_dir_all(clon).with_context(|| format!("{}", clon.display()))?;
    }
    // Git se lanza desde la carpeta padre: con un destino relativo lo buscaría dentro de ella.
    let clon = &std::path::absolute(clon)?;
    let padre = clon.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(padre)?;
    let destino = clon.to_string_lossy().into_owned();
    git(padre, &["clone", "--quiet", "--", url, &destino]).with_context(|| tr!("can't clone {}", "no se puede clonar {}", sin_credenciales(url)))?;
    if remoto_tiene_rama(clon)? {
        git(clon, &["checkout", "--quiet", "--force", "-B", RAMA, &format!("origin/{RAMA}")])?;
    } else {
        git(clon, &["symbolic-ref", "HEAD", &format!("refs/heads/{RAMA}")])?;
    }
    git(clon, &["config", "user.name", "omoikane"])?;
    git(clon, &["config", "user.email", "omoikane@localhost"])?;
    git(clon, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

/// Deja el clon como el remoto (o como su último commit): lo de este equipo se vuelve a escribir desde
/// la base en cada sincronización, así que un commit o un índice a medias de una vez anterior (un
/// proceso que murió, un commit que falló) no atasca nada.
fn limpiar(clon: &Path) -> Result<()> {
    if hay_head(clon) {
        git(clon, &["reset", "--quiet", "--hard", "HEAD"])?;
    }
    git(clon, &["clean", "--quiet", "-fdx"])?;
    if remoto_tiene_rama(clon)? {
        git(clon, &["fetch", "--quiet", "origin", RAMA])?;
        git(clon, &["checkout", "--quiet", "--force", "-B", RAMA, &format!("origin/{RAMA}")])?;
    }
    Ok(())
}

/// Los ficheros fijos del repositorio: sin conversiones de texto, y la comprobación de la clave.
fn ficheros_fijos(clon: &Path, cifra: &Cifra) -> Result<()> {
    let atributos = clon.join(".gitattributes");
    if !atributos.exists() {
        std::fs::write(atributos, "* -text\n")?;
    }
    let antes = std::fs::read_to_string(clon.join(CHEQUEO)).ok();
    let mut ch = antes.as_deref().map(Chequeo::de_texto).unwrap_or_default();
    let (abren, fallan) = ch.comprobar(cifra);
    if !fallan.is_empty() || (abren == 0 && !ch.lineas.is_empty()) {
        bail!(clave_ajena());
    }
    // Las claves de aquí que el repositorio aún no apunta: la de un repositorio nuevo, la nueva de una
    // rotación, o una cuya subida se perdió.
    for id in cifra.ids() {
        if let std::collections::btree_map::Entry::Vacant(e) = ch.lineas.entry(id) {
            e.insert(cifra.cifrar_con(id, &texto_chequeo(id))?);
        }
    }
    ch.actual = ch.vigente();
    let texto = ch.texto();
    if antes.as_deref() != Some(texto.as_str()) {
        std::fs::write(clon.join(CHEQUEO), texto)?;
    }
    // `omoikane.format`: si no está, el de esta versión; si está cifrado con una clave anterior a la
    // vigente de aquí (una rotación), el mismo contenido con la vigente. Lo que no se pueda abrir no se
    // toca.
    let formato = clon.join(FORMATO_REPO);
    match std::fs::read_to_string(&formato) {
        Err(_) => std::fs::write(&formato, cifra.cifrar(&serde_json::json!({ "min_reader": LECTOR_REPO }).to_string())? + "\n")?,
        Ok(t) => {
            if let Some((id, _)) = partir(t.trim())
                && id < cifra.vigente()
                && let Ok(claro) = cifra.descifrar(t.trim())
            {
                std::fs::write(&formato, cifra.cifrar(&claro)? + "\n")?;
            }
        }
    }
    Ok(())
}

/// El `min_reader` de `omoikane.format` en el clon, si lo tiene y este equipo lo puede abrir.
fn lector_pedido(clon: &Path, cifra: &Cifra) -> Option<u64> {
    let t = std::fs::read_to_string(clon.join(FORMATO_REPO)).ok()?;
    let claro = cifra.descifrar(t.trim()).ok()?;
    serde_json::from_str::<serde_json::Value>(&claro).ok()?.get("min_reader")?.as_u64()
}

/// Si el repositorio pide una versión más nueva de omoi que esta (según su clon, sin red): el
/// `min_reader` que pide.
pub fn necesita_version(r: &Rutas) -> Option<u64> {
    let cifra = leer_claves(r).ok()?;
    lector_pedido(&r.clon, &cifra).filter(|n| *n > LECTOR_REPO)
}

/// El aviso de `necesita_version`.
pub fn aviso_version() -> String {
    tr!(
        "this repository needs a newer version of omoi: update it (`omoi upgrade`); until then nothing is written to it",
        "este repositorio necesita una versión más nueva de omoi: actualízalo (`omoi upgrade`); hasta entonces no se escribe nada en él"
    )
}

/// Tras una rotación, los ficheros de este equipo con el nombre de una clave vieja: lo suyo se vuelve
/// a escribir con la vigente (y con el nombre de la vigente), así que los viejos se borran. Si no,
/// quien solo tiene la clave vieja seguiría leyendo lo que este equipo escribió antes.
fn retirar_viejos(clon: &Path, equipo: &str, cifra: &Cifra) -> Result<()> {
    let viejos = cifra.ficheros_viejos(equipo);
    if viejos.is_empty() {
        return Ok(());
    }
    for e in std::fs::read_dir(clon)?.flatten() {
        if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
            continue;
        }
        for n in &viejos {
            let f = e.path().join(n);
            if f.exists() {
                std::fs::remove_file(f)?;
            }
        }
    }
    Ok(())
}

/// Si el repositorio ya cifra con una clave que este equipo no tiene (otro la rotó): lo que se escriba
/// desde entonces no se puede leer aquí. Mira el clon de la última sincronización (sin red).
pub fn clave_cambiada(r: &Rutas) -> bool {
    let Ok(cifra) = leer_claves(r) else { return false };
    Chequeo::leer(&r.clon).is_some_and(|c| c.vigente() > 0 && !cifra.claves.contains_key(&c.vigente()))
}

/// El aviso de `clave_cambiada`.
pub fn aviso_clave_cambiada() -> String {
    tr!(
        "this repository's key has changed: ask for the new code (`omoi sync code` on a computer that has it) and run `omoi sync join <code>` here",
        "la clave de este repositorio ha cambiado: pide el código nuevo (`omoi sync code` en un equipo que la tenga) y aquí `omoi sync join <código>`"
    )
}

/// Los ids de las claves que tiene este equipo para un repositorio y el de la vigente del repositorio
/// (según su clon; 0 si aún no se sabe).
pub fn claves_de(r: &Rutas) -> (Vec<u32>, u32) {
    let ids = leer_claves(r).map(|c| c.ids()).unwrap_or_default();
    (ids, Chequeo::leer(&r.clon).map_or(0, |c| c.vigente()))
}

// --- Exportar e importar -----------------------------------------------------------------------------

/// Todas las memorias, con su sello (`hlc`), el de su último cambio de texto (`content_hlc`) y sus
/// campos desconocidos (`extra`).
pub(crate) fn todas(db: &Connection) -> Result<Vec<(Memoria, String, String, Extra)>> {
    let mut q = db.prepare(&format!("SELECT {COLUMNAS}, m.hlc, m.content_hlc, m.extra FROM memory m ORDER BY m.id"))?;
    let v = q
        .query_map([], |f| Ok((de_fila(f)?, f.get::<_, String>(NCOL)?, f.get::<_, String>(NCOL + 1)?, extra_de(&f.get::<_, String>(NCOL + 2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// La línea de una memoria, para la sincronización y `omoi export`.
pub(crate) fn linea_de(m: Memoria, hlc: String, content_hlc: String, equipo: &str, versions: Vec<crate::memoria::Version>, extra: Extra) -> Linea {
    Linea::Memory(CuerpoMemoria::de(m, hlc, content_hlc, equipo, versions, extra))
}

/// Qué subiría este equipo (para el resumen de `init` y `join`): globales, de proyectos y cuántos
/// proyectos.
pub fn resumen_de_subida(db: &Connection, config: &Config) -> Result<(usize, usize, usize)> {
    let (mut globales, mut de_proyectos, mut proyectos) = (0, 0, HashSet::new());
    for (m, _, _, _) in todas(db)? {
        if config.comparte(&m) {
            if m.project_key.is_empty() {
                globales += 1;
            } else {
                de_proyectos += 1;
                proyectos.insert(m.project_key);
            }
        }
    }
    Ok((globales, de_proyectos, proyectos.len()))
}

/// Escribe los ficheros de este equipo en el clon y borra los suyos que ya no tocan. Devuelve cuántas
/// memorias exportó.
fn exportar(db: &Connection, clon: &Path, equipo: &str, config: &Config, cifra: &Cifra) -> Result<usize> {
    let mut ficheros: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut n = 0;
    // La carpeta de cada memoria que se comparte: sus veredictos van con ella.
    let mut carpetas: HashMap<String, String> = HashMap::new();
    // El historial de todas, en una consulta (una por memoria eran 50.000 con una base grande).
    let mut historiales = crate::memoria::historiales(db)?;
    for (m, hlc, content_hlc, extra) in todas(db)? {
        if !config.comparte(&m) {
            continue;
        }
        let carpeta = if !m.project_key.is_empty() {
            cifra.carpeta(&m.project_key)
        } else if !m.space.is_empty() {
            cifra.carpeta(&format!("space:{}", m.space))
        } else {
            "global".to_string()
        };
        carpetas.insert(m.id.clone(), carpeta.clone());
        // El historial viaja con la memoria: quien se une a un repositorio (un compañero, al pasar un
        // proyecto personal a un espacio) la recibe con sus versiones de antes.
        let versiones = historiales.remove(&m.id).unwrap_or_default();
        // Segunda capa: lo que se guardó antes de la 0.19.0 (o con `OMOIKANE_REDACT=0` en otro momento)
        // no sale del equipo con su secreto, ni en la memoria ni en sus versiones. Aquí se queda como
        // está, con su aviso en Revisar.
        let (m, versiones) = if crate::secretos::activo() {
            let tapar = |t: &str| crate::secretos::redactar(t).0;
            let versiones = versiones.into_iter().map(|v| crate::memoria::Version { title: tapar(&v.title), content: tapar(&v.content), ..v }).collect();
            (Memoria { title: tapar(&m.title), content: tapar(&m.content), ..m }, versiones)
        } else {
            (m, versiones)
        };
        let l = linea_de(m, hlc, content_hlc, equipo, versiones, extra);
        ficheros.entry(carpeta).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        n += 1;
    }
    for v in crate::veredictos::todos(db)? {
        // Solo si las dos memorias viajan: un veredicto sobre algo que el otro equipo no tiene no sirve.
        let (Some(carpeta), true) = (carpetas.get(&v.memory_id), v.other_id.is_empty() || carpetas.contains_key(&v.other_id)) else {
            continue;
        };
        let l = Linea::veredicto(v, equipo);
        ficheros.entry(carpeta.clone()).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
    }
    {
        let mut q = db.prepare(&format!("SELECT t.id, t.deleted_at, t.hlc, t.space_key, {EXTRA_OLVIDO} FROM tombstone t ORDER BY t.id"))?;
        let marcas: Vec<(String, String, String, String, String)> =
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, deleted_at, hlc, espacio, extra) in marcas {
            // Cada olvido, solo al repositorio de su espacio: el que tenía la memoria al olvidarla (o el
            // del repositorio por el que llegó). Sin espacio (o de antes del esquema 10), al general.
            if !va_al_repo(config, &espacio) {
                continue;
            }
            let l = Linea::olvido(id, deleted_at, hlc, espacio, equipo, extra_de(&extra));
            ficheros.entry("olvidos".into()).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        }
    }
    let nombre = cifra.fichero(equipo);
    // Los ficheros de este equipo que ya no tienen nada se borran.
    for e in std::fs::read_dir(clon)?.flatten() {
        let carpeta = e.file_name().to_string_lossy().into_owned();
        if carpeta.starts_with('.') || !e.path().is_dir() {
            continue;
        }
        let f = e.path().join(&nombre);
        if f.exists() && !ficheros.contains_key(&carpeta) {
            std::fs::remove_file(&f)?;
        }
    }
    for (carpeta, mut lineas) in ficheros {
        // Ordenadas (por texto cifrado, que es determinista): el mismo contenido da el mismo fichero.
        lineas.sort();
        let dir = clon.join(&carpeta);
        std::fs::create_dir_all(&dir)?;
        let destino = dir.join(&nombre);
        let texto = lineas.join("\n") + "\n";
        if std::fs::read_to_string(&destino).ok().as_deref() != Some(texto.as_str()) {
            let tmp = dir.join(format!(".{nombre}.tmp"));
            std::fs::write(&tmp, texto)?;
            std::fs::rename(&tmp, &destino)?;
        }
    }
    Ok(n)
}

/// Si la base se sincronizó por última vez con un lector de líneas más viejo que el de esta versión
/// (`LECTOR_LINEAS`; sin apuntar, el 0), olvida por dónde iba en todos los repositorios para releerlo
/// todo, y apunta el de ahora. Devuelve si lo hizo.
fn releer_si_hace_falta(db: &Connection) -> Result<bool> {
    let v: i64 = db
        .query_row("SELECT v FROM meta WHERE k = 'sync_lector'", [], |f| f.get::<_, String>(0))
        .optional()?
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if v >= LECTOR_LINEAS {
        return Ok(false);
    }
    db.execute("DELETE FROM meta WHERE k = 'sync_commit' OR substr(k, 1, 12) = 'sync_commit:'", [])?;
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sync_lector', ?1)", [LECTOR_LINEAS.to_string()])?;
    Ok(true)
}

/// Qué ficheros de otros equipos hay que leer: los que cambiaron desde la última sincronización, o
/// todos si es la primera (o si la base no recuerda por dónde iba).
fn por_leer(db: &Connection, clon: &Path, equipo: &str, cifra: &Cifra, marca: &str) -> Result<Vec<PathBuf>> {
    let propio = cifra.fichero(equipo);
    let ultimo: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [marca], |f| f.get(0)).optional()?;
    let cambiados: Option<Vec<String>> = match ultimo {
        Some(u) => git(clon, &["diff", "--name-only", &u, "HEAD"]).ok().map(|s| s.lines().map(String::from).collect()),
        None => None,
    };
    let mut v: Vec<PathBuf> = match cambiados {
        Some(lista) => lista.into_iter().map(|f| clon.join(f)).filter(|p| p.exists()).collect(),
        None => {
            let mut todos = Vec::new();
            for e in std::fs::read_dir(clon)?.flatten() {
                if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(e.path())?.flatten() {
                    todos.push(f.path());
                }
            }
            todos
        }
    };
    v.retain(|p| p.extension().is_some_and(|x| x == "jsonl") && p.file_name().is_some_and(|n| n.to_string_lossy() != propio));
    v.sort();
    Ok(v)
}

#[derive(Debug, Default, Serialize)]
pub struct Resumen {
    pub equipo: String,
    pub exportadas: usize,
    pub leidos: usize,
    pub nuevas: usize,
    pub actualizadas: usize,
    pub olvidadas: usize,
    /// Líneas de otros equipos que no se pudieron descifrar o no son válidas (se saltan).
    pub ilegibles: usize,
    /// Líneas de un tipo (o con una etiqueta de cifrado) que esta versión no conoce: las escribió un
    /// omoi más nuevo. Se saltan y se vuelven a leer al actualizar (`LECTOR_LINEAS`).
    pub de_version_nueva: usize,
    /// Líneas rechazadas porque tocan algo que aquí es de otro espacio (un proyecto, una memoria, las
    /// de un veredicto o un olvido) o llegan por un repositorio que no lleva su espacio: nada de un
    /// cliente entra en otro ni cambia de espacio por lo que llega. No se aplican.
    pub conflictos_de_espacio: usize,
    /// Por fichero leído en esta pasada (ruta dentro del repositorio), cuántas líneas malas tenía.
    #[serde(skip)]
    pub malos: BTreeMap<String, usize>,
    /// Lo mismo con las de una versión más nueva.
    #[serde(skip)]
    pub futuros: BTreeMap<String, usize>,
    /// Lo mismo con las rechazadas por ser de otro espacio.
    #[serde(skip)]
    pub conflictos: BTreeMap<String, usize>,
}

impl Resumen {
    /// Junta las cuentas de otro repositorio (los ficheros malos se apuntan en el estado de cada uno).
    fn sumar(&mut self, o: Resumen) {
        self.exportadas += o.exportadas;
        self.leidos += o.leidos;
        self.nuevas += o.nuevas;
        self.actualizadas += o.actualizadas;
        self.olvidadas += o.olvidadas;
        self.ilegibles += o.ilegibles;
        self.de_version_nueva += o.de_version_nueva;
        self.conflictos_de_espacio += o.conflictos_de_espacio;
    }
}

fn hlc_local(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

fn hlc_olvido(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

/// El olvido de `id` en el espacio `espacio`: el que puede parar una memoria de ese espacio. Uno que
/// llegó por el repositorio de otro espacio no la para (no puede borrar lo de otro cliente).
fn hlc_olvido_en(db: &Connection, id: &str, espacio: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1 AND space_key = ?2", [id, espacio], |f| f.get(0)).optional()?)
}

/// El espacio (su clave; vacía si no es de ninguno) del proyecto de esta clave, si aquí hay uno.
fn espacio_del_proyecto(db: &Connection, clave: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT COALESCE((SELECT key FROM space WHERE id = p.space_id), '') FROM project p WHERE p.key = ?1", [clave], |f| f.get(0))
        .optional()?)
}

/// El espacio de una memoria de la papelera: el que tiene ahora aquí su proyecto (si aquí está), o el
/// que se apuntó al olvidarla.
pub(crate) fn espacio_de_papelera(db: &Connection, texto: &str) -> Result<String> {
    let Some((m, _, _)) = de_papelera(texto) else { return Ok(String::new()) };
    if !m.project_key.is_empty()
        && let Some(e) = espacio_del_proyecto(db, &m.project_key)?
    {
        return Ok(e);
    }
    Ok(m.space)
}

/// El espacio (su clave; vacía si no es de ninguno) de una memoria de aquí, viva o en la papelera;
/// `None` si aquí no está.
pub(crate) fn espacio_aqui(db: &Connection, id: &str) -> Result<Option<String>> {
    let viva: Option<String> = db
        .query_row(
            "SELECT COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '') FROM memory m WHERE m.id = ?1",
            [id],
            |f| f.get(0),
        )
        .optional()?;
    if viva.is_some() {
        return Ok(viva);
    }
    let papelera: Option<String> = db.query_row("SELECT memory FROM forgotten WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    papelera.map(|t| espacio_de_papelera(db, &t)).transpose()
}

/// `a` es igual o posterior a `b`.
fn no_menor(a: &str, b: &str) -> bool {
    reloj::comparar(a, b) != std::cmp::Ordering::Less
}

/// Aplica líneas con las reglas de la sincronización, vengan del repositorio o de un fichero de
/// `omoi export`: gana el sello más nuevo, lo olvidado no resucita, un proyecto se reconoce por su
/// clave y, si aquí no existe, toma la ruta de origen. Sin `config` (un import) se acepta todo.
pub(crate) struct Aplicador<'a> {
    config: Option<&'a Config>,
    person: String,
}

impl<'a> Aplicador<'a> {
    /// Con `actual` (la carpeta de la sesión), esa carpeta queda apuntada antes de aplicar nada: lo que
    /// llegue con su clave es ya suyo.
    pub(crate) fn nuevo(db: &Connection, config: Option<&'a Config>, actual: Option<&str>) -> Result<Self> {
        let person = crate::db::persona_de(db);
        if let Some(a) = actual.filter(|a| !a.is_empty() && !crate::proyecto::es_general(a)) {
            crate::proyecto::registrar(db, a)?;
        }
        Ok(Aplicador { config, person })
    }

    /// Aplica una línea. `Err` es una línea que no se pudo aplicar (se cuenta y se sigue). Las
    /// marcas de olvido conviene aplicarlas antes que las memorias, y los veredictos después.
    pub(crate) fn aplicar(&mut self, db: &mut Connection, origen: &str, l: Linea, modelo: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
        match l {
            Linea::Header { .. } | Linea::End { .. } => Ok(()),
            Linea::Tombstone { id, deleted_at, hlc, space_key, extra, .. } => {
                if hlc_olvido(db, &id)?.is_some_and(|h| no_menor(&h, &hlc)) {
                    return Ok(());
                }
                // Por el repositorio de un espacio solo llegan olvidos de ese espacio (sin `space_key`,
                // de una versión que no lo manda: el del repositorio).
                let espacio = match (space_key.is_empty(), self.config.and_then(|c| c.space.as_deref())) {
                    (true, Some(e)) => crate::proyecto::clave_de_espacio(e),
                    _ => space_key,
                };
                let aqui = espacio_aqui(db, &id)?;
                // Un olvido solo borra lo que lleva el repositorio por el que llega: el de acme no olvida
                // una memoria de beta (ni una personal).
                if let Some(c) = self.config
                    && (!va_al_repo(c, &espacio) || aqui.as_deref().is_some_and(|e| !va_al_repo(c, e)))
                {
                    r.conflictos_de_espacio += 1;
                    return Ok(());
                }
                let espacio = aqui.unwrap_or(espacio);
                // La marca que llega pasa a ser la de aquí (en cualquiera de los tres casos): sus campos
                // desconocidos, con ella.
                guardar_extra_olvido(db, &id, &extra)?;
                match hlc_local(db, &id)? {
                    Some(local) if reloj::mayor(&local, &hlc) => {
                        // Aquí se cambió después de que allí se olvidara: se queda, y la marca vieja se apunta.
                        memoria::marcar_olvido(db, &id, Some(&deleted_at), &hlc, &espacio)?;
                        reloj::recibir(db, &hlc)?;
                    }
                    Some(_) => {
                        memoria::olvidar_con(db, &id, &hlc, &deleted_at, origen)?;
                        r.olvidadas += 1;
                    }
                    None => {
                        memoria::marcar_olvido(db, &id, Some(&deleted_at), &hlc, &espacio)?;
                        reloj::recibir(db, &hlc)?;
                    }
                }
                Ok(())
            }
            Linea::Memory(CuerpoMemoria { id, project_key, project_path, space_key, kind, title, content, paths, tags, pinned, created_at, updated_at, hlc, content_hlc, author, versions, extra, .. }) => {
                if let Some(c) = self.config {
                    // Por un repositorio solo entra lo de su espacio (el general: lo de ninguno, y los
                    // espacios que diga su `spaces`): lo demás se rechaza y se cuenta.
                    if !va_al_repo(c, &space_key) {
                        r.conflictos_de_espacio += 1;
                        return Ok(());
                    }
                    // Lo que este equipo no comparte tampoco se acepta (p. ej. las preferencias de otra
                    // persona).
                    if !c.acepta(&kind, &project_key, &space_key) {
                        return Ok(());
                    }
                }
                // Lo olvidado no vuelve; por la sincronización, solo lo para un olvido de su mismo espacio.
                let olvido = if self.config.is_some() { hlc_olvido_en(db, &id, &space_key)? } else { hlc_olvido(db, &id)? };
                if olvido.is_some_and(|h| no_menor(&h, &hlc)) {
                    return Ok(());
                }
                let local = hlc_local(db, &id)?;
                if local.as_ref().is_some_and(|h| no_menor(h, &hlc)) {
                    return Ok(());
                }
                // Nada cambia de espacio por lo que llega: ni una memoria de aquí, ni un proyecto de aquí.
                // Una línea que caería en otro espacio (la carpeta `web` de otro cliente con la misma
                // clave, o una clave de beta escrita en el repositorio de acme) se rechaza.
                if self.config.is_some() {
                    let memoria_fuera = local.is_some() && espacio_aqui(db, &id)?.is_some_and(|e| e != space_key);
                    let proyecto_fuera = !project_key.is_empty() && espacio_del_proyecto(db, &project_key)?.is_some_and(|e| e != space_key);
                    if memoria_fuera || proyecto_fuera {
                        r.conflictos_de_espacio += 1;
                        return Ok(());
                    }
                }
                // El proyecto, por su clave (si aquí no hay ninguno, con la ruta de origen, que se adopta
                // en cuanto aparezca aquí una carpeta con esa clave); o el espacio; o global.
                let (project_id, space_id) = if !project_key.is_empty() {
                    let p = crate::proyecto::de_clave(db, &project_key, &project_path, &self.person, self.config.is_none())?;
                    // Un proyecto nuevo toma el espacio que trae la línea (por la sincronización, uno que ya
                    // estaba aquí en otro espacio se ha rechazado arriba; en un import, uno sin espacio
                    // toma el que trae).
                    if !space_key.is_empty() && crate::proyecto::espacio_de(db, p)?.is_none() {
                        let s = crate::proyecto::espacio_de_clave(db, &space_key)?;
                        db.execute("UPDATE project SET space_id = ?2 WHERE id = ?1", params![p, s])?;
                    }
                    (Some(p), None)
                } else if !space_key.is_empty() {
                    (None, crate::proyecto::espacio_de_clave(db, &space_key)?)
                } else {
                    (Some(0), None)
                };
                let tipo = kind.parse::<Tipo>().ok();
                let kind_text = if tipo.is_none() { Some(kind.clone()) } else { None };
                // Los campos que esta versión no conoce se guardan con ella, para volver a escribirlos.
                let extra = extra_a_texto(&extra);
                let o = Origen { created_at: &created_at, updated_at: &updated_at, hlc: &hlc, content_hlc: &content_hlc, author: &author, extra: &extra };
                // Una línea que no pasa las comprobaciones (título vacío o largo, contenido vacío…) se
                // salta y se cuenta: nunca para la sincronización.
                if local.is_some() {
                    let c = Cambios {
                        title: Some(title),
                        content: Some(content),
                        kind: Some(tipo.unwrap_or(Tipo::Fact)),
                        kind_text,
                        paths: Some(paths),
                        tags: Some(tags),
                        pinned: Some(pinned),
                        project_id,
                        space_id,
                        reason: Some(format!("sync: {origen}")),
                        ..Default::default()
                    };
                    memoria::actualizar_con_origen(db, &id, c, &o, modelo)?;
                    r.actualizadas += 1;
                } else {
                    let nueva = Nueva {
                        project: String::new(),
                        project_id,
                        space_id,
                        kind: Some(tipo.unwrap_or(Tipo::Fact)),
                        kind_text,
                        title,
                        content,
                        paths,
                        tags,
                        pinned,
                        source: format!("sync:{origen}"),
                    };
                    memoria::guardar_con_id(db, &id, nueva, &o, modelo)?;
                    r.nuevas += 1;
                }
                // Vuelve a existir con un sello posterior a su olvido (otro equipo la restauró): ya no
                // está en la papelera.
                db.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
                // El historial que venga: lo que ya hay con ese número se queda.
                for v in versions {
                    db.execute(
                        "INSERT OR IGNORE INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![id, v.version, v.title, v.content, v.kind, serde_json::to_string(&v.paths)?, serde_json::to_string(&v.tags)?, v.changed_at, v.reason],
                    )?;
                }
                Ok(())
            }
            Linea::Verdict { memory_id, other_id, relation, at, hlc, reason, author, extra, .. } => {
                // De una memoria que aquí no está (no se comparte, o se olvidó del todo) no se apunta
                // nada. Las de la papelera, sí: si vuelve, vuelve con sus veredictos (`omoi export` los
                // lleva, y van después de la papelera).
                let Some(de_una) = espacio_aqui(db, &memory_id)? else { return Ok(()) };
                let de_otra = match other_id.is_empty() {
                    true => None,
                    false => match espacio_aqui(db, &other_id)? {
                        Some(e) => Some(e),
                        None => return Ok(()),
                    },
                };
                // Un veredicto solo toca memorias de lo que lleva el repositorio por el que llega.
                if let Some(c) = self.config
                    && (!va_al_repo(c, &de_una) || de_otra.as_deref().is_some_and(|e| !va_al_repo(c, e)))
                {
                    r.conflictos_de_espacio += 1;
                    return Ok(());
                }
                let extra = extra_a_texto(&extra);
                crate::veredictos::recibir(db, &crate::veredictos::Veredicto { memory_id, other_id, relation, at, hlc, reason, author, extra })?;
                Ok(())
            }
            Linea::Forgotten { id, memory, versions, hlc, deleted_at, by, extra } => {
                // A la papelera, si aquí no está viva ni ya en la papelera. Se guarda como la de aquí
                // (`Memoria`, con su proyecto y su espacio por clave) y con sus campos desconocidos.
                if hlc_local(db, &id)?.is_some() {
                    return Ok(());
                }
                let origen = if memory.machine.is_empty() { format!("sync:{origen}") } else { format!("sync:{}", memory.machine) };
                let texto = a_papelera(&memory.a_memoria(&origen), &memory.extra, &extra)?;
                db.execute(
                    "INSERT OR IGNORE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, texto, serde_json::to_string(&versions)?, hlc, deleted_at, by],
                )?;
                Ok(())
            }
        }
    }
}

/// Lee los ficheros de otros equipos y aplica sus líneas. Una línea que no se entiende (de una versión
/// futura, de otra clave, o dañada) se salta y se cuenta, sin parar la sincronización.
#[allow(clippy::too_many_arguments)]
fn importar(db: &mut Connection, clon: &Path, ficheros: &[PathBuf], config: &Config, cifra: &Cifra, actual: Option<&str>, modelo: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
    let mut aplicador = Aplicador::nuevo(db, Some(config), actual)?;
    // (equipo de origen, fichero, línea)
    let mut lineas: Vec<(String, String, Linea)> = Vec::new();
    for f in ficheros {
        let relativo = f.strip_prefix(clon).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let texto = std::fs::read_to_string(f).with_context(|| format!("{}", f.display()))?;
        r.malos.insert(relativo.clone(), 0);
        r.futuros.insert(relativo.clone(), 0);
        r.conflictos.insert(relativo.clone(), 0);
        for l in texto.lines().filter(|l| !l.trim().is_empty()) {
            // Una etiqueta que no es de clave (`x1:…`) es de un formato futuro.
            if partir(l).is_none() {
                *r.futuros.entry(relativo.clone()).or_default() += 1;
                continue;
            }
            let Ok(claro) = cifra.descifrar(l) else {
                *r.malos.entry(relativo.clone()).or_default() += 1;
                continue;
            };
            let x = match serde_json::from_str::<Linea>(&claro) {
                Ok(x) => x,
                // Un tipo de línea de una versión más nueva: se cuenta aparte, para pedir que se
                // actualice, y se volverá a leer entonces.
                Err(_) if de_tipo_nuevo(&claro) => {
                    *r.futuros.entry(relativo.clone()).or_default() += 1;
                    continue;
                }
                Err(_) => {
                    *r.malos.entry(relativo.clone()).or_default() += 1;
                    continue;
                }
            };
            if !x.valida() {
                *r.malos.entry(relativo.clone()).or_default() += 1;
                continue;
            }
            let origen = if x.machine().is_empty() { f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { x.machine().to_string() };
            lineas.push((origen, relativo.clone(), x));
        }
        r.leidos += 1;
    }
    // Primero los olvidos (para no traer algo que se va a borrar), luego las memorias y al final los
    // veredictos, que necesitan las memorias.
    let orden = |l: &Linea| match l {
        Linea::Tombstone { .. } => 0,
        Linea::Memory(_) => 1,
        _ => 2,
    };
    lineas.sort_by_key(|(_, _, l)| orden(l));
    for (origen, fichero, l) in lineas {
        let antes = r.conflictos_de_espacio;
        if aplicador.aplicar(db, &origen, l, modelo, r).is_err() {
            *r.malos.entry(fichero.clone()).or_default() += 1;
        }
        if r.conflictos_de_espacio > antes {
            *r.conflictos.entry(fichero).or_default() += r.conflictos_de_espacio - antes;
        }
    }
    r.ilegibles = r.malos.values().sum();
    r.de_version_nueva = r.futuros.values().sum();
    Ok(())
}

// --- Sincronizar -----------------------------------------------------------------------------------

fn no_configurada() -> String {
    tr!(
        "sync is not set up: `omoi sync init <git url>` (or `omoi sync join <code>`)",
        "la sincronización no está configurada: `omoi sync init <url de git>` (o `omoi sync join <código>`)"
    )
}

/// Una sincronización completa de cada repositorio de este equipo (el general y el de cada espacio):
/// bajar, juntar, escribir lo de este equipo, commit y subir. Apunta cómo fue cada uno en su estado;
/// si uno falla, los demás se sincronizan igual y se devuelve el primer error.
pub fn sincronizar(db: &mut Connection, ruta_db: &Path, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<Resumen> {
    let repos = repos(ruta_db);
    ensure!(!repos.is_empty(), no_configurada());
    let mut total = Resumen { equipo: reloj::equipo(db)?, ..Default::default() };
    let mut fallo = None;
    for r in &repos {
        let res = leer_config_de(r)
            .and_then(|c| c.context(no_configurada()))
            .and_then(|config| sincronizar_repo(db, r, &config, actual, modelo, None))
            .map(|(res, _)| res);
        let res = match &r.espacio {
            Some(e) => res.with_context(|| tr!("space {}", "espacio {}", e)),
            None => res,
        };
        apuntar(r, &res);
        match res {
            Ok(x) => total.sumar(x),
            Err(e) => {
                fallo.get_or_insert(e);
            }
        }
    }
    match fallo {
        Some(e) => Err(e),
        None => Ok(total),
    }
}

/// Sincroniza un repositorio. Con `rotar`, antes de escribir nada estrena esa clave: le da el id
/// siguiente al más alto que conozca el repositorio, la apunta en `omoikane.check` como vigente y
/// cifra con ella todo lo de este equipo. Devuelve las claves con las que lo hizo.
fn sincronizar_repo(db: &mut Connection, r: &Rutas, config: &Config, actual: Option<&str>, modelo: Option<&Modelo>, rotar: Option<&[u8]>) -> Result<(Resumen, Cifra)> {
    let base = leer_claves(r)?;
    ensure!(r.clon.join(".git").exists(), tr!("the sync clone is missing: run `omoi sync init` again", "falta el clon de la sincronización: vuelve a hacer `omoi sync init`"));
    if let Some(a) = actual {
        adoptar(db, a)?;
    }
    let equipo = reloj::equipo(db)?;
    releer_si_hace_falta(db)?;
    for intento in 0..4 {
        // Las cuentas, de cada intento: un reintento vuelve a leer los mismos ficheros.
        let mut res = Resumen { equipo: equipo.clone(), ..Default::default() };
        limpiar(&r.clon)?;
        // El id de la clave nueva, de cada intento: si otro equipo rotó entre medias, la siguiente.
        let cifra = match rotar {
            Some(k) => base.con(Chequeo::leer(&r.clon).map_or(0, |c| c.vigente()).max(base.vigente()) + 1, k.to_vec()),
            None => base.clone(),
        };
        // Un repositorio que pide un lector más nuevo: ni se lee ni se escribe en él.
        if lector_pedido(&r.clon, &cifra).is_some_and(|n| n > LECTOR_REPO) {
            bail!(aviso_version());
        }
        ficheros_fijos(&r.clon, &cifra)?;
        retirar_viejos(&r.clon, &equipo, &cifra)?;
        let ficheros = por_leer(db, &r.clon, &equipo, &cifra, &r.marca_commit)?;
        importar(db, &r.clon, &ficheros, config, &cifra, actual, modelo, &mut res)?;
        // Si esta versión se saltó líneas nuevas y la base la usó antes una más nueva (se volvió a una
        // vieja), esa tendrá que releerlas cuando vuelva.
        if res.de_version_nueva > 0 {
            db.execute("UPDATE meta SET v = ?1 WHERE k = 'sync_lector' AND CAST(v AS INTEGER) > ?1", [LECTOR_LINEAS])?;
        }
        res.exportadas = exportar(db, &r.clon, &equipo, config, &cifra)?;
        git(&r.clon, &["add", "-A"])?;
        if !git(&r.clon, &["status", "--porcelain"])?.trim().is_empty() {
            git(&r.clon, &["commit", "--quiet", "--no-verify", "-m", "omoikane"])?;
        }
        let hay_commits = hay_head(&r.clon);
        let subir = if hay_commits { git(&r.clon, &["push", "--quiet", "origin", &format!("HEAD:{RAMA}")]) } else { Ok(String::new()) };
        match subir {
            Ok(_) => {
                if hay_commits {
                    let head = git(&r.clon, &["rev-parse", "HEAD"])?;
                    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![r.marca_commit, head.trim()])?;
                }
                // Lo que llegó de otros equipos puede dejar anticuadas memorias de aquí (o al revés).
                let _ = crate::revisar::al_dia(db);
                return Ok((res, cifra));
            }
            // Otro equipo subió entre medias: se baja lo suyo y se vuelve a intentar.
            Err(_) if intento < 3 => std::thread::sleep(Duration::from_millis(200 * (intento + 1))),
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

// --- Configurar: init, join, code ------------------------------------------------------------------

/// Lo que se elige al configurar.
#[derive(Clone, Debug)]
pub struct Eleccion {
    pub preferencias: bool,
    pub global: bool,
    /// Carpetas de proyecto; vacío = todos.
    pub proyectos: Vec<String>,
    /// Espacios (nombre o clave) que también van por el repositorio general; vacío = ninguno.
    pub espacios: Vec<String>,
    pub autor: Option<String>,
}

/// El nombre de git del usuario (`git config user.name`), si lo tiene.
pub fn nombre_git() -> Option<String> {
    let o = std::process::Command::new("git").args(["config", "--get", "user.name"]).output().ok()?;
    let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !n.is_empty()).then_some(n)
}

pub fn config_de(url: &str, e: &Eleccion) -> Config {
    config_con_persona(url, e, None, "")
}

/// Como `config_de`, pero con la persona ya decidida (`join` usa la del equipo que invita: así los
/// proyectos sin remoto de una misma persona se juntan entre sus equipos aunque cada uno lleve otro
/// nombre de autor) y, si no hay ni autor ni nombre de git, la persona local de la base (`local`).
pub fn config_con_persona(url: &str, e: &Eleccion, fija: Option<&str>, local: &str) -> Config {
    let mut kinds = vec![Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Procedure];
    if e.preferencias {
        kinds.push(Tipo::Preference);
    }
    // La persona: la que se da; si no, la del nombre; sin nombre, un identificador fijo al azar
    // (guardado en la configuración y pasado en el código para unirse), nunca uno común a todos.
    let person = fija
        .map(persona)
        .filter(|p| !p.is_empty())
        .or_else(|| e.autor.clone().or_else(nombre_git).map(|n| persona(&n)).filter(|p| !p.is_empty()))
        .or_else(|| Some(persona(local)).filter(|p| !p.is_empty()))
        .unwrap_or_else(|| format!("u-{}", ulid::Ulid::generate().to_string()[16..].to_lowercase()));
    let projects = (!e.proyectos.is_empty()).then(|| e.proyectos.iter().map(|p| clave(p, &person)).collect());
    let spaces = (!e.espacios.is_empty()).then(|| e.espacios.iter().map(|s| crate::proyecto::clave_de_espacio(s)).collect());
    Config { url: url.to_string(), kinds, global: e.global, projects, spaces, author: e.autor.clone(), person, space: None }
}

/// Tras `sync init` o `join`: si la persona de la sincronización (`nueva`) no es la de esta base, pasa
/// a serlo (es la misma persona en otro equipo): las claves `dir:` de los proyectos de aquí se vuelven a
/// calcular con ella y sus memorias se vuelven a sellar, para que viajen con la clave buena, y todos
/// los repositorios de este equipo la apuntan. Con `--another-person`, la de la configuración ya es la
/// local y no cambia nada. Devuelve cuántos proyectos cambiaron de clave.
pub fn adoptar_persona(db: &mut Connection, ruta_db: &Path, nueva: &str) -> Result<usize> {
    let vieja = crate::db::persona_de(db);
    if nueva.is_empty() || vieja == nueva {
        return Ok(0);
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('persona', ?1)", [nueva])?;
    let n = crate::proyecto::cambiar_persona(&tx, &vieja, nueva)?;
    tx.commit()?;
    let (de, a) = (format!("dir:{vieja}/"), format!("dir:{nueva}/"));
    for r in repos(ruta_db) {
        let Some(mut c) = leer_config_de(&r)? else { continue };
        c.person = nueva.to_string();
        for k in c.projects.iter_mut().flatten() {
            if let Some(resto) = k.strip_prefix(&de) {
                *k = format!("{a}{resto}");
            }
        }
        escribir_privado(&r.config, &serde_json::to_string_pretty(&c)?)?;
    }
    Ok(n)
}

fn guardar_config(db: &Connection, r: &Rutas, config: &Config, cifra: &Cifra) -> Result<()> {
    if let Some(d) = r.config.parent() {
        std::fs::create_dir_all(d)?;
    }
    guardar_claves(r, cifra)?;
    escribir_privado(&r.config, &serde_json::to_string_pretty(config)?)?;
    // El autor de la base es el del repositorio general.
    if r.espacio.is_none() {
        match &config.author {
            Some(a) => db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', ?1)", [a])?,
            None => db.execute("DELETE FROM meta WHERE k = 'autor'", [])?,
        };
    }
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    Ok(())
}

fn hay_memorias_en(clon: &Path) -> bool {
    std::fs::read_dir(clon).into_iter().flatten().flatten().any(|e| {
        e.path().is_dir()
            && !e.file_name().to_string_lossy().starts_with('.')
            && std::fs::read_dir(e.path()).into_iter().flatten().flatten().any(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
    })
}

fn ya_en_uso() -> String {
    tr!(
        "this repository is already in use by Omoikane: on a computer that already syncs with it run `omoi sync code`, and here `omoi sync join <code>`",
        "Omoikane ya usa este repositorio: en un equipo que ya sincroniza con él ejecuta `omoi sync code` y aquí `omoi sync join <código>`"
    )
}

/// `omoi sync init <url>`: un repositorio nuevo (vacío o sin memorias), con una clave nueva.
pub fn iniciar(db: &mut Connection, ruta_db: &Path, config: &Config) -> Result<()> {
    let r = rutas_de(ruta_db, config.space.as_deref());
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    clonar(&config.url, &r.clon)?;
    if r.clon.join(CHEQUEO).exists() || hay_memorias_en(&r.clon) {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(ya_en_uso());
    }
    guardar_config(db, &r, config, &Cifra::una(1, clave_nueva()?))
}

fn clave_nueva() -> Result<Vec<u8>> {
    let mut clave = vec![0u8; 64];
    getrandom::fill(&mut clave).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(clave)
}

/// El código para unir otro equipo a un repositorio (el general o el de un espacio): lleva la URL y
/// la clave vigente con su id (¡pásalo por un canal privado!). Un repositorio que nunca rotó da el
/// mismo código que antes de las claves con id (sin `id`).
pub fn codigo(ruta_db: &Path, espacio: Option<&str>) -> Result<String> {
    let r = rutas_de(ruta_db, espacio);
    let config = leer_config_de(&r)?.context(tr!("sync is not set up", "la sincronización no está configurada"))?;
    let cifra = leer_claves(&r)?;
    let mut json = serde_json::json!({ "url": config.url, "key": a_hex(cifra.clave_vigente()), "person": config.person });
    if cifra.vigente() != 1 {
        json["id"] = cifra.vigente().into();
    }
    if let Some(e) = &config.space {
        json["space"] = e.clone().into();
    }
    Ok(format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())))
}

/// Lo que lleva un código de `omoi sync code`.
#[derive(Debug, PartialEq)]
pub struct Codigo {
    pub url: String,
    pub clave: Vec<u8>,
    /// El id de la clave (los códigos de antes de las claves con id son de la 1).
    pub id: u32,
    /// La persona del equipo que invita (los códigos de antes no la llevan).
    pub persona: Option<String>,
    /// El espacio del repositorio (`None`: el general).
    pub espacio: Option<String>,
}

/// URL, clave y persona de un código de `omoi sync code`.
pub fn leer_codigo(codigo: &str) -> Result<Codigo> {
    let mal = || anyhow!(tr!("that is not an Omoikane sync code", "eso no es un código de sincronización de Omoikane"));
    let resto = codigo.trim().strip_prefix(PREFIJO_CODIGO).ok_or_else(mal)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(resto).map_err(|_| mal())?;
    let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| mal())?;
    let url = v["url"].as_str().ok_or_else(mal)?.to_string();
    let k = desde_hex(v["key"].as_str().ok_or_else(mal)?).ok_or_else(mal)?;
    ensure!(k.len() == 64, mal());
    let id = match v.get("id") {
        None => 1,
        Some(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n >= 1).ok_or_else(mal)?,
    };
    let espacio = v["space"].as_str().filter(|e| !e.is_empty()).map(espacio_valido).transpose()?;
    Ok(Codigo { url, clave: k, id, persona: v["person"].as_str().filter(|p| !p.is_empty()).map(String::from), espacio })
}

/// `omoi sync join <código>`: el repositorio y la clave de otro equipo. Comprueba que la clave abre
/// el repositorio antes de guardar nada.
pub fn unirse(db: &mut Connection, ruta_db: &Path, config: &Config, id: u32, clave: &[u8]) -> Result<()> {
    let r = rutas_de(ruta_db, config.space.as_deref());
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    clonar(&config.url, &r.clon)?;
    let cifra = Cifra::una(id, clave.to_vec());
    let vale = match Chequeo::leer(&r.clon) {
        Some(ch) => ch.lineas.get(&id).is_some_and(|l| cifra.abre(id, l)),
        // Un repositorio sin comprobación (o aún vacío): la primera línea que haya.
        None => std::fs::read_dir(&r.clon)?
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
            .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
            .find_map(|f| std::fs::read_to_string(f.path()).ok().and_then(|t| t.lines().next().map(String::from)))
            .is_none_or(|l| cifra.descifrar(&l).is_ok()),
    };
    if !vale {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(clave_ajena());
    }
    guardar_config(db, &r, config, &cifra)
}

/// `omoi sync join <código>` en un equipo que ya sincroniza con ese repositorio (tras una rotación):
/// guarda la clave nueva junto a las que ya tenía, que siguen sirviendo para leer lo que otros aún no
/// han vuelto a cifrar, y la siguiente sincronización vuelve a leerlo todo y recifra lo de este
/// equipo con ella. `false` si ya la tenía.
pub fn anadir_clave(db: &Connection, ruta_db: &Path, espacio: Option<&str>, id: u32, clave: &[u8]) -> Result<bool> {
    let r = rutas_de(ruta_db, espacio);
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    let otro = || {
        anyhow!(tr!(
            "that code is for another repository, and this computer already syncs one here (`omoi sync status`; `omoi sync off` to start over)",
            "ese código es de otro repositorio, y este equipo ya sincroniza uno aquí (`omoi sync status`; `omoi sync off` para empezar de nuevo)"
        ))
    };
    let cifra = leer_claves(&r)?;
    if let Some(k) = cifra.claves.get(&id) {
        ensure!(k.as_slice() == clave, otro());
        return Ok(false);
    }
    limpiar(&r.clon)?;
    let nueva = Cifra::una(id, clave.to_vec());
    ensure!(Chequeo::leer(&r.clon).and_then(|c| c.lineas.get(&id).cloned()).is_some_and(|l| nueva.abre(id, &l)), otro());
    guardar_claves(&r, &cifra.con(id, clave.to_vec()))?;
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    Ok(true)
}

/// `omoi sync rotate`: una clave nueva para un repositorio. La apunta como vigente en
/// `omoikane.check`, vuelve a cifrar con ella todo lo de este equipo (y borra sus ficheros con la
/// vieja), hace commit y push, y la guarda aquí junto a las viejas. Desde entonces `omoi sync code` da
/// el código de la nueva; quien solo tiene la vieja ya no lee lo que se escriba con ella. Devuelve el
/// id de la clave nueva.
pub fn rotar(db: &mut Connection, ruta_db: &Path, espacio: Option<&str>, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<(u32, Resumen)> {
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    let r = rutas_de(ruta_db, espacio);
    let config = leer_config_de(&r)?.context(no_configurada())?;
    let nueva = clave_nueva()?;
    let res = sincronizar_repo(db, &r, &config, actual, modelo, Some(&nueva));
    let (res, cifra) = match res {
        Ok((res, cifra)) => (Ok(res), Some(cifra)),
        Err(e) => (Err(e), None),
    };
    // Solo con la subida hecha: si no, el repositorio no sabe nada de la clave nueva.
    if let Some(c) = &cifra {
        guardar_claves(&r, c)?;
    }
    apuntar(&r, &res);
    Ok((cifra.map_or(0, |c| c.vigente()), res?))
}

/// Deja de sincronizar un repositorio (el general o el de un espacio): borra su configuración, sus
/// claves y su clon (las memorias de aquí se quedan).
pub fn desactivar(db: &Connection, ruta_db: &Path, espacio: Option<&str>) -> Result<()> {
    let r = rutas_de(ruta_db, espacio);
    let c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    for f in [&r.config, &r.clave, &r.estado] {
        let _ = std::fs::remove_file(f);
    }
    if r.clon.exists() {
        std::fs::remove_dir_all(&r.clon)?;
    }
    match &r.espacio {
        Some(_) => {
            if let Some(d) = r.config.parent() {
                let _ = std::fs::remove_dir_all(d);
            }
        }
        None => {
            db.execute("DELETE FROM meta WHERE k = 'autor'", [])?;
        }
    }
    if repos(ruta_db).is_empty() {
        let _ = std::fs::remove_file(&r.pendiente);
        let _ = std::fs::remove_file(&r.hecho);
    }
    db.execute("DELETE FROM meta WHERE k = ?1", [&r.marca_commit])?;
    drop(c);
    Ok(())
}

fn otra_en_marcha() -> String {
    tr!("another sync is running; try again in a moment", "hay otra sincronización en marcha; prueba dentro de un momento")
}

// --- En segundo plano ------------------------------------------------------------------------------

/// Cómo fue la última sincronización.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Estado {
    pub ultimo_ok: Option<String>,
    pub ultimo_error: Option<String>,
    /// Desde cuándo falla (la primera vez seguida).
    pub error_desde: Option<String>,
    /// Algo que conviene saber aunque haya ido bien (líneas que no se pudieron leer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aviso: Option<String>,
    /// Ficheros del repositorio con líneas que no se pudieron leer, y cuántas: se quedan hasta que una
    /// sincronización vuelva a leer el fichero y esté bien.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_malos: BTreeMap<String, usize>,
    /// Lo mismo con las líneas de una versión más nueva de omoi.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_futuros: BTreeMap<String, usize>,
    /// Lo mismo con las líneas rechazadas por tocar algo de otro espacio (`Resumen::conflictos_de_espacio`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_conflicto: BTreeMap<String, usize>,
}

impl Estado {
    /// Cuántas líneas de una versión más nueva quedan sin leer en el repositorio.
    pub fn de_version_nueva(&self) -> usize {
        self.ficheros_futuros.values().sum()
    }

    /// Cuántas líneas del repositorio se rechazaron por tocar algo de otro espacio.
    pub fn conflictos_de_espacio(&self) -> usize {
        self.ficheros_conflicto.values().sum()
    }
}

/// Cómo fue la última sincronización del repositorio general.
pub fn leer_estado(ruta_db: &Path) -> Estado {
    leer_estado_de(&rutas(ruta_db))
}

pub fn leer_estado_de(r: &Rutas) -> Estado {
    std::fs::read_to_string(&r.estado).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn ahora() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    s.to_string()
}

fn segundos(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

fn apuntar(r: &Rutas, resultado: &Result<Resumen>) {
    let mut e = leer_estado_de(r);
    match resultado {
        Ok(r) => {
            e.ultimo_ok = Some(ahora());
            e.ultimo_error = None;
            e.error_desde = None;
            for (cuentas, nuevas) in [(&mut e.ficheros_malos, &r.malos), (&mut e.ficheros_futuros, &r.futuros), (&mut e.ficheros_conflicto, &r.conflictos)] {
                for (f, n) in nuevas {
                    if *n == 0 {
                        cuentas.remove(f);
                    } else {
                        cuentas.insert(f.clone(), *n);
                    }
                }
            }
            let malas: usize = e.ficheros_malos.values().sum();
            let futuras = e.de_version_nueva();
            let mut avisos = Vec::new();
            if malas > 0 {
                avisos.push(tr!(
                    "{} shared line(s) couldn't be read and were skipped (another key, or not valid)",
                    "{} línea(s) compartidas no se pudieron leer y se saltaron (otra clave, o no válidas)",
                    malas
                ));
            }
            if futuras > 0 {
                avisos.push(aviso_lineas_nuevas(futuras));
            }
            let conflictos = e.conflictos_de_espacio();
            if conflictos > 0 {
                avisos.push(tr!(
                    "{} shared line(s) rejected: they touch a project or memory that is in another space here (nothing from one space reaches another)",
                    "{} línea(s) compartidas rechazadas: tocan un proyecto o una memoria que aquí es de otro espacio (nada de un espacio llega a otro)",
                    conflictos
                ));
            }
            e.aviso = (!avisos.is_empty()).then(|| avisos.join(" · "));
        }
        Err(err) => {
            e.ultimo_error = Some(sin_credenciales(&format!("{err:#}")));
            e.error_desde.get_or_insert_with(ahora);
        }
    }
    if let Ok(t) = serde_json::to_string_pretty(&e) {
        let _ = std::fs::write(&r.estado, t);
    }
}

/// Hay cambios sin subir: la última marca de cambio no es la que ya se sincronizó.
pub fn pendiente(ruta_db: &Path) -> bool {
    let r = rutas(ruta_db);
    match std::fs::read_to_string(&r.pendiente) {
        Ok(m) => std::fs::read_to_string(&r.hecho).ok().as_deref() != Some(m.as_str()),
        Err(_) => false,
    }
}

/// La primera línea de un error, sin credenciales ni lo que el servidor añada (`remote: …`), que no
/// debe llegar al contexto del agente.
fn error_corto(e: &str) -> String {
    let l = e.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("remote:")).unwrap_or_default();
    let l = l.split("remote:").next().unwrap_or_default().trim();
    sin_credenciales(&l.chars().take(200).collect::<String>())
}

/// «N líneas de una versión más nueva: actualiza omoi».
pub fn aviso_lineas_nuevas(n: usize) -> String {
    tr!(
        "{} line(s) from a newer version: update omoi (`omoi upgrade`)",
        "{} línea(s) de una versión más nueva: actualiza omoi (`omoi upgrade`)",
        n
    )
}

/// Un aviso para el inicio de sesión: si algún repositorio pide una versión más nueva de omoi (en
/// cuanto se sabe), o si la sincronización lleva más de una hora fallando.
pub fn aviso(ruta_db: &Path) -> Option<String> {
    if repos(ruta_db).iter().any(|r| necesita_version(r).is_some()) {
        return Some(
            "An Omoikane sync repository needs a newer version of omoi: nothing is written to it until omoi is updated (`omoi upgrade`). Tell the user; `omoi sync status` shows more."
                .into(),
        );
    }
    leer_config(ruta_db).ok().flatten()?;
    let e = leer_estado(ruta_db);
    let desde = segundos(e.error_desde.as_deref()?);
    let ahora = segundos(&ahora());
    (ahora.saturating_sub(desde) >= 3600).then(|| {
        format!(
            "Omoikane sync has been failing for {} h (last error: {}). Tell the user; `omoi sync status` shows more.",
            (ahora - desde) / 3600,
            error_corto(e.ultimo_error.as_deref().unwrap_or_default())
        )
    })
}

/// Tras un cambio: si la sincronización está configurada, deja una marca de pendiente nueva y lanza
/// `omoi sync --background` aparte, sin esperar. No falla nunca (lo peor es no sincronizar ahora).
pub fn programar(ruta_db: &Path) {
    let r = rutas(ruta_db);
    // `OMOIKANE_SYNC_AUTO=0`: solo a mano (`omoi sync`).
    if (!r.config.exists() && espacios(ruta_db).is_empty()) || std::env::var("OMOIKANE_SYNC_AUTO").is_ok_and(|v| v == "0") {
        return;
    }
    // Cada cambio, una marca distinta: el proceso de fondo repite mientras la marca no sea la que ya
    // sincronizó (sin borrarla nunca, así no se pierde una que llegue justo en medio).
    let marca = ulid::Ulid::generate().to_string();
    if std::fs::write(&r.pendiente, marca).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(["sync", "--background"])
        .env("OMOIKANE_DB", ruta_db)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP: con una consola oculta propia (así las órdenes
        // de git que lance tampoco abren ventanas) y sin morir con un Ctrl+C de quien lo lanza.
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_heredar_salidas();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Su propio grupo de procesos: un Ctrl+C en la terminal de quien lo lanzó no lo corta.
        c.process_group(0);
    }
    if let Ok(mut hijo) = c.spawn() {
        // Quien espera al hijo es un hilo: en procesos largos (`omoi mcp`, `omoi serve`) no quedan
        // procesos zombis; en uno corto, al salir, el sistema lo adopta.
        std::thread::spawn(move || {
            let _ = hijo.wait();
        });
    }
}

/// Lanza `omoi <args>` aparte, sin ventana, sin esperarlo y sin que se quede con las salidas de quien
/// llama (un hook las lee hasta el final). Lo usa la actualización diaria (`actualizar`).
pub(crate) fn lanzar_aparte(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_heredar_salidas();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    if let Ok(mut hijo) = c.spawn() {
        std::thread::spawn(move || {
            let _ = hijo.wait();
        });
    }
}

/// En Windows un proceso hijo hereda todos los handles heredables, también la entrada y las salidas
/// de este proceso aunque al hijo se le den otras. Si quien llamó a `omoi` (un hook, un agente) lee
/// esas salidas, esperaría a que acabe la sincronización en segundo plano. Este proceso termina
/// enseguida, así que se marcan como no heredables antes de lanzarla.
#[cfg(windows)]
fn no_heredar_salidas() {
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn SetHandleInformation(h: isize, mascara: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE y STD_ERROR_HANDLE (-10, -11, -12).
    for n in [-10i32, -11, -12] {
        // SAFETY: llamadas de la API de Windows con un handle que devuelve el propio sistema.
        unsafe {
            let h = GetStdHandle(n as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Si un proceso sigue vivo.
#[cfg(unix)]
fn vivo(pid: u32) -> bool {
    // SAFETY: kill con la señal 0 solo comprueba si el proceso existe.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn vivo(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(acceso: u32, heredar: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(h: isize, codigo: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: llamadas de la API de Windows; el handle se cierra siempre.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut codigo = 0u32;
        let ok = GetExitCodeProcess(h, &mut codigo);
        CloseHandle(h);
        ok != 0 && codigo == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn vivo(_pid: u32) -> bool {
    true
}

/// Un cerrojo por equipo: un fichero creado en exclusiva con el pid, la hora y una ficha propia. Uno
/// de un proceso que ya no existe, o sin renovar en 10 minutos, se da por abandonado. Mientras se tiene,
/// un hilo lo renueva; al soltarlo solo se borra si sigue siendo el nuestro.
pub struct Cerrojo {
    ruta: PathBuf,
    ficha: String,
    parar: Arc<AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
}

const CERROJO_CADUCA: u64 = 600;

impl Cerrojo {
    fn contenido(ficha: &str) -> String {
        format!("{}\n{}\n{ficha}\n", std::process::id(), ahora())
    }

    fn abandonado(p: &Path) -> bool {
        let Ok(t) = std::fs::read_to_string(p) else { return false };
        let mut l = t.lines();
        let pid = l.next().and_then(|x| x.trim().parse::<u32>().ok());
        let cuando = l.next().map(|x| segundos(x.trim())).unwrap_or(0);
        let viejo = segundos(&ahora()).saturating_sub(cuando) > CERROJO_CADUCA;
        match pid {
            Some(pid) => !vivo(pid) || viejo,
            // Un cerrojo a medio escribir (o de una versión anterior, vacío): por su fecha.
            None => std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(CERROJO_CADUCA)),
        }
    }

    pub fn tomar(ruta_db: &Path) -> Option<Cerrojo> {
        let p = rutas(ruta_db).cerrojo;
        let ficha = ulid::Ulid::generate().to_string();
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(f) => {
                    use std::io::Write;
                    let mut f = f;
                    let _ = f.write_all(Cerrojo::contenido(&ficha).as_bytes());
                    drop(f);
                    let parar = Arc::new(AtomicBool::new(false));
                    let hilo = {
                        let (p, ficha, parar) = (p.clone(), ficha.clone(), parar.clone());
                        std::thread::spawn(move || {
                            let mut n = 0u32;
                            while !parar.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(200));
                                n += 1;
                                // Cada minuto, si sigue siendo nuestro, se renueva la hora.
                                if n.is_multiple_of(300) && std::fs::read_to_string(&p).is_ok_and(|t| t.contains(&ficha)) {
                                    let _ = std::fs::write(&p, Cerrojo::contenido(&ficha));
                                }
                            }
                        })
                    };
                    return Some(Cerrojo { ruta: p, ficha, parar, hilo: Some(hilo) });
                }
                Err(_) => {
                    let visto = std::fs::read_to_string(&p).ok();
                    if !Cerrojo::abandonado(&p) {
                        return None;
                    }
                    // Se aparta con un nombre propio (renombrar es atómico: si dos lo ven abandonado,
                    // solo uno lo consigue) y se comprueba que lo apartado es lo que se juzgó abandonado.
                    let aparte = p.with_extension(format!("lock.{ficha}"));
                    if std::fs::rename(&p, &aparte).is_err() {
                        return None;
                    }
                    if std::fs::read_to_string(&aparte).ok() != visto {
                        // Otro ya lo había tomado entre medias: se le devuelve.
                        let _ = std::fs::rename(&aparte, &p);
                        return None;
                    }
                    let _ = std::fs::remove_file(&aparte);
                }
            }
        }
        None
    }

    /// Espera hasta `max` a que quede libre.
    pub fn esperar(ruta_db: &Path, max: Duration) -> Option<Cerrojo> {
        let inicio = Instant::now();
        loop {
            if let Some(c) = Cerrojo::tomar(ruta_db) {
                return Some(c);
            }
            if inicio.elapsed() >= max {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Cerrojo {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(h) = self.hilo.take() {
            let _ = h.join();
        }
        if std::fs::read_to_string(&self.ruta).is_ok_and(|t| t.contains(&self.ficha)) {
            let _ = std::fs::remove_file(&self.ruta);
        }
    }
}

fn espera() -> Duration {
    Duration::from_millis(std::env::var("OMOIKANE_SYNC_ESPERA_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000))
}

/// Apunta que la marca de cambio `marca` ya está sincronizada.
fn hecho(r: &Rutas, marca: Option<&str>) {
    if let Some(m) = marca {
        let _ = std::fs::write(&r.hecho, m);
    }
}

/// `omoi sync --background`: espera unos segundos para juntar cambios, sincroniza, y repite si
/// mientras tanto llegaron más. Si ya hay otro en marcha, se va (ese verá la marca nueva). `abrir`
/// no necesita cargar el modelo: lo que llegue sin vector lo completa la siguiente orden que lo abra.
pub fn en_segundo_plano(ruta_db: &Path, abrir: impl Fn() -> Result<(Connection, Option<Modelo>)>) -> Result<()> {
    let Some(_cerrojo) = Cerrojo::tomar(ruta_db) else { return Ok(()) };
    let r = rutas(ruta_db);
    for _ in 0..20 {
        std::thread::sleep(espera());
        let marca = std::fs::read_to_string(&r.pendiente).ok();
        // `sincronizar` apunta cómo fue cada repositorio; un fallo al abrir la base, en el general.
        let resultado = match abrir() {
            Ok((mut db, modelo)) => sincronizar(&mut db, ruta_db, None, modelo.as_ref()),
            Err(e) => {
                let e = Err(e);
                apuntar(&r, &e);
                e
            }
        };
        if resultado.is_err() {
            break;
        }
        hecho(&r, marca.as_deref());
        if !pendiente(ruta_db) {
            break;
        }
    }
    Ok(())
}

/// `omoi sync` a mano: espera a que acabe uno en segundo plano si lo hay, y apunta el resultado.
pub fn ahora_mismo(db: &mut Connection, ruta_db: &Path, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<Resumen> {
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    let r = rutas(ruta_db);
    // Un cambio hecho mientras tanto lanza una sincronización de fondo que se va enseguida (este tiene
    // el cerrojo): se repite aquí mientras la marca de pendiente avance.
    let mut vueltas = 0;
    loop {
        let marca = std::fs::read_to_string(&r.pendiente).ok();
        let res = sincronizar(db, ruta_db, actual, modelo)?;
        hecho(&r, marca.as_deref());
        vueltas += 1;
        if !pendiente(ruta_db) || vueltas >= 5 {
            return Ok(res);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotos_iguales() {
        let a = normalizar_remoto("git@github.com:Marc/Tienda.git");
        assert_eq!(a, "github.com/marc/tienda");
        assert_eq!(normalizar_remoto("https://github.com/marc/tienda"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com/marc/tienda.git/"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com:22/marc/tienda.git"), a);
    }

    #[test]
    fn cifrado_determinista_y_clave_mala() {
        let c = Cifra::una(1, (0..64).collect());
        let x = c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap();
        assert_eq!(x, c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap(), "la misma línea cifra igual");
        assert_eq!(c.descifrar(&x).unwrap(), "{\"hola\":\"mundo, una línea\"}");
        let otra = Cifra::una(1, (1..65).collect());
        assert!(otra.descifrar(&x).is_err());
        assert_ne!(c.carpeta("git:a/b"), otra.carpeta("git:a/b"));
        assert!(c.fichero("marc-pc-abc").starts_with("m-") && !c.fichero("marc-pc-abc").contains("marc"));
        assert_ne!(c.fichero("a"), c.fichero("b"));
        assert!(c.carpeta("git:a/b").starts_with("p-"));
    }

    #[test]
    fn codigo_va_y_vuelve() {
        let k: Vec<u8> = (0..64).collect();
        let json = serde_json::json!({ "url": "git@x:y/z.git", "key": a_hex(&k) });
        let c = format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap(), Codigo { url: "git@x:y/z.git".to_string(), clave: k.clone(), id: 1, persona: None, espacio: None });
        let json = serde_json::json!({ "url": "u", "key": a_hex(&k), "person": "marc" });
        let c = format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap().persona.as_deref(), Some("marc"));
        assert!(leer_codigo("hola").is_err());
    }

    #[test]
    fn personas() {
        assert_eq!(persona("Marc Mayol"), "marc-mayol");
        assert_eq!(persona("  "), "");
        let e = Eleccion { preferencias: false, global: true, proyectos: vec![], espacios: vec![], autor: Some(" ".into()) };
        let c = config_de("x", &e);
        assert!(c.person.starts_with("u-") || !c.person.is_empty(), "nunca vacía: {}", c.person);
        assert_eq!(config_con_persona("x", &e, Some("Ana López"), "").person, "ana-lópez");
        assert_eq!(config_con_persona("x", &e, None, "u-1234abcd").person, "u-1234abcd", "sin autor, la persona local");
        assert!(id_valido("01M62EWVHRXM0000000000ABCD"));
        assert!(!id_valido("01m62ewvhrxm0000000000abcd") && !id_valido("CORTO") && !id_valido("01M62EWVHRXM0000000000ABCÑ"));
    }

    #[test]
    fn credenciales_fuera() {
        assert_eq!(sin_credenciales("https://ghp_x:y@github.com/a/b.git"), "https://***@github.com/a/b.git");
        assert_eq!(sin_credenciales("git clone https://tok@host/r falló; ssh://git@h:22/r"), "git clone https://***@host/r falló; ssh://***@h:22/r");
        assert_eq!(sin_credenciales("git@github.com:a/b.git"), "git@github.com:a/b.git");
        assert_eq!(error_corto("git push: remote: Permission to x denied to ghp\nmás"), "git push:");
    }

    #[test]
    fn rutas_por_base() {
        let a = rutas(Path::new("/x/omoikane.db"));
        assert!(a.config.ends_with("sync.json") && a.clon.ends_with("sync"));
        let b = rutas(Path::new("/x/trabajo.db"));
        assert!(b.config.ends_with("trabajo.sync.json") && b.clon.ends_with("trabajo.sync") && b.cerrojo.ends_with("trabajo.sync.lock"));
        let e = rutas_de(Path::new("/x/trabajo.db"), Some("acme"));
        assert!(e.config.ends_with("trabajo.sync-spaces/acme/sync.json") && e.clon.ends_with("trabajo.sync-spaces/acme/repo"), "{}", e.config.display());
        assert_eq!(e.cerrojo, b.cerrojo, "un solo cerrojo por base");
        assert_ne!(e.marca_commit, b.marca_commit);
        assert_eq!(espacio_valido(" Acme-2 ").unwrap(), "acme-2");
        assert!(espacio_valido("../x").is_err() && espacio_valido("").is_err() && espacio_valido("-x").is_err());
    }

    #[test]
    fn lineas_con_id_de_clave() {
        let k1: Vec<u8> = (0..64).collect();
        let k2: Vec<u8> = (64..128).collect();
        let vieja = Cifra::una(1, k1.clone());
        let ambas = vieja.con(2, k2.clone());
        assert_eq!(ambas.vigente(), 2);
        // La 1 sin prefijo (igual que antes); las demás con `k<id>:`.
        let l1 = vieja.cifrar("hola").unwrap();
        assert!(!l1.contains(':'));
        let l2 = ambas.cifrar("hola").unwrap();
        assert!(l2.starts_with("k2:"), "{l2}");
        assert_eq!(ambas.descifrar(&l1).unwrap(), "hola", "con las dos se lee lo viejo");
        assert_eq!(ambas.descifrar(&l2).unwrap(), "hola");
        assert!(vieja.descifrar(&l2).unwrap_err().to_string().contains("k2"), "sin la nueva no se lee, y lo dice");
        assert_eq!(vieja.descifrar(&format!("k1:{l1}")).unwrap(), "hola", "`k1:` también vale");
        assert!(ambas.descifrar(&format!("x9:{l1}")).is_err(), "una etiqueta desconocida es de un formato futuro");
        assert_eq!(Cifra::una(2, k2.clone()).fichero("pc"), ambas.fichero("pc"));
        assert_eq!(ambas.ficheros_viejos("pc"), vec![vieja.fichero("pc")]);
    }

    #[test]
    fn el_chequeo_de_siempre_y_el_de_varias_claves() {
        let k1: Vec<u8> = (0..64).collect();
        let vieja = Cifra::una(1, k1.clone());
        // Con solo la 1: una línea, como siempre.
        let mut ch = Chequeo::default();
        ch.lineas.insert(1, vieja.cifrar_con(1, &texto_chequeo(1)).unwrap());
        let t = ch.texto();
        assert_eq!(t.lines().count(), 1);
        assert!(Chequeo::de_texto(&t).comprobar(&vieja) == (1, vec![]));
        // Con la 2: la cabecera con la vigente, y quien solo tiene la 1 sabe que existe.
        let ambas = vieja.con(2, (64..128).collect());
        ch.lineas.insert(2, ambas.cifrar_con(2, &texto_chequeo(2)).unwrap());
        let t = ch.texto();
        assert!(t.starts_with("omoikane-check-v2 current=2\n"), "{t}");
        let leido = Chequeo::de_texto(&(t.clone() + "omoikane-check-v3 algo=nuevo\nz7:futuro\n"));
        assert_eq!(leido.vigente(), 2);
        assert_eq!(leido.comprobar(&vieja), (1, vec![]));
        assert_eq!(leido.comprobar(&ambas), (2, vec![]));
        // Otra clave con el mismo id no pasa, ni la línea de una clave puesta como la de otra.
        assert_eq!(leido.comprobar(&vieja.con(2, (1..65).collect())), (1, vec![2]));
        assert!(!ambas.abre(2, &ch.lineas[&1]));
    }
}
