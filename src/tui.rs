//! `omoi tui`: view, search, pin and delete memories without going through the agent.
//!
//! List on the left (the recent ones, or what the search finds), the selected memory on the
//! right (below, or only the list, if the terminal is narrow). `/` searches as you type, Enter
//! opens the whole memory, `h` its history, `p` pins or unpins it, `d` deletes it (asks for confirmation),
//! `a` switches between this project and all those visible from here, `e` filters by space, `t` opens the
//! trash (restore, delete for good, empty) and `q` quits.
//!
//! **Who sees what**: the same as the web view (`http::Viewpoint`), because it looks through it. From a
//! project in a space, "all" is that whole space plus global; from a project without a space, the
//! projects without a space plus global; from the user's folder, everything. The trash, the space
//! filter and what gets restored or deleted for good follow the same rule: another client's memories
//! don't show up, and aren't touched even if you have their id.

use crate::search::{self, SearchOptions};
use crate::embed::Model;
use crate::http::Viewpoint;
use crate::memory::{self, Scope, Changes, Memory, Forgotten, Version};
use crate::project::Space;
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
pub enum View {
    List,
    Searching,
    Full,
    History,
    Delete,
    /// The forgotten memories visible from here.
    Trash,
    /// Confirm deleting the selected trash item for good.
    TrashDelete,
    /// Confirm emptying the trash (what is visible from here).
    TrashEmpty,
}

/// A memory in the trash and how many days it has been there.
#[derive(Debug, Clone)]
pub struct TrashItem {
    pub forgotten: Forgotten,
    pub days: f64,
}

pub struct State {
    pub project: String,
    pub all: bool,
    /// Space filter: `None`, no filter; `Some(None)`, projects without a space; `Some(Some(id))`,
    /// that space. Global memories are always shown, as on the web. Only one visible from here is valid.
    pub space: Option<Option<i64>>,
    /// The spaces visible from here (for the filter and the names).
    pub spaces: Vec<Space>,
    /// Whether it's looking from the user's folder (everything is visible).
    pub sees_all: bool,
    /// The space key of the project being looked from (empty: none, or everything).
    pub own_space: String,
    pub query: String,
    pub memories: Vec<Memory>,
    pub list: ListState,
    pub view: View,
    pub scroll: u16,
    pub history: Vec<Version>,
    pub trash: Vec<TrashItem>,
    pub trash_list: ListState,
    pub notice: String,
    pub quit: bool,
}

impl State {
    pub fn new(project: String) -> State {
        State {
            project,
            all: false,
            space: None,
            spaces: Vec::new(),
            sees_all: false,
            own_space: String::new(),
            query: String::new(),
            memories: Vec::new(),
            list: ListState::default(),
            view: View::List,
            scroll: 0,
            history: Vec::new(),
            trash: Vec::new(),
            trash_list: ListState::default(),
            notice: String::new(),
            quit: false,
        }
    }

    /// Where it's looking from: like the web, worked out each time (a project can change space
    /// while the TUI is open, through sync).
    fn viewpoint(&self, db: &Connection) -> Result<Viewpoint> {
        Viewpoint::new(db, None, self.project.clone())
    }

    /// What the list shows: the space filter if there is one (and it's visible from here), otherwise "all" (the
    /// wide scope from here: its space, never everything from a project) or this project.
    pub fn scope(&self, db: &Connection) -> Result<Scope> {
        let m = self.viewpoint(db)?;
        if let Some(f) = self.space {
            let a = Scope::Space(f);
            if m.covers(&a) {
                return Ok(a);
            }
        }
        Ok(if self.all { m.view } else { m.base })
    }

    /// The space filters that can be picked from here: the visible spaces and, if visible,
    /// the projects without a space.
    pub fn space_filters(&self, db: &Connection) -> Result<Vec<Option<i64>>> {
        let m = self.viewpoint(db)?;
        let mut v: Vec<Option<i64>> = self.spaces.iter().map(|e| Some(e.id)).collect();
        v.push(None);
        Ok(v.into_iter().filter(|f| m.covers(&Scope::Space(*f))).collect())
    }

