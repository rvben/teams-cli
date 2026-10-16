#![cfg(any(target_os = "macos", target_os = "linux"))]

use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn identity(pid: u32, _executable: &Path) -> Value {
    #[cfg(target_os = "macos")]
    let started = {
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "uid=,lstart="])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .split_once(char::is_whitespace)
            .unwrap()
            .1
            .trim()
            .to_owned()
    };
    #[cfg(target_os = "linux")]
    let started = fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap()
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned();
    #[cfg(target_os = "macos")]
    let executable = {
        #[link(name = "proc")]
        unsafe extern "C" {
            fn proc_pidpath(pid: i32, buffer: *mut libc::c_void, size: u32) -> i32;
        }
        let mut path = vec![0u8; 4096];
        assert!(
            unsafe { proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) } > 0
        );
        PathBuf::from(std::ffi::OsString::from(
            String::from_utf8(path.split(|b| *b == 0).next().unwrap().to_vec()).unwrap(),
        ))
    };
    #[cfg(target_os = "linux")]
    let executable = fs::canonicalize(_executable).unwrap();
    json!({"pid": pid, "started": started, "executable": executable})
}
fn command(base: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("teams"));
    cmd.env("TEAMS_DESKTOP_BROWSER_DATA_DIR", base)
        .env("TEAMS_DESKTOP_BROWSER", "/bin/sh")
        .env_remove("TEAMS_DESKTOP_BROWSER_CLIENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}
fn supervise(base: &Path) -> (Process, String) {
    supervise_using(base, Path::new("/bin/sh"))
}
fn supervise_using(base: &Path, executable: &Path) -> (Process, String) {
    mkdir(base);
    let mut child = Process(
        command(base)
            .env("TEAMS_DESKTOP_BROWSER", executable)
            .args(["--json", "desktop", "supervise"])
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let _ = send.send(line);
    });
    let line = receive.recv_timeout(Duration::from_secs(35)).unwrap();
    if line.is_empty() {
        use std::io::Read;
        let mut error = String::new();
        child
            .0
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        panic!("supervisor did not start: {error}");
    }
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["protocol"], 1);
    (child, ready["client"].as_str().unwrap().to_owned())
}
fn wait_exit(child: &mut Process) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            if !status.success() {
                use std::io::Read;
                let mut error = String::new();
                child
                    .0
                    .stderr
                    .as_mut()
                    .unwrap()
                    .read_to_string(&mut error)
                    .unwrap();
                panic!("supervisor failed: {error}");
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "supervisor did not finish cleanup"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn mkdir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
struct Browser {
    process: Process,
    dir: PathBuf,
    state: Value,
}
impl Browser {
    fn new(base: &Path, clients: &[&str], auto_close: bool) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = base.join(port.to_string());
        mkdir(&dir);
        mkdir(&dir.join("profile"));
        // A real process with the managed browser's ownership arguments, no children,
        // network access, Chrome installation, sign-in, or user's browser profile.
        let process = Process(
            Command::new("/bin/sh")
                .args(["-c", "read line", "browser"])
                .arg(format!("--user-data-dir={}", dir.join("profile").display()))
                .arg(format!("--remote-debugging-port={port}"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        // Let exec complete before recording the executable and start time.
        std::thread::sleep(Duration::from_millis(50));
        let state = json!({"version":1,"port":port,"owner":identity(process.0.id(), Path::new("/bin/sh")),"visible":false,"core":false,"phase":"active",
            "lifetime":{"auto_close":auto_close,"clients":clients,"users":[]}});
        let result = Self {
            process,
            dir,
            state,
        };
        result.save();
        result
    }
    fn save(&self) {
        fs::write(
            self.dir.join("connection.json"),
            serde_json::to_vec(&self.state).unwrap(),
        )
        .unwrap();
    }
    fn running(&mut self) -> bool {
        self.process.0.try_wait().unwrap().is_none()
    }
}

#[test]
fn last_window_closes_only_its_browser_and_preserves_saved_sign_in() {
    let base = tempfile::tempdir().unwrap();
    let (mut first, one) = supervise(base.path());
    let (mut second, two) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&one], true);
    let status = command(base.path())
        .env("TEAMS_DESKTOP_BROWSER_CLIENT", &two)
        .args([
            "desktop",
            "status",
            "--desktop-client",
            "browser",
            "--port",
            &browser.state["port"].to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let joined: Value =
        serde_json::from_slice(&fs::read(browser.dir.join("connection.json")).unwrap()).unwrap();
    assert!(
        joined["lifetime"]["clients"]
            .as_array()
            .unwrap()
            .contains(&json!(two))
    );
    assert!(!joined["lifetime"]["users"].as_array().unwrap().is_empty());
    let mut persistent = Browser::new(base.path(), &[], false);
    let sign_in = browser.dir.join("profile/saved-sign-in");
    fs::write(&sign_in, "retained").unwrap();
    drop(first.0.stdin.take());
    wait_exit(&mut first);
    assert!(browser.running(), "another window still uses this browser");
    drop(second.0.stdin.take());
    wait_exit(&mut second);
    assert!(!browser.running());
    assert!(persistent.running());
    assert!(!browser.dir.join("connection.json").exists());
    assert_eq!(fs::read_to_string(sign_in).unwrap(), "retained");
}

#[test]
fn simultaneous_exits_do_not_leave_a_browser_behind() {
    let base = tempfile::tempdir().unwrap();
    let (mut first, one) = supervise(base.path());
    let (mut second, two) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&one, &two], true);
    drop(first.0.stdin.take());
    drop(second.0.stdin.take());
    wait_exit(&mut first);
    wait_exit(&mut second);
    assert!(!browser.running());
}

#[test]
fn cleanup_waits_for_in_flight_cli_users() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    let mut user = Process(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    std::thread::sleep(Duration::from_millis(50));
    let lease = identity(user.0.id(), Path::new("/bin/sleep"));
    #[cfg(target_os = "macos")]
    let lease = {
        let mut lease = lease;
        let output = Command::new("/bin/ps")
            .args(["-p", &user.0.id().to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .unwrap();
        lease["started"] = json!(String::from_utf8(output.stdout).unwrap().trim());
        lease
    };
    browser.state["lifetime"]["users"] = json!([lease]);
    browser.save();
    drop(supervisor.0.stdin.take());
    std::thread::sleep(Duration::from_millis(600));
    assert!(browser.running());
    assert!(supervisor.0.try_wait().unwrap().is_none());
    user.0.kill().unwrap();
    user.0.wait().unwrap();
    wait_exit(&mut supervisor);
    assert!(!browser.running());
}

#[test]
fn cleanup_rechecks_the_record_after_waiting_for_a_connection_lock() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    let lock = File::create(browser.dir.join("connection.lock")).unwrap();
    lock.lock().unwrap();
    drop(supervisor.0.stdin.take());
    std::thread::sleep(Duration::from_millis(600));
    browser.state["lifetime"]["auto_close"] = json!(false);
    browser.save();
    lock.unlock().unwrap();
    wait_exit(&mut supervisor);
    assert!(browser.running());
}

#[test]
fn changed_process_identity_is_never_killed() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    browser.state["owner"]["started"] = json!("reused PID");
    browser.save();
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(browser.running());
}

#[test]
fn legacy_sessions_stay_persistent_and_expired_tokens_cannot_launch() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, _) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[], false);
    browser.state.as_object_mut().unwrap().remove("lifetime");
    browser.save();
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(browser.running());
    let result = command(base.path())
        .env(
            "TEAMS_DESKTOP_BROWSER_CLIENT",
            "00000000000000000000000000000000",
        )
        .args([
            "--yes",
            "desktop",
            "connect",
            "--desktop-client",
            "browser",
            "--port",
            "19999",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("supervisor has closed"));
    assert!(!base.path().join("19999/connection.json").exists());
}

