//! Copias fieles (0.18.0): `omoi export` escribe todo lo que hace falta para reconstruir la base
//! (memorias con su historial, marcas de olvido, veredictos y papelera) y `omoi import` lo vuelve a
//! meter con las mismas reglas que la sincronización (`sync::Aplicador`): los mismos ids, fechas y
//! sellos, gana el más nuevo y lo olvidado no resucita. Así un import es idempotente y sirve tanto
//! para restaurar una base como para juntar dos.
//!
//! Formato `omoi-export`, versión 2: JSONL, una cosa por línea y en streaming (una línea en memoria
//! cada vez, al escribir y al leer). La primera línea es la cabecera y la última el cierre, con
//! cuántas líneas van entre medias y su SHA-256: un fichero cortado o tocado no se importa. Las
//! líneas son las de la sincronización (`sync::Linea`) y crecen igual que ellas: un campo que esta
//! versión no conoce se guarda y se vuelve a exportar (`sync::Extra`), y una línea de un tipo que no
//! conoce entra en el SHA, se cuenta (`unknown`) y se salta, en vez de parar el import. La versión
//! del formato solo sube con un cambio que una versión vieja no pueda leer bien; añadir no la sube.
//! Va todo: las memorias, las marcas de olvido, la papelera (cada memoria con su proyecto y su
//! espacio por clave, como las vivas) y todos los veredictos, también los de lo que está en la
//! papelera (después de ella, para que al importar encuentren su memoria).
//! Con `--encrypt`, cada línea va cifrada con la clave vigente de la sincronización (AES-SIV) y con su
//! id delante (`k2:…`; sin nada, la 1), como en el repositorio, para guardar la copia en cualquier
//! sitio: la del repositorio del espacio del ámbito exportado (la copia de un cliente, con la clave de
//! ese cliente) o, sin espacio, la del general. Con `--all` (1.9.0), cada línea con la de su espacio:
//! lo de acme con la de acme, lo de beta con la de beta y lo que no es de ningún espacio con la del
//! general; si falta la clave de alguno, error antes de escribir nada (antes iba todo con la del
//! general). El formato no cambia: al importarla, cada línea se abre con la primera clave del equipo
//! que la abre, también en las versiones de antes, que ya lo hacían así. A un equipo al que le falte
//! alguna de esas claves no se le aplica nada: el import lee el fichero entero antes de aplicarlo y
//! falla en la primera línea que no abre.
//!
//! Y la copia diaria: una vez al día, al empezar la primera sesión, `VACUUM INTO` a
//! `<carpeta de la base>/backups/omoikane-AAAA-MM-DD.db`, conservando las últimas
//! `OMOIKANE_BACKUPS` (7; 0 = apagado), y de paso se vacía de la papelera lo que lleva más de
//! `OMOIKANE_TRASH_DAYS` (30; 0 = nunca).

use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, COLUMNAS, NCOL, de_fila};
use crate::sync::{self, Aplicador, Cifra, Linea, Resumen};
use crate::tr;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::collections::HashSet;
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
    /// Líneas `project` (las de la sincronización; un import no las aplica).
    #[serde(skip_serializing_if = "is_zero")]
    pub projects: usize,
    /// Líneas de un tipo que esta versión no conoce (de un omoi más nuevo): se saltan.
    pub unknown: usize,
    /// Memorias con el sello de verificada por una persona que entran sin él (importando desde la shell
    /// de un agente). No cuenta en `total`: son memorias de `memories` o `forgotten`.
    #[serde(skip_serializing_if = "is_zero")]
    pub unverified: usize,
    /// Memorias con el sello de verificada de otra persona (no firmado por quien importa) que entran sin
    /// él, sin `--trust-verified`. Tampoco cuenta en `total`.
    #[serde(skip_serializing_if = "is_zero")]
    pub untrusted_verified: usize,
}

