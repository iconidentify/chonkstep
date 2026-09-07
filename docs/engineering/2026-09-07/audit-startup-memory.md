# Startup, shell, instruments and X11 performance audit

Review of worktree `chonkstep-capture-performance`, based on `c74772d`, on
2026-09-07. This records source evidence, bounded changes, component measurements
and an isolated visible-Dock comparison; it does not claim that every candidate
improves whole-session latency. The coordinator serialized builds and workloads;
the panel comparison ran in an explicitly granted exclusive slot. Earlier
results in `docs/performance.md` and the 2026-09-05 engineering log were checked
against current code. Final review is against r6 source; component and panel
measurements retain their original r3/r4 labels and are not relabeled as r6 runs.

## Ranked findings

### 1. Session environment publisher rescanned the entire current-session log

Baseline `scripts/wayland-session.sh:310–405`, `publish_portal_env`: every
second, `tail` started at the same initial byte offset and `awk` scanned every
subsequent line. For a steadily growing log, cumulative work grows quadratically
with session duration; unchanged logs still created several processes per poll.
The nested compositor benchmark launches the executable directly, so it misses
this wrapper cost. The implemented watcher retains one descriptor and byte
cursor, bounds chunks to 64 KiB and each read pass to 1 MiB, and checks a bounded
256-byte anchor when metadata changes. Unchanged polls use `stat` and sleep;
they do not reread session history. Late XWayland publication and session
activation cleanup remain supported.

The six isolated watcher regressions passed in
`portal-env-nul-tests-final.log` and the full wave-two gate. They cover bounded
history reads, generation changes, split/oversized/binary lines, late sockets,
rotation and copytruncate/regrowth. This establishes bounded parsing behavior,
not a measured whole-login speedup. Larger 50 MiB history comparisons remain an
optional component experiment, not a completed benchmark or claimed gain.

### 2. Sampler pipe handling converted valid large replies into eight-second failures

`crates/chonk-shell/src/widgets/sampling.rs`, `wait_with_deadline` (baseline
line 500): the worker waited for child exit before reading captured stdout.
A `pactl`/`busctl` JSON response exceeding the kernel pipe capacity blocks the
writer, then the eight-second timeout falsely reports missing device state.
After child exit, a descendant holding stdout open could also make the blocking
read outlive the advertised deadline. The Tailscale query already had a smaller
output workaround for the first failure mode.

Implemented: nonblocking pipe drain on the existing worker, pipe-readiness wait,
one deadline covering exit and EOF, complete UTF-8 validation, and a 4 MiB output
limit. Over-limit output fails as a whole; it never supplies a valid-looking
prefix. Failed direct children are killed/reaped; inherited pipe descriptors are
closed at the deadline. Existing effect process-group behavior is preserved.
No new reader thread. Tests exercise a 256 KiB subprocess reply, exact limit and
limit+1, split/invalid UTF-8, failure after partial output, a retained writer and
early stdout EOF. The final capacity/deadline checks passed the wave-two full
`scripts/check.sh all` gate, including 424 shell unit tests.

### 3. Closed instrument panels still pay for seven panel-only command sources

`chonk-instruments/src/sound.rs:298` registers the tile's volume query plus
three panel-only `pactl` readers (two at one second, streams at two seconds).
`wifi.rs:326` appends `LinkPanel::sources` (`link_panel.rs:216`): three `nmcli`
queries at three seconds and Tailscale status at five seconds. Their results are
parsed even while no panel is visible. Source cadence implies approximately
3.7 extra command starts per second when those commands are available, before
accounting for execution time. Seven sampler workers are potentially involved.
The already-fixed hidden-Dock suspension does not address a visible Dock with
closed panels. Bluetooth's BlueZ snapshot also drives its tile, so it cannot be
treated as panel-only.

Implemented and validated in the wave-two source: a defaulted SDK
activity query preserves declaration order and once-bound source ids. Sound's
sink-list reader remains active until its first successful discovery, preserving
the existing no-pactl fallback and recovery from an initially unavailable mixer.
After discovery, closed healthy panels stop all seven periodic command sources.
Pending switch/toggle reconciliation and explicit-rescan cooldowns retain their
existing finite sample budgets; permanent confirming-source failure retires the
associated pending state. Tile sources, including Bluetooth's shared BlueZ
snapshot, retain their existing cadence.

