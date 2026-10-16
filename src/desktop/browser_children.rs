//! Record verified browser helpers before closing their parent. Chromium can leave
//! a helper alive after Browser.close, so parent exit alone is not completion.
//! Never signal a process group: each saved PID is reverified independently.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Child {
    identity: Identity,
    started: String,
}

fn group(pid: u32) -> Option<u32> {
    let pid = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    // SAFETY: getpgid only reads process metadata.
    u32::try_from(unsafe { libc::getpgid(pid) }).ok()
}

fn browser_helper(browser: &Path, executable: &Path, args: &[OsString]) -> bool {
    // An app opened by Chrome is not a browser helper, even if it inherits the
    // group. Require a Chromium child role and its original browser package.
    let role = args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("--type="));
    let crashpad = executable
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with("_crashpad_handler"));
    if !role && !crashpad {
        return false;
    }
    if executable == browser {
        return true;
    }
    #[cfg(target_os = "macos")]
    if let Some(bundle) = browser
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
    {
        return executable.starts_with(bundle);
    }
    crashpad && executable.parent() == browser.parent()
}

fn gone(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only checks existence.
    (unsafe { libc::kill(pid, 0) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
        || matches!(system::zombie(pid as u32), Ok(true))
}

// Only uninterrupted helper ancestry establishes ownership. A separately
// launched browser root or application is a boundary, even in our process group.
fn descends_from(pid: u32, owner: u32, parents: &std::collections::HashMap<u32, u32>) -> bool {
    let mut next = pid;
    let mut visited = std::collections::HashSet::new();
    while visited.insert(next) {
        let Some(parent) = parents.get(&next) else {
            return false;
        };
        if *parent == owner {
            return true;
        }
        next = *parent;
    }
    false
}

fn crashpad(args: &[OsString]) -> bool {
    args.first().is_some_and(|arg| {
        Path::new(arg)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("_crashpad_handler"))
    })
}

fn private_database(args: &[OsString], dir: &Path) -> bool {
    let mut paths = args.iter().enumerate().filter_map(|(i, arg)| {
        let text = arg.to_string_lossy();
        if text == "--database" {
            args.get(i + 1).map(PathBuf::from)
        } else {
            text.strip_prefix("--database=").map(PathBuf::from)
        }
    });
    let Some(path) = paths.next() else {
        return false;
    };
    paths.next().is_none()
        && path
            .canonicalize()
            .ok()
            .zip(profile(dir).canonicalize().ok())
            .is_some_and(|(path, profile)| path.starts_with(profile))
}

#[cfg(target_os = "linux")]
fn inspect(pid: u32) -> Result<(Identity, Vec<OsString>, u32), AppError> {
    let dir = PathBuf::from(format!("/proc/{pid}"));
    let before = std::fs::read_to_string(dir.join("stat"))?;
    let args = system::arguments(&std::fs::read(dir.join("cmdline"))?);
    let status = std::fs::read_to_string(dir.join("status"))?;
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .ok_or_else(|| failed("missing browser helper UID"))?;
    // /proc/PID itself becomes root-owned for non-dumpable sandbox helpers.
    // The kernel's real/effective UIDs remain available in status.
    if !uid
        .split_whitespace()
        .take(2)
        .all(|uid| uid.parse::<u32>().ok() == Some(unsafe { libc::geteuid() }))
    {
        return Err(failed("browser helper belongs to another user"));
    }
    let executable = match std::fs::read_link(dir.join("exe")) {
        Ok(path) => path,
        Err(error) if matches!(error.raw_os_error(), Some(libc::EACCES | libc::EPERM)) => {
            // This fallback is used only with verified helper ancestry, role,
            // package, UID, kernel start time and dedicated group membership.
            let path = args
                .first()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| failed("sandbox helper executable cannot be verified"))?;
            path.canonicalize()?
        }
        Err(error) => return Err(error.into()),
    };
    let parent = before
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(1))
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| failed("invalid helper parent"))?;
    let started = system::start_time(&before)?;
    if system::start_time(&std::fs::read_to_string(dir.join("stat"))?)? != started {
        return Err(failed("browser helper changed during inspection"));
    }
    Ok((
        Identity {
            pid,
            started,
            executable,
        },
        args,
        parent,
    ))
}

