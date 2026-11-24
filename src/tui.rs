//! `omoi tui`: view, search, pin and delete memories without going through the agent.
//!
//! List on the left (the recent ones, or what the search finds), the selected memory on the
//! right (below, or only the list, if the terminal is narrow). `/` searches as you type, Enter
//! opens the whole memory, `h` its history, `p` pins or unpins it, `d` deletes it (asks for confirmation),
//! `a` switches between this project and all those visible from here, `e` filters by space, `t` opens the
//! trash (restore, delete for good, empty) and `q` quits.
//!
//! **Who sees what**: the same as the web view (`http::Mirador`), because it looks through it. From a
//! project in a space, "all" is that whole space plus global; from a project without a space, the
//! projects without a space plus global; from the user's folder, everything. The trash, the space
//! filter and what gets restored or deleted for good follow the same rule: another client's memories
//! don't show up, and aren't touched even if you have their id.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
use crate::http::Mirador;
use crate::memoria::{self, Ambito, Cambios, Memoria, Olvidada, Version};
use crate::proyecto::Espacio;
use crate::tr;
use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rusqlite::Connection;

const LIMIT: usize = 200;
/// Below this width, the detail pane goes below the list (or is hidden) instead of beside it.
const TWO_COLUMN_WIDTH: u16 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vista {
    Lista,
    Buscando,
    Entera,
    Historial,
    Borrar,
    /// The forgotten memories visible from here.
    Papelera,
    /// Confirm deleting the selected trash item for good.
    PapeleraBorrar,
    /// Confirm emptying the trash (what is visible from here).
    PapeleraVaciar,
}

/// A memory in the trash and how many days it has been there.
#[derive(Debug, Clone)]
pub struct EnPapelera {
    pub olvidada: Olvidada,
    pub dias: f64,
}

pub struct Estado {
    pub proyecto: String,
    pub todos: bool,
    /// Space filter: `None`, no filter; `Some(None)`, projects without a space; `Some(Some(id))`,
    /// that space. Global memories are always shown, as on the web. Only one visible from here is valid.
    pub espacio: Option<Option<i64>>,
    /// The spaces visible from here (for the filter and the names).
    pub espacios: Vec<Espacio>,
    /// Whether it's looking from the user's folder (everything is visible).
    pub ve_todo: bool,
    /// The space key of the project being looked from (empty: none, or everything).
    pub espacio_propio: String,
    pub consulta: String,
    pub memorias: Vec<Memoria>,
    pub lista: ListState,
    pub vista: Vista,
    pub desplazamiento: u16,
    pub historial: Vec<Version>,
    pub papelera: Vec<EnPapelera>,
    pub lista_papelera: ListState,
    pub aviso: String,
    pub salir: bool,
}

impl Estado {
    pub fn nuevo(project: String) -> Estado {
        Estado {
            proyecto: project,
            todos: false,
            espacio: None,
            espacios: Vec::new(),
            ve_todo: false,
            espacio_propio: String::new(),
            consulta: String::new(),
            memorias: Vec::new(),
            lista: ListState::default(),
            vista: Vista::Lista,
            desplazamiento: 0,
            historial: Vec::new(),
            papelera: Vec::new(),
            lista_papelera: ListState::default(),
            aviso: String::new(),
            salir: false,
        }
    }

    /// Where it's looking from: like the web, worked out each time (a project can change space
    /// while the TUI is open, through sync).
    fn viewpoint(&self, db: &Connection) -> Result<Mirador> {
        Mirador::nuevo(db, None, self.proyecto.clone())
    }

    /// What the list shows: the space filter if there is one (and it's visible from here), otherwise "all" (the
    /// wide scope from here: its space, never everything from a project) or this project.
    pub fn ambito(&self, db: &Connection) -> Result<Ambito> {
        let m = self.viewpoint(db)?;
        if let Some(f) = self.espacio {
            let a = Ambito::Espacio(f);
            if m.abarca(&a) {
                return Ok(a);
            }
        }
        Ok(if self.todos { m.vista } else { m.base })
    }

