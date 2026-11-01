//! El servicio que mantiene abierto EmbeddingGemma (`gemma`) para quien necesite un vector.
//!
//! Cargar el modelo cuesta unos 2 s y el recall arranca un proceso en cada mensaje, así que el modelo
//! vive en un proceso aparte, uno por usuario: `omoi model serve-semantic` (oculto). Escucha solo en
//! 127.0.0.1, en un puerto libre, y exige un token; los dos, con su pid, van en
//! `~/.omoikane/significado.json` cuando el modelo ya está cargado. Se cierra solo tras un rato sin
//! peticiones (`OMOIKANE_SEMANTIC_IDLE` segundos, 30 min por defecto).
//!
//! Quien pide (`vectores`) tiene un presupuesto de tiempo: si no hay servicio, o tarda más, sigue sin
//! significado, como hasta ahora, y deja el servicio arrancando para la próxima vez. Nunca espera a
//! que cargue.
//!
//! Protocolo: una línea JSON por petición y otra por respuesta.
//! `{"token":…,"tipo":"pregunta"|"documento","textos":[…]}` → `{"vectores":[[…]…]}` o `{"error":…}`;
//! `{"token":…,"ping":true}` → `{"pid":…,"modelo":…}`.

use crate::gemma::{Gemma, Tipo};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REGISTRO: &str = "significado.json";
const ARRANCANDO: &str = "significado.arrancando";
/// Carpeta del modelo dentro de `~/.omoikane/models`.
pub const NOMBRE_MODELO: &str = "embeddinggemma-300m";
const OCIOSO_POR_DEFECTO: u64 = 30 * 60;
/// Tras lanzar el servicio, no se vuelve a lanzar hasta pasado esto (lo que tarda en cargar, de sobra).
const REINTENTO_ARRANQUE: u64 = 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registro {
    pub puerto: u16,
    pub token: String,
    pub pid: u32,
    pub modelo: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Peticion {
    token: String,
    #[serde(default)]
    tipo: Option<String>,
    #[serde(default)]
    textos: Vec<String>,
    #[serde(default)]
    ping: bool,
    /// La base de quien pide: el servicio calcula en segundo plano los vectores que le falten.
    #[serde(default)]
    db: Option<String>,
    /// Buscar en el servicio (`parecidas`) en vez de pedir vectores.
    #[serde(default)]
    buscar: Option<Busqueda>,
}

fn carpeta() -> Option<PathBuf> {
    std::env::var_os("OMOIKANE_HOME").filter(|r| !r.is_empty()).map(PathBuf::from).or_else(|| std::env::home_dir().map(|h| h.join(".omoikane")))
}

const PREFERENCIA: &str = "semantic";

/// Lo que eligió el usuario con `omoi model semantic --on/--off` (`None`: no eligió nada).
pub fn preferencia() -> Option<bool> {
    match std::fs::read_to_string(carpeta()?.join(PREFERENCIA)).ok()?.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Guarda la elección: `Some(true)` (`--on`), `Some(false)` (`--off`) o `None` (`--auto`: que mande el
/// tamaño de la memoria). Apagarlo cierra también el servicio si está abierto: el vigía ve que el
/// registro ya no es suyo y sale. Con él apagado todo va como antes de la 1.2: solo palabras y potion.
pub fn fijar(eleccion: Option<bool>) -> Result<()> {
    let c = carpeta().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    match eleccion {
        Some(on) => std::fs::write(c.join(PREFERENCIA), if on { "on\n" } else { "off\n" })?,
        None => {
            let _ = std::fs::remove_file(c.join(PREFERENCIA));
        }
    }
    if eleccion == Some(false) {
        let _ = std::fs::remove_file(c.join(REGISTRO));
    }
    Ok(())
}

/// Desde cuántas memorias se enciende solo el modelo de significado, si nadie eligió nada.
///
/// Con pocas memorias no compensa: el veto del recall casi no quita ruido por debajo de unas 1.000
/// (`omoi-revision-12/2-cuando.md`: con 150-1.000 el ruido es el mismo con y sin modelo; con 3.000
/// baja del 40 % al 7,5 %) y cuesta ~450 MB de disco, un servicio residente e indexar cada memoria.
/// Provisional (1.500): lo calibra la medición final entre 1.000 y 3.000. `OMOIKANE_SEMANTIC_THRESHOLD`
/// lo cambia (pruebas, calibración).
///
/// Cuentan todas las memorias de la base, no las del ámbito de la sesión. El modelo, su descarga, el
/// servicio y los vectores son de la base entera (no de un proyecto), y la decisión tiene que ser la
/// misma desde cualquier carpeta: si dependiera del ámbito, se encendería y apagaría al cambiar de
/// proyecto, y la base se indexaría a medias. Además, desde una carpeta general el recall y la búsqueda
/// ven todas, y en una base grande el ruido de palabras sube también dentro de cada proyecto (lo
/// arrastran las memorias globales). Cuesta una lectura de `memory_cuenta` (microsegundos).
pub const UMBRAL_MEMORIAS: usize = 1500;

pub fn umbral() -> usize {
    std::env::var("OMOIKANE_SEMANTIC_THRESHOLD").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(UMBRAL_MEMORIAS)
}

/// Por qué el modelo está encendido o apagado.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Porque {
    /// `OMOIKANE_SEMANTIC=0`/`1`.
    Env,
    /// `omoi model semantic --on/--off`.
    Chosen,
    /// Nadie eligió nada: manda el tamaño de la memoria (`UMBRAL_MEMORIAS`).
    Size,
}

/// En qué estado está el modelo de significado para una base, y por qué.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Estado {
    pub on: bool,
    pub reason: Porque,
    pub memories: usize,
    pub threshold: usize,
}

/// ¿Lo ha prohibido alguien? `OMOIKANE_SEMANTIC=0` o `--off`. No dice si está encendido (eso depende
/// también del tamaño, ver `estado`): el servicio y quien lo arranca solo miran esto.
pub fn permitido() -> bool {
    match std::env::var("OMOIKANE_SEMANTIC") {
        Ok(v) if !v.is_empty() => v != "0",
        _ => preferencia() != Some(false),
    }
}

/// `OMOIKANE_SEMANTIC` manda; si no, lo que eligió el usuario; y si no eligió nada, el tamaño: desde
/// `umbral()` memorias en la base, encendido.
pub fn estado(db: &rusqlite::Connection) -> Estado {
    let memories = crate::memoria::cuantas(db, &crate::memoria::Ambito::Todos).unwrap_or(0).max(0) as usize;
    let threshold = umbral();
    let (on, reason) = match std::env::var("OMOIKANE_SEMANTIC") {
        Ok(v) if !v.is_empty() => (v != "0", Porque::Env),
        _ => match preferencia() {
            Some(p) => (p, Porque::Chosen),
            None => (memories >= threshold, Porque::Size),
        },
    };
    Estado { on, reason, memories, threshold }
}

/// ¿Se usa el modelo con esta base? Encendido (`estado`) e instalado.
pub fn en_uso(db: &rusqlite::Connection) -> bool {
    permitido() && modelo().is_some() && estado(db).on
}

/// `en_uso` con la ruta de la base (abre una conexión solo si el modelo está instalado y permitido).
pub fn en_uso_en(ruta: &std::path::Path) -> bool {
    if !permitido() || modelo().is_none() || !ruta.exists() {
        return false;
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    rusqlite::Connection::open_with_flags(ruta, flags).is_ok_and(|c| estado(&c).on)
}

const DESCARGANDO: &str = "significado.descargando";
const AVISADO: &str = "significado.avisado";
/// Tras lanzar una descarga, no se lanza otra hasta pasado esto (si falló, se reintenta entonces).
const REINTENTO_DESCARGA: u64 = 3600;

/// ¿Hay una descarga del modelo en marcha (lanzada hace menos de `REINTENTO_DESCARGA`)?
pub fn descargando() -> bool {
    carpeta()
        .and_then(|c| std::fs::read_to_string(c.join(DESCARGANDO)).ok())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .is_some_and(|t| ahora().saturating_sub(t) < REINTENTO_DESCARGA)
}

/// Lanza aparte `omoi model install --semantic` para `db` (que, al acabar, deja el servicio indexando
/// esa base), salvo si ya hay una descarga en marcha. No espera.
pub fn descargar_aparte(db: &std::path::Path) {
    if !permitido() || modelo().is_some() || descargando() {
        return;
    }
    let Some(c) = carpeta() else { return };
    let _ = std::fs::create_dir_all(&c);
    if std::fs::write(c.join(DESCARGANDO), ahora().to_string()).is_ok() {
        let db = db.to_string_lossy();
        crate::sync::lanzar_aparte(&["--db", &db, "model", "install", "--semantic"]);
    }
}

/// La descarga acabó (bien): quita la marca.
pub fn descarga_hecha() {
    if let Some(c) = carpeta() {
        let _ = std::fs::remove_file(c.join(DESCARGANDO));
    }
}

/// Al empezar sesión: si el modelo está encendido para esta base y falta, lo descarga en segundo
/// plano; si está y la base aún no tiene sus vectores, deja el servicio indexándola. Devuelve el aviso
/// para el agente la primera vez que se enciende solo por el tamaño (una vez por usuario).
pub fn al_empezar(db: &rusqlite::Connection) -> Option<String> {
    if !permitido() {
        return None;
    }
    let e = estado(db);
    let ruta = db.path().filter(|p| !p.is_empty()).map(PathBuf::from)?;
    if !e.on {
        return None;
    }
    if modelo().is_none() {
        descargar_aparte(&ruta);
        let c = carpeta()?;
        if e.reason == Porque::Size && !c.join(AVISADO).exists() && std::fs::write(c.join(AVISADO), ahora().to_string()).is_ok() {
            return Some(format!(
                "Omoikane now holds {} memories (from {} on, words alone bring more noise): it is downloading its meaning model (EmbeddingGemma, about 450 MB) in the background and will then index the memories, so searches find what is asked with other words and recall injects less noise. Until it finishes everything works as before. `omoi model semantic --off` stops it for good. Tell the user once.",
                e.memories, e.threshold
            ));
        }
        return None;
    }
    if !listo(db) {
        avisar(&ruta);
    }
    None
}

/// La carpeta de EmbeddingGemma si está instalado: `OMOIKANE_SEMANTIC_MODEL`, o
/// `~/.omoikane/models/embeddinggemma-300m`.
pub fn modelo() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOIKANE_SEMANTIC_MODEL")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::modelos::carpeta().join(NOMBRE_MODELO));
    crate::gemma::es_modelo(&dir).then_some(dir)
}

/// De dónde se descarga el formato compacto (`gemma::compactar`), en orden. `OMOIKANE_SEMANTIC_SOURCE`
/// las sustituye (una o varias separadas por comas).
pub const ORIGENES: [&str; 2] = [
    "https://huggingface.co/natzx94/embeddinggemma-300m-omoikane/resolve/main",
    "https://github.com/marcmayol/omoikane/releases/download/modelo-embeddinggemma-300m-v1",
];

/// SHA-256 de cada fichero del formato compacto publicado: lo que se descarga tiene que ser
/// exactamente esto, venga del espejo que venga.
const SUMAS: [(&str, &str); 5] = [
    ("config.json", "8f863f76e2d9c710cc833dc92efa898c9adfd41031c786507cc6b0e49c2e3e68"),
    ("tokenizer.json", "6852f8d561078cc0cebe70ca03c5bfdd0d60a45f9d2e0e1e4cc05b68e9ec329e"),
    ("tokens.escala", "ffba8e813e1ce38f96a360df3f70f7c2dd14b30c462c5a869da5def52abd5e1a"),
    ("pesos.safetensors", "29c6f8b54070cc8889f1f0f335dbf9d2e5a6957f2c01830ab9e5284f832b7aec"),
    ("tokens.i8", "02c0fd1158bf4195f15f5db815f67f1de120e0852d010a0f2a74917167fc2465"),
];

/// El aviso que pide la licencia de Gemma a quien lo redistribuye; va en la carpeta del modelo.
const AVISO_LICENCIA: &str = "EmbeddingGemma 300M (google/embeddinggemma-300m), converted by Omoikane to a compact format\n\
(token table in int8, the rest in float16).\n\n\
Gemma is provided under and subject to the Gemma Terms of Use found at ai.google.dev/gemma/terms\n\
Its use is restricted by the Gemma Prohibited Use Policy: ai.google.dev/gemma/prohibited_use_policy\n";

fn suma(f: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut h = Sha256::new();
    let mut r = std::fs::File::open(f)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Instala EmbeddingGemma en `~/.omoikane/models/embeddinggemma-300m`. Sin `desde`, descarga el
/// formato compacto (unos 450 MB) y comprueba su SHA-256; con `desde`, lo toma de una carpeta, en
/// formato compacto o de sentence-transformers (que convierte). Si ya está instalado, no hace nada.
/// Devuelve la carpeta.
pub fn instalar(desde: Option<&std::path::Path>, aviso: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    use crate::gemma;
    use crate::tr;
    let base = crate::modelos::carpeta();
    let destino = base.join(NOMBRE_MODELO);
    if gemma::es_modelo(&destino) {
        return Ok(destino);
    }
    std::fs::create_dir_all(&base)?;
    let temporal = base.join(format!(".instalando-significado-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    let hecho = (|| -> Result<()> {
        match desde {
            Some(d) if gemma::es_compacto(d) => {
                std::fs::create_dir_all(&temporal)?;
                for f in gemma::FICHEROS_COMPACTO {
                    std::fs::copy(d.join(f), temporal.join(f))?;
                }
            }
            Some(d) if gemma::es_sentence_transformers(d) => {
                aviso(&tr!("converting the model to the compact format (a few seconds)", "convirtiendo el modelo al formato compacto (unos segundos)"));
                gemma::compactar(d, &temporal)?;
            }
            Some(d) => bail!(tr!("{}: no EmbeddingGemma there", "{}: ahí no hay EmbeddingGemma", d.display())),
            None => {
                let origenes: Vec<String> = match std::env::var("OMOIKANE_SEMANTIC_SOURCE") {
                    Ok(o) if !o.trim().is_empty() => o.split(',').map(|x| x.trim().trim_end_matches('/').to_string()).filter(|x| !x.is_empty()).collect(),
                    _ => ORIGENES.iter().map(|o| o.to_string()).collect(),
                };
                let mut fallos = Vec::new();
                for o in origenes {
                    let r = crate::modelos::descargar_ficheros(&o, &gemma::FICHEROS_COMPACTO, &temporal, aviso).and_then(|()| {
                        for (f, esperada) in SUMAS {
                            let s = suma(&temporal.join(f))?;
                            if s != esperada {
                                bail!(tr!("{f}: wrong SHA-256 ({s})", "{f}: SHA-256 equivocado ({s})"));
                            }
                        }
                        Ok(())
                    });
                    match r {
                        Ok(()) => {
                            fallos.clear();
                            break;
                        }
                        Err(e) => {
                            aviso(&tr!("failed: {e:#}", "falló: {e:#}"));
                            fallos.push(format!("{o}: {e:#}"));
                        }
                    }
                }
                if !fallos.is_empty() {
                    bail!(tr!("couldn't download the meaning model:\n{}", "no se pudo descargar el modelo de significado:\n{}", fallos.join("\n")));
                }
            }
        }
        std::fs::write(temporal.join("NOTICE"), AVISO_LICENCIA)?;
        // El tokenizer compilado (`bpe`), aquí y no en la descarga: los ficheros publicados y sus
        // SHA-256 no cambian. Si fallase, se compila la primera vez que se cargue el modelo.
        let _ = crate::bpe::compilar(&temporal.join("tokenizer.json"), &temporal.join(crate::bpe::FICHERO));
        // Si otro proceso lo instaló a la vez, vale el suyo.
        if std::fs::rename(&temporal, &destino).is_err() && !gemma::es_modelo(&destino) {
            bail!(tr!("can't move the model into {}", "no se puede mover el modelo a {}", destino.display()));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&temporal);
    hecho.map(|()| destino)
}

// ------------------------------------------------------------------ los vectores en la base

/// Los vectores de EmbeddingGemma de cada memoria: una tabla local, como `vec_bloque` (no se
/// sincroniza, una 1.0 la ignora y no sube `lector_minimo`). Por `id` y no por `n`: una migración que
/// rehaga `memory` puede renumerar. `hlc` es el `content_hlc` de la memoria cuando se calculó: si
/// el texto cambia, deja de coincidir y se vuelve a calcular. `v`: escala (f32) y 768 int8.
const TABLA: &str = "CREATE TABLE IF NOT EXISTS memory_sig (id TEXT PRIMARY KEY, hlc TEXT NOT NULL, v BLOB NOT NULL)";

/// Qué modelo y qué cálculo hicieron los vectores guardados: si cambia, se borran y se recalculan.
const VERSION_VECTORES: &str = "embeddinggemma-300m/2048";

pub fn crear_tabla(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(TABLA)?;
    let hecha: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'sig_version'", [], |f| f.get(0)).ok();
    if hecha.as_deref() != Some(VERSION_VECTORES) {
        db.execute("DELETE FROM memory_sig", [])?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sig_version', ?1)", [VERSION_VECTORES])?;
    }
    Ok(())
}

fn a_blob(v: &[f32]) -> Vec<u8> {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let e = if max > 0.0 { max / 127.0 } else { 1.0 };
    let mut b = Vec::with_capacity(4 + v.len());
    b.extend_from_slice(&e.to_le_bytes());
    b.extend(v.iter().map(|x| (x / e).round().clamp(-127.0, 127.0) as i8 as u8));
    b
}

/// Coseno entre la consulta (normalizada) y un vector guardado (normalizado al guardarlo).
fn coseno(q: &[f32], b: &[u8]) -> f32 {
    if b.len() != 4 + q.len() {
        return 0.0;
    }
    let e = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    // De 8 en 8 con sumas parciales: así el compilador lo vectoriza (con una sola suma no puede
    // reordenarla). Con 10.000 memorias, comparar pasaba de 8,2 a 2,0 ms (revisión de velocidad).
    let mut s = [0f32; 8];
    let ((cq, rq), (cb, rb)) = (q.as_chunks::<8>(), b[4..].as_chunks::<8>());
    for (x, y) in cq.iter().zip(cb) {
        for i in 0..8 {
            s[i] += x[i] * f32::from(y[i] as i8);
        }
    }
    let resto: f32 = rq.iter().zip(rb).map(|(x, &y)| x * f32::from(y as i8)).sum();
    e * (s.iter().sum::<f32>() + resto)
}

/// El texto que se convierte en vector: el título y el contenido, como en el benchmark que lo midió.
pub fn texto_de(titulo: &str, contenido: &str) -> String {
    format!("{titulo}\n{contenido}")
}

/// Hasta `max` memorias sin vector (o con el texto cambiado desde que se calculó): `(id, hlc, texto)`.
pub fn pendientes(db: &rusqlite::Connection, max: usize) -> Result<Vec<(String, String, String)>> {
    let mut q = db.prepare(
        "SELECT m.id, m.content_hlc, m.title, m.content FROM memory m LEFT JOIN memory_sig s ON s.id = m.id
         WHERE s.id IS NULL OR s.hlc <> m.content_hlc LIMIT ?1",
    )?;
    let filas = q.query_map([max as i64], |f| Ok((f.get(0)?, f.get(1)?, texto_de(&f.get::<_, String>(2)?, &f.get::<_, String>(3)?))))?;
    Ok(filas.collect::<rusqlite::Result<_>>()?)
}

pub fn guardar(db: &rusqlite::Connection, id: &str, hlc: &str, v: &[f32]) -> Result<()> {
    db.execute("INSERT OR REPLACE INTO memory_sig(id, hlc, v) VALUES (?1, ?2, ?3)", rusqlite::params![id, hlc, a_blob(v)])?;
    Ok(())
}

/// Cuántas memorias tienen su vector al día, y cuántas hay. Borra de paso los de memorias que ya no
/// están (olvidadas o purgadas).
pub fn cobertura(db: &rusqlite::Connection) -> Result<(usize, usize)> {
    let _ = db.execute("DELETE FROM memory_sig WHERE id NOT IN (SELECT id FROM memory)", []);
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0))?;
    let hechas: i64 = db.query_row("SELECT count(*) FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc", [], |f| f.get(0))?;
    Ok((hechas as usize, total as usize))
}

/// ¿Hay vectores para casi todo? Mientras se indexa (los primeros minutos tras instalar el modelo),
/// mezclar significado con memorias sin vector las hundiría: se sigue como antes.
pub fn listo(db: &rusqlite::Connection) -> bool {
    let total: i64 = db.query_row("SELECT count(*) FROM memory", [], |f| f.get(0)).unwrap_or(0);
    if total == 0 {
        return false;
    }
    let hechas: i64 = db.query_row("SELECT count(*) FROM memory_sig", [], |f| f.get(0)).unwrap_or(0);
    hechas as f64 >= 0.95 * total as f64
}

/// El coseno de `consulta` con cada memoria del ámbito que tiene vector, de mayor a menor.
pub fn cosenos(db: &rusqlite::Connection, consulta: &[f32], ambito: &crate::memoria::Ambito, kind: Option<crate::memoria::Tipo>) -> Result<Vec<(String, f32)>> {
    let mut q = db.prepare(&format!(
        "SELECT s.id, s.v FROM memory_sig s JOIN memory m ON m.id = s.id WHERE {} AND (?1 IS NULL OR m.kind = ?1)",
        ambito.sql()
    ))?;
    let mut filas = q.query(rusqlite::params![kind.map(crate::memoria::Tipo::as_str)])?;
    let mut v = Vec::new();
    while let Some(f) = filas.next()? {
        let c = coseno(consulta, f.get_ref(1)?.as_blob()?);
        v.push((f.get::<_, String>(0)?, c));
    }
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(v)
}

// ------------------------------------------------------------------ tope de RAM al indexar

/// Lo que ocupa en RAM cada token de un lote mientras se calcula (los vectores intermedios de las
/// capas; medido: 35-41 KB, ver `fix-motor.md` de la revisión). Los pesos no cuentan: van mapeados del
/// disco y son los mismos indexe o no.
const KB_POR_TOKEN: u64 = 40;
/// Tope por defecto de la RAM del indexado: lotes de unos 3.300 tokens (25-30 notas cortas, o 2-5
/// memorias largas). Con más ya no va más rápido (medido con lotes de 1.024 a 4.096 tokens).
pub const RAM_POR_DEFECTO_MB: u64 = 128;
/// El mínimo: lo que necesita la memoria más larga (2.048 tokens) para calcularse sola. Con menos no
/// se podría indexar alguna.
pub const RAM_MINIMA_MB: u64 = (crate::gemma::MAX_TOKENS as u64 * KB_POR_TOKEN).div_ceil(1024);
const FICHERO_RAM: &str = "semantic-ram";

/// De dónde sale el tope de RAM en uso.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum OrigenRam {
    /// `OMOIKANE_SEMANTIC_RAM`.
    #[serde(rename = "env")]
    Entorno,
    /// `omoi model semantic --ram`.
    #[serde(rename = "chosen")]
    Elegido,
    #[serde(rename = "default")]
    PorDefecto,
}

/// El tope de RAM del indexado en MB y de dónde sale: `OMOIKANE_SEMANTIC_RAM` manda; si no, lo que
/// se eligió con `omoi model semantic --ram`; si no, `RAM_POR_DEFECTO_MB`. Nunca por debajo de
/// `RAM_MINIMA_MB`.
pub fn tope_ram() -> (u64, OrigenRam) {
    let leer = |s: &str| s.trim().parse::<u64>().ok().filter(|&n| n > 0);
    let (mb, origen) = if let Some(n) = std::env::var("OMOIKANE_SEMANTIC_RAM").ok().as_deref().and_then(leer) {
        (n, OrigenRam::Entorno)
    } else if let Some(n) = carpeta().and_then(|c| std::fs::read_to_string(c.join(FICHERO_RAM)).ok()).as_deref().and_then(leer) {
        (n, OrigenRam::Elegido)
    } else {
        (RAM_POR_DEFECTO_MB, OrigenRam::PorDefecto)
    };
    (mb.max(RAM_MINIMA_MB), origen)
}

/// Guarda el tope (`None`: volver al de por defecto). Lo lee el indexado antes de cada tanda: no
/// hace falta reiniciar el servicio. Devuelve el que queda en uso (subido al mínimo si hacía falta).
pub fn fijar_ram(mb: Option<u64>) -> Result<u64> {
    let c = carpeta().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    match mb {
        Some(mb) => std::fs::write(c.join(FICHERO_RAM), format!("{}\n", mb.max(RAM_MINIMA_MB)))?,
        None => {
            let _ = std::fs::remove_file(c.join(FICHERO_RAM));
        }
    }
    Ok(tope_ram().0)
}

/// Tokens por lote al indexar con el tope de RAM en uso. Varias memorias en una sola pasada del
/// modelo: cada multiplicación se hace de una vez para todas (más rápido que de una en una, y el mismo
/// vector). El tope acota la RAM y lo que espera una consulta que llegue a mitad de un lote (como
/// mucho un paso del lote: ver `ceder`). Una memoria más larga que el lote va sola.
pub fn tokens_por_lote() -> usize {
    (tope_ram().0 * 1024 / KB_POR_TOKEN) as usize
}

/// Consultas que el servicio está atendiendo ahora mismo: mientras haya alguna, el indexado se para
/// entre paso y paso (`ceder`) para no quitarle la CPU.
static CONSULTAS: AtomicU64 = AtomicU64::new(0);

/// Lo que llama el modelo entre paso y paso mientras indexa: espera a que no haya consultas.
fn ceder() {
    while CONSULTAS.load(Ordering::Relaxed) > 0 {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Calcula los vectores que faltan en `db`, de `lote` en `lote` memorias leídas y por lotes de hasta
/// `tokens_por_lote` tokens, hasta acabar o hasta que `seguir` diga que no. `aviso` recibe cuántas
/// lleva. Devuelve cuántas calculó.
pub fn indexar(db: &rusqlite::Connection, gemma: &Gemma, lote: usize, seguir: &mut dyn FnMut() -> bool, aviso: &mut dyn FnMut(usize)) -> Result<usize> {
    let mut hechas = 0;
    loop {
        // Cada tanda vuelve a mirar el tope: `omoi model semantic --ram` vale sin reiniciar.
        let tope = tokens_por_lote();
        let p = pendientes(db, lote)?;
        if p.is_empty() {
            return Ok(hechas);
        }
        let ids = p.iter().map(|(_, _, t)| gemma.ids(t, Tipo::Documento)).collect::<Result<Vec<_>>>()?;
        let mut i = 0;
        while i < p.len() {
            if !seguir() {
                return Ok(hechas);
            }
            let (mut j, mut suma) = (i, 0);
            while j < p.len() && (j == i || suma + ids[j].len() <= tope) {
                suma += ids[j].len();
                j += 1;
            }
            for ((id, hlc, _), v) in p[i..j].iter().zip(gemma.vectores_de_ids(&ids[i..j], &ceder)?) {
                guardar(db, id, hlc, &v)?;
                hechas += 1;
                aviso(hechas);
            }
            i = j;
        }
    }
}

// ------------------------------------------------------------------ buscar dentro del servicio

/// Memorias con su coseno, de mayor a menor.
pub type Cosenos = Vec<(String, f32)>;

/// Lo que el servicio tiene en memoria de una base para comparar sin leerla en cada mensaje: cada
/// recall es un proceso nuevo, y leer los vectores de todas las memorias (77 MB con 100.000) le
/// costaba más que el propio modelo.
struct Indice {
    huella: (i64, i64, String, String, i64),
    filas: Vec<Fila>,
    /// El espacio de cada proyecto (para `Ambito::Espacio`).
    espacio_de: std::collections::HashMap<i64, Option<i64>>,
    memorias: i64,
}

struct Fila {
    id: String,
    project_id: i64,
    space_id: Option<i64>,
    kind: String,
    v: Vec<u8>,
}

/// Cambia si cambia algún vector, alguna memoria (texto, ámbito o tipo) o algún proyecto.
fn huella(db: &rusqlite::Connection) -> Result<(i64, i64, String, String, i64)> {
    Ok(db.query_row(
        "SELECT (SELECT count(*) FROM memory_sig), (SELECT COALESCE(max(rowid), 0) FROM memory_sig),
                (SELECT COALESCE(max(hlc), '') FROM memory), (SELECT COALESCE(max(hlc), '') FROM project),
                (SELECT count(*) FROM memory)",
        [],
        |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)),
    )?)
}

impl Indice {
    fn cargar(db: &rusqlite::Connection) -> Result<Indice> {
        let huella = huella(db)?;
        let mut q = db.prepare(
            "SELECT s.id, m.project_id, m.space_id, m.kind, s.v FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc",
        )?;
        let filas = q
            .query_map([], |f| Ok(Fila { id: f.get(0)?, project_id: f.get(1)?, space_id: f.get(2)?, kind: f.get(3)?, v: f.get(4)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut p = db.prepare("SELECT id, space_id FROM project")?;
        let espacio_de = p.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(Indice { memorias: huella.4, huella, filas, espacio_de })
    }

    /// `Ambito::sql`, en memoria (hay un test que comprueba que dan lo mismo).
    fn ve(&self, a: &crate::memoria::Ambito, f: &Fila) -> bool {
        use crate::memoria::Ambito;
        let espacio = |s: Option<i64>| f.project_id != 0 || f.space_id.is_none() || (s.is_some() && f.space_id == s);
        match *a {
            Ambito::Proyecto { id, space } => (f.project_id == id || f.project_id == 0) && espacio(space),
            Ambito::Espacio(Some(s)) => (f.project_id == 0 || self.espacio_de.get(&f.project_id) == Some(&Some(s))) && espacio(Some(s)),
            Ambito::Espacio(None) => self.espacio_de.get(&f.project_id) == Some(&None) && espacio(None),
            Ambito::Todos => true,
        }
    }

    /// Los `k` más parecidos a `q` en el ámbito (de mayor a menor) y el coseno de `pedidas`.
    fn buscar(&self, q: &[f32], a: &crate::memoria::Ambito, kind: Option<&str>, k: usize, pedidas: &[String]) -> (Cosenos, Cosenos) {
        let mut todos: Vec<(usize, f32)> = self
            .filas
            .iter()
            .enumerate()
            .filter(|(_, f)| kind.is_none_or(|k| f.kind == k) && self.ve(a, f))
            .map(|(i, f)| (i, coseno(q, &f.v)))
            .collect();
        let pedido: std::collections::HashSet<&str> = pedidas.iter().map(|s| s.as_str()).collect();
        let de_pedidas = todos.iter().filter(|(i, _)| pedido.contains(self.filas[*i].id.as_str())).map(|(i, c)| (self.filas[*i].id.clone(), *c)).collect();
        let k = k.min(todos.len());
        if k > 0 {
            todos.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
            todos.truncate(k);
        }
        todos.sort_by(|a, b| b.1.total_cmp(&a.1));
        (todos.into_iter().map(|(i, c)| (self.filas[i].id.clone(), c)).collect(), de_pedidas)
    }

    fn listo(&self) -> bool {
        self.memorias > 0 && self.filas.len() as f64 >= 0.95 * self.memorias as f64
    }
}

/// Lo que el servicio respondería para el vector `q`, sin servicio: carga el índice de `db` y busca
/// (para los tests, que comparan con `cosenos`, que filtra el ámbito en SQL).
pub fn parecidas_en_base(db: &rusqlite::Connection, q: &[f32], ambito: &crate::memoria::Ambito, kind: Option<crate::memoria::Tipo>, k: usize) -> Result<Vec<(String, f32)>> {
    let i = Indice::cargar(db)?;
    Ok(i.buscar(q, ambito, kind.map(|k| k.as_str()), k, &[]).0)
}

/// Los índices del servicio, por base, con su conexión.
fn indices() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, (rusqlite::Connection, Indice)>> {
    static I: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, (rusqlite::Connection, Indice)>>> = std::sync::OnceLock::new();
    I.get_or_init(Default::default)
}

/// Lo que responde el servicio a una búsqueda: si la base tiene vectores para casi todo, los `k` más
/// parecidos y el coseno de las memorias pedidas.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Parecidas {
    pub listo: bool,
    #[serde(default)]
    pub top: Vec<(String, f32)>,
    #[serde(default)]
    pub pedidas: Vec<(String, f32)>,
}

fn buscar_en_servicio(gemma: &Gemma, b: &Busqueda) -> Result<Parecidas> {
    let ruta = PathBuf::from(&b.db);
    // El vector de la pregunta primero, sin el candado: es lo que más tarda.
    let q = gemma.vector(&b.texto, Tipo::Pregunta)?;
    let mut ix = indices().lock().map_err(|_| anyhow!("index lock"))?;
    if !ix.contains_key(&ruta) {
        let c = crate::db::abrir(&ruta)?;
        let i = Indice::cargar(&c)?;
        ix.insert(ruta.clone(), (c, i));
    }
    let (c, i) = ix.get_mut(&ruta).expect("recién puesto");
    if huella(c)? != i.huella {
        *i = Indice::cargar(c)?;
    }
    if !i.listo() {
        return Ok(Parecidas::default());
    }
    let (top, pedidas) = i.buscar(&q, &b.ambito, b.kind.as_deref(), b.k, &b.ids);
    Ok(Parecidas { listo: true, top, pedidas })
}

#[derive(Serialize, Deserialize, Debug)]
struct Busqueda {
    db: String,
    texto: String,
    ambito: crate::memoria::Ambito,
    #[serde(default)]
    kind: Option<String>,
    k: usize,
    #[serde(default)]
    ids: Vec<String>,
}

/// Por qué el servicio no dio las parecidas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallo {
    /// Apagado (`--off`, `OMOIKANE_SEMANTIC=0`) o sin instalar.
    Apagado,
    /// No hay servicio (o no escucha): se deja arrancando para la próxima vez.
    SinServicio,
    /// No respondió dentro del presupuesto (CPU ocupada, texto largo).
    Tarde,
    /// La base aún no tiene vectores para casi todo (indexando).
    Indexando,
    /// Cualquier otro error (respuesta rota, token…).
    Error,
}

impl Fallo {
    pub fn como_texto(self) -> &'static str {
        match self {
            Fallo::Apagado => "off",
            Fallo::SinServicio => "no_service",
            Fallo::Tarde => "timeout",
            Fallo::Indexando => "indexing",
            Fallo::Error => "error",
        }
    }
}

/// Las memorias más parecidas a `texto` en `ambito`, calculadas por el servicio (que tiene el modelo
/// y los vectores en memoria), y el coseno de `ids`. `None` si el modelo está apagado o sin
/// instalar, el servicio no está o no responde en `presupuesto`, o la base aún no tiene vectores
/// para casi todo: entonces se sigue por palabras. Quien llama decide antes si el modelo está
/// encendido para esta base (`en_uso`): aquí solo se mira que no esté prohibido.
pub fn parecidas(
    db: &std::path::Path,
    texto: &str,
    ambito: &crate::memoria::Ambito,
    kind: Option<crate::memoria::Tipo>,
    k: usize,
    ids: &[String],
    presupuesto: Duration,
) -> Option<Parecidas> {
    parecidas_o_fallo(db, texto, ambito, kind, k, ids, presupuesto).ok()
}

/// `parecidas`, diciendo por qué no llegaron.
pub fn parecidas_o_fallo(
    db: &std::path::Path,
    texto: &str,
    ambito: &crate::memoria::Ambito,
    kind: Option<crate::memoria::Tipo>,
    k: usize,
    ids: &[String],
    presupuesto: Duration,
) -> std::result::Result<Parecidas, Fallo> {
    if !permitido() || modelo().is_none() {
        return Err(Fallo::Apagado);
    }
    let Some(r) = registro() else {
        arrancar(Some(db));
        return Err(Fallo::SinServicio);
    };
    let p = Peticion {
        token: r.token.clone(),
        db: Some(db.to_string_lossy().into_owned()),
        buscar: Some(Busqueda {
            db: db.to_string_lossy().into_owned(),
            texto: texto.to_string(),
            ambito: *ambito,
            kind: kind.map(|k| k.as_str().to_string()),
            k,
            ids: ids.to_vec(),
        }),
        ..Default::default()
    };
    match pedir(&r, &p, presupuesto) {
        Ok(v) => match serde_json::from_value::<Parecidas>(v) {
            Ok(p) if p.listo => Ok(p),
            Ok(_) => Err(Fallo::Indexando),
            Err(_) => Err(Fallo::Error),
        },
        Err(e) => match e.downcast_ref::<std::io::Error>().map(|e| e.kind()) {
            Some(std::io::ErrorKind::ConnectionRefused) => {
                arrancar(Some(db));
                Err(Fallo::SinServicio)
            }
            Some(std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => Err(Fallo::Tarde),
            _ => Err(Fallo::Error),
        },
    }
}

/// Las bases que el servicio indexa: las que le han pedido algo.
fn bases_pedidas() -> &'static std::sync::Mutex<std::collections::BTreeSet<PathBuf>> {
    static B: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<PathBuf>>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// El vector de una consulta para buscar en `db` (que el servicio apunta para indexarla).
pub fn vector_consulta(db: &std::path::Path, texto: &str, presupuesto: Duration) -> Option<Vec<f32>> {
    vectores_en(Some(db), &[texto], Tipo::Pregunta, presupuesto)?.into_iter().next()
}

/// Avisa al servicio de que `db` ha cambiado para que calcule los vectores que falten, sin esperar
/// (lo arranca si no está). Para `remember`, `update`, `import` y la sincronización.
pub fn avisar(db: &std::path::Path) {
    if !en_uso_en(db) {
        return;
    }
    let _ = vectores_en(Some(db), &[], Tipo::Documento, Duration::from_millis(100));
}

fn ahora() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn registro() -> Option<Registro> {
    serde_json::from_str(&std::fs::read_to_string(carpeta()?.join(REGISTRO)).ok()?).ok()
}

/// Manda `p` y lee una línea de respuesta, sin pasarse de `presupuesto` en total.
fn pedir(r: &Registro, p: &Peticion, presupuesto: Duration) -> Result<serde_json::Value> {
    let inicio = Instant::now();
    let dir = SocketAddr::from(([127, 0, 0, 1], r.puerto));
    let s = TcpStream::connect_timeout(&dir, presupuesto)?;
    let resto = || presupuesto.saturating_sub(inicio.elapsed()).max(Duration::from_millis(1));
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(resto()))?;
    let mut linea = serde_json::to_string(p)?;
    linea.push('\n');
    (&s).write_all(linea.as_bytes())?;
    s.set_read_timeout(Some(resto()))?;
    let mut respuesta = String::new();
    BufReader::new(&s).read_line(&mut respuesta)?;
    let v: serde_json::Value = serde_json::from_str(&respuesta).context("semantic service: bad answer")?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("semantic service: {e}");
    }
    Ok(v)
}

