# Matched constrained capture comparison

Compared `capture-r6` (before) to `capture-r7` (after).

Complete: **True**. Matched process pairs: **3**. Scope passed: **True**. Owned cgroup removed: **True**.

Both builds used 2 GiB MemoryMax, 1536 MiB MemoryHigh, swap disabled, CPU quota 200%, affinity [2, 3], and LP_NUM_THREADS=2. Synthetic retained anonymous memory was 1280 MiB with 32 MiB churn; private real foot, native animation, and input-observer clients were also active.

The frozen mixed-v2 harness changes only diagnostic screenshot marker publication from v1 to atomic rename. Both matched builds use v2; no measured phase, payload, client, loop, or deadline changed. Earlier successful calibration remains independent evidence.

| Metric (process medians) | capture-r6 | capture-r7 | Paired change median [min, max] | Lower pairs |
| --- | ---: | ---: | ---: | ---: |
| First open, ms | 87.280 | 86.275 | +3.225 [-27.611, +30.067] | 1/3 |
| Warm open, ms | 54.274 | 53.829 | -0.445 [-7.799, +1.760] | 2/3 |
| Client input before capture, ms | 27.693 | 26.572 | -1.383 [-2.617, -0.859] | 3/3 |
| Client input after capture, ms | 28.895 | 27.663 | -1.630 [-25.633, +156.891] | 2/3 |
| Draw compositor CPU, % of one CPU | 14.386 | 14.319 | -0.022 [-0.380, +0.217] | 2/3 |
| Resize compositor CPU, % of one CPU | 13.912 | 14.106 | +0.193 [-0.623, +0.249] | 1/3 |
| Draw downstream barrier, ms | 40.630 | 46.624 | -0.093 [-8.662, +8.722] | 2/3 |
| Resize downstream barrier, ms | 42.338 | 51.208 | -2.498 [-9.198, +8.925] | 2/3 |
| Draw render calls | 186.000 | 187.000 | +1.000 [-2.000, +2.000] | 1/3 |
| Resize render calls | 185.000 | 186.000 | +1.000 [+1.000, +2.000] | 0/3 |
| Closed animation callbacks | 382.000 | 380.000 | -4.000 [-9.000, +6.000] | 2/3 |
| Closed compositor PSS, KiB | 139164.000 | 139676.000 | +210.000 [-265.000, +995.000] | 1/3 |
| Window memory PSI some, % | 4.626 | 5.068 | +1.226 [+0.442, +1.709] | 0/3 |
| Window memory PSI full, % | 3.725 | 4.048 | +0.861 [+0.323, +1.221] | 0/3 |
| Window memory high events | 7096.000 | 7113.000 | +621.000 [+17.000, +1224.000] | 0/3 |
| Window CPU PSI some, % | 2.998 | 3.026 | +0.230 [-0.448, +0.332] | 1/3 |
| Window CPU quota throttled periods | 0.000 | 0.000 | +0.000 [+0.000, +0.000] | 0/3 |

Every process is retained below in execution order. Phase details, full warm-open vectors, sender lateness, callback counts, and kernel sample boundary excess are preserved in the compact JSON.

| Process | First open ms | Input after ms | Draw / resize CPU % | Draw / resize barrier ms | Memory PSI some % | Memory high events |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 00-capture-r6 | 113.118 | 53.296 | 14.342 / 13.912 | 55.287 / 53.707 | 1.908 | 4455 |
| 00-capture-r7 | 85.506 | 27.663 | 14.319 / 14.106 | 46.624 / 51.208 | 3.135 | 5076 |
| 01-capture-r7 | 86.275 | 27.265 | 14.006 / 14.116 | 28.229 / 51.263 | 5.068 | 7113 |
| 01-capture-r6 | 56.208 | 28.895 | 14.386 / 13.867 | 28.323 / 42.338 | 4.626 | 7096 |
| 02-capture-r6 | 87.280 | 27.731 | 14.537 / 14.230 | 40.630 / 36.732 | 6.706 | 9106 |
| 02-capture-r7 | 90.505 | 184.622 | 14.754 / 13.607 | 49.352 / 27.535 | 8.416 | 10330 |

Whole-scope OOM event observed: **False**. Whole-scope final counters: `{"cpu.pressure": {"full_total_us": 520889, "some_total_us": 12020496}, "cpu.stat": {"burst_usec": 0, "core_sched.force_idle_usec": 0, "nice_usec": 3331, "nr_bursts": 0, "nr_periods": 3750, "nr_throttled": 0, "system_usec": 21313164, "throttled_usec": 0, "usage_usec": 295373706, "user_usec": 274060541}, "memory.events": {"high": 44656, "low": 0, "max": 0, "oom": 0, "oom_group_kill": 0, "oom_kill": 0}, "memory.pressure": {"full_total_us": 14129964, "some_total_us": 17576613}}`.

Earlier baseline-only 2g-1280 calibration used shorter phases and is not pooled: selection draw/resize downstream barriers were 43.128 / 54.436 ms. Those stalls remain part of the evidence.

Interpretation limits:

- All expected samples are retained; any missing sample or failed scope prevents complete=true.
- Identical caps/payloads are held fixed. Actual PSI/high events may differ because allocation/rendering behavior differs; report that severity difference.
- Active-window pressure spans first-open through closed-idle, including intervening settling/actions, but excludes initial client setup.
- External samples enclose each window; excess boundary milliseconds are explicit. Short phase values are not exact kernel attribution.
- Per-label memory maxima are sampled memory.current, not per-label kernel peaks; the kernel peak applies to the entire mixed scope.
- Compare callback/render counts with phase durations and sender lateness. Nested barriers are not physical display/input latency.
- A 500 ms open/input pass gate is not a hitch-free guarantee.

- Three matched pairs provide a small-sample check; they do not establish robust tail percentiles or hardware-wide performance.
- Synthetic reclaim severity is an observed outcome. Identical limits and payloads can produce different PSI and render throughput between builds.
- Both r6 and r7 render visible toolbar cursors; this comparison preserves the capture UI and workload.

Compact evidence: `/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/comparison-r6-r7-2g-1280-mixed-capture/compact-pressure-report.json`. Raw scope, fixture logs, sample data, and separate cleanup verification remain alongside the campaign.
