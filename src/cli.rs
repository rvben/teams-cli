use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

use crate::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "teams",
    version,
    about = "Microsoft Teams from your terminal, for humans and agents",
    after_help = "Get started:\n  teams init                         Guided setup\n  teams tui --demo                   Explore without an account\n  teams doctor                       Diagnose configuration and access\n  teams schema --command 'messages send'\n                                     Inspect the automation contract"
)]
pub struct Cli {
    /// Config profile to use
    #[arg(long, global = true, env = "TEAMS_PROFILE")]
    pub profile: Option<String>,
    /// Output format; auto is text on a terminal and JSON when piped
    #[arg(short = 'o', long, global = true, value_enum, default_value = "auto")]
    pub output: OutputFormat,
    /// Suppress progress and informational messages
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Alias for --output json
    #[arg(long, global = true, hide = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Guided Microsoft Entra app and account setup
    Init(InitArgs),
    /// Manage delegated Microsoft authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Show the signed-in Microsoft 365 identity
    Whoami,
    /// List teams joined by the signed-in user
    List(OffsetPageArgs),
    /// Work with team channels
    Channels {
        #[command(subcommand)]
        command: ChannelsCommand,
    },
    /// Work with one-to-one and group chats
    Chats {
        #[command(subcommand)]
        command: ChatsCommand,
    },
    /// Read and send messages
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    /// Open the keyboard-first terminal interface
    Tui(TuiArgs),
    /// Check configuration, credential storage, and Graph access
    Doctor,
    /// Describe supported and deliberately unsupported capabilities
    Capabilities,
    /// Emit the offline CLI Spec v0.3 contract
    Schema {
        #[arg(long)]
        command: Option<String>,
    },
    /// Generate shell completions
    Completions { shell: Shell },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Public-client application (client) ID
    #[arg(long, env = "TEAMS_CLIENT_ID")]
    pub client_id: Option<String>,
    /// Tenant ID/domain, or `organizations`
    #[arg(long, default_value = "organizations", env = "TEAMS_TENANT")]
    pub tenant: String,
    /// Save configuration without starting sign-in
    #[arg(long)]
    pub no_login: bool,
    /// Use device-code sign-in instead of browser PKCE
    #[arg(long)]
    pub device_code: bool,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in with delegated OAuth
    Login {
        #[arg(long)]
        device_code: bool,
    },
    /// Remove locally stored credentials
    Logout,
    /// Show local authentication status without network access
    Status,
}

#[derive(Debug, Subcommand)]
pub enum ChannelsCommand {
    /// List channels in a team
    List {
        team: String,
        #[command(flatten)]
        page: OffsetPageArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum ChatsCommand {
    List(PageArgs),
}

#[derive(Debug, Args, Clone)]
pub struct PageArgs {
    /// Maximum records in this page
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=50))]
    pub limit: u16,
    /// Opaque continuation URL from a previous response
    #[arg(long)]
    pub cursor: Option<String>,
    /// Comma-separated output fields
    #[arg(long)]
    pub fields: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct OffsetPageArgs {
    /// Maximum records in this page
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: u16,
    /// Number of records to skip
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Comma-separated output fields
    #[arg(long)]
    pub fields: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum MessagesCommand {
    /// List messages from a chat or channel
    List {
        #[arg(long, conflicts_with = "team", required_unless_present = "team")]
        chat: Option<String>,
        #[arg(long, requires = "channel", required_unless_present = "chat")]
        team: Option<String>,
        #[arg(long, requires = "team")]
        channel: Option<String>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Send a message as the signed-in user
    Send {
        #[arg(long, conflicts_with = "team", required_unless_present = "team")]
        chat: Option<String>,
        #[arg(long, requires = "channel", required_unless_present = "chat")]
        team: Option<String>,
        #[arg(long, requires = "team")]
        channel: Option<String>,
        /// Plain-text message body; use '-' to read stdin
        #[arg(long)]
        body: String,
    },
}

#[derive(Debug, Args)]
pub struct TuiArgs {
    /// Use deterministic sample data; no account or network required
    #[arg(long)]
    pub demo: bool,
    /// Print one ANSI-free frame instead of taking over the terminal
    #[arg(long, requires = "demo")]
    pub snapshot: bool,
    #[arg(long, default_value_t = 100, hide = true)]
    pub width: u16,
    #[arg(long, default_value_t = 30, hide = true)]
    pub height: u16,
}
