//! Deriva por código (1.8.5, la idea de `validate --drift` de OKF Agent Memory): una memoria que habla
//! de unos ficheros (sus `paths` y los que nombra en el texto con su carpeta, `src/sync.rs`) puede
//! quedarse vieja cuando esos ficheros cambian mucho después de escribirla. La revisión lo propone
//! como `code_drift`; nunca cambia la memoria sola. «Está bien así» la da por revisada hoy: vuelve a
//! avisar solo si los ficheros cambian otra vez lo bastante.
//!
//! Qué cuenta como cambio, de lo más barato a lo más caro:
//! 1. La fecha de modificación del fichero: si ninguno ha cambiado desde que se escribió o se revisó
//!    la memoria, no hay nada que mirar (un `stat` por fichero). Sola no basta: el mismo día se toca
//!    el fichero del que se acaba de hablar, y eso no es deriva.
//! 2. El historial de git, si el fichero está en un repositorio: una orden `git log --numstat` por
//!    repositorio, solo con los ficheros que han cambiado, y en paralelo. Deriva: las líneas cambiadas
//!    desde entonces son al menos la mitad de lo que miden hoy (`PARTE_MIN`) y al menos `LINEAS_MIN`.
//! 3. Fuera de git, el diario de sesiones (`diario`): deriva si se editaron en `SESIONES_MIN`
//!    sesiones distintas después (sin contar la que guardó la memoria).
//!
//! Lo que cambia en las `GRACIA` horas después de escribirla no cuenta: lo normal es guardar la memoria
//! y después confirmar el trabajo del que habla. Un fichero que ya no existe ya lo avisa la revisión
//! (`broken_path`); aquí solo se mira lo que sigue en el disco.
//!
//! Calibrado con una copia de la memoria real de Marc del 8-nov-2026 (225 memorias; 66 con ficheros
//! que existen, 23 por sus `paths`). Contar commits no separa: con 5 o más salían 9, casi todas por
//! retoques (7 commits y 27 líneas sobre 535; un catálogo que se regenera en cada versión). La parte
//! cambiada sí: sale una sola, la nota que decía que el detector de desfasadas «sigue sin ser
//! incremental» (56 %; desde entonces lo es); detrás, la historia de las fases de Omoikane (42 %) y una
//! decisión sobre un experimento (38 %), que siguen valiendo, y el resto por debajo del 22 %. El
//! diario aún es corto (una semana) y no marcó ninguna: las sesiones de ese equipo trabajan en otras
//! carpetas, y queda para quien no usa git.
//!
//! Coste: nunca en el recall de cada mensaje. Lo calculado se guarda en `meta` (`id`, desde cuándo se
//! mira y el resultado) y vale mientras la memoria no cambie: con eso, la revisión entera (`omoi
//! review`, la herramienta `context`, la pestaña Revisar) tarda un milisegundo más, y el aviso al
//! empezar la sesión solo lee lo guardado, sin git ni disco. Lo calcula de nuevo una vez al día un
//! proceso aparte (`omoi review --refresh-drift`): con 225 memorias, unos 0,6-0,9 s, casi todo el
//! arranque de git en ocho repositorios. Es local: no viaja con la sincronización.

use crate::memoria::{Ambito, Memoria};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// El tipo de aviso en la revisión.
pub const DERIVA: &str = "code_drift";
/// La parte de lo que miden hoy sus ficheros que tiene que haber cambiado (líneas añadidas más quitadas).
pub const PARTE_MIN: f64 = 0.5;
/// Y al menos tantas líneas: en un fichero de diez, cinco líneas no dicen nada.
pub const LINEAS_MIN: usize = 20;
/// Fuera de git: sesiones del diario que editaron sus ficheros después.
pub const SESIONES_MIN: usize = 5;
/// Horas después de escribirla (o revisarla) en las que lo que cambia aún es el trabajo de esa sesión.
pub const GRACIA: i64 = 6;
/// Cuántos ficheros de una memoria se miran (los suyos primero, luego los que nombra).
const MAX_FICHEROS: usize = 20;
/// Ficheros más grandes no se cuentan por líneas (un modelo, un binario).
const MAX_BYTES: u64 = 4 << 20;
/// Cuántas órdenes de git a la vez.
const HILOS: usize = 8;
/// Lo más que se espera a un `git log`: si tarda más, sus ficheros se miran como si no fueran de git.
const ESPERA_GIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Dónde se guarda lo calculado hoy: `día` y una línea `id\tdesde\tdetalle` por memoria.
const CLAVE: &str = "deriva_codigo";
/// El día en que se lanzó el cálculo en segundo plano desde el aviso al empezar.
const CLAVE_LANZADO: &str = "deriva_codigo_lanzado";

