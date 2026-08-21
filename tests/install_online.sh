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

chmod 755 "$mock_bin"/*

run_installer() {
    TEST_UNAME_S="$1" \
    TEST_UNAME_M="$2" \
    TEST_CURL_LOG="$curl_log" \
    TEST_INSTALL_LOG="$install_log" \
    TEST_CHECKSUM_FAIL="${3:-0}" \
    PATH="$mock_bin:/usr/bin:/bin" \
        sh "$project_dir/scripts/install-online.sh"
}

run_installer Linux x86_64
grep -q 'served-linux-amd64-v9.8.7-full.tar.gz$' "$curl_log"
grep -q 'install.sh --yes$' "$install_log"

: > "$curl_log"
: > "$install_log"
run_installer Darwin arm64
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

if run_installer Darwin powerpc >/dev/null 2>&1; then
    printf 'error: unsupported architecture unexpectedly succeeded\n' >&2
    exit 1
fi
if run_installer FreeBSD x86_64 >/dev/null 2>&1; then
    printf 'error: unsupported operating system unexpectedly succeeded\n' >&2
    exit 1
fi

printf 'online installer checks passed\n'
