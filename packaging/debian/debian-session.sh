#!/bin/sh
# The Debian/LCOS session entry point: give the desktop a terminal it
# can actually launch, then hand off to the shared session launcher.
#
# chonkstep's built-in default terminal is foot, which is a Wayland
# client. On an X11-only system -- which is every system this package
# targets -- the root menu's Terminal item and alt+shift+return do
# nothing at all: foot is not installed, and could not run against an X
# server if it were. The session log records it as
#
#   WARN chonk_shell::spawn: failed to launch program="foot"
#
# and the user sees a menu item that silently does nothing. The
# project's own guidance (scripts/install.sh, docs/config.example.toml)
# is to name your own terminal with `terminal =` in the config; there is
# no system-wide config file a package could put that in, because
# wm-config resolves exactly one path -- $XDG_CONFIG_HOME/chonkstep/
# config.toml. So the package seeds the user's own, once.
#
# `x-terminal-emulator` rather than a named terminal: it is Debian's
# alternatives symlink, so the desktop launches whatever this machine
# already considers its terminal -- cool-retro-term on LCOS, xterm on a
# plain Debian -- and follows the admin if they change it. Naming a
# terminal here would override a choice the system had already made.
set -u

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/chonkstep"
CONFIG="$CONFIG_DIR/config.toml"
EXAMPLE=/usr/share/doc/chonkstep/config.example.toml

# Only when there is no config at all. An existing config is the user's,
# and is never edited -- if they have chosen a terminal, or deliberately
# left the default, that decision stands.
if [ ! -e "$CONFIG" ] && mkdir -p "$CONFIG_DIR" 2>/dev/null; then
    # Prepended, not appended: the example ends inside a [table], so a
    # bare key added after it would be parsed as a member of that table
    # rather than as a top-level setting. Everything above the first
    # table header is comments, so the top of the file is the one place
    # a top-level key can safely go.
    {
        printf '# Set by the Debian package (see /usr/lib/chonkstep/debian-session.sh).\n'
        printf '# The built-in default terminal is a Wayland client and cannot run here.\n'
        printf 'terminal = "x-terminal-emulator"\n\n'
        [ -r "$EXAMPLE" ] && cat "$EXAMPLE"
    } > "$CONFIG" 2>/dev/null || true
fi

exec /usr/lib/chonkstep/xsession.sh "$@"
