//! `omoi tui`: ver, buscar, fijar y borrar memorias sin pasar por el agente.
//!
//! Lista a la izquierda (las recientes, o lo que encuentra la búsqueda), la memoria elegida a la
//! derecha. `/` busca mientras se escribe, Intro abre la memoria entera, `h` su historial, `p` la fija
//! o la suelta, `d` la borra (pide confirmación), `a` cambia entre este proyecto y todos, `q` sale.

use crate::buscar::{self, Opciones};
use crate::embeber::Modelo;
use crate::memoria::{self, Ambito, Cambios, Memoria, Version};
use crate::tr;
use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rusqlite::Connection;

const LIMITE: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vista {
    Lista,
    Buscando,
    Entera,
    Historial,
    Borrar,
}

pub struct Estado {
    pub proyecto: String,
    pub todos: bool,
    pub consulta: String,
    pub memorias: Vec<Memoria>,
    pub lista: ListState,
    pub vista: Vista,
    pub desplazamiento: u16,
    pub historial: Vec<Version>,
    pub aviso: String,
    pub salir: bool,
}

impl Estado {
    pub fn nuevo(proyecto: String) -> Estado {
        Estado {
            proyecto,
            todos: false,
            consulta: String::new(),
            memorias: Vec::new(),
            lista: ListState::default(),
            vista: Vista::Lista,
            desplazamiento: 0,
            historial: Vec::new(),
            aviso: String::new(),
            salir: false,
        }
    }

    fn ambito(&self, db: &Connection) -> Result<Ambito> {
        if self.todos { Ok(Ambito::Todos) } else { Ambito::de(db, &self.proyecto) }
    }

    pub fn elegida(&self) -> Option<&Memoria> {
        self.lista.selected().and_then(|i| self.memorias.get(i))
    }

    /// Vuelve a leer la lista (recientes o búsqueda) conservando la elegida si sigue.
    pub fn cargar(&mut self, db: &mut Connection, modelo: Option<&Modelo>) -> Result<()> {
        let antes = self.elegida().map(|m| m.id.clone());
        self.memorias = if self.consulta.trim().is_empty() {
            memoria::recientes(db, &self.ambito(db)?, None, LIMITE)?
        } else {
            let o = Opciones { ambito: self.ambito(db)?, kind: None, tags: vec![], limite: 50 };
            buscar::buscar_sin_apuntar_con(db, &self.consulta, &o, modelo)?.into_iter().map(|r| r.memoria).collect()
        };
        let i = antes.and_then(|id| self.memorias.iter().position(|m| m.id == id)).unwrap_or(0);
        self.lista.select((!self.memorias.is_empty()).then_some(i));
        Ok(())
    }

    /// Una tecla. Devuelve si hay que volver a cargar la lista.
    pub fn tecla(&mut self, k: KeyEvent, db: &mut Connection) -> Result<bool> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.salir = true;
            return Ok(false);
        }
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
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('s')) {
                    if let Some(m) = self.elegida().cloned() {
                        let antes = crate::revisar::antes(db)?;
                        memoria::olvidar(db, &m.id)?;
                        crate::revisar::al_dia_de(db, &antes, std::slice::from_ref(&m.id))?;
                        self.aviso = tr!("forgotten: {}", "olvidada: {}", m.title);
                    }
                    self.vista = Vista::Lista;
                    return Ok(true);
                }
                self.vista = Vista::Lista;
            }
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
                KeyCode::Down | KeyCode::Char('j') => self.mover(1),
                KeyCode::Up | KeyCode::Char('k') => self.mover(-1),
                KeyCode::PageDown => self.mover(15),
                KeyCode::PageUp => self.mover(-15),
                KeyCode::Enter | KeyCode::Right => {
                    if self.elegida().is_some() {
                        self.vista = Vista::Entera;
                        self.desplazamiento = 0;
                    }
                }
                KeyCode::Char('h') => {
                    if let Some(m) = self.elegida() {
                        self.historial = memoria::historial(db, &m.id)?;
                        self.vista = Vista::Historial;
                        self.desplazamiento = 0;
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(m) = self.elegida().cloned() {
                        let c = Cambios { pinned: Some(!m.pinned), reason: Some("pinned from omoi tui".into()), ..Default::default() };
                        let antes = crate::revisar::antes(db)?;
                        memoria::actualizar(db, &m.id, c, None)?;
                        crate::revisar::al_dia_de(db, &antes, std::slice::from_ref(&m.id))?;
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
                    return Ok(true);
                }
                _ => {}
            },
        }
        Ok(false)
    }

    fn mover(&mut self, n: i64) {
        if self.memorias.is_empty() {
            return;
        }
        let i = self.lista.selected().unwrap_or(0) as i64 + n;
        self.lista.select(Some(i.clamp(0, self.memorias.len() as i64 - 1) as usize));
    }
}

