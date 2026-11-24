//! What a memory is based on (1.13, from the evidence log of trycompai/crm). The agent does not score
//! its own certainty ("a model that scores its certainty errs towards whatever looks most useful"): it
//! says what it saw, with a closed kind and a detail a person can read.
//!
//! | Kind | What it is | Detail |
//! |---|---|---|
//! | `user-said` | The user said it | Their words, quoted |
//! | `code-read` | Read in the code | The file (and line) |
//! | `tested` | Checked by running it | What ran and what came out |
//! | `doc` | Read in documentation or on a web page | What it says; the URL in `url` |
//! | `inferred` | The agent worked it out | What it worked it out from |
//!
//! Anything based only on `inferred` weighs less in search ([`INFERRED_FACTOR`]) and is marked
//! "inferred" in lists: the agent receiving it knows nobody has checked it. A memory without
//! evidence (those from before 1.13, imported ones) is left alone.
//!
//! It is not the provenance (`procedencia`: in which session and commit it was written) but the basis
//! of the fact. It goes in `memory.extra` under the key [`KEY`], like provenance: no schema change,
//! it travels with sync and an older version keeps it without understanding it. It is added with
//! `update` (nothing is overwritten and no version is created); two equal details of the same kind count as one.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// The key in `memory.extra`.
pub const KEY: &str = "evidence";
/// At most how many a memory carries (the first ones stay).
pub const MAX: usize = 10;
/// The maximum length of a detail, in characters.
const MAX_DETAIL_LEN: usize = 400;
/// The search weight of a memory based only on what the agent inferred.
pub const INFERRED_FACTOR: f64 = 0.85;

/// The kind of evidence: what whoever saves it saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "Clase")]
pub enum EvidenceKind {
    /// The user said it (quote their words in `detail`).
    UserSaid,
    /// Read in the code (the file, and line, in `detail`).
    CodeRead,
    /// Checked by running or testing it (what ran and what came out).
    Tested,
    /// Read in documentation or on a web page (the URL in `url`).
    Doc,
    /// The agent worked it out (from what, in `detail`).
    Inferred,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::UserSaid => "user-said",
            EvidenceKind::CodeRead => "code-read",
            EvidenceKind::Tested => "tested",
            EvidenceKind::Doc => "doc",
            EvidenceKind::Inferred => "inferred",
        }
    }

    pub fn parse(t: &str) -> Result<EvidenceKind> {
        Ok(match t.trim().to_lowercase().replace('_', "-").as_str() {
            "user-said" | "user" | "said" => EvidenceKind::UserSaid,
            "code-read" | "code" => EvidenceKind::CodeRead,
            "tested" | "test" | "ran" => EvidenceKind::Tested,
            "doc" | "docs" | "web" => EvidenceKind::Doc,
            "inferred" | "inference" => EvidenceKind::Inferred,
            other => bail!(crate::tr!(
                "unknown evidence kind «{other}»: use user-said, code-read, tested, doc or inferred",
                "tipo de evidencia desconocido «{other}»: usa user-said, code-read, tested, doc o inferred"
            )),
        })
    }
}

/// One piece of evidence: what was seen, in a person's words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "Evidencia")]
pub struct Evidence {
    /// What kind of evidence: user-said, code-read, tested, doc or inferred.
    pub kind: EvidenceKind,
    /// What you saw, readable by a person: the user's words, the file and line, the command and its output, what the page says. Not «confirmed».
    pub detail: String,
    /// The page or document, for `doc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Evidence {
    /// On one line: "tested: cargo test, 83 passed".
    pub fn text(&self) -> String {
        match &self.url {
            Some(u) => format!("{}: {} ({u})", self.kind.as_str(), self.detail),
            None => format!("{}: {}", self.kind.as_str(), self.detail),
        }
    }
}

/// Cleans what comes in (detail without extra spaces and truncated, URL only if http/https) and removes
/// duplicates. An empty detail is an error: evidence that doesn't say what was seen is not evidence.
pub fn clean(v: Vec<Evidence>) -> Result<Vec<Evidence>> {
    let mut r: Vec<Evidence> = Vec::new();
    for mut e in v {
        e.detail = e.detail.split_whitespace().collect::<Vec<_>>().join(" ");
        if e.detail.is_empty() {
            bail!(crate::tr!(
                "evidence «{}» without detail: say what you saw (the user's words, the file, the command and its output…)",
                "evidencia «{}» sin detalle: di qué viste (las palabras del usuario, el fichero, la orden y lo que salió…)",
                e.kind.as_str()
            ));
        }
        // A user quote or a command's output may carry a secret, like the content.
        if crate::secretos::activo() {
            e.detail = crate::secretos::redactar(&e.detail).0;
        }
        if e.detail.chars().count() > MAX_DETAIL_LEN {
            e.detail = e.detail.chars().take(MAX_DETAIL_LEN - 1).collect::<String>() + "…";
        }
        e.url = e.url.map(|u| u.trim().to_string()).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
        if !r.iter().any(|x| x.kind == e.kind && x.detail == e.detail) {
            r.push(e);
        }
    }
    Ok(r)
}

