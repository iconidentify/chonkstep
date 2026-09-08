#!/usr/bin/env bash
# Builds the Debian package: chonkstep's X11 session, and only that.
#
# This is the LCOS/Devuan artifact. It is deliberately a NARROWER
# package than packaging/arch, which ships both backends plus the SDDM
# theme, the uwsm session entries, the xdg-desktop-portal map and the
# Omarchy shell plugins. None of that belongs here:
#
#   - LCOS is Devuan Excalibur: sysvinit, no systemd, no logind. The
#     Wayland compositor's session backend opens DRM and input through
#     libseat and wants a seat manager; the X11 binary asks the X server
#     for everything and needs none of it.
#   - LCOS's display manager is LightDM, not SDDM, so the SDDM theme and
#     its drop-ins have nothing to configure.
#   - There is no Omarchy on LCOS to host the plugins or the menu.
#
# Keeping the Debian package to the X11 half is what makes supporting
# LCOS cheap: it adds one `cargo build -p chonkstep` to the matrix and
# inherits every fix the Omarchy work already makes to the shared
# `chonk-shell`. It does not add a second desktop to maintain.
#
# Build it the way CI does, in a container that matches the target's
# libc exactly (LCOS 0.3 and debian:trixie are both glibc 2.41):
#
#   docker run --rm --platform linux/amd64 \
#     -v "$PWD":/src -w /src rust:trixie \
#     packaging/debian/build-deb.sh
#
# This is also the ONLY build that carries the LCOS assets: see the
# `--features lcos` note by the cargo invocation below. Omarchy's Arch
# package builds the same tree with default features and ships none of
# them.
#
# Output: dist/chonkstep_<version>-<revision>_<arch>.deb
set -euo pipefail

cd "$(dirname "$0")/../.."
REPO_ROOT="$PWD"

# The workspace version is the single source of truth for the package
# version, exactly as packaging/arch reads it — a Debian package that
# disagreed with the Arch one about what 0.3.0 means would be a bug.
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)"
REVISION="${DEB_REVISION:-1}"
: "${CARGO_TARGET_DIR:=$REPO_ROOT/target}"
export CARGO_TARGET_DIR

# dpkg's architecture name, not uname's: amd64/arm64, not x86_64/aarch64.
ARCH="$(dpkg --print-architecture)"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> building chonkstep $VERSION-$REVISION ($ARCH), X11 session only"

# Only the three binaries the X11 desktop actually runs. `--workspace`
# would drag in wm-wayland and Smithay, which is precisely the cost this
# package exists to avoid: none of those crates' system dependencies
# (libdrm, libinput, libseat, libgbm) are needed to talk to an X server,
# and asking for them would make the LCOS build fail for reasons that
# have nothing to do with LCOS.
# `--features lcos` is what makes this the LCOS build rather than a
# Debian build of the Omarchy desktop. It compiles in the Lunduke Navy
# artwork, the LCOS dock mark, the LCOS and Lunduke desk themes, and the
# theme/photograph pairing — about 2.5MB of assets that the Arch package
# deliberately does not carry. Measured: the default binary is
# 26,996,816 bytes and this one 29,666,504, and no LCOS string appears
# in the former at all.
cargo build --release --locked --features lcos \
    -p chonkstep -p chonk-about -p chonk-netjoin

echo "==> staging package tree"
install -Dm755 "$CARGO_TARGET_DIR/release/chonkstep"     "$STAGE/usr/bin/chonkstep"
install -Dm755 "$CARGO_TARGET_DIR/release/chonk-about"   "$STAGE/usr/bin/chonk-about"
install -Dm755 "$CARGO_TARGET_DIR/release/chonk-netjoin" "$STAGE/usr/bin/chonk-netjoin"
install -Dm755 scripts/chonk-get                          "$STAGE/usr/bin/chonk-get"
install -Dm755 scripts/chonkstep-bugreport                "$STAGE/usr/bin/chonkstep-bugreport"

# The session launcher and the two live-reload helpers the running
# desktop calls. xsession.sh resolves the binary through PATH when it is
# not sitting next to a target/release checkout, which is exactly the
# package case — see its own comment; it needs no packaging variant.
install -Dm755 scripts/xsession.sh       "$STAGE/usr/lib/chonkstep/xsession.sh"
install -Dm755 scripts/reload.sh         "$STAGE/usr/lib/chonkstep/reload.sh"
install -Dm755 scripts/restart.sh        "$STAGE/usr/lib/chonkstep/restart.sh"
install -Dm755 scripts/verify-install.sh "$STAGE/usr/lib/chonkstep/verify-install.sh"

# The Debian session entry point. It seeds a terminal the X11 session
# can actually launch and then execs xsession.sh; see its own comment
# for why that cannot be done anywhere else.
install -Dm755 packaging/debian/debian-session.sh "$STAGE/usr/lib/chonkstep/debian-session.sh"

