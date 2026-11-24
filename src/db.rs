//! The database: where it lives, how it is opened and its schema.
//!
//! One connection per process, in WAL, with its own wait of up to 5 s (`wait_turn`) and `IMMEDIATE`
//! transactions to write: several agents (each with its own `omoi`) can write at the same time
//! without a `SQLITE_BUSY` reaching the user. A deferred transaction that reads and then writes would
//! get `BUSY_SNAPSHOT` without waiting if someone else writes in between: that's why all the ones that
//! write are `IMMEDIATE`. `tests/concurrencia.rs` checks it, with real `omoi` processes.

use crate::tr;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Schema version this version of omoi can create and read.
pub const VERSION_ESQUEMA: i64 = 10;

/// The oldest schema that can read a database of this schema, stored in `meta('lector_minimo')`.
/// A version of omoi opens a database of a schema newer than its own if `lector_minimo` doesn't go
/// past its own: a change that only adds (a table, a column with a default value) doesn't raise it,
/// and the database keeps opening with the previous version. Schema 10 changes the `memory` table (the
/// project goes from path to id), so it sets it to 10.
pub const LECTOR_MINIMO: i64 = 10;

/// `OMOIKANE_DB`, or `omoikane.db` in Omoikane's folder (`OMOIKANE_HOME` or `~/.omoikane`). Without
/// either, relative as before: the CLI has already refused earlier (`entorno::sin_casa`).
pub fn ruta_por_defecto() -> PathBuf {
    if let Some(r) = std::env::var_os("OMOIKANE_DB").filter(|r| !r.is_empty()) {
        // Absolute, like `--db`: sync uses it from other folders.
        let r = PathBuf::from(r);
        return std::path::absolute(&r).unwrap_or(r);
    }
    crate::entorno::casa().unwrap_or_else(|| PathBuf::from(".omoikane")).join("omoikane.db")
}

/// Registers sqlite-vec in every connection opened afterwards. Once per process is enough.
pub fn registrar_sqlite_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
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

/// The longest a connection waits for another to release the database before giving `database is locked`.
pub const ESPERA_MAXIMA: Duration = Duration::from_secs(5);

/// Microseconds this process has spent waiting for another connection (see `wait_turn`).
static WAITED_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long this process has waited for another to release the database. Shown by the trace (`OMOIKANE_TRAZA`).
pub fn esperado() -> Duration {
    Duration::from_micros(WAITED_US.load(std::sync::atomic::Ordering::Relaxed))
}

/// The wait when another connection has the database (instead of `busy_timeout`). SQLite's sleeps 1,
/// 2, 5, 10… up to 100 ms between attempts (and on Windows a `Sleep(1)` can be quite a bit more), so
/// whoever was waiting arrived late to the gaps. With 20 agents at once (`tests/concurrencia.rs`), the
/// mean wait of a `remember` went down from 200-300 ms to 150, that of each message's recall from
/// 95-170 to 50, and its P99 from 1.4-2.1 s to 0.8-1.2 s. Here it retries after 0.1-2 ms (with some
/// randomness, so they don't all wake up at once), up to `ESPERA_MAXIMA` from the first attempt.
fn wait_turn(attempts: i32) -> bool {
    thread_local! {
        static SINCE: std::cell::Cell<std::time::Instant> = std::cell::Cell::new(std::time::Instant::now());
    }
    let now = std::time::Instant::now();
    if attempts == 0 {
        SINCE.with(|d| d.set(now));
    }
    if now.duration_since(SINCE.with(|d| d.get())) >= ESPERA_MAXIMA {
        return false;
    }
    let cap = (100u64 << attempts.clamp(0, 5)).min(2_000);
    // The randomness of the clock's nanoseconds is enough: it's only to spread out the wake-ups.
    let jitter = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0);
    let us = cap / 2 + jitter % (cap / 2 + 1);
    std::thread::sleep(Duration::from_micros(us));
    WAITED_US.fetch_add(now.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Opens (or creates) the database and brings it to the latest schema version.
///
/// If, when opening, another connection has the database and SQLite gives `SQLITE_BUSY` without going
/// through `wait_turn` (it happens, rarely, when several processes create a new database at once: 2
/// of about 1,500 opens in the tests, and one was a `remember` that failed with `database is locked`),
/// it is opened again from the start, up to `ESPERA_MAXIMA`. Everything the open does can be repeated:
/// each step first checks whether it's already done, and what it writes goes in transactions.
pub fn abrir(path: &Path) -> Result<Connection> {
    let since = std::time::Instant::now();
    let mut attempt = 0u32;
    loop {
        match open_once(path) {
            Err(e) if ocupada(&e) && since.elapsed() < ESPERA_MAXIMA => {
                if std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()) {
                    eprintln!("TRAZA abrir_reintento {}", serde_json::json!({ "error": format!("{e:#}") }));
                }
                attempt += 1;
                std::thread::sleep(Duration::from_millis(u64::from(attempt.min(10))));
            }
            r => return r,
        }
    }
}

/// Whether the error is that another connection had the database (`SQLITE_BUSY` or `SQLITE_LOCKED`).
pub fn ocupada(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<rusqlite::Error>()
            .and_then(|r| r.sqlite_error_code())
            .is_some_and(|c| matches!(c, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    })
}

