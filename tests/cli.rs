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
fn schema_validates_against_the_official_clispec_v03_schema() {
    let official: Value = serde_json::from_str(include_str!("fixtures/clispec-v0.3.json")).unwrap();
    let validator = jsonschema::validator_for(&official).unwrap();
    let schema = |args: &[&str]| -> Value {
        let output = Command::cargo_bin("teams")
            .unwrap()
            .arg("schema")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let full = schema(&[]);
    if let Err(error) = validator.validate(&full) {
        panic!("{error} at {}", error.instance_path());
    }
    let kinds: Vec<&str> = full["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    let globals: Vec<&str> = full["global_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    for command in full["commands"].as_array().unwrap() {
        let name = command["name"].as_str().unwrap();
        assert!(validator.is_valid(&schema(&["--command", name])), "{name}");
        for kind in command["errors"].as_array().unwrap() {
            assert!(kinds.contains(&kind.as_str().unwrap()), "{name}: {kind}");
        }
        if let Some(bypass) = command["confirmation_bypass_arg"].as_str() {
            assert!(globals.contains(&bypass), "{name}");
        }
        let example = command["example"]["args"].as_array().unwrap();
        let words: Vec<&str> = example.iter().map(|a| a.as_str().unwrap()).collect();
        assert_eq!(words[..name.split(' ').count()].join(" "), name);
    }
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
        .code(2)
        .stderr(predicate::str::contains("\"kind\":\"tty_required\""));
}

#[test]
fn noninteractive_init_uses_the_maintained_client_id() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["init", "--no-login"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["client_id"], "66ebad71-1604-48fc-a086-0d4caa24988b");
    assert_eq!(result["tenant"], "organizations");
    assert_eq!(result["channel_history_requested"], false);
    assert_eq!(result["read_only"], false);
    assert!(temp.path().join("teams/config.toml").is_file());
}

#[test]
fn init_can_create_a_read_only_profile_and_config_show_resolves_it() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["init", "--no-login", "--read-only"])
        .assert()
        .success();

    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"read_only\": true"));
}

#[test]
fn profile_lifecycle_and_offline_auth_status_are_local() {
    let temp = tempfile::tempdir().unwrap();
    for name in ["work", "sandbox"] {
        Command::cargo_bin("teams")
            .unwrap()
            .env("XDG_CONFIG_HOME", temp.path())
            .args(["--profile", name, "init", "--no-login"])
            .assert()
            .success();
    }

    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["profile", "use", "work"])
        .assert()
        .success();

    let list = Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["--output", "json", "profile", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let value: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["total"], 2);
    assert!(
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|profile| { profile["name"] == "work" && profile["active"] == true })
    );

    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["auth", "status", "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"verified\": false"));

    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .args(["profile", "remove", "sandbox", "--yes"])
        .assert()
        .success();
}

#[test]
fn suite_exit_codes_and_error_kinds_are_stable() {
    let schema: Value = serde_json::from_slice(
        &Command::cargo_bin("teams")
            .unwrap()
            .arg("schema")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let errors = schema["errors"].as_array().unwrap();
    let code = |kind: &str| {
        errors.iter().find(|error| error["kind"] == kind).unwrap()["exit_code"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(code("invalid_input"), 2);
    assert_eq!(code("auth"), 3);
    assert_eq!(code("not_found"), 4);
    assert_eq!(code("api_error"), 5);
    assert_eq!(code("rate_limit"), 6);
    assert_eq!(code("read_only"), 2);
    assert_eq!(code("unhealthy"), 7);
    assert_eq!(code("unconfirmed"), 8);
    assert_eq!(code("confirmation_required"), 2);
}

#[test]
fn doctor_reports_failing_checks_and_exits_unhealthy() {
    let temp = tempfile::tempdir().unwrap();
    let teams = || {
        let mut command = Command::cargo_bin("teams").unwrap();
        command
            .env("XDG_CONFIG_HOME", temp.path())
            .env_remove("TEAMS_PROFILE")
            .env_remove("TEAMS_ACCESS_TOKEN");
        command
    };
    teams().args(["init", "--no-login"]).assert().success();

    let output = teams().args(["doctor", "--offline"]).output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["healthy"], false);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("\"kind\":\"unhealthy\""));
    assert!(stderr.contains("credential_store"));

    teams()
        .args(["--output", "text", "doctor", "--offline"])
        .assert()
        .code(7)
        .stdout(predicate::str::contains(
            "channel_history    optional  optional;",
        ))
        .stdout(predicate::str::contains(
            "credential_store   fail      run `teams auth login`",
        ))
        .stderr(predicate::str::contains("Hint:").not());
}

#[test]
fn headless_browser_login_refuses_before_writing_config() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("teams")
        .unwrap()
        .env("XDG_CONFIG_HOME", temp.path())
        .arg("init")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "browser sign-in requires a terminal",
        ));
    assert!(!temp.path().join("teams/config.toml").exists());
}

#[test]
fn schema_declares_default_app_and_privileged_scope() {
    let output = Command::cargo_bin("teams")
        .unwrap()
        .arg("schema")
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        schema["extensions"]["default_client_id"],
        "66ebad71-1604-48fc-a086-0d4caa24988b"
    );
    assert!(
        schema["extensions"]["baseline_scopes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|scope| scope != "ChannelMessage.Read.All")
    );
    assert_eq!(
        schema["extensions"]["privileged_scopes"],
        serde_json::json!(["ChannelMessage.Read.All"])
    );
}
