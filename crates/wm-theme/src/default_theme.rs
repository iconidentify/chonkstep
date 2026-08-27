use wm_theme_api::ButtonKind;

use crate::model::{
    Bevel, BevelStyle, BorderStyle, ButtonStyle, Color, Fill, FontSpec, FontStyle, FontWeight,
    Gradient, GradientDirection, MenuStyle, ResizeBarStyle, TerminalPalette, TextAlign, Theme,
    TitlebarStyle,
};

/// `(id, label)` for every built-in theme, in menu order — kept as a
/// const so menu construction doesn't have to build five full `Theme`
/// structs just to list them. `registry_matches_choices` pins this to
/// `all_themes()`.
pub const CHOICES: [(&str, &str); 5] = [
    ("window-maker", "Window Maker"),
    ("amber-phosphor", "Amber Phosphor"),
    ("teal-blueprint", "Teal Blueprint"),
    ("graphite", "Graphite"),
    ("next-lavender", "NeXT Lavender"),
];

/// Every built-in theme, same order as `CHOICES`.
pub fn all_themes() -> Vec<Theme> {
    vec![nextstep_classic(), amber_phosphor(), teal_blueprint(), graphite(), next_lavender()]
}

/// Looks a built-in theme up by its stable id (what theme selection
/// persists), `None` for ids from a newer/older version.
pub fn theme_by_id(id: &str) -> Option<Theme> {
    all_themes().into_iter().find(|t| t.id == id)
}

