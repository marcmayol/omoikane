//! Memoria compartida por git: los equipos de una persona (o un grupo) juntan sus memorias en un
//! repositorio git suyo, sin servicio propio y sin coste. Omoikane solo usa el `git` del usuario, con
//! sus credenciales.
//!
//! - **Qué se sube:** texto, nunca el SQLite. Una línea por memoria (y por cada olvido), ordenadas y
//!   partidas por proyecto y por equipo: `<proyecto>/<equipo>.jsonl`, `global/<equipo>.jsonl` y
//!   `olvidos/<equipo>.jsonl`, donde `<equipo>` es un nombre opaco derivado de la clave (el nombre del
//!   equipo, que lleva el del sistema, solo va dentro de las líneas cifradas). Cada equipo solo
//!   escribe sus ficheros, así que git nunca tiene conflictos; al bajar se leen solo los ficheros que
//!   cambiaron desde la última vez. Los commits van firmados como «omoikane», sin nombres.
//! - **Cifrado solo en el remoto:** cada línea va cifrada con AES-SIV (RFC 5297), que es determinista:
//!   una línea que no cambia queda igual byte a byte y el diff de git sigue siendo de una línea. Las
//!   carpetas de proyecto tienen un nombre opaco derivado de la clave. En el equipo todo está en claro.
//!   El repositorio lleva un fichero de comprobación (`omoikane.check`) cifrado con su clave: un equipo
//!   con otra clave lo sabe antes de leer nada.
//! - **Quién gana:** el sello del reloj lógico (`reloj`) más alto. La versión de aquí que pierde queda
//!   en el historial con el motivo `sync: <equipo>`. Los olvidos se aplican antes que las memorias, así
//!   que lo olvidado no resucita; lo que olvida otro equipo se guarda aquí aparte (`omoi sync restore`).
//! - **Proyectos en rutas distintas:** se reconocen por el remoto `origin` de git (o, sin remoto, por
//!   la persona y el nombre de la carpeta), y cada equipo guarda la memoria con su propia ruta.
//! - **Automático:** después de cada cambio, `programar` lanza `omoi sync --background` aparte; espera
//!   unos segundos para juntar cambios y solo corre uno a la vez por equipo.
//!
//! Todo vive junto a la base: `sync.json` (configuración), `sync.key` (clave), `sync/` (el clon),
//! `sync-estado.json`, `sync.pendiente`, `sync.hecho` y `sync.lock`. Con una base que no se llama
//! `omoikane.db`, los nombres llevan delante el de la base (`trabajo.sync.json`…), para que dos bases
//! en la misma carpeta no compartan sincronización.

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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Cabecera asociada al cifrado: una línea de otro formato o versión no se descifra por accidente.
const CABECERA: &[u8] = b"omoikane-sync-v1";
const RAMA: &str = "main";
const PREFIJO_CODIGO: &str = "omoi1.";
/// El fichero de comprobación de la clave, en la raíz del repositorio, y lo que lleva dentro (cifrado).
const CHEQUEO: &str = "omoikane.check";
const TEXTO_CHEQUEO: &str = "omoikane-key-check-v1";
/// Lo que puede tardar una orden de git antes de darla por colgada.
const LIMITE_GIT: Duration = Duration::from_secs(120);

// --- Rutas y configuración -------------------------------------------------------------------------

/// Los ficheros de la sincronización, junto a la base.
pub struct Rutas {
    pub config: PathBuf,
    pub clave: PathBuf,
    pub clon: PathBuf,
    pub estado: PathBuf,
    pub pendiente: PathBuf,
    pub hecho: PathBuf,
    pub cerrojo: PathBuf,
}

pub fn rutas(db: &Path) -> Rutas {
    let dir = db.parent().map(Path::to_path_buf).unwrap_or_default();
    let delante = match db.file_name().map(|n| n.to_string_lossy().into_owned()) {
        Some(n) if n != "omoikane.db" && !n.is_empty() => {
            format!("{}.", db.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n))
        }
        _ => String::new(),
    };
    let f = |nombre: &str| dir.join(format!("{delante}{nombre}"));
    Rutas {
        config: f("sync.json"),
        clave: f("sync.key"),
        clon: f("sync"),
        estado: f("sync-estado.json"),
        pendiente: f("sync.pendiente"),
        hecho: f("sync.hecho"),
        cerrojo: f("sync.lock"),
    }
}