    /// The space filters that can be picked from here: the visible spaces and, if visible,
    /// the projects without a space.
    pub fn filtros(&self, db: &Connection) -> Result<Vec<Option<i64>>> {
        let m = self.viewpoint(db)?;
        let mut v: Vec<Option<i64>> = self.espacios.iter().map(|e| Some(e.id)).collect();
        v.push(None);
        Ok(v.into_iter().filter(|f| m.abarca(&Ambito::Espacio(*f))).collect())
    }

    pub fn elegida(&self) -> Option<&Memoria> {
        self.lista.selected().and_then(|i| self.memorias.get(i))
    }

    pub fn elegida_papelera(&self) -> Option<&EnPapelera> {
        self.lista_papelera.selected().and_then(|i| self.papelera.get(i))
    }

    /// A space's name from its key (the key if unknown).
    pub fn nombre_espacio(&self, key: &str) -> String {
        self.espacios.iter().find(|e| e.key == key).map(|e| e.name.clone()).unwrap_or_else(|| key.to_string())
    }

    /// Reloads the list (recent or search) and the trash, keeping the selection if it's still there.
    pub fn cargar(&mut self, db: &mut Connection, model: Option<&Modelo>) -> Result<()> {
        let m = self.viewpoint(db)?;
        self.ve_todo = m.todo;
        self.espacio_propio = m.espacio.clone();
        self.espacios = crate::proyecto::espacios(db)?.into_iter().filter(|e| m.ve_espacio(&e.key, false)).collect();
        if let Some(f) = self.espacio
            && !m.abarca(&Ambito::Espacio(f))
        {
            self.espacio = None;
        }
        let scope = self.ambito(db)?;
        let before = self.elegida().map(|m| m.id.clone());
        self.memorias = if self.consulta.trim().is_empty() {
            memoria::recientes(db, &scope, None, LIMIT)?
        } else {
            let o = Opciones { ambito: scope, kind: None, tags: vec![], limite: 50 };
            buscar::buscar_sin_apuntar_con(db, &self.consulta, &o, model)?.into_iter().map(|r| r.memoria).collect()
        };
        let i = before.and_then(|id| self.memorias.iter().position(|m| m.id == id)).unwrap_or(0);
        self.lista.select((!self.memorias.is_empty()).then_some(i));
        self.load_trash(db, &m)
    }

    /// The trash as the web sees it (`GET /api/trash`): what is visible from here and, with a space
    /// filter, only that space's memories plus global.
    fn load_trash(&mut self, db: &Connection, m: &Mirador) -> Result<()> {
        let before = self.elegida_papelera().map(|o| o.olvidada.id.clone());
        let space_filter = match self.espacio {
            Some(f) => Some(crate::proyecto::clave_espacio(db, f)?),
            None => None,
        };
        let mut days_since = db.prepare("SELECT julianday('now') - julianday(?1)")?;
        let mut v = Vec::new();
        for o in memoria::papelera(db)? {
            if !m.ve(db, &o.id)? {
                continue;
            }
            if let Some(key) = &space_filter {
                let global = o.project.is_empty() && o.space.is_empty();
                if !global && o.space != *key {
                    continue;
                }
            }
            let d: Option<f64> = days_since.query_row([&o.deleted_at], |f| f.get(0))?;
            v.push(EnPapelera { olvidada: o, dias: d.unwrap_or(0.0).max(0.0) });
        }
        self.papelera = v;
        let i = before.and_then(|id| self.papelera.iter().position(|o| o.olvidada.id == id)).unwrap_or(0);
        self.lista_papelera.select((!self.papelera.is_empty()).then_some(i));
        Ok(())
    }

