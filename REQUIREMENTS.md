# served Requirements

Status: product requirements draft

`served` is a lightweight Linux-first service manager for local development. It
manages host processes through a per-user `systemd --user` manager. It is not a
container runtime and does not manage root services.

## Core Model

- One service is represented by one directory.
- The directory contains the service definition `.served.json`.
- The optional environment file is `.env` in the same directory.
- The service working directory is always its configuration directory.
- The manager discovers services through user-owned enable links.
- A service name is defined by the JSON `name` field and must be globally unique
  among enabled services.
- Renaming an enabled service requires `disable`, editing the name, and then
  `enable` again.

The enable registry is:

```text
~/.config/served/enabled/<name> -> /path/to/service-directory
```

The link points to the service directory, not directly to the JSON file, so the
manager can consistently load `.served.json`, `.env`, and the working directory.

## Configuration

`.served.json` is a direct JSON object for one service. The minimum shape is:

```json
{
  "name": "api",
  "command": "python app.py",
  "tty": true,
  "restart": "never"
}
```

Fields:

- `name`: required, globally unique among enabled services.
- `command`: required shell command string.
- `tty`: optional boolean, default `true`.
- `restart`: optional policy, default `never`.
- Supported restart policies: `never`, `on-failure`, and `always`.

Commands are executed through `/bin/sh -c`. Inline shell assignments such as
`FOO=bar command` require no special configuration support.

The service environment starts with the environment of the `systemd --user`
manager. If `.env` exists, it overlays that environment. `.env` is parsed using
standard dotenv semantics; it is not sourced as a shell script and does not
support arbitrary file paths or `env_file` configuration.

The manager environment is a startup snapshot. Changes to `/etc/profile` or
other shell startup files are not expected to update running services until the
user manager is restarted or its environment is explicitly refreshed. `.env`
uses standard dotenv parsing (including comments, quoting, and supported
variable expansion), but it is still data and is never shell-sourced.

## Commands

### `served`

Opens the global service-management TUI. It lists enabled services known to the
manager, regardless of the current working directory.

If the current directory contains a service configuration that has not been
enabled, the TUI may show a hint such as:

```text
enable your service to manage it here!
```

An unenabled service is not manageable by the manager.

### `served edit`

Opens the structured TUI editor for the current directory's `.served.json` and
`.env`. If the files do not exist, the editor can create templates. Editing
changes files only; it does not apply a running-service change automatically.

The JSON editor is form-based. The `.env` editor manages the fixed same-directory
file. The fields are rendered from top to bottom in this order: `name`, `command`,
`TTY`, `restart`, and `.env`. `Tab` moves forward and `Shift-Tab` moves backward.
The `TTY` field displays `Enabled` or `Disabled`; the `restart` field displays
`never`, `on-failure`, or `always`.

When `TTY` or `restart` has focus, `Enter` opens a selection popup. Up/down arrow
keys or `j`/`k` move its temporary highlight. `Enter` applies the highlighted
value, while `Esc` closes the popup without applying it. In the normal editor,
`Ctrl-S` saves and `Esc` or `Ctrl-C` cancels the edit. The bottom operation bar
shows the controls available for the current focus.

### `served enable`

Only valid in a service directory.

1. Read and validate `.served.json` and `.env`.
2. Reject missing or invalid configuration.
3. Reject a duplicate global service name.
4. Create the user-level enable link.
5. Start the service under the manager.

There is no separate `start` command.

### `served disable [name]`

Removes the enable link and stops the service. With no name, the current
directory is used. With a name, the enabled service can be controlled from any
directory.

There is no separate `stop` command.

### `served restart [name]`

With no name, operates on the current service directory. With a name, operates
on an enabled service from any directory.

Restart always reads the current `.served.json` and `.env`, validates them, then
stops and starts the service. There is no separate `reload` operation.

Validation must complete before the old process is stopped. Invalid JSON or
`.env` leaves the currently running service unchanged and reports the error.

### `served attach [name]`

Attaches directly to a running PTY service without opening the service-management
TUI. With no name, the current directory is canonicalized and matched against an
enabled service directory. With a name, the enabled service can be attached from
any directory.

The target must be running. The command uses the current terminal in raw mode and
enters the terminal alternate screen, clearing it before streaming live output.
The original shell screen and terminal mode are restored when the session ends.
For `tty: true`, the session forwards input to the PTY and only one client may
attach. For `tty: false`, the session is read-only: stdout and stderr are forwarded
as raw bytes, input is ignored, and multiple observers are allowed. `Ctrl+C`
detaches from either session and is not forwarded to the service. Detaching does
not stop or disable the service.

### `served list`

Lists services currently running under the manager.

## Process Lifecycle

