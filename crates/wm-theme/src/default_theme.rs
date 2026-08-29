use wm_theme_api::ButtonKind;

use crate::model::{
    Bevel, BevelStyle, BorderStyle, ButtonStyle, Color, Fill, FontSpec, FontStyle, FontWeight,
    Gradient, GradientDirection, MenuStyle, ResizeBarStyle, TerminalPalette, TextAlign, Theme,
    TileStyle, TitlebarStyle,
};

/// Diagonal tile gradient helper — every theme's tile is a diagonal
/// gradient, top-left light to bottom-right dark.
fn tile_gradient(from: Color, to: Color, bevel: Bevel) -> TileStyle {
    TileStyle {
        fill: Fill::Gradient(Gradient { direction: GradientDirection::Diagonal, from, to }),
        bevel,
    }
}

/// `(id, label)` for every built-in theme, in menu order — kept as a
/// const so menu construction doesn't have to build every full `Theme`
/// struct just to list them. `registry_matches_choices` pins this to
/// `all_themes()`.
pub const CHOICES: [(&str, &str); 8] = [
    ("nextstep-classic", "NeXTSTEP Classic"),
    ("amber-phosphor", "Amber Phosphor"),
    ("teal-blueprint", "Teal Blueprint"),
    ("graphite", "Graphite"),
    ("next-lavender", "NeXT Lavender"),
    ("jade-lacquer", "Jade Lacquer"),
    ("ivory-halftone", "Ivory Halftone"),
    ("indigo-filament", "Indigo Filament"),
];

/// Every built-in theme, same order as `CHOICES`.
pub fn all_themes() -> Vec<Theme> {
    vec![
        nextstep_classic(),
        amber_phosphor(),
        teal_blueprint(),
        graphite(),
        next_lavender(),
        jade_lacquer(),
        ivory_halftone(),
        indigo_filament(),
    ]
}

/// Looks a built-in theme up by its stable id (what theme selection
/// persists), `None` for ids from a newer/older version.
pub fn theme_by_id(id: &str) -> Option<Theme> {
    all_themes().into_iter().find(|t| t.id == id)
}

/// The flagship built-in theme: the stock chiseled chrome,
/// value-for-value rather than eyeballed from screenshots:
/// - Focused titlebar `(solid, black)` with `white` text; unfocused
///   `(solid, "rgb:aa/aa/aa")` with `black` text; resizebar the same
///   `aa` gray; frame border 1px plain `black`.
/// - Titlebar/button/resizebar relief is the double raised recipe: +80
///   light lines on top/left, -40 inner shade plus a hard black outer
///   line on bottom/right — *relative* to whatever fill is underneath,
///   which is how the same recipe reads correctly on both the black and
///   the gray bars. Painted by `paint::draw_raised2_bevel`, not from
///   this struct's absolute light/dark colors.
/// - Buttons take the full titlebar height, flush at the bar's ends
///   (margin 0), showing the titlebar's own fill through with their own
///   relief; glyphs are the stock 10x10 pixmap masks stamped in the
///   title text color. (The older NeXT-inset variant used smaller inset
///   squares — see `TitlebarStyle::button_margin`.)
/// - Font: `"Sans:bold:pixelsize=12"`, which fontconfig resolves to
///   DejaVu Sans on a stock Linux desktop — the exact face visible in
///   reference screenshots. Titlebar height 23 = that font's 15px line
///   height plus the stock 4px of titlebar padding above and below.
pub fn nextstep_classic() -> Theme {
    const FONT_FAMILY: &str = "DejaVu Sans";

    // `width` drives the painted thickness of the raised relief (and
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
        id: "nextstep-classic".to_string(),
        name: "NeXTSTEP Classic".to_string(),
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
            // The classic arrangement: Miniaturize left-anchored, Close
            // right-anchored — confirmed by reading actual screenshots
            // (a stock "Info" dialog and a themed desktop, both showing
            // miniaturize-left/close-right) rather than assumed. No
            // Maximize: the classic chrome has no maximize button at all
            // (zoom is menu/keybinding-driven) — `ButtonKind::Maximize`
            // still exists as a WM-core primitive (reachable via
            // Ctrl+Shift+double-click, see `manager.rs`), a theme is
            // just free to not expose it as a titlebar button, same as
            // this one now doesn't.
            //
            // Size and placement follow the stock rule `bsize = theight`
            // — buttons are squares filling the full titlebar height,
            // flush at its ends (margin 0), not the older NeXT-inset
            // variant's smaller inset squares.
            buttons: vec![
                ButtonStyle { kind: ButtonKind::Miniaturize, size: 23, bevel: bevel_raised },
                ButtonStyle { kind: ButtonKind::Close, size: 23, bevel: bevel_raised },
            ],
            button_margin: 0,
        },
        // The stock metrics: an 8px bar with 28px corner grips, filled
        // with the same aa-gray as the unfocused titlebar.
        resize_bar: ResizeBarStyle {
            height: 8,
            fill: Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)),
            bevel: bevel_raised,
            corner_width: 28,
        },
        // Stock behavior: the focused and unfocused frame borders are
        // both plain black — identical. There is a separate, brighter
        // border color in the classic scheme ("white"), but that marks
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
        // The stock icon-background gradient, value-for-value: diagonal,
        // "rgb:a6/a6/b6" to "rgb:51/55/61".
        tile: tile_gradient(Color::rgb(0xA6, 0xA6, 0xB6), Color::rgb(0x51, 0x55, 0x61), bevel_raised),
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
            // The stock menu palette: a solid black title bar with white
            // text, a solid "rgb:aa/aa/aa" item background with black
            // text, and the selected item inverted to black on white.
            title_bar: Fill::Solid(Color::rgb(0x00, 0x00, 0x00)),
            title_text_color: Color::rgb(0xFF, 0xFF, 0xFF),
            background: Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)),
            text_color: Color::rgb(0x00, 0x00, 0x00),
            highlight_background: Fill::Solid(Color::rgb(0xFF, 0xFF, 0xFF)),
            highlight_text_color: Color::rgb(0x00, 0x00, 0x00),
            bevel: bevel_raised,
            item_height: 20,
        },
    }
}

