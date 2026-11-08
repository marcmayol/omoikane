//! Verificada por una persona frente a escrita por un agente (1.8.5, de OKF Agent Memory: sus
//! niveles `generated`/`verified` con `verified.by`).
//!
//! Casi todo lo que hay en la memoria lo escribe un agente. Que una persona diga «esto es así» vale
//! más, y el agente que la recibe tiene que poder distinguirlo. La verificación es un sello en
//! `memory.extra` con la clave [`CLAVE`]: quién, cuándo y la huella del texto que vio (tipo, título y
//! contenido). Sin cambiar el esquema y con el mismo camino que los datos (`data`): viaja como un
//! campo `verified` de la línea, y una 1.0–1.8 lo guarda y lo reenvía sin entenderlo.
//!
//! **Un agente no puede verificarse a sí mismo.** Ninguna herramienta MCP lo hace ni lo lee de lo
//! que recibe (`remember`, `update` y `data` no llegan a esta clave, y `update` sobre el texto la
//! quita); el endpoint `/tools/` de `omoi serve` es el MCP, así que tampoco. Verifican la persona en
//! la CLI (`omoi verify`, que en una terminal pregunta y fuera de ella pide `--yes`, y que se niega
//! dentro de la shell de un agente) y en la vista web (un botón, `POST /api/verify`, que pide el
//! `Origin` de la vista y el token que `omoi serve` mete en la página). Además llegan sellos al
//! importar, nunca dentro de la shell de un agente: `omoi import --okf` (el `verified` de una persona
//! de un bundle firmado por esta base y con el mismo texto, o de otro con `--trust-verified`; ver
//! `okf`) y `omoi import <fichero>` (los sellos del fichero). La sincronización trae los de los otros
//! equipos de la persona, por su repositorio cifrado: son su confianza. Un agente con shell que
//! quiera saltárselo a propósito puede (como puede editar la base o cargar la página para leer el
//! token): esto no es una frontera de seguridad sino que ningún camino normal del agente acabe en
//! «verificada».
//!
//! **Cambiar el texto la des-verifica.** La persona verificó ese texto, no el que venga después: si
//! un agente lo cambia, la memoria vuelve a ser «escrita por un agente» hasta que alguien la mire
//! otra vez. Cambiar etiquetas, rutas, fijarla o moverla no la toca (no cambia lo que dice). Esta
//! versión quita el sello al cambiar el texto; y como el sello lleva la huella del texto, un cambio
//! hecho por una versión anterior (que conserva el sello sin entenderlo) también la deja sin
//! verificar: al leerla, la huella ya no coincide.

use crate::memoria::{AHORA, Memoria};
use crate::tr;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// La clave del sello en `memory.extra` (y el campo `verified` de la línea que viaja).
pub const CLAVE: &str = "verified";

/// La marca local (en `meta`, que no viaja) de un sello de verificada de otra persona que llegó por el
/// repositorio de un espacio (`sync::sello_de_otro`): `sello_ajeno:<id>` con su `by` y su `at` en dos
/// líneas. Mientras la memoria lleve ese mismo sello, aquí no cuenta como verificada
/// (`memoria::COLUMNAS`); el sello sigue en la memoria, tal cual, para los demás.
pub const SELLO_AJENO: &str = "sello_ajeno:";

/// Cuándo la persona le quitó la verificación por última vez (`memory.extra`), para que reimportar un
/// bundle viejo no la vuelva a verificar.
pub const DESVERIFICADA: &str = "unverified_at";

/// Lo que se enseña: quién la verificó y cuándo. Solo si la verificación sigue valiendo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verificada {
    pub by: String,
    pub at: String,
}

/// Lo que se guarda: lo anterior, la huella del texto verificado y la firma de quien lo puso ([`Llaves`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Sello {
    by: String,
    at: String,
    #[serde(default)]
    sha: String,
    /// HMAC-SHA256 del sello (id de la memoria, `by`, `at` y `sha`) con la clave de sellos de la
    /// persona que lo puso: dice que es suyo sin fiarse de `by`, que cualquiera puede escribir.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    mac: String,
}

