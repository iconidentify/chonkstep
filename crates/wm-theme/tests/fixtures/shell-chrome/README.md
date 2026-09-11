# Shell chrome implementation goldens

120 cases: both styles × 1/1.5/2 scales × classic/Amber Phosphor palettes ×
root menu, window menu, cascade, switcher, minimized icon, fallback Overview,
selection, normal/inverted native caption and desktop close control.

These are reviewed, original ChonkStep shell designs. They are **not historical
System 7 captures**. The separate System 7 document-frame fixtures remain the
OS-derived acceptance oracle. The original atlas supplies these System 7 labels;
WindowMaker uses the already-licensed, test-only DejaVu font in the sibling
`windowmaker` fixture directory for deterministic output.

The compressed records preserve case names, menu hit/close rectangles, dimensions
and every RGBA byte. `tests/support/shell_chrome.rs` also compares every
WindowMaker output to its existing public renderer on every run, so adding the
style context cannot silently change its legacy behavior.

To propose a deliberate change, generate into a **new directory**, inspect the
PNGs and explain the pixel/geometry difference before replacing this oracle:

```sh
cargo run -p wm-theme --example shell_chrome_goldens -- --write /tmp/new-shell-oracle
```

Initial source: the #164 follow-up to main `d66dc2d` (PR #167). PNGs were visually
reviewed at 1× and 2×; native application/menu interaction and screenshot checks
live in `chonk-testkit/tests/shell_chrome.rs`. The native Overview test also
changes a real client's pixels while Overview remains open, in both styles,
without enabling capture previews or resizing clients.
