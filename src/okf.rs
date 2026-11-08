//! Exportar e importar en OKF v0.2 (1.8.5): el Open Knowledge Format de Google
//! (<https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md>), el que usa
//! okf-agent-memory. Un bundle es una carpeta de Markdown con cabecera YAML: un fichero por concepto
//! (`type` es lo único obligatorio), `index.md` en las carpetas (el de la raíz con
//! `okf_version: "0.2"`) y un `log.md` con los cambios por fecha.
//!
//! - `omoi export --okf <carpeta>`: un concepto por memoria en `<grupo>/<tipo>/<título>.md`, con los
//!   grupos y el ámbito del export de Obsidian (lo personal y lo global, o un espacio sin lo global;
//!   nunca dos espacios en el mismo bundle). Es un espejo: solo escribe lo que cambia y solo borra lo
//!   suyo (los conceptos con `omoikane:` en la cabecera y los índices con la marca del final). Una
//!   carpeta con un bundle de otro (un `index.md` o `log.md` sin la marca) no se toca.
//! - `omoi import --okf <carpeta>`: cada concepto, una memoria guardada por clave (`remember --key`):
//!   la que traiga el concepto si salió de Omoikane, `okf:<su id>` si no. Importar dos veces no
//!   duplica. Va al proyecto de la carpeta (o `--dir`; lo global de esta base, a lo global), con
//!   `--space` a ese espacio y con `--global` a lo global. Solo lo firmado por esta base (`firma`)
//!   entra global, fijado o verificado; lo olvidado aquí no vuelve. Título, contenido y datos pasan por la redacción de secretos, como cualquier `remember`.
//!
//! Cómo se corresponden los campos (en los dos sentidos):
//!
//! | Omoikane | OKF |
//! |---|---|
//! | `kind` | `type`: `Fact`, `Decision`, `Gotcha`, `Preference`, `Procedure` (al importar, los de otros por su nombre: `Runbook` → procedure, `Convention` → preference…; lo demás, fact, y el `type` original se guarda) |
//! | título y contenido | `title`, el cuerpo; `description` es la primera frase |
//! | `tags` | `tags` |
//! | `paths` | `code_refs` |
//! | `review:AAAA-MM-DD` | `stale_after: AAAA-MM-DDT00:00:00Z` |
//! | verificada por una persona (el sello de `omoi verify`, `verificado`) | `verified: [{ by: human:<quién>, at }]`, con la huella del texto en `omoikane.verified_sha` (al importar, el `verified` de una persona pone el sello con su fecha si el concepto lleva la firma de esta base y el texto es el verificado, o con `--trust-verified`; nunca dentro de la shell de un agente; una etiqueta `verified` es una etiqueta más) |
//! | sustituida por otra (veredicto vigente) | `status: deprecated` y el enlace a la que la sustituye; al importar, el veredicto vuelve |
//! | etiquetas `draft` y `deprecated` | `status: draft` / `status: deprecated` |
//! | `source` | `generated.by` (`<agente>/omoikane`), con `generated.at` = su último cambio |
//! | el resto (id, clave, fijada, fechas, autor, datos) | `omoikane:` (una clave más, que OKF manda conservar) |
//!
//! Lo que trae un concepto de otro y Omoikane no tiene (`sources`, `resource`, `generated`, los
//! `verified` de procesos, `governance`…) se guarda en los datos de la memoria (`data.okf`) y vuelve a
//! salir igual al exportar.

use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, Cambios, Memoria, Nueva, Tipo};
use crate::obsidian::{Que, corto};
use crate::{tr, veredictos};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// La versión de OKF que se escribe.
pub const VERSION: &str = "0.2";
const BORRADOR: &str = "draft";
const OBSOLETA: &str = "deprecated";
/// Dónde van, dentro de los datos de una memoria, los campos de OKF que Omoikane no tiene.
const DATOS_OKF: &str = "okf";
/// La marca de los índices y del registro que escribe Omoikane (lo que no la lleva no se toca).
const MARCA: &str = "<!-- omoikane: rewritten on every export -->";
/// Lo que va detrás en el cuerpo de un concepto (los enlaces a otros) y no es su contenido.
const PIE: &str = "<!-- omoikane: links -->";
/// El concepto de resumen de cada grupo (`<grupo>/overview.md`).
const RESUMEN: &str = "overview";
/// Las claves de la cabecera que se convierten en campos de la memoria.
const MAPEADAS: [&str; 8] = ["type", "title", "description", "tags", "status", "stale_after", "code_refs", "omoikane"];

// --- Exportar ----------------------------------------------------------------------------------------

/// Lo que hizo una exportación (contando solo los conceptos; los índices y el registro van aparte).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Cuenta {
    pub memories: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    /// Los conceptos de resumen, uno por grupo.
    pub hubs: usize,
}

/// «3 memorias», «1 memoria».
fn cuantas(n: usize) -> String {
    if n == 1 { tr!("1 memory", "1 memoria").to_string() } else { tr!("{n} memories", "{n} memorias") }
}

/// Lo que lista el `index.md` de una carpeta: sus subcarpetas y sus conceptos (fichero, título,
/// descripción).
type Indice = (BTreeSet<String>, Vec<(String, String, String)>);

struct Grupo {
    carpeta: String,
    titulo: String,
}

/// Exporta al bundle `dir` y devuelve qué ha cambiado.
pub fn exportar(db: &Connection, dir: &Path, que: Que) -> Result<Cuenta> {
    crate::proyecto::exigir_local(dir)?;
    for f in ["index.md", "log.md"] {
        let p = dir.join(f);
        if p.exists() && !es_nuestro_indice(&p) {
            bail!(tr!(
                "{} is not from an Omoikane export: choose an empty folder (to bring that bundle in, use `omoi import --okf`)",
                "{} no es de una exportación de Omoikane: elige una carpeta vacía (para traer ese bundle, `omoi import --okf`)",
                p.display()
            ));
        }
    }
    let ambito = match que {
        Que::Personal => Ambito::Espacio(None),
        Que::Espacio(s) => Ambito::Espacio(Some(s)),
    };
    let mut todas = memoria::recientes(db, &ambito, None, usize::MAX >> 1)?;
    if matches!(que, Que::Espacio(_)) {
        todas.retain(|m| !m.global());
    }
    todas.sort_by(|a, b| a.id.cmp(&b.id));
    std::fs::create_dir_all(dir).with_context(|| tr!("could not create {}", "no se pudo crear {}", dir.display()))?;
    let datos = memoria::datos_de(db, &todas.iter().map(|m| m.id.as_str()).collect::<Vec<_>>())?;
    // La firma de esta base: al importarlo aquí, lo firmado se reconoce como propio (ver `firma`).
    let clave = firma::clave(db, true)?;

    // El grupo de cada memoria, como en el export de Obsidian.
    let mut nombres: HashMap<String, BTreeSet<String>> = HashMap::new();
    for m in &todas {
        if m.project_id != 0 {
            nombres.entry(crate::proyecto::nombre_de(&m.project, &m.project_key)).or_default().insert(m.project_key.clone());
        }
    }
    let grupo = |m: &Memoria| -> Grupo {
        if m.project_id == 0 {
            return if m.space.is_empty() {
                Grupo { carpeta: "_global".into(), titulo: tr!("Global", "Global").to_string() }
            } else {
                Grupo { carpeta: format!("_{}", slug(&m.space, 40)), titulo: m.space.clone() }
            };
        }
        let nombre = crate::proyecto::nombre_de(&m.project, &m.project_key);
        let mut s = slug(&nombre, 40);
        if nombres.get(&nombre).is_some_and(|k| k.len() > 1) {
            s = format!("{s}-{}", corto(&m.project_key));
        }
        Grupo { carpeta: s, titulo: nombre }
    };

    // La ruta de cada concepto (su id OKF: sin `.md`), única sin mirar mayúsculas.
    // Cada grupo tiene su concepto de resumen (`<grupo>/overview`), que enlaza a todas sus memorias.
    let mut titulos_grupo: BTreeMap<String, String> = BTreeMap::new();
    for m in &todas {
        let g = grupo(m);
        titulos_grupo.insert(g.carpeta, g.titulo);
    }
    let mut ocupadas: BTreeSet<String> = titulos_grupo.keys().map(|g| format!("{g}/{RESUMEN}").to_lowercase()).collect();
    let mut ruta: HashMap<String, String> = HashMap::new();
    for m in &todas {
        let g = grupo(m);
        let sub = memoria::clave_de(m).and_then(ruta_de_clave).unwrap_or_else(|| format!("{}/{}", slug(m.tipo_texto(), 30), slug(&m.title, 60)));
        let mut r = format!("{}/{sub}", g.carpeta);
        let ultimo = r.rsplit('/').next().unwrap_or("").to_string();
        if ocupadas.contains(&r.to_lowercase()) || matches!(ultimo.as_str(), "index" | "log") || reservado(&ultimo) {
            r = format!("{r}-{}", corto(&m.id));
        }
        ocupadas.insert(r.to_lowercase());
        ruta.insert(m.id.clone(), r);
    }

    // Veredictos vigentes entre lo exportado.
    let mut sustituida_por: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sustituye_a: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut contradice: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in veredictos::parejas(db)?.into_values() {
        if !ruta.contains_key(&v.memory_id) || !ruta.contains_key(&v.other_id) {
            continue;
        }
        if v.relation == veredictos::SUSTITUIDA {
            sustituida_por.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            sustituye_a.entry(v.other_id).or_default().push(v.memory_id);
        } else if v.relation == veredictos::CONTRADICE {
            contradice.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            contradice.entry(v.other_id).or_default().push(v.memory_id);
        }
    }
    for v in [&mut sustituida_por, &mut sustituye_a, &mut contradice] {
        v.values_mut().for_each(|x| {
            x.sort();
            x.dedup();
        });
    }

    let titulo_de: HashMap<&str, &str> = todas.iter().map(|m| (m.id.as_str(), m.title.as_str())).collect();
    let mut c = Cuenta { memories: todas.len(), ..Cuenta::default() };
    let mut escritos: BTreeSet<PathBuf> = BTreeSet::new();
    // Lo que va en cada índice: carpeta → sus subcarpetas y sus conceptos.
    let mut carpetas: BTreeMap<String, Indice> = BTreeMap::new();
    let mut registro: Vec<(String, String, bool, String)> = Vec::new();
    // Lo de cada grupo, para su resumen: (ruta, título, descripción, último cambio).
    let mut de_grupo: BTreeMap<String, Vec<(String, String, String, String)>> = BTreeMap::new();
    for m in &todas {
        let r = &ruta[&m.id];
        let enlace = |id: &String| format!("[{}](/{}.md)", texto_de_enlace(titulo_de[id.as_str()]), ruta[id]);
        let rutas_de = |ids: Option<&Vec<String>>| -> Vec<String> { ids.map(|v| v.iter().map(|id| format!("/{}.md", ruta[id])).collect()).unwrap_or_default() };
        let mut pie = Vec::new();
        for (rotulo, ids) in [("Superseded by", sustituida_por.get(&m.id)), ("Supersedes", sustituye_a.get(&m.id)), ("Contradicts", contradice.get(&m.id))] {
            if let Some(v) = ids.filter(|v| !v.is_empty()) {
                pie.push(format!("- {rotulo}: {}", v.iter().map(enlace).collect::<Vec<_>>().join(", ")));
            }
        }
        let rel = Relaciones { sustituida_por: rutas_de(sustituida_por.get(&m.id)), sustituye_a: rutas_de(sustituye_a.get(&m.id)), contradice: rutas_de(contradice.get(&m.id)) };
        let descripcion = descripcion(&m.content, &m.title);
        let texto = concepto(m, r, &grupo(m).titulo, datos.get(&m.id), &rel, &descripcion, &pie, clave.as_deref());
        let fichero = dir.join(format!("{r}.md"));
        escribir(&fichero, &texto, Some(&mut c))?;
        escritos.insert(fichero);
        // Al índice de su carpeta, y cada carpeta al de la de arriba.
        let (padre, nombre) = r.rsplit_once('/').unwrap_or(("", r));
        carpetas.entry(padre.to_string()).or_default().1.push((format!("{nombre}.md"), m.title.clone(), descripcion.clone()));
        de_grupo.entry(grupo(m).carpeta).or_default().push((r.clone(), m.title.clone(), descripcion, m.updated_at.clone()));
        let mut d = padre.to_string();
        while !d.is_empty() {
            let (arriba, esta) = match d.rsplit_once('/') {
                Some((a, e)) => (a.to_string(), e.to_string()),
                None => (String::new(), d.clone()),
            };
            carpetas.entry(arriba.clone()).or_default().0.insert(esta);
            d = arriba;
        }
        registro.push((m.updated_at.clone(), r.clone(), m.created_at.get(..10) == m.updated_at.get(..10), m.title.clone()));
    }

    // El resumen de cada grupo: sin él, cada memoria sería un concepto suelto (okf-agent-memory da
    // `okf validate --strict` por fallido si un concepto no enlaza ni recibe enlaces de otro).
    for (carpeta, v) in &mut de_grupo {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        let titulo = titulos_grupo.get(carpeta).cloned().unwrap_or_else(|| carpeta.clone());
        let descripcion = tr!("The Omoikane memories of {titulo}.", "Las memorias de Omoikane de {titulo}.");
        let cuando = v.iter().map(|x| x.3.as_str()).max().unwrap_or("").to_string();
        let mut f = Map::new();
        f.insert("type".into(), json!("Overview"));
        f.insert("title".into(), json!(titulo));
        f.insert("description".into(), json!(descripcion));
        f.insert("generated".into(), json!({ "by": format!("omoikane/{}", env!("CARGO_PKG_VERSION")), "at": cuando }));
        f.insert("omoikane".into(), json!({ "hub": true }));
        let mut s = String::from("---\n");
        for (k, x) in &f {
            yaml::bloque(&mut s, k, x, 0);
        }
        s += &format!("---\n\n# {}\n\n", texto_de_enlace(&titulo));
        for (r, t, d, _) in v.iter() {
            s += &format!("* [{}](/{r}.md) - {d}\n", texto_de_enlace(t));
        }
        let fichero = dir.join(carpeta).join(format!("{RESUMEN}.md"));
        escribir(&fichero, &s, None)?;
        escritos.insert(fichero);
        carpetas.entry(carpeta.clone()).or_default().1.push((format!("{RESUMEN}.md"), titulo.clone(), descripcion));
        carpetas.entry(String::new()).or_default().0.insert(carpeta.clone());
        c.hubs += 1;
    }

    // Los índices: el de la raíz con `okf_version`, los demás sin cabecera.
    let cuantas_en = |carpeta: &str| ruta.values().filter(|r| r.starts_with(&format!("{carpeta}/"))).count();
    carpetas.entry(String::new()).or_default();
    for (carpeta, (subs, conceptos)) in &carpetas {
        let mut s = String::new();
        if carpeta.is_empty() {
            s += &format!("---\nokf_version: \"{VERSION}\"\n---\n\n# {}\n\n", tr!("Omoikane memories", "Memorias de Omoikane"));
        } else {
            let nombre = carpeta.rsplit('/').next().unwrap_or(carpeta);
            let titulo = if carpeta.contains('/') { nombre.to_string() } else { titulos_grupo.get(carpeta).cloned().unwrap_or_else(|| nombre.to_string()) };
            s += &format!("# {}\n\n", texto_de_enlace(&titulo));
        }
        for sub in subs {
            let completa = if carpeta.is_empty() { sub.clone() } else { format!("{carpeta}/{sub}") };
            let titulo = if carpeta.is_empty() { titulos_grupo.get(sub).cloned().unwrap_or_else(|| sub.clone()) } else { sub.clone() };
            s += &format!("* [{}]({sub}/) - {}\n", texto_de_enlace(&titulo), cuantas(cuantas_en(&completa)));
        }
        let mut conceptos = conceptos.clone();
        conceptos.sort_by(|a, b| a.0.cmp(&b.0));
        for (f, t, d) in &conceptos {
            s += &format!("* [{}]({f}) - {d}\n", texto_de_enlace(t));
        }
        if subs.is_empty() && conceptos.is_empty() {
            s += &format!("{}\n", tr!("Nothing exported yet.", "Todavía no hay nada exportado."));
        }
        s += &format!("\n{MARCA}\n");
        let p = if carpeta.is_empty() { dir.join("index.md") } else { dir.join(carpeta).join("index.md") };
        escribir(&p, &s, None)?;
        escritos.insert(p);
    }

    // El registro: por día de su último cambio, de lo más nuevo a lo más viejo.
    registro.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut s = format!("# {}\n", tr!("Omoikane memory log", "Registro de las memorias de Omoikane"));
    let mut dia = String::new();
    for (cuando, r, nueva, titulo) in &registro {
        let d = cuando.get(..10).unwrap_or(cuando);
        if d != dia {
            s += &format!("\n## {d}\n");
            dia = d.to_string();
        }
        let que = if *nueva { "**Creation**" } else { "**Update**" };
        s += &format!("* {que}: [{}](/{r}.md)\n", texto_de_enlace(titulo));
    }
    s += &format!("\n{MARCA}\n");
    let p = dir.join("log.md");
    escribir(&p, &s, None)?;
    escritos.insert(p);

    c.deleted = limpiar(dir, &escritos)?;
    Ok(c)
}

