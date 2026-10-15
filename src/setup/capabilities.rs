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
        Self {
            browser: Availability::from_check(browser),
            core: Availability::from_check(core),
            native: Availability::from_check(native.map(|()| false)),
            graph_device_code: cfg!(target_os = "linux")
                && !["DISPLAY", "WAYLAND_DISPLAY"]
                    .iter()
                    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty())),
        }
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