fn open_once(path: &Path) -> Result<Connection> {
    registrar_sqlite_vec();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| tr!("can't create {}", "no se puede crear {}", dir.display()))?;
    }
    let mut db = Connection::open(path).with_context(|| tr!("can't open {}", "no se puede abrir {}", path.display()))?;
    db.busy_handler(Some(wait_turn))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    // Without moving the WAL into the database on close. Almost every `omoi` is a short process (the
    // hook of each message) and almost always the only connection: on closing it, SQLite moved the WAL
    // into the database, synced it to disk and deleted the WAL, and the next process created it again
    // and synced it when writing the first page. Each sync is 30-70 ms on Windows: the recall without
    // the meaning model took 185 ms instead of 34. The WAL stays on disk (it is the database: nobody
    // should copy `omoikane.db` without it; copies go through `VACUUM INTO`) and is moved into the
    // database when it reaches `wal_autocheckpoint` pages, as with several open connections.
    db.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    // Foreign keys, after migrating: rebuilding `memory` (schema 10) requires them off, and
    // `PRAGMA foreign_keys` doesn't change inside a transaction (lang_altertable.html, step 1).
    migrate(&mut db)?;
    update_on_open(&mut db)?;
    ensure_locals(&mut db)?;
    crate::significado::crear_tabla(&db)?;
    crate::oficios::crear_tabla(&db)?;
    db.pragma_update(None, "foreign_keys", true)?;
    Ok(db)
}

/// What this version adds to stay fast with many memories and that isn't part of the schema: an
/// index, two tables and some triggers that a 1.0 ignores (it doesn't read them, and the triggers, in
/// plain SQL, it also maintains when writing). So the database still opens with 1.0 and
/// `lector_minimo` doesn't go up.
///
/// - `memory_ambito`: each memory's scope by its `n`, to join with `memory_fts` and `memory_vec`
///   without reading the whole row (with 100,000 memories, counting those of a word went from 10 to
///   3 ms). Used with `INDEXED BY`: on its own, SQLite goes by the primary key.
/// - `memory_reciente`: the scope's latest in order, without reading and sorting all of them (at
///   session start, with 100,000 memories, 440 ms).
/// - `memory_hlc`: the latest stamp, without reading the table (the fingerprint of `revisar::al_dia`,
///   in every session: with 100,000 memories, 90 ms).
/// - `memory_cuenta`: how many memories there are per project and space, kept up to date by triggers;
///   counting a scope's no longer scans the table.
/// - `vec_cambio`: which vectors have changed since the vector summary was last brought up to date
///   (`vectores::al_dia`), with the previous vector. The triggers fill it, whoever writes (also a 1.0
///   or sync).
///
/// If any of the pieces is missing (a new database, a 1.0 one or one whose `memory` a migration
/// rebuilt), all are rebuilt and recounted; the vector summary will be rebuilt whole when used.
const LOCALS: &str = "
CREATE INDEX memory_ambito ON memory(n, project_id, space_id, kind);
CREATE INDEX memory_hlc ON memory(hlc);
CREATE INDEX memory_reciente ON memory(updated_at, id, project_id, space_id);

CREATE TABLE memory_cuenta (project_id INTEGER NOT NULL, space_id INTEGER, n INTEGER NOT NULL);
INSERT INTO memory_cuenta(project_id, space_id, n) SELECT project_id, space_id, count(*) FROM memory GROUP BY project_id, space_id;
CREATE TRIGGER memory_cuenta_mas AFTER INSERT ON memory BEGIN
  INSERT INTO memory_cuenta(project_id, space_id, n) SELECT NEW.project_id, NEW.space_id, 0
    WHERE NOT EXISTS (SELECT 1 FROM memory_cuenta WHERE project_id = NEW.project_id AND space_id IS NEW.space_id);
  UPDATE memory_cuenta SET n = n + 1 WHERE project_id = NEW.project_id AND space_id IS NEW.space_id;
END;
CREATE TRIGGER memory_cuenta_menos AFTER DELETE ON memory BEGIN
  UPDATE memory_cuenta SET n = n - 1 WHERE project_id = OLD.project_id AND space_id IS OLD.space_id;
END;
CREATE TRIGGER memory_cuenta_mueve AFTER UPDATE OF project_id, space_id ON memory BEGIN
  UPDATE memory_cuenta SET n = n - 1 WHERE project_id = OLD.project_id AND space_id IS OLD.space_id;
  INSERT INTO memory_cuenta(project_id, space_id, n) SELECT NEW.project_id, NEW.space_id, 0
    WHERE NOT EXISTS (SELECT 1 FROM memory_cuenta WHERE project_id = NEW.project_id AND space_id IS NEW.space_id);
  UPDATE memory_cuenta SET n = n + 1 WHERE project_id = NEW.project_id AND space_id IS NEW.space_id;
END;

