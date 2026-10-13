# teams-cli

Microsoft Teams from your terminal—human-first and agent-first.

`teams-cli` is a Rust CLI and TUI for everyday Microsoft Teams work through supported Microsoft Graph APIs, with an optional experimental desktop backend that reads chats from a running Teams client on Windows or WSL. It is designed as two equally first-class experiences over one typed core: a fast, calm terminal interface for people and a deterministic, introspectable command contract for agents. The default Graph backend signs in as you with delegated OAuth and protects refresh credentials in your operating system's credential store. Output is readable text on a TTY and structured JSON when piped.

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

## Experimental desktop backend

Use a desktop profile to inspect an existing Teams debugging endpoint on macOS,
Windows, or WSL, and to read chats on Windows or WSL:

```console
teams init --profile local --backend desktop --read-only --no-login
teams --profile local doctor --offline
teams --profile local doctor
teams --profile local auth status
# Match a separately enabled endpoint on another port:
teams init --profile local --backend desktop --desktop-port 19222 --no-login
# Windows/WSL: read chats and chat messages
teams --profile local chats list --limit 20
teams --profile local messages list --chat '19:...@thread.v2'
```

On Windows and WSL, `chats list` and `messages list --chat` read from the running
Teams client's local cache: the helper evaluates one fixed, read-only script in the
Teams page and returns projected fields (ids, topics, timestamps, senders, bodies),
never tokens or raw records. Pages use the same shape and opaque `--cursor` as Graph
and add `"source": "desktop_cache"`. The cache only holds what Teams has synced, so
older history in a chat you never scrolled back through can be missing. Chat ids are
the same as Graph's. On macOS these commands return `unsupported`.

Teams, channels, identity, subscriptions, sends, and live TUI data return
`unsupported` on desktop. Demo/snapshot TUI modes remain available. Desktop profiles are always read-only,
including when `TEAMS_READ_ONLY=false`. Existing profiles without a backend field
continue to use Graph; Graph remains the default for `init`.

The endpoint must already be enabled through an explicit Teams debugging launch.
On Windows, for example, set a user environment variable
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`, quit Teams
fully and start it again; remove the variable and restart Teams to close the port.
These commands never launch, restart, quit, or sign into Teams, and never enable
debugging. `init --no-login` saves configuration without probing; ordinary desktop
initialization probes the existing endpoint. A normal Teams launch usually has no
endpoint, which `doctor` reports as `no_listener`.

macOS checks listener binding and installed Teams process ancestry before reading
loopback metadata. Windows uses a fixed PowerShell helper with JSON stdin/stdout;
WSL invokes that Windows helper through executable interop, so NAT mode needs no
networking changes. Windows requires the installed MSTeams package and
`powershell.exe` on PATH. Both paths refuse non-loopback listeners and unrelated
owners. The Windows reader also verifies the Teams process belongs to the invoking
Windows user. Diagnostics are limited to `GET /json/version` and `GET /json/list`, and their
results omit titles, full URLs, target IDs, WebSocket URLs and private origins.
Chat reads then attach to the first Teams-origin page over the same verified
loopback port.

Endpoint discovery does not verify sign-in: `signed_in` stays `null`, `verified`
and `session_verified` stay `false`, and `endpoint_available` describes metadata
access only. `doctor.healthy` means the requested diagnostic checks passed; it does
not read chats. When any check fails, `doctor` still prints
the full report and then exits with code 7 (`unhealthy`). Offline commands do not contact the
endpoint. Desktop profiles ignore Graph OAuth environment settings and credentials;
`auth login`/`logout` return `unsupported` because Teams owns its session.
A CLI profile selects connection configuration, not a Teams account. There is no
automatic fallback to Graph.

The helper timeout is 45 seconds. Cancellation stops the local helper, but WSL can
retain a Windows-side helper process until its own bounded request completes.
Microsoft treats [WebView2 flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)
as development diagnostics and advises against relying on them in production.
This backend is experimental: chat reading depends on the Teams client's internal
cache layout, and an unrecognised layout fails with `desktop_error` rather than
guessing.

Windows CI runs synthetic tests against the production PowerShell reader. To run
them elsewhere with an installed PowerShell:

```console
TEAMS_TEST_POWERSHELL=/path/to/pwsh cargo test --test desktop_bridge -- --ignored
```

## Graph boundaries

- Microsoft 365 work or school accounts only. The relevant Teams Graph endpoints do not support personal Microsoft accounts.
- A work account can authenticate successfully before Microsoft Teams is licensed or provisioned; `teams doctor` distinguishes identity access from Teams availability.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens, browser interception, or private Teams service calls; the experimental desktop backend only reads the local client cache.
- Tenant policy and admin consent can restrict delegated capabilities.

## CLI Spec

`teams schema` is offline, unauthenticated, and follows [CLI Spec v0.3](https://clispec.dev/): flat invocable command paths, declared effects and cardinality, bounded collections, stable error kinds and exit codes, stdout/stderr separation, and safe noninteractive behavior. Its backend capabilities and per-command backend extensions distinguish Graph operations from the experimental desktop backend.

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
