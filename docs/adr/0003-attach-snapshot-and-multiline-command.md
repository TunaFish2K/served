# ADR 0003: Attach Snapshot and Multiline Command Editing

- Status: Accepted
- Date: 2026-07-24

## Context

Attach previously began at the live broadcast boundary, so a user connecting to
a busy service had no immediate context. The manager already retained a bounded
current-run output buffer, but replaying raw PTY bytes would not reliably recreate
a terminal screen.

The structured editor also decoded JSON `\n` escapes into actual newlines while
placing the whole command into a one-row field. Existing multiline shell scripts
could therefore contain invisible line breaks and pasted scripts were difficult to
inspect.

## Decision

- Direct attach and TUI attach both begin with the current run's latest 48 logical
  output lines, aggregated across output events and capped at 16 KiB.
- The snapshot removes ANSI and unsafe control sequences, normalizes CR/CRLF to
  LF, emits terminal-friendly CRLF separators, and adds a boundary newline before
  live output when needed.
- The snapshot is display-only. It is never sent to a PTY, appended to logs, or
  interpreted as cursor/screen state. Pipe observers each receive their own copy.
- The worker subscribes to live output before writing the snapshot, preventing a
  gap between replay and the live broadcast. The raw attach protocol and version
  remain unchanged.
- The command editor splits actual LF characters into visible TextArea rows, uses a
  bounded dynamic command area, supports bracketed paste, and normalizes CR/CRLF to
  LF on edit. Actual LF remains `/bin/sh -c` script syntax.

## Consequences

Attach has useful recent context without becoming a terminal emulator. Colors and
cursor behavior from the cached output are intentionally not reproduced, while
live output keeps its existing raw behavior. A literal backslash-n argument remains
distinct from an actual newline and must use the corresponding JSON escaping.
