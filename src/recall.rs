//! Automatic recall: what the Claude Code hooks inject without the agent asking.
//!
//! - At start (or after compacting): the preferences and pinned memories of the project and global ones.
//! - On each message: the memories that really have to do with it, or none. Injecting too much
//!   clutters the agent's context, so the criterion is strict and calibrated with real
//!   messages: the message must have at least `TERMINOS_MIN` useful words, the
//!   memory must share at least `RAROS_MIN` words that are rare in the database, and those
//!   words must weigh (by IDF) at least `COBERTURA_MIN` of what the message weighs.
//! - Also, the memories whose paths are mentioned in the message (a file's `gotcha`s).
//!
//! - When opening a file (1.11, `omoi recall --file-open`, the `PreToolUse` hook of Read, Edit and
//!   Write): the memories tied to that file by their paths, even if the message doesn't name it, plus its
//!   history from the journal (`riesgo`). No model, and only with `memory_path`.
//!
//! Each memory is injected once per session (`recall_log` table), except after compacting.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, Memoria};
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};

/// Minimum useful words in the message. «como ha ido todo?» must bring nothing.
pub const TERMINOS_MIN: usize = 3;
/// Rare words (in ≤ `FRACCION_RARA` of the memories) that must match.
pub const RAROS_MIN: usize = 2;
pub const FRACCION_RARA: f64 = 0.2;
/// Share of the message's IDF weight that must be in the memory. It was 0.40; recalibrated on
/// 28-Oct-2026 with 80 blind-written questions over 127 real notes: with 0.40 something was injected
/// in 8 of 20 questions without an answer, with 0.50 in 2 (and 36 of 60 with the right note versus 40). With
/// real messages, from 21 % to 11 % with something injected, and positives from 99 % to 98 %.
pub const COBERTURA_MIN: f64 = 0.50;
/// Only those close to the best: if one stands out, it goes alone. With 0.85 and at most 2 (before 0.7 and
/// 3), the same good notes with 39 % fewer extra memories; validated with a third set
/// of blind questions and another AI as judge: precision from 62 % to 68 %.
pub const RELATIVO_A_LA_MEJOR: f64 = 0.85;
/// Meaning channel (paraphrases that share no rare words): a memory also gets in if
/// its similarity (centred vectors) reaches `SIMILITUD_RECALL` and its words cover at least
/// `COBERTURA_CON_VECTOR` of the message. Both at once: the vector alone only adds noise. Only in
/// messages of up to `LARGO_MAX_CON_VECTOR` characters: the vector of a long text (a session
/// summary, an automatic task) is the average of everything and looks like anything.
/// Calibrated on 28-Oct-2026 with 80 blind questions: paraphrases with the right note from 3 to 7 of 15,
/// the same noise in the questions without an answer (2 of 20); with real messages, from 7 % to 12 % with
/// something injected, almost all of it useful when reviewed by hand.
pub const SIMILITUD_RECALL: f32 = 0.35;
pub const COBERTURA_CON_VECTOR: f64 = 0.25;
pub const LARGO_MAX_CON_VECTOR: usize = 500;
/// With EmbeddingGemma (`significado`), in the messages that look at meaning (`checks_meaning`):
/// what the words or the light model bring is left out if it isn't among the `VETO_PUESTO` most
/// similar to the message, and the most similar gets in if it reaches `ANADIR_GEMMA` and leads the second
/// by `VENTAJA_GEMMA`. Both relative: by rank and by lead, not by a fixed cosine, which changes with
/// the language and the writing style (in the benchmark, documentation English, the right note is around
/// 0.7; in Marc's real messages, Spanish with typos, 0.35, and a veto at 0.5 removed the useful ones).
/// What comes from a path in the message is not touched. Chosen with the 250 blind questions of
/// `omoikane-bench` (3,000 notes), tuning with one half and checking with the other, and reviewed by
/// hand with the last 300 real messages: the right note in questions goes from 93 % to 94 %, in
/// paraphrases from 5 % to 29 %, and in questions without an answer it adds something 8 % of the time instead
/// of 40 %.
pub const VETO_PUESTO: usize = 3;
pub const ANADIR_GEMMA: f32 = 0.6;
pub const VENTAJA_GEMMA: f32 = 0.03;
/// How long to wait for the meaning service on each message. If it takes longer (busy CPU), the
/// recall goes on with words only, as in 1.1: waiting longer would make every message slow to remove,
/// at best, one noise memory. Not silently: it is recorded (`record_model_query`) and
/// shown by `omoi doctor` and the trace (`OMOIKANE_TRAZA`).
const GEMMA_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);
/// Policy D (`omoi-revision-12/2-cuando.md`): the big model is only asked in order to **veto** what
/// the words already brought, in messages of more than `buscar::CORTA` words and of up to
/// `LARGO_MAX_GEMMA` characters. With Marc's 300 real messages, from 237 queries to 20 with the
/// same 6 changes; in the benchmark (3,000) the same noise (7.5 %), in exchange for the rescue of
/// paraphrases in recall (0.29 → 0.09), which stays in search. Above ~300 characters the
/// vector doesn't arrive within `GEMMA_BUDGET` (37 of 237 real messages waited 250 ms for nothing).
pub const LARGO_MAX_GEMMA: usize = 300;
/// At most this many memories per message.
pub const MAXIMO: usize = 2;
/// From how many letters a word also counts as a prefix («memoria» → «memorias»). With fewer,
/// «pon» matched «pone», «ponga»…
pub const PREFIJO_MIN: usize = 5;
/// Save reminder: from these messages and minutes of session without any project memory saved
/// or changed, and at most once every `MINUTOS_ENTRE_RECORDATORIOS`.
pub const MENSAJES_ANTES_DE_RECORDAR: i64 = 10;
pub const MINUTOS_ANTES_DE_RECORDAR: i64 = 15;
pub const MINUTOS_ENTRE_RECORDATORIOS: i64 = 30;
pub const RECORDATORIO: &str = "Reminder: nothing has been saved to long-term memory (Omoikane) in this session yet. If you have learned something worth knowing next time (a non-obvious trap, a decision and its reasons, how the user wants things done), save it with `remember`, including where unfinished work was left (what is done, what is next; tag `status`, and `update` the project's existing one). If not, ignore this.";

/// With `OMOIKANE_TRAZA` set, recall writes to stderr one `TRAZA <step> <json>` line per step:
/// what it saw, how it scored it and what it did with the meaning model (and why it didn't ask it, or
/// why its answer didn't arrive). For measuring and for understanding a case; without the variable it costs nothing.
fn tracing_on() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("OMOIKANE_TRAZA").is_some_and(|v| !v.is_empty()))
}

fn trace(label: &str, value: impl FnOnce() -> serde_json::Value) {
    if tracing_on() {
        eprintln!("TRAZA {label} {}", value());
    }
}

/// `MAXIMO` and `RELATIVO_A_LA_MEJOR`, except when calibrating (`examples`, test benches).
fn max_count() -> usize {
    std::env::var("OMOIKANE_CALIBRAR_MAXIMO").ok().and_then(|v| v.parse().ok()).unwrap_or(MAXIMO)
}

fn relative_cut() -> f64 {
    std::env::var("OMOIKANE_CALIBRAR_RELATIVO").ok().and_then(|v| v.parse().ok()).unwrap_or(RELATIVO_A_LA_MEJOR)
}

/// Snippet of each injected memory; the agent asks with `get` if it wants the rest.
const SNIPPET: usize = 200;
/// Snippet of each status memory at session start: longer, so that resuming doesn't need `get`.
const STATUS_SNIPPET: usize = 400;
/// How many projects with a status are shown at session start (from a general folder there are several).
pub const ESTADOS: usize = 5;

/// Conversation fillers that don't say what the message is about (besides the search's
/// stop words). Already without accents.
const CHATTER: &[&str] = &[
    "pues", "dale", "vale", "okey", "ahora", "bueno", "hay", "pon", "mira", "eso", "esto", "esta", "este", "todo", "toda",
    "todos", "algo", "hacer", "hace", "haz", "tengo", "tiene", "puedo", "puede", "quiero", "mejor", "bien", "mal", "asi",
    "aqui", "solo", "tambien", "cosas", "cosa", "otro", "otra", "otros", "otras", "ver", "sea", "ser", "estar", "porque",
    "cuando", "donde", "cual", "igual", "menos", "mas", "nada", "sin", "entonces", "creo", "hecho", "dime", "dame",
    "sigue", "seguimos", "listo", "perfecto", "gracias", "favor", "vamos", "ves", "voy", "van", "ido", "ese", "esa",
    "esos", "esas", "aun", "repite", "repito", "prueba", "mismo", "misma", "cada", "pero", "the", "this", "that", "with",
    "please", "now", "can", "you", "it", "let", "lets", "ok", "sabes", "sabe", "saber", "tengas", "tienes", "tenemos",
    "puedes", "podemos", "quieres", "necesito", "necesitas", "explica", "explicame", "ayuda", "ayudame", "hazme",
    "dimelo", "mirar", "revisa", "revisar", "estoy", "estas", "estamos", "pensando", "parece",
    "know", "want", "need", "tell", "explain", "help", "check", "look", "think",
];

