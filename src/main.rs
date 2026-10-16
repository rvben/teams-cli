use std::io::{self, IsTerminal, Read};

use clap::{CommandFactory, Parser};
use serde::Serialize;
use serde_json::Value;

use teams_cli::auth;
use teams_cli::backend::TeamsBackend;
use teams_cli::cli::{
    AuthCommand, ChannelsCommand, ChatsCommand, Cli, Command, ConfigCommand, DesktopCommand,
    InitArgs, MessagesCommand, OffsetPageArgs, PageArgs, PeopleCommand, ProfileCommand, TuiArgs,
};
use teams_cli::config::{self, BackendKind, Profile};
use teams_cli::desktop::{self, DesktopClient};
use teams_cli::error::AppError;
use teams_cli::graph::{self, Page};
use teams_cli::output::{Output, OutputFormat, print_error, structured_from_args};
use teams_cli::{render, schema, setup, tui};

#[tokio::main]
async fn main() {
    let structured = structured_from_args();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                error.exit();
            }
            if structured {
                let app_error = AppError::InvalidInput(error.to_string());
                print_error(&app_error, true);
                std::process::exit(app_error.contract().exit_code);
            }
            error.exit();
        }
    };
    let out = Output {
        format: if cli.json && cli.output == OutputFormat::Auto {
            OutputFormat::Json
        } else {
            cli.output
        },
        quiet: cli.quiet,
    };
    if let Err(error) = dispatch(cli, out).await {
        print_error(&error, out.json());
        std::process::exit(error.contract().exit_code);
    }
}

async fn dispatch(cli: Cli, out: Output) -> Result<(), AppError> {
    let profile_arg = cli.profile.as_deref();
    let yes = cli.yes;
    let command = match cli.command {
        Some(command) => command,
        None if io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && !out.json()
            && config::connection_profile(profile_arg)?.is_none() =>
        {
            Command::Init(default_init_args()?)
        }
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => Command::Tui(TuiArgs {
            demo: false,
            snapshot: false,
            width: 100,
            height: 30,
        }),
        None => {
            return Err(AppError::NonInteractive(
                "no command was provided; run `teams --help` or `teams schema`".into(),
            ));
        }
    };
    if desktop_unsupported(&command)
        && config::selected_backend(profile_arg)? == BackendKind::Desktop
    {
        return Err(desktop::messaging_unsupported());
    }
    if matches!(
        command,
        Command::Auth {
            command: AuthCommand::Login { .. } | AuthCommand::Logout
        }
    ) && config::selected_backend(profile_arg)? == BackendKind::Desktop
    {
        return Err(AppError::Unsupported("desktop authentication is managed in Teams; sign in or out in the app. Use `teams auth status` for connection diagnostics".into()));
    }
    match command {
        Command::DesktopHelper => desktop::serve_helper().await,
        Command::Desktop { command } => desktop_connection(profile_arg, command, yes, out).await,
        Command::Init(args) => {
            if setup::offered(&args)
                && !yes
                && !out.quiet
                && !out.json()
                && io::stdin().is_terminal()
                && io::stdout().is_terminal()
                && io::stderr().is_terminal()
            {
                guided_init(profile_arg, args, out).await
            } else {
                init(profile_arg.unwrap_or("default"), args, yes, out, false).await
            }
        }
        Command::Auth { command } => auth_command(profile_arg, command, out).await,
        Command::Profile { command } => profile_command(command, yes, out),
        Command::Config { command } => config_command(profile_arg, command, out),
        Command::Whoami => {
            let client = client(profile_arg).await?;
            let value = client.me().await?;
            out.value(&value, || identity_text(&value))
        }
        Command::List(page) => {
            let client = client(profile_arg).await?;
            let mut result = client.teams(page.limit, page.offset).await?;
            render_offset_page(&mut result, &page, out)
        }
        Command::Channels {
            command: ChannelsCommand::List { team, page },
        } => {
            let client = client(profile_arg).await?;
            let mut result = client.channels(&team, page.limit, page.offset).await?;
            render_offset_page(&mut result, &page, out)
        }
        Command::Chats {
            command: ChatsCommand::List(page),
        } => {
            let client = client(profile_arg).await?;
            let mut result = client.chats(page.limit, page.cursor.as_deref()).await?;
            render_conversation(&mut result, &page, out, render::chats)
        }
        Command::People {
            command: PeopleCommand::Search { query, limit },
        } => {
            graph::validate_people_query(&query, usize::from(limit))?;
            let result = client(profile_arg)
                .await?
                .search_people(&query, limit)
                .await?;
            out.value(&result, || {
                if result.items.is_empty() {
                    return "No people found. Try a longer name or an exact email address.".into();
                }
                let mut lines = result
                    .items
                    .iter()
                    .map(|person| {
                        let safe = |key: &str| {
                            person[key]
                                .as_str()
                                .unwrap_or_default()
                                .chars()
                                .filter(|ch| !ch.is_control())
                                .collect::<String>()
                        };
                        format!(
                            "{}\n  {}\n  {}",
                            safe("displayName"),
                            safe("email"),
                            safe("id")
                        )
                    })
                    .collect::<Vec<_>>();
                if result.truncated {
                    lines.push("More people match; refine your search.".into());
                }
                lines.join("\n\n")
            })
        }
        Command::Chats {
            command: ChatsCommand::Create { recipient },
        } => {
            let (name, profile) = config::load(profile_arg)?;
            profile.require_writable()?;
            if recipient.trim() != recipient
                || recipient.is_empty()
                || recipient.len() > 512
                || recipient.chars().any(char::is_control)
            {
                return Err(AppError::InvalidInput(
                    "--recipient must be an exact person ID returned by `teams people search`"
                        .into(),
                ));
            }
            confirm_create_chat(yes, &name, &recipient)?;
            let value = client(profile_arg).await?.create_chat(&recipient).await?;
            out.value(&value, || {
                format!(
                    "Conversation ready: {}. No message was sent.",
                    value["id"].as_str().unwrap_or("unknown")
                )
            })
        }
        Command::Messages {
            command:
                MessagesCommand::Status {
                    chat,
                    client_message_id,
                },
        } => {
            desktop::check_chat(&chat)?;
            if !teams_cli::delivery::valid_client_message_id(&client_message_id) {
                return Err(AppError::InvalidInput(
                    "client message ID must contain 18 digits".into(),
                ));
            }
            if config::selected_backend(profile_arg)? != BackendKind::Desktop {
                return Err(AppError::Unsupported(
                    "client message ID delivery checks require a desktop profile".into(),
                ));
            }
            let value = client(profile_arg)
                .await?
                .delivery(&chat, &client_message_id)
                .await?;
            out.value(&value, || format!("Delivery: {}. Checked newest 50 cached messages; an absent result does not prove failure.", value["delivery_state"].as_str().unwrap_or("unknown")))
        }
        Command::Messages {
            command:
                MessagesCommand::List {
                    chat,
                    team,
                    channel,
                    page,
                },
        } => {
            let client = client(profile_arg).await?;
            let mut result = client
                .messages(
                    chat.as_deref(),
                    team.as_deref(),
                    channel.as_deref(),
                    page.limit,
                    page.cursor.as_deref(),
                )
                .await?;
            render_conversation(&mut result, &page, out, render::messages)
        }
        Command::Messages {
            command:
                MessagesCommand::Send {
                    chat,
                    team,
                    channel,
                    body,
                },
        } => {
            let (_, profile) = config::load(profile_arg)?;
            profile.require_writable()?;
            // Desktop sends are refused before reading a body from stdin when they cannot go ahead.
            let desktop_chat = match (profile.backend, chat.as_deref()) {
                (BackendKind::Desktop, Some(chat)) => {
                    desktop::check_chat(chat)?;
                    if !(yes || io::stdin().is_terminal() && io::stderr().is_terminal()) {
                        return Err(AppError::ConfirmationRequired(
                            "desktop sends need confirmation; pass --yes when not running in a terminal"
                                .into(),
                        ));
                    }
                    Some(chat)
                }
                _ => None,
            };
            let body = read_body(&body)?;
            if body.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "message body cannot be empty".into(),
                ));
            }
            if let Some(chat) = desktop_chat
                && !yes
            {
                confirm_desktop_send(chat, &body)?;
            }
            let client = client(profile_arg).await?;
            let value = client
                .send(chat.as_deref(), team.as_deref(), channel.as_deref(), &body)
                .await?;
            out.value(&value, || {
                if value["source"] == "desktop" {
                    format!(
                        "{} Message {} (client ID {}).",
                        teams_cli::delivery::State::from_message(&value).notice(),
                        value["id"].as_str().unwrap_or("unknown"),
                        value["clientMessageId"].as_str().unwrap_or("unknown")
                    )
                } else {
                    format!(
                        "Sent message {}",
                        string_at(&value, "/id").unwrap_or("successfully")
                    )
                }
            })
        }
        Command::Tui(args) => tui_command(profile_arg, args, !out.json()).await,
        Command::Doctor { offline } => doctor(profile_arg, offline, out).await,
        Command::Capabilities => capabilities(out),
        Command::Schema { command } => {
            let value = schema::generate(command.as_deref());
            if let Some(name) = command
                && value["commands"].as_array().is_some_and(Vec::is_empty)
            {
                return Err(AppError::NotFound(format!(
                    "command '{}' is not declared",
                    name
                )));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&value)
                    .map_err(|e| AppError::Unexpected(e.to_string()))?
            );
            Ok(())
        }
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "teams", &mut io::stdout());
            Ok(())
        }
    }
}

