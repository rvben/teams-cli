//! Existing-endpoint diagnostics, chat reading through the Teams client's own cache, and
//! sending plain-text chat messages through the Teams window's own client. Never launches
//! Teams; evaluates only the fixed scripts in `desktop.js` and `desktop_send.js`.
//!
//! On Windows the CLI verifies the endpoint and reads in-process. Under WSL it drives the
//! Windows build of itself as a long-lived helper (`teams.exe desktop-helper`).
#[cfg(any(windows, test))]
mod cdp;
pub mod helper;
#[cfg(not(windows))]
pub mod install;
#[cfg(any(windows, test))]
mod owner;
#[cfg(any(windows, test))]
pub mod session;
#[cfg(windows)]
pub mod windows;

use crate::{error::AppError, graph::Page};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(any(target_os = "macos", windows, test))]
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::{path::PathBuf, process::Stdio};
#[cfg(target_os = "macos")]
use tokio::process::Command;

#[cfg(target_os = "macos")]
const PROCESS_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(any(target_os = "macos", windows, test))]
const MAX_METADATA: usize = 1024 * 1024;
const MAX_READ: usize = 8 * 1024 * 1024;
/// Teams' own limit for one chat message.
pub const MAX_BODY: usize = 28_000;

pub fn messaging_unsupported() -> AppError {
    AppError::Unsupported("the experimental desktop backend reads chats and chat messages and sends plain-text chat messages only (Windows/WSL); teams, channels and identity need a Graph profile".into())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    NoListener,
    UnsafeListenerRefused,
    NonTeamsListenerRefused,
    MetadataProbeFailed,
    MetadataOk,
}