/// Un texto en NFC (letras compuestas: «é» como un solo carácter). macOS (HFS+) guarda los nombres de
/// fichero en NFD («e» y el acento aparte) y los devuelve así al listar: comparar o firmar rutas sin
/// normalizar daba otro nombre para el mismo concepto (un duplicado al importar, o un borrado al
/// reexportar en espejo).
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// El `slug` de Obsidian sobre el texto en NFC: en NFD, el acento suelto no es una letra y «café»
/// salía `cafe`.
fn slug(s: &str, max: usize) -> String {
    crate::obsidian::slug(&nfc(s), max)
}

/// Una ruta como texto en NFC, para comparar las escritas con las que da el listado de la carpeta.
fn ruta_nfc(p: &Path) -> String {
    nfc(&p.to_string_lossy())
}

/// Si dos rutas son el mismo fichero en el disco: en macOS, el nombre NFC con el que se escribió y el
/// NFD con el que lo lista HFS+. Fuera de Unix (NTFS no normaliza), dos nombres distintos son dos ficheros.
#[cfg(unix)]
fn mismo_fichero(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn mismo_fichero(_: &Path, _: &Path) -> bool {
    false
}

struct Relaciones {
    sustituida_por: Vec<String>,
    sustituye_a: Vec<String>,
    contradice: Vec<String>,
}

/// La ruta de un concepto que vino de otro bundle (`okf:<su id>`), para que vuelva a salir donde
/// estaba; las claves de Omoikane (`okf:<id de memoria>`) y las demás, no.
fn ruta_de_clave(clave: &str) -> Option<String> {
    let id = clave.strip_prefix("okf:")?;
    if es_ulid(id) {
        return None;
    }
    let partes: Vec<&str> = id.split('/').collect();
    if partes.iter().any(|p| !p.chars().any(char::is_alphanumeric)) {
        return None;
    }
    Some(partes.iter().map(|p| slug(p, 60)).collect::<Vec<_>>().join("/"))
}

/// Un id de memoria que llega de fuera, en la forma en que lo guarda la base (`None` si no es un ULID).
pub fn id_normal(s: &str) -> Option<String> {
    ulid::Ulid::from_string(s.trim()).ok().map(|u| u.to_string())
}

fn es_ulid(s: &str) -> bool {
    s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Los nombres que Windows no deja usar como fichero.
fn reservado(k: &str) -> bool {
    let k = k.to_lowercase();
    matches!(k.as_str(), "con" | "prn" | "aux" | "nul") || (k.len() == 4 && (k.starts_with("com") || k.starts_with("lpt")) && k.as_bytes()[3].is_ascii_digit())
}

/// El texto de un enlace Markdown: en una línea y sin corchetes sueltos.
fn texto_de_enlace(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ").replace('[', "\\[").replace(']', "\\]")
}

/// La `description` de OKF: una frase, en una línea. La primera línea con texto del contenido, sin
/// marcas de título, hasta el primer punto (como mucho 200 caracteres); si no hay, el título.
fn descripcion(contenido: &str, titulo: &str) -> String {
    let linea = contenido
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim_start_matches(['-', '*', '>']).trim())
        .find(|l| !l.is_empty() && !l.starts_with("```"))
        .unwrap_or(titulo);
    let mut frase = linea.to_string();
    for fin in [". ", "? ", "! "] {
        if let Some(i) = frase.find(fin) {
            frase.truncate(i + 1);
        }
    }
    let frase: String = frase.split_whitespace().collect::<Vec<_>>().join(" ");
    if frase.chars().count() > 200 { format!("{}…", frase.chars().take(199).collect::<String>().trim_end()) } else { frase }
}

/// El `type` canónico de cada tipo.
fn tipo_okf(t: &str) -> String {
    let mut c = t.chars();
    match c.next() {
        Some(p) => p.to_uppercase().chain(c).collect(),
        None => "Fact".into(),
    }
}

/// El tipo de Omoikane de un `type` de OKF (de cualquiera: no hay lista cerrada).
pub fn tipo_de_okf(t: &str) -> Tipo {
    let t: String = t.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let tiene = |ps: &[&str]| ps.iter().any(|p| t == *p || t.split(' ').any(|w| w == *p));
    if tiene(&["decision", "adr", "decisions"]) {
        Tipo::Decision
    } else if tiene(&["gotcha", "pitfall", "caveat", "warning", "bug", "bugfix", "issue", "trap", "antipattern"]) {
        Tipo::Gotcha
    } else if tiene(&["preference", "convention", "guideline", "style", "rule", "policy", "standard"]) {
        Tipo::Preference
    } else if tiene(&["procedure", "playbook", "runbook", "process", "howto", "how to", "recipe", "workflow", "guide", "steps"]) {
        Tipo::Procedure
    } else {
        Tipo::Fact
    }
}

/// El actor de `generated.by` (`<productor>/<versión>`): el agente que la guardó, a través de Omoikane.
fn actor(fuente: &str) -> String {
    let f = fuente.trim();
    let f = f.strip_prefix("mcp:").unwrap_or(f);
    let productor = slug(f.split(':').next().unwrap_or(""), 40);
    format!("{}/omoikane", if f.is_empty() { "omoikane".to_string() } else { productor })
}

/// Las entradas de un `verified` de OKF (una lista o un mapa suelto).
fn entradas_verificadas(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    }
}

fn por_de(x: &Value) -> &str {
    x.get("by").and_then(Value::as_str).unwrap_or("")
}

/// Las verificaciones: la de una persona solo si la memoria lleva el sello y vale (`m.verified`, el de
/// `omoi verify`, la web o una importación; una etiqueta no cuenta), delante; y las de procesos que
/// trajo de OKF (`data.okf.verified`), tal cual.
fn verificaciones(m: &Memoria, guardadas: Option<&Value>) -> Vec<Value> {
    let mut v: Vec<Value> = Vec::new();
    if let Some(s) = &m.verified {
        v.push(json!({ "by": format!("human:{}", id_de_actor(&s.by)), "at": s.at }));
    }
    v.extend(entradas_verificadas(guardadas).into_iter().filter(|x| !es_humano(por_de(x))));
    v
}

/// El id de un actor de OKF no lleva espacios (`human:\S+`): el nombre de quien verificó, con los
/// espacios como `%20` (y `%` como `%25`), para que vuelva igual al importarlo.
fn id_de_actor(nombre: &str) -> String {
    let mut r = String::new();
    for c in nombre.chars() {
        match c {
            '%' => r.push_str("%25"),
            c if c.is_whitespace() => r.push_str("%20"),
            c => r.push(c),
        }
    }
    r
}

/// Lo contrario de [`id_de_actor`]: `%20` es un espacio y `%25`, `%`; lo demás, tal cual.
fn nombre_de_actor(id: &str) -> String {
    let mut r = String::new();
    let mut resto = id;
    while let Some(i) = resto.find('%') {
        r.push_str(&resto[..i]);
        let tras = &resto[i..];
        let (c, n) = if tras.starts_with("%20") {
            (" ", 3)
        } else if tras.starts_with("%25") {
            ("%", 3)
        } else {
            ("%", 1)
        };
        r.push_str(c);
        resto = &tras[n..];
    }
    r.push_str(resto);
    r
}

/// Quién verificó, de un actor de persona (`human:<id>`, `human/<id>`; `human` a secas, nadie).
fn persona_de(by: &str) -> Option<String> {
    let b = by.trim();
    let resto = b.get(5..).filter(|_| b.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("human")))?;
    let q = nombre_de_actor(resto.strip_prefix(':').or_else(|| resto.strip_prefix('/'))?);
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    // El sello admite hasta 100 caracteres.
    (!q.is_empty()).then(|| q.chars().take(100).collect::<String>().trim_end().to_string())
}

/// Un actor de persona, en la forma de OKF (`human:<id>`) o en la de okf-agent-memory (`human/…`).
fn es_humano(by: &str) -> bool {
    let b = by.trim().to_lowercase();
    b == "human" || b.starts_with("human:") || b.starts_with("human/")
}

#[allow(clippy::too_many_arguments)]
fn concepto(m: &Memoria, id_concepto: &str, proyecto: &str, datos: Option<&Value>, rel: &Relaciones, descripcion: &str, pie: &[String], clave: Option<&[u8]>) -> String {
    // Lo que trajo de OKF y Omoikane no tiene, y lo demás de sus datos.
    let (okf, resto): (Map<String, Value>, Option<Value>) = match datos {
        Some(Value::Object(o)) => {
            let mut o = o.clone();
            let okf = match o.remove(DATOS_OKF) {
                Some(Value::Object(x)) => x,
                _ => Map::new(),
            };
            (okf, (!o.is_empty()).then_some(Value::Object(o)))
        }
        Some(otro) => (Map::new(), Some(otro.clone())),
        None => (Map::new(), None),
    };
    let tipo = okf.get("type").and_then(Value::as_str).filter(|t| tipo_de_okf(t).as_str() == m.tipo_texto()).map(str::to_string).unwrap_or_else(|| tipo_okf(m.tipo_texto()));
    let mut f = Map::new();
    f.insert("type".into(), json!(tipo));
    f.insert("title".into(), json!(m.title));
    f.insert("description".into(), json!(descripcion));
    let es_estado = |t: &str| t == BORRADOR || t == OBSOLETA;
    let tags: Vec<&String> = m
        .tags
        .iter()
        .filter(|t| !t.starts_with(crate::revisar::ETIQUETA_FECHA) && !t.starts_with(memoria::ETIQUETA_TEMA) && !es_estado(t))
        .collect();
    if !tags.is_empty() {
        f.insert("tags".into(), json!(tags));
    }
    let estado = if !rel.sustituida_por.is_empty() || m.tags.iter().any(|t| t == OBSOLETA) {
        Some(OBSOLETA)
    } else if m.tags.iter().any(|t| t == BORRADOR) {
        Some(BORRADOR)
    } else {
        None
    };
    if let Some(e) = estado {
        f.insert("status".into(), json!(e));
    }
    if let Some(d) = crate::revisar::fecha_revision(&m.tags) {
        f.insert("stale_after".into(), json!(format!("{d}T00:00:00Z")));
    }
    // `code_refs` son rutas del repositorio: las absolutas (y las que salen con `..`) van aparte.
    let (relativas, otras): (Vec<&String>, Vec<&String>) = m.paths.iter().partition(|p| es_relativa(p));
    if !relativas.is_empty() {
        f.insert("code_refs".into(), json!(relativas));
    }
    let por = okf.get("generated").and_then(|g| g.get("by")).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| actor(&m.source));
    // Con el sello (que vale solo si el texto es el que vio la persona), el texto no ha cambiado desde
    // que se verificó aunque después cambiaran etiquetas o rutas: `generated.at` no pasa de esa fecha
    // (OKF da por superada una verificación anterior a `generated.at`). La fecha de verdad va en
    // `omoikane.updated`.
    let generada = match &m.verified {
        Some(v) if v.at < m.updated_at => &v.at,
        _ => &m.updated_at,
    };
    f.insert("generated".into(), json!({ "by": por, "at": generada }));
    let verificado = verificaciones(m, okf.get("verified"));
    if !verificado.is_empty() {
        f.insert("verified".into(), Value::Array(verificado));
    }
    let mut guardadas = Vec::new();
    for (k, v) in &okf {
        if !f.contains_key(k) && k != "omoikane" {
            f.insert(k.clone(), v.clone());
        }
        guardadas.push(k.clone());
    }
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    if let Some(k) = memoria::clave_de(m) {
        o.insert("key".into(), json!(k));
    }
    o.insert("kind".into(), json!(m.tipo_texto()));
    if m.global() {
        o.insert("scope".into(), json!("global"));
    } else if m.project_id != 0 {
        o.insert("project".into(), json!(proyecto));
        if !m.project_key.is_empty() {
            o.insert("project_key".into(), json!(m.project_key));
        }
    }
    if !m.space.is_empty() {
        o.insert("space".into(), json!(m.space));
    }
    if !otras.is_empty() {
        o.insert("paths".into(), json!(otras));
    }
    if m.pinned {
        o.insert("pinned".into(), json!(true));
    }
    if !m.source.is_empty() {
        o.insert("source".into(), json!(m.source));
    }
    if !m.author.is_empty() {
        o.insert("author".into(), json!(m.author));
    }
    o.insert("created".into(), json!(m.created_at));
    o.insert("updated".into(), json!(m.updated_at));
    for (k, v) in [("superseded_by", &rel.sustituida_por), ("supersedes", &rel.sustituye_a), ("contradicts", &rel.contradice)] {
        if !v.is_empty() {
            o.insert(k.into(), json!(v));
        }
    }
    if !guardadas.is_empty() {
        o.insert("okf_keys".into(), json!(guardadas));
    }
    if let Some(d) = resto {
        o.insert("data".into(), d);
    }
    // La huella del texto verificado: al importarlo, el sello vuelve solo si el texto sigue siendo ese
    // (reeditar el Markdown no se queda con la verificación).
    let sha = m.verified.as_ref().map(|_| crate::verificado::huella(m.tipo_texto(), &m.title, &m.content)).unwrap_or_default();
    if !sha.is_empty() {
        o.insert("verified_sha".into(), json!(sha));
    }
    f.insert("omoikane".into(), Value::Object(o.clone()));
    let escrito = |f: &Map<String, Value>| {
        let mut s = String::from("---\n");
        for (k, v) in f {
            yaml::bloque(&mut s, k, v, 0);
        }
        s += "---\n\n";
        s += m.content.trim_end();
        s += "\n";
        if !pie.is_empty() {
            s += &format!("\n{PIE}\n{}\n", pie.join("\n"));
        }
        s
    };
    let s = escrito(&f);
    // La firma, de lo que la importación sacará de este mismo texto (ver `firma`): se lee como se leerá
    // al importarlo y se firma eso. Si no se pudiera leer (no pasa), va sin firma: entraría como de otro.
    if let Some(k) = clave
        && let Some((cabecera, cuerpo)) = separar(&s)
        && let Ok(cabecera) = yaml::leer(cabecera)
        && let Ok(e) = entrada(&Concepto { id: id_concepto.to_string(), cabecera, cuerpo: cuerpo.to_string() })
    {
        o.insert("sig".into(), json!(firma::firmar(k, &texto_firmado(&e))));
        f.insert("omoikane".into(), Value::Object(o));
        return escrito(&f);
    }
    s
}

