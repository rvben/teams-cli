use std::io::{self, IsTerminal, Read};

use clap::{CommandFactory, Parser};
use serde::Serialize;
use serde_json::Value;

use teams_cli::auth;
use teams_cli::backend::TeamsBackend;
use teams_cli::cli::{
    AuthCommand, ChannelsCommand, ChatsCommand, Cli, Command, ConfigCommand, DesktopCommand,
    InitArgs, MessagesCommand, OffsetPageArgs, PageArgs, ProfileCommand, TuiArgs,
};
use teams_cli::config::{self, BackendKind, Profile};
use teams_cli::desktop::{self, DesktopClient};
use teams_cli::error::AppError;
use teams_cli::graph::{self, Page};
use teams_cli::output::{Output, OutputFormat, print_error, structured_from_args};
use teams_cli::{render, schema, tui};

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
        None if io::stdin().is_terminal() && io::stdout().is_terminal() && !config::exists() => {
            Command::Init(InitArgs {
                backend: BackendKind::Graph,
                desktop_port: None,
                desktop_account: None,
                client_id: None,
                tenant: None,
                no_login: false,
                device_code: false,
                channel_history: false,
                read_only: false,
                allow_send: false,
            })
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
        Command::Init(args) => init(profile_arg.unwrap_or("default"), args, yes, out).await,
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
        Command::Tui(args) => tui_command(profile_arg, args).await,
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

async fn init(profile_name: &str, args: InitArgs, yes: bool, out: Output) -> Result<(), AppError> {
    if args.backend == BackendKind::Desktop {
        if args.client_id.is_some()
            || args.device_code
            || args.channel_history
            || args.tenant.is_some()
        {
            return Err(AppError::InvalidInput("desktop profiles do not use OAuth client, tenant, device-code or channel-history options".into()));
        }
        if args.allow_send && args.read_only {
            return Err(AppError::InvalidInput(
                "--allow-send conflicts with --read-only (or TEAMS_READ_ONLY)".into(),
            ));
        }
        let read_only = !args.allow_send;
        let profile = Profile {
            backend: BackendKind::Desktop,
            client_id: String::new(),
            tenant: String::new(),
            read_only,
            allow_send: args.allow_send,
            desktop_port: args.desktop_port.unwrap_or(config::DEFAULT_DESKTOP_PORT),
            desktop_account: args.desktop_account.unwrap_or_default(),
        };
        let port = profile.desktop_port;
        let output = |path: &std::path::Path, extra: serde_json::Value| {
            let mut value = serde_json::json!({"profile":profile_name,"backend":"desktop","experimental":true,
                "config_path":path,"signed_in":null,"client_id":"","tenant":"","channel_history_requested":false,
                "read_only":read_only,"desktop_port":port,"desktop_account":profile.desktop_account});
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
                serde_json::json!({"endpoint_available":null,"session_verified":false,
                "teams_restarted":null,"chat_list_empty":null,"connection":null}),
            );
            return out.value(&value, || {
                format!("Saved desktop profile '{profile_name}' without contacting Teams.\nConfig: {}\nNext: `teams --profile {profile_name} init --backend desktop` to connect", path.display())
            });
        }
        // The profile is saved only once a chat read works, so a failed init leaves any
        // existing profile of this name untouched.
        let client = DesktopClient::with_account(port, profile.desktop_account)?;
        let mut connection = client.probe().await?;
        let needs_restart = !connection.endpoint_available();
        if needs_restart {
            if connection.status != desktop::ProbeStatus::NoListener {
                return Err(AppError::Desktop(format!(
                    "No profile was saved. {}",
                    connection.status.detail()
                )));
            }
            confirm_teams_restart(
                yes,
                Some(
                    "Teams is not running with its debugging port open. Opening it restarts Teams, and while it is open any program running as you can read your chats and send as you. `teams desktop disconnect` closes it again.",
                ),
                " No profile was saved and Teams was not restarted; run init again when Teams can restart.",
            )?;
            desktop::connection::manage(&client, desktop::connection::Action::Connect).await?;
        }
        let verified = async {
            if needs_restart {
                connection = client.probe().await?;
            }
            // An open endpoint does not prove sign-in; reading a chat page does.
            let chats = client.chats(1, None).await?;
            let path = config::save(profile_name, profile.clone())?;
            Ok::<_, AppError>((chats, path))
        }
        .await;
        let (chats, path) = match verified {
            Ok(verified) => verified,
            Err(err) if needs_restart => return Err(err.with_hint(&format!(
                "No profile was saved, and Teams' debugging port is still open; `teams desktop disconnect --port {port}` closes it."
            ))),
            Err(err) => return Err(err.with_hint("No profile was saved.")),
        };
        let chat_list_empty = chats.items.is_empty();
        let value = output(
            &path,
            serde_json::json!({"endpoint_available":connection.endpoint_available(),
            "session_verified":true,"teams_restarted":needs_restart,"chat_list_empty":chat_list_empty,
            "connection":connection}),
        );
        return out.value(&value, || {
            let restarted = if needs_restart {
                format!(" Teams was restarted with its debugging port open; `teams --profile {profile_name} desktop disconnect` closes it.")
            } else {
                String::new()
            };
            let access = if read_only {
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
    if args.desktop_port.is_some() || args.desktop_account.is_some() || args.allow_send {
        return Err(AppError::InvalidInput(
            "--desktop-port, --desktop-account and --allow-send require --backend desktop".into(),
        ));
    }
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if interactive && !out.json() {
        eprintln!("\n  teams\n  Microsoft Teams, without leaving your flow.\n");
        eprintln!("  We’ll open Microsoft sign-in in your browser.");
        eprintln!("  No password or client secret is shared with teams-cli.");
        eprintln!("  Graph supports work or school accounts.");
        eprintln!(
            "  For a personal account on macOS/Windows/WSL, use `teams init --backend desktop`.\n"
        );
    }
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
    let path = config::save(
        profile_name,
        Profile {
            backend: BackendKind::Graph,
            client_id: client_id.clone(),
            tenant: tenant.clone(),
            read_only: args.read_only,
            allow_send: false,
            desktop_port: config::DEFAULT_DESKTOP_PORT,
            desktop_account: Default::default(),
        },
    )?;
    let mut signed_in = false;
    if !args.no_login {
        let (_, profile) = config::load(Some(profile_name))?;
        auth::login(
            profile_name,
            &profile,
            args.device_code,
            args.channel_history,
            &out,
        )
        .await?;
        signed_in = true;
    }
    #[derive(Serialize)]
    struct InitResult<'a> {
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
        profile: profile_name,
        backend: "graph",
        config_path: path.display().to_string(),
        signed_in,
        client_id: &client_id,
        tenant: &tenant,
        channel_history_requested: args.channel_history,
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
                "desktop_port": if profile.backend == BackendKind::Desktop { Some(profile.desktop_port) } else { None },
                "desktop_account": if profile.backend == BackendKind::Desktop { Some(profile.desktop_account) } else { None },
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
                    if profile.backend == BackendKind::Desktop { format!("\nDesktop port: {}\nDesktop account: {}", profile.desktop_port, profile.desktop_account.as_str()) } else { String::new() },
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
            Some(
                DesktopClient::with_account(profile.desktop_port, profile.desktop_account)?
                    .probe()
                    .await?,
            )
        };
        let value = serde_json::json!({"profile":name,"backend":"desktop","experimental":true,"configured":true,
            "signed_in":null,"verified":false,"session_verified":false,"endpoint_available":connection.as_ref().map(|c| c.endpoint_available()),
            "identity":null,"authentication":"teams_desktop_session_unverified","offline":offline,
            "config_path":config::path(),"read_only":profile.read_only,"desktop_port":profile.desktop_port,"desktop_account":profile.desktop_account,
            "granted_scopes":null,"channel_history":null,"connection":connection});
        return out.value(&value, || format!("Profile: {name}\nBackend: desktop (experimental)\nEndpoint: {}\nSigned in: unknown\nSession verified: no\nRead only: {}",
            connection.as_ref().map(|c| c.status.detail()).unwrap_or("not checked (offline)"), yes_no(profile.read_only)));
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
            let signed_in = auth::has_token(name);
            let scopes = auth::granted_scopes(name);
            let channel_history = auth::channel_history_granted(name);
            let identity = if !offline && configured.is_some() && signed_in {
                Some(client(Some(name)).await?.me().await?)
            } else {
                None
            };
            let value = serde_json::json!({"profile":name,"backend":"graph","configured":configured.is_some(),"signed_in":signed_in,"verified":identity.is_some(),"identity":identity,"config_path":config::path(),"read_only":configured.as_ref().is_some_and(|(_, profile)| profile.read_only),"granted_scopes":scopes,"channel_history":channel_history});
            out.value(&value, || {
                format!(
                    "Profile: {name}\nConfigured: {}\nSigned in: {}\nRead only: {}\nChannel history: {}\nConfig: {}",
                    yes_no(configured.is_some()),
                    yes_no(signed_in),
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
            if config::selected_backend(Some(&name))? == BackendKind::Graph {
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

async fn tui_command(profile_arg: Option<&str>, args: TuiArgs) -> Result<(), AppError> {
    if args.snapshot {
        println!("{}", tui::snapshot(args.width, args.height)?);
        return Ok(());
    }
    if args.demo {
        tokio::task::block_in_place(|| tui::run(tui::Source::Demo))?;
        return Ok(());
    }
    let profile_name = config::selected_profile_name(profile_arg)?;
    let mut reason_override = None;
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
                match tokio::task::block_in_place(|| tui::run(source))? {
                    tui::TuiExit::Quit => return Ok(()),
                    tui::TuiExit::Authenticate => None,
                }
            }
            Err(error) if tui_auth_recoverable(&error) => Some(error),
            Err(error) => return Err(error),
        };
        let configured = config::configured_profile(profile_arg).is_some();
        if let Some(error) = connection_error {
            let reason = reason_override
                .take()
                .unwrap_or_else(|| tui_connection_reason(&error));
            if tui::request_authentication(&profile_name, configured, &reason)?
                == tui::TuiExit::Quit
            {
                return Ok(());
            }
        }

        let auth_output = Output {
            format: OutputFormat::Text,
            quiet: false,
        };
        let result = if configured {
            let (_, profile) = config::load(profile_arg)?;
            auth::login(&profile_name, &profile, false, false, &auth_output)
                .await
                .map(|_| ())
        } else {
            init(
                &profile_name,
                InitArgs {
                    backend: BackendKind::Graph,
                    desktop_port: None,
                    desktop_account: None,
                    client_id: None,
                    tenant: None,
                    no_login: false,
                    device_code: false,
                    channel_history: false,
                    read_only: false,
                    allow_send: false,
                },
                false,
                auth_output,
            )
            .await
        };
        if let Err(error) = result {
            reason_override = Some(format!(
                "Sign-in did not complete: {} You can try again.",
                concise_error(&error)
            ));
        }
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
    let signed_in = configured
        .as_ref()
        .is_some_and(|(name, _)| auth::has_token(name));
    checks.push(Check {
        name: "credential_store",
        status: if signed_in { "pass" } else { "fail" },
        detail: if signed_in {
            "credential is present".into()
        } else {
            "run `teams auth login`".into()
        },
    });
    let channel_history = auth::channel_history_granted(
        configured
            .as_ref()
            .map(|(name, _)| name.as_str())
            .unwrap_or("default"),
    );
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
    let prerequisites = desktop::local_prerequisites();
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
    if let Some((status, detail)) = desktop::helper_check() {
        checks.push(Check {
            name: "desktop_helper",
            status,
            detail,
        });
    }
    let desktop = DesktopClient::with_account(profile.desktop_port, profile.desktop_account)?;
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
        detail: connection
            .as_ref()
            .map(|c| c.status.detail())
            .unwrap_or(if offline {
                "endpoint check skipped (offline)"
            } else {
                "endpoint check unavailable"
            })
            .into(),
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
    let setup = debugging_setup(connection.as_ref(), profile.desktop_port);
    let value = serde_json::json!({"profile":name,"backend":"desktop","experimental":true,"healthy":healthy,"offline":offline,
        "endpoint_available":connection.as_ref().map(|c| c.endpoint_available()),"session_verified":false,"signed_in":null,
        "desktop_port":profile.desktop_port,"desktop_account":profile.desktop_account,"connection":connection,"debugging_setup":setup,"checks":checks});
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
        "supported":["delegated work/school sign-in","identity","joined teams","channels","chats","chat messages","channel messages","send messages","TUI"],
        "unsupported":["personal Microsoft accounts through Graph","user calls and screen sharing","private Teams protocols","tenant administration"],
        "account_types":["Microsoft 365 work or school","personal Microsoft accounts (desktop on macOS/Windows/WSL)"],
        "api":"Microsoft Graph v1.0",
        "backends":schema::backend_capabilities(),
        "notes":["Normal sign-in uses only user-consentable delegated permissions.","Reading channel messages requires `teams auth login --channel-history` and admin consent for ChannelMessage.Read.All.","Calls require bot/application APIs and are outside this user client."]
    });
    out.value(&value, || "Graph (default)\n  Work/school delegated sign-in; teams, channels, chats, messages; send as yourself; TUI\n\nDesktop (experimental, macOS/Windows/WSL)\n  Endpoint diagnostics. Personal and work/school accounts support chats and chat messages from the running Teams client's local cache, and plain-text chat sending for profiles created with `--allow-send`; the TUI uses the same backend on all three platforms.\n\nNot supported\n  Desktop teams, channels, replies and attachments; personal accounts through Graph; calls or screen sharing; private Teams protocols; tenant administration\n\nRun `teams schema` for the machine contract.".into())
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

async fn desktop_connection(
    profile: Option<&str>,
    command: DesktopCommand,
    yes: bool,
    out: Output,
) -> Result<(), AppError> {
    use desktop::connection::{self, Action};
    let (action, args) = match command {
        DesktopCommand::Status(args) => (Action::Status, args),
        DesktopCommand::Connect(args) => (Action::Connect, args),
        DesktopCommand::Disconnect(args) => (Action::Disconnect, args),
        DesktopCommand::Recover(args) => (Action::Recover, args),
    };
    if matches!(action, Action::Connect | Action::Disconnect) {
        confirm_teams_restart(yes, None, "")?;
    }
    let port = match args.port {
        Some(port) => port,
        None => config::load(profile)?.1.desktop_port,
    };
    let value = connection::manage(&DesktopClient::new(port)?, action).await?;
    out.value(&value, || {
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
