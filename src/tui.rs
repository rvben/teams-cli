//! The keyboard-first chat reader.
//!
//! A calm two-pane ledger: chats on the left, the open conversation on the
//! right, a header that always says where you are and a footer that always
//! says what the keys do. Reads run in the background so the keyboard never
//! waits on the network, results are cached per chat, and every screen stays
//! legible without colour (`NO_COLOR`).
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use jiff::{ToSpan, Zoned, tz::TimeZone};
use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

use crate::backend::TeamsBackend;
use crate::error::AppError;
use crate::graph::Page;
use crate::render::{self, ChatRow, Entry, View};

/// Records requested per read; the most both backends accept.
const PAGE: u16 = 50;
/// How long a selection must rest before its conversation is fetched, so
/// scrolling through the ledger does not fire a read per row.
const DWELL: Duration = Duration::from_millis(250);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Below this width the ledger and the conversation take turns.
const SPLIT_WIDTH: u16 = 80;
const MIN_WIDTH: u16 = 50;
const MIN_HEIGHT: u16 = 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiExit {
    Quit,
    Authenticate,
}

/// Where chats come from: a connected profile or deterministic sample data.
pub enum Source {
    Live {
        backend: Arc<TeamsBackend>,
        profile: String,
    },
    Demo,
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self, AppError> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn require_terminal(hint: &str) -> Result<(), AppError> {
    if io::stdout().is_terminal() && io::stdin().is_terminal() {
        Ok(())
    } else {
        Err(AppError::NonInteractive(format!(
            "`teams tui` needs a terminal; {hint}"
        )))
    }
}

/// Opens the chat reader and returns how the person left it.
pub fn run(source: Source) -> Result<TuiExit, AppError> {
    require_terminal("use `teams tui --demo --snapshot` for an inspectable frame")?;
    let color = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let mut app = App::new(
        source,
        Theme { color },
        Zoned::now().with_time_zone(TimeZone::system()),
    );
    app.start();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        app.tick();
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match app.handle_key(key) {
                Action::None => {}
                Action::Quit => return Ok(TuiExit::Quit),
                Action::Authenticate => return Ok(TuiExit::Authenticate),
            }
        }
    }
}

/// One ANSI-free frame of the demo, for documentation and tests.
pub fn snapshot(width: u16, height: u16) -> Result<String, AppError> {
    let now: Zoned = "2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
        .parse()
        .expect("valid demo clock");
    let mut app = App::new(Source::Demo, Theme { color: false }, now);
    app.start();
    app.open_selected();
    let mut terminal = Terminal::new(TestBackend::new(
        width.max(MIN_WIDTH),
        height.max(MIN_HEIGHT),
    ))
    .expect("test backend is infallible");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("test backend is infallible");
    Ok(buffer_text(terminal.backend().buffer()))
}

fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut output = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        output.push_str(line.trim_end());
        output.push('\n');
    }
    while output.ends_with("\n\n") {
        output.pop();
    }
    output
}

enum Action {
    None,
    Quit,
    Authenticate,
}

enum Outcome {
    Chats {
        more: bool,
        result: Result<Page, AppError>,
    },
    Messages {
        chat: String,
        older: bool,
        result: Result<Page, AppError>,
    },
    Me(Result<Value, AppError>),
}

enum Load {
    Loading,
    Ready,
    Failed { message: String, auth: bool },
}

impl Load {
    fn failed(error: &AppError) -> Self {
        Self::Failed {
            message: error.to_string(),
            auth: matches!(error, AppError::Auth(_)),
        }
    }
}

struct Thread {
    items: Vec<Value>,
    cursor: Option<String>,
    load: Load,
    loading_older: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Focus {
    Chats,
    Thread,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Overlay {
    Help,
    Filter,
}

#[derive(Clone, Copy)]
enum Tone {
    Quiet,
    Success,
    Error,
}

struct Notice {
    tone: Tone,
    text: String,
}

impl Notice {
    fn new(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            tone,
            text: text.into(),
        }
    }
}

struct App {
    theme: Theme,
    source: Source,
    outcomes: (mpsc::Sender<Outcome>, mpsc::Receiver<Outcome>),
    now: Zoned,
    account: Option<String>,
    viewer: Option<String>,
    chats: Vec<Value>,
    rows: Vec<ChatRow>,
    chat_cursor: Option<String>,
    chats_load: Load,
    loading_more: bool,
    filter: String,
    /// Indices into `rows` that pass the filter, in ledger order.
    visible: Vec<usize>,
    list: ListState,
    selected_at: Option<Instant>,
    threads: HashMap<String, Thread>,
    focus: Focus,
    /// Conversation lines hidden below the viewport; 0 follows the newest.
    scroll: usize,
    thread_height: usize,
    overlay: Option<Overlay>,
    notice: Notice,
    frame: usize,
}

impl App {
    fn new(source: Source, theme: Theme, now: Zoned) -> Self {
        Self {
            theme,
            source,
            outcomes: mpsc::channel(),
            now,
            account: None,
            viewer: None,
            chats: Vec::new(),
            rows: Vec::new(),
            chat_cursor: None,
            chats_load: Load::Loading,
            loading_more: false,
            filter: String::new(),
            visible: Vec::new(),
            list: ListState::default(),
            selected_at: None,
            threads: HashMap::new(),
            focus: Focus::Chats,
            scroll: 0,
            thread_height: 10,
            overlay: None,
            notice: Notice::new(Tone::Quiet, "Reading your chats…"),
            frame: 0,
        }
    }

    fn view(&self) -> View {
        View {
            now: self.now.clone(),
            width: 100,
        }
    }

    fn backend(&self) -> Option<Arc<TeamsBackend>> {
        match &self.source {
            Source::Live { backend, .. } => Some(backend.clone()),
            Source::Demo => None,
        }
    }

    fn location(&self) -> String {
        let (profile, kind) = match &self.source {
            Source::Live { backend, profile } => (
                profile.as_str(),
                match backend.as_ref() {
                    TeamsBackend::Graph(_) => "Microsoft Graph",
                    TeamsBackend::Desktop(_) => "Teams desktop cache",
                },
            ),
            Source::Demo => ("demo", "sample data"),
        };
        let mut parts = vec![profile.to_string()];
        if let Some(account) = &self.account {
            parts.push(account.clone());
        }
        parts.push(kind.into());
        if matches!(self.chats_load, Load::Ready) {
            let unread = self.rows.iter().filter(|row| row.unread).count();
            parts.push(format!(
                "{}{} chats",
                self.rows.len(),
                if self.chat_cursor.is_some() { "+" } else { "" }
            ));
            if unread > 0 {
                parts.push(format!("{unread} unread"));
            }
        }
        parts.join("  ·  ")
    }