/// Una ruta dentro del repositorio: ni absoluta (`/x`, `C:/x`, `\\servidor`) ni con `..`.
fn es_relativa(p: &str) -> bool {
    let p = p.replace('\\', "/");
    !p.starts_with('/') && !p.starts_with('~') && !p.contains(':') && !p.split('/').any(|x| x == "..")
}

/// Escribe solo si cambia (y lo cuenta, si es un concepto).
fn escribir(ruta: &Path, texto: &str, c: Option<&mut Cuenta>) -> Result<()> {
    let hay = std::fs::read(ruta).ok();
    if hay.as_deref() == Some(texto.as_bytes()) {
        if let Some(c) = c {
            c.unchanged += 1;
        }
        return Ok(());
    }
    if let Some(c) = c {
        if hay.is_some() {
            c.updated += 1;
        } else {
            c.created += 1;
        }
    }
    if let Some(p) = ruta.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(ruta, texto).with_context(|| tr!("could not write {}", "no se pudo escribir {}", ruta.display()))
}

/// Borra lo de Omoikane que ya no sale (y las carpetas que se queden vacías): los conceptos con
/// `omoikane:` en la cabecera y los índices con la marca. Las carpetas que empiezan por `.` no se miran.
fn limpiar(raiz: &Path, escritos: &BTreeSet<PathBuf>) -> Result<usize> {
    let mut borrados = 0;
    // Lo escrito, por su nombre en NFC: el listado de HFS+ lo devuelve en NFD.
    let por_nfc: HashMap<String, &PathBuf> = escritos.iter().map(|p| (ruta_nfc(p), p)).collect();
    // Escrito con este nombre, o el mismo fichero con el nombre en otra forma (HFS+). Un fichero aparte
    // con el mismo nombre en NFD (un bundle de un Mac copiado a Linux o Windows) es una copia vieja del
    // concepto: se borra como lo que ya no sale.
    let escrito = |p: &Path| escritos.contains(p) || por_nfc.get(&ruta_nfc(p)).is_some_and(|e| mismo_fichero(p, e));
    let mut carpetas = Vec::new();
    let mut pendientes = vec![raiz.to_path_buf()];
    while let Some(d) = pendientes.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if !e.file_name().to_string_lossy().starts_with('.') {
                    carpetas.push(p.clone());
                    pendientes.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "md") && !escrito(&p) {
                let reservado = matches!(e.file_name().to_str(), Some("index.md" | "log.md"));
                if (reservado && es_nuestro_indice(&p)) || (!reservado && es_nuestro_concepto(&p)) {
                    std::fs::remove_file(&p)?;
                    borrados += 1;
                }
            }
        }
    }
    carpetas.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in carpetas {
        let _ = std::fs::remove_dir(&d); // solo si está vacía
    }
    Ok(borrados)
}

fn es_nuestro_indice(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|t| t.contains(MARCA))
}

fn es_nuestro_concepto(p: &Path) -> bool {
    let Ok(t) = std::fs::read_to_string(p) else { return false };
    let Some((cabecera, _)) = separar(&t) else { return false };
    cabecera.lines().any(|l| l.starts_with("omoikane:"))
}

// --- Firma de esta base ----------------------------------------------------------------------------

/// Cómo se sabe que un concepto salió de ESTA base: cada uno lleva en `omoikane.sig` un HMAC-SHA256,
/// con una clave aleatoria de esta base (`meta('okf_firma')`, que no viaja con la sincronización ni con
/// `omoi export`), de TODO lo que la importación aplica de él ([`texto_firmado`]): su ruta, su id, su
/// clave, su proyecto, si es global o fijada, su tipo, título y contenido, sus rutas, sus etiquetas
/// (también las de `status` y `stale_after`), sus datos, sus fechas, su origen, sus relaciones
/// (`superseded_by`, `contradicts`) y su verificación. Al exportar se calcula leyendo el concepto ya
/// escrito igual que al importarlo, así que lo firmado es exactamente lo que se aplicará. Un bundle de
/// otro (otra persona, otra base, uno escrito a mano o retocado en cualquier campo) no lo puede
/// calcular: nada de eso se cree (el texto sí se importa, como el de cualquier bundle, en una memoria
/// nueva). Que los ids existan aquí no basta: cualquiera que haya visto un bundle tuyo los tiene.
mod firma {
    use anyhow::{Result, anyhow};
    use rusqlite::{Connection, OptionalExtension, params};

    const META: &str = "okf_firma";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn de_hex(s: &str) -> Option<Vec<u8>> {
        let s = s.trim();
        if !s.len().is_multiple_of(2) || !s.is_ascii() {
            return None;
        }
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
    }

    /// La clave de esta base; con `crear`, la crea si aún no hay (al exportar). Al importar sin clave,
    /// nada es propio.
    pub fn clave(db: &Connection, crear: bool) -> Result<Option<Vec<u8>>> {
        let hay: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [META], |f| f.get(0)).optional()?;
        if let Some(k) = hay.as_deref().and_then(de_hex).filter(|k| k.len() == 32) {
            return Ok(Some(k));
        }
        if !crear {
            return Ok(None);
        }
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(|e| anyhow!("getrandom: {e}"))?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![META, hex(&b)])?;
        Ok(Some(b.to_vec()))
    }

    pub fn firmar(clave: &[u8], texto: &str) -> String {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, clave);
        hex(ring::hmac::sign(&k, texto.as_bytes()).as_ref())
    }

    pub fn vale(clave: &[u8], texto: &str, firma: &str) -> bool {
        let Some(f) = de_hex(firma) else { return false };
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, clave);
        ring::hmac::verify(&k, texto.as_bytes(), &f).is_ok()
    }
}

/// Lo que se firma de un concepto: todo lo que la importación saca de él y aplica (ver `firma`), tal
/// como sale de [`entrada`], en JSON (los mapas en el orden en que vienen: otro orden es otra firma).
/// Menos la firma misma y lo que no se aplica (el pie de enlaces, `description` de un concepto propio).
fn texto_firmado(e: &Entrada) -> String {
    let n = &e.nueva;
    let v = json!({
        "concept": e.concepto,
        "id": e.id,
        "key": e.su_clave,
        "match_key": e.clave,
        "global": e.global,
        "project_key": e.project_key,
        "kind": n.kind.map(|k| k.as_str()),
        "kind_text": n.kind_text,
        "title": n.title,
        "content": n.content,
        "paths": n.paths,
        "tags": n.tags,
        "pinned": n.pinned,
        "source": n.source,
        "data": e.datos,
        "created": e.creada,
        "updated": e.cambiada,
        "generated": e.generada,
        "superseded_by": e.sustituida_por,
        "contradicts": e.contradice,
        "verified": e.verificada,
        "verified_sha": e.verified_sha,
    });
    format!("omoikane-okf-3\n{v}")
}

// --- Leer un bundle --------------------------------------------------------------------------------

/// Un concepto leído: su id (la ruta sin `.md`), su cabecera y su cuerpo.
#[derive(Clone, Debug)]
pub struct Concepto {
    pub id: String,
    pub cabecera: Map<String, Value>,
    pub cuerpo: String,
}

/// Un bundle leído y comprobado.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub raiz: PathBuf,
    /// El `okf_version` del `index.md` de la raíz, si lo dice.
    pub version: Option<String>,
    pub conceptos: Vec<Concepto>,
    /// Lo que se ha saltado al leerlo (enlaces simbólicos, ficheros demasiado grandes), para decirlo.
    pub avisos: Vec<String>,
}

/// Un concepto más grande no se lee (se salta avisando): una cabecera y una nota, no un volcado.
pub const MAX_CONCEPTO: u64 = 1 << 20;

/// Un fichero cuya cabecera empieza con `---\r\n` lo ha pasado a `\r\n` entero quien lo copió (git con
/// `core.autocrlf=true`, lo de serie en Git para Windows; un editor): Omoikane escribe siempre `\n`.
/// Vuelve a `\n` entero, para que un concepto de esta base siga valiendo con su firma (se firmó con
/// `\n`) y no entre como uno nuevo, duplicado y sin su verificación. Un contenido con `\r\n` de verdad
/// va en un fichero con la cabecera en `\n`, y no se toca.
fn sin_crlf_de_git(texto: String) -> String {
    let t = texto.strip_prefix('\u{feff}').unwrap_or(&texto);
    if t.starts_with("---\r\n") { texto.replace("\r\n", "\n") } else { texto }
}

/// La cabecera y el cuerpo de un Markdown, si empieza por una cabecera `---` cerrada.
fn separar(texto: &str) -> Option<(&str, &str)> {
    let t = texto.strip_prefix('\u{feff}').unwrap_or(texto);
    let resto = t.strip_prefix("---\r\n").or_else(|| t.strip_prefix("---\n"))?;
    let mut i = 0;
    for l in resto.split_inclusive('\n') {
        let limpia = l.trim_end_matches(['\n', '\r']);
        if limpia == "---" || limpia == "..." {
            return Some((&resto[..i], &resto[i + l.len()..]));
        }
        i += l.len();
    }
    // Una cabecera que acaba en el último `---` sin salto de línea.
    if resto.ends_with("\n---") {
        return Some((&resto[..resto.len() - 3], ""));
    }
    None
}

/// Lee un bundle OKF y comprueba que lo es (OKF §11): cada `.md` que no sea `index.md` ni `log.md`
/// tiene una cabecera YAML legible con un `type` no vacío. Si la carpeta no tiene `index.md` pero sí
/// `knowledge/`, lee esa (la carpeta de un repositorio con su bundle).
pub fn leer(dir: &Path) -> Result<Bundle> {
    leer_con_topes(dir, &Topes::default())
}

/// Cuántas entradas (ficheros y carpetas) se recorren como mucho al leer un bundle.
pub const MAX_FICHEROS: usize = 20_000;
/// Cuántos bytes de conceptos se leen como mucho al leer un bundle. Un bundle de verdad es mucho
/// menos: el de una base real de 230 memorias ocupa 0,4 MB, y 20.000 conceptos de 2 KB son 40 MB.
pub const MAX_BYTES: u64 = 50 << 20;
/// Lo más que mide la cabecera YAML de un concepto (o de `index.md`). Una de verdad no pasa de unos
/// pocos KB (la de un concepto exportado de Omoikane, con su firma, ~1 KB); lo demás va en el cuerpo.
pub const MAX_CABECERA: usize = 256 << 10;
/// Lo más que miden todas las cabeceras juntas: 20.000 conceptos de 800 bytes.
pub const MAX_CABECERAS: usize = 16 << 20;
/// Cuántos valores YAML (cada texto, número, lista o mapa) se leen como mucho entre todas las
/// cabeceras. Leída, una cabecera ocupa en memoria unas 40 veces lo que mide si es todo listas
/// cortas (`[a, a, a, …]`): 192 MB de cabeceras así eran 60 s y 7,5 GB de RAM. Con este tope, unos
/// 200 MB como mucho; una cabecera de verdad tiene de 10 a 40 valores.
pub const MAX_NODOS: usize = 2_000_000;

/// Los topes al leer un bundle (ver las constantes; los tests los bajan).
#[derive(Clone, Copy, Debug)]
pub struct Topes {
    pub ficheros: usize,
    pub bytes: u64,
    pub cabecera: usize,
    pub cabeceras: usize,
    pub nodos: usize,
}

impl Default for Topes {
    fn default() -> Topes {
        Topes { ficheros: MAX_FICHEROS, bytes: MAX_BYTES, cabecera: MAX_CABECERA, cabeceras: MAX_CABECERAS, nodos: MAX_NODOS }
    }
}

/// Cuántos valores tiene un valor leído: él y los de dentro.
fn nodos(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(nodos).sum::<usize>(),
        Value::Object(o) => 1 + o.values().map(nodos).sum::<usize>(),
        _ => 1,
    }
}

/// El error de un bundle que pasa de un tope, para que se vea que no es un fallo de un concepto.
fn demasiado(raiz: &Path, que: String) -> anyhow::Error {
    anyhow!(tr!(
        "{} {que}: too much for a knowledge bundle (is it the right folder?); nothing was imported",
        "{} {que}: demasiado para un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
        raiz.display()
    ))
}

/// Si una entrada se puede leer sin seguir un enlace: ni enlace simbólico ni unión de Windows.
fn sin_enlace(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| !m.file_type().is_symlink()) && !es_union(p)
}

