# Shims for Omarchy's commands that do not work here

One left, and that is the news.

This directory used to hold four scripts, for four Omarchy commands
that read "no answer" from `hyprctl` as an answer and took a wrong
branch. Three of them are gone: chonkstep now answers Hyprland's IPC
(`docs/hyprland-ipc.md`, on by default), so `omarchy-launch-shell`,
`omarchy-launch-or-focus` and `omarchy-restart-shell` are **correct
unmodified** and a shim for them would only be a copy that can drift.
What was tested, and how, is in `docs/omarchy-integration.md`.

```
bin/omarchy-system-logout    ends the session through logind
install.sh                   symlinks it onto PATH, and takes it off again
```

The one that remains is not a compositor problem at all. `uwsm stop`
looks for a `wayland-wm@*.service` that a chonkstep session does not
have, prints "Compositor is not running." and exits **0**, so Omarchy's
Logout row shows its OSD, closes the windows, and leaves you logged in.
Answering Hyprland's IPC cannot help with that; logind can.

The file's header states the original mechanism, what it does under
chonkstep, and what was substituted — read that before changing it. It
is Omarchy's script with a patch applied, not a rewrite, so a rebase
onto a newer Omarchy is a diff. It needs nothing from this repository:
no `chonk-toplevel`, no control socket, no build.

**Nothing under `/usr/share/omarchy` or `/usr/bin` is touched.** A shim
takes effect by being earlier on `PATH` than the command it stands in
for, and it is uninstalled by removing one symlink.

```sh
omarchy/shims/install.sh              # into ~/.local/bin
omarchy/shims/install.sh --list       # what is linked; what PATH resolves today
omarchy/shims/install.sh --uninstall
```

**`docs/omarchy-integration.md` is the page to read**: which commands
work unshimmed (most of them, now), which routes a `~/.local/bin`
install actually covers (the login-shell PATH the mirrored Omarchy menu
runs under — not chonkstep's `[commands]`, and never
`omarchy <subcommand>`), and what stays Hyprland-only.

The compositor-agnostic version of this script, prepared as a patch for
Omarchy itself, is in `../upstream/`.