#[cfg(target_os = "macos")]
fn inspect(pid: u32) -> Result<(Identity, Vec<OsString>, u32), AppError> {
    let (identity, args) = system::process(pid)?;
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()?;
    let parent = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| failed("invalid helper parent"))?;
    Ok((identity, args, parent))
}

pub(super) fn capture(state: &State, dir: &Path) -> Result<Vec<Child>, AppError> {
    let Some(owner) = state.owner.as_ref() else {
        return Ok(vec![]);
    };
    if !owned(state, dir) {
        return Err(failed(
            "the browser process changed; its helpers were left untouched",
        ));
    }
    // All our browser launches get a dedicated group. Legacy/unisolated sessions
    // cannot establish ownership of helpers this way and retain parent-only close.
    if group(owner.pid) != Some(owner.pid) {
        return Ok(vec![]);
    }
    let mut candidates = vec![];
    let mut parents = std::collections::HashMap::new();
    for pid in system::pids()? {
        if pid == owner.pid || group(pid) != Some(owner.pid) {
            continue;
        }
        #[cfg(target_os = "linux")]
        {
            let args = match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(bytes) => system::arguments(&bytes),
                Err(_) if gone(pid) => continue,
                Err(error) => return Err(error.into()),
            };
            if !args
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with("--type="))
                && !crashpad(&args)
            {
                continue;
            }
        }
        let (identity, args, parent) = match inspect(pid) {
            Ok(value) => value,
            Err(_) if gone(pid) => continue,
            Err(_) => {
                return Err(failed(
                    "a process in the dedicated browser group could not be inspected; its recovery record was preserved",
                ));
            }
        };
        if !browser_helper(&owner.executable, &identity.executable, &args) {
            continue;
        }
        let (lease, _) = match system::lease_process(pid) {
            Ok(value) => value,
            Err(_) if gone(pid) => continue,
            Err(_) => {
                return Err(failed(
                    "a browser helper's start time could not be verified; its recovery record was preserved",
                ));
            }
        };
        // Bracket the snapshot so PID reuse or a concurrent exec is not adopted.
        if group(pid) == Some(owner.pid)
            && inspect(pid).is_ok_and(|(again, again_args, again_parent)| {
                again == identity && again_args == args && again_parent == parent
            })
        {
            let is_crashpad = crashpad(&args);
            if !is_crashpad || private_database(&args, dir) {
                parents.insert(pid, parent);
                candidates.push((
                    Child {
                        identity,
                        started: lease.started,
                    },
                    is_crashpad,
                ));
            }
        } else if !gone(pid) {
            return Err(failed(
                "a browser helper changed during verification; its recovery record was preserved",
            ));
        }
    }
    if !owned(state, dir) {
        return Err(failed(
            "the browser process changed; its helpers were left untouched",
        ));
    }
    Ok(candidates
        .into_iter()
        .filter_map(|(child, crashpad)| {
            (crashpad || descends_from(child.identity.pid, owner.pid, &parents)).then_some(child)
        })
        .collect())
}

fn lease_present(child: &Child) -> Result<bool, AppError> {
    let pid = i32::try_from(child.identity.pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| failed("invalid browser helper process ID"))?;
    // SAFETY: signal zero only checks existence.
    if unsafe { libc::kill(pid, 0) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Ok(false);
    }
    let lease = Identity {
        started: child.started.clone(),
        ..child.identity.clone()
    };
    lifetime::live(&lease)
}

fn present(child: &Child, group_id: u32, browser: &Path) -> Result<bool, AppError> {
    if !lease_present(child)? {
        return Ok(false);
    }
    let (current, args, _) = match inspect(child.identity.pid) {
        Ok(value) => value,
        Err(_) if gone(child.identity.pid) => return Ok(false),
        Err(error) => return Err(error),
    };
    if current.executable != child.identity.executable
        || group(current.pid) != Some(group_id)
        || !browser_helper(browser, &current.executable, &args)
    {
        return Err(failed(
            "a saved browser helper changed; it was left untouched",
        ));
    }
    lease_present(child)
}

