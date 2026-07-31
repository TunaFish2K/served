# served

[简体中文](README.zh-CN.md)

`served` deploys an existing project directory as a long-running service for personal, non-critical
use. A systemd system unit starts the manager as the installation user.
served manages host processes. It does not run containers.

This V1 release targets Linux with glibc.

## What served Does

served manages a project directory that already exists. You prepare the project, its command, and
its dependencies. You can run the project directly for local tests. Use served after you decide to
keep the project running.

served does not upload project files. It does not build projects or install dependencies. It does
not provide root service management, namespaces, resource limits, or health checks.

## Who Should Use served

Use served for a personal service that can stop without affecting basic host maintenance. Good
examples include:

- bots
- webhooks
- personal APIs
- workers

Do not use served for a host-critical service. Do not use it for `sshd`, login services, network
services, or any service that you need to maintain the host.

## Quick Deployment

You need Linux with glibc, a systemd system manager, and working `sudo` access. The full release
package is the recommended installation method.

1. Download the `full.tar.gz` package from a GitHub Release.
2. Extract the package and enter its directory.
3. Run `./install.sh`.
4. Enter your project directory.
5. Run `served edit` to create and edit `.served.json`.
6. Run `served enable` to enable the project service.

The installer enables and starts `served.service`. This unit starts the served manager. It does
not enable project services. `served enable` adds the current project directory and starts its
service.

Check the service after installation:

```bash
served list
served attach <name>
```

Run `served restart` after you update the project. Use `served attach`, `served history`,
and persistent logs to investigate service failures. served does not upload or build the project.

The full package contains these files:

```text
served
served.service
install.sh
uninstall.sh
README.md
README.zh-CN.md
```

## Common Commands

Run `served edit` in a project directory:

```text
served                 Open the global service TUI
served daemon          Run the manager with fixed paths
served edit            Open .served.json in an external editor
served edit -e <cmd>   Use the specified editor command
served edit --path     Create a missing template and print its path
served enable          Enable and start the current service
served disable [name]  Disable the current or named service
served restart [name]  Restart the current or named service
served attach [name]   Attach to the current or named service
served history [name]  Open latest.log in an editor
served history [name] --run <id>
                       Open a selected archived log
served history [name] -e <command>
                       Use the specified editor command
served history [name] --path
                       Print the selected persistent log path
served list            List services managed by the manager
```

Commands without a name use the service for the current directory. Commands with a name work from
any directory.

Use `served disable` when you no longer want to manage a project. Use `served restart`
after you change its configuration. served does not provide separate service-level `start`,
`stop`, or `reload` commands.

## Service Configuration

Run `served edit` in the service directory. If `.served.json` does not exist, served creates
a commented JSON5 template and opens it in your editor. served does not rewrite or format an existing
file.

```json5
{
  name: "api",
  command: "python app.py",
  tty: true,
  syncRowsCols: true,
  restart: "never",
  persist_logs: false,
  env: {
    // PORT: "8080",
  },
}
```

JSON5 accepts comments, single or double quoted strings, unquoted field names, and trailing commas.
The template explains every supported field.

- `name` is the unique name for the enabled service. Use only letters, numbers, `.`, `_`,
  and `-`.
- `command` is a shell command string. served runs it with `/bin/sh -c`.
- `command` can contain a multi-line shell script. In a JSON5 string, `\n` means a real
  newline. Write `\\n` when an argument must contain the literal characters `\n`.
- `tty` is optional and defaults to `true`. Set it to `false` to use pipe mode.
- `syncRowsCols` is optional and defaults to `true`. For a TTY service, served applies the
  current terminal size to the service PTY. The field has no effect when `tty: false`.
- `restart` is optional and defaults to `never`. Valid values are `never`, `on-failure`,
  and `always`.
- `persist_logs` is optional and defaults to `false`. Set it to `true` to save the
  complete output for each run under `$HOME/.local/state/served/logs/<name>/`. The setting takes
  effect after the next start or restart.
- `env` is an optional object of literal string values. served does not expand shell variables
  in these values. JSON5 `env` values override the manager environment and old `.env.served`
  values with the same key.

`.env.served` is the only supported environment file. It must be in the service directory.
New templates do not create or edit this file. served reads an existing file with dotenv rules for
backward compatibility. JSON5 `env` values override duplicate keys.

The manager records its environment when it starts. A service receives values in this order:

1. The manager environment.
2. Values from the old `.env.served` file.
3. Values from JSON5 `env`.

Changes to shell startup files such as `/etc/profile` do not update a running manager.

## Attach and TUI

The global TUI shows service state and provides restart, disable, attach, history, and rotating
`tips:` messages. The footer shows the available actions. A narrow terminal can wrap the
footer to two lines.

TTY services provide a writable PTY attach. Pipe services provide a read-only attach. Pipe services
can have multiple read-only observers. Both modes use the terminal's alternate screen.

Run `served attach [name]` to attach without opening the service TUI. Without a name, served
uses the enabled service for the current directory. With a name, served can attach from any directory.
The target service must be running.

An attach session first shows the latest 48 cleaned logical lines from the current run. It then
shows live output. The snapshot is limited to about 16 KiB. served does not send it to the service.
The snapshot does not reproduce PTY screen state. served restores the previous shell or TUI screen
when the session ends.

A `tty: true` session sends input to the service PTY. A `tty: false` session forwards
snapshot data and live stdout/stderr and ignores input. Press `Ctrl-C` to leave attach. served
does not stop the service, and it does not send this key to the service.

For a `tty: true` service, attach applies the terminal `rows` and `cols` values when the
session starts and when the terminal changes. Set `syncRowsCols: false` to keep the initial PTY
size. A control connection can fail without stopping raw attach. The client reconnects in the
background and sends the current size again. Detach does not reset the PTY size.

The main TUI does not edit service configuration. Use `served edit` to open `.served.json`
in an external editor. `-e/--editor COMMAND` takes priority over `$EDITOR`. The editor command
can contain arguments. served adds the configuration path as the last argument. `--path` creates
a missing template and prints its absolute path. `--path` conflicts with `--editor`.

## Logs and Troubleshooting

A runner records non-zero exits and worker start or run errors in a rolling 60-second window. After
three failures, served reports a recent crash loop when attach finds that the service is not running.

The direct attach command asks `Open latest.log? [y/N]` only in an interactive terminal. The TUI
uses `y` or `Enter` to open the file. Use `n` or `Esc` to cancel. A log path
exists only when the current run has persistent logs. Otherwise, use the TUI history browser or
enable `persist_logs`.

served opens logs with `$EDITOR`. After the editor exits, attach returns the original service
not running error. It does not retry attach. This warning appears only when attach fails. It does
not change the service list.

Press `h` in the TUI to select `latest` or a time archive. Press `Enter` to view
cleaned log content. The history page supports the arrow keys, `j/k`, `PgUp/PgDn`, and
`g/G`. It shows the current logical line position as `current/total`. Visual wrapping
does not change the total line count. History stays separate from attach. Attach does not replay old
PTY control state.

The command `served history` opens the selected persistent raw log. Without `--run`, it
selects `latest`. `-e/--editor COMMAND` takes priority over `$EDITOR`. The editor
command can contain arguments. served adds the log path as the last safely quoted argument.
`--path` prints only the path. It conflicts with `--editor`. A non-persistent record has
no file path. Use the TUI to view it or enable `persist_logs`.

Each process start creates a separate history record. This includes automatic and manual restarts.
The runner owns the history, so a manager restart does not remove it. Persistent logs use:

```text
$HOME/.local/state/served/logs/<name>/
```

The current run writes to `latest.log`. At the next start, served archives the old run by its
start time as `YYYYMMDD-HHMMSS.log`. If names conflict, served adds `-1`, `-2`,
and so on. `.latest.started` stores the current start time. Each service keeps up to 100
archives and one latest file. The log directory uses mode `0700`. Log files use mode `0600`.

With `persist_logs: false`, served does not add disk logs. The runner keeps the current record
and the latest 100 memory archives during its lifetime. A manager restart keeps these records. A
runner or service restart starts a new current record.

TTY history stores raw PTY bytes. Pipe history merges stdout and stderr in runner event order. The
history view removes ANSI and invisible control sequences. If persistent storage fails, the service
continues with memory history and the manager records a warning.

## Installation, Upgrade, and Uninstall

The repository stores the installer in `scripts/` and the system unit template in `systemd/`.
The installer runs as a normal installation user. It uses `sudo` when needed to install
`/usr/local/bin/served` and `/etc/systemd/system/served.service`. It then runs a system
scope `daemon-reload`, enables the unit, and starts the manager. The Rust program does not
call `systemctl` or D-Bus.

