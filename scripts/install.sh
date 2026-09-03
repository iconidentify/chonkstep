#!/usr/bin/env bash
# Installs chonkstep on an Omarchy (or any Arch-based) system, both
# halves of it: the X11 session and the Wayland session, each a real
# login session selectable from a display manager. Nothing is copied
# out of this checkout: the session entries point back into the repo,
# so scripts/update.sh (git pull + rebuild + hot-restart) is the whole
# upgrade story.
#
# What this does:
#   1. Installs runtime dependencies with pacman (Xorg, foot, picom,
#      the theme fonts, the graphics, input, and seat libraries the
#      Wayland compositor builds and runs against, and the portal stack
#      for screen sharing) and a Rust toolchain if the system has none.
#   2. Builds the release binaries - chonkstep, chonkstep-wayland and
#      omarchy-export-themes.
#   3. Installs both session entries pointing at this checkout's
#      launcher scripts - /usr/share/xsessions/chonkstep.desktop
#      (scripts/xsession.sh) and
#      /usr/share/wayland-sessions/chonkstep.desktop
#      (scripts/wayland-session.sh) - so a login manager offers
#      chonkstep in either flavour, and a machine with no login manager
#      can start either one from a TTY.
#   4. Installs the portal backend map,
#      /usr/share/xdg-desktop-portal/chonkstep-portals.conf, which
#      routes screen sharing to xdg-desktop-portal-wlr.
#   5. Seeds ~/.config/chonkstep/config.toml from the fully commented
#      example, only if there is none.
#   6. Links the two user-facing tools into ~/.local/bin: chonk-get (the
#      dockapp installer) and omarchy-export-themes (chonkstep's themes
#      as Omarchy themes). Nothing under ~/.config/omarchy is touched;
#      the Omarchy bar widgets under omarchy/plugins/ are yours to link
#      or not (omarchy/README.md).
#
# Every step is safe to repeat: pacman is asked with --needed, the
# entries and links are rewritten in place, and an existing config is
# never overwritten.
#
# Usage: scripts/install.sh
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$(pwd)"

if ! command -v pacman >/dev/null 2>&1; then
    echo "This installer targets Omarchy/Arch (pacman not found)." >&2
    exit 1
fi

