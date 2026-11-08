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
//!   duplica. Va al proyecto de la carpeta (o `--dir`; lo global, a lo global) o, con `--space`, a ese
//!   espacio. Título, contenido y datos pasan por la redacción de secretos, como cualquier `remember`.
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
//! | etiqueta `verified` (o `verified:<quién>`) | `verified: [{ by: human:<quién>, at }]` (al importar, un `verified` de una persona da la etiqueta) |
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
use crate::obsidian::{Que, corto, slug};
use crate::{tr, veredictos};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// La versión de OKF que se escribe.
pub const VERSION: &str = "0.2";
/// La etiqueta de lo que una persona ha comprobado (`verified`, o `verified:<quién>`). Si la 1.8.5 trae
/// otra forma de marcarlo, se cambia aquí.
pub const ETIQUETA_VERIFICADA: &str = "verified";
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
        let texto = concepto(m, &grupo(m).titulo, datos.get(&m.id), &rel, &descripcion, &pie);
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

/// Las verificaciones: las que trajo de OKF (`data.okf.verified`) sin las de personas si ya no lleva
/// la etiqueta, y la suya si la lleva y no hay ninguna de una persona.
fn verificaciones(m: &Memoria, guardadas: Option<&Value>) -> Vec<Value> {
    let marca = m.tags.iter().find(|t| *t == ETIQUETA_VERIFICADA || t.starts_with(&format!("{ETIQUETA_VERIFICADA}:")));
    let mut v: Vec<Value> = match guardadas {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    };
    if marca.is_none() {
        v.retain(|x| !es_humano(x.get("by").and_then(Value::as_str).unwrap_or("")));
    } else if !v.iter().any(|x| es_humano(x.get("by").and_then(Value::as_str).unwrap_or(""))) {
        let quien = marca.and_then(|t| t.split_once(':')).map(|(_, q)| q.to_string()).filter(|q| !q.is_empty()).unwrap_or_else(|| m.author.clone());
        let quien = if quien.trim().is_empty() { "user".to_string() } else { slug(&quien, 60) };
        v.push(json!({ "by": format!("human:{quien}"), "at": m.updated_at }));
    }
    v
}

/// Un actor de persona, en la forma de OKF (`human:<id>`) o en la de okf-agent-memory (`human/…`).
fn es_humano(by: &str) -> bool {
    let b = by.trim().to_lowercase();
    b == "human" || b.starts_with("human:") || b.starts_with("human/")
}

fn concepto(m: &Memoria, proyecto: &str, datos: Option<&Value>, rel: &Relaciones, descripcion: &str, pie: &[String]) -> String {
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
        .filter(|t| !t.starts_with(crate::revisar::ETIQUETA_FECHA) && !t.starts_with(memoria::ETIQUETA_TEMA) && *t != ETIQUETA_VERIFICADA && !t.starts_with(&format!("{ETIQUETA_VERIFICADA}:")) && !es_estado(t))
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
    f.insert("generated".into(), json!({ "by": por, "at": m.updated_at }));
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
    f.insert("omoikane".into(), Value::Object(o));
    let mut s = String::from("---\n");
    for (k, v) in &f {
        yaml::bloque(&mut s, k, v, 0);
    }
    s += "---\n\n";
    s += m.content.trim_end();
    s += "\n";
    if !pie.is_empty() {
        s += &format!("\n{PIE}\n{}\n", pie.join("\n"));
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
            } else if p.extension().is_some_and(|x| x == "md") && !escritos.contains(&p) {
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
    if !dir.is_dir() {
        bail!(tr!("{} is not a folder", "{} no es una carpeta", dir.display()));
    }
    let raiz = if !dir.join("index.md").exists() && dir.join("knowledge").join("index.md").exists() { dir.join("knowledge") } else { dir.to_path_buf() };
    let mut version = None;
    let indice = raiz.join("index.md");
    if indice.exists() {
        let t = std::fs::read_to_string(&indice).with_context(|| indice.display().to_string())?;
        if let Some((cabecera, _)) = separar(&t) {
            let c = yaml::leer(cabecera).map_err(|e| anyhow!(tr!("{}: the YAML header can't be read: {e}", "{}: no se puede leer la cabecera YAML: {e}", indice.display())))?;
            version = c.get("okf_version").map(|v| match v {
                Value::String(s) => s.clone(),
                otro => otro.to_string(),
            });
        }
    }
    let mut ficheros = Vec::new();
    let mut pendientes = vec![raiz.clone()];
    while let Some(d) = pendientes.pop() {
        let rd = std::fs::read_dir(&d).with_context(|| d.display().to_string())?;
        for e in rd.flatten() {
            let p = e.path();
            let nombre = e.file_name().to_string_lossy().to_string();
            if nombre.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                pendientes.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")) && !matches!(nombre.as_str(), "index.md" | "log.md") {
                ficheros.push(p);
            }
        }
    }
    ficheros.sort();
    let mut conceptos = Vec::new();
    let mut malos = Vec::new();
    for p in &ficheros {
        let rel = p.strip_prefix(&raiz).unwrap_or(p).to_string_lossy().replace('\\', "/");
        let id = rel.strip_suffix(".md").or_else(|| rel.strip_suffix(".MD")).unwrap_or(&rel).to_string();
        let texto = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                malos.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let Some((cabecera, cuerpo)) = separar(&texto) else {
            malos.push(tr!("{rel}: no YAML header (--- … ---)", "{rel}: sin cabecera YAML (--- … ---)"));
            continue;
        };
        let c = match yaml::leer(cabecera) {
            Ok(c) => c,
            Err(e) => {
                malos.push(tr!("{rel}: the YAML header can't be read: {e}", "{rel}: no se puede leer la cabecera YAML: {e}"));
                continue;
            }
        };
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
    Ok(Bundle { raiz, version, conceptos })
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
    let verificado = match f.get("verified") {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    };
    if verificado.iter().any(|v| es_humano(v.get("by").and_then(Value::as_str).unwrap_or(""))) {
        tags.push(ETIQUETA_VERIFICADA.into());
    }
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
    let datos = match (om.get("data").cloned(), okf.is_empty()) {
        (d, true) => d,
        (None, false) => Some(json!({ DATOS_OKF: okf })),
        (Some(Value::Object(mut o)), false) => {
            o.insert(DATOS_OKF.into(), Value::Object(okf));
            Some(Value::Object(o))
        }
        (Some(otro), false) => Some(json!({ "value": otro, DATOS_OKF: okf })),
    };
    let generado = f.get("generated").and_then(|g| g.get("at")).map(escalar_texto);
    let texto = |k: &str| om.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Entrada {
        concepto: c.id.clone(),
        clave: clave_de_concepto(&c.id, &om),
        su_clave: om.get("key").and_then(Value::as_str).and_then(|k| memoria::clave_tema(k).ok()),
        id: texto("id").filter(|i| ulid::Ulid::from_string(i).is_ok()),
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
    })
}

