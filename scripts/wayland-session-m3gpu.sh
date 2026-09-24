#!/usr/bin/env bash
# Opt-in "chonkstep (M3 GPU, experimental)" session for the Apple M3 Pro
# (J516S, G15S): the ordinary Wayland session (scripts/wayland-session.sh)
# composed on the M3 GPU instead of llvmpipe. Started through
# scripts/chonkstep-session-m3gpu, the uwsm entry point that
# scripts/install-m3gpu-session.sh registers. It never replaces the
# ordinary "chonkstep" entries, which stay the default and keep llvmpipe.
#
# Two display modes (CHONKSTEP_M3_DISPLAY, below):
#
#   - dcp: the native display card (Apple DCP; devicetree
#     apple,t6030-display-subsystem, KMS driver "m3-dcp" today and
#     "apple" later) drives the panel. The prefix's drirc selects zink for
#     it, so Mesa's kmsro renders with zink on the M3 render node straight
#     into the display card's scanout buffers: the Asahi M1/M2 model, no
#     CPU copy, no second GPU. EGL names the M3 render node for the display
#     card, so the compositor keeps its ordinary single-GPU path; no
#     CHONKSTEP_RENDER_DEVICE is set.
#   - simpledrm: the display-only boot framebuffer (simpledrm) drives the
#     panel. CHONKSTEP_RENDER_DEVICE composes on the M3 render node and
#     copies each frame into simpledrm's buffers, whose own renderer is
#     llvmpipe (kms_swrast). See docs/gpu-pipeline.md.
#
# What differs from the ordinary session is only the environment:
#
#   - Mesa comes from a private prefix: Honeykrisp (Vulkan) and zink
#     (GLES 3.1) for the M3, zink through kmsro for the native display
#     card, and llvmpipe/kms_swrast for simpledrm. The distribution's Mesa
#     must never render on this GPU: it accepts the device and submits
#     command streams for an older generation.
#   - The display mode's device selection, above.
#   - CHONKSTEP_DMABUF_REQUIRE_MESA offers linux-dmabuf (and therefore
#     the M3) only to clients running that same Mesa. Everything else
#     falls back to shared memory and renders in software.
#   - Xwayland runs without glamor/DRI3: the prefix has no GLX, so an X11
#     OpenGL client would otherwise load the distribution's GLX driver
#     on the M3.
#
# The kernel driver has no GPU fault recovery. One fault leaves the GPU,
# and with it this desktop, frozen until reboot. Before trying this,
# read ~/src/m3-gpu-work/merge-plan/CHONKSTEP-M3.md (start, watch, back
# out).
#
# Knobs, read from the environment or from the optional per-user file
# ${XDG_CONFIG_HOME:-~/.config}/chonkstep/m3gpu-session.env (shell
# assignments, sourced below; only this launcher reads it):
#
#   CHONKSTEP_M3_DISPLAY      auto (default): dcp when the native display
#                             card exists, simpledrm otherwise. dcp or
#                             simpledrm force a mode (and fail closed if its
#                             display is missing). A leading argument
#                             --display=auto|dcp|simpledrm (given by the
#                             entry point, e.g. the autologin chooser)
#                             overrides the variable and the file.
#   CHONKSTEP_M3_MESA_PREFIX  private Mesa prefix
#                             (default ~/src/m3-gpu-work/mesa-eryk-prefix)
#   CHONKSTEP_M3_RENDER_NODE  M3 render node (default /dev/dri/renderD128)
#   CHONKSTEP_M3_KMS_DEVICE   simpledrm mode: the display-only KMS device
#                             (default /dev/dri/card0). dcp mode: the card
#                             is found by its devicetree node; if this is
#                             set it must name that same card.
#   CHONKSTEP_M3_CLIENTS      gpu (default): clients running the prefix Mesa
#                             see linux-dmabuf and render on the M3.
#                             software: only the compositor uses the GPU.
#   CHONKSTEP_M3_DIRECT_SCANOUT=1  dcp mode: allow fullscreen client
#                             buffers on the display plane. Off by default
#                             (CHONKSTEP_NO_DIRECT_SCANOUT=1) until
#                             qualified; simpledrm mode never offers it.
#   CHONKSTEP_M3_XWAYLAND_GLAMOR=1  keep Xwayland's glamor/DRI3. Not
#                             safe until the prefix provides GLX.
#   CHONKSTEP_M3_IGNORE_KERNEL_LOG=1  start even when this boot's kernel
#                             log cannot be read or already shows a GPU fault.
set -u