/// ¿Responde el servicio? Devuelve su registro si sí.
pub fn vivo(presupuesto: Duration) -> Option<Registro> {
    let r = registro()?;
    let v = pedir(&r, &Peticion { token: r.token.clone(), ping: true, ..Default::default() }, presupuesto).ok()?;
    (v.get("pid").and_then(|p| p.as_u64()) == Some(r.pid as u64)).then_some(r)
}

/// Lanza el servicio aparte si hay modelo y no se lanzó hace poco, con la base que lo pide (`db`) para
/// que empiece a indexarla en cuanto cargue. No espera a que cargue.
pub fn arrancar(db: Option<&std::path::Path>) {
    if !permitido() || modelo().is_none() {
        return;
    }
    let Some(c) = carpeta() else { return };
    let marca = c.join(ARRANCANDO);
    let reciente = std::fs::read_to_string(&marca).ok().and_then(|t| t.trim().parse::<u64>().ok()).is_some_and(|t| ahora().saturating_sub(t) < REINTENTO_ARRANQUE);
    if reciente {
        return;
    }
    let _ = std::fs::create_dir_all(&c);
    if std::fs::write(&marca, ahora().to_string()).is_ok() {
        match db {
            Some(d) => crate::sync::lanzar_aparte(&["--db", &d.to_string_lossy(), "model", "serve-semantic"]),
            None => crate::sync::lanzar_aparte(&["model", "serve-semantic"]),
        }
    }
}