fn tipo_okf_de(t: Tipo) -> String {
    tipo_okf(t.as_str())
}

fn es_resumen(c: &Concepto) -> bool {
    c.cabecera.get("omoikane").and_then(|o| o.get("hub")).and_then(Value::as_bool) == Some(true)
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
}

/// Importa un bundle ya leído. Con `en_seco`, solo cuenta lo que haría, sin escribir (`created`: lo que
/// sería nuevo; `updated`: lo que ya está y se pondría al día).
pub fn importar(db: &mut Connection, modelo: Option<&Modelo>, b: &Bundle, destino: &Destino, en_seco: bool) -> Result<Resumen> {
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
    // Los resúmenes de grupo de una exportación de Omoikane no son memorias.
    let conceptos: Vec<&Concepto> = b.conceptos.iter().filter(|c| !es_resumen(c)).collect();
    let mut r = Resumen { concepts: conceptos.len(), ..Resumen::default() };
    // Id de concepto (en minúsculas) → id de memoria, y las relaciones que trae cada uno.
    let mut ids: HashMap<String, String> = HashMap::new();
    // Las memorias que ya ha tocado esta importación.
    let mut tocadas: BTreeSet<String> = BTreeSet::new();
    let mut relaciones: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    for c in conceptos {
        match importar_uno(db, modelo, c, destino, en_seco, &mut tocadas) {
            Ok((id, hecho, e, tapado)) => {
                match hecho {
                    Hecho::Creada => r.created += 1,
                    Hecho::Cambiada => r.updated += 1,
                    Hecho::Igual => r.unchanged += 1,
                }
                if tapado {
                    r.redacted.push(e.nueva.title.clone());
                }
                if let Some(id) = id {
                    ids.insert(e.concepto.to_lowercase(), id.clone());
                    relaciones.push((id, e.sustituida_por, e.contradice));
                }
            }
            Err(err) => {
                eprintln!("omoikane: {}: {err:#}", c.id);
                r.failed += 1;
            }
        }
    }
    if en_seco {
        return Ok(r);
    }
    // Los veredictos entre lo importado: `superseded_by` y `contradicts` llevan a otros conceptos del
    // bundle (`/ruta.md`). Uno que ya vale no se vuelve a poner.
    let vigentes = veredictos::parejas(db)?;
    let id_de = |ruta: &str| -> Option<&String> {
        let r = ruta.trim().trim_start_matches('/');
        let r = r.strip_suffix(".md").unwrap_or(r);
        ids.get(&r.to_lowercase())
    };
    for (id, sustituida_por, contradice) in &relaciones {
        for (relacion, otras) in [(veredictos::SUSTITUIDA, sustituida_por), (veredictos::CONTRADICE, contradice)] {
            for otra in otras.iter().filter_map(|o| id_de(o)) {
                if otra == id {
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

/// Importa un concepto: el id de su memoria (en seco, el de la que ya está, si está), qué pasó, la
/// entrada y si se tapó algo que parecía un secreto.
fn importar_uno(db: &mut Connection, modelo: Option<&Modelo>, c: &Concepto, destino: &Destino, en_seco: bool, tocadas: &mut BTreeSet<String>) -> Result<(Option<String>, Hecho, Entrada, bool)> {
    let mut e = entrada(c)?;
    let (proyecto, project_id, space_id) = donde(db, destino, &e)?;
    e.nueva.project = proyecto.clone();
    e.nueva.project_id = project_id;
    e.nueva.space_id = space_id;
    // La misma memoria, si esta base ya la tiene (se exportó de aquí) y está donde caería.
    let propia = match &e.id {
        Some(id) if memoria::obtener(db, id).is_ok() => {
            let (global, espacio) = memoria::alcance_de_memoria(db, id)?.unwrap_or((false, String::new()));
            let su_espacio = crate::proyecto::clave_espacio(db, space_id)?;
            (espacio == su_espacio && (space_id.is_some() || global == e.global)).then(|| id.clone())
        }
        _ => None,
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
    let con_su_id = e.id.clone().filter(|id| propia.is_none() && por_su_clave.is_none() && memoria::obtener(db, id).is_err());
    let antes = match (&propia, &por_su_clave) {
        (Some(id), _) => Some((id.clone(), sello(db, id)?)),
        (None, Some((_, x))) => Some(x.clone()),
        (None, None) if con_su_id.is_some() => None,
        (None, None) => existente(db, &proyecto, project_id, space_id, &e.clave)?,
    };
    if en_seco {
        let hecho = if antes.is_some() { Hecho::Cambiada } else { Hecho::Creada };
        return Ok((antes.map(|(id, _)| id), hecho, e, false));
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
                    params![v.trim().trim_end_matches('Z').replace('T', " "), id],
                )?;
            }
        }
    }
    let hecho = match antes {
        None => Hecho::Creada,
        Some((_, h)) if sello(db, &id)? != h => Hecho::Cambiada,
        Some(_) => Hecho::Igual,
    };
    tocadas.insert(id.clone());
    Ok((Some(id), hecho, e, secreto || !tapados.is_empty()))
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
        let mut l = Lector { lineas, crudas, i: 0 };
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
                let mut todo = resto.to_string();
                while !cerrado(&todo) {
                    let Some(l) = self.lineas.get(self.i) else { return Err(format!("line {}: unclosed {}", n + 1, &resto[..1])) };
                    todo.push(' ');
                    todo.push_str(sin_comentario(&l.texto));
                    self.i += 1;
                }
                let mut p = Flujo { c: todo.chars().collect(), i: 0 };
                let v = p.valor().map_err(|e| format!("line {}: {e}", n + 1))?;
                p.espacios();
                if p.i < p.c.len() {
                    return Err(format!("line {}: unexpected text after the closing bracket", n + 1));
                }
                return Ok(v);
            }
            if resto.starts_with('"') || resto.starts_with('\'') {
                let mut p = Flujo { c: resto.chars().collect(), i: 0 };
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
            let partes: Vec<String> = crudas.iter().map(|l| if l.len() >= dentro { l[dentro..].to_string() } else { String::new() }).collect();
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
            let mut p = Flujo { c: t.chars().collect(), i: 0 };
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

    /// Si los `[` y `{` de fuera de las comillas están cerrados.
    fn cerrado(t: &str) -> bool {
        let mut nivel: i32 = 0;
        let mut comilla: Option<char> = None;
        let mut escapado = false;
        for c in t.chars() {
            match comilla {
                Some('"') if escapado => escapado = false,
                Some('"') if c == '\\' => escapado = true,
                Some(q) if c == q => comilla = None,
                Some(_) => {}
                None => match c {
                    '"' | '\'' => comilla = Some(c),
                    '[' | '{' => nivel += 1,
                    ']' | '}' => nivel -= 1,
                    _ => {}
                },
            }
        }
        nivel <= 0 && comilla.is_none()
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
    }

    impl Flujo {
        fn espacios(&mut self) {
            while self.c.get(self.i).is_some_and(|c| c.is_whitespace()) {
                self.i += 1;
            }
        }

        fn valor(&mut self) -> Result<Value, String> {
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
    }

    #[test]
    fn separar_cabecera() {
        assert_eq!(separar("---\na: 1\n---\ncuerpo\n"), Some(("a: 1\n", "cuerpo\n")));
        assert_eq!(separar("\u{feff}---\r\na: 1\r\n---\r\nx"), Some(("a: 1\r\n", "x")));
        assert_eq!(separar("sin cabecera"), None);
    }
}
