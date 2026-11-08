//! Ver la memoria en Obsidian (1.3.5): `omoi export --obsidian <vault>` escribe una nota Markdown por
//! memoria en `<vault>/omoikane/`, para mirarla con la vista de grafo de Obsidian. Solo hacia fuera:
//! la carpeta es un espejo que se rehace en cada exportación, y lo que se edite allí se pierde.
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

use crate::memoria::{Ambito, Memoria};
use crate::tr;
use anyhow::{Context, Result};
use rusqlite::Connection;
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
}

/// Qué se exporta: lo personal y lo global, o un espacio sin lo global.
#[derive(Clone, Copy, Debug)]
pub enum Que {
    Personal,
    Espacio(i64),
}

/// Exporta al vault y devuelve qué ha cambiado.
pub fn exportar(db: &Connection, vault: &Path, que: Que) -> Result<Cuenta> {
    let ambito = match que {
        Que::Personal => Ambito::Espacio(None),
        Que::Espacio(s) => Ambito::Espacio(Some(s)),
    };
    let mut todas = crate::memoria::recientes(db, &ambito, None, usize::MAX >> 1)?;
    if matches!(que, Que::Espacio(_)) {
        todas.retain(|m| !m.global());
    }
    todas.sort_by(|a, b| a.id.cmp(&b.id));
    let raiz = vault.join(CARPETA);
    std::fs::create_dir_all(&raiz).with_context(|| tr!("could not create {}", "no se pudo crear {}", raiz.display()))?;

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
                Grupo { carpeta: "_global".into(), nota: String::new(), titulo: tr!("Global", "Global").to_string() }
            } else {
                let s = slug(&m.space, 40);
                Grupo { carpeta: format!("_{s}"), nota: format!("espacio-{s}"), titulo: m.space.clone() }
            };
        }
        let nombre = crate::proyecto::nombre_de(&m.project, &m.project_key);
        let mut s = slug(&nombre, 40);
        if nombres.get(&nombre).is_some_and(|k| k.len() > 1) {
            s = format!("{s}-{}", corto(&m.project_key));
        }
        Grupo { carpeta: s.clone(), nota: format!("proyecto-{s}"), titulo: nombre }
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

    let mut escritos: BTreeSet<PathBuf> = BTreeSet::new();
    let mut c = Cuenta { memories: todas.len(), ..Cuenta::default() };
    let mut grupos: BTreeMap<String, Grupo> = BTreeMap::new();
    for m in &todas {
        let g = grupo(m);
        let ruta = raiz.join(&g.carpeta).join(format!("{}.md", ficheros[&m.id]));
        let texto = nota(m, &g, &ficheros, &destinos, &nota_fichero, &sustituida_por, &sustituye_a, &contradice);
        escribir(&ruta, &texto, &mut c)?;
        escritos.insert(ruta);
        if !g.nota.is_empty() {
            grupos.insert(g.nota.clone(), g);
        }
    }
    for g in grupos.values() {
        let texto = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-hub\"]\n---\n# {}\n\n{}\n",
            g.titulo,
            tr!("The memories of this group link here. A copy from Omoikane: rewritten on every export.", "Las memorias de este grupo enlazan aquí. Copia de Omoikane: se rehace en cada exportación.")
        );
        let ruta = raiz.join(&g.carpeta).join(format!("{}.md", g.nota));
        escribir(&ruta, &texto, &mut c)?;
        escritos.insert(ruta);
        c.hubs += 1;
    }
    for (p, n) in &nota_fichero {
        let texto = format!(
            "---\nomoikane: hub\ntags: [\"omoikane-fichero\"]\n---\n# {}\n\n{}\n",
            p.replace('\\', "/"),
            tr!("The memories that cite this file link here.", "Las memorias que citan este fichero enlazan aquí.")
        );
        let ruta = raiz.join("_ficheros").join(format!("{n}.md"));
        escribir(&ruta, &texto, &mut c)?;
        escritos.insert(ruta);
        c.hubs += 1;
    }
    c.deleted = limpiar(&raiz, &escritos)?;
    configurar_grafo(vault)?;
    Ok(c)
}

struct Grupo {
    carpeta: String,
    nota: String,
    titulo: String,
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
    s += "\n\n---\n";
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
    s += &format!("\n*{}*\n", tr!("A copy from Omoikane: rewritten on every export; changes made here are lost.", "Copia de Omoikane: se rehace en cada exportación; lo que cambies aquí se pierde."));
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
}
