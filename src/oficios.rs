//! Oficios (stacks): android, web, wordpress, rust… Lo que pide Kumi, la megafactoría (PLAN §9.7b):
//! cada cadena de montaje tiene su memoria de oficio. «No matar el emulador con taskkill» le sirve a
//! cualquier proyecto Android, de cualquier cliente, y no llega a la factoría web.
//!
//! Un nivel entre el proyecto y lo global, sin tocar el esquema 10:
//! - **El oficio de una memoria**: una memoria global (sin proyecto ni espacio) con la etiqueta
//!   reservada `trade:<oficio>` (puede llevar varias). Viaja, se exporta y se importa como cualquier
//!   etiqueta, así que una 1.0 la entiende entera (la ve como una global más; ver el CHANGELOG). En una
//!   memoria de proyecto o de espacio la etiqueta no significa nada: lo de un cliente nunca se vuelve de
//!   oficio por llevarla.
//! - **El oficio de un proyecto**: lo que se detecta en sus ficheros (barato y determinista: la raíz y
//!   un nivel de subcarpetas), más lo que la persona corrige a mano (`omoi trades add|remove|set`), en
//!   dos tablas locales que no viajan (`project_trade`, `project_trade_seen`): cada equipo lo detecta
//!   en su carpeta, y una corrección es de quien la hace. Como las de `db::LOCALES`, una 1.0 las ignora.
//!
//! Desde un proyecto se ven las memorias de los oficios que tiene; desde un espacio, las de los oficios
//! de sus proyectos; desde la carpeta del usuario, todas (`Ambito::sql`, con [`ocultas`]).
//!
//! Lo más importante es que por aquí no salga nada de un cliente: al guardar una memoria de oficio,
//! [`fugas`] avisa si nombra proyectos, espacios (clientes), rutas absolutas, repositorios o dominios
//! que solo salen en memorias de un cliente (en el texto, las rutas, las etiquetas, la clave y los
//! datos), y no se guarda salvo los falsos positivos que deje pasar el permiso `allow_leaks` (nunca una
//! ruta ni un repositorio). Promover a un oficio lo de un cliente crea una memoria nueva
//! ([`crear_de_oficio`]): su historial y sus veredictos se quedan en el cliente. Y por la sincronización
//! solo van por el repositorio general (son globales: `sync::va_al_repo` ya no las deja ir a uno de espacio),
//! y un repositorio de cliente no las trae (`sync`, con [`lleva_oficio`]).

use crate::memoria::{Ambito, Memoria};
use crate::tr;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// La etiqueta de una memoria de oficio es este prefijo y el oficio: `trade:android`.
pub const PREFIJO: &str = "trade:";

/// Los oficios que se detectan solos. Los nombres son los de las factorías de Kumi (`android`, `web`,
/// `rust`, `plugin-claude`, `python`, `escritorio-node`, `alexa`), para que no haga falta una tabla de
/// equivalencias; `seo-geo` no se deduce de los ficheros con fiabilidad y se pone a mano, como cualquier
/// otro (`kotlin`…).
pub const DETECTABLES: &[&str] = &[
    "3d-print",
    "alexa",
    "android",
    "browser-extension",
    "cpp",
    "dart",
    "deno",
    "docker",
    "dotnet",
    "escritorio-node",
    "flutter",
    "go",
    "godot",
    "ios",
    "jvm",
    "node",
    "php",
    "plugin-claude",
    "python",
    "react-native",
    "ruby",
    "rust",
    "swift",
    "terraform",
    "unity",
    "web",
    "wordpress",
];

