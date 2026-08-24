# teams-cli

Microsoft Teams from your terminal—human-first and agent-first.

`teams-cli` is a Rust CLI and TUI for everyday Microsoft Teams work through supported Microsoft Graph APIs. It is designed as two equally first-class experiences over one typed core: a fast, calm terminal interface for people and a deterministic, introspectable command contract for agents. It signs in as you with delegated OAuth, protects refresh credentials in your operating system's credential store, emits readable terminal output on a TTY, and defaults to structured JSON when piped.

For humans, no arguments open guided onboarding on first use and the TUI afterward. For agents, no command ever waits for an undeclared prompt: `teams schema` works offline, collection sizes are caller-bounded, writes declare their effects, errors have stable kinds and exit codes, and stdout contains data only.

## Install

```console
pipx install teams-cli
# or
cargo install teams-cli --locked
```

The PyPI distribution is named `teams-cli`; the executable is `teams`, matching the `jira-cli` → `jira` and `confluence-cli` → `confluence` family.

To work from source:

```console
cargo install --path .
# or build a local wheel
maturin build
```

## Explore before configuring Microsoft 365

```console
teams tui --demo
teams tui --demo --snapshot
teams capabilities
teams schema --command "messages send"
```

## Onboarding

Run `teams` in a terminal on first use, or start explicitly:

```console
teams init
```

You need a Microsoft Entra public-client application:

1. In [Microsoft Entra app registrations](https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade), register an app for organizational accounts.
2. Under **Authentication**, add the **Mobile and desktop applications** redirect URI `http://localhost` and enable public client flows.
3. Under **API permissions**, add these delegated Microsoft Graph permissions (plus the standard OpenID scopes requested at sign-in):

```text
openid profile offline_access User.Read Team.ReadBasic.All
Channel.ReadBasic.All Chat.Read Chat.Create ChatMessage.Send
ChannelMessage.Send ChannelMessage.Read.All
```

Reading channel messages requires the administrator-consented `ChannelMessage.Read.All` permission. If the tenant has not granted it, Microsoft may require an administrator during sign-in. The CLI surfaces missing consent as a stable `permission_denied` error instead of silently reaching for undocumented Teams APIs.

Headless setup is explicit and never prompts:

```console
teams init --client-id "$TEAMS_CLIENT_ID" --tenant organizations --no-login
teams auth login --device-code
teams whoami --output json
```

For short-lived automation, `TEAMS_ACCESS_TOKEN` overrides the stored credential. Refresh credentials are never written to the config file.

## Everyday commands

```console
teams list
teams channels list TEAM_ID
teams chats list --limit 25
teams messages list --chat CHAT_ID
teams messages send --chat CHAT_ID --body "On it."
printf 'Status update' | teams messages send --chat CHAT_ID --body -
teams doctor
```

Collection commands expose `--limit` and `--fields`; Graph-native collections use `--cursor`, while joined teams and channels use `--offset` because those endpoints do not accept `$top`. With a terminal they render concise text; when stdout is piped they emit JSON envelopes with continuation metadata and `truncated`.

## Boundaries

- Microsoft 365 work or school accounts only. The relevant Teams Graph endpoints do not support personal Microsoft accounts.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens, browser interception, or private Teams protocols.
- Tenant policy and admin consent can restrict delegated capabilities.

## CLI Spec

`teams schema` is offline, unauthenticated, and follows [CLI Spec v0.3](https://clispec.dev/): flat invocable command paths, declared effects and cardinality, bounded collections, stable error kinds and exit codes, stdout/stderr separation, and safe noninteractive behavior.
