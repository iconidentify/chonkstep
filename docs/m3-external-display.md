# M3 external shadow display

The M3 GPU session can adopt an additional `apple-dcpext-shadow` DRM card
while keeping the native panel as its primary device. This carries the
previously tested M3 external-display path forward alongside the current
Xwayland GL support.

This path is opt-in. In `~/.config/chonkstep/m3gpu-session.env`, export the
specific display device and the proof file the display supervisor will issue:

```sh
export CHONKSTEP_EXTRA_DRM_DEVICES=/dev/dri/by-path/platform-2d2c00000.dcp-card
export CHONKSTEP_EXTRA_DRM_M3_PROOF=/run/user/1000/chonkstep-dcpext-m3-curated-proof.json
```

Use the desktop user's runtime directory. The kernel's external-display
supervisor must start firmware, establish scanout and publish the external
card before ChonkStep can use it. The curated supervisor is maintained in
`m3-roadmap/tools/dcpext-supervisor` and pins the installed kernel, display
driver and compositor builds. Changing these environment variables alone
does not start the display firmware.

GPU adoption requires this boot's successful private cross-card import
probe. ChonkStep checks the boot, renderer, Mesa path, all device identities,
and a bounded 3840×2160 linear XRGB8888 buffer layout. The shadow card must
advertise the expected fixed 4K60 mode and compatible allocations. Failed
validation leaves the native panel running and the extra card unopened.

Each KMS device has separate connector, CRTC, hotplug and page-flip state.
DRM object numbers on different cards cannot identify the same output.
Only explicitly listed shadow devices are adopted, and desktop-only
environment controls are removed from launched applications.

This remains a display bring-up path with a CPU copy for scanout. Successful
unit tests do not establish dock replug, suspend/resume, or physical output;
those require a live test on the installed build.