    pub fn selected(&self) -> Option<&Memory> {
        self.list.selected().and_then(|i| self.memories.get(i))
    }

    pub fn selected_trash(&self) -> Option<&TrashItem> {
        self.trash_list.selected().and_then(|i| self.trash.get(i))
    }

    /// A space's name from its key (the key if unknown).
    pub fn space_name(&self, key: &str) -> String {
        self.spaces.iter().find(|e| e.key == key).map(|e| e.name.clone()).unwrap_or_else(|| key.to_string())
    }

    /// Reloads the list (recent or search) and the trash, keeping the selection if it's still there.
    pub fn load(&mut self, db: &mut Connection, model: Option<&Model>) -> Result<()> {
        let m = self.viewpoint(db)?;
        self.sees_all = m.sees_all;
        self.own_space = m.space.clone();
        self.spaces = crate::project::spaces(db)?.into_iter().filter(|e| m.sees_space(&e.key, false)).collect();
        if let Some(f) = self.space
            && !m.covers(&Scope::Space(f))
        {
            self.space = None;
        }
        let scope = self.scope(db)?;
        let before = self.selected().map(|m| m.id.clone());
        self.memories = if self.query.trim().is_empty() {
            memory::recent(db, &scope, None, LIMIT)?
        } else {
            let o = SearchOptions { scope, kind: None, tags: vec![], limit: 50 };
            search::search_without_recording_with(db, &self.query, &o, model)?.into_iter().map(|r| r.memory).collect()
        };
        let i = before.and_then(|id| self.memories.iter().position(|m| m.id == id)).unwrap_or(0);
        self.list.select((!self.memories.is_empty()).then_some(i));
        self.load_trash(db, &m)
    }

    /// The trash as the web sees it (`GET /api/trash`): what is visible from here and, with a space
    /// filter, only that space's memories plus global.
    fn load_trash(&mut self, db: &Connection, m: &Viewpoint) -> Result<()> {
        let before = self.selected_trash().map(|o| o.forgotten.id.clone());
        let space_filter = match self.space {
            Some(f) => Some(crate::project::space_key(db, f)?),
            None => None,
        };
        let mut days_since = db.prepare("SELECT julianday('now') - julianday(?1)")?;
        let mut v = Vec::new();
        for o in memory::trash(db)? {
            if !m.sees(db, &o.id)? {
                continue;
            }
            if let Some(key) = &space_filter {
                let global = o.project.is_empty() && o.space.is_empty();
                if !global && o.space != *key {
                    continue;
                }
            }
            let d: Option<f64> = days_since.query_row([&o.deleted_at], |f| f.get(0))?;
            v.push(TrashItem { forgotten: o, days: d.unwrap_or(0.0).max(0.0) });
        }
        self.trash = v;
        let i = before.and_then(|id| self.trash.iter().position(|o| o.forgotten.id == id)).unwrap_or(0);
        self.trash_list.select((!self.trash.is_empty()).then_some(i));
        Ok(())
    }

    /// Restores memory `id` (full) from the trash, if visible from here: the same as
    /// `POST /api/restore` and `omoi restore`. The outcome goes in the notice.
    pub fn restore(&mut self, db: &mut Connection, id: &str, model: Option<&Model>) -> Result<bool> {
        if !self.viewpoint(db)?.sees(db, id)? {
            self.notice = out_of_scope();
            return Ok(false);
        }
        let before = crate::review::before(db)?;
        match memory::restore(db, id, model) {
            Ok(r) => {
                crate::review::refresh_for(db, &before, std::slice::from_ref(&r.id))?;
                self.notice = tr!("restored: {}", "recuperada: {}", r.title);
                Ok(true)
            }
            Err(e) => {
                self.notice = format!("{e:#}");
                Ok(false)
            }
        }
    }

