//! Search: words (FTS5 with BM25) and, as a fallback, meaning (centered vectors).

use crate::embeber::Model;
use crate::memoria::{NOW, Scope, COLUMNS, Memory, Kind, from_row, scope_index_hint, sees_all};
use crate::vectores;
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

/// Candidates requested from each list before filtering by tags and fusing.
const CANDIDATES: usize = 40;
/// The k of Reciprocal Rank Fusion (the one from the original paper).
const RRF_K: f64 = 60.0;
/// BM25 weights per column: title, content, tags, paths.
const BM25_WEIGHTS: &str = "3.0, 1.0, 2.0, 1.5";
const PINNED_BOOST: f64 = 1.5;
const RECENCY_BOOST: f64 = 0.3;
const RECENCY_DAYS: f64 = 90.0;

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub scope: Scope,
    pub kind: Option<Kind>,
    /// If any, the memory has to carry all of them.
    pub tags: Vec<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub memory: Memory,
    pub score: f64,
    /// Cosine with the query, if it came up by meaning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// How many distinct query words the memory has (0 if it only came up by meaning).
    pub matched_words: usize,
}

/// Words almost every text contains that don't help find anything (Spanish, English, Catalan).
/// Already without accents, as the index compares them.
const STOPWORDS: &[&str] = &[
    "a", "al", "algo", "como", "con", "de", "del", "el", "ella", "en", "es", "esa", "ese", "eso", "esta", "este", "esto",
    "la", "las", "le", "les", "lo", "los", "mas", "me", "mi", "muy", "no", "o", "para", "pero", "por", "que", "se",
    "si", "sin", "su", "sus", "te", "tu", "un", "una", "uno", "y", "ya", "an", "and", "are", "as", "at", "be", "but",
    "by", "for", "from", "has", "have", "how", "i", "if", "in", "is", "it", "its", "of", "on", "or", "that", "the",
    "this", "to", "was", "what", "when", "why", "with", "amb", "els", "per", "dels", "aixo", "pel",
];

