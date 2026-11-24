//! `omoi setup claude`: gets Omoikane ready in Claude Code with its own CLI (`claude plugin …`):
//! adds the marketplace and installs the plugin, which brings the MCP server and the recall hooks. If it's
//! already done, it touches nothing.

use crate::tr;
use anyhow::{Context, Result, bail};
use std::process::{Command, Output};

pub const MARKETPLACE_REPO: &str = "marcmayol/drac-plugins";
pub const MARKETPLACE: &str = "drac";
pub const PLUGIN: &str = "omoikane";

/// `claude` can be an .exe or, installed with npm on Windows, a .cmd that can't be launched directly.
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

fn output_text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// One installation step: what it does and with which command.
pub struct Step {
    pub what: String,
    pub args: Vec<String>,
}

/// What is left to do on this machine (empty if everything is done).
pub fn plan() -> Result<Vec<Step>> {
    let version = claude(&["--version"]);
    if !version.as_ref().is_ok_and(|o| o.status.success()) {
        bail!(tr!(
            "Claude Code (`claude`) is not installed or not on the PATH. Install it first: https://code.claude.com",
            "Claude Code (`claude`) no está instalado o no está en el PATH. Instálalo primero: https://code.claude.com"
        ));
    }
    let mut steps = Vec::new();
    let marketplaces = output_text(&claude(&["plugin", "marketplace", "list"])?);
    // Each marketplace shows up as «❯ name»; any source is fine (GitHub or a local folder).
    let has_marketplace = marketplaces.lines().any(|l| l.trim().trim_start_matches('❯').trim() == MARKETPLACE);
    if !has_marketplace {
        steps.push(Step {
            what: tr!("add the marketplace {MARKETPLACE_REPO}", "añadir el marketplace {MARKETPLACE_REPO}"),
            args: ["plugin", "marketplace", "add", MARKETPLACE_REPO].map(String::from).to_vec(),
        });
    }
    let plugins = output_text(&claude(&["plugin", "list"])?);
    if !plugins.contains(&format!("{PLUGIN}@")) {
        if has_marketplace {
            // A `drac` marketplace added earlier can be a copy from before the plugin existed.
            steps.push(Step {
                what: tr!("update the marketplace {MARKETPLACE}", "actualizar el marketplace {MARKETPLACE}"),
                args: ["plugin", "marketplace", "update", MARKETPLACE].map(String::from).to_vec(),
            });
        }
        steps.push(Step {
            what: tr!(
                "install the plugin {PLUGIN}@{MARKETPLACE} for your user (memory tools + automatic recall)",
                "instalar el plugin {PLUGIN}@{MARKETPLACE} para tu usuario (herramientas de memoria + recall automático)"
            ),
            args: ["plugin", "install", &format!("{PLUGIN}@{MARKETPLACE}"), "--scope", "user"].map(String::from).to_vec(),
        });
    }
    Ok(steps)
}

/// `~/.claude/settings.json` (or `CLAUDE_CONFIG_DIR`).
fn claude_settings() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).or_else(|| std::env::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join("settings.json"))
}

/// Turns the marketplace's automatic update in Claude Code on or off: `autoUpdate` in its
/// `extraKnownMarketplaces` entry (Claude Code leaves it off by default for marketplaces
/// that aren't Anthropic's). If the entry isn't there, it creates it with the GitHub repository.
pub fn auto_update(on: bool) -> Result<bool> {
    let Some(f) = claude_settings() else { return Ok(false) };
    let text = std::fs::read_to_string(&f).unwrap_or_else(|_| "{}".into());
    let mut j: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).with_context(|| f.display().to_string())?;
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
    let backup = f.with_file_name("settings.json.antes-de-omoikane");
    if f.exists() && !backup.exists() {
        std::fs::copy(&f, &backup)?;
    }
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&f, serde_json::to_string_pretty(&j)? + "\n")?;
    Ok(true)
}

/// Whether the Omoikane plugin is installed in Claude Code.
pub fn installed() -> bool {
    claude(&["plugin", "list"]).is_ok_and(|o| output_text(&o).contains(&format!("{PLUGIN}@")))
}

pub fn run(steps: &[Step]) -> Result<()> {
    for p in steps {
        let args: Vec<&str> = p.args.iter().map(String::as_str).collect();
        let o = claude(&args)?;
        if !o.status.success() {
            bail!(tr!("`claude {}` failed: {}", "`claude {}` falló: {}", args.join(" "), output_text(&o).trim()));
        }
    }
    Ok(())
}
