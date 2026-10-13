//! Existing-endpoint diagnostics and read-only chat access through the Teams client's own
//! cache. Never launches Teams; evaluates only the fixed read script embedded in the helper.
use crate::{error::AppError, graph::Page};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

const SCRIPT: &str = include_str!("desktop.ps1");
const PROCESS_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_METADATA: usize = 1024 * 1024;
const MAX_READ: usize = 8 * 1024 * 1024;

pub fn messaging_unsupported() -> AppError {
    AppError::Unsupported("the experimental desktop backend reads chats and chat messages only (Windows/WSL); teams, channels, identity, sending and the live TUI need a Graph profile".into())
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
                "No endpoint on this port. Teams must already be launched with loopback debugging enabled; the CLI does not launch or restart it."
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
    #[cfg(any(target_os = "macos", test))]
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

#[derive(Debug)]
pub struct DesktopClient {
    port: u16,
}

impl DesktopClient {
    pub fn new(port: u16) -> Result<Self, AppError> {
        if port < 1024 {
            return Err(AppError::InvalidInput(
                "desktop port must be between 1024 and 65535".into(),
            ));
        }
        Ok(Self { port })
    }
    pub async fn probe(&self) -> Result<ConnectionMetadata, AppError> {
        let transport = platform()?;
        if transport == "macos_loopback" {
            #[cfg(target_os = "macos")]
            return tokio::time::timeout(PROCESS_TIMEOUT, self.probe_macos())
                .await
                .map_err(|_| AppError::Desktop("desktop metadata inspection timed out".into()))?;
        }
        self.probe_windows().await
    }

    async fn probe_windows(&self) -> Result<ConnectionMetadata, AppError> {
        let text = self
            .run_helper(json!({"operation":"probe","port":self.port}), MAX_METADATA)
            .await?;
        serde_json::from_str(&text)
            .map_err(|_| AppError::Desktop("Windows helper returned invalid metadata".into()))
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
        read_page(&self.run_helper(request, MAX_READ).await?)
    }

    async fn run_helper(&self, request: Value, limit: usize) -> Result<String, AppError> {
        let executable = powershell()?;
        let encoded = STANDARD.encode(
            helper_script()
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut command = Command::new(executable);
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ]);
        let output = run_process(command, Some(serde_json::to_vec(&request).unwrap())).await?;
        if !output.status.success() {
            return Err(AppError::Desktop("Windows desktop helper failed; check PowerShell availability and policy. No Teams lifecycle or policy settings were changed".into()));
        }
        if output.stdout.len() > limit {
            return Err(AppError::Desktop(
                "Windows helper response exceeds the size limit".into(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_start_matches('\u{feff}')
            .trim()
            .into())
    }
}

/// Comments and indentation are dropped to stay within the Windows command-line limit.
fn helper_script() -> String {
    SCRIPT
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
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
    if let Some(chat) = chat
        && !(chat.starts_with("19:") && valid_id(chat))
    {
        return Err(AppError::InvalidInput(
            "desktop chat ids look like `19:...@thread.v2` or `19:...@unq.gbl.spaces`".into(),
        ));
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

/// Maps the helper's output: a refusal code, or the raw `Runtime.evaluate` reply.
fn read_page(text: &str) -> Result<Page, AppError> {
    let invalid = || AppError::Desktop("Windows helper returned an invalid chat response".into());
    let reply: Value = serde_json::from_str(text).map_err(|_| invalid())?;
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
        Ok("windows_helper")
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

pub fn powershell() -> Result<PathBuf, AppError> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("powershell.exe")).find(|p| p.is_file())
        .ok_or_else(|| AppError::Desktop("powershell.exe was not found on PATH; enable Windows executable interop when using WSL".into()))
}

pub fn local_prerequisites() -> Result<&'static str, AppError> {
    let transport = platform()?;
    if transport == "macos_loopback" {
        if !std::path::Path::new("/usr/sbin/lsof").is_file() {
            return Err(AppError::Desktop(
                "lsof is required for Teams listener ownership checks".into(),
            ));
        }
    } else {
        powershell()?;
    }
    Ok(transport)
}

async fn run_process(
    mut command: Command,
    input: Option<Vec<u8>>,
) -> Result<std::process::Output, AppError> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| AppError::Desktop("could not start the local metadata helper".into()))?;
    let operation = async {
        if let Some(input) = input {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| AppError::Desktop("metadata helper input is unavailable".into()))?;
            stdin
                .write_all(&input)
                .await
                .map_err(|_| AppError::Desktop("could not send the metadata request".into()))?;
            drop(stdin);
        }
        child
            .wait_with_output()
            .await
            .map_err(|_| AppError::Desktop("could not read the metadata helper result".into()))
    };
    tokio::time::timeout(PROCESS_TIMEOUT, operation).await
        .map_err(|_| AppError::Desktop("metadata helper timed out; WSL may retain a Windows-side helper process. No Teams restart or session operation was requested".into()))?
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
        let output = run_process(command, None).await?;
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
        let uid_output = run_process(uid_command, None).await?;
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
        match read_metadata(self.port).await {
            Ok((version, targets)) => {
                metadata.version = Some(BrowserVersion {
                    browser: version
                        .get("Browser")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    protocol: version
                        .get("Protocol-Version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                });
                metadata.browser_websocket_advertised = version
                    .get("webSocketDebuggerUrl")
                    .and_then(Value::as_str)
                    .is_some_and(|v| !v.is_empty());
                metadata.targets = targets.iter().map(sanitize_target).collect();
                metadata.status = ProbeStatus::MetadataOk;
            }
            Err(_) => metadata.status = ProbeStatus::MetadataProbeFailed,
        }
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
        let output = run_process(command, None).await?;
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

#[cfg(any(target_os = "macos", test))]
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

#[cfg(any(target_os = "macos", test))]
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

    fn reply(value: Value) -> String {
        json!({"id":1,"result":{"result":{"type":"string","value":value.to_string()}}}).to_string()
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
        let layout = json!({"id":1,"result":{"exceptionDetails":{"exception":{"description":"Error: layout\n    at x"}}}});
        assert!(
            matches!(read_page(&layout.to_string()), Err(AppError::Desktop(m)) if m.contains("layout"))
        );
        for text in [
            r#"{"error":"no_target"}"#,
            r#"{"error":"non_teams_listener_refused"}"#,
            r#"{"error":"read_failed"}"#,
            "not json",
            &reply(json!({"items": [], "next": {"t": 1, "id": "bad id"}})),
        ] {
            assert!(matches!(read_page(text), Err(AppError::Desktop(_))));
        }
    }

    #[test]
    fn native_script_fits_windows_command_line() {
        let encoded = helper_script()
            .encode_utf16()
            .count()
            .saturating_mul(2)
            .div_ceil(3)
            * 4;
        assert!(encoded < 32000);
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