impl ProbeStatus {
    pub fn detail(self) -> &'static str {
        match self {
            Self::NoListener => {
                "No endpoint on this port: Teams is not running with its debugging port open. `teams doctor` shows the commands to open it; the CLI never changes Teams' settings or restarts it."
            }
            Self::UnsafeListenerRefused => "Refused a listener bound beyond loopback.",
            Self::NonTeamsListenerRefused => {
                "Refused a listener not owned by this user's installed Teams application."
            }
            Self::MetadataProbeFailed => {
                "Desktop metadata could not be verified; no session access was attempted."
            }
            Self::MetadataOk => {
                "Teams connection metadata is available. Sign-in and chat access remain unverified."
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BrowserVersion {
    pub browser: String,
    pub protocol: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TargetMetadata {
    #[serde(rename = "type")]
    pub kind: String,
    pub url_scheme: String,
    pub url_origin: String,
    pub teams_origin: bool,
    pub websocket_advertised: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionMetadata {
    pub port: u16,
    pub status: ProbeStatus,
    pub loopback_only: Option<bool>,
    pub teams_owned: Option<bool>,
    pub version: Option<BrowserVersion>,
    pub browser_websocket_advertised: bool,
    pub targets: Vec<TargetMetadata>,
}

impl ConnectionMetadata {
    #[cfg(any(target_os = "macos", windows, test))]
    fn empty(port: u16) -> Self {
        Self {
            port,
            status: ProbeStatus::NoListener,
            loopback_only: None,
            teams_owned: None,
            version: None,
            browser_websocket_advertised: false,
            targets: vec![],
        }
    }
    pub fn endpoint_available(&self) -> bool {
        self.status == ProbeStatus::MetadataOk
            && self.loopback_only == Some(true)
            && self.teams_owned == Some(true)
    }
}

pub struct DesktopClient {
    port: u16,
    /// The verified page connection, reused across reads.
    #[cfg(windows)]
    session: tokio::sync::Mutex<session::Session<windows::WindowsSystem>>,
    /// The Windows helper under WSL, started on first use.
    #[cfg(not(windows))]
    helper: tokio::sync::Mutex<helper::HelperClient>,
}

impl std::fmt::Debug for DesktopClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopClient")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl DesktopClient {
    pub fn new(port: u16) -> Result<Self, AppError> {
        if port < 1024 {
            return Err(AppError::InvalidInput(
                "desktop port must be between 1024 and 65535".into(),
            ));
        }
        Ok(Self {
            port,
            #[cfg(windows)]
            session: tokio::sync::Mutex::new(session::Session::new(windows::WindowsSystem)),
            #[cfg(not(windows))]
            helper: Default::default(),
        })
    }

    pub async fn probe(&self) -> Result<ConnectionMetadata, AppError> {
        let transport = platform()?;
        if transport == "macos_loopback" {
            #[cfg(target_os = "macos")]
            return tokio::time::timeout(PROCESS_TIMEOUT, self.probe_macos())
                .await
                .map_err(|_| AppError::Desktop("desktop metadata inspection timed out".into()))?;
        }
        #[cfg(windows)]
        return self.session.lock().await.probe(self.port).await;
        #[cfg(not(windows))]
        {
            let metadata = self
                .helper
                .lock()
                .await
                .call(json!({"op": "probe", "port": self.port}))
                .await?;
            serde_json::from_value(metadata)
                .map_err(|_| AppError::Desktop("Windows helper returned invalid metadata".into()))
        }
    }

    pub async fn chats(&self, limit: u16, cursor: Option<&str>) -> Result<Page, AppError> {
        self.read(None, limit, cursor).await
    }

    pub async fn messages(
        &self,
        chat: &str,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<Page, AppError> {
        self.read(Some(chat), limit, cursor).await
    }

    async fn read(
        &self,
        chat: Option<&str>,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<Page, AppError> {
        let request = read_request(self.port, chat, limit, cursor)?;
        if platform()? == "macos_loopback" {
            return Err(AppError::Unsupported(
                "desktop chat reading is available on Windows and WSL only".into(),
            ));
        }
        #[cfg(windows)]
        let reply = self.session.lock().await.read(&request).await?;
        #[cfg(not(windows))]
        let reply = self
            .helper
            .lock()
            .await
            .call(json!({"op": "read", "request": request}))
            .await?;
        read_page(&reply)
    }

    /// Sends one plain-text chat message as the signed-in Teams user. Never retried: a lost
    /// answer is reported as unconfirmed, because Teams may already have the message.
    pub async fn send(&self, chat: &str, body: &str) -> Result<Value, AppError> {
        let request = send_request(self.port, chat, body)?;
        desktop_messaging()?;
        #[cfg(windows)]
        let reply = self.session.lock().await.send(&request).await?;
        #[cfg(not(windows))]
        let reply = self
            .helper
            .lock()
            .await
            .call_once(json!({"op": "send", "request": request}))
            .await?;
        sent_message(&reply, chat)
    }

    /// Whether the Teams window that sends messages is reachable; sends nothing.
    pub async fn send_check(&self) -> Result<(), AppError> {
        desktop_messaging()?;
        #[cfg(windows)]
        let reply = self.session.lock().await.send_check(self.port).await?;
        #[cfg(not(windows))]
        let reply = self
            .helper
            .lock()
            .await
            .call(json!({"op": "send_check", "port": self.port}))
            .await?;
        match reply["error"].as_str() {
            None if reply["ok"] == true => Ok(()),
            code => Err(send_error(code.unwrap_or_default())),
        }
    }
}

fn desktop_messaging() -> Result<(), AppError> {
    if platform()? == "macos_loopback" {
        return Err(AppError::Unsupported(
            "desktop chat sending is available on Windows and WSL only".into(),
        ));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    (1..=256).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:@-".contains(&b))
}

fn read_request(
    port: u16,
    chat: Option<&str>,
    limit: u16,
    cursor: Option<&str>,
) -> Result<Value, AppError> {
    if let Some(chat) = chat {
        check_chat(chat)?;
    }
    let after = cursor.map(decode_cursor).transpose()?;
    Ok(json!({
        "operation": "read",
        "port": port,
        "query": if chat.is_some() { "messages" } else { "chats" },
        "chat_id": chat,
        "limit": limit,
        "after": after,
    }))
}

/// A chat id as `teams chats` lists them; `48:notes` is the chat with yourself.
fn chat_id(id: &str) -> bool {
    id == NOTES || id.starts_with("19:") && valid_id(id)
}

const NOTES: &str = "48:notes";

/// Refuses anything but a chat id as `teams chats` lists them.
pub fn check_chat(chat: &str) -> Result<(), AppError> {
    if chat_id(chat) {
        Ok(())
    } else {
        Err(AppError::InvalidInput(
            "desktop chat ids look like `19:...@thread.v2`, `19:...@unq.gbl.spaces` or `48:notes`"
                .into(),
        ))
    }
}

/// Plain text Teams accepts as one message.
fn valid_body(body: &str) -> bool {
    !body.trim().is_empty() && body.chars().count() <= MAX_BODY && !body.contains('\0')
}

/// A send request with a fresh client message id, which Teams uses to recognise the message.
fn send_request(port: u16, chat: &str, body: &str) -> Result<Value, AppError> {
    check_chat(chat)?;
    if !valid_body(body) {
        return Err(AppError::InvalidInput(format!(
            "message body must be non-empty plain text of at most {MAX_BODY} characters"
        )));
    }
    let id: u64 = rand::Rng::random_range(
        &mut rand::rng(),
        100_000_000_000_000_000..1_000_000_000_000_000_000,
    );
    Ok(json!({
        "operation": "send",
        "port": port,
        "chat_id": chat,
        "body": body,
        "client_message_id": id.to_string(),
    }))
}

fn send_error(code: &str) -> AppError {
    match code {
        "not_found" => AppError::NotFound("chat not found in the Teams desktop cache".into()),
        "unconfirmed" => AppError::Unconfirmed(
            "Teams did not confirm the message; it may still be delivered. Check the chat before sending again".into(),
        ),
        "layout" => AppError::Desktop(
            "The Teams window's send interface was not recognised; this Teams version is not supported for sending yet".into(),
        ),
        "no_target" => AppError::Desktop(
            "No Teams window is available on the debugging endpoint; open Teams and sign in".into(),
        ),
        code => match serde_json::from_value::<ProbeStatus>(json!(code)) {
            Ok(status) => AppError::Desktop(status.detail().into()),
            Err(_) => AppError::Desktop(
                "The message was not sent; the Teams page could not be reached. Run `teams doctor`".into(),
            ),
        },
    }
}

/// Maps a send reply to the Graph send shape, plus Teams' own status label.
fn sent_message(reply: &Value, chat: &str) -> Result<Value, AppError> {
    if let Some(code) = reply.get("error").and_then(Value::as_str) {
        return Err(send_error(code));
    }
    let result = &reply["result"];
    if let Some(details) = result.get("exceptionDetails") {
        // The script reports every failure after handing the message to Teams itself, so an
        // exception means nothing was sent.
        let layout = details["exception"]["description"]
            .as_str()
            .is_some_and(|d| d.starts_with("Error: layout"));
        return Err(send_error(if layout { "layout" } else { "" }));
    }
    let value: Value = result["result"]["value"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .ok_or_else(|| send_error("unconfirmed"))?;
    if let Some(code) = value["error"].as_str() {
        return Err(send_error(code));
    }
    let id = value["id"]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or_else(|| send_error("unconfirmed"))?;
    let label = |key: &str| {
        value[key]
            .as_str()
            .filter(|v| v.len() <= 64 && valid_id(v))
            .map(String::from)
    };
    let arrival = &value["originalArrivalTime"];
    let created = arrival
        .as_i64()
        .or_else(|| arrival.as_str().and_then(|t| t.parse().ok()))
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .or_else(|| arrival.as_str().and_then(|t| t.parse().ok()))
        .map(|t| t.to_string());
    Ok(json!({
        "id": id,
        "chatId": chat,
        "createdDateTime": created,
        "webUrl": null,
        "clientMessageId": label("clientMessageId"),
        "status": label("status"),
        "source": "desktop",
    }))
}

/// Cursors are opaque to callers: base64 of the last item's sort key.
fn encode_cursor(next: &Value) -> Result<String, AppError> {
    let t = next.get("t").and_then(Value::as_i64);
    let id = next.get("id").and_then(Value::as_str);
    match (t, id) {
        (Some(t), Some(id)) if valid_id(id) => {
            Ok(URL_SAFE_NO_PAD.encode(json!({"t":t,"id":id}).to_string()))
        }
        _ => Err(AppError::Desktop(
            "Teams cache returned an invalid page position".into(),
        )),
    }
}

fn decode_cursor(cursor: &str) -> Result<Value, AppError> {
    let invalid = || AppError::InvalidInput("invalid desktop cursor".into());
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| invalid())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let t = value.get("t").and_then(Value::as_i64).ok_or_else(invalid)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if t < 0 || !valid_id(id) {
        return Err(invalid());
    }
    Ok(json!({"t":t,"id":id}))
}

/// Maps a session reply: a refusal code, or the raw `Runtime.evaluate` result.
fn read_page(reply: &Value) -> Result<Page, AppError> {
    let invalid = || AppError::Desktop("the Teams page returned an invalid chat response".into());
    if let Some(code) = reply.get("error").and_then(Value::as_str) {
        let status = serde_json::from_value::<ProbeStatus>(json!(code)).ok();
        return Err(AppError::Desktop(match (status, code) {
            (Some(status), _) => status.detail().into(),
            (None, "no_target") => {
                "No Teams window is available on the debugging endpoint; open Teams and sign in"
                    .into()
            }
            _ => "The Teams page could not be read over the debugging endpoint; run `teams doctor`"
                .into(),
        }));
    }
    let result = &reply["result"];
    if let Some(details) = result.get("exceptionDetails") {
        let layout = details["exception"]["description"]
            .as_str()
            .is_some_and(|d| d.starts_with("Error: layout"));
        return Err(AppError::Desktop(if layout {
            "The Teams cache layout was not recognised; this Teams version is not supported yet"
                .into()
        } else {
            "The Teams page could not read its local cache; run `teams doctor`".into()
        }));
    }
    let value: Value = result["result"]["value"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .ok_or_else(invalid)?;
    if value["error"] == "not_found" {
        return Err(AppError::NotFound(
            "chat not found in the Teams desktop cache".into(),
        ));
    }
    let items = value["items"].as_array().ok_or_else(invalid)?.clone();
    let next_cursor = match &value["next"] {
        Value::Null => None,
        next => Some(encode_cursor(next)?),
    };
    Ok(Page {
        items,
        truncated: next_cursor.is_some(),
        next_cursor,
        next_offset: None,
        source: Some("desktop_cache"),
        viewer: value["me"]
            .as_str()
            .filter(|me| valid_id(me))
            .map(String::from),
    })
}

pub fn platform() -> Result<&'static str, AppError> {
    if cfg!(target_os = "macos") {
        Ok("macos_loopback")
    } else if cfg!(windows) {
        Ok("windows_native")
    } else if cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("microsoft")
    {
        Ok("wsl_windows_helper")
    } else {
        Err(AppError::Unsupported(
            "desktop diagnostics require macOS, Windows, or WSL with Windows executable interop"
                .into(),
        ))
    }
}

/// Commands that open and close Teams' debugging port on Windows, for people to run
/// themselves: the CLI never changes Teams' settings or restarts Teams. Both set the
/// user variable WebView2 reads at start, then restart Teams so it takes effect.
#[derive(Debug, Serialize)]
pub struct DebuggingSetup {
    /// `wsl` for a WSL shell, `powershell` for Windows PowerShell.
    pub shell: &'static str,
    pub open: String,
    pub close: String,
}

impl DebuggingSetup {
    pub fn new(port: u16) -> Option<Self> {
        let shell = match platform().ok()? {
            "wsl_windows_helper" => "wsl",
            "windows_native" => "powershell",
            _ => return None,
        };
        Some(Self::for_shell(shell, port))
    }

    fn for_shell(shell: &'static str, port: u16) -> Self {
        // Only double quotes inside, so the WSL form can wrap it in single quotes.
        let command = |value: &str| {
            let code = format!(
                r#"[Environment]::SetEnvironmentVariable("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", {value}, "User"); Get-Process ms-teams -ErrorAction SilentlyContinue | Stop-Process; Start-Sleep 2; Start-Process "shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams""#
            );
            match shell {
                "wsl" => format!("powershell.exe -NoProfile -Command '{code}'"),
                _ => code,
            }
        };
        Self {
            shell,
            open: command(&format!(r#""--remote-debugging-port={port}""#)),
            close: command("$null"),
        }
    }

    pub fn text(&self) -> String {
        let shell = match self.shell {
            "wsl" => "this WSL shell",
            _ => "PowerShell",
        };
        format!(
            "To open Teams' debugging port, run this in {shell}. It restarts Teams, which ends any call in progress:\n  {}\nTo close it again:\n  {}\nWhile the port is open, any program running as you can read your chats and send as you.",
            self.open, self.close
        )
    }
}

/// The hidden `desktop-helper` command: answers the Linux CLI under WSL until stdin closes.
pub async fn serve_helper() -> Result<(), AppError> {
    #[cfg(windows)]
    {
        helper::serve(
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            &mut session::Session::new(windows::WindowsSystem),
        )
        .await
    }
    #[cfg(not(windows))]
    Err(AppError::Unsupported(
        "the desktop helper runs only on Windows".into(),
    ))
}

/// The WSL helper's install state for `doctor` as (status, detail); `None` where no
/// helper is involved. Never downloads.
pub fn helper_check() -> Option<(&'static str, String)> {
    #[cfg(not(windows))]
    if platform().ok() == Some("wsl_windows_helper") {
        return Some(match install::state() {
            Ok(install::HelperState::Override(path)) if !path.is_file() => (
                "fail",
                format!(
                    "TEAMS_DESKTOP_HELPER points to {}, which does not exist",
                    path.display()
                ),
            ),
            Ok(state @ install::HelperState::Missing(_)) => ("skip", state.describe()),
            Ok(state) => ("pass", state.describe()),
            Err(error) => ("fail", error.to_string()),
        });
    }
    None
}

pub fn local_prerequisites() -> Result<&'static str, AppError> {
    let transport = platform()?;
    if transport == "macos_loopback" {
        if !std::path::Path::new("/usr/sbin/lsof").is_file() {
            return Err(AppError::Desktop(
                "lsof is required for Teams listener ownership checks".into(),
            ));
        }
    } else if transport == "wsl_windows_helper"
        && !["WSLInterop", "WSLInterop-late"].iter().any(|name| {
            std::path::Path::new("/proc/sys/fs/binfmt_misc")
                .join(name)
                .exists()
        })
    {
        return Err(AppError::Desktop(
            "WSL cannot run Windows executables; enable interop in /etc/wsl.conf".into(),
        ));
    }
    Ok(transport)
}

#[cfg(target_os = "macos")]
async fn run_process(mut command: Command) -> Result<std::process::Output, AppError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|_| AppError::Desktop("could not start the local metadata helper".into()))?;
    tokio::time::timeout(PROCESS_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| AppError::Desktop("metadata helper timed out".into()))?
        .map_err(|_| AppError::Desktop("could not read the metadata helper result".into()))
}

#[cfg(target_os = "macos")]
impl DesktopClient {
    async fn probe_macos(&self) -> Result<ConnectionMetadata, AppError> {
        local_prerequisites()?;
        let mut command = Command::new("/usr/sbin/lsof");
        command.args([
            "-nP",
            &format!("-iTCP:{}", self.port),
            "-sTCP:LISTEN",
            "-Fpn",
        ]);
        let output = run_process(command).await?;
        let text = String::from_utf8_lossy(&output.stdout);
        let listeners = parse_listeners(&text)?;
        let mut metadata = ConnectionMetadata::empty(self.port);
        if listeners.is_empty() {
            if output.status.code() != Some(1) {
                return Err(AppError::Desktop("listener inspection failed".into()));
            }
            return Ok(metadata);
        }
        if !output.status.success() {
            return Err(AppError::Desktop("listener inspection failed".into()));
        }
        metadata.loopback_only = Some(
            listeners
                .iter()
                .all(|(_, address)| loopback_address(address, self.port)),
        );
        if metadata.loopback_only != Some(true) {
            metadata.status = ProbeStatus::UnsafeListenerRefused;
            return Ok(metadata);
        }
        let mut uid_command = Command::new("/usr/bin/id");
        uid_command.arg("-u");
        let uid_output = run_process(uid_command).await?;
        let uid = String::from_utf8_lossy(&uid_output.stdout)
            .trim()
            .parse::<u32>()
            .map_err(|_| AppError::Desktop("could not verify the desktop process owner".into()))?;
        let mut owned = true;
        for (pid, _) in listeners {
            if !teams_owner(pid, uid).await? {
                owned = false;
                break;
            }
        }
        metadata.teams_owned = Some(owned);
        if !owned {
            metadata.status = ProbeStatus::NonTeamsListenerRefused;
            return Ok(metadata);
        }
        fill_metadata(&mut metadata).await;
        Ok(metadata)
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_listeners(text: &str) -> Result<Vec<(u32, String)>, AppError> {
    let mut pid = None;
    let mut listeners = vec![];
    for line in text.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = Some(
                value
                    .parse()
                    .map_err(|_| AppError::Desktop("invalid listener metadata".into()))?,
            );
        } else if let Some(address) = line.strip_prefix('n') {
            listeners.push((
                pid.ok_or_else(|| AppError::Desktop("listener ownership missing".into()))?,
                address.into(),
            ));
        }
    }
    Ok(listeners)
}

#[cfg(any(target_os = "macos", test))]
fn loopback_address(address: &str, port: u16) -> bool {
    address == format!("127.0.0.1:{port}") || address == format!("[::1]:{port}")
}

#[cfg(target_os = "macos")]
async fn teams_owner(mut pid: u32, uid: u32) -> Result<bool, AppError> {
    let mut expected = vec![PathBuf::from(
        "/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams",
    )];
    if let Some(home) = dirs::home_dir() {
        expected.push(home.join("Applications/Microsoft Teams.app/Contents/MacOS/MSTeams"));
    }
    let mut seen = std::collections::HashSet::new();
    for _ in 0..32 {
        if pid == 0 || !seen.insert(pid) {
            break;
        }
        let mut command = Command::new("/bin/ps");
        command.args(["-p", &pid.to_string(), "-o", "uid=,ppid=,comm="]);
        let output = run_process(command).await?;
        if !output.status.success() {
            return Ok(false);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // ps pads columns with several spaces; executable paths can contain spaces.
        let process_uid = text
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u32>().ok());
        if process_uid != Some(uid) {
            return Ok(false);
        }
        let rest = text
            .trim()
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim_start())
            .unwrap_or("");
        let Some((parent, executable)) = rest.split_once(char::is_whitespace) else {
            return Ok(false);
        };
        let path = PathBuf::from(executable.trim_start());
        if expected.iter().any(|e| e == &path && e.is_file()) {
            return Ok(process_uid == Some(uid));
        }
        pid = parent
            .parse()
            .map_err(|_| AppError::Desktop("invalid process ancestry metadata".into()))?;
    }
    Ok(false)
}

/// Reads endpoint metadata into `metadata` for an already-verified listener and returns
/// the raw targets, which stay in-process.
#[cfg(any(target_os = "macos", windows, test))]
async fn fill_metadata(metadata: &mut ConnectionMetadata) -> Vec<Value> {
    let Ok((version, targets)) = read_metadata(metadata.port).await else {
        metadata.status = ProbeStatus::MetadataProbeFailed;
        return vec![];
    };
    let text = |key: &str| {
        version
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    metadata.version = Some(BrowserVersion {
        browser: text("Browser"),
        protocol: text("Protocol-Version"),
    });
    metadata.browser_websocket_advertised = !text("webSocketDebuggerUrl").is_empty();
    metadata.targets = targets.iter().map(sanitize_target).collect();
    metadata.status = ProbeStatus::MetadataOk;
    targets
}

#[cfg(any(target_os = "macos", windows, test))]
async fn read_metadata(port: u16) -> Result<(Value, Vec<Value>), AppError> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .user_agent("TeamsCLI-DesktopProbe/0.1")
        .build()
        .map_err(|_| AppError::Desktop("could not create metadata reader".into()))?;
    async fn read(http: &reqwest::Client, port: u16, route: &str) -> Result<Value, AppError> {
        let mut response = http
            .get(format!("http://127.0.0.1:{port}{route}"))
            .send()
            .await
            .map_err(|_| AppError::Desktop("metadata endpoint is unavailable".into()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(AppError::Desktop(
                "metadata endpoint returned an unexpected status".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AppError::Desktop("metadata response failed".into()))?
        {
            if bytes.len() + chunk.len() > MAX_METADATA {
                return Err(AppError::Desktop("metadata size limit exceeded".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::Desktop("invalid endpoint metadata".into()))
    }
    let version = read(&http, port, "/json/version").await?;
    let targets = read(&http, port, "/json/list").await?;
    if !version.is_object() {
        return Err(AppError::Desktop("invalid browser metadata".into()));
    }
    Ok((
        version,
        targets
            .as_array()
            .ok_or_else(|| AppError::Desktop("invalid target list".into()))?
            .clone(),
    ))
}

#[cfg(any(target_os = "macos", windows, test))]
fn sanitize_target(target: &Value) -> TargetMetadata {
    let uri = target
        .get("url")
        .and_then(Value::as_str)
        .and_then(|u| url::Url::parse(u).ok());
    let scheme = uri.as_ref().map(|u| u.scheme()).unwrap_or("");
    let host = uri.as_ref().and_then(|u| u.host_str()).unwrap_or("");
    let secure =
        scheme == "https" && uri.as_ref().and_then(|u| u.port_or_known_default()) == Some(443);
    let teams = secure
        && matches!(
            host,
            "teams.microsoft.com" | "teams.cloud.microsoft" | "teams.live.com"
        );
    let origin =
        if teams || (secure && matches!(host, "login.microsoftonline.com" | "login.live.com")) {
            format!("https://{host}")
        } else if matches!(scheme, "about" | "blob" | "file") {
            format!("{scheme}:")
        } else {
            "[other origin omitted]".into()
        };
    TargetMetadata {
        kind: target
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("other")
            .into(),
        url_scheme: scheme.into(),
        url_origin: origin,
        teams_origin: teams,
        websocket_advertised: target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    const CHAT: &str = "19:a@thread.v2";

    fn page_reply(value: Value) -> Value {
        json!({"result": {"result": {"type": "string", "value": value.to_string()}}})
    }

    #[test]
    fn debugging_setup_sets_and_clears_the_variable_and_restarts_teams() {
        let wsl = DebuggingSetup::for_shell("wsl", 9333);
        assert_eq!(
            wsl.open,
            r#"powershell.exe -NoProfile -Command '[Environment]::SetEnvironmentVariable("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--remote-debugging-port=9333", "User"); Get-Process ms-teams -ErrorAction SilentlyContinue | Stop-Process; Start-Sleep 2; Start-Process "shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams"'"#
        );
        assert!(
            wsl.close
                .contains(r#""WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", $null, "User")"#)
        );
        // The WSL form is wrapped in single quotes, so nothing inside may close them.
        for command in [&wsl.open, &wsl.close] {
            let inner = &command["powershell.exe -NoProfile -Command '".len()..command.len() - 1];
            assert!(!inner.contains('\''), "{command}");
        }
        let native = DebuggingSetup::for_shell("powershell", 9333);
        assert!(
            native
                .open
                .starts_with("[Environment]::SetEnvironmentVariable(")
        );
        assert!(native.text().contains("in PowerShell"));
        assert!(wsl.text().contains("in this WSL shell"));
        assert!(wsl.text().contains("any program running as you"));
    }

    /// Helpers and CLIs of different versions talk to each other whenever their protocol
    /// numbers match, so everything that crosses between them is pinned here: the
    /// requests the CLI builds and the page scripts whose results it parses. When this
    /// fails, bump `helper::PROTOCOL` if a helper or CLI of the old protocol would misread
    /// the new one; otherwise just update the expectations.
    #[test]
    fn the_protocol_is_pinned() {
        assert_eq!(helper::PROTOCOL, 2);
        let cursor = encode_cursor(&json!({"t": 1, "id": "m1"})).unwrap();
        assert_eq!(
            read_request(9222, Some(NOTES), 20, Some(&cursor)).unwrap(),
            json!({"operation": "read", "port": 9222, "query": "messages",
                   "chat_id": NOTES, "limit": 20, "after": {"t": 1, "id": "m1"}})
        );
        assert_eq!(
            read_request(9222, None, 20, None).unwrap(),
            json!({"operation": "read", "port": 9222, "query": "chats",
                   "chat_id": null, "limit": 20, "after": null})
        );
        let mut send = send_request(9222, NOTES, "hi").unwrap();
        assert!(
            send["client_message_id"]
                .as_str()
                .is_some_and(|id| id.len() == 18)
        );
        send["client_message_id"] = json!("<id>");
        assert_eq!(
            send,
            json!({"operation": "send", "port": 9222, "chat_id": NOTES,
                   "body": "hi", "client_message_id": "<id>"})
        );
        let scripts = [include_str!("desktop.js"), include_str!("desktop_send.js")].map(|script| {
            use sha2::Digest;
            // Line endings depend on the checkout, not on the script.
            let digest = sha2::Sha256::digest(script.replace('\r', ""));
            digest[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
        assert_eq!(scripts, ["3ee7c6f94e71", "1ab6c9523718"]);
    }

    #[test]
    fn send_requests_validate_and_carry_a_fresh_client_message_id() {
        let request = send_request(9222, CHAT, "hello\nthere").unwrap();
        assert_eq!(request["operation"], "send");
        assert_eq!(request["chat_id"], CHAT);
        assert_eq!(request["body"], "hello\nthere");
        let id = request["client_message_id"].as_str().unwrap();
        assert!(id.len() == 18 && id.bytes().all(|b| b.is_ascii_digit()));
        assert_ne!(
            send_request(9222, CHAT, "x").unwrap()["client_message_id"],
            request["client_message_id"]
        );
        for (chat, body) in [
            ("fixture", "hello"),
            ("19:a b@thread.v2", "hello"),
            ("48:notes2", "hello"),
            ("48:other", "hello"),
            (CHAT, ""),
            (CHAT, " \n\t"),
            (CHAT, "a\0b"),
            (CHAT, &"x".repeat(MAX_BODY + 1)),
        ] {
            assert!(
                matches!(
                    send_request(9222, chat, body),
                    Err(AppError::InvalidInput(_))
                ),
                "{chat:?} accepted"
            );
        }
        assert!(send_request(9222, CHAT, &"é".repeat(MAX_BODY)).is_ok());
        assert!(send_request(9222, NOTES, "hello").is_ok());
    }

    #[test]
    fn sent_messages_map_to_the_graph_shape() {
        let reply = page_reply(
            json!({"id": "1760000000000", "clientMessageId": "123456789012345678",
            "originalArrivalTime": "1760000000000", "status": "Sent"}),
        );
        let message = sent_message(&reply, CHAT).unwrap();
        assert_eq!(message["id"], "1760000000000");
        assert_eq!(message["chatId"], CHAT);
        assert_eq!(message["createdDateTime"], "2025-10-09T08:53:20Z");
        assert_eq!(message["clientMessageId"], "123456789012345678");
        assert_eq!(message["status"], "Sent");
        assert_eq!(message["source"], "desktop");
        for arrival in [json!(1760000000000_i64), json!("2025-10-09T08:53:20Z")] {
            let reply = page_reply(json!({"id": "1", "originalArrivalTime": arrival}));
            assert_eq!(
                sent_message(&reply, CHAT).unwrap()["createdDateTime"],
                "2025-10-09T08:53:20Z"
            );
        }
        let reply = page_reply(json!({"id": "1", "status": "<b>not a label</b>"}));
        let message = sent_message(&reply, CHAT).unwrap();
        assert!(message["status"].is_null() && message["createdDateTime"].is_null());
    }

    #[test]
    fn send_failures_say_whether_the_message_may_have_gone() {
        let unconfirmed = |error: AppError| error.to_string().contains("may still be delivered");
        for reply in [
            json!({"error": "unconfirmed"}),
            page_reply(json!({"error": "unconfirmed"})),
            page_reply(json!({"id": "bad id"})),
            page_reply(json!({})),
            json!({"result": {"result": {"type": "undefined"}}}),
        ] {
            assert!(
                unconfirmed(sent_message(&reply, CHAT).unwrap_err()),
                "{reply}"
            );
        }
        assert!(matches!(
            sent_message(&page_reply(json!({"error": "not_found"})), CHAT),
            Err(AppError::NotFound(_))
        ));
        let thrown =
            json!({"result": {"exceptionDetails": {"exception": {"description": "TypeError: x"}}}});
        let error = sent_message(&thrown, CHAT).unwrap_err();
        assert!(
            !unconfirmed(error)
                && sent_message(&thrown, CHAT)
                    .unwrap_err()
                    .to_string()
                    .contains("not sent")
        );
        for code in [
            "layout",
            "no_target",
            "non_teams_listener_refused",
            "something_else",
        ] {
            let error = sent_message(&json!({"error": code}), CHAT).unwrap_err();
            assert!(
                matches!(error, AppError::Desktop(_)) && !unconfirmed(error),
                "{code}"
            );
        }
    }

    #[test]
    fn read_requests_validate_chat_ids_and_cursors() {
        let request = read_request(9222, None, 5, None).unwrap();
        assert_eq!(request["query"], "chats");
        assert!(request["after"].is_null());
        let cursor = encode_cursor(&json!({"t": 42, "id": "19:a@thread.v2"})).unwrap();
        let request = read_request(9222, Some("19:a@thread.v2"), 5, Some(&cursor)).unwrap();
        assert_eq!(request["query"], "messages");
        assert_eq!(request["chat_id"], "19:a@thread.v2");
        assert_eq!(request["after"], json!({"t": 42, "id": "19:a@thread.v2"}));
        for chat in ["fixture", "19:a b", "19:a'); x", ""] {
            assert!(matches!(
                read_request(9222, Some(chat), 5, None),
                Err(AppError::InvalidInput(_))
            ));
        }
        let negative = URL_SAFE_NO_PAD.encode(r#"{"t":-1,"id":"x"}"#);
        for cursor in ["%%%", "e30", negative.as_str()] {
            assert!(matches!(
                read_request(9222, None, 5, Some(cursor)),
                Err(AppError::InvalidInput(_))
            ));
        }
    }

    fn reply(value: Value) -> Value {
        json!({"result":{"result":{"type":"string","value":value.to_string()}}})
    }

    #[test]
    fn read_pages_map_cache_results_and_failures() {
        let page = read_page(&reply(json!({
            "me": "0f0e-guid",
            "items": [{"id": "19:a@thread.v2", "topic": null}],
            "next": {"t": 7, "id": "19:a@thread.v2"}
        })))
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.viewer.as_deref(), Some("0f0e-guid"));
        assert!(serde_json::to_value(&page).unwrap().get("viewer").is_none());
        assert!(page.truncated);
        assert_eq!(page.source, Some("desktop_cache"));
        assert_eq!(
            decode_cursor(page.next_cursor.as_deref().unwrap()).unwrap(),
            json!({"t": 7, "id": "19:a@thread.v2"})
        );
        let last = read_page(&reply(json!({"me": "a b", "items": [], "next": null}))).unwrap();
        assert!(!last.truncated && last.next_cursor.is_none() && last.viewer.is_none());
        assert!(matches!(
            read_page(&reply(json!({"error": "not_found"}))),
            Err(AppError::NotFound(_))
        ));
        let layout = json!({"result":{"exceptionDetails":{"exception":{"description":"Error: layout\n    at x"}}}});
        assert!(matches!(read_page(&layout), Err(AppError::Desktop(m)) if m.contains("layout")));
        for reply in [
            json!({"error": "no_target"}),
            json!({"error": "non_teams_listener_refused"}),
            json!({"error": "read_failed"}),
            json!("not an object"),
            reply(json!({"items": [], "next": {"t": 1, "id": "bad id"}})),
        ] {
            assert!(matches!(read_page(&reply), Err(AppError::Desktop(_))));
        }
    }

    #[test]
    fn listener_parser_keeps_every_binding_and_refuses_non_loopback() {
        let listeners =
            parse_listeners("p12\nn127.0.0.1:9222\nn[::1]:9222\np13\nn*:9222\n").unwrap();
        assert_eq!(listeners.len(), 3);
        assert!(loopback_address(&listeners[0].1, 9222));
        assert!(loopback_address(&listeners[1].1, 9222));
        assert!(!loopback_address(&listeners[2].1, 9222));
        assert!(!loopback_address("127.0.0.1:19222", 9222));
        assert!(parse_listeners("n127.0.0.1:9222").is_err());
    }

    #[test]
    fn origins_are_exact_and_private_metadata_is_removed() {
        let cases = [
            (
                "https://teams.microsoft.com/v2/?account=PRIVATE",
                true,
                "https://teams.microsoft.com",
            ),
            (
                "https://teams.cloud.microsoft/path",
                true,
                "https://teams.cloud.microsoft",
            ),
            (
                "https://teams.live.com/PRIVATE",
                true,
                "https://teams.live.com",
            ),
            (
                "https://teams.microsoft.com.evil.example/PRIVATE",
                false,
                "[other origin omitted]",
            ),
            (
                "https://teams.microsoft.com:444/PRIVATE",
                false,
                "[other origin omitted]",
            ),
            (
                "http://teams.microsoft.com/PRIVATE",
                false,
                "[other origin omitted]",
            ),
            ("file:///Users/PRIVATE/SECRET.html", false, "file:"),
            ("blob:https://private-tenant.example/SECRET", false, "blob:"),
            (
                "https://login.microsoftonline.com/PRIVATE",
                false,
                "https://login.microsoftonline.com",
            ),
        ];
        for (url, teams, origin) in cases {
            let target = sanitize_target(
                &json!({"type":"page","url":url,"title":"PRIVATE","id":"SECRET","webSocketDebuggerUrl":"ws://127.0.0.1/SECRET"}),
            );
            assert_eq!(target.teams_origin, teams);
            assert_eq!(target.url_origin, origin);
            let output = serde_json::to_string(&target).unwrap();
            assert!(!output.contains("PRIVATE") && !output.contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn metadata_reader_uses_only_the_two_read_routes_and_generic_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/json/version"))
            .and(header("User-Agent", "TeamsCLI-DesktopProbe/0.1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"Browser":"Edg/fixture","Protocol-Version":"1.3"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/json/list"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"type":"page","url":"https://teams.live.com/PRIVATE"}])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (version, targets) = read_metadata(server.address().port()).await.unwrap();
        assert_eq!(version["Protocol-Version"], "1.3");
        assert_eq!(
            sanitize_target(&targets[0]).url_origin,
            "https://teams.live.com"
        );
        let mut connection = ConnectionMetadata::empty(server.address().port());
        connection.status = ProbeStatus::MetadataOk;
        assert!(!connection.endpoint_available());
        connection.loopback_only = Some(true);
        connection.teams_owned = Some(true);
        assert!(connection.endpoint_available());
    }

    #[tokio::test]
    async fn metadata_reader_refuses_redirects_and_oversized_responses() {
        let server = MockServer::start().await;
        let sink = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&sink)
            .await;
        Mock::given(path("/json/version"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", sink.uri()))
            .mount(&server)
            .await;
        assert!(read_metadata(server.address().port()).await.is_err());
        server.reset().await;
        Mock::given(path("/json/version"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(MAX_METADATA + 1)))
            .mount(&server)
            .await;
        assert!(read_metadata(server.address().port()).await.is_err());
    }
}