# A documented refusal, not a limitation to paper over: the session
# entries below carry this checkout's absolute path in Exec=, and SDDM
# hands that value to its stock session wrappers
# (/usr/share/sddm/scripts/{Xsession,wayland-session}), both of which
# end in an unquoted `exec $@`. A path containing whitespace
# word-splits there and the session dies before it starts - no
# .desktop-file quoting survives that wrapper, so no escaping here can
# fix it. Quotes and backslashes are refused for the same reason plus
# the .desktop value unescaping (\\s becomes a space) that would mangle
# them first. Clone to a plain path (e.g. ~/src/chonkstep) instead.
case "$repo" in
    *[[:space:]\"\\]*)
        echo "This checkout lives at '${repo}', and that path contains" >&2
        echo "whitespace, a quote, or a backslash. SDDM's session wrapper" >&2
        echo "word-splits the Exec path ('exec \$@'), so a session entry" >&2
        echo "pointing here can never start. Move or re-clone the checkout" >&2
        echo "to a path without those characters and re-run scripts/install.sh." >&2
        exit 1
        ;;
esac

echo "Installing dependencies (sudo)..."
# xorg-server/xinit/xauth: the X session itself (xauth is what startx
# needs on a machine with no display manager - stock Omarchy). foot:
# the terminal the root menu and alt+shift+return launch, the one the
# desktop themes end to end; it is a Wayland client, so on the X11
# session name your own with `terminal =` in the config. picom: the
# X11 compositing manager behind the themes' translucent terminals -
# the Wayland session needs no equivalent, because a compositor
# composites itself. wireplumber:
# wpctl, which the dock's sound instrument reads and controls (already
# present on any PipeWire desktop; harmless elsewhere - without a sink
# the instrument shows its dead-screen face). Fonts: DejaVu (the
# chiseled window chrome), gsfonts/Nimbus Sans (the NeXT Lavender
# theme), JetBrains Mono Nerd (terminal), Noto (fallback coverage).
# The link instrument needs nothing extra: it prefers nmcli when the
# system has NetworkManager and falls back to /sys/class/net on
# anything else (Omarchy's iwd setup included).
#
# The last two lines are the Wayland half's build and runtime needs,
# and they are not optional even for an X11-only user: the workspace
# build below compiles wm-wayland on any Linux host, and that links
# against every one of them.
#
# Nested and session backends alike: libxkbcommon (keyboard layouts),
# libglvnd/mesa (EGL/GLES for the compositor's renderer, and libgbm for
# the session backend's buffer allocation), xorg-xwayland (the Xwayland
# binary the compositor spawns so X11 apps run in a Wayland session).
#
# The session backend on top of that - what turns the compositor from a
# window on someone else's desktop into a login session that owns the
# machine: libdrm (mode setting on the graphics device), libinput (real
# keyboards, mice, and touchpads), systemd-libs (libudev, which is how
# those devices are discovered and hot-plugged), and seatd (libseat,
# which is how the compositor opens the DRM and input devices without
# being root and hands them back across VT switches). Package names
# checked with `pacman -Qo /usr/lib/libseat.so` and friends rather than
# guessed - libseat lives in seatd, and libudev in systemd-libs, on
# Arch. The seatd *daemon* is not needed on a logind system, which is
# every systemd machine including stock Omarchy; the closing notes
# below say so only when logind is actually absent.
#
# All of these are already present on essentially any graphical Arch
# system; --needed makes listing them free.
# The portal stack (last line): what a browser's "share your screen"
# talks to. xdg-desktop-portal is the D-Bus frontend, xdg-desktop-
# portal-wlr the ScreenCast/Screenshot backend that speaks the
# zwlr_screencopy protocol chonkstep-wayland advertises, xdg-desktop-
# portal-gtk the fallback for everything else (file chooser and
# friends), and pipewire carries the frames from backend to browser.
# See docs/screen-sharing.md.
# JetBrains Mono Nerd comes in two conflicting packages and stock
# Omarchy ships the -basic variant; demanding the full one makes
# pacman refuse the whole transaction on exactly the machines this
# installer targets. Either variant carries the family the terminal
# theme names, so ask for the full package only when neither is
# already installed.
jb_font="ttf-jetbrains-mono-nerd"
pacman -Qq | grep -qx 'ttf-jetbrains-mono-nerd\(-basic\)\?' && jb_font=""
sudo pacman -S --needed --noconfirm \
    xorg-server xorg-xinit xorg-xauth \
    foot picom wireplumber \
    ttf-dejavu gsfonts $jb_font noto-fonts \
    libxkbcommon libglvnd mesa xorg-xwayland \
    libdrm libinput systemd-libs seatd \
    pipewire xdg-desktop-portal xdg-desktop-portal-wlr xdg-desktop-portal-gtk

if ! command -v cargo >/dev/null 2>&1; then
    echo "Installing Rust toolchain..."
    sudo pacman -S --needed --noconfirm rustup
    rustup default stable
elif command -v rustup >/dev/null 2>&1 && ! rustup show active-toolchain >/dev/null 2>&1; then
    rustup default stable
fi

echo "Building chonkstep (release)..."
cargo build --release --workspace

echo "Installing session entries (sudo)..."
# Written to a scratch file first and placed with `install -m644`, not
# piped through `sudo tee`: tee creates the file with this shell's
# umask, and a hardened umask (077) would leave the entry 0600 root -
# unreadable by the sddm greeter user, whose session list then shows a
# blank row or nothing at all. install's explicit mode is immune.
#
# Deliberately NO TryExec in either entry, and this is load-bearing:
# SDDM's greeter runs as the sddm user and stats an absolute TryExec,
# hiding the session when the stat fails - and a default 0700 home
# (login.defs HOME_MODE on Arch) fails exactly that stat for a script
# under ~. Exec is never checked at listing time, and at launch the
# session runs as the logging-in user, who can of course read their
# own home. The Arch package's entries DO carry TryExec, because
# /usr/lib/chonkstep is world-traversable by construction.
entry_tmp="$(mktemp)"
trap 'rm -f "$entry_tmp"' EXIT
cat > "$entry_tmp" <<DESKTOP
[Desktop Entry]
Name=chonkstep
Comment=A NeXTSTEP-style window manager with chiseled chrome
Exec=${repo}/scripts/xsession.sh
Type=Application
DESKTOP
sudo install -Dm644 "$entry_tmp" /usr/share/xsessions/chonkstep.desktop

# The Wayland twin. Display managers read this directory for sessions
# they must start on a bare VT (no Xorg first), which is exactly what
# the compositor's session backend wants: it opens the DRM device and
# the input devices itself through libseat. The name carries the
# "(Wayland)" suffix because both entries land in the same picker and
# "chonkstep" twice would be a coin flip for the user.
# DesktopNames is what a display manager exports as XDG_CURRENT_DESKTOP
# for the session — the key xdg-desktop-portal matches against
# chonkstep-portals.conf (installed below) to pick the ScreenCast
# backend. The session script also exports it, for TTY logins that
# never see this file; both spell it the same.
cat > "$entry_tmp" <<DESKTOP
[Desktop Entry]
Name=chonkstep (Wayland)
Comment=The chonkstep desktop as a native Wayland compositor
Exec=${repo}/scripts/wayland-session.sh
DesktopNames=chonkstep
Type=Application
DESKTOP
sudo install -Dm644 "$entry_tmp" /usr/share/wayland-sessions/chonkstep.desktop

# The portal backend map: ScreenCast/Screenshot to the wlr backend
# (screen sharing — see docs/screen-sharing.md), the rest to GTK. The
# file is matched by the XDG_CURRENT_DESKTOP value set above, and a
# user can override it in ~/.config/xdg-desktop-portal/.
sudo install -Dm644 packaging/portal/chonkstep-portals.conf \
    /usr/share/xdg-desktop-portal/chonkstep-portals.conf

# Seed the user's config from the fully-commented example, so tuning
# scale/keybindings starts from a documented template instead of a
# search through the repo. Only when absent - never overwrite a real
# config on reinstall/update.
config="${XDG_CONFIG_HOME:-$HOME/.config}/chonkstep/config.toml"
if [ ! -e "$config" ]; then
    install -Dm644 docs/config.example.toml "$config"
    echo "Seeded ${config} (all defaults, fully commented)."
fi

# The two tools a user reaches for by name, put on PATH the same way
# the session entries were: as links back into this checkout, so
# scripts/update.sh keeps them current with nothing else to do.
# chonk-get is a script and lives in scripts/; omarchy-export-themes is
# a release binary beside the session ones. ln -sfn makes a rerun (or
# a moved checkout, after re-running this script) rewrite the links
# rather than fail on them. ~/.local/bin is the XDG-blessed spot and
# is on PATH on stock Omarchy; the note below covers a shell where it
# is not, since a link nobody can reach is worse than none.
bin="$HOME/.local/bin"
install -d "$bin"
ln -sfn "${repo}/scripts/chonk-get" "$bin/chonk-get"
ln -sfn "${repo}/target/release/omarchy-export-themes" "$bin/omarchy-export-themes"
# chonk-netjoin is the odd one out here: nobody types it. It is the
# wifi passphrase dialog the LNK panel spawns when someone clicks a
# secured network it has no saved profile for, and the dock launches it
# by bare name through the effect runner - which resolves it on PATH.
# So it is linked for the machine's benefit rather than the user's, and
# without the link the panel's "JOIN..." row silently does nothing.
ln -sfn "${repo}/target/release/chonk-netjoin" "$bin/chonk-netjoin"
bin_on_path=""
case ":${PATH}:" in
    *":${bin}:"*) bin_on_path="yes" ;;
