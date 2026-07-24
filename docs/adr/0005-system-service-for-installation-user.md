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
- Set `HOME` in the unit and start `/usr/local/bin/served daemon` through
  `/bin/sh -lc`, so the installation user's profile environment is captured at
  manager startup.
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

## Consequences

The manager survives logout and uses one stable socket path in both manual and
systemd launches. System installation needs `sudo`, and only one installation user
is supported per host. Existing custom XDG data is not moved automatically. A
legacy user service needs a reachable user manager during migration so the script
can stop and disable it safely.
