# Decoration performance baseline — 2026-09-11

Measured from main `22f5590675cdc771ccdfc685547fb441de22adf9` before moving the renderer.
The only microbenchmark source difference is the expanded measurement harness.
Release executables were preserved before any compositor changes; hashes are in
[baseline-binaries.json](baseline-binaries.json), host/compiler in [machine.txt](machine.txt).
Five runs per workload, on i9beef (Intel Core i9-9900K). The normal desktop and
background services stayed running; no overlapping builds or stress tests.

## Renderer

[Raw matrix samples](matrix-baseline.jsonl). Times are median [min–max]. Cold means
an empty per-engine title cache: engine construction, font discovery, glyph warm-up
and per-scale layout setup are outside the interval. Warm calls use the actual
owned-buffer API, including allocation/copy costs. `peak_live_growth_bytes` on the
cold workload includes title caches retained by its disposable engines; it is not
an individual window's footprint. The `retained_bytes` field is that window's exact
CPU pixel payload, excluding Vec capacity/metadata and GPU storage.

| Content | Scale | Layout ns | Cold render µs | Warm render µs | Retained bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| 800×600 | 1 | 181.00 [180.00–213.00] | 94.75 [92.55–125.13] | 8.71 [8.61–10.16] | 110,664 |
| 800×600 | 2 | 183.00 [182.00–207.00] | 137.25 [136.02–162.71] | 16.11 [16.01–19.33] | 221,856 |
| 1280×800 | 1 | 177.00 [177.00–178.00] | 106.99 [104.14–107.71] | 13.00 [12.70–13.84] | 175,624 |
| 1280×800 | 2 | 183.00 [183.00–186.00] | 179.73 [178.86–182.48] | 25.13 [24.96–25.22] | 351,776 |
| 2560×1600 | 1 | 177.00 [176.00–185.00] | 165.61 [164.00–166.70] | 25.37 [25.30–25.37] | 350,984 |
| 2560×1600 | 2 | 183.00 [182.00–221.00] | 294.73 [293.38–296.55] | 49.96 [49.63–54.94] | 702,496 |

Layout currently requests **4 allocations / 600 bytes** at every listed size and
scale. Warm rendering requests **8 allocations**, with the exact byte budgets
pinned in `crates/wm-theme/tests/decoration_contract.rs`. This API is not currently
allocation-free. Its owned Vec outputs and cached-title copies remain counted.
Cold-render allocation counts/bytes, checksums and the unchanged Overview and
instrument-panel workloads are in the raw matrix. The original, unextended
example's five runs are retained in [original-micro.jsonl](original-micro.jsonl).

## Nested three-window session

Five private 1280×800 scale-1 ChonkStep sessions on headless Weston/pixman, using
Mesa llvmpipe (LLVM 22.1.8). XWayland and compatibility IPC enabled; Dock hidden;
three real Foot clients (320×180 requested, stable text). Fifteen-second idle
intervals after three seconds settling, then five seconds of titlebar drag with
625 motion inputs. The actual framebuffer geometry and client memberships are
saved per sample; every drag verified that its target frame moved.

This measures a nested software compositor, not native RTX 3090 scanout or login.
The first-scene barrier measures the empty shell; the three-client barrier also
includes process startup and mapping. Idle accounting has 10 ms CPU tick
resolution, so a 15-second sample's one-tick step is about 0.0667% of one core.

[Metadata](session-before-metadata.json), [all sample data](session-before-samples.json),
[summary](session-before-summary.json). The rejected first harness attempt waited
for a reply to a reply-free input command and is excluded; a regression test now
pins that protocol distinction. Its diagnostic files remain local.

| Metric | Median | Range |
| --- | ---: | ---: |
| Idle CPU, % of one core | 0.067 | 0.067–0.133 |
| Compositor RSS, KiB | 300228.000 | 299828.000–300320.000 |
| First-scene barrier, ms | 260.773 | 256.431–273.136 |
| Three-client barrier, ms | 534.530 | 516.487–583.743 |
| Drag CPU, % of one core | 17.855 | 14.241–18.213 |
| Actual motion inputs/s | 124.998 | 124.998–124.998 |

The real renderer through wm-core also produced exactly **60 rasters for 125
synthetic resize inputs across 60 frame boundaries**. The fake backend verifies
that title, focus and pressed-button transitions change only the title band.
Backend-specific regressions separately protect omission of unchanged uploads.

Reproduction:

```sh
cargo run --release -p wm-theme --example performance -- --style windowmaker
python3 scripts/bench-compositor.py --binary before=/path/to/saved/chonkstep-wayland \
  --output /tmp/chonk-style-base --runs 5 --idle-seconds 15 --settle-seconds 3 \
  --decoration-workload
```

The original executable must be saved before rebuilding. Subsequent comparisons
use both `--binary before=... --binary after=...` to alternate run order, on the
same host with identical settings. Timing claims require distributions, matching
pixels, and the allocation/storage gates; a single fast sample is insufficient.

## Paired foundation comparison

After the style seam and unchanged-band upload fixes, seven alternating microbenchmark pairs and five alternating nested pairs used the same workload and host. [Micro samples](matrix-paired.jsonl), [session samples](session-paired-samples.json), [session summary](session-paired-summary.json), [metadata](session-paired-metadata.json), and [executable hashes](paired-binaries.json) preserve the complete comparison. No builds, tests or browser workloads overlapped these measurements.

