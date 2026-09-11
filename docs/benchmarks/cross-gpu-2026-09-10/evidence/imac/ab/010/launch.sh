#!/bin/bash
set -eu
printf "%s\n" "$XDG_SESSION_ID" > /tmp/cim/ab/010/session
while [ ! -e /tmp/cim/ab/010/go ]; do sleep 0.02; done
exec /usr/bin/env -i PATH=/tmp/cim/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/home/chrisk/.local/share/mise/shims:/home/chrisk/.local/bin LOGNAME=chrisk HOME=/home/chrisk USER=chrisk XDG_CONFIG_HOME=/tmp/cim/ab/010/config XDG_STATE_HOME=/tmp/cim/ab/010/state XDG_CACHE_HOME=/tmp/cim/ab/010/cache XDG_DATA_HOME=/tmp/cim/ab/010/data XDG_RUNTIME_DIR=/tmp/cim/ab/010/runtime CHONKSTEP_BACKEND=drm CHONKSTEP_DRM_DEVICE=/dev/dri/card1 CHONKSTEP_GPU_TIMINGS=0 CHONKSTEP_NO_VRR=1 CHONKSTEP_NO_APPEARANCE_PROPAGATION=1 CHONKSTEP_TEST_SOCKET=/tmp/cim/ab/010/runtime/door.sock RUST_LOG=info NO_COLOR=1 GSETTINGS_BACKEND=memory XDG_SESSION_ID="$XDG_SESSION_ID" XDG_SESSION_TYPE=wayland XDG_SEAT=seat0 XDG_VTNR=3 dbus-run-session -- /bin/sh -c 'printf "%s\n" "$$" > /tmp/cim/ab/010/pid; exec /tmp/cim/bin/baseline' > /tmp/cim/ab/010/compositor.log 2>&1
