//! Memoria compartida por git: los equipos de una persona (o un grupo) juntan sus memorias en un
//! repositorio git suyo, sin servicio propio y sin coste. Omoikane solo usa el `git` del usuario, con
//! sus credenciales.
//!
//! - **Qué se sube:** texto, nunca el SQLite. Una línea por memoria (y por cada olvido), ordenadas y
//!   partidas por proyecto y por equipo: `<proyecto>/<equipo>.jsonl`, `global/<equipo>.jsonl` y
//!   `olvidos/<equipo>.jsonl`. Cada equipo solo escribe sus ficheros, así que git nunca tiene
//!   conflictos; al bajar se leen solo los ficheros que cambiaron desde la última vez.
//! - **Cifrado solo en el remoto:** cada línea va cifrada con AES-SIV (RFC 5297), que es determinista:
//!   una línea que no cambia queda igual byte a byte y el diff de git sigue siendo de una línea. Las
//!   carpetas de proyecto tienen un nombre opaco derivado de la clave. En el equipo todo está en claro.
//! - **Quién gana:** el sello del reloj lógico (`reloj`) más alto. La versión de aquí que pierde queda
//!   en el historial con el motivo `sync: <equipo>`. Los olvidos se aplican antes que las memorias, así
//!   que lo olvidado no resucita.
//! - **Proyectos en rutas distintas:** se reconocen por el remoto `origin` de git (o, sin remoto, por
//!   la persona y el nombre de la carpeta), y cada equipo guarda la memoria con su propia ruta.
//! - **Automático:** después de cada cambio, `programar` lanza `omoi sync --background` aparte; espera
//!   unos segundos para juntar cambios y solo corre uno a la vez por equipo.
//!
//! Todo vive junto a la base: `sync.json` (configuración), `sync.key` (clave), `sync/` (el clon),
//! `sync-estado.json`, `sync.pendiente` y `sync.lock`.

use crate::embeber::Modelo;
use crate::memoria::{self, COLUMNAS, Cambios, Memoria, Nueva, Origen, Tipo, de_fila};
use crate::{reloj, tr};
use aes_siv::KeyInit;
use aes_siv::siv::Aes256Siv;
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Cabecera asociada al cifrado: una línea de otro formato o versión no se descifra por accidente.
const CABECERA: &[u8] = b"omoikane-sync-v1";
const RAMA: &str = "main";
const PREFIJO_CODIGO: &str = "omoi1.";

// --- Rutas y configuración -------------------------------------------------------------------------

/// Los ficheros de la sincronización, junto a la base.
pub struct Rutas {
    pub config: PathBuf,
    pub clave: PathBuf,
    pub clon: PathBuf,
    pub estado: PathBuf,
    pub pendiente: PathBuf,
    pub cerrojo: PathBuf,
}

pub fn rutas(db: &Path) -> Rutas {
    let dir = db.parent().map(Path::to_path_buf).unwrap_or_default();
    Rutas {
        config: dir.join("sync.json"),
        clave: dir.join("sync.key"),
        clon: dir.join("sync"),
        estado: dir.join("sync-estado.json"),
        pendiente: dir.join("sync.pendiente"),
        cerrojo: dir.join("sync.lock"),
    }
}

/// Qué se comparte y cómo. Se decide en `omoi sync init` (o `join`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    /// Tipos que se suben (las preferencias, solo si se eligió compartirlas).
    pub kinds: Vec<Tipo>,
    /// Si se suben las memorias globales.
    pub global: bool,
    /// Proyectos que se suben, por su clave; ninguno = todos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<String>>,
    /// El nombre que se apunta como autor de cada memoria; ninguno = no se apunta.
    #[serde(default)]
    pub author: Option<String>,
    /// Para reconocer los proyectos sin remoto git: los de la misma persona se juntan entre sus
    /// equipos, los de dos personas no.
    pub person: String,
}

impl Config {
    fn comparte(&self, m: &Memoria, clave: &str) -> bool {
        if !self.kinds.contains(&m.kind) {
            return false;
        }
        if m.project.is_empty() {
            return self.global;
        }
        self.projects.as_ref().is_none_or(|p| p.iter().any(|k| k == clave))
    }
}

pub fn leer_config(db: &Path) -> Result<Option<Config>> {
    let r = rutas(db);
    if !r.config.exists() {
        return Ok(None);
    }
    let texto = std::fs::read_to_string(&r.config)?;
    Ok(Some(serde_json::from_str(&texto).with_context(|| format!("{}", r.config.display()))?))
}

fn leer_clave(r: &Rutas) -> Result<Vec<u8>> {
    let texto = std::fs::read_to_string(&r.clave).with_context(|| tr!("can't read the sync key {}", "no se puede leer la clave de sincronización {}", r.clave.display()))?;
    let k = desde_hex(texto.trim()).context(tr!("the sync key is damaged", "la clave de sincronización está dañada"))?;
    ensure!(k.len() == 64, tr!("the sync key is damaged", "la clave de sincronización está dañada"));
    Ok(k)
}

