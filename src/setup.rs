//! Interactive setup selects user intent; explicit CLI flags keep their existing contract.
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

use crate::cli::InitArgs;
use crate::config::{BackendKind, DesktopAccount, DesktopClientKind, Profile};
use crate::error::AppError;

mod capabilities;
pub use capabilities::{Availability, Capabilities, graph_device_code_needed};

pub struct Plan {
    pub name: String,
    pub args: InitArgs,
    pub browser_confirmed: bool,
    pub forget_pending: Option<crate::config::PendingBrowserSetup>,
}

pub fn offered(args: &InitArgs) -> bool {
    args.backend.is_none()
        && args.desktop_port.is_none()
        && args.desktop_account.is_none()
        && args.desktop_client.is_none()
        && args.credential_store.is_none()
        && args.pass_store_dir.is_none()
        && args.client_id.is_none()
        && args.tenant.is_none()
        && !args.no_login
        && !args.device_code
        && !args.channel_history
        && !args.auto_start
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

/// Offer an initialized pass store only for new Graph profiles. Existing choices
/// are preserved by the caller, including when their helper is unavailable.
pub fn choose_credential_store(
    native_label: &str,
    native_error: Option<&str>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Option<crate::config::CredentialStore>, AppError> {
    writeln!(output, "\nProtect your sign-in")?;
    if let Some(reason) = native_error {
        writeln!(
            output,
            "  {native_label} is unavailable: {reason}\n  2  pass — your GPG-encrypted password store (recommended)\n"
        )?;
    } else {
        writeln!(
            output,
            "  1  {native_label} (recommended)\n  2  pass — your GPG-encrypted password store\n"
        )?;
    }
    writeln!(
        output,
        "pass may ask you to unlock your GPG key during sign-in or later reads."
    )?;
    let (default, allowed): (&str, &[&str]) = if native_error.is_some() {
        ("2", &["2"])
    } else {
        ("1", &["1", "2"])
    };
    Ok(
        choice(input, output, "Credential store", default, allowed)?.map(|answer| {
            if answer == "2" {
                crate::config::CredentialStore::Pass
            } else {
                crate::config::CredentialStore::Native
            }
        }),
    )
}

pub fn read_only_fallback(
    reason: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, AppError> {
    writeln!(
        output,
        "\nYour chats are readable, but sending is unavailable.\n{reason}\n"
    )?;
    confirm(input, output, "Continue with read-only access")
}

/// Unavailable choices have no selectable number. Typed disabled keys explain why.
fn menu(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
    items: &[(&str, &str, Option<&str>)],
) -> Result<Option<String>, AppError> {
    writeln!(output)?;
    for (key, label, blocked) in items {
        match blocked {
            None => writeln!(output, "  {key}  {label}")?,
            Some(reason) => writeln!(output, "     {label} — unavailable\n     {reason}")?,
        }
    }
    writeln!(output)?;
    let Some((default, _, _)) = items.iter().find(|(_, _, blocked)| blocked.is_none()) else {
        return Err(AppError::Unsupported("No connection is available with these options. Resolve the requirements above and run `teams init` again.".into()));
    };
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
        let answer = if answer.is_empty() { *default } else { &answer };
        if let Some((key, label, blocked)) = items.iter().find(|(key, _, _)| *key == answer) {
            if let Some(reason) = blocked {
                writeln!(output, "{label} is unavailable: {reason}")?;
            } else {
                return Ok(Some((*key).into()));
            }
        } else {
            writeln!(
                output,
                "Enter one of the available numbers, or q to cancel."
            )?;
        }
    }
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
    if !profiles.contains_key(base)
        && let Some((name, _)) = profiles
            .iter()
            .find(|(_, profile)| compatible(profile, backend, kind, account))
    {
        return name.clone();
    }
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

fn fresh_name(
    base: &str,
    profiles: &BTreeMap<String, Profile>,
    pending: &[crate::config::PendingBrowserSetup],
) -> String {
    for number in 1.. {
        let name = if number == 1 {
            base.into()
        } else {
            format!("{base}-{number}")
        };
        if !profiles.contains_key(&name) && !pending.iter().any(|record| record.name == name) {
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
    capabilities: &Capabilities,
    requested: Option<&str>,
    profiles: &BTreeMap<String, Profile>,
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut available: impl FnMut(u16) -> Result<bool, AppError>,
) -> Result<Option<Plan>, AppError> {
    writeln!(
        output,
        "\nTeams\n\nConnect your account. Enter q at any prompt to cancel."
    )?;
    let graph_blocked = args.allow_send.then_some("--allow-send applies to Teams Desktop and browser connections. Run `teams init` without this flag to use Microsoft Graph.")
        .or_else(|| capabilities.graph.reason());
    let work_blocked = if capabilities.preferred(false).is_none() {
        graph_blocked
    } else {
        None
    };
    let personal_blocked = capabilities.preferred(true).is_none().then_some(
        "Personal needs a Teams browser or Teams Desktop. See Other connection options for requirements.");
    let Some(account) = menu(
        input,
        output,
        "Account",
        &[
            ("1", "Work or school", work_blocked),
            ("2", "Personal", personal_blocked),
            ("3", "Other connection options", None),
        ],
    )?
    else {
        return Ok(None);
    };
    let mut backend = BackendKind::Desktop;
    let mut kind = capabilities.preferred(account == "2").unwrap_or_else(|| {
        backend = BackendKind::Graph;
        DesktopClientKind::Native
    });
    let mut account = if account == "2" {
        DesktopAccount::Personal
    } else if account == "1" {
        DesktopAccount::WorkSchool
    } else {
        let Some(method) = menu(
            input,
            output,
            "Connection",
            &[
                (
                    "1",
                    "Microsoft Graph — work/school, including teams and channels",
                    graph_blocked,
                ),
                (
                    "2",
                    "Teams Desktop — use the account signed in to the app",
                    capabilities.native.reason(),
                ),
                (
                    "3",
                    "Full browser — personal account",
                    capabilities.browser.reason(),
                ),
                (
                    "4",
                    "Lightweight browser — personal account (experimental)",
                    capabilities.core.reason(),
                ),
            ],
        )?
        else {
            return Ok(None);
        };
        match method.as_str() {
            "1" => {
                backend = BackendKind::Graph;
                DesktopAccount::WorkSchool
            }
            "2" => {
                backend = BackendKind::Desktop;
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
                backend = BackendKind::Desktop;
                kind = if method == "4" {
                    DesktopClientKind::BrowserCore
                } else {
                    DesktopClientKind::Browser
                };
                DesktopAccount::Personal
            }
        }
    };
    if backend == BackendKind::Graph {
        kind = DesktopClientKind::Native;
        account = DesktopAccount::Auto;
        args.device_code = capabilities.graph_device_code;
        writeln!(output, "\nUsing Microsoft Graph for work or school.")?;
        if args.device_code {
            writeln!(
                output,
                "Sign in with a code on another device; no local browser is needed."
            )?;
        }
    } else {
        writeln!(
            output,
            "\nUsing {}.",
            match kind {
                DesktopClientKind::Browser => "a separate Teams browser",
                DesktopClientKind::BrowserCore => "the lightweight Teams browser (experimental)",
                DesktopClientKind::Native => "Teams Desktop",
            }
        )?;
        if account == DesktopAccount::Personal
            && kind != DesktopClientKind::BrowserCore
            && let Some(reason) = capabilities.core.reason()
        {
            writeln!(output, "Lightweight browser unavailable: {reason}")?;
        }
        if matches!(
            capabilities.availability(kind),
            Availability::Ready { download: true }
        ) {
            writeln!(
                output,
                "A matching browser pair will download from Google after you confirm.\nAllow a few GiB of free space for installation and saved browser data."
            )?;
        }
    }
    let base = if backend == BackendKind::Graph {
        "graph"
    } else if account == DesktopAccount::WorkSchool {
        "work"
    } else {
        "personal"
    };
    let mut name = profile_name(requested, profiles, backend, kind, account);
    if profiles
        .get(&name)
        .is_some_and(|profile| compatible(profile, backend, kind, account))
    {
        if requested.is_some() {
            if !confirm(
                input,
                output,
                &format!(
                    "{} '{}'",
                    if backend == BackendKind::Graph {
                        "Sign in again and update"
                    } else {
                        "Update connection using the saved sign-in for"
                    },
                    name.escape_default(),
                ),
            )? {
                return Ok(None);
            }
        } else {
            let update = if backend == BackendKind::Graph {
                format!("Sign in again to update '{}'", name.escape_default())
            } else {
                format!("Update '{}' using its saved sign-in", name.escape_default())
            };
            let new = if backend == BackendKind::Desktop && kind == DesktopClientKind::Native {
                "Create a separate profile for Teams Desktop"
            } else {
                "Connect another account"
            };
            let Some(action) = menu(
                input,
                output,
                "Connection",
                &[("1", new, None), ("2", &update, None)],
            )?
            else {
                return Ok(None);
            };
            if action == "1" {
                name = fresh_name(base, profiles, &capabilities.pending);
            }
        }
    }
    let mut pending_port = None;
    let mut forget_pending = None;
    if backend == BackendKind::Desktop && kind.is_browser() && !profiles.contains_key(&name) {
        let matches: Vec<_> = capabilities
            .pending
            .iter()
            .filter(|record| {
                requested.is_none_or(|_| record.name == name)
                    && !profiles.contains_key(&record.name)
                    && record.account == account
                    && record.client == kind
            })
            .collect();
        if requested.is_some() && matches.len() > 1 {
            return Err(AppError::Desktop("More than one unfinished setup matches this connection. Use `teams init --profile NEW_NAME` for a separate sign-in; existing browsers were preserved.".into()));
        }
        let pending = if matches.len() > 1 {
            let mut choices: Vec<_> = matches
                .iter()
                .enumerate()
                .map(|(i, record)| {
                    (
                        (i + 1).to_string(),
                        format!(
                            "Continue unfinished '{}' (port {})",
                            record.name.escape_default(),
                            record.port
                        ),
                    )
                })
                .collect();
            choices.push((
                (choices.len() + 1).to_string(),
                "Start a new sign-in".into(),
            ));
            let items: Vec<_> = choices
                .iter()
                .map(|(key, label)| (key.as_str(), label.as_str(), None))
                .collect();
            let Some(selected) = menu(input, output, "Saved sign-ins", &items)? else {
                return Ok(None);
            };
            matches.get(selected.parse::<usize>().unwrap() - 1).copied()
        } else {
            matches.first().copied()
        };
        if let Some(pending) = pending {
            name = pending.name.clone();
            writeln!(
                output,
                "\nUnfinished connection: '{}'",
                name.escape_default()
            )?;
            let Some(action) = menu(
                input,
                output,
                "Unfinished setup",
                &[
                    ("1", "Resume saved sign-in", None),
                    (
                        "2",
                        "Connect another account",
                        requested.map(
                            |_| "Use `teams init --profile NEW_NAME` for a separate connection.",
                        ),
                    ),
                    ("3", "Forget this unfinished setup and start again", None),
                ],
            )?
            else {
                return Ok(None);
            };
            if action == "1" {
                pending_port = Some(pending.port);
            } else if action == "3" {
                writeln!(
                    output,
                    "Only the unfinished-setup record will be removed after you confirm browser startup. Its browser data and saved sign-in will be kept."
                )?;
                if !confirm(input, output, "Forget this unfinished setup")? {
                    return Ok(None);
                }
                forget_pending = Some(pending.clone());
            } else if requested.is_some() {
                return Ok(None);
            } else {
                name = fresh_name(base, profiles, &capabilities.pending);
            }
        }
    }
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
    if backend == BackendKind::Graph
        && let Some(existing) = existing.filter(|profile| profile.backend == BackendKind::Graph)
    {
        args.client_id = Some(existing.client_id.clone());
        args.tenant = Some(existing.tenant.clone());
    }
    if backend == BackendKind::Desktop {
        args.desktop_client = Some(kind);
        args.desktop_account = Some(account);
        args.desktop_port = Some(
            if let Some(profile) = existing.filter(|p| compatible(p, backend, kind, account)) {
                profile.desktop_port
            } else if let Some(port) = pending_port {
                port
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
                    }) && !capabilities
                        .pending
                        .iter()
                        .any(|record| record.port == port)
                        && available(port)?
                    {
                        break port;
                    }
                }
            } else {
                crate::config::DEFAULT_DESKTOP_PORT
            },
        );
    }
    if !args.read_only && !args.allow_send {
        writeln!(output, "\n  1  Read and send messages\n  2  Read only\n")?;
        let Some(access) = choice(input, output, "Message access", "2", &["1", "2"])? else {
            return Ok(None);
        };
        args.read_only = access == "2";
        args.allow_send = access == "1" && backend == BackendKind::Desktop;
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
        forget_pending,
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

    fn capabilities() -> Capabilities {
        Capabilities {
            graph: Availability::Ready { download: false },
            pending: Vec::new(),
            browser: Availability::Ready { download: false },
            core: Availability::Ready { download: false },
            native: Availability::Ready { download: false },
            graph_device_code: false,
        }
    }

    fn profile(kind: DesktopClientKind, account: DesktopAccount, port: u16) -> Profile {
        Profile {
            credential_store: Default::default(),
            pass_store_dir: None,
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
    fn credential_choice_defaults_to_native_and_cancel_never_selects_a_store() {
        use crate::config::CredentialStore;
        for (answer, expected) in [
            ("\n", Some(CredentialStore::Native)),
            ("2\n", Some(CredentialStore::Pass)),
            ("q\n", None),
            ("", None),
        ] {
            let mut output = Vec::new();
            let selected = super::choose_credential_store(
                "Windows DPAPI",
                None,
                &mut std::io::Cursor::new(answer),
                &mut output,
            )
            .unwrap();
            assert_eq!(selected, expected);
            assert!(
                String::from_utf8(output)
                    .unwrap()
                    .contains("unlock your GPG key")
            );
        }
    }

    #[test]
    fn unavailable_native_storage_cannot_be_selected() {
        let mut output = Vec::new();
        let selected = super::choose_credential_store(
            "Linux Secret Service",
            Some("no D-Bus session"),
            &mut std::io::Cursor::new("1\n\n"),
            &mut output,
        )
        .unwrap();
        assert_eq!(selected, Some(crate::config::CredentialStore::Pass));
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("no D-Bus session")
        );
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
            &capabilities(),
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
            &capabilities(),
            None,
            &profiles,
            &mut "2\n2\n1\nyes\n".as_bytes(),
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
            &capabilities(),
            None,
            &profiles,
            &mut "2\n2\ny\n".as_bytes(),
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
                &capabilities(),
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

    #[test]
    fn unrelated_profiles_are_preserved_and_explicit_replacement_needs_confirmation() {
        let profiles = BTreeMap::from([(
            "work".into(),
            profile(DesktopClientKind::Native, DesktopAccount::WorkSchool, 9222),
        )]);
        let plan = run(
            args(&[]),
            &capabilities(),
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
                &capabilities(),
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
            &capabilities(),
            None,
            &BTreeMap::new(),
            &mut "3\n1\n2\n".as_bytes(),
            &mut vec![],
            |_| panic!("Graph has no browser port"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.backend, Some(BackendKind::Graph));
        assert!(!plan.args.auto_start && !plan.browser_confirmed);
    }

    #[test]
    fn windows_options_cannot_select_a_browser_even_by_typing_its_hidden_number() {
        let mut caps = capabilities();
        caps.browser = Availability::Blocked("Run inside WSL2 for browser mode.".into());
        caps.core = caps.browser.clone();
        let mut output = vec![];
        let plan = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "3\n3\n4\n2\n2\n2\n".as_bytes(),
            &mut output,
            |_| panic!("native setup does not allocate browser ports"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.desktop_client, Some(DesktopClientKind::Native));
        assert_eq!(plan.args.desktop_account, Some(DesktopAccount::Personal));
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("  3  Full browser"));
        assert!(!text.contains("  4  Lightweight browser"));
        assert!(text.contains("Full browser — personal account is unavailable"));
        assert!(text.contains("Run inside WSL2"));
    }

    #[test]
    fn headless_linux_excludes_personal_and_native_and_offers_graph_device_code() {
        let caps = Capabilities {
            graph: Availability::Ready { download: false },
            pending: Vec::new(),
            browser: Availability::Blocked("Enable a graphical display.".into()),
            core: Availability::Blocked("Enable a graphical display.".into()),
            native: Availability::Blocked("Teams Desktop needs Windows interop.".into()),
            graph_device_code: true,
        };
        let mut output = vec![];
        let plan = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "2\n1\n2\n".as_bytes(),
            &mut output,
            |_| panic!("no browser"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.backend, Some(BackendKind::Graph));
        assert!(plan.args.device_code);
        assert!(plan.args.read_only && !plan.args.allow_send);
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("  2  Personal"));
        assert!(text.contains("Personal is unavailable"));
        assert!(text.contains("no local browser is needed"));
        assert!(
            run(
                args(&["--allow-send"]),
                &caps,
                None,
                &BTreeMap::new(),
                &mut "3\n".as_bytes(),
                &mut vec![],
                |_| panic!("no browser")
            )
            .is_err()
        );
    }

    #[test]
    fn personal_falls_back_to_full_browser_when_core_has_no_compatible_download() {
        let mut caps = capabilities();
        caps.core =
            Availability::Blocked("No matching browser pair exists for this platform.".into());
        let plan = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "2\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.desktop_client, Some(DesktopClientKind::Browser));
        assert_eq!(plan.args.desktop_account, Some(DesktopAccount::Personal));
        assert!(plan.args.auto_start);
    }

    #[test]
    fn graph_cannot_be_selected_with_an_explicit_desktop_send_flag() {
        let mut output = vec![];
        assert!(
            run(
                args(&["--allow-send"]),
                &capabilities(),
                None,
                &BTreeMap::new(),
                &mut "3\n1\nq\n".as_bytes(),
                &mut output,
                |_| Ok(true)
            )
            .unwrap()
            .is_none()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("  1  Microsoft Graph"));
        assert!(text.contains("--allow-send applies to"));
    }

    #[test]
    fn conflicting_graph_environment_cannot_be_selected_but_desktop_still_works() {
        let mut caps = capabilities();
        caps.graph =
            Availability::Blocked("Unset TEAMS_ACCESS_TOKEN before guided Graph setup.".into());
        let mut output = vec![];
        let plan = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "3\n1\n2\n1\n2\n".as_bytes(),
            &mut output,
            |_| panic!("native has no browser port"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.args.backend, Some(BackendKind::Desktop));
        assert_eq!(plan.args.desktop_client, Some(DesktopClientKind::Native));
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("  1  Microsoft Graph"));
        assert!(text.contains(
            "Microsoft Graph — work/school, including teams and channels is unavailable"
        ));
        assert!(text.contains("Unset TEAMS_ACCESS_TOKEN"));
    }

    #[test]
    fn lightweight_download_is_disclosed_before_consent() {
        let mut caps = capabilities();
        caps.core = Availability::Ready { download: true };
        let mut output = vec![];
        assert!(
            run(
                args(&[]),
                &caps,
                None,
                &BTreeMap::new(),
                &mut "2\n2\nn\n".as_bytes(),
                &mut output,
                |_| Ok(true)
            )
            .unwrap()
            .is_none()
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("download from Google after you confirm"));
    }

    #[test]
    fn existing_connections_require_a_choice_and_graph_updates_preserve_registration() {
        let mut graph = profile(DesktopClientKind::Native, DesktopAccount::Auto, 9222);
        graph.backend = BackendKind::Graph;
        graph.client_id = "custom-application".into();
        graph.tenant = "custom-tenant".into();
        let profiles = BTreeMap::from([("graph".into(), graph)]);
        let fresh = run(
            args(&[]),
            &capabilities(),
            None,
            &profiles,
            &mut "3\n1\n1\n2\n".as_bytes(),
            &mut vec![],
            |_| panic!(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(fresh.name, "graph-2");
        let update = run(
            args(&[]),
            &capabilities(),
            Some("graph"),
            &profiles,
            &mut "3\n1\ny\n2\n".as_bytes(),
            &mut vec![],
            |_| panic!(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(update.args.client_id.as_deref(), Some("custom-application"));
        assert_eq!(update.args.tenant.as_deref(), Some("custom-tenant"));
        assert!(
            run(
                args(&[]),
                &capabilities(),
                Some("graph"),
                &profiles,
                &mut "3\n1\nn\n".as_bytes(),
                &mut vec![],
                |_| panic!()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn unfinished_browser_setup_can_resume_its_exact_port_or_keep_a_separate_account() {
        let mut caps = capabilities();
        caps.pending.push(crate::config::PendingBrowserSetup {
            name: "work".into(),
            port: 19311,
            account: DesktopAccount::WorkSchool,
            client: DesktopClientKind::Browser,
        });
        let resumed = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "1\n1\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| panic!("reuse the exact saved port"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resumed.args.desktop_port, Some(19311));
        let separate = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "1\n2\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(separate.name, "work-2");
        assert_ne!(separate.args.desktop_port, Some(19311));
        assert!(separate.forget_pending.is_none());
        let forgotten = run(
            args(&[]),
            &caps,
            None,
            &BTreeMap::new(),
            &mut "1\n3\ny\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(forgotten.name, "work");
        assert_eq!(forgotten.forget_pending.as_ref(), caps.pending.first());
        assert_ne!(forgotten.args.desktop_port, Some(19311));
        assert!(
            run(
                args(&[]),
                &caps,
                None,
                &BTreeMap::new(),
                &mut "1\n3\ny\n2\nn\n".as_bytes(),
                &mut vec![],
                |_| Ok(true),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn existing_nondefault_names_are_reachable_and_new_names_skip_pending_accounts() {
        let existing = profile(
            DesktopClientKind::Browser,
            DesktopAccount::WorkSchool,
            19312,
        );
        let profiles = BTreeMap::from([("work-2".into(), existing)]);
        let mut caps = capabilities();
        caps.pending.push(crate::config::PendingBrowserSetup {
            name: "work".into(),
            port: 19311,
            account: DesktopAccount::WorkSchool,
            client: DesktopClientKind::Browser,
        });
        let update = run(
            args(&[]),
            &caps,
            None,
            &profiles,
            &mut "1\n2\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| panic!(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(update.name, "work-2");
        assert_eq!(update.args.desktop_port, Some(19312));
        let new = run(
            args(&[]),
            &caps,
            None,
            &profiles,
            &mut "1\n1\n2\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(new.name, "work-3");
        assert!(![19311, 19312].contains(&new.args.desktop_port.unwrap()));
    }

    #[test]
    fn connecting_another_account_offers_all_matching_unfinished_sign_ins() {
        let profiles = BTreeMap::from([(
            "work".into(),
            profile(
                DesktopClientKind::Browser,
                DesktopAccount::WorkSchool,
                19310,
            ),
        )]);
        let mut caps = capabilities();
        for (name, port) in [("work-2", 19311), ("work-3", 19312)] {
            caps.pending.push(crate::config::PendingBrowserSetup {
                name: name.into(),
                port,
                account: DesktopAccount::WorkSchool,
                client: DesktopClientKind::Browser,
            });
        }
        let mut output = vec![];
        let resumed = run(
            args(&[]),
            &caps,
            None,
            &profiles,
            &mut "1\n1\n2\n1\n2\ny\n".as_bytes(),
            &mut output,
            |_| panic!("resume the saved port"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resumed.name, "work-3");
        assert_eq!(resumed.args.desktop_port, Some(19312));
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Continue unfinished 'work-2' (port 19311)"));
        assert!(text.contains("Continue unfinished 'work-3' (port 19312)"));
        let new = run(
            args(&[]),
            &caps,
            None,
            &profiles,
            &mut "1\n1\n3\n2\ny\n".as_bytes(),
            &mut vec![],
            |_| Ok(true),
        )
        .unwrap()
        .unwrap();
        assert_eq!(new.name, "work-4");
        assert!(![19310, 19311, 19312].contains(&new.args.desktop_port.unwrap()));
    }

    #[test]
    fn unavailable_send_interface_needs_explicit_consent_to_continue_read_only() {
        for (input, expected) in [
            ("y\n", true),
            ("yes\n", true),
            ("\n", false),
            ("n\n", false),
            ("q\n", false),
            ("", false),
        ] {
            let mut output = vec![];
            assert_eq!(
                read_only_fallback(
                    "No send client in this account.",
                    &mut input.as_bytes(),
                    &mut output
                )
                .unwrap(),
                expected
            );
            assert!(
                String::from_utf8(output)
                    .unwrap()
                    .contains("Continue with read-only access [n]")
            );
        }
    }
}
