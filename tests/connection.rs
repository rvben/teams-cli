#[cfg(windows)]
#[test]
fn windows_connection_state_machine_preserves_settings_and_process_ownership() {
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = std::process::Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/connection.test.ps1"
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["real_user_environment_changes"], 0);
    assert_eq!(result["real_teams_restarts"], 0);
}
