//! Búsqueda: palabras (FTS5 con BM25) y, como rescate, significado (vectores centrados).

use crate::embeber::Modelo;
use crate::memoria::{AHORA, Ambito, COLUMNAS, Memoria, Tipo, de_fila};
use crate::vectores;
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

/// Candidatos que se piden a cada lista antes de filtrar por etiquetas y fusionar.
const CANDIDATOS: usize = 40;
/// La k de Reciprocal Rank Fusion (la del artículo original).
const RRF_K: f64 = 60.0;
/// Pesos de BM25 por columna: título, contenido, etiquetas, rutas.
const PESOS: &str = "3.0, 1.0, 2.0, 1.5";
const BOOST_FIJADA: f64 = 1.5;
const BOOST_RECENCIA: f64 = 0.3;
const DIAS_RECENCIA: f64 = 90.0;

#[derive(Clone, Debug)]
pub struct Opciones {
    pub ambito: Ambito,
    pub kind: Option<Tipo>,
    /// Si hay, la memoria tiene que llevarlas todas.
    pub tags: Vec<String>,
    pub limite: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Resultado {
    #[serde(flatten)]
    pub memoria: Memoria,
    pub score: f64,
    /// Coseno con la consulta, si salió por significado.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// Cuántas palabras distintas de la consulta tiene la memoria (0 si solo salió por significado).
    pub matched_words: usize,
}

/// Palabras que casi todo texto contiene y no ayudan a encontrar nada (castellano, inglés, catalán).
/// Ya sin tildes, como las compara el índice.
const VACIAS: &[&str] = &[
    "a", "al", "algo", "como", "con", "de", "del", "el", "ella", "en", "es", "esa", "ese", "eso", "esta", "este", "esto",
    "la", "las", "le", "les", "lo", "los", "mas", "me", "mi", "muy", "no", "o", "para", "pero", "por", "que", "se",
    "si", "sin", "su", "sus", "te", "tu", "un", "una", "uno", "y", "ya", "an", "and", "are", "as", "at", "be", "but",
    "by", "for", "from", "has", "have", "how", "i", "if", "in", "is", "it", "its", "of", "on", "or", "that", "the",
    "this", "to", "was", "what", "when", "why", "with", "amb", "els", "per", "dels", "aixo", "pel",
];

fn sin_tildes(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            c => c,
        })
        .collect()
}

/// Convierte lo que escribe una persona en una consulta FTS5 segura: cada palabra entre comillas
/// (nada de sintaxis FTS5 que pueda romper la consulta), unidas con OR para que BM25 ordene por
/// cuántas coinciden. A partir de 3 letras va dos veces, exacta y como prefijo: «memoria» encuentra
/// «memorias», pero la memoria que dice «memoria» suma las dos y queda delante (si no, buscar
/// «v20» podría sacar antes «v200»). `None` si no queda ninguna palabra.
pub fn consulta_fts(texto: &str) -> Option<String> {
    let terminos = terminos(texto);
    let mut q: Vec<String> = terminos
        .iter()
        .map(|p| if p.chars().count() >= 3 { format!("\"{p}\" OR \"{p}\"*") } else { format!("\"{p}\"") })
        .collect();
    q.dedup();
    (!q.is_empty()).then(|| q.join(" OR "))
}

/// Las palabras de la consulta que cuentan, en minúsculas.
pub fn terminos(texto: &str) -> Vec<String> {
    let palabras: Vec<String> =
        texto.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(|p| p.to_lowercase()).collect();
    let utiles: Vec<&String> = palabras.iter().filter(|p| !VACIAS.contains(&sin_tildes(p).as_str())).collect();
    // Si todo eran palabras vacías («para qué es esto»), mejor buscarlas que no buscar nada.
    let elegidas: Vec<&String> = if utiles.is_empty() { palabras.iter().collect() } else { utiles };
    elegidas.into_iter().cloned().collect()
}

/// Cuántos de los términos aparecen en la memoria, con la misma regla que el índice: sin tildes y
/// por prefijo a partir de 3 letras.
fn coincidencias(terminos: &[String], m: &Memoria) -> usize {
    let texto = sin_tildes(&format!("{} {} {}", m.title, m.content, m.tags.join(" ")).to_lowercase());
    let palabras: Vec<&str> = texto.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    let esta = |t: &String| {
        let t = sin_tildes(t);
        if t.chars().count() >= 3 { palabras.iter().any(|p| p.starts_with(&t)) } else { palabras.contains(&t.as_str()) }
    };
    let mut vistos: Vec<String> = terminos.iter().map(|t| sin_tildes(t)).collect();
    vistos.sort();
    vistos.dedup();
    vistos.iter().filter(|t| esta(t)).count()
}