# LightDM reads this directory for X11 sessions and shows Name= in its
# greeter's session list. There is no "(Wayland)" twin to disambiguate
# against here, so the entry is simply "chonkstep" — matching the Arch
# package's X11 entry byte for byte, deliberately.
install -d "$STAGE/usr/share/xsessions"
cat > "$STAGE/usr/share/xsessions/chonkstep.desktop" <<'DESKTOP'
[Desktop Entry]
Name=chonkstep
Comment=A NeXTSTEP-style window manager with chiseled chrome
Exec=/usr/lib/chonkstep/debian-session.sh
TryExec=/usr/lib/chonkstep/debian-session.sh
Type=Application
DESKTOP

# Only the documentation that describes something this package ships.
# The Wayland, portal, screen-sharing and Omarchy documents are omitted
# on purpose: shipping them would promise features the X11-only package
# does not have.
install -d "$STAGE/usr/share/doc/chonkstep"
install -Dm644 docs/config.example.toml docs/quickstart.md docs/keybindings.md \
               docs/appearance.md docs/control-socket.md docs/logging.md \
               docs/memory.md docs/instrument-platform.md docs/dockapp-protocol.md \
               README.md CHANGELOG.md \
               -t "$STAGE/usr/share/doc/chonkstep"
install -Dm644 LICENSE "$STAGE/usr/share/doc/chonkstep/copyright"

echo "==> computing library dependencies"
# Ask dpkg what the binaries actually link rather than writing a Depends
# line by hand. chonkstep links libc, libm and libgcc and nothing else —
# wm-x11 speaks the X protocol through x11rb, which is pure Rust, so
# there is no libX11/libxcb here to get the versions wrong about. Letting
# dpkg-shlibdeps derive it keeps that true if a future dependency changes
# it.
install -d "$STAGE/DEBIAN"
mkdir -p "$STAGE/debian"
touch "$STAGE/debian/control"
SHLIB_DEPS="$(cd "$STAGE" && dpkg-shlibdeps -O --ignore-missing-info \
    usr/bin/chonkstep usr/bin/chonk-about usr/bin/chonk-netjoin 2>/dev/null \
    | sed 's/^shlibs:Depends=//')"
rm -rf "$STAGE/debian"
: "${SHLIB_DEPS:=libc6}"

# dbus is a real dependency, not a recommendation: xsession.sh execs the
# window manager through `dbus-run-session`, so the session does not
# start without it. x-terminal-emulator likewise: the root menu's
# Terminal item and the spawn-terminal keybinding are core to the
# desktop, and the built-in default (foot) is a Wayland client that
# cannot run on this session -- see debian-session.sh.
#
# fonts-urw-base35 supplies Nimbus Sans, which the NeXT Lavender theme
# names (the Arch recipe lists gsfonts for exactly this). LCOS happens to
# have it already via the base desktop, so it is a Suggests rather than a
# Recommends -- but on a minimal Debian that theme would fall back.
#
# There is deliberately NO dependency on an X server. Debian's own window
# managers (openbox, i3-wm, fluxbox) all depend on libraries only, and
# that convention is what makes this package work on LCOS at all: LCOS
# has no xserver-xorg-core to depend on. Its XLibre packages Provide
# x-window-system-core/xorg under entirely different package names, and a
# window manager can in any case be pointed at a display it did not
# start.
cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: chonkstep
Version: $VERSION-$REVISION
Section: x11
Priority: optional
Architecture: $ARCH
Maintainer: chonkstep maintainers <noreply@github.com>
Depends: $SHLIB_DEPS, dbus, x-terminal-emulator
Recommends: picom, fonts-dejavu-core
Suggests: x11-xserver-utils, fonts-jetbrains-mono, fonts-noto-core, xinit, fonts-urw-base35
Homepage: https://github.com/iconidentify/chonkstep
Description: NeXTSTEP-style X11 window manager for LCOS
 chonkstep is a traditional floating window manager whose chrome, dock and
 menus follow the NeXTSTEP look: chiseled bevels, a titlebar carrying a
 miniaturize box and a close box, and a dock of live instrument tiles.
 .
 This package ships the X11 session only. It runs against any X server
 that speaks X11R6, including the XLibre server used by LCOS, and pulls in
 no Wayland, DRM or seat-management libraries.
 .
 Built with the "lcos" feature: it carries the LCOS and Lunduke desk
 themes, the Lunduke Navy artwork and the LCOS dock mark, and pairs each
 desk theme with its photograph from /usr/share/backgrounds/lcos when the
 host ships them. The Arch package for Omarchy carries none of that.
CONTROL

echo "==> building the .deb"
mkdir -p "$REPO_ROOT/dist"
DEB="$REPO_ROOT/dist/chonkstep_${VERSION}-${REVISION}_${ARCH}.deb"
# --root-owner-group so the package's files are root:root without the
# build needing to run as root or fakeroot.
dpkg-deb --root-owner-group --build "$STAGE" "$DEB" >/dev/null

echo "==> $DEB"
dpkg-deb --info "$DEB" | sed -n '/^ Package:/,/^ Description:/p'
echo
echo "contents:"
dpkg-deb --contents "$DEB" | awk '{print "  " $6, $7, $8}'
