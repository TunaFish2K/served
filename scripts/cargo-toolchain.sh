#!/usr/bin/env bash
set -euo pipefail

toolchain="${1:-}"
[[ -n "$toolchain" ]] || {
    printf 'error: usage: scripts/cargo-toolchain.sh TOOLCHAIN CARGO_ARGS...\n' >&2
    exit 1
}
shift

cargo_path="$(rustup which --toolchain "$toolchain" cargo)"
toolchain_bin="$(dirname "$cargo_path")"
export PATH="$toolchain_bin:$PATH"
export RUSTUP_TOOLCHAIN="$toolchain"
exec "$cargo_path" "$@"
