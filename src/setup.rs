//! Interactive setup selects user intent; explicit CLI flags keep their existing contract.
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

use crate::cli::InitArgs;
use crate::config::{BackendKind, DesktopAccount, DesktopClientKind, Profile};
use crate::error::AppError;

pub struct Plan {
    pub name: String,
    pub args: InitArgs,
    pub browser_confirmed: bool,
}

pub fn offered(args: &InitArgs) -> bool {
    args.backend.is_none()
        && args.desktop_port.is_none()
        && args.desktop_account.is_none()
        && args.desktop_client.is_none()
        && args.client_id.is_none()
        && args.tenant.is_none()
        && !args.no_login
        && !args.device_code
        && !args.channel_history
        && !(args.allow_send && args.read_only)
}

fn choice(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
    default: &str,
    allowed: &[&str],
) -> Result<Option<String>, AppError> {
    loop {
        write!(output, "{prompt} [{default}]: ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        let answer = answer.trim().to_ascii_lowercase();
        if matches!(answer.as_str(), "q" | "quit") {
            return Ok(None);
        }
        let answer = if answer.is_empty() { default } else { &answer };
        if allowed.contains(&answer) {
            return Ok(Some(answer.into()));
        }
        writeln!(output, "Choose {}, or q to cancel.", allowed.join(" / "))?;
    }
}

fn confirm(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
) -> Result<bool, AppError> {
    Ok(
        choice(input, output, prompt, "n", &["y", "yes", "n", "no"])?
            .is_some_and(|answer| matches!(answer.as_str(), "y" | "yes")),
    )
}

fn compatible(
    profile: &Profile,
    backend: BackendKind,
    kind: DesktopClientKind,
    account: DesktopAccount,
) -> bool {
    profile.backend == backend
        && (backend == BackendKind::Graph
            || (profile.desktop_client == kind && profile.desktop_account == account))
}

fn profile_name(
    requested: Option<&str>,
    profiles: &BTreeMap<String, Profile>,
    backend: BackendKind,
    kind: DesktopClientKind,
    account: DesktopAccount,
) -> String {
    if let Some(name) = requested {
        return name.into();
    }
    let base = if backend == BackendKind::Graph {
        "graph"
    } else if account == DesktopAccount::WorkSchool {
        "work"
    } else {
        "personal"
    };
    for number in 1.. {
        let name = if number == 1 {
            base.into()
        } else {
            format!("{base}-{number}")
        };
        if profiles
            .get(&name)
            .is_none_or(|profile| compatible(profile, backend, kind, account))
        {
            return name;
        }
    }
    unreachable!()
}

pub fn available_port(port: u16) -> Result<bool, AppError> {
    let Ok(_ipv4) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
        return Ok(false);
    };
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(_ipv6) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) => {}
        Err(_) => return Ok(false),
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if crate::desktop::browser::has_saved_data(port)? {
        return Ok(false);
    }
    Ok(true)
}

