#!/usr/bin/env bash
# Install the menu bar frontend while retaining the existing broker/adapters.
set -euo pipefail
plugin_source=$(cd -- "$(dirname -- "$0")" && pwd)
plugin_config="${XDG_CONFIG_HOME:-$HOME/.config}"
plugin_state="${XDG_STATE_HOME:-$HOME/.local/state}"
plugin_target="$plugin_config/omarchy/plugins/chonkstep.agents"
mkdir -p "$plugin_state/chonkstep/backups"
plugin_backup=$(mktemp -d "$plugin_state/chonkstep/backups/agent-bar.XXXXXX")
if [ -d "$plugin_target" ]; then cp -a "$plugin_target" "$plugin_backup/plugin"; fi
if [ -f "$plugin_config/omarchy/shell.json" ]; then
    cp -a "$plugin_config/omarchy/shell.json" "$plugin_backup/shell.json"
fi
mkdir -p "$plugin_target"
for plugin_file in manifest.json Widget.qml SessionPanel.qml bridge.py README.md; do
    install -m644 "$plugin_source/$plugin_file" "$plugin_target/$plugin_file"
done
# Removing only the registration keeps provider hooks, session history,
# dependencies, and the independently autostarted service intact.
if [ -f "$plugin_config/chonkstep/dockapps/chonk-agents.dockapp" ]; then
    mv "$plugin_config/chonkstep/dockapps/chonk-agents.dockapp" "$plugin_backup/chonk-agents.dockapp"
fi
omarchy-shell shell rescanPlugins
omarchy plugin enable chonkstep.agents --section right --before omarchy.agents
printf 'Agent Sessions installed. Backup: %s\n' "$plugin_backup"
