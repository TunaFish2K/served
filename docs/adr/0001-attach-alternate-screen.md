# ADR 0001: Attach Alternate Screen and Pipe Observers

- Status: Accepted
- Date: 2026-07-24

## Context

served has two attach entry points: the global TUI and the direct
`served attach [name]` command. PTY services already expose a bidirectional raw
socket relay. Pipe services only collected output into an in-memory ring buffer.

The product needs attach sessions to use a terminal second buffer while keeping
the manager TUI recoverable. It also needs a useful read-only attach path for
services that intentionally do not allocate a PTY.

## Decision

- The second buffer is the terminal alternate screen.
- Direct attach owns and restores its alternate screen.
- TUI attach reuses the TUI-owned alternate screen, clears it for the session, and
  redraws the TUI after detach. Nested alternate-screen ownership is not used.
- `tty: true` remains the only mode that accepts service input and remains limited
  to one attach writer.
- `tty: false` gets a raw stdout/stderr broadcast with ignored input. Multiple
  read-only observers are allowed.
- Attach starts at the live stream boundary. The ring buffer is retained but is not
  interpreted as a terminal screen or replayed into attach.

## Consequences

The terminal cleanup path is explicit for both direct and TUI attach. Pipe attach
does not require a fake PTY or a new protocol message, but it is a live viewer and
does not reconstruct an already-rendered screen. The existing `output_tail` remains
available for a later history feature.
