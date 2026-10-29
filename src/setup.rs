//! `omoi setup claude`: deja Omoikane listo en Claude Code con su propia CLI (`claude plugin …`):
//! añade el marketplace y instala el plugin, que trae el servidor MCP y los hooks del recall. Si ya
//! está hecho, no toca nada.

use crate::tr;
use anyhow::{Context, Result, bail};
use std::process::{Command, Output};

pub const MARKETPLACE_REPO: &str = "marcmayol/drac-plugins";
pub const MARKETPLACE: &str = "drac";
pub const PLUGIN: &str = "omoikane";

/// `claude` puede ser un .exe o, instalado con npm en Windows, un .cmd que no se lanza directamente.
fn claude(args: &[&str]) -> Result<Output> {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg("claude");
        c
    } else {
        Command::new("claude")
    };
    c.args(args).output().context(tr!("can't run `claude`", "no se puede ejecutar `claude`"))
}

fn texto(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// Un paso de la instalación: qué hace y con qué orden.
pub struct Paso {
    pub que: String,
    pub args: Vec<String>,
}

/// Qué falta por hacer en este equipo (vacío si ya está todo).
pub fn plan() -> Result<Vec<Paso>> {
    let version = claude(&["--version"]);
    if !version.as_ref().is_ok_and(|o| o.status.success()) {
        bail!(tr!(
            "Claude Code (`claude`) is not installed or not on the PATH. Install it first: https://code.claude.com",
            "Claude Code (`claude`) no está instalado o no está en el PATH. Instálalo primero: https://code.claude.com"
        ));
    }
    let mut pasos = Vec::new();
    let marketplaces = texto(&claude(&["plugin", "marketplace", "list"])?);
    // Cada marketplace sale como «❯ nombre»; vale cualquier origen (GitHub o una carpeta local).
    let tiene_marketplace = marketplaces.lines().any(|l| l.trim().trim_start_matches('❯').trim() == MARKETPLACE);
    if !tiene_marketplace {
        pasos.push(Paso {
            que: tr!("add the marketplace {MARKETPLACE_REPO}", "añadir el marketplace {MARKETPLACE_REPO}"),
            args: ["plugin", "marketplace", "add", MARKETPLACE_REPO].map(String::from).to_vec(),
        });
    }
    let plugins = texto(&claude(&["plugin", "list"])?);
    if !plugins.contains(&format!("{PLUGIN}@")) {
        if tiene_marketplace {
            // Un marketplace `drac` ya añadido puede ser una copia de antes de que existiera el plugin.
            pasos.push(Paso {
                que: tr!("update the marketplace {MARKETPLACE}", "actualizar el marketplace {MARKETPLACE}"),
                args: ["plugin", "marketplace", "update", MARKETPLACE].map(String::from).to_vec(),
            });
        }
        pasos.push(Paso {
            que: tr!(
                "install the plugin {PLUGIN}@{MARKETPLACE} for your user (memory tools + automatic recall)",
                "instalar el plugin {PLUGIN}@{MARKETPLACE} para tu usuario (herramientas de memoria + recall automático)"
            ),
            args: ["plugin", "install", &format!("{PLUGIN}@{MARKETPLACE}"), "--scope", "user"].map(String::from).to_vec(),
        });
    }
    Ok(pasos)
}

/// `~/.claude/settings.json` (o `CLAUDE_CONFIG_DIR`).
fn ajustes_claude() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join("settings.json"))
}

/// Enciende o apaga la actualización automática del marketplace en Claude Code: `autoUpdate` en su
/// entrada de `extraKnownMarketplaces` (Claude Code la deja apagada por defecto para los marketplaces
/// que no son de Anthropic). Si no está la entrada, la crea con el repositorio de GitHub.
pub fn auto_update(on: bool) -> Result<bool> {
    let Some(f) = ajustes_claude() else { return Ok(false) };
    let texto = std::fs::read_to_string(&f).unwrap_or_else(|_| "{}".into());
    let mut j: serde_json::Value = serde_json::from_str(texto.trim_start_matches('\u{feff}')).with_context(|| f.display().to_string())?;
    if !j["extraKnownMarketplaces"].is_object() {
        j["extraKnownMarketplaces"] = serde_json::json!({});
    }
    let m = &mut j["extraKnownMarketplaces"][MARKETPLACE];
    if !m.is_object() {
        *m = serde_json::json!({ "source": { "source": "github", "repo": MARKETPLACE_REPO } });
    }
    if m["autoUpdate"] == on {
        return Ok(false);
    }
    m["autoUpdate"] = serde_json::json!(on);
    let copia = f.with_file_name("settings.json.antes-de-omoikane");
    if f.exists() && !copia.exists() {
        std::fs::copy(&f, &copia)?;
    }
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&f, serde_json::to_string_pretty(&j)? + "\n")?;
    Ok(true)
}

/// Si el plugin de Omoikane está instalado en Claude Code.
pub fn instalado() -> bool {
    claude(&["plugin", "list"]).is_ok_and(|o| texto(&o).contains(&format!("{PLUGIN}@")))
}

pub fn ejecutar(pasos: &[Paso]) -> Result<()> {
    for p in pasos {
        let args: Vec<&str> = p.args.iter().map(String::as_str).collect();
        let o = claude(&args)?;
        if !o.status.success() {
            bail!(tr!("`claude {}` failed: {}", "`claude {}` falló: {}", args.join(" "), texto(&o).trim()));
        }
    }
    Ok(())
}
