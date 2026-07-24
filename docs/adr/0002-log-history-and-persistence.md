# ADR 0002: Per-Run Output History and Optional Persistence

- Status: Accepted
- Date: 2026-07-24

## Context

served needs useful output history without turning attach into a terminal replay
engine. A service may run with a PTY or with stdout/stderr pipes, and users may
want either temporary history or complete records across manager restarts.

## Decision

- `persist_logs` is a per-service boolean in `.served.json`, defaulting to `false`.
- Every process start creates a new run record, including automatic and manual
  restarts.
- Persistent records live under `$XDG_STATE_HOME/served/logs/<service>/`, with a
  fallback of `~/.local/state/served/logs/<service>/`.
- The active persistent record is `latest.log`. On the next process start it is
  renamed using the previous run's `.latest.started` timestamp in local
  `YYYYMMDD-HHMMSS.log` format. Name collisions receive numeric suffixes.
- Persistent files contain complete raw output. In-memory records retain a 64 KiB
  tail per run and the most recent 100 archives. Persistent storage retains 100
  archives plus `latest.log`.
- TTY output is recorded as PTY bytes. Pipe stdout and stderr are merged in manager
  event order. Display code removes ANSI and unsafe control sequences; stored bytes
  are unchanged.
- Persistence failures produce a warning and fall back to memory for that run; they
  do not stop the service.
- History is served through manager IPC, with paginated content reads. TUI `h` and
  `served history [name]` use the same records. Attach remains live-only and keeps
  its existing alternate-screen ownership.

## Consequences

The manager owns both disk and memory history, so clients do not need access to the
state directory and both storage modes have one API. Turning persistence off stops
new disk writes but does not delete old files. A manager restart clears memory-only
history while persistent files remain available. History display is safe for a
terminal but intentionally does not reproduce terminal state, colors, or cursor
behavior from a PTY session.
