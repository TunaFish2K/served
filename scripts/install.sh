#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
binary_target="$bin_dir/served"
unit_target="$HOME/.config/systemd/user/served.service"
service_name="served.service"
user_name="${USER:-$(id -un)}"

upgrade=0
upgrade_was_active=0
fresh_enable_attempted=0
linger_before=0
linger_attempted=0
backup_dir=""
staging_dir=""
binary_was_present=0
unit_was_present=0

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

require_interactive() {
    if [[ ! -t 0 || ! -t 1 ]]; then
        fatal "interactive terminal required for $1"
    fi
}

confirm_yes() {
    local prompt="$1"
    local answer

    while true; do
        printf '%s [Y/n] ' "$prompt" >&2
        if ! IFS= read -r answer; then
            printf '\n' >&2
            return 2
        fi

        case "${answer,,}" in
            ""|y|yes)
                return 0
                ;;
            n|no)
                return 1
                ;;
            *)
                printf 'please answer y or n\n' >&2
                ;;
        esac
    done
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

require_service_inactive() {
    local status

    if service_active; then
        fatal "${service_name} is still active"
    else
        status=$?
    fi
    if ((status != 1)); then
        fatal "could not determine whether ${service_name} stopped"
    fi
}

service_must_be_active() {
    local status

    if service_active; then
        return 0
    else
        status=$?
    fi
    if ((status == 1)); then
        return 1
    fi
    return 2
}

read_linger_state() {
    local state

    state="$(loginctl show-user "$user_name" --property=Linger --value 2>/dev/null || true)"
    case "$state" in
        yes|true|1)
            linger_before=1
            ;;
        no|false|0)
            linger_before=0
            ;;
        *)
            fatal "could not determine the existing linger state for $user_name"
            ;;
    esac
}

save_existing_files() {
    if path_exists "$binary_target"; then
        cp -a -- "$binary_target" "$backup_dir/served" || return 1
        binary_was_present=1
    fi

    if path_exists "$unit_target"; then
        cp -a -- "$unit_target" "$backup_dir/served.service" || return 1
        unit_was_present=1
    fi
}

restore_file() {
    local target="$1"
    local backup="$2"
    local was_present="$3"

    if [[ "$was_present" == 1 ]]; then
        rm -f -- "$target" || return 1
        cp -a -- "$backup" "$target" || return 1
    else
        rm -f -- "$target" || return 1
    fi
}

restore_upgrade_files() {
    local failed=0

    restore_file "$binary_target" "$backup_dir/served" "$binary_was_present" || failed=1
    restore_file "$unit_target" "$backup_dir/served.service" "$unit_was_present" || failed=1
    return "$failed"
}

remove_fresh_files() {
    rm -f -- "$binary_target" "$unit_target"
}

rollback_fresh_install() {
    local failed=0
    local status

    if ((fresh_enable_attempted)); then
        if ! systemctl --user disable "$service_name"; then
            printf 'warning: could not disable %s during rollback\n' "$service_name" >&2
            failed=1
        fi
    fi

    if service_active; then
        if ! systemctl --user stop "$service_name"; then
            printf 'warning: could not stop %s during rollback\n' "$service_name" >&2
            failed=1
        fi
    else
        status=$?
        if ((status != 1)); then
            printf 'warning: could not determine %s state during rollback\n' "$service_name" >&2
            failed=1
        fi
    fi

    if ((failed == 0)); then
        remove_fresh_files || failed=1
        if ! systemctl --user daemon-reload; then
            printf 'warning: daemon-reload failed during rollback\n' >&2
            failed=1
        fi
    fi

    if ((linger_attempted && linger_before == 0)); then
        if ! loginctl disable-linger "$user_name"; then
            printf 'warning: could not restore linger state\n' >&2
            failed=1
        fi
    fi

    return "$failed"
}

rollback_upgrade_files() {
    local failed=0

    if ! restore_upgrade_files; then
        failed=1
    fi
    if ! systemctl --user daemon-reload; then
        printf 'warning: daemon-reload failed while restoring the old installation\n' >&2
        failed=1
    fi
    return "$failed"
}

abort_fresh_install() {
    local reason="$1"

    printf 'error: %s\n' "$reason" >&2
    if rollback_fresh_install; then
        printf 'fresh installation rolled back\n' >&2
    else
        printf 'warning: fresh installation rollback was incomplete\n' >&2
    fi
    exit 1
}

abort_upgrade() {
    local reason="$1"

    printf 'error: %s\n' "$reason" >&2
    if rollback_upgrade_files; then
        printf 'old installation restored; service remains stopped\n' >&2
    else
        printf 'warning: old installation rollback was incomplete\n' >&2
    fi
    exit 1
}