// --- De quién es un sello ----------------------------------------------------------------------------
//
// Por el repositorio de un espacio (compartido con otras personas) llegan sellos que puso otra persona:
// su confianza, no la de quien usa esta base (`sync::sello_de_otro`). El `by` de la línea no sirve para
// saberlo: cualquiera que comparta el espacio puede escribir `"by": "Marc"`. Lo que decide es una firma
// que solo puede calcular esta persona: un HMAC del sello con su **clave de sellos**, que sale de la
// clave de su repositorio general (el personal, cifrado, que solo tienen sus equipos: nunca viaja por
// el de un espacio) y por eso es la misma en todos sus equipos. Sin repositorio general, una clave
// aleatoria de esta base (`meta('sello_firma')`, que no viaja): sus sellos valen aquí y en ningún otro
// equipo. Un sello sin firma, con la de otra persona o copiado de otra memoria (la firma cubre el id) o
// de otro texto (cubre la huella) no es de esta persona.
//
// **Cuando cambian las claves** (1.8.6). Rotar la clave del general, unirse a uno (`join`), crearlo
// (`init`) o apagarlo (`sync off`) cambia la clave de sellos. Para que las verificaciones de la persona
// no dejen de contar:
// - Las claves de sellos que salieron de un repositorio general de esta persona se quedan apuntadas en
//   `meta('sello_firma_antes')` (local, como la propia): siguen valiendo para comprobar sellos aunque ya
//   no esté su clave (`sync off`, otro general). Nunca se firma con ellas: solo con la vigente.
// - Tras el cambio ([`tras_cambio_de_claves`]), los sellos que esta base tiene por suyos (su firma vale
//   con alguna de sus claves y no está marcado como de otra persona) se vuelven a firmar con la clave
//   vigente del general, para que los acepten los equipos que solo tienen esa (uno que se une después
//   de rotarla). Y los marcados como de otra persona (`SELLO_AJENO`) se vuelven a mirar con las claves
//   de ahora: uno firmado con una clave que este equipo acaba de recibir (la nueva de una rotación hecha
//   en otro equipo) era suyo. Ninguno pasa a ser suyo sin una firma que valga.
// - El general de otra persona (`join --another-person`) no da clave de sellos: su clave la tiene
//   también la otra persona, y lo que firmara con ella contaría aquí como propio. Esta base sigue con
//   la suya (la propia y las apuntadas).

/// La clave de esta base para firmar sellos sin repositorio general (hex, 32 bytes).
const META_SELLOS: &str = "sello_firma";
/// Las claves de sellos de los repositorios generales que tuvo esta base (hex, una por línea, de la más
/// vieja a la más nueva): siguen valiendo para comprobar.
const META_ANTES: &str = "sello_firma_antes";
/// Lo que se firma con la clave del repositorio general para sacar la de sellos (nunca se usa esa
/// clave tal cual: es la del cifrado).
const DERIVAR: &[u8] = b"omoikane-verified-seal-key-1";

/// Las claves de sellos de esta persona en este equipo: con cuál firma y con cuáles valen.
pub(crate) struct Llaves {
    firmar: Option<Vec<u8>>,
    validas: Vec<Vec<u8>>,
    /// Si `firmar` sale del repositorio general (la tienen todos los equipos de la persona).
    de_general: bool,
}

fn hmac(clave: &[u8], texto: &[u8]) -> Vec<u8> {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, clave), texto).as_ref().to_vec()
}

/// Las claves de sellos que salen del repositorio general de esta base, de la más vieja a la vigente
/// (ninguna sin general, o si es el de otra persona).
fn del_general(db: &Connection) -> Vec<Vec<u8>> {
    let Some(ruta) = db.path().filter(|p| !p.is_empty()) else { return Vec::new() };
    let r = crate::sync::rutas(std::path::Path::new(ruta));
    if !crate::proyecto::existe(&r.clave) || crate::sync::leer_config_de(&r).ok().flatten().is_some_and(|c| c.another_person) {
        return Vec::new();
    }
    match crate::sync::leer_claves(&r) {
        Ok(c) => c.por_orden().map(|k| hmac(k, DERIVAR)).collect(),
        Err(_) => Vec::new(),
    }
}

