//! A dedicated Chromium profile. Verification precedes every page connection; closing
//! uses the verified browser's DevTools socket, never a signal to an arbitrary PID. Only
//! the verified browser is signalled, when it does not exit after closing its endpoint.
use super::{ConnectionMetadata, ProbeStatus, cdp::Cdp, connection::Action, session::Verifier};
use crate::{config::DesktopAccount, error::AppError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::{ffi::OsStringExt, fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub struct BrowserSystem {
    pub core: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    pid: u32,
    started: String,
    executable: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    version: u8,
    port: u16,
    owner: Option<Identity>,
    visible: bool,
    #[serde(default)]
    core: bool,
    phase: String,
}

fn failed(message: &str) -> AppError {
    AppError::Desktop(message.into())
}

pub(super) fn data_directory() -> Result<PathBuf, AppError> {
    let base = match std::env::var_os("TEAMS_DESKTOP_BROWSER_DATA_DIR") {
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(failed(
                    "TEAMS_DESKTOP_BROWSER_DATA_DIR must be an absolute path",
                ));
            }
            path
        }
        None => dirs::data_local_dir()
            .ok_or_else(|| failed("could not locate the browser profile directory"))?
            .join("teams-cli/browser"),
    };
    Ok(base)
}

fn directory(port: u16) -> Result<PathBuf, AppError> {
    Ok(data_directory()?.join(port.to_string()))
}

/// Avoid assigning a new account a port with an older browser's saved sign-in.
pub fn has_saved_data(port: u16) -> Result<bool, AppError> {
    Ok(directory(port)?.try_exists()?)
}

fn profile(dir: &Path) -> PathBuf {
    dir.join("profile")
}

pub fn executable() -> Result<PathBuf, AppError> {
    if let Some(path) = std::env::var_os("TEAMS_DESKTOP_BROWSER") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(failed(
                "TEAMS_DESKTOP_BROWSER must be an absolute path to the Chrome/Chromium/Edge executable (not a shell wrapper)",
            ));
        }
        return std::fs::canonicalize(path)
            .map_err(|_| failed("TEAMS_DESKTOP_BROWSER does not exist; fix or unset this override, then retry setup"));
    }
    let candidates = if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else {
        vec![
            "/opt/google/chrome/chrome",
            "/opt/microsoft/msedge/msedge",
            "/opt/microsoft/msedge-beta/msedge",
            "/usr/lib/chromium/chromium",
            "/usr/lib64/chromium-browser/chromium-browser",
            "/usr/lib/chromium-browser/chromium-browser",
        ]
    };
    candidates.iter().find_map(|p| std::fs::canonicalize(p).ok())
        .ok_or_else(|| failed("install Chrome, Chromium or Edge, or set TEAMS_DESKTOP_BROWSER to its executable; WSL2 uses a Linux browser"))
}

pub fn prerequisites() -> Result<(), AppError> {
    prerequisites_for(false)
}

pub fn prerequisites_for(core: bool) -> Result<(), AppError> {
    if core {
        super::browser_install::installed().map_err(|error| failed(&format!("The lightweight browser is not ready: {error}. Run `teams desktop login --desktop-client browser-core` to set it up.")))?;
    } else {
        executable()?;
    }
    verification_prerequisites()
}

fn verification_prerequisites() -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    if !Path::new("/usr/sbin/lsof").is_file() {
        return Err(failed("lsof is required to verify the browser listener"));
    }
    #[cfg(target_os = "linux")]
    if !Path::new("/proc/net/tcp").is_file() {
        return Err(failed(
            "Linux /proc is required to verify the browser listener",
        ));
    }
    Ok(())
}

fn display_prerequisites() -> Result<(), AppError> {
    #[cfg(target_os = "linux")]
    if !["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
    {
        return Err(failed(
            "Browser sign-in needs a display. Enable WSLg on WSL2, or run from a Linux desktop session.",
        ));
    }
    Ok(())
}

/// Local prerequisites and (only if needed) public download metadata. No profile is created.
pub async fn preflight(core: bool) -> Result<bool, AppError> {
    display_prerequisites()?;
    verification_prerequisites()?;
    let base = data_directory()?;
    if base.try_exists()? {
        validate_directory(&base)?;
    }
    if core {
        super::browser_install::preflight().await
    } else {
        browser_version(&executable()?)?;
        Ok(false)
    }
}

fn session_executable(core: bool, visible: bool) -> Result<PathBuf, AppError> {
    if core {
        let (chrome, shell) = super::browser_install::installed()?;
        Ok(if visible { chrome } else { shell })
    } else {
        executable()
    }
}

pub(super) fn browser_version(path: &Path) -> Result<String, AppError> {
    browser_version_with_timeout(path, Duration::from_secs(5))
}

fn browser_version_with_timeout(path: &Path, timeout: Duration) -> Result<String, AppError> {
    // Files avoid pipe backpressure; a broken executable must not stall setup.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error.into(),
                    _ => failed(
                        "Checking the browser version timed out. Check its executable or unset the browser overrides, then retry setup.",
                    ),
                });
            }
        }
    };
    if !status.success() {
        stderr.rewind()?;
        let mut detail = Vec::new();
        stderr.take(400).read_to_end(&mut detail)?;
        return Err(failed(&format!(
            "The browser could not start: {}. On Linux, install the Chrome system libraries named in this error and retry setup.",
            String::from_utf8_lossy(&detail).trim()
        )));
    }
    stdout.rewind()?;
    let mut version = Vec::new();
    stdout.take(4096).read_to_end(&mut version)?;
    String::from_utf8_lossy(&version)
        .split_whitespace()
        .find(|s| s.split('.').count() == 4 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .map(str::to_owned)
        .ok_or_else(|| failed("unrecognized browser version"))
}

fn validate_core(account: DesktopAccount) -> Result<(), AppError> {
    if account != DesktopAccount::Personal {
        return Err(failed(
            "browser-core currently requires --desktop-account personal",
        ));
    }
    if browser_version(&session_executable(true, true)?)?
        != browser_version(&session_executable(true, false)?)?
    {
        return Err(failed(
            "visible browser and headless shell must have exactly matching versions",
        ));
    }
    Ok(())
}