pub(crate) fn strip_accents(s: &str) -> String {
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

/// Turns what a person types into a safe FTS5 query: each word in quotes
/// (no FTS5 syntax that could break the query), joined with OR so BM25 ranks by
/// how many match. From 3 letters on it goes twice, exact and as a prefix: «memoria» finds
/// «memorias», but the memory that says «memoria» adds both and comes first (otherwise, searching
/// «v20» could bring «v200» first). `None` if no word is left.
pub fn fts_query(text: &str) -> Option<String> {
    let terms = terms(text);
    let mut q: Vec<String> = terms
        .iter()
        .map(|p| match stem(p) {
            Some(r) => format!("\"{p}\" OR \"{r}\"*"),
            None if p.chars().count() >= 3 => format!("\"{p}\" OR \"{p}\"*"),
            None => format!("\"{p}\""),
        })
        .collect();
    q.dedup();
    (!q.is_empty()).then(|| q.join(" OR "))
}

/// Stem to also search other forms of the word («reinicia» → «reinici»: «reinicios»,
/// «reiniciar»): without the last two letters, in words of at least `STEM_MIN_LEN`. Spanish inflects
/// almost always at the end, and the prefix alone doesn't cover «reinicia» ↔ «reinicios».
pub(crate) fn stem(p: &str) -> Option<String> {
    let min_len: usize = std::env::var("OMOIKANE_CALIBRAR_RAIZ").ok().and_then(|v| v.parse().ok()).unwrap_or(STEM_MIN_LEN);
    let n = p.chars().count();
    (min_len > 0 && n >= min_len && p.chars().all(char::is_alphabetic)).then(|| p.chars().take(n - 2).collect())
}

/// The query words that count, lowercase.
pub fn terms(text: &str) -> Vec<String> {
    let words: Vec<String> =
        text.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(|p| p.to_lowercase()).collect();
    let useful: Vec<&String> = words.iter().filter(|p| !STOPWORDS.contains(&strip_accents(p).as_str())).collect();
    // If they were all stopwords («para qué es esto»), better search for them than search nothing.
    let chosen: Vec<&String> = if useful.is_empty() { words.iter().collect() } else { useful };
    chosen.into_iter().cloned().collect()
}

/// How many of the terms appear in the memory, with the same rule as the index: without accents and
/// by prefix from 3 letters on.
fn match_count(terms: &[String], m: &Memory) -> usize {
    let text = strip_accents(&format!("{} {} {}", m.title, m.content, m.tags.join(" ")).to_lowercase());
    let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    let is_in = |t: &String| {
        let t = strip_accents(t);
        let t = stem(&t).unwrap_or(t);
        if t.chars().count() >= 3 { words.iter().any(|p| p.starts_with(&t)) } else { words.contains(&t.as_str()) }
    };
    let mut seen: Vec<String> = terms.iter().map(|t| strip_accents(t)).collect();
    seen.sort();
    seen.dedup();
    seen.iter().filter(|t| is_in(t)).count()
}

/// Like `coverage`, but each term weighs its IDF in the scope (Lucene, like the recall): a generic
/// word («golpe», «largo») doesn't lift the long note that contains everything.
fn idf_coverage(terms: &[String], m: &Memory, idf: &std::collections::HashMap<String, f64>) -> f64 {
    let text = strip_accents(&format!("{} {} {}", m.title, m.content, m.tags.join(" ")).to_lowercase());
    let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
    let mut seen: Vec<String> = terms.iter().map(|t| strip_accents(t)).collect();
    seen.sort();
    seen.dedup();
    let total: f64 = seen.iter().map(|t| idf.get(t).copied().unwrap_or(0.0)).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let inside: f64 = seen
        .iter()
        .filter(|t| {
            let r = stem(t).unwrap_or_else(|| t.to_string());
            if r.chars().count() >= 3 { words.iter().any(|p| p.starts_with(r.as_str())) } else { words.contains(&r.as_str()) }
        })
        .map(|t| idf.get(t).copied().unwrap_or(0.0))
        .sum();
    inside / total
}

/// Lucene IDF of each term (without accents) in the scope, counting through the index like the search.
fn term_idf(db: &Connection, terms: &[String], o: &SearchOptions) -> Result<std::collections::HashMap<String, f64>> {
    let n: f64 = crate::memoria::count(db, &o.scope)? as f64;
    let mut q = if sees_all(db, &o.scope) {
        db.prepare("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH ?1")?
    } else {
        db.prepare(&format!(
            "SELECT count(*) FROM memory_fts JOIN memory m {} ON m.n = memory_fts.rowid
             WHERE memory_fts MATCH ?1 AND {}",
            scope_index_hint(db),
            o.scope.sql()
        ))?
    };
    let mut v = std::collections::HashMap::new();
    for t in terms {
        let t = strip_accents(t);
        if v.contains_key(&t) {
            continue;
        }
        let query = match stem(&t) {
            Some(r) => format!("\"{r}\"*"),
            None if t.chars().count() >= 3 => format!("\"{t}\"*"),
            None => format!("\"{t}\""),
        };
        let df = q.query_row(params![query], |f| f.get::<_, i64>(0)).unwrap_or(0) as f64;
        let d = df.max(1.0);
        v.insert(t, (1.0 + (n - d + 0.5) / (d + 0.5)).ln());
    }
    Ok(v)
}

/// What fraction of the terms appears in the memory (0 to 1). Breaks ties when BM25 can't tell, which
/// is normal with few memories (if a word is in half of them, its IDF is zero).
fn coverage(terms: &[String], m: &Memory) -> f64 {
    let mut distinct: Vec<String> = terms.iter().map(|t| strip_accents(t)).collect();
    distinct.sort();
    distinct.dedup();
    if distinct.is_empty() {
        return 0.0;
    }
    match_count(terms, m) as f64 / distinct.len() as f64
}

/// Search: the order by words (BM25) rules; the vector adds behind what is very similar and
/// the words didn't find. Then pinned and recency weigh in. Without a model, only words.
pub fn search(db: &mut Connection, text: &str, o: &SearchOptions, model: Option<&Model>) -> Result<Vec<SearchResult>> {
    search_base(db, text, o, model, vectores::SEARCH_SIMILARITY, FUSION, true, true)
}

/// How the list by words and the list by meaning are combined.
#[derive(Clone, Copy, Debug)]
pub enum Fusion {
    /// RRF: each list contributes `weight / (k + rank)`; the vector with the given weight (the words, 1).
    Rrf { vector_weight: f64 },
    /// The order by words rules; the vector only adds, behind, what the words didn't find.
    Fallback,
    /// Like the fallback, but the last `slots` places are reserved for what only the
    /// vector finds with at least `desde` similarity: with the plain fallback, words almost always
    /// fill the limit and a paraphrase that only the vector sees never got in. It doesn't change the
    /// first places.
    Reserve { slots: usize, min_similarity: f32 },
    /// Score by words (BM25 + coverage, divided by the best's) plus `weight` per
    /// similarity point above `desde`: the vector reorders, but only when it is really
    /// similar. What only the vector brings gets in if it passes the minimum similarity, as in the fallback.
    Blend { weight: f64, min_similarity: f32 },
}

/// Fallback and not RRF: with 127 real notes, RRF dropped the first hit from 98 % (words only)
/// to 84 % (weight 1) or 90 % (weight 0.5); the fallback keeps it at 98 %. With one slot reserved at the
/// end for what the vector sees as very similar (from 0.4), one more paraphrase out of 70 gets into the top 5,
/// without changing anything else in 310 blind questions.
const FUSION: Fusion = Fusion::Reserve { slots: 1, min_similarity: 0.4 };

/// From how many letters the stem is also searched (see `stem`); 0, never.
const STEM_MIN_LEN: usize = 6;

/// Only by words and without counting the results as recalled: for whoever filters afterwards (the
/// recall), which records only what it really uses.
pub fn search_without_recording(db: &mut Connection, text: &str, o: &SearchOptions) -> Result<Vec<SearchResult>> {
    search_base(db, text, o, None, vectores::SEARCH_SIMILARITY, FUSION, false, false)
}

/// `search` with another minimum similarity and another fusion (to calibrate them).
/// Like `search`, without counting the results as recalled: for looking (the viewer).
pub fn search_without_recording_with(db: &mut Connection, text: &str, o: &SearchOptions, model: Option<&Model>) -> Result<Vec<SearchResult>> {
    search_base(db, text, o, model, vectores::SEARCH_SIMILARITY, FUSION, false, true)
}

pub fn search_with(
    db: &mut Connection,
    text: &str,
    o: &SearchOptions,
    model: Option<&Model>,
    min_similarity: f32,
    chosen_fusion: Fusion,
) -> Result<Vec<SearchResult>> {
    search_base(db, text, o, model, min_similarity, chosen_fusion, true, false)
}

#[allow(clippy::too_many_arguments)]
fn search_base(
    db: &mut Connection,
    text: &str,
    o: &SearchOptions,
    model: Option<&Model>,
    min_similarity: f32,
    chosen_fusion: Fusion,
    record: bool,
    with_meaning: bool,
) -> Result<Vec<SearchResult>> {
    let tags: Vec<String> = o.tags.iter().map(|t| t.trim().trim_start_matches('#').to_lowercase()).collect();
    let has_tags = |m: &Memory| tags.iter().all(|t| m.tags.contains(t));

    let word_hits: Vec<(Memory, usize, f64)> = by_words(db, text, o)?.into_iter().filter(|(m, _, _)| has_tags(m)).collect();
    let ids: Vec<String> = word_hits.iter().map(|x| x.0.id.clone()).collect();
    if with_meaning && let Some(cosines) = cosines_for(db, text, o, &ids) {
        let v = with_gemma(db, text, word_hits, cosines, &has_tags)?;
        return rank(db, v, o, record);
    }
    let meaning_hits: Vec<(Memory, f32)> = match model {
        Some(model) if !text.trim().is_empty() => vectores::similar_in(
            db,
            model,
            text,
            &o.scope,
            o.kind,
            // Ask for all of them: the threshold only applies to what doesn't also come up by words.
            -1.0,
            CANDIDATES.max(o.limit),
        )?
        .into_iter()
        .filter(|(m, _)| has_tags(m))
        .collect(),
        _ => Vec::new(),
    };

    // RRF: each list contributes 1 / (k + rank). What shows up in both adds both.
    let mut fused: Vec<SearchResult> = Vec::new();
    let mut add = |m: &Memory, pos: usize, sim: Option<f32>, weight: f64, n: usize| {
        let contribution = weight / (RRF_K + pos as f64 + 1.0);
        match fused.iter_mut().find(|r| r.memory.id == m.id) {
            Some(r) => {
                r.score += contribution;
                r.similarity = r.similarity.or(sim);
            }
            None => fused.push(SearchResult { memory: m.clone(), score: contribution, similarity: sim, matched_words: n }),
        }
    };
    if let Fusion::Blend { weight, min_similarity: min_sim } = chosen_fusion {
        let best = word_hits.iter().map(|x| x.2).fold(f64::MIN, f64::max).max(1e-9);
        let mut v: Vec<SearchResult> = word_hits
            .iter()
            .map(|(m, n, r)| SearchResult { memory: m.clone(), score: r / best, similarity: None, matched_words: *n })
            .collect();
        for (m, s) in &meaning_hits {
            let extra = weight * f64::from((*s - min_sim).max(0.0));
            match v.iter_mut().find(|r| r.memory.id == m.id) {
                Some(r) => {
                    r.score += extra;
                    r.similarity = Some(*s);
                }
                None if *s >= min_similarity => {
                    v.push(SearchResult { memory: m.clone(), score: extra, similarity: Some(*s), matched_words: 0 })
                }
                None => {}
            }
        }
        return rank(db, v, o, record);
    }
    for (i, (m, n, _)) in word_hits.iter().enumerate() {
        add(m, i, None, 1.0, *n);
    }
    // What comes up by words and by meaning adds both positions. What only the vector brings
    // gets in only if it is really similar: with a static model, any text is somewhat similar
    // to any other (above all in the same language) and without this filter everything returns noise.
    let n_words = word_hits.len();
    for (i, (m, s)) in meaning_hits.iter().enumerate() {
        let also_by_words = word_hits.iter().any(|(p, _, _)| p.id == m.id);
        match chosen_fusion {
            Fusion::Rrf { vector_weight } => {
                if *s >= min_similarity || also_by_words {
                    add(m, i, Some(*s), vector_weight, 0);
                }
            }
            Fusion::Blend { .. } => unreachable!(),
            Fusion::Fallback | Fusion::Reserve { .. } => {
                if also_by_words {
                    add(m, 0, Some(*s), 0.0, 0); // only records the similarity, without moving the order
                } else if *s >= min_similarity {
                    add(m, n_words + i, Some(*s), 1.0, 0);
                }
            }
        }
    }
    if let Fusion::Reserve { slots, min_similarity: min_sim } = chosen_fusion {
        // What the vector sees as very similar (whether it came up lower by words or not), if it passes `desde`.
        let vector_close: Vec<SearchResult> = fused.iter().filter(|r| r.similarity.is_some_and(|s| s >= min_sim)).cloned().collect();
        if !vector_close.is_empty() {
            let mut v = rank(db, fused, &SearchOptions { limit: usize::MAX, ..o.clone() }, false)?;
            let cut = o.limit.saturating_sub(slots.min(vector_close.len()));
            let mut top: Vec<SearchResult> = v.drain(..cut.min(v.len())).collect();
            let mut extra: Vec<SearchResult> = vector_close.into_iter().filter(|r| !top.iter().any(|a| a.memory.id == r.memory.id)).collect();
            extra.sort_by(|a, b| b.similarity.unwrap_or(0.0).total_cmp(&a.similarity.unwrap_or(0.0)));
            top.extend(extra.into_iter().take(o.limit - top.len()));
            for r in v {
                if top.len() >= o.limit {
                    break;
                }
                if !top.iter().any(|a| a.memory.id == r.memory.id) {
                    top.push(r);
                }
            }
            if record {
                record_recalled(db, top.iter().map(|r| r.memory.id.as_str()), o.scope.project())?;
            }
            return Ok(top);
        }
    }
    rank(db, fused, o, record)
}

/// How much words weigh next to meaning: EmbeddingGemma's cosine plus this weight times the
/// score by words (divided by the best's) to the fourth power, so it only counts close to
/// the best. Short queries (`SHORT_QUERY_WORDS` words or fewer) are keywords, as in a search engine:
/// there words weigh more. Chosen with the 250 blind questions of `omoikane-bench`, tuning
/// with half and checking with the other: with 3,000 notes, the first hit goes from 0.94 / 0.85 /
/// 0.09 (keywords / questions / paraphrases, words only) to 0.92 / 0.89 / 0.55.
const WORDS_WEIGHT: f64 = 0.05;
const SHORT_WORDS_WEIGHT: f64 = 0.3;
pub const SHORT_QUERY_WORDS: usize = 5;
/// How long a search waits for the meaning service before going on with words only.
const MEANING_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);

