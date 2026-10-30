//! Copias fieles (0.18.0): `omoi export` escribe todo lo que hace falta para reconstruir la base
//! (memorias con su historial, marcas de olvido, veredictos y papelera) y `omoi import` lo vuelve a
//! meter con las mismas reglas que la sincronización (`sync::Aplicador`): los mismos ids, fechas y
//! sellos, gana el más nuevo y lo olvidado no resucita. Así un import es idempotente y sirve tanto
//! para restaurar una base como para juntar dos.
//!
//! Formato `omoi-export`, versión 2: JSONL, una cosa por línea y en streaming (una línea en memoria
//! cada vez, al escribir y al leer). La primera línea es la cabecera y la última el cierre, con
//! cuántas líneas van entre medias y su SHA-256: un fichero cortado o tocado no se importa.
//! Con `--encrypt`, cada línea va cifrada con la clave vigente de la sincronización (AES-SIV) y con su
//! id delante (`k2:…`; sin nada, la 1), como en el repositorio, para guardar la copia en cualquier
//! sitio. Al importarla vale cualquiera de las claves que tenga el equipo.
//!
//! Y la copia diaria: una vez al día, al empezar la primera sesión, `VACUUM INTO` a
//! `<carpeta de la base>/backups/omoikane-AAAA-MM-DD.db`, conservando las últimas
//! `OMOIKANE_BACKUPS` (7; 0 = apagado), y de paso se vacía de la papelera lo que lleva más de
//! `OMOIKANE_TRASH_DAYS` (30; 0 = nunca).

use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, COLUMNAS, de_fila};
use crate::sync::{self, Aplicador, Cifra, Linea, Resumen};
use crate::tr;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub const FORMATO: &str = "omoi-export";
pub const VERSION: u32 = 2;

/// Cuántas líneas de cada cosa (para `--dry-run` y para contar lo exportado).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Cuenta {
    pub memories: usize,
    pub tombstones: usize,
    pub verdicts: usize,
    pub forgotten: usize,
}

impl Cuenta {
    pub fn total(&self) -> usize {
        self.memories + self.tombstones + self.verdicts + self.forgotten
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn cifra_de(ruta_db: &Path) -> Result<Cifra> {
    let r = sync::rutas(ruta_db);
    ensure!(r.clave.exists(), tr!("encrypting needs the sync key: set up `omoi sync` first", "para cifrar hace falta la clave de la sincronización: configura antes `omoi sync`"));
    sync::leer_claves(&r)
}

/// Escribe el fichero. Devuelve cuántas líneas van entre la cabecera y el cierre.
pub fn exportar(db: &Connection, ruta_db: &Path, ambito: &Ambito, cifrar: bool, salida: &mut impl Write) -> Result<Cuenta> {
    let cifra = if cifrar { Some(cifra_de(ruta_db)?) } else { None };
    let persona = sync::leer_config(ruta_db)?.map(|c| c.person).unwrap_or_default();
    // Las mismas claves que exporta la sincronización: una ruta que llegó de otro equipo lleva la clave
    // con la que llegó (`remote_project`), no una calculada aquí.
    let claves = sync::claves_locales(db, &persona)?;
    let equipo = crate::reloj::equipo(db)?;
    let cabecera = Linea::Header {
        format: FORMATO.into(),
        version: VERSION,
        schema: crate::db::VERSION_ESQUEMA,
        exported_at: db.query_row(&format!("SELECT {}", memoria::AHORA), [], |f| f.get(0))?,
        machine: equipo.clone(),
        encrypted: cifrar,
    };
    serde_json::to_writer(&mut *salida, &cabecera)?;
    salida.write_all(b"\n")?;
    let mut hash = Sha256::new();
    let mut cuenta = Cuenta::default();
    let mut escribir = |l: &Linea, salida: &mut dyn Write| -> Result<()> {
        let texto = serde_json::to_string(l)?;
        hash.update(texto.as_bytes());
        hash.update(b"\n");
        match &cifra {
            Some(c) => salida.write_all(c.cifrar(&texto)?.as_bytes())?,
            None => salida.write_all(texto.as_bytes())?,
        }
        salida.write_all(b"\n")?;
        Ok(())
    };
    // Primero las marcas de olvido (al importar se aplican antes que las memorias), luego las memorias
    // con su historial, los veredictos y la papelera.
    {
        let mut q = db.prepare("SELECT id, deleted_at, hlc FROM tombstone ORDER BY id")?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            escribir(&Linea::Tombstone { id: f.get(0)?, deleted_at: f.get(1)?, hlc: f.get(2)?, machine: equipo.clone() }, salida)?;
            cuenta.tombstones += 1;
        }
    }
    {
        let mut q = db.prepare(&format!("SELECT {COLUMNAS}, hlc FROM memory WHERE (?1 IS NULL OR project = ?1 OR project = '') ORDER BY id"))?;
        let mut filas = q.query([ambito.filtro()])?;
        while let Some(f) = filas.next()? {
            let m = de_fila(f)?;
            let hlc: String = f.get(14)?;
            let versions = memoria::historial(db, &m.id)?;
            let l = Linea::Memory {
                project_key: claves.get(&m.project).cloned().unwrap_or_else(|| sync::clave(&m.project, &persona)),
                project_path: m.project,
                id: m.id,
                kind: m.kind,
                title: m.title,
                content: m.content,
                paths: m.paths,
                tags: m.tags,
                pinned: m.pinned,
                created_at: m.created_at,
                updated_at: m.updated_at,
                hlc,
                author: m.author,
                machine: equipo.clone(),
                versions,
            };
            escribir(&l, salida)?;
            cuenta.memories += 1;
        }
    }
    for v in crate::veredictos::todos(db)? {
        escribir(&Linea::Verdict { memory_id: v.memory_id, other_id: v.other_id, relation: v.relation, at: v.at, hlc: v.hlc, reason: v.reason, author: v.author, machine: equipo.clone() }, salida)?;
        cuenta.verdicts += 1;
    }
    {
        let mut q = db.prepare("SELECT id, memory, versions, hlc, deleted_at, by FROM forgotten ORDER BY id")?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            let memoria: String = f.get(1)?;
            let versiones: String = f.get(2)?;
            let Ok(memory) = serde_json::from_str(&memoria) else { continue };
            let l = Linea::Forgotten {
                id: f.get(0)?,
                memory,
                versions: serde_json::from_str(&versiones).unwrap_or_default(),
                hlc: f.get(3)?,
                deleted_at: f.get(4)?,
                by: f.get(5)?,
            };
            escribir(&l, salida)?;
            cuenta.forgotten += 1;
        }
    }
    // El cierre va como las demás líneas (cifrado si ellas lo van), pero no entra en el SHA.
    let fin = serde_json::to_string(&Linea::End { count: cuenta.total(), sha256: hex(&hash.finalize()) })?;
    match &cifra {
        Some(c) => salida.write_all(c.cifrar(&fin)?.as_bytes())?,
        None => salida.write_all(fin.as_bytes())?,
    }
    salida.write_all(b"\n")?;
    Ok(cuenta)
}

