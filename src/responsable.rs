//! La vista de responsable (1.7.0): quien lleva un cliente, o la factory entera, ve por espacio qué
//! hay (proyectos con su último `status`, lo fijado y las decisiones abiertas), quién lo escribió
//! (memorias y actividad por autor y por agente) y qué está desfasado o en pausa (lo que dice
//! `revisar`, los proyectos en pausa de la sincronización y lo que nadie toca desde hace meses).
//!
//! Solo lee, y solo lo que se ve desde donde se mira (`Ambito`): desde un proyecto, su espacio; desde
//! la carpeta del usuario, todos. Lo global no es de ningún cliente y no sale aquí.

use crate::memoria::{self, Ambito, Memoria};
use crate::{proyecto, revisar};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Los meses sin tocar a partir de los que una memoria sale como olvidada, si no se pide otra cosa.
pub const MESES: u32 = 6;
/// Cuántas filas se mandan de cada lista (las cuentas van enteras).
const POR_LISTA: usize = 12;
/// Los días que cuentan como actividad reciente de cada autor.
const DIAS_RECIENTES: u32 = 30;

/// Un grupo de la vista: un espacio, o los proyectos sin espacio (clave vacía).
#[derive(Default)]
struct Grupo<'a> {
    memorias: Vec<&'a Memoria>,
    proyectos: Vec<Value>,
    pausas: Vec<Value>,
    avisos: Vec<&'a revisar::Aviso>,
}

/// Lo de un autor dentro de un grupo.
#[derive(Default)]
struct Autor<'a> {
    memorias: usize,
    ultima: &'a str,
    recientes: usize,
    /// Por agente (`source`), cuántas.
    fuentes: BTreeMap<&'a str, usize>,
}

/// La memoria en pocas palabras, para las listas.
fn breve(m: &Memoria) -> Value {
    json!({
        "id": m.id,
        "title": m.title,
        "kind": m.kind.as_str(),
        "project": m.project,
        "author": m.author,
        "source": m.source,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
    })
}

