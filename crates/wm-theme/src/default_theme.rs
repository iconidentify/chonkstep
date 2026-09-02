use wm_theme_api::ButtonKind;

use crate::model::{
    Appearance, Bevel, BevelStyle, BorderStyle, ButtonStyle, Color, Fill, FontSpec, FontStyle,
    FontWeight, Gradient, GradientDirection, MenuStyle, ResizeBarStyle, TerminalPalette, TextAlign,
    Theme, TileStyle, TitlebarStyle,
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

/// Every built-in theme in its *native* rendition, same order as
/// `CHOICES`. The native rendition is the one each theme originally
/// shipped as — dark for seven of the eight, light for Ivory Halftone —
/// which is what keeps this function (and everything built on it,
/// `theme_by_id` included) returning exactly what it returned before
/// the appearance axis existed. For a specific rendition, use
/// [`all_themes_in`] / [`theme_variant`].
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

/// Every built-in theme rendered for one appearance, same order as
/// `CHOICES`.
pub fn all_themes_in(appearance: Appearance) -> Vec<Theme> {
    CHOICES
        .iter()
        .filter_map(|(id, _)| theme_variant(id, appearance))
        .collect()
}

/// Looks a built-in theme up by its stable id (what theme selection
/// persists), `None` for ids from a newer/older version. Returns the
/// theme's **native** rendition — callers that carry a session
/// appearance should resolve through [`theme_variant`] instead; this
/// stays for the callers (the SDK's `CHONKSTEP_THEME` fallback, tests)
/// that have an id and nothing else.
pub fn theme_by_id(id: &str) -> Option<Theme> {
    all_themes().into_iter().find(|t| t.id == id)
}

/// The appearance a built-in theme originally shipped in — the
/// rendition [`theme_by_id`] answers with. Sessions with no persisted
/// or configured appearance default to this, so upgrading into the
/// appearance axis changes nothing on screen: an Ivory Halftone desk
/// stays cream, every other desk stays dark. `None` for unknown ids.
pub fn native_appearance(id: &str) -> Option<Appearance> {
    theme_by_id(id).map(|theme| theme.appearance)
}

/// Resolves one theme id in one appearance — the pair the session
/// actually dresses in. Both renditions of an id share that id, name,
/// and chrome geometry; what differs is the dress: fills, bevel tones,
/// menu palette, terminal scheme, and which rendition of the wallpaper
/// artwork the shell composes underneath.
pub fn theme_variant(id: &str, appearance: Appearance) -> Option<Theme> {
    let theme = match (id, appearance) {
        ("nextstep-classic", Appearance::Dark) => nextstep_classic(),
        ("nextstep-classic", Appearance::Light) => nextstep_classic_light(),
        ("amber-phosphor", Appearance::Dark) => amber_phosphor(),
        ("amber-phosphor", Appearance::Light) => amber_phosphor_light(),
        ("teal-blueprint", Appearance::Dark) => teal_blueprint(),
        ("teal-blueprint", Appearance::Light) => teal_blueprint_light(),
        ("graphite", Appearance::Dark) => graphite(),
        ("graphite", Appearance::Light) => graphite_light(),
        ("next-lavender", Appearance::Dark) => next_lavender(),
        ("next-lavender", Appearance::Light) => next_lavender_light(),
        ("jade-lacquer", Appearance::Dark) => jade_lacquer(),
        ("jade-lacquer", Appearance::Light) => jade_lacquer_light(),
        ("ivory-halftone", Appearance::Dark) => ivory_halftone_dark(),
        ("ivory-halftone", Appearance::Light) => ivory_halftone(),
        ("indigo-filament", Appearance::Dark) => indigo_filament(),
        ("indigo-filament", Appearance::Light) => indigo_filament_light(),
        _ => return None,
    };
    Some(theme)
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
        appearance: Appearance::Dark,
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
///
/// Crate-visible (not just this module's) so a theme derived from
/// somewhere else — `crate::omarchy` pours an Omarchy palette into
/// it — is built through the same recipe rather than a copy of it:
/// there is exactly one statement of the chrome geometry, and a
/// derived theme cannot drift from the built-ins any more than the
/// built-ins can drift from each other.
pub(crate) struct ChromeSpec {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    pub(crate) wallpaper: &'static str,
    pub(crate) font_family: &'static str,
    pub(crate) active: Fill,
    pub(crate) inactive: Fill,
    pub(crate) text_active: Color,
    pub(crate) text_inactive: Color,
    pub(crate) border: Color,
    pub(crate) resizebar: Fill,
    pub(crate) bevel: Bevel,
    pub(crate) menu_title_bg: Fill,
    pub(crate) menu_title_text: Color,
    pub(crate) menu_bg: Fill,
    pub(crate) menu_text: Color,
    pub(crate) menu_highlight_bg: Fill,
    pub(crate) menu_highlight_text: Color,
    pub(crate) terminal: TerminalPalette,
    pub(crate) tile: (Color, Color),
}

pub(crate) fn build_chrome(spec: ChromeSpec) -> Theme {
    let font = |weight| FontSpec {
        family: spec.font_family.to_string(),
        size: 12.0,
        weight,
        style: FontStyle::Normal,
    };
    Theme {
        id: spec.id,
        name: spec.name,
        appearance: spec.appearance,
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
        id: "amber-phosphor".into(),
        name: "Amber Phosphor".into(),
        appearance: Appearance::Dark,
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
        id: "teal-blueprint".into(),
        name: "Teal Blueprint".into(),
        appearance: Appearance::Dark,
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
        id: "graphite".into(),
        name: "Graphite".into(),
        appearance: Appearance::Dark,
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
        id: "next-lavender".into(),
        name: "NeXT Lavender".into(),
        appearance: Appearance::Dark,
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
        id: "jade-lacquer".into(),
        name: "Jade Lacquer".into(),
        appearance: Appearance::Dark,
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
        id: "ivory-halftone".into(),
        name: "Ivory Halftone".into(),
        appearance: Appearance::Light,
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
        id: "indigo-filament".into(),
        name: "Indigo Filament".into(),
        appearance: Appearance::Dark,
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

// ---------------------------------------------------------------------
// The light renditions (and Ivory Halftone's dark one).
//
// A light variant is not an inversion. Three rules, learned from Ivory
// Halftone (the first light theme) and applied to every rendition here:
//
// - **The focused titlebar stays ink.** A dark bar on a pale desk is
//   what makes keyboard focus readable at a glance; inverting it is
//   how light themes usually die. What flips is the ground: unfocused
//   bars, resizebars, menus, tiles and the terminal go pale.
// - **Selection highlights use the theme's accent, not white.** White
//   on a pale menu is a smudge; each light rendition names a real
//   accent for its highlight instead of inverting.
// - **The bevels are re-derived, not reused.** The chisel's absolute
//   light/dark pair (menus, widgets) must straddle the *new* pale
//   fills — a highlight that read as bright against near-black reads
//   as flat against paper, so every light rendition carries its own
//   ramp: near-white catch light, mid-tone shadow.
//
// Terminal palettes are redrawn per rendition in the same mood family
// (amber stays amber, jade stays jade) with every ANSI slot darkened
// to hold contrast on the pale ground; following Ivory Halftone's
// precedent the bright-white slot is the scheme's darkest ink, because
// on paper the most legible color is the darkest one. Glass opacity
// rises on light terminals (94-95) for the same reason Ivory's did:
// pale glass has no contrast left to spend on the wallpaper behind it.

/// NeXTSTEP Classic by daylight. The chrome barely moves — the classic
/// desktop was always a light-gray one wearing a dark focused bar —
/// so this rendition's work is where the mood actually lives: menus a
/// step brighter than the aa-gray bars (so the bars still read as
/// chrome *on* something), a paper terminal in the same restrained ink
/// set, and the lavender-grid artwork's light rendition underneath.
pub fn nextstep_classic_light() -> Theme {
    let mut theme = nextstep_classic();
    theme.appearance = Appearance::Light;
    // Menus lift from the bars' aa-gray to a brighter platter, with
    // the classic selection kept: black title bar, white highlight.
    theme.menu.background = Fill::Solid(Color::rgb(0xC8, 0xC8, 0xC8));
    theme.tile = tile_gradient(
        Color::rgb(0xC2, 0xC2, 0xD0),
        Color::rgb(0x7A, 0x7E, 0x8C),
        theme.titlebar.bevel,
    );
    theme.terminal = TerminalPalette {
        fg: Color::rgb(0x20, 0x20, 0x20),
        bg: Color::rgb(0xF2, 0xF2, 0xF2),
        cursor: Color::rgb(0x20, 0x20, 0x20),
        ansi: [
            Color::rgb(0x20, 0x20, 0x20),
            Color::rgb(0xA0, 0x30, 0x30),
            Color::rgb(0x3F, 0x7A, 0x3F),
            Color::rgb(0x99, 0x70, 0x0A),
            Color::rgb(0x3A, 0x5F, 0x92),
            Color::rgb(0x7E, 0x4E, 0x7E),
            Color::rgb(0x3D, 0x7E, 0x7E),
            Color::rgb(0x6E, 0x6E, 0x6E),
            Color::rgb(0xB4, 0xB4, 0xB4),
            Color::rgb(0xC0, 0x4A, 0x4A),
            Color::rgb(0x4E, 0x99, 0x50),
            Color::rgb(0xB8, 0x87, 0x1A),
            Color::rgb(0x4A, 0x76, 0xB8),
            Color::rgb(0x9A, 0x5F, 0x9A),
            Color::rgb(0x4A, 0x99, 0x99),
            Color::rgb(0x20, 0x20, 0x20),
        ],
        opacity: Some(94),
    };
    theme
}

/// Amber Phosphor in print: the CRT's glow becomes amber ink on warm
/// paper — the service manual the terminal's firmware was printed in.
/// The focused bar keeps the phosphor identity (near-black behind
/// amber text); everything under it is parchment, and the highlight is
/// a solid amber the pale menu can actually show.
pub fn amber_phosphor_light() -> Theme {
    build_chrome(ChromeSpec {
        id: "amber-phosphor".into(),
        name: "Amber Phosphor".into(),
        appearance: Appearance::Light,
        wallpaper: "amber-terminal",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x05, 0x04, 0x03)),
        inactive: Fill::Solid(Color::rgb(0xE4, 0xD5, 0xB0)),
        text_active: Color::rgb(0xFF, 0xB0, 0x00),
        text_inactive: Color::rgb(0x6B, 0x4E, 0x14),
        border: Color::rgb(0x2A, 0x1F, 0x08),
        resizebar: Fill::Solid(Color::rgb(0xE4, 0xD5, 0xB0)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xFF, 0xF6, 0xDC), dark: Color::rgb(0x8A, 0x6E, 0x3C) },
        menu_title_bg: Fill::Solid(Color::rgb(0x05, 0x04, 0x03)),
        menu_title_text: Color::rgb(0xFF, 0xB0, 0x00),
        menu_bg: Fill::Solid(Color::rgb(0xF1, 0xE4, 0xC3)),
        menu_text: Color::rgb(0x4A, 0x36, 0x08),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xC8, 0x78, 0x00)),
        menu_highlight_text: Color::rgb(0xFF, 0xF6, 0xDC),
        tile: (Color::rgb(0xF5, 0xE9, 0xC8), Color::rgb(0xC0, 0xA5, 0x6A)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x4A, 0x36, 0x08),
            bg: Color::rgb(0xF6, 0xEC, 0xD3),
            cursor: Color::rgb(0xB4, 0x5F, 0x06),
            ansi: [
                Color::rgb(0x4A, 0x36, 0x08),
                Color::rgb(0xB3, 0x30, 0x1F),
                Color::rgb(0x5A, 0x7A, 0x1E),
                Color::rgb(0xA8, 0x78, 0x00),
                Color::rgb(0x3E, 0x6A, 0x8A),
                Color::rgb(0x8A, 0x4A, 0x66),
                Color::rgb(0x3E, 0x7A, 0x6E),
                Color::rgb(0x8A, 0x7A, 0x5A),
                Color::rgb(0xC9, 0xB9, 0x8E),
                Color::rgb(0xD1, 0x4A, 0x36),
                Color::rgb(0x6E, 0x94, 0x30),
                Color::rgb(0xC2, 0x8C, 0x00),
                Color::rgb(0x4E, 0x86, 0xAC),
                Color::rgb(0xA8, 0x60, 0x8A),
                Color::rgb(0x4E, 0x94, 0x88),
                Color::rgb(0x4A, 0x36, 0x08),
            ],
            opacity: Some(95),
        },
    })
}

