# Memory behaviour and measurement

chonkstep creates large, short-lived pixel buffers while opening the
Overview, changing themes, and serving screenshots. On glibc, the
allocator's adaptive mmap threshold can move one of those buffers into
the main heap arena after an earlier large allocation. Freeing the
buffer returns it to the allocator but may leave the process's `[heap]`
mapping at its high-water mark, making an idle compositor look as if it
still owns the buffer.

Both the X11 and Wayland session binaries therefore set glibc's
`M_MMAP_THRESHOLD` to 128 KiB and `M_TRIM_THRESHOLD` to 256 KiB before
logging or worker-thread startup. Large transient allocations get
separate mappings that disappear when freed, and excess arena space is
eligible for trimming without a periodic maintenance wakeup. Other
libc implementations keep their native allocator policy.

## Reproducing the measurement

Use the same release build, output layout, theme, and client set for
both snapshots. First identify the compositor process and create an
artifact directory:

```sh
pid=$(pidof chonkstep-wayland 2>/dev/null || pidof chonkstep)
pid=${pid%% *}
test -n "$pid"
artifact_dir=$(mktemp -d /tmp/chonkstep-memory.XXXXXX)
```

Capture the initial mappings and glibc heap details:

```sh
cp "/proc/$pid/maps" "$artifact_dir/before.maps"
cp "/proc/$pid/smaps" "$artifact_dir/before.smaps"
grep '\[heap\]' "$artifact_dir/before.maps"
awk '
  $NF == "[heap]" { heap = 1; print; next }
  heap && /^(Size|Rss|Private_Dirty|Swap):/ { print }
  heap && /^VmFlags:/ { exit }
' "$artifact_dir/before.smaps"
```

Now open and close the Overview, switch to another theme and back, and
take a screenshot with `grim`. Wait five seconds for the compositor to
become idle, then capture the same files again:

```sh
grim "$artifact_dir/screenshot.png"
sleep 5
cp "/proc/$pid/maps" "$artifact_dir/after.maps"
cp "/proc/$pid/smaps" "$artifact_dir/after.smaps"
grep '\[heap\]' "$artifact_dir/after.maps"
awk '
  $NF == "[heap]" { heap = 1; print; next }
  heap && /^(Size|Rss|Private_Dirty|Swap):/ { print }
  heap && /^VmFlags:/ { exit }
' "$artifact_dir/after.smaps"
```

Compare the `[heap]` mapping range and its `Size`, `Rss`,
`Private_Dirty`, and `Swap` values. After the transient workload, the
heap extent and resident size should remain within a few MiB of the
initial snapshot. `/proc/$pid/smaps_rollup` is useful as an additional
whole-process snapshot, but it includes mapped libraries, shared
buffers, graphics allocations, and clients embedded in the shell; it
is not a measurement of the glibc heap alone.

For the allocator-level regression, the sequence that exposed the
adaptive-threshold ratchet grew the arena from 132 KiB to 15,496 KiB
under glibc's defaults. With chonkstep's fixed policy, the same
allocate, touch, and free sequence left the arena at 132 KiB and only
68 KiB of additional heap pages resident. The subprocess test in
`chonk-shell::startup` repeats that allocation shape without changing
the allocator used by unrelated tests.
