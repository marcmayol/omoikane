//! Nombres de proyecto que se parecen: la misma comparación para quien escribe un nombre
//! (`proyecto::buscar`), para la revisión (proyectos duplicados) y para el recall (un nombre mal
//! dictado en el mensaje).
//!
//! Dos niveles. **Igual**: el mismo nombre sin mayúsculas, acentos, guiones, guiones bajos, espacios
//! ni puntos (`social-publish` = `Social_Publish` = `social publish`, `Crónicas` = `cronicas`).
//! **A una letra**: una letra cambiada, de más o de menos, con `LETRAS_MIN` letras o más
//! (`Omoikane`/`Omoikae`). El segundo es menos seguro: la revisión solo lo usa junto a otra señal, y
//! nunca un parecido a secas (`Tokimichi` y `tokimichi-web` son proyectos distintos).

use crate::memoria::Ambito;
use crate::proyecto::{self, Proyecto};
use crate::tr;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

/// Desde cuántas letras (del nombre más corto, normalizado) cuenta el parecido a una letra.
pub const LETRAS_MIN: usize = 6;

/// En el recall, una palabra que no está en ninguna memoria puede ser un nombre mal dictado desde 6
/// letras (la palabra y el nombre): que no exista ya es la otra señal. Con 5, palabras normales que no
/// salían en ninguna memoria se volvían proyectos («tropa» → Troba, «rodeo» y «rodes» → Rodet, «balsa»
/// → Balua): mirado con `examples/eval.rs` sobre `tests/recall_casos.tsv`.
pub const LETRAS_MIN_RECALL: usize = 6;

/// Desde cuántas letras se busca, en la revisión, el nombre de una carpeta que ya no existe en las
/// memorias de otro proyecto (`Narrador` → `Aloud`).
const LETRAS_MIN_CONTINUA: usize = 5;

/// Terminaciones de flexión (plural, infinitivo, participio…) en castellano e inglés: una palabra que
/// solo se aparta de un nombre en sus dos últimas letras y acaba así es una palabra normal con su
/// terminación, no un nombre mal dictado («faltas» no es Faltis; «tokimicho» sí es Tokimichi).
const FLEXIONES: &[&str] = &["s", "ar", "er", "ir", "an", "en", "ed"];

/// Si `t` (normalizado) se aparta de `n` solo en sus dos últimas letras y acaba en una flexión.
fn es_flexion(t: &str, n: &str) -> bool {
    let comun = t.chars().zip(n.chars()).take_while(|(a, b)| a == b).count();
    comun + 2 >= t.chars().count() && FLEXIONES.iter().any(|f| t.ends_with(f))
}

/// La condición SQL sobre `p` (un proyecto) de los proyectos que se ven desde un ámbito: desde un
/// proyecto o un espacio, los de su espacio (o los que no tienen); desde una carpeta general, todos.
fn proyectos_visibles(ambito: &Ambito) -> String {
    match ambito {
        Ambito::Todos => "1".to_string(),
        a => match a.espacio() {
            Some(s) => format!("p.space_id = {s}"),
            None => "p.space_id IS NULL".to_string(),
        },
    }
}

/// Cómo se parecen dos nombres.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parecido {
    /// El mismo nombre, normalizado.
    Igual,
    /// A una letra (cambiada, de más o de menos).
    UnaLetra,
}

/// Una letra sin su acento (en minúscula).
fn sin_acento(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    }
}

/// Un texto sin acentos (letra a letra, como [`normal`] pero sin quitar separadores): para buscar un
/// nombre como palabra en un texto.
pub fn sin_acentos(texto: &str) -> String {
    texto.chars().map(|c| c.to_lowercase().next().map_or(c, sin_acento)).collect()
}

/// El nombre para comparar: minúsculas, sin acentos y sin `-`, `_`, espacios ni puntos.
pub fn normal(nombre: &str) -> String {
    nombre.trim().to_lowercase().chars().filter(|c| !matches!(c, '-' | '_' | '.') && !c.is_whitespace()).map(sin_acento).collect()
}

