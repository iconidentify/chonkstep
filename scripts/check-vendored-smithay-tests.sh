#!/usr/bin/env bash
# Cargo does not run dependency unit tests. Test the exact vendored source
# outside the workspace, without editing it or the global registry cache.
set -euo pipefail
cd "$(dirname "$0")/.."
repo_root=$PWD
test_root=$(mktemp -d "${TMPDIR:-/tmp}/chonk-smithay-gles.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
cp -R vendor/smithay-0.7.0/. "$test_root/"
# Seed common dependencies with the versions we ship. Cargo may extend this
# disposable lockfile for upstream's test-only dependencies; the workspace
# lockfile and vendored manifest are never changed.
cp Cargo.lock "$test_root/Cargo.lock"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target}/smithay-gles-tests"
export LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe
python3 scripts/check-native-tests.py \
    --require backend::renderer::gles::read_batch_tests::scoped_texture_reads_hold_locks_and_recover_from_errors_and_unwind \
    --require backend::renderer::gles::read_batch_tests::custom_pixel_vertices_validate_inputs_and_preserve_default_drawing \
    -- cargo test --manifest-path "$test_root/Cargo.toml" --lib \
    --no-default-features --features renderer_gl read_batch_tests:: -- \
    --ignored --test-threads=1 --format=pretty --color=never
