# 0.5.0 release review

This review covers the unpublished Mac/Spaces and GPU work based on `c7dad60`,
including the existing nineteen development commits through `4d7be2a`, and the
release fixes below. The new Spaces miniatures are part of 0.5.0. This is a
prerelease through `preview-v0.5.0`, not a claim of complete macOS compatibility.

## Defects reproduced and fixed

| Trigger | Defect | Fix and executable regression |
| --- | --- | --- |
| An input method replaces its keyboard grab, then destroys the old object | Old destruction could unset the replacement and clear its state | Match the active protocol object and installation serial; `keyboard_focus::retiring_an_old_input_method_grab_preserves_its_replacement` failed before and passes after |
| A translated key is held while focus changes through an input method | The translated release could reach the new focus | Retire the forwarded-key ledger entry before entering the IME; `keyboard_focus::a_translated_release_cannot_cross_focus_through_an_input_method` failed before and passes after |
| A projected IME chord is followed by an ordinary key without a modifier delta | Projected modifiers could remain in the IME stream | Restore current physical modifiers on leaving projection |
| Enable Mac mode after a client already owns the clipboard, then quit that client | Persistence had never observed the earlier selection | Adopt the existing native or XWM offer on the enable transition; both `selection_transfer::enabling_mac_preserves_an_existing_*_clipboard_owner` tests failed before and pass after |
| Open Overview with windows on several desktops | Desktop strip showed wallpaper/counts instead of the actual windows | Render shared window surfaces in desktop geometry, clipped and stacked; new native and XWayland pixel regressions |
| Scene imports run after querying a nested EGL backbuffer age | NVIDIA recordings could retain dimmed strips on some reused buffers | Complete imports and GPU-timer setup before latching/querying the backbuffer; verify every frame during a stable capture hold |
| X11 client sets its title before mapping and never changes it | The cached title remained empty | Initialize the title cache when creating its record; exercised by the XWayland miniature workflow |
| A browser or terminal changes its title during an Overview drag | Semantic refresh cancelled the held gesture | Preserve the gesture and its visual when card geometry and desktop identities are unchanged; invalidate layout or target changes |
| Record RGB desktop content through newer wf-recorder/FFmpeg | Limited-range YUV was tagged full-range, lifting blacks and compressing highlights | Set the encoder's color range explicitly; real screen/region recording failed with red 226 instead of 240 before and passes the RGB-level assertion after |
| Reuse a nested EGL window after offscreen capture | An offscreen framebuffer could still be bound when querying window buffer age; a recording retained a selection edge | Bind the default framebuffer before latching/querying the window target; three consecutive recordings passed all 135 checked frames and nine complete overlay-image comparisons |

The clipboard tests toggle the live profile three times and verify exact UTF-8
text after the original owner exits. The Mac key regression also toggles eight
times in one real client and observes Control versus Meta key delivery plus
ordinary typing afterward. Physical Control remains independent of Command.
Existing keyboard-focus, real-Fcitx, native/X11 selection transfer, reload,
fullscreen, hotplug, capture and rendering tests remain part of the suite.

Two existing fullscreen pixel assertions assumed that publishing WM geometry
meant the client had already acknowledged its configure and painted a new buffer.
They now poll the same exact expected pixels with the existing bounded deadline.
No compositor frame or image requirement was relaxed.

## Spaces miniatures

The strip draws actual Wayland/XWayland surfaces and existing frame textures;
there are no new full-desktop captures. Window membership refreshes when the WM's
semantic revision changes. Geometry is read from the current scene, and each
miniature is clipped to its owning output. Pinned windows are included in their
display's desktops; minimized and application-hidden windows are excluded.
Fullscreen Spaces contain the actual fullscreen client.

The original candidate passed the complete local E2E suite and 2,107 Rust tests
plus 86 Python harness tests. Subsequent video QA found a nested repaint defect
that the screenshot-based suite did not catch: 10 of 45 video frames retained a
dark strip, although the saved PNG was clean. Texture imports could switch to a
surfaceless EGL context between querying buffer age and drawing. Both single-head
and virtual-head paths now finish imports before latching/querying the target;
timer setup follows the same rule. Incremental rendering remains enabled.
The patched recording passed all 45 sampled frames (maximum header variation
one level out of 255). The runner now rejects stale, blank, truncated, and
undecodable capture evidence. Four harness regressions protect that verification.

The twenty `mac_spaces` workflows exercise the candidate in an isolated GL
host with two real `wl_output` heads. New assertions cover window positions and
colors on separate desktops, live repaint on a parked desktop, XWayland repaint,
edge clipping, real pointer drag between desktops (including title updates), close/removal, pinned and
minimized membership, fullscreen creation/removal, and stopping parked client
callbacks when Overview closes. The pre-feature miniature regression fails on
the earlier executable.

