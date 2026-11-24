//! Secrets: what must not get into memory (Phase 8.2, item 4). Tokens with a known prefix
//! (GitHub, OpenAI, Anthropic, AWS, Slack, GitLab, Google, Stripe), PEM private keys, JWT, URLs with
//! `user:password@`, anything marked `<private>…</private>` (like engram, for those coming from there),
//! passwords typed by hand («password: hunter2», «la contraseña del wifi es Xy7#k2») and long
//! strings that look random. Each finding is replaced by `[redacted:<type>]`.
//!
//! No regular expressions and no model: hand-written shapes, like `candidatas`. What matters most is
//! no false positives: a ULID (26 uppercase letters and digits, no lowercase), a git hash (hex, no
//! uppercase), a path, a normal URL, a file name or a camelCase identifier with a digit
//! (`Llama4ForConditionalGeneration`) are not secrets. The «random» string has to be 20 or more long
//! without separators, mix lowercase, uppercase and digits, and switch character class every few
//! characters (an identifier is words: it switches every 4 or more). Calibrated with the 144 real
//! memories and the 1,836 lines of `tests/corpus.txt`: 0 false positives (see `tests/secretos.rs`).
//! The only one the real ones produced, an artifact id in a URL (`claude.ai/artifact/4jDj…`), was
//! removed by the rule that a path segment (what follows `/` or `\`) is a name and not a key.
//!
//! Applied when saving and changing (`memoria`), when exporting in sync (`sync`, for what slipped
//! through before 0.19.0) and as a `secret` warning in Review over what is already there.
//! `OMOIKANE_REDACT=0` turns it off, and only that: an agent can't skip it via a parameter. Recall
//! doesn't touch it.

use serde::Serialize;

/// A piece of the text that looks like a secret: what it looks like and where it is (bytes of the original text).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hallazgo {
    /// `github-token`, `openai-key`, `anthropic-key`, `aws-key`, `slack-token`, `gitlab-token`,
    /// `google-key`, `stripe-key`, `slack-webhook`, `private-key`, `jwt`, `omoikane-sync-code`, `url-credentials`, `private`,
    /// `password` (a password typed by hand: «password: hunter2», «la contraseña es Xy7#…») or
    /// `token` (a long string that looks random).
    #[serde(rename = "type")]
    pub tipo: &'static str,
    pub inicio: usize,
    pub fin: usize,
}

/// Turned off only with `OMOIKANE_REDACT=0`.
pub fn activo() -> bool {
    std::env::var("OMOIKANE_REDACT").ok().is_none_or(|v| v.trim() != "0")
}

/// The text with each finding replaced by `[redacted:<type>]`, and the findings (positions in the
/// original text). Without findings it returns the same text. Running it again changes nothing.
pub fn redactar(text: &str) -> (String, Vec<Hallazgo>) {
    let found = hallazgos(text);
    if found.is_empty() {
        return (text.to_string(), found);
    }
    let mut s = String::with_capacity(text.len());
    let mut i = 0;
    for h in &found {
        s.push_str(&text[i..h.inicio]);
        s.push_str("[redacted:");
        s.push_str(h.tipo);
        s.push(']');
        i = h.fin;
    }
    s.push_str(&text[i..]);
    (s, found)
}

/// The distinct types of a list of findings, in the order they appear.
pub fn tipos(found: &[Hallazgo]) -> Vec<&'static str> {
    tipos_distintos(&found.iter().map(|h| h.tipo).collect::<Vec<_>>())
}

/// The same from the loose types (`Memoria::redacted`, one per finding).
pub fn tipos_distintos(ts: &[&'static str]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for t in ts {
        if !v.contains(t) {
            v.push(t);
        }
    }
    v
}