/// Una fecha de la base menos `cuanto` (p. ej. `-6 months`), en el mismo formato que `updated_at`, para
/// comparar como texto.
fn hace(db: &Connection, cuanto: &str) -> Result<String> {
    Ok(db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now',?1)", [cuanto], |f| f.get(0))?)
}

/// Lo que ve el responsable desde `base`, agrupado por espacio. `meses`: desde cuándo una memoria sin
/// tocar cuenta como olvidada.
pub fn panorama(db: &Connection, ruta_db: &Path, base: &Ambito, meses: u32) -> Result<Value> {
    let meses = meses.clamp(1, 120);
    // Desde un proyecto, todo su espacio (los demás proyectos del cliente también), no solo lo suyo.
    let base = &base.amplio();
    let todas = memoria::recientes(db, base, None, usize::MAX >> 1)?;
    let avisos = revisar::avisos(db, base)?;
    // El espacio desde el que se mira (su clave; vacía: ninguno) y si se ve todo.
    let todo = matches!(base, Ambito::Todos);
    let propio = proyecto::clave_espacio(db, base.espacio())?;
    let se_ve = |clave: &str| todo || clave == propio;
    let corte = hace(db, &format!("-{meses} months"))?;
    let reciente = hace(db, &format!("-{DIAS_RECIENTES} days"))?;

    let mut grupos: BTreeMap<String, Grupo> = BTreeMap::new();
    if !todo {
        grupos.entry(propio.clone()).or_default();
    }
    for e in proyecto::espacios(db)? {
        if se_ve(&e.key) {
            grupos.entry(e.key).or_default();
        }
    }
    let mut globales = 0usize;
    for m in &todas {
        if m.project_id == 0 && m.space.is_empty() {
            globales += 1;
        } else if se_ve(&m.space) {
            grupos.entry(m.space.clone()).or_default().memorias.push(m);
        }
    }
    let por_id: HashMap<&str, &Memoria> = todas.iter().map(|m| (m.id.as_str(), m)).collect();
    for a in &avisos {
        if let Some(m) = por_id.get(a.memory_id.as_str())
            && !(m.project_id == 0 && m.space.is_empty())
            && let Some(g) = grupos.get_mut(&m.space)
        {
            g.avisos.push(a);
        }
    }
    // Los proyectos en pausa, por su clave: salen en su ficha y en la lista de su espacio.
    let pausas: HashMap<String, Value> = proyecto::en_pausa(db)?
        .iter()
        .filter(|p| se_ve(&p.here))
        .map(|p| {
            let v = json!({
                "name": p.name,
                "key": p.key,
                "machine": p.pausa.machine,
                "space": p.pausa.space,
                "text": crate::sync::texto_pausa(ruta_db, p),
                "keep": crate::sync::orden_quedarse(p),
                "accept": crate::sync::orden_aceptar(p),
            });
            if let Some(g) = grupos.get_mut(&p.here) {
                g.pausas.push(v.clone());
            }
            (p.key.clone(), v)
        })
        .collect();
    // El último `status` de cada proyecto (la lista va de la más nueva a la más vieja).
    let mut estados: HashMap<i64, &Memoria> = HashMap::new();
    for m in &todas {
        if m.project_id != 0 && m.tags.iter().any(|t| t == memoria::ESTADO) {
            estados.entry(m.project_id).or_insert(m);
        }
    }
    for p in proyecto::lista(db, None, None, usize::MAX)? {
        if !se_ve(&p.space) {
            continue;
        }
        let estado = estados.get(&p.id).map(|m| {
            json!({ "id": m.id, "title": m.title, "content": m.content, "updated_at": m.updated_at, "author": m.author })
        });
        let pinned = todas.iter().filter(|m| m.project_id == p.id && m.pinned).count();
        let v = json!({
            "name": p.name,
            "path": p.path,
            "key": p.key,
            "memories": p.memories,
            "last_update": p.last_update,
            "pinned": pinned,
            "status": estado,
            "gone": p.gone,
            "paused": pausas.contains_key(&p.key),
        });
        grupos.entry(p.space.clone()).or_default().proyectos.push(v);
    }
    let nombres: HashMap<String, String> = proyecto::espacios(db)?.into_iter().map(|e| (e.key, e.name)).collect();

    // Los espacios por nombre y, al final, los proyectos sin espacio.
    let mut grupos: Vec<(String, Grupo)> = grupos.into_iter().collect();
    grupos.sort_by_cached_key(|(k, _)| (k.is_empty(), nombres.get(k).map(|n| n.to_lowercase()).unwrap_or_default(), k.clone()));
    let espacios: Vec<Value> = grupos
        .into_iter()
        // Sin espacio y sin nada, el grupo no dice nada.
        .filter(|(k, g)| !k.is_empty() || !g.memorias.is_empty() || !g.proyectos.is_empty() || !g.pausas.is_empty())
        .map(|(clave, g)| {
            let ms = &g.memorias;
            // Por autor: cuántas, la última, cuántas en los últimos días y con qué agentes.
            let mut autores: BTreeMap<&str, Autor> = BTreeMap::new();
            for m in ms {
                let a = autores.entry(m.author.as_str()).or_default();
                a.memorias += 1;
                if m.updated_at.as_str() > a.ultima {
                    a.ultima = m.updated_at.as_str();
                }
                if m.updated_at >= reciente {
                    a.recientes += 1;
                }
                *a.fuentes.entry(m.source.as_str()).or_default() += 1;
            }
            let mut autores: Vec<Value> = autores
                .into_iter()
                .map(|(nombre, a)| {
                    let mut fuentes: Vec<(&str, usize)> = a.fuentes.into_iter().collect();
                    fuentes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                    json!({
                        "author": nombre,
                        "memories": a.memorias,
                        "last": a.ultima,
                        "recent": a.recientes,
                        "sources": fuentes.iter().map(|(s, n)| json!({ "source": s, "memories": n })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            autores.sort_by(|a, b| b["last"].as_str().cmp(&a["last"].as_str()));
            let abiertas: Vec<&&Memoria> =
                ms.iter().filter(|m| m.kind == memoria::Tipo::Decision && m.tags.iter().any(|t| t == "open")).collect();
            let fijadas: Vec<&&Memoria> = ms.iter().filter(|m| m.pinned).collect();
            // Las que nadie toca desde el corte, de la más vieja a la más nueva.
            let mut olvidadas: Vec<&&Memoria> = ms.iter().filter(|m| m.updated_at < corte).collect();
            olvidadas.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            let mut por_tipo: BTreeMap<&str, usize> = BTreeMap::new();
            for a in &g.avisos {
                *por_tipo.entry(a.kind.as_str()).or_default() += 1;
            }
            let lista = |v: &[&&Memoria]| v.iter().take(POR_LISTA).map(|m| breve(m)).collect::<Vec<_>>();
            json!({
                "key": clave,
                "name": nombres.get(&clave).cloned().unwrap_or_default(),
                "memories": ms.len(),
                // Las del propio espacio, sin proyecto.
                "space_memories": ms.iter().filter(|m| m.project_id == 0).count(),
                "projects": g.proyectos,
                "pinned": { "count": fijadas.len(), "items": lista(&fijadas) },
                "open_decisions": { "count": abiertas.len(), "items": lista(&abiertas) },
                "authors": autores,
                "recent": ms.iter().take(POR_LISTA).map(|m| breve(m)).collect::<Vec<_>>(),
                "review": {
                    "count": g.avisos.len(),
                    "kinds": por_tipo,
                    "items": g.avisos.iter().take(POR_LISTA * 2).map(|a| json!({
                        "kind": a.kind,
                        "memory_id": a.memory_id,
                        "title": a.title,
                        "project": a.project,
                        "other_id": a.other_id,
                        "other_title": a.other_title,
                        "detail": a.detail,
                    })).collect::<Vec<_>>(),
                },
                "paused": g.pausas,
                "untouched": { "count": olvidadas.len(), "items": lista(&olvidadas) },
            })
        })
        .collect();
    Ok(json!({
        // Desde dónde se mira: todo (la carpeta del usuario) o un espacio (vacío: los proyectos sin espacio).
        "scope": if todo { "all" } else { "space" },
        "space": if todo { Value::Null } else { propio.into() },
        "months": meses,
        "recent_days": DIAS_RECIENTES,
        "global_memories": globales,
        "spaces": espacios,
    }))
}
