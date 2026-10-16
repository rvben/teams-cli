//! Existing-endpoint diagnostics, chat reading through the Teams client's own cache, and
//! sending plain-text chat messages through the Teams window's own client. The WSL TUI
//! can restart Teams with explicit confirmation; evaluates only the fixed scripts in `desktop.js` and `desktop_send.js`.
//!
//! On Windows the CLI verifies the endpoint and reads in-process. Under WSL it drives the
//! Windows build of itself as a long-lived helper (`teams.exe desktop-helper`).
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
mod avatars;
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
mod cdp;
pub mod connection;
pub mod helper;
#[cfg(not(windows))]
pub mod install;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(any(windows, test))]
mod owner;
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
pub mod session;
#[cfg(windows)]
pub mod windows;

use crate::config::{DesktopAccount, DesktopClientKind, Profile};
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod browser;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod browser_core;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod browser_install;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod browser_setup;
use crate::{error::AppError, graph::Page};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
const MAX_METADATA: usize = 1024 * 1024;
const MAX_READ: usize = 8 * 1024 * 1024;
/// Teams' own limit for one chat message.
pub const MAX_BODY: usize = 28_000;

/// A read-only startup check owned by the inbox; dropping it cancels the task.
pub struct ConnectionWatch {
    pub(crate) receiver: std::sync::mpsc::Receiver<Result<(), AppError>>,
    pub(crate) task: tokio::task::JoinHandle<()>,
}

impl ConnectionWatch {
    pub(crate) fn result(&self) -> Option<Result<(), AppError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(AppError::Desktop(
                "The Teams startup check stopped unexpectedly. Reopen the inbox to retry; your browser and sign-in were preserved.".into(),
            ))),
        }
    }
}

impl Drop for ConnectionWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn messaging_unsupported() -> AppError {
    AppError::Unsupported("the experimental desktop backend reads chats and chat messages and sends plain-text chat messages only (macOS/Windows/WSL); teams, channels and identity need a Graph profile".into())
}

fn account_selection_error() -> AppError {
    AppError::Desktop("Multiple Teams accounts are available; use `init --backend desktop --desktop-account personal` or `--desktop-account work-school` to select a surface. Multiple cached accounts within that surface are still unsupported".into())
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
                "No endpoint on this port: Teams is not running with its debugging port open. `teams desktop connect` opens it by restarting Teams after confirmation (it ends any call in progress)."
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
    /// Executable names of the applications hosting refused listeners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_listeners: Vec<String>,
    /// The user-wide WebView2 arguments open a debugging port in every WebView2 app.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_debugging_flags: bool,
}

impl ConnectionMetadata {
    #[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
    fn empty(port: u16) -> Self {
        Self {
            port,
            status: ProbeStatus::NoListener,
            loopback_only: None,
            teams_owned: None,
            version: None,
            browser_websocket_advertised: false,
            targets: vec![],
            foreign_listeners: vec![],
            user_debugging_flags: false,
        }
    }
    pub fn endpoint_available(&self) -> bool {
        self.status == ProbeStatus::MetadataOk
            && self.loopback_only == Some(true)
            && self.teams_owned == Some(true)
    }

    /// The verified browser shows web pages but no Teams page: Teams was sent to sign-in.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    pub fn signed_out(&self) -> bool {
        let pages: Vec<_> = self.targets.iter().filter(|t| t.kind == "page").collect();
        self.endpoint_available()
            && pages.iter().any(|t| t.url_scheme == "https")
            && !pages.iter().any(|t| t.teams_origin)
    }

    /// The status explanation plus what is known about a refused listener.
    pub fn detail(&self) -> String {
        let mut detail = self.status.detail().to_string();
        if self.status != ProbeStatus::NonTeamsListenerRefused {
            return detail;
        }
        if !self.foreign_listeners.is_empty() {
            detail.push_str(&format!(
                " Port {} is also served by {}.",
                self.port,
                self.foreign_listeners.join(", ")
            ));
        }
        if self.user_debugging_flags {
            detail.push_str(" A user-wide WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS setting opens a debugging port in every WebView2 application. Run `teams desktop clear-flags` to remove only its debugging flags, then fully quit and reopen those applications.");
        }
        detail
    }

    /// Applications to quit and reopen after the user-wide flags are cleared.
    pub fn stray_debugging_flags(&self) -> Option<&[String]> {
        (self.status == ProbeStatus::NonTeamsListenerRefused && self.user_debugging_flags)
            .then_some(self.foreign_listeners.as_slice())
    }
}

