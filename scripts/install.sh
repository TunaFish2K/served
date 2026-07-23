#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
install -Dm755 "$script_dir/served" "$HOME/.local/bin/served"
install -Dm644 "$script_dir/served.service" \
  "$HOME/.config/systemd/user/served.service"

systemctl --user daemon-reload
systemctl --user enable --now served.service
loginctl enable-linger "$USER"

printf 'served installed at %s\n' "$HOME/.local/bin/served"