Every microbenchmark checksum, allocation count and allocated-byte count matches before/after. The 720 pre-refactor golden cases and all eight Omarchy preview PNGs also match byte for byte.

### Microbenchmarks

All times below are nanoseconds, median [min–max]; seven samples per side.

| Workload | Content / scale | Before ns | After ns | Median change |
| --- | --- | ---: | ---: | ---: |
| layout | 800×600 / 1 | 181 [179–219] | 183 [179–220] | +1.1% |
| render-warm | 800×600 / 1 | 8,728 [8,643–10,728] | 8,610 [8,486–10,054] | -1.4% |
| render-cold | 800×600 / 1 | 98,080 [94,489–135,825] | 95,999 [93,553–123,449] | -2.1% |
| layout | 800×600 / 2 | 185 [181–231] | 185 [183–213] | +0.0% |
| render-warm | 800×600 / 2 | 16,294 [15,944–19,977] | 16,098 [15,694–16,332] | -1.2% |
| render-cold | 800×600 / 2 | 142,237 [134,810–199,659] | 123,737 [120,764–127,246] | -13.0% |
| layout | 1280×800 / 1 | 178 [176–239] | 178 [177–181] | +0.0% |
| render-warm | 1280×800 / 1 | 12,993 [12,778–14,886] | 13,151 [12,917–13,329] | +1.2% |
| render-cold | 1280×800 / 1 | 108,021 [106,556–134,380] | 93,251 [92,177–94,465] | -13.7% |
| layout | 1280×800 / 2 | 186 [183–200] | 187 [184–194] | +0.5% |
| render-warm | 1280×800 / 2 | 25,091 [24,697–28,351] | 25,827 [24,796–33,121] | +2.9% |
| render-cold | 1280×800 / 2 | 182,209 [179,915–236,924] | 167,423 [163,257–199,353] | -8.1% |
| layout | 2560×1600 / 1 | 179 [176–221] | 180 [178–204] | +0.6% |
| render-warm | 2560×1600 / 1 | 25,759 [24,994–31,168] | 25,867 [25,467–29,707] | +0.4% |
| render-cold | 2560×1600 / 1 | 169,580 [166,053–199,936] | 153,720 [151,520–193,248] | -9.4% |
| layout | 2560×1600 / 2 | 185 [183–199] | 186 [185–218] | +0.5% |
| render-warm | 2560×1600 / 2 | 50,304 [49,344–53,440] | 49,605 [49,195–58,810] | -1.4% |
| render-cold | 2560×1600 / 2 | 339,658 [294,913–370,857] | 269,564 [266,317–289,834] | -20.6% |
| decoration-1600x1000 | original fixture | 359,800 [351,651–468,951] | 354,333 [351,076–447,686] | -1.5% |
| sparse-decoration-1600x1000 | original fixture | 16,097 [16,043–19,668] | 16,094 [15,892–16,643] | -0.0% |
| clock-224 | original fixture | 356,069 [348,300–374,705] | 354,361 [349,172–364,095] | -0.5% |
| overview-empty-1920x1080 | original fixture | 653,432 [651,098–689,266] | 674,548 [664,871–897,490] | +3.2% |
| panel-label-64 | original fixture | 74,851 [73,755–77,829] | 76,393 [74,430–85,180] | +2.1% |
| panel-label-1024 | original fixture | 499,155 [495,317–506,029] | 496,121 [492,074–500,215] | -0.6% |
| panel-label-4096 | original fixture | 1,785,413 [1,751,508–1,806,711] | 1,768,490 [1,748,801–1,775,363] | -0.9% |

The largest warm-decoration median increase is 2.9% (1280×800 at scale 2), with overlapping ranges. The unchanged Overview workload has a 3.2% higher median and one slower after sample; its pixel/allocation outputs also match. Layout medians differ by 0–2 ns. Cold-title timings trend lower, but this experiment does not isolate a cause and is not evidence of a general renderer speedup.

### Nested sessions

| Metric | Before median [range] | After median [range] |
| --- | ---: | ---: |
| Idle CPU, % one core | 0.133 [0.133–0.133] | 0.067 [0.067–0.133] |
| RSS, KiB | 300212.000 [300104.000–300404.000] | 300192.000 [300024.000–300600.000] |
| First scene, ms | 266.158 [257.459–275.977] | 272.643 [258.398–280.197] |
| Three clients ready, ms | 535.928 [514.184–587.191] | 552.148 [544.494–555.939] |
| 125 Hz drag CPU, % one core | 18.049 [14.015–18.814] | 18.415 [14.445–18.979] |
| Motion inputs/s | 124.998 [124.998–124.998] | 124.998 [124.998–124.999] |

These ranges overlap. The idle difference is one CPU accounting tick per sample; it must not be advertised as a 50% CPU reduction. RSS is effectively unchanged. First-scene and three-client medians are 6.5 ms and 16.2 ms higher, respectively, inside the observed startup ranges; drag CPU is 0.37 percentage points higher with overlapping ranges. The data show no clear performance regression from the foundation, and do not establish a native-GPU acceleration claim.
