//! Locates the Windows build that serves as the WSL helper, downloading the matching
//! release on first use and verifying it against the release's SHA256SUMS.
use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use super::helper::PROTOCOL;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const OVERRIDE: &str = "TEAMS_DESKTOP_HELPER";
const MAX_ARCHIVE: usize = 64 * 1024 * 1024;

pub enum HelperState {
    Override(PathBuf),
    Installed(PathBuf),
    Missing(PathBuf),
}

impl HelperState {
    pub fn describe(&self) -> String {
        match self {
            Self::Override(path) => format!("{OVERRIDE}={}", path.display()),
            Self::Installed(path) => format!("installed at {}", path.display()),
            Self::Missing(_) if !release_has_helper(VERSION) => format!(
                "not installed; v{VERSION} predates desktop helpers; {}",
                development_remedy()
            ),
            Self::Missing(_) => format!(
                "not installed; teams-cli {VERSION} for Windows downloads from the GitHub release on first use"
            ),
        }
    }
}

pub fn state() -> Result<HelperState, AppError> {
    if let Some(path) = std::env::var_os(OVERRIDE).filter(|p| !p.is_empty()) {
        return Ok(HelperState::Override(PathBuf::from(path)));
    }
    let path = root()?.join(VERSION).join("teams.exe");
    Ok(if path.is_file() {
        HelperState::Installed(path)
    } else {
        HelperState::Missing(path)
    })
}

/// How to get a helper that speaks this CLI's protocol, for whichever helper is in use.
pub fn remedy() -> String {
    if !release_has_helper(VERSION) {
        return development_remedy();
    }
    match state() {
        Ok(HelperState::Override(_)) => format!(
            "point {OVERRIDE} at a teams.exe that speaks protocol {PROTOCOL}, such as teams-cli {VERSION} for Windows, or unset it to use the matching release"
        ),
        Ok(HelperState::Installed(path) | HelperState::Missing(path)) => {
            let dir = path.parent().unwrap_or(&path);
            format!(
                "delete {} to download the matching release again, or run `make windows-helper` in a source checkout",
                dir.display()
            )
        }
        Err(error) => error.to_string(),
    }
}

/// The helper's path, downloading and installing it if needed.
pub async fn ensure() -> Result<PathBuf, AppError> {
    ensure_state(state()?, VERSION).await
}

// This already-published alpha is diagnostics-only. Development checkouts still
// carrying its version need a built helper, not its incompatible release binary.
fn release_has_helper(version: &str) -> bool {
    version != "0.1.9-alpha.1"
}

fn development_remedy() -> String {
    format!(
        "run `make windows-helper` in the source checkout or set {OVERRIDE} to a teams.exe that speaks protocol {PROTOCOL}"
    )
}