    fn start(&mut self) {
        if let Some(backend) = self.backend()
            && matches!(backend.as_ref(), TeamsBackend::Graph(_))
        {
            let sender = self.outcomes.0.clone();
            tokio::spawn(async move {
                let _ = sender.send(Outcome::Me(backend.me().await));
            });
        }
        self.load_chats(false);
    }

    fn load_chats(&mut self, more: bool) {
        let cursor = if more {
            if self.loading_more || self.chat_cursor.is_none() {
                return;
            }
            self.loading_more = true;
            self.chat_cursor.clone()
        } else {
            if !matches!(self.chats_load, Load::Ready) {
                self.chats_load = Load::Loading;
            }
            None
        };
        match self.backend() {
            Some(backend) => {
                let sender = self.outcomes.0.clone();
                tokio::spawn(async move {
                    let result = backend.chats(PAGE, cursor.as_deref()).await;
                    let _ = sender.send(Outcome::Chats { more, result });
                });
            }
            None => self.apply(Outcome::Chats {
                more,
                result: Ok(demo_chats(&self.now)),
            }),
        }
    }

    fn load_messages(&mut self, chat: String, older: bool) {
        let cursor = match self.threads.get_mut(&chat) {
            Some(thread) if older => {
                if thread.loading_older || thread.cursor.is_none() {
                    return;
                }
                thread.loading_older = true;
                thread.cursor.clone()
            }
            Some(thread) => {
                if !matches!(thread.load, Load::Ready) {
                    thread.load = Load::Loading;
                }
                None
            }
            None => {
                self.threads.insert(
                    chat.clone(),
                    Thread {
                        items: Vec::new(),
                        cursor: None,
                        load: Load::Loading,
                        loading_older: false,
                    },
                );
                None
            }
        };
        match self.backend() {
            Some(backend) => {
                let sender = self.outcomes.0.clone();
                tokio::spawn(async move {
                    let result = backend
                        .messages(Some(&chat), None, None, PAGE, cursor.as_deref())
                        .await;
                    let _ = sender.send(Outcome::Messages {
                        chat,
                        older,
                        result,
                    });
                });
            }
            None => {
                let result = Ok(demo_messages(&chat, &self.now));
                self.apply(Outcome::Messages {
                    chat,
                    older,
                    result,
                });
            }
        }
    }

    fn apply(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Chats { more, result } => {
                if more {
                    self.loading_more = false;
                }
                match result {
                    Ok(page) => {
                        if page.viewer.is_some() {
                            self.viewer = page.viewer;
                        }
                        let keep = self.selected_row().map(|row| row.id.clone());
                        if more {
                            let known: HashSet<String> =
                                self.rows.iter().map(|row| row.id.clone()).collect();
                            self.chats.extend(page.items.into_iter().filter(|chat| {
                                !chat["id"].as_str().is_some_and(|id| known.contains(id))
                            }));
                        } else {
                            if matches!(self.chats_load, Load::Ready) {
                                self.notice = Notice::new(Tone::Success, "Up to date.");
                            } else {
                                self.notice = Notice::new(
                                    Tone::Quiet,
                                    "Enter opens a chat  ·  / filters  ·  ? shows every key",
                                );
                            }
                            self.chats = page.items;
                        }
                        self.chat_cursor = page.next_cursor.filter(|_| page.truncated);
                        self.chats_load = Load::Ready;
                        self.rebuild(keep.as_deref());
                    }
                    Err(error) if more || matches!(self.chats_load, Load::Ready) => {
                        self.notice = Notice::new(Tone::Error, error.to_string());
                    }
                    Err(error) => {
                        self.notice = Notice::new(Tone::Error, "Could not read your chats.");
                        self.chats_load = Load::failed(&error);
                    }
                }
            }
            Outcome::Messages {
                chat,
                older,
                result,
            } => {
                let Some(thread) = self.threads.get_mut(&chat) else {
                    return;
                };
                if older {
                    thread.loading_older = false;
                }
                match result {
                    Ok(page) => {
                        if older {
                            let known: HashSet<&str> = thread
                                .items
                                .iter()
                                .filter_map(|m| m["id"].as_str())
                                .collect();
                            let fresh: Vec<Value> = page
                                .items
                                .into_iter()
                                .filter(|m| !m["id"].as_str().is_some_and(|id| known.contains(id)))
                                .collect();
                            let added = fresh.len();
                            thread.items.extend(fresh);
                            self.notice = Notice::new(
                                Tone::Success,
                                format!(
                                    "Loaded {added} older message{}.",
                                    if added == 1 { "" } else { "s" }
                                ),
                            );
                        } else {
                            thread.items = page.items;
                        }
                        thread.cursor = page.next_cursor.filter(|_| page.truncated);
                        thread.load = Load::Ready;
                        if self.viewer.is_none() && page.viewer.is_some() {
                            self.viewer = page.viewer;
                            self.rebuild(None);
                        }
                    }
                    Err(error) if older || matches!(thread.load, Load::Ready) => {
                        self.notice = Notice::new(Tone::Error, error.to_string());
                    }
                    Err(error) => thread.load = Load::failed(&error),
                }
            }
            Outcome::Me(Ok(me)) => {
                self.account = me["displayName"].as_str().map(String::from);
                if let Some(id) = me["id"].as_str() {
                    self.viewer = Some(id.into());
                    self.rebuild(None);
                }
            }
            // The header simply stays without a name; chats still load.
            Outcome::Me(Err(_)) => {}
        }
    }