esac

# The Tailscale operator grant, offered once and never taken silently.
#
# Reading the tailnet needs no privilege, so the LNK panel can always
# show it. Moving it is different: tailscaled's IPN bus is
# root-or-operator, and a `tailscale up`/`down` from the panel comes
# back "Access denied: ... must be root or Operator" unless this user
# holds the operator role. The panel handles that honestly - the row
# locks and shows the remedy rather than pretending the toggle worked
# (docs/link-panel.md) - but an honest dead toggle is still a dead
# toggle, and the fix is one command that has to be run as root.
#
# So: explain it, ask, and default to no. This grants THIS user the
# power to change the machine's VPN state with no further
# authentication, which is a real privilege decision and not one an
# installer should make on someone's behalf while they look away.
#
# The test is "can this user talk to the socket", not "is the operator
# preference set", because the first is the question that actually
# decides whether the toggle works: tailscaled hands the operator its
# control socket, but a site that has widened the socket's permissions
# some other way is equally able to toggle, and nagging it to run a
# command it does not need would be wrong.
ts_sock="/var/run/tailscale/tailscaled.sock"
if command -v tailscale >/dev/null 2>&1 && [ -S "$ts_sock" ] && [ ! -w "$ts_sock" ]; then
    cat <<TAILSCALE