    /// Restores memory `id` (full) from the trash, if visible from here: the same as
    /// `POST /api/restore` and `omoi restore`. The outcome goes in the notice.
    pub fn restaurar(&mut self, db: &mut Connection, id: &str, model: Option<&Modelo>) -> Result<bool> {
        if !self.viewpoint(db)?.ve(db, id)? {
            self.aviso = out_of_scope();
            return Ok(false);
        }
        let before = crate::revisar::antes(db)?;
        match memoria::restaurar(db, id, model) {
            Ok(r) => {
                crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&r.id))?;
                self.aviso = tr!("restored: {}", "recuperada: {}", r.title);
                Ok(true)
            }
            Err(e) => {
                self.aviso = format!("{e:#}");
                Ok(false)
            }
        }
    }

    /// Deletes for good from the trash the memories `ids` (full) that are visible from here; the rest are
    /// left alone. Returns how many.
    pub fn borrar_de_papelera(&mut self, db: &mut Connection, ids: &[String]) -> Result<usize> {
        let m = self.viewpoint(db)?;
        let mut visible = Vec::new();
        for id in ids {
            if m.ve(db, id)? {
                visible.push(id.clone());
            }
        }
        if visible.len() < ids.len() {
            self.aviso = out_of_scope();
        }
        memoria::vaciar_papelera_de(db, &visible)
    }

    /// Empties the visible part of the trash. From the user's folder and with no filter, the whole trash
    /// (like `omoi trash --empty`); from a project or with a filter, only what it shows.
    pub fn vaciar_papelera(&mut self, db: &mut Connection) -> Result<usize> {
        if self.viewpoint(db)?.todo && self.espacio.is_none() {
            return memoria::vaciar_papelera(db);
        }
        let ids: Vec<String> = self.papelera.iter().map(|o| o.olvidada.id.clone()).collect();
        self.borrar_de_papelera(db, &ids)
    }

    /// One key press. Returns whether the list must be reloaded.
    pub fn tecla(&mut self, k: KeyEvent, db: &mut Connection, model: Option<&Modelo>) -> Result<bool> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.salir = true;
            return Ok(false);
        }
        let yes = matches!(k.code, KeyCode::Char('y') | KeyCode::Char('s'));
        match self.vista {
            Vista::Buscando => match k.code {
                KeyCode::Esc => {
                    self.consulta.clear();
                    self.vista = Vista::Lista;
                    return Ok(true);
                }
                KeyCode::Enter | KeyCode::Down => self.vista = Vista::Lista,
                KeyCode::Backspace => {
                    self.consulta.pop();
                    return Ok(true);
                }
                KeyCode::Char(c) => {
                    self.consulta.push(c);
                    return Ok(true);
                }
                _ => {}
            },
            Vista::Borrar => {
                if yes && let Some(m) = self.elegida().cloned() {
                    let before = crate::revisar::antes(db)?;
                    memoria::olvidar(db, &m.id)?;
                    crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                    self.aviso = tr!("forgotten: {} (t: trash)", "olvidada: {} (t: papelera)", m.title);
                    self.vista = Vista::Lista;
                    return Ok(true);
                }
                self.vista = Vista::Lista;
            }
            Vista::PapeleraBorrar => {
                self.vista = Vista::Papelera;
                if yes && let Some(o) = self.elegida_papelera().cloned() {
                    if self.borrar_de_papelera(db, std::slice::from_ref(&o.olvidada.id))? > 0 {
                        self.aviso = tr!("deleted for good: {}", "borrada del todo: {}", o.olvidada.title);
                    }
                    return Ok(true);
                }
            }
            Vista::PapeleraVaciar => {
                self.vista = Vista::Papelera;
                if yes {
                    let n = self.vaciar_papelera(db)?;
                    self.aviso = tr!("trash emptied: {n} deleted for good", "papelera vaciada: {n} borradas del todo");
                    return Ok(true);
                }
            }
            Vista::Papelera => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') | KeyCode::Left => self.vista = Vista::Lista,
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.lista_papelera, self.papelera.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.lista_papelera, self.papelera.len(), -1),
                KeyCode::PageDown => move_selection(&mut self.lista_papelera, self.papelera.len(), 15),
                KeyCode::PageUp => move_selection(&mut self.lista_papelera, self.papelera.len(), -15),
                KeyCode::Enter | KeyCode::Char('r') => {
                    if let Some(o) = self.elegida_papelera().cloned() {
                        return self.restaurar(db, &o.olvidada.id, model);
                    }
                }
                KeyCode::Char('x') | KeyCode::Delete if self.elegida_papelera().is_some() => self.vista = Vista::PapeleraBorrar,
                KeyCode::Char('E') if !self.papelera.is_empty() => self.vista = Vista::PapeleraVaciar,
                _ => {}
            },
            Vista::Entera | Vista::Historial => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Left => {
                    self.vista = Vista::Lista;
                    self.desplazamiento = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => self.desplazamiento = self.desplazamiento.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.desplazamiento = self.desplazamiento.saturating_sub(1),
                KeyCode::PageDown => self.desplazamiento = self.desplazamiento.saturating_add(15),
                KeyCode::PageUp => self.desplazamiento = self.desplazamiento.saturating_sub(15),
                _ => {}
            },
            Vista::Lista => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => self.salir = true,
                KeyCode::Char('/') => self.vista = Vista::Buscando,
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.lista, self.memorias.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.lista, self.memorias.len(), -1),
                KeyCode::PageDown => move_selection(&mut self.lista, self.memorias.len(), 15),
                KeyCode::PageUp => move_selection(&mut self.lista, self.memorias.len(), -15),
                KeyCode::Enter | KeyCode::Right => {
                    if self.elegida().is_some() {
                        self.vista = Vista::Entera;
                        self.desplazamiento = 0;
                    }
                }
                KeyCode::Char('h') => {
                    if let Some(m) = self.elegida() {
                        // Like `omoi history`: through the folder's scope, which never shows another client's memories.
                        self.historial = memoria::historial_en(db, &self.viewpoint(db)?.base, &m.id)?;
                        self.vista = Vista::Historial;
                        self.desplazamiento = 0;
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(m) = self.elegida().cloned() {
                        let c = Cambios { pinned: Some(!m.pinned), reason: Some("pinned from omoi tui".into()), ..Default::default() };
                        let before = crate::revisar::antes(db)?;
                        memoria::actualizar(db, &m.id, c, None)?;
                        crate::revisar::al_dia_de(db, &before, std::slice::from_ref(&m.id))?;
                        self.aviso = if m.pinned { tr!("unpinned", "soltada") } else { tr!("pinned", "fijada") };
                        return Ok(true);
                    }
                }
                KeyCode::Char('d') => {
                    if self.elegida().is_some() {
                        self.vista = Vista::Borrar;
                    }
                }
                KeyCode::Char('a') => {
                    self.todos = !self.todos;
                    self.espacio = None;
                    return Ok(true);
                }
                KeyCode::Char('e') => {
                    // No filter → each visible space → projects without a space → no filter.
                    let filters = self.filtros(db)?;
                    self.espacio = match self.espacio.and_then(|f| filters.iter().position(|x| *x == f)) {
                        None => filters.first().copied(),
                        Some(i) => filters.get(i + 1).copied(),
                    };
                    if self.espacio.is_none() && filters.is_empty() {
                        self.aviso = tr!("no spaces to filter by from here", "desde aquí no hay espacios por los que filtrar");
                    }
                    return Ok(true);
                }
                KeyCode::Char('t') => {
                    self.vista = Vista::Papelera;
                    return Ok(true);
                }
                _ => {}
            },
        }
        Ok(false)
    }

    /// What is shown, in a few words, for the title.
    fn scope_text(&self) -> String {
        match self.espacio {
            Some(Some(id)) => {
                let name = self.espacios.iter().find(|e| e.id == id).map(|e| e.name.clone()).unwrap_or_default();
                tr!("space {} + global", "espacio {} + globales", name)
            }
            Some(None) => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None if !self.todos => tr!("this project + global", "este proyecto + globales"),
            None if self.ve_todo => tr!("all projects", "todos los proyectos"),
            None if self.espacio_propio.is_empty() => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None => tr!("space {} + global", "espacio {} + globales", self.nombre_espacio(&self.espacio_propio)),
        }
    }

    /// Whether the list mixes spaces (or global with a space) and should show each one's space.
    fn mixes_spaces(&self) -> bool {
        self.todos || self.espacio.is_some()
    }

    /// A memory's (or forgotten memory's) space, short, for the list column.
    fn space_label(&self, project: &str, space: &str, space_level: bool) -> String {
        if space.is_empty() {
            return if project.is_empty() { tr!("global", "global") } else { "-".to_string() };
        }
        let n = self.nombre_espacio(space);
        if space_level { format!("◇{n}") } else { n }
    }

    /// Says where a memory lives: the project with its space, the space, or global.
    fn location(&self, project: &str, space: &str, space_level: bool) -> String {
        if space_level {
            return tr!("space {}", "espacio {}", self.nombre_espacio(space));
        }
        if project.is_empty() {
            return tr!("global", "global");
        }
        let short = std::path::Path::new(project).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| project.to_string());
        if space.is_empty() {
            tr!("{} · no space", "{} · sin espacio", short)
        } else {
            tr!("{} · space {}", "{} · espacio {}", short, self.nombre_espacio(space))
        }
    }
}

