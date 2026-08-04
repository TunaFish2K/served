# served

[简体中文](README.zh-CN.md)

`served` runs an existing project directory as a long-running service for personal, non-critical
use. It manages host processes directly and does not run containers. The foreground manager can
run under any process supervisor; the included systemd unit is an optional Linux integration.

Release binaries support macOS and Linux with glibc on amd64/x64 and arm64.

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

Download the release package for the host operating system and architecture, then put `served` in a
directory on `PATH`. Configure your process supervisor to run this foreground command as the target
user with that user's normal `HOME`:

```bash
served daemon
```

Use `served shutdown` for a graceful stop. Use `served daemon --handoff` after replacing the binary
to switch managers while keeping runners and managed services alive. Sending `SIGTERM` or `SIGINT`
to the foreground manager also performs a graceful stop.

Linux users who want systemd can download the matching `full.tar.gz` package and run
`./install.sh`; this requires working `sudo` access. The installer enables
`served@$USER.service` for the invoking account but does not enable project services.

After the manager is running:

1. Enter the project directory.
2. Run `served edit` to create and edit `.served.json`.
3. Run `served enable` to enable and start the project service.

Check the service after installation:

```bash
served list
served attach <name>
```

Run `served restart` after you update the project. Use `served attach`, `served history`,
and persistent logs to investigate service failures. served does not upload or build the project.

The Linux full package contains these files:

```text
served
served@.service
install.sh
uninstall.sh
README.md
README.zh-CN.md
```

## Nix and AUR

Install only the binary from this flake:

```bash
nix profile install github:TunaFish2K/served
```

For NixOS, import `inputs.served.nixosModules.default` and declare every account that needs an
independent manager:

```nix
services.served = {
  enable = true;
  users = [ "alice" "bob" ];
};
```

The module adds the binary to `environment.systemPackages` and creates one system service per user.
It does not accept `root`, an empty user list, or duplicate users.

The AUR metadata under `packaging/aur/` produces two packages. `served` contains only the binary.
`served-systemd` depends on that exact binary package and installs the optional systemd template.
After both packages are installed, enable the accounts you need:

```bash
sudo systemctl enable --now "served@$USER.service"
sudo systemctl enable --now served@alice.service
```

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

The current run writes to `latest.log`. When it reaches `log_max_bytes`, served archives the
current segment by its run start time as `YYYYMMDD-HHMMSS.log` and continues with a new
`latest.log`. If names conflict, served adds `-1`, `-2`, and so on. `.latest.started` stores the
run start time. Each service keeps `log_max_files` archives and one latest file. The default is
`10 MiB` per segment and `3` archives. The log directory uses mode `0700`. Log files use mode
`0600`.

With `persist_logs: false`, served does not add disk logs. The runner keeps the current record
and the latest 100 memory archives during its lifetime. A manager restart keeps these records. A
runner or service restart starts a new current record.

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

## Release Downloads

Push a `v<semver>` tag that matches the version in `Cargo.toml` to create a GitHub
Release. The workflow builds and tests native macOS and Linux binaries for amd64 and arm64. Linux
release binaries require glibc 2.17 or later. macOS requires 10.12 or later on amd64 and 11.0 or
later on arm64.

For version `0.4.0`, the release contains:

```text
served-linux-amd64-v0.4.0-binary
served-linux-amd64-v0.4.0-binary.sha256
served-linux-amd64-v0.4.0-full.tar.gz
served-linux-amd64-v0.4.0-full.tar.gz.sha256
served-linux-arm64-v0.4.0-binary
served-linux-arm64-v0.4.0-binary.sha256
served-linux-arm64-v0.4.0-full.tar.gz
served-linux-arm64-v0.4.0-full.tar.gz.sha256
served-macos-amd64-v0.4.0.tar.gz
served-macos-amd64-v0.4.0.tar.gz.sha256
served-macos-arm64-v0.4.0.tar.gz
served-macos-arm64-v0.4.0.tar.gz.sha256
served-v0.4.0-source.tar.gz
served-v0.4.0-source.tar.gz.sha256
```

The Linux `binary` asset contains only the executable. The Linux `full.tar.gz` asset contains the
executable, `served@.service`, installer, uninstaller, and both README files. The deterministic
source archive is the input used by the AUR package. Each asset has its own SHA-256 sidecar file.

The macOS archive contains the executable, both README files, and the license. macOS binaries use
ad-hoc code signatures and are not notarized. The workflow does not build musl or Windows targets.

## Security and Limits

- The manager runs as a normal user. The manager socket is readable and writable by that user.
- A process supervisor starts the foreground manager as the installation user. The systemd unit is
  one supported Linux configuration.
- Each enabled service has an independent runner. The manager adopts it through a private runner
  socket.
- After a manager restart, the manager scans the enabled registry and adopts existing
  runners first.
- A runner at `$HOME/.local/state/served/runtime/runners/<name>/` owns the service process,
  PTY, log cache, restart state, and crash-loop window. A manager crash does not stop these items.
- `served shutdown` performs a graceful shutdown for all runners. `served disable` and
  `served restart` stop or replace the matching runner. A manager reload uses
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
make systemd-check   # Validate the systemd template
make aur-check       # Build and inspect both AUR packages (Arch Linux)
make linux-check     # Run the Linux checks in Docker
```

`make run` starts an isolated manager with `HOME` under `.dev/`. In another terminal, use
`make cli ARGS="list"` or another served command against that manager. Linux cross releases use
Zig 0.14.1 and cargo-zigbuild 0.21.8. Cross-operating-system builds are not supported: macOS builds
the two macOS targets, and Linux builds the two Linux targets. The Docker check runs on Rust 1.85;
local builds and CI use stable unless `RUST_TOOLCHAIN` selects another installed rustup toolchain.

Core requirements are in [REQUIREMENTS.md](REQUIREMENTS.md). Technical decisions are in
[TECH-STACK.md](TECH-STACK.md).

## License

served is released under the [Unlicense](LICENSE). You can use, copy, modify, publish, and
distribute it without licensing restrictions. The software is provided without warranty.
