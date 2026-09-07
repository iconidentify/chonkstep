# Matched constrained capture comparison

Complete: **True**. Matched process pairs: **3**. Scope passed: **True**. Owned cgroup removed: **True**.

Both builds used 2 GiB MemoryMax, 1536 MiB MemoryHigh, swap disabled, CPU quota 200%, affinity [2, 3], and LP_NUM_THREADS=2. Synthetic retained anonymous memory was 1280 MiB with 32 MiB churn; private real foot, native animation, and input-observer clients were also active.

The frozen mixed-v2 harness changes only diagnostic screenshot marker publication from v1 to atomic rename. Both matched builds use v2; no measured phase, payload, client, loop, or deadline changed. Earlier successful calibration remains independent evidence.

| Metric (process medians) | Baseline | Candidate | Paired change median [min, max] | Lower pairs |
| --- | ---: | ---: | ---: | ---: |
| First open, ms | 93.059 | 85.047 | +7.121 [-14.010, +23.556] | 1/3 |
| Warm open, ms | 40.344 | 53.443 | +13.099 [-11.531, +23.384] | 1/3 |
| Client input before capture, ms | 53.102 | 26.544 | -26.675 [-27.766, +36.245] | 2/3 |
| Client input after capture, ms | 32.176 | 53.961 | +21.785 [+20.696, +128.653] | 0/3 |
| Draw compositor CPU, % of one CPU | 60.283 | 14.571 | -45.712 [-46.041, -45.534] | 3/3 |
| Resize compositor CPU, % of one CPU | 58.497 | 13.689 | -44.879 [-46.039, -44.242] | 3/3 |
| Draw downstream barrier, ms | 37.765 | 50.327 | +9.090 [+7.501, +25.535] | 0/3 |
| Resize downstream barrier, ms | 54.247 | 43.913 | -10.335 [-78.865, +21.091] | 2/3 |
| Draw render calls | 173.000 | 187.000 | +12.000 [+8.000, +14.000] | 0/3 |
| Resize render calls | 173.000 | 184.000 | +9.000 [+6.000, +14.000] | 0/3 |
| Closed animation callbacks | 368.000 | 367.000 | -2.000 [-8.000, +26.000] | 2/3 |
| Closed compositor PSS, KiB | 139237.000 | 139586.000 | +583.000 [+347.000, +776.000] | 0/3 |
| Window memory PSI some, % | 6.861 | 4.605 | +0.792 [-2.256, +1.879] | 1/3 |
| Window memory PSI full, % | 5.160 | 3.707 | +1.078 [-1.453, +1.283] | 1/3 |
| Window memory high events | 8519.000 | 7220.000 | -252.000 [-1299.000, +2419.000] | 2/3 |
| Window CPU PSI some, % | 4.701 | 2.856 | -1.970 [-2.178, -1.845] | 3/3 |
| Window CPU quota throttled periods | 0.000 | 0.000 | +0.000 [+0.000, +0.000] | 0/3 |

Every process is retained below in execution order. Phase details, full warm-open vectors, sender lateness, callback counts, and kernel sample boundary excess are preserved in the compact JSON.

| Process | First open ms | Input after ms | Draw / resize CPU % | Draw / resize barrier ms | Memory PSI some % | Memory high events |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 00-baseline | 93.059 | 32.284 | 60.283 / 60.207 | 37.765 / 54.247 | 1.791 | 4099 |
| 00-capture-r6 | 100.181 | 52.980 | 14.571 / 14.168 | 46.855 / 43.913 | 3.670 | 6518 |
| 01-capture-r6 | 79.829 | 53.961 | 14.491 / 13.618 | 50.327 / 55.155 | 4.605 | 7220 |
| 01-baseline | 93.839 | 32.176 | 60.533 / 58.497 | 42.826 / 34.063 | 6.861 | 8519 |
| 02-baseline | 61.491 | 31.603 | 60.209 / 57.930 | 28.667 / 107.708 | 7.753 | 9808 |
| 02-capture-r6 | 85.047 | 160.256 | 14.675 / 13.689 | 54.203 / 28.843 | 8.545 | 9556 |

Whole-scope OOM event observed: **False**. Whole-scope final counters: `{"cpu.pressure": {"full_total_us": 552174, "some_total_us": 14878152}, "cpu.stat": {"burst_usec": 0, "core_sched.force_idle_usec": 0, "nice_usec": 3325, "nr_bursts": 0, "nr_periods": 3741, "nr_throttled": 1, "system_usec": 21390730, "throttled_usec": 1867, "usage_usec": 307648114, "user_usec": 286257383}, "memory.events": {"high": 46940, "low": 0, "max": 0, "oom": 0, "oom_group_kill": 0, "oom_kill": 0}, "memory.pressure": {"full_total_us": 15365707, "some_total_us": 19491777}}`.

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
- The original toolbar cursor is invisible; toolbar-motion costs compare different visible output and cannot establish a like-for-like regression.

Compact evidence: `/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/comparison-capture-r6-2g-1280-mixed-capture/compact-pressure-report.json`. Raw scope, fixture logs, sample data, and separate cleanup verification remain alongside the campaign.
