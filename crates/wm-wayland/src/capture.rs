//! Window and output capture: the compositor's answer to "what does
//! that window look like right now".
//!
//! X11 could hand `wm-core` a window's pixels on demand (XGetImage on
//! a live drawable), which is how Alt-Tab thumbnails and miniaturized
//! icon previews are drawn. A compositor cannot answer that
//! synchronously from inside the backend - the renderer lives on the
//! `Compositor`, not on the ledger `wm-core` holds - so the flow is
//! inverted: rendering keeps a small, throttled snapshot per mapped
//! window in its `WindowRecord`, and `Backend::capture_window_image`
//! serves the most recent one. The shell asks for a preview at
//! miniaturize time, by which point a snapshot is already waiting.
//!
//! Interim skeleton: the real offscreen-render implementation is being
//! built against this exact API. Until it lands, snapshots stay empty
//! and the shell's own no-preview faces (a plain icon tile, a
//! titled-but-blank switcher entry) are what shows - the same
//! graceful degradation those renderers were designed for.

use std::path::Path;

use crate::state::Compositor;

/// Refreshes the per-window snapshots used for previews. Called once
/// per rendered frame; the implementation throttles its own work, so
/// this stays affordable at frame rate.
pub(crate) fn refresh_snapshots(_comp: &mut Compositor) {}

// Staging: the screenshot path lands with the capture implementation
// and its callers (a keybinding, and headless verification of a real
// session); the allow keeps the build warning-free until then.
#[allow(dead_code)]
/// Writes the current output's contents to `path` as a PNG - the
/// session's screenshot path, and the way a headless verifier (CI, a
/// developer over SSH) sees what a DRM session is actually showing.
pub(crate) fn capture_output_png(_comp: &mut Compositor, _path: &Path) -> Result<(), String> {
    Err("output capture is not built yet".to_string())
}
