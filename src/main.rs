//! `omoi`: memoria local para agentes de IA. Un binario, un fichero SQLite.
//!
//! Esqueleto: solo `doctor` hace algo. El resto de subcomandos se implementan en las fases del PLAN.md.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "omoi", version, about = "Memoria local para agentes de IA")]
struct Cli {
    #[command(subcommand)]
    orden: Orden,
}

#[derive(Subcommand)]
enum Orden {
    /// Comprueba que SQLite, FTS5 y sqlite-vec funcionan y dónde vive la base de datos.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.orden {
        Orden::Doctor => doctor(),
    }
}

fn doctor() -> Result<()> {
    // sqlite-vec se registra como extensión automática antes de abrir ninguna conexión.
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int,
        >(sqlite_vec::sqlite3_vec_init as *const ())));
    }
    let db = rusqlite::Connection::open_in_memory()?;
    let sqlite: String = db.query_row("select sqlite_version()", [], |r| r.get(0))?;
    let vec: String = db.query_row("select vec_version()", [], |r| r.get(0))?;
    db.execute_batch("create virtual table t using fts5(x, tokenize='unicode61 remove_diacritics 2');")?;
    db.execute_batch("create virtual table v using vec0(e float[4]);")?;
    let datos = dirs::home_dir().unwrap_or_default().join(".omoikane");
    println!("sqlite     {sqlite}");
    println!("sqlite-vec {vec}");
    println!("fts5       ok (unicode61, sin acentos)");
    println!("vec0       ok");
    println!("datos      {}", datos.display());
    Ok(())
}
