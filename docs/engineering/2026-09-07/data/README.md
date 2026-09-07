# Compact campaign evidence

These records accompany [the capture performance report](../capture-performance.md).
They preserve individual values, paired summaries and build/fixture metadata.
The complete process snapshots, logs, source freezes and executables remain in
its named local artifact directory.

- `capture-r3-*`: seven ordinary baseline/r3 pairs. This checkpoint predates
  the final panel, clipboard, joined-PNG and wlr fanout changes.
- `capture-r5-*`: three ordinary baseline/r5 pairs confirming the final capture
  implementation. The later r6 product change only affects Link readiness.
- `capture-r6-1g-512-*` and `capture-r6-2g-1280-*`: three matched mixed-workload
  pairs per limit, with observed pressure, every individual sample, scope
  outcomes and process/cgroup cleanup. The 2 GiB adverse analysis retains the
  post-capture input and drawing-barrier regressions.
- `capture-r6-pressure-*` and `mixed-harness-v2-*`: actual commands, execution,
  source hashes and the atomic diagnostic marker fixture correction.
- `panel-r4-*`: three visible-Dock/panel pairs, including the slower surface
  mapping/settling values and fresh command-start measurements.
- `capture-r6-memory-*`, `*-memory-profile-*` and `normal-v2-*`:
  three separate instrumented allocation pairs, profile build metadata and
  fixture provenance. These records do not support timing or total-RAM claims.
- `validation-checkpoints.json`: completed gates and corrected fixture outcomes,
  explicitly distinguished by product checkpoint.
- `capture-r6-soak-*`: the 602-second, 1,813-cycle ordinary r6 resource soak;
  includes all sampled memory values, scope limits and outcomes. The separate
  `capture-r6-cleanup.json` records removal of its owned cgroup.
- `capture-r7-*` and the `r6-r7-*` comparison records preserve the rejected
  early-flush experiment, including its passing correctness gates, every
  latency sample, differing reclaim and the decision against promotion.
- `accepted-r6-*`: verification of the restored production/scripts/packaging
  sources and the ordinary build's exact match to the accepted frozen binary.
- `mosaic-comparison.json`: exact baseline/candidate placement digests and
  component timings for all 54 scenarios.
- `png-r4.log`: final bounded PNG adapter component timings, requested allocator
  traffic and encoded sizes. This is not filesystem or total RSS accounting.

Allocation diagnostics and sustained-soak records are recorded separately from
ordinary timing and matched pressure evidence.
