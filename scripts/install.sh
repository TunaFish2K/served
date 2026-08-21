#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
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

staging_dir=""
backup_dir=""
had_binary=0
had_template=0
had_legacy_system=0
shared_upgrade=0
fresh_integration=0
legacy_system_present=0
legacy_user_present=0
legacy_system_active=0
legacy_system_enabled=0
legacy_user_active=0
legacy_user_enabled=0
instance_active=0
instance_enabled=0
target_active=0
target_enabled=0
handoff_preserved=0
assume_yes=0
declare -a active_instances=()

fatal() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

path_exists() {
    [[ -e "$1" || -L "$1" ]]
}

require_interactive() {
    ((assume_yes)) && return 0
    [[ -t 0 && -t 1 ]] || fatal "interactive terminal required for $1"
}

confirm_yes() {
    local prompt="$1"
    local answer

    ((assume_yes)) && return 0
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

user_unit_active() {
    local state
    state="$(systemctl --user is-active "$legacy_system_name" 2>/dev/null || true)"
    case "$state" in
        active|activating|deactivating|reloading) return 0 ;;
        inactive|failed|dead|unknown|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

user_unit_enabled() {
    local state
    state="$(systemctl --user is-enabled "$legacy_system_name" 2>/dev/null || true)"
    case "$state" in
        enabled|enabled-runtime|linked|linked-runtime|alias) return 0 ;;
        disabled|static|indirect|generated|transient|masked|not-found) return 1 ;;
        *) return 2 ;;
    esac
}