    /// Recomputes rows and the filtered ledger, keeping the selected chat.
    fn rebuild(&mut self, keep: Option<&str>) {
        let keep = keep
            .map(String::from)
            .or_else(|| self.selected_row().map(|row| row.id.clone()));
        let view = self.view();
        self.rows = self
            .chats
            .iter()
            .map(|chat| ChatRow::new(chat, &view, self.viewer.as_deref()))
            .collect();
        let needle = self.filter.to_lowercase();
        self.visible = (0..self.rows.len())
            .filter(|&i| {
                let row = &self.rows[i];
                needle.is_empty()
                    || row.title.to_lowercase().contains(&needle)
                    || row
                        .preview
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(&needle))
            })
            .collect();
        let position = keep
            .and_then(|id| self.visible.iter().position(|&i| self.rows[i].id == id))
            .or_else(|| (!self.visible.is_empty()).then_some(0));
        if position != self.list.selected() {
            self.selected_at = Some(Instant::now());
            self.scroll = 0;
        }
        self.list.select(position);
    }

    fn selected_row(&self) -> Option<&ChatRow> {
        self.list
            .selected()
            .and_then(|i| self.visible.get(i))
            .map(|&i| &self.rows[i])
    }

    fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            return;
        }
        let index = index.min(self.visible.len() - 1);
        if self.list.selected() != Some(index) {
            self.list.select(Some(index));
            self.selected_at = Some(Instant::now());
            self.scroll = 0;
        }
        if index + 3 >= self.visible.len() && self.filter.is_empty() {
            self.load_chats(true);
        }
    }

    fn open_selected(&mut self) {
        self.selected_at = None;
        if let Some(id) = self.selected_row().map(|row| row.id.clone())
            && !self.threads.contains_key(&id)
        {
            self.load_messages(id, false);
        }
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        while let Ok(outcome) = self.outcomes.1.try_recv() {
            self.apply(outcome);
        }
        if self
            .selected_at
            .is_some_and(|at| at.elapsed() >= DWELL || self.focus == Focus::Thread)
        {
            self.open_selected();
        }
    }

    fn busy(&self) -> bool {
        matches!(self.chats_load, Load::Loading)
            || self.loading_more
            || self
                .threads
                .values()
                .any(|t| t.loading_older || matches!(t.load, Load::Loading))
    }

    fn sign_in_needed(&self) -> bool {
        matches!(self.chats_load, Load::Failed { auth: true, .. })
            || self.selected_row().is_some_and(|row| {
                matches!(
                    self.threads.get(&row.id).map(|t| &t.load),
                    Some(Load::Failed { auth: true, .. })
                )
            })
    }

    fn refresh(&mut self) {
        if matches!(self.source, Source::Live { .. }) {
            self.now = Zoned::now().with_time_zone(TimeZone::system());
        }
        self.notice = Notice::new(Tone::Quiet, "Refreshing…");
        self.load_chats(false);
        if let Some(id) = self.selected_row().map(|row| row.id.clone()) {
            self.load_messages(id, false);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        match self.overlay {
            Some(Overlay::Help) => {
                if matches!(
                    key.code,
                    KeyCode::Char('?' | 'q') | KeyCode::Esc | KeyCode::Enter
                ) {
                    self.overlay = None;
                }
                return Action::None;
            }
            Some(Overlay::Filter) => {
                match key.code {
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.overlay = None;
                        self.rebuild(None);
                    }
                    KeyCode::Enter => self.overlay = None,
                    KeyCode::Backspace => {
                        self.filter.pop();
                        self.rebuild(None);
                    }
                    KeyCode::Down => self.select(self.list.selected().map_or(0, |i| i + 1)),
                    KeyCode::Up => self.select(self.list.selected().unwrap_or(0).saturating_sub(1)),
                    KeyCode::Char(c) if !control => {
                        self.filter.push(c);
                        self.rebuild(None);
                    }
                    _ => {}
                }
                return Action::None;
            }
            None => {}
        }
        let half = (self.thread_height / 2).max(1);
        let in_thread = self.focus == Focus::Thread;
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('/') => {
                self.focus = Focus::Chats;
                self.overlay = Some(Overlay::Filter);
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Enter | KeyCode::Char('a') if self.sign_in_needed() => {
                return Action::Authenticate;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if in_thread || self.selected_row().is_none() {
                    Focus::Chats
                } else {
                    Focus::Thread
                };
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.selected_row().is_some() {
                    self.focus = Focus::Thread;
                    self.open_selected();
                }
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                if in_thread {
                    self.focus = Focus::Chats;
                } else if !self.filter.is_empty() && key.code == KeyCode::Esc {
                    self.filter.clear();
                    self.rebuild(None);
                }
            }
            KeyCode::Char('d') if control => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::Char('u') if control => self.scroll += half,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(half),
            KeyCode::PageUp => self.scroll += half,
            KeyCode::Down | KeyCode::Char('j') if in_thread => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            KeyCode::Up | KeyCode::Char('k') if in_thread => self.scroll += 1,
            KeyCode::Down | KeyCode::Char('j') => {
                self.select(self.list.selected().map_or(0, |i| i + 1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select(self.list.selected().unwrap_or(0).saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') if in_thread => self.scroll = usize::MAX,
            KeyCode::End | KeyCode::Char('G') if in_thread => self.scroll = 0,
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            KeyCode::Char('o') => match self.selected_row().map(|row| row.id.clone()) {
                Some(id) if self.threads.get(&id).is_some_and(|t| t.cursor.is_some()) => {
                    self.notice = Notice::new(Tone::Quiet, "Reading older messages…");
                    self.load_messages(id, true);
                }
                Some(_) => self.notice = Notice::new(Tone::Quiet, "No older messages to load."),
                None => {}
            },
            _ => {}
        }
        Action::None
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let theme = self.theme;
        frame.render_widget(Block::default().style(theme.canvas()), area);
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            let message = Paragraph::new(vec![
                Line::styled("NEEDS MORE ROOM", theme.title()),
                Line::styled(
                    format!("Make the terminal at least {MIN_WIDTH}×{MIN_HEIGHT}."),
                    theme.muted(),
                ),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
            frame.render_widget(message, centered(area, area.width, 2));
            return;
        }
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .areas(area);
        self.draw_header(frame, header);
        match &self.chats_load {
            Load::Ready => {
                if body.width >= SPLIT_WIDTH {
                    let ledger = (body.width * 38 / 100).clamp(34, 48);
                    let [left, right] =
                        Layout::horizontal([Constraint::Length(ledger), Constraint::Min(30)])
                            .areas(body);
                    self.draw_ledger(frame, left, true);
                    self.draw_thread(frame, right);
                } else if self.focus == Focus::Thread {
                    self.draw_thread(frame, body);
                } else {
                    self.draw_ledger(frame, body, false);
                }
            }
            Load::Loading => state_panel(
                frame,
                body,
                theme,
                "READING CHATS",
                vec![Line::styled(
                    format!("{}  Asking Teams for your recent chats", self.spinner()),
                    theme.muted(),
                )],
                theme.rule(),
            ),
            Load::Failed { message, auth } => {
                let (title, lines) = failure_copy(theme, message, *auth);
                state_panel(frame, body, theme, title, lines, theme.active_rule());
            }
        }
        self.draw_footer(frame, footer);
        if self.overlay == Some(Overlay::Help) {
            self.draw_help(frame);
        }
    }

    fn spinner(&self) -> &'static str {
        SPINNER[self.frame % SPINNER.len()]
    }

    fn draw_header(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let theme = self.theme;
        let mut title = vec![
            Span::styled(" TEAMS ", theme.brand()),
            Span::styled("  CHATS", theme.title()),
        ];
        if self.busy() {
            let status = format!("{} syncing ", self.spinner());
            let used: usize = title.iter().map(|s| s.content.width()).sum();
            let gap = usize::from(area.width).saturating_sub(used + status.width());
            title.push(Span::raw(" ".repeat(gap)));
            title.push(Span::styled(status, theme.accent()));
        }
        let location = render::truncate(&self.location(), usize::from(area.width));
        let header = Paragraph::new(vec![
            Line::from(title),
            Line::styled(location, theme.muted()),
        ])
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(theme.rule()),
        );
        frame.render_widget(header, area);
    }

    fn draw_ledger(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect, split: bool) {
        let theme = self.theme;
        let active = self.focus == Focus::Chats || !split;
        let block = Block::default()
            .borders(if split { Borders::RIGHT } else { Borders::NONE })
            .border_style(if active {
                theme.active_rule()
            } else {
                theme.rule()
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [heading, list_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        let label = if self.filter.is_empty() {
            " RECENT".to_string()
        } else {
            format!(" MATCHING “{}”", self.filter)
        };
        let count = format!("{} ", self.visible.len());
        let gap = usize::from(heading.width).saturating_sub(label.width() + count.width());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, theme.column_header()),
                Span::styled(" ".repeat(gap), theme.column_header()),
                Span::styled(count, theme.column_header()),
            ])),
            heading,
        );
        if self.visible.is_empty() {
            let text = if self.filter.is_empty() {
                vec![
                    Line::styled("No chats yet", theme.title()),
                    Line::styled("Teams has nothing cached for this profile.", theme.muted()),
                ]
            } else {
                vec![
                    Line::styled("Nothing matches", theme.title()),
                    Line::styled("esc clears the filter", theme.muted()),
                ]
            };
            frame.render_widget(
                Paragraph::new(text)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                centered(list_area, list_area.width, 2),
            );
            return;
        }
        let width = usize::from(list_area.width);
        let selected = self.list.selected();
        let mut items: Vec<ListItem> = self
            .visible
            .iter()
            .enumerate()
            .map(|(position, &i)| {
                chat_item(
                    &self.rows[i],
                    width,
                    selected == Some(position),
                    active,
                    theme,
                )
            })
            .collect();
        if self.chat_cursor.is_some() {
            items.push(ListItem::new(Line::styled(
                if self.loading_more {
                    format!("   {} reading more chats", self.spinner())
                } else {
                    "   more chats load as you reach the end".into()
                },
                theme.muted(),
            )));
        }
        frame.render_stateful_widget(List::new(items), list_area, &mut self.list);
    }

    fn draw_thread(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let theme = self.theme;
        let area = Rect {
            x: area.x + 2,
            width: area.width.saturating_sub(3),
            ..area
        };
        let Some(row) = self.selected_row() else {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("No chat selected", theme.title()),
                    Line::styled("Pick a chat on the left to read it here.", theme.muted()),
                ])
                .alignment(Alignment::Center),
                centered(area, area.width, 2),
            );
            return;
        };
        let [heading, body] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let width = usize::from(heading.width);
        let kind = match (row.kind, row.members) {
            (Some(kind), n) if n > 0 => format!("{kind} chat  ·  {n} people"),
            (Some(kind), _) => format!("{kind} chat"),
            (None, _) => "one-to-one chat".into(),
        };
        let mut meta = vec![Span::styled(kind, theme.muted())];
        if self.scroll > 0 {
            meta.push(Span::styled("  ·  ", theme.muted()));
            meta.push(Span::styled("G", theme.key()));
            meta.push(Span::styled(" newest", theme.muted()));
        }
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(render::truncate(&row.title, width), theme.detail_title()),
                Line::from(meta),
            ])
            .block(Block::default().borders(Borders::BOTTOM).border_style(
                if self.focus == Focus::Thread {
                    theme.active_rule()
                } else {
                    theme.rule()
                },
            )),
            heading,
        );
        let body = Rect {
            y: body.y + 1,
            height: body.height.saturating_sub(1),
            ..body
        };
        let id = row.id.clone();
        self.thread_height = usize::from(body.height);
        let Some(thread) = self.threads.get(&id) else {
            frame.render_widget(
                Paragraph::new(Line::styled("…", theme.muted())).alignment(Alignment::Center),
                centered(body, body.width, 1),
            );
            return;
        };
        match &thread.load {
            Load::Loading => {
                frame.render_widget(
                    Paragraph::new(Line::styled(
                        format!("{}  Reading messages", self.spinner()),
                        theme.muted(),
                    ))
                    .alignment(Alignment::Center),
                    centered(body, body.width, 1),
                );
                return;
            }
            Load::Failed { message, auth } => {
                let (title, lines) = failure_copy(theme, message, *auth);
                let mut text = vec![Line::styled(title, theme.error()), Line::raw("")];
                text.extend(lines);
                frame.render_widget(
                    Paragraph::new(text)
                        .alignment(Alignment::Center)
                        .wrap(Wrap { trim: true }),
                    centered(body, body.width, 6),
                );
                return;
            }
            Load::Ready => {}
        }
        let entries = render::conversation(&thread.items, &self.view(), self.viewer.as_deref());
        let mut lines = Vec::new();
        if thread.loading_older {
            lines.push(
                Line::styled(
                    format!("{}  reading older messages", self.spinner()),
                    theme.muted(),
                )
                .alignment(Alignment::Center),
            );
        } else if thread.cursor.is_some() {
            lines.push(
                Line::from(vec![
                    Span::styled("older messages  ·  ", theme.muted()),
                    Span::styled("o", theme.key()),
                    Span::styled(" load", theme.muted()),
                ])
                .alignment(Alignment::Center),
            );
        }
        if entries.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("No messages in this chat yet.", theme.muted()).centered());
        }
        lines.extend(conversation_lines(&entries, usize::from(body.width), theme));
        let height = usize::from(body.height);
        let overflow = lines.len().saturating_sub(height);
        self.scroll = self.scroll.min(overflow);
        let start = overflow - self.scroll;
        let mut shown: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
        // Short conversations sit at the bottom, next to where new messages arrive.
        let pad = height.saturating_sub(shown.len());
        shown.splice(0..0, std::iter::repeat_n(Line::raw(""), pad));
        frame.render_widget(Paragraph::new(shown), body);
    }

    fn draw_footer(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let theme = self.theme;
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(theme.rule());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [status, keys] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
        if self.overlay == Some(Overlay::Filter) {
            let prompt = Line::from(vec![
                Span::styled("› filter ", theme.key()),
                Span::styled(self.filter.clone(), theme.body()),
            ]);
            frame.render_widget(Paragraph::new(prompt), status);
            let x = status.x + 9 + self.filter.width() as u16;
            frame.set_cursor_position((x.min(status.right().saturating_sub(1)), status.y));
        } else {
            let style = match self.notice.tone {
                Tone::Quiet => theme.muted(),
                Tone::Success => theme.success(),
                Tone::Error => theme.error(),
            };
            frame.render_widget(
                Paragraph::new(Line::styled(
                    render::truncate(&self.notice.text, usize::from(status.width)),
                    style,
                )),
                status,
            );
        }
        frame.render_widget(Paragraph::new(self.hints(usize::from(keys.width))), keys);
    }

    /// Key hints for the current context, dropping the least important ones
    /// first when the terminal is narrow. Lower rank means more important.
    fn hints(&self, width: usize) -> Line<'static> {
        let all: Vec<(&str, &str, u8)> = if self.overlay == Some(Overlay::Filter) {
            vec![("enter", "keep", 0), ("esc", "clear", 0), ("↑↓", "move", 1)]
        } else if self.sign_in_needed() {
            vec![
                ("enter / a", "sign in", 0),
                ("r", "retry", 1),
                ("q", "quit", 0),
            ]
        } else if self.focus == Focus::Thread {
            vec![
                ("↑↓", "scroll", 1),
                ("pgup/pgdn", "page", 2),
                ("o", "older", 1),
                ("esc", "chats", 0),
                ("r", "refresh", 2),
                ("?", "help", 1),
                ("q", "quit", 0),
            ]
        } else {
            vec![
                ("↑↓", "move", 1),
                ("enter", "read", 0),
                ("/", "filter", 1),
                ("tab", "switch pane", 3),
                ("r", "refresh", 2),
                ("?", "help", 1),
                ("q", "quit", 0),
            ]
        };
        let mut chosen = vec![false; all.len()];
        let mut used = 0;
        for rank in 0..=3 {
            for (i, (key, description, r)) in all.iter().enumerate() {
                let need = key.width() + 1 + description.width() + 3;
                if *r == rank && used + need <= width {
                    chosen[i] = true;
                    used += need;
                }
            }
        }
        let mut spans = Vec::new();
        for (i, (key, description, _)) in all.into_iter().enumerate() {
            if chosen[i] {
                if !spans.is_empty() {
                    spans.push(Span::raw("   "));
                }
                spans.push(Span::styled(key.to_string(), self.theme.key()));
                spans.push(Span::styled(format!(" {description}"), self.theme.muted()));
            }
        }
        Line::from(spans)
    }

    fn draw_help(&self, frame: &mut ratatui::Frame<'_>) {
        let theme = self.theme;
        let rows = [
            (
                "↑ / k, ↓ / j",
                "Move through chats, or scroll a conversation",
            ),
            ("enter / → / l", "Read the selected chat"),
            ("esc / ← / h", "Back to the chat list"),
            ("tab", "Switch between the list and the conversation"),
            ("pgup / pgdn", "Scroll the conversation by half a screen"),
            ("g / G", "First / last chat, or oldest / newest message"),
            ("o", "Load older messages in this chat"),
            ("/", "Filter chats by name or last message"),
            ("r", "Refresh chats and the open conversation"),
            ("enter / a", "Sign in when the session needs attention"),
            ("q / ctrl-c", "Return to the shell"),
        ];
        let mut lines = vec![
            Line::styled(
                "Read your Teams chats without leaving the terminal",
                theme.muted(),
            ),
            Line::raw(""),
        ];
        for (key, description) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:<16}"), theme.key()),
                Span::styled(description, theme.body()),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Read-only: nothing here sends, edits or marks messages as read.",
            theme.success(),
        ));
        let height = lines.len() as u16 + 2;
        let area = centered(frame.area(), 70, height);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .title(" KEYBOARD MAP  ·  ? OR ESC TO CLOSE ")
                    .title_style(theme.title())
                    .borders(Borders::ALL)
                    .border_style(theme.active_rule())
                    .padding(Padding::horizontal(1))
                    .style(theme.canvas()),
            ),
            area,
        );
    }
}

