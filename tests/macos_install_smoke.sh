#!/usr/bin/env bash
set -euo pipefail

archive="${1:-}"
binary_target="/usr/local/bin/served"
label_prefix="io.github.tunafish2k.served"
label="${label_prefix}.$(id -u)"
plist_target="/Library/LaunchDaemons/${label}.plist"
other_label="${label_prefix}.999999"
other_plist="/Library/LaunchDaemons/${other_label}.plist"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/served-macos-smoke.XXXXXX")"
service_dir="$test_root/service"
package_dir=""
owns_install=0
service_name="served-macos-smoke"

fail() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

cleanup() {
    if ((owns_install)); then
        "$binary_target" disable "$service_name" >/dev/null 2>&1 || true
        sudo launchctl bootout "system/$label" >/dev/null 2>&1 || true
        sudo rm -f "$plist_target" "$other_plist" "$binary_target"
    fi
    rm -rf "$test_root"
}
trap cleanup EXIT

[[ "$(uname -s)" == Darwin ]] || fail "macOS install smoke test requires macOS"
[[ -f "$archive" ]] || fail "usage: tests/macos_install_smoke.sh FULL_ARCHIVE"
[[ ! -e "$binary_target" && ! -e "$plist_target" && ! -e "$other_plist" ]] ||
    fail "refusing to replace an existing served installation on the smoke-test host"

tar -C "$test_root" -xzf "$archive"
package_dir="$test_root/$(basename "$archive" .tar.gz)"
[[ -x "$package_dir/install.sh" && -x "$package_dir/uninstall.sh" ]] ||
    fail "macOS full package is missing its lifecycle scripts"

"$package_dir/install.sh" --yes
owns_install=1
"$binary_target" list >/dev/null
sudo launchctl print "system/$label" >/dev/null

mkdir -p "$service_dir"
cat > "$service_dir/.served.json" <<EOF
{
  name: "$service_name",
  command: "sleep 300",
  tty: false,
  restart: "never",
}
EOF

(cd "$service_dir" && "$binary_target" enable)

service_pid() {
    local pid
    local attempt=0

    while ((attempt < 20)); do
        pid="$(
            "$binary_target" list \
                | awk -v name="$service_name" '$1 == name { for (i = 1; i <= NF; i++) if ($i ~ /^pid=/) { sub(/^pid=/, "", $i); print $i; exit } }'
        )"
        if [[ -n "$pid" && "$pid" != "-" ]]; then
            printf '%s\n' "$pid"
            return 0
        fi
        ((attempt += 1))
        sleep 1
    done
    return 1
}

before_pid="$(service_pid)" || fail "service did not start before the launchd reload"
sudo plutil -insert ServedSmokeMarker -bool true "$plist_target"
"$package_dir/install.sh" --yes
after_pid="$(service_pid)" || fail "service was not adopted after the launchd reload"
[[ "$before_pid" == "$after_pid" ]] ||
    fail "launchd reload changed service pid from $before_pid to $after_pid"

"$binary_target" disable "$service_name"
sudo launchctl bootout "system/$label"
sudo plutil -insert ServedSmokeMarker -bool true "$plist_target"
"$package_dir/install.sh" --yes
if sudo launchctl print "system/$label" >/dev/null 2>&1; then
    fail "installer loaded an instance that was stopped before upgrade"
fi
sudo launchctl enable "system/$label"
sudo launchctl bootstrap system "$plist_target"
sudo launchctl kickstart "system/$label"
for _ in {1..10}; do
    "$binary_target" list >/dev/null 2>&1 && break
    sleep 1
done
"$binary_target" list >/dev/null || fail "manager did not restart after the unloaded-instance check"

cp "$package_dir/served" "$test_root/served.good"
printf '#!/bin/sh\nexit 1\n' > "$package_dir/served"
chmod 755 "$package_dir/served"
if "$package_dir/install.sh" --yes >/dev/null 2>&1; then
    fail "upgrade with a broken manager binary unexpectedly succeeded"
fi
cmp -s "$binary_target" "$test_root/served.good" ||
    fail "failed upgrade did not restore the installed binary"
"$binary_target" list >/dev/null || fail "failed upgrade did not restore the active manager"
install -m 755 "$test_root/served.good" "$package_dir/served"

sudo cp "$plist_target" "$other_plist"
sudo plutil -replace Label -string "$other_label" "$other_plist"
sudo chown root:wheel "$other_plist"
"$package_dir/uninstall.sh" --yes
[[ -e "$binary_target" && ! -e "$plist_target" ]] ||
    fail "macOS uninstaller removed a binary still shared by another instance"
sudo rm -f "$other_plist"

"$package_dir/install.sh" --yes
"$package_dir/uninstall.sh" --yes
owns_install=0
[[ ! -e "$binary_target" && ! -e "$plist_target" && ! -e "$other_plist" ]] ||
    fail "macOS uninstaller left shared installation files behind"
[[ -d "$HOME/.local/state/served" ]] || fail "macOS uninstaller removed user state"

printf 'macOS install smoke checks passed\n'
