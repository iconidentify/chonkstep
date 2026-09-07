# Touchpad gestures

The native **Wayland login session** accepts three- and four-finger swipes by
default:

| Swipe | Action |
| --- | --- |
| Left | Next workspace; creates one past an occupied final desktop |
| Right | Previous workspace; elastic resistance at the first |
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

Drag a window upward onto a desktop thumbnail to move it there without leaving
Overview. The live window image follows the pointer, becomes smaller and
translucent, and highlights the entire destination thumbnail. Desktop labels
include window counts. A drop keeps Overview open on the source desktop; a
drop outside the row cancels. Escape cancels a held drag first, and a second
Escape closes Overview. Releasing a cancelled drag cannot activate a window.
The close-control corner is part of the drop target, not a delete operation.
These generous targets follow [Mission Control's window-to-Space interaction](https://support.apple.com/guide/mac-help/work-in-multiple-spaces-mh14112/mac).

Each desktop thumbnail has an × in its upper-right corner while more than one
desktop exists. Click it to remove that desktop. Its windows move to the desktop
on its left (or the next desktop when closing the first), keeping their sizes,
focus and minimized state. The row renumbers immediately and Overview stays open.
The final desktop cannot be closed. Pressing × and releasing away cancels.

Swiping left on an empty final desktop stops there, so repeated swipes cannot
create a chain of empty desktops. Minimized windows still count as occupying a
desktop. Explicit workspace keyboard commands can still create desktops on demand.

Once the initial axis is recognized, the desktop follows the fingers on each
composited frame. Moving left slides the current workspace left and brings the
next one in from the right; moving right does the reverse. Reverse direction at
any time, including through the origin toward the other neighbor. The logical
workspace and keyboard focus do not change while neighboring windows are exposed.

Lift slowly before halfway to cancel, or beyond halfway to commit. A quick
flick can commit earlier; reversing before release can cancel even after crossing
halfway. The release hands its current speed to a critically damped spring,
not a fresh easing animation. Touching a settling desktop catches its current
position. Missing neighbors have bounded, nonlinear elastic resistance and
spring back. Each stroke switches at most one workspace.

Overview also follows vertical finger movement: live windows move and scale
between their desktop geometry and the cached Overview arrangement, while its
controls fade in or out. Reversing reverses the morph. Opening and closing use
the same velocity projection and spring as workspace movement. Up while already
open does not close it, and down while closed does not open it.

Initial jitter and ambiguous diagonal strokes do not start transitions.
Cancelled libinput gestures settle back without committing. Two-finger scrolling and pinch-to-zoom
continue to reach applications. Swipes are physical touchpad directions,
independent of `natural_scroll`, `scroll_factor` and output scale.

The compositor owns the complete reserved swipe stream, including cancellation.
Session lock, seat pause/resume and device removal retire an in-flight swipe. A drag,
menu, exclusive layer keyboard owner, focus grab or Alt+Tab session prevents a
desktop action. Disabling gestures passes swipes through to applications.
During a partial transition, pointer hit-testing cannot focus or activate an
exposed window. A pointer press cancels the transition and consumes that click's
matching release. Keyboard commands take over from the gesture. Topology, output
layout/scale changes, or replacement of the cached Overview scene cancel safely.

## Configuration

Add this optional table to `~/.config/chonkstep/config.toml`; changes apply with
the normal live reload:

```toml
[input.gestures]
enabled = true
fingers = 0       # 0 = both three and four; or choose 3 or 4
distance = 80    # slow-release midpoint; full gesture span = 160 logical units
```

Settings are latched at the beginning of each stroke. Hyprland's arbitrary
`gesture` / `hl.gesture` bindings remain unsupported; this table controls the
native desktop gestures. The X11 session does not receive this libinput gesture
stream. A nested compositor depends on its host forwarding gestures; use the
native login session for physical touchpad gestures.
The accepted distance range remains 24..1000. There are no new configuration
keys. Sensitivity is independent of output width, resolution, and scale; each
output converts the same normalized progress into its own physical translation.

## Bar boundary

When a top layer-shell bar such as the Omarchy bar reserves an exclusive zone,
managed window titlebars stay below it during placement, movement, resizing and
session restore. A bar that appears late also pushes existing titlebars out of
its strip. Each output uses its own reservation, including scaled outputs.
Removing or hiding the bar releases the boundary. Fullscreen windows continue
to cover the full output. No fixed bar height or Omarchy process lookup is used.

## Cost and verification

The recognizer keeps eight timed samples inline on the seat and performs no
heap allocation during updates. Axis lock prepares neighboring scene metadata
once; updates change only motion/physics state and mark damage. Native KMS uses
the existing vblank/frame-clock scheduling. The nested backend has an animation
deadline only while a spring is active. There is no animation thread, helper
process, permanent polling timer, screenshot animation, or per-update packing.
The settled action reuses workspace management exactly once. Native Overview reuses client
GPU textures and sparse decoration buffers, suspends screenshot readbacks while
open or transitioning, and never allocates a monitor-sized raster. Captions are painted once per
entry set; selection changes only the outline and which cached caption is
shown. Packing runs only when the entry set changes (at most 32 linear passes).
Closing releases the scene, labels and any small minimized-window fallbacks.
Close glyphs are small cached textures created with the desktop row; hovering
does not repaint or allocate them. Removing a desktop scans client memberships
once and only maps windows newly revealed by the merge.
Move snapping borrows monitor/window records instead of building a target vector
on every pointer event, and repeated motion against the bar does not resend an
unchanged frame position.

`cargo test -p wm-core --test gesture_allocations` checks zero allocator requests
over 100,000 timed recognition/physics updates and snapping queries. This is an allocation
budget, not a hardware latency or RSS benchmark. `scripts/e2e.sh --headless
--test desktop_gestures` checks actual input routing, workspace visibility,
live pixel following at 1x/1.5x/2x, velocity decisions, Overview morph/lifecycle,
client configure counts, cancellation, device/seat ownership, protocol pass-through,
configuration and the layer-shell bar boundary. Overview E2E covers window drops,
keyboard/pointer activation, Escape, and desktop deletion.

The test door's `gesture` ledger reports raw displacement, axis, normalized and
projected progress, release/spring velocity, settle target, and logical origin.
`overview progress=` reports the morph fraction. Nothing is logged per sample
in production. See the [engineering note](engineering/2026-09-06-interactive-gestures.md)
for constants, measurements and remaining physical-hardware validation.