fn load(dir: &Path, port: u16) -> Result<Option<State>, AppError> {
    match std::fs::read(dir.join("connection.json")) {
        Ok(bytes) => {
            let state: State = serde_json::from_slice(&bytes)
                .map_err(|_| failed("invalid browser connection record; preserved for recovery"))?;
            if state.version != 1
                || state.port != port
                || !matches!(state.phase.as_str(), "starting" | "active" | "core_ready")
            {
                return Err(failed(
                    "invalid browser connection record; preserved for recovery",
                ));
            }
            Ok(Some(state))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save(dir: &Path, state: &State) -> Result<(), AppError> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(&serde_json::to_vec(state).map_err(|_| failed("invalid browser state"))?)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("connection.json"))
        .map_err(|e| e.error)?;
    Ok(())
}

fn arguments_match(args: &[OsString], profile: &Path, port: u16) -> bool {
    let mut expected = OsString::from("--user-data-dir=");
    expected.push(profile);
    let debug = OsString::from(format!("--remote-debugging-port={port}"));
    args.iter()
        .filter(|a| a.to_string_lossy().starts_with("--user-data-dir"))
        .count()
        == 1
        && args.contains(&expected)
        && args
            .iter()
            .filter(|a| a.to_string_lossy().starts_with("--remote-debugging-port"))
            .count()
            == 1
        && args.contains(&debug)
        && !args
            .iter()
            .any(|a| a == "--remote-debugging-pipe" || a.to_string_lossy().starts_with("--type="))
}

fn owned(state: &State, dir: &Path) -> bool {
    if validate_directory(dir).is_err() || validate_directory(&profile(dir)).is_err() {
        return false;
    }
    state.owner.as_ref().is_some_and(|owner| {
        system::process(owner.pid).is_ok_and(|(identity, args)| {
            identity == *owner && arguments_match(&args, &profile(dir), state.port)
        })
    })
}

impl Verifier for BrowserSystem {
    fn core_only(&self, port: u16) -> bool {
        self.core
            && directory(port)
                .and_then(|d| load(&d, port))
                .map_or(true, |state| state.is_none_or(|s| !s.visible))
    }

    async fn verify(&self, port: u16) -> Result<(ConnectionMetadata, Vec<Value>), AppError> {
        prerequisites_for(self.core)?;
        let mut metadata = ConnectionMetadata::empty(port);
        let sockets = system::listeners(port)?;
        if sockets.is_empty() {
            return Ok((metadata, vec![]));
        }
        metadata.loopback_only = Some(sockets.iter().all(|s| s.loopback));
        if metadata.loopback_only != Some(true) {
            metadata.status = ProbeStatus::UnsafeListenerRefused;
            return Ok((metadata, vec![]));
        }
        let dir = directory(port)?;
        let state = load(&dir, port)?;
        let trusted = state.as_ref().is_some_and(|s| {
            s.core == self.core
                && owned(s, &dir)
                && s.owner.as_ref().is_some_and(|o| {
                    session_executable(s.core, s.visible).is_ok_and(|e| e == o.executable)
                        && system::owns_sockets(o.pid, &sockets)
                })
                && owned(s, &dir)
        });
        metadata.teams_owned = Some(trusted);
        if !trusted {
            metadata.status = ProbeStatus::NonTeamsListenerRefused;
            return Ok((metadata, vec![]));
        }
        let targets = super::fill_metadata(&mut metadata).await;
        if state
            .as_ref()
            .is_some_and(|s| s.core && !s.visible && s.phase != "core_ready")
        {
            return Ok((metadata, vec![]));
        }
        Ok((metadata, targets))
    }
}

fn launch_args(
    dir: &Path,
    port: u16,
    account: DesktopAccount,
    visible: bool,
    core: bool,
) -> Vec<OsString> {
    let mut data = OsString::from("--user-data-dir=");
    data.push(profile(dir));
    let mut args = vec![
        data,
        format!("--remote-debugging-port={port}").into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
    ];
    if core && cfg!(target_os = "macos") {
        args.push("--use-mock-keychain".into());
    }
    #[cfg(target_os = "linux")]
    if visible && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        args.push("--ozone-platform-hint=auto".into());
    }
    if !visible {
        args.push("--headless=new".into());
        if core {
            args.push("--disable-gpu".into());
            args.push("--js-flags=--lite-mode --optimize-for-size".into());
        }
    }
    let url = if account == DesktopAccount::Personal {
        "https://teams.live.com"
    } else {
        "https://teams.cloud.microsoft"
    };
    args.push(if core && !visible {
        "about:blank".into()
    } else {
        format!("--app={url}").into()
    });
    args
}

struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) fn prepare_directory(dir: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    validate_directory(dir)
}

pub(super) fn validate_directory(dir: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(dir)?;
    let me = system::uid()?;
    if !metadata.is_dir() || metadata.uid() != me || metadata.mode() & 0o077 != 0 {
        return Err(failed(
            "the dedicated browser directory must be owned by you with permissions 0700",
        ));
    }
    Ok(())
}

/// Browser sessions persist independently of the CLI. Sign-in data survives disconnect.
// Separate orchestration from OS/browser operations so the visibility transition
// and failure behavior can be tested without a signed-in Microsoft account.
trait LoginSession {
    async fn connect(&mut self, visible: bool) -> Result<(), AppError>;
    async fn disconnect(&mut self) -> Result<(), AppError>;
    async fn wait_read(&mut self, timeout: Duration) -> Result<(), AppError>;
    /// Whether the headless browser keeps Teams signed in for `period`.
    async fn stays_signed_in(&mut self, period: Duration) -> Result<bool, AppError>;
}

/// Returns whether the browser was left visible.
async fn login_flow(session: &mut impl LoginSession) -> Result<bool, AppError> {
    session.connect(true).await?;
    session
        .wait_read(Duration::from_secs(300))
        .await
        .map_err(|error| {
            error.with_hint(
        "The visible browser and its profile were preserved; sign-in has not completed there yet.")
        })?;
    session.disconnect().await?;
    session.connect(false).await?;
    session
        .wait_read(Duration::from_secs(60))
        .await
        .map_err(|error| {
            error.with_hint(
        "The headless browser and saved profile were preserved; headless chat access is not ready.")
        })?;
    if session.stays_signed_in(Duration::from_secs(60)).await? {
        return Ok(false);
    }
    // Teams renews its sign-in shortly after loading. Organisations that sign in through
    // their own identity provider (such as AD FS with Windows sign-in) refuse that renewal
    // headless and send the page back to sign-in, so the visible browser stays open.
    session.disconnect().await?;
    session.connect(true).await?;
    session.wait_read(Duration::from_secs(300)).await.map_err(|error| error.with_hint(
        "Your organisation's sign-in needs the visible browser; sign-in has not completed there yet."))?;
    Ok(true)
}

