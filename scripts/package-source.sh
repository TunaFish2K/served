#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
output="${1:-}"
version="${2:-}"

[[ -n "$output" && -n "$version" ]] || {
    printf 'usage: scripts/package-source.sh OUTPUT VERSION\n' >&2
    exit 1
}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
    printf 'error: VERSION must be semantic\n' >&2
    exit 1
}

manifest_version="$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' "$project_dir/Cargo.toml")"
[[ "$manifest_version" = "$version" ]] || {
    printf 'error: version %s does not match Cargo.toml version %s\n' "$version" "$manifest_version" >&2
    exit 1
}

inputs=(
    Cargo.toml
    Cargo.lock
    LICENSE
    Makefile
    README.md
    README.zh-CN.md
    REQUIREMENTS.md
    TECH-STACK.md
    CONTEXT.md
    rust-toolchain.toml
    docs
    launchd
    scripts
    src
    systemd
    tests
)

command -v python3 >/dev/null 2>&1 || {
    printf 'error: python3 is required\n' >&2
    exit 1
}
mkdir -p "$(dirname -- "$output")"
python3 "$project_dir/scripts/package-source.py" \
    "$project_dir" "$output" "$version" "${inputs[@]}"
