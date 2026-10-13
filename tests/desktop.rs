use assert_cmd::Command;
use serde_json::Value;
use std::{
    process::Stdio,
    time::{Duration, Instant},
};

fn cli(temp: &tempfile::TempDir) -> Command {
    let mut command = Command::cargo_bin("teams").unwrap();
    command.env("XDG_CONFIG_HOME", temp.path());
    for name in [
        "TEAMS_PROFILE",
        "TEAMS_CLIENT_ID",
        "TEAMS_TENANT",
        "TEAMS_READ_ONLY",
        "TEAMS_ACCESS_TOKEN",
    ] {
        command.env_remove(name);
    }
    command
}

fn init(temp: &tempfile::TempDir) {
    cli(temp)
        .args([
            "--profile",
            "local",
            "init",
            "--backend",
            "desktop",
            "--desktop-port",
            "19222",
            "--no-login",
        ])
        .assert()
        .success();
}

#[test]
fn desktop_profile_lifecycle_and_offline_status_need_no_oauth_or_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let output = cli(&temp)
        .env("TEAMS_CLIENT_ID", "GRAPH-FIXTURE")
        .env("TEAMS_TENANT", "GRAPH-TENANT")
        .args([
            "init",
            "--backend",
            "desktop",
            "--desktop-port",
            "19222",
            "--no-login",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["backend"], "desktop");
    assert_eq!(value["read_only"], true);
    assert_eq!(value["client_id"], "");
    assert_eq!(value["tenant"], "");
    assert!(value["signed_in"].is_null() && value["connection"].is_null());
    let config: toml::Value =
        toml::from_str(&std::fs::read_to_string(temp.path().join("teams/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        config["profiles"]["default"]["desktop_port"].as_integer(),
        Some(19222)
    );
    let output = cli(&temp)
        .env("TEAMS_ACCESS_TOKEN", "GRAPH-FIXTURE-TOKEN")
        .env("TEAMS_READ_ONLY", "false")
        .args(["auth", "status", "--offline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["signed_in"].is_null());
    assert_eq!(value["verified"], false);
    assert_eq!(value["session_verified"], false);
    assert_eq!(value["read_only"], true);
    assert!(value["endpoint_available"].is_null() && value["connection"].is_null());
    cli(&temp)
        .args(["profile", "remove", "default", "--yes"])
        .assert()
        .success();
}

#[test]
fn desktop_commands_reject_before_oauth_or_tui_prompts() {
    let temp = tempfile::tempdir().unwrap();
    init(&temp);
    for args in [
        vec!["chats", "list"],
        vec!["list"],
        vec!["whoami"],
        vec!["channels", "list", "fixture"],
        vec!["messages", "list", "--chat", "fixture"],
        vec!["auth", "login", "--device-code"],
        vec!["auth", "logout"],
        vec!["tui"],
    ] {
        let output = cli(&temp)
            .env("TEAMS_ACCESS_TOKEN", "GRAPH-FIXTURE-TOKEN")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["kind"], "unsupported");
        assert!(output.stdout.is_empty());
    }
    cli(&temp)
        .args(["tui", "--demo", "--snapshot"])
        .assert()
        .success();
}

#[test]
fn desktop_send_rejects_with_stdin_still_open() {
    let temp = tempfile::tempdir().unwrap();
    init(&temp);
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("teams"))
        .env("XDG_CONFIG_HOME", temp.path())
        .env_remove("TEAMS_PROFILE")
        .args(["messages", "send", "--chat", "fixture", "--body", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            panic!("desktop send waited for stdin");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["kind"], "unsupported");
}

#[test]
fn old_profiles_remain_graph_and_minimal_desktop_profiles_ignore_graph_settings() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("teams");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("config.toml"),
        "[profiles.default]\nclient_id='fixture'\ntenant='organizations'\n",
    )
    .unwrap();
    let output = cli(&temp).args(["config", "show"]).output().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["backend"], "graph");
    std::fs::write(
        directory.join("config.toml"),
        "[profiles.default]\nbackend='desktop'\n",
    )
    .unwrap();
    let output = cli(&temp)
        .env("TEAMS_CLIENT_ID", "GRAPH-FIXTURE")
        .env("TEAMS_TENANT", "GRAPH-TENANT")
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["backend"], "desktop");
    assert_eq!(value["client_id"], "");
    assert_eq!(value["tenant"], "");
    assert_eq!(value["desktop_port"], 9222);
}

#[test]
fn backend_options_and_schema_describe_only_implemented_capabilities() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "--no-login", "--desktop-port", "19222"],
        vec![
            "init",
            "--backend",
            "desktop",
            "--desktop-port",
            "80",
            "--no-login",
        ],
        vec![
            "init",
            "--backend",
            "desktop",
            "--device-code",
            "--no-login",
        ],
    ] {
        cli(&temp).args(args).assert().code(2);
        assert!(!temp.path().join("teams/config.toml").exists());
    }
    let output = cli(&temp).arg("schema").output().unwrap();
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["extensions"]["backends"]["graph"]["default"], true);
    assert_eq!(
        schema["extensions"]["backends"]["desktop"]["session_verified"],
        false
    );
    let commands = schema["commands"].as_array().unwrap();
    assert_eq!(
        commands
            .iter()
            .find(|c| c["name"] == "messages send")
            .unwrap()["extensions"]["backends"],
        serde_json::json!(["graph"])
    );
    assert!(
        schema["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "desktop_error")
    );
    init(&temp);
    let output = cli(&temp).args(["doctor", "--offline"]).output().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    // The local transport check fails where PowerShell is unavailable.
    let expected = if value["healthy"] == true { 0 } else { 7 };
    assert_eq!(output.status.code(), Some(expected));
    assert_eq!(value["backend"], "desktop");
    assert!(value["connection"].is_null());
    assert!(
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "desktop_endpoint" && c["status"] == "skip")
    );
}
