//! Persists which built-in theme this session dresses in — the same
//! one-file-one-id state mechanism `wallpaper.rs` uses, kept separate
//! from it because a theme *implies* a wallpaper (picking one persists
//! both) while the Wallpaper menu can still override the artwork
//! afterward without touching the theme.

use std::path::PathBuf;

use wm_theme::Theme;

/// The selected theme, falling back to the flagship when this is the
/// first launch or the state file names a theme from another version.
pub fn load() -> Theme {
    state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|id| wm_theme::default_theme::theme_by_id(id.trim()))
        .unwrap_or_else(wm_theme::default_theme::nextstep_classic)
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
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(root).join("chonkstep/theme"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/chonkstep/theme"))
}
