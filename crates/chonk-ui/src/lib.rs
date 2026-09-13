
#[cfg(feature = "x11")]
mod app;

#[cfg(feature = "x11")]
pub use app::App;

pub use wm_theme::{default_theme::nextstep_classic as nextstep_theme, model, paint, tile};

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
/// this crate alone, so it cannot call `chonk_shell::startup::resolve_look`
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
///
/// `CHONKSTEP_THEME=omarchy` means the desk follows Omarchy's current
/// theme (`wm_theme::omarchy`): the app reads the same `colors.toml`
/// the shell did, so it wears what the desk wears. The appearance
/// variable is moot there — an Omarchy palette has one mood, its own.
/// With no readable palette the flagship stands in, as it does on the
/// desk.
pub fn active_theme() -> model::Theme {
    let Some(id) = std::env::var("CHONKSTEP_THEME").ok() else {
        return nextstep_theme();
    };
    let id = id.trim();
    if id == wm_theme::omarchy::ID {
        return wm_theme::omarchy::load_current().unwrap_or_else(|reason| {
            tracing::warn!(reason, "CHONKSTEP_THEME says to follow Omarchy, but its palette is unreadable; using the default instead");
            nextstep_theme()
        });
    }
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
