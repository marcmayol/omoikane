//! La documentación del proyecto, en solo lectura (1.6): un proyecto que ya tiene su documentación en
//! Markdown (una carpeta `docs/`, una LLM Wiki al estilo de Karpathy, una bóveda de Obsidian) le dice a
//! Omoikane dónde está (`omoi docs add <carpeta>`), y el recall de cada mensaje trae también, si viene a
//! cuento, el apartado que toca. No se copia como memorias: los ficheros siguen siendo de quien los
//! escribe, y aquí solo hay un índice para encontrarlos.
//!
//! - Cada `.md` se parte por sus títulos (`#`, `##`, `###`, fuera de los bloques de código), en trozos de
//!   como mucho `TROZO_MAX` caracteres, en una tabla FTS5 con el mismo tokenizador que las memorias.
//! - El índice se pone al día al empezar la sesión (solo lo que cambió de tamaño o de fecha, y se borra
//!   lo que ya no está), con un tope de tiempo: lo que no quepa queda para la siguiente. Un error nunca
//!   corta el arranque. `omoi docs add` indexa en el momento.
//! - En cada mensaje, el mismo criterio que las memorias (palabras útiles, palabras raras, cobertura del
//!   peso IDF del mensaje), con las frecuencias de la documentación: como mucho un trozo, una vez por
//!   sesión (`docs_seen`, aparte de `recall_log`, que alimenta `memory_use` y el diario).
//!
//! Todo es local, como `diary` o `session_steps`: las tablas aparecen con el primer `omoi docs add`, no
//! al abrir la base, no viajan con la sincronización ni con `omoi export` (la documentación ya tiene su
//! repositorio, y las rutas son de este equipo) y una 1.0 que abra la base las ignora. Sin carpetas
//! configuradas, el recall de cada mensaje hace una consulta más a `sqlite_schema`.

use crate::memoria::AHORA;
use crate::recall;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

/// `docs_folder`: qué carpetas tiene cada proyecto (el texto de `proyecto::de`, como `session.project`).
/// `docs_file`: cada fichero indexado, por carpeta y ruta relativa, con su fecha y tamaño para saber si
/// cambió, y cuántos trozos tiene. `docs_fts`: los trozos; su `rowid` es `id << PARTE_BITS | parte`, así
/// que los de un fichero se borran y se juntan con él por rango de `rowid`, sin recorrer la tabla (una
/// columna `UNINDEXED` obligaba a leerla entera: indexar 785 ficheros tardaba 11 s). `docs_seen`: qué
/// trozos llegaron ya a cada sesión.
const TABLAS: &str = "
CREATE TABLE IF NOT EXISTS docs_folder (
  project  TEXT NOT NULL,
  folder   TEXT NOT NULL,
  added_at TEXT NOT NULL,
  PRIMARY KEY (project, folder)
);
CREATE TABLE IF NOT EXISTS docs_file (
  id     INTEGER PRIMARY KEY,
  folder TEXT NOT NULL,
  path   TEXT NOT NULL,
  mtime  INTEGER NOT NULL,
  size   INTEGER NOT NULL,
  chunks INTEGER NOT NULL DEFAULT 0,
  UNIQUE (folder, path)
);
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
  title, body,
  tokenize='unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS docs_seen (
  session TEXT NOT NULL,
  chunk   TEXT NOT NULL,
  at      TEXT NOT NULL,
  PRIMARY KEY (session, chunk)
);
";

/// Bits del `rowid` de `docs_fts` para el número de trozo dentro del fichero; un fichero no pasa de
/// `MAX_BYTES_FICHERO` y no llega ni de lejos a 2^20 trozos.
const PARTE_BITS: i64 = 20;
/// El fichero de un trozo, en SQL.
const FICHERO_DEL_TROZO: &str = "(docs_fts.rowid >> 20)";