/// Qué parte de los términos aparece en la memoria (0 a 1). Desempata cuando BM25 no distingue, que
/// es lo normal con pocas memorias (si una palabra está en la mitad de ellas, su IDF es cero).
fn cobertura(terminos: &[String], m: &Memoria) -> f64 {
    let mut distintos: Vec<String> = terminos.iter().map(|t| sin_tildes(t)).collect();
    distintos.sort();
    distintos.dedup();
    if distintos.is_empty() {
        return 0.0;
    }
    coincidencias(terminos, m) as f64 / distintos.len() as f64
}

/// Búsqueda: manda el orden por palabras (BM25); el vector añade detrás lo que se parece mucho y
/// las palabras no encontraron. Después pesan la fijada y la recencia. Sin modelo, solo palabras.
pub fn buscar(db: &mut Connection, texto: &str, o: &Opciones, modelo: Option<&Modelo>) -> Result<Vec<Resultado>> {
    buscar_con(db, texto, o, modelo, vectores::SIMILITUD_BUSQUEDA, FUSION)
}

/// Cómo se juntan la lista por palabras y la lista por significado.
#[derive(Clone, Copy, Debug)]
pub enum Fusion {
    /// RRF: cada lista aporta `peso / (k + puesto)`; el vector con el peso dado (las palabras, 1).
    Rrf { peso_vector: f64 },
    /// El orden por palabras manda; el vector solo añade, detrás, lo que las palabras no encontraron.
    Rescate,
    /// Como el rescate, pero los últimos `huecos` puestos se reservan para lo que solo encuentra el
    /// vector con al menos `desde` de parecido: con el rescate a secas, las palabras casi siempre
    /// llenan el límite y una paráfrasis que solo ve el vector no entraba nunca. No cambia los
    /// primeros puestos.
    Reserva { huecos: usize, desde: f32 },
    /// Puntuación por palabras (BM25 + cobertura, dividida por la de la mejor) más `peso` por cada
    /// punto de similitud por encima de `desde`: el vector reordena, pero solo cuando se parece de
    /// verdad. Lo que solo trae el vector entra si pasa la similitud mínima, como en el rescate.
    Mezcla { peso: f64, desde: f32 },
}

/// Rescate y no RRF: con 127 notas reales, RRF bajaba el primer acierto del 98 % (solo palabras)
/// al 84 % (peso 1) o al 90 % (peso 0,5); el rescate lo deja en el 98 %. Con un hueco reservado al
/// final para lo que el vector ve muy parecido (desde 0,4), una paráfrasis más de 70 entra en los 5
/// primeros, sin cambiar nada más en 310 preguntas a ciegas.
const FUSION: Fusion = Fusion::Reserva { huecos: 1, desde: 0.4 };

/// Solo por palabras y sin contar los resultados como recuperados: para quien filtra después (el
/// recall), que apunta solo lo que de verdad usa.
pub fn buscar_sin_apuntar(db: &mut Connection, texto: &str, o: &Opciones) -> Result<Vec<Resultado>> {
    buscar_base(db, texto, o, None, vectores::SIMILITUD_BUSQUEDA, FUSION, false)
}

/// `buscar` con otra similitud mínima y otra fusión (para calibrarlas).
/// Como `buscar`, sin contar los resultados como recuperados: para mirar (el visor).
pub fn buscar_sin_apuntar_con(db: &mut Connection, texto: &str, o: &Opciones, modelo: Option<&Modelo>) -> Result<Vec<Resultado>> {
    buscar_base(db, texto, o, modelo, vectores::SIMILITUD_BUSQUEDA, FUSION, false)
}

pub fn buscar_con(
    db: &mut Connection,
    texto: &str,
    o: &Opciones,
    modelo: Option<&Modelo>,
    similitud_minima: f32,
    fusion_elegida: Fusion,
) -> Result<Vec<Resultado>> {
    buscar_base(db, texto, o, modelo, similitud_minima, fusion_elegida, true)
}

