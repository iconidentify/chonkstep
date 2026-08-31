//! Minimal reusable GUI toolkit for chonkstep apps.
//!
//! Two shapes of app live here, sharing one visual vocabulary:
//!
//! - An [`App`] is a regular, independent X11 client — chonkstep
//!   decorates its window automatically, the same as any other window —
//!   but it draws its *content* with the same `wm-theme` paint
//!   primitives (flat/gradient fills, the chisel bevel, themed text)
//!   the desktop shell itself uses for the dock and root menu, via the
//!   re-exported [`paint`] module and [`nextstep_theme`]. That's the
//!   whole point of the SDK: app content and window chrome come from
//!   the same visual vocabulary instead of an app inventing its own.
//! - A [`dockapp`] is a separate process that owns one dock tile. It
//!   opens no display connection at all; it pushes finished tile pixels
//!   to the shell over a private socket, and the shell blits them
//!   exactly as it blits a built-in instrument.
//!
//! The [`App`] side is a deliberately small first cut: one fixed-size
//! window, a single redraw callback, and click notification — no layout
//! engine or widget tree yet. Enough to prove real apps can inherit the
//! look and feel; a fuller widget toolkit is future work.
//!
//! # SDK surface
//!
//! Everything re-exported here is public API with a compatibility
//! obligation; `wm-theme`'s other modules are not, and `raster` is
//! already private. Concretely:
//!
//! - [`model`] — the `Theme` data model.
//! - [`paint`] — the low-level drawing primitives (fills, bevels,
//!   themed text).
//! - [`tile`] — the common square-tile platform (face, relief,
//!   luminance-picked ink, sunken wells). Every dock item, every
//!   miniaturized-window icon and every third-party tile is built on
//!   it, which is why the whole desktop reads as one family.
//! - [`panel`] — the instrument kit one level up from `tile`: a
//!   theme-reactive LED screen with seven-segment digits, meters and
//!   history matrices, plus `render_dead_tile` for a tile with nothing
//!   to say. This is what a dockapp actually draws on.
//! - [`tiny_skia`] — the pixel buffer type both callbacks work in.
//! - [`clock`] — the analog clock face, re-exported because
//!   `examples/chonk-dockclock`, the conformance dockapp, has to be
//!   buildable against this crate alone; a "public SDK" the reference
//!   example cannot use is not one.

#[cfg(feature = "x11")]
mod app;
pub mod dockapp;

#[cfg(feature = "x11")]
pub use app::App;

pub use wm_theme::{clock, default_theme::nextstep_classic as nextstep_theme, model, paint, panel, tile};

/// The pixel buffer type the whole SDK is written in terms of —
/// [`App::run`]'s and [`dockapp::Handlers::draw`]'s callbacks both hand
/// one out. Re-exported rather than left as an implicit dependency
/// because a consumer that had to add its own `tiny-skia` line would be
/// one Cargo resolution away from a `Pixmap` that is not *this*
/// `Pixmap`, and the resulting type error names two identical paths.
pub use tiny_skia;

/// Reads the same `CHONKSTEP_SCALE` env var chonkstep itself reads (see
/// `chonkstep::read_scale_factor`) — every window chonkstep manages sits
/// in the same session and should agree on one scale, so an SDK app has
/// no reason to invent its own convention. Deliberately duplicated
/// rather than shared via a common crate: `chonk-ui` apps are meant to
/// be buildable as fully independent X11 clients, with zero dependency
/// on chonkstep's own crates (`wm-core`, `wm-x11`, ...) — the four lines
/// this saves aren't worth coupling the SDK to the WM binary.
pub fn scale_factor() -> f32 {
    std::env::var("CHONKSTEP_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// The theme this session is wearing, from `CHONKSTEP_THEME`.
///
/// Reads the environment for exactly the reason [`scale_factor`] does,
/// and with the same constraint: an SDK app must be buildable against
/// this crate alone, so it cannot call `chonk_shell::startup::resolve_theme`
/// or read the shell's private state file. The environment variable is
/// the session's one published channel for "which theme is active", and
/// `wm-theme`'s own `theme_by_id` — the same lookup `startup.rs` makes —
/// turns it back into a `Theme`.
///
/// This closes a real, visible bug rather than adding a feature: until
/// this existed, [`scaled_theme`] returned the flagship theme
/// unconditionally, so `chonk-about` (which the root menu launches)
/// rendered in NeXTSTEP Classic no matter what the user had picked.
///
/// An unknown id falls back to the flagship with a warning rather than
/// failing, matching `startup::config_theme_fallback`: a stale or
/// misspelled value costs an app the right colors, never its launch.
///
/// Note that this reads what it is *told*. The launcher has to export
/// `CHONKSTEP_THEME` for a child to see it; with the variable absent
/// the behavior is exactly what it was before — the flagship theme.
/// `CHONKSTEP_APPEARANCE` rides beside it (`"light"` / `"dark"`) and
/// picks which of the theme's two renditions to resolve; absent or
/// unrecognized, the theme's own native rendition is used — exactly
/// what `theme_by_id` answered before the appearance axis existed, so
/// an app launched by an older desktop looks the way it always did.
pub fn active_theme() -> model::Theme {
    let Some(id) = std::env::var("CHONKSTEP_THEME").ok() else {
        return nextstep_theme();
    };
    let id = id.trim();
    let appearance = std::env::var("CHONKSTEP_APPEARANCE")
        .ok()
        .and_then(|mode| wm_theme::Appearance::from_name(&mode));
    let theme = match appearance {
        Some(appearance) => wm_theme::default_theme::theme_variant(id, appearance),
        None => wm_theme::default_theme::theme_by_id(id),
    };
    match theme {
        Some(theme) => theme,
        None => {
            tracing::warn!(theme = id, "CHONKSTEP_THEME names an unknown theme; using the default instead");
            nextstep_theme()
        }
    }
}

/// [`active_theme`] scaled by [`scale_factor`] — the theme an app
/// should actually draw with. Every font size, so text an app draws
/// stays crisp (re-shaped at the target size) rather than looking like
/// the unscaled theme's output blown up and blurry.
pub fn scaled_theme() -> model::Theme {
    active_theme().scaled(scale_factor())
}
