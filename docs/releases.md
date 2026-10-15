# Releases

Vership is the release control plane for `teams-cli`. GitHub Actions is the
execution plane for cross-platform builds, publication, provenance, and
downstream packaging.

## Create a release

Start from a clean, up-to-date `main` branch and run:

```sh
vership preflight
vership bump patch --prepare # or minor/major
# Review the candidate and obtain approval to push and publish.
vership release
tarry cmd --timeout 20m -- vership verify
```

Vership runs `make check`, updates the package version and changelog, creates
the Conventional Commit release commit. `--prepare` stops before tagging or
pushing. After approval, `vership release` publishes that same prepared version;
do not bump again. Its tag starts the release workflow.

Cargo and Python package versions are kept synchronized as one release unit.

## Release 0.3.0

The version is prepared. Do not bump it again. This release adds `people search`
and `chats create`; opening a conversation does not send a message. Personal
desktop search separates exact email lookup from name search and excludes
off-network invitations. A recipient must resolve to a Teams identity before
chat creation. Graph directory search requires renewed consent to
`User.ReadBasic.All`.

The Windows helper uses protocol 4 and must match the CLI version and build.
Validate the Windows helper and WSL client together. Conversation requests and
sends with an uncertain outcome must never be automatically retried.

Teams Companion 0.1.1 downloads CLI 0.3.0. Publish and verify the CLI archives and
checksums before distributing the extension so its built-in download works.
Older stable CLIs from 0.1.11 retain reading and replies; New Message offers an
update when the required commands are missing. Vership prepares the extension's
version, while its VSIX package check validates the installable artifact.
Vership does not publish or verify a VS Code Marketplace target.

Before publication, distinguish existing-chat reuse from first-contact coverage.
For an approved, discoverable recipient with no existing conversation, resolve
their exact identity, create the chat once, send one labelled message and verify
its exact ID and body. A second successful create must return the same chat ID.
An invite-only search result does not establish a supported recipient and must
not trigger a message or invitation. Record account/backend/platform limitations
separately from successful runtime tests.

After approved publication workflows finish, verify every CLI target:

```sh
vership verify 0.3.0 --targets tag,release,crates,pypi
```

## Release 0.2.0

The managed lightweight browser is an experimental personal-account option on
macOS and Linux/WSL2. `teams init --backend desktop --desktop-client browser-core`
installs a matching Chrome for Testing pair from Google's Stable channel, uses
port 9223, and saves the CLI profile only after both visible and hidden chat reads
succeed. No version bump is needed: 0.2.0 is already prepared.

Fresh macOS onboarding passed real downloads, automatic hiding, signed-in reads,
one-shot sends with exact ID/content readback, incoming updates from a separate
verified client without reloading, reuse with the download network unavailable,
stale-process reconnection, repeated login, quiet mode, and verified shutdown.
The managed browser measured approximately 316 MiB idle process footprint in this
short personal-account run. macOS/Linux tests and lint, and Windows cross-compilation
cover the installer and integration. Linux signed-in use, MFA expiry, prolonged
operation and work/school core accounts remain outside the validated scope.

After publication workflows finish, verify every intended target:

```sh
vership verify 0.2.0 --targets tag,release,crates,pypi
```

## Release 0.1.11

The `0.1.11` candidate builds on the desktop implementation validated in
`0.1.10-alpha.1`, adding direct Enter-to-send with multiline paste handling and a
confirmed Teams debugging restart from the WSL TUI. Managed desktop connection
commands preserve prior settings, share Windows/WSL recovery state and check
process ownership before disconnecting. Send results distinguish submission,
queueing and delivery; possibly-sent TUI drafts require an explicit second-copy
confirmation. Exact client-ID checks inspect the newest 50 cached messages and
leave missing or unspecified delivery evidence unknown. It supports cached chat reads and opt-in plain-text sends
through Teams on macOS, Windows and WSL. The Windows helper speaks protocol 3.
Personal Microsoft accounts use the desktop backend; Graph remains limited to
work/school accounts.

An unsuffixed release can include an explicitly experimental, opt-in backend.
Graph remains the default. Desktop compatibility still depends on Teams internals
and is limited to the operations and tested scope documented below. A beta release
is optional; it is not a prerequisite for publishing `0.1.11`.

The version is already prepared. Finish it with `vership release` after approval;
do not run another bump. Cargo, the Python package and the CLI all use `0.1.11`.
Before publishing, run `make check`, the release dry run and package checks on the
final candidate. Live-test the new TUI send/paste behavior and the WSL restart
confirmation, cancellation and reconnection paths. Release CI must pass its
Linux/macOS/Windows checks and build every intended archive and Python wheel.

After the workflows finish, verify every publication target:

```sh
vership verify 0.1.11 --targets tag,release,crates,pypi
```

The binary build jobs extract and smoke test their packaged CLI before uploading
it. Follow publication with installation tests on macOS, Windows and WSL,
including the matching Windows helper's automatic download under WSL. Desktop
diagnostics can be tested with isolated profiles; successful connection metadata
does not establish sign-in or message access.

### Desktop validation scope

The instrumented runtime checks below used the published `0.1.10-alpha.1`
artifacts. Work/school WSL2 coverage is a separate user report.

