//! Session policy: the handful of decisions a chonkstep session makes
//! before it draws anything - how big the UI is, which theme it wears,
//! whether focus follows the mouse - and, deliberately, the one place
//! both binaries make them.
//!
//! These rules are the kind that quietly diverge. Each resolver
//! combines an environment variable, a config-file value, and a
//! built-in default, and every one of them was written for the X11
//! session first; when the Wayland compositor grew its own startup it
//! reimplemented two of them and dropped the environment layer from a
//! third, so a machine with `CHONKSTEP_SCALE=2` in `/etc/environment`
//! got a HiDPI X session and a tiny Wayland one. That is exactly the
//! class of difference the shared-shell architecture exists to
//! prevent, so the policy lives here with the rest of the shared
//! desktop rather than in either binary.
//!
//! Every resolver splits into a thin `read_*` that touches the process
//! environment and a pure `resolve_*` that does not: precedence logic
//! stays unit-testable without `set_var`, which tests running in
//! parallel threads of one process cannot use safely.

use wm_theme::Theme;

use crate::theme_select;

/// UI scale. Precedence: `CHONKSTEP_SCALE` beats the config file's
/// `scale`, which beats 1.0 (no scaling). The environment stays on top
/// because session launchers and dev scripts use it to override a
/// user's baseline per invocation.
pub fn read_scale_factor(config_scale: Option<f32>) -> f32 {
    resolve_scale(std::env::var("CHONKSTEP_SCALE").ok().as_deref(), config_scale)
}

/// Pure core of [`read_scale_factor`]. A value that fails validation -
/// unparseable, non-finite, zero, or negative - is *skipped*, not
/// clamped: a broken env var falls through to the config value and a
/// broken config value falls through to 1.0, so a typo degrades to the
/// next-best answer instead of a garbage scale or a dead session.
pub fn resolve_scale(env: Option<&str>, config: Option<f32>) -> f32 {
    let valid = |s: &f32| s.is_finite() && *s > 0.0;
    env.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(valid)
        .or_else(|| config.filter(valid))
        .unwrap_or(1.0)
}

/// Whether to switch from click-to-focus to focus-follows-mouse.
/// `CHONKSTEP_FOCUS_FOLLOWS_MOUSE` wins over the config file in *both*
/// directions: set to anything other than `1` it forces the policy off
/// even when the config asks for it, so a session script can pin
/// either behavior. Only its total absence lets the config value
/// apply.
pub fn read_focus_follows_mouse(config_value: bool) -> bool {
    resolve_focus_follows_mouse(
        std::env::var("CHONKSTEP_FOCUS_FOLLOWS_MOUSE").ok().as_deref(),
        config_value,
    )
}

/// Pure core of [`read_focus_follows_mouse`].
pub fn resolve_focus_follows_mouse(env: Option<&str>, config: bool) -> bool {
    match env {
        Some(value) => value == "1",
        None => config,
    }
}

/// The theme this session dresses in. Precedence: the persisted
/// theme-menu choice wins over the config file's `theme`, which wins
/// over the flagship default - the menu is the more recent, more
/// deliberate gesture (picking a theme from it restarts on the spot),
/// so a config line written once must not keep overriding it forever.
///
/// [`theme_select::load`] already implements the outer two layers
/// (state file, else flagship); the config layer slots between them
/// here rather than inside that module, which is a pure persist/recall
/// mechanism the menu itself also uses. Which theme wins at *startup*
/// is session policy.
pub fn resolve_theme(config_theme: Option<&str>) -> Theme {
    if !persisted_theme_choice_exists() {
        if let Some(theme) = config_theme_fallback(config_theme) {
            return theme;
        }
    }
    theme_select::load()
}

/// The config-file layer of [`resolve_theme`], kept free of filesystem
/// access so its edges are testable: `None` when no theme is
/// configured, and - critically - `None` with a warning, not an error,
/// when the configured id names a theme this build does not ship. A
/// misspelled theme must cost the user the flagship look for one
/// session, never the session itself.
pub fn config_theme_fallback(config_theme: Option<&str>) -> Option<Theme> {
    let requested = config_theme?;
    let theme = wm_theme::default_theme::theme_by_id(requested);
    if theme.is_none() {
        tracing::warn!(theme = requested, "config names an unknown theme; using the default instead");
    }
    theme
}

