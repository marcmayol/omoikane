//! 0.19.0, secretos: el detector (`secretos::redactar`) acierta con 3 secretos sembrados de cada tipo
//! y no da falsos positivos con lo que de verdad hay en una memoria: ULID, hashes de git, rutas, URLs,
//! ids de memorias, nombres de fichero, identificadores en camelCase y las 1.836 líneas de `corpus.txt`.
//! Y por la CLI: `remember` y `update` redactan (y lo dicen), `OMOIKANE_REDACT=0` lo apaga, Revisar y
//! `doctor` avisan de lo que ya estaba, y «Redactar» lo quita también del historial.
//!
//! `OMOIKANE_AUDITAR_DB=<copia.db> cargo test --test secretos -- --ignored --nocapture` recorre una base
//! y enseña cada hallazgo, para calibrar con memorias reales (siempre sobre una copia).

use omoikane::secretos::{self, hallazgos, tipos};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Los secretos de ejemplo de estos tests llevan una `~` en mitad del prefijo («xo~xb-», «gl~pat-»):
/// sin ella, el escáner de secretos de GitHub los toma por reales y no deja subir el repositorio. Las
/// tres funciones de abajo la quitan antes de llamar al detector, así que se prueba el texto entero.
fn sin_marca(t: &str) -> String {
    t.replace('~', "")
}

fn redactar(t: &str) -> (String, Vec<secretos::Hallazgo>) {
    secretos::redactar(&sin_marca(t))
}

fn tipos_de(t: &str) -> Vec<&'static str> {
    tipos(&hallazgos(&sin_marca(t)))
}

fn limpio(t: &str) {
    let t = sin_marca(t);
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
        assert_eq!(tipos_de(t), vec!["github-token"], "{t}");
        assert!(!redactar(t).0.contains("gh"), "{t}");
    }
    assert_eq!(redactar("export GH=gh~p_abcdefghijklmnopqrstuvwxyz0123456789").0, "export GH=[redacted:github-token]");
}

#[test]
fn openai_y_anthropic() {
    for t in [
        concat!("OPENAI_API_KEY=sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34"),
        "la vieja: sk-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90",
        "clave \"sk-1234567890abcdefghijklmnopqrstuvwxyz1234\"",
    ] {
        assert_eq!(tipos_de(t), vec!["openai-key"], "{t}");
    }
    for t in [
        "ANTHROPIC_API_KEY=sk~-ant-api03-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34Yz56Ab78Cd90Ef12Gh34Ij56Kl78Mn90-AbCdEfAA",
        "sk~-ant-admin01-abcdefghijklmnopqrstuvwxyz0123456789",
        "token: sk~-ant-sid01-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdef",
    ] {
        assert_eq!(tipos_de(t), vec!["anthropic-key"], "{t}");
    }
    // «sk-» a secas delante de palabras no es una clave.
    limpio("the sk-learn-compatible-estimator-wrapper is fine");
}

#[test]
fn aws() {
    for t in ["AWS_ACCESS_KEY_ID=AK~IAIOSFODNN7EXAMPLE", "key AK~IAJ4X7ZQ2PLM9NB3WE and more", "temporary AS~IAQ3R7T2WXYZ8KLMNP."] {
        assert_eq!(tipos_de(t), vec!["aws-key"], "{t}");
    }
    // 21 caracteres, o con minúsculas, no es una clave de AWS; «AKIA» dentro de una palabra tampoco.
    limpio("AK~IAIOSFODNN7EXAMPLEX");
    limpio("AK~IAiosfodnn7example");
    limpio("the word NAK~IAIOSFODNN7EXAMPLE");
}