/// Segundos desde 1970 de una fecha ISO (`2026-10-30T18:45:00.570Z`), en UTC.
fn segundos(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (a, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13).unwrap_or(0), n(14..16).unwrap_or(0), n(17..19).unwrap_or(0));
    // Días desde 1970-01-01 (Howard Hinnant).
    let y = if m <= 2 { a - 1 } else { a };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + se)
}

/// Si una ruta es absoluta en cualquier sistema (`C:/…`, `C:\…`, `/…`).
fn absoluta(r: &str) -> bool {
    let b = r.as_bytes();
    r.starts_with('/') || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
}

/// Dónde está en este equipo una ruta de la memoria: absoluta, `~/…` o relativa a su proyecto. Nunca una
/// de red (`\\servidor\…`, `//cdn…`): mirarla abriría una conexión (ver `proyecto::ruta_de_red`).
fn en_disco(m: &Memoria, ruta: &str) -> Option<PathBuf> {
    if crate::proyecto::ruta_de_red(ruta) {
        return None;
    }
    let d = if let Some(r) = ruta.strip_prefix("~/") {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|c| PathBuf::from(c).join(r))?
    } else if absoluta(ruta) {
        PathBuf::from(ruta)
    } else if !m.project.is_empty() && !m.project.starts_with('[') {
        Path::new(&m.project).join(ruta)
    } else {
        return None;
    };
    crate::proyecto::ruta_local(&d).then_some(d)
}

/// Las rutas de fichero que nombra un texto: palabras con una barra y una extensión al final
/// (`src/sync.rs`, `D:\repo\a.py`), sin las direcciones web. Las que no tienen carpeta (`sync.rs`) no:
/// en un repositorio hay muchos `main.rs`.
pub fn nombrados(texto: &str) -> Vec<String> {
    let mut r: Vec<String> = Vec::new();
    for p in texto.split(|c: char| c.is_whitespace() || "`'\"«»()[]{}<>,;|*".contains(c)) {
        let p = p.trim_end_matches(['.', ':', '!', '?']);
        // Ni direcciones web ni rutas de red (`//cdn.example.com/x.js`, `\\servidor\recurso\a.py`).
        if crate::proyecto::ruta_de_red(p) || p.contains("://") || p.starts_with("www.") || !(p.contains('/') || p.contains('\\')) || p.ends_with(['/', '\\']) {
            continue;
        }
        let nombre = p.rsplit(['/', '\\']).next().unwrap_or("");
        let Some((base, ext)) = nombre.rsplit_once('.') else { continue };
        let ext_ok = (1..=6).contains(&ext.len()) && ext.starts_with(|c: char| c.is_ascii_alphabetic()) && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if !base.is_empty() && ext_ok && !r.iter().any(|x| x == p) {
            r.push(p.to_string());
        }
    }
    r
}

/// Los ficheros de una memoria que existen en este equipo: sus rutas y las que nombra, sin repetir.
/// (lo que muestra, ruta en el disco)
fn ficheros(m: &Memoria) -> Vec<(String, PathBuf)> {
    let mut r: Vec<(String, PathBuf)> = Vec::new();
    let mut vistos = BTreeSet::new();
    let nombrados = nombrados(&m.content);
    for ruta in m.paths.iter().chain(&nombrados) {
        if r.len() >= MAX_FICHEROS {
            break;
        }
        if let Some(d) = en_disco(m, ruta)
            && d.is_file()
            && vistos.insert(crate::proyecto::comparable(&d.to_string_lossy().replace('\\', "/")))
        {
            r.push((ruta.replace('\\', "/"), d));
        }
    }
    r
}

