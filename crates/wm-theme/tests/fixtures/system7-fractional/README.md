# Fractional System 7 implementation goldens

These 144 cases pin ChonkStep's documented fractional rounding at 1.25× and
1.5×. They cover focus, zoom availability, shading, pressed boxes and three
atlas-only titles. Unlike the neighboring `system7` fixtures, they come from
our renderer and are **not** historical OS screenshots. The independent 1×/2×
OS-capture oracle is tested separately.

`fractional.bin.zlib` contains length-prefixed case names and payloads. Each
payload records layout metrics and sparse part offsets/dimensions, followed by
their RGBA bytes. The source matrix lives in `tests/support/system7_fractional.rs`.
Generated on 2026-09-11 while implementing issue #163, after matching every
monochrome OS oracle. The generator refuses an existing output directory:

```sh
cargo run -p wm-theme --example system7_fractional_goldens -- --write NEW_DIRECTORY
```

Review changes against `docs/decoration-styles/system7.md`; do not regenerate a
failed golden merely to make a changed renderer pass.