/// Si dos textos (ya normalizados) están exactamente a una letra: una cambiada, una de más o una de
/// menos. Iguales no cuentan.
pub fn a_una_letra_normal(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (corto, largo) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    match largo.len() - corto.len() {
        0 => corto.iter().zip(largo.iter()).filter(|(x, y)| x != y).count() == 1,
        1 => {
            // La primera diferencia es la letra de más: el resto tiene que coincidir.
            let i = corto.iter().zip(largo.iter()).position(|(x, y)| x != y).unwrap_or(corto.len());
            corto[i..] == largo[i + 1..]
        }
        _ => false,
    }
}

/// Cómo se parecen dos nombres de proyecto (`None`: nada).
pub fn parecido(a: &str, b: &str) -> Option<Parecido> {
    let (na, nb) = (normal(a), normal(b));
    if na.is_empty() || nb.is_empty() {
        return None;
    }
    if na == nb {
        return Some(Parecido::Igual);
    }
    (na.chars().count().min(nb.chars().count()) >= LETRAS_MIN && a_una_letra_normal(&na, &nb)).then_some(Parecido::UnaLetra)
}

// --- El recall ----------------------------------------------------------------------------------

/// Los nombres de los proyectos con memorias que se ven desde `ambito`, normalizados, con las palabras
/// de cada uno (en minúsculas y sin acentos, como los términos del recall).
fn nombres_con_memorias(db: &Connection, ambito: &Ambito) -> Result<Vec<(String, Vec<String>)>> {
    let mut q = db.prepare(&format!(
        "SELECT name FROM project p WHERE p.id != 0 AND {} AND EXISTS (SELECT 1 FROM memory m WHERE m.project_id = p.id)",
        proyectos_visibles(ambito)
    ))?;
    let nombres: Vec<String> = q.query_map([], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut v: Vec<(String, Vec<String>)> = nombres
        .iter()
        .map(|n| {
            let partes = n
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|p| !p.is_empty())
                .map(|p| p.chars().map(sin_acento).collect())
                .collect();
            (normal(n), partes)
        })
        .filter(|(n, _)| !n.is_empty())
        .collect();
    v.sort();
    v.dedup();
    Ok(v)
}

/// Un término del mensaje que es un nombre de proyecto mal escrito: las palabras del nombre bueno.
/// Solo se miran los términos que no están en ninguna memoria (`en_ninguna[i]`), de `LETRAS_MIN_RECALL`
/// letras o más, a una letra de un único proyecto de también `LETRAS_MIN_RECALL` letras que se vea desde
/// `ambito` (nunca el de otro cliente), sin ser una flexión ([`es_flexion`]); o iguales normalizados a
/// uno que se escribe con separadores («socialpublish» → «social publish»). Devuelve (índice del
/// término, palabras).
pub fn mal_escritos(db: &Connection, ambito: &Ambito, terminos: &[String], en_ninguna: &[bool]) -> Result<Vec<(usize, Vec<String>)>> {
    let dudosos: Vec<usize> = (0..terminos.len()).filter(|&i| en_ninguna.get(i).copied().unwrap_or(false) && terminos[i].chars().count() >= LETRAS_MIN_RECALL).collect();
    if dudosos.is_empty() {
        return Ok(Vec::new());
    }
    let nombres = nombres_con_memorias(db, ambito)?;
    let mut r = Vec::new();
    for i in dudosos {
        let t = normal(&terminos[i]);
        let encajan: Vec<&(String, Vec<String>)> = nombres
            .iter()
            .filter(|(n, partes)| {
                (*n == t && partes.len() > 1) || (n.chars().count() >= LETRAS_MIN_RECALL && a_una_letra_normal(n, &t) && !es_flexion(&t, n))
            })
            .collect();
        if let [(_, partes)] = encajan.as_slice() {
            r.push((i, partes.clone()));
        }
    }
    Ok(r)
}

// --- La revisión: proyectos duplicados ----------------------------------------------------------

/// El mismo nombre normalizado.
pub const MISMO_NOMBRE: &str = "same_name";
/// A una letra, con otra señal.
pub const UNA_LETRA: &str = "one_letter";
/// Una carpeta que ya no existe y otro proyecto que la continúa.
pub const CONTINUA: &str = "continued";
/// Un «proyecto» en una carpeta temporal o de subidas.
pub const TEMPORAL: &str = "temp_folder";

/// Un proyecto que tiene «muy pocas» memorias.
const POCAS: i64 = 3;