| Account | Apple Silicon macOS | Native Windows | WSL2 |
| --- | --- | --- | --- |
| Personal | Instrumented reads, send and exact readback | Instrumented reads, send and exact readback | Instrumented reads, send and exact readback |
| Work/school | Instrumented cache reads | No separate work/school send validation | User-reported reads, send and readback |

The work/school report did not capture the exact CLI or Teams version. Keep it
separate from instrumented evidence. The user also reported Windows working;
individual native Windows TUI interactions were not instrumented.
The `0.1.11` reliability candidate has separate instrumented validation: managed
connect, repeated connect without restarting, disconnect and settings preservation
on macOS and native Windows; a multiline Mac TUI paste that did not send until
Enter and appeared exactly once; and a WSL terminal fixture for lost responses,
guarded resend, exact-ID reconciliation and the structured uncertainty contract.
The fixture made no real Teams calls. Windows state-machine fixtures cover
interrupted setup, external setting changes, conflicting owners, changed
processes, existing debugging flags and repeated disconnects. These checks use
isolated records and simulated platform effects.

WSL live checks also covered restart confirmation and cancellation with Windows
PATH entries removed, one TUI send with exact readback, and native Windows
disconnect after WSL TUI use. The final lifecycle run restored normal Teams,
closed the debugging port and cleared the recovery record without fallback.
Windows fixtures additionally cover auxiliary processes appearing or exiting,
reused process IDs, changed listener ownership and a rebound port during cleanup.

The tested personal Teams cache omitted delivery status even when the exact
message was present. Readback therefore reported `found: true` and delivery
`unknown`; cache presence alone is not a delivery receipt. Each real send was
issued once, with its checkpoint and exact correlation ID retained privately.

Instrumented checks also covered cache pagination, read-only refusal, explicit
account-surface selection and ambiguity refusal, malformed cached text and
sentinel timestamps, macOS Teams restart recovery, WSL helper replacement while
the TUI stayed open, and normal shutdown with debugging closed. A controlled
helper fixture verified one send attempt and an `unconfirmed` error after a lost
response; no real Teams send was deliberately interrupted. Each real test send
was issued once and read back by its exact message ID.

Multi-day daily use, actual account switching, macOS work/school sending, live
Intel Mac coverage, and broader Teams-version and managed-policy coverage remain
to be validated. These are follow-up coverage goals for the experimental backend;
they do not require putting the whole CLI on a prerelease channel.
The cache contains only synced history; only existing chats and plain-text sends
are supported. Missing cached profiles leave names or viewer identity unknown
without breaking reads. Multiple accounts within one selected surface are
refused. Keep these limits visible in the normal release documentation.

## Failure policy

If a release fails before any release, artifact, package, checksum, or
attestation becomes public, delete and recreate the brief tag and retry the
same version. Once anything was published, preserve the tag and issue a patch
release for release-content corrections. Registry and downstream-packaging
jobs are separate recovery domains; retry only the failed job when possible.

## Dry runs

The Release workflow can be dispatched from `main` with `dry_run` enabled. It
uses the package version unless an explicit `version` is supplied, builds the
real artifacts, and exercises registry validation without publishing packages,
creating a GitHub release, generating attestations, or updating downstream
packaging.

## Browser-client validation

The optional `--desktop-client browser` route runs an isolated Chrome/Chromium/Edge
profile on macOS or Linux, including WSL2 with a Linux browser. A visible initial
sign-in and subsequent headless operation use the same persistent profile. Native
Teams and the WSL Windows helper retain their existing behavior.

Before claiming live browser compatibility, validate signed-in personal and
work/school cache reads, a single explicitly approved send with exact readback,
visible-to-headless session persistence, expired-sign-in recovery, and normal
shutdown on each intended platform. Linux process/socket tests and browser lifecycle
smoke tests establish plumbing behavior, not signed-in Teams compatibility.

Repeat the unsigned-in real-browser lifecycle test with a test installation:

```sh
TEAMS_DESKTOP_BROWSER=/absolute/path/to/test/chromium cargo test --locked --test desktop browser_lifecycle_with_disposable_profile -- --ignored
```

The test uses a disposable profile, preserves the normal browser profile, exercises
idempotent connect and mode-change refusal, and closes the managed browser on exit.
On macOS, use Chrome for Testing for this short-lived validation.

Automatic browser login opens visibly, waits for a successful chat read, closes
only the verified managed browser, and restarts headless with the same profile.
The headless chat read must also succeed. Test `desktop login` and browser `init`
with real sign-in, MFA, an empty chat list, expiry, and interrupted/timeout recovery
before claiming this automatic flow is validated on a platform.


## Core browser release checks

`browser-core` is an opt-in, build-pinned personal-account route. It requires
a matching Chrome for Testing / chrome-headless-shell pair (automatically
installed from Google on first setup) and a separate private profile. A startup verifies the retained data client with a fresh chat
query before allowing cache reads or sends. Unknown bootstrap hashes and missing
readiness markers refuse operations; there is no automatic full-UI fallback.

Release validation should cover actual CLI connect/read/login/disconnect, incoming
updates, a separately approved send and exact readback, and footprint after those
operations. Pure browser prototype evidence is insufficient for CLI integration.
Run macOS and Linux checks; compile Windows native paths too. Do not claim Linux
signed-in compatibility, work/school core support, a fixed RAM limit, or long-running
reliability from the short personal-account macOS validation. Prototype reports,
profiles, public bundle copies and memory snapshots remain local and uncommitted.
