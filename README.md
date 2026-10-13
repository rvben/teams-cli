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

Development checks (`make check` or `cargo test --all-targets`) also need Python 3
and Node.js 20 or newer for release-smoke and synthetic desktop-script fixtures.

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
background and are cached per chat, so moving through the list never blocks, and
chats refresh on their own without losing your place: on the desktop backend the open
chat every 3 seconds and the list every 15, on Graph every 15 seconds and every minute. The header always shows the profile and account,
whether it can send (`✎ can send` or `read-only`), a status dot (`● live`, or
`● stale since` the last good read while refreshes fail), and your chat and unread
counts. Narrow terminals show one
pane at a time, and `NO_COLOR` is respected.

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k` | Move through chats, or scroll the open conversation |
| `enter`, `→` / `esc`, `←` | Open a chat / return to the list |
| `tab` | Switch between the list and the conversation |
| `pgup`/`pgdn`, `g`/`G` | Page, or jump to the oldest/newest message |
| `o` | Load older messages in the open chat |
| `/` | Filter chats by name or last message |
| `c` | Write a message in the open chat (`enter` to send, `ctrl-j` for a new line, `esc` to close and keep the draft) |
| `r` | Refresh now |
| `?` | Show every key |
| `q` | Quit |

The mouse works too: the wheel scrolls whichever pane is under the pointer (older
messages load when you scroll past the top of a chat), and a click opens a chat or
focuses the conversation. Because the reader captures the mouse, hold `shift` while
dragging to select text in most terminals.

The TUI works with both the Graph and desktop backends. It sends only on a profile
that allows writes (Graph without `read_only`, or desktop created with `--allow-send`),
and only after you confirm the message with `y`; writability is checked again before
every send. Like `messages send`, a send is never retried: if it fails, the draft is
kept, the chat reloads so you can see whether it arrived, and nothing is resent until
you confirm again. Drafts live in memory only.

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
Windows or WSL with a signed-in personal Microsoft account or a work/school account:

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
teams init --profile local --backend desktop --desktop-port 19222 --allow-send --no-login
teams --profile local messages send --chat '19:...@thread.v2' --body 'On my way'
```

On Windows and WSL, `chats list` and `messages list --chat` read from the running
Teams client's local cache: the helper evaluates one fixed, read-only script in the
Teams page and returns projected fields (ids, topics, members, last-message previews,
read state, timestamps, senders, bodies), never tokens or raw records. Member and sender
names come from the matching account's cached profiles. The signed-in user's identity
is resolved from that same cache; if a personal profile is missing or conflicting,
the viewer stays unknown rather than guessing. Personal and legacy Skype user ids
retain their `8:live:` or `8:` prefixes. Pages use the same shape and opaque `--cursor` as Graph
and add `"source": "desktop_cache"`. The cache only holds what Teams has synced, so
older history in a chat you never scrolled back through can be missing. Use the chat
ids returned by `chats list`; personal chats are accessed through the desktop backend.
The chat with yourself is listed too, with the id `48:notes`, so
you can try sending there first. On macOS these commands return `unsupported`.

Desktop profiles are read-only unless created with `init --allow-send`;
`TEAMS_READ_ONLY=true` still blocks sending, but `TEAMS_READ_ONLY=false` cannot unlock a
desktop profile. With the opt-in, `messages send --chat` sends one plain-text message
through the running Teams window, the same way its Send button does: Teams performs the
request with its own sign-in, and nothing is shown or clicked in its window. Each send
asks for confirmation on a terminal and requires `--yes` otherwise. The listener is
verified again and a fresh connection to the Teams main window is used for every send,
and a send is never retried: if Teams does not answer, the command reports that the
message may still be delivered (error kind `unconfirmed`, exit code 8), so check the chat
before sending again. Teams queues
the message like any other, so a send made just before Teams closes goes out when Teams
next starts. The result has the Graph send shape plus `"source": "desktop"`,
`clientMessageId` and Teams' own `status`. `doctor` checks that the send path is still
recognised without sending anything.

Teams, channels, identity, subscriptions, replies, mentions, attachments and edits
return `unsupported` on desktop; the TUI reads chats from the same cache and sends
through the same path as `messages send`. Existing profiles without a backend field
continue to use Graph; Graph remains the default for `init`.

