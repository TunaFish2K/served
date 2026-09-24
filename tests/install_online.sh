#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-online-test.XXXXXX")"
mock_bin="$test_dir/bin"
curl_log="$test_dir/curl.log"
install_log="$test_dir/install.log"

cleanup() {
    rm -rf "$test_dir"
}
trap cleanup EXIT
mkdir -p "$mock_bin"

cat > "$mock_bin/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) printf '%s\n' "${TEST_UNAME_S:?}" ;;
    -m) printf '%s\n' "${TEST_UNAME_M:?}" ;;
    *) exit 2 ;;
esac
EOF

cat > "$mock_bin/systemctl" <<'EOF'
#!/bin/sh
exit 0
EOF

cat > "$mock_bin/curl" <<'EOF'
#!/bin/sh
output=""
write_effective=0
last=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            output="$2"
            shift 2
            ;;
        -w)
            write_effective=1
            shift 2
            ;;
        --proto|--proto-redir)
            shift 2
            ;;
        -*) shift ;;
        *)
            last="$1"
            shift
            ;;
    esac
done
printf '%s\n' "$last" >> "${TEST_CURL_LOG:?}"
if [ "$write_effective" -eq 1 ]; then
    printf '%s' 'https://github.com/TunaFish2K/served/releases/tag/v9.8.7'
elif [ -n "$output" ]; then
    [ "${TEST_MISSING_ASSET:-0}" -eq 0 ] || exit 22
    : > "$output"
fi
EOF

cat > "$mock_bin/sha256sum" <<'EOF'
#!/bin/sh
if [ "${TEST_CHECKSUM_FAIL:-0}" -eq 1 ]; then
    exit 1
fi
exit 0
EOF

cat > "$mock_bin/tar" <<'EOF'
#!/bin/sh
destination=""
archive=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -C)
            destination="$2"
            shift 2
            ;;
        -xzf)
            archive="$2"
            shift 2
            ;;
        *) shift ;;
    esac
done
root="$(basename "$archive" .tar.gz)"
mkdir -p "$destination/$root"
: > "$destination/$root/install.sh"
EOF

cat > "$mock_bin/bash" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" > "${TEST_INSTALL_LOG:?}"
EOF

cat > "$mock_bin/served" <<'EOF'
#!/bin/sh
[ "$*" = 'version --output json' ] || exit 1
[ "${TEST_INSTALLED_VARIANT:-old}" != old ] || exit 2
printf '{"schema_version":1,"ok":true,"data":{"variant":"%s"}}\n' "$TEST_INSTALLED_VARIANT"
EOF

chmod 755 "$mock_bin"/*

run_installer() {
    local os="$1" arch="$2" checksum="${3:-0}"
    shift 3
    TEST_UNAME_S="$os" \
    TEST_UNAME_M="$arch" \
    TEST_CURL_LOG="$curl_log" \
    TEST_INSTALL_LOG="$install_log" \
    TEST_CHECKSUM_FAIL="$checksum" \
    PATH="$mock_bin:/usr/bin:/bin" \
        sh "$project_dir/scripts/install-online.sh" "$@"
}

run_installer Linux x86_64 0
grep -q 'served-linux-amd64-v9.8.7-full.tar.gz$' "$curl_log"
grep -q 'install.sh --yes$' "$install_log"

: > "$curl_log"
: > "$install_log"
run_installer Darwin arm64 0
grep -q 'served-macos-arm64-v9.8.7-full.tar.gz$' "$curl_log"
grep -q 'install.sh --yes$' "$install_log"

: > "$install_log"
if run_installer Linux aarch64 1 >/dev/null 2>&1; then
    printf 'error: checksum failure unexpectedly succeeded\n' >&2
    exit 1
fi
[[ ! -s "$install_log" ]] || {
    printf 'error: package installer ran after checksum failure\n' >&2
    exit 1
}

if run_installer Darwin powerpc 0 >/dev/null 2>&1; then
    printf 'error: unsupported architecture unexpectedly succeeded\n' >&2
    exit 1
fi
if run_installer FreeBSD x86_64 0 >/dev/null 2>&1; then
    printf 'error: unsupported operating system unexpectedly succeeded\n' >&2
    exit 1
fi

for platform in 'Linux x86_64 linux amd64' 'Linux aarch64 linux arm64' 'Darwin x86_64 macos amd64' 'Darwin arm64 macos arm64'; do
    read -r os arch asset_os asset_arch <<< "$platform"
    for variant in full headless; do
        : > "$curl_log"
        run_installer "$os" "$arch" 0 --variant "$variant"
        suffix=""
        [[ "$variant" != headless ]] || suffix=-headless
        grep -q "served-${asset_os}-${asset_arch}-v9.8.7${suffix}-full.tar.gz$" "$curl_log"
    done
done
for variant in full headless; do
    export TEST_INSTALLED_VARIANT="$variant"
    : > "$curl_log"
    run_installer Linux x86_64 0
    suffix=""
    [[ "$variant" != headless ]] || suffix=-headless
    grep -q "served-linux-amd64-v9.8.7${suffix}-full.tar.gz$" "$curl_log"
done
: > "$curl_log"
run_installer Linux x86_64 0 --variant full
grep -q 'served-linux-amd64-v9.8.7-full.tar.gz$' "$curl_log"
for variant in full headless; do
    : > "$install_log"
    if run_installer Linux x86_64 1 --variant "$variant" >/dev/null 2>&1; then
        echo 'checksum failure unexpectedly succeeded' >&2; exit 1
    fi
    [[ ! -s "$install_log" ]]
done
export TEST_MISSING_ASSET=1
: > "$curl_log"
: > "$install_log"
if run_installer Linux x86_64 0 --variant headless >/dev/null 2>&1; then
    echo 'missing headless asset unexpectedly succeeded' >&2; exit 1
fi
[[ ! -s "$install_log" ]]
if grep -q 'v9.8.7-full.tar.gz$' "$curl_log"; then
    echo 'installer fell back to the full asset' >&2; exit 1
fi
unset TEST_MISSING_ASSET
for option in '--variant=unknown' '--variant' '--unknown'; do
    if run_installer Linux x86_64 0 "$option" >/dev/null 2>&1; then
        echo 'invalid installer option unexpectedly succeeded' >&2; exit 1
    fi
done
export TEST_INSTALLED_VARIANT=malformed
if run_installer Linux x86_64 0 >/dev/null 2>&1; then
    echo 'malformed variant unexpectedly accepted' >&2; exit 1
fi

printf 'online installer checks passed\n'
