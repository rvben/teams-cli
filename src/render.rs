//! Human text views for chats and messages, shared by the Graph and desktop backends.
use jiff::{Timestamp, Zoned, tz::TimeZone};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::graph::Page;

/// The local clock and terminal width a view is rendered against.
pub struct View {
    pub now: Zoned,
    pub width: usize,
}

impl View {
    pub fn terminal() -> Self {
        let width = crossterm::terminal::size().map_or(100, |(w, _)| usize::from(w));
        Self {
            now: Zoned::now().with_time_zone(TimeZone::system()),
            width: width.clamp(40, 100),
        }
    }

    fn local(&self, value: &Value) -> Option<Zoned> {
        let ts: Timestamp = value.as_str()?.parse().ok()?;
        Some(ts.to_zoned(self.now.time_zone().clone()))
    }

    /// Compact time for list rows: "14:03", "Mon 14:03", "12 Oct" or "12 Oct 2025".
    fn short(&self, at: &Zoned) -> String {
        let days = self.days_ago(at);
        if days == 0 {
            at.strftime("%H:%M").to_string()
        } else if (1..7).contains(&days) {
            at.strftime("%a %H:%M").to_string()
        } else if at.year() == self.now.year() {
            at.strftime("%-d %b").to_string()
        } else {
            at.strftime("%-d %b %Y").to_string()
        }
    }

    fn day_header(&self, at: &Zoned) -> String {
        match self.days_ago(at) {
            0 => "Today".into(),
            1 => "Yesterday".into(),
            _ if at.year() == self.now.year() => at.strftime("%A %-d %B").to_string(),
            _ => at.strftime("%A %-d %B %Y").to_string(),
        }
    }

    fn days_ago(&self, at: &Zoned) -> i64 {
        at.date()
            .until(self.now.date())
            .map_or(0, |span| i64::from(span.get_days()))
    }
}

/// One chat as a list row: what the text view and the TUI ledger both show.
pub struct ChatRow {
    pub id: String,
    pub title: String,
    pub kind: Option<&'static str>,
    pub members: usize,
    pub time: String,
    pub preview: Option<String>,
    pub unread: bool,
}

impl ChatRow {
    pub fn new(chat: &Value, view: &View, viewer: Option<&str>) -> Self {
        let preview = &chat["lastMessagePreview"];
        Self {
            id: str_at(chat, &["id"]).unwrap_or_default().to_string(),
            title: chat_title(chat, viewer),
            kind: match str_at(chat, &["chatType"]) {
                Some("group") => Some("group"),
                Some("meeting") => Some("meeting"),
                _ => None,
            },
            members: chat["members"].as_array().map_or(0, Vec::len),
            time: view
                .local(&preview["createdDateTime"])
                .or_else(|| view.local(&chat["lastUpdatedDateTime"]))
                .map(|at| view.short(&at))
                .unwrap_or_default(),
            preview: preview_text(chat, viewer),
            unread: is_unread(chat, viewer),
        }
    }
}

/// A conversation laid out oldest first, before wrapping to a width.
#[derive(Debug, PartialEq)]
pub enum Entry {
    Day(String),
    Sender {
        name: String,
        time: Option<String>,
        own: bool,
    },
    /// One source line of a message body; empty for a paragraph break.
    Text(String),
}

pub fn conversation(items: &[Value], view: &View, viewer: Option<&str>) -> Vec<Entry> {
    let mut items: Vec<(Option<Zoned>, &Value)> = items
        .iter()
        .filter(|m| matches!(str_at(m, &["messageType"]), None | Some("message")))
        .map(|m| (view.local(&m["createdDateTime"]), m))
        .collect();
    // Services list newest first; a conversation reads oldest first.
    items.sort_by_key(|(at, _)| at.as_ref().map(Zoned::timestamp));
    let mut entries = Vec::new();
    let mut last: Option<(String, Zoned)> = None;
    for (at, message) in items {
        let name = sender_name(message, viewer);
        let own = viewer.is_some() && str_at(message, &["from", "user", "id"]) == viewer;
        if let Some(at) = &at {
            let new_day = last
                .as_ref()
                .is_none_or(|(_, prev)| prev.date() != at.date());
            if new_day {
                entries.push(Entry::Day(view.day_header(at)));
            }
            // Messages from one sender a few minutes apart read as one turn.
            let continues = !new_day
                && last.as_ref().is_some_and(|(who, prev)| {
                    *who == name
                        && prev.until(at).is_ok_and(|gap| {
                            gap.total(jiff::Unit::Minute).unwrap_or(f64::MAX) < 5.0
                        })
                });
            if !continues {
                entries.push(Entry::Sender {
                    name: name.clone(),
                    time: Some(at.strftime("%H:%M").to_string()),
                    own,
                });
            }
            last = Some((name, at.clone()));
        } else {
            entries.push(Entry::Sender {
                name,
                time: None,
                own,
            });
            last = None;
        }
        entries.extend(message_text(message).lines().map(|l| Entry::Text(l.into())));
    }
    entries
}

