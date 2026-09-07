# Matched constrained capture comparison

Complete: **True**. Matched process pairs: **3**. Scope passed: **True**. Owned cgroup removed: **True**.

Both builds used 1 GiB MemoryMax, 768 MiB MemoryHigh, swap disabled, CPU quota 200%, affinity [2, 3], and LP_NUM_THREADS=2. Synthetic retained anonymous memory was 512 MiB with 32 MiB churn; private real foot, native animation, and input-observer clients were also active.

The frozen mixed-v2 harness changes only diagnostic screenshot marker publication from v1 to atomic rename. Both matched builds use v2; no measured phase, payload, client, loop, or deadline changed. Earlier successful calibration remains independent evidence.

| Metric (process medians) | Baseline | Candidate | Paired change median [min, max] | Lower pairs |
| --- | ---: | ---: | ---: | ---: |
| First open, ms | 91.299 | 81.895 | -9.404 [-32.198, +40.956] | 2/3 |
| Warm open, ms | 54.387 | 53.208 | -1.179 [-22.525, +0.523] | 2/3 |
| Client input before capture, ms | 53.878 | 29.386 | -24.492 [-26.383, +9.097] | 2/3 |
| Client input after capture, ms | 69.350 | 28.139 | -44.963 [-183.256, -41.210] | 3/3 |
| Draw compositor CPU, % of one CPU | 45.040 | 14.286 | -31.043 [-31.444, -28.825] | 3/3 |
| Resize compositor CPU, % of one CPU | 44.388 | 13.390 | -30.998 [-33.680, -27.970] | 3/3 |
| Draw downstream barrier, ms | 192.164 | 42.445 | -58.539 [-149.719, -54.709] | 3/3 |
| Resize downstream barrier, ms | 103.584 | 39.737 | -58.098 [-175.842, -1.064] | 3/3 |
| Draw render calls | 133.000 | 182.000 | +49.000 [+41.000, +53.000] | 0/3 |
| Resize render calls | 130.000 | 177.000 | +53.000 [+40.000, +56.000] | 0/3 |
| Closed animation callbacks | 354.000 | 365.000 | +11.000 [+4.000, +11.000] | 0/3 |
| Closed compositor PSS, KiB | 139223.000 | 139648.000 | +918.000 [-140.000, +946.000] | 1/3 |
| Window memory PSI some, % | 34.593 | 28.884 | +0.373 [-6.026, +0.407] | 1/3 |
| Window memory PSI full, % | 27.693 | 22.348 | -1.100 [-6.222, +0.137] | 2/3 |
| Window memory high events | 16785.000 | 15150.000 | -863.000 [-5344.000, +312.000] | 2/3 |
| Window CPU PSI some, % | 4.210 | 3.338 | -0.988 [-1.280, -0.816] | 3/3 |
| Window CPU quota throttled periods | 0.000 | 0.000 | +0.000 [+0.000, +0.000] | 0/3 |

Every process is retained below in execution order. Phase details, full warm-open vectors, sender lateness, callback counts, and kernel sample boundary excess are preserved in the compact JSON.

| Process | First open ms | Input after ms | Draw / resize CPU % | Draw / resize barrier ms | Memory PSI some % | Memory high events |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 00-baseline | 91.299 | 237.684 | 45.040 / 46.747 | 218.386 / 207.354 | 28.510 | 16013 |
| 00-capture-r6 | 81.895 | 54.428 | 13.996 / 13.067 | 163.677 / 31.512 | 28.884 | 15150 |
| 01-capture-r6 | 99.918 | 28.139 | 14.286 / 13.390 | 32.022 / 39.737 | 28.567 | 13612 |
| 01-baseline | 58.962 | 69.350 | 45.730 / 44.388 | 90.561 / 40.801 | 34.593 | 18956 |
| 02-baseline | 92.313 | 54.909 | 43.473 / 41.370 | 192.164 / 103.584 | 35.898 | 16785 |
| 02-capture-r6 | 60.115 | 9.946 | 14.648 / 13.400 | 42.445 / 45.487 | 36.305 | 17097 |

Whole-scope OOM event observed: **False**. Whole-scope final counters: `{"cpu.pressure": {"full_total_us": 617798, "some_total_us": 14877614}, "cpu.stat": {"burst_usec": 0, "core_sched.force_idle_usec": 0, "nice_usec": 6666, "nr_bursts": 0, "nr_periods": 3884, "nr_throttled": 0, "system_usec": 21359170, "throttled_usec": 0, "usage_usec": 291611419, "user_usec": 270252248}, "memory.events": {"high": 101932, "low": 0, "max": 0, "oom": 0, "oom_group_kill": 0, "oom_kill": 0}, "memory.pressure": {"full_total_us": 92387304, "some_total_us": 120343592}}`.

Earlier baseline-only 1g-512 calibration used shorter phases and is not pooled: selection draw/resize downstream barriers were 297.761 / 288.687 ms. Those stalls remain part of the evidence.

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

Compact evidence: `/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/comparison-capture-r6-1g-512-mixed-capture/compact-pressure-report.json`. Raw scope, fixture logs, sample data, and separate cleanup verification remain alongside the campaign.
