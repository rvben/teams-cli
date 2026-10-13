use std::process::Command;

#[test]
fn desktop_page_scripts_pass_synthetic_cache_and_send_fixtures() {
    let output = Command::new("node")
        .args([
            "--test",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/desktop_scripts.test.cjs"
            ),
        ])
        .output()
        .expect("Node.js 20 or newer is required for desktop script tests");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
