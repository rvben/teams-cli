use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::error::AppError;

const INK: Color = Color::Rgb(230, 233, 239);
const MUTED: Color = Color::Rgb(151, 158, 171);
const FOCUS: Color = Color::Rgb(102, 194, 255);
const LIVE: Color = Color::Rgb(255, 119, 107);
const SURFACE: Color = Color::Rgb(20, 23, 30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiExit {
    Quit,
    Authenticate,
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

#[derive(Debug, Clone)]
pub struct TuiData {
    pub account: String,
    pub conversations: Vec<Conversation>,
}

#[derive(Debug, Clone)]
pub struct Conversation {
    pub name: String,
    pub unread: u16,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub sender: String,
    pub time: String,
    pub body: String,
}

pub fn demo_data() -> TuiData {
    TuiData {
        account: "Alex Morgan · Northstar Studio".into(),
        conversations: vec![
            Conversation {
                name: "Launch room".into(),
                unread: 3,
                messages: vec![
                    Message {
                        sender: "Mina".into(),
                        time: "09:42".into(),
                        body: "The release candidate is green. I pinned the final checklist."
                            .into(),
                    },
                    Message {
                        sender: "Jon".into(),
                        time: "09:46".into(),
                        body: "Docs are ready. One broken link remains in the migration guide."
                            .into(),
                    },
                    Message {
                        sender: "You".into(),
                        time: "09:48".into(),
                        body: "I’ll take it. Hold the announcement until I clear the link check."
                            .into(),
                    },
                ],
            },
            Conversation {
                name: "Design crit".into(),
                unread: 0,
                messages: vec![],
            },
            Conversation {
                name: "Ari Chen".into(),
                unread: 1,
                messages: vec![],
            },
            Conversation {
                name: "Platform on-call".into(),
                unread: 0,
                messages: vec![],
            },
        ],
    }
}

struct App {
    data: TuiData,
    selected: usize,
}

pub fn snapshot(width: u16, height: u16) -> Result<String, AppError> {
    let backend = TestBackend::new(width.max(48), height.max(18));
    let mut terminal = Terminal::new(backend).expect("test backend is infallible");
    let app = App {
        data: demo_data(),
        selected: 0,
    };
    terminal
        .draw(|frame| draw(frame, &app))
        .expect("test backend is infallible");
    let buffer = terminal.backend().buffer();
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
    Ok(output)
}

pub fn connection_snapshot(
    width: u16,
    height: u16,
    profile: &str,
    configured: bool,
    reason: &str,
) -> Result<String, AppError> {
    let backend = TestBackend::new(width.max(48), height.max(18));
    let mut terminal = Terminal::new(backend).expect("test backend is infallible");
    terminal
        .draw(|frame| draw_connection(frame, profile, configured, reason))
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

pub fn run(data: TuiData) -> Result<(), AppError> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(AppError::NonInteractive("`teams tui` needs a terminal; use `teams tui --demo --snapshot` for an inspectable frame".into()));
    }
    let _terminal_guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut app = App { data, selected: 0 };
    (|| -> Result<(), AppError> {
        loop {
            terminal.draw(|frame| draw(frame, &app))?;
            if event::poll(Duration::from_millis(150))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.selected =
                            (app.selected + 1).min(app.data.conversations.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.selected = app.selected.saturating_sub(1)
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })()
}

/// Present an actionable connection state inside the same terminal experience.
/// Returning first restores the alternate screen before the caller opens a
/// browser or prints device-code instructions.
pub fn request_authentication(
    profile: &str,
    configured: bool,
    reason: &str,
) -> Result<TuiExit, AppError> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(AppError::NonInteractive(
            "`teams tui` needs a terminal; run `teams auth login --device-code` for headless sign-in"
                .into(),
        ));
    }
    let _terminal_guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    loop {
        terminal.draw(|frame| draw_connection(frame, profile, configured, reason))?;
        if event::poll(Duration::from_millis(150))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
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

fn draw_connection(frame: &mut ratatui::Frame, profile: &str, configured: bool, reason: &str) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(SURFACE)), area);
    let panel_width = area.width.saturating_sub(2).clamp(1, 74);
    let panel_height = area.height.saturating_sub(2).clamp(1, 12);
    let panel = Rect {
        x: area.x + area.width.saturating_sub(panel_width) / 2,
        y: area.y + area.height.saturating_sub(panel_height) / 2,
        width: panel_width,
        height: panel_height,
    };
    let title = if configured {
        " SIGN-IN REQUIRED "
    } else {
        " CONNECT MICROSOFT TEAMS "
    };
    let action = if configured {
        "Continue with Microsoft sign-in. Your password is never shared with teams-cli."
    } else {
        "Set up this profile and continue with secure Microsoft sign-in."
    };
    let content = vec![
        Line::styled(
            if configured {
                "Your Teams session needs attention"
            } else {
                "Bring your Teams workspace into focus"
            },
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Line::styled(format!("Profile: {profile}"), Style::default().fg(MUTED)),
        Line::raw(""),
        Line::styled(reason, Style::default().fg(INK)),
        Line::styled(action, Style::default().fg(MUTED)),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                " enter / a ",
                Style::default()
                    .fg(Color::Black)
                    .bg(FOCUS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if configured {
                    " sign in   "
                } else {
                    " set up   "
                },
                Style::default().fg(MUTED),
            ),
            Span::styled("q", Style::default().fg(INK)),
            Span::styled(" quit", Style::default().fg(MUTED)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(content).wrap(Wrap { trim: true }).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(FOCUS))
                .padding(ratatui::widgets::Padding::horizontal(2)),
        ),
        panel,
    );
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(area);
    header(frame, vertical[0], app);
    if area.width < 76 {
        conversation(frame, vertical[1], app);
    } else {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(28), Constraint::Min(44)])
            .split(vertical[1]);
        rail(frame, body[0], app);
        conversation(frame, body[1], app);
    }
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            " j/k ",
            Style::default()
                .fg(Color::Black)
                .bg(FOCUS)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" move   ", Style::default().fg(MUTED)),
        Span::styled("q", Style::default().fg(INK)),
        Span::styled(" quit", Style::default().fg(MUTED)),
    ]))
    .style(Style::default().bg(SURFACE));
    frame.render_widget(footer, vertical[2]);
}