fn modificado(p: &Path) -> Option<i64> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// La carpeta del repositorio de git que contiene `p` (la que tiene `.git`, carpeta o fichero de un
/// worktree), mirando cada carpeta una vez.
fn repo_de(p: &Path, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    let mut camino = Vec::new();
    let mut d = p.parent();
    let mut hallado = None;
    while let Some(x) = d {
        if let Some(r) = cache.get(x) {
            hallado = r.clone();
            break;
        }
        camino.push(x.to_path_buf());
        if x.join(".git").exists() {
            hallado = Some(x.to_path_buf());
            break;
        }
        d = x.parent();
    }
    for c in camino {
        cache.insert(c, hallado.clone());
    }
    hallado
}

/// Un cambio de git en un fichero: (commit, cuándo, líneas añadidas más quitadas).
type Cambio = (String, i64, usize);
/// Los cambios de un repositorio por fichero (ruta comparable).
type Log = HashMap<String, Vec<Cambio>>;
/// Lo guardado: `id -> (desde, detalle)`.
type Guardado = HashMap<String, (String, String)>;
/// Del diario, por fichero: (sesión, último momento, ids guardados en ella).
type PorFichero = HashMap<String, Vec<(String, String, Vec<String>)>>;

/// Los cambios de `rel` (rutas relativas al repositorio, con `/`) desde `desde` (segundos), por ruta
/// comparable. `None` si git no está o falla: entonces esos ficheros se miran como si no fueran de git.
fn git_log(repo: &Path, desde: i64, rel: &[String]) -> Option<Log> {
    let mut c = std::process::Command::new("git");
    // Un día de margen en `--since` (que entiende fechas a su manera); el corte exacto va después.
    c.args(["-c", "core.quotepath=off", "-C"])
        .arg(repo)
        .args(["log", "--no-renames", "--no-merges", "--format=%x00%H %ct", "--numstat"])
        .arg(format!("--since={}", desde - 86400))
        .arg("--");
    let magia = if cfg!(any(windows, target_os = "macos")) { ":(literal,icase)" } else { ":(literal)" };
    for r in rel {
        c.arg(format!("{magia}{r}"));
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let salida = con_tope(c, ESPERA_GIT)?;
    let mut r: Log = HashMap::new();
    for bloque in String::from_utf8_lossy(&salida).split('\0').skip(1) {
        let mut lineas = bloque.lines();
        let Some((hash, cuando)) = lineas.next().and_then(|l| l.split_once(' ')) else { continue };
        let Ok(cuando) = cuando.trim().parse::<i64>() else { continue };
        for l in lineas {
            let mut p = l.splitn(3, '\t');
            let (Some(a), Some(q), Some(ruta)) = (p.next(), p.next(), p.next()) else { continue };
            let n = a.parse::<usize>().unwrap_or(0) + q.parse::<usize>().unwrap_or(0);
            r.entry(crate::proyecto::comparable(ruta)).or_default().push((hash.to_string(), cuando, n));
        }
    }
    Some(r)
}

/// Lo que escribe una orden (con la salida ya redirigida a una tubería) si acaba bien antes de `tope`;
/// si tarda más, se para (ese proceso, el nuestro, por su handle) y `None`: un repositorio enorme, un
/// disco de red o un `git` colgado no dejan esperando la revisión.
fn con_tope(mut c: std::process::Command, tope: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut hijo = c.spawn().ok()?;
    let mut tubo = hijo.stdout.take()?;
    // La salida se lee aparte: si nadie la lee, git se queda esperando con la tubería llena.
    let lector = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = tubo.read_to_end(&mut v);
        v
    });
    let fin = std::time::Instant::now() + tope;
    let estado = loop {
        match hijo.try_wait() {
            Ok(Some(e)) => break Some(e),
            Ok(None) if std::time::Instant::now() < fin => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = hijo.kill();
                let _ = hijo.wait();
                break None;
            }
        }
    };
    let salida = lector.join().ok()?;
    estado.filter(|e| e.success()).map(|_| salida)
}

/// Cuántas líneas tiene hoy un fichero (0 si es muy grande o no se puede leer).
fn lineas(p: &Path) -> usize {
    if std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return 0;
    }
    match std::fs::read(p) {
        Ok(b) if !b.is_empty() => b.iter().filter(|c| **c == b'\n').count() + usize::from(*b.last().unwrap_or(&b'\n') != b'\n'),
        _ => 0,
    }
}