/// Tamaño máximo de un trozo. Un apartado más largo se parte por párrafos: un trozo muy largo contiene
/// casi cualquier palabra y «cubre» cualquier mensaje (lo mismo que `recall::LARGO_PARA_RECALL` con las
/// memorias, que son más cortas que un apartado de documentación).
pub const TROZO_MAX: usize = 2000;
/// Cuánto del trozo llega al agente; el resto lo lee él del fichero si le hace falta.
pub const FRAGMENTO: usize = 700;
/// Ficheros que se miran como mucho en cada carpeta, y el tamaño a partir del cual un fichero no se
/// indexa (un volcado, no documentación).
pub const MAX_FICHEROS: usize = 5000;
pub const MAX_BYTES_FICHERO: u64 = 2 << 20;
/// Parte del peso IDF del mensaje que tiene que cubrir un trozo. Más que la de las memorias
/// (`recall::COBERTURA_MIN`, 0,50): un apartado es más largo que una memoria y contiene más palabras.
/// Mirado con la documentación de Omoikane sola y con 785 `.md` más (14.548 trozos), sobre 302 mensajes
/// reales y 9 escritos a mano: con 0,50 traían algo 3 de los reales y 1 de los 4 escritos que no iban
/// de la documentación, todo ruido (0,52 a 0,56); con 0,60 queda 1 de los reales (0,68-0,70, y dudoso:
/// «tema claro, tema oscuro» trae lo de la vista web) y siguen todos los que sí iban (0,62 a 1,00).
pub const COBERTURA_MIN: f64 = 0.60;
/// Ficheros por transacción al indexar.
const TANDA: usize = 20;
/// Cuántos candidatos por BM25 se miran antes de medir su cobertura.
const CANDIDATOS: usize = 20;
/// Las carpetas que nunca son documentación.
const SALTAR: &[&str] = &["node_modules", "target", "vendor", "__pycache__"];

/// La tabla `nombre` existe.
fn hay(db: &Connection, nombre: &str) -> bool {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)", [nombre], |f| f.get(0)).unwrap_or(false)
}

fn crear(db: &Connection) -> Result<()> {
    db.execute_batch(TABLAS)?;
    Ok(())
}

/// La carpeta tal como se guarda: la ruta real escrita como la de los proyectos (sin el `\\?\` de
/// Windows y con la letra de unidad en mayúscula).
fn normalizar(carpeta: &Path) -> Result<String> {
    let real = crate::proyecto::canonica(carpeta)?;
    anyhow::ensure!(real.is_dir(), crate::tr!("{} is not a folder", "{} no es una carpeta", carpeta.display()));
    Ok(crate::proyecto::texto(&real))
}

/// Una carpeta de documentación de un proyecto, con lo que tiene indexado.
#[derive(Debug, Clone, Serialize)]
pub struct Carpeta {
    pub folder: String,
    pub added_at: String,
    pub files: i64,
    pub chunks: i64,
}

/// Lo que hizo un refresco.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Refresco {
    /// Ficheros indexados de nuevo (nuevos o cambiados).
    pub indexed: usize,
    /// Ficheros que ya no están y salen del índice.
    pub removed: usize,
    /// Ficheros sin cambios.
    pub unchanged: usize,
    /// Ficheros que no se indexan (demasiado grandes, ilegibles o más allá de `MAX_FICHEROS`).
    pub skipped: usize,
    /// Si se miró todo; si no (tope de tiempo), lo que falta queda para la próxima.
    pub complete: bool,
}

/// `omoi docs add`: apunta la carpeta en el proyecto y la indexa ahora.
pub fn anadir(db: &Connection, project: &str, carpeta: &Path) -> Result<(String, Refresco)> {
    let folder = normalizar(carpeta)?;
    crear(db)?;
    db.execute(&format!("INSERT OR IGNORE INTO docs_folder(project, folder, added_at) VALUES (?1, ?2, {AHORA})"), params![project, folder])?;
    let r = refrescar(db, &folder, None)?;
    Ok((folder, r))
}

