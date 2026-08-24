# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[0.1.1]: https://github.com/rvben/teams-cli/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/rvben/teams-cli/releases/tag/v0.1.0