pub struct DesktopClient {
    port: u16,
    account: DesktopAccount,
    kind: DesktopClientKind,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    browser: tokio::sync::Mutex<session::Session<browser::BrowserSystem>>,
    /// The verified page connection, reused across reads.
    #[cfg(windows)]
    session: tokio::sync::Mutex<session::Session<windows::WindowsSystem>>,
    #[cfg(target_os = "macos")]
    session: tokio::sync::Mutex<session::Session<macos::MacSystem>>,
    /// The Windows helper under WSL, started on first use.
    #[cfg(not(any(windows, target_os = "macos")))]
    helper: tokio::sync::Mutex<helper::HelperClient>,
}

impl std::fmt::Debug for DesktopClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopClient")
            .field("port", &self.port)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl DesktopClient {
    pub fn new(port: u16) -> Result<Self, AppError> {
        Self::with_account(port, DesktopAccount::Auto)
    }

    pub fn with_account(port: u16, account: DesktopAccount) -> Result<Self, AppError> {
        if port < 1024 {
            return Err(AppError::InvalidInput(
                "desktop port must be between 1024 and 65535".into(),
            ));
        }
        Ok(Self {
            port,
            account,
            kind: DesktopClientKind::Native,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            browser: tokio::sync::Mutex::new(session::Session::new(browser::BrowserSystem {
                core: false,
            })),
            #[cfg(windows)]
            session: tokio::sync::Mutex::new(session::Session::new(windows::WindowsSystem)),
            #[cfg(target_os = "macos")]
            session: tokio::sync::Mutex::new(session::Session::new(macos::MacSystem)),
            #[cfg(not(any(windows, target_os = "macos")))]
            helper: Default::default(),
        })
    }

    pub fn from_profile(profile: &Profile) -> Result<Self, AppError> {
        Self::with_client(
            profile.desktop_port,
            profile.desktop_account,
            profile.desktop_client,
        )
    }

    pub fn with_client(
        port: u16,
        account: DesktopAccount,
        kind: DesktopClientKind,
    ) -> Result<Self, AppError> {
        kind.validate_account(account)?;
        let mut client = Self::with_account(port, account)?;
        if kind.is_browser() && !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(AppError::Unsupported(
                "the browser desktop client currently requires macOS or Linux (including WSL2)"
                    .into(),
            ));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if kind == DesktopClientKind::BrowserCore {
            client.browser =
                tokio::sync::Mutex::new(session::Session::new(browser::BrowserSystem {
                    core: true,
                }));
        }
        client.kind = kind;
        Ok(client)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn browser_lease(&self) -> Result<bool, AppError> {
        self.prerequisites().map_err(|error| {
            AppError::Desktop(format!(
                "Browser prerequisites are not ready; no session access was attempted: {error}"
            ))
        })?;
        let port = self.port;
        tokio::task::spawn_blocking(move || browser::lifetime::attach(port))
            .await
            .map_err(|_| {
                AppError::Desktop("browser lease could not be established: worker stopped".into())
            })?
            .map_err(|error| {
                AppError::Desktop(format!("browser lease could not be established: {error}"))
            })
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn require_browser_lease(&self) -> Result<(), AppError> {
        if self.browser_lease().await? {
            Ok(())
        } else {
            Err(AppError::Desktop(format!(
                "No endpoint on this port: connect the browser on port {} before retrying",
                self.port
            )))
        }
    }

    pub fn debugging_setup(&self) -> Option<DebuggingSetup> {
        if self.kind.is_browser() {
            let mut setup = DebuggingSetup::for_shell("bash", self.port);
            setup.open = format!(
                "teams desktop login --desktop-client {} --port {}",
                self.kind.as_str(),
                self.port
            );
            setup.close = format!(
                "teams desktop disconnect --desktop-client {} --port {}",
                self.kind.as_str(),
                self.port
            );
            Some(setup)
        } else {
            DebuggingSetup::new(self.port)
        }
    }

    pub fn prerequisites(&self) -> Result<&'static str, AppError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            browser::prerequisites_for(self.kind == DesktopClientKind::BrowserCore)?;
            return Ok("browser_loopback");
        }
        local_prerequisites()
    }

    /// Only offered by the TUI after a verified no-listener probe on WSL.
    pub async fn needs_debugging_restart(&self) -> bool {
        self.kind == DesktopClientKind::Native
            && platform().ok() == Some("wsl_windows_helper")
            && self
                .probe()
                .await
                .is_ok_and(|probe| probe.status == ProbeStatus::NoListener)
    }

    /// Applications serving the port because of user-wide debugging flags, offered
    /// for cleanup by the TUI after a failed read.
    pub async fn stray_debugging_flags(&self) -> Option<Vec<String>> {
        if self.kind != DesktopClientKind::Native {
            return None;
        }
        let probe = self.probe().await.ok()?;
        probe.stray_debugging_flags().map(<[String]>::to_vec)
    }

    /// Called only after the person confirms the cleanup in the TUI.
    pub async fn clear_debugging_flags(&self) -> Result<(), AppError> {
        connection::clear_flags(self).await.map(|_| ())
    }

    /// Called only after the person confirms the restart in the TUI.
    pub async fn restart_with_debugging(&self) -> Result<(), AppError> {
        if !self.needs_debugging_restart().await {
            return Err(AppError::Desktop(
                "Connection changed; press r to retry before restarting Teams".into(),
            ));
        }
        connection::manage(self, connection::Action::Connect)
            .await
            .map(|_| ())
    }

    pub async fn probe(&self) -> Result<ConnectionMetadata, AppError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            self.browser_lease().await?;
            return self.browser.lock().await.probe(self.port).await;
        }
        platform()?;
        #[cfg(any(windows, target_os = "macos"))]
        return self.session.lock().await.probe(self.port).await;
        #[cfg(not(any(windows, target_os = "macos")))]
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

    /// Install the lightweight browser pair without launching or changing sign-in.
    pub async fn prepare_browser(&self, quiet: bool) -> Result<(), AppError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind == DesktopClientKind::BrowserCore {
            browser_install::ensure_with_progress(quiet).await?;
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = quiet;
        Ok(())
    }

    /// Authenticate visibly, then verify the same dedicated profile headless.
    pub async fn login(&self) -> Result<Value, AppError> {
        self.login_with_progress(false).await
    }

    pub async fn login_with_progress(&self, quiet: bool) -> Result<Value, AppError> {
        self.login_with_recovery(quiet, true).await
    }

    /// Guided setup supplies its own profile-aware recovery instructions.
    pub async fn login_for_setup(&self, quiet: bool) -> Result<Value, AppError> {
        self.login_with_recovery(quiet, false).await
    }

    async fn login_with_recovery(&self, quiet: bool, recovery: bool) -> Result<Value, AppError> {
        if !self.kind.is_browser() {
            return Err(AppError::InvalidInput(
                "desktop login requires --desktop-client browser".into(),
            ));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        return browser::login_with_recovery(
            self.port,
            self.account,
            self.kind == DesktopClientKind::BrowserCore,
            quiet,
            recovery,
        )
        .await;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = (quiet, recovery);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        Err(AppError::Unsupported(
            "browser login requires macOS or Linux (including WSL2)".into(),
        ))
    }

    pub async fn chats(&self, limit: u16, cursor: Option<&str>) -> Result<Page, AppError> {
        self.read(None, limit, cursor).await
    }

    pub async fn search_people(&self, query: &str, limit: u16) -> Result<Page, AppError> {
        crate::graph::validate_people_query(query, usize::from(limit))?;
        let value = self
            .people(json!({"op":"search","query":query.trim(),"limit":limit}))
            .await?;
        let items = value["items"]
            .as_array()
            .ok_or_else(|| AppError::Desktop("Invalid recipient-search response".into()))?
            .clone();
        Ok(Page {
            items,
            next_cursor: None,
            next_offset: None,
            truncated: value["truncated"] == true,
            source: Some("desktop_people"),
            viewer: None,
        })
    }

    pub async fn create_chat(&self, recipient: &str) -> Result<Value, AppError> {
        check_recipient(recipient)?;
        let value = self
            .people(json!({"op":"create","recipient":recipient}))
            .await?;
        if !value["id"].as_str().is_some_and(chat_id) {
            return Err(AppError::Unconfirmed("Teams may have created the conversation; check Teams before trying again. No message was sent".into()));
        }
        Ok(value)
    }

    async fn people(&self, mut request: Value) -> Result<Value, AppError> {
        let create = request["op"] == "create";
        request["port"] = json!(self.port);
        request["account_type"] = json!(self.account);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            self.require_browser_lease().await?;
            return people_result(&self.browser.lock().await.people(&request).await?, create);
        }
        platform()?;
        #[cfg(any(windows, target_os = "macos"))]
        let reply = self.session.lock().await.people(&request).await?;
        #[cfg(not(any(windows, target_os = "macos")))]
        let reply = {
            let mut helper = self.helper.lock().await;
            let op = json!({"op":"people","request":request});
            if create {
                helper.call_once(op).await?
            } else {
                helper.call(op).await?
            }
        };
        people_result(&reply, create)
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
        let mut request = read_request(self.port, chat, limit, cursor)?;
        request["account_type"] = json!(self.account);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            self.require_browser_lease().await?;
            let mut browser = self.browser.lock().await;
            return match read_page(&browser.read(&request).await?) {
                Ok(page) => Ok(page),
                Err(error) => {
                    let hint = format!(
                        "Sign in again: `teams desktop login --desktop-client {} --port {}`.",
                        self.kind.as_str(),
                        self.port
                    );
                    let signed_out = browser
                        .probe(self.port)
                        .await
                        .is_ok_and(|metadata| metadata.signed_out());
                    Err(if signed_out {
                        AppError::Desktop(format!(
                            "Teams in the dedicated browser was sent back to Microsoft sign-in. {hint}"
                        ))
                    } else {
                        error.with_hint(&hint)
                    })
                }
            };
        }
        platform()?;
        #[cfg(any(windows, target_os = "macos"))]
        let reply = self.session.lock().await.read(&request).await?;
        #[cfg(not(any(windows, target_os = "macos")))]
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
        self.send_with_id(chat, body, &crate::delivery::client_message_id())
            .await
    }

    /// Uses one preallocated correlation ID. This is not an idempotency/retry key.
    pub async fn send_with_id(&self, chat: &str, body: &str, id: &str) -> Result<Value, AppError> {
        let mut request = send_request(self.port, chat, body)?;
        if !crate::delivery::valid_client_message_id(id) {
            return Err(AppError::InvalidInput("invalid client message ID".into()));
        }
        request["client_message_id"] = json!(id);
        request["account_type"] = json!(self.account);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            self.require_browser_lease().await?;
            let reply = self.browser.lock().await.send(&request).await?;
            return self.send_result(&reply, chat, id);
        }
        platform()?;
        #[cfg(any(windows, target_os = "macos"))]
        let reply = self.session.lock().await.send(&request).await?;
        #[cfg(not(any(windows, target_os = "macos")))]
        let reply = self
            .helper
            .lock()
            .await
            .call_once(json!({"op": "send", "request": request}))
            .await?;
        self.send_result(&reply, chat, id)
    }

    fn send_result(&self, reply: &Value, chat: &str, id: &str) -> Result<Value, AppError> {
        sent_message(reply, chat).map_err(|error| match error {
            AppError::Unconfirmed(message) => AppError::UnconfirmedSend {
                message,
                chat_id: chat.into(),
                client_message_id: id.into(),
            },
            other => other,
        })
    }

    /// Checks only the newest 50 cached messages; absence is not proof of failure.
    pub async fn delivery(&self, chat: &str, id: &str) -> Result<Value, AppError> {
        check_chat(chat)?;
        if !crate::delivery::valid_client_message_id(id) {
            return Err(AppError::InvalidInput(
                "client message ID must contain 18 digits".into(),
            ));
        }
        let page = self.messages(chat, 50, None).await?;
        Ok(crate::delivery::reconcile(chat, id, &page.items))
    }

    /// Whether the Teams window that sends messages is reachable; sends nothing.
    pub async fn send_check(&self) -> Result<(), AppError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.kind.is_browser() {
            self.require_browser_lease().await?;
            let reply = self
                .browser
                .lock()
                .await
                .send_check(self.port, self.account)
                .await?;
            return check_send_reply(&reply);
        }
        platform()?;
        #[cfg(any(windows, target_os = "macos"))]
        let reply = self
            .session
            .lock()
            .await
            .send_check(self.port, self.account)
            .await?;
        #[cfg(not(any(windows, target_os = "macos")))]
        let reply = self
            .helper
            .lock()
            .await
            .call(json!({"op": "send_check", "port": self.port, "account_type": self.account}))
            .await?;
        check_send_reply(&reply)
    }
}

