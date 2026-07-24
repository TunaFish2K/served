#!/usr/bin/env bash
set -euo pipefail

service_name="served.service"
binary_target="/usr/local/bin/served"
unit_target="/etc/systemd/system/${service_name}"
user_name="$(id -un)"
user_home=""
legacy_binary_target=""
legacy_unit_target=""
legacy_manager_checked=0
legacy_was_active=0
legacy_was_enabled=0
legacy_was_disabled=0
legacy_was_stopped=0

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

require_interactive() {
    [[ -t 0 && -t 1 ]] || fatal "interactive terminal required for uninstall"
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

require_target_user() {
    ((EUID != 0)) || fatal "run uninstall.sh as the installation user, not root; it uses sudo internally"
    command -v getent >/dev/null 2>&1 || fatal "getent is required to resolve the installation user home"
    if ! user_home="$(getent passwd "$user_name" 2>/dev/null | awk -F: 'NR == 1 { print $6; exit }')"; then
        fatal "could not resolve the installation user home from passwd"
    fi
    [[ -n "$user_home" && "$user_home" = /* ]] ||
        fatal "installation user home from passwd is not an absolute path"
    [[ -d "$user_home" ]] || fatal "installation user home does not exist: $user_home"
    if [[ "${HOME:-}" != "$user_home" ]]; then
        printf 'warning: HOME does not match the passwd home; using %s for uninstall paths\n' "$user_home" >&2
    fi
    legacy_binary_target="$user_home/.local/bin/served"
    legacy_unit_target="$user_home/.config/systemd/user/${service_name}"
    command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system uninstall"
}

root_cmd() {
    sudo "$@"
}

systemctl_root() {
    root_cmd systemctl "$@"
}

service_active() {
    local state

    state="$(systemctl_root is-active "$service_name" 2>/dev/null || true)"
    case "$state" in
        active|activating|deactivating|reloading) return 0 ;;
        inactive|failed|dead|unknown|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

service_enabled() {
    local state

    state="$(systemctl_root is-enabled "$service_name" 2>/dev/null || true)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias) return 0 ;;
        disabled|static|indirect|generated|transient|masked|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

legacy_active_state() {
    systemctl --user is-active "$service_name" 2>/dev/null || true
}

legacy_enabled_state() {
    systemctl --user is-enabled "$service_name" 2>/dev/null || true
}

require_legacy_manager() {
    local state

    if ((legacy_manager_checked)); then
        return 0
    fi
    state="$(legacy_active_state)"
    case "$state" in
        active|activating|deactivating|reloading|inactive|failed|dead|unknown|not-found)
            legacy_manager_checked=1
            return 0
            ;;
        *)
            fatal "old user service detected, but the user systemd manager is unavailable; no files were removed"
            ;;
    esac
}

inspect_legacy_service() {
    path_exists "$legacy_unit_target" || return 0
    require_legacy_manager

    local state
    state="$(legacy_active_state)"
    case "$state" in
        active|activating|deactivating|reloading) legacy_was_active=1 ;;
        inactive|failed|dead|unknown|not-found) ;;
        *) fatal "could not determine the old user service state; no files were removed" ;;
    esac

    state="$(legacy_enabled_state)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias) legacy_was_enabled=1 ;;
        disabled|static|indirect|generated|transient|masked|not-found) ;;
        *) fatal "could not determine whether the old user service is enabled; no files were removed" ;;
    esac
}

read_existing_system_user() {
    [[ -f "$unit_target" ]] || return 0
    root_cmd awk "\$1 ~ /^User=/ { sub(/^User=/, \"\", \$1); print \$1; exit }" \
        "$unit_target" 2>/dev/null || true
}

check_system_user_conflict() {
    local existing_user

    existing_user="$(read_existing_system_user)"
    [[ -n "$existing_user" ]] ||
        fatal "existing ${unit_target} has no User=; refusing to uninstall an installation with unknown ownership"
    [[ "$existing_user" = "$user_name" ]] ||
        fatal "${unit_target} belongs to installation user ${existing_user@Q}; refusing to remove it as ${user_name@Q}"
}

rollback_legacy() {
    local failed=0

    if ((legacy_was_disabled && legacy_was_enabled)); then
        systemctl --user enable "$service_name" || failed=1
    fi
    if ((legacy_was_stopped && legacy_was_active)); then
        systemctl --user start "$service_name" || failed=1
    fi
    return "$failed"
}

require_target_user
require_interactive

if [[ -d "$binary_target" || -d "$unit_target" || -d "$legacy_binary_target" || -d "$legacy_unit_target" ]]; then
    fatal "an installation target is a directory; refusing to remove it"
fi
if path_exists "$unit_target"; then
    check_system_user_conflict
fi

if confirm_no "Uninstall served and disable its service?"; then
    :
else
    status=$?
    ((status == 2)) && fatal "could not read uninstall confirmation"
    printf 'uninstall canceled; no changes made\n'
    exit 0
fi

inspect_legacy_service

system_was_active=0
if service_active; then
    system_was_active=1
else
    status=$?
    ((status == 1)) || fatal "could not determine whether ${service_name} is running"
fi

if service_enabled; then
    systemctl_root disable "$service_name" ||
        fatal "could not disable ${service_name}; files were kept"
else
    status=$?
    ((status == 1)) || fatal "could not determine whether ${service_name} is enabled"
fi

if ((system_was_active)); then
    systemctl_root stop "$service_name" ||
        fatal "could not stop ${service_name}; files were kept and the service remains disabled"
    service_active && fatal "${service_name} is still active; files were kept and the service remains disabled"
    status=$?
    ((status == 1)) || fatal "could not determine whether ${service_name} stopped"
fi

if path_exists "$legacy_unit_target" && ((legacy_was_enabled)); then
    systemctl --user disable "$service_name" || {
        rollback_legacy || true
        fatal "could not disable the old user service; system files were kept"
    }
    legacy_was_disabled=1
fi
if path_exists "$legacy_unit_target" && ((legacy_was_active)); then
    systemctl --user stop "$service_name" || {
        rollback_legacy || true
        fatal "could not stop the old user service; system files were kept"
    }
    legacy_was_stopped=1
    state="$(legacy_active_state)"
    [[ "$state" != active && "$state" != activating && "$state" != deactivating && "$state" != reloading ]] || {
        rollback_legacy || true
        fatal "the old user service is still active; system files were kept"
    }
fi

root_cmd rm -f -- "$unit_target" "$binary_target"
systemctl_root daemon-reload ||
    printf 'warning: system daemon-reload failed after removing served files\n' >&2

rm -f -- "$legacy_unit_target" "$legacy_binary_target"
if ((legacy_manager_checked)); then
    systemctl --user daemon-reload ||
        printf 'warning: old user manager daemon-reload failed after removing legacy files\n' >&2
fi

printf 'served system service and binary removed; configuration and state were preserved.\n'
