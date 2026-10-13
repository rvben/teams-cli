use serde_json::{Value, json};

use crate::error;

fn field(name: &str, kind: &str) -> Value {
    json!({"name": name, "type": kind})
}
fn array_field(name: &str, item_kind: &str) -> Value {
    json!({"name": name, "type": "array", "items": {"type": item_kind}})
}
fn nullable_field(name: &str, kind: &str) -> Value {
    json!({"name": name, "type": kind, "nullable": true})
}
fn nullable_array_field(name: &str, item_kind: &str) -> Value {
    json!({"name": name, "type": "array", "items": {"type": item_kind}, "nullable": true})
}
fn object_field(name: &str, nullable: bool, description: &str, fields: Vec<Value>) -> Value {
    json!({"name": name, "type": "object", "nullable": nullable, "description": description, "fields": fields})
}
fn described(mut field: Value, description: &str) -> Value {
    field["description"] = json!(description);
    field
}
fn debugging_setup() -> Value {
    object_field(
        "debugging_setup",
        true,
        "Commands for the user to run that open and close Teams' debugging port; only when nothing listens on the desktop port under Windows or WSL. Running them restarts Teams; the CLI never does",
        vec![
            described(
                field("shell", "string"),
                "wsl or powershell: where the commands run",
            ),
            field("open", "string"),
            field("close", "string"),
        ],
    )
}
fn sender() -> Value {
    object_field(
        "from",
        true,
        "Sender; null for system events",
        vec![object_field(
            "user",
            true,
            "The sending user",
            vec![
                field("id", "string"),
                nullable_field("displayName", "string"),
            ],
        )],
    )
}
fn body() -> Value {
    object_field(
        "body",
        false,
        "Message content",
        vec![
            json!({"name": "contentType", "type": "string", "enum": ["text", "html"]}),
            field("content", "string"),
        ],
    )
}
fn arg(name: &str, kind: &str, description: &str) -> Value {
    json!({"name": name, "type": kind, "description": description})
}
fn required_arg(name: &str, kind: &str, description: &str) -> Value {
    json!({"name": name, "type": kind, "description": description, "required": true})
}

fn single(name: &str, description: &str, effects: &str, fields: Vec<Value>) -> Value {
    json!({"name": name, "description": description, "effects": effects, "cardinality": "single", "output_fields": fields})
}

fn paged(name: &str, description: &str, fields: Vec<Value>) -> Value {
    json!({
        "name": name, "description": description, "effects": "read_only", "cardinality": "unbounded",
        "pagination": {"style": "cursor", "cursor_field": "next_cursor", "cursor_arg": "--cursor", "limit_arg": "--limit"},
        "fields_arg": "--fields",
        "args": [
            {"name": "--limit", "type": "integer", "default": 50, "x-minimum": 1, "x-maximum": 50, "description": "Maximum records in this page"},
            arg("--cursor", "string", "Opaque next_cursor value from the previous page; omit for the newest page"),
            arg("--fields", "string", "Comma-separated top-level output fields to keep; unknown names are omitted")
        ],
        "output_fields": fields
    })
}

fn offset_paged(name: &str, description: &str, fields: Vec<Value>) -> Value {
    json!({
        "name": name, "description": description, "effects": "read_only", "cardinality": "unbounded",
        "pagination": {"style": "offset", "offset_arg": "--offset", "limit_arg": "--limit"},
        "fields_arg": "--fields",
        "args": [
            {"name": "--limit", "type": "integer", "default": 50, "x-minimum": 1, "x-maximum": 100, "description": "Maximum records in this page"},
            {"name": "--offset", "type": "integer", "default": 0, "x-minimum": 0, "description": "Number of records to skip; use next_offset from the previous page"},
            arg("--fields", "string", "Comma-separated top-level output fields to keep; unknown names are omitted")
        ],
        "output_fields": fields
    })
}

