# Reproduce the mixed capture workload

[`bench-capture-mixed.py`](../../../scripts/bench-capture-mixed.py) exercises
capture overlay opening, motion, selection and closing while three real private
Wayland clients run: a frame-paced animation, an input observer and foot hosting
an explicitly synthetic memory worker. It creates a private Weston/pixman host,
nested 1280×800 compositor and D-Bus session. It does not save a user capture or
launch review apps; diagnostic PNGs are separate from latency measurements.

The scripts were promoted from the validated frozen experiment. The base
helper import and pure-test source path changed. One diagnostic-only correction
publishes the screenshot marker through a sibling temporary file and atomic
rename: exposing an empty marker could otherwise request the default screenshot
path before its intended filename was written. Timed workload behavior remains
unchanged. The existing
`scripts/bench-compositor.py` was byte-identical to the preserved base helper;
`scripts/bench-pressure.py` already matched the frozen memory-high runner. Past
artifact scripts and measurement records remain unchanged. Diagnostic PNGs are
created between timed phases in the original harness too; their marker
publication is corrected in the separately frozen v2 pressure harness. Preserve
each experiment's original hashes when citing its results rather than
substituting the repository script.

Use Linux cgroup v2 with a working systemd user manager and memory/CPU
controllers, Python, Weston, foot, XWayland and the compositor's runtime
dependencies. Preserve ordinary release compositor binaries and one shared pair
of `chonk-fullscreen-probe` and `chonk-input-probe` executables supporting
`animate-frame` and `--app-id`. Keep identical client binaries, workload settings
and helper sources for before/after. Run comparisons sequentially, with no
competing build, test or benchmark. Memory-profile binaries require `--memory`
and a separate diagnostic run; do not mix their timings with ordinary builds.

The five fixture tests perform no display launch or memory stress. Before a
campaign, run these checks in a separate non-timing slot:

```sh
python3 -B -m unittest discover -s scripts/tests -p 'test_mixed_capture_fixture.py' -v
python3 -B -m unittest discover -s scripts/tests -p 'test_bench_pressure.py' -v
```

## Fixed scope and payload choices

| Choice | MemoryMax | MemoryHigh, 75% | Retained synthetic payload | Churn block | Peak worker mappings |
| --- | ---: | ---: | ---: | ---: | ---: |
| `--memory 1G --payload-mib 512` | 1,024 MiB | 768 MiB | 512 MiB | 32 MiB | 576 MiB |
| `--memory 2G --payload-mib 1280` | 2,048 MiB | 1,536 MiB | 1,280 MiB | 32 MiB | 1,344 MiB |

The worker touches private anonymous pages and replaces its churn block every
two seconds, temporarily retaining both old and new blocks. Peak mappings are
payload plus two churn blocks, excluding Python and all other processes. The
worker requires at least 96 MiB of headroom below MemoryMax; that check does not
guarantee enough memory for the complete desktop workload.

The pressure runner uses two available CPUs on distinct physical cores,
`CPUQuota=200%`, `LP_NUM_THREADS=2` and `MemorySwapMax=0`. It validates effective
scope settings before releasing the workload. Choose suitable CPU ids for the
host; `2,3` below are the original experiment's choice. Only a newly created
random scope is constrained, and its observer remains outside that scope.

From the repository root, this reproduces the short 1 GiB/512 MiB calibration
workload. Replace the frozen paths and use new output directories for every run:

```sh
python3 -B scripts/bench-pressure.py \
  --memory 1G --memory-high-percent 75 --cpus 2,3 \
  --sample-seconds 0.25 --timeout-seconds 300 \
  --output /absolute/path/to/new-before-1g-scope -- \
  python3 -B scripts/bench-capture-mixed.py \
    --binary before=/absolute/path/to/frozen-before/chonkstep-wayland \
    --animation-probe /absolute/path/to/frozen-clients/chonk-fullscreen-probe \
    --input-probe /absolute/path/to/frozen-clients/chonk-input-probe \
    --foot /usr/bin/foot \
    --payload-mib 512 --churn-mib 32 --worker-seconds 240 \
    --runs 1 --warm-opens 3 --idle-seconds 3 --motion-seconds 2 \
    --motion-hz 120 --settle-seconds 1 \
    --input-deadline-ms 500 --capture-deadline-ms 500 \
    --output /absolute/path/to/new-before-1g-capture
```

For the second choice, change `--memory` to `2G`, `--payload-mib` to `1280`, and
use new `2g` output paths; all other settings stay identical. Repeat for
`--binary after=/absolute/path/to/frozen-after/chonkstep-wayland` in a fresh
scope. Collect at least three matched pairs per choice, alternating before/after
execution order between pairs. Preserve failed attempts. Each scope contains
one labeled compositor run, so its pressure counters remain attributable to
that workload. The worker and scope have bounded lifetimes; inspect the outer
result's cleanup status before beginning the next run.

The recorded longer paired campaign uses one scope per memory-limit choice,
with both `--binary before=...` and `--binary after=...`, `--runs 3`,
`--warm-opens 10`, `--idle-seconds 10`, `--motion-seconds 5`,
`--settle-seconds 2`, `--worker-seconds 800`, and outer `--timeout-seconds 900`.
That form alternates six fresh nested sessions inside the same constrained
scope. Per-build pressure attribution uses the externally sampled active time
windows, including their documented sampling-boundary excess; the kernel's
whole-scope peak cannot be assigned to a single build. Separate scopes per
labeled run, as above, make individual kernel counters easier to attribute but
are a different scope-lifetime experiment. Keep that distinction in reports.

## Evidence and interpretation

The outer directory retains requested/effective limits, periodic cgroup/process
snapshots, `memory.events`, memory/CPU PSI, CPU throttling, peak memory, command
status and cleanup results. Its runner fails on observed OOM events. If OOM
destroys the cgroup before the final snapshot, use retained periodic samples and
unit status and disclose that final-counter limitation.

The inner directory records executable/helper hashes, configurations, process
snapshots, animation callbacks, worker heartbeats, input receipts and capture
barrier timings. Unexpected client exit, stalled heartbeat, no animation
progress after capture closes, or missed input/open deadlines fail the fixture.
The 500 ms deadlines are explicit pass thresholds, not a 60 Hz frame guarantee.
Input receipt includes client logging and 1 ms polling; capture latency includes
nested rendering-barrier scheduling and is not physical input-to-photon time.

A cap or `memory.events high` count alone does not prove meaningful reclaim
stalls. Report actual `memory.pressure` some/full total deltas, CPU PSI and
throttling, memory events, working sets and process outcomes. If memory PSI shows
no stalls, describe a constrained workload without observed reclaim stalls.
Scope memory is not whole-machine RAM: shared cache pages can be charged outside
the scope. Anonymous page pressure in a real terminal does not emulate an actual
browser/application workload, old CPU caches, memory bandwidth, storage, or
physical GPU/DRM behavior. These measurements cannot establish that all low-end
machines will remain responsive under arbitrary pressure.
