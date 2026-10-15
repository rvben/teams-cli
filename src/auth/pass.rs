//! Explicit pass backend. Never fall back to another store after an error.
use std::any::Any;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use keyring::credential::CredentialApi;
use sha2::{Digest, Sha256};

use crate::error::AppError;

fn program() -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("pass"))
        .find(|path| executable(path))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

fn default_directory() -> Option<PathBuf> {
    std::env::var_os("PASSWORD_STORE_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".password-store")))
}

/// Read-only detection. Does not invoke GPG, prompt, or create a test secret.
pub(super) fn prepare(directory: Option<&Path>) -> Result<PathBuf, AppError> {
    if program().is_none() {
        return Err(AppError::InvalidInput(
            "pass is unavailable; install pass on Linux/macOS and initialize it with `pass init GPG_KEY_ID`, or choose --credential-store native".into(),
        ));
    }
    let directory = directory
        .map(Path::to_path_buf)
        .or_else(default_directory)
        .ok_or_else(|| {
            AppError::InvalidInput(
                "Cannot locate the pass store; specify --pass-store-dir PATH".into(),
            )
        })?;
    let directory = directory.canonicalize().map_err(|_| AppError::InvalidInput(
        "The pass directory is unavailable; initialize it with `pass init GPG_KEY_ID` or specify an existing --pass-store-dir PATH".into(),
    ))?;
    if !directory.is_dir() {
        return Err(AppError::InvalidInput(
            "--pass-store-dir must be a directory".into(),
        ));
    }
    // pass supports a separate recipient list for a subtree.
    let nested = directory.join("teams-cli/.gpg-id");
    let recipients = if nested.try_exists()? {
        nested
    } else {
        directory.join(".gpg-id")
    };
    let initialized = std::fs::metadata(&recipients).is_ok_and(|metadata| metadata.len() <= 65536)
        && std::fs::read_to_string(&recipients).is_ok_and(|text| {
            text.lines()
                .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        });
    if !initialized {
        return Err(AppError::InvalidInput(
            "The pass store has no readable GPG recipients; initialize it with `pass init GPG_KEY_ID` before choosing pass".into(),
        ));
    }
    Ok(directory)
}

/// Test the configured recipients with synthetic data in a private scratch
/// store. This verifies encryption AND decryption before Microsoft sign-in,
/// without adding probe entries or Git commits to the user's password store.
pub(super) fn probe(directory: &Path) -> Result<(), AppError> {
    let directory = prepare(Some(directory))?;
    let program = program().ok_or_else(|| AppError::Unexpected("pass is unavailable".into()))?;
    probe_with(program, &directory).map_err(|error| {
        AppError::Unexpected(format!(
            "pass credential storage check failed: {error}. Sign-in was not started",
        ))
    })
}

fn probe_with(program: PathBuf, directory: &Path) -> keyring::Result<()> {
    let scratch = tempfile::tempdir().map_err(|_| failure("check"))?;
    let nested = directory.join("teams-cli/.gpg-id");
    let recipients = if nested.try_exists().map_err(|_| failure("check"))? {
        nested
    } else {
        directory.join(".gpg-id")
    };
    std::fs::copy(&recipients, scratch.path().join(".gpg-id")).map_err(|_| failure("check"))?;
    let signature = recipients.with_file_name(".gpg-id.sig");
    if signature.try_exists().map_err(|_| failure("check"))? {
        std::fs::copy(signature, scratch.path().join(".gpg-id.sig"))
            .map_err(|_| failure("check"))?;
    }
    let credential = PassCredential {
        program,
        directory: scratch.path().to_owned(),
        name: "teams-cli/storage-check".into(),
    };
    let synthetic = format!("teams-cli-storage-check-{:032x}", rand::random::<u128>());
    credential.set_password(&synthetic)?;
    if credential.get_password()? != synthetic {
        return Err(failure("verify"));
    }
    credential.delete_credential()
}

pub(super) fn available_directory() -> Option<PathBuf> {
    prepare(None).ok()
}

pub(super) fn entry(profile: &str, directory: &Path) -> Result<keyring::Entry, AppError> {
    if !directory.is_absolute() {
        return Err(AppError::InvalidInput("The saved pass_store_dir must be an absolute path; restore the original directory in this profile".into()));
    }
    let directory = prepare(Some(directory))?;
    let program = program().ok_or_else(|| AppError::Unexpected("pass is unavailable".into()))?;
    Ok(keyring::Entry::new_with_credential(Box::new(
        PassCredential {
            program,
            directory,
            name: format!("teams-cli/{:x}", Sha256::digest(profile.as_bytes())),
        },
    )))
}

#[derive(Debug)]
struct PassCredential {
    program: PathBuf,
    directory: PathBuf,
    name: String,
}

fn failure(action: &str) -> keyring::Error {
    // GPG/pass errors can contain the input or user/key identities. Do not echo them.
    keyring::Error::PlatformFailure(Box::new(std::io::Error::other(format!(
        "pass could not {action} the credential; check access to the saved pass directory and unlock your GPG key with pinentry/gpg-agent, then retry. No other credential store was used"
    ))))
}

struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        // Explicitly unlock before close: unrelated concurrent forks may inherit
        // this descriptor, and closing only our copy would prolong the lock.
        let _ = self.0.unlock();
    }
}

