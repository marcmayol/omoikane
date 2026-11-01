//! Los dos motores de EmbeddingGemma (candle y `rapido`, con AVX2 y por el camino genérico) frente a
//! frente: mismo vector, cuánto tardan según la longitud y cuánto cuesta indexar de una en una, por
//! lotes o una memoria por núcleo.
//!
//! ```text
//! cargo run --release --example motor_igual -- igual  <modelo> <textos.json> [paso]
//! cargo run --release --example motor_igual -- tiempo <modelo> <largos.json> <rondas>
//! cargo run --release --example motor_igual -- indexar <modelo> <textos.json> <grupo> <variante> [n]
//! cargo run --release --example motor_igual -- carga  <modelo> <rapido|candle>
//! ```

use omoikane::gemma::{Gemma, Tipo};
use std::time::Instant;

fn coseno(a: &[f32], b: &[f32]) -> f32 {
    let p: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    p / (a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}

fn mediana(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn textos(f: &str) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    Ok(serde_json::from_slice(&std::fs::read(f)?)?)
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::Path::new(&a[1]);
    let hilos = Gemma::hilos_por_defecto();
    match a[0].as_str() {
        "carga" => {
            let t = Instant::now();
            let g = if a[2] == "candle" { Gemma::cargar_candle(dir)? } else { Gemma::cargar_rapido(dir, hilos, omoikane::rapido::simd_disponible())? };
            let carga = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            g.vector("How do I undo a cargo yank?", Tipo::Pregunta)?;
            let primero = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            g.vector("How do I undo a cargo yank?", Tipo::Pregunta)?;
            println!(
                "{{\"carga_ms\": {carga:.1}, \"primer_vector_ms\": {primero:.1}, \"segundo_ms\": {:.1}, \"bpe\": {}}}",
                t.elapsed().as_secs_f64() * 1000.0,
                g.tokenizer_compilado()
            );
        }
        "igual" => {
            let paso: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let c = Gemma::cargar_candle(dir)?;
            let r = Gemma::cargar_rapido(dir, hilos, true)?;
            let gene = Gemma::cargar_rapido(dir, hilos, false)?;
            println!("tokenizer compilado: candle {} rápido {}", c.tokenizer_compilado(), r.tokenizer_compilado());
            for (g, v) in textos(&a[2])? {
                let tipo = if g == "preguntas" || g == "mensajes" { Tipo::Pregunta } else { Tipo::Documento };
                let ts: Vec<&str> = v.as_array().unwrap().iter().step_by(paso).map(|t| t.as_str().unwrap()).collect();
                let (mut min_cr, mut min_cg, mut min_rg, mut max_lote, mut errores) = (1f32, 1f32, 1f32, 0f32, 0);
                let mut buenos = Vec::new();
                let mut uno_a_uno = Vec::new();
                for t in &ts {
                    let (vc, vr, vg) = match (c.vector(t, tipo), r.vector(t, tipo), gene.vector(t, tipo)) {
                        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                        _ => {
                            errores += 1;
                            continue;
                        }
                    };
                    min_cr = min_cr.min(coseno(&vc, &vr));
                    min_cg = min_cg.min(coseno(&vc, &vg));
                    min_rg = min_rg.min(coseno(&vr, &vg));
                    buenos.push(*t);
                    uno_a_uno.push(vr);
                }
                // Por lotes de 8 textos: bit a bit lo mismo que de uno en uno.
                for (trozo, solos) in buenos.chunks(8).zip(uno_a_uno.chunks(8)) {
                    for (x, y) in r.vectores(trozo, tipo)?.iter().zip(solos) {
                        max_lote = max_lote.max(x.iter().zip(y).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max));
                    }
                }
                println!(
                    "{g}: {} textos ({errores} con error); coseno mín candle-rápido {min_cr:.6}, candle-genérico {min_cg:.6}, rápido-genérico {min_rg:.6}; lote frente a uno a uno, diferencia máx {max_lote:e}",
                    buenos.len()
                );
            }
        }
        "tiempo" => {
            let largos: Vec<String> = serde_json::from_slice(&std::fs::read(&a[2])?)?;
            let rondas: usize = a[3].parse()?;
            let c = Gemma::cargar_candle(dir)?;
            let r = Gemma::cargar_rapido(dir, hilos, true)?;
            let gene = Gemma::cargar_rapido(dir, hilos, false)?;
            let gs = [&c, &r, &gene];
            println!("tokens\tcandle\trapido\tgenerico (ms, mediana de {rondas}, intercalados)");
            for t in &largos {
                let n = r.ids(t, Tipo::Documento)?.len();
                let mut ms = [Vec::new(), Vec::new(), Vec::new()];
                for i in 0..=rondas {
                    // El orden rota en cada ronda; la primera no cuenta.
                    for k in 0..3 {
                        let j = (i + k) % 3;
                        let t0 = Instant::now();
                        gs[j].vector(t, Tipo::Documento)?;
                        if i > 0 {
                            ms[j].push(t0.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
                let [a, b, c] = ms;
                println!("{n}\t{:.1}\t{:.1}\t{:.1}", mediana(a), mediana(b), mediana(c));
            }
        }
        "indexar" => {
            // Una variante cada vez (cada una en su proceso, para medir su RAM aparte).
            let grupo = &a[3];
            let variante = a[4].as_str();
            let n: usize = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
            let todos = textos(&a[2])?;
            let ts: Vec<&str> = todos[grupo.as_str()].as_array().unwrap().iter().take(n).map(|t| t.as_str().unwrap()).collect();
            let hecho = match variante {
                "candle" => {
                    let g = Gemma::cargar_candle(dir)?;
                    let t0 = Instant::now();
                    for t in &ts {
                        g.vector(t, Tipo::Documento)?;
                    }
                    t0.elapsed()
                }
                "uno" => {
                    let g = Gemma::cargar_rapido(dir, hilos, true)?;
                    let t0 = Instant::now();
                    for t in &ts {
                        g.vector(t, Tipo::Documento)?;
                    }
                    t0.elapsed()
                }
                v if v.starts_with("lote") => {
                    // lote<N>: lotes de hasta N tokens (un texto más largo va solo).
                    let tope: usize = v[4..].parse()?;
                    let g = Gemma::cargar_rapido(dir, hilos, true)?;
                    let t0 = Instant::now();
                    let ids: Vec<Vec<u32>> = ts.iter().map(|t| g.ids(t, Tipo::Documento)).collect::<anyhow::Result<_>>()?;
                    let mut i = 0;
                    while i < ids.len() {
                        let (mut j, mut suma) = (i, 0);
                        while j < ids.len() && (j == i || suma + ids[j].len() <= tope) {
                            suma += ids[j].len();
                            j += 1;
                        }
                        g.vectores_de_ids(&ids[i..j], &|| {})?;
                        i = j;
                    }
                    t0.elapsed()
                }
                "nucleos" => {
                    // Una memoria por núcleo, cada una con su motor de un hilo (los pesos mapeados se comparten).
                    let gs: Vec<Gemma> = (0..hilos).map(|_| Gemma::cargar_rapido(dir, 1, true)).collect::<anyhow::Result<_>>()?;
                    let t0 = Instant::now();
                    let sig = std::sync::atomic::AtomicUsize::new(0);
                    std::thread::scope(|s| {
                        for g in &gs {
                            let (sig, ts) = (&sig, &ts);
                            s.spawn(move || {
                                loop {
                                    let i = sig.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    if i >= ts.len() {
                                        break;
                                    }
                                    g.vector(ts[i], Tipo::Documento).unwrap();
                                }
                            });
                        }
                    });
                    t0.elapsed()
                }
                v => anyhow::bail!("variante {v}"),
            };
            println!(
                "{{\"grupo\": \"{grupo}\", \"variante\": \"{variante}\", \"textos\": {}, \"ms_por_texto\": {:.2}}}",
                ts.len(),
                hecho.as_secs_f64() * 1000.0 / ts.len() as f64
            );
        }
        o => anyhow::bail!("orden {o}"),
    }
    Ok(())
}
