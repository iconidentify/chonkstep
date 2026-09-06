# Compositor compatibility audit

Working matrix, started at baseline `1e6db21`. This is not a parity claim.
Reference revisions and licenses are in [WORK_LOG.md](WORK_LOG.md). A registry
global proves discovery only; a unit test proves a policy only; a nested test
cannot prove a KMS page flip, VRR, explicit synchronization or hardware latency.

## Contracts and comparison method

Use the [Wayland core specification](https://wayland.freedesktop.org/docs/html/apa.html)
for client-visible semantics, the pinned Smithay 0.7 implementation for the
actual framework behavior, and upstream compositor source to identify testable
design choices. Implement independently rather than copying another project's
policy or assuming its advertised feature is appropriate for ChonkStep's UX.

- Pointer and touch positions are surface-local. Buffer scale and viewport
  scaling cannot be represented by subtracting a global pixel origin alone.
- Keyboard repeat rate/delay must be nonnegative; rate zero disables repeat.
  Clients consume updates after initialization as well as the initial event.
- Clipboard transfer, primary selection, drag-and-drop, clipboard persistence
  and clipboard history are different capabilities. Test each separately.
- Interactive requests and selection access have seat/focus/serial boundaries.
  Convenience must not accidentally permit a background client to steal input.

## Initial matrix

| Capability | ChonkStep source / existing evidence | Campaign work still required |
| --- | --- | --- |
| Integer/fractional-scale input | Typed surface-coordinate boundary; 12 native root/subsurface pointer, native-DnD and touch cases pass at 1x/1.5x/2x; real Edge and Chromium selection/held-arrow matrix passes all three scales, windowed and fullscreen | Mixed-output transitions, popup, confinement and cursor-hint edge cases remain |
| Keyboard layouts and repeat | 13 native/Foot/X11/settings/reload regressions and seven modal/focus cases pass; Caps Lock/layout retained on no-op reload; modal ownership and first-held Alt-Tab tested | Exclusive layers/focus grabs interrupting a modal hold, active IME composition and complete lock/inhibition boundaries; browser-specific held-repeat case |
| X11 window lifetime | Thirty first-key/retirement cycles, ten withdraw/remap cycles and twelve-window exit pass; XWayland restart reuses surface serials safely under generation-qualified identity, including actual first-key delivery and outstanding selection cancellation | More restart churn, override-redirect ancestry and native-session server-loss behavior |
| Wayland/X11 clipboard and primary selection | Nineteen real native/X11 cases pass: UTF-8/binary/large INCR both directions, early ownership, replacement/clearing, backpressure in both directions, slow native consumers, cancellation, producer exit, XWayland restart/dead offers, unfocused-access denial and completed-buffer retirement | Unusual MIME combinations, more concurrent owner/requestor lifetimes, history/persistence and application-level interop; no claim that clipboard success proves DnD |
| Native drag-and-drop | Delegated Smithay data-device grabs; no-op client/server DnD callbacks | Negotiation, coordinates, completion/cancellation, icon presentation and file-manager/browser interoperability |
| X11↔Wayland drag-and-drop | `xwayland.rs` explicitly distinguishes selection bridge from DnD | Implement/verify the missing bridge separately; clipboard success is not DnD success |
| XDG popups and interactive requests | Popup tracking/reposition and existing real-browser tests; popup grab callback currently declines to implement grab semantics; move/resize comments acknowledge unchecked serials | Valid/invalid serials, dismissal, nested grabs, output-edge unconstraining and focus restoration |
| Fullscreen / gaming input | Real Chonkcraft/JBR menu clicks and fullscreen restoration pass at 1x/1.5x/2x on both baseline and candidate; explicit lock/confine regions, null-region motion, committed-region changes and Overview release pass; 8,192 exact locked raw deltas produce no render attempts | In-battle Chonkcraft/Alt-F and hardware pacing; scaled hints, confinement clipping, multiple buttons, additional focus-loss and shortcut-inhibition boundaries |
| Presentation and frame pacing | Presentation feedback, FIFO and commit-timing code in renderer/session/core protocols | Protocol completion under visibility changes; frame-time distributions; native KMS validation; update stale comments contradicting implemented presentation support |
| Scanout / VRR / explicit sync | Native-session scanout policy, VRR state, syncobj advertised only with required device support | Actual DRM/KMS and GPU matrix; no nested result counts as hardware validation; investigate tearing and color-management gaps separately |
| Omarchy / Hyprland integration | Config and IPC adapter crates; globals and end-to-end tests for Quickshell/Omarchy-facing features | Inventory commands/settings/events used by installed Omarchy; fail visibly for unsupported options instead of silently claiming a full drop-in |
| Lock / remote input / capture | Session-lock input domain, virtual input, screencopy and image-copy capture, existing lifecycle tests | Re-run after focus-type changes; selection and remote-input access boundaries; portal and screenshare lifecycle |
| Old / mixed hardware | Prior llvmpipe, GLES2 and nested Intel/NVIDIA samples | Preserve fallback behavior; investigate known nested NVIDIA/Alacritty failures; real old-hardware, AMD, hotplug and suspend/resume remain unvalidated |

## Upstream observations to turn into tests

- [Smithay 0.7 pointer grab](https://github.com/Smithay/smithay/blob/v0.7.0/src/input/pointer/grab.rs)
  retains press-time focus/origin. Its data-device DnD implementation instead
  derives wire coordinates from each current focus. The adapter must satisfy
  both contracts; replacing only hover math cannot fix held-button drags.
- [Niri click-grab policy](https://github.com/niri-wm/niri/blob/dd75865f547f0eac0e9b6c4d86d2cd00c0744252/src/input/click_grab.rs)
  explicitly treats movement during scrolling-to-focus as a UX tradeoff. This
  does not justify incorrect scale conversion: distinguish changing window
  position during a grab from moving the pointer over a stationary surface.
- [Sway keyboard configuration](https://github.com/swaywm/sway/blob/5bc72dee4771a2d2d2648b8f69d30e0747f263f6/sway/input/keyboard.c)
  compares repeat settings before updating them. Test ChonkStep's reload path
  for unnecessary keymap/repeat reconfiguration and held-key cancellation.
- [Sway output submission](https://github.com/swaywm/sway/blob/5bc72dee4771a2d2d2648b8f69d30e0747f263f6/sway/desktop/output.c)
  includes an explicit tearing policy and fallback when the output rejects it.
  Discovering an async/VRR capability is not proof a submitted frame used it.
- [Chonkcraft's input handlers](https://github.com/iconidentify/chonkcraft/blob/fe7c787c936bd7c44647baf8e3b3ae063a7889ec/desktop/src/main/java/net/chonkbase/chonkcraft/desktop/GameScreen.java)
  distinguish AWT-normalized positions, interface scaling and map scaling.
  Their comments describe prior game-side double-scaling fixes. Record the
  tested game/runtime revision and window backend before assigning blame.

Hyprland is checked out at a pinned revision for further source comparisons;
no measured performance superiority over Hyprland has been established.

- [Hyprland's XWM selection-access check](https://github.com/hyprwm/Hyprland/blob/ab136393c2eb9e106846a704da1a1b3d6af415b4/src/xwayland/XWM.cpp)
  checks the seat's focused Wayland client against its XWayland client, not a
  post-commit window index. ChonkStep's typed keyboard focus now supplies its
  live XWM identity directly, retaining a generation-specific access boundary
  even before the first buffer commit. The negative-focus regression prevents
  first-paste compatibility from being obtained by granting background access.

## Focus contracts clarified during testing

- Compositor modal UI must take real client keyboard focus away, not merely
  filter later key presses. Niri likewise represents Overview/MRU focus as a
  distinct owner without a client surface. ChonkStep now restores the same
  window after modal cancellation even when window-manager focus never changed.
- A workspace entered from an empty workspace still needs to choose its own
  most recent eligible window. The old shared-core path skipped that choice
  when there was no previous client; unit and real keyboard/text-input tests
  reproduce and now cover the correction.
- Smithay 0.7's `SeatHandler::focus_changed` promises every change but omits
  removal to `None`. A real inhibited client receives keyboard leave on
  self-minimization but no inhibitor-inactive before the narrow correction.
- Text-input has its own Smithay enter/leave handling and requires an actual
  input-method instance before text-input enter is sent. The regression uses
  a private input-method-v2 peer; an absent IME is not counted as a compositor
  focus failure. These tests do not yet exercise composition/preedit/commit.

## Ownership and gaming-input findings

- [Smithay's upstream selection-device cleanup](https://github.com/Smithay/smithay/commit/712d565b4b8bf34a136f42b39fcbf729ab635d3f)
  retires all four protocol device types on connection teardown, not only on
  explicit destruction requests. A narrow attributed backport is covered by
  10,240 object lifetimes including SIGKILL and legacy core clients. This does
  not establish clipboard history, persistence or X11/native DnD support.
- [Niri's relative-input path](https://github.com/niri-wm/niri/blob/dd75865f547f0eac0e9b6c4d86d2cd00c0744252/src/input/mod.rs)
  forwards locked relative motion without ordinary absolute-pointer routing.
  ChonkStep now checks a separate render-attempt counter: zero submissions alone
  hid one discarded scene rebuild per input report. Constraint-region checks
  run outside Smithay's surface-state mutex; null-region confinement previously
  reentered that mutex and hung. These tests establish neither native scanout
  nor a hardware polling-rate/latency result.
- The full pointer checkpoint exposed a real-browser parent-resize race:
  a stale commit can replace a newly staged IPC resize. A controlled real
  client reproduces it immediately; recognizing staged configure debt fixes
  all sixteen rounds. The later incoming-owner checkpoint passes a complete
  suite; the earlier failures remain preserved with their distinct causes.
- xdg interactive move/resize now treats the request serial as authority, as
  the protocol requires: owning seat, exact active pointer grab and same client
  are checked before wm-core sees the request, and DnD grabs are not repurposed.
  The old handler admits five invalid classes; an eight-case real-client matrix
  now rejects zero/stale/wrong/cross-client/DnD serials and preserves legitimate
  client-decorated move and resize. Touch/tablet-initiated interactive requests
  are still a declared parity gap rather than being misrouted through the mouse
  pointer position.
- Menu replacement previously reused revision 1, permitting an older popup's
  index to resolve a different command. Process-unique revisions now reject
  it. Initial policy installation also avoids a duplicate menu parse while
  explicit reload retains its reread contract. Network-panel source completions
  are consumed independently of tile sample freshness, without forcing tile
  redraws.