/// The flagship built-in theme: real WindowMaker's stock look,
/// value-for-value from its own source rather than eyeballed from
/// screenshots (`src/defaults.c` unless noted):
/// - Focused titlebar `(solid, black)` with `white` text; unfocused
///   `(solid, "rgb:aa/aa/aa")` with `black` text; resizebar the same
///   `aa` gray; frame border 1px plain `black`.
/// - Titlebar/button/resizebar relief is wrlib's `RBEV_RAISED2`
///   (`wrlib/misc.c`): +80 light lines on top/left, -40 inner shade
///   plus a hard black outer line on bottom/right — *relative* to
///   whatever fill is underneath, which is how the same recipe reads
///   correctly on both the black and the gray bars. Painted by
///   `paint::draw_raised2_bevel`, not from this struct's absolute
///   light/dark colors.
/// - Buttons are WindowMaker's default "new" style (`TS_NEW`,
///   `src/framewin.c`): the full titlebar height, flush at the bar's
///   ends (margin 0), showing the titlebar's own fill through with
///   their own relief; glyphs are the stock 10x10 pixmap masks
///   (`src/def_pixmaps.h`) stamped in the title text color.
/// - Font: `"Sans:bold:pixelsize=12"` (`src/wconfig.h.in`), which
///   fontconfig resolves to DejaVu Sans on a stock Linux desktop —
///   the exact face visible in reference screenshots. Titlebar height
///   23 = that font's 15px line height plus WindowMaker's
///   `TITLEBAR_EXTEND_SPACE` (4) above and below.
pub fn nextstep_classic() -> Theme {
    const FONT_FAMILY: &str = "DejaVu Sans";

    // `width` drives the painted thickness of the RAISED2 relief (and
    // scales with CHONKSTEP_SCALE); the absolute light/dark colors only
    // matter to chrome still drawn with the absolute-color `draw_bevel`
    // (menus, widgets).
    let bevel_raised = Bevel {
        style: BevelStyle::Raised,
        width: 1,
        light: Color::rgb(0xF8, 0xF8, 0xF8),
        dark: Color::rgb(0x50, 0x50, 0x50),
    };

    Theme {
        id: "window-maker".to_string(),
        name: "Window Maker".to_string(),
        wallpaper: "lavender-grid".to_string(),
        // Classic silver-on-black with a restrained ANSI set — the
        // terminal a stock 90s Unix desktop wishes it shipped with.
        terminal: TerminalPalette {
            fg: Color::rgb(0xB8, 0xB8, 0xB8),
            bg: Color::rgb(0x10, 0x10, 0x10),
            cursor: Color::rgb(0xB8, 0xB8, 0xB8),
            ansi: [
                Color::rgb(0x10, 0x10, 0x10),
                Color::rgb(0xBF, 0x40, 0x40),
                Color::rgb(0x5F, 0xAF, 0x5F),
                Color::rgb(0xCF, 0xAF, 0x5F),
                Color::rgb(0x5F, 0x87, 0xC7),
                Color::rgb(0xAF, 0x6F, 0xAF),
                Color::rgb(0x5F, 0xAF, 0xAF),
                Color::rgb(0xB8, 0xB8, 0xB8),
                Color::rgb(0x5A, 0x5A, 0x5A),
                Color::rgb(0xDF, 0x6A, 0x6A),
                Color::rgb(0x87, 0xDF, 0x87),
                Color::rgb(0xEF, 0xD7, 0x87),
                Color::rgb(0x87, 0xAF, 0xEF),
                Color::rgb(0xDF, 0x9F, 0xDF),
                Color::rgb(0x87, 0xDF, 0xDF),
                Color::rgb(0xEF, 0xEF, 0xEF),
            ],
            opacity: Some(86),
        },
        titlebar: TitlebarStyle {
            height: 23,
            active: Fill::Solid(Color::rgb(0x00, 0x00, 0x00)),
            inactive: Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)),
            font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Bold,
                style: FontStyle::Normal,
            },
            text_color_active: Color::rgb(0xFF, 0xFF, 0xFF),
            text_color_inactive: Color::rgb(0x00, 0x00, 0x00),
            text_align: TextAlign::Center,
            bevel: bevel_raised,
            // Real WindowMaker's default: Miniaturize left-anchored,
            // Close right-anchored — confirmed by reading actual
            // screenshots (windowmaker.org's own "Info" dialog and a
            // themed desktop, both showing miniaturize-left/close-right)
            // rather than assumed. No Maximize: real WindowMaker has no
            // maximize button at all (zoom is menu/keybinding-driven) —
            // `ButtonKind::Maximize` still exists as a WM-core primitive
            // (reachable via Ctrl+Shift+double-click, see `manager.rs`),
            // a theme is just free to not expose it as a titlebar button,
            // same as this one now doesn't.
            //
            // Size and placement are WindowMaker's default `TS_NEW`
            // branch in `wFrameWindowUpdateBorders` (`src/framewin.c`):
            // `bsize = theight` — buttons are squares filling the full
            // titlebar height, flush at its ends (margin 0), not the
            // older `TS_NEXT` style's smaller inset squares.
            buttons: vec![
                ButtonStyle { kind: ButtonKind::Miniaturize, size: 23, bevel: bevel_raised },
                ButtonStyle { kind: ButtonKind::Close, size: 23, bevel: bevel_raised },
            ],
            button_margin: 0,
        },
        // `RESIZEBAR_HEIGHT` 8 / `RESIZEBAR_CORNER_WIDTH` 28
        // (`src/wconfig.h.in`), `ResizebarBack` the same aa-gray as the
        // unfocused titlebar (`src/defaults.c`).
        resize_bar: ResizeBarStyle {
            height: 8,
            fill: Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)),
            bevel: bevel_raised,
            corner_width: 28,
        },
        // Real WindowMaker's own defaults.c: both "FrameBorderColor"
        // (unfocused) and "FrameFocusedBorderColor" default to plain
        // "black" — identical. There's a separate, brighter
        // "FrameSelectedBorderColor" ("white"), but that's for
        // rubber-band multi-window *selection*, a different state
        // entirely, not everyday focus/unfocus. An unfocused window's
        // border sitting adjacent to a focused one used to read as a
        // conspicuous light-gray stripe running the unfocused window's
        // full height — confirmed live, sitting right behind/beside a
        // focused window it looked like a rendering artifact rather
        // than "this other window is just unfocused."
        border: BorderStyle {
            width: 1,
            color_active: Color::rgb(0x00, 0x00, 0x00),
            color_inactive: Color::rgb(0x00, 0x00, 0x00),
        },
        menu: MenuStyle {
            title_font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Bold,
                style: FontStyle::Normal,
            },
            item_font: FontSpec {
                family: FONT_FAMILY.to_string(),
                size: 12.0,
                weight: FontWeight::Normal,
                style: FontStyle::Normal,
            },
            // Real WindowMaker's menu defaults (`src/defaults.c`):
            // `MenuTitleBack (solid, black)` with white text,
            // `MenuTextBack (solid, "rgb:aa/aa/aa")` with black text,
            // and the selected item inverted to `HighlightColor white`
            // with `HighlightTextColor black`.
            title_bar: Fill::Solid(Color::rgb(0x00, 0x00, 0x00)),
            title_text_color: Color::rgb(0xFF, 0xFF, 0xFF),
            background: Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)),
            text_color: Color::rgb(0x00, 0x00, 0x00),
            highlight_background: Fill::Solid(Color::rgb(0xFF, 0xFF, 0xFF)),
            highlight_text_color: Color::rgb(0x00, 0x00, 0x00),
            bevel: bevel_raised,
            item_height: 20,
            min_width: 140,
        },
    }
}