async fn resume_flow(session: &mut impl LoginSession, visible: bool) -> Result<bool, AppError> {
    session.connect(visible).await?;
    session.wait_read(Duration::from_secs(60)).await?;
    Ok(visible)
}

async fn check_resumed_session(session: &mut impl LoginSession) -> Result<(), AppError> {
    if session.stays_signed_in(Duration::from_secs(60)).await? {
        Ok(())
    } else {
        Err(AppError::Auth("Teams needs its browser window to stay signed in. Reopen sign-in to continue; your saved session is preserved.".into()))
    }
}

/// Reuse an opted-in inbox's saved session without forcing a visible login or mode switch.
pub async fn resume(
    port: u16,
    account: DesktopAccount,
    core: bool,
    quiet: bool,
) -> Result<(bool, Option<super::ConnectionWatch>), AppError> {
    let status = manage(port, account, Action::Status, false, core).await?;
    let already_running = status["endpoint_available"] == true;
    let visible = status["management"]["visible"].as_bool().unwrap_or(false);
    let mut session = BrowserLogin {
        port,
        account,
        core,
        quiet,
        resume_only: true,
    };
    let result = resume_flow(&mut session, visible).await;
    if let Err(error) = result {
        // Only a fresh, verified sign-in redirect is classified as expired auth.
        // Ownership failures and unknown cache layouts must never trigger a login restart.
        let metadata = (BrowserSystem { core }).probe(port).await?;
        if metadata.signed_out() {
            return Err(AppError::Auth(
                "Your saved Teams session needs sign-in again.".into(),
            ));
        }
        return Err(error);
    }
    let visible = result?;
    let watch = if !visible && !already_running && account != DesktopAccount::Personal {
        let (sender, receiver) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            session.quiet = true;
            let result = check_resumed_session(&mut session).await;
            let _ = sender.send(result);
        });
        Some(super::ConnectionWatch { receiver, task })
    } else {
        None
    };
    Ok((visible, watch))
}

struct BrowserLogin {
    port: u16,
    account: DesktopAccount,
    core: bool,
    quiet: bool,
    resume_only: bool,
}

impl LoginSession for BrowserLogin {
    async fn connect(&mut self, visible: bool) -> Result<(), AppError> {
        // An existing session can be reused in its current mode. A mode change
        // closes only the verified managed browser and keeps its profile.
        let dir = directory(self.port)?;
        if let Some(state) = load(&dir, self.port)?
            && state.core == self.core
            && !owned(&state, &dir)
            && system::listeners(self.port)?.is_empty()
        {
            manage(self.port, self.account, Action::Recover, false, self.core).await?;
        }
        if let Some(state) = load(&dir, self.port)?
            && state.visible != visible
        {
            self.disconnect().await?;
        }
        manage(self.port, self.account, Action::Connect, visible, self.core).await?;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), AppError> {
        manage(
            self.port,
            self.account,
            Action::Disconnect,
            false,
            self.core,
        )
        .await?;
        Ok(())
    }

