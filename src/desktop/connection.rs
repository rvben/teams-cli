//! Explicit, recoverable debugging launches. Never restarts Teams on process exit.
use super::{DesktopClient, ProbeStatus};
use crate::error::AppError;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::process::Command;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Action {
    Status,
    Connect,
    Disconnect,
    Recover,
}

fn failure(code: &str) -> AppError {
    let message = match code {
        "busy" => "Another desktop connection command is running; try again shortly",
        "different_port" => {
            "Another port has a managed session; select that profile and disconnect or recover it first"
        }
        "already_managed" => {
            "A saved desktop session needs attention; run `teams desktop recover`, then disconnect it before connecting again"
        }
        "existing_flags" => {
            "Existing remote-debugging flags were preserved; close that debugging session before using managed connect"
        }
        "settings_changed" => {
            "Debugging settings changed outside this CLI and were preserved; restore the prior settings manually before retrying recovery"
        }
        "not_managed" | "session_changed" => {
            "This Teams process was not started by this managed session; it was left running. Quit Teams completely and relaunch it normally, then run `teams desktop disconnect` to clear the record"
        }
        "invalid_state" | "missing_state" => {
            "The desktop recovery record is missing or invalid; no settings were changed. Close debugging manually using `teams doctor`"
        }
        _ => {
            "Desktop connection operation failed; run `teams desktop recover` to restore interrupted setup settings, then `teams doctor`"
        }
    };
    AppError::Desktop(message.into())
}

/// Includes metadata only. An available endpoint does not establish sign-in.
pub async fn manage(client: &DesktopClient, action: Action) -> Result<Value, AppError> {
    super::platform()?;
    let token = format!("{:032x}", rand::random::<u128>());
    if matches!(action, Action::Recover) {
        let state = local("recover", client.port, false, None).await?;
        return Ok(
            json!({"port":client.port,"endpoint_available":null,"session_verified":false,"management":state,"connection":null}),
        );
    }
    let metadata = client.probe().await?;
    if matches!(action, Action::Connect) && !metadata.endpoint_available() {
        if metadata.status != ProbeStatus::NoListener {
            return Err(AppError::Desktop(metadata.status.detail().into()));
        }
        // Even if starting or waiting fails, preserve a recovery record and try to
        // restore the temporary user variable. A killed process can recover next run.
        let result = async {
            local("connect", client.port, false, Some(&token)).await?;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let current = client.probe().await?;
                if current.endpoint_available() {
                    local("finish", client.port, false, Some(&token)).await?;
                    return Ok(());
                }
                if current.status != ProbeStatus::NoListener {
                    return Err(AppError::Desktop(current.status.detail().into()));
                }
            }
            Err(AppError::Desktop("Teams has not opened its endpoint; run `teams desktop status` and `teams desktop recover` before trying again".into()))
        }.await;
        if let Err(error) = result {
            if let Err(recovery) = local("recover", client.port, false, Some(&token)).await {
                return Err(AppError::Desktop(format!("{error}. Recovery: {recovery}")));
            }
            return Err(error);
        }
    } else if matches!(action, Action::Disconnect) {
        if !metadata.endpoint_available() && metadata.status != ProbeStatus::NoListener {
            return Err(AppError::Desktop(metadata.status.detail().into()));
        }
        local(
            "disconnect",
            client.port,
            metadata.endpoint_available(),
            None,
        )
        .await?;
    }
    let state = local("status", client.port, false, None).await?;
    let metadata = if matches!(action, Action::Status) {
        metadata
    } else {
        client.probe().await?
    };
    Ok(
        json!({"port":client.port,"endpoint_available":metadata.endpoint_available(),"session_verified":false,"management":state,"connection":metadata}),
    )
}