/// Las claves de sellos apuntadas de generales anteriores ([`META_ANTES`]).
fn apuntadas(db: &Connection) -> Result<Vec<Vec<u8>>> {
    let v: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META_ANTES], |f| f.get(0)).optional()?;
    Ok(v.unwrap_or_default().lines().filter_map(de_hex).filter(|k| k.len() == 32).collect())
}

/// Apunta en [`META_ANTES`] las claves de sellos del repositorio general de ahora que aún no estén: así
/// siguen valiendo si su clave se va (`sync off`, otro general). Lo llaman `sync off` (antes de borrar la
/// clave) y [`tras_cambio_de_claves`].
pub(crate) fn recordar(db: &Connection) -> Result<()> {
    let mut v = apuntadas(db)?;
    let n = v.len();
    for k in del_general(db) {
        if !v.contains(&k) {
            v.push(k);
        }
    }
    if v.len() != n {
        let texto: Vec<String> = v.iter().map(|k| a_hex(k)).collect();
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META_ANTES, texto.join("\n")])?;
    }
    Ok(())
}

/// Las claves de sellos de esta base: una por cada clave del repositorio general (con la vigente se
/// firma; con las de antes de rotarla siguen valiendo los sellos de entonces), las de generales
/// anteriores ([`META_ANTES`], solo para comprobar) y la propia de la base (con la que se firma si no
/// hay repositorio general). Con `crear`, la de la base se crea si hace falta para firmar.
pub(crate) fn llaves(db: &Connection, crear: bool) -> Result<Llaves> {
    let mut l = Llaves { firmar: None, validas: Vec::new(), de_general: false };
    for d in del_general(db) {
        l.validas.push(d.clone());
        l.firmar = Some(d);
        l.de_general = true;
    }
    for k in apuntadas(db)? {
        if !l.validas.contains(&k) {
            l.validas.push(k);
        }
    }
    let hay: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META_SELLOS], |f| f.get(0)).optional()?;
    let mut propia = hay.as_deref().and_then(de_hex).filter(|k| k.len() == 32);
    if propia.is_none() && crear && l.firmar.is_none() {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META_SELLOS, a_hex(&b)])?;
        propia = Some(b.to_vec());
    }
    if let Some(p) = propia {
        if l.firmar.is_none() {
            l.firmar = Some(p.clone());
        }
        l.validas.push(p);
    }
    Ok(l)
}