Opening invalidates old in-flight replies and requests a fresh snapshot even
when discovery or reconciliation already had a source active. Cached panel
pixels remain; controls require their relevant fresh sources, so a slow
Tailscale query does not gate connection controls and a slow Sound stream query
does not gate volume/mute. Sound now submits the entire ordered switch/migration
batch through RunAll rather than waiting for later pointer events to dispatch
the remaining moves. Effect completion can request one closed-panel reading
without restarting periodic polling; a hidden Dock still defers it until shown.
Panel grants are recomputed when data dirties the panel, using the existing
workarea and protocol limits, and buffers are replaced only when the clamped
dimensions change. This lets a cold first-open panel grow as rows arrive.

Worker lifecycle/one-shot/epoch regressions, native discovery/freshness/action
reconciliation fixtures, and the Desktop ownership/grant reuse test passed the
full gate: 424 shell, 251 instrument and nine SDK unit tests, alongside the
workspace's other checks. No source ids or resample handles are recreated when
a panel opens. Six panel worker threads are never started before first use;
Sound's sink discovery worker starts once and parks. Once used, these workers
remain parked, so thread-count savings do not persist across panel use.

The r6 readiness correction closes an additional Tailscale-specific hole:
receiving fresh text is insufficient when `parse_status` rejects it. Empty,
malformed and missing replies retain cached display state but revoke action
readiness; an unusable executable removes the row and its pending work. A fresh
valid status restores the action appropriate to the new state. Invalid fresh
samples still consume the finite reconciliation budget, and rejected stale
clicks create no pending work. The two regressions for invalid-after-reopen and
invalid-after-valid-opening passed in `capture-r6/panel-units.log`, alongside
253 instrument and 424 shell tests. This fix changes readiness, not the sampling
cadence measured below. The healthy r4 fixture does not exercise malformed
Tailscale replies and is not retroactive validation of that correction.

The frozen baseline and ordinary r4 executable were compared in one successful
10-second smoke pair, then three alternating pairs with two 20-second closed
phases per process. Complete private configs contain `omarchy_shell = false`
exactly once and show the native Dock. Real sampler workers invoke deterministic
healthy Bash replies for audio/network commands; every timed phase must retain
all four command-backed tile categories. The fixture never delegates control
actions to live services. Procfs/sysfs tile sources remain real and read-only.

| Median across three pairs | Baseline, before first open | r4, before first open | Baseline, after both panels close | r4, after both panels close |
| --- | ---: | ---: | ---: | ---: |
| Panel command starts/second | 3.45 | 0 | 3.75 | 0 |
| All fixture command starts/second | 6.75 | 3.50 | 7.15 | 3.45 |
| Reaped command-child CPU, percent of one CPU | 0.700 | 0.400 | 0.749 | 0.400 |
| Compositor CPU, percent of one CPU | 0.749 | 0.749 | 0.799 | 0.749 |
| All compositor thread context switches/second | 101.9 | 85.7 | 115.9 | 101.4 |
| Total compositor threads | 35 | 28 | 35 | 34 |
| Sampler threads, every run | 18 | 12 | 18 | 18 |
| Compositor PSS, KiB | 129,691 | 129,823 | 131,917 | 132,109 |

All six r4 closed phases issued zero panel commands. The source-derived 3.7/s
estimate excludes command runtime and phase-boundary quantization, explaining
the measured 3.45–3.75/s baseline range. The total initial seven-thread difference
includes the independent lazy capture-worker change; only six are panel sampler
threads. Compositor CPU is small and noisy, and PSS shows no demonstrated memory
reduction. Child CPU measures cheap synthetic Bash commands, not the real service
cost or total machine CPU. This is baseline versus the complete r4 candidate,
not an isolated panel-only patch comparison. Rendering uses llvmpipe with four
workers under headless Weston/pixman; it does not establish DRM/KMS or old-machine
performance.

First-opening medians expose a tradeoff. Sound mapping was 27.20 → 27.41 ms;
Link mapping was 26.99 → 39.38 ms. The following forced-frame/empty-backend-queue
barrier was 27.35 → 52.92 ms for Sound and 27.11 → 80.09 ms for Link. That barrier
includes settling work and is not a direct first-pixel or input-ready measurement.
Every candidate panel query started within 1.0–1.7 ms of the click; baseline
source medians waited 82–348 ms for Sound and 984–2,903 ms for Link's existing
cadence. Command start does not prove parsing or control readiness. In every r4
run Link first mapped at 230×163 and grew to 230×286, matching the baseline's
populated size; Sound stayed 398×108. Cached presentation, immediate refresh,
and fresh-control gating remain separate aspects of the behavior.

