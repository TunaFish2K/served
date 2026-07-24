# served Context

Status: accepted implementation context for attach and output history behavior.

## Glossary

- **Second buffer** means the terminal alternate screen, not a manager-side output
  history or a nested terminal emulator.
- **TTY service** means a service with `tty: true` and a manager-owned PTY. Its
  attach session is bidirectional and has one writer.
- **Pipe service** means a service with `tty: false`. Its attach session is a
  read-only live output viewer.
- **Attach** is the same named raw socket handoff for the TUI `a` action and the
  `served attach [name]` command.
- **Output history** is manager-owned stdout/stderr or PTY output captured per
  process run. It is not shell command history and is not a replayable terminal screen.
- **Persistent log** is a complete raw run file below the XDG state directory. A
  non-persistent log is a bounded in-memory run record.

## Confirmed Decisions

- Direct attach enters an alternate screen, clears it, enables raw mode, and
  restores the shell screen and terminal mode on detach, EOF, or error.
- TUI attach keeps ownership of the TUI's existing alternate screen. It clears the
  screen for the service session and fully redraws the manager after detach; it
  does not nest alternate-screen ownership.
- Only attach clients enter the alternate screen. `tty` continues to control PTY
  allocation and does not cause the manager to inject terminal control sequences
  when a service starts.
- Attach displays only output received after the connection starts. Output history
  is exposed by a separate TUI history page and the `served history` CLI command;
  it is not replayed into attach.
- Pipe attach forwards stdout/stderr as raw bytes, ignores input, and permits
  multiple observers. PTY attach remains bidirectional and single-writer.
- `Ctrl-C` detaches without forwarding the byte to either service type.
- The main TUI no longer renders a recent-output panel and shows `a attach` for
  either TTY mode.
- `persist_logs` defaults to `false` and takes effect on the next process start or
  restart. Persistent logs use `$XDG_STATE_HOME/served/logs/<service>/`, keep
  `latest.log` plus 100 archives, and use private `0700`/`0600` permissions.
- Every process start rotates an existing `latest.log` using `.latest.started`, even
  when the new run is non-persistent, so the history model has one `latest` record.
- TTY and pipe output are both captured as raw bytes. History display removes ANSI and
  other unsafe control sequences; raw files remain unchanged.

## Compatibility

The `Request::Attach { name }` wire shape and protocol version remain unchanged.
The manager selects the PTY or pipe relay from the enabled service configuration.
History requests use the same manager IPC and require a current manager binary.
