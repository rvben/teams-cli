//! Locates the Windows build that serves as the WSL helper, downloading the matching
//! release on first use and verifying it against the release's SHA256SUMS.
use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tar_codec::{Archive as _, Member, MemberPayload as _, TarArchive};
use tokio::io::AsyncReadExt as _;

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
    // Member payloads borrow the decoder across awaits. Parse on a blocking
    // worker so the downloader remains Send on Rust 1.90, and decompression
    // does not occupy an async runtime worker.
    let helper = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(extract(&archive))
    })
    .await
    .map_err(|_| AppError::Desktop("could not read the helper archive".into()))??;
    install(&path, &helper)?;
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

/// Only one top-level `teams.exe` is accepted. Validate the entire archive
/// before installing it, including entries after the executable and gzip CRC.
async fn extract(archive: &[u8]) -> Result<Vec<u8>, AppError> {
    let invalid = || AppError::Desktop("the helper archive is not valid".into());
    if archive.len() > MAX_ARCHIVE {
        return Err(invalid());
    }
    let mut reader =
        async_compression::tokio::bufread::GzipDecoder::new(archive).take(MAX_ARCHIVE as u64 + 1);
    let mut helper = None;
    {
        let mut entries = TarArchive::new(&mut reader).members();
        let mut chunk = Vec::new();
        while let Some(entry) = entries.next().await.map_err(|_| invalid())? {
            if let Member::File {
                metadata,
                size,
                mut payload,
                ..
            } = entry
            {
                // Match the release layout without normalizing away traversal.
                let is_helper = matches!(metadata.path.as_str(), "teams.exe" | "./teams.exe");
                if is_helper {
                    if helper.is_some() || size > MAX_ARCHIVE as u64 {
                        return Err(invalid());
                    }
                    let mut bytes = Vec::new();
                    while payload
                        .next_chunk(&mut chunk, 64 * 1024)
                        .await
                        .map_err(|_| invalid())?
                    {
                        if bytes.len() + chunk.len() > MAX_ARCHIVE {
                            return Err(invalid());
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    if !bytes.starts_with(b"MZ") {
                        return Err(invalid());
                    }
                    helper = Some(bytes);
                } else {
                    payload.skip().await.map_err(|_| invalid())?;
                }
            } else if !matches!(entry, Member::Directory { .. }) {
                // Release packages contain only directories and regular files.
                return Err(invalid());
            }
        }
    }
    // The tar end marker can precede padding and the gzip trailer. Reading to
    // EOF also makes the decompressed-size budget cover ignored archive data.
    let mut padding = [0; 8192];
    loop {
        let read = reader.read(&mut padding).await.map_err(|_| invalid())?;
        if read == 0 {
            break;
        }
        if padding[..read].iter().any(|&byte| byte != 0) {
            return Err(invalid());
        }
    }
    if reader.limit() == 0 || !reader.get_ref().get_ref().is_empty() {
        return Err(invalid());
    }
    helper.ok_or_else(invalid)
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

    #[test]
    fn helper_downloader_can_be_sent_between_runtime_workers() {
        fn assert_send(_: impl Send) {}
        assert_send(ensure());
    }

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

    #[tokio::test]
    async fn extracts_only_the_top_level_windows_executable() {
        let exe = b"MZ fixture";
        let bytes = archive(&[("./README.md", b"readme"), ("./teams.exe", exe)]);
        assert_eq!(extract(&bytes).await.unwrap(), exe);
        assert!(
            extract(&archive(&[("./nested/teams.exe", exe)]))
                .await
                .is_err()
        );
        assert!(
            extract(&archive(&[("./teams.exe", b"#!/bin/sh")]))
                .await
                .is_err()
        );
        assert!(extract(b"not an archive").await.is_err());
    }

    #[tokio::test]
    async fn extracts_a_helper_across_multiple_payload_chunks() {
        let mut exe = vec![0x5a; 3 * 64 * 1024 + 17];
        exe[..2].copy_from_slice(b"MZ");
        assert_eq!(
            extract(&archive(&[("teams.exe", &exe)])).await.unwrap(),
            exe
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accepts_the_release_workflows_system_tar_packaging() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("package");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("teams.exe"), b"MZ release fixture").unwrap();
        std::fs::write(package.join("README.md"), b"readme").unwrap();
        let output = temp.path().join("helper.tar.gz");
        // Match .github/workflows/release.yml, including the '.' root entry.
        assert!(
            std::process::Command::new("tar")
                .arg("-C")
                .arg(&package)
                .arg("-czf")
                .arg(&output)
                .arg(".")
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            extract(&std::fs::read(output).unwrap()).await.unwrap(),
            b"MZ release fixture"
        );
    }

    #[tokio::test]
    async fn rejects_duplicate_helpers_and_damage_after_the_helper() {
        let duplicate = archive(&[("teams.exe", b"MZ first"), ("./teams.exe", b"MZ second")]);
        assert!(extract(&duplicate).await.is_err());

        let mut damaged = archive(&[("teams.exe", b"MZ fixture")]);
        let crc = damaged.len() - 8;
        damaged[crc] ^= 1;
        assert!(extract(&damaged).await.is_err(), "gzip CRC must be checked");
        damaged.truncate(crc);
        assert!(
            extract(&damaged).await.is_err(),
            "missing trailer must fail"
        );

        // A malformed second tar header must not be hidden by finding the
        // executable first. Recompress so the gzip checksum remains valid.
        use std::io::{Read as _, Write as _};
        let valid = archive(&[("teams.exe", b"MZ fixture"), ("README.md", b"readme")]);
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(valid.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw[1024] ^= 1;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        assert!(extract(&gz.finish().unwrap()).await.is_err());

        let mut trailing = archive(&[("teams.exe", b"MZ fixture")]);
        trailing.extend_from_slice(b"junk");
        assert!(
            extract(&trailing).await.is_err(),
            "trailing compressed data must fail"
        );

        let mut concatenated = archive(&[("teams.exe", b"MZ first")]);
        concatenated.extend_from_slice(&archive(&[("teams.exe", b"MZ second")]));
        assert!(
            extract(&concatenated).await.is_err(),
            "multiple gzip streams must fail"
        );
    }

    #[tokio::test]
    async fn rejects_links_even_after_a_valid_helper() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut file = tar::Header::new_gnu();
        file.set_size(2);
        file.set_mode(0o644);
        builder
            .append_data(&mut file, "teams.exe", &b"MZ"[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        builder
            .append_link(&mut link, "other", "teams.exe")
            .unwrap();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        assert!(extract(&bytes).await.is_err());
    }

    #[tokio::test]
    async fn bounds_decompressed_data_including_ignored_files_and_padding() {
        // Compressed input is small, but ignored payloads still consume the
        // same total budget as the helper itself.
        let bytes = archive(&[("README.md", &vec![0; MAX_ARCHIVE]), ("teams.exe", b"MZ")]);
        assert!(bytes.len() < MAX_ARCHIVE);
        assert!(extract(&bytes).await.is_err());

        use std::io::{Read as _, Write as _};
        let valid = archive(&[("teams.exe", b"MZ")]);
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(valid.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw.resize(MAX_ARCHIVE + 1, 0);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        assert!(extract(&gz.finish().unwrap()).await.is_err());
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
