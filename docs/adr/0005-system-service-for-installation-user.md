# ADR 0005: System Service for the Installation User

- Status: Accepted
- Date: 2026-07-25

## Context

The manager must continue running after an SSH session ends, while its managed
processes should remain unprivileged and owned by one ordinary installation user.
The previous user-service model required lingering and depended on session-scoped
runtime paths. It also made a direct `served daemon` invocation easy to configure
differently from the installed manager.

## Decision

- Install one fixed `/etc/systemd/system/served.service` unit for the host.
- Run the unit with `User=` and `Group=` set to the installation user's identity;
  the manager is never a root daemon.
- Enable the unit for `multi-user.target`, with `Restart=always`, `RestartSec=1s`,
  and `NoNewPrivileges=yes`.
- Use the installation user's login environment and home directory as the unit's
  environment and working directory, then start `/usr/local/bin/served daemon`
  through `/bin/sh -lc` so the installation user's profile is captured at manager
  startup. The unit must not use the system manager's `%h` specifier for these
  paths; in a system instance it can resolve to the manager's home rather than
  the `User=` home and fail at `CHDIR` before the manager starts.
- Derive configuration, state, and socket paths from `HOME` only:
  `~/.config`, `~/.local/state`, and
  `~/.local/state/served/runtime/served.sock`. `XDG_*` variables do not select
  served paths.
- The installer is run by the target user and uses internal `sudo` calls. It
  refuses to overwrite a system unit owned by another user.
- A legacy `systemd --user` installation is migrated only after confirmation. The
  old user manager must be reachable; otherwise migration aborts without deleting
  old files. Legacy files are removed only after the new system service is active.

## Alternatives Considered

- Keep `systemd --user` and enable lingering: rejected because the service lifetime
  and runtime socket depend on user-manager/session behavior.
- Run the system unit as root: rejected because it expands the privilege boundary
  without being needed for served's host-user process model.
- Use a systemd template unit for multiple users: rejected for V1; one host has one
  installation user and one fixed service name.
- Keep honoring XDG runtime/config/state variables: rejected because direct daemon
  and installed service could resolve different locations.
- Use the system manager's `%h` for `HOME` and `WorkingDirectory`: rejected because
  it is not a reliable reference to the `User=` account in a system unit.
- Render an absolute home path into the unit at install time: rejected because it
  duplicates the home source and requires reinstalling if the account home moves.

## Consequences

The manager survives logout and uses one stable socket path in both manual and
systemd launches. System installation needs `sudo`, and only one installation user
is supported per host. Existing custom XDG data is not moved automatically. A
legacy user service needs a reachable user manager during migration so the script
can stop and disable it safely. An upgrade preserves an inactive or failed service
as stopped and reports the command needed to start it; it does not silently change
the service's enabled state.
