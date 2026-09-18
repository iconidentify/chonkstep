use std::path::PathBuf;

/// Keep the existing pin and ordering files so upgrading restores the desk.
pub fn state_file(name: &str) -> Option<PathBuf> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(root.join("chonkstep").join(name))
}

pub fn launch_env(
    theme: &str,
    appearance: Option<wm_theme::Appearance>,
    scale: f32,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("CHONKSTEP_THEME".into(), theme.into()),
        ("CHONKSTEP_SCALE".into(), scale.to_string()),
    ];
    if let Some(mode) = appearance {
        env.push(("CHONKSTEP_APPEARANCE".into(), mode.name().into()));
    }
    env
}
