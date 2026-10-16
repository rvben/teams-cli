//! Application leases are backed by a supervised stdin pipe, not the application's PID.
//! EOF survives application crashes and avoids PID-reuse and extension-host sharing bugs.
//! All per-browser changes use the same lock as connect/disconnect; cleanup never acts
//! on a snapshot taken before acquiring that lock.
use super::*;
use std::collections::HashMap;
use std::io::IsTerminal;
use tokio::io::AsyncReadExt;

pub const CLIENT_ENV: &str = "TEAMS_DESKTOP_BROWSER_CLIENT";

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Lifetime {
    pub auto_close: bool,
    #[serde(default)]
    pub clients: Vec<String>,
    #[serde(default)]
    pub users: Vec<Identity>,
    #[serde(default)]
    pub idle_since: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Client {
    token: String,
    owner: Identity,
    keep_alive: bool,
}

pub(super) fn live(identity: &Identity) -> Result<bool, AppError> {
    live_with(identity, system::lease_process)
}

fn live_with(
    identity: &Identity,
    inspect: impl FnOnce(u32) -> Result<(Identity, Vec<OsString>), AppError>,
) -> Result<bool, AppError> {
    match inspect(identity.pid) {
        // A lease follows the process across exec or an executable upgrade.
        // The browser itself still requires full executable/profile verification.
        Ok((current, _)) => Ok(current.pid == identity.pid && current.started == identity.started),
        Err(error) => {
            // Inspection can fail for a live process (permissions, transient OS
            // errors). Only ESRCH proves it is gone; uncertainty must preserve its
            // lease, rather than closing a browser still in use.
            let pid = i32::try_from(identity.pid)
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or_else(|| failed("invalid browser lease process ID"))?;
            // SAFETY: signal zero only checks existence; it never signals a process.
            let exists = unsafe { libc::kill(pid, 0) };
            if exists == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else if exists == 0 && system::zombie(pid as u32)? {
                // Zombies still answer kill(pid, 0), but cannot use a browser.
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

fn clients_directory() -> Result<PathBuf, AppError> {
    Ok(data_directory()?.join("clients"))
}

fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn client(token: &str) -> Result<Option<Client>, AppError> {
    if !valid_token(token) {
        return Err(failed("invalid browser supervisor token"));
    }
    let dir = clients_directory()?;
    if !dir.try_exists()? {
        return Ok(None);
    }
    validate_directory(&dir)?;
    let path = dir.join(token);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: Client = serde_json::from_slice(&bytes)
        .map_err(|_| failed("invalid browser supervisor record; preserved for recovery"))?;
    Ok((value.token == token && live(&value.owner)?).then_some(value))
}

pub(super) fn current_client() -> Result<Option<Client>, AppError> {
    match std::env::var(CLIENT_ENV) {
        Ok(token) => client(&token)?.map(Some).ok_or_else(|| {
            failed("the browser supervisor has closed; reopen Teams Companion before retrying")
        }),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(failed("invalid browser supervisor token")),
    }
}

impl Lifetime {
    pub(super) fn for_launch() -> Result<Self, AppError> {
        let client = current_client()?;
        let keep_alive = match std::env::var("TEAMS_DESKTOP_BROWSER_KEEP_ALIVE") {
            Ok(value) if value == "1" => true,
            Ok(value) if value == "0" => false,
            Err(std::env::VarError::NotPresent) => client.as_ref().is_some_and(|c| c.keep_alive),
            _ => return Err(failed("invalid browser keep-alive policy")),
        };
        let mut lifetime = Self {
            auto_close: client.is_some() && !keep_alive,
            ..Self::default()
        };
        lifetime.touch(client.as_ref())?;
        Ok(lifetime)
    }

    pub(super) fn prune(&mut self) -> Result<(), AppError> {
        let mut active = Vec::new();
        for token in &self.clients {
            // An unreadable/damaged peer lease must protect the session without
            // blocking healthy clients from using it. Explicit recovery can clear it.
            if !matches!(client(token), Ok(None)) {
                active.push(token.clone());
            }
        }
        let mut users = Vec::new();
        for user in &self.users {
            if live(user).unwrap_or(true) {
                users.push(user.clone());
            }
        }
        self.clients = active;
        self.users = users;
        Ok(())
    }

    pub(super) fn touch(&mut self, client: Option<&Client>) -> Result<(), AppError> {
        if !self.auto_close {
            return Ok(());
        }
        self.prune()?;
        self.idle_since = None;
        if let Some(client) = client
            && !self.clients.contains(&client.token)
        {
            self.clients.push(client.token.clone());
        }
        // Also protect in-flight commands after their application's pipe closes,
        // and standalone CLI users sharing this browser (including long-lived TUIs).
        let (identity, _) = system::lease_process(std::process::id())?;
        if !self.users.contains(&identity) {
            self.users.push(identity);
        }
        Ok(())
    }
}

/// Register a reader before it can connect a CDP socket. Persistent sessions need no lease.
pub fn attach(port: u16) -> Result<bool, AppError> {
    let dir = directory(port)?;
    if !dir.try_exists()? {
        return Ok(false);
    }
    validate_directory(&dir)?;
    match load(&dir, port)? {
        None => return Ok(false),
        Some(state) if !state.lifetime.auto_close => return Ok(true),
        _ => {}
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("connection.lock"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(failed(
                    "another browser connection command is running; retry shortly",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    let _guard = Guard(lock);
    if let Some(mut state) = load(&dir, port)?
        && state.lifetime.auto_close
    {
        let client = current_client()?;
        state.lifetime.touch(client.as_ref())?;
        save(&dir, &state)?;
    }
    Ok(load(&dir, port)?.is_some())
}

async fn sweep_session(path: &Path, port: u16) -> Result<(bool, bool), AppError> {
    // Do not follow arbitrary directories while enumerating saved sessions.
    validate_directory(path)?;
    let Some(state) = load(path, port)? else {
        return Ok((false, false));
    };
    if !state.lifetime.auto_close {
        return Ok((false, false));
    }
    let result = manage_inner(
        port,
        DesktopAccount::Auto,
        Action::Recover,
        false,
        state.core,
        true,
    )
    .await?;
    Ok((result["pending"] == true, result["grace"] == true))
}

#[derive(Default)]
struct Sweeper {
    retries: HashMap<PathBuf, (std::time::Instant, u64, u8)>,
}
impl Sweeper {
    async fn sweep(&mut self, closing: bool) -> Result<(bool, bool, bool), AppError> {
        let base = data_directory()?;
        let mut pending = false;
        let mut failed = false;
        let mut grace = false;
        for entry in std::fs::read_dir(&base)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    failed = true;
                    continue;
                }
            };
            let Some(port) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u16>().ok())
            else {
                continue;
            };
            let path = entry.path();
            if let Some((until, _, attempts)) = self.retries.get(&path)
                && ((closing && *attempts >= 3) || *until > std::time::Instant::now())
            {
                failed = true;
                pending |= closing && *attempts < 3;
                continue;
            }
            match sweep_session(&path, port).await {
                Ok((waiting, in_grace)) => {
                    pending |= waiting;
                    grace |= in_grace;
                    self.retries.remove(&path);
                }
                Err(_) => {
                    failed = true;
                    let delay = self
                        .retries
                        .get(&path)
                        .map_or(2, |(_, delay, _)| (delay * 2).min(60));
                    let attempts = self
                        .retries
                        .get(&path)
                        .map_or(1, |(_, _, attempts)| attempts.saturating_add(1));
                    pending |= closing && attempts < 3;
                    self.retries.insert(
                        path,
                        (
                            std::time::Instant::now() + Duration::from_secs(delay),
                            delay,
                            attempts,
                        ),
                    );
                }
            }
        }
        // One damaged record must never prevent cleanup of independent browser sessions.
        // Registrations are immutable and unguessable. Only remove a record whose
        // owner is positively gone; never let a damaged entry block other sessions.
        if let Ok(entries) = std::fs::read_dir(clients_directory()?) {
            for entry in entries.flatten() {
                if let Some(token) = entry.file_name().to_str().filter(|s| valid_token(s))
                    && matches!(client(token), Ok(None))
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok((pending, failed, grace))
    }
}

/// Protocol 1: a single JSON ready line, then stdin stays open for the client's life.
/// The caller must give this process its own process group and must not inherit its
/// stdin into other children. Closing stdin requests cleanup; stdout EOF confirms it.
pub async fn supervise(keep_alive: bool) -> Result<(), AppError> {
    if std::io::stdin().is_terminal() {
        return Err(crate::error::AppError::InvalidInput(
            "desktop supervise requires a piped stdin owned by the calling application".into(),
        ));
    }
    let base = data_directory()?;
    prepare_directory(&base)?;
    let dir = clients_directory()?;
    prepare_directory(&dir)?;
    let token = format!("{:032x}", rand::random::<u128>());
    let record = Client {
        token: token.clone(),
        owner: system::lease_process(std::process::id())?.0,
        keep_alive,
    };
    let path = dir.join(&token);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(
        &serde_json::to_vec(&record).map_err(|_| failed("invalid browser supervisor record"))?,
    )?;
    file.sync_all()?;
    // Drop removes the registration even when stdout is closed during startup.
    struct Registration(PathBuf);
    impl Drop for Registration {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let registration = Registration(path);
    writeln!(
        std::io::stdout(),
        "{}",
        json!({"protocol": 1, "client": token})
    )?;
    std::io::stdout().flush()?;
    let mut sweeper = Sweeper::default();
    let mut input = tokio::io::stdin();
    let mut byte = [0; 1];
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            read = input.read(&mut byte) => {
                if read? == 0 { break; }
            }
            _ = timer.tick() => {
                // Recover abandoned leases left by a killed supervisor as well.
                // Fail closed on verification errors; do not repeatedly emit diagnostics.
                let _ = sweeper.sweep(false).await;
            }
        }
    }
    drop(registration);
    // Start a bounded retry budget afresh; background errors may have recovered.
    sweeper.retries.clear();
    loop {
        let (pending, failed_cleanup, grace) = sweeper.sweep(true).await?;
        if !pending {
            if failed_cleanup {
                return Err(failed(
                    "Some browser sessions could not be closed safely. Their records and sign-in were preserved; use `teams desktop status` and `teams desktop recover`.",
                ));
            }
            break;
        }
        tokio::time::sleep(if grace {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(2)
        })
        .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_failure_does_not_expire_a_live_lease() {
        let identity = Identity {
            pid: std::process::id(),
            started: "test".into(),
            executable: PathBuf::from("/test"),
        };
        assert!(live_with(&identity, |_| Err(failed("inspection unavailable"))).is_err());
        assert!(live_with(&identity, |_| Ok((identity.clone(), vec![]))).unwrap());
        let upgraded = Identity {
            executable: PathBuf::from("/test (deleted)"),
            ..identity.clone()
        };
        assert!(live_with(&identity, |_| Ok((upgraded, vec![]))).unwrap());
        let changed = Identity {
            started: "other".into(),
            ..identity.clone()
        };
        assert!(!live_with(&identity, |_| Ok((changed, vec![]))).unwrap());
    }

    #[test]
    fn a_reused_pid_is_expired_even_when_its_new_owner_has_another_uid() {
        // PID 1 belongs to the system on ordinary hosts. If procfs hides it,
        // uncertainty remains conservative instead of blocking other readers.
        let old = Identity {
            pid: 1,
            started: "not the current process start".into(),
            executable: PathBuf::new(),
        };
        if system::lease_process(1).is_ok() {
            assert!(!live(&old).unwrap());
        }
    }

    #[test]
    fn a_real_zombie_expires_without_reaping_it() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let identity = system::lease_process(child.id()).unwrap().0;
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let expired = loop {
            if matches!(live(&identity), Ok(false)) {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        child.wait().unwrap();
        assert!(
            expired,
            "an unreaped process must not retain a browser lease"
        );
    }

    #[test]
    fn inspection_failure_expires_a_process_confirmed_gone() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let identity = Identity {
            pid: child.id(),
            started: "test".into(),
            executable: PathBuf::from("/bin/sh"),
        };
        child.wait().unwrap();
        assert!(!live_with(&identity, |_| Err(failed("process gone"))).unwrap());
    }
}