/// `omoi docs remove`: quita la carpeta del proyecto y, si ningún otro proyecto la usa, su índice.
/// Devuelve si estaba. Acepta la ruta tal cual se dio o como sale en `omoi docs list`.
pub fn quitar(db: &Connection, project: &str, carpeta: &str) -> Result<bool> {
    if !hay(db, "docs_folder") {
        return Ok(false);
    }
    let folder = normalizar(Path::new(carpeta)).unwrap_or_else(|_| carpeta.to_string());
    let n = db.execute("DELETE FROM docs_folder WHERE project = ?1 AND folder = ?2", params![project, folder])?;
    let usada: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM docs_folder WHERE folder = ?1)", [&folder], |f| f.get(0))?;
    if n > 0 && !usada {
        let tx = db.unchecked_transaction()?;
        borrar_trozos(&tx, "SELECT id FROM docs_file WHERE folder = ?1", &folder)?;
        tx.execute("DELETE FROM docs_file WHERE folder = ?1", [&folder])?;
        tx.commit()?;
    }
    Ok(n > 0)
}

/// Las carpetas del proyecto.
pub fn carpetas(db: &Connection, project: &str) -> Result<Vec<String>> {
    if !hay(db, "docs_folder") {
        return Ok(Vec::new());
    }
    let mut q = db.prepare("SELECT folder FROM docs_folder WHERE project = ?1 ORDER BY folder")?;
    let v = q.query_map([project], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// `omoi docs list`: las carpetas del proyecto con cuántos ficheros y trozos tienen indexados.
pub fn listar(db: &Connection, project: &str) -> Result<Vec<Carpeta>> {
    if !hay(db, "docs_folder") {
        return Ok(Vec::new());
    }
    let mut q = db.prepare(
        "SELECT d.folder, d.added_at,
                (SELECT count(*) FROM docs_file f WHERE f.folder = d.folder),
                (SELECT total(chunks) FROM docs_file f WHERE f.folder = d.folder)
         FROM docs_folder d WHERE d.project = ?1 ORDER BY d.folder",
    )?;
    let v = q
        .query_map([project], |f| Ok(Carpeta { folder: f.get(0)?, added_at: f.get(1)?, files: f.get(2)?, chunks: f.get::<_, f64>(3)? as i64 }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Un fichero de la carpeta: ruta relativa con `/`, fecha en ms y tamaño.
type Visto = (String, i64, u64);

/// Los `.md` de la carpeta, sin entrar en carpetas ocultas (`.git`, `.obsidian`) ni en `SALTAR`. El
/// segundo valor dice si se cortó en `MAX_FICHEROS`.
fn recorrer(raiz: &Path) -> Result<(Vec<Visto>, bool)> {
    let mut v = Vec::new();
    let mut pendientes = vec![raiz.to_path_buf()];
    let mut cortado = false;
    while let Some(dir) = pendientes.pop() {
        let Ok(entradas) = std::fs::read_dir(&dir) else { continue };
        for e in entradas.flatten() {
            let nombre = e.file_name().to_string_lossy().into_owned();
            if nombre.starts_with('.') {
                continue;
            }
            let Ok(tipo) = e.file_type() else { continue };
            let ruta = e.path();
            if tipo.is_dir() {
                if !SALTAR.contains(&nombre.as_str()) {
                    pendientes.push(ruta);
                }
                continue;
            }
            let ext = ruta.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("md" | "markdown")) {
                continue;
            }
            if v.len() >= MAX_FICHEROS {
                cortado = true;
                break;
            }
            let Ok(meta) = e.metadata() else { continue };
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
            let rel = ruta.strip_prefix(raiz).unwrap_or(&ruta).to_string_lossy().replace('\\', "/");
            v.push((rel, mtime, meta.len()));
        }
        if cortado {
            break;
        }
    }
    Ok((v, cortado))
}

/// Un trozo: el título del apartado (con los de arriba: «Sync › Spaces») y su texto.
#[derive(Debug, Clone, PartialEq)]
pub struct Trozo {
    pub titulo: String,
    pub texto: String,
}

/// Parte un Markdown por sus títulos `#`, `##` y `###` (los de dentro de un bloque de código no
/// cuentan), y cada apartado en trozos de como mucho `TROZO_MAX` caracteres por párrafos. Lo de antes
/// del primer título lleva el nombre del fichero. La cabecera YAML (`---`) de Obsidian no se indexa.
pub fn trocear(nombre: &str, md: &str) -> Vec<Trozo> {
    let md = md.strip_prefix('\u{feff}').unwrap_or(md);
    let cuerpo = match md.strip_prefix("---\n").or_else(|| md.strip_prefix("---\r\n")) {
        Some(resto) => match resto.find("\n---") {
            Some(i) => resto[i + 4..].trim_start_matches(['-', '\r', '\n']),
            None => md,
        },
        None => md,
    };
    let mut niveles: [String; 3] = Default::default();
    let mut apartados: Vec<(String, String)> = Vec::new();
    let mut actual = (nombre.to_string(), String::new());
    let mut en_codigo = false;
    for linea in cuerpo.lines() {
        let t = linea.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            en_codigo = !en_codigo;
        }
        let nivel = if en_codigo { 0 } else { t.chars().take_while(|c| *c == '#').count() };
        if (1..=3).contains(&nivel) && t[nivel..].starts_with(' ') {
            apartados.push(std::mem::take(&mut actual));
            niveles[nivel - 1] = t[nivel..].trim().trim_end_matches('#').trim().to_string();
            for n in niveles.iter_mut().skip(nivel) {
                n.clear();
            }
            let titulo = niveles.iter().filter(|n| !n.is_empty()).cloned().collect::<Vec<_>>().join(" › ");
            actual = (titulo, String::new());
            continue;
        }
        actual.1.push_str(linea);
        actual.1.push('\n');
    }
    apartados.push(actual);
    let mut v = Vec::new();
    for (titulo, texto) in apartados {
        if texto.trim().is_empty() {
            continue;
        }
        for parte in partir(texto.trim(), TROZO_MAX) {
            v.push(Trozo { titulo: titulo.clone(), texto: parte });
        }
    }
    v
}

/// `texto` en partes de como mucho `max` caracteres, cortando por párrafos (o por líneas, o donde
/// caiga, si un párrafo solo ya pasa del tope).
fn partir(texto: &str, max: usize) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut actual = String::new();
    let meter = |trozo: &str, actual: &mut String, v: &mut Vec<String>| {
        if actual.chars().count() + trozo.chars().count() + 2 > max && !actual.is_empty() {
            v.push(std::mem::take(actual).trim().to_string());
        }
        if !actual.is_empty() {
            actual.push_str("\n\n");
        }
        actual.push_str(trozo);
    };
    for parrafo in texto.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if parrafo.chars().count() <= max {
            meter(parrafo, &mut actual, &mut v);
            continue;
        }
        for linea in parrafo.lines() {
            let letras: Vec<char> = linea.chars().collect();
            for c in letras.chunks(max) {
                meter(&c.iter().collect::<String>(), &mut actual, &mut v);
            }
        }
    }
    if !actual.trim().is_empty() {
        v.push(actual.trim().to_string());
    }
    v
}

/// Guarda los trozos de una tanda de ficheros: primero se borran los viejos de todos y después se meten
/// los nuevos, cada paso en orden de `rowid`. FTS5 vuelca a disco lo que lleva en memoria cada vez que
/// le llega un `rowid` que no es mayor que el anterior; borrar e insertar fichero a fichero lo hacía
/// en cada uno, y reindexar 785 ficheros tardaba 38 s en vez de unos 3.
fn guardar_tanda(db: &Connection, folder: &str, tanda: Vec<Indexado>) -> Result<()> {
    if tanda.is_empty() {
        return Ok(());
    }
    let empieza = Instant::now();
    let n = tanda.len();
    let tx = db.unchecked_transaction()?;
    let mut con_id: Vec<(i64, Vec<Trozo>)> = Vec::with_capacity(tanda.len());
    for f in tanda {
        let id: i64 = tx.query_row(
            "INSERT INTO docs_file(folder, path, mtime, size, chunks) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(folder, path) DO UPDATE SET mtime = excluded.mtime, size = excluded.size, chunks = excluded.chunks RETURNING id",
            params![folder, f.path, f.mtime, f.size, f.trozos.len() as i64],
            |x| x.get(0),
        )?;
        con_id.push((id, f.trozos));
    }
    con_id.sort_by_key(|x| x.0);
    for (id, _) in &con_id {
        borrar_trozos_de(&tx, *id)?;
    }
    {
        let mut ins = tx.prepare_cached("INSERT INTO docs_fts(rowid, title, body) VALUES (?1, ?2, ?3)")?;
        for (id, trozos) in &con_id {
            for (i, t) in trozos.iter().enumerate() {
                ins.execute(params![(id << PARTE_BITS) | i as i64, t.titulo, t.texto])?;
            }
        }
    }
    let antes_de_guardar = empieza.elapsed();
    tx.commit()?;
    if std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()) {
        eprintln!("TRAZA docs_tanda {}", serde_json::json!({"files": n, "ms": empieza.elapsed().as_secs_f64() * 1000.0, "commit_ms": (empieza.elapsed() - antes_de_guardar).as_secs_f64() * 1000.0}));
    }
    Ok(())
}

