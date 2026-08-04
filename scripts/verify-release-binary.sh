#!/usr/bin/env bash
set -euo pipefail

os="${1:-}"
arch="${2:-}"
binary="${3:-}"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

[[ -f "$binary" ]] || fail "release binary does not exist: $binary"

case "$os/$arch" in
    linux/amd64) file "$binary" | grep -Eq 'ELF 64-bit.*(x86-64|x86_64)' ;;
    linux/arm64) file "$binary" | grep -Eq 'ELF 64-bit.*(aarch64|ARM aarch64)' ;;
    macos/amd64) file "$binary" | grep -Eq 'Mach-O 64-bit.*x86_64' ;;
    macos/arm64) file "$binary" | grep -Eq 'Mach-O 64-bit.*arm64' ;;
    *) fail "usage: scripts/verify-release-binary.sh macos|linux amd64|arm64 PATH" ;;
esac

if [[ "$os" == "linux" ]]; then
    required="$(
        readelf -W --version-info --dyn-syms "$binary" \
            | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' \
            | sort -Vu \
            | tail -n 1
    )"
    if [[ -n "$required" && "$(printf '%s\n' "$required" 2.17 | sort -V | tail -n 1)" != "2.17" ]]; then
        fail "$binary requires glibc $required, expected at most 2.17"
    fi
    exit 0
fi

codesign --verify --strict "$binary"
command -v vtool >/dev/null 2>&1 || fail "vtool is required to verify macOS deployment targets"
minimum="$(
    vtool -show-build "$binary" \
        | awk '$1 == "minos" || $1 == "version" { print $2; exit }'
)"
if [[ "$arch" == "amd64" ]]; then
    expected="10.12"
else
    expected="11.0"
fi
[[ "$minimum" == "$expected" ]] ||
    fail "$binary has macOS deployment target ${minimum:-unknown}, expected $expected"
