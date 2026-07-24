#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
template_path="$project_dir/systemd/served.service"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-system-service.XXXXXX")"
rendered_path="$temp_dir/served.service"
trap 'rm -rf -- "$temp_dir"' EXIT

command -v systemd-analyze >/dev/null 2>&1 || {
    printf 'error: systemd-analyze is required for this check\n' >&2
    exit 1
}

user_name="$(id -un)"
group_name="$(id -gn)"
sed \
    -e "s|@SERVED_USER@|$user_name|g" \
    -e "s|@SERVED_GROUP@|$group_name|g" \
    "$template_path" > "$rendered_path"

grep -Fq "User=$user_name" "$rendered_path"
grep -Fq "Group=$group_name" "$rendered_path"
grep -Fq 'SetLoginEnvironment=yes' "$rendered_path"
grep -Fq 'WorkingDirectory=~' "$rendered_path"
if grep -Fq '%h' "$rendered_path"; then
    printf 'error: system service template must not use %%h for home paths\n' >&2
    exit 1
fi
if grep -Eq '^Environment=HOME=' "$rendered_path"; then
    printf 'error: system service template must not override HOME directly\n' >&2
    exit 1
fi
if grep -Fq '@SERVED_' "$rendered_path"; then
    printf 'error: rendered system service template contains unresolved placeholders\n' >&2
    exit 1
fi

systemd-analyze verify "$rendered_path"
printf 'system service template is valid\n'