/// Those from a column (the JSON of `extra.evidence`); anything not understood, none.
pub(crate) fn from_column(json: Option<&str>) -> Vec<Evidence> {
    json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
}

/// Whether it is based only on what the agent inferred (no evidence doesn't count: unknown).
pub fn only_inferred(v: &[Evidence]) -> bool {
    !v.is_empty() && v.iter().all(|e| e.kind == EvidenceKind::Inferred)
}

/// Its kinds, deduplicated and in order, for lists: "tested, user-said". `None` without evidence.
pub fn kinds(v: &[Evidence]) -> Option<String> {
    let mut t: Vec<&str> = Vec::new();
    for e in v {
        if !t.contains(&e.kind.as_str()) {
            t.push(e.kind.as_str());
        }
    }
    (!t.is_empty()).then(|| t.join(", "))
}

/// Merges the new ones with those already in `extra` (older ones first, no duplicates, up to
/// [`MAX`]). `true` if anything changed.
pub(crate) fn add(extra: &mut serde_json::Map<String, serde_json::Value>, new: &[Evidence]) -> bool {
    let mut v: Vec<Evidence> = extra.get(KEY).and_then(|x| serde_json::from_value(x.clone()).ok()).unwrap_or_default();
    let before = v.len();
    for e in new {
        if v.len() < MAX && !v.iter().any(|x| x.kind == e.kind && x.detail == e.detail) {
            v.push(e.clone());
        }
    }
    if v.len() == before {
        return false;
    }
    extra.insert(KEY.into(), serde_json::to_value(v).unwrap_or_default());
    true
}

/// `--evidence kind:detail` from the CLI (with `doc:https://…` the URL goes in `url`).
pub fn from_cli(t: &str) -> Result<Evidence> {
    let Some((k, d)) = t.split_once(':') else {
        bail!(crate::tr!("--evidence KIND:DETAIL, e.g. --evidence \"tested:cargo test, 83 passed\"", "--evidence TIPO:DETALLE, p. ej. --evidence \"tested:cargo test, 83 passed\""))
    };
    let kind = EvidenceKind::parse(k)?;
    let d = d.trim();
    let (detail, url) = match kind {
        EvidenceKind::Doc if d.starts_with("http://") || d.starts_with("https://") => match d.split_once(char::is_whitespace) {
            Some((u, rest)) => (rest.trim().to_string(), Some(u.to_string())),
            None => (d.to_string(), Some(d.to_string())),
        },
        _ => (d.to_string(), None),
    };
    Ok(Evidence { kind, detail, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(k: EvidenceKind, d: &str) -> Evidence {
        Evidence { kind: k, detail: d.into(), url: None }
    }

    #[test]
    fn cleans_and_removes_duplicates() {
        let v = clean(vec![ev(EvidenceKind::Tested, "  cargo   test "), ev(EvidenceKind::Tested, "cargo test"), ev(EvidenceKind::Inferred, "x")]).unwrap();
        assert_eq!(v, [ev(EvidenceKind::Tested, "cargo test"), ev(EvidenceKind::Inferred, "x")]);
        assert!(clean(vec![ev(EvidenceKind::Doc, "  ")]).is_err());
        let u = clean(vec![Evidence { kind: EvidenceKind::Doc, detail: "d".into(), url: Some("javascript:x".into()) }]).unwrap();
        assert_eq!(u[0].url, None);
    }

    #[test]
    fn only_inferred_false_without_evidence() {
        assert!(!only_inferred(&[]));
        assert!(only_inferred(&[ev(EvidenceKind::Inferred, "a")]));
        assert!(!only_inferred(&[ev(EvidenceKind::Inferred, "a"), ev(EvidenceKind::UserSaid, "b")]));
    }

    #[test]
    fn from_cli_parses_kind_and_detail() {
        assert_eq!(from_cli("tested: cargo test").unwrap(), ev(EvidenceKind::Tested, "cargo test"));
        let d = from_cli("doc:https://x.dev/a dice que sí").unwrap();
        assert_eq!((d.url.as_deref(), d.detail.as_str()), (Some("https://x.dev/a"), "dice que sí"));
        assert!(from_cli("guess:algo").is_err());
        assert!(from_cli("sin tipo").is_err());
    }

    #[test]
    fn add_without_duplicates_or_exceeding_max() {
        let mut extra = serde_json::Map::new();
        assert!(add(&mut extra, &[ev(EvidenceKind::Tested, "a")]));
        assert!(!add(&mut extra, &[ev(EvidenceKind::Tested, "a")]));
        let many: Vec<Evidence> = (0..20).map(|i| ev(EvidenceKind::CodeRead, &format!("f{i}"))).collect();
        add(&mut extra, &many);
        assert_eq!(from_column(Some(&extra[KEY].to_string())).len(), MAX);
    }
}
