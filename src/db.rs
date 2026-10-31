//! La base: dónde vive, cómo se abre y su esquema.
//!
//! Una conexión por proceso, en WAL, con `busy_timeout` de 5 s y transacciones `IMMEDIATE` para
//! escribir: varios agentes (cada uno con su `omoi`) pueden escribir a la vez sin que un
//! `SQLITE_BUSY` llegue al usuario.

use crate::tr;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Versión del esquema que esta versión de omoi sabe crear y leer.
pub const VERSION_ESQUEMA: i64 = 10;

/// El esquema más viejo que puede leer una base de este esquema, guardado en `meta('lector_minimo')`.
/// Una versión de omoi abre una base de un esquema mayor que el suyo si `lector_minimo` no pasa del
/// suyo: un cambio que solo añade (una tabla, una columna con valor por defecto) no lo sube, y la base
/// sigue abriéndose con la versión anterior. El esquema 10 cambia la tabla `memory` (el proyecto pasa
/// de ruta a id), así que lo pone en 10.
pub const LECTOR_MINIMO: i64 = 10;

/// `OMOIKANE_DB` o `~/.omoikane/omoikane.db`.
pub fn ruta_por_defecto() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_DB").filter(|r| !r.is_empty()) {
        // Absoluta, como `--db`: la sincronización la usa desde otras carpetas.
        let r = PathBuf::from(r);
        return std::path::absolute(&r).unwrap_or(r);
    }
    dirs::home_dir().unwrap_or_default().join(".omoikane").join("omoikane.db")
}

/// Registra sqlite-vec en todas las conexiones que se abran después. Basta con una vez por proceso.
pub fn registrar_sqlite_vec() {
    static UNA_VEZ: std::sync::Once = std::sync::Once::new();
    UNA_VEZ.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    });
}

/// Abre (o crea) la base y la deja en la última versión del esquema.
pub fn abrir(ruta: &Path) -> Result<Connection> {
    registrar_sqlite_vec();
    if let Some(dir) = ruta.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| tr!("can't create {}", "no se puede crear {}", dir.display()))?;
    }
    let mut db = Connection::open(ruta).with_context(|| tr!("can't open {}", "no se puede abrir {}", ruta.display()))?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    // Las claves ajenas, después de migrar: reconstruir `memory` (esquema 10) pide tenerlas apagadas, y
    // `PRAGMA foreign_keys` no cambia dentro de una transacción (lang_altertable.html, paso 1).
    migrar(&mut db)?;
    asegurar_persona(&db)?;
    db.pragma_update(None, "foreign_keys", true)?;
    Ok(db)
}

fn version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA user_version", [], |f| f.get(0))?)
}

/// El `lector_minimo` que dice una base (el propio esquema si no lo dice: las de antes del 10 no lo
/// apuntaban, y cualquier esquema mayor rompía).
fn lector_minimo(db: &Connection, v: i64) -> i64 {
    db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |f| f.get::<_, String>(0))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(v)
}

/// Una base de un esquema mayor que el de esta versión: se abre tal cual si su `lector_minimo` lo
/// permite (lo nuevo que tenga, esta versión no lo toca), y si no, error.
fn comprobar_lector(db: &Connection, v: i64) -> Result<()> {
    let minimo = lector_minimo(db, v);
    anyhow::ensure!(
        minimo <= VERSION_ESQUEMA,
        tr!(
            "the database comes from a newer omoi (schema {v}, readable from schema {minimo}; this one reads up to {VERSION_ESQUEMA}): update omoi",
            "la base es de una versión más nueva de omoi (esquema {v}, legible desde el esquema {minimo}; esta entiende hasta {VERSION_ESQUEMA}): actualiza omoi"
        )
    );
    Ok(())
}

/// La persona local: la que va en las claves `dir:` (`dir:<persona>/<carpeta>`), para que la carpeta
/// `notas` de Ana y la de Carlos no sean el mismo proyecto. Vive en `meta('persona')`, que `abrir`
/// pone siempre; al configurar la sincronización se adopta la de la sync (`sync::adoptar_persona`) si
/// es la misma persona en otro equipo.
pub fn persona_de(db: &Connection) -> String {
    db.query_row("SELECT v FROM meta WHERE k = 'persona'", [], |f| f.get::<_, String>(0))
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| persona_inicial(db))
}

/// La persona de una base que aún no la tiene: la del `sync.json` general o la de cualquier
/// repositorio de espacio, si los hay; si no, una nueva y estable (`u-` y 8 caracteres de un ULID).
fn persona_inicial(db: &Connection) -> String {
    if let Some(ruta) = db.path().filter(|p| !p.is_empty()).map(PathBuf::from) {
        for r in crate::sync::repos(&ruta) {
            if let Ok(Some(c)) = crate::sync::leer_config_de(&r)
                && !c.person.is_empty()
            {
                return c.person;
            }
        }
    }
    format!("u-{}", ulid::Ulid::generate().to_string()[18..].to_lowercase())
}

