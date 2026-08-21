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

host_os() {
    case "$(uname -s)" in
        Darwin) printf 'macos\n' ;;
        Linux) printf 'linux\n' ;;
        *) fail "supported build hosts are macOS and Linux" ;;
    esac
}

host_arch() {
    case "$(uname -m)" in
        x86_64|amd64) printf 'amd64\n' ;;
        arm64|aarch64) printf 'arm64\n' ;;
        *) fail "supported build architectures are x64 and arm64" ;;
    esac
}

rust_target() {
    case "$1/$2" in
        macos/amd64) printf 'x86_64-apple-darwin\n' ;;
        macos/arm64) printf 'aarch64-apple-darwin\n' ;;
        linux/amd64) printf 'x86_64-unknown-linux-gnu\n' ;;
        linux/arm64) printf 'aarch64-unknown-linux-gnu\n' ;;
        *) fail "unsupported target $1/$2" ;;
    esac
}

build_target() {
    local os="$1"
    local arch="$2"
    local target

    target="$(rust_target "$os" "$arch")"
    rustup target add --toolchain "$rust_toolchain" "$target"

    if [[ "$os" == "macos" ]]; then
        if [[ "$arch" == "amd64" ]]; then
            MACOSX_DEPLOYMENT_TARGET=10.12 \
                "${cargo_for_target[@]}" build --release --locked --target "$target"
        else
            MACOSX_DEPLOYMENT_TARGET=11.0 \
                "${cargo_for_target[@]}" build --release --locked --target "$target"
        fi
        return
    fi

    if [[ "$(python3 -m ziglang version 2>/dev/null || true)" == "0.14.1" ]]; then
        zig_version="$(python3 -m ziglang version)"
        CARGO_ZIGBUILD_PYTHON_PATH="$(command -v python3)"
        export CARGO_ZIGBUILD_PYTHON_PATH
    elif command -v zig >/dev/null 2>&1; then
        zig_version="$(zig version)"
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
os="$(host_os)"
native_arch="$(host_arch)"
if [[ "$native_arch" == "amd64" ]]; then
    other_arch="arm64"
else
    other_arch="amd64"
fi

case "$mode" in
    all)
        build_target "$os" "$native_arch"
        build_target "$os" "$other_arch"
        ;;
    cross)
        build_target "$os" "$other_arch"
        ;;
    amd64|arm64)
        build_target "$os" "$mode"
        ;;
esac
