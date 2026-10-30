//! Importar memorias de otro sistema: la exportación JSON de engram (`engram export`) o la del propio
//! `omoi export` (una memoria por línea). Se conserva lo que cada formato sabe (tipo, proyecto,
//! fijada, fechas) y lo que no tiene equivalente va como etiqueta. Lo que ya está (casi igual) en la
//! base se salta, salvo con `force`. De la exportación de Omoikane solo entra la versión actual, sin
//! el historial.

use crate::embeber::Modelo;
use crate::memoria::{self, Nueva, Tipo};
use crate::{proyecto, vectores};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Una memoria tal como se va a guardar, con sus fechas originales.
#[derive(Debug, Clone)]
pub struct Entrante {
    pub nueva: Nueva,
    pub creada: Option<String>,
    pub cambiada: Option<String>,
}

#[derive(Debug, Default)]
pub struct Resumen {
    pub guardadas: usize,
    pub duplicadas: usize,
    /// Las que no se pudieron guardar ni arreglando lo arreglable (se dice cuáles por stderr).
    pub fallidas: usize,
}

/// Lo que `memoria::guardar` no admite y se puede arreglar sin perder nada: un título de más de 200
/// caracteres se corta (y el entero pasa al principio del contenido) y un contenido vacío toma el título.
fn ajustar(mut n: Nueva) -> Nueva {
    if n.title.chars().count() > 200 {
        let corto: String = n.title.chars().take(199).collect();
        n.content = if n.content.trim().is_empty() { n.title.clone() } else { format!("{}\n\n{}", n.title, n.content) };
        n.title = format!("{corto}…");
    }
    if n.content.trim().is_empty() {
        n.content = n.title.clone();
    }
    n
}

/// ¿Hay ya una memoria con el mismo título y contenido en ese ámbito? Para cuando no hay modelo.
fn igual(db: &Connection, n: &Nueva) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory WHERE project_id = ?1 AND title = ?2 AND content = ?3)",
        params![crate::proyecto::resolver(db, &n.project)?.map_or(-1, |(id, _)| id), n.title, n.content],
        |f| f.get(0),
    )?)
}

#[derive(Deserialize)]
struct ExportEngram {
    #[serde(default)]
    sessions: Vec<SesionEngram>,
    observations: Vec<ObsEngram>,
}

#[derive(Deserialize)]
struct SesionEngram {
    id: String,
    #[serde(default)]
    directory: Option<String>,
}

#[derive(Deserialize)]
struct ObsEngram {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default, rename = "type")]
    tipo: Option<String>,
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    topic_key: Option<String>,
    #[serde(default, deserialize_with = "booleano")]
    pinned: bool,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

/// engram exporta `pinned` como booleano o como texto según la versión.
fn booleano<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    })
}

#[derive(Deserialize)]
struct LineaOmoikane {
    project: String,
    kind: Tipo,
    title: String,
    content: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    pinned: bool,
    created_at: Option<String>,
    updated_at: Option<String>,
}

/// Tipo de engram → tipo de Omoikane. Lo que no tiene equivalente es un `fact`.
fn tipo_de_engram(t: &str) -> Tipo {
    match t {
        "bugfix" => Tipo::Gotcha,
        "decision" => Tipo::Decision,
        "preference" => Tipo::Preference,
        _ => Tipo::Fact,
    }
}

/// Lee un fichero de exportación (el formato se reconoce solo). `dir` fuerza el proyecto de todas
/// las memorias que no son globales; sin él, las de engram van al proyecto de la carpeta de su
/// sesión si existe en este equipo, y si no al de `actual`.
pub fn leer(ruta: &Path, dir: Option<&Path>, actual: &str) -> Result<(Vec<Entrante>, usize)> {
    let texto = std::fs::read_to_string(ruta).with_context(|| format!("{}", ruta.display()))?;
    let forzado = dir.map(proyecto::de).transpose()?;
    let texto = texto.strip_prefix('\u{feff}').unwrap_or(&texto);
    // Un único objeto JSON con `observations` es una exportación de engram; si no se puede leer,
    // se dice por qué (y no «no es ni una cosa ni otra»).
    if texto.trim_start().starts_with('{') && texto.contains("\"observations\"") {
        let e: ExportEngram = serde_json::from_str(texto).with_context(|| format!("{} looks like an engram export but can't be read", ruta.display()))?;
        return Ok(de_engram(e, forzado.as_deref(), actual));
    }
    // Una memoria por línea: la exportación de Omoikane.
    let mut v = Vec::new();
    for (i, l) in texto.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let m: LineaOmoikane = serde_json::from_str(l).with_context(|| format!("line {} isn't an engram export nor an Omoikane export", i + 1))?;
        let project = match (&forzado, m.project.is_empty()) {
            (_, true) => String::new(),
            (Some(p), false) => p.clone(),
            (None, false) => m.project,
        };
        v.push(Entrante {
            nueva: Nueva { project, kind: Some(m.kind), title: m.title, content: m.content, paths: m.paths, tags: m.tags, pinned: m.pinned, source: "import:omoikane".into(), ..Default::default() },
            creada: m.created_at,
            cambiada: m.updated_at,
        });
    }
    if v.is_empty() {
        bail!("{} has no memories", ruta.display());
    }
    Ok((v, 0))
}

