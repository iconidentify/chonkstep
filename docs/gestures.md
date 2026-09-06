# Touchpad gestures

The native **Wayland login session** accepts three- and four-finger swipes by
default:

| Swipe | Action |
| --- | --- |
| Left | Next workspace, creating one when needed |
| Right | Previous workspace; stops at the first |
| Up | Open Overview with smaller previews of the current workspace's windows |
| Down | Dismiss Overview |

These use the workspace and Mission Control directions described in
[Apple's trackpad gesture guide](https://support.apple.com/en-us/102482).
Wayland Overview shows proportionally scaled live windows over the wallpaper,
with desktop thumbnails across the top, a selection outline and a compact
window caption. Small windows are never enlarged. Click-to-select, arrow keys,
Return and Escape work throughout. An upward swipe while it is open keeps it
open; horizontal swipes also work inside Overview. X11 retains the rasterized
card fallback.

An action commits when the fingers lift after enough travel. Short movements,
ambiguous diagonal strokes, cancelled gestures and strokes that return to their
starting point do nothing. Each stroke switches at most one workspace. Windows
do not animate along with the fingers. Two-finger scrolling and pinch-to-zoom
continue to reach applications. Swipes are physical touchpad directions,
independent of `natural_scroll`, `scroll_factor` and output scale.

The compositor owns the complete reserved swipe stream, including cancellation.
Session lock, seat resume and device removal retire an in-flight swipe. A drag,
menu, exclusive layer keyboard owner, focus grab or Alt+Tab session prevents a
desktop action. Disabling gestures passes swipes through to applications.

## Configuration

Add this optional table to `~/.config/chonkstep/config.toml`; changes apply with
the normal live reload:

```toml
[input.gestures]
enabled = true
fingers = 0       # 0 = both three and four; or choose 3 or 4
distance = 80    # logical touchpad travel; accepted range 24..1000
```

Settings are latched at the beginning of each stroke. Hyprland's arbitrary
`gesture` / `hl.gesture` bindings remain unsupported; this table controls the
native desktop gestures. The X11 session does not receive this libinput gesture
stream. A nested compositor depends on its host forwarding gestures; use the
native login session for physical touchpad gestures.

## Bar boundary

When a top layer-shell bar such as the Omarchy bar reserves an exclusive zone,
managed window titlebars stay below it during placement, movement, resizing and
session restore. A bar that appears late also pushes existing titlebars out of
its strip. Each output uses its own reservation, including scaled outputs.
Removing or hiding the bar releases the boundary. Fullscreen windows continue
to cover the full output. No fixed bar height or Omarchy process lookup is used.

## Cost and verification

The swipe recognizer stores at most 40 bytes per seat and performs no allocation
or rendering during updates. It has no helper process, thread or polling timer.
The committed action reuses workspace management. Native Overview reuses client
GPU textures and sparse decoration buffers, suspends screenshot readbacks while
open, and never allocates a monitor-sized raster. Captions are painted once per
entry set; selection changes only the outline and which cached caption is
shown. Packing runs only when the entry set changes (at most 32 linear passes).
Closing releases the scene, labels and any small minimized-window fallbacks.
Move snapping borrows monitor/window records instead of building a target vector
on every pointer event, and repeated motion against the bar does not resend an
unchanged frame position.

`cargo test -p wm-core --test gesture_allocations` checks zero allocator requests
over 100,000 recognition updates and snapping queries. This is an allocation
budget, not a hardware latency or RSS benchmark. `scripts/e2e.sh --headless
--test desktop_gestures` checks actual input routing, workspace visibility,
Overview lifecycle at 1x/2x, configuration and the layer-shell bar boundary.
