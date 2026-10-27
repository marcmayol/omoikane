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
pub const VERSION_ESQUEMA: i64 = 1;

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
    tx.pragma_update(None, "user_version", VERSION_ESQUEMA)?;
    tx.commit()?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