/// Everything that *varies* between the built-in themes. The shared
/// structure — the stock chrome geometry (23px titlebar, full-height
/// flush buttons, 8px resizebar with 28px grips, 1px border), 12px
/// bold titles — comes from `build_chrome`, so every
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
    tile: (Color, Color),
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
        tile: tile_gradient(spec.tile.0, spec.tile.1, spec.bevel),
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
        tile: (Color::rgb(0x40, 0x36, 0x24), Color::rgb(0x14, 0x11, 0x0B)),
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
        tile: (Color::rgb(0x3B, 0x6E, 0x6E), Color::rgb(0x04, 0x2B, 0x2D)),
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
        tile: (Color::rgb(0x8E, 0x8E, 0x8E), Color::rgb(0x30, 0x30, 0x30)),
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
/// the strict stock-chrome parity pass: diagonal near-black/silver
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
        tile: (Color::rgb(0xB4, 0xB4, 0xC2), Color::rgb(0x6E, 0x6E, 0x80)),
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

/// Lacquered jade with warm sand ink, composed with the jade-terrace
/// artwork. Slot 4 is given a real blue rather than the source
/// palette's jade: a scheme whose blue and green are the same color
/// reads a diff as one undifferentiated wash.
pub fn jade_lacquer() -> Theme {
    build_chrome(ChromeSpec {
        id: "jade-lacquer",
        name: "Jade Lacquer",
        wallpaper: "jade-terrace",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x0B, 0x17, 0x14)),
        inactive: Fill::Solid(Color::rgb(0x6E, 0x8A, 0x79)),
        text_active: Color::rgb(0xF7, 0xE8, 0xB2),
        text_inactive: Color::rgb(0x0E, 0x1E, 0x18),
        border: Color::rgb(0x04, 0x0B, 0x09),
        resizebar: Fill::Solid(Color::rgb(0x6E, 0x8A, 0x79)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xC4, 0xD6, 0xC2), dark: Color::rgb(0x07, 0x12, 0x0F) },
        menu_title_bg: Fill::Solid(Color::rgb(0x0B, 0x17, 0x14)),
        menu_title_text: Color::rgb(0xF7, 0xE8, 0xB2),
        menu_bg: Fill::Solid(Color::rgb(0x16, 0x28, 0x21)),
        menu_text: Color::rgb(0xC1, 0xC4, 0x97),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xD6, 0xD5, 0xBC)),
        menu_highlight_text: Color::rgb(0x0B, 0x17, 0x14),
        tile: (Color::rgb(0x5C, 0x7C, 0x6A), Color::rgb(0x14, 0x26, 0x1F)),
        terminal: TerminalPalette {
            fg: Color::rgb(0xC1, 0xC4, 0x97),
            bg: Color::rgb(0x0C, 0x15, 0x12),
            // Jade rather than the palette's brighter aqua: the aqua is
            // Teal Blueprint's LED almost exactly, and two themes that
            // glow the same color are one theme on an instrument panel.
            cursor: Color::rgb(0x6F, 0xC9, 0x8A),
            ansi: [
                Color::rgb(0x0C, 0x15, 0x12),
                Color::rgb(0xFF, 0x53, 0x45),
                Color::rgb(0x54, 0x9E, 0x6A),
                Color::rgb(0xE5, 0xC7, 0x36),
                Color::rgb(0x4E, 0x9F, 0xB8),
                Color::rgb(0xD2, 0x68, 0x9C),
                Color::rgb(0x2D, 0xD5, 0xB7),
                Color::rgb(0xC1, 0xC4, 0x97),
                Color::rgb(0x3B, 0x4F, 0x45),
                Color::rgb(0xFF, 0x7A, 0x6E),
                Color::rgb(0x63, 0xB0, 0x7A),
                Color::rgb(0xF7, 0xE8, 0xB2),
                Color::rgb(0x7C, 0xC0, 0xD6),
                Color::rgb(0xE0, 0x8A, 0xB4),
                Color::rgb(0x8C, 0xD3, 0xCB),
                Color::rgb(0xE8, 0xE6, 0xC9),
            ],
            opacity: Some(86),
        },
    })
}