/// «2 (github-token, token)»: for the CLI and the warnings.
pub fn resumen(ts: &[&'static str]) -> String {
    format!("{} ({})", ts.len(), tipos_distintos(ts).join(", "))
}

/// What looks like a secret, sorted and without overlaps: the one that starts first wins (so `<private>…`
/// and a PEM block take what they contain), and with the same start, the longest.
pub fn hallazgos(text: &str) -> Vec<Hallazgo> {
    let mut v = Vec::new();
    private_tags(text, &mut v);
    pem(text, &mut v);
    jwt(text, &mut v);
    sync_code(text, &mut v);
    url_credentials(text, &mut v);
    slack_webhook(text, &mut v);
    passwords(text, &mut v);
    words(text, &mut v);
    v.sort_by(|a, b| a.inicio.cmp(&b.inicio).then(b.fin.cmp(&a.fin)));
    let mut r: Vec<Hallazgo> = Vec::with_capacity(v.len());
    for h in v {
        if r.last().is_none_or(|u| h.inicio >= u.fin) {
            r.push(h);
        }
    }
    r
}

fn finding(kind: &'static str, start: usize, end: usize) -> Hallazgo {
    Hallazgo { tipo: kind, inicio: start, fin: end }
}

// --- Detectors -------------------------------------------------------------------------------------
//
// Every position computed falls on an ASCII byte or right after one, so they are character
// boundaries and the `&str` can be sliced at them.

/// `<private>…</private>`, tags included. Without a closing tag it isn't touched: someone may be
/// talking about the tag.
fn private_tags(t: &str, v: &mut Vec<Hallazgo>) {
    const OPEN: &str = "<private>";
    const CLOSE: &str = "</private>";
    let mut i = 0;
    while let Some(k) = t[i..].find(OPEN) {
        let a = i + k;
        let Some(c) = t[a + OPEN.len()..].find(CLOSE) else { break };
        let end = a + OPEN.len() + c + CLOSE.len();
        v.push(finding("private", a, end));
        i = end;
    }
}

/// `-----BEGIN … PRIVATE KEY-----` up to its `-----END … PRIVATE KEY-----` (or the end of the text if
/// there isn't one: half a key is still a key).
fn pem(t: &str, v: &mut Vec<Hallazgo>) {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut i = 0;
    while let Some(k) = t[i..].find(BEGIN) {
        let a = i + k;
        let line_end = t[a..].find('\n').map_or(t.len(), |x| a + x);
        if !t[a..line_end].contains("PRIVATE KEY-----") {
            i = a + BEGIN.len();
            continue;
        }
        let end = match t[line_end..].find(END) {
            Some(e) => {
                let e = line_end + e + END.len();
                t[e..].find("-----").map_or(t.len(), |x| e + x + 5)
            }
            None => t.len(),
        };
        v.push(finding("private-key", a, end));
        i = end;
    }
}

fn is_b64url(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

/// How far, from `from`, the run of bytes matching `class` goes.
fn run_end(b: &[u8], from: usize, class: fn(u8) -> bool) -> usize {
    let mut j = from;
    while j < b.len() && class(b[j]) {
        j += 1;
    }
    j
}

/// A JWT: `eyJ…`, dot, `eyJ…`, dot, signature; all three parts 10 or more long. Header and payload are
/// base64url JSON, and JSON starts with `{"`, which encoded is `eyJ`.
fn jwt(t: &str, v: &mut Vec<Hallazgo>) {
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find("eyJ") {
        let a = i + k;
        if a > 0 && is_b64url(b[a - 1]) {
            i = a + 3;
            continue;
        }
        let p1 = run_end(b, a, is_b64url);
        if p1 - a >= 10 && b.get(p1) == Some(&b'.') && t[p1 + 1..].starts_with("eyJ") {
            let p2 = run_end(b, p1 + 1, is_b64url);
            if p2 - (p1 + 1) >= 10 && b.get(p2) == Some(&b'.') {
                let p3 = run_end(b, p2 + 1, is_b64url);
                if p3 - (p2 + 1) >= 10 {
                    v.push(finding("jwt", a, p3));
                    i = p3;
                    continue;
                }
            }
        }
        i = a + 3;
    }
}

/// A code from `omoi sync code` (`omoi1.` and base64url): it carries the key of a memory repository.
/// An agent with no person in front has it in `OMOIKANE_SYNC_CODE` and might want to note it down.
fn sync_code(t: &str, v: &mut Vec<Hallazgo>) {
    const PREFIX: &str = "omoi1.";
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find(PREFIX) {
        let a = i + k;
        let end = run_end(b, a + PREFIX.len(), is_b64url);
        if (a == 0 || !is_b64url(b[a - 1])) && end - (a + PREFIX.len()) >= 40 {
            v.push(finding("omoikane-sync-code", a, end));
            i = end;
        } else {
            i = a + PREFIX.len();
        }
    }
}

/// `scheme://user:password@host`: `user:password` is redacted. User only (`ssh://git@…`) is not
/// a secret, and neither is `host:port` without `@`.
fn url_credentials(t: &str, v: &mut Vec<Hallazgo>) {
    let mut i = 0;
    while let Some(k) = t[i..].find("://") {
        let start = i + k + 3;
        let end = t[start..].find(|c: char| c.is_whitespace() || "/?#\"'<>()[]`,;".contains(c)).map_or(t.len(), |x| start + x);
        let authority = &t[start..end];
        if let Some(at) = authority.rfind('@')
            && let Some((user, password)) = authority[..at].split_once(':')
            && !user.is_empty()
            && !password.is_empty()
        {
            v.push(finding("url-credentials", start, start + at));
        }
        i = end.max(start);
    }
}

/// `hooks.slack.com/services/T…/B…/…`: whoever has the URL writes to the channel. The path is redacted.
fn slack_webhook(t: &str, v: &mut Vec<Hallazgo>) {
    const BASE: &str = "hooks.slack.com/services/";
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find(BASE) {
        let start = i + k + BASE.len();
        let end = run_end(b, start, |c| c.is_ascii_alphanumeric() || c == b'/');
        if end - start >= 20 && t[start..end].matches('/').count() >= 2 {
            v.push(finding("slack-webhook", start, end));
        }
        i = end.max(start);
    }
}

/// Words that name a password, in ASCII lowercase (uppercase `Ñ` isn't lowered by
/// `to_ascii_lowercase`, that's why it is separate), and whether they count in prose («la contraseña es …»).
/// `pass` and `pwd` only with `=` (`pass=…`, `PWD=…`): with `:` or in prose they are something else («Tests pass: 45/45»).
const PASSWORD_NAMES: &[(&str, bool)] = &[
    ("password", true),
    ("passwd", true),
    ("passphrase", true),
    ("contraseña", true),
    ("contraseÑa", true),
    ("contrasena", true),
    ("pass", false),
    ("pwd", false),
];

/// What may go between the word and the verb in prose («la contraseña del wifi es …», «the password
/// for root is …»): only if the first word is one of these, and up to 5 words.
const PASSWORD_LINKS: &[&str] = &["de", "del", "of", "for", "para"];
/// The prose verb: «la contraseña es …», «the password is …».
const PASSWORD_VERBS: &[&str] = &["es", "is", "era", "was"];

/// Letter or digit, counting as a letter any byte of a non-ASCII character («ñ», «é»).
fn is_letter_or_digit(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c >= 0x80
}

/// Passwords typed by hand: a word that names one followed by `:` or `=` (`password: hunter2`,
/// `DB_PASSWORD=Xy7#k2`, `"password": "…"`, `pass=…`) or, in prose, by «es»/«is» («la contraseña es
/// Xy7#k2», «the wifi password is hunter2», «la contraseña del router es …»). Only the value is
/// redacted. To avoid taking normal prose for a password («la contraseña se guarda en el llavero»,
/// «password reset flow», «the password is required») the value has to look like one: see
/// `looks_like_password`.
fn passwords(t: &str, v: &mut Vec<Hallazgo>) {
    let l = t.to_ascii_lowercase(); // the same bytes at the same positions
    let b = l.as_bytes();
    for &(name, in_prose) in PASSWORD_NAMES {
        let mut i = 0;
        while let Some(k) = l[i..].find(name) {
            let a = i + k;
            let mut j = a + name.len();
            i = j;
            // Whole word after it («passwords», «passing», «contraseñas», `password_hash`, `--password-stdin` no).
            // Before it, whole or after `_` (`DB_PASSWORD`); `password` and `passwd` also glued to
            // another word (`PGPASSWORD=`), but then only with `=` or `:`.
            if j < b.len() && (is_letter_or_digit(b[j]) || matches!(b[j], b'_' | b'-')) {
                continue;
            }
            let glued = a > 0 && is_letter_or_digit(b[a - 1]);
            if glued && !matches!(name, "password" | "passwd") {
                continue;
            }
            // `--password` of a command: with `=` or with the value after it (`--password Xy7#k2`); after a
            // single dash it's something else (`no-password`).
            let option = a >= 2 && b[a - 2..a] == *b"--"; // by bytes: what's before may be a multi-byte character (`—password`)
            if a > 0 && b[a - 1] == b'-' && !option {
                continue;
            }
            if option && b.get(j) == Some(&b' ') {
                let start = run_end(b, j, |c| c == b' ');
                if let Some((start, end)) = password_value(t, start)
                    && looks_like_password(&t[start..end], false)
                {
                    v.push(finding("password", start, end));
                    i = end;
                }
                continue;
            }
            // The quote that closes a key (`"password": …`) and spaces.
            if j < b.len() && matches!(b[j], b'"' | b'\'') {
                j += 1;
            }
            while j < b.len() && matches!(b[j], b' ' | b'\t') {
                j += 1;
            }
            let prose = match b.get(j) {
                Some(b':') if !matches!(name, "pass" | "pwd") => {
                    match b.get(j + 1) {
                        Some(b':') => continue, // `Password::new`
                        Some(b'=') => j += 1,   // `:=`
                        _ => {}
                    }
                    j += 1;
                    false
                }
                Some(b'=') => {
                    if b.get(j + 1) == Some(&b'=') || matches!(b[j - 1], b'!' | b'<' | b'>') {
                        continue; // a comparison
                    }
                    if b.get(j + 1) == Some(&b'>') {
                        j += 1; // `"password" => "…"`
                    }
                    j += 1;
                    false
                }
                _ if in_prose && !glued => {
                    let Some(verb_end) = password_verb(&l, j) else { continue };
                    j = verb_end;
                    if b.get(j) == Some(&b':') {
                        j += 1;
                    }
                    true
                }
                _ => continue,
            };
            while j < b.len() && matches!(b[j], b' ' | b'\t') {
                j += 1;
            }
            if let Some((start, end)) = password_value(t, j)
                && looks_like_password(&t[start..end], prose)
            {
                v.push(finding("password", start, end));
                i = end;
            }
        }
    }
}

/// From `j` (after the word and its spaces), where «es»/«is» ends, first skipping a «del wifi» or
/// a «for root» (up to 5 words, the first from `PASSWORD_LINKS`: «de la base de datos»). `l` is lowercase.
fn password_verb(l: &str, mut j: usize) -> Option<usize> {
    let b = l.as_bytes();
    let mut linked = false;
    for _ in 0..=5 {
        let f = run_end(b, j, |c| is_letter_or_digit(c) || c == b'_' || c == b'-');
        let w = &l[j..f];
        if w.is_empty() {
            return None;
        }
        if PASSWORD_VERBS.contains(&w) && b.get(f).is_none_or(|c| matches!(c, b' ' | b'\t' | b':')) {
            return Some(f);
        }
        if !linked && !PASSWORD_LINKS.contains(&w) {
            return None;
        }
        linked = true;
        if b.get(f) != Some(&b' ') {
            return None;
        }
        j = f + 1;
    }
    None
}

/// The value starting at `j`: in quotes (on the same line), or up to the first space and without
/// the sign that ends the sentence («… es hunter2.»).
fn password_value(t: &str, j: usize) -> Option<(usize, usize)> {
    let b = t.as_bytes();
    let c = *b.get(j)?;
    if matches!(c, b'"' | b'\'' | b'`') {
        let end = t[j + 1..].find([c as char, '\n']).map(|x| j + 1 + x)?;
        return (b[end] == c).then_some((j + 1, end));
    }
    if t[j..].starts_with('«') {
        let start = j + '«'.len_utf8();
        let end = t[start..].find(['»', '\n']).map(|x| start + x)?;
        return t[end..].starts_with('»').then_some((start, end));
    }
    let mut end = t[j..].find(char::is_whitespace).map_or(t.len(), |x| j + x);
    // Closing typographic quotes («“password: x1”») and the final punctuation are not part
    // of the value, in any order («“password: x1”.»).
    while let Some(c) = t[j..end].chars().next_back().filter(|c| matches!(c, '»' | '”' | '’' | '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\'')) {
        end -= c.len_utf8();
    }
    (end > j).then_some((j, end))
}

/// Whether what follows «password:» or «la contraseña es» looks like a password, and not a word of the
/// sentence, a blank to fill in or code: 4 to 128 bytes, with some digit or some symbol (a lone word
/// like «required» or «segura» doesn't count; «swordfish» is missed, in exchange for no false
/// positives), and not being `$VAR`, `%VAR%`, `<yours>`, `${{ secrets.X }}`, `****`, a path,
/// `os.environ["X"]`, `getpass()` or `config.db_password`. In prose, also, not ending in a
/// compound word («SHA256-hashed», «bcrypt-based»).
fn looks_like_password(w: &str, prose: bool) -> bool {
    let b = w.as_bytes();
    if b.len() < 4 || b.len() > 128 || w.contains('\n') {
        return false;
    }
    if matches!(b[0], b'$' | b'%' | b'<' | b'{' | b'[' | b'(' | b'*' | b'/' | b'~' | b'.' | b'#' | b'-' | b'\\') || w.starts_with('•') || w.starts_with('…') {
        return false;
    }
    if w.contains("${") || w.contains("{{") || w.contains('(') || w.contains("[\"") || w.contains("['") {
        return false;
    }
    if b.iter().all(|&c| c == b[0]) {
        return false;
    }
    // `config.db_password`, `settings.PASSWORD`: a dotted name, without digits.
    if w.contains('.') && b.iter().all(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'.') {
        return false;
    }
    let digit = b.iter().any(u8::is_ascii_digit);
    let symbol = b.iter().any(|&c| c.is_ascii_punctuation() && !matches!(c, b'_' | b'-' | b'.' | b'\'' | b'"' | b','));
    if !digit && !symbol {
        return false;
    }
    if prose
        && let Some((_, tail)) = w.rsplit_once('-')
        && tail.len() >= 4
        && tail.bytes().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    true
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn is_alnum_or_underscore(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_alnum_or_dash(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

fn is_upper_or_digit(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

/// A known prefix at the start of a word: (prefix, type, what may follow it, how much must follow
/// it at least). Real lengths are well above the minimum.
type Prefix = (&'static str, &'static str, fn(u8) -> bool, usize);

const PREFIXES: &[Prefix] = &[
    ("github_pat_", "github-token", is_alnum_or_underscore, 20),
    ("ghp_", "github-token", is_alnum_or_underscore, 20),
    ("gho_", "github-token", is_alnum_or_underscore, 20),
    ("ghu_", "github-token", is_alnum_or_underscore, 20),
    ("ghs_", "github-token", is_alnum_or_underscore, 20),
    ("ghr_", "github-token", is_alnum_or_underscore, 20),
    ("sk-ant-", "anthropic-key", is_b64url, 20),
    ("sk-", "openai-key", is_b64url, 20),
    ("sk_live_", "stripe-key", is_alnum_or_underscore, 20),
    ("sk_test_", "stripe-key", is_alnum_or_underscore, 20),
    ("rk_live_", "stripe-key", is_alnum_or_underscore, 20),
    ("rk_test_", "stripe-key", is_alnum_or_underscore, 20),
    ("xoxa-", "slack-token", is_alnum_or_dash, 10),
    ("xoxb-", "slack-token", is_alnum_or_dash, 10),
    ("xoxp-", "slack-token", is_alnum_or_dash, 10),
    ("xoxs-", "slack-token", is_alnum_or_dash, 10),
    ("xoxr-", "slack-token", is_alnum_or_dash, 10),
    ("glpat-", "gitlab-token", is_b64url, 20),
    ("AIza", "google-key", is_b64url, 30),
];

/// Word by word (runs of letters, digits, `_` and `-`): known prefixes at the start of the
/// word, AWS keys, and the general rule of the string that looks random.
fn words(t: &str, v: &mut Vec<Hallazgo>) {
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !is_word_byte(b[i]) {
            i += 1;
            continue;
        }
        let end = run_end(b, i, is_word_byte);
        let word = &t[i..end];
        if let Some((prefix, kind, class, min)) = PREFIXES.iter().find(|(p, ..)| word.starts_with(p)) {
            let rest_end = run_end(b, i + prefix.len(), *class);
            let rest = &t[i + prefix.len()..rest_end];
            // An OpenAI or Google key always has digits; a bare «sk-» before words, no.
            let has_digit = !matches!(*kind, "openai-key" | "google-key") || rest.bytes().any(|c| c.is_ascii_digit());
            if rest.len() >= *min && has_digit {
                v.push(finding(kind, i, rest_end));
                i = end;
                continue;
            }
        }
        // AWS access key: AKIA (or ASIA, temporary) and 16 uppercase letters or digits, exactly 20.
        if (word.starts_with("AKIA") || word.starts_with("ASIA")) && word.len() >= 20 {
            let key_end = run_end(b, i, is_upper_or_digit);
            if key_end - i == 20 && (key_end == end || matches!(b[key_end], b'-' | b'_')) {
                v.push(finding("aws-key", i, key_end));
                i = end;
                continue;
            }
        }
        // A path segment (`claude.ai/artifact/4jDjbrNkj3ra1V9PqHx5eD`, `C:\x\…`) is a name, not
        // a key: artifact, video or gist ids look random and are shown on purpose. What is
        // secret in a path has a known shape (the Slack webhook, separately).
        if i > 0 && matches!(b[i - 1], b'/' | b'\\') {
            i = end;
            continue;
        }
        // LinkedIn member ids (`ACoAA…`, and `ACwAA…` in Sales Navigator) look random but
        // are a profile's public id: they appear in its URLs (`/in/ACoAA…`, `?rcm=ACoAA…`, the parameter
        // LinkedIn uses to mark who a recommendation comes from). Same for the value of any `rcm=`.
        if word.starts_with("ACoAA") || word.starts_with("ACwAA") || t[..i].ends_with("rcm=") {
            i = end;
            continue;
        }
        // General rule, by chunks without `_` or `-` («potion-multilingual-128M-int8» are words).
        let mut chunk_start = i;
        for (k, c) in b[i..end].iter().enumerate() {
            let pos = i + k;
            if *c == b'_' || *c == b'-' {
                if parece_aleatoria(&t[chunk_start..pos]) {
                    v.push(finding("token", chunk_start, pos));
                }
                chunk_start = pos + 1;
            }
        }
        if parece_aleatoria(&t[chunk_start..end]) {
            v.push(finding("token", chunk_start, end));
        }
        i = end;
    }
}

/// A string of 20 or more letters and digits without separators that mixes lowercase, uppercase and
/// digits and switches character class every less than 3.5 characters on average. A camelCase
/// identifier (`Llama4ForConditionalGeneration`: Llama·4·For·Conditional·Generation, 6 per chunk)
/// doesn't switch that much; a token (`q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0`, 26 chunks in 33) does. An
/// uppercase letter followed by lowercase counts as a single word. Without lowercase (a ULID) or without uppercase (a hash) it doesn't count.
pub fn parece_aleatoria(w: &str) -> bool {
    let b = w.as_bytes();
    if b.len() < 20 || !b.iter().all(u8::is_ascii_alphanumeric) {
        return false;
    }
    let (mut lower, mut upper, mut digits) = (false, false, false);
    let mut chunks = 0usize;
    let mut previous = 0u8; // 1 lowercase, 2 uppercase, 3 digit
    for &c in b {
        let class = if c.is_ascii_lowercase() {
            lower = true;
            1
        } else if c.is_ascii_uppercase() {
            upper = true;
            2
        } else {
            digits = true;
            3
        };
        if class != previous && !(previous == 2 && class == 1) {
            chunks += 1;
        }
        previous = class;
    }
    lower && upper && digits && chunks * 7 >= b.len() * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_and_positions() {
        let (s, found) = redactar("token ghp_abcdefghijklmnopqrstuvwxyz0123456789 y ya");
        assert_eq!(s, "token [redacted:github-token] y ya");
        assert_eq!(found.len(), 1);
        assert_eq!(&"token ghp_abcdefghijklmnopqrstuvwxyz0123456789 y ya"[found[0].inicio..found[0].fin], "ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(redactar(&s).0, s);
    }

    #[test]
    fn sync_code_is_redacted() {
        // A whole `omoi sync code` code: `omoi1.` and the JSON in base64url (starts with `eyJ`).
        let code = "omoi1.eyJ1cmwiOiJnaXRAeDp5L3ouZ2l0Iiwia2V5IjoiMDAxMTIyMzM0NDU1NjY3Nzg4OTlhYWJiY2NkZGVlZmYifQ";
        let (s, found) = redactar(&format!("OMOIKANE_SYNC_CODE={code} en la CI"));
        assert_eq!(s, "OMOIKANE_SYNC_CODE=[redacted:omoikane-sync-code] en la CI");
        assert_eq!(tipos(&found), vec!["omoikane-sync-code"]);
        // Talking about the format is not a secret.
        assert!(hallazgos("los códigos empiezan por omoi1. y llevan la clave").is_empty());
    }

    #[test]
    fn the_one_that_starts_first_wins() {
        let (s, found) = redactar("<private>clave ghp_abcdefghijklmnopqrstuvwxyz0123456789</private> fin");
        assert_eq!(s, "[redacted:private] fin");
        assert_eq!(tipos(&found), vec!["private"]);
    }

    #[test]
    fn random_versus_identifier() {
        assert!(parece_aleatoria("q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0"));
        assert!(!parece_aleatoria("Llama4ForConditionalGeneration"));
        assert!(!parece_aleatoria("01K8ZQ4V9N3X2M7P6R5T8W1Y0A")); // ULID
        assert!(!parece_aleatoria("3f9a1c2b4d5e6f7a8b9c0d1e2f3a4b5c")); // hash
        assert!(!parece_aleatoria("corto1A"));
    }

    /// 1.8.6: what comes before «password» may be a multi-byte character; checking whether it is `--`
    /// must not cut it in half.
    #[test]
    fn password_after_multibyte_character() {
        assert!(hallazgos("—password x1").is_empty());
        assert!(hallazgos("x…password: abc").is_empty());
        assert_eq!(redactar("“password: x1”").0, "“password: x1”");
        assert_eq!(redactar("“password: Xy7#k2”").0, "“password: [redacted:password]”");
        assert_eq!(redactar("“password: Xy7#k2”.").0, "“password: [redacted:password]”.");
        assert_eq!(redactar("(password: Xy7#k2”),").0, "(password: [redacted:password]”),");
        assert_eq!(redactar("—password=Xy7#k2").0, "—password=[redacted:password]");
    }
}
