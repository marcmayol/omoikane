//! Ver la memoria en Obsidian (1.3.5): `omoi export --obsidian <vault>` escribe una nota Markdown por
//! memoria en `<vault>/omoikane/`, para mirarla con la vista de grafo de Obsidian. La carpeta es un
//! espejo que se rehace en cada exportación; desde la 1.9.0, `omoi import --obsidian <vault>` trae de
//! vuelta lo editado allí (ver [`importar`]), y hasta entonces la exportación no pisa ni borra una nota
//! editada (ver [`pendiente`]).
//!
//! - Una carpeta por proyecto (`_global` para lo global, `_<espacio>` para lo de un espacio sin
//!   proyecto), plana: el tipo va en las etiquetas, que es lo que colorea el grafo.
//! - Cada nota enlaza a la de su proyecto (`proyecto-<nombre>`; lo global, a ninguna: casi todo es
//!   global para quien trabaja desde su carpeta personal, y una sola nota para todo lo convertía en
//!   el centro de una estrella que tapaba los enlaces de verdad), a las memorias que nombra con
//!   `[[nombre]]` (resueltos con las reglas de `enlaces`, pero solo entre lo exportado), a las que
//!   sustituye o contradice (veredictos vigentes, `veredictos::parejas`) y a una nota por fichero
//!   citado por dos memorias o más (`_ficheros/`).
//! - Cada nota se llama como el título de su memoria (es lo que Obsidian enseña como título), con el
//!   final del id detrás solo si otra se llamaría igual: retitular una memoria la mueve de nombre, y
//!   la vieja se borra. Solo se escribe lo que cambia y solo se borra lo que lleva la marca
//!   `omoikane:` en la cabecera (lo que haya puesto otra persona en la carpeta se queda).
//! - Ámbito: por defecto lo personal (proyectos sin espacio) y lo global; con un espacio, solo lo de
//!   ese cliente, sin lo global. Nunca dos espacios en el mismo vault.
//! - `.obsidian/graph.json` con un color por tipo, solo si el vault no tiene uno.
//! - Cada nota de memoria lleva en la cabecera `base` (la huella de lo editable, tal como salió) y
//!   `sig` (la firma de esta base, con la clave de la de OKF: `okf::firma`), para saber al volver si
//!   se editó y si de verdad es de esa memoria.

use crate::memoria::{Ambito, Cambios, Memoria, Tipo};
use crate::tr;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// La subcarpeta del vault que es de Omoikane.
pub const CARPETA: &str = "omoikane";

/// Lo que hizo una exportación.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Cuenta {
    pub memories: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
    pub hubs: usize,
    /// Las notas editadas en Obsidian que aún no se han importado: no se pisan ni se borran.
    pub kept: usize,
}

/// Qué se exporta: lo personal y lo global, o un espacio sin lo global.
#[derive(Clone, Copy, Debug)]
pub enum Que {
    Personal,
    Espacio(i64),
}

/// Exporta al vault y devuelve qué ha cambiado.
pub fn exportar(db: &Connection, vault: &Path, que: Que) -> Result<Cuenta> {
    exportar_con(db, vault, que, &BTreeSet::new())
}

/// Exporta; las notas de `pisar` se rehacen aunque estén editadas sin importar (`--prefer omoikane`).
fn exportar_con(db: &Connection, vault: &Path, que: Que, pisar: &BTreeSet<PathBuf>) -> Result<Cuenta> {
    // Una carpeta de red no se mira (ver `proyecto::ruta_local`); con una letra de unidad, sí.
    crate::proyecto::exigir_local(vault)?;
    let p = preparar(db, que)?;
    let raiz = vault.join(CARPETA);
    std::fs::create_dir_all(&raiz).with_context(|| tr!("could not create {}", "no se pudo crear {}", raiz.display()))?;
    let clave = crate::okf::firma::clave(db, true)?.context("okf_firma")?;
    let alcance = alcance_de(db, que)?;

    // Lo editado en Obsidian y aún sin importar no se pisa ni se borra (ver `pendiente`).
    let por_id: HashMap<&str, usize> = p.notas.iter().enumerate().map(|(i, n)| (n.m.id.as_str(), i)).collect();
    let mut guardar: BTreeSet<PathBuf> = BTreeSet::new();
    for (ruta, l) in notas_del_vault(&raiz, &mut Vec::new()).unwrap_or_default() {
        if !pisar.contains(&ruta) && pendiente(&l, &p, &por_id, &clave, &alcance) {
            guardar.insert(ruta);
        }
    }

    let mut escritos: BTreeSet<PathBuf> = BTreeSet::new();
    let mut c = Cuenta { memories: p.notas.len(), kept: guardar.len(), ..Cuenta::default() };
    for n in &p.notas {
        let ruta = raiz.join(&n.rel);
        if !guardar.contains(&ruta) {
            escribir(&ruta, &firmada(&n.texto, &n.rel, &n.m, &alcance, &clave), &mut c)?;
        }
        escritos.insert(ruta);
    }
    for (rel, texto) in &p.hubs {
        let ruta = raiz.join(rel);
        escribir(&ruta, texto, &mut c)?;
        escritos.insert(ruta);
        c.hubs += 1;
    }
    escritos.extend(guardar);
    c.deleted = limpiar(&raiz, &escritos)?;
    configurar_grafo(vault)?;
    Ok(c)
}

/// Una nota de memoria como la escribe la exportación ahora (sin `base` ni `sig`).
struct Render {
    m: Memoria,
    /// Su ruta dentro de `<vault>/omoikane/`.
    rel: PathBuf,
    texto: String,
}

/// Todo lo que escribiría una exportación, sin escribir nada.
struct Preparado {
    notas: Vec<Render>,
    /// Las notas de grupo y de fichero (ruta dentro de `omoikane/`, texto).
    hubs: Vec<(PathBuf, String)>,
    /// Los grupos, por su carpeta en minúsculas: a qué proyecto va una nota nueva puesta en ella.
    grupos: BTreeMap<String, Grupo>,
    /// El nombre (en minúsculas) de la nota de cada memoria → su id.
    nombres: HashMap<String, String>,
}

