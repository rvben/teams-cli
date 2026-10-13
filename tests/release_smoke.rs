use std::process::Command;

#[test]
fn packaged_binary_smoke_contract_accepts_this_build() {
    let python = if cfg!(windows) { "python" } else { "python3" };
    let output = Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/smoke-release.py"
        ))
        .args([env!("CARGO_BIN_EXE_teams"), env!("CARGO_PKG_VERSION")])
        .output()
        .expect("Python 3 is required for release smoke tests");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wrong_version = Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/smoke-release.py"
        ))
        .args([env!("CARGO_BIN_EXE_teams"), "0.0.0-invalid"])
        .output()
        .unwrap();
    assert!(!wrong_version.status.success());
    assert!(
        String::from_utf8_lossy(&wrong_version.stderr)
            .contains("packaged CLI version does not match the release")
    );
}
