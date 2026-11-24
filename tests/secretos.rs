//! 0.19.0, secrets: the detector (`secretos::redactar`) gets 3 seeded secrets of each kind right
//! and gives no false positives with what a memory really holds: ULIDs, git hashes, paths, URLs,
//! memory ids, file names, camelCase identifiers and the 1,836 lines of `corpus.txt`.
//! And through the CLI: `remember` and `update` redact (and say so), `OMOIKANE_REDACT=0` turns it off, Review and
//! `doctor` warn about what was already there, and «Redact» also removes it from the history.
//!
//! `OMOIKANE_AUDITAR_DB=<copy.db> cargo test --test secretos -- --ignored --nocapture` walks a database
//! and shows each finding, to calibrate with real memories (always on a copy).

mod comun;

use omoikane::secretos::{self, hallazgos, tipos};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// The example secrets in these tests carry a `~` in the middle of the prefix («xo~xb-», «gl~pat-»):
/// without it, GitHub's secret scanner takes them for real ones and won't let the repository be pushed. The
/// three functions below remove it before calling the detector, so the whole text is tested.
fn unmark(t: &str) -> String {
    t.replace('~', "")
}

fn redact(t: &str) -> (String, Vec<secretos::Hallazgo>) {
    secretos::redactar(&unmark(t))
}

fn kinds_of(t: &str) -> Vec<&'static str> {
    tipos(&hallazgos(&unmark(t)))
}

fn assert_clean(t: &str) {
    let t = unmark(t);
    let hs = hallazgos(&t);
    assert!(hs.is_empty(), "falso positivo en «{t}»: {:?} → «{}»", hs, secretos::redactar(&t).0);
}

#[test]
fn github() {
    for t in [
        "export GH=gh~p_abcdefghijklmnopqrstuvwxyz0123456789",
        "github~_pat_11ABCDEFG0abcdefghijklmn_opqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijk",
        "fine: gho_16C7e42F292c6912E7710c838347Ae178B4a",
    ] {
        assert_eq!(kinds_of(t), vec!["github-token"], "{t}");
        assert!(!redact(t).0.contains("gh"), "{t}");
    }
    assert_eq!(redact("export GH=gh~p_abcdefghijklmnopqrstuvwxyz0123456789").0, "export GH=[redacted:github-token]");
}

#[test]
fn openai_and_anthropic() {
    for t in [
        concat!("OPENAI_API_KEY=sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34"),
        "la vieja: sk-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90",
        "clave \"sk-1234567890abcdefghijklmnopqrstuvwxyz1234\"",
    ] {
        assert_eq!(kinds_of(t), vec!["openai-key"], "{t}");
    }
    for t in [
        "ANTHROPIC_API_KEY=sk~-ant-api03-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34Yz56Ab78Cd90Ef12Gh34Ij56Kl78Mn90-AbCdEfAA",
        "sk~-ant-admin01-abcdefghijklmnopqrstuvwxyz0123456789",
        "token: sk~-ant-sid01-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdef",
    ] {
        assert_eq!(kinds_of(t), vec!["anthropic-key"], "{t}");
    }
    // A bare «sk-» in front of words is not a key.
    assert_clean("the sk-learn-compatible-estimator-wrapper is fine");
}

#[test]
fn aws() {
    for t in ["AWS_ACCESS_KEY_ID=AK~IAIOSFODNN7EXAMPLE", "key AK~IAJ4X7ZQ2PLM9NB3WE and more", "temporary AS~IAQ3R7T2WXYZ8KLMNP."] {
        assert_eq!(kinds_of(t), vec!["aws-key"], "{t}");
    }
    // 21 characters, or with lowercase, is not an AWS key; «AKIA» inside a word isn't either.
    assert_clean("AK~IAIOSFODNN7EXAMPLEX");
    assert_clean("AK~IAiosfodnn7example");
    assert_clean("the word NAK~IAIOSFODNN7EXAMPLE");
}

