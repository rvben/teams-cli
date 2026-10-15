//! Installs a matching Chrome for Testing pair into a private directory. No system
//! browser is modified, and an installed pair is reused without a network request.
use super::browser::{browser_version, prepare_directory, validate_directory};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const METADATA: &str = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
const MAX_ARCHIVE: u64 = 400 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;

fn failed(message: impl Into<String>) -> AppError {
    AppError::Desktop(message.into())
}

fn network(error: reqwest::Error) -> AppError {
    failed(format!(
        "Browser download failed: {error}. Retry the setup command; no sign-in profile was changed."
    ))
}

#[derive(Debug, Serialize, Deserialize)]
struct Installed {
    version: String,
    platform: String,
}

pub fn platform() -> Result<&'static str, AppError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("mac-arm64"),
        ("macos", "x86_64") => Ok("mac-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("linux", "x86_64") => Ok("linux64"),
        _ => Err(failed(
            "the lightweight browser requires macOS or Linux on ARM64 or x86-64",
        )),
    }
}

fn root() -> Result<PathBuf, AppError> {
    let base = if let Some(path) = std::env::var_os("TEAMS_DESKTOP_BROWSER_DATA_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(failed(
                "TEAMS_DESKTOP_BROWSER_DATA_DIR must be an absolute path",
            ));
        }
        path
    } else {
        dirs::data_local_dir()
            .ok_or_else(|| failed("could not locate the browser data directory"))?
            .join("teams-cli/browser")
    };
    Ok(base.join("runtimes"))
}

fn valid_version(version: &str) -> bool {
    version.split('.').count() == 4
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn paths(base: &Path, version: &str, platform: &str) -> (PathBuf, PathBuf) {
    let dir = base.join(format!("{version}-{platform}"));
    let chrome = dir.join(format!("chrome-{platform}"));
    let chrome = if platform.starts_with("mac-") {
        chrome.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
    } else {
        chrome.join("chrome")
    };
    (
        chrome,
        dir.join(format!(
            "chrome-headless-shell-{platform}/chrome-headless-shell"
        )),
    )
}

/// Explicit paths must always be supplied as a pair; mixing an override with a
/// managed installation could silently change encryption or browser versions.
pub fn overrides() -> Result<Option<(PathBuf, PathBuf)>, AppError> {
    match (
        std::env::var_os("TEAMS_DESKTOP_BROWSER"),
        std::env::var_os("TEAMS_DESKTOP_HEADLESS_BROWSER"),
    ) {
        (None, None) => Ok(None),
        (Some(chrome), Some(shell)) => {
            let chrome = PathBuf::from(chrome);
            let shell = PathBuf::from(shell);
            if !chrome.is_absolute()
                || !shell.is_absolute()
                || shell
                    .file_name()
                    .is_none_or(|n| n != "chrome-headless-shell")
            {
                return Err(failed(
                    "browser-core overrides must be absolute executable paths; use chrome-headless-shell for TEAMS_DESKTOP_HEADLESS_BROWSER",
                ));
            }
            Ok(Some((
                std::fs::canonicalize(chrome)?,
                std::fs::canonicalize(shell)?,
            )))
        }
        _ => Err(failed(
            "set both TEAMS_DESKTOP_BROWSER and TEAMS_DESKTOP_HEADLESS_BROWSER to matching executables, or unset both for automatic installation",
        )),
    }
}

pub fn installed() -> Result<(PathBuf, PathBuf), AppError> {
    if let Some(pair) = overrides()? {
        return Ok(pair);
    }
    let base = root()?;
    validate_directory(&base)?;
    let record: Installed = serde_json::from_slice(&std::fs::read(base.join("installed.json"))?)
        .map_err(|_| failed("invalid managed browser installation; retry `teams desktop login`"))?;
    if !valid_version(&record.version) || record.platform != platform()? {
        return Err(failed("invalid managed browser version or platform"));
    }
    validate_directory(&base.join(format!("{}-{}", record.version, record.platform)))?;
    let pair = paths(&base, &record.version, &record.platform);
    for path in [&pair.0, &pair.1] {
        let actual = std::fs::canonicalize(path)?;
        if !actual.starts_with(std::fs::canonicalize(&base)?) || !actual.is_file() {
            return Err(failed("invalid managed browser executable"));
        }
    }
    Ok((
        std::fs::canonicalize(pair.0)?,
        std::fs::canonicalize(pair.1)?,
    ))
}

fn validate_pair(pair: &(PathBuf, PathBuf), expected: Option<&str>) -> Result<String, AppError> {
    let version = browser_version(&pair.0)?;
    if browser_version(&pair.1)? != version || expected.is_some_and(|e| e != version) {
        return Err(failed(
            "visible browser and headless shell must have exactly matching versions; fix or unset both browser overrides, then retry setup",
        ));
    }
    Ok(version)
}

fn selection(metadata: &Value, platform: &str) -> Result<(String, [String; 2]), AppError> {
    let stable = &metadata["channels"]["Stable"];
    let version = stable["version"]
        .as_str()
        .filter(|v| valid_version(v))
        .ok_or_else(|| failed("Chrome for Testing did not provide a stable browser version"))?;
    let mut urls = Vec::new();
    for asset in ["chrome", "chrome-headless-shell"] {
        let url = stable["downloads"][asset]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["platform"] == platform))
            .and_then(|i| i["url"].as_str())
            .ok_or_else(|| {
                failed(format!(
                    "Chrome for Testing has no matching {asset} for {platform}"
                ))
            })?;
        let expected = format!(
            "https://storage.googleapis.com/chrome-for-testing-public/{version}/{platform}/{asset}-{platform}.zip"
        );
        if url != expected {
            return Err(failed("refused an unexpected browser download URL"));
        }
        urls.push(url.to_owned());
    }
    Ok((version.into(), [urls.remove(0), urls.remove(0)]))
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    quiet: bool,
) -> Result<(), AppError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(network)?
        .error_for_status()
        .map_err(network)?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ARCHIVE)
    {
        return Err(failed("browser archive is too large"));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(dest)?;
    let mut size = 0;
    let mut announced = 0;
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        size += chunk.len() as u64;
        if size > MAX_ARCHIVE {
            return Err(failed("browser archive is too large"));
        }
        file.write_all(&chunk)?;
        if size / (25 * 1024 * 1024) > announced {
            announced = size / (25 * 1024 * 1024);
            if !quiet {
                eprintln!("  Downloaded {} MiB…", size / (1024 * 1024));
            }
        }
    }
    file.sync_all()?;
    Ok(())
}

