//! El tope de RAM del indexado del modelo de significado: `omoi model semantic --ram`, la variable
//! `OMOIKANE_SEMANTIC_RAM` por encima, el mínimo (siempre cabe la memoria más larga) y lo que enseñan
//! `omoi model semantic` y `omoi doctor`.

mod comun;

use omoikane::significado::{self as s, OrigenRam, RAM_MINIMA_MB, RAM_POR_DEFECTO_MB};
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
fn tope_de_ram() {
    let home = comun::Temporal::nueva("ram");
    // SAFETY: único test de este fichero (un proceso aparte).
    unsafe {
        std::env::set_var("OMOIKANE_HOME", &home);
        std::env::remove_var("OMOIKANE_SEMANTIC_RAM");
    }
    const { assert!(RAM_MINIMA_MB <= RAM_POR_DEFECTO_MB) };
    // Con el mínimo cabe sola la memoria más larga.
    assert_eq!(s::tope_ram(), (RAM_POR_DEFECTO_MB, OrigenRam::PorDefecto));
    assert_eq!(s::fijar_ram(Some(1)).unwrap(), RAM_MINIMA_MB);
    assert!(s::tokens_por_lote() >= omoikane::gemma::MAX_TOKENS);
    assert_eq!(s::fijar_ram(Some(500)).unwrap(), 500);
    assert_eq!(s::tope_ram(), (500, OrigenRam::Elegido));
    let mas = s::tokens_por_lote();
    assert!(mas > omoikane::gemma::MAX_TOKENS * 3, "{mas}");
    // La variable manda por encima de lo elegido (y tampoco baja del mínimo).
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_RAM", "200") };
    assert_eq!(s::tope_ram(), (200, OrigenRam::Entorno));
    unsafe { std::env::set_var("OMOIKANE_SEMANTIC_RAM", "3") };
    assert_eq!(s::tope_ram(), (RAM_MINIMA_MB, OrigenRam::Entorno));
    unsafe { std::env::remove_var("OMOIKANE_SEMANTIC_RAM") };
    assert_eq!(s::fijar_ram(None).unwrap(), RAM_POR_DEFECTO_MB);

    // Por la línea de órdenes.
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic", "--ram", "300"], None)).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(300), Some("chosen")));
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic"], Some("150"))).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(150), Some("env")));
    assert!(omoi(&home, &["model", "semantic"], None).contains("indexing RAM: up to 300 MB"));
    let j: serde_json::Value = serde_json::from_str(&omoi(&home, &["--json", "model", "semantic", "--ram", "0"], None)).unwrap();
    assert_eq!((j["indexing_ram_mb"].as_u64(), j["indexing_ram_from"].as_str()), (Some(RAM_POR_DEFECTO_MB), Some("default")));
}
