#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
package_root="${1:?usage: package-docs.sh PACKAGE_ROOT}"
install -d -m 755 "$package_root/share/man/man1" "$package_root/share/man/man5" \
    "$package_root/share/served/skills/served/references"
install -m 644 "$project_dir/docs/man/served.1" "$package_root/share/man/man1/served.1"
install -m 644 "$project_dir/docs/man/served.5" "$package_root/share/man/man5/served.5"
install -m 644 "$project_dir/skills/served/SKILL.md" "$package_root/share/served/skills/served/SKILL.md"
install -m 644 "$project_dir/skills/served/references/cli.md" "$project_dir/skills/served/references/config.md" \
    "$package_root/share/served/skills/served/references/"
install -m 644 "$project_dir/scripts/install-docs.sh" "$package_root/install-docs.sh"

for document in README.md README.zh-CN.md; do
    sed 's|(skills/served/SKILL.md)|(share/served/skills/served/SKILL.md)|g' \
        "$project_dir/$document" > "$package_root/$document"
    chmod 644 "$package_root/$document"
done
