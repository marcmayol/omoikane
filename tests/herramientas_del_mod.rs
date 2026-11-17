//! Cada herramienta que lista `omoi tool` la da de alta el mod de Claude Code (hooks/omoikane.ts),
//! así que también tiene que tener su `on('tool.call', …)`: sin él, la llamada falla con «no hook
//! answered this call». Le pasó a `decisions` en 1.10.0 y 1.10.1.

use std::process::Command;

#[test]
fn cada_herramienta_tiene_su_manejador_en_el_mod() {
    let salida = Command::new(env!("CARGO_BIN_EXE_omoi")).arg("tool").output().unwrap();
    assert!(salida.status.success());
    let lista: serde_json::Value = serde_json::from_slice(&salida.stdout).unwrap();
    let modulo = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/omoikane.ts")).unwrap();
    let nombres: Vec<&str> = lista["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!nombres.is_empty());
    let faltan: Vec<&&str> = nombres
        .iter()
        .filter(|n| !modulo.contains(&format!("{{ tool: 'mcp__omoikane__{n}' }}, ($, e) => runTool($, '{n}', e))")))
        .collect();
    assert!(faltan.is_empty(), "herramientas sin on('tool.call') en hooks/omoikane.ts: {faltan:?}");
}
