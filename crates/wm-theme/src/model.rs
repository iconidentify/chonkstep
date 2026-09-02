use serde::{Deserialize, Serialize};
use wm_theme_api::ButtonKind;

/// The session-wide light/dark axis every theme is rendered along.
///
/// An appearance is not a theme: the theme decides *which* desktop you
/// have (Amber Phosphor, Teal Blueprint, ...) and the appearance
/// decides which of that theme's two renditions you are looking at.
/// Every built-in theme ships both — same identity, same chrome
/// geometry, two deliberate palettes — and
/// `default_theme::theme_variant` resolves an `(id, Appearance)` pair
/// to the right one. A [`Theme`] value records which rendition it is
/// in [`Theme::appearance`], so anything holding a resolved theme
/// (the shell, a dockapp fed `theme_toml`) can tell without asking.
///
/// Serialized in kebab-lowercase (`"light"` / `"dark"`) — the same
/// spelling the config file, the published state file and the
/// appearance-request IPC file all use, so there is exactly one
/// vocabulary for the axis everywhere it appears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    /// The default: seven of the eight built-ins are natively dark,
    /// and a value that deserializes from a source too old to name an
    /// appearance should look like that source did when it was written.
    #[default]
    Dark,
}

impl Appearance {
    /// Parses the one vocabulary (`"light"` / `"dark"`, trimmed,
    /// case-insensitive). `None` for anything else — every caller
    /// (config parsing, the request file) wants to warn-and-skip
    /// rather than guess.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    /// The canonical spelling, for files and logs.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The other rendition — what an `appearance-request` of `toggle`
    /// resolves to.
    pub const fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

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

    /// Parses `#rrggbb` (case-insensitive; a trailing `aa` byte is
    /// accepted and dropped, since every consumer of this type paints
    /// chrome opaque). `None` for anything else — callers that read
    /// user-authored palettes want to warn and derive, not guess.
    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.trim().strip_prefix('#')?;
        // Byte-indexed below, so the text has to be ASCII first: six
        // *bytes* of `#éé` would otherwise be sliced mid code point,
        // and this parses palettes people type.
        if !hex.is_ascii() || (hex.len() != 6 && hex.len() != 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// The `#rrggbb` spelling every palette file in the wild uses —
    /// the inverse of [`Color::from_hex`], alpha omitted.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Linear interpolation toward `other` by `amount` in `0.0..=1.0`
    /// (clamped), rounded half-up per channel — the same arithmetic
    /// Omarchy's own `mix_color` performs, so a shade this derives
    /// matches the one its template engine would have written.
    pub fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| (a as f32 * (1.0 - amount) + b as f32 * amount + 0.5).floor() as u8;
        Self::rgb(channel(self.r, other.r), channel(self.g, other.g), channel(self.b, other.b))
    }

