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
# xorg-server/xinit: the X session itself. rxvt-unicode: the terminal
# the root menu launches. picom: the session compositor behind the
# themes' translucent terminals. Fonts: DejaVu (WindowMaker-parity
# chrome), gsfonts/Nimbus Sans (the NeXT Lavender theme), JetBrains
# Mono Nerd (terminal), Noto (fallback coverage).
sudo pacman -S --needed --noconfirm \
    xorg-server xorg-xinit \
    rxvt-unicode picom \
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

cat <<DONE

chonkstep is installed.

  - Log out and pick "chonkstep" in the login manager's session list.
  - On a setup without a session picker, start it from a TTY instead:
      startx ${repo}/scripts/xsession.sh
  - Update later with: scripts/update.sh

DONE
