#!/usr/bin/env bash
# Build the session hook helper and copy licensed fonts for the review app.
set -euo pipefail
cd "$(dirname "$0")"
font_root="../../crates/wm-theme/assets/fonts/modern"
if [ -d "$font_root" ]; then
    mkdir -p fonts
    cp "$font_root/IBMPlexMono-Regular.ttf" "$font_root/ibmplexmono-OFL.txt" \
        "$font_root/IBMPlexSans[wdth,wght].ttf" "$font_root/ibmplexsans-OFL.txt" fonts/
fi
if [ -f hook-helper/Cargo.toml ]; then
    cargo build --manifest-path hook-helper/Cargo.toml --release --locked
    mkdir -p bin
    install -m755 hook-helper/target/release/chonk-agent-hook bin/chonk-agent-hook
elif [ ! -x bin/chonk-agent-hook ]; then
    echo 'chonk-agents: the native hook helper is missing.' >&2
    exit 1
fi
if [ -f adapters/package-lock.json ]; then
    npm ci --prefix adapters --omit=optional --ignore-scripts --no-audit --no-fund
fi
chmod +x chonk-agents.py
