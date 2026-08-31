# Screen sharing

Sharing the screen from a browser video call works in the Wayland
session, and this document is the map of how — what carries the pixels,
what has to be installed, what the session script sets up, and what to
check when a call shows a black rectangle instead of the desktop.

Verified end to end on 2026-08-30 against a nested compositor: a
ScreenCast request driven over D-Bus returned a PipeWire node, and
frames pulled off that node were the live desktop, pixel for pixel.
The transcript is reproduced at the bottom.

## The chain

A browser cannot talk to a Wayland compositor about screens; the
protocol deliberately gives clients no way to see other clients'
windows. What it talks to instead is a chain of brokers:

    browser (WebRTC getDisplayMedia)
      → xdg-desktop-portal            org.freedesktop.portal.ScreenCast, D-Bus
        → xdg-desktop-portal-wlr      the backend chonkstep-portals.conf selects
          → chonkstep-wayland         zwlr_screencopy_manager_v1 (v3)
            → PipeWire                the video stream the browser consumes

`xdg-desktop-portal` is the frontend every sandboxed-or-not app talks
to. It picks a *backend* per portal interface, and
`xdg-desktop-portal-wlr` is the one that captures wlroots-style
compositors. It never checks whether the compositor actually is
wlroots — it speaks protocols, and the two it needs for capture are
ones chonkstep-wayland advertises: `zwlr_screencopy_manager_v1`
version 3 and `zxdg_output_manager_v1` version 3 (see
`crates/wm-wayland/src/protocols.rs`; the compositor's screencopy is
shm-only, which is exactly the path xdg-desktop-portal-wlr implements
for every compositor without dmabuf capture). Frames captured over
screencopy are pushed into a PipeWire stream, and the node id of that
stream is what the portal hands back to the browser.

## What has to be installed

`scripts/install.sh` installs all of it; by hand it is:

    pacman -S --needed pipewire xdg-desktop-portal \
        xdg-desktop-portal-wlr xdg-desktop-portal-gtk

- **pipewire** (with its stock user services) carries the frames. It
  is already running on essentially any current Arch/Omarchy desktop.
- **xdg-desktop-portal** is the D-Bus frontend, activated on demand
  into the systemd user session.
- **xdg-desktop-portal-wlr** is the ScreenCast/Screenshot backend.
- **xdg-desktop-portal-gtk** answers everything else — the file
  chooser a browser opens for uploads, notifications, settings — so
  the desktop is not left portal-less outside of capture.

## How the pieces find each other

Two routing decisions have to come out right, and both are
configuration this repo ships:

**Which backend answers which portal.** `xdg-desktop-portal` reads
`$XDG_CURRENT_DESKTOP` and looks for a matching
`<desktop>-portals.conf`. The session sets `XDG_CURRENT_DESKTOP=chonkstep`
(exported by `scripts/wayland-session.sh` for TTY logins, and set as
`DesktopNames=chonkstep` in the session's .desktop entry for display
managers — both spellings installed by `scripts/install.sh`), and
`packaging/portal/chonkstep-portals.conf` — installed to
`/usr/share/xdg-desktop-portal/chonkstep-portals.conf` — routes it:

    [preferred]
    default=gtk
    org.freedesktop.impl.portal.ScreenCast=wlr
    org.freedesktop.impl.portal.Screenshot=wlr

A user override can live at
`~/.config/xdg-desktop-portal/chonkstep-portals.conf`.

**Which Wayland socket the backend opens.** The portals are D-Bus
activated into the systemd user session, so they are *not* children of
the compositor and inherit nothing from it. Something has to publish
the compositor's `WAYLAND_DISPLAY` (and `XDG_CURRENT_DESKTOP`) into
the D-Bus activation environment and the systemd user environment, or
the wlr backend starts, finds no display, and dies — the classic
silent screen-share failure on every wlroots-adjacent desktop. Only
the compositor knows which socket name it allocated, and it announces
it in one log line (`wayland socket listening socket="wayland-N"`,
`crates/wm-wayland/src/state.rs`). `scripts/wayland-session.sh` runs a
small background watcher that tails the session log for that line and
runs the standard incantation whenever the name changes (it can — a
crash recovery re-execs the compositor):

    dbus-update-activation-environment --systemd \
        WAYLAND_DISPLAY=<socket> XDG_CURRENT_DESKTOP=chonkstep \
        XDG_SESSION_TYPE=wayland

Nothing in the compositor itself touches D-Bus; the session script
owns this, as it owns the rest of the session environment.

## Portal interface status

Checked against the frontend with the chonkstep config active:

| Portal interface | Backend | Status |
| --- | --- | --- |
| ScreenCast | wlr | **Works** — verified end to end; node id returned, frames flowing, pixels correct |
| Screenshot | wlr | **Works** — `Screenshot()` returned a real PNG of the session (xdg-desktop-portal-wlr shells out to `grim`, which is also installed) |
| FileChooser, Notification, Settings, and the rest | gtk | **Backend activates and answers** (D-Bus ping + interface version 4 confirmed); the dialogs themselves are ordinary windows and were not driven headlessly |
| Window/toplevel capture (`types=2` in SelectSources) | wlr | **Not available** — xdg-desktop-portal-wlr only captures outputs (`AvailableSourceTypes=1`); this is an upstream backend limitation, not a chonkstep gap. "Share entire screen" works; "share a single window" is not offered |

## Troubleshooting

The three classic failures, in the order to check them:

1. **The portal sees no WAYLAND_DISPLAY.** Symptom: the share picker
   appears but selecting a screen fails instantly, and
   `journalctl --user -u xdg-desktop-portal` shows the wlr backend
   failing to start. Check
   `systemctl --user show-environment | grep WAYLAND_DISPLAY` — it
   must name the compositor's live socket. If it is missing, the
   session script's watcher never fired: look for the
   `wayland socket listening` line in
   `~/.local/state/chonkstep/wayland-session.log`, and confirm
   `dbus-update-activation-environment` exists on the machine.

2. **The wrong backend is chosen.** Symptom: screen share silently
   unavailable, or a GNOME/KDE dialog appears on a chonkstep session.
   Check `echo $XDG_CURRENT_DESKTOP` inside a terminal on the session
   (must be `chonkstep`) and that
   `/usr/share/xdg-desktop-portal/chonkstep-portals.conf` exists
   (re-run `scripts/install.sh` if not). The frontend caches its
   choice: `systemctl --user restart xdg-desktop-portal` after fixing
   either. `/usr/lib/xdg-desktop-portal -v -r` in a terminal prints
   exactly which backend it maps each interface to.

3. **PipeWire is not running.** Symptom: the portal returns a node id
   but the browser shows black. `systemctl --user status pipewire
   wireplumber` must both be active, and `pw-cli ls Node` while a
   share is up must list an `xdpw-streaming-output` style node.

Two smaller ones met while verifying:

- **Very first frame blank.** The first buffer of a brand-new stream
  arrived zeroed once in testing before real frames followed; a
  browser consuming a continuous stream never shows this, but a
  one-frame probe can catch it. Probe with a few buffers, not one.
- **Nested session captures upside-down.** The winit (nested) backend
  advertises `Transform::Flipped180` on its output (a deliberate EGL
  coordinate squaring — `crates/wm-wayland/src/state.rs`).
  `grim` un-applies the advertised transform itself;
  xdg-desktop-portal-wlr instead forwards it as PipeWire
  `videotransform` metadata, which simple consumers (`gst-launch`)
  ignore — so a nested-session capture can render rotated 180° in
  tools that skip the metadata. The real session's DRM outputs
  advertise `Transform::Normal` (`crates/wm-wayland/src/session.rs`),
  so logged-in screen sharing is upright.

## Reproducing the verification

Everything below ran against a nested compositor
(`CHONKSTEP_BACKEND=winit`), with the frontend pointed at it —
which is exactly what the session script automates on a real login:

    dbus-update-activation-environment --systemd \
        WAYLAND_DISPLAY=wayland-3 XDG_CURRENT_DESKTOP=chonkstep
    systemctl --user restart xdg-desktop-portal

then a plain D-Bus client doing what a browser does — `CreateSession`,
`SelectSources` (monitor, single), `Start`:

    ScreenCast portal version=4 AvailableSourceTypes=1
    <- Response(CreateSession): code=0
       results={'session_handle': '/org/freedesktop/portal/desktop/session/1_42391/probesess'}
    <- Response(SelectSources): code=0 results={}
    <- Response(Start): code=0 results={'streams':
       [(86, {'position': (0, 0), 'size': (1280, 800), 'source_type': 1})]}

and frames pulled off node 86 while the session stayed open:

    gst-launch-1.0 pipewiresrc path=86 num-buffers=5 \
        ! videoconvert ! pngenc snapshot=true ! filesink location=frame.png

produced a 2560×1600 PNG (a scale-2 session) of the live desktop.
The `Screenshot` portal answered the same way:

    <- Response(Screenshot): code=0 results={'uri': 'file:///tmp/out.png'}

For the interactive chooser-free run above, the backend was given a
config (`~/.config/xdg-desktop-portal-wlr/config`) naming the output;
in normal use xdg-desktop-portal-wlr pops its chooser (`slurp`) so the
user picks the screen, exactly as a browser share dialog expects.
