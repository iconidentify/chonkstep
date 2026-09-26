#!/usr/bin/env bash
# The display-free Rust/harness checks shared by local preflight and CI.
# E2E still runs separately with scripts/e2e.sh --headless.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
    echo "Usage: scripts/check.sh [all|lint|docs|unit|wayland-unit|gles|harness]" >&2
}

check_lint() {
    cargo clippy --locked --workspace --all-targets --no-deps -- \
        -D warnings -D clippy::disallowed_methods -D clippy::disallowed_types \
        -D clippy::undocumented_unsafe_blocks
}

check_docs() {
    RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings -A rustdoc::private_intra_doc_links" \
        cargo doc --workspace --no-deps --locked --document-private-items
}

check_unit() {
    # CI's display-free job has no Wayland system libraries. The next gate
    # owns those crates, and `all` runs both gates locally, in debug profile.
    cargo test --locked --workspace --exclude wm-wayland --exclude chonkstep-wayland
}

check_wayland_unit() {
    cargo test --locked -p wm-wayland -p chonkstep-wayland
}

check_gles() {
    # These tests create their own surfaceless EGL contexts. No running
    # desktop or GPU is needed, but missing Mesa support must fail the gate.
    LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
        python3 scripts/check-native-tests.py \
        --require frame_effects::tests::native_cached_gaussian_shadow_crop_relocation_and_warm_allocation \
        --require frame_effects::tests::native_modern_composed_lower_border_has_single_coverage \
        --require rounded::tests::native_gles_opaque_client_avoids_double_rounded_fill_coverage \
        --require rounded::tests::native_gles_rounded_texture_crop_relocation_and_shadow_pixels \
        --require shadow_cache::tests::native_immutable_atlas_shared_context_eviction_and_state_restoration \
        -- cargo test --locked -p wm-wayland --lib native_ -- \
        --ignored --test-threads=1 --format=pretty --color=never
    scripts/check-vendored-smithay-tests.sh
}

check_harness() {
    python3 -B -m unittest discover -s scripts/tests -v
}

if [ "$#" -gt 1 ]; then
    usage
    exit 2
fi
case "${1:-all}" in
    all)
        check_lint
        check_docs
        check_unit
        check_wayland_unit
        check_gles
        check_harness
        ;;
    lint) check_lint ;;
    docs) check_docs ;;
    unit) check_unit ;;
    wayland-unit) check_wayland_unit ;;
    gles) check_gles ;;
    harness) check_harness ;;
    -h|--help) usage ;;
    *) usage; exit 2 ;;
esac
