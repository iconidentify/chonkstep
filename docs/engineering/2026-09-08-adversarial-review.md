# Adversarial compositor review — 2026-09-08

This review produced six fixes across Wayland presentation, session restart,
sandbox capabilities, shell workers, configuration polling and X11 property
reads. The baseline is `facf999aa92a2bd06f8b411ec64e191bbb20fb6e` (0.4.4 plus
PR #147's resize/logout fixes), not an older build missing those fixes.

## Scope and comparison method

The inventory covers roughly 204,000 tracked Rust/Python/Go/shell lines in 384
files outside `vendor/`. This is a repository-wide architecture and risk review
with deeper investigation of critical paths, not a claim that every line or
hardware combination has been exhaustively verified. Previous September 7
reviews and existing regression tests were checked before selecting new findings;
their already-fixed issues are not counted again here.

| Area | Review performed |
| --- | --- |
| `wm-wayland` | Frame/presentation/FIFO ownership, surface-tree visibility, DRM queue/vblank boundaries, restart environment, protocol admission, grabs and activation, capture limits and lock filtering |
| `wm-core` | Layout/lifecycle invariants and previous solver/geometry audit; workspace regression suite |
| `wm-x11` | Synchronous property reads, class/title/protocol parsing, resize/event paths; new live Xvfb regression |
| `chonk-shell`, config, theme | External process lifetimes, appearance ordering, config-watch allocation, existing IPC/restore limits, raster/render tests |
| Dock protocol and SDKs | Message limits, framing and timeout paths; Rust, Python and Go suites |
| Instruments, utility apps, packaging | Workspace lint/tests, dependency inventory, installer fixtures and CI/test wiring; no new application-specific runtime certification |
| Vendored Smithay | Read the actual 0.7.0 renderer/primary-output, callback and security-context implementations used by this tree; no vendor changes |

Comparisons use primary sources, pinned where possible:

- [Niri, `dd75865`](https://github.com/niri-wm/niri/blob/dd75865f547f0eac0e9b6c4d86d2cd00c0744252/src/niri.rs): actual render-element states determine primary outputs and presentation feedback. Its explicit hidden-client fallback pacing differs from ChonkStep's existing policy of letting hidden windows sleep. ChonkStep now follows actual render visibility while retaining that sleeping policy.
- [Sway, `5bc72de`](https://github.com/swaywm/sway/blob/5bc72dee4771a2d2d2648b8f69d30e0747f263f6/sway/server.c): privileged globals are unavailable to clients admitted through a security context. This directly informed the shared admission-policy fix.
- [Weston output/repaint documentation](https://wayland.pages.freedesktop.org/weston/toc/libweston/output.html): idle, scheduled, deferred and awaiting-completion states are explicit. ChonkStep's existing per-output damage/queue/vblank separation was retained; presentation ownership was corrected inside it.

These are design comparisons, not a speed ranking against those compositors.

## Findings and fixes

### 1. High: sandbox tags did not restrict privileged protocols

`privileged_global_visible` identified confined clients and then returned `true`
for everyone. A real client admitted through `wp_security_context_v1` could see
layer-shell and other desktop-wide capabilities despite its confinement tag.
This was an intentional permissive policy in the previous code, but an unsafe
boundary for applications whose launcher actually supplies a sandbox context.

The shared predicate now rejects confined clients. Normal application protocols
and ordinary desktop-helper access remain available. The live regression checks
normal globals, denied privileged globals, and a forged bind using a numeric
global ID learned from an unrestricted connection. The forged bind must produce
an error, not merely disappear from registry enumeration. Native-only gamma and
output-power globals cannot be exercised by a nested backend.

The surrounding sandbox must prevent direct access to the unrestricted display
socket. This change does not sandbox ordinary same-user processes or XWayland.

### 2. High: nested hot restart reconnected to its own dead display

Startup overwrites `WAYLAND_DISPLAY`/`DISPLAY` so children connect to ChonkStep.
Nested restart preserved those values, so the replacement attempted to use the
compositor socket destroyed by its own exec. The preserved baseline fails the
new test with `winit backend init failed: Failed to initialize an event loop`.

Capture the upstream display addresses before startup changes them and restore
them for nested exec. Native restart still removes display addresses and pins
DRM. Never reuse a consumed `WAYLAND_SOCKET` connection. Tests cover both display
address kinds, absent values, native selection, two real successive nested execs,
autostart signal delivery and clean logout afterward.

An FD-only host connection with no reconnectable display address remains outside
the tested restart contract; replaying the same initialized Wayland stream after
exec would be invalid.

### 3. Medium: invisible surfaces were paced and falsely reported as presented

The old primary-output calculation chose the monitor with maximum rectangle
intersection even when every intersection was zero. It also assigned every
subsurface and popup its parent's output, ignoring rendering transforms and
occlusion. An unrelated visible frame could therefore report an offscreen
window's buffer as presented and request another invisible frame.

Presentation collection now updates Smithay's per-surface primary output from
actual render results. Frame callbacks and FIFO presentation-barrier release use
that output. The same logic serves nested swaps and native queued frames/vblank;
capture-only renders do not update display presentation ownership. Cursor
surfaces participate in the same feedback/visibility collection.

Real-client regressions cover offscreen sleep/reveal, opaque occlusion/uncover,
and a visible subsurface extending from an offscreen parent, then the reverse.
Existing FIFO, commit-timing and locked/hidden-workspace tests remain in the gate.
The new occlusion fixture was visually reviewed: its first version used an
ambiguous substring selector and moved the wrong window. Exact selectors and
separate names fixed the fixture before accepting its result.

### 4. Medium: appearance switches created unbounded competing workers

Each switch spawned a thread whose `gsettings` `.output()` calls had neither a
deadline nor an output limit. A stalled settings service retained one thread per
switch; concurrent writes could finish in the wrong order.

One lazy worker now serializes writes and retains one replaceable pending mode.
It uses the existing bounded subprocess reader, with a two-second deadline per
helper and the reader's 4 MiB output cap. Managed-theme adoption and the nested
session opt-out remain intact. The concurrency regression holds one operation
in progress, submits 20,001 subsequent requests, and verifies that only the
latest pending mode runs next. Existing reader tests cover hangs, pipe overflow,
nonzero exits and descendants holding stdout open.

### 5. Low/performance: unchanged config polls cloned every watched path

Every one-second poll built two vectors and cloned all paths despite an unchanged
watch set. With 256 files, 20 polls requested **5,240 allocations / 646,000 bytes**.
The new implementation reuses paths and updates scalar metadata in place. The
same measured operation now requests **zero allocations / zero bytes**.

All files and directories are still sampled even after detecting a change;
rename-over inode changes, deletion, cadence and following new source files retain
their previous semantics. Filesystem `stat` calls are intentionally unchanged.
These numbers describe allocation requests, not RSS savings or latency.

### 6. Medium: X11 property replies had effectively unbounded requested sizes

Title, class and atom-list requests used `GetProperty(long_length = u32::MAX)`.
A client-controlled property could therefore make the WM receive and allocate
arbitrarily large replies before parsing them on the desktop thread.

Requests now cap replies at 64 KiB, accounting for X11's four-byte length units.
Titles accept a bounded prefix; oversized identities and atom lists are rejected
so partial data cannot invent an application-rule match or protocol capability.
String-format checks reject malformed values. The live Xvfb regression compares
a 128 KiB property with the 64 KiB bound and checks normal UTF-8 titles/classes,
oversized protocol/state atom lists and ordinary protocol/modal handling.
CI now runs this regression on its own private Xvfb display.

## Validation and measurements

[Raw measurements and environment](2026-09-08-audit-metrics.json) include all
paired samples, summary ranges and exact executable SHA-256 hashes. Baseline and
candidate binaries were preserved separately. Only explanatory comments and
review artifacts changed after the measured build; the artifact records the
pre-cleanup hashes for those two source files.

| Workload | Baseline | Candidate |
| --- | ---: | ---: |
| Offscreen client redraws, 120 visible frames, each of three paired runs | 120 | 0 |
| Compositor dispatches, median of those runs | 383 | 260 |
| Visible presentations in every run | 120 | 120 |
| Allocation requests, 20 unchanged polls / 256 config files | 5,240 | 0 |
| Requested bytes for those polls | 646,000 | 0 |
| Idle CPU range, five runs | 0–0.2% | 0–0.2% |
| Idle PSS range | 137,214–138,179 KiB | 137,066–137,948 KiB |
| Idle descriptors / threads | 46 / 40 | 46 / 40 |

The hidden client does no further redraw work, and median compositor dispatches
fall by 32%. Both cases remain paced by the visible animation. Render/dispatch
wall durations include nested presentation waits and are not CPU-time or
input-latency measurements. Idle CPU quantization is approximately 0.2 percentage
points for these five-second samples; the baseline median is 0%, candidate 0.2%.
The overlapping ranges do not establish an idle CPU, memory or startup gain.
Unrelated background workloads were left running.

Validation completed:

- Strict workspace Clippy, with disallowed blocking calls and undocumented
  unsafe blocks denied; Rustdoc with warnings denied.
- 2,057 Rust test successes including doctests, plus the private Xvfb oversized
  property regression. Wayland and appearance unit suites rerun after self-review.
- Full release-mode Wayland suite: 271 integration tests and three installed
  Omarchy fixture checks; no missing-client skips. This includes real Chromium,
  capture, clipboard, input constraints, geometry, locking, spatial layouts and
  desktop-churn tests, along with the new regressions.
- 65 Python harness tests; 44 Python SDK cases (seven intentional abstract-base
  skips); Go SDK `go test ./...`; disposable Omarchy installer fixtures.
- New restart, offscreen-presentation, security-context and X11-property
  regressions demonstrated the original failures before their fixes. The config
  allocation test recorded its failing baseline before implementation changed.

Reproduction commands:

```sh
scripts/check.sh
scripts/e2e.sh --headless --release
xvfb-run -a cargo test --locked -p wm-x11 hostile_properties -- --ignored
python3 -B -m unittest discover bindings/python/tests
mise exec go@1.27.0 -- go test ./...  # from bindings/go/chonkdock
CHONKSTEP_WAYLAND_BIN=/absolute/preserved/binary scripts/e2e.sh --headless --release --test surface_pacing offscreen_animation_work_profile --nocapture
python3 scripts/bench-compositor.py --binary baseline=/absolute/baseline --binary candidate=/absolute/candidate --runs 5 --settle-seconds 2 --idle-seconds 5 --output /tmp/cs-audit-idle
```

All live tests and measurements used private display/bus/configuration contexts.
The running desktop was not restarted or replaced.

## Remaining work and limits

- Native DRM/KMS, mixed-refresh monitors, hotplug, VT switching and GPU-reset
  recovery require hardware validation. Nested rendering and shared-path unit
  tests cannot establish physical presentation timing or input-to-photon latency.
- X11 still performs synchronous property round trips. Bounding replies limits
  resource consumption; pipelining startup atom/property requests needs its own
  latency experiment and careful event-order review.
- Activation admission currently caps/expires tokens but does not implement
  a user-serial-based focus-stealing policy. That is a separate behavior
  change requiring launch/notification compatibility tests.
- `cargo audit` reports no advisories in its vulnerability category, but does
  report unmaintained `cgmath` and `ttf-parser`, plus the `cgmath::swap_columns`
  identical-index unsoundness advisory. There are no `swap_columns` call sites in
  our crates or vendored Smithay. Dependency migration remains necessary; the
  absence of a direct call is not a general dependency safety certificate.
- End-user Omarchy configuration, installed binaries, login services and release
  publication were not changed. The fixes are reviewable repository changes.