#[test]
fn slack_gitlab_google_stripe() {
    for t in ["xo~xb-1234567890-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx", "xo~xp-12345-67890-abcdef", "xo~xa-2-ABCDEFGHIJ"] {
        assert_eq!(tipos_de(t), vec!["slack-token"], "{t}");
    }
    for t in ["gl~pat-Ab12Cd34Ef56Gh78Ij90", "GITLAB_TOKEN=gl~pat-abcdefghijklmnopqrstuvwxyz", "gl~pat-ZYXWVUTSRQPONMLKJIHGFEDCBA-1"] {
        assert_eq!(tipos_de(t), vec!["gitlab-token"], "{t}");
    }
    for t in ["AI~zaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r", "key=AI~zaSyDaGmWKa4JsXZ-HjGw7ISLn_3namBGewQe", "AI~zaSyB0123456789abcdefghijklmnopqrstuv"] {
        assert_eq!(tipos_de(t), vec!["google-key"], "{t}");
    }
    for t in ["sk~_live_4eC39HqLyjWDarjtT1zdp7dc", "sk~_test_51Hb1234567890abcdefghijkl", "rk~_live_abcdefghijklmnopqrstuvwxyz"] {
        assert_eq!(tipos_de(t), vec!["stripe-key"], "{t}");
    }
}

#[test]
fn webhooks_de_slack() {
    for t in [
        "https://hooks.sla~ck.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX",
        "curl -X POST hooks.sla~ck.com/services/T0ABCDEF1/B0ABCDEF2/abcdefghijklmnopqrstuvwx -d '{}'",
        "SLACK_WEBHOOK=https://hooks.sla~ck.com/services/TA1B2C3D4/BZ9Y8X7W6/q7XbP2mZr9KdT4wLs8NcV1hY",
    ] {
        assert_eq!(tipos_de(t), vec!["slack-webhook"], "{t}");
    }
    assert_eq!(
        redactar("POST https://hooks.sla~ck.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX ya").0,
        "POST https://hooks.slack.com/services/[redacted:slack-webhook] ya"
    );
    limpio("la doc está en https://hooks.slack.com/services/ y poco más");
}

#[test]
fn claves_pem() {
    let rsa = "-----BEGIN RSA PRIV~ATE KEY-----\nMIIEowIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyF8PbnGy0AHIw\nfoo\n-----END RSA PRIV~ATE KEY-----";
    let openssh = "antes\n-----BEGIN OPENSSH PRIV~ATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n-----END OPENSSH PRIV~ATE KEY-----\ndespués";
    let a_medias = "-----BEGIN EC PRIV~ATE KEY-----\nMHQCAQEEIBzK... y aquí se cortó";
    for t in [rsa, openssh, a_medias] {
        assert_eq!(tipos_de(t), vec!["private-key"], "{t}");
    }
    assert_eq!(redactar(openssh).0, "antes\n[redacted:private-key]\ndespués");
    assert_eq!(redactar(a_medias).0, "[redacted:private-key]");
    // Un certificado no es una clave privada.
    limpio("-----BEGIN CERTIFICATE-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA\n-----END CERTIFICATE-----");
}

#[test]
fn jwt() {
    for t in [
        "Authorization: Bearer ey~JhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ey~JzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        "ey~JhbGciOiJSUzI1NiJ9.ey~Jpc3MiOiJvbW9pIn0.abcdefghijklmnop",
        "token=ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4",
    ] {
        assert_eq!(tipos_de(t), vec!["jwt"], "{t}");
    }
    assert_eq!(redactar("token=ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4 ok").0, "token=[redacted:jwt] ok");
    // Dos partes, o la segunda sin «eyJ», no es un JWT (la cabecera suelta, de 20 en base64, sigue
    // pareciendo una cadena aleatoria, y eso es lo que es).
    assert!(!tipos_de("ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ").contains(&"jwt"));
    assert!(!tipos_de("ey~JhbGciOiJIUzI1NiJ9.abcdefghijklmnop.Zm9vYmFyYmF6cXV4").contains(&"jwt"));
}

#[test]
fn credenciales_en_urls() {
    for t in [
        "git clone https://marc:hunter2@github.com/marcmayol/omoikane.git",
        "postgres://app:S3cr3t!@db.local:5432/omoi",
        "la URL es ftp://user:pass@ftp.example.com/dir y nada más",
    ] {
        assert_eq!(tipos_de(t), vec!["url-credentials"], "{t}");
    }
    assert_eq!(redactar("git clone https://marc:hunter2@github.com/marcmayol/omoikane.git").0, "git clone https://[redacted:url-credentials]@github.com/marcmayol/omoikane.git");
    // Solo usuario, puerto o un correo no son credenciales.
    limpio("ssh://git@github.com/marcmayol/omoikane.git");
    limpio("http://127.0.0.1:7438/ y http://localhost:7438/#token");
    limpio("escribe a marc@example.com o mira https://example.com/a?b=c@d");
}

