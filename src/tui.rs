//! `omoi tui`: ver, buscar, fijar y borrar memorias sin pasar por el agente.
//!
//! Lista a la izquierda (las recientes, o lo que encuentra la búsqueda), la memoria elegida a la
//! derecha (debajo, o solo la lista, si la terminal es estrecha). `/` busca mientras se escribe, Intro
//! abre la memoria entera, `h` su historial, `p` la fija o la suelta, `d` la borra (pide confirmación),
//! `a` cambia entre este proyecto y todos los que se ven desde aquí, `e` filtra por espacio, `t` abre la
//! papelera (recuperar, borrar del todo, vaciar) y `q` sale.
//!
//! **Quién ve qué**: lo mismo que la vista web (`http::Mirador`), porque mira con ella. Desde un
//! proyecto de un espacio, «todos» es ese espacio entero y lo global; desde un proyecto sin espacio, los
//! proyectos sin espacio y lo global; desde la carpeta del usuario, todo. La papelera, el filtro por
//! espacio y lo que se recupera o se borra del todo pasan por la misma regla: lo de otro cliente no
//! aparece, ni se toca aunque se tenga su id.

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

const LIMITE: usize = 200;
/// Por debajo de este ancho, la ficha va debajo de la lista (o no va) en vez de al lado.
const ANCHO_DOS_COLUMNAS: u16 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vista {
    Lista,
    Buscando,
    Entera,
    Historial,
    Borrar,
    /// Lo olvidado que se ve desde aquí.
    Papelera,
    /// Confirmar que se borra del todo la elegida de la papelera.
    PapeleraBorrar,
    /// Confirmar que se vacía la papelera (lo que se ve desde aquí).
    PapeleraVaciar,
}

/// Una memoria de la papelera y cuántos días lleva en ella.
#[derive(Debug, Clone)]
pub struct EnPapelera {
    pub olvidada: Olvidada,
    pub dias: f64,
}