/// EmbeddingGemma's cosines of the query with the memories in scope, if the model is
/// on and installed, the database has vectors for almost everything and the service answers in time.
/// The service compares them, as it has the model and the vectors in memory: the `CANDIDATES` most
/// similar and the cosine of those the words brought (`ids`).
fn cosines_for(db: &Connection, text: &str, o: &SearchOptions, ids: &[String]) -> Option<Vec<(String, f32)>> {
    if text.trim().is_empty() || !crate::significado::in_use(db) {
        return None;
    }
    let path = db.path().filter(|r| !r.is_empty()).map(std::path::PathBuf::from)?;
    let p = crate::significado::similar(&path, text, &o.scope, o.kind, CANDIDATES.max(o.limit), ids, MEANING_BUDGET)?;
    let mut v = p.top;
    for x in p.requested {
        if !v.iter().any(|y| y.0 == x.0) {
            v.push(x);
        }
    }
    Some(v)
}

/// What only meaning brings (without any query word) gets in if its cosine reaches
/// this. Without it the search with a model never came back empty and, with 3,000 unrelated notes in scope,
/// put an unrelated one first in 84 of 1,161 real messages versus 39 without the model
/// (`4-valor.md`). Calibrated on 1-nov-2026 with the 1,161 real messages over their database plus 3,000 English
/// notes (`omoi-revision-12/fix-politica.md`), tuning with the even ones and checking with the
/// odd ones: with 0.35, a first result with no word goes from 21 % to 6 % of messages (almost all
/// chit-chat that asks for nothing: «me encanta» → a 3D printing note) and the search comes back empty in
/// 4 %; in the benchmark's 250 questions and the 540 cases in English, Spanish, Catalan and with typos
/// no first hit changes (the right one with no words is around 0.6; its 5th percentile, 0.50). With 0.4
/// own memories that were relevant were already dropped (0.38: «¿ya se me puede considerar micro
/// influencer?» → the 10,000 followers).
pub const GEMMA_FLOOR: f32 = 0.35;

