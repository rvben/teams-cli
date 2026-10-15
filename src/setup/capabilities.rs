use crate::config::DesktopClientKind;
use crate::{desktop, error::AppError};

#[derive(Clone, Debug)]
pub enum Availability {
    Ready { download: bool },
    Blocked(String),
}

impl Availability {
    fn from_check(result: Result<bool, AppError>) -> Self {
        match result {
            Ok(download) => Self::Ready { download },
            Err(error) => Self::Blocked(error.to_string()),
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ready { .. } => None,
            Self::Blocked(reason) => Some(reason),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub graph: Availability,
    pub pending: Vec<crate::config::PendingBrowserSetup>,
    pub browser: Availability,
    pub core: Availability,
    pub native: Availability,
    pub graph_device_code: bool,
}

impl Capabilities {
    pub async fn detect() -> Self {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let (browser, core, native) = tokio::join!(
            desktop::browser::preflight(false),
            desktop::browser::preflight(true),
            desktop::connection::preflight(),
        );
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let (browser, core, native) = {
            let unavailable = || {
                Err(AppError::Unsupported(
                "Browser mode needs macOS, Linux or WSL2. On Windows, run `teams init` inside WSL2.".into()))
            };
            (
                unavailable(),
                unavailable(),
                desktop::connection::preflight().await,
            )
        };
        #[allow(unused_mut)] // Only Unix has pending browser setup records.
        let mut detected = Self {
            graph: graph_availability(|key| std::env::var_os(key)),
            pending: Vec::new(),
            browser: Availability::from_check(browser),
            core: Availability::from_check(core),
            native: Availability::from_check(native.map(|()| false)),
            graph_device_code: graph_device_code_needed(),
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        match desktop::browser_setup::list() {
            Ok(pending) => detected.pending = pending,
            Err(error) => {
                let reason = format!(
                    "{error} Check the browser setup records before retrying; no saved sign-in was changed."
                );
                detected.browser = Availability::Blocked(reason.clone());
                detected.core = Availability::Blocked(reason);
            }
        }
        detected
    }

    pub fn availability(&self, kind: DesktopClientKind) -> &Availability {
        match kind {
            DesktopClientKind::Browser => &self.browser,
            DesktopClientKind::BrowserCore => &self.core,
            DesktopClientKind::Native => &self.native,
        }
    }

    pub fn preferred(&self, personal: bool) -> Option<DesktopClientKind> {
        use DesktopClientKind::{Browser, BrowserCore, Native};
        let preference: &[_] = if personal {
            &[BrowserCore, Browser, Native]
        } else {
            &[Browser, Native]
        };
        preference
            .iter()
            .copied()
            .find(|kind| self.availability(*kind).reason().is_none())
    }
}

pub fn graph_device_code_needed() -> bool {
    cfg!(target_os = "linux")
        && !["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

fn graph_availability(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Availability {
    let blocked = if get("TEAMS_ACCESS_TOKEN").is_some() {
        Some(
            "TEAMS_ACCESS_TOKEN overrides browser sign-in. Unset it before guided Graph setup, or use explicit init flags for automation.",
        )
    } else if get("TEAMS_READ_ONLY").is_some_and(|value| {
        matches!(
            value
                .to_str()
                .map(str::trim)
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("false" | "0" | "no" | "off")
        )
    }) {
        Some(
            "TEAMS_READ_ONLY=false overrides Graph's saved read-only setting. Unset it before guided Graph setup, or use explicit init flags.",
        )
    } else if get("TEAMS_CLIENT_ID").is_some() {
        Some(
            "TEAMS_CLIENT_ID overrides saved Graph application settings. Unset it before guided setup, or use explicit init flags for a custom application.",
        )
    } else if get("TEAMS_TENANT").is_some() {
        Some(
            "TEAMS_TENANT overrides saved Graph tenant settings. Unset it before guided setup, or use explicit init flags for a custom tenant.",
        )
    } else {
        None
    };
    match blocked {
        Some(reason) => Availability::Blocked(reason.into()),
        None => Availability::Ready { download: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_graph_excludes_overrides_that_could_verify_another_session_or_unlock_reads() {
        for (key, value) in [
            ("TEAMS_ACCESS_TOKEN", "synthetic-token"),
            ("TEAMS_ACCESS_TOKEN", ""),
            ("TEAMS_READ_ONLY", "false"),
            ("TEAMS_READ_ONLY", " OFF "),
            ("TEAMS_CLIENT_ID", " "),
            ("TEAMS_CLIENT_ID", "synthetic-app"),
            ("TEAMS_TENANT", ""),
            ("TEAMS_TENANT", "synthetic-tenant"),
        ] {
            let availability = graph_availability(|name| (name == key).then(|| value.into()));
            assert!(availability.reason().unwrap().contains(key));
            if value.starts_with("synthetic-") {
                assert!(!availability.reason().unwrap().contains(value));
            }
        }
        for value in [None, Some("true")] {
            assert!(
                graph_availability(|name| {
                    (name == "TEAMS_READ_ONLY")
                        .then_some(value)
                        .flatten()
                        .map(Into::into)
                })
                .reason()
                .is_none()
            );
        }
    }
}
