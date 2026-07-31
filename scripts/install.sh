#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
service_name="served.service"
binary_target="/usr/local/bin/served"
unit_target="/etc/systemd/system/${service_name}"
user_name="$(id -un)"
group_name="$(id -gn)"
user_home=""
legacy_binary_target=""
legacy_unit_target=""

upgrade=0
upgrade_was_active=0
upgrade_restarted=0
upgrade_handed_off=0
system_binary_was_present=0
system_unit_was_present=0
legacy_present=0
legacy_manager_checked=0
legacy_was_active=0
legacy_was_enabled=0
legacy_was_stopped=0
legacy_was_disabled=0
staging_dir=""
backup_dir=""

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

path_exists() {
    [[ -e "$1" || -L "$1" ]]
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
            ""|y|yes) return 0 ;;
            n|no) return 1 ;;
            *) printf 'please answer y or n\n' >&2 ;;
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
            y|yes) return 0 ;;
            ""|n|no) return 1 ;;
            *) printf 'please answer y or n\n' >&2 ;;
        esac
    done
}

require_target_user() {
    if ((EUID == 0)); then
        fatal "run install.sh as the installation user, not root; it uses sudo internally"
    fi
    command -v getent >/dev/null 2>&1 || fatal "getent is required to resolve the installation user home"
    if ! user_home="$(getent passwd "$user_name" 2>/dev/null | awk -F: 'NR == 1 { print $6; exit }')"; then
        fatal "could not resolve the installation user home from passwd"
    fi
    [[ -n "$user_home" && "$user_home" = /* ]] ||
        fatal "installation user home from passwd is not an absolute path"
    [[ -d "$user_home" ]] || fatal "installation user home does not exist: $user_home"
    if [[ "${HOME:-}" != "$user_home" ]]; then
        printf 'warning: HOME does not match the passwd home; using %s for installation paths\n' "$user_home" >&2
    fi
    legacy_binary_target="$user_home/.local/bin/served"
    legacy_unit_target="$user_home/.config/systemd/user/${service_name}"
    command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system installation"
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

require_service_inactive() {
    local status

    if service_active; then
        fatal "${service_name} is still active"
    else
        status=$?
    fi
    ((status == 1)) || fatal "could not determine whether ${service_name} stopped"
}

service_must_be_active() {
    local status

    if service_active; then
        return 0
    else
        status=$?
    fi
    ((status == 1)) && return 1
    return 2
}

legacy_active_state() {
    systemctl --user is-active "$service_name" 2>/dev/null || true
}

legacy_enabled_state() {
    systemctl --user is-enabled "$service_name" 2>/dev/null || true
}

require_legacy_manager() {
    local state

    ((legacy_manager_checked)) && return 0
    state="$(legacy_active_state)"
    case "$state" in
        active|activating|deactivating|reloading|inactive|failed|dead|unknown|not-found)
            legacy_manager_checked=1
            return 0
            ;;
        *)
            fatal "old user service detected, but the user systemd manager is unavailable; log into a session with the old user manager running and retry migration"
            ;;
    esac
}

inspect_legacy_installation() {
    if ! path_exists "$legacy_binary_target" && ! path_exists "$legacy_unit_target"; then
        return 0
    fi
    legacy_present=1

    if path_exists "$legacy_unit_target"; then
        require_legacy_manager

        local state
        state="$(legacy_active_state)"
        case "$state" in
            active|activating|deactivating|reloading) legacy_was_active=1 ;;
            inactive|failed|dead|unknown|not-found) ;;
            *) fatal "could not determine the old user service state" ;;
        esac

        state="$(legacy_enabled_state)"
        case "$state" in
            enabled|enabled-runtime|linked|linked-runtime|alias) legacy_was_enabled=1 ;;
            disabled|static|indirect|generated|transient|masked|not-found) ;;
            *) fatal "could not determine whether the old user service is enabled" ;;
        esac
    fi

    require_interactive "legacy user-service migration"
    if confirm_yes "Old per-user served installation found. Migrate it to the system service?"; then
        :
    else
        local status=$?
        ((status == 2)) && fatal "could not read migration confirmation"
        printf 'migration canceled; no files changed\n'
        exit 0
    fi
}

migrate_legacy_service() {
    ((legacy_present)) || return 0

    if path_exists "$legacy_unit_target" && ((legacy_was_enabled)); then
        if ! systemctl --user disable "$service_name"; then
            restore_legacy_service || true
            printf 'error: could not disable the old user service; no files changed\n' >&2
            return 1
        fi
        legacy_was_disabled=1
    fi
    if path_exists "$legacy_unit_target" && ((legacy_was_active)); then
        if ! systemctl --user stop "$service_name"; then
            restore_legacy_service || true
            printf 'error: could not stop the old user service; no files changed\n' >&2
            return 1
        fi
        legacy_was_stopped=1
        local state
        state="$(legacy_active_state)"
        if [[ "$state" = active || "$state" = activating || "$state" = deactivating || "$state" = reloading ]]; then
            restore_legacy_service || true
            printf 'error: the old user service is still active; no files changed\n' >&2
            return 1
        fi
    fi
}

restore_legacy_service() {
    local failed=0

    if ((legacy_was_disabled && legacy_was_enabled)); then
        if ! systemctl --user enable "$service_name"; then
            printf 'warning: could not re-enable the old user service during rollback\n' >&2
            failed=1
        fi
    fi
    if ((legacy_was_stopped && legacy_was_active)); then
        if ! systemctl --user start "$service_name"; then
            printf 'warning: could not restart the old user service during rollback\n' >&2
            failed=1
        fi
    fi
    return "$failed"
}

remove_legacy_files() {
    rm -f -- "$legacy_unit_target" "$legacy_binary_target"
    if ((legacy_manager_checked)); then
        systemctl --user daemon-reload ||
            printf 'warning: old user manager daemon-reload failed after migration\n' >&2
    fi
}

read_existing_system_user() {
    [[ -f "$unit_target" ]] || return 0
    root_cmd awk "\$1 ~ /^User=/ { sub(/^User=/, \"\", \$1); print \$1; exit }" \
        "$unit_target" 2>/dev/null || true
}

check_system_user_conflict() {
    local existing_user

    existing_user="$(read_existing_system_user)"
    if [[ -z "$existing_user" ]]; then
        fatal "existing ${unit_target} has no User=; refusing to overwrite an installation with unknown ownership"
    fi
    [[ "$existing_user" = "$user_name" ]] ||
        fatal "${unit_target} belongs to installation user ${existing_user@Q}; refusing to overwrite it as ${user_name@Q}"
}

warn_about_custom_xdg() {
    local warned=0

    if [[ -n "${XDG_CONFIG_HOME:-}" && "$XDG_CONFIG_HOME" != "$user_home/.config" ]]; then
        printf 'warning: XDG_CONFIG_HOME is ignored by served; existing data under %s is not migrated\n' "$XDG_CONFIG_HOME" >&2
        warned=1
    fi
    if [[ -n "${XDG_STATE_HOME:-}" && "$XDG_STATE_HOME" != "$user_home/.local/state" ]]; then
        printf 'warning: XDG_STATE_HOME is ignored by served; existing data under %s is not migrated\n' "$XDG_STATE_HOME" >&2
        warned=1
    fi
    if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
        printf 'warning: XDG_RUNTIME_DIR is ignored by served; the socket uses %s/.local/state/served/runtime\n' "$user_home" >&2
        warned=1
    fi
    ((warned == 0)) || printf 'warning: custom XDG data is left in place; move it manually if needed\n' >&2
}

save_existing_files() {
    if path_exists "$binary_target"; then
        root_cmd cp -a -- "$binary_target" "$backup_dir/served" || return 1
        system_binary_was_present=1
    fi
    if path_exists "$unit_target"; then
        root_cmd cp -a -- "$unit_target" "$backup_dir/served.service" || return 1
        system_unit_was_present=1
    fi
}

restore_file() {
    local target="$1"
    local backup="$2"
    local was_present="$3"

    root_cmd rm -f -- "$target" || return 1
    if [[ "$was_present" == 1 ]]; then
        root_cmd cp -a -- "$backup" "$target" || return 1
    fi
}

restore_system_files() {
    local failed=0

    restore_file "$binary_target" "$backup_dir/served" "$system_binary_was_present" || failed=1
    restore_file "$unit_target" "$backup_dir/served.service" "$system_unit_was_present" || failed=1
    return "$failed"
}

install_new_files() {
    sed \
        -e "s|@SERVED_USER@|$user_name|g" \
        -e "s|@SERVED_GROUP@|$group_name|g" \
        "$script_dir/served.service" > "$staging_dir/served.service"
    if grep -q '@SERVED_' "$staging_dir/served.service"; then
        fatal "served.service template contains unresolved placeholders"
    fi
    root_cmd install -Dm755 "$script_dir/served" "$binary_target" || return 1
    root_cmd install -Dm644 "$staging_dir/served.service" "$unit_target" || return 1
}

cleanup() {
    [[ -n "$staging_dir" ]] && rm -rf -- "$staging_dir"
    [[ -n "$backup_dir" ]] && rm -rf -- "$backup_dir"
}
trap cleanup EXIT

rollback_fresh_install() {
    local failed=0

    if service_active; then
        systemctl_root stop "$service_name" || failed=1
    else
        local status=$?
        ((status == 1)) || failed=1
    fi
    if service_enabled; then
        systemctl_root disable "$service_name" || failed=1
    else
        local status=$?
        ((status == 1)) || failed=1
    fi
    root_cmd rm -f -- "$binary_target" "$unit_target" || failed=1
    systemctl_root daemon-reload || failed=1
    restore_legacy_service || failed=1
    return "$failed"
}

rollback_upgrade() {
    local failed=0

    restore_system_files || failed=1
    systemctl_root daemon-reload || failed=1
    restore_legacy_service || failed=1
    return "$failed"
}

rollback_upgrade_after_start_failure() {
    local failed=0

    rollback_upgrade || failed=1
    if ((upgrade_was_active)); then
        systemctl_root start "$service_name" || failed=1
        service_must_be_active || failed=1
    fi
    return "$failed"
}

abort_fresh() {
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
    if rollback_upgrade; then
        printf 'old installation restored; service remains stopped\n' >&2
    else
        printf 'warning: old installation rollback was incomplete\n' >&2
    fi
    exit 1
}

abort_upgrade_after_start_failure() {
    local reason="$1"

    printf 'error: %s\n' "$reason" >&2
    if rollback_upgrade_after_start_failure; then
        printf 'old installation restored and started; upgrade was not completed\n' >&2
    else
        printf 'warning: old installation rollback or restart was incomplete\n' >&2
    fi
    exit 1
}

print_install_hint() {
    printf 'served is installed at %s and is available from any directory.\n' "$binary_target"
}

print_start_hint() {
    local status

    if service_enabled; then
        printf 'start it with: sudo systemctl start %s\n' "$service_name"
    else
        status=$?
        if ((status == 1)); then
            printf 'enable and start it with: sudo systemctl enable --now %s\n' "$service_name"
        else
            printf 'warning: could not determine whether %s is enabled; check it with sudo systemctl status %s\n' \
                "$service_name" "$service_name" >&2
        fi
    fi
}

require_target_user
[[ -f "$script_dir/served" ]] || fatal "served binary is missing from the package"
[[ -f "$script_dir/served.service" ]] || fatal "served.service template is missing from the package"

warn_about_custom_xdg

if [[ -d "$binary_target" || -d "$unit_target" ]]; then
    fatal "an installation target is a directory; refusing to replace it"
fi
if path_exists "$unit_target"; then
    check_system_user_conflict
fi
if path_exists "$binary_target" || path_exists "$unit_target"; then
    upgrade=1
fi

if ((upgrade)); then
    require_interactive "overwrite upgrade"
    if confirm_yes "Existing served system installation found. Overwrite it?"; then
        :
    else
        status=$?
        ((status == 2)) && fatal "could not read overwrite confirmation"
        printf 'overwrite canceled; no files changed\n'
        exit 0
    fi
fi

inspect_legacy_installation

staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-install.XXXXXX")" ||
    fatal "could not create a staging directory"

if ((upgrade)); then
    backup_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-backup.XXXXXX")" ||
        fatal "could not create an upgrade backup directory"
    save_existing_files || fatal "could not save the existing installation"

    if service_active; then
        upgrade_was_active=1
    else
        status=$?
        ((status == 1)) || fatal "could not determine whether ${service_name} is running"
    fi

fi

if ! migrate_legacy_service; then
    if ((upgrade)); then
        abort_upgrade "could not migrate the old user service"
    fi
    abort_fresh "could not migrate the old user service"
fi

if ! install_new_files; then
    if ((upgrade)); then
        abort_upgrade "could not install the new binary and system unit"
    fi
    abort_fresh "could not install the binary and system unit"
fi

if ! systemctl_root daemon-reload; then
    if ((upgrade)); then
        abort_upgrade "daemon-reload failed after installing the new files"
    fi
    abort_fresh "daemon-reload failed after installing the new files"
fi

if ((upgrade)); then
    if ((upgrade_was_active)); then
        if confirm_yes "Apply the upgraded ${service_name} without stopping managed services?"; then
            if systemctl_root reload "$service_name" && service_must_be_active; then
                upgrade_handed_off=1
            else
                printf 'warning: manager handoff is unavailable; performing a controlled restart\n' >&2
                if systemctl_root restart "$service_name" && service_must_be_active; then
                    upgrade_restarted=1
                else
                    abort_upgrade_after_start_failure "the upgraded ${service_name} failed to start"
                fi
            fi
        else
            status=$?
            if ((status == 2)); then
                printf 'handoff confirmation could not be read; leaving the existing manager running\n' >&2
            else
                printf 'upgrade installed; existing manager remains running\n'
            fi
        fi
    fi

    if ((legacy_present)); then
        if service_must_be_active && ((upgrade_handed_off || upgrade_restarted)); then
            remove_legacy_files
        else
            abort_upgrade "new system service is not active; legacy migration was not completed"
        fi
    fi
    printf 'served upgraded at %s\n' "$binary_target"
    if ((upgrade_handed_off)); then
        printf '%s manager handed off with runners preserved\n' "$service_name"
    elif ((upgrade_restarted)); then
        printf '%s restarted with a controlled service restart\n' "$service_name"
    elif ((upgrade_was_active)); then
        printf '%s remains on the previous manager process\n' "$service_name"
        printf 'apply it later with: sudo systemctl reload %s\n' "$service_name"
    else
        printf '%s was inactive before the upgrade and remains stopped\n' "$service_name"
        print_start_hint
    fi
    print_install_hint
    exit 0
fi

if ! systemctl_root enable --now "$service_name"; then
    abort_fresh "could not enable and start ${service_name}"
fi
if service_must_be_active; then
    :
else
    status=$?
    if ((status == 1)); then
        abort_fresh "${service_name} did not remain active after installation"
    fi
    abort_fresh "could not determine whether ${service_name} started"
fi

if ((legacy_present)); then
    remove_legacy_files
fi
printf 'served installed at %s\n' "$binary_target"
print_install_hint
