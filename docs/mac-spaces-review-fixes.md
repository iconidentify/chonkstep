# Per-display Spaces review fixes

The adversarial review of `fe81b40` confirmed seven defects. This follow-up fixes
all seven and promotes the reproductions into permanent regression coverage.

| Finding | Resulting behavior | End-to-end regression in `mac_spaces` |
| --- | --- | --- |
| Swipe with no outputs aborts the compositor | Hotplug cancels active gestures; headless input cannot create a desktop transition; reconnect restores pixels and input | `headless_swipes_are_ignored_and_reconnect_recovers_input_and_pixels` |
| Linking displays leaves a hidden client focused | Visibility changes repair core and seat focus; the visible app receives subsequent keys | `linking_displays_repairs_focus_before_more_keys_reach_clients` |
| Moving a fullscreen parent's dialog corrupts its Space | Exit fullscreen for the family, retire the empty exclusive Space, move membership together, and continue the drag | `dragging_a_fullscreen_dialog_moves_a_consistent_family` |
| Smaller surviving display destroys home geometry | Preserve the normal and freeform home rectangles independently of temporary rescue placement, including across restart | `smaller_survivor_keeps_home_position_and_restore_geometry` |
| Enabling separate Spaces abandons the linked desktop | Seed per-display selections from the current desktop before distributing windows; keep older desktops hidden | `enabling_separate_spaces_preserves_the_live_linked_desktop` |
| Fullscreen over maximize loses the normal session rectangle | Persist the pre-maximize rectangle and reconstruct maximize/fullscreen around it | `saved_fullscreen_over_maximize_unwinds_to_the_original_window` |
| Clicking a pinned window switches Spaces | Focus the visible window without navigating to its membership Space | `clicking_a_pinned_window_keeps_the_current_space` |

The expanded E2E tests exposed two additional lifecycle edges while fixing these
findings. A full disconnect caused late xdg buffer commits to become 1x1 resize
requests; existing windows now retain their accepted size bound while headless.
Retiring a fullscreen Space cancelled the dialog drag at the display boundary;
the migration now preserves the freeform pointer grab through that operation.
A further failing unit test proved that entering fullscreen on a borrowed desktop
changed its home display. The exclusive Space now inherits its origin's home,
and the unequal-display E2E workflow covers entering fullscreen both before and
after disconnect.

Ten new core regressions cover the review cases plus hidden desktop separation,
retained geometry across shape changes and restart metadata, deliberate adoption
of another home display, pinned/maximized headless parking, and fullscreen entered
while borrowed. Two shell regressions cover the session rectangle and checked
home-geometry serialization. The real client fixture creates an actual parented
xdg dialog; the compositor fixture supports two outputs, a 400x300 survivor, and
complete disconnect/reconnect through the production topology path.

See [validation results](mac-mode-validation.md) for final commands, counts,
release artifact identity, and retained evidence. Testing uses isolated nested
Wayland compositors under a GPU-backed Weston host. Physical Apple input, native
KMS hotplug and independent physical refresh rates require hardware qualification.
