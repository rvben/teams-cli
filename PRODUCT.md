# Product

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

## Stack

Rust application distributed as the `teams-cli` Python package on PyPI, exposing the `teams` executable. The product includes both a conventional command interface and a terminal user interface.

## Users

- People who work in Microsoft Teams and prefer fast, keyboard-driven terminal workflows.
- Developers, scripts, and AI agents that need a predictable, inspectable interface to supported Teams capabilities.
- Existing users of the same `jira-cli` and `confluence-cli` family who expect consistent installation and interaction patterns.

## Product Purpose

Make everyday Microsoft Teams work—finding conversations, reading context, sending messages, and moving between teams and channels—pleasant from the terminal without sacrificing automation safety. Success means the common path feels faster and calmer than opening the desktop client, while every automated operation remains explicit and dependable.

## Positioning

A human-first and agent-first Microsoft Teams terminal client: a polished TUI and exceptional onboarding share one typed command model with CLI Spec v0.3 introspection, bounded structured output, stable errors, and explicit effects. Neither audience is a compatibility layer over the other. It uses supported Microsoft Graph APIs rather than extracted desktop tokens or undocumented Teams protocols.

## Operating Context

- Installed from PyPI, preferably with `pipx install teams-cli`, despite the implementation being Rust.
- Invoked as `teams` alongside sibling tools such as `jira` and `confluence`.
- Used interactively in a local terminal, non-interactively in shell pipelines, and by AI agents.
- Authenticates a Microsoft 365 work or school account through the maintained teams-cli Entra public-client application, with an explicit override for customer-owned registrations.
- Opens the native or web Teams client for experiences that Microsoft Graph does not expose, such as normal user calls.

## Capabilities and Constraints

- Browser-based delegated OAuth with PKCE is the preferred login; device-code flow is an explicit headless fallback.
- Everyday commands use a user-consentable baseline; administrator-consented channel history is a deliberate login opt-in.
- Refresh credentials must use OS-protected storage and must never be written as plaintext tokens.
- Initial product surface covers onboarding, authentication diagnostics, identity, teams, channels, chats, messages, and the TUI foundation.
- Commands offer human text and structured JSON output, with clean stdout/stderr separation.
- The command contract follows CLI Spec v0.3, including offline schema introspection, declared effects, bounded output, and stable error kinds and exit codes.
- Microsoft Graph permissions and tenant policy can limit features; the product must explain these failures in task language.
- Personal Microsoft accounts are not supported by the relevant Teams Graph APIs.
- Calling, screen sharing, and private Teams protocols are outside the supported core.
- The exact long-term scope beyond everyday messaging and navigation remains open.

## Brand Commitments

- Package name: `teams-cli`.
- Executable name: `teams`.
- The experience belongs to the same practical, polished family as `jira-cli` and `confluence-cli`.
- Voice is concise, calm, capable, and candid about permissions and effects.
- CLI Spec v0.3 is a binding design reference, not merely documentation.

## Evidence on Hand

- The workspace begins empty; there are no incumbent visual assets or customer claims to preserve.
- Microsoft Graph documentation confirms delegated support for core work/school Teams scenarios.
- Existing alternatives demonstrate demand but split between broad administrative CLIs, agent-first Graph wrappers, and clients using unsupported private protocols.
- No testimonials, usage benchmarks, or adoption claims are available and none should be fabricated.

## Product Principles

1. Names before IDs: accept the way people think, while preserving exact identifiers for automation.
2. One truthful contract: parsing, help, schema, effects, errors, and rendering must not drift apart.
3. Safe without ceremony: reads flow quickly; writes are explicit, previewable, and retry-aware.
4. Supported and secure by construction: use public Microsoft APIs, least privilege, and protected credentials.
5. Terminal-native craft: speed, hierarchy, keyboard flow, copy, and empty states are product features.
6. Dual-first, not lowest-common-denominator: interactive and machine surfaces may render differently, but share semantics, capabilities, and quality.

## Accessibility & Inclusion

The CLI and TUI must remain usable without color, respect `NO_COLOR`, avoid color-only state, support narrow terminals, expose full keyboard operation, and provide equivalent non-interactive commands for TUI actions.