pub fn chats(page: &Page, view: &View, continuation: &str) -> String {
    let viewer = page.viewer.as_deref();
    let mut blocks = Vec::new();
    for chat in &page.items {
        let row = ChatRow::new(chat, view, viewer);
        let mut title = row.title;
        if let Some(kind) = row.kind {
            title = format!("{title} · {kind}");
        }
        let time = row.time;
        let room = view.width.saturating_sub(time.width() + 4).max(10);
        let title = truncate(&title, room);
        let gap = view
            .width
            .saturating_sub(title.width() + time.width() + 2)
            .max(1);
        let mut lines = vec![format!(
            "{}{title}{}{time}",
            if row.unread { "● " } else { "  " },
            " ".repeat(gap)
        )];
        if let Some(text) = row.preview {
            lines.push(format!("  {}", truncate(&text, view.width - 2)));
        }
        lines.push(format!("  {}", row.id));
        blocks.push(lines.join("\n"));
    }
    if blocks.is_empty() {
        return "No chats.".into();
    }
    let mut out = blocks.join("\n\n");
    if page.truncated {
        out.push_str(&format!(
            "\n\nMore chats available. Continue with --cursor '{continuation}'"
        ));
    }
    out
}

pub fn messages(page: &Page, view: &View, continuation: &str) -> String {
    if page.items.is_empty() {
        return "No messages.".into();
    }
    let mut lines = Vec::new();
    if page.truncated {
        lines.push(format!(
            "Older messages available. Continue with --cursor '{continuation}'"
        ));
    }
    for entry in conversation(&page.items, view, page.viewer.as_deref()) {
        match entry {
            Entry::Day(day) => {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(format!("── {day} ──"));
            }
            Entry::Sender {
                name,
                time: Some(time),
                ..
            } => lines.push(format!("{name}  {time}")),
            Entry::Sender { name, .. } => lines.push(name),
            Entry::Text(line) if line.is_empty() => lines.push(String::new()),
            Entry::Text(line) => lines.extend(
                wrap(&line, view.width - 2)
                    .into_iter()
                    .map(|l| format!("  {l}")),
            ),
        }
    }
    lines.join("\n")
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |v, key| v.get(*key))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn chat_title(chat: &Value, viewer: Option<&str>) -> String {
    if let Some(topic) = str_at(chat, &["topic"]) {
        return topic.trim().to_string();
    }
    let names: Vec<&str> = chat["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| viewer.is_none() || str_at(m, &["userId"]) != viewer)
        .filter_map(|m| str_at(m, &["displayName"]))
        .collect();
    let members = chat["members"].as_array().map_or(&[][..], Vec::as_slice);
    let only_viewer = viewer.is_some()
        && !members.is_empty()
        && members.iter().all(|m| str_at(m, &["userId"]) == viewer);
    match names.as_slice() {
        [] if only_viewer => "You".into(),
        [] => match str_at(chat, &["chatType"]) {
            Some("oneOnOne") => "(unknown person)".into(),
            Some("meeting") => "(untitled meeting)".into(),
            _ => "(untitled chat)".into(),
        },
        [one] => (*one).into(),
        _ if names.len() <= 3 => names.join(", "),
        _ => format!("{} +{}", names[..3].join(", "), names.len() - 3),
    }
}

fn is_unread(chat: &Value, viewer: Option<&str>) -> bool {
    let preview = &chat["lastMessagePreview"];
    let sent = str_at(preview, &["createdDateTime"]).and_then(|t| t.parse::<Timestamp>().ok());
    let read = str_at(chat, &["viewpoint", "lastMessageReadDateTime"])
        .and_then(|t| t.parse::<Timestamp>().ok());
    let from_viewer = viewer.is_some() && str_at(preview, &["from", "user", "id"]) == viewer;
    matches!(str_at(preview, &["messageType"]), None | Some("message"))
        && !from_viewer
        && matches!((sent, read), (Some(sent), Some(read)) if sent > read)
}

fn preview_text(chat: &Value, viewer: Option<&str>) -> Option<String> {
    let preview = &chat["lastMessagePreview"];
    if preview.is_null() || str_at(preview, &["messageType"]).is_some_and(|t| t != "message") {
        return None;
    }
    let text = message_text(preview)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let from_viewer = viewer.is_some() && str_at(preview, &["from", "user", "id"]) == viewer;
    let author = if from_viewer {
        Some("You")
    } else if str_at(chat, &["chatType"]) == Some("oneOnOne") {
        None
    } else {
        str_at(preview, &["from", "user", "displayName"])
            .and_then(|name| name.split_whitespace().next())
    };
    Some(match author {
        Some(author) => format!("{author}: {text}"),
        None => text,
    })
}

fn sender_name(message: &Value, viewer: Option<&str>) -> String {
    let user = &message["from"]["user"];
    if viewer.is_some() && str_at(user, &["id"]) == viewer {
        return "You".into();
    }
    str_at(user, &["displayName"])
        .or_else(|| str_at(message, &["from", "application", "displayName"]))
        .unwrap_or("Unknown sender")
        .to_string()
}

fn message_text(message: &Value) -> String {
    if message["deleted"] == true
        || str_at(message, &["deletedDateTime"]).is_some()
        || message["isDeleted"] == true
    {
        return "(message deleted)".into();
    }
    let content = str_at(message, &["body", "content"]).unwrap_or_default();
    let text = if str_at(message, &["body", "contentType"]) == Some("html") {
        html_to_text(content)
    } else {
        content.trim().to_string()
    };
    let attachments = message["attachments"].as_array().map_or(0, Vec::len);
    if !text.is_empty() {
        text
    } else if attachments > 0 {
        format!(
            "[{attachments} attachment{}]",
            if attachments == 1 { "" } else { "s" }
        )
    } else {
        "(no text)".into()
    }
}

/// Reduces Teams message HTML to readable plain text.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut quote = 0usize;
    let mut skip = 0usize;
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        if skip == 0 {
            push_text(&mut out, &decode_entities(&rest[..start]), quote);
        }
        let Some(end) = rest[start..].find('>') else {
            rest = &rest[start..];
            break;
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("style" | "script", false) if !tag.ends_with('/') => skip += 1,
            ("style" | "script", true) => skip = skip.saturating_sub(1),
            ("br", _) => {
                newline(&mut out, quote);
                // Teams formats its HTML with CRLF after <br>. That whitespace
                // is not another visible line break; consecutive <br>s still are.
                rest = rest.trim_start_matches([' ', '\t', '\r', '\n']);
            }
            ("p" | "div" | "tr" | "h1" | "h2" | "h3" | "pre" | "ul" | "ol", _) => {
                block(&mut out, quote)
            }
            ("li", false) => {
                block(&mut out, quote);
                out.push_str("• ");
            }
            ("td" | "th", false) => out.push(' '),
            ("blockquote", false) => {
                quote += 1;
                block(&mut out, quote);
            }
            ("blockquote", true) => {
                quote = quote.saturating_sub(1);
                newline(&mut out, quote);
            }
            ("at", false) => out.push('@'),
            ("img" | "emoji", false) if skip == 0 => match attribute(tag, "alt") {
                Some(alt) if name == "emoji" || tag.contains("emoji") => {
                    out.push_str(&decode_entities(&alt))
                }
                _ if name == "img" => out.push_str("[image]"),
                _ => {}
            },
            ("attachment", false) => out.push_str("[attachment]"),
            _ => {}
        }
    }
    if skip == 0 {
        push_text(&mut out, &decode_entities(rest), quote);
    }
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let blank = line.chars().all(|c| c == '>' || c == ' ');
        if blank && lines.last().is_none_or(|l: &String| l.is_empty()) {
            continue;
        }
        lines.push(if blank { String::new() } else { line });
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// Starts a new line unless the current one is still empty.
fn block(out: &mut String, quote: usize) {
    let current = out.rsplit('\n').next().unwrap_or_default();
    if current.chars().any(|c| c != '>' && !c.is_whitespace()) {
        newline(out, quote);
    } else {
        let start = out.len() - current.len();
        out.truncate(start);
        out.push_str(&"> ".repeat(quote));
    }
}