/// `true` exactly when [`theme_select::load`] would return a persisted
/// menu choice rather than its flagship fallback: the state file
/// exists and names a theme this build still ships. Must stay in
/// lockstep with `theme_select`'s own state path.
pub fn persisted_theme_choice_exists() -> bool {
    let path = if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        std::path::PathBuf::from(root).join("chonkstep/theme")
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local/state/chonkstep/theme")
    } else {
        return false;
    };
    std::fs::read_to_string(path)
        .ok()
        .map(|id| id.trim().to_string())
        .is_some_and(|id| wm_theme::default_theme::theme_by_id(&id).is_some())
}

/// Sets `XCURSOR_SIZE` on this process (inherited by every app the
/// session spawns) unless it is already set. Both sessions scale their
/// own chrome and their own pointer, but an application that draws its
/// *own* cursor through Xcursor - most toolkits, and every X11 client
/// under XWayland - has no way to learn about that scale and reads
/// this variable instead. Without it, such an app's cursor is visibly
/// out of proportion the instant the pointer crosses onto its content.
/// 24px is Xcursor's own conventional 1x base size.
pub fn ensure_xcursor_size(scale: f32) {
    if std::env::var_os("XCURSOR_SIZE").is_some() {
        return;
    }
    let size = (24.0 * scale).round().max(1.0) as u32;
    // SAFETY: called once at the very start of a session's startup,
    // before any other thread exists, so no concurrent env access is
    // possible.
    unsafe {
        std::env::set_var("XCURSOR_SIZE", size.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_defaults_to_one_with_neither_source() {
        assert_eq!(resolve_scale(None, None), 1.0);
    }
    #[test]
    fn unparseable_env_scale_falls_through_to_config() {
        // A typo'd env var must degrade to the next-best answer, not to
        // the hard default — the config value is still a deliberate
        // user choice.
        assert_eq!(resolve_scale(Some("garbage"), Some(1.5)), 1.5);
    }
    #[test]
    fn out_of_range_env_scale_falls_through_to_config() {
        assert_eq!(resolve_scale(Some("0"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("-2"), Some(1.5)), 1.5);
        // `parse::<f32>` accepts these spellings; the validity filter
        // must still reject them (a NaN scale poisons every pixel
        // computation downstream).
        assert_eq!(resolve_scale(Some("inf"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("NaN"), Some(1.5)), 1.5);
    }
    #[test]
    fn invalid_config_scale_falls_through_to_default() {
        assert_eq!(resolve_scale(None, Some(0.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(-1.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::NAN)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::INFINITY)), 1.0);
    }
    #[test]
    fn both_sources_invalid_still_yields_a_usable_scale() {
        // The end-to-end graceful-degradation guarantee: no combination
        // of broken inputs may leave the session without a scale.
        assert_eq!(resolve_scale(Some("bogus"), Some(-3.0)), 1.0);
    }
    #[test]
    fn whitespace_around_env_scale_is_tolerated() {
        assert_eq!(resolve_scale(Some(" 1.5 "), None), 1.5);
    }
    #[test]
    fn env_focus_var_enables_over_config_off() {
        assert!(resolve_focus_follows_mouse(Some("1"), false));
    }
    #[test]
    fn env_focus_var_disables_over_config_on() {
        // The env var wins in BOTH directions: any present value other
        // than "1" pins the policy off regardless of the config.
        assert!(!resolve_focus_follows_mouse(Some("0"), true));
        assert!(!resolve_focus_follows_mouse(Some(""), true));
        assert!(!resolve_focus_follows_mouse(Some("true"), true));
    }
    #[test]
    fn config_focus_value_applies_without_env() {
        assert!(resolve_focus_follows_mouse(None, true));
        assert!(!resolve_focus_follows_mouse(None, false));
    }
    #[test]
    fn config_theme_resolves_known_ids() {
        for id in ["nextstep-classic", "amber-phosphor", "teal-blueprint", "graphite", "next-lavender"] {
            let theme = config_theme_fallback(Some(id));
            assert_eq!(theme.map(|t| t.id), Some(id.to_string()), "built-in theme id {id} must resolve from config");
        }
    }
    #[test]
    fn unknown_config_theme_degrades_to_none() {
        // `resolve_theme` then falls through to the flagship — a
        // misspelled theme name must never cost the user the session.
        assert!(config_theme_fallback(Some("no-such-theme")).is_none());
        assert!(config_theme_fallback(Some("")).is_none());
    }
    #[test]
    fn absent_config_theme_is_not_an_error() {
        assert!(config_theme_fallback(None).is_none());
    }
}