fn buscar_base(
    db: &mut Connection,
    texto: &str,
    o: &Opciones,
    modelo: Option<&Modelo>,
    similitud_minima: f32,
    fusion_elegida: Fusion,
    apuntar: bool,
) -> Result<Vec<Resultado>> {
    let tags: Vec<String> = o.tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).collect();
    let con_tags = |m: &Memoria| tags.iter().all(|t| m.tags.contains(t));

    let por_palabras: Vec<(Memoria, usize, f64)> = palabras(db, texto, o)?.into_iter().filter(|(m, _, _)| con_tags(m)).collect();
    let por_significado: Vec<(Memoria, f32)> = match modelo {
        Some(modelo) if !texto.trim().is_empty() => vectores::parecidas(
            db,
            modelo,
            texto,
            o.ambito.filtro(),
            o.kind,
            // Se piden todas: el umbral solo se aplica a lo que no sale también por palabras.
            -1.0,
            CANDIDATOS.max(o.limite),
        )?
        .into_iter()
        .filter(|(m, _)| con_tags(m))
        .collect(),
        _ => Vec::new(),
    };

    // RRF: cada lista aporta 1 / (k + puesto). Lo que sale en las dos, suma las dos.
    let mut fusion: Vec<Resultado> = Vec::new();
    let mut sumar = |m: &Memoria, puesto: usize, sim: Option<f32>, peso: f64, n: usize| {
        let aporte = peso / (RRF_K + puesto as f64 + 1.0);
        match fusion.iter_mut().find(|r| r.memoria.id == m.id) {
            Some(r) => {
                r.score += aporte;
                r.similarity = r.similarity.or(sim);
            }
            None => fusion.push(Resultado { memoria: m.clone(), score: aporte, similarity: sim, matched_words: n }),
        }
    };
    if let Fusion::Mezcla { peso, desde } = fusion_elegida {
        let mejor = por_palabras.iter().map(|x| x.2).fold(f64::MIN, f64::max).max(1e-9);
        let mut v: Vec<Resultado> = por_palabras
            .iter()
            .map(|(m, n, r)| Resultado { memoria: m.clone(), score: r / mejor, similarity: None, matched_words: *n })
            .collect();
        for (m, s) in &por_significado {
            let extra = peso * f64::from((*s - desde).max(0.0));
            match v.iter_mut().find(|r| r.memoria.id == m.id) {
                Some(r) => {
                    r.score += extra;
                    r.similarity = Some(*s);
                }
                None if *s >= similitud_minima => {
                    v.push(Resultado { memoria: m.clone(), score: extra, similarity: Some(*s), matched_words: 0 })
                }
                None => {}
            }
        }
        return ordenar(db, v, o, apuntar);
    }
    for (i, (m, n, _)) in por_palabras.iter().enumerate() {
        sumar(m, i, None, 1.0, *n);
    }
    // Lo que sale por palabras y por significado suma las dos posiciones. Lo que solo trae el vector
    // entra únicamente si se parece de verdad: con un modelo estático, cualquier texto se parece
    // algo a cualquier otro (sobre todo en el mismo idioma) y sin este filtro todo devuelve ruido.
    let n_palabras = por_palabras.len();
    for (i, (m, s)) in por_significado.iter().enumerate() {
        let tambien_por_palabras = por_palabras.iter().any(|(p, _, _)| p.id == m.id);
        match fusion_elegida {
            Fusion::Rrf { peso_vector } => {
                if *s >= similitud_minima || tambien_por_palabras {
                    sumar(m, i, Some(*s), peso_vector, 0);
                }
            }
            Fusion::Mezcla { .. } => unreachable!(),
            Fusion::Rescate | Fusion::Reserva { .. } => {
                if tambien_por_palabras {
                    sumar(m, 0, Some(*s), 0.0, 0); // solo apunta la similitud, sin mover el orden
                } else if *s >= similitud_minima {
                    sumar(m, n_palabras + i, Some(*s), 1.0, 0);
                }
            }
        }
    }
    if let Fusion::Reserva { huecos, desde } = fusion_elegida {
        // Lo que el vector ve muy parecido (haya salido por palabras más abajo o no), si pasa `desde`.
        let solo_vector: Vec<Resultado> = fusion.iter().filter(|r| r.similarity.is_some_and(|s| s >= desde)).cloned().collect();
        if !solo_vector.is_empty() {
            let mut v = ordenar(db, fusion, &Opciones { limite: usize::MAX, ..o.clone() }, false)?;
            let corte = o.limite.saturating_sub(huecos.min(solo_vector.len()));
            let mut arriba: Vec<Resultado> = v.drain(..corte.min(v.len())).collect();
            let mut extra: Vec<Resultado> = solo_vector.into_iter().filter(|r| !arriba.iter().any(|a| a.memoria.id == r.memoria.id)).collect();
            extra.sort_by(|a, b| b.similarity.unwrap_or(0.0).total_cmp(&a.similarity.unwrap_or(0.0)));
            arriba.extend(extra.into_iter().take(o.limite - arriba.len()));
            for r in v {
                if arriba.len() >= o.limite {
                    break;
                }
                if !arriba.iter().any(|a| a.memoria.id == r.memoria.id) {
                    arriba.push(r);
                }
            }
            if apuntar {
                apuntar_recuperadas(db, arriba.iter().map(|r| r.memoria.id.as_str()))?;
            }
            return Ok(arriba);
        }
    }
    ordenar(db, fusion, o, apuntar)
}

