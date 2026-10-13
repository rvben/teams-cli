use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The maintained, multitenant public-client registration shipped with teams-cli.
pub const DEFAULT_CLIENT_ID: &str = "66ebad71-1604-48fc-a086-0d4caa24988b";
pub const DEFAULT_TENANT: &str = "organizations";
pub const DEFAULT_DESKTOP_PORT: u16 = 9222;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Graph,
    Desktop,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub backend: BackendKind,
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_desktop_port")]
    pub desktop_port: u16,
}

impl Profile {
    pub fn require_graph(&self) -> Result<(), AppError> {
        if self.backend == BackendKind::Desktop {
            Err(crate::desktop::messaging_unsupported())
        } else {
            Ok(())
        }
    }

    pub fn require_writable(&self) -> Result<(), AppError> {
        if self.backend == BackendKind::Desktop {
            return Err(crate::desktop::messaging_unsupported());
        }
        if self.read_only {
            Err(AppError::ReadOnly(
                "read-only mode is enabled (unset TEAMS_READ_ONLY or disable read_only in the active profile to allow writes)"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }
}

fn default_tenant() -> String {
    DEFAULT_TENANT.into()
}

fn default_desktop_port() -> u16 {
    DEFAULT_DESKTOP_PORT
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    active_profile: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub active: bool,
    pub tenant: String,
    pub client_id: String,
    pub read_only: bool,
    pub backend: BackendKind,
}

pub fn path() -> PathBuf {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(base).join("teams").join("config.toml")
    } else {
        config_base().join("teams").join("config.toml")
    }
}

pub fn exists() -> bool {
    path().is_file()
}

pub fn save(profile_name: &str, profile: Profile) -> Result<PathBuf, AppError> {
    let path = path();
    let mut config = read_file()?;
    config.profiles.insert(profile_name.to_string(), profile);
    config.active_profile = Some(profile_name.to_string());
    write_file(&path, &config)?;
    Ok(path)
}

fn write_file(path: &std::path::Path, config: &ConfigFile) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Unexpected("configuration path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let body = toml::to_string_pretty(&config).map_err(|e| AppError::Unexpected(e.to_string()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".toml.tmp")
        .tempfile_in(parent)?;
    temp.write_all(body.as_bytes())?;
    temp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn load(requested: Option<&str>) -> Result<(String, Profile), AppError> {
    let config = read_file()?;
    let name = selected_profile_name_from(requested, config.active_profile.as_deref());
    let stored = config.profiles.get(&name);
    let backend = stored.map(|p| p.backend).unwrap_or_default();
    let client_id = if backend == BackendKind::Desktop {
        String::new()
    } else {
        std::env::var("TEAMS_CLIENT_ID")
            .ok()
            .or_else(|| stored.map(|profile| profile.client_id.clone()))
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                AppError::InvalidInput(format!(
                    "profile '{name}' is not configured; run `teams init` or set TEAMS_CLIENT_ID"
                ))
            })?
    };
    let tenant = if backend == BackendKind::Desktop {
        String::new()
    } else {
        std::env::var("TEAMS_TENANT")
            .ok()
            .or_else(|| stored.map(|profile| profile.tenant.clone()))
            .unwrap_or_else(default_tenant)
    };
    let read_only = match std::env::var("TEAMS_READ_ONLY") {
        Ok(value) => parse_bool("TEAMS_READ_ONLY", &value)?,
        Err(std::env::VarError::NotPresent) => stored.is_some_and(|profile| profile.read_only),
        Err(error) => {
            return Err(AppError::InvalidInput(format!(
                "cannot read TEAMS_READ_ONLY: {error}"
            )));
        }
    };
    let desktop_port = stored
        .map(|p| p.desktop_port)
        .unwrap_or(DEFAULT_DESKTOP_PORT);
    if backend == BackendKind::Desktop && desktop_port < 1024 {
        return Err(AppError::InvalidInput(
            "desktop_port must be between 1024 and 65535".into(),
        ));
    }
    let profile = Profile {
        backend,
        client_id,
        tenant,
        read_only: read_only || backend == BackendKind::Desktop,
        desktop_port,
    };
    Ok((name, profile))
}

/// Resolve the profile a command would use without requiring that profile to
/// be configured. Interactive recovery screens need the name even on first
/// use, before there is a profile for [`load`] to return.
pub fn selected_profile_name(requested: Option<&str>) -> Result<String, AppError> {
    let config = read_file()?;
    Ok(selected_profile_name_from(
        requested,
        config.active_profile.as_deref(),
    ))
}

/// Resolve backend selection without consulting OAuth configuration or credentials.
pub fn selected_backend(requested: Option<&str>) -> Result<BackendKind, AppError> {
    let config = read_file()?;
    let name = selected_profile_name_from(requested, config.active_profile.as_deref());
    Ok(config
        .profiles
        .get(&name)
        .map(|p| p.backend)
        .unwrap_or_default())
}

fn selected_profile_name_from(requested: Option<&str>, active: Option<&str>) -> String {
    requested
        .map(str::to_owned)
        .or_else(|| std::env::var("TEAMS_PROFILE").ok())
        .or_else(|| active.map(str::to_owned))
        .unwrap_or_else(|| "default".into())
}

fn parse_bool(name: &str, value: &str) -> Result<bool, AppError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(AppError::InvalidInput(format!(
            "{name} must be true or false"
        ))),
    }
}

#[cfg(windows)]
fn config_base() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"))
}

#[cfg(not(windows))]
fn config_base() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".config"))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

pub fn configured_profile(requested: Option<&str>) -> Option<(String, Profile)> {
    load(requested).ok()
}

pub fn profile_summaries() -> Result<Vec<ProfileSummary>, AppError> {
    let config = read_file()?;
    Ok(config
        .profiles
        .iter()
        .map(|(name, profile)| ProfileSummary {
            name: name.clone(),
            active: config.active_profile.as_deref() == Some(name.as_str()),
            tenant: if profile.backend == BackendKind::Desktop {
                String::new()
            } else {
                profile.tenant.clone()
            },
            client_id: if profile.backend == BackendKind::Desktop {
                String::new()
            } else {
                profile.client_id.clone()
            },
            read_only: profile.read_only || profile.backend == BackendKind::Desktop,
            backend: profile.backend,
        })
        .collect())
}

pub fn use_profile(name: &str) -> Result<(), AppError> {
    let path = path();
    let mut config = read_file()?;
    if !config.profiles.contains_key(name) {
        return Err(AppError::InvalidInput(format!(
            "profile '{name}' is not configured"
        )));
    }
    config.active_profile = Some(name.into());
    write_file(&path, &config)
}

pub fn remove_profile(name: &str) -> Result<bool, AppError> {
    let path = path();
    let mut config = read_file()?;
    let removed = config.profiles.remove(name).is_some();
    if removed {
        if config.active_profile.as_deref() == Some(name) {
            config.active_profile = config.profiles.keys().next().cloned();
        }
        write_file(&path, &config)?;
    }
    Ok(removed)
}

fn read_file() -> Result<ConfigFile, AppError> {
    match std::fs::read_to_string(path()) {
        Ok(body) => toml::from_str(&body)
            .map_err(|e| AppError::InvalidInput(format!("cannot parse {}: {e}", path().display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(e) => Err(e.into()),
    }
}