/// Commands the desktop backend refuses before any helper or prompt runs.
fn desktop_unsupported(command: &Command) -> bool {
    matches!(
        command,
        Command::Whoami
            | Command::List(_)
            | Command::Channels { .. }
            | Command::Messages {
                command: MessagesCommand::Send { chat: None, .. }
                    | MessagesCommand::Send { team: Some(_), .. }
                    | MessagesCommand::Send {
                        channel: Some(_),
                        ..
                    }
                    | MessagesCommand::List { chat: None, .. }
            }
    )
}

fn default_init_args() -> Result<InitArgs, AppError> {
    // Reuse clap defaults, including TEAMS_READ_ONLY, for the no-argument path.
    match Cli::try_parse_from(["teams", "init"])
        .map_err(|error| AppError::InvalidInput(error.to_string()))?
        .command
    {
        Some(Command::Init(args)) => Ok(args),
        _ => unreachable!("the fixed init command parses as Init"),
    }
}

async fn guided_init(
    profile_arg: Option<&str>,
    args: InitArgs,
    out: Output,
) -> Result<(), AppError> {
    out.note("Checking this computer and available browser downloads…");
    let capabilities = setup::Capabilities::detect().await;
    let plan = setup::run(
        args,
        &capabilities,
        profile_arg,
        &config::profiles()?,
        &mut io::stdin().lock(),
        &mut io::stderr().lock(),
        setup::available_port,
    )?;
    let Some(plan) = plan else {
        out.note("Setup cancelled. Your connection settings were not changed.");
        return Ok(());
    };
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(pending) = &plan.forget_pending {
        desktop::browser_setup::finish(pending.port)?;
        out.note(format!("Forgot the unfinished setup. Its sign-in data was kept. To close its browser: `teams desktop disconnect --desktop-client {} --port {}`.", pending.client.as_str(), pending.port));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = &plan.forget_pending;
    init(&plan.name, plan.args, plan.browser_confirmed, out, true).await?;
    tui_command(
        Some(&plan.name),
        TuiArgs {
            demo: false,
            snapshot: false,
            width: 100,
            height: 30,
        },
        true,
    )
    .await
}

async fn verify_setup_sending(
    client: &DesktopClient,
    profile: &mut Profile,
    guided: bool,
    out: Output,
) -> Result<(), AppError> {
    if !profile.allow_send {
        return Ok(());
    }
    out.note("Checking the send interface without sending a message…");
    if let Err(error) = client.send_check().await {
        if guided
            && setup::read_only_fallback(
                &concise_error(&error),
                &mut io::stdin().lock(),
                &mut io::stderr().lock(),
            )?
        {
            profile.allow_send = false;
            profile.read_only = true;
            return Ok(());
        }
        return Err(error.with_hint(if guided {
            "Choose Read only during setup to use the readable chat history."
        } else {
            "No profile was saved. Re-run the same init command without --allow-send to use read-only access."
        }));
    }
    Ok(())
}

async fn init(
    profile_name: &str,
    args: InitArgs,
    yes: bool,
    out: Output,
    guided: bool,
) -> Result<(), AppError> {
    if args.auto_start
        && (args.backend != Some(BackendKind::Desktop)
            || !args.desktop_client.unwrap_or_default().is_browser())
    {
        return Err(AppError::InvalidInput(
            "--auto-start requires a desktop browser profile".into(),
        ));
    }
    if args.backend == Some(BackendKind::Desktop) {
        if args.client_id.is_some()
            || args.device_code
            || args.channel_history
            || args.tenant.is_some()
            || args.credential_store.is_some()
            || args.pass_store_dir.is_some()
        {
            return Err(AppError::InvalidInput(
                "desktop profiles do not use OAuth or Graph credential-store options".into(),
            ));
        }
        if args.allow_send && args.read_only {
            return Err(AppError::InvalidInput(
                "--allow-send conflicts with --read-only (or TEAMS_READ_ONLY)".into(),
            ));
        }
        let read_only = !args.allow_send;
        // Preserve the Graph store choice even while a profile uses Desktop,
        // so switching the connection back cannot redirect its old credential.
        let previous = config::connection_profile(Some(profile_name))?;
        let mut profile = Profile {
            credential_store: previous
                .as_ref()
                .map(|profile| profile.credential_store)
                .unwrap_or_default(),
            pass_store_dir: previous.and_then(|profile| profile.pass_store_dir),
            backend: BackendKind::Desktop,
            client_id: String::new(),
            tenant: String::new(),
            read_only,
            allow_send: args.allow_send,
            desktop_port: args
                .desktop_port
                .unwrap_or_else(|| args.desktop_client.unwrap_or_default().default_port()),
            desktop_account: args
                .desktop_account
                .unwrap_or_else(|| args.desktop_client.unwrap_or_default().default_account()),
            desktop_client: args.desktop_client.unwrap_or_default(),
            browser_auto_start: args.auto_start,
        };
        profile
            .desktop_client
            .validate_account(profile.desktop_account)?;
        let port = profile.desktop_port;
        let output = |path: &std::path::Path, profile: &Profile, extra: serde_json::Value| {
            let mut value = serde_json::json!({"profile":profile_name,"backend":"desktop","experimental":true,
                "config_path":path,"signed_in":null,"client_id":"","tenant":"","channel_history_requested":false,
                "read_only":profile.read_only,"desktop_port":port,"desktop_account":profile.desktop_account,"desktop_client":profile.desktop_client,
                "browser_auto_start":profile.browser_auto_start});
            if let (Some(value), serde_json::Value::Object(extra)) = (value.as_object_mut(), extra)
            {
                value.extend(extra);
            }
            value
        };
        if args.no_login {
            let path = config::save(profile_name, profile.clone())?;
            let value = output(
                &path,
                &profile,
                serde_json::json!({"endpoint_available":null,"session_verified":false,
                "send_capability_verified":null,"teams_restarted":null,"browser_started":null,"chat_list_empty":null,"connection":null}),
            );
            return out.value(&value, || {
                format!("Saved desktop profile '{profile_name}' without contacting Teams.\nConfig: {}\nNext: `teams --profile {profile_name} desktop {}` to connect", path.display(), if profile.desktop_client.is_browser() { "login" } else { "connect" })
            });
        }
        // The profile is saved only once a chat read works, so a failed init leaves any
        // existing profile of this name untouched.
        let client = DesktopClient::from_profile(&profile)?;
        if profile.desktop_client.is_browser() {
            confirm_browser_connection(yes)?;
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if guided {
                desktop::browser_setup::begin(profile_name, &profile)?;
            }
            let verified = async {
                let login = if guided {
                    client.login_for_setup(out.quiet).await?
                } else {
                    client.login_with_progress(out.quiet).await?
                };
                let chats = client.chats(1, None).await?;
                verify_setup_sending(&client, &mut profile, guided, out).await?;
                let connection = client.probe().await?;
                let path = config::save(profile_name, profile.clone())?;
                Ok::<_, AppError>((login, chats, connection, path))
            }
            .await;
            let (login, chats, connection, path) = verified.map_err(|error| {
                let retry = if guided {
                    format!("Retry `teams init --profile {}` and confirm the connection; your saved sign-in will be reused.", shell_word(profile_name))
                } else { "Retry the same init command.".into() };
                error.with_hint(&format!("No connection settings were saved. {retry} To close any browser left running for this setup: `teams desktop disconnect --desktop-client {} --port {port}`.", profile.desktop_client.as_str()))
            })?;
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if guided && let Err(error) = desktop::browser_setup::finish(port) {
                out.note(format!("You're connected, but the unfinished-setup marker could not be removed: {error}"));
            }
            let mode = browser_mode(&login);
            let value = output(
                &path,
                &profile,
                serde_json::json!({"endpoint_available":true,
                "session_verified":true,"teams_restarted":false,"browser_started":true,
                "send_capability_verified":profile.allow_send.then_some(true),
                "chat_list_empty":chats.items.is_empty(),"connection":connection}),
            );
            return out.value(&value, || {
                if profile.browser_auto_start {
                    format!("You're connected. Run `teams` to open your inbox.\nThe Teams browser will reopen when needed; it is currently running {mode}.")
                } else {
                    format!("Connected desktop profile '{profile_name}'; browser is running {mode}.\nConfig: {}\nNext: `teams --profile {profile_name}` opens your inbox", path.display())
                }
            });
        }
        let mut connection = client.probe().await?;
        let needs_restart = !connection.endpoint_available();
        if needs_restart {
            if connection.status != desktop::ProbeStatus::NoListener {
                return Err(AppError::Desktop(format!(
                    "No profile was saved. {}",
                    connection.detail()
                )));
            }
            if profile.desktop_client.is_browser() {
                confirm_browser_connection(yes)?;
            } else {
                confirm_teams_restart(
                    yes,
                    Some(
                        "Teams is not running with its debugging port open. Opening it restarts Teams, and while it is open any program running as you can read your chats and send as you. `teams desktop disconnect` closes it again.",
                    ),
                    " No profile was saved and Teams was not restarted; run init again when Teams can restart.",
                )?;
            }
            desktop::connection::manage(&client, desktop::connection::Action::Connect).await?;
        }
        let verified = async {
            if needs_restart {
                connection = client.probe().await?;
            }
            // An open endpoint does not prove sign-in; reading a chat page does.
            let chats = client.chats(1, None).await?;
            verify_setup_sending(&client, &mut profile, guided, out).await?;
            let path = config::save(profile_name, profile.clone())?;
            Ok::<_, AppError>((chats, path))
        }
        .await;
        let (chats, path) = match verified {
            Ok(verified) => verified,
            Err(err) if needs_restart => return Err(err.with_hint(&format!(
                "No profile was saved, and the debugging port is still open; `teams desktop disconnect --desktop-client {} --port {port}` closes it.", profile.desktop_client.as_str()
            ))),
            Err(err) => return Err(err.with_hint("No profile was saved.")),
        };
        let chat_list_empty = chats.items.is_empty();
        let value = output(
            &path,
            &profile,
            serde_json::json!({"endpoint_available":connection.endpoint_available(),
            "send_capability_verified":profile.allow_send.then_some(true),
            "session_verified":true,"teams_restarted":needs_restart && profile.desktop_client == config::DesktopClientKind::Native,
            "browser_started":if profile.desktop_client.is_browser() { Some(needs_restart) } else { None },"chat_list_empty":chat_list_empty,
            "connection":connection}),
        );
        return out.value(&value, || {
            let restarted = if needs_restart {
                format!(" The Teams client was opened with its debugging port enabled; `teams --profile {profile_name} desktop disconnect` closes it.")
            } else {
                String::new()
            };
            let access = if profile.read_only {
                "read-only; re-run init with --allow-send to send messages"
            } else {
                "sending allowed"
            };
            let reads = if chat_list_empty {
                "the chat list is readable but empty"
            } else {
                "chats are readable"
            };
            format!("Connected desktop profile '{profile_name}' ({access}): {reads}.{restarted}\nConfig: {}\nNext: `teams --profile {profile_name}` opens your inbox", path.display())
        });
    }
    if args.desktop_port.is_some()
        || args.desktop_account.is_some()
        || args.desktop_client.is_some()
        || args.allow_send
    {
        return Err(AppError::InvalidInput(
            "--desktop-port, --desktop-account, --desktop-client and --allow-send require --backend desktop".into(),
        ));
    }
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if interactive && !out.json() && !guided {
        eprintln!("\n  teams\n  Microsoft Teams, without leaving your flow.\n");
        if args.device_code {
            eprintln!("  Use the Microsoft sign-in code on another device.");
        } else {
            eprintln!("  We’ll open Microsoft sign-in in your browser.");
        }
        eprintln!("  No password or client secret is shared with teams-cli.");
        eprintln!("  Graph supports work or school accounts.");
        eprintln!(
            "  For a personal account on macOS/Windows/WSL, use `teams init --backend desktop`.\n"
        );
    }
    let existing = config::connection_profile(Some(profile_name))?;
    let mut credential_store = args
        .credential_store
        .or_else(|| existing.as_ref().map(|profile| profile.credential_store))
        .unwrap_or_default();
    if existing.is_none()
        && args.credential_store.is_none()
        && args.pass_store_dir.is_none()
        && interactive
        && !out.json()
        && !args.no_login
        && auth::available_pass_directory().is_some()
    {
        let Some(selected) = setup::choose_credential_store(
            auth::native_store_label(),
            auth::native_store_error(profile_name).as_deref(),
            &mut io::stdin().lock(),
            &mut io::stderr().lock(),
        )?
        else {
            return Ok(());
        };
        credential_store = selected;
    }
    if existing
        .as_ref()
        .is_some_and(|profile| profile.credential_store != credential_store)
    {
        return Err(AppError::InvalidInput(
            "This profile already uses another credential store. Use --profile NEW_NAME to create a separate profile; the existing sign-in was preserved".into(),
        ));
    }
    let pass_store_dir = if credential_store == config::CredentialStore::Pass {
        let saved = existing
            .as_ref()
            .and_then(|profile| profile.pass_store_dir.as_deref());
        if existing.is_some() && saved.is_none() {
            return Err(AppError::InvalidInput("This pass profile has no saved pass_store_dir; restore its original directory in the profile configuration".into()));
        }
        let directory = auth::prepare_pass_directory(args.pass_store_dir.as_deref().or(saved))?;
        if let Some(saved) = saved
            && std::fs::canonicalize(saved)? != directory
        {
            return Err(AppError::InvalidInput(
                "This profile already uses another pass directory. Use --profile NEW_NAME to create a separate profile; the existing sign-in was preserved".into(),
            ));
        }
        Some(directory)
    } else {
        if args.pass_store_dir.is_some() {
            return Err(AppError::InvalidInput(
                "--pass-store-dir requires --credential-store pass".into(),
            ));
        }
        None
    };
    let client_id = args
        .client_id
        .or_else(|| std::env::var("TEAMS_CLIENT_ID").ok())
        .unwrap_or_else(|| config::DEFAULT_CLIENT_ID.into());
    let tenant = args
        .tenant
        .or_else(|| std::env::var("TEAMS_TENANT").ok())
        .unwrap_or_else(|| config::DEFAULT_TENANT.into());
    if client_id.trim().is_empty() {
        return Err(AppError::InvalidInput("client ID cannot be empty".into()));
    }
    if !args.no_login && !args.device_code && !interactive {
        return Err(AppError::NonInteractive(
            "browser sign-in requires a terminal; add --device-code or use --no-login".into(),
        ));
    }
    let profile = Profile {
        credential_store,
        pass_store_dir: pass_store_dir.clone(),
        backend: BackendKind::Graph,
        client_id: client_id.clone(),
        tenant: tenant.clone(),
        read_only: args.read_only,
        allow_send: false,
        desktop_port: config::DEFAULT_DESKTOP_PORT,
        desktop_account: Default::default(),
        desktop_client: Default::default(),
        browser_auto_start: false,
    };
    let channel_history = args.channel_history
        || (guided
            && config::connection_profile(Some(profile_name))?
                .is_some_and(|existing| existing.backend == BackendKind::Graph)
            && auth::channel_history_granted(profile_name) == Some(true));
    // Explicit/offline init keeps its contract. Guided setup saves only after a live read.
    let saved = if guided {
        None
    } else {
        Some(config::save(profile_name, profile.clone())?)
    };
    let mut signed_in = false;
    let mut pending_login = None;
    if !args.no_login {
        let resolved = if guided {
            profile.clone()
        } else {
            config::load(Some(profile_name))?.1
        };
        if guided {
            auth::check_store(profile_name, &resolved)?;
            let bundle = auth::sign_in(&resolved, args.device_code, channel_history, &out).await?;
            out.note("Checking chat access…");
            let check = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                graph::GraphClient::new(bundle.access_token.clone()).chats(1, None),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(AppError::Api(
                    "The chat access check timed out after 30 seconds.".into(),
                )),
            };
            check.map_err(|error| error.with_hint("Sign-in completed, but chat access could not be verified. No connection settings or saved credentials were changed. Check your organisation's Teams access, or run `teams init` and choose Teams Desktop or a browser."))?;
            pending_login = Some(bundle);
        } else {
            auth::login(
                profile_name,
                &resolved,
                args.device_code,
                channel_history,
                &out,
            )
            .await?;
        }
        signed_in = true;
    }
    let path = match (saved, pending_login) {
        (Some(path), _) => path,
        (_, Some(bundle)) => auth::save_verified_login(profile_name, &profile, &bundle, || {
            config::save(profile_name, profile.clone())
        })?,
        _ => config::save(profile_name, profile)?,
    };
    #[derive(Serialize)]
    struct InitResult<'a> {
        credential_store: config::CredentialStore,
        pass_store_dir: Option<std::path::PathBuf>,
        profile: &'a str,
        backend: &'a str,
        config_path: String,
        signed_in: bool,
        client_id: &'a str,
        tenant: &'a str,
        channel_history_requested: bool,
        read_only: bool,
    }
    let result = InitResult {
        credential_store,
        pass_store_dir,
        profile: profile_name,
        backend: "graph",
        config_path: path.display().to_string(),
        signed_in,
        client_id: &client_id,
        tenant: &tenant,
        channel_history_requested: channel_history,
        read_only: args.read_only,
    };
    out.value(&result, || {
        if signed_in {
            "You’re ready. Run `teams` to open your inbox.".into()
        } else {
            format!("Saved {}. Next: `teams auth login`", path.display())
        }
    })
}