#[test]
fn slack_gitlab_google_stripe() {
    for t in ["xo~xb-1234567890-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "xo~xp-12345-67890-abcdef", "xo~xa-2-ABCDEFGHIJ"] {
        assert_eq!(kinds_of(t), vec!["slack-token"], "{t}");
    }
    for t in ["gl~pat-Ab12Cd34Ef56Gh78Ij90", "GITLAB_TOKEN=gl~pat-abcdefghijklmnopqrstuvwxyz", "gl~pat-ZYXWVUTSRQPONMLKJIHGFEDCBA-1"] {
        assert_eq!(kinds_of(t), vec!["gitlab-token"], "{t}");
    }
    for t in ["AI~zaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r", "key=AI~zaSyDaGmWKa4JsXZ-HjGw7ISLn_3namBGewQe", "AI~zaSyB0123456789abcdefghijklmnopqrstuv"] {
        assert_eq!(kinds_of(t), vec!["google-key"], "{t}");
    }
    for t in ["sk~_live_4eC39HqLyjWDarjtT1zdp7dc", "sk~_test_51Hb1234567890abcdefghijkl", "rk~_live_abcdefghijklmnopqrstuvwxyz"] {
        assert_eq!(kinds_of(t), vec!["stripe-key"], "{t}");
    }
}

#[test]
fn slack_webhooks() {
    for t in [
        "https://hooks.sla~ck.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX",
        "curl -X POST hooks.sla~ck.com/services/T0ABCDEF1/B0ABCDEF2/abcdefghijklmnopqrstuvwx -d '{}'",
        "SLACK_WEBHOOK=https://hooks.sla~ck.com/services/TA1B2C3D4/BZ9Y8X7W6/q7XbP2mZr9KdT4wLs8NcV1hY",
    ] {
        assert_eq!(kinds_of(t), vec!["slack-webhook"], "{t}");
    }
    assert_eq!(
        redact("POST https://hooks.sla~ck.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX ya").0,
        "POST https://hooks.slack.com/services/[redacted:slack-webhook] ya"
    );
    assert_clean("la doc está en https://hooks.slack.com/services/ y poco más");
}

#[test]
fn pem_keys() {
    let rsa = "-----BEGIN RSA PRIV~ATE KEY-----\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHIw\nfoo\n-----END RSA PRIV~ATE KEY-----";
    let openssh = "antes\n-----BEGIN OPENSSH PRIV~ATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n-----END OPENSSH PRIV~ATE KEY-----\ndespués";
    let truncated = "-----BEGIN EC PRIV~ATE KEY-----\nMHQCAQEEIBzK... y aquí se cortó";
    for t in [rsa, openssh, truncated] {
        assert_eq!(kinds_of(t), vec!["private-key"], "{t}");
    }
    assert_eq!(redact(openssh).0, "antes\n[redacted:private-key]\ndespués");
    assert_eq!(redact(truncated).0, "[redacted:private-key]");
    // A certificate is not a private key.
    assert_clean("-----BEGIN CERTIFICATE-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA\n-----END CERTIFICATE-----");
}

#[test]
fn jwt() {
    for t in [
        "Authorization: Bearer ey~JhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ey~JzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        "ey~JhbGciOiJSUzI1NiJ9.ey~Jpc3MiOiJvbW9pIn0.abcdefghijklmnop",
        "token=ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4",
    ] {
        assert_eq!(kinds_of(t), vec!["jwt"], "{t}");
    }
    assert_eq!(redact("token=ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4 ok").0, "token=[redacted:jwt] ok");
    // Two parts, or the second without «eyJ», is not a JWT (the lone header, 20 in base64, still
    // looks like a random string, and that's what it is).
    assert!(!kinds_of("ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ").contains(&"jwt"));
    assert!(!kinds_of("ey~JhbGciOiJIUzI1NiJ9.abcdefghijklmnop.Zm9vYmFyYmF6cXV4").contains(&"jwt"));
}

#[test]
fn credentials_in_urls() {
    for t in [
        "git clone https://marc:hunter2@github.com/marcmayol/omoikane.git",
        "postgres://app:S3cr3t!@db.local:5432/omoi",
        "la URL es ftp://user:pass@ftp.example.com/dir y nada más",
    ] {
        assert_eq!(kinds_of(t), vec!["url-credentials"], "{t}");
    }
    assert_eq!(redact("git clone https://marc:hunter2@github.com/marcmayol/omoikane.git").0, "git clone https://[redacted:url-credentials]@github.com/marcmayol/omoikane.git");
    // Just a user, a port or an email are not credentials.
    assert_clean("ssh://git@github.com/marcmayol/omoikane.git");
    assert_clean("http://127.0.0.1:7438/ y http://localhost:7438/#token");
    assert_clean("escribe a marc@example.com o mira https://example.com/a?b=c@d");
}