    async fn wait_read(&mut self, timeout: Duration) -> Result<(), AppError> {
        tokio::time::timeout(timeout, async {
            loop {
                // Fresh verification on every attempt, including during navigation.
                let mut session = super::session::Session::new(BrowserSystem { core: self.core });
                let mut request = super::read_request(self.port, None, 1, None)?;
                request["account_type"] = json!(self.account);
                let reply = session.read(&request).await?;
                if login_read_ready(&reply)? {
                    return Ok(());
                }
                if self.resume_only
                    && (BrowserSystem { core: self.core })
                        .probe(self.port)
                        .await?
                        .signed_out()
                {
                    return Err(AppError::Auth(
                        "Your saved Teams session needs sign-in again.".into(),
                    ));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await
        .map_err(|_| failed("Timed out waiting for a readable Teams chat cache"))?
    }

    async fn stays_signed_in(&mut self, period: Duration) -> Result<bool, AppError> {
        // Personal accounts renew headless; their sign-in needs no extra wait.
        if self.account == DesktopAccount::Personal {
            return Ok(true);
        }
        if !self.quiet {
            eprintln!("Checking that Teams stays signed in without its window (up to a minute).");
        }
        if !watch_sign_in(&BrowserSystem { core: self.core }, self.port, period).await? {
            return Ok(false);
        }
        self.wait_read(Duration::from_secs(60)).await?;
        Ok(true)
    }
}

async fn watch_sign_in(
    system: &impl Verifier,
    port: u16,
    period: Duration,
) -> Result<bool, AppError> {
    let deadline = tokio::time::Instant::now() + period;
    while tokio::time::Instant::now() < deadline {
        let (metadata, _) = system.verify(port).await?;
        if !metadata.endpoint_available() {
            return Err(failed(&format!(
                "The dedicated browser became unavailable while checking sign-in. {}",
                metadata.detail()
            )));
        }
        if metadata.signed_out() {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(true)
}

fn login_read_ready(reply: &Value) -> Result<bool, AppError> {
    if matches!(reply["error"].as_str(), Some("no_target" | "read_failed")) {
        return Ok(false);
    }
    // The cache is not yet available during sign-in and initial Teams loading.
    // Ownership/origin/account refusals still fail immediately.
    if reply["result"].get("exceptionDetails").is_some() {
        return Ok(false);
    }
    if let Some(raw) = reply["result"]["result"]["value"].as_str()
        && let Ok(value) = serde_json::from_str::<Value>(raw)
        && matches!(value["error"].as_str(), Some("layout" | "core_not_ready"))
    {
        return Ok(false);
    }
    super::read_page(reply).map(|_| true)
}

pub async fn login(port: u16, account: DesktopAccount, core: bool) -> Result<Value, AppError> {
    login_with_progress(port, account, core, false).await
}

pub async fn login_with_progress(
    port: u16,
    account: DesktopAccount,
    core: bool,
    quiet: bool,
) -> Result<Value, AppError> {
    login_with_recovery(port, account, core, quiet, true).await
}

pub(super) async fn login_with_recovery(
    port: u16,
    account: DesktopAccount,
    core: bool,
    quiet: bool,
    recovery: bool,
) -> Result<Value, AppError> {
    display_prerequisites()?;
    if core {
        super::browser_install::ensure_with_progress(quiet).await?;
    }
    prerequisites_for(core)?;
    let dir = directory(port)?;
    prepare_directory(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("login.lock"))?;
    lock.try_lock()
        .map_err(|_| failed("another browser login is running"))?;
    let _guard = Guard(lock);
    if !quiet {
        eprintln!(
            "Opening Teams sign-in. Complete sign-in in the window; it will hide automatically once your chats are ready."
        );
    }
    let visible = login_flow(&mut BrowserLogin {
        port,
        account,
        core,
        quiet,
        resume_only: false,
    })
    .await.map_err(|error| if recovery {
        error.with_hint(&format!("Retry sign-in: `teams desktop login --desktop-client {} --port {port}`. To close this dedicated browser: `teams desktop disconnect --desktop-client {} --port {port}`.", if core { "browser-core" } else { "browser" }, if core { "browser-core" } else { "browser" }))
    } else { error })?;
    if visible && !quiet {
        eprintln!(
            "Your organisation's sign-in does not allow Teams to stay signed in without its window, so the browser stays visible. Minimise it, but keep it open."
        );
    }
    let mut value = manage(port, account, Action::Status, false, core).await?;
    if value["endpoint_available"] != true {
        return Err(failed(
            "The dedicated browser closed or changed after sign-in; retry the sign-in command.",
        ));
    }
    value["session_verified"] = json!(true);
    Ok(value)
}

pub async fn manage(
    port: u16,
    account: DesktopAccount,
    action: Action,
    visible: bool,
    core: bool,
) -> Result<Value, AppError> {
    if core && matches!(action, Action::Connect) {
        super::browser_install::ensure().await?;
    }
    prerequisites_for(core)?;
    let dir = directory(port)?;
    prepare_directory(&dir)?;
    prepare_directory(&profile(&dir))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("connection.lock"))?;
    lock.try_lock()
        .map_err(|_| failed("another browser connection command is running"))?;
    let _guard = Guard(lock);
    let mut state = load(&dir, port)?;
    let system = BrowserSystem { core };
    let recover = format!(
        "teams desktop recover --desktop-client {} --port {port}",
        if core { "browser-core" } else { "browser" }
    );
    if state.as_ref().is_some_and(|s| s.core != core) {
        return Err(failed(
            "this port uses another browser mode; use its original --desktop-client to disconnect it first",
        ));
    }
    match action {
        Action::Status => {}
        Action::Connect => {
            if core {
                validate_core(account)?;
            }
            // A dead/reused owner with an empty port can be forgotten without
            // touching a process. Unrecorded startups and occupied ports still refuse.
            if state
                .as_ref()
                .is_some_and(|s| s.owner.is_some() && !owned(s, &dir))
                && system::listeners(port)?.is_empty()
            {
                std::fs::remove_file(dir.join("connection.json"))?;
                state = None;
            }
            if let Some(current) = state.as_ref() {
                if !owned(current, &dir) {
                    return Err(failed(&format!(
                        "the saved browser session is no longer owned; run `{recover}` before connecting"
                    )));
                }
                if current.visible != visible {
                    return Err(failed(
                        "disconnect the browser before changing between visible and headless mode",
                    ));
                }
            } else {
                let (metadata, _) = system.verify(port).await?;
                if metadata.status != ProbeStatus::NoListener {
                    return Err(failed(
                        "the browser debugging port is already in use; its listener was left untouched",
                    ));
                }
                let mut next = State {
                    version: 1,
                    port,
                    owner: None,
                    visible,
                    core,
                    phase: "starting".into(),
                };
                save(&dir, &next)?;
                let mut child = Command::new(session_executable(core, visible)?)
                    .args(launch_args(&dir, port, account, visible, core))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .map_err(|error| {
                        failed(&format!(
                            "could not launch the Teams browser: {error}; run `{recover}`"
                        ))
                    })?;
                let pid = child.id();
                // Capture the process after exec, then reap it without tying its life to the CLI.
                let owner = system::process(pid).map(|(identity, _)| identity);
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                next.owner = Some(owner?);
                save(&dir, &next)?;
                state = Some(next);
            }
            let mut ready = false;
            for _ in 0..100 {
                let current = state.as_ref().expect("browser state");
                if !owned(current, &dir) {
                    break;
                }
                if system.probe(port).await?.endpoint_available() {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if !ready {
                return Err(failed(&format!(
                    "the browser did not open its debugging endpoint; its recovery record was preserved. Inspect it with `teams desktop status`, or run `{recover}`."
                )));
            }
            let current = state.as_mut().expect("browser state");
            if core && !visible && current.phase != "core_ready" {
                let targets = super::fill_metadata(&mut ConnectionMetadata::empty(port)).await;
                let pages: Vec<_> = targets.iter().filter(|t| t["type"] == "page").collect();
                let target = pages
                    .first()
                    .filter(|_| pages.len() == 1)
                    .and_then(|t| t["id"].as_str().filter(|id| super::valid_id(id)))
                    .ok_or_else(|| {
                        failed("core startup requires exactly one managed browser page")
                    })?;
                super::browser_core::start(port, target).await?;
                current.phase = "core_ready".into();
            } else if !core || visible {
                current.phase = "active".into();
            }
            save(&dir, current)?;
        }
        Action::Disconnect | Action::Recover => {
            if matches!(action, Action::Recover)
                && let Some(current) = state.as_mut()
                && current.owner.is_none()
            {
                let expected = session_executable(current.core, current.visible)?;
                let mut candidates = system::pids()?
                    .into_iter()
                    .filter_map(|pid| system::process(pid).ok())
                    .filter(|(identity, args)| {
                        identity.executable == expected
                            && arguments_match(args, &profile(&dir), port)
                    });
                if let Some((identity, _)) = candidates.next() {
                    if candidates.next().is_some() {
                        return Err(failed(
                            "multiple browser processes match the interrupted startup; they were left untouched",
                        ));
                    }
                    current.owner = Some(identity);
                    save(&dir, current)?;
                }
            }
            if let Some(current) = state.as_ref() {
                if owned(current, &dir) && system::listeners(port)?.is_empty() {
                    // It already released its endpoint, as after a close request it never
                    // finished, so DevTools cannot reach it any more.
                    stop(current, &dir).await?;
                } else if owned(current, &dir) {
                    let (metadata, _) = system.verify(port).await?;
                    if !metadata.endpoint_available() {
                        return Err(failed(&format!(
                            "browser ownership could not be verified; close the dedicated browser manually, then run `{recover}`"
                        )));
                    }
                    let (version, _) = super::read_metadata(port).await?;
                    let id = browser_id(&version, port)?;
                    if !owned(current, &dir) {
                        return Err(failed("the browser process changed; it was left untouched"));
                    }
                    Cdp::close_browser(port, &id).await?;
                    if !exited(current, &dir, Duration::from_secs(10)).await {
                        stop(current, &dir).await?;
                    }
                } else if current.owner.is_some() {
                    // A reused or changed PID is never contacted or signalled. Recover
                    // clears stale state only when there is no listener left on its port.
                    if !matches!(action, Action::Recover) || !system::listeners(port)?.is_empty() {
                        return Err(failed(
                            "the saved browser process changed; it was left untouched. Close the dedicated browser, then recover",
                        ));
                    }
                } else if std::fs::symlink_metadata(profile(&dir).join("SingletonLock")).is_ok()
                    || !system::listeners(port)?.is_empty()
                {
                    return Err(failed(
                        "browser startup was interrupted before ownership was saved; close the dedicated browser manually, then recover",
                    ));
                }
                std::fs::remove_file(dir.join("connection.json"))?;
                state = None;
            } else if !system::listeners(port)?.is_empty() {
                return Err(failed(
                    "this browser is not managed by teams-cli; it was left untouched",
                ));
            }
        }
    }
    let metadata = system.probe(port).await?;
    let live = state.as_ref().is_some_and(|s| owned(s, &dir));
    Ok(
        json!({"port":port,"endpoint_available":metadata.endpoint_available(),"session_verified":false,
        "connection":metadata,"browser_profile":profile(&dir),
        "management":{"managed":state.is_some(),"port":state.as_ref().map(|s|s.port),
        "phase":state.as_ref().map(|s|s.phase.as_str()).unwrap_or("idle"),
        "visible":state.as_ref().map(|s|s.visible),"core_only":core,"settings_pending":false,
        "needs_recovery":state.as_ref().is_some_and(|s|!matches!(s.phase.as_str(), "active" | "core_ready") || !live)}}),
    )
}

/// Whether the saved browser exits within `timeout`.
async fn exited(state: &State, dir: &Path, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while owned(state, dir) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// Stops the saved browser when it was asked to close and did not: Chrome can hang on
/// exit after closing its endpoint. Only the process verified by PID, start time,
/// executable and profile arguments is signalled.
async fn stop(state: &State, dir: &Path) -> Result<(), AppError> {
    if let Some(owner) = state.owner.as_ref()
        && owned(state, dir)
    {
        // SAFETY: kill(2) has no memory effects; the PID was verified just above.
        unsafe { libc::kill(owner.pid as libc::pid_t, libc::SIGKILL) };
    }
    if exited(state, dir, Duration::from_secs(5)).await {
        return Ok(());
    }
    Err(failed(
        "the browser has not closed; its recovery record was preserved. Close the dedicated browser manually, then recover.",
    ))
}

impl BrowserSystem {
    async fn probe(&self, port: u16) -> Result<ConnectionMetadata, AppError> {
        Ok(self.verify(port).await?.0)
    }
}

fn browser_id(version: &Value, port: u16) -> Result<String, AppError> {
    let url = version["webSocketDebuggerUrl"]
        .as_str()
        .and_then(|s| url::Url::parse(s).ok())
        .ok_or_else(|| failed("the browser did not advertise a valid control socket"))?;
    let id = url.path().strip_prefix("/devtools/browser/").unwrap_or("");
    if url.scheme() != "ws"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || url.port() != Some(port)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(8..=64).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    {
        return Err(failed("the browser advertised an unsafe control socket"));
    }
    Ok(id.into())
}

#[cfg(target_os = "linux")]
mod system {
    use super::*;
    use std::{collections::HashSet, os::unix::fs::MetadataExt};
    #[derive(Debug)]
    pub struct Socket {
        pub loopback: bool,
        inode: u64,
        uid: u32,
    }

    pub fn parse_sockets(text: &str, port: u16, ipv6: bool) -> Result<Vec<Socket>, AppError> {
        let mut sockets = vec![];
        for row in text.lines().skip(1) {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() < 10 {
                return Err(failed("invalid Linux socket metadata"));
            }
            let (address, number) = fields[1]
                .split_once(':')
                .ok_or_else(|| failed("invalid Linux socket address"))?;
            let number =
                u16::from_str_radix(number, 16).map_err(|_| failed("invalid Linux socket port"))?;
            if number != port || fields[3] != "0A" {
                continue;
            }
            sockets.push(Socket {
                loopback: address
                    == if ipv6 {
                        "00000000000000000000000001000000"
                    } else {
                        "0100007F"
                    },
                uid: fields[7]
                    .parse()
                    .map_err(|_| failed("invalid Linux socket owner"))?,
                inode: fields[9]
                    .parse()
                    .map_err(|_| failed("invalid Linux socket inode"))?,
            });
        }
        Ok(sockets)
    }
    pub fn listeners(port: u16) -> Result<Vec<Socket>, AppError> {
        let mut sockets = parse_sockets(&std::fs::read_to_string("/proc/net/tcp")?, port, false)?;
        match std::fs::read_to_string("/proc/net/tcp6") {
            Ok(text) => sockets.extend(parse_sockets(&text, port, true)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(sockets)
    }
    pub fn process(pid: u32) -> Result<(Identity, Vec<OsString>), AppError> {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let uid = std::fs::metadata("/proc/self")?.uid();
        if std::fs::metadata(&dir)?.uid() != uid {
            return Err(failed("browser belongs to another user"));
        }
        let before = std::fs::read_to_string(dir.join("stat"))?;
        let args = arguments(&std::fs::read(dir.join("cmdline"))?);
        let exe = std::fs::read_link(dir.join("exe"))?;
        let after = std::fs::read_to_string(dir.join("stat"))?;
        let started = start_time(&before)?;
        if start_time(&after)? != started {
            return Err(failed("browser process changed during verification"));
        }
        Ok((
            Identity {
                pid,
                started,
                executable: exe,
            },
            args,
        ))
    }
    /// Chrome rewrites its process title as its arguments joined by spaces, so /proc
    /// can report a single argument; split that title back at each option.
    pub fn arguments(cmdline: &[u8]) -> Vec<OsString> {
        let args: Vec<_> = cmdline
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .collect();
        let parts = match args[..] {
            [title] => {
                let mut parts = vec![];
                let mut start = 0;
                for i in 0..title.len().saturating_sub(2) {
                    if &title[i..i + 3] == b" --" {
                        parts.push(&title[start..i]);
                        start = i + 1;
                    }
                }
                parts.push(&title[start..]);
                parts
            }
            _ => args,
        };
        parts
            .into_iter()
            .map(|a| OsString::from_vec(a.into()))
            .collect()
    }
    pub fn start_time(stat: &str) -> Result<String, AppError> {
        stat.rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().nth(19))
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
            .map(String::from)
            .ok_or_else(|| failed("invalid Linux process identity"))
    }
    pub fn uid() -> Result<u32, AppError> {
        Ok(std::fs::metadata("/proc/self")?.uid())
    }
    pub fn pids() -> Result<Vec<u32>, AppError> {
        Ok(std::fs::read_dir("/proc")?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect())
    }
    pub fn owns_sockets(pid: u32, sockets: &[Socket]) -> bool {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        let inodes: HashSet<_> = fds
            .filter_map(Result::ok)
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter_map(|p| {
                p.to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse::<u64>()
                    .ok()
            })
            .collect();
        let Ok(me) = std::fs::metadata("/proc/self") else {
            return false;
        };
        sockets
            .iter()
            .all(|s| s.uid == me.uid() && inodes.contains(&s.inode))
    }
}

#[cfg(target_os = "macos")]
mod system {
    use super::*;
    #[derive(Debug)]
    pub struct Socket {
        pub loopback: bool,
        pid: u32,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
    }
    fn run(path: &str, args: &[&str]) -> Result<String, AppError> {
        let output = Command::new(path)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(failed("macOS browser process inspection failed"));
        }
        String::from_utf8(output.stdout).map_err(|_| failed("invalid macOS process metadata"))
    }
    pub fn listeners(port: u16) -> Result<Vec<Socket>, AppError> {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(failed("browser listener inspection failed"));
        }
        super::super::macos::parse_listeners(&String::from_utf8_lossy(&output.stdout)).map(|list| {
            list.into_iter()
                .map(|(pid, address)| Socket {
                    pid,
                    loopback: super::super::macos::loopback_address(&address, port),
                })
                .collect()
        })
    }
    pub fn process(pid: u32) -> Result<(Identity, Vec<OsString>), AppError> {
        let snapshot = run("/bin/ps", &["-p", &pid.to_string(), "-o", "uid=,lstart="])?;
        let (uid, stamp) = snapshot
            .trim()
            .split_once(char::is_whitespace)
            .ok_or_else(|| failed("invalid browser process metadata"))?;
        // These OS queries do not inspect browser credentials or page data.
        if uid.parse::<u32>().ok() != Some(unsafe { libc::geteuid() }) {
            return Err(failed("browser belongs to another user"));
        }
        let mut path = vec![0u8; 4096];
        let n = unsafe { proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
        if n <= 0 {
            return Err(failed("could not inspect the browser executable"));
        }
        let executable = PathBuf::from(OsString::from_vec(
            path.split(|b| *b == 0).next().unwrap_or_default().into(),
        ));
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
        let mut bytes = vec![0u8; 1024 * 1024];
        let mut len = bytes.len();
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                bytes.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            return Err(failed("could not inspect the browser launch arguments"));
        }
        bytes.truncate(len);
        let args = parse_args(&bytes)?;
        if run("/bin/ps", &["-p", &pid.to_string(), "-o", "uid=,lstart="])? != snapshot {
            return Err(failed("browser process changed"));
        }
        Ok((
            Identity {
                pid,
                started: stamp.trim().into(),
                executable,
            },
            args,
        ))
    }
    pub fn parse_args(bytes: &[u8]) -> Result<Vec<OsString>, AppError> {
        let count = bytes
            .get(..4)
            .map(|b| i32::from_ne_bytes(b.try_into().expect("four bytes")))
            .filter(|n| *n > 0 && *n <= 4096)
            .ok_or_else(|| failed("invalid browser arguments"))? as usize;
        let end = bytes[4..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| failed("invalid browser arguments"))?
            + 4;
        let args = bytes[end..]
            .split(|b| *b == 0)
            .skip_while(|a| a.is_empty())
            .take(count)
            .map(|a| OsString::from_vec(a.into()))
            .collect::<Vec<_>>();
        if args.len() != count {
            return Err(failed("invalid browser arguments"));
        }
        Ok(args)
    }
    pub fn uid() -> Result<u32, AppError> {
        Ok(unsafe { libc::geteuid() })
    }
    pub fn pids() -> Result<Vec<u32>, AppError> {
        Ok(run("/bin/ps", &["-axo", "pid="])?
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect())
    }
    pub fn owns_sockets(pid: u32, sockets: &[Socket]) -> bool {
        sockets.iter().all(|s| s.pid == pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeLogin {
        events: Vec<&'static str>,
        fail_visible: bool,
        fail_headless: bool,
        fail_close: bool,
        signs_out_headless: bool,
    }
    impl LoginSession for FakeLogin {
        async fn connect(&mut self, visible: bool) -> Result<(), AppError> {
            self.events
                .push(if visible { "visible" } else { "headless" });
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), AppError> {
            self.events.push("close");
            if self.fail_close {
                return Err(failed("ownership changed"));
            }
            Ok(())
        }
        async fn wait_read(&mut self, timeout: Duration) -> Result<(), AppError> {
            let visible = timeout.as_secs() == 300;
            self.events.push(if visible {
                "read-visible"
            } else {
                "read-headless"
            });
            if (visible && self.fail_visible) || (!visible && self.fail_headless) {
                return Err(failed("not signed in"));
            }
            Ok(())
        }
        async fn stays_signed_in(&mut self, _period: Duration) -> Result<bool, AppError> {
            self.events.push("watch-headless");
            Ok(!self.signs_out_headless)
        }
    }

    #[tokio::test]
    async fn resume_preserves_visible_sessions_and_skips_repeated_work_sign_in_checks() {
        for visible in [false, true] {
            let mut session = FakeLogin::default();
            assert_eq!(resume_flow(&mut session, visible).await.unwrap(), visible);
            assert_eq!(
                session.events,
                [
                    if visible { "visible" } else { "headless" },
                    "read-headless"
                ]
            );
        }
    }

    #[tokio::test]
    async fn cold_resume_opens_a_verified_inbox_without_blocking_on_the_background_watch() {
        let mut session = FakeLogin {
            signs_out_headless: true,
            ..Default::default()
        };
        assert!(!resume_flow(&mut session, false).await.unwrap());
        assert_eq!(session.events, ["headless", "read-headless"]);
        let mut refused = FakeLogin {
            fail_headless: true,
            ..Default::default()
        };
        assert!(resume_flow(&mut refused, false).await.is_err());
        assert_eq!(refused.events, ["headless", "read-headless"]);
    }

    #[tokio::test]
    async fn the_background_watch_reports_sign_in_loss_without_restarting_the_browser() {
        let mut session = FakeLogin {
            signs_out_headless: true,
            ..Default::default()
        };
        assert!(matches!(
            check_resumed_session(&mut session).await,
            Err(AppError::Auth(_))
        ));
        assert_eq!(session.events, ["watch-headless"]);
    }

    #[tokio::test]
    async fn login_verifies_reads_before_and_after_hiding() {
        let mut session = FakeLogin::default();
        assert!(!login_flow(&mut session).await.unwrap());
        assert_eq!(
            session.events,
            [
                "visible",
                "read-visible",
                "close",
                "headless",
                "read-headless",
                "watch-headless"
            ]
        );
    }

    #[tokio::test]
    async fn sign_in_that_needs_the_window_reopens_it_visible() {
        let mut session = FakeLogin {
            signs_out_headless: true,
            ..Default::default()
        };
        assert!(login_flow(&mut session).await.unwrap());
        assert_eq!(
            session.events,
            [
                "visible",
                "read-visible",
                "close",
                "headless",
                "read-headless",
                "watch-headless",
                "close",
                "visible",
                "read-visible"
            ]
        );
    }

    #[tokio::test]
    async fn sign_in_watch_refuses_a_closed_or_unsafe_browser() {
        use super::super::owner::tests::FakeSystem;
        for listeners in [vec![], vec![(30, false)]] {
            let system = FakeSystem {
                listeners,
                ..FakeSystem::teams()
            };
            let error = watch_sign_in(&system, 9224, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("became unavailable"));
        }
    }

    #[tokio::test]
    async fn failed_visible_login_keeps_window_open() {
        let mut session = FakeLogin {
            fail_visible: true,
            ..Default::default()
        };
        let error = login_flow(&mut session).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("visible browser and its profile were preserved")
        );
        assert_eq!(session.events, ["visible", "read-visible"]);
    }

    #[tokio::test]
    async fn headless_sign_in_is_verified_before_success() {
        let mut session = FakeLogin {
            fail_headless: true,
            ..Default::default()
        };
        let error = login_flow(&mut session).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("headless browser and saved profile were preserved")
        );
        assert_eq!(session.events.last(), Some(&"read-headless"));
    }

    #[tokio::test]
    async fn failed_verified_close_prevents_headless_restart() {
        let mut session = FakeLogin {
            fail_close: true,
            ..Default::default()
        };
        assert!(login_flow(&mut session).await.is_err());
        assert_eq!(session.events, ["visible", "read-visible", "close"]);
    }

    #[test]
    fn login_retries_loading_but_refuses_unsafe_or_ambiguous_sessions() {
        for code in ["no_target", "read_failed"] {
            assert!(!login_read_ready(&json!({"error":code})).unwrap());
        }
        assert!(!login_read_ready(&json!({"result":{"exceptionDetails":{}}})).unwrap());
        for code in ["account_ambiguous", "not_loopback", "metadata_probe_failed"] {
            assert!(login_read_ready(&json!({"error":code})).is_err());
        }
        assert!(
            login_read_ready(
                &json!({"result":{"result":{"value":"{\"items\":[],\"next\":null}"}}})
            )
            .unwrap()
        );
    }

    #[test]
    fn browser_directories_refuse_shared_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        prepare_directory(&private).unwrap();
        validate_directory(&private).unwrap();
        let link = temp.path().join("link");
        symlink(&private, &link).unwrap();
        assert!(prepare_directory(&link).is_err());
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_directory(&private).is_err());
    }

    #[test]
    fn core_launch_preserves_sign_in_and_skips_ui_startup() {
        let dir = Path::new("/tmp/core profile");
        let args = launch_args(dir, 9333, DesktopAccount::Personal, false, true);
        assert!(args.contains(&OsString::from("about:blank")));
        assert!(args.contains(&OsString::from(
            "--js-flags=--lite-mode --optimize-for-size"
        )));
        assert!(arguments_match(&args, &profile(dir), 9333));
        let visible = launch_args(dir, 9333, DesktopAccount::Personal, true, true);
        assert!(!visible.contains(&OsString::from("--headless=new")));
        if cfg!(target_os = "macos") {
            assert!(visible.contains(&OsString::from("--use-mock-keychain")));
        }
    }

    #[test]
    fn launch_is_isolated_and_headless_unless_signing_in() {
        let dir = Path::new("/tmp/teams profile");
        let headless = launch_args(dir, 9333, DesktopAccount::Personal, false, false);
        assert!(headless.contains(&OsString::from("--headless=new")));
        assert!(headless.contains(&OsString::from("--app=https://teams.live.com")));
        assert!(arguments_match(&headless, &profile(dir), 9333));
        assert!(!arguments_match(
            &headless,
            Path::new("/tmp/normal-browser"),
            9333
        ));
        assert!(!arguments_match(&headless, &profile(dir), 9222));
        let visible = launch_args(dir, 9333, DesktopAccount::WorkSchool, true, false);
        assert!(!visible.contains(&OsString::from("--headless=new")));
        assert!(visible.contains(&OsString::from("--app=https://teams.cloud.microsoft")));
        let mut conflicting = headless.clone();
        conflicting.push("--user-data-dir=/tmp/normal-browser".into());
        assert!(!arguments_match(&conflicting, &profile(dir), 9333));
        let mut renderer = headless;
        renderer.push("--type=renderer".into());
        assert!(!arguments_match(&renderer, &profile(dir), 9333));
    }

    #[test]
    fn a_hung_browser_version_check_times_out_and_reaps_its_process() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let browser = dir.path().join("browser");
        std::fs::write(
            &browser,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"${0}.pid\"\nexec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        let error = browser_version_with_timeout(&browser, Duration::from_millis(100)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(dir.path().join("browser.pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn browser_control_socket_never_follows_an_advertised_remote_host() {
        let socket = |url: &str| json!({"webSocketDebuggerUrl":url});
        assert_eq!(
            browser_id(
                &socket("ws://localhost:9333/devtools/browser/abcdef12-3456"),
                9333
            )
            .unwrap(),
            "abcdef12-3456"
        );
        for url in [
            "ws://example.com:9333/devtools/browser/abcdef12",
            "ws://127.0.0.1:9222/devtools/browser/abcdef12",
            "ws://user@127.0.0.1:9333/devtools/browser/abcdef12",
            "ws://127.0.0.1:9333/devtools/page/abcdef12",
            "ws://127.0.0.1:9333/devtools/browser/../../x",
            "ws://127.0.0.1:9333/devtools/browser/abcdef12?secret=x",
        ] {
            assert!(browser_id(&socket(url), 9333).is_err(), "{url}");
        }
    }

    #[test]
    fn corrupt_or_wrong_port_state_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.json");
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(load(dir.path(), 9333).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        let state = State {
            version: 1,
            port: 9333,
            owner: None,
            visible: true,
            core: false,
            phase: "starting".into(),
        };
        save(dir.path(), &state).unwrap();
        assert!(load(dir.path(), 9222).is_err());
        assert!(load(dir.path(), 9333).unwrap().is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sockets_include_unsafe_and_foreign_listeners() {
        let table = "header\n 0: 0100007F:2475 00000000:0000 0A 0:0 0:0 0 1000 0 42\n 1: 00000000:2475 00000000:0000 0A 0:0 0:0 0 2000 0 43\n 2: 0100007F:2475 00000000:0000 01 0:0 0:0 0 1000 0 44\n";
        let sockets = system::parse_sockets(table, 9333, false).unwrap();
        assert_eq!(sockets.len(), 2);
        assert!(sockets[0].loopback);
        assert!(!sockets[1].loopback);
        assert!(system::parse_sockets("header\nbroken", 9333, false).is_err());
        let v6 = "header\n 0: 00000000000000000000000001000000:2475 0:0 0A 0:0 0:0 0 1000 0 42\n";
        assert!(system::parse_sockets(v6, 9333, true).unwrap()[0].loopback);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_identity_and_live_socket_ownership_are_checked() {
        let pid = std::process::id();
        let (identity, _) = system::process(pid).unwrap();
        assert_eq!(identity.executable, std::env::current_exe().unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sockets = system::listeners(listener.local_addr().unwrap().port()).unwrap();
        assert!(!sockets.is_empty());
        assert!(system::owns_sockets(pid, &sockets));
        assert!(!system::owns_sockets(0, &sockets));
        assert!(system::start_time("broken stat").is_err());
        // Command names can contain ')' and spaces; field positions start after the last ') '.
        let stat = format!("1 (browser ) name) S {} 1234 0", "0 ".repeat(18));
        assert_eq!(system::start_time(&stat).unwrap(), "1234");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_verified_browser_that_hangs_is_stopped() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("9333");
        prepare_directory(&dir).unwrap();
        prepare_directory(&profile(&dir)).unwrap();
        let mut data = OsString::from("--user-data-dir=");
        data.push(profile(&dir));
        // The trailing command keeps the shell from replacing itself with `sleep`.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30; :", "browser"])
            .arg(data)
            .arg("--remote-debugging-port=9333")
            .spawn()
            .unwrap();
        let (owner, _) = system::process(child.id()).unwrap();
        let state = State {
            version: 1,
            port: 9333,
            owner: Some(owner),
            visible: false,
            core: false,
            phase: "active".into(),
        };
        assert!(owned(&state, &dir));
        let reaper = std::thread::spawn(move || child.wait());
        stop(&state, &dir).await.unwrap();
        assert!(!owned(&state, &dir));
        assert!(!reaper.join().unwrap().unwrap().success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_arguments_survive_chrome_process_title_rewrite() {
        let profile = Path::new("/home/me/teams data/9333/profile");
        let launched = b"/opt/my chrome/chrome\0--user-data-dir=/home/me/teams data/9333/profile\0--remote-debugging-port=9333\0--app=https://teams.live.com\0";
        let mut title = launched.map(|b| if b == 0 { b' ' } else { b });
        *title.last_mut().unwrap() = 0;
        for cmdline in [launched.as_slice(), title.as_slice()] {
            let args = system::arguments(cmdline);
            assert_eq!(args[0], "/opt/my chrome/chrome");
            assert!(arguments_match(&args, profile, 9333));
            assert!(!arguments_match(&args, profile, 9222));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_launch_arguments_keep_paths_with_spaces_and_exclude_environment() {
        let args = [
            b"Chrome".as_slice(),
            b"--user-data-dir=/tmp/teams profile",
            b"--remote-debugging-port=9333",
        ];
        let mut bytes = (args.len() as i32).to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/Applications/Google Chrome\0\0\0");
        for arg in args {
            bytes.extend_from_slice(arg);
            bytes.push(0);
        }
        bytes.extend_from_slice(b"SECRET=not-an-argument\0");
        let parsed = system::parse_args(&bytes).unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(arguments_match(
            &parsed,
            Path::new("/tmp/teams profile"),
            9333
        ));
        assert!(system::parse_args(b"bad").is_err());
    }
}