/// Tras cambiar las claves del repositorio general (ver arriba): apunta las de sellos que salen de él
/// ([`recordar`]), quita la marca de otra persona ([`SELLO_AJENO`]) a los sellos cuya firma vale ahora con
/// alguna de las claves de esta base, y vuelve a firmar con la vigente del general los sellos que esta
/// base tiene por suyos y estaban firmados con otra. Volver a firmar cambia el sello de tiempo de la
/// memoria: la próxima sincronización lo lleva. Devuelve cuántas marcas quitó y cuántos sellos firmó.
pub fn tras_cambio_de_claves(db: &Connection) -> Result<(usize, usize)> {
    recordar(db)?;
    let l = llaves(db, false)?;
    let tx = db.unchecked_transaction()?;
    let sello_de = |id: &str| -> Result<Option<(serde_json::Value, Sello)>> {
        let extra: Option<String> = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
        let v = extra.and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok()).and_then(|mut e| e.get_mut(CLAVE).map(serde_json::Value::take));
        Ok(v.and_then(|v| serde_json::from_value::<Sello>(v.clone()).ok().map(|s| (v, s))))
    };
    // Las marcas de otra persona que ya no tocan: su firma vale con las claves de ahora.
    let marcas: Vec<(String, String)> = {
        let mut q = tx.prepare("SELECT k, v FROM meta WHERE k LIKE 'sello_ajeno:%'")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut quitadas = 0;
    let mut ajenos = std::collections::HashSet::new();
    for (k, marca) in marcas {
        let id = k[SELLO_AJENO.len()..].to_string();
        let Some((v, s)) = sello_de(&id)? else { continue };
        if marca != format!("{}\n{}", s.by, s.at) {
            continue;
        }
        // Como `sync::sello_de_otro`: uno anterior a cuando la persona se lo quitó aquí no vuelve.
        let desverificada_despues = desverificada_en(&tx, &id)?.is_some_and(|d| d > s.at);
        if !desverificada_despues && sello_propio(&l, &id, &v) {
            tx.execute("DELETE FROM meta WHERE k = ?1", [&k])?;
            quitadas += 1;
        } else {
            ajenos.insert(id);
        }
    }
    // Los suyos, firmados de nuevo con la clave vigente del general. Sin general, nada: la propia no la
    // tiene ningún otro equipo, y los firmados con una apuntada siguen valiendo donde ya valían.
    let mut firmados = 0;
    if let (true, Some(k)) = (l.de_general, l.firmar.as_ref()) {
        let filas: Vec<(String, String, String, String)> = {
            let mut q = tx.prepare("SELECT id, kind, title, content FROM memory WHERE instr(extra, '\"verified\"') > 0")?;
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let vigente = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, k);
        for (id, kind, title, content) in filas {
            if ajenos.contains(&id) {
                continue;
            }
            let Some((v, mut s)) = sello_de(&id)? else { continue };
            // Solo uno que siga valiendo para su texto, de esta persona y aún sin la clave vigente.
            if s.sha != huella(&kind, &title, &content) || !sello_propio(&l, &id, &v) {
                continue;
            }
            if de_hex(&s.mac).is_some_and(|f| ring::hmac::verify(&vigente, texto_mac(&id, &s).as_bytes(), &f).is_ok()) {
                continue;
            }
            s.mac = a_hex(&hmac(k, texto_mac(&id, &s).as_bytes()));
            poner(&tx, &id, Some(serde_json::to_value(&s)?))?;
            firmados += 1;
        }
    }
    tx.commit()?;
    Ok((quitadas, firmados))
}

fn a_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn de_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Lo que firma la clave de sellos: cada campo con su largo delante.
fn texto_mac(id: &str, s: &Sello) -> String {
    let mut t = String::from("omoikane-seal-1");
    for c in [id, s.by.as_str(), s.at.as_str(), s.sha.as_str()] {
        t += &format!("\n{}:{c}", c.len());
    }
    t
}

/// Si el sello (el valor de [`CLAVE`] en `extra`) de la memoria `id` lo puso esta persona: su firma vale
/// con alguna de sus claves de sellos.
pub(crate) fn sello_propio(l: &Llaves, id: &str, v: &serde_json::Value) -> bool {
    let Ok(s) = serde_json::from_value::<Sello>(v.clone()) else { return false };
    let Some(f) = de_hex(&s.mac) else { return false };
    let texto = texto_mac(id, &s);
    l.validas.iter().any(|k| ring::hmac::verify(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, k), texto.as_bytes(), &f).is_ok())
}

/// Firma de nuevo un sello con la clave de esta persona (al hacerlo suyo: `omoi import <fichero>
/// --trust-verified`). `false` si no es un sello o no hay con qué firmar.
pub(crate) fn resellar(l: &Llaves, id: &str, v: &mut serde_json::Value) -> bool {
    let (Ok(mut s), Some(k)) = (serde_json::from_value::<Sello>(v.clone()), l.firmar.as_ref()) else { return false };
    s.mac = a_hex(&hmac(k, texto_mac(id, &s).as_bytes()));
    match serde_json::to_value(&s) {
        Ok(x) => {
            *v = x;
            true
        }
        Err(_) => false,
    }
}