/// Teal Blueprint on actual drafting paper: the deep drafting-table
/// dark flips to the pale sheet the lines were always meant to be
/// drawn on, with the same teal doing the drawing. The focused bar
/// stays the deep-teal slab (the drawing board under the sheet).
pub fn teal_blueprint_light() -> Theme {
    build_chrome(ChromeSpec {
        id: "teal-blueprint".into(),
        name: "Teal Blueprint".into(),
        appearance: Appearance::Light,
        wallpaper: "teal-blueprint",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x05, 0x46, 0x49)),
        inactive: Fill::Solid(Color::rgb(0xD8, 0xE4, 0xDC)),
        text_active: Color::rgb(0xF2, 0xEF, 0xE1),
        text_inactive: Color::rgb(0x14, 0x34, 0x32),
        border: Color::rgb(0x0A, 0x3A, 0x3C),
        resizebar: Fill::Solid(Color::rgb(0xD8, 0xE4, 0xDC)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xF4, 0xFA, 0xF6), dark: Color::rgb(0x6E, 0x8A, 0x84) },
        menu_title_bg: Fill::Solid(Color::rgb(0x05, 0x46, 0x49)),
        menu_title_text: Color::rgb(0xF2, 0xEF, 0xE1),
        menu_bg: Fill::Solid(Color::rgb(0xE6, 0xF0, 0xEA)),
        menu_text: Color::rgb(0x0A, 0x3A, 0x3C),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x0A, 0x6E, 0x6A)),
        menu_highlight_text: Color::rgb(0xF2, 0xEF, 0xE1),
        tile: (Color::rgb(0xEA, 0xF2, 0xEC), Color::rgb(0xA8, 0xC2, 0xBA)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x0A, 0x3A, 0x3C),
            bg: Color::rgb(0xED, 0xF5, 0xEF),
            cursor: Color::rgb(0x0A, 0x8A, 0x78),
            ansi: [
                Color::rgb(0x0A, 0x3A, 0x3C),
                Color::rgb(0xB3, 0x40, 0x2E),
                Color::rgb(0x1F, 0x7A, 0x52),
                Color::rgb(0x9A, 0x74, 0x10),
                Color::rgb(0x2E, 0x6E, 0x96),
                Color::rgb(0x7A, 0x50, 0x80),
                Color::rgb(0x0A, 0x80, 0x80),
                Color::rgb(0x5E, 0x7A, 0x74),
                Color::rgb(0xA8, 0xC2, 0xBA),
                Color::rgb(0xCC, 0x5C, 0x48),
                Color::rgb(0x2A, 0x99, 0x6A),
                Color::rgb(0xB5, 0x8A, 0x1E),
                Color::rgb(0x42, 0x88, 0xB4),
                Color::rgb(0x96, 0x68, 0x9E),
                Color::rgb(0x1E, 0x9C, 0x9C),
                Color::rgb(0x0A, 0x3A, 0x3C),
            ],
            opacity: Some(94),
        },
    })
}