The endpoint must already be enabled through an explicit Teams debugging launch.
On Windows, set the user environment variable
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222`, quit Teams
fully and start it again; remove the variable and restart Teams to close the port.
When `doctor` or `init` finds nothing listening on Windows or WSL, it prints both
commands, ready to paste, for the profile's port (and returns them as
`debugging_setup` in JSON). The variable applies to every WebView2 app you start
while it is set, not just Teams. An administrator can limit it to Teams with the
WebView2 `AdditionalBrowserArguments` policy, a string value named `ms-teams.exe`
under `HKLM\SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments`.
While the port is open, any program running as you can read your chats and send as
you, so close it when you are done.
These commands never launch, restart, quit, or sign into Teams, and never enable
debugging. `init --no-login` saves configuration without probing; ordinary desktop
initialization probes the existing endpoint. A normal Teams launch usually has no
endpoint, which `doctor` reports as `no_listener`.

macOS checks listener binding and installed Teams process ancestry before reading
loopback metadata. On Windows, `teams.exe` checks the listener itself through the
Windows APIs: every listener on the port must be loopback-only and descend from an
`ms-teams.exe` that belongs to the installed MSTeams package and runs as the invoking
Windows user. Under WSL, the Linux CLI starts the Windows build as a helper
(`teams.exe desktop-helper`) and keeps it running for the session, talking JSON over
its stdin and stdout, so NAT mode needs no networking changes. Diagnostics are
limited to `GET /json/version` and `GET /json/list`, and their results omit titles,
full URLs, target IDs, WebSocket URLs and private origins. Chat reads then attach to
the first Teams-origin page over the same verified loopback port and keep that
connection open; if it drops, the listener is verified again before the next read.
Related cache databases are selected for the same account. If a window exposes
several conversation-account caches, reads and sends refuse with a desktop error;
selecting among those accounts is not supported yet. An endpoint exposing both
personal and work/school Teams pages is also refused rather than choosing an account.

On first use under WSL, the helper is downloaded from the matching GitHub release,
checked against the release's `SHA256SUMS`, and cached in
`~/.cache/teams-cli/windows-helper/<version>/`. `doctor --offline` reports whether
it is installed without downloading it. Online probes install a compatible release
helper when needed. Set `TEAMS_DESKTOP_HELPER` to use another
`teams.exe`, or run `make windows-helper` (needs
[cargo-xwin](https://github.com/rust-cross/cargo-xwin)) to build one for a
development checkout. If the download is blocked, fetch the
`x86_64-pc-windows-msvc` archive from the release page, unpack `teams.exe` and point
`TEAMS_DESKTOP_HELPER` at it.
The already published `0.1.9-alpha.1` binary predates this helper protocol. Source
checkouts still carrying that version use `make windows-helper` or an explicit
compatible helper; they do not download that incompatible release.

The CLI and the helper agree on a protocol number, and any helper that speaks the CLI's
protocol is accepted, whatever its version. When they differ, the error names both
sides' version, build and protocol, along with the fix for how the helper was
installed. `teams --version` shows the commit a binary was built from, with `-dirty`
for uncommitted changes.

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

- The Graph backend supports Microsoft 365 work or school accounts only. Use the experimental desktop backend on Windows or WSL for personal Microsoft accounts; the relevant Teams Graph endpoints do not support them.
- A work account can authenticate successfully before Microsoft Teams is licensed or provisioned; `teams doctor` distinguishes identity access from Teams availability.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens, browser interception, or direct calls to private Teams services; the experimental desktop backend reads the local client cache and sends only through the Teams window's own client.
- Tenant policy and admin consent can restrict delegated capabilities.

## CLI Spec

`teams schema` is offline, unauthenticated, and follows [CLI Spec v0.3](https://clispec.dev/): flat invocable command paths, declared effects and cardinality, bounded collections, stable error kinds and exit codes, stdout/stderr separation, and safe noninteractive behavior. Its backend capabilities and per-command backend extensions distinguish Graph operations from the experimental desktop backend.

For agents, each command declares the error kinds it can return, an example invocation, its output fields (nested where useful), argument defaults and limits, and the environment variables that affect it; `teams schema` validates against the official v0.3 JSON Schema in the test suite. Two error kinds matter most for writes:

| Kind | Exit | Meaning |
| --- | --- | --- |
| `confirmation_required` | 2 | The command needs confirmation; it prompts on a terminal, otherwise pass `--yes`. Nothing was done. |
| `unconfirmed` | 8 | A send may have been delivered but was not confirmed (Graph server fault or lost answer, or Teams did not answer). Read the conversation before sending again; never retry blindly. |

`rate_limit` errors include `retry_after_seconds` when Microsoft provides it. Sends cannot be made idempotent, because neither Graph nor Teams accepts a caller-supplied key.

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