fn escribir_privado(ruta: &Path, texto: &str) -> Result<()> {
    std::fs::write(ruta, texto)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(ruta, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn a_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn desde_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

// --- Cifrado ---------------------------------------------------------------------------------------

struct Cifra {
    clave: Vec<u8>,
}

impl Cifra {
    fn siv(&self) -> Aes256Siv {
        Aes256Siv::new_from_slice(&self.clave).expect("clave de 64 bytes")
    }

    fn cifrar(&self, texto: &str) -> Result<String> {
        let c = self.siv().encrypt([CABECERA], texto.as_bytes()).map_err(|_| anyhow!("AES-SIV"))?;
        Ok(base64::engine::general_purpose::STANDARD_NO_PAD.encode(c))
    }

    fn descifrar(&self, linea: &str) -> Result<String> {
        let mal = || anyhow!(tr!(
            "can't decrypt the shared memories: this computer's key is not the repository's (join it with `omoi sync join <code>`)",
            "no se pueden descifrar las memorias compartidas: la clave de este equipo no es la del repositorio (únete con `omoi sync join <código>`)"
        ));
        let c = base64::engine::general_purpose::STANDARD_NO_PAD.decode(linea.trim()).map_err(|_| mal())?;
        let t = self.siv().decrypt([CABECERA], &c).map_err(|_| mal())?;
        String::from_utf8(t).map_err(|_| mal())
    }

    /// Nombre opaco de la carpeta de un proyecto: no dice qué proyecto es a quien no tiene la clave.
    fn carpeta(&self, clave_proyecto: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&self.clave);
        h.update(clave_proyecto.as_bytes());
        format!("p-{}", &a_hex(&h.finalize())[..16])
    }
}

// --- Formato de las líneas -------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Linea {
    Memory {
        id: String,
        /// Cómo se reconoce el proyecto en otro equipo (vacío: global).
        project_key: String,
        /// La ruta en el equipo de origen, por si aquí aún no hay ningún proyecto con esa clave.
        project_path: String,
        kind: Tipo,
        title: String,
        content: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        pinned: bool,
        created_at: String,
        updated_at: String,
        hlc: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        author: String,
    },
    Tombstone {
        id: String,
        deleted_at: String,
        hlc: String,
    },
}

// --- Proyectos ---------------------------------------------------------------------------------------

/// La clave de un proyecto para reconocerlo en otro equipo: el remoto `origin` de git, normalizado
/// (`git:github.com/dueño/repo`), o, sin remoto, la persona y el nombre de la carpeta
/// (`dir:<persona>/<carpeta>`), para que dos personas con una carpeta `web` no se mezclen.
pub fn clave(project: &str, persona: &str) -> String {
    if project.is_empty() {
        return String::new();
    }
    if let Some(url) = origen_git(Path::new(project)) {
        return format!("git:{}", normalizar_remoto(&url));
    }
    let ruta = Path::new(project);
    let nombre = ruta.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_else(|| project.to_lowercase());
    format!("dir:{persona}/{nombre}")
}

/// La URL del remoto `origin`, leyendo `.git/config` (rápido; en un worktree, el del repositorio).
fn origen_git(raiz: &Path) -> Option<String> {
    let punto_git = raiz.join(".git");
    let config = if punto_git.is_dir() {
        punto_git.join("config")
    } else {
        // Worktree: `.git` es un fichero «gitdir: <repo>/.git/worktrees/<nombre>».
        let t = std::fs::read_to_string(&punto_git).ok()?;
        let gitdir = PathBuf::from(t.trim().strip_prefix("gitdir:")?.trim());
        let comun = std::fs::read_to_string(gitdir.join("commondir")).ok().map(|c| gitdir.join(c.trim())).unwrap_or(gitdir);
        comun.join("config")
    };
    let texto = std::fs::read_to_string(config).ok()?;
    let mut en_origin = false;
    for l in texto.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            en_origin = l == "[remote \"origin\"]";
        } else if en_origin && let Some(resto) = l.strip_prefix("url") {
            return Some(resto.trim_start_matches([' ', '\t', '=']).trim().to_string());
        }
    }
    None
}