#[test]
fn new_launches_honor_the_current_policy_without_restarting_supervision() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    for keep_alive in ["0", "1"] {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // /bin/sh rejects Chrome's flags immediately; the saved startup record
        // still proves that lifetime ownership was decided before spawning.
        let result = command(base.path())
            .env("TEAMS_DESKTOP_BROWSER_CLIENT", &token)
            .env("TEAMS_DESKTOP_BROWSER_KEEP_ALIVE", keep_alive)
            .args([
                "--yes",
                "desktop",
                "connect",
                "--desktop-client",
                "browser",
                "--port",
                &port.to_string(),
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let state: Value = serde_json::from_slice(
            &fs::read(base.path().join(port.to_string()).join("connection.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["lifetime"]["auto_close"], keep_alive == "0");
    }
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
}

#[test]
fn one_corrupt_record_does_not_block_cleanup_of_other_sessions() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    let damaged = base.path().join("1024");
    mkdir(&damaged);
    fs::write(damaged.join("connection.json"), "corrupt").unwrap();
    drop(supervisor.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        if let Some(status) = supervisor.0.try_wait().unwrap() {
            assert!(!status.success(), "preserved records must be reported");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!browser.running());
    assert_eq!(
        fs::read_to_string(damaged.join("connection.json")).unwrap(),
        "corrupt"
    );
}

fn status(base: &Path, browser: &Browser, token: &str) -> Command {
    let mut cmd = command(base);
    cmd.env("TEAMS_DESKTOP_BROWSER_CLIENT", token).args([
        "desktop",
        "status",
        "--desktop-client",
        "browser",
        "--port",
        &browser.state["port"].to_string(),
    ]);
    cmd
}

#[test]
fn concurrent_raw_reads_and_status_wait_for_the_lifecycle_lock() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let browser = Browser::new(base.path(), &[&token], true);
    let config = base.path().join("teams");
    mkdir(&config);
    fs::write(
        config.join("config.toml"),
        format!(
            "[profiles.default]\nbackend='desktop'\ndesktop_client='browser'\ndesktop_port={}\n",
            browser.state["port"]
        ),
    )
    .unwrap();
    let lock = File::create(browser.dir.join("connection.lock")).unwrap();
    lock.lock().unwrap();
    let mut raw = Process(
        command(base.path())
            .env("XDG_CONFIG_HOME", base.path())
            .env("TEAMS_DESKTOP_BROWSER_CLIENT", &token)
            .args(["--json", "chats", "list"])
            .spawn()
            .unwrap(),
    );
    let mut statuses: Vec<_> = (0..3)
        .map(|_| Process(status(base.path(), &browser, &token).spawn().unwrap()))
        .collect();
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        raw.0.try_wait().unwrap().is_none(),
        "raw reader should wait instead of failing busy"
    );
    assert!(
        statuses
            .iter_mut()
            .all(|p| p.0.try_wait().unwrap().is_none())
    );
    lock.unlock().unwrap();
    raw.0.wait().unwrap();
    use std::io::Read;
    let mut error = String::new();
    raw.0
        .stderr
        .as_mut()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    assert!(!error.contains("another browser connection"));
    for process in &mut statuses {
        wait_exit(process);
    }
    let mut reader = command(base.path());
    reader
        .env("XDG_CONFIG_HOME", base.path())
        .env("TEAMS_DESKTOP_BROWSER_CLIENT", &token)
        .args(["--json", "chats", "list"]);
    let mut child = reader.spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let state: Value =
        serde_json::from_slice(&fs::read(browser.dir.join("connection.json")).unwrap()).unwrap();
    assert!(
        state["lifetime"]["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|user| user["pid"] == pid),
        "real CLI reads must register before using CDP"
    );
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
}

#[test]
fn a_reloaded_window_joins_during_the_shutdown_grace_period() {
    let base = tempfile::tempdir().unwrap();
    let (mut first, one) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&one], true);
    drop(first.0.stdin.take());
    std::thread::sleep(Duration::from_secs(1));
    assert!(browser.running());
    let (mut replacement, token) = supervise(base.path());
    assert!(
        status(base.path(), &browser, &token)
            .status()
            .unwrap()
            .success()
    );
    wait_exit(&mut first);
    assert!(browser.running());
    drop(replacement.0.stdin.take());
    wait_exit(&mut replacement);
    assert!(!browser.running());
}

#[test]
fn a_new_supervisor_reclaims_a_killed_supervisors_registration() {
    let base = tempfile::tempdir().unwrap();
    let (mut first, one) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&one], true);
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let (mut replacement, token) = supervise(base.path());
    assert!(
        status(base.path(), &browser, &token)
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while base.path().join("clients").join(&one).exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(browser.running());
    drop(replacement.0.stdin.take());
    wait_exit(&mut replacement);
    assert!(!browser.running());
}