fn chat_item(
    row: &ChatRow,
    width: usize,
    selected: bool,
    active: bool,
    theme: Theme,
) -> ListItem<'static> {
    let marker = if selected {
        Span::styled(
            "▌",
            if active {
                theme.active_rule()
            } else {
                theme.muted()
            },
        )
    } else {
        Span::raw(" ")
    };
    let dot = if row.unread {
        Span::styled("● ", theme.unread())
    } else {
        Span::raw("  ")
    };
    let kind = row
        .kind
        .map(|kind| format!(" · {kind}"))
        .unwrap_or_default();
    let time = format!("{} ", row.time);
    let room = width.saturating_sub(4 + time.width() + 1);
    // The kind is a hint; when space is short the name wins.
    let kind = if row.title.width() + kind.width() <= room {
        kind
    } else {
        String::new()
    };
    let title = render::truncate(&row.title, room);
    let gap = width.saturating_sub(4 + title.width() + kind.width() + time.width());
    let title_style = if selected && active {
        theme.accent()
    } else if row.unread {
        theme.title()
    } else {
        theme.body()
    };
    let first = Line::from(vec![
        marker.clone(),
        Span::raw(" "),
        dot,
        Span::styled(title, title_style),
        Span::styled(kind, theme.muted()),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            time,
            if row.unread {
                theme.unread()
            } else {
                theme.muted()
            },
        ),
    ]);
    let preview = row.preview.as_deref().unwrap_or("");
    let second = Line::from(vec![
        marker,
        Span::raw("   "),
        Span::styled(
            render::truncate(preview, width.saturating_sub(5)),
            if row.unread {
                theme.body()
            } else {
                theme.muted()
            },
        ),
    ]);
    let item = ListItem::new(vec![first, second, Line::raw("")]);
    if selected {
        item.style(theme.selection())
    } else {
        item
    }
}

