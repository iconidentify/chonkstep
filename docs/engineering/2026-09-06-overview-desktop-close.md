# Overview desktop removal — 2026-09-06

Overview now has a circular × in each desktop thumbnail's upper-right corner
when more than one desktop exists. Native and raster fallback rendering share
the same close rectangles and glyph painter. The glyphs are small cached
textures; pointer movement neither repaints them nor captures window pixels.
Press and release must hit the same control. Rebuilding Overview invalidates
an armed close, including when a new application arrives during the press.

`WindowManager::remove_workspace` merges windows into the preceding desktop
(the next one when closing the first), compacts memberships, and preserves the
active desktop, focus, minimized state and transient relationships. It performs
one membership scan and only maps newly visible windows; already-visible
windows do not unmap/remap. Workspace count, window membership and workarea
publications follow the new row. Native workspace clients receive removed
events for retired numbered slots, and stale handles cannot activate them.

Swiping beyond an empty final desktop now stops. Minimized windows still count
as occupants. Explicit keyboard workspace commands retain on-demand creation.
This prevents a series of swipes from creating a chain of empty desktops.

## Validation

- `scripts/check.sh all` passed: strict Clippy, documentation, workspace and
  Wayland unit checks, and all 26 Python harness tests.
- Unit coverage removes each of four desktop positions from every active
  position, preserving focus, minimizing state, window count, visibility and
  published memberships. Additional cases cover empty desktop removal, the
  last-desktop guard, frameless parent/dialog windows and pinned windows.
- Close-control geometry is checked for 1, 2, 8 and 64 desktops at 1x/2x scale
  in both native and raster fallback layouts.
- The immutable optimized compositor passed 14 nested cases: Overview (4),
  gestures (3), native workspace protocol (2), fullscreen (4), buffer age (1).
  Each target ran in a separate `scripts/e2e.sh --test` invocation.
- The new Overview case starts with eight desktops, checks actual white glyph
  pixels, cancels a dragged-off click, invalidates a press when a new window
  arrives, closes empty and occupied desktops, and reduces the row to one
  while retaining all three application windows. A native protocol client
  attempts to reactivate every removed handle; the row remains at one.
- Repeated swipes on the empty trailing desktop leave the count unchanged.

Optimized executable SHA-256:
`3fd207dd7c9b39a1141a623370016039446290e11173457584e1eec4953ed4de`.
Installed stripped executable SHA-256:
`155d196a88602f7e403169b33218807bacd97d4b42a767f8b268a9ebb6819614`.
ELF build ID: `2e666163102c35dc3127ae0dcb135811c55d1531`.

Local evidence is retained in `/tmp/chonkstep-overview-close/`: source patch,
immutable executable, build/check/E2E logs and restart verification. The two
scale-specific screenshot sets and workspace-client logs are under
`/tmp/chonk-testkit/overview-close-desktops-{1,2}/`. Both screenshots were
visually inspected at 2x in addition to the pixel assertions at both scales.

The tested executable and debug symbols were installed, and the native session
restarted. `/proc/1750603/exe` matched the installed binary and a Wayland round
trip succeeded. Runtime IPC reported one remaining desktop. The previous binary
is backed up at `~/.local/state/chonkstep/backups/chonkstep-wayland-before-desktop-close-20260906`.

Physical touchpad timing and native DRM frame latency were not measured. These
checks establish behavior and retained-buffer structure, not a latency guarantee.
