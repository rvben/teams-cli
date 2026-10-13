//! Exercises the bundled reader with synthetic ownership; HTTP and redaction stay real.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

fn bridge(mode: &str, port: u16) -> Value {
    let shell = std::env::var("TEAMS_TEST_POWERSHELL").unwrap_or_else(|_| "powershell.exe".into());
    let production = include_str!("../src/desktop.ps1");
    let identity = "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value";
    assert!(production.contains(identity));
    let code = format!(
        "$script:fixtureMode='{mode}'\n{}\n{}",
        include_str!("fixtures/desktop-mock.ps1"),
        production.replace(identity, "'fixture-sid'")
    );
    let encoded = STANDARD.encode(
        code.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let output = assert_cmd::Command::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .write_stdin(serde_json::to_vec(&json!({"operation":"probe","port":port})).unwrap())
        .timeout(std::time::Duration::from_secs(15))
        .output()
        .unwrap();
    assert!(output.status.success(), "native fixture failed");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(windows),
    ignore = "requires PowerShell; set TEAMS_TEST_POWERSHELL and run --ignored"
)]
async fn powershell_reader_refuses_unsafe_binding_wrong_package_and_wrong_user_without_http() {
    let server = MockServer::start().await;
    Mock::given(path("/json/version"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    for (mode, status) in [
        ("unsafe", "unsafe_listener_refused"),
        ("wrong_package", "non_teams_listener_refused"),
        ("wrong_user", "non_teams_listener_refused"),
    ] {
        let result = bridge(mode, server.address().port());
        assert_eq!(result["status"], status);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(windows),
    ignore = "requires PowerShell; set TEAMS_TEST_POWERSHELL and run --ignored"
)]
async fn powershell_reader_keeps_only_safe_metadata() {
    let server = MockServer::start().await;
    Mock::given(path("/json/version")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"Browser":"Edg/fixture","Protocol-Version":"1.3","webSocketDebuggerUrl":"ws://127.0.0.1/SECRET","User-Agent":"PRIVATE"}))).expect(1).mount(&server).await;
    Mock::given(path("/json/list")).respond_with(ResponseTemplate::new(200).set_body_json(json!([
        {"type":"page","url":"https://teams.live.com/?account=PRIVATE","title":"PRIVATE TITLE","id":"SECRET","webSocketDebuggerUrl":"ws://127.0.0.1/SECRET"},
        {"type":"page","url":"file:///C:/PRIVATE/SECRET.html"},
        {"type":"iframe","url":"https://private-tenant.example/PRIVATE"}
    ]))).expect(1).mount(&server).await;
    let result = bridge("metadata", server.address().port());
    assert_eq!(result["status"], "metadata_ok");
    assert_eq!(result["targets"][0]["url_origin"], "https://teams.live.com");
    assert_eq!(result["targets"][1]["url_origin"], "file:");
    let output = serde_json::to_string(&result).unwrap();
    assert!(
        !output.contains("PRIVATE")
            && !output.contains("SECRET")
            && !output.contains("private-tenant")
    );
    server.reset().await;
    Mock::given(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(path("/json/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let empty = bridge("metadata", server.address().port());
    assert_eq!(empty["status"], "metadata_ok");
    assert_eq!(empty["targets"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(windows),
    ignore = "requires PowerShell; set TEAMS_TEST_POWERSHELL and run --ignored"
)]
async fn powershell_reader_refuses_invalid_json_shapes_and_oversized_metadata() {
    let server = MockServer::start().await;
    for (version, targets) in [
        ("[]".to_owned(), "[]"),
        ("{}".to_owned(), "{}"),
        (
            format!("{{\"padding\":\"{}\"}}", "x".repeat(1024 * 1024)),
            "[]",
        ),
    ] {
        server.reset().await;
        Mock::given(path("/json/version"))
            .respond_with(ResponseTemplate::new(200).set_body_string(version))
            .mount(&server)
            .await;
        Mock::given(path("/json/list"))
            .respond_with(ResponseTemplate::new(200).set_body_string(targets))
            .mount(&server)
            .await;
        let result = bridge("metadata", server.address().port());
        assert_eq!(result["status"], "metadata_probe_failed");
    }
}