fn config_command(
    profile_arg: Option<&str>,
    command: ConfigCommand,
    out: Output,
) -> Result<(), AppError> {
    match command {
        ConfigCommand::Path => out.value(&config::path().display().to_string(), || {
            config::path().display().to_string()
        }),
        ConfigCommand::Show => {
            let (name, profile) = config::load(profile_arg)?;
            let value = serde_json::json!({
                "profile": name,
                "backend": profile.backend,
                "credential_store": if profile.backend == BackendKind::Graph { Some(profile.credential_store) } else { None },
                "pass_store_dir": if profile.backend == BackendKind::Graph { profile.pass_store_dir.as_ref() } else { None },
                "desktop_port": if profile.backend == BackendKind::Desktop { Some(profile.desktop_port) } else { None },
                "desktop_account": if profile.backend == BackendKind::Desktop { Some(profile.desktop_account) } else { None },
                "desktop_client": if profile.backend == BackendKind::Desktop { Some(profile.desktop_client) } else { None },
                "browser_auto_start": profile.browser_auto_start,
                "client_id": profile.client_id,
                "tenant": profile.tenant,
                "read_only": profile.read_only,
                "config_path": config::path(),
            });
            out.value(&value, || {
                format!(
                    "Profile: {}\nBackend: {}\nTenant: {}\nClient ID: {}\nRead only: {}\nConfig: {}{}",
                    value["profile"].as_str().unwrap_or_default(),
                    profile.backend.as_str(),
                    value["tenant"].as_str().unwrap_or_default(),
                    value["client_id"].as_str().unwrap_or_default(),
                    yes_no(value["read_only"].as_bool().unwrap_or(false)),
                    config::path().display(),
                    if profile.backend == BackendKind::Desktop { format!("\nDesktop port: {}\nDesktop account: {}", profile.desktop_port, profile.desktop_account.as_str()) } else {
                        format!("\nCredential store: {}{}", if profile.credential_store == config::CredentialStore::Pass { "pass" } else { auth::native_store_label() }, profile.pass_store_dir.as_ref().map(|dir| format!(" ({})", dir.display())).unwrap_or_default())
                    },
                )
            })
        }
    }
}