const TABLAS: &str = "
CREATE TABLE IF NOT EXISTS project_trade (
  project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  trade      TEXT NOT NULL,
  how        TEXT NOT NULL,
  PRIMARY KEY (project_id, trade)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS project_trade_seen (
  project_id INTEGER PRIMARY KEY REFERENCES project(id) ON DELETE CASCADE,
  seen_at    TEXT NOT NULL,
  fixed      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS memory_oficio ON memory(n) WHERE project_id = 0 AND space_id IS NULL AND instr(tags, '\"trade:') > 0;
";

/// `project_trade.how`: detectado en los ficheros, añadido a mano o quitado a mano (gana a lo detectado).
const AUTO: &str = "auto";
const ANADIDO: &str = "add";
const QUITADO: &str = "off";

/// Deja las tablas de los oficios y el índice parcial de las memorias de oficio (`memory_oficio`: solo
/// las globales con la etiqueta, para listarlas sin recorrer la tabla). Lo normal es que ya estén.
pub fn crear_tabla(db: &Connection) -> Result<()> {
    let hay: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name IN ('project_trade', 'project_trade_seen', 'memory_oficio')",
        [],
        |f| f.get(0),
    )?;
    if hay < 3 {
        db.execute_batch(TABLAS)?;
    }
    Ok(())
}

// --- Memorias de oficio ---------------------------------------------------------------------------

/// La etiqueta de un oficio.
pub fn etiqueta(oficio: &str) -> String {
    format!("{PREFIJO}{oficio}")
}

/// Los oficios de unas etiquetas (`trade:<oficio>`).
pub fn de_etiquetas(tags: &[String]) -> Vec<String> {
    tags.iter().filter_map(|t| t.strip_prefix(PREFIJO)).filter(|t| !t.is_empty()).map(String::from).collect()
}

/// Si unas etiquetas llevan algún oficio.
pub fn lleva_oficio(tags: &[String]) -> bool {
    tags.iter().any(|t| t.starts_with(PREFIJO))
}

/// Las etiquetas sin las de oficio.
pub fn sin_oficio(tags: Vec<String>) -> Vec<String> {
    tags.into_iter().filter(|t| !t.starts_with(PREFIJO) && !t.starts_with(PARA)).collect()
}

/// Para quién es una memoria de oficio, si no es para todos: `trade-for:client` (solo los proyectos de
/// un cliente, los que están en un espacio) o `trade-for:personal` (solo los que no están en ninguno).
/// Sin la etiqueta, para los dos. No hace falta nada nuevo en el proyecto: el espacio ya lo dice.
pub const PARA: &str = "trade-for:";
pub const CLIENTE: &str = "client";
pub const PERSONAL: &str = "personal";

/// `for` de `remember` y `update`: `client`, `personal` o nada (`""`, `both`: para los dos).
pub fn para(texto: &str) -> Result<Option<String>> {
    let t = texto.trim().to_lowercase();
    Ok(match t.as_str() {
        "" | "both" | "all" | "any" => None,
        "client" | "clients" | "cliente" => Some(CLIENTE.to_string()),
        "personal" | "own" | "propio" => Some(PERSONAL.to_string()),
        _ => anyhow::bail!(tr!(
            "«{}» is not valid for `for`: client (projects in a space), personal (projects in no space) or both",
            "«{}» no vale para `for`: client (proyectos en un espacio), personal (proyectos sin espacio) o both",
            texto.trim()
        )),
    })
}

/// Para quién es una memoria de oficio por sus etiquetas (`None`: para todos).
pub fn para_de(tags: &[String]) -> Option<String> {
    tags.iter().find_map(|t| t.strip_prefix(PARA)).filter(|p| *p == CLIENTE || *p == PERSONAL).map(String::from)
}

/// Las etiquetas de una memoria de oficio: las suyas (sin las de oficio), sus oficios y para quién.
pub fn etiquetas_de_oficio(tags: Vec<String>, oficios: &[String], para: Option<&str>) -> Vec<String> {
    sin_oficio(tags).into_iter().chain(oficios.iter().map(|o| etiqueta(o))).chain(para.map(|p| format!("{PARA}{p}"))).collect()
}

/// Los oficios de una memoria: los de sus etiquetas si es global; ninguno si es de un proyecto o de un
/// espacio (ahí la etiqueta no cuenta).
pub fn de_memoria(m: &Memoria) -> Vec<String> {
    if m.global() { de_etiquetas(&m.tags) } else { Vec::new() }
}

/// Un nombre de oficio válido: minúsculas, letras, cifras, `-`, `+`, `.` y `#` (`c#`, `c++`), hasta 32.
pub fn nombre(texto: &str) -> Result<String> {
    let n = texto.trim().trim_start_matches(PREFIJO).trim().to_lowercase();
    ensure!(
        !n.is_empty()
            && n.chars().count() <= 32
            && n.chars().next().is_some_and(|c| c.is_alphanumeric())
            && n.chars().all(|c| c.is_alphanumeric() || "-+.#".contains(c)),
        tr!(
            "«{}» is not a valid trade: letters, digits, -, +, . and #, up to 32 (android, web, wordpress…)",
            "«{}» no vale como oficio: letras, cifras, -, +, . y #, hasta 32 (android, web, wordpress…)",
            texto.trim()
        )
    );
    Ok(n)
}

/// Los oficios de un texto como «android, web» (separados por comas o espacios), validados y sin repetir.
pub fn lista(texto: &str) -> Result<Vec<String>> {
    let mut v: Vec<String> = Vec::new();
    for t in texto.split([',', ' ']).filter(|t| !t.trim().is_empty()) {
        let n = nombre(t)?;
        if !v.contains(&n) {
            v.push(n);
        }
    }
    Ok(v)
}

/// Los proyectos cuyos oficios cuentan desde un ámbito, como lista SQL (`None`: desde todo, se ven todas).
fn proyectos_sql(a: &Ambito) -> Option<String> {
    match a {
        Ambito::Todos => None,
        Ambito::Proyecto { id, .. } => Some(id.to_string()),
        Ambito::Espacio(Some(s)) => Some(format!("SELECT id FROM project WHERE space_id = {s}")),
        Ambito::Espacio(None) => Some("SELECT id FROM project WHERE space_id IS NULL AND id <> 0".to_string()),
    }
}

/// Las memorias de oficio (por `n`) que no se ven desde `a`: las de oficios que no tiene ningún
/// proyecto del ámbito (de cliente si es `trade-for:client`, personal si es `trade-for:personal`). `None` desde todo. Es una subconsulta sin correlación: SQLite la hace una vez
/// por consulta, por el índice parcial `memory_oficio` (que `crear_tabla` deja siempre; sin `INDEXED BY`,
/// SQLite prefería `memory_scope` y leía todas las globales), y luego mira cada global por su `n`.
pub(crate) fn ocultas(a: &Ambito) -> Option<String> {
    let ps = proyectos_sql(a)?;
    Some(format!(
        "SELECT o.n FROM memory o INDEXED BY memory_oficio WHERE o.project_id = 0 AND o.space_id IS NULL AND instr(o.tags, '\"trade:') > 0
           AND NOT EXISTS (SELECT 1 FROM json_each(o.tags) t JOIN project_trade pt ON pt.trade = substr(t.value, 7) JOIN project p ON p.id = pt.project_id
                           WHERE t.value LIKE 'trade:%' AND pt.how <> 'off' AND pt.project_id IN ({ps})
                             AND (instr(o.tags, '\"trade-for:client\"') = 0 OR p.space_id IS NOT NULL)
                             AND (instr(o.tags, '\"trade-for:personal\"') = 0 OR p.space_id IS NULL))"
    ))
}

/// Si una memoria (por su id) es de oficio y no se ve desde `a`.
pub(crate) fn oculta(db: &Connection, a: &Ambito, id: &str) -> Result<bool> {
    match ocultas(a) {
        None => Ok(false),
        Some(q) => Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE m.id = ?1 AND m.n IN ({q}))"), [id], |f| f.get(0))?),
    }
}

/// Cuántas memorias de oficio no se ven desde `a`.
pub(crate) fn cuantas_ocultas(db: &Connection, a: &Ambito) -> Result<i64> {
    match ocultas(a) {
        None => Ok(0),
        Some(q) => Ok(db.query_row(&format!("SELECT count(*) FROM ({q})"), [], |f| f.get(0))?),
    }
}

// --- Oficios de un proyecto -----------------------------------------------------------------------