/// Lo que se necesita de una memoria: su id, desde cuándo se mira y, si hay que calcularla, ella.
struct Pendiente {
    id: String,
    desde: String,
    m: Memoria,
    archivos: Vec<(String, PathBuf)>,
}

/// La memoria cuyas rutas se miran y desde cuándo: su último cambio o el último «está bien así» de este
/// aviso, lo más nuevo.
fn desde(m_updated: &str, ok: Option<&String>) -> String {
    match ok {
        Some(at) if at.as_str() > m_updated => at.clone(),
        _ => m_updated.to_string(),
    }
}

/// Lo último calculado: el día y `id → (desde, detalle)`.
fn leer_cache(db: &Connection) -> Result<(String, Guardado)> {
    Ok(de_texto(crudo(db)?.as_deref()))
}

/// Lo guardado (`crudo`) como el día y `id → (desde, detalle)`.
fn de_texto(v: Option<&str>) -> (String, Guardado) {
    let mut r = HashMap::new();
    let Some(v) = v else { return (String::new(), r) };
    let mut ls = v.lines();
    let dia = ls.next().unwrap_or("").to_string();
    for l in ls {
        let mut p = l.splitn(3, '\t');
        if let (Some(id), Some(d), Some(det)) = (p.next(), p.next(), p.next()) {
            r.insert(id.to_string(), (d.to_string(), det.to_string()));
        }
    }
    (dia, r)
}

fn guardar_cache(db: &Connection, hoy: &str, c: &Guardado) -> Result<()> {
    let mut v = hoy.to_string();
    let mut ids: Vec<&String> = c.keys().collect();
    ids.sort();
    for id in ids {
        let (d, det) = &c[id];
        v.push('\n');
        v.push_str(&format!("{id}\t{d}\t{}", det.replace(['\n', '\t'], " ")));
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [CLAVE, &v])?;
    Ok(())
}

fn hoy(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT date('now', 'localtime')", [], |f| f.get(0))?)
}

/// Para el aviso al empezar la sesión, que tiene que ser rápido: solo lo ya calculado para las memorias
/// que no han cambiado desde entonces (sin leer su contenido, sin git ni disco). Si lo guardado es de
/// otro día, se recalcula todo en segundo plano y lo verá la siguiente sesión. Si nunca se ha calculado,
/// nada: la primera revisión (`omoi review`, `context`, la pestaña Revisar) lo calcula.
pub fn de_ambito(db: &Connection, ambito: &Ambito) -> Result<HashMap<String, String>> {
    let mut q = db.prepare(&format!("SELECT m.id, m.updated_at FROM memory m WHERE {}", ambito.sql()))?;
    let filas: Vec<(String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let (r, dia) = de_lo_guardado(db, filas, |_| {})?;
    refrescar_aparte(db, &dia)?;
    Ok(r)
}

/// Lo guardado para `filas` (`id`, último cambio) cuyo «desde» no ha cambiado, y de qué día es. Las que
/// no tienen nada guardado van a `falta`.
fn de_lo_guardado(db: &Connection, filas: Vec<(String, String)>, mut falta: impl FnMut(String)) -> Result<(HashMap<String, String>, String)> {
    let (dia, cache) = leer_cache(db)?;
    let oks = crate::veredictos::oks(db, false)?;
    let mut r = HashMap::new();
    for (id, updated) in filas {
        let d = desde(&updated, oks.get(&(id.clone(), DERIVA.to_string())));
        match cache.get(&id) {
            Some((dc, det)) if *dc == d => {
                if !det.is_empty() {
                    r.insert(id, det.clone());
                }
            }
            _ => falta(id),
        }
    }
    Ok((r, dia))
}

/// Si lo guardado es de otro día (`dia`), lanza `omoi review --refresh-drift` sobre esta base, una vez
/// al día y solo si quien corre es `omoi` (desde un test, el ejecutable es el de las pruebas y
/// entendería otra cosa).
fn refrescar_aparte(db: &Connection, dia: &str) -> Result<()> {
    let hoy = hoy(db)?;
    if dia.is_empty() || dia == hoy {
        return Ok(());
    }
    lanzar_hoy(db, &hoy)
}

/// Lanza `omoi review --refresh-drift` aparte, si no se ha lanzado ya hoy.
fn lanzar_hoy(db: &Connection, hoy: &str) -> Result<()> {
    let lanzado: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [CLAVE_LANZADO], |f| f.get(0)).optional()?;
    if lanzado.as_deref() == Some(hoy) {
        return Ok(());
    }
    let es_omoi = std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| s.to_string_lossy().to_lowercase())).is_some_and(|s| s == "omoi");
    if let (true, Some(ruta)) = (es_omoi, db.path().filter(|p| !p.is_empty())) {
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", [CLAVE_LANZADO, hoy])?;
        crate::sync::lanzar(&["review", "--refresh-drift"], Some(Path::new(ruta)));
    }
    Ok(())
}