fn header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " teams ",
            Style::default()
                .fg(Color::Black)
                .bg(LIVE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  INBOX",
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", app.data.account),
            Style::default().fg(MUTED),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(55, 61, 72))),
    );
    frame.render_widget(title, area);
}

fn rail(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let items = app
        .data
        .conversations
        .iter()
        .enumerate()
        .map(|(index, conversation)| {
            let marker = if index == app.selected { "›" } else { " " };
            let unread = if conversation.unread > 0 {
                format!("  {}", conversation.unread)
            } else {
                String::new()
            };
            let style = if index == app.selected {
                Style::default().fg(FOCUS).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(INK)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {marker} {}", conversation.name), style),
                Span::styled(unread, Style::default().fg(LIVE)),
            ]))
        });
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Conversations ")
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(Color::Rgb(55, 61, 72))),
        ),
        area,
    );
}

fn conversation(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let conversation = &app.data.conversations[app.selected];
    let mut lines = vec![
        Line::styled(
            conversation.name.clone(),
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Line::styled("Channel · 12 members", Style::default().fg(MUTED)),
        Line::raw(""),
    ];
    if conversation.messages.is_empty() {
        lines.push(Line::styled(
            "No recent messages",
            Style::default().fg(MUTED),
        ));
        lines.push(Line::styled(
            "Press c to start the conversation.",
            Style::default().fg(MUTED),
        ));
    } else {
        for message in &conversation.messages {
            lines.push(Line::from(vec![
                Span::styled(
                    &message.sender,
                    Style::default().fg(FOCUS).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", message.time), Style::default().fg(MUTED)),
            ]));
            lines.push(Line::styled(&message.body, Style::default().fg(INK)));
            lines.push(Line::raw(""));
        }
    }
    let block = Block::default()
        .title(" Now ")
        .borders(Borders::NONE)
        .padding(ratatui::widgets::Padding::horizontal(2));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_contains_real_states() {
        let frame = super::snapshot(90, 26).unwrap();
        assert!(frame.contains("Launch room"));
        assert!(frame.contains("release candidate is green"));
        assert!(frame.contains("q quit"));
    }

    #[test]
    fn connection_snapshot_has_a_clear_recovery_action() {
        let frame =
            super::connection_snapshot(80, 22, "work", true, "Your saved session has expired.")
                .unwrap();
        assert!(frame.contains("SIGN-IN REQUIRED"));
        assert!(frame.contains("Profile: work"));
        assert!(frame.contains("enter / a"));
        assert!(frame.contains("sign in"));
        assert!(frame.contains("q quit"));
    }
}