/// Los oficios de un proyecto (detectados o puestos a mano, sin los quitados), por orden alfabético.
pub fn del_proyecto(db: &Connection, proyecto: i64) -> Result<Vec<String>> {
    let mut q = db.prepare_cached("SELECT trade FROM project_trade WHERE project_id = ?1 AND how <> 'off' ORDER BY trade")?;
    Ok(q.query_map([proyecto], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Los oficios de todos los proyectos, por id.
pub fn de_proyectos(db: &Connection) -> Result<HashMap<i64, Vec<String>>> {
    let mut q = db.prepare("SELECT project_id, trade FROM project_trade WHERE how <> 'off' ORDER BY project_id, trade")?;
    let mut m: HashMap<i64, Vec<String>> = HashMap::new();
    for f in q.query_map([], |f| Ok((f.get::<_, i64>(0)?, f.get::<_, String>(1)?)))? {
        let (p, t) = f?;
        m.entry(p).or_default().push(t);
    }
    Ok(m)
}

/// Lo que se sabe de los oficios de un proyecto, para `omoi trades`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Estado {
    /// Los que cuentan.
    pub trades: Vec<String>,
    /// Detectados en sus ficheros.
    pub detected: Vec<String>,
    /// Puestos a mano.
    pub added: Vec<String>,
    /// Quitados a mano (aunque se detecten).
    pub removed: Vec<String>,
    /// Con `omoi trades set`: la detección ya no añade nada.
    pub fixed: bool,
    /// Cuándo se miraron sus ficheros por última vez.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<String>,
}

pub fn estado(db: &Connection, proyecto: i64) -> Result<Estado> {
    let mut e = Estado::default();
    let mut q = db.prepare("SELECT trade, how FROM project_trade WHERE project_id = ?1 ORDER BY trade")?;
    for f in q.query_map([proyecto], |f| Ok((f.get::<_, String>(0)?, f.get::<_, String>(1)?)))? {
        let (t, how) = f?;
        match how.as_str() {
            AUTO => e.detected.push(t),
            QUITADO => e.removed.push(t),
            _ => e.added.push(t),
        }
    }
    let visto: Option<(String, bool)> =
        db.query_row("SELECT seen_at, fixed FROM project_trade_seen WHERE project_id = ?1", [proyecto], |f| Ok((f.get(0)?, f.get(1)?))).optional()?;
    if let Some((s, fijo)) = visto {
        e.seen_at = Some(s);
        e.fixed = fijo;
    }
    e.trades = e.detected.iter().chain(&e.added).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    Ok(e)
}

/// Escribe en un `SAVEPOINT` (vale dentro y fuera de una transacción).
fn en_bloque<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    db.execute_batch("SAVEPOINT oficios")?;
    match f() {
        Ok(v) => {
            db.execute_batch("RELEASE oficios")?;
            Ok(v)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK TO oficios; RELEASE oficios");
            Err(e)
        }
    }
}

fn apuntar_visto(db: &Connection, proyecto: i64, fijo: Option<bool>) -> Result<()> {
    db.execute(
        "INSERT INTO project_trade_seen(project_id, seen_at, fixed) VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), COALESCE(?2, 0))
         ON CONFLICT(project_id) DO UPDATE SET seen_at = excluded.seen_at, fixed = COALESCE(?2, fixed)",
        params![proyecto, fijo],
    )?;
    Ok(())
}

/// Mira los ficheros del proyecto (en `ruta`) y deja apuntado lo que detecta, salvo que se fijaran a
/// mano (`set`). Solo escribe si algo cambia o es la primera vez. Devuelve los oficios que cuentan.
/// Una carpeta que no existe aquí no cambia nada.
pub fn al_dia(db: &Connection, proyecto: i64, ruta: &str) -> Result<Vec<String>> {
    if proyecto <= 0 || ruta.is_empty() || !Path::new(ruta).is_dir() {
        return del_proyecto(db, proyecto);
    }
    let e = estado(db, proyecto)?;
    if e.fixed {
        return Ok(e.trades);
    }
    let detectados = detectar(Path::new(ruta));
    if e.seen_at.is_none() || detectados != e.detected {
        en_bloque(db, || {
            db.execute("DELETE FROM project_trade WHERE project_id = ?1 AND how = 'auto'", [proyecto])?;
            for t in &detectados {
                db.execute("INSERT OR IGNORE INTO project_trade(project_id, trade, how) VALUES (?1, ?2, 'auto')", params![proyecto, t])?;
            }
            apuntar_visto(db, proyecto, None)
        })?;
    }
    del_proyecto(db, proyecto)
}

/// Como `al_dia`, pero solo si nunca se miró (para los sitios que no deben mirar el disco cada vez).
pub fn al_dia_si_falta(db: &Connection, proyecto: i64, ruta: &str) -> Result<Vec<String>> {
    let visto: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM project_trade_seen WHERE project_id = ?1)", [proyecto], |f| f.get(0))?;
    if visto { del_proyecto(db, proyecto) } else { al_dia(db, proyecto, ruta) }
}

/// Antes de mirar desde una carpeta (MCP, CLI): sus oficios, si nunca se miraron. Un proyecto que aún
/// no tiene nada queda apuntado si tiene alguno (las memorias de su oficio ya le sirven), y uno sin
/// oficios no se apunta. Desde una carpeta general, nada.
pub fn preparar(db: &Connection, ruta: &str) -> Result<()> {
    if ruta.is_empty() || crate::proyecto::es_general(ruta) {
        return Ok(());
    }
    match crate::proyecto::resolver(db, ruta)? {
        Some((id, _)) if id > 0 => {
            al_dia_si_falta(db, id, ruta)?;
        }
        Some(_) => {}
        None => {
            if !detectar(Path::new(ruta)).is_empty() {
                let id = crate::proyecto::registrar(db, ruta)?;
                al_dia(db, id, ruta)?;
            }
        }
    }
    Ok(())
}

/// `omoi trades add`: el proyecto tiene este oficio aunque no se detecte.
pub fn anadir(db: &Connection, proyecto: i64, oficios: &[String]) -> Result<()> {
    en_bloque(db, || {
        for t in oficios {
            db.execute(
                "INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3) ON CONFLICT(project_id, trade) DO UPDATE SET how = ?3",
                params![proyecto, t, ANADIDO],
            )?;
        }
        apuntar_visto(db, proyecto, None)
    })
}

/// `omoi trades remove`: el proyecto no tiene este oficio aunque se detecte.
pub fn quitar(db: &Connection, proyecto: i64, oficios: &[String]) -> Result<()> {
    en_bloque(db, || {
        for t in oficios {
            db.execute(
                "INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3) ON CONFLICT(project_id, trade) DO UPDATE SET how = ?3",
                params![proyecto, t, QUITADO],
            )?;
        }
        apuntar_visto(db, proyecto, None)
    })
}

/// `omoi trades set`: exactamente estos, y la detección deja de añadir.
pub fn fijar(db: &Connection, proyecto: i64, oficios: &[String]) -> Result<()> {
    en_bloque(db, || {
        db.execute("DELETE FROM project_trade WHERE project_id = ?1", [proyecto])?;
        for t in oficios {
            db.execute("INSERT INTO project_trade(project_id, trade, how) VALUES (?1, ?2, ?3)", params![proyecto, t, ANADIDO])?;
        }
        apuntar_visto(db, proyecto, Some(true))
    })
}

/// `omoi trades auto`: se olvida lo puesto a mano y se vuelve a detectar.
pub fn automatico(db: &Connection, proyecto: i64, ruta: &str) -> Result<Vec<String>> {
    en_bloque(db, || {
        db.execute("DELETE FROM project_trade WHERE project_id = ?1", [proyecto])?;
        db.execute("DELETE FROM project_trade_seen WHERE project_id = ?1", [proyecto])?;
        Ok(())
    })?;
    al_dia(db, proyecto, ruta)
}

/// Cuántas memorias de cada oficio hay.
pub fn memorias_por_oficio(db: &Connection) -> Result<HashMap<String, i64>> {
    let mut q = db.prepare(
        "SELECT substr(t.value, 7), count(*) FROM memory o, json_each(o.tags) t
         WHERE o.project_id = 0 AND o.space_id IS NULL AND instr(o.tags, '\"trade:') > 0 AND t.value LIKE 'trade:%'
         GROUP BY 1",
    )?;
    Ok(q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
}

// --- Detección ------------------------------------------------------------------------------------

/// Subcarpetas de la raíz que se miran como mucho (un monorepo: `app/`, `web/`, `android/`…).
const MAX_SUBCARPETAS: usize = 60;
/// Entradas de una carpeta que se leen como mucho.
const MAX_ENTRADAS: usize = 3000;
/// Bytes que se leen de un fichero para mirar su contenido.
const MAX_BYTES: u64 = 256 * 1024;

/// Subcarpetas que no dicen nada del oficio (dependencias, compilados, cachés).
fn ignorada(nombre: &str) -> bool {
    let l = nombre.to_lowercase();
    l.starts_with('.')
        || matches!(
            l.as_str(),
            "node_modules"
                | "target"
                | "build"
                | "dist"
                | "out"
                | "vendor"
                | "venv"
                | "env"
                | "__pycache__"
                | "bin"
                | "obj"
                | "pods"
                | "deriveddata"
                | "coverage"
                | "tmp"
                | "temp"
                | "cache"
                | "logs"
                | "site-packages"
        )
}

fn leer_carpeta(dir: &Path) -> Vec<(String, bool)> {
    let Ok(it) = std::fs::read_dir(dir) else { return Vec::new() };
    it.flatten()
        .take(MAX_ENTRADAS)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.file_type().map(|t| t.is_dir()).unwrap_or(false)))
        .collect()
}