/// Lays a conversation out for a pane: centred day rules, coloured senders,
/// indented and wrapped bodies, quotes dimmed.
fn conversation_lines(entries: &[Entry], width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut after_day = false;
    for entry in entries {
        match entry {
            Entry::Day(day) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                let label = format!(" {day} ");
                let side = width.saturating_sub(label.width()) / 2;
                let rest = width.saturating_sub(label.width() + side);
                lines.push(
                    Line::from(vec![
                        Span::styled("─".repeat(side.min(24)), theme.rule()),
                        Span::styled(label, theme.muted()),
                        Span::styled("─".repeat(rest.min(24)), theme.rule()),
                    ])
                    .centered(),
                );
                after_day = true;
            }
            Entry::Sender { name, time, own } => {
                if !after_day {
                    lines.push(Line::raw(""));
                }
                after_day = false;
                let mut spans = vec![Span::styled(
                    render::truncate(name, width.saturating_sub(8)),
                    theme.sender(name, *own),
                )];
                if let Some(time) = time {
                    spans.push(Span::styled(format!("  {time}"), theme.muted()));
                }
                lines.push(Line::from(spans));
            }
            Entry::Text(text) if text.is_empty() => lines.push(Line::raw("")),
            Entry::Text(text) => {
                let style = if text.starts_with('>') {
                    theme.quote()
                } else {
                    theme.body()
                };
                lines.extend(
                    render::wrap(text, width.saturating_sub(2))
                        .into_iter()
                        .map(|line| Line::from(vec![Span::raw("  "), Span::styled(line, style)])),
                );
            }
        }
    }
    lines
}