fn preparar(db: &Connection, que: Que) -> Result<Preparado> {
    let ambito = match que {
        Que::Personal => Ambito::Espacio(None),
        Que::Espacio(s) => Ambito::Espacio(Some(s)),
    };
    let mut todas = crate::memoria::recientes(db, &ambito, None, usize::MAX >> 1)?;
    if matches!(que, Que::Espacio(_)) {
        todas.retain(|m| !m.global());
    }
    todas.sort_by(|a, b| a.id.cmp(&b.id));
    let espacio = match que {
        Que::Personal => None,
        Que::Espacio(s) => Some(s),
    };

    // Grupo (carpeta y nota de proyecto) de cada memoria; dos proyectos con el mismo nombre se
    // distinguen por el principio de su clave.
    let mut nombres: HashMap<String, BTreeSet<String>> = HashMap::new();
    for m in &todas {
        if m.project_id != 0 {
            nombres.entry(crate::proyecto::nombre_de(&m.project, &m.project_key)).or_default().insert(m.project_key.clone());
        }
    }
    let grupo = |m: &Memoria| -> Grupo {
        if m.project_id == 0 {
            return if m.space.is_empty() {
                Grupo { carpeta: "_global".into(), nota: String::new(), titulo: tr!("Global", "Global").to_string(), project_id: 0, space_id: None }
            } else {
                let s = slug(&m.space, 40);
                Grupo { carpeta: format!("_{s}"), nota: format!("espacio-{s}"), titulo: m.space.clone(), project_id: 0, space_id: espacio }
            };
        }
        let nombre = crate::proyecto::nombre_de(&m.project, &m.project_key);
        let mut s = slug(&nombre, 40);
        if nombres.get(&nombre).is_some_and(|k| k.len() > 1) {
            s = format!("{s}-{}", corto(&m.project_key));
        }
        Grupo { carpeta: s.clone(), nota: format!("proyecto-{s}"), titulo: nombre, project_id: m.project_id, space_id: None }
    };

    let ocupados: BTreeSet<String> = todas.iter().map(|m| grupo(m).nota.to_lowercase()).filter(|n| !n.is_empty()).collect();
    let ficheros = nota_por_id(&todas, &ocupados);
    // Los `[[nombre]]`, como en `enlaces::recalcular`, pero solo entre lo exportado.
    let mut por_nombre: HashMap<String, Vec<&str>> = HashMap::new();
    for m in &todas {
        for t in &m.tags {
            por_nombre.entry(crate::enlaces::nombre(t)).or_default().push(m.id.as_str());
        }
    }
    let destinos = |m: &Memoria, e: &str| -> Vec<String> {
        match por_nombre.get(&crate::enlaces::nombre(e)) {
            Some(v) => v.iter().filter(|d| **d != m.id).map(|d| d.to_string()).collect(),
            None => {
                let s = e.trim().to_uppercase();
                let es_id = s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric());
                if es_id { todas.iter().find(|x| x.id.starts_with(&s) && x.id != m.id).map(|x| vec![x.id.clone()]).unwrap_or_default() } else { vec![] }
            }
        }
    };

    // Veredictos vigentes entre lo exportado.
    let mut sustituida_por: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut sustituye_a: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut contradice: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in crate::veredictos::parejas(db)?.into_values() {
        if !ficheros.contains_key(&v.memory_id) || !ficheros.contains_key(&v.other_id) {
            continue;
        }
        if v.relation == crate::veredictos::SUSTITUIDA {
            sustituida_por.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            sustituye_a.entry(v.other_id).or_default().push(v.memory_id);
        } else if v.relation == crate::veredictos::CONTRADICE {
            contradice.entry(v.memory_id.clone()).or_default().push(v.other_id.clone());
            contradice.entry(v.other_id).or_default().push(v.memory_id);
        }
    }

    // Una nota por fichero citado por dos memorias o más.
    let mut citas: BTreeMap<String, usize> = BTreeMap::new();
    for m in &todas {
        for p in m.paths.iter().collect::<BTreeSet<_>>() {
            *citas.entry(p.clone()).or_default() += 1;
        }
    }
    let nota_fichero: BTreeMap<String, String> = citas.into_iter().filter(|(_, n)| *n >= 2).map(|(p, _)| { let n = nota_de_fichero(&p); (p, n) }).collect();

    let mut notas = Vec::new();
    let mut grupos: BTreeMap<String, Grupo> = BTreeMap::new();
    for m in &todas {
        let g = grupo(m);
        let rel = Path::new(&g.carpeta).join(format!("{}.md", ficheros[&m.id]));
        let texto = nota(m, &g, &ficheros, &destinos, &nota_fichero, &sustituida_por, &sustituye_a, &contradice);
        notas.push(Render { m: m.clone(), rel, texto });
        grupos.entry(g.carpeta.to_lowercase()).or_insert(g);
    }
    let mut hubs = Vec::new();
    for g in grupos.values().filter(|g| !g.nota.is_empty()) {
        let texto = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-hub\"]\n---\n# {}\n\n{}\n",
            g.titulo,
            tr!("The memories of this group link here. A copy from Omoikane: rewritten on every export.", "Las memorias de este grupo enlazan aquí. Copia de Omoikane: se rehace en cada exportación.")
        );
        hubs.push((Path::new(&g.carpeta).join(format!("{}.md", g.nota)), texto));
    }
    for (p, n) in &nota_fichero {
        let texto = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-fichero\"]\n---\n# {}\n\n{}\n",
            p.replace('\\', "/"),
            tr!("The memories that cite this file link here.", "Las memorias que citan este fichero enlazan aquí.")
        );
        hubs.push((Path::new(FICHEROS).join(format!("{n}.md")), texto));
    }
    let nombres = ficheros.into_iter().map(|(id, n)| (n.to_lowercase(), id)).collect();
    Ok(Preparado { notas, hubs, grupos, nombres })
}

/// La carpeta de las notas de fichero (no son memorias: la vuelta no entra).
const FICHEROS: &str = "_ficheros";

#[derive(Clone, Debug)]
struct Grupo {
    carpeta: String,
    nota: String,
    titulo: String,
    /// Su proyecto (0: lo global o un espacio sin proyecto).
    project_id: i64,
    /// El espacio de `_<espacio>` (memorias de un espacio sin proyecto).
    space_id: Option<i64>,
}

/// El nombre de la nota de cada memoria: su título, que es lo que Obsidian enseña como título de la
/// nota. Obsidian resuelve los enlaces por nombre en todo el vault sin distinguir mayúsculas, así que
/// si dos memorias (o una memoria y una nota de grupo) se llamarían igual, llevan detrás el final de
/// su id, que no cambia.
fn nota_por_id(todas: &[Memoria], ocupados: &BTreeSet<String>) -> HashMap<String, String> {
    let mut veces: HashMap<String, usize> = HashMap::new();
    for m in todas {
        *veces.entry(titulo_de_fichero(&m.title).to_lowercase()).or_default() += 1;
    }
    todas
        .iter()
        .map(|m| {
            let t = titulo_de_fichero(&m.title);
            let k = t.to_lowercase();
            let repetido = veces[&k] > 1 || ocupados.contains(&k) || reservado(&k);
            (m.id.clone(), if repetido { format!("{t} ({})", corto(&m.id)) } else { t })
        })
        .collect()
}

/// Un título que vale como nombre de fichero en Windows, macOS y Linux y como enlace de Obsidian: sin
/// `\ / : * ? " < > |` (Windows) ni `# ^ [ ]` (Obsidian), sin espacios repetidos ni punto o espacio
/// al final (Windows), y como mucho 100 caracteres.
fn titulo_de_fichero(titulo: &str) -> String {
    let limpio: String = titulo.chars().map(|c| if c.is_control() || "\\/:*?\"<>|#^[]".contains(c) { ' ' } else { c }).collect();
    let r: String = limpio.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
    let r = r.trim_end_matches(['.', ' ']).trim_start_matches(['.', ' ']).to_string();
    if r.is_empty() { "Nota".into() } else { r }
}

/// Los nombres que Windows no deja usar como fichero.
fn reservado(k: &str) -> bool {
    matches!(k, "con" | "prn" | "aux" | "nul") || (k.len() == 4 && (k.starts_with("com") || k.starts_with("lpt")) && k.as_bytes()[3].is_ascii_digit())
}

pub(crate) fn corto(s: &str) -> String {
    let s: Vec<char> = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    s[s.len().saturating_sub(8)..].iter().collect::<String>().to_lowercase()
}

/// Un nombre que vale en Windows, macOS, Linux y en un enlace de Obsidian: letras y cifras, el resto
/// en `-`, en minúsculas y como mucho `max` caracteres.
pub(crate) fn slug(s: &str, max: usize) -> String {
    let mut r = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            r.extend(ch.to_lowercase());
        } else if !r.ends_with('-') && !r.is_empty() {
            r.push('-');
        }
        if r.chars().count() >= max {
            break;
        }
    }
    let r = r.trim_end_matches('-').to_string();
    if r.is_empty() { "nota".into() } else { r }
}

