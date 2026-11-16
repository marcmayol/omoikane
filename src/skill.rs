//! De memoria a skill del repositorio (1.10, de Beacon: `beacon memory skills install`). Una memoria
//! se recupera cuando el recall la trae; una skill la carga el propio agente cuando su descripción
//! encaja con la tarea, y si se sube al repositorio llega a todo el equipo y a todos los agentes, con
//! Omoikane instalado o sin él. `.agents/skills/<nombre>/SKILL.md` lo leen Codex, Cursor, Gemini CLI,
//! Copilot, OpenCode y otros; Claude Code lee `.claude/skills`. Se escriben las dos copias (un enlace
//! simbólico pide permisos en Windows).
//!
//! Lo que Beacon no hace: cada fichero lleva en la cabecera el id de la memoria y una huella de lo
//! generado. Con ella se sabe si la memoria cambió después (la skill está **desfasada** y `refresh` la
//! pone al día) o si alguien editó el fichero a mano (**editada**: no se pisa sin `force`). Tampoco se
//! escribe una skill cuyo texto parezca llevar un secreto, porque el fichero va a acabar en un commit.

use crate::memoria::{Memoria, Tipo};
use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Las carpetas de skills del repositorio: la común y la de Claude Code.
pub const CARPETAS: [&str; 2] = [".agents/skills", ".claude/skills"];
/// El largo máximo del nombre y de la descripción (especificación de Agent Skills).
const MAX_NOMBRE: usize = 64;
const MAX_DESCRIPCION: usize = 1024;

/// A qué carpetas se escribe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destino {
    Ambas,
    Agentes,
    Claude,
}

impl Destino {
    pub fn de(texto: Option<&str>) -> Result<Destino> {
        Ok(match texto.map(|t| t.trim().to_lowercase()).as_deref() {
            None | Some("") | Some("both") | Some("ambas") => Destino::Ambas,
            Some("agents") | Some("agentes") => Destino::Agentes,
            Some("claude") => Destino::Claude,
            Some(otro) => bail!(crate::tr!("unknown target «{otro}»: use both, agents or claude", "destino desconocido «{otro}»: usa both, agents o claude")),
        })
    }

    fn carpetas(self) -> &'static [&'static str] {
        match self {
            Destino::Ambas => &CARPETAS,
            Destino::Agentes => &CARPETAS[..1],
            Destino::Claude => &CARPETAS[1..],
        }
    }
}

/// En qué estado está una skill generada por Omoikane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Estado {
    /// Dice lo mismo que la memoria.
    UpToDate,
    /// La memoria cambió después: `refresh` la pone al día.
    Outdated,
    /// Alguien la editó a mano: no se pisa sin `force`.
    Edited,
    /// La memoria ya no existe (olvidada, o de otra base).
    Orphan,
}

/// Una skill del repositorio que generó Omoikane.
#[derive(Clone, Debug, Serialize)]
pub struct Skill {
    /// La ruta del `SKILL.md`, relativa al repositorio y con `/`.
    pub path: String,
    pub memory_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: Estado,
}