/// Graphite by gallery light: the same strict monochrome argument on
/// white paper — ink bar, dove-gray chrome, an inverting highlight
/// (the one theme whose accent honestly *is* black), and a paper
/// terminal in the same desaturated set.
pub fn graphite_light() -> Theme {
    build_chrome(ChromeSpec {
        id: "graphite".into(),
        name: "Graphite".into(),
        appearance: Appearance::Light,
        wallpaper: "graphite-fold",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x1A, 0x1A, 0x1A)),
        inactive: Fill::Solid(Color::rgb(0xD6, 0xD6, 0xD6)),
        text_active: Color::rgb(0xEC, 0xEC, 0xEC),
        text_inactive: Color::rgb(0x1A, 0x1A, 0x1A),
        border: Color::rgb(0x00, 0x00, 0x00),
        resizebar: Fill::Solid(Color::rgb(0xD6, 0xD6, 0xD6)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xFC, 0xFC, 0xFC), dark: Color::rgb(0x90, 0x90, 0x90) },
        menu_title_bg: Fill::Solid(Color::rgb(0x1A, 0x1A, 0x1A)),
        menu_title_text: Color::rgb(0xEC, 0xEC, 0xEC),
        menu_bg: Fill::Solid(Color::rgb(0xE8, 0xE8, 0xE8)),
        menu_text: Color::rgb(0x16, 0x16, 0x16),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x2A, 0x2A, 0x2A)),
        menu_highlight_text: Color::rgb(0xF2, 0xF2, 0xF2),
        tile: (Color::rgb(0xF0, 0xF0, 0xF0), Color::rgb(0xB0, 0xB0, 0xB0)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x26, 0x26, 0x26),
            bg: Color::rgb(0xF5, 0xF5, 0xF5),
            cursor: Color::rgb(0x00, 0x00, 0x00),
            ansi: [
                Color::rgb(0x26, 0x26, 0x26),
                Color::rgb(0xA0, 0x34, 0x34),
                Color::rgb(0x4E, 0x7A, 0x2E),
                Color::rgb(0x91, 0x70, 0x0F),
                Color::rgb(0x3A, 0x64, 0x94),
                Color::rgb(0x7C, 0x4E, 0x7C),
                Color::rgb(0x39, 0x7E, 0x7E),
                Color::rgb(0x70, 0x70, 0x70),
                Color::rgb(0xB8, 0xB8, 0xB8),
                Color::rgb(0xB8, 0x4A, 0x4A),
                Color::rgb(0x5E, 0x94, 0x40),
                Color::rgb(0xAB, 0x86, 0x18),
                Color::rgb(0x4E, 0x7E, 0xB4),
                Color::rgb(0x96, 0x60, 0x9A),
                Color::rgb(0x45, 0x99, 0x9B),
                Color::rgb(0x26, 0x26, 0x26),
            ],
            opacity: Some(95),
        },
    })
}

