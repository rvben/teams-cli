use std::io::{self, IsTerminal, Read, Write};

use clap::{CommandFactory, Parser};
use serde::Serialize;
use serde_json::Value;

use teams_cli::auth;
use teams_cli::cli::{
    AuthCommand, ChannelsCommand, ChatsCommand, Cli, Command, InitArgs, MessagesCommand,
    OffsetPageArgs, PageArgs, TuiArgs,
};
use teams_cli::config::{self, Profile};
use teams_cli::error::AppError;
use teams_cli::graph::{self, GraphClient, Page};
use teams_cli::output::{Output, OutputFormat, print_error, structured_from_args};
use teams_cli::{schema, tui};

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
        format: if cli.json {
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
    let command = match cli.command {
        Some(command) => command,
        None if io::stdin().is_terminal() && io::stdout().is_terminal() && !config::exists() => {
            Command::Init(InitArgs {
                client_id: None,
                tenant: "organizations".into(),
                no_login: false,
                device_code: false,
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
    match command {
        Command::Init(args) => init(profile_arg.unwrap_or("default"), args, out).await,
        Command::Auth { command } => auth_command(profile_arg, command, out).await,
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
            render_page(&mut result, &page, out)
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
            render_page(&mut result, &page, out)
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
            let body = read_body(&body)?;
            if body.trim().is_empty() {
                return Err(AppError::InvalidInput(
                    "message body cannot be empty".into(),
                ));
            }
            let client = client(profile_arg).await?;
            let value = client
                .send(chat.as_deref(), team.as_deref(), channel.as_deref(), &body)
                .await?;
            out.value(&value, || {
                format!(
                    "Sent message {}",
                    string_at(&value, "/id").unwrap_or("successfully")
                )
            })
        }
        Command::Tui(args) => tui_command(profile_arg, args).await,
        Command::Doctor => doctor(profile_arg, out).await,
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

async fn init(profile_name: &str, args: InitArgs, out: Output) -> Result<(), AppError> {
    let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
    if interactive && !out.json() {
        eprintln!("\n  teams\n  Microsoft Teams, without leaving your flow.\n");
        eprintln!("  Before we sign in, create a Microsoft Entra public-client app.");
        eprintln!(
            "  1. Open https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade"
        );
        eprintln!("  2. Register a multitenant app for organizational accounts.");
        eprintln!("  3. Add Mobile and desktop redirect URI: http://localhost");
        eprintln!(
            "  4. Enable public client flows and grant the delegated permissions in README.md."
        );
        eprintln!(
            "  Work or school accounts are supported; personal Microsoft accounts are not.\n"
        );
    }
    let client_id = match args.client_id {
        Some(value) => value,
        None if interactive => prompt("Application (client) ID")?,
        None => return Err(AppError::NonInteractive("--client-id is required without a terminal; example: `teams init --client-id <uuid> --no-login`".into())),
    };
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
            client_id,
            tenant: args.tenant,
        },
    )?;
    let mut signed_in = false;
    if !args.no_login {
        let (_, profile) = config::load(Some(profile_name))?;
        auth::login(profile_name, &profile, args.device_code, &out).await?;
        signed_in = true;
    }
    #[derive(Serialize)]
    struct InitResult<'a> {
        profile: &'a str,
        config_path: String,
        signed_in: bool,
    }
    let result = InitResult {
        profile: profile_name,
        config_path: path.display().to_string(),
        signed_in,
    };
    out.value(&result, || {
        if signed_in {
            "You’re ready. Run `teams` to open your inbox.".into()
        } else {
            format!("Saved {}. Next: `teams auth login`", path.display())
        }
    })
}

async fn auth_command(
    profile_arg: Option<&str>,
    command: AuthCommand,
    out: Output,
) -> Result<(), AppError> {
    match command {
        AuthCommand::Login { device_code } => {
            if !device_code && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
                return Err(AppError::NonInteractive(
                    "browser sign-in requires a terminal; use `teams auth login --device-code` for headless sign-in"
                        .into(),
                ));
            }
            let (name, profile) = config::load(profile_arg)?;
            let token = auth::login(&name, &profile, device_code, &out).await?;
            let value = serde_json::json!({"profile":name,"expires_at":token.expires_at,"scope":token.scope});
            out.value(&value, || format!("Signed in profile '{name}'."))
        }
        AuthCommand::Logout => {
            let (name, _) = config::load(profile_arg)?;
            auth::logout(&name)?;
            let value = serde_json::json!({"profile":name,"signed_in":false});
            out.value(&value, || format!("Signed out profile '{name}'."))
        }
        AuthCommand::Status => {
            let configured = config::configured_profile(profile_arg);
            let name = configured
                .as_ref()
                .map(|(n, _)| n.as_str())
                .unwrap_or(profile_arg.unwrap_or("default"));
            let signed_in = auth::has_token(name);
            let value = serde_json::json!({"profile":name,"configured":configured.is_some(),"signed_in":signed_in,"config_path":config::path()});
            out.value(&value, || {
                format!(
                    "Profile: {name}\nConfigured: {}\nSigned in: {}\nConfig: {}",
                    yes_no(configured.is_some()),
                    yes_no(signed_in),
                    config::path().display()
                )
            })
        }
    }
}

async fn client(profile_arg: Option<&str>) -> Result<GraphClient, AppError> {
    let (name, profile) = config::load(profile_arg)?;
    let token = auth::access_token(&name, &profile).await?;
    Ok(GraphClient::new(token))
}

fn render_page(page: &mut Page, args: &PageArgs, out: Output) -> Result<(), AppError> {
    graph::select_fields(page, args.fields.as_deref())?;
    let continuation = page.next_cursor.as_deref().unwrap_or("<cursor>");
    render_page_text(page, out, "--cursor", continuation)
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
        return tui::run(tui::demo_data());
    }
    let graph = client(profile_arg).await?;
    let me = graph.me().await?;
    let teams = graph.teams(50, 0).await?;
    let mut conversations: Vec<tui::Conversation> = teams
        .items
        .into_iter()
        .map(|team| tui::Conversation {
            name: team
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or("Unnamed team")
                .into(),
            unread: 0,
            messages: Vec::new(),
        })
        .collect();
    if conversations.is_empty() {
        conversations.push(tui::Conversation {
            name: "No joined teams".into(),
            unread: 0,
            messages: vec![],
        });
    }
    tui::run(tui::TuiData {
        account: string_at(&me, "/displayName")
            .unwrap_or("Microsoft 365")
            .into(),
        conversations,
    })
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

async fn doctor(profile_arg: Option<&str>, out: Output) -> Result<(), AppError> {
    let mut checks = Vec::new();
    let configured = config::configured_profile(profile_arg);
    checks.push(Check {
        name: "configuration",
        status: if configured.is_some() { "pass" } else { "fail" },
        detail: config::path().display().to_string(),
    });
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
    let mut graph_ok = false;
    let detail = if configured.is_some() && signed_in {
        match client(profile_arg).await {
            Ok(client) => match client.me().await {
                Ok(value) => {
                    graph_ok = true;
                    format!(
                        "signed in as {}",
                        string_at(&value, "/userPrincipalName")
                            .or_else(|| string_at(&value, "/displayName"))
                            .unwrap_or("unknown user")
                    )
                }
                Err(error) => error.to_string(),
            },
            Err(error) => error.to_string(),
        }
    } else {
        "skipped until setup is complete".into()
    };
    checks.push(Check {
        name: "microsoft_graph",
        status: if graph_ok { "pass" } else { "fail" },
        detail,
    });
    let healthy = checks.iter().all(|check| check.status == "pass");
    let value = serde_json::json!({"checks":checks,"healthy":healthy});
    out.value(&value, || {
        checks
            .iter()
            .map(|check| format!("{:<18} {:<4}  {}", check.name, check.status, check.detail))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn capabilities(out: Output) -> Result<(), AppError> {
    let value = serde_json::json!({
        "supported":["delegated work/school sign-in","identity","joined teams","channels","chats","chat messages","channel messages","send messages","TUI"],
        "unsupported":["personal Microsoft accounts","user calls and screen sharing","private Teams protocols","tenant administration"],
        "account_types":["Microsoft 365 work or school"],
        "api":"Microsoft Graph v1.0",
        "notes":["Reading channel messages can require admin consent for ChannelMessage.Read.All.","Calls require bot/application APIs and are outside this user client."]
    });
    out.value(&value, || "Supported\n  Work/school delegated sign-in; teams, channels, chats, messages; send as yourself; TUI\n\nNot supported\n  Personal Microsoft accounts; calls or screen sharing; private Teams protocols; tenant administration\n\nRun `teams schema` for the machine contract.".into())
}

fn prompt(label: &str) -> Result<String, AppError> {
    eprint!("  {label}: ");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().into())
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
fn identity_text(value: &Value) -> String {
    format!(
        "{}\n{}",
        string_at(value, "/displayName").unwrap_or("Unknown user"),
        string_at(value, "/userPrincipalName")
            .or_else(|| string_at(value, "/mail"))
            .unwrap_or("")
    )
}