/// The fusion with EmbeddingGemma: candidates by words and the most similar by meaning, each
/// with its cosine plus the weight of its words (see `WORDS_WEIGHT`).
fn with_gemma(
    db: &Connection,
    text: &str,
    word_hits: Vec<(Memory, usize, f64)>,
    cosines: Vec<(String, f32)>,
    has_tags: &dyn Fn(&Memory) -> bool,
) -> Result<Vec<SearchResult>> {
    let query_words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count();
    let weight = if query_words <= SHORT_QUERY_WORDS { SHORT_WORDS_WEIGHT } else { WORDS_WEIGHT };
    let cosine: std::collections::HashMap<&str, f32> = cosines.iter().map(|(id, c)| (id.as_str(), *c)).collect();
    let best = word_hits.iter().map(|x| x.2).fold(f64::MIN, f64::max).max(1e-9);
    let mut v: Vec<SearchResult> = word_hits
        .into_iter()
        .map(|(m, n, r)| {
            let c = cosine.get(m.id.as_str()).copied();
            let score = f64::from(c.unwrap_or(0.0)) + weight * (r / best).max(0.0).powi(4);
            SearchResult { memory: m, score, similarity: c, matched_words: n }
        })
        .collect();
    let mut q = db.prepare(&format!("SELECT {COLUMNS} FROM memory m WHERE m.id = ?1"))?;
    let floor = std::env::var("OMOIKANE_CALIBRAR_SUELO").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(GEMMA_FLOOR);
    for (id, c) in cosines.iter().take(CANDIDATES) {
        if *c < floor || v.iter().any(|r| r.memory.id == *id) {
            continue;
        }
        let m = q.query_row([id], from_row)?;
        if has_tags(&m) {
            v.push(SearchResult { memory: m, score: f64::from(*c), similarity: Some(*c), matched_words: 0 });
        }
    }
    Ok(v)
}