/// Calcula de nuevo la deriva de todas las memorias y la guarda para hoy (lo que corre en segundo plano).
pub fn refrescar(db: &Connection) -> Result<usize> {
    let todas = crate::memoria::recientes(db, &Ambito::Todos, None, usize::MAX >> 1)?;
    let oks = crate::veredictos::oks(db, false)?;
    let r = calcular(db, todas.iter().map(|m| Pendiente::de(m, &oks)).collect())?;
    guardar_cache(db, &hoy(db)?, &r)?;
    Ok(r.values().filter(|(_, det)| !det.is_empty()).count())
}

/// Las memorias con deriva por código entre `memorias`: `id → detalle`. Es lo que usa la revisión
/// entera. Lo guardado vale mientras la memoria no cambie, aunque sea de otro día: entonces se deja
/// recalculando en segundo plano. Solo lo que no tiene nada guardado (memorias nuevas, cambiadas o
/// dadas por buenas, o todas la primera vez) se calcula aquí, y casi siempre basta con la fecha de sus
/// ficheros.
pub fn de(db: &Connection, memorias: &[Memoria]) -> Result<HashMap<String, String>> {
    let por_id: HashMap<&str, &Memoria> = memorias.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut faltan = Vec::new();
    // Lo guardado tal cual se lee: se escribe encima solo si nadie lo ha cambiado mientras (ver abajo).
    let crudo_antes = crudo(db)?;
    let (mut r, dia) = de_lo_guardado(db, memorias.iter().map(|m| (m.id.clone(), m.updated_at.clone())).collect(), |id| faltan.push(id))?;
    refrescar_aparte(db, &dia)?;
    if faltan.is_empty() {
        return Ok(r);
    }
    let oks = crate::veredictos::oks(db, false)?;
    let pendientes = faltan.iter().filter_map(|id| por_id.get(id.as_str())).map(|m| Pendiente::de(m, &oks)).collect();
    let nuevos = calcular(db, pendientes)?;
    let (_, mut cache) = de_texto(crudo_antes.as_deref());
    for (id, (d, det)) in nuevos {
        if !det.is_empty() {
            r.insert(id.clone(), det.clone());
        }
        cache.insert(id, (d, det));
    }
    // Lo nuevo se apunta con el día de lo que había (o hoy, si no había nada): así lo viejo sigue
    // contando como viejo hasta que se recalcule entero.
    // Compare-and-set: mientras se calculaba, `refrescar` (en otro proceso) puede haber guardado un
    // resultado entero más nuevo; pisarlo con lo de antes más esto lo perdería. Si lo guardado ya no es
    // lo que se leyó, no se escribe (lo calculado vale para esta respuesta, y la siguiente lo
    // recalculará si hace falta). Es una caché: si no se puede escribir, no pasa nada.
    let hoy = hoy(db)?;
    let _ = guardar_si_no_cambio(db, crudo_antes.as_deref(), if dia.is_empty() { &hoy } else { &dia }, &cache);
    Ok(r)
}

/// Como [`de`], para quien no puede esperar (la herramienta `context`, al empezar una sesión): si aún
/// no se ha calculado nunca (lo de todas, con git, puede tardar segundos), no se calcula aquí: se lanza
/// aparte y lo verá la siguiente. Si ya hay algo guardado, lo que falte (memorias nuevas o cambiadas,
/// casi siempre solo la fecha de sus ficheros), como [`de`].
pub fn de_sin_esperar(db: &Connection, memorias: &[Memoria]) -> Result<HashMap<String, String>> {
    if crudo(db)?.is_none() {
        lanzar_hoy(db, &hoy(db)?)?;
        return Ok(HashMap::new());
    }
    de(db, memorias)
}

