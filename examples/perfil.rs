//! Dónde se van los 1,65 s de cargar el modelo multilingüe.

use std::time::Instant;
use tokenizers::Tokenizer;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() -> anyhow::Result<()> {
    let dir = dirs::home_dir().unwrap().join(".omoikane/models/potion-multilingual-128M-int8");

    let t = Instant::now();
    let tok_b = std::fs::read(dir.join("tokenizer.json"))?;
    let mod_b = std::fs::read(dir.join("model.safetensors"))?;
    println!("leer ficheros              {:>7.0} ms", ms(t));

    let t = Instant::now();
    let v: serde_json::Value = serde_json::from_slice(&tok_b)?;
    println!("solo parsear el JSON       {:>7.0} ms", ms(t));
    drop(v);

    let t = Instant::now();
    let tok = Tokenizer::from_bytes(&tok_b).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Tokenizer::from_bytes      {:>7.0} ms", ms(t));

    let t = Instant::now();
    let n = tok.get_vocab(false).len();
    println!("get_vocab ({n})        {:>7.0} ms", ms(t));

    let t = Instant::now();
    let v = serde_json::to_value(tok.get_model())?;
    println!("to_value(model) [unk_id]   {:>7.0} ms", ms(t));
    drop(v);

    let t = Instant::now();
    let raw: Vec<i8> = mod_b[..].iter().map(|&b| b as i8).collect();
    println!("copiar tabla int8          {:>7.0} ms ({} MB)", ms(t), raw.len() >> 20);

    let t = Instant::now();
    let m = model2vec_rs::model::StaticModel::from_bytes(&tok_b, &mod_b, std::fs::read(dir.join("config.json"))?, None)?;
    println!("from_bytes completo        {:>7.0} ms", ms(t));
    drop(m);
    Ok(())
}