    /// Deletes for good from the trash the memories `ids` (full) that are visible from here; the rest are
    /// left alone. Returns how many.
    pub fn delete_from_trash(&mut self, db: &mut Connection, ids: &[String]) -> Result<usize> {
        let m = self.viewpoint(db)?;
        let mut visible = Vec::new();
        for id in ids {
            if m.sees(db, id)? {
                visible.push(id.clone());
            }
        }
        if visible.len() < ids.len() {
            self.notice = out_of_scope();
        }
        memory::empty_trash_of(db, &visible)
    }

    /// Empties the visible part of the trash. From the user's folder and with no filter, the whole trash
    /// (like `omoi trash --empty`); from a project or with a filter, only what it shows.
    pub fn empty_trash(&mut self, db: &mut Connection) -> Result<usize> {
        if self.viewpoint(db)?.sees_all && self.space.is_none() {
            return memory::empty_trash(db);
        }
        let ids: Vec<String> = self.trash.iter().map(|o| o.forgotten.id.clone()).collect();
        self.delete_from_trash(db, &ids)
    }

    /// One key press. Returns whether the list must be reloaded.
    pub fn key(&mut self, k: KeyEvent, db: &mut Connection, model: Option<&Model>) -> Result<bool> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return Ok(false);
        }
        let yes = matches!(k.code, KeyCode::Char('y') | KeyCode::Char('s'));
        match self.view {
            View::Searching => match k.code {
                KeyCode::Esc => {
                    self.query.clear();
                    self.view = View::List;
                    return Ok(true);
                }
                KeyCode::Enter | KeyCode::Down => self.view = View::List,
                KeyCode::Backspace => {
                    self.query.pop();
                    return Ok(true);
                }
                KeyCode::Char(c) => {
                    self.query.push(c);
                    return Ok(true);
                }
                _ => {}
            },
            View::Delete => {
                if yes && let Some(m) = self.selected().cloned() {
                    let before = crate::review::before(db)?;
                    memory::forget(db, &m.id)?;
                    crate::review::refresh_for(db, &before, std::slice::from_ref(&m.id))?;
                    self.notice = tr!("forgotten: {} (t: trash)", "olvidada: {} (t: papelera)", m.title);
                    self.view = View::List;
                    return Ok(true);
                }
                self.view = View::List;
            }
            View::TrashDelete => {
                self.view = View::Trash;
                if yes && let Some(o) = self.selected_trash().cloned() {
                    if self.delete_from_trash(db, std::slice::from_ref(&o.forgotten.id))? > 0 {
                        self.notice = tr!("deleted for good: {}", "borrada del todo: {}", o.forgotten.title);
                    }
                    return Ok(true);
                }
            }
            View::TrashEmpty => {
                self.view = View::Trash;
                if yes {
                    let n = self.empty_trash(db)?;
                    self.notice = tr!("trash emptied: {n} deleted for good", "papelera vaciada: {n} borradas del todo");
                    return Ok(true);
                }
            }
            View::Trash => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') | KeyCode::Left => self.view = View::List,
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.trash_list, self.trash.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.trash_list, self.trash.len(), -1),
                KeyCode::PageDown => move_selection(&mut self.trash_list, self.trash.len(), 15),
                KeyCode::PageUp => move_selection(&mut self.trash_list, self.trash.len(), -15),
                KeyCode::Enter | KeyCode::Char('r') => {
                    if let Some(o) = self.selected_trash().cloned() {
                        return self.restore(db, &o.forgotten.id, model);
                    }
                }
                KeyCode::Char('x') | KeyCode::Delete if self.selected_trash().is_some() => self.view = View::TrashDelete,
                KeyCode::Char('E') if !self.trash.is_empty() => self.view = View::TrashEmpty,
                _ => {}
            },
            View::Full | View::History => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Left => {
                    self.view = View::List;
                    self.scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(15),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
                _ => {}
            },
            View::List => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Char('/') => self.view = View::Searching,
                KeyCode::Down | KeyCode::Char('j') => move_selection(&mut self.list, self.memories.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => move_selection(&mut self.list, self.memories.len(), -1),
                KeyCode::PageDown => move_selection(&mut self.list, self.memories.len(), 15),
                KeyCode::PageUp => move_selection(&mut self.list, self.memories.len(), -15),
                KeyCode::Enter | KeyCode::Right => {
                    if self.selected().is_some() {
                        self.view = View::Full;
                        self.scroll = 0;
                    }
                }
                KeyCode::Char('h') => {
                    if let Some(m) = self.selected() {
                        // Like `omoi history`: through the folder's scope, which never shows another client's memories.
                        self.history = memory::history_in(db, &self.viewpoint(db)?.base, &m.id)?;
                        self.view = View::History;
                        self.scroll = 0;
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(m) = self.selected().cloned() {
                        let c = Changes { pinned: Some(!m.pinned), reason: Some("pinned from omoi tui".into()), ..Default::default() };
                        let before = crate::review::before(db)?;
                        memory::update(db, &m.id, c, None)?;
                        crate::review::refresh_for(db, &before, std::slice::from_ref(&m.id))?;
                        self.notice = if m.pinned { tr!("unpinned", "soltada") } else { tr!("pinned", "fijada") };
                        return Ok(true);
                    }
                }
                KeyCode::Char('d') => {
                    if self.selected().is_some() {
                        self.view = View::Delete;
                    }
                }
                KeyCode::Char('a') => {
                    self.all = !self.all;
                    self.space = None;
                    return Ok(true);
                }
                KeyCode::Char('e') => {
                    // No filter → each visible space → projects without a space → no filter.
                    let filters = self.space_filters(db)?;
                    self.space = match self.space.and_then(|f| filters.iter().position(|x| *x == f)) {
                        None => filters.first().copied(),
                        Some(i) => filters.get(i + 1).copied(),
                    };
                    if self.space.is_none() && filters.is_empty() {
                        self.notice = tr!("no spaces to filter by from here", "desde aquí no hay espacios por los que filtrar");
                    }
                    return Ok(true);
                }
                KeyCode::Char('t') => {
                    self.view = View::Trash;
                    return Ok(true);
                }
                _ => {}
            },
        }
        Ok(false)
    }

    /// What is shown, in a few words, for the title.
    fn scope_text(&self) -> String {
        match self.space {
            Some(Some(id)) => {
                let name = self.spaces.iter().find(|e| e.id == id).map(|e| e.name.clone()).unwrap_or_default();
                tr!("space {} + global", "espacio {} + globales", name)
            }
            Some(None) => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None if !self.all => tr!("this project + global", "este proyecto + globales"),
            None if self.sees_all => tr!("all projects", "todos los proyectos"),
            None if self.own_space.is_empty() => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None => tr!("space {} + global", "espacio {} + globales", self.space_name(&self.own_space)),
        }
    }

    /// Whether the list mixes spaces (or global with a space) and should show each one's space.
    fn mixes_spaces(&self) -> bool {
        self.all || self.space.is_some()
    }

    /// A memory's (or forgotten memory's) space, short, for the list column.
    fn space_label(&self, project: &str, space: &str, space_level: bool) -> String {
        if space.is_empty() {
            return if project.is_empty() { tr!("global", "global") } else { "-".to_string() };
        }
        let n = self.space_name(space);
        if space_level { format!("◇{n}") } else { n }
    }

    /// Says where a memory lives: the project with its space, the space, or global.
    fn location(&self, project: &str, space: &str, space_level: bool) -> String {
        if space_level {
            return tr!("space {}", "espacio {}", self.space_name(space));
        }
        if project.is_empty() {
            return tr!("global", "global");
        }
        let short = std::path::Path::new(project).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| project.to_string());
        if space.is_empty() {
            tr!("{} · no space", "{} · sin espacio", short)
        } else {
            tr!("{} · space {}", "{} · espacio {}", short, self.space_name(space))
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

pub fn draw(f: &mut Frame, e: &mut State) {
    let area = f.area();
    let help = help(e);
    let line = if e.notice.is_empty() { help } else { format!("{} · {help}", e.notice) };
    // The help line, in as many rows as it needs (up to 3): it doesn't fit in one at 80 columns.
    let width = area.width.max(1) as usize;
    let footer_rows = (line.chars().count().div_ceil(width) as u16).clamp(1, 3);
    let [top, middle, bottom] = Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(footer_rows)]).areas(area);

    let in_trash = matches!(e.view, View::Trash | View::TrashDelete | View::TrashEmpty);
    let title = if in_trash {
        let days = memory::trash_days();
        let kept = if days > 0.0 { tr!("kept {} days", "se guarda {} días", days) } else { tr!("kept until emptied", "se guarda hasta vaciarla") };
        format!(" {} · {} · {} · {kept} ", tr!("trash", "papelera"), e.scope_text(), e.trash.len())
    } else {
        format!(" {} · {} · {} ", tr!("search", "buscar"), e.scope_text(), e.memories.len())
    };
    let cursor = if e.view == View::Searching { "▏" } else { "" };
    let search_box = Block::default().borders(Borders::ALL).title(title).border_style(if e.view == View::Searching {
        Style::default().bold()
    } else {
        Style::default()
    });
    f.render_widget(Paragraph::new(format!("{}{cursor}", e.query)).block(search_box), top);

    match e.view {
        View::Full => draw_full(f, e, middle),
        View::History => draw_history(f, e, middle),
        View::Trash | View::TrashDelete | View::TrashEmpty => draw_trash(f, e, middle),
        _ => {
            let (left, right) = split_area(middle);
            let inner = left.width.saturating_sub(2) as usize;
            let with_kind = inner >= 40;
            let with_space = e.mixes_spaces() && inner >= 30;
            let items: Vec<ListItem> = e
                .memories
                .iter()
                .map(|m| {
                    let marker = if m.pinned { "* " } else { "  " };
                    let mut v = vec![Span::raw(marker)];
                    if with_kind {
                        v.push(Span::styled(column(m.kind.as_str(), 10), Style::default().add_modifier(Modifier::DIM)));
                    }
                    if with_space {
                        v.push(Span::styled(column(&e.space_label(&m.project, &m.space, m.space_only()), 8), Style::default().add_modifier(Modifier::DIM)));
                    }
                    v.push(Span::raw(m.title.clone()));
                    ListItem::new(Line::from(v))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tr!(" memories ", " memorias ")))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(list, left, &mut e.list);
            if let Some(right) = right {
                let detail = e.selected().map(|m| detail_lines(e, m)).unwrap_or_default();
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

fn help(e: &State) -> String {
    match e.view {
        View::Searching => tr!("type to search · Enter: to the list · Esc: clear", "escribe para buscar · Intro: a la lista · Esc: borrar"),
        View::Delete => tr!("forget this memory? It goes to the trash with its history · y / n", "¿olvidar esta memoria? Va a la papelera con su historial · s / n"),
        View::Full | View::History => tr!("↑↓ PgUp PgDn: scroll · Esc: back", "↑↓ RePág AvPág: desplazar · Esc: volver"),
        View::List => tr!(
            "/ search · Enter open · h history · p pin · d forget · a all · e space · t trash · q quit",
            "/ buscar · Intro abrir · h historial · p fijar · d olvidar · a todos · e espacio · t papelera · q salir"
        ),
        View::Trash => tr!(
            "Enter/r restore · x delete for good · E empty · Esc back",
            "Intro/r recuperar · x borrar del todo · E vaciar · Esc volver"
        ),
        View::TrashDelete => {
            let t = e.selected_trash().map(|o| o.forgotten.title.clone()).unwrap_or_default();
            tr!("delete «{}» for good? It can't be undone · y / n", "¿borrar «{}» del todo? No se puede deshacer · s / n", t)
        }
        View::TrashEmpty => {
            let n = e.trash.len();
            if e.sees_all && e.space.is_none() {
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
fn detail_lines(e: &State, m: &Memory) -> Vec<Line<'static>> {
    let mut v = vec![Line::from(m.title.clone()).bold(), Line::raw("")];
    let location = e.location(&m.project, &m.space, m.space_only());
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

fn draw_full(f: &mut Frame, e: &State, area: Rect) {
    let Some(m) = e.selected() else { return };
    let p = Paragraph::new(detail_lines(e, m)).wrap(Wrap { trim: false }).scroll((e.scroll, 0)).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_history(f: &mut Frame, e: &State, area: Rect) {
    let mut v: Vec<Line> = Vec::new();
    if e.history.is_empty() {
        v.push(Line::raw(tr!("No previous versions.", "Sin versiones anteriores.")));
    }
    for h in &e.history {
        v.push(Line::from(format!("v{} · {} · {}", h.version, &h.changed_at[..h.changed_at.len().min(16)], h.reason.clone().unwrap_or_default())).bold());
        v.push(Line::raw(h.title.clone()));
        v.extend(h.content.lines().map(|l| Line::raw(l.to_string())));
        v.push(Line::raw(""));
    }
    let p = Paragraph::new(v)
        .wrap(Wrap { trim: false })
        .scroll((e.scroll, 0))
        .block(Block::default().borders(Borders::ALL).title(tr!(" history ", " historial ")));
    f.render_widget(p, area);
}

fn draw_trash(f: &mut Frame, e: &mut State, area: Rect) {
    let (left, right) = split_area(area);
    let inner = left.width.saturating_sub(2) as usize;
    let with_date = inner >= 34;
    let with_space = inner >= 30;
    let items: Vec<ListItem> = e
        .trash
        .iter()
        .map(|o| {
            let o = &o.forgotten;
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
    f.render_stateful_widget(list, left, &mut e.trash_list);
    let Some(right) = right else { return };
    let detail: Vec<Line> = match e.selected_trash() {
        None if empty => vec![Line::raw(tr!("The trash is empty.", "La papelera está vacía."))],
        None => Vec::new(),
        Some(p) => {
            let o = &p.forgotten;
            let space_level = o.project.is_empty() && !o.space.is_empty();
            let mut v = vec![Line::from(o.title.clone()).bold(), Line::raw("")];
            v.push(Line::from(format!("{} · {}", o.kind.as_str(), e.location(&o.project, &o.space, space_level))).dim());
            if !o.project.is_empty() {
                v.push(Line::from(o.project.clone()).dim());
            }
            v.push(Line::from(tr!("forgotten {} on {}", "olvidada el {} en {}", o.deleted_at.get(..10).unwrap_or(&o.deleted_at), o.by)).dim());
            let total = memory::trash_days();
            if total > 0.0 {
                let remaining = (total - p.days).max(0.0).ceil() as i64;
                v.push(Line::from(tr!("deleted for good in {remaining} days", "se borra del todo en {remaining} días")).dim());
            }
            v.push(Line::from(format!("id {}", o.id)).dim());
            v
        }
    };
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), right);
}

/// The terminal loop.
pub fn run(db: &mut Connection, model: Option<&Model>, project: String, all: bool) -> Result<()> {
    let mut e = State::new(project);
    e.all = all;
    e.load(db, model)?;
    let mut terminal = ratatui::init();
    let r = (|| -> Result<()> {
        while !e.quit {
            terminal.draw(|f| draw(f, &mut e))?;
            if let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                e.notice.clear();
                if e.key(k, db, model)? {
                    e.load(db, model)?;
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    r
}