require_target_user() {
    ((EUID != 0)) || fatal "run install.sh as an installation user, not root; it uses sudo internally"
    [[ "$user_name" != root ]] || fatal "served managers must not run as root"
    [[ "$user_name" =~ ^[A-Za-z_][A-Za-z0-9_.-]*$ ]] ||
        fatal "installation user name cannot be represented as a served systemd instance: $user_name"
    command -v getent >/dev/null 2>&1 || fatal "getent is required to resolve the installation user home"
    user_home="$(getent passwd "$user_name" | awk -F: 'NR == 1 { print $6; exit }')"
    [[ -n "$user_home" && "$user_home" = /* ]] ||
        fatal "installation user home from passwd is not an absolute path"
    [[ -d "$user_home" ]] || fatal "installation user home does not exist: $user_home"
    command -v sudo >/dev/null 2>&1 || fatal "sudo is required for system installation"
    legacy_user_binary="$user_home/.local/bin/served"
    legacy_user_unit="$user_home/.config/systemd/user/${legacy_system_name}"
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
    ((warned == 0)) || printf 'warning: custom XDG data is left in place; move it manually if needed\n' >&2
}

record_unit_state() {
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

inspect_installation() {
    local active_instance_output
    local legacy_user_owner
    local status

    path_exists "$binary_target" && had_binary=1
    path_exists "$template_target" && had_template=1
    path_exists "$legacy_system_target" && {
        had_legacy_system=1
        legacy_system_present=1
    }
    ((had_binary || had_template)) && shared_upgrade=1
    ((had_template == 0 && legacy_system_present == 0)) && fresh_integration=1

    record_unit_state "$instance_name" instance_active instance_enabled
    if ((legacy_system_present)); then
        legacy_user_owner="$(root_cmd awk "\$1 ~ /^User=/ { sub(/^User=/, \"\", \$1); print \$1; exit }" "$legacy_system_target" 2>/dev/null || true)"
        [[ -n "$legacy_user_owner" ]] ||
            fatal "existing ${legacy_system_target} has no User=; refusing an ownership-ambiguous migration"
        [[ "$legacy_user_owner" = "$user_name" ]] ||
            fatal "existing ${legacy_system_target} belongs to ${legacy_user_owner@Q}, not ${user_name@Q}"
        record_unit_state "$legacy_system_name" legacy_system_active legacy_system_enabled
    fi

    if path_exists "$legacy_user_binary" || path_exists "$legacy_user_unit"; then
        legacy_user_present=1
        if path_exists "$legacy_user_unit"; then
            if user_unit_active; then
                legacy_user_active=1
            else
                status=$?
                ((status == 1)) || fatal "old user service manager is unavailable"
            fi
            if user_unit_enabled; then
                legacy_user_enabled=1
            else
                status=$?
                ((status == 1)) || fatal "could not determine whether the old user service is enabled"
            fi
        fi
    fi

    ((instance_active + legacy_system_active + legacy_user_active <= 1)) ||
        fatal "more than one served manager is active for ${user_name}; stop the duplicate before migrating"

    target_active=$((instance_active || legacy_system_active || legacy_user_active))
    target_enabled=$((instance_enabled || legacy_system_enabled || legacy_user_enabled))
    if ((fresh_integration && legacy_user_present == 0)); then
        target_active=1
        target_enabled=1
    elif ((legacy_user_present && target_active == 0 && target_enabled == 0 && shared_upgrade == 0)); then
        target_active=1
        target_enabled=1
    fi

    active_instance_output="$(
        systemctl_root list-units --type=service --state=active 'served@*.service' \
            --no-legend --plain
    )" || fatal "could not list active served template instances"
    mapfile -t active_instances < <(awk 'NF { print $1 }' <<<"$active_instance_output")
}

backup_files() {
    backup_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-backup.XXXXXX")" ||
        fatal "could not create an upgrade backup directory"
    ((had_binary == 0)) || root_cmd cp -a -- "$binary_target" "$backup_dir/served"
    ((had_template == 0)) || root_cmd cp -a -- "$template_target" "$backup_dir/served@.service"
    ((had_legacy_system == 0)) || root_cmd cp -a -- "$legacy_system_target" "$backup_dir/served.service"
}

render_units() {
    staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/served-install.XXXXXX")" ||
        fatal "could not create a staging directory"
    sed 's|@SERVED_BIN@|/usr/local/bin/served|g' \
        "$script_dir/served@.service" > "$staging_dir/served@.service"
    if grep -q '@SERVED_BIN@' "$staging_dir/served@.service"; then
        fatal "served@.service contains an unresolved binary placeholder"
    fi
    if ((legacy_system_present)); then
        sed "s|%i|$user_name|g" "$staging_dir/served@.service" > "$staging_dir/served.service"
    fi
}

restore_files() {
    local failed=0
    root_cmd rm -f -- "$binary_target" "$template_target" "$legacy_system_target" || failed=1
    ((had_binary == 0)) || root_cmd cp -a -- "$backup_dir/served" "$binary_target" || failed=1
    ((had_template == 0)) || root_cmd cp -a -- "$backup_dir/served@.service" "$template_target" || failed=1
    ((had_legacy_system == 0)) || root_cmd cp -a -- "$backup_dir/served.service" "$legacy_system_target" || failed=1
    systemctl_root daemon-reload || failed=1
    return "$failed"
}

restore_system_unit_state() {
    local unit="$1"
    local was_active="$2"
    local was_enabled="$3"
    local failed=0
    local status

    if ((was_active == 0)); then
        if unit_active "$unit"; then
            systemctl_root stop "$unit" || failed=1
        else
            status=$?
            ((status == 1)) || failed=1
        fi
    fi
    if ((was_enabled == 0)); then
        if unit_enabled "$unit"; then
            systemctl_root disable "$unit" || failed=1
        else
            status=$?
            ((status == 1)) || failed=1
        fi
    fi
    if ((was_enabled)); then
        systemctl_root enable "$unit" || failed=1
    fi
    if ((was_active)); then
        systemctl_root start "$unit" || failed=1
    fi
    return "$failed"
}

rollback_states() {
    local failed=0

    restore_system_unit_state "$instance_name" "$instance_active" "$instance_enabled" || failed=1
    if ((legacy_system_present)); then
        restore_system_unit_state \
            "$legacy_system_name" "$legacy_system_active" "$legacy_system_enabled" || failed=1
    fi
    if ((legacy_user_enabled)); then
        systemctl --user enable "$legacy_system_name" || failed=1
    fi
    if ((legacy_user_active)); then
        systemctl --user start "$legacy_system_name" || failed=1
    fi
    return "$failed"
}

restore_active_instance_managers() {
    local unit
    local failed=0

    for unit in "${active_instances[@]}"; do
        [[ -n "$unit" ]] || continue
        if systemctl_root reload "$unit" && unit_active "$unit"; then
            continue
        fi
        if ! systemctl_root restart "$unit" || ! unit_active "$unit"; then
            printf 'warning: could not restore %s after rollback\n' "$unit" >&2
            failed=1
        fi
    done
    return "$failed"
}

abort_install() {
    local files_failed=0
    local managers_failed=0
    local states_failed=0

    printf 'error: %s\n' "$1" >&2
    restore_files || files_failed=1
    restore_active_instance_managers || managers_failed=1
    rollback_states || states_failed=1
    if ((files_failed == 0 && managers_failed == 0 && states_failed == 0)); then
        printf 'previous installation restored\n' >&2
    else
        printf 'warning: installation rollback was incomplete\n' >&2
    fi
    exit 1
}

install_files() {
    root_cmd install -Dm755 "$script_dir/served" "$binary_target"
    root_cmd install -Dm644 "$staging_dir/served@.service" "$template_target"
    if ((legacy_system_present)); then
        root_cmd install -Dm644 "$staging_dir/served.service" "$legacy_system_target"
    fi
    systemctl_root daemon-reload
}

reload_active_instances() {
    local unit
    for unit in "${active_instances[@]}"; do
        [[ -n "$unit" ]] || continue
        if systemctl_root reload "$unit" && unit_active "$unit"; then
            printf '%s manager handed off with runners preserved\n' "$unit"
        else
            printf 'warning: %s handoff failed; performing a controlled restart\n' "$unit" >&2
            if ! systemctl_root restart "$unit" || ! unit_active "$unit"; then
                abort_install "${unit} failed after the binary upgrade"
            fi
        fi
    done
}

relinquish_legacy_system_manager() {
    local upgraded=0

    ((legacy_system_active)) || return 0
    if [[ -x "$backup_dir/served" ]]; then
        HOME="$user_home" "$backup_dir/served" daemon --handoff >/dev/null 2>&1 || true
    fi
    if HOME="$user_home" "$binary_target" list >/dev/null 2>&1; then
        upgraded=1
    fi
    if ((upgraded)) && HOME="$user_home" "$binary_target" daemon --relinquish; then
        handoff_preserved=1
        return 0
    fi

    printf 'warning: legacy manager transfer failed; performing a controlled stop\n' >&2
    systemctl_root stop "$legacy_system_name" || return 1
}

migrate_legacy_services() {
    if ((legacy_system_present)); then
        if ((legacy_system_enabled)); then
            systemctl_root disable "$legacy_system_name" || return 1
        fi
        relinquish_legacy_system_manager || return 1
    fi

    if ((legacy_user_present)); then
        if ((legacy_user_enabled)); then
            systemctl --user disable "$legacy_system_name" || return 1
        fi
        if ((legacy_user_active)); then
            systemctl --user stop "$legacy_system_name" || return 1
        fi
    fi
}

apply_target_state() {
    if ((target_enabled)); then
        systemctl_root enable "$instance_name" || return 1
    fi
    if ((target_active)); then
        systemctl_root start "$instance_name" || return 1
        unit_active "$instance_name" || return 1
    fi
}

remove_legacy_files() {
    if ((legacy_system_present)); then
        if ! root_cmd rm -f -- "$legacy_system_target"; then
            printf 'warning: could not remove old %s; it remains disabled\n' "$legacy_system_target" >&2
            return 0
        fi
        systemctl_root reset-failed "$legacy_system_name" >/dev/null 2>&1 || true
        systemctl_root daemon-reload ||
            printf 'warning: system daemon-reload failed after removing the old fixed unit\n' >&2
    fi
    if ((legacy_user_present)); then
        if ! rm -f -- "$legacy_user_unit" "$legacy_user_binary"; then
            printf 'warning: old user-service files could not be removed; they remain disabled\n' >&2
            return 0
        fi
        systemctl --user daemon-reload ||
            printf 'warning: old user manager daemon-reload failed after migration\n' >&2
    fi
}

cleanup() {
    [[ -z "$staging_dir" ]] || rm -rf -- "$staging_dir"
    [[ -z "$backup_dir" ]] || rm -rf -- "$backup_dir"
}
trap cleanup EXIT

case "${1:-}" in
    "") ;;
    --yes) assume_yes=1 ;;
    *) fatal "usage: install.sh [--yes]" ;;
esac

require_target_user
[[ -f "$script_dir/served" ]] || fatal "served binary is missing from the package"
[[ -f "$script_dir/served@.service" ]] || fatal "served@.service is missing from the package"
warn_about_custom_xdg

if [[ -d "$binary_target" || -d "$template_target" || -d "$legacy_system_target" ]]; then
    fatal "an installation target is a directory; refusing to replace it"
fi

inspect_installation
render_units
if ((had_binary && had_template && legacy_system_present == 0 && legacy_user_present == 0)) &&
    cmp -s "$script_dir/served" "$binary_target" &&
    cmp -s "$staging_dir/served@.service" "$template_target"; then
    printf 'served is already installed at %s\n' "$binary_target"
    if ((instance_active)); then
        printf '%s is active\n' "$instance_name"
    else
        printf '%s remains stopped; start it with: sudo systemctl start %s\n' \
            "$instance_name" "$instance_name"
    fi
    if ((instance_enabled == 0)); then
        printf 'enable it at boot with: sudo systemctl enable %s\n' "$instance_name"
    fi
    exit 0
fi
if ((shared_upgrade || legacy_system_present || legacy_user_present)); then
    require_interactive "served installation or migration"
    if confirm_yes "Install the shared served binary and systemd template for ${user_name}?"; then
        :
    else
        status=$?
        ((status == 2)) && fatal "could not read installation confirmation"
        printf 'installation canceled; no files changed\n'
        exit 0
    fi
fi

backup_files
install_files || abort_install "could not install the shared binary and systemd template"

if ((legacy_system_present || legacy_user_present)); then
    migrate_legacy_services || abort_install "could not stop the legacy service for migration"
fi
reload_active_instances

apply_target_state || abort_install "could not apply ${instance_name} state"
remove_legacy_files

if ((handoff_preserved)); then
    printf '%s adopted the existing runners without stopping managed services\n' "$instance_name"
elif ((target_active)); then
    printf '%s is active\n' "$instance_name"
else
    printf '%s remains stopped; start it with: sudo systemctl start %s\n' "$instance_name" "$instance_name"
fi
if ((target_enabled == 0)); then
    printf 'enable it at boot with: sudo systemctl enable %s\n' "$instance_name"
fi
printf 'served installed at %s\n' "$binary_target"
