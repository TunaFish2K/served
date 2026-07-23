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
file.

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
- Attach enters a full-screen terminal session and forwards input to the PTY.
- Detach returns to the service TUI without stopping the service.
- Only one client may hold attach write access at a time.
- A second attach attempt is rejected rather than sharing input.
- `Ctrl+C` in the service-management TUI exits the TUI client; it does not stop
  managed services.

## TUI

The global TUI provides:

- enabled-service list and current status;
- restart action;
- disable action;
- attach action for PTY services;
- recent output from an in-memory ring buffer;
- structured editing through `served edit`;
- `.env` editing;
- one rotating tips line:

```text
tips: <tip text>
```

Tips are built in and a tip is selected randomly on every TUI startup. A tip
may repeat; no tip position or other manager state is persisted.

Service output is not persisted to disk in the first version. Manager restart
clears the in-memory output history.

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

## Non-Goals for V1

- Docker-compatible image or filesystem isolation.
- Root/system service management.
- Multiple services in one directory or one JSON file.
- Service dependencies or readiness checks.
- Persistent service logs.
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
