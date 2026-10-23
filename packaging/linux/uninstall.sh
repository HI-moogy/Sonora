#!/bin/sh
# Removes what install.sh added, and Sonora's start-at-login entry.
# Your settings stay in ~/.config/sonora; delete that folder to remove them.
set -eu

data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}"

pkill -x sonora 2>/dev/null && sleep 1 || true
rm -f "$HOME/.local/bin/sonora" \
    "$data_dir/applications/sonora.desktop" \
    "$data_dir/icons/hicolor/256x256/apps/sonora.png" \
    "$config_dir/autostart/sonora.desktop"
update-desktop-database "$data_dir/applications" >/dev/null 2>&1 || true

echo "Sonora is uninstalled. Your settings are still in $config_dir/sonora."
