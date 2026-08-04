#!/usr/bin/env bash
set -euo pipefail

binary_target="/usr/local/bin/served"
template_name="served@.service"
template_target="/etc/systemd/system/${template_name}"
legacy_system_name="served.service"
legacy_system_target="/etc/systemd/system/${legacy_system_name}"
user_name="$(id -un)"
instance_name="served@${user_name}.service"
user_home=""
legacy_user_binary=""
legacy_user_unit=""
instance_active=0
instance_enabled=0
legacy_system_active=0
legacy_system_enabled=0
legacy_user_active=0
legacy_user_enabled=0
declare -A other_instances=()

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

    while true; do
        printf '%s [y/N] ' "$prompt" >&2
        if ! IFS= read -r answer; then
            printf '\n' >&2
            return 2
        fi
        case "${answer,,}" in
            y|yes) return 0 ;;
            ""|n|no) return 1 ;;
            *) printf 'please answer y or n\n' >&2 ;;
        esac
    done
}

root_cmd() {
    sudo "$@"
}

systemctl_root() {
    root_cmd systemctl "$@"
}

unit_active() {
    local unit="$1"
    local state
    state="$(systemctl_root is-active "$unit" 2>/dev/null || true)"
    case "$state" in
        active|activating|deactivating|reloading) return 0 ;;
        inactive|failed|dead|unknown|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