/// Una propuesta de la revisión sobre los proyectos: juntar dos, o sacar las memorias de una carpeta
/// que no es un proyecto. Nunca se hace sola: el agente la propone con la orden ya escrita.
#[derive(Clone, Debug, Serialize)]
pub struct Propuesta {
    pub kind: &'static str,
    /// El proyecto que desaparecería al juntar (o el de la carpeta temporal).
    pub project: String,
    pub path: String,
    pub memories: i64,
    /// El que se quedaría (vacío en una carpeta temporal).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub other: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub other_path: String,
    /// Por qué, en inglés y corto.
    pub detail: String,
    /// La orden que lo haría (si el usuario dice que sí).
    pub command: String,
    /// La orden para que no vuelva a proponerlo.
    pub dismiss: String,
}

/// Dónde se apuntan las parejas que el usuario dijo que son distintas (`omoi projects distinct`):
/// un JSON con «claveA|claveB» (ordenadas; claveB vacía: la carpeta temporal es un proyecto de verdad).
const CLAVE_DISTINTOS: &str = "proyectos_distintos";

fn pareja(a: &str, b: &str) -> String {
    if b.is_empty() || a <= b { format!("{a}|{b}") } else { format!("{b}|{a}") }
}

fn distintos(db: &Connection) -> Result<BTreeSet<String>> {
    let v: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [CLAVE_DISTINTOS], |f| f.get(0)).optional()?;
    Ok(v.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default())
}

/// `omoi projects distinct A [B]`: la revisión deja de proponer juntar A y B (o, sin B, de tratar la
/// carpeta temporal de A como si no fuera un proyecto). Es de este equipo: no viaja.
pub fn marcar_distintos(db: &Connection, a: i64, b: Option<i64>) -> Result<()> {
    let ka = proyecto::clave_de(db, a)?;
    anyhow::ensure!(!ka.is_empty(), tr!("global memories are not a project", "las memorias globales no son un proyecto"));
    let kb = match b {
        Some(b) => {
            anyhow::ensure!(b != a, tr!("it's the same project", "es el mismo proyecto"));
            proyecto::clave_de(db, b)?
        }
        None => String::new(),
    };
    let mut v = distintos(db)?;
    v.insert(pareja(&ka, &kb));
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![CLAVE_DISTINTOS, serde_json::to_string(&v)?])?;
    Ok(())
}

/// Si la carpeta es temporal o de subidas: `.claude/uploads/<…>`, la carpeta temporal del sistema
/// (`%TEMP%`, `$TMPDIR`), `AppData\Local\Temp` o `/tmp`.
pub fn es_temporal(ruta: &str) -> bool {
    if ruta.is_empty() || ruta.starts_with('[') {
        return false;
    }
    let r = ruta.replace('\\', "/").to_lowercase();
    if r.contains("/.claude/uploads/") || r.contains("/appdata/local/temp/") || r.starts_with("/tmp/") || r.starts_with("/var/tmp/") {
        return true;
    }
    // La carpeta temporal tal cual y en forma canónica (con `TEMP` en nombre corto, `CLAUDE~1`, las
    // rutas de los proyectos llegan en el largo).
    let tmp = std::env::temp_dir();
    let canonica = proyecto::canonica(&tmp).map(|p| proyecto::texto(&p)).unwrap_or_default();
    [tmp.to_string_lossy().into_owned(), canonica].iter().any(|t| {
        let t = t.replace('\\', "/").to_lowercase();
        let t = t.trim_end_matches('/');
        !t.is_empty() && r.starts_with(&format!("{t}/"))
    })
}

/// Cómo se nombra un proyecto en una orden: su nombre si `proyecto::buscar` lo encuentra solo a él,
/// y si no, su carpeta (entre comillas si lleva espacios).
fn en_orden(db: &Connection, p: &Proyecto) -> String {
    let t = if proyecto::buscar(db, &p.name).ok() == Some(p.id) { p.name.clone() } else { p.path.clone() };
    if t.contains([' ', '\'']) || t.is_empty() { format!("\"{t}\"") } else { t }
}

/// El `origin` de git de un proyecto: el de su clave `git:`, o el de alguna de sus carpetas de aquí.
fn origen_de(db: &Connection, p: &Proyecto) -> Result<String> {
    if let Some(o) = p.key.strip_prefix("git:") {
        return Ok(o.to_string());
    }
    Ok(db
        .query_row("SELECT origin FROM project_path WHERE project_id = ?1 AND is_foreign = 0 AND origin != '' LIMIT 1", [p.id], |f| f.get(0))
        .optional()?
        .unwrap_or_default())
}