/// Recencia y fijadas, orden final, límite y, si toca, apuntarlas como recuperadas.
fn ordenar(db: &mut Connection, mut fusion: Vec<Resultado>, o: &Opciones, apuntar: bool) -> Result<Vec<Resultado>> {
    let mut dias = db.prepare("SELECT julianday('now') - julianday(?1)")?;
    for r in &mut fusion {
        let d: f64 = dias.query_row([&r.memoria.updated_at], |f| f.get(0)).unwrap_or(0.0);
        let recencia = 1.0 / (1.0 + d.max(0.0) / DIAS_RECENCIA);
        let fijada = if r.memoria.pinned { BOOST_FIJADA } else { 1.0 };
        r.score *= fijada * (1.0 + BOOST_RECENCIA * recencia);
    }
    drop(dias);
    fusion.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| b.memoria.id.cmp(&a.memoria.id)));
    fusion.truncate(o.limite);
    if apuntar {
        apuntar_recuperadas(db, fusion.iter().map(|r| r.memoria.id.as_str()))?;
    }
    Ok(fusion)
}

/// Candidatos por palabras, del mejor al peor (BM25 más la cobertura, que desempata cuando BM25 no
/// distingue, como pasa con pocas memorias), con cuántas palabras de la consulta tiene cada uno.
fn palabras(db: &Connection, texto: &str, o: &Opciones) -> Result<Vec<(Memoria, usize, f64)>> {
    let Some(consulta) = consulta_fts(texto) else { return Ok(Vec::new()) };
    let terminos = terminos(texto);
    let columnas = COLUMNAS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ");
    let mut q = db.prepare(&format!(
        "SELECT {columnas}, -bm25(memory_fts, {PESOS}) AS relevancia
         FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1
           AND (?2 IS NULL OR m.project = ?2 OR m.project = '')
           AND (?3 IS NULL OR m.kind = ?3)
         ORDER BY bm25(memory_fts, {PESOS}) LIMIT ?4"
    ))?;
    let ncol = COLUMNAS.split(", ").count();
    let mut v: Vec<(Memoria, f64)> = q
        .query_map(params![consulta, o.ambito.filtro(), o.kind.map(Tipo::as_str), CANDIDATOS.max(o.limite) as i64], |f| {
            Ok((de_fila(f)?, f.get::<_, f64>(ncol)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (m, r) in &mut v {
        *r += cobertura(&terminos, m);
    }
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.id.cmp(&a.0.id)));
    Ok(v.into_iter().map(|(m, r)| { let n = coincidencias(&terminos, &m); (m, n, r) }).collect())
}

/// Lo que devuelven la búsqueda y el recall cuenta como recuperado: sirve para ordenar y, más
/// adelante, para proponer borrar lo que nunca sale.
pub fn apuntar_recuperadas<'a>(db: &mut Connection, ids: impl Iterator<Item = &'a str>) -> Result<()> {
    let ids: Vec<&str> = ids.collect();
    if ids.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    {
        let mut u = tx.prepare(&format!(
            "UPDATE memory SET recalled_at = {AHORA}, recall_count = recall_count + 1 WHERE id = ?1"
        ))?;
        for id in ids {
            u.execute([id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consulta_segura() {
        assert_eq!(
            consulta_fts("fallo de autenticación").unwrap(),
            r#""fallo" OR "fallo"* OR "autenticación" OR "autenticación"*"#
        );
        // La sintaxis de FTS5 no pasa: comillas, NEAR, paréntesis, dos puntos…
        // «or» e «y» son palabras vacías: se quitan.
        assert_eq!(
            consulta_fts(r#"title:"x" OR (NEAR y) z"#).unwrap(),
            r#""title" OR "title"* OR "x" OR "near" OR "near"* OR "z""#
        );
        assert_eq!(
            consulta_fts("para qué es esto").unwrap(),
            r#""para" OR "para"* OR "qué" OR "qué"* OR "es" OR "esto" OR "esto"*"#
        );
        assert!(consulta_fts(" ¿?¡! ").is_none());
    }

    #[test]
    fn cobertura_sin_tildes_y_por_prefijo() {
        let m = Memoria {
            id: "X".into(),
            project: String::new(),
            kind: Tipo::Gotcha,
            title: "Bug en auth".into(),
            content: "Fallo de autenticación al renovar tokens".into(),
            paths: vec![],
            tags: vec!["login".into()],
            pinned: false,
            source: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            recalled_at: None,
            recall_count: 0,
        };
        assert_eq!(cobertura(&terminos("fallo autenticacion"), &m), 1.0);
        assert_eq!(cobertura(&terminos("token caducado"), &m), 0.5);
        assert_eq!(cobertura(&terminos("receta"), &m), 0.0);
    }
}
