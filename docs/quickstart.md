# Quickstart

ChonkStep is a performance-focused Wayland compositor and window manager for
Omarchy. It supplies native window decorations, desktop menus and window
navigation. Omarchy supplies the bar, launchers and system services. There is
no built-in dock or persistent desktop workspace switcher on either backend.

## Install

On an Arch/Omarchy development machine, from a source checkout:

```sh
./scripts/install.sh
```

The script installs dependencies, builds Wayland and X11 binaries, and installs
login-session integration. Published packages may precede the current source;
consult their release notes. Build prerequisites and options are documented
in the installer itself.

Add this to `~/.config/chonkstep/config.toml`:

```toml
desktop = "omarchy"
```

Select **chonkstep (uwsm)** at your next login. Keep Hyprland installed while
trying ChonkStep. The [compatibility reference](hyprland-ipc.md) lists supported
commands and remaining gaps; Hyprland compatibility is an ongoing target.

Installing a new binary takes effect at the next login. Restarting a Wayland
compositor closes its clients. Configuration and theme changes reload in place.

## Try a theme without changing your desktop

```sh
cargo build --release -p chonkstep-wayland -p chonk-shell --bins
./scripts/preview-modern.sh obsidian
```

The preview runs a nested Wayland compositor with a private profile. It accepts
`obsidian`, `washi` or `relay`; close its outer window to exit. Omarchy previews
use Bubblewrap to isolate the shell's configuration and state.

For your normal session, export the themes with `omarchy-export-themes` and
select one from Omarchy's theme picker. See [modern themes](modern-themes.md)
for export paths, appearance overrides and the native theme descriptor.

## Open and manage windows

In Omarchy mode, Super+Return opens your configured terminal and Super+Space
opens Omarchy's launcher. The live Hyprland configuration supplies supported
keybindings. Right-click the desktop for ChonkStep's menu; right-click a
titlebar for window actions.

Minimized windows remain in Alt-Tab and restore when selected. Escape cancels
the selection without restoring them. Overview is an on-demand workspace and
window navigator; Omarchy's bar owns persistent workspace indicators.

See the [keybinding card](keybindings.md), [Mac interaction](mac-mode.md)
and [Hyprland configuration](hyprland-config.md).

## Agent sessions in Omarchy

Install the independent service and plugin from the checkout:

```sh
bash examples/chonk-agents/install.sh
chonk-agents setup
bash omarchy/plugins/chonkstep.agents/install.sh
chonk-agents serve
```

Provider setup preserves unrelated hooks and creates backups. Start or resume
the provider CLI to load its adapter. The service must run in the graphical
session; append its absolute command to your existing autostart configuration
for future sessions. Read [the agent guide](../examples/chonk-agents/README.md)
for supported providers, lifecycle semantics and verified session navigation.

## Configuration and troubleshooting

- [Omarchy mode](omarchy-mode.md): defaults, overrides and shell integration.
- [Configuration reference](config.example.toml): commented options.
- [Theme appearance](appearance.md): light/dark behavior and application colors.
- [Performance](performance.md): measurements and reproducible workloads.

X11 remains available as a secondary backend. Both backends share window
policy, themes and menus. The retired dock SDK and example tiles are no longer
built or installed; their earlier implementation remains in Git history.