/// NeXT Lavender at midday: the silver gradients brighten a step, the
/// slate terminal becomes slate ink on a lavender-white sheet, and the
/// solid classic-lavender desktop lightens with it. The focused bar
/// keeps the original near-black diagonal — it is the theme's
/// signature and the light desk's focus anchor at once.
pub fn next_lavender_light() -> Theme {
    build_chrome(ChromeSpec {
        id: "next-lavender".into(),
        name: "NeXT Lavender".into(),
        appearance: Appearance::Light,
        wallpaper: "classic-lavender",
        font_family: "Nimbus Sans",
        active: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0x28, 0x28, 0x2C),
            to: Color::rgb(0x06, 0x06, 0x08),
        }),
        inactive: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0xD8, 0xD8, 0xE0),
            to: Color::rgb(0xBC, 0xBC, 0xC6),
        }),
        text_active: Color::rgb(0xFF, 0xFF, 0xFF),
        text_inactive: Color::rgb(0x28, 0x28, 0x2C),
        border: Color::rgb(0x2A, 0x2A, 0x2E),
        resizebar: Fill::Solid(Color::rgb(0xC8, 0xC8, 0xD0)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xF4, 0xF4, 0xF8), dark: Color::rgb(0x7E, 0x7E, 0x8C) },
        menu_title_bg: Fill::Gradient(Gradient {
            direction: GradientDirection::Diagonal,
            from: Color::rgb(0x28, 0x28, 0x2C),
            to: Color::rgb(0x06, 0x06, 0x08),
        }),
        menu_title_text: Color::rgb(0xFF, 0xFF, 0xFF),
        menu_bg: Fill::Solid(Color::rgb(0xDC, 0xDC, 0xE4)),
        menu_text: Color::rgb(0x10, 0x10, 0x10),
        menu_highlight_bg: Fill::Solid(Color::rgb(0xFF, 0xFF, 0xFF)),
        menu_highlight_text: Color::rgb(0x10, 0x10, 0x10),
        tile: (Color::rgb(0xD4, 0xD4, 0xE2), Color::rgb(0x9C, 0x9C, 0xB0)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x24, 0x30, 0x3F),
            bg: Color::rgb(0xEE, 0xF1, 0xF8),
            cursor: Color::rgb(0x24, 0x30, 0x3F),
            ansi: [
                Color::rgb(0x24, 0x30, 0x3F),
                Color::rgb(0xC2, 0x2B, 0x2B),
                Color::rgb(0x15, 0x80, 0x3C),
                Color::rgb(0xB4, 0x53, 0x09),
                Color::rgb(0x1D, 0x4F, 0xD8),
                Color::rgb(0xA2, 0x1C, 0xAF),
                Color::rgb(0x0E, 0x74, 0x90),
                Color::rgb(0x64, 0x74, 0x8B),
                Color::rgb(0x94, 0xA3, 0xB8),
                Color::rgb(0xDC, 0x26, 0x26),
                Color::rgb(0x16, 0xA3, 0x4A),
                Color::rgb(0xD9, 0x77, 0x06),
                Color::rgb(0x25, 0x63, 0xEB),
                Color::rgb(0xC0, 0x26, 0xD3),
                Color::rgb(0x08, 0x91, 0xB2),
                Color::rgb(0x24, 0x30, 0x3F),
            ],
            opacity: Some(94),
        },
    })
}