/// Cuándo se creó cada proyecto.
fn creados(db: &Connection) -> Result<HashMap<i64, String>> {
    let mut q = db.prepare("SELECT id, created_at FROM project WHERE id != 0")?;
    Ok(q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
}

/// Cuántas memorias de cada proyecto nombran la palabra (por el índice de texto).
fn mencionan(db: &Connection, palabra: &str) -> Result<HashMap<i64, i64>> {
    let mut q = db.prepare("SELECT m.project_id, count(*) FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid WHERE memory_fts MATCH ?1 GROUP BY m.project_id")?;
    let consulta = format!("\"{}\"", palabra.replace('"', ""));
    Ok(q.query_map([consulta], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>().unwrap_or_default())
}

/// Lo que la revisión propone sobre los proyectos que se ven desde un ámbito (los de su espacio; desde
/// una carpeta general, todos): juntar los duplicados y sacar las memorias de las carpetas temporales.
/// Señales, de más a menos segura:
/// - el mismo nombre normalizado, siempre (en el mismo espacio);
/// - a una letra, solo con otra: una de las carpetas ya no existe, uno tiene muy pocas memorias y es
///   más nuevo, o los dos tienen el mismo `origin` de git;
/// - una carpeta que ya no existe y otro proyecto, más nuevo, cuyas memorias la nombran (`Narrador` →
///   `Aloud`);
/// - un «proyecto» en una carpeta temporal o de subidas (`.claude\uploads\<uuid>`, `%TEMP%`).
///
/// Nada de parecidos por nombre a secas. Solo proyectos con memorias; como temporales, solo los que
/// no tienen `origin` (una clave `git:` es un repositorio de verdad, esté donde esté).
pub fn propuestas(db: &Connection, ambito: &Ambito) -> Result<Vec<Propuesta>> {
    propuestas_con(db, ambito, &es_temporal)
}

/// `propuestas` con los proyectos que ya tiene quien llama: `proyecto::lista(db, None, filtro_de(ambito),
/// usize::MAX)` (`omoi review` los lista antes, y cada lista recorre la tabla de memorias).
pub fn propuestas_de(db: &Connection, todos: &[Proyecto]) -> Result<Vec<Propuesta>> {
    propuestas_entre(db, todos, &es_temporal)
}

/// El filtro de espacio con el que `propuestas` lista los proyectos de `ambito`.
pub fn filtro_de(ambito: &Ambito) -> Option<Option<i64>> {
    if matches!(ambito, Ambito::Todos) { None } else { Some(ambito.espacio()) }
}

/// `propuestas`, con otra forma de decir qué carpeta es temporal (los tests viven en `%TEMP%`).
pub fn propuestas_con(db: &Connection, ambito: &Ambito, es_temporal: &dyn Fn(&str) -> bool) -> Result<Vec<Propuesta>> {
    propuestas_entre(db, &proyecto::lista(db, None, filtro_de(ambito), usize::MAX)?, es_temporal)
}

fn propuestas_entre(db: &Connection, todos: &[Proyecto], es_temporal: &dyn Fn(&str) -> bool) -> Result<Vec<Propuesta>> {
    let fuera = distintos(db)?;
    let creado = creados(db)?;
    let espacio: HashMap<i64, Option<i64>> = todos.iter().map(|p| Ok((p.id, proyecto::espacio_de(db, p.id)?))).collect::<Result<_>>()?;
    let mut r: Vec<Propuesta> = Vec::new();
    let mut ya: BTreeSet<(i64, i64)> = BTreeSet::new();
    let temporales: BTreeSet<i64> = todos.iter().filter(|p| p.key.starts_with("dir:") && es_temporal(&p.path)).map(|p| p.id).collect();
    let mut proponer = |db: &Connection, kind: &'static str, de: &Proyecto, en: &Proyecto, detalle: String, r: &mut Vec<Propuesta>| {
        let par = (de.id.min(en.id), de.id.max(en.id));
        if !ya.insert(par) || fuera.contains(&pareja(&de.key, &en.key)) {
            return;
        }
        let (a, b) = (en_orden(db, de), en_orden(db, en));
        r.push(Propuesta {
            kind,
            project: de.name.clone(),
            path: de.path.clone(),
            memories: de.memories,
            other: en.name.clone(),
            other_path: en.path.clone(),
            detail: detalle,
            command: format!("omoi projects merge {a} {b}"),
            dismiss: format!("omoi projects distinct {a} {b}"),
        });
    };
    // Cuál desaparece al juntar: el que ya no está, si no el de menos memorias, si no el más nuevo.
    let orden = |a: &Proyecto, b: &Proyecto| -> bool {
        let clave = |p: &Proyecto| (!p.gone, p.memories, std::cmp::Reverse(creado.get(&p.id).cloned().unwrap_or_default()));
        clave(a) < clave(b)
    };
    for (i, a) in todos.iter().enumerate() {
        for b in &todos[i + 1..] {
            if temporales.contains(&a.id) || temporales.contains(&b.id) || espacio.get(&a.id) != espacio.get(&b.id) {
                continue;
            }
            let (de, en) = if orden(a, b) { (a, b) } else { (b, a) };
            match parecido(&a.name, &b.name) {
                Some(Parecido::Igual) => {
                    proponer(db, MISMO_NOMBRE, de, en, "same name (ignoring case, accents and separators)".into(), &mut r);
                }
                Some(Parecido::UnaLetra) => {
                    let (oa, ob) = (origen_de(db, a)?, origen_de(db, b)?);
                    let nuevo_y_pequeno = |x: &Proyecto, y: &Proyecto| x.memories <= POCAS && x.memories < y.memories && creado.get(&x.id) > creado.get(&y.id);
                    let senal = if a.gone || b.gone {
                        Some("one letter apart, and one folder no longer exists".to_string())
                    } else if !oa.is_empty() && oa == ob {
                        Some(format!("one letter apart, and the same git origin ({oa})"))
                    } else if nuevo_y_pequeno(a, b) || nuevo_y_pequeno(b, a) {
                        Some("one letter apart, and the newer one has very few memories".to_string())
                    } else {
                        None
                    };
                    if let Some(s) = senal {
                        proponer(db, UNA_LETRA, de, en, s, &mut r);
                    }
                }
                None => {}
            }
        }
    }
    // Una carpeta que ya no existe, continuada por otro proyecto más nuevo que la nombra.
    for viejo in todos.iter().filter(|p| p.gone && !temporales.contains(&p.id)) {
        let palabra = normal(&viejo.name);
        if palabra.chars().count() < LETRAS_MIN_CONTINUA || !palabra.chars().all(char::is_alphanumeric) {
            continue;
        }
        let nombran = mencionan(db, &palabra)?;
        let candidatos: Vec<&Proyecto> = todos
            .iter()
            .filter(|p| {
                p.id != viejo.id
                    && !p.gone
                    && !temporales.contains(&p.id)
                    && espacio.get(&p.id) == espacio.get(&viejo.id)
                    && nombran.get(&p.id).copied().unwrap_or(0) > 0
                    && creado.get(&p.id) > creado.get(&viejo.id)
            })
            .collect();
        if let [nuevo] = candidatos.as_slice() {
            proponer(db, CONTINUA, viejo, nuevo, format!("its folder no longer exists, and {} (newer) mentions it", nuevo.name), &mut r);
        }
    }
    // Las carpetas temporales o de subidas.
    for p in todos.iter().filter(|p| temporales.contains(&p.id)) {
        if fuera.contains(&pareja(&p.key, "")) {
            continue;
        }
        let a = en_orden(db, p);
        r.push(Propuesta {
            kind: TEMPORAL,
            project: p.name.clone(),
            path: p.path.clone(),
            memories: p.memories,
            other: String::new(),
            other_path: String::new(),
            detail: format!(
                "a temporary or uploads folder, not a project: make its {} global (`omoi update <id> --global`) or move them to their project (`omoi projects merge {a} <project>`)",
                if p.memories == 1 { "memory".to_string() } else { format!("{} memories", p.memories) }
            ),
            command: format!("omoi recent --project {a}"),
            dismiss: format!("omoi projects distinct {a}"),
        });
    }
    Ok(r)
}

/// Dónde se apunta el último aviso de proyectos de cada ámbito: `día`.
fn clave_aviso(ambito: &Ambito) -> String {
    let e = match ambito {
        Ambito::Todos => "all".to_string(),
        a => format!("{:?}", a.espacio()),
    };
    format!("revision_proyectos:{e}")
}

/// Al empezar la sesión, una vez al día por ámbito: el párrafo para el agente con lo que se propone
/// sobre los proyectos (`None` si no hay nada o ya salió hoy). Se calcula solo una vez al día.
pub fn aviso_al_empezar(db: &Connection, ambito: &Ambito) -> Result<Option<String>> {
    let hoy: String = db.query_row("SELECT date('now', 'localtime')", [], |f| f.get(0))?;
    let k = clave_aviso(ambito);
    let antes: Option<String> = db.query_row("SELECT v FROM meta WHERE k = ?1", [&k], |f| f.get(0)).optional()?;
    if antes.as_deref() == Some(hoy.as_str()) {
        return Ok(None);
    }
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES (?1, ?2)", params![k, hoy])?;
    let v = propuestas(db, ambito)?;
    Ok((!v.is_empty()).then(|| texto_aviso(&v)))
}

/// El párrafo para el agente, en inglés como el resto del recall.
pub fn texto_aviso(v: &[Propuesta]) -> String {
    let mut s = String::from(
        "Projects review: these look like duplicates or not real projects. Ask the user, one by one, with the command already written; run it only if they say yes (a merge reaches every synced computer). If they say no, run the dismiss command.\n",
    );
    for p in v {
        let quien = if p.other.is_empty() { format!("{} ({})", p.project, p.path) } else { format!("{} ({}) → {} ({})", p.project, p.path, p.other, p.other_path) };
        let (si, no) = if p.other.is_empty() { ("its memories", "if it is a real project") } else { ("merge", "if not") };
        s.push_str(&format!("- {quien}: {}. {si}: `{}` · {no}: `{}`\n", p.detail, p.command, p.dismiss));
    }
    s.pop();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_iguales_normalizados() {
        assert_eq!(normal("Social Publish"), "socialpublish");
        assert_eq!(parecido("social-publish", "Social_Publish"), Some(Parecido::Igual));
        assert_eq!(parecido("Crónicas", "cronicas"), Some(Parecido::Igual));
        assert_eq!(parecido("marcmayol.github.io", "marcmayol-github-io"), Some(Parecido::Igual));
        assert_eq!(parecido("Omoikane", "omoikane"), Some(Parecido::Igual));
    }

    #[test]
    fn a_una_letra() {
        assert_eq!(parecido("Omoikane", "Omoikae"), Some(Parecido::UnaLetra), "una de menos");
        assert_eq!(parecido("Omoikane", "Onmoikane"), Some(Parecido::UnaLetra), "una de más");
        assert_eq!(parecido("Tokimichi", "tokimicho"), Some(Parecido::UnaLetra), "una cambiada");
        assert_eq!(parecido("Troba", "trova"), None, "con menos de 6 letras, no");
        assert_eq!(parecido("Tokimichi", "tokimichi-web"), None, "un nombre dentro de otro no es parecido");
        assert_eq!(parecido("Omoikane", "Omokiane"), None, "dos letras cambiadas de sitio son dos");
        assert!(!a_una_letra_normal("abc", "abc"));
        assert!(a_una_letra_normal("abcdef", "abcdefg"));
        assert!(a_una_letra_normal("xabcdef", "abcdef"));
    }

    #[test]
    fn carpetas_temporales() {
        assert!(es_temporal(r"C:\Users\x\.claude\uploads\d35c0488-7e14-4d1e-ac3b-b60a8d6d8f39"));
        assert!(es_temporal(r"C:\Users\x\AppData\Local\Temp\tmp.abc"));
        assert!(es_temporal("/tmp/x"));
        assert!(!es_temporal(r"D:\Omoikane"));
        assert!(!es_temporal(""));
        let t = std::env::temp_dir().join("algo");
        assert!(es_temporal(&t.to_string_lossy()));
        // Y como llega la ruta de un proyecto: canónica (en Windows, con el nombre largo).
        let d = crate::comun::Temporal::nueva("temporal");
        assert!(es_temporal(&proyecto::de(&d).unwrap()));
    }
}
