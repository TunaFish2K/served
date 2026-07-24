# served Context

Status: accepted implementation context for the current attach behavior.

## Glossary

- **Second buffer** means the terminal alternate screen, not a manager-side output
  history or a nested terminal emulator.
- **TTY service** means a service with `tty: true` and a manager-owned PTY. Its
  attach session is bidirectional and has one writer.
- **Pipe service** means a service with `tty: false`. Its attach session is a
  read-only live output viewer.
- **Attach** is the same named raw socket handoff for the TUI `a` action and the
  `served attach [name]` command.

## Confirmed Decisions

- Direct attach enters an alternate screen, clears it, enables raw mode, and
  restores the shell screen and terminal mode on detach, EOF, or error.
- TUI attach keeps ownership of the TUI's existing alternate screen. It clears the
  screen for the service session and fully redraws the manager after detach; it
  does not nest alternate-screen ownership.
- Only attach clients enter the alternate screen. `tty` continues to control PTY
  allocation and does not cause the manager to inject terminal control sequences
  when a service starts.
- Attach displays only output received after the connection starts. The manager's
  bounded `output_tail` remains available for future history work but is not
  replayed or rendered by the main TUI.
- Pipe attach forwards stdout/stderr as raw bytes, ignores input, and permits
  multiple observers. PTY attach remains bidirectional and single-writer.
- `Ctrl-C` detaches without forwarding the byte to either service type.
- The main TUI no longer renders a recent-output panel and shows `a attach` for
  either TTY mode.

## Compatibility

The `Request::Attach { name }` wire shape and protocol version remain unchanged.
The manager selects the PTY or pipe relay from the enabled service configuration.