async fn local(
    action: &str,
    port: u16,
    restart: bool,
    token: Option<&str>,
) -> Result<Value, AppError> {
    #[cfg(target_os = "macos")]
    {
        mac::manage(action, port, restart, token).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        powershell(action, port, restart, token).await
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn powershell_path() -> Result<PathBuf, AppError> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        candidates.push(PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"));
    }
    candidates.push(PathBuf::from(
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
    ));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("powershell.exe")));
    }
    candidates.into_iter().find(|path| path.is_file()).ok_or_else(|| AppError::Desktop("Windows PowerShell was not found; enable WSL Windows executable interop or add its WindowsPowerShell/v1.0 directory to PATH".into()))
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
async fn powershell(
    action: &str,
    port: u16,
    restart: bool,
    token: Option<&str>,
) -> Result<Value, AppError> {
    let request = json!({"action":action,"port":port,"restart":restart,"token":token});
    let script = format!(
        "$request = ConvertFrom-Json '{request}';\n{}",
        include_str!("connection.ps1")
    );
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut command = Command::new(powershell_path()?);
    command.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
    let bytes = run(command).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| failure("operation_failed"))?;
    if let Some(code) = value["error"].as_str() {
        return Err(failure(code));
    }
    Ok(value)
}

async fn run(mut command: Command) -> Result<Vec<u8>, AppError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| {
            AppError::Desktop(
                "Desktop connection operation timed out; run `teams desktop recover`".into(),
            )
        })?
        .map_err(|_| failure("operation_failed"))?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Err(failure("operation_failed"));
    }
    Ok(output.stdout)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        os::unix::fs::OpenOptionsExt,
    };

    #[derive(Serialize, Deserialize)]
    struct State {
        version: u8,
        port: u16,
        phase: String,
        token: String,
        previous: Option<String>,
        owners: Vec<String>,
    }

    fn save(path: &std::path::Path, state: &State) -> Result<(), AppError> {
        let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("state directory"))?;
        file.write_all(&serde_json::to_vec(state).map_err(|_| failure("invalid_state"))?)?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
    async fn owners() -> Result<Vec<String>, AppError> {
        let mut command = Command::new("/bin/ps");
        command.args(["-axo", "pid=,lstart=,comm="]);
        let text =
            String::from_utf8(run(command).await?).map_err(|_| failure("operation_failed"))?;
        Ok(text
            .lines()
            .filter(|line| {
                line.trim_end()
                    .ends_with("/Microsoft Teams.app/Contents/MacOS/MSTeams")
            })
            .map(|s| s.trim().to_string())
            .collect())
    }
    async fn launch(value: &str) -> Result<(), AppError> {
        let mut quit = Command::new("/usr/bin/osascript");
        quit.args(["-e", "tell application id \"com.microsoft.teams2\" to quit"]);
        run(quit).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut open = Command::new("/usr/bin/open");
        open.args([
            "-b",
            "com.microsoft.teams2",
            "--env",
            &format!("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS={value}"),
        ]);
        run(open).await?;
        Ok(())
    }
    pub async fn manage(
        action: &str,
        port: u16,
        restart: bool,
        token: Option<&str>,
    ) -> Result<Value, AppError> {
        let dir = dirs::data_local_dir()
            .ok_or_else(|| failure("invalid_state"))?
            .join("teams-cli");
        manage_at(&dir, action, port, restart, token).await
    }
    async fn manage_at(
        dir: &std::path::Path,
        action: &str,
        port: u16,
        restart: bool,
        token: Option<&str>,
    ) -> Result<Value, AppError> {
        std::fs::create_dir_all(dir)?;
        let lock: File = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("desktop-connection.lock"))?;
        lock.try_lock().map_err(|_| failure("busy"))?;
        let path = dir.join("desktop-connection.json");
        let mut state: Option<State> = match std::fs::read(&path) {
            Ok(bytes) => {
                Some(serde_json::from_slice(&bytes).map_err(|_| failure("invalid_state"))?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if state
            .as_ref()
            .is_some_and(|s| s.version != 1 || s.port < 1024)
        {
            return Err(failure("invalid_state"));
        }
        if action != "status" && state.as_ref().is_some_and(|s| s.port != port) {
            return Err(failure("different_port"));
        }
        if token.is_some()
            && state
                .as_ref()
                .is_some_and(|s| Some(s.token.as_str()) != token)
        {
            return Err(failure("busy"));
        }
        match action {
            "connect" => {
                if state.is_some() {
                    return Err(failure("already_managed"));
                }
                let previous = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok();
                if previous
                    .as_ref()
                    .is_some_and(|s| s.contains("--remote-debugging-"))
                {
                    return Err(failure("existing_flags"));
                }
                let next = State {
                    version: 1,
                    port,
                    phase: "starting".into(),
                    token: token.unwrap_or_default().into(),
                    previous,
                    owners: vec![],
                };
                save(&path, &next)?;
                launch(&format!(
                    "{} --remote-debugging-port={port} --remote-debugging-address=127.0.0.1",
                    next.previous.as_deref().unwrap_or_default()
                ))
                .await?;
                state = Some(next);
            }
            "finish" => {
                let current = state.as_mut().ok_or_else(|| failure("missing_state"))?;
                if current.phase != "starting" {
                    return Err(failure("busy"));
                }
                current.owners = owners().await?;
                if current.owners.is_empty() {
                    return Err(failure("session_changed"));
                }
                current.phase = "active".into();
                save(&path, current)?;
            }
            "disconnect" => {
                let Some(current) = state.as_ref() else {
                    if restart {
                        return Err(failure("not_managed"));
                    }
                    return Ok(
                        json!({"managed":false,"port":null,"phase":"idle","settings_pending":false,"needs_recovery":false}),
                    );
                };
                if current.phase == "starting" {
                    return Err(failure("busy"));
                }
                if restart {
                    if current.owners.is_empty() || current.owners != owners().await? {
                        return Err(failure("session_changed"));
                    }
                    launch(current.previous.as_deref().unwrap_or_default()).await?;
                }
                std::fs::remove_file(&path)?;
                state = None;
            }
            "recover" => {
                if let Some(current) = state.as_mut()
                    && current.phase == "starting"
                {
                    current.phase = "interrupted".into();
                    save(&path, current)?;
                }
            }
            "status" => {}
            _ => return Err(failure("invalid_action")),
        }
        Ok(
            json!({"managed":state.is_some(),"port":state.as_ref().map(|s|s.port),"phase":state.as_ref().map(|s|s.phase.as_str()).unwrap_or("idle"),"settings_pending":false,"needs_recovery":state.as_ref().is_some_and(|s|s.phase != "active")}),
        )
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn interrupted(port: u16) -> State {
            State {
                version: 1,
                port,
                phase: "starting".into(),
                token: "first".into(),
                previous: None,
                owners: vec![],
            }
        }
        #[tokio::test]
        async fn disconnect_is_idempotent_but_never_restarts_an_unmanaged_process() {
            let temp = tempfile::tempdir().unwrap();
            assert_eq!(
                manage_at(temp.path(), "disconnect", 19222, false, None)
                    .await
                    .unwrap()["managed"],
                false
            );
            assert!(
                manage_at(temp.path(), "disconnect", 19222, true, None)
                    .await
                    .is_err()
            );
            assert!(!temp.path().join("desktop-connection.json").exists());
        }
        #[tokio::test]
        async fn recovery_is_local_and_does_not_adopt_an_unrecorded_process() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("desktop-connection.json");
            save(&path, &interrupted(19222)).unwrap();
            let result = manage_at(temp.path(), "recover", 19222, false, None)
                .await
                .unwrap();
            assert_eq!(result["phase"], "interrupted");
            assert_eq!(result["settings_pending"], false);
            assert!(
                manage_at(temp.path(), "disconnect", 19222, true, None)
                    .await
                    .is_err()
            );
            assert!(path.exists());
            assert_eq!(
                manage_at(temp.path(), "disconnect", 19222, false, None)
                    .await
                    .unwrap()["managed"],
                false
            );
        }
        #[tokio::test]
        async fn competing_owner_wrong_port_and_corrupt_records_are_preserved() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("desktop-connection.json");
            save(&path, &interrupted(19222)).unwrap();
            let original = std::fs::read(&path).unwrap();
            assert!(
                manage_at(temp.path(), "recover", 19222, false, Some("second"))
                    .await
                    .is_err()
            );
            assert!(
                manage_at(temp.path(), "recover", 19223, false, None)
                    .await
                    .is_err()
            );
            assert!(
                manage_at(temp.path(), "disconnect", 19222, false, None)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
            std::fs::write(&path, b"not-json").unwrap();
            assert!(
                manage_at(temp.path(), "recover", 19222, false, None)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"not-json");
        }
        #[tokio::test]
        async fn lock_contention_never_changes_a_record() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("desktop-connection.json");
            save(&path, &interrupted(19222)).unwrap();
            let original = std::fs::read(&path).unwrap();
            let lock = File::create(temp.path().join("desktop-connection.lock")).unwrap();
            lock.try_lock().unwrap();
            assert!(
                manage_at(temp.path(), "recover", 19222, false, None)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
}
