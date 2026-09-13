//! Shared modern design language. Palettes and metrics are data; neither the
//! shell nor instruments need to know which named theme supplied them.
use crate::model::{Appearance, Color, Fill, FontStyle, FontWeight, TextAlign, Theme};
use serde::{Deserialize, Serialize};
use tiny_skia::Pixmap;
use wm_theme_api::{FrameMetrics, OverviewMetrics};

/// Visual-only effect tokens. Bounds protect imported themes; metric scaling
/// happens with the rest of Chrome, never inside a backend or render loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub x: i16,
    pub y: i16,
    pub blur: u16,
    pub color: Color,
}

impl Shadow {
    pub fn normalized(mut self) -> Self {
        self.x = self.x.clamp(-512, 512);
        self.y = self.y.clamp(-512, 512);
        self.blur = self.blur.min(512);
        self
    }
    pub fn scaled(mut self, factor: f32) -> Self {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        self.x = (f32::from(self.x) * factor).round() as i16;
        self.y = (f32::from(self.y) * factor).round() as i16;
        self.blur = (f32::from(self.blur) * factor).round() as u16;
        self.normalized()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chrome {
    /// Instrument-specific overrides use shared semantic colors by default.
    #[serde(default)]
    pub instrument: InstrumentChrome,
    #[serde(default, deserialize_with = "deserialize_shadow")]
    pub shadow: Shadow,
    #[serde(default, deserialize_with = "deserialize_frame")]
    pub frame: FrameMetrics,
    #[serde(default, deserialize_with = "deserialize_overview")]
    pub overview: OverviewMetrics,
    pub background: Color,
    pub panel: Color,
    pub surface: Color,
    pub raised: Color,
    pub text: Color,
    pub muted: Color,
    pub line: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub selection: Color,
    pub success: Color,
    pub danger: Color,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstrumentChrome {
    pub background: Option<Color>,
    pub border: Option<Color>,
    pub accent_edge: u16,
}

fn deserialize_shadow<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Shadow, D::Error> {
    Shadow::deserialize(deserializer).map(Shadow::normalized)
}

fn deserialize_frame<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<FrameMetrics, D::Error> {
    FrameMetrics::deserialize(deserializer).map(FrameMetrics::normalized)
}

fn deserialize_overview<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<OverviewMetrics, D::Error> {
    OverviewMetrics::deserialize(deserializer).map(OverviewMetrics::normalized)
}

impl Chrome {
    pub fn normalized(mut self) -> Self {
        self.instrument.accent_edge = self.instrument.accent_edge.min(64);
        self.shadow = self.shadow.normalized();
        self.frame = self.frame.normalized();
        self.overview = self.overview.normalized();
        self
    }

    pub fn scaled(mut self, factor: f32) -> Self {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        self.instrument.accent_edge =
            ((f32::from(self.instrument.accent_edge) * factor).round() as u16).min(64);
        self.shadow = self.shadow.scaled(factor);
        self.frame = self.frame.scaled(factor);
        self.overview = self.overview.scaled(factor);
        self
    }

    /// A Modern recipe can also dress an imported palette. Resolve this once
    /// with the theme, never by reading configuration in a painter.
    pub fn from_theme(theme: &Theme) -> Self {
        Self::from_theme_at_scale(theme, 1.0)
    }

    /// Existing tokens already use the supplied theme's device pixels. Only
    /// fallback geometry needs scaling when a frame override imports a palette
    /// that has no modern shell tokens of its own.
    pub fn from_theme_at_scale(theme: &Theme, scale: f32) -> Self {
        if let Some(chrome) = theme.chrome {
            return chrome.normalized();
        }
        Self {
            instrument: InstrumentChrome::default(),
            shadow: Shadow::default(),
            frame: FrameMetrics::default(),
            overview: OverviewMetrics::default(),
            background: theme.terminal.bg,
            panel: theme.terminal.bg,
            surface: theme.terminal.bg,
            raised: theme.terminal.ansi[8],
            text: theme.terminal.fg,
            muted: theme.terminal.ansi[7],
            line: theme.terminal.ansi[8],
            accent: theme.terminal.cursor,
            accent_text: theme.terminal.bg,
            selection: theme.terminal.ansi[8],
            success: theme.terminal.ansi[2],
            danger: theme.terminal.ansi[1],
        }
        .scaled(scale)
    }
}

fn c(hex: u32) -> Color {
    Color::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const CHOICES: [(&str, &str); 3] = [
    ("obsidian", "Obsidian"),
    ("washi", "Washi"),
    ("relay", "Relay"),
];

pub fn theme(id: &str, appearance: Appearance) -> Option<Theme> {
    let (name, radius, title, edge, family, _native_light) = match id {
        "obsidian" => ("Obsidian", 3, 34, 2, "IBM Plex Mono", false),
        "washi" => ("Washi", 0, 32, 0, "IBM Plex Sans", true),
        "relay" => ("Relay", 9, 36, 0, "Space Grotesk", false),
        _ => return None,
    };
    let light = appearance == Appearance::Light;
    // These are the exact reference palettes. Companion appearances preserve
    // hue identity and geometry, with readable text on their own surfaces.
    let colors = match (id, light) {
        ("obsidian", false) => [
            0x161917, 0x202421, 0x191d1a, 0x2b302b, 0xe0e3d7, 0xa4ad9f, 0x41493e, 0xe3b566,
            0x171b17, 0x38352a, 0xafc995, 0xef9c91,
        ],
        ("washi", true) => [
            0xe5e0d3, 0xfffcf0, 0xf5f1e5, 0xe9e4d8, 0x34372e, 0x65685c, 0xaaa99b, 0xb6412e,
            0xfffaf0, 0xeeded1, 0x506737, 0xa12d29,
        ],
        ("relay", false) => [
            0x101422, 0x1c2235, 0x151b2c, 0x282f49, 0xd4dcf3, 0xa0accb, 0x414e71, 0x9ab3ff,
            0x141c32, 0x2b3554, 0xa2d7c8, 0xf5a6b5,
        ],
        ("obsidian", true) => [
            0xe1e3d9, 0xf5f6ef, 0xebede3, 0xdde1d4, 0x252b23, 0x5b6555, 0x959e8d, 0x8c5b13,
            0xfffcf0, 0xeee2c8, 0x426430, 0xa12f27,
        ],
        ("washi", false) => [
            0x24241e, 0x303029, 0x292922, 0x3b3c31, 0xf4efdf, 0xb7b49f, 0x636452, 0xf19277,
            0x27251e, 0x493a2e, 0xb0c68c, 0xf3a191,
        ],
        _ => [
            0xdfe5f4, 0xf4f6fd, 0xe9edf9, 0xdbe2f5, 0x242e49, 0x596785, 0x92a0c0, 0x355aaa,
            0xffffff, 0xd4dff8, 0x286758, 0xa63459,
        ],
    };
    let [background, panel, surface, raised, text, muted, line, accent, accent_text, selection, success, danger] =
        colors.map(c);
    let shadow = match id {
        "washi" => Shadow {
            x: 5,
            y: 5,
            blur: 0,
            color: Color {
                r: 0x77,
                g: 0x7d,
                b: 0x6b,
                a: 0x38,
            },
        },
        "relay" => Shadow {
            x: 0,
            y: 10,
            blur: 30,
            color: Color {
                r: 0x06,
                g: 0x09,
                b: 0x16,
                a: 0x70,
            },
        },
        _ => Shadow {
            x: 0,
            y: 10,
            blur: 30,
            color: Color {
                r: 0x08,
                g: 0x0b,
                b: 0x09,
                a: 0x80,
            },
        },
    };
    let instrument = InstrumentChrome {
        background: Some(if id == "relay" { panel } else { surface }),
        border: Some(if id == "relay" { accent } else { line }),
        accent_edge: if id == "washi" { 3 } else { 0 },
    };
    let chrome = Chrome {
        instrument,
        shadow,
        frame: FrameMetrics {
            title_height: title,
            radius,
            focus_edge: edge,
            round_focus_mark: id == "relay",
            ..FrameMetrics::default()
        },
        overview: OverviewMetrics {
            radius,
            ..OverviewMetrics::default()
        },
        background,
        panel,
        surface,
        raised,
        text,
        muted,
        line,
        accent,
        accent_text,
        selection,
        success,
        danger,
    };
    let mut theme = crate::default_theme::nextstep_classic();
    theme.id = id.into();
    theme.name = name.into();
    theme.appearance = appearance;
    theme.chrome = Some(chrome);
    theme.wallpaper = id.into();
    theme.titlebar.height = title;
    theme.titlebar.font.family = family.into();
    theme.titlebar.font.size = 11.0;
    theme.titlebar.font.weight = FontWeight::Normal;
    theme.titlebar.font.style = FontStyle::Normal;
    theme.titlebar.text_align = TextAlign::Left;
    let active = if id == "washi" {
        text
    } else if id == "relay" {
        selection
    } else {
        surface
    };
    theme.titlebar.active = Fill::Solid(active);
    theme.titlebar.inactive = Fill::Solid(raised);
    theme.titlebar.text_color_active = if id == "washi" { panel } else { text };
    theme.titlebar.text_color_inactive = muted;
    theme.titlebar.bevel.width = 0;
    theme.titlebar.button_margin = 8;
    let button = theme.titlebar.buttons[0].clone();
    theme.titlebar.buttons = [
        wm_theme_api::ButtonKind::Miniaturize,
        wm_theme_api::ButtonKind::Maximize,
        wm_theme_api::ButtonKind::Close,
    ]
    .map(|kind| crate::model::ButtonStyle {
        kind,
        size: 20,
        ..button.clone()
    })
    .to_vec();
    theme.border.color_active = if id == "washi" {
        text
    } else if id == "relay" {
        line
    } else {
        accent
    };
    theme.border.color_inactive = line;
    theme.resize_bar.height = 0;
    theme.resize_bar.bevel.width = 0;
    theme.tile.fill = Fill::Solid(raised);
    theme.tile.bevel.width = 0;
    theme.menu.title_font = theme.titlebar.font.clone();
    theme.menu.item_font = theme.titlebar.font.clone();
    theme.menu.title_bar = Fill::Solid(surface);
    theme.menu.title_text_color = muted;
    theme.menu.background = Fill::Solid(panel);
    theme.menu.text_color = text;
    theme.menu.highlight_background = Fill::Solid(selection);
    theme.menu.highlight_text_color = text;
    theme.menu.bevel.width = 0;
    theme.menu.item_height = 30;
    theme.terminal.bg = panel;
    theme.terminal.fg = text;
    theme.terminal.cursor = accent;
    theme.terminal.ansi = [
        if light { text } else { surface },
        danger,
        success,
        accent,
        c(0x7aa2f7),
        c(0xb9a3d9),
        c(0x81b6ac),
        text,
        muted,
        danger,
        success,
        accent,
        c(0x9ab3ff),
        c(0xd2bae9),
        c(0xa2d7c8),
        text,
    ];
    theme.terminal.opacity = None;
    Some(theme)
}

/// Opaque flat rounded surface, shared by shell cards, tiles and panels.
/// Only the small corner squares need distance arithmetic; the body is a
/// normal rectangle fill. No blur, full-surface masks, or transient paths.
pub fn surface(
    image: &mut Pixmap,
    rect: wm_theme_api::Rect,
    radius: u32,
    fill: Color,
    border: Color,
    line: u32,
) {
    let (x, y, w, h) = (rect.pos.x, rect.pos.y, rect.size.w, rect.size.h);
    if w == 0 || h == 0 {
        return;
    }
    crate::paint::fill_rect(image, x, y, w, h, border);
    let line = line.min(w / 2).min(h / 2);
    // A public caller can supply a rectangle beyond either i32 edge. Clip in
    // wide coordinates before converting back to the destination's pixels.
    let left = (i64::from(x) + i64::from(line)).clamp(0, i64::from(image.width()));
    let top = (i64::from(y) + i64::from(line)).clamp(0, i64::from(image.height()));
    let right =
        (i64::from(x) + i64::from(w) - i64::from(line)).clamp(left, i64::from(image.width()));
    let bottom =
        (i64::from(y) + i64::from(h) - i64::from(line)).clamp(top, i64::from(image.height()));
    crate::paint::fill_rect(
        image,
        left as i32,
        top as i32,
        (right - left) as u32,
        (bottom - top) as u32,
        fill,
    );
    finish_corners(image, rect, radius, Some((border, line)));
}

/// Apply the final outer silhouette after composing opaque child content.
pub fn clip_surface(image: &mut Pixmap, radius: u32) {
    let rect = wm_theme_api::Rect::new(
        wm_theme_api::Point::new(0, 0),
        wm_theme_api::Size::new(image.width(), image.height()),
    );
    round_corners(image, rect, radius);
}

/// Restore the curved border after composing rectangular child content, then
/// apply the outer silhouette. Straight border edges remain the caller's paint.
/// Call once on an unclipped composite; applying coverage again compounds alpha.
pub fn finish_surface(image: &mut Pixmap, radius: u32, border: Color, line: u32) {
    let rect = wm_theme_api::Rect::new(
        wm_theme_api::Point::new(0, 0),
        wm_theme_api::Size::new(image.width(), image.height()),
    );
    finish_corners(image, rect, radius, Some((border, line)));
}

/// Source-over shapes preserve the rail under rounded buttons. Clearing a
/// rectangle's corners after painting it would punch holes in that background.
pub(crate) fn rounded_fill(
    image: &mut Pixmap,
    rect: wm_theme_api::Rect,
    radius: u32,
    color: Color,
) {
    let radius = radius.min(rect.size.w / 2).min(rect.size.h / 2);
    if radius == 0 {
        crate::paint::fill_rect(
            image,
            rect.pos.x,
            rect.pos.y,
            rect.size.w,
            rect.size.h,
            color,
        );
        return;
    }
    let (x, y) = (rect.pos.x as f32, rect.pos.y as f32);
    let (right, bottom) = (x + rect.size.w as f32, y + rect.size.h as f32);
    let r = radius as f32;
    let k = r * 0.552_284_8;
    let mut path = tiny_skia::PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(right - r, y);
    path.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
    path.line_to(right, bottom - r);
    path.cubic_to(
        right,
        bottom - r + k,
        right - r + k,
        bottom,
        right - r,
        bottom,
    );
    path.line_to(x + r, bottom);
    path.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
    path.line_to(x, y + r);
    path.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    path.close();
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, 255);
    image.fill_path(
        &path.finish().unwrap(),
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
}

pub(crate) fn round_corners(image: &mut Pixmap, rect: wm_theme_api::Rect, radius: u32) {
    finish_corners(image, rect, radius, None);
}

fn finish_corners(
    image: &mut Pixmap,
    rect: wm_theme_api::Rect,
    radius: u32,
    border: Option<(Color, u32)>,
) {
    let radius = radius.min(rect.size.w / 2).min(rect.size.h / 2);
    let border = border
        .filter(|(_, line)| *line > 0)
        .map(|(color, line)| (color, radius.saturating_sub(line) as f32));
    let (left, top) = (i64::from(rect.pos.x), i64::from(rect.pos.y));
    let (right, bottom) = (left + i64::from(rect.size.w), top + i64::from(rect.size.h));
    let r = i64::from(radius);
    // Intersect each corner with the actual destination first: even a huge
    // offscreen radius costs at most the pixels that can change. Corner regions
    // are disjoint because radius is at most half either dimension.
    for (cx, cy, flip_x, flip_y) in [
        (left, top, false, false),
        (right - r, top, true, false),
        (left, bottom - r, false, true),
        (right - r, bottom - r, true, true),
    ] {
        let x0 = cx.max(0);
        let y0 = cy.max(0);
        let x1 = (cx + r).min(i64::from(image.width()));
        let y1 = (cy + r).min(i64::from(image.height()));
        for py in y0..y1 {
            let dy = if flip_y { bottom - 1 - py } else { py - top };
            let y = radius as f32 - dy as f32 - 0.5;
            for px in x0..x1 {
                let dx = if flip_x { right - 1 - px } else { px - left };
                let x = radius as f32 - dx as f32 - 0.5;
                let distance = (x * x + y * y).sqrt();
                let coverage = (radius as f32 + 0.5 - distance).clamp(0.0, 1.0);
                let index = (py as usize * image.width() as usize + px as usize) * 4;
                if let Some((color, inner_radius)) = border {
                    let ink = (distance - inner_radius + 0.5).clamp(0.0, 1.0);
                    if ink > 0.0 {
                        // Source-over keeps premultiplied child pixels valid,
                        // including transparent content underneath this ring.
                        for (value, color) in image.data_mut()[index..index + 4]
                            .iter_mut()
                            .zip([color.r, color.g, color.b, 255])
                        {
                            *value = (f32::from(color) * ink + f32::from(*value) * (1.0 - ink))
                                .round() as u8;
                        }
                    }
                }
                for v in &mut image.data_mut()[index..index + 4] {
                    *v = (*v as f32 * coverage).round() as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_outline_survives_composed_rectangular_content() {
        let mut panel = Pixmap::new(32, 24).unwrap();
        let mut composed = panel.clone();
        let rect = wm_theme_api::Rect::new(
            wm_theme_api::Point::new(0, 0),
            wm_theme_api::Size::new(32, 24),
        );
        let fill = Color::rgb(24, 32, 48);
        let border = Color::rgb(160, 192, 240);
        surface(&mut panel, rect, 9, fill, border, 1);
        surface(&mut composed, rect, 0, fill, border, 1);
        crate::paint::fill_rect(&mut composed, 1, 1, 30, 22, fill);
        finish_surface(&mut composed, 9, border, 1);
        assert_eq!(
            panel.data(),
            composed.data(),
            "composition must apply corner coverage once"
        );
        let edge = panel.pixels()[3 * 32 + 2].demultiply();
        assert!(
            edge.red() > 150 && edge.blue() > 230,
            "the curved outline must carry border ink"
        );
        assert_eq!(panel.pixels()[0].alpha(), 0);
        assert_eq!(panel.pixels()[12 * 32 + 16].red(), fill.r);
        composed.data_mut().fill(0);
        finish_surface(&mut composed, 9, border, 1);
        assert!(composed
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[..3].iter().all(|value| *value <= p[3])));
    }

    #[test]
    fn invalid_direct_chrome_scales_preserve_shadow_and_instrument_metrics() {
        let mut chrome = theme("washi", Appearance::Light).unwrap().chrome.unwrap();
        chrome.shadow = Shadow {
            x: 8,
            y: -12,
            blur: 24,
            color: chrome.text,
        };
        chrome.instrument.accent_edge = 3;
        for factor in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(chrome.scaled(factor), chrome);
            assert_eq!(chrome.shadow.scaled(factor), chrome.shadow);
        }
        assert_eq!(chrome.scaled(2.0).instrument.accent_edge, 6);
        assert_eq!(chrome.scaled(2.0).shadow.y, -24);
        assert_eq!(chrome.scaled(f32::MAX).instrument.accent_edge, 64);
        assert_eq!(chrome.scaled(f32::MAX).shadow.blur, 512);
    }
    use wm_theme_api::{Point, Rect, Size};

    #[test]
    fn serialized_themes_preserve_palette_and_bound_imported_geometry() {
        for (id, _) in CHOICES {
            for appearance in [Appearance::Dark, Appearance::Light] {
                let original = theme(id, appearance).unwrap();
                let decoded: Theme = toml::from_str(&toml::to_string(&original).unwrap()).unwrap();
                assert_eq!(decoded, original);
                let mut oversized = original;
                let chrome = oversized.chrome.as_mut().unwrap();
                chrome.frame.title_height = u16::MAX;
                chrome.frame.border = u16::MAX;
                let decoded: Theme = toml::from_str(&toml::to_string(&oversized).unwrap()).unwrap();
                assert_eq!(decoded, oversized.normalized_chrome());
            }
        }
        let legacy = crate::default_theme::nextstep_classic();
        let serialized = toml::to_string(&legacy).unwrap();
        assert!(!serialized.contains("[chrome"));
        assert_eq!(toml::from_str::<Theme>(&serialized).unwrap(), legacy);
    }

    #[test]
    fn fallback_geometry_scales_once_and_modern_zero_relief_stays_zero() {
        let legacy = crate::default_theme::nextstep_classic().scaled(2.0);
        let fallback = Chrome::from_theme_at_scale(&legacy, 2.0);
        assert_eq!(fallback.frame.title_height, 64);
        let scaled = theme("obsidian", Appearance::Dark).unwrap().scaled(2.0);
        assert_eq!(
            Chrome::from_theme_at_scale(&scaled, 2.0),
            scaled.chrome.unwrap()
        );
        assert_eq!(
            (
                scaled.tile.bevel.width,
                scaled.titlebar.bevel.width,
                scaled.resize_bar.height,
                scaled.resize_bar.bevel.width
            ),
            (0, 0, 0, 0)
        );
        for invalid in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert_eq!(scaled.scaled(invalid), scaled);
        }
    }

    #[test]
    fn clipped_corner_work_is_bounded_by_the_destination_at_coordinate_extremes() {
        let mut image = Pixmap::new(4, 4).unwrap();
        let white = Color::rgb(255, 255, 255);
        for point in [
            Point::new(i32::MAX, i32::MAX),
            Point::new(i32::MIN, i32::MIN),
            Point::new(-1, -1),
            Point::new(0, 0),
        ] {
            for size in [Size::new(4, 4), Size::new(u32::MAX, u32::MAX)] {
                surface(
                    &mut image,
                    Rect::new(point, size),
                    u32::MAX,
                    white,
                    white,
                    u32::MAX,
                );
            }
        }
        assert_eq!(image.data().len(), 64);
    }

    #[test]
    fn clipping_keeps_reference_corner_pixels_unchanged() {
        for point in [Point::new(0, 0), Point::new(-2, -3), Point::new(6, 7)] {
            for radius in [0, 1, 3, 7] {
                let rect = Rect::new(point, Size::new(13, 14));
                let mut expected = Pixmap::new(16, 16).unwrap();
                expected.fill(tiny_skia::Color::WHITE);
                let mut actual = expected.clone();
                let radius = radius.min(rect.size.w / 2).min(rect.size.h / 2);
                for dy in 0..radius {
                    for dx in 0..radius {
                        let x = radius as f32 - dx as f32 - 0.5;
                        let y = radius as f32 - dy as f32 - 0.5;
                        let coverage =
                            (radius as f32 + 0.5 - (x * x + y * y).sqrt()).clamp(0.0, 1.0);
                        for (px, py) in [
                            (dx, dy),
                            (rect.size.w - 1 - dx, dy),
                            (dx, rect.size.h - 1 - dy),
                            (rect.size.w - 1 - dx, rect.size.h - 1 - dy),
                        ] {
                            let (px, py) = (rect.pos.x + px as i32, rect.pos.y + py as i32);
                            if (0..16).contains(&px) && (0..16).contains(&py) {
                                let index = (py as usize * 16 + px as usize) * 4;
                                for v in &mut expected.data_mut()[index..index + 4] {
                                    *v = (*v as f32 * coverage).round() as u8;
                                }
                            }
                        }
                    }
                }
                round_corners(&mut actual, rect, radius);
                assert_eq!(actual, expected, "{point:?}, radius {radius}");
            }
        }
    }
}
