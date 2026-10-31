//! Memorias: guardar, leer, cambiar (guardando la versión anterior), olvidar y listar.
//!
//! Toda escritura toca `memory`, `memory_fts` y `memory_path` en una sola transacción `IMMEDIATE`:
//! nunca queda una memoria sin indexar ni un índice que apunte a una memoria borrada.

use crate::embeber::Modelo;
use crate::{tr, vectores};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use std::str::FromStr;

/// Cada tipo cambia cómo se recupera la memoria (ver PLAN.md §3). Un tipo que esta versión no conoce
/// (de una versión futura, por la sincronización) se guarda tal cual en la base y se trata como `fact`
/// (ver `Memoria::kind_text`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tipo {
    #[default]
    Fact,
    Decision,
    Gotcha,
    Preference,
    /// Una receta: pasos que se siguen (publicar una versión, desplegar…), con una línea `when:` de
    /// las palabras que la traen entera (Fase 7.3).
    Procedure,
}

impl Tipo {
    pub const TODOS: [Tipo; 5] = [Tipo::Fact, Tipo::Decision, Tipo::Gotcha, Tipo::Preference, Tipo::Procedure];

    pub fn as_str(self) -> &'static str {
        match self {
            Tipo::Fact => "fact",
            Tipo::Decision => "decision",
            Tipo::Gotcha => "gotcha",
            Tipo::Preference => "preference",
            Tipo::Procedure => "procedure",
        }
    }
}

impl FromStr for Tipo {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Tipo> {
        Tipo::TODOS
            .into_iter()
            .find(|t| t.as_str() == s.trim().to_lowercase())
            .ok_or_else(|| anyhow!(tr!("unknown kind «{s}»: use fact, decision, gotcha, preference or procedure", "tipo «{s}» desconocido: usa fact, decision, gotcha, preference o procedure")))
    }
}

impl std::fmt::Display for Tipo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Memoria {
    pub id: String,
    /// Carpeta principal de su proyecto (`project.path`); vacío = global o de espacio.
    pub project: String,
    /// Su proyecto en esta base (0 = ninguno). No sale en el JSON: entre equipos vale la clave.
    #[serde(skip)]
    pub project_id: i64,
    /// La clave de sincronización de su proyecto (vacía si no tiene).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project_key: String,
    /// El espacio (su clave): el de la memoria si es de espacio (sin proyecto), o el de su proyecto.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub space: String,
    pub kind: Tipo,
    /// El tipo tal como está guardado, cuando esta versión no lo conoce (y `kind` dice `fact`): así no
    /// se pierde al reexportarlo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_text: Option<String>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
    /// El sello de su último cambio de texto: solo en la copia que va a la papelera (para que sus
    /// veredictos sigan valiendo si vuelve). Al leerla de la base va vacío.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_hlc: String,
    pub created_at: String,
    pub updated_at: String,
    pub recalled_at: Option<String>,
    pub recall_count: i64,
    /// Quién la escribió (el nombre de git), si la sincronización lo apunta.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Lo que `secretos` quitó al guardarla o cambiarla en esta llamada, un tipo por hallazgo (vacío si
    /// nada). No se guarda ni sale en el JSON: es para que quien guarda lo diga.
    #[serde(skip)]
    pub redacted: Vec<&'static str>,
}

impl Memoria {
    /// De un espacio, sin proyecto.
    pub fn de_espacio(&self) -> bool {
        self.project_id == 0 && !self.space.is_empty()
    }

    /// Global: ni de un proyecto ni de un espacio.
    pub fn global(&self) -> bool {
        self.project_id == 0 && self.space.is_empty()
    }

    /// El tipo tal como se guarda y viaja (el desconocido, tal cual).
    pub fn tipo_texto(&self) -> &str {
        self.kind_text.as_deref().unwrap_or(self.kind.as_str())
    }
}

/// Lo que decide quien guarda. Lo demás (id, fechas, estadísticas) lo pone Omoikane.
#[derive(Clone, Debug, Default)]
pub struct Nueva {
    /// La carpeta del proyecto (vacía = global). Se ignora si viene `project_id` o `space_id`.
    pub project: String,
    /// El proyecto ya resuelto (lo que llega por la sincronización).
    pub project_id: Option<i64>,
    /// Memoria de este espacio, sin proyecto.
    pub space_id: Option<i64>,
    pub kind: Option<Tipo>,
    /// El tipo tal cual, si esta versión no lo conoce (lo que llega por la sincronización).
    pub kind_text: Option<String>,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
}

/// Solo cambia lo que viene en `Some`.
#[derive(Clone, Debug, Default)]
pub struct Cambios {
    pub title: Option<String>,
    pub content: Option<String>,
    pub kind: Option<Tipo>,
    pub paths: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    /// Llevarla a otro proyecto (`""` = global). Como fijar, no crea versión.
    pub project: Option<String>,
    /// Llevarla a un proyecto ya resuelto (la sincronización). Manda sobre `project`.
    pub project_id: Option<i64>,
    /// Llevarla a un espacio, sin proyecto. Manda sobre los dos anteriores.
    pub space_id: Option<i64>,
    /// El tipo tal cual, si esta versión no lo conoce (la sincronización).
    pub kind_text: Option<String>,
    /// Por qué cambia: se guarda con la versión anterior.
    pub reason: Option<String>,
    /// Edición parcial (0.19.0): `(buscar, poner)` literales, en orden. Cada trozo buscado tiene que
    /// aparecer exactamente una vez en el contenido tal como está al aplicarlo; si no, nada cambia.
    pub replace: Vec<(String, String)>,
    /// Texto que se añade al final del contenido (un paso más en una receta), con una línea en
    /// blanco entre medias.
    pub append: Option<String>,
}

/// Un trozo que `replace` o `append` cambió, con [`CONTEXTO`] caracteres a cada lado: lo que la
/// respuesta de `update` enseña para que el agente no tenga que pedir `get`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cambiado {
    /// El trozo que se buscó (`replace`); nada si fue `append`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub appended: bool,
    pub context: String,
}

/// Caracteres de contexto a cada lado de lo cambiado.
pub const CONTEXTO: usize = 80;

/// Una versión anterior de una memoria.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub version: i64,
    pub kind: String,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    /// Cuándo dejó de ser la versión viva.
    pub changed_at: String,
    pub reason: Option<String>,
}

/// Qué memorias se ven. Desde un proyecto, las suyas, las de su espacio y las globales; nunca las de
/// otro espacio. Desde una carpeta general (la del usuario, ver `proyecto::es_general`), todas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ambito {
    /// Un proyecto (`id`; -1 si aún no tiene nada) y su espacio.
    Proyecto { id: i64, space: Option<i64> },
    /// Todos los proyectos de un espacio, lo del espacio y lo global; con `None`, todos los proyectos
    /// sin espacio y lo global (lo que ve «todos los proyectos» desde un proyecto sin espacio).
    Espacio(Option<i64>),
    Todos,
}

