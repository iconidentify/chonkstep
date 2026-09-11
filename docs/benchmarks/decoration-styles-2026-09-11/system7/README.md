# System 7 renderer measurements

Release builds on i9beef, 2026-09-11. Seven alternating runs per build/style.
`performance --style STYLE`; no overlapping builds, tests or other benchmark
jobs. Candidate renderer source is in commit `4f8de18`. Times below are
median [min–max]; raw samples retain allocation counts/bytes, retained pixels
and checksums. Allocations are counted by the example in both executables.

| Client / scale | Before WM layout ns | After WM layout ns | System 7 layout ns |
| --- | --- | --- | --- |
| 800×600 / 1 | 186.00 [181.00–191.00] | 81.00 [80.00–82.00] | 82.00 [80.00–84.00] |
| 800×600 / 2 | 186.00 [184.00–210.00] | 86.00 [86.00–87.00] | 87.00 [86.00–93.00] |
| 1280×800 / 1 | 179.00 [177.00–214.00] | 80.00 [80.00–80.00] | 80.00 [79.00–89.00] |
| 1280×800 / 2 | 185.00 [184.00–191.00] | 87.00 [86.00–87.00] | 87.00 [86.00–91.00] |
| 2560×1600 / 1 | 178.00 [176.00–193.00] | 80.00 [80.00–80.00] | 80.00 [79.00–91.00] |
| 2560×1600 / 2 | 185.00 [183.00–236.00] | 86.00 [86.00–102.00] | 87.00 [86.00–88.00] |

## Cold paint (µs)

| Client / scale | Before WindowMaker | After WindowMaker | System 7 |
| --- | --- | --- | --- |
| 800×600 / 1 | 98.53 [95.72–101.98] | 93.09 [92.41–94.35] | 36.02 [35.25–38.93] |
| 800×600 / 2 | 127.10 [124.44–162.03] | 127.69 [126.51–129.31] | 66.47 [66.16–78.93] |
| 1280×800 / 1 | 96.55 [94.74–98.68] | 95.11 [94.26–95.77] | 44.35 [43.88–58.30] |
| 1280×800 / 2 | 167.81 [166.22–214.60] | 169.81 [166.89–176.65] | 100.93 [99.22–142.65] |
| 2560×1600 / 1 | 157.81 [155.48–201.45] | 155.66 [153.09–184.10] | 77.16 [75.68–80.31] |
| 2560×1600 / 2 | 277.15 [272.56–286.28] | 272.09 [269.91–317.53] | 168.51 [164.82–210.22] |

## Warm paint (µs)

| Client / scale | Before WindowMaker | After WindowMaker | System 7 |
| --- | --- | --- | --- |
| 800×600 / 1 | 8.64 [8.40–8.77] | 8.31 [8.07–8.52] | 1.81 [1.77–1.89] |
| 800×600 / 2 | 16.33 [15.95–20.10] | 15.88 [15.77–15.94] | 3.71 [3.62–4.28] |
| 1280×800 / 1 | 13.27 [13.06–13.33] | 12.38 [12.30–12.52] | 2.79 [2.75–3.45] |
| 1280×800 / 2 | 26.22 [25.67–26.74] | 24.95 [24.55–28.96] | 6.05 [5.92–6.39] |
| 2560×1600 / 1 | 26.34 [25.96–29.92] | 24.88 [24.28–25.61] | 6.14 [5.94–6.45] |
| 2560×1600 / 2 | 50.62 [49.55–51.95] | 48.28 [47.15–57.51] | 14.17 [13.62–15.60] |

Both layouts now take two allocations: WindowMaker requests 360 bytes, System 7
280 bytes. WindowMaker reserves its former final 16-slot resize capacity directly,
avoiding two reallocations (the baseline requested four allocations/600 bytes).
System 7 caches scale metrics and copies its bounded rectangle array directly.
Its occasional 1 ns median difference from WindowMaker is within overlapping
sample ranges.

WindowMaker warm painting now makes six allocations instead of eight. Its
resize band is painted at the final size, eliminating a cloned request and a
second pixel vector used only to crop a temporary border. Every baseline golden
still matches. System 7 warm painting also uses six allocations. Neither owned-
buffer API is allocation-free; both retain only perimeter-sized chrome.

## Allocation and storage details

