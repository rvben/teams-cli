# teams-cli

Microsoft Teams from your terminal—human-first and agent-first.

`teams-cli` is a Rust CLI and TUI for everyday Microsoft Teams work through supported Microsoft Graph APIs, with an optional experimental desktop backend that reads chats from, and can send chat messages through, a running Teams client on Windows or WSL. It is designed as two equally first-class experiences over one typed core: a fast, calm terminal interface for people and a deterministic, introspectable command contract for agents. The default Graph backend signs in as you with delegated OAuth and protects refresh credentials in your operating system's credential store. Output is readable text on a TTY and structured JSON when piped.

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

## Terminal interface

`teams` (or `teams tui`) opens a keyboard-first chat reader: recent chats on the
left with unread markers, last-message previews and times, and the selected
conversation on the right, grouped by day and sender. Reads run in the
background and are cached per chat, so moving through the list never blocks.
Narrow terminals show one pane at a time, and `NO_COLOR` is respected.

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k` | Move through chats, or scroll the open conversation |
| `enter`, `→` / `esc`, `←` | Open a chat / return to the list |
| `tab` | Switch between the list and the conversation |
| `pgup`/`pgdn`, `g`/`G` | Page, or jump to the oldest/newest message |
| `o` | Load older messages in the open chat |
| `/` | Filter chats by name or last message |
| `r` | Refresh |
| `?` | Show every key |
| `q` | Quit |

The TUI is read-only and works with both the Graph and desktop backends.

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
Windows, or WSL, and to read chats (and, if you opt in, send chat messages) on
Windows or WSL:

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
# Windows/WSL: opt in to sending plain-text chat messages
teams init --profile local --backend desktop --allow-send --no-login
teams --profile local messages send --chat '19:...@thread.v2' --body 'On my way'
```

On Windows and WSL, `chats list` and `messages list --chat` read from the running
Teams client's local cache: the helper evaluates one fixed, read-only script in the
Teams page and returns projected fields (ids, topics, members, last-message previews,
read state, timestamps, senders, bodies), never tokens or raw records. Member and sender
names come from Teams' cached profiles, so 1:1 chats show the other person's name. Pages use the same shape and opaque `--cursor` as Graph
and add `"source": "desktop_cache"`. The cache only holds what Teams has synced, so
older history in a chat you never scrolled back through can be missing. Chat ids are
the same as Graph's; the chat with yourself is listed too, with the id `48:notes`, so
you can try sending there first. On macOS these commands return `unsupported`.

Desktop profiles are read-only unless created with `init --allow-send`;
`TEAMS_READ_ONLY=true` still blocks sending, but `TEAMS_READ_ONLY=false` cannot unlock a
desktop profile. With the opt-in, `messages send --chat` sends one plain-text message
through the running Teams window, the same way its Send button does: Teams performs the
request with its own sign-in, and nothing is shown or clicked in its window. Each send
asks for confirmation on a terminal and requires `--yes` otherwise. The listener is
verified again and a fresh connection to the Teams main window is used for every send,
and a send is never retried: if Teams does not answer, the command reports that the
message may still be delivered, so check the chat before sending again. Teams queues
the message like any other, so a send made just before Teams closes goes out when Teams
next starts. The result has the Graph send shape plus `"source": "desktop"`,
`clientMessageId` and Teams' own `status`. `doctor` checks that the send path is still
recognised without sending anything.

Teams, channels, identity, subscriptions, replies, mentions, attachments and edits
return `unsupported` on desktop; the TUI reads chats from the same cache and does not
send. Existing profiles without a backend field
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
loopback metadata. On Windows, `teams.exe` checks the listener itself through the
Windows APIs: every listener on the port must be loopback-only and descend from an
`ms-teams.exe` that belongs to the installed MSTeams package and runs as the invoking
Windows user. Under WSL, the Linux CLI starts the Windows build of the same version as
a helper (`teams.exe desktop-helper`) and keeps it running for the session, talking JSON
over its stdin and stdout, so NAT mode needs no networking changes. Diagnostics are
limited to `GET /json/version` and `GET /json/list`, and their results omit titles,
full URLs, target IDs, WebSocket URLs and private origins. Chat reads then attach to
the first Teams-origin page over the same verified loopback port and keep that
connection open; if it drops, the listener is verified again before the next read.

On first use under WSL, the helper is downloaded from the matching GitHub release,
checked against the release's `SHA256SUMS`, and cached in
`~/.cache/teams-cli/windows-helper/<version>/`. `doctor` reports whether it is
installed and never downloads it. Set `TEAMS_DESKTOP_HELPER` to use another
`teams.exe` of the same version, or run `make windows-helper` (needs
[cargo-xwin](https://github.com/rust-cross/cargo-xwin)) to build one for a
development checkout.

Endpoint discovery does not verify sign-in: `signed_in` stays `null`, `verified`
and `session_verified` stay `false`, and `endpoint_available` describes metadata
access only. `doctor.healthy` means the requested diagnostic checks passed; it does
not read chats. When any check fails, `doctor` still prints
the full report and then exits with code 7 (`unhealthy`). Offline commands do not contact the
endpoint. Desktop profiles ignore Graph OAuth environment settings and credentials;
`auth login`/`logout` return `unsupported` because Teams owns its session.
A CLI profile selects connection configuration, not a Teams account. There is no
automatic fallback to Graph.

Each read is bounded to 45 seconds. The WSL helper exits when the CLI does, though
Windows can keep it alive until an in-flight read completes.
Microsoft treats [WebView2 flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)
as development diagnostics and advises against relying on them in production.
This backend is experimental: chat reading depends on the Teams client's internal
cache layout and sending on its internal send interface, and an unrecognised layout
fails with `desktop_error` rather than guessing.

## Graph boundaries

- Microsoft 365 work or school accounts only. The relevant Teams Graph endpoints do not support personal Microsoft accounts.
- A work account can authenticate successfully before Microsoft Teams is licensed or provisioned; `teams doctor` distinguishes identity access from Teams availability.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens, browser interception, or direct calls to private Teams services; the experimental desktop backend reads the local client cache and sends only through the Teams window's own client.
- Tenant policy and admin consent can restrict delegated capabilities.

## CLI Spec

`teams schema` is offline, unauthenticated, and follows [CLI Spec v0.3](https://clispec.dev/): flat invocable command paths, declared effects and cardinality, bounded collections, stable error kinds and exit codes, stdout/stderr separation, and safe noninteractive behavior. Its backend capabilities and per-command backend extensions distinguish Graph operations from the experimental desktop backend.

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
