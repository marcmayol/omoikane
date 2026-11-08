//! Secretos: lo que no debe entrar en la memoria (Fase 8.2, punto 4). Tokens con prefijo conocido
//! (GitHub, OpenAI, Anthropic, AWS, Slack, GitLab, Google, Stripe), claves privadas PEM, JWT, URLs con
//! `usuario:clave@`, lo marcado `<private>…</private>` (como engram, por quien venga de allí), las
//! contraseñas escritas a mano («password: hunter2», «la contraseña del wifi es Xy7#k2») y cadenas
//! largas que parecen aleatorias. Cada hallazgo se cambia por `[redacted:<tipo>]`.
//!
//! Sin expresiones regulares ni modelo: formas a mano, como `candidatas`. Lo que manda es no dar
//! falsos positivos: un ULID (26 mayúsculas y cifras, sin minúsculas), un hash de git (hexadecimal,
//! sin mayúsculas), una ruta, una URL normal, un nombre de fichero o un identificador en camelCase con
//! una cifra (`Llama4ForConditionalGeneration`) no son secretos. La cadena «aleatoria» tiene que medir
//! 20 o más sin separadores, mezclar minúsculas, mayúsculas y cifras, y cambiar de clase de carácter
//! cada pocos caracteres (un identificador son palabras: cambia cada 4 o más). Calibrado con las 144
//! memorias reales y las 1.836 líneas de `tests/corpus.txt`: 0 falsos positivos (ver `tests/secretos.rs`).
//! El único que dieron las reales, un id de artifact en una URL (`claude.ai/artifact/4jDj…`), lo quitó
//! la regla de que un segmento de ruta (lo que va tras `/` o `\`) es un nombre y no una clave.
//!
//! Se aplica al guardar y al cambiar (`memoria`), al exportar en la sincronización (`sync`, por lo que
//! se coló antes de la 0.19.0) y como aviso `secret` en Revisar sobre lo que ya hay. `OMOIKANE_REDACT=0`
//! lo apaga, y solo eso: un agente no puede saltárselo por parámetro. El recall no lo toca.

use serde::Serialize;

/// Un trozo del texto que parece un secreto: qué parece y dónde está (bytes del texto original).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hallazgo {
    /// `github-token`, `openai-key`, `anthropic-key`, `aws-key`, `slack-token`, `gitlab-token`,
    /// `google-key`, `stripe-key`, `slack-webhook`, `private-key`, `jwt`, `omoikane-sync-code`, `url-credentials`, `private`,
    /// `password` (una contraseña escrita a mano: «password: hunter2», «la contraseña es Xy7#…») o
    /// `token` (cadena larga que parece aleatoria).
    #[serde(rename = "type")]
    pub tipo: &'static str,
    pub inicio: usize,
    pub fin: usize,
}

/// Apagado solo con `OMOIKANE_REDACT=0`.
pub fn activo() -> bool {
    std::env::var("OMOIKANE_REDACT").ok().is_none_or(|v| v.trim() != "0")
}

/// El texto con cada hallazgo cambiado por `[redacted:<tipo>]`, y los hallazgos (posiciones en el
/// texto original). Sin hallazgos devuelve el mismo texto. Volver a pasarlo no cambia nada.
pub fn redactar(texto: &str) -> (String, Vec<Hallazgo>) {
    let hs = hallazgos(texto);
    if hs.is_empty() {
        return (texto.to_string(), hs);
    }
    let mut s = String::with_capacity(texto.len());
    let mut i = 0;
    for h in &hs {
        s.push_str(&texto[i..h.inicio]);
        s.push_str("[redacted:");
        s.push_str(h.tipo);
        s.push(']');
        i = h.fin;
    }
    s.push_str(&texto[i..]);
    (s, hs)
}

/// Los tipos distintos de una lista de hallazgos, en el orden en que aparecen.
pub fn tipos(hs: &[Hallazgo]) -> Vec<&'static str> {
    tipos_distintos(&hs.iter().map(|h| h.tipo).collect::<Vec<_>>())
}

/// Lo mismo desde los tipos sueltos (`Memoria::redacted`, uno por hallazgo).
pub fn tipos_distintos(ts: &[&'static str]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    for t in ts {
        if !v.contains(t) {
            v.push(t);
        }
    }
    v
}

