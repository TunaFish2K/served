#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
binary_source="$script_dir/served"
template_source="$script_dir/served.plist"
binary_target="/usr/local/bin/served"
daemon_dir="/Library/LaunchDaemons"
label_prefix="io.github.tunafish2k.served"
user_name="$(id -un)"
user_uid="$(id -u)"
label="${label_prefix}.${user_uid}"
plist_target="${daemon_dir}/${label}.plist"
user_home=""
user_shell=""
rendered_plist=""
backup_dir=""
had_binary=0
had_plist=0
target_loaded=0
binary_changed=1
plist_changed=1
target_reloaded=0
target_fresh_loaded=0
assume_yes=0
active_labels=()
active_users=()
active_homes=()

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

confirm_yes() {
    local prompt="$1"
    local answer

    ((assume_yes)) && return 0
    [[ -t 0 && -t 1 ]] || fatal "interactive terminal required; rerun with --yes for unattended installation"
    while true; do
        printf '%s [Y/n] ' "$prompt" >&2
        if ! IFS= read -r answer; then
            printf '\n' >&2
            return 2
        fi
        case "$answer" in
            ""|y|Y|yes|YES|Yes) return 0 ;;
            n|N|no|NO|No) return 1 ;;
            *) printf 'please answer y or n\n' >&2 ;;
        esac
    done
}

root_cmd() {
    sudo "$@"
}

launchd_loaded() {
    root_cmd launchctl print "system/$1" >/dev/null 2>&1
}

plist_value() {
    root_cmd plutil -extract "$2" raw -o - "$1" 2>/dev/null
}

