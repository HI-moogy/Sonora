#!/bin/sh
# Builds Sonora for Linux and packs it as dist/sonora-<version>-linux-x86_64.tar.gz.
#
# The binary runs on systems with at least the glibc it's built against, so
# release builds come from the oldest supported base (Ubuntu 22.04, as the
# release workflow uses). Needs Rust and libpulse (the dev package, or just
# libpulse.so.0).
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -n 1)
target_dir="${CARGO_TARGET_DIR:-$root/target}"

cargo build --release --locked -p app

name="sonora-$version-linux-x86_64"
# Staged in a temporary folder, where file modes stick (they don't on a
# Windows drive under WSL).
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage="$work/$name"
mkdir -p "$stage" "$root/dist"
cp "$target_dir/release/sonora" "$stage/"
cp crates/app/assets/icon.png "$stage/sonora.png"
cp packaging/linux/sonora.desktop packaging/linux/install.sh packaging/linux/uninstall.sh "$stage/"
cp README.md CHANGELOG.md "$stage/" 2>/dev/null || cp README.md "$stage/"
chmod 644 "$stage"/*
chmod 755 "$stage/sonora" "$stage/install.sh" "$stage/uninstall.sh"
tar -C "$work" -czf "$root/dist/$name.tar.gz" "$name"
echo "$root/dist/$name.tar.gz"