fn leer_con_topes(dir: &Path, topes: &Topes) -> Result<Bundle> {
    let (max_ficheros, max_bytes) = (topes.ficheros, topes.bytes);
    crate::proyecto::exigir_local(dir)?;
    if !dir.is_dir() {
        bail!(tr!("{} is not a folder", "{} no es una carpeta", dir.display()));
    }
    let conocimiento = dir.join("knowledge");
    let raiz = if !dir.join("index.md").exists() && sin_enlace(&conocimiento) && conocimiento.join("index.md").exists() { conocimiento } else { dir.to_path_buf() };
    let mut version = None;
    let mut avisos = Vec::new();
    let indice = raiz.join("index.md");
    // El `index.md`, con los mismos límites que un concepto: sin seguir enlaces y de 1 MB como mucho.
    let indice_vale = indice.exists() && {
        if !sin_enlace(&indice) {
            avisos.push(tr!("index.md: a symbolic link, skipped", "index.md: un enlace simbólico, saltado"));
            false
        } else if std::fs::metadata(&indice).is_ok_and(|m| m.len() > MAX_CONCEPTO) {
            avisos.push(tr!("index.md: over 1 MB, skipped", "index.md: de más de 1 MB, saltado"));
            false
        } else {
            true
        }
    };
    if indice_vale {
        let t = std::fs::read_to_string(&indice).with_context(|| indice.display().to_string())?;
        if let Some((cabecera, _)) = separar(&t).filter(|(c, _)| {
            let cabe = c.len() <= topes.cabecera;
            if !cabe {
                avisos.push(tr!("index.md: a YAML header over {} KB, skipped", "index.md: una cabecera YAML de más de {} KB, saltado", topes.cabecera >> 10));
            }
            cabe
        }) {
            let c = yaml::leer(cabecera).map_err(|e| anyhow!(tr!("{}: the YAML header can't be read: {e}", "{}: no se puede leer la cabecera YAML: {e}", indice.display())))?;
            version = c.get("okf_version").map(|v| match v {
                Value::String(s) => s.clone(),
                otro => otro.to_string(),
            });
        }
    }
    let mut ficheros = Vec::new();
    let (mut vistas, mut bytes) = (0usize, 0u64);
    let mut pendientes = vec![raiz.clone()];
    while let Some(d) = pendientes.pop() {
        let rd = std::fs::read_dir(&d).with_context(|| d.display().to_string())?;
        for e in rd.flatten() {
            vistas += 1;
            if vistas > max_ficheros {
                bail!(tr!(
                    "{} has over {max_ficheros} files and folders: that's not a knowledge bundle (is it the right folder?); nothing was imported",
                    "{} tiene más de {max_ficheros} ficheros y carpetas: eso no es un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
                    raiz.display()
                ));
            }
            let p = e.path();
            let nombre = e.file_name().to_string_lossy().to_string();
            if nombre.starts_with('.') {
                continue;
            }
            // El tipo de la entrada misma, sin seguir enlaces: un enlace simbólico (o una unión de
            // Windows) puede apuntar a una carpeta de arriba y no acabar nunca, o sacar el bundle fuera.
            let Ok(t) = e.file_type() else { continue };
            let rel = p.strip_prefix(&raiz).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if t.is_symlink() || es_union(&p) {
                avisos.push(tr!("{rel}: a symbolic link, skipped", "{rel}: un enlace simbólico, saltado"));
                continue;
            }
            if t.is_dir() {
                pendientes.push(p);
            } else if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")) && !matches!(nombre.as_str(), "index.md" | "log.md") {
                let largo = e.metadata().map(|m| m.len()).unwrap_or(0);
                if largo > MAX_CONCEPTO {
                    avisos.push(tr!("{rel}: over 1 MB, skipped", "{rel}: de más de 1 MB, saltado"));
                    continue;
                }
                bytes += largo;
                if bytes > max_bytes {
                    bail!(tr!(
                        "{} holds over {} MB of concepts: too much for a knowledge bundle (is it the right folder?); nothing was imported",
                        "{} tiene más de {} MB de conceptos: demasiado para un bundle de conocimiento (¿es la carpeta buena?); no se ha importado nada",
                        raiz.display(),
                        max_bytes >> 20
                    ));
                }
                ficheros.push(p);
            }
        }
    }
    ficheros.sort();
    let mut conceptos = Vec::new();
    let mut malos = Vec::new();
    // Primero, los textos y lo que miden sus cabeceras (sin leer el YAML): una cabecera enorme no se
    // llega a leer, y las de todos juntos tienen su tope antes de leer ninguna.
    let mut leidos: Vec<(String, String, String)> = Vec::new();
    let mut en_cabeceras = 0usize;
    for p in &ficheros {
        let rel = p.strip_prefix(&raiz).unwrap_or(p).to_string_lossy().replace('\\', "/");
        // En NFC: un bundle que pasó por un Mac (HFS+) lista sus nombres en NFD, y el id del concepto
        // (su ruta) entra en su clave y en su firma.
        let id = nfc(rel.strip_suffix(".md").or_else(|| rel.strip_suffix(".MD")).unwrap_or(&rel));
        let texto = match std::fs::read_to_string(p) {
            Ok(t) => sin_crlf_de_git(t),
            Err(e) => {
                malos.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let Some((cabecera, _)) = separar(&texto) else {
            malos.push(tr!("{rel}: no YAML header (--- … ---)", "{rel}: sin cabecera YAML (--- … ---)"));
            continue;
        };
        if cabecera.len() > topes.cabecera {
            malos.push(tr!(
                "{rel}: a YAML header over {} KB (a concept's header holds a few fields; the rest goes in the body)",
                "{rel}: una cabecera YAML de más de {} KB (la de un concepto lleva unos pocos campos; lo demás va en el cuerpo)",
                topes.cabecera >> 10
            ));
            continue;
        }
        en_cabeceras += cabecera.len();
        if en_cabeceras > topes.cabeceras {
            return Err(demasiado(&raiz, tr!("holds over {} MB of YAML headers", "tiene más de {} MB de cabeceras YAML", topes.cabeceras >> 20)));
        }
        leidos.push((rel, id, texto));
    }
    let mut en_nodos = 0usize;
    for (rel, id, texto) in leidos {
        let Some((cabecera, cuerpo)) = separar(&texto) else { continue };
        let c = match yaml::leer(cabecera) {
            Ok(c) => c,
            Err(e) => {
                malos.push(tr!("{rel}: the YAML header can't be read: {e}", "{rel}: no se puede leer la cabecera YAML: {e}"));
                continue;
            }
        };
        en_nodos += c.values().map(nodos).sum::<usize>();
        if en_nodos > topes.nodos {
            return Err(demasiado(&raiz, tr!("holds over {} values in its YAML headers", "tiene más de {} valores en sus cabeceras YAML", topes.nodos)));
        }
        if !c.get("type").is_some_and(|t| !escalar_texto(t).trim().is_empty()) {
            malos.push(tr!("{rel}: the header has no `type`", "{rel}: la cabecera no tiene `type`"));
            continue;
        }
        conceptos.push(Concepto { id, cabecera: c, cuerpo: cuerpo.to_string() });
    }
    if !malos.is_empty() {
        let n = malos.len();
        malos.truncate(5);
        bail!(tr!(
            "{} is not a valid OKF bundle ({n} files wrong; every .md other than index.md and log.md needs a YAML header with `type`):\n  {}",
            "{} no es un bundle OKF válido ({n} ficheros mal; cada .md que no sea index.md ni log.md necesita una cabecera YAML con `type`):\n  {}",
            raiz.display(),
            malos.join("\n  ")
        ));
    }
    if conceptos.is_empty() {
        bail!(tr!(
            "{} has no OKF concepts (Markdown files with a YAML header that says their `type`)",
            "{} no tiene conceptos OKF (ficheros Markdown con una cabecera YAML que diga su `type`)",
            raiz.display()
        ));
    }
    Ok(Bundle { raiz, version, conceptos, avisos })
}

/// Una unión (junction) de Windows: `file_type` no la da por enlace simbólico, pero es un punto de
/// montaje que puede llevar a cualquier carpeta (también a una de arriba).
#[cfg(windows)]
fn es_union(p: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const PUNTO_DE_REANALISIS: u32 = 0x400;
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_attributes() & PUNTO_DE_REANALISIS != 0)
}

#[cfg(not(windows))]
fn es_union(_: &Path) -> bool {
    false
}

fn escalar_texto(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        otro => otro.to_string(),
    }
}

fn lista_de_textos(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(escalar_texto).filter(|s| !s.trim().is_empty()).collect(),
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(otro) => vec![escalar_texto(otro)],
    }
}

// --- Importar ----------------------------------------------------------------------------------------

/// Adónde va lo importado.
#[derive(Clone, Debug)]
pub enum Destino {
    /// Al proyecto de esta carpeta (vacía: a lo global); lo que se exportó como global, a lo global.
    Proyecto(String),
    /// A este espacio, sin proyecto.
    Espacio(i64),
}

/// Lo que hizo (o haría, en seco) una importación.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Resumen {
    pub concepts: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub verdicts: usize,
    /// Las memorias a las que se puso (o en seco, se pondría) el sello de verificada por una persona.
    pub verified: usize,
    /// Las que traían el `verified` de una persona y se importaron sin sello (dentro de la shell de un
    /// agente).
    pub not_verified: usize,
    /// Las que traían el `verified` de una persona en un bundle que no es de esta base y se importaron
    /// sin sello (sin `--trust-verified`).
    pub untrusted_verified: usize,
    /// Las que traían el `verified` de una persona que ya no vale: el texto cambió desde entonces (no es
    /// el de `verified_sha`), la persona la desverificó después aquí, o la fecha es del futuro.
    pub stale_verified: usize,
    /// Las que entran (o entrarían) como globales.
    pub global: usize,
    /// Las que entran (o entrarían) fijadas.
    pub pinned: usize,
    /// Los conceptos que no salieron de esta base (sin la firma de aquí, ver `firma`).
    pub foreign: usize,
    /// De esos, los que pedían ser globales y van al destino.
    pub ignored_global: usize,
    /// Y los que pedían estar fijados y entran sin fijar.
    pub ignored_pinned: usize,
    /// Los que son de una memoria olvidada aquí (en la papelera o con su marca de olvido): no vuelven.
    pub skipped_forgotten: usize,
    /// Los de esta base más viejos que su memoria de aquí (exportados antes de cambiarla): no la tocan.
    pub skipped_older: usize,
    /// Los títulos de las memorias en las que se tapó algo que parecía un secreto.
    pub redacted: Vec<String>,
}

/// Una memoria lista para guardar, sacada de un concepto.
struct Entrada {
    concepto: String,
    /// La clave por la que se guarda si no vuelve con su id: la suya o la del concepto.
    clave: String,
    /// La clave que tenía en Omoikane (`remember --key`), si tenía.
    su_clave: Option<String>,
    /// El id de memoria que trae, si salió de Omoikane.
    id: Option<String>,
    global: bool,
    /// La clave de sincronización de su proyecto, si salió de uno de Omoikane.
    project_key: Option<String>,
    nueva: Nueva,
    datos: Option<Value>,
    creada: Option<String>,
    cambiada: Option<String>,
    sustituida_por: Vec<String>,
    contradice: Vec<String>,
    /// Quién la verificó y cuándo, si trae el `verified` de una persona.
    verificada: Option<(String, Option<String>)>,
    /// El `generated.at` del concepto: una verificación anterior está superada (OKF) y no da sello.
    generada: Option<String>,
    /// La huella del texto que se verificó (`omoikane.verified_sha`), si salió de Omoikane con sello.
    verified_sha: Option<String>,
    /// La firma de la base de la que salió (`omoikane.sig`, ver `firma`).
    firma: Option<String>,
}

/// La clave por la que se guarda un concepto: la que trae de Omoikane, `okf:<id de memoria>` si salió
/// de Omoikane sin clave, y `okf:<id del concepto>` si viene de otro sitio (en minúsculas, con lo que
/// no vale en una clave cambiado por `-`, y con un trozo de su suma si pasa de 80).
fn clave_de_concepto(id: &str, om: &Map<String, Value>) -> String {
    if let Some(k) = om.get("key").and_then(Value::as_str)
        && let Ok(k) = memoria::clave_tema(k)
    {
        return k;
    }
    if let Some(i) = om.get("id").and_then(Value::as_str).filter(|i| es_ulid(i)) {
        return format!("okf:{}", i.to_lowercase());
    }
    let limpio: String = id.to_lowercase().chars().map(|c| if c.is_alphanumeric() || "-_./".contains(c) { c } else { '-' }).collect();
    let k = format!("okf:{limpio}");
    if k.chars().count() <= 80 {
        return k;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(id.as_bytes());
    let corto: String = k.chars().take(71).collect();
    format!("{corto}-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}

/// Un instante ISO 8601 en la forma que entienden las funciones de fecha de SQLite.
fn instante_sql(s: &str) -> String {
    s.trim().trim_end_matches('Z').replace('T', " ")
}

/// Una fecha `AAAA-MM-DD` válida al principio de un instante ISO 8601.
fn dia_de(instante: &str) -> Option<String> {
    let d = instante.trim().get(..10)?;
    let b = d.as_bytes();
    let ok = b.len() == 10 && b[4] == b'-' && b[7] == b'-' && d.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    let (mes, dia): (u32, u32) = (d[5..7].parse().ok()?, d[8..10].parse().ok()?);
    (ok && (1..=12).contains(&mes) && (1..=31).contains(&dia)).then(|| d.to_string())
}

fn entrada(c: &Concepto) -> Result<Entrada> {
    let f = &c.cabecera;
    let om = match f.get("omoikane") {
        Some(Value::Object(o)) => o.clone(),
        _ => Map::new(),
    };
    let nuestra = !om.is_empty();
    let tipo_okf = escalar_texto(&f["type"]).trim().to_string();
    let kind = om.get("kind").and_then(Value::as_str).and_then(|k| k.parse::<Tipo>().ok()).unwrap_or_else(|| tipo_de_okf(&tipo_okf));
    let titulo = f.get("title").map(escalar_texto).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
        let nombre = c.id.rsplit('/').next().unwrap_or(&c.id);
        nombre.replace(['-', '_'], " ")
    });
    let mut cuerpo = c.cuerpo.as_str();
    if nuestra && let Some(i) = cuerpo.find(PIE) {
        cuerpo = &cuerpo[..i];
    }
    // El cuerpo de un concepto nuestro, tal cual (el contenido puede llevar `\r\n`); el de otro, con
    // los saltos de línea de Omoikane.
    let mut contenido = if nuestra { cuerpo.trim().to_string() } else { cuerpo.replace("\r\n", "\n").trim().to_string() };
    if let Some(d) = f.get("description").map(escalar_texto).map(|d| d.trim().to_string()).filter(|d| !d.is_empty())
        && !nuestra
        && !contenido.contains(&d)
    {
        contenido = if contenido.is_empty() { d } else { format!("{d}\n\n{contenido}") };
    }
    let mut titulo = titulo.split_whitespace().collect::<Vec<_>>().join(" ");
    if titulo.chars().count() > 200 {
        contenido = format!("{titulo}\n\n{contenido}");
        titulo = format!("{}…", titulo.chars().take(199).collect::<String>());
    }
    if contenido.trim().is_empty() {
        contenido = titulo.clone();
    }

    let mut tags: Vec<String> = lista_de_textos(f.get("tags")).into_iter().filter(|t| !t.starts_with(crate::revisar::ETIQUETA_FECHA)).collect();
    if let Some(d) = f.get("stale_after").map(escalar_texto).as_deref().and_then(dia_de) {
        tags.push(format!("{}{d}", crate::revisar::ETIQUETA_FECHA));
    }
    // La verificación de una persona (la más reciente, si hay varias): se convierte en el sello al
    // guardarla, no en datos ni en una etiqueta.
    let verificada = entradas_verificadas(f.get("verified"))
        .iter()
        .filter_map(|v| Some((persona_de(por_de(v))?, v.get("at").map(escalar_texto).filter(|a| !a.trim().is_empty()))))
        .max_by(|a, b| a.1.cmp(&b.1));
    let sustituida_por = lista_de_textos(om.get("superseded_by"));
    match f.get("status").map(escalar_texto).map(|s| s.trim().to_lowercase()).as_deref() {
        Some(OBSOLETA) if sustituida_por.is_empty() => tags.push(OBSOLETA.into()),
        Some(BORRADOR) => tags.push(BORRADOR.into()),
        _ => {}
    }

    // Lo de OKF que Omoikane no tiene, a los datos: de un concepto nuestro, solo lo que ya venía de
    // fuera (`okf_keys`); de uno de otro, todo lo que no se convierte en un campo.
    let mut okf = Map::new();
    if nuestra {
        for k in lista_de_textos(om.get("okf_keys")) {
            if let Some(v) = f.get(&k) {
                okf.insert(k, v.clone());
            }
        }
    } else {
        for (k, v) in f {
            if !MAPEADAS.contains(&k.as_str()) {
                okf.insert(k.clone(), v.clone());
            }
        }
        if tipo_okf != tipo_okf_de(kind) {
            okf.insert("type".into(), json!(tipo_okf));
        }
    }
    // De `verified`, a los datos solo las de procesos: las de personas son el sello (o nada).
    if let Some(v) = okf.remove("verified") {
        let procesos: Vec<Value> = entradas_verificadas(Some(&v)).into_iter().filter(|x| !es_humano(por_de(x))).collect();
        if !procesos.is_empty() {
            okf.insert("verified".into(), Value::Array(procesos));
        }
    }
    let datos = match (om.get("data").cloned(), okf.is_empty()) {
        (d, true) => d,
        (None, false) => Some(json!({ DATOS_OKF: okf })),
        (Some(Value::Object(mut o)), false) => {
            o.insert(DATOS_OKF.into(), Value::Object(okf));
            Some(Value::Object(o))
        }
        (Some(otro), false) => Some(json!({ "value": otro, DATOS_OKF: okf })),
    };
    // La fecha del texto: `generated.at`; sin ella, la verificación más reciente (el texto ya existía
    // entonces, y así no sale como superada al volver a exportarlo).
    let generado = f.get("generated").and_then(|g| g.get("at")).map(escalar_texto).or_else(|| {
        entradas_verificadas(f.get("verified")).iter().filter_map(|v| v.get("at").map(escalar_texto)).filter(|a| !a.trim().is_empty()).max()
    });
    let texto = |k: &str| om.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Entrada {
        concepto: c.id.clone(),
        clave: clave_de_concepto(&c.id, &om),
        su_clave: om.get("key").and_then(Value::as_str).and_then(|k| memoria::clave_tema(k).ok()),
        // En su forma (mayúsculas): `Ulid::from_string` también acepta minúsculas, y la base guarda y
        // compara los ids tal cual (un olvido con el id en minúsculas no casaba y la memoria volvía).
        id: texto("id").and_then(|i| id_normal(&i)),
        global: texto("scope").as_deref() == Some("global"),
        project_key: texto("project_key").filter(|k| !k.is_empty()),
        nueva: Nueva {
            kind: Some(kind),
            // Un tipo de una versión futura de Omoikane, tal cual (como en la sincronización).
            kind_text: om.get("kind").and_then(Value::as_str).filter(|k| !k.is_empty() && k.parse::<Tipo>().is_err()).map(str::to_string),
            title: titulo,
            content: contenido,
            paths: lista_de_textos(f.get("code_refs")).into_iter().chain(lista_de_textos(om.get("paths"))).collect(),
            tags,
            pinned: om.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            source: texto("source").unwrap_or_else(|| if nuestra { String::new() } else { "import:okf".into() }),
            ..Default::default()
        },
        datos,
        creada: texto("created").or_else(|| generado.clone()),
        cambiada: texto("updated").or(generado),
        sustituida_por,
        contradice: lista_de_textos(om.get("contradicts")),
        verificada,
        generada: f.get("generated").and_then(|g| g.get("at")).map(escalar_texto).filter(|a| !a.trim().is_empty()),
        verified_sha: texto("verified_sha").filter(|s| !s.is_empty()),
        firma: texto("sig").filter(|s| !s.is_empty()),
    })
}