impl PassCredential {
    fn path(&self) -> PathBuf {
        self.directory.join(format!("{}.gpg", self.name))
    }

    fn lock(&self) -> keyring::Result<StoreLock> {
        let parent = self.directory.join("teams-cli");
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&parent).map_err(|_| failure("access"))?;
        let id = self.name.rsplit('/').next().expect("credential name");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options
            .open(parent.join(format!(".{id}.lock")))
            .map_err(|_| failure("access"))?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => keyring::Error::PlatformFailure(Box::new(std::io::Error::other(
                "This pass credential is busy in another teams process; retry shortly. No other credential store was used",
            ))),
            TryLockError::Error(_) => failure("lock"),
        })?;
        Ok(StoreLock(lock))
    }

    fn exists(&self) -> keyring::Result<bool> {
        self.path().try_exists().map_err(|_| failure("read"))
    }

    fn request(&self, action: &str, arguments: &[&str], input: &[u8]) -> keyring::Result<Vec<u8>> {
        let mut command = Command::new(&self.program);
        command
            .args(arguments)
            .arg(&self.name)
            .env("PASSWORD_STORE_DIR", &self.directory)
            .env("PASSWORD_STORE_ENABLE_EXTENSIONS", "false");
        #[cfg(unix)]
        if std::env::var_os("GPG_TTY").is_none() {
            let mut buffer = vec![0u8; 4096];
            for descriptor in [libc::STDIN_FILENO, libc::STDERR_FILENO] {
                // ttyname_r writes a NUL-terminated name into our valid buffer.
                if unsafe { libc::ttyname_r(descriptor, buffer.as_mut_ptr().cast(), buffer.len()) }
                    == 0
                {
                    if let Ok(name) = std::ffi::CStr::from_bytes_until_nul(&buffer) {
                        command.env(
                            "GPG_TTY",
                            std::ffi::OsStr::new(&name.to_string_lossy().into_owned()),
                        );
                    }
                    break;
                }
            }
        }
        let output = super::process::run(command, input.to_vec(), Duration::from_secs(60))
            .map_err(|_| failure(action))?;
        if !output.status.success() {
            return Err(failure(action));
        }
        Ok(output.stdout)
    }
}