fn failure_copy(theme: Theme, message: &str, auth: bool) -> (&'static str, Vec<Line<'static>>) {
    if auth {
        (
            "SIGN-IN REQUIRED",
            vec![
                Line::styled("Your Teams session needs attention.", theme.body()),
                Line::styled(message.to_string(), theme.muted()),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("enter / a", theme.key()),
                    Span::styled("  sign in   ", theme.muted()),
                    Span::styled("q", theme.key()),
                    Span::styled("  quit", theme.muted()),
                ]),
            ],
        )
    } else {
        (
            "COULD NOT READ TEAMS",
            vec![
                Line::styled(message.to_string(), theme.body()),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("r", theme.key()),
                    Span::styled("  try again   ", theme.muted()),
                    Span::styled("q", theme.key()),
                    Span::styled("  quit", theme.muted()),
                ]),
            ],
        )
    }
}

fn state_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    theme: Theme,
    title: &str,
    lines: Vec<Line<'static>>,
    border: Style,
) {
    let width = area.width.saturating_sub(4).min(72);
    let room = usize::from(width.saturating_sub(4)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| line.width().div_ceil(room).max(1))
        .sum();
    let panel = centered(area, width, rows as u16 + 4);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(format!(" {title} "))
                    .title_style(theme.title())
                    .borders(Borders::ALL)
                    .border_style(border)
                    .padding(Padding::uniform(1)),
            ),
        panel,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// A Teams-like indigo palette. Every style collapses to the terminal
/// default when colour is off, so structure carries the meaning.
#[derive(Clone, Copy)]
struct Theme {
    color: bool,
}

impl Theme {
    fn style(self, foreground: Color, background: Color) -> Style {
        if self.color {
            Style::default().fg(foreground).bg(background)
        } else {
            Style::default()
        }
    }

    fn fg(self, foreground: Color) -> Style {
        self.style(foreground, Color::Reset)
    }

    fn canvas(self) -> Style {
        self.style(Color::Rgb(222, 225, 240), Color::Rgb(15, 16, 28))
    }

    fn body(self) -> Style {
        self.fg(Color::Rgb(222, 225, 240))
    }

    fn muted(self) -> Style {
        self.fg(Color::Rgb(128, 134, 168))
    }

    fn quote(self) -> Style {
        self.muted().add_modifier(Modifier::ITALIC)
    }

    fn brand(self) -> Style {
        self.style(Color::Rgb(15, 16, 28), Color::Rgb(123, 131, 235))
            .add_modifier(Modifier::BOLD)
    }

    fn title(self) -> Style {
        self.fg(Color::Rgb(242, 243, 252))
            .add_modifier(Modifier::BOLD)
    }

    fn detail_title(self) -> Style {
        self.fg(Color::Rgb(170, 177, 255))
            .add_modifier(Modifier::BOLD)
    }

    fn accent(self) -> Style {
        self.fg(Color::Rgb(155, 162, 255))
            .add_modifier(Modifier::BOLD)
    }

    fn column_header(self) -> Style {
        self.style(Color::Rgb(150, 156, 190), Color::Rgb(24, 26, 44))
            .add_modifier(Modifier::BOLD)
    }

    fn key(self) -> Style {
        self.fg(Color::Rgb(246, 183, 82))
            .add_modifier(Modifier::BOLD)
    }

    fn rule(self) -> Style {
        self.fg(Color::Rgb(50, 54, 86))
    }

    fn active_rule(self) -> Style {
        self.fg(Color::Rgb(123, 131, 235))
    }