/// La nota de un fichero citado: su nombre y un trozo de la suma de la ruta (dos `main.rs` no chocan).
fn nota_de_fichero(ruta: &str) -> String {
    use sha2::{Digest, Sha256};
    let normal = ruta.replace('\\', "/");
    let nombre = normal.trim_end_matches('/').rsplit('/').next().unwrap_or(&normal);
    let h = Sha256::digest(normal.to_lowercase().as_bytes());
    format!("fichero-{}-{:02x}{:02x}{:02x}", slug(nombre, 50), h[0], h[1], h[2])
}

#[allow(clippy::too_many_arguments)]
fn nota(
    m: &Memoria,
    g: &Grupo,
    ficheros: &HashMap<String, String>,
    destinos: &dyn Fn(&Memoria, &str) -> Vec<String>,
    nota_fichero: &BTreeMap<String, String>,
    sustituida_por: &BTreeMap<String, Vec<String>>,
    sustituye_a: &BTreeMap<String, Vec<String>>,
    contradice: &BTreeMap<String, Vec<String>>,
) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let lista = |v: &[String]| format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "));
    let mut etiquetas = vec![m.kind.as_str().to_string()];
    for t in &m.tags {
        let e = etiqueta(t);
        if !e.is_empty() && !etiquetas.contains(&e) {
            etiquetas.push(e);
        }
    }
    let mut s = String::from("---\n");
    s += &format!("omoikane: {}\n", q(&m.id));
    s += &format!("kind: {}\n", m.kind.as_str());
    s += &format!("aliases: [{}]\n", q(&m.title));
    s += &format!("tags: {}\n", lista(&etiquetas));
    s += &format!("project: {}\n", q(&g.titulo));
    if !m.space.is_empty() {
        s += &format!("space: {}\n", q(&m.space));
    }
    if !m.paths.is_empty() {
        s += &format!("paths: {}\n", lista(&m.paths));
    }
    if m.pinned {
        s += "pinned: true\n";
    }
    if !m.author.is_empty() {
        s += &format!("author: {}\n", q(&m.author));
    }
    // Sin el título como encabezado: Obsidian ya enseña el nombre del fichero, que es el título.
    s += &format!("created: {}\nupdated: {}\n---\n\n", q(&m.created_at), q(&m.updated_at));

    // El contenido, con cada `[[nombre]]` llevado a la nota que toca (o en texto si no lleva a nada
    // exportado: así no salen nodos fantasma) y sin que un `#d62828` se vuelva etiqueta.
    let mut enlazadas: BTreeSet<String> = BTreeSet::new();
    let mut extra: BTreeSet<String> = BTreeSet::new();
    let contenido = cambiar_enlaces(&m.content, |e| {
        let d = destinos(m, e);
        let Some(primera) = d.first() else { return e.to_string() };
        enlazadas.insert(primera.clone());
        extra.extend(d.iter().skip(1).cloned());
        format!("[[{}|{}]]", ficheros[primera], e)
    });
    s += &sin_etiquetas(&contenido);
    s += &format!("\n\n{PIE}\n\n---\n");
    let enlace = |id: &String| format!("[[{}]]", ficheros[id]);
    let linea = |s: &mut String, rotulo: &str, ids: Option<&Vec<String>>| {
        if let Some(v) = ids.filter(|v| !v.is_empty()) {
            *s += &format!("- {rotulo}: {}\n", v.iter().map(enlace).collect::<Vec<_>>().join(", "));
        }
    };
    if !g.nota.is_empty() {
        s += &format!("- {}: [[{}]]\n", tr!("Group", "Grupo"), g.nota);
    }
    linea(&mut s, &tr!("Superseded by", "Sustituida por"), sustituida_por.get(&m.id));
    linea(&mut s, &tr!("Supersedes", "Sustituye a"), sustituye_a.get(&m.id));
    linea(&mut s, &tr!("Contradicts", "Contradice a"), contradice.get(&m.id));
    let otras: Vec<String> = extra.into_iter().filter(|d| !enlazadas.contains(d)).collect();
    linea(&mut s, &tr!("Also links to", "También enlaza a"), Some(&otras));
    if !m.paths.is_empty() {
        let v: Vec<String> = m.paths.iter().map(|p| match nota_fichero.get(p) { Some(n) => format!("[[{n}|{}]]", p.replace('\\', "/")), None => format!("`{}`", p.replace('\\', "/")) }).collect();
        s += &format!("- {}: {}\n", tr!("Files", "Ficheros"), v.join(", "));
    }
    s += &format!(
        "\n*{}*\n",
        tr!(
            "A copy from Omoikane: edit the text above and run `omoi import --obsidian` to bring the change back; what's below the line is rewritten on every export.",
            "Copia de Omoikane: edita el texto de arriba y lanza `omoi import --obsidian` para llevar el cambio de vuelta; lo de debajo de la raya se rehace en cada exportación."
        )
    );
    s
}

/// Una etiqueta de Obsidian: sin espacios ni signos (solo letras, cifras, `_`, `-` y `/`), y no solo cifras.
fn etiqueta(t: &str) -> String {
    let r: String = t.trim().chars().map(|c| if c.is_alphanumeric() || matches!(c, '_' | '-' | '/') { c } else { '-' }).collect();
    let r = r.trim_matches('-').to_string();
    if r.chars().all(|c| c.is_ascii_digit()) { String::new() } else { r }
}

/// Cambia cada `[[nombre]]` (o `[[nombre|texto]]`) por lo que diga `f` del nombre.
fn cambiar_enlaces(texto: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut r = String::new();
    let mut resto = texto;
    while let Some(i) = resto.find("[[") {
        let tras = &resto[i + 2..];
        let Some(fin) = tras.find("]]") else { break };
        let dentro = tras[..fin].split('|').next().unwrap_or("").trim();
        r += &resto[..i];
        if dentro.is_empty() || dentro.len() > 120 || dentro.contains('\n') {
            r += &resto[i..i + 2 + fin + 2];
        } else {
            r += &f(dentro);
        }
        resto = &tras[fin + 2..];
    }
    r + resto
}

/// Escapa un `#` pegado a una palabra (en Obsidian sería una etiqueta: `#d62828`, `#3673`) fuera del
/// código; los títulos (`## Algo`) se quedan.
fn sin_etiquetas(texto: &str) -> String {
    let mut r = String::new();
    let mut en_bloque = false;
    for (n, linea) in texto.split('\n').enumerate() {
        if n > 0 {
            r.push('\n');
        }
        if linea.trim_start().starts_with("```") {
            en_bloque = !en_bloque;
            r += linea;
            continue;
        }
        if en_bloque {
            r += linea;
            continue;
        }
        let mut en_codigo = false;
        let mut previo = ' ';
        let cs: Vec<char> = linea.chars().collect();
        for (i, &c) in cs.iter().enumerate() {
            if c == '`' {
                en_codigo = !en_codigo;
            }
            let siguiente = cs.get(i + 1).copied().unwrap_or(' ');
            if c == '#' && !en_codigo && !previo.is_alphanumeric() && previo != '#' && previo != '\\' && (siguiente.is_alphanumeric() || siguiente == '_') {
                r.push('\\');
            }
            r.push(c);
            previo = c;
        }
    }
    r
}

/// Escribe solo si cambia.
fn escribir(ruta: &Path, texto: &str, c: &mut Cuenta) -> Result<()> {
    match std::fs::read(ruta) {
        Ok(hay) if hay == texto.as_bytes() => {
            c.unchanged += 1;
            return Ok(());
        }
        Ok(_) => c.updated += 1,
        Err(_) => c.created += 1,
    }
    if let Some(p) = ruta.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(ruta, texto).with_context(|| tr!("could not write {}", "no se pudo escribir {}", ruta.display()))?;
    Ok(())
}

