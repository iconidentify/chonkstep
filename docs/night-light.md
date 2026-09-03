# Night light

Warming the screen at night works in the Wayland session, through the
same protocol every night-light tool in the Wayland ecosystem already
speaks. This document says what is supported, on which backend, and —
the part that matters most when something looks broken — exactly what a
tool gets told when the answer is no.

Until this landed, chonkstep could not tint or dim the display *at
all*, by any program. The gap surfaced from the outside: an upstream
patch to Omarchy's nightlight script could not be tested here, because
nothing on this session could change a screen's colour. That patch is
now tested against this implementation — `omarchy/upstream/` carries
the run — which is the point of the exercise.

## What carries it

    wlsunset / gammastep / redshift
      → zwlr_gamma_control_unstable_v1     the compositor global
        → chonkstep-wayland                crates/wm-wayland/src/gamma.rs
          → DRM_IOCTL_MODE_SETGAMMA        the crtc's gamma LUT

`zwlr_gamma_control_unstable_v1` is the protocol wlroots compositors
expose and every night-light daemon uses. Implementing it makes the
whole ecosystem work at once, which is why it is the one that is here.

Hyprland's `hyprsunset` is the exception: it uses Hyprland's own
`hyprland-ctm-control-v1`, which chonkstep does not implement, so
`hyprsunset` will not work. Any of the three tools above will. On Arch:

    pacman -S --needed wlsunset      # or gammastep, or redshift

Then, for a fixed warm screen:

    wlsunset -T 6500 -t 3000 -S 07:00 -s 20:00

or, to see it immediately at one temperature, run it with a sunset time
that has already passed today.

## What is supported

- **Per output.** Every connector the session drives gets its own
  gamma control, and a tool can warm one screen and leave another
  alone.
- **Exclusive, by design.** Only one client at a time may hold an
  output. A second one is answered `failed` and gets nothing. That is
  the protocol's own rule and it is worth having: two night-light
  daemons running at once would otherwise fight, each undoing the
  other every few seconds.
- **Restored when the client goes away.** The ramp in force before a
  daemon claimed the output is captured at claim time and put back
  when the daemon exits — including when it crashes or is killed, and
  including when the whole session ends or hot-restarts. A night-light
  daemon that dies does not leave the screen orange.
- **Survives a VT switch.** Handing the seat to another session resets
  every crtc's LUT; chonkstep programs the ramp that is still in force
  back when the seat comes home, so `Ctrl+Alt+F2` and back does not
  silently turn the night light off.

## What is not

- **The nested (winit) backend advertises no gamma control at all.**
  Running chonkstep in a window on somebody else's desktop means there
  is no crtc to program, so the global is simply absent and a
  night-light tool says so and exits:

      $ WAYLAND_DISPLAY=wayland-2 wlsunset -T 6500 -t 3000
      compositor doesn't support wlr-gamma-control-unstable-v1

  This is deliberate, and the alternative was considered and rejected:
  advertising the global and quietly dropping the ramps would leave a
  tool reporting success while the screen never changed, which is worse
  than a failure because there is no way to tell it apart from a broken
  monitor. (Applying the ramp in the renderer as a shader LUT would
  work, but it means an offscreen colour pass through the one render
  path the whole end-to-end suite runs, to buy a preview-only
  convenience.) The compositor names the decision in its log:

      wlr-gamma-control is NOT advertised (night-light tools will say
      so and exit rather than silently do nothing)

- **Hardware with no gamma LUT.** An output whose crtc reports a
  `gamma_length` of zero cannot be controlled. Its claims are answered
  with the protocol's `failed` event — "the output doesn't support
  gamma tables" — rather than accepted and discarded. If *no* output on
  a session can be controlled, no global is advertised at all.

- **`hyprsunset`**, as above: a different protocol.

- **Colour transform matrices, HDR, ICC profiles.** This is a gamma
  LUT and nothing more.

## Which DRM mechanism, and why