async fn ensure_state(state: HelperState, version: &str) -> Result<PathBuf, AppError> {
    let path = match state {
        HelperState::Override(path) if path.is_file() => return Ok(path),
        HelperState::Override(path) => {
            return Err(AppError::Desktop(format!(
                "{OVERRIDE} points to {}, which does not exist",
                path.display()
            )));
        }
        HelperState::Installed(path) => return Ok(path),
        HelperState::Missing(path) => path,
    };
    if !release_has_helper(version) {
        return Err(AppError::Desktop(format!(
            "v{version} was published before desktop helpers; automatic helper download is unavailable for this source version; {}",
            development_remedy()
        )));
    }
    let asset = format!("teams-cli-v{version}-x86_64-pc-windows-msvc.tar.gz");
    let release = format!("{}/releases/tag/v{version}", env!("CARGO_PKG_REPOSITORY"));
    let base = format!(
        "{}/releases/download/v{version}",
        env!("CARGO_PKG_REPOSITORY")
    );
    // Whatever went wrong, the same archive can be fetched and pointed at by hand.
    let by_hand = |problem: String| {
        AppError::Desktop(format!(
            "{problem}; download {asset} from {release}, unpack teams.exe and set {OVERRIDE} to its path, or run `make windows-helper` in a source checkout"
        ))
    };
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("teams-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AppError::Desktop("could not create the helper downloader".into()))?;
    let sums = download(&http, &format!("{base}/SHA256SUMS"))
        .await
        .map_err(by_hand)?;
    let archive = download(&http, &format!("{base}/{asset}"))
        .await
        .map_err(by_hand)?;
    let expected = checksum(&String::from_utf8_lossy(&sums), &asset)
        .ok_or_else(|| by_hand(format!("SHA256SUMS of v{version} does not list {asset}")))?;
    if hex(&Sha256::digest(&archive)) != expected {
        return Err(AppError::Desktop(format!(
            "{asset} does not match its published checksum; nothing was installed"
        )));
    }
    install(&path, &extract(&archive)?)?;
    Ok(path)
}

fn root() -> Result<PathBuf, AppError> {
    Ok(dirs::cache_dir()
        .ok_or_else(|| AppError::Desktop("no cache directory is available".into()))?
        .join("teams-cli")
        .join("windows-helper"))
}

/// The body at `url`, or what went wrong in words.
async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let failed = |_| {
        format!(
            "could not download the Windows desktop helper for teams-cli {VERSION}; check the network connection"
        )
    };
    let mut response = http.get(url).send().await.map_err(failed)?;
    match response.status() {
        status if status.is_success() => {}
        reqwest::StatusCode::NOT_FOUND => {
            return Err(format!(
                "teams-cli {VERSION} has no published Windows desktop helper (a build that is not a release, or a release still being published)"
            ));
        }
        status => {
            return Err(format!(
                "downloading the Windows desktop helper failed with HTTP {}",
                status.as_u16()
            ));
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if bytes.len() + chunk.len() > MAX_ARCHIVE {
            return Err("the helper download exceeds the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest listed for `name` in `sha256sum` output (`<hex>  ./<name>`).
fn checksum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file.strip_prefix("./").unwrap_or(file) == name
            && digest.len() == 64
            && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| digest.to_ascii_lowercase())
    })
}

/// Only `teams.exe` is taken from the archive.
fn extract(archive: &[u8]) -> Result<Vec<u8>, AppError> {
    let invalid = || AppError::Desktop("the helper archive is not valid".into());
    let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in entries.entries().map_err(|_| invalid())? {
        let entry = entry.map_err(|_| invalid())?;
        let is_helper = entry.header().entry_type().is_file()
            && entry.path().ok().is_some_and(|p| {
                p.components().filter(|c| c.as_os_str() != ".").count() == 1
                    && p.file_name().is_some_and(|n| n == "teams.exe")
            });
        if is_helper {
            let mut bytes = Vec::new();
            entry
                .take(MAX_ARCHIVE as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid())?;
            if bytes.len() > MAX_ARCHIVE || !bytes.starts_with(b"MZ") {
                return Err(invalid());
            }
            return Ok(bytes);
        }
    }
    Err(invalid())
}

/// Writes atomically, marks it executable for WSL interop, and removes other versions.
fn install(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let dir = path
        .parent()
        .ok_or_else(|| AppError::Desktop("invalid helper location".into()))?;
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut file, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    file.persist(path).map_err(|e| AppError::Io(e.error))?;
    if let Some(versions) = dir.parent()
        && let Ok(entries) = std::fs::read_dir(versions)
    {
        for entry in entries.flatten() {
            if entry.path() != dir {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn legacy_release_needs_a_built_helper_but_accepts_explicit_or_cached_builds() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("teams.exe");
        let error = ensure_state(HelperState::Missing(path.clone()), "0.1.9-alpha.1")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("published before desktop helpers")
                && error.contains("make windows-helper")
        );
        assert!(!path.exists());
        std::fs::write(&path, b"MZ fixture").unwrap();
        for state in [
            HelperState::Override(path.clone()),
            HelperState::Installed(path.clone()),
        ] {
            assert_eq!(ensure_state(state, "0.1.9-alpha.1").await.unwrap(), path);
        }
    }

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn checksums_match_sha256sum_output_exactly() {
        let digest = "a".repeat(64);
        let sums = format!(
            "{}  ./teams-cli-v1-x.tar.gz.bak\n{digest}  ./teams-cli-v1-x.tar.gz\n",
            "b".repeat(64)
        );
        assert_eq!(checksum(&sums, "teams-cli-v1-x.tar.gz"), Some(digest));
        assert_eq!(checksum(&sums, "other.tar.gz"), None);
        assert_eq!(checksum("xyz  ./a.tar.gz", "a.tar.gz"), None);
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    #[test]
    fn extracts_only_the_top_level_windows_executable() {
        let exe = b"MZ fixture";
        let bytes = archive(&[("./README.md", b"readme"), ("./teams.exe", exe)]);
        assert_eq!(extract(&bytes).unwrap(), exe);
        assert!(extract(&archive(&[("./nested/teams.exe", exe)])).is_err());
        assert!(extract(&archive(&[("./teams.exe", b"#!/bin/sh")])).is_err());
        assert!(extract(b"not an archive").is_err());
    }

    #[test]
    fn installs_atomically_and_prunes_other_versions() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("0.0.1");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("teams.exe"), b"MZ old").unwrap();
        let path = root.path().join(VERSION).join("teams.exe");
        install(&path, b"MZ new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"MZ new");
        assert!(!old.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }
}
