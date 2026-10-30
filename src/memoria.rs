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

/// Cada tipo cambia cómo se recupera la memoria (ver PLAN.md §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tipo {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Memoria {
    pub id: String,
    /// Raíz del proyecto; vacío = global.
    pub project: String,
    pub kind: Tipo,
    pub title: String,
    pub content: String,
    pub paths: Vec<String>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub source: String,
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

/// Lo que decide quien guarda. Lo demás (id, fechas, estadísticas) lo pone Omoikane.
#[derive(Clone, Debug, Default)]
pub struct Nueva {
    pub project: String,
    pub kind: Option<Tipo>,
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

/// Qué memorias se ven: las del proyecto más las globales, o todas.
#[derive(Clone, Debug)]
pub enum Ambito {
    Proyecto(String),
    Todos,
}

impl Ambito {
    /// Lo que se ve desde un proyecto: él y las globales; desde una carpeta general (la del
    /// usuario, ver `proyecto::es_general`), todo.
    pub fn de(project: &str) -> Ambito {
        if crate::proyecto::es_general(project) { Ambito::Todos } else { Ambito::Proyecto(project.to_string()) }
    }


    /// Valor para el parámetro `?` de `(? IS NULL OR project = ? OR project = '')`.
    pub(crate) fn filtro(&self) -> Option<&str> {
        match self {
            Ambito::Proyecto(p) => Some(p),
            Ambito::Todos => None,
        }
    }
}

pub(crate) const AHORA: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
pub(crate) const COLUMNAS: &str =
    "id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at, recalled_at, recall_count, author";

pub(crate) fn de_fila(f: &Row) -> rusqlite::Result<Memoria> {
    let lista = |i: usize| -> rusqlite::Result<Vec<String>> {
        let s: String = f.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let kind: String = f.get(2)?;
    Ok(Memoria {
        id: f.get(0)?,
        project: f.get(1)?,
        kind: kind.parse().unwrap_or(Tipo::Fact),
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

/// De dónde viene una memoria que llega por la sincronización: su id, fechas, sello y autor de origen.
pub(crate) struct Origen<'a> {
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub hlc: &'a str,
    pub author: &'a str,
}

/// Para la sincronización: la misma memoria, con el mismo id, fechas, sello y autor que en su equipo.
pub(crate) fn guardar_con_id(db: &mut Connection, id: &str, nueva: Nueva, o: &Origen, modelo: Option<&Modelo>) -> Result<Memoria> {
    insertar(db, id, nueva, Some(o), modelo)
}

/// El sello de una memoria (vacío si no existe).
fn hlc_de(db: &Connection, id: &str) -> Result<String> {
    Ok(db.query_row("SELECT hlc FROM memory WHERE id = ?1", [id], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// Apunta una marca de olvido, salvo que ya haya una posterior.
pub(crate) fn marcar_olvido(db: &Connection, id: &str, deleted_at: Option<&str>, hlc: &str) -> Result<()> {
    let actual: Option<String> = db.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [id], |f| f.get(0)).optional()?;
    if actual.is_none_or(|a| crate::reloj::mayor(hlc, &a)) {
        db.execute(
            &format!("INSERT OR REPLACE INTO tombstone(id, deleted_at, hlc) VALUES (?1, COALESCE(?2, {AHORA}), ?3)"),
            params![id, deleted_at, hlc],
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
    tx.execute(
        &format!(
            "INSERT INTO memory(id, project, kind, title, content, paths, tags, pinned, source, created_at, updated_at, hlc, author)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, COALESCE(?10, {AHORA}), COALESCE(?11, {AHORA}), ?12, ?13)"
        ),
        params![
            id,
            nueva.project,
            kind.as_str(),
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
    db.query_row(&format!("SELECT {COLUMNAS} FROM memory WHERE id = ?1"), [id], de_fila)
        .optional()?
        .ok_or_else(|| anyhow!(tr!("no memory with id {id}", "no existe la memoria {id}")))
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
        paths: c.paths.as_deref().map(rutas).unwrap_or_else(|| antes.paths.clone()),
        tags: c.tags.as_deref().map(etiquetas).unwrap_or_else(|| antes.tags.clone()),
        pinned: c.pinned.unwrap_or(antes.pinned),
        project: c.project.clone().unwrap_or_else(|| antes.project.clone()),
        ..antes.clone()
    };
    let cambia_texto = despues.title != antes.title
        || despues.content != antes.content
        || despues.kind != antes.kind
        || despues.paths != antes.paths
        || despues.tags != antes.tags;
    if !cambia_texto {
        // Fijar o desfijar no es un cambio de contenido: sin versión nueva y sin tocar updated_at
        // (que da la recencia en la búsqueda).
        if despues.pinned != antes.pinned || despues.project != antes.project || origen.is_some() {
            tx.execute(
                "UPDATE memory SET pinned = ?2, project = ?3, hlc = ?4 WHERE id = ?1",
                params![id, despues.pinned, despues.project, hlc],
            )?;
            if let Some(o) = origen {
                tx.execute("UPDATE memory SET updated_at = ?2, author = ?3 WHERE id = ?1", params![id, o.updated_at, o.author])?;
            }
        }
        return Ok((Memoria { pinned: despues.pinned, project: despues.project, redacted, ..antes }, cambiados));
    }
    // La foto anterior va al historial.
    guardar_version(tx, id, &antes, None, c.reason.as_deref())?;
    tx.execute(
        &format!(
            "UPDATE memory SET title = ?2, content = ?3, kind = ?4, paths = ?5, tags = ?6, pinned = ?7, project = ?8,
             updated_at = {AHORA}, hlc = ?9, author = CASE WHEN author = '' THEN ?10 ELSE author END WHERE id = ?1"
        ),
        params![
            id,
            despues.title,
            despues.content,
            despues.kind.as_str(),
            serde_json::to_string(&despues.paths)?,
            serde_json::to_string(&despues.tags)?,
            despues.pinned,
            despues.project,
            hlc,
            autor(tx)?,
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
            m.kind.as_str(),
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
            let m: Memoria = serde_json::from_str(&m).unwrap_or_else(|_| Memoria {
                id: String::new(),
                project: String::new(),
                kind: Tipo::Fact,
                title: String::new(),
                content: String::new(),
                paths: Vec::new(),
                tags: Vec::new(),
                pinned: false,
                source: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                recalled_at: None,
                recall_count: 0,
                author: String::new(),
                redacted: Vec::new(),
            });
            Ok(Olvidada { id: f.get(0)?, title: m.title, kind: m.kind, project: m.project, by: f.get(2)?, deleted_at: f.get(3)? })
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
    let filas: Vec<(String, String, String)> = {
        let mut q = tx.prepare("SELECT id, memory, versions FROM forgotten WHERE id >= ?1 AND id < ?1 || '~' ORDER BY id LIMIT 2")?;
        q.query_map([&id], |f| Ok((f.get(0)?, f.get(1)?, f.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let (id, m, versiones) = match <[_; 1]>::try_from(filas) {
        Ok([una]) => una,
        Err(v) if v.is_empty() => bail!(tr!("nothing in the trash with id {id}", "no hay nada en la papelera con id {id}")),
        Err(_) => bail!(tr!("several memories in the trash start with {id}: type more characters", "hay varias memorias en la papelera que empiezan por {id}: escribe más caracteres")),
    };
    let m: Memoria = serde_json::from_str(&m)?;
    let versiones: Vec<Version> = serde_json::from_str(&versiones).unwrap_or_default();
    let marca: String = tx.query_row("SELECT hlc FROM tombstone WHERE id = ?1", [&id], |f| f.get(0)).optional()?.unwrap_or_default();
    let hlc = crate::reloj::sello_tras(&tx, &marca)?;
    let nueva = Nueva { project: m.project.clone(), kind: Some(m.kind), title: m.title.clone(), content: m.content.clone(), paths: m.paths.clone(), tags: m.tags.clone(), pinned: m.pinned, source: m.source.clone() };
    let o = Origen { created_at: &m.created_at, updated_at: &m.updated_at, hlc: &hlc, author: &m.author };
    tx.execute("DELETE FROM tombstone WHERE id = ?1", [&id])?;
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
    marcar_olvido(tx, id, deleted_at, &hlc)?;
    let m = leer(tx, id)?;
    // A la papelera, con su historial: `restaurar` la devuelve desde cualquier equipo. `by` es el
    // equipo que la olvidó: otro (quizá otra persona) o este.
    let quien = match marca {
        Some((_, _, equipo)) => equipo.to_string(),
        None => crate::reloj::equipo(tx)?,
    };
    let versiones = historial(tx, id)?;
    let hlc_m = hlc_de(tx, id)?;
    tx.execute(
        &format!("INSERT OR REPLACE INTO forgotten(id, memory, versions, hlc, deleted_at, by) VALUES (?1, ?2, ?3, ?4, COALESCE(?5, {AHORA}), ?6)"),
        params![id, serde_json::to_string(&m)?, serde_json::to_string(&versiones)?, hlc_m, deleted_at, quien],
    )?;
    let n: i64 = tx.query_row("SELECT n FROM memory WHERE id = ?1", [id], |f| f.get(0))?;
    desindexar(tx, n)?;
    // memory_version y memory_path caen con ON DELETE CASCADE.
    tx.execute("DELETE FROM memory WHERE id = ?1", [id])?;
    Ok(m)
}

/// Versiones anteriores, de la más reciente a la más antigua.
pub fn historial(db: &Connection, id: &str) -> Result<Vec<Version>> {
    let id = resolver_id(db, id)?;
    let mut q = db.prepare(
        "SELECT version, kind, title, content, paths, tags, changed_at, reason
         FROM memory_version WHERE memory_id = ?1 ORDER BY version DESC",
    )?;
    let v = q
        .query_map([&id], |f| {
            let lista = |i: usize| -> rusqlite::Result<Vec<String>> {
                Ok(serde_json::from_str(&f.get::<_, String>(i)?).unwrap_or_default())
            };
            Ok(Version {
                version: f.get(0)?,
                kind: f.get(1)?,
                title: f.get(2)?,
                content: f.get(3)?,
                paths: lista(4)?,
                tags: lista(5)?,
                changed_at: f.get(6)?,
                reason: f.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Las últimas guardadas o cambiadas.
pub fn recientes(db: &Connection, ambito: &Ambito, kind: Option<Tipo>, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory
         WHERE (?1 IS NULL OR project = ?1 OR project = '') AND (?2 IS NULL OR kind = ?2)
         ORDER BY updated_at DESC, id DESC LIMIT ?3"
    ))?;
    let v = q
        .query_map(params![ambito.filtro(), kind.map(Tipo::as_str), limite as i64], de_fila)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// La etiqueta de las memorias que dicen en qué punto se quedó el trabajo de un proyecto.
pub const ESTADO: &str = "status";

/// En qué punto se quedó cada proyecto: la memoria más reciente con la etiqueta [`ESTADO`] de cada
/// uno (las globales cuentan como un proyecto más), de la más nueva a la más vieja.
pub fn estados(db: &Connection, ambito: &Ambito, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory
         WHERE (?1 IS NULL OR project = ?1 OR project = '')
           AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE value = ?2)
         ORDER BY updated_at DESC, id DESC"
    ))?;
    let todas: Vec<Memoria> = q.query_map(params![ambito.filtro(), ESTADO], de_fila)?.collect::<rusqlite::Result<_>>()?;
    let mut v: Vec<Memoria> = Vec::new();
    for m in todas {
        if v.len() < limite && !v.iter().any(|x| x.project == m.project) {
            v.push(m);
        }
    }
    Ok(v)
}

/// Lo que el agente debería saber siempre al empezar en un proyecto: las preferencias y las fijadas,
/// del proyecto y globales. Primero las fijadas, luego las más recientes.
pub fn contexto(db: &Connection, project: &str, limite: usize) -> Result<Vec<Memoria>> {
    let mut q = db.prepare(&format!(
        "SELECT {COLUMNAS} FROM memory
         WHERE (project = ?1 OR project = '') AND (kind = 'preference' OR pinned = 1)
         ORDER BY pinned DESC, updated_at DESC, id DESC LIMIT ?2"
    ))?;
    let v = q.query_map(params![project, limite as i64], de_fila)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

#[derive(Clone, Debug, Serialize)]
pub struct Proyecto {
    /// Vacío = memorias globales.
    pub project: String,
    pub memories: i64,
    pub last_update: String,
}

pub fn proyectos(db: &Connection) -> Result<Vec<Proyecto>> {
    let mut q = db.prepare(
        "SELECT project, count(*), max(updated_at) FROM memory GROUP BY project ORDER BY max(updated_at) DESC",
    )?;
    let v = q
        .query_map([], |f| Ok(Proyecto { project: f.get(0)?, memories: f.get(1)?, last_update: f.get(2)? }))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
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
