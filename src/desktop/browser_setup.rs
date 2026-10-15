//! An interrupted wizard can resume its exact browser and sign-in without saving a profile.
use super::browser::{data_directory, prepare_directory, validate_directory};
use crate::{
    config::{PendingBrowserSetup, Profile},
    error::AppError,
};
use std::{
    io::{Read, Write},
    path::Path,
};

const MARKER: &str = "pending-setup.json";

fn read(path: &Path, port: u16) -> Result<PendingBrowserSetup, AppError> {
    validate_directory(path)?;
    let file = std::fs::File::open(path.join(MARKER))?;
    if file.metadata()?.len() > 4096 {
        return Err(AppError::Desktop(
            "The pending browser setup record is too large; it was preserved for recovery.".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    let record: PendingBrowserSetup = serde_json::from_slice(&bytes).map_err(|_| {
        AppError::Desktop(
            "The pending browser setup record is invalid; it was preserved for recovery.".into(),
        )
    })?;
    if record.port != port
        || port < 1024
        || !record.client.is_browser()
        || record.name.is_empty()
        || record.name.chars().any(char::is_control)
    {
        return Err(AppError::Desktop("The pending browser setup record does not match its browser; it was preserved for recovery.".into()));
    }
    record.client.validate_account(record.account)?;
    Ok(record)
}

fn list_in(base: &Path) -> Result<Vec<PendingBrowserSetup>, AppError> {
    if !base.try_exists()? {
        return Ok(Vec::new());
    }
    validate_directory(base)?;
    let mut records = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let Some(port) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u16>().ok())
        else {
            continue;
        };
        if entry.path().join(MARKER).try_exists()? {
            records.push(read(&entry.path(), port).map_err(|error| {
                error.with_hint(&format!(
                    "Check {}. To abandon only this unfinished setup, remove that record file; keep its browser directory to preserve sign-in data.",
                    entry.path().join(MARKER).display()
                ))
            })?);
        }
    }
    records.sort_by_key(|record| record.port);
    Ok(records)
}

pub fn list() -> Result<Vec<PendingBrowserSetup>, AppError> {
    list_in(&data_directory()?)
}

pub fn begin(name: &str, profile: &Profile) -> Result<(), AppError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(AppError::InvalidInput(
            "A browser setup profile name must contain 1–256 bytes without control characters."
                .into(),
        ));
    }
    let dir = data_directory()?.join(profile.desktop_port.to_string());
    prepare_directory(&dir)?;
    let record = PendingBrowserSetup {
        name: name.into(),
        port: profile.desktop_port,
        account: profile.desktop_account,
        client: profile.desktop_client,
    };
    if dir.join(MARKER).try_exists()? {
        if read(&dir, record.port)? == record {
            return Ok(());
        }
        return Err(AppError::Desktop("This browser has another unfinished setup; select its original connection or use another browser port.".into()));
    }
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.write_all(&serde_json::to_vec(&record).map_err(|e| AppError::Unexpected(e.to_string()))?)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(dir.join(MARKER))
        .map_err(|e| e.error)?;
    Ok(())
}

pub fn finish(port: u16) -> Result<(), AppError> {
    let dir = data_directory()?.join(port.to_string());
    validate_directory(&dir)?;
    match std::fs::remove_file(dir.join(MARKER)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_records_are_bounded_and_cannot_cross_a_port_or_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = tempfile::tempdir().unwrap();
        std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = base.path().join("19300");
        prepare_directory(&dir).unwrap();
        let record = PendingBrowserSetup {
            name: "work".into(),
            port: 19300,
            account: crate::config::DesktopAccount::WorkSchool,
            client: crate::config::DesktopClientKind::Browser,
        };
        std::fs::write(dir.join(MARKER), serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(list_in(base.path()).unwrap(), vec![record]);
        assert!(read(&dir, 19301).is_err());
        symlink(&dir, base.path().join("19301")).unwrap();
        assert!(list_in(base.path()).is_err());
        std::fs::remove_file(base.path().join("19301")).unwrap();
        std::fs::write(dir.join(MARKER), vec![b' '; 4097]).unwrap();
        assert!(list_in(base.path()).is_err());
        assert_eq!(std::fs::metadata(dir.join(MARKER)).unwrap().len(), 4097);
    }
}