impl Ambito {
    /// Lo que se ve desde la carpeta de un proyecto (sin escribir nada en la base).
    pub fn de(db: &Connection, project: &str) -> Result<Ambito> {
        if crate::proyecto::es_general(project) {
            return Ok(Ambito::Todos);
        }
        let (id, space) = crate::proyecto::resolver(db, project)?.unwrap_or((-1, None));
        Ok(Ambito::Proyecto { id, space })
    }

    /// «Todos los proyectos» vistos desde aquí: desde un proyecto, los de su espacio (o los que no
    /// tienen espacio); desde una carpeta general, todos.
    pub fn amplio(self) -> Ambito {
        match self {
            Ambito::Proyecto { space, .. } => Ambito::Espacio(space),
            otro => otro,
        }
    }

    /// El espacio desde el que se mira (`None` si no hay, o desde una carpeta general).
    pub fn espacio(&self) -> Option<i64> {
        match self {
            Ambito::Proyecto { space, .. } => *space,
            Ambito::Espacio(s) => *s,
            Ambito::Todos => None,
        }
    }

    /// Si una memoria (por su id) se ve desde aquí: la misma condición que `sql`.
    pub fn ve(&self, db: &Connection, id: &str) -> Result<bool> {
        Ok(db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM memory m WHERE m.id = ?1 AND {})", self.sql()), [id], |f| f.get(0))?)
    }

    /// El proyecto desde el que se mira, si es uno.
    pub fn proyecto(&self) -> Option<i64> {
        match self {
            Ambito::Proyecto { id, .. } if *id > 0 => Some(*id),
            _ => None,
        }
    }

    /// Si desde aquí se puede mirar `otro`: desde una carpeta general, todo; desde un proyecto o un
    /// espacio, solo lo de su mismo espacio (o, sin espacio, lo que no tiene).
    pub fn permite(&self, otro: &Ambito) -> bool {
        matches!(self, Ambito::Todos) || self.espacio() == otro.espacio()
    }

    /// Si una memoria de este proyecto (`project_id`; 0 si no tiene) y, sin proyecto, de este espacio
    /// (`space_id`) se ve desde aquí: la misma condición que `sql`, para lo que no está en la tabla
    /// `memory` (lo de la papelera).
    pub fn incluye(&self, db: &Connection, project_id: i64, space_id: Option<i64>) -> Result<bool> {
        let su_espacio = |s: Option<i64>| project_id != 0 || space_id.is_none() || space_id == s;
        Ok(match self {
            Ambito::Todos => true,
            Ambito::Proyecto { id, space } => (project_id == *id || project_id == 0) && su_espacio(*space),
            Ambito::Espacio(s) => (project_id == 0 || crate::proyecto::espacio_de(db, project_id)? == *s) && su_espacio(*s),
        })
    }

    /// La condición SQL sobre la memoria `m` (con `m.project_id IN (…)`, que va por el índice
    /// `memory_scope`). Los ids son enteros de la base: van en el texto, no hace falta parámetro.
    pub(crate) fn sql(&self) -> String {
        let espacio = |s: Option<i64>| match s {
            Some(s) => format!("(m.project_id <> 0 OR m.space_id IS NULL OR m.space_id = {s})"),
            None => "(m.project_id <> 0 OR m.space_id IS NULL)".to_string(),
        };
        match self {
            Ambito::Proyecto { id, space } => format!("(m.project_id IN ({id}, 0) AND {})", espacio(*space)),
            Ambito::Espacio(Some(s)) => format!("(m.project_id IN (SELECT 0 UNION ALL SELECT id FROM project WHERE space_id = {s}) AND {})", espacio(Some(*s))),
            Ambito::Espacio(None) => format!("(m.project_id IN (SELECT id FROM project WHERE space_id IS NULL) AND {})", espacio(None)),
            Ambito::Todos => "1".to_string(),
        }
    }
}

pub(crate) const AHORA: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
/// Las columnas de una memoria, sobre `memory m`. Lo del proyecto y el espacio, con subconsultas por
/// clave primaria: solo se evalúan para las filas que se devuelven (tras el `LIMIT`).
pub(crate) const COLUMNAS: &str = "m.id, COALESCE((SELECT path FROM project WHERE id = m.project_id), ''), m.kind, m.title, m.content, m.paths, m.tags, m.pinned, m.source, m.created_at, m.updated_at, m.recalled_at, m.recall_count, m.author, m.project_id, COALESCE((SELECT key FROM project WHERE id = m.project_id), ''), COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '')";
/// Cuántas columnas son: lo que se pida detrás de `COLUMNAS` empieza en este índice.
pub(crate) const NCOL: usize = 17;