#[test]
fn private_de_engram() {
    for t in [
        "la clave es <private>hunter2</private>, no la pierdas",
        "<private>todo esto</private>",
        "dos: <private>a</private> y <private>b</private>",
    ] {
        assert_eq!(tipos_de(t), vec!["private"], "{t}");
    }
    assert_eq!(redactar("la clave es <private>hunter2</private>, no la pierdas").0, "la clave es [redacted:private], no la pierdas");
    assert_eq!(redactar("dos: <private>a</private> y <private>b</private>").0, "dos: [redacted:private] y [redacted:private]");
    // Sin cierre, se habla de la etiqueta.
    limpio("engram usa la etiqueta <private> para esto");
}

#[test]
fn cadenas_aleatorias() {
    for t in [
        "echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s",
        "API_SECRET=Zk8pQ2vL5mN1xR7tW3yB9cD4fG6hJ0a",
        "secret: 7Hq2Lp9Rz4Kx1Vb8Nm3Tc5Yw0Fd6Gs",
    ] {
        assert_eq!(tipos_de(t), vec!["token"], "{t}");
    }
    assert_eq!(redactar("echo \"q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0\" > /tmp/s").0, "echo \"[redacted:token]\" > /tmp/s");
}

#[test]
fn sin_falsos_positivos() {
    for t in [
        // ULID e ids de memorias
        "01K8ZQ4V9N3X2M7P6R5T8W1Y0A",
        "la memoria 01MABCDEFGHJKMNPQRSTVWXYZ0 sustituye a 01M9ZQ4V9N3X2M7P6R5T8W1Y0B",
        // hashes de git y sha256
        "commit 3f9a1c2b4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a y 0F1E2D3C4B5A69788796A5B4C3D2E1F0A1B2C3D4",
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        // rutas y nombres de fichero
        "D:\\Omoikane\\src\\recall.rs y potion-multilingual-128M-int8",
        "C:\\Users\\marcm\\AppData\\Local\\Temp\\omoi-real\\real.db",
        "~/.omoikane/backups/omoikane-2026-10-30.db y plex-mono-600.woff2",
        "gemma-3n-E2B-it-litertlm, Qwen2.5-7B-Instruct, EmbeddingGemma2LocalSearch",
        // URLs normales, también con ids que parecen aleatorios en la ruta (el falso positivo de las
        // memorias reales: un artifact de claude.ai)
        "https://github.com/marcmayol/omoikane/releases/tag/v0.18.0",
        "diseño temporal en claude.ai/artifact/4jDjbrNkj3ra1V9PqHx5eD (oscuro premium)",
        "https://www.youtube.com/watch/dQw4w9WgXcQ y https://gist.github.com/marcmayol/8f3Ab92kLm4Qx7Zp1Rt6Vw",
        "D:\\fotos\\IMG4jDjbrNkj3ra1V9PqHx5eD.jpg",
        "http://127.0.0.1:7438/#/revisar y https://marcmayol.com/exercise-api?lang=es",
        "git@github.com:marcmayol/omoikane.git",
        // identificadores en camelCase con cifras
        "Llama4ForConditionalGeneration, Gemma3nForConditionalGeneration y AutoModelForCausalLM",
        "OMOIKANE_DB, OMOIKANE_REDACT=0 y CARGO_BIN_EXE_omoi",
        // versiones, fechas, números
        "0.19.0 del 2026-10-30T16:34:00Z, 50.000 memorias, 1,9 MB",
        // lo ya redactado
        "ya pasó: [redacted:github-token] y [redacted:private]",
        // texto normal con las palabras que podrían asustar
        "el token de GitHub va en la variable GH_TOKEN; la clave privada está en ~/.ssh/id_ed25519",
        "Me gusta mucho la 1C, pero no la veo como portada",
    ] {
        limpio(t);
    }
}

/// Las 1.836 líneas del corpus de equivalencia: solo la sembrada con `<private>` (que está dos veces,
/// una entre comillas) da hallazgo.
#[test]
fn corpus_sin_falsos_positivos() {
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    let mut con_hallazgo = Vec::new();
    for l in corpus.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let hs = hallazgos(l);
        if !hs.is_empty() {
            con_hallazgo.push((l.to_string(), tipos(&hs)));
        }
    }
    assert_eq!(con_hallazgo.len(), 2, "{con_hallazgo:#?}");
    for (linea, tipos) in &con_hallazgo {
        assert!(linea.contains("<private>secreto</private>"), "{linea}");
        assert_eq!(*tipos, vec!["private"]);
    }
}