/// Un fichero leído y troceado, a falta de guardarlo.
struct Indexado {
    path: String,
    mtime: i64,
    size: i64,
    trozos: Vec<Trozo>,
}

/// Borra los trozos del fichero `id` (por rango de `rowid`).
fn borrar_trozos_de(db: &Connection, id: i64) -> Result<()> {
    db.execute("DELETE FROM docs_fts WHERE rowid BETWEEN ?1 AND ?2", params![id << PARTE_BITS, ((id + 1) << PARTE_BITS) - 1])?;
    Ok(())
}

/// Borra los trozos de los ficheros que devuelve `ids` (una consulta con `?1`).
fn borrar_trozos(db: &Connection, ids: &str, p: &str) -> Result<()> {
    let v: Vec<i64> = db.prepare(ids)?.query_map([p], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    for id in v {
        borrar_trozos_de(db, id)?;
    }
    Ok(())
}

/// Pone al día el índice de una carpeta: indexa lo nuevo o cambiado (por fecha y tamaño) y quita lo
/// que ya no está. Con `hasta`, para al llegar a esa hora; lo que falte, la próxima vez.
pub fn refrescar(db: &Connection, folder: &str, hasta: Option<Instant>) -> Result<Refresco> {
    let raiz = PathBuf::from(folder);
    let mut r = Refresco::default();
    // Una carpeta que no está (un disco que no está montado) no se toca: su índice sigue sirviendo.
    if !raiz.is_dir() {
        return Ok(r);
    }
    let (ficheros, cortado) = recorrer(&raiz)?;
    let mut antes: HashMap<String, (i64, i64, i64)> = HashMap::new();
    {
        let mut q = db.prepare("SELECT path, id, mtime, size FROM docs_file WHERE folder = ?1")?;
        for f in q.query_map([folder], |f| Ok((f.get::<_, String>(0)?, (f.get(1)?, f.get(2)?, f.get(3)?))))? {
            let (p, x) = f?;
            antes.insert(p, x);
        }
    }
    let vistos: BTreeSet<&str> = ficheros.iter().map(|f| f.0.as_str()).collect();
    let cambiados: Vec<&Visto> = ficheros
        .iter()
        .filter(|(p, mtime, size)| antes.get(p).is_none_or(|(_, m, s)| *m != *mtime || *s != *size as i64))
        .collect();
    r.unchanged = ficheros.len() - cambiados.len();
    r.skipped = usize::from(cortado);
    r.complete = true;
    // Por tandas de `TANDA` ficheros: una transacción larga dejaría la base bloqueada para los demás
    // procesos. El tope de tiempo se mira en cada fichero; lo indexado hasta ahí queda guardado.
    let mut tanda: Vec<Indexado> = Vec::new();
    for (rel, mtime, size) in cambiados {
        if hasta.is_some_and(|h| Instant::now() >= h) {
            r.complete = false;
            break;
        }
        if tanda.len() >= TANDA {
            guardar_tanda(db, folder, std::mem::take(&mut tanda))?;
        }
        let texto = if *size > MAX_BYTES_FICHERO { None } else { std::fs::read(raiz.join(rel)).ok().map(|b| String::from_utf8_lossy(&b).into_owned()) };
        // Un fichero que no se puede leer o es demasiado grande queda apuntado con su fecha y sin
        // trozos, para no intentarlo en cada sesión.
        let nombre = Path::new(rel.as_str()).file_stem().map_or_else(|| rel.clone(), |s| s.to_string_lossy().into_owned());
        let mut trozos = texto.as_deref().map(|t| trocear(&nombre, t)).unwrap_or_default();
        trozos.truncate(1 << PARTE_BITS);
        if texto.is_some() {
            r.indexed += 1;
        } else {
            r.skipped += 1;
        }
        tanda.push(Indexado { path: rel.clone(), mtime: *mtime, size: *size as i64, trozos });
    }
    guardar_tanda(db, folder, tanda)?;
    // Lo que ya no está, solo si se recorrió la carpeta entera.
    if !cortado {
        let fuera: Vec<i64> = antes.iter().filter(|(p, _)| !vistos.contains(p.as_str())).map(|(_, x)| x.0).collect();
        if !fuera.is_empty() {
            let tx = db.unchecked_transaction()?;
            for id in &fuera {
                borrar_trozos_de(&tx, *id)?;
                tx.execute("DELETE FROM docs_file WHERE id = ?1", [id])?;
            }
            tx.commit()?;
        }
        r.removed = fuera.len();
    }
    Ok(r)
}

/// Al empezar la sesión (o tras compactar): lo que se trajo en ella puede volver. Sin carpetas no
/// hace nada; nunca falla ni corta el arranque. El índice no se pone al día aquí, sino aparte
/// ([`refrescar_aparte`]): con cientos de ficheros cambiados (otra rama) tardaba casi un segundo.
pub fn al_empezar(db: &Connection, _project: &str, sesion: &str) {
    if !hay(db, "docs_folder") {
        return;
    }
    if let Err(e) = db.execute("DELETE FROM docs_seen WHERE session = ?1 OR at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')", [sesion]) {
        eprintln!("omoikane: docs: {e:#}");
    }
}

/// Cada cuánto se pone al día como mucho el índice de un proyecto al empezar sesiones.
const REFRESCO_CADA: i64 = 60;

/// Al empezar la sesión, si el proyecto tiene carpetas de documentación: lanza `omoi docs refresh`
/// aparte, sin esperarlo, como la bajada de la sincronización. Una vez por minuto y proyecto como
/// mucho (varias sesiones a la vez lanzan uno). Mientras tanto el recall usa el índice que haya.
pub fn refrescar_aparte(db: &Connection, project: &str, ruta_db: &Path) {
    if !hay(db, "docs_folder") || carpetas(db, project).map(|v| v.is_empty()).unwrap_or(true) {
        return;
    }
    let ahora = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let marca = format!("docs_refrescado:{project}");
    let tomada = db
        .execute(
            "INSERT INTO meta(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v
             WHERE CAST(meta.v AS INTEGER) <= ?3 OR CAST(meta.v AS INTEGER) > ?4",
            params![marca, ahora.to_string(), ahora - REFRESCO_CADA, ahora],
        )
        .is_ok_and(|n| n == 1);
    if tomada {
        crate::sync::lanzar(&["docs", "refresh", "--dir", project], Some(ruta_db));
    }
}

/// El trozo de la documentación que trae un mensaje.
#[derive(Debug, Clone, Serialize)]
pub struct Encontrado {
    /// La ruta del fichero: relativa al proyecto si está dentro, entera si no.
    pub path: String,
    pub title: String,
    /// El fragmento que llega al agente (como mucho `FRAGMENTO` caracteres).
    pub excerpt: String,
    pub coverage: f64,
    /// Para `docs_seen`.
    #[serde(skip)]
    pub clave: String,
}

/// Minúsculas, sin tildes, partido por lo que no es letra o cifra (como `recall::fichas`).
fn palabras(t: &str) -> BTreeSet<String> {
    let sin_tildes: String = t
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            c => c,
        })
        .collect();
    sin_tildes.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
}