#[test]
fn lease_identity_is_independent_of_the_callers_locale_and_timezone() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    assert!(
        status(base.path(), &browser, &token)
            .env("TZ", "Pacific/Honolulu")
            .env("LC_ALL", "C")
            .status()
            .unwrap()
            .success()
    );
    let state: Value =
        serde_json::from_slice(&fs::read(browser.dir.join("connection.json")).unwrap()).unwrap();
    assert!(
        state["lifetime"]["clients"]
            .as_array()
            .unwrap()
            .contains(&json!(token))
    );
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(!browser.running());
}

#[test]
fn a_terminal_connect_pins_the_browser_and_manual_recovery_ignores_bad_leases() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    let mut connect = Process(
        command(base.path())
            .args([
                "--yes",
                "desktop",
                "connect",
                "--desktop-client",
                "browser",
                "--port",
                &browser.state["port"].to_string(),
            ])
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state: Value =
            serde_json::from_slice(&fs::read(browser.dir.join("connection.json")).unwrap())
                .unwrap();
        if state["lifetime"]["auto_close"] == false {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    connect.0.kill().unwrap();
    connect.0.wait().unwrap();
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(browser.running());
    browser.state["lifetime"]["clients"] = json!(["corrupt token"]);
    browser.save();
    let result = command(base.path())
        .env("TEAMS_DESKTOP_BROWSER_CLIENT", "expired")
        .args([
            "--yes",
            "desktop",
            "recover",
            "--desktop-client",
            "browser",
            "--port",
            &browser.state["port"].to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!browser.running());
}

#[test]
fn cached_browser_clients_renew_their_lease_after_a_relaunch() {
    use std::io::Write;
    if let Ok(port) = std::env::var("TEAMS_LIFETIME_CACHED_CLIENT_HELPER") {
        let port = port.parse::<u16>().unwrap();
        let client = teams_cli::desktop::DesktopClient::with_client(
            port,
            teams_cli::config::DesktopAccount::Auto,
            teams_cli::config::DesktopClientKind::Browser,
        )
        .unwrap();
        println!("LEASE_HELPER_READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.probe())
            .unwrap();
        let base = PathBuf::from(std::env::var_os("TEAMS_DESKTOP_BROWSER_DATA_DIR").unwrap());
        let state: Value = serde_json::from_slice(
            &fs::read(base.join(port.to_string()).join("connection.json")).unwrap(),
        )
        .unwrap();
        assert!(
            state["lifetime"]["users"]
                .as_array()
                .unwrap()
                .iter()
                .any(|user| user["pid"] == std::process::id())
        );
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    let mut helper = Process(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cached_browser_clients_renew_their_lease_after_a_relaunch",
                "--nocapture",
            ])
            .env(
                "TEAMS_LIFETIME_CACHED_CLIENT_HELPER",
                browser.state["port"].to_string(),
            )
            .env("TEAMS_DESKTOP_BROWSER_DATA_DIR", base.path())
            .env("TEAMS_DESKTOP_BROWSER", "/bin/sh")
            .env("TEAMS_DESKTOP_BROWSER_CLIENT", &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = helper.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut send = Some(send);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("LEASE_HELPER_READY")
                && let Some(send) = send.take()
            {
                let _ = send.send(());
            }
        }
    });
    receive.recv_timeout(Duration::from_secs(10)).unwrap();
    // A fresh launch creates a new lifetime without the cached client's old lease.
    browser.state["lifetime"]["users"] = json!([]);
    browser.save();
    helper.0.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
    wait_exit(&mut helper);
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(!browser.running());
}

#[test]
fn shutdown_retries_a_damaged_record_that_is_repaired_during_backoff() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, _) = supervise(base.path());
    let damaged = base.path().join("1024");
    mkdir(&damaged);
    fs::write(damaged.join("connection.json"), "partial state").unwrap();
    drop(supervisor.0.stdin.take());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        supervisor.0.try_wait().unwrap().is_none(),
        "a single transient error must not abandon cleanup"
    );
    fs::remove_file(damaged.join("connection.json")).unwrap();
    wait_exit(&mut supervisor);
}

