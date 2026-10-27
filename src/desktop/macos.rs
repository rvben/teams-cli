//! macOS endpoint verification. The listener must descend from this user's installed
//! Teams executable; no connection is attempted until every socket passes the checks.
#[cfg(target_os = "macos")]
use super::{ConnectionMetadata, ProbeStatus, fill_metadata, session::Verifier};
use crate::error::AppError;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
#[cfg(target_os = "macos")]
use {
    serde_json::Value,
    std::{process::Stdio, time::Duration},
    tokio::process::Command,
};

#[cfg(target_os = "macos")]
pub struct MacSystem;

#[derive(Debug, PartialEq)]
struct Process {
    uid: u32,
    parent: u32,
    executable: PathBuf,
}

#[cfg(target_os = "macos")]
async fn run(mut command: Command) -> Result<std::process::Output, AppError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|_| AppError::Desktop("could not start the local ownership check".into()))?;
    tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .map_err(|_| AppError::Desktop("desktop ownership check timed out".into()))?
        .map_err(|_| AppError::Desktop("could not read desktop ownership metadata".into()))
}

#[cfg(target_os = "macos")]
impl Verifier for MacSystem {
    async fn verify(&self, port: u16) -> Result<(ConnectionMetadata, Vec<Value>), AppError> {
        super::local_prerequisites()?;
        let mut metadata = ConnectionMetadata::empty(port);
        let mut command = Command::new("/usr/sbin/lsof");
        command.args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"]);
        let output = run(command).await?;
        let listeners = parse_listeners(&String::from_utf8_lossy(&output.stdout))?;
        if listeners.is_empty() && output.status.code() == Some(1) {
            return Ok((metadata, vec![]));
        }
        if !output.status.success() || listeners.is_empty() {
            return Err(AppError::Desktop("listener inspection failed".into()));
        }
        metadata.loopback_only = Some(
            listeners
                .iter()
                .all(|(_, address)| loopback_address(address, port)),
        );
        if metadata.loopback_only != Some(true) {
            metadata.status = ProbeStatus::UnsafeListenerRefused;
            return Ok((metadata, vec![]));
        }
        let mut command = Command::new("/usr/bin/id");
        command.arg("-u");
        let output = run(command).await?;
        let uid = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|_| output.status.success())
            .ok_or_else(|| {
                AppError::Desktop("could not verify the desktop process owner".into())
            })?;
        let mut command = Command::new("/bin/ps");
        command.args(["-axo", "pid=,uid=,ppid=,comm="]);
        let output = run(command).await?;
        if !output.status.success() {
            return Err(AppError::Desktop("process inspection failed".into()));
        }
        let processes = parse_processes(&String::from_utf8_lossy(&output.stdout))?;
        let mut installed = vec![PathBuf::from(
            "/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams",
        )];
        if let Some(home) = dirs::home_dir() {
            installed.push(home.join("Applications/Microsoft Teams.app/Contents/MacOS/MSTeams"));
        }
        installed.retain(|path| path.is_file());
        let owned = listeners
            .iter()
            .all(|(pid, _)| teams_owner(&processes, *pid, uid, &installed));
        metadata.teams_owned = Some(owned);
        if !owned {
            metadata.status = ProbeStatus::NonTeamsListenerRefused;
            return Ok((metadata, vec![]));
        }
        let targets = fill_metadata(&mut metadata).await;
        Ok((metadata, targets))
    }
}

pub(super) fn parse_listeners(text: &str) -> Result<Vec<(u32, String)>, AppError> {
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

pub(super) fn loopback_address(address: &str, port: u16) -> bool {
    address == format!("127.0.0.1:{port}") || address == format!("[::1]:{port}")
}

fn parse_processes(text: &str) -> Result<HashMap<u32, Process>, AppError> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let invalid = || AppError::Desktop("invalid process ancestry metadata".into());
            let mut rest = line.trim();
            let mut field = || {
                let (value, tail) = rest.split_once(char::is_whitespace).ok_or_else(invalid)?;
                rest = tail.trim_start();
                Ok::<_, AppError>(value)
            };
            let pid = field()?.parse::<u32>().map_err(|_| invalid())?;
            let uid = field()?;
            // macOS ps prints some uid_t values as signed integers (for
            // example nobody as -2). Preserve their underlying 32-bit value.
            let uid = uid
                .parse::<u32>()
                .or_else(|_| uid.parse::<i32>().map(|uid| uid as u32))
                .map_err(|_| invalid())?;
            let parent = field()?.parse::<u32>().map_err(|_| invalid())?;
            if rest.is_empty() {
                return Err(invalid());
            }
            Ok((
                pid,
                Process {
                    uid,
                    parent,
                    executable: rest.into(),
                },
            ))
        })
        .collect()
}

fn teams_owner(
    processes: &HashMap<u32, Process>,
    mut pid: u32,
    uid: u32,
    installed: &[PathBuf],
) -> bool {
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if pid == 0 || !seen.insert(pid) {
            return false;
        }
        let Some(process) = processes.get(&pid) else {
            return false;
        };
        if process.uid != uid {
            return false;
        }
        if installed.contains(&process.executable) {
            return true;
        }
        pid = process.parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_system_uids_do_not_break_listener_ownership_checks() {
        let installed = [PathBuf::from("/Applications/Teams")];
        let processes = parse_processes(
            "1 0 0 /sbin/launchd\n2 -2 1 /system/service\n3 501 1 /Applications/Teams\n4 -2 3 /helper\n5 4294967294 1 /system/service\n",
        )
        .unwrap();
        assert_eq!(processes[&2].uid, u32::MAX - 1);
        assert_eq!(processes[&5].uid, processes[&2].uid);
        assert!(teams_owner(&processes, 3, 501, &installed));
        assert!(!teams_owner(&processes, 2, 501, &installed));
        assert!(!teams_owner(&processes, 4, 501, &installed));
        for malformed in [
            "-2 501 1 /process",
            "2 -2147483649 1 /process",
            "2 4294967296 1 /process",
            "2 501 -1 /process",
        ] {
            assert!(parse_processes(malformed).is_err());
        }
    }

    #[test]
    fn ownership_requires_the_installed_path_and_current_user_throughout_the_chain() {
        let installed = [PathBuf::from(
            "/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams",
        )];
        let mut processes = parse_processes(" 30 501 20 /Applications/Microsoft Teams.app/Contents/Helpers/WebView\n 20 501 1 /Applications/Microsoft Teams.app/Contents/MacOS/MSTeams\n").unwrap();
        assert!(teams_owner(&processes, 30, 501, &installed));
        assert!(!teams_owner(&processes, 30, 502, &installed));
        assert!(!teams_owner(&processes, 30, 501, &[]));
        assert!(!teams_owner(&processes, 99, 501, &installed));
        processes.get_mut(&30).unwrap().uid = 502;
        assert!(!teams_owner(&processes, 30, 501, &installed));
        processes.get_mut(&30).unwrap().uid = 501;
        processes.get_mut(&20).unwrap().executable =
            "/tmp/Microsoft Teams.app/Contents/MacOS/MSTeams".into();
        assert!(!teams_owner(&processes, 30, 501, &installed));
        processes.get_mut(&20).unwrap().parent = 30;
        assert!(!teams_owner(&processes, 30, 501, &installed));
        for malformed in ["oops", "1 501 x /Teams", "1 501 0 "] {
            assert!(parse_processes(malformed).is_err());
        }
    }
}