pub fn check_recipient(id: &str) -> Result<(), AppError> {
    if id.starts_with("8:") && valid_id(id) && id.len() > 2 {
        Ok(())
    } else {
        Err(AppError::InvalidInput("Select an exact recipient ID from `teams people search`; desktop recipient IDs start with 8:".into()))
    }
}

fn people_result(reply: &Value, create: bool) -> Result<Value, AppError> {
    let invalid = || {
        if create {
            AppError::Unconfirmed("Teams may have created the conversation; check Teams before trying again. No message was sent".into())
        } else {
            AppError::Desktop(
                "Recipient search failed. Check the Teams connection and try again".into(),
            )
        }
    };
    let value = if reply["error"].is_string() {
        reply.clone()
    } else {
        reply["result"]["result"]["value"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .ok_or_else(invalid)?
    };
    if let Some(code) = value["error"].as_str() {
        return Err(match code {
            "account_ambiguous" => account_selection_error(),
            "permission_denied" => AppError::Permission("Teams policy does not allow a chat with this recipient".into()),
            "search_failed" => AppError::Desktop("Teams could not resolve this person. Check sign-in and try their full email address".into()),
            "layout" => AppError::Unsupported("This Teams version does not expose a supported recipient interface".into()),
            "unconfirmed" => invalid(),
            _ => send_error(code),
        });
    }
    Ok(value)
}

fn check_send_reply(reply: &Value) -> Result<(), AppError> {
    refused(reply)?;
    match reply["error"].as_str() {
        None if reply["ok"] == true => Ok(()),
        code => Err(send_error(code.unwrap_or_default())),
    }
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

/// A listener refusal that carries its diagnosis explains itself in full.
fn refused(reply: &Value) -> Result<(), AppError> {
    match reply
        .get("connection")
        .cloned()
        .map(serde_json::from_value::<ConnectionMetadata>)
    {
        Some(Ok(metadata)) if reply.get("error").is_some() => {
            Err(AppError::Desktop(metadata.detail()))
        }
        _ => Ok(()),
    }
}

fn send_error(code: &str) -> AppError {
    match code {
        "not_found" => AppError::NotFound("chat not found in the Teams desktop cache".into()),
        "account_ambiguous" => account_selection_error(),
        "unconfirmed" => AppError::Unconfirmed(
            "Teams did not confirm the message; it may still be delivered. Check the chat before sending again".into(),
        ),
        "layout" => AppError::Desktop(
            "The Teams window's send interface was not recognised; this Teams version is not supported for sending yet".into(),
        ),
        "no_target" => AppError::Desktop(
            "No Teams window is available on the debugging endpoint; open Teams and sign in.".into(),
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
    refused(reply)?;
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
    let mut message = json!({
        "id": id,
        "chatId": chat,
        "createdDateTime": created,
        "webUrl": null,
        "clientMessageId": label("clientMessageId"),
        "status": label("status"),
        "source": "desktop",
    });
    message["delivery_state"] = json!(crate::delivery::State::from_message(&message));
    Ok(message)
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
    refused(reply)?;
    if let Some(code) = reply.get("error").and_then(Value::as_str) {
        let status = serde_json::from_value::<ProbeStatus>(json!(code)).ok();
        if code == "account_ambiguous" {
            return Err(account_selection_error());
        }
        return Err(AppError::Desktop(match (status, code) {
            (Some(status), _) => status.detail().into(),
            (None, "no_target") => {
                "No Teams window is available on the debugging endpoint; open Teams and sign in."
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
    if value["error"] == "account_ambiguous" {
        return Err(account_selection_error());
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

/// Commands for manual setup and the confirmed WSL TUI restart. macOS uses
/// process-only environment; Windows/WSL set the flags only for the Teams launch.
#[derive(Debug, Serialize)]
pub struct DebuggingSetup {
    /// `zsh` on macOS, `wsl` for a WSL shell, `powershell` on Windows.
    pub shell: &'static str,
    pub open: String,
    pub close: String,
}

impl DebuggingSetup {
    pub fn new(port: u16) -> Option<Self> {
        let shell = match platform().ok()? {
            "wsl_windows_helper" => "wsl",
            "windows_native" => "powershell",
            "macos_loopback" => "zsh",
            _ => return None,
        };
        Some(Self::for_shell(shell, port))
    }

    fn for_shell(shell: &'static str, port: u16) -> Self {
        Self {
            shell,
            open: format!("teams desktop connect --port {port}"),
            close: format!("teams desktop disconnect --port {port}"),
        }
    }

    pub fn text(&self) -> String {
        if self.shell == "bash" {
            return format!(
                "Sign in once, then continue with a hidden Teams browser:\n  {}\nThe window closes automatically after sign-in is verified. Close the hidden browser:\n  {}\nThe sign-in profile is retained. While its debugging port is open, local programs can read chats and send as you.",
                self.open, self.close
            );
        }
        let shell = match self.shell {
            "wsl" => "this WSL shell",
            "zsh" => "Terminal (zsh or bash)",
            _ => "PowerShell",
        };
        format!(
            "To open Teams' debugging port, run this in {shell}. It restarts Teams, which ends any call in progress:\n  {}\nTo close it again:\n  {}\nThe flags apply only to that Teams launch; other apps and your settings are unchanged. If setup is interrupted, run `teams desktop recover`. While the port is open, any program running as you can read your chats and send as you.",
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

/// Reads endpoint metadata into `metadata` for an already-verified listener and returns
/// the raw targets, which stay in-process.
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
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

#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
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
    use super::macos::{loopback_address, parse_listeners};
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
    fn debugging_setup_uses_managed_commands_on_every_shell() {
        for shell in ["wsl", "powershell", "zsh"] {
            let setup = DebuggingSetup::for_shell(shell, 9333);
            assert_eq!(setup.open, "teams desktop connect --port 9333");
            assert_eq!(setup.close, "teams desktop disconnect --port 9333");
            assert!(setup.text().contains("teams desktop recover"));
            assert!(setup.text().contains("any program running as you"));
        }
    }

    /// Helpers and CLIs of different versions talk to each other whenever their protocol
    /// numbers match, so everything that crosses between them is pinned here: the
    /// requests the CLI builds and the page scripts whose results it parses. When this
    /// fails, bump `helper::PROTOCOL` if a helper or CLI of the old protocol would misread
    /// the new one; otherwise just update the expectations.
    #[test]
    fn the_protocol_is_pinned() {
        assert_eq!(helper::PROTOCOL, 4);
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
        let scripts = [
            include_str!("desktop.js"),
            include_str!("desktop_send.js"),
            include_str!("desktop_cache.js"),
            include_str!("desktop_people.js"),
        ]
        .map(|script| {
            use sha2::Digest;
            // Line endings depend on the checkout, not on the script.
            let digest = sha2::Sha256::digest(script.replace('\r', ""));
            digest[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
        // Protocol 4 adds recipient operations; existing sends retain their request shapes.
        assert_eq!(
            scripts,
            [
                "32ca0293d2d5",
                "baa0bc29c44b",
                "3e837cc21336",
                "3d628a24b464"
            ]
        );
    }

    #[test]
    fn recipient_scripts_keep_search_readonly_and_creation_once_only() {
        let output = std::process::Command::new("node")
            .args(["--test", "tests/js/people.test.mjs"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("Node.js 20 or newer is required for desktop script tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn recipient_replies_preserve_uncertainty_and_account_errors() {
        assert!(matches!(
            people_result(&json!({"error":"unconfirmed"}), true),
            Err(AppError::Unconfirmed(_))
        ));
        assert!(matches!(
            people_result(&json!({"result":{}}), true),
            Err(AppError::Unconfirmed(_))
        ));
        assert!(matches!(
            people_result(&page_reply(json!({"error":"permission_denied"})), true),
            Err(AppError::Permission(_))
        ));
        assert!(matches!(
            people_result(&page_reply(json!({"error":"account_ambiguous"})), false),
            Err(AppError::Desktop(_))
        ));
        for id in [
            "someone@example.invalid",
            "8:bad id",
            "8:",
            "8:bad/extra",
            "",
        ] {
            assert!(check_recipient(id).is_err());
        }
        assert!(check_recipient("8:live:.cid.012345").is_ok());
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
        for ambiguous in [
            json!({"error": "account_ambiguous"}),
            reply(json!({"error": "account_ambiguous"})),
        ] {
            assert!(
                matches!(read_page(&ambiguous), Err(AppError::Desktop(m)) if m.contains("--desktop-account"))
            );
            assert!(
                matches!(sent_message(&ambiguous, CHAT), Err(AppError::Desktop(m)) if m.contains("--desktop-account"))
            );
        }
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

    #[test]
    fn sign_out_means_web_pages_without_a_teams_page() {
        let metadata = |pages: &[(&str, &str)]| {
            let mut metadata = ConnectionMetadata::empty(9224);
            metadata.status = ProbeStatus::MetadataOk;
            metadata.loopback_only = Some(true);
            metadata.teams_owned = Some(true);
            metadata.targets = pages
                .iter()
                .map(|(kind, url)| sanitize_target(&json!({"type":kind,"url":url})))
                .collect();
            metadata
        };
        let login = ("page", "https://login.microsoftonline.com/common/oauth2");
        let federated = ("page", "https://adfs.example.com/adfs/ls/wia");
        let teams = ("page", "https://teams.cloud.microsoft/");
        let worker = ("service_worker", "https://teams.cloud.microsoft/sw.js");
        assert!(metadata(&[login, worker]).signed_out());
        assert!(metadata(&[federated]).signed_out());
        assert!(!metadata(&[teams, worker]).signed_out());
        assert!(!metadata(&[teams, login]).signed_out());
        // A starting browser or the lightweight client's blank page is not a sign-out.
        assert!(!metadata(&[]).signed_out());
        assert!(!metadata(&[("page", "about:blank")]).signed_out());
        let mut refused = metadata(&[login]);
        refused.teams_owned = Some(false);
        assert!(!refused.signed_out());
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