-- Without `INSERT OR IGNORE`: inside a trigger the `OR` of the outer statement rules (the
-- `INSERT OR REPLACE` of `vectores::guardar`), which would change the recorded previous vector. The
-- first one counts: it's the one in the summary. In the `INSERT` the previous one is read before the
-- `REPLACE` deletes it (that delete fires nothing). Without a summary nothing is recorded: it'll be
-- built whole (that way, changing model doesn't copy all the old vectors here).
CREATE TABLE vec_cambio (n INTEGER PRIMARY KEY, viejo BLOB);
CREATE TRIGGER vec_cambio_pon BEFORE INSERT ON memory_vec WHEN EXISTS (SELECT 1 FROM meta WHERE k = 'vec_resumen') BEGIN
  INSERT INTO vec_cambio(n, viejo) SELECT NEW.n, (SELECT e FROM memory_vec WHERE n = NEW.n)
    WHERE NOT EXISTS (SELECT 1 FROM vec_cambio WHERE n = NEW.n);
END;
CREATE TRIGGER vec_cambio_cambia BEFORE UPDATE ON memory_vec WHEN EXISTS (SELECT 1 FROM meta WHERE k = 'vec_resumen') BEGIN
  INSERT INTO vec_cambio(n, viejo) SELECT OLD.n, OLD.e WHERE NOT EXISTS (SELECT 1 FROM vec_cambio WHERE n = OLD.n);
  INSERT INTO vec_cambio(n, viejo) SELECT NEW.n, (SELECT e FROM memory_vec WHERE n = NEW.n)
    WHERE NOT EXISTS (SELECT 1 FROM vec_cambio WHERE n = NEW.n);
END;
CREATE TRIGGER vec_cambio_quita BEFORE DELETE ON memory_vec WHEN EXISTS (SELECT 1 FROM meta WHERE k = 'vec_resumen') BEGIN
  INSERT INTO vec_cambio(n, viejo) SELECT OLD.n, OLD.e WHERE NOT EXISTS (SELECT 1 FROM vec_cambio WHERE n = OLD.n);
END;

-- The vector summary (`vectores::al_dia`): those of `memory_vec` in int8, 1,024 at a time by `n`.
CREATE TABLE vec_bloque (b INTEGER PRIMARY KEY, ns BLOB NOT NULL, datos BLOB NOT NULL);
";

/// The names of what `LOCALS` creates, to check whether it's all there.
const LOCAL_NAMES: &[&str] = &[
    "memory_ambito",
    "memory_hlc",
    "memory_reciente",
    "memory_cuenta",
    "memory_cuenta_mas",
    "memory_cuenta_menos",
    "memory_cuenta_mueve",
    "vec_cambio",
    "vec_cambio_pon",
    "vec_cambio_cambia",
    "vec_cambio_quita",
    "vec_bloque",
];

/// Puts `LOCALS` in the database (see there). Normally it's already there: one query to `sqlite_schema`.
fn ensure_locals(db: &mut Connection) -> Result<()> {
    let list = LOCAL_NAMES.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(", ");
    let query = format!("SELECT count(*) FROM sqlite_schema WHERE name IN ({list})");
    let present = |db: &Connection| -> Result<usize> { Ok(db.query_row(&query, [], |r| r.get::<_, i64>(0))? as usize) };
    if present(db)? == LOCAL_NAMES.len() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if present(&tx)? != LOCAL_NAMES.len() {
        for p in LOCAL_NAMES {
            let kind: Option<String> = tx.query_row("SELECT type FROM sqlite_schema WHERE name = ?1", [p], |r| r.get(0)).optional()?;
            if let Some(kind) = kind {
                tx.execute_batch(&format!("DROP {} \"{p}\"", kind.to_uppercase()))?;
            }
        }
        tx.execute_batch(LOCALS)?;
        crate::vectores::olvidar_resumen(&tx)?;
    }
    tx.commit()?;
    Ok(())
}

fn schema_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// The `lector_minimo` a database states (its own schema if it doesn't state one: those before 10
/// didn't record it, and any newer schema broke them).
fn min_reader(db: &Connection, v: i64) -> i64 {
    db.query_row("SELECT v FROM meta WHERE k = 'lector_minimo'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(v)
}

/// A database of a schema newer than this version's: it's opened as is if its `lector_minimo` allows
/// it (whatever new it has, this version doesn't touch), and if not, error.
fn check_reader(db: &Connection, v: i64) -> Result<()> {
    let minimum = min_reader(db, v);
    anyhow::ensure!(
        minimum <= VERSION_ESQUEMA,
        tr!(
            "the database comes from a newer omoi (schema {v}, readable from schema {minimum}; this one reads up to {VERSION_ESQUEMA}): update omoi",
            "la base es de una versión más nueva de omoi (esquema {v}, legible desde el esquema {minimum}; esta entiende hasta {VERSION_ESQUEMA}): actualiza omoi"
        )
    );
    Ok(())
}

/// The local person: the one that goes in `dir:` keys (`dir:<person>/<folder>`), so that Ana's
/// `notas` folder and Carlos's aren't the same project. It lives in `meta('persona')`, which `abrir`
/// always sets; when setting up sync, sync's person is adopted (`sync::adoptar_persona`) if it's the
/// same person on another computer.
pub fn persona_de(db: &Connection) -> String {
    db.query_row("SELECT v FROM meta WHERE k = 'persona'", [], |r| r.get::<_, String>(0))
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| initial_person(db))
}

/// The person of a database that doesn't have one yet: the one from the general `sync.json` or from
/// any space repository, if there are any; otherwise, a new and stable one (`u-` and 8 characters of
/// a ULID).
fn initial_person(db: &Connection) -> String {
    if let Some(path) = db.path().filter(|p| !p.is_empty()).map(PathBuf::from) {
        for r in crate::sync::repos(&path) {
            if let Ok(Some(c)) = crate::sync::leer_config_de(&r)
                && !c.person.is_empty()
            {
                return c.person;
            }
        }
    }
    format!("u-{}", ulid::Ulid::generate().to_string()[18..].to_lowercase())
}

/// Records the person in `meta` if it isn't yet and returns it. A schema 10 database from before the
/// local person (from an unreleased version) is brought up to date along the way: the column
/// `tombstone.space_key` and the `dir:/<folder>` keys, which were left without a person.
fn ensure_person(db: &Connection) -> Result<String> {
    if let Some(p) = db.query_row("SELECT v FROM meta WHERE k = 'persona'", [], |r| r.get::<_, String>(0)).optional()?.filter(|p| !p.is_empty()) {
        return Ok(p);
    }
    let person = initial_person(db);
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('persona', ?1)", [&person])?;
    if schema_version(db)? >= 10 {
        let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('tombstone') WHERE name = 'space_key')", [], |r| r.get(0))?;
        if !has {
            db.execute_batch("ALTER TABLE tombstone ADD COLUMN space_key TEXT NOT NULL DEFAULT ''")?;
        }
        crate::proyecto::cambiar_persona(db, "", &person)?;
    }
    Ok(person)
}

