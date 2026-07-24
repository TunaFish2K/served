# ADR 0001: Attach Alternate Screen and Pipe Observers

- Status: Accepted
- Date: 2026-07-24

## Context

served has two attach entry points: the global TUI and the direct
`served attach [name]` command. PTY services already expose a bidirectional raw
socket relay. Pipe services expose read-only live output, while both service types
may also produce output history.

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
- Attach begins with a sanitized display-only snapshot of the current run's most
  recent 48 logical lines, then continues with live output. The snapshot is not
  interpreted as terminal state and is not sent to the service.

## Consequences

The terminal cleanup path is explicit for both direct and TUI attach. Pipe attach
does not require a fake PTY or a new protocol message, and each observer receives
its own snapshot before the shared live broadcast. The snapshot is a cleaned output
prelude rather than a reconstructed terminal screen. History is accessed through a
separate list/content view and does not alter attach terminal state.