pub struct Estado {
    pub proyecto: String,
    pub todos: bool,
    /// Filtro por espacio: `None`, sin filtro; `Some(None)`, los proyectos sin espacio; `Some(Some(id))`,
    /// ese espacio. Lo global se ve siempre, como en la web. Solo vale uno que se vea desde aquí.
    pub espacio: Option<Option<i64>>,
    /// Los espacios que se ven desde aquí (para el filtro y los nombres).
    pub espacios: Vec<Espacio>,
    /// Si se mira desde la carpeta del usuario (se ve todo).
    pub ve_todo: bool,
    /// La clave del espacio del proyecto desde el que se mira (vacía: ninguno, o todo).
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
    pub fn nuevo(proyecto: String) -> Estado {
        Estado {
            proyecto,
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

    /// Desde dónde se mira: como la web, se calcula cada vez (un proyecto puede cambiar de espacio
    /// mientras la TUI está abierta, por la sincronización).
    fn mirador(&self, db: &Connection) -> Result<Mirador> {
        Mirador::nuevo(db, None, self.proyecto.clone())
    }

    /// Lo que enseña la lista: el filtro de espacio si hay uno (y se ve desde aquí), si no «todos» (lo
    /// amplio desde aquí: su espacio, nunca todo desde un proyecto) o este proyecto.
    pub fn ambito(&self, db: &Connection) -> Result<Ambito> {
        let m = self.mirador(db)?;
        if let Some(f) = self.espacio {
            let a = Ambito::Espacio(f);
            if m.abarca(&a) {
                return Ok(a);
            }
        }
        Ok(if self.todos { m.vista } else { m.base })
    }

    /// Los filtros de espacio que se pueden elegir desde aquí: los espacios que se ven y, si se ven,
    /// los proyectos sin espacio.
    pub fn filtros(&self, db: &Connection) -> Result<Vec<Option<i64>>> {
        let m = self.mirador(db)?;
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

    /// El nombre de un espacio por su clave (la clave si no se conoce).
    pub fn nombre_espacio(&self, clave: &str) -> String {
        self.espacios.iter().find(|e| e.key == clave).map(|e| e.name.clone()).unwrap_or_else(|| clave.to_string())
    }

    /// Vuelve a leer la lista (recientes o búsqueda) y la papelera, conservando la elegida si sigue.
    pub fn cargar(&mut self, db: &mut Connection, modelo: Option<&Modelo>) -> Result<()> {
        let m = self.mirador(db)?;
        self.ve_todo = m.todo;
        self.espacio_propio = m.espacio.clone();
        self.espacios = crate::proyecto::espacios(db)?.into_iter().filter(|e| m.ve_espacio(&e.key, false)).collect();
        if let Some(f) = self.espacio
            && !m.abarca(&Ambito::Espacio(f))
        {
            self.espacio = None;
        }
        let ambito = self.ambito(db)?;
        let antes = self.elegida().map(|m| m.id.clone());
        self.memorias = if self.consulta.trim().is_empty() {
            memoria::recientes(db, &ambito, None, LIMITE)?
        } else {
            let o = Opciones { ambito, kind: None, tags: vec![], limite: 50 };
            buscar::buscar_sin_apuntar_con(db, &self.consulta, &o, modelo)?.into_iter().map(|r| r.memoria).collect()
        };
        let i = antes.and_then(|id| self.memorias.iter().position(|m| m.id == id)).unwrap_or(0);
        self.lista.select((!self.memorias.is_empty()).then_some(i));
        self.cargar_papelera(db, &m)
    }

    /// La papelera como la ve la web (`GET /api/trash`): lo que se ve desde aquí y, con un filtro de
    /// espacio, solo lo de ese espacio y lo global.
    fn cargar_papelera(&mut self, db: &Connection, m: &Mirador) -> Result<()> {
        let antes = self.elegida_papelera().map(|o| o.olvidada.id.clone());
        let filtro = match self.espacio {
            Some(f) => Some(crate::proyecto::clave_espacio(db, f)?),
            None => None,
        };
        let mut dias = db.prepare("SELECT julianday('now') - julianday(?1)")?;
        let mut v = Vec::new();
        for o in memoria::papelera(db)? {
            if !m.ve(db, &o.id)? {
                continue;
            }
            if let Some(clave) = &filtro {
                let global = o.project.is_empty() && o.space.is_empty();
                if !global && o.space != *clave {
                    continue;
                }
            }
            let d: Option<f64> = dias.query_row([&o.deleted_at], |f| f.get(0))?;
            v.push(EnPapelera { olvidada: o, dias: d.unwrap_or(0.0).max(0.0) });
        }
        self.papelera = v;
        let i = antes.and_then(|id| self.papelera.iter().position(|o| o.olvidada.id == id)).unwrap_or(0);
        self.lista_papelera.select((!self.papelera.is_empty()).then_some(i));
        Ok(())
    }

    /// Devuelve de la papelera la memoria `id` (entero), si se ve desde aquí: lo mismo que
    /// `POST /api/restore` y `omoi restore`. Lo que pase queda en el aviso.
    pub fn restaurar(&mut self, db: &mut Connection, id: &str, modelo: Option<&Modelo>) -> Result<bool> {
        if !self.mirador(db)?.ve(db, id)? {
            self.aviso = fuera();
            return Ok(false);
        }
        let antes = crate::revisar::antes(db)?;
        match memoria::restaurar(db, id, modelo) {
            Ok(r) => {
                crate::revisar::al_dia_de(db, &antes, std::slice::from_ref(&r.id))?;
                self.aviso = tr!("restored: {}", "recuperada: {}", r.title);
                Ok(true)
            }
            Err(e) => {
                self.aviso = format!("{e:#}");
                Ok(false)
            }
        }
    }

    /// Borra del todo de la papelera las memorias `ids` (enteras) que se vean desde aquí; las demás no
    /// se tocan. Devuelve cuántas.
    pub fn borrar_de_papelera(&mut self, db: &mut Connection, ids: &[String]) -> Result<usize> {
        let m = self.mirador(db)?;
        let mut visibles = Vec::new();
        for id in ids {
            if m.ve(db, id)? {
                visibles.push(id.clone());
            }
        }
        if visibles.len() < ids.len() {
            self.aviso = fuera();
        }
        memoria::vaciar_papelera_de(db, &visibles)
    }

    /// Vacía lo que se ve de la papelera. Desde la carpeta del usuario y sin filtro, la papelera entera
    /// (como `omoi trash --empty`); desde un proyecto o con filtro, solo lo que enseña.
    pub fn vaciar_papelera(&mut self, db: &mut Connection) -> Result<usize> {
        if self.mirador(db)?.todo && self.espacio.is_none() {
            return memoria::vaciar_papelera(db);
        }
        let ids: Vec<String> = self.papelera.iter().map(|o| o.olvidada.id.clone()).collect();
        self.borrar_de_papelera(db, &ids)
    }

    /// Una tecla. Devuelve si hay que volver a cargar la lista.
    pub fn tecla(&mut self, k: KeyEvent, db: &mut Connection, modelo: Option<&Modelo>) -> Result<bool> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.salir = true;
            return Ok(false);
        }
        let si = matches!(k.code, KeyCode::Char('y') | KeyCode::Char('s'));
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
                if si && let Some(m) = self.elegida().cloned() {
                    let antes = crate::revisar::antes(db)?;
                    memoria::olvidar(db, &m.id)?;
                    crate::revisar::al_dia_de(db, &antes, std::slice::from_ref(&m.id))?;
                    self.aviso = tr!("forgotten: {} (t: trash)", "olvidada: {} (t: papelera)", m.title);
                    self.vista = Vista::Lista;
                    return Ok(true);
                }
                self.vista = Vista::Lista;
            }
            Vista::PapeleraBorrar => {
                self.vista = Vista::Papelera;
                if si && let Some(o) = self.elegida_papelera().cloned() {
                    if self.borrar_de_papelera(db, std::slice::from_ref(&o.olvidada.id))? > 0 {
                        self.aviso = tr!("deleted for good: {}", "borrada del todo: {}", o.olvidada.title);
                    }
                    return Ok(true);
                }
            }
            Vista::PapeleraVaciar => {
                self.vista = Vista::Papelera;
                if si {
                    let n = self.vaciar_papelera(db)?;
                    self.aviso = tr!("trash emptied: {n} deleted for good", "papelera vaciada: {n} borradas del todo");
                    return Ok(true);
                }
            }
            Vista::Papelera => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') | KeyCode::Left => self.vista = Vista::Lista,
                KeyCode::Down | KeyCode::Char('j') => mover(&mut self.lista_papelera, self.papelera.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => mover(&mut self.lista_papelera, self.papelera.len(), -1),
                KeyCode::PageDown => mover(&mut self.lista_papelera, self.papelera.len(), 15),
                KeyCode::PageUp => mover(&mut self.lista_papelera, self.papelera.len(), -15),
                KeyCode::Enter | KeyCode::Char('r') => {
                    if let Some(o) = self.elegida_papelera().cloned() {
                        return self.restaurar(db, &o.olvidada.id, modelo);
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
                KeyCode::Down | KeyCode::Char('j') => mover(&mut self.lista, self.memorias.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => mover(&mut self.lista, self.memorias.len(), -1),
                KeyCode::PageDown => mover(&mut self.lista, self.memorias.len(), 15),
                KeyCode::PageUp => mover(&mut self.lista, self.memorias.len(), -15),
                KeyCode::Enter | KeyCode::Right => {
                    if self.elegida().is_some() {
                        self.vista = Vista::Entera;
                        self.desplazamiento = 0;
                    }
                }
                KeyCode::Char('h') => {
                    if let Some(m) = self.elegida() {
                        // Como `omoi history`: por el ámbito de la carpeta, que no saca lo de otro cliente.
                        self.historial = memoria::historial_en(db, &self.mirador(db)?.base, &m.id)?;
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
                    self.espacio = None;
                    return Ok(true);
                }
                KeyCode::Char('e') => {
                    // Sin filtro → cada espacio que se ve → los proyectos sin espacio → sin filtro.
                    let filtros = self.filtros(db)?;
                    self.espacio = match self.espacio.and_then(|f| filtros.iter().position(|x| *x == f)) {
                        None => filtros.first().copied(),
                        Some(i) => filtros.get(i + 1).copied(),
                    };
                    if self.espacio.is_none() && filtros.is_empty() {
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

    /// Qué se ve, en pocas palabras, para el título.
    fn texto_ambito(&self) -> String {
        match self.espacio {
            Some(Some(id)) => {
                let nombre = self.espacios.iter().find(|e| e.id == id).map(|e| e.name.clone()).unwrap_or_default();
                tr!("space {} + global", "espacio {} + globales", nombre)
            }
            Some(None) => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None if !self.todos => tr!("this project + global", "este proyecto + globales"),
            None if self.ve_todo => tr!("all projects", "todos los proyectos"),
            None if self.espacio_propio.is_empty() => tr!("projects without a space + global", "proyectos sin espacio + globales"),
            None => tr!("space {} + global", "espacio {} + globales", self.nombre_espacio(&self.espacio_propio)),
        }
    }

    /// Si la lista mezcla espacios (o lo global con un espacio) y conviene decir el de cada una.
    fn mezcla_espacios(&self) -> bool {
        self.todos || self.espacio.is_some()
    }

    /// El espacio de una memoria (o de lo olvidado), corto, para la columna de la lista.
    fn etiqueta(&self, proyecto: &str, espacio: &str, de_espacio: bool) -> String {
        if espacio.is_empty() {
            return if proyecto.is_empty() { tr!("global", "global") } else { "-".to_string() };
        }
        let n = self.nombre_espacio(espacio);
        if de_espacio { format!("◇{n}") } else { n }
    }

    /// Lo que dice dónde vive una memoria: el proyecto con su espacio, el espacio, o global.
    fn donde(&self, proyecto: &str, espacio: &str, de_espacio: bool) -> String {
        if de_espacio {
            return tr!("space {}", "espacio {}", self.nombre_espacio(espacio));
        }
        if proyecto.is_empty() {
            return tr!("global", "global");
        }
        let corto = std::path::Path::new(proyecto).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| proyecto.to_string());
        if espacio.is_empty() {
            tr!("{} · no space", "{} · sin espacio", corto)
        } else {
            tr!("{} · space {}", "{} · espacio {}", corto, self.nombre_espacio(espacio))
        }
    }
}

fn fuera() -> String {
    tr!(
        "that memory is not in this scope: it belongs to another space; open omoi tui in that project, or in the home folder",
        "esa memoria no es de este ámbito: es de otro espacio; abre omoi tui en ese proyecto, o en tu carpeta de usuario"
    )
}

fn mover(lista: &mut ListState, largo: usize, n: i64) {
    if largo == 0 {
        return;
    }
    let i = lista.selected().unwrap_or(0) as i64 + n;
    lista.select(Some(i.clamp(0, largo as i64 - 1) as usize));
}

/// `texto` cortado a `ancho` caracteres y rellenado hasta él.
fn columna(texto: &str, ancho: usize) -> String {
    format!("{texto:<ancho$.ancho$} ")
}

pub fn dibujar(f: &mut Frame, e: &mut Estado) {
    let area = f.area();
    let ayuda = ayuda(e);
    let linea = if e.aviso.is_empty() { ayuda } else { format!("{} · {ayuda}", e.aviso) };
    // La línea de ayuda, en las filas que necesite (hasta 3): en 80 columnas no cabe en una.
    let ancho = area.width.max(1) as usize;
    let pie = (linea.chars().count().div_ceil(ancho) as u16).clamp(1, 3);
    let [arriba, centro, abajo] = Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(pie)]).areas(area);

    let en_papelera = matches!(e.vista, Vista::Papelera | Vista::PapeleraBorrar | Vista::PapeleraVaciar);
    let titulo = if en_papelera {
        let dias = memoria::dias_papelera();
        let guarda = if dias > 0.0 { tr!("kept {} days", "se guarda {} días", dias) } else { tr!("kept until emptied", "se guarda hasta vaciarla") };
        format!(" {} · {} · {} · {guarda} ", tr!("trash", "papelera"), e.texto_ambito(), e.papelera.len())
    } else {
        format!(" {} · {} · {} ", tr!("search", "buscar"), e.texto_ambito(), e.memorias.len())
    };
    let cursor = if e.vista == Vista::Buscando { "▏" } else { "" };
    let caja = Block::default().borders(Borders::ALL).title(titulo).border_style(if e.vista == Vista::Buscando {
        Style::default().bold()
    } else {
        Style::default()
    });
    f.render_widget(Paragraph::new(format!("{}{cursor}", e.consulta)).block(caja), arriba);

    match e.vista {
        Vista::Entera => dibujar_entera(f, e, centro),
        Vista::Historial => dibujar_historial(f, e, centro),
        Vista::Papelera | Vista::PapeleraBorrar | Vista::PapeleraVaciar => dibujar_papelera(f, e, centro),
        _ => {
            let (izq, der) = partir(centro);
            let interior = izq.width.saturating_sub(2) as usize;
            let con_tipo = interior >= 40;
            let con_espacio = e.mezcla_espacios() && interior >= 30;
            let items: Vec<ListItem> = e
                .memorias
                .iter()
                .map(|m| {
                    let marca = if m.pinned { "* " } else { "  " };
                    let mut v = vec![Span::raw(marca)];
                    if con_tipo {
                        v.push(Span::styled(columna(m.kind.as_str(), 10), Style::default().add_modifier(Modifier::DIM)));
                    }
                    if con_espacio {
                        v.push(Span::styled(columna(&e.etiqueta(&m.project, &m.space, m.de_espacio()), 8), Style::default().add_modifier(Modifier::DIM)));
                    }
                    v.push(Span::raw(m.title.clone()));
                    ListItem::new(Line::from(v))
                })
                .collect();
            let lista = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(tr!(" memories ", " memorias ")))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_stateful_widget(lista, izq, &mut e.lista);
            if let Some(der) = der {
                let detalle = e.elegida().map(|m| ficha(e, m)).unwrap_or_default();
                f.render_widget(Paragraph::new(detalle).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), der);
            }
        }
    }

    f.render_widget(Paragraph::new(linea).wrap(Wrap { trim: true }).style(Style::default().add_modifier(Modifier::DIM)), abajo);
}

/// La lista y, si cabe, la ficha: al lado en una terminal ancha, debajo en una estrecha y alta, y
/// ninguna si no queda sitio (Intro la abre entera).
fn partir(area: Rect) -> (Rect, Option<Rect>) {
    if area.width >= ANCHO_DOS_COLUMNAS {
        let [izq, der] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
        (izq, Some(der))
    } else if area.height >= 14 {
        let [arriba, abajo] = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
        (arriba, Some(abajo))
    } else {
        (area, None)
    }
}

fn ayuda(e: &Estado) -> String {
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

/// La memoria como texto: título, datos y contenido.
fn ficha(e: &Estado, m: &Memoria) -> Vec<Line<'static>> {
    let mut v = vec![Line::from(m.title.clone()).bold(), Line::raw("")];
    let donde = e.donde(&m.project, &m.space, m.de_espacio());
    v.push(Line::from(format!("{} · {donde} · {}", m.kind.as_str(), &m.updated_at[..m.updated_at.len().min(10)])).dim());
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

fn dibujar_entera(f: &mut Frame, e: &Estado, area: Rect) {
    let Some(m) = e.elegida() else { return };
    let p = Paragraph::new(ficha(e, m)).wrap(Wrap { trim: false }).scroll((e.desplazamiento, 0)).block(Block::default().borders(Borders::ALL));
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

fn dibujar_papelera(f: &mut Frame, e: &mut Estado, area: Rect) {
    let (izq, der) = partir(area);
    let interior = izq.width.saturating_sub(2) as usize;
    let con_fecha = interior >= 34;
    let con_espacio = interior >= 30;
    let items: Vec<ListItem> = e
        .papelera
        .iter()
        .map(|o| {
            let o = &o.olvidada;
            let mut v = Vec::new();
            if con_fecha {
                v.push(Span::styled(columna(o.deleted_at.get(..10).unwrap_or(&o.deleted_at), 10), Style::default().add_modifier(Modifier::DIM)));
            }
            if con_espacio {
                let de_espacio = o.project.is_empty() && !o.space.is_empty();
                v.push(Span::styled(columna(&e.etiqueta(&o.project, &o.space, de_espacio), 8), Style::default().add_modifier(Modifier::DIM)));
            }
            v.push(Span::raw(o.title.clone()));
            ListItem::new(Line::from(v))
        })
        .collect();
    let vacia = items.is_empty();
    let lista = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(tr!(" trash ", " papelera ")))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(lista, izq, &mut e.lista_papelera);
    let Some(der) = der else { return };
    let detalle: Vec<Line> = match e.elegida_papelera() {
        None if vacia => vec![Line::raw(tr!("The trash is empty.", "La papelera está vacía."))],
        None => Vec::new(),
        Some(p) => {
            let o = &p.olvidada;
            let de_espacio = o.project.is_empty() && !o.space.is_empty();
            let mut v = vec![Line::from(o.title.clone()).bold(), Line::raw("")];
            v.push(Line::from(format!("{} · {}", o.kind.as_str(), e.donde(&o.project, &o.space, de_espacio))).dim());
            if !o.project.is_empty() {
                v.push(Line::from(o.project.clone()).dim());
            }
            v.push(Line::from(tr!("forgotten {} on {}", "olvidada el {} en {}", o.deleted_at.get(..10).unwrap_or(&o.deleted_at), o.by)).dim());
            let total = memoria::dias_papelera();
            if total > 0.0 {
                let quedan = (total - p.dias).max(0.0).ceil() as i64;
                v.push(Line::from(tr!("deleted for good in {quedan} days", "se borra del todo en {quedan} días")).dim());
            }
            v.push(Line::from(format!("id {}", o.id)).dim());
            v
        }
    };
    f.render_widget(Paragraph::new(detalle).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL)), der);
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
                if e.tecla(k, db, modelo)? {
                    e.cargar(db, modelo)?;
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    r
}