/// El principio de un fichero, en minúsculas (vacío si no se puede leer).
fn principio(ruta: &Path) -> String {
    use std::io::Read;
    let mut s = Vec::new();
    if let Ok(f) = std::fs::File::open(ruta) {
        let _ = f.take(MAX_BYTES).read_to_end(&mut s);
    }
    String::from_utf8_lossy(&s).to_lowercase()
}

/// Lo que dice un `package.json`: sus dependencias.
fn de_package_json(ruta: &Path, v: &mut BTreeSet<String>) {
    v.insert("node".into());
    // `principio` lo da en minúsculas: las claves van como `devdependencies`.
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&principio(ruta)) else { return };
    let deps: HashSet<String> = ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|k| j.get(k.to_lowercase()).and_then(|d| d.as_object()))
        .flat_map(|d| d.keys().cloned())
        .collect();
    let movil = deps.contains("react-native") || deps.contains("expo");
    if movil {
        v.insert("react-native".into());
    }
    // Una app de escritorio con Electron o Tauri (su interfaz es web, pero no es una web).
    if ["electron", "@tauri-apps/api", "@tauri-apps/cli", "nw", "neutralinojs"].iter().any(|d| deps.contains(*d)) {
        v.insert("escritorio-node".into());
    }
    const WEB: &[&str] = &[
        "astro",
        "next",
        "nuxt",
        "vue",
        "svelte",
        "@sveltejs/kit",
        "@angular/core",
        "solid-js",
        "preact",
        "lit",
        "gatsby",
        "@remix-run/react",
        "vite",
        "vitepress",
        "@docusaurus/core",
        "@11ty/eleventy",
        "ember-source",
        "jquery",
        "alpinejs",
        "htmx.org",
        "@builder.io/qwik",
        "webpack",
        "parcel",
    ];
    // React a secas es web salvo en una app de React Native (que también lo lleva).
    if WEB.iter().any(|w| deps.contains(*w)) || (!movil && (deps.contains("react") || deps.contains("react-dom"))) {
        v.insert("web".into());
    }
}

/// Los oficios de un proyecto por sus ficheros: la raíz y un nivel de subcarpetas (sin dependencias,
/// compilados ni ocultas), sin recorrer el árbol. Determinista: los mismos ficheros dan lo mismo.
pub fn detectar(raiz: &Path) -> Vec<String> {
    let mut v = BTreeSet::new();
    let mut gradle_sin_android = false;
    let entradas = leer_carpeta(raiz);
    mirar(raiz, &entradas, true, &mut v, &mut gradle_sin_android);
    for (nombre, _) in entradas.iter().filter(|(n, d)| *d && !ignorada(n)).take(MAX_SUBCARPETAS) {
        let d = raiz.join(nombre);
        mirar(&d, &leer_carpeta(&d), false, &mut v, &mut gradle_sin_android);
    }
    // Un Gradle sin Android (en ningún sitio) es de la JVM: en un proyecto Android, el `settings.gradle`
    // de la raíz no siempre lo nombra.
    if gradle_sin_android && !v.contains("android") {
        v.insert("jvm".into());
    }
    // Una app de escritorio hecha con tecnología web no es una web (su `vite.config` tampoco lo dice).
    if v.contains("escritorio-node") {
        v.remove("web");
    }
    v.into_iter().collect()
}

fn mirar(dir: &Path, entradas: &[(String, bool)], raiz: bool, v: &mut BTreeSet<String>, gradle_sin_android: &mut bool) {
    let mut aqui: BTreeSet<String> = BTreeSet::new();
    let mut paquetes: BTreeSet<String> = BTreeSet::new();
    let mut pon = |t: &str| {
        aqui.insert(t.to_string());
    };
    let nombres: HashSet<String> = entradas.iter().map(|(n, _)| n.to_lowercase()).collect();
    let mut php_leidos = 0;
    for (nombre, es_dir) in entradas {
        let l = nombre.to_lowercase();
        let ext = l.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if *es_dir {
            match l.as_str() {
                "wp-content" | "wp-includes" => pon("wordpress"),
                "skill-package" => pon("alexa"),
                ".claude-plugin" => pon("plugin-claude"),
                "src-tauri" => pon("escritorio-node"),
                "projectsettings" if nombres.contains("assets") => pon("unity"),
                _ if ext == "xcodeproj" || ext == "xcworkspace" => pon("ios"),
                _ => {}
            }
            continue;
        }
        let ruta = dir.join(nombre);
        match l.as_str() {
            "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => {
                if principio(&ruta).contains("android") {
                    pon("android");
                } else {
                    *gradle_sin_android = true;
                }
            }
            "androidmanifest.xml" => pon("android"),
            "pom.xml" => pon("jvm"),
            "cargo.toml" => pon("rust"),
            "go.mod" => pon("go"),
            "pyproject.toml" | "setup.py" | "setup.cfg" | "pipfile" | "uv.lock" | "poetry.lock" => pon("python"),
            "package.json" => de_package_json(&ruta, &mut paquetes),
            "angular.json" => pon("web"),
            "index.html" | "_config.yml" | "hugo.toml" if raiz => pon("web"),
            "wp-config.php" => pon("wordpress"),
            "style.css" => {
                if principio(&ruta).contains("theme name:") {
                    pon("wordpress");
                }
            }
            "composer.json" => pon("php"),
            "gemfile" => pon("ruby"),
            "pubspec.yaml" => {
                pon("dart");
                if principio(&ruta).contains("flutter") {
                    pon("flutter");
                }
            }
            "podfile" => pon("ios"),
            "package.swift" => pon("swift"),
            "cmakelists.txt" => pon("cpp"),
            "dockerfile" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => pon("docker"),
            "project.godot" => pon("godot"),
            "manifest.json" => {
                if principio(&ruta).contains("\"manifest_version\"") {
                    pon("browser-extension");
                }
            }
            "deno.json" | "deno.jsonc" => pon("deno"),
            "tauri.conf.json" => pon("escritorio-node"),
            "ask-resources.json" => pon("alexa"),
            _ => {}
        }
        if l.starts_with("requirements") && ext == "txt" {
            pon("python");
        }
        if l.starts_with("dockerfile.") {
            pon("docker");
        }
        if ["astro.config.", "next.config.", "vite.config.", "nuxt.config.", "svelte.config.", "gatsby-config.", "remix.config."].iter().any(|p| l.starts_with(p)) {
            pon("web");
        }
        match ext {
            "py" | "ipynb" if raiz => pon("python"),
            "csproj" | "fsproj" | "vbproj" | "sln" => pon("dotnet"),
            "tf" => pon("terraform"),
            "stl" | "3mf" | "gcode" => pon("3d-print"),
            "php" => {
                pon("php");
                // Un plugin de WordPress: su fichero principal lleva la cabecera «Plugin Name:».
                if php_leidos < 10 {
                    php_leidos += 1;
                    if principio(&ruta).contains("plugin name:") {
                        pon("wordpress");
                    }
                }
            }
            _ => {}
        }
    }
    // Un mod de Claude Code: `hooks/hooks.json` (en la raíz o en un módulo).
    if dir.join("hooks").join("hooks.json").is_file() {
        pon("plugin-claude");
    }
    // El manifiesto de Android vive en `<módulo>/src/main/`.
    if dir.join("src").join("main").join("AndroidManifest.xml").is_file() {
        pon("android");
    }
    v.extend(aqui);
    v.extend(paquetes);
}