/// Recency, pinned and inferred, final order, limit and, if due, record them as recalled.
fn rank(db: &mut Connection, mut results: Vec<SearchResult>, o: &SearchOptions, record: bool) -> Result<Vec<SearchResult>> {
    // The contradicted ones waiting for the person (`veredictos::on_hold`), quite a bit lower.
    let waiting = crate::veredictos::on_hold(db)?;
    let mut days = db.prepare("SELECT julianday('now') - julianday(?1)")?;
    for r in &mut results {
        let d: f64 = days.query_row([&r.memory.updated_at], |f| f.get(0)).unwrap_or(0.0);
        let recency = 1.0 / (1.0 + d.max(0.0) / RECENCY_DAYS);
        let pinned = if r.memory.pinned { PINNED_BOOST } else { 1.0 };
        // What an agent only inferred, without anyone saying or proving it, a bit behind
        // (`evidencia`); one verified by a person not, even if it was inferred.
        let inferred = if r.memory.verified.is_none() && crate::evidencia::only_inferred(&r.memory.evidence) { crate::evidencia::INFERRED_FACTOR } else { 1.0 };
        let on_hold = if waiting.contains(&r.memory.id) { crate::veredictos::ON_HOLD_FACTOR } else { 1.0 };
        r.score *= pinned * inferred * on_hold * (1.0 + RECENCY_BOOST * recency);
    }
    drop(days);
    results.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| b.memory.id.cmp(&a.memory.id)));
    results.truncate(o.limit);
    if record {
        record_recalled(db, results.iter().map(|r| r.memory.id.as_str()), o.scope.project())?;
    }
    Ok(results)
}

