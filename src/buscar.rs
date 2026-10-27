//! Búsqueda. Fase 1: solo palabras (FTS5 con BM25). En la fase 2 se le suma el vector con RRF.

use crate::memoria::{AHORA, Ambito, COLUMNAS, Memoria, Tipo, de_fila};
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

/// Candidatos que se piden al índice antes de filtrar por etiquetas y reordenar.
const CANDIDATOS: usize = 40;
/// Pesos de BM25 por columna: título, contenido, etiquetas.
const PESOS: &str = "3.0, 1.0, 2.0";
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
fn terminos(texto: &str) -> Vec<String> {
    let palabras: Vec<String> =
        texto.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(|p| p.to_lowercase()).collect();
    let utiles: Vec<&String> = palabras.iter().filter(|p| !VACIAS.contains(&sin_tildes(p).as_str())).collect();
    // Si todo eran palabras vacías («para qué es esto»), mejor buscarlas que no buscar nada.
    let elegidas: Vec<&String> = if utiles.is_empty() { palabras.iter().collect() } else { utiles };
    elegidas.into_iter().cloned().collect()
}

/// Qué parte de los términos aparece en la memoria (0 a 1), con la misma regla que el índice:
/// sin tildes y por prefijo a partir de 3 letras. Desempata cuando BM25 no distingue, que es lo
/// normal con pocas memorias (si una palabra está en la mitad de ellas, su IDF es cero).
fn cobertura(terminos: &[String], m: &Memoria) -> f64 {
    if terminos.is_empty() {
        return 0.0;
    }
    let texto = sin_tildes(&format!("{} {} {}", m.title, m.content, m.tags.join(" ")).to_lowercase());
    let palabras: Vec<&str> = texto.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    let esta = |t: &String| {
        let t = sin_tildes(t);
        if t.chars().count() >= 3 { palabras.iter().any(|p| p.starts_with(&t)) } else { palabras.contains(&t.as_str()) }
    };
    terminos.iter().filter(|t| esta(t)).count() as f64 / terminos.len() as f64
}

pub fn buscar(db: &mut Connection, texto: &str, o: &Opciones) -> Result<Vec<Resultado>> {
    let Some(consulta) = consulta_fts(texto) else { return Ok(Vec::new()) };
    let terminos = terminos(texto);
    let columnas = COLUMNAS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ");
    let mut q = db.prepare(&format!(
        "SELECT {columnas}, -bm25(memory_fts, {PESOS}) AS relevancia,
                julianday('now') - julianday(m.updated_at) AS dias
         FROM memory_fts JOIN memory m ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1
           AND (?2 IS NULL OR m.project = ?2 OR m.project = '')
           AND (?3 IS NULL OR m.kind = ?3)
         ORDER BY bm25(memory_fts, {PESOS}) LIMIT ?4"
    ))?;
    let ncol = COLUMNAS.split(", ").count();
    let filas = q.query_map(
        params![consulta, o.ambito.filtro(), o.kind.map(Tipo::as_str), CANDIDATOS.max(o.limite) as i64],
        |f| Ok((de_fila(f)?, f.get::<_, f64>(ncol)?, f.get::<_, f64>(ncol + 1)?)),
    )?;
    let tags: Vec<String> = o.tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).collect();
    let mut res: Vec<Resultado> = Vec::new();
    for fila in filas {
        let (memoria, relevancia, dias) = fila?;
        if !tags.iter().all(|t| memoria.tags.contains(t)) {
            continue;
        }
        let recencia = 1.0 / (1.0 + dias.max(0.0) / DIAS_RECENCIA);
        let fijada = if memoria.pinned { BOOST_FIJADA } else { 1.0 };
        let score = (relevancia + cobertura(&terminos, &memoria)) * fijada * (1.0 + BOOST_RECENCIA * recencia);
        res.push(Resultado { memoria, score });
    }
    drop(q);
    res.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| b.memoria.id.cmp(&a.memoria.id)));
    res.truncate(o.limite);
    apuntar_recuperadas(db, res.iter().map(|r| r.memoria.id.as_str()))?;
    Ok(res)
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
