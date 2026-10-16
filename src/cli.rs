use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

use crate::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "teams",
    version,
    long_version = env!("TEAMS_LONG_VERSION"),
    about = "Microsoft Teams from your terminal, for humans and agents",
    after_help = "Get started:\n  teams init                         Connect a personal or work account\n  teams                              Reopen your inbox after setup
  teams tui --demo                   Explore without an account\n  teams doctor                       Diagnose configuration and access\n  teams schema --command 'messages send'\n                                     Inspect the automation contract"
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
    /// Skip confirmation prompts for destructive operations
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,
    /// Alias for --output json
    #[arg(long, global = true, hide = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Guided account setup; explicit flags configure a Graph or desktop profile
    Init(InitArgs),
    /// Manage delegated Microsoft authentication
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List, select, or remove configuration profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Inspect resolved, secret-free configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
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
    /// Find people to start a conversation with
    People {
        #[command(subcommand)]
        command: PeopleCommand,
    },
    /// Read and send messages
    Messages {
        #[command(subcommand)]
        command: MessagesCommand,
    },
    /// Open the keyboard-first terminal interface
    Tui(TuiArgs),
    /// Check the selected backend's configuration and access
    Doctor {
        /// Check configuration/platform without contacting Graph or the desktop endpoint
        #[arg(long)]
        offline: bool,
    },
    /// Manage a recoverable Teams desktop debugging session
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    /// Describe supported and deliberately unsupported capabilities
    Capabilities,
    /// Emit the offline CLI Spec v0.3 contract
    Schema {
        #[arg(long)]
        command: Option<String>,
    },
    /// Generate shell completions
    Completions { shell: Shell },
    /// Serve desktop reads for the Linux CLI under WSL over stdin and stdout
    #[command(hide = true, name = "desktop-helper")]
    DesktopHelper,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Graph credential store (default: OS store; WSL uses Windows DPAPI)
    #[arg(long, value_enum)]
    pub credential_store: Option<crate::config::CredentialStore>,
    /// Existing pass directory; defaults to PASSWORD_STORE_DIR or ~/.password-store and is saved
    #[arg(long)]
    pub pass_store_dir: Option<std::path::PathBuf>,
    /// Connection method (omitted: guided in a terminal, Graph in scripts)
    #[arg(long, value_enum)]
    pub backend: Option<crate::config::BackendKind>,
    /// Existing loopback debugging port for a desktop profile
    #[arg(long, value_parser = clap::value_parser!(u16).range(1024..))]
    pub desktop_port: Option<u16>,
    /// Select the personal or work/school Teams surface; auto refuses mixed surfaces
    #[arg(long, value_enum)]
    pub desktop_account: Option<crate::config::DesktopAccount>,
    /// Use the native Teams app or a dedicated Chrome/Edge browser profile
    #[arg(long, value_enum)]
    pub desktop_client: Option<crate::config::DesktopClientKind>,
    /// Override the maintained teams-cli public-client application ID
    #[arg(long)]
    pub client_id: Option<String>,
    /// Tenant ID/domain, or `organizations`
    #[arg(long)]
    pub tenant: Option<String>,
    /// Save configuration without sign-in or probing the desktop endpoint
    #[arg(long)]
    pub no_login: bool,
    /// Use device-code sign-in instead of browser PKCE
    #[arg(long)]
    pub device_code: bool,
    /// Request admin-consented access to read channel history
    #[arg(long)]
    pub channel_history: bool,
    /// Block remote write operations for this profile
    #[arg(long, env = "TEAMS_READ_ONLY")]
    pub read_only: bool,
    /// Allow a desktop profile to send chat messages (desktop profiles are read-only otherwise)
    #[arg(long)]
    pub allow_send: bool,
    /// Reopen this dedicated browser when you open the inbox (browser profiles only)
    #[arg(long)]
    pub auto_start: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved profile without credentials
    Show,
    /// Print the absolute configuration file path
    Path,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Sign in with delegated OAuth
    Login {
        /// Use device-code sign-in for headless and remote environments
        #[arg(long)]
        device_code: bool,
        /// Request admin-consented access to read channel history
        #[arg(long)]
        channel_history: bool,
    },
    /// Remove locally stored credentials
    Logout,
    /// Verify authentication and show the selected profile's status
    Status {
        /// Inspect configuration without contacting Graph or the desktop endpoint
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// List configured profiles and identify the active one
    List,
    /// Select the default profile for future commands
    Use { name: String },
    /// Remove a profile and its stored credential
    Remove { name: String },
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
    /// Create or reuse a one-to-one conversation; does not send a message
    Create {
        /// Exact person ID returned by `people search`
        #[arg(long)]
        recipient: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PeopleCommand {
    /// Search people by name or email, without sending anything
    Search {
        query: String,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=50))]
        limit: u16,
    },
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
    /// Check one send's exact client ID in the newest 50 cached desktop messages
    Status {
        #[arg(long)]
        chat: String,
        #[arg(long)]
        client_message_id: String,
    },
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

#[derive(Debug, Subcommand)]
pub enum DesktopCommand {
    /// Supervise application-owned browsers until stdin closes (integration protocol)
    Supervise {
        /// Preserve newly started browsers after the application exits
        #[arg(long)]
        keep_alive: bool,
    },
    /// Inspect endpoint metadata and managed-session recovery state
    Status(DesktopArgs),
    /// Open a debugging session (native Teams restart or dedicated browser launch)
    Connect(DesktopArgs),
    /// Sign in visibly, then automatically continue headless (browser only)
    Login(DesktopArgs),
    /// Close the dedicated browser, or restart native Teams normally
    Disconnect(DesktopArgs),
    /// Recover interrupted native settings or close a managed browser session
    Recover(DesktopArgs),
    /// Remove remote-debugging flags from the user-wide WebView2 setting (Windows/WSL)
    ClearFlags(DesktopArgs),
}

#[derive(Debug, Args)]
pub struct DesktopArgs {
    /// Override the configured desktop client
    #[arg(long, value_enum)]
    pub desktop_client: Option<crate::config::DesktopClientKind>,
    /// Open the browser visibly for sign-in (browser connect only)
    #[arg(long)]
    pub visible: bool,
    /// Override the selected profile's desktop port
    #[arg(long, value_parser = clap::value_parser!(u16).range(1024..))]
    pub port: Option<u16>,
}