/// What the agent sends to the hook on standard input. Claude Code, Codex and Gemini CLI use
/// these names; Cursor calls the session `conversation_id` and sends `workspace_roots` instead of `cwd`.
#[derive(Debug, Default, Deserialize)]
pub struct Entrada {
    #[serde(default, alias = "conversation_id", alias = "sessionId", alias = "conversationId")]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, alias = "workspacePaths")]
    pub workspace_roots: Vec<String>,
    /// The conversation (Claude Code and most: a `.jsonl`), to pull candidates from at the end.
    #[serde(default, alias = "transcriptPath")]
    pub transcript_path: String,
    /// The tool and its arguments (`PreToolUse`): the file about to be opened comes from here.
    #[serde(default, alias = "toolName")]
    pub tool_name: String,
    #[serde(default, alias = "toolInput")]
    pub tool_input: serde_json::Value,
}

/// The tools that open a file: with them `PreToolUse` says which file (`fichero_de`).
pub const ABREN_FICHERO: [&str; 5] = ["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// How many memories of a file are injected when opening it.
pub const MAXIMO_AL_ABRIR: usize = 3;

/// The file that a `PreToolUse`'s tool is about to open (`file_path`, `notebook_path` or
/// `path`), if it is one of [`ABREN_FICHERO`] (or doesn't say which).
pub fn fichero_de(e: &Entrada) -> Option<String> {
    if !e.tool_name.is_empty() && !ABREN_FICHERO.contains(&e.tool_name.as_str()) {
        return None;
    }
    ["file_path", "notebook_path", "path"].iter().find_map(|k| e.tool_input.get(k)?.as_str().map(str::trim).filter(|f| !f.is_empty()).map(str::to_string))
}

/// When opening a file (`PreToolUse` of Read, Edit or Write): the memories that have it in their paths
/// and haven't arrived yet in this session (traps and pinned first, at most
/// [`MAXIMO_AL_ABRIR`]), and the line of its history in the journal if not said already. Nothing if there
/// are none: the usual case. Before joining `memory_path` with `memory` it checks whether any path matches,
/// so opening a file with no memories costs one read of the distinct paths.
pub fn al_abrir(db: &mut Connection, project: &str, e: &Entrada) -> Result<Option<String>> {
    let Some(file) = fichero_de(e) else { return Ok(None) };
    let rel = crate::diario::relativo(&file, project);
    let matches = |path: &str| crate::riesgo::misma(&crate::diario::relativo(path, project), &rel);
    let mut all_paths = db.prepare("SELECT DISTINCT path FROM memory_path")?;
    let any_match = all_paths.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|r| matches(r));
    drop(all_paths);
    let mut chosen: Vec<Memoria> = Vec::new();
    if any_match {
        let seen = already_injected(db, &e.session_id)?;
        let mut q = db.prepare(&format!("SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id WHERE {}", Ambito::de(db, project)?.sql()))?;
        let paths: Vec<(String, String)> = q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        drop(q);
        let mut ids: Vec<String> = Vec::new();
        for (id, path) in paths {
            if matches(&path) && !seen.contains(&id) && !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in &ids {
            chosen.push(memoria::obtener(db, id)?);
        }
        let rank = |m: &Memoria| (m.kind != memoria::Tipo::Gotcha, !m.pinned);
        chosen.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.updated_at.cmp(&a.updated_at)));
        chosen.truncate(MAXIMO_AL_ABRIR);
    }
    let history = crate::riesgo::al_preguntar(db, project, &e.session_id, &rel).unwrap_or_else(|x| {
        eprintln!("omoikane: history: {x:#}");
        None
    });
    if chosen.is_empty() && history.is_none() {
        return Ok(None);
    }
    let current = current_project(db, project);
    let own = own_author(db, current);
    let mut s = String::from("<omoikane>\n");
    if !chosen.is_empty() {
        let notes = crate::revisar::notas(db, &Ambito::de(db, project)?, &chosen.iter().collect::<Vec<_>>())?;
        s.push_str(&format!("Memories about `{rel}`, the file you are opening (`get` an id for the whole memory):\n"));
        for m in &chosen {
            s.push_str(&line(m, current, own.as_deref()));
            s.push_str(&note(&notes, m));
            s.push('\n');
        }
    }
    if let Some(h) = &history {
        s.push_str(h);
        s.push('\n');
    }
    s.push_str("</omoikane>");
    let ids: Vec<String> = chosen.iter().map(|m| m.id.clone()).collect();
    record_injected(db, &e.session_id, &ids, current.max(0))?;
    Ok(Some(s))
}

/// The format the hook answers in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Formato {
    /// Claude Code and Codex: `hookSpecificOutput.additionalContext`.
    Claude,
    /// Gemini CLI: the same, with `BeforeAgent` as the message event.
    Gemini,
    /// Cursor: `additional_context` (only at session start).
    Cursor,
    /// Copilot (CLI and VS Code): `additionalContext` at the top level.
    Copilot,
    /// Antigravity: a system step in `injectSteps`.
    Antigravity,
    /// Just the text (Kiro, Kimi Code).
    Plain,
}