| Client / scale | Before WM cold calls / bytes | After WM cold calls / bytes | System 7 cold calls / bytes |
| --- | --- | --- | --- |
| 800×600 / 1 | 47 / 321100 | 45 / 288985 | 31 / 140092 |
| 800×600 / 2 | 65 / 633158 | 63 / 568803 | 31 / 276732 |
| 1280×800 / 1 | 47 / 499340 | 45 / 448025 | 31 / 219292 |
| 1280×800 / 2 | 65 / 989638 | 63 / 886883 | 31 / 435132 |
| 2560×1600 / 1 | 47 / 976780 | 45 / 874265 | 31 / 433692 |
| 2560×1600 / 2 | 65 / 1944518 | 63 / 1739363 | 31 / 863932 |

| Client / scale | WM warm bytes | System 7 warm bytes | WM retained pixels (bytes) | System 7 retained pixels (bytes) |
| --- | --- | --- | --- | --- |
| 800×600 / 1 | 110859 | 74847 | 110664 | 74652 |
| 800×600 / 2 | 222051 | 150003 | 221856 | 149808 |
| 1280×800 / 1 | 175819 | 117567 | 175624 | 117372 |
| 1280×800 / 2 | 351971 | 235443 | 351776 | 235248 |
| 2560×1600 / 1 | 351179 | 234687 | 350984 | 234492 |
| 2560×1600 / 2 | 702691 | 469683 | 702496 | 469488 |

Retained pixel bytes exclude Vec metadata/capacity and GPU buffers. Cold peak
growth includes title caches retained by the benchmark’s disposable engines.

## Unchanged surrounding workloads

| Workload (WindowMaker) | Before µs | After µs |
| --- | --- | --- |
| clock-224 | 354.31 [348.96–360.56] | 361.87 [359.10–390.77] |
| overview-empty-1920x1080 | 697.58 [676.05–967.17] | 674.37 [659.44–698.12] |
| panel-label-64 | 76.31 [75.32–85.67] | 76.14 [75.49–77.65] |
| panel-label-1024 | 504.51 [497.91–513.45] | 492.98 [488.97–499.19] |
| panel-label-4096 | 1793.06 [1765.16–1806.97] | 1767.45 [1755.19–1772.62] |

All 25 WindowMaker workload checksums match between both executables across
all seven runs, including the surrounding workloads. The clock's median is
2.1% higher with overlapping sample ranges; the Overview and instrument-panel
medians are slightly lower. These small differences are not evidence of a
separate optimization to those surfaces.

## Regression investigation

The first System 7 implementation regressed two WindowMaker warm cases to
roughly 94 µs from 26 µs. An isolated no-prewarm negative-control build removed
the large regression. Syscall tracing showed repeated heap growth/trimming
after temporary fallback font setup. The final implementation warms the existing
session font selector and copies only resident face records into a compact
fallback database. It does not create a worker thread or retain a cloned full
installed-font slot map. The redundant WindowMaker band allocation and layout
reallocations are removed too.

All 720 WindowMaker golden cases, 84 OS-derived System 7 comparisons, 144
fractional System 7 implementation cases and the cold-Unicode no-I/O tripwire
pass. New tests also cover decomposed accents and emoji grapheme clusters.

Executable SHA-256 (preserved comparison artifacts):

- `performance-foundation`: `f561224e7da4ef7f3883ffd775e96d654fa10f3e5eddfbd6e57b21ba68ad492e`
- `performance-system7-final`: `ad02ea6965f34b198587371fa08c2c264e7e1499fb326dbc1e0d9ee135489fb6`

## Full-session review and fixes

The [initial ordered session samples](session-before-filter-samples.json),
[summary](session-before-filter-summary.json) and
[metadata](session-before-filter-metadata.json) caught a regression that the
CPU raster microbenchmark could not: System 7 consumed 16.643% of one core
[15.788–16.816] during a five-second drag, versus current WindowMaker's 14.240%
[13.267–15.070]. CPU time per compositor render call was 4.884 ms versus
4.088 ms. These are valid adverse results, retained for comparison.

A userspace `perf record -F 499 --call-graph dwarf,8192 -e cycles:u` profile
of 20-second drags, plus inspection of Smithay's GLES draw path, identified a
candidate source of avoidable submissions: the one transparent texel at each
shadow corner still caused a separate blended draw. The implementation in
`a233328` intersects draw damage with the opaque mask for pixel-aligned binary
alpha buffers. A fixed 32-rectangle stack array batches the visible pixels;
it keeps element identity, damage, opaque regions and input unchanged. Scaling,
fades, other transforms and excessive fragmentation retain the ordinary path.
Zero-alpha pixels with nonzero RGB also retain the ordinary blend behavior.

Three alternating ten-second drag pairs verify the change using the same
System 7 scene and renderer binary inputs apart from that optimization:

| System 7 | Drag CPU, % of one core | CPU ms per render call |
| --- | --- | --- |
| Before filtering | 16.741 [16.512–17.243] | 4.812 [4.800–4.860] |
| After filtering | 14.054 [14.042–14.442] | 4.052 [4.029–4.073] |

[All paired samples](binary-alpha-samples.json),
[summary](binary-alpha-summary.json), [metadata/hashes](binary-alpha-metadata.json).
The 15.8% reduction in median CPU time per render call removes the measured
extra-draw cost. Real 1× and 2× Foot desktop PNGs are byte-identical before/after;
the System 7 foot/xterm interaction suite and both styles' client-density matrix
also pass using the optimized release binary.

The comparison harness itself required a correction: concurrent terminal
startup randomized the drag target's position and occlusion. Eleven
[rejected preliminary samples](rejected-concurrent-samples.json) and their
[metadata](rejected-concurrent-metadata.json) are retained explicitly as invalid
comparators. The fixed harness waits for each client **and frame** before
launching the next, then checks their ordered row. A regression test covers the
client-before-frame lifecycle. Every valid session comparison below uses this
same fixed harness for all three executables/styles. Its three-client startup
interval includes sequential launches and should not be directly compared with
the earlier concurrent-startup campaign.

## Final nested-session comparison

Five alternating runs per build/style, three framed 318×180 Foot clients in a
fixed ordered row, 15 seconds idle after three seconds settling, then five
seconds dragging the rightmost terminal with 625 motion inputs. No builds,
tests or profiling overlapped this final campaign. Values are median [min–max].

| Metric | Before foundation WM | Current WM | Current System 7 |
| --- | --- | --- | --- |
| Idle CPU, % of one core | 0.067 [0.067–0.133] | 0.133 [0.067–0.133] | 0.067 [0.067–0.133] |
| Compositor RSS, KiB | 262284.000 [261968.000–262476.000] | 284888.000 [284388.000–285344.000] | 284256.000 [283372.000–284424.000] |
| First-scene barrier, ms | 270.776 [260.044–280.745] | 268.212 [266.822–302.927] | 282.770 [266.002–290.806] |
| Three-client barrier, ms | 715.706 [703.339–775.411] | 707.585 [703.642–777.605] | 781.138 [761.380–854.680] |
| Drag CPU, % of one core | 14.298 [13.854–14.487] | 14.109 [13.856–14.667] | 14.056 [12.900–14.256] |
| CPU ms per render call | 4.045 [4.023–4.124] | 4.070 [4.057–4.128] | 4.037 [4.022–4.128] |
| Motion inputs/s | 124.998 [124.997–124.998] | 124.998 [124.998–124.998] | 124.998 [124.998–124.998] |

[All 15 samples](session-samples.json), [summary](session-summary.json),
[metadata and executable hashes](session-metadata.json). CPU milliseconds per
render call are derived from each sample's CPU-tick delta (100 ticks/second)
and `render_calls`; this counts compositor rendering calls, not a native-panel
FPS measurement. The idle measurement has a one-tick step of approximately
0.0667% of one core; the median difference is within that quantization and
identical overlapping ranges.

The final drag measurements and CPU per render call overlap across all three
variants: the transparent-corner penalty is removed. There are still measured
startup/resource costs. System 7's median three-client barrier is 65 ms slower
than the foundation and 74 ms slower than current WindowMaker in this cold,
private-shader-cache software setup. First-scene ranges overlap; no claim of
zero startup cost is made.

RSS rises by 22.1 MiB for WindowMaker and 21.5 MiB for System 7 versus the
foundation. `/proc/PID/smaps` from the preceding ordered campaign identifies
roughly 21 MiB of additional font mappings: most is the 19,584 KiB
`NotoSansCJK-Bold.ttc` mapping touched during fallback preparation. Anonymous
memory changes by less than 1 MiB in that comparison. This is startup-selected,
file-backed resident coverage, shared by later engine/style changes; it is not
an allocation-free or memory-neutral change. Installed fonts affect this cost. [Extracted mapping evidence](font-rss.json)
records the source snapshot hashes and individual font totals.


The original foundation executable is the same artifact recorded in the
[foundation campaign](../paired-binaries.json); its version string includes
`22f5590-dirty` because that measurement preceded its commit. The final candidate
was built from the rendering code committed as `a233328` before committing it;
its artifact hash, source/version string and harness hash are all retained in
the metadata. Microbenchmark source remains `4f8de18`: the subsequent fix only
changes Wayland submission, not the theme renderer.

These tests use headless Weston/pixman and Mesa llvmpipe. They do not establish
native GPU throughput, scanout gains or an idle CPU reduction.
