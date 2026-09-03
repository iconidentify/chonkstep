# Shims for Omarchy's Hyprland-bound commands

Four of Omarchy's scripts ask Hyprland a question and read "no answer"
as an answer. These are those four scripts, with the Hyprland-specific
part replaced and everything else kept verbatim.

```
bin/omarchy-launch-shell     supervises Quickshell; liveness by Wayland socket
bin/omarchy-restart-shell    kills and respawns it without hyprctl dispatch
bin/omarchy-launch-or-focus  finds and activates a window over wlr-foreign-toplevel
bin/omarchy-system-logout    closes the windows and ends the session through logind
install.sh                   symlinks them onto PATH, and takes them off again
```

Each file's header states the original mechanism, what it does under
chonkstep, and what was substituted — read that before changing one.
They are Omarchy's scripts with a patch applied, not rewrites, so a
rebase onto a newer Omarchy is a diff.

**Nothing under `/usr/share/omarchy` or `/usr/bin` is touched.** A shim
takes effect by being earlier on `PATH` than the command it stands in
for, and it is uninstalled by removing one symlink.

```sh
omarchy/shims/install.sh              # into ~/.local/bin
omarchy/shims/install.sh --list       # what is linked; what PATH resolves today
omarchy/shims/install.sh --uninstall
```

`omarchy-launch-or-focus` and `omarchy-system-logout` need
`chonk-toplevel` (`cargo build --release -p chonk-toplevel`); they find
it on `PATH`, under `target/release`, under `target/debug`, or at
`$CHONK_TOPLEVEL`.

**`docs/omarchy-integration.md` is the page to read**: which commands
work unshimmed, which do not, which routes a `~/.local/bin` install
actually covers (the login-shell PATH the mirrored Omarchy menu runs
under — not chonkstep's `[commands]`, and never `omarchy <subcommand>`),
the extra step the supervisor needs, and what stays Hyprland-only.

The compositor-agnostic version of each of these, prepared as a patch
set for Omarchy itself, is in `../upstream/`.