#[test]
fn engram_private() {
    for t in [
        "la clave es <private>hunter2</private>, no la pierdas",
        "<private>todo esto</private>",
        "dos: <private>a</private> y <private>b</private>",
    ] {
        assert_eq!(kinds_of(t), vec!["private"], "{t}");
    }
    assert_eq!(redact("la clave es <private>hunter2</private>, no la pierdas").0, "la clave es [redacted:private], no la pierdas");
    assert_eq!(redact("dos: <private>a</private> y <private>b</private>").0, "dos: [redacted:private] y [redacted:private]");
    // Without a closing tag, it's talking about the tag.
    assert_clean("engram usa la etiqueta <private> para esto");
}

#[test]
fn random_strings() {
    for t in [
        "echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s",
        "API_SECRET=Zk8pQ2vL5mN1xR7tW3yB9cD4fG6hJ0a",
        "secret: 7Hq2Lp9Rz4Kx1Vb8Nm3Tc5Yw0Fd6Gs",
    ] {
        assert_eq!(kinds_of(t), vec!["token"], "{t}");
    }
    assert_eq!(redact("echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s").0, "echo \"[redacted:token]\" > /tmp/s");
}

#[test]
fn no_false_positives() {
    for t in [
        // ULIDs and memory ids
        "01K8ZQ4V9N3X2M7P6R5T8W1Y0A",
        "la memoria 01MABCDEFGHJKMNPQRSTVWXYZ0 sustituye a 01M9ZQ4V9N3X2M7P6R5T8W1Y0B",
        // git hashes and sha256
        "commit 3f9a1c2b4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a y 0F1E2D3C4B5A69788796A5B4C3D2E1F0A1B2C3D4",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        // paths and file names
        "D:\\Omoikane\\src\\recall.rs y potion-multilingual-128M-int8",
        "C:\\Users\\marcm\\AppData\\Local\\Temp\\omoi-real\\real.db",
        "~/.omoikane/backups/omoikane-2026-10-30.db y plex-mono-600.woff2",
        "gemma-3n-E2B-it-litertlm, Qwen2.5-7B-Instruct, EmbeddingGemma2LocalSearch",
        // normal URLs, also with ids that look random in the path (the false positive from
        // real memories: a claude.ai artifact)
        "https://github.com/marcmayol/omoikane/releases/tag/v0.18.0",
        "diseño temporal en claude.ai/artifact/4jDjbrNkj3ra1V9PqHx5eD (oscuro premium)",
        "https://www.youtube.com/watch/dQw4w9WgXcQ y https://gist.github.com/marcmayol/8f3Ab92kLm4Qx7Zp1Rt6Vw",
        "D:\\fotos\\IMG4jDjbrNkj3ra1V9PqHx5eD.jpg",
        "http://127.0.0.1:7438/#/revisar y https://marcmayol.com/exercise-api?lang=es",
        "git@github.com:marcmayol/omoikane.git",
        // camelCase identifiers with digits
        "Llama4ForConditionalGeneration, Gemma3nForConditionalGeneration y AutoModelForCausalLM",
        "OMOIKANE_DB, OMOIKANE_REDACT=0 y CARGO_BIN_EXE_omoi",
        // versions, dates, numbers
        "0.19.0 del 2026-10-30T16:34:00Z, 50.000 memorias, 1,9 MB",
        // what is already redacted
        "ya pasó: [redacted:github-token] y [redacted:private]",
        // normal text with the words that could be scary
        "el token de GitHub va en la variable GH_TOKEN; la clave privada está en ~/.ssh/id_ed25519",
        "Me gusta mucho la 1C, pero no la veo como portada",
    ] {
        assert_clean(t);
    }
}