/// Lo que cuesta: `redactar` sobre 6.000 caracteres (lo que el recall lee de una memoria), sin
/// secretos y con cuatro, y el escaneo que `revisar::avisos` hace al leer, sobre una base (una COPIA)
/// con `OMOIKANE_MEDIR_DB`, intercalado con `avisos` entero. En release:
/// `OMOIKANE_MEDIR_DB=<copia.db> cargo test --release --test secretos medir -- --ignored --nocapture`.
#[test]
#[ignore]
fn medir() {
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    let mut limpio6k = String::new();
    for l in corpus.lines().filter(|l| !l.starts_with('#') && !l.contains("<private>")) {
        if limpio6k.chars().count() + l.chars().count() + 1 > 6000 {
            break;
        }
        limpio6k.push_str(l);
        limpio6k.push('\n');
    }
    let corte = (0..=limpio6k.len() - 400).rev().find(|i| limpio6k.is_char_boundary(*i)).unwrap();
    let mut sucio6k = limpio6k[..corte].to_string();
    sucio6k.push_str(" gh~p_abcdefghijklmnopqrstuvwxyz0123456789 AK~IAIOSFODNN7EXAMPLE https://u:p@x.io/ ey~JhbGciOiJIUzI1NiJ9.ey~JhIjoxfQ.Zm9vYmFyYmF6cXV4 ");
    for (nombre, t) in [("sin secretos", &limpio6k), ("con 4", &sucio6k)] {
        let n = 2000;
        let t0 = std::time::Instant::now();
        let mut hs = 0;
        for _ in 0..n {
            hs += std::hint::black_box(redactar(std::hint::black_box(t))).1.len();
        }
        eprintln!("redactar {} caracteres, {nombre}: {:.1} µs ({} hallazgos)", t.chars().count(), t0.elapsed().as_secs_f64() * 1e6 / n as f64, hs / n);
    }
    let Ok(ruta) = std::env::var("OMOIKANE_MEDIR_DB") else {
        eprintln!("OMOIKANE_MEDIR_DB no está: sin medida de Revisar");
        return;
    };
    let db = omoikane::db::abrir(std::path::Path::new(&ruta)).unwrap();
    let todas = omoikane::memoria::recientes(&db, &omoikane::memoria::Ambito::Todos, None, usize::MAX >> 1).unwrap();
    omoikane::revisar::avisos(&db, &omoikane::memoria::Ambito::Todos).unwrap();
    let (mut escaneo, mut avisos) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        let t0 = std::time::Instant::now();
        let con = todas.iter().filter(|m| !omoikane::revisar::secretos_de(m).is_empty()).count();
        escaneo.push(t0.elapsed().as_secs_f64() * 1000.0);
        let t0 = std::time::Instant::now();
        let v = omoikane::revisar::avisos(&db, &omoikane::memoria::Ambito::Todos).unwrap();
        avisos.push(t0.elapsed().as_secs_f64() * 1000.0);
        eprintln!("  {} memorias, {con} con hallazgos, {} avisos `secret` de {}", todas.len(), v.iter().filter(|a| a.kind == "secret").count(), v.len());
    }
    let caracteres: usize = todas.iter().map(|m| m.title.len() + m.content.len()).sum();
    escaneo.sort_by(f64::total_cmp);
    avisos.sort_by(f64::total_cmp);
    eprintln!("escaneo de secretos: mediana {:.0} ms ({:?}); {:.1} MB de texto", escaneo[2], escaneo, caracteres as f64 / 1e6);
    eprintln!("revisar::avisos entero: mediana {:.0} ms ({:?})", avisos[2], avisos);
}