fn conversation_args() -> Vec<Value> {
    vec![
        json!({"name": "--chat", "type": "string", "x-conflicts": ["--team", "--channel"], "description": "Chat ID from chats list; give this, or --team with --channel"}),
        json!({"name": "--team", "type": "string", "x-requires": ["--channel"], "description": "Team ID (Graph only)"}),
        json!({"name": "--channel", "type": "string", "x-requires": ["--team"], "description": "Channel ID within --team (Graph only)"}),
    ]
}

/// Error kinds each command can return, beyond `invalid_input` and `unexpected_error`,
/// which every command can.
fn command_errors(name: &str) -> &'static [&'static str] {
    const GRAPH_READ: &[&str] = &[
        "auth",
        "permission_denied",
        "not_found",
        "rate_limit",
        "api_error",
        "unsupported",
    ];
    const CONVERSATION_READ: &[&str] = &[
        "auth",
        "permission_denied",
        "not_found",
        "rate_limit",
        "api_error",
        "unsupported",
        "desktop_error",
    ];
    match name {
        "init" => &["auth", "tty_required", "desktop_error"],
        "auth login" => &["auth", "tty_required", "unsupported"],
        "auth logout" => &["unsupported"],
        "auth status" => &["auth", "rate_limit", "api_error", "desktop_error"],
        "profile remove" => &["confirmation_required"],
        "whoami" | "list" | "channels list" => GRAPH_READ,
        "chats list" | "messages list" => CONVERSATION_READ,
        "messages send" => &[
            "auth",
            "read_only",
            "permission_denied",
            "not_found",
            "rate_limit",
            "api_error",
            "confirmation_required",
            "unconfirmed",
            "unsupported",
            "desktop_error",
        ],
        "tui" => &["tty_required"],
        "doctor" => &["unhealthy"],
        "schema" => &["not_found"],
        _ => &[],
    }
}

fn example(name: &str) -> Value {
    let (args, stdin): (&[&str], Option<&str>) = match name {
        "init" => (&["init", "--no-login"], None),
        "auth login" => (&["auth", "login", "--device-code"], None),
        "auth logout" => (&["auth", "logout"], None),
        "auth status" => (&["auth", "status", "--offline"], None),
        "profile list" => (&["profile", "list"], None),
        "profile use" => (&["profile", "use", "work"], None),
        "profile remove" => (&["profile", "remove", "old", "--yes"], None),
        "config show" => (&["config", "show"], None),
        "config path" => (&["config", "path"], None),
        "whoami" => (&["whoami"], None),
        "list" => (&["list", "--limit", "20"], None),
        "channels list" => (&["channels", "list", "TEAM_ID"], None),
        "chats list" => (
            &[
                "chats",
                "list",
                "--limit",
                "20",
                "--fields",
                "id,topic,lastUpdatedDateTime",
            ],
            None,
        ),
        "messages list" => (
            &["messages", "list", "--chat", "CHAT_ID", "--limit", "20"],
            None,
        ),
        "messages send" => (
            &[
                "messages", "send", "--chat", "CHAT_ID", "--body", "-", "--yes",
            ],
            Some("On it."),
        ),
        "tui" => (&["tui", "--demo", "--snapshot"], None),
        "doctor" => (&["doctor", "--offline"], None),
        "capabilities" => (&["capabilities"], None),
        "schema" => (&["schema", "--command", "messages send"], None),
        "completions" => (&["completions", "bash"], None),
        _ => return Value::Null,
    };
    let mut value = json!({"args": args});
    if let Some(stdin) = stdin {
        value["stdin"] = json!(stdin);
    }
    value
}

