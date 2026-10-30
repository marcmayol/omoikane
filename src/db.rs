//! La base: dónde vive, cómo se abre y su esquema.
//!
//! Una conexión por proceso, en WAL, con `busy_timeout` de 5 s y transacciones `IMMEDIATE` para
//! escribir: varios agentes (cada uno con su `omoi`) pueden escribir a la vez sin que un
//! `SQLITE_BUSY` llegue al usuario.

use crate::tr;
use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Versión del esquema que esta versión de omoi sabe crear y leer.
pub const VERSION_ESQUEMA: i64 = 9;

/// `OMOIKANE_DB` o `~/.omoikane/omoikane.db`.
pub fn ruta_por_defecto() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_DB").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
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
    db.pragma_update(None, "foreign_keys", true)?;
    migrar(&mut db)?;
    Ok(db)
}

fn version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA user_version", [], |f| f.get(0))?)
}

fn migrar(db: &mut Connection) -> Result<()> {
    if version(db)? == VERSION_ESQUEMA {
        return Ok(());
    }
    // Antes de migrar una base que ya tenía datos, una copia: una versión vieja no abre la nueva.
    let antes = version(db)?;
    if antes > 0
        && antes < VERSION_ESQUEMA
        && let Some(ruta) = db.path().filter(|p| !p.is_empty()).map(String::from)
    {
        let copia = format!("{ruta}.antes-del-esquema-{VERSION_ESQUEMA}");
        if !std::path::Path::new(&copia).exists() {
            let _ = db.execute("VACUUM INTO ?1", [&copia]);
        }
    }
    // IMMEDIATE: si dos procesos abren una base nueva a la vez, solo uno crea el esquema.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let v = version(&tx)?;
    anyhow::ensure!(
        v <= VERSION_ESQUEMA,
        tr!(
            "the database comes from a newer omoi (schema {v}; this one reads up to {VERSION_ESQUEMA})",
            "la base es de una versión más nueva de omoi (esquema {v}, esta entiende hasta {VERSION_ESQUEMA})"
        )
    );
    if v < 1 {
        tx.execute_batch(ESQUEMA_1)?;
    }
    if v < 2 {
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
    if v < 3 {
        tx.execute_batch(ESQUEMA_3)?;
    }
    if v < 4 {
        tx.execute_batch(ESQUEMA_4)?;
    }
    if v < 5 {
        tx.execute_batch(ESQUEMA_5)?;
    }
    if v < 6 {
        tx.execute_batch(ESQUEMA_6)?;
    }
    if v < 7 {
        tx.execute_batch(ESQUEMA_7)?;
        aflojar_tipos(&tx)?;
    }
    if v < 8 {
        tx.execute_batch(ESQUEMA_8)?;
        pasar_descartes(&tx)?;
    }
    if v < 9 {
        tx.execute_batch(ESQUEMA_9)?;
    }
    tx.pragma_update(None, "user_version", VERSION_ESQUEMA)?;
    tx.commit()?;
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