pub fn run(
    mut args: InitArgs,
    requested: Option<&str>,
    profiles: &BTreeMap<String, Profile>,
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut available: impl FnMut(u16) -> Result<bool, AppError>,
) -> Result<Option<Plan>, AppError> {
    writeln!(
        output,
        "\nTeams\n\nConnect your account. Enter q at any prompt to cancel.\n"
    )?;
    writeln!(
        output,
        "  1  Work or school\n  2  Personal\n  3  Other connection options\n"
    )?;
    let Some(account) = choice(input, output, "Account", "1", &["1", "2", "3"])? else {
        return Ok(None);
    };
    let mut backend = BackendKind::Desktop;
    let mut kind = if cfg!(any(target_os = "macos", target_os = "linux")) {
        if account == "2" {
            DesktopClientKind::BrowserCore
        } else {
            DesktopClientKind::Browser
        }
    } else {
        DesktopClientKind::Native
    };
    let mut account = if account == "2" {
        DesktopAccount::Personal
    } else if account == "1" {
        DesktopAccount::WorkSchool
    } else {
        writeln!(
            output,
            "\n  1  Microsoft Graph — work/school API, including teams and channels\n  2  Teams Desktop — use the account signed in to the app\n  3  Full browser — personal account\n"
        )?;
        let Some(method) = choice(input, output, "Connection", "1", &["1", "2", "3"])? else {
            return Ok(None);
        };
        match method.as_str() {
            "1" => {
                backend = BackendKind::Graph;
                DesktopAccount::WorkSchool
            }
            "2" => {
                kind = DesktopClientKind::Native;
                writeln!(output, "\n  1  Work or school\n  2  Personal\n")?;
                let Some(account) =
                    choice(input, output, "Teams Desktop account", "1", &["1", "2"])?
                else {
                    return Ok(None);
                };
                if account == "1" {
                    DesktopAccount::WorkSchool
                } else {
                    DesktopAccount::Personal
                }
            }
            _ => {
                if !cfg!(any(target_os = "macos", target_os = "linux")) {
                    return Err(AppError::Unsupported("The browser client needs macOS, Linux or WSL2. Run `teams init` inside WSL2, or choose Teams Desktop.".into()));
                }
                kind = DesktopClientKind::Browser;
                DesktopAccount::Personal
            }
        }
    };
    if backend == BackendKind::Graph {
        kind = DesktopClientKind::Native;
        account = DesktopAccount::Auto;
    }
    let name = profile_name(requested, profiles, backend, kind, account);
    let existing = profiles.get(&name);
    if existing.is_some_and(|profile| !compatible(profile, backend, kind, account))
        && !confirm(
            input,
            output,
            &format!(
                "Replace the connection settings for profile '{}'",
                name.escape_default()
            ),
        )?
    {
        return Ok(None);
    }
    args.backend = Some(backend);
    if backend == BackendKind::Desktop {
        args.desktop_client = Some(kind);
        args.desktop_account = Some(account);
        args.desktop_port = Some(
            if let Some(profile) = existing.filter(|p| compatible(p, backend, kind, account)) {
                profile.desktop_port
            } else if kind.is_browser() {
                let preferred = if account == DesktopAccount::WorkSchool {
                    9224
                } else if kind == DesktopClientKind::BrowserCore {
                    9223
                } else {
                    9225
                };
                let mut candidates = std::iter::once(preferred).chain(19230..19330);
                loop {
                    let Some(port) = candidates.next() else {
                        return Err(AppError::Desktop("No free browser port was found. Use advanced setup with --desktop-port.".into()));
                    };
                    if !profiles.values().any(|profile| {
                        profile.backend == BackendKind::Desktop && profile.desktop_port == port
                    }) && available(port)?
                    {
                        break port;
                    }
                }
            } else {
                crate::config::DEFAULT_DESKTOP_PORT
            },
        );
        if !args.read_only && !args.allow_send {
            writeln!(output, "\n  1  Read and send messages\n  2  Read only\n")?;
            let Some(access) = choice(input, output, "Message access", "2", &["1", "2"])? else {
                return Ok(None);
            };
            args.allow_send = access == "1";
        }
    }
    let browser_confirmed = backend == BackendKind::Desktop && kind.is_browser();
    if browser_confirmed {
        writeln!(
            output,
            "\nA separate Teams browser will open for sign-in. After setup, `teams` reopens\nyour saved session and inbox. Your usual browser and Teams app stay separate.\nSome organisations need the browser window to stay open.\n\nWhile connected, other programs running as you can access this Teams session,\nincluding reading and sending messages. `teams desktop disconnect` closes it.\n"
        )?;
        if !confirm(
            input,
            output,
            "Open Teams and enable this startup behaviour",
        )? {
            return Ok(None);
        }
        args.auto_start = true;
    } else if backend == BackendKind::Desktop {
        writeln!(output, "\nUsing the account signed in to Teams Desktop.\n")?;
        #[cfg(target_os = "windows")]
        writeln!(output, "For browser mode, run `teams init` inside WSL2.\n")?;
    }
    Ok(Some(Plan {
        name,
        args,
        browser_confirmed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(flags: &[&str]) -> InitArgs {
        let cli = Cli::try_parse_from(["teams", "init"].into_iter().chain(flags.iter().copied()))
            .unwrap();
        let Some(Command::Init(args)) = cli.command else {
            panic!("init")
        };
        args
    }

    fn profile(kind: DesktopClientKind, account: DesktopAccount, port: u16) -> Profile {
        Profile {
            backend: BackendKind::Desktop,
            client_id: String::new(),
            tenant: String::new(),
            read_only: true,
            allow_send: false,
            desktop_port: port,
            desktop_client: kind,
            desktop_account: account,
            browser_auto_start: false,
        }
    }

    #[test]
    fn explicit_connection_flags_and_offline_setup_skip_the_wizard() {
        assert!(offered(&args(&[])));
        assert!(offered(&args(&["--read-only"])));
        for flags in [
            vec!["--backend", "graph"],
            vec!["--backend", "desktop"],
            vec!["--desktop-client", "browser"],
            vec!["--client-id", "example"],
            vec!["--no-login"],
            vec!["--device-code"],
            vec!["--tenant", "organizations"],
        ] {
            assert!(!offered(&args(&flags)), "{flags:?}");
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn work_setup_chooses_its_browser_and_avoids_reserved_ports_and_old_data() {
        let profiles = BTreeMap::from([(
            "existing".into(),
            profile(DesktopClientKind::Native, DesktopAccount::Personal, 9224),
        )]);
        let mut checked = vec![];
        let mut output = vec![];
        let plan = run(
            args(&[]),
            None,
            &profiles,
            &mut "1\n2\ny\n".as_bytes(),
            &mut output,
            |port| {
                checked.push(port);
                Ok(port != 19230)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.name, "work");
        assert_eq!(plan.args.desktop_account, Some(DesktopAccount::WorkSchool));
        assert_eq!(plan.args.desktop_client, Some(DesktopClientKind::Browser));
        assert_eq!(plan.args.desktop_port, Some(19231));
        assert!(!plan.args.allow_send);
        assert!(plan.args.auto_start && plan.browser_confirmed);
        assert_eq!(checked, [19230, 19231]);
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("including reading and sending"));
        assert!(!text.contains("--desktop-"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn personal_setup_requires_explicit_send_choice_and_reuses_compatible_profile() {
        let profiles = BTreeMap::from([(
            "personal".into(),
            profile(
                DesktopClientKind::BrowserCore,
                DesktopAccount::Personal,
                19300,
            ),
        )]);
        let plan = run(
            args(&[]),
            None,
            &profiles,
            &mut "2\n1\nyes\n".as_bytes(),
            &mut vec![],
            |_| panic!("must reuse existing browser profile"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.name, "personal");
        assert_eq!(plan.args.desktop_port, Some(19300));
        assert!(plan.args.allow_send);
        let plan = run(
            args(&["--read-only"]),
            None,
            &profiles,
            &mut "2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert!(plan.args.read_only && !plan.args.allow_send);
    }

    #[test]
    fn cancelled_or_incomplete_setup_returns_no_plan() {
        for input in ["", "q\n", "1\nq\n", "1\n2\nn\n", "1\n2\n"] {
            let result = run(
                args(&[]),
                None,
                &BTreeMap::new(),
                &mut input.as_bytes(),
                &mut vec![],
                |_| Ok(true),
            )
            .unwrap();
            if cfg!(any(target_os = "macos", target_os = "linux"))
                || matches!(input, "" | "q\n" | "1\nq\n")
            {
                assert!(result.is_none(), "{input:?}");
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn unrelated_profiles_are_preserved_and_explicit_replacement_needs_confirmation() {
        let profiles = BTreeMap::from([(
            "work".into(),
            profile(DesktopClientKind::Native, DesktopAccount::WorkSchool, 9222),
        )]);
        let plan = run(
            args(&[]),
            None,
            &profiles,
            &mut "1\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.name, "work-2");
        assert!(
            run(
                args(&[]),
                Some("work"),
                &profiles,
                &mut "1\nn\n".as_bytes(),
                &mut vec![],
                |_| Ok(true)
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn graph_is_an_explicit_advanced_choice_and_does_not_reserve_a_browser_port() {
        let plan = run(
            args(&[]),
            None,
            &BTreeMap::new(),
            &mut "3\n1\n".as_bytes(),
            &mut vec![],
            |_| panic!("Graph has no browser port"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.backend, Some(BackendKind::Graph));
        assert!(!plan.args.auto_start && !plan.browser_confirmed);
    }
}