fn strip_accents(s: &str) -> String {
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

/// The words of the message that say what it is about: no stop words or fillers, no lone numbers and with
/// at least 3 letters.
pub fn utiles(text: &str) -> Vec<String> {
    buscar::terminos(text)
        .iter()
        .map(|t| strip_accents(t))
        .filter(|t| t.chars().count() >= 3 && !t.chars().all(|c| c.is_ascii_digit()) && !CHATTER.contains(&t.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// How much of the content counts for recall. A very long memory contains almost any word
/// and «covers» any message: with 127 real notes, one of 87 KB showed up in half of the
/// noise injections. With 6,000 characters, real messages with something injected drop from 12 %
/// to 7 % without losing any hit with blind-written questions. 3,000 was tried, better with the
/// tuning questions, but with the checking ones (which ask about details in the middle and the
/// end of the notes) it lost 7 of 35 natural ones: it was overfitted. Saving warns that a
/// longer memory is better split.
pub const LARGO_PARA_RECALL: usize = 6000;

/// The words of a memory (title, the start of the content, tags and paths), in
/// lowercase and without accents: used to check whether a word is there ([`contains_word`]).
pub(crate) fn fichas(m: &Memoria) -> BTreeSet<String> {
    let len = std::env::var("OMOIKANE_CALIBRAR_LARGO").ok().and_then(|v| v.parse().ok()).unwrap_or(LARGO_PARA_RECALL);
    let content: String = m.content.chars().take(len).collect();
    let text = format!("{} {} {} {}", m.title, content, m.tags.join(" "), m.paths.join(" "));
    strip_accents(&text.to_lowercase()).split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(String::from).collect()
}

fn contains_word(t: &str, words: &BTreeSet<String>) -> bool {
    // Those starting with `t` come together in the set's order, from the first one not less
    // than `t`: checking that one is enough (all of them used to be walked, and review does it thousands of times).
    if t.chars().count() >= PREFIJO_MIN { words.range::<str, _>((std::ops::Bound::Included(t), std::ops::Bound::Unbounded)).next().is_some_and(|p| p.starts_with(t)) } else { words.contains(t) }
}

/// Memories in scope (the project, its space and the global ones; all of them from a general folder) and in
/// how many each term appears.
fn frequencies(db: &Connection, project: &str, terms: &[String]) -> Result<(f64, Vec<f64>)> {
    Frecuencias::nuevas(db, &Ambito::de(db, project)?)?.de(terms)
}

/// In how many memories of a scope each word appears, remembering the ones already counted: for whoever measures
/// many texts in the same scope (the trade duplicates review, which used to count the
/// same words once per pair).
pub(crate) struct Frecuencias<'a> {
    q: rusqlite::Statement<'a>,
    /// How many memories there are in the scope.
    pub n: f64,
    counted: HashMap<String, f64>,
}

impl<'a> Frecuencias<'a> {
    pub fn nuevas(db: &'a Connection, scope: &Ambito) -> Result<Frecuencias<'a>> {
        let n = memoria::cuantas(db, scope)? as f64;
        let q = if memoria::ve_todas(db, scope) {
            db.prepare("SELECT count(*) FROM memory_fts WHERE memory_fts MATCH ?1")?
        } else {
            db.prepare(&format!(
                "SELECT count(*) FROM memory_fts JOIN memory m {} ON m.n = memory_fts.rowid
                 WHERE memory_fts MATCH ?1 AND {}",
                memoria::por_ambito(db),
                scope.sql()
            ))?
        };
        Ok(Frecuencias { q, n, counted: HashMap::new() })
    }

    /// `(n, df)` of `terms`, like `frequencies`.
    pub fn de(&mut self, terms: &[String]) -> Result<(f64, Vec<f64>)> {
        let mut df = Vec::with_capacity(terms.len());
        for t in terms {
            if let Some(d) = self.counted.get(t) {
                df.push(*d);
                continue;
            }
            let query = if t.chars().count() >= PREFIJO_MIN { format!("\"{t}\"*") } else { format!("\"{t}\"") };
            let d = self.q.query_row(params![query], |row| row.get::<_, i64>(0)).unwrap_or(0) as f64;
            self.counted.insert(t.clone(), d);
            df.push(d);
        }
        Ok((self.n, df))
    }
}

/// The weights (IDF) of the words of a text, to measure how much of it other memories cover without
/// counting them again: [`Pesos::cobertura`].
pub(crate) struct Pesos {
    terms: Vec<String>,
    idf: Vec<f64>,
    total: f64,
}

impl Pesos {
    pub fn de(text: &str, freqs: &mut Frecuencias) -> Result<Pesos> {
        let terms = utiles(text);
        let (n, df) = freqs.de(&terms)?;
        let idf: Vec<f64> = df.iter().map(|d| idf(n, *d)).collect();
        let total = idf.iter().sum();
        Ok(Pesos { terms, idf, total })
    }

    /// The same as `cobertura` for a memory whose `fichas` are already known.
    pub fn cobertura(&self, words: &BTreeSet<String>) -> f64 {
        let inside: f64 = self.terms.iter().zip(&self.idf).filter(|(t, _)| contains_word(t, words)).map(|(_, i)| i).sum();
        if self.total > 0.0 { inside / self.total } else { 0.0 }
    }
}

/// The Lucene IDF of a word that appears in `df` of `n` memories (see `relevant_with`).
fn idf(n: f64, df: f64) -> f64 {
    (1.0 + (n - df.max(1.0) + 0.5) / (df.max(1.0) + 0.5)).ln()
}

/// Which share of the IDF weight of the words of `text` is in each memory (IDF of the scope, as in
/// `relevantes`). The duplicates warning uses it too.
pub fn cobertura<'a>(db: &Connection, project: &str, text: &str, memories: impl Iterator<Item = &'a Memoria>) -> Result<Vec<f64>> {
    cobertura_en(db, &Ambito::de(db, project)?, text, memories)
}

/// `cobertura` with the IDF of an already resolved scope. For trade ones, `Ambito::Todos`: from
/// global they aren't visible (with their scope, the IDF of a database with only trade memories came out negative).
pub fn cobertura_en<'a>(db: &Connection, scope: &Ambito, text: &str, memories: impl Iterator<Item = &'a Memoria>) -> Result<Vec<f64>> {
    let weights = Pesos::de(text, &mut Frecuencias::nuevas(db, scope)?)?;
    Ok(memories.map(|m| weights.cobertura(&fichas(m))).collect())
}

/// `cobertura` with the frequencies of `terms` (those of `utiles(text)`) already computed.
fn coverage_with<'a>(terms: &[String], (n, df): &(f64, Vec<f64>), memories: impl Iterator<Item = &'a Memoria>) -> Vec<f64> {
    let idf: Vec<f64> = df.iter().map(|d| idf(*n, *d)).collect();
    let total: f64 = idf.iter().sum();
    memories
        .map(|m| {
            let words = fichas(m);
            let inside: f64 = terms.iter().zip(&idf).filter(|(t, _)| contains_word(t, &words)).map(|(_, i)| i).sum();
            if total > 0.0 { inside / total } else { 0.0 }
        })
        .collect()
}

/// The memories that have to do with `prompt`, from most to least, with their coverage.
pub fn relevantes(db: &mut Connection, project: &str, prompt: &str) -> Result<Vec<(Memoria, f64)>> {
    let terms = utiles(prompt);
    if terms.len() < TERMINOS_MIN {
        return Ok(Vec::new());
    }
    let freqs = frequencies(db, project, &terms)?;
    relevant_with(db, project, &terms, &freqs)
}

/// `relevantes` with the message's terms (at least `TERMINOS_MIN`) and their frequencies already computed.
fn relevant_with(db: &mut Connection, project: &str, terms: &[String], (n, df): &(f64, Vec<f64>)) -> Result<Vec<(Memoria, f64)>> {
    let n = *n;
    // Lucene IDF, always positive: with the classic BM25 one, in a database of one or two memories everything
    // weighs zero and nothing would be injected right when someone starts.
    // A word that is in no memory weighs like one that were in a single one, no more: with
    // the whole IDF, in a small database it sank the coverage of the memory that is on topic («estoy
    // pensando en cambiar la pasarela de pago a Stripe» didn't bring the decision about the gateway). Without
    // counting them at all, long messages brought something almost always (53 % of the real ones).
    let idf: Vec<f64> = df.iter().map(|d| (1.0 + (n - d.max(1.0) + 0.5) / (d.max(1.0) + 0.5)).ln()).collect();
    let total: f64 = idf.iter().sum();
    if total <= 0.0 {
        return Ok(Vec::new());
    }
    let rare_df = (FRACCION_RARA * n).max(1.0);
    trace("terminos", || serde_json::json!({"n": n, "terminos": terms, "df": df, "idf": idf, "raros_msg": df.iter().filter(|d| **d <= rare_df).count()}));
    // Only for calibrating (`examples/fase4.rs`): without these variables the constants apply.
    let read_env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let min_coverage = read_env("OMOIKANE_CALIBRAR_COBERTURA").unwrap_or(COBERTURA_MIN);
    let min_rare = read_env("OMOIKANE_CALIBRAR_RAROS").map_or(RAROS_MIN, |v| v as usize);
    // If the message doesn't have that many rare words, no memory can share them: there is no need to
    // look for candidates.
    if df.iter().filter(|d| **d <= rare_df).count() < min_rare {
        return Ok(Vec::new());
    }
    // 30 candidates: a long note that is exactly on topic can rank far down in BM25.
    let opts = Opciones { ambito: Ambito::de(db, project)?, kind: None, tags: vec![], limite: 30 };
    // Words only: the vector didn't improve search measurably and here noise
    // costs more. This way the hook doesn't open the model either.
    let candidates = buscar::buscar_sin_apuntar(db, &terms.join(" "), &opts)?;
    let mut trace_c: Vec<serde_json::Value> = Vec::new();
    let mut v: Vec<(Memoria, f64, usize)> = candidates
        .into_iter()
        .filter_map(|r| {
            let words = fichas(&r.memoria);
            let title_words = fichas(&Memoria { content: String::new(), paths: vec![], tags: vec![], ..r.memoria.clone() });
            let (mut weight, mut rare) = (0.0, 0);
            for (i, t) in terms.iter().enumerate() {
                if contains_word(t, &words) {
                    weight += idf[i];
                    rare += usize::from(df[i] <= rare_df);
                }
            }
            let coverage = weight / total;
            // How many words of the message are in the title: breaks ties in favour of the memory that is
            // exactly on topic («building my future» → the Building My Future note).
            let in_title = terms.iter().filter(|t| contains_word(t, &title_words)).count();
            if tracing_on() {
                trace_c.push(serde_json::json!({"id": r.memoria.id, "bm25": r.score, "pal": r.matched_words, "cob": coverage, "raros": rare, "tit": in_title, "largo": r.memoria.content.chars().count()}));
            }
            (rare >= min_rare && coverage >= min_coverage).then_some((r.memoria, coverage, in_title))
        })
        .collect();
    trace("candidatas", || serde_json::Value::Array(trace_c));
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
    let best = v.first().map(|x| x.1).unwrap_or(0.0);
    v.retain(|x| x.1 >= relative_cut() * best);
    v.truncate(max_count());
    Ok(v.into_iter().map(|(m, c, _)| (m, c)).collect())
}

/// Whether the meaning channel looks at this message (see `SIMILITUD_RECALL`): if not, there's no need to even open
/// the model.
fn checks_meaning(prompt: &str) -> bool {
    let min_sim = std::env::var("OMOIKANE_CALIBRAR_SIM_RECALL").ok().and_then(|v| v.parse::<f64>().ok()).map_or(SIMILITUD_RECALL, |v| v as f32);
    min_sim <= 1.0 && prompt.chars().count() <= LARGO_MAX_CON_VECTOR && utiles(prompt).len() >= TERMINOS_MIN
}

/// Those that resemble the message in meaning and share part of its words (see
/// `SIMILITUD_RECALL`), from most to least similar.
pub fn por_significado(db: &Connection, project: &str, prompt: &str, model: &Modelo) -> Result<Vec<Memoria>> {
    by_meaning_with(db, project, prompt, model, None)
}

/// `por_significado`, with the frequencies of `utiles(prompt)` if they were already computed.
fn by_meaning_with(db: &Connection, project: &str, prompt: &str, model: &Modelo, freqs: Option<&(f64, Vec<f64>)>) -> Result<Vec<Memoria>> {
    if !checks_meaning(prompt) {
        return Ok(Vec::new());
    }
    let read_env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let min_sim = read_env("OMOIKANE_CALIBRAR_SIM_RECALL").map_or(SIMILITUD_RECALL, |v| v as f32);
    let min_cov = read_env("OMOIKANE_CALIBRAR_COB_VECTOR").unwrap_or(COBERTURA_CON_VECTOR);
    let v = crate::vectores::parecidas_en(db, model, prompt, &Ambito::de(db, project)?, None, min_sim, MAXIMO)?;
    if v.is_empty() {
        return Ok(Vec::new());
    }
    let c = match freqs {
        Some(f) => coverage_with(&utiles(prompt), f, v.iter().map(|x| &x.0)),
        None => cobertura(db, project, prompt, v.iter().map(|x| &x.0))?,
    };
    Ok(v.into_iter().zip(c).filter(|(_, c)| *c >= min_cov).map(|((m, _), _)| m).collect())
}

/// Memories in scope with some path that appears in the message (the whole path, or the file
/// name if it has at least 5 characters).
pub fn por_rutas(db: &Connection, project: &str, prompt: &str) -> Result<Vec<Memoria>> {
    let text = prompt.to_lowercase().replace('\\', "/");
    let appears = |path: &str| {
        let path = path.to_lowercase();
        let name = path.rsplit('/').next().unwrap_or(&path);
        text.contains(&path) || (name.chars().count() >= 5 && text.contains(name))
    };
    // Usually no path appears in the message: only `memory_path` is checked, without joining it with
    // `memory` (which walks the memories in scope: 90 ms with 100,000).
    let mut all_paths = db.prepare("SELECT DISTINCT path FROM memory_path")?;
    let any = all_paths.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.iter().any(|r| appears(r));
    if !any {
        return Ok(Vec::new());
    }
    let mut q = db.prepare(&format!(
        "SELECT DISTINCT mp.memory_id, mp.path FROM memory_path mp JOIN memory m ON m.id = mp.memory_id
         WHERE {}",
        Ambito::de(db, project)?.sql()
    ))?;
    let paths: Vec<(String, String)> = q.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut ids: Vec<String> = Vec::new();
    for (id, path) in paths {
        if appears(&path) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.iter().take(MAXIMO).map(|id| memoria::obtener(db, id)).collect()
}

fn already_injected(db: &Connection, session: &str) -> Result<BTreeSet<String>> {
    let mut q = db.prepare("SELECT memory_id FROM recall_log WHERE session = ?1")?;
    let v = q.query_map([session], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Records what was injected in the session, with the project it was asked from (0 from a general
/// folder or a project with no memories yet).
fn record_injected(db: &mut Connection, session: &str, ids: &[String], project_id: i64) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for id in ids {
        tx.execute(
            &format!("INSERT OR IGNORE INTO recall_log(session, memory_id, at, project_id) VALUES (?1, ?2, {}, ?3)", memoria::AHORA),
            params![session, id, project_id],
        )?;
    }
    buscar::apuntar_recuperadas_en(&tx, &ids.iter().map(String::as_str).collect::<Vec<_>>(), None)?;
    tx.commit()?;
    Ok(())
}

/// Where a memory lives, if it isn't from the session's project (`current`): `[global]`, `[space x]` or
/// `[project x]` (seen from a general folder or from another project of the space).
fn location(m: &Memoria, current: i64) -> String {
    let trades = crate::oficios::de_memoria(m);
    if !trades.is_empty() {
        let for_projects = crate::oficios::para_de(&m.tags).map(|p| format!(", {p} projects")).unwrap_or_default();
        format!(" [trade {}{for_projects}]", trades.join(", "))
    } else if m.global() {
        " [global]".to_string()
    } else if m.de_espacio() {
        format!(" [space {}]", m.space)
    } else if m.project_id != current {
        format!(" [project {}]", crate::proyecto::nombre_de(&m.project, &m.project_key))
    } else {
        String::new()
    }
}

/// The id of the session's project (-1 if it has nothing yet; never 0: global is not a session).
fn current_project(db: &Connection, project: &str) -> i64 {
    match crate::proyecto::resolver(db, project) {
        Ok(Some((id, _))) if !project.is_empty() => id,
        _ => -1,
    }
}

/// `current` is the session's project: a memory from another project (seen from a general folder)
/// says which one it is from. `own` is this machine's author: a memory from another person says whose it is,
/// so that the agent treats it with care.
fn line(m: &Memoria, current: i64, own: Option<&str>) -> String {
    let snippet = snippet(&m.content, SNIPPET);
    let paths = if m.paths.is_empty() { String::new() } else { format!(" ({})", m.paths.join(", ")) };
    let place = location(m, current);
    // Without an own author (this machine doesn't record its own), whatever has an author comes from another machine.
    let by = match own {
        _ if m.author.is_empty() => String::new(),
        Some(me) if m.author == me => String::new(),
        _ => format!(" [by {}]", m.author),
    };
    format!("- [{}{}]{place}{by}{paths} {}: {snippet} (id {})", m.kind, mark(m), m.title, m.id.chars().take(12).collect::<String>())
}

/// The mark after the kind: `[decision, verified]` if a person verified it (`verificado`), or
/// `[fact, inferred]` if an agent only inferred it (`evidencia`): to be checked before relying on it.
fn mark(m: &Memoria) -> &'static str {
    if m.verified.is_some() {
        ", verified"
    } else if crate::evidencia::solo_deducida(&m.evidence) {
        ", inferred"
    } else {
        ""
    }
}

/// The `revisar` warning about a memory, for the end of its line (or nothing).
fn note(notes: &std::collections::HashMap<String, String>, m: &Memoria) -> String {
    notes.get(&m.id).map(|n| format!(" ⚠ {n}")).unwrap_or_default()
}

/// The content on one line and cut to `n` characters.
fn snippet(text: &str, n: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n {
        format!("{}…", flat.chars().take(n).collect::<String>())
    } else {
        flat
    }
}

/// At session start (or after compacting): the context the agent must always have.
/// This machine's author, if sync records it: with it, other people's memories are told apart.
/// `current`: the session's project (-1 or 0, none): in a project of a space, the author this
/// machine signs with in that space.
fn own_author(db: &Connection, current: i64) -> Option<String> {
    // The one recorded in each memory saved here (that of its space's repository or the general one).
    let author = if current > 0 { memoria::autor_de(db, current, None) } else { memoria::autor_en(db, "") };
    if let Some(a) = author.ok().filter(|a| !a.is_empty()) {
        return Some(a);
    }
    let path = db.path().filter(|p| !p.is_empty())?;
    crate::sync::leer_config(std::path::Path::new(path)).ok().flatten().and_then(|c| c.author)
}

pub fn al_empezar(db: &mut Connection, project: &str, e: &Entrada) -> Result<Option<String>> {
    // Once per session: what fixes itself (old statuses with a newer one, decisions already
    // resolved by their task) and then the warnings and links up to date (after updating, or what sync brought).
    crate::revisar::limpiar_callado(db);
    crate::revisar::al_dia(db)?;
    let update_notice = crate::actualizar::aviso();
    // The session's project gets recorded (with its key, computed once) and its `origin` up to date. If
    // it is the new folder of a project that moved (same `origin`), it is said once.
    let moved = if crate::proyecto::es_general(project) { None } else { crate::proyecto::registrar_avisando(db, project)?.1 };
    let scope = Ambito::de(db, project)?;
    // Its trades, looking at its files (root and one level down): with them come the trade memories.
    // Optional: if it fails, the session starts without them.
    let trades = match scope.proyecto() {
        Some(id) => crate::oficios::al_dia(db, id, project).unwrap_or_else(|x| {
            eprintln!("omoikane: trades: {x:#}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    let current = current_project(db, project);
    let own = own_author(db, current);
    // A session that starts or is compacted no longer has in its context what was injected before. What gets
    // deleted is summarized in `memory_use` (how many times and from which project each memory was received).
    {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = "session = ?1 OR at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')";
        tx.execute(
            &format!(
                "INSERT INTO memory_use(memory_id, project_id, veces, ultima)
                 SELECT memory_id, project_id, count(*), max(at) FROM recall_log WHERE {expired} GROUP BY memory_id, project_id
                 ON CONFLICT(memory_id, project_id) DO UPDATE SET veces = veces + excluded.veces, ultima = max(ultima, excluded.ultima)"
            ),
            [&e.session_id],
        )?;
        tx.execute(&format!("DELETE FROM recall_log WHERE {expired}"), [&e.session_id])?;
        tx.commit()?;
    }
    // File history is optional: if it fails, the session starts anyway.
    if let Err(x) = crate::riesgo::al_empezar(db, &e.session_id) {
        eprintln!("omoikane: history: {x:#}");
    }
    let total = memoria::cuantas(db, &scope)?;
    let context = memoria::contexto(db, &scope, 30)?;
    // The first session here in a project of a space: what the team has (before recording the
    // session, which is what says whether it is the first). Optional: an error only leaves the session without it.
    let welcome = crate::llegada::bienvenida(db, &scope, &e.session_id, own.as_deref()).unwrap_or_else(|x| {
        eprintln!("omoikane: welcome: {x:#}");
        None
    });
    // After compacting it is still the same session: the reminder clock is not reset.
    db.execute(
        &format!(
            "INSERT INTO session(session, project, started_at) VALUES (?1, ?2, {now})
             ON CONFLICT(session) DO UPDATE SET project = excluded.project,
               started_at = CASE WHEN ?3 = 'compact' THEN session.started_at ELSE excluded.started_at END,
               prompts = CASE WHEN ?3 = 'compact' THEN session.prompts ELSE 0 END,
               reminded_at = CASE WHEN ?3 = 'compact' THEN session.reminded_at ELSE NULL END",
            now = memoria::AHORA
        ),
        params![e.session_id, project, e.source],
    )?;
    let mut s = String::from("<omoikane>\n");
    // A paused project isn't synced until the person decides: don't let it be forgotten.
    if let Some(p) = crate::sync::aviso_de_pausa_agente(db) {
        s.push_str(&p);
        s.push('\n');
    }
    if let Some(old) = &moved {
        s.push_str(&format!("This project used to be in {old}, which no longer exists: its memories follow this folder now (same git origin). Tell the user once.\n"));
    }
    if crate::proyecto::es_general(project) {
        s.push_str(&format!(
            "Long-term memory (Omoikane) holds {total} memories across all projects. Use the omoikane tools: `search` before working on something, `remember` what is worth knowing next time.\nThis folder is not a project, so search covers every project. When a memory is about one project, pass its folder as `project` (or put its absolute paths in `paths`); otherwise it is saved as global.\n"
        ));
    } else {
        s.push_str(&format!(
            "Long-term memory (Omoikane) holds {total} memories for this project and globally. Use the omoikane tools: `search` before working on something, `remember` what is worth knowing next time.\n"
        ));
    }
    if !trades.is_empty() {
        s.push_str(&format!(
            "This project's trades (its stack, from its files): {}. A trap or know-how about the stack itself, useful to any project of that trade and with nothing of this project or its client in it, goes with `trade` (e.g. `trade: \"{}\"`) and reaches every {} project; anything about this project stays in the project.\n",
            trades.join(", "),
            trades[0],
            trades[0]
        ));
    }
    if let Some(l) = &welcome {
        s.push_str(l);
    }
    if !context.is_empty() {
        let notes = crate::revisar::notas(db, &scope, &context.iter().collect::<Vec<_>>())?;
        s.push_str("The user's preferences and the pinned memories:\n");
        for m in &context {
            s.push_str(&line(m, current, own.as_deref()));
            s.push_str(&note(&notes, m));
            s.push('\n');
        }
    }
    // Where each project was left (the latest `status` memory of each one), with a good
    // chunk of the content: «sigamos con X» is answered without searching. These don't count as injected either.
    let statuses: Vec<Memoria> = memoria::estados(db, &scope, ESTADOS)?
        .into_iter()
        .filter(|m| !context.iter().any(|c| c.id == m.id))
        .collect();
    if !statuses.is_empty() {
        let notes = crate::revisar::notas(db, &scope, &statuses.iter().collect::<Vec<_>>())?;
        s.push_str("Where work was left (the latest `status` memory of each project, newest first):\n");
        for m in &statuses {
            let line = short_line(m, current, own.as_deref());
            s.push_str(&format!("{line}{}: {}\n", note(&notes, m), snippet(&m.content, STATUS_SNIPPET)));
        }
    }
    // The decisions waiting for the person (tag `open`): they show up in every session until they decide.
    // They don't count as injected either: recall can still bring them whole when relevant.
    // Optional like the history: an error only leaves the session without the list.
    let open: Vec<Memoria> = memoria::abiertas(db, &scope, usize::MAX)
        .unwrap_or_else(|x| {
            eprintln!("omoikane: open decisions: {x:#}");
            Vec::new()
        })
        .into_iter()
        .filter(|m| !context.iter().any(|c| c.id == m.id))
        .collect();
    if !open.is_empty() {
        // Blocking ones go first (`memoria::abiertas`) and marked: the work that depends on them
        // waits for the person; the rest are for reviewing later.
        let blocking = open.iter().filter(|m| m.bloqueante()).count();
        if blocking > 0 {
            s.push_str(&format!("Decisions waiting for the user (decisions tagged `open`; the {blocking} marked BLOCKING first, then the rest, newest first. BLOCKING: don't go on with the work that depends on it, ask the user now; the rest are for later: mention them at a natural moment. When they decide, `update` the memory with the answer and drop the `open` and `blocking` tags):\n"));
        } else {
            s.push_str("Decisions waiting for the user (decisions tagged `open`, newest first; for later: mention them to the user at a natural moment; when they decide, `update` the memory with the answer and drop the `open` tag):\n");
        }
        for m in open.iter().take(ABIERTAS) {
            let line = short_line(m, current, own.as_deref());
            let line = if m.bloqueante() { line.replacen("- ", "- BLOCKING ", 1) } else { line };
            s.push_str(&citado(&format!("{line}: {}\n", snippet(&m.content, OPEN_SNIPPET))));
        }
        if open.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`search` with the tag `open` for the rest).\n", open.len() - ABIERTAS));
        }
    }
    // The contradictions waiting for the person (1.14): the contradicted one doesn't come up on its own until they say
    // which holds. Optional like the decisions.
    let pairs = contradictions_in(db, &scope).unwrap_or_else(|x| {
        eprintln!("omoikane: contradictions: {x:#}");
        Vec::new()
    });
    if !pairs.is_empty() {
        s.push_str("Contradictions waiting for the user (two memories disagree and nobody has said which holds; the one ON HOLD doesn't come up in automatic recall until then. Ask the user at a natural moment, or now if your work depends on it, and save their answer with `decisions`: contradiction [both ids], keep (the id that holds, or \"both\") and answer (their words). If you can check it yourself, still ask before deciding):\n");
        for (a, b, reason) in pairs.iter().take(ABIERTAS) {
            let short = |m: &Memoria| format!("«{}» (id {})", m.title, m.id.chars().take(12).collect::<String>());
            let because = if reason.trim().is_empty() { String::new() } else { format!(": {}", crate::veredictos::motivo_corto(reason)) };
            s.push_str(&citado(&format!("- {} vs ON HOLD {}{because}\n", short(a), short(b))));
        }
        if pairs.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`context` lists them under review).\n", pairs.len() - ABIERTAS));
        }
    }
    // The review dates that have arrived, with what to check (1.14): someone set it to
    // find out that day.
    let dates = crate::revisar::fechas_llegadas(db, &scope).unwrap_or_else(|x| {
        eprintln!("omoikane: review dates: {x:#}");
        Vec::new()
    });
    if !dates.is_empty() {
        s.push_str("Review dates reached (set when saving, to check something that day; tell the user at a natural moment. Once checked, `update` the memory: what changed, a new review:YYYY-MM-DD tag with its review_reason, or the tag removed):\n");
        for (m, f) in dates.iter().take(ABIERTAS) {
            let line = short_line(m, current, own.as_deref());
            let what = m.review_reason.as_deref().unwrap_or("no reason given: check it still holds");
            s.push_str(&citado(&format!("{line}: {f}, {what}\n")));
        }
        if dates.len() > ABIERTAS {
            s.push_str(&format!("…and {} more (`omoi review`).\n", dates.len() - ABIERTAS));
        }
    }
    // The last things worked on: titles only, so that «sigamos con lo de X» doesn't force the
    // agent to explore. They aren't recorded as injected: their content can still arrive with recall.
    // A status that is no longer the latest of its project doesn't show up either: it would lead to resuming from the old one.
    let latest: Vec<Memoria> = memoria::recientes(db, &scope, None, 50)?
        .into_iter()
        .filter(|m| !context.iter().any(|c| c.id == m.id) && !m.tags.iter().any(|t| t == memoria::ESTADO))
        .filter(|m| !open.iter().any(|a| a.id == m.id))
        .take(RECIENTES)
        .collect();
    if !latest.is_empty() {
        s.push_str("Last worked on (newest first; `get` an id for the whole memory):\n");
        let notes = crate::revisar::notas(db, &scope, &latest.iter().collect::<Vec<_>>())?;
        for m in &latest {
            s.push_str(&short_line(m, current, own.as_deref()));
            s.push_str(&note(&notes, m));
            s.push('\n');
        }
    }
    // What was maybe worth saving from previous sessions (7.2): the agent decides.
    let cs = crate::candidatas::para_empezar(db, project, &e.session_id)?;
    if !cs.is_empty() {
        s.push_str("From your previous sessions here, possibly worth keeping (Omoikane picked them without understanding them: save the ones that are worth it with `remember`, in the user's own words and language, merging into existing memories where they fit; ask the user if unsure; ignore the rest, they won't come back):\n");
        for c in &cs {
            s.push_str(&format!("- [{}] {} ({})\n", c.kind, c.text.chars().take(900).collect::<String>(), c.created_at.get(..10).unwrap_or("")));
        }
    }
    // The review, if it has piled up (once a day): the agent proposes, the user decides. It is
    // optional: if it fails (the database busy with a long sync), only the warning is lost, not the block.
    if let Some(a) = crate::revisar::aviso_al_empezar(db, &scope, total as usize).ok().flatten() {
        s.push_str(&a);
        s.push('\n');
    }
    if let Some(a) = &update_notice {
        s.push_str(a);
        s.push('\n');
    }
    // The meaning model, if the memory already needs it: it downloads it or leaves it indexing, and the
    // first time it turns on by itself it says so.
    if let Some(a) = crate::significado::al_empezar(db) {
        s.push_str(&a);
        s.push('\n');
    }
    s.push_str(CONTINUE_HINT);
    s.push_str("</omoikane>");
    crate::documentos::al_empezar(db, project, &e.session_id);
    let ids: Vec<String> = context.iter().map(|m| m.id.clone()).collect();
    record_injected(db, &e.session_id, &ids, scope.proyecto().unwrap_or(0))?;
    Ok(Some(s))
}

/// The current contradictions in which both memories are visible from `scope`: (the contradicting one,
/// the contradicted one, the reason), from newest to oldest.
fn contradictions_in(db: &Connection, scope: &Ambito) -> Result<Vec<(Memoria, Memoria, String)>> {
    let mut out = Vec::new();
    for v in crate::veredictos::contradicciones(db)? {
        if scope.ve(db, &v.memory_id)? && scope.ve(db, &v.other_id)? {
            out.push((memoria::obtener(db, &v.memory_id)?, memoria::obtener(db, &v.other_id)?, v.reason));
        }
    }
    Ok(out)
}

/// Text not written by Omoikane (a documentation file, a memory) inside the
/// `<omoikane>` block: its `<omoikane` and `</omoikane`, upper or lower case, become `‹omoikane`, so
/// that they can't close the block or open another one.
pub fn citado(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(i) = rest.find('<') {
        s.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let after = after.strip_prefix('/').unwrap_or(after);
        let is_tag = after.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("omoikane"));
        s.push(if is_tag { '‹' } else { '<' });
        rest = &rest[i + 1..];
    }
    s.push_str(rest);
    s
}

/// How many recent memories are shown at session start.
pub const RECIENTES: usize = 5;

/// How many open decisions are shown at session start; for the rest only how many there are is said.
pub const ABIERTAS: usize = 10;
/// Snippet of each open decision: just enough to remember the question.
const OPEN_SNIPPET: usize = 160;

/// What to do with an ambiguous request and when to leave a note of where something was left.
const CONTINUE_HINT: &str = "If the user asks to continue something and it is unclear what, check the list above and do at most one `search`; if that doesn't settle it, ask the user right away instead of exploring files. When you finish a piece of work or the session winds down, save where it was left (what is done, what is next) with `remember` and the tag `status`: one per project, so if the project already has one (listed above), `update` it instead of saving another. When a product or business decision is the user's and you can't ask now, save it as kind decision with the tag `open` (question, options, what you assumed meanwhile), keep working, and when you finish a long piece of work tell the user which ones you left. Add the tag `blocking` too only when the work can't go on without the answer (no safe assumption, or a wrong guess would be costly to undo): then stop that part and ask. Without it, the decision is for later.\n";

/// A memory on one line, without content: date, kind, project if not the session's, title and id.
fn short_line(m: &Memoria, current: i64, own: Option<&str>) -> String {
    let place = location(m, current);
    let by = match own {
        Some(me) if !m.author.is_empty() && m.author != me => format!(" [by {}]", m.author),
        _ => String::new(),
    };
    let date = m.updated_at.get(..10).unwrap_or(&m.updated_at);
    format!("- {date} [{}{}]{place}{by} {} (id {})", m.kind, mark(m), m.title, m.id.chars().take(12).collect::<String>())
}

/// On each message: what has to do with it and hasn't been injected yet in this session, or nothing.
/// What a linked memory must cover of the message to be brought along with the one linking it: half
/// of the recall bar, because the link already says it is related.
pub const LISTON_VECINA: f64 = 0.25;
/// And if it is the injected memory that links it: whoever wrote it already said they go together.
pub const LISTON_ENLAZADA: f64 = 0.10;

/// How much of a procedure is injected (the whole steps, not a snippet).
const PROCEDURE_LEN: usize = 4000;

/// The words of a procedure's `when:` line (comma-separated), in lowercase and without accents.
pub fn disparadores(m: &Memoria) -> Vec<String> {
    m.content
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            let norm = strip_accents(&l.to_lowercase());
            norm.strip_prefix("when:").or_else(|| norm.strip_prefix("cuando:")).map(|r| r.to_string())
        })
        .flat_map(|r| r.split(',').map(|w| w.trim().trim_matches(['"', '\'', '«', '»', '`', '.']).to_string()).collect::<Vec<_>>())
        .filter(|w| w.chars().count() >= 3)
        .collect()
}

