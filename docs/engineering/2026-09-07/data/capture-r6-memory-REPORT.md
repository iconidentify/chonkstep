# Capture allocation diagnostics

Complete: **True**. All timing claims use the separate ordinary builds; this report uses only allocation counters.

Three alternating process pairs: `baseline-memory` and `capture-r6-memory`. The identical normal-v2 private fixture has no synthetic memory worker or pressure scope. Its only difference from the preserved original timing harness is atomic diagnostic marker publication outside measured phases.

| Phase (process medians) | Allocation operations before → after | Requested MiB before → after | Operations lower pairs | Requested bytes lower pairs | Live KiB after phase before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| first-open | 19,006 → 2,654 | 3.908 → 1.114 | 3/3 | 3/3 | 5801.328 → 5625.028 |
| warm-open | 4,220 → 1,511 | 5.829 → 0.056 | 3/3 | 3/3 | 5558.016 → 5622.583 |
| overlay-idle | 1,332 → 294 | 0.181 → 0.012 | 3/3 | 3/3 | 5802.160 → 5625.798 |
| toolbar-motion | 6,102 → 17,878 | 0.660 → 26.553 | 0/3 | 0/3 | 5802.207 → 5626.110 |
| selection-draw | 31,344 → 12,954 | 57.224 → 18.031 | 3/3 | 3/3 | 5833.058 → 5641.823 |
| selection-resize | 31,284 → 12,477 | 57.209 → 17.227 | 3/3 | 3/3 | 5833.198 → 5642.347 |
| selection-settled-idle | 726 → 346 | 0.095 → 0.014 | 3/3 | 3/3 | 5832.667 → 5641.831 |
| closed-idle | 708 → 296 | 0.092 → 0.012 | 3/3 | 3/3 | 5588.761 → 5639.542 |

Motion normalization retains actual render counts and the fixed injected event count. Values are whole-process totals over each phase, including the diagnostic protocol.

| Phase | Events before → after | Renders before → after | Requested bytes/event before → after | Operations/event before → after |
| --- | ---: | ---: | ---: | ---: |
| toolbar-motion | 600 → 600 | 1 → 188 | 1,154.158 → 46,404.302 | 10.170 → 29.797 |
| selection-draw | 600 → 600 | 186 → 187 | 100,005.755 → 31,510.750 | 52.240 → 21.590 |
| selection-resize | 600 → 600 | 186 → 186 | 99,979.287 → 30,106.537 | 52.140 → 20.795 |

Every per-process counter vector, paired sign/range, phase live-byte delta, process peak, glibc snapshot and shell glyph-cache snapshot is retained in the allocation-summary JSON. The original toolbar cursor is invisible, so its restored visible cursor changes the work performed.

Interpretation limits:

- These memory-profile binaries are separate instrumentation builds. No timing or CPU conclusion is derived from them.
- Relaxed global counters approximate successful Rust alloc/alloc_zeroed/realloc operations and requested bytes, including the full replacement size for realloc. C allocations and direct mappings bypass them.
- Phase deltas include process-wide background activity and test-door query overhead. They are not stack attribution or exclusive capture-function allocation counts.
- Peak is the process lifetime high-water requested Rust byte count, not a phase-local resident peak. Peak growth may be zero despite large temporary activity below an earlier high-water mark.
- Pre-first-capture snapshots include compositor startup, initial IPC/frame queries and settling; they are not isolated startup allocation measurements.
- Warm phase measures ten open/close cycles; first-open measures only the initial open.
- glibc mallinfo fields are allocator snapshots with arena/thread-cache/version limits. They cannot be subtracted from Rust counters to identify exact C-library ownership.
- Glyph fields come from the shell font-cache statistics endpoint, not an inventory of every capture or library font cache.
- Three matched pairs are a small diagnostic sample. Per-render normalization can include differing frame counts, while per-event normalization holds injected motion count fixed.
- Original toolbar cursor is invisible; toolbar allocation comparisons include different visible behavior.

Raw compact evidence: `/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/comparison-capture-r6-memory-three/allocation-summary.json`.