fn normalizar_remoto(url: &str) -> String {
    let mut u = url.trim().to_lowercase();
    for p in ["https://", "http://", "ssh://", "git://", "git+ssh://"] {
        if let Some(r) = u.strip_prefix(p) {
            u = r.to_string();
        }
    }
    if let Some((_, r)) = u.split_once('@') {
        u = r.to_string();
    }
    // `host:dueño/repo` (scp) y `host/dueño/repo` son lo mismo; un puerto (`host:22/…`) se quita.
    if let Some((host, resto)) = u.split_once(':') {
        let resto = resto.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('/');
        u = format!("{host}/{resto}");
    }
    u.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// Una persona como parte de una clave: minúsculas, letras y números.
pub fn persona(nombre: &str) -> String {
    let p: String = nombre.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let p = p.trim_matches('-').to_string();
    if p.is_empty() { "user".into() } else { p }
}

/// Si hay memorias que llegaron con la ruta de otro equipo y aquí aparece un proyecto con la misma
/// clave, pasan a la ruta de aquí. Barato si no hay ninguna pendiente.
pub fn adoptar(db: &Connection, project: &str, persona: &str) -> Result<usize> {
    let pendientes: i64 = db.query_row("SELECT count(*) FROM remote_project", [], |f| f.get(0))?;
    if pendientes == 0 || project.is_empty() {
        return Ok(0);
    }
    let k = clave(project, persona);
    let rutas: Vec<String> = {
        let mut q = db.prepare("SELECT path FROM remote_project WHERE key = ?1 AND path != ?2")?;
        q.query_map(params![k, project], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let mut n = 0;
    for r in rutas {
        n += db.execute("UPDATE memory SET project = ?2 WHERE project = ?1", params![r, project])?;
        db.execute("DELETE FROM remote_project WHERE path = ?1", [&r])?;
    }
    Ok(n)
}

// --- git ---------------------------------------------------------------------------------------------

/// Ejecuta git en el clon. Sin preguntas (en segundo plano no hay nadie para contestar).
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh -o BatchMode=yes".into()))
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| tr!("can't run git: is it installed?", "no se puede ejecutar git: ¿está instalado?"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())
    }
}

fn remoto_tiene_rama(clon: &Path) -> Result<bool> {
    Ok(!git(clon, &["ls-remote", "--heads", "origin", RAMA])?.trim().is_empty())
}

/// Clona el repositorio (vacío o no), deja la rama `main` y una identidad propia en el clon, para que
/// los commits no dependan de que el usuario tenga git configurado.
fn clonar(url: &str, clon: &Path, autor: Option<&str>) -> Result<()> {
    if clon.exists() {
        std::fs::remove_dir_all(clon).with_context(|| format!("{}", clon.display()))?;
    }
    let padre = clon.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(padre)?;
    let destino = clon.to_string_lossy().into_owned();
    git(padre, &["clone", "--quiet", url, &destino]).with_context(|| tr!("can't clone {url}", "no se puede clonar {url}"))?;
    if remoto_tiene_rama(clon)? {
        git(clon, &["checkout", "--quiet", "-B", RAMA, &format!("origin/{RAMA}")])?;
    } else {
        git(clon, &["symbolic-ref", "HEAD", &format!("refs/heads/{RAMA}")])?;
    }
    git(clon, &["config", "user.name", autor.unwrap_or("Omoikane")])?;
    git(clon, &["config", "user.email", "omoikane@localhost"])?;
    git(clon, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

// --- Exportar e importar -----------------------------------------------------------------------------

fn todas(db: &Connection) -> Result<Vec<(Memoria, String)>> {
    let mut q = db.prepare(&format!("SELECT {COLUMNAS}, hlc FROM memory ORDER BY id"))?;
    let v = q.query_map([], |f| Ok((de_fila(f)?, f.get::<_, String>(14)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// La clave de cada proyecto de esta base (las rutas que llegaron de fuera la tienen apuntada).
fn claves_locales(db: &Connection, persona: &str) -> Result<HashMap<String, String>> {
    let mut m: HashMap<String, String> = {
        let mut q = db.prepare("SELECT path, key FROM remote_project")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let proyectos: Vec<String> = {
        let mut q = db.prepare("SELECT DISTINCT project FROM memory WHERE project != ''")?;
        q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for p in proyectos {
        m.entry(p).or_insert_with_key(|p| clave(p, persona));
    }
    Ok(m)
}

/// Qué subiría este equipo (para el resumen de `init`): globales, de proyectos y cuántos proyectos.
pub fn resumen_de_subida(db: &Connection, config: &Config) -> Result<(usize, usize, usize)> {
    let claves = claves_locales(db, &config.person)?;
    let (mut globales, mut de_proyectos, mut proyectos) = (0, 0, HashSet::new());
    for (m, _) in todas(db)? {
        let k = claves.get(&m.project).cloned().unwrap_or_default();
        if config.comparte(&m, &k) {
            if m.project.is_empty() {
                globales += 1;
            } else {
                de_proyectos += 1;
                proyectos.insert(k);
            }
        }
    }
    Ok((globales, de_proyectos, proyectos.len()))
}

/// Escribe los ficheros de este equipo en el clon y borra los suyos que ya no tocan. Devuelve cuántas
/// memorias exportó.
fn exportar(db: &Connection, clon: &Path, equipo: &str, config: &Config, cifra: &Cifra) -> Result<usize> {
    let claves = claves_locales(db, &config.person)?;
    let mut ficheros: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut n = 0;
    for (m, hlc) in todas(db)? {
        let k = claves.get(&m.project).cloned().unwrap_or_default();
        if !config.comparte(&m, &k) {
            continue;
        }
        let carpeta = if m.project.is_empty() { "global".to_string() } else { cifra.carpeta(&k) };
        let l = Linea::Memory {
            id: m.id,
            project_key: k,
            project_path: m.project,
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
        };
        ficheros.entry(carpeta).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        n += 1;
    }
    {
        let mut q = db.prepare("SELECT id, deleted_at, hlc FROM tombstone ORDER BY id")?;
        let marcas: Vec<(String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, deleted_at, hlc) in marcas {
            let l = Linea::Tombstone { id, deleted_at, hlc };
            ficheros.entry("olvidos".into()).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        }
    }
    let nombre = format!("{equipo}.jsonl");
    // Los ficheros de este equipo que ya no tienen nada se borran.
    for e in std::fs::read_dir(clon)?.flatten() {
        let carpeta = e.file_name().to_string_lossy().into_owned();
        if carpeta.starts_with('.') || !e.path().is_dir() {
            continue;
        }
        let f = e.path().join(&nombre);
        if f.exists() && !ficheros.contains_key(&carpeta) {
            std::fs::remove_file(&f)?;
        }
    }
    for (carpeta, mut lineas) in ficheros {
        // Ordenadas (por texto cifrado, que es determinista): el mismo contenido da el mismo fichero.
        lineas.sort();
        let dir = clon.join(&carpeta);
        std::fs::create_dir_all(&dir)?;
        let destino = dir.join(&nombre);
        let texto = lineas.join("\n") + "\n";
        if std::fs::read_to_string(&destino).ok().as_deref() != Some(texto.as_str()) {
            let tmp = dir.join(format!(".{nombre}.tmp"));
            std::fs::write(&tmp, texto)?;
            std::fs::rename(&tmp, &destino)?;
        }
    }
    Ok(n)
}

/// Qué ficheros de otros equipos hay que leer: los que cambiaron desde la última sincronización, o
/// todos si es la primera (o si la base no recuerda por dónde iba).
fn por_leer(db: &Connection, clon: &Path, equipo: &str) -> Result<Vec<PathBuf>> {
    let propio = format!("{equipo}.jsonl");
    let ultimo: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'sync_commit'", [], |f| f.get(0)).optional()?;
    let cambiados: Option<Vec<String>> = match ultimo {
        Some(u) => git(clon, &["diff", "--name-only", &u, "HEAD"]).ok().map(|s| s.lines().map(String::from).collect()),
        None => None,
    };
    let mut v: Vec<PathBuf> = match cambiados {
        Some(lista) => lista.into_iter().map(|f| clon.join(f)).filter(|p| p.exists()).collect(),
        None => {
            let mut todos = Vec::new();
            for e in std::fs::read_dir(clon)?.flatten() {
                if e.file_name().to_string_lossy().starts_with('.') || !e.path().is_dir() {
                    continue;
                }
                for f in std::fs::read_dir(e.path())?.flatten() {
                    todos.push(f.path());
                }
            }
            todos
        }
    };
    v.retain(|p| p.extension().is_some_and(|x| x == "jsonl") && p.file_name().is_some_and(|n| n.to_string_lossy() != propio));
    v.sort();
    Ok(v)
}

#[derive(Debug, Default, Serialize)]
pub struct Resumen {
    pub equipo: String,
    pub exportadas: usize,
    pub leidos: usize,
    pub nuevas: usize,
    pub actualizadas: usize,
    pub olvidadas: usize,
}

fn hlc_local(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

fn hlc_olvido(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

/// Lee los ficheros de otros equipos y los junta con la base.
fn importar(db: &mut Connection, ficheros: &[PathBuf], config: &Config, cifra: &Cifra, actual: Option<&str>, modelo: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
    // Clave → proyecto de aquí: los que ya tienen memorias (o se adoptaron) y el de la carpeta actual.
    let mut locales: HashMap<String, String> = HashMap::new();
    for (p, k) in claves_locales(db, &config.person)? {
        locales.entry(k).or_insert(p);
    }
    if let Some(a) = actual.filter(|a| !a.is_empty() && !crate::proyecto::es_general(a)) {
        locales.insert(clave(a, &config.person), a.to_string());
    }
    let mut lineas: Vec<(String, Linea)> = Vec::new();
    for f in ficheros {
        let origen = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let texto = std::fs::read_to_string(f).with_context(|| format!("{}", f.display()))?;
        for l in texto.lines().filter(|l| !l.trim().is_empty()) {
            let claro = cifra.descifrar(l)?;
            match serde_json::from_str::<Linea>(&claro) {
                Ok(x) => lineas.push((origen.clone(), x)),
                Err(_) => continue, // una línea de una versión futura: se ignora
            }
        }
        r.leidos += 1;
    }
    // Primero los olvidos, para no traer algo que se va a borrar.
    for (_, l) in &lineas {
        let Linea::Tombstone { id, deleted_at, hlc } = l else { continue };
        if hlc_olvido(db, id)?.is_some_and(|h| &h >= hlc) {
            continue;
        }
        match hlc_local(db, id)? {
            Some(local) if &local > hlc => {
                // Aquí se cambió después de que allí se olvidara: se queda, y la marca vieja se apunta.
                db.execute(
                    "INSERT INTO tombstone(id, deleted_at, hlc) VALUES (?1, ?2, ?3)
                     ON CONFLICT(id) DO UPDATE SET deleted_at = excluded.deleted_at, hlc = excluded.hlc WHERE excluded.hlc > tombstone.hlc",
                    params![id, deleted_at, hlc],
                )?;
                reloj::recibir(db, hlc)?;
            }
            Some(_) => {
                memoria::olvidar_con(db, id, hlc, deleted_at)?;
                r.olvidadas += 1;
            }
            None => {
                db.execute(
                    "INSERT INTO tombstone(id, deleted_at, hlc) VALUES (?1, ?2, ?3)
                     ON CONFLICT(id) DO UPDATE SET deleted_at = excluded.deleted_at, hlc = excluded.hlc WHERE excluded.hlc > tombstone.hlc",
                    params![id, deleted_at, hlc],
                )?;
                reloj::recibir(db, hlc)?;
            }
        }
    }
    for (origen, l) in lineas {
        let Linea::Memory { id, project_key, project_path, kind, title, content, paths, tags, pinned, created_at, updated_at, hlc, author } = l else {
            continue;
        };
        if hlc_olvido(db, &id)?.is_some_and(|h| h >= hlc) {
            continue;
        }
        let local = hlc_local(db, &id)?;
        if local.as_ref().is_some_and(|h| h >= &hlc) {
            continue;
        }
        let project = if project_key.is_empty() {
            String::new()
        } else if let Some(p) = locales.get(&project_key) {
            p.clone()
        } else {
            // Aquí aún no hay ningún proyecto con esa clave: la ruta de origen, apuntada para adoptarla.
            db.execute("INSERT OR REPLACE INTO remote_project(path, key) VALUES (?1, ?2)", params![project_path, project_key])?;
            locales.insert(project_key.clone(), project_path.clone());
            project_path
        };
        let o = Origen { created_at: &created_at, updated_at: &updated_at, hlc: &hlc, author: &author };
        if local.is_some() {
            let c = Cambios {
                title: Some(title),
                content: Some(content),
                kind: Some(kind),
                paths: Some(paths),
                tags: Some(tags),
                pinned: Some(pinned),
                project: Some(project),
                reason: Some(format!("sync: {origen}")),
            };
            memoria::actualizar(db, &id, c, modelo)?;
            memoria::fijar_origen(db, &id, &o)?;
            r.actualizadas += 1;
        } else {
            let nueva = Nueva { project, kind: Some(kind), title, content, paths, tags, pinned, source: format!("sync:{origen}") };
            memoria::guardar_con_id(db, &id, nueva, &o, modelo)?;
            r.nuevas += 1;
        }
    }
    Ok(())
}

// --- Sincronizar -----------------------------------------------------------------------------------

/// Una sincronización completa: bajar, juntar, escribir lo de este equipo, commit y subir.
pub fn sincronizar(db: &mut Connection, ruta_db: &Path, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<Resumen> {
    let r = rutas(ruta_db);
    let config = leer_config(ruta_db)?.context(tr!(
        "sync is not set up: `omoi sync init <git url>` (or `omoi sync join <code>`)",
        "la sincronización no está configurada: `omoi sync init <url de git>` (o `omoi sync join <código>`)"
    ))?;
    let cifra = Cifra { clave: leer_clave(&r)? };
    ensure!(r.clon.join(".git").exists(), tr!("the sync clone is missing: run `omoi sync init` again", "falta el clon de la sincronización: vuelve a hacer `omoi sync init`"));
    if let Some(a) = actual {
        adoptar(db, a, &config.person)?;
    }
    let equipo = reloj::equipo(db)?;
    let mut res = Resumen { equipo: equipo.clone(), ..Default::default() };
    for intento in 0..4 {
        if remoto_tiene_rama(&r.clon)? {
            git(&r.clon, &["fetch", "--quiet", "origin", RAMA])?;
            if git(&r.clon, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
                // Este clon aún no tiene commits (se unió con el repositorio vacío): toma la rama tal cual.
                git(&r.clon, &["checkout", "--quiet", "-B", RAMA, &format!("origin/{RAMA}")])?;
            } else {
                // Cada equipo solo toca sus ficheros: el rebase no choca nunca.
                git(&r.clon, &["rebase", "--quiet", &format!("origin/{RAMA}")]).inspect_err(|_| {
                    let _ = git(&r.clon, &["rebase", "--abort"]);
                })?;
            }
        }
        let ficheros = por_leer(db, &r.clon, &equipo)?;
        importar(db, &ficheros, &config, &cifra, actual, modelo, &mut res)?;
        res.exportadas = exportar(db, &r.clon, &equipo, &config, &cifra)?;
        git(&r.clon, &["add", "-A"])?;
        if !git(&r.clon, &["status", "--porcelain"])?.trim().is_empty() {
            git(&r.clon, &["commit", "--quiet", "-m", &format!("omoikane: {equipo}")])?;
        }
        let hay_commits = git(&r.clon, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
        let subir = if hay_commits { git(&r.clon, &["push", "--quiet", "origin", &format!("HEAD:{RAMA}")]) } else { Ok(String::new()) };
        match subir {
            Ok(_) => {
                if hay_commits {
                    let head = git(&r.clon, &["rev-parse", "HEAD"])?;
                    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sync_commit', ?1)", [head.trim()])?;
                }
                return Ok(res);
            }
            // Otro equipo subió entre medias: se baja lo suyo y se vuelve a intentar.
            Err(e) if intento < 3 => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(200 * (intento + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

// --- Configurar: init, join, code ------------------------------------------------------------------

/// Lo que se elige al configurar.
#[derive(Clone, Debug)]
pub struct Eleccion {
    pub preferencias: bool,
    pub global: bool,
    /// Carpetas de proyecto; vacío = todos.
    pub proyectos: Vec<String>,
    pub autor: Option<String>,
}

/// El nombre de git del usuario (`git config user.name`), si lo tiene.
pub fn nombre_git() -> Option<String> {
    let o = std::process::Command::new("git").args(["config", "--get", "user.name"]).output().ok()?;
    let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !n.is_empty()).then_some(n)
}

pub fn config_de(url: &str, e: &Eleccion) -> Config {
    let mut kinds = vec![Tipo::Fact, Tipo::Decision, Tipo::Gotcha];
    if e.preferencias {
        kinds.push(Tipo::Preference);
    }
    let person = persona(e.autor.as_deref().map(String::from).or_else(nombre_git).as_deref().unwrap_or("user"));
    let projects = (!e.proyectos.is_empty()).then(|| e.proyectos.iter().map(|p| clave(p, &person)).collect());
    Config { url: url.to_string(), kinds, global: e.global, projects, author: e.autor.clone(), person }
}

fn guardar_config(db: &Connection, r: &Rutas, config: &Config, clave: &[u8]) -> Result<()> {
    escribir_privado(&r.clave, &a_hex(clave))?;
    escribir_privado(&r.config, &serde_json::to_string_pretty(config)?)?;
    match &config.author {
        Some(a) => db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('autor', ?1)", [a])?,
        None => db.execute("DELETE FROM meta WHERE k = 'autor'", [])?,
    };
    db.execute("DELETE FROM meta WHERE k = 'sync_commit'", [])?;
    Ok(())
}

fn hay_memorias_en(clon: &Path) -> bool {
    std::fs::read_dir(clon).into_iter().flatten().flatten().any(|e| {
        e.path().is_dir()
            && !e.file_name().to_string_lossy().starts_with('.')
            && std::fs::read_dir(e.path()).into_iter().flatten().flatten().any(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
    })
}

/// `omoi sync init <url>`: un repositorio nuevo (vacío o sin memorias), con una clave nueva.
pub fn iniciar(db: &mut Connection, ruta_db: &Path, config: &Config) -> Result<()> {
    let r = rutas(ruta_db);
    clonar(&config.url, &r.clon, config.author.as_deref())?;
    if hay_memorias_en(&r.clon) {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(tr!(
            "this repository already has memories: on a computer that already syncs with it run `omoi sync code`, and here `omoi sync join <code>`",
            "este repositorio ya tiene memorias: en un equipo que ya sincroniza con él ejecuta `omoi sync code` y aquí `omoi sync join <código>`"
        ));
    }
    let mut clave = vec![0u8; 64];
    getrandom::fill(&mut clave).map_err(|e| anyhow!("getrandom: {e}"))?;
    guardar_config(db, &r, config, &clave)
}

/// El código para unir otro equipo: lleva la URL y la clave (¡pásalo por un canal privado!).
pub fn codigo(ruta_db: &Path) -> Result<String> {
    let r = rutas(ruta_db);
    let config = leer_config(ruta_db)?.context(tr!("sync is not set up", "la sincronización no está configurada"))?;
    let k = leer_clave(&r)?;
    let json = serde_json::json!({ "url": config.url, "key": a_hex(&k) });
    Ok(format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())))
}

/// URL y clave de un código de `omoi sync code`.
pub fn leer_codigo(codigo: &str) -> Result<(String, Vec<u8>)> {
    let mal = || anyhow!(tr!("that is not an Omoikane sync code", "eso no es un código de sincronización de Omoikane"));
    let resto = codigo.trim().strip_prefix(PREFIJO_CODIGO).ok_or_else(mal)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(resto).map_err(|_| mal())?;
    let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| mal())?;
    let url = v["url"].as_str().ok_or_else(mal)?.to_string();
    let k = desde_hex(v["key"].as_str().ok_or_else(mal)?).ok_or_else(mal)?;
    ensure!(k.len() == 64, mal());
    Ok((url, k))
}

/// `omoi sync join <código>`: el repositorio y la clave de otro equipo. Comprueba que la clave abre
/// lo que ya hay antes de guardar nada.
pub fn unirse(db: &mut Connection, ruta_db: &Path, config: &Config, clave: &[u8]) -> Result<()> {
    let r = rutas(ruta_db);
    clonar(&config.url, &r.clon, config.author.as_deref())?;
    let cifra = Cifra { clave: clave.to_vec() };
    let primera = std::fs::read_dir(&r.clon)?
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
        .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
        .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
        .find_map(|f| std::fs::read_to_string(f.path()).ok().and_then(|t| t.lines().next().map(String::from)));
    if let Some(l) = primera
        && let Err(e) = cifra.descifrar(&l)
    {
        let _ = std::fs::remove_dir_all(&r.clon);
        return Err(e);
    }
    guardar_config(db, &r, config, clave)
}

/// Deja de sincronizar: borra la configuración, la clave y el clon (las memorias de aquí se quedan).
pub fn desactivar(db: &Connection, ruta_db: &Path) -> Result<()> {
    let r = rutas(ruta_db);
    for f in [&r.config, &r.clave, &r.estado, &r.pendiente, &r.cerrojo] {
        let _ = std::fs::remove_file(f);
    }
    if r.clon.exists() {
        std::fs::remove_dir_all(&r.clon)?;
    }
    db.execute("DELETE FROM meta WHERE k IN ('autor', 'sync_commit')", [])?;
    Ok(())
}

// --- En segundo plano ------------------------------------------------------------------------------

/// Cómo fue la última sincronización.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Estado {
    pub ultimo_ok: Option<String>,
    pub ultimo_error: Option<String>,
    /// Desde cuándo falla (la primera vez seguida).
    pub error_desde: Option<String>,
}

pub fn leer_estado(ruta_db: &Path) -> Estado {
    std::fs::read_to_string(rutas(ruta_db).estado).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn ahora() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    s.to_string()
}

fn segundos(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

fn apuntar(ruta_db: &Path, resultado: &Result<Resumen>) {
    let mut e = leer_estado(ruta_db);
    match resultado {
        Ok(_) => {
            e.ultimo_ok = Some(ahora());
            e.ultimo_error = None;
            e.error_desde = None;
        }
        Err(err) => {
            e.ultimo_error = Some(format!("{err:#}"));
            e.error_desde.get_or_insert_with(ahora);
        }
    }
    if let Ok(t) = serde_json::to_string_pretty(&e) {
        let _ = std::fs::write(rutas(ruta_db).estado, t);
    }
}

/// Hay cambios sin subir.
pub fn pendiente(ruta_db: &Path) -> bool {
    rutas(ruta_db).pendiente.exists()
}

/// Un aviso para el inicio de sesión si la sincronización lleva más de una hora fallando.
pub fn aviso(ruta_db: &Path) -> Option<String> {
    leer_config(ruta_db).ok().flatten()?;
    let e = leer_estado(ruta_db);
    let desde = segundos(e.error_desde.as_deref()?);
    let ahora = segundos(&ahora());
    (ahora.saturating_sub(desde) >= 3600).then(|| {
        format!(
            "Omoikane sync has been failing for {} h (last error: {}). Tell the user; `omoi sync status` shows more.",
            (ahora - desde) / 3600,
            e.ultimo_error.unwrap_or_default().lines().next().unwrap_or_default()
        )
    })
}

/// Tras un cambio: si la sincronización está configurada, deja la marca de pendiente y lanza
/// `omoi sync --background` aparte, sin esperar. No falla nunca (lo peor es no sincronizar ahora).
pub fn programar(ruta_db: &Path) {
    let r = rutas(ruta_db);
    // `OMOIKANE_SYNC_AUTO=0`: solo a mano (`omoi sync`).
    if !r.config.exists() || std::env::var("OMOIKANE_SYNC_AUTO").is_ok_and(|v| v == "0") {
        return;
    }
    let marca =format!("{}\n{}", ahora(), ulid::Ulid::generate());
    if std::fs::write(&r.pendiente, marca).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(["sync", "--background"])
        .env("OMOIKANE_DB", ruta_db)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: sin consola y sin morir con
        // quien lo lanza.
        c.creation_flags(0x0800_0000 | 0x0000_0008 | 0x0000_0200);
        no_heredar_salidas();
    }
    let _ = c.spawn();
}

/// En Windows un proceso hijo hereda todos los handles heredables, también la entrada y las salidas
/// de este proceso aunque al hijo se le den otras. Si quien llamó a `omoi` (un hook, un agente) lee
/// esas salidas, esperaría a que acabe la sincronización en segundo plano. Este proceso termina
/// enseguida, así que se marcan como no heredables antes de lanzarla.
#[cfg(windows)]
fn no_heredar_salidas() {
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn SetHandleInformation(h: isize, mascara: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE y STD_ERROR_HANDLE (-10, -11, -12).
    for n in [-10i32, -11, -12] {
        // SAFETY: llamadas de la API de Windows con un handle que devuelve el propio sistema.
        unsafe {
            let h = GetStdHandle(n as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Un cerrojo por equipo: un fichero creado en exclusiva. Uno de más de 10 minutos se da por
/// abandonado (un proceso que murió a medias).
pub struct Cerrojo(PathBuf);

impl Cerrojo {
    pub fn tomar(ruta_db: &Path) -> Option<Cerrojo> {
        let p = rutas(ruta_db).cerrojo;
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(_) => return Some(Cerrojo(p)),
                Err(_) => {
                    let viejo = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(600));
                    if viejo {
                        let _ = std::fs::remove_file(&p);
                    } else {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Espera hasta `max` a que quede libre.
    pub fn esperar(ruta_db: &Path, max: Duration) -> Option<Cerrojo> {
        let inicio = std::time::Instant::now();
        loop {
            if let Some(c) = Cerrojo::tomar(ruta_db) {
                return Some(c);
            }
            if inicio.elapsed() >= max {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for Cerrojo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn espera() -> Duration {
    Duration::from_millis(std::env::var("OMOIKANE_SYNC_ESPERA_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000))
}

/// `omoi sync --background`: espera unos segundos para juntar cambios, sincroniza, y repite si
/// mientras tanto llegaron más. Si ya hay otro en marcha, se va (ese recogerá la marca).
pub fn en_segundo_plano(ruta_db: &Path, abrir: impl Fn() -> Result<(Connection, Option<Modelo>)>) -> Result<()> {
    let Some(_cerrojo) = Cerrojo::tomar(ruta_db) else { return Ok(()) };
    let r = rutas(ruta_db);
    for _ in 0..20 {
        std::thread::sleep(espera());
        let marca = std::fs::read_to_string(&r.pendiente).ok();
        let resultado = abrir().and_then(|(mut db, modelo)| sincronizar(&mut db, ruta_db, None, modelo.as_ref()));
        apuntar(ruta_db, &resultado);
        if resultado.is_err() {
            break;
        }
        // Si nadie cambió nada mientras tanto, se borra la marca y se acaba.
        if std::fs::read_to_string(&r.pendiente).ok() == marca {
            let _ = std::fs::remove_file(&r.pendiente);
            break;
        }
    }
    Ok(())
}

/// `omoi sync` a mano: espera a que acabe uno en segundo plano si lo hay, y apunta el resultado.
pub fn ahora_mismo(db: &mut Connection, ruta_db: &Path, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<Resumen> {
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60))
        .context(tr!("another sync is running; try again in a moment", "hay otra sincronización en marcha; prueba dentro de un momento"))?;
    let marca = std::fs::read_to_string(rutas(ruta_db).pendiente).ok();
    let r = sincronizar(db, ruta_db, actual, modelo);
    apuntar(ruta_db, &r);
    if r.is_ok() && std::fs::read_to_string(rutas(ruta_db).pendiente).ok() == marca {
        let _ = std::fs::remove_file(rutas(ruta_db).pendiente);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotos_iguales() {
        let a = normalizar_remoto("git@github.com:Marc/Tienda.git");
        assert_eq!(a, "github.com/marc/tienda");
        assert_eq!(normalizar_remoto("https://github.com/marc/tienda"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com/marc/tienda.git/"), a);
        assert_eq!(normalizar_remoto("ssh://git@github.com:22/marc/tienda.git"), a);
    }

    #[test]
    fn cifrado_determinista_y_clave_mala() {
        let c = Cifra { clave: (0..64).collect() };
        let x = c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap();
        assert_eq!(x, c.cifrar("{\"hola\":\"mundo, una línea\"}").unwrap(), "la misma línea cifra igual");
        assert_eq!(c.descifrar(&x).unwrap(), "{\"hola\":\"mundo, una línea\"}");
        let otra = Cifra { clave: (1..65).collect() };
        assert!(otra.descifrar(&x).is_err());
        assert_ne!(c.carpeta("git:a/b"), otra.carpeta("git:a/b"));
        assert!(c.carpeta("git:a/b").starts_with("p-"));
    }

    #[test]
    fn codigo_va_y_vuelve() {
        let k: Vec<u8> = (0..64).collect();
        let json = serde_json::json!({ "url": "git@x:y/z.git", "key": a_hex(&k) });
        let c = format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap(), ("git@x:y/z.git".to_string(), k));
        assert!(leer_codigo("hola").is_err());
    }

    #[test]
    fn personas() {
        assert_eq!(persona("Marc Mayol"), "marc-mayol");
        assert_eq!(persona("  "), "user");
    }
}