/// The procedure the message asks for: a `procedure` in scope with one of its `when:` words in it
/// (whole word: «publica» is not «publicación») and something more saying it is that procedure. A lone
/// word fits many things: in Marc's real messages, «publica un post en LinkedIn» or
/// «pues publica» brought «Publicar una versión de Omoikane» (9 of the 19 noise cases left with the
/// model, `4-valor.md`). One of these is enough:
/// - a phrase from its `when:` («saca versión») or two different words from the list;
/// - that the procedure is from the session's project (in Omoikane's folder, «publica» means publishing it);
/// - that the message also says something from its title, its tags or its project («publica la versión»,
///   «publica omoikane»), not counting the `when:` words themselves.
fn matching_procedure(db: &Connection, project: &str, prompt: &str) -> Result<Option<Memoria>> {
    let norm = strip_accents(&prompt.to_lowercase());
    let has_word = |w: &str| {
        norm.match_indices(w).any(|(i, _)| {
            norm[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()) && norm[i + w.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric())
        })
    };
    let procedures = memoria::recientes(db, &Ambito::de(db, project)?, Some(memoria::Tipo::Procedure), 200)?;
    if procedures.is_empty() {
        return Ok(None);
    }
    let current = current_project(db, project);
    let prompt_words = utiles(prompt);
    Ok(procedures.into_iter().find(|m| {
        let triggers = disparadores(m);
        let said: Vec<&String> = triggers.iter().filter(|w| has_word(w)).collect();
        if said.is_empty() {
            return false;
        }
        // Each word by its root (5 letters): «publica» and «publicar» are the same.
        let root = |w: &str| w.chars().take(PREFIJO_MIN).collect::<String>();
        let distinct: BTreeSet<String> = said.iter().map(|w| root(w)).collect();
        if said.iter().any(|w| w.contains(' ')) || distinct.len() >= 2 || (current > 0 && m.project_id == current) {
            return true;
        }
        let roots: BTreeSet<String> = triggers.iter().filter(|w| !w.contains(' ')).map(|w| root(w)).collect();
        let is_trigger = |p: &str| roots.iter().any(|r| p.starts_with(r.as_str()));
        let about: BTreeSet<String> = {
            let name = if m.project_id > 0 { crate::proyecto::nombre_de(&m.project, &m.project_key) } else { String::new() };
            let only_name = Memoria { content: name, paths: vec![], ..m.clone() };
            fichas(&only_name).into_iter().filter(|p| !is_trigger(p)).collect()
        };
        prompt_words.iter().any(|t| !is_trigger(t) && contains_word(t, &about))
    }))
}

