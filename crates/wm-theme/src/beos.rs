//! BeOS R5's fixed desktop/chrome palette, with a separate application appearance.
use crate::model::{Color, Fill, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::{Appearance, Theme};
use wm_theme_api::DecorationStyle;

pub const DESKTOP: Color = Color::rgb(48, 100, 152);
pub const PANEL: Color = Color::rgb(216, 216, 216);
pub const YELLOW: Color = Color::rgb(252, 200, 0);
pub const MENU_SELECTION: Color = Color::rgb(152, 152, 152);
pub const BLUE: Color = Color::rgb(0, 0, 152);
pub const INK: Color = Color::rgb(0, 0, 0);
pub const WHITE: Color = Color::rgb(252, 252, 252);

pub fn theme(id: &str, appearance: Appearance) -> Option<Theme> {
    if id != "beos" {
        return None;
    }
    let mut theme = crate::default_theme::nextstep_classic_light();
    theme.id = id.into();
    theme.name = "BeOS R5".into();
    theme.appearance = appearance;
    theme.preferred_decoration_style = Some(DecorationStyle::BeOS);
    theme.wallpaper = "beos-blue".into();
    theme.titlebar.height = 19;
    theme.titlebar.active = Fill::Solid(YELLOW);
    theme.titlebar.inactive = Fill::Solid(Color::rgb(232, 232, 232));
    theme.titlebar.text_color_active = INK;
    theme.titlebar.text_color_inactive = Color::rgb(80, 80, 80);
    theme.titlebar.font = font(true);
    theme.titlebar.text_align = TextAlign::Left;
    theme.border.width = 5;
    for (button, kind) in theme.titlebar.buttons.iter_mut().zip([
        wm_theme_api::ButtonKind::Close,
        wm_theme_api::ButtonKind::Maximize,
    ]) {
        button.kind = kind;
        button.size = 14;
    }
    theme.border.color_active = PANEL;
    theme.border.color_inactive = Color::rgb(232, 232, 232);
    theme.menu.title_font = font(true);
    theme.menu.item_font = font(false);
    theme.menu.item_height = 20;
    theme.menu.title_bar = Fill::Solid(PANEL);
    theme.menu.title_text_color = INK;
    theme.menu.background = Fill::Solid(PANEL);
    theme.menu.text_color = INK;
    theme.menu.highlight_background = Fill::Solid(MENU_SELECTION);
    theme.menu.highlight_text_color = INK;
    theme.tile.fill = Fill::Solid(PANEL);
    (theme.terminal.bg, theme.terminal.fg) = match appearance {
        Appearance::Light => (WHITE, INK),
        Appearance::Dark => (Color::rgb(24, 28, 32), WHITE),
    };
    theme.terminal.cursor = theme.terminal.fg;
    theme.terminal.opacity = None;
    Some(theme)
}

fn font(bold: bool) -> FontSpec {
    FontSpec {
        family: "Liberation Sans".into(),
        size: 12.0,
        weight: if bold {
            FontWeight::Bold
        } else {
            FontWeight::Normal
        },
        style: FontStyle::Normal,
    }
}
