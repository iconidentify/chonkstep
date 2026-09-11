# Decoration styles

The palette and frame recipe are independent:

```toml
theme = "nextstep-classic"         # or another palette, including "omarchy"
appearance = "dark"                # session palette preference
decoration_style = "system7"        # "windowmaker" by default
```

Edit `~/.config/chonkstep/config.toml` and run `/usr/lib/chonkstep/reload.sh`. The selector is
config-only; there is no competing state file, request file or root-menu picker.
Unknown names and non-string values produce a warning naming both accepted
values and retain WindowMaker while the rest of the config applies.
`--check-config` reports the diagnostic and `--print-config` prints the effective
style. The control socket's `theme` event and the session's look-change log carry
`decoration_style` too.

WindowMaker keeps its existing frame, title and control behavior. System 7 uses
the measured classic document-window recipe: close left, zoom right for resizable
windows, striped active title, a one-pixel outline and offset shadow, and no
miniaturize button. Its chrome is always light. [The reference specification](decoration-styles/system7.md)
describes its measured pixels, original title atlas, palette roles and exact
integer versus faithful fractional scaling. Theme following continues to supply
the active style's palette.

Reloading reflows each managed frame once through the same backend path as a
theme change. Floating clients keep their content geometry, workspace and stack
position. Spatial layouts recompute their available client area for the new
chrome. An active move/resize is rebased to the new frame so the next pointer
motion uses current offsets and opposite edges. Minimized/shaded frames are
updated for their eventual restore; fullscreen and client-decorated windows keep
their normal decoration exemptions.

Mac keyboard behavior is a separate feature: changing decoration style does not
change Command-key translations, shortcuts or Spaces policy.

## Rendering and verification

`RasterThemeEngine::with_style(DecorationStyle)` explicitly selects a renderer and
returns an error for a reserved but unavailable style. Existing constructors keep
their signatures and default to WindowMaker. `SUPPORTED_DECORATION_STYLES` lists
only implemented renderers, so tests and tools never exercise a silent stand-in.
The frame recipe lives in `wm-theme/src/styles/windowmaker.rs`; font discovery,
glyph/title caches and per-scale variants stay in the shared engine.

Offline inspection preserves the original positional arguments:

```sh
cargo run -p wm-theme --example dump_decoration -- --style windowmaker 2 "Terminal" /tmp/frame
cargo run --release -p wm-theme --example performance -- --style windowmaker
```

The [720-case compatibility oracle](../crates/wm-theme/tests/fixtures/windowmaker/README.md)
pins pre-refactor WindowMaker geometry and every sparse pixel, with a fixed
test-only font. Its explicit generator refuses to overwrite existing fixtures.

## Performance contract

Every style inherits the gates in [issue #159](https://github.com/iconidentify/chonkstep/issues/159).
The [September 11 baseline](benchmarks/decoration-styles-2026-09-11/README.md)
records main before the style refactor, including executable hashes and raw samples.

- Render/layout microbenchmarks cover 800×600, 1280×800 and 2560×1600 content
  at scales 1 and 2, with cold and warm title caches, allocation counts/bytes,
  exact retained pixel bytes and output checksums. WindowMaker must remain
  within measured run-to-run noise; a new style must not exceed its equivalent
  WindowMaker workload. Archive every sample and compare the same binary/config.
- `decoration_contract` pins the actual existing allocation budgets: layout has
  four requests/600 bytes; warm rendering has eight requests with size-dependent
  bytes. The owned-buffer API currently allocates. A style must not raise these
  budgets or call itself allocation-free while making the same copies.
- Sparse parts stay within the frame, never cover client pixels, never overlap,
  and retain at most perimeter × maximum band width × four RGBA bytes, across
  focused/inactive, resizable/fixed, shaded and button states at 1/1.5/2 scales.
- A real renderer through the wm-core fake backend verifies title/focus/button
  changes preserve non-title pixels. Backends omit unchanged band damage/uploads;
  X11 size changes invalidate retained server pixels, and failed uploads retry.
  A palette which actually changes a side-band color must still repaint it.
- The real renderer receives exactly 60 raster calls for 125 synthetic resize
  inputs across 60 frame boundaries. Pure moves reuse chrome. New styles join
  this same test, rather than a separate mock implementation.
- Nested release sessions measure first-scene readiness, three-client readiness,
  loaded idle CPU/RSS and a real 125 Hz titlebar drag. Use alternating before/after
  runs, retain medians/ranges/raw samples, and distinguish software nesting from
  native GPU/KMS measurements.
- Rebuilding an engine with resident `FontState` performs no file/process I/O.
  A disposable Linux test child installs a syscall tripwire, with a negative
  control proving forbidden I/O terminates it. Initial font discovery and warming
  occur before this boundary. Style atlases must be embedded and initialized at
  construction; tests for new fallback glyph paths must exercise those paths too.
- Add each implemented style to the contract tests and the real mixed-DPI
  `chonk-testkit/tests/scale_change.rs` pixel matrix. All tests run in CI; missing
  dependencies or skipped checks are not passing evidence.

The style seam must preserve `wm_theme_api::ThemeEngine`'s contract. Golden
fixtures are generated deliberately from a named source revision with a committed
generator. System 7 reference goldens must come from genuine emulator captures,
with source and pixel coordinates; renderer output cannot serve as its own oracle.