/// How many most-similar memories recall asks the service for: the 3 of the veto, the 1st and 2nd to
/// add, and the cosine of almost any that the words bring (the rest count as 0).
const SIMILAR_FOR_RECALL: usize = 40;

/// Why the big model is not asked on this message (policy D, see `LARGO_MAX_GEMMA`), or
/// `None` if it is asked: only when there is something to veto.
fn skip_gemma_reason(prompt: &str, chosen: &[Memoria], from_paths: &BTreeSet<String>) -> Option<&'static str> {
    if prompt.chars().count() > LARGO_MAX_GEMMA {
        Some("long")
    } else if prompt.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count() <= buscar::CORTA {
        // A short message is keywords: there meaning falls short and the veto removed the
        // right note (from 94 % to 88 % in the benchmark) without removing noise.
        Some("short")
    } else if !chosen.iter().any(|m| vetoable(m, from_paths)) {
        Some("nothing_to_veto")
    } else {
        None
    }
}

/// What the model can remove: what doesn't come from a path in the message and isn't a preference. A
/// preference applies without resembling the message («lamina en Creality Print, no en Orca» with «ábrelo
/// en creality»): in Marc's real messages, 22-27 % of the vetoed preferences were useful,
/// versus 6-7 % of the facts (`4-valor.md`).
fn vetoable(m: &Memoria, from_paths: &BTreeSet<String>) -> bool {
    !from_paths.contains(&m.id) && m.kind != memoria::Tipo::Preference
}

