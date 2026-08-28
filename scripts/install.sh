#!/usr/bin/env bash
# Installs chonkstep on an Omarchy (or any Arch-based) system, both
# halves of it: the X11 session (a selectable session alongside the
# existing desktop) and the Wayland compositor (nested, for now - see
# the closing notes this prints). Nothing is copied out of this
# checkout: the session entry points back into the repo, so
# scripts/update.sh (git pull + rebuild + hot-restart) is the whole
# upgrade story.
#
# What this does:
#   1. Installs runtime dependencies with pacman (Xorg, the terminal,
#      the session compositor, the theme fonts, the Wayland stack the
#      compositor builds and runs against) and a Rust toolchain if the
#      system has none.
#   2. Builds the release binaries - chonkstep and chonkstep-wayland.
#   3. Installs /usr/share/xsessions/chonkstep.desktop pointing at this
#      checkout's scripts/xsession.sh, so chonkstep appears in the
#      login manager's session picker. No wayland-sessions entry: the
#      compositor has no DRM/KMS backend yet, so a login entry for it
#      would fail the moment it was chosen.
#
# Usage: scripts/install.sh
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$(pwd)"

if ! command -v pacman >/dev/null 2>&1; then
    echo "This installer targets Omarchy/Arch (pacman not found)." >&2
    exit 1
fi

echo "Installing dependencies (sudo)..."
# xorg-server/xinit/xauth: the X session itself (xauth is what startx
# needs on a machine with no display manager - stock Omarchy). rxvt-
# unicode: the terminal the root menu launches. picom: the session
# compositor behind the themes' translucent terminals. wireplumber:
# wpctl, which the dock's sound instrument reads and controls (already
# present on any PipeWire desktop; harmless elsewhere - without a sink
# the instrument shows its dead-screen face). Fonts: DejaVu
# (WindowMaker-parity chrome), gsfonts/Nimbus Sans (the NeXT Lavender
# theme), JetBrains Mono Nerd (terminal), Noto (fallback coverage).
# The link instrument needs nothing extra: it prefers nmcli when the
# system has NetworkManager and falls back to /sys/class/net on
# anything else (Omarchy's iwd setup included).
#
# The last line is the Wayland half's build and runtime needs, and it
# is not optional even for an X11-only user: the workspace build below
# compiles wm-wayland on any Linux host, and that links against
# libxkbcommon and EGL. libxkbcommon (keyboard layouts), libglvnd/mesa
# (EGL/GLES for the compositor's renderer), xorg-xwayland (the
# Xwayland binary the compositor spawns so X11 apps run in a Wayland
# session). All four are already present on essentially any graphical
# Arch system; --needed makes listing them free.
sudo pacman -S --needed --noconfirm \
    xorg-server xorg-xinit xorg-xauth \
    rxvt-unicode picom wireplumber \
    ttf-dejavu gsfonts ttf-jetbrains-mono-nerd noto-fonts \
    libxkbcommon libglvnd mesa xorg-xwayland

if ! command -v cargo >/dev/null 2>&1; then
    echo "Installing Rust toolchain..."
    sudo pacman -S --needed --noconfirm rustup
    rustup default stable
elif command -v rustup >/dev/null 2>&1 && ! rustup show active-toolchain >/dev/null 2>&1; then
    rustup default stable
fi

echo "Building chonkstep (release)..."
cargo build --release --workspace

echo "Installing session entry (sudo)..."
sudo install -d /usr/share/xsessions
sudo tee /usr/share/xsessions/chonkstep.desktop >/dev/null <<DESKTOP
[Desktop Entry]
Name=chonkstep
Comment=A window manager with WindowMaker parity
Exec=${repo}/scripts/xsession.sh
Type=Application
DESKTOP

# Seed the user's config from the fully-commented example, so tuning
# scale/keybindings starts from a documented template instead of a
# search through the repo. Only when absent - never overwrite a real
# config on reinstall/update.
config="${XDG_CONFIG_HOME:-$HOME/.config}/chonkstep/config.toml"
if [ ! -e "$config" ]; then
    install -Dm644 docs/config.example.toml "$config"
    echo "Seeded ${config} (all defaults, fully commented)."
fi

# Stock Omarchy boots straight into Hyprland via autologin - there is
# no login-manager session picker for the xsessions entry to appear in,
# so point those users at startx; on a machine that does run a display
# manager, the session-list path is the smoother one.
has_dm=""
for dm in sddm gdm lightdm greetd lemurs ly; do
    if systemctl is-enabled "$dm" >/dev/null 2>&1; then
        has_dm="$dm"
        break
    fi
done

# Which display stack the user is on right now, so the advice below
# names the path they can actually take today. Deliberately NOT used to
# pick "the Wayland version" as their session: see the note printed
# below - the compositor has no DRM/KMS backend yet, so it cannot own
# a TTY the way Xorg does, and installing a wayland-sessions entry
# would put a session in the login picker that fails the moment it is
# chosen. That entry lands with the session feature, not before.
session_type="${XDG_SESSION_TYPE:-}"
if [ -z "$session_type" ] && [ -n "${WAYLAND_DISPLAY:-}" ]; then
    session_type="wayland"
fi

cat <<DONE

chonkstep is installed - both binaries: chonkstep (X11) and
chonkstep-wayland (the Smithay compositor).

DONE
if [ -n "$has_dm" ]; then
    cat <<DONE
  - X11 session (the full desktop): log out and pick "chonkstep" in
    ${has_dm}'s session list.
DONE
else
    cat <<DONE
  - X11 session (the full desktop): no display manager is enabled
    (stock Omarchy boots straight into Hyprland), so switch to a TTY
    (Ctrl+Alt+F3), log in, and run:
      startx ${repo}/scripts/xsession.sh
    To get a graphical session picker instead, install and enable a
    display manager (e.g. sddm) - the session entry is already in place.
DONE
fi
cat <<DONE
  - Wayland: run the compositor nested inside your current desktop -
      ${repo}/target/release/chonkstep-wayland
    It opens a window that is its screen: same chrome, dock, menus, and
    themes as the X11 session, with X11 apps running through XWayland.
    Nested is the whole story today - the DRM/KMS session that would
    make it a login option of its own is not built yet, which is why
    this installer deliberately does not add a wayland-sessions entry.
DONE
if [ "$session_type" = "wayland" ]; then
    cat <<DONE
    (You are on a Wayland session right now, so the nested compositor
    will run here as-is; the X11 session above needs the TTY route.)
DONE
fi
cat <<DONE
  - HiDPI: set "scale = 2.0" in ${config}
    (the whole file is optional and every line is documented; both
    backends read it).
  - Update later with: scripts/update.sh
  - The session entry points at this checkout (${repo});
    moving the checkout means re-running scripts/install.sh.

DONE
