# served Technical Stack

Status: implementation baseline

This document records the implementation choices for V1. The product is a
Linux-first, per-user process manager. "Minimal" means a small operational
surface and simple ownership boundaries, not the fewest possible crates.

## Runtime

| Area | Choice | Boundary |
| --- | --- | --- |
| Language | Rust stable | `#![forbid(unsafe_code)]` in first-party code |
| Build | Cargo, committed `Cargo.lock` | One package, one `served` binary |
| Async runtime | Tokio | Unix socket server, process supervision, timers |
| Manager model | One actor owning service state | Worker tasks report typed lifecycle/output events |
| Service command | `/bin/sh -c <command>` | Working directory is the service directory |
| Process without PTY | `tokio::process` and async pipes | Used when `.served.json` has `tty: false` |
| Process with PTY | `portable-pty` | Default path; master remains owned by the worker |

The manager runs once per user as `systemd --user` service. Managed children
remain in that user service's cgroup. `served` does not become a root daemon,
container runtime, namespace manager, or resource policy engine.

## Configuration and State

- `serde` and `serde_json` parse the direct-object `.served.json` format.
- `dotenvy` parses the fixed `.env` file using dotenv semantics. The file is
  parsed as data and is never sourced by a shell.
- The manager captures its startup environment and overlays `.env` values for
  each service.
- The enable registry is symlinks in
  `$XDG_CONFIG_HOME/served/enabled/<name>` (or `~/.config/...`).
- Service output is kept in a bounded in-memory ring buffer only.
- No manager state JSON or persisted tip cursor is used in V1.

## IPC

- Control commands use a Unix domain socket below `$XDG_RUNTIME_DIR`.
- Frames are length-prefixed JSON messages through `tokio-util`.
- Every connection starts with a protocol-version handshake.
- PTY attach switches the already-authenticated connection to a raw
  bidirectional byte stream. The manager keeps one attach writer per service.
- The socket is created with user-only filesystem permissions and is never
  exposed over TCP.

## CLI and TUI

- `clap` derive defines the single-binary command surface.
- `ratatui` and `crossterm` render the service list and editor.
- `tui-textarea` supplies text fields for the structured editor, including the
  `.env` buffer.
- `rand` supplies a non-cryptographic random tip selection on each TUI start.
- The first TUI screen is the global enabled-service list. It exposes status,
  recent output, restart, disable, attach, and the single `tips:` line. A
  contextual two-line operation bar stays visible below the tip and indicates
  which actions are available for the current selection; pipe services expose
  attach as unavailable.
- `served edit` keeps the JSON and fixed `.env` buffers in `tui-textarea` fields
  and renders `TTY` and `restart` as ordinary visible choice rows. The visual
  order is also the keyboard focus order. Enter opens an in-memory popup for a
  choice; Enter applies it and Esc discards it. Tab and Shift-Tab move through
  all five fields.
- `served attach [name]` reuses the existing name-based raw socket handoff. An
  omitted name is resolved client-side from the canonical current directory and
  the manager's enabled-service list. Direct attach uses crossterm raw mode
  without alternate-screen rendering and restores the terminal on exit.
- The shared attach relay treats `Ctrl-C` as detach for both direct and TUI
  attach; it is not forwarded to the service.

## Errors, Logging, and Tests

- `anyhow` is used at application boundaries.
- `thiserror` defines errors that are useful to callers and tests.
- `tracing` plus `tracing-subscriber` logs manager lifecycle events to stderr
  and the systemd journal.
- Unit tests cover configuration, dotenv overlay, registry behavior, protocol
  framing, and restart backoff.
- Integration tests use `tempfile` and `assert_cmd`; Linux release smoke tests
  additionally exercise a real `systemd --user` installation when available.
- TUI rendering tests use Ratatui's `TestBackend`; snapshots are optional and
  are not required for the first implementation slice.

## Packaging

- Pushing a matching `v<semver>` tag runs the GitHub release workflow.
- The workflow targets Linux amd64 with glibc and publishes a binary-only asset
  plus a full offline installation package.
- Each asset has its own SHA-256 sidecar named by appending `.sha256` to the
  original filename.
- The full package contains the glibc-linked binary, the user unit,
  `install.sh`, `uninstall.sh`, and `README.md`.
- Shell scripts own user-unit installation, `daemon-reload`, enable/start, and
  linger setup. Rust does not shell out to `systemctl`, `loginctl`, or D-Bus.
- V1 targets Linux with glibc first. Other platforms are not a compatibility
  promise.

## Dependency Policy

The dependency set is intentionally conventional and mature. New crates need
to remove meaningful complexity or satisfy a concrete boundary; they should
not be added only to wrap a few lines of local code.