- A service is represented by the shell process started for its command.
- Descendants created through `nohup`, `&`, daemonization, or similar behavior
  are outside the service guarantee and are not promised to be cleaned up.
- Stop operations first send `SIGTERM` to the managed shell.
- If it does not exit before the termination timeout, the manager sends
  `SIGKILL`.
- `restart=never` leaves an exited service stopped until an explicit restart.
- `restart=on-failure` restarts non-successful exits.
- `restart=always` restarts every exit.
- Automatic restarts use exponential backoff with a maximum delay and continue
  retrying.
- A manual restart resets the service's restart attempt state.

## PTY and Attach

- Services use a PTY by default.
- A service may opt out with `tty: false`.
- TUI attach takes over the TUI-owned alternate screen, clears it, and redraws the
  service manager after detach; it does not nest another alternate-screen owner.
- Direct CLI attach enters its own alternate screen and returns to the shell after
  detach.
- PTY attach forwards input and permits one write client. Pipe attach forwards raw
  stdout/stderr only, ignores input, and permits multiple observers.
- Attach starts with live bytes received after connection; `output_tail` is not
  replayed as a terminal screen.
- `Ctrl+C` in an attach session detaches instead of being sent to the service.
- `Ctrl+C` in the service-management TUI outside an attach session exits the TUI
  client; it does not stop managed services.

## Output History

- `.served.json` has an optional `persist_logs` boolean, defaulting to `false`.
- Every process start creates a separate output record, including automatic and
  manual restarts.
- TTY and pipe services both produce history. TTY output is raw PTY bytes; pipe
  stdout/stderr are merged in manager event order.
- With `persist_logs: true`, complete records are stored under
  `$XDG_STATE_HOME/served/logs/<name>/` or `~/.local/state/served/logs/<name>/`.
- The current record is `latest.log`; old records use the previous run's start
  time in `YYYYMMDD-HHMMSS.log` format, with numeric suffixes on collisions.
- Persistent storage keeps 100 archives plus `latest.log`. Directories are `0700`
  and files are `0600`.
- With `persist_logs: false`, the current record and 100 archives remain in memory;
  manager restart clears those records. Existing disk records remain viewable.
- Persistent write failures warn and fall back to memory without stopping the service.
- History is accessed through a separate TUI list/content page and
  `served history [name]`; attach remains live-only and does not replay history.
- History content is read through paginated manager IPC and displayed after ANSI and
  unsafe control-sequence cleanup.

## TUI

The global TUI provides:

- enabled-service list and current status;
- restart action;
- disable action;
- attach action for both PTY and pipe services;
- history list and scrollable history content pages;
- structured editing through `served edit`;
- `.env` editing;
- one rotating tips line:

```text
tips: <tip text>
```

Tips are built in and a tip is selected randomly on every TUI startup. A tip
may repeat; no tip position or other manager state is persisted.

The TUI keeps the `tips:` line and the operation bar visible together. The global
operation bar is contextual: it shows navigation and quit when no service is
selected; for a selected service it shows restart, disable, attach, and history. The bar
wraps to two lines in a narrow terminal instead of being truncated.

The `served edit` screen uses the same rotating tips set and displays its own
contextual operation bar. Its focus order is the same as the visual field order
above.

The editor focus order is `name`, `command`, `TTY`, `restart`, `persist logs`, and
`.env`. Choice rows use an Enter popup and show their available keys in the bottom
operation bar.

## Manager and Security Boundary

- The manager runs as a `systemd --user` service.
- User lingering is enabled so the manager and enabled services can continue
  after logout.
- `systemd --user` only starts and supervises the manager; the manager directly
  owns the child service processes.
- TUI and commands communicate with the manager through a Unix socket under
  `$XDG_RUNTIME_DIR`.
- The socket is user-only.
- All managed services run with the same user identity as the manager.
- No root mode, privilege escalation, container isolation, namespace policy,
  resource limits, dependency graph, or health-check protocol is provided.
- If the manager user service is restarted, its child processes are cleaned up
  with the manager's cgroup. The manager then discovers enabled services again
  and starts them.

If the user manager is not installed, `served` reports that setup is required;
it does not silently start an alternate in-process manager.

## Installation Lifecycle

- The install script installs the binary into `~/.local/bin` without modifying
  user shell configuration files.
- A fresh install does not ask for an upgrade confirmation.
- If either the target binary or the user unit already exists, the install is
  treated as an overwrite upgrade and asks for confirmation before changing
  files. This also covers repairing a partial installation.
- For an overwrite upgrade of a running manager, the script asks for
  confirmation before stopping it. The stop operation does not disable the
  service, so its enabled state is preserved. If stopping is declined, the
  upgrade makes no file changes.
