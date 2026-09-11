# Mac Spaces across displays

Mac mode now gives each connected output its own Space row. Switching one display
keeps the other displays' active Spaces visible. This applies to the ChonkStep
Wayland compositor, including its managed XWayland applications.

```toml
interaction_mode = "mac"

[mac]
separate_spaces = true  # default in Mac mode
```

Set `separate_spaces = false` to use the previous linked-display desktop policy.
Configuration reload preserves windows; changing the policy first exits dedicated
fullscreen Spaces so their return desktops remain meaningful. Enabling separate
Spaces keeps the active linked desktop visible on each display. Linking displays
repairs keyboard focus before accepting further input if the previously focused
window is parked.

## Display and window policy

- Physical pointer movement selects the display under the pointer, including
  movement over application content. It does not steal keyboard focus.
- Explicit application activation selects that application's display and reveals
  its Space. Command-Tab can therefore return to an app on another display
  without switching unrelated displays.
- Control-Left/Right and horizontal desktop swipes visit neighboring Spaces on
  the selected display. They stop at that display's boundaries. Carry-next and
  carry-previous follow the same row.
- A new desktop belongs to the selected display. The Clip's indicator uses local
  numbering. Explicit numeric IPC/config commands retain global slot numbering
  for compatibility; numbers are not stable Space identities.
- New windows open on the selected display's regular desktop. A transient follows
  its parent. Moving a window's center across the display boundary joins the
  destination display's active regular desktop and carries its dialog family.
  Moving into a fullscreen display reveals that display's regular desktop.
  Dragging a fullscreen application's dialog exits the parent's fullscreen Space,
  moves the whole family, and continues the drag on the destination display.
- Client content, popups and decorations are clipped to their owning output.
  Input uses the same boundary. Unmanaged X11 menus inherit the managed parent's
  display. A window straddling an edge cannot paint or take clicks in the adjacent
  display's Space.
- Control-Command-F creates an independent fullscreen Space on the window's
  display. Multiple displays can be fullscreen simultaneously. Exiting restores
  the original desktop and geometry, removes an empty fullscreen Space, and
  preserves the other displays' active Spaces. Fullscreen suppresses its display's
  desktop furniture even when keyboard focus is elsewhere.
- Clicking a pinned window keeps the current Space selected; its original Space
  remains its membership, not an instruction to navigate away.
- Deleting an ordinary Space moves its windows to a neighboring ordinary Space
  on the same display. Each display retains an ordinary desktop. A live fullscreen
  Space must be exited before it can be deleted.

Overview opens on the selected display and shows that display's local strip and
windows. It stays attached to the display where it was opened while the pointer
moves. Application Overview additionally filters that set by application.
Topology changes close Overview and release its keyboard grab.

The desktop strip contains live miniatures of the windows on each desktop, in
actual desktop positions and stacking order. Previews are clipped to the owning
display and include both Wayland and XWayland clients. Moving, resizing, closing
or dragging a window to another desktop updates the miniatures while Overview
remains open. Pinned windows appear in each desktop in their display's row;
minimized and Command-hidden windows are absent from desktop miniatures. Shaded
windows show their titlebar. Fullscreen Spaces display the fullscreen window.
Application Overview filters its main cards; desktop miniatures retain the whole
desktop. Client textures are shared, without taking full-size screenshots.
Inactive clients receive frame callbacks while their previews are visible and
return to the normal parked policy when Overview closes.

The existing Dock, launcher strip, and Clip stay on the primary display. Root
menus open at the pointer. This change does not implement macOS's edge-triggered
Dock relocation or a replicated global application menu bar.

## Disconnect, reconnect, and restore

A display uses a unique EDID identity when available, with connector names for
virtual or indistinguishable displays. Existing connector identities stay stable
when duplicate EDIDs appear or disappear. Display enumeration order does not own
Space identity.