/// Candidates by words, from best to worst (BM25 plus coverage, which breaks ties when BM25 can't
/// tell, as happens with few memories), with how many query words each one has.
fn by_words(db: &Connection, text: &str, o: &SearchOptions) -> Result<Vec<(Memory, usize, f64)>> {
    let Some(query) = fts_query(text) else { return Ok(Vec::new()) };
    let terms = terms(text);
    // First only the `n` and the score, and then the rows of those that remain: with the columns in the
    // same query, SQLite read in full (with its content) every memory that had any word
    // before sorting. The order and the cut are the same: the same query, with fewer columns. If
    // everything is visible (`sees_all`) and no kind is asked for, it doesn't even need to be joined with `memory`.
    let joined = format!(
        "SELECT m.n, -bm25(memory_fts, {BM25_WEIGHTS}) AS relevancia
         FROM memory_fts JOIN memory m {} ON m.n = memory_fts.rowid
         WHERE memory_fts MATCH ?1
           AND {}
           AND (?2 IS NULL OR m.kind = ?2)
         ORDER BY bm25(memory_fts, {BM25_WEIGHTS}) LIMIT ?3",
        scope_index_hint(db),
        o.scope.sql()
    );
    let alone = format!(
        "SELECT rowid, -bm25(memory_fts, {BM25_WEIGHTS}) AS relevancia
         FROM memory_fts
         WHERE memory_fts MATCH ?1 AND ?2 IS NULL
         ORDER BY bm25(memory_fts, {BM25_WEIGHTS}) LIMIT ?3"
    );
    let mut q = db.prepare(if o.kind.is_none() && sees_all(db, &o.scope) { &alone } else { &joined })?;
    let ranked: Vec<(i64, f64)> = q
        .query_map(params![query, o.kind.map(Kind::as_str), CANDIDATES.max(o.limit) as i64], |f| Ok((f.get(0)?, f.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut row = db.prepare(&format!("SELECT {COLUMNS} FROM memory m WHERE m.n = ?1"))?;
    let mut v: Vec<(Memory, f64)> =
        ranked.into_iter().map(|(n, r)| Ok((row.query_row([n], from_row)?, r))).collect::<rusqlite::Result<_>>()?;
    // With IDF, one more paraphrase out of 70 in first place and better MRR, without losing anything in 310
    // blind questions; `OMOIKANE_CALIBRAR_COB_IDF=0` goes back to unweighted coverage.
    if !std::env::var("OMOIKANE_CALIBRAR_COB_IDF").is_ok_and(|x| x == "0") {
        let idf = term_idf(db, &terms, o)?;
        for (m, r) in &mut v {
            *r += idf_coverage(&terms, m, &idf);
        }
    } else {
        for (m, r) in &mut v {
            *r += coverage(&terms, m);
        }
    }
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.id.cmp(&a.0.id)));
    Ok(v.into_iter().map(|(m, r)| { let n = match_count(&terms, &m); (m, n, r) }).collect())
}

/// What the search and the recall return counts as recalled: it is used for ranking and, later
/// on, to propose deleting what never comes up. With `project` (a search made from a
/// project), it is also added to `memory_use`: from which projects each memory is used (the recall
/// adds it when its record expires, see `recall::on_session_start`).
pub fn record_recalled<'a>(db: &mut Connection, ids: impl Iterator<Item = &'a str>, project: Option<i64>) -> Result<()> {
    let ids: Vec<&str> = ids.collect();
    if ids.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    record_recalled_in(&tx, &ids, project)?;
    tx.commit()?;
    Ok(())
}