pub fn dibujar(f: &mut Frame, e: &mut Estado) {
    let [arriba, centro, abajo] = Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
    let ambito = if e.todos { tr!("all projects", "todos los proyectos") } else { tr!("this project + global", "este proyecto + globales") };
    let titulo_busqueda = format!(" {} · {ambito} · {} ", tr!("search", "buscar"), e.memorias.len());
    let cursor = if e.vista == Vista::Buscando { "▏" } else { "" };
    let caja = Block::default().borders(Borders::ALL).title(titulo_busqueda).border_style(if e.vista == Vista::Buscando {
        Style::default().bold()
    } else {
        Style::default()
    });
    f.render_widget(Paragraph::new(format!("{}{cursor}", e.consulta)).block(caja), arriba);

    match e.vista {
        Vista::Entera => dibujar_entera(f, e, centro),
        Vista::Historial => dibujar_historial(f, e, centro),
        _ => {
            let [izq, der] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(centro);
            let items: Vec<ListItem> = e
                .memorias
                .iter()
                .map(|m| {
                    let marca = if m.pinned { "* " } else { "  " };
                    ListItem::new(Line::from(vec![
                        Span::raw(marca),
                        Span::styled(format!("{:<10} ", m.kind.as_str()), Style::default().add_modifier(Modifier::DIM)),
                        Span::raw(m.title.clone()),
                    ]))
                })
                .collect();
            let lista = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tr!(" memories ", " memorias ")))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(lista, izq, &mut e.lista);
            let detalle = e.elegida().map(ficha).unwrap_or_default();
            f.render_widget(
                Paragraph::new(detalle).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)),
                der,
            );
        }
    }

    let ayuda = match e.vista {
        Vista::Buscando => tr!("type to search · Enter: to the list · Esc: clear", "escribe para buscar · Intro: a la lista · Esc: borrar"),
        Vista::Borrar => tr!("forget this memory and its history? y / n", "¿olvidar esta memoria y su historial? s / n"),
        Vista::Entera | Vista::Historial => tr!("↑↓ PgUp PgDn: scroll · Esc: back", "↑↓ RePág AvPág: desplazar · Esc: volver"),
        Vista::Lista => tr!(
            "/ search · Enter open · h history · p pin · d forget · a all projects · q quit",
            "/ buscar · Intro abrir · h historial · p fijar · d olvidar · a todos los proyectos · q salir"
        ),
    };
    let linea = if e.aviso.is_empty() { ayuda } else { format!("{} · {ayuda}", e.aviso) };
    f.render_widget(Paragraph::new(linea).style(Style::default().add_modifier(Modifier::DIM)), abajo);
}

/// La memoria como texto: título, datos y contenido.
fn ficha(m: &Memoria) -> Vec<Line<'static>> {
    let mut v = vec![Line::from(m.title.clone()).bold(), Line::raw("")];
    let ambito = if m.de_espacio() { tr!("space {}", "espacio {}", m.space) } else if m.project.is_empty() { tr!("global", "global") } else { m.project.clone() };
    v.push(Line::from(format!("{} · {ambito} · {}", m.kind.as_str(), &m.updated_at[..m.updated_at.len().min(10)])).dim());
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

fn dibujar_entera(f: &mut Frame, e: &Estado, area: Rect) {
    let Some(m) = e.elegida() else { return };
    let p = Paragraph::new(ficha(m)).wrap(Wrap { trim: false }).scroll((e.desplazamiento, 0)).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn dibujar_historial(f: &mut Frame, e: &Estado, area: Rect) {
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

/// El bucle de la terminal.
pub fn ejecutar(db: &mut Connection, modelo: Option<&Modelo>, proyecto: String, todos: bool) -> Result<()> {
    let mut e = Estado::nuevo(proyecto);
    e.todos = todos;
    e.cargar(db, modelo)?;
    let mut terminal = ratatui::init();
    let r = (|| -> Result<()> {
        while !e.salir {
            terminal.draw(|f| dibujar(f, &mut e))?;
            if let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                e.aviso.clear();
                if e.tecla(k, db)? {
                    e.cargar(db, modelo)?;
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    r
}
