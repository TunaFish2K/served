#!/usr/bin/env bash
set -euo pipefail

systemctl --user disable --now served.service || true
rm -f "$HOME/.config/systemd/user/served.service"
rm -f "$HOME/.local/bin/served"
systemctl --user daemon-reload || true

printf 'served binary and user unit removed; enabled service links were kept.\n'