/// What `record_recalled` does, inside a transaction that is already open (the recall joins it with
/// its record: one write turn per message instead of two).
pub fn record_recalled_in(tx: &Connection, ids: &[&str], project: Option<i64>) -> Result<()> {
    {
        let mut u = tx.prepare(&format!(
            "UPDATE memory SET recalled_at = {NOW}, recall_count = recall_count + 1 WHERE id = ?1"
        ))?;
        let mut usage = tx.prepare(&format!(
            "INSERT INTO memory_use(memory_id, project_id, veces, ultima) VALUES (?1, ?2, 1, {NOW})
             ON CONFLICT(memory_id, project_id) DO UPDATE SET veces = veces + 1, ultima = excluded.ultima"
        ))?;
        for id in ids {
            u.execute([id])?;
            if let Some(p) = project {
                usage.execute(params![id, p])?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_query() {
        assert_eq!(
            fts_query("fallo de autenticación").unwrap(),
            // From 6 letters on, also the stem: «autenticaci*» finds «autenticaciones».
            r#""fallo" OR "fallo"* OR "autenticación" OR "autenticaci"*"#
        );
        // FTS5 syntax doesn't get through: quotes, NEAR, parentheses, colons…
        // «or» and «y» are stopwords: they are removed.
        assert_eq!(
            fts_query(r#"title:"x" OR (NEAR y) z"#).unwrap(),
            r#""title" OR "title"* OR "x" OR "near" OR "near"* OR "z""#
        );
        assert_eq!(
            fts_query("para qué es esto").unwrap(),
            r#""para" OR "para"* OR "qué" OR "qué"* OR "es" OR "esto" OR "esto"*"#
        );
        assert!(fts_query(" ¿?¡! ").is_none());
    }

    #[test]
    fn coverage_without_accents_and_by_prefix() {
        let m = Memory {
            id: "X".into(),
            kind: Kind::Gotcha,
            title: "Bug en auth".into(),
            content: "Fallo de autenticación al renovar tokens".into(),
            tags: vec!["login".into()],
            ..Memory::default()
        };
        assert_eq!(coverage(&terms("fallo autenticacion"), &m), 1.0);
        assert_eq!(coverage(&terms("token caducado"), &m), 0.5);
        assert_eq!(coverage(&terms("receta"), &m), 0.0);
    }
}
