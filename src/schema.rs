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
            arg("--limit", "integer", "Maximum records in this page"),
            arg("--cursor", "string", "Opaque continuation URL from the previous page"),
            arg("--fields", "string", "Comma-separated output fields")
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
            arg("--limit", "integer", "Maximum records in this page"),
            arg("--offset", "integer", "Number of records to skip"),
            arg("--fields", "string", "Comma-separated output fields")
        ],
        "output_fields": fields
    })
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
                "Configure an Entra public client and optionally sign in",
                "idempotent",
                vec![
                    field("profile", "string"),
                    field("config_path", "string"),
                    field("signed_in", "boolean"),
                    field("client_id", "string"),
                    field("tenant", "string"),
                    field("channel_history_requested", "boolean"),
                ],
            );
            value["args"] = json!([
                arg(
                    "--client-id",
                    "string",
                    "Override the maintained teams-cli public-client application ID"
                ),
                arg("--tenant", "string", "Tenant ID, domain, or organizations"),
                arg(
                    "--no-login",
                    "boolean",
                    "Save configuration without signing in"
                ),
                arg("--device-code", "boolean", "Use device-code sign-in"),
                arg(
                    "--channel-history",
                    "boolean",
                    "Request admin-consented channel-history access"
                )
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
        single(
            "auth status",
            "Inspect local configuration and credential presence without network access",
            "read_only",
            vec![
                field("profile", "string"),
                field("configured", "boolean"),
                field("signed_in", "boolean"),
                field("config_path", "string"),
                nullable_array_field("granted_scopes", "string"),
                nullable_field("channel_history", "boolean"),
            ],
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
                field("id", "string"),
                field("chatType", "string"),
                nullable_field("topic", "string"),
                field("lastUpdatedDateTime", "string"),
            ],
        ),
        {
            let mut value = paged(
                "messages list",
                "List messages from one chat, or from a channel with optional admin-consented access",
                vec![
                    field("id", "string"),
                    field("createdDateTime", "string"),
                    field("from", "object"),
                    field("body", "object"),
                ],
            );
            let args = value["args"].as_array_mut().unwrap();
            args.splice(
                0..0,
                [
                    arg("--chat", "string", "Chat ID"),
                    arg("--team", "string", "Team ID"),
                    arg("--channel", "string", "Channel ID"),
                ],
            );
            value
        },
        {
            let mut value = single(
                "messages send",
                "Send one plain-text message as the signed-in user",
                "non_idempotent",
                vec![
                    field("id", "string"),
                    field("createdDateTime", "string"),
                    nullable_field("webUrl", "string"),
                ],
            );
            value["args"] = json!([
                arg("--chat", "string", "Chat ID"),
                arg("--team", "string", "Team ID"),
                arg("--channel", "string", "Channel ID"),
                required_arg("--body", "string", "Message body, or - for stdin")
            ]);
            value
        },
        json!({"name":"tui", "description":"Open the keyboard-first terminal interface", "effects":"read_only", "output_kind":"opaque", "media_type":"text/plain", "requires_tty":true, "args":[arg("--demo", "boolean", "Use deterministic sample data"), arg("--snapshot", "boolean", "Print one ANSI-free demo frame")]}),
        single(
            "doctor",
            "Check configuration, credential storage, Microsoft identity, and Teams provisioning",
            "read_only",
            vec![array_field("checks", "object"), field("healthy", "boolean")],
        ),
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
            {"name":"--profile", "type":"string", "description":"Configuration profile"},
            {"name":"--quiet", "type":"boolean", "description":"Suppress informational stderr output"}
        ],
        "commands": commands,
        "errors": error::ALL.iter().map(|e| json!({"kind":e.kind,"exit_code":e.exit_code,"retryable":e.retryable,"description":e.description})).collect::<Vec<_>>(),
        "extensions": {
            "authentication":"delegated_oauth",
            "api":"Microsoft Graph v1.0",
            "default_client_id": crate::config::DEFAULT_CLIENT_ID,
            "login_modes":["browser_pkce","device_code"],
            "baseline_scopes": crate::auth::BASE_SCOPES.split_whitespace().collect::<Vec<_>>(),
            "privileged_scopes":[crate::auth::CHANNEL_HISTORY_SCOPE]
        }
    })
}