Tailscale is installed here, and the dock's LNK panel can show your
tailnet - but not toggle it. Changing Tailscale's state needs root or
the "operator" role, which this account does not have, so the panel
will draw that row locked with the command that would unlock it.

Granting it runs:

    sudo tailscale set --operator=${USER}

after which ${USER} can bring Tailscale up and down - from the panel or
the command line - without a password. That is a genuine privilege;
say no if you would rather use sudo each time.

TAILSCALE
    # Only ask where there is somebody to answer. A piped or CI install
    # has no terminal, and the safe default for an unattended run is
    # the one that changes nothing.
    if [ -t 0 ] && [ -t 1 ]; then
        printf 'Grant the operator role to %s now? [y/N] ' "$USER"
        read -r ts_reply || ts_reply=""
        case "$ts_reply" in
            [yY] | [yY][eE][sS])
                if sudo tailscale set --operator="$USER"; then
                    echo "Granted. The panel's Tailscale row is now a working toggle."
                else
                    echo "That did not take; the panel will keep showing the remedy." >&2
                fi
                ;;
            *) echo "Left alone. Run the command above whenever you want it." ;;
        esac
    else
        echo "(Not a terminal, so nothing was asked or changed.)"
    fi
    echo
fi

# How this machine logs in decides which instructions are honest.
# Three shapes in the wild:
#   - a display manager with a session picker: log out, pick chonkstep;
#   - Omarchy 4: SDDM is enabled, but its stock greeter theme
#     (Theme Current=omarchy) draws NO session picker - it takes a
#     password and hardwires the login to the Hyprland (uwsm) session
#     (see its Main.qml: sessionIndex searches the list for "uwsm").
#     chonkstep can be perfectly registered and never appear; those
#     users need the autologin route or a theme with a picker;
#   - no display manager at all (Omarchy <= 3 and minimal Arch): the
#     TTY routes.
has_dm=""
for dm in sddm gdm lightdm greetd lemurs ly; do
    if systemctl is-enabled "$dm" >/dev/null 2>&1; then
        has_dm="$dm"
        break
    fi
done
# SDDM's config: conf.d sorted then sddm.conf, last assignment wins -
# close enough for a diagnostic.
sddm_theme=""
if [ "$has_dm" = "sddm" ]; then
    sddm_theme="$(cat /etc/sddm.conf.d/*.conf /etc/sddm.conf 2>/dev/null | sed -n 's/^Current=//p' | tail -n 1)"
fi

# Whether this machine has logind, which decides whether the Wayland
# session needs any seat setup at all. libseat prefers logind and falls
# back to the seatd daemon; on a systemd machine (every Omarchy, and
# nearly every Arch desktop) logind is already granting device access
# to whoever owns the active VT, so nothing else is required and
# telling the user to enable seatd would be noise that invites them to
# break a working setup. /run/systemd/seats is the direct evidence -
# logind creates it - and an answering loginctl is the fallback for a
# layout that differs.
has_logind=""
if [ -d /run/systemd/seats ] ||
    { command -v loginctl >/dev/null 2>&1 && loginctl show-seat seat0 >/dev/null 2>&1; }; then
    has_logind="yes"
