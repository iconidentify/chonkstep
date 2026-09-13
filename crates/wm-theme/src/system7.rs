//! System 7 themes: monochrome document chrome and QuickDraw desktop patterns.
use crate::{Appearance, Theme};
use crate::model::Color;
use wm_theme_api::DecorationStyle;

pub const CHOICES: [(&str, &str); 3] = [
    ("system-7-classic", "System 7 Classic"),
    ("system-7-light-gray", "System 7 Light Gray"),
    ("system-7-dark-gray", "System 7 Dark Gray"),
];

pub fn theme(id: &str, appearance: Appearance) -> Option<Theme> {
    let (_, name) = CHOICES.iter().find(|(choice, _)| *choice == id)?;
    let mut theme = match appearance {
        Appearance::Light => crate::default_theme::nextstep_classic_light(),
        Appearance::Dark => crate::default_theme::nextstep_classic(),
    };
    theme.id = id.into();
    theme.name = (*name).into();
    theme.preferred_decoration_style = Some(DecorationStyle::System7);
    theme.wallpaper = format!("{id}-pattern");
    let white = Color::rgb(255, 255, 255);
    let black = Color::rgb(0, 0, 0);
    (theme.terminal.bg, theme.terminal.fg) = match appearance {
        Appearance::Light => (white, black),
        Appearance::Dark => (black, white),
    };
    theme.terminal.cursor = theme.terminal.fg;
    theme.terminal.opacity = None;
    Some(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_themes_choose_system7_without_a_global_override() {
        for (id, _) in CHOICES {
            let theme = crate::default_theme::theme_by_id(id).unwrap();
            assert_eq!(theme.appearance, Appearance::Light);
            assert_eq!(theme.resolve_style(DecorationStyle::Auto), DecorationStyle::System7);
            assert_eq!(theme.resolve_style(DecorationStyle::Modern), DecorationStyle::Modern);
            let serialized = toml::to_string(&theme).unwrap();
            let restored: Theme = toml::from_str(&serialized).unwrap();
            assert_eq!(restored.resolve_style(DecorationStyle::Auto), DecorationStyle::System7);
        }
    }
}