fn esta(t: &str, fichas: &BTreeSet<String>) -> bool {
    if t.chars().count() >= recall::PREFIJO_MIN { fichas.range(t.to_string()..).next().is_some_and(|p| p.starts_with(t)) } else { fichas.contains(t) }
}

/// El trozo de la documentación del proyecto que tiene que ver con `prompt`, con el mismo criterio que
/// las memorias (`recall::relevantes`): al menos `TERMINOS_MIN` palabras útiles, `RAROS_MIN` raras en
/// la documentación y `COBERTURA_MIN` (algo más que con las memorias) del peso IDF del mensaje. Sin contar los de `vistos`.
pub fn buscar(db: &Connection, project: &str, prompt: &str, vistos: &BTreeSet<String>) -> Result<Option<Encontrado>> {
    let folders = carpetas(db, project)?;
    if folders.is_empty() {
        return Ok(None);
    }
    let terminos = recall::utiles(prompt);
    if terminos.len() < recall::TERMINOS_MIN {
        return Ok(None);
    }
    let lista = folders.iter().map(|f| format!("'{}'", f.replace('\'', "''"))).collect::<Vec<_>>().join(",");
    let n: f64 = db.query_row(&format!("SELECT total(chunks) FROM docs_file WHERE folder IN ({lista})"), [], |f| f.get(0))?;
    // Lo normal es que todo el índice sea de este proyecto: entonces las frecuencias se cuentan sin
    // juntar cada trozo con su fichero (con 14.548 trozos, unos milisegundos por mensaje).
    let todo: f64 = db.query_row("SELECT total(chunks) FROM docs_file", [], |f| f.get(0))?;
    let filtro = if todo == n { String::new() } else { format!("JOIN docs_file f ON f.id = {FICHERO_DEL_TROZO} AND f.folder IN ({lista})") };
    if n == 0.0 {
        return Ok(None);
    }
    let consulta = |t: &str| if t.chars().count() >= recall::PREFIJO_MIN { format!("\"{t}\"*") } else { format!("\"{t}\"") };
    let mut q = db.prepare(&format!(
        "SELECT count(*) FROM docs_fts {filtro} WHERE docs_fts MATCH ?1"
    ))?;
    let df: Vec<f64> = terminos.iter().map(|t| q.query_row([consulta(t)], |f| f.get::<_, i64>(0)).unwrap_or(0) as f64).collect();
    // IDF de Lucene, como el recall: una palabra que no sale en la documentación pesa como una que sale
    // en un solo trozo, así que un mensaje que no va de ella no llega a la cobertura.
    let idf: Vec<f64> = df.iter().map(|d| (1.0 + (n - d.max(1.0) + 0.5) / (d.max(1.0) + 0.5)).ln()).collect();
    let total: f64 = idf.iter().sum();
    let raro = (recall::FRACCION_RARA * n).max(1.0);
    if total <= 0.0 || df.iter().filter(|d| **d > 0.0 && **d <= raro).count() < recall::RAROS_MIN {
        return Ok(None);
    }
    let o = terminos.iter().zip(&df).filter(|(_, d)| **d > 0.0).map(|(t, _)| consulta(t)).collect::<Vec<_>>().join(" OR ");
    let mut q = db.prepare(&format!(
        "SELECT docs_fts.title, docs_fts.body, docs_fts.rowid & {}, f.folder, f.path FROM docs_fts JOIN docs_file f ON f.id = {FICHERO_DEL_TROZO}
         WHERE docs_fts MATCH ?1 AND f.folder IN ({lista}) ORDER BY bm25(docs_fts, 3.0, 1.0) LIMIT {CANDIDATOS}",
        (1i64 << PARTE_BITS) - 1
    ))?;
    let filas: Vec<(String, String, i64, String, String)> =
        q.query_map([o], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?, f.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut mejor: Option<(f64, usize, Encontrado)> = None;
    for (titulo, cuerpo, part, folder, path) in filas {
        let clave = format!("{folder}/{path}#{part}");
        if vistos.contains(&clave) {
            continue;
        }
        let f = palabras(&format!("{titulo} {cuerpo} {path}"));
        let (mut peso, mut raros) = (0.0, 0);
        for (i, t) in terminos.iter().enumerate() {
            if esta(t, &f) {
                peso += idf[i];
                raros += usize::from(df[i] <= raro);
            }
        }
        let cobertura = peso / total;
        // Cuántas palabras del mensaje están en el título o el nombre del fichero: desempata a favor
        // del apartado que es justo del tema. Con todo igual, el primero por BM25.
        let de_titulo = palabras(&format!("{titulo} {path}"));
        let en_titulo = terminos.iter().filter(|t| esta(t, &de_titulo)).count();
        if raros < recall::RAROS_MIN || cobertura < COBERTURA_MIN || mejor.as_ref().is_some_and(|m| (m.0, m.1) >= (cobertura, en_titulo)) {
            continue;
        }
        // La palabra más rara del mensaje que está en el trozo: el fragmento empieza en su párrafo.
        let ancla = terminos.iter().zip(&idf).filter(|(t, _)| esta(t, &palabras(&cuerpo))).max_by(|a, b| a.1.total_cmp(b.1)).map(|(t, _)| t.clone());
        let e = Encontrado {
            path: ruta_para_mostrar(project, &folder, &path),
            title: titulo,
            excerpt: fragmento(&cuerpo, ancla.as_deref(), FRAGMENTO),
            coverage: cobertura,
            clave,
        };
        mejor = Some((cobertura, en_titulo, e));
    }
    Ok(mejor.map(|m| m.2))
}

/// La ruta del fichero relativa al proyecto si cae dentro (el agente trabaja ahí), o entera.
fn ruta_para_mostrar(project: &str, folder: &str, path: &str) -> String {
    let entera = Path::new(folder).join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    match entera.strip_prefix(project) {
        Ok(r) if !project.is_empty() && !crate::proyecto::es_general(project) => r.to_string_lossy().replace('\\', "/"),
        _ => entera.to_string_lossy().into_owned(),
    }
}

/// Hasta `n` caracteres del trozo, desde el párrafo donde sale `ancla` si no cabe desde el principio;
/// las líneas en blanco seguidas, juntas.
fn fragmento(cuerpo: &str, ancla: Option<&str>, n: usize) -> String {
    let parrafos: Vec<&str> = cuerpo.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
    let mut desde = 0;
    if cuerpo.chars().count() > n
        && let Some(a) = ancla
    {
        let mut largo = 0;
        for (i, p) in parrafos.iter().enumerate() {
            if esta(a, &palabras(p)) {
                // Si el párrafo ya cabe desde el principio, se empieza por el principio.
                if largo + p.chars().count() > n {
                    desde = i;
                }
                break;
            }
            largo += p.chars().count() + 2;
        }
    }
    let texto = parrafos[desde.min(parrafos.len())..].join("\n\n");
    let previo = if desde > 0 { "…\n" } else { "" };
    if texto.chars().count() > n {
        format!("{previo}{}…", texto.chars().take(n).collect::<String>().trim_end())
    } else {
        format!("{previo}{texto}")
    }
}

/// En cada mensaje: el bloque para el recall con el trozo que toca, o `None`. Sin carpetas cuesta una
/// consulta a `sqlite_schema`; nunca falla (un error solo deja el mensaje sin documentación).
pub fn al_preguntar(db: &Connection, project: &str, sesion: &str, prompt: &str) -> Option<String> {
    if prompt.trim().is_empty() || !hay(db, "docs_folder") {
        return None;
    }
    let r = (|| -> Result<Option<String>> {
        let vistos: BTreeSet<String> = {
            let mut q = db.prepare("SELECT chunk FROM docs_seen WHERE session = ?1")?;
            q.query_map([sesion], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let Some(e) = buscar(db, project, prompt, &vistos)? else { return Ok(None) };
        if !sesion.is_empty() {
            let tx = db.unchecked_transaction()?;
            tx.execute(&format!("INSERT OR IGNORE INTO docs_seen(session, chunk, at) VALUES (?1, ?2, {AHORA})"), params![sesion, e.clave])?;
            tx.commit()?;
        }
        let excerpt = e.excerpt.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
        Ok(Some(format!(
            "From this project's documentation (read-only, not memories; open the file for the rest):\n- {} › {}\n{excerpt}\n",
            e.path, e.title
        )))
    })();
    r.unwrap_or_else(|e| {
        eprintln!("omoikane: docs: {e:#}");
        None
    })
}