/// «2 (github-token, token)»: para la CLI y los avisos.
pub fn resumen(ts: &[&'static str]) -> String {
    format!("{} ({})", ts.len(), tipos_distintos(ts).join(", "))
}

/// Lo que parece un secreto, ordenado y sin solapes: el que empieza antes gana (así `<private>…` y un
/// bloque PEM se llevan lo que tienen dentro), y a igual principio, el más largo.
pub fn hallazgos(texto: &str) -> Vec<Hallazgo> {
    let mut v = Vec::new();
    privados(texto, &mut v);
    pem(texto, &mut v);
    jwt(texto, &mut v);
    codigo_sync(texto, &mut v);
    credenciales_url(texto, &mut v);
    webhook_slack(texto, &mut v);
    contrasenas(texto, &mut v);
    palabras(texto, &mut v);
    v.sort_by(|a, b| a.inicio.cmp(&b.inicio).then(b.fin.cmp(&a.fin)));
    let mut r: Vec<Hallazgo> = Vec::with_capacity(v.len());
    for h in v {
        if r.last().is_none_or(|u| h.inicio >= u.fin) {
            r.push(h);
        }
    }
    r
}

fn hallazgo(tipo: &'static str, inicio: usize, fin: usize) -> Hallazgo {
    Hallazgo { tipo, inicio, fin }
}

// --- Detectores ------------------------------------------------------------------------------------
//
// Todas las posiciones que se calculan caen en un byte ASCII o justo después de uno, así que son
// límites de carácter y se puede cortar el `&str` por ellas.

/// `<private>…</private>`, etiquetas incluidas. Sin cierre no se toca: alguien puede estar hablando
/// de la etiqueta.
fn privados(t: &str, v: &mut Vec<Hallazgo>) {
    const ABRE: &str = "<private>";
    const CIERRA: &str = "</private>";
    let mut i = 0;
    while let Some(k) = t[i..].find(ABRE) {
        let a = i + k;
        let Some(c) = t[a + ABRE.len()..].find(CIERRA) else { break };
        let fin = a + ABRE.len() + c + CIERRA.len();
        v.push(hallazgo("private", a, fin));
        i = fin;
    }
}

/// `-----BEGIN … PRIVATE KEY-----` hasta su `-----END … PRIVATE KEY-----` (o el final del texto si no
/// lo hay: una clave a medias sigue siendo una clave).
fn pem(t: &str, v: &mut Vec<Hallazgo>) {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut i = 0;
    while let Some(k) = t[i..].find(BEGIN) {
        let a = i + k;
        let fin_linea = t[a..].find('\n').map_or(t.len(), |x| a + x);
        if !t[a..fin_linea].contains("PRIVATE KEY-----") {
            i = a + BEGIN.len();
            continue;
        }
        let fin = match t[fin_linea..].find(END) {
            Some(e) => {
                let e = fin_linea + e + END.len();
                t[e..].find("-----").map_or(t.len(), |x| e + x + 5)
            }
            None => t.len(),
        };
        v.push(hallazgo("private-key", a, fin));
        i = fin;
    }
}

fn es_b64url(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

/// Hasta dónde llega, desde `desde`, la tira de bytes que cumplen `clase`.
fn fin_tira(b: &[u8], desde: usize, clase: fn(u8) -> bool) -> usize {
    let mut j = desde;
    while j < b.len() && clase(b[j]) {
        j += 1;
    }
    j
}

/// Un JWT: `eyJ…`, punto, `eyJ…`, punto, firma; las tres partes de 10 o más. Cabecera y carga son
/// JSON en base64url, y JSON empieza por `{"`, que codificado es `eyJ`.
fn jwt(t: &str, v: &mut Vec<Hallazgo>) {
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find("eyJ") {
        let a = i + k;
        if a > 0 && es_b64url(b[a - 1]) {
            i = a + 3;
            continue;
        }
        let p1 = fin_tira(b, a, es_b64url);
        if p1 - a >= 10 && b.get(p1) == Some(&b'.') && t[p1 + 1..].starts_with("eyJ") {
            let p2 = fin_tira(b, p1 + 1, es_b64url);
            if p2 - (p1 + 1) >= 10 && b.get(p2) == Some(&b'.') {
                let p3 = fin_tira(b, p2 + 1, es_b64url);
                if p3 - (p2 + 1) >= 10 {
                    v.push(hallazgo("jwt", a, p3));
                    i = p3;
                    continue;
                }
            }
        }
        i = a + 3;
    }
}

/// Un código de `omoi sync code` (`omoi1.` y base64url): lleva la clave de un repositorio de memorias.
/// Un agente sin persona delante lo tiene en `OMOIKANE_SYNC_CODE` y podría querer apuntarlo.
fn codigo_sync(t: &str, v: &mut Vec<Hallazgo>) {
    const PREFIJO: &str = "omoi1.";
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find(PREFIJO) {
        let a = i + k;
        let fin = fin_tira(b, a + PREFIJO.len(), es_b64url);
        if (a == 0 || !es_b64url(b[a - 1])) && fin - (a + PREFIJO.len()) >= 40 {
            v.push(hallazgo("omoikane-sync-code", a, fin));
            i = fin;
        } else {
            i = a + PREFIJO.len();
        }
    }
}

/// `esquema://usuario:clave@equipo`: se redacta `usuario:clave`. Solo usuario (`ssh://git@…`) no es
/// un secreto, y `equipo:puerto` sin `@` tampoco.
fn credenciales_url(t: &str, v: &mut Vec<Hallazgo>) {
    let mut i = 0;
    while let Some(k) = t[i..].find("://") {
        let ini = i + k + 3;
        let fin = t[ini..].find(|c: char| c.is_whitespace() || "/?#\"'<>()[]`,;".contains(c)).map_or(t.len(), |x| ini + x);
        let autoridad = &t[ini..fin];
        if let Some(arroba) = autoridad.rfind('@')
            && let Some((usuario, clave)) = autoridad[..arroba].split_once(':')
            && !usuario.is_empty()
            && !clave.is_empty()
        {
            v.push(hallazgo("url-credentials", ini, ini + arroba));
        }
        i = fin.max(ini);
    }
}

/// `hooks.slack.com/services/T…/B…/…`: quien tiene la URL escribe en el canal. Se redacta la ruta.
fn webhook_slack(t: &str, v: &mut Vec<Hallazgo>) {
    const BASE: &str = "hooks.slack.com/services/";
    let b = t.as_bytes();
    let mut i = 0;
    while let Some(k) = t[i..].find(BASE) {
        let ini = i + k + BASE.len();
        let fin = fin_tira(b, ini, |c| c.is_ascii_alphanumeric() || c == b'/');
        if fin - ini >= 20 && t[ini..fin].matches('/').count() >= 2 {
            v.push(hallazgo("slack-webhook", ini, fin));
        }
        i = fin.max(ini);
    }
}

/// Palabras que nombran una contraseña, en minúsculas ASCII (la `Ñ` mayúscula no baja con
/// `to_ascii_lowercase`, por eso va aparte), y si valen en prosa («la contraseña es …»). `pass` y
/// `pwd` solo con `=` (`pass=…`, `PWD=…`): con `:` o en prosa son otra cosa («Tests pass: 45/45»).
const NOMBRES_CONTRASENA: &[(&str, bool)] = &[
    ("password", true),
    ("passwd", true),
    ("passphrase", true),
    ("contraseña", true),
    ("contraseÑa", true),
    ("contrasena", true),
    ("pass", false),
    ("pwd", false),
];

/// Lo que puede ir entre la palabra y el verbo en prosa («la contraseña del wifi es …», «the password
/// for root is …»): solo si la primera palabra es una de estas, y hasta 5 palabras.
const ENLACES_CONTRASENA: &[&str] = &["de", "del", "of", "for", "para"];
/// El verbo de la prosa: «la contraseña es …», «the password is …».
const VERBOS_CONTRASENA: &[&str] = &["es", "is", "era", "was"];

/// Letra o cifra, contando como letra cualquier byte de un carácter no ASCII («ñ», «é»).
fn es_letra_o_cifra(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c >= 0x80
}

/// Contraseñas escritas a mano: una palabra que la nombra seguida de `:` o `=` (`password: hunter2`,
/// `DB_PASSWORD=Xy7#k2`, `"password": "…"`, `pass=…`) o, en prosa, de «es»/«is» («la contraseña es
/// Xy7#k2», «the wifi password is hunter2», «la contraseña del router es …»). Se redacta solo el
/// valor. Para no tomar por contraseña la prosa normal («la contraseña se guarda en el llavero»,
/// «password reset flow», «the password is required») el valor tiene que parecerlo: ver
/// `parece_contrasena`.
fn contrasenas(t: &str, v: &mut Vec<Hallazgo>) {
    let l = t.to_ascii_lowercase(); // los mismos bytes en las mismas posiciones
    let b = l.as_bytes();
    for &(nombre, en_prosa) in NOMBRES_CONTRASENA {
        let mut i = 0;
        while let Some(k) = l[i..].find(nombre) {
            let a = i + k;
            let mut j = a + nombre.len();
            i = j;
            // Palabra entera por detrás («passwords», «passing», «contraseñas», `password_hash`, `--password-stdin` no).
            // Por delante, entera o tras `_` (`DB_PASSWORD`); `password` y `passwd` también pegadas a
            // otra palabra (`PGPASSWORD=`), pero entonces solo con `=` o `:`.
            if j < b.len() && (es_letra_o_cifra(b[j]) || matches!(b[j], b'_' | b'-')) {
                continue;
            }
            let pegada = a > 0 && es_letra_o_cifra(b[a - 1]);
            if pegada && !matches!(nombre, "password" | "passwd") {
                continue;
            }
            // `--password` de una orden: con `=` o con el valor detrás (`--password Xy7#k2`); tras un
            // solo guion es otra cosa (`no-password`).
            let opcion = a >= 2 && b[a - 2..a] == *b"--"; // por bytes: lo de antes puede ser un carácter de varios (`—password`)
            if a > 0 && b[a - 1] == b'-' && !opcion {
                continue;
            }
            if opcion && b.get(j) == Some(&b' ') {
                let ini = fin_tira(b, j, |c| c == b' ');
                if let Some((ini, fin)) = valor_de_contrasena(t, ini)
                    && parece_contrasena(&t[ini..fin], false)
                {
                    v.push(hallazgo("password", ini, fin));
                    i = fin;
                }
                continue;
            }
            // La comilla que cierra una clave (`"password": …`) y espacios.
            if j < b.len() && matches!(b[j], b'"' | b'\'') {
                j += 1;
            }
            while j < b.len() && matches!(b[j], b' ' | b'\t') {
                j += 1;
            }
            let prosa = match b.get(j) {
                Some(b':') if !matches!(nombre, "pass" | "pwd") => {
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
                        continue; // una comparación
                    }
                    if b.get(j + 1) == Some(&b'>') {
                        j += 1; // `"password" => "…"`
                    }
                    j += 1;
                    false
                }
                _ if en_prosa && !pegada => {
                    let Some(fin_verbo) = verbo_de_contrasena(&l, j) else { continue };
                    j = fin_verbo;
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
            if let Some((ini, fin)) = valor_de_contrasena(t, j)
                && parece_contrasena(&t[ini..fin], prosa)
            {
                v.push(hallazgo("password", ini, fin));
                i = fin;
            }
        }
    }
}

/// Desde `j` (tras la palabra y sus espacios), dónde acaba «es»/«is», saltando antes un «del wifi» o
/// un «for root» (hasta 5 palabras, la primera de `ENLACES_CONTRASENA`: «de la base de datos»). `l` va en minúsculas.
fn verbo_de_contrasena(l: &str, mut j: usize) -> Option<usize> {
    let b = l.as_bytes();
    let mut enlazado = false;
    for _ in 0..=5 {
        let f = fin_tira(b, j, |c| es_letra_o_cifra(c) || c == b'_' || c == b'-');
        let w = &l[j..f];
        if w.is_empty() {
            return None;
        }
        if VERBOS_CONTRASENA.contains(&w) && b.get(f).is_none_or(|c| matches!(c, b' ' | b'\t' | b':')) {
            return Some(f);
        }
        if !enlazado && !ENLACES_CONTRASENA.contains(&w) {
            return None;
        }
        enlazado = true;
        if b.get(f) != Some(&b' ') {
            return None;
        }
        j = f + 1;
    }
    None
}

/// El valor que empieza en `j`: entre comillas (en la misma línea), o hasta el primer espacio y sin
/// el signo que cierra la frase («… es hunter2.»).
fn valor_de_contrasena(t: &str, j: usize) -> Option<(usize, usize)> {
    let b = t.as_bytes();
    let c = *b.get(j)?;
    if matches!(c, b'"' | b'\'' | b'`') {
        let fin = t[j + 1..].find([c as char, '\n']).map(|x| j + 1 + x)?;
        return (b[fin] == c).then_some((j + 1, fin));
    }
    if t[j..].starts_with('«') {
        let ini = j + '«'.len_utf8();
        let fin = t[ini..].find(['»', '\n']).map(|x| ini + x)?;
        return t[fin..].starts_with('»').then_some((ini, fin));
    }
    let mut fin = t[j..].find(char::is_whitespace).map_or(t.len(), |x| j + x);
    // Las comillas tipográficas que cierran («“password: x1”») no son parte del valor.
    while let Some(c) = t[j..fin].chars().next_back().filter(|c| matches!(c, '»' | '”' | '’')) {
        fin -= c.len_utf8();
    }
    while fin > j && matches!(b[fin - 1], b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'"' | b'\'') {
        fin -= 1;
    }
    (fin > j).then_some((j, fin))
}

/// Si lo que sigue a «password:» o a «la contraseña es» parece una contraseña, y no una palabra de la
/// frase, un hueco por rellenar o código: de 4 a 128 bytes, con alguna cifra o algún signo (una
/// palabra suelta como «required» o «segura» no entra; se pierde «swordfish», a cambio de no dar
/// falsos positivos), y sin ser `$VAR`, `%VAR%`, `<la tuya>`, `${{ secrets.X }}`, `****`, una ruta,
/// `os.environ["X"]`, `getpass()` ni `config.db_password`. En prosa, además, sin acabar en una
/// palabra compuesta («SHA256-hashed», «bcrypt-based»).
fn parece_contrasena(w: &str, prosa: bool) -> bool {
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
    // `config.db_password`, `settings.PASSWORD`: un nombre con puntos, sin cifras.
    if w.contains('.') && b.iter().all(|&c| c.is_ascii_alphabetic() || c == b'_' || c == b'.') {
        return false;
    }
    let cifra = b.iter().any(u8::is_ascii_digit);
    let signo = b.iter().any(|&c| c.is_ascii_punctuation() && !matches!(c, b'_' | b'-' | b'.' | b'\'' | b'"' | b','));
    if !cifra && !signo {
        return false;
    }
    if prosa
        && let Some((_, cola)) = w.rsplit_once('-')
        && cola.len() >= 4
        && cola.bytes().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    true
}

fn es_de_palabra(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn es_alnum_o_guion_bajo(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn es_alnum_o_guion(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

fn es_mayuscula_o_cifra(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

/// Un prefijo conocido al principio de una palabra: (prefijo, tipo, lo que puede seguirle, cuánto
/// tiene que seguirle como mínimo). Los largos reales van bastante por encima del mínimo.
type Prefijo = (&'static str, &'static str, fn(u8) -> bool, usize);

const PREFIJOS: &[Prefijo] = &[
    ("github_pat_", "github-token", es_alnum_o_guion_bajo, 20),
    ("ghp_", "github-token", es_alnum_o_guion_bajo, 20),
    ("gho_", "github-token", es_alnum_o_guion_bajo, 20),
    ("ghu_", "github-token", es_alnum_o_guion_bajo, 20),
    ("ghs_", "github-token", es_alnum_o_guion_bajo, 20),
    ("ghr_", "github-token", es_alnum_o_guion_bajo, 20),
    ("sk-ant-", "anthropic-key", es_b64url, 20),
    ("sk-", "openai-key", es_b64url, 20),
    ("sk_live_", "stripe-key", es_alnum_o_guion_bajo, 20),
    ("sk_test_", "stripe-key", es_alnum_o_guion_bajo, 20),
    ("rk_live_", "stripe-key", es_alnum_o_guion_bajo, 20),
    ("rk_test_", "stripe-key", es_alnum_o_guion_bajo, 20),
    ("xoxa-", "slack-token", es_alnum_o_guion, 10),
    ("xoxb-", "slack-token", es_alnum_o_guion, 10),
    ("xoxp-", "slack-token", es_alnum_o_guion, 10),
    ("xoxs-", "slack-token", es_alnum_o_guion, 10),
    ("xoxr-", "slack-token", es_alnum_o_guion, 10),
    ("glpat-", "gitlab-token", es_b64url, 20),
    ("AIza", "google-key", es_b64url, 30),
];

/// Palabra a palabra (tiras de letras, cifras, `_` y `-`): los prefijos conocidos al principio de la
/// palabra, las claves de AWS, y la regla general de la cadena que parece aleatoria.
fn palabras(t: &str, v: &mut Vec<Hallazgo>) {
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !es_de_palabra(b[i]) {
            i += 1;
            continue;
        }
        let fin = fin_tira(b, i, es_de_palabra);
        let palabra = &t[i..fin];
        if let Some((prefijo, tipo, clase, minimo)) = PREFIJOS.iter().find(|(p, ..)| palabra.starts_with(p)) {
            let resto_fin = fin_tira(b, i + prefijo.len(), *clase);
            let resto = &t[i + prefijo.len()..resto_fin];
            // Una clave de OpenAI o de Google lleva cifras siempre; «sk-» a secas delante de palabras, no.
            let con_cifra = !matches!(*tipo, "openai-key" | "google-key") || resto.bytes().any(|c| c.is_ascii_digit());
            if resto.len() >= *minimo && con_cifra {
                v.push(hallazgo(tipo, i, resto_fin));
                i = fin;
                continue;
            }
        }
        // Clave de acceso de AWS: AKIA (o ASIA, temporal) y 16 mayúsculas o cifras, 20 justas.
        if (palabra.starts_with("AKIA") || palabra.starts_with("ASIA")) && palabra.len() >= 20 {
            let fin_clave = fin_tira(b, i, es_mayuscula_o_cifra);
            if fin_clave - i == 20 && (fin_clave == fin || matches!(b[fin_clave], b'-' | b'_')) {
                v.push(hallazgo("aws-key", i, fin_clave));
                i = fin;
                continue;
            }
        }
        // Un segmento de ruta (`claude.ai/artifact/4jDjbrNkj3ra1V9PqHx5eD`, `C:\x\…`) es un nombre, no
        // una clave: los ids de artifacts, vídeos o gists parecen aleatorios y se enseñan a propósito. Lo
        // que sí es secreto en una ruta tiene forma conocida (el webhook de Slack, aparte).
        if i > 0 && matches!(b[i - 1], b'/' | b'\\') {
            i = fin;
            continue;
        }
        // Los ids de miembro de LinkedIn (`ACoAA…`, y `ACwAA…` en Sales Navigator) parecen aleatorios pero
        // son el id público de un perfil: salen en sus URLs (`/in/ACoAA…`, `?rcm=ACoAA…`, el parámetro
        // con el que LinkedIn marca de quién viene una recomendación). Lo mismo el valor de cualquier `rcm=`.
        if palabra.starts_with("ACoAA") || palabra.starts_with("ACwAA") || t[..i].ends_with("rcm=") {
            i = fin;
            continue;
        }
        // Regla general, por trozos sin `_` ni `-` («potion-multilingual-128M-int8» son palabras).
        let mut ini_trozo = i;
        for (k, c) in b[i..fin].iter().enumerate() {
            let pos = i + k;
            if *c == b'_' || *c == b'-' {
                if parece_aleatoria(&t[ini_trozo..pos]) {
                    v.push(hallazgo("token", ini_trozo, pos));
                }
                ini_trozo = pos + 1;
            }
        }
        if parece_aleatoria(&t[ini_trozo..fin]) {
            v.push(hallazgo("token", ini_trozo, fin));
        }
        i = fin;
    }
}

/// Una cadena de 20 o más letras y cifras sin separadores que mezcla minúsculas, mayúsculas y cifras y
/// cambia de clase de carácter cada menos de 3,5 caracteres de media. Un identificador en camelCase
/// (`Llama4ForConditionalGeneration`: Llama·4·For·Conditional·Generation, 6 por trozo) no cambia tanto;
/// un token (`q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0`, 26 trozos en 33) sí. Una mayúscula seguida de
/// minúsculas cuenta como una sola palabra. Sin minúsculas (un ULID) o sin mayúsculas (un hash) no entra.
pub fn parece_aleatoria(w: &str) -> bool {
    let b = w.as_bytes();
    if b.len() < 20 || !b.iter().all(u8::is_ascii_alphanumeric) {
        return false;
    }
    let (mut minus, mut mayus, mut cifras) = (false, false, false);
    let mut trozos = 0usize;
    let mut anterior = 0u8; // 1 minúscula, 2 mayúscula, 3 cifra
    for &c in b {
        let clase = if c.is_ascii_lowercase() {
            minus = true;
            1
        } else if c.is_ascii_uppercase() {
            mayus = true;
            2
        } else {
            cifras = true;
            3
        };
        if clase != anterior && !(anterior == 2 && clase == 1) {
            trozos += 1;
        }
        anterior = clase;
    }
    minus && mayus && cifras && trozos * 7 >= b.len() * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacta_y_coloca() {
        let (s, hs) = redactar("token ghp_abcdefghijklmnopqrstuvwxyz0123456789 y ya");
        assert_eq!(s, "token [redacted:github-token] y ya");
        assert_eq!(hs.len(), 1);
        assert_eq!(&"token ghp_abcdefghijklmnopqrstuvwxyz0123456789 y ya"[hs[0].inicio..hs[0].fin], "ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(redactar(&s).0, s);
    }

    #[test]
    fn codigo_de_sync() {
        // Un código de `omoi sync code` entero: `omoi1.` y el JSON en base64url (empieza por `eyJ`).
        let codigo = "omoi1.eyJ1cmwiOiJnaXRAeDp5L3ouZ2l0Iiwia2V5IjoiMDAxMTIyMzM0NDU1NjY3Nzg4OTlhYWJiY2NkZGVlZmYifQ";
        let (s, hs) = redactar(&format!("OMOIKANE_SYNC_CODE={codigo} en la CI"));
        assert_eq!(s, "OMOIKANE_SYNC_CODE=[redacted:omoikane-sync-code] en la CI");
        assert_eq!(tipos(&hs), vec!["omoikane-sync-code"]);
        // Hablar del formato no es un secreto.
        assert!(hallazgos("los códigos empiezan por omoi1. y llevan la clave").is_empty());
    }

    #[test]
    fn el_que_empieza_antes_gana() {
        let (s, hs) = redactar("<private>clave ghp_abcdefghijklmnopqrstuvwxyz0123456789</private> fin");
        assert_eq!(s, "[redacted:private] fin");
        assert_eq!(tipos(&hs), vec!["private"]);
    }

    #[test]
    fn aleatoria_frente_a_identificador() {
        assert!(parece_aleatoria("q7XbP2mZr9KdT4wLs8NcV1hY6fJa3GeU0"));
        assert!(!parece_aleatoria("Llama4ForConditionalGeneration"));
        assert!(!parece_aleatoria("01K8ZQ4V9N3X2M7P6R5T8W1Y0A")); // ULID
        assert!(!parece_aleatoria("3f9a1c2b4d5e6f7a8b9c0d1e2f3a4b5c")); // hash
        assert!(!parece_aleatoria("corto1A"));
    }

    /// 1.8.6: lo de antes de «password» puede ser un carácter de varios bytes; mirar si es `--` no
    /// puede cortarlo por la mitad.
    #[test]
    fn contrasena_tras_caracter_de_varios_bytes() {
        assert!(hallazgos("—password x1").is_empty());
        assert!(hallazgos("x…password: abc").is_empty());
        assert_eq!(redactar("“password: x1”").0, "“password: x1”");
        assert_eq!(redactar("“password: Xy7#k2”").0, "“password: [redacted:password]”");
        assert_eq!(redactar("—password=Xy7#k2").0, "—password=[redacted:password]");
    }
}