// --- Fugas: lo de un cliente que no debe ir en una memoria de oficio ----------------------------

/// Algo del texto de una memoria de oficio que parece de un proyecto o de un cliente.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fuga {
    /// `project` (nombre de un proyecto), `space` (un espacio: un cliente), `path` (ruta absoluta),
    /// `repository` (URL de un repositorio conocido o por SSH) o `domain` (un dominio que solo sale en
    /// memorias de un cliente, o el servidor git de uno).
    #[serde(rename = "type")]
    pub tipo: &'static str,
    pub text: String,
}

/// Nombres de proyecto tan genéricos que nombrarlos no dice de quién es (y los de los oficios).
const GENERICOS: &[&str] = &[
    "app", "apps", "web", "site", "website", "server", "client", "backend", "frontend", "api", "docs", "test", "tests", "demo", "main", "core", "common",
    "shared", "utils", "tools", "scripts", "plugin", "plugins", "mobile", "scratchpad", "temp", "project", "projects", "proyecto", "build", "release",
    "android", "kotlin", "java", "gradle", "react", "python", "node", "docker", "wordpress", "rust", "cargo", "astro", "next", "vite", "flutter", "swift",
    "theme", "themes", "library", "lib", "sdk", "cli", "bot", "service", "worker", "landing", "blog", "shop", "store", "admin", "dashboard", "src",
];

/// Extensiones de fichero que, al final de una palabra con punto, no son un dominio (`README.md`).
const EXTENSIONES: &[&str] = &[
    "rs", "kt", "kts", "java", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "json", "jsonc", "md", "mdx", "toml", "yaml", "yml", "xml", "gradle", "txt", "html",
    "htm", "css", "scss", "php", "lock", "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "sh", "ps1", "bat", "cmd", "exe", "dll", "so", "dylib", "apk",
    "aab", "db", "sql", "log", "cfg", "ini", "conf", "properties", "vue", "svelte", "astro", "go", "rb", "cs", "swift", "c", "h", "cpp", "hpp", "zip",
    "gz", "tar", "pdf", "mp4", "mp3", "wav", "wasm", "onnx", "bin", "stl", "3mf", "gcode", "env", "pem", "key", "crt", "lnk", "csv", "tsv", "jar", "aar",
    "pro", "plist", "storyboard", "xib", "dart", "lua", "vim", "el", "ex", "exs", "erl", "hs", "ml", "fs", "r", "jl", "nim", "zig", "v", "d", "m", "mm",
    "patch", "diff", "orig", "bak", "tmp", "old", "dat", "pak", "ttf", "otf", "woff", "woff2", "map", "min", "spec", "test", "local", "example", "sample",
];

/// Forjas públicas: su dominio no es de nadie (un repositorio en ellas sí, si es de un proyecto conocido).
const FORJAS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org", "dev.azure.com", "ssh.dev.azure.com", "sr.ht", "git.sr.ht", "huggingface.co"];