fn safe_link(path: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = path.parent().map_or(0, |p| p.components().count());
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return false,
        }
    }
    true
}

fn extract(archive: &Path, dest: &Path, expected_root: &str) -> Result<(), AppError> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)
        .map_err(|e| failed(format!("invalid browser ZIP: {e}")))?;
    if zip.len() > 30_000 {
        return Err(failed("browser archive has too many entries"));
    }
    let mut total: u64 = 0;
    let mut links = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| failed(format!("invalid browser ZIP entry: {e}")))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| failed("unsafe browser archive path"))?;
        if relative.components().next() != Some(Component::Normal(expected_root.as_ref())) {
            return Err(failed("unexpected browser archive root"));
        }
        total = total
            .checked_add(entry.size())
            .filter(|s| *s <= MAX_EXPANDED)
            .ok_or_else(|| failed("expanded browser archive is too large"))?;
        let path = dest.join(&relative);
        if entry.is_dir() {
            prepare_directory(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            prepare_directory(parent)?;
        }
        let mode = entry.unix_mode().unwrap_or(0o600);
        if mode & 0o170000 == 0o120000 {
            if entry.size() > 4096 {
                return Err(failed("browser archive symlink is too large"));
            }
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            if !safe_link(&relative, Path::new(&target)) {
                return Err(failed("unsafe browser archive symlink"));
            }
            links.push((path, target));
        } else {
            if mode & 0o170000 != 0 && mode & 0o170000 != 0o100000 {
                return Err(failed("unsupported browser archive file"));
            }
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600 | (mode & 0o100))
                .open(&path)?;
            let size = entry.size();
            let copied = std::io::copy(&mut entry.take(size + 1), &mut file)?;
            if copied != size {
                return Err(failed("invalid browser archive size"));
            }
            std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(0o600 | (mode & 0o100)),
            )?;
        }
    }
    // Symlinks are created last, so extraction cannot write through an archive link.
    for (path, target) in &links {
        std::os::unix::fs::symlink(target, path)?;
    }
    let canonical = std::fs::canonicalize(dest)?;
    for (path, _) in links {
        if !std::fs::canonicalize(path)?.starts_with(&canonical) {
            return Err(failed("browser archive link escaped its installation"));
        }
    }
    Ok(())
}

