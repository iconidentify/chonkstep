# ChonkStep 0.3.1 performance + compatibility update

We just completed a deep pass over input, clipboard, gaming, memory, startup
work and the Rust architecture behind ChonkStep:

- Microsoft Edge text selection is now correct at 1x, 1.5x and 2x scale,
  windowed and fullscreen. The unchanged compositor reproduced the misplaced
  caret at both fractional and 2x scale; the fixed build passes the full real
  Edge and Chromium matrix.
- Held-key repeat is reliable across native Wayland and X11 apps, including
  zero-rate repeat disable, live timing changes, layout reloads, modal UI and
  focus transitions. We also fixed the legacy Wayland keymap terminator.
- Clipboard and primary selection now pass 19 real Wayland/X11 transfer and
  lifecycle cases: UTF-8, binary data, large INCR transfers, slow readers,
  backpressure, cancellation, owner death and XWayland restart.
- A stalled 8 MiB clipboard consumer previously added 8,196 KiB of compositor
  anonymous PSS in every measured run. After bounded staging: 0 KiB in 7/7
  matched runs, with every resumed payload still byte-perfect.
- The hot X11-to-Wayland partial-write path went from 8,192 temporary
  allocations requesting 260 MiB cumulatively to zero in the controlled 8 MiB
  workload. A real pressured transfer went from 288 unread-tail allocations to
  zero. These are allocation-traffic figures, not resident-RAM claims.
- Selection devices are now retired when clients disappear—even after SIGKILL.
  A 10,240-object lifecycle test finishes with zero live records. In matched
  two-hour desktop churn runs, requested-live Rust growth fell 61.2%, observed
  peak live allocation fell almost exactly 10 MB, and anonymous+swap growth
  fell 56.4%. Both runs kept non-pipe FDs and thread counts flat.
- Pointer lock and confinement now honor regions immediately, release across
  Overview/focus transitions and avoid invisible redraw work. A locked-pointer
  workload went from 128 render attempts to zero; an 8,192-report burst also
  produced zero render attempts and zero submissions while preserving every
  raw delta.
- Client-driven move/resize now validates the exact live pointer serial, seat,
  client identity and grab type. Zero, stale, cross-client and drag-and-drop
  serials can no longer be repurposed as window-management authority.
- A configure-order race found by a real Chromium test is fixed. The unchanged
  build can lose a requested resize on the first controlled round; the fixed
  build delivers the exact requested geometry in 16/16 rounds.
- X11 lifecycle handling is substantially tougher: first-key focus and repeat,
  withdraw/remap identity, client teardown, selection cancellation and
  generation-safe XWayland restart all have real-session coverage.
- Chonkcraft menu clicks and geometry now have a real 1x/1.5x/2x windowed and
  fullscreen compatibility matrix. Both old and new builds pass, so we gained
  a permanent gaming guardrail without pretending we reproduced the historical
  click report.
- Omarchy's initial menu is parsed once instead of twice, stale menu actions
  cannot resolve against replacement content, and panel source completions are
  no longer lost behind unrelated tile freshness.
- The periodic NetworkManager parser now allocates nothing for ignored virtual
  devices or bridge profiles (previously 1,792/1,948 allocations per 256 rows),
  with exhaustive equivalence coverage over 19,531 escaped input strings.
- The 56px dock clock now needs 32 allocator requests per render instead of 52
  (-38.5%), with byte-identical output verified for every size from 8–128.
- Large Wayland backend modules were split along input, selection, XWayland and
  state ownership boundaries, and the new allocation meter remains dev-only so
  none of the measurement machinery enters the shipping dependency graph.
- CI now distinguishes requested fullscreen geometry from the pixels a client
  has actually committed, eliminating a real Chromium presentation race with
  an observable frame fence instead of sleeps, retries or weaker assertions.
- The frozen optimized binary finishes a final 199-case nested release suite
  plus all three installed-Omarchy checks with no skipped clients.

How we tested: matched 7,202-second baseline/candidate lifecycle soaks, 59,089
combined churn cycles, paired 60-second startup/idle samples, 14 byte-verified
clipboard trials, a 306-second allocation-profile pair, and a final 202-case
nested/installed-Omarchy release gate. Raw samples, screenshots, executable
hashes and known limitations are preserved.

The complete raw logs, immutable binary hashes, before/after tables and known
limits are preserved in the repository. These results come from controlled
nested software-rendered tests; we are not claiming a Hyprland RAM/startup win,
native DRM/KMS frame-time result or universal hardware validation until we have
matched measurements for those too.

ChonkStep is becoming a faster, safer and much more credible Omarchy-compatible
desktop—one reproduced bug and one honest benchmark at a time.

https://github.com/iconidentify/chonkstep