/// La huella del texto que se verifica: tipo, título y contenido (32 cifras hex).
pub(crate) fn huella(kind: &str, title: &str, content: &str) -> String {
    let mut h = Sha256::new();
    for parte in [kind, title, content] {
        h.update(parte.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// La verificación de su columna en `memoria::COLUMNAS` (el JSON del sello), si su huella es la del texto de ahora.
pub(crate) fn de_columna(json: Option<&str>, kind: &str, title: &str, content: &str) -> Option<Verificada> {
    let s: Sello = serde_json::from_str(json?).ok()?;
    (!s.by.trim().is_empty() && s.sha == huella(kind, title, content)).then_some(Verificada { by: s.by, at: s.at })
}

/// Quién verifica si no se dice (`--by`, o la vista web): el autor de este equipo para lo de esa
/// memoria (`OMOIKANE_AUTHOR` o el nombre con el que se unió a la sincronización), y si no hay, el
/// usuario del sistema.
pub fn quien_por_defecto(db: &Connection, m: &Memoria) -> Result<String> {
    quien_aqui(db, &m.space)
}

/// [`quien_por_defecto`] para lo de un espacio (su clave; vacía, sin espacio).
pub(crate) fn quien_aqui(db: &Connection, espacio: &str) -> Result<String> {
    let a = crate::memoria::autor_en(db, espacio)?;
    if !a.trim().is_empty() {
        return Ok(a);
    }
    Ok(["USERNAME", "USER"].iter().filter_map(|v| std::env::var(v).ok()).map(|v| v.trim().to_string()).find(|v| !v.is_empty()).unwrap_or_default())
}

/// Las variables que pone la shell de un agente de programación: dentro de ella, `omoi verify` se
/// niega (la verificación es de la persona). Es un freno para que un agente no la use sin querer o
/// porque una memoria se lo pida, no una barrera contra quien la quiera saltar.
///
/// Solo las que pone el agente al lanzar sus órdenes (comprobado en su código o su documentación, o en
/// la shell de Claude Code de verdad; la tabla de `detect-coding-agent` las recoge con su fuente), nunca
/// las de configuración que la persona pone en su propia terminal: ni `CLAUDE_CODE_*` en general
/// (`CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS`… van en el perfil de la persona), ni
/// `AIDER_*` (Aider no pone ninguna al ejecutar: todas son su configuración), ni `CURSOR_TRACE_ID` o
/// `TERM_PROGRAM` (también están en la terminal de la persona dentro del editor). Con una de esas,
/// `omoi verify` se negaría también a la persona.
pub const DE_AGENTE: &[&str] = &[
    // Genéricas (agents.md): las ponen Claude Code, Copilot en VS Code, Devin, OpenCode, Crush, Amp,
    // Goose…
    "AI_AGENT",
    "AGENT",
    // Claude Code (y Claude Cowork).
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_IS_COWORK",
    // Gemini CLI, Qwen Code, Antigravity.
    "GEMINI_CLI",
    "QWEN_CODE",
    "ANTIGRAVITY_AGENT",
    // Codex.
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_CI",
    // Cursor (agente del editor y CLI).
    "CURSOR_AGENT",
    // OpenCode y Kilo Code; Crush; Amp; Goose.
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    "KILO",
    "CRUSH",
    "AMP_CURRENT_THREAD_ID",
    "GOOSE_TERMINAL",
    // Cline y Roo Code.
    "CLINE_ACTIVE",
    "ROO_ACTIVE",
    // GitHub Copilot: en VS Code, su CLI y su agente en la nube.
    "COPILOT_AGENT",
    "COPILOT_CLI",
    "COPILOT_AGENT_SESSION_ID",
    "COPILOT_AGENT_JOB_ID",
    // Windsurf (Cascade), Augment, Pi, Junie, Kiro, Trae.
    "WINDSURF_CASCADE_TERMINAL",
    "AUGMENT_AGENT",
    "PI_CODING_AGENT",
    "JUNIE_DATA",
    "KIRO_AGENT_PATH",
    "TRAE_AI_SHELL_ID",
];

/// La variable de agente que hay en el entorno, si hay alguna.
pub fn shell_de_agente() -> Option<&'static str> {
    DE_AGENTE.iter().copied().find(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()))
}

/// Marca la memoria `id` (entero) como verificada por `by`. No crea versión ni cambia su fecha (como
/// fijarla); cambia su sello para que la sincronización lo lleve. Devuelve la memoria.
pub fn verificar(db: &mut Connection, id: &str, by: &str) -> Result<Memoria> {
    Ok(verificar_en(db, id, by, None)?.0)
}

/// Como [`verificar`], con la fecha en que se verificó si se sabe (un instante ISO 8601, el `at` de un
/// `verified` de OKF al importarlo; si no se entiende, ahora). Si ya tiene ese mismo sello y vale, no
/// lo vuelve a poner (no cambia su sello de tiempo): devuelve la memoria y si lo ha puesto.
pub fn verificar_en(db: &mut Connection, id: &str, by: &str, at: Option<&str>) -> Result<(Memoria, bool)> {
    let by = by.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!by.is_empty(), tr!("who verifies it? Give --by <name>", "¿quién la verifica? Di --by <nombre>"));
    ensure!(by.chars().count() <= 100, tr!("--by: up to 100 characters", "--by: hasta 100 caracteres"));
    let tx = crate::memoria::escribir_tx(db)?;
    let (kind, title, content): (String, String, String) =
        tx.query_row("SELECT kind, title, content FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?;
    // La fecha dada, en la forma de las de Omoikane (`…T…:…:….mmmZ`, en UTC); si no se entiende, ahora.
    let dada: Option<String> = match at.map(|a| a.trim().trim_end_matches('Z').replace('T', " ")).filter(|a| !a.is_empty()) {
        Some(a) => tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1)", [a], |f| f.get(0))?,
        None => None,
    };
    // Una fecha de más de un día en el futuro no es de cuando se verificó (un reloj mal puesto, o un
    // bundle que quiere quedar por delante de cualquier cambio): no se acepta.
    if let Some(a) = &dada {
        let futura: bool = tx.query_row("SELECT ?1 > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day')", [a], |f| f.get(0))?;
        ensure!(!futura, tr!("the verification date {a} is in the future", "la fecha de verificación {a} es del futuro"));
    }
    let at = match dada {
        Some(a) => a,
        None => tx.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?,
    };
    let antes = crate::memoria::leer(&tx, id)?;
    if antes.verified.as_ref().is_some_and(|v| v.by == by && v.at == at) {
        return Ok((antes, false));
    }
    let mut sello = Sello { by, at, sha: huella(&kind, &title, &content), mac: String::new() };
    // Firmado con la clave de sellos de esta persona (ver `Llaves`): así cuenta como suyo en sus otros
    // equipos aunque llegue por el repositorio de un espacio.
    if let Some(k) = llaves(&tx, true)?.firmar {
        sello.mac = a_hex(&hmac(&k, texto_mac(id, &sello).as_bytes()));
    }
    poner(&tx, id, Some(serde_json::to_value(sello)?))?;
    // Si lo había marcado como de otra persona (`SELLO_AJENO`), ya no: ahora es suyo.
    tx.execute("DELETE FROM meta WHERE k = ?1", [format!("{SELLO_AJENO}{id}")])?;
    let m = crate::memoria::leer(&tx, id)?;
    tx.commit()?;
    Ok((m, true))
}