/// Jade Lacquer as celadon: the lacquered near-black flips to glazed
/// celadon paper with deep pine ink, and the highlight is a real jade
/// rather than the dark rendition's sand. The focused bar stays the
/// lacquer slab with its warm sand title.
pub fn jade_lacquer_light() -> Theme {
    build_chrome(ChromeSpec {
        id: "jade-lacquer".into(),
        name: "Jade Lacquer".into(),
        appearance: Appearance::Light,
        wallpaper: "jade-terrace",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x0B, 0x17, 0x14)),
        inactive: Fill::Solid(Color::rgb(0xD2, 0xDC, 0xCB)),
        text_active: Color::rgb(0xF7, 0xE8, 0xB2),
        text_inactive: Color::rgb(0x16, 0x28, 0x1E),
        border: Color::rgb(0x0E, 0x20, 0x18),
        resizebar: Fill::Solid(Color::rgb(0xD2, 0xDC, 0xCB)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xF4, 0xF8, 0xEC), dark: Color::rgb(0x7E, 0x93, 0x7C) },
        menu_title_bg: Fill::Solid(Color::rgb(0x0B, 0x17, 0x14)),
        menu_title_text: Color::rgb(0xF7, 0xE8, 0xB2),
        menu_bg: Fill::Solid(Color::rgb(0xE8, 0xEE, 0xDD)),
        menu_text: Color::rgb(0x16, 0x28, 0x1E),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x2E, 0x6E, 0x4E)),
        menu_highlight_text: Color::rgb(0xF4, 0xF0, 0xD8),
        tile: (Color::rgb(0xE6, 0xEE, 0xDA), Color::rgb(0xA2, 0xB8, 0x96)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x22, 0x33, 0x24),
            bg: Color::rgb(0xF1, 0xF4, 0xE4),
            cursor: Color::rgb(0x2E, 0x8A, 0x54),
            ansi: [
                Color::rgb(0x22, 0x33, 0x24),
                Color::rgb(0xC2, 0x3A, 0x2E),
                Color::rgb(0x2E, 0x7A, 0x44),
                Color::rgb(0xA0, 0x84, 0x14),
                Color::rgb(0x2E, 0x74, 0x9A),
                Color::rgb(0xA0, 0x44, 0x78),
                Color::rgb(0x14, 0x8A, 0x74),
                Color::rgb(0x5C, 0x70, 0x59),
                Color::rgb(0xA9, 0xBC, 0xA0),
                Color::rgb(0xDB, 0x5C, 0x50),
                Color::rgb(0x3E, 0x99, 0x58),
                Color::rgb(0xBA, 0x9C, 0x22),
                Color::rgb(0x42, 0x8C, 0xB8),
                Color::rgb(0xBC, 0x60, 0x96),
                Color::rgb(0x22, 0xA8, 0x90),
                Color::rgb(0x22, 0x33, 0x24),
            ],
            opacity: Some(94),
        },
    })
}

