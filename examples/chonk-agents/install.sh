#!/usr/bin/env bash
# Install the independent agent service; provider setup remains explicit.
set -euo pipefail
agent_source=$(cd -- "$(dirname -- "$0")" && pwd)
# Keep the established path so existing native provider hooks keep working.
agent_target="${XDG_DATA_HOME:-$HOME/.local/share}/chonkstep/dockapps/chonk-agents"
agent_state="${XDG_STATE_HOME:-$HOME/.local/state}/chonkstep/backups"
bash "$agent_source/build.sh"
mkdir -p "$agent_state" "$agent_target" "$HOME/.local/bin"
if [ -f "$agent_target/chonk-agents.py" ]; then
    agent_backup=$(mktemp -d "$agent_state/agent-service.XXXXXX")
    cp -a "$agent_target/." "$agent_backup/"
    printf 'Previous service saved to %s\n' "$agent_backup"
fi
for agent_file in "$agent_source"/*.py; do
    install -m644 "$agent_file" "$agent_target/"
done
install -m644 "$agent_source/README.md" "$agent_source/build.sh" "$agent_target/"
# Old installs contain the retired SDK and tile manifest. Their complete
# contents were backed up above before replacing the service.
if [ -n "${agent_backup:-}" ]; then
    rm -rf -- "$agent_target/chonkdock"
    rm -f -- "$agent_target/chonk-agents.dockapp"
fi
for agent_dir in adapters bin fonts; do
    if [ -d "$agent_source/$agent_dir" ]; then
        mkdir -p "$agent_target/$agent_dir"
        cp -a "$agent_source/$agent_dir/." "$agent_target/$agent_dir/"
    fi
done
chmod +x "$agent_target/chonk-agents.py"
if [ ! -e "$HOME/.local/bin/chonk-agents" ] && [ ! -L "$HOME/.local/bin/chonk-agents" ]; then
    ln -s "$agent_target/chonk-agents.py" "$HOME/.local/bin/chonk-agents"
fi
printf 'Service installed: %s\nNext: chonk-agents setup; then chonk-agents serve in your graphical session.\n' "$agent_target"