/// The 1,836 lines of the equivalence corpus: only the one seeded with `<private>` (which is there twice,
/// once between quotes) gives a finding.
#[test]
fn corpus_has_no_false_positives() {
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    let mut with_findings = Vec::new();
    for l in corpus.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let hs = hallazgos(l);
        if !hs.is_empty() {
            with_findings.push((l.to_string(), tipos(&hs)));
        }
    }
    assert_eq!(with_findings.len(), 2, "{with_findings:#?}");
    for (line, kinds) in &with_findings {
        assert!(line.contains("<private>secreto</private>"), "{line}");
        assert_eq!(*kinds, vec!["private"]);
    }
}

/// What it costs: `redactar` over 6,000 characters (what the recall reads from a memory), with no
/// secrets and with four, and the scan `revisar::notices` does when reading, over a database (a COPY)
/// with `OMOIKANE_MEDIR_DB`, interleaved with the whole `notices`. In release:
/// `OMOIKANE_MEDIR_DB=<copy.db> cargo test --release --test secretos measure -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure() {
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    let mut clean6k = String::new();
    for l in corpus.lines().filter(|l| !l.starts_with('#') && !l.contains("<private>")) {
        if clean6k.chars().count() + l.chars().count() + 1 > 6000 {
            break;
        }
        clean6k.push_str(l);
        clean6k.push('\n');
    }
    let cut = (0..=clean6k.len() - 400).rev().find(|i| clean6k.is_char_boundary(*i)).unwrap();
    let mut dirty6k = clean6k[..cut].to_string();
    dirty6k.push_str(" gh~p_abcdefghijklmnopqrstuvwxyz0123456789 AK~IAIOSFODNN7EXAMPLE https://u:p@x.io/ ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4 ");
    for (name, t) in [("sin secretos", &clean6k), ("con 4", &dirty6k)] {
        let n = 2000;
        let t0 = std::time::Instant::now();
        let mut hs = 0;
        for _ in 0..n {
            hs += std::hint::black_box(redact(std::hint::black_box(t))).1.len();
        }
        eprintln!("redactar {} caracteres, {name}: {:.1} µs ({} hallazgos)", t.chars().count(), t0.elapsed().as_secs_f64() * 1e6 / n as f64, hs / n);
    }
    let Ok(path) = std::env::var("OMOIKANE_MEDIR_DB") else {
        eprintln!("OMOIKANE_MEDIR_DB no está: sin medida de Revisar");
        return;
    };
    let db = omoikane::db::open(std::path::Path::new(&path)).unwrap();
    let all = omoikane::memoria::recent(&db, &omoikane::memoria::Scope::All, None, usize::MAX >> 1).unwrap();
    omoikane::revisar::notices(&db, &omoikane::memoria::Scope::All).unwrap();
    let (mut scan, mut warnings) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        let t0 = std::time::Instant::now();
        let flagged = all.iter().filter(|m| !omoikane::revisar::secrets_of(m).is_empty()).count();
        scan.push(t0.elapsed().as_secs_f64() * 1000.0);
        let t0 = std::time::Instant::now();
        let v = omoikane::revisar::notices(&db, &omoikane::memoria::Scope::All).unwrap();
        warnings.push(t0.elapsed().as_secs_f64() * 1000.0);
        eprintln!("  {} memorias, {flagged} con hallazgos, {} avisos `secret` de {}", all.len(), v.iter().filter(|a| a.kind == "secret").count(), v.len());
    }
    let total_chars: usize = all.iter().map(|m| m.title.len() + m.content.len()).sum();
    scan.sort_by(f64::total_cmp);
    warnings.sort_by(f64::total_cmp);
    eprintln!("escaneo de secretos: mediana {:.0} ms ({:?}); {:.1} MB de texto", scan[2], scan, total_chars as f64 / 1e6);
    eprintln!("revisar::avisos entero: mediana {:.0} ms ({:?})", warnings[2], warnings);
}