/// Deja la persona apuntada en `meta` si aún no lo está y la devuelve. Una base del esquema 10 de
/// antes de la persona local (de una versión sin publicar) se pone al día de paso: la columna
/// `tombstone.space_key` y las claves `dir:/<carpeta>`, que se quedaron sin persona.
fn asegurar_persona(db: &Connection) -> Result<String> {
    if let Some(p) = db.query_row("SELECT v FROM meta WHERE k = 'persona'", [], |f| f.get::<_, String>(0)).optional()?.filter(|p| !p.is_empty()) {
        return Ok(p);
    }
    let persona = persona_inicial(db);
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('persona', ?1)", [&persona])?;
    if version(db)? >= 10 {
        let tiene: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('tombstone') WHERE name = 'space_key')", [], |f| f.get(0))?;
        if !tiene {
            db.execute_batch("ALTER TABLE tombstone ADD COLUMN space_key TEXT NOT NULL DEFAULT ''")?;
        }
        crate::proyecto::cambiar_persona(db, "", &persona)?;
    }
    Ok(persona)
}

fn migrar(db: &mut Connection) -> Result<()> {
    migrar_hasta(db, VERSION_ESQUEMA)
}

/// Una base en un esquema viejo (`hasta`), sin migrar más allá: para probar las migraciones con bases
/// como las que tiene la gente. No es para usarla: esta versión solo sabe trabajar con la última.
#[doc(hidden)]
pub fn abrir_en_esquema(ruta: &Path, hasta: i64) -> Result<Connection> {
    registrar_sqlite_vec();
    let mut db = Connection::open(ruta)?;
    db.busy_timeout(Duration::from_secs(5))?;
    migrar_hasta(&mut db, hasta)?;
    Ok(db)
}

fn migrar_hasta(db: &mut Connection, hasta: i64) -> Result<()> {
    let copia = db.path().filter(|p| !p.is_empty()).map(|r| format!("{r}.antes-del-esquema-{VERSION_ESQUEMA}"));
    migrar_copiando(db, hasta, copia)
}

/// Migra hasta `hasta`, con la copia previa en `copia` (ninguna para una base en memoria).
fn migrar_copiando(db: &mut Connection, hasta: i64, copia: Option<String>) -> Result<()> {
    let antes = version(db)?;
    if antes == hasta {
        return Ok(());
    }
    if antes > VERSION_ESQUEMA {
        return comprobar_lector(db, antes);
    }
    // Antes de migrar una base que ya tenía datos, una copia: una versión vieja no abre la nueva. Si la
    // copia falla (disco lleno, permisos), no se migra: sin ella no hay vuelta atrás.
    if antes > 0
        && let Some(copia) = copia
        && !copia_buena(Path::new(&copia))
    {
        // A un nombre temporal y luego se renombra: una copia cortada a medias (el proceso muere, el
        // disco se llena) nunca queda con el nombre de la buena.
        let tmp = format!("{copia}.tmp");
        let _ = std::fs::remove_file(&tmp);
        let fallo = || {
            tr!(
                "can't copy the database to {copia} before updating it to schema {VERSION_ESQUEMA}; nothing was changed",
                "no se puede copiar la base en {copia} antes de pasarla al esquema {VERSION_ESQUEMA}; no se ha cambiado nada"
            )
        };
        db.execute("VACUUM INTO ?1", [&tmp]).with_context(fallo)?;
        std::fs::rename(&tmp, &copia).with_context(fallo)?;
    }
    // Las claves ajenas, apagadas antes del BEGIN (dentro de una transacción no se pueden cambiar): con
    // ellas encendidas, el `DROP TABLE memory` del esquema 10 borraría en cascada el historial, las
    // rutas y los vectores. `abrir` las vuelve a encender al terminar.
    db.pragma_update(None, "foreign_keys", false)?;
    // IMMEDIATE: si dos procesos abren una base nueva a la vez, solo uno crea el esquema.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let v = version(&tx)?;
    if v >= hasta {
        // Otro proceso la migró mientras se esperaba el cerrojo (o es de una versión más nueva).
        comprobar_lector(&tx, v)?;
        return Ok(());
    }
    if v < 1 && 1 <= hasta {
        tx.execute_batch(ESQUEMA_1)?;
    }
    if v < 2 && 2 <= hasta {
        tx.execute_batch(ESQUEMA_2)?;
        // El índice FTS nuevo (con rutas) se rellena desde la tabla.
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, tags, paths)
             SELECT n, title, content,
                    (SELECT COALESCE(group_concat(value, ' '), '') FROM json_each(memory.tags)),
                    (SELECT COALESCE(group_concat(value, ' '), '') FROM json_each(memory.paths))
             FROM memory",
            [],
        )?;
    }
    if v < 3 && 3 <= hasta {
        tx.execute_batch(ESQUEMA_3)?;
    }
    if v < 4 && 4 <= hasta {
        tx.execute_batch(ESQUEMA_4)?;
    }
    if v < 5 && 5 <= hasta {
        tx.execute_batch(ESQUEMA_5)?;
    }
    if v < 6 && 6 <= hasta {
        tx.execute_batch(ESQUEMA_6)?;
    }
    if v < 7 && 7 <= hasta {
        tx.execute_batch(ESQUEMA_7)?;
        aflojar_tipos(&tx)?;
    }
    if v < 8 && 8 <= hasta {
        tx.execute_batch(ESQUEMA_8)?;
        pasar_descartes(&tx)?;
    }
    if v < 9 && 9 <= hasta {
        tx.execute_batch(ESQUEMA_9)?;
    }
    if v < 10 && 10 <= hasta {
        // La persona de las claves `dir:` antes de calcularlas: nunca `dir:/<carpeta>`.
        let persona = asegurar_persona(&tx)?;
        esquema_10(&tx, &persona)?;
    }
    // Con las claves ajenas apagadas nada las ha comprobado: se mira antes de dar la migración por buena.
    let malas: usize = {
        let mut q = tx.prepare("PRAGMA foreign_key_check")?;
        let mut filas = q.query([])?;
        let mut n = 0;
        while filas.next()?.is_some() {
            n += 1;
        }
        n
    };
    anyhow::ensure!(malas == 0, "schema {hasta}: {malas} rows break a foreign key; nothing was changed");
    tx.pragma_update(None, "user_version", hasta)?;
    tx.commit()?;
    Ok(())
}