/// Si el fichero empieza por la cabecera de este formato (lo demás es la exportación antigua o la de
/// engram, que lee `importar`).
pub fn es_propio(ruta: &Path) -> Result<bool> {
    let f = std::fs::File::open(ruta).with_context(|| format!("{}", ruta.display()))?;
    let mut primera = String::new();
    BufReader::new(f).read_line(&mut primera)?;
    let primera = primera.trim_start_matches('\u{feff}');
    Ok(matches!(serde_json::from_str::<Linea>(primera), Ok(Linea::Header { format, .. }) if format == FORMATO))
}

/// Lee el fichero línea a línea: la cabecera, cada línea descifrada si hace falta, y comprueba el
/// cierre. `f` recibe cada línea de en medio ya entendida.
fn leer(ruta_db: &Path, ruta: &Path, mut f: impl FnMut(Linea) -> Result<()>) -> Result<Cuenta> {
    let fichero = std::fs::File::open(ruta).with_context(|| format!("{}", ruta.display()))?;
    let mut lineas = BufReader::new(fichero).lines();
    let primera = lineas.next().transpose()?.unwrap_or_default();
    let cabecera: Linea = serde_json::from_str(primera.trim_start_matches('\u{feff}')).with_context(|| tr!("{} is not an omoi export", "{} no es una exportación de omoi", ruta.display()))?;
    let Linea::Header { format, version, encrypted, .. } = cabecera else {
        bail!(tr!("{} is not an omoi export", "{} no es una exportación de omoi", ruta.display()));
    };
    ensure!(format == FORMATO && version <= VERSION, tr!("{} comes from a newer omoi (export format {version}; this one reads up to {VERSION})", "{} viene de un omoi más nuevo (formato {version}, este entiende hasta el {VERSION})", ruta.display()));
    let cifra = if encrypted { Some(cifra_de(ruta_db)?) } else { None };
    let mut hash = Sha256::new();
    let mut cuenta = Cuenta::default();
    let mut cierre: Option<(usize, String)> = None;
    for (i, l) in lineas.enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let claro = match &cifra {
            Some(c) => c.descifrar(&l).with_context(|| tr!("line {}", "línea {}", i + 2))?,
            None => l,
        };
        let x: Linea = serde_json::from_str(&claro).with_context(|| tr!("line {} can't be read", "la línea {} no se entiende", i + 2))?;
        if let Linea::End { count, sha256 } = x {
            cierre = Some((count, sha256));
            break;
        }
        ensure!(x.valida(), tr!("line {} has an invalid id", "la línea {} lleva un id no válido", i + 2));
        hash.update(claro.as_bytes());
        hash.update(b"\n");
        match &x {
            Linea::Memory { .. } => cuenta.memories += 1,
            Linea::Tombstone { .. } => cuenta.tombstones += 1,
            Linea::Verdict { .. } => cuenta.verdicts += 1,
            Linea::Forgotten { .. } => cuenta.forgotten += 1,
            Linea::Header { .. } | Linea::End { .. } => {}
        }
        f(x)?;
    }
    let Some((count, sha256)) = cierre else {
        bail!(tr!("{} is incomplete: it has no end line", "{} está incompleto: no tiene la línea de cierre", ruta.display()));
    };
    ensure!(count == cuenta.total() && sha256 == hex(&hash.finalize()), tr!("{} is damaged: its contents don't match its end line", "{} está dañado: su contenido no cuadra con la línea de cierre", ruta.display()));
    Ok(cuenta)
}

