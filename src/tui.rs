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

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::backend::TeamsBackend;
use crate::config;
use crate::desktop::MAX_BODY;
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
/// How often a live session re-reads the open chat and the chat list. Desktop reads come
/// from the local cache, so they can be frequent; Graph is kept within its rate limits.
const DESKTOP_POLL: (Duration, Duration) = (Duration::from_secs(3), Duration::from_secs(15));
const GRAPH_POLL: (Duration, Duration) = (Duration::from_secs(15), Duration::from_secs(60));
/// Rows the composer grows to before its draft scrolls.
const COMPOSE_ROWS: usize = 5;
/// Lines one ledger entry takes: title, preview and a blank separator.
const CHAT_ROW_HEIGHT: u16 = 3;
/// Conversation lines one mouse-wheel notch scrolls.
const WHEEL_LINES: usize = 3;

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
        /// Whether the profile allowed writes at launch; checked again before every send.
        can_send: bool,
    },
    Demo,
}

/// Raw mode and the alternate screen for as long as it lives; with `mouse`, the
/// terminal also reports wheel and clicks instead of turning the wheel into arrow keys.
struct TerminalGuard {
    mouse: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self, AppError> {
        enable_raw_mode()?;
        let entered = if mouse {
            execute!(
                io::stdout(),
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste
            )
        } else {
            execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)
        };
        if let Err(error) = entered {
            let _ = execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { mouse })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
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
    run_with_watch(source, None, &mut InboxState::default())
}

/// Drafts stay in memory while the CLI steps out of the inbox to reauthenticate.
#[derive(Default)]
pub struct InboxState {
    drafts: HashMap<String, String>,
    pending_sends: HashMap<String, String>,
}

