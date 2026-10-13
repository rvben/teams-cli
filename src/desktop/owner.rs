//! Listener verification: loopback-only, owned by this user's installed Teams package.
use super::{ConnectionMetadata, ProbeStatus, fill_metadata};
use crate::error::AppError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const TEAMS_EXECUTABLE: &str = "ms-teams.exe";

pub struct Listener {
    pub pid: u32,
    pub loopback: bool,
}

/// The operating-system facts verification relies on, so the decision logic is
/// testable everywhere.
pub trait System: Send + Sync {
    /// Every listening socket on `port`, IPv4 and IPv6.
    fn listeners(&self, port: u16) -> Result<Vec<Listener>, AppError>;
    /// One snapshot of pid -> (parent pid, executable file name).
    fn processes(&self) -> Result<HashMap<u32, (u32, String)>, AppError>;
    /// Whether `pid` belongs to the installed Teams package and runs as the current user.
    fn trusted_teams(&self, pid: u32) -> bool;
}

/// Verifies the listener on `port` and, only if it is safe, reads endpoint metadata.
/// The raw targets are returned for page selection and never leave the process.
pub async fn verify(
    system: &impl System,
    port: u16,
) -> Result<(ConnectionMetadata, Vec<Value>), AppError> {
    let mut metadata = ConnectionMetadata::empty(port);
    let listeners = system.listeners(port)?;
    if listeners.is_empty() {
        return Ok((metadata, vec![]));
    }
    let loopback = listeners.iter().all(|l| l.loopback);
    metadata.loopback_only = Some(loopback);
    if !loopback {
        metadata.status = ProbeStatus::UnsafeListenerRefused;
        return Ok((metadata, vec![]));
    }
    let processes = system.processes()?;
    let owned = listeners.iter().all(|listener| {
        teams_ancestor(&processes, listener.pid).is_some_and(|pid| system.trusted_teams(pid))
    });
    metadata.teams_owned = Some(owned);
    if !owned {
        metadata.status = ProbeStatus::NonTeamsListenerRefused;
        return Ok((metadata, vec![]));
    }
    let targets = fill_metadata(&mut metadata).await;
    Ok((metadata, targets))
}

/// The nearest `ms-teams.exe` in the listener's ancestry; Teams serves the endpoint
/// from a child process.
fn teams_ancestor(processes: &HashMap<u32, (u32, String)>, mut pid: u32) -> Option<u32> {
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if pid == 0 || !seen.insert(pid) {
            return None;
        }
        let (parent, name) = processes.get(&pid)?;
        if name.eq_ignore_ascii_case(TEAMS_EXECUTABLE) {
            return Some(pid);
        }
        pid = *parent;
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub struct FakeSystem {
        pub listeners: Vec<(u32, bool)>,
        pub processes: Vec<(u32, u32, &'static str)>,
        pub trusted: Vec<u32>,
    }

    impl FakeSystem {
        /// A Teams-owned loopback listener served from a WebView child process.
        pub fn teams() -> Self {
            Self {
                listeners: vec![(30, true), (30, true)],
                processes: vec![
                    (30, 20, "msedgewebview2.exe"),
                    (20, 10, "ms-teams.exe"),
                    (10, 1, "explorer.exe"),
                ],
                trusted: vec![20],
            }
        }
    }

    impl System for FakeSystem {
        fn listeners(&self, _port: u16) -> Result<Vec<Listener>, AppError> {
            Ok(self
                .listeners
                .iter()
                .map(|&(pid, loopback)| Listener { pid, loopback })
                .collect())
        }
        fn processes(&self) -> Result<HashMap<u32, (u32, String)>, AppError> {
            Ok(self
                .processes
                .iter()
                .map(|&(pid, parent, name)| (pid, (parent, name.into())))
                .collect())
        }
        fn trusted_teams(&self, pid: u32) -> bool {
            self.trusted.contains(&pid)
        }
    }

    #[tokio::test]
    async fn refuses_unsafe_foreign_and_untrusted_listeners() {
        let status = |system: FakeSystem| async move {
            let (metadata, targets) = verify(&system, 9222).await.unwrap();
            assert!(targets.is_empty());
            (
                metadata.status,
                metadata.loopback_only,
                metadata.teams_owned,
            )
        };
        let none = FakeSystem {
            listeners: vec![],
            ..FakeSystem::teams()
        };
        assert_eq!(status(none).await, (ProbeStatus::NoListener, None, None));
        let mut exposed = FakeSystem::teams();
        exposed.listeners.push((30, false));
        assert_eq!(
            status(exposed).await,
            (ProbeStatus::UnsafeListenerRefused, Some(false), None)
        );
        // Wrong package or another user's Teams: the system check fails.
        let untrusted = FakeSystem {
            trusted: vec![],
            ..FakeSystem::teams()
        };
        assert_eq!(
            status(untrusted).await,
            (
                ProbeStatus::NonTeamsListenerRefused,
                Some(true),
                Some(false)
            )
        );
        // A second listener from an unrelated process taints the port.
        let mut mixed = FakeSystem::teams();
        mixed.listeners.push((40, true));
        mixed.processes.push((40, 10, "node.exe"));
        assert_eq!(
            status(mixed).await,
            (
                ProbeStatus::NonTeamsListenerRefused,
                Some(true),
                Some(false)
            )
        );
    }

    #[test]
    fn ancestry_finds_the_nearest_teams_process_and_stops_on_cycles() {
        let processes = FakeSystem::teams().processes().unwrap();
        assert_eq!(teams_ancestor(&processes, 30), Some(20));
        assert_eq!(teams_ancestor(&processes, 10), None);
        assert_eq!(teams_ancestor(&processes, 99), None);
        let cycle: HashMap<_, _> = [(1, (2, "a.exe".into())), (2, (1, "b.exe".into()))].into();
        assert_eq!(teams_ancestor(&cycle, 1), None);
        let upper: HashMap<_, _> = [(5, (0, "MS-Teams.EXE".into()))].into();
        assert_eq!(teams_ancestor(&upper, 5), Some(5));
    }

    #[tokio::test]
    async fn trusted_listeners_report_sanitized_metadata() {
        let port = crate::desktop::cdp::tests::devtools_server(
            json!([{"type": "page", "url": "https://teams.microsoft.com/v2/?PRIVATE", "id": "ABCDEF12"}]),
            |_| None,
        )
        .await;
        let (metadata, targets) = verify(&FakeSystem::teams(), port).await.unwrap();
        assert!(metadata.endpoint_available());
        assert_eq!(targets.len(), 1);
        let output = serde_json::to_string(&metadata).unwrap();
        assert!(output.contains("https://teams.microsoft.com") && !output.contains("PRIVATE"));
    }
}