/// Si `nombre` (en minúsculas) sale en `texto` (en minúsculas) como palabra entera.
fn aparece(texto: &str, nombre: &str) -> bool {
    if nombre.is_empty() {
        return false;
    }
    let borde = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut desde = 0;
    while let Some(i) = texto[desde..].find(nombre) {
        let ini = desde + i;
        let fin = ini + nombre.len();
        if borde(texto[..ini].chars().next_back()) && borde(texto[fin..].chars().next()) {
            return true;
        }
        desde = ini + texto[ini..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Las palabras del texto, partidas por espacios y por lo que suele rodear una ruta o una URL.
fn palabras(texto: &str) -> impl Iterator<Item = &str> {
    texto
        .split(|c: char| c.is_whitespace() || "`\"'«»()[]<>,;|*".contains(c))
        .map(|p| p.trim_end_matches(['.', ':', '?', '!']))
        .filter(|p| !p.is_empty())
}

/// Si una palabra es una ruta absoluta de las que dicen de quién es: de una unidad de Windows (salvo
/// `Program Files` y `Windows`), una carpeta compartida, o una casa o una web de Unix.
fn ruta_personal(p: &str) -> bool {
    let b = p.as_bytes();
    let l = p.to_lowercase().replace('\\', "/");
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        return !["program files", "windows"].iter().any(|s| l[3..].starts_with(s));
    }
    if p.starts_with("\\\\") && p.len() > 3 {
        return true;
    }
    ["/home/", "/users/", "/var/www/", "/srv/", "/root/"].iter().any(|s| l.starts_with(s) && l.len() > s.len())
}

/// El dominio de una palabra (`https://a.b.com/x` o `a.b.com`), en minúsculas, si lo parece.
fn dominio(p: &str) -> Option<String> {
    let l = p.to_lowercase();
    let resto = l.split_once("://").map_or(l.as_str(), |(_, r)| r);
    let resto = resto.rsplit_once('@').map_or(resto, |(_, r)| r);
    let host = resto.split(['/', '?', '#']).next()?.split(':').next()?.trim_end_matches('.');
    let etiquetas: Vec<&str> = host.split('.').collect();
    let tld = etiquetas.last()?;
    let valido = etiquetas.len() >= 2
        && etiquetas.iter().all(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && (l.contains("://") || !EXTENSIONES.contains(tld));
    valido.then(|| host.to_string())
}

/// Los dominios que aparecen en un texto.
fn dominios_en(texto: &str) -> HashSet<String> {
    palabras(texto).filter_map(dominio).collect()
}

/// Los dos últimos trozos de un dominio (`gitlab.acme.com` → `acme.com`).
fn registrable(host: &str) -> String {
    let v: Vec<&str> = host.split('.').collect();
    v[v.len().saturating_sub(2)..].join(".")
}

/// Las formas de un nombre (ya en minúsculas y sin acentos) que se buscan en el texto: tal cual, con
/// cada separador cambiado por los otros y sin separadores («social-publish», «social publish»,
/// «social_publish», «socialpublish»).
fn variantes(n: &str) -> Vec<String> {
    let mut v = vec![n.to_string(), n.replace(['-', '_'], " "), n.replace(['-', ' '], "_"), n.replace(['_', ' '], "-"), n.replace(['-', '_', ' ', '.'], "")];
    v.retain(|x| !x.is_empty());
    v.sort();
    v.dedup();
    v
}

/// Las etiquetas reservadas no dicen nada de nadie: las de oficio (`trade:`, `trade-for:`), la fecha de
/// revisión (`review:`) y las de estado (`open`, `blocking`, `resolved`, `status`). De la de la clave
/// (`topic:<clave>`) cuenta la clave. Devuelve lo que hay que mirar de una etiqueta.
fn etiqueta_libre(t: &str) -> Option<&str> {
    use crate::memoria::{ABIERTA, BLOQUEANTE, ESTADO, ETIQUETA_TEMA, RESUELTA};
    if t.starts_with(PREFIJO) || t.starts_with(PARA) || t.starts_with(crate::revisar::ETIQUETA_FECHA) || [ABIERTA, BLOQUEANTE, RESUELTA, ESTADO].contains(&t) {
        return None;
    }
    Some(t.strip_prefix(ETIQUETA_TEMA).unwrap_or(t))
}

/// Las claves y los textos de unos datos (`data`), aplanados, para mirarlos como texto.
fn aplanar(v: &serde_json::Value, salida: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => salida.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| aplanar(x, salida)),
        serde_json::Value::Object(o) => {
            for (k, x) in o {
                salida.push(k.clone());
                aplanar(x, salida);
            }
        }
        _ => {}
    }
}

/// Todo lo que se mira en una memoria de oficio, como un texto: los trozos que se den (título,
/// contenido, rutas…), sus etiquetas sin las reservadas ([`etiqueta_libre`]), la clave y los datos
/// (sus claves y sus textos).
pub fn texto_para_fugas(partes: &[&str], tags: &[String], clave: Option<&str>, datos: Option<&serde_json::Value>) -> String {
    let mut v: Vec<String> = partes.iter().map(|p| p.to_string()).collect();
    v.extend(tags.iter().filter_map(|t| etiqueta_libre(t)).map(String::from));
    v.extend(clave.map(String::from));
    if let Some(d) = datos {
        aplanar(d, &mut v);
    }
    v.retain(|x| !x.trim().is_empty());
    v.join("\n")
}

/// Las fugas que impiden guardar. Con el permiso de fugas (`allow_leaks`), solo las que nunca son un
/// falso positivo: una ruta absoluta personal o la URL de un repositorio.
pub fn bloquean(fugas: Vec<Fuga>, permitir: bool) -> Vec<Fuga> {
    if permitir { fugas.into_iter().filter(|f| f.tipo == "path" || f.tipo == "repository").collect() } else { fugas }
}

/// Lo que en el texto de una memoria de oficio parece de un proyecto o de un cliente: nombres de
/// proyectos y de espacios, rutas absolutas personales, URLs de repositorios conocidos (o por SSH) y
/// dominios que solo salen en memorias de un cliente (o el de su servidor git). Sin repetir. Los
/// nombres se comparan sin mayúsculas ni acentos («balua» es «Balùa»).
pub fn fugas(db: &Connection, texto: &str) -> Result<Vec<Fuga>> {
    let min = crate::nombres::sin_acentos(&texto.to_lowercase());
    let mut v: Vec<Fuga> = Vec::new();
    let mut pon = |tipo: &'static str, text: String| {
        if !v.iter().any(|f| f.tipo == tipo && f.text.eq_ignore_ascii_case(&text)) {
            v.push(Fuga { tipo, text });
        }
    };
    // Espacios: un cliente, se llame como se llame (desde 2 letras: «QA», «HP»).
    let espacios: Vec<(String, String)> = {
        let mut q = db.prepare("SELECT key, name FROM space")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (key, name) in &espacios {
        for n in [key, name] {
            let n = crate::nombres::sin_acentos(&n.trim().to_lowercase());
            if n.chars().count() >= 2 && variantes(&n).iter().any(|x| x.chars().count() >= 2 && aparece(&min, x)) {
                pon("space", name.clone());
            }
        }
    }
    // Proyectos, salvo los de nombre genérico o muy corto.
    let proyectos: Vec<(String, String)> = {
        let mut q = db.prepare("SELECT name, key FROM project WHERE id != 0")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (name, _) in &proyectos {
        let n = crate::nombres::sin_acentos(&name.trim().to_lowercase());
        if n.chars().count() < 4 || GENERICOS.contains(&n.as_str()) || DETECTABLES.contains(&n.as_str()) || n.starts_with("tmp.") {
            continue;
        }
        if variantes(&n).iter().any(|x| x.chars().count() >= 4 && aparece(&min, x)) {
            pon("project", name.clone());
        }
    }
    // Rutas absolutas.
    for p in palabras(texto) {
        if ruta_personal(p) {
            pon("path", p.to_string());
        }
    }
    // Repositorios: los de los proyectos conocidos (por su clave o el `origin` de sus carpetas).
    let mut origenes: Vec<String> = proyectos.iter().filter_map(|(_, k)| k.strip_prefix("git:").map(String::from)).collect();
    {
        let mut q = db.prepare("SELECT DISTINCT origin FROM project_path WHERE origin != ''")?;
        origenes.extend(q.query_map([], |f| f.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    origenes.retain(|o| o.contains('/'));
    // Los servidores git que no son una forja pública son de alguien (`gitlab.acme.com`).
    let servidores: HashSet<String> = origenes
        .iter()
        .filter_map(|o| o.split('/').next())
        .filter(|h| h.contains('.') && !FORJAS.contains(h))
        .map(registrable)
        .collect();
    for p in palabras(texto) {
        let l = p.to_lowercase();
        if l.starts_with("git@") || l.starts_with("ssh://") {
            pon("repository", p.to_string());
            continue;
        }
        if !l.contains("://") && !l.contains('/') {
            continue;
        }
        let n = crate::sync::normalizar_remoto(&l);
        if origenes.iter().any(|o| n == *o || n.starts_with(&format!("{o}/"))) {
            pon("repository", p.to_string());
        }
    }
    // Dominios: el de un servidor git de alguien, o uno que sale en memorias de un cliente (de un
    // espacio o de sus proyectos) y en ninguna global.
    let aqui = dominios_en(texto);
    if aqui.is_empty() {
        return Ok(v);
    }
    let mut de_clientes: HashSet<String> = HashSet::new();
    if !espacios.is_empty() {
        let mut q = db.prepare(
            "SELECT m.title || ' ' || m.content FROM memory m
             WHERE m.space_id IS NOT NULL OR m.project_id IN (SELECT id FROM project WHERE space_id IS NOT NULL)",
        )?;
        for t in q.query_map([], |f| f.get::<_, String>(0))? {
            de_clientes.extend(dominios_en(&t?).into_iter().filter(|d| aqui.contains(d)));
        }
        if !de_clientes.is_empty() {
            let mut q = db.prepare("SELECT m.title || ' ' || m.content FROM memory m WHERE m.project_id = 0 AND m.space_id IS NULL")?;
            for t in q.query_map([], |f| f.get::<_, String>(0))? {
                for d in dominios_en(&t?) {
                    de_clientes.remove(&d);
                }
            }
        }
    }
    let mut aqui: Vec<String> = aqui.into_iter().collect();
    aqui.sort();
    for d in aqui {
        if de_clientes.contains(&d) || servidores.contains(&registrable(&d)) {
            pon("domain", d);
        }
    }
    Ok(v)
}

/// Las memorias cuyo historial no viaja: las que una versión previa de la 1.8.0 (sin publicar) promovió
/// de un cliente a un oficio con el mismo id y apuntó en `meta('historial_local:<id>')`. Desde que
/// promover lo de un cliente crea una memoria nueva ([`crear_de_oficio`]) ya no se apunta ninguna; las
/// que haya siguen sin viajar y su historial solo se enseña desde una carpeta general
/// (`memoria::historial_en`).
pub(crate) fn historiales_solo_aqui(db: &Connection) -> Result<HashSet<String>> {
    let mut q = db.prepare("SELECT substr(k, 17) FROM meta WHERE k LIKE 'historial_local:%'")?;
    Ok(q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?)
}

/// Si el historial de esta memoria se quedó en este equipo (ver [`historiales_solo_aqui`]).
pub(crate) fn historial_solo_aqui(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE k = 'historial_local:' || ?1)", [id], |f| f.get(0))?)
}

/// Lo que pide un `update` que toca los oficios (MCP y CLI).
pub struct PedidoCambio<'a> {
    /// `trade`: los oficios («android, web»); `Some("")` deja de ser de oficio.
    pub trade: Option<&'a str>,
    /// `for`: `client`, `personal` o `both`.
    pub para: Option<&'a str>,
    /// Lo que cambia (título, contenido, `replace`, `append`, rutas, etiquetas, a dónde va).
    pub cambios: &'a crate::memoria::Cambios,
    /// Los datos nuevos (`Some(None)`: se quitan), si se cambian.
    pub datos: Option<Option<&'a serde_json::Value>>,
    /// El permiso de fugas (`allow_leaks`): deja pasar lo que parezca un falso positivo, nunca una
    /// ruta ni un repositorio.
    pub permitir_fugas: bool,
}

/// Qué hacer con los oficios en un `update`.
#[derive(Debug, Default)]
pub struct Plan {
    /// Las etiquetas que quedan (`None`: no cambian).
    pub tags: Option<Vec<String>>,
    /// Hay que llevarla a global (era de un proyecto o de un espacio y pasa a ser de oficio).
    pub hacer_global: bool,
    /// Pasa de un cliente (un espacio, o un proyecto en uno) a un oficio: no se mueve, se crea una
    /// memoria de oficio nueva ([`crear_de_oficio`]) y la de cliente se queda donde estaba.
    pub nueva: bool,
}

/// Lo que hace un `update` con los oficios: hacerla de oficio (promoverla), dejar de serlo, cambiar para
/// quién es, o sacarla de su oficio al llevarla a un proyecto o un espacio. Lo nuevo de una memoria de
/// oficio pasa por `fugas` (todo lo que quedará si entra en un oficio: texto, rutas, etiquetas, clave y
/// datos), salvo lo que deje pasar el permiso de fugas: `Err` con ellas en `Ok(Err(…))`.
///
/// Promover una memoria personal (de un proyecto sin espacio) la mueve con su id, su historial y sus
/// veredictos: todo eso es de la persona. Promover una de un cliente crea una memoria nueva (`nueva`):
/// su historial y sus veredictos son del cliente y no deben llegar a los demás.
pub fn al_cambiar(db: &Connection, actual: &Memoria, p: PedidoCambio) -> Result<std::result::Result<Plan, Vec<Fuga>>> {
    let norma = |v: &[String]| -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty()).collect();
        v.sort();
        v.dedup();
        v
    };
    let c = p.cambios;
    let base = norma(&c.tags.clone().unwrap_or_else(|| actual.tags.clone()));
    let a_proyecto = c.space_id.is_some() || c.project.as_deref().is_some_and(|d| !d.is_empty());
    let a_global = c.project.as_deref() == Some("");
    let era = de_memoria(actual);
    let quitar = p.trade.is_some_and(|t| t.trim().is_empty());
    let mut pedidos = match p.trade.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => lista(t)?,
        None => Vec::new(),
    };
    for o in de_etiquetas(&base) {
        let o = nombre(&o)?;
        if !pedidos.contains(&o) {
            pedidos.push(o);
        }
    }
    let para_nuevo = p.para.map(para).transpose()?;
    let cambia = |tags: Vec<String>| -> Option<Vec<String>> {
        let tags = norma(&tags);
        (tags != norma(&actual.tags)).then_some(tags)
    };
    if quitar || a_proyecto {
        return Ok(Ok(Plan { tags: cambia(sin_oficio(base)), ..Plan::default() }));
    }
    let entra = !pedidos.is_empty() && (p.trade.is_some() || actual.global() || a_global);
    if !entra && era.is_empty() {
        ensure!(para_nuevo.is_none(), tr!("`for` only applies to a trade memory: give `trade` too", "`for` solo vale en una memoria de oficio: di también `trade`"));
        return Ok(Ok(Plan { tags: c.tags.as_ref().and_then(|_| cambia(sin_oficio(base))), ..Plan::default() }));
    }
    let oficios = if entra { pedidos } else { era.clone() };
    let para_final = para_nuevo.unwrap_or_else(|| para_de(&actual.tags));
    let nuevo_oficio = era.is_empty() || oficios.iter().any(|o| !era.contains(o));
    let texto = if nuevo_oficio {
        // Entra en un oficio (o en otro): todo lo que quedará, como quedará (tras `replace` y `append`).
        let (titulo, contenido) = crate::memoria::texto_tras(actual, c)?;
        let rutas = c.paths.clone().unwrap_or_else(|| actual.paths.clone());
        let datos = match p.datos {
            Some(d) => d.cloned(),
            None => crate::memoria::datos_de(db, &[actual.id.as_str()])?.remove(&actual.id),
        };
        texto_para_fugas(&[&titulo, &contenido, &rutas.join(" ")], &base, None, datos.as_ref())
    } else {
        // Ya era de este oficio: lo nuevo (lo que ponen `replace` y `append`, el título, el contenido y
        // las rutas que vengan, las etiquetas que no tenía y los datos nuevos).
        let mut partes: Vec<String> = c.replace.iter().map(|(_, con)| con.clone()).collect();
        partes.extend(c.append.clone());
        partes.extend(c.title.clone());
        partes.extend(c.content.clone());
        partes.extend(c.paths.iter().flatten().cloned());
        let tenia = norma(&actual.tags);
        let nuevas: Vec<String> = base.iter().filter(|t| !tenia.contains(t)).cloned().collect();
        texto_para_fugas(&partes.iter().map(String::as_str).collect::<Vec<_>>(), &nuevas, None, p.datos.flatten())
    };
    let f = bloquean(fugas(db, &texto)?, p.permitir_fugas);
    if !f.is_empty() {
        return Ok(Err(f));
    }
    Ok(Ok(Plan {
        tags: cambia(etiquetas_de_oficio(base, &oficios, para_final.as_deref())),
        hacer_global: !actual.global(),
        nueva: !actual.global() && !actual.space.is_empty(),
    }))
}

