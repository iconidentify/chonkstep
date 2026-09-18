#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/.."
packages=(-p chonk-dock -p chonk-dock-theme -p chonk-dock-proto -p chonk-dock-widget
    -p chonk-instruments -p chonk-dock-client -p chonk-dockclock -p chonk-shelf -p chonk-dockapp-torture)
cargo test --locked "${packages[@]}"
cargo clippy --locked "${packages[@]}" --all-targets --no-deps -- \
    -D warnings -D clippy::disallowed_methods -D clippy::disallowed_types -D clippy::undocumented_unsafe_blocks
RUSTDOCFLAGS='-D warnings -A rustdoc::private_intra_doc_links' \
    cargo doc --locked "${packages[@]}" --no-deps --document-private-items
python3 -B -m unittest discover -s dock/bindings/python/tests
python3 -B -m unittest discover -s dock/examples/chonk-switch/tests
python3 -B -m unittest discover -s dock/tests
(cd dock/bindings/go/chonkdock && go test ./...)
