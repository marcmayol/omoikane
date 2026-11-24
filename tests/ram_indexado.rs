//! The RAM cap of the meaning model's indexing: `omoi model semantic --ram`, the variable
//! `OMOIKANE_SEMANTIC_RAM` on top, the minimum (the longest memory always fits) and what
//! `omoi model semantic` and `omoi doctor` show.

mod comun;

use omoikane::significado::{self as s, RamSource, MIN_RAM_MB, DEFAULT_RAM_MB};
use std::process::Command;

fn omoi(home: &std::path::Path, args: &[&str], ram: Option<&str>) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_omoi"));
    c.args(args).env("OMOIKANE_HOME", home).env("OMOIKANE_DB", home.join("o.db")).env("OMOIKANE_MODELS", home.join("models")).env("OMOIKANE_AUTO_UPDATE", "0").env("OMOIKANE_LANG", "en").env_remove("OMOIKANE_SEMANTIC_RAM");
    if let Some(r) = ram {
        c.env("OMOIKANE_SEMANTIC_RAM", r);
    }
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn ram_cap() {
    let home = comun::Temporal::nueva("ram");
    // SAFETY: the only test in this file (a separate process).
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &home);
        std::env::remove_var("OMOIKANE_SEMANTIC_RAM");
    }
    const { assert!(MIN_RAM_MB <= DEFAULT_RAM_MB) };
    // With the minimum the longest memory fits on its own.
    assert_eq!(s::ram_cap(), (DEFAULT_RAM_MB, RamSource::BuiltIn));
    assert_eq!(s::set_ram_cap(Some(1)).unwrap(), MIN_RAM_MB);
    assert!(s::tokens_per_batch() >= omoikane::gemma::MAX_TOKENS);
    assert_eq!(s::set_ram_cap(Some(500)).unwrap(), 500);
    assert_eq!(s::ram_cap(), (500, RamSource::Chosen));
    let more = s::tokens_per_batch();
    assert!(more > omoikane::gemma::MAX_TOKENS * 3, "{more}");
    // The variable rules over what was chosen (and doesn't go below the minimum either).
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_RAM", "200") };
    assert_eq!(s::ram_cap(), (200, RamSource::Env));
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_RAM", "3") };
    assert_eq!(s::ram_cap(), (MIN_RAM_MB, RamSource::Env));
    unsafe { std::env::remove_var("OMOIKANE_SEMANTIC_RAM") };
    assert_eq!(s::set_ram_cap(None).unwrap(), DEFAULT_RAM_MB);

    // From the command line.
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic", "--ram", "300"], None)).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(300), Some("chosen")));
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic"], Some("150"))).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(150), Some("env")));
    assert!(omoi(&home, &["model", "semantic"], None).contains("indexing RAM: up to 300 MB"));
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic", "--ram", "0"], None)).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(DEFAULT_RAM_MB), Some("default")));
}