fn out_of_scope() -> String {
    tr!(
        "that memory is not in this scope: it belongs to another space; open omoi tui in that project, or in the home folder",
        "esa memoria no es de este ámbito: es de otro espacio; abre omoi tui en ese proyecto, o en tu carpeta de usuario"
    )
}

fn move_selection(list: &mut ListState, len: usize, n: i64) {
    if len == 0 {
        return;
    }
    let i = list.selected().unwrap_or(0) as i64 + n;
    list.select(Some(i.clamp(0, len as i64 - 1) as usize));
}

/// `text` cut to `width` characters and padded up to it.
fn column(text: &str, width: usize) -> String {
    format!("{text:<width$.width$} ")
}

pub fn dibujar(f: &mut Frame, e: &mut Estado) {
    let area = f.area();
    let help = help(e);
    let line = if e.aviso.is_empty() { help } else { format!("{} · {help}", e.aviso) };
    // The help line, in as many rows as it needs (up to 3): it doesn't fit in one at 80 columns.
    let width = area.width.max(1) as usize;
    let footer_rows = (line.chars().count().div_ceil(width) as u16).clamp(1, 3);
    let [top, middle, bottom] = Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(footer_rows)]).areas(area);

    let in_trash = matches!(e.vista, Vista::Papelera | Vista::PapeleraBorrar | Vista::PapeleraVaciar);
    let title = if in_trash {
        let days = memoria::dias_papelera();
        let kept = if days > 0.0 { tr!("kept {} days", "se guarda {} días", days) } else { tr!("kept until emptied", "se guarda hasta vaciarla") };
        format!(" {} · {} · {} · {kept} ", tr!("trash", "papelera"), e.scope_text(), e.papelera.len())
    } else {
        format!(" {} · {} · {} ", tr!("search", "buscar"), e.scope_text(), e.memorias.len())
    };
    let cursor = if e.vista == Vista::Buscando { "▏" } else { "" };
    let search_box = Block::default().borders(Borders::ALL).title(title).border_style(if e.vista == Vista::Buscando {
        Style::default().bold()
    } else {
        Style::default()
    });
    f.render_widget(Paragraph::new(format!("{}{cursor}", e.consulta)).block(search_box), top);

    match e.vista {
        Vista::Entera => draw_full(f, e, middle),
        Vista::Historial => draw_history(f, e, middle),
        Vista::Papelera | Vista::PapeleraBorrar | Vista::PapeleraVaciar => draw_trash(f, e, middle),
        _ => {
            let (left, right) = split_area(middle);
            let inner = left.width.saturating_sub(2) as usize;
            let with_kind = inner >= 40;
            let with_space = e.mixes_spaces() && inner >= 30;
            let items: Vec<ListItem> = e
                .memorias
                .iter()
                .map(|m| {
                    let marker = if m.pinned { "* " } else { "  " };
                    let mut v = vec![Span::raw(marker)];
                    if with_kind {
                        v.push(Span::styled(column(m.kind.as_str(), 10), Style::default().add_modifier(Modifier::DIM)));
                    }
                    if with_space {
                        v.push(Span::styled(column(&e.space_label(&m.project, &m.space, m.de_espacio()), 8), Style::default().add_modifier(Modifier::DIM)));
                    }
                    v.push(Span::raw(m.title.clone()));
                    ListItem::new(Line::from(v))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tr!(" memories ", " memorias ")))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(list, left, &mut e.lista);
            if let Some(right) = right {
                let detail = e.elegida().map(|m| detail_lines(e, m)).unwrap_or_default();
                f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), right);
            }
        }
    }

    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }).style(Style::default().add_modifier(Modifier::DIM)), bottom);
}

