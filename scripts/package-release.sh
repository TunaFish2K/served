#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$project_dir/dist"
requested_arch="${1:-all}"
requested_variant="${2:-all}"
rust_toolchain="${RUST_TOOLCHAIN:-stable}"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

checksum() {
    local file="$1"
    local name

    name="$(basename "$file")"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$dist" && sha256sum "$name" > "${name}.sha256")
    else
        (cd "$dist" && shasum -a 256 "$name" > "${name}.sha256")
    fi
}

package_linux() {
    local version="$1"
    local arch="$2"
    local target="$3"
    local binary_asset="served-linux-${arch}-v${version}${variant_suffix}-binary"
    local full_asset="served-linux-${arch}-v${version}${variant_suffix}-full.tar.gz"
    local full_root="$dist/served-linux-${arch}-v${version}${variant_suffix}-full"

    "$project_dir/scripts/verify-release-binary.sh" \
        linux "$arch" "$build_root/$target/release/served"
    install -m 755 "$build_root/$target/release/served" "$dist/$binary_asset"
    mkdir -p "$full_root"
    install -m 755 "$build_root/$target/release/served" "$full_root/served"
    sed 's|@SERVED_BIN@|/usr/local/bin/served|g' \
        "$project_dir/systemd/served@.service" > "$full_root/served@.service"
    chmod 644 "$full_root/served@.service"
    install -m 755 "$project_dir/scripts/install.sh" "$full_root/install.sh"
    install -m 755 "$project_dir/scripts/uninstall.sh" "$full_root/uninstall.sh"
    install -m 644 "$project_dir/LICENSE" "$full_root/LICENSE"
    "$project_dir/scripts/package-docs.sh" "$full_root"
    tar -C "$dist" -czf "$dist/$full_asset" "$(basename "$full_root")"
    rm -rf "$full_root"
    checksum "$dist/$binary_asset"
    checksum "$dist/$full_asset"
}

package_macos() {
    local version="$1"
    local arch="$2"
    local target="$3"
    local binary_asset="served-macos-${arch}-v${version}${variant_suffix}-binary"
    local full_asset="served-macos-${arch}-v${version}${variant_suffix}-full.tar.gz"
    local full_root="$dist/served-macos-${arch}-v${version}${variant_suffix}-full"

    install -m 755 "$build_root/$target/release/served" "$dist/$binary_asset"
    codesign --force --sign - --timestamp=none "$dist/$binary_asset"
    "$project_dir/scripts/verify-release-binary.sh" macos "$arch" "$dist/$binary_asset"
    mkdir -p "$full_root"
    install -m 755 "$dist/$binary_asset" "$full_root/served"
    install -m 644 "$project_dir/launchd/served.plist" "$full_root/served.plist"
    install -m 755 "$project_dir/scripts/install-macos.sh" "$full_root/install.sh"
    install -m 755 "$project_dir/scripts/uninstall-macos.sh" "$full_root/uninstall.sh"
    install -m 644 "$project_dir/LICENSE" "$full_root/LICENSE"
    "$project_dir/scripts/package-docs.sh" "$full_root"
    tar -C "$dist" -czf "$dist/$full_asset" "$(basename "$full_root")"
    rm -rf "$full_root"
    checksum "$dist/$binary_asset"
    checksum "$dist/$full_asset"
}

cd "$project_dir"
package_id="$(
    "$project_dir/scripts/cargo-toolchain.sh" "$rust_toolchain" pkgid --locked
)"
detected_version="${package_id##*#}"
detected_version="${detected_version##*@}"
version="${RELEASE_VERSION:-$detected_version}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] ||
    fail "could not determine a semantic package version"
case "$requested_arch" in
    all|amd64|arm64) ;;
    *) fail "usage: scripts/package-release.sh [all|amd64|arm64] [all|full|headless]" ;;
esac

case "$requested_variant" in
    all) variants=(full headless) ;;
    full|headless) variants=("$requested_variant") ;;
    *) fail "package variant must be all, full, or headless" ;;
esac
rm -rf "$dist"
mkdir -p "$dist"
for variant in "${variants[@]}"; do
    variant_suffix=""
    build_root="$project_dir/target"
    if [[ "$variant" == headless ]]; then
        variant_suffix="-headless"
        build_root="$project_dir/target/headless"
    fi
    ./scripts/build-targets.sh "$requested_arch" "$variant"

    case "$(uname -s)" in
        Linux)
            if [[ "$requested_arch" == "all" || "$requested_arch" == "amd64" ]]; then
                package_linux "$version" amd64 x86_64-unknown-linux-gnu
            fi
            if [[ "$requested_arch" == "all" || "$requested_arch" == "arm64" ]]; then
                package_linux "$version" arm64 aarch64-unknown-linux-gnu
            fi
            ;;
        Darwin)
            if [[ "$requested_arch" == "all" || "$requested_arch" == "amd64" ]]; then
                package_macos "$version" amd64 x86_64-apple-darwin
            fi
            if [[ "$requested_arch" == "all" || "$requested_arch" == "arm64" ]]; then
                package_macos "$version" arm64 aarch64-apple-darwin
            fi
            ;;
        *) fail "release packaging supports macOS and Linux" ;;
    esac
done

printf 'release assets written to %s\n' "$dist"
