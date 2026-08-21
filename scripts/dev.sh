#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
dev_home="$project_dir/.dev/home"
command_name="${1:-}"
rust_toolchain="${RUST_TOOLCHAIN:-stable}"
cargo_for_dev=("$project_dir/scripts/cargo-toolchain.sh" "$rust_toolchain")
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
cargo_zigbuild="$cargo_home/bin/cargo-zigbuild"
shift || true

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

host_targets() {
    case "$(uname -s)" in
        Darwin) printf '%s\n' x86_64-apple-darwin aarch64-apple-darwin ;;
        Linux) printf '%s\n' x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu ;;
        *) fail "supported development hosts are macOS and Linux" ;;
    esac
}

bootstrap() {
    command -v rustup >/dev/null 2>&1 || fail "rustup is required"
    while IFS= read -r target; do
        rustup target add --toolchain "$rust_toolchain" "$target"
    done < <(host_targets)
    rustup component add --toolchain "$rust_toolchain" rustfmt clippy

    if [[ "$(uname -s)" == "Linux" ]]; then
        if [[ ! -x "$cargo_zigbuild" ]] ||
            [[ "$("$cargo_zigbuild" --version 2>/dev/null)" != "cargo-zigbuild 0.23.0" ]]; then
            "${cargo_for_dev[@]}" install --locked --version 0.23.0 cargo-zigbuild
        fi
        if [[ "$(python3 -m ziglang version 2>/dev/null || true)" == "0.16.0" ]]; then
            zig_version="$(python3 -m ziglang version)"
        elif command -v zig >/dev/null 2>&1; then
            zig_version="$(zig version)"
        else
            fail "install Zig 0.16.0 and rerun make bootstrap"
        fi
        [[ "$zig_version" == "0.16.0" ]] || fail \
            "Zig 0.16.0 is required, found $zig_version"
    fi
}

case "$command_name" in
    bootstrap)
        bootstrap
        ;;
    msrv-check)
        rustup toolchain install 1.85.0 --profile minimal
        "$project_dir/scripts/cargo-toolchain.sh" 1.85.0 \
            check --all-targets --locked
        ;;
    run)
        mkdir -p "$dev_home"
        cd "$project_dir"
        "${cargo_for_dev[@]}" build --locked
        exec env HOME="$dev_home" RUST_LOG="${RUST_LOG:-served=info}" \
            "$project_dir/target/debug/served" daemon
        ;;
    cli)
        [[ "$#" -gt 0 ]] || fail "usage: make cli ARGS='list'"
        mkdir -p "$dev_home"
        cd "$project_dir"
        "${cargo_for_dev[@]}" build --locked
        exec env HOME="$dev_home" "$project_dir/target/debug/served" "$@"
        ;;
    linux-check)
        command -v docker >/dev/null 2>&1 || fail "Docker is required"
        cd "$project_dir"
        docker build -f Dockerfile.dev -t served-dev:1.85 .
        docker run --rm served-dev:1.85
        ;;
    *)
        fail "usage: scripts/dev.sh bootstrap|msrv-check|run|cli|linux-check"
        ;;
esac