impl CredentialApi for PassCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        let _lock = self.lock()?;
        // pass encrypts directly to the destination. Keep a private, encrypted
        // backup so a failed GPG invocation cannot destroy the old sign-in.
        let backup = match File::open(self.path()) {
            Ok(mut file) => {
                let mut backup = tempfile::Builder::new()
                    .prefix(".teams-backup-")
                    .suffix(".gpg")
                    .tempfile_in(self.directory.join("teams-cli"))
                    .map_err(|_| failure("preserve"))?;
                std::io::copy(&mut file, backup.as_file_mut()).map_err(|_| failure("preserve"))?;
                Some(backup)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(failure("preserve")),
        };
        let saved = self
            .request("save", &["insert", "--multiline", "--force", "--"], secret)
            .map(|_| ());
        if let Err(error) = saved {
            let restored = match backup {
                Some(backup) => match backup.persist(self.path()) {
                    Ok(_) => Ok(()),
                    Err(error) => {
                        let _ = error.file.keep();
                        Err(())
                    }
                },
                None => match std::fs::remove_file(self.path()) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(_) => Err(()),
                },
            };
            if restored.is_err() {
                return Err(keyring::Error::PlatformFailure(Box::new(
                    std::io::Error::other(
                        "pass failed to save and could not restore the previous encrypted credential; check the pass directory and recover the credential from your password-store backup",
                    ),
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        if !self
            .directory
            .join("teams-cli")
            .try_exists()
            .map_err(|_| failure("read"))?
        {
            return Err(keyring::Error::NoEntry);
        }
        let _lock = self.lock()?;
        if !self.exists()? {
            return Err(keyring::Error::NoEntry);
        }
        self.request("read", &["show", "--"], &[])
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        if !self
            .directory
            .join("teams-cli")
            .try_exists()
            .map_err(|_| failure("remove"))?
        {
            return Err(keyring::Error::NoEntry);
        }
        let _lock = self.lock()?;
        if !self.exists()? {
            return Err(keyring::Error::NoEntry);
        }
        self.request("remove", &["remove", "--force", "--"], &[])
            .map(|_| ())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const MOCK_PASS: &str = r##"#!/bin/sh
set -eu
[ "$PASSWORD_STORE_ENABLE_EXTENSIONS" = false ]
case "$1" in
  insert) [ "$2" = --multiline ]; [ "$3" = --force ]; [ "$4" = -- ]; name="$5"; mkdir -p "$PASSWORD_STORE_DIR/teams-cli"; cat > "$PASSWORD_STORE_DIR/$name.gpg" ;;
  show) [ "$2" = -- ]; cat "$PASSWORD_STORE_DIR/$3.gpg" ;;
  remove) [ "$2" = --force ]; [ "$3" = -- ]; rm "$PASSWORD_STORE_DIR/$4.gpg" ;;
  *) exit 1 ;;
esac
"##;

    fn fixture(script: &str) -> (tempfile::TempDir, PassCredential) {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("pass");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let credential = PassCredential {
            program,
            directory: temp.path().to_owned(),
            name: "teams-cli/synthetic-profile".into(),
        };
        (temp, credential)
    }

    #[test]
    fn round_trip_uses_pinned_directory_stdin_and_safe_arguments() {
        let (_temp, credential) = fixture(MOCK_PASS);
        assert!(matches!(
            credential.get_secret(),
            Err(keyring::Error::NoEntry)
        ));
        let secret = "synthetic-token-€".repeat(10000);
        credential.set_password(&secret).unwrap();
        assert_eq!(credential.get_password().unwrap(), secret);
        credential.set_password("replacement").unwrap();
        assert_eq!(credential.get_password().unwrap(), "replacement");
        credential.delete_credential().unwrap();
        assert!(matches!(
            credential.delete_credential(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn decryption_failure_is_not_a_missing_entry_and_does_not_expose_helper_output() {
        let (temp, credential) = fixture(
            r##"#!/bin/sh
if [ "$1" = insert ]; then printf broken > "$PASSWORD_STORE_DIR/$5.gpg"; fi
printf 'synthetic-secret'
printf 'synthetic-secret' >&2
exit 1
"##,
        );
        std::fs::create_dir(temp.path().join("teams-cli")).unwrap();
        std::fs::write(
            temp.path().join("teams-cli/synthetic-profile.gpg"),
            "encrypted",
        )
        .unwrap();
        for result in [
            credential.get_secret().map(|_| ()),
            credential.set_secret(b"synthetic-secret"),
        ] {
            let error = result.unwrap_err();
            assert!(!matches!(error, keyring::Error::NoEntry));
            assert!(!error.to_string().contains("synthetic-secret"));
            assert!(error.to_string().contains("No other credential store"));
        }
        assert_eq!(
            std::fs::read_to_string(temp.path().join("teams-cli/synthetic-profile.gpg")).unwrap(),
            "encrypted"
        );
    }
    #[test]
    fn concurrent_updates_are_refused_before_touching_the_secret() {
        let (_temp, credential) = fixture("#!/bin/sh\nexit 99\n");
        let lock = credential.lock().unwrap();
        assert!(
            credential
                .set_password("synthetic")
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        assert!(!credential.path().exists());
        let inherited = lock.0.try_clone().unwrap();
        drop(lock);
        // A copied/inherited descriptor must not keep a finished operation busy.
        credential.lock().unwrap();
        drop(inherited);
    }
    #[test]
    fn storage_probe_verifies_round_trip_in_scratch_without_live_entries() {
        let (temp, credential) = fixture(MOCK_PASS);
        std::fs::create_dir(temp.path().join("teams-cli")).unwrap();
        std::fs::write(temp.path().join("teams-cli/.gpg-id"), "synthetic-key\n").unwrap();
        std::fs::write(
            temp.path().join("teams-cli/.gpg-id.sig"),
            "synthetic-signature",
        )
        .unwrap();
        probe_with(credential.program.clone(), temp.path()).unwrap();
        let entries: Vec<_> = std::fs::read_dir(temp.path().join("teams-cli"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 2);
        // A broken decryption path must be rejected even when encryption succeeds.
        let script = MOCK_PASS.replace("  show)", "  show) exit 1 ;;\n  unused)");
        std::fs::write(&credential.program, script).unwrap();
        assert!(probe_with(credential.program, temp.path()).is_err());
        assert_eq!(
            std::fs::read_dir(temp.path().join("teams-cli"))
                .unwrap()
                .count(),
            2
        );
    }
    #[test]
    #[ignore = "requires real pass/GPG and TEAMS_TEST_PASS_STORE pointing to initialized test recipients"]
    fn real_pass_gpg_round_trip_and_failed_replacement_preserve_encrypted_credentials() {
        let recipients =
            std::env::var_os("TEAMS_TEST_PASS_STORE").expect("isolated test recipient directory");
        let scratch = tempfile::tempdir().unwrap();
        let recipients = std::fs::read(std::path::Path::new(&recipients).join(".gpg-id")).unwrap();
        std::fs::write(scratch.path().join(".gpg-id"), &recipients).unwrap();
        probe(scratch.path()).unwrap();
        let credential = entry("synthetic-real-pass-test", scratch.path()).unwrap();
        let secret = "synthetic-access-and-refresh-token-€".repeat(10000);
        credential.set_password(&secret).unwrap();
        assert_eq!(credential.get_password().unwrap(), secret);
        let path = scratch.path().join(format!(
            "teams-cli/{:x}.gpg",
            Sha256::digest(b"synthetic-real-pass-test")
        ));
        let encrypted = std::fs::read(&path).unwrap();
        assert!(
            !encrypted
                .windows(b"synthetic-access".len())
                .any(|bytes| bytes == b"synthetic-access")
        );
        credential.set_password("synthetic-replacement").unwrap();
        assert_eq!(credential.get_password().unwrap(), "synthetic-replacement");
        // Unknown recipients make real GPG fail during a refresh/update.
        std::fs::write(
            scratch.path().join(".gpg-id"),
            "nonexistent-teams-cli-synthetic-recipient\n",
        )
        .unwrap();
        assert!(credential.set_password("synthetic-failed-update").is_err());
        assert_eq!(credential.get_password().unwrap(), "synthetic-replacement");
        std::fs::write(scratch.path().join(".gpg-id"), recipients).unwrap();
        credential.delete_credential().unwrap();
        assert!(matches!(
            credential.get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}