/// Le quita la verificación y apunta cuándo ([`DESVERIFICADA`]): reimportar un bundle exportado antes
/// no la vuelve a verificar. `false` si no tenía.
pub fn desverificar(db: &mut Connection, id: &str) -> Result<bool> {
    let tx = crate::memoria::escribir_tx(db)?;
    let hay = poner(&tx, id, None)?;
    if hay {
        let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
        let ahora: String = tx.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?;
        o.insert(DESVERIFICADA.into(), ahora.into());
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    }
    tx.commit()?;
    Ok(hay)
}

/// Cuándo le quitó la persona la verificación por última vez ([`desverificar`]), si se sabe.
pub(crate) fn desverificada_en(db: &Connection, id: &str) -> Result<Option<String>> {
    let extra: Option<String> = db.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    Ok(extra
        .filter(|e| e.contains(DESVERIFICADA))
        .and_then(|e| serde_json::from_str::<serde_json::Value>(&e).ok())
        .and_then(|v| v.get(DESVERIFICADA).and_then(|x| x.as_str()).map(str::to_string)))
}

/// Pone o quita el sello con un sello de tiempo nuevo. `false` si no había nada que quitar.
fn poner(tx: &Connection, id: &str, sello: Option<serde_json::Value>) -> Result<bool> {
    let extra: String = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    let habia = match sello {
        Some(s) => {
            o.insert(CLAVE.into(), s);
            true
        }
        None => o.remove(CLAVE).is_some(),
    };
    if !habia {
        return Ok(false);
    }
    let hlc = crate::reloj::sello_tras(tx, &crate::memoria::hlc_de(tx, id)?)?;
    tx.execute("UPDATE memory SET extra = ?2, hlc = ?3 WHERE id = ?1", params![id, serde_json::to_string(&o)?, hlc])?;
    Ok(true)
}

