# R6 / R7 bounded experiment recommendation

Do not promote r7 as a demonstrated performance improvement from this experiment alone. Retain r6 as the accepted measured reference unless the pre-render flush is accepted on separate event-ordering grounds. This is a recommendation about the evidence threshold, not a proven diagnosis of an r7 regression.

All six process samples passed. The owned scope was removed and all 36 recorded PID/start-time identities exited. No follow-on benchmark was started.

The first pair's post-close input bracketing windows had no recorded reclaim; the full active-window memory PSI some was still 1.908% for r6 and 3.135% for r7. Post-capture client input receipt improved from 53.296 to 27.663 ms in that pair, consistent with the proposed mechanism without establishing causality. Across all three pairs, input-before improved by 0.859–2.617 ms. Input-after improved in two pairs but worsened by 156.891 ms in the third: r6 values were 53.296, 28.895 and 27.731 ms; r7 values were 27.663, 27.265 and 184.622 ms. The process median changed only 28.895→27.663 ms. Every sample is retained.

Draw and resize endpoint barriers had mixed paired directions; median paired differences were −0.093 and −2.498 ms, with positive differences in one pair each. Their independent label medians were worse in r7. CPU and render/callback distributions substantially overlapped; the common capture optimization remains present in both versions.

Aggregate draw flush time increased from a median 3.470 to 4.490 ms per approximately five-second phase. Resize flush time was 4.218→4.069 ms; closed animation phase flush time was 8.190→8.383 ms over approximately ten seconds. R6 measures end flush only; r7 measures pre-render plus end flush. These are aggregate elapsed costs, not exact per-scan overhead. The measured additional work is modest and does not explain the large endpoint tail by itself.

R7's active-window memory PSI and memory.high event counts were higher in all three pairs despite identical limits and synthetic payloads. Median memory PSI some was 4.626→5.068%, full 3.725→4.048%. The outlier's enclosing post-close window had 92.138 ms memory PSI some and 48.928 ms full. The 250 ms external sample cadence and absence of exact input start/end timestamps prevent attribution to the key receipt interval. Do not discard the outlier because reclaim was present.

An earlier independent baseline/r6 pressure campaign observed a 160.256 ms r6 post-input tail. Therefore this three-pair r6/r7 experiment does not establish a reliable tail distribution or prove r7 caused a regression. It also does not establish that r7 fixes pressured input latency. Report the uncertainty and preserve the experimental source/evidence.

Scope events: 44,656 memory.high, zero memory.max/OOM/kill events and zero CPU quota throttles. Two physical CPU affinities, quota200%, 2GiB MemoryMax, 1.5GiB MemoryHigh, swap0, 1280MiB retained plus32MiB churn were unchanged. Rendering used the same isolated nested Winit/Weston llvmpipe fixture and frozen clients; conclusions do not establish DRM hardware latency.