fn tipo_okf_de(t: Tipo) -> String {
    tipo_okf(t.as_str())
}

fn es_resumen(c: &Concepto) -> bool {
    c.cabecera.get("omoikane").and_then(|o| o.get("hub")).and_then(Value::as_bool) == Some(true)
}

/// Cuántos conceptos de un bundle salieron de Omoikane (llevan `omoikane:`) fuera del espacio de clave
/// `espacio`: memorias personales (sin espacio) o de otro cliente. Importadas con `--space`, irían al
/// repositorio de ese espacio, que comparten otras personas.
pub fn de_fuera_del_espacio(b: &Bundle, espacio: &str) -> usize {
    b.conceptos
        .iter()
        .filter(|c| !es_resumen(c))
        .filter_map(|c| c.cabecera.get("omoikane").and_then(Value::as_object))
        .filter(|o| o.get("space").and_then(Value::as_str).unwrap_or("") != espacio)
        .count()
}

/// El espacio de un bundle exportado desde un espacio (el `omoikane.space` de sus conceptos).
pub fn espacio_de(b: &Bundle) -> Option<String> {
    b.conceptos.iter().find_map(|c| c.cabecera.get("omoikane").and_then(|o| o.get("space")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
}

/// Dónde cae una entrada: (carpeta del proyecto, id del proyecto, espacio) para `Nueva`. Lo que salió
/// de un proyecto que esta base también tiene (la misma clave de sincronización, en el mismo espacio
/// o sin espacio) vuelve a él; lo demás, al destino.
fn donde(db: &Connection, destino: &Destino, e: &Entrada) -> Result<(String, Option<i64>, Option<i64>)> {
    let suyo = |espacio: Option<i64>| -> Result<Option<i64>> {
        let Some(k) = &e.project_key else { return Ok(None) };
        Ok(db.query_row("SELECT id FROM project WHERE key = ?1 AND space_id IS ?2 AND id != 0 LIMIT 1", params![k, espacio], |f| f.get(0)).optional()?)
    };
    Ok(match destino {
        Destino::Espacio(s) => match suyo(Some(*s))? {
            Some(p) => (String::new(), Some(p), None),
            None => (String::new(), None, Some(*s)),
        },
        Destino::Proyecto(_) if e.global => (String::new(), None, None),
        Destino::Proyecto(p) => match suyo(None)? {
            Some(id) => (String::new(), Some(id), None),
            None => (p.clone(), None, None),
        },
    })
}

/// La memoria que ya tiene esa clave donde caería la entrada, con su sello (para saber si cambió).
fn existente(db: &Connection, proyecto: &str, project_id: Option<i64>, espacio: Option<i64>, clave: &str) -> Result<Option<(String, String)>> {
    let project_id = match (espacio, project_id) {
        (Some(_), _) => 0,
        (None, Some(p)) => p,
        (None, None) => match crate::proyecto::resolver(db, proyecto)? {
            Some((id, _)) => id,
            None => return Ok(None),
        },
    };
    Ok(db
        .query_row(
            "SELECT id, hlc FROM memory WHERE project_id = ?1 AND space_id IS ?2
               AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?3) ORDER BY created_at, id LIMIT 1",
            params![project_id, espacio, format!("{}{clave}", memoria::ETIQUETA_TEMA)],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?)
}

fn sello(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0))?)
}

/// Lo que pasó con un concepto al importarlo.
enum Hecho {
    Creada,
    Cambiada,
    Igual,
    /// De una memoria olvidada aquí: no vuelve.
    Olvidada,
}

/// Cómo se importa un bundle.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opciones {
    /// Solo contar lo que haría, sin escribir (`created`: lo que sería nuevo; `updated`: lo que ya está
    /// y se pondría al día).
    pub en_seco: bool,
    /// Fuera de la shell de un agente: el `verified` de una persona puede poner el sello de verificada
    /// (importar es una acción de la persona). Sin esto, esas memorias entran sin sello y se cuentan en
    /// `not_verified`.
    pub verificar: bool,
    /// `--trust-verified`: el `verified` de una persona de un bundle que no salió de esta base también
    /// pone el sello. Sin esto, solo el de lo firmado aquí (ver `firma`).
    pub confiar_verificadas: bool,
}

/// Importa un bundle ya leído, sin creer el `verified` de un bundle de otro (ver [`importar_con`]).
pub fn importar(db: &mut Connection, modelo: Option<&Modelo>, b: &Bundle, destino: &Destino, en_seco: bool, verificar: bool) -> Result<Resumen> {
    importar_con(db, modelo, b, destino, Opciones { en_seco, verificar, confiar_verificadas: false })
}

/// Importa un bundle ya leído.
///
/// Lo que da poder a un concepto (`omoikane.scope: global`, `pinned`, el `verified` de una persona, su
/// id y su clave —con qué memoria de aquí se junta— y su `project_key`) solo se cree si salió de ESTA
/// base: lleva su firma, que cubre también su texto (ver `firma`). De un bundle de otro (o de uno
/// propio retocado, también en el texto) va al destino pedido como memoria nueva, por la clave del
/// concepto, sin fijar y sin sello (el sello, con `confiar_verificadas`, sí). Lo que quede como
/// memoria de oficio pasa por `oficios::fugas`. Y aun con la firma, el sello vuelve solo si el texto es el que se
/// verificó (`omoikane.verified_sha`), la persona no la desverificó aquí después y la fecha no es del
/// futuro. Lo que se olvidó aquí (en la papelera o con su marca de olvido) no vuelve: se salta y se
/// cuenta en `skipped_forgotten` (como en la sincronización, donde un olvido para la memoria que llega;
/// para traerla, `omoi restore`).
pub fn importar_con(db: &mut Connection, modelo: Option<&Modelo>, b: &Bundle, destino: &Destino, op: Opciones) -> Result<Resumen> {
    if let (Destino::Proyecto(_), Some(s)) = (destino, espacio_de(b)) {
        bail!(tr!(
            "this bundle was exported from the space «{s}»: import it with --space <name>, so it doesn't mix with personal memories",
            "este bundle se exportó del espacio «{s}»: impórtalo con --space <nombre>, para que no se mezcle con lo personal"
        ));
    }
    // Lo personal exportado de Omoikane no entra en el proyecto de un cliente (uno en un espacio); un
    // bundle de otro (el `knowledge/` de su repositorio), sí.
    if let Destino::Proyecto(p) = destino
        && b.conceptos.iter().any(|c| c.cabecera.contains_key("omoikane"))
        && let Some((_, Some(_))) = crate::proyecto::resolver(db, p)?
    {
        bail!(tr!(
            "these are personal memories exported from Omoikane and this folder's project is in a space (a client's): import them from a personal project, or into a space with --space <name>",
            "son memorias personales exportadas de Omoikane y el proyecto de esta carpeta está en un espacio (de un cliente): impórtalas desde un proyecto personal, o a un espacio con --space <nombre>"
        ));
    }
    let clave = firma::clave(db, false)?;
    // Los resúmenes de grupo de una exportación de Omoikane no son memorias.
    let conceptos: Vec<&Concepto> = b.conceptos.iter().filter(|c| !es_resumen(c)).collect();
    let mut r = Resumen { concepts: conceptos.len(), ..Resumen::default() };
    // Id de concepto (en minúsculas) → id de memoria, si es de esta base y si se saltó por más viejo que
    // su memoria de aquí (`None`: dos conceptos con la misma ruta salvo mayúsculas, que no se sabe cuál
    // es), y las relaciones que trae cada uno.
    let mut ids: HashMap<String, Option<(String, bool, bool)>> = HashMap::new();
    // Las memorias que ya ha tocado esta importación.
    let mut tocadas: BTreeSet<String> = BTreeSet::new();
    let mut relaciones: Vec<(String, bool, Vec<String>, Vec<String>)> = Vec::new();
    for c in conceptos {
        match importar_uno(db, modelo, c, destino, op, clave.as_deref(), &mut tocadas) {
            Ok((u, e)) => {
                match u.hecho {
                    Hecho::Creada => r.created += 1,
                    Hecho::Cambiada => r.updated += 1,
                    Hecho::Igual => r.unchanged += 1,
                    Hecho::Olvidada => {
                        r.skipped_forgotten += 1;
                        continue;
                    }
                }
                match u.sello {
                    Sello::Puesto => r.verified += 1,
                    Sello::Agente => r.not_verified += 1,
                    Sello::Ajeno => r.untrusted_verified += 1,
                    Sello::Viejo => r.stale_verified += 1,
                    Sello::Nada | Sello::Ya => {}
                }
                r.foreign += usize::from(u.ajeno);
                r.ignored_global += usize::from(u.sin_global);
                r.ignored_pinned += usize::from(u.sin_fijar);
                r.global += usize::from(u.global);
                r.pinned += usize::from(e.nueva.pinned);
                if u.tapado {
                    r.redacted.push(e.nueva.title.clone());
                }
                r.skipped_older += usize::from(u.mas_viejo);
                if let Some(id) = u.id {
                    ids.entry(e.concepto.to_lowercase()).and_modify(|x| *x = None).or_insert_with(|| Some((id.clone(), u.propio, u.mas_viejo)));
                    // Las relaciones de uno que no se ha aplicado (más viejo que la memoria de aquí), no.
                    if !u.mas_viejo {
                        relaciones.push((id, u.propio, e.sustituida_por, e.contradice));
                    }
                }
            }
            Err(err) => {
                eprintln!("omoikane: {}: {err:#}", c.id);
                r.failed += 1;
            }
        }
    }
    if op.en_seco {
        return Ok(r);
    }
    // Los veredictos entre lo importado: `superseded_by` y `contradicts` llevan a otros conceptos del
    // bundle (`/ruta.md`). Uno que ya vale no se vuelve a poner.
    let vigentes = veredictos::parejas(db)?;
    let id_de = |ruta: &str| -> Option<&(String, bool, bool)> {
        let r = ruta.trim().trim_start_matches('/');
        let r = r.strip_suffix(".md").unwrap_or(r);
        ids.get(&nfc(r).to_lowercase()).and_then(Option::as_ref)
    };
    for (id, propio, sustituida_por, contradice) in &relaciones {
        for (relacion, otras) in [(veredictos::SUSTITUIDA, sustituida_por), (veredictos::CONTRADICE, contradice)] {
            for (otra, otra_propia, otra_vieja) in otras.iter().filter_map(|o| id_de(o)) {
                // Nunca entre un concepto de esta base y uno sin su firma: uno ajeno no puede dar por
                // sustituida (ni contradecir) una memoria de aquí, ni al revés con lo que diga un concepto
                // propio copiado junto a otros de fuera. Ni hacia uno que no se aplicó por viejo.
                if otra == id || otra_propia != propio || *otra_vieja {
                    continue;
                }
                let ya = vigentes.get(&veredictos::pareja(id, otra)).is_some_and(|v| v.relation == relacion && (relacion == veredictos::CONTRADICE || v.memory_id == *id));
                if ya {
                    continue;
                }
                match veredictos::poner(db, id, otra, relacion, "okf import", "import:okf") {
                    Ok(()) => r.verdicts += 1,
                    Err(err) => eprintln!("omoikane: {id}: {err:#}"),
                }
            }
        }
    }
    Ok(r)
}

/// Qué pasó con el `verified` de una persona de un concepto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sello {
    /// No traía.
    Nada,
    /// Se le ha puesto (en seco: se le pondría).
    Puesto,
    /// Ya tenía ese mismo.
    Ya,
    /// No, desde la shell de un agente.
    Agente,
    /// No, de un bundle que no es de esta base (sin `confiar_verificadas`).
    Ajeno,
    /// No, porque ya no vale (otro texto, desverificada después, fecha del futuro).
    Viejo,
}

/// Lo que pasó con un concepto al importarlo.
struct Uno {
    /// El id de su memoria (en seco, el de la que ya está, si está).
    id: Option<String>,
    hecho: Hecho,
    /// Si se tapó algo que parecía un secreto.
    tapado: bool,
    sello: Sello,
    /// Sin la firma de esta base.
    ajeno: bool,
    /// Pedía ser global (o estar fijada) y no se le ha hecho caso por ser de otro.
    sin_global: bool,
    sin_fijar: bool,
    /// Entra como global.
    global: bool,
    /// Lleva la firma de esta base.
    propio: bool,
    /// Es de esta base pero más viejo que la memoria de aquí (su `updated` es anterior): no la toca.
    mas_viejo: bool,
}

/// Error si la entrada deja (o crea) una memoria de oficio —global, con `trade:`— con algo de un
/// proyecto o de un cliente (`oficios::fugas`): en su texto, sus rutas, sus etiquetas, su clave o sus
/// datos. `global` es si la memoria queda global; `antes`, la que se pone al día (sus etiquetas y rutas
/// se juntan con las nuevas). Lo que esa ya tenía siendo de oficio (guardado quizá con `allow_leaks`)
/// no vuelve a pararla: solo lo nuevo.
fn comprobar_oficio(db: &Connection, e: &Entrada, global: bool, antes: Option<&str>, clave: &str) -> Result<()> {
    if !global {
        return Ok(());
    }
    let vieja = match antes {
        Some(id) => Some(memoria::obtener(db, id)?),
        None => None,
    };
    let mut tags: Vec<String> = e.nueva.tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).collect();
    let mut paths = e.nueva.paths.clone();
    if let Some(v) = &vieja {
        tags.extend(v.tags.iter().cloned());
        paths.extend(v.paths.iter().cloned());
    }
    if !crate::oficios::lleva_oficio(&tags) {
        return Ok(());
    }
    let rutas = paths.join(" ");
    let campos = crate::oficios::campos_para_fugas(&[("title", &e.nueva.title), ("content", &e.nueva.content), ("paths", &rutas)], &tags, Some(clave), e.datos.as_ref());
    let mut fugas = crate::oficios::fugas_en(db, &campos)?;
    if let Some(v) = vieja.filter(|v| v.global() && crate::oficios::lleva_oficio(&v.tags)) {
        let datos = memoria::datos_de(db, &[v.id.as_str()])?.remove(&v.id);
        let rutas = v.paths.join(" ");
        let ya = crate::oficios::campos_para_fugas(&[("title", &v.title), ("content", &v.content), ("paths", &rutas)], &v.tags, memoria::clave_de(&v), datos.as_ref());
        let ya: BTreeSet<String> = crate::oficios::fugas_en(db, &ya)?.into_iter().map(|f| f.text.to_lowercase()).collect();
        fugas.retain(|f| !ya.contains(&f.text.to_lowercase()));
    }
    if fugas.is_empty() {
        return Ok(());
    }
    let lista: Vec<String> = fugas.iter().map(|f| if f.campo.is_empty() { format!("{} «{}»", f.tipo, f.text) } else { format!("{} «{}» ({})", f.tipo, f.text, f.campo) }).collect();
    bail!(tr!(
        "it would be a trade memory (global with trade:), which reaches every project of that trade of every client, and it names what looks like a project or a client: {}. Remove it from the concept, or import it into a project instead of --global",
        "sería una memoria de oficio (global con trade:), que llega a todos los proyectos de ese oficio de todos los clientes, y nombra algo que parece de un proyecto o de un cliente: {}. Quítalo del concepto, o impórtalo en un proyecto en vez de con --global",
        lista.join(", ")
    ))
}

