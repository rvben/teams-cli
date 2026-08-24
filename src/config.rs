use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The maintained, multitenant public-client registration shipped with teams-cli.
pub const DEFAULT_CLIENT_ID: &str = "66ebad71-1604-48fc-a086-0d4caa24988b";
pub const DEFAULT_TENANT: &str = "organizations";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub client_id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
}

fn default_tenant() -> String {
    DEFAULT_TENANT.into()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    active_profile: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
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
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Unexpected("configuration path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let body = toml::to_string_pretty(&config).map_err(|e| AppError::Unexpected(e.to_string()))?;
    std::fs::write(&path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

pub fn load(requested: Option<&str>) -> Result<(String, Profile), AppError> {
    let config = read_file()?;
    let name = requested
        .map(str::to_owned)
        .or_else(|| std::env::var("TEAMS_PROFILE").ok())
        .or(config.active_profile)
        .unwrap_or_else(|| "default".into());
    let stored = config.profiles.get(&name);
    let client_id = std::env::var("TEAMS_CLIENT_ID")
        .ok()
        .or_else(|| stored.map(|profile| profile.client_id.clone()))
        .ok_or_else(|| {
            AppError::InvalidInput(format!(
                "profile '{name}' is not configured; run `teams init` or set TEAMS_CLIENT_ID"
            ))
        })?;
    let tenant = std::env::var("TEAMS_TENANT")
        .ok()
        .or_else(|| stored.map(|profile| profile.tenant.clone()))
        .unwrap_or_else(default_tenant);
    let profile = Profile { client_id, tenant };
    Ok((name, profile))
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

fn read_file() -> Result<ConfigFile, AppError> {
    match std::fs::read_to_string(path()) {
        Ok(body) => toml::from_str(&body)
            .map_err(|e| AppError::InvalidInput(format!("cannot parse {}: {e}", path().display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(e) => Err(e.into()),
    }
}