type GemmaReply = std::result::Result<crate::significado::Cosenos, crate::significado::Fallo>;

/// Asks the meaning service for the memories in scope most similar to the message (and the cosine of
/// `ids`), within `GEMMA_BUDGET`.
fn ask_gemma(db: &Connection, project: &str, prompt: &str, ids: &[String]) -> Result<GemmaReply> {
    use crate::significado as s;
    let Some(path) = db.path().filter(|r| !r.is_empty()).map(std::path::PathBuf::from) else { return Ok(Err(s::Fallo::Apagado)) };
    let scope = Ambito::de(db, project)?;
    Ok(s::parecidas_o_fallo(&path, prompt, &scope, None, SIMILAR_FOR_RECALL, ids, GEMMA_BUDGET).map(|p| {
        let mut v = p.top;
        for x in p.pedidas {
            if !v.iter().any(|y| y.0 == x.0) {
                v.push(x);
            }
        }
        v
    }))
}

/// Records in the database (`meta`, local) each recall query to the model and those that got no
/// answer, with the last one and its reason, so that `omoi doctor` shows it: without an answer in time the
/// recall goes by words only, and that must not happen silently. Recall never fails because of this.
fn record_model_query(db: &Connection, failure: Option<crate::significado::Fallo>) {
    let increment = "INSERT INTO meta(k, v) VALUES (?1, '1') ON CONFLICT(k) DO UPDATE SET v = CAST(v AS INTEGER) + 1";
    let _ = db.execute(increment, ["sig_recall_consultas"]);
    if let Some(f) = failure {
        let _ = db.execute(increment, ["sig_recall_sin_respuesta"]);
        let _ = db.execute(
            &format!("INSERT OR REPLACE INTO meta(k, v) VALUES ('sig_recall_ultimo_fallo', {} || ' ' || ?1)", memoria::AHORA),
            [f.como_texto()],
        );
    }
}