fn de_engram(e: ExportEngram, forzado: Option<&str>, actual: &str) -> (Vec<Entrante>, usize) {
    let dirs: HashMap<String, String> = e.sessions.into_iter().filter_map(|s| Some((s.id, s.directory?))).collect();
    let mut borradas = 0;
    let mut v = Vec::new();
    for o in e.observations {
        if o.deleted_at.as_deref().is_some_and(|d| !d.is_empty()) {
            borradas += 1;
            continue;
        }
        let project = if o.scope.as_deref() == Some("personal") {
            String::new()
        } else if let Some(p) = forzado {
            p.to_string()
        } else {
            o.session_id
                .as_ref()
                .and_then(|s| dirs.get(s))
                .filter(|d| Path::new(d).is_dir())
                .and_then(|d| proyecto::de(Path::new(d)).ok())
                .unwrap_or_else(|| actual.to_string())
        };
        let tipo = o.tipo.unwrap_or_default();
        let mut tags = Vec::new();
        if !tipo.is_empty() && !matches!(tipo.as_str(), "bugfix" | "decision" | "preference" | "manual") {
            tags.push(tipo.clone());
        }
        if let Some(t) = o.topic_key.filter(|t| !t.is_empty()) {
            tags.push(format!("topic:{t}"));
        }
        v.push(Entrante {
            nueva: Nueva {
                project,
                kind: Some(tipo_de_engram(&tipo)),
                title: o.title,
                content: o.content,
                paths: Vec::new(),
                tags,
                pinned: o.pinned,
                source: "import:engram".into(),
                ..Default::default()
            },
            creada: o.created_at,
            cambiada: o.updated_at,
        });
    }
    (v, borradas)
}

/// Guarda las memorias leídas, saltando las que ya están (salvo `force`), con sus fechas.
pub fn guardar(db: &mut Connection, modelo: Option<&Modelo>, entrantes: Vec<Entrante>, force: bool) -> Result<Resumen> {
    let mut r = Resumen::default();
    for e in entrantes {
        let nueva = ajustar(e.nueva);
        // Sin modelo, al menos lo idéntico: reimportar el mismo fichero no debe duplicarlo todo.
        let repetida = match modelo {
            _ if force => false,
            Some(m) => vectores::duplicada(db, m, &nueva.project, &nueva.title, &nueva.content)?.is_some(),
            None => igual(db, &nueva)?,
        };
        if repetida {
            r.duplicadas += 1;
            continue;
        }
        let titulo = nueva.title.clone();
        let id = match memoria::guardar(db, nueva, modelo) {
            Ok(m) => m.id,
            Err(err) => {
                // Una entrada rota no para la importación: se cuenta y se sigue.
                eprintln!("omoikane: «{titulo}»: {err:#}");
                r.fallidas += 1;
                continue;
            }
        };
        // Las fechas de origen, si se entienden: «2026-10-28 13:28:16» o ya en el formato propio.
        for (columna, valor) in [("created_at", e.creada), ("updated_at", e.cambiada)] {
            if let Some(v) = valor.filter(|v| !v.is_empty()) {
                db.execute(
                    &format!(
                        "UPDATE memory SET {columna} = strftime('%Y-%m-%dT%H:%M:%fZ', ?1) WHERE id = ?2 AND strftime('%Y-%m-%dT%H:%M:%fZ', ?1) IS NOT NULL"
                    ),
                    params![v.trim_end_matches('Z').replace('T', " "), id],
                )?;
            }
        }
        r.guardadas += 1;
    }
    Ok(r)
}