The installer puts the executable at `/usr/local/bin/served`. It does not change shell startup
files. You can run `served` from any directory after installation.

The first installation enables and starts `served.service` at `multi-user.target`. If the
executable or unit already exists, the installer asks before it performs an overwrite upgrade.
After installation, it asks whether to run `systemctl reload` for a manager handoff. The
default answer accepts the handoff. A successful handoff does not stop runners or managed services.
If the handoff fails, the installer uses a controlled restart. If you decline, the old manager keeps
running and the installer prints a command for a later reload. If the upgrade fails, the installer
tries to restore the old files and service.

If the service was inactive or failed before an upgrade, the upgrade keeps it stopped. The installer
prints the matching `systemctl start` or `systemctl enable --now` command. Pressing
Enter accepts the overwrite handoff. Uninstall uses `y/N`, and Enter cancels. After uninstall
confirmation, the installer disables the unit before it stops the running service. It deletes the
installed files only after the stop succeeds. It does not delete configuration or state. It does not
change shell configuration. Non-interactive use skips actions that require confirmation.

The installer detects old `~/.config/systemd/user/served.service` and `~/.local/bin/served`
files. After migration confirmation, it stops and disables the old user service. It deletes old files
only after the new system service becomes active. If the old user manager is not available, migration
stops without deleting old files. Custom XDG directories receive a migration notice. The installer
does not copy or delete them automatically.

The installed systemd service uses `systemctl reload served` for manager handoff. A manager
upgrade does not restart managed services. `systemctl restart served` and
`systemctl stop served` are explicit lifecycle actions. They stop runners. If the manager
exits unexpectedly, systemd starts it again and the runners continue. The new manager adopts them.

## Release Downloads

Push a `v<semver>` tag that matches the version in `Cargo.toml` to create a GitHub
Release. The current workflow builds Linux amd64 with glibc.

For version `0.1.8`, the release contains:

```text
served-linux-amd64-v0.1.8-binary
served-linux-amd64-v0.1.8-binary.sha256
served-linux-amd64-v0.1.8-full.tar.gz
served-linux-amd64-v0.1.8-full.tar.gz.sha256
```

The `binary` asset contains only the executable. The `full.tar.gz` asset contains the
executable, system unit, installer, uninstaller, and both README files. Each asset has its own
SHA-256 sidecar file with `.sha256` appended to the original file name.

The workflow does not build ARM, musl, or other operating systems.

## Security and Limits

- The manager runs as a normal user. The manager socket is readable and writable by that user.
- The systemd system unit starts and supervises the manager as the installation user.
- Each enabled service has an independent runner. The manager adopts it through a private runner
  socket.
- After a manager or system unit restart, the manager scans the enabled registry and adopts existing
  runners first.
- A runner at `$HOME/.local/state/served/runtime/runners/<name>/` owns the service process,
  PTY, log cache, restart state, and crash-loop window. A manager crash does not stop these items.
- `systemctl stop served` performs a graceful shutdown for all runners. `served disable`
  and `served restart` stop or replace the matching runner. A systemd reload uses manager
  handoff and keeps the service PID. A first upgrade from the old worker architecture may need one
  controlled restart.
- The system service sets `HOME` from the installation user's login environment. It starts
  the manager with a login shell, so files such as `/etc/profile` load when the manager starts.
  The manager keeps that environment snapshot until it restarts.
- The system service uses the installation user's home as its working directory. It does not use the
  system manager's `%h` expansion.
- The runner sends `SIGTERM` first. It sends `SIGKILL` after the timeout. A manager
  crash does not run this cleanup path. The runner ends managed shells only for explicit shutdown,
  disable, or restart actions.
- Detached child processes created with `nohup`, background commands, or daemonization are
  outside the cleanup guarantee.
- V1 does not provide root mode, container isolation, namespaces, resource limits, dependency graphs,
  or health checks.

## Maintainer Build

These commands build and check served itself. You do not need a Rust toolchain to deploy a personal
project. Use a full release package for personal deployment.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Core requirements are in [REQUIREMENTS.md](REQUIREMENTS.md). Technical decisions are in
[TECH-STACK.md](TECH-STACK.md).

## License

served is released under the [Unlicense](LICENSE). You can use, copy, modify, publish, and
distribute it without licensing restrictions. The software is provided without warranty.