#[test]
fn persistent_readers_ignore_stale_tokens_and_lifecycle_locks() {
    let base = tempfile::tempdir().unwrap();
    let browser = Browser::new(base.path(), &[], false);
    let config = base.path().join("teams");
    mkdir(&config);
    fs::write(
        config.join("config.toml"),
        format!(
            "[profiles.default]\nbackend='desktop'\ndesktop_client='browser'\ndesktop_port={}\n",
            browser.state["port"]
        ),
    )
    .unwrap();
    let before = fs::read(browser.dir.join("connection.json")).unwrap();
    let lock = File::create(browser.dir.join("connection.lock")).unwrap();
    lock.lock().unwrap();
    let start = Instant::now();
    let output = command(base.path())
        .env("XDG_CONFIG_HOME", base.path())
        .env("TEAMS_DESKTOP_BROWSER_CLIENT", "expired")
        .args(["--json", "chats", "list"])
        .output()
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("supervisor"), "{error}");
    assert!(!error.contains("another browser connection"), "{error}");
    assert_eq!(
        before,
        fs::read(browser.dir.join("connection.json")).unwrap()
    );
}

#[test]
fn an_uncertain_peer_lease_preserves_the_browser_without_blocking_healthy_clients() {
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token, "damaged peer token"], true);
    let result = status(base.path(), &browser, &token).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(browser.running(), "uncertainty must preserve the session");
    let result = command(base.path())
        .args([
            "--yes",
            "desktop",
            "recover",
            "--desktop-client",
            "browser",
            "--port",
            &browser.state["port"].to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!browser.running());
}

