use serde::{Deserialize, Serialize};
use wm_theme_api::ButtonKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

/// One of WindowMaker's texture kinds for a background. Textured/pixmap
/// fills (`TPIXMAP`) are a real WindowMaker feature but out of scope for
/// milestone 1 — an additive variant here later, not a rewrite.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Fill {
    Solid(Color),
    Gradient(Gradient),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub direction: GradientDirection,
    pub from: Color,
    pub to: Color,
}

/// WM: `VGRADIENT` / `HGRADIENT` / `DGRADIENT`. Diagonal (top-left to
/// bottom-right) is the signature WindowMaker titlebar look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GradientDirection {
    Vertical,
    Horizontal,
    Diagonal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontWeight {
    Normal,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontStyle {
    Normal,
    Italic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontSpec {
    /// Fontconfig-style family name; resolved (with fallback) at render
    /// time by `wm-theme`'s text rendering, not here.
    pub family: String,
    /// Pixels, not points, so a theme looks pixel-identical across
    /// displays.
    pub size: f32,
    pub weight: FontWeight,
    pub style: FontStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BevelStyle {
    Raised,
    Sunken,
    Flat,
}

/// The classic NeXTSTEP chiseled border: hard, non-anti-aliased 1-2px
/// edges — `light` on the implied-top-left-lightsource edges for
/// `Raised`, swapped for `Sunken`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bevel {
    pub style: BevelStyle,
    pub width: u8,
    pub light: Color,
    pub dark: Color,
}

/// No `fill` field: real WindowMaker's titlebar buttons aren't a
/// separately-colored control sitting on the titlebar, they're the
/// titlebar's own active/inactive fill showing straight through, framed
/// by just a bevel — `render_decoration` paints buttons with the exact
/// same fill it just used for the titlebar itself, not a per-button one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ButtonStyle {
    pub kind: ButtonKind,
    pub size: u16,
    pub bevel: Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitlebarStyle {
    pub height: u16,
    pub active: Fill,
    pub inactive: Fill,
    pub font: FontSpec,
    pub text_color_active: Color,
    pub text_color_inactive: Color,
    pub text_align: TextAlign,
    pub bevel: Bevel,
    pub buttons: Vec<ButtonStyle>,
    /// How far a button's outer edge sits from the titlebar's own edge.
    /// Real WindowMaker's `TS_NEXT` (the actual NeXTSTEP-lookalike
    /// style, `src/framewin.c`) hardcodes this to `3` regardless of
    /// titlebar height — distinct from `TS_NEW` (WindowMaker's own,
    /// non-NeXT default style), which uses a flush `0` instead.
    pub button_margin: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResizeBarStyle {
    pub height: u16,
    pub fill: Fill,
    pub bevel: Bevel,
    /// Width of the corner grip regions at each end of the bar — real
    /// WindowMaker's `RESIZEBAR_CORNER_WIDTH` (28, `src/wconfig.h.in`).
    /// Both the etched notch lines `render_decoration` draws and the
    /// SouthEast/SouthWest hit regions derive from this same value, so
    /// the visible grip delimiters and the diagonal-resize zones always
    /// agree exactly.
    pub corner_width: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BorderStyle {
    pub width: u8,
    pub color_active: Color,
    pub color_inactive: Color,
}

/// WindowMaker/NeXTSTEP-style popup menu (the root menu and app menus):
/// a titled bar above a vertical list of items, each item inverting to
/// `highlight_background`/`highlight_text_color` on hover/selection, the
/// whole thing framed by the same 3D bevel language as window chrome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuStyle {
    pub title_font: FontSpec,
    pub item_font: FontSpec,
    pub title_bar: Fill,
    /// Menu titles sit on a dark bar (matching the window titlebar's
    /// active-focus treatment) with light text — distinct from item
    /// text, which sits on a light background with dark text.
    pub title_text_color: Color,
    pub background: Fill,
    pub text_color: Color,
    pub highlight_background: Fill,
    pub highlight_text_color: Color,
    pub bevel: Bevel,
    pub item_height: u16,
    pub min_width: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    /// Stable kebab-case identity — what gets persisted when the user
    /// picks a theme, so `name` can be reworded freely.
    pub id: String,
    pub name: String,
    pub titlebar: TitlebarStyle,
    pub resize_bar: ResizeBarStyle,
    pub border: BorderStyle,
    pub menu: MenuStyle,
    /// Terminal colors spawned terminals launch with — themes restyle
    /// the whole desktop, terminals included.
    pub terminal: TerminalPalette,
    /// Id of the wallpaper artwork this theme composes with (resolved
    /// by the desktop shell, which owns the embedded images) — picking
    /// the theme selects this wallpaper too.
    pub wallpaper: String,
    // Icon/miniwindow appearance and Dock/Clip iconography: out of scope
    // for this milestone.
}

/// A full terminal color scheme: foreground/background/cursor plus the
/// 16-slot ANSI palette (colors 0-7 normal, 8-15 bright), in the order
/// terminals expect them. Colors only — font and geometry are not a
/// per-theme concern.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerminalPalette {
    pub fg: Color,
    pub bg: Color,
    pub cursor: Color,
    pub ansi: [Color; 16],
    /// Terminal background opacity in percent (e.g. 85 = slightly
    /// translucent glass over the wallpaper), `None` = fully opaque.
    /// Realized as true alpha: the session runs a compositor (picom,
    /// started by `scripts/xsession.sh`) and terminals launch with a
    /// 32-bit visual and an alpha-tagged background color. urxvt's own
    /// compositor-free pseudo-transparency was tried first and
    /// abandoned: its 9.31 background engine silently falls back to an
    /// opaque background for larger windows (confirmed live — the same
    /// arguments ghost at 600x400 and go flat at 1300x800).
    pub opacity: Option<u8>,
}

impl Theme {
    /// Returns a copy with every pixel-valued dimension (titlebar/button/
    /// bevel/border sizes, font sizes, menu metrics) multiplied by
    /// `factor` — colors are untouched. Exists for HiDPI displays: this
    /// theme's absolute pixel sizes (a 20px titlebar, 14px buttons) were
    /// chosen assuming roughly 1:1 pixel density, and read as tiny on a
    /// modern high-density panel with no display-level scaling in play
    /// (as is typical for a nested/nonnative X server). `factor` above
    /// 1.0 scales the WM's whole chrome up to compensate.
    pub fn scaled(&self, factor: f32) -> Theme {
        if factor == 1.0 {
            return self.clone();
        }
        let scale_u8 = |v: u8| ((v as f32) * factor).round().clamp(1.0, 255.0) as u8;
        let scale_u16 = |v: u16| ((v as f32) * factor).round().max(1.0) as u16;

        let mut theme = self.clone();

        theme.titlebar.height = scale_u16(theme.titlebar.height);
        theme.titlebar.font.size *= factor;
        theme.titlebar.bevel.width = scale_u8(theme.titlebar.bevel.width);
        // Unlike every other dimension, a margin of 0 is a deliberate
        // "flush to the edge" (WindowMaker's own default TS_NEW button
        // placement) and must stay exactly 0 at any scale — the min-1
        // clamp the other u16 fields want would silently un-flush it.
        theme.titlebar.button_margin = ((theme.titlebar.button_margin as f32) * factor).round() as u16;
        for button in &mut theme.titlebar.buttons {
            button.size = scale_u16(button.size);
            button.bevel.width = scale_u8(button.bevel.width);
        }

        theme.resize_bar.height = scale_u16(theme.resize_bar.height);
        theme.resize_bar.bevel.width = scale_u8(theme.resize_bar.bevel.width);
        theme.resize_bar.corner_width = scale_u16(theme.resize_bar.corner_width);

        theme.border.width = scale_u8(theme.border.width);

        theme.menu.title_font.size *= factor;
        theme.menu.item_font.size *= factor;
        theme.menu.bevel.width = scale_u8(theme.menu.bevel.width);
        theme.menu.item_height = scale_u16(theme.menu.item_height);
        theme.menu.min_width = scale_u16(theme.menu.min_width);

        theme
    }
}

#[cfg(test)]
mod tests {
    use crate::default_theme::nextstep_classic;

    #[test]
    fn scaled_doubles_pixel_dimensions_but_not_colors() {
        let base = nextstep_classic();
        let doubled = base.scaled(2.0);

        assert_eq!(doubled.titlebar.height, base.titlebar.height * 2);
        assert_eq!(doubled.border.width, base.border.width * 2);
        assert_eq!(doubled.menu.item_height, base.menu.item_height * 2);
        assert!((doubled.titlebar.font.size - base.titlebar.font.size * 2.0).abs() < f32::EPSILON);
        assert_eq!(doubled.titlebar.text_color_active, base.titlebar.text_color_active);
    }

    #[test]
    fn scaled_by_one_is_a_plain_copy() {
        let base = nextstep_classic();
        assert_eq!(base.scaled(1.0), base);
    }
}