/// Importa un fichero de este formato. Primero lo lee entero para comprobarlo (sin aplicar nada), y
/// solo entonces aplica línea a línea. Con `dry_run`, solo cuenta.
pub fn importar(db: &mut Connection, ruta_db: &Path, ruta: &Path, actual: &str, modelo: Option<&Modelo>, dry_run: bool) -> Result<(Cuenta, Resumen)> {
    let cuenta = leer(ruta_db, ruta, |_| Ok(()))?;
    let mut r = Resumen::default();
    if dry_run {
        return Ok((cuenta, r));
    }
    let mut aplicador = Aplicador::nuevo(db, None, Some(actual))?;
    let nombre = ruta.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    leer(ruta_db, ruta, |l| {
        let origen = match &l {
            Linea::Memory { machine, .. } | Linea::Tombstone { machine, .. } if !machine.is_empty() => machine.clone(),
            _ => format!("import:{nombre}"),
        };
        if aplicador.aplicar(db, &origen, l, modelo, &mut r).is_err() {
            r.ilegibles += 1;
        }
        Ok(())
    })?;
    crate::revisar::al_dia(db)?;
    Ok((cuenta, r))
}

// --- Copia diaria ------------------------------------------------------------------------------------

/// Cuántas copias se conservan (`OMOIKANE_BACKUPS`; 0 = no se hacen).
pub fn copias_a_conservar() -> usize {
    std::env::var("OMOIKANE_BACKUPS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(7)
}

pub fn carpeta_copias(ruta_db: &Path) -> PathBuf {
    ruta_db.parent().map(Path::to_path_buf).unwrap_or_default().join("backups")
}

/// Las copias que hay, de la más nueva a la más vieja: (ruta, bytes).
pub fn copias(ruta_db: &Path) -> Vec<(PathBuf, u64)> {
    let mut v: Vec<(PathBuf, u64)> = std::fs::read_dir(carpeta_copias(ruta_db))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("omoikane-") && e.path().extension().is_some_and(|x| x == "db"))
                .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

/// Hace la copia de hoy (sustituye la de hoy si ya había) y borra las que sobren. Devuelve su ruta.
pub fn copiar(db: &Connection, ruta_db: &Path) -> Result<PathBuf> {
    let carpeta = carpeta_copias(ruta_db);
    std::fs::create_dir_all(&carpeta)?;
    let hoy: String = db.query_row("SELECT strftime('%Y-%m-%d', 'now')", [], |f| f.get(0))?;
    let destino = carpeta.join(format!("omoikane-{hoy}.db"));
    // `VACUUM INTO` no sobreescribe: se escribe aparte y se cambia de nombre al final.
    let tmp = carpeta.join(format!(".omoikane-{hoy}.db.tmp"));
    let _ = std::fs::remove_file(&tmp);
    db.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])?;
    let _ = std::fs::remove_file(&destino);
    std::fs::rename(&tmp, &destino)?;
    for (ruta, _) in copias(ruta_db).into_iter().skip(copias_a_conservar().max(1)) {
        let _ = std::fs::remove_file(ruta);
    }
    Ok(destino)
}

/// Al empezar la primera sesión del día: la copia y la purga de la papelera. Nunca falla ni se
/// repite el mismo día (lo apunta en `meta`).
pub fn diaria(db: &Connection, ruta_db: &Path) -> Option<PathBuf> {
    let hoy: String = db.query_row("SELECT strftime('%Y-%m-%d', 'now')", [], |f| f.get(0)).ok()?;
    let ultima: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'copia_dia'", [], |f| f.get(0)).optional().ok()?;
    if ultima.as_deref() == Some(hoy.as_str()) {
        return None;
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('copia_dia', ?1)", [&hoy]).ok()?;
    let _ = memoria::purgar(db, memoria::dias_papelera());
    if copias_a_conservar() == 0 {
        return None;
    }
    copiar(db, ruta_db).ok()
}