async fn wait(children: &[Child], duration: Duration) -> Result<bool, AppError> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let mut active = false;
        for child in children {
            active |= lease_present(child)?;
        }
        if !active {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn signal(children: &[Child], group_id: u32, browser: &Path, signal: i32) -> Result<(), AppError> {
    for child in children {
        if present(child, group_id, browser)? {
            // SAFETY: positive PID with matching kernel start time, executable,
            // and the dedicated group captured while its browser was verified.
            if unsafe { libc::kill(child.identity.pid as i32, signal) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn finish(state: &State) -> Result<(), AppError> {
    let Some(owner) = state.owner.as_ref() else {
        return Ok(());
    };
    if state.children.is_empty() || wait(&state.children, Duration::from_secs(2)).await? {
        return Ok(());
    }
    signal(&state.children, owner.pid, &owner.executable, libc::SIGTERM)?;
    if wait(&state.children, Duration::from_secs(2)).await? {
        return Ok(());
    }
    signal(&state.children, owner.pid, &owner.executable, libc::SIGKILL)?;
    if wait(&state.children, Duration::from_secs(5)).await? {
        return Ok(());
    }
    Err(failed(
        "a verified browser helper has not exited; its recovery record was preserved",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ancestry_stops_at_another_browser_root_or_external_application() {
        let parents = std::collections::HashMap::from([(2, 1), (3, 2), (5, 4), (6, 5), (8, 8)]);
        assert!(descends_from(2, 1, &parents));
        assert!(descends_from(3, 1, &parents));
        assert!(!descends_from(5, 1, &parents));
        assert!(!descends_from(6, 1, &parents));
        assert!(!descends_from(8, 1, &parents));
    }

    #[test]
    fn crashpad_database_must_resolve_inside_the_managed_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(profile(dir.path()).join("Crashpad")).unwrap();
        let database = profile(dir.path()).join("Crashpad");
        let arg = format!("--database={}", database.display());
        assert!(private_database(&[arg.clone().into()], dir.path()));
        assert!(!private_database(
            &[arg.into(), "--database=/tmp".into()],
            dir.path()
        ));
        assert!(!private_database(&["--database=/tmp".into()], dir.path()));
        assert!(!private_database(&[], dir.path()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_dumpable_same_user_helper_can_be_inspected() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(fds[0]);
                let ready = if libc::prctl(libc::PR_SET_DUMPABLE, 0) == 0 {
                    1u8
                } else {
                    0u8
                };
                libc::write(fds[1], (&ready as *const u8).cast(), 1);
                libc::close(fds[1]);
                loop {
                    libc::pause();
                }
            }
        }
        unsafe {
            libc::close(fds[1]);
        }
        let mut ready = 0u8;
        let read = unsafe { libc::read(fds[0], (&mut ready as *mut u8).cast(), 1) };
        let exe_access = std::fs::read_link(format!("/proc/{pid}/exe"));
        let result = inspect(pid as u32);
        unsafe {
            libc::close(fds[0]);
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert_eq!(read, 1);
        assert_eq!(ready, 1);
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                exe_access.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(
            result.unwrap().0.executable,
            std::env::current_exe().unwrap()
        );
    }

    #[test]
    fn external_apps_and_other_browser_roots_are_not_helpers() {
        let browser = Path::new("/opt/chrome/chrome");
        assert!(browser_helper(
            browser,
            browser,
            &["--type=renderer".into()]
        ));
        assert!(!browser_helper(
            browser,
            browser,
            &["--user-data-dir=another-profile".into()]
        ));
        assert!(!browser_helper(
            browser,
            Path::new("/usr/bin/editor"),
            &["--type=renderer".into()]
        ));
        assert!(browser_helper(
            browser,
            Path::new("/opt/chrome/chrome_crashpad_handler"),
            &[]
        ));
        assert!(!browser_helper(
            browser,
            Path::new("/other/chrome_crashpad_handler"),
            &[]
        ));
    }
}
