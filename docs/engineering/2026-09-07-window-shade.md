# Window shade side-strip regression

Shading a window left two vertical lines below its titlebar. The earlier fixes
were still present: `9099bcf` shortened the decoration layout and `1cf05f2`
requested a full repaint when chrome was retired. The latter tested rectangle
arithmetic without checking the rendered scene.

The sparse painter introduced in `4a11a45` builds side strips from
`DecorationRequest.content_size.h`. `shaded_paint_inputs` shortened
`layout.frame_size.h` but retained the full content height in the request. It
therefore produced fresh full-height side strips on every shaded repaint. The
frame's geometry does not clip those buffers on Wayland, and full damage simply
draws the incorrect strips again. Existing core tests used a full-buffer fake
theme and only recorded `surface.frame_size`, so they could not see this.

The fix sets the cloned paint request's content height to zero. That removes
the side strips and lets the sparse painter produce the titlebar's bottom
border. The client's actual content geometry and restore layout remain intact.

Two regressions failed before the fix:

- The real raster test found a `1x200` side strip at `(0, 24)` outside a
  `302x25` shaded frame. It now checks every built-in theme in both appearances
  at scales 1, 1.5, and 2, including changed title/focus inputs, and compares
  assembled sparse pixels with the full painter.
- The nested Wayland test captured the two lines and failed at `(0, 25)`:
  black chrome remained where the empty-desktop baseline had wallpaper. It
  now passes at scales 1, 1.5, and 2, checking every pixel below the shade,
  repeated keyboard shade/restore, titlebar double-click and release, focus
  repaint, dragging, and restored pixels.

Run the raster regression with
`cargo test --locked -p wm-core shaded_sparse_chrome`. Run the live regression
with `scripts/e2e.sh --headless --test window_shade`, or build
`chonkstep-wayland` and `chonk-testkit` and run
`cargo test --locked -p chonk-testkit --test window_shade -- --ignored --test-threads=1`
inside a Wayland session. The latter was used locally; the existing DRM session
was not replaced. Screencopy rerenders the scene, which still exposes this bug
because the invalid strips are current scene elements, not stale buffer pixels.
