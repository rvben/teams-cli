use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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

pub fn run(data: TuiData) -> Result<(), AppError> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(AppError::NonInteractive("`teams tui` needs a terminal; use `teams tui --demo --snapshot` for an inspectable frame".into()));
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App { data, selected: 0 };
    let result = (|| -> Result<(), AppError> {
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
    })();
    let cleanup = (|| -> Result<(), AppError> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    })();
    result.and(cleanup)
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
}
