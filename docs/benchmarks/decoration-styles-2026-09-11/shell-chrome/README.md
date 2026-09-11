# Shell chrome measurements

Release executables on i9beef, September 11, 2026; seven alternating rounds.
Before is the preserved final #163 renderer (`4f8de18`); after is this #164 change
on `d66dc2d`. All samples, executable hashes and ordering are recorded below.
Our builds, tests and profiling were stopped during this campaign. The host's normal
background services remained running. Times are median [min–max], in microseconds.

## Existing workloads before and after

All 25 workloads in **both styles** retain identical checksums, allocation counts,
allocated bytes and retained-pixel values across all seven before/after rounds.
These are CPU microbenchmarks, not native GPU/FPS or idle-CPU measurements.

| Workload / style | Before µs | After µs | Median change |
| --- | --- | --- | --- |
| overview-empty-1920x1080 / windowmaker | 671.11 [649.70–689.07] | 675.14 [663.96–694.51] | +0.6% |
| panel-label-64 / windowmaker | 76.02 [75.26–83.97] | 76.12 [75.68–77.96] | +0.1% |
| panel-label-1024 / windowmaker | 492.10 [488.83–649.14] | 495.81 [492.57–503.03] | +0.8% |
| panel-label-4096 / windowmaker | 1776.87 [1747.73–1858.70] | 1770.58 [1765.24–1804.68] | -0.4% |
| overview-empty-1920x1080 / system7 | 672.51 [652.04–881.79] | 689.11 [656.38–693.00] | +2.5% |
| panel-label-64 / system7 | 76.77 [75.18–99.92] | 75.60 [74.55–82.96] | -1.5% |
| panel-label-1024 / system7 | 499.43 [495.90–602.89] | 500.86 [497.60–518.86] | +0.3% |
| panel-label-4096 / system7 | 1778.95 [1770.16–2011.51] | 1786.30 [1765.40–1817.81] | +0.4% |

The surrounding Overview and instrument-panel render functions are unchanged.
Small differences in timed samples should not be described as a separate optimization.

## New shell raster workload

`performance --shell-chrome --style STYLE` measures menu, two-window switcher,
minimized icon and cached-caption rasterization at 1× and 2×, using ASCII atlas
labels and no client preview images. Font setup and output checksums are outside
the timed interval. These are costs on open/semantic changes; native Overview
animation and selection reuse the rendered captions instead of invoking this code.

| Surface / scale | WindowMaker µs | System 7 µs | WM / System 7 allocations | WM / System 7 allocated bytes |
| --- | --- | --- | --- | --- |
| shell-menu / 1 | 239.43 [231.37–311.65] | 19.08 [18.43–19.61] | 193 / 3 | 97708 / 30080 |
| shell-switcher / 1 | 86.21 [85.38–100.13] | 21.89 [21.49–22.27] | 76 / 1 | 89341 / 51888 |
| shell-icon / 1 | 29.37 [28.95–31.86] | 6.66 [6.57–8.52] | 26 / 1 | 17452 / 12544 |
| shell-caption / 1 | 45.20 [44.60–50.60] | 9.44 [9.33–12.48] | 92 / 1 | 53471 / 16128 |
| shell-menu / 2 | 311.60 [306.46–357.50] | 52.86 [52.72–66.14] | 211 / 3 | 220702 / 119936 |
| shell-switcher / 2 | 255.50 [254.02–278.26] | 54.31 [53.68–61.67] | 76 / 1 | 315837 / 207552 |
| shell-icon / 2 | 84.98 [84.31–102.80] | 15.64 [15.59–19.71] | 26 / 1 | 55084 / 50176 |
| shell-caption / 2 | 90.04 [86.97–103.99] | 30.20 [30.10–32.13] | 92 / 1 | 99839 / 64512 |

The owned-buffer APIs allocate the output pixels. Both styles use the same
session font state; System 7 does not add a font database/worker or per-frame
font discovery. Rust allocator counts do not include system-library allocations.

Native Overview stores its four border IDs per card on scene creation, reuses
them on semantic refresh, and submits through the existing retained per-output
element vector. Its live-client test verifies changing terminal content at 1×/2×
in both styles, with zero readback previews, an input-only output shell and
unchanged caption storage during pointer selection. The existing frame allocation,
retained-bytes, sparse-damage and coalescing regression gates also pass.

## Reproduction and raw data

```sh
cargo build --release -p wm-theme --example performance
# Preserve each revision's target/release/examples/performance before rebuilding.
performance-before --style windowmaker
performance-after --style windowmaker
performance-before --style system7
performance-after --style system7
# Seven rounds, reversing the command order on alternate rounds.
performance-after --shell-chrome --style windowmaker
performance-after --shell-chrome --style system7
# Seven rounds, reversing order on alternate rounds.
```

- [Complete before/after timing and allocation table](all-workloads.md)
- [All 700 existing-workload records](existing.jsonl)
- [All 112 shell-workload records](shell-chrome.jsonl)
- [Machine, order and binary SHA-256](metadata.json)
- [Desktop screenshots at 1× and 2×](../../../../site/shots/shell-chrome/README.md)
