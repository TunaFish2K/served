#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
template_path="$project_dir/systemd/served@.service"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-system-service.XXXXXX")"
user_name="$(id -un)"
rendered_path="$temp_dir/served@${user_name}.service"
verify_dir="$temp_dir/verify"
verify_path="$verify_dir/served@${user_name}.service"
trap 'rm -rf -- "$temp_dir"' EXIT

command -v systemd-analyze >/dev/null 2>&1 || {
    printf 'error: systemd-analyze is required for this check\n' >&2
    exit 1
}

sed \
    -e 's|@SERVED_BIN@|/usr/local/bin/served|g' \
    "$template_path" > "$rendered_path"

grep -Fq 'User=%i' "$rendered_path"
if grep -Eq '^Group=' "$rendered_path"; then
    printf 'error: system service template must use the account primary group\n' >&2
    exit 1
fi
grep -Fq 'ExecCondition=/bin/sh -c '\''test "%i" != root'\''' "$rendered_path"
grep -Fq 'SetLoginEnvironment=yes' "$rendered_path"
grep -Fq 'WorkingDirectory=~' "$rendered_path"
grep -Fq 'ExecStop=/usr/local/bin/served shutdown' "$rendered_path"
grep -Fq 'ExecReload=/usr/local/bin/served daemon --handoff' "$rendered_path"
grep -Fq 'RestartPreventExitStatus=75' "$rendered_path"
grep -Fq 'SuccessExitStatus=75' "$rendered_path"
grep -Fq 'KillMode=process' "$rendered_path"
if grep -Fq '%h' "$rendered_path"; then
    printf 'error: system service template must not use %%h for home paths\n' >&2
    exit 1
fi
if grep -Eq '^Environment=HOME=' "$rendered_path"; then
    printf 'error: system service template must not override HOME directly\n' >&2
    exit 1
fi
if grep -Fq '@SERVED_BIN@' "$rendered_path"; then
    printf 'error: rendered system service template contains unresolved placeholders\n' >&2
    exit 1
fi

# Verify the unit syntax without requiring the installed served binary to exist
# on the machine running this check.
mkdir -p "$verify_dir"
sed 's|/usr/local/bin/served|/bin/true|g' "$rendered_path" > "$verify_path"
systemd-analyze verify "$verify_path"
printf 'system service template is valid\n'
