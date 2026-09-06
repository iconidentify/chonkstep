# Allocation attribution without changing ordinary builds

Status: the September 5 campaign's diagnostic build, accounting/parser/cache
unit tests and strict feature Clippy checks pass. Runtime counters are live;
two-hour attribution is in progress, not a completed leak diagnosis.

A five-minute heaptrack capture has identified a missing selection-device
disconnect cleanup, corroborated by Smithay's upstream fix. The campaign
tracks its isolated lifecycle regression separately from whole-process memory
drift. Heaptrack's end-of-capture “leaked” total is not a product leak estimate:
the nested harness kills the compositor, leaving normal live allocations too.

The optional `memory-profile` feature installs a `System`-forwarding Rust
allocator with atomic size counters in `chonkstep-wayland`. The library does
not replace another application's allocator merely because its feature is
enabled. Ordinary default builds contain no allocation-counting wrapper.
The diagnostic binary identifies itself in `--version` and startup logs.

```sh
CARGO_BUILD_JOBS=4 cargo build --release -p chonkstep-wayland --features memory-profile
sha256sum target/release/chonkstep-wayland
```

Preserve this executable separately from every production/performance binary.
Do not overwrite an executable supplying a running baseline or soak. Use an
absolute preserved path through `CHONKSTEP_WAYLAND_BIN` when testing it:

```sh
profile_scratch=$(mktemp -d /tmp/cmp.XXXXXX)
TMPDIR="$profile_scratch" LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
  CHONKSTEP_SOAK_MEMORY_STATS=1 CHONKSTEP_SOAK_SECONDS=7200 \
  CHONKSTEP_WAYLAND_BIN=/absolute/path/to/preserved-profile-binary \
  scripts/e2e.sh --headless --release --test stability_soak --nocapture
```

The test uses private Weston, compositor, D-Bus and client sessions. It does
not restart the live desktop. Longer campaigns use separate bounded two-hour
runs. Explicit software-renderer variables keep driver discovery comparable
to the campaign's earlier software soak; `--headless` alone is not a renderer
selection. `memory-stats` exists only in a memory-profile build and only through
the explicitly enabled test door. A normal build rejects it; the harness
rejects missing counters instead of reporting zero allocations.

## What each view means

| View | Meaning and limits |
| --- | --- |
| `rust_live_bytes` | Outstanding requested sizes through the Rust global allocator; excludes allocator overhead, direct mappings and C-library allocations |
| `rust_peak_bytes` | Highest observed outstanding requested size, not peak resident memory or temporary allocator-internal realloc copies |
| `rust_operations`, `rust_allocated_bytes` | Cumulative successful alloc/zeroed-alloc/realloc operations and requested sizes; realloc counts the new request size |
| `glibc_*` | Reported mallinfo2 arena/in-use/free/mapping/top-block counters; not an exact ownership census across all libraries or thread caches |
| `glyph_*` | Image/outline entry counts and payload capacity; excludes hash tables, scaler state and loaded font objects |
| `font_faces` | Available font-database faces, not loaded fonts |
| `/proc` anonymous PSS | Resident anonymous pages attributable to the process; includes Rust, libraries and allocator retention, and is not requested heap size |

Rust fields use relaxed atomic loads and are not one transaction across
threads. Do not assert an exact cross-field identity from a concurrent sample.
No allocation payloads, addresses or stack traces are recorded. Glyph
observation does not trim the cache, and allocator observation does not call
malloc_trim or alter allocation policy.

The [glibc manual](https://sourceware.org/glibc/manual/latest/html_node/Statistics-of-Malloc.html)
defines the allocator fields. The [Linux mallinfo2 manual](https://man7.org/linux/man-pages/man3/mallinfo.3.html)
warns about arena visibility and allocator-state concurrency. Do not subtract
Rust live bytes from mallinfo2 and label the difference exact C-library memory.
Sampling occurs during ordinary event-loop execution, not a signal handler or
concurrently with ChonkStep's initial mallopt setup.

`CHONKSTEP_SOAK_SELECTION_STATS=1` additionally observes the retained core,
primary, wlr-data-control and ext-data-control device ledger after client
teardown. It asserts that no dead resources remain. This requires a binary
with the new `selection-devices` test-door command; omit it for earlier
preserved executables. Observation never performs cleanup on the test's behalf.

Use trends together: bounded Rust/cache bytes with rising anonymous PSS calls
for C-library/direct-mapping/allocator analysis; rising Rust bytes calls for
ownership and cache attribution. Neither pattern alone proves a leak.
The counter overhead changes execution cost, so this binary is unsuitable for
startup/idle-CPU/frame-time comparisons with a normal build. Continue those
comparisons with preserved, uninstrumented executables and quiet workloads.
`scripts/bench-compositor.py` rejects the diagnostic marker in `--version`
before starting its display host or collecting samples.
