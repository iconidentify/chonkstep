# Native recording keyframe comparison — 2026-09-11 UTC

An explicit one-second maximum GOP fixes ChonkStep's previously unconstrained
seek interval. It is a storage/latency tradeoff, not a CPU optimization.

| Measurement | Old g=250 | New g=60 | Change |
| --- | ---: | ---: | ---: |
| Maximum keyframe interval | 4.167 s | 1.000 s | −76% |
| Encoder CPU, % of one core | 609.17% | 612.21% | +0.50% |
| Mean file size, decimal MB | 17.412 | 57.281 | +229% (3.29×) |
| Actual frame rate from packet timestamps | 60 | 60 | unchanged |
| Complete decode | passed both | passed both | — |

## Method

Native i9beef / RTX 3090, 3840×2160 output at 144 Hz, UI scale 1.5, VRR and
experimental scanout flags off. Four 60-second wall-time takes, ABBA order:
250, 60, 60, 250. One Foot window with static text and a five-Hz single-digit
counter remained in place; the remainder was the detailed desktop wallpaper.
The same compositor binary, scene and recorder arguments were used for all
four takes. No compilation or test workloads overlapped timing. Existing host
background services were not stopped, so absolute CPU is not an idle-machine
measurement. This is an encoder comparison, not whole-system GPU telemetry.

`wf-recorder 0.6.0`, FFmpeg n9.0.1, libx264 `preset=veryfast`, `crf=18`,
requested 60 fps, no DMA-BUF, no damage gating, limited-range yuv420p. Only
`-p g=250` versus `-p g=60` changed. CPU is the reaped recorder's user+system
CPU divided by wall time, including startup and finalization. It does not
include the compositor, Foot, or the post-measurement validation decoder.
The 60-second process lifetime produced 59.4-second videos because of startup.

Every file was fully decoded with `ffmpeg -v error -xerror -i TAKE -f null -`.
`ffprobe` independently measured keyframe timestamps and all packet timestamps.
The Matroska stream's nominal `r_frame_rate=120/1` is a timestamp-resolution
artifact: packet counts and PTS establish 60 fps. Raw results and exact argv
are in [results.json](results.json); tool and binary identity are in
[metadata.json](metadata.json).

A preliminary take was rejected when the physical monitor briefly disconnected
during link training. The complete ABBA series began after a 15-second display
warmup. The original VT was restored. No M1 session was changed.

## Interpretation

The issue's proposed “within a few percent” storage target is contradicted by
this measurement. More frequent intra frames repeatedly encode the detailed
static background. CRF and resolution were deliberately held constant; reducing
quality to hide the storage cost would be a different change. Other scenes,
presets and frame rates need their own measurements.

Accurate seeking can already decode interframes with the previous GOP. In
Omacut 0.4.0, the thumbnail helper invokes FFmpeg with input `-ss` and a decoded
output frame, rather than extracting only keyframes. The new GOP bounds the
work before reaching a requested frame; it does not add missing frames or
prove that every reported editor freeze was caused by GOP length.