pub async fn ensure() -> Result<(PathBuf, PathBuf), AppError> {
    ensure_with_progress(false).await
}

/// Check executables or download availability without changing the installation or launching Teams.
/// `true` means setup will need to download its matching browser pair after consent.
pub async fn preflight() -> Result<bool, AppError> {
    if let Some(pair) = overrides()? {
        validate_pair(&pair, None)?;
        return Ok(false);
    }
    let base = root()?;
    if base.try_exists()? {
        validate_directory(&base)?;
        if base.join("installed.json").try_exists()? {
            validate_pair(&installed()?, None)?;
            return Ok(false);
        }
    }
    let platform = platform()?;
    let client = reqwest::Client::builder()
        .user_agent("teams-cli-browser-installer")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(network)?;
    let mut metadata = client
        .get(METADATA)
        .send()
        .await
        .map_err(network)?
        .error_for_status()
        .map_err(network)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = metadata.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(failed("browser version metadata is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let metadata: Value =
        serde_json::from_slice(&bytes).map_err(|_| failed("invalid browser version metadata"))?;
    selection(&metadata, platform)?;
    Ok(true)
}

pub async fn ensure_with_progress(quiet: bool) -> Result<(PathBuf, PathBuf), AppError> {
    if let Some(pair) = overrides()? {
        validate_pair(&pair, None)?;
        return Ok(pair);
    }
    let base = root()?;
    prepare_directory(&base)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(base.join("install.lock"))?;
    lock.try_lock()
        .map_err(|_| failed("another browser installation is running; retry when it finishes"))?;
    // Corrupt manifests fail closed instead of silently choosing a different browser.
    if base.join("installed.json").exists() {
        let pair = installed()?;
        validate_pair(&pair, None)?;
        return Ok(pair);
    }
    // A canceled download may leave a private staging directory. The installation
    // lock ensures no other installer can still be using one.
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("install-")
            && entry.file_type()?.is_dir()
        {
            validate_directory(&entry.path())?;
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let client = reqwest::Client::builder()
        .user_agent("teams-cli-browser-installer")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(network)?;
    if !quiet {
        eprintln!("Preparing the lightweight Teams browser (one-time download from Google)…");
    }
    let mut metadata = client
        .get(METADATA)
        .send()
        .await
        .map_err(network)?
        .error_for_status()
        .map_err(network)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = metadata.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(failed("browser version metadata is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let metadata: Value =
        serde_json::from_slice(&bytes).map_err(|_| failed("invalid browser version metadata"))?;
    let platform = platform()?;
    let (version, urls) = selection(&metadata, platform)?;
    let staging = tempfile::Builder::new()
        .prefix("install-")
        .tempdir_in(&base)?;
    let contents = staging.path().join("contents");
    prepare_directory(&contents)?;
    for (asset, url) in ["chrome", "chrome-headless-shell"].into_iter().zip(urls) {
        if !quiet {
            eprintln!("Downloading {asset} {version}…");
        }
        let archive = staging.path().join(format!("{asset}.zip"));
        download(&client, &url, &archive, quiet).await?;
        let output = contents.clone();
        let expected = format!("{asset}-{platform}");
        tokio::task::spawn_blocking(move || extract(&archive, &output, &expected))
            .await
            .map_err(|_| failed("browser extraction was interrupted"))??;
    }
    let pair = {
        let chrome = contents.join(format!("chrome-{platform}"));
        let chrome = if platform.starts_with("mac-") {
            chrome.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
        } else {
            chrome.join("chrome")
        };
        (
            chrome,
            contents.join(format!(
                "chrome-headless-shell-{platform}/chrome-headless-shell"
            )),
        )
    };
    validate_pair(&pair, Some(&version))?;
    let destination = base.join(format!("{version}-{platform}"));
    if destination.exists() {
        return Err(failed(format!(
            "An interrupted browser installation exists at {}. Remove only this version directory and retry setup; your sign-in profiles are separate and will be preserved.",
            destination.display()
        )));
    }
    std::fs::rename(contents, destination)?;
    let mut record = tempfile::NamedTempFile::new_in(&base)?;
    record.write_all(
        &serde_json::to_vec(&Installed {
            version,
            platform: platform.into(),
        })
        .map_err(|_| failed("invalid installation record"))?,
    )?;
    record.as_file().sync_all()?;
    record
        .persist(base.join("installed.json"))
        .map_err(|e| e.error)?;
    if !quiet {
        eprintln!("Browser ready. Your regular browser and profile are unchanged.");
    }
    installed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn metadata(version: &str) -> Value {
        json!({"channels":{"Stable":{"version":version,"downloads":{
            "chrome":[{"platform":"linux64","url":format!("https://storage.googleapis.com/chrome-for-testing-public/{version}/linux64/chrome-linux64.zip")}],
            "chrome-headless-shell":[{"platform":"linux64","url":format!("https://storage.googleapis.com/chrome-for-testing-public/{version}/linux64/chrome-headless-shell-linux64.zip")}]
        }}}})
    }
    #[test]
    fn download_selection_requires_a_complete_matching_pair_from_google() {
        let mut value = metadata("156.0.8078.4");
        assert!(selection(&value, "linux64").is_ok());
        assert!(selection(&value, "mac-arm64").is_err());
        value["channels"]["Stable"]["downloads"]["chrome"][0]["url"] =
            json!("https://attacker.invalid/chrome.zip");
        assert!(selection(&value, "linux64").is_err());
        assert!(selection(&metadata("../outside"), "linux64").is_err());
    }
    fn archive(dir: &Path, name: &str, body: &[u8], link: bool) -> PathBuf {
        let path = dir.join("input.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        if link {
            zip.add_symlink(name, std::str::from_utf8(body).unwrap(), options)
                .unwrap();
        } else {
            zip.start_file(name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        path
    }
    #[test]
    fn extraction_preserves_executability_but_rejects_traversal_and_foreign_roots() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        prepare_directory(&output).unwrap();
        let path = archive(dir.path(), "chrome-linux64/chrome", b"browser", false);
        extract(&path, &output, "chrome-linux64").unwrap();
        assert_eq!(
            std::fs::read(output.join("chrome-linux64/chrome")).unwrap(),
            b"browser"
        );
        assert_eq!(
            std::fs::metadata(output.join("chrome-linux64/chrome"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let path = archive(dir.path(), "../outside", b"escape", false);
        assert!(extract(&path, &output, "chrome-linux64").is_err());
        assert!(!dir.path().join("outside").exists());
        let path = archive(dir.path(), "unexpected/file", b"escape", false);
        assert!(extract(&path, &output, "chrome-linux64").is_err());
    }
    #[test]
    fn archive_links_cannot_escape_or_be_followed_during_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        prepare_directory(&output).unwrap();
        let path = archive(dir.path(), "chrome-linux64/link", b"../../outside", true);
        assert!(extract(&path, &output, "chrome-linux64").is_err());
        assert!(!output.join("chrome-linux64/link").exists());
        let path = archive(dir.path(), "chrome-linux64/link", b"/tmp", true);
        assert!(extract(&path, &output, "chrome-linux64").is_err());
        assert!(safe_link(
            Path::new("chrome-mac-arm64/Framework/Versions/Current"),
            Path::new("A")
        ));
    }
    #[tokio::test]
    async fn downloads_refuse_oversized_and_failed_responses_without_an_executable() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let dir = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let output = dir.path().join("download.zip");
        assert!(
            download(&client, &server.uri(), &output, true)
                .await
                .is_err()
        );
        assert!(!output.exists());
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 1];
            socket.read_exact(&mut request).await.unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        MAX_ARCHIVE + 1
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert!(
            download(&client, &format!("http://{address}"), &output, true)
                .await
                .is_err()
        );
        response.await.unwrap();
        assert!(!output.exists());
    }
}
