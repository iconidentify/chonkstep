#!/bin/bash
set -eu
printf "%s\n" "$XDG_SESSION_ID" > /tmp/cm1/flags/005/session
while [ ! -e /tmp/cm1/flags/005/go ]; do sleep 0.02; done
exec /usr/bin/env -i LOGNAME=chrisk HOME=/home/chrisk USER=chrisk PATH=/usr/local/sbin:/usr/local/bin:/usr/bin:/home/chrisk/.local/share/mise/shims:/home/chrisk/.local/bin XDG_CONFIG_HOME=/tmp/cm1/flags/005/config XDG_STATE_HOME=/tmp/cm1/flags/005/state XDG_CACHE_HOME=/tmp/cm1/flags/005/cache XDG_DATA_HOME=/tmp/cm1/flags/005/data XDG_RUNTIME_DIR=/tmp/cm1/flags/005/runtime CHONKSTEP_BACKEND=drm CHONKSTEP_DRM_DEVICE=/dev/dri/card2 CHONKSTEP_GPU_TIMINGS=0 CHONKSTEP_NO_VRR=1 CHONKSTEP_NO_APPEARANCE_PROPAGATION=1 CHONKSTEP_TEST_SOCKET=/tmp/cm1/flags/005/runtime/door.sock RUST_LOG=info NO_COLOR=1 GSETTINGS_BACKEND=memory XDG_SESSION_ID="$XDG_SESSION_ID" XDG_SESSION_TYPE=wayland XDG_SEAT=seat0 XDG_VTNR=3 dbus-run-session -- /bin/sh -c 'printf "%s\n" "$$" > /tmp/cm1/flags/005/pid; exec /tmp/cm1/bin/final' > /tmp/cm1/flags/005/compositor.log 2>&1
