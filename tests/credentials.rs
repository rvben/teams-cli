#![cfg(unix)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture {
    temp: tempfile::TempDir,
    store: PathBuf,
    path: std::ffi::OsString,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("pass store with spaces");
        let bin = temp.path().join("bin");
        std::fs::create_dir(&store).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(store.join(".gpg-id"), "synthetic-test-key\n").unwrap();
        // A protocol fixture, not an encryption implementation. Tests seed
        // synthetic data only; production encryption is delegated to pass/GPG.
        let program = bin.join("pass");
        std::fs::write(
            &program,
            r##"#!/bin/sh
set -eu
if [ -f "$PASSWORD_STORE_DIR/fail" ]; then
  printf 'synthetic-secret-do-not-print' >&2
  printf 'synthetic-secret-do-not-print'
  exit 1
fi
case "$1" in
  show) [ "$2" = -- ]; cat "$PASSWORD_STORE_DIR/$3.gpg" ;;
  remove) [ "$2" = --force ]; [ "$3" = -- ]; rm "$PASSWORD_STORE_DIR/$4.gpg" ;;
  *) exit 99 ;;
esac
"##,
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        Self { temp, store, path }
    }

    fn cli(&self) -> Command {
        let mut command = Command::cargo_bin("teams").unwrap();
        command
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env("PATH", &self.path)
            .env("PASSWORD_STORE_DIR", &self.store)
            .env_remove("TEAMS_ACCESS_TOKEN")
            .env_remove("TEAMS_PROFILE")
            .env_remove("TEAMS_CLIENT_ID")
            .env_remove("TEAMS_TENANT")
            .env_remove("TEAMS_READ_ONLY")
            .arg("--json");
        command
    }

    fn init(&self) {
        self.cli()
            .args(["init", "--credential-store", "pass", "--no-login"])
            .assert()
            .success();
    }

    fn credential(&self) -> PathBuf {
        self.store
            .join(format!("teams-cli/{:x}.gpg", Sha256::digest(b"default")))
    }

    fn seed(&self) {
        std::fs::create_dir_all(self.store.join("teams-cli")).unwrap();
        std::fs::write(self.credential(), r#"{"access_token":"synthetic-token","refresh_token":"synthetic-refresh","expires_at":18446744073709551615,"scope":"Chat.Read"}"#).unwrap();
    }
}

#[test]
fn backend_and_directory_remain_pinned_across_updates_status_and_logout() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.seed();
    // A temporary Desktop connection must not forget the Graph store choice.
    fixture
        .cli()
        .args(["init", "--backend", "desktop", "--no-login"])
        .assert()
        .success();
    fixture
        .cli()
        .args(["init", "--backend", "graph", "--no-login"])
        .assert()
        .success();
    let other = fixture.temp.path().join("another-store");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join(".gpg-id"), "another-synthetic-key\n").unwrap();
    fixture
        .cli()
        .env("PASSWORD_STORE_DIR", &other)
        .args(["init", "--no-login"])
        .assert()
        .success();
    let output = fixture
        .cli()
        .env("PASSWORD_STORE_DIR", &other)
        .args(["config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let config: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(config["credential_store"], "pass");
    assert_eq!(
        config["pass_store_dir"],
        fixture.store.canonicalize().unwrap().to_str().unwrap()
    );
    let output = fixture
        .cli()
        .env("PASSWORD_STORE_DIR", &other)
        .args(["auth", "status", "--offline"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["credential_store"], "pass");
    assert_eq!(status["granted_scopes"], serde_json::json!(["Chat.Read"]));
    assert!(!String::from_utf8_lossy(&output).contains("synthetic-token"));
    fixture
        .cli()
        .env("PASSWORD_STORE_DIR", &other)
        .args(["auth", "logout"])
        .assert()
        .success();
    assert!(!fixture.credential().exists());
    fixture.cli().args(["auth", "logout"]).assert().success();
}

#[test]
fn existing_profiles_cannot_silently_move_stores_or_directories() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.seed();
    let original = std::fs::read(fixture.temp.path().join("config/teams/config.toml")).unwrap();
    fixture
        .cli()
        .args(["init", "--credential-store", "native", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("another credential store"));
    let other = fixture.temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join(".gpg-id"), "synthetic-key\n").unwrap();
    fixture
        .cli()
        .args(["init", "--no-login", "--pass-store-dir"])
        .arg(other)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("another pass directory"));
    assert_eq!(
        std::fs::read(fixture.temp.path().join("config/teams/config.toml")).unwrap(),
        original
    );
    assert!(fixture.credential().exists());
}

#[test]
fn gpg_failure_is_reported_without_relogin_fallback_or_secret_disclosure() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.seed();
    std::fs::write(fixture.store.join("fail"), "").unwrap();
    let output = fixture
        .cli()
        .args(["auth", "status", "--offline"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&output).unwrap();
    assert!(status["signed_in"].is_null());
    assert!(
        status["credential_error"]
            .as_str()
            .unwrap()
            .contains("unlock your GPG key")
    );
    assert!(!String::from_utf8_lossy(&output).contains("synthetic-secret-do-not-print"));
    fixture
        .cli()
        .args(["auth", "login", "--device-code"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Sign-in was not started"));
    fixture.cli().args(["auth", "logout"]).assert().failure();
    assert!(fixture.credential().exists());
}

#[test]
fn unavailable_or_uninitialized_pass_is_rejected_before_configuration_is_saved() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.store.join(".gpg-id")).unwrap();
    fixture
        .cli()
        .args(["init", "--credential-store", "pass", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("pass init"));
    assert!(
        !fixture
            .temp
            .path()
            .join("config/teams/config.toml")
            .exists()
    );
    fixture
        .cli()
        .env("PATH", fixture.temp.path().join("empty-bin"))
        .args(["init", "--credential-store", "pass", "--no-login"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("pass is unavailable"));
    fixture
        .cli()
        .args([
            "init",
            "--backend",
            "desktop",
            "--credential-store",
            "pass",
            "--no-login",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("credential-store"));
}
