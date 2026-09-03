# teams-cli

Microsoft Teams from your terminal—human-first and agent-first.

`teams-cli` is a Rust CLI and TUI for everyday Microsoft Teams work through supported Microsoft Graph APIs. It is designed as two equally first-class experiences over one typed core: a fast, calm terminal interface for people and a deterministic, introspectable command contract for agents. It signs in as you with delegated OAuth, protects refresh credentials in your operating system's credential store, emits readable terminal output on a TTY, and defaults to structured JSON when piped.

For humans, no arguments open guided onboarding on first use and the TUI afterward. For agents, no command ever waits for an undeclared prompt: `teams schema` works offline, collection sizes are caller-bounded, writes declare their effects, errors have stable kinds and exit codes, and stdout contains data only.

## Install

```console
pipx install teams-cli
# or
cargo install teams-cli --locked
# or run from PyPI without installing
uvx teams-cli --help
```

The PyPI distribution provides both `teams` and `teams-cli` as executable names.

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

`teams-cli` includes a maintained multitenant Microsoft Entra public-client registration, so normal setup does not require creating an app or copying a client ID:

```console
teams init
```

Browser PKCE is the human-friendly default. The browser talks directly to Microsoft, and the CLI never receives or stores your password. Normal sign-in requests these delegated scopes:

```text
openid profile offline_access User.Read Team.ReadBasic.All
Channel.ReadBasic.All Chat.Read Chat.Create ChatMessage.Send
ChannelMessage.Send
```

The TUI is also the recovery path: an unconfigured profile, missing credential,
expired refresh session, or Microsoft `401` opens a focused connection screen.
Press `Enter` or `a` to restore the normal terminal, complete Microsoft sign-in,
and return directly to the inbox; `q` exits without changing anything.

Reading channel history is intentionally separate because `ChannelMessage.Read.All` requires administrator consent. Request it only when needed:

```console
teams auth login --channel-history
```

The CLI surfaces missing consent as a stable `permission_denied` error instead of silently reaching for undocumented Teams APIs. Tenant policy can still require an administrator to approve otherwise user-consentable permissions.

Headless setup is explicit and never prompts:

```console
teams init --no-login
teams auth login --device-code
teams whoami --output json
```

Organizations that prefer their own Entra registration can override the bundled application:

```console
teams init --client-id "$TEAMS_CLIENT_ID" --tenant organizations --no-login
```

The custom app must be a public client with `http://localhost` registered for browser login, public-client flows enabled for device-code login, and the baseline delegated permissions listed above.

For short-lived automation, `TEAMS_ACCESS_TOKEN` overrides the stored credential. Refresh credentials are never written to the config file.

## Everyday commands

```console
teams list
teams channels list TEAM_ID
teams chats list --limit 25
teams messages list --chat CHAT_ID
teams auth login --channel-history  # once, with administrator approval
teams messages list --team TEAM_ID --channel CHANNEL_ID
teams messages send --chat CHAT_ID --body "On it."
printf 'Status update' | teams messages send --chat CHAT_ID --body -
teams auth status
teams auth status --offline
teams profile list
teams profile use work
teams profile remove old --yes
teams config show
teams config path
teams doctor --offline
```

Collection commands expose `--limit` and `--fields`; Graph-native collections use `--cursor`, while joined teams and channels use `--offset` because those endpoints do not accept `$top`. With a terminal they render concise text; when stdout is piped they emit JSON envelopes with continuation metadata and `truncated`.

Profiles can block every remote write while keeping reads, sign-in, diagnostics, and the TUI available:

```console
teams init --no-login --read-only
# or for one invocation/environment
TEAMS_READ_ONLY=true teams messages send --chat CHAT_ID --body "Blocked"
```

Like the other tools in the suite, the config file is selected with `--profile`, overridden by `TEAMS_*` environment variables, and inspectable without exposing credentials through `teams config show` and `teams config path`.

## Boundaries

- Microsoft 365 work or school accounts only. The relevant Teams Graph endpoints do not support personal Microsoft accounts.
- A work account can authenticate successfully before Microsoft Teams is licensed or provisioned; `teams doctor` distinguishes identity access from Teams availability.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens, browser interception, or private Teams protocols.
- Tenant policy and admin consent can restrict delegated capabilities.

## CLI Spec

`teams schema` is offline, unauthenticated, and follows [CLI Spec v0.3](https://clispec.dev/): flat invocable command paths, declared effects and cardinality, bounded collections, stable error kinds and exit codes, stdout/stderr separation, and safe noninteractive behavior.

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
