#!/usr/bin/env bash
# The display-free Rust/harness checks shared by local preflight and CI.
# E2E still runs separately with scripts/e2e.sh --headless.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
    echo "Usage: scripts/check.sh [all|lint|docs|unit|wayland-unit|harness]" >&2
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
    cargo test --locked -p wm-wayland
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
        check_harness
        ;;
    lint) check_lint ;;
    docs) check_docs ;;
    unit) check_unit ;;
    wayland-unit) check_wayland_unit ;;
    harness) check_harness ;;
    -h|--help) usage ;;
    *) usage; exit 2 ;;
esac