unit_enabled() {
    local unit="$1"
    local state
    state="$(systemctl_root is-enabled "$unit" 2>/dev/null || true)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias) return 0 ;;
        disabled|static|indirect|generated|transient|masked|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

require_target_user() {
    ((EUID != 0)) || fatal "run uninstall.sh as an installation user, not root; it uses sudo internally"
    [[ "$user_name" != root ]] || fatal "served managers must not run as root"
    [[ "$user_name" =~ ^[A-Za-z_][A-Za-z0-9_.-]*$ ]] ||
        fatal "installation user name cannot be represented as a served systemd instance: $user_name"
    command -v getent >/dev/null 2>&1 || fatal "getent is required to resolve the installation user home"
    user_home="$(getent passwd "$user_name" | awk -F: 'NR == 1 { print $6; exit }')"
    [[ -n "$user_home" && "$user_home" = /* ]] ||
        fatal "installation user home from passwd is not an absolute path"
    command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system uninstall"
    legacy_user_binary="$user_home/.local/bin/served"
    legacy_user_unit="$user_home/.config/systemd/user/${legacy_system_name}"
}

record_system_state() {
    local unit="$1"
    local active_var="$2"
    local enabled_var="$3"
    local status
    if unit_active "$unit"; then
        printf -v "$active_var" '%s' 1
    else
        status=$?
        ((status == 1)) || fatal "could not determine whether ${unit} is active"
    fi
    if unit_enabled "$unit"; then
        printf -v "$enabled_var" '%s' 1
    else
        status=$?
        ((status == 1)) || fatal "could not determine whether ${unit} is enabled"
    fi
}

record_legacy_user_state() {
    local state
    path_exists "$legacy_user_unit" || return 0
    state="$(systemctl --user is-active "$legacy_system_name" 2>/dev/null || true)"
    case "$state" in
        active|activating|deactivating|reloading) legacy_user_active=1 ;;
        inactive|failed|dead|unknown|not-found) ;;
        *) fatal "old user service manager is unavailable" ;;
    esac
    state="$(systemctl --user is-enabled "$legacy_system_name" 2>/dev/null || true)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias) legacy_user_enabled=1 ;;
        disabled|static|indirect|generated|transient|masked|not-found) ;;
        *) fatal "could not determine whether the old user service is enabled" ;;
    esac
}

inspect_legacy_system_unit() {
    local owner
    path_exists "$legacy_system_target" || return 0
    owner="$(root_cmd awk "\$1 ~ /^User=/ { sub(/^User=/, \"\", \$1); print \$1; exit }" "$legacy_system_target" 2>/dev/null || true)"
    [[ -n "$owner" ]] || fatal "existing ${legacy_system_target} has no User=; refusing to remove it"
    [[ "$owner" = "$user_name" ]] ||
        fatal "existing ${legacy_system_target} belongs to ${owner@Q}, not ${user_name@Q}"
    record_system_state "$legacy_system_name" legacy_system_active legacy_system_enabled
}

disable_and_stop_system_unit() {
    local unit="$1"
    local was_active="$2"
    local was_enabled="$3"
    if ((was_enabled)); then
        systemctl_root disable "$unit" || fatal "could not disable ${unit}; files were kept"
    fi
    if ((was_active)); then
        if ! systemctl_root stop "$unit"; then
            ((was_enabled == 0)) || systemctl_root enable "$unit" || true
            fatal "could not stop ${unit}; shared files were kept"
        fi
        if unit_active "$unit"; then
            fatal "${unit} is still active; shared files were kept"
        fi
    fi
}

disable_and_stop_legacy_user_unit() {
    if ((legacy_user_enabled)); then
        systemctl --user disable "$legacy_system_name" ||
            fatal "could not disable the old user service; shared files were kept"
    fi
    if ((legacy_user_active)); then
        if ! systemctl --user stop "$legacy_system_name"; then
            ((legacy_user_enabled == 0)) || systemctl --user enable "$legacy_system_name" || true
            fatal "could not stop the old user service; shared files were kept"
        fi
    fi
}

find_other_instances() {
    local active_units
    local unit
    local unit_files

    unit_files="$(
        systemctl_root list-unit-files 'served@*.service' --no-legend --plain
    )" || fatal "could not list enabled served template instances; shared files were kept"
    active_units="$(
        systemctl_root list-units --type=service --state=active 'served@*.service' \
            --no-legend --plain
    )" || fatal "could not list active served template instances; shared files were kept"

    while read -r unit _; do
        [[ -n "$unit" && "$unit" != "$template_name" && "$unit" != "$instance_name" ]] || continue
        other_instances["$unit"]=1
    done <<<"$unit_files"
    while read -r unit _; do
        [[ -n "$unit" && "$unit" != "$instance_name" ]] || continue
        other_instances["$unit"]=1
    done <<<"$active_units"
}

require_target_user
[[ -t 0 && -t 1 ]] || fatal "interactive terminal required for uninstall"
if [[ -d "$binary_target" || -d "$template_target" || -d "$legacy_system_target" ||
      -d "$legacy_user_binary" || -d "$legacy_user_unit" ]]; then
    fatal "an installation target is a directory; refusing to remove it"
fi

record_system_state "$instance_name" instance_active instance_enabled
inspect_legacy_system_unit
record_legacy_user_state

if confirm_no "Disable and remove served integration for ${user_name}?"; then
    :
else
    status=$?
    ((status == 2)) && fatal "could not read uninstall confirmation"
    printf 'uninstall canceled; no changes made\n'
    exit 0
fi

disable_and_stop_system_unit "$instance_name" "$instance_active" "$instance_enabled"
disable_and_stop_system_unit "$legacy_system_name" "$legacy_system_active" "$legacy_system_enabled"
disable_and_stop_legacy_user_unit

rm -f -- "$legacy_user_unit" "$legacy_user_binary"
if ((legacy_user_active || legacy_user_enabled)); then
    systemctl --user daemon-reload ||
        printf 'warning: old user manager daemon-reload failed after removing legacy files\n' >&2
fi
if path_exists "$legacy_system_target"; then
    root_cmd rm -f -- "$legacy_system_target"
    systemctl_root daemon-reload
fi

find_other_instances
if ((${#other_instances[@]})); then
    printf 'served integration for %s was removed; shared files remain for:' "$user_name"
    printf ' %s' "${!other_instances[@]}"
    printf '\nconfiguration and state were preserved.\n'
    exit 0
fi

if confirm_no "No other enabled or active instances were found. Remove the shared served binary and template?"; then
    root_cmd rm -f -- "$template_target" "$binary_target"
    systemctl_root daemon-reload ||
        printf 'warning: system daemon-reload failed after removing shared served files\n' >&2
    printf 'shared served binary and systemd template removed; configuration and state were preserved.\n'
else
    status=$?
    ((status == 2)) && fatal "could not read shared-file removal confirmation"
    printf 'served integration for %s was removed; shared files and user data were preserved.\n' "$user_name"
fi