async fn auth_command(
    profile_arg: Option<&str>,
    command: AuthCommand,
    out: Output,
) -> Result<(), AppError> {
    if config::selected_backend(profile_arg)? == BackendKind::Desktop {
        let AuthCommand::Status { offline } = command else {
            return Err(AppError::Unsupported(
                "desktop authentication is managed in Teams".into(),
            ));
        };
        let (name, profile) = config::load(profile_arg)?;
        let connection = if offline {
            None
        } else {
            Some(DesktopClient::from_profile(&profile)?.probe().await?)
        };
        let value = serde_json::json!({"profile":name,"backend":"desktop","experimental":true,"configured":true,
            "signed_in":null,"verified":false,"session_verified":false,"endpoint_available":connection.as_ref().map(|c| c.endpoint_available()),
            "identity":null,"authentication":"teams_desktop_session_unverified","offline":offline,
            "config_path":config::path(),"read_only":profile.read_only,"desktop_port":profile.desktop_port,"desktop_account":profile.desktop_account,"desktop_client":profile.desktop_client,
            "granted_scopes":null,"channel_history":null,"connection":connection});
        return out.value(&value, || format!("Profile: {name}\nBackend: desktop (experimental)\nEndpoint: {}\nSigned in: unknown\nSession verified: no\nRead only: {}",
            connection.as_ref().map(|c| c.detail()).unwrap_or_else(|| "not checked (offline)".into()), yes_no(profile.read_only)));
    }
    match command {
        AuthCommand::Login {
            device_code,
            channel_history,
        } => {
            if !device_code && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
                return Err(AppError::NonInteractive(
                    "browser sign-in requires a terminal; use `teams auth login --device-code` for headless sign-in"
                        .into(),
                ));
            }
            let (name, profile) = config::load(profile_arg)?;
            let token = auth::login(&name, &profile, device_code, channel_history, &out).await?;
            let value = serde_json::json!({"profile":name,"expires_at":token.expires_at,"scope":token.scope,"channel_history_requested":channel_history});
            out.value(&value, || format!("Signed in profile '{name}'."))
        }
        AuthCommand::Logout => {
            let (name, _) = config::load(profile_arg)?;
            auth::logout(&name)?;
            let value = serde_json::json!({"profile":name,"signed_in":false});
            out.value(&value, || format!("Signed out profile '{name}'."))
        }
        AuthCommand::Status { offline } => {
            let configured = config::configured_profile(profile_arg);
            let name = configured
                .as_ref()
                .map(|(n, _)| n.as_str())
                .unwrap_or(profile_arg.unwrap_or("default"));
            let (signed_in, credential_error) = match auth::credential_present(name) {
                Ok(present) => (present, None),
                Err(error) => (false, Some(error.to_string())),
            };
            let scopes = if signed_in {
                auth::granted_scopes(name)
            } else {
                None
            };
            let channel_history = scopes.as_ref().map(|scopes| {
                scopes
                    .iter()
                    .any(|scope| scope.eq_ignore_ascii_case(auth::CHANNEL_HISTORY_SCOPE))
            });
            let identity = if !offline && configured.is_some() && signed_in {
                Some(client(Some(name)).await?.me().await?)
            } else {
                None
            };
            let value = serde_json::json!({"profile":name,"backend":"graph","configured":configured.is_some(),"credential_store":configured.as_ref().map(|(_, profile)| profile.credential_store),"credential_error":credential_error,"signed_in":if credential_error.is_some() { None } else { Some(signed_in) },"verified":identity.is_some(),"identity":identity,"config_path":config::path(),"read_only":configured.as_ref().is_some_and(|(_, profile)| profile.read_only),"granted_scopes":scopes,"channel_history":channel_history});
            out.value(&value, || {
                format!(
                    "Profile: {name}\nConfigured: {}\nSigned in: {}\nRead only: {}\nChannel history: {}\nConfig: {}",
                    yes_no(configured.is_some()),
                    if let Some(error) = credential_error.as_ref() { format!("unavailable ({error})") } else { yes_no(signed_in).into() },
                    yes_no(value["read_only"].as_bool().unwrap_or(false)),
                    optional_access(channel_history),
                    config::path().display()
                )
            })
        }
    }
}

