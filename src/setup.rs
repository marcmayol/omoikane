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
        pasos.push(Paso {
            que: tr!(
                "install the plugin {PLUGIN}@{MARKETPLACE} for your user (MCP server + recall hooks)",
                "instalar el plugin {PLUGIN}@{MARKETPLACE} para tu usuario (servidor MCP + hooks del recall)"
            ),
            args: ["plugin", "install", &format!("{PLUGIN}@{MARKETPLACE}"), "--scope", "user"].map(String::from).to_vec(),
        });
    }
    Ok(pasos)
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