/// The list and, if it fits, the detail pane: beside it in a wide terminal, below in a narrow tall one, and
/// none if there's no room (Enter opens it in full).
fn split_area(area: Rect) -> (Rect, Option<Rect>) {
    if area.width >= TWO_COLUMN_WIDTH {
        let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
        (left, Some(right))
    } else if area.height >= 14 {
        let [top, bottom] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
        (top, Some(bottom))
    } else {
        (area, None)
    }
}

fn help(e: &Estado) -> String {
    match e.vista {
        Vista::Buscando => tr!("type to search · Enter: to the list · Esc: clear", "escribe para buscar · Intro: a la lista · Esc: borrar"),
        Vista::Borrar => tr!("forget this memory? It goes to the trash with its history · y / n", "¿olvidar esta memoria? Va a la papelera con su historial · s / n"),
        Vista::Entera | Vista::Historial => tr!("↑↓ PgUp PgDn: scroll · Esc: back", "↑↓ RePág AvPág: desplazar · Esc: volver"),
        Vista::Lista => tr!(
            "/ search · Enter open · h history · p pin · d forget · a all · e space · t trash · q quit",
            "/ buscar · Intro abrir · h historial · p fijar · d olvidar · a todos · e espacio · t papelera · q salir"
        ),
        Vista::Papelera => tr!(
            "Enter/r restore · x delete for good · E empty · Esc back",
            "Intro/r recuperar · x borrar del todo · E vaciar · Esc volver"
        ),
        Vista::PapeleraBorrar => {
            let t = e.elegida_papelera().map(|o| o.olvidada.title.clone()).unwrap_or_default();
            tr!("delete «{}» for good? It can't be undone · y / n", "¿borrar «{}» del todo? No se puede deshacer · s / n", t)
        }
        Vista::PapeleraVaciar => {
            let n = e.papelera.len();
            if e.ve_todo && e.espacio.is_none() {
                tr!("empty the trash ({n} memories)? It can't be undone · y / n", "¿vaciar la papelera ({n} memorias)? No se puede deshacer · s / n")
            } else {
                tr!(
                    "empty what is listed ({n} memories; the rest of the trash stays)? It can't be undone · y / n",
                    "¿vaciar lo que se ve ({n} memorias; el resto de la papelera se queda)? No se puede deshacer · s / n"
                )
            }
        }
    }
}

