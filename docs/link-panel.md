# Network controls

The built-in dock, instrument host, tile protocol and dock SDKs were retired in
0.6.0 on both Wayland and X11. Their earlier design is
available in Git history. They are no longer built or installed.

Omarchy supplies system panels and workspace indicators through its menu bar.
See [Omarchy mode](omarchy-mode.md) and the
[Agent Sessions plugin](../omarchy/plugins/chonkstep.agents/README.md) for the
current desktop integration. Separate applications can use the
[control socket](control-socket.md) or [Hyprland-compatible IPC](hyprland-ipc.md).
