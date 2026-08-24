# Terminal design

The interface is designed from actual Ratatui output, not image mockups. `teams tui --demo --snapshot` is the canonical review surface: it renders deterministic sample data through Ratatui's test backend and prints an ANSI-free frame suitable for snapshots, terminals, diffs, and agent inspection.

The visual system uses hierarchy rather than decoration: a compact coral product marker, cyan keyboard focus, high-contrast neutral content, muted metadata, one conversation rail, and a borderless reading surface. State is always conveyed by text or position as well as color. At narrow widths the rail yields to the active conversation instead of compressing the message measure.

The command interface and TUI share the same configuration, auth, Graph, and error layers. Equivalent CLI commands remain available for every network operation; the TUI is an efficient view over the product rather than a separate client.
