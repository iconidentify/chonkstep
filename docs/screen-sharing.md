# Screen sharing

Sharing the screen from a browser video call works in the Wayland
session, and this document is the map of how — what carries the pixels,
what has to be installed, what the session script sets up, and what to
check when a call shows a black rectangle instead of the desktop.

Verified end to end on 2026-09-03 in a clean Omarchy 4.0.2 virtual
machine installed from the official ISO: a ScreenCast request driven
through the public portal API returned a PipeWire node, and five
1280x800 frames pulled from that node were the live ChonkStep desktop.
The reusable probe and transcript are at the bottom.

## The chain

A browser cannot talk to a Wayland compositor about screens; the
protocol deliberately gives clients no way to see other clients'
windows. What it talks to instead is a chain of brokers:

    browser (WebRTC getDisplayMedia)
      → xdg-desktop-portal            org.freedesktop.portal.ScreenCast, D-Bus
        → xdg-desktop-portal-wlr      the backend chonkstep-portals.conf selects
          → chonkstep-wayland         ext_image_copy_capture_manager_v1
                                      + output/toplevel capture sources
                                      (zwlr_screencopy_manager_v1 v3 fallback)
            → PipeWire                the video stream the browser consumes

`xdg-desktop-portal` is the frontend every sandboxed-or-not app talks
to. It picks a *backend* per portal interface, and
`xdg-desktop-portal-wlr` is the one that captures wlroots-style
compositors. It never checks whether the compositor actually is
wlroots — it speaks protocols, and the ones it needs for capture are
ones chonkstep-wayland advertises. Its preferred path is
`ext_image_copy_capture_manager_v1`, with
`ext_output_image_capture_source_manager_v1` for monitors and
`ext_foreign_toplevel_image_capture_source_manager_v1` plus
`ext_foreign_toplevel_list_v1` for individual windows. The legacy
`zwlr_screencopy_manager_v1` version 3 remains advertised as a fallback,
alongside `zxdg_output_manager_v1` version 3 and a feedback-capable
`zwp_linux_dmabuf_v1` global (see `crates/wm-wayland/src/image_capture.rs`,
`protocols.rs`, and `dmabuf.rs`). Captured frames are pushed into a
PipeWire stream, and the node id of that stream is what the portal hands
back to the browser.

### linux-dmabuf feedback

The preferred ext capture path currently negotiates writable `wl_shm`
buffers; the retained `zwlr_screencopy` fallback does the same.
`zwp_linux_dmabuf_v1` is the separate path GPU clients use to submit
their own buffers. xdg-desktop-portal-wlr 0.8.2 nevertheless requires
the dmabuf global and installs linux-dmabuf feedback listeners before
capture starts.

For a hardware login, ChonkStep asks EGL for the node of the GPU that
actually renders. That distinction matters on split hardware such as
Apple Silicon: `apple-drm` owns the KMS connector while `asahi` owns
`/dev/dri/renderD128`. Advertising the display-only primary node as
`main_device` makes xdg-desktop-portal-wlr 0.8.2 dereference a missing
render device and crash. ChonkStep now accepts only a node whose DRM
type is `Render`, uses it for both feedback and direct-scanout filtering,
and declines both paths when the identity cannot be proven.

When EGL lacks its optional device-query extension, a hardware session
may use the render node paired with its KMS fd—but only if that real node
exists. This preserves the clean virtio/TCG path: `wayland-info`
reported linux-dmabuf version 5, `/dev/dri/renderD128` as the main
device, and the renderer's 114 real format/modifier pairs; the wlr
portal then stayed active and delivered frames. There is deliberately
no fallback to a primary node.

A nested backend has no KMS fd, so it asks EGL for its render node.
Reproduction with a minimal registry walk proved that ChonkStep's
version-3 format list was protocol-correct, but xdg-desktop-portal-wlr
0.8.2 incorrectly installed version-4 feedback listeners against it
and then segfaulted because those events do not exist before version 4.
ChonkStep therefore declines to advertise linux-dmabuf when it cannot
construct modern feedback. The portal can report the missing capability
instead of crashing. It does not affect an SDDM/UWSM hardware session,
including the tested virtual GPU session, because the compositor
already has the KMS node.

## What has to be installed

`scripts/install.sh` installs all of it; by hand it is:

    pacman -S --needed pipewire xdg-desktop-portal \
        xdg-desktop-portal-wlr xdg-desktop-portal-gtk