/// Ivory Halftone after dark: the press turns its cream sheet over to
/// ink. Same halftone argument, same blue accent, everything the light
/// rendition prints in ink now prints in cream — the palette is the
/// same press's night shift rather than a different press.
pub fn ivory_halftone_dark() -> Theme {
    build_chrome(ChromeSpec {
        id: "ivory-halftone".into(),
        name: "Ivory Halftone".into(),
        appearance: Appearance::Dark,
        wallpaper: "ivory-orb",
        font_family: "DejaVu Sans",
        active: Fill::Solid(Color::rgb(0x05, 0x04, 0x04)),
        inactive: Fill::Solid(Color::rgb(0x34, 0x33, 0x31)),
        text_active: Color::rgb(0xFF, 0xFC, 0xF0),
        text_inactive: Color::rgb(0xB7, 0xB5, 0xAC),
        border: Color::rgb(0x00, 0x00, 0x00),
        resizebar: Fill::Solid(Color::rgb(0x34, 0x33, 0x31)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0x87, 0x85, 0x80), dark: Color::rgb(0x0D, 0x0C, 0x0C) },
        menu_title_bg: Fill::Solid(Color::rgb(0x05, 0x04, 0x04)),
        menu_title_text: Color::rgb(0xFF, 0xFC, 0xF0),
        menu_bg: Fill::Solid(Color::rgb(0x1C, 0x1B, 0x1A)),
        menu_text: Color::rgb(0xCE, 0xCD, 0xC3),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x43, 0x85, 0xBE)),
        menu_highlight_text: Color::rgb(0xFF, 0xFC, 0xF0),
        tile: (Color::rgb(0x40, 0x3E, 0x3C), Color::rgb(0x16, 0x15, 0x14)),
        terminal: TerminalPalette {
            fg: Color::rgb(0xCE, 0xCD, 0xC3),
            bg: Color::rgb(0x10, 0x0F, 0x0F),
            cursor: Color::rgb(0x43, 0x85, 0xBE),
            ansi: [
                Color::rgb(0x10, 0x0F, 0x0F),
                Color::rgb(0xD1, 0x4D, 0x41),
                Color::rgb(0x87, 0x9A, 0x39),
                Color::rgb(0xD0, 0xA2, 0x15),
                Color::rgb(0x43, 0x85, 0xBE),
                Color::rgb(0xCE, 0x5D, 0x97),
                Color::rgb(0x3A, 0xA9, 0x9F),
                Color::rgb(0xCE, 0xCD, 0xC3),
                Color::rgb(0x57, 0x56, 0x53),
                Color::rgb(0xE0, 0x83, 0x7A),
                Color::rgb(0xA8, 0xBC, 0x5C),
                Color::rgb(0xE3, 0xBC, 0x42),
                Color::rgb(0x66, 0xA0, 0xD4),
                Color::rgb(0xDE, 0x85, 0xB2),
                Color::rgb(0x5F, 0xC4, 0xBA),
                Color::rgb(0xFF, 0xFC, 0xF0),
            ],
            opacity: Some(90),
        },
    })
}