- If the confirmed stop fails or the service remains active, the upgrade aborts
  before changing any installation file.
- After a confirmed upgrade, the script asks whether to restart the manager,
  with restart selected by the default `Y` response.
- If restart is declined, the upgraded manager remains stopped and the script
  prints the manual `systemctl --user start served.service` command. The
  service remains enabled.
- Uninstallation asks for confirmation. After confirmation it disables the user
  service, stops it if still active, and removes the installed unit and binary
  only after both service operations succeed.
- An already-disabled service or a missing enable link satisfies the disable
  step during uninstall; only an actual systemd failure aborts cleanup.
- Uninstall never modifies user shell configuration, including PATH blocks that
  may have been written by an older release.
- Confirmation prompts require an interactive terminal; in non-interactive
  execution the script aborts without changing service state or files.
- If disabling succeeds but stopping an active service fails, uninstall aborts
  before removing any file.
- The uninstall confirmation is the only uninstall prompt; after it is
  accepted, disabling and stopping are performed without a second prompt.
- A fresh install enables user lingering; an overwrite upgrade preserves the
  user's existing lingering state and does not reconfigure it.
- If a fresh install fails, the script removes files and service enablement
  created by that attempt before exiting.
- If a failed fresh install enabled linger, it disables linger only when linger
  was disabled before the attempt; pre-existing linger remains enabled.
- Upgrade, stop, and post-install restart prompts use `Y/n` with affirmative
  default. The uninstall prompt uses `y/N` and requires an explicit `y`.
- A fresh install enables and starts the user service directly without a
  restart prompt. The post-install restart prompt applies only to overwrite
  upgrades.
- An overwrite upgrade preserves an inactive service as inactive; it does not
  start a service that was already stopped before the upgrade.
- An overwrite upgrade preserves the service's enabled or disabled state and
  does not call `enable` or `disable`; only a fresh install enables the user
  service.
- After a successful install or upgrade, the script prints an export command
  that the user can copy into the current shell to add `~/.local/bin` to PATH.
- Upgrade file replacement is transactional: if installing the new binary or
  unit fails after the old files were saved, the script restores both old files
  and leaves the service stopped.
- If the confirmed post-upgrade restart fails, the script restores the old
  binary and unit and attempts to start the old manager. If the rollback start
  also fails, the service remains stopped and both errors are reported.

## Non-Goals for V1

- Docker-compatible image or filesystem isolation.
- Root/system service management.
- Multiple services in one directory or one JSON file.
- Service dependencies or readiness checks.
- Independent `start`, `stop`, or `reload` commands.
- Arbitrary `.env` file locations.
- Automatic discovery of unrelated processes or ports.

## Acceptance Scenarios

1. `served edit` creates `.served.json` and `.env` in an empty service directory.
2. `served enable` creates a directory symlink, starts the service, and makes it
   visible in global `served` and `served list` views.
3. Enabling a duplicate `name` fails without replacing the existing link.
4. `served disable` removes the link and stops the service.
5. `served restart` applies current JSON and `.env` changes only after complete
   validation.
6. Invalid configuration leaves an already-running service untouched.
7. `never`, `on-failure`, and `always` have distinct, testable behavior.
8. A PTY service can be attached, detached, and restarted without losing the
   manager.
9. A second attach client cannot write to an active session.
10. Restarting the user manager restores all enabled services.
11. The TUI tips line selects a built-in tip randomly on every TUI startup.
12. Unenabled service directories are not controllable from the global manager.
13. The global TUI operation bar reflects whether a service is selected and shows
    attach and history for either `tty` mode.
14. `served edit` presents `TTY` and `restart` as visible fields in visual order,
    and popup selection changes are applied only after `Enter`.
15. `Esc` closes an open editor popup without changing the in-memory value, while
    `Esc` outside a popup cancels the entire edit.
16. The editor focus wraps through `name`, `command`, `TTY`, `restart`, `persist logs`,
    and `.env` in both Tab directions.
17. `served attach <name>` enters a running PTY service without opening the TUI;
    `Ctrl-C` exits the session while leaving the service running.
18. `served attach` resolves the current directory to its enabled service, and
    rejects an unenabled directory while allowing a running pipe service read-only.
19. Direct attach enters and exits the alternate screen while restoring the shell;
    TUI attach returns to a fully redrawn manager screen.
20. Multiple pipe observers receive live raw output, while pipe input does not reach
    the managed service.
21. Persistent history writes a `latest.log`, rotates it by the previous run's start
    time, and exposes both latest and archived records through CLI/manager reads.
22. Non-persistent history creates no log files, retains records across service
    restarts while the manager remains alive, and clears them after manager restart.
23. History content reads are paginated and strip ANSI/control sequences for display
    without changing the raw persistent file.
