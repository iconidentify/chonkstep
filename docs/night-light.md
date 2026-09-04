# Night light and color control

The Wayland session supports both color-control paths used on Omarchy
and wlroots-style desktops:

```text
hyprsunset / Omarchy Night Light
  → hyprland_ctm_control_manager_v1 v2
    → per-output gamma LUT

wlsunset / gammastep / redshift
  → zwlr_gamma_control_manager_v1
    → per-output gamma LUT
```

On the DRM backend the final step programs the CRTC with
`DRM_IOCTL_MODE_SETGAMMA`. The nested winit backend has no CRTC; it
advertises neither color-control global unless the explicit test gamma
stand-in is enabled. A client therefore receives a clean unsupported
answer instead of reporting success for a screen that cannot change.

## Omarchy and `hyprsunset`

Omarchy's unmodified `omarchy-toggle-nightlight` and Quickshell
indicator use `hyprsunset`, then query its separate IPC socket through
`hyprctl hyprsunset temperature`. They do not use wlr gamma control.
Chonkstep consequently implements `hyprland-ctm-control-v1` v2 rather
than replacing either program.

`hyprsunset` sends diagonal color-transform matrices for temperature
and gamma. Chonkstep lowers each diagonal coefficient to a channel
gain over the output's original gamma ramp. The change is transactional:
`set_ctm_for_output` stages values and `commit` applies them together.

A real-client end-to-end test runs `hyprsunset -t 3000`, confirms it
stays alive, verifies `hyprctl hyprsunset temperature` returns `3000`,
observes the reduced blue channel in the hardware path, kills the
client, and verifies restoration. This is the same route Omarchy's
6500↔4000 toggle and bar indicator use, so no night-light shim or
upstream patch remains.

## Safety and arbitration

- One color-control owner may hold the outputs at a time. A second CTM
  manager receives the v2 `blocked` event. A wlr gamma client receives
  `failed` while CTM owns the ramps, and vice versa.
- The original ramp is captured before the first change and restored
  when the owner destroys its object, disconnects, crashes, or the
  session ends.
- A VT switch can reset hardware LUTs; the active ramp is programmed
  again when the session regains the seat.
- Negative, non-finite, or non-diagonal CTMs are rejected with the
  protocol's `invalid_matrix` error and a named warning. Off-diagonal
  color mixing needs a real KMS CTM property and is never approximated
  as three independent gains.
- A wlr gamma table must be exactly three `gamma_size` arrays of
  little-endian `u16`. It is read with `pread`, so a malicious pipe
  cannot block the compositor, and wrong length earns
  `invalid_gamma`.

## Supported environments

| Environment | `hyprsunset` | wlr gamma clients | Reason |
| --- | --- | --- | --- |
| DRM/KMS output with nonzero gamma LUT | yes | yes | Real hardware ramp; one owner at a time |
| Output reporting gamma length zero | no | no | No truthful hardware operation exists |
| Nested winit session | no | no | The host compositor owns the physical output |

For a non-Omarchy fixed schedule, any ordinary client remains valid:

```sh
pacman -S --needed wlsunset
wlsunset -T 6500 -t 3000 -S 07:00 -s 20:00
```

## Diagnostics

The session log names the relevant transitions:

```text
hyprland-ctm-control advertised for hyprsunset
gamma ramp programmed ... white_r=... white_g=... white_b=...
CTM refused: non-diagonal or negative matrix ...
CTM manager released; restoring original gamma ramps
```

No advertised global means either a nested session or no usable output
LUT; startup logs which. A warm white point keeps red high and reduces
blue. If the programming line appears but the physical display remains
unchanged, the driver accepted the ioctl and the remaining fault is
below the compositor.

## Tests

`crates/chonk-testkit/tests/gamma.rs` covers absence on nested output,
wlr exclusivity, malformed tables, CTM exclusivity and arbitration,
non-diagonal refusal, client-death restoration, and real `hyprsunset`.
Run it under a host Wayland display or private Xvfb:

```sh
cargo test -p chonk-testkit --test gamma -- --ignored --test-threads=1
```

`CHONKSTEP_TEST_GAMMA_SIZE=256` is test apparatus: it exposes the real
protocol dispatch against a logged stand-in ramp. It has no effect
unless explicitly set.