pub fn run_with_watch(
    source: Source,
    mut watch: Option<crate::desktop::ConnectionWatch>,
    saved: &mut InboxState,
) -> Result<TuiExit, AppError> {
    require_terminal("use `teams tui --demo --snapshot` for an inspectable frame")?;
    let color = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let mut app = App::new(
        source,
        Theme { color },
        Zoned::now().with_time_zone(TimeZone::system()),
    );
    app.drafts = std::mem::take(&mut saved.drafts);
    app.pending_sends = std::mem::take(&mut saved.pending_sends);
    app.start();
    let _guard = TerminalGuard::enter(true)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        app.tick();
        if let Some(result) = watch
            .as_ref()
            .and_then(crate::desktop::ConnectionWatch::result)
        {
            if let Err(error) = result {
                app.startup_failed(&error);
            }
            watch = None;
        }
        terminal.draw(|frame| app.draw(frame))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Paste(text) => {
                app.paste(&text);
                Action::None
            }
            Event::Mouse(mouse) => {
                app.handle_mouse(mouse);
                Action::None
            }
            _ => Action::None,
        };
        match action {
            Action::None => {}
            Action::Quit => {
                app.preserve_drafts(saved);
                return Ok(TuiExit::Quit);
            }
            Action::Authenticate => {
                app.preserve_drafts(saved);
                return Ok(TuiExit::Authenticate);
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
    DebuggingRequired,
    Restarted(Result<(), AppError>),
    StrayFlags(Vec<String>),
    FlagsCleared(Result<(), AppError>),
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
    Delivery {
        chat: String,
        id: String,
        result: Result<Value, AppError>,
    },
    Sent {
        chat: String,
        result: Result<Value, AppError>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DebuggingRestart {
    Offered,
    Confirming,
    Running,
}

/// Cleanup of user-wide WebView2 debugging flags that let other apps hold the port.
#[derive(Clone, PartialEq, Eq)]
struct FlagCleanup {
    stage: Cleanup,
    /// Applications that served the port, to quit and reopen afterwards.
    apps: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cleanup {
    Offered,
    Confirming,
    Running,
    Done,
}

impl FlagCleanup {
    fn apps(&self) -> String {
        if self.apps.is_empty() {
            "the apps that use WebView2".into()
        } else {
            self.apps.join(", ")
        }
    }
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
    /// A newest-page read is in flight, so polls do not stack up.
    fetching: bool,
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Editing,
    Sending,
    Unconfirmed,
    Checking,
    ConfirmResend,
}

/// A draft for one chat; `cursor` is a byte offset into `text`.
struct Compose {
    chat: String,
    text: String,
    cursor: usize,
    stage: Stage,
}

impl Compose {
    fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }
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
    startup_auth_error: Option<String>,
    sending_paused: bool,
    debugging_restart: Option<DebuggingRestart>,
    flag_cleanup: Option<FlagCleanup>,
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
    /// Conversation lines above the viewport when scrolled to the newest.
    thread_overflow: usize,
    /// Where the last frame drew the ledger rows and the conversation pane, for the mouse.
    ledger_area: Option<Rect>,
    thread_area: Option<Rect>,
    overlay: Option<Overlay>,
    notice: Notice,
    frame: usize,
    compose: Option<Compose>,
    /// Closed, unsent drafts per chat; kept in memory only.
    drafts: HashMap<String, String>,
    /// Correlation IDs for possibly-sent drafts; kept across closing the composer.
    pending_sends: HashMap<String, String>,
    /// Set by `r` so its result is announced; quiet reloads say nothing.
    refreshing: bool,
    fetching_chats: bool,
    /// When a newest-page read last succeeded, and whether a refresh has failed since.
    synced_at: Option<Zoned>,
    stale: bool,
    thread_polled: Instant,
    chats_polled: Instant,
}

impl App {
    fn preserve_drafts(&mut self, saved: &mut InboxState) {
        if let Some(compose) = self.compose.take() {
            self.drafts.insert(compose.chat, compose.text);
        }
        saved.drafts = std::mem::take(&mut self.drafts);
        saved.pending_sends = std::mem::take(&mut self.pending_sends);
    }

    fn startup_failed(&mut self, error: &AppError) {
        self.sending_paused |= self.can_send();
        if let Source::Live { can_send, .. } = &mut self.source {
            *can_send = false;
        }
        if matches!(error, AppError::Auth(_)) {
            if let Some(compose) = self.compose.take() {
                self.drafts.insert(compose.chat, compose.text);
            }
            self.overlay = None;
            self.debugging_restart = None;
            self.flag_cleanup = None;
            self.startup_auth_error = Some(error.to_string());
            self.chats_load = Load::failed(error);
            self.notice = Notice::new(Tone::Error, error.to_string());
        } else {
            let message = if self.sending_paused {
                format!("{error} Sending is paused; reopen the inbox to check it again.")
            } else {
                error.to_string()
            };
            self.notice = Notice::new(Tone::Error, message);
        }
    }

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
            startup_auth_error: None,
            sending_paused: false,
            debugging_restart: None,
            flag_cleanup: None,
            loading_more: false,
            filter: String::new(),
            visible: Vec::new(),
            list: ListState::default(),
            selected_at: None,
            threads: HashMap::new(),
            focus: Focus::Chats,
            scroll: 0,
            thread_height: 10,
            thread_overflow: 0,
            ledger_area: None,
            thread_area: None,
            overlay: None,
            notice: Notice::new(Tone::Quiet, "Reading your chats…"),
            frame: 0,
            compose: None,
            drafts: HashMap::new(),
            pending_sends: HashMap::new(),
            refreshing: false,
            fetching_chats: false,
            synced_at: None,
            stale: false,
            thread_polled: Instant::now(),
            chats_polled: Instant::now(),
        }
    }

    /// Records the outcome of reading the newest chats or messages for the header.
    fn synced(&mut self, ok: bool) {
        if ok {
            self.synced_at = Some(match self.source {
                Source::Live { .. } => Zoned::now().with_time_zone(TimeZone::system()),
                Source::Demo => self.now.clone(),
            });
            self.stale = false;
        } else {
            self.stale = true;
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

    fn can_send(&self) -> bool {
        match &self.source {
            Source::Live { can_send, .. } => *can_send,
            Source::Demo => true,
        }
    }

    fn profile(&self) -> &str {
        match &self.source {
            Source::Live { profile, .. } => profile,
            Source::Demo => "demo",
        }
    }

    /// The second header line as parts, most useful first; whole parts are dropped
    /// from the end when the terminal is narrow.
    fn context(&self) -> Vec<Vec<Span<'static>>> {
        let theme = self.theme;
        let mut parts = Vec::new();
        if matches!(self.chats_load, Load::Ready) {
            let more = if self.chat_cursor.is_some() { "+" } else { "" };
            parts.push(vec![
                Span::styled(format!("{}{more}", self.rows.len()), theme.figure()),
                Span::styled(" chats", theme.muted()),
            ]);
            let unread = self.rows.iter().filter(|row| row.unread).count();
            if unread > 0 {
                parts.push(vec![
                    Span::styled(unread.to_string(), theme.unread()),
                    Span::styled(" unread", theme.unread_label()),
                ]);
            }
        }
        parts.push(vec![Span::styled(
            match &self.source {
                Source::Live { backend, .. } => match backend.as_ref() {
                    TeamsBackend::Graph(_) => "Microsoft Graph",
                    TeamsBackend::Desktop(_) => "Teams desktop cache",
                },
                Source::Demo => "sample data",
            },
            theme.muted(),
        )]);
        parts
    }

    /// How fresh the chats are, as a status dot: live, syncing, or since when a
    /// refresh keeps failing. Sample data has nothing to be fresh about.
    fn freshness(&self) -> Option<Vec<Span<'static>>> {
        let theme = self.theme;
        let dot = |style: Style, text: String| {
            vec![
                Span::styled("● ", style),
                Span::styled(text, style.remove_modifier(Modifier::BOLD)),
            ]
        };
        if self.busy() {
            return Some(vec![Span::styled(
                format!("{} syncing", self.spinner()),
                theme.accent(),
            )]);
        }
        if let Load::Failed { auth, .. } = self.chats_load {
            let text = if auth {
                "sign-in needed"
            } else {
                "not connected"
            };
            return Some(dot(theme.error(), text.into()));
        }
        if matches!(self.source, Source::Demo) {
            return None;
        }
        let at = self.synced_at.as_ref()?.strftime("%H:%M");
        Some(if self.stale {
            dot(theme.error(), format!("stale since {at}"))
        } else {
            dot(theme.success(), "live".into())
        })
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
        self.debugging_restart = None;
        self.flag_cleanup = None;
        let initial = !more && !matches!(self.chats_load, Load::Ready);
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
            self.fetching_chats = true;
            None
        };
        match self.backend() {
            Some(backend) => {
                let sender = self.outcomes.0.clone();
                tokio::spawn(async move {
                    if initial
                        && let TeamsBackend::Desktop(client) = backend.as_ref()
                        && client.needs_debugging_restart().await
                    {
                        let _ = sender.send(Outcome::DebuggingRequired);
                        return;
                    }
                    let result = backend.chats(PAGE, cursor.as_deref()).await;
                    if initial
                        && result.is_err()
                        && let TeamsBackend::Desktop(client) = backend.as_ref()
                        && let Some(apps) = client.stray_debugging_flags().await
                    {
                        let _ = sender.send(Outcome::StrayFlags(apps));
                        return;
                    }
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
                thread.fetching = true;
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
                        fetching: true,
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
            Outcome::DebuggingRequired => {
                self.fetching_chats = false;
                self.debugging_restart = Some(DebuggingRestart::Offered);
                self.notice = Notice::new(Tone::Quiet, "Teams needs debugging enabled to connect");
            }
            Outcome::StrayFlags(apps) => {
                self.fetching_chats = false;
                self.flag_cleanup = Some(FlagCleanup {
                    stage: Cleanup::Offered,
                    apps,
                });
                self.notice =
                    Notice::new(Tone::Quiet, "Another app holds the Teams debugging port");
            }
            Outcome::FlagsCleared(result) => match result {
                Ok(()) => {
                    if let Some(cleanup) = &mut self.flag_cleanup {
                        cleanup.stage = Cleanup::Done;
                    }
                    self.notice = Notice::new(Tone::Quiet, "Debugging flags removed");
                }
                Err(error) => {
                    self.flag_cleanup = None;
                    self.chats_load = Load::failed(&error);
                    self.notice =
                        Notice::new(Tone::Error, "Flags were not removed; press r to retry");
                }
            },
            Outcome::Restarted(result) => {
                self.debugging_restart = None;
                match result {
                    Ok(()) => self.load_chats(false),
                    Err(error) => {
                        self.chats_load = Load::failed(&error);
                        self.notice =
                            Notice::new(Tone::Error, "Restart did not connect; press r to retry");
                    }
                }
            }
            Outcome::Chats { more, result } => {
                if more {
                    self.loading_more = false;
                } else {
                    self.fetching_chats = false;
                }
                match result {
                    Ok(page) => {
                        if page.viewer.is_some() {
                            self.viewer = page.viewer;
                        }
                        let keep = self.selected_row().map(|row| row.id.clone());
                        let cursor = page.next_cursor.filter(|_| page.truncated);
                        if more {
                            let known: HashSet<String> =
                                self.rows.iter().map(|row| row.id.clone()).collect();
                            self.chats.extend(page.items.into_iter().filter(|chat| {
                                !chat["id"].as_str().is_some_and(|id| known.contains(id))
                            }));
                            self.chat_cursor = cursor;
                        } else {
                            if !matches!(self.chats_load, Load::Ready) {
                                self.notice = Notice::new(
                                    Tone::Quiet,
                                    "Enter opens a chat  ·  / filters  ·  ? shows every key",
                                );
                            } else if std::mem::take(&mut self.refreshing) {
                                self.notice = Notice::new(Tone::Success, "Up to date.");
                            }
                            let old = std::mem::take(&mut self.chats);
                            (self.chats, self.chat_cursor) =
                                merge_newest(page.items, cursor, old, self.chat_cursor.take());
                        }
                        self.chats_load = Load::Ready;
                        self.rebuild(keep.as_deref());
                        if !more {
                            self.synced(true);
                        }
                    }
                    Err(error) if more || matches!(self.chats_load, Load::Ready) => {
                        self.refreshing = false;
                        self.notice = Notice::new(Tone::Error, error.to_string());
                        if !more {
                            self.synced(false);
                        }
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
                let opened = self.selected_row().is_some_and(|row| row.id == chat);
                let Some(thread) = self.threads.get_mut(&chat) else {
                    return;
                };
                if older {
                    thread.loading_older = false;
                } else {
                    thread.fetching = false;
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
                            thread.cursor = page.next_cursor.filter(|_| page.truncated);
                        } else {
                            let cursor = page.next_cursor.filter(|_| page.truncated);
                            let old = std::mem::take(&mut thread.items);
                            (thread.items, thread.cursor) =
                                merge_newest(page.items, cursor, old, thread.cursor.take());
                        }
                        thread.load = Load::Ready;
                        if self.viewer.is_none() && page.viewer.is_some() {
                            self.viewer = page.viewer;
                            self.rebuild(None);
                        }
                        if opened && !older {
                            self.prefetch_neighbours();
                            self.synced(true);
                        }
                    }
                    Err(error) if older || matches!(thread.load, Load::Ready) => {
                        self.notice = Notice::new(Tone::Error, error.to_string());
                        if opened && !older {
                            self.synced(false);
                        }
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
            Outcome::Delivery { chat, id, result } => {
                if self.pending_sends.get(&chat) != Some(&id) {
                    return;
                }
                match result {
                    Ok(value)
                        if value["found"] == true
                            && matches!(
                                value["delivery_state"].as_str(),
                                Some("delivered" | "queued")
                            ) =>
                    {
                        self.pending_sends.remove(&chat);
                        self.drafts.remove(&chat);
                        if self.compose.as_ref().is_some_and(|c| c.chat == chat) {
                            self.compose = None;
                        }
                        self.notice = Notice::new(
                            Tone::Success,
                            if value["delivery_state"] == "delivered" {
                                "Delivered; matched the original client message ID."
                            } else {
                                "Found queued in Teams; delivery is not confirmed."
                            },
                        );
                    }
                    result => {
                        if let Some(compose) = self.compose.as_mut().filter(|c| c.chat == chat) {
                            compose.stage = Stage::Unconfirmed;
                        }
                        self.notice = Notice::new(Tone::Error, match result {
                            Err(error) => error.to_string(),
                            Ok(value) if value["found"] == true => "Found the original message in Teams' cache; delivery is still unconfirmed.".into(),
                            Ok(_) => "Delivery remains uncertain. Checked the newest 50 cached messages; absence does not prove failure.".into(),
                        });
                    }
                }
            }
            Outcome::Sent { chat, result } => {
                match result {
                    Ok(value) => {
                        let state = crate::delivery::State::from_message(&value);
                        if value["source"] == "desktop"
                            && matches!(
                                state,
                                crate::delivery::State::Failed | crate::delivery::State::Unknown
                            )
                        {
                            if let Some(compose) = self.compose.as_mut().filter(|c| c.chat == chat)
                            {
                                compose.stage = Stage::Unconfirmed;
                            }
                            self.notice = Notice::new(Tone::Error, state.notice());
                        } else {
                            if self.compose.as_ref().is_some_and(|c| c.chat == chat) {
                                self.compose = None;
                            }
                            self.drafts.remove(&chat);
                            self.pending_sends.remove(&chat);
                            self.notice = Notice::new(
                                Tone::Success,
                                if value["source"] == "desktop" {
                                    state.notice()
                                } else {
                                    "Sent."
                                },
                            );
                        }
                    }
                    Err(error) => {
                        let uncertain = matches!(
                            error,
                            AppError::Unconfirmed(_) | AppError::UnconfirmedSend { .. }
                        );
                        if let Some(compose) = self.compose.as_mut().filter(|c| c.chat == chat) {
                            compose.stage = if uncertain {
                                Stage::Unconfirmed
                            } else {
                                Stage::Editing
                            };
                        }
                        if !uncertain {
                            self.pending_sends.remove(&chat);
                        }
                        self.notice = Notice::new(Tone::Error, error.to_string());
                    }
                }
                // Even a failed send may have reached Teams, so show what the chat holds now.
                if self.backend().is_some() {
                    self.load_chats(false);
                    self.load_messages(chat, false);
                }
            }
        }
    }

    /// Recomputes rows and the filtered ledger, keeping the selected chat.
    fn rebuild(&mut self, keep: Option<&str>) {
        let before = self.selected_row().map(|row| row.id.clone());
        let keep = keep.map(String::from).or_else(|| before.clone());
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
        self.list.select(position);
        // A reload can move the open chat in the ledger; only a different chat starts afresh.
        if self.selected_row().map(|row| row.id.clone()) != before {
            self.selected_at = Some(Instant::now());
            self.scroll = 0;
        }
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
        // A chat that failed while prefetched gets one fresh attempt when opened.
        if let Some(id) = self.selected_row().map(|row| row.id.clone())
            && self
                .threads
                .get(&id)
                .is_none_or(|thread| matches!(thread.load, Load::Failed { .. }))
        {
            self.load_messages(id, false);
        }
    }

    /// Loads the chats either side of the open one, so stepping through the ledger
    /// shows them without waiting.
    fn prefetch_neighbours(&mut self) {
        let Some(index) = self.list.selected() else {
            return;
        };
        let neighbours: Vec<String> = [index.checked_sub(1), index.checked_add(1)]
            .into_iter()
            .flatten()
            .filter_map(|i| self.visible.get(i))
            .map(|&i| self.rows[i].id.clone())
            .filter(|id| !self.threads.contains_key(id))
            .collect();
        for id in neighbours {
            self.load_messages(id, false);
        }
    }

    fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        while let Ok(outcome) = self.outcomes.1.try_recv() {
            self.apply(outcome);
        }
        if let Some(message) = &self.startup_auth_error {
            self.chats_load = Load::Failed {
                message: message.clone(),
                auth: true,
            };
            self.notice = Notice::new(
                Tone::Error,
                "Sign-in is needed. Reply drafts are kept while you reconnect.",
            );
            return;
        }
        if self
            .selected_at
            .is_some_and(|at| at.elapsed() >= DWELL || self.focus == Focus::Thread)
        {
            self.open_selected();
        }
        if let Some(backend) = self.backend() {
            let (thread_every, chats_every) = match backend.as_ref() {
                TeamsBackend::Desktop(_) => DESKTOP_POLL,
                TeamsBackend::Graph(_) => GRAPH_POLL,
            };
            if self.chats_polled.elapsed() >= chats_every
                && matches!(self.chats_load, Load::Ready)
                && !self.loading_more
                && !self.fetching_chats
            {
                self.chats_polled = Instant::now();
                self.now = Zoned::now().with_time_zone(TimeZone::system());
                self.load_chats(false);
            }
            if self.thread_polled.elapsed() >= thread_every {
                self.thread_polled = Instant::now();
                if let Some(id) = self.selected_row().map(|row| row.id.clone())
                    && self.threads.get(&id).is_some_and(|t| {
                        matches!(t.load, Load::Ready) && !t.fetching && !t.loading_older
                    })
                {
                    self.load_messages(id, false);
                }
            }
        }
    }

    /// Background prefetches stay quiet; only the open chat shows activity.
    fn busy(&self) -> bool {
        matches!(self.chats_load, Load::Loading)
            || self.loading_more
            || self.selected_row().is_some_and(|row| {
                self.threads
                    .get(&row.id)
                    .is_some_and(|t| t.loading_older || matches!(t.load, Load::Loading))
            })
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
        self.notice = Notice::new(Tone::Quiet, "Refreshing…");
        self.refreshing = true;
        self.reload();
    }

    /// Reloads the ledger and the open chat, keeping older pages, selection and scroll.
    fn reload(&mut self) {
        self.thread_polled = Instant::now();
        self.chats_polled = Instant::now();
        if matches!(self.source, Source::Live { .. }) {
            self.now = Zoned::now().with_time_zone(TimeZone::system());
        }
        self.load_chats(false);
        if let Some(id) = self.selected_row().map(|row| row.id.clone())
            && self.threads.get(&id).is_none_or(|t| !t.loading_older)
        {
            self.load_messages(id, false);
        }
    }

    fn open_compose(&mut self) {
        let Some(chat) = self.selected_row().map(|row| row.id.clone()) else {
            return;
        };
        if !self.can_send() {
            self.notice = Notice::new(
                Tone::Error,
                if self.sending_paused {
                    "Sending is paused after a startup check failed. Reopen the inbox to check the connection again; your profile's send permission is unchanged."
                } else {
                    "This profile is read-only; sending needs a profile created with `teams init --backend desktop --allow-send`."
                },
            );
            return;
        }
        let text = self.drafts.remove(&chat).unwrap_or_default();
        self.focus = Focus::Thread;
        let stage = if self.pending_sends.contains_key(&chat) {
            Stage::Unconfirmed
        } else {
            Stage::Editing
        };
        self.compose = Some(Compose {
            chat,
            cursor: text.len(),
            text,
            stage,
        });
    }

    /// Every key but ctrl-c belongs to the composer while it is open.
    fn compose_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        match compose.stage {
            Stage::Sending | Stage::Checking => {}
            Stage::ConfirmResend => {
                if key.code == KeyCode::Char('y') {
                    self.send();
                } else {
                    compose.stage = Stage::Unconfirmed;
                    self.notice = Notice::new(
                        Tone::Quiet,
                        "Resend cancelled; delivery is still uncertain.",
                    );
                }
            }
            Stage::Unconfirmed => match key.code {
                KeyCode::Char('r') => self.check_delivery(),
                KeyCode::Char('s') => {
                    compose.stage = Stage::ConfirmResend;
                    self.notice = Notice::new(
                        Tone::Error,
                        "The original may still arrive. Send a second copy? Press y to resend; any other key cancels.",
                    );
                }
                KeyCode::Esc => {
                    let compose = self.compose.take().expect("composer is open");
                    self.drafts.insert(compose.chat, compose.text);
                    self.notice = Notice::new(
                        Tone::Quiet,
                        "Possibly-sent draft kept; c reopens delivery checks.",
                    );
                }
                _ => {
                    self.notice = Notice::new(
                        Tone::Error,
                        "Possibly sent: r checks delivery; s explicitly resends; esc keeps the draft.",
                    );
                }
            },
            Stage::Editing => match key.code {
                KeyCode::Esc => {
                    let compose = self.compose.take().expect("composer is open");
                    if !compose.text.trim().is_empty() {
                        self.drafts.insert(compose.chat, compose.text);
                        self.notice = Notice::new(Tone::Quiet, "Draft kept; c continues it.");
                    }
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => compose.insert('\n'),
                KeyCode::Char('j') if control => compose.insert('\n'),
                KeyCode::Enter => {
                    let length = compose.text.chars().count();
                    if compose.text.trim().is_empty() {
                        self.notice = Notice::new(Tone::Quiet, "Write something first.");
                    } else if length > MAX_BODY {
                        self.notice = Notice::new(
                            Tone::Error,
                            format!("Too long to send: {length} of at most {MAX_BODY} characters."),
                        );
                    } else {
                        self.send();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(c) = compose.text[..compose.cursor].chars().next_back() {
                        compose.cursor -= c.len_utf8();
                        compose.text.remove(compose.cursor);
                    }
                }
                KeyCode::Delete if compose.cursor < compose.text.len() => {
                    compose.text.remove(compose.cursor);
                }
                KeyCode::Left => {
                    if let Some(c) = compose.text[..compose.cursor].chars().next_back() {
                        compose.cursor -= c.len_utf8();
                    }
                }
                KeyCode::Right => {
                    if let Some(c) = compose.text[compose.cursor..].chars().next() {
                        compose.cursor += c.len_utf8();
                    }
                }
                KeyCode::Home => compose.cursor = 0,
                KeyCode::End => compose.cursor = compose.text.len(),
                KeyCode::Char(c) if !control && c != '\0' => compose.insert(c),
                _ => {}
            },
        }
    }

    /// Bracketed paste is text, never a sequence of commands or send keystrokes.
    fn paste(&mut self, text: &str) {
        if let Some(compose) = self.compose.as_mut().filter(|c| c.stage == Stage::Editing) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            for c in text
                .chars()
                .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            {
                compose.insert(c);
            }
        }
    }

    fn send(&mut self) {
        if !self.can_send() {
            self.notice = Notice::new(
                Tone::Error,
                "Sending is paused; reconnect Teams before sending this draft",
            );
            return;
        }
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        compose.stage = Stage::Sending;
        let (chat, text) = (compose.chat.clone(), compose.text.clone());
        let id = crate::delivery::client_message_id();
        self.pending_sends.insert(chat.clone(), id.clone());
        self.notice = Notice::new(Tone::Quiet, "Sending…");
        match &self.source {
            Source::Live {
                backend, profile, ..
            } => {
                let (backend, profile) = (backend.clone(), profile.clone());
                let sender = self.outcomes.0.clone();
                tokio::spawn(async move {
                    let result = async {
                        config::load(Some(&profile))?.1.require_writable()?;
                        backend.send_with_id(&chat, &text, &id).await
                    }
                    .await;
                    let _ = sender.send(Outcome::Sent { chat, result });
                });
            }
            Source::Demo => {
                let message = json!({
                    "id": format!("{chat}/sent{}", self.frame),
                    "messageType": "message",
                    "createdDateTime": self.now.timestamp().to_string(),
                    "from": {"user": demo_person("me")},
                    "body": {"contentType": "text", "content": text},
                });
                if let Some(thread) = self.threads.get_mut(&chat) {
                    thread.items.insert(0, message.clone());
                }
                self.apply(Outcome::Sent {
                    chat,
                    result: Ok(message),
                });
            }
        }
    }

    fn check_delivery(&mut self) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let chat = compose.chat.clone();
        let Some(id) = self.pending_sends.get(&chat).cloned() else {
            self.notice = Notice::new(
                Tone::Error,
                "No correlation ID is available; inspect the chat before explicitly resending.",
            );
            return;
        };
        compose.stage = Stage::Checking;
        self.notice = Notice::new(Tone::Quiet, "Checking the original client message ID…");
        if let Some(backend) = self.backend() {
            let sender = self.outcomes.0.clone();
            tokio::spawn(async move {
                let result = backend.delivery(&chat, &id).await;
                let _ = sender.send(Outcome::Delivery { chat, id, result });
            });
        } else {
            let messages = self
                .threads
                .get(&chat)
                .map(|t| t.items.as_slice())
                .unwrap_or_default();
            let result = Ok(crate::delivery::reconcile(&chat, &id, messages));
            self.apply(Outcome::Delivery { chat, id, result });
        }
    }

    fn restart_teams(&mut self) {
        let Some(backend) = self.backend() else {
            return;
        };
        self.debugging_restart = Some(DebuggingRestart::Running);
        self.notice = Notice::new(
            Tone::Quiet,
            "Restarting Teams and waiting for the connection…",
        );
        let sender = self.outcomes.0.clone();
        tokio::spawn(async move {
            if let TeamsBackend::Desktop(client) = backend.as_ref() {
                let _ = sender.send(Outcome::Restarted(client.restart_with_debugging().await));
            }
        });
    }

    fn clear_flags(&mut self) {
        let Some(backend) = self.backend() else {
            return;
        };
        if let Some(cleanup) = &mut self.flag_cleanup {
            cleanup.stage = Cleanup::Running;
        }
        self.notice = Notice::new(Tone::Quiet, "Removing debugging flags…");
        let sender = self.outcomes.0.clone();
        tokio::spawn(async move {
            if let TeamsBackend::Desktop(client) = backend.as_ref() {
                let _ = sender.send(Outcome::FlagsCleared(client.clear_debugging_flags().await));
            }
        });
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.startup_auth_error.is_some() {
            return match key.code {
                KeyCode::Enter | KeyCode::Char('a') => Action::Authenticate,
                KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                _ => Action::None,
            };
        }
        if let Some(restart) = self.debugging_restart {
            match (restart, key.code) {
                (_, KeyCode::Char('q')) => return Action::Quit,
                (DebuggingRestart::Offered, KeyCode::Enter) => {
                    self.debugging_restart = Some(DebuggingRestart::Confirming);
                }
                (DebuggingRestart::Confirming, KeyCode::Char('y')) => self.restart_teams(),
                (DebuggingRestart::Confirming, _) => {
                    self.debugging_restart = Some(DebuggingRestart::Offered);
                }
                (DebuggingRestart::Offered, KeyCode::Char('r')) => self.load_chats(false),
                (DebuggingRestart::Offered, KeyCode::Esc) => return Action::Quit,
                _ => {}
            }
            return Action::None;
        }
        if let Some(stage) = self.flag_cleanup.as_ref().map(|cleanup| cleanup.stage) {
            match (stage, key.code) {
                (_, KeyCode::Char('q')) => return Action::Quit,
                (Cleanup::Offered, KeyCode::Enter) => {
                    if let Some(cleanup) = &mut self.flag_cleanup {
                        cleanup.stage = Cleanup::Confirming;
                    }
                }
                (Cleanup::Confirming, KeyCode::Char('y')) => self.clear_flags(),
                (Cleanup::Confirming, _) => {
                    if let Some(cleanup) = &mut self.flag_cleanup {
                        cleanup.stage = Cleanup::Offered;
                    }
                }
                (Cleanup::Offered | Cleanup::Done, KeyCode::Char('r')) => self.load_chats(false),
                (Cleanup::Offered | Cleanup::Done, KeyCode::Esc) => return Action::Quit,
                _ => {}
            }
            return Action::None;
        }
        if self.compose.is_some() {
            self.compose_key(key);
            return Action::None;
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
            KeyCode::Char('c') => self.open_compose(),
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
            KeyCode::Char('o') => self.load_older(true),
            _ => {}
        }
        Action::None
    }

    /// Reads the page before the oldest loaded message; `announce` also says when
    /// there is none, which the wheel stays quiet about.
    fn load_older(&mut self, announce: bool) {
        let Some(id) = self.selected_row().map(|row| row.id.clone()) else {
            return;
        };
        match self.threads.get(&id) {
            Some(thread) if thread.loading_older => {}
            Some(thread) if thread.cursor.is_some() => {
                self.notice = Notice::new(Tone::Quiet, "Reading older messages…");
                self.load_messages(id, true);
            }
            _ if announce => {
                self.notice = Notice::new(Tone::Quiet, "No older messages to load.");
            }
            _ => {}
        }
    }

    /// The wheel scrolls whichever pane is under the pointer, and a click picks a
    /// chat or focuses the conversation. The composer keeps its chat, so clicks on
    /// the ledger wait until it is closed.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let over = |area: Option<Rect>| {
            area.is_some_and(|a| {
                a.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
            })
        };
        if self.overlay == Some(Overlay::Help) {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.overlay = None;
            }
            return;
        }
        let in_thread = over(self.thread_area);
        let in_ledger = over(self.ledger_area);
        match mouse.kind {
            MouseEventKind::ScrollUp if in_thread => {
                if self.scroll >= self.thread_overflow {
                    self.load_older(false);
                } else {
                    self.scroll = (self.scroll + WHEEL_LINES).min(self.thread_overflow);
                }
            }
            MouseEventKind::ScrollDown if in_thread => {
                self.scroll = self.scroll.saturating_sub(WHEEL_LINES);
            }
            MouseEventKind::ScrollUp if in_ledger && self.compose.is_none() => {
                self.select(self.list.selected().unwrap_or(0).saturating_sub(1));
            }
            MouseEventKind::ScrollDown if in_ledger && self.compose.is_none() => {
                self.select(self.list.selected().map_or(0, |i| i + 1));
            }
            MouseEventKind::Down(MouseButton::Left) if in_thread => {
                if self.selected_row().is_some() {
                    self.focus = Focus::Thread;
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_ledger && self.compose.is_none() => {
                let Some(area) = self.ledger_area else {
                    return;
                };
                let row = usize::from((mouse.row - area.y) / CHAT_ROW_HEIGHT);
                let index = self.list.offset() + row;
                if index < self.visible.len() {
                    self.select(index);
                    self.focus = Focus::Thread;
                    self.open_selected();
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let theme = self.theme;
        self.ledger_area = None;
        self.thread_area = None;
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
        if let Some(restart) = self.debugging_restart {
            let (title, copy) = match restart {
                DebuggingRestart::Offered => (
                    "CONNECT TEAMS",
                    "Restart Teams with debugging enabled to read your chats.",
                ),
                DebuggingRestart::Confirming => (
                    "RESTART TEAMS?",
                    "This ends any call in progress. Restart now?",
                ),
                DebuggingRestart::Running => (
                    "RESTARTING TEAMS",
                    "Waiting for Teams to open its debugging port…",
                ),
            };
            state_panel(
                frame,
                body,
                theme,
                title,
                vec![
                    Line::styled(copy, theme.body()),
                    Line::styled(
                        "While enabled, programs running as you can read chats and send as you.",
                        theme.muted(),
                    ),
                ],
                theme.active_rule(),
            );
        } else if let Some(cleanup) = &self.flag_cleanup {
            let apps = cleanup.apps();
            let (title, copy) = match cleanup.stage {
                Cleanup::Offered => (
                    "DEBUGGING LEFT ON",
                    format!("{apps} holds the Teams port through a user-wide WebView2 debugging setting. Remove just those flags?"),
                ),
                Cleanup::Confirming => (
                    "REMOVE DEBUGGING FLAGS?",
                    "Edits the user-wide WebView2 arguments and keeps its other arguments. Remove now?".into(),
                ),
                Cleanup::Running => ("REMOVING FLAGS", "Updating the user-wide setting…".into()),
                Cleanup::Done => (
                    "FLAGS REMOVED",
                    format!("Quit {apps} completely and reopen, then press r."),
                ),
            };
            state_panel(
                frame,
                body,
                theme,
                title,
                vec![
                    Line::styled(copy, theme.body()),
                    Line::styled("Apps keep the port until fully quit.", theme.muted()),
                ],
                theme.active_rule(),
            );
        } else {
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
        let width = usize::from(area.width);
        let spans_width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
        let separator = || Span::styled("  ·  ", theme.rule());

        // Right: the send mode always shows, with a one-column margin like the left.
        let mut right = if self.can_send() {
            vec![Span::styled("✎ can send", theme.badge_send())]
        } else if self.sending_paused {
            vec![Span::styled("sending paused", theme.error())]
        } else {
            vec![Span::styled("read-only", theme.muted())]
        };
        right.push(Span::raw(" "));
        let brand = Span::styled(" TEAMS ", theme.brand());
        let fixed = brand.width() + 2 + spans_width(&right) + 2;
        let profile_width = self.profile().width();
        // Then freshness, as long as some of the profile name still fits beside it.
        if let Some(mut freshness) = self.freshness() {
            let needed = spans_width(&freshness) + 3;
            if fixed + needed + profile_width.min(10) <= width {
                freshness.push(Span::raw("   "));
                freshness.extend(right);
                right = freshness;
            }
        }
        let used = brand.width() + 2 + spans_width(&right) + 2;
        let mut left = vec![
            brand,
            Span::raw("  "),
            Span::styled(
                render::truncate(self.profile(), width.saturating_sub(used)),
                theme.title(),
            ),
        ];
        // Last, the signed-in account next to the profile when there is room.
        if let Some(account) = &self.account
            && used + profile_width + 5 + account.width() <= width
        {
            left.push(separator());
            left.push(Span::styled(account.clone(), theme.muted()));
        }
        let gap = width.saturating_sub(spans_width(&left) + spans_width(&right));
        let mut title = left;
        title.push(Span::raw(" ".repeat(gap)));
        title.extend(right);

        let mut context = vec![Span::raw(" ")];
        for (index, part) in self.context().into_iter().enumerate() {
            let extra = if index == 0 { 0 } else { 5 };
            if spans_width(&context) + extra + spans_width(&part) + 1 > width {
                break;
            }
            if index > 0 {
                context.push(separator());
            }
            context.extend(part);
        }
        let header = Paragraph::new(vec![Line::from(title), Line::from(context)]).block(
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
        self.ledger_area = Some(list_area);
    }

    fn draw_thread(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let theme = self.theme;
        self.thread_area = Some(area);
        self.thread_overflow = 0;
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
        let mut body = Rect {
            y: body.y + 1,
            height: body.height.saturating_sub(1),
            ..body
        };
        if let Some(compose) = self.compose.as_ref().filter(|c| c.chat == row.id) {
            let (rows, (line, column)) =
                layout_draft(&compose.text, compose.cursor, usize::from(body.width));
            let shown = rows.len().min(COMPOSE_ROWS);
            let [conversation, editor] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(shown as u16 + 1)])
                    .areas(body);
            body = conversation;
            let title = match compose.stage {
                Stage::Editing => " MESSAGE ",
                Stage::Sending => " SENDING ",
                Stage::Unconfirmed => " POSSIBLY SENT ",
                Stage::Checking => " CHECKING DELIVERY ",
                Stage::ConfirmResend => " RESEND? ",
            };
            let block = Block::default()
                .borders(Borders::TOP)
                .border_style(theme.active_rule())
                .title(Span::styled(title, theme.column_header()));
            let inner = block.inner(editor);
            frame.render_widget(block, editor);
            let start = (line + 1).saturating_sub(COMPOSE_ROWS);
            let lines: Vec<Line> = if compose.text.is_empty() {
                vec![Line::styled("Write a message…", theme.muted())]
            } else {
                rows.into_iter()
                    .skip(start)
                    .take(COMPOSE_ROWS)
                    .map(|row| Line::styled(row, theme.body()))
                    .collect()
            };
            frame.render_widget(Paragraph::new(lines), inner);
            if compose.stage == Stage::Editing {
                let x = inner.x + (column as u16).min(inner.width.saturating_sub(1));
                frame.set_cursor_position((x, inner.y + (line - start) as u16));
            }
        }
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
        self.thread_overflow = overflow;
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
        let mut all: Vec<(&str, &str, u8)> = if let Some(restart) = self.debugging_restart {
            match restart {
                DebuggingRestart::Offered => vec![
                    ("enter", "restart Teams", 0),
                    ("r", "retry", 1),
                    ("q", "quit", 0),
                ],
                DebuggingRestart::Confirming => vec![
                    ("y", "restart Teams", 0),
                    ("esc", "cancel", 0),
                    ("q", "quit", 1),
                ],
                DebuggingRestart::Running => vec![("q", "quit", 0)],
            }
        } else if let Some(cleanup) = &self.flag_cleanup {
            match cleanup.stage {
                Cleanup::Offered => vec![
                    ("enter", "remove flags", 0),
                    ("r", "retry", 1),
                    ("q", "quit", 0),
                ],
                Cleanup::Confirming => vec![
                    ("y", "remove flags", 0),
                    ("esc", "cancel", 0),
                    ("q", "quit", 1),
                ],
                Cleanup::Running => vec![("q", "quit", 0)],
                Cleanup::Done => vec![("r", "retry", 0), ("q", "quit", 0)],
            }
        } else if let Some(compose) = &self.compose {
            match compose.stage {
                Stage::Editing => vec![
                    ("enter", "send", 0),
                    ("ctrl-j", "new line", 1),
                    ("esc", "close, keep draft", 0),
                ],
                Stage::Sending | Stage::Checking => vec![("ctrl-c", "quit", 1)],
                Stage::Unconfirmed => vec![
                    ("r", "check", 0),
                    ("s", "resend…", 0),
                    ("esc", "keep draft", 1),
                ],
                Stage::ConfirmResend => vec![("y", "send second copy", 0), ("esc", "cancel", 0)],
            }
        } else if self.overlay == Some(Overlay::Filter) {
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
        if self.debugging_restart.is_none()
            && self.flag_cleanup.is_none()
            && self.compose.is_none()
            && self.overlay.is_none()
            && !self.sign_in_needed()
            && self.can_send()
        {
            let rank = if self.focus == Focus::Thread { 0 } else { 2 };
            all.insert(all.len() - 3, ("c", "reply", rank));
        }
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
            (
                "wheel",
                "Scroll the pane under the pointer; older messages load at the top",
            ),
            ("click", "Open a chat, or focus the conversation"),
            (
                "shift-drag",
                "Select text to copy, while the mouse is in use here",
            ),
            ("/", "Filter chats by name or last message"),
            ("c", "Write a message in the open chat"),
            ("r", "Refresh now; chats also refresh on their own"),
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
            if self.can_send() {
                "Enter sends · Ctrl-J adds a line · Esc keeps your draft."
            } else {
                "Read-only: nothing here sends, edits or marks messages as read."
            },
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

/// Wraps a draft to `width` columns, honouring line breaks, and finds the row and
/// column of the byte offset `cursor`.
fn layout_draft(text: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut at = None;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            if i == cursor {
                at = Some((rows.len() - 1, column));
            }
            rows.push(String::new());
            column = 0;
            continue;
        }
        let w = c.width().unwrap_or(0);
        if column + w > width {
            rows.push(String::new());
            column = 0;
        }
        if i == cursor {
            at = Some((rows.len() - 1, column));
        }
        rows.last_mut().expect("never empty").push(c);
        column += w;
    }
    let at = at.unwrap_or((rows.len() - 1, column));
    (rows, at)
}

/// Puts a fresh first page ahead of what was loaded before. Older pages are kept, with
/// their cursor, only when the fresh page overlaps them, so a burst of new items never
/// leaves a silent gap.
fn merge_newest(
    fresh: Vec<Value>,
    cursor: Option<String>,
    old: Vec<Value>,
    old_cursor: Option<String>,
) -> (Vec<Value>, Option<String>) {
    let ids: HashSet<&str> = fresh.iter().filter_map(|v| v["id"].as_str()).collect();
    let overlaps = old
        .iter()
        .any(|v| v["id"].as_str().is_some_and(|id| ids.contains(id)));
    if cursor.is_none() || !overlaps {
        return (fresh, cursor);
    }
    let rest: Vec<Value> = old
        .into_iter()
        .filter(|v| !v["id"].as_str().is_some_and(|id| ids.contains(id)))
        .collect();
    let cursor = if rest.is_empty() { cursor } else { old_cursor };
    let mut items = fresh;
    items.extend(rest);
    (items, cursor)
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

    /// Text colour only: the cell keeps the canvas, selection or header behind it. A
    /// `Color::Reset` background here would show the terminal's own colour through the text.
    fn fg(self, foreground: Color) -> Style {
        if self.color {
            Style::default().fg(foreground)
        } else {
            Style::default()
        }
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

    /// Writing is possible on this profile: warm, so it is noticed without alarm.
    fn badge_send(self) -> Style {
        self.fg(Color::Rgb(246, 183, 82))
            .add_modifier(Modifier::BOLD)
    }

    /// A number worth scanning for in otherwise quiet text.
    fn figure(self) -> Style {
        self.body().add_modifier(Modifier::BOLD)
    }

    fn unread_label(self) -> Style {
        self.fg(Color::Rgb(255, 128, 112))
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
    let _guard = TerminalGuard::enter(false)?;
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

    #[test]
    fn debugging_restart_requires_confirmation_and_can_be_cancelled() {
        let mut app = demo_app();
        app.apply(Outcome::DebuggingRequired);
        press(&mut app, KeyCode::Char('y'));
        assert!(app.debugging_restart == Some(DebuggingRestart::Offered));
        press(&mut app, KeyCode::Enter);
        assert!(app.debugging_restart == Some(DebuggingRestart::Confirming));
        press(&mut app, KeyCode::Esc);
        assert!(app.debugging_restart == Some(DebuggingRestart::Offered));
        assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::Quit));
        press(&mut app, KeyCode::Char('r'));
        assert!(app.debugging_restart.is_none());
        assert!(matches!(app.chats_load, Load::Ready));
    }

    #[test]
    fn debugging_restart_completion_recovers_or_shows_retry() {
        let mut app = demo_app();
        app.debugging_restart = Some(DebuggingRestart::Running);
        app.apply(Outcome::Restarted(Err(AppError::Desktop(
            "Restart failed".into(),
        ))));
        assert!(app.debugging_restart.is_none());
        assert!(matches!(app.chats_load, Load::Failed { auth: false, .. }));
        app.apply(Outcome::Restarted(Ok(())));
        assert!(matches!(app.chats_load, Load::Ready));
    }

    #[test]
    fn debugging_restart_panels_show_actions_and_consequences() {
        for (width, height) in [(50, 14), (80, 24), (120, 30)] {
            for state in [
                DebuggingRestart::Offered,
                DebuggingRestart::Confirming,
                DebuggingRestart::Running,
            ] {
                let mut app = demo_app();
                app.chats_load = Load::Loading;
                app.debugging_restart = Some(state);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text = buffer_text(terminal.backend().buffer());
                assert!(!text.contains("teams doctor"));
                let words = text
                    .replace('│', " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(words.contains("send as you"), "{text}");
                assert!(text.contains("quit"), "{text}");
                if state == DebuggingRestart::Confirming {
                    assert!(text.contains("ends any call"), "{text}");
                    assert!(text.contains("cancel"), "{text}");
                }
            }
        }
    }

    #[test]
    fn flag_cleanup_requires_confirmation_and_ends_with_a_retry() {
        let mut app = demo_app();
        app.apply(Outcome::StrayFlags(vec!["OUTLOOK.EXE".into()]));
        let stage = |app: &App| app.flag_cleanup.as_ref().map(|cleanup| cleanup.stage);
        press(&mut app, KeyCode::Char('y'));
        assert!(stage(&app) == Some(Cleanup::Offered));
        press(&mut app, KeyCode::Enter);
        assert!(stage(&app) == Some(Cleanup::Confirming));
        press(&mut app, KeyCode::Esc);
        assert!(stage(&app) == Some(Cleanup::Offered));
        press(&mut app, KeyCode::Enter);
        // The demo app has no desktop client, so nothing is changed.
        press(&mut app, KeyCode::Char('y'));
        assert!(stage(&app) == Some(Cleanup::Confirming));
        app.apply(Outcome::FlagsCleared(Ok(())));
        assert!(stage(&app) == Some(Cleanup::Done));
        assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::Quit));
        press(&mut app, KeyCode::Char('r'));
        assert!(app.flag_cleanup.is_none());
        assert!(matches!(app.chats_load, Load::Ready));

        app.apply(Outcome::StrayFlags(vec![]));
        app.apply(Outcome::FlagsCleared(Err(AppError::Desktop(
            "Clearing failed".into(),
        ))));
        assert!(app.flag_cleanup.is_none());
        assert!(matches!(app.chats_load, Load::Failed { auth: false, .. }));
    }

    #[test]
    fn flag_cleanup_panels_name_the_apps_and_the_consequences() {
        for (width, height) in [(50, 14), (80, 24), (120, 30)] {
            for stage in [
                Cleanup::Offered,
                Cleanup::Confirming,
                Cleanup::Running,
                Cleanup::Done,
            ] {
                let mut app = demo_app();
                app.chats_load = Load::Loading;
                app.flag_cleanup = Some(FlagCleanup {
                    stage,
                    apps: vec!["OUTLOOK.EXE".into()],
                });
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text = buffer_text(terminal.backend().buffer());
                let words = text
                    .replace('│', " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(words.contains("until fully quit"), "{text}");
                assert!(text.contains("quit"), "{text}");
                match stage {
                    Cleanup::Offered | Cleanup::Done => {
                        assert!(words.contains("OUTLOOK.EXE"), "{text}")
                    }
                    Cleanup::Confirming => {
                        assert!(words.contains("keeps its other"), "{text}");
                        assert!(text.contains("cancel"), "{text}");
                    }
                    Cleanup::Running => {}
                }
            }
        }
    }

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
    fn colour_paints_every_cell_so_the_terminal_background_never_shows() {
        let now: Zoned = "2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
            .parse()
            .unwrap();
        let mut app = App::new(Source::Demo, Theme { color: true }, now);
        app.start();
        let unpainted = |buffer: &ratatui::buffer::Buffer| {
            buffer
                .content()
                .iter()
                .filter(|cell| cell.bg == Color::Reset)
                .count()
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(unpainted(terminal.backend().buffer()), 0);
        press(&mut app, KeyCode::Enter);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(unpainted(terminal.backend().buffer()), 0);
        terminal
            .draw(|frame| draw_connection(frame, Theme { color: true }, "default", true, "x"))
            .unwrap();
        assert_eq!(unpainted(terminal.backend().buffer()), 0);
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

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.handle_mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer_and_clicks_open_chats() {
        let mut app = demo_app();
        app.open_selected();
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let ledger = app.ledger_area.expect("ledger drawn");
        let thread = app.thread_area.expect("conversation drawn");
        assert!(
            app.thread_overflow > WHEEL_LINES,
            "the demo chat overflows 16 rows"
        );

        // The wheel over the conversation scrolls it even while the ledger has focus.
        assert!(app.focus == Focus::Chats);
        mouse(
            &mut app,
            MouseEventKind::ScrollUp,
            thread.x + 5,
            thread.y + 5,
        );
        assert_eq!(app.scroll, WHEEL_LINES);
        assert_eq!(app.list.selected(), Some(0), "the ledger did not move");
        for _ in 0..50 {
            mouse(
                &mut app,
                MouseEventKind::ScrollUp,
                thread.x + 5,
                thread.y + 5,
            );
        }
        assert_eq!(
            app.scroll, app.thread_overflow,
            "scrolling stops at the oldest line"
        );
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            thread.x + 5,
            thread.y + 5,
        );
        assert_eq!(app.scroll, app.thread_overflow - WHEEL_LINES);

        // The wheel over the ledger moves the selection, which resets the conversation.
        mouse(&mut app, MouseEventKind::ScrollDown, ledger.x + 2, ledger.y);
        assert_eq!(app.list.selected(), Some(1));
        assert_eq!(app.scroll, 0);
        mouse(&mut app, MouseEventKind::ScrollUp, ledger.x + 2, ledger.y);
        assert_eq!(app.list.selected(), Some(0));

        // A click on the second entry opens it; a click in the conversation focuses it.
        let left = MouseEventKind::Down(MouseButton::Left);
        mouse(&mut app, left, ledger.x + 2, ledger.y + CHAT_ROW_HEIGHT + 1);
        assert_eq!(app.list.selected(), Some(1));
        assert!(app.focus == Focus::Thread);
        let id = app.selected_row().unwrap().id.clone();
        assert!(app.threads.contains_key(&id));
        press(&mut app, KeyCode::Tab);
        assert!(app.focus == Focus::Chats);
        mouse(&mut app, left, thread.x + 5, thread.y + 5);
        assert!(app.focus == Focus::Thread);

        // Outside both panes, and while the help is open, the mouse does nothing else.
        mouse(&mut app, MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(app.list.selected(), Some(1));
        press(&mut app, KeyCode::Char('?'));
        mouse(&mut app, MouseEventKind::ScrollDown, ledger.x + 2, ledger.y);
        assert_eq!(app.list.selected(), Some(1));
        mouse(&mut app, left, 0, 0);
        assert!(app.overlay.is_none(), "a click closes the help");
    }

    #[test]
    fn the_composer_keeps_its_chat_when_the_ledger_is_clicked() {
        let mut app = demo_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('c'));
        assert!(app.compose.is_some());
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let ledger = app.ledger_area.expect("ledger drawn");
        let left = MouseEventKind::Down(MouseButton::Left);
        mouse(&mut app, left, ledger.x + 2, ledger.y + CHAT_ROW_HEIGHT + 1);
        mouse(&mut app, MouseEventKind::ScrollDown, ledger.x + 2, ledger.y);
        assert_eq!(app.list.selected(), Some(0));
    }

    #[test]
    fn opening_a_chat_prefetches_its_neighbours_and_retries_failures() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        let ids: Vec<String> = app
            .visible
            .iter()
            .map(|&i| app.rows[i].id.clone())
            .collect();
        let below = ids[2].clone();
        assert!(app.threads.contains_key(&ids[0]) && app.threads.contains_key(&below));
        assert!(!app.threads.contains_key(&ids[3]));
        app.threads.get_mut(&below).unwrap().load =
            Load::failed(&AppError::Desktop("read failed".into()));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('j'));
        app.open_selected();
        assert!(matches!(app.threads[&below].load, Load::Ready));
        assert!(app.threads.contains_key(&ids[3]));
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
    fn background_sign_in_loss_preserves_the_draft_and_disables_sending() {
        let mut app = live_app(true, "synthetic-work");
        app.compose = Some(Compose {
            chat: "synthetic-chat".into(),
            text: "Keep this draft".into(),
            cursor: 15,
            stage: Stage::Editing,
        });
        app.startup_failed(&AppError::Auth("Teams needs its browser window".into()));
        assert!(!app.can_send());
        assert!(app.sign_in_needed());
        assert!(app.compose.is_none());
        assert_eq!(app.drafts["synthetic-chat"], "Keep this draft");
        assert!(matches!(
            press(&mut app, KeyCode::Char('a')),
            Action::Authenticate
        ));
        assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::Quit));
        app.send();
        assert!(app.pending_sends.is_empty());
        assert_eq!(app.drafts["synthetic-chat"], "Keep this draft");
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now)),
        });
        app.tick();
        assert!(app.sign_in_needed());
        assert!(!app.notice.text.contains("Enter opens a chat"));
        let mut saved = InboxState::default();
        app.preserve_drafts(&mut saved);
        assert_eq!(saved.drafts["synthetic-chat"], "Keep this draft");
    }

    #[test]
    fn a_non_auth_startup_failure_keeps_chats_readable_and_refresh_can_succeed() {
        let mut app = live_app(true, "synthetic-work");
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now)),
        });
        app.startup_failed(&AppError::Desktop("Temporary verification failure".into()));
        assert!(matches!(app.chats_load, Load::Ready));
        assert!(!app.can_send());
        assert!(app.notice.text.contains("Sending is paused"));
        app.chats_load = Load::Loading;
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now)),
        });
        app.tick();
        assert!(matches!(app.chats_load, Load::Ready));
        assert!(!app.sign_in_needed());
        assert!(!app.can_send());
        press(&mut app, KeyCode::Char('c'));
        assert!(app.notice.text.contains("Sending is paused"));
        assert!(!app.notice.text.contains("profile is read-only"));
        assert!(frame_text(&mut app).contains("sending paused"));
    }

    #[test]
    fn a_read_only_profile_does_not_claim_sending_was_paused() {
        let mut app = live_app(false, "synthetic-profile");
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now)),
        });
        app.startup_failed(&AppError::Desktop("Temporary verification failure".into()));
        assert_eq!(app.notice.text, "Temporary verification failure");
        press(&mut app, KeyCode::Char('c'));
        assert!(app.notice.text.contains("profile is read-only"));
        assert!(!app.notice.text.contains("Sending is paused"));
        let text = frame_text(&mut app);
        assert!(text.lines().next().unwrap().contains("read-only"));
        assert!(!text.contains("sending paused"));
    }

    fn frame_text(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn enter_sends_the_draft_and_the_message_appears() {
        let mut app = demo_app();
        app.open_selected();
        press(&mut app, KeyCode::Char('c'));
        assert!(app.focus == Focus::Thread);
        // List keys are text while composing.
        type_text(&mut app, "on it q r /");
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        type_text(&mut app, "shipping");
        assert!(app.overlay.is_none());
        let compose = app.compose.as_ref().unwrap();
        assert_eq!(compose.text, "on it q r /\nshipping");
        assert!(frame_text(&mut app).contains("MESSAGE"));

        press(&mut app, KeyCode::Enter);
        assert!(app.compose.is_none());
        assert_eq!(app.notice.text, "Sent.");
        let thread = &app.threads["19:launch@thread.v2"];
        assert_eq!(thread.items.len(), 8);
        assert_eq!(thread.items[0]["body"]["content"], "on it q r /\nshipping");
        assert!(frame_text(&mut app).contains("shipping"));
    }

    #[test]
    fn paste_keeps_multiline_text_in_the_draft_until_enter() {
        let mut app = demo_app();
        app.open_selected();
        app.paste("c accidental commands\n");
        assert!(app.compose.is_none());
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "ab");
        press(&mut app, KeyCode::Left);
        app.paste("é\r\nsecond\rthird\n");
        let compose = app.compose.as_ref().unwrap();
        assert_eq!(compose.text, "aé\nsecond\nthird\nb");
        assert_eq!(&compose.text[..compose.cursor], "aé\nsecond\nthird\n");
        assert_eq!(app.threads["19:launch@thread.v2"].items.len(), 7);
        press(&mut app, KeyCode::Enter);
        assert!(app.compose.is_none());
        assert_eq!(app.threads["19:launch@thread.v2"].items.len(), 8);
    }

    #[test]
    fn sending_ignores_extra_keys_and_paste_and_failure_keeps_the_draft() {
        let mut app = demo_app();
        app.open_selected();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "keep this");
        app.compose.as_mut().unwrap().stage = Stage::Sending;
        for key in [KeyCode::Enter, KeyCode::Char('y'), KeyCode::Esc] {
            press(&mut app, key);
        }
        app.paste("more\n");
        assert_eq!(app.compose.as_ref().unwrap().text, "keep this");
        assert_eq!(app.threads["19:launch@thread.v2"].items.len(), 7);
        app.apply(Outcome::Sent {
            chat: "19:launch@thread.v2".into(),
            result: Err(AppError::Unconfirmed(
                "Check the conversation before sending again".into(),
            )),
        });
        assert!(app.compose.as_ref().unwrap().stage == Stage::Unconfirmed);
        assert_eq!(app.compose.as_ref().unwrap().text, "keep this");
        assert!(app.notice.text.contains("Check the conversation"));
        assert_eq!(app.threads["19:launch@thread.v2"].items.len(), 7);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.compose.as_ref().unwrap().text, "keep this");
    }

    #[test]
    fn uncertain_send_blocks_enter_paste_and_reopening_until_explicit_resend() {
        let mut app = demo_app();
        app.open_selected();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "one copy");
        let chat = app.compose.as_ref().unwrap().chat.clone();
        let before = app.threads[&chat].items.len();
        app.pending_sends
            .insert(chat.clone(), "100000000000000001".into());
        app.apply(Outcome::Sent {
            chat: chat.clone(),
            result: Err(AppError::Unconfirmed("may arrive".into())),
        });
        for _ in 0..3 {
            press(&mut app, KeyCode::Enter);
        }
        app.paste("another copy\n");
        assert_eq!(app.compose.as_ref().unwrap().text, "one copy");
        assert_eq!(app.threads[&chat].items.len(), before);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('c'));
        assert!(app.compose.as_ref().unwrap().stage == Stage::Unconfirmed);
        press(&mut app, KeyCode::Char('r'));
        assert!(app.compose.as_ref().unwrap().stage == Stage::Unconfirmed);
        assert!(app.notice.text.contains("absence does not prove failure"));
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter); // Enter is not confirmation.
        assert_eq!(app.threads[&chat].items.len(), before);
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.threads[&chat].items.len(), before + 1);
        assert!(app.compose.is_none());
        assert!(!app.pending_sends.contains_key(&chat));
    }

    #[test]
    fn delivery_check_uses_original_id_and_queued_success_is_not_labelled_sent() {
        let mut app = demo_app();
        app.open_selected();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "once");
        let chat = app.compose.as_ref().unwrap().chat.clone();
        let id = "100000000000000001".to_string();
        app.pending_sends.insert(chat.clone(), id.clone());
        app.apply(Outcome::Sent {
            chat: chat.clone(),
            result: Err(AppError::Unconfirmed("uncertain".into())),
        });
        app.apply(Outcome::Delivery {
            chat: chat.clone(),
            id: "100000000000000002".into(),
            result: Ok(json!({"found":true,"delivery_state":"delivered"})),
        });
        assert!(app.compose.is_some());
        app.apply(Outcome::Delivery {
            chat: chat.clone(),
            id: id.clone(),
            result: Ok(json!({"found":true,"delivery_state":"unknown"})),
        });
        assert!(app.compose.as_ref().unwrap().stage == Stage::Unconfirmed);
        assert!(app.notice.text.contains("Found the original message"));
        app.apply(Outcome::Delivery {
            chat: chat.clone(),
            id,
            result: Ok(json!({"found":true,"delivery_state":"delivered"})),
        });
        assert!(app.compose.is_none());
        assert!(app.notice.text.contains("original client message ID"));
        press(&mut app, KeyCode::Char('c'));
        app.apply(Outcome::Sent {
            chat,
            result: Ok(json!({"id":"queued","source":"desktop","status":"Queued"})),
        });
        assert!(app.notice.text.starts_with("Queued in Teams"));
        assert!(!app.notice.text.starts_with("Sent"));
    }

    #[test]
    fn composer_rejects_oversized_drafts_and_alt_enter_inserts_a_line() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('c'));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.compose.as_ref().unwrap().text, "\n");
        app.paste(&"é".repeat(MAX_BODY));
        press(&mut app, KeyCode::Enter);
        assert!(app.compose.as_ref().unwrap().stage == Stage::Editing);
        assert!(app.notice.text.contains("Too long to send"));
    }

    #[test]
    fn composer_hints_fit_narrow_and_wide_terminals() {
        for width in [50, 100] {
            let mut app = demo_app();
            app.open_selected();
            press(&mut app, KeyCode::Char('c'));
            let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text = buffer_text(terminal.backend().buffer());
            assert!(text.contains("enter send"), "{text}");
            assert!(text.contains("esc close, keep draft"), "{text}");
            assert!(!text.contains("SEND THIS?"));
        }
    }

    #[test]
    fn drafts_survive_closing_and_blank_ones_are_not_offered() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);
        assert!(app.compose.as_ref().unwrap().stage == Stage::Editing);
        type_text(&mut app, "draft");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Esc);
        assert!(app.compose.is_none());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('c'));
        let compose = app.compose.as_ref().unwrap();
        assert_eq!(compose.text, " drat");
        assert_eq!(compose.cursor, compose.text.len());
    }

    #[test]
    fn read_only_profiles_only_explain_how_to_send() {
        let now: Zoned = "2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
            .parse()
            .unwrap();
        let source = Source::Live {
            backend: Arc::new(TeamsBackend::Graph(crate::graph::GraphClient::new(
                "fixture".into(),
            ))),
            profile: "fixture".into(),
            can_send: false,
        };
        let mut app = App::new(source, Theme { color: false }, now.clone());
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&now)),
        });
        press(&mut app, KeyCode::Char('c'));
        assert!(app.compose.is_none());
        assert!(app.notice.text.contains("read-only"));
        assert!(!frame_text(&mut app).contains("c reply"));
    }

    fn header(app: &mut App, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        buffer_text(terminal.backend().buffer())
            .lines()
            .take(2)
            .map(String::from)
            .collect()
    }

    fn live_app(can_send: bool, profile: &str) -> App {
        let now: Zoned = "2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
            .parse()
            .unwrap();
        let source = Source::Live {
            backend: Arc::new(TeamsBackend::Graph(crate::graph::GraphClient::new(
                "fixture".into(),
            ))),
            profile: profile.into(),
            can_send,
        };
        App::new(source, Theme { color: false }, now)
    }

    #[test]
    fn the_header_shows_the_profile_send_mode_freshness_and_counts() {
        let mut app = demo_app();
        let [title, context] = header(&mut app, 100).try_into().unwrap();
        assert!(title.starts_with(" TEAMS   demo "));
        assert!(
            title.ends_with("  ✎ can send"),
            "sample data has no freshness: {title}"
        );
        assert_eq!(context, " 6 chats  ·  2 unread  ·  sample data");

        let mut app = live_app(false, "work");
        let [title, context] = header(&mut app, 100).try_into().unwrap();
        assert!(title.ends_with(" syncing   read-only"), "{title}");
        assert_eq!(context.trim(), "Microsoft Graph");
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now.clone())),
        });
        app.account = Some("Ari Chen".into());
        let [title, _] = header(&mut app, 100).try_into().unwrap();
        assert!(title.starts_with(" TEAMS   work  ·  Ari Chen "), "{title}");
        assert!(title.ends_with(" ● live   read-only"), "{title}");
    }

    #[test]
    fn narrow_headers_keep_the_send_mode_and_drop_the_rest_in_order() {
        let mut app = live_app(true, "a-rather-long-profile-name-for-work");
        app.apply(Outcome::Chats {
            more: false,
            result: Ok(demo_chats(&app.now.clone())),
        });
        app.account = Some("Someone With A Long Display Name".into());
        let [title, _] = header(&mut app, 110).try_into().unwrap();
        assert!(title.contains("for-work  ·  Someone With"), "{title}");
        let [title, _] = header(&mut app, 100).try_into().unwrap();
        assert!(
            !title.contains("Someone"),
            "the account goes first: {title}"
        );
        assert!(title.contains("for-work "), "{title}");
        let [title, context] = header(&mut app, 50).try_into().unwrap();
        // The one-column right margin is trimmed from the frame text.
        assert_eq!(title.chars().count(), 49, "{title}");
        assert!(title.ends_with("● live   ✎ can send"), "{title}");
        assert!(title.contains("a-rather-long-pro"), "{title}");
        assert!(title.contains('…'), "the profile is shortened: {title}");
        assert_eq!(context, " 6 chats  ·  2 unread  ·  Microsoft Graph");
    }

    #[test]
    fn failed_refreshes_mark_the_chats_stale_until_one_succeeds() {
        let mut app = live_app(false, "work");
        let chats = || Outcome::Chats {
            more: false,
            result: Ok(demo_chats(
                &"2026-10-13T10:15:00+02:00[Europe/Amsterdam]"
                    .parse()
                    .unwrap(),
            )),
        };
        app.apply(chats());
        let at = app
            .synced_at
            .clone()
            .expect("a successful read is recorded");
        assert!(!app.stale);
        app.fetching_chats = true;
        app.apply(Outcome::Chats {
            more: false,
            result: Err(AppError::Api("unavailable".into())),
        });
        assert!(app.stale);
        let [title, _] = header(&mut app, 100).try_into().unwrap();
        assert!(
            title.contains(&format!("stale since {}", at.strftime("%H:%M"))),
            "{title}"
        );
        app.apply(chats());
        assert!(!app.stale);
        let [title, _] = header(&mut app, 100).try_into().unwrap();
        assert!(title.contains("● live"), "{title}");
    }

    #[test]
    fn reloads_keep_older_pages_only_when_the_fresh_page_overlaps() {
        let ids = |items: &[Value]| -> Vec<String> {
            items
                .iter()
                .map(|v| v["id"].as_str().unwrap().to_string())
                .collect()
        };
        let page =
            |names: &[&str]| -> Vec<Value> { names.iter().map(|id| json!({"id": id})).collect() };
        let old = page(&["c", "b", "a"]);
        let (items, cursor) = merge_newest(
            page(&["d", "c"]),
            Some("new".into()),
            old.clone(),
            Some("old".into()),
        );
        assert_eq!(ids(&items), ["d", "c", "b", "a"]);
        assert_eq!(cursor.as_deref(), Some("old"));
        let (items, cursor) =
            merge_newest(page(&["f", "e"]), Some("new".into()), old.clone(), None);
        assert_eq!(ids(&items), ["f", "e"], "no overlap means a possible gap");
        assert_eq!(cursor.as_deref(), Some("new"));
        let (items, cursor) = merge_newest(page(&["d", "c"]), None, old, Some("old".into()));
        assert_eq!(
            ids(&items),
            ["d", "c"],
            "a complete page replaces everything"
        );
        assert!(cursor.is_none());
    }

    #[test]
    fn drafts_wrap_and_track_the_cursor() {
        assert_eq!(layout_draft("", 0, 10), (vec![String::new()], (0, 0)));
        let (rows, at) = layout_draft("abcdef\ngh", 8, 4);
        assert_eq!(rows, ["abcd", "ef", "gh"]);
        assert_eq!(at, (2, 1));
        assert_eq!(layout_draft("abcd", 4, 4).1, (0, 4));
        assert_eq!(layout_draft("ab\n", 3, 4).1, (1, 0));
        let (rows, at) = layout_draft("你好你", "你好".len(), 4);
        assert_eq!(rows, ["你好", "你"]);
        assert_eq!(at, (1, 0));
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