/// Everything that *varies* between the built-in themes. The shared
/// structure — WindowMaker's default `TS_NEW` geometry (23px titlebar,
/// full-height flush buttons, 8px resizebar with 28px grips, 1px
/// border), 12px bold titles — comes from `build_chrome`, so every
/// theme hit-tests and lays out identically and only the dress
/// changes.
struct ChromeSpec {
    id: &'static str,
    name: &'static str,
    wallpaper: &'static str,
    font_family: &'static str,
    active: Fill,
    inactive: Fill,
    text_active: Color,
    text_inactive: Color,
    border: Color,
    resizebar: Fill,
    bevel: Bevel,
    menu_title_bg: Fill,
    menu_title_text: Color,
    menu_bg: Fill,
    menu_text: Color,
    menu_highlight_bg: Fill,
    menu_highlight_text: Color,
    terminal: TerminalPalette,
}

fn build_chrome(spec: ChromeSpec) -> Theme {
    let font = |weight| FontSpec {
        family: spec.font_family.to_string(),
        size: 12.0,
        weight,
        style: FontStyle::Normal,
    };
    Theme {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        wallpaper: spec.wallpaper.to_string(),
        terminal: spec.terminal,
        titlebar: TitlebarStyle {
            height: 23,
            active: spec.active,
            inactive: spec.inactive,
            font: font(FontWeight::Bold),
            text_color_active: spec.text_active,
            text_color_inactive: spec.text_inactive,
            text_align: TextAlign::Center,
            bevel: spec.bevel,
            buttons: vec![
                ButtonStyle { kind: ButtonKind::Miniaturize, size: 23, bevel: spec.bevel },
                ButtonStyle { kind: ButtonKind::Close, size: 23, bevel: spec.bevel },
            ],
            button_margin: 0,
        },
        resize_bar: ResizeBarStyle {
            height: 8,
            fill: spec.resizebar,
            bevel: spec.bevel,
            corner_width: 28,
        },
        border: BorderStyle { width: 1, color_active: spec.border, color_inactive: spec.border },
        menu: MenuStyle {
            title_font: font(FontWeight::Bold),
            item_font: font(FontWeight::Normal),
            title_bar: spec.menu_title_bg,
            title_text_color: spec.menu_title_text,
            background: spec.menu_bg,
            text_color: spec.menu_text,
            highlight_background: spec.menu_highlight_bg,
            highlight_text_color: spec.menu_highlight_text,
            bevel: spec.bevel,
            item_height: 20,
            min_width: 140,
        },
    }
}

