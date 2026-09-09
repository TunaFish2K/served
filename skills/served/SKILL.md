---
name: served
description: Configure, run, inspect, restart, and troubleshoot personal services with the served CLI on Linux or macOS. Use when the user wants to manage a service with served, including external configuration files, shared working directories, temporary runs, and service logs.
---

# Manage services with served

Use this skill to operate an existing served installation or prepare service configuration. Check `served --version` and `served list` to establish the installed capabilities and current state. Independent file/workdir options require 0.9.0 or later. If the manager is unavailable or its protocol differs, determine whether the user's task includes installing or upgrading it; use the documented installation flow for that task.

Read [the command reference](references/cli.md) for options, lifecycle behavior, history modes, and editor selection. Read [the configuration reference](references/config.md) when preparing configuration, resolving paths/environment, or finding an existing service's registered source. Both references are bundled for offline use.

## Choose the service definition

- For a service that should start when the manager starts, prepare a JSON5 configuration and use `served enable`. Its `command` is a shell string executed with `/bin/sh -c`.
- For a temporary managed process, use `served run [options] -- program args...`. It ignores project configuration and dotenv files, preserves argument boundaries, and does not survive normal shutdown or a host reboot. Use explicit `sh -c` only when shell syntax is intended.
- Choose a unique name from the current service list. Multiple services can share a workdir; use names for subsequent operations rather than relying on the current directory.

## Configure and apply

1. Establish the intended command, configuration path, working directory, environment, and restart policy from the user's task and project. Keep existing source comments and unrelated settings.
2. For new configuration, `served edit -f /configs/api.json5 --path` creates the missing template and prints its path. **Even `--path` can create files.** Read an existing source directly when only inspection is needed.
3. Resolve paths deliberately: CLI paths are relative to the invocation directory, configuration `cwd` is relative to the configuration file, and the default cwd is that file's parent. `enable --workdir` takes priority and is saved for later restarts. With no `-f`, it also chooses the directory searched for default configuration.
4. Enable a new persistent service, for example `served enable -f /configs/api.json5 --workdir /srv/api`. For an existing service, locate its source via the enabled registry described in the configuration reference, edit that source, then `served restart NAME`. The saved workdir override requires disable and enable again to change or clear it; editing `cwd` alone cannot override it.
5. Verify `served list` and `served history NAME --json` or `--stdout`. Successful enable/restart means the request succeeded; inspect whether the process stayed running and produced the expected output.

Configuration environment values override `.env.served` beside the configuration, which overrides the manager's startup environment. Project `.env` is not read. A CLI invocation's environment is not a replacement for the manager snapshot.

## Diagnose and control

Prefer `history NAME --json` or `--stdout` for noninteractive inspection. Editor/path modes require persisted logs; memory history has no raw file path. Use `--run ID` for a known archived run.

An invalid configuration or cwd rejects restart while leaving the old process running. Correct the reported source or directory, then apply the intended change. Do not treat a failed restart as proof that the service stopped.

Use attach when an interactive session is wanted. PTY attach has one writer; pipe attach is read-only. Ctrl-C detaches rather than sending an interrupt to the process. History is output, not terminal-state replay.

Use `disable NAME` to stop and unregister one service. `shutdown` affects the manager and all its services. Match lifecycle actions to the user's requested scope and existing authorization. A manager handoff preserves runners but disconnects existing attach streams; use the documented supervisor upgrade procedure when upgrading an installation.