resolve_account() {
    local account

    ((EUID != 0)) || fatal "run install.sh as an installation user, not root; it uses sudo internally"
    [[ "$user_name" != root ]] || fatal "served managers must not run as root"
    [[ "$user_name" =~ ^[A-Za-z_][A-Za-z0-9_.-]*$ ]] ||
        fatal "installation user name is not safe for a launchd service: $user_name"
    command -v dscl >/dev/null 2>&1 || fatal "dscl is required to resolve the installation account"
    command -v plutil >/dev/null 2>&1 || fatal "plutil is required to render the launchd property list"
    command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system installation"
    account="$(dscl . -read "/Users/$user_name" NFSHomeDirectory UserShell 2>/dev/null)" ||
        fatal "could not resolve installation account $user_name"
    user_home="$(printf '%s\n' "$account" | awk '$1 == "NFSHomeDirectory:" { $1=""; sub(/^ /, ""); print; exit }')"
    user_shell="$(printf '%s\n' "$account" | awk '$1 == "UserShell:" { print $2; exit }')"
    [[ -n "$user_home" && "$user_home" = /* && -d "$user_home" ]] ||
        fatal "installation user home is not an existing absolute path"
    [[ -n "$user_shell" && "$user_shell" = /* && -x "$user_shell" ]] ||
        fatal "installation user shell is not an executable absolute path"
}

render_plist() {
    local state_dir="$user_home/.local/state/served"

    mkdir -p "$state_dir"
    chmod 700 "$state_dir"
    rendered_plist="$(mktemp "${TMPDIR:-/tmp}/served-launchd.XXXXXX")" ||
        fatal "could not create a rendered launchd property list"
    cp "$template_source" "$rendered_plist"
    plutil -replace Label -string "$label" "$rendered_plist"
    plutil -replace UserName -string "$user_name" "$rendered_plist"
    plutil -replace ProgramArguments.0 -string "$user_shell" "$rendered_plist"
    plutil -replace ProgramArguments.2 -string "exec $binary_target daemon" "$rendered_plist"
    plutil -replace WorkingDirectory -string "$user_home" "$rendered_plist"
    plutil -replace EnvironmentVariables.HOME -string "$user_home" "$rendered_plist"
    plutil -replace EnvironmentVariables.USER -string "$user_name" "$rendered_plist"
    plutil -replace EnvironmentVariables.LOGNAME -string "$user_name" "$rendered_plist"
    plutil -replace EnvironmentVariables.SHELL -string "$user_shell" "$rendered_plist"
    plutil -replace StandardOutPath -string "$state_dir/manager.stdout.log" "$rendered_plist"
    plutil -replace StandardErrorPath -string "$state_dir/manager.stderr.log" "$rendered_plist"
    plutil -lint "$rendered_plist" >/dev/null
    [[ "$(plutil -extract Label raw -o - "$rendered_plist" 2>/dev/null)" = "$label" &&
        "$(plutil -extract UserName raw -o - "$rendered_plist" 2>/dev/null)" = "$user_name" &&
        "$(plutil -extract ProgramArguments.0 raw -o - "$rendered_plist" 2>/dev/null)" = "$user_shell" &&
        "$(plutil -extract ProgramArguments.2 raw -o - "$rendered_plist" 2>/dev/null)" = "exec $binary_target daemon" &&
        "$(plutil -extract WorkingDirectory raw -o - "$rendered_plist" 2>/dev/null)" = "$user_home" &&
        "$(plutil -extract EnvironmentVariables.HOME raw -o - "$rendered_plist" 2>/dev/null)" = "$user_home" &&
        "$(plutil -extract EnvironmentVariables.USER raw -o - "$rendered_plist" 2>/dev/null)" = "$user_name" &&
        "$(plutil -extract EnvironmentVariables.LOGNAME raw -o - "$rendered_plist" 2>/dev/null)" = "$user_name" &&
        "$(plutil -extract EnvironmentVariables.SHELL raw -o - "$rendered_plist" 2>/dev/null)" = "$user_shell" &&
        "$(plutil -extract StandardOutPath raw -o - "$rendered_plist" 2>/dev/null)" = "$state_dir/manager.stdout.log" &&
        "$(plutil -extract StandardErrorPath raw -o - "$rendered_plist" 2>/dev/null)" = "$state_dir/manager.stderr.log" ]] ||
        fatal "rendered launchd property list does not match the installation account"
}

inspect_installation() {
    local path
    local instance_label
    local instance_uid
    local instance_user
    local instance_home

    if path_exists "$binary_target"; then
        [[ -f "$binary_target" && ! -L "$binary_target" ]] ||
            fatal "installation target is not a regular file: $binary_target"
    fi
    if path_exists "$plist_target"; then
        [[ -f "$plist_target" && ! -L "$plist_target" ]] ||
            fatal "installation target is not a regular file: $plist_target"
    fi
    path_exists "$binary_target" && had_binary=1
    path_exists "$plist_target" && had_plist=1
    if ((had_binary)) && cmp -s "$binary_source" "$binary_target"; then
        binary_changed=0
    fi
    if ((had_plist)) && cmp -s "$rendered_plist" "$plist_target"; then
        plist_changed=0
    fi
    if launchd_loaded "$label"; then
        target_loaded=1
    fi

    for path in "$daemon_dir"/"${label_prefix}."*.plist; do
        path_exists "$path" || continue
        [[ -f "$path" && ! -L "$path" ]] ||
            fatal "served launchd property list is not a regular file: $path"
        root_cmd plutil -lint "$path" >/dev/null ||
            fatal "installed served launchd property list is invalid: $path"
        instance_label="$(plist_value "$path" Label)"
        instance_uid="${instance_label#"${label_prefix}".}"
        instance_user="$(plist_value "$path" UserName)"
        instance_home="$(plist_value "$path" EnvironmentVariables.HOME)"
        [[ "$instance_label" == "${label_prefix}."* ]] ||
            fatal "unexpected label in served launchd property list: $path"
        [[ "$instance_uid" =~ ^[0-9]+$ ]] ||
            fatal "invalid UID suffix in served launchd property list: $path"
        [[ "$path" == "$daemon_dir/$instance_label.plist" ]] ||
            fatal "served launchd property list name does not match its label: $path"
        [[ "$instance_user" =~ ^[A-Za-z_][A-Za-z0-9_.-]*$ ]] ||
            fatal "invalid UserName in served launchd property list: $path"
        [[ -n "$instance_home" && "$instance_home" = /* ]] ||
            fatal "invalid HOME in served launchd property list: $path"
        if launchd_loaded "$instance_label"; then
            active_labels+=("$instance_label")
            active_users+=("$instance_user")
            active_homes+=("$instance_home")
        fi
    done
}

backup_files() {
    backup_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-macos-backup.XXXXXX")" ||
        fatal "could not create an upgrade backup directory"
    ((had_binary == 0)) || root_cmd cp -p "$binary_target" "$backup_dir/served"
    ((had_plist == 0)) || root_cmd cp -p "$plist_target" "$backup_dir/served.plist"
}

install_files() {
    root_cmd install -d -m 755 "$(dirname "$binary_target")"
    root_cmd install -d -m 755 "$daemon_dir"
    if ((binary_changed)); then
        root_cmd install -m 755 "$binary_source" "$binary_target"
        root_cmd chown root:wheel "$binary_target"
    fi
    if ((plist_changed || had_plist == 0)); then
        root_cmd install -m 644 "$rendered_plist" "$plist_target"
        root_cmd chown root:wheel "$plist_target"
    fi
}

wait_for_manager() {
    local instance_user="$1"
    local instance_home="$2"
    local instance_label="$3"
    local attempt=0

    while ((attempt < 10)); do
        if launchd_loaded "$instance_label" &&
            root_cmd -u "$instance_user" env HOME="$instance_home" "$binary_target" list >/dev/null 2>&1; then
            return 0
        fi
        ((attempt += 1))
        sleep 1
    done
    return 1
}

report_manager_failure() {
    local state_dir="$user_home/.local/state/served"
    local log_path

    printf 'launchd state for %s:\n' "$label" >&2
    root_cmd launchctl print "system/$label" >&2 || true
    for log_path in "$state_dir/manager.stdout.log" "$state_dir/manager.stderr.log"; do
        [[ -f "$log_path" ]] || continue
        printf '%s (last 50 lines):\n' "$log_path" >&2
        tail -n 50 "$log_path" >&2 || true
    done
}

handoff_instance() {
    local instance_user="$1"
    local instance_home="$2"
    local instance_label="$3"

    root_cmd -u "$instance_user" env HOME="$instance_home" \
        "$binary_target" daemon --handoff >/dev/null
    wait_for_manager "$instance_user" "$instance_home" "$instance_label"
}

reload_target_plist() {
    target_reloaded=1
    root_cmd launchctl disable "system/$label"
    root_cmd -u "$user_name" env HOME="$user_home" \
        "$binary_target" daemon --relinquish >/dev/null
    root_cmd launchctl bootout "system/$label"
    root_cmd launchctl enable "system/$label"
    root_cmd launchctl bootstrap system "$plist_target"
    root_cmd launchctl kickstart "system/$label"
    wait_for_manager "$user_name" "$user_home" "$label"
}

restore_files() {
    local failed=0

    if ((had_binary)); then
        root_cmd install -m 755 "$backup_dir/served" "$binary_target" || failed=1
        root_cmd chown root:wheel "$binary_target" || failed=1
    else
        root_cmd rm -f "$binary_target" || failed=1
    fi
    if ((had_plist)); then
        root_cmd install -m 644 "$backup_dir/served.plist" "$plist_target" || failed=1
        root_cmd chown root:wheel "$plist_target" || failed=1
    else
        root_cmd rm -f "$plist_target" || failed=1
    fi
    return "$failed"
}

restore_instances() {
    local index
    local failed=0

    if ((target_fresh_loaded)); then
        root_cmd launchctl disable "system/$label" || failed=1
        root_cmd launchctl bootout "system/$label" || failed=1
    elif ((target_reloaded && target_loaded)); then
        root_cmd launchctl disable "system/$label" || failed=1
        root_cmd -u "$user_name" env HOME="$user_home" \
            "$binary_target" daemon --relinquish >/dev/null 2>&1 || true
        root_cmd launchctl bootout "system/$label" >/dev/null 2>&1 || true
        root_cmd launchctl enable "system/$label" || failed=1
        root_cmd launchctl bootstrap system "$plist_target" || failed=1
        root_cmd launchctl kickstart "system/$label" || failed=1
    fi

    for ((index = 0; index < ${#active_labels[@]}; index++)); do
        if ! handoff_instance \
            "${active_users[$index]}" "${active_homes[$index]}" "${active_labels[$index]}"; then
            failed=1
        fi
    done
    return "$failed"
}

abort_install() {
    local files_failed=0
    local instances_failed=0

    printf 'error: %s\n' "$1" >&2
    restore_files || files_failed=1
    restore_instances || instances_failed=1
    if ((files_failed == 0 && instances_failed == 0)); then
        printf 'previous installation restored\n' >&2
    else
        printf 'warning: installation rollback was incomplete\n' >&2
    fi
    exit 1
}

activate_or_upgrade() {
    local index
    local instance_label

    if ((had_plist == 0)); then
        root_cmd launchctl enable "system/$label" || return 1
        root_cmd launchctl bootstrap system "$plist_target" || return 1
        target_fresh_loaded=1
        root_cmd launchctl kickstart "system/$label" || return 1
        wait_for_manager "$user_name" "$user_home" "$label" || return 1
    fi

    if ((binary_changed)); then
        for ((index = 0; index < ${#active_labels[@]}; index++)); do
            instance_label="${active_labels[$index]}"
            if [[ "$instance_label" == "$label" && "$plist_changed" -eq 1 ]]; then
                continue
            fi
            handoff_instance \
                "${active_users[$index]}" "${active_homes[$index]}" "$instance_label" || return 1
        done
    fi

    if ((had_plist && target_loaded && plist_changed)); then
        reload_target_plist || return 1
    fi
}

cleanup() {
    [[ -z "$rendered_plist" ]] || rm -f "$rendered_plist"
    [[ -z "$backup_dir" ]] || root_cmd rm -rf "$backup_dir" >/dev/null 2>&1 || true
}
trap cleanup EXIT

case "${1:-}" in
    "") ;;
    --yes) assume_yes=1 ;;
    *) fatal "usage: install.sh [--yes]" ;;
esac

[[ "$(uname -s)" == Darwin ]] || fatal "this installer requires macOS"
resolve_account
[[ -f "$binary_source" && -x "$binary_source" ]] ||
    fatal "served binary is missing or not executable in the package"
[[ -f "$template_source" ]] || fatal "served.plist is missing from the package"
render_plist
inspect_installation

if ((binary_changed == 0 && plist_changed == 0 && had_plist)); then
    printf 'served is already installed at %s\n' "$binary_target"
    ((target_loaded)) || printf '%s remains unloaded\n' "$label"
    exit 0
fi

if ((had_binary || had_plist)); then
    if confirm_yes "Install or upgrade served for ${user_name}?"; then
        :
    else
        status=$?
        ((status == 2)) && fatal "could not read installation confirmation"
        printf 'installation canceled; no files changed\n'
        exit 0
    fi
fi

backup_files
install_files || abort_install "could not install the shared binary and launchd property list"
if ! activate_or_upgrade; then
    report_manager_failure
    abort_install "could not activate all served LaunchDaemon instances"
fi

if ((had_plist && target_loaded == 0)); then
    printf '%s remains unloaded; start it with: sudo launchctl bootstrap system %s\n' \
        "$label" "$plist_target"
else
    printf '%s is active\n' "$label"
fi
printf 'served installed at %s\n' "$binary_target"