/// Esquema 10 (0.20.0), el que congela la 1.0:
/// - **Proyectos y espacios como tablas** (`space`, `project`, `project_path`): la memoria guarda el id
///   de su proyecto en vez de la ruta, y cada proyecto su clave de sincronización (calculada una vez,
///   al crearlo) y todas las carpetas donde vive (clones, worktrees, la carpeta a la que se movió, o
///   la ruta que trajo otro equipo, `is_foreign`). Una memoria es de un proyecto (`project_id`), de
///   un espacio (`space_id`, sin proyecto) o global (ni uno ni otro): la restricción `CHECK` impide
///   las dos cosas a la vez.
/// - `kind` sin `CHECK`: un tipo que llegue de una versión futura se guarda tal cual.
/// - `content_hlc`: el sello del último cambio de título, contenido o tipo, con el que caducan los
///   veredictos (antes, comparando relojes de pared de equipos distintos).
/// - `extra` en `memory` y `verdict`: campos de versiones futuras que esta no entiende, para no
///   perderlos al reexportar.
/// - `review` vuelve a ser solo caché (sin `dismissed_at`, que ya vivía en `verdict`), y
///   `remote_project` pasa a `project_path`.
/// - `tombstone.space_key`: el espacio de lo olvidado, para que cada olvido vaya solo al repositorio de
///   su espacio (los de antes, vacío: al general). Y `meta('persona')`, la de las claves `dir:`.
/// - Locales, que no viajan: el proyecto desde el que se recibió cada memoria (`recall_log.project_id`)
///   y su resumen cuando el registro caduca (`memory_use`).
const ESQUEMA_10: &str = "
CREATE TABLE space (
  id         INTEGER PRIMARY KEY,
  key        TEXT NOT NULL UNIQUE,
  name       TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE project (
  id         INTEGER PRIMARY KEY,
  key        TEXT NOT NULL UNIQUE,
  name       TEXT NOT NULL,
  path       TEXT NOT NULL DEFAULT '',
  space_id   INTEGER REFERENCES space(id),
  created_at TEXT NOT NULL
);
CREATE INDEX project_name ON project(name);
CREATE INDEX project_space ON project(space_id) WHERE space_id IS NOT NULL;
INSERT INTO project(id, key, name, path, created_at) VALUES (0, '', '', '', strftime('%Y-%m-%dT%H:%M:%fZ','now'));
CREATE TABLE project_path (
  path       TEXT PRIMARY KEY,
  path_cmp   TEXT NOT NULL UNIQUE,
  project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
  origin     TEXT NOT NULL DEFAULT '',
  is_foreign INTEGER NOT NULL DEFAULT 0,
  last_seen  TEXT
) WITHOUT ROWID;
CREATE INDEX project_path_project ON project_path(project_id);

DROP TABLE review;
CREATE TABLE review (
  memory_id TEXT NOT NULL,
  kind      TEXT NOT NULL,
  other_id  TEXT NOT NULL DEFAULT '',
  detail    TEXT NOT NULL DEFAULT '',
  found_at  TEXT NOT NULL,
  PRIMARY KEY (memory_id, kind, other_id)
) WITHOUT ROWID;
DELETE FROM meta WHERE k = 'revision';

ALTER TABLE verdict ADD COLUMN extra TEXT NOT NULL DEFAULT '{}';
-- El espacio de cada olvido (su clave; vacío: de ninguno o de antes): a qué repositorio va.
ALTER TABLE tombstone ADD COLUMN space_key TEXT NOT NULL DEFAULT '';
ALTER TABLE recall_log ADD COLUMN project_id INTEGER NOT NULL DEFAULT 0;
CREATE TABLE memory_use (
  memory_id  TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL,
  veces      INTEGER NOT NULL DEFAULT 0,
  ultima     TEXT NOT NULL,
  PRIMARY KEY (memory_id, project_id)
) WITHOUT ROWID;

-- Lo que una base de antes tuviera suelto (con las claves ajenas apagadas en algún momento) no debe
-- parar la migración: se quita antes de comprobarlas.
DELETE FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM memory_path WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM recall_log WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM memory_vec WHERE n NOT IN (SELECT n FROM memory);
";

/// La tabla `memory` nueva, con los 12 pasos de lang_altertable.html: crear `memory_nueva`, copiar
/// conservando `n` (así `memory_fts` y `memory_vec`, que van por `n`, siguen valiendo), borrar la
/// vieja, renombrar y rehacer los índices (no hay disparadores ni vistas). `mig_proyecto` (ruta vieja →
/// id) la rellena `pasar_a_proyectos`.
const ESQUEMA_10_MEMORIA: &str = "
CREATE TABLE memory_nueva (
  n            INTEGER PRIMARY KEY,
  id           TEXT NOT NULL UNIQUE,
  project_id   INTEGER NOT NULL DEFAULT 0 REFERENCES project(id),
  space_id     INTEGER REFERENCES space(id),
  kind         TEXT NOT NULL,
  title        TEXT NOT NULL,
  content      TEXT NOT NULL,
  paths        TEXT NOT NULL DEFAULT '[]',
  tags         TEXT NOT NULL DEFAULT '[]',
  pinned       INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  recalled_at  TEXT,
  recall_count INTEGER NOT NULL DEFAULT 0,
  hlc          TEXT NOT NULL DEFAULT '',
  content_hlc  TEXT NOT NULL DEFAULT '',
  author       TEXT NOT NULL DEFAULT '',
  extra        TEXT NOT NULL DEFAULT '{}',
  CHECK (project_id = 0 OR space_id IS NULL)
);
-- content_hlc: el último cambio de texto fue en updated_at (fijar y mover no lo tocan). Como sello de
-- contador 0 y sin equipo, un veredicto que valía (updated_at <= at) sigue valiendo: su sello nunca
-- es anterior a su hora.
INSERT INTO memory_nueva(n, id, project_id, space_id, kind, title, content, paths, tags, pinned, source,
                         created_at, updated_at, recalled_at, recall_count, hlc, content_hlc, author)
SELECT n, id, COALESCE((SELECT project_id FROM mig_proyecto WHERE path = memory.project), 0), NULL, kind,
       title, content, paths, tags, pinned, source, created_at, updated_at, recalled_at, recall_count, hlc,
       printf('%013d-%010d-', CAST(round((julianday(updated_at) - 2440587.5) * 86400000) AS INTEGER), 0), author
FROM memory;
DROP TABLE memory;
ALTER TABLE memory_nueva RENAME TO memory;
CREATE INDEX memory_scope   ON memory(project_id, kind, updated_at);
CREATE INDEX memory_updated ON memory(updated_at);
CREATE INDEX memory_space   ON memory(space_id) WHERE space_id IS NOT NULL;
CREATE INDEX memory_always  ON memory(project_id, updated_at) WHERE pinned = 1 OR kind = 'preference';

UPDATE recall_log SET project_id = COALESCE(
  (SELECT p.project_id FROM session s JOIN mig_proyecto p ON p.path = s.project WHERE s.session = recall_log.session), 0);
DROP TABLE mig_proyecto;
DROP TABLE remote_project;
";

/// Si en `ruta` hay una copia previa que vale: se abre y tiene tablas. Una vacía o dañada (de una
/// migración que se cortó en una versión que copiaba directamente con el nombre final) se rehace.
fn copia_buena(ruta: &Path) -> bool {
    ruta.exists()
        && Connection::open_with_flags(ruta, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| c.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table'", [], |f| f.get::<_, i64>(0)))
            .is_ok_and(|n| n > 0)
}

fn esquema_10(tx: &Connection, persona: &str) -> Result<()> {
    tx.execute_batch(ESQUEMA_10)?;
    normalizar_sellos(tx)?;
    pasar_a_proyectos(tx, persona)?;
    tx.execute_batch(ESQUEMA_10_MEMORIA)?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', ?1)", [LECTOR_MINIMO.to_string()])?;
    Ok(())
}

/// Los sellos del esquema 5 llevan el contador con 4 cifras (`%013d-0000-`); los de ahora, con 10. Se
/// comparaban bien con `reloj::comparar`, pero no como texto (`max(hlc)`, `w.hlc > v.hlc`): ahora
/// todos tienen el mismo ancho y comparar como texto es comparar sellos.
fn normalizar_sellos(tx: &Connection) -> Result<()> {
    // (tabla, columnas que identifican la fila)
    for (tabla, clave) in [("memory", "n"), ("tombstone", "id"), ("forgotten", "id"), ("verdict", "memory_id || char(31) || other_id || char(31) || relation")] {
        let filas: Vec<(String, String)> = {
            let mut q = tx.prepare(&format!("SELECT CAST({clave} AS TEXT), hlc FROM {tabla} WHERE hlc != ''"))?;
            q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut u = tx.prepare(&format!("UPDATE {tabla} SET hlc = ?2 WHERE CAST({clave} AS TEXT) = ?1"))?;
        for (k, hlc) in filas {
            if let Some(nuevo) = crate::reloj::normalizado(&hlc) {
                u.execute(rusqlite::params![k, nuevo])?;
            }
        }
    }
    let ultimo: Option<String> = tx.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).ok();
    if let Some(nuevo) = ultimo.as_deref().and_then(crate::reloj::normalizado) {
        tx.execute("UPDATE meta SET v = ?1 WHERE k = 'hlc'", [nuevo])?;
    }
    Ok(())
}

/// Cada ruta de `memory.project` pasa a un proyecto con su clave:
/// - si llegó de otro equipo (`remote_project`), la clave con la que llegó, como ruta ajena;
/// - si existe en el disco, la de siempre (`sync::clave`: el `origin` de git, o `dir:<persona>/<carpeta>`);
/// - si ya no existe, la `dir:` (sin el `origin` no se puede saber más), y queda sin `last_seen`: es
///   un proyecto que «ya no está» (Revisar lo enseña con `omoi projects move`).
///
/// Las rutas con la misma clave `git:` son el mismo proyecto (clones, worktrees); con la misma `dir:`,
/// solo una ruta local se queda la clave (la que existe; las ajenas se juntan con ella, como hacía
/// `sync::adoptar`), y las demás, carpetas distintas con el mismo nombre, llevan `~2`, `~3`…
fn pasar_a_proyectos(tx: &Connection, persona: &str) -> Result<()> {
    tx.execute_batch("CREATE TEMP TABLE mig_proyecto (path TEXT PRIMARY KEY, project_id INTEGER NOT NULL);")?;
    let remotos: std::collections::HashMap<String, String> = {
        let mut q = tx.prepare("SELECT path, key FROM remote_project")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    // (ruta, memorias), las de más memorias primero: a igualdad de todo, esa se queda la clave.
    let rutas: Vec<(String, i64)> = {
        let mut q = tx.prepare("SELECT project, count(*) FROM memory WHERE project != '' GROUP BY project ORDER BY count(*) DESC, project")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    struct Ruta {
        ruta: String,
        clave: String,
        ajena: bool,
        existe: bool,
        origen: String,
    }
    let mut grupos: Vec<(String, Vec<Ruta>)> = Vec::new();
    for (ruta, _) in rutas {
        let r = match remotos.get(&ruta) {
            Some(k) => Ruta { clave: k.clone(), ajena: true, existe: false, origen: String::new(), ruta },
            None => {
                let existe = Path::new(&ruta).is_dir();
                Ruta { clave: crate::sync::clave(&ruta, persona), ajena: false, existe, origen: crate::proyecto::origen(&ruta), ruta }
            }
        };
        match grupos.iter_mut().find(|(k, _)| *k == r.clave) {
            Some((_, v)) => v.push(r),
            None => grupos.push((r.clave.clone(), vec![r])),
        }
    }
    let ahora: String = tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |f| f.get(0))?;
    let mut claves: std::collections::HashSet<String> = grupos.iter().map(|(k, _)| k.clone()).collect();
    for (clave, mut rutas) in grupos {
        // Las que existen, delante; dentro de eso, el orden de antes (más memorias primero).
        rutas.sort_by_key(|r| (r.ajena, !r.existe));
        let mut proyectos: Vec<(String, Vec<Ruta>)> = vec![(clave.clone(), Vec::new())];
        for r in rutas {
            let primera_local = proyectos[0].1.iter().any(|x| !x.ajena);
            if clave.starts_with("dir:") && !r.ajena && primera_local {
                // Otra carpeta con el mismo nombre: un proyecto aparte, con su sufijo.
                let mut n = 2;
                while claves.contains(&format!("{clave}~{n}")) {
                    n += 1;
                }
                let k = format!("{clave}~{n}");
                claves.insert(k.clone());
                proyectos.push((k, vec![r]));
            } else {
                proyectos[0].1.push(r);
            }
        }
        for (k, rs) in proyectos {
            let principal = rs.iter().find(|r| !r.ajena && r.existe).or_else(|| rs.iter().find(|r| !r.ajena)).or(rs.first()).map(|r| r.ruta.clone()).unwrap_or_default();
            let nombre = crate::proyecto::nombre_de(&principal, &k);
            tx.execute(
                "INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![k, nombre, principal, ahora],
            )?;
            let id = tx.last_insert_rowid();
            for r in rs {
                tx.execute(
                    "INSERT OR IGNORE INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![r.ruta, crate::proyecto::comparable(&r.ruta), id, r.origen, r.ajena, r.existe.then_some(&ahora)],
                )?;
                tx.execute("INSERT OR REPLACE INTO mig_proyecto(path, project_id) VALUES (?1, ?2)", rusqlite::params![r.ruta, id])?;
            }
        }
    }
    Ok(())
}

/// El tipo `procedure` (Fase 7.3) no cabía en el `CHECK` de `memory`. SQLite no cambia un `CHECK`
/// con `ALTER TABLE`; para aflojarlo sin tocar los datos, su documentación (lang_altertable.html,
/// «otros cambios de esquema») reescribe la definición con `writable_schema` y sube `schema_version`.
fn aflojar_tipos(tx: &Connection) -> Result<()> {
    let sql: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'memory'", [], |f| f.get(0))?;
    let viejo = "CHECK (kind IN ('fact','decision','gotcha','preference'))";
    if !sql.contains(viejo) {
        return Ok(());
    }
    let nuevo = sql.replace(viejo, "CHECK (kind IN ('fact','decision','gotcha','preference','procedure'))");
    let version: i64 = tx.query_row("PRAGMA schema_version", [], |f| f.get(0))?;
    tx.execute_batch("PRAGMA writable_schema = ON")?;
    tx.execute("UPDATE sqlite_schema SET sql = ?1 WHERE type = 'table' AND name = 'memory'", [&nuevo])?;
    tx.execute_batch(&format!("PRAGMA schema_version = {}; PRAGMA writable_schema = OFF", version + 1))?;
    Ok(())
}

/// Cada veredicto con su motivo y su autor (0.19.0): el `reason` del `update` o el título de la
/// memoria nueva, y la persona de la sync, `agent:<cliente>` o `user`. Los de antes se quedan con los
/// dos vacíos; las versiones viejas ignoran los campos en la línea `verdict` de la sincronización.
const ESQUEMA_9: &str = "
ALTER TABLE verdict ADD COLUMN reason TEXT NOT NULL DEFAULT '';
ALTER TABLE verdict ADD COLUMN author TEXT NOT NULL DEFAULT '';
";

/// Veredictos (0.17.0, `veredictos`): lo que el agente o el usuario dicen de una memoria o de una
/// pareja (`superseded`, `contradicts`, `compatible`, `ok:<aviso>`). Viajan por la sincronización,
/// así que llevan su sello; para una pareja manda el más nuevo.
const ESQUEMA_8: &str = "
CREATE TABLE verdict (
  memory_id TEXT NOT NULL,
  other_id  TEXT NOT NULL DEFAULT '',
  relation  TEXT NOT NULL,
  at        TEXT NOT NULL,
  hlc       TEXT NOT NULL,
  PRIMARY KEY (memory_id, other_id, relation)
) WITHOUT ROWID;
CREATE INDEX verdict_other ON verdict(other_id);
";

/// Los «está bien así» de la tabla `review` pasan a ser veredictos: el de una pareja desfasada,
/// `compatible`; el de un aviso de una sola memoria, `ok:<aviso>`.
fn pasar_descartes(tx: &Connection) -> Result<()> {
    let filas: Vec<(String, String, String, String)> = {
        let mut q = tx.prepare("SELECT memory_id, kind, other_id, dismissed_at FROM review WHERE dismissed_at IS NOT NULL")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, kind, otra, at) in filas {
        let relation = if otra.is_empty() { format!("ok:{kind}") } else { "compatible".to_string() };
        let hlc = crate::reloj::sello(tx)?;
        tx.execute("INSERT OR IGNORE INTO verdict(memory_id, other_id, relation, at, hlc) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![id, otra, relation, at, hlc])?;
    }
    tx.execute("DELETE FROM review WHERE dismissed_at IS NOT NULL", [])?;
    Ok(())
}

/// Candidatas sacadas de las conversaciones (Fase 7.2, `candidatas`), que el agente de la sesión
/// siguiente guarda o deja; y los enlaces `[[nombre]]` entre memorias (Fase 7.4, `enlaces`).
const ESQUEMA_7: &str = "
CREATE TABLE candidate (
  id         INTEGER PRIMARY KEY,
  project    TEXT NOT NULL,
  session    TEXT NOT NULL,
  kind       TEXT NOT NULL,
  text       TEXT NOT NULL,
  created_at TEXT NOT NULL,
  shown_at   TEXT,
  UNIQUE (session, text)
);
CREATE TABLE memory_link (
  from_id TEXT NOT NULL,
  to_id   TEXT NOT NULL,
  PRIMARY KEY (from_id, to_id)
) WITHOUT ROWID;
CREATE INDEX memory_link_to ON memory_link(to_id);
";

/// Revisión (Fase 7.1, `revisar`): las memorias que otra más nueva contradice, y lo que el usuario
/// dio por bueno («está bien así», `dismissed_at`), que vale hasta que la memoria vuelva a cambiar.
const ESQUEMA_6: &str = "
CREATE TABLE review (
  memory_id    TEXT NOT NULL,
  kind         TEXT NOT NULL,
  other_id     TEXT NOT NULL DEFAULT '',
  detail       TEXT NOT NULL DEFAULT '',
  found_at     TEXT NOT NULL,
  dismissed_at TEXT,
  PRIMARY KEY (memory_id, kind, other_id)
) WITHOUT ROWID;
";

/// Sincronización entre equipos: un reloj lógico híbrido por memoria (`hlc`, ver `reloj`), quién la
/// escribió (`author`) y las marcas de lo olvidado, para que otro equipo no lo devuelva. Las memorias
/// que ya había toman su sello de `updated_at`, sin equipo (así cualquier cambio posterior gana).
const ESQUEMA_5: &str = "
ALTER TABLE memory ADD COLUMN hlc TEXT NOT NULL DEFAULT '';
ALTER TABLE memory ADD COLUMN author TEXT NOT NULL DEFAULT '';
UPDATE memory SET hlc = printf('%013d-0000-', CAST(round((julianday(updated_at) - 2440587.5) * 86400000) AS INTEGER));
CREATE TABLE tombstone (
  id         TEXT PRIMARY KEY,
  deleted_at TEXT NOT NULL,
  hlc        TEXT NOT NULL
);
-- Proyectos que llegaron de otro equipo con una ruta que aquí no existe: en cuanto aparece aquí un
-- proyecto con la misma clave, sus memorias pasan a esa ruta.
CREATE TABLE remote_project (
  path TEXT PRIMARY KEY,
  key  TEXT NOT NULL
);
-- Lo que olvidó otro equipo (quizá otra persona): una copia con su historial, para `omoi sync restore`.
CREATE TABLE forgotten (
  id         TEXT PRIMARY KEY,
  memory     TEXT NOT NULL,
  versions   TEXT NOT NULL DEFAULT '[]',
  hlc        TEXT NOT NULL DEFAULT '',
  deleted_at TEXT NOT NULL,
  by         TEXT NOT NULL DEFAULT ''
);
";

/// `n` es el enlace estable con el índice FTS (y en la fase 2 con el vectorial); `id` es el ULID
/// que ven el agente y el usuario.
const ESQUEMA_1: &str = "
CREATE TABLE memory (
  n            INTEGER PRIMARY KEY,
  id           TEXT NOT NULL UNIQUE,
  project      TEXT NOT NULL DEFAULT '',
  kind         TEXT NOT NULL CHECK (kind IN ('fact','decision','gotcha','preference')),
  title        TEXT NOT NULL,
  content      TEXT NOT NULL,
  paths        TEXT NOT NULL DEFAULT '[]',
  tags         TEXT NOT NULL DEFAULT '[]',
  pinned       INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT '',
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  recalled_at  TEXT,
  recall_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX memory_project ON memory(project, kind, updated_at);
CREATE INDEX memory_pinned  ON memory(project) WHERE pinned = 1;

CREATE TABLE memory_version (
  memory_id  TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  version    INTEGER NOT NULL,
  title      TEXT NOT NULL,
  content    TEXT NOT NULL,
  kind       TEXT NOT NULL,
  paths      TEXT NOT NULL,
  tags       TEXT NOT NULL,
  changed_at TEXT NOT NULL,
  reason     TEXT,
  PRIMARY KEY (memory_id, version)
);

CREATE TABLE memory_path (
  memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  path      TEXT NOT NULL,
  PRIMARY KEY (memory_id, path)
);
CREATE INDEX memory_path_path ON memory_path(path);

-- Sin contenido propio (el texto ya está en memory) y con borrado por rowid.
CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags,
  content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
";

/// Cuándo empezó cada sesión y cuántos mensajes lleva, para recordar al agente que guarde lo que
/// aprende si en una sesión larga no ha guardado nada (la idea es de engram).
const ESQUEMA_4: &str = "
CREATE TABLE session (
  session     TEXT PRIMARY KEY,
  project     TEXT NOT NULL,
  started_at  TEXT NOT NULL,
  prompts     INTEGER NOT NULL DEFAULT 0,
  reminded_at TEXT
);
";

/// Fase 4: qué memorias se han inyectado ya en cada sesión, para no repetirlas en cada mensaje.
const ESQUEMA_3: &str = "
CREATE TABLE recall_log (
  session   TEXT NOT NULL,
  memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  at        TEXT NOT NULL,
  PRIMARY KEY (session, memory_id)
);
CREATE INDEX recall_log_at ON recall_log(at);
";

/// Fase 2: las rutas entran en la búsqueda (FTS5 no deja añadir columnas: se rehace el índice) y
/// los vectores van en una tabla normal. Un BLOB con `vec_distance_cosine` filtra por proyecto en el
/// mismo SQL y fue el doble de rápido que `vec0` (que no sabe filtrar dentro del KNN).
const ESQUEMA_2: &str = "
DROP TABLE memory_fts;
CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags, paths,
  content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

-- Vector de title + content, normalizado. El modelo con el que se calcularon va en meta('modelo').
CREATE TABLE memory_vec (
  n INTEGER PRIMARY KEY REFERENCES memory(n) ON DELETE CASCADE,
  e BLOB NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migra_de_1_a_2_y_reindexa() {
        let dir = std::env::temp_dir().join(format!("omoi-db-migra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ruta = dir.join("o.db");
        {
            // Una base de la fase 1, a mano.
            let db = Connection::open(&ruta).unwrap();
            db.execute_batch(ESQUEMA_1).unwrap();
            db.execute_batch(
                "INSERT INTO memory(n, id, kind, title, content, paths, tags, created_at, updated_at)
                 VALUES (7, 'X', 'gotcha', 'Trampa', 'texto', '[\"src/auth.rs\"]', '[\"seguridad\"]', 'a', 'a');
                 INSERT INTO memory_fts(rowid, title, content, tags) VALUES (7, 'Trampa', 'texto', 'seguridad');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = abrir(&ruta).unwrap();
        assert_eq!(version(&db).unwrap(), VERSION_ESQUEMA);
        let por = |q: &str| -> i64 {
            db.query_row("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH ?1", [q], |f| f.get(0)).unwrap()
        };
        assert_eq!(por("auth"), 1);
        assert_eq!(por("seguridad"), 1);
        assert_eq!(por("trampa"), 1);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sin_copia_previa_no_se_migra() {
        let dir = std::env::temp_dir().join(format!("omoi-db-sin-copia-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ruta = dir.join("o.db");
        abrir_en_esquema(&ruta, 9).unwrap();
        let mut db = Connection::open(&ruta).unwrap();
        // Una copia que no se puede escribir (su carpeta no existe): la base se queda como estaba.
        let imposible = dir.join("no-existe").join("copia.db").display().to_string();
        let e = migrar_copiando(&mut db, VERSION_ESQUEMA, Some(imposible)).unwrap_err();
        assert!(format!("{e:#}").contains("copia") || format!("{e:#}").contains("copy"), "{e:#}");
        assert_eq!(version(&db).unwrap(), 9);
        drop(db);
        // Con la copia posible, sí.
        let db = abrir(&ruta).unwrap();
        assert_eq!(version(&db).unwrap(), VERSION_ESQUEMA);
        assert!(dir.join("o.db.antes-del-esquema-10").exists());
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rehace_una_copia_previa_a_medias() {
        let dir = std::env::temp_dir().join(format!("omoi-db-copia-medias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ruta = dir.join("o.db");
        abrir_en_esquema(&ruta, 9).unwrap().execute_batch("INSERT INTO meta(k, v) VALUES ('marca', 'de la 9')").unwrap();
        // Una copia cortada (de una versión que copiaba directamente con el nombre final): sin tablas.
        let copia = dir.join("o.db.antes-del-esquema-10");
        std::fs::write(&copia, b"").unwrap();
        let db = abrir(&ruta).unwrap();
        assert_eq!(version(&db).unwrap(), VERSION_ESQUEMA);
        drop(db);
        let c = Connection::open(&copia).unwrap();
        assert_eq!(version(&c).unwrap(), 9, "la copia es la base de antes");
        assert_eq!(c.query_row("SELECT v FROM meta WHERE k = 'marca'", [], |f| f.get::<_, String>(0)).unwrap(), "de la 9");
        drop(c);
        assert!(!dir.join("o.db.antes-del-esquema-10.tmp").exists(), "sin el temporal");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn crea_y_reabre() {
        let dir = std::env::temp_dir().join(format!("omoi-db-{}", std::process::id()));
        let ruta = dir.join("sub").join("o.db");
        let db = abrir(&ruta).unwrap();
        assert_eq!(version(&db).unwrap(), VERSION_ESQUEMA);
        drop(db);
        let db = abrir(&ruta).unwrap();
        let modo: String = db.query_row("PRAGMA journal_mode", [], |f| f.get(0)).unwrap();
        assert_eq!(modo, "wal");
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