Smithay's damage tracker retains multiple instances of the same surface ID, so
main cards and miniatures share texture identities safely. Actual render-element
visibility still gates frame/presentation feedback. The inactive-preview visitor
deduplicates ordinary scene and gesture-neighbor visits; closing Overview restores
the ordinary visible-scene path.

## Validation and media

Reproduce the required gates:

```sh
scripts/check.sh all
scripts/e2e.sh --headless --host-renderer gl
```

CI explicitly installs the real-Fcitx/GTK test dependencies and Pillow for
benchmark image verification; the first remote SDK run exposed the missing
Pillow dependency.
The remote Wayland run also exposed Ubuntu's Chromium user-namespace restriction:
the Mac fixtures now use the same CI-only private-page SwANGLE launch settings as
the existing browser tests and report early process exits immediately. Nautilus
readiness now requires a dark fixture background plus file/text pixels; a blank
white loading surface previously satisfied the test's brightness-only predicate.
Ubuntu 24.04's Nautilus 46.4 reproduced a second readiness race locally. The file
workflow now observes the selected item, reads the actual offered file URI, and
waits for the destination view before one Paste. Writer likewise must paint its
fixture paragraph, not just a white loading surface, before one Copy. The exact
copied file bytes and bidirectional Writer/browser text assertions remain intact.

The first command runs strict Clippy, private Rustdoc, workspace and Wayland unit
tests, and the Python harness tests. The second uses real clients and production
input/protocol/rendering paths. The E2E compositor can be pinned with
`CHONKSTEP_WAYLAND_BIN=/absolute/path/to/preserved/binary` so a concurrent rebuild
cannot silently change the executable under test.

[Product-demo tooling](../product-demos.md) creates isolated 1080p recordings for
capture and Spaces, with real Foot/GTK fixture windows. Each run records its
executable version and hash, action timeline, output hashes, full video-decode
checks and ffprobe metadata. Spaces additionally records actual membership
checkpoints. Captioned share videos retain the same uncut footage; the original
recording is included separately. Final publication must use the release binary,
not a relabeled rehearsal.

Final video QA also compares decoded dark pixels with the independent screenshot.
The old recording path changed the fixture's RGB `[240, 20, 20]` to `[226, 32, 32]`.
The worker and demo recorder now explicitly convert RGB to limited-range YUV and
label it correctly. Older Ubuntu wf-recorder/FFmpeg otherwise used a full-range
conversion; its real regression failed before the explicit conversion and passed
after. The real recording workflow checks RGB fidelity for
both screen and area modes; the demo verifies raw and captioned video against its
PNG. Two harness regressions reject uniformly washed-out video even when every
frame has consistent pixels.

A later recording retained a thin selection edge in both the parent screenshot
and video. The nested target setup now also restores the default framebuffer
before making the window surface current and querying its age. Incremental
rendering remains enabled. The demo compares each complete capture-overlay PNG
with an independent full repaint, excluding only the fixture's small pulsing dot.
The failing image is rejected by this check; all nine overlay pairs in three
consecutive corrected recordings matched exactly outside that dot.

## Limits and defaults

Saved captures now open in both keyboard profiles: screenshots in imv and
finalized recordings in Omacut. The Mac regression first reproduced the missing
viewer, then verified exact literal file arguments, publication before launch,
and no viewer for clipboard-only captures. Desktop-mode workflows cover launch
failures and ensure failed saves never launch an app. The reusable demo also
opens the actual installed applications and displays the saved PNG and MP4.

Release CI also exposed the completed-paste test's process-memory heuristic:
5,128 KiB of anonymous growth exceeded its 4 MiB allowance. The regression now
inspects retained XWM transfer records directly, requiring zero after 128 pastes
while every requestor stays alive. Process memory remains recorded as diagnostic
evidence. A held transfer must report one; a deliberately broken test executable
that skips completed-transfer removal reports 129 and fails. The read-only
diagnostic neither cleans up records nor exposes clipboard contents.

The accelerated-path flags remain separate, opt-in experiments. Hardware results
are in the [cross-GPU report](../benchmarks/cross-gpu-2026-09-10/README.md), including
the Apple scanout regression and test conditions. Nested tests establish protocol,
input and pixel behavior; they do not prove every native DRM plane/driver
combination. Existing physical-machine benchmark data must not be represented as
new measurements of the thumbnail change.

The Mac profile is optional. Existing terminal bindings must retain their native
copy/paste actions; tested Foot bindings and input-method behavior are documented
in [Mac mode](../mac-mode.md). No finite test run guarantees compatibility with
every application, keymap, monitor topology or third-party input method.
