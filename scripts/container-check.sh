#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_dir"

make check
make shellcheck systemd-check
scripts/build-targets.sh all

for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
    binary="target/$target/release/served"
    if [[ "$target" == x86_64-* ]]; then
        arch="amd64"
    else
        arch="arm64"
    fi
    scripts/verify-release-binary.sh linux "$arch" "$binary"
done
