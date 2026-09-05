# Logging and live diagnostics

The Wayland session writes its current log to
`${XDG_STATE_HOME:-$HOME/.local/state}/chonkstep/wayland-session.log`.
The previous login is retained as `wayland-session.log.old`. The X11
session writes to the same state directory through its session script.

`RUST_LOG` selects tracing targets at startup. Useful examples:

```sh
RUST_LOG=info,wm_wayland::session=debug
RUST_LOG=info,wm_wayland::layers=trace
RUST_LOG=info,wm_wayland::input=debug
RUST_LOG=info,wm_wayland::renderer=debug
```

The Wayland compositor's filter is reloadable. These commands change it
without closing clients or restarting the desktop:

```sh
hyprctl log-filter 'info,wm_wayland::session=debug'
hyprctl log-filter 'info,wm_wayland::layers=trace'
```

| Symptom | Suggested target |
|---|---|
| Frozen or dark output | `wm_wayland::session=debug` |
| Popup/layer misplaced | `wm_wayland::layers=trace,wm_wayland::xdg=debug` |
| Window size or scale wrong | `wm_wayland::xdg=debug,wm_wayland::output_mgmt=debug` |
| Screen sharing is black | `wm_wayland::capture=debug,wm_wayland::image_capture=debug` |
| Key or pointer binding fails | `wm_wayland::input=debug` |

`hyprctl systeminfo` returns a live build/output/scene summary. The
control socket also accepts `{"request":"debug","topic":"scene"}`;
`focus` and `clients` are valid topics as well. Run
`chonkstep-bugreport` to collect those answers, both log generations,
the package/build identity, recovery marker, and user configuration into
one mode-0600 file. The report is not redacted; review it before sharing.

## Diagnostic switches

The environment variables below establish startup defaults. On Wayland,
the corresponding live knob can be changed with
`hyprctl debug-set KNOB BOOL`.

| Environment variable | Live knob | Effect |
|---|---|---|
| `CHONKSTEP_DAMAGE_LOG` | `damage-log` | Log renderer damage rectangles per submitted frame. |
| `CHONKSTEP_IDLE_LOG` | `idle-log` | Log actual idle-inhibition policy transitions. |
| `CHONKSTEP_FULL_DAMAGE` | `full-damage` | Force whole-output repainting for artifact diagnosis. |
| `CHONKSTEP_NO_DIRECT_SCANOUT` | `no-direct-scanout` | Keep fullscreen clients on the GLES composition path. |
| `CHONKSTEP_NO_CURSOR_PLANE` | `no-cursor-plane` | Composite the cursor rather than using a KMS cursor plane. |
| `CHONKSTEP_STRICT_BUFFER_RELEASE` | — | Hold composited client buffers through vblank; startup-only because it changes buffer ownership. |
| `CHONKSTEP_DRM_DEVICE` | — | Select the single DRM card the session drives; startup-only. |

The dispatch loop emits a `dispatch_pass` span, phase spans around input
and connector work, and surface/layer handler spans. Enabling their
targets adds that context to nested warnings and Smithay events.