#[test]
fn verified_helpers_are_recovered_after_parent_exit_without_killing_other_group_members() {
    interrupted_helper_cleanup(false);
}

#[test]
fn reconnect_finishes_saved_helpers_before_forgetting_a_dead_browser() {
    interrupted_helper_cleanup(true);
}

fn interrupted_helper_cleanup(reconnect: bool) {
    use std::os::unix::process::CommandExt;
    fn stamp(pid: u32) -> Option<String> {
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "stat=,lstart="])
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let (state, started) = text.trim().split_once(char::is_whitespace)?;
        // Containers may have an init that does not reap orphans. A zombie has
        // already exited and cannot consume browser resources or receive signals.
        (!state.starts_with('Z')).then(|| started.trim().to_owned())
    }
    struct Helper {
        pid: u32,
        group: u32,
        started: String,
    }
    impl Drop for Helper {
        fn drop(&mut self) {
            if stamp(self.pid).as_ref() == Some(&self.started)
                && unsafe { libc::getpgid(self.pid as i32) } == self.group as i32
            {
                unsafe {
                    libc::kill(self.pid as i32, libc::SIGKILL);
                }
            }
        }
    }
    let base = tempfile::tempdir().unwrap();
    let (mut supervisor, token) = supervise(base.path());
    let mut browser = Browser::new(base.path(), &[&token], true);
    browser.process.0.kill().unwrap();
    browser.process.0.wait().unwrap();
    let one = browser.dir.join("helper-fifo");
    let two = browser.dir.join("external-fifo");
    for fifo in [&one, &two] {
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    }
    let mut root = Process(
        Command::new("/bin/sh")
            .args([
                "-c",
                r#"
      /bin/sh -c 'trap "" TERM; read line < "$1"' helper "$1" --type=utility &
      printf '%s\n' "$!"
      /bin/sh -c 'read line < "$1"' unrelated "$2" &
      printf '%s\n' "$!"
      read line
    "#,
                "browser",
            ])
            .arg(&one)
            .arg(&two)
            .arg(format!(
                "--user-data-dir={}",
                browser.dir.join("profile").display()
            ))
            .arg(format!("--remote-debugging-port={}", browser.state["port"]))
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut lines = BufReader::new(root.0.stdout.take().unwrap()).lines();
    let helper = lines.next().unwrap().unwrap().parse::<u32>().unwrap();
    let external = lines.next().unwrap().unwrap().parse::<u32>().unwrap();
    let _helper_guard = Helper {
        pid: helper,
        group: root.0.id(),
        started: stamp(helper).unwrap(),
    };
    let _external_guard = Helper {
        pid: external,
        group: root.0.id(),
        started: stamp(external).unwrap(),
    };
    std::thread::sleep(Duration::from_millis(100));
    browser.state["owner"] = identity(root.0.id(), Path::new("/bin/sh"));
    browser.process = root;
    browser.save();
    drop(supervisor.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let state: Value =
            serde_json::from_slice(&fs::read(browser.dir.join("connection.json")).unwrap())
                .unwrap();
        if state["children"].as_array().is_some_and(|children| {
            children
                .iter()
                .any(|child| child["identity"]["pid"] == helper)
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the verified helper must be saved before closing its parent"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Interrupt cleanup while helpers are still alive. Their persisted identities
    // must be sufficient for another supervisor after the browser PID is gone.
    let deadline = Instant::now() + Duration::from_secs(15);
    while browser.running() {
        assert!(
            Instant::now() < deadline,
            "browser parent must exit before interrupted recovery"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    supervisor.0.kill().unwrap();
    supervisor.0.wait().unwrap();
    if reconnect {
        let _result = command(base.path())
            .args([
                "--yes",
                "desktop",
                "connect",
                "--desktop-client",
                "browser",
                "--port",
                &browser.state["port"].to_string(),
            ])
            .output()
            .unwrap();
    } else {
        let (mut replacement, _) = supervise(base.path());
        drop(replacement.0.stdin.take());
        wait_exit(&mut replacement);
    }
    assert!(!browser.running());
    let deadline = Instant::now() + Duration::from_secs(5);
    while stamp(helper).is_some() {
        assert!(
            Instant::now() < deadline,
            "a verified browser helper was left behind"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        stamp(external).is_some(),
        "an external app in the group must remain untouched"
    );
}

#[test]
fn an_os_verified_browser_with_an_unresponsive_http_endpoint_is_stopped() {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let base = tempfile::tempdir().unwrap();
    let mut browser = Browser::new(base.path(), &[], true);
    browser.process.0.kill().unwrap();
    browser.process.0.wait().unwrap();
    let socket =
        std::net::TcpListener::bind(("127.0.0.1", browser.state["port"].as_u64().unwrap() as u16))
            .unwrap();
    let fd = socket.as_raw_fd();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "read line", "browser"])
        .arg(format!(
            "--user-data-dir={}",
            browser.dir.join("profile").display()
        ))
        .arg(format!("--remote-debugging-port={}", browser.state["port"]))
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    browser.process = Process(cmd.spawn().unwrap());
    drop(socket);
    std::thread::sleep(Duration::from_millis(50));
    let owner = identity(browser.process.0.id(), Path::new("/bin/sh"));
    let executable = PathBuf::from(owner["executable"].as_str().unwrap());
    browser.state["owner"] = owner;
    let (mut supervisor, token) = supervise_using(base.path(), &executable);
    browser.state["lifetime"]["clients"] = json!([token]);
    browser.save();
    drop(supervisor.0.stdin.take());
    wait_exit(&mut supervisor);
    assert!(!browser.running());
    assert!(!browser.dir.join("connection.json").exists());
}