/// `update_schema_10` and `ensure_person` on open. Almost always there's nothing to do and reading is
/// enough; if there is, it's done in an `IMMEDIATE` transaction that checks again inside: two
/// processes opening an old database at once don't both add the same column («duplicate column») nor
/// each record a different person.
fn update_on_open(db: &mut Connection) -> Result<()> {
    let has_person: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE k = 'persona' AND v != '')", [], |r| r.get(0))?;
    if has_person && schema_version(db)? != 10 {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    update_schema_10(&tx)?;
    ensure_person(&tx)?;
    tx.commit()?;
    Ok(())
}

/// A schema 10 database from an unreleased version missing `project.hlc` (the stamp of the project's
/// last space change, which travels in `project` lines) or `project.paused` (the pause a `project`
/// line that states another space leaves it in): they are added, empty.
///
/// And since 1.9.0, `memory_version.extra` (the fields of a history version this version doesn't
/// know, like `memory.extra`): a column with a default value added on open, without raising the schema
/// or `lector_minimo`. Versions 1.0 to 1.8.6 write to `memory_version` naming their columns, so they
/// keep opening and writing the database (their new versions carry `'{}'`).
fn update_schema_10(db: &Connection) -> Result<()> {
    if schema_version(db)? == 10 {
        for (table, column, default) in [("project", "hlc", "''"), ("project", "paused", "''"), ("memory_version", "extra", "'{}'")] {
            let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)", [table, column], |r| r.get(0))?;
            if !has {
                db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT {default}"))?;
            }
        }
    }
    Ok(())
}

fn migrate(db: &mut Connection) -> Result<()> {
    migrate_to(db, VERSION_ESQUEMA)
}

/// A database at an old schema (`target`), without migrating further: to test the migrations with
/// databases like the ones people have. Not for real use: this version only knows how to work with
/// the latest.
#[doc(hidden)]
pub fn abrir_en_esquema(path: &Path, target: i64) -> Result<Connection> {
    registrar_sqlite_vec();
    let mut db = Connection::open(path)?;
    db.busy_handler(Some(wait_turn))?;
    migrate_to(&mut db, target)?;
    Ok(db)
}

fn migrate_to(db: &mut Connection, target: i64) -> Result<()> {
    let backup = db.path().filter(|p| !p.is_empty()).map(|r| format!("{r}.antes-del-esquema-{VERSION_ESQUEMA}"));
    migrate_with_backup(db, target, backup)
}

/// Migrates up to `target`, with the prior copy at `backup` (none for an in-memory database).
fn migrate_with_backup(db: &mut Connection, target: i64, backup: Option<String>) -> Result<()> {
    let before = schema_version(db)?;
    if before == target {
        return Ok(());
    }
    if before > VERSION_ESQUEMA {
        return check_reader(db, before);
    }
    // Before migrating a database that already had data, a copy: an old version doesn't open the new
    // one. If the copy fails (disk full, permissions), it isn't migrated: without it there's no way back.
    if before > 0
        && let Some(backup) = backup
        && !backup_ok(Path::new(&backup))
    {
        // To a temporary name and then renamed: a copy cut halfway (the process dies, the disk fills
        // up) never ends up with the good one's name.
        let tmp = format!("{backup}.tmp");
        let _ = std::fs::remove_file(&tmp);
        let failure = || {
            tr!(
                "can't copy the database to {backup} before updating it to schema {VERSION_ESQUEMA}; nothing was changed",
                "no se puede copiar la base en {backup} antes de pasarla al esquema {VERSION_ESQUEMA}; no se ha cambiado nada"
            )
        };
        db.execute("VACUUM INTO ?1", [&tmp]).with_context(failure)?;
        std::fs::rename(&tmp, &backup).with_context(failure)?;
    }
    // Foreign keys, turned off before the BEGIN (they can't be changed inside a transaction): with them
    // on, schema 10's `DROP TABLE memory` would cascade-delete the history, the paths and the vectors.
    // `abrir` turns them back on at the end.
    db.pragma_update(None, "foreign_keys", false)?;
    // IMMEDIATE: if two processes open a new database at once, only one creates the schema.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let v = schema_version(&tx)?;
    if v >= target {
        // Another process migrated it while waiting for the lock (or it's from a newer version).
        check_reader(&tx, v)?;
        return Ok(());
    }
    if v < 1 && 1 <= target {
        tx.execute_batch(SCHEMA_1)?;
    }
    if v < 2 && 2 <= target {
        tx.execute_batch(SCHEMA_2)?;
        // The new FTS index (with paths) is filled from the table.
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, tags, paths)
             SELECT n, title, content,
                    (SELECT COALESCE(group_concat(value, ' '), '') FROM json_each(memory.tags)),
                    (SELECT COALESCE(group_concat(value, ' '), '') FROM json_each(memory.paths))
             FROM memory",
            [],
        )?;
    }
    if v < 3 && 3 <= target {
        tx.execute_batch(SCHEMA_3)?;
    }
    if v < 4 && 4 <= target {
        tx.execute_batch(SCHEMA_4)?;
    }
    if v < 5 && 5 <= target {
        tx.execute_batch(SCHEMA_5)?;
    }
    if v < 6 && 6 <= target {
        tx.execute_batch(SCHEMA_6)?;
    }
    if v < 7 && 7 <= target {
        tx.execute_batch(SCHEMA_7)?;
        loosen_kinds(&tx)?;
    }
    if v < 8 && 8 <= target {
        tx.execute_batch(SCHEMA_8)?;
        move_dismissals(&tx)?;
    }
    if v < 9 && 9 <= target {
        tx.execute_batch(SCHEMA_9)?;
    }
    if v < 10 && 10 <= target {
        // The person of the `dir:` keys before computing them: never `dir:/<folder>`.
        let person = ensure_person(&tx)?;
        schema_10(&tx, &person)?;
    }
    // With foreign keys off nothing has checked them: they're checked before accepting the migration.
    let bad: usize = {
        let mut q = tx.prepare("PRAGMA foreign_key_check")?;
        let mut rows = q.query([])?;
        let mut n = 0;
        while rows.next()?.is_some() {
            n += 1;
        }
        n
    };
    anyhow::ensure!(bad == 0, "schema {target}: {bad} rows break a foreign key; nothing was changed");
    tx.pragma_update(None, "user_version", target)?;
    tx.commit()?;
    Ok(())
}