/// Lo que hizo `escribir` en cada carpeta.
#[derive(Clone, Debug, Serialize)]
pub struct Escrita {
    pub path: String,
    /// `created`, `updated`, `unchanged`, o `skipped` con el motivo en `reason`.
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// La cabecera que importa de un `SKILL.md`.
#[derive(Debug, Default, PartialEq)]
struct Cabecera {
    id: Option<String>,
    huella: Option<String>,
    descripcion: String,
    cuerpo: String,
}

/// El nombre de la skill: el título sin acentos, en minúsculas, con letras, cifras y `-`.
pub fn nombre(m: &Memoria) -> String {
    let mut r = String::new();
    for c in crate::nombres::sin_acentos(&m.title).chars() {
        if c.is_ascii_alphanumeric() {
            r.push(c.to_ascii_lowercase());
        } else if !r.is_empty() && !r.ends_with('-') {
            r.push('-');
        }
        if r.len() >= MAX_NOMBRE {
            break;
        }
    }
    let r = r.trim_matches('-').to_string();
    if r.is_empty() { format!("memoria-{}", sufijo(&m.id)) } else { r }
}

fn sufijo(id: &str) -> String {
    id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<String>().to_lowercase()
}

/// La descripción, que decide cuándo la carga el agente: el título y, en una receta, sus palabras
/// `when:`; si no, los ficheros a los que se refiere. Una línea.
pub fn descripcion(m: &Memoria) -> String {
    let titulo = m.title.trim().trim_end_matches('.');
    let cuando = if m.kind == Tipo::Procedure { palabras_when(m) } else { Vec::new() };
    let mut d = if !cuando.is_empty() {
        let w = cuando.join(", ");
        if en_espanol(m) { format!("{titulo}. Úsala cuando: {w}.") } else { format!("{titulo}. Use it when: {w}.") }
    } else if !m.paths.is_empty() {
        let p = m.paths.join(", ");
        if en_espanol(m) { format!("{titulo}. Se aplica a: {p}.") } else { format!("{titulo}. Applies to: {p}.") }
    } else {
        format!("{titulo}.")
    };
    d = d.split_whitespace().collect::<Vec<_>>().join(" ");
    if d.chars().count() > MAX_DESCRIPCION {
        d = d.chars().take(MAX_DESCRIPCION - 1).collect::<String>() + "…";
    }
    d
}

/// Las palabras de la línea `when:` (o `cuando:`) de una receta, tal como las escribió el usuario.
fn palabras_when(m: &Memoria) -> Vec<String> {
    m.content
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            let n = l.to_lowercase();
            ["when:", "cuando:"].iter().find(|p| n.starts_with(*p)).map(|p| l[p.len()..].to_string())
        })
        .flat_map(|r| r.split(',').map(|w| w.trim().trim_matches(['"', '\'', '«', '»', '`', '.']).to_string()).collect::<Vec<_>>())
        .filter(|w| w.chars().count() >= 3)
        .collect()
}

/// El cuerpo: el título, el texto de la memoria y de dónde sale.
fn cuerpo(m: &Memoria) -> String {
    let corto: String = m.id.chars().take(12).collect();
    let fecha = m.updated_at.get(..10).unwrap_or(&m.updated_at);
    format!(
        "# {}\n\n{}\n\n---\n\n{}\n",
        m.title.trim(),
        m.content.trim(),
        if en_espanol(m) {
            format!(
                "Generada desde la memoria de Omoikane `{corto}` ({}, cambiada el {fecha}). `omoi skill --refresh` la pone al día cuando cambia la memoria; lo que se edite aquí a mano se respeta.",
                m.kind
            )
        } else {
            format!(
                "Generated from Omoikane memory `{corto}` ({}, changed {fecha}). `omoi skill --refresh` brings it up to date when the memory changes; edits made here by hand are kept.",
                m.kind
            )
        }
    )
}

/// Si la memoria está en español (las palabras fijas de la skill van en su idioma, no en el de la
/// CLI): tildes, eñes y signos de apertura, o más palabras frecuentes del español que del inglés.
fn en_espanol(m: &Memoria) -> bool {
    let t = format!(" {} {} ", m.title, m.content).to_lowercase();
    let marcas = t.chars().filter(|c| "áéíóúñ¿¡".contains(*c)).count();
    let cuenta = |ws: &[&str]| ws.iter().map(|w| t.matches(&format!(" {w} ")).count()).sum::<usize>();
    let es = cuenta(&["de", "que", "el", "la", "los", "las", "y", "en", "para", "con", "por", "un", "una", "se", "no"]);
    let en = cuenta(&["the", "and", "of", "to", "is", "with", "for", "in", "it", "on", "a", "an", "not", "be"]);
    marcas * 2 + es > en
}