Evidence: the sibling artifact directory
`chonkstep-capture-performance-artifacts/2026-09-07/panel-sampling-three/` holds
executable/fixture hashes, configs, command logs, raw process snapshots,
`summary.json`, `opening-summary.json` and `RESULTS.md`. The reusable harness and
synthetic command definitions are in `panel-sampling/`; the successful smoke is
in `panel-sampling-smoke/`. No artifact-only fixture repair was needed. All owned
benchmark processes were stopped before returning the workload slot.

### 4. Every Dock repaint also rasterized fully clipped widget faces

`chonk-shell/src/desktop.rs`, `tick_items` and `redraw_dock` (baseline lines
2222 and 3198): any changed tile caused the column, logo and every widget to
render. `item_slots` included slots entirely below the output-height clamp.
An active clock or sampler therefore rasterized instruments that could not
contribute a pixel on a short screen or large scale. Pointer hit testing also
allocated a complete slot vector before finding one match.

Implemented the smallest proven change: lazy slot geometry for hit testing;
collect only the visible prefix before borrowing renderers; keep partly visible
tiles, all widget updates and remote lifecycles. A regression counts renders and
updates through exact clip boundaries, one-row visibility, reveal, scale,
appearance and reorder. No per-widget cache was added: such a cache must also
honor hover, press/effects, panel state, external samples, eviction and theme
changes. Measure a short/HiDPI visible Dock separately; a tall fixture with every
slot visible is not evidence for this clipping improvement.

### 5. Application collation had quadratic id deduplication

`chonk-shell/src/apps.rs:210`, `collate_scanned`: each source searched the full
chosen vector. Distinct ids alone require N(N−1)/2 comparisons, or 8,386,560
comparisons at 4096 ids, before parsing and sorting. Multiple XDG roots increase
work. The old comment assumed systems had at most a few hundred entries.

Implemented an owned-key hash map. Strictly lower directory rank replaces an
entry; ties retain the first source; deduplication still precedes parsing so
`Hidden`, invalid and `TryExec` overrides cannot expose the system copy. Final
name/id sorting remains deterministic. A shuffled catalogue is checked against
the previous algorithm. The ignored `benchmark_application_collation` alternates
old/new algorithms for 64, 512 and 4096 ids, with fixture construction excluded.
The frozen r3 release component benchmark measured 65.404 → 47.543 µs at 64 ids,
2.003 → 0.456 ms at 512 ids and 122.791 → 3.244 ms at 4,096 ids, with four sources
per id. Fixtures were constructed outside the alternating measured sections;
the old/new behavior oracle passed. These establish catalogue-processing cost,
not a measured whole-startup percentage. No allocator-traffic or peak-memory
claim is made for the hash table. Evidence is in
`capture-r3/microbenchmarks/chonk_shell-benchmark.log` and its `commands.json`.

### 6. Deferred: duplicate Hyprland configuration reads and watch allocations

`chonk-shell/src/shell.rs:1190`, `hyprland_watch`, explicitly rereads files that
`wm_config::load` already read to resolve `SessionState`. On a watch event,
`shell.rs:3252` reads the graph for reporting/watch membership, then `reresolve`
calls `resolve_live_config` (`1601`), which loads and parses it again. This is
filesystem I/O and parsing on the shell thread. Separately,
`wm-config/src/hyprland/mod.rs:1303`, `Watch::signature`, clones every watched
path into fresh vectors once per second even when only metadata needs comparing.

Conservative API proposal, not implemented: return resolved config plus optional
read provenance from the one loader; initialize/follow the watch using that same
read. Keep precedence, fallback/reporting, newly added include discovery and
failed-config rollback intact. A watch signature can store metadata parallel to
stable path identities and rebuild identities only when its file set changes.
Measure file-open/stat counts and main-thread duration against 40/256-file
fixtures. Existing include depth/file/byte budgets and cycle detection remain.

### 7. Deferred: appearance changes can accumulate hung workers

`chonk-shell/src/appearance.rs:312`: every gesture spawns a worker that calls
blocking `gsettings.output()` without a deadline. A stuck bus/tool retains one
thread per gesture; independent workers can finish an old appearance after a new
one. Deferred proposal: one bounded worker with a latest-request generation and
bounded subprocess waits, preserving the managed GTK theme-pair test and the
environment gate that keeps nested tests from changing the real desktop.