impl Cuenta {
    pub fn total(&self) -> usize {
        self.memories + self.tombstones + self.verdicts + self.forgotten + self.projects + self.unknown
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Con qué se cifra cada línea de una copia.
enum Cifrado {
    /// Sin `--encrypt`.
    Nada,
    /// Todo con las claves de un repositorio: el del espacio del ámbito, o el general.
    Una(Cifra),
    /// `--all`: cada línea con las del repositorio de su espacio (`""`: el general). El cierre, que no
    /// es de ningún espacio (ni lleva nada de ninguno), va con las del general o, sin él, con las del
    /// primer espacio. Un veredicto de una memoria de la que aquí no queda nada no sale.
    PorEspacio(std::collections::BTreeMap<String, Cifra>),
}

impl Cifrado {
    /// Las claves de una línea del espacio `espacio` (su clave; `None`: de ninguno concreto).
    fn para(&self, espacio: Option<&str>) -> Option<&Cifra> {
        match self {
            Cifrado::Nada => None,
            Cifrado::Una(c) => Some(c),
            Cifrado::PorEspacio(v) => match espacio.and_then(|e| v.get(e)) {
                Some(c) => Some(c),
                None => v.get("").or_else(|| v.values().next()),
            },
        }
    }

    fn cifrar(&self, espacio: Option<&str>, texto: &str) -> Result<String> {
        match self.para(espacio) {
            Some(c) => c.cifrar(texto),
            None => Ok(texto.to_string()),
        }
    }
}

/// Los espacios (su clave; `""`: ninguno) de todo lo que lleva una copia de todo: memorias vivas, en la
/// papelera y marcas de olvido. Los veredictos van con su memoria.
fn espacios_de_todo(db: &Connection) -> Result<std::collections::BTreeSet<String>> {
    let mut v = std::collections::BTreeSet::new();
    for consulta in [
        "SELECT DISTINCT COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '') FROM memory m",
        "SELECT DISTINCT space_key FROM tombstone",
    ] {
        let mut q = db.prepare(consulta)?;
        for e in q.query_map([], |f| f.get::<_, String>(0))? {
            v.insert(e?);
        }
    }
    let mut q = db.prepare("SELECT memory FROM forgotten")?;
    for t in q.query_map([], |f| f.get::<_, String>(0))? {
        v.insert(sync::espacio_de_papelera(db, &t?)?);
    }
    Ok(v)
}

/// `--all --encrypt`: las claves de cada espacio que lleva la copia (y las del general para lo que no
/// es de ninguno). Error, antes de escribir nada, con los espacios cuyo repositorio no está en este
/// equipo: lo de un cliente nunca va con la clave personal ni con la de otro cliente.
fn cifras_por_espacio(db: &Connection, ruta_db: &Path) -> Result<Cifrado> {
    let mut v = std::collections::BTreeMap::new();
    let general = sync::rutas(ruta_db);
    if general.clave.exists() {
        v.insert(String::new(), sync::leer_claves(&general)?);
    }
    let mut faltan = Vec::new();
    for espacio in espacios_de_todo(db)? {
        if espacio.is_empty() {
            if !v.contains_key("") {
                faltan.push(tr!("no space (the general repository)", "ninguno (el repositorio general)"));
            }
            continue;
        }
        match sync::repo_de_espacio(ruta_db, &espacio).filter(|r| r.clave.exists()) {
            Some(r) => {
                v.insert(espacio, sync::leer_claves(&r)?);
            }
            None => faltan.push(espacio),
        }
    }
    ensure!(
        faltan.is_empty(),
        tr!(
            "encrypting a copy of everything puts each space under the key of its own sync repository, and this computer has none for: {}. Set them up first (`omoi sync init <git url> --space <space>`, or `omoi sync init <git url>` for the general one) or export without --encrypt",
            "una copia cifrada de todo lleva cada espacio con la clave de su propio repositorio de sincronización, y este equipo no tiene la de: {}. Configúralos antes (`omoi sync init <url de git> --space <espacio>`, u `omoi sync init <url de git>` para el general) o exporta sin --encrypt",
            faltan.join(", ")
        )
    );
    ensure!(
        !v.is_empty(),
        tr!(
            "encrypting needs the key of the general sync repository: set up `omoi sync init <git url>` (or `omoi sync join <code>`) first",
            "para cifrar hace falta la clave del repositorio general de la sincronización: configura antes `omoi sync init <url de git>` (o `omoi sync join <código>`)"
        )
    );
    Ok(Cifrado::PorEspacio(v))
}

/// Las claves con las que se cifra una copia de este ámbito: las del repositorio de su espacio (desde
/// un proyecto de acme, las de acme: la copia de un cliente no se abre con la clave personal) o, desde
/// algo sin espacio, las del general. Con `--all`, las de cada espacio (`cifras_por_espacio`).
fn cifra_para(db: &Connection, ruta_db: &Path, ambito: &Ambito) -> Result<Cifra> {
    let espacio = crate::proyecto::clave_espacio(db, ambito.espacio())?;
    if espacio.is_empty() {
        let r = sync::rutas(ruta_db);
        ensure!(
            r.clave.exists(),
            tr!(
                "encrypting needs the key of the general sync repository: set up `omoi sync init <git url>` (or `omoi sync join <code>`) first",
                "para cifrar hace falta la clave del repositorio general de la sincronización: configura antes `omoi sync init <url de git>` (o `omoi sync join <código>`)"
            )
        );
        return sync::leer_claves(&r);
    }
    match sync::repo_de_espacio(ruta_db, &espacio).filter(|r| r.clave.exists()) {
        Some(r) => sync::leer_claves(&r),
        None => bail!(tr!(
            "encrypting a copy of the space {espacio} needs the key of its sync repository: set it up first with `omoi sync init <git url> --space {espacio}` (or `omoi sync join --space {espacio} <code>`)",
            "para cifrar una copia del espacio {espacio} hace falta la clave de su repositorio de sincronización: configúralo antes con `omoi sync init <url de git> --space {espacio}` (o `omoi sync join --space {espacio} <código>`)"
        )),
    }
}

/// Todas las claves de este equipo (la del general y las de cada espacio), para leer una copia cifrada
/// con cualquiera de ellas.
fn todas_las_cifras(ruta_db: &Path) -> Result<Vec<Cifra>> {
    let v: Vec<Cifra> = sync::repos(ruta_db).iter().filter(|r| r.clave.exists()).filter_map(|r| sync::leer_claves(r).ok()).collect();
    ensure!(!v.is_empty(), tr!("the copy is encrypted and this computer has no sync key: set up `omoi sync` first", "la copia va cifrada y este equipo no tiene ninguna clave de sincronización: configura antes `omoi sync`"));
    Ok(v)
}

/// Escribe el fichero. Devuelve cuántas líneas van entre la cabecera y el cierre.
pub fn exportar(db: &Connection, ruta_db: &Path, ambito: &Ambito, cifrar: bool, salida: &mut impl Write) -> Result<Cuenta> {
    let todo = matches!(ambito, Ambito::Todos);
    let cifra = match (cifrar, todo) {
        (false, _) => Cifrado::Nada,
        (true, true) => cifras_por_espacio(db, ruta_db)?,
        (true, false) => Cifrado::Una(cifra_para(db, ruta_db, ambito)?),
    };
    // Las mismas claves que exporta la sincronización: las de la tabla `project` (una ruta que llegó
    // de otro equipo lleva la clave con la que llegó, no una calculada aquí).
    let equipo = crate::reloj::equipo(db)?;
    let cabecera = Linea::Header {
        format: FORMATO.into(),
        version: VERSION,
        schema: crate::db::VERSION_ESQUEMA,
        exported_at: db.query_row(&format!("SELECT {}", memoria::AHORA), [], |f| f.get(0))?,
        machine: equipo.clone(),
        encrypted: cifrar,
        person: crate::db::persona_de(db),
    };
    serde_json::to_writer(&mut *salida, &cabecera)?;
    salida.write_all(b"\n")?;
    let mut hash = Sha256::new();
    let mut cuenta = Cuenta::default();
    // Cada línea, con las claves del espacio `espacio` (ver `Cifrado::para`).
    let mut escribir = |l: &Linea, espacio: Option<&str>, salida: &mut dyn Write| -> Result<()> {
        let texto = serde_json::to_string(l)?;
        hash.update(texto.as_bytes());
        hash.update(b"\n");
        salida.write_all(cifra.cifrar(espacio, &texto)?.as_bytes())?;
        salida.write_all(b"\n")?;
        Ok(())
    };
    // Sin `--all` (un proyecto o un espacio), de la papelera, los olvidos y los veredictos solo va lo de
    // ese ámbito: una copia de un cliente no lleva nada de otro. La papelera de este ámbito, primero:
    // los olvidos y los veredictos dependen de ella.
    let mut en_papelera: HashSet<String> = HashSet::new();
    let mut con_copia: HashSet<String> = HashSet::new();
    {
        let mut q = db.prepare("SELECT id, memory FROM forgotten")?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            let (id, texto): (String, String) = (f.get(0)?, f.get(1)?);
            if todo || de_papelera_en(db, ambito, &texto)? {
                en_papelera.insert(id.clone());
            }
            con_copia.insert(id);
        }
    }
    let espacio_del_ambito = crate::proyecto::clave_espacio(db, ambito.espacio())?;
    // Primero las marcas de olvido (al importar se aplican antes que las memorias), luego las memorias
    // con su historial, la papelera y al final los veredictos, que necesitan sus memorias (vivas o en
    // la papelera).
    {
        let mut q = db.prepare(&format!("SELECT t.id, t.deleted_at, t.hlc, t.space_key, {} FROM tombstone t ORDER BY t.id", sync::EXTRA_OLVIDO))?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            let (id, espacio, extra): (String, String, String) = (f.get(0)?, f.get(3)?, f.get(4)?);
            // De este ámbito: lo que está en su papelera o, sin copia en la papelera, lo de su espacio.
            if !todo && !en_papelera.contains(&id) && (con_copia.contains(&id) || espacio != espacio_del_ambito) {
                continue;
            }
            let l = Linea::olvido(id, f.get(1)?, f.get(2)?, espacio.clone(), &equipo, sync::extra_de(&extra));
            escribir(&l, Some(&espacio), salida)?;
            cuenta.tombstones += 1;
        }
    }
    // Las memorias que van (vivas o en la papelera): sus veredictos, con ellas.
    let mut van = en_papelera.clone();
    // El historial de las que una versión previa de la 1.8.0 promovió de un cliente con su id era texto
    // del cliente: solo sale en la copia de todo (como en `memoria::historial_en`).
    let sin_historial = if todo { Default::default() } else { crate::oficios::historiales_solo_aqui(db)? };
    {
        let mut q = db.prepare(&format!("SELECT {COLUMNAS}, m.hlc, m.content_hlc, m.extra FROM memory m WHERE {} ORDER BY m.id", ambito.sql()))?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            let m = de_fila(f)?;
            let (hlc, content_hlc, extra): (String, String, String) = (f.get(NCOL)?, f.get(NCOL + 1)?, f.get(NCOL + 2)?);
            let versions = if sin_historial.contains(&m.id) { Vec::new() } else { memoria::historial(db, &m.id)? };
            van.insert(m.id.clone());
            let espacio = m.space.clone();
            let l = sync::linea_de(m, hlc, content_hlc, &equipo, versions, sync::extra_de(&extra));
            escribir(&l, Some(&espacio), salida)?;
            cuenta.memories += 1;
        }
    }
    {
        let mut q = db.prepare("SELECT id, memory, versions, hlc, deleted_at, by FROM forgotten ORDER BY id")?;
        let mut filas = q.query([])?;
        while let Some(f) = filas.next()? {
            let id: String = f.get(0)?;
            if !en_papelera.contains(&id) {
                continue;
            }
            let (memoria, versiones): (String, String) = (f.get(1)?, f.get(2)?);
            let versions = serde_json::from_str(&versiones).unwrap_or_default();
            let espacio = if todo { sync::espacio_de_papelera(db, &memoria)? } else { String::new() };
            let Some(l) = Linea::papelera(db, id, &memoria, versions, f.get(3)?, f.get(4)?, f.get(5)?)? else { continue };
            escribir(&l, Some(&espacio), salida)?;
            cuenta.forgotten += 1;
        }
    }
    for v in crate::veredictos::todos(db)? {
        // Las dos memorias, del ámbito: un veredicto no se lleva a una copia que no tiene la otra.
        if !todo && !(van.contains(&v.memory_id) && (v.other_id.is_empty() || van.contains(&v.other_id))) {
            continue;
        }
        // Con la clave del espacio de su memoria: la de aquí, viva o en la papelera, o la de su marca de
        // olvido si se borró del todo. Si no se sabe, cifrado no sale: antes eso que la clave de otro
        // cliente o la personal.
        let espacio = if todo { espacio_del_veredicto(db, &v.memory_id)? } else { None };
        if todo && espacio.is_none() && !matches!(cifra, Cifrado::Nada) {
            continue;
        }
        escribir(&Linea::veredicto(v, &equipo), espacio.as_deref(), salida)?;
        cuenta.verdicts += 1;
    }
    // El cierre va como las demás líneas (cifrado si ellas lo van), pero no entra en el SHA.
    let fin = serde_json::to_string(&Linea::End { count: cuenta.total(), sha256: hex(&hash.finalize()) })?;
    salida.write_all(cifra.cifrar(None, &fin)?.as_bytes())?;
    salida.write_all(b"\n")?;
    Ok(cuenta)
}