/// Al cambiar el texto desde aquí (`memoria::actualizar_en`, que ya pone el sello de tiempo): quita la
/// verificación, apunta cuándo ([`DESVERIFICADA`], como [`desverificar`]) y devuelve la que había, si
/// valía. Sin la fecha, el sello viejo volvería con una línea más nueva que trajera el texto de antes
/// (otro equipo de la persona que aún no vio el cambio, `import <fichero>`, un bundle OKF): con el texto
/// de antes, su huella vuelve a coincidir.
pub(crate) fn quitar_al_cambiar(tx: &Transaction, id: &str, antes: &Memoria) -> Result<Option<Verificada>> {
    let extra: Option<String> = tx.query_row("SELECT extra FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    let Some(extra) = extra.filter(|e| e.contains("\"verified\"")) else { return Ok(None) };
    let mut o: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&extra).unwrap_or_default();
    if o.remove(CLAVE).is_none() {
        return Ok(None);
    }
    let ahora: String = tx.query_row(&format!("SELECT {AHORA}"), [], |f| f.get(0))?;
    o.insert(DESVERIFICADA.into(), ahora.into());
    tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, serde_json::to_string(&o)?])?;
    Ok(antes.verified.clone())
}

/// Lo que llega en una línea más nueva (sincronización, `import <fichero>`) para una memoria a la que
/// aquí se le quitó la verificación en `desverificada` ([`DESVERIFICADA`]): la fecha se conserva (la más
/// reciente de las dos, para la próxima vez) y, si `quitar_sello`, un sello de antes de esa fecha se
/// quita. (Por el repositorio de un espacio el sello se queda en la línea para los demás y aquí no
/// cuenta: ver `sync::sello_de_otro`.)
pub(crate) fn sin_sello_viejo(extra: &mut serde_json::Map<String, serde_json::Value>, desverificada: &str, quitar_sello: bool) {
    if extra.get(DESVERIFICADA).and_then(|x| x.as_str()).is_none_or(|d| d < desverificada) {
        extra.insert(DESVERIFICADA.into(), desverificada.into());
    }
    let viejo = extra.get(CLAVE).and_then(|v| v.get("at")).and_then(|x| x.as_str()).is_some_and(|at| at < desverificada);
    if viejo && quitar_sello {
        extra.remove(CLAVE);
    }
}

/// Lo que se enseña en una línea: «verified by Marc, 2026-11-08».
pub fn texto(v: &Verificada) -> String {
    format!("verified by {}, {}", v.by, v.at.get(..10).unwrap_or(&v.at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_huella_cambia_con_el_texto() {
        let a = huella("decision", "T", "C");
        assert_eq!(a.len(), 32);
        assert_ne!(a, huella("fact", "T", "C"));
        assert_ne!(a, huella("decision", "T", "C."));
        // El separador: «ab»+«c» no es «a»+«bc».
        assert_ne!(huella("x", "ab", "c"), huella("x", "a", "bc"));
        let s = serde_json::json!({ "by": "Marc", "at": "2026-11-08T10:00:00.000Z", "sha": a }).to_string();
        assert_eq!(de_columna(Some(&s), "decision", "T", "C").unwrap().by, "Marc");
        assert!(de_columna(Some(&s), "decision", "T", "otro").is_none());
        assert!(de_columna(Some(r#"{"by":"Marc","at":"x"}"#), "decision", "T", "C").is_none());
        assert!(de_columna(None, "decision", "T", "C").is_none());
    }
}
