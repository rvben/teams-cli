# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- macOS desktop chat reads, plain-text sends, and process-only debugging setup commands, sharing the Windows/WSL session backend.
- Explicit desktop account-type selection with `init --desktop-account personal|work-school`, supported by helper protocol 3; mixed surfaces are refused by default.
- Personal Teams desktop chat reads and plain-text sends on Windows and WSL, including personal/Skype profile names and signed-in user identity.

### Fixed

- Preserve personal identity namespaces and leave missing or conflicting viewer mappings unknown while keeping account caches isolated.

## [0.1.10-alpha.1](https://github.com/rvben/teams-cli/compare/v0.1.9-alpha.1...v0.1.10-alpha.1) - 2026-10-13

### Added

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