fi

cat <<DONE

chonkstep is installed - both binaries: chonkstep (X11) and
chonkstep-wayland (the Smithay compositor) - and both are real login
sessions, running the same desktop.

DONE
if [ "$sddm_theme" = "omarchy" ]; then
    cat <<DONE
  - This is Omarchy: SDDM is the login manager, but its stock greeter
    theme has no session picker - it logs every interactive login into
    Hyprland, so chonkstep will NOT appear at the login screen even
    though both entries are installed. Two ways in:
      1. Make chonkstep the boot session (autologin):
           printf '[Autologin]\nUser=$USER\nSession=chonkstep.desktop\n' |
             sudo tee /etc/sddm.conf.d/20-chonkstep-session.conf
         Session=chonkstep.desktop resolves to the Wayland session
         (SDDM searches wayland-sessions first). Remove the file to go
         back to Hyprland.
      2. Switch the greeter to a theme with a session menu (SDDM ships
         elarun, maldives, maya): edit /etc/sddm.conf.d/10-theme.conf
         and set Current= to one of them, then pick "chonkstep
         (Wayland)" from its session menu at login.
DONE
elif [ -n "$has_dm" ]; then
    cat <<DONE
  - X11 session: log out and pick "chonkstep" in ${has_dm}'s session list.
  - Wayland session: log out and pick "chonkstep (Wayland)" in the same
    list. It takes the graphics device, the input devices, and the VT
    for itself - a session in its own right, not a window inside one.
DONE
else
    cat <<DONE
  - X11 session: no display manager is enabled (Omarchy 3 and earlier
    boot straight into Hyprland), so switch to a TTY (Ctrl+Alt+F3),
    log in, and run:
      startx ${repo}/scripts/xsession.sh
  - Wayland session: from that same TTY, instead run:
      exec ${repo}/scripts/wayland-session.sh
    (exec, so the compositor replaces the login shell and the session
    ends when it does. No startx: it is the display server.)
    To get a graphical session picker offering both instead, install
    and enable a display manager (e.g. sddm) - both session entries are
    already in place.
DONE
fi
cat <<DONE
  - Wayland, nested: for development or a look before you log in, run
      ${repo}/target/release/chonkstep-wayland
    from a terminal inside your current desktop. The compositor sees a
    desktop already running and opens a window that is its screen -
    same chrome, dock, menus, and themes, with X11 apps through
    XWayland.
DONE
if [ -z "$has_logind" ]; then
    cat <<DONE
  - Seat access: this machine has no logind, so the Wayland session
    needs the seatd daemon (installed above) to hand it the graphics
    and input devices. Enable it -
      sudo systemctl enable --now seatd
    or your init system's equivalent - and join the seat group:
      sudo usermod -aG seat \$USER
    Log out and back in afterwards for the group to take effect.
DONE
fi
cat <<DONE
  - HiDPI: set "scale = 2.0" in ${config}
    (the whole file is optional and every line is documented; both
    backends read it).
  - On PATH, as links into this checkout: chonk-get (install a dockapp:
    chonk-get install examples/chonk-shelf) and omarchy-export-themes
    (write chonkstep's themes where omarchy-theme-set can find them).
DONE
if [ -z "$bin_on_path" ]; then
    cat <<DONE
    Note: ${bin} is not on this shell's PATH, so reach them by that
    path or add it (Omarchy's stock bashrc already does).
DONE
fi
cat <<DONE
  - Update later with: scripts/update.sh
  - Both session entries and both links point at this checkout
    (${repo}); moving the checkout means re-running scripts/install.sh.
  - Not showing up at the login screen? scripts/verify-install.sh
    checks everything mechanical (entries, permissions, Exec targets)
    and diagnoses the environment; the "Log in" section of
    docs/quickstart.md covers the rest.

DONE
