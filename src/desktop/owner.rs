//! Listener verification: loopback-only, owned by this user's installed Teams package.
use super::{ConnectionMetadata, ProbeStatus, fill_metadata};
use crate::error::AppError;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const TEAMS_EXECUTABLE: &str = "ms-teams.exe";
const WEBVIEW_EXECUTABLE: &str = "msedgewebview2.exe";

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
    /// Whether the user-wide WebView2 arguments contain remote-debugging flags,
    /// which open the port in every WebView2 application the user starts.
    fn user_debugging_flags(&self) -> bool;
}

impl<S: System> super::session::Verifier for S {
    async fn verify(&self, port: u16) -> Result<(ConnectionMetadata, Vec<Value>), AppError> {
        verify(self, port).await
    }
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
    let foreign: Vec<_> = listeners
        .iter()
        .filter(|listener| {
            !teams_ancestor(&processes, listener.pid).is_some_and(|pid| system.trusted_teams(pid))
        })
        .collect();
    metadata.teams_owned = Some(foreign.is_empty());
    if !foreign.is_empty() {
        metadata.status = ProbeStatus::NonTeamsListenerRefused;
        let mut names: Vec<_> = foreign
            .iter()
            .filter_map(|listener| host_name(&processes, listener.pid))
            .collect();
        names.sort_unstable_by_key(|name| name.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        metadata.foreign_listeners = names;
        metadata.user_debugging_flags = system.user_debugging_flags();
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

/// The application a listener serves: WebView2 processes are attributed to the
/// nearest ancestor that is not WebView2 itself. Only short, plain names are reported.
fn host_name(processes: &HashMap<u32, (u32, String)>, mut pid: u32) -> Option<String> {
    let mut seen = HashSet::new();
    let mut name = None;
    for _ in 0..32 {
        if pid == 0 || !seen.insert(pid) {
            break;
        }
        let Some((parent, current)) = processes.get(&pid) else {
            break;
        };
        name = Some(current);
        if !current.eq_ignore_ascii_case(WEBVIEW_EXECUTABLE) {
            break;
        }
        pid = *parent;
    }
    name.filter(|name| {
        name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
    })
    .cloned()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub struct FakeSystem {
        pub listeners: Vec<(u32, bool)>,
        pub processes: Vec<(u32, u32, &'static str)>,
        pub trusted: Vec<u32>,
        pub user_flags: bool,
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
                user_flags: false,
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
        fn user_debugging_flags(&self) -> bool {
            self.user_flags
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

    #[tokio::test]
    async fn refusals_name_the_host_application_and_the_user_wide_flags() {
        // Outlook's WebView2 opened the port because of a user-wide setting.
        let mut outlook = FakeSystem::teams();
        outlook.listeners.push((42, true));
        outlook.processes.extend([
            (42, 41, "msedgewebview2.exe"),
            (41, 40, "msedgewebview2.exe"),
            (40, 10, "OUTLOOK.EXE"),
            (43, 10, "Widgets.exe"),
        ]);
        outlook.user_flags = true;
        let (metadata, _) = verify(&outlook, 9222).await.unwrap();
        assert_eq!(metadata.foreign_listeners, ["OUTLOOK.EXE"]);
        assert!(metadata.user_debugging_flags);
        assert!(metadata.detail().contains("served by OUTLOOK.EXE"));
        assert!(metadata.detail().contains("teams desktop clear-flags"));
        assert_eq!(
            metadata.stray_debugging_flags(),
            Some(["OUTLOOK.EXE".to_string()].as_slice())
        );
        // Several listeners from one app are named once; Teams itself is not named.
        outlook.listeners.extend([(42, true), (43, true)]);
        let (metadata, _) = verify(&outlook, 9222).await.unwrap();
        assert_eq!(metadata.foreign_listeners, ["OUTLOOK.EXE", "Widgets.exe"]);
        // An untrusted Teams copy and an unknown process: nothing to clean up.
        let mut other = FakeSystem {
            trusted: vec![],
            ..FakeSystem::teams()
        };
        other.listeners.push((77, true));
        other.processes.push((50, 10, "bad\\name<script>.exe"));
        other.listeners.push((50, true));
        let (metadata, _) = verify(&other, 9222).await.unwrap();
        assert_eq!(metadata.foreign_listeners, ["ms-teams.exe"]);
        assert!(!metadata.user_debugging_flags);
        assert_eq!(metadata.stray_debugging_flags(), None);
        let json = serde_json::to_value(&metadata).unwrap();
        assert!(json.get("user_debugging_flags").is_none());
        // A healthy probe never reports either field.
        let mut healthy = FakeSystem::teams();
        healthy.user_flags = true;
        let (metadata, _) = verify(&healthy, 9222).await.unwrap();
        assert!(metadata.foreign_listeners.is_empty() && !metadata.user_debugging_flags);
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
