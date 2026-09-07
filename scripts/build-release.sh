#!/usr/bin/env bash
# Build only the executables installed by the Arch package. Optimizing every
# E2E probe with thin LTO used to dominate release time; those are built and
# exercised by the full debug test suite in PKGBUILD's check() and Wayland CI.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --workspace \
  --bin chonkstep \
  --bin chonkstep-wayland \
  --bin chonk-netjoin \
  --bin chonk-about \
  --bin omarchy-export-themes \
  "$@"