/// Recorre una base (una COPIA) y enseña cada hallazgo: para calibrar con memorias reales.
#[test]
#[ignore]
fn auditar_base() {
    let Ok(ruta) = std::env::var("OMOIKANE_AUDITAR_DB") else {
        eprintln!("OMOIKANE_AUDITAR_DB no está: nada que auditar");
        return;
    };
    let db = rusqlite::Connection::open_with_flags(&ruta, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut q = db.prepare("SELECT id, title, content FROM memory ORDER BY id").unwrap();
    let filas: Vec<(String, String, String)> = q.query_map([], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
    let mut n = 0;
    let t0 = std::time::Instant::now();
    for (id, titulo, contenido) in &filas {
        let mut hs = hallazgos(titulo);
        hs.extend(hallazgos(contenido).into_iter().map(|h| omoikane::secretos::Hallazgo { inicio: h.inicio + titulo.len(), fin: h.fin + titulo.len(), ..h }));
        if !hs.is_empty() {
            n += 1;
            let todo = format!("{titulo}{contenido}");
            eprintln!("{id}  «{}»", titulo.chars().take(70).collect::<String>());
            for h in &hs {
                let trozo: String = todo[h.inicio..h.fin].chars().take(60).collect();
                eprintln!("    {:<16} {trozo}", h.tipo);
            }
        }
    }
    eprintln!("{} memorias, {n} con hallazgos, {:?}", filas.len(), t0.elapsed());
}

// --- Por la CLI -----------------------------------------------------------------------------------

// Partido para que el escáner de GitHub no lo tome por real (ver `sin_marca`).
const TOKEN: &str = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");

struct Entorno {
    dir: PathBuf,
}

impl Entorno {
    fn nuevo(nombre: &str) -> Entorno {
        let dir = std::env::temp_dir().join(format!("omoi-secretos-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        Entorno { dir }
    }

    fn base(&self) -> PathBuf {
        self.dir.join("o.db")
    }

    fn omoi(&self, args: &[&str], entrada: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
        c.args(args)
            .current_dir(self.dir.join("repo"))
            .env("OMOIKANE_DB", self.base())
            .env("OMOIKANE_MODELS", self.dir.join("models"))
            .env("OMOIKANE_AUTO_UPDATE", "0")
            .env("OMOIKANE_BACKUPS", "0")
            .env("OMOIKANE_HOME", self.dir.join("home-omoikane"))
            .env_remove("OMOIKANE_LANG")
            .env_remove("OMOIKANE_REDACT")
            .stdin(if entrada.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut hijo = c.spawn().unwrap();
        if let Some(e) = entrada {
            use std::io::Write;
            hijo.stdin.take().unwrap().write_all(e.as_bytes()).unwrap();
        }
        let o = hijo.wait_with_output().unwrap();
        assert!(o.status.success(), "omoi {args:?}: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        o
    }

    fn json(&self, args: &[&str], env: &[(&str, &str)]) -> serde_json::Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_slice(&self.omoi(&a, None, env).stdout).unwrap()
    }

    fn contenido(&self, id: &str) -> String {
        self.json(&["get", id], &[])["content"].as_str().unwrap().to_string()
    }

    fn avisos_secret(&self) -> Vec<serde_json::Value> {
        self.json(&["review"], &[]).as_array().unwrap().iter().filter(|a| a["kind"] == "secret").cloned().collect()
    }
}

impl Drop for Entorno {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn remember_y_update_redactan_y_lo_dicen() {
    let e = Entorno::nuevo("cli");
    let texto = format!("El token de CI es {TOKEN} y la base postgres://app:S3cr3t@db.local/omoi");
    let o = e.omoi(&["--json", "remember", "-k", "fact", "-t", "Credenciales de CI", &texto], None, &[]);
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("redacted 2 (github-token, url-credentials)"), "{err}");
    let c = e.contenido(&id);
    assert_eq!(c, "El token de CI es [redacted:github-token] y la base postgres://[redacted:url-credentials]@db.local/omoi");
    // Lo guardado ya no avisa en Revisar.
    assert!(e.avisos_secret().is_empty());

    // `update` también, y solo lo que trae.
    let o = e.omoi(&["update", &id, "--content", &format!("Ahora es {TOKEN}"), "--reason", "rotado"], None, &[]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("redacted 1 (github-token)"));
    assert_eq!(e.contenido(&id), "Ahora es [redacted:github-token]");

    // Por MCP (`omoi tool`, lo mismo que el servidor): `redacted: N` y los tipos en la respuesta.
    let args = serde_json::json!({ "title": "Clave de OpenAI", "content": concat!("OPENAI_API_KEY=sk-", "proj-Ab12Cd34Ef56Gh78Ij90Kl12Mn34Op56Qr78St90Uv12Wx34"), "kind": "fact", "force": true });
    let o = e.omoi(&["tool", "remember"], Some(&args.to_string()), &[]);
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["redacted"], 1, "{r}");
    assert_eq!(r["redacted_types"], serde_json::json!(["openai-key"]));
    assert_eq!(e.contenido(r["id"].as_str().unwrap()), "OPENAI_API_KEY=[redacted:openai-key]");
    let args = serde_json::json!({ "id": id, "content": format!("<private>{TOKEN}</private> fuera"), "reason": "x" });
    let r: serde_json::Value = serde_json::from_slice(&e.omoi(&["tool", "update"], Some(&args.to_string()), &[]).stdout).unwrap();
    assert_eq!(r["redacted"], 1, "{r}");
    assert_eq!(r["redacted_types"], serde_json::json!(["private"]));
    // Sin nada que quitar, la respuesta no lo menciona.
    let args = serde_json::json!({ "id": id, "content": "nada secreto", "reason": "x" });
    let r: serde_json::Value = serde_json::from_slice(&e.omoi(&["tool", "update"], Some(&args.to_string()), &[]).stdout).unwrap();
    assert!(r.get("redacted").is_none(), "{r}");
}

#[test]
fn apagado_revisar_doctor_y_redactar() {
    let e = Entorno::nuevo("revisar");
    let apagado = [("OMOIKANE_REDACT", "0")];
    let texto = format!("export GH_TOKEN={TOKEN}");
    let o = e.omoi(&["--json", "remember", "-k", "fact", "-t", "Token de GitHub", &texto], None, &apagado);
    assert!(!String::from_utf8_lossy(&o.stderr).contains("redacted"));
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(e.contenido(&id), texto, "con OMOIKANE_REDACT=0 se guarda tal cual");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &apagado).stdout).unwrap();
    assert!(doctor.contains("redaction OFF (OMOIKANE_REDACT=0)"), "{doctor}");

    // Revisar lo avisa (sin enseñar el secreto), y doctor lo cuenta.
    let avisos = e.avisos_secret();
    assert_eq!(avisos.len(), 1, "{avisos:?}");
    assert_eq!(avisos[0]["memory_id"], id.as_str());
    assert_eq!(avisos[0]["detail"], "looks like a secret: 1 (github-token); redact it");
    assert!(!avisos[0].to_string().contains("ghp_"));
    let texto_review = String::from_utf8(e.omoi(&["review"], None, &[]).stdout).unwrap();
    assert!(texto_review.contains("secret") && !texto_review.contains("ghp_"), "{texto_review}");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &[]).stdout).unwrap();
    assert!(doctor.contains("redaction on; 1 memory looks like it holds one"), "{doctor}");

    // «Está bien así» lo calla hasta que la memoria cambie.
    e.omoi(&["review", "--dismiss", &id, "--kind", "secret"], None, &[]);
    assert!(e.avisos_secret().is_empty());

    // «Redactar» (lo que hace POST /api/redact): la memoria y su historial, con el motivo «redacted».
    e.omoi(&["update", &id, "--title", "Token de GitHub (CI)", "--reason", "título"], None, &apagado);
    {
        let mut db = omoikane::db::abrir(&e.base()).unwrap();
        let m = omoikane::memoria::redactar(&mut db, &id, None).unwrap();
        assert_eq!(m.redacted, vec!["github-token"]);
        assert_eq!(m.content, "export GH_TOKEN=[redacted:github-token]");
        // Otra vez no hay nada.
        assert!(omoikane::memoria::redactar(&mut db, &id, None).unwrap().redacted.is_empty());
    }
    let h = e.json(&["history", &id], &[]);
    let h = h.as_array().unwrap();
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(h[0]["reason"], "redacted");
    assert!(h.iter().all(|v| !v.to_string().contains("ghp_")), "{h:?}");
    let doctor = String::from_utf8(e.omoi(&["doctor"], None, &[]).stdout).unwrap();
    assert!(doctor.contains("no memory looks like it holds one"), "{doctor}");
}