display_arg=
case "${1:-}" in
    --display=*)
        display_arg="${1#--display=}"
        shift
        ;;
esac

LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/chonkstep"
mkdir -p "$LOG_DIR"
# The same log the ordinary session appends to: wayland-session.sh's
# watcher finds the compositor's socket by reading exactly that file.
LOG="$LOG_DIR/wayland-session.log"

fail() {
    printf 'chonkstep M3 GPU session: %s\n' "$1" | tee -a "$LOG" >&2
    exit 1
}

conf="${XDG_CONFIG_HOME:-$HOME/.config}/chonkstep/m3gpu-session.env"
if [ -r "$conf" ]; then
    # shellcheck disable=SC1090
    . "$conf"
fi
unset conf

P="${CHONKSTEP_M3_MESA_PREFIX:-$HOME/src/m3-gpu-work/mesa-eryk-prefix}"
RENDER_NODE="${CHONKSTEP_M3_RENDER_NODE:-/dev/dri/renderD128}"
CLIENTS="${CHONKSTEP_M3_CLIENTS:-gpu}"
MODE="${display_arg:-${CHONKSTEP_M3_DISPLAY:-auto}}"
unset display_arg

# The native display card: the DRM card whose device is the devicetree
# display subsystem. Found by that node, never by number (it is card2 on
# today's lab boots, after the GPU's card1).
dcp_card() {
    local dev
    for dev in /sys/class/drm/card*; do
        case "${dev##*/}" in *-*) continue ;; esac
        [ -r "$dev/device/of_node/compatible" ] || continue
        if tr '\0' '\n' < "$dev/device/of_node/compatible" | grep -qx 'apple,t6030-display-subsystem'; then
            printf '/dev/dri/%s\n' "${dev##*/}"
            return 0
        fi
    done
    return 1
}

# Whether the prefix drirc selects dri_driver $2 for kernel driver $1.
drirc_selects() {
    awk -v dev="kernel_driver=\"$1\"" -v val="value=\"$2\"" '
        index($0, "<device") && index($0, dev) { inside = 1; next }
        inside && index($0, "</device>") { inside = 0 }
        inside && index($0, "name=\"dri_driver\"") && index($0, val) { found = 1 }
        END { exit !found }' "$DRIRC" 2>/dev/null
}

case "$MODE" in
    auto)
        if DCP_CARD="$(dcp_card)"; then MODE=dcp; else MODE=simpledrm; fi
        ;;
    dcp)
        DCP_CARD="$(dcp_card)" \
            || fail "display mode dcp: no DRM card for the native display (devicetree apple,t6030-display-subsystem); the DCP takeover did not happen this boot"
        ;;
    simpledrm) ;;
    *) fail "CHONKSTEP_M3_DISPLAY / --display must be auto, dcp or simpledrm (got '$MODE')" ;;
esac
if [ "$MODE" = dcp ]; then
    if [ -n "${CHONKSTEP_M3_KMS_DEVICE:-}" ] \
        && [ "$(stat -L -c %t:%T "$CHONKSTEP_M3_KMS_DEVICE" 2>/dev/null)" != "$(stat -L -c %t:%T "$DCP_CARD")" ]; then
        fail "CHONKSTEP_M3_KMS_DEVICE=$CHONKSTEP_M3_KMS_DEVICE is not the native display card $DCP_CARD"
    fi
    KMS_DEVICE="$DCP_CARD"
else
    KMS_DEVICE="${CHONKSTEP_M3_KMS_DEVICE:-/dev/dri/card0}"
fi

# ---------------------------------------------------------------------
# Pre-flight. Every check fails closed: the display manager returns to
# the greeter with the reason in the log, instead of a session that
# would put the wrong Mesa on the GPU.