    fn selection(self) -> Style {
        if self.color {
            Style::default().bg(Color::Rgb(30, 33, 60))
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }

    fn unread(self) -> Style {
        self.fg(Color::Rgb(255, 128, 112))
            .add_modifier(Modifier::BOLD)
    }

    fn success(self) -> Style {
        self.fg(Color::Rgb(110, 220, 170))
    }

    fn error(self) -> Style {
        self.fg(Color::Rgb(255, 126, 119))
            .add_modifier(Modifier::BOLD)
    }

    /// The same person always gets the same colour; you get the accent.
    fn sender(self, name: &str, own: bool) -> Style {
        const PALETTE: [Color; 6] = [
            Color::Rgb(122, 200, 255),
            Color::Rgb(240, 160, 220),
            Color::Rgb(130, 220, 160),
            Color::Rgb(250, 200, 110),
            Color::Rgb(190, 160, 255),
            Color::Rgb(110, 215, 210),
        ];
        let color = if own {
            Color::Rgb(155, 162, 255)
        } else {
            let hash = name.bytes().fold(0usize, |h, b| {
                h.wrapping_mul(31).wrapping_add(usize::from(b))
            });
            PALETTE[hash % PALETTE.len()]
        };
        self.fg(color).add_modifier(Modifier::BOLD)
    }
}

fn demo_time(now: &Zoned, minutes_ago: i64) -> String {
    now.checked_sub(minutes_ago.minutes())
        .unwrap_or_else(|_| now.clone())
        .timestamp()
        .to_string()
}

const DEMO_PEOPLE: [(&str, &str); 7] = [
    ("me", "Alex Morgan"),
    ("mina", "Mina Okafor"),
    ("jon", "Jon Bakker"),
    ("ari", "Ari Chen"),
    ("priya", "Priya Raman"),
    ("sam", "Sam Rivera"),
    ("lee", "Lee Novak"),
];

fn demo_person(id: &str) -> Value {
    let name = DEMO_PEOPLE
        .iter()
        .find(|(person, _)| *person == id)
        .map_or(id, |(_, name)| name);
    json!({"id": id, "displayName": name})
}

/// (id, topic, type, members, unread) for each sample chat.
type DemoChat = (
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static [&'static str],
    bool,
);

const DEMO_CHATS: [DemoChat; 6] = [
    (
        "19:launch@thread.v2",
        Some("Launch room"),
        "group",
        &["me", "mina", "jon", "priya"],
        true,
    ),
    (
        "19:ari@unq.gbl.spaces",
        None,
        "oneOnOne",
        &["me", "ari"],
        true,
    ),
    (
        "19:design@thread.v2",
        Some("Design crit"),
        "group",
        &["me", "lee", "ari"],
        false,
    ),
    (
        "19:oncall@thread.v2",
        Some("Platform on-call"),
        "group",
        &["me", "priya", "jon", "sam", "lee"],
        false,
    ),
    (
        "19:weekly@thread.v2",
        Some("Weekly sync"),
        "meeting",
        &["me", "mina", "jon", "ari"],
        false,
    ),
    (
        "19:sam@unq.gbl.spaces",
        None,
        "oneOnOne",
        &["me", "sam"],
        false,
    ),
];

/// (minutes ago, sender, html) per sample chat, newest last.
fn demo_script(chat: &str) -> &'static [(i64, &'static str, &'static str)] {
    match chat {
        "19:launch@thread.v2" => &[
            (
                1460,
                "priya",
                "<p>Staging is frozen for the launch. Ping me before any deploy.</p>",
            ),
            (1452, "me", "<p>Noted, thanks <at>Priya</at>.</p>"),
            (
                43,
                "jon",
                "<p>Docs are ready. One broken link remains in the migration guide.</p>",
            ),
            (
                41,
                "me",
                "<p>I’ll take it. Hold the announcement until I clear the link check.</p>",
            ),
            (
                40,
                "me",
                "<p>Fixed in <code>docs#412</code>, the checker is green.</p>",
            ),
            (
                23,
                "mina",
                "<ul><li>notes reviewed</li><li>rollback tested</li><li>status page drafted</li></ul>",
            ),
            (
                22,
                "mina",
                "<p>The release candidate is green. I pinned the final checklist.</p>",
            ),
        ],
        "19:ari@unq.gbl.spaces" => &[
            (180, "me", "<p>Thanks for the review yesterday!</p>"),
            (
                31,
                "ari",
                "<p>Can you look at the migration guide before lunch? Two screenshots are outdated.</p>",
            ),
        ],
        "19:design@thread.v2" => &[
            (300, "lee", "<p>New onboarding flow is in Figma 🎨</p>"),
            (95, "me", "<p>Love the empty states. Uploaded my notes.</p>"),
        ],
        "19:oncall@thread.v2" => &[
            (
                1600,
                "sam",
                "<blockquote><p>Is the queue backlog back to normal?</p></blockquote><p>Yes, drained overnight.</p>",
            ),
            (
                1500,
                "priya",
                "<p>Paging is quiet. Handing over at 18:00.</p>",
            ),
        ],
        "19:weekly@thread.v2" => &[(
            4400,
            "mina",
            "<p>Agenda for Monday is in the meeting notes.</p>",
        )],
        _ => &[(16000, "sam", "<p>Here are the slides from the offsite.</p>")],
    }
}

fn demo_message(
    chat: &str,
    index: usize,
    now: &Zoned,
    (ago, from, html): (i64, &str, &str),
) -> Value {
    json!({
        "id": format!("{chat}/{index}"),
        "messageType": "message",
        "createdDateTime": demo_time(now, ago),
        "from": {"user": demo_person(from)},
        "body": {"contentType": "html", "content": html},
    })
}

fn demo_chats(now: &Zoned) -> Page {
    let items = DEMO_CHATS
        .iter()
        .map(|(id, topic, kind, members, unread)| {
            let script = demo_script(id);
            let last = script[script.len() - 1];
            let mut preview = demo_message(id, script.len() - 1, now, last);
            preview.as_object_mut().map(|p| p.remove("id"));
            let read = if *unread { last.0 + 5 } else { 0 };
            json!({
                "id": id,
                "topic": topic,
                "chatType": kind,
                "lastUpdatedDateTime": demo_time(now, last.0),
                "viewpoint": {"lastMessageReadDateTime": demo_time(now, read)},
                "members": members.iter().map(|m| {
                    let person = demo_person(m);
                    json!({"userId": person["id"], "displayName": person["displayName"]})
                }).collect::<Vec<_>>(),
                "lastMessagePreview": preview,
            })
        })
        .collect();
    demo_page(items)
}

fn demo_messages(chat: &str, now: &Zoned) -> Page {
    demo_page(
        demo_script(chat)
            .iter()
            .enumerate()
            .rev()
            .map(|(index, line)| demo_message(chat, index, now, *line))
            .collect(),
    )
}

fn demo_page(items: Vec<Value>) -> Page {
    Page {
        items,
        next_cursor: None,
        next_offset: None,
        truncated: false,
        source: None,
        viewer: Some("me".into()),
    }
}

/// Present an actionable connection state inside the same terminal experience.
/// Returning first restores the alternate screen before the caller opens a
/// browser or prints device-code instructions.
pub fn request_authentication(
    profile: &str,
    configured: bool,
    reason: &str,
) -> Result<TuiExit, AppError> {
    require_terminal("run `teams auth login --device-code` for headless sign-in")?;
    let theme = Theme {
        color: std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()),
    };
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        terminal.draw(|frame| draw_connection(frame, theme, profile, configured, reason))?;
        if event::poll(Duration::from_millis(150))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Enter | KeyCode::Char('a') => return Ok(TuiExit::Authenticate),
                KeyCode::Char('q') | KeyCode::Esc => return Ok(TuiExit::Quit),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(TuiExit::Quit);
                }
                _ => {}
            }
        }
    }
}

