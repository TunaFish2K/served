# served

[简体中文](README.zh-CN.md)

`served` runs an existing project directory as a long-running service for personal, non-critical
use. It manages host processes directly and does not run containers. The foreground manager can
run under any process supervisor; the repository includes optional systemd and launchd integrations.

Release binaries support macOS and Linux with glibc on amd64/x64 and arm64.

## Manuals and AI skill

Full packages and the online installer include English command and configuration manuals:

```sh
man served
man 5 served
```

The portable [served skill](skills/served/SKILL.md) helps AI agents configure and manage services.
It includes offline command and configuration references generated from the same manuals.
Download `served-v<version>-skill.tar.gz` and its SHA-256 file from GitHub Releases, or copy the
installed `/usr/local/share/served/skills/served/` directory into your AI tool's skills directory.
Copy the entire directory, including `references/`; choose the destination supported by your tool.
Re-import the skill when you upgrade served. The served installer updates its shared copy; your
imported copy belongs to you. A standalone binary download does not include these documentation files.

For example, after importing the skill, ask your agent to “use served to run an API and worker from
separate config files in the same project”, or “inspect the API logs and explain why its restart failed”.

Documentation authors edit `docs/man/served.1` and `docs/man/served.5`, then run `make docs` and commit
the generated skill references. `make docs-check` validates rendering, references, packaging, repair,
and rollback without installing system files. These targets need Python 3.9+ and mandoc.
`make skill-dist` creates the standalone archive and checksum in `dist/`.


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

For a systemd Linux host or macOS host, install the latest stable release and its native supervisor
integration with one command:

```bash
curl -fsSL https://raw.githubusercontent.com/TunaFish2K/served/main/scripts/install-online.sh | sh
```

Run the same command again to update. It detects the operating system and architecture, downloads
the matching full package and SHA-256 sidecar, verifies the checksum, and then runs the packaged
installer. Installation requires working `sudo` access. Other supervisors can use the standalone
binary and run this foreground command as the target user with that user's normal `HOME`:

```bash
served daemon
```

Use `served shutdown` for a graceful stop. Use `served daemon --handoff` after replacing the binary
to switch managers while keeping runners and managed services alive. Sending `SIGTERM` or `SIGINT`
to the foreground manager also performs a graceful stop.

The Linux full package enables `served@$USER.service`. The macOS full package installs a system
LaunchDaemon named `io.github.tunafish2k.served.<uid>`. Neither installer enables project services.

After the manager is running:

1. Enter the project directory.
2. Run `served edit` to create and edit `.served.json5`.
3. Run `served enable` to enable and start the project service.

To run a temporary service without project configuration, use `served run`:

```bash
served run -- python app.py
```

Check the service after installation:

```bash
served list
served attach <name>
```

Run `served restart` after you update the project. Use `served attach`, `served history`,
and persistent logs to investigate service failures. served does not upload or build the project.

The Linux full package contains `served`, `served@.service`, `install.sh`, `uninstall.sh`, the
README files, and the license. The macOS full package replaces the systemd template with
`served.plist`.

## Common Commands

Run `served edit` in a project directory:

```text
served                 Open the global service TUI
served daemon          Run the foreground manager with fixed HOME paths
served daemon --handoff
                       Replace the manager while keeping runners alive
served daemon --relinquish
                       Exit the manager while keeping runners alive for another supervisor
served shutdown        Stop the manager and all managed runners
served edit            Open .served.json5 in an external editor
served edit -e <cmd>   Use the specified editor command
served edit --path     Create a missing template and print its path
served enable          Enable and start the current service
served run [options] -- <program> [args...]
                       Create a temporary service without project configuration
served disable [name]  Disable the current or named service
served start [name]    Start a stopped managed service
served stop [name]     Stop a service and retain its registration and history
served restart [name]  Restart the current or named service
served attach [name]   Attach to the current or named service
served history [name]  Open latest.log in an editor
served history [name] --run <id>
                       Open a selected archived log
served history [name] -e <command>
                       Use the specified editor command
served history [name] --path
                       Print the selected persistent log path
served history [name] --stdout
                       Print sanitized persistent or in-memory history
served history [name] --json
                       Print sanitized history and metadata as JSON
served list            List services managed by the manager
```

Commands without a name match the current process working directory. If several services match,
served reports their names and requires an explicit name. Named commands work from any directory.

Use `served disable` when you no longer want to manage a project. Use `served restart`
after you change its configuration. There is no separate service-level `reload` command.