/// Si la memoria `id` (el id entero, en su forma) está viva aquí.
fn viva(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM memory WHERE id = ?1)", [id], |f| f.get(0))?)
}

/// Si `id` se olvidó aquí: tiene su marca de olvido o está en la papelera.
fn olvidada(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM tombstone WHERE id = ?1) OR EXISTS(SELECT 1 FROM forgotten WHERE id = ?1)", [id], |f| f.get(0))?)
}

/// Si el instante `a` es posterior a `b` (dos ISO 8601, con su zona si la dicen). `false` si alguno no
/// se entiende.
fn despues_de(db: &Connection, a: &str, b: &str) -> Result<bool> {
    let x: Option<bool> = db.query_row("SELECT julianday(?1) > julianday(?2)", params![instante_sql(a), instante_sql(b)], |f| f.get(0))?;
    Ok(x == Some(true))
}

/// Importa un concepto: qué pasó y su entrada.
fn importar_uno(db: &mut Connection, modelo: Option<&Modelo>, c: &Concepto, destino: &Destino, op: Opciones, clave: Option<&[u8]>, tocadas: &mut BTreeSet<String>) -> Result<(Uno, Entrada)> {
    let mut e = entrada(c)?;
    // ¿Salió de esta base? Su firma cubre todo lo que se aplica de él (ver `firma` y `texto_firmado`):
    // un campo cambiado, quitado o añadido lo deja sin firma.
    let propio = match (clave, &e.firma, &e.id) {
        (Some(k), Some(f), Some(_)) => firma::vale(k, &texto_firmado(&e), f),
        _ => false,
    };
    let mut u = Uno { id: None, hecho: Hecho::Igual, tapado: false, sello: Sello::Nada, ajeno: !propio, sin_global: false, sin_fijar: false, global: false, propio, mas_viejo: false };
    if !propio {
        // De otro: ni global, ni fijada; va al destino pedido como cualquier concepto.
        if e.global {
            u.sin_global = matches!(destino, Destino::Proyecto(p) if !p.is_empty());
            e.global = false;
        }
        if e.nueva.pinned {
            u.sin_fijar = true;
            e.nueva.pinned = false;
        }
        // Y nada de lo que diga su `omoikane:` decide con qué memoria de aquí se junta ni adónde va: ni
        // su id (con el de una global fijada, de otro proyecto o de oficio la reescribía donde
        // estuviera), ni su clave, ni la clave de su proyecto (lo mandaba a otro proyecto que el
        // pedido). Se guarda con un id nuevo por la clave del concepto (`okf:<ruta>`) en el destino:
        // importarlo otra vez la pone al día.
        e.id = None;
        e.su_clave = None;
        e.project_key = None;
        e.clave = clave_de_concepto(&c.id, &Map::new());
    }
    // Una verificación anterior a `generated.at` es de un texto que ya no es este (OKF la da por
    // superada): no pone el sello.
    if let Some((_, Some(at))) = &e.verificada
        && let Some(gen_at) = &e.generada
        && despues_de(db, gen_at, at)?
    {
        e.verificada = None;
    }
    // Lo olvidado aquí no vuelve con su id.
    if let Some(id) = &e.id
        && !viva(db, id)?
        && olvidada(db, id)?
    {
        u.hecho = Hecho::Olvidada;
        return Ok((u, e));
    }
    // Los datos, comprobados antes de escribir nada: si no valen, el concepto falla sin dejar una
    // memoria a medias.
    if let Some(d) = &e.datos {
        memoria::comprobar_datos(d)?;
    }
    let (proyecto, project_id, space_id) = donde(db, destino, &e)?;
    e.nueva.project = proyecto.clone();
    e.nueva.project_id = project_id;
    e.nueva.space_id = space_id;
    // La misma memoria (solo de un concepto firmado aquí: uno de otro ya no trae id), si esta base aún
    // la tiene en el mismo espacio. Donde esté ahora: si después de exportarla se movió (una global que
    // se llevó a un proyecto), se pone al día ahí, sin volver a moverla ni crear otra por su clave; lo
    // que se hizo aquí después es más nuevo que el bundle.
    let propia = match &e.id {
        Some(id) if viva(db, id)? => {
            let (_, espacio) = memoria::alcance_de_memoria(db, id)?.unwrap_or((false, String::new()));
            let su_espacio = crate::proyecto::clave_espacio(db, space_id)?;
            (espacio == su_espacio).then(|| id.clone())
        }
        _ => None,
    };
    // Global: lo que pide serlo (y se cree), todo si el destino es lo global (`--global`), o la que ya
    // es global y se pone al día donde está.
    u.global = match &propia {
        Some(id) => memoria::alcance_de_memoria(db, id)?.is_some_and(|(g, _)| g),
        None => space_id.is_none() && project_id.is_none() && proyecto.is_empty(),
    };
    // Cómo se guarda: poniendo al día la misma memoria; por su clave, si ya hay una con ella donde cae;
    // con su mismo id, si salió de Omoikane y esta base no lo tiene; y si no, por la clave del concepto.
    // Dos conceptos de la misma importación nunca van a la misma memoria (memorias de dos proyectos con
    // la misma clave que caen en uno solo): el segundo no se queda con la clave.
    let mut por_su_clave = match &e.su_clave {
        Some(k) => existente(db, &proyecto, project_id, space_id, k)?.map(|x| (k.clone(), x)),
        None => None,
    };
    if por_su_clave.as_ref().is_some_and(|(_, (id, _))| tocadas.contains(id)) {
        por_su_clave = None;
        e.su_clave = None;
        e.clave = match &e.id {
            Some(i) => format!("okf:{}", i.to_lowercase()),
            None => clave_de_concepto(&c.id, &Map::new()),
        };
    }
    if e.su_clave.is_none() && existente(db, &proyecto, project_id, space_id, &e.clave)?.is_some_and(|(id, _)| tocadas.contains(&id)) {
        use sha2::{Digest, Sha256};
        let h = Sha256::digest(c.id.as_bytes());
        let base: String = e.clave.chars().take(71).collect();
        e.clave = format!("{base}-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3]);
    }
    let con_su_id = match e.id.clone() {
        Some(id) if propia.is_none() && por_su_clave.is_none() && !viva(db, &id)? => Some(id),
        _ => None,
    };
    let antes = match (&propia, &por_su_clave) {
        (Some(id), _) => Some((id.clone(), sello(db, id)?)),
        (None, Some((_, x))) => Some(x.clone()),
        (None, None) if con_su_id.is_some() => None,
        (None, None) => existente(db, &proyecto, project_id, space_id, &e.clave)?,
    };
    // Vuelta atrás: un concepto de esta base más viejo que la memoria de aquí (su `updated`, firmado, es
    // anterior al de ella: un bundle exportado antes de cambiarla) no la toca. Ni su texto, ni sus
    // etiquetas, rutas o datos, ni sus veredictos, ni el sello: devolvería la memoria a lo de entonces y
    // la verificaría con la fecha de entonces.
    if propio && let Some((id, _)) = &antes {
        let aqui: String = db.query_row("SELECT updated_at FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
        let mas_viejo = match &e.cambiada {
            Some(c) => despues_de(db, &aqui, c)?,
            None => true,
        };
        if mas_viejo {
            u.mas_viejo = true;
            u.hecho = Hecho::Igual;
            u.id = Some(id.clone());
            tocadas.insert(id.clone());
            return Ok((u, e));
        }
    }
    // Si el `verified` de una persona puede poner el sello.
    u.sello = match &e.verificada {
        None => Sello::Nada,
        Some(_) if !op.verificar => Sello::Agente,
        Some(_) if !propio && !op.confiar_verificadas => Sello::Ajeno,
        Some((_, at)) => {
            let kind = e.nueva.kind_text.clone().unwrap_or_else(|| e.nueva.kind.unwrap_or(Tipo::Fact).as_str().to_string());
            let otro_texto = e.verified_sha.as_deref().is_some_and(|s| s != crate::verificado::huella(&kind, &e.nueva.title, &e.nueva.content));
            // La persona la desverificó aquí después de esa verificación (reimportar un bundle viejo).
            let desverificada = match (&antes, at) {
                (Some((id, _)), Some(at)) => match crate::verificado::desverificada_en(db, id)? {
                    Some(d) => despues_de(db, &d, at)?,
                    None => false,
                },
                _ => false,
            };
            let futura = match at {
                Some(at) => db.query_row("SELECT julianday(?1) > julianday('now', '+1 day')", [instante_sql(at)], |f| f.get::<_, Option<bool>>(0))? == Some(true),
                None => false,
            };
            if otro_texto || desverificada || futura { Sello::Viejo } else { Sello::Puesto }
        }
    };
    // Una memoria de oficio que se crea o cambia al importar pasa por `oficios::fugas`, como en
    // `remember` y `update` (sin `allow_leaks`: lo que marque, se arregla en el bundle o se importa a un
    // proyecto). Antes de escribir nada, también en seco.
    comprobar_oficio(db, &e, u.global, antes.as_ref().map(|(id, _)| id.as_str()), por_su_clave.as_ref().map_or(e.clave.as_str(), |(k, _)| k.as_str()))?;
    if op.en_seco {
        u.hecho = if antes.is_some() { Hecho::Cambiada } else { Hecho::Creada };
        u.id = antes.map(|(id, _)| id);
        return Ok((u, e));
    }
    // Lo que parezca un secreto lo tapa `memoria` al guardar (también en los datos); aquí solo se
    // apunta para decirlo.
    let secreto = crate::secretos::activo() && !crate::secretos::hallazgos(&format!("{}\n{}", e.nueva.title, e.nueva.content)).is_empty();
    let (id, creada, tapados) = match &propia {
        Some(id) => {
            let m = memoria::obtener(db, id)?;
            let mut tags = m.tags.clone();
            tags.extend(e.nueva.tags.iter().cloned());
            let mut paths = m.paths.clone();
            paths.extend(e.nueva.paths.iter().cloned());
            let cambios = Cambios {
                title: Some(e.nueva.title.clone()),
                content: Some(e.nueva.content.clone()),
                kind: e.nueva.kind,
                paths: Some(paths),
                tags: Some(tags),
                pinned: e.nueva.pinned.then_some(true),
                reason: Some("okf import".into()),
                ..Default::default()
            };
            let m = memoria::actualizar(db, id, cambios, modelo)?;
            let mut tapados = m.redacted.clone();
            if e.datos.is_some() {
                tapados.extend(memoria::poner_datos(db, id, e.datos.clone())?);
            }
            (m.id, false, tapados)
        }
        None if con_su_id.is_some() => {
            let mut nueva = e.nueva.clone();
            if let Some(k) = &e.su_clave {
                nueva.tags.push(format!("{}{k}", memoria::ETIQUETA_TEMA));
            }
            let m = memoria::guardar_con_su_id(db, con_su_id.as_deref().unwrap_or_default(), nueva, modelo)?;
            let mut tapados = m.redacted.clone();
            if e.datos.is_some() {
                tapados.extend(memoria::poner_datos(db, &m.id, e.datos.clone())?);
            }
            (m.id, true, tapados)
        }
        None => {
            let clave = por_su_clave.as_ref().map_or(e.clave.as_str(), |(k, _)| k.as_str());
            let p = memoria::guardar_por_clave(db, clave, e.nueva.clone(), e.datos.clone(), modelo)?;
            let mut tapados = p.memoria.redacted.clone();
            tapados.extend(p.tapados);
            (p.memoria.id, p.creada, tapados)
        }
    };
    if creada {
        // Las fechas de origen, si se entienden.
        for (columna, valor) in [("created_at", &e.creada), ("updated_at", &e.cambiada)] {
            if let Some(v) = valor.as_deref().filter(|v| !v.is_empty()) {
                db.execute(
                    &format!("UPDATE memory SET {columna} = strftime('%Y-%m-%dT%H:%M:%fZ', ?1) WHERE id = ?2 AND strftime('%Y-%m-%dT%H:%M:%fZ', ?1) IS NOT NULL"),
                    params![instante_sql(v), id],
                )?;
            }
        }
    }
    // El sello vuelve solo si el texto que se verificó (`verified_sha`) es el que la memoria tiene AHORA,
    // ya escrita (al guardarla se pudo tapar un secreto, o la memoria a la que fue tenía otro texto).
    if u.sello == Sello::Puesto
        && let Some(sha) = &e.verified_sha
    {
        let m = memoria::obtener(db, &id)?;
        if crate::verificado::huella(m.tipo_texto(), &m.title, &m.content) != *sha {
            u.sello = Sello::Viejo;
        }
    }
    // El sello de verificada, después del texto (cambiarlo quita el que hubiera) y con la fecha original.
    // Si ya tiene ese mismo, no se toca.
    if u.sello == Sello::Puesto
        && let Some((by, at)) = &e.verificada
    {
        u.sello = match crate::verificado::verificar_en(db, &id, by, at.as_deref()) {
            Ok((_, true)) => Sello::Puesto,
            Ok((_, false)) => Sello::Ya,
            Err(err) => {
                eprintln!("omoikane: {}: {err:#}", c.id);
                Sello::Viejo
            }
        };
    }
    u.hecho = match antes {
        None => Hecho::Creada,
        Some((_, h)) if sello(db, &id)? != h => Hecho::Cambiada,
        Some(_) => Hecho::Igual,
    };
    tocadas.insert(id.clone());
    u.id = Some(id);
    u.tapado = secreto || !tapados.is_empty();
    Ok((u, e))
}

// --- YAML ------------------------------------------------------------------------------------------

/// Lo de YAML que usan las cabeceras de OKF: mapas y listas por sangría, `[…]` y `{…}` (también en
/// varias líneas), textos entre comillas simples o dobles, bloques `|` y `>`, comentarios, números,
/// `true`/`false` y `null`. Sin anclas, alias ni etiquetas `!!`: un error claro si aparecen. Sin
/// dependencias: lo que se lee se queda en `serde_json::Value`.
pub mod yaml {
    use serde_json::{Map, Number, Value};

    struct Linea {
        sangria: usize,
        texto: String,
        n: usize,
    }

    struct Lector {
        lineas: Vec<Linea>,
        crudas: Vec<String>,
        i: usize,
        /// Cuántos mapas y listas hay abiertos (ver [`PROFUNDIDAD`]).
        prof: usize,
    }

    /// Cuántos mapas y listas anidados se leen como mucho: una cabecera de verdad no pasa de tres o
    /// cuatro, y sin tope una de `x: [[[[…` (o de mapas cada vez más sangrados) desborda la pila y
    /// tumba el proceso.
    pub const PROFUNDIDAD: usize = 64;

    fn honda() -> String {
        format!("nested deeper than {PROFUNDIDAD} levels")
    }

    /// Lee una cabecera (lo de entre los `---`) como un mapa.
    pub fn leer(texto: &str) -> Result<Map<String, Value>, String> {
        let crudas: Vec<String> = texto.lines().map(|l| l.trim_end_matches('\r').to_string()).collect();
        let mut lineas = Vec::new();
        for (n, l) in crudas.iter().enumerate() {
            let sin = l.trim_start_matches(' ');
            if sin.starts_with('\t') {
                return Err(format!("line {}: tabs can't indent YAML", n + 1));
            }
            if sin.is_empty() || sin.starts_with('#') {
                continue;
            }
            if n == 0 && sin.starts_with('%') {
                return Err(format!("line {}: YAML directives are not supported", n + 1));
            }
            lineas.push(Linea { sangria: l.len() - sin.len(), texto: sin.to_string(), n });
        }
        let mut l = Lector { lineas, crudas, i: 0, prof: 0 };
        if l.lineas.is_empty() {
            return Ok(Map::new());
        }
        let s = l.lineas[0].sangria;
        if es_item(&l.lineas[0].texto) {
            return Err("the header is a list, not keys and values".into());
        }
        let m = l.mapa(s)?;
        if let Some(x) = l.lineas.get(l.i) {
            return Err(format!("line {}: unexpected indentation", x.n + 1));
        }
        Ok(m)
    }

    fn es_item(t: &str) -> bool {
        t == "-" || t.starts_with("- ")
    }

    impl Lector {
        fn mapa(&mut self, sangria: usize) -> Result<Map<String, Value>, String> {
            self.prof += 1;
            let r = self.mapa_dentro(sangria);
            self.prof -= 1;
            r
        }

        fn mapa_dentro(&mut self, sangria: usize) -> Result<Map<String, Value>, String> {
            if self.prof > PROFUNDIDAD {
                return Err(honda());
            }
            let mut m = Map::new();
            while let Some(l) = self.lineas.get(self.i) {
                if l.sangria < sangria || (l.sangria == sangria && es_item(&l.texto)) {
                    break;
                }
                if l.sangria > sangria {
                    return Err(format!("line {}: unexpected indentation", l.n + 1));
                }
                let n = l.n;
                let texto = l.texto.clone();
                let (clave, resto) = partir_clave(&texto).ok_or_else(|| format!("line {}: expected `key: value`", n + 1))?;
                self.i += 1;
                let v = self.valor(sangria, resto.trim(), n)?;
                m.insert(clave, v);
            }
            Ok(m)
        }

        fn lista(&mut self, sangria: usize) -> Result<Vec<Value>, String> {
            self.prof += 1;
            let r = self.lista_dentro(sangria);
            self.prof -= 1;
            r
        }

        fn lista_dentro(&mut self, sangria: usize) -> Result<Vec<Value>, String> {
            if self.prof > PROFUNDIDAD {
                return Err(honda());
            }
            let mut v = Vec::new();
            while let Some(l) = self.lineas.get(self.i) {
                if l.sangria != sangria || !es_item(&l.texto) {
                    if l.sangria > sangria {
                        return Err(format!("line {}: unexpected indentation", l.n + 1));
                    }
                    break;
                }
                let n = l.n;
                let resto = l.texto[1..].to_string();
                let dentro = resto.len() - resto.trim_start().len();
                let item = resto.trim().to_string();
                if !item.is_empty() && (es_item(&item) || (partir_clave(&item).is_some() && !item.starts_with(['[', '{', '"', '\'']))) {
                    // `- clave: valor` (o `- - x`): lo de después del guion es el principio de un mapa (o
                    // una lista) con la sangría de donde empieza.
                    let s = sangria + 1 + dentro;
                    self.lineas[self.i] = Linea { sangria: s, texto: item.clone(), n };
                    v.push(if es_item(&item) { Value::Array(self.lista(s)?) } else { Value::Object(self.mapa(s)?) });
                } else {
                    self.i += 1;
                    v.push(self.valor(sangria, &item, n)?);
                }
            }
            Ok(v)
        }

        /// El valor de una clave (o de un elemento) en la línea `n` con sangría `sangria`: lo que haya
        /// detrás, o lo que venga debajo.
        fn valor(&mut self, sangria: usize, resto: &str, n: usize) -> Result<Value, String> {
            let resto = sin_comentario(resto);
            if resto.is_empty() {
                return Ok(match self.lineas.get(self.i) {
                    Some(l) if l.sangria > sangria => {
                        let s = l.sangria;
                        if es_item(&l.texto) { Value::Array(self.lista(s)?) } else { Value::Object(self.mapa(s)?) }
                    }
                    Some(l) if l.sangria == sangria && es_item(&l.texto) => Value::Array(self.lista(sangria)?),
                    _ => Value::Null,
                });
            }
            if resto.starts_with('|') || resto.starts_with('>') {
                return Ok(Value::String(self.bloque(sangria, resto, n)?));
            }
            if resto.starts_with('[') || resto.starts_with('{') {
                // Un `[…]` o `{…}` puede seguir en las líneas de debajo.
                // Lo abierto se lleva línea a línea (`Cierre`): volver a mirar todo lo juntado en cada
                // línea era cuadrático (una cabecera de 400 KB en una sola lista tardaba 15 s).
                let mut todo = resto.to_string();
                let mut cierre = Cierre::default();
                cierre.mirar(&todo);
                while !cierre.cerrado() {
                    let Some(l) = self.lineas.get(self.i) else { return Err(format!("line {}: unclosed {}", n + 1, &resto[..1])) };
                    let mas = sin_comentario(&l.texto);
                    todo.push(' ');
                    todo.push_str(mas);
                    cierre.mirar(" ");
                    cierre.mirar(mas);
                    self.i += 1;
                }
                let mut p = Flujo { c: todo.chars().collect(), i: 0, prof: 0 };
                let v = p.valor().map_err(|e| format!("line {}: {e}", n + 1))?;
                p.espacios();
                if p.i < p.c.len() {
                    return Err(format!("line {}: unexpected text after the closing bracket", n + 1));
                }
                return Ok(v);
            }
            if resto.starts_with('"') || resto.starts_with('\'') {
                let mut p = Flujo { c: resto.chars().collect(), i: 0, prof: 0 };
                let v = p.comillas().map_err(|e| format!("line {}: {e}", n + 1))?;
                p.espacios();
                if p.i < p.c.len() {
                    return Err(format!("line {}: unexpected text after the closing quote", n + 1));
                }
                return Ok(Value::String(v));
            }
            if resto.starts_with(['&', '*', '!']) {
                return Err(format!("line {}: YAML anchors, aliases and tags are not supported", n + 1));
            }
            // Un texto sin comillas puede seguir en las líneas de debajo, más sangradas.
            let mut texto = resto.to_string();
            while let Some(l) = self.lineas.get(self.i) {
                if l.sangria <= sangria {
                    break;
                }
                if es_item(&l.texto) || partir_clave(&l.texto).is_some() {
                    return Err(format!("line {}: unexpected indentation", l.n + 1));
                }
                texto.push(' ');
                texto.push_str(sin_comentario(&l.texto));
                self.i += 1;
            }
            Ok(plano(&texto))
        }

        /// Un bloque `|` (las líneas tal cual) o `>` (unidas con espacios), con `-` o `+` al final.
        fn bloque(&mut self, sangria: usize, cabeza: &str, n: usize) -> Result<String, String> {
            let literal = cabeza.starts_with('|');
            let cortar = cabeza.contains('-');
            let guardar = cabeza.contains('+');
            // Las líneas crudas hasta la siguiente con sangría <= la de la clave.
            let fin = self.lineas.get(self.i).filter(|l| l.sangria <= sangria).map(|l| l.n).or_else(|| {
                let mut j = self.i;
                while self.lineas.get(j).is_some_and(|l| l.sangria > sangria) {
                    j += 1;
                }
                self.lineas.get(j).map(|l| l.n)
            });
            let fin = fin.unwrap_or(self.crudas.len());
            while self.lineas.get(self.i).is_some_and(|l| l.n < fin) {
                self.i += 1;
            }
            let crudas: Vec<&str> = self.crudas[n + 1..fin].iter().map(String::as_str).collect();
            let dentro = crudas.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).min().unwrap_or(sangria + 2);
            // Se quitan solo espacios ASCII (cada uno, un byte): cortar en el byte `dentro` de una línea
            // que empieza por otro espacio (`\u{a0}`, de varios bytes) partiría un carácter.
            let partes: Vec<String> = crudas
                .iter()
                .map(|l| {
                    let espacios = l.len() - l.trim_start_matches(' ').len();
                    match l.get(espacios.min(dentro)..) {
                        Some(r) if !l.trim().is_empty() || espacios == l.len() => r.to_string(),
                        _ => String::new(),
                    }
                })
                .collect();
            let mut s = if literal {
                partes.join("\n")
            } else {
                let mut r = String::new();
                for p in &partes {
                    if p.is_empty() {
                        r.push('\n');
                    } else {
                        if !r.is_empty() && !r.ends_with('\n') {
                            r.push(' ');
                        }
                        r.push_str(p);
                    }
                }
                r
            };
            if !guardar {
                s = s.trim_end_matches('\n').to_string();
                if !cortar {
                    s.push('\n');
                }
            }
            Ok(s)
        }
    }

    /// `clave: resto` (o `"clave": resto`), si la línea empieza así.
    fn partir_clave(t: &str) -> Option<(String, &str)> {
        if t.starts_with('"') || t.starts_with('\'') {
            let mut p = Flujo { c: t.chars().collect(), i: 0, prof: 0 };
            let k = p.comillas().ok()?;
            let usado: usize = p.c[..p.i].iter().map(|c| c.len_utf8()).sum();
            let resto = t[usado..].trim_start().strip_prefix(':')?;
            return (resto.is_empty() || resto.starts_with(' ')).then_some((k, resto));
        }
        if t.starts_with(['[', '{', '-', '#', '?']) && !t.starts_with("-") || t.starts_with("- ") {
            return None;
        }
        let i = t.find(": ").or_else(|| t.strip_suffix(':').map(|s| s.len()))?;
        let k = t[..i].trim();
        if k.is_empty() || k.contains(" #") {
            return None;
        }
        Some((k.to_string(), &t[i + 1..]))
    }

    /// Quita un comentario (` #…`) de fuera de las comillas.
    fn sin_comentario(t: &str) -> &str {
        let mut comilla: Option<char> = None;
        let mut previo = ' ';
        let mut escapado = false;
        for (i, c) in t.char_indices() {
            match comilla {
                Some('"') if escapado => escapado = false,
                Some('"') if c == '\\' => escapado = true,
                Some(q) if c == q => comilla = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => {
                    if previo == ' ' || previo == '[' || previo == '{' || previo == ',' || previo == ':' || i == 0 {
                        comilla = Some(c)
                    }
                }
                None if c == '#' && (i == 0 || previo == ' ') => return t[..i].trim_end(),
                None => {}
            }
            previo = c;
        }
        t.trim_end()
    }

    /// Cuántos `[` y `{` de fuera de las comillas siguen abiertos, mirando el texto a trozos (cada
    /// trozo, una sola vez).
    #[derive(Default)]
    struct Cierre {
        nivel: i64,
        comilla: Option<char>,
        escapado: bool,
    }

    impl Cierre {
        fn mirar(&mut self, t: &str) {
            for c in t.chars() {
                match self.comilla {
                    Some('"') if self.escapado => self.escapado = false,
                    Some('"') if c == '\\' => self.escapado = true,
                    Some(q) if c == q => self.comilla = None,
                    Some(_) => {}
                    None => match c {
                        '"' | '\'' => self.comilla = Some(c),
                        '[' | '{' => self.nivel += 1,
                        ']' | '}' => self.nivel -= 1,
                        _ => {}
                    },
                }
            }
        }

        /// Si los `[` y `{` de fuera de las comillas están cerrados.
        fn cerrado(&self) -> bool {
            self.nivel <= 0 && self.comilla.is_none()
        }
    }

    /// Un valor sin comillas: `null`, `true`/`false`, un número o un texto.
    fn plano(t: &str) -> Value {
        let t = t.trim();
        match t {
            "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
            "true" | "True" | "TRUE" => return Value::Bool(true),
            "false" | "False" | "FALSE" => return Value::Bool(false),
            _ => {}
        }
        let digitos = t.strip_prefix(['-', '+']).unwrap_or(t);
        if !digitos.is_empty()
            && digitos.chars().all(|c| c.is_ascii_digit())
            && !(digitos.len() > 1 && digitos.starts_with('0'))
            && let Ok(n) = t.parse::<i64>()
        {
            return Value::Number(n.into());
        }
        let es_decimal = {
            let (m, e) = match digitos.find(['e', 'E']) {
                Some(i) => (&digitos[..i], Some(&digitos[i + 1..])),
                None => (digitos, None),
            };
            let partes: Vec<&str> = m.split('.').collect();
            partes.len() == 2
                && partes.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
                && partes.iter().any(|p| !p.is_empty())
                && e.is_none_or(|e| {
                    let e = e.strip_prefix(['-', '+']).unwrap_or(e);
                    !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
                })
        };
        if es_decimal && let Some(n) = t.parse::<f64>().ok().and_then(Number::from_f64) {
            return Value::Number(n);
        }
        Value::String(t.to_string())
    }

    struct Flujo {
        c: Vec<char>,
        i: usize,
        /// Cuántos `[` y `{` hay abiertos (ver [`PROFUNDIDAD`]).
        prof: usize,
    }

    impl Flujo {
        fn espacios(&mut self) {
            while self.c.get(self.i).is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
        }

        fn valor(&mut self) -> Result<Value, String> {
            self.prof += 1;
            let r = if self.prof > PROFUNDIDAD { Err(honda()) } else { self.valor_dentro() };
            self.prof -= 1;
            r
        }

        fn valor_dentro(&mut self) -> Result<Value, String> {
            self.espacios();
            match self.c.get(self.i) {
                Some('[') => {
                    self.i += 1;
                    let mut v = Vec::new();
                    loop {
                        self.espacios();
                        match self.c.get(self.i) {
                            Some(']') => {
                                self.i += 1;
                                return Ok(Value::Array(v));
                            }
                            None => return Err("unclosed [".into()),
                            _ => {}
                        }
                        v.push(self.valor()?);
                        self.espacios();
                        match self.c.get(self.i) {
                            Some(',') => self.i += 1,
                            Some(']') => {}
                            _ => return Err("expected , or ] in a list".into()),
                        }
                    }
                }
                Some('{') => {
                    self.i += 1;
                    let mut m = Map::new();
                    loop {
                        self.espacios();
                        match self.c.get(self.i) {
                            Some('}') => {
                                self.i += 1;
                                return Ok(Value::Object(m));
                            }
                            None => return Err("unclosed {".into()),
                            _ => {}
                        }
                        let k = match self.c.get(self.i) {
                            Some('"' | '\'') => self.comillas()?,
                            _ => self.plano_hasta(true),
                        };
                        self.espacios();
                        if self.c.get(self.i) != Some(&':') {
                            return Err(format!("expected : after «{k}»"));
                        }
                        self.i += 1;
                        let v = self.valor()?;
                        m.insert(k.trim().to_string(), v);
                        self.espacios();
                        match self.c.get(self.i) {
                            Some(',') => self.i += 1,
                            Some('}') => {}
                            _ => return Err("expected , or } in a map".into()),
                        }
                    }
                }
                Some('"' | '\'') => Ok(Value::String(self.comillas()?)),
                Some('&' | '*' | '!') => Err("YAML anchors, aliases and tags are not supported".into()),
                _ => {
                    let t = self.plano_hasta(false);
                    Ok(plano(&t))
                }
            }
        }

        /// Un texto sin comillas dentro de `[…]` o `{…}`: hasta `,`, `]` o `}` (una clave, también
        /// hasta `: `).
        fn plano_hasta(&mut self, clave: bool) -> String {
            let mut s = String::new();
            while let Some(&c) = self.c.get(self.i) {
                if matches!(c, ',' | ']' | '}') {
                    break;
                }
                if clave && c == ':' && self.c.get(self.i + 1).is_none_or(|x| x.is_whitespace() || matches!(x, ',' | '}' | ']')) {
                    break;
                }
                s.push(c);
                self.i += 1;
            }
            s.trim().to_string()
        }

        fn comillas(&mut self) -> Result<String, String> {
            let q = self.c[self.i];
            self.i += 1;
            let mut s = String::new();
            loop {
                let Some(&c) = self.c.get(self.i) else { return Err("unclosed quote".into()) };
                self.i += 1;
                if q == '\'' {
                    if c == '\'' {
                        if self.c.get(self.i) == Some(&'\'') {
                            s.push('\'');
                            self.i += 1;
                            continue;
                        }
                        return Ok(s);
                    }
                    s.push(c);
                    continue;
                }
                match c {
                    '"' => return Ok(s),
                    '\\' => {
                        let Some(&e) = self.c.get(self.i) else { return Err("unclosed quote".into()) };
                        self.i += 1;
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            '0' => s.push('\0'),
                            '"' | '\\' | '/' | ' ' => s.push(e),
                            'u' | 'x' | 'U' => {
                                let n = match e {
                                    'x' => 2,
                                    'u' => 4,
                                    _ => 8,
                                };
                                let h: String = self.c.iter().skip(self.i).take(n).collect();
                                self.i += n;
                                let cp = u32::from_str_radix(&h, 16).map_err(|_| format!("bad escape \\{e}{h}"))?;
                                // Un par sustituto de JSON (`😀`).
                                if (0xD800..0xDC00).contains(&cp) && self.c.get(self.i) == Some(&'\\') && self.c.get(self.i + 1) == Some(&'u') {
                                    let h2: String = self.c.iter().skip(self.i + 2).take(4).collect();
                                    if let Ok(bajo) = u32::from_str_radix(&h2, 16)
                                        && (0xDC00..0xE000).contains(&bajo)
                                    {
                                        self.i += 6;
                                        s.push(char::from_u32(0x10000 + ((cp - 0xD800) << 10) + (bajo - 0xDC00)).unwrap_or('\u{fffd}'));
                                        continue;
                                    }
                                }
                                s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                            otro => return Err(format!("unknown escape \\{otro}")),
                        }
                    }
                    _ => s.push(c),
                }
            }
        }
    }

    // --- Escribir ---

    /// Un texto como valor de YAML: tal cual si no hay duda de que se lee igual, si no entre comillas
    /// dobles (con los escapes de JSON, que YAML entiende).
    pub fn escalar(v: &Value) -> String {
        match v {
            Value::String(s) => {
                let seguro = !s.is_empty()
                    && s.trim() == s
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || " _./:+@-".contains(c))
                    && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                    && !s.contains(": ")
                    && !s.ends_with(':')
                    && plano(s) == Value::String(s.clone());
                if seguro { s.clone() } else { serde_json::to_string(s).unwrap_or_default() }
            }
            Value::Array(_) | Value::Object(_) => flujo(v),
            otro => otro.to_string(),
        }
    }

    /// Un valor en una línea: `[a, b]`, `{ by: x, at: y }`.
    pub fn flujo(v: &Value) -> String {
        match v {
            Value::Array(a) => format!("[{}]", a.iter().map(flujo).collect::<Vec<_>>().join(", ")),
            Value::Object(o) if o.is_empty() => "{}".into(),
            Value::Object(o) => format!("{{ {} }}", o.iter().map(|(k, v)| format!("{}: {}", clave(k), flujo(v))).collect::<Vec<_>>().join(", ")),
            otro => escalar(otro),
        }
    }

    fn clave(k: &str) -> String {
        if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !k.starts_with('-') {
            k.to_string()
        } else {
            serde_json::to_string(k).unwrap_or_default()
        }
    }

    /// `clave: valor` con la sangría que toca: los mapas, debajo; las listas de mapas, con `- `; lo
    /// demás en la misma línea.
    pub fn bloque(s: &mut String, k: &str, v: &Value, sangria: usize) {
        let pre = " ".repeat(sangria);
        match v {
            Value::Object(o) if !o.is_empty() => {
                *s += &format!("{pre}{}:\n", clave(k));
                for (k2, v2) in o {
                    bloque(s, k2, v2, sangria + 2);
                }
            }
            Value::Array(a) if a.iter().any(|x| x.is_object() || x.is_array()) => {
                *s += &format!("{pre}{}:\n", clave(k));
                for x in a {
                    *s += &format!("{pre}  - {}\n", flujo(x));
                }
            }
            otro => *s += &format!("{pre}{}: {}\n", clave(k), escalar(otro)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_de_okf() {
        let t = "type: Attested Computation\ntitle: \"Incident: freshness\"\ndescription: Steps to triage. # comentario\ntags: [sales, \"a b\", 2026]\n\
                 generated: { by: reference_agent/gemini-2.5-pro, at: 2026-05-28T14:30:00Z }\nverified:\n  - { by: human:ahormati, at: 2026-06-25T09:00:00Z }\n  - by: process:x\n    at: 2026-06-26T02:00:00Z\n\
                 parameters:\n- { name: year, type: integer, required: true }\nexecutor:\n  resource: references/skills/run-on-bq.md\n  receipt: [job_id,\n    executed_sql]\n\
                 nota: |\n  línea 1\n\n  línea 2\nplegado: >-\n  uno\n  dos\nvacio:\nversion: \"0.2\"\nnumero: 0.2\nsi: true\ncita: 'it''s'\nlargo: una frase\n  que sigue\n";
        let m = yaml::leer(t).unwrap();
        assert_eq!(m["type"], "Attested Computation");
        assert_eq!(m["title"], "Incident: freshness");
        assert_eq!(m["description"], "Steps to triage.");
        assert_eq!(m["tags"], json!(["sales", "a b", 2026]));
        assert_eq!(m["generated"]["by"], "reference_agent/gemini-2.5-pro");
        assert_eq!(m["generated"]["at"], "2026-05-28T14:30:00Z");
        assert_eq!(m["verified"][0]["by"], "human:ahormati");
        assert_eq!(m["verified"][1]["at"], "2026-06-26T02:00:00Z");
        assert_eq!(m["parameters"][0]["required"], true);
        assert_eq!(m["executor"]["receipt"], json!(["job_id", "executed_sql"]));
        assert_eq!(m["nota"], "línea 1\n\nlínea 2\n");
        assert_eq!(m["plegado"], "uno dos");
        assert_eq!(m["vacio"], Value::Null);
        assert_eq!(m["version"], "0.2");
        assert_eq!(m["si"], true);
        assert_eq!(m["cita"], "it's");
        assert_eq!(m["largo"], "una frase que sigue");
        assert!(yaml::leer("a: &x 1").is_err());
        assert!(yaml::leer("solo texto").is_err());
        assert!(yaml::leer("a: [1, 2").is_err());
        assert!(yaml::leer("a: 1\n    b: 2").is_err());
    }

    #[test]
    fn yaml_de_ida_y_vuelta() {
        let v = json!({
            "type": "Decision",
            "title": "Usar: \"comillas\" y #almohadilla",
            "tags": ["a", "true", "1.5", "x y"],
            "n": 3,
            "verified": [{ "by": "human:marc", "at": "2026-11-08T10:00:00.123Z" }],
            "anidado": { "lista": [1, 2], "texto": "línea\nsegunda", "vacio": {} },
            "raro": "- empieza con guion",
            "nulo": null,
        });
        let mut s = String::new();
        for (k, x) in v.as_object().unwrap() {
            yaml::bloque(&mut s, k, x, 0);
        }
        let m = yaml::leer(&s).unwrap();
        assert_eq!(Value::Object(m), v, "{s}");
    }

    #[test]
    fn tipos_y_claves() {
        assert_eq!(tipo_de_okf("Decision"), Tipo::Decision);
        assert_eq!(tipo_de_okf("Architecture Decision Record"), Tipo::Decision);
        assert_eq!(tipo_de_okf("Runbook"), Tipo::Procedure);
        assert_eq!(tipo_de_okf("Convention"), Tipo::Preference);
        assert_eq!(tipo_de_okf("BigQuery Table"), Tipo::Fact);
        for t in Tipo::TODOS {
            assert_eq!(tipo_de_okf(&tipo_okf(t.as_str())), t);
        }
        assert_eq!(clave_de_concepto("Architecture/Governance Model", &Map::new()), "okf:architecture/governance-model");
        let largo = "a/".repeat(60);
        let k = clave_de_concepto(&largo, &Map::new());
        assert!(k.chars().count() <= 80 && memoria::clave_tema(&k).is_ok(), "{k}");
        assert_eq!(dia_de("2026-09-23T00:00:00Z").as_deref(), Some("2026-09-23"));
        assert_eq!(dia_de("mañana"), None);
        assert_eq!(descripcion("# Título\n\nPrimera frase. Segunda.", "t"), "Título");
        assert_eq!(descripcion("Primera frase. Segunda.", "t"), "Primera frase.");
        assert_eq!(ruta_de_clave("okf:architecture/governance-model").as_deref(), Some("architecture/governance-model"));
        assert_eq!(ruta_de_clave("okf:01jabcdefghjkmnpqrstvwxyz0"), None);
        assert_eq!(actor("mcp:claude-code"), "claude-code/omoikane");
        assert_eq!(actor("cli"), "cli/omoikane");
        assert_eq!(id_de_actor("Marc Mayol 100%"), "Marc%20Mayol%20100%25");
        for n in ["Marc Mayol 100%", "a%20b", "%", "ana"] {
            assert_eq!(nombre_de_actor(&id_de_actor(n)), n);
        }
        assert_eq!(persona_de("human:Marc%20Mayol").as_deref(), Some("Marc Mayol"));
        assert_eq!(persona_de("Human/ana").as_deref(), Some("ana"));
        assert_eq!(persona_de("human"), None);
        assert_eq!(persona_de("human:"), None);
        assert_eq!(persona_de("process:nightly"), None);
        assert_eq!(persona_de("humano:x"), None);
    }

    #[test]
    fn separar_cabecera() {
        assert_eq!(separar("---\na: 1\n---\ncuerpo\n"), Some(("a: 1\n", "cuerpo\n")));
        assert_eq!(separar("\u{feff}---\r\na: 1\r\n---\r\nx"), Some(("a: 1\r\n", "x")));
        assert_eq!(separar("sin cabecera"), None);
    }

    /// Una cabecera maliciosa no tumba el proceso: ni anidando sin fin (antes, desbordaba la pila y
    /// abortaba) ni con espacios que no son ASCII en un bloque (antes, cortaba un carácter por la mitad).
    #[test]
    fn yaml_malicioso_da_error_y_no_revienta() {
        // 200 000 `[` en una línea.
        let hondo = format!("x: {}{}", "[".repeat(200_000), "]".repeat(200_000));
        assert!(yaml::leer(&hondo).unwrap_err().contains("nested deeper"));
        let hondo = format!("x: {}1{}", "{a: ".repeat(100_000), "}".repeat(100_000));
        assert!(yaml::leer(&hondo).unwrap_err().contains("nested deeper"));
        // Listas anidadas por sangría (`- - - … x`) y mapas cada vez más sangrados.
        let listas = format!("x:\n  {}x\n", "- ".repeat(100_000));
        assert!(yaml::leer(&listas).unwrap_err().contains("nested deeper"));
        let mut mapas = String::new();
        for i in 0..2_000 {
            mapas += &format!("{}k{i}:\n", " ".repeat(i));
        }
        assert!(yaml::leer(&mapas).unwrap_err().contains("nested deeper"));
        // Lo de siempre sigue: 60 niveles valen.
        assert!(yaml::leer(&format!("x: {}1{}", "[".repeat(60), "]".repeat(60))).is_ok());
        // Un bloque con una línea de espacios no ASCII (`\u{a0}`) más corta que su sangría.
        let m = yaml::leer("nota: |\n   uno\n  \u{a0}\n   dos\n").unwrap();
        assert_eq!(m["nota"], "uno\n\ndos\n");
        let m = yaml::leer("nota: |\n  \u{a0}uno\n  dos\n").unwrap();
        assert_eq!(m["nota"], "\u{a0}uno\ndos\n");
    }

    /// Un `[…]` partido en muchas líneas se lee en tiempo lineal: antes se volvía a mirar todo lo
    /// juntado en cada línea (una cabecera de 400 KB en una sola lista, 15 s).
    #[test]
    fn yaml_en_muchas_lineas_no_es_cuadratico() {
        let mut t = String::from("tags: [\n");
        for i in 0..80_000 {
            t += &format!("  \"a{i}\",\n");
        }
        t += "  fin ]\n";
        assert!(t.len() > 900_000);
        let inicio = std::time::Instant::now();
        let m = yaml::leer(&t).unwrap();
        assert_eq!(m["tags"].as_array().unwrap().len(), 80_001);
        assert!(inicio.elapsed() < std::time::Duration::from_secs(3), "{:?}", inicio.elapsed());
        // Lo que se parte a mitad de unas comillas sigue abierto en la línea siguiente.
        let m = yaml::leer("x: [\"a,\n  b]\", c]\n").unwrap();
        assert_eq!(m["x"], serde_json::json!(["a, b]", "c"]));
    }

    /// El `index.md` tiene los límites de un concepto (ni enlace simbólico ni más de 1 MB), y un bundle
    /// enorme (de ficheros o de bytes) se rechaza entero con un error claro.
    #[test]
    fn indice_y_topes_del_bundle() {
        let t = crate::comun::Temporal::nueva("okf-topes");
        let raiz = t.join("b");
        std::fs::create_dir_all(raiz.join("sub")).unwrap();
        std::fs::write(raiz.join("index.md"), format!("---\nokf_version: \"0.2\"\nx: [{}]\n---\n", "1, ".repeat(400_000))).unwrap();
        for i in 0..5 {
            std::fs::write(raiz.join("sub").join(format!("c{i}.md")), "---\ntype: Fact\n---\nA.\n").unwrap();
        }
        let b = leer(&raiz).unwrap();
        assert!(b.version.is_none() && b.avisos.iter().any(|a| a.contains("index.md") && a.contains("1 MB")), "{:?}", b.avisos);
        assert_eq!(b.conceptos.len(), 5);
        let e = format!("{}", leer_con_topes(&raiz, &Topes { ficheros: 4, ..Topes::default() }).unwrap_err());
        assert!(e.contains("over 4 files and folders") && e.contains("nothing was imported"), "{e}");
        let e = format!("{}", leer_con_topes(&raiz, &Topes { bytes: 40, ..Topes::default() }).unwrap_err());
        assert!(e.contains("MB of concepts"), "{e}");
        // Los topes de las cabeceras: cada una, todas juntas y los valores leídos.
        let e = format!("{}", leer_con_topes(&raiz, &Topes { cabeceras: 40, ..Topes::default() }).unwrap_err());
        assert!(e.contains("MB of YAML headers") && e.contains("nothing was imported"), "{e}");
        let e = format!("{}", leer_con_topes(&raiz, &Topes { nodos: 3, ..Topes::default() }).unwrap_err());
        assert!(e.contains("values in its YAML headers") && e.contains("nothing was imported"), "{e}");
        assert_eq!(MAX_BYTES, 50 << 20);
        assert_eq!(MAX_CABECERA, 256 << 10);
        // Una cabecera de más de 256 KB (aquí, una lista `[…]` de 300 KB): el bundle no vale, sin leerla.
        std::fs::write(raiz.join("sub").join("enorme.md"), format!("---\ntype: Fact\nx: [{}]\n---\nA.\n", "a, ".repeat(100_000))).unwrap();
        let inicio = std::time::Instant::now();
        let e = format!("{:#}", leer(&raiz).unwrap_err());
        assert!(e.contains("sub/enorme.md") && e.contains("over 256 KB"), "{e}");
        assert!(inicio.elapsed() < std::time::Duration::from_secs(2), "{:?}", inicio.elapsed());
        std::fs::remove_file(raiz.join("sub").join("enorme.md")).unwrap();
        // Un `index.md` que es un enlace (aquí, a un fichero de fuera) no se lee.
        let fuera = t.join("fuera.md");
        std::fs::write(&fuera, "---\nokf_version: \"9.9\"\n---\n").unwrap();
        std::fs::remove_file(raiz.join("index.md")).unwrap();
        // En Windows, crear un enlace simbólico pide el modo de desarrollador: sin él, no se comprueba.
        #[cfg(windows)]
        let enlazado = std::os::windows::fs::symlink_file(&fuera, raiz.join("index.md")).is_ok();
        #[cfg(unix)]
        let enlazado = std::os::unix::fs::symlink(&fuera, raiz.join("index.md")).is_ok();
        if enlazado {
            let b = leer(&raiz).unwrap();
            assert!(b.version.is_none() && b.avisos.iter().any(|a| a.contains("index.md") && a.contains("symbolic link")), "{:?}", b.avisos);
        }
    }
}
