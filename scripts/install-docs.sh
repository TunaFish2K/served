#!/usr/bin/env bash
# Shared by the Linux/macOS lifecycle scripts. root_cmd is supplied by the caller.

served_docs_init() {
    served_docs_source="$1/share"
    served_docs_target="$2/share"
    served_docs_files=(
        man/man1/served.1
        man/man5/served.5
        served/skills/served/SKILL.md
        served/skills/served/references/cli.md
        served/skills/served/references/config.md
    )
}

served_docs_validate() {
    local relative
    for relative in "${served_docs_files[@]}"; do
        [[ -f "$served_docs_source/$relative" ]] || {
            printf 'error: documentation missing from package: %s\n' "$relative" >&2
            return 1
        }
        if [[ -L "$served_docs_target/$relative" || -d "$served_docs_target/$relative" ]]; then
            printf 'error: documentation target is a symlink or directory: %s\n' "$relative" >&2
            return 1
        fi
    done
}

served_docs_current() {
    local relative
    for relative in "${served_docs_files[@]}"; do
        cmp -s "$served_docs_source/$relative" "$served_docs_target/$relative" || return 1
    done
}

served_docs_backup() {
    local destination="$1" relative
    mkdir -p "$destination" || return 1
    for relative in "${served_docs_files[@]}"; do
        if [[ -f "$served_docs_target/$relative" ]]; then
            mkdir -p "$destination/$(dirname "$relative")" || return 1
            root_cmd cp -p "$served_docs_target/$relative" "$destination/$relative" || return 1
        fi
    done
}

served_docs_install() {
    local relative
    for relative in "${served_docs_files[@]}"; do
        root_cmd install -d -m 755 "$served_docs_target/$(dirname "$relative")" || return 1
        root_cmd install -m 644 "$served_docs_source/$relative" "$served_docs_target/$relative" || return 1
    done
}

served_docs_restore() {
    local backup="$1" relative failed=0
    for relative in "${served_docs_files[@]}"; do
        if [[ -f "$backup/$relative" ]]; then
            root_cmd cp -p "$backup/$relative" "$served_docs_target/$relative" || failed=1
        else
            root_cmd rm -f "$served_docs_target/$relative" || failed=1
        fi
    done
    return "$failed"
}

served_docs_update_only() {
    local backup
    served_docs_current && return 0
    backup="$(mktemp -d "${TMPDIR:-/tmp}/served-docs-backup.XXXXXX")" || return 1
    if ! served_docs_backup "$backup"; then
        root_cmd rm -rf "$backup"
        return 1
    fi
    if ! served_docs_install; then
        served_docs_restore "$backup" || {
            printf 'error: documentation rollback incomplete; backup retained at %s\n' "$backup" >&2
            return 1
        }
        root_cmd rm -rf "$backup"
        return 1
    fi
    root_cmd rm -rf "$backup"
    printf 'served manuals and AI skill updated\n'
}

served_docs_remove() {
    local relative failed=0
    for relative in "${served_docs_files[@]}"; do
        root_cmd rm -f "$served_docs_target/$relative" || failed=1
    done
    # Remove only empty served-owned directories; keep the shared man hierarchy.
    for relative in served/skills/served/references served/skills/served served/skills served; do
        root_cmd rmdir "$served_docs_target/$relative" 2>/dev/null || true
    done
    return "$failed"
}