### 8. Deferred: effects and app reapers have one thread per concurrent launch

`widgets/sampling.rs:457`, `run_detached`, starts a thread per action batch, each
command allowed 120 seconds. Repeated sound scroll reports can accumulate many
workers when `wpctl` is slow. `spawn.rs:165,321` similarly retains a waiting thread
per live application/dockapp. They do keep waits off the compositor and correctly
reap children; do not replace them with a global SIGCHLD policy that races
Smithay/XWayland. Proposed experiment: gated helper plus many gestures/launches,
count worker stacks and verify action order. Any queue/reaper redesign must bound
concurrency without silently dropping relative adjustments or killing GUI apps.

### 9. Deferred: X11 startup atoms and resize notifications make serial round trips

`wm-x11/src/backend.rs:306` performs ten consecutive intern/reply pairs;
`EwmhAtoms::intern` (`1500`) does another group sequentially. Conservative
proposal: issue cookies together, then collect replies with the same error
semantics. Measure actual X11 startup before assigning a gain; localhost RTT is
small, remote/nested server latency makes the case larger. `resize_client`
(`2857`) also waits for `translate_coordinates` for each synthetic ConfigureNotify.
The comment says this “costs nothing”; it does cost a round trip. Preserve the
ICCCM compatibility behavior and prove any cached-position alternative against
reparent/move/resize combinations before changing it.

### 10. Capture encoding keeps avoidable full-size buffers

`wm-wayland/src/capture_tool/worker.rs`, `screenshot`: tiny-skia's encoder clones
the RGBA image to demultiply alpha, then the PNG encoder accumulates compressed
and final PNG vectors. A 4K RGBA clone alone is 31.6 MiB. The worker also used to
retain image/PNG ownership through file and clipboard finalization. Streaming
the already-owned pixels via one demultiplied row to the private output file
preserves tiny-skia rounding with only width-proportional scratch. The png0.18
crate already exists in the lockfile. Explicitly finish both stream and outer
writer before fsync/publication; errors during IEND must not be ignored by Drop.
The final r4 implementation fuses exact demultiplication with Sub filtering and
joins the filter byte to its row before passing it to the original fast
compressor. Bounded IDAT chunks then go through the PNG outer writer. This
replaces the earlier generic StreamWriter version, whose split filter/row writes
regressed compressor performance. Compression, every IDAT write and outer
finish are checked; underlying failures are latched, including partial writes,
checksums, IEND and flush. A recoverable one-shot error must not publish a corrupt
file. Pixel/all-alpha, compressed-stream, tiny-chunk and injected-error oracles
passed, as did the requested-allocation budget below one full source image.

The authoritative r4 `microbenchmarks/png.log` records three alternating timed
rounds after warmup. Pure encoding excludes fixture construction, allocation
counting, filesystem writes and fsync:

| Fixture | Previous encoder, ms | Bounded encoder, ms | Previous requested bytes | Bounded requested bytes |
| --- | ---: | ---: | ---: | ---: |
| 1920×1080 structured | 11.464 | 12.201 | 23,040,945 | 138,753 |
| 1920×1080 noise | 17.496 | 17.566 | 92,467,946 | 138,753 |
| 3840×2160 structured | 51.737 | 47.604 | 92,116,056 | 146,433 |
| 3840×2160 noise | 117.105 | 69.881 | 369,783,766 | 146,433 |

The candidate makes three allocation requests in each fixture; these cumulative
requested bytes are not peak live memory or RSS, and the owned source image is
additional. Noise output is approximately 8.9% larger because the bounded path
does not apply the old whole-image stored fallback. There is no universal speed
or file-size improvement. The older r3 generic-stream timings are superseded.
The final r6 worker source is byte-identical to the measured r4 worker
(`bbbb7e1d…5113427` SHA-256); the timing evidence still belongs to r4.

Deferred: the capture worker's `xdg-user-dir` and `date` probes still use unbounded
`output()` calls. They run off the compositor thread but a hanging replacement
blocks all capture jobs. Recording's external `kill` invocation also uses a
blocking `status()` before its bounded recorder wait. Bounded helpers or
in-process equivalents preserving the existing fallbacks would address these
remaining queue stalls; no improvement is implemented or measured for them.

## Final opener, packaging and lifetime review