/// Lo guardado en `meta`, tal cual.
fn crudo(db: &Connection) -> Result<Option<String>> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = ?1", [CLAVE], |f| f.get(0)).optional()?)
}

/// Guarda la caché solo si lo guardado sigue siendo `antes`, en una transacción `IMMEDIATE` (nadie
/// escribe entre mirarlo y escribirlo). `false` si había cambiado.
fn guardar_si_no_cambio(db: &Connection, antes: Option<&str>, dia: &str, c: &Guardado) -> Result<bool> {
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    if crudo(&tx)?.as_deref() != antes {
        return Ok(false);
    }
    guardar_cache(&tx, dia, c)?;
    tx.commit()?;
    Ok(true)
}

impl Pendiente {
    fn de(m: &Memoria, oks: &HashMap<(String, String), String>) -> Pendiente {
        Pendiente { id: m.id.clone(), desde: desde(&m.updated_at, oks.get(&(m.id.clone(), DERIVA.to_string()))), m: m.clone(), archivos: Vec::new() }
    }
}

/// `id → (desde, detalle)` de cada pendiente (detalle vacío si no deriva).
fn calcular(db: &Connection, mut pendientes: Vec<Pendiente>) -> Result<Guardado> {
    let detalles = if pendientes.is_empty() { HashMap::new() } else { calcular_pendientes(db, &mut pendientes)? };
    Ok(pendientes.into_iter().map(|p| (p.id.clone(), (p.desde, detalles.get(&p.id).cloned().unwrap_or_default()))).collect())
}

