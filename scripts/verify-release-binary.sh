#!/usr/bin/env bash
set -euo pipefail

arch="${1:-}"
binary="${2:-}"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

[[ -f "$binary" ]] || fail "release binary does not exist: $binary"

case "$arch" in
    amd64) file "$binary" | grep -Eq 'ELF 64-bit.*(x86-64|x86_64)' ;;
    arm64) file "$binary" | grep -Eq 'ELF 64-bit.*(aarch64|ARM aarch64)' ;;
    *) fail "usage: scripts/verify-release-binary.sh amd64|arm64 PATH" ;;
esac

required="$(
    readelf -W --version-info --dyn-syms "$binary" \
        | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' \
        | sort -Vu \
        | tail -n 1
)"
if [[ -n "$required" && "$(printf '%s\n' "$required" 2.17 | sort -V | tail -n 1)" != "2.17" ]]; then
    fail "$binary requires glibc $required, expected at most 2.17"
fi
