#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
template="$project_dir/launchd/served.plist"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

[[ -f "$template" ]] || fail "launchd template is missing"
command -v python3 >/dev/null 2>&1 || fail "python3 is required to validate the launchd template"
python3 - "$template" <<'PY'
import plistlib
import sys

with open(sys.argv[1], "rb") as source:
    plist = plistlib.load(source)

expected = {
    "Label": "@SERVED_LABEL@",
    "UserName": "@SERVED_USER@",
    "ProgramArguments": ["@SERVED_SHELL@", "-lc", "exec @SERVED_BIN@ daemon"],
    "WorkingDirectory": "@SERVED_HOME@",
    "EnvironmentVariables": {
        "HOME": "@SERVED_HOME@",
        "USER": "@SERVED_USER@",
        "LOGNAME": "@SERVED_USER@",
        "SHELL": "@SERVED_SHELL@",
    },
    "KeepAlive": True,
    "ThrottleInterval": 1,
    "ExitTimeOut": 30,
    "Umask": 0o77,
    "AbandonProcessGroup": True,
    "StandardOutPath": "@SERVED_STDOUT@",
    "StandardErrorPath": "@SERVED_STDERR@",
}
if plist != expected:
    raise SystemExit("launchd template does not match the required lifecycle contract")
PY

if [[ "$(uname -s)" == Darwin ]]; then
    plutil -lint "$template" >/dev/null
fi

printf 'launchd template checks passed\n'