fn profile_command(command: ProfileCommand, yes: bool, out: Output) -> Result<(), AppError> {
    match command {
        ProfileCommand::List => {
            let profiles = config::profile_summaries()?;
            out.value(
                &serde_json::json!({"items": profiles, "total": profiles.len()}),
                || {
                    if profiles.is_empty() {
                        "No profiles configured. Run `teams init`.".into()
                    } else {
                        profiles
                            .iter()
                            .map(|profile| {
                                format!(
                                    "{} {}  {}  {}",
                                    if profile.active { "*" } else { " " },
                                    profile.name,
                                    profile.backend.as_str(),
                                    profile.tenant
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                },
            )
        }
        ProfileCommand::Use { name } => {
            config::use_profile(&name)?;
            out.value(
                &serde_json::json!({"profile": name, "active": true}),
                || format!("Active profile set to '{name}'."),
            )
        }
        ProfileCommand::Remove { name } => {
            if !yes {
                return Err(AppError::ConfirmationRequired(
                    "profile removal requires --yes".into(),
                ));
            }
            if config::connection_profile(Some(&name))?.is_some_and(|profile| {
                profile.backend == BackendKind::Graph
                    || profile.credential_store == config::CredentialStore::Pass
            }) {
                auth::logout(&name)?;
            }
            if !config::remove_profile(&name)? {
                return Err(AppError::InvalidInput(format!(
                    "profile '{name}' is not configured"
                )));
            }
            out.value(
                &serde_json::json!({"profile": name, "removed": true}),
                || format!("Removed profile '{name}'."),
            )
        }
    }
}

async fn client(profile_arg: Option<&str>) -> Result<TeamsBackend, AppError> {
    TeamsBackend::connect(profile_arg).await
}

fn render_conversation(
    page: &mut Page,
    args: &PageArgs,
    out: Output,
    text: fn(&Page, &render::View, &str) -> String,
) -> Result<(), AppError> {
    graph::select_fields(page, args.fields.as_deref())?;
    let continuation = page.next_cursor.as_deref().unwrap_or("<cursor>");
    out.value(page, || text(page, &render::View::terminal(), continuation))
}

fn render_offset_page(page: &mut Page, args: &OffsetPageArgs, out: Output) -> Result<(), AppError> {
    graph::select_fields(page, args.fields.as_deref())?;
    let continuation = page
        .next_offset
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<offset>".into());
    render_page_text(page, out, "--offset", &continuation)
}

fn render_page_text(
    page: &Page,
    out: Output,
    continuation_arg: &str,
    continuation: &str,
) -> Result<(), AppError> {
    out.value(page, || {
        let mut lines = Vec::new();
        for value in &page.items {
            let title = ["displayName", "topic", "subject", "id"]
                .iter()
                .find_map(|key| value.get(*key).and_then(Value::as_str))
                .unwrap_or("(untitled)");
            let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
            lines.push(if title == id || id.is_empty() {
                title.into()
            } else {
                format!("{title}\n  {id}")
            });
        }
        if page.truncated {
            lines.push(format!(
                "\nMore available. Continue with {continuation_arg} '{continuation}'"
            ));
        }
        if lines.is_empty() {
            "No results.".into()
        } else {
            lines.join("\n")
        }
    })
}

async fn tui_command(
    profile_arg: Option<&str>,
    args: TuiArgs,
    interactive_resume: bool,
) -> Result<(), AppError> {
    if args.snapshot {
        println!("{}", tui::snapshot(args.width, args.height)?);
        return Ok(());
    }
    if args.demo {
        tokio::task::block_in_place(|| tui::run(tui::Source::Demo))?;
        return Ok(());
    }
    if config::connection_profile(profile_arg)?.is_none()
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && io::stderr().is_terminal()
    {
        return Box::pin(guided_init(
            profile_arg,
            default_init_args()?,
            Output {
                format: OutputFormat::Text,
                quiet: false,
            },
        ))
        .await;
    }
    let profile_name = config::selected_profile_name(profile_arg)?;
    let mut startup_watch = None;
    if let Some(profile) = config::connection_profile(profile_arg)?
        && profile.backend == BackendKind::Desktop
        && profile.desktop_client.is_browser()
        && profile.browser_auto_start
        && interactive_resume
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
    {
        startup_watch = resume_browser(&profile_name, &profile).await?;
    }
    let mut inbox_state = tui::InboxState::default();
    loop {
        // `None` means the person already asked to sign in from inside the TUI.
        let connection_error = match client(profile_arg).await {
            Ok(backend) => {
                let can_send = config::load(Some(&profile_name))
                    .is_ok_and(|(_, profile)| profile.require_writable().is_ok());
                let source = tui::Source::Live {
                    backend: std::sync::Arc::new(backend),
                    profile: profile_name.clone(),
                    can_send,
                };
                match tokio::task::block_in_place(|| {
                    tui::run_with_watch(source, startup_watch.take(), &mut inbox_state)
                })? {
                    tui::TuiExit::Quit => return Ok(()),
                    tui::TuiExit::Authenticate => None,
                }
            }
            Err(error) if tui_auth_recoverable(&error) => Some(error),
            Err(error) => return Err(error),
        };
        let configured = config::configured_profile(profile_arg).is_some();
        if let Some(error) = connection_error {
            let reason = tui_connection_reason(&error);
            if tui::request_authentication(&profile_name, configured, &reason)?
                == tui::TuiExit::Quit
            {
                return Ok(());
            }
        }

        loop {
            let auth_output = Output {
                format: OutputFormat::Text,
                quiet: false,
            };
            let result = if configured {
                let (_, profile) = config::load(profile_arg)?;
                if profile.backend == BackendKind::Desktop {
                    let client = DesktopClient::from_profile(&profile)?;
                    if profile.desktop_client.is_browser() {
                        client.login_with_progress(false).await.map(|_| ())
                    } else {
                        if tui::request_authentication(
                            &profile_name,
                            true,
                            "Finish signing in in Teams Desktop, then press Enter to check the connection.",
                        )? == tui::TuiExit::Quit
                        {
                            return Ok(());
                        }
                        client.chats(1, None).await.map(|_| ())
                    }
                } else {
                    auth::login(
                        &profile_name,
                        &profile,
                        setup::graph_device_code_needed(),
                        auth::channel_history_granted(&profile_name) == Some(true),
                        &auth_output,
                    )
                    .await
                    .map(|_| ())
                }
            } else {
                init(
                    &profile_name,
                    default_init_args()?,
                    false,
                    auth_output,
                    false,
                )
                .await
            };
            let Err(error) = result else {
                break;
            };
            if tui::request_authentication(
                &profile_name,
                configured,
                &format!(
                    "Sign-in did not complete: {} You can try again. Your reply drafts are kept while you retry.",
                    concise_error(&error)
                ),
            )? == tui::TuiExit::Quit
            {
                return Ok(());
            }
        }
    }
}

async fn resume_browser(
    profile_name: &str,
    profile: &Profile,
) -> Result<Option<desktop::ConnectionWatch>, AppError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        eprintln!("Opening your Teams inbox…");
        let result = desktop::browser::resume(
            profile.desktop_port,
            profile.desktop_account,
            profile.desktop_client == config::DesktopClientKind::BrowserCore,
            false,
        )
        .await;
        match result {
            Ok((visible, watch)) => {
                if visible { eprintln!("Keep the Teams browser window open; you can minimise it."); }
                Ok(watch)
            },
            Err(AppError::Auth(_)) => {
                use std::io::Write;
                eprint!("Teams needs you to sign in again. Open sign-in now? [Y/n]: ");
                io::stderr().flush()?;
                let mut answer = String::new();
                if io::stdin().read_line(&mut answer)? > 0
                    && matches!(answer.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
                    DesktopClient::from_profile(profile)?.login_with_progress(false).await?;
                    return Ok(None);
                }
                Err(AppError::Auth(format!("Sign-in paused. Run `teams --profile {profile_name}` when you're ready.")))
            },
            Err(error) => Err(error.with_hint(&format!(
                "To sign in again, run `teams --profile {profile_name} desktop login`. Your saved connection was preserved."))),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (profile_name, profile);
        Err(AppError::Unsupported(
            "Browser inbox startup needs macOS, Linux or WSL2.".into(),
        ))
    }
}

fn tui_auth_recoverable(error: &AppError) -> bool {
    matches!(error, AppError::Auth(_))
        || matches!(error, AppError::InvalidInput(message) if message.contains("is not configured"))
}

fn tui_connection_reason(error: &AppError) -> String {
    match error {
        AppError::InvalidInput(message) if message.contains("is not configured") => {
            "This profile has not been connected yet.".into()
        }
        AppError::Auth(message)
            if message.contains("not signed in") || message.contains("unreadable") =>
        {
            "This profile does not have a usable Microsoft sign-in yet.".into()
        }
        AppError::Auth(message) if message.contains("expired") => {
            "Your saved Microsoft session has expired.".into()
        }
        _ => "Microsoft rejected the current session.".into(),
    }
}

fn concise_error(error: &AppError) -> String {
    error.to_string().trim_end_matches('.').to_string()
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugging_setup_is_offered_only_when_nothing_listens() {
        assert!(debugging_setup(None, 9333).is_none());
        for status in [
            desktop::ProbeStatus::UnsafeListenerRefused,
            desktop::ProbeStatus::NonTeamsListenerRefused,
            desktop::ProbeStatus::MetadataProbeFailed,
            desktop::ProbeStatus::MetadataOk,
        ] {
            let connection = desktop::ConnectionMetadata {
                port: 9333,
                status,
                loopback_only: None,
                teams_owned: None,
                version: None,
                browser_websocket_advertised: false,
                targets: Vec::new(),
                foreign_listeners: Vec::new(),
                user_debugging_flags: false,
            };
            assert!(
                debugging_setup(Some(&connection), 9333).is_none(),
                "{status:?}"
            );
        }
    }

    #[test]
    fn tui_recovers_configuration_and_authentication_only() {
        assert!(tui_auth_recoverable(&AppError::InvalidInput(
            "profile 'work' is not configured; run `teams init`".into()
        )));
        assert!(tui_auth_recoverable(&AppError::Auth(
            "session expired".into()
        )));
        assert!(!tui_auth_recoverable(&AppError::InvalidInput(
            "invalid field selection".into()
        )));
        assert!(!tui_auth_recoverable(&AppError::RateLimit(None)));
    }

    #[test]
    fn tui_connection_copy_is_actionable_without_shell_instructions() {
        let reason = tui_connection_reason(&AppError::Auth(
            "not signed in; run `teams auth login`".into(),
        ));
        assert_eq!(
            reason,
            "This profile does not have a usable Microsoft sign-in yet."
        );
        assert!(!reason.contains("teams auth login"));
    }
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

async fn doctor(profile_arg: Option<&str>, offline: bool, out: Output) -> Result<(), AppError> {
    if config::selected_backend(profile_arg)? == BackendKind::Desktop {
        return desktop_doctor(profile_arg, offline, out).await;
    }
    let mut checks = Vec::new();
    let configured = config::configured_profile(profile_arg);
    checks.push(Check {
        name: "configuration",
        status: if configured.is_some() { "pass" } else { "fail" },
        detail: config::path().display().to_string(),
    });
    if let Some((_, profile)) = &configured {
        checks.push(Check {
            name: "write_safety",
            status: "pass",
            detail: if profile.read_only {
                "read-only mode is enabled".into()
            } else {
                "remote writes are enabled".into()
            },
        });
    }
    let credential = configured
        .as_ref()
        .map(|(name, _)| auth::credential_present(name))
        .unwrap_or(Ok(false));
    let signed_in = matches!(credential, Ok(true));
    checks.push(Check {
        name: "credential_store",
        status: if signed_in { "pass" } else { "fail" },
        detail: match credential {
            Ok(true) => "credential is present".into(),
            Ok(false) => "run `teams auth login`".into(),
            Err(error) => error.to_string(),
        },
    });
    let channel_history = if signed_in {
        auth::channel_history_granted(
            configured
                .as_ref()
                .map(|(name, _)| name.as_str())
                .unwrap_or("default"),
        )
    } else {
        None
    };

    checks.push(Check {
        name: "channel_history",
        status: if channel_history == Some(true) {
            "pass"
        } else {
            "optional"
        },
        detail: if channel_history == Some(true) {
            "admin-consented channel reading is available".into()
        } else {
            "optional; run `teams auth login --channel-history` if an admin approves it".into()
        },
    });
    let mut identity_ok = false;
    let mut teams_ok = false;
    let (identity_detail, teams_detail) = if !offline && configured.is_some() && signed_in {
        match client(profile_arg).await {
            Ok(client) => {
                let identity_detail = match client.me().await {
                    Ok(value) => {
                        identity_ok = true;
                        format!(
                            "signed in as {}",
                            string_at(&value, "/userPrincipalName")
                                .or_else(|| string_at(&value, "/displayName"))
                                .unwrap_or("unknown user")
                        )
                    }
                    Err(error) => error.to_string(),
                };
                let teams_detail = match client.teams_available().await {
                    Ok(_) => {
                        teams_ok = true;
                        "Microsoft Teams is available".into()
                    }
                    Err(error) => error.to_string(),
                };
                (identity_detail, teams_detail)
            }
            Err(error) => {
                let detail = error.to_string();
                (detail.clone(), detail)
            }
        }
    } else {
        let detail = if offline {
            "network check skipped"
        } else {
            "skipped until setup is complete"
        }
        .to_string();
        (detail.clone(), detail)
    };
    checks.push(Check {
        name: "microsoft_identity",
        status: if offline {
            "skip"
        } else if identity_ok {
            "pass"
        } else {
            "fail"
        },
        detail: identity_detail,
    });
    checks.push(Check {
        name: "microsoft_teams",
        status: if offline {
            "skip"
        } else if teams_ok {
            "pass"
        } else {
            "fail"
        },
        detail: teams_detail,
    });
    let healthy = is_healthy(&checks);
    let value =
        serde_json::json!({"backend":"graph","checks":checks,"healthy":healthy,"offline":offline});
    doctor_report(&value, &checks, None, out)
}

async fn desktop_doctor(
    profile_arg: Option<&str>,
    offline: bool,
    out: Output,
) -> Result<(), AppError> {
    let (name, profile) = config::load(profile_arg)?;
    let prerequisites = DesktopClient::from_profile(&profile)?.prerequisites();
    let mut checks = vec![
        Check {
            name: "configuration",
            status: "pass",
            detail: config::path().display().to_string(),
        },
        Check {
            name: "write_safety",
            status: "pass",
            detail: if profile.read_only {
                "profile is read-only; diagnostics send nothing"
            } else {
                "profile may send chat messages, each confirmed; diagnostics send nothing"
            }
            .into(),
        },
        Check {
            name: "desktop_transport",
            status: if prerequisites.is_ok() {
                "pass"
            } else {
                "fail"
            },
            detail: prerequisites
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|e| e.to_string()),
        },
    ];
    if let Some((status, detail)) = (profile.desktop_client == config::DesktopClientKind::Native)
        .then(desktop::helper_check)
        .flatten()
    {
        checks.push(Check {
            name: "desktop_helper",
            status,
            detail,
        });
    }
    let desktop = DesktopClient::from_profile(&profile)?;
    let connection = if !offline && prerequisites.is_ok() {
        match desktop.probe().await {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                checks.push(Check {
                    name: "desktop_probe",
                    status: "fail",
                    detail: error.to_string(),
                });
                None
            }
        }
    } else {
        None
    };
    checks.push(Check {
        name: "desktop_endpoint",
        status: if offline {
            "skip"
        } else if connection.as_ref().is_some_and(|c| c.endpoint_available()) {
            "pass"
        } else {
            "fail"
        },
        detail: connection.as_ref().map(|c| c.detail()).unwrap_or_else(|| {
            if offline {
                "endpoint check skipped (offline)"
            } else {
                "endpoint check unavailable"
            }
            .into()
        }),
    });
    let endpoint_ok = connection.as_ref().is_some_and(|c| c.endpoint_available());
    let (status, detail) = if profile.read_only {
        ("skip", "sending disabled by profile".to_string())
    } else if offline {
        ("skip", "send check skipped (offline)".to_string())
    } else if !endpoint_ok {
        ("skip", "skipped until the endpoint verifies".to_string())
    } else {
        match desktop.send_check().await {
            Ok(()) => ("pass", "the Teams window can send messages".to_string()),
            Err(error) => ("fail", error.to_string()),
        }
    };
    checks.push(Check {
        name: "desktop_send",
        status,
        detail,
    });
    checks.push(Check {
        name: "session_access",
        status: "unverified",
        detail:
            "sign-in is not verified; doctor does not read chats (try `teams chats list --limit 1`)"
                .into(),
    });
    let healthy = is_healthy(&checks);
    let setup = if profile.desktop_client.is_browser() {
        connection
            .as_ref()
            .filter(|c| c.status == desktop::ProbeStatus::NoListener)
            .and_then(|_| desktop.debugging_setup())
    } else {
        debugging_setup(connection.as_ref(), profile.desktop_port)
    };
    let value = serde_json::json!({"profile":name,"backend":"desktop","experimental":true,"healthy":healthy,"offline":offline,
        "endpoint_available":connection.as_ref().map(|c| c.endpoint_available()),"session_verified":false,"signed_in":null,
        "desktop_port":profile.desktop_port,"desktop_account":profile.desktop_account,"desktop_client":profile.desktop_client,"connection":connection,"debugging_setup":setup,"checks":checks});
    doctor_report(&value, &checks, setup.map(|s| s.text()), out)
}

fn is_healthy(checks: &[Check]) -> bool {
    checks.iter().all(|check| check.status != "fail")
}

/// The commands to open Teams' debugging port, when a probe found nothing listening.
fn debugging_setup(
    connection: Option<&desktop::ConnectionMetadata>,
    port: u16,
) -> Option<desktop::DebuggingSetup> {
    connection
        .filter(|c| c.status == desktop::ProbeStatus::NoListener)
        .and_then(|_| desktop::DebuggingSetup::new(port))
}

/// Prints the full report and any advice after it, then fails with `unhealthy` so scripts
/// can rely on the exit code.
fn doctor_report(
    value: &Value,
    checks: &[Check],
    advice: Option<String>,
    out: Output,
) -> Result<(), AppError> {
    let width = checks.iter().map(|c| c.status.len()).max().unwrap_or(0);
    out.value(value, || {
        let report = checks
            .iter()
            .map(|c| format!("{:<18} {:<width$}  {}", c.name, c.status, c.detail))
            .collect::<Vec<_>>()
            .join("\n");
        match &advice {
            Some(advice) => format!("{report}\n\n{advice}"),
            None => report,
        }
    })?;
    let failed: Vec<_> = checks
        .iter()
        .filter(|c| c.status == "fail")
        .map(|c| c.name)
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        Err(AppError::Unhealthy(format!(
            "doctor found failing checks: {}",
            failed.join(", ")
        )))
    }
}

fn capabilities(out: Output) -> Result<(), AppError> {
    let value = serde_json::json!({
        "supported":["delegated work/school sign-in","identity","joined teams","channels","chats","people search","create one-to-one chats","chat messages","channel messages","send messages","TUI"],
        "unsupported":["personal Microsoft accounts through Graph","user calls and screen sharing","private Teams protocols","tenant administration"],
        "account_types":["Microsoft 365 work or school","personal Microsoft accounts (desktop; browser client also on Linux)"],
        "api":"Microsoft Graph v1.0",
        "backends":schema::backend_capabilities(),
        "notes":["Normal sign-in uses only user-consentable delegated permissions.","Reading channel messages requires `teams auth login --channel-history` and admin consent for ChannelMessage.Read.All.","Calls require bot/application APIs and are outside this user client."]
    });
    out.value(&value, || "Graph (default)\n  Work/school delegated sign-in; teams, channels, people search, chats, messages; create one-to-one chats; send as yourself; TUI\n\nDesktop (experimental, macOS/Windows/WSL; browser client on macOS/Linux/WSL2)\n  Endpoint diagnostics. Personal and work/school accounts support chats and chat messages from the running Teams client's local cache, people search, and one-to-one chat creation and plain-text sending for profiles created with `--allow-send`; the TUI uses the same backend. The browser client uses a dedicated profile, with visible sign-in and headless operation.\n\nNot supported\n  Desktop teams, channels, replies and attachments; personal accounts through Graph; calls or screen sharing; private Teams protocols; tenant administration\n\nRun `teams schema` for the machine contract.".into())
}

/// Desktop sends act through the user's own Teams window, so each one is confirmed at the
/// terminal unless `--yes` was given.
fn confirm_desktop_send(chat: &str, body: &str) -> Result<(), AppError> {
    let first = body.trim().lines().next().unwrap_or_default();
    let preview: String = first.chars().take(60).collect();
    let more = if preview.len() < body.trim().len() {
        "…"
    } else {
        ""
    };
    eprint!("Send to {chat} as you in Teams:\n  {preview}{more}\nSend? [y/N] ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(AppError::ConfirmationRequired(
            "send cancelled; nothing was sent".into(),
        ))
    }
}