/// Ink on press-cream paper — the first built-in light theme, composed
/// with the ivory-orb artwork. What "light" changes is everything the
/// bars sit *on*: the focused titlebar stays the dark bar the classic
/// chrome focuses with, because inverting that is what makes a light
/// desktop stop showing you which window has the keyboard. Two other
/// deliberate departures follow from the ground being pale rather than
/// dark: menu selection inverts to the palette's blue instead of to
/// white (white on a cream menu is not a selection, it is a smudge),
/// and the terminal's bright-white slot is the same ink as its black,
/// since on cream the most legible color is the darkest one.
pub fn ivory_halftone() -> Theme {
    build_chrome(ChromeSpec {
        id: "ivory-halftone",
        name: "Ivory Halftone",
        wallpaper: "ivory-orb",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x10, 0x0F, 0x0F)),
        inactive: Fill::Solid(Color::rgb(0xCE, 0xCD, 0xC3)),
        text_active: Color::rgb(0xFF, 0xFC, 0xF0),
        text_inactive: Color::rgb(0x40, 0x3E, 0x3C),
        border: Color::rgb(0x10, 0x0F, 0x0F),
        resizebar: Fill::Solid(Color::rgb(0xCE, 0xCD, 0xC3)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xFF, 0xFC, 0xF0), dark: Color::rgb(0x87, 0x85, 0x80) },
        menu_title_bg: Fill::Solid(Color::rgb(0x10, 0x0F, 0x0F)),
        menu_title_text: Color::rgb(0xFF, 0xFC, 0xF0),
        menu_bg: Fill::Solid(Color::rgb(0xE6, 0xE4, 0xD9)),
        menu_text: Color::rgb(0x10, 0x0F, 0x0F),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x20, 0x5E, 0xA6)),
        menu_highlight_text: Color::rgb(0xFF, 0xFC, 0xF0),
        tile: (Color::rgb(0xF2, 0xEF, 0xE4), Color::rgb(0xA9, 0xA7, 0x9C)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x10, 0x0F, 0x0F),
            bg: Color::rgb(0xFF, 0xFC, 0xF0),
            cursor: Color::rgb(0x43, 0x85, 0xBE),
            ansi: [
                Color::rgb(0x10, 0x0F, 0x0F),
                Color::rgb(0xAF, 0x30, 0x29),
                Color::rgb(0x66, 0x80, 0x0B),
                Color::rgb(0xAD, 0x83, 0x01),
                Color::rgb(0x20, 0x5E, 0xA6),
                Color::rgb(0xA0, 0x2F, 0x6F),
                Color::rgb(0x24, 0x83, 0x7B),
                Color::rgb(0x6F, 0x6E, 0x69),
                Color::rgb(0xB7, 0xB5, 0xAC),
                Color::rgb(0xD1, 0x4D, 0x41),
                Color::rgb(0x87, 0x9A, 0x39),
                Color::rgb(0xD0, 0xA2, 0x15),
                Color::rgb(0x43, 0x85, 0xBE),
                Color::rgb(0xCE, 0x5D, 0x97),
                Color::rgb(0x3A, 0xA9, 0x9F),
                Color::rgb(0x10, 0x0F, 0x0F),
            ],
            // Barely tinted: glass this pale has no contrast left to
            // spend on the wallpaper behind it.
            opacity: Some(94),
        },
    })
}