No concrete blocker was found in the final routes. A successfully published PNG
goes to `xdg-open` with one literal absolute-path argument; completed remuxed MP4
goes directly to `omacut`. The installed desktop entries use `imv %F` and
`omacut %f`, and Omarchy's shipped PNG association is `imv.desktop`. The product
does not override the user's chosen MIME handler. Both Arch package recipes and
the source installer add `xdg-utils`; Omacut remains provided by Omarchy, with
its absence reported while the saved recording remains intact. This was a
read-only packaging review, not a new installation or real-viewer launch.

The capture changes introduce no MIME-default, Hyprland, or Omarchy preference
writes. Existing installer theme export behavior is separate and unchanged.
Openers use literal argv without shell interpolation and reset inherited
INT/TERM/HUP signal state. Unit and native fixtures check routing, hostile/non-UTF8 argument
boundaries, launch failure and publication ordering through isolated launchers;
they do not measure a real viewer's startup or memory consumption.

The I/O worker stays lazy. It tracks at most 16 direct review processes and
eight notification helpers; notifications have a two-second deadline serviced
by worker polling. Live review apps are not killed on logout. These bounds cover
tracked direct children, not arbitrary descendants or single-instance apps that
detach from their launcher. Clipboard candidates remain serialized: at most two
unresolved capture paths, one prior provider plus one candidate, and admission
held until a result. The 100 ms probe checks liveness rather than proving
Wayland selection ownership. Real clipboard bytes are covered by native tests;
deterministic failed-replacement/clipboard-manager handoff is a remaining
validation limit. Encoder allocation counters exclude admitted input images,
file cache, clipboard processes and review apps.

The deferred items above remain opportunities for a later bounded campaign.
They are neither completed optimizations nor evidence of additional startup,
memory or whole-session speed gains.

## Coverage and rejected assumptions

| Area | Reviewed inventory and result |
| --- | --- |
| Runtime scripts | `chonkstep-session`, Wayland/X11 sessions, start-session; wrapper rescan above. Install/check/dev/E2E scripts inventoried separately from login cost. |
| wm-config | Main/preset loaders, Hyprland graph/conf/Lua/dispatch/rules/key modules; double reads and watch allocation above; existing budgets retained. |
| chonk-shell | Startup, shell/desktop, apps, sampler/spawn, appearance, wallpaper, launchdock, menu/follow/export/host, control, session layout, dockapp registry/tile/panel/handoff, Overview. Existing bounded socket/frame queues and hidden-Dock lifecycle reviewed. |
| chonk-instruments | Net, sysload, sound/audio panel, Wi-Fi/link panel, Bluetooth/BlueZ panel, power, clock; source inventory distinguishes tile state from closed-panel data. Power already uses a ten-second cadence; clock has no sampling thread. |
| wm-theme | Raster/paint caches plus icon, menu, tile and instrument renderers. Shared FontState and bounded glyph cache already exist; old quadratic text fitting is already fixed. `icon::draw_preview` now borrows PixmapRef with exact length validation. Old-renderer pixel/fallback checks and the public 56px-icon/1080p-preview allocation regression passed the full gate. This proves removal of the source copy, not an independently measured icon-latency or RSS gain. |
| wm-x11 | Backend initialization/atoms, RandR/output inventory, render/capture and resize calls; X11-specific round trips above. |
| Capture worker | Re-reviewed earlier opener changes, bounds, deadlines, retention and PNG path; compositor thread remains isolated. |

`Desktop::new` maps Dock/Clip but does **not** rasterize their widget faces:
its final raster operation is `repaint_wallpaper`; sampler threads are already
deferred. The observed dock-socket→control-socket startup interval also includes
wallpaper rendering and menu construction, so it cannot yet be attributed to
hidden Dock painting. Passing the already-resolved remembered visibility into
construction could avoid map/unmap and make logo decode lazy, but needs separate
measurement. Do not duplicate preference resolution in a new constructor.

`session_layout::write_atomic` writes/renames on the event loop after debounce;
it does **not** fsync. Background persistence might help slow storage, but must
preserve latest-state ordering and failure retry. It is not a demonstrated
fsync stall. X11's `picom --no-use-damage` is an explicit workaround for stale
restacks; deleting it for a benchmark without visual correctness evidence is
not an acceptable optimization. No cold-login, native scanout or low-end
hardware speed claim follows from the nested measurements alone.