/// Qué se comparte y cómo. Se decide en `omoi sync init` (o `join`). Vale para lo que se sube y para
/// lo que se acepta de los demás.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub url: String,
    /// Tipos que se comparten (las preferencias, solo si se eligió compartirlas).
    pub kinds: Vec<Tipo>,
    /// Si se comparten las memorias globales.
    pub global: bool,
    /// Proyectos que se comparten, por su clave; ninguno = todos.
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
    fn acepta(&self, kind: Tipo, clave: &str) -> bool {
        if !self.kinds.contains(&kind) && !(kind == Tipo::Procedure && self.kinds.contains(&Tipo::Fact)) {
            return false;
        }
        if clave.is_empty() {
            return self.global;
        }
        self.projects.as_ref().is_none_or(|p| p.iter().any(|k| k == clave))
    }

    fn comparte(&self, m: &Memoria, clave: &str) -> bool {
        self.acepta(m.kind, if m.project.is_empty() { "" } else { clave })
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

/// Escribe un fichero que solo puede leer el usuario (en unix, creado ya con permisos 0600).
fn escribir_privado(ruta: &Path, texto: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = ruta.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(texto.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, ruta)?;
    }
    #[cfg(not(unix))]
    std::fs::write(ruta, texto)?;
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

/// Quita usuario y contraseña de las URL de un texto (`https://token@host/…` → `https://***@host/…`):
/// para enseñar URLs y errores sin enseñar credenciales.
pub fn sin_credenciales(texto: &str) -> String {
    let mut s = String::with_capacity(texto.len());
    let mut resto = texto;
    while let Some(i) = resto.find("://") {
        let (antes, despues) = resto.split_at(i + 3);
        s.push_str(antes);
        let fin = despues.find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"').unwrap_or(despues.len());
        let anfitrion = &despues[..fin];
        match anfitrion.rfind('@') {
            Some(a) => {
                s.push_str("***@");
                s.push_str(&anfitrion[a + 1..]);
            }
            None => s.push_str(anfitrion),
        }
        resto = &despues[fin..];
    }
    s.push_str(resto);
    s
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
        let mal = || anyhow!(clave_ajena());
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

    /// Nombre opaco del fichero de un equipo: el nombre del equipo (con el del sistema) no queda a la
    /// vista en el repositorio.
    fn fichero(&self, equipo: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&self.clave);
        h.update(b"equipo:");
        h.update(equipo.as_bytes());
        format!("m-{}.jsonl", &a_hex(&h.finalize())[..16])
    }

    /// Si `omoikane.check` del clon se abre con esta clave. `None` si el repositorio no lo tiene.
    fn comprueba(&self, clon: &Path) -> Option<bool> {
        let t = std::fs::read_to_string(clon.join(CHEQUEO)).ok()?;
        Some(self.descifrar(t.trim()).is_ok_and(|x| x == TEXTO_CHEQUEO))
    }
}

fn clave_ajena() -> String {
    tr!(
        "this computer's key is not the repository's: join it with `omoi sync join <code>` (from `omoi sync code` on a computer that already syncs)",
        "la clave de este equipo no es la del repositorio: únete con `omoi sync join <código>` (de `omoi sync code` en un equipo que ya sincroniza)"
    )
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
        /// El equipo que escribió el fichero (para el historial: «sync: <equipo>»).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
    },
    Tombstone {
        id: String,
        deleted_at: String,
        hlc: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
    },
    /// Un veredicto (`veredictos`), en la carpeta de la memoria juzgada. Las versiones de antes de la
    /// 0.17.0 no lo entienden y se lo saltan.
    Verdict {
        memory_id: String,
        #[serde(default)]
        other_id: String,
        relation: String,
        at: String,
        hlc: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
    },
}

