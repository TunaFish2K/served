# ADR 0004: Attach PTY Window-Size Synchronization

- Status: Accepted
- Date: 2026-07-25

## Context

PTY processes start with a manager-owned default size. A terminal application
such as Vim reads that size from the PTY, so a client attached from a terminal
with different dimensions renders with the wrong number of rows and columns.
The raw attach stream is already a bidirectional byte handoff and cannot safely
carry manager control messages alongside user input and service output.

## Decision

- Add `syncRowsCols` to `.served.json`, defaulting to `true` for existing and new
  configurations. The field is visible in `served edit` as an Enabled/Disabled
  choice after TTY. It is stored but has no effect for `tty: false`.
- Editing the field only writes configuration. The value takes effect on the next
  service start or restart; `served edit` does not restart a running service.
- Bump the manager protocol to version 2. Attach returns an opaque session token.
  Resize requests contain the service name, token, and positive `cols`/`rows`.
  Protocol mismatches fail during the existing handshake.
- Keep resize control on a separate long-lived framed manager connection. The
  attach client sends its current terminal size immediately, polls
  `crossterm::terminal::size()` about every 250ms, and sends only changes.
- Control connection failures are nonfatal to raw attach. The client reconnects
  with backoff and resends the current dimensions after reconnecting.
- The manager applies a valid resize only for a running TTY service with matching
  active attach token and enabled synchronization. Pipe services, stale tokens,
  disabled synchronization, and absent attach sessions are successful no-ops;
  missing services, stopped services, and zero dimensions are errors.
- Detach leaves the last PTY size in place. A newly created PTY starts with the
  default size.

## Alternatives Considered

- Put resize frames in the raw attach stream: rejected because arbitrary PTY
  input/output bytes must remain opaque and compatible with interactive programs.
- Open a new manager connection for every size change: rejected because it adds
  connection churn and makes ordering/reconnect behavior harder to reason about.
- Use terminal resize signals or a second stdin reader: rejected because the
  client can poll dimensions without competing with the existing raw input loop.

## Consequences

Interactive TUI programs receive the attaching terminal's dimensions and can
render correctly after terminal resizes. The manager has a small versioned control
surface and token validation to prevent delayed resize requests from an old attach
session affecting a new one. A lost control connection does not interrupt the
interactive session, but a short delay may occur before a resize is applied.
