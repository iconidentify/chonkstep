# ChonkBlocker native Wayland resizing

ChonkBlocker on JBR 25/Vulkan could jump much larger or smaller during a drag,
and its rendered surface could separate from the server title bar. A protocol
capture on a 150% display showed a 2548x1622 GPU buffer committed with a 1328x853
viewport destination. The buffer/destination mismatch was temporary, but the
inferred surface density changed and fed incorrect sizes back to the client.

The compositor retains the density used for an outstanding resize until the
requested geometry is committed at that density and the drag has ended. A
matching frame between pointer motions must not release it: another delayed
buffer can arrive before the next motion. Rendering, input, and frame
geometry share that factor. An actual output-scale change releases it. Accepting
an already-committed size, including a terminal's cell-grid adjustment, releases
the retained factor once the buffer density agrees, so subsequent application
density changes still work.

Mapping also acknowledges the client's initial dimensions. JBR otherwise keeps
its startup resize pending and can ignore the first user drag. Map-time window
rules and fullscreen/maximize requests can still override those dimensions
before the deferred configure is sent.

## Integration with current resize handling

This builds on the configure-serial checks, client-size acceptance without an
extra configure, and presentation fitting already on `main` (introduced in
`f75bdc9`). It does not restore the old size-history filter: a client that has
committed the latest configure can legitimately choose a prior size, for
example when a terminal adjusts to its cell grid. The retained density only
prevents a delayed GPU allocation from changing the coordinate system.

The burst probe sends 32 requests, acknowledges the first 31 while retaining an
older rendered buffer, and commits before reading the final configure. That
old frame cannot erase the final request. Committing the old size *after* the
final acknowledgment would instead be a valid client-selected size under the
current configure contract.

## Reproduction and validation

Run the nested protocol regressions with:

```sh
scripts/e2e.sh --headless --release --test resize_order
scripts/e2e.sh --headless --release --test scale_change
scripts/e2e.sh --headless --release --test fullscreen
scripts/e2e.sh --headless --release --test hyprland_coordinates
```

`resize_order` checks the initial acknowledgment, delayed buffers across staged
requests and a burst of requests, terminal cell-grid adjustments, and painted
pixels while a buffer trails a growing or shrinking frame. `scale_change`
replays the JBR buffer/viewport mismatch at 100%, 150%, and 200%, alongside
application density changes, output-scale changes and pixel/frame assertions
for every decoration style. A held-drag regression first commits a matching
frame, then a delayed buffer before the next pointer motion; it also verifies
that a deliberate density change works again after release. The other suites
cover map-time window states and logical IPC coordinates at different output
scales.

Validation after integration with `main` at `e5a1af1`:

- All 16 nested regressions pass: four resize-order, three scale-change, seven
  fullscreen and two logical-coordinate tests.
- 347 compositor unit tests and strict Clippy for `wm-wayland` and all
  `chonk-testkit` targets pass.
- The held-drag regression fails against the first integrated build, which
  releases density after an intermediate matching frame, and passes once the
  density is retained through release.
- The real ChonkBlocker/JBR Vulkan game was tested in a separate visible
  compositor on an NVIDIA RTX 3090. The first requested 1500x900 size was
  honored; 100 corner-drag samples stayed within the gesture (1497–1616 wide,
  897–956 high, including IPC rounding). The final 999x599 Java frame, root,
  content and viewport matched the 1498x898 physical frame within fractional
  rounding, and the captured Endless pause screen was complete. One launch
  attempt stalled before the game mapped; the subsequent isolated run completed.

The app-side changes are tracked separately in `chonkblocker` commit `7ca33e7e`.
