//! WSL credentials are encrypted by the Windows user's DPAPI, without D-Bus.
use std::any::Any;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use base64::Engine;
use keyring::credential::CredentialApi;
use sha2::{Digest, Sha256};

const SCRIPT: &str = include_str!("wsl_credentials.ps1");

#[cfg(target_os = "linux")]
pub(super) fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("microsoft")
}

pub(super) fn entry(profile: &str) -> keyring::Entry {
    keyring::Entry::new_with_credential(Box::new(WslCredential {
        // Fixed-size ASCII names prevent path traversal and script interpolation.
        id: format!("{:x}", Sha256::digest(profile.as_bytes())),
    }))
}

#[derive(Debug)]
struct WslCredential {
    id: String,
}

fn failure() -> keyring::Error {
    // Never forward PowerShell output: an error may include secret input.
    keyring::Error::PlatformFailure(Box::new(std::io::Error::other(
        "Windows credential protection failed; WSL requires Windows PowerShell interop (powershell.exe) and access to the Windows user profile",
    )))
}

fn powershell_path() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("powershell.exe"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            PathBuf::from("/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")
        })
}

impl WslCredential {
    fn request(&self, action: &str, secret: &[u8]) -> keyring::Result<Vec<u8>> {
        let request = serde_json::to_vec(&serde_json::json!({
            "action": action,
            "id": self.id,
            "secret": base64::engine::general_purpose::STANDARD.encode(secret),
        }))
        .map_err(|_| failure())?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            SCRIPT
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut command = Command::new(powershell_path());
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ]);
        let output = super::process::run(command, request, Duration::from_secs(30))
            .map_err(|_| failure())?;
        if !output.status.success() {
            return Err(failure());
        }
        let bytes = output.stdout;
        decode_response(&bytes)
    }
}

fn decode_response(bytes: &[u8]) -> keyring::Result<Vec<u8>> {
    let response: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| failure())?;
    match response["status"].as_str() {
        Some("missing") => Err(keyring::Error::NoEntry),
        Some("ok") => base64::engine::general_purpose::STANDARD
            .decode(response["secret"].as_str().unwrap_or(""))
            .map_err(|_| failure()),
        _ => Err(failure()),
    }
}

impl CredentialApi for WslCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        self.request("set", secret).map(|_| ())
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        self.request("get", &[])
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        self.request("delete", &[]).map(|_| ())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_distinct_safe_names() {
        let first = entry("../../arbitrary/'$path");
        let first = first
            .get_credential()
            .downcast_ref::<WslCredential>()
            .unwrap();
        let second = entry("other-profile");
        let second = second
            .get_credential()
            .downcast_ref::<WslCredential>()
            .unwrap();
        assert_eq!(first.id.len(), 64);
        assert!(first.id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn responses_distinguish_missing_credentials_and_failures_without_leaking_secrets() {
        assert!(matches!(
            decode_response(br#"{"status":"missing"}"#),
            Err(keyring::Error::NoEntry)
        ));
        assert_eq!(
            decode_response(br#"{"status":"ok","secret":"dG9rZW4="}"#).unwrap(),
            b"token"
        );
        for bytes in [
            br#"{"status":"error","detail":"synthetic-secret"}"#.as_slice(),
            b"synthetic-secret",
        ] {
            let error = decode_response(bytes).unwrap_err().to_string();
            assert!(!error.contains("synthetic-secret"));
            assert!(error.contains("Windows"));
        }
    }
}
