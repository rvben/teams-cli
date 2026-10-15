# teams-cli

Microsoft Teams from your terminal—human-first and agent-first.

`teams-cli` is a Rust CLI and TUI for everyday Microsoft Teams work through supported Microsoft Graph APIs, with an optional experimental desktop backend that reads chats from, and can send chat messages through, a running Teams client on macOS, Windows or WSL. A dedicated browser client also provides an experimental route on native Linux; see [browser setup](#dedicated-browser-client-experimental). It is designed as two equally first-class experiences over one typed core: a fast, calm terminal interface for people and a deterministic, introspectable command contract for agents. The default Graph backend signs in as you with delegated OAuth and protects refresh credentials in your operating system's credential store. Output is readable text on a TTY and structured JSON when piped.

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

Choose **Work or school** or **Personal**, choose whether to enable sending, and
follow sign-in. Setup checks this machine first and gives unavailable connections
no selectable number, with an explanation of what they need. It chooses a separate
browser port and opens your inbox once chats are readable. After that, just run:

```console
teams
```

On macOS, Linux and WSL2, work/school setup prefers a dedicated browser; personal
setup prefers the lightweight browser. Missing display support, missing executables,
invalid browser overrides and unavailable matching downloads are checked before
selection. Personal can use a full browser or Teams Desktop when lightweight mode
is unavailable. Work/school can use Teams Desktop or Microsoft Graph when needed.
On a Linux terminal without a display, Graph uses device-code sign-in on another device.
You explicitly approve reopening a dedicated browser
when you start the inbox. The saved sign-in is reused, and expired work/school
sign-in offers to reopen the sign-in window. Some organisations require keeping
the browser visible; minimise it instead of closing it. WSL2 needs WSLg for sign-in.
On native Windows, browser options are unavailable; use Teams Desktop or run inside
WSL2 for browser mode.

Guided setup saves connection settings only after a successful chat read, including
Graph profiles. If sending was requested, desktop/browser setup also probes the
send interface without sending a message. If that fails, you can explicitly continue
read-only; setup never silently downgrades your choice. A successful probe establishes
that the interface is available, while delivery and organisation policies are checked
when you actually send. Teams build compatibility and sign-in still need live validation.

**Other connection options** keeps Microsoft Graph and the running Teams Desktop
available. Microsoft Graph includes a maintained Entra public-client registration,
so normal API setup needs no app registration or copied client ID.

Scripts and explicit connection flags keep their existing behaviour. For example:

```console
teams init --backend graph
# Or configure an automatically reopened work browser explicitly:
teams init --profile work --backend desktop --desktop-client browser \
  --desktop-account work-school --desktop-port 9224 --auto-start
```

Browser profiles default to read-only; add `--allow-send` to enable sending.
Existing profiles retain their connection behaviour unless you opt in with setup
or `--auto-start`. CLI reads, JSON output, demo mode and snapshots never start a
browser automatically. See [lightweight browser setup](#low-memory-browser-client-experimental)
for personal-account compatibility limits.

For Microsoft Graph profiles, browser PKCE is the default when a local browser is available. The browser talks directly to Microsoft, and the CLI never receives or stores your password. Normal Graph sign-in requests these delegated scopes:

```text
openid profile offline_access User.Read User.ReadBasic.All
Team.ReadBasic.All Channel.ReadBasic.All Chat.Read Chat.Create ChatMessage.Send
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
| `c` | Write a message in the open chat (`enter` to send, `ctrl-j` or `alt-enter` for a new line, `esc` to close and keep the draft) |
| `r` | Refresh now |
| `?` | Show every key |
| `q` | Quit |

The mouse works too: the wheel scrolls whichever pane is under the pointer (older
messages load when you scroll past the top of a chat), and a click opens a chat or
focuses the conversation. Because the reader captures the mouse, hold `shift` while
dragging to select text in most terminals.

The TUI works with both the Graph and desktop backends. It sends only on a profile
that allows writes (Graph without `read_only`, or desktop created with `--allow-send`),
and pressing `enter` sends directly; writability is checked again before every send.
Use `ctrl-j` or `alt-enter` for a new line. Multiline pastes stay in the draft until
you press `enter` (in terminals supporting bracketed paste). Like `messages send`,
a send is never retried: if it fails, the draft is kept, the chat reloads so you can
see whether it arrived, and nothing is resent until you press `enter` again.
Drafts live in memory only.

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
teams people search "Alex" --limit 20
teams chats create --recipient PERSON_ID --yes  # no message is sent
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

Chat, message, team and channel collections expose `--limit` and `--fields`; Graph-native collections use `--cursor`, while joined teams and channels use `--offset` because those endpoints do not accept `$top`. With a terminal they render concise text; when stdout is piped they emit JSON envelopes with continuation metadata and `truncated`.

To message someone for the first time, use `people search` to find their verified
identity in the active profile. Search accepts a name or email prefix of 2–256
characters and returns at most 50 people; refine the query if `truncated` is true.
Pass the exact returned ID to `chats create`, then pass the resulting chat ID to
`messages send`. Creating a conversation reuses an existing one-to-one chat when
available and sends no message. It requires a writable profile and confirmation
(`--yes` for automation). Recipient IDs belong to their selected account/backend.
If creation is unconfirmed, inspect your chats before trying again.

Graph recipient search needs delegated `User.ReadBasic.All` consent. Existing
sign-ins can keep reading chats; run `teams auth login` again to enable directory
search. Your organization may restrict consent, directory guest access, external
recipients or chat creation. Personal accounts use the desktop backend.

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
all three platforms with a signed-in personal Microsoft account or a work/school account:

```console
teams init --profile local --backend desktop --read-only --no-login
teams --profile local doctor --offline
teams --profile local doctor
teams --profile local auth status
# Match a separately enabled endpoint on another port:
teams init --profile local --backend desktop --desktop-port 19222 --no-login
# If Teams exposes personal and work/school pages, choose the intended surface:
teams init --profile local --backend desktop --desktop-port 19222 --desktop-account personal --no-login
# macOS/Windows/WSL: read chats and chat messages
teams --profile local chats list --limit 20
teams --profile local messages list --chat '19:...@thread.v2'
# macOS/Windows/WSL: opt in to sending plain-text chat messages
teams init --profile local --backend desktop --desktop-port 19222 --desktop-account personal --allow-send --no-login
teams --profile local messages send --chat '19:...@thread.v2' --body 'On my way'
```

Personal-account read, send and readback have been tested with released binaries
on Apple Silicon macOS, native Windows and WSL2. Work/school read, send and
readback have also been reported working by a WSL2 user; macOS work/school cache
reads were tested separately. See the [release validation scope](docs/releases.md#desktop-validation-scope)
for coverage and remaining limits.

On macOS, Windows and WSL, `chats list` and `messages list --chat` read from the running
Teams client's local cache: the backend evaluates one fixed, read-only script in the
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
you can try sending there first.

Desktop chat results may also include optional `avatarData` raster data URIs on
members and one-to-one chats. Photos come only from image resources already loaded
by that account's Teams page; the CLI never fetches a photo URL. Missing, stale,
ambiguous, or unsupported photos leave the fields absent, so clients should show
initials as a fallback. Reads allow at most 16 photos, 256 KiB per photo, 1 MiB of
avatar data in total, and a one-second budget. A photo failure never fails a chat
read. Personal photo reads have been checked on macOS; work/school and other
platforms still need photo-specific runtime validation.

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
`clientMessageId`, Teams' own `status` and a normalized `delivery_state`:
`submitted`, `queued`, `delivered`, `failed` or `unknown`. A successful submission is
not proof of delivery. `doctor` checks that the send path is still recognised
without sending anything.

An uncertain desktop send also returns `chatId` and `clientMessageId` in its JSON
error. Check the exact ID with:

```sh
teams --profile local messages status --chat CHAT_ID --client-message-id CLIENT_MESSAGE_ID
```

This checks the newest 50 cached messages and returns metadata only. Missing cache
records remain `unknown`, not failed or safe to retry. The ID is a correlation ID,
not an idempotency key. In the TUI, a possibly-sent draft blocks ordinary Enter and
paste; `r` checks delivery, `s` offers an explicit second-copy confirmation, and
`esc` keeps the draft and uncertainty together in memory. Reopening it does not
unlock ordinary sending. Queued results are labelled queued rather than sent.

Desktop supports people search and one-to-one chat creation through the selected
Teams client's own session. Creation requires a profile with `--allow-send` and
sends no message. Teams, channels, identity commands, subscriptions,
replies, mentions, attachments and edits return `unsupported` on desktop. The TUI
reads chats from the same cache and sends through the same path as `messages send`.
Existing profiles without a backend field
continue to use Graph; Graph remains the default for `init`.

Use a managed debugging session on macOS, Windows or WSL:

```sh
teams --profile local desktop connect
teams --profile local desktop status
teams --profile local desktop disconnect
```

Connect and disconnect ask before restarting Teams because a restart ends active
calls; pass `--yes` for a scripted restart. `--port` overrides the selected profile's
port. An existing verified endpoint is inspected without being adopted; disconnect
only restarts recorded Teams processes that still own the debugging listener.
Added auxiliary processes are left alone. If Teams was
restarted independently, quit and relaunch it normally, then disconnect again to
clear the old record. No command automatically restarts Teams on exit.

Debugging flags apply only to the managed Teams launch; other apps and the user
environment are left unchanged. On Windows/WSL, Teams starts through its
`ms-teams.exe` app execution alias, which must be enabled (Settings > Apps >
Advanced app settings > App execution aliases). Connect refuses a user-wide
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` that already has debugging flags: such a
setting opens the port in every WebView2 app, for example Outlook, and the
listener is then refused because Teams does not own it. Errors name those apps.
`teams desktop clear-flags` removes only the debugging flags from that setting
after confirmation (`--yes` when scripted); quit and reopen the named apps
afterwards. The TUI offers the same cleanup.
Windows and WSL share a recovery record and lock. WSL finds PowerShell in its normal
Windows system directory even when Windows PATH entries are disabled; Windows
executable interop must still be enabled.

If setup is interrupted, run `teams --profile local desktop recover`. It clears the
interrupted record (and restores settings left by older versions) without restarting Teams or requiring the Windows helper; it does
not claim ownership of an unrecorded Teams process. Use `desktop status` to inspect
the endpoint and saved session. Recovery cannot close an already-running debugging
endpoint: quit Teams and relaunch it normally if its ownership was not recorded.

While the port is open, any program running as you can read chats and send as you.
The WSL TUI offers the same managed connect after explicit restart confirmation.
`teams init --backend desktop` sets the connection up end to end. When no endpoint
listens it runs the same managed connect after the same restart confirmation (`--yes`
when scripted), then reads one chat page. It saves the profile only when that read
succeeds, so `session_verified: true` means reads work, and a declined restart or a
failed read leaves Teams and any existing profile of that name untouched. If the read
fails after a restart, the error says the debugging port is still open and how to close
it. `init --no-login` saves configuration without contacting Teams. `doctor` prints the
managed opening and closing commands when no endpoint is found; it does not launch
Teams or verify sign-in. A normal Teams launch
usually has no endpoint, which `doctor` reports as `no_listener`.

macOS checks listener binding and installed Teams process ancestry before accessing
loopback metadata or the Teams page. On Windows, `teams.exe` checks the listener itself through the
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
selecting among those accounts is not supported yet. `--desktop-account auto` is
the default and refuses an endpoint exposing both personal and work/school pages.
Set `--desktop-account personal` or `--desktop-account work-school` when creating
the profile to select that surface explicitly. The choice applies to reads, sends
and `doctor`'s send check, and never falls back to the other surface. Multiple
cached accounts within the selected surface remain unsupported.

On first use under WSL, the helper is downloaded from the matching GitHub release,
checked against the release's `SHA256SUMS`, and cached in
`~/.cache/teams-cli/windows-helper/<version>/`. `doctor --offline` reports whether
it is installed without downloading it. Online probes install a compatible release
helper when needed. Set `TEAMS_DESKTOP_HELPER` to use another
`teams.exe` speaking protocol 3 (including account selection), or run `make windows-helper` (needs
[cargo-xwin](https://github.com/rust-cross/cargo-xwin)) to build one for a
development checkout. If the download is blocked, fetch the
`x86_64-pc-windows-msvc` archive from the release page, unpack `teams.exe` and point
`TEAMS_DESKTOP_HELPER` at it.
For older source checkouts still carrying `0.1.9-alpha.1`, use `make windows-helper`
or an explicit compatible helper: that published binary predates protocol 3 and
cannot serve as the helper for current source.

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

### Dedicated browser client (experimental)

On native Linux, macOS, or WSL2, the desktop backend can use an installed Chrome,
Chromium, or Edge browser instead of the native Teams app. WSL2 uses a Linux browser;
visible sign-in requires WSLg or another graphical display. The existing Windows
helper route remains the default for native desktop profiles.

Create a separate read-only profile and sign in in the window that opens:

```console
teams init --profile browser --backend desktop --desktop-client browser --desktop-account work-school --auto-start --yes
# The window closes automatically after a successful chat read.
# Teams then runs headless with the same saved sign-in:
teams --profile browser
# To renew sign-in or complete MFA later:
teams --profile browser desktop login --yes
```

Login waits up to five minutes for a readable chat cache, then restarts headless
and verifies another read before reporting success. An empty chat list is valid.
A sign-in timeout leaves the visible window open; finish signing in and retry
`desktop login`. Interrupting login preserves the browser and its saved profile.
On WSL2, the visible step needs WSLg or another graphical display.

Some organisations sign in through their own identity provider (for example AD FS
with Windows sign-in) and do not let Teams renew its sign-in in a headless browser.
For work/school accounts, login therefore watches the headless browser for up to a
minute; when Teams is sent back to sign-in, the visible browser is reopened and stays
open instead. Minimise that window, but keep it open. When a headless session is sent
back to sign-in later, reads report it; run `desktop login` again.

Use `--desktop-account personal` for a personal Microsoft account. Sending still
requires an explicit `init --allow-send` and the existing send confirmation.
`init --no-login` only saves configuration. `desktop connect --visible` remains
available for manually managed sign-in; `desktop connect` starts headless.

The browser uses its own profile under the local data directory at
`teams-cli/browser/<port>/profile`; it never copies or opens your normal browser
profile. Different ports use different browser profiles. Disconnect closes this
browser and keeps its sign-in data. To switch between visible and headless operation,
disconnect first. `desktop recover` closes a verified managed browser or clears a
stale record when no listener remains. A verified browser that does not exit after
closing its debugging port is stopped; unverifiable processes are left untouched.

The CLI checks that every listener is loopback-only and belongs to the recorded
browser process, running as you with the expected executable, debugging port and
dedicated profile. Only HTTPS Teams pages are used for reads or sends. While the
port is open, other programs running as you can access that browser session.

Browser discovery supports common system installations. For another installation,
set `TEAMS_DESKTOP_BROWSER` to the absolute path of the actual browser executable,
not a shell launcher. Snap/Flatpak installations are not automatically supported.
The optional `TEAMS_DESKTOP_BROWSER_DATA_DIR` sets an absolute browser data directory
(each port gets a separate subdirectory). Keep the same browser executable and data
directory settings while managing a session. Headless mode removes the visible window; Teams still consumes browser CPU and RAM.

This route is experimental. Browser lifecycle and ownership checks can be tested
without signing in, but live Teams Web reads, sends, session persistence and MFA
recovery still require validation on a signed-in machine. Existing native Teams
validation does not establish browser-client compatibility.

### Low-memory browser client (experimental)

`--desktop-client browser-core` keeps the authenticated Teams data runtime without
starting the main UI. It supports personal accounts only and requires the
exact Teams Personal bootstrap build validated by this CLI. Unknown builds fail
with `desktop_error`; the CLI does not fall back to old cache data or the full UI.
Use `--desktop-client browser` on a separate port for the full web client.

Start with one command:

```console
teams init --profile core --backend desktop --desktop-client browser-core
```

The CLI downloads a matching **Chrome for Testing / chrome-headless-shell** pair
from Google's official Stable channel on first setup, opens a separate sign-in
window, and hides it once chats are readable. It verifies the hidden session before
saving your CLI profile. Core mode defaults to a personal account, port **9223**,
and read-only access; add `--allow-send` to enable sending. Your normal browser,
its profile, and the native Teams connection on port 9222 are left alone.

Browser binaries and sign-in state live in a private `teams-cli/browser` data
directory. The installed pair is reused offline and is not silently updated while
sessions are running. Setup needs roughly 1 GiB of disk space for the browsers and
extraction; the ZIP downloads are removed after installation. Linux/WSL2 need a
desktop display (WSLg on WSL2) for initial sign-in.

```console
teams --profile core desktop login       # Sign in again; automatically hide afterward
teams --profile core desktop disconnect  # Stop the browser; retain sign-in
teams --profile core desktop connect     # Start hidden using the saved session
teams --profile core desktop status      # Inspect process and recovery state
```

An interrupted sign-in retains the dedicated window and profile. Rerun `desktop
login` to finish; an expired hidden session can use the same command. A stale
process record is recovered automatically only when its port has no listener;
unrelated or changed processes are refused. `desktop recover` preserves sign-in
and can close only a verified managed browser. The status command does not prove
that you are signed in; `chats list` or `doctor` verifies message access.

Advanced setups can override both `TEAMS_DESKTOP_BROWSER` and
`TEAMS_DESKTOP_HEADLESS_BROWSER` with absolute paths to matching executables.
Set both or neither; a partial override is refused. `TEAMS_DESKTOP_BROWSER_DATA_DIR`
changes the private data location. Keep these settings stable while a session is
running. Unknown Teams builds are refused; use the full `browser` client on its
own port if your Teams build is not yet supported.

On macOS, both dedicated test browsers use mock-keychain encryption because the
headless shell uses it. This profile contains sensitive login state and remains in
a private directory; it never opens your ordinary browser profile.

The integrated CLI measured about **307 MiB after collection and 322 MiB idle**
of summed browser process physical footprint on macOS with a personal account.
Signed-in reads, visible-to-headless login, sending/readback and incoming updates
from a second verified client passed. This is approximate process accounting, not a guaranteed RAM cap or a
Linux measurement. Startup peaks, large accounts, overnight use, reconnect/MFA and
Linux/WSL2 signed-in operation still need validation. Size-oriented V8 settings may
trade CPU for memory. Avatar fields may be absent because UI photos are not loaded.
Sending still requires `init --allow-send` and the existing send confirmation.

## Graph boundaries

- The Graph backend supports Microsoft 365 work or school accounts only. Use the experimental desktop backend on macOS, Windows or WSL for personal Microsoft accounts; the relevant Teams Graph endpoints do not support them.
- A work account can authenticate successfully before Microsoft Teams is licensed or provisioned; `teams doctor` distinguishes identity access from Teams availability.
- Calling and screen sharing are not available as a delegated user CLI; Graph calling is built around bot/application scenarios.
- No extracted desktop tokens or direct calls to private Teams services; the experimental desktop backend reads the local client cache and sends only through the Teams window's own client.
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