/// Un id como los que escribe Omoikane (ULID: 26 letras y números en mayúsculas). Una línea con otro
/// id no se acepta.
fn id_valido(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
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

/// Una persona como parte de una clave: minúsculas, letras y números (vacío si no queda nada).
pub fn persona(nombre: &str) -> String {
    let p: String = nombre.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    p.trim_matches('-').to_string()
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

#[cfg(windows)]
const NULO: &str = "NUL";
#[cfg(not(windows))]
const NULO: &str = "/dev/null";

/// Sin ventanas en Windows (en segundo plano no hay consola; cada orden abriría una).
fn sin_ventana(c: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Si el usuario ya dice cómo usar ssh (`GIT_SSH_COMMAND`, `GIT_SSH` o `core.sshCommand`: plink, una
/// clave concreta…). Entonces se respeta; si no, ssh va en modo sin preguntas.
fn ssh_propio() -> bool {
    static PROPIO: OnceLock<bool> = OnceLock::new();
    *PROPIO.get_or_init(|| {
        if std::env::var_os("GIT_SSH_COMMAND").is_some() || std::env::var_os("GIT_SSH").is_some() {
            return true;
        }
        let mut c = std::process::Command::new("git");
        c.args(["config", "--get", "core.sshCommand"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        sin_ventana(&mut c);
        c.output().is_ok_and(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
    })
}

/// Mata git y lo que haya lanzado (git-remote-https, ssh…), no solo el proceso git.
fn matar_arbol(h: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let mut k = std::process::Command::new("taskkill");
        k.args(["/T", "/F", "/PID", &h.id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        sin_ventana(&mut k);
        let _ = k.status();
    }
    #[cfg(unix)]
    {
        // git va en su propio grupo de procesos: se mata el grupo entero.
        // SAFETY: kill con un pid de grupo negativo solo manda la señal a ese grupo.
        unsafe {
            libc::kill(-(h.id() as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = h.kill();
    let _ = h.wait();
}

/// Lee una salida en un hilo, guardando lo que llega a medida que llega: si un nieto de git (un ssh
/// con `ControlPersist`) la deja abierta, se puede seguir sin esperar a que la cierre.
fn lector(mut r: Box<dyn Read + Send>) -> (Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>) {
    let datos = Arc::new(Mutex::new(Vec::new()));
    let fin = Arc::new(AtomicBool::new(false));
    let (d, f) = (datos.clone(), fin.clone());
    std::thread::spawn(move || {
        let mut b = [0u8; 8192];
        loop {
            match r.read(&mut b) {
                Ok(0) | Err(_) => break,
                Ok(n) => d.lock().map(|mut v| v.extend_from_slice(&b[..n])).unwrap_or(()),
            }
        }
        f.store(true, Ordering::Relaxed);
    });
    (datos, fin)
}

/// Ejecuta git en el clon. Sin preguntas ni ventanas (en segundo plano no hay nadie para contestar),
/// sin los hooks ni las conversiones de fin de línea de la configuración del usuario (el clon es de
/// Omoikane, y un hook que rechace texto cifrado lo atascaría), y con un límite de tiempo para toda la
/// orden, también para leer lo que escribe.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut c = std::process::Command::new("git");
    c.arg("-c")
        .arg(format!("core.hooksPath={NULO}"))
        .args(["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if !ssh_propio() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    sin_ventana(&mut c);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut h = c.spawn().with_context(|| tr!("can't run git: is it installed?", "no se puede ejecutar git: ¿está instalado?"))?;
    let (salida, fin_salida) = lector(Box::new(h.stdout.take().expect("stdout")));
    let (errores, fin_errores) = lector(Box::new(h.stderr.take().expect("stderr")));
    let limite = Instant::now() + LIMITE_GIT;
    let colgado = || anyhow!("git {}: {}", args.first().unwrap_or(&""), tr!("no answer after {} s", "sin respuesta tras {} s", LIMITE_GIT.as_secs()));
    let estado = loop {
        if let Some(e) = h.try_wait()? {
            break e;
        }
        if Instant::now() > limite {
            matar_arbol(&mut h);
            return Err(colgado());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Lo que quede por leer: hasta que se cierren las salidas, con un margen corto (un nieto que las
    // deja abiertas no debe colgar la sincronización) y nunca más allá del límite.
    let margen = (Instant::now() + Duration::from_secs(2)).min(limite);
    while !(fin_salida.load(Ordering::Relaxed) && fin_errores.load(Ordering::Relaxed)) && Instant::now() < margen {
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = salida.lock().map(|v| v.clone()).unwrap_or_default();
    let err = errores.lock().map(|v| v.clone()).unwrap_or_default();
    if estado.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        bail!("git {}: {}", args.join(" "), sin_credenciales(String::from_utf8_lossy(&err).trim()))
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn remoto_tiene_rama(clon: &Path) -> Result<bool> {
    Ok(!git(clon, &["ls-remote", "--heads", "origin", RAMA])?.trim().is_empty())
}

fn hay_head(clon: &Path) -> bool {
    git(clon, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// Clona el repositorio (vacío o no), deja la rama `main` y una identidad propia y neutra en el clon:
/// los commits no dependen de que el usuario tenga git configurado ni dejan su nombre a la vista.
fn clonar(url: &str, clon: &Path) -> Result<()> {
    if clon.exists() {
        std::fs::remove_dir_all(clon).with_context(|| format!("{}", clon.display()))?;
    }
    let padre = clon.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(padre)?;
    let destino = clon.to_string_lossy().into_owned();
    git(padre, &["clone", "--quiet", "--", url, &destino]).with_context(|| tr!("can't clone {}", "no se puede clonar {}", sin_credenciales(url)))?;
    if remoto_tiene_rama(clon)? {
        git(clon, &["checkout", "--quiet", "--force", "-B", RAMA, &format!("origin/{RAMA}")])?;
    } else {
        git(clon, &["symbolic-ref", "HEAD", &format!("refs/heads/{RAMA}")])?;
    }
    git(clon, &["config", "user.name", "omoikane"])?;
    git(clon, &["config", "user.email", "omoikane@localhost"])?;
    git(clon, &["config", "commit.gpgsign", "false"])?;
    Ok(())
}

/// Deja el clon como el remoto (o como su último commit): lo de este equipo se vuelve a escribir desde
/// la base en cada sincronización, así que un commit o un índice a medias de una vez anterior (un
/// proceso que murió, un commit que falló) no atasca nada.
fn limpiar(clon: &Path) -> Result<()> {
    if hay_head(clon) {
        git(clon, &["reset", "--quiet", "--hard", "HEAD"])?;
    }
    git(clon, &["clean", "--quiet", "-fdx"])?;
    if remoto_tiene_rama(clon)? {
        git(clon, &["fetch", "--quiet", "origin", RAMA])?;
        git(clon, &["checkout", "--quiet", "--force", "-B", RAMA, &format!("origin/{RAMA}")])?;
    }
    Ok(())
}

/// Los ficheros fijos del repositorio: sin conversiones de texto, y la comprobación de la clave.
fn ficheros_fijos(clon: &Path, cifra: &Cifra) -> Result<()> {
    let atributos = clon.join(".gitattributes");
    if !atributos.exists() {
        std::fs::write(atributos, "* -text\n")?;
    }
    match cifra.comprueba(clon) {
        Some(true) => {}
        Some(false) => bail!(clave_ajena()),
        None => std::fs::write(clon.join(CHEQUEO), cifra.cifrar(TEXTO_CHEQUEO)? + "\n")?,
    }
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

/// Qué subiría este equipo (para el resumen de `init` y `join`): globales, de proyectos y cuántos
/// proyectos.
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
    // La carpeta de cada memoria que se comparte: sus veredictos van con ella.
    let mut carpetas: HashMap<String, String> = HashMap::new();
    for (m, hlc) in todas(db)? {
        let k = claves.get(&m.project).cloned().unwrap_or_default();
        if !config.comparte(&m, &k) {
            continue;
        }
        let carpeta = if m.project.is_empty() { "global".to_string() } else { cifra.carpeta(&k) };
        carpetas.insert(m.id.clone(), carpeta.clone());
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
            machine: equipo.to_string(),
        };
        ficheros.entry(carpeta).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        n += 1;
    }
    for v in crate::veredictos::todos(db)? {
        // Solo si las dos memorias viajan: un veredicto sobre algo que el otro equipo no tiene no sirve.
        let (Some(carpeta), true) = (carpetas.get(&v.memory_id), v.other_id.is_empty() || carpetas.contains_key(&v.other_id)) else {
            continue;
        };
        let l = Linea::Verdict { memory_id: v.memory_id, other_id: v.other_id, relation: v.relation, at: v.at, hlc: v.hlc, machine: equipo.to_string() };
        ficheros.entry(carpeta.clone()).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
    }
    {
        let mut q = db.prepare("SELECT id, deleted_at, hlc FROM tombstone ORDER BY id")?;
        let marcas: Vec<(String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, deleted_at, hlc) in marcas {
            let l = Linea::Tombstone { id, deleted_at, hlc, machine: equipo.to_string() };
            ficheros.entry("olvidos".into()).or_default().push(cifra.cifrar(&serde_json::to_string(&l)?)?);
        }
    }
    let nombre = cifra.fichero(equipo);
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
fn por_leer(db: &Connection, clon: &Path, equipo: &str, cifra: &Cifra) -> Result<Vec<PathBuf>> {
    let propio = cifra.fichero(equipo);
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
    /// Líneas de otros equipos que no se pudieron descifrar o no son válidas (se saltan).
    pub ilegibles: usize,
    /// Por fichero leído en esta pasada (ruta dentro del repositorio), cuántas líneas malas tenía.
    #[serde(skip)]
    pub malos: BTreeMap<String, usize>,
}

fn hlc_local(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

fn hlc_olvido(db: &Connection, id: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?)
}

/// `a` es igual o posterior a `b`.
fn no_menor(a: &str, b: &str) -> bool {
    reloj::comparar(a, b) != std::cmp::Ordering::Less
}

/// Lee los ficheros de otros equipos y los junta con la base. Una línea que no se descifra (de otra
/// clave, o dañada) se salta y se cuenta, sin parar la sincronización.
#[allow(clippy::too_many_arguments)]
fn importar(db: &mut Connection, clon: &Path, ficheros: &[PathBuf], config: &Config, cifra: &Cifra, actual: Option<&str>, modelo: Option<&Modelo>, r: &mut Resumen) -> Result<()> {
    // Clave → proyecto de aquí: los que ya tienen memorias (o se adoptaron) y el de la carpeta actual.
    let mut locales: HashMap<String, String> = HashMap::new();
    for (p, k) in claves_locales(db, &config.person)? {
        locales.entry(k).or_insert(p);
    }
    if let Some(a) = actual.filter(|a| !a.is_empty() && !crate::proyecto::es_general(a)) {
        locales.insert(clave(a, &config.person), a.to_string());
    }
    // (equipo de origen, fichero, línea)
    let mut lineas: Vec<(String, String, Linea)> = Vec::new();
    for f in ficheros {
        let relativo = f.strip_prefix(clon).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let texto = std::fs::read_to_string(f).with_context(|| format!("{}", f.display()))?;
        r.malos.insert(relativo.clone(), 0);
        for l in texto.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(claro) = cifra.descifrar(l) else {
                *r.malos.entry(relativo.clone()).or_default() += 1;
                continue;
            };
            let x = match serde_json::from_str::<Linea>(&claro) {
                Ok(x) => x,
                Err(_) => continue, // una línea de una versión futura: se ignora
            };
            let (id, machine) = match &x {
                Linea::Memory { id, machine, .. } | Linea::Tombstone { id, machine, .. } => (id, machine),
                Linea::Verdict { memory_id, machine, .. } => (memory_id, machine),
            };
            if !id_valido(id) || matches!(&x, Linea::Verdict { other_id, .. } if !other_id.is_empty() && !id_valido(other_id)) {
                *r.malos.entry(relativo.clone()).or_default() += 1;
                continue;
            }
            let origen = if machine.is_empty() { f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { machine.clone() };
            lineas.push((origen, relativo.clone(), x));
        }
        r.leidos += 1;
    }
    // Primero los olvidos, para no traer algo que se va a borrar. Uno que falla se salta y se cuenta.
    for (origen, fichero, l) in &lineas {
        let Linea::Tombstone { id, deleted_at, hlc, .. } = l else { continue };
        let aplicado = (|| -> Result<()> {
            if hlc_olvido(db, id)?.is_some_and(|h| no_menor(&h, hlc)) {
                return Ok(());
            }
            match hlc_local(db, id)? {
                Some(local) if reloj::mayor(&local, hlc) => {
                    // Aquí se cambió después de que allí se olvidara: se queda, y la marca vieja se apunta.
                    memoria::marcar_olvido(db, id, Some(deleted_at), hlc)?;
                    reloj::recibir(db, hlc)?;
                }
                Some(_) => {
                    memoria::olvidar_con(db, id, hlc, deleted_at, origen)?;
                    r.olvidadas += 1;
                }
                None => {
                    memoria::marcar_olvido(db, id, Some(deleted_at), hlc)?;
                    reloj::recibir(db, hlc)?;
                }
            }
            Ok(())
        })();
        if aplicado.is_err() {
            *r.malos.entry(fichero.clone()).or_default() += 1;
        }
    }
    // Las rutas que ya son proyectos aquí, y las que se van asignando en esta misma pasada: dos claves
    // desconocidas con la misma ruta de origen no acaban en el mismo proyecto.
    let mut rutas_locales: HashSet<String> = locales.values().cloned().collect();
    // Los veredictos, después de las memorias que juzgan.
    let mut veredictos = Vec::new();
    for (origen, fichero, l) in lineas {
        let Linea::Memory { id, project_key, project_path, kind, title, content, paths, tags, pinned, created_at, updated_at, hlc, author, .. } = l else {
            if let Linea::Verdict { memory_id, other_id, relation, at, hlc, .. } = l {
                veredictos.push((fichero, crate::veredictos::Veredicto { memory_id, other_id, relation, at, hlc }));
            }
            continue;
        };
        // Lo que este equipo no comparte tampoco se acepta (p. ej. las preferencias de otra persona).
        if !config.acepta(kind, &project_key) {
            continue;
        }
        if hlc_olvido(db, &id)?.is_some_and(|h| no_menor(&h, &hlc)) {
            continue;
        }
        let local = hlc_local(db, &id)?;
        if local.as_ref().is_some_and(|h| no_menor(h, &hlc)) {
            continue;
        }
        let project = if project_key.is_empty() {
            String::new()
        } else if let Some(p) = locales.get(&project_key) {
            p.clone()
        } else {
            // Aquí aún no hay ningún proyecto con esa clave: la ruta de origen, apuntada para adoptarla.
            // Si esa ruta ya es aquí otro proyecto (dos repositorios distintos en `/workspace`), una
            // ruta propia que no se confunda con él.
            let ocupada = |r: &str| rutas_locales.contains(r) || (Path::new(r).exists() && clave(r, &config.person) != project_key);
            let mut ruta = if ocupada(&project_path) { format!("{project_path} [{project_key}]") } else { project_path };
            let base = ruta.clone();
            let mut n = 2;
            while rutas_locales.contains(&ruta) {
                ruta = format!("{base} ({n})");
                n += 1;
            }
            db.execute("INSERT OR REPLACE INTO remote_project(path, key) VALUES (?1, ?2)", params![ruta, project_key])?;
            locales.insert(project_key.clone(), ruta.clone());
            rutas_locales.insert(ruta.clone());
            ruta
        };
        let o = Origen { created_at: &created_at, updated_at: &updated_at, hlc: &hlc, author: &author };
        // Una línea que no pasa las comprobaciones (título vacío o largo, contenido vacío…) se salta y
        // se cuenta: nunca para la sincronización.
        let hecho = if local.is_some() {
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
            memoria::actualizar_con_origen(db, &id, c, &o, modelo).map(|_| r.actualizadas += 1)
        } else {
            let nueva = Nueva { project, kind: Some(kind), title, content, paths, tags, pinned, source: format!("sync:{origen}") };
            memoria::guardar_con_id(db, &id, nueva, &o, modelo).map(|_| r.nuevas += 1)
        };
        if hecho.is_err() {
            *r.malos.entry(fichero).or_default() += 1;
        }
    }
    for (fichero, v) in veredictos {
        // De una memoria que aquí no está (no se comparte, o se olvidó) no se apunta nada.
        let existe = |id: &str| -> Result<bool> { Ok(db.query_row("SELECT count(*) FROM memory WHERE id = ?1", [id], |f| f.get::<_, i64>(0))? > 0) };
        let hecho = (|| -> Result<()> {
            if existe(&v.memory_id)? && (v.other_id.is_empty() || existe(&v.other_id)?) {
                crate::veredictos::recibir(db, &v)?;
            }
            Ok(())
        })();
        if hecho.is_err() {
            *r.malos.entry(fichero).or_default() += 1;
        }
    }
    r.ilegibles = r.malos.values().sum();

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
    for intento in 0..4 {
        // Las cuentas, de cada intento: un reintento vuelve a leer los mismos ficheros.
        let mut res = Resumen { equipo: equipo.clone(), ..Default::default() };
        limpiar(&r.clon)?;
        ficheros_fijos(&r.clon, &cifra)?;
        let ficheros = por_leer(db, &r.clon, &equipo, &cifra)?;
        importar(db, &r.clon, &ficheros, &config, &cifra, actual, modelo, &mut res)?;
        res.exportadas = exportar(db, &r.clon, &equipo, &config, &cifra)?;
        git(&r.clon, &["add", "-A"])?;
        if !git(&r.clon, &["status", "--porcelain"])?.trim().is_empty() {
            git(&r.clon, &["commit", "--quiet", "--no-verify", "-m", "omoikane"])?;
        }
        let hay_commits = hay_head(&r.clon);
        let subir = if hay_commits { git(&r.clon, &["push", "--quiet", "origin", &format!("HEAD:{RAMA}")]) } else { Ok(String::new()) };
        match subir {
            Ok(_) => {
                if hay_commits {
                    let head = git(&r.clon, &["rev-parse", "HEAD"])?;
                    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('sync_commit', ?1)", [head.trim()])?;
                }
                // Lo que llegó de otros equipos puede dejar anticuadas memorias de aquí (o al revés).
                let _ = crate::revisar::al_dia(db);
                return Ok(res);
            }
            // Otro equipo subió entre medias: se baja lo suyo y se vuelve a intentar.
            Err(_) if intento < 3 => std::thread::sleep(Duration::from_millis(200 * (intento + 1))),
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
    config_con_persona(url, e, None)
}

/// Como `config_de`, pero con la persona ya decidida (`join` usa la del equipo que invita: así los
/// proyectos sin remoto de una misma persona se juntan entre sus equipos aunque cada uno lleve otro
/// nombre de autor).
pub fn config_con_persona(url: &str, e: &Eleccion, fija: Option<&str>) -> Config {
    let mut kinds = vec![Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Procedure];
    if e.preferencias {
        kinds.push(Tipo::Preference);
    }
    // La persona: la que se da; si no, la del nombre; sin nombre, un identificador fijo al azar
    // (guardado en la configuración y pasado en el código para unirse), nunca uno común a todos.
    let person = fija
        .map(persona)
        .filter(|p| !p.is_empty())
        .or_else(|| e.autor.clone().or_else(nombre_git).map(|n| persona(&n)).filter(|p| !p.is_empty()))
        .unwrap_or_else(|| format!("u-{}", ulid::Ulid::generate().to_string()[16..].to_lowercase()));
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

fn ya_en_uso() -> String {
    tr!(
        "this repository is already in use by Omoikane: on a computer that already syncs with it run `omoi sync code`, and here `omoi sync join <code>`",
        "Omoikane ya usa este repositorio: en un equipo que ya sincroniza con él ejecuta `omoi sync code` y aquí `omoi sync join <código>`"
    )
}

/// `omoi sync init <url>`: un repositorio nuevo (vacío o sin memorias), con una clave nueva.
pub fn iniciar(db: &mut Connection, ruta_db: &Path, config: &Config) -> Result<()> {
    let r = rutas(ruta_db);
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    clonar(&config.url, &r.clon)?;
    if r.clon.join(CHEQUEO).exists() || hay_memorias_en(&r.clon) {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(ya_en_uso());
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
    let json = serde_json::json!({ "url": config.url, "key": a_hex(&k), "person": config.person });
    Ok(format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())))
}

/// Lo que lleva un código de `omoi sync code`.
#[derive(Debug, PartialEq)]
pub struct Codigo {
    pub url: String,
    pub clave: Vec<u8>,
    /// La persona del equipo que invita (los códigos de antes no la llevan).
    pub persona: Option<String>,
}

/// URL, clave y persona de un código de `omoi sync code`.
pub fn leer_codigo(codigo: &str) -> Result<Codigo> {
    let mal = || anyhow!(tr!("that is not an Omoikane sync code", "eso no es un código de sincronización de Omoikane"));
    let resto = codigo.trim().strip_prefix(PREFIJO_CODIGO).ok_or_else(mal)?;
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(resto).map_err(|_| mal())?;
    let v: serde_json::Value = serde_json::from_slice(&json).map_err(|_| mal())?;
    let url = v["url"].as_str().ok_or_else(mal)?.to_string();
    let k = desde_hex(v["key"].as_str().ok_or_else(mal)?).ok_or_else(mal)?;
    ensure!(k.len() == 64, mal());
    Ok(Codigo { url, clave: k, persona: v["person"].as_str().filter(|p| !p.is_empty()).map(String::from) })
}

/// `omoi sync join <código>`: el repositorio y la clave de otro equipo. Comprueba que la clave abre
/// el repositorio antes de guardar nada.
pub fn unirse(db: &mut Connection, ruta_db: &Path, config: &Config, clave: &[u8]) -> Result<()> {
    let r = rutas(ruta_db);
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    clonar(&config.url, &r.clon)?;
    let cifra = Cifra { clave: clave.to_vec() };
    let vale = match cifra.comprueba(&r.clon) {
        Some(v) => v,
        // Un repositorio sin comprobación (o aún vacío): la primera línea que haya.
        None => std::fs::read_dir(&r.clon)?
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
            .filter(|f| f.path().extension().is_some_and(|x| x == "jsonl"))
            .find_map(|f| std::fs::read_to_string(f.path()).ok().and_then(|t| t.lines().next().map(String::from)))
            .is_none_or(|l| cifra.descifrar(&l).is_ok()),
    };
    if !vale {
        let _ = std::fs::remove_dir_all(&r.clon);
        bail!(clave_ajena());
    }
    guardar_config(db, &r, config, clave)
}

/// Deja de sincronizar: borra la configuración, la clave y el clon (las memorias de aquí se quedan).
pub fn desactivar(db: &Connection, ruta_db: &Path) -> Result<()> {
    let r = rutas(ruta_db);
    let c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    for f in [&r.config, &r.clave, &r.estado, &r.pendiente, &r.hecho] {
        let _ = std::fs::remove_file(f);
    }
    if r.clon.exists() {
        std::fs::remove_dir_all(&r.clon)?;
    }
    db.execute("DELETE FROM meta WHERE k IN ('autor', 'sync_commit')", [])?;
    drop(c);
    Ok(())
}

fn otra_en_marcha() -> String {
    tr!("another sync is running; try again in a moment", "hay otra sincronización en marcha; prueba dentro de un momento")
}

// --- En segundo plano ------------------------------------------------------------------------------

/// Cómo fue la última sincronización.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Estado {
    pub ultimo_ok: Option<String>,
    pub ultimo_error: Option<String>,
    /// Desde cuándo falla (la primera vez seguida).
    pub error_desde: Option<String>,
    /// Algo que conviene saber aunque haya ido bien (líneas que no se pudieron leer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aviso: Option<String>,
    /// Ficheros del repositorio con líneas que no se pudieron leer, y cuántas: se quedan hasta que una
    /// sincronización vuelva a leer el fichero y esté bien.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ficheros_malos: BTreeMap<String, usize>,
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
        Ok(r) => {
            e.ultimo_ok = Some(ahora());
            e.ultimo_error = None;
            e.error_desde = None;
            for (f, n) in &r.malos {
                if *n == 0 {
                    e.ficheros_malos.remove(f);
                } else {
                    e.ficheros_malos.insert(f.clone(), *n);
                }
            }
            let malas: usize = e.ficheros_malos.values().sum();
            e.aviso = (malas > 0).then(|| {
                tr!(
                    "{} shared line(s) couldn't be read and were skipped (another key, or not valid)",
                    "{} línea(s) compartidas no se pudieron leer y se saltaron (otra clave, o no válidas)",
                    malas
                )
            });
        }
        Err(err) => {
            e.ultimo_error = Some(sin_credenciales(&format!("{err:#}")));
            e.error_desde.get_or_insert_with(ahora);
        }
    }
    if let Ok(t) = serde_json::to_string_pretty(&e) {
        let _ = std::fs::write(rutas(ruta_db).estado, t);
    }
}

/// Hay cambios sin subir: la última marca de cambio no es la que ya se sincronizó.
pub fn pendiente(ruta_db: &Path) -> bool {
    let r = rutas(ruta_db);
    match std::fs::read_to_string(&r.pendiente) {
        Ok(m) => std::fs::read_to_string(&r.hecho).ok().as_deref() != Some(m.as_str()),
        Err(_) => false,
    }
}

/// La primera línea de un error, sin credenciales ni lo que el servidor añada (`remote: …`), que no
/// debe llegar al contexto del agente.
fn error_corto(e: &str) -> String {
    let l = e.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("remote:")).unwrap_or_default();
    let l = l.split("remote:").next().unwrap_or_default().trim();
    sin_credenciales(&l.chars().take(200).collect::<String>())
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
            error_corto(e.ultimo_error.as_deref().unwrap_or_default())
        )
    })
}

/// Tras un cambio: si la sincronización está configurada, deja una marca de pendiente nueva y lanza
/// `omoi sync --background` aparte, sin esperar. No falla nunca (lo peor es no sincronizar ahora).
pub fn programar(ruta_db: &Path) {
    let r = rutas(ruta_db);
    // `OMOIKANE_SYNC_AUTO=0`: solo a mano (`omoi sync`).
    if !r.config.exists() || std::env::var("OMOIKANE_SYNC_AUTO").is_ok_and(|v| v == "0") {
        return;
    }
    // Cada cambio, una marca distinta: el proceso de fondo repite mientras la marca no sea la que ya
    // sincronizó (sin borrarla nunca, así no se pierde una que llegue justo en medio).
    let marca = ulid::Ulid::generate().to_string();
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
        // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP: con una consola oculta propia (así las órdenes
        // de git que lance tampoco abren ventanas) y sin morir con un Ctrl+C de quien lo lanza.
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_heredar_salidas();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Su propio grupo de procesos: un Ctrl+C en la terminal de quien lo lanzó no lo corta.
        c.process_group(0);
    }
    if let Ok(mut hijo) = c.spawn() {
        // Quien espera al hijo es un hilo: en procesos largos (`omoi mcp`, `omoi serve`) no quedan
        // procesos zombis; en uno corto, al salir, el sistema lo adopta.
        std::thread::spawn(move || {
            let _ = hijo.wait();
        });
    }
}

/// Lanza `omoi <args>` aparte, sin ventana, sin esperarlo y sin que se quede con las salidas de quien
/// llama (un hook las lee hasta el final). Lo usa la actualización diaria (`actualizar`).
pub(crate) fn lanzar_aparte(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = std::process::Command::new(exe);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW | 0x0000_0200);
        no_heredar_salidas();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    if let Ok(mut hijo) = c.spawn() {
        std::thread::spawn(move || {
            let _ = hijo.wait();
        });
    }
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

/// Si un proceso sigue vivo.
#[cfg(unix)]
fn vivo(pid: u32) -> bool {
    // SAFETY: kill con la señal 0 solo comprueba si el proceso existe.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn vivo(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(acceso: u32, heredar: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(h: isize, codigo: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: llamadas de la API de Windows; el handle se cierra siempre.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return false;
        }
        let mut codigo = 0u32;
        let ok = GetExitCodeProcess(h, &mut codigo);
        CloseHandle(h);
        ok != 0 && codigo == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn vivo(_pid: u32) -> bool {
    true
}

/// Un cerrojo por equipo: un fichero creado en exclusiva con el pid, la hora y una ficha propia. Uno
/// de un proceso que ya no existe, o sin renovar en 10 minutos, se da por abandonado. Mientras se tiene,
/// un hilo lo renueva; al soltarlo solo se borra si sigue siendo el nuestro.
pub struct Cerrojo {
    ruta: PathBuf,
    ficha: String,
    parar: Arc<AtomicBool>,
    hilo: Option<std::thread::JoinHandle<()>>,
}

const CERROJO_CADUCA: u64 = 600;

impl Cerrojo {
    fn contenido(ficha: &str) -> String {
        format!("{}\n{}\n{ficha}\n", std::process::id(), ahora())
    }

    fn abandonado(p: &Path) -> bool {
        let Ok(t) = std::fs::read_to_string(p) else { return false };
        let mut l = t.lines();
        let pid = l.next().and_then(|x| x.trim().parse::<u32>().ok());
        let cuando = l.next().map(|x| segundos(x.trim())).unwrap_or(0);
        let viejo = segundos(&ahora()).saturating_sub(cuando) > CERROJO_CADUCA;
        match pid {
            Some(pid) => !vivo(pid) || viejo,
            // Un cerrojo a medio escribir (o de una versión anterior, vacío): por su fecha.
            None => std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(CERROJO_CADUCA)),
        }
    }

    pub fn tomar(ruta_db: &Path) -> Option<Cerrojo> {
        let p = rutas(ruta_db).cerrojo;
        let ficha = ulid::Ulid::generate().to_string();
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(f) => {
                    use std::io::Write;
                    let mut f = f;
                    let _ = f.write_all(Cerrojo::contenido(&ficha).as_bytes());
                    drop(f);
                    let parar = Arc::new(AtomicBool::new(false));
                    let hilo = {
                        let (p, ficha, parar) = (p.clone(), ficha.clone(), parar.clone());
                        std::thread::spawn(move || {
                            let mut n = 0u32;
                            while !parar.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(200));
                                n += 1;
                                // Cada minuto, si sigue siendo nuestro, se renueva la hora.
                                if n.is_multiple_of(300) && std::fs::read_to_string(&p).is_ok_and(|t| t.contains(&ficha)) {
                                    let _ = std::fs::write(&p, Cerrojo::contenido(&ficha));
                                }
                            }
                        })
                    };
                    return Some(Cerrojo { ruta: p, ficha, parar, hilo: Some(hilo) });
                }
                Err(_) => {
                    let visto = std::fs::read_to_string(&p).ok();
                    if !Cerrojo::abandonado(&p) {
                        return None;
                    }
                    // Se aparta con un nombre propio (renombrar es atómico: si dos lo ven abandonado,
                    // solo uno lo consigue) y se comprueba que lo apartado es lo que se juzgó abandonado.
                    let aparte = p.with_extension(format!("lock.{ficha}"));
                    if std::fs::rename(&p, &aparte).is_err() {
                        return None;
                    }
                    if std::fs::read_to_string(&aparte).ok() != visto {
                        // Otro ya lo había tomado entre medias: se le devuelve.
                        let _ = std::fs::rename(&aparte, &p);
                        return None;
                    }
                    let _ = std::fs::remove_file(&aparte);
                }
            }
        }
        None
    }

    /// Espera hasta `max` a que quede libre.
    pub fn esperar(ruta_db: &Path, max: Duration) -> Option<Cerrojo> {
        let inicio = Instant::now();
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
        self.parar.store(true, Ordering::Relaxed);
        if let Some(h) = self.hilo.take() {
            let _ = h.join();
        }
        if std::fs::read_to_string(&self.ruta).is_ok_and(|t| t.contains(&self.ficha)) {
            let _ = std::fs::remove_file(&self.ruta);
        }
    }
}

fn espera() -> Duration {
    Duration::from_millis(std::env::var("OMOIKANE_SYNC_ESPERA_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000))
}

/// Apunta que la marca de cambio `marca` ya está sincronizada.
fn hecho(r: &Rutas, marca: Option<&str>) {
    if let Some(m) = marca {
        let _ = std::fs::write(&r.hecho, m);
    }
}

/// `omoi sync --background`: espera unos segundos para juntar cambios, sincroniza, y repite si
/// mientras tanto llegaron más. Si ya hay otro en marcha, se va (ese verá la marca nueva). `abrir`
/// no necesita cargar el modelo: lo que llegue sin vector lo completa la siguiente orden que lo abra.
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
        hecho(&r, marca.as_deref());
        if !pendiente(ruta_db) {
            break;
        }
    }
    Ok(())
}

/// `omoi sync` a mano: espera a que acabe uno en segundo plano si lo hay, y apunta el resultado.
pub fn ahora_mismo(db: &mut Connection, ruta_db: &Path, actual: Option<&str>, modelo: Option<&Modelo>) -> Result<Resumen> {
    let _c = Cerrojo::esperar(ruta_db, Duration::from_secs(60)).context(otra_en_marcha())?;
    let r = rutas(ruta_db);
    // Un cambio hecho mientras tanto lanza una sincronización de fondo que se va enseguida (este tiene
    // el cerrojo): se repite aquí mientras la marca de pendiente avance.
    let mut vueltas = 0;
    loop {
        let marca = std::fs::read_to_string(&r.pendiente).ok();
        let res = sincronizar(db, ruta_db, actual, modelo);
        apuntar(ruta_db, &res);
        let res = res?;
        hecho(&r, marca.as_deref());
        vueltas += 1;
        if !pendiente(ruta_db) || vueltas >= 5 {
            return Ok(res);
        }
    }
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
        assert!(c.fichero("marc-pc-abc").starts_with("m-") && !c.fichero("marc-pc-abc").contains("marc"));
        assert_ne!(c.fichero("a"), c.fichero("b"));
        assert!(c.carpeta("git:a/b").starts_with("p-"));
    }

    #[test]
    fn codigo_va_y_vuelve() {
        let k: Vec<u8> = (0..64).collect();
        let json = serde_json::json!({ "url": "git@x:y/z.git", "key": a_hex(&k) });
        let c = format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap(), Codigo { url: "git@x:y/z.git".to_string(), clave: k.clone(), persona: None });
        let json = serde_json::json!({ "url": "u", "key": a_hex(&k), "person": "marc" });
        let c = format!("{PREFIJO_CODIGO}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string()));
        assert_eq!(leer_codigo(&c).unwrap().persona.as_deref(), Some("marc"));
        assert!(leer_codigo("hola").is_err());
    }

    #[test]
    fn personas() {
        assert_eq!(persona("Marc Mayol"), "marc-mayol");
        assert_eq!(persona("  "), "");
        let e = Eleccion { preferencias: false, global: true, proyectos: vec![], autor: Some(" ".into()) };
        let c = config_de("x", &e);
        assert!(c.person.starts_with("u-") || !c.person.is_empty(), "nunca vacía: {}", c.person);
        assert_eq!(config_con_persona("x", &e, Some("Ana López")).person, "ana-lópez");
        assert!(id_valido("01M62EWVHRXM0000000000ABCD"));
        assert!(!id_valido("01m62ewvhrxm0000000000abcd") && !id_valido("CORTO") && !id_valido("01M62EWVHRXM0000000000ABCÑ"));
    }

    #[test]
    fn credenciales_fuera() {
        assert_eq!(sin_credenciales("https://ghp_x:y@github.com/a/b.git"), "https://***@github.com/a/b.git");
        assert_eq!(sin_credenciales("git clone https://tok@host/r falló; ssh://git@h:22/r"), "git clone https://***@host/r falló; ssh://***@h:22/r");
        assert_eq!(sin_credenciales("git@github.com:a/b.git"), "git@github.com:a/b.git");
        assert_eq!(error_corto("git push: remote: Permission to x denied to ghp\nmás"), "git push:");
    }

    #[test]
    fn rutas_por_base() {
        let a = rutas(Path::new("/x/omoikane.db"));
        assert!(a.config.ends_with("sync.json") && a.clon.ends_with("sync"));
        let b = rutas(Path::new("/x/trabajo.db"));
        assert!(b.config.ends_with("trabajo.sync.json") && b.clon.ends_with("trabajo.sync") && b.cerrojo.ends_with("trabajo.sync.lock"));
    }
}