case "$P" in
    /*) ;;
    *) fail "CHONKSTEP_M3_MESA_PREFIX must be an absolute path (got '$P')" ;;
esac
ICD="$P/share/vulkan/icd.d/asahi_icd.aarch64.json"
EGL_VENDOR="$P/share/glvnd/egl_vendor.d/50_mesa.json"
DRIRC="$P/share/drirc.d/10-m3-asahi-zink.conf"
for required in "$ICD" "$EGL_VENDOR" "$P/lib/libgbm.so.1" "$P/lib/libEGL_mesa.so.0" "$P/lib/dri/kms_swrast_dri.so"; do
    [ -e "$required" ] || fail "$required is missing: build and install the M3 Mesa prefix first (with llvmpipe)"
done
compgen -G "$P/lib/libgallium-*.so" >/dev/null || fail "no libgallium in $P/lib"
if [ "$MODE" = dcp ]; then
    # The display card must go to zink through kmsro (and the M3 to zink):
    # without the pin the generic "unknown KMS driver -> kmsro" fallback
    # would do the same today, but only the pin is what was qualified.
    for pin in asahi m3-dcp apple; do
        drirc_selects "$pin" zink || fail "$DRIRC does not select zink for kernel driver '$pin' (D5 pins missing)"
    done
    unset pin
else
    # Without this drirc pin the prefix's kmsro would wrap the display in zink
    # "renderonly" and let the M3 draw into simpledrm's buffers; the
    # compositor refuses that target anyway, but say why here.
    grep -q 'kernel_driver="softpipe"' "$DRIRC" 2>/dev/null \
        || fail "$DRIRC (display -> kms_swrast, asahi -> zink) is missing"
fi

[ -c "$RENDER_NODE" ] || fail "$RENDER_NODE is not a character device"
case "${RENDER_NODE##*/}" in
    renderD*) ;;
    *) fail "$RENDER_NODE is not a DRM render node" ;;
esac
render_driver="$(basename "$(readlink -f "/sys/class/drm/${RENDER_NODE##*/}/device/driver")")"
[ "$render_driver" = asahi ] || fail "$RENDER_NODE is driven by '$render_driver', not asahi"
[ -c "$KMS_DEVICE" ] || fail "$KMS_DEVICE is not a character device"
kms_sys="/sys/class/drm/${KMS_DEVICE##*/}/device/drm"
if compgen -G "$kms_sys/renderD*" >/dev/null; then
    if [ "$MODE" = dcp ]; then
        fail "the native display card $KMS_DEVICE has a render node of its own: kmsro would not pair it with $RENDER_NODE"
    fi
    fail "$KMS_DEVICE has a render node: it is no longer the display-only boot framebuffer this session was built for"
fi
if [ "$MODE" = dcp ]; then
    # kmsro pairs a display-only platform card with the first render-capable
    # platform node it can open. That must be the M3 and nothing else.
    render_nodes=(/sys/class/drm/renderD*)
    [ "${#render_nodes[@]}" -eq 1 ] && [ "${render_nodes[0]##*/}" = "${RENDER_NODE##*/}" ] \
        || fail "kmsro would pick from several render nodes (${render_nodes[*]##*/}); only ${RENDER_NODE##*/} is qualified"
    # The display card appears during the takeover just before autologin, and its connector can
    # still read "unknown" for a moment afterwards: wait for it (up to 15 s) rather than refuse.
    connected=0
    for _ in $(seq 1 150); do
        for status_file in "/sys/class/drm/${KMS_DEVICE##*/}"-*/status; do
            [ "$(cat "$status_file" 2>/dev/null)" = connected ] && connected=1
        done
        [ "$connected" = 1 ] && break
        sleep 0.1
    done
    [ "$connected" = 1 ] || fail "the native display card $KMS_DEVICE has no connected connector (waited 15 s)"
    unset render_nodes connected status_file
fi
unset render_driver kms_sys

# A GPU that already faulted this boot stays dead until reboot; starting a
# desktop on it only produces a frozen screen. In dcp mode a failed DCP
# flip (the lab KMS driver then refuses every later commit), a compute job
# the kernel rejected, or any firmware error also refuses the start.
if [ "$MODE" = dcp ]; then
    fault_pattern='M3 bank|execution failed|M3 firmware error|M3 compute rejected|native DCP flip failed'
else
    fault_pattern='M3 bank|execution failed|firmware error event'
fi
if [ "${CHONKSTEP_M3_IGNORE_KERNEL_LOG:-0}" != 1 ]; then
    faults="$(journalctl -k -b -q --no-pager -g "$fault_pattern" 2>/dev/null)"
    status=$?
    # journalctl -g: 0 = matches, 1 = none, anything else = unreadable.
    if [ "$status" -eq 0 ]; then
        fail "the GPU already faulted this boot (reboot first): $(printf '%s' "$faults" | tail -n 1)"
    elif [ "$status" -ne 1 ]; then
        fail "cannot read this boot's kernel log to check for GPU faults (set CHONKSTEP_M3_IGNORE_KERNEL_LOG=1 to skip)"
    fi
    unset faults status
fi
unset fault_pattern

case "$CLIENTS" in
    gpu) dmabuf_policy="$P" ;;
    software) dmabuf_policy=none ;;
    *) fail "CHONKSTEP_M3_CLIENTS must be gpu or software (got '$CLIENTS')" ;;
esac