abort_upgrade_after_start_failure() {
    local reason="$1"
    local failed=0
    local status

    printf 'error: %s\n' "$reason" >&2
    if ! restore_upgrade_files; then
        failed=1
    fi
    if ! systemctl --user daemon-reload; then
        printf 'warning: daemon-reload failed while restoring the old installation\n' >&2
        failed=1
    fi

    if ((failed == 0)); then
        if ! systemctl --user start "$service_name"; then
            printf 'warning: the old %s could not be started\n' "$service_name" >&2
            failed=1
        elif service_must_be_active; then
            :
        else
            status=$?
            printf 'warning: the old %s did not remain active\n' "$service_name" >&2
            failed=1
            if ((status != 1)); then
                printf 'warning: could not determine the old service state\n' >&2
            fi
        fi
    fi

    if ((failed == 0)); then
        printf 'old installation restored and started; upgrade was not completed\n' >&2
    else
        printf 'warning: old installation rollback or restart was incomplete; service remains stopped\n' >&2
    fi
    exit 1
}

install_new_files() {
    mkdir -p "$bin_dir" "$(dirname -- "$unit_target")" || return 1
    install -Dm755 "$script_dir/served" "$staging_dir/served" || return 1
    install -Dm644 "$script_dir/served.service" \
        "$staging_dir/served.service" || return 1
    mv -f -- "$staging_dir/served" "$binary_target" || return 1
    mv -f -- "$staging_dir/served.service" "$unit_target" || return 1
}

print_export_hint() {
    printf 'copy this command into the current shell to use served from any directory:\n'
    printf '%s\n' "export PATH=\"\$HOME/.local/bin:\$PATH\""
}

[[ -f "$script_dir/served" ]] || fatal "served binary is missing from the package"
[[ -f "$script_dir/served.service" ]] || fatal "served.service is missing from the package"

if [[ -d "$binary_target" || -d "$unit_target" ]]; then
    fatal "an installation target is a directory; refusing to replace it"
fi

if path_exists "$binary_target" || path_exists "$unit_target"; then
    upgrade=1
fi

if ((upgrade)); then
    require_interactive "overwrite upgrade"
    if confirm_yes "Existing served installation found. Overwrite it?"; then
        :
    else
        status=$?
        if ((status == 2)); then
            fatal "could not read overwrite confirmation"
        fi
        printf 'overwrite canceled; no files changed\n'
        exit 0
    fi
else
    read_linger_state
fi

if ! staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-install.XXXXXX")"; then
    fatal "could not create a staging directory"
fi

if ((upgrade)); then
    if ! backup_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-backup.XXXXXX")"; then
        fatal "could not create an upgrade backup directory"
    fi

    if ! save_existing_files; then
        fatal "could not save the existing installation"
    fi

    if service_active; then
        upgrade_was_active=1
    else
        status=$?
        if ((status != 1)); then
            fatal "could not determine whether ${service_name} is running"
        fi
    fi

    if ((upgrade_was_active)); then
        if confirm_yes "${service_name} is running. Stop it before upgrading?"; then
            :
        else
            status=$?
            if ((status == 2)); then
                fatal "could not read stop confirmation"
            fi
            printf 'upgrade canceled; no files changed\n'
            exit 0
        fi

        if ! systemctl --user stop "$service_name"; then
            fatal "could not stop ${service_name}; no files changed"
        fi
        require_service_inactive
    fi
fi

if ! install_new_files; then
    if ((upgrade)); then
        abort_upgrade "could not install the new binary and user unit"
    fi
    abort_fresh_install "could not install the binary and user unit"
fi

if ! systemctl --user daemon-reload; then
    if ((upgrade)); then
        abort_upgrade "daemon-reload failed after installing the new files"
    fi
    abort_fresh_install "daemon-reload failed after installing the new files"
fi

if ((upgrade)); then
    if ((upgrade_was_active)); then
        if confirm_yes "Restart ${service_name} with the upgraded files?"; then
            if systemctl --user start "$service_name" && service_active; then
                :
            else
                abort_upgrade_after_start_failure "the upgraded ${service_name} failed to start"
            fi
        else
            status=$?
            if ((status == 2)); then
                printf 'restart confirmation could not be read; leaving the upgraded service stopped\n' >&2
            else
                printf 'upgrade installed; service remains stopped\n'
            fi
            printf 'start it with: systemctl --user start %s\n' "$service_name"
            print_export_hint
            exit 0
        fi
    fi

    printf 'served upgraded at %s\n' "$binary_target"
    if ((upgrade_was_active)); then
        printf '%s restarted\n' "$service_name"
    else
        printf '%s was inactive before the upgrade and remains stopped\n' "$service_name"
    fi
    print_export_hint
    exit 0
fi

fresh_enable_attempted=1
if ! systemctl --user enable --now "$service_name"; then
    abort_fresh_install "could not enable and start ${service_name}"
fi
if service_must_be_active; then
    :
else
    status=$?
    if ((status == 1)); then
        abort_fresh_install "${service_name} did not remain active after installation"
    fi
    abort_fresh_install "could not determine whether ${service_name} started"
fi

if ((linger_before == 0)); then
    linger_attempted=1
    if ! loginctl enable-linger "$user_name"; then
        abort_fresh_install "could not enable user lingering"
    fi
fi

printf 'served installed at %s\n' "$binary_target"
print_export_hint
