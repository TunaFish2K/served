#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-}"
rust_toolchain="${RUST_TOOLCHAIN:-stable}"
cargo_for_target=("$project_dir/scripts/cargo-toolchain.sh" "$rust_toolchain")
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
cargo_zigbuild="$cargo_home/bin/cargo-zigbuild"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

require_linux_host() {
    [[ "$(uname -s)" == "Linux" ]] || fail "release builds require Linux"
}

host_arch() {
    case "$(uname -m)" in
        x86_64|amd64) printf 'amd64\n' ;;
        arm64|aarch64) printf 'arm64\n' ;;
        *) fail "supported build architectures are x64 and arm64" ;;
    esac
}

rust_target() {
    case "$1" in
        amd64) printf 'x86_64-unknown-linux-gnu\n' ;;
        arm64) printf 'aarch64-unknown-linux-gnu\n' ;;
        *) fail "unsupported target architecture $1" ;;
    esac
}

build_target() {
    local arch="$1"
    local target

    target="$(rust_target "$arch")"
    rustup target add --toolchain "$rust_toolchain" "$target"

    if command -v zig >/dev/null 2>&1; then
        zig_version="$(zig version)"
    elif python3 -m ziglang version >/dev/null 2>&1; then
        zig_version="$(python3 -m ziglang version)"
        CARGO_ZIGBUILD_PYTHON_PATH="$(command -v python3)"
        export CARGO_ZIGBUILD_PYTHON_PATH
    else
        fail "zig 0.14.1 is required for Linux release builds; run make bootstrap"
    fi
    [[ "$zig_version" == "0.14.1" ]] ||
        fail "Zig 0.14.1 is required, found $zig_version"
    if [[ ! -x "$cargo_zigbuild" ]] ||
        [[ "$("$cargo_zigbuild" --version 2>/dev/null)" != "cargo-zigbuild 0.21.8" ]]; then
        fail "cargo-zigbuild 0.21.8 is required; run make bootstrap"
    fi
    "${cargo_for_target[@]}" zigbuild --release --locked --target "${target}.2.17"
}

case "$mode" in
    cross|all|amd64|arm64) ;;
    *) fail "usage: scripts/build-targets.sh cross|all|amd64|arm64" ;;
esac

cd "$project_dir"
require_linux_host
native_arch="$(host_arch)"
if [[ "$native_arch" == "amd64" ]]; then
    other_arch="arm64"
else
    other_arch="amd64"
fi

case "$mode" in
    all)
        build_target "$native_arch"
        build_target "$other_arch"
        ;;
    cross)
        build_target "$other_arch"
        ;;
    amd64|arm64)
        build_target "$mode"
        ;;
esac