fn confirm_create_chat(yes: bool, profile: &str, recipient: &str) -> Result<(), AppError> {
    if yes {
        return Ok(());
    }
    if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
        return Err(AppError::ConfirmationRequired("creating a conversation needs confirmation; pass --yes when not running in a terminal. No message will be sent".into()));
    }
    eprint!(
        "Create or open a one-to-one conversation with {recipient} using profile '{profile}'? No message will be sent. [y/N] "
    );
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(AppError::ConfirmationRequired(
            "conversation creation cancelled; nothing was sent".into(),
        ))
    }
}

fn read_body(body: &str) -> Result<String, AppError> {
    if body != "-" {
        return Ok(body.into());
    }
    let mut value = String::new();
    io::stdin().read_to_string(&mut value)?;
    Ok(value)
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
fn optional_access(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "granted",
        Some(false) => "not granted (optional)",
        None => "unknown",
    }
}
fn identity_text(value: &Value) -> String {
    format!(
        "{}\n{}",
        string_at(value, "/displayName").unwrap_or("Unknown user"),
        string_at(value, "/userPrincipalName")
            .or_else(|| string_at(value, "/mail"))
            .unwrap_or("")
    )
}

/// Asks before restarting Teams unless `--yes` was given; `on_refusal` is appended to
/// the error when the restart is not confirmed.
fn confirm_teams_restart(yes: bool, note: Option<&str>, on_refusal: &str) -> Result<(), AppError> {
    if yes {
        return Ok(());
    }
    if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
        return Err(AppError::ConfirmationRequired(format!(
            "Restarting Teams needs confirmation; pass --yes when not running in a terminal.{on_refusal}"
        )));
    }
    if let Some(note) = note {
        eprintln!("{note}");
    }
    eprint!("Restart Teams? This ends any call in progress. [y/N] ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        return Err(AppError::ConfirmationRequired(format!(
            "Teams restart cancelled.{on_refusal}"
        )));
    }
    Ok(())
}