    /// WCAG relative luminance in `0.0..=1.0` (sRGB linearized, Rec.709
    /// weights) — the perceptual brightness contrast decisions are made
    /// from, not the plain channel mean.
    pub fn relative_luminance(self) -> f32 {
        let linear = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// WCAG contrast ratio between two colors, `1.0..=21.0`; symmetric.
    pub fn contrast_ratio(self, other: Self) -> f32 {
        let (a, b) = (self.relative_luminance() + 0.05, other.relative_luminance() + 0.05);
        if a > b { a / b } else { b / a }
    }
}

/// How a background surface is painted. Textured/pixmap fills are part
/// of the classic vocabulary but out of scope for milestone 1 — an
/// additive variant here later, not a rewrite.
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

/// Diagonal (top-left to bottom-right) is the signature titlebar look
/// of the classic NeXT-style desktop; vertical and horizontal are the
/// other two the era's themes are written in.
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

/// No `fill` field: classic titlebar buttons aren't a
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
    /// The NeXTSTEP-faithful value is a hardcoded `3` regardless of
    /// titlebar height; the later, flatter chrome styles of the era use
    /// a flush `0` instead, which is why this is a field and not a
    /// constant.
    pub button_margin: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResizeBarStyle {
    pub height: u16,
    pub fill: Fill,
    pub bevel: Bevel,
    /// Width of the corner grip regions at each end of the bar — 28px
    /// unscaled, the classic value. Both the etched notch lines
    /// `render_decoration` draws and the SouthEast/SouthWest hit
    /// regions derive from this same value, so the visible grip
    /// delimiters and the diagonal-resize zones always agree exactly.
    pub corner_width: u16,
}

/// The tile: the square platform every dock item, icon, and Clip sits
/// on — this theme system's common UI surface, and the piece that
/// makes disparate widgets read as one family. The distinctive classic
/// look here is a diagonal gradient (a6a6b6 to 515561), not a flat
/// fill; each built-in theme supplies its own gradient in that spirit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TileStyle {
    pub fill: Fill,
    /// Relief thickness comes from this bevel's width (the relief
    /// itself is the relative double raised recipe, like all chrome).
    pub bevel: Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BorderStyle {
    pub width: u8,
    pub color_active: Color,
    pub color_inactive: Color,
}

/// NeXTSTEP-style popup menu (the root menu and app menus):
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
    /// Row height for entries; the menu's width has no counterpart
    /// here on purpose — these menus size themselves to their widest
    /// entry, so width is derived from content at render time, never
    /// configured.
    pub item_height: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    /// Stable kebab-case identity — what gets persisted when the user
    /// picks a theme, so `name` can be reworded freely. Both of a
    /// theme's renditions share this id: the appearance axis picks
    /// between them, it never forks the identity.
    pub id: String,
    pub name: String,
    /// Which rendition of the theme this value is — see [`Appearance`].
    /// `#[serde(default)]` (= `Dark`) so a `theme_toml` written before
    /// the axis existed still deserializes, wearing the mood it was
    /// authored in.
    #[serde(default)]
    pub appearance: Appearance,
    pub titlebar: TitlebarStyle,
    pub resize_bar: ResizeBarStyle,
    pub border: BorderStyle,
    pub menu: MenuStyle,
    /// See [`TileStyle`].
    pub tile: TileStyle,
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
    /// Terminal window opacity in percent (e.g. 88 = slightly
    /// translucent glass over the wallpaper), `None` = fully opaque.
    /// Applied by the session compositor to the terminal's whole frame
    /// via `_NET_WM_WINDOW_OPACITY` (see `add_opacity_rule` in
    /// `wm-x11`), never by the terminal itself: both of urxvt's own
    /// transparency mechanisms were tried first and reverted — its
    /// pseudo-transparency silently goes opaque on larger windows, and
    /// its 32-bit-visual alpha background leaves stale framebuffer
    /// garbage in regions it fails to repaint on scroll/resize (both
    /// confirmed live).
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
        // "flush to the edge" button placement and must stay exactly 0
        // at any scale — the min-1 clamp the other u16 fields want
        // would silently un-flush it.
        theme.titlebar.button_margin = ((theme.titlebar.button_margin as f32) * factor).round() as u16;
        for button in &mut theme.titlebar.buttons {
            button.size = scale_u16(button.size);
            button.bevel.width = scale_u8(button.bevel.width);
        }

        theme.tile.bevel.width = scale_u8(theme.tile.bevel.width);
        theme.resize_bar.height = scale_u16(theme.resize_bar.height);
        theme.resize_bar.bevel.width = scale_u8(theme.resize_bar.bevel.width);
        theme.resize_bar.corner_width = scale_u16(theme.resize_bar.corner_width);

        theme.border.width = scale_u8(theme.border.width);

        theme.menu.title_font.size *= factor;
        theme.menu.item_font.size *= factor;
        theme.menu.bevel.width = scale_u8(theme.menu.bevel.width);
        theme.menu.item_height = scale_u16(theme.menu.item_height);

        theme
    }
}

#[cfg(test)]
mod tests {
    use super::Color;
    use crate::default_theme::nextstep_classic;

    #[test]
    fn hex_colours_parse_in_every_spelling_people_use_and_refuse_the_rest() {
        assert_eq!(Color::from_hex("#1a2B3c"), Some(Color::rgb(0x1a, 0x2b, 0x3c)));
        assert_eq!(Color::from_hex("  #1a2b3cff "), Some(Color::rgb(0x1a, 0x2b, 0x3c)), "alpha is accepted and dropped");
        for bad in ["1a2b3c", "#1a2b3", "#1a2b3c7", "#gg0000", "", "#", "#éé", "#1a2b3cé"] {
            assert_eq!(Color::from_hex(bad), None, "{bad:?}");
        }
        assert_eq!(Color::from_hex(&Color::rgb(7, 8, 9).hex()), Some(Color::rgb(7, 8, 9)), "hex() is the inverse");
    }

    #[test]
    fn contrast_and_mixing_follow_the_wcag_arithmetic() {
        let black = Color::rgb(0, 0, 0);
        let white = Color::rgb(255, 255, 255);
        assert!((white.contrast_ratio(black) - 21.0).abs() < 0.01, "black on white is the 21:1 maximum");
        assert!((black.contrast_ratio(white) - 21.0).abs() < 0.01, "and the ratio is symmetric");
        assert!((white.contrast_ratio(white) - 1.0).abs() < 0.01);
        assert_eq!(black.mix(white, 0.0), black);
        assert_eq!(black.mix(white, 1.0), white);
        assert_eq!(black.mix(white, 0.5), Color::rgb(128, 128, 128), "half-up rounding per channel");
        assert_eq!(black.mix(white, 2.0), white, "the amount is clamped");
    }

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