fn newline(out: &mut String, quote: usize) {
    out.push('\n');
    out.push_str(&"> ".repeat(quote));
}

fn push_text(out: &mut String, text: &str, quote: usize) {
    let mut parts = text.split('\n');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        newline(out, quote);
        out.push_str(part);
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before = lower[..at].chars().last();
        let after = lower[from..].trim_start();
        if before.is_some_and(char::is_whitespace) && after.starts_with('=') {
            let value = tag[tag.len() - after.len() + 1..].trim_start();
            let quote = value.chars().next()?;
            return if quote == '"' || quote == '\'' {
                value[1..].split(quote).next().map(String::from)
            } else {
                value.split_whitespace().next().map(String::from)
            };
        }
    }
    None
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| {
                let entity = &rest[1..=end];
                let c = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                        .and_then(char::from_u32),
                }?;
                Some((c, end + 2))
            });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Greedy word wrap by display width; words longer than a line are split.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut lines = Vec::new();
    let prefix_len = text.len() - text.trim_start_matches("> ").len();
    let (prefix, body) = text.split_at(prefix_len);
    let room = width.saturating_sub(prefix.width()).max(10);
    let mut line = String::new();
    for word in body.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let need = if line.is_empty() {
                word.width()
            } else {
                line.width() + 1 + word.width()
            };
            if need <= room {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            let mut head = String::new();
            let mut tail = word.chars();
            while let Some(c) = tail.clone().next() {
                if head.width() + c.width().unwrap_or(0) > room {
                    break;
                }
                head.push(c);
                tail.next();
            }
            lines.push(head);
            word = tail.collect();
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(|l| format!("{prefix}{l}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view() -> View {
        View {
            now: "2026-10-13T15:00:00+00:00[UTC]".parse().unwrap(),
            width: 60,
        }
    }

    fn page(items: Vec<Value>, viewer: Option<&str>) -> Page {
        Page {
            items,
            next_cursor: None,
            next_offset: None,
            truncated: false,
            source: None,
            viewer: viewer.map(String::from),
        }
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<div><blockquote itemtype="http://schema.skype.com/Reply"><strong>Ann</strong><p>old  point</p></blockquote><p>Hi <at id="0">Bob</at>,&nbsp;5 &lt; 6 &amp; &#x1F600;</p><ul><li>one</li><li>two</li></ul><emoji alt="🙂" title="Smile"></emoji><img src="x"><br><style>p{}</style></div><p>a<br><br>b</p><p>c</p>"#;
        assert_eq!(
            html_to_text(html),
            "> Ann\n> old point\n\nHi @Bob, 5 < 6 & 😀\n• one\n• two\n🙂[image]\na\n\nb\nc"
        );
        assert_eq!(html_to_text("<p></p><p>  </p>"), "");
        assert_eq!(
            html_to_text("<p>line one<br />\r\nline two</p>"),
            "line one\nline two"
        );
        assert_eq!(decode_entities("a &bogus; &#65; &"), "a &bogus; A &");
    }

    #[test]
    fn plain_text_entities_are_literal_in_conversations_and_previews() {
        let content = "Use &amp; and &#65; literally";
        let message = json!({"from": {"user": {"displayName": "Ann"}},
            "body": {"contentType": "text", "content": content}});
        let entries = conversation(std::slice::from_ref(&message), &view(), None);
        assert!(entries.contains(&Entry::Text(content.into())));
        let chat = json!({"id": "chat", "chatType": "oneOnOne", "lastMessagePreview": message});
        assert_eq!(
            ChatRow::new(&chat, &view(), None).preview.as_deref(),
            Some(content)
        );
        assert_eq!(
            message_text(&json!({"body": {"contentType": "html", "content": content}})),
            "Use & and A literally"
        );
    }

    #[test]
    fn times_are_relative_to_the_local_day() {
        let view = view();
        let at = |s: &str| view.local(&json!(s)).unwrap();
        assert_eq!(view.short(&at("2026-10-13T09:05:00Z")), "09:05");
        assert_eq!(view.short(&at("2026-10-12T09:05:00.123Z")), "Mon 09:05");
        assert_eq!(view.short(&at("2026-09-02T09:05:00Z")), "2 Sep");
        assert_eq!(view.short(&at("2025-09-02T09:05:00Z")), "2 Sep 2025");
        assert_eq!(view.day_header(&at("2026-10-12T23:00:00Z")), "Yesterday");
        assert_eq!(
            view.day_header(&at("2026-10-01T23:00:00Z")),
            "Thursday 1 October"
        );
    }

    #[test]
    fn chat_titles_prefer_topic_then_other_members() {
        let members = json!([
            {"userId": "me", "displayName": "Sam"},
            {"userId": "a", "displayName": "Ann Smith"},
            {"userId": "b", "displayName": null}
        ]);
        let chat = json!({"topic": " Release ", "members": members});
        assert_eq!(chat_title(&chat, Some("me")), "Release");
        let chat = json!({"topic": null, "chatType": "oneOnOne", "members": members});
        assert_eq!(chat_title(&chat, Some("me")), "Ann Smith");
        assert_eq!(chat_title(&chat, None), "Sam, Ann Smith");
        let chat = json!({"chatType": "oneOnOne", "members": []});
        assert_eq!(chat_title(&chat, Some("me")), "(unknown person)");
        let notes =
            json!({"chatType": "oneOnOne", "members": [{"userId": "me", "displayName": "Sam"}]});
        assert_eq!(chat_title(&notes, Some("me")), "You");
        let members: Vec<Value> = ["A", "B", "C", "D", "E"]
            .map(|n| json!({"userId": n, "displayName": n}))
            .into();
        let many = json!({ "members": members });
        assert_eq!(chat_title(&many, None), "A, B, C +2");
    }

    #[test]
    fn chat_rows_show_unread_title_time_preview_and_id() {
        let chat = |from: &str, read: &str| {
            json!({
                "id": "19:a@unq.gbl.spaces", "topic": null, "chatType": "oneOnOne",
                "members": [{"userId": "me", "displayName": "Sam"}, {"userId": "a", "displayName": "Ann"}],
                "viewpoint": {"lastMessageReadDateTime": read},
                "lastMessagePreview": {"createdDateTime": "2026-10-13T14:03:00Z", "messageType": "message",
                    "from": {"user": {"id": from, "displayName": "Ann"}},
                    "body": {"contentType": "html", "content": "<p>see you</p>"}}
            })
        };
        let text = chats(
            &page(vec![chat("a", "2026-10-13T10:00:00Z")], Some("me")),
            &view(),
            "c",
        );
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("● Ann ") && lines[0].ends_with(" 14:03"));
        assert_eq!(lines[0].width(), 60);
        assert_eq!(lines[1..], ["  see you", "  19:a@unq.gbl.spaces"]);
        let text = chats(
            &page(vec![chat("me", "2026-10-13T10:00:00Z")], Some("me")),
            &view(),
            "c",
        );
        assert!(text.starts_with("  Ann ") && text.contains("\n  You: see you\n"));
        let mut more = page(vec![], None);
        assert_eq!(chats(&more, &view(), "c"), "No chats.");
        more.items
            .push(json!({"id": "19:g@thread.v2", "chatType": "group", "topic": "Ops"}));
        more.truncated = true;
        let text = chats(&more, &view(), "abc");
        assert!(text.starts_with("  Ops · group"));
        assert!(text.ends_with("Continue with --cursor 'abc'"));
    }

    #[test]
    fn messages_read_oldest_first_grouped_by_day_and_sender() {
        let msg = |id: &str, at: &str, from: &str, body: &str| {
            json!({"id": id, "createdDateTime": at,
                "from": {"user": {"id": from, "displayName": from.to_uppercase()}},
                "body": {"contentType": "text", "content": body}})
        };
        let mut deleted = msg("5", "2026-10-13T14:30:00Z", "ann", "x");
        deleted["deleted"] = json!(true);
        let mut event = msg("6", "2026-10-13T14:40:00Z", "ann", "");
        event["messageType"] = json!("systemEventMessage");
        let mut page = page(
            vec![
                event,
                deleted,
                msg("4", "2026-10-13T14:02:00Z", "ann", "and more"),
                msg("3", "2026-10-13T14:00:00Z", "ann", "a &amp; b"),
                msg("2", "2026-10-12T09:00:00Z", "me", "first"),
            ],
            Some("me"),
        );
        page.truncated = true;
        assert_eq!(
            messages(&page, &view(), "older"),
            "Older messages available. Continue with --cursor 'older'\n\n\
             ── Yesterday ──\nYou  09:00\n  first\n\n\
             ── Today ──\nANN  14:00\n  a &amp; b\n  and more\nANN  14:30\n  (message deleted)"
        );
    }

    #[test]
    fn wrapping_respects_width_and_quote_prefix() {
        assert_eq!(wrap("aaa bbb ccc ddd", 11), ["aaa bbb ccc", "ddd"]);
        assert_eq!(wrap("> aaaa bbbb cccc", 12), ["> aaaa bbbb", "> cccc"]);
        assert_eq!(wrap("abcdefghijklmn", 10), ["abcdefghij", "klmn"]);
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
