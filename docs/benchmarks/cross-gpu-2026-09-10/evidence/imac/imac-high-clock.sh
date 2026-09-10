#!/bin/bash
set -eu
gpu_policy_file=/sys/class/drm/card1/device/power_dpm_force_performance_level
gpu_previous_policy=$(cat "$gpu_policy_file")
printf '%s\n' "$gpu_previous_policy" > /tmp/cim/power-policy-before.txt
restore_policy() {
    printf '%s\n' "$gpu_previous_policy" | sudo -n tee "$gpu_policy_file" >/dev/null
    cat "$gpu_policy_file" > /tmp/cim/power-policy-restored.txt
}
trap restore_policy EXIT
trap 'exit 1' HUP INT TERM
printf 'high\n' | sudo -n tee "$gpu_policy_file" >/dev/null
cat "$gpu_policy_file" > /tmp/cim/power-policy-diagnostic.txt
PATH=/tmp/cim/bin:$PATH python /tmp/cim/scripts/bench-native-gpu.py \
    --binary final=/tmp/cim/bin/final --probe /tmp/cim/bin/probe \
    --drm-info /tmp/cim/bin/drm-info --device /dev/dri/card1 --connector eDP-1 \
    --test-vt 3 --return-vt 1 --width 3840 --height 2160 --hz 59.997 \
    --cases native fractional --policies default --runs 2 --seconds 12 \
    --settle-seconds 6 --gpu-timings --output /tmp/cim/timers-high
