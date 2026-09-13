# Agent Sessions for Omarchy

Live Codex, Claude Code, OpenCode, and configured T3 sessions in Omarchy's menu
bar. The badge counts connected sessions. The panel lists working, waiting,
idle, disconnected, and ended sessions, with project search and keyboard
navigation. Enter or a click opens a verified terminal, tmux pane, OpenCode
session, or T3 browser thread. Multiple attached terminals produce a chooser.

The existing [Chonk Agents broker and adapters](../../../examples/chonk-agents/README.md)
provide the session metadata. This plugin subscribes to that broker; it does
not start another agent or change provider permissions. Sessions must be
attached through those adapters to appear. Disconnected sessions remain visible
but cannot be opened until they reconnect. T3 requires an explicit connection.

Install from the Chonkstep checkout after installing the broker:

```sh
bash omarchy/plugins/chonkstep.agents/install.sh
```

The installer preserves the broker and hooks and removes only the old AI dock
tile registration. The broker's existing autostart command continues to work.
If editing QML changes an IPC signature, `omarchy restart shell` clears older
cached component instances; application windows remain open.

```sh
omarchy-shell chonkstep.agents open
omarchy-shell chonkstep.agents status
omarchy-shell chonkstep.agents openSession <broker-session-id>
python3 -m unittest discover -s omarchy/plugins/chonkstep.agents/tests -v
```

The panel uses Omarchy's colors, fonts, bar button and panel controls. Its
`SessionPanel.qml` is adapted from Omarchy's MIT-licensed KeyboardPanel; the
configured layer size keeps it aligned on older Chonkstep builds whose
fractional xdg-output geometry is incorrect. Current Chonkstep also fixes that
protocol report for all other bar panels.