On disconnect, the removed display lends its entire Space row to the primary
surviving display. The survivor keeps its active Space; the borrowed row remains
reachable through its local navigation. Windows move into usable coordinates and
focus is repaired if its previous window became hidden. Reconnecting returns the
borrowed Spaces and windows to their home display and restores its remembered
active Space. Interactive drags end at topology changes. With no connected outputs,
windows (including pinned windows) are parked while their Spaces and geometry
remain available for reconnect. Active swipes are cancelled and new desktop swipes
are ignored until an output returns.

Temporary placement on a smaller surviving display does not overwrite the home
position, normal restore size, or freeform restore rectangle. These return when
the home display reconnects, including after a restart while it was disconnected.
Explicitly moving a window into a Space belonging to another home display adopts
that display and discards the old return point.

The existing 99-Space safety limit remains global. At capacity, adding an output
reassigns an existing ordinary desktop, preferring an empty one, rather than
leaving the new display without a Space.

Opt-in `restore_session = true` now restores stable IDs, empty Space rows,
per-display active selections, window membership, and dedicated fullscreen return
Spaces. The existing atomic session file gains an `@spaces` record and optional
fullscreen and retained home-geometry metadata on window records. A window saved
while maximized and fullscreen retains its original normal rectangle, so leaving
both states after restore recovers its original size. Legacy window/layout records remain
readable; malformed topology metadata is rejected without discarding otherwise
valid window records. Geometry uses monitor-relative coordinates, including a
connector-name fallback for virtual displays.

## Native protocols and compatibility

`ext-workspace-v1` publishes one group per connected display, with actual
`wl_output` membership, local coordinates and an active Space for each group.
Space handles use stable IDs through compaction and reconnect. Activation is
batched until the manager commits; removed handles cannot activate replacement
Spaces. Linked mode publishes one group covering the connected outputs.

Hyprland-compatible IPC reports each monitor's actual active workspace and assigns
empty Spaces to their owning monitor too. Existing global numeric workspace IDs
remain unchanged as the compatibility projection. EWMH exposes the selected
output's active Space through its single-current-desktop field.

Surface output membership, frame callbacks, rendering, and input all consume the
same visible scene. Fully contained surfaces retain their original render elements;
only boundary crossings need crop wrappers. Clipping walks only the newly appended
window elements and retains vector capacity.

## Verification

The `mac_spaces` E2E target creates two real `wl_output` heads within an isolated
nested host window. It uses the production output hotplug path, per-output scene
builder, physical seat input route, real xdg clients, screencopy pixels, native
output-enter/leave events, and workspace IPC. Each head has its own viewport;
both share the host swap cadence. The private test socket is the only entry point
for this virtual topology; native hardware sessions reject it.

```sh
scripts/e2e.sh --headless --host-renderer gl --release --test mac_spaces
cargo test --locked -p wm-core -p wm-config -p chonk-shell -p wm-wayland --lib
```

The twenty workflows cover independent Control-arrow navigation and Command-Tab,
dual fullscreen with exact geometry restore and visible-Space furniture rules, live swipes with local boundary resistance, clipping plus input exclusion and
surface output membership, native workspace groups, hotplug recovery, second-display
Overview and grab cleanup, and persisted fullscreen/empty Spaces across reconnect.
Pixel assertions additionally verify native and XWayland miniature placement,
parked content updates, edge clipping, drag membership, removal, pinned/minimized
visibility, fullscreen previews, and callback parking after Overview closes.
The regression cases also exercise policy reload focus, active-desktop preservation,
pinned-window clicks, fullscreen-dialog dragging, complete disconnect with swipes,
unequal display sizes, and maximized/fullscreen save-and-restart both online and
while the home display is absent. Core tests additionally cover dialog families, output reordering, duplicate EDIDs,
all outputs disconnected, capacity limits, malformed restore data, and linked mode.

See [validation results](mac-mode-validation.md) for the completed regression run.
This software fixture does not qualify physical Apple input, lid events, KMS hotplug,
mixed physical refresh rates or scanout behavior. Paired Split View, Space reordering,
per-app assignment controls and automatic Dock relocation remain separate work.