/// Borra las notas de Omoikane que ya no salen (y las carpetas que se queden vacías). Solo las que
/// llevan la marca `omoikane:` en la cabecera.
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
                carpetas.push(p.clone());
                pendientes.push(p);
            } else if p.extension().is_some_and(|x| x == "md") && !escritos.contains(&p) && es_nuestra(&p) {
                std::fs::remove_file(&p)?;
                borrados += 1;
            }
        }
    }
    carpetas.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in carpetas {
        let _ = std::fs::remove_dir(&d); // solo si está vacía
    }
    Ok(borrados)
}

fn es_nuestra(p: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(p) else { return false };
    let mut l = BufReader::new(f).lines();
    matches!(l.next(), Some(Ok(a)) if a == "---") && matches!(l.next(), Some(Ok(b)) if b.starts_with("omoikane: "))
}

/// Un color por tipo en la vista de grafo, solo si el vault aún no tiene la suya.
fn configurar_grafo(vault: &Path) -> Result<()> {
    let ruta = vault.join(".obsidian").join("graph.json");
    if ruta.exists() {
        return Ok(());
    }
    let grupo = |q: &str, rgb: u32| serde_json::json!({ "query": q, "color": { "a": 1, "rgb": rgb } });
    let v = serde_json::json!({
        "colorGroups": [
            grupo("tag:#gotcha", 0xd62828),
            grupo("tag:#decision", 0xe9a23b),
            grupo("tag:#preference", 0x2a9d8f),
            grupo("tag:#procedure", 0x8e7dbe),
            grupo("tag:#fact", 0x5b8bd6),
            grupo("tag:#omoikane-hub", 0xeeeeee),
            grupo("tag:#omoikane-fichero", 0x7a7a7a),
        ],
        "showArrow": false,
        "showTags": false,
    });
    std::fs::create_dir_all(ruta.parent().unwrap())?;
    std::fs::write(&ruta, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

/// Exporta cada `intervalo` mientras la base cambie (la primera vez, siempre). No vuelve.
pub fn vigilar(db: &Connection, vault: &Path, que: Que, intervalo: std::time::Duration, mut avisar: impl FnMut(&Cuenta)) -> Result<()> {
    let mut visto: Option<i64> = None;
    loop {
        // `data_version` cambia cuando otra conexión escribe: si no ha cambiado, no hay nada que mirar.
        let v: i64 = db.query_row("PRAGMA data_version", [], |f| f.get(0))?;
        if visto != Some(v) {
            avisar(&exportar(db, vault, que)?);
            visto = Some(v);
        }
        std::thread::sleep(intervalo);
    }
}

// --- La vuelta: de Obsidian a Omoikane (1.9.0) ------------------------------------------------------

/// Lo que separa el texto de una nota (lo que se edita) de los enlaces que la exportación rehace
/// debajo. Es un comentario de Obsidian: no se ve al leer la nota.
const PIE: &str = "%% omoikane: below this line, rewritten on every export %%";
/// El dominio de la firma de una nota (ver [`texto_firma`]).
const FIRMA: &str = "omoikane-obsidian-1";

/// Una nota leída del vault.
#[derive(Clone, Debug)]
struct Leida {
    /// Su nombre sin `.md` (lo que Obsidian enseña como título), en NFC.
    stem: String,
    cab: Map<String, Value>,
    /// El texto: sin la cabecera y, en una nota de Omoikane, sin lo de debajo de [`PIE`].
    cuerpo: String,
}

impl Leida {
    fn campo(&self, k: &str) -> Option<String> {
        self.cab.get(k).map(crate::okf::escalar_texto).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn lista(&self, k: &str) -> Vec<String> {
        crate::okf::lista_de_textos(self.cab.get(k)).into_iter().map(|s| s.trim().to_string()).collect()
    }

    /// El id de su memoria, si es la nota de una memoria exportada por Omoikane.
    fn id(&self) -> Option<String> {
        self.campo("omoikane").filter(|i| i != "hub")
    }

    fn es_hub(&self) -> bool {
        self.campo("omoikane").as_deref() == Some("hub")
    }
}

fn stem_de(rel: &Path) -> String {
    crate::okf::nfc(&rel.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
}

/// Lee una nota: su cabecera YAML (si la tiene; `None` si no se puede leer) y su texto.
fn leer_texto(stem: &str, texto: &str) -> Option<Leida> {
    let texto = crate::okf::sin_crlf_de_git(texto.to_string());
    let (cab, resto) = match crate::okf::separar(&texto) {
        Some((c, r)) => (crate::okf::yaml::leer(c).ok()?, r),
        None => (Map::new(), texto.trim_start_matches('\u{feff}')),
    };
    let mut cuerpo = resto;
    if cab.contains_key("omoikane")
        && let Some(i) = cuerpo.rfind(PIE)
    {
        cuerpo = &cuerpo[..i];
    }
    // En `\n`: un contenido guardado con `\r\n` sale así en la nota, y Obsidian (o el editor) lo puede
    // guardar en `\n`; eso no es una edición. Las líneas sin tocar vuelven de la memoria, con su `\r`.
    let cuerpo = cuerpo.replace("\r\n", "\n");
    Some(Leida { stem: stem.to_string(), cab, cuerpo: cuerpo.trim_start_matches('\n').trim_end().to_string() })
}

/// La huella de lo que se puede editar de una nota (nombre, alias, tipo, etiquetas, rutas y texto),
/// sobre los valores leídos y no sobre el texto de la cabecera: Obsidian la reescribe a su manera
/// (listas en bloque, sin comillas) al tocar una propiedad, y eso no es un cambio.
fn vista(l: &Leida) -> String {
    use sha2::{Digest, Sha256};
    let v = serde_json::json!([l.stem, l.lista("aliases"), l.campo("kind"), l.lista("tags"), l.lista("paths"), l.cuerpo]);
    Sha256::digest(v.to_string().as_bytes())[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Lo que se firma de una nota exportada: de qué memoria es, desde qué alcance se exportó (`""` lo
/// personal, o la clave del espacio), qué versión de la memoria tenía (`updated`) y la huella de lo
/// editable tal como salió (`base`). El texto no: es lo que se edita.
fn texto_firma(id: &str, alcance: &str, updated: &str, base: &str) -> String {
    format!("{FIRMA}\n{}", serde_json::json!({ "id": id, "scope": alcance, "updated": updated, "base": base }))
}

/// La nota con su `base` y su `sig` en la cabecera.
fn firmada(texto: &str, rel: &Path, m: &Memoria, alcance: &str, clave: &[u8]) -> String {
    let Some(l) = leer_texto(&stem_de(rel), texto) else { return texto.to_string() };
    let base = vista(&l);
    let sig = crate::okf::firma::firmar(clave, &texto_firma(&m.id, alcance, &m.updated_at, &base));
    let i = texto.get(3..).and_then(|t| t.find("\n---\n")).map_or(0, |i| i + 4);
    format!("{}base: \"{base}\"\nsig: \"{sig}\"\n{}", &texto[..i], &texto[i..])
}

/// Si la nota la exportó esta base, desde este alcance, para la memoria que dice.
fn firma_vale(l: &Leida, clave: &[u8], alcance: &str) -> bool {
    let (Some(id), Some(base), Some(sig)) = (l.id(), l.campo("base"), l.campo("sig")) else { return false };
    crate::okf::firma::vale(clave, &texto_firma(&id, alcance, &l.campo("updated").unwrap_or_default(), &base), &sig)
}

/// Si se ha editado desde que se exportó.
fn editada(l: &Leida) -> bool {
    l.campo("base").as_deref() != Some(vista(l).as_str())
}

fn alcance_de(db: &Connection, que: Que) -> Result<String> {
    match que {
        Que::Personal => Ok(String::new()),
        Que::Espacio(s) => crate::proyecto::clave_espacio(db, Some(s)),
    }
}

/// Las notas de `<vault>/omoikane/` (menos `_ficheros/`, lo oculto y los enlaces simbólicos), con los
/// topes de un bundle OKF. Lo que se salta, a `avisos`.
fn notas_del_vault(raiz: &Path, avisos: &mut Vec<String>) -> Result<Vec<(PathBuf, Leida)>> {
    let mut r = Vec::new();
    let mut vistas = 0usize;
    let mut pendientes = vec![raiz.to_path_buf()];
    while let Some(d) = pendientes.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            vistas += 1;
            anyhow::ensure!(
                vistas <= crate::okf::MAX_FICHEROS,
                tr!(
                    "{} has over {} files and folders: is it the right vault? Nothing was imported",
                    "{} tiene más de {} ficheros y carpetas: ¿es el vault bueno? No se ha importado nada",
                    raiz.display(),
                    crate::okf::MAX_FICHEROS
                )
            );
            let p = e.path();
            let nombre = e.file_name().to_string_lossy().to_string();
            if nombre.starts_with('.') {
                continue;
            }
            let Ok(t) = e.file_type() else { continue };
            let rel = p.strip_prefix(raiz).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if t.is_symlink() || crate::okf::es_union(&p) {
                avisos.push(tr!("{rel}: a symbolic link, skipped", "{rel}: un enlace simbólico, saltado"));
                continue;
            }
            if t.is_dir() {
                if !(d == raiz && nombre == FICHEROS) {
                    pendientes.push(p);
                }
                continue;
            }
            if !nombre.to_lowercase().ends_with(".md") {
                continue;
            }
            if std::fs::metadata(&p).is_ok_and(|m| m.len() > crate::okf::MAX_CONCEPTO) {
                avisos.push(tr!("{rel}: over 1 MB, skipped", "{rel}: de más de 1 MB, saltada"));
                continue;
            }
            let Ok(texto) = std::fs::read_to_string(&p) else {
                avisos.push(tr!("{rel}: not UTF-8 text, skipped", "{rel}: no es texto UTF-8, saltada"));
                continue;
            };
            match leer_texto(&stem_de(Path::new(&nombre)), &texto) {
                Some(l) => r.push((p, l)),
                None => avisos.push(tr!("{rel}: its YAML header can't be read, skipped", "{rel}: no se puede leer su cabecera YAML, saltada")),
            }
        }
    }
    r.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(r)
}

/// Lo que dejaría una nota editada en su memoria: todo normalizado como al guardar, para compararlo.
#[derive(Debug)]
struct Propuesta {
    title: String,
    content: String,
    /// El tipo, si la nota lo cambia.
    kind: Option<Tipo>,
    tags: Vec<String>,
    paths: Vec<String>,
}

impl Propuesta {
    /// Si la memoria ya está así (lo importado otra vez, o un cambio que no cambia nada).
    fn igual(&self, m: &Memoria) -> bool {
        let orden = |v: &[String]| v.iter().cloned().collect::<BTreeSet<String>>();
        self.title == m.title
            && self.content == m.content
            && self.kind.is_none_or(|k| k == m.kind && m.kind_text.is_none())
            && orden(&self.tags) == orden(&m.tags)
            && orden(&self.paths) == orden(&m.paths)
    }
}

/// Lo que cambia una nota editada (`l`) en su memoria (`m`), comparando con la nota que la exportación
/// escribiría ahora para ella (`b`). Solo título, texto, tipo, etiquetas y rutas: nunca a qué proyecto
/// o espacio pertenece, si es global, fijada o verificada. De las etiquetas, solo las sueltas: las que
/// llevan `:` (`trade:`, `topic:`, `review:`…) dan ámbito o significado y no se quitan ni se ponen.
fn proponer(l: &Leida, m: &Memoria, b: &Leida, nombres: &HashMap<String, String>) -> Result<Propuesta> {
    // El título: al exportar, el nombre del fichero es el alias hecho nombre de fichero. Si siguen
    // casando, manda el alias (editado o no); si no, lo que se haya cambiado: el nombre (Obsidian
    // renombra el fichero al cambiar el título de la nota) si el alias es el de la memoria, y si no el alias.
    let alias = |x: &Leida| x.lista("aliases").into_iter().next().unwrap_or_default();
    let (al, ab) = (alias(l), alias(b));
    let sufijo = format!(" ({})", corto(&m.id));
    let nombre = l.stem.strip_suffix(&sufijo).unwrap_or(&l.stem).to_string();
    let title = if al.is_empty() {
        nombre
    } else if titulo_de_fichero(&al) == nombre || al != ab {
        al
    } else {
        nombre
    };
    let kind = match (l.campo("kind"), b.campo("kind")) {
        (Some(k), antes) if Some(&k) != antes.as_ref() => Some(k.parse::<Tipo>()?),
        _ => None,
    };
    let sueltas = |v: Vec<String>| -> BTreeSet<String> {
        v.into_iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).filter(|t| !t.is_empty() && t.parse::<Tipo>().is_err()).collect()
    };
    let (en_nota, antes) = (sueltas(l.lista("tags")), sueltas(b.lista("tags")));
    let quitadas: BTreeSet<&String> = antes.difference(&en_nota).collect();
    let mut tags = m.tags.clone();
    tags.retain(|t| t.contains(':') || !quitadas.contains(&etiqueta(t).to_lowercase()));
    tags.extend(en_nota.difference(&antes).filter(|t| !t.contains(':')).cloned());
    let paths = if l.lista("paths") != b.lista("paths") { l.lista("paths") } else { m.paths.clone() };
    let content = if l.cuerpo == b.cuerpo { m.content.clone() } else { fusionar(&m.content, &b.cuerpo, &l.cuerpo, nombres) };
    anyhow::ensure!(
        !content.trim().is_empty(),
        tr!("the note's text is empty: to forget the memory, use `omoi forget`", "el texto de la nota está vacío: para olvidar la memoria, `omoi forget`")
    );
    let (title, content, paths, tags) = crate::memoria::como_se_guarda(&title, &content, &paths, &tags)?;
    Ok(Propuesta { title, content, kind, tags, paths })
}

/// El texto nuevo de una memoria: cada línea de la nota editada (`e`) que sigue igual que en la
/// exportada (`b`) vuelve a ser la línea original del contenido (`c`), y las cambiadas pasan por
/// [`inversa`]. Exportar cambia cada línea por separado (enlaces y `#`), así que `c` y `b` tienen las
/// mismas líneas; si no (o la nota es enorme), todas pasan por `inversa`.
fn fusionar(c: &str, b: &str, e: &str, nombres: &HashMap<String, String>) -> String {
    let (cl, bl, el): (Vec<&str>, Vec<&str>, Vec<&str>) = (c.split('\n').collect(), b.split('\n').collect(), e.split('\n').collect());
    if cl.len() != bl.len() || bl.len().saturating_mul(el.len()) > 4_000_000 {
        return el.iter().map(|x| inversa(x, nombres)).collect::<Vec<_>>().join("\n");
    }
    // La subsecuencia común más larga entre `bl` y `el`.
    let (n, m) = (bl.len(), el.len());
    let ancho = m + 1;
    let mut t = vec![0u32; (n + 1) * ancho];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * ancho + j] = if bl[i] == el[j] { t[(i + 1) * ancho + j + 1] + 1 } else { t[(i + 1) * ancho + j].max(t[i * ancho + j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut r: Vec<String> = Vec::with_capacity(m);
    while j < m {
        if i < n && bl[i] == el[j] && t[i * ancho + j] == t[(i + 1) * ancho + j + 1] + 1 {
            r.push(cl[i].to_string());
            i += 1;
            j += 1;
        } else if i < n && t[(i + 1) * ancho + j] >= t[i * ancho + j + 1] {
            i += 1;
        } else {
            r.push(inversa(el[j], nombres));
            j += 1;
        }
    }
    r.join("\n")
}

/// Una línea escrita en Obsidian, a como la guarda Omoikane: `[[nota|nombre]]` (como exporta los
/// enlaces) vuelve a `[[nombre]]`, un `[[nota]]` a otra memoria pasa a `[[<su id>]]` (así se resuelve
/// en Omoikane) y el `\#` que pone la exportación vuelve a `#`. Los demás enlaces, tal cual.
fn inversa(linea: &str, nombres: &HashMap<String, String>) -> String {
    let mut r = String::new();
    let mut resto = linea;
    while let Some(i) = resto.find("[[") {
        let tras = &resto[i + 2..];
        let Some(fin) = tras.find("]]") else { break };
        let dentro = &tras[..fin];
        r += &resto[..i];
        let (destino, texto) = match dentro.split_once('|') {
            Some((a, t)) => (a.trim(), t.trim()),
            None => (dentro.trim(), ""),
        };
        let destino = destino.rsplit('/').next().unwrap_or(destino).trim_end_matches(".md");
        match nombres.get(&destino.to_lowercase()) {
            Some(_) if !texto.is_empty() => r += &format!("[[{texto}]]"),
            Some(id) => r += &format!("[[{id}]]"),
            None => r += &resto[i..i + 2 + fin + 2],
        }
        resto = &tras[fin + 2..];
    }
    r += resto;
    let cs: Vec<char> = r.chars().collect();
    let mut s = String::with_capacity(r.len());
    let mut en_codigo = false;
    for (i, &c) in cs.iter().enumerate() {
        if c == '`' {
            en_codigo = !en_codigo;
        }
        if c == '\\' && !en_codigo && cs.get(i + 1) == Some(&'#') && cs.get(i + 2).is_some_and(|n| n.is_alphanumeric() || *n == '_') {
            continue;
        }
        s.push(c);
    }
    s
}

/// Si una nota de Omoikane está editada y su cambio aún no está en su memoria (sin importar, o en
/// conflicto): la exportación no la pisa ni la borra. También la de una memoria que ya no sale (olvidada
/// o llevada a otro sitio): es texto de la persona, y lo borra ella.
fn pendiente(l: &Leida, p: &Preparado, por_id: &HashMap<&str, usize>, clave: &[u8], alcance: &str) -> bool {
    let Some(id) = l.id() else { return false };
    if !firma_vale(l, clave, alcance) || !editada(l) {
        return false;
    }
    let Some(&i) = por_id.get(id.as_str()) else { return true };
    let n = &p.notas[i];
    let Some(b) = leer_texto(&stem_de(&n.rel), &n.texto) else { return true };
    proponer(l, &n.m, &b, &p.nombres).map(|x| !x.igual(&n.m)).unwrap_or(true)
}

/// Qué hacer con una nota editada cuya memoria también ha cambiado en Omoikane desde la exportación.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preferir {
    /// Nada: se dice y se deja como está (la nota no se pisa).
    #[default]
    Nada,
    /// Lo de Obsidian: la nota se aplica (lo de Omoikane queda en el historial).
    Obsidian,
    /// Lo de Omoikane: la nota se rehace desde la memoria y su cambio se pierde.
    Omoikane,
}

/// Una nota que no ha entrado, y por qué.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Saltada {
    pub note: String,
    pub reason: String,
}

/// Lo que hizo (o haría, en seco) una importación desde Obsidian.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Resumen {
    /// Las notas de memoria (una por memoria) y las nuevas que se han mirado.
    pub notes: usize,
    pub updated: usize,
    pub created: usize,
    pub unchanged: usize,
    /// Editadas en Obsidian y cambiadas también en Omoikane después de exportarlas: no se tocan.
    pub conflicts: Vec<String>,
    /// Conflictos resueltos con `--prefer obsidian` (aplicados) o `--prefer omoikane` (descartados).
    pub conflicts_applied: usize,
    pub conflicts_discarded: usize,
    /// Con `omoikane:` pero sin la firma de esta base y de este alcance (otra base, otra versión, o
    /// retocadas en esos campos): no tocan ninguna memoria.
    pub foreign: Vec<String>,
    /// Editadas, pero su memoria ya no sale en este vault (olvidada, o llevada a otro sitio).
    pub gone: Vec<String>,
    /// Notas nuevas que no pueden entrar (sin proyecto o sin tipo que se pueda deducir, globales, vacías).
    pub skipped: Vec<Saltada>,
    pub failed: Vec<Saltada>,
    /// Los títulos de las memorias en las que se tapó algo que parecía un secreto.
    pub redacted: Vec<String>,
    /// Las memorias verificadas por una persona cuyo texto cambió: pierden la verificación.
    pub unverified: usize,
    /// Lo que se saltó al leer el vault.
    pub notices: Vec<String>,
    /// La exportación que deja el vault al día después de importar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<Cuenta>,
}

/// La clave por la que se guarda una nota nueva: `obsidian:<su ruta>` (importarla otra vez la pone al
/// día en vez de duplicarla).
fn clave_de_nota(rel: &str) -> String {
    let r = rel.to_lowercase();
    let r = r.strip_suffix(".md").unwrap_or(&r);
    let limpio: String = r.chars().map(|c| if c.is_alphanumeric() || "-_./".contains(c) { c } else { '-' }).collect();
    let k = format!("obsidian:{limpio}");
    if k.chars().count() <= 80 {
        return k;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(rel.as_bytes());
    format!("{}-{:02x}{:02x}{:02x}{:02x}", k.chars().take(71).collect::<String>(), h[0], h[1], h[2], h[3])
}

/// Trae a Omoikane lo editado en un vault exportado con [`exportar`] (`omoi import --obsidian`).
///
/// - Una nota de memoria (con `omoikane: <id>`) editada pone al día su memoria, con historial (motivo
///   `obsidian import`): título (el alias o el nombre del fichero), texto, tipo, etiquetas sueltas y
///   rutas. Solo si la nota lleva la firma de esta base para este alcance (`sig`, ver
///   [`texto_firma`]): un id no es un secreto (viaja en la sincronización, en los bundles y en otros
///   vaults), y sin firma nadie puede apuntar una nota a una memoria. Nunca cambia de proyecto o de
///   espacio, ni la hace global, fijada o verificada; cambiar el texto de una verificada le quita la
///   verificación, como `update`. Una memoria de oficio pasa por `oficios::fugas`.
/// - Sin editar (su huella es la de `base`) o ya importada: nada.
/// - Editada y cambiada también en Omoikane después de exportarla (`updated` ya no es el de la
///   memoria): conflicto. Sin `preferir`, no se toca ninguna de las dos y se dice; la exportación
///   tampoco pisa la nota.
/// - Una nota nueva (sin `omoikane:`) en la carpeta de un proyecto (o con un `project:` que es uno de
///   este vault) y con `kind:` (o una etiqueta que es un tipo) entra como memoria nueva de ese proyecto
///   (o del espacio, en `_<espacio>/`), por la clave `obsidian:<ruta>`, sin fijar y con origen
///   `import:obsidian`; su fichero pasa a ser la copia de Omoikane. En `_global/` no: lo global no se
///   crea desde un fichero que puede escribir cualquiera.
/// - Las notas de grupo y de fichero, lo de fuera de `omoikane/` y lo borrado en el vault no hacen nada.
///
/// Todo lo que entra pasa por la redacción de secretos, como cualquier escritura. Después, si no es en
/// seco, exporta otra vez para dejar el vault al día.
pub fn importar(db: &mut Connection, modelo: Option<&crate::embeber::Modelo>, vault: &Path, que: Que, preferir: Preferir, en_seco: bool) -> Result<Resumen> {
    crate::proyecto::exigir_local(vault)?;
    let raiz = vault.join(CARPETA);
    anyhow::ensure!(
        raiz.is_dir(),
        tr!(
            "{} has no {CARPETA}/ folder: export to it first (omoi export --obsidian <vault>)",
            "{} no tiene la carpeta {CARPETA}/: exporta antes a él (omoi export --obsidian <vault>)",
            vault.display()
        )
    );
    let mut r = Resumen::default();
    let notas = notas_del_vault(&raiz, &mut r.notices)?;
    let p = preparar(db, que)?;
    let alcance = alcance_de(db, que)?;
    let clave = crate::okf::firma::clave(db, false)?;
    let rel_de = |ruta: &Path| ruta.strip_prefix(&raiz).unwrap_or(ruta).to_string_lossy().replace('\\', "/");

    // Un vault exportado desde otro alcance (lo personal frente a un espacio): se dice claro.
    let de_memoria: Vec<&Leida> = notas.iter().map(|(_, l)| l).filter(|l| l.id().is_some()).collect();
    if let Some(otro) = de_memoria.first().map(|l| l.campo("space").unwrap_or_default())
        && otro != alcance
        && de_memoria.iter().all(|l| l.campo("space").unwrap_or_default() == otro)
    {
        if alcance.is_empty() {
            bail!(tr!("this vault was exported from the space «{otro}»: import it with --space {otro}", "este vault se exportó del espacio «{otro}»: impórtalo con --space {otro}"));
        }
        bail!(tr!(
            "this vault wasn't exported from the space «{alcance}»: import it with the --space it was exported from (or without --space if it's the personal one)",
            "este vault no se exportó del espacio «{alcance}»: impórtalo con el --space del que se exportó (o sin --space si es el personal)"
        ));
    }

    let mut por_memoria: BTreeMap<String, Vec<(PathBuf, Leida)>> = BTreeMap::new();
    let mut nuevas: Vec<(PathBuf, Leida)> = Vec::new();
    for (ruta, l) in notas {
        if l.es_hub() {
            continue;
        }
        if !l.cab.contains_key("omoikane") {
            nuevas.push((ruta, l));
        } else if let (Some(id), Some(k)) = (l.id(), &clave)
            && firma_vale(&l, k, &alcance)
        {
            por_memoria.entry(id).or_default().push((ruta, l));
        } else {
            r.foreign.push(rel_de(&ruta));
        }
    }

    let por_id: HashMap<&str, usize> = p.notas.iter().enumerate().map(|(i, n)| (n.m.id.as_str(), i)).collect();
    let mut pisar: BTreeSet<PathBuf> = BTreeSet::new();
    for (id, copias) in por_memoria {
        r.notes += 1;
        let editadas: Vec<&(PathBuf, Leida)> = copias.iter().filter(|(_, l)| editada(l)).collect();
        let (ruta, l) = match editadas.as_slice() {
            [] => {
                r.unchanged += 1;
                continue;
            }
            [una] => (&una.0, &una.1),
            varias => {
                // Dos copias de la misma memoria editadas (una renombrada y otra que volvió a salir): no
                // se sabe cuál vale.
                r.conflicts.push(varias.iter().map(|(p, _)| rel_de(p)).collect::<Vec<_>>().join(" + "));
                continue;
            }
        };
        let rel = rel_de(ruta);
        let Some(&i) = por_id.get(id.as_str()) else {
            r.gone.push(rel);
            continue;
        };
        let n = &p.notas[i];
        let m = &n.m;
        let propuesta = leer_texto(&stem_de(&n.rel), &n.texto).context("render").and_then(|b| proponer(l, m, &b, &p.nombres));
        let propuesta = match propuesta {
            Ok(x) => x,
            Err(e) => {
                r.failed.push(Saltada { note: rel, reason: format!("{e:#}") });
                continue;
            }
        };
        // Lo que ya está en su memoria (o se aplica ahora), la exportación de después lo rehace (con el
        // nombre nuevo si cambió el título), aunque la nota siga pareciendo editada.
        if propuesta.igual(m) {
            r.unchanged += 1;
            pisar.insert(ruta.clone());
            continue;
        }
        if l.campo("updated").as_deref() != Some(m.updated_at.as_str()) {
            match preferir {
                Preferir::Nada => {
                    r.conflicts.push(rel);
                    continue;
                }
                Preferir::Omoikane => {
                    r.conflicts_discarded += 1;
                    pisar.insert(ruta.clone());
                    continue;
                }
                Preferir::Obsidian => r.conflicts_applied += 1,
            }
        }
        let mut cambios = Cambios {
            title: (propuesta.title != m.title).then(|| propuesta.title.clone()),
            content: (propuesta.content != m.content).then(|| propuesta.content.clone()),
            kind: propuesta.kind,
            paths: Some(propuesta.paths.clone()),
            tags: Some(propuesta.tags.clone()),
            reason: Some("obsidian import".into()),
            ..Default::default()
        };
        // Una memoria de oficio: lo nuevo pasa por `oficios::fugas`, como en `update`.
        if crate::oficios::lleva_oficio(&m.tags) {
            let pedido = crate::oficios::PedidoCambio { trade: None, para: None, cambios: &cambios, datos: None, permitir_fugas: false };
            match crate::oficios::al_cambiar(db, m, pedido)? {
                Err(fugas) => {
                    let lista: Vec<String> = fugas.iter().map(|f| format!("{} «{}»", f.tipo, f.text)).collect();
                    r.failed.push(Saltada {
                        note: rel,
                        reason: tr!(
                            "a trade memory (reaches every project of its trade) and the change names what looks like a project or a client: {}",
                            "una memoria de oficio (llega a todos los proyectos de su oficio) y el cambio nombra algo que parece de un proyecto o de un cliente: {}",
                            lista.join(", ")
                        ),
                    });
                    continue;
                }
                Ok(plan) if plan.hacer_global || plan.nueva => {
                    r.failed.push(Saltada { note: rel, reason: tr!("it would change where the memory belongs", "cambiaría dónde está la memoria") });
                    continue;
                }
                Ok(plan) => {
                    if let Some(t) = plan.tags {
                        cambios.tags = Some(t);
                    }
                }
            }
        }
        if en_seco {
            r.updated += 1;
            continue;
        }
        match crate::memoria::actualizar(db, &m.id, cambios, modelo) {
            Ok(x) => {
                r.updated += 1;
                pisar.insert(ruta.clone());
                if !x.redacted.is_empty() {
                    r.redacted.push(x.title.clone());
                }
                r.unverified += usize::from(x.unverified.is_some());
            }
            Err(e) => r.failed.push(Saltada { note: rel, reason: format!("{e:#}") }),
        }
    }

    for (ruta, l) in nuevas {
        r.notes += 1;
        let rel = rel_de(&ruta);
        let mut saltar = |motivo: String| r.skipped.push(Saltada { note: rel.clone(), reason: motivo });
        let carpeta = rel.split_once('/').map(|(c, _)| c.to_lowercase());
        let grupo = match carpeta.as_ref().and_then(|c| p.grupos.get(c)) {
            Some(g) => Some(g),
            None => l.campo("project").and_then(|t| p.grupos.values().find(|g| g.titulo.eq_ignore_ascii_case(&t) || g.carpeta.eq_ignore_ascii_case(&t))),
        };
        let Some(g) = grupo else {
            saltar(tr!(
                "no project can be told: put it in a project's folder of this vault (or give it a `project:` that is one)",
                "no se sabe de qué proyecto es: ponla en la carpeta de un proyecto de este vault (o dale un `project:` que sea uno)"
            ));
            continue;
        };
        if g.project_id == 0 && g.space_id.is_none() {
            saltar(tr!(
                "in _global/: a note from the vault doesn't become a global memory (every project sees those); save it with `omoi remember --global`",
                "en _global/: una nota del vault no se convierte en memoria global (las ve cualquier proyecto); guárdala con `omoi remember --global`"
            ));
            continue;
        }
        let kind = match l.campo("kind") {
            Some(k) => match k.parse::<Tipo>() {
                Ok(t) => t,
                Err(e) => {
                    saltar(format!("{e:#}"));
                    continue;
                }
            },
            None => match l.lista("tags").iter().find_map(|t| t.trim_start_matches('#').parse::<Tipo>().ok()) {
                Some(t) => t,
                None => {
                    saltar(tr!(
                        "no kind: add `kind: fact` (or decision, gotcha, preference, procedure) to its properties",
                        "sin tipo: añade `kind: fact` (o decision, gotcha, preference, procedure) a sus propiedades"
                    ));
                    continue;
                }
            },
        };
        if l.cuerpo.trim().is_empty() {
            saltar(tr!("empty", "vacía").to_string());
            continue;
        }
        let title = l.lista("aliases").into_iter().next().filter(|t| !t.is_empty()).or_else(|| l.campo("title")).unwrap_or_else(|| l.stem.clone());
        let tags: Vec<String> = l
            .lista("tags")
            .into_iter()
            .map(|t| t.trim_start_matches('#').to_lowercase())
            .filter(|t| !t.is_empty() && !t.contains(':') && t.parse::<Tipo>().is_err())
            .collect();
        let nueva = crate::memoria::Nueva {
            project_id: (g.project_id != 0).then_some(g.project_id),
            space_id: if g.project_id == 0 { g.space_id } else { None },
            kind: Some(kind),
            title,
            content: l.cuerpo.clone(),
            paths: l.lista("paths"),
            tags,
            pinned: false,
            source: "import:obsidian".into(),
            ..Default::default()
        };
        let clave_nota = clave_de_nota(&rel);
        let tema = format!("{}{clave_nota}", crate::memoria::ETIQUETA_TEMA);
        let antes: Option<(String, String)> = db
            .query_row(
                "SELECT id, hlc FROM memory WHERE project_id = ?1 AND space_id IS ?2
                   AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?3) ORDER BY created_at, id LIMIT 1",
                rusqlite::params![g.project_id, nueva.space_id, tema],
                |f| Ok((f.get(0)?, f.get(1)?)),
            )
            .optional()?;
        if en_seco {
            if antes.is_some() { r.updated += 1 } else { r.created += 1 }
            continue;
        }
        let (titulo, contenido) = (nueva.title.clone(), nueva.content.clone());
        match crate::memoria::guardar_por_clave(db, &clave_nota, nueva, None, modelo) {
            Ok(x) => {
                let cambiada = match &antes {
                    None => true,
                    Some((id, hlc)) => crate::memoria::hlc_de(db, id).map(|h| h != *hlc).unwrap_or(true),
                };
                match (x.creada, cambiada) {
                    (true, _) => r.created += 1,
                    (false, true) => r.updated += 1,
                    (false, false) => r.unchanged += 1,
                }
                let tapado = !x.memoria.redacted.is_empty();
                if tapado {
                    r.redacted.push(x.memoria.title.clone());
                }
                // La nota pasa a ser la copia de Omoikane (la exportación de después la escribe con su id).
                // Si se tapó algo, se queda como está: su texto no es el de la memoria.
                if !tapado && x.memoria.title == titulo.split_whitespace().collect::<Vec<_>>().join(" ") && x.memoria.content == contenido.trim() {
                    let _ = std::fs::remove_file(&ruta);
                }
            }
            Err(e) => r.failed.push(Saltada { note: rel, reason: format!("{e:#}") }),
        }
    }

    if !en_seco {
        r.export = Some(exportar_con(db, vault, que, &pisar)?);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_y_texto() {
        assert_eq!(slug("¿Publicar 1.3.0 en crates.io?", 60), "publicar-1-3-0-en-crates-io");
        assert_eq!(slug("***", 60), "nota");
        assert_eq!(titulo_de_fichero("Su \"Netflix casero\": C:\\Users\\x [beta] #1."), "Su Netflix casero C Users x beta 1");
        assert_eq!(titulo_de_fichero("..."), "Nota");
        assert!(reservado("con") && reservado("com1") && !reservado("comer"));
        assert_eq!(etiqueta("project status"), "project-status");
        assert_eq!(etiqueta("2026"), "");
        assert_eq!(cambiar_enlaces("ver [[a-b|texto]] y [[c]]", |e| format!("<{e}>")), "ver <a-b> y <c>");
        assert_eq!(sin_etiquetas("## Título\nrojo #d62828, `#no` y #3673\n```\n#dentro\n```"), "## Título\nrojo \\#d62828, `#no` y \\#3673\n```\n#dentro\n```");
        assert_ne!(nota_de_fichero("src/main.rs"), nota_de_fichero("tests/main.rs"));
    }

    #[test]
    fn vuelta_del_texto() {
        let nombres: HashMap<String, String> = [("usamos y".to_string(), "01ABC".to_string())].into();
        // Lo exportado, sin tocar, vuelve tal cual; lo nuevo, a la forma de Omoikane.
        assert_eq!(inversa("ver [[Usamos Y|nota-b]], [[Usamos Y]], [[otra]] y \\#rojo `\\#x`", &nombres), "ver [[nota-b]], [[01ABC]], [[otra]] y #rojo `\\#x`");
        let c = "Ver [[nota-b]].\nEl rojo #d62828.\nFin.";
        let b = "Ver [[Usamos Y|nota-b]].\nEl rojo \\#d62828.\nFin.";
        assert_eq!(fusionar(c, b, "Ver [[Usamos Y|nota-b]].\nNueva [[Usamos Y]].\nEl rojo \\#d62828.\nFin.", &nombres), "Ver [[nota-b]].\nNueva [[01ABC]].\nEl rojo #d62828.\nFin.");
        assert_eq!(fusionar(c, b, "El rojo \\#d62828.", &nombres), "El rojo #d62828.");
        // Un contenido en `\r\n` que vuelve en `\n` (como se lee la nota): solo cambia la línea nueva.
        let l = leer_texto("x", "---\nomoikane: \"A\"\n---\n\nUno.\r\nDos.\r\n\n%% omoikane: below this line, rewritten on every export %%\n").unwrap();
        assert_eq!(l.cuerpo, "Uno.\nDos.");
        assert_eq!(fusionar("Uno.\r\nDos.", &l.cuerpo, "Uno.\nDos.\nTres.", &nombres), "Uno.\r\nDos.\nTres.");
    }
}