/// Walks a database (a COPY) and shows each finding: to calibrate with real memories.
#[test]
#[ignore]
fn audit_db() {
    let Ok(path) = std::env::var("OMOIKANE_AUDITAR_DB") else {
        eprintln!("OMOIKANE_AUDITAR_DB no está: nada que auditar");
        return;
    };
    let db = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut q = db.prepare("SELECT id, title, content FROM memory ORDER BY id").unwrap();
    let rows: Vec<(String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
    let mut n = 0;
    let t0 = std::time::Instant::now();
    for (id, title, content) in &rows {
        let mut hs = hallazgos(title);
        hs.extend(hallazgos(content).into_iter().map(|h| omoikane::secretos::Hallazgo { inicio: h.inicio + title.len(), fin: h.fin + title.len(), ..h }));
        if !hs.is_empty() {
            n += 1;
            let all_text = format!("{title}{content}");
            eprintln!("{id}  «{}»", title.chars().take(70).collect::<String>());
            for h in &hs {
                let snippet: String = all_text[h.inicio..h.fin].chars().take(60).collect();
                eprintln!("    {:<16} {snippet}", h.tipo);
            }
        }
    }
    eprintln!("{} memorias, {n} con hallazgos, {:?}", rows.len(), t0.elapsed());
}

// --- Through the CLI -------------------------------------------------------------------------------

// Split so GitHub's scanner doesn't take it for real (see `unmark`).
const TOKEN: &str = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");

struct TestEnv {
    dir: comun::Temporal,
}

impl TestEnv {
    fn new(name: &str) -> TestEnv {
        let dir = comun::Temporal::nueva(&format!("secretos-{name}"));
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        TestEnv { dir }
    }

    fn db_path(&self) -> PathBuf {
        self.dir.join("o.db")
    }

    fn omoi(&self, args: &[&str], input: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.db_path())
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_REDACT")
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        if let Some(e) = input {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        o
    }

    fn json(&self, args: &[&str], env: &[(&str, &str)]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_slice(&self.omoi(&a, None, env).stdout).unwrap()
    }

    fn content(&self, id: &str) -> String {
        self.json(&["get", id], &[])["content"].as_str().unwrap().to_string()
    }

    fn secret_warnings(&self) -> Vec<serde_json::Value> {
        self.json(&["review"], &[]).as_array().unwrap().iter().filter(|a| a["kind"] == "secret").cloned().collect()
    }
}

#[test]
fn remember_and_update_redact_and_say_so() {
    let e = TestEnv::new("cli");
    let text = format!("El token de CI es {TOKEN} y la base postgres://app:S3cr3t@db.local/omoi");
    let o = e.omoi(&["--json", "remember", "-k", "fact", "-t", "Credenciales de CI", &text], None, &[]);
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("redacted 2 (github-token, url-credentials)"), "{err}");
    let c = e.content(&id);
    assert_eq!(c, "El token de CI es [redacted:github-token] y la base postgres://[redacted:url-credentials]@db.local/omoi");
    // What was saved no longer warns in Review.
    assert!(e.secret_warnings().is_empty());

    // `update` too, and only what it brings.
    let o = e.omoi(&["update", &id, "--content", &format!("Ahora es {TOKEN}"), "--reason", "rotado"], None, &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("redacted 1 (github-token)"));
    assert_eq!(e.content(&id), "Ahora es [redacted:github-token]");

    // Through MCP (`omoi tool`, the same as the server): `redacted: N` and the kinds in the response.
    let args = serde_json::json!({ "title": "Clave de OpenAI", "content": concat!("OPENAI_API_KEY=sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34"), "kind": "fact", "force": true });
    let o = e.omoi(&["tool", "remember"], Some(&args.to_string()), &[]);
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["redacted"], 1, "{r}");
    assert_eq!(r["redacted_types"], serde_json::json!(["openai-key"]));
    assert_eq!(e.content(r["id"].as_str().unwrap()), "OPENAI_API_KEY=[redacted:openai-key]");
    let args = serde_json::json!({ "id": id, "content": format!("<private>{TOKEN}</private> fuera"), "reason": "x" });
    let r: serde_json::Value = serde_json::from_slice(&e.omoi(&["tool", "update"], Some(&args.to_string()), &[]).stdout).unwrap();
    assert_eq!(r["redacted"], 1, "{r}");
    assert_eq!(r["redacted_types"], serde_json::json!(["private"]));
    // With nothing to remove, the response doesn't mention it.
    let args = serde_json::json!({ "id": id, "content": "nada secreto", "reason": "x" });
    let r: serde_json::Value = serde_json::from_slice(&e.omoi(&["tool", "update"], Some(&args.to_string()), &[]).stdout).unwrap();
    assert!(r.get("redacted").is_none(), "{r}");
}