`served stop [name]` stops the process and cancels automatic restarts, including `restart=always`.
It keeps the registration, runner, and log history. `served start [name]` starts a stopped managed
service; it does not register an unknown service. Both commands are idempotent. Start leaves a
running, starting, or automatically restarting service unchanged, without reading edited configuration.
When stopped, an enabled service reloads and validates its registered configuration before starting;
a temporary service reuses its original command, options, and environment. Invalid configuration
leaves the service stopped. Restart also starts a stopped service.

Manual stop survives manager handoff, relinquish, and crash recovery while the runner is alive.
After normal shutdown and a fresh manager start, or a host reboot, enabled services start again;
temporary services are removed. A live manager preserves a known manual stop when replacing a failed
runner. There is no durable stop flag if both manager and runner are lost. Stop closes attach sessions;
history remains readable, and the next start creates a new run record.

Client and manager use protocol v9. Handoff can retain an older runner that does not support
start/stop. These commands report an error without changing it. To use them, disable the service,
then enable it again with its original configuration source and working-directory override, or run
the temporary service again with its original arguments and environment. This recreation loses
in-memory history; persistent logs remain.

### Temporary Services

`served run` creates a managed temporary service in the current directory, or in `--workdir DIR`.
The manager must already be running. The command does not read or create `.served.json5`, the deprecated `.served.json`, or
`.env.served`. It does not create an enabled registry link. After creation, the command prints the
service name and exits.

```bash
served run --name api --no-tty --restart on-failure \
  --env PORT=8080 -- python app.py --verbose
```

The service name defaults to a sanitized form of the selected working directory name. By default, served
allocates a TTY and syncs its size with an attach client. The default restart policy is `never`.
Logs remain in memory by default. `--restart` accepts `never`, `on-failure`, or `always`.

Use `--no-tty` or `--no-sync-rows-cols` to disable the TTY options. Use `--persist-logs` to keep raw
logs on disk. `--log-max-bytes` and `--log-max-files` use the same defaults as `.served.json5`.

Each `--env KEY=VALUE` option overrides the manager environment snapshot. If a key occurs more than
once, the last value applies.

Service names must be unique across all managed services. Multiple services can share a working
directory. `served run` rejects a name conflict without changing the existing service.

Arguments after `--` keep their exact boundaries. served does not interpret shell syntax in these
arguments. Use an explicit `sh -c` when a command requires pipes, redirects, or expansion.

The TUI and `served list` show temporary services. You can attach to these services, read their
history, restart them, or disable them. After the program exits, the service remains stopped. You
can still read its history or restart it. `served disable` removes the private runtime definition.
It keeps persistent logs.

A manager handoff, relinquish, or unexpected crash keeps a live temporary service available for
adoption. The manager validates its runner with a private runtime definition. Shutdown and a normal
manager stop remove this definition. After a host reboot, the manager does not restore the service.

## Service Configuration

Use `-f/--file PATH` with `edit` or `enable` to select a configuration with any filename. It is
always parsed as JSON5. Explicit selection bypasses default filename discovery and deprecation
warnings; a missing or invalid file is an error for `enable`. `edit -f` creates a missing template
and its parent directories, and preserves existing source text, including invalid source.

```bash
served edit -f /configs/api.json5
served enable -f /configs/api.json5 --workdir /srv/api
served run --name worker --workdir /srv/api -- ./worker
served restart api
```

The working directory is selected in this order: the saved `enable --workdir` override, the
configuration's optional `cwd`, then the configuration file's parent directory. CLI paths are
relative to the invocation directory; relative `cwd` values are relative to the configuration file.
With `enable --workdir DIR` and no `-f`, default configuration discovery uses `DIR`. Without either
option, discovery uses the invocation directory. The legacy `.env.served` remains beside the
configuration, even when the service runs elsewhere. `run` does not load it.

The working directory must exist. Restart reloads the original configuration source and validates
it before stopping the existing process. The CLI override survives restart and manager recovery;
disable and enable again to change or remove it. It never rewrites the configuration. Only `edit`
and `enable` accept `-f`; use names for subsequent management.

Ordinary directory enables retain the existing `~/.config/served/enabled/<name>` directory symlink.
Enables with `-f` or `--workdir` instead store a private, versioned JSON record at that path, containing
the source location and directory override. Existing links need no migration. Client and manager
must both support manager protocol v9; runner protocol v1 remains compatible with existing runners.


Run `served edit` in the service directory. If neither supported configuration file exists, served
creates a commented JSON5 template at `.served.json5` and opens it in your editor. served does not
rewrite or format an existing file.