- **pipewire** (with its stock user services) carries the frames. It
  is already running on essentially any current Arch/Omarchy desktop.
- **xdg-desktop-portal** is the D-Bus frontend, activated on demand
  into the systemd user session.
- **xdg-desktop-portal-wlr** is the ScreenCast/Screenshot backend.
- **xdg-desktop-portal-gtk** answers the toolkit portals — the file
  chooser a browser opens for uploads, notifications, settings — while
  ChonkStep itself answers Inhibit because it owns the idle timers.

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
    org.freedesktop.impl.portal.Inhibit=chonkstep
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

The compositor's D-Bus use is deliberately limited to its idle-inhibit
service. The session script still owns activation-environment publishing.

## Portal interface status

Checked against the frontend with the chonkstep config active:

| Portal interface | Backend | Status |
| --- | --- | --- |
| ScreenCast | wlr | **Works** — verified end to end; node id returned, frames flowing, pixels correct |
| Screenshot | wlr | **Works** — `Screenshot()` returned a real PNG of the session (xdg-desktop-portal-wlr shells out to `grim`, which is also installed) |
| Inhibit | chonkstep | Idle requests feed the compositor's own timers and are released on request close or caller disconnect |
| FileChooser, Notification, Settings, and the rest | gtk | **Backend activates and answers** (D-Bus ping + interface version 4 confirmed); the dialogs themselves are ordinary windows and were not driven headlessly |
| Window/toplevel capture (`types=2` in SelectSources) | wlr | **Supported** — ext image-copy-capture advertises both output and foreign-toplevel sources; compatible portal backends expose `AvailableSourceTypes=3` |

Backend matrix:

| Chonkstep graphics path | Capture globals | linux-dmabuf | ScreenCast |
| --- | --- | --- | --- |
| DRM login (physical or virtual GPU) | ext v1 + wlr v3 fallback | v5 with default feedback | **Supported; clean Omarchy VM verified** |
| DRM login with separate KMS/render devices | ext v1 + wlr v3 fallback | v5 when EGL identifies the actual render node; otherwise absent | Supported when the driver exposes its renderer identity; otherwise fails cleanly |
| Nested GPU with EGL render-node discovery | ext v1 + wlr v3 fallback | v5 with default feedback | Supported |
| Nested EGL with formats but no discoverable node | ext v1 + wlr v3 fallback | absent | Unsupported, but the portal fails cleanly instead of crashing |
| Renderer without dmabuf import formats | ext v1 + wlr v3 fallback | absent | Unsupported by the current wlr portal backend |

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

The repository includes a browser-equivalent probe. For a deterministic
test, temporarily give xdg-desktop-portal-wlr the chooser-free fixture,
restart the frontend, and run it from the ChonkStep session:

    install -Dm644 scripts/fixtures/xdg-desktop-portal-wlr-e2e.conf \
        ~/.config/xdg-desktop-portal-wlr/config
    systemctl --user restart xdg-desktop-portal-wlr xdg-desktop-portal
    scripts/portal-screencast-e2e.py --buffers 5

It does `CreateSession`, `SelectSources` (one monitor), `Start`, and
`OpenPipeWireRemote` through `org.freedesktop.portal.ScreenCast`, then
uses `pipewiresrc` to consume the returned node. The clean Omarchy VM
run below predates ext image-copy-capture support and records the former
monitor-only baseline:

    ScreenCast portal version=4 AvailableSourceTypes=1
    <- Response(CreateSession): code=0
       results={'session_handle': '/org/freedesktop/portal/desktop/session/...'}
    <- Response(SelectSources): code=0 results={}
    <- Response(Start): code=0 results={'streams':
       [(59, {'position': (0, 0), 'size': (1280, 800), 'source_type': 1})]}

and wrote five complete 1280x800 PPM frames (15,360,080 bytes) from
node 59. The captured pixels showed the running Omarchy shell, a native
Wayland terminal, cursor, notifications, wallpaper, and dock.
The `Screenshot` portal answered the same way:

    <- Response(Screenshot): code=0 results={'uri': 'file:///tmp/out.png'}

Remove the temporary config and restart both portal services after the test.
In normal use xdg-desktop-portal-wlr opens its chooser (`slurp`) so the
user picks the screen, exactly as a browser share dialog expects.