#[test]
fn off_review_doctor_and_redact() {
    let e = TestEnv::new("revisar");
    let redaction_off = [("OMOIKANE_REDACT", "0")];
    let text = format!("export GH_TOKEN={TOKEN}");
    let o = e.omoi(&["--json", "remember", "-k", "fact", "-t", "Token de GitHub", &text], None, &redaction_off);
    assert!(!String::from_utf8_lossy(&o.stderr).contains("redacted"));
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(e.content(&id), text, "con OMOIKANE_REDACT=0 se guarda tal cual");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &redaction_off).stdout).unwrap();
    assert!(doctor.contains("redaction OFF (OMOIKANE_REDACT=0)"), "{doctor}");

    // Review warns about it (without showing the secret), and doctor counts it.
    let warnings = e.secret_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["memory_id"], id.as_str());
    assert_eq!(warnings[0]["detail"], "looks like a secret: 1 (github-token); redact it");
    assert!(!warnings[0].to_string().contains("ghp_"));
    let review_text = String::from_utf8(e.omoi(&["review"], None, &[]).stdout).unwrap();
    assert!(review_text.contains("secret") && !review_text.contains("ghp_"), "{review_text}");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &[]).stdout).unwrap();
    assert!(doctor.contains("redaction on; 1 memory looks like it holds one"), "{doctor}");

    // «It's fine like this» silences it until the memory changes.
    e.omoi(&["review", "--dismiss", &id, "--kind", "secret"], None, &[]);
    assert!(e.secret_warnings().is_empty());

    // «Redact» (what POST /api/redact does): the memory and its history, with the reason «redacted».
    e.omoi(&["update", &id, "--title", "Token de GitHub (CI)", "--reason", "título"], None, &redaction_off);
    {
        let mut db = omoikane::db::open(&e.db_path()).unwrap();
        let m = omoikane::memoria::redact(&mut db, &id, None).unwrap();
        assert_eq!(m.redacted, vec!["github-token"]);
        assert_eq!(m.content, "export GH_TOKEN=[redacted:github-token]");
        // Nothing there again.
        assert!(omoikane::memoria::redact(&mut db, &id, None).unwrap().redacted.is_empty());
    }
    let h = e.json(&["history", &id], &[]);
    let h = h.as_array().unwrap();
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(h[0]["reason"], "redacted");
    assert!(h.iter().all(|v| !v.to_string().contains("ghp_")), "{h:?}");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &[]).stdout).unwrap();
    assert!(doctor.contains("no memory looks like it holds one"), "{doctor}");
}

/// 1.8.6: handwritten passwords, in Spanish and in English. The value is redacted, not the word.
#[test]
fn handwritten_passwords() {
    for (t, expected) in [
        ("password: hunter2", "password: [redacted:password]"),
        ("Password=hunter2", "Password=[redacted:password]"),
        ("pass=Xy7#k2mQ en la VPN", "pass=[redacted:password] en la VPN"),
        ("PWD=s3cr3t#9", "PWD=[redacted:password]"),
        ("export DB_PASSWORD=Xy7#k2mQ", "export DB_PASSWORD=[redacted:password]"),
        ("PGPASSWORD=hunter2 psql -h db", "PGPASSWORD=[redacted:password] psql -h db"),
        ("{\"user\": \"marc\", \"password\": \"hunter2\"}", "{\"user\": \"marc\", \"password\": \"[redacted:password]\"}"),
        ("passwd := 'Xy7#k2'", "passwd := '[redacted:password]'"),
        ("la contraseña es Xy7#k2mQ", "la contraseña es [redacted:password]"),
        ("La Contraseña es: Xy7#k2mQ.", "La Contraseña es: [redacted:password]."),
        ("LA CONTRASEÑA ES Xy7#k2mQ", "LA CONTRASEÑA ES [redacted:password]"),
        ("la contraseña del wifi es Pollo2024#, no la cambies", "la contraseña del wifi es [redacted:password], no la cambies"),
        ("la contraseña de la base de datos es «Xy7#k2 mQ»", "la contraseña de la base de datos es «[redacted:password]»"),
        ("the password is hunter2", "the password is [redacted:password]"),
        ("The wifi password is \"correct horse 9\"", "The wifi password is \"[redacted:password]\""),
        ("the password for root was Xy7#k2mQ", "the password for root was [redacted:password]"),
        ("mysql -u root --password Xy7#k2mQ", "mysql -u root --password [redacted:password]"),
        ("mysql -u root --password=Xy7#k2mQ", "mysql -u root --password=[redacted:password]"),
    ] {
        assert_eq!(redact(t).0, expected, "{t}");
        assert_eq!(kinds_of(t), vec!["password"], "{t}");
        assert_eq!(redact(expected).0, expected, "volver a pasarlo cambia «{expected}»");
    }
}