/// How a verified browser sign-in left the browser, from its management status.
fn browser_mode(status: &serde_json::Value) -> &'static str {
    if status["management"]["visible"] == true {
        "visible; keep its window open"
    } else {
        "headless"
    }
}

fn confirm_browser_connection(yes: bool) -> Result<(), AppError> {
    if yes {
        return Ok(());
    }
    if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
        return Err(AppError::ConfirmationRequired("Managing the Teams browser needs confirmation; pass --yes when not running in a terminal".into()));
    }
    eprint!(
        "Open or close the dedicated Teams browser? Closing it ends any call in that window. Its debugging port permits local programs to read chats and send as you. [y/N] "
    );
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err(AppError::ConfirmationRequired(
            "Browser connection cancelled".into(),
        ))
    }
}

fn confirm_clear_flags(yes: bool) -> Result<(), AppError> {
    if yes {
        return Ok(());
    }
    if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
        return Err(AppError::ConfirmationRequired("Changing the user-wide WebView2 setting needs confirmation; pass --yes when not running in a terminal".into()));
    }
    eprint!(
        "Remove the remote-debugging flags from the user-wide WebView2 setting? Other arguments are kept. Apps already running keep the port until fully quit. [y/N] "
    );
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err(AppError::ConfirmationRequired(
            "Clearing debugging flags cancelled".into(),
        ))
    }
}