/// El detalle de las que derivan entre `pendientes`.
fn calcular_pendientes(db: &Connection, pendientes: &mut [Pendiente]) -> Result<HashMap<String, String>> {
    // Las que tienen algún fichero cambiado desde entonces (más la gracia); las demás no tienen nada.
    let mut repos: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    // repo → (desde más antiguo, rutas relativas)
    let mut por_repo: HashMap<PathBuf, (i64, BTreeSet<String>)> = HashMap::new();
    let mut fuera_de_git = false;
    let mut vivas: Vec<usize> = Vec::new();
    for (i, p) in pendientes.iter_mut().enumerate() {
        let Some(corte) = segundos(&p.desde) else { continue };
        let corte = corte + GRACIA * 3600;
        p.archivos = ficheros(&p.m);
        let cambiados: Vec<&PathBuf> = p.archivos.iter().map(|(_, d)| d).filter(|d| modificado(d).is_some_and(|t| t > corte)).collect();
        if cambiados.is_empty() {
            continue;
        }
        vivas.push(i);
        for d in cambiados {
            match repo_de(d, &mut repos) {
                Some(repo) => {
                    let rel = d.strip_prefix(&repo).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                    if !rel.is_empty() {
                        let e = por_repo.entry(repo).or_insert((corte, BTreeSet::new()));
                        e.0 = e.0.min(corte);
                        e.1.insert(rel);
                    }
                }
                None => fuera_de_git = true,
            }
        }
    }
    if vivas.is_empty() {
        return Ok(HashMap::new());
    }
    // Una orden por repositorio, en paralelo.
    let trabajos: Vec<(PathBuf, i64, Vec<String>)> = por_repo.into_iter().map(|(r, (d, rel))| (r, d, rel.into_iter().collect())).collect();
    let mut logs: HashMap<PathBuf, Option<Log>> = HashMap::new();
    for tanda in trabajos.chunks(HILOS) {
        let hechos: Vec<(PathBuf, Option<Log>)> = std::thread::scope(|s| {
            let hs: Vec<_> = tanda.iter().map(|(r, d, rel)| s.spawn(move || (r.clone(), git_log(r, *d, rel)))).collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        logs.extend(hechos);
    }
    // El diario, solo si algún fichero cambiado no es de git (o git no respondió).
    let fuera_de_git = fuera_de_git || logs.values().any(Option::is_none);
    let diario = if fuera_de_git { sesiones(db)? } else { HashMap::new() };

    let mut r = HashMap::new();
    for i in vivas {
        let p = &pendientes[i];
        let corte = segundos(&p.desde).unwrap_or(0) + GRACIA * 3600;
        let (mut commits, mut cambiadas, mut total) = (BTreeSet::new(), 0usize, 0usize);
        let mut sesiones_de: BTreeSet<&str> = BTreeSet::new();
        let mut nombres: Vec<&str> = Vec::new();
        for (mostrado, d) in &p.archivos {
            // `Some(Some(..))`: su repositorio respondió; `Some(None)`: git falló; `None`: o no es de git
            // o en su repositorio no cambió nada que mirar.
            let repo = repo_de(d, &mut repos);
            let log = repo.as_ref().and_then(|repo| {
                let rel = d.strip_prefix(repo).ok()?.to_string_lossy().replace('\\', "/");
                logs.get(repo).map(|l| l.as_ref().map(|l| l.get(&crate::proyecto::comparable(&rel)).cloned().unwrap_or_default()))
            });
            match log {
                Some(Some(cambios)) => {
                    total += lineas(d);
                    let mut suyo = false;
                    for (h, _, n) in cambios.iter().filter(|(_, t, _)| *t > corte) {
                        commits.insert(h.clone());
                        cambiadas += n;
                        suyo = true;
                    }
                    if suyo {
                        nombres.push(mostrado);
                    }
                }
                // De git pero sin cambios desde entonces (no estaba entre los cambiados): mide igual.
                None if repo.is_some() => total += lineas(d),
                _ => {
                    let clave = crate::proyecto::comparable(&d.to_string_lossy().replace('\\', "/"));
                    let mut suyo = false;
                    for (sesion, ultima, guardadas) in diario.get(&clave).map(Vec::as_slice).unwrap_or_default() {
                        if segundos(ultima).is_some_and(|t| t > corte) && !guardadas.contains(&p.id) {
                            sesiones_de.insert(sesion);
                            suyo = true;
                        }
                    }
                    if suyo {
                        nombres.push(mostrado);
                    }
                }
            }
        }
        let parte = cambiadas as f64 / total.max(1) as f64;
        let por_git = cambiadas >= LINEAS_MIN && parte >= PARTE_MIN;
        let por_diario = sesiones_de.len() >= SESIONES_MIN;
        if !(por_git || por_diario) {
            continue;
        }
        let lista = match nombres.as_slice() {
            [] => String::new(),
            [a] => format!("`{a}`"),
            [a, b] => format!("`{a}` and `{b}`"),
            [a, b, resto @ ..] => format!("`{a}`, `{b}` and {} more", resto.len()),
        };
        let dia = p.desde.get(..10).unwrap_or("");
        let mut cifras = Vec::new();
        if por_git {
            let n = commits.len();
            cifras.push(format!("{n} commit{}, {cambiadas} lines changed, {:.0}% of their size", if n == 1 { "" } else { "s" }, parte * 100.0));
        }
        if por_diario || !sesiones_de.is_empty() {
            cifras.push(format!("edited in {} sessions", sesiones_de.len()));
        }
        r.insert(p.id.clone(), format!("{lista} changed a lot since {dia} ({}): is it still true?", cifras.join("; ")));
    }
    Ok(r)
}

/// Las sesiones del diario por fichero (ruta absoluta comparable): (sesión, último momento, ids
/// guardados en ella). Los ficheros relativos lo son a la carpeta del proyecto de la línea.
fn sesiones(db: &Connection) -> Result<PorFichero> {
    let mut r: PorFichero = HashMap::new();
    if !crate::diario::hay_tabla(db)? {
        return Ok(r);
    }
    let mut q = db.prepare(
        "SELECT d.session, COALESCE(NULLIF((SELECT path FROM project WHERE id = d.project_id), ''), d.folder), d.last_at, d.files, d.saved
         FROM diary d WHERE d.files <> '[]'",
    )?;
    let mut filas = q.query([])?;
    while let Some(f) = filas.next()? {
        let (sesion, carpeta, ultima): (String, String, String) = (f.get(0)?, f.get(1)?, f.get(2)?);
        let files: Vec<String> = serde_json::from_str(&f.get::<_, String>(3)?).unwrap_or_default();
        let saved: Vec<String> = serde_json::from_str(&f.get::<_, String>(4)?).unwrap_or_default();
        for x in files {
            let abs = if absoluta(&x) { x.replace('\\', "/") } else { format!("{}/{}", carpeta.replace('\\', "/").trim_end_matches('/'), x.replace('\\', "/")) };
            r.entry(crate::proyecto::comparable(&abs)).or_default().push((sesion.clone(), ultima.clone(), saved.clone()));
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rutas_nombradas() {
        let t = "Está en `src/sync.rs` y en D:\\repo\\a.py, no en main.rs ni en https://x.com/a.html ni en src/; también (tests/x.rs).";
        assert_eq!(nombrados(t), vec!["src/sync.rs", "D:\\repo\\a.py", "tests/x.rs"]);
    }

    /// Las rutas de red no se toman por ficheros ni se miran en el disco (en Windows, mirarlas abre una
    /// conexión SMB: un cuelgue y el hash NTLM al servidor que diga la memoria).
    #[test]
    fn nunca_rutas_de_red() {
        let t = "Carga //cdn.example.com/x.js y \\\\host\\share\\a.py, o \\\\?\\C:\\x\\b.rs y \\\\.\\pipe\\c.rs; pero sí src/d.rs.";
        assert_eq!(nombrados(t), vec!["src/d.rs"]);
        let m = Memoria { project: "/p".into(), ..Default::default() };
        for r in ["//cdn.example.com/x.js", "\\\\host\\share\\a.py", "\\\\?\\C:\\x\\b.rs", "\\\\.\\pipe\\c.rs", "/\\host\\x.rs"] {
            assert_eq!(en_disco(&m, r), None, "{r}");
        }
        // Relativa a un proyecto que está en un recurso de red: tampoco.
        let remota = Memoria { project: "\\\\host\\share\\repo".into(), ..Default::default() };
        assert_eq!(en_disco(&remota, "src/a.rs"), None);
        assert!(en_disco(&m, "src/a.rs").is_some());
    }

    /// Un `git log` que no acaba se para al llegar al tope, sin esperar a que termine.
    #[test]
    fn git_con_tope() {
        let mut c = if cfg!(windows) {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 20"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("20");
            c
        };
        c.stdout(std::process::Stdio::piped()).stdin(std::process::Stdio::null());
        let t = std::time::Instant::now();
        assert_eq!(con_tope(c, std::time::Duration::from_millis(300)), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    /// Lo calculado no pisa lo que otro proceso guardó mientras (`refrescar`): solo se escribe si lo
    /// guardado sigue siendo lo que se leyó.
    #[test]
    fn la_cache_no_pisa_lo_mas_nuevo() {
        let t = crate::comun::Temporal::nueva("deriva-cache");
        let db = crate::db::abrir(&t.join("o.db")).unwrap();
        let mut viejo = Guardado::new();
        viejo.insert("A".into(), ("2026-01-01".into(), String::new()));
        guardar_cache(&db, "2026-11-01", &viejo).unwrap();
        let leido = crudo(&db).unwrap();
        // Mientras, `refrescar` guarda el de hoy.
        let mut nuevo = Guardado::new();
        nuevo.insert("A".into(), ("2026-01-01".into(), "cambió mucho".into()));
        guardar_cache(&db, "2026-11-08", &nuevo).unwrap();
        let ahora = crudo(&db).unwrap();
        assert!(!guardar_si_no_cambio(&db, leido.as_deref(), "2026-11-01", &viejo).unwrap());
        assert_eq!(crudo(&db).unwrap(), ahora, "lo más nuevo se queda");
        assert!(guardar_si_no_cambio(&db, ahora.as_deref(), "2026-11-08", &viejo).unwrap());
        // Y `context` no lo calcula la primera vez: sin nada guardado, no espera.
        let vacia = crate::db::abrir(&t.join("v.db")).unwrap();
        assert!(de_sin_esperar(&vacia, &[]).unwrap().is_empty());
        assert_eq!(crudo(&vacia).unwrap(), None);
    }

    #[test]
    fn fechas_en_segundos() {
        assert_eq!(segundos("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(segundos("2026-10-30T18:45:00.570Z").unwrap() - segundos("2026-10-30T12:45:00Z").unwrap(), 6 * 3600);
    }
}