/// Warm CRT-phosphor amber on near-black, composed with the
/// amber-terminal artwork. The ANSI slots keep enough hue separation
/// (a steel blue, a sage cyan) to stay usable while everything reads
/// warm.
pub fn amber_phosphor() -> Theme {
    build_chrome(ChromeSpec {
        id: "amber-phosphor",
        name: "Amber Phosphor",
        wallpaper: "amber-terminal",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x05, 0x04, 0x03)),
        inactive: Fill::Solid(Color::rgb(0x2A, 0x24, 0x1A)),
        text_active: Color::rgb(0xFF, 0xB0, 0x00),
        text_inactive: Color::rgb(0xA8, 0x7E, 0x2C),
        border: Color::rgb(0x00, 0x00, 0x00),
        resizebar: Fill::Solid(Color::rgb(0x2A, 0x24, 0x1A)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xC8, 0x96, 0x3C), dark: Color::rgb(0x14, 0x10, 0x08) },
        menu_title_bg: Fill::Solid(Color::rgb(0x05, 0x04, 0x03)),
        menu_title_text: Color::rgb(0xFF, 0xB0, 0x00),
        menu_bg: Fill::Solid(Color::rgb(0x1A, 0x15, 0x0E)),
        menu_text: Color::rgb(0xD8, 0x9E, 0x3F),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xFF, 0xB0, 0x00)),
        menu_highlight_text: Color::rgb(0x14, 0x0F, 0x08),
        terminal: TerminalPalette {
            fg: Color::rgb(0xFF, 0xB0, 0x00),
            bg: Color::rgb(0x0C, 0x0B, 0x09),
            cursor: Color::rgb(0xFF, 0xB0, 0x00),
            ansi: [
                Color::rgb(0x0C, 0x0B, 0x09),
                Color::rgb(0xE5, 0x53, 0x3B),
                Color::rgb(0xA3, 0xC4, 0x40),
                Color::rgb(0xFF, 0xB0, 0x00),
                Color::rgb(0x7A, 0x93, 0xA8),
                Color::rgb(0xB0, 0x78, 0x8C),
                Color::rgb(0x86, 0xB3, 0xA2),
                Color::rgb(0xE8, 0xC8, 0x8A),
                Color::rgb(0x5A, 0x4A, 0x32),
                Color::rgb(0xFF, 0x7A, 0x5C),
                Color::rgb(0xC0, 0xE0, 0x60),
                Color::rgb(0xFF, 0xD7, 0x5F),
                Color::rgb(0x9D, 0xB8, 0xCE),
                Color::rgb(0xD4, 0x9A, 0xB2),
                Color::rgb(0xA8, 0xD4, 0xC2),
                Color::rgb(0xFF, 0xE8, 0xB8),
            ],
            opacity: Some(88),
        },
    })
}

/// Deep drafting-table teal with cream ink, composed with the
/// teal-blueprint artwork.
pub fn teal_blueprint() -> Theme {
    build_chrome(ChromeSpec {
        id: "teal-blueprint",
        name: "Teal Blueprint",
        wallpaper: "teal-blueprint",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x05, 0x46, 0x49)),
        inactive: Fill::Solid(Color::rgb(0x7E, 0x96, 0x93)),
        text_active: Color::rgb(0xF2, 0xEF, 0xE1),
        text_inactive: Color::rgb(0x1A, 0x24, 0x22),
        border: Color::rgb(0x02, 0x28, 0x2A),
        resizebar: Fill::Solid(Color::rgb(0x7E, 0x96, 0x93)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xC7, 0xDA, 0xD4), dark: Color::rgb(0x0A, 0x1F, 0x1E) },
        menu_title_bg: Fill::Solid(Color::rgb(0x05, 0x46, 0x49)),
        menu_title_text: Color::rgb(0xF2, 0xEF, 0xE1),
        menu_bg: Fill::Solid(Color::rgb(0x0A, 0x36, 0x39)),
        menu_text: Color::rgb(0xCF, 0xE0, 0xDA),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xF2, 0xEF, 0xE1)),
        menu_highlight_text: Color::rgb(0x0A, 0x36, 0x39),
        terminal: TerminalPalette {
            fg: Color::rgb(0xD7, 0xE5, 0xDC),
            bg: Color::rgb(0x04, 0x28, 0x2B),
            cursor: Color::rgb(0x8F, 0xE3, 0xD2),
            ansi: [
                Color::rgb(0x04, 0x28, 0x2B),
                Color::rgb(0xE0, 0x6C, 0x60),
                Color::rgb(0x63, 0xC5, 0xA0),
                Color::rgb(0xE5, 0xC0, 0x7B),
                Color::rgb(0x5C, 0xA7, 0xC7),
                Color::rgb(0xB4, 0x8E, 0xAD),
                Color::rgb(0x56, 0xC6, 0xC6),
                Color::rgb(0xD7, 0xE5, 0xDC),
                Color::rgb(0x3E, 0x6A, 0x66),
                Color::rgb(0xEF, 0x8B, 0x80),
                Color::rgb(0x83, 0xE0, 0xBC),
                Color::rgb(0xF2, 0xD4, 0x9B),
                Color::rgb(0x7C, 0xC3, 0xE3),
                Color::rgb(0xCB, 0xA6, 0xC3),
                Color::rgb(0x76, 0xE0, 0xE0),
                Color::rgb(0xF2, 0xEF, 0xE1),
            ],
            opacity: Some(86),
        },
    })
}