# ---------------------------------------------------------------------
# The Mesa environment, inherited by the compositor and every client it
# starts. Deliberately NOT MESA_LOADER_DRIVER_OVERRIDE or GALLIUM_DRIVER:
# either one applies to every device in a process, and in simpledrm mode
# this compositor needs zink on the M3 and llvmpipe on the display at the
# same time (in dcp mode both are zink, the display through kmsro). The
# per-device choice lives in the prefix's drirc (see $DRIRC).
unset MESA_LOADER_DRIVER_OVERRIDE GALLIUM_DRIVER LIBGL_ALWAYS_SOFTWARE \
    GBM_ALWAYS_SOFTWARE DRIRC_CONFIGDIR VK_ADD_DRIVER_FILES
export LD_LIBRARY_PATH="$P/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LIBGL_DRIVERS_PATH="$P/lib/dri"
export GBM_BACKENDS_PATH="$P/lib/gbm"
export __EGL_VENDOR_LIBRARY_FILENAMES="$EGL_VENDOR"
export VK_DRIVER_FILES="$ICD"
export VK_ICD_FILENAMES="$ICD"
# The system's implicit Vulkan layers belong to the system's drivers.
export VK_LOADER_LAYERS_DISABLE='~implicit~'
# Eryk's M3 runtime settings (m3_lab/tools/m3-desktop/graphics.env).
export ASAHI_M3_EXPERIMENTAL=1
export AGX_MESA_DEBUG=nopromote
export HK_DEBUG_LOG=0
export QSG_RHI_BACKEND=opengl

if [ "$MODE" = dcp ]; then
    # kmsro already renders on the M3 for the display card, and EGL names
    # the M3 render node: the single-GPU path, composing in scanout memory.
    unset CHONKSTEP_RENDER_DEVICE
    if [ "${CHONKSTEP_M3_DIRECT_SCANOUT:-0}" != 1 ]; then
        export CHONKSTEP_NO_DIRECT_SCANOUT=1
    fi
else
    export CHONKSTEP_RENDER_DEVICE="$RENDER_NODE"
fi
export CHONKSTEP_DRM_DEVICE="$KMS_DEVICE"
export CHONKSTEP_DMABUF_REQUIRE_MESA="$dmabuf_policy"
if [ "${CHONKSTEP_M3_XWAYLAND_GLAMOR:-0}" != 1 ]; then
    export XWAYLAND_NO_GLAMOR=1
fi
unset dmabuf_policy

# D-Bus- and systemd-activated services (portals, `uwsm app -t service`)
# do not descend from the compositor. Under uwsm, the session script's
# `uwsm finalize` also exports these names to the activation environment
# and records them in uwsm's cleanup list, which unsets them when the
# session stops, so the next ordinary login does not inherit them.
# Only under uwsm: a UWSM_* variable is also what makes
# wayland-session.sh treat the login as uwsm-managed.
if [ -r /proc/self/cgroup ] && grep -Eq '(^|/)wayland-wm@[^/]+\.service($|/)' /proc/self/cgroup; then
    export UWSM_FINALIZE_VARNAMES="${UWSM_FINALIZE_VARNAMES:+$UWSM_FINALIZE_VARNAMES }LD_LIBRARY_PATH LIBGL_DRIVERS_PATH GBM_BACKENDS_PATH __EGL_VENDOR_LIBRARY_FILENAMES VK_DRIVER_FILES VK_ICD_FILENAMES VK_LOADER_LAYERS_DISABLE ASAHI_M3_EXPERIMENTAL AGX_MESA_DEBUG HK_DEBUG_LOG QSG_RHI_BACKEND"
fi

if [ "$MODE" = dcp ]; then
    printf 'chonkstep M3 GPU session: %s display=dcp kms=%s (native display card, zink via kmsro) render=%s mesa=%s clients=%s direct_scanout=%s xwayland_glamor=%s\n' \
        "$(date -Is)" "$KMS_DEVICE" "$RENDER_NODE" "$P" "$CLIENTS" "${CHONKSTEP_M3_DIRECT_SCANOUT:-0}" \
        "${CHONKSTEP_M3_XWAYLAND_GLAMOR:-0}" >> "$LOG"
else
    printf 'chonkstep M3 GPU session: %s render=%s kms=%s mesa=%s clients=%s xwayland_glamor=%s\n' \
        "$(date -Is)" "$RENDER_NODE" "$KMS_DEVICE" "$P" "$CLIENTS" "${CHONKSTEP_M3_XWAYLAND_GLAMOR:-0}" >> "$LOG"
fi

exec "$(dirname "$0")/wayland-session.sh" "$@"
