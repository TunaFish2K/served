#!/usr/bin/env bash
set -euo pipefail

bin_dir="$HOME/.local/bin"
binary_target="$bin_dir/served"
unit_target="$HOME/.config/systemd/user/served.service"
service_name="served.service"

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

require_interactive() {
    if [[ ! -t 0 || ! -t 1 ]]; then
        fatal "interactive terminal required for uninstall"
    fi
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
            y|yes)
                return 0
                ;;
            ""|n|no)
                return 1
                ;;
            *)
                printf 'please answer y or n\n' >&2
                ;;
        esac
    done
}

service_active() {
    local state

    state="$(systemctl --user is-active "$service_name" 2>/dev/null || true)"
    case "$state" in
        active|activating|deactivating|reloading)
            return 0
            ;;
        inactive|failed|dead|unknown|not-found)
            return 1
            ;;
        *)
            return 2
            ;;
    esac
}

disable_service() {
    local state

    state="$(systemctl --user is-enabled "$service_name" 2>/dev/null || true)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias)
            systemctl --user disable "$service_name" || \
                fatal "could not disable ${service_name}"
            ;;
        disabled|static|indirect|generated|transient|masked|not-found)
            ;;
        *)
            fatal "could not determine whether ${service_name} is enabled"
            ;;
    esac
}

if [[ -d "$binary_target" || -d "$unit_target" ]]; then
    fatal "an installation target is a directory; refusing to remove it"
fi

require_interactive
if confirm_no "Uninstall served and disable its user service?"; then
    :
else
    status=$?
    if ((status == 2)); then
        fatal "could not read uninstall confirmation"
    fi
    printf 'uninstall canceled; no changes made\n'
    exit 0
fi

disable_service

if service_active; then
    if ! systemctl --user stop "$service_name"; then
        fatal "could not stop ${service_name}; files were kept and the service remains disabled"
    fi
    if service_active; then
        fatal "${service_name} is still active; files were kept and the service remains disabled"
    else
        status=$?
    fi
    if ((status != 1)); then
        fatal "could not determine whether ${service_name} stopped"
    fi
else
    status=$?
    if ((status != 1)); then
        fatal "could not determine whether ${service_name} is running"
    fi
fi

rm -f -- "$unit_target" "$binary_target"

if ! systemctl --user daemon-reload; then
    printf 'warning: daemon-reload failed after removing served files\n' >&2
fi

printf 'served binary and user unit removed; service is disabled and shell configuration was not changed.\n'
