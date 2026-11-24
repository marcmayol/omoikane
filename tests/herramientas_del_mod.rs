//! Every tool that `omoi tool` lists is registered by the Claude Code mod (hooks/omoikane.ts),
//! so it must also have its `on('tool.call', …)`: without it, the call fails with «no hook
//! answered this call». It happened to `decisions` in 1.10.0 and 1.10.1.

use std::process::Command;

#[test]
fn every_tool_has_its_handler_in_the_mod() {
    let output = Command::new(env!("CARGO_BIN_EXE_omoi")).arg("tool").output().unwrap();
    assert!(output.status.success());
    let list: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let module = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/omoikane.ts")).unwrap();
    let names: Vec<&str> = list["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!names.is_empty());
    let missing: Vec<&&str> = names
        .iter()
        .filter(|n| !module.contains(&format!("{{ tool: 'mcp__omoikane__{n}' }}, ($, e) => runTool($, '{n}', e))")))
        .collect();
    assert!(missing.is_empty(), "herramientas sin on('tool.call') en hooks/omoikane.ts: {missing:?}");
}
