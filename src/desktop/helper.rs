//! The WSL bridge: the Linux CLI keeps one `teams.exe desktop-helper` running and talks
//! newline-delimited JSON over its stdin and stdout. The helper exits when stdin closes.
//! The protocol number is the whole contract: a helper is accepted when it speaks this
//! CLI's protocol, whatever its version or build, which are only reported. Bump it when
//! either side would misread the other (`desktop::tests::the_protocol_is_pinned`).
//! `hello` and its `version` and `protocol` fields never change. Protocol 2 adds sending.
use crate::error::AppError;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL: u32 = 2;
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit this binary was built from (see `build.rs`), reported in `hello` for
/// diagnosis and never compared; empty when unknown.
const BUILD: &str = env!("TEAMS_BUILD");
const MAX_LINE: usize = super::MAX_READ + 64 * 1024;

/// Reads one line, refusing lines beyond `MAX_LINE`; `None` at end of input.
async fn read_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<String>, AppError> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (chunk, done) = match buffer.iter().position(|&b| b == b'\n') {
            Some(end) => (&buffer[..end], Some(end + 1)),
            None => (buffer, None),
        };
        if line.len() + chunk.len() > MAX_LINE {
            return Err(AppError::Desktop(
                "desktop helper message is too large".into(),
            ));
        }
        line.extend_from_slice(chunk);
        let consumed = done.unwrap_or(chunk.len());
        reader.consume(consumed);
        if done.is_some() {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

async fn write_line(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<(), AppError> {
    let mut line = value.to_string();
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// The helper side: answers requests in order until the input closes.
#[cfg(any(windows, test))]
pub async fn serve<S: super::owner::System>(
    mut reader: impl AsyncBufRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    session: &mut super::session::Session<S>,
) -> Result<(), AppError> {
    while let Some(line) = read_line(&mut reader).await? {
        let request: Value = serde_json::from_str(&line).unwrap_or_default();
        let result = match request["op"].as_str() {
            Some("hello") => Ok(json!({"version": VERSION, "protocol": PROTOCOL, "build": BUILD})),
            Some("probe") => match request["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
                Some(port) => session.probe(port).await.map(|metadata| json!(metadata)),
                None => Err(AppError::InvalidInput("invalid desktop port".into())),
            },
            Some("read") => session.read(&request["request"]).await,
            Some("send") => session.send(&request["request"]).await,
            Some("send_check") => {
                match request["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
                    Some(port) => session.send_check(port).await,
                    None => Err(AppError::InvalidInput("invalid desktop port".into())),
                }
            }
            _ => Err(AppError::InvalidInput(
                "unknown desktop helper request".into(),
            )),
        };
        let reply = match result {
            Ok(value) => json!({"id": request["id"], "ok": value}),
            Err(error) => json!({"id": request["id"], "error": error.to_string()}),
        };
        write_line(&mut writer, &reply).await?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub use client::HelperClient;

#[cfg(not(windows))]
mod client {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        process::Stdio,
        time::Duration,
    };
    use tokio::{
        io::BufReader,
        process::{Child, ChildStdin, ChildStdout, Command},
    };

    const START_TIMEOUT: Duration = Duration::from_secs(20);
    const CALL_TIMEOUT: Duration = Duration::from_secs(45);

    enum Failure {
        /// The helper answered with an error; a new helper would answer the same.
        Refused(String),
        /// The helper is gone or unusable.
        Transport,
    }

    struct Running {
        // Held so the helper is killed when this connection is dropped.
        _child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
        next_id: u64,
    }

    impl Running {
        async fn call(&mut self, mut request: Value) -> Result<Value, Failure> {
            self.next_id += 1;
            request["id"] = json!(self.next_id);
            let exchange = async {
                write_line(&mut self.stdin, &request).await.ok()?;
                let line = read_line(&mut self.stdout).await.ok()??;
                serde_json::from_str::<Value>(&line).ok()
            };
            let reply = tokio::time::timeout(CALL_TIMEOUT, exchange)
                .await
                .ok()
                .flatten()
                .ok_or(Failure::Transport)?;
            if reply["id"] != self.next_id {
                return Err(Failure::Transport);
            }
            match (reply.get("ok"), reply["error"].as_str()) {
                (Some(ok), _) => Ok(ok.clone()),
                (None, Some(message)) => Err(Failure::Refused(message.into())),
                _ => Err(Failure::Transport),
            }
        }
    }

    /// One helper per CLI process, started on first use and restarted once if it dies.
    #[derive(Default)]
    pub struct HelperClient {
        running: Option<Running>,
        /// Resolved, and installed if needed, on first start.
        executable: Option<PathBuf>,
    }

    impl HelperClient {
        pub async fn call(&mut self, request: Value) -> Result<Value, AppError> {
            if let Some(running) = &mut self.running {
                match running.call(request.clone()).await {
                    Ok(value) => return Ok(value),
                    Err(Failure::Refused(message)) => return Err(AppError::Desktop(message)),
                    Err(Failure::Transport) => self.running = None,
                }
            }
            match self.started().await?.call(request).await {
                Ok(value) => Ok(value),
                Err(Failure::Refused(message)) => Err(AppError::Desktop(message)),
                Err(Failure::Transport) => {
                    self.running = None;
                    Err(AppError::Desktop(
                        "the Windows desktop helper stopped unexpectedly".into(),
                    ))
                }
            }
        }

        /// Sends `request` exactly once. A lost answer becomes `{"error": "unconfirmed"}`
        /// instead of a retry, because the helper may already have acted on it.
        pub async fn call_once(&mut self, request: Value) -> Result<Value, AppError> {
            match self.started().await?.call(request).await {
                Ok(value) => Ok(value),
                Err(Failure::Refused(message)) => Err(AppError::Desktop(message)),
                Err(Failure::Transport) => {
                    self.running = None;
                    Ok(json!({"error": "unconfirmed"}))
                }
            }
        }

        /// The running helper, started first if needed.
        async fn started(&mut self) -> Result<&mut Running, AppError> {
            let running = match self.running.take() {
                Some(running) => running,
                None => {
                    let executable = match &self.executable {
                        Some(executable) => executable.clone(),
                        None => super::super::install::ensure().await?,
                    };
                    let running = start(&executable).await?;
                    self.executable = Some(executable);
                    running
                }
            };
            Ok(self.running.insert(running))
        }
    }

    /// Why a helper that answered `hello` cannot serve this CLI, if it cannot.
    fn mismatch(hello: &Value) -> Option<String> {
        (hello["protocol"] != PROTOCOL).then(|| {
            let theirs = describe(
                hello["version"].as_str(),
                hello["build"].as_str(),
                hello["protocol"].as_u64(),
            );
            let ours = describe(Some(VERSION), Some(BUILD), Some(PROTOCOL.into()));
            format!("is {theirs}, but this CLI is {ours}")
        })
    }

    /// `teams-cli 0.1.9 (abc123) speaking protocol 2`, leaving out what is unknown.
    fn describe(version: Option<&str>, build: Option<&str>, protocol: Option<u64>) -> String {
        let mut text = format!("teams-cli {}", version.unwrap_or("of an unknown version"));
        if let Some(build) = build.filter(|b| !b.is_empty()) {
            text.push_str(&format!(" ({build})"));
        }
        match protocol {
            Some(protocol) => text.push_str(&format!(" speaking protocol {protocol}")),
            None => text.push_str(" without a protocol number"),
        }
        text
    }

    async fn start(executable: &Path) -> Result<Running, AppError> {
        let mut child = Command::new(executable)
            .arg("desktop-helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                AppError::Desktop(format!(
                    "could not start the Windows desktop helper at {}; check that WSL can run Windows executables",
                    executable.display()
                ))
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(AppError::Desktop(
                "desktop helper pipes are unavailable".into(),
            ));
        };
        let mut running = Running {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        };
        let hello = tokio::time::timeout(START_TIMEOUT, running.call(json!({"op": "hello"})))
            .await
            .ok()
            .and_then(Result::ok);
        let problem = match &hello {
            None => Some("did not answer".to_owned()),
            Some(hello) => mismatch(hello),
        };
        if let Some(problem) = problem {
            return Err(AppError::Desktop(format!(
                "the Windows desktop helper at {} {problem}; {}",
                executable.display(),
                super::super::install::remedy()
            )));
        }
        Ok(running)
    }

    #[cfg(all(test, unix))]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// A stand-in helper: answers hello with `version`, then echoes requests, and
        /// exits after `lives` echoes.
        fn fake_helper(dir: &std::path::Path, version: &str, protocol: u32, lives: u32) -> PathBuf {
            let path = dir.join("teams.exe");
            let script = format!(
                r#"#!/bin/sh
[ "$1" = desktop-helper ] || exit 2
n=0
while read -r line; do
  id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  case "$line" in
    *'"op":"hello"'*) echo "{{\"id\":$id,\"ok\":{{\"version\":\"{version}\",\"protocol\":{protocol},\"build\":\"0123abcd\"}}}}" ;;
    *'"op":"fail"'*) echo "{{\"id\":$id,\"error\":\"refused\"}}" ;;
    *) n=$((n+1)); echo "{{\"id\":$id,\"ok\":$n}}"; [ $n -ge {lives} ] && exit 0 ;;
  esac
done
"#
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[tokio::test]
        async fn handshakes_and_refuses_other_protocols_only() {
            let dir = tempfile::tempdir().unwrap();
            let mut running = start(&fake_helper(dir.path(), VERSION, PROTOCOL, 9))
                .await
                .unwrap();
            assert_eq!(running.call(json!({"op": "x"})).await.ok(), Some(json!(1)));
            assert_eq!(running.call(json!({"op": "x"})).await.ok(), Some(json!(2)));
            assert!(matches!(
                running.call(json!({"op": "fail"})).await,
                Err(Failure::Refused(m)) if m == "refused"
            ));
            // Another version or build of the same protocol is accepted.
            let older = tempfile::tempdir().unwrap();
            assert!(
                start(&fake_helper(older.path(), "0.0.1", PROTOCOL, 9))
                    .await
                    .is_ok()
            );
            let other = tempfile::tempdir().unwrap();
            let error = start(&fake_helper(other.path(), "0.0.1", 1, 9))
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(
                error
                    .contains("is teams-cli 0.0.1 (0123abcd) speaking protocol 1, but this CLI is"),
                "{error}"
            );
            assert!(
                error.contains(&format!("speaking protocol {PROTOCOL};")),
                "{error}"
            );
        }

        #[tokio::test]
        async fn a_dead_helper_is_replaced_once() {
            let dir = tempfile::tempdir().unwrap();
            let mut client = HelperClient {
                running: None,
                executable: Some(fake_helper(dir.path(), VERSION, PROTOCOL, 1)),
            };
            // Each helper answers once, then exits; the next call starts a new one.
            for _ in 0..3 {
                assert_eq!(client.call(json!({"op": "x"})).await.unwrap(), json!(1));
            }
            assert!(matches!(
                client.call(json!({"op": "fail"})).await,
                Err(AppError::Desktop(m)) if m == "refused"
            ));
        }

        #[tokio::test]
        async fn a_lost_answer_to_a_single_call_is_unconfirmed() {
            let dir = tempfile::tempdir().unwrap();
            // The helper exits right after its first answer, so it is gone for the second.
            let mut client = HelperClient {
                running: None,
                executable: Some(fake_helper(dir.path(), VERSION, PROTOCOL, 1)),
            };
            assert_eq!(
                client.call_once(json!({"op": "x"})).await.unwrap(),
                json!(1)
            );
            assert_eq!(
                client.call_once(json!({"op": "x"})).await.unwrap(),
                json!({"error": "unconfirmed"})
            );
            // The next call starts a fresh helper rather than reusing the dead one.
            assert_eq!(
                client.call_once(json!({"op": "x"})).await.unwrap(),
                json!(1)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{owner::tests::FakeSystem, session::Session};
    use tokio::io::{BufReader, duplex};

    #[tokio::test]
    async fn serves_hello_probe_errors_and_stops_at_end_of_input() {
        let (client, server) = duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server);
        let task = tokio::spawn(async move {
            let mut session = Session::new(FakeSystem {
                listeners: vec![],
                ..FakeSystem::teams()
            });
            serve(BufReader::new(server_read), server_write, &mut session).await
        });
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);
        let mut call = async |request: Value| {
            write_line(&mut client_write, &request).await.unwrap();
            serde_json::from_str::<Value>(&read_line(&mut client_read).await.unwrap().unwrap())
                .unwrap()
        };
        let hello = call(json!({"id": 1, "op": "hello"})).await;
        assert_eq!(hello["id"], 1);
        assert_eq!(
            hello["ok"],
            json!({"version": VERSION, "protocol": PROTOCOL, "build": BUILD})
        );
        let probe = call(json!({"id": 2, "op": "probe", "port": 9222})).await;
        assert_eq!(probe["ok"]["status"], "no_listener");
        let bad = call(json!({"id": 3, "op": "read", "request": {"port": 9222}})).await;
        assert!(bad["error"].is_string() && bad.get("ok").is_none());
        let send = call(json!({"id": 4, "op": "send", "request": {"port": 9222}})).await;
        assert!(send["error"].is_string() && send.get("ok").is_none());
        let check = call(json!({"id": 5, "op": "send_check", "port": 9222})).await;
        assert_eq!(check["ok"]["error"], "no_listener");
        let unknown = call(json!({"id": 6, "op": "launch"})).await;
        assert!(unknown["error"].is_string());
        // Both halves share the stream; the server sees end of input once both are gone.
        drop((client_read, client_write));
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_lines_are_refused() {
        let data = vec![b'x'; MAX_LINE + 1];
        let mut reader = BufReader::new(&data[..]);
        assert!(read_line(&mut reader).await.is_err());
        let mut reader = BufReader::new(&b"a\nb"[..]);
        assert_eq!(read_line(&mut reader).await.unwrap().as_deref(), Some("a"));
        assert_eq!(read_line(&mut reader).await.unwrap().as_deref(), Some("b"));
        assert_eq!(read_line(&mut reader).await.unwrap(), None);
    }
}