/// Schema 10 (0.20.0), the one 1.0 freezes:
/// - **Projects and spaces as tables** (`space`, `project`, `project_path`): the memory stores its
///   project's id instead of the path, and each project its sync key (computed once, when it's
///   created) and every folder where it lives (clones, worktrees, the folder it moved to, or the path
///   another computer brought, `is_foreign`). A memory belongs to a project (`project_id`), to a
///   space (`space_id`, without a project) or is global (neither): the `CHECK` constraint prevents
///   both at once.
/// - `kind` without `CHECK`: a kind arriving from a future version is stored as is.
/// - `content_hlc`: the stamp of the last change of title, content or kind, which verdicts expire
///   with (before, by comparing wall clocks of different computers).
/// - `extra` in `memory` and `verdict`: fields from future versions this one doesn't understand, so
///   as not to lose them when re-exporting.
/// - `review` is again only a cache (without `dismissed_at`, which already lived in `verdict`), and
///   `remote_project` moves to `project_path`.
/// - `project.hlc`: the stamp of the project's last space change (empty if it never changed), with
///   which its space travels as a `project` line. That line never changes anything's space: if it
///   states another, the project is paused (`project.paused`, local: JSON with the space it states,
///   its stamp and which computer) until the person decides here (`omoi projects move` or `omoi
///   projects keep`). Also paused (`claimed`) is the project of a space that a folder here joins when
///   this computer had none of its own: it's in that space only because its repository says so.
/// - `tombstone.space_key`: the space of what was forgotten, so that each forget goes only to its
///   space's repository (older ones, empty: to the general one). And `meta('persona')`, the one of the
///   `dir:` keys.
/// - Local, not travelling: the project from which each memory was received (`recall_log.project_id`)
///   and its summary when the log expires (`memory_use`).
const SCHEMA_10: &str = "
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
  created_at TEXT NOT NULL,
  hlc        TEXT NOT NULL DEFAULT '',
  paused     TEXT NOT NULL DEFAULT ''
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
-- The space of each forget (its key; empty: none or from before): which repository it goes to.
ALTER TABLE tombstone ADD COLUMN space_key TEXT NOT NULL DEFAULT '';
ALTER TABLE recall_log ADD COLUMN project_id INTEGER NOT NULL DEFAULT 0;
CREATE TABLE memory_use (
  memory_id  TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL,
  veces      INTEGER NOT NULL DEFAULT 0,
  ultima     TEXT NOT NULL,
  PRIMARY KEY (memory_id, project_id)
) WITHOUT ROWID;

-- Whatever an older database had dangling (with foreign keys off at some point) must not stop the
-- migration: it's removed before checking them.
DELETE FROM memory_version WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM memory_path WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM recall_log WHERE memory_id NOT IN (SELECT id FROM memory);
DELETE FROM memory_vec WHERE n NOT IN (SELECT n FROM memory);
";