pub(crate) fn de_fila(f: &Row) -> rusqlite::Result<Memoria> {
    let lista = |i: usize| -> rusqlite::Result<Vec<String>> {
        let s: String = f.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let kind: String = f.get(2)?;
    let tipo = kind.parse::<Tipo>().ok();
    Ok(Memoria {
        id: f.get(0)?,
        project: f.get(1)?,
        project_id: f.get(14)?,
        project_key: f.get(15)?,
        space: f.get(16)?,
        kind: tipo.unwrap_or(Tipo::Fact),
        kind_text: if tipo.is_none() { Some(kind) } else { None },
        title: f.get(3)?,
        content: f.get(4)?,
        paths: lista(5)?,
        tags: lista(6)?,
        pinned: f.get(7)?,
        source: f.get(8)?,
        created_at: f.get(9)?,
        updated_at: f.get(10)?,
        recalled_at: f.get(11)?,
        recall_count: f.get(12)?,
        author: f.get(13)?,
        content_hlc: String::new(),
        redacted: Vec::new(),
    })
}

fn escribir_tx(db: &mut Connection) -> Result<Transaction<'_>> {
    Ok(db.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

// --- Normalizar lo que llega -----------------------------------------------------------------------

fn titulo(t: &str) -> Result<String> {
    // Una línea: los saltos se vuelven espacios.
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!t.is_empty(), tr!("the title can't be empty", "el título no puede estar vacío"));
    ensure!(t.chars().count() <= 200, tr!("the title is over 200 characters: put the details in the content", "el título pasa de 200 caracteres: lo largo va en el contenido"));
    Ok(t)
}

/// Lo que se escribe aquí (no lo que llega de la sincronización ni lo que vuelve de la papelera) pasa
/// por `secretos::redactar`, salvo con `OMOIKANE_REDACT=0`. Apunta en `hallados` el tipo de cada uno.
fn sin_secretos(texto: &str, hallados: &mut Vec<&'static str>) -> String {
    if !crate::secretos::activo() {
        return texto.to_string();
    }
    let (s, hs) = crate::secretos::redactar(texto);
    hallados.extend(hs.iter().map(|h| h.tipo));
    s
}

fn contenido(c: &str) -> Result<String> {
    let c = c.trim().to_string();
    ensure!(!c.is_empty(), tr!("the content can't be empty", "el contenido no puede estar vacío"));
    Ok(c)
}

/// Rutas relativas a la raíz del proyecto, con `/` y sin `./` delante.
fn rutas(ps: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ps
        .iter()
        .map(|p| p.trim().replace('\\', "/"))
        .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn etiquetas(ts: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ts
        .iter()
        .map(|t| t.trim().trim_start_matches('#').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Lo que va de `inicio` a `fin` del texto con [`CONTEXTO`] caracteres a cada lado, y «…» donde
/// se corta.
fn alrededor(texto: &str, inicio: usize, fin: usize) -> String {
    let desde = texto[..inicio].char_indices().rev().nth(CONTEXTO - 1).map(|(i, _)| i).unwrap_or(0);
    let hasta = texto[fin..].char_indices().nth(CONTEXTO).map(|(i, _)| fin + i).unwrap_or(texto.len());
    let (antes, despues) = (if desde > 0 { "…" } else { "" }, if hasta < texto.len() { "…" } else { "" });
    format!("{antes}{}{despues}", &texto[desde..hasta])
}

/// Aplica a `contenido` los `replace` (cada uno exactamente una vez, en orden) y el `append` de
/// unos `Cambios`. Devuelve los trozos cambiados con su contexto; con un `find` que aparece 0 o
/// varias veces, error con el recuento y el contenido no se toca.
fn aplicar_edicion(contenido: &mut String, c: &Cambios) -> Result<Vec<Cambiado>> {
    let mut nuevo = contenido.clone();
    let mut cambiados = Vec::new();
    for (find, with) in &c.replace {
        ensure!(!find.is_empty(), tr!("replace: `find` can't be empty", "replace: `find` no puede estar vacío"));
        let veces = nuevo.matches(find.as_str()).count();
        ensure!(
            veces == 1,
            tr!(
                "replace: «{find}» appears {veces} times in the content (it has to appear exactly once); nothing changed",
                "replace: «{find}» aparece {veces} veces en el contenido (tiene que aparecer exactamente una); no se ha cambiado nada"
            )
        );
        let inicio = nuevo.find(find.as_str()).unwrap_or(0);
        nuevo.replace_range(inicio..inicio + find.len(), with);
        cambiados.push(Cambiado { find: Some(find.clone()), appended: false, context: alrededor(&nuevo, inicio, inicio + with.len()) });
    }
    if let Some(a) = c.append.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        let base = nuevo.trim_end().to_string();
        let inicio = base.len() + 2;
        nuevo = format!("{base}\n\n{a}");
        cambiados.push(Cambiado { find: None, appended: true, context: alrededor(&nuevo, inicio, nuevo.len()) });
    }
    *contenido = nuevo;
    Ok(cambiados)
}

// --- Índices ---------------------------------------------------------------------------------------

fn indexar(tx: &Transaction, n: i64, m: &Memoria) -> Result<()> {
    tx.execute(
        "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
    )?;
    tx.execute("DELETE FROM memory_path WHERE memory_id = ?1", [&m.id])?;
    for p in &m.paths {
        tx.execute("INSERT INTO memory_path(memory_id, path) VALUES (?1, ?2)", params![m.id, p])?;
    }
    Ok(())
}

fn desindexar(tx: &Transaction, n: i64) -> Result<()> {
    tx.execute("DELETE FROM memory_fts WHERE rowid = ?1", [n])?;
    Ok(())
}

// --- Operaciones -----------------------------------------------------------------------------------

/// Guarda sin mirar si ya existe una parecida: eso es `vectores::duplicada`, antes, para que quien
/// llama decida. Con `modelo`, el vector va en la misma transacción.
pub fn guardar(db: &mut Connection, nueva: Nueva, modelo: Option<&Modelo>) -> Result<Memoria> {
    insertar(db, &ulid::Ulid::generate().to_string(), nueva, None, modelo)
}

/// De dónde viene una memoria que llega por la sincronización: su id, fechas, sellos y autor de origen.
pub(crate) struct Origen<'a> {
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub hlc: &'a str,
    /// El sello de su último cambio de texto en el equipo de origen (vacío: el de `hlc`, si el texto
    /// cambia; lo mandan las versiones de antes del esquema 10 y las líneas en que coinciden).
    pub content_hlc: &'a str,
    pub author: &'a str,
    /// Los campos de la línea que esta versión no conoce, en JSON (`memory.extra`): la memoria los
    /// guarda y la sincronización y `omoi export` los vuelven a escribir.
    pub extra: &'a str,
}

/// Dónde va una memoria: (proyecto, espacio). Un espacio va sin proyecto; una carpeta se apunta como
/// proyecto si aún no lo es.
fn alcance(tx: &Connection, project: &str, project_id: Option<i64>, space_id: Option<i64>) -> Result<(i64, Option<i64>)> {
    Ok(match (space_id, project_id) {
        (Some(s), _) => (0, Some(s)),
        (None, Some(p)) => (p, None),
        (None, None) => (crate::proyecto::registrar(tx, project)?, None),
    })
}

/// Para la sincronización: la misma memoria, con el mismo id, fechas, sello y autor que en su equipo.
pub(crate) fn guardar_con_id(db: &mut Connection, id: &str, nueva: Nueva, o: &Origen, modelo: Option<&Modelo>) -> Result<Memoria> {
    insertar(db, id, nueva, Some(o), modelo)
}

/// El sello de una memoria (vacío si no existe).
fn hlc_de(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Apunta una marca de olvido, salvo que ya haya una posterior, con el espacio (su clave; vacía: de
/// ninguno) de lo olvidado: el olvido va solo al repositorio de ese espacio.
pub(crate) fn marcar_olvido(db: &Connection, id: &str, deleted_at: Option<&str>, hlc: &str, espacio: &str) -> Result<()> {
    let actual: Option<String> = db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    if actual.is_none_or(|a| crate::reloj::mayor(hlc, &a)) {
        db.execute(
            &format!("INSERT OR REPLACE INTO tombstone(id, deleted_at, hlc, space_key) VALUES (?1, COALESCE(?2, {AHORA}), ?3, ?4)"),
            params![id, deleted_at, hlc, espacio],
        )?;
    }
    Ok(())
}

/// El autor que se apunta en lo que se guarda aquí (lo configura la sincronización; vacío = ninguno).
pub(crate) fn autor(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'autor'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

fn insertar(db: &mut Connection, id: &str, nueva: Nueva, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<Memoria> {
    let tx = escribir_tx(db)?;
    let m = insertar_en(&tx, id, nueva, origen, modelo)?;
    tx.commit()?;
    Ok(m)
}

/// `insertar` dentro de una transacción que ya está abierta (quien llama hace el commit).
fn insertar_en(tx: &Transaction, id: &str, nueva: Nueva, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<Memoria> {
    let kind = nueva.kind.with_context(|| tr!("missing kind: fact, decision, gotcha, preference or procedure", "falta el tipo: fact, decision, gotcha, preference o procedure"))?;
    // Se redacta antes de validar: lo que cuenta para los 200 caracteres del título es lo que se guarda.
    let mut redacted = Vec::new();
    let (title, content) = match origen {
        None => (titulo(&sin_secretos(&nueva.title, &mut redacted))?, contenido(&sin_secretos(&nueva.content, &mut redacted))?),
        Some(_) => (titulo(&nueva.title)?, contenido(&nueva.content)?),
    };
    let (paths, tags) = (rutas(&nueva.paths), etiquetas(&nueva.tags));
    let (hlc, author) = match origen {
        Some(o) => {
            crate::reloj::recibir(tx, o.hlc)?;
            (o.hlc.to_string(), o.author.to_string())
        }
        None => (crate::reloj::sello(tx)?, autor(tx)?),
    };
    let content_hlc = match origen {
        Some(o) if !o.content_hlc.is_empty() => o.content_hlc.to_string(),
        _ => hlc.clone(),
    };
    let (project_id, space_id) = alcance(tx, &nueva.project, nueva.project_id, nueva.space_id)?;
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project_id, space_id, kind, title, content, paths, tags, pinned, source, created_at, updated_at, hlc, author, content_hlc, extra)
             VALUES (?1, ?2, ?14, ?3, ?4, ?5, ?6, ?7, ?8, ?9, COALESCE(?10, {AHORA}), COALESCE(?11, {AHORA}), ?12, ?13, ?15, ?16)"
        ),
        params![
            id,
            project_id,
            nueva.kind_text.as_deref().unwrap_or(kind.as_str()),
            title,
            content,
            serde_json::to_string(&paths)?,
            serde_json::to_string(&tags)?,
            nueva.pinned,
            nueva.source,
            origen.map(|o| o.created_at),
            origen.map(|o| o.updated_at),
            hlc,
            author,
            space_id,
            content_hlc,
            origen.map_or("{}", |o| o.extra),
        ],
    )?;
    let n = tx.last_insert_rowid();
    let m = leer(tx, id)?;
    indexar(tx, n, &m)?;
    if let Some(modelo) = modelo {
        vectores::guardar(tx, n, modelo, &m.title, &m.content)?;
    }
    Ok(Memoria { redacted, ..m })
}

fn leer(db: &Connection, id: &str) -> Result<Memoria> {
    db.query_row(&format!("SELECT {COLUMNAS} FROM memory m WHERE m.id = ?1"), [id], de_fila)
        .optional()?
        .ok_or_else(|| anyhow!(tr!("no memory with id {id}", "no existe la memoria {id}")))
}

/// Dónde está una memoria de aquí, viva o en la papelera: si es global (ni proyecto ni espacio) y la
/// clave de su espacio (el suyo o el de su proyecto; vacía si no tiene). `None` si aquí no está.
pub(crate) fn alcance_de_memoria(db: &Connection, id: &str) -> Result<Option<(bool, String)>> {
    let viva: Option<(bool, String)> = db
        .query_row(
            "SELECT m.project_id = 0 AND m.space_id IS NULL,
                    COALESCE((SELECT key FROM space WHERE id = COALESCE(m.space_id, (SELECT space_id FROM project WHERE id = m.project_id))), '')
             FROM memory m WHERE m.id = ?1",
            [id],
            |f| Ok((f.get(0)?, f.get(1)?)),
        )
        .optional()?;
    if viva.is_some() {
        return Ok(viva);
    }
    let Some(texto) = db.query_row("SELECT memory FROM forgotten WHERE id = ?1", [id], |f| f.get::<_, String>(0)).optional()? else { return Ok(None) };
    let global = crate::sync::de_papelera(&texto).is_some_and(|(m, _, _)| m.project_key.is_empty() && m.project.is_empty() && m.space.is_empty());
    Ok(Some((global, crate::sync::espacio_de_papelera(db, &texto)?)))
}

/// Id completo a partir del id o de su principio (lo que enseña la CLI basta).
pub fn resolver_id(db: &Connection, id: &str) -> Result<String> {
    let id = id.trim().to_uppercase();
    ensure!(id.len() >= 4, tr!("id too short: «{id}»", "id demasiado corto: «{id}»"));
    ensure!(id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let mut q = db.prepare("SELECT id FROM memory WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
    let ids: Vec<String> = q.query_map([&id], |f| f.get(0))?.collect::<rusqlite::Result<_>>()?;
    match ids.as_slice() {
        [uno] => Ok(uno.clone()),
        [] => bail!(tr!("no memory with id {id}", "no existe ninguna memoria con id {id}")),
        _ => bail!(tr!("several memories start with {id}: type more characters", "hay varias memorias que empiezan por {id}: escribe más caracteres")),
    }
}

pub fn obtener(db: &Connection, id: &str) -> Result<Memoria> {
    leer(db, &resolver_id(db, id)?)
}

pub fn actualizar(db: &mut Connection, id: &str, c: Cambios, modelo: Option<&Modelo>) -> Result<Memoria> {
    Ok(actualizar_con_cambios(db, id, c, modelo)?.0)
}

/// Como `actualizar`, y además devuelve lo que `replace` y `append` cambiaron, con su contexto.
pub fn actualizar_con_cambios(db: &mut Connection, id: &str, c: Cambios, modelo: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    actualizar_desde(db, id, c, None, modelo)
}

/// Para la sincronización: el cambio que llega de otro equipo, con sus fechas, sello y autor, todo en
/// la misma transacción (la versión de aquí queda en el historial).
pub(crate) fn actualizar_con_origen(db: &mut Connection, id: &str, c: Cambios, o: &Origen, modelo: Option<&Modelo>) -> Result<Memoria> {
    Ok(actualizar_desde(db, id, c, Some(o), modelo)?.0)
}

fn actualizar_desde(db: &mut Connection, id: &str, c: Cambios, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let r = actualizar_en(&tx, &id, c, origen, modelo)?;
    tx.commit()?;
    Ok(r)
}

/// `actualizar` dentro de una transacción que ya está abierta (quien llama hace el commit), con el
/// id ya resuelto.
fn actualizar_en(tx: &Transaction, id: &str, c: Cambios, origen: Option<&Origen>, modelo: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>)> {
    let antes = leer(tx, id)?;
    let hlc_antes = hlc_de(tx, id)?;
    // El sello del cambio: el de origen, o uno de aquí posterior al de la versión que se cambia.
    let hlc = match origen {
        Some(o) => {
            crate::reloj::recibir(tx, o.hlc)?;
            o.hlc.to_string()
        }
        None => crate::reloj::sello_tras(tx, &hlc_antes)?,
    };
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    // Lo que llega de otro equipo trae la memoria entera, también los campos que esta versión no
    // conoce; un cambio de aquí los deja como estaban.
    if let Some(o) = origen {
        tx.execute("UPDATE memory SET extra = ?2 WHERE id = ?1", params![id, o.extra])?;
    }
    // Solo lo que trae el cambio, y solo si se escribe aquí: lo que ya había lo avisa Revisar.
    let mut redacted = Vec::new();
    let mut limpio = |t: &str| if origen.is_none() { sin_secretos(t, &mut redacted) } else { t.to_string() };
    // El contenido nuevo, y encima la edición parcial (`replace`, `append`).
    let mut content = match &c.content {
        Some(t) => contenido(t)?,
        None => antes.content.clone(),
    };
    let cambiados = aplicar_edicion(&mut content, &c)?;
    // Lo nuevo (el contenido entero, o lo que trajo la edición) pasa por la redacción.
    if c.content.is_some() || !cambiados.is_empty() {
        content = limpio(&content);
    }
    let despues = Memoria {
        title: match &c.title {
            Some(t) => titulo(&limpio(t))?,
            None => antes.title.clone(),
        },
        content: contenido(&content)?,
        kind: c.kind.unwrap_or(antes.kind),
        // Un tipo nuevo conocido borra el desconocido; uno desconocido que llega lo trae.
        kind_text: match (&c.kind_text, c.kind) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(_)) => None,
            (None, None) => antes.kind_text.clone(),
        },
        paths: c.paths.as_deref().map(rutas).unwrap_or_else(|| antes.paths.clone()),
        tags: c.tags.as_deref().map(etiquetas).unwrap_or_else(|| antes.tags.clone()),
        pinned: c.pinned.unwrap_or(antes.pinned),
        ..antes.clone()
    };
    // Dónde queda: lo que diga el cambio (un espacio, un proyecto ya resuelto, una carpeta o global) o
    // donde estaba.
    let antes_espacio: Option<i64> = tx.query_row("SELECT space_id FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    let (project_id, space_id): (i64, Option<i64>) = if c.space_id.is_some() || c.project_id.is_some() || c.project.is_some() {
        alcance(tx, c.project.as_deref().unwrap_or(""), c.project_id, c.space_id)?
    } else {
        (antes.project_id, antes_espacio)
    };
    let se_mueve = project_id != antes.project_id || space_id != antes_espacio;
    // Lo que caduca los veredictos: título, contenido o tipo (fijar, etiquetar o mover, no).
    let cambia_juzgado = despues.title != antes.title || despues.content != antes.content || despues.tipo_texto() != antes.tipo_texto();
    let cambia_texto = cambia_juzgado || despues.paths != antes.paths || despues.tags != antes.tags;
    let content_hlc: Option<String> = match origen {
        Some(o) if cambia_juzgado => Some(if o.content_hlc.is_empty() { o.hlc.to_string() } else { o.content_hlc.to_string() }),
        None if cambia_juzgado => Some(hlc.clone()),
        _ => None,
    };
    if !cambia_texto {
        // Fijar o desfijar no es un cambio de contenido: sin versión nueva y sin tocar updated_at
        // (que da la recencia en la búsqueda).
        if despues.pinned != antes.pinned || se_mueve || origen.is_some() {
            tx.execute(
                "UPDATE memory SET pinned = ?2, project_id = ?3, space_id = ?4, hlc = ?5 WHERE id = ?1",
                params![id, despues.pinned, project_id, space_id, hlc],
            )?;
            if let Some(o) = origen {
                tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
            }
        }
        return Ok((Memoria { redacted, ..leer(tx, id)? }, cambiados));
    }
    // La foto anterior va al historial.
    guardar_version(tx, id, &antes, None, c.reason.as_deref())?;
    tx.execute(
        &format!(
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, project_id = ?8, space_id = ?11,
             updated_at = {AHORA}, hlc = ?9, author = CASE WHEN author = '' THEN ?10 ELSE author END,
             content_hlc = COALESCE(?12, content_hlc) WHERE id = ?1"
        ),
        params![
            id,
            despues.title,
            despues.content,
            despues.tipo_texto(),
            serde_json::to_string(&despues.paths)?,
            serde_json::to_string(&despues.tags)?,
            despues.pinned,
            project_id,
            hlc,
            autor(tx)?,
            space_id,
            content_hlc,
        ],
    )?;
    if let Some(o) = origen {
        tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
    }
    desindexar(tx, n)?;
    let m = leer(tx, id)?;
    indexar(tx, n, &m)?;
    if despues.title != antes.title || despues.content != antes.content {
        match modelo {
            Some(modelo) => vectores::guardar(tx, n, modelo, &m.title, &m.content)?,
            // Sin modelo, el vector viejo ya no vale: se borra y `sincronizar` lo rehará.
            None => {
                tx.execute("DELETE FROM memory_vec WHERE n = ?1", [n])?;
            }
        }
    }
    Ok((Memoria { redacted, ..m }, cambiados))
}

/// Cuelga una foto de `m` del historial de `id`, con el número siguiente al último que tiene.
/// `changed_at` es cuándo dejó de ser la versión viva (ahora, si no se dice).
fn guardar_version(tx: &Transaction, id: &str, m: &Memoria, changed_at: Option<&str>, reason: Option<&str>) -> Result<()> {
    let version: i64 =
        tx.query_row("SELECT COALESCE(MAX(version), 0) + 1 FROM memory_version WHERE memory_id = ?1", [id], |f| f.get(0))?;
    tx.execute(
        &format!(
            "INSERT INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, COALESCE(?8, {AHORA}), ?9)"
        ),
        params![
            id,
            version,
            m.title,
            m.content,
            m.tipo_texto(),
            serde_json::to_string(&m.paths)?,
            serde_json::to_string(&m.tags)?,
            changed_at,
            reason.map(str::trim).filter(|r| !r.is_empty()),
        ],
    )?;
    Ok(())
}

/// Junta N memorias en una (0.19.0): `id` se queda con el contenido fundido que trae `c` (lo escribe
/// el agente o la persona; Omoikane no redacta) y las `ids` van a la papelera con un veredicto
/// `superseded` hacia ella. Sus etiquetas y rutas se suman a la que queda (así los `[[enlaces]]`
/// que llegaban a ellas por etiqueta pasan a resolver a esta) y su historial se cuelga del de la
/// que queda, con el motivo «absorbed from <id>». Todo en una transacción. Devuelve la que queda,
/// lo que `replace`/`append` cambiaron y los ids absorbidos.
pub fn absorber(db: &mut Connection, id: &str, ids: &[String], mut c: Cambios, modelo: Option<&Modelo>) -> Result<(Memoria, Vec<Cambiado>, Vec<String>)> {
    ensure!(
        c.content.is_some(),
        tr!(
            "absorb needs the merged text: write the content that joins the memories and pass it as `content` (Omoikane doesn't write it)",
            "absorb necesita el texto fundido: escribe el contenido que junta las memorias y pásalo en `content` (Omoikane no lo redacta)"
        )
    );
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let viva = leer(&tx, &id)?;
    let mut absorbidas: Vec<Memoria> = Vec::new();
    for otra in ids {
        let otra = resolver_id(&tx, otra)?;
        ensure!(otra != id, tr!("a memory can't absorb itself", "una memoria no puede absorberse a sí misma"));
        if !absorbidas.iter().any(|a| a.id == otra) {
            absorbidas.push(leer(&tx, &otra)?);
        }
    }
    ensure!(!absorbidas.is_empty(), tr!("absorb: no memory to absorb", "absorb: ninguna memoria que absorber"));
    // Etiquetas y rutas: las que vengan en los cambios (o las de la viva) más las de las absorbidas.
    let mut tags = c.tags.take().unwrap_or_else(|| viva.tags.clone());
    let mut paths = c.paths.take().unwrap_or_else(|| viva.paths.clone());
    for a in &absorbidas {
        tags.extend(a.tags.iter().cloned());
        paths.extend(a.paths.iter().cloned());
    }
    c.tags = Some(tags);
    c.paths = Some(paths);
    let lista = absorbidas.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ");
    if c.reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
        c.reason = Some(format!("absorbed {lista}"));
    }
    let (m, cambiados) = actualizar_en(&tx, &id, c, None, modelo)?;
    for a in &absorbidas {
        // El veredicto antes de borrarla: se resuelve contra las memorias vivas, y vuelve a valer si
        // alguien la restaura de la papelera.
        crate::veredictos::poner(&tx, &a.id, &id, crate::veredictos::SUSTITUIDA, &format!("absorbed into {id}"), "omoikane")?;
        // Su historial, de la más vieja a la última viva, a continuación del de la que queda.
        let motivo = format!("absorbed from {}", a.id);
        for v in historial(&tx, &a.id)?.into_iter().rev() {
            let foto = Memoria { title: v.title, content: v.content, kind: v.kind.parse().unwrap_or(a.kind), paths: v.paths, tags: v.tags, ..a.clone() };
            guardar_version(&tx, &id, &foto, Some(&v.changed_at), Some(&motivo))?;
        }
        guardar_version(&tx, &id, a, None, Some(&motivo))?;
        borrar_en(&tx, &a.id, None)?;
    }
    tx.commit()?;
    Ok((m, cambiados, absorbidas.into_iter().map(|a| a.id).collect()))
}

/// Redacta una memoria que ya está guardada (el botón «Redactar» de Revisar): título y contenido sin
/// sus secretos, con el motivo «redacted» en la versión anterior, y su historial también (si no, el
/// secreto seguiría en la versión de antes). Lo pide la persona, así que se hace aunque
/// `OMOIKANE_REDACT=0`. Sin nada que quitar, la deja como está (`redacted` vacío).
pub fn redactar(db: &mut Connection, id: &str, modelo: Option<&Modelo>) -> Result<Memoria> {
    let m = obtener(db, id)?;
    let (title, ht) = crate::secretos::redactar(&m.title);
    let (content, hc) = crate::secretos::redactar(&m.content);
    if ht.is_empty() && hc.is_empty() {
        return Ok(m);
    }
    let c = Cambios {
        title: (!ht.is_empty()).then_some(title),
        content: (!hc.is_empty()).then_some(content),
        reason: Some("redacted".into()),
        ..Cambios::default()
    };
    let (r, _) = actualizar_desde(db, &m.id, c, None, modelo)?;
    let tx = escribir_tx(db)?;
    let versiones: Vec<(i64, String, String)> = {
        let mut q = tx.prepare("SELECT version, title, content FROM memory_version WHERE memory_id = ?1")?;
        q.query_map([&m.id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (version, titulo, contenido) in versiones {
        let (t, c) = (crate::secretos::redactar(&titulo).0, crate::secretos::redactar(&contenido).0);
        if t != titulo || c != contenido {
            tx.execute("UPDATE memory_version SET title = ?3, content = ?4 WHERE memory_id = ?1 AND version = ?2", params![m.id, version, t, c])?;
        }
    }
    tx.commit()?;
    Ok(Memoria { redacted: ht.iter().chain(&hc).map(|h| h.tipo).collect(), ..r })
}

/// Rehace el índice de palabras desde la tabla. Devuelve cuántas memorias indexó.
pub fn reindexar_palabras(db: &mut Connection) -> Result<usize> {
    let tx = escribir_tx(db)?;
    tx.execute("DELETE FROM memory_fts", [])?;
    let ids: Vec<(i64, String)> = {
        let mut q = tx.prepare("SELECT n, id FROM memory")?;
        q.query_map([], |f| Ok((f.get(0)?, f.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let todas: Vec<(i64, Memoria)> = ids.into_iter().map(|(n, id)| Ok((n, leer(&tx, &id)?))).collect::<Result<_>>()?;
    for (n, m) in &todas {
        tx.execute(
            "INSERT INTO memory_fts(rowid, title, content, tags, paths) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![n, m.title, m.content, m.tags.join(" "), m.paths.join(" ")],
        )?;
    }
    tx.commit()?;
    Ok(todas.len())
}

/// Olvida la memoria: sale de la base y de los índices, deja una marca para que la sincronización no
/// la devuelva desde otro equipo, y queda en la papelera (`forgotten`) con su historial, de donde
/// `restaurar` la devuelve. Devuelve lo que se olvidó.
pub fn olvidar(db: &mut Connection, id: &str) -> Result<Memoria> {
    borrar(db, id, None)
}

/// Olvida y, además, la saca de la papelera: no hay vuelta atrás.
pub fn olvidar_del_todo(db: &mut Connection, id: &str) -> Result<Memoria> {
    let m = borrar(db, id, None)?;
    db.execute("DELETE FROM forgotten WHERE id = ?1", [&m.id])?;
    Ok(m)
}

/// Días que una memoria olvidada espera en la papelera antes de borrarse del todo
/// (`OMOIKANE_TRASH_DAYS`; 0 = nunca).
pub fn dias_papelera() -> f64 {
    std::env::var("OMOIKANE_TRASH_DAYS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(30.0)
}

/// Borra de la papelera lo olvidado hace más de `dias` (0 = nada). Devuelve cuántas.
pub fn purgar(db: &Connection, dias: f64) -> Result<usize> {
    if dias <= 0.0 {
        return Ok(0);
    }
    let n = db.execute(
        "DELETE FROM forgotten WHERE deleted_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
        [format!("-{} seconds", (dias * 86400.0) as i64)],
    )?;
    Ok(n)
}

/// Vacía la papelera entera. Devuelve cuántas.
pub fn vaciar_papelera(db: &Connection) -> Result<usize> {
    Ok(db.execute("DELETE FROM forgotten", [])?)
}

/// Para la sincronización: olvidar con la marca (sello y fecha) que llega de otro equipo. La memoria se
/// guarda aparte por si hay que devolverla (`restaurar`).
pub(crate) fn olvidar_con(db: &mut Connection, id: &str, hlc: &str, deleted_at: &str, equipo: &str) -> Result<Memoria> {
    borrar(db, id, Some((hlc, deleted_at, equipo)))
}

/// Una memoria en la papelera.
#[derive(Clone, Debug, Serialize)]
pub struct Olvidada {
    pub id: String,
    pub title: String,
    pub kind: Tipo,
    pub project: String,
    /// Su espacio (la clave): el suyo si era de espacio, o el de su proyecto.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub space: String,
    /// Quién la olvidó: el nombre del equipo (y `[by …]` ya dice la persona, si la sincronización lo apunta).
    pub by: String,
    pub deleted_at: String,
}

/// La papelera: lo olvidado aquí o en otro equipo, de lo más reciente a lo más antiguo.
pub fn papelera(db: &Connection) -> Result<Vec<Olvidada>> {
    let mut q = db.prepare("SELECT id, memory, by, deleted_at FROM forgotten ORDER BY deleted_at DESC")?;
    let v = q
        .query_map([], |f| {
            let m: String = f.get(1)?;
            let m: Memoria = serde_json::from_str(&m).unwrap_or_default();
            Ok(Olvidada { id: f.get(0)?, title: m.title, kind: m.kind, project: m.project, space: m.space, by: f.get(2)?, deleted_at: f.get(3)? })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Devuelve una memoria de la papelera (olvidada aquí o en otro equipo), con un sello posterior al
/// olvido: así vuelve también a los demás equipos en la próxima sincronización. Su historial anterior
/// se conserva.
pub fn restaurar(db: &mut Connection, id: &str, modelo: Option<&Modelo>) -> Result<Memoria> {
    // El id como en las demás órdenes: en mayúsculas, y un principio que valga para varias es un error.
    let id = id.trim().to_uppercase();
    ensure!(!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()), tr!("invalid id: «{id}»", "id no válido: «{id}»"));
    let tx = escribir_tx(db)?;
    let filas: Vec<(String, String, String, String)> = {
        let mut q = tx.prepare("SELECT id, memory, versions, hlc FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
        q.query_map([&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?, f.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let (id, m, versiones, hlc_olvidada) = match <[_; 1]>::try_from(filas) {
        Ok([una]) => una,
        Err(v) if v.is_empty() => bail!(tr!("nothing in the trash with id {id}", "no hay nada en la papelera con id {id}")),
        Err(_) => bail!(tr!("several memories in the trash start with {id}: type more characters", "hay varias memorias en la papelera que empiezan por {id}: escribe más caracteres")),
    };
    // Ya está viva (otro equipo la restauró y llegó por la sincronización, de una base de antes de la
    // 0.19.1): solo sobraba en la papelera.
    if tx.query_row("SELECT count(*) FROM memory WHERE id = ?1", [&id], |f| f.get::<_, i64>(0))? > 0 {
        tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
        tx.commit()?;
        bail!(tr!("{id} is not forgotten here (it came back from another computer); removed it from the trash", "{id} no está olvidada aquí (volvió desde otro equipo); la quito de la papelera"));
    }
    let (m, extra, _) = crate::sync::de_papelera(&m).ok_or_else(|| anyhow!(tr!("{id} can't be read from the trash", "{id} no se entiende en la papelera")))?;
    let extra = crate::sync::extra_a_texto(&extra);
    let hlc_olvidada = if m.content_hlc.is_empty() { hlc_olvidada } else { m.content_hlc.clone() };
    let versiones: Vec<Version> = serde_json::from_str(&versiones).unwrap_or_default();
    let marca: String = tx.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?.unwrap_or_default();
    let hlc = crate::reloj::sello_tras(&tx, &marca)?;
    let (project_id, space_id) = alcance_guardado(&tx, &m)?;
    let nueva = Nueva {
        project: m.project.clone(),
        project_id: Some(project_id),
        space_id,
        kind: Some(m.kind),
        kind_text: m.kind_text.clone(),
        title: m.title.clone(),
        content: m.content.clone(),
        paths: m.paths.clone(),
        tags: m.tags.clone(),
        pinned: m.pinned,
        source: m.source.clone(),
    };
    // Vuelve con su texto de siempre: el sello de contenido, el de cuando se olvidó (los veredictos que
    // tenía siguen valiendo).
    let o = Origen { created_at: &m.created_at, updated_at: &m.updated_at, hlc: &hlc, content_hlc: &hlc_olvidada, author: &m.author, extra: &extra };
    tx.execute("DELETE FROM tombstone WHERE id = ?1", [&id])?;
    crate::sync::guardar_extra_olvido(&tx, &id, &Default::default())?;
    let r = insertar_en(&tx, &id, nueva, Some(&o), modelo)?;
    for v in versiones {
        tx.execute(
            "INSERT OR IGNORE INTO memory_version(memory_id, version, title, content, kind, paths, tags, changed_at, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, v.version, v.title, v.content, v.kind, serde_json::to_string(&v.paths)?, serde_json::to_string(&v.tags)?, v.changed_at, v.reason],
        )?;
    }
    tx.execute("DELETE FROM forgotten WHERE id = ?1", [&id])?;
    tx.commit()?;
    Ok(r)
}

/// Dónde vuelve una memoria guardada fuera de la base (papelera, copia): su proyecto por la clave (o,
/// en las copias de antes del esquema 10, por la ruta), su espacio, o global.
pub(crate) fn alcance_guardado(tx: &Connection, m: &Memoria) -> Result<(i64, Option<i64>)> {
    if !m.project.is_empty() || !m.project_key.is_empty() {
        let id = match crate::proyecto::resolver(tx, &m.project)? {
            Some((id, _)) if m.project_key.is_empty() || crate::proyecto::clave_de(tx, id)? == m.project_key => id,
            _ if !m.project_key.is_empty() => crate::proyecto::de_clave(tx, &m.project_key, &m.project, &crate::db::persona_de(tx), true)?,
            _ => crate::proyecto::registrar(tx, &m.project)?,
        };
        // Como al recibirla por la sincronización: un proyecto sin espacio aquí toma el que trae; uno
        // con espacio se queda el suyo.
        if !m.space.is_empty() && crate::proyecto::espacio_de(tx, id)?.is_none() {
            let s = crate::proyecto::espacio_de_clave(tx, &m.space)?;
            tx.execute("UPDATE project SET space_id = ?2 WHERE id = ?1", params![id, s])?;
        }
        return Ok((id, None));
    }
    Ok((0, crate::proyecto::espacio_de_clave(tx, &m.space)?))
}

fn borrar(db: &mut Connection, id: &str, marca: Option<(&str, &str, &str)>) -> Result<Memoria> {
    let id = resolver_id(db, id)?;
    let tx = escribir_tx(db)?;
    let m = borrar_en(&tx, &id, marca)?;
    tx.commit()?;
    Ok(m)
}

/// `borrar` dentro de una transacción que ya está abierta (quien llama hace el commit), con el id
/// ya resuelto.
fn borrar_en(tx: &Transaction, id: &str, marca: Option<(&str, &str, &str)>) -> Result<Memoria> {
    let (hlc, deleted_at): (String, Option<&str>) = match marca {
        Some((h, d, _)) => {
            crate::reloj::recibir(tx, h)?;
            (h.to_string(), Some(d))
        }
        None => (crate::reloj::sello_tras(tx, &hlc_de(tx, id)?)?, None),
    };
    let m = leer(tx, id)?;
    marcar_olvido(tx, id, deleted_at, &hlc, &m.space)?;
    // A la papelera, con su historial: `restaurar` la devuelve desde cualquier equipo. `by` es el
    // equipo que la olvidó: otro (quizá otra persona) o este.
    let quien = match marca {
        Some((_, _, equipo)) => equipo.to_string(),
        None => crate::reloj::equipo(tx)?,
    };
    let versiones = historial(tx, id)?;
    let hlc_m = hlc_de(tx, id)?;
    let (content_hlc, extra): (String, String) = tx.query_row("SELECT content_hlc, extra FROM memory WHERE id = ?1", [id], |f| Ok((f.get(0)?, f.get(1)?)))?;
    let m = Memoria { content_hlc, ..m };
    // En la papelera va con sus campos desconocidos: si vuelve (aquí o en otro equipo), vuelven con ella.
    let texto = crate::sync::a_papelera(&m, &crate::sync::extra_de(&extra), &Default::default())?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, COALESCE(?5, {AHORA}), ?6)"),
        params![id, texto, serde_json::to_string(&versiones)?, hlc_m, deleted_at, quien],
    )?;
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    desindexar(tx, n)?;
    // memory_version y memory_path caen con ON DELETE CASCADE.
    tx.execute("DELETE FROM memory WHERE id = ?1", [id])?;
    Ok(m)
}

/// Una versión de `memory_version` desde las columnas `version, kind, title, content, paths, tags,
/// changed_at, reason` (a partir de la columna `desde`).
fn version_de_fila(f: &Row, desde: usize) -> rusqlite::Result<Version> {
    let lista = |i: usize| -> rusqlite::Result<Vec<String>> { Ok(serde_json::from_str(&f.get::<_, String>(desde + i)?).unwrap_or_default()) };
    Ok(Version {
        version: f.get(desde)?,
        kind: f.get(desde + 1)?,
        title: f.get(desde + 2)?,
        content: f.get(desde + 3)?,
        paths: lista(4)?,
        tags: lista(5)?,
        changed_at: f.get(desde + 6)?,
        reason: f.get(desde + 7)?,
    })
}

/// Versiones anteriores, de la más reciente a la más antigua.
pub fn historial(db: &Connection, id: &str) -> Result<Vec<Version>> {
    let id = resolver_id(db, id)?;
    let mut q = db.prepare(
        "SELECT version, kind, title, content, paths, tags, changed_at, reason
         FROM memory_version WHERE memory_id = ?1 ORDER BY version DESC",
    )?;
    let v = q.query_map([&id], |f| version_de_fila(f, 0))?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// El historial de todas las memorias que lo tienen, en una sola pasada (para la sincronización: una
/// consulta por memoria era lo más lento de `omoi sync` con una base grande).
pub(crate) fn historiales(db: &Connection) -> Result<std::collections::HashMap<String, Vec<Version>>> {
    let mut q = db.prepare(
        "SELECT memory_id, version, kind, title, content, paths, tags, changed_at, reason
         FROM memory_version ORDER BY memory_id, version DESC",
    )?;
    let mut v: std::collections::HashMap<String, Vec<Version>> = std::collections::HashMap::new();
    let mut filas = q.query([])?;
    while let Some(f) = filas.next()? {
        v.entry(f.get(0)?).or_default().push(version_de_fila(f, 1)?);
    }
    Ok(v)
}

/// Las últimas guardadas o cambiadas.
pub fn recientes(db: &Connection, ambito: &Ambito, kind: Option<Tipo>, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory m
         WHERE {} AND (?1 IS NULL OR m.kind = ?1)
         ORDER BY m.updated_at DESC, m.id DESC LIMIT ?2",
        ambito.sql()
    ))?;
    let v = q.query_map(params![kind.map(Tipo::as_str), limite as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// La etiqueta de las memorias que dicen en qué punto se quedó el trabajo de un proyecto.
pub const ESTADO: &str = "status";

/// En qué punto se quedó cada proyecto: la memoria más reciente con la etiqueta [`ESTADO`] de cada
/// uno (las globales cuentan como un proyecto más), de la más nueva a la más vieja.
pub fn estados(db: &Connection, ambito: &Ambito, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory m
         WHERE {} AND m.tags LIKE ?2 AND EXISTS (SELECT 1 FROM json_each(m.tags) WHERE value = ?1)
         ORDER BY m.updated_at DESC, m.id DESC",
        ambito.sql()
    ))?;
    // El `LIKE` descarta sin leer el JSON lo que no lleva la etiqueta.
    let todas: Vec<Memoria> = q.query_map(params![ESTADO, format!("%\"{ESTADO}\"%")], de_fila)?.collect::<rusqlite::Result<_>>()?;
    let mut v: Vec<Memoria> = Vec::new();
    for m in todas {
        // Una por proyecto; las de espacio, una por espacio, y las globales, una.
        if v.len() < limite && !v.iter().any(|x| x.project_id == m.project_id && (m.project_id != 0 || x.space == m.space)) {
            v.push(m);
        }
    }
    Ok(v)
}

/// Lo que el agente debería saber siempre al empezar en un proyecto: las preferencias y las fijadas,
/// del proyecto, de su espacio y globales (desde una carpeta general, solo las globales). Primero las
/// fijadas, luego las más recientes.
pub fn contexto(db: &Connection, ambito: &Ambito, limite: usize) -> Result<Vec<Memoria>> {
    let donde = match ambito {
        Ambito::Todos => "(m.project_id = 0 AND m.space_id IS NULL)".to_string(),
        a => a.sql(),
    };
    // La condición, igual que la del índice parcial `memory_always`, para que lo use.
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory m
         WHERE {donde} AND (m.pinned = 1 OR m.kind = 'preference')
         ORDER BY m.pinned DESC, m.updated_at DESC, m.id DESC LIMIT ?1"
    ))?;
    let v = q.query_map(params![limite as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Cuántas memorias se ven desde un ámbito.
pub fn cuantas(db: &Connection, ambito: &Ambito) -> Result<i64> {
    Ok(db.query_row(&format!("SELECT count(*) FROM memory m WHERE {}", ambito.sql()), [], |f| f.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_rutas_y_etiquetas() {
        assert_eq!(rutas(&["./src\\auth.rs".into(), "src/auth.rs".into(), " docs/ ".into(), "".into()]), [
            "docs",
            "src/auth.rs"
        ]);
        assert_eq!(etiquetas(&["#Auth".into(), "auth".into(), " Login ".into()]), ["auth", "login"]);
    }

    #[test]
    fn titulo_en_una_linea() {
        assert_eq!(titulo("  dos\nlíneas  ").unwrap(), "dos líneas");
        assert!(titulo("   ").is_err());
        assert!(titulo(&"x".repeat(201)).is_err());
    }

    #[test]
    fn tipos() {
        assert_eq!("Gotcha".parse::<Tipo>().unwrap(), Tipo::Gotcha);
        assert!("bugfix".parse::<Tipo>().is_err());
    }
}
