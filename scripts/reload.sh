#!/usr/bin/env bash
# Asks a running chonkstep to re-read ~/.config/chonkstep/config.toml
# and apply it to the live session — no logout, no re-exec, nothing
# closed. chonkstep polls for this marker file once per event-loop tick
# and applies the config the instant it sees it.
#
# What this applies: the theme, the UI scale, focus policy, window
# placement, edge resistance, the terminal font size, the keybindings
# themselves, the move/resize drag modifier, and the [decorations]
# overrides — which re-decide the chrome of every window already open,
# not just the next one to map.
#
# That list is the same one the bound `reload` action applies, and it
# has to stay that way: everything here travels through one path
# (SessionState), so the two routes cannot drift. They had — an edited
# decoration policy applied through this script and was silently
# skipped by the key.
#
# The difference from scripts/restart.sh is what it costs you. A reload
# keeps everything: your windows, their client connections, and your
# dockapps. A restart replaces the process image, which on the X11
# session costs nothing visible (windows survive via the X11 SaveSet)
# but on the Wayland session costs you every client — Wayland clients
# die with the socket they were connected to. So reach for this one
# unless you have actually rebuilt the binary, which is the one change
# a running process cannot apply to itself.
#
# Usage: edit the config, then run this. (Or bind the `reload` action to
# a key and never leave the keyboard — see docs/config.example.toml.)
set -eu
STATE_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$STATE_DIR"
touch "$STATE_DIR/reload"
echo "reload requested — chonkstep should apply the config within ~100ms"