The old `.served.json` filename remains supported and is parsed as JSON5. When it is the only
configuration, served uses it without renaming it and prints a deprecation warning. If both files
exist, `.served.json5` takes precedence and served warns that `.served.json` is ignored. An invalid
`.served.json5` is reported as an error and does not fall back to `.served.json`.

```json5
{
  name: "api",
  command: "python app.py",
  cwd: null, // Or an absolute path, or a path relative to this configuration.
  tty: true,
  syncRowsCols: true,
  restart: "never",
  persist_logs: false,
  log_max_bytes: 10485760,
  log_max_files: 3,
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
- `log_max_bytes` is optional and defaults to `10485760` bytes (`10 MiB`). When a persistent
  segment reaches this size, served archives it and continues with a new `latest.log`.
- `log_max_files` is optional and defaults to `3`. It is the number of archived persistent
  segments to keep. `latest.log` is kept in addition to these archives. Older or oversized
  archives are removed when the service starts or rotates its logs.
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

The borderless TUI shows service names and states, with the selected service's directory and type below.
Press Enter for actions or `?` for contextual help. Use arrows or `j/k` to move and Esc/q to go back.
The direct shortcuts remain: `a` attach, `s` start, `x` stop, `r` restart, `h` history, and `d` disable.
Disable requires confirmation and defaults to Cancel. Operations report progress; success messages clear
after three seconds, while errors remain readable until dismissed. During a manager disconnection,
the last list is marked stale and service actions are blocked until reconnection.

The minimum usable size is 40×10. Long names and paths are shortened in the list; the actions page’s `?` help
shows full details, scrollable with PgUp/PgDn. There are no random tips or decorative panels.
See [TUI design rules](docs/TUI-DESIGN.md) for the shared page templates and interaction contract.

TTY services provide a writable PTY attach. Pipe services provide a read-only attach. Pipe services
can have multiple read-only observers. Both modes use the terminal's alternate screen.

Run `served attach [name]` to attach without opening the service TUI. Without a name, served
uses the managed service for the current directory. With a name, served can attach from any directory.
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

The main TUI does not edit service configuration. Use `served edit` to open the selected
configuration file in an external editor. `-e/--editor COMMAND` takes priority over `$EDITOR`. If neither is set,
served searches `PATH` for `editor`, `sensible-editor`, `nvim`, `vim`, `vi`, `nano`, `micro`, then
`hx`. The editor command can contain arguments. served adds the configuration path as the last
argument. `--path` creates a missing template and prints its absolute path. `--path` conflicts with
`--editor`.

## Logs and Troubleshooting

A runner records non-zero exits and worker start or run errors in a rolling 60-second window. After
three failures, served reports a recent crash loop when attach finds that the service is not running.

The direct attach command asks `Open latest.log? [y/N]` only in an interactive terminal. The TUI
uses `y` or `Enter` to open the file. Use `n` or `Esc` to cancel. A log path
exists only when the current run has persistent logs. Otherwise, use the TUI history browser or
enable `persist_logs`.

served opens logs with `$EDITOR` or the editor fallback described above. After the editor exits,
attach returns the original service not running error. It does not retry attach. This warning
appears only when attach fails. It does not change the service list.

Press `h` in the TUI to select `latest` or a time archive. Press `Enter` to view
cleaned log content. The history page supports the arrow keys, `j/k`, `PgUp/PgDn`, and
`g/G`. It shows the current logical line position as `current/total`. Visual wrapping
does not change the total line count. History stays separate from attach. Attach does not replay old
PTY control state.

The command `served history` selects `latest` unless `--run <id>` is present. With no output mode,
it opens the selected persistent raw log; `-e/--editor COMMAND` takes priority over `$EDITOR` and
the editor fallback. `--path` prints only a persistent path. `--stdout` streams sanitized content,
and `--json` prints the same content with service, record, storage, byte, and line metadata. These
two output modes work for persistent and in-memory records and never create a temporary file.
`--path`, `--editor`, `--stdout`, and `--json` are mutually exclusive output modes.

Each process start creates a separate history record. This includes automatic and manual restarts.
The runner owns the history, so a manager restart does not remove it. Persistent logs use:

```text
$HOME/.local/state/served/logs/<name>/
```

The current run writes to `latest.log`. When it reaches `log_max_bytes`, served archives the
current segment by its run start time as `YYYYMMDD-HHMMSS.log` and continues with a new
`latest.log`. If names conflict, served adds `-1`, `-2`, and so on. `.latest.started` stores the
run start time. Each service keeps `log_max_files` archives and one latest file. The default is
`10 MiB` per segment and `3` archives. The log directory uses mode `0700`. Log files use mode
`0600`.

With `persist_logs: false`, served does not add disk logs. The runner keeps the current record
and the latest 100 memory archives during its lifetime. A manager restart keeps these records. A
service restart starts a new current record; terminating the runner clears them. Use
`served history --stdout` or `--json` to read them from scripts and AI tools.

TTY history stores raw PTY bytes. Pipe history merges stdout and stderr in runner event order. The
history view removes ANSI and invisible control sequences. If persistent storage fails, the service
continues with memory history and the manager records a warning.

## Optional systemd Installation

The systemd integration is only for Linux. The repository stores its installer in `scripts/` and
the unit template in `systemd/`. Run the installer as the normal user that will own the manager. It
uses `sudo` to install the shared `/usr/local/bin/served` binary and
`/etc/systemd/system/served@.service`, then enables and starts `served@$USER.service`. The Rust
program does not call `systemctl` or D-Bus.

The template uses `User=%i`; each instance gets that account's login environment, home directory,
socket, registry, runners, and managed services. It refuses the `root` instance. It does not set
`Group=`, so systemd uses the account's primary group. To add another account after installing the
shared files, enable its instance explicitly:

```bash
sudo systemctl enable --now served@alice.service
```

The first installation on a host enables and starts the invoking account's instance at
`multi-user.target`.
Upgrades preserve every instance's enabled and active state. When the shared binary changes, the
installer reloads every active `served@*.service`; the new client tells each manager which executable
to run, so a replaced path also works. If handoff fails, that instance receives a controlled restart.
Stopped instances remain stopped. File or service failures restore the previous shared files and
attempt to restore the recorded instance states.

The installer automatically detects the old fixed `/etc/systemd/system/served.service`, the old
`~/.config/systemd/user/served.service`, and `~/.local/bin/served`. It verifies that a fixed unit
belongs to the invoking account. For an active fixed service, it first upgrades the manager, asks it
to release its socket without stopping runners, and starts the new template instance to adopt them.
If that transfer is unavailable, migration uses a controlled stop. Old files are deleted only after
the new instance reaches its requested state. Custom XDG directories are reported but not moved.

Run `./uninstall.sh` as the account whose integration you want to remove. It disables and stops only
that account's instance and keeps configuration and state. If any other enabled or active instance
exists, it keeps the shared binary and template. Otherwise, a separate `y/N` prompt controls shared
file removal. Non-interactive operations that require confirmation stop without changing state.

Use `systemctl reload "served@$USER.service"` for manager handoff. `systemctl restart` and
`systemctl stop` are explicit lifecycle actions for that account and stop its runners. If a manager
exits unexpectedly, systemd starts it again and the surviving runners are adopted.

## Optional launchd Installation

The macOS full package installs `/usr/local/bin/served` and a per-user system LaunchDaemon at
`/Library/LaunchDaemons/io.github.tunafish2k.served.<uid>.plist`. The plist is owned by root, but
`UserName` runs the manager and all services as the installation user. It uses that account's home,
working directory, login shell, primary group, socket, registry, runners, and logs. It does not
depend on a graphical login session.

The first installation bootstraps and starts the invoking user's daemon. An upgrade hands off every
active served LaunchDaemon on the host after replacing the shared binary. If the current user's
plist changes, the installer relinquishes the manager before bootstrapping the new plist so the new
manager can adopt existing runners. A failure restores the old binary, plist, and active managers.
An already unloaded instance remains unloaded.

Run the packaged `./uninstall.sh` as the account whose integration you want to remove. It stops and
removes only that user's LaunchDaemon and keeps configuration and state. The shared binary remains
while another served LaunchDaemon exists; otherwise a separate `y/N` prompt controls its removal.

macOS privacy controls can prevent a LaunchDaemon from accessing protected Desktop, Documents, or
Downloads locations. Grant Full Disk Access to `/usr/local/bin/served`, or keep managed projects in
an unprotected directory. Release binaries use ad-hoc signatures and are not notarized.

## Release Downloads

Push a `v<semver>` tag that matches the version in `Cargo.toml` to create a GitHub Release. The
workflow builds and tests macOS and Linux binaries for amd64 and arm64. Linux release binaries
require glibc 2.17 or later. macOS amd64 targets 10.12 or later; arm64 targets 11.0 or later.

For a release tag `vX.Y.Z`, the assets follow this naming scheme:

```text
served-linux-amd64-vX.Y.Z-binary
served-linux-amd64-vX.Y.Z-binary.sha256
served-linux-amd64-vX.Y.Z-full.tar.gz
served-linux-amd64-vX.Y.Z-full.tar.gz.sha256
served-linux-arm64-vX.Y.Z-binary
served-linux-arm64-vX.Y.Z-binary.sha256
served-linux-arm64-vX.Y.Z-full.tar.gz
served-linux-arm64-vX.Y.Z-full.tar.gz.sha256
served-macos-amd64-vX.Y.Z-binary
served-macos-amd64-vX.Y.Z-binary.sha256
served-macos-amd64-vX.Y.Z-full.tar.gz
served-macos-amd64-vX.Y.Z-full.tar.gz.sha256
served-macos-arm64-vX.Y.Z-binary
served-macos-arm64-vX.Y.Z-binary.sha256
served-macos-arm64-vX.Y.Z-full.tar.gz
served-macos-arm64-vX.Y.Z-full.tar.gz.sha256
served-vX.Y.Z-source.tar.gz
served-vX.Y.Z-source.tar.gz.sha256
```

Each `binary` asset contains only the executable. A Linux full package adds systemd integration; a
macOS full package adds LaunchDaemon integration. The deterministic source archive contains the
buildable project source. Each asset has its own SHA-256 sidecar file. macOS binaries use ad-hoc
signatures and are not notarized. The workflow does not build musl or Windows targets.

## Security and Limits

- The manager runs as a normal user. The manager socket is readable and writable by that user.
- A process supervisor starts the foreground manager as the installation user. The systemd unit and
  macOS LaunchDaemon are supported platform integrations.
- Each managed service has an independent runner. The manager adopts it through a private runner
  socket.
- After an unexpected manager restart, the manager scans the enabled registry and temporary runtime
  definitions for live runners. It adopts matching runners without restarting their service
  processes.
- A runner at `$HOME/.local/state/served/runtime/runners/<name>/` owns the service process,
  PTY, log cache, restart state, and crash-loop window. A manager crash does not stop these items.
- `served shutdown` performs a graceful shutdown for all runners. `served disable` stops the
  matching runner. `served stop` and `served restart` retain it and its history. A manager reload uses
  handoff and keeps the service PID. A first upgrade from the old worker architecture may need one
  controlled restart.
- The system service sets `HOME` from the installation user's login environment. It starts
  the manager with a login shell, so files such as `/etc/profile` load when the manager starts.
  The manager keeps that environment snapshot until it restarts.
- The system service uses the installation user's home as its working directory. It does not use the
  system manager's `%h` expansion.
- The runner creates one process group for each pipe or PTY service. It sends `SIGTERM` to the
  group first, then `SIGKILL` after the timeout, and confirms that the service leader was reaped.
  A failed stop or restart is returned as an error. A manager crash does not run this cleanup path.
- Detached child processes created with `nohup`, background commands, or daemonization are
  outside the cleanup guarantee.
- V1 does not provide root mode, container isolation, namespaces, resource limits, dependency graphs,
  or health checks.

## Maintainer Build

These commands build and check served itself. You do not need a Rust toolchain to deploy a personal
project. Use a full release package for personal deployment.

```bash
make bootstrap       # Install same-OS amd64 and arm64 targets
make check           # Format, clippy, and native tests
make msrv-check      # Compile every target with Rust 1.85
make build-cross     # Build the other host architecture
make build-all       # Build both host architectures
make dist            # Package both host architectures
make source-dist     # Create the deterministic source archive
make shellcheck      # Check all repository shell scripts
make installer-check # Test the online installer with mocks
make systemd-check   # Validate the systemd template
make launchd-check   # Validate the launchd template
make linux-check     # Run the Linux checks in Docker
```

`make run` starts an isolated manager with `HOME` under `.dev/`. In another terminal, use
`make cli ARGS="list"` or another served command against that manager. Linux cross releases use
Zig 0.16.0 and cargo-zigbuild 0.23.0. Builds do not cross operating systems: macOS builds both macOS
architectures and Linux builds both Linux architectures. The Docker check runs on Rust 1.85; local
builds and CI use stable unless `RUST_TOOLCHAIN` selects another installed rustup toolchain.

Core requirements are in [REQUIREMENTS.md](REQUIREMENTS.md). Technical decisions are in
[TECH-STACK.md](TECH-STACK.md).

## License

served is released under the [Unlicense](LICENSE). You can use, copy, modify, publish, and
distribute it without licensing restrictions. The software is provided without warranty.
