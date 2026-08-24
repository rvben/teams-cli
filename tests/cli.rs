use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

#[test]
fn schema_is_offline_and_clispec_v03_shaped() {
    let output = Command::cargo_bin("teams")
        .unwrap()
        .arg("schema")
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["clispec"], "0.3");
    assert_eq!(schema["name"], "teams");

    let commands = schema["commands"].as_array().unwrap();
    assert!(!commands.is_empty());
    for command in commands {
        assert!(command["name"].as_str().is_some());
        assert!(command["description"].as_str().is_some());
        assert!(matches!(
            command["effects"].as_str(),
            Some("read_only" | "idempotent" | "non_idempotent")
        ));
        if command
            .get("output_kind")
            .and_then(Value::as_str)
            .unwrap_or("data")
            == "data"
        {
            assert!(command["cardinality"].as_str().is_some());
            assert!(
                command.get("output_fields").is_some() || command.get("stdout_schema").is_some()
            );
        }
        if command["cardinality"] == "unbounded" {
            let names: Vec<&str> = command["args"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|arg| arg["name"].as_str())
                .collect();
            match command["pagination"]["style"].as_str().unwrap() {
                "cursor" => {
                    assert!(names.contains(&command["pagination"]["cursor_arg"].as_str().unwrap()))
                }
                "offset" => {
                    assert!(names.contains(&command["pagination"]["offset_arg"].as_str().unwrap()))
                }
                style => panic!("unexpected pagination style: {style}"),
            }
            assert!(names.contains(&command["pagination"]["limit_arg"].as_str().unwrap()));
            assert!(names.contains(&command["fields_arg"].as_str().unwrap()));
        }
    }
    assert!(
        schema["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| error["kind"] == "tty_required")
    );
}

#[test]
fn schema_can_select_one_command() {
    Command::cargo_bin("teams")
        .unwrap()
        .args(["schema", "--command", "messages send"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"name\": \"messages send\"")
                .and(predicate::str::contains("channels list").not()),
        );
}

#[test]
fn demo_snapshot_is_deterministic_and_ansi_free() {
    Command::cargo_bin("teams")
        .unwrap()
        .args([
            "tui",
            "--demo",
            "--snapshot",
            "--width",
            "90",
            "--height",
            "26",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Launch room"))
        .stdout(predicate::str::contains("release candidate is green"))
        .stdout(predicate::str::contains("\u{1b}[").not());
}

#[test]
fn no_args_never_prompts_when_piped() {
    Command::cargo_bin("teams")
        .unwrap()
        .assert()
        .code(8)
        .stderr(predicate::str::contains("\"kind\":\"tty_required\""));
}

#[test]
fn noninteractive_init_requires_client_id() {
    Command::cargo_bin("teams")
        .unwrap()
        .args(["init", "--no-login"])
        .assert()
        .code(8)
        .stderr(predicate::str::contains("--client-id is required"));
}

#[test]
fn headless_browser_login_refuses_before_writing_config() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args([
            "init",
            "--client-id",
            "00000000-0000-0000-0000-000000000000",
        ])
        .assert()
        .code(8)
        .stderr(predicate::str::contains(
            "browser sign-in requires a terminal",
        ));
    assert!(!temp.path().join("teams/config.toml").exists());
}
