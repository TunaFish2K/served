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
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
export PATH="$toolchain_bin:$cargo_home/bin:$PATH"
export RUSTUP_TOOLCHAIN="$toolchain"
exec "$cargo_path" "$@"