/// Strict monochrome — near-black chrome, desaturated accents —
/// composed with the graphite-fold artwork.
pub fn graphite() -> Theme {
    build_chrome(ChromeSpec {
        id: "graphite",
        name: "Graphite",
        wallpaper: "graphite-fold",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x1A, 0x1A, 0x1A)),
        inactive: Fill::Solid(Color::rgb(0x8A, 0x8A, 0x8A)),
        text_active: Color::rgb(0xEC, 0xEC, 0xEC),
        text_inactive: Color::rgb(0x1A, 0x1A, 0x1A),
        border: Color::rgb(0x00, 0x00, 0x00),
        resizebar: Fill::Solid(Color::rgb(0x8A, 0x8A, 0x8A)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xF0, 0xF0, 0xF0), dark: Color::rgb(0x30, 0x30, 0x30) },
        menu_title_bg: Fill::Solid(Color::rgb(0x1A, 0x1A, 0x1A)),
        menu_title_text: Color::rgb(0xEC, 0xEC, 0xEC),
        menu_bg: Fill::Solid(Color::rgb(0x2A, 0x2A, 0x2A)),
        menu_text: Color::rgb(0xD8, 0xD8, 0xD8),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xEC, 0xEC, 0xEC)),
        menu_highlight_text: Color::rgb(0x16, 0x16, 0x16),
        terminal: TerminalPalette {
            fg: Color::rgb(0xD4, 0xD4, 0xD4),
            bg: Color::rgb(0x16, 0x16, 0x16),
            cursor: Color::rgb(0xFF, 0xFF, 0xFF),
            ansi: [
                Color::rgb(0x16, 0x16, 0x16),
                Color::rgb(0xC7, 0x5B, 0x5B),
                Color::rgb(0x98, 0xB4, 0x75),
                Color::rgb(0xC7, 0xA9, 0x6B),
                Color::rgb(0x7A, 0x9C, 0xBF),
                Color::rgb(0xA8, 0x86, 0xA8),
                Color::rgb(0x7F, 0xB2, 0xB2),
                Color::rgb(0xD4, 0xD4, 0xD4),
                Color::rgb(0x58, 0x58, 0x58),
                Color::rgb(0xD9, 0x80, 0x80),
                Color::rgb(0xB2, 0xCF, 0x96),
                Color::rgb(0xE0, 0xC2, 0x87),
                Color::rgb(0x9C, 0xBC, 0xE0),
                Color::rgb(0xC4, 0xA6, 0xC4),
                Color::rgb(0x9C, 0xD0, 0xD0),
                Color::rgb(0xF2, 0xF2, 0xF2),
            ],
            opacity: Some(88),
        },
    })
}

