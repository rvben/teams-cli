# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
