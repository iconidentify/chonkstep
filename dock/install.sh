#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
prefix="${PREFIX:-$HOME/.local}"
cargo build --release --locked -p chonk-dock -p chonk-netjoin -p chonk-btpair
for binary in chonk-dock chonk-netjoin chonk-btpair; do
    install -Dm755 "target/release/$binary" "$prefix/bin/$binary"
done
install -Dm755 dock/chonk-get "$prefix/bin/chonk-get"
install -Dm644 dock/README.md "$prefix/share/doc/chonkstep/dock/README.md"
cp -a dock/bindings dock/docs dock/examples "$prefix/share/doc/chonkstep/dock/"
install -Dm644 dock/chonk-dock.desktop "$prefix/share/applications/chonk-dock.desktop"
echo "Installed Chonk Dock in $prefix. Run chonk-dock to start it."