/// Promover a un oficio una memoria de un cliente: una memoria de oficio NUEVA (otro id, sin historial
/// ni veredictos) con el texto tal como queda tras los cambios (`c`, que ya pasaron por `fugas`), sus
/// rutas, su tipo, sus datos y las etiquetas del plan (`tags`, con el oficio). La del cliente no se toca:
/// sigue en su proyecto o en su espacio, con su historial, y la sincronización la sigue llevando por el
/// repositorio de su cliente (moverla al general dejaba la copia de los otros equipos del cliente en
/// conflicto, y su historial era texto del cliente).
pub fn crear_de_oficio(
    db: &mut Connection,
    actual: &Memoria,
    c: &crate::memoria::Cambios,
    tags: Vec<String>,
    origen: &str,
    modelo: Option<&crate::embeber::Modelo>,
) -> Result<Memoria> {
    let (title, content) = crate::memoria::texto_tras(actual, c)?;
    let datos = crate::memoria::datos_de(db, &[actual.id.as_str()])?.remove(&actual.id);
    let nueva = crate::memoria::Nueva {
        project_id: Some(0),
        kind: Some(c.kind.unwrap_or(actual.kind)),
        kind_text: if c.kind.is_none() { actual.kind_text.clone() } else { None },
        title,
        content,
        paths: c.paths.clone().unwrap_or_else(|| actual.paths.clone()),
        tags,
        pinned: c.pinned.unwrap_or(actual.pinned),
        source: origen.to_string(),
        ..Default::default()
    };
    let m = crate::memoria::guardar(db, nueva, modelo)?;
    if let Some(d) = datos {
        crate::memoria::poner_datos(db, &m.id, Some(d))?;
    }
    Ok(m)
}