/// La huella de lo generado: la descripción y el cuerpo, con los finales de línea en `\n` (git en
/// Windows puede pasarlos a `\r\n`) y sin espacios al final.
fn huella(descripcion: &str, cuerpo: &str) -> String {
    let normal = |t: &str| t.replace("\r\n", "\n").trim_end().to_string();
    let h = Sha256::digest(format!("{}\n{}", normal(descripcion), normal(cuerpo)).as_bytes());
    h.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// El `SKILL.md` entero de la memoria `m` con el nombre `n`.
pub fn generar(m: &Memoria, n: &str) -> String {
    let d = descripcion(m);
    let c = cuerpo(m);
    let h = huella(&d, &c);
    // La descripción va como texto JSON, que también es YAML válido: así ni `:` ni `#` la rompen.
    format!(
        "---\nname: {n}\ndescription: {}\nmetadata:\n  omoikane-id: \"{}\"\n  omoikane-hash: \"{h}\"\n---\n\n{c}",
        serde_json::to_string(&d).unwrap_or_default(),
        m.id
    )
}

fn leer_cabecera(texto: &str) -> Option<Cabecera> {
    let t = texto.replace("\r\n", "\n");
    let resto = t.strip_prefix("---\n")?;
    let fin = resto.find("\n---\n")?;
    let (cab, cuerpo) = (&resto[..fin], &resto[fin + 5..]);
    let mut c = Cabecera { cuerpo: cuerpo.trim_start_matches('\n').to_string(), ..Default::default() };
    let valor = |v: &str| -> String {
        let v = v.trim();
        serde_json::from_str::<String>(v).unwrap_or_else(|_| v.trim_matches(['"', '\'']).to_string())
    };
    for linea in cab.lines() {
        let l = linea.trim();
        if let Some(v) = linea.strip_prefix("description:") {
            c.descripcion = valor(v);
        } else if let Some(v) = l.strip_prefix("omoikane-id:") {
            c.id = Some(valor(v));
        } else if let Some(v) = l.strip_prefix("omoikane-hash:") {
            c.huella = Some(valor(v));
        }
    }
    Some(c)
}

/// La raíz del repositorio del proyecto, o un error si la sesión no está en uno.
fn raiz(project: &str) -> Result<PathBuf> {
    if project.is_empty() || crate::proyecto::es_general(project) {
        bail!(crate::tr!(
            "skills go in a repository: run it inside one (or pass --dir)",
            "las skills van en un repositorio: lánzalo dentro de uno (o pasa --dir)"
        ));
    }
    Ok(PathBuf::from(project))
}

fn relativa(raiz: &Path, p: &Path) -> String {
    p.strip_prefix(raiz).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Los `SKILL.md` generados por Omoikane en `carpeta` (de `CARPETAS`): (ruta, cabecera).
fn generadas_en(raiz: &Path, carpeta: &str) -> Vec<(PathBuf, Cabecera)> {
    let Ok(dir) = std::fs::read_dir(raiz.join(carpeta)) else { return Vec::new() };
    let mut v: Vec<(PathBuf, Cabecera)> = dir
        .flatten()
        .map(|e| e.path().join("SKILL.md"))
        .filter_map(|p| {
            let c = leer_cabecera(&std::fs::read_to_string(&p).ok()?)?;
            c.id.as_ref()?;
            Some((p, c))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn estado(db: &Connection, c: &Cabecera) -> (Estado, Option<Memoria>) {
    let Some(m) = c.id.as_deref().and_then(|id| crate::memoria::obtener(db, id).ok()) else { return (Estado::Orphan, None) };
    if c.huella.as_deref() != Some(huella(&c.descripcion, &c.cuerpo).as_str()) {
        return (Estado::Edited, Some(m));
    }
    let nueva = huella(&descripcion(&m), &cuerpo(&m));
    (if c.huella.as_deref() == Some(nueva.as_str()) { Estado::UpToDate } else { Estado::Outdated }, Some(m))
}

/// Una memoria del proyecto que podría ser skill, con el porqué.
#[derive(Clone, Debug, Serialize)]
pub struct Sugerencia {
    pub id: String,
    pub title: String,
    pub kind: Tipo,
    /// En cuántas sesiones la aplicó el agente (diario).
    pub applied_sessions: i64,
    pub recall_count: i64,
}

/// Cuántas sesiones aplicando una memoria la hacen candidata (las recetas lo son siempre).
pub const MIN_APLICADA: i64 = 2;
/// Cuántas se proponen como mucho.
pub const MAX_SUGERENCIAS: usize = 5;

/// Las memorias del proyecto (no las globales, ni las de su espacio u oficio: una skill es del
/// repositorio) que encajan como skill y aún no lo son: las recetas, y las que el agente aplicó en
/// [`MIN_APLICADA`] sesiones o más. Primero las más aplicadas, luego las más recordadas. Sin las que
/// parecen llevar un secreto. Es lo que hace que se usen: sin ellas, hay que ir a buscar el id.
pub fn sugerencias(db: &Connection, project: &str) -> Result<Vec<Sugerencia>> {
    raiz(project)?;
    let Some((pid, _)) = crate::proyecto::resolver(db, project)? else { return Ok(Vec::new()) };
    if pid == 0 {
        return Ok(Vec::new());
    }
    let ya: std::collections::BTreeSet<String> = listar(db, project)?.into_iter().map(|s| s.memory_id).collect();
    let todas = crate::memoria::recientes(db, &crate::memoria::Ambito::de(db, project)?, None, usize::MAX >> 1)?;
    let mut r: Vec<Sugerencia> = Vec::new();
    for m in todas.into_iter().filter(|m| m.project_id == pid && !ya.contains(&m.id)) {
        let (aplicada, _) = crate::diario::aplicada(db, &m.id)?;
        if m.kind != Tipo::Procedure && aplicada < MIN_APLICADA {
            continue;
        }
        if !crate::secretos::hallazgos(&format!("{}\n{}", m.title, m.content)).is_empty() {
            continue;
        }
        r.push(Sugerencia { id: m.id, title: m.title, kind: m.kind, applied_sessions: aplicada, recall_count: m.recall_count });
    }
    r.sort_by(|a, b| (b.applied_sessions, b.recall_count).cmp(&(a.applied_sessions, a.recall_count)).then_with(|| a.id.cmp(&b.id)));
    r.truncate(MAX_SUGERENCIAS);
    Ok(r)
}

/// La memoria que se pide para hacerla skill: un id (o su principio) o unas palabras. Con palabras,
/// se busca en lo que se ve desde el proyecto: si una sale claramente por delante (es la única, o
/// tiene más palabras de la consulta que la segunda), esa; si no, un error que lista las candidatas
/// con su id, para elegir.
pub fn resolver(db: &mut Connection, project: &str, pedida: &str) -> Result<String> {
    let p = pedida.trim();
    if !p.contains(char::is_whitespace)
        && let Ok(id) = crate::memoria::resolver_id(db, p)
    {
        return Ok(id);
    }
    let o = crate::buscar::Opciones { ambito: crate::memoria::Ambito::de(db, project)?, kind: None, tags: Vec::new(), limite: 5 };
    let mut rs = crate::buscar::buscar_sin_apuntar(db, p, &o)?;
    // La búsqueda puntúa casi igual a las que comparten palabras; quien pide una skill nombra su
    // título. Se ordena por cuántas palabras de la consulta están en el título (sin tildes), y en
    // empate se queda el orden de la búsqueda.
    let palabras = |t: &str| -> std::collections::BTreeSet<String> {
        crate::nombres::sin_acentos(t).split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_string).collect()
    };
    let consulta = palabras(p);
    let en_titulo = |m: &Memoria| palabras(&m.title).intersection(&consulta).count();
    rs.sort_by_key(|r| std::cmp::Reverse(en_titulo(&r.memoria)));
    match rs.as_slice() {
        [] => bail!(crate::tr!("no memory matches «{p}»", "ninguna memoria encaja con «{p}»")),
        [una] => Ok(una.memoria.id.clone()),
        [a, b, ..] if en_titulo(&a.memoria) > en_titulo(&b.memoria) => Ok(a.memoria.id.clone()),
        varias => {
            let lista: Vec<String> = varias.iter().map(|r| format!("  {}  {}", r.memoria.id.chars().take(12).collect::<String>(), r.memoria.title)).collect();
            bail!(crate::tr!(
                "several memories match «{p}»; pick one by its id:\n{}",
                "varias memorias encajan con «{p}»; elige una por su id:\n{}",
                lista.join("\n")
            ))
        }
    }
}

/// Las skills que generó Omoikane en el repositorio de `project`, con su estado.
pub fn listar(db: &Connection, project: &str) -> Result<Vec<Skill>> {
    let raiz = raiz(project)?;
    let mut r = Vec::new();
    for carpeta in CARPETAS {
        for (p, c) in generadas_en(&raiz, carpeta) {
            let (state, m) = estado(db, &c);
            r.push(Skill { path: relativa(&raiz, &p), memory_id: c.id.unwrap_or_default(), title: m.map(|m| m.title), state });
        }
    }
    Ok(r)
}

/// Escribe (o pone al día) la skill de la memoria `id` en el repositorio de `project`. Con `simulacro`
/// no toca nada y dice lo que haría. Sin `force` no pisa un fichero editado a mano ni uno que no sea
/// de Omoikane, y no escribe una memoria que parezca llevar un secreto.
pub fn escribir(db: &Connection, project: &str, id: &str, destino: Destino, force: bool, simulacro: bool) -> Result<Vec<Escrita>> {
    let raiz = raiz(project)?;
    let m = crate::memoria::obtener(db, id)?;
    crate::memoria::en_ambito(db, &crate::memoria::Ambito::de(db, project)?, &m.id)?;
    let secretos = crate::secretos::tipos(&crate::secretos::hallazgos(&format!("{}\n{}", m.title, m.content)));
    if !secretos.is_empty() && !force {
        bail!(crate::tr!(
            "the memory looks like it holds a secret ({}): a skill ends up in a commit. Fix the memory, or pass force if it is a false alarm",
            "la memoria parece llevar un secreto ({}): una skill acaba en un commit. Corrige la memoria, o pasa force si es una falsa alarma",
            crate::secretos::resumen(&secretos)
        ));
    }
    let mut r = Vec::new();
    for carpeta in destino.carpetas() {
        // Si ya hay una skill de esta memoria (quizá con el nombre de un título anterior), esa misma.
        let propia = generadas_en(&raiz, carpeta).into_iter().find(|(_, c)| c.id.as_deref() == Some(m.id.as_str()));
        let ruta = match &propia {
            Some((p, _)) => p.clone(),
            None => {
                let base = nombre(&m);
                let p = raiz.join(carpeta).join(&base).join("SKILL.md");
                // El nombre lo tiene otra skill: se le añade el final del id.
                if p.exists() && !force { raiz.join(carpeta).join(format!("{base}-{}", sufijo(&m.id))).join("SKILL.md") } else { p }
            }
        };
        let n = ruta.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| nombre(&m));
        let texto = generar(&m, &n);
        let rel = relativa(&raiz, &ruta);
        let antes = std::fs::read_to_string(&ruta).ok();
        let saltar = |motivo: String| Escrita { path: rel.clone(), action: "skipped", reason: Some(motivo) };
        let action = match (&antes, &propia) {
            (Some(a), _) if a.replace("\r\n", "\n") == texto => "unchanged",
            (Some(_), Some((_, c))) if !force && estado(db, c).0 == Estado::Edited => {
                r.push(saltar(crate::tr!("edited by hand: pass force to replace it", "editada a mano: pasa force para sustituirla").to_string()));
                continue;
            }
            (Some(_), None) if !force => {
                r.push(saltar(crate::tr!("a skill that isn't from Omoikane is already there", "ahí ya hay una skill que no es de Omoikane").to_string()));
                continue;
            }
            (Some(_), _) => "updated",
            (None, _) => "created",
        };
        if !simulacro && action != "unchanged" {
            if let Some(d) = ruta.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&ruta, &texto)?;
        }
        r.push(Escrita { path: rel, action, reason: None });
    }
    Ok(r)
}

/// Pone al día las skills desfasadas del repositorio (no las editadas ni las huérfanas).
pub fn refrescar(db: &Connection, project: &str, simulacro: bool) -> Result<Vec<Escrita>> {
    let raiz = raiz(project)?;
    let mut r = Vec::new();
    for carpeta in CARPETAS {
        for (p, c) in generadas_en(&raiz, carpeta) {
            let (Estado::Outdated, Some(m)) = estado(db, &c) else { continue };
            let n = p.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| nombre(&m));
            if !simulacro {
                std::fs::write(&p, generar(&m, &n))?;
            }
            r.push(Escrita { path: relativa(&raiz, &p), action: "updated", reason: None });
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memoria(titulo: &str, contenido: &str, kind: Tipo) -> Memoria {
        Memoria {
            id: "01M64R0KKD0AXYZ123456789AB".into(),
            kind,
            title: titulo.into(),
            content: contenido.into(),
            updated_at: "2026-11-16T10:00:00Z".into(),
            ..Default::default()
        }
    }

    #[test]
    fn nombres_validos() {
        let m = memoria("Publicar una versión de Omoikane (crates.io)", "", Tipo::Procedure);
        assert_eq!(nombre(&m), "publicar-una-version-de-omoikane-crates-io");
        let largo = memoria(&"palabra ".repeat(30), "", Tipo::Fact);
        assert!(nombre(&largo).len() <= MAX_NOMBRE && !nombre(&largo).ends_with('-'));
        assert_eq!(nombre(&memoria("«»", "", Tipo::Fact)), "memoria-6789ab");
    }

    #[test]
    fn la_receta_dice_cuando() {
        let m = memoria("Publicar una versión", "1. tag\n2. push\n\nwhen: publica, saca versión", Tipo::Procedure);
        let d = descripcion(&m);
        assert!(d.contains("publica") && d.contains("saca versión"), "{d}");
        assert!(d.contains("Úsala cuando"), "{d}");
        let mut g = memoria("Don't kill the emulator", "taskkill leaves the lock file behind", Tipo::Gotcha);
        g.paths = vec!["scripts/emu.ps1".into()];
        assert_eq!(descripcion(&g), "Don't kill the emulator. Applies to: scripts/emu.ps1.");
    }

    #[test]
    fn la_cabecera_se_lee_y_la_huella_cuadra() {
        let m = memoria("Título: con dos puntos # y almohadilla", "Paso 1\nPaso 2", Tipo::Procedure);
        let t = generar(&m, "titulo");
        let c = leer_cabecera(&t).unwrap();
        assert_eq!(c.id.as_deref(), Some(m.id.as_str()));
        assert_eq!(c.descripcion, descripcion(&m));
        assert_eq!(c.huella.as_deref(), Some(huella(&c.descripcion, &c.cuerpo).as_str()));
        // Con los finales de línea de Windows, la misma huella.
        let c2 = leer_cabecera(&t.replace('\n', "\r\n")).unwrap();
        assert_eq!(huella(&c2.descripcion, &c2.cuerpo), c.huella.unwrap());
        // Editada: otra huella.
        let c3 = leer_cabecera(&t.replace("Paso 2", "Paso 2 bis")).unwrap();
        assert_ne!(huella(&c3.descripcion, &c3.cuerpo), c3.huella.unwrap());
    }

    #[test]
    fn destinos() {
        assert_eq!(Destino::de(None).unwrap(), Destino::Ambas);
        assert_eq!(Destino::de(Some("claude")).unwrap().carpetas(), &[".claude/skills"]);
        assert!(Destino::de(Some("otro")).is_err());
    }
}
