# Open Decisions

## Deferred: Attach History View

The manager keeps a bounded in-memory `output_tail`, but attach currently starts
with live bytes only. A future history view may expose that buffer separately from
the terminal alternate-screen session. It is intentionally outside the current
implementation and must not be treated as a replayable terminal screen without a
separate terminal-state design.
