//! Persists which theme this session dresses in — the same
//! one-file-one-id state mechanism `wallpaper.rs` uses, kept separate
//! from it because a theme *implies* a wallpaper (picking one persists
//! both) while the Wallpaper menu can still override the artwork
//! afterward without touching the theme.
//!
//! The file holds an id: one of the built-ins', or
//! [`wm_theme::omarchy::ID`], which means "whatever Omarchy's current
//! theme is" — the file records the *choice* to follow, never the
//! palette that choice resolved to, so Omarchy switching themes
//! underneath us needs no write here.

use std::path::PathBuf;

/// Whether `id` names something a session can dress in: a built-in, or
/// the follow-Omarchy pseudo-theme.
pub fn is_known(id: &str) -> bool {
    id == wm_theme::omarchy::ID || wm_theme::default_theme::theme_by_id(id).is_some()
}

/// The persisted choice, when there is one and this build still knows
/// it. `None` on a first launch or when the file names a theme from
/// another version — the caller falls back to the config, then the
/// flagship.
pub fn load_id() -> Option<String> {
    let id = std::fs::read_to_string(state_path()?).ok()?;
    let id = id.trim();
    is_known(id).then(|| id.to_string())
}

pub fn persist(id: &str) -> std::io::Result<()> {
    let Some(path) = state_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, id)
}

fn state_path() -> Option<PathBuf> {
    crate::startup::state_file("theme")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_follow_omarchy_id_is_a_known_choice_beside_the_built_ins() {
        assert!(is_known("omarchy"));
        assert!(is_known("nextstep-classic"));
        assert!(!is_known("no-such-theme"));
        assert!(!is_known(""));
    }
}