/// How many times recall asked the model in this database, how many got no answer and the last one that
/// didn't (date and reason).
pub fn consultas_al_modelo(db: &Connection) -> (i64, i64, Option<String>) {
    let read = |k: &str| db.query_row("SELECT v FROM meta WHERE k = ?1", [k], |row| row.get::<_, String>(0)).ok();
    let count = |k: &str| read(k).and_then(|v| v.parse().ok()).unwrap_or(0);
    (count("sig_recall_consultas"), count("sig_recall_sin_respuesta"), read("sig_recall_ultimo_fallo"))
}

pub fn al_preguntar(db: &mut Connection, project: &str, e: &Entrada, model: Option<&Modelo>) -> Result<Option<String>> {
    al_preguntar_abriendo(db, project, e, &mut || model)
}

/// `al_preguntar`, opening the model only if needed (`model` is called at most once): most
/// messages don't reach the meaning channel, and opening it costs a few milliseconds.
pub fn al_preguntar_abriendo<'m>(
    db: &mut Connection,
    project: &str,
    e: &Entrada,
    model: &mut dyn FnMut() -> Option<&'m Modelo>,
) -> Result<Option<String>> {
    let current = current_project(db, project);
    let own = own_author(db, current);
    let seen = already_injected(db, &e.session_id)?;
    // Contradicted ones wait for the person to say which holds (`veredictos::en_espera`): they don't come
    // on their own; the one contradicting them does, with its note.
    let on_hold = crate::veredictos::en_espera(db)?;
    let mut chosen: Vec<Memoria> = Vec::new();
    let procedure = matching_procedure(db, project, &e.prompt)?.filter(|m| !seen.contains(&m.id) && !on_hold.contains(&m.id));
    let choose = |m: Memoria, chosen: &mut Vec<Memoria>| {
        if !seen.contains(&m.id) && !on_hold.contains(&m.id) && !chosen.iter().any(|x| x.id == m.id) && procedure.as_ref().is_none_or(|r| r.id != m.id) && chosen.len() < max_count() {
            chosen.push(m);
        }
    };
    // The frequencies of the message's words, once for words and meaning.
    let mut terms = utiles(&e.prompt);
    let mut freqs = if terms.len() >= TERMINOS_MIN { Some(frequencies(db, project, &terms)?) } else { None };
    // A misdictated project name («trova» for Troba) is in no memory: search by the
    // right name (`nombres::mal_escritos`). Names are only checked if there are words that are
    // in no memory.
    if let Some((_, df)) = &freqs
        && df.contains(&0.0)
    {
        let in_none: Vec<bool> = df.iter().map(|d| *d == 0.0).collect();
        // Only the projects visible from here: another client's name never gets into the message.
        let fixes = crate::nombres::mal_escritos(db, &Ambito::de(db, project)?, &terms, &in_none)?;
        if !fixes.is_empty() {
            trace("nombres", || serde_json::json!(fixes.iter().map(|(i, p)| (terms[*i].clone(), p.join(" "))).collect::<Vec<_>>()));
            for (i, parts) in fixes.iter().rev() {
                terms.splice(*i..*i + 1, parts.iter().filter(|p| p.chars().count() >= 3).cloned());
            }
            terms.sort();
            terms.dedup();
            freqs = Some(crate::recall::frequencies(db, project, &terms)?);
        }
    }
    let by_words = match &freqs {
        Some(f) => relevant_with(db, project, &terms, f)?,
        None => Vec::new(),
    };
    let by_path = por_rutas(db, project, &e.prompt)?;
    let from_paths: BTreeSet<String> = by_path.iter().map(|m| m.id.clone()).collect();
    for m in by_path.into_iter().chain(by_words.into_iter().map(|x| x.0)) {
        choose(m, &mut chosen);
    }
    // The meaning channel goes last: if paths and words already fill the quota, what it brings
    // wouldn't get in and there's no need to look at it (or read the vectors).
    if chosen.len() < max_count()
        && checks_meaning(&e.prompt)
        && let Some(model) = model()
    {
        for m in by_meaning_with(db, project, &e.prompt, model, freqs.as_ref())? {
            choose(m, &mut chosen);
        }
    }
    trace("antes_gemma", || serde_json::json!({"elegidas": chosen.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "rutas": from_paths, "mira": checks_meaning(&e.prompt)}));
    // The big model, only to veto (policy D) and after the words.
    let cosines = match skip_gemma_reason(&e.prompt, &chosen, &from_paths) {
        Some(reason) => {
            trace("gemma", || serde_json::json!({"asked": false, "why": reason}));
            None
        }
        None if !crate::significado::en_uso(db) => {
            trace("gemma", || {
                let state = crate::significado::estado(db);
                serde_json::json!({"asked": false, "why": "not_in_use", "state": state, "installed": crate::significado::modelo().is_some()})
            });
            None
        }
        None => {
            let ids: Vec<String> = chosen.iter().map(|m| m.id.clone()).collect();
            let r = ask_gemma(db, project, &e.prompt, &ids)?;
            record_model_query(db, r.as_ref().err().copied());
            match r {
                Ok(c) => {
                    trace("gemma", || serde_json::json!({"asked": true, "cosines": c}));
                    Some(c)
                }
                Err(f) => {
                    // No answer (busy CPU, service starting or indexing): words only, as
                    // in 1.1. It is recorded for `omoi doctor`.
                    trace("gemma", || serde_json::json!({"asked": true, "failed": f.como_texto()}));
                    None
                }
            }
        }
    };
    if let Some(cosines) = cosines {
        let cosine_of: std::collections::HashMap<&str, f32> = cosines.iter().map(|(id, c)| (id.as_str(), *c)).collect();
        let top: BTreeSet<&str> = cosines.iter().take(VETO_PUESTO).map(|(id, _)| id.as_str()).collect();
        // Without an up-to-date vector (just saved or changed, not computed yet) it can't be judged: it stays.
        let no_vector = |id: &str| -> bool {
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_sig s JOIN memory m ON m.id = s.id AND m.content_hlc = s.hlc WHERE s.id = ?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
        };
        chosen.retain(|m| !vetoable(m, &from_paths) || top.contains(m.id.as_str()) || no_vector(&m.id));
        if let [(id, c1), (_, c2), ..] = cosines.as_slice()
            && *c1 >= ANADIR_GEMMA
            && c1 - c2 >= VENTAJA_GEMMA
        {
            // With the quota full, it replaces the least similar one (except those from paths).
            if chosen.len() >= max_count() && !chosen.iter().any(|m| m.id == *id) {
                let worst = chosen
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !from_paths.contains(&m.id))
                    .map(|(i, m)| (i, cosine_of.get(m.id.as_str()).copied().unwrap_or(0.0)))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, c)) = worst
                    && c < *c1
                {
                    chosen.remove(i);
                }
            }
            choose(memoria::obtener(db, id)?, &mut chosen);
        }
    }
    trace("final", || serde_json::json!(chosen.iter().map(|m| m.id.clone()).collect::<Vec<_>>()));
    // A memory linked with the chosen ones, if it is a trap or a preference and is related (7.4).
    let neighbour = if chosen.is_empty() { None } else { crate::enlaces::vecina_para(db, project, &e.prompt, &chosen, &seen, LISTON_ENLAZADA, LISTON_VECINA)? };
    let remind = reminder_due(db, project, &e.session_id)?;
    // The history of the files the message names, taken from the journal (`riesgo`).
    // Optional, like the documentation: if it fails, recall goes on without it.
    let history = crate::riesgo::al_preguntar(db, project, &e.session_id, &e.prompt).unwrap_or_else(|x| {
        eprintln!("omoikane: history: {x:#}");
        None
    });
    let docs = crate::documentos::al_preguntar(db, project, &e.session_id, &e.prompt);
    if chosen.is_empty() && !remind && procedure.is_none() && history.is_none() && docs.is_none() {
        return Ok(None);
    }
    let mut s = String::from("<omoikane>\n");
    if let Some(r) = &procedure {
        let steps: String = r.content.chars().take(PROCEDURE_LEN).collect();
        s.push_str(&format!(
            "A saved procedure for this (id {}): follow it step by step as a checklist; if a step fails or has changed, `update` the procedure with what you learned.\n### {}\n{steps}\n",
            r.id.chars().take(12).collect::<String>(),
            r.title
        ));
    }
    if !chosen.is_empty() {
        let notes = crate::revisar::notas(db, &Ambito::de(db, project)?, &chosen.iter().collect::<Vec<_>>())?;
        s.push_str("Memories from previous sessions that may be relevant (`get` an id for the whole memory):\n");
        for m in &chosen {
            s.push_str(&line(m, current, own.as_deref()));
            s.push_str(&note(&notes, m));
            s.push('\n');
        }
        if let Some((n, from)) = &neighbour {
            s.push_str(&format!("{} (linked from {})\n", line(n, current, own.as_deref()), from.chars().take(12).collect::<String>()));
        }
    }
    if let Some(h) = &history {
        s.push_str(h);
        s.push('\n');
    }
    if let Some(d) = &docs {
        s.push_str(d);
    }
    if remind {
        s.push_str(RECORDATORIO);
        s.push('\n');
    }
    s.push_str("</omoikane>");
    let ids: Vec<String> = chosen.iter().chain(&procedure).chain(neighbour.iter().map(|(n, _)| n)).map(|m| m.id.clone()).collect();
    record_injected(db, &e.session_id, &ids, current.max(0))?;
    Ok(Some(s))
}

