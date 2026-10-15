//! Run credential helpers with secrets on stdin, bounded output and a deadline.
use std::io::{self, Read, Write};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(super) fn run(mut command: Command, input: Vec<u8>, timeout: Duration) -> io::Result<Output> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other(
                "credential helper response exceeds size limit",
            ));
        }
        Ok(bytes)
    });
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => {
                // pass invokes GPG, which may be waiting for pinentry. Stop the
                // helper's entire group so pipes and prompts cannot outlive it.
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break Err(result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "credential helper timed out")
                }));
            }
        }
    };
    let written = writer
        .join()
        .map_err(|_| io::Error::other("credential helper writer failed"))?;
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("credential helper reader failed"))?;
    Ok(Output {
        status: status?,
        stdout: {
            written?;
            bytes?
        },
        stderr: Vec::new(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn blocked_input_and_inherited_output_cannot_outlive_the_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();
        let error = run(command, vec![b'x'; 200_000], Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
