#!/usr/bin/env bash
# Installs chonkstep on an Omarchy (or any Arch-based) system as a
# selectable X11 session alongside the existing desktop. Nothing is
# copied out of this checkout: the session entry points back into the
# repo, so scripts/update.sh (git pull + rebuild + hot-restart) is the
# whole upgrade story.
#
# What this does:
#   1. Installs runtime dependencies with pacman (Xorg, the terminal,
#      the compositor, the theme fonts) and a Rust toolchain if the
#      system has none.
#   2. Builds the release binaries.
#   3. Installs /usr/share/xsessions/chonkstep.desktop pointing at this
#      checkout's scripts/xsession.sh, so chonkstep appears in the
#      login manager's session picker.
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
sudo pacman -S --needed --noconfirm \
    xorg-server xorg-xinit xorg-xauth \
    rxvt-unicode picom wireplumber \
    ttf-dejavu gsfonts ttf-jetbrains-mono-nerd noto-fonts

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

cat <<DONE

chonkstep is installed.

DONE
if [ -n "$has_dm" ]; then
    cat <<DONE
  - Log out and pick "chonkstep" in ${has_dm}'s session list.
DONE
else
    cat <<DONE
  - No display manager detected (stock Omarchy boots straight into
    Hyprland). Switch to a TTY (Ctrl+Alt+F3), log in, and run:
      startx ${repo}/scripts/xsession.sh
    To make chonkstep appear in a graphical session picker instead,
    install and enable a display manager (e.g. sddm) - the session
    entry is already in place.
DONE
fi
cat <<DONE
  - HiDPI: set "scale = 2.0" in ${config}
    (the whole file is optional and every line is documented).
  - Update later with: scripts/update.sh
  - The session entry points at this checkout (${repo});
    moving the checkout means re-running scripts/install.sh.

DONE
