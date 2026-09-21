//! OS/2 Warp 4's fixed Workplace Shell palette, independent of app appearance.
use crate::model::{Color, Fill, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::{Appearance, Theme};
use wm_theme_api::{ButtonKind, DecorationStyle};

pub const DESKTOP: Color = Color::rgb(0, 0, 85);
pub const PANEL: Color = Color::rgb(207, 207, 207);
pub const TITLE: Color = Color::rgb(40, 0, 170);
pub const BLUE: Color = Color::rgb(0, 0, 170);
pub const SHADE: Color = Color::rgb(130, 130, 130);
pub const INK: Color = Color::rgb(0, 0, 0);
pub const WHITE: Color = Color::rgb(255, 255, 255);

pub fn theme(id: &str, appearance: Appearance) -> Option<Theme> {
    if id != "os2-warp-4" {
        return None;
    }
    let mut theme = crate::default_theme::nextstep_classic_light();
    theme.id = id.into();
    theme.name = "OS/2 Warp 4".into();
    theme.appearance = appearance;
    theme.preferred_decoration_style = Some(DecorationStyle::OS2Warp);
    theme.wallpaper = "os2-warp-4".into();
    theme.titlebar.height = 24;
    theme.titlebar.active = Fill::Solid(TITLE);
    theme.titlebar.inactive = Fill::Solid(SHADE);
    theme.titlebar.text_color_active = WHITE;
    theme.titlebar.text_color_inactive = PANEL;
    theme.titlebar.font = font(true);
    theme.titlebar.text_align = TextAlign::Left;
    theme.border.width = 4;
    for (button, kind) in theme.titlebar.buttons.iter_mut().zip([
        ButtonKind::Close,
        ButtonKind::Miniaturize,
        ButtonKind::Maximize,
    ]) {
        button.kind = kind;
        button.size = 14;
    }
    theme.border.color_active = PANEL;
    theme.border.color_inactive = PANEL;
    theme.menu.title_font = font(true);
    theme.menu.item_font = font(false);
    theme.menu.item_height = 20;
    theme.menu.title_bar = Fill::Solid(TITLE);
    theme.menu.title_text_color = WHITE;
    theme.menu.background = Fill::Solid(PANEL);
    theme.menu.text_color = INK;
    theme.menu.highlight_background = Fill::Solid(BLUE);
    theme.menu.highlight_text_color = WHITE;
    theme.tile.fill = Fill::Solid(PANEL);
    (theme.terminal.bg, theme.terminal.fg) = match appearance {
        Appearance::Light => (WHITE, INK),
        Appearance::Dark => (Color::rgb(0, 0, 40), WHITE),
    };
    theme.terminal.cursor = theme.terminal.fg;
    theme.terminal.opacity = None;
    Some(theme)
}
fn font(bold: bool) -> FontSpec {
    // External applications use a resident compatible sans; our own chrome
    // uses the independently verified WarpSans bitmap, including its advances.
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
