# served Context

Status: accepted implementation context for attach, output history, and service
installation behavior.

## Glossary

- **Second buffer** means the terminal alternate screen, not a manager-side output
  history or a nested terminal emulator.
- **TTY service** means a service with `tty: true` and a manager-owned PTY. Its
  attach session is bidirectional and has one writer.
- **Pipe service** means a service with `tty: false`. Its attach session is a
  read-only live output viewer.
- **PTY size synchronization** means that an active TTY attach client can apply
  its terminal rows and columns to the manager-owned PTY. It is controlled by
  `syncRowsCols` and does not replay terminal screen state.
- **Attach** is the same named raw socket handoff for the TUI `a` action and the
  `served attach [name]` command.
- **Output history** is manager-owned stdout/stderr or PTY output captured per
  process run. It is not shell command history and is not a replayable terminal screen.
- **Attach snapshot** is a sanitized display-only tail of the current run: the most
  recent 48 logical lines, capped at 16 KiB. It is not a terminal-state replay.
- **Persistent log** is a complete raw run file below the fixed HOME state
  directory. A non-persistent log is a bounded in-memory run record.
- **System service** means the fixed `/etc/systemd/system/served.service` unit
  managed by the system manager, not a `systemd --user` unit.
- **Installation user** means the one ordinary user selected by the installer;
  the system unit and all managed children run with this identity.
- **Fixed HOME paths** means configuration under `$HOME/.config` and state under
  `$HOME/.local/state`; `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, and
  `XDG_RUNTIME_DIR` do not select served's paths.
- **Installation-user home** is the canonical home for the installed system
  service; a normal direct daemon invocation by that user uses the same
  `$HOME`-derived paths. Deliberately overriding `HOME` is outside this contract.
- **Served environment file** means the optional `.env.served` file beside
  `.served.json`; a project `.env` file is outside served's configuration.

## Confirmed Decisions

- Direct attach enters an alternate screen, clears it, enables raw mode, and
  restores the shell screen and terminal mode on detach, EOF, or error.
- TUI attach keeps ownership of the TUI's existing alternate screen. It clears the
  screen for the service session and fully redraws the manager after detach; it
  does not nest alternate-screen ownership.
- Only attach clients enter the alternate screen. `tty` continues to control PTY
  allocation and does not cause the manager to inject terminal control sequences
  when a service starts.
- Attach starts with the current run's sanitized 48-line snapshot, then displays
  live output. The snapshot is display-only and is not sent to the service PTY.
  Full output history remains exposed by the separate TUI history page and the
  `served history` CLI command; history records are not replayed as terminal state.
- Pipe attach forwards stdout/stderr as raw bytes, ignores input, and permits
  multiple observers. PTY attach remains bidirectional and single-writer.
- `Ctrl-C` detaches without forwarding the byte to either service type.
- The main TUI no longer renders a recent-output panel and shows `a attach` for
  either TTY mode.
- `persist_logs` defaults to `false` and takes effect on the next process start or
  restart. Persistent logs use `$HOME/.local/state/served/logs/<service>/`, keep
  `latest.log` plus 100 archives, and use private `0700`/`0600` permissions.
- Every process start rotates an existing `latest.log` using `.latest.started`, even
  when the new run is non-persistent, so the history model has one `latest` record.
- TTY and pipe output are both captured as raw bytes. History display removes ANSI and
  other unsafe control sequences; raw files remain unchanged.
- The structured editor exposes actual command newlines as separate rows. CRLF and
  standalone CR are normalized to LF when editing; actual LF remains `/bin/sh -c`
  script syntax.
- `syncRowsCols` defaults to `true`, is shown as a choice row in the structured
  editor, and takes effect on the next service start or restart. It is a no-op for
  pipe services.
- Resize control uses a separate long-lived framed manager connection, an opaque
  attach token, and a versioned protocol. The client sends the initial terminal
  size immediately, polls for changes, and reconnects with backoff after control
  failures. The last PTY size remains after detach; a newly started PTY uses its
  default size.
- The system service runs as the installation user and uses that user's canonical
  home for its login environment and working directory; system-manager home
  expansion is not part of the service path contract.

## Compatibility

The `Request::Attach { name }` request remains named and the manager selects the
PTY or pipe relay from the enabled service configuration. The protocol version is
2 because attach now returns an opaque token and resize control is an explicit
`Request::Resize` message on a separate framed connection. Version mismatch is
rejected during the handshake; raw PTY bytes are never mixed with control frames.
History requests use the same manager IPC and require a current manager binary.