pub fn connection_snapshot(
    width: u16,
    height: u16,
    profile: &str,
    configured: bool,
    reason: &str,
) -> Result<String, AppError> {
    let mut terminal = Terminal::new(TestBackend::new(
        width.max(MIN_WIDTH),
        height.max(MIN_HEIGHT),
    ))
    .expect("test backend is infallible");
    terminal
        .draw(|frame| draw_connection(frame, Theme { color: false }, profile, configured, reason))
        .expect("test backend is infallible");
    Ok(buffer_text(terminal.backend().buffer()))
}

fn draw_connection(
    frame: &mut ratatui::Frame<'_>,
    theme: Theme,
    profile: &str,
    configured: bool,
    reason: &str,
) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme.canvas()), area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" TEAMS ", theme.brand()),
            Span::styled("  CONNECTION", theme.title()),
        ])),
        Rect { height: 1, ..area },
    );
    let (title, heading, action) = if configured {
        (
            "SIGN-IN REQUIRED",
            "Your Teams session needs attention",
            "Continue with Microsoft sign-in. Your password is never shared with teams-cli.",
        )
    } else {
        (
            "CONNECT MICROSOFT TEAMS",
            "Bring your Teams chats into the terminal",
            "Set up this profile and continue with secure Microsoft sign-in.",
        )
    };
    let lines = vec![
        Line::styled(heading, theme.title()),
        Line::styled(format!("Profile: {profile}"), theme.muted()),
        Line::raw(""),
        Line::styled(reason.to_string(), theme.body()),
        Line::styled(action, theme.muted()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("enter / a", theme.key()),
            Span::styled(
                if configured {
                    "  sign in   "
                } else {
                    "  set up   "
                },
                theme.muted(),
            ),
            Span::styled("q", theme.key()),
            Span::styled(" quit", theme.muted()),
        ]),
    ];
    state_panel(frame, area, theme, title, lines, theme.active_rule());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_app() -> App {
        let now: Zoned = "2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
            .parse()
            .unwrap();
        let mut app = App::new(Source::Demo, Theme { color: false }, now);
        app.start();
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn snapshot_shows_the_ledger_and_the_open_conversation() {
        let frame = snapshot(90, 26).unwrap();
        assert!(frame.contains("TEAMS"));
        assert!(frame.contains("Launch room · group"));
        assert!(frame.contains("Ari Chen"));
        assert!(frame.contains("release candidate is green"));
        assert!(frame.contains("── Today ──"));
        assert!(frame.contains("You  09:34"));
        assert!(frame.contains("q quit"));
    }

    #[test]
    fn narrow_terminals_take_turns_and_tiny_ones_ask_for_room() {
        let frame = snapshot(60, 24).unwrap();
        assert!(frame.contains("Launch room"));
        assert!(!frame.contains("rollback tested"));
        let mut app = demo_app();
        press(&mut app, KeyCode::Enter);
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("rollback tested"));
        assert!(text.contains("esc chats"));
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("NEEDS MORE ROOM"));
    }

    #[test]
    fn filtering_narrows_the_ledger_and_esc_restores_it() {
        let mut app = demo_app();
        assert_eq!(app.visible.len(), DEMO_CHATS.len());
        press(&mut app, KeyCode::Char('/'));
        for c in "on-call".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.selected_row().unwrap().title, "Platform on-call");
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.visible.len(), DEMO_CHATS.len());
        assert_eq!(app.selected_row().unwrap().title, "Platform on-call");
    }

    #[test]
    fn opening_a_chat_loads_it_once_and_scroll_is_bounded() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert!(app.focus == Focus::Thread);
        assert!(app.threads.contains_key("19:ari@unq.gbl.spaces"));
        for _ in 0..50 {
            press(&mut app, KeyCode::Char('k'));
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.scroll, 0, "a short chat cannot scroll");
        press(&mut app, KeyCode::Esc);
        assert!(app.focus == Focus::Chats);
        press(&mut app, KeyCode::Char('?'));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("KEYBOARD MAP"));
        assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::None));
        assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::Quit));
    }

    #[test]
    fn auth_failures_offer_sign_in_from_inside_the_tui() {
        let mut app = demo_app();
        app.chats_load = Load::failed(&AppError::Auth("session expired".into()));
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("SIGN-IN REQUIRED"));
        assert!(text.contains("enter / a"));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Action::Authenticate
        ));
    }

    #[test]
    fn connection_snapshot_has_a_clear_recovery_action() {
        let frame =
            connection_snapshot(80, 22, "work", true, "Your saved session has expired.").unwrap();
        assert!(frame.contains("SIGN-IN REQUIRED"));
        assert!(frame.contains("Profile: work"));
        assert!(frame.contains("enter / a"));
        assert!(frame.contains("sign in"));
        assert!(frame.contains("q quit"));
    }
}