/// The memory as text: title, metadata and content.
fn detail_lines(e: &Estado, m: &Memoria) -> Vec<Line<'static>> {
    let mut v = vec![Line::from(m.title.clone()).bold(), Line::raw("")];
    let location = e.location(&m.project, &m.space, m.de_espacio());
    v.push(Line::from(format!("{} · {location} · {}", m.kind.as_str(), &m.updated_at[..m.updated_at.len().min(10)])).dim());
    if !m.project.is_empty() {
        v.push(Line::from(m.project.clone()).dim());
    }
    if !m.tags.is_empty() {
        v.push(Line::from(m.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")).dim());
    }
    if !m.paths.is_empty() {
        v.push(Line::from(m.paths.join(", ")).dim());
    }
    v.push(Line::raw(""));
    v.extend(m.content.lines().map(|l| Line::raw(l.to_string())));
    v
}

fn draw_full(f: &mut Frame, e: &Estado, area: Rect) {
    let Some(m) = e.elegida() else { return };
    let p = Paragraph::new(detail_lines(e, m)).wrap(Wrap { trim: false }).scroll((e.desplazamiento, 0)).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_history(f: &mut Frame, e: &Estado, area: Rect) {
    let mut v: Vec<Line> = Vec::new();
    if e.historial.is_empty() {
        v.push(Line::raw(tr!("No previous versions.", "Sin versiones anteriores.")));
    }
    for h in &e.historial {
        v.push(Line::from(format!("v{} · {} · {}", h.version, &h.changed_at[..h.changed_at.len().min(16)], h.reason.clone().unwrap_or_default())).bold());
        v.push(Line::raw(h.title.clone()));
        v.extend(h.content.lines().map(|l| Line::raw(l.to_string())));
        v.push(Line::raw(""));
    }
    let p = Paragraph::new(v)
        .wrap(Wrap { trim: false })
        .scroll((e.desplazamiento, 0))
        .block(Block::default().borders(Borders::ALL).title(tr!(" history ", " historial ")));
    f.render_widget(p, area);
}

fn draw_trash(f: &mut Frame, e: &mut Estado, area: Rect) {
    let (left, right) = split_area(area);
    let inner = left.width.saturating_sub(2) as usize;
    let with_date = inner >= 34;
    let with_space = inner >= 30;
    let items: Vec<ListItem> = e
        .papelera
        .iter()
        .map(|o| {
            let o = &o.olvidada;
            let mut v = Vec::new();
            if with_date {
                v.push(Span::styled(column(o.deleted_at.get(..10).unwrap_or(&o.deleted_at), 10), Style::default().add_modifier(Modifier::DIM)));
            }
            if with_space {
                let space_level = o.project.is_empty() && !o.space.is_empty();
                v.push(Span::styled(column(&e.space_label(&o.project, &o.space, space_level), 8), Style::default().add_modifier(Modifier::DIM)));
            }
            v.push(Span::raw(o.title.clone()));
            ListItem::new(Line::from(v))
        })
        .collect();
    let empty = items.is_empty();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(tr!(" trash ", " papelera ")))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, left, &mut e.lista_papelera);
    let Some(right) = right else { return };
    let detail: Vec<Line> = match e.elegida_papelera() {
        None if empty => vec![Line::raw(tr!("The trash is empty.", "La papelera está vacía."))],
        None => Vec::new(),
        Some(p) => {
            let o = &p.olvidada;
            let space_level = o.project.is_empty() && !o.space.is_empty();
            let mut v = vec![Line::from(o.title.clone()).bold(), Line::raw("")];
            v.push(Line::from(format!("{} · {}", o.kind.as_str(), e.location(&o.project, &o.space, space_level))).dim());
            if !o.project.is_empty() {
                v.push(Line::from(o.project.clone()).dim());
            }
            v.push(Line::from(tr!("forgotten {} on {}", "olvidada el {} en {}", o.deleted_at.get(..10).unwrap_or(&o.deleted_at), o.by)).dim());
            let total = memoria::dias_papelera();
            if total > 0.0 {
                let remaining = (total - p.dias).max(0.0).ceil() as i64;
                v.push(Line::from(tr!("deleted for good in {remaining} days", "se borra del todo en {remaining} días")).dim());
            }
            v.push(Line::from(format!("id {}", o.id)).dim());
            v
        }
    };
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), right);
}

/// The terminal loop.
pub fn ejecutar(db: &mut Connection, model: Option<&Modelo>, project: String, all: bool) -> Result<()> {
    let mut e = Estado::nuevo(project);
    e.todos = all;
    e.cargar(db, model)?;
    let mut terminal = ratatui::init();
    let r = (|| -> Result<()> {
        while !e.salir {
            terminal.draw(|f| dibujar(f, &mut e))?;
            if let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                e.aviso.clear();
                if e.tecla(k, db, model)? {
                    e.cargar(db, model)?;
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    r
}