/// Lit filament traces on near-black indigo, composed with the
/// indigo-waves artwork. The mauve cursor is the theme's declared
/// accent and the same value the wallpaper's cap square is cut in, so
/// the dock's LEDs and the desktop behind them glow as one.
pub fn indigo_filament() -> Theme {
    let night = Fill::Gradient(Gradient {
        direction: GradientDirection::Diagonal,
        from: Color::rgb(0x31, 0x32, 0x44),
        to: Color::rgb(0x11, 0x11, 0x1B),
    });
    build_chrome(ChromeSpec {
        id: "indigo-filament",
        name: "Indigo Filament",
        wallpaper: "indigo-waves",
        font_family: "DejaVu Sans",
        active: night.clone(),
        inactive: Fill::Solid(Color::rgb(0x45, 0x47, 0x5A)),
        text_active: Color::rgb(0xCD, 0xD6, 0xF4),
        text_inactive: Color::rgb(0x93, 0x99, 0xB2),
        border: Color::rgb(0x0A, 0x0A, 0x12),
        resizebar: Fill::Solid(Color::rgb(0x45, 0x47, 0x5A)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0x8A, 0x90, 0xB4), dark: Color::rgb(0x0D, 0x0D, 0x16) },
        menu_title_bg: night,
        menu_title_text: Color::rgb(0xCD, 0xD6, 0xF4),
        menu_bg: Fill::Solid(Color::rgb(0x1E, 0x1E, 0x2E)),
        menu_text: Color::rgb(0xBA, 0xC2, 0xDE),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xCD, 0xD6, 0xF4)),
        menu_highlight_text: Color::rgb(0x1E, 0x1E, 0x2E),
        tile: (Color::rgb(0x58, 0x5B, 0x70), Color::rgb(0x1A, 0x1A, 0x26)),
        terminal: TerminalPalette {
            fg: Color::rgb(0xCD, 0xD6, 0xF4),
            bg: Color::rgb(0x18, 0x18, 0x25),
            cursor: Color::rgb(0xCB, 0xA6, 0xF7),
            ansi: [
                Color::rgb(0x45, 0x47, 0x5A),
                Color::rgb(0xF3, 0x8B, 0xA8),
                Color::rgb(0xA6, 0xE3, 0xA1),
                Color::rgb(0xF9, 0xE2, 0xAF),
                Color::rgb(0x89, 0xB4, 0xFA),
                Color::rgb(0xCB, 0xA6, 0xF7),
                Color::rgb(0x94, 0xE2, 0xD5),
                Color::rgb(0xBA, 0xC2, 0xDE),
                Color::rgb(0x58, 0x5B, 0x70),
                Color::rgb(0xF3, 0x8B, 0xA8),
                Color::rgb(0xA6, 0xE3, 0xA1),
                Color::rgb(0xF9, 0xE2, 0xAF),
                Color::rgb(0x89, 0xB4, 0xFA),
                Color::rgb(0xF5, 0xC2, 0xE7),
                Color::rgb(0x94, 0xE2, 0xD5),
                Color::rgb(0xA6, 0xAD, 0xC8),
            ],
            opacity: Some(84),
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
        assert_eq!(kinds, vec![ButtonKind::Miniaturize, ButtonKind::Close], "the classic chrome has no maximize button");
    }

    /// The one theme that inverts the desktop's ground. Its *focused*
    /// bar still has to be the dark one - that is what makes focus
    /// readable at a glance - so this pins the pair, not just "some
    /// part of it is light": dark bar, pale everything underneath.
    #[test]
    fn the_light_theme_darkens_focus_and_lightens_what_sits_under_it() {
        let theme = ivory_halftone();
        let lum = |c: Color| (c.r as u16 + c.g as u16 + c.b as u16) / 3;
        let solid = |fill: &Fill| match fill {
            Fill::Solid(c) => *c,
            Fill::Gradient(g) => g.from,
        };
        assert!(lum(solid(&theme.titlebar.active)) < 48, "the focused bar stays ink");
        assert!(lum(theme.titlebar.text_color_active) > 200, "on which the title is paper");
        assert!(lum(solid(&theme.titlebar.inactive)) > 180, "the unfocused bar is paper");
        assert!(lum(solid(&theme.menu.background)) > 180, "menus are paper");
        assert!(lum(theme.terminal.bg) > 200, "and so is the terminal");
        // White-on-cream is a smudge, not a selection, so the light
        // theme highlights with its accent rather than inverting.
        assert!(lum(solid(&theme.menu.highlight_background)) < 128);
    }

    /// The stock chiseled look is flat solids — black focused, aa-gray
    /// unfocused — not gradients (an earlier version of this theme used
    /// diagonal gradients, visibly wrong beside a reference desktop).
    #[test]
    fn flagship_theme_uses_the_stock_solid_fills() {
        let theme = nextstep_classic();
        assert_eq!(theme.titlebar.active, Fill::Solid(Color::rgb(0x00, 0x00, 0x00)));
        assert_eq!(theme.titlebar.inactive, Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA)));
        assert_eq!(theme.titlebar.buttons[0].size, theme.titlebar.height, "buttons fill the titlebar height");
        assert_eq!(theme.titlebar.button_margin, 0, "buttons sit flush at the bar's ends");
        assert_eq!(theme.resize_bar.height, 8, "stock resizebar height");
        assert_eq!(theme.resize_bar.corner_width, 28, "stock resizebar corner width");
    }
}