/// Los vectores de `textos`, si el servicio los da dentro de `presupuesto`. Si no hay servicio, lo
/// deja arrancando y devuelve `None`: quien llama sigue sin significado.
pub fn vectores(textos: &[&str], tipo: Tipo, presupuesto: Duration) -> Option<Vec<Vec<f32>>> {
    vectores_en(None, textos, tipo, presupuesto)
}

fn vectores_en(db: Option<&std::path::Path>, textos: &[&str], tipo: Tipo, presupuesto: Duration) -> Option<Vec<Vec<f32>>> {
    if !permitido() || modelo().is_none() {
        return None;
    }
    let Some(r) = registro() else {
        arrancar(db);
        return None;
    };
    let p = Peticion {
        token: r.token.clone(),
        tipo: Some(if tipo == Tipo::Pregunta { "pregunta" } else { "documento" }.into()),
        textos: textos.iter().map(|t| t.to_string()).collect(),
        ping: false,
        db: db.map(|d| d.to_string_lossy().into_owned()),
        buscar: None,
    };
    match pedir(&r, &p, presupuesto) {
        Ok(v) => serde_json::from_value(v.get("vectores")?.clone()).ok(),
        Err(e) => {
            // Rechazada o sin nadie escuchando: el servicio murió (o lo cerraron) y hay que lanzarlo
            // otra vez. Un plazo agotado es solo lentitud: no se lanza otro.
            let caido = e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
            if caido {
                arrancar(db);
            }
            None
        }
    }
}