/// Counts the message and says whether it is time to remind the agent to save: a session of at least
/// `MINUTOS_ANTES_DE_RECORDAR` and `MENSAJES_ANTES_DE_RECORDAR` messages, no project memory
/// (or global one) saved or changed since it started, and no other reminder in the last
/// `MINUTOS_ENTRE_RECORDATORIOS`. A session that didn't go through `SessionStart` (hooks half
/// added) starts counting here. Via `memory_updated`: the ones changed since the session started are
/// few, and through the scope index SQLite read all the project's ones (25 ms with 100,000).
fn reminder_due(db: &Connection, project: &str, session: &str) -> Result<bool> {
    if session.is_empty() {
        return Ok(false);
    }
    let now = memoria::AHORA;
    db.execute(
        &format!(
            "INSERT INTO session(session, project, started_at, prompts) VALUES (?1, ?2, {now}, 1)
             ON CONFLICT(session) DO UPDATE SET prompts = session.prompts + 1"
        ),
        params![session, project],
    )?;
    let due: bool = db.query_row(
        &format!(
            "SELECT s.prompts >= ?2
                    AND s.started_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3)
                    AND (s.reminded_at IS NULL OR s.reminded_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?4))
                    AND NOT EXISTS (SELECT 1 FROM memory m INDEXED BY memory_updated WHERE {} AND m.updated_at >= s.started_at)
             FROM session s WHERE s.session = ?1",
            Ambito::de(db, project)?.sql()
        ),
        params![
            session,
            MENSAJES_ANTES_DE_RECORDAR,
            format!("-{MINUTOS_ANTES_DE_RECORDAR} minutes"),
            format!("-{MINUTOS_ENTRE_RECORDATORIOS} minutes"),
        ],
        |row| row.get(0),
    )?;
    if due {
        db.execute(&format!("UPDATE session SET reminded_at = {now} WHERE session = ?1"), [session])?;
    }
    Ok(due)
}

/// The hook's answer for Claude Code.
pub fn salida(event: &str, context: &str) -> String {
    salida_en(Formato::Claude, event, context)
}

/// The hook's answer in the agent's format. `event` is Claude Code's
/// (`SessionStart` or `UserPromptSubmit`).
pub fn salida_en(fmt: Formato, event: &str, context: &str) -> String {
    match fmt {
        Formato::Claude => serde_json::json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": context } }),
        Formato::Gemini => {
            let event = if event == "UserPromptSubmit" { "BeforeAgent" } else { event };
            serde_json::json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": context } })
        }
        Formato::Cursor => serde_json::json!({ "additional_context": context }),
        Formato::Copilot => serde_json::json!({ "additionalContext": context }),
        Formato::Antigravity => serde_json::json!({ "injectSteps": [{ "systemMessage": { "systemMessage": context } }] }),
        Formato::Plain => return context.to_string(),
    }
    .to_string()
}

/// How many memories have already been injected in a session.
pub fn inyectadas(db: &Connection, session: &str) -> Result<usize> {
    Ok(db.query_row("SELECT count(*) FROM recall_log WHERE session = ?1", [session], |row| row.get::<_, i64>(0))? as usize)
}