async fn desktop_connection(
    profile: Option<&str>,
    command: DesktopCommand,
    yes: bool,
    out: Output,
) -> Result<(), AppError> {
    use desktop::connection::{self, Action};
    if let DesktopCommand::Supervise { keep_alive } = &command {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        return desktop::browser::lifetime::supervise(*keep_alive).await;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = keep_alive;
            return Err(AppError::Unsupported(
                "browser supervision requires macOS or Linux".into(),
            ));
        }
    }
    let login = matches!(&command, DesktopCommand::Login(_));
    let clear = matches!(&command, DesktopCommand::ClearFlags(_));
    let (action, args) = match command {
        DesktopCommand::Status(args) => (Action::Status, args),
        DesktopCommand::Connect(args) | DesktopCommand::Login(args) => (Action::Connect, args),
        DesktopCommand::Disconnect(args) => (Action::Disconnect, args),
        DesktopCommand::Recover(args) => (Action::Recover, args),
        DesktopCommand::ClearFlags(args) => (Action::Status, args),
        DesktopCommand::Supervise { .. } => unreachable!("handled above"),
    };
    let selected = config::connection_profile(profile)?;
    let kind = args.desktop_client.unwrap_or_else(|| {
        selected
            .as_ref()
            .map(|p| p.desktop_client)
            .unwrap_or_default()
    });
    let port = args.port.unwrap_or_else(|| {
        selected
            .as_ref()
            .filter(|p| p.desktop_client == kind)
            .map(|p| p.desktop_port)
            .unwrap_or_else(|| kind.default_port())
    });
    let account = selected
        .as_ref()
        .map(|p| p.desktop_account)
        .filter(|account| {
            kind != config::DesktopClientKind::BrowserCore
                || *account != config::DesktopAccount::Auto
        })
        .unwrap_or_else(|| kind.default_account());
    if login && !kind.is_browser() {
        return Err(AppError::InvalidInput(
            "desktop login requires --desktop-client browser".into(),
        ));
    }
    if clear && kind.is_browser() {
        return Err(AppError::InvalidInput(
            "desktop clear-flags applies to the Teams desktop client".into(),
        ));
    }
    if args.visible && (login || !kind.is_browser() || !matches!(action, Action::Connect)) {
        return Err(AppError::InvalidInput(
            "--visible requires desktop connect --desktop-client browser".into(),
        ));
    }
    if login
        || matches!(action, Action::Connect | Action::Disconnect)
        || (kind.is_browser() && matches!(action, Action::Recover))
    {
        if kind.is_browser() {
            confirm_browser_connection(yes)?;
        } else {
            confirm_teams_restart(yes, None, "")?;
        }
    }
    let client = DesktopClient::with_client(port, account, kind)?;
    if clear {
        confirm_clear_flags(yes)?;
        let value = connection::clear_flags(&client).await?;
        return out.value(&value, || {
            if value["cleared"] == true {
                "Removed the debugging flags from the user-wide WebView2 setting. Fully quit and reopen apps that use WebView2 (such as Outlook) so they close the port.".into()
            } else {
                "The user-wide WebView2 setting has no debugging flags; nothing changed.".into()
            }
        });
    }
    let value = if login {
        client.login_with_progress(out.quiet).await?
    } else {
        if matches!(action, Action::Connect) {
            client.prepare_browser(out.quiet).await?;
        }
        connection::manage_visible(&client, action, args.visible).await?
    };
    out.value(&value, || {
        if login {
            return format!(
                "Teams sign-in verified; the browser is running {}.",
                browser_mode(&value)
            );
        }
        format!(
            "Desktop endpoint: {}. Managed session: {}. Recovery needed: {}.",
            if value["endpoint_available"] == true {
                "available"
            } else if value["endpoint_available"].is_null() {
                "not checked"
            } else {
                "unavailable"
            },
            value["management"]["managed"],
            value["management"]["needs_recovery"]
        )
    })
}