pub fn generate(command_filter: Option<&str>) -> Value {
    let identity = vec![
        field("id", "string"),
        field("displayName", "string"),
        field("userPrincipalName", "string"),
        nullable_field("mail", "string"),
    ];
    let mut commands = vec![
        {
            let mut value = single(
                "init",
                "Configure a Graph or experimental desktop profile",
                "idempotent",
                vec![
                    field("profile", "string"),
                    field("backend", "string"),
                    field("config_path", "string"),
                    nullable_field("signed_in", "boolean"),
                    field("client_id", "string"),
                    field("tenant", "string"),
                    field("channel_history_requested", "boolean"),
                    field("read_only", "boolean"),
                    nullable_field("desktop_port", "integer"),
                    nullable_field("endpoint_available", "boolean"),
                    nullable_field("connection", "object"),
                    field("session_verified", "boolean"),
                    debugging_setup(),
                ],
            );
            value["args"] = json!([
                {"name":"--backend","type":"string","enum":["graph","desktop"],"default":"graph","description":"Connection method; desktop supports diagnostics, chats and opt-in chat sending"},
                {"name":"--desktop-port","type":"integer","description":"Existing loopback endpoint port (desktop only; default 9222)","x-minimum":1024,"x-maximum":65535},
                arg(
                    "--client-id",
                    "string",
                    "Override the maintained teams-cli public-client application ID"
                ),
                arg("--tenant", "string", "Tenant ID, domain, or organizations"),
                arg(
                    "--no-login",
                    "boolean",
                    "Save configuration without sign-in or desktop endpoint access"
                ),
                arg("--device-code", "boolean", "Use device-code sign-in"),
                arg(
                    "--channel-history",
                    "boolean",
                    "Request admin-consented channel-history access"
                ),
                {"name":"--read-only","type":"boolean","x-conflicts":["--allow-send"],"description":"Block remote write operations"},
                {"name":"--allow-send","type":"boolean","x-requires":{"--backend":"desktop"},"x-conflicts":["--read-only"],"description":"Allow a desktop profile to send chat messages (desktop profiles are read-only otherwise)"}
            ]);
            value
        },
        {
            let mut value = single(
                "auth login",
                "Sign in with delegated Microsoft OAuth and protect credentials in the OS store",
                "idempotent",
                vec![
                    field("profile", "string"),
                    field("expires_at", "integer"),
                    nullable_field("scope", "string"),
                    field("channel_history_requested", "boolean"),
                ],
            );
            value["args"] = json!([
                arg(
                    "--device-code",
                    "boolean",
                    "Use device-code sign-in for headless and remote environments"
                ),
                arg(
                    "--channel-history",
                    "boolean",
                    "Request admin-consented channel-history access"
                )
            ]);
            value
        },
        single(
            "auth logout",
            "Remove locally stored delegated credentials",
            "idempotent",
            vec![field("profile", "string"), field("signed_in", "boolean")],
        ),
        {
            let mut value = single(
                "auth status",
                "Verify authentication and inspect credential state",
                "read_only",
                vec![
                    field("profile", "string"),
                    field("configured", "boolean"),
                    field("backend", "string"),
                    nullable_field("signed_in", "boolean"),
                    field("verified", "boolean"),
                    nullable_field("identity", "object"),
                    field("config_path", "string"),
                    field("read_only", "boolean"),
                    nullable_array_field("granted_scopes", "string"),
                    nullable_field("channel_history", "boolean"),
                    nullable_field("endpoint_available", "boolean"),
                    nullable_field("connection", "object"),
                    field("session_verified", "boolean"),
                ],
            );
            value["args"] = json!([arg(
                "--offline",
                "boolean",
                "Inspect local state without contacting Graph or the desktop endpoint"
            )]);
            value
        },
        single(
            "profile list",
            "List configured profiles and identify the active one",
            "read_only",
            vec![
                json!({"name": "items", "type": "array", "items": {"type": "object", "fields": [
                    field("name", "string"),
                    field("active", "boolean"),
                    json!({"name": "backend", "type": "string", "enum": ["graph", "desktop"]}),
                    field("client_id", "string"),
                    field("tenant", "string"),
                    field("read_only", "boolean")
                ]}}),
                field("total", "integer"),
            ],
        ),
        {
            let mut value = single(
                "profile use",
                "Select the default profile for future commands",
                "idempotent",
                vec![field("profile", "string"), field("active", "boolean")],
            );
            value["args"] = json!([required_arg("name", "string", "Profile name")]);
            value
        },
        {
            let mut value = single(
                "profile remove",
                "Remove a profile and its stored credential",
                "idempotent",
                vec![field("profile", "string"), field("removed", "boolean")],
            );
            value["args"] = json!([required_arg("name", "string", "Profile name")]);
            value["confirmation_bypass_arg"] = json!("--yes");
            value
        },
        single(
            "config show",
            "Print the resolved profile without credentials",
            "read_only",
            vec![
                field("profile", "string"),
                field("backend", "string"),
                nullable_field("desktop_port", "integer"),
                field("client_id", "string"),
                field("tenant", "string"),
                field("read_only", "boolean"),
                field("config_path", "string"),
            ],
        ),
        single(
            "config path",
            "Print the absolute configuration file path",
            "read_only",
            vec![],
        ),
        single(
            "whoami",
            "Show the signed-in Microsoft 365 identity",
            "read_only",
            identity,
        ),
        offset_paged(
            "list",
            "List teams joined by the signed-in user",
            vec![
                field("id", "string"),
                field("displayName", "string"),
                nullable_field("description", "string"),
            ],
        ),
        {
            let mut value = offset_paged(
                "channels list",
                "List channels in a team",
                vec![
                    field("id", "string"),
                    field("displayName", "string"),
                    nullable_field("description", "string"),
                    field("membershipType", "string"),
                ],
            );
            value["args"]
                .as_array_mut()
                .unwrap()
                .insert(0, required_arg("team", "string", "Team ID"));
            value
        },
        paged(
            "chats list",
            "List one-to-one, group, and meeting chats",
            vec![
                described(
                    field("id", "string"),
                    "Chat ID for messages list/send; the chat with yourself is 48:notes on desktop",
                ),
                json!({"name": "chatType", "type": "string", "enum": ["oneOnOne", "group", "meeting"]}),
                nullable_field("topic", "string"),
                described(field("lastUpdatedDateTime", "string"), "RFC 3339"),
                json!({"name": "members", "type": "array", "items": {"type": "object", "fields": [
                    field("userId", "string"),
                    nullable_field("displayName", "string")
                ]}}),
                object_field(
                    "viewpoint",
                    true,
                    "The signed-in user's read state",
                    vec![nullable_field("lastMessageReadDateTime", "string")],
                ),
                object_field(
                    "lastMessagePreview",
                    true,
                    "Latest message; desktop only",
                    vec![
                        field("id", "string"),
                        field("createdDateTime", "string"),
                        sender(),
                        body(),
                    ],
                ),
            ],
        ),
        {
            let mut value = paged(
                "messages list",
                "List messages from one chat, or from a channel with optional admin-consented access",
                vec![
                    field("id", "string"),
                    described(
                        field("createdDateTime", "string"),
                        "RFC 3339; pages are newest first",
                    ),
                    sender(),
                    body(),
                    described(nullable_field("deleted", "boolean"), "Desktop only"),
                ],
            );
            let args = value["args"].as_array_mut().unwrap();
            args.splice(0..0, conversation_args());
            value
        },
        {
            let mut value = single(
                "messages send",
                "Send one plain-text message as the signed-in user",
                "non_idempotent",
                vec![
                    described(field("id", "string"), "Message ID"),
                    nullable_field("createdDateTime", "string"),
                    nullable_field("webUrl", "string"),
                    nullable_field("chatId", "string"),
                    described(
                        nullable_field("source", "string"),
                        "\"desktop\" for desktop sends",
                    ),
                    described(nullable_field("clientMessageId", "string"), "Desktop only"),
                    described(
                        nullable_field("status", "string"),
                        "Desktop only; Teams' own delivery label, e.g. Delivered",
                    ),
                ],
            );
            let mut args = conversation_args();
            args.push(json!({"name": "--body", "type": "string", "required": true, "x-max-length": crate::desktop::MAX_BODY,
                "description": "Plain-text message body, or - to read it from stdin; must not be blank"}));
            value["args"] = json!(args);
            value["confirmation_bypass_arg"] = json!("--yes");
            value
        },
        json!({"name":"tui", "description":"Open the keyboard-first terminal interface; sends a chat message only after an explicit confirmation, on a profile that allows writes", "effects":"non_idempotent", "output_kind":"opaque", "media_type":"text/plain", "requires_tty":true, "args":[arg("--demo", "boolean", "Use deterministic sample data"), arg("--snapshot", "boolean", "Print one ANSI-free demo frame")]}),
        {
            let mut value = single(
                "doctor",
                "Check configuration and the selected backend's access (desktop checks endpoint metadata)",
                "read_only",
                vec![
                    field("backend", "string"),
                    array_field("checks", "object"),
                    field("healthy", "boolean"),
                    nullable_field("connection", "object"),
                    nullable_field("endpoint_available", "boolean"),
                    field("session_verified", "boolean"),
                    debugging_setup(),
                ],
            );
            value["args"] = json!([arg(
                "--offline",
                "boolean",
                "Check local state without contacting Graph or the desktop endpoint"
            )]);
            value
        },
        single(
            "capabilities",
            "Describe supported and deliberately unsupported capabilities",
            "read_only",
            vec![
                array_field("supported", "string"),
                array_field("unsupported", "string"),
                array_field("account_types", "string"),
                field("api", "string"),
                array_field("notes", "string"),
                field("backends", "object"),
            ],
        ),
        json!({
            "name":"schema",
            "description":"Emit the offline CLI Spec v0.3 contract",
            "effects":"read_only",
            "cardinality":"single",
            "stdout_schema":{},
            "args":[arg("--command", "string", "Return only one complete command path")]
        }),
        json!({"name":"completions", "description":"Generate a shell completion script", "effects":"read_only", "output_kind":"opaque", "media_type":"text/plain", "args":[arg("shell", "string", "Shell name")]}),
    ];
    for command in &mut commands {
        let name = command["name"].as_str().unwrap_or_default().to_string();
        let mut errors = vec!["invalid_input", "unexpected_error"];
        errors.extend(command_errors(&name));
        command["errors"] = json!(errors);
        let example = example(&name);
        if !example.is_null() {
            command["example"] = example;
        }
        command["extensions"] = json!({"backends": match command["name"].as_str().unwrap_or_default() {
            "auth login" | "auth logout" | "whoami" | "list" | "channels list" => vec!["graph"],
            _ => vec!["graph", "desktop"],
        }});
        match command["name"].as_str().unwrap_or_default() {
            "tui" => {
                command["extensions"]["desktop"] = json!(
                    "Windows/WSL only; chats and chat messages from the Teams client's local cache; sending needs a profile created with --allow-send"
                )
            }
            "chats list" | "messages list" => {
                command["extensions"]["desktop"] = json!(
                    "Windows/WSL only; chats, not channels; read from the Teams client's local cache; pages carry source \"desktop_cache\""
                )
            }
            "messages send" => {
                command["extensions"]["desktop"] = json!(
                    "Windows/WSL only; plain-text chat messages through the running Teams window; needs a profile created with `init --allow-send`; prompts on a terminal and needs --yes otherwise; results carry source \"desktop\", clientMessageId and Teams' status"
                )
            }
            _ => {}
        }
    }
    if let Some(filter) = command_filter {
        commands.retain(|value| value["name"] == filter);
    }
    json!({
        "$schema": "https://clispec.dev/schema/v0.3.json",
        "clispec": "0.3",
        "name": "teams",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "A Microsoft Teams CLI and TUI designed equally for humans and agents",
        "output": {"tty": "text", "piped": "json"},
        "global_args": [
            {"name":"--output", "short":"-o", "type":"string", "enum":["auto","text","json"], "default":"auto", "description":"Output format"},
            {"name":"--profile", "type":"string", "x-env":"TEAMS_PROFILE", "description":"Configuration profile; defaults to the active one"},
            {"name":"--quiet", "type":"boolean", "description":"Suppress informational stderr output"},
            {"name":"--yes", "short":"-y", "type":"boolean", "description":"Confirm without prompting (desktop sends, profile removal)"}
        ],
        "commands": commands,
        "errors": error::ALL.iter().map(|e| json!({"kind":e.kind,"exit_code":e.exit_code,"retryable":e.retryable,"description":e.description})).collect::<Vec<_>>(),
        "extensions": {
            "backends":backend_capabilities(),
            "authentication":"delegated_oauth",
            "api":"Microsoft Graph v1.0",
            "default_client_id": crate::config::DEFAULT_CLIENT_ID,
            "login_modes":["browser_pkce","device_code"],
            "read_only_env":"TEAMS_READ_ONLY",
            "environment":{
                "TEAMS_PROFILE":"Profile to use when --profile is not given",
                "TEAMS_READ_ONLY":"true blocks every remote write; false cannot unlock a desktop profile",
                "TEAMS_ACCESS_TOKEN":"Graph access token that overrides the stored credential for this run",
                "TEAMS_CLIENT_ID":"Graph application ID override",
                "TEAMS_TENANT":"Graph tenant override",
                "TEAMS_DESKTOP_HELPER":"Path to a teams.exe speaking the same helper protocol to use as the WSL desktop helper"
            },
            "collection_envelope":{
                "items":"array of records described by the command's output_fields",
                "next_cursor":"cursor-paged commands: pass to --cursor for the next page; absent on the last page",
                "next_offset":"offset-paged commands: pass to --offset for the next page; absent on the last page",
                "truncated":"true when more records exist",
                "source":"\"desktop_cache\" when records come from the Teams client's local cache; absent for Graph"
            },
            "error_envelope":"stderr carries {\"error\":{\"kind\",\"message\",\"retryable\"}} when stdout is not a terminal or --output json is given; rate_limit adds retry_after_seconds when Microsoft sends it",
            "idempotency":"Sends cannot be made idempotent: neither Graph nor Teams accepts a caller-supplied key. Never resend after unconfirmed without reading the conversation first.",
            "baseline_scopes": crate::auth::BASE_SCOPES.split_whitespace().collect::<Vec<_>>(),
            "privileged_scopes":[crate::auth::CHANNEL_HISTORY_SCOPE]
        }
    })
}

pub fn backend_capabilities() -> Value {
    json!({
        "graph":{"default":true,"authentication":"delegated_oauth","api":"Microsoft Graph v1.0","account_types":["Microsoft 365 work or school"]},
        "desktop":{"experimental":true,"read_only_by_default":true,"platforms":["macos","windows","wsl"],
            "account_types":["Microsoft 365 work or school","personal Microsoft accounts"],"account_selection":"single_account_only",
            "authentication":"teams_desktop_session_unverified","supported":["configuration","connection metadata","doctor","auth status","chats (windows, wsl)","chat messages (windows, wsl)","send plain-text chat messages (windows, wsl; opt-in)","TUI (windows, wsl)"],
            "unsupported":["teams and channels","identity","channel messages, replies, mentions, attachments and edits","subscriptions","automatic Teams launch or restart"],
            "requires":"existing Teams-owned loopback debugging endpoint; WSL requires Windows executable interop",
            "transport":{"macos":"native_loopback","windows":"windows_native","wsl":"windows_helper_stdio"},
            "chat_source":"desktop_cache","session_verified":false,"default_port":crate::config::DEFAULT_DESKTOP_PORT}
    })
}