/// The new `memory` table, with the 12 steps of lang_altertable.html: create `memory_nueva`, copy
/// keeping `n` (so `memory_fts` and `memory_vec`, which go by `n`, stay valid), drop the old one,
/// rename and rebuild the indexes (there are no triggers or views). `mig_proyecto` (old path → id) is
/// filled by `move_to_projects`.
const SCHEMA_10_MEMORY: &str = "
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
-- content_hlc: the last text change was at updated_at (pinning and moving don't touch it). As a stamp
-- with counter 0 and no computer, a verdict that was valid (updated_at <= at) stays valid: its stamp
-- is never earlier than its time.
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

/// Whether there's a valid prior copy at `path`: it opens and has tables. An empty or damaged one
/// (from a migration cut short in a version that copied straight to the final name) is redone.
fn backup_ok(path: &Path) -> bool {
    path.exists()
        && Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| c.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'table'", [], |r| r.get::<_, i64>(0)))
            .is_ok_and(|n| n > 0)
}

fn schema_10(tx: &Connection, person: &str) -> Result<()> {
    tx.execute_batch(SCHEMA_10)?;
    normalize_stamps(tx)?;
    move_to_projects(tx, person)?;
    tx.execute_batch(SCHEMA_10_MEMORY)?;
    tx.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('lector_minimo', ?1)", [LECTOR_MINIMO.to_string()])?;
    Ok(())
}

/// Schema 5's stamps carry the counter with 4 digits (`%013d-0000-`); today's, with 10. They compared
/// fine with `reloj::comparar`, but not as text (`max(hlc)`, `w.hlc > v.hlc`): now they all have the
/// same width and comparing as text is comparing stamps.
fn normalize_stamps(tx: &Connection) -> Result<()> {
    // (table, columns that identify the row)
    for (table, key) in [("memory", "n"), ("tombstone", "id"), ("forgotten", "id"), ("verdict", "memory_id || char(31) || other_id || char(31) || relation")] {
        let rows: Vec<(String, String)> = {
            let mut q = tx.prepare(&format!("SELECT CAST({key} AS TEXT), hlc FROM {table} WHERE hlc != ''"))?;
            q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut u = tx.prepare(&format!("UPDATE {table} SET hlc = ?2 WHERE CAST({key} AS TEXT) = ?1"))?;
        for (k, hlc) in rows {
            if let Some(new) = crate::reloj::normalizado(&hlc) {
                u.execute(rusqlite::params![k, new])?;
            }
        }
    }
    let last: Option<String> = tx.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |r| r.get(0)).ok();
    if let Some(new) = last.as_deref().and_then(crate::reloj::normalizado) {
        tx.execute("UPDATE meta SET v = ?1 WHERE k = 'hlc'", [new])?;
    }
    Ok(())
}

/// Each path in `memory.project` becomes a project with its key:
/// - if it came from another computer (`remote_project`), the key it came with, as a foreign path;
/// - if it exists on disk, the usual one (`sync::clave`: git's `origin`, or `dir:<person>/<folder>`);
/// - if it no longer exists, the `dir:` one (without the `origin` nothing more can be known), and it's
///   left without `last_seen`: it's a project that "is gone" (Review shows it with `omoi projects move`).
///
/// Paths with the same `git:` key are the same project (clones, worktrees); with the same `dir:`, only
/// one local path keeps the key (the one that exists; foreign ones join it, as `sync::adoptar` did),
/// and the others, different folders with the same name, get `~2`, `~3`…
fn move_to_projects(tx: &Connection, person: &str) -> Result<()> {
    tx.execute_batch("CREATE TEMP TABLE mig_proyecto (path TEXT PRIMARY KEY, project_id INTEGER NOT NULL);")?;
    let remotes: std::collections::HashMap<String, String> = {
        let mut q = tx.prepare("SELECT path, key FROM remote_project")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    // (path, memories), those with most memories first: all else equal, that one keeps the key.
    let paths: Vec<(String, i64)> = {
        let mut q = tx.prepare("SELECT project, count(*) FROM memory WHERE project != '' GROUP BY project ORDER BY count(*) DESC, project")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    struct PathInfo {
        path: String,
        key: String,
        foreign: bool,
        exists: bool,
        origin: String,
    }
    let mut groups: Vec<(String, Vec<PathInfo>)> = Vec::new();
    for (path, _) in paths {
        let r = match remotes.get(&path) {
            Some(k) => PathInfo { key: k.clone(), foreign: true, exists: false, origin: String::new(), path },
            None => {
                let exists = crate::proyecto::es_carpeta(Path::new(&path));
                PathInfo { key: crate::sync::clave(&path, person), foreign: false, exists, origin: crate::proyecto::origen(&path), path }
            }
        };
        match groups.iter_mut().find(|(k, _)| *k == r.key) {
            Some((_, v)) => v.push(r),
            None => groups.push((r.key.clone(), vec![r])),
        }
    }
    let now: String = tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))?;
    let mut keys: std::collections::HashSet<String> = groups.iter().map(|(k, _)| k.clone()).collect();
    for (key, mut paths) in groups {
        // Those that exist, first; within that, the previous order (most memories first).
        paths.sort_by_key(|r| (r.foreign, !r.exists));
        let mut projects: Vec<(String, Vec<PathInfo>)> = vec![(key.clone(), Vec::new())];
        for r in paths {
            let first_local = projects[0].1.iter().any(|x| !x.foreign);
            if key.starts_with("dir:") && !r.foreign && first_local {
                // Another folder with the same name: a separate project, with its suffix.
                let mut n = 2;
                while keys.contains(&format!("{key}~{n}")) {
                    n += 1;
                }
                let k = format!("{key}~{n}");
                keys.insert(k.clone());
                projects.push((k, vec![r]));
            } else {
                projects[0].1.push(r);
            }
        }
        for (k, rs) in projects {
            let main = rs.iter().find(|r| !r.foreign && r.exists).or_else(|| rs.iter().find(|r| !r.foreign)).or(rs.first()).map(|r| r.path.clone()).unwrap_or_default();
            let name = crate::proyecto::nombre_de(&main, &k);
            tx.execute(
                "INSERT INTO project(key, name, path, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![k, name, main, now],
            )?;
            let id = tx.last_insert_rowid();
            for r in rs {
                tx.execute(
                    "INSERT OR IGNORE INTO project_path(path, path_cmp, project_id, origin, is_foreign, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![r.path, crate::proyecto::comparable(&r.path), id, r.origin, r.foreign, r.exists.then_some(&now)],
                )?;
                tx.execute("INSERT OR REPLACE INTO mig_proyecto(path, project_id) VALUES (?1, ?2)", rusqlite::params![r.path, id])?;
            }
        }
    }
    Ok(())
}

/// The `procedure` kind (Phase 7.3) didn't fit `memory`'s `CHECK`. SQLite doesn't change a `CHECK`
/// with `ALTER TABLE`; to loosen it without touching the data, its documentation (lang_altertable.html,
/// "other schema changes") rewrites the definition with `writable_schema` and bumps `schema_version`.
fn loosen_kinds(tx: &Connection) -> Result<()> {
    let sql: String = tx.query_row("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'memory'", [], |r| r.get(0))?;
    let old = "CHECK (kind IN ('fact','decision','gotcha','preference'))";
    if !sql.contains(old) {
        return Ok(());
    }
    let new = sql.replace(old, "CHECK (kind IN ('fact','decision','gotcha','preference','procedure'))");
    let version: i64 = tx.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
    tx.execute_batch("PRAGMA writable_schema = ON")?;
    tx.execute("UPDATE sqlite_schema SET sql = ?1 WHERE type = 'table' AND name = 'memory'", [&new])?;
    tx.execute_batch(&format!("PRAGMA schema_version = {}; PRAGMA writable_schema = OFF", version + 1))?;
    Ok(())
}

/// Each verdict with its reason and its author (0.19.0): the `reason` of the `update` or the title of
/// the new memory, and the sync's person, `agent:<client>` or `user`. Older ones keep both empty; old
/// versions ignore the fields in sync's `verdict` line.
const SCHEMA_9: &str = "
ALTER TABLE verdict ADD COLUMN reason TEXT NOT NULL DEFAULT '';
ALTER TABLE verdict ADD COLUMN author TEXT NOT NULL DEFAULT '';
";

/// Verdicts (0.17.0, `veredictos`): what the agent or the user say about a memory or a pair
/// (`superseded`, `contradicts`, `compatible`, `ok:<warning>`). They travel through sync, so they
/// carry their stamp; for a pair the newest wins.
const SCHEMA_8: &str = "
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

/// The "it's fine as is" of the `review` table become verdicts: that of an outdated pair,
/// `compatible`; that of a single memory's warning, `ok:<warning>`.
fn move_dismissals(tx: &Connection) -> Result<()> {
    let rows: Vec<(String, String, String, String)> = {
        let mut q = tx.prepare("SELECT memory_id, kind, other_id, dismissed_at FROM review WHERE dismissed_at IS NOT NULL")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, kind, other, at) in rows {
        let relation = if other.is_empty() { format!("ok:{kind}") } else { "compatible".to_string() };
        let hlc = crate::reloj::sello(tx)?;
        tx.execute("INSERT OR IGNORE INTO verdict(memory_id, other_id, relation, at, hlc) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![id, other, relation, at, hlc])?;
    }
    tx.execute("DELETE FROM review WHERE dismissed_at IS NOT NULL", [])?;
    Ok(())
}

/// Candidates drawn from conversations (Phase 7.2, `candidatas`), which the next session's agent
/// saves or drops; and the `[[name]]` links between memories (Phase 7.4, `enlaces`).
const SCHEMA_7: &str = "
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

/// Review (Phase 7.1, `revisar`): the memories a newer one contradicts, and what the user accepted
/// ("it's fine as is", `dismissed_at`), which holds until the memory changes again.
const SCHEMA_6: &str = "
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

/// Sync between computers: a hybrid logical clock per memory (`hlc`, see `reloj`), who wrote it
/// (`author`) and the marks of what was forgotten, so another computer doesn't bring it back. The
/// memories already there take their stamp from `updated_at`, without a computer (so any later change wins).
const SCHEMA_5: &str = "
ALTER TABLE memory ADD COLUMN hlc TEXT NOT NULL DEFAULT '';
ALTER TABLE memory ADD COLUMN author TEXT NOT NULL DEFAULT '';
UPDATE memory SET hlc = printf('%013d-0000-', CAST(round((julianday(updated_at) - 2440587.5) * 86400000) AS INTEGER));
CREATE TABLE tombstone (
  id         TEXT PRIMARY KEY,
  deleted_at TEXT NOT NULL,
  hlc        TEXT NOT NULL
);
-- Projects that arrived from another computer with a path that doesn't exist here: as soon as a
-- project with the same key shows up here, their memories move to that path.
CREATE TABLE remote_project (
  path TEXT PRIMARY KEY,
  key  TEXT NOT NULL
);
-- What another computer (maybe another person) forgot: a copy with its history, for `omoi sync restore`.
CREATE TABLE forgotten (
  id         TEXT PRIMARY KEY,
  memory     TEXT NOT NULL,
  versions   TEXT NOT NULL DEFAULT '[]',
  hlc        TEXT NOT NULL DEFAULT '',
  deleted_at TEXT NOT NULL,
  by         TEXT NOT NULL DEFAULT ''
);
";

/// `n` is the stable link with the FTS index (and in phase 2 with the vector one); `id` is the ULID
/// the agent and the user see.
const SCHEMA_1: &str = "
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

-- No content of its own (the text is already in memory) and with delete by rowid.
CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags,
  content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
";

/// When each session started and how many messages it has, to remind the agent to save what it
/// learns if in a long session it hasn't saved anything (the idea comes from engram).
const SCHEMA_4: &str = "
CREATE TABLE session (
  session     TEXT PRIMARY KEY,
  project     TEXT NOT NULL,
  started_at  TEXT NOT NULL,
  prompts     INTEGER NOT NULL DEFAULT 0,
  reminded_at TEXT
);
";

/// Phase 4: which memories have already been injected in each session, so as not to repeat them in
/// every message.
const SCHEMA_3: &str = "
CREATE TABLE recall_log (
  session   TEXT NOT NULL,
  memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  at        TEXT NOT NULL,
  PRIMARY KEY (session, memory_id)
);
CREATE INDEX recall_log_at ON recall_log(at);
";

/// Phase 2: paths enter the search (FTS5 doesn't allow adding columns: the index is rebuilt) and the
/// vectors go in a normal table. A BLOB with `vec_distance_cosine` filters by project in the same SQL
/// and was twice as fast as `vec0` (which can't filter inside the KNN).
const SCHEMA_2: &str = "
DROP TABLE memory_fts;
CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, content, tags, paths,
  content='', contentless_delete=1,
  tokenize='unicode61 remove_diacritics 2'
);

-- Vector of title + content, normalized. The model they were computed with goes in meta('modelo').
CREATE TABLE memory_vec (
  n INTEGER PRIMARY KEY REFERENCES memory(n) ON DELETE CASCADE,
  e BLOB NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_from_1_to_2_and_reindexes() {
        let dir = crate::comun::Temporal::nueva("db-migra");
        let path = dir.join("o.db");
        {
            // A phase 1 database, by hand.
            let db = Connection::open(&path).unwrap();
            db.execute_batch(SCHEMA_1).unwrap();
            db.execute_batch(
                "INSERT INTO memory(n, id, kind, title, content, paths, tags, created_at, updated_at)
                 VALUES (7, 'X', 'gotcha', 'Trampa', 'texto', '[\"src/auth.rs\"]', '[\"seguridad\"]', 'a', 'a');
                 INSERT INTO memory_fts(rowid, title, content, tags) VALUES (7, 'Trampa', 'texto', 'seguridad');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = abrir(&path).unwrap();
        assert_eq!(schema_version(&db).unwrap(), VERSION_ESQUEMA);
        let count = |q: &str| -> i64 {
            db.query_row("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH ?1", [q], |r| r.get(0)).unwrap()
        };
        assert_eq!(count("auth"), 1);
        assert_eq!(count("seguridad"), 1);
        assert_eq!(count("trampa"), 1);
        drop(db);
    }

    #[test]
    fn without_a_prior_copy_it_does_not_migrate() {
        let dir = crate::comun::Temporal::nueva("db-sin-copia");
        let path = dir.join("o.db");
        abrir_en_esquema(&path, 9).unwrap();
        let mut db = Connection::open(&path).unwrap();
        // A copy that can't be written (its folder doesn't exist): the database stays as it was.
        let impossible = dir.join("no-existe").join("copia.db").display().to_string();
        let e = migrate_with_backup(&mut db, VERSION_ESQUEMA, Some(impossible)).unwrap_err();
        assert!(format!("{e:#}").contains("copia") || format!("{e:#}").contains("copy"), "{e:#}");
        assert_eq!(schema_version(&db).unwrap(), 9);
        drop(db);
        // With the copy possible, it does.
        let db = abrir(&path).unwrap();
        assert_eq!(schema_version(&db).unwrap(), VERSION_ESQUEMA);
        assert!(dir.join("o.db.antes-del-esquema-10").exists());
        drop(db);
    }

    #[test]
    fn redoes_a_half_written_prior_copy() {
        let dir = crate::comun::Temporal::nueva("db-copia-medias");
        let path = dir.join("o.db");
        abrir_en_esquema(&path, 9).unwrap().execute_batch("INSERT INTO meta(k, v) VALUES ('marca', 'de la 9')").unwrap();
        // A cut copy (from a version that copied straight to the final name): no tables.
        let backup = dir.join("o.db.antes-del-esquema-10");
        std::fs::write(&backup, b"").unwrap();
        let db = abrir(&path).unwrap();
        assert_eq!(schema_version(&db).unwrap(), VERSION_ESQUEMA);
        drop(db);
        let c = Connection::open(&backup).unwrap();
        assert_eq!(schema_version(&c).unwrap(), 9, "la copia es la base de antes");
        assert_eq!(c.query_row("SELECT v FROM meta WHERE k = 'marca'", [], |r| r.get::<_, String>(0)).unwrap(), "de la 9");
        drop(c);
        assert!(!dir.join("o.db.antes-del-esquema-10.tmp").exists(), "sin el temporal");
    }

    #[test]
    fn creates_and_reopens() {
        let dir = crate::comun::Temporal::nueva("db");
        let path = dir.join("sub").join("o.db");
        let db = abrir(&path).unwrap();
        assert_eq!(schema_version(&db).unwrap(), VERSION_ESQUEMA);
        drop(db);
        let db = abrir(&path).unwrap();
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        drop(db);
    }

    /// On close, the WAL isn't moved into the database (each `omoi` paid two syncs to disk) and what
    /// was written is still there on reopening, even though the WAL wasn't moved.
    #[test]
    fn closing_does_not_checkpoint_the_wal() {
        let dir = crate::comun::Temporal::nueva("db-wal");
        let path = dir.join("o.db");
        let db = abrir(&path).unwrap();
        assert!(db.db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE).unwrap());
        db.execute("INSERT INTO meta(k, v) VALUES ('prueba-wal', 'sí')", []).unwrap();
        drop(db);
        assert!(dir.join("o.db-wal").metadata().unwrap().len() > 0);
        let db = abrir(&path).unwrap();
        let v: String = db.query_row("SELECT v FROM meta WHERE k = 'prueba-wal'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "sí");
        drop(db);
    }
}
