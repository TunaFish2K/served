#!/bin/sh
set -eu

repository="TunaFish2K/served"
github="https://github.com"
temporary_dir=""

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary_dir" ]; then
        rm -rf "$temporary_dir"
    fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

command -v bash >/dev/null 2>&1 || fail "bash is required"
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

case "$(uname -s)" in
    Linux)
        os="linux"
        command -v systemctl >/dev/null 2>&1 ||
            fail "the one-command Linux installer requires systemd; use the binary release with another supervisor"
        ;;
    Darwin) os="macos" ;;
    *) fail "supported installation systems are Linux and macOS" ;;
esac

case "$(uname -m)" in
    x86_64|amd64) arch="amd64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) fail "supported installation architectures are amd64 and arm64" ;;
esac

latest_url="$github/$repository/releases/latest"
release_url="$(
    curl -fsSL --proto '=https' --proto-redir '=https' \
        -o /dev/null -w '%{url_effective}' "$latest_url"
)" || fail "could not resolve the latest served release"
release_url="${release_url%/}"
tag="${release_url##*/}"
printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$' ||
    fail "latest release returned an invalid tag: $tag"

asset="served-${os}-${arch}-${tag}-full.tar.gz"
download_base="$github/$repository/releases/download/$tag"
temporary_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-online.XXXXXX")" ||
    fail "could not create a temporary download directory"

curl -fsSL --proto '=https' --proto-redir '=https' \
    -o "$temporary_dir/$asset" "$download_base/$asset" ||
    fail "could not download $asset"
curl -fsSL --proto '=https' --proto-redir '=https' \
    -o "$temporary_dir/$asset.sha256" "$download_base/$asset.sha256" ||
    fail "could not download $asset.sha256"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$temporary_dir" && sha256sum -c "$asset.sha256") ||
        fail "release checksum verification failed"
elif command -v shasum >/dev/null 2>&1; then
    (cd "$temporary_dir" && shasum -a 256 -c "$asset.sha256") ||
        fail "release checksum verification failed"
else
    fail "sha256sum or shasum is required"
fi

tar -C "$temporary_dir" -xzf "$temporary_dir/$asset" ||
    fail "could not extract $asset"
package_root="$temporary_dir/${asset%.tar.gz}"
[ -f "$package_root/install.sh" ] || fail "release package does not contain install.sh"
bash "$package_root/install.sh" --yes
