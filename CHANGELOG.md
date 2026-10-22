# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `teams people search` finds work and school colleagues again with the desktop backend. It now uses the same directory search as the Teams people picker; the lookup it used before returned nothing, or failed with "Teams could not resolve this person" in a headless browser.

## [0.2.5](https://github.com/rvben/teams-cli/compare/v0.2.4...v0.2.5) - 2026-10-21

### Fixed

- Update TLS certificate verification and bundled root certificates to address RUSTSEC-2026-0382 and RUSTSEC-2026-0383.
- Desktop chats whose messages Teams keeps in a secondary reply-chain store (`replychains-2`, ...) no longer look empty. All such stores are now read and merged, which also fills in member names for those 1:1 chats.
- Desktop chat messages missing from the current cache are filled in from caches the same user and tenant left under earlier display languages. Those caches no longer update, so messages edited or deleted since can appear as they were.
- In the TUI, a chat whose messages are missing now shows its latest message from the chat list and why, instead of "No messages in this chat yet." With the desktop backend it says no messages for the chat were found in the Teams cache on this PC.
- **deps**: update TLS certificate trust dependencies ([435b11c](https://github.com/rvben/teams-cli/commit/435b11c39113e165e843d73579bc28505ef05620))

## [0.2.4](https://github.com/rvben/teams-cli/compare/v0.2.3...v0.2.4) - 2026-10-20

### Fixed

- When Teams keeps caches for several accounts, guest organizations or display languages, the desktop backend now reads the one for the signed-in user, tenant and display language instead of refusing.

### Changed

- Desktop errors now tell apart two cases: personal and work/school Teams both being open, which `--desktop-account` resolves, and one surface holding cached data for several accounts or organizations, which it does not.
- Windows desktop helpers use protocol 5. WSL requires the helper from the same CLI release.

## [0.2.3](https://github.com/rvben/teams-cli/compare/v0.2.2...v0.2.3) - 2026-10-16

### Fixed

- Close extension-managed browser sessions after the last client exits, including crashes and editor reloads, while retaining saved sign-in and browsers started independently.
- Track shared clients and active CLI reads before shutdown. Recover interrupted cleanup and stop individually verified browser helpers, including hung processes, without signalling unrelated applications.

## [0.2.2](https://github.com/rvben/teams-cli/compare/v0.2.1...v0.2.2) - 2026-10-15

### Added

- Guided `teams init` asks for Work/school or Personal, chooses a separate browser port, offers explicit send permission, and opens the inbox after verified sign-in.
- Browser setup can opt in to reopening its saved session with `teams`; existing profiles, native Teams and scripted reads keep their connection behaviour. Explicit setup supports `--auto-start`.
- Offer initialized `pass` stores during new Graph setup on Linux, WSL and macOS, with explicit `--credential-store pass` and `--pass-store-dir` options. Save the backend and absolute directory per profile, verify GPG encryption/decryption before sign-in, and preserve encrypted credentials on failed updates.

### Fixed

- Report credential-helper failures distinctly from missing sign-ins; bound helper operations and preserve the chosen backend across sign-in, refresh and logout.
- Store WSL Graph credentials with Windows DPAPI through PowerShell, so callback and device-code sign-in, token refresh, and logout do not require D-Bus or Secret Service.

### Changed

- Guided setup offers separate accounts or explicit connection updates, preserves Graph registration and channel-history access, resumes interrupted browser sign-in on the original port, and verifies fresh Graph credentials before saving them with rollback on config failure.
- Cold work-browser inbox startup checks sign-in renewal in the background; failures pause sending and offer sign-in while preserving drafts. First-run `teams tui` uses guided setup, including headless Graph recovery and environment-override checks.
- Guided setup checks local browser/display support, Teams installation and Windows interop, and matching lightweight browser downloads before allowing a connection choice. Unavailable options explain their requirements; supported alternatives are selected automatically.
- Guided Graph setup verifies chat access before saving connection settings. Desktop/browser sending opt-in now requires a send-interface probe; guided setup offers an explicit read-only alternative if it fails, without sending a test message.
- Reject personal-only lightweight browser/work-school combinations before saving even with `--no-login`.

## [0.2.1](https://github.com/rvben/teams-cli/compare/v0.2.0...v0.2.1) - 2026-10-15

### Added

- **desktop**: add `teams desktop clear-flags` and a confirmed TUI cleanup that remove remote-debugging flags from the user-wide WebView2 setting while keeping its other arguments.
- Search people by name or email and create or reuse a one-to-one conversation with `people search` and `chats create --recipient ID --yes`. Opening a conversation never sends a message.
- Support these operations through the selected personal or work/school desktop account and through Microsoft Graph. Graph search requires a new sign-in to consent to `User.ReadBasic.All`.

### Changed

- Windows desktop helpers use protocol 4. WSL requires the helper from the same CLI release.
- Ambiguous conversation-creation outcomes remain unconfirmed and are never automatically retried.
- Personal-account searches put verified live matches first and filter unrelated recent-contact suggestions before applying the result limit.

### Fixed

- **desktop**: pass the debugging flags only to the Teams launch on Windows and WSL, so other WebView2 apps such as Outlook no longer open the debugging port.
- **desktop**: name the application that holds a refused port and report a user-wide debugging setting as the cause.
- **desktop**: recognise the dedicated browser on Linux and WSL when Chrome rewrites its process title, so setup no longer stops with "did not open its debugging endpoint"; recovery hints now name the browser mode and port in use.
- **desktop**: keep the work/school browser visible when the organisation's sign-in (such as AD FS with Windows sign-in) does not let Teams stay signed in headless, instead of reporting success and then losing the session; reads now say when Teams was sent back to sign-in, and setup reports whether the browser runs headless or visible.
- **desktop**: stop a verified dedicated browser that hangs after closing its debugging port, so disconnect and recover no longer leave it running.
- **desktop**: refuse sign-in success when the browser closes or loses verified ownership during the sign-in check, and verify reads again after observing work-account sign-in.
- **desktop**: separate email lookup from name search ([b0647f1](https://github.com/rvben/teams-cli/commit/b0647f177c3f4cb5f73e74f42aed11078832e632))

## [0.2.0](https://github.com/rvben/teams-cli/compare/v0.1.13...v0.2.0) - 2026-10-14

### Added

- **desktop**: add one-command lightweight browser setup with automatic matching browser downloads, visible sign-in followed by verified hidden operation, personal-account defaults, and safe reconnect/recovery.
- **desktop**: add managed browser clients and cached avatars ([319c494](https://github.com/rvben/teams-cli/commit/319c494b18c9dff98d26b2d069d4347a39d02e6b))

### Fixed

- **security**: update rustls to 0.23.46 to address the newly published TLS advisories.

## [0.1.13](https://github.com/rvben/teams-cli/compare/v0.1.12...v0.1.13) - 2026-10-14

### Fixed

- **tui**: keep the canvas behind text so the terminal background never shows through ([596eb7b](https://github.com/rvben/teams-cli/commit/596eb7b6f79db174ba1f33e6eaf5eb345912504a))
- **desktop**: send URLs as clickable links like Teams' compose box does ([4b46ca2](https://github.com/rvben/teams-cli/commit/4b46ca2bb2a3d466313ba2f7ce8172c6eb065dbd))

## [0.1.12](https://github.com/rvben/teams-cli/compare/v0.1.11...v0.1.12) - 2026-10-14

### Breaking Changes

- **desktop**: open the debugging port after confirmation and verify a chat read during init ([3eb2110](https://github.com/rvben/teams-cli/commit/3eb211042ac5eed3995a73307dd56270c06d61bb))

## [0.1.11](https://github.com/rvben/teams-cli/compare/v0.1.10-alpha.1...v0.1.11) - 2026-10-14

### Added

- Preserve desktop debugging settings with managed connect/status/disconnect/recover commands, ownership checks and WSL PowerShell discovery without Windows PATH entries.
- Distinguish queued and delivered sends, retain correlation IDs on uncertain outcomes, and require an explicit resend action for possibly-sent TUI drafts.
- Offer a confirmed Teams restart with debugging enabled when the WSL TUI finds no endpoint, with cancellation and retry actions.

### Changed

- Include opt-in experimental desktop support in the regular release channel, with macOS setup instructions and documented personal and work/school validation scope.
- Clarify that desktop history is cache-bound, chat creation is unsupported, and Enter sends a TUI draft directly.

### Fixed

- Keep multiline bracketed pastes in the TUI draft until Enter is pressed, preserve drafts after failed sends, and ignore additional input while sending.
- Render Teams' formatting whitespace after HTML line breaks without adding an extra blank line.
- Keep managed Windows/WSL disconnect working when Teams starts auxiliary processes, while refusing changed listener owners and reused process IDs.

## [0.1.10-alpha.1](https://github.com/rvben/teams-cli/compare/v0.1.9-alpha.1...v0.1.10-alpha.1) - 2026-10-14

### Added

- macOS desktop chat reads, plain-text sends, and process-only debugging setup commands, sharing the Windows/WSL session backend.
- Explicit desktop account-type selection with `init --desktop-account personal|work-school`, supported by helper protocol 3; mixed surfaces are refused by default.
- Personal Teams desktop chat reads and plain-text sends on Windows and WSL, including personal/Skype profile names and signed-in user identity.
- **desktop**: show the commands to open and close Teams' debugging port when doctor or init finds no endpoint ([4100faf](https://github.com/rvben/teams-cli/commit/4100fafd3eede3bb93e0ba6ee831433ebcc0cf81))
- **desktop**: accept any helper that speaks the same protocol, report builds in --version and mismatches, and explain helper download failures ([6e0b22f](https://github.com/rvben/teams-cli/commit/6e0b22f84b0e4f749ced19303044a4e6df0cf6d6))
- **tui**: show send mode, a live status dot and the account in the header, keeping counts visible on narrow terminals ([baaaf49](https://github.com/rvben/teams-cli/commit/baaaf494c10720c4262cf4c533db78427eae8462))
- **tui**: scroll the pane under the pointer with the mouse wheel and open chats with a click ([6b8e834](https://github.com/rvben/teams-cli/commit/6b8e83447664834736ce890fb07d9131f96eaadc))
- **schema**: declare output fields, errors, examples, limits and environment for every command ([3018721](https://github.com/rvben/teams-cli/commit/30187211f686893f6c540fe075328c82c15e9d78))
- **errors**: report possibly delivered sends as unconfirmed and missing confirmation as confirmation_required ([ea2b519](https://github.com/rvben/teams-cli/commit/ea2b519b874d198b47b4c449eccf61c3e123d8a3))
- **tui**: reply in the open chat after confirmation and poll the open chat and chat list automatically ([eb228d6](https://github.com/rvben/teams-cli/commit/eb228d6e09bda15ce0e38d9b9d6c85293afb4917))
- **desktop**: send plain-text chat messages through the running Teams client ([b7412ce](https://github.com/rvben/teams-cli/commit/b7412ce50581398136db7d34cb95b21abda960d8))
- **tui**: prefetch neighbouring chats so moving through the list is instant ([ee68954](https://github.com/rvben/teams-cli/commit/ee689543c938d93ca3d05c016d04fb327dae3651))
- **desktop**: read Teams natively on Windows and through a persistent helper under WSL ([9b4cd42](https://github.com/rvben/teams-cli/commit/9b4cd426d34597c4a2488e93d8154ed7cc321960))
- **tui**: browse real chats and conversations in the terminal interface ([3a7cca9](https://github.com/rvben/teams-cli/commit/3a7cca9fa81f563a232d7a09a844bd046afeca49))
- **output**: render chats and messages as readable conversations ([b960acd](https://github.com/rvben/teams-cli/commit/b960acdff53ccac0a734dbf48c48a86513f440f8))
- **desktop**: project chat members, previews and read state from the cache ([b6c28c5](https://github.com/rvben/teams-cli/commit/b6c28c5ee6c94ce8798c6a66955af9dec90e70e3))
- **desktop**: read chats and chat messages from the Teams client cache ([ba4deda](https://github.com/rvben/teams-cli/commit/ba4dedaef1495f04bcf84412ff2a7aab3ac9af32))

### Fixed

- Replace unpaired UTF-16 characters in projected desktop text so malformed cached previews cannot break a chat page.
- Normalize missing and sentinel cache timestamps so older chats and messages retain valid pagination cursors.
- Preserve personal identity namespaces and leave missing or conflicting viewer mappings unknown while keeping account caches isolated.
- resolve desktop review regressions ([09fb74d](https://github.com/rvben/teams-cli/commit/09fb74de579f16116da420ba7cf75db5ef16acdf))
- **desktop**: find powershell.exe in the standard Windows location when PATH omits it ([0406340](https://github.com/rvben/teams-cli/commit/04063402778cb0dbbe3e5cf16699081bd3082918))
- **doctor**: exit with unhealthy when a check fails ([fe9bb66](https://github.com/rvben/teams-cli/commit/fe9bb66ecab88974704bb6b743ca59e97f8b75de))

## [0.1.9-alpha.1](https://github.com/rvben/teams-cli/compare/v0.1.8-alpha.1...v0.1.9-alpha.1) - 2026-10-13

This alpha replaces `0.1.8-alpha.1`, whose publication stopped at the Windows
archive smoke test. Desktop support remains limited to connection diagnostics.

### Fixed

- **ci**: use portable temporary paths for Windows archive smoke tests ([5239bde](https://github.com/rvben/teams-cli/commit/5239bde5f3adde32c24616a9988c0c7e7c2ead76))

## [0.1.8-alpha.1](https://github.com/rvben/teams-cli/compare/v0.1.7...v0.1.8-alpha.1) - 2026-10-13

### Added

- **desktop**: add experimental connection diagnostics ([b24c7c7](https://github.com/rvben/teams-cli/commit/b24c7c7e1fa6cd28dae99b0a1118e60795f1c3d2))
- Graph-default and experimental Desktop profiles with `doctor`, `auth status`, and capability reporting on macOS, Windows, and WSL. Desktop session, chat, and message access remain unverified and unsupported.
- Prerelease publication and smoke tests of the extracted release binaries.

### Fixed

- Update `quinn-proto` to address RUSTSEC-2026-0338, RUSTSEC-2026-0340, and RUSTSEC-2026-0343.
- Exclude design documents and local research from Cargo and Python source packages.

## [0.1.7](https://github.com/rvben/teams-cli/compare/v0.1.6...v0.1.7) - 2026-09-27

### Fixed

- **deps**: update rustls to 0.23.45 for RUSTSEC-2026-0285 ([d2338ff](https://github.com/rvben/teams-cli/commit/d2338ff8abf676501931c5ba7225097bb154f39c))

## [0.1.6](https://github.com/rvben/teams-cli/compare/v0.1.5...v0.1.6) - 2026-09-03

### Added

- **auth**: standardize authentication workflow ([bb0019f](https://github.com/rvben/teams-cli/commit/bb0019f0ceb86dbb963f3499348c58a95922081e))

### Fixed

- **auth**: tolerate missing Linux credential service ([7764383](https://github.com/rvben/teams-cli/commit/776438359f3c913d0514de921042d770c6a88bf0))

## [0.1.5](https://github.com/rvben/teams-cli/compare/v0.1.4...v0.1.5) - 2026-08-26

### Added

- **tui**: recover Microsoft sign-in in place ([8266887](https://github.com/rvben/teams-cli/commit/8266887feef3feb651fa4ae5348752be2703d36b))

## [0.1.4](https://github.com/rvben/teams-cli/compare/v0.1.3...v0.1.4) - 2026-08-26

### Added

- **packaging**: add package-named launcher ([35a14d3](https://github.com/rvben/teams-cli/commit/35a14d329c5ca5a24f0e47c81cf29f30bad8773c))

## [0.1.3](https://github.com/rvben/teams-cli/compare/v0.1.2...v0.1.3) - 2026-08-26

### Added

- **cli**: align safety and configuration contract ([c564f63](https://github.com/rvben/teams-cli/commit/c564f63f7deeb4e8a7c8faa359e32f3ea0da8556))

### Fixed

- **config**: persist profiles atomically ([ababe5e](https://github.com/rvben/teams-cli/commit/ababe5e46f24a52d86007de5737cd7601a1ae0c3))
- **ci**: install pinned Rust components ([f5ecdcc](https://github.com/rvben/teams-cli/commit/f5ecdccfb4ca697ff23cc828393e519a788cbea8))

## [0.1.2] - 2026-08-24

### Added

- Bundled the maintained multitenant teams-cli Entra public-client registration for zero-ID onboarding.
- Added explicit `--channel-history` consent and machine-readable authentication capability metadata.

### Changed

- Normal login and token refresh now use only the user-consentable delegated permission baseline.
- Diagnostics now distinguish successful Microsoft identity access from an unlicensed or unprovisioned Teams tenant.

## [0.1.1] - 2026-08-24

### Fixed

- Made crates.io publication idempotent by checking the registry's version list with a privacy-preserving User-Agent before requesting publish credentials.

## [0.1.0] - 2026-08-24

### Added

- Human-first commands and a responsive Ratatui interface for Microsoft Teams.
- Agent-first CLI Spec v0.3 schema, JSON envelopes, stable errors, and safe noninteractive behavior.
- Delegated Microsoft OAuth with browser PKCE, explicit device-code login, and OS-keyring credential storage.
- Commands for identity, joined teams, channels, chats, and reading or sending messages through Microsoft Graph.
- Guided onboarding, diagnostics, completions, bounded collections, and deterministic TUI snapshots.
- Distribution through crates.io, PyPI binary wheels, and signed GitHub release artifacts.