fn token_nuevo() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn atender(s: TcpStream, modelo: &Gemma, token: &str, nombre: &str, uso: &AtomicU64) -> Result<()> {
    s.set_nodelay(true)?;
    let mut lector = BufReader::new(&s);
    let mut linea = String::new();
    while lector.read_line(&mut linea)? > 0 {
        uso.store(ahora(), Ordering::Relaxed);
        let respuesta = match serde_json::from_str::<Peticion>(&linea) {
            Err(e) => serde_json::json!({ "error": e.to_string() }),
            Ok(p) if p.token != token => serde_json::json!({ "error": "bad token" }),
            Ok(p) if p.ping => serde_json::json!({ "pid": std::process::id(), "modelo": nombre }),
            Ok(p) => {
                // Mientras se atiende, el indexado espera (`ceder`).
                CONSULTAS.fetch_add(1, Ordering::Relaxed);
                struct Hecha;
                impl Drop for Hecha {
                    fn drop(&mut self) {
                        CONSULTAS.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                let _hecha = Hecha;
                if let Some(db) = &p.db {
                    bases_pedidas().lock().map(|mut b| b.insert(PathBuf::from(db))).ok();
                }
                if let Some(b) = &p.buscar {
                    let r = match buscar_en_servicio(modelo, b) {
                        Ok(r) => serde_json::to_value(r).unwrap_or_default(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                    };
                    let mut salida = r.to_string();
                    salida.push('\n');
                    (&s).write_all(salida.as_bytes())?;
                    linea.clear();
                    continue;
                }
                let tipo = if p.tipo.as_deref() == Some("documento") { Tipo::Documento } else { Tipo::Pregunta };
                match p.textos.iter().map(|t| modelo.vector(t, tipo)).collect::<Result<Vec<_>>>() {
                    Ok(v) => serde_json::json!({ "vectores": v }),
                    Err(e) => serde_json::json!({ "error": e.to_string() }),
                }
            }
        };
        let mut salida = respuesta.to_string();
        salida.push('\n');
        (&s).write_all(salida.as_bytes())?;
        linea.clear();
    }
    Ok(())
}

/// El servicio (`omoi model serve-semantic`). Si ya hay uno respondiendo, sale sin hacer nada. `db`:
/// la base que lo arrancó, para indexarla en cuanto cargue el modelo.
pub fn servir(db: Option<&std::path::Path>) -> Result<()> {
    if !permitido() {
        bail!("the meaning model is off (`omoi model semantic --on` turns it on)");
    }
    if vivo(Duration::from_millis(500)).is_some() {
        return Ok(());
    }
    let dir = modelo().context("EmbeddingGemma is not installed")?;
    let c = carpeta().context("no home folder")?;
    std::fs::create_dir_all(&c)?;
    let gemma = Arc::new(Gemma::cargar(&dir)?);
    {
        let g = gemma.clone();
        std::thread::spawn(move || g.precalentar());
    }
    let escucha = TcpListener::bind(("127.0.0.1", 0))?;
    let yo = Registro {
        puerto: escucha.local_addr()?.port(),
        token: token_nuevo()?,
        pid: std::process::id(),
        modelo: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    // Primero a un temporal y luego renombrar: quien lee nunca ve un registro a medias.
    let tmp = c.join(format!("{REGISTRO}.{}", yo.pid));
    std::fs::write(&tmp, serde_json::to_string(&yo)?)?;
    std::fs::rename(&tmp, c.join(REGISTRO))?;
    let _ = std::fs::remove_file(c.join(ARRANCANDO));
    if let Some(d) = db {
        bases_pedidas().lock().map(|mut b| b.insert(d.to_path_buf())).ok();
    }
    let uso = Arc::new(AtomicU64::new(ahora()));
    let ocioso = std::env::var("OMOIKANE_SEMANTIC_IDLE").ok().and_then(|s| s.parse().ok()).unwrap_or(OCIOSO_POR_DEFECTO);
    {
        // Vigía: se cierra tras `ocioso` sin peticiones, o si otro servicio arrancó a la vez y se
        // quedó con el registro (gana el último que lo escribió).
        let (uso, yo, c) = (uso.clone(), yo.clone(), c.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let mio = registro().is_some_and(|r| r.pid == yo.pid && r.puerto == yo.puerto);
                if !mio {
                    std::process::exit(0);
                }
                if ahora().saturating_sub(uso.load(Ordering::Relaxed)) >= ocioso {
                    let _ = std::fs::remove_file(c.join(REGISTRO));
                    std::process::exit(0);
                }
            }
        });
    }
    {
        // Indexador: calcula los vectores que falten en las bases que han pedido algo, solo cuando
        // lleva un segundo sin peticiones (comparten la CPU), por lotes (`indexar`), y parándose entre
        // paso y paso si entra una consulta (`ceder`). Mientras le quede trabajo, cuenta como uso: el
        // servicio no se cierra a medias.
        let (gemma, uso) = (gemma.clone(), uso.clone());
        std::thread::spawn(move || {
            let mut abiertas: std::collections::BTreeMap<PathBuf, rusqlite::Connection> = Default::default();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let bases: Vec<PathBuf> = bases_pedidas().lock().map(|b| b.iter().cloned().collect()).unwrap_or_default();
                for ruta in bases {
                    if ahora().saturating_sub(uso.load(Ordering::Relaxed)) < 1 {
                        break;
                    }
                    if !abiertas.contains_key(&ruta) {
                        match crate::db::abrir(&ruta) {
                            Ok(c) => {
                                abiertas.insert(ruta.clone(), c);
                            }
                            Err(_) => {
                                bases_pedidas().lock().map(|mut b| b.remove(&ruta)).ok();
                                continue;
                            }
                        }
                    }
                    let db = &abiertas[&ruta];
                    let mut seguir = || ahora().saturating_sub(uso.load(Ordering::Relaxed)) >= 1;
                    // Cada memoria hecha cuenta como uso (un segundo atrás, para no frenarse a sí mismo).
                    let mut hecha = |_| {
                        uso.fetch_max(ahora().saturating_sub(1), Ordering::Relaxed);
                    };
                    match indexar(db, &gemma, 64, &mut seguir, &mut hecha) {
                        Ok(0) => {
                            // Al día: se saca de la lista hasta que alguien vuelva a pedir algo.
                            bases_pedidas().lock().map(|mut b| b.remove(&ruta)).ok();
                            abiertas.remove(&ruta);
                        }
                        Ok(_) => {}
                        Err(_) => {
                            bases_pedidas().lock().map(|mut b| b.remove(&ruta)).ok();
                            abiertas.remove(&ruta);
                        }
                    }
                }
            }
        });
    }
    for s in escucha.incoming().flatten() {
        let (gemma, token, nombre, uso) = (gemma.clone(), yo.token.clone(), yo.modelo.clone(), uso.clone());
        std::thread::spawn(move || {
            let _ = atender(s, &gemma, &token, &nombre, &uso);
        });
    }
    Ok(())
}
