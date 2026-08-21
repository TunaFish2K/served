#!/usr/bin/env bash
set -euo pipefail

binary_target="/usr/local/bin/served"
daemon_dir="/Library/LaunchDaemons"
label_prefix="io.github.tunafish2k.served"
user_name="$(id -un)"
user_uid="$(id -u)"
label="${label_prefix}.${user_uid}"
plist_target="${daemon_dir}/${label}.plist"
assume_yes=0
was_loaded=0

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

confirm_no() {
    local prompt="$1"
    local answer

    ((assume_yes)) && return 0
    [[ -t 0 && -t 1 ]] || fatal "interactive terminal required; rerun with --yes for unattended uninstall"
    while true; do
        printf '%s [y/N] ' "$prompt" >&2
        if ! IFS= read -r answer; then
            printf '\n' >&2
            return 2
        fi
        case "$answer" in
            y|Y|yes|YES|Yes) return 0 ;;
            ""|n|N|no|NO|No) return 1 ;;
            *) printf 'please answer y or n\n' >&2 ;;
        esac
    done
}

root_cmd() {
    sudo "$@"
}

case "${1:-}" in
    "") ;;
    --yes) assume_yes=1 ;;
    *) fatal "usage: uninstall.sh [--yes]" ;;
esac

[[ "$(uname -s)" == Darwin ]] || fatal "this uninstaller requires macOS"
((EUID != 0)) || fatal "run uninstall.sh as an installation user, not root; it uses sudo internally"
[[ "$user_name" != root ]] || fatal "served managers must not run as root"
command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system uninstall"
command -v plutil >/dev/null 2>&1 || fatal "plutil is required for system uninstall"

if ! path_exists "$plist_target"; then
    fatal "no served LaunchDaemon is installed for $user_name"
fi
[[ -f "$plist_target" && ! -L "$plist_target" ]] ||
    fatal "served LaunchDaemon property list is not a regular file: $plist_target"
root_cmd plutil -lint "$plist_target" >/dev/null ||
    fatal "served LaunchDaemon property list is invalid: $plist_target"
installed_label="$(root_cmd plutil -extract Label raw -o - "$plist_target" 2>/dev/null)" ||
    fatal "served LaunchDaemon property list has no readable Label"
[[ "$installed_label" == "$label" ]] ||
    fatal "served LaunchDaemon property list label does not match its file name"

if confirm_no "Disable and remove served integration for ${user_name}?"; then
    :
else
    status=$?
    ((status == 2)) && fatal "could not read uninstall confirmation"
    printf 'uninstall canceled; no changes made\n'
    exit 0
fi

if root_cmd launchctl print "system/$label" >/dev/null 2>&1; then
    was_loaded=1
    root_cmd launchctl disable "system/$label"
    if ! root_cmd launchctl bootout "system/$label"; then
        root_cmd launchctl enable "system/$label" || true
        fatal "could not stop ${label}; files were kept"
    fi
fi
if ! root_cmd rm -f "$plist_target"; then
    if ((was_loaded)); then
        root_cmd launchctl enable "system/$label" || true
        root_cmd launchctl bootstrap system "$plist_target" || true
        root_cmd launchctl kickstart "system/$label" || true
    fi
    fatal "could not remove ${plist_target}; the previous LaunchDaemon was restored"
fi

other_plists=""
for candidate in "$daemon_dir"/"${label_prefix}."*.plist; do
    if path_exists "$candidate"; then
        other_plists="$candidate"
        break
    fi
done
if [[ -n "$other_plists" ]]; then
    printf 'served integration for %s was removed; shared binary remains for other users.\n' "$user_name"
    printf 'configuration and state were preserved.\n'
    exit 0
fi

if confirm_no "No other served LaunchDaemons were found. Remove the shared served binary?"; then
    root_cmd rm -f "$binary_target"
    printf 'shared served binary removed; configuration and state were preserved.\n'
else
    status=$?
    ((status == 2)) && fatal "could not read shared-file removal confirmation"
    printf 'served integration for %s was removed; shared binary and user data were preserved.\n' "$user_name"
fi
