#!/bin/sh
# Installs Sonora for your user only: no root needed, so it works on the
# Steam Deck's read-only system too. Run it again to update.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin_dir="$HOME/.local/bin"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
program="$bin_dir/sonora"

# ldconfig is often outside a user's PATH.
libraries=$( (ldconfig -p || /sbin/ldconfig -p || /usr/sbin/ldconfig -p) 2>/dev/null || true)
if [ -n "$libraries" ] && ! printf '%s' "$libraries" | grep -q 'libpulse\.so\.0'; then
    echo "Note: libpulse.so.0 wasn't found. Sonora needs PipeWire (with pipewire-pulse) or PulseAudio;"
    echo "most desktops have it. If Sonora doesn't start, install your distro's libpulse package."
fi

mkdir -p "$bin_dir" "$data_dir/applications" "$data_dir/icons/hicolor/256x256/apps"
# Replace rather than overwrite, so a running Sonora keeps working.
cp "$here/sonora" "$program.new"
chmod 755 "$program.new"
mv -f "$program.new" "$program"
cp "$here/sonora.png" "$data_dir/icons/hicolor/256x256/apps/sonora.png"
escaped=$(printf '%s' "$program" | sed 's/[\\&|]/\\&/g')
sed "s|@PROGRAM@|$escaped|" "$here/sonora.desktop" > "$data_dir/applications/sonora.desktop"
update-desktop-database "$data_dir/applications" >/dev/null 2>&1 || true
gtk-update-icon-cache -q "$data_dir/icons/hicolor" >/dev/null 2>&1 || true

echo "Sonora is installed. Open it from your apps menu, or run: $program"