/// Indigo Filament by daylight: the lit traces go out and the board
/// they were etched on turns over to its pale mask — the same indigo
/// family (this is the latte face of the palette the dark rendition's
/// waves are cut in), with the night gradient kept for the focused bar
/// and a lavender-blue accent doing the highlighting.
pub fn indigo_filament_light() -> Theme {
    let night = Fill::Gradient(Gradient {
        direction: GradientDirection::Diagonal,
        from: Color::rgb(0x31, 0x32, 0x44),
        to: Color::rgb(0x11, 0x11, 0x1B),
    });
    build_chrome(ChromeSpec {
        id: "indigo-filament".into(),
        name: "Indigo Filament".into(),
        appearance: Appearance::Light,
        wallpaper: "indigo-waves",
        font_family: "DejaVu Sans",
        active: night.clone(),
        inactive: Fill::Solid(Color::rgb(0xCC, 0xD0, 0xDA)),
        text_active: Color::rgb(0xCD, 0xD6, 0xF4),
        text_inactive: Color::rgb(0x4C, 0x4F, 0x69),
        border: Color::rgb(0x3C, 0x3F, 0x53),
        resizebar: Fill::Solid(Color::rgb(0xCC, 0xD0, 0xDA)),
        bevel: Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(0xF6, 0xF8, 0xFD), dark: Color::rgb(0x8C, 0x8F, 0xA1) },
        menu_title_bg: night,
        menu_title_text: Color::rgb(0xCD, 0xD6, 0xF4),
        menu_bg: Fill::Solid(Color::rgb(0xEF, 0xF1, 0xF5)),
        menu_text: Color::rgb(0x4C, 0x4F, 0x69),
        menu_highlight_bg: Fill::Solid(Color::rgb(0x72, 0x87, 0xFD)),
        menu_highlight_text: Color::rgb(0xEF, 0xF1, 0xF5),
        tile: (Color::rgb(0xE6, 0xE9, 0xEF), Color::rgb(0xAC, 0xB0, 0xBE)),
        terminal: TerminalPalette {
            fg: Color::rgb(0x4C, 0x4F, 0x69),
            bg: Color::rgb(0xEF, 0xF1, 0xF5),
            cursor: Color::rgb(0x88, 0x39, 0xEF),
            ansi: [
                Color::rgb(0x5C, 0x5F, 0x77),
                Color::rgb(0xD2, 0x0F, 0x39),
                Color::rgb(0x40, 0xA0, 0x2B),
                Color::rgb(0xDF, 0x8E, 0x1D),
                Color::rgb(0x1E, 0x66, 0xF5),
                Color::rgb(0x88, 0x39, 0xEF),
                Color::rgb(0x17, 0x92, 0x99),
                Color::rgb(0xAC, 0xB0, 0xBE),
                Color::rgb(0x6C, 0x6F, 0x85),
                Color::rgb(0xD2, 0x0F, 0x39),
                Color::rgb(0x40, 0xA0, 0x2B),
                Color::rgb(0xDF, 0x8E, 0x1D),
                Color::rgb(0x1E, 0x66, 0xF5),
                Color::rgb(0xEA, 0x76, 0xCB),
                Color::rgb(0x17, 0x92, 0x99),
                Color::rgb(0x5C, 0x5F, 0x77),
            ],
            opacity: Some(94),
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

    // ---- the appearance axis -------------------------------------------

    fn lum(c: Color) -> u16 {
        // Perceptual (Rec.601) luma rather than a plain mean: pure
        // amber (#FFB000) *is* a bright title on black, and a mean
        // that weighs the empty blue channel equally would deny it.
        ((c.r as u32 * 299 + c.g as u32 * 587 + c.b as u32 * 114) / 1000) as u16
    }

    fn solid(fill: &Fill) -> Color {
        match fill {
            Fill::Solid(c) => *c,
            Fill::Gradient(g) => g.from,
        }
    }

    /// Every id resolves in both appearances, and a rendition keeps the
    /// theme's identity: same id, same name, tagged with the appearance
    /// it was asked for. The axis picks a rendition; it never forks the
    /// theme.
    #[test]
    fn every_theme_resolves_in_both_appearances_without_losing_its_identity() {
        for (id, label) in CHOICES {
            for appearance in [Appearance::Light, Appearance::Dark] {
                let theme = theme_variant(id, appearance)
                    .unwrap_or_else(|| panic!("{id} must have a {} rendition", appearance.name()));
                assert_eq!(theme.id, id);
                assert_eq!(theme.name, label);
                assert_eq!(theme.appearance, appearance, "{id}");
            }
        }
        assert!(theme_variant("not-a-theme", Appearance::Light).is_none());
        assert!(theme_variant("not-a-theme", Appearance::Dark).is_none());
    }

    /// Both renditions of a theme share the flagship chrome geometry —
    /// the appearance axis restyles the dress exactly the way a theme
    /// pick does, so hit-testing and layout cannot move on a switch.
    #[test]
    fn both_renditions_share_the_flagship_chrome_geometry() {
        let flagship = nextstep_classic();
        for appearance in [Appearance::Light, Appearance::Dark] {
            for theme in all_themes_in(appearance) {
                assert_eq!(theme.titlebar.height, flagship.titlebar.height, "{}", theme.id);
                assert_eq!(theme.titlebar.button_margin, flagship.titlebar.button_margin, "{}", theme.id);
                assert_eq!(theme.resize_bar.height, flagship.resize_bar.height, "{}", theme.id);
                assert_eq!(theme.resize_bar.corner_width, flagship.resize_bar.corner_width, "{}", theme.id);
                assert_eq!(theme.border.width, flagship.border.width, "{}", theme.id);
            }
        }
    }

    /// `theme_by_id` (and `all_themes`) answer the native rendition, so
    /// nothing that predates the axis changes what it gets — and the
    /// native map is what it historically was: Ivory Halftone light,
    /// everything else dark.
    #[test]
    fn native_renditions_are_what_each_theme_originally_shipped_as() {
        for (id, _) in CHOICES {
            let expected = if id == "ivory-halftone" { Appearance::Light } else { Appearance::Dark };
            assert_eq!(native_appearance(id), Some(expected), "{id}");
            let by_id = theme_by_id(id).unwrap();
            assert_eq!(by_id.appearance, expected, "{id}");
            assert_eq!(Some(by_id), theme_variant(id, expected), "{id}: theme_by_id must be the native variant");
        }
        assert_eq!(native_appearance("not-a-theme"), None);
    }

    /// The mood is real on the surfaces that carry it. For every theme:
    /// the light rendition's terminal is paper with dark ink and the
    /// dark rendition's is the reverse; menus and unfocused bars sit on
    /// the same side of the axis as the terminal; and the focused
    /// titlebar stays ink in BOTH renditions, because a light desk that
    /// inverts its focus bar is a desk where focus stops being legible.
    #[test]
    fn every_rendition_carries_its_mood_and_keeps_focus_ink() {
        for theme in all_themes_in(Appearance::Light) {
            let id = &theme.id;
            assert!(lum(theme.terminal.bg) > 200, "{id}: light terminal is paper");
            assert!(lum(theme.terminal.fg) < 100, "{id}: on which the text is ink");
            assert!(lum(solid(&theme.menu.background)) > 180, "{id}: light menus are pale");
            assert!(lum(theme.menu.text_color) < 110, "{id}: with dark item text");
            assert!(lum(solid(&theme.titlebar.inactive)) > 150, "{id}: unfocused bars are pale");
            assert!(lum(solid(&theme.titlebar.active)) < 64, "{id}: the focused bar stays ink");
            assert!(lum(theme.titlebar.text_color_active) > 150, "{id}: with a pale title");
        }
        for theme in all_themes_in(Appearance::Dark) {
            let id = &theme.id;
            assert!(lum(theme.terminal.bg) < 100, "{id}: dark terminal is dark");
            assert!(lum(theme.terminal.fg) > 120, "{id}: with light text");
            assert!(lum(solid(&theme.titlebar.active)) < 64, "{id}: the focused bar is ink here too");
        }
    }

    /// A light rendition's ANSI table has to hold contrast on its own
    /// paper: every one of the eight *normal* slots except white (7)
    /// must be visibly darker than the terminal background it prints
    /// on. (Bright slots are allowed to be pale — slot 8 is the
    /// conventional "comment gray" — and dark renditions get the mirror
    /// check against their own ground.)
    #[test]
    fn terminal_palettes_hold_contrast_on_their_own_ground() {
        for theme in all_themes_in(Appearance::Light) {
            let paper = lum(theme.terminal.bg);
            for (slot, color) in theme.terminal.ansi.iter().enumerate().take(7) {
                assert!(
                    paper.saturating_sub(lum(*color)) > 60,
                    "{}: light ANSI slot {slot} ({:?}) too pale for its paper",
                    theme.id,
                    color
                );
            }
            assert!(paper.saturating_sub(lum(theme.terminal.fg)) > 100, "{}", theme.id);
        }
        for theme in all_themes_in(Appearance::Dark) {
            let ground = lum(theme.terminal.bg);
            for (slot, color) in theme.terminal.ansi.iter().enumerate().skip(1).take(6) {
                assert!(
                    lum(*color).saturating_sub(ground) > 40,
                    "{}: dark ANSI slot {slot} ({:?}) too dim for its ground",
                    theme.id,
                    color
                );
            }
        }
    }

    /// The two renditions of a theme are genuinely different dresses —
    /// not one palette wearing two tags — and they agree on the
    /// wallpaper artwork (the artwork itself has a rendition per
    /// appearance on the shell side, so the id must not fork).
    #[test]
    fn renditions_differ_in_dress_but_agree_on_artwork() {
        for (id, _) in CHOICES {
            let light = theme_variant(id, Appearance::Light).unwrap();
            let dark = theme_variant(id, Appearance::Dark).unwrap();
            assert_ne!(light.terminal, dark.terminal, "{id}");
            assert_ne!(light.menu.background, dark.menu.background, "{id}");
            assert_eq!(light.wallpaper, dark.wallpaper, "{id}");
        }
    }

    /// The serialized theme (`theme_toml`, what dockapps are fed)
    /// round-trips the appearance tag, and a serialization written
    /// before the axis existed still deserializes — as dark, the mood
    /// such files were authored in.
    #[test]
    fn appearance_survives_toml_and_defaults_to_dark_when_absent() {
        let light = ivory_halftone();
        let toml = toml::to_string(&light).unwrap();
        let back: Theme = toml::from_str(&toml).unwrap();
        assert_eq!(back.appearance, Appearance::Light);

        let stripped: String = toml.lines().filter(|line: &&str| !line.starts_with("appearance")).collect::<Vec<_>>().join("\n");
        let old: Theme = toml::from_str(&stripped).unwrap();
        assert_eq!(old.appearance, Appearance::Dark);
    }

    #[test]
    fn appearance_names_round_trip_and_toggle() {
        assert_eq!(Appearance::from_name("light"), Some(Appearance::Light));
        assert_eq!(Appearance::from_name(" DARK \n"), Some(Appearance::Dark));
        assert_eq!(Appearance::from_name("dusk"), None);
        assert_eq!(Appearance::from_name(""), None);
        for a in [Appearance::Light, Appearance::Dark] {
            assert_eq!(Appearance::from_name(a.name()), Some(a));
            assert_eq!(a.toggled().toggled(), a);
            assert_ne!(a.toggled(), a);
        }
        assert_eq!(Appearance::default(), Appearance::Dark);
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