The legacy `DRM_IOCTL_MODE_SETGAMMA` ioctl, through the `drm` crate's
`set_gamma` on the control device — not the atomic `GAMMA_LUT`
property blob.

Both work. The legacy ioctl is one call on a device smithay already
hands us, whereas `GAMMA_LUT` would mean building a property blob and
committing it atomically — and every atomic commit on these crtcs
belongs to smithay's `DrmCompositor`, which builds its own full
property set per frame and offers no seam for an extra property.
Committing behind it is how a compositor desynchronises its own modeset
state.

On an atomic driver the legacy ioctl is not a legacy *path*: the kernel
routes it through `drm_atomic_helper_legacy_gamma_set`, which writes the
same atomic `GAMMA_LUT` state the property would have. Because
smithay's own commits never mention `GAMMA_LUT`, the value survives
every frame queued afterwards — an unlisted property keeps its value
across an atomic commit.

The one cost, stated honestly: that kernel helper performs a *blocking*
commit, so setting gamma can wait up to about one vblank for a page flip
already in flight on that crtc. The compositor therefore programs ramps
in one place, once per event-loop pass, keeping only the newest — so a
client sending `set_gamma` in a tight loop costs one such wait per pass
rather than one per request. A night-light daemon sets gamma a few times
an hour.

## Reading a client's table safely

`set_gamma` hands over a file descriptor, and a client is free to hand
over anything at all. Two rules make that harmless:

- The table is read with `pread`, not `read`. `pread` is defined only
  for seekable objects, so a client passing a pipe with nothing in it
  gets an immediate error instead of freezing the compositor's single
  event loop for as long as it feels like.
- The length is the *compositor's*, from the hardware — never a number
  the client supplied. A table that is not exactly three ramps of
  `gamma_size` little-endian `u16`s is refused with the protocol's
  `invalid_gamma` error, short, long, and misaligned alike.

## When it does not work

- **`compositor doesn't support wlr-gamma-control-unstable-v1`** — you
  are on the nested backend, or every output's hardware reports no
  gamma ramp. The compositor's log says which at startup.
- **The tool starts and the screen does not change** — check the
  compositor log for `gamma ramp programmed`; it names the output and
  the white point (`white_r`, `white_g`, `white_b`) it just set. A
  warm ramp pulls `white_b` well below `white_r`. If the line is there
  and the screen is unchanged, the crtc took the LUT and the panel is
  ignoring it.
- **Two tools, one screen** — the second gets `failed`. Stop the first
  one; `wlsunset` and friends release the output on exit.
- **The screen stayed orange after the tool died** — this is the
  failure this implementation exists to prevent, and there is an
  end-to-end test for it (`crates/chonk-testkit/tests/gamma.rs`). The
  log line to look for is `restoring the original ramp`, followed by a
  `gamma ramp programmed` with a neutral white point.

## Testing it

The unit tests for table parsing, exclusivity and the restore live with
the implementation (`crates/wm-wayland/src/gamma.rs`). The end-to-end
tests drive a real client over a real socket:

    cargo test -p chonk-testkit --test gamma -- --ignored --test-threads=1

`chonk-gamma-probe` is that client — a night-light daemon reduced to the
steps a test needs to watch:

    chonk-gamma-probe report            # what the compositor offers
    chonk-gamma-probe set 3000          # claim, set one ramp, exit
    chonk-gamma-probe exclusive         # two claims, one output
    chonk-gamma-probe hold 3000         # claim, set, wait to be killed
    chonk-gamma-probe bad-table         # a deliberately wrong table

Because a nested session has no crtc, the end-to-end tests give it a
stand-in: `CHONKSTEP_TEST_GAMMA_SIZE=256` advertises the global and
records the ramps into the log instead of scanning them out. That is
test apparatus in the same shape as `CHONKSTEP_TEST_SOCKET` — inert
unless a test sets it, and announced loudly in the log when it is set.
Nothing a user runs sets it.
