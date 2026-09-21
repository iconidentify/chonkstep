# Tiny windows after monitor disconnect: root cause

Confirmed on 2026-09-20 (America/Los_Angeles). The checkout was fast-forwarded
from `a65399f` to `865d586` (0.7.0) before investigation. The installed and
running compositor initially reported `preview-v0.5.0-74-g658f816`. Both revisions contain
the faulty configure shortcut; updating to unmodified 0.7.0 does not fix it.

## Evidence

The live desktop has one Sony SDMU27M90 on DP-1 at 3840×2160, scale 1.5,
with `interaction_mode = "spaces"` and `keyboard_mode = "desktop"`.
Read-only `hyprctl -j clients` inspection found a floating `foot` window,
address `0x500000003`, with reported content size **11×25**.

The [live log excerpt](live-session-excerpt.log) records DP-1 disappearing,
the output count becoming zero, and the desktop resizing to 0×0. On September
15 at 23:12 PDT (September 16 at 06:12 UTC), the same dispatch then bounds
1226×1856, 1434×893 and 1920×1230 window geometries to **1×1**. September 20
at 20:11:47 and 20:12:10 PDT shows the same zero-output transitions. Geometry
warnings are emitted once per surface, so later occurrences need not repeat
the clamp warning. The historical surface IDs do not independently establish
which warning belongs to the currently tiny terminal.

An isolated Weston-hosted compositor built from `865d586` reproduces the
failure with two real `foot` clients. Removing its only virtual output and
restoring it changes the background terminal from **700×487 to 7×17** on the
first cycle. The [reproduction trace](reproduction-before.log) shows the
intermediate 1×1 policy update and the terminal's eventual small buffer.
The user's physical monitor and application windows were not manipulated.

## Causal chain

1. `apply_output_change` removes the final monitor and sets `output_size`
   to `union_size([])`, which is 0×0.
2. Spaces parks windows and repairs keyboard focus. `apply_pending_focus`
   stages activation state on every native toplevel, including windows
   that were already inactive.
3. For those unchanged toplevels, Smithay deduplicates the configure.
   `WaylandBackend::flush_configures` then takes its existing-buffer shortcut
   and calls `committed_content_size` with the empty desktop's `output_size`.
4. `client_size_limit(0×0)` is 1×1. A valid retained buffer is therefore read
   as 1×1, and the shortcut queues a synthetic `ClientSizeCommitted` event.
5. The dispatch validation repeats the same bounded measurement and accepts
   it. `handle_configure_request` writes 1×1 into `Client::geometry` **before**
   calling `reflow_frame_internal`. The latter's headless guard prevents an
   immediate backend resize, but cannot undo the corrupted policy geometry.
6. Reconnection reflows the saved policy geometry and sends the tiny resize
   request to the terminal. `foot` responds with its small cell-sized content.

This explains why background windows are susceptible and why the damage can
remain invisible until the monitor returns. The already-focused window changes
activation state and takes a different configure branch; the existing
single-client reconnect test did not exercise the unchanged-background case.
The ordinary late-buffer commit handler already has a headless safeguard,
but this synthetic event producer bypassed it.

The shortcut was introduced by `f75bdc94c8426f62126fafa1fa1362a786652b23`
("Keep client buffers and window frames aligned during resize", September 13).
It legitimately handles fractional-scale requests that round to an unchanged
logical size; it needs a connected output before interpreting desktop limits.

## Local correction and verification

`crates/wm-wayland/src/xdg.rs` now requires a nonempty monitor list before
the deduplicated-configure shortcut synthesizes a client size event. Actual
configure delivery and the allocation bounds remain intact.

`monitor_disconnect_preserves_terminal_sizes` in
`crates/chonk-testkit/tests/mac_spaces.rs` covers a focused terminal and an
inactive terminal through three disconnect/reconnect cycles. It inspects both
the backend geometry and policy geometry while headless, then the restored
window sizes. The original reproduction failed on its first reconnect;
the corrected implementation passes all three cycles.

Validation completed: the new regression, the existing headless reconnect
test, and all four `resize_order` end-to-end tests pass. `wm-wayland` library
tests report 382 passed and 8 ignored. Clippy for `wm-wayland` and
`chonk-testkit`, including all targets and the repository's denied lints,
passes. `git diff --check` passes.

Run with:

```sh
scripts/e2e.sh --headless --test mac_spaces monitor_disconnect_preserves_terminal_sizes
```

The initial investigation left the patch in local source and did not resize
any live application or restart the desktop.

## Authorized installation

At the user's subsequent request, the patched 0.7.0 compositor was built in
release mode, passed the disconnect regression again using that exact binary,
and was installed at `/usr/bin/chonkstep-wayland`. The desktop was restarted
on September 20 at 20:42 PDT. The running process was verified against the
installed executable, with ELF build ID
`b93481f5d51283ee88beaea2fb02b6e76439f263`. DP-1 returned at 3840×2160,
144 Hz, scale 1.5; the session service and Omarchy shell are running.

The previous binary, patch, installed build and restart verification are kept
under `~/.local/state/chonkstep/builds/2026-09-20-display-disconnect-fix/`.
This replaces the compositor executable; it is not a package-manager upgrade
of the other ChonkStep utilities.