/// Lo que se le dice al agente con las fugas.
pub const AVISO_FUGAS: &str = "A trade memory reaches every project of that trade, of every client: it must hold only knowledge about the stack itself. This memory (its text, tags, key or data) names what looks like a project or a client (see trade_leaks). Rewrite it without them (generic names, no absolute paths, repository URLs or client domains), or save it in the project instead (without `trade`). Only if they are false positives (a library's name, public documentation), call again with allow_leaks=true; that never lets through an absolute path or a repository URL. (`force` only skips the duplicate check.)";

/// Lo que se le dice al agente al promover una memoria de un cliente.
pub const AVISO_PROMOVIDA: &str = "It came from a client's project, so a NEW trade memory was created (this id, with no history or verdicts): the client's memory stays where it was, with its history. If it now only repeats this one, `forget` it.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_de_oficio() {
        assert_eq!(nombre(" Android ").unwrap(), "android");
        assert_eq!(nombre("trade:web").unwrap(), "web");
        assert_eq!(nombre("c++").unwrap(), "c++");
        assert!(nombre("").is_err());
        assert!(nombre("a b").is_err());
        assert!(nombre("-x").is_err());
        assert_eq!(lista("android, web android").unwrap(), ["android", "web"]);
    }

    #[test]
    fn palabras_enteras() {
        assert!(aparece("en balua el emulador", "balua"));
        assert!(!aparece("en baluarte", "balua"));
        assert!(aparece("ver social-publish-plugin/x", "social-publish-plugin"));
        assert!(aparece("ñandú y acme", "acme"));
    }

    #[test]
    fn rutas_y_dominios() {
        assert!(ruta_personal(r"C:\Users\ana\acme\app"));
        assert!(ruta_personal("D:/Clientes/acme"));
        assert!(!ruta_personal(r"C:\Program Files\Android\Android Studio"));
        assert!(ruta_personal("/home/ana/acme"));
        assert!(!ruta_personal("/system/bin/sh"));
        assert!(!ruta_personal("app/src/main"));
        assert_eq!(dominio("https://staging.acme-shop.com/login").as_deref(), Some("staging.acme-shop.com"));
        assert_eq!(dominio("acme-shop.com").as_deref(), Some("acme-shop.com"));
        assert_eq!(dominio("README.md"), None);
        assert_eq!(dominio("build.gradle.kts"), None);
        assert_eq!(dominio("v1.2.3"), None);
        assert_eq!(registrable("gitlab.acme.com"), "acme.com");
    }

    #[test]
    fn detecta_por_ficheros() {
        let t = crate::comun::Temporal::nueva("oficios-detecta");
        let raiz = t.to_path_buf();
        let escribe = |r: &str, c: &str| {
            let p = raiz.join(r);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        // Un monorepo: Android en `app/`, una web Astro en `web/`, y lo que no cuenta.
        escribe("settings.gradle.kts", "rootProject.name = \"x\"\ninclude(\":app\")");
        escribe("app/build.gradle.kts", "plugins { id(\"com.android.application\") }");
        escribe("web/package.json", r#"{"dependencies":{"astro":"5.0.0"}}"#);
        escribe("node_modules/x/Cargo.toml", "");
        escribe(".hidden/go.mod", "");
        escribe("web/sub/deep/Cargo.toml", "");
        assert_eq!(detectar(&raiz), ["android", "node", "web"]);
        // Gradle sin Android: JVM.
        let t2 = crate::comun::Temporal::nueva("oficios-jvm");
        std::fs::write(t2.join("build.gradle"), "plugins { id 'java' }").unwrap();
        assert_eq!(detectar(&t2), ["jvm"]);
        // Un tema de WordPress y un plugin.
        let t3 = crate::comun::Temporal::nueva("oficios-wp");
        std::fs::write(t3.join("style.css"), "/*\nTheme Name: Acme\n*/").unwrap();
        std::fs::write(t3.join("functions.php"), "<?php").unwrap();
        assert_eq!(detectar(&t3), ["php", "wordpress"]);
        let t4 = crate::comun::Temporal::nueva("oficios-wp-plugin");
        std::fs::write(t4.join("mi-plugin.php"), "<?php\n/**\n * Plugin Name: Mi plugin\n */").unwrap();
        assert_eq!(detectar(&t4), ["php", "wordpress"]);
        // React Native no es web.
        let t5 = crate::comun::Temporal::nueva("oficios-rn");
        std::fs::write(t5.join("package.json"), r#"{"dependencies":{"react":"18","react-native":"0.76"}}"#).unwrap();
        assert_eq!(detectar(&t5), ["node", "react-native"]);
        // Electron con React: escritorio, no web.
        let t7 = crate::comun::Temporal::nueva("oficios-electron");
        std::fs::write(t7.join("package.json"), r#"{"dependencies":{"react":"18"},"devDependencies":{"Electron":"30"}}"#).unwrap();
        std::fs::write(t7.join("vite.config.ts"), "").unwrap();
        assert_eq!(detectar(&t7), ["escritorio-node", "node"]);
        // Un plugin de Claude Code en Rust (Omoikane), y una skill de Alexa.
        let t8 = crate::comun::Temporal::nueva("oficios-plugin");
        std::fs::create_dir_all(t8.join(".claude-plugin")).unwrap();
        std::fs::write(t8.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
        std::fs::write(t8.join("Cargo.toml"), "").unwrap();
        assert_eq!(detectar(&t8), ["plugin-claude", "rust"]);
        let t9 = crate::comun::Temporal::nueva("oficios-alexa");
        std::fs::create_dir_all(t9.join("skill-package")).unwrap();
        std::fs::create_dir_all(t9.join("hooks")).unwrap();
        std::fs::write(t9.join("hooks").join("hooks.json"), "{}").unwrap();
        assert_eq!(detectar(&t9), ["alexa", "plugin-claude"]);
        // Nada que detectar.
        let t6 = crate::comun::Temporal::nueva("oficios-nada");
        std::fs::write(t6.join("notas.md"), "x").unwrap();
        assert!(detectar(&t6).is_empty());
    }
}