/// The NeXTSTEP-sampled look this project's flagship theme wore before
/// the strict WindowMaker parity pass: diagonal near-black/silver
/// gradients, Nimbus Sans (fontconfig's Helvetica), softer chisel
/// tones — kept alive as its own theme, composed with the solid
/// classic-lavender desktop. Terminal is the slate palette the desktop
/// shipped with originally.
pub fn next_lavender() -> Theme {
    build_chrome(ChromeSpec {
        id: "next-lavender",
        name: "NeXT Lavender",
        wallpaper: "classic-lavender",
        font_family: "Nimbus Sans",
        active: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0x28, 0x28, 0x2C),
            to: Color::rgb(0x06, 0x06, 0x08),
        }),
        inactive: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0xB4, 0xB4, 0xBC),
            to: Color::rgb(0x94, 0x94, 0x9E),
        }),
        text_active: Color::rgb(0xFF, 0xFF, 0xFF),
        text_inactive: Color::rgb(0x28, 0x28, 0x2C),
        border: Color::rgb(0x08, 0x08, 0x08),
        resizebar: Fill::Solid(Color::rgb(0xA2, 0xA2, 0xAA)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xD8, 0xD8, 0xDC), dark: Color::rgb(0x30, 0x30, 0x30) },
        menu_title_bg: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0x28, 0x28, 0x2C),
            to: Color::rgb(0x06, 0x06, 0x08),
        }),
        menu_title_text: Color::rgb(0xFF, 0xFF, 0xFF),
        menu_bg: Fill::Solid(Color::rgb(0xC0, 0xC0, 0xC0)),
        menu_text: Color::rgb(0x10, 0x10, 0x10),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xF2, 0xF2, 0xF2)),
        menu_highlight_text: Color::rgb(0x10, 0x10, 0x10),
        terminal: TerminalPalette {
            fg: Color::rgb(0xE2, 0xE8, 0xF0),
            bg: Color::rgb(0x0B, 0x12, 0x20),
            cursor: Color::rgb(0xE2, 0xE8, 0xF0),
            ansi: [
                Color::rgb(0x0B, 0x12, 0x20),
                Color::rgb(0xEF, 0x44, 0x44),
                Color::rgb(0x22, 0xC5, 0x5E),
                Color::rgb(0xF5, 0x9E, 0x0B),
                Color::rgb(0x3B, 0x82, 0xF6),
                Color::rgb(0xD9, 0x46, 0xEF),
                Color::rgb(0x06, 0xB6, 0xD4),
                Color::rgb(0xE2, 0xE8, 0xF0),
                Color::rgb(0x47, 0x55, 0x69),
                Color::rgb(0xF8, 0x71, 0x71),
                Color::rgb(0x4A, 0xDE, 0x80),
                Color::rgb(0xFD, 0xE0, 0x68),
                Color::rgb(0x60, 0xA5, 0xFA),
                Color::rgb(0xF0, 0xAB, 0xFC),
                Color::rgb(0x67, 0xE8, 0xF9),
                Color::rgb(0xF8, 0xFA, 0xFC),
            ],
            opacity: Some(86),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry, the `CHOICES` menu constant, and every theme's
    /// own id must all agree — a theme unreachable from the menu (or a
    /// menu entry pointing at nothing) is exactly the drift this pins.
    #[test]
    fn registry_matches_choices() {
        let themes = all_themes();
        assert_eq!(themes.len(), CHOICES.len());
        for (theme, (id, label)) in themes.iter().zip(CHOICES) {
            assert_eq!(theme.id, id);
            assert_eq!(theme.name, label);
            assert_eq!(theme_by_id(id).map(|t| t.name), Some(label.to_string()));
        }
        assert!(theme_by_id("not-a-theme").is_none());
    }

    /// Themes restyle only the dress: every built-in must share the
    /// flagship's chrome geometry so hit-testing, button placement,
    /// and resize zones behave identically across all of them.
    #[test]
    fn all_themes_share_the_flagship_chrome_geometry() {
        let flagship = nextstep_classic();
        for theme in all_themes() {
            assert_eq!(theme.titlebar.height, flagship.titlebar.height, "{}", theme.id);
            assert_eq!(theme.titlebar.button_margin, flagship.titlebar.button_margin, "{}", theme.id);
            assert_eq!(theme.resize_bar.height, flagship.resize_bar.height, "{}", theme.id);
            assert_eq!(theme.resize_bar.corner_width, flagship.resize_bar.corner_width, "{}", theme.id);
            assert_eq!(theme.border.width, flagship.border.width, "{}", theme.id);
        }
    }

    #[test]
    fn flagship_theme_has_miniaturize_left_and_close_right_only() {
        let theme = nextstep_classic();
        let kinds: Vec<_> = theme.titlebar.buttons.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![ButtonKind::Miniaturize, ButtonKind::Close], "matches real WindowMaker: no maximize button");
    }

    /// Real WindowMaker's stock look is flat solids — `FTitleBack
    /// (solid, black)`, `UTitleBack (solid, aa)` — not gradients (an
    /// earlier version of this theme used NeXTSTEP-style diagonal
    /// gradients, visibly wrong next to a real WindowMaker desktop).
    #[test]
    fn flagship_theme_uses_windowmaker_solid_fills() {
        let theme = nextstep_classic();
        assert_eq!(theme.titlebar.active, Fill::Solid(Color::rgb(0x00, 0x00, 0x00)));
        assert_eq!(theme.titlebar.inactive, Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)));
        assert_eq!(theme.titlebar.buttons[0].size, theme.titlebar.height, "TS_NEW: buttons fill the titlebar height");
        assert_eq!(theme.titlebar.button_margin, 0, "TS_NEW: buttons sit flush at the bar's ends");
        assert_eq!(theme.resize_bar.height, 8, "RESIZEBAR_HEIGHT");
        assert_eq!(theme.resize_bar.corner_width, 28, "RESIZEBAR_CORNER_WIDTH");
    }
}
