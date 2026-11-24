//! The two EmbeddingGemma engines (candle and `engine`, with AVX2 and through the generic path) head
//! to head: same vector, how long they take by length and how much indexing costs one at a time, in
//! batches or one memory per core.
//!
//! ```text
//! cargo run --release --example motor_igual -- igual  <modelo> <textos.json> [paso]
//! cargo run --release --example motor_igual -- tiempo <modelo> <largos.json> <rondas>
//! cargo run --release --example motor_igual -- indexar <modelo> <textos.json> <grupo> <variante> [n]
//! cargo run --release --example motor_igual -- carga  <modelo> <rapido|candle>
//! ```

use omoikane::gemma::{Gemma, TextKind};
use std::time::Instant;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let p: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    p / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn texts(f: &str) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    Ok(serde_json::from_slice(&std::fs::read(f)?)?)
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::Path::new(&a[1]);
    let threads = Gemma::default_threads();
    match a[0].as_str() {
        "carga" => {
            let t = Instant::now();
            let g = if a[2] == "candle" { Gemma::load_candle(dir)? } else { Gemma::load_fast(dir, threads, omoikane::engine::simd_available())? };
            let load = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            g.vector("How do I undo a cargo yank?", TextKind::Query)?;
            let first = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            g.vector("How do I undo a cargo yank?", TextKind::Query)?;
            println!(
                "{{\"carga_ms\": {load:.1}, \"primer_vector_ms\": {first:.1}, \"segundo_ms\": {:.1}, \"bpe\": {}}}",
                t.elapsed().as_secs_f64() * 1000.0,
                g.compiled_tokenizer()
            );
        }
        "igual" => {
            let step: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let c = Gemma::load_candle(dir)?;
            let r = Gemma::load_fast(dir, threads, true)?;
            let generic = Gemma::load_fast(dir, threads, false)?;
            println!("tokenizer compilado: candle {} rápido {}", c.compiled_tokenizer(), r.compiled_tokenizer());
            for (g, v) in texts(&a[2])? {
                let kind = if g == "preguntas" || g == "mensajes" { TextKind::Query } else { TextKind::Document };
                let ts: Vec<&str> = v.as_array().unwrap().iter().step_by(step).map(|t| t.as_str().unwrap()).collect();
                let (mut min_cr, mut min_cg, mut min_rg, mut max_batch, mut errors) = (1f32, 1f32, 1f32, 0f32, 0);
                let mut good = Vec::new();
                let mut one_by_one = Vec::new();
                for t in &ts {
                    let (vc, vr, vg) = match (c.vector(t, kind), r.vector(t, kind), generic.vector(t, kind)) {
                        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                        _ => {
                            errors += 1;
                            continue;
                        }
                    };
                    min_cr = min_cr.min(cosine(&vc, &vr));
                    min_cg = min_cg.min(cosine(&vc, &vg));
                    min_rg = min_rg.min(cosine(&vr, &vg));
                    good.push(*t);
                    one_by_one.push(vr);
                }
                // In batches of 8 texts: bit for bit the same as one at a time.
                for (chunk, singles) in good.chunks(8).zip(one_by_one.chunks(8)) {
                    for (x, y) in r.vectors(chunk, kind)?.iter().zip(singles) {
                        max_batch = max_batch.max(x.iter().zip(y).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max));
                    }
                }
                println!(
                    "{g}: {} textos ({errors} con error); coseno mín candle-rápido {min_cr:.6}, candle-genérico {min_cg:.6}, rápido-genérico {min_rg:.6}; lote frente a uno a uno, diferencia máx {max_batch:e}",
                    good.len()
                );
            }
        }
        "tiempo" => {
            let long_texts: Vec<String> = serde_json::from_slice(&std::fs::read(&a[2])?)?;
            let rounds: usize = a[3].parse()?;
            let c = Gemma::load_candle(dir)?;
            let r = Gemma::load_fast(dir, threads, true)?;
            let generic = Gemma::load_fast(dir, threads, false)?;
            let gs = [&c, &r, &generic];
            println!("tokens\tcandle\trapido\tgenerico (ms, mediana de {rounds}, intercalados)");
            for t in &long_texts {
                let n = r.ids(t, TextKind::Document)?.len();
                let mut ms = [Vec::new(), Vec::new(), Vec::new()];
                for i in 0..=rounds {
                    // The order rotates each round; the first doesn't count.
                    for k in 0..3 {
                        let j = (i + k) % 3;
                        let t0 = Instant::now();
                        gs[j].vector(t, TextKind::Document)?;
                        if i > 0 {
                            ms[j].push(t0.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
                let [a, b, c] = ms;
                println!("{n}\t{:.1}\t{:.1}\t{:.1}", median(a), median(b), median(c));
            }
        }
        "indexar" => {
            // One variant at a time (each in its own process, to measure its RAM separately).
            let group = &a[3];
            let variant = a[4].as_str();
            let n: usize = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            let all = texts(&a[2])?;
            let ts: Vec<&str> = all[group.as_str()].as_array().unwrap().iter().take(n).map(|t| t.as_str().unwrap()).collect();
            let elapsed = match variant {
                "candle" => {
                    let g = Gemma::load_candle(dir)?;
                    let t0 = Instant::now();
                    for t in &ts {
                        g.vector(t, TextKind::Document)?;
                    }
                    t0.elapsed()
                }
                "uno" => {
                    let g = Gemma::load_fast(dir, threads, true)?;
                    let t0 = Instant::now();
                    for t in &ts {
                        g.vector(t, TextKind::Document)?;
                    }
                    t0.elapsed()
                }
                v if v.starts_with("lote") => {
                    // lote<N>: batches of up to N tokens (a longer text goes alone).
                    let cap: usize = v[4..].parse()?;
                    let g = Gemma::load_fast(dir, threads, true)?;
                    let t0 = Instant::now();
                    let ids: Vec<Vec<u32>> = ts.iter().map(|t| g.ids(t, TextKind::Document)).collect::<anyhow::Result<_>>()?;
                    let mut i = 0;
                    while i < ids.len() {
                        let (mut j, mut sum) = (i, 0);
                        while j < ids.len() && (j == i || sum + ids[j].len() <= cap) {
                            sum += ids[j].len();
                            j += 1;
                        }
                        g.vectors_from_ids(&ids[i..j], &|| {})?;
                        i = j;
                    }
                    t0.elapsed()
                }
                "nucleos" => {
                    // One memory per core, each with its own single-thread engine (the mapped weights are shared).
                    let gs: Vec<Gemma> = (0..threads).map(|_| Gemma::load_fast(dir, 1, true)).collect::<anyhow::Result<_>>()?;
                    let t0 = Instant::now();
                    let next = std::sync::atomic::AtomicUsize::new(0);
                    std::thread::scope(|s| {
                        for g in &gs {
                            let (next, ts) = (&next, &ts);
                            s.spawn(move || {
                                loop {
                                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    if i >= ts.len() {
                                        break;
                                    }
                                    g.vector(ts[i], TextKind::Document).unwrap();
                                }
                            });
                        }
                    });
                    t0.elapsed()
                }
                v => anyhow::bail!("variante {v}"),
            };
            println!(
                "{{\"grupo\": \"{group}\", \"variante\": \"{variant}\", \"textos\": {}, \"ms_por_texto\": {:.2}}}",
                ts.len(),
                elapsed.as_secs_f64() * 1000.0 / ts.len() as f64
            );
        }
        o => anyhow::bail!("orden {o}"),
    }
    Ok(())
}