/// Normal prose and code that talk about passwords without giving any.
#[test]
fn passwords_without_false_positives() {
    for t in [
        "la contraseña se guarda en el llavero",
        "la contraseña es segura y la guarda Bitwarden",
        "la contraseña es la del router",
        "la contraseña de 12 caracteres se pide al entrar",
        "la contraseña es de 16 caracteres como mínimo",
        "contraseñas: en el gestor, nunca en el repo",
        "password reset flow",
        "the password is required",
        "the password is 8 chars long",
        "the password is SHA256-hashed before saving",
        "Password: see the vault",
        "password: <your password>",
        "PASSWORD: ********",
        "password: ${{ secrets.DB_PASSWORD }}",
        "password=$DB_PASSWORD",
        "set PGPASSWORD=%DB_PASS%",
        "password = os.environ[\"DB_PASSWORD\"]",
        "password = getpass()",
        "password=config.db_password",
        "if password == \"x1\": return",
        "password_hash = bcrypt(pw)",
        "docker login --password-stdin < token.txt",
        "passwordless login with passkeys",
        "Tests pass: 45/45",
        "compass=N12 bypass=1",
        "let p = Password::new(\"x\");",
        "--no-password",
        "cd ~ && pwd",
        "password: [redacted:password]",
    ] {
        assert_clean(t);
    }
}

/// 1.8.6: the `rcm=` of LinkedIn URLs (the public id of a profile) is not a secret.
#[test]
fn linkedin_rcm_is_not_a_secret() {
    assert_clean("https://www.linkedin.com/feed/update/urn:li:activity:7390000000000000000/?rcm=ACoAAD1xYz2AbCdEf3GhIjK4LmNoPq5RsTuVw6Xy");
    assert_clean("https://www.linkedin.com/in/ACoAAD1xYz2AbCdEf3GhIjK4LmNoPq5RsTuVw6Xy");
    assert_clean("perfil ACoAAD1xYz2AbCdEf3GhIjK4LmNoPq5RsTuVw6Xy en Sales Navigator: ACwAAD1xYz2AbCdEf3GhIjK4LmNoPq5RsTuVw6Xy");
    // The rest of that URL is still checked: a real token in another parameter is redacted.
    assert_eq!(
        kinds_of("https://www.linkedin.com/feed/?rcm=ACoAAD1xYz2AbCdEf3GhIjK4LmNoPq5RsTuVw6Xy&access_token=q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0"),
        vec!["token"]
    );
}

/// 1.8.6: the 2 bytes before «password» were looked at as `&str` (`--password`) and panicked if
/// they fell in the middle of a multi-byte character. Through `remember`, and through `review --all` over what was
/// saved without redacting.
#[test]
fn passwords_after_multibyte_chars() {
    for t in ["—password x1", "x…password: abc", "“password: x1”", "éé--password Xy7#k2"] {
        let _ = hallazgos(t);
    }
    assert_eq!(redact("“password: Xy7#k2”").0, "“password: [redacted:password]”");
    assert_eq!(redact("éé --password Xy7#k2").0, "éé --password [redacted:password]");

    let e = TestEnv::new("multibyte");
    for (i, t) in ["—password x1", "x…password: abc", "“password: x1”"].iter().enumerate() {
        let o = e.omoi(&["--json", "remember", "-k", "fact", "-t", &format!("Texto {i}"), "--force", t], None, &[]);
        let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
        assert!(!e.content(&id).is_empty());
        e.omoi(&["--json", "remember", "-k", "fact", "-t", &format!("Sin redactar {i}"), "--force", t], None, &[("OMOIKANE_REDACT", "0")]);
    }
    e.omoi(&["review", "--all"], None, &[]);
    e.omoi(&["--json", "review", "--all"], None, &[]);
}
