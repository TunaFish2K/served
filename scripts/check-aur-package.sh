#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
pkgbuild_source="$project_dir/packaging/aur"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-aur.XXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

for command_name in makepkg sha256sum bsdtar; do
    command -v "$command_name" >/dev/null 2>&1 || fail "$command_name is required"
done

version="$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' "$project_dir/Cargo.toml")"
[[ -n "$version" ]] || fail "could not read the package version"

source_dir="$work_dir/sources"
build_dir="$work_dir/build"
package_dir="$work_dir/packages"
archive_name="served-${version}.tar.gz"
mkdir -p "$source_dir" "$build_dir" "$package_dir"

"$project_dir/scripts/package-source.sh" "$source_dir/$archive_name" "$version"
actual_hash="$(sha256sum "$source_dir/$archive_name" | awk '{ print $1 }')"
expected_hash="$(
    bash -c 'source "$1"; printf "%s\n" "${sha256sums[0]}"' _ "$pkgbuild_source/PKGBUILD"
)"
[[ "$expected_hash" != SOURCE_ARCHIVE_SHA256 ]] || fail "PKGBUILD still contains the source hash placeholder"
[[ "$actual_hash" = "$expected_hash" ]] ||
    fail "source archive hash is $actual_hash, but PKGBUILD records $expected_hash"

cp -a "$pkgbuild_source/." "$build_dir/"
generated_srcinfo="$(cd "$build_dir" && makepkg --printsrcinfo)"
if ! diff -u "$build_dir/.SRCINFO" <(printf '%s\n' "$generated_srcinfo"); then
    fail "packaging/aur/.SRCINFO is stale"
fi

(
    cd "$build_dir"
    SRCDEST="$source_dir" PKGDEST="$package_dir" \
        makepkg --cleanbuild --check --nodeps --noconfirm
)

shopt -s nullglob
all_packages=("$package_dir"/*.pkg.tar.*)
served_packages=("$package_dir"/served-[0-9]*.pkg.tar.*)
systemd_packages=("$package_dir"/served-systemd-[0-9]*.pkg.tar.*)
shopt -u nullglob
(( ${#all_packages[@]} == 2 )) || fail "expected exactly two package artifacts"
(( ${#served_packages[@]} == 1 )) || fail "expected one served binary package"
(( ${#systemd_packages[@]} == 1 )) || fail "expected one served-systemd package"

binary_contents="$(bsdtar -tf "${served_packages[0]}")"
systemd_contents="$(bsdtar -tf "${systemd_packages[0]}")"
grep -Fxq 'usr/bin/served' <<<"$binary_contents" || fail "served package does not contain /usr/bin/served"
if grep -Fq 'served@.service' <<<"$binary_contents"; then
    fail "served binary package unexpectedly contains the systemd unit"
fi
grep -Fxq 'usr/lib/systemd/system/served@.service' <<<"$systemd_contents" ||
    fail "served-systemd package does not contain the service template"
if grep -Fxq 'usr/bin/served' <<<"$systemd_contents"; then
    fail "served-systemd package unexpectedly contains the binary"
fi

if command -v namcap >/dev/null 2>&1; then
    namcap "$build_dir/PKGBUILD" "${served_packages[0]}" "${systemd_packages[0]}"
fi

printf 'AUR packages are valid: %s %s\n' \
    "$(basename "${served_packages[0]}")" \
    "$(basename "${systemd_packages[0]}")"