/// El espacio (su clave; vacía: ninguno) del veredicto de la memoria `id`: el de la memoria, viva o en
/// la papelera, o el de su marca de olvido (`tombstone.space_key`) si se borró del todo. `None` si
/// aquí no queda nada de ella.
fn espacio_del_veredicto(db: &Connection, id: &str) -> Result<Option<String>> {
    if let Some(e) = sync::espacio_aqui(db, id)? {
        return Ok(Some(e));
    }
    Ok(db.query_row("SELECT space_key FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

/// Si una memoria de la papelera (`texto`, lo que guarda `forgotten.memory`) es de este ámbito: su
/// proyecto por la clave (o, en las de antes del esquema 10, por la ruta) o su espacio. Una cuyo
/// proyecto o espacio aquí ya no existe no es de ningún ámbito.
fn de_papelera_en(db: &Connection, ambito: &Ambito, texto: &str) -> Result<bool> {
    let Some((m, _, _)) = sync::de_papelera(texto) else { return Ok(false) };
    let proyecto = if !m.project_key.is_empty() {
        db.query_row("SELECT id FROM project WHERE key = ?1", [&m.project_key], |f| f.get::<_, i64>(0)).optional()?
    } else if !m.project.is_empty() {
        crate::proyecto::resolver(db, &m.project)?.map(|(id, _)| id)
    } else {
        Some(0)
    };
    let Some(proyecto) = proyecto else { return Ok(false) };
    let espacio = match (proyecto, m.space.is_empty()) {
        (0, false) => match db.query_row("SELECT id FROM space WHERE key = ?1", [&m.space], |f| f.get::<_, i64>(0)).optional()? {
            Some(s) => Some(s),
            None => return Ok(false),
        },
        _ => None,
    };
    ambito.incluye(db, proyecto, espacio)
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
    let cifras = if encrypted { todas_las_cifras(ruta_db)? } else { Vec::new() };
    let mut hash = Sha256::new();
    let mut cuenta = Cuenta::default();
    let mut cierre: Option<(usize, String)> = None;
    for (i, l) in lineas.enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let claro = if encrypted {
            // Con la primera clave que la abra; si no la abre ninguna, el error de la primera.
            let mut fallo = None;
            let claro = cifras.iter().find_map(|c| c.descifrar(&l).map_err(|e| fallo.get_or_insert(e)).ok());
            match claro {
                Some(t) => t,
                // (Una copia de todo lleva cada espacio con la clave de su repositorio: lo normal es que
                // a este equipo le falte la de alguno.)
                None => {
                    return Err(fallo.unwrap_or_else(|| anyhow::anyhow!("?"))).with_context(|| {
                        tr!(
                            "line {} doesn't open with any sync key of this computer, so nothing was imported. A copy of everything carries each space under the key of its own sync repository: join the repositories of the spaces it has (`omoi sync join --space <space> <code>`) and import it again",
                            "la línea {} no se abre con ninguna clave de sincronización de este equipo, así que no se ha importado nada. Una copia de todo lleva cada espacio con la clave de su propio repositorio: únete a los repositorios de los espacios que lleva (`omoi sync join --space <espacio> <código>`) e impórtala otra vez",
                            i + 2
                        )
                    });
                }
            }
        } else {
            l
        };
        // Una línea de un tipo que esta versión no conoce (de un omoi más nuevo) cuenta para el cierre
        // y se salta; las demás que no se entienden están dañadas.
        if sync::de_tipo_nuevo(&claro) {
            hash.update(claro.as_bytes());
            hash.update(b"\n");
            cuenta.unknown += 1;
            continue;
        }
        let x: Linea = serde_json::from_str(&claro).with_context(|| tr!("line {} can't be read", "la línea {} no se entiende", i + 2))?;
        if let Linea::End { count, sha256 } = x {
            cierre = Some((count, sha256));
            break;
        }
        ensure!(x.valida(), tr!("line {} has an invalid id", "la línea {} lleva un id no válido", i + 2));
        hash.update(claro.as_bytes());
        hash.update(b"\n");
        match &x {
            Linea::Memory(_) => cuenta.memories += 1,
            Linea::Tombstone { .. } => cuenta.tombstones += 1,
            Linea::Verdict { .. } => cuenta.verdicts += 1,
            Linea::Forgotten { .. } => cuenta.forgotten += 1,
            Linea::Project { .. } => cuenta.projects += 1,
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

/// Qué se hace con los sellos de verificada de un fichero (ver [`importar`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Sellos {
    /// Quien importa está en la shell de un agente (`verificado::shell_de_agente`): ninguno vale.
    pub agente: bool,
    /// `--trust-verified`: también los que no puso esta persona (se firman como suyos).
    pub confiar: bool,
}

/// Qué pasó con el sello de una línea.
#[derive(PartialEq, Eq)]
enum ConSello {
    /// No traía, o se queda.
    Nada,
    /// Quitado por la shell de un agente.
    Agente,
    /// Quitado por no ser de esta persona (sin `--trust-verified`).
    Ajeno,
}

/// El sello de verificada de una línea (`extra.verified`, el de la memoria o el de la que va a la
/// papelera): se queda si lo puso esta persona (su firma vale con su clave de sellos, ver
/// `verificado::Llaves`: su copia de seguridad, lo exportado en otro de sus equipos con su repositorio
/// general) o con `--trust-verified` (y entonces se firma como suyo); si no, o en la shell de un agente,
/// se quita.
fn filtrar_sello(l: &mut Linea, llaves: &crate::verificado::Llaves, s: Sellos) -> ConSello {
    let (id, extra) = match l {
        Linea::Memory(c) => (c.id.clone(), &mut c.extra),
        Linea::Forgotten { memory, .. } => (memory.id.clone(), &mut memory.extra),
        _ => return ConSello::Nada,
    };
    let Some(v) = extra.get_mut(crate::verificado::CLAVE) else { return ConSello::Nada };
    if s.agente {
        extra.remove(crate::verificado::CLAVE);
        return ConSello::Agente;
    }
    if crate::verificado::sello_propio(llaves, &id, v) {
        return ConSello::Nada;
    }
    if s.confiar {
        // (En seco no se crea la clave con la que firmarlo: se queda tal cual, y no se escribe.)
        crate::verificado::resellar(llaves, &id, v);
        return ConSello::Nada;
    }
    extra.remove(crate::verificado::CLAVE);
    ConSello::Ajeno
}

/// Importa un fichero de este formato. Primero lo lee entero para comprobarlo (sin aplicar nada), y
/// solo entonces aplica línea a línea. Con `dry_run`, solo cuenta.
///
/// Los sellos de verificada por una persona, con la misma regla que `import --okf`: el fichero lo puede
/// haber escrito cualquiera (y la huella del sello se calcula con el texto), así que solo se quedan los
/// que puso esta persona (firmados con su clave de sellos) y, con `--trust-verified`, los demás. Desde
/// la shell de un agente (`sellos.agente`), ninguno: sería que un agente se verificara a sí mismo. Los
/// quitados se cuentan en `Cuenta::unverified` (agente) y `Cuenta::untrusted_verified` (de otra
/// persona) para avisar. (La sincronización sí trae los sellos de los otros equipos de la persona, por
/// su repositorio cifrado: ver `sync::sello_de_otro`.)
pub fn importar(db: &mut Connection, ruta_db: &Path, ruta: &Path, actual: &str, modelo: Option<&Modelo>, dry_run: bool, sellos: Sellos) -> Result<(Cuenta, Resumen)> {
    let llaves = crate::verificado::llaves(db, sellos.confiar && !sellos.agente && !dry_run)?;
    let (mut de_agente, mut ajenos) = (0, 0);
    let mut cuenta = leer(ruta_db, ruta, |mut l| {
        match filtrar_sello(&mut l, &llaves, sellos) {
            ConSello::Agente => de_agente += 1,
            ConSello::Ajeno => ajenos += 1,
            ConSello::Nada => {}
        }
        Ok(())
    })?;
    cuenta.unverified = de_agente;
    cuenta.untrusted_verified = ajenos;
    let mut r = Resumen::default();
    if dry_run {
        return Ok((cuenta, r));
    }
    // En una base recién creada (sin memorias, sin papelera y sin sincronización), la persona de la
    // copia: es la misma persona restaurando su base, y sus proyectos sin remoto vuelven a sus carpetas.
    if let Some(persona) = persona_de_copia(ruta)?
        && sync::repos(ruta_db).is_empty()
        && db.query_row("SELECT NOT EXISTS(SELECT 1 FROM memory) AND NOT EXISTS(SELECT 1 FROM forgotten)", [], |f| f.get::<_, bool>(0))?
    {
        sync::adoptar_persona(db, ruta_db, &persona)?;
    }
    let mut aplicador = Aplicador::nuevo(db, None, Some(actual))?;
    let nombre = ruta.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    leer(ruta_db, ruta, |mut l| {
        filtrar_sello(&mut l, &llaves, sellos);
        let origen = match l.machine() {
            "" => format!("import:{nombre}"),
            m => m.to_string(),
        };
        if aplicador.aplicar(db, &origen, l, modelo, &mut r).is_err() {
            r.ilegibles += 1;
        }
        Ok(())
    })?;
    // La misma clave escrita en otro equipo sin ver esta: manda una (`memoria::resolver_claves_repetidas`).
    crate::memoria::resolver_claves_repetidas(db)?;
    crate::revisar::al_dia(db)?;
    Ok((cuenta, r))
}

/// La persona que dice la cabecera de un fichero de `omoi export` (las de antes no la llevan).
fn persona_de_copia(ruta: &Path) -> Result<Option<String>> {
    let f = std::fs::File::open(ruta).with_context(|| format!("{}", ruta.display()))?;
    let mut primera = String::new();
    BufReader::new(f).read_line(&mut primera)?;
    Ok(match serde_json::from_str::<Linea>(primera.trim_start_matches('\u{feff}')) {
        Ok(Linea::Header { person, .. }) if !person.is_empty() => Some(person),
        _ => None,
    })
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
