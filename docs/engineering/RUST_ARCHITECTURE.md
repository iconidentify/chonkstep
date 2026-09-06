# Rust ownership and engineering boundaries

This is a navigation and review guide for the current implementation, not a
proposal to replace the compositor framework or rewrite working features.

## Keep policy separate from protocols and presentation

`wm-core` owns window-management policy: focus, workspaces, placement and
fullscreen. It receives typed backend events and expresses decisions through
the backend contract. It must not learn Wayland object identities, X11 atom
numbers, renderer details or toolkit-specific coordinate conventions.

`wm-wayland` implements that contract and owns the display server. Its concrete
`Compositor` type is required by Smithay's dispatch/delegate traits; one owner
does not require one implementation file. Protocol handlers and resources live
beside the lifecycle rules that make them safe.

```text
wm-wayland/src/
  state.rs                 bootstrap, shared application state, dispatch ordering
  backend_impl.rs          wm-core backend contract and retained scene records
  input/
    seat.rs                seat dispatch and pointer/touch focus delivery
    surface.rs             explicit surface coordinate transform boundary
    constraints.rs         region-aware pointer lock/confinement and activation
    keyboard/              resolved configuration, focus ownership, repeat state
  selection.rs             native selection ownership and X11 reconciliation
  xwayland.rs              XWM events, X11 focus/access policy, stacking bridge
  xwayland/
    lifecycle.rs           generation-owned connections and bounded restart
    settings.rs            XSETTINGS and shared X resource publication
  session.rs               native session/DRM backend and presentation scheduling
  memory_profile.rs        opt-in allocation diagnostics, absent by default
```

`chonk-shell` owns desktop UI and user-facing shell behavior. `wm-theme` owns
font discovery, rasterization and bounded glyph data. The one shared `FontState`
survives theme changes; theme engines and transient UI need not survive them.
`chonk-testkit` observes real clients in isolated sessions. Its injection door
drives the same input policy as normal backend events; application-side observers
must not bypass that policy with synthetic clicks or direct action hooks.

`chonk-test-support` is a small display-free **dev-dependency**, not part of the
shipping compositor graph. It centralizes per-thread allocation measurement,
with explicit allocator installation, panic-safe scopes and tests that reject
nested or cross-thread-contaminated counts. Budgets state allocation requests,
not RSS. `chonk-instruments::link_panel::terse` separates borrowed field
boundaries from optional unescaping so irrelevant rows can be rejected before
allocating owned UI data.

Shell construction and initial window-manager policy installation share the
live applier, but initialization preserves the already loaded menu. A live
reload still rereads it. Menu action revisions are unique across handle
replacement; object-local counters cannot validate indices in an older popup.

## Rules for changes

- Represent units and ownership at boundaries. A Wayland surface location is
  not an output pixel location, and an X11 window ID is not unique across XWM
  generations. Never infer keyboard ownership from a scene index that is only
  populated on first buffer commit.
- Retire resources where their lifetime ends. Completion, requestor destruction,
  client disconnect and XWayland loss are distinct paths; every retained event
  source, file descriptor, offer and record needs an answer for each applicable
  path. Group generation-owned X11 connections under `xwayland::State`.
- Preserve backpressure. A pipe or peer being temporarily unwritable is not
  transfer completion. Bound staging memory without truncating valid payloads,
  and resume only when the destination can accept more data.
- Keep repaint/input dispatch free of blocking subprocess waits. Reuse the
  event loop and explicit deadlines where possible; do not add an idle thread
  or timer solely to move an existing state-machine transition elsewhere.
- Optimize after capturing a reproducible before case. A smaller source file
  is a maintainability result, not evidence of lower runtime memory. Do not
  alter user-visible behavior or remove fallback paths to improve a number.
- Keep diagnostics out of ordinary timing binaries. Counting allocators and
  heap profilers perturb allocation costs. Read-only cache observation must
  not evict what it is trying to measure.

## Verification contract

`scripts/check.sh all` runs the shared strict Clippy, documentation, debug unit
and Python harness gates. The separate `scripts/e2e.sh --headless --release`
suite drives real nested clients. An immutable executable supplied through
`CHONKSTEP_WAYLAND_BIN` keeps a test result tied to the code actually run while
later work continues. Supply explicit software-renderer variables when that is
the intended setup; headless does not itself mean software rendering.

Freeze harness sources/manifests while Cargo is compiling them, and do not edit
a running shell script: neither Cargo's already-resolved dependency graph nor
Bash's file read offset is a source snapshot. Preserve immutable compositor
binaries **and** the harness revision/provenance used to test them. A compile or
runner-edit error is not a compositor test failure, and is not a green run.

Protocol fixes require negative cases as well as successful requests: denied
focus, stale offers, failed startup, producer/consumer exit, transient I/O and
replacement clients. Repeated runs must preserve unique per-launch evidence;
retrying away a failure is not a stability fix.

Long-running work records hashes, raw measurements, failed fixture attempts and
limitations in dated engineering logs. Native DRM/KMS, hardware-specific gaming,
mixed-GPU behavior and old-machine performance require their own measurements;
passing a nested llvmpipe suite does not establish them.
