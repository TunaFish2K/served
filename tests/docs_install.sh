#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/served-docs-test.XXXXXX")"
trap 'rm -rf "$test_root"' EXIT
package="$test_root/package"
prefix="$test_root/prefix"
fail_target=""
root_cmd() {
    if [[ "$1" == install && -n "$fail_target" && "${*: -1}" == "$fail_target" ]]; then
        return 1
    fi
    "$@"
}
"$project_dir/scripts/package-docs.sh" "$package"
# shellcheck source=scripts/install-docs.sh
source "$project_dir/scripts/install-docs.sh"
served_docs_init "$package" "$prefix"
served_docs_validate
served_docs_backup "$test_root/fresh-backup"
served_docs_install
served_docs_current
if command -v man >/dev/null 2>&1; then
    man -M "$prefix/share/man" -w 1 served >/dev/null
    man -M "$prefix/share/man" -w 5 served >/dev/null
fi
python3 - "$prefix" <<'PY'
from pathlib import Path
import sys
root = Path(sys.argv[1])
files = list(root.rglob('*'))
assert len([p for p in files if p.is_file()]) == 5
assert all(p.stat().st_mode & 0o777 == (0o755 if p.is_dir() else 0o644) for p in files)
PY
# Fresh-install rollback removes payload files that did not exist before.
served_docs_restore "$test_root/fresh-backup"
[[ ! -e "$prefix/share/man/man1/served.1" ]]
served_docs_install
# Reinstallation repairs a missing file, without any supervisor command.
rm "$prefix/share/served/skills/served/references/config.md"
served_docs_update_only
served_docs_current
# A late documentation failure restores both existing files and prior absences.
rm "$prefix/share/man/man5/served.5"
cp "$prefix/share/man/man1/served.1" "$test_root/original"
printf '\nUpdated source\n' >> "$package/share/man/man1/served.1"
fail_target="$prefix/share/served/skills/served/SKILL.md"
if served_docs_update_only; then
    printf 'error: injected failure was ignored\n' >&2
    exit 1
fi
cmp "$test_root/original" "$prefix/share/man/man1/served.1"
[[ ! -e "$prefix/share/man/man5/served.5" ]]
fail_target=""
served_docs_update_only
served_docs_current
# Only owned files are removed, even when another tool shares the hierarchy.
printf 'keep\n' > "$prefix/share/man/man1/other.1"
printf 'keep\n' > "$prefix/share/served/skills/served/local-notes"
served_docs_remove
[[ -f "$prefix/share/man/man1/other.1" ]]
[[ -f "$prefix/share/served/skills/served/local-notes" ]]
[[ ! -e "$prefix/share/served/skills/served/SKILL.md" ]]
# Standalone archive contains a complete portable skill and is reproducible.
python3 "$project_dir/scripts/package-skill.py" "$test_root/first" >/dev/null
python3 "$project_dir/scripts/package-skill.py" "$test_root/second" >/dev/null
python3 - "$test_root" <<'PY'
from pathlib import Path
import hashlib
import sys
import tarfile
root = Path(sys.argv[1])
first = next((root / 'first').glob('*.tar.gz'))
second = next((root / 'second').glob('*.tar.gz'))
assert first.read_bytes() == second.read_bytes()
assert first.with_name(first.name + '.sha256').read_text().split()[0] == hashlib.sha256(first.read_bytes()).hexdigest()
with tarfile.open(first) as tar:
    assert set(tar.getnames()) == {'served/SKILL.md', 'served/references/cli.md', 'served/references/config.md'}
    assert all(member.mode == 0o644 for member in tar.getmembers())
PY
printf 'documentation packaging, installation, repair, rollback, and removal passed\n'
