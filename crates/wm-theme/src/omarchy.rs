//! The theme bridge to Omarchy: an Omarchy `colors.toml` palette in,
//! a chonkstep [`Theme`] out — and the reverse, a built-in theme
//! written back out as a palette Omarchy can wear.
//!
//! Omarchy describes a theme as ~30 *semantic* colours (`background`,
//! `accent`, `muted`, the ANSI hues, ...) and renders every application
//! config from templates over them. chonkstep describes a theme as a
//! fully specified `Theme` — fills, bevels, menu palette, terminal
//! palette, chrome geometry. This module is the mapping between the
//! two vocabularies, in both directions, and it is written so that
//! **chonkstep keeps its identity**: an Omarchy palette changes what
//! colour the chrome is, never what the chrome *is*. The result goes
//! through the same `build_chrome` recipe every built-in uses (23px
//! titlebar, flush full-height buttons, 8px resize bar with 28px grips,
//! 1px border, the double-raised NeXT relief), so hit-testing and
//! layout are identical to every other theme; only the dress changes.
//!
//! # Reading a palette
//!
//! [`OmarchyPalette`] deserializes any `colors.toml` Omarchy itself
//! would accept, including the two legacy forms still in the wild —
//! the short names (`bg`, `fg`, `dark_bg`, ...) and the bare ANSI table
//! (`color0`..`color15`, which is all `omarchy-theme-colors-from-alacritty`
//! generates for a theme that ships only an `alacritty.toml`). Only
//! `background`, `foreground` and `accent` are required; every other
//! key is derived when absent, by **the same cascade
//! `omarchy-theme-color` applies** (that script is the canonical
//! resolver every Omarchy consumer — templates, tmux, GNOME, the shell
//! — goes through). Mirroring it exactly rather than inventing nicer
//! derivations is the point: a theme that only names `red` gets the
//! *same* `bright_red` here that Omarchy's terminals get, so the two
//! never disagree about what a colour is. In brief:
//!
//! | missing                         | derived from                                   |
//! |---------------------------------|------------------------------------------------|
//! | `mode`                          | `theme_type`, else background luminance         |
//! | `red`..`cyan`                   | `color1`..`color6`; `magenta` also from `purple`|
//! | `bright_*` hues                 | `color9`..`color14`, else the hue mixed 20% white|
//! | `light_foreground`              | `color7`, else `foreground`                     |
//! | `bright_foreground`             | `color15`, else `foreground`                    |
//! | `lighter_background`            | `color0`, else `background`                     |
//! | `dark_foreground`               | `color8`, else `foreground`                     |
//! | `muted`                         | `color8`, else `dark_foreground`                |
//! | `selection`                     | `selection_background`, `color8`, `color0`, `background` |
//! | `dark_background`               | `background` mixed 25% black                    |
//! | `darker_background`             | `background` mixed 50% black                    |
//! | `orange`                        | `yellow`                                        |
//! | `brown`                         | `orange` mixed 50% black                        |
//!
//! One departure, for a file Omarchy's own scripts would leave holes
//! in: a hue with no ANSI slot either (`red` and `color1` both absent)
//! falls back to `foreground`, so a three-key palette still yields a
//! usable — monochrome — terminal rather than a parse error.
//!
//! # The mapping (palette → chrome)
//!
//! Designed around two rules the built-ins already obey. First, **the
//! focused titlebar stays ink in both moods** — a light desk that
//! inverts its focus bar is a desk where focus stops being legible
//! (see `default_theme`'s appearance tests). Second, text is never
//! placed on a fill by convention alone: every ink is chosen from two
//! palette candidates by WCAG contrast against the fill it lands on,
//! so a palette whose `accent` is pale gets dark highlight text and
//! one whose accent is deep gets pale text, with no per-theme tuning.
//!
//! | chrome                    | dark palette                        | light palette                       |
//! |---------------------------|-------------------------------------|-------------------------------------|
//! | focused titlebar          | `darker_background`                 | `foreground` (ink)                  |
//! | focused title text        | `bright_foreground` / `background`  | `background` / `bright_foreground`  |
//! | unfocused titlebar, resize bar | `muted`                        | `selection`                         |
//! | unfocused title text      | `foreground` / `background`         | `light_foreground` / `background`   |
//! | border                    | `darker_background`                 | `foreground`                        |
//! | menu title bar            | = focused titlebar                  | = focused titlebar                  |
//! | menu body                 | `lighter_background`                | `lighter_background`                |
//! | menu text                 | `foreground` / `background`         | `foreground` / `background`         |
//! | menu highlight            | `accent`, text by contrast          | `accent`, text by contrast          |
//! | bevel light / dark        | `dark_foreground` / `darker_background` | `background` / `dark_foreground` |
//! | dock tile gradient        | `lighter_background` → `darker_background` | `dark_background` → `muted`  |
//! | terminal                  | the 16 ANSI slots, see below        | same                                |
//! | wallpaper                 | Omarchy's `current/background`      | same                                |
//!
//! Why `muted` for the unfocused bar in the dark mood and `selection`
//! in the light one: in a dark palette `selection` is a text-selection
//! tint a few values above the background (Tokyo Night: `#292e42` on
//! `#1a1b26`) and an unfocused bar painted with it would be
//! indistinguishable from the menu body next to it, while `muted` is
//! Omarchy's own "dim chrome" tone; in a light palette `muted` is a
//! mid-grey *ink* (`#acb0be`, `#808080`) that reads as disabled when
//! used as a surface, and `selection` is the pale bar — which is
//! exactly the pairing the built-in light theme, Ivory Halftone, uses
//! (its unfocused bar `#CECDC3` is Flexoki Light's `selection`).
//!
//! The bevel colours only matter to chrome drawn with the absolute
//! `draw_bevel` (menus, widgets); titlebars use the relative relief.
//! Taking them straight from the palette's own steps (`dark_foreground`
//! as the lit edge on a dark menu, `background` as the lit edge on a
//! light one) keeps the relief in the palette's key rather than in a
//! computed grey the author never chose.
//!
//! The wallpaper is Omarchy's own. An Omarchy theme is a palette *and*
//! a set of backgrounds, and `omarchy-theme-set` (and
//! `omarchy-theme-bg-next`) point `current/background` at the one in
//! use; a follower that took the palette and left the picture behind
//! would not look like Omarchy to anyone who has seen it. The theme
//! carries the id [`WALLPAPER`], and the shell resolves it to the
//! image that link names when it paints ([`current_background_path`]),
//! re-reading it whenever the link moves — so Omarchy's background
//! keys cycle this desk's wallpaper too, with no hook on Omarchy's
//! side. When the link is missing (Omarchy installed, no background
//! set) the shell falls back to Graphite Fold, the neutral artwork
//! with no hue to argue with whichever accent the palette brings.
//!
//! # The terminal
//!
//! The 16 slots mirror Omarchy's alacritty template
//! (`default/themed/alacritty.toml.tpl`) slot for slot: normal =
//! `background`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`,
//! `foreground`; bright = `muted`, `bright_red`, `bright_green`,
//! `bright_yellow`, `bright_blue`, `bright_magenta`, `bright_cyan`,
//! `bright_foreground`; cursor = `bright_foreground`. So a chonkstep
//! terminal and an Omarchy alacritty on the same desk print identical
//! colours. Opacity is 98% — Omarchy's default window rule is
//! `0.985` active — rather than the built-ins' deeper glass, again to
//! match the terminals sitting beside it.
//!
//! # Writing a palette (theme → Omarchy)
//!
//! [`palette_from_theme`] is the reverse mapping, used by the
//! `omarchy-export-themes` binary to publish the built-ins as Omarchy
//! themes: the terminal's 16 slots go straight back to their names,
//! `accent` is the menu highlight, `selection` the unfocused bar,
//! `lighter_background` the menu body, the foreground steps come from
//! the title inks and the bevel. It is lossy in one direction only —
//! geometry has no palette counterpart — and the terminal palette
//! round-trips exactly, which the tests pin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::{Deserializer, Error as _};
use serde::Deserialize;

use crate::default_theme::{build_chrome, ChromeSpec};
use crate::model::{Appearance, Bevel, BevelStyle, Color, Fill, TerminalPalette, Theme};

/// The theme id a session dresses in when it follows Omarchy —
/// what `theme = "omarchy"` in the config, the persisted menu choice,
/// and [`Theme::id`] all say. Stable across whichever Omarchy theme
/// is current: the *identity* is "follow Omarchy", the name carries
/// which theme that is right now.
pub const ID: &str = "omarchy";

/// The wallpaper id every Omarchy-derived theme carries: not one of
/// the shell's embedded artworks but a pointer at Omarchy's own
/// current background, which the shell resolves through
/// [`current_background_path`] when it paints. See the module docs.
pub const WALLPAPER: &str = "omarchy";

/// The percentage the terminal is painted at, matching Omarchy's own
/// `0.985` active-window opacity rule.
const TERMINAL_OPACITY: u8 = 98;

/// An Omarchy palette with every semantic key resolved — the file's
/// own values where it had them, `omarchy-theme-color`'s derivations
/// where it did not (see the module docs). Deserializes from the
/// `colors.toml` text directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmarchyPalette {
    pub mode: Appearance,
    pub accent: Color,
    pub selection: Color,
    pub muted: Color,
    pub background: Color,
    pub dark_background: Color,
    pub darker_background: Color,
    pub lighter_background: Color,
    pub foreground: Color,
    pub dark_foreground: Color,
    pub light_foreground: Color,
    pub bright_foreground: Color,
    pub red: Color,
    pub yellow: Color,
    pub orange: Color,
    pub green: Color,
    pub cyan: Color,
    pub blue: Color,
    pub magenta: Color,
    pub brown: Color,
    pub bright_red: Color,
    pub bright_yellow: Color,
    pub bright_green: Color,
    pub bright_cyan: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
}

/// The raw file: every key, with non-colour values kept as text so
/// `mode` can be read and everything else (Hyprland border gradients,
/// tab colours) can be ignored rather than rejected.
struct RawPalette {
    colors: BTreeMap<String, Color>,
    mode: Option<String>,
}

impl RawPalette {
    fn from_table(table: BTreeMap<String, toml::Value>) -> Self {
        let mut colors = BTreeMap::new();
        let mut mode = None;
        for (key, value) in table {
            let toml::Value::String(text) = value else {
                tracing::warn!(key, "omarchy colors.toml: value is not a string; ignoring it");
                continue;
            };
            if key == "mode" || key == "theme_type" {
                // `mode` wins over the legacy `theme_type` spelling
                // when both are present, whatever order they came in.
                if key == "mode" || mode.is_none() {
                    mode = Some(text);
                }
                continue;
            }
            match Color::from_hex(&text) {
                Some(color) => {
                    colors.insert(key, color);
                }
                // `hyprland_active_border = "rgba(...) rgba(...) 45deg"`
                // and friends: not ours, and Omarchy's own parser
                // ignores them for colour purposes too.
                None => tracing::debug!(key, value = %text, "omarchy colors.toml: not a #rrggbb colour; skipping the key"),
            }
        }
        Self { colors, mode }
    }

    /// The first of `keys` the file defines.
    fn first(&self, keys: &[&str]) -> Option<Color> {
        keys.iter().find_map(|key| self.colors.get(*key).copied())
    }

    fn resolve(self) -> Result<OmarchyPalette, String> {
        let black = Color::rgb(0, 0, 0);
        let white = Color::rgb(0xFF, 0xFF, 0xFF);
        let required = |keys: &[&str]| self.first(keys).ok_or_else(|| format!("missing required key `{}`", keys[0]));

        // The three keys with no derivation. `background`/`foreground`
        // accept the legacy short names and the bare ANSI slots the
        // alacritty importer writes, exactly as `omarchy-theme-color`
        // does; `accent` has no fallback there either.
        let background = required(&["background", "bg", "color0"])?;
        let foreground = required(&["foreground", "fg", "color7"])?;
        let accent = required(&["accent"])?;

        // A hue named neither semantically nor by ANSI slot prints as
        // plain text — the one derivation Omarchy's resolver lacks.
        let hue = |names: &[&str]| self.first(names).unwrap_or(foreground);
        let red = hue(&["red", "color1"]);
        let green = hue(&["green", "color2"]);
        let yellow = hue(&["yellow", "color3"]);
        let blue = hue(&["blue", "color4"]);
        let magenta = hue(&["magenta", "color5", "purple"]);
        let cyan = hue(&["cyan", "color6"]);

        let light_foreground = self.first(&["light_foreground", "light_fg", "color7"]).unwrap_or(foreground);
        let bright_foreground = self.first(&["bright_foreground", "bright_fg", "color15"]).unwrap_or(foreground);
        let lighter_background = self.first(&["lighter_background", "lighter_bg", "color0"]).unwrap_or(background);
        let dark_foreground = self.first(&["dark_foreground", "dark_fg", "color8"]).unwrap_or(foreground);
        let muted = self.first(&["muted", "color8"]).unwrap_or(dark_foreground);
        let selection = self.first(&["selection", "selection_background", "color8", "color0"]).unwrap_or(background);
        let orange = self.first(&["orange"]).unwrap_or(yellow);
        let brown = self.first(&["brown"]).unwrap_or_else(|| orange.mix(black, 0.5));
        let dark_background = self.first(&["dark_background", "dark_bg"]).unwrap_or_else(|| background.mix(black, 0.25));
        let darker_background = self.first(&["darker_background", "darker_bg"]).unwrap_or_else(|| background.mix(black, 0.5));
        let bright = |names: &[&str], base: Color| self.first(names).unwrap_or_else(|| base.mix(white, 0.2));
        let bright_red = bright(&["bright_red", "color9"], red);
        let bright_green = bright(&["bright_green", "color10"], green);
        let bright_yellow = bright(&["bright_yellow", "color11"], yellow);
        let bright_blue = bright(&["bright_blue", "color12"], blue);
        let bright_magenta = bright(&["bright_magenta", "color13", "bright_purple"], magenta);
        let bright_cyan = bright(&["bright_cyan", "color14"], cyan);

        // `mode`, else `theme_type` (folded in above), else the same
        // channel-sum threshold `omarchy-theme-color` auto-detects with.
        let mode = match self.mode.as_deref().map(str::trim) {
            Some(name) => match Appearance::from_name(name) {
                Some(mode) => mode,
                None => {
                    tracing::warn!(mode = name, "omarchy colors.toml: mode is neither \"light\" nor \"dark\"; judging by the background");
                    mode_from_background(background)
                }
            },
            None => mode_from_background(background),
        };

        Ok(OmarchyPalette {
            mode,
            accent,
            selection,
            muted,
            background,
            dark_background,
            darker_background,
            lighter_background,
            foreground,
            dark_foreground,
            light_foreground,
            bright_foreground,
            red,
            yellow,
            orange,
            green,
            cyan,
            blue,
            magenta,
            brown,
            bright_red,
            bright_yellow,
            bright_green,
            bright_cyan,
            bright_blue,
            bright_magenta,
        })
    }
}

/// Omarchy's own auto-detect for a palette that names no mode: a
/// channel sum above 382 (half of 3 × 255) is light.
fn mode_from_background(background: Color) -> Appearance {
    if background.r as u32 + background.g as u32 + background.b as u32 > 382 {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

impl<'de> Deserialize<'de> for OmarchyPalette {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let table = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        RawPalette::from_table(table).resolve().map_err(D::Error::custom)
    }
}

impl OmarchyPalette {
    /// Parses `colors.toml` text. The error is TOML syntax or a missing
    /// required key, spelled for a log line.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|error| error.to_string())
    }

    /// The palette as Omarchy's full canonical key set — what
    /// `omarchy-export-themes` writes, and what [`Self::parse`] reads
    /// back to exactly the same value (every key is present, so no
    /// derivation runs on the way back).
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("mode = \"{}\"\n\n", self.mode.name()));
        let mut block = |pairs: &[(&str, Color)]| {
            for (key, color) in pairs {
                out.push_str(&format!("{key} = \"{}\"\n", color.hex()));
            }
            out.push('\n');
        };
        block(&[("accent", self.accent), ("selection", self.selection), ("muted", self.muted)]);
        block(&[
            ("background", self.background),
            ("dark_background", self.dark_background),
            ("darker_background", self.darker_background),
            ("lighter_background", self.lighter_background),
        ]);
        block(&[
            ("foreground", self.foreground),
            ("dark_foreground", self.dark_foreground),
            ("light_foreground", self.light_foreground),
            ("bright_foreground", self.bright_foreground),
        ]);
        block(&[
            ("red", self.red),
            ("yellow", self.yellow),
            ("orange", self.orange),
            ("green", self.green),
            ("cyan", self.cyan),
            ("blue", self.blue),
            ("magenta", self.magenta),
            ("brown", self.brown),
        ]);
        block(&[
            ("bright_red", self.bright_red),
            ("bright_yellow", self.bright_yellow),
            ("bright_green", self.bright_green),
            ("bright_cyan", self.bright_cyan),
            ("bright_blue", self.bright_blue),
            ("bright_magenta", self.bright_magenta),
        ]);
        out.trim_end().to_string() + "\n"
    }

    /// The terminal scheme this palette prescribes, slot for slot as
    /// Omarchy's alacritty template lays it out (module docs).
    pub fn terminal(&self) -> TerminalPalette {
        TerminalPalette {
            fg: self.foreground,
            bg: self.background,
            cursor: self.bright_foreground,
            ansi: [
                self.background,
                self.red,
                self.green,
                self.yellow,
                self.blue,
                self.magenta,
                self.cyan,
                self.foreground,
                self.muted,
                self.bright_red,
                self.bright_green,
                self.bright_yellow,
                self.bright_blue,
                self.bright_magenta,
                self.bright_cyan,
                self.bright_foreground,
            ],
            opacity: Some(TERMINAL_OPACITY),
        }
    }
}

/// Of two candidate inks, the one with more WCAG contrast against
/// `fill`. The first is the palette's *intended* ink for that surface
/// and wins ties, so a well-formed palette is used as written and the
/// swap only fires when the intended ink would be unreadable.
fn ink_on(fill: Color, intended: Color, otherwise: Color) -> Color {
    if otherwise.contrast_ratio(fill) > intended.contrast_ratio(fill) {
        otherwise
    } else {
        intended
    }
}

/// Builds the chonkstep theme an Omarchy palette prescribes — the
/// mapping the module docs lay out, through the built-ins' own
/// `build_chrome`. `name` is the Omarchy theme's display name
/// ("Tokyo Night"); it lands in [`Theme::name`] as
/// `Omarchy (Tokyo Night)` while [`Theme::id`] stays [`ID`].
pub fn theme_from_palette(palette: &OmarchyPalette, name: &str) -> Theme {
    let p = palette;
    let light = p.mode == Appearance::Light;

    let active = if light { p.foreground } else { p.darker_background };
    let text_active = if light {
        ink_on(active, p.background, p.bright_foreground)
    } else {
        ink_on(active, p.bright_foreground, p.background)
    };
    let inactive = if light { p.selection } else { p.muted };
    let text_inactive = if light {
        ink_on(inactive, p.light_foreground, p.background)
    } else {
        ink_on(inactive, p.foreground, p.background)
    };
    let border = if light { p.foreground } else { p.darker_background };
    let bevel = Bevel {
        style: BevelStyle::Raised,
        width: 1,
        light: if light { p.background } else { p.dark_foreground },
        dark: if light { p.dark_foreground } else { p.darker_background },
    };
    let menu_bg = p.lighter_background;
    let menu_text = ink_on(menu_bg, p.foreground, p.background);
    let menu_highlight_text = ink_on(p.accent, p.background, p.foreground);
    let tile = if light {
        (p.dark_background, p.muted)
    } else {
        (p.lighter_background, p.darker_background)
    };

    build_chrome(ChromeSpec {
        id: ID.to_string(),
        name: display_name(name),
        appearance: p.mode,
        wallpaper: WALLPAPER,
        font_family: "DejaVu Sans",
        active: Fill::Solid(active),
        inactive: Fill::Solid(inactive),
        text_active,
        text_inactive,
        border,
        resizebar: Fill::Solid(inactive),
        bevel,
        menu_title_bg: Fill::Solid(active),
        menu_title_text: text_active,
        menu_bg: Fill::Solid(menu_bg),
        menu_text,
        menu_highlight_bg: Fill::Solid(p.accent),
        menu_highlight_text,
        terminal: p.terminal(),
        tile,
    })
}

/// `Omarchy (Tokyo Night)` — the label the Themes submenu and
/// [`Theme::name`] carry while following. A blank name (no
/// `theme.name` file) is just `Omarchy`.
pub fn display_name(theme_name: &str) -> String {
    let name = theme_name.trim();
    if name.is_empty() {
        "Omarchy".to_string()
    } else {
        format!("Omarchy ({name})")
    }
}

/// Omarchy's own display spelling of a theme directory name
/// (`omarchy-theme-list`): `tokyo-night` → `Tokyo Night`.
pub fn title_case(theme_dir_name: &str) -> String {
    theme_dir_name
        .trim()
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- the reverse direction ----------------------------------------------

/// The Omarchy palette a chonkstep theme prescribes — the reverse of
/// [`theme_from_palette`], for publishing the built-ins as Omarchy
/// themes. The terminal palette maps back to its slots exactly, save
/// the three Omarchy's template does not let a theme choose (slot 0 is
/// `background`, slot 7 is `foreground`, the cursor is
/// `bright_foreground`); the chrome maps to Omarchy's surface and ink
/// steps as the module docs describe:
///
/// | Omarchy key            | from the theme                                   |
/// |------------------------|--------------------------------------------------|
/// | `mode`                 | `appearance`                                     |
/// | `background`, `foreground` | terminal `bg`, `fg`                          |
/// | `accent`               | menu highlight fill                              |
/// | `selection`            | unfocused titlebar fill                          |
/// | `muted`                | ANSI slot 8                                      |
/// | `lighter_background`   | menu body fill                                   |
/// | `dark_background`, `darker_background` | tile gradient's light end; focused titlebar (dark) or bevel dark (light) |
/// | `dark_foreground`      | bevel dark (dark) / bevel dark (light)           |
/// | `light_foreground`     | unfocused title text                             |
/// | `bright_foreground`    | ANSI slot 15                                     |
/// | hues, `bright_*`       | ANSI slots 1–6, 9–14                             |
/// | `orange`, `brown`      | derived the way Omarchy derives them (no chrome counterpart) |
pub fn palette_from_theme(theme: &Theme) -> OmarchyPalette {
    let solid = |fill: &Fill| match fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => g.from,
    };
    let t = &theme.terminal;
    let light = theme.appearance == Appearance::Light;
    let tile_light = match &theme.tile.fill {
        Fill::Gradient(g) => g.from,
        Fill::Solid(c) => *c,
    };
    let orange = t.ansi[3];
    OmarchyPalette {
        mode: theme.appearance,
        accent: solid(&theme.menu.highlight_background),
        selection: solid(&theme.titlebar.inactive),
        muted: t.ansi[8],
        background: t.bg,
        // A light theme's darker steps are its bevel shadow; a dark
        // theme's darkest step is its ink-black focus bar.
        dark_background: if light { tile_light } else { tile_light.mix(t.bg, 0.5) },
        darker_background: if light { theme.titlebar.bevel.dark } else { solid(&theme.titlebar.active) },
        lighter_background: solid(&theme.menu.background),
        foreground: t.fg,
        dark_foreground: if light { theme.titlebar.bevel.dark } else { theme.titlebar.bevel.light },
        light_foreground: theme.titlebar.text_color_inactive,
        bright_foreground: t.ansi[15],
        red: t.ansi[1],
        green: t.ansi[2],
        yellow: t.ansi[3],
        blue: t.ansi[4],
        magenta: t.ansi[5],
        cyan: t.ansi[6],
        orange,
        brown: orange.mix(Color::rgb(0, 0, 0), 0.5),
        bright_red: t.ansi[9],
        bright_green: t.ansi[10],
        bright_yellow: t.ansi[11],
        bright_blue: t.ansi[12],
        bright_magenta: t.ansi[13],
        bright_cyan: t.ansi[14],
    }
}

// ---- Omarchy's current theme on this machine ------------------------------

/// Where `omarchy-theme-set` keeps the *current* theme — `theme/` (a
/// copy of the theme directory, swapped atomically), `theme.name` (the
/// directory name it came from) and the `background` link. State, not
/// config: this is Omarchy's own contract, read here and written by
/// nobody but Omarchy.
///
/// Omarchy spells the path `$HOME/.local/state/omarchy/current` and
/// never consults `XDG_STATE_HOME`, so that is the answer here too —
/// with one exception, for the same reason `omarchy_menu` honours
/// `XDG_CONFIG_HOME`: when `$XDG_STATE_HOME/omarchy` already exists,
/// it is preferred, which is how an isolated test session points the
/// desk at a palette of its own rather than the developer's. A user
/// who merely *sets* `XDG_STATE_HOME` has no such directory, and
/// follows the theme Omarchy actually writes.
pub fn current_dir() -> Option<PathBuf> {
    current_dir_in(std::env::var_os("XDG_STATE_HOME").map(PathBuf::from), std::env::var_os("HOME").map(PathBuf::from))
}

/// The pure half of [`current_dir`].
fn current_dir_in(xdg_state_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(isolated) = xdg_state_home.map(|root| root.join("omarchy")).filter(|dir| dir.is_dir()) {
        return Some(isolated.join("current"));
    }
    home.map(|home| home.join(".local/state/omarchy/current"))
}

/// The current theme's `colors.toml`, whether or not it exists yet.
pub fn current_colors_path() -> Option<PathBuf> {
    current_dir().map(|dir| dir.join("theme/colors.toml"))
}

/// `current/background`: the symlink `omarchy-theme-set` and
/// `omarchy-theme-bg-set` point at the background image in use (a
/// `.webp`, `.jpg` or `.png` under the theme's `backgrounds/`, or
/// anything the user handed `omarchy theme bg set`). Whether or not it
/// exists yet — the shell reads through it and falls back when it
/// cannot.
pub fn current_background_path() -> Option<PathBuf> {
    current_dir().map(|dir| dir.join("background"))
}

/// The current Omarchy theme's directory name (`tokyo-night`), when
/// Omarchy has set one.
pub fn current_theme_name() -> Option<String> {
    let text = std::fs::read_to_string(current_dir()?.join("theme.name")).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Whether Omarchy has a current theme with a palette this bridge can
/// read — the gate for offering "Omarchy" in the Themes submenu at all.
pub fn is_available() -> bool {
    current_colors_path().is_some_and(|path| path.is_file())
}

/// Reads Omarchy's current palette and builds the theme it prescribes,
/// named after the current theme. `Err` carries a log-ready reason:
/// no state directory, no file, or a file that does not parse.
pub fn load_current() -> Result<Theme, String> {
    let dir = current_dir().ok_or("neither XDG_STATE_HOME nor HOME is set")?;
    load_from_dir(&dir)
}

/// [`load_current`] against an explicit `current` directory.
pub fn load_from_dir(current: &Path) -> Result<Theme, String> {
    let path = current.join("theme/colors.toml");
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let palette = OmarchyPalette::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let name = std::fs::read_to_string(current.join("theme.name"))
        .ok()
        .map(|text| title_case(&text))
        .unwrap_or_default();
    Ok(theme_from_palette(&palette, &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokyo Night as Omarchy ships it — the full modern key set.
    const TOKYO_NIGHT: &str = r##"
mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"

bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

    /// Catppuccin Latte — a light palette, with `orange`/`brown`.
    const CATPPUCCIN_LATTE: &str = r##"
mode = "light"

accent = "#1e66f5"
selection = "#ccd0da"
muted = "#acb0be"

background = "#eff1f5"
dark_background = "#e3e4e8"
darker_background = "#d7d8dc"
lighter_background = "#dce0e8"

foreground = "#4c4f69"
dark_foreground = "#9ca0b0"
light_foreground = "#5c5f77"
bright_foreground = "#4c4f69"

red = "#d20f39"
yellow = "#df8e1d"
orange = "#d84e2b"
green = "#40a02b"
cyan = "#179299"
blue = "#1e66f5"
magenta = "#ea76cb"
brown = "#6c2715"

bright_red = "#d20f39"
bright_yellow = "#df8e1d"
bright_green = "#40a02b"
bright_cyan = "#179299"
bright_blue = "#1e66f5"
bright_magenta = "#ea76cb"
"##;

    /// Solitude — an older theme: no `orange`/`brown`, plus the
    /// Hyprland gradient keys a strict parser would choke on.
    const SOLITUDE: &str = r##"
mode = "dark"

accent = "#798186"
selection = "#343d41"
muted = "#4b4e55"

background = "#101315"
dark_background = "#0c0e10"
darker_background = "#080a0b"
lighter_background = "#101315"

foreground = "#cacccc"
dark_foreground = "#4b4e55"
light_foreground = "#cbc2be"
bright_foreground = "#a5aeb4"

hyprland_active_border = "rgba(798186ee) rgba(caccccee)"
hyprland_inactive_border = "rgb(1e1e1e)"
active_border_color = "#a8adb0"
active_tab_background = "#798186"

red = "#565d60"
yellow = "#d9dbdc"
green = "#9fa5a9"
cyan = "#707070"
blue = "#798186"
magenta = "#aeaeae"

bright_red = "#de6145"
bright_yellow = "#c9c2b4"
bright_green = "#343d41"
bright_cyan = "#707070"
bright_blue = "#5d6367"
bright_magenta = "#9a9a9a"
"##;

    /// Exactly what `omarchy-theme-colors-from-alacritty` writes for a
    /// theme that ships only an alacritty.toml: no mode, no semantic
    /// shades, the bare ANSI table.
    const MINIMAL_FROM_ALACRITTY: &str = r##"
accent = "#5f87c7"
selection = "#5a5a5a"

background = "#101010"
foreground = "#b8b8b8"

color0 = "#101010"
color1 = "#bf4040"
color2 = "#5faf5f"
color3 = "#cfaf5f"
color4 = "#5f87c7"
color5 = "#af6faf"
color6 = "#5fafaf"
color7 = "#b8b8b8"
color8 = "#5a5a5a"
color9 = "#df6a6a"
color10 = "#87df87"
color11 = "#efd787"
color12 = "#87afef"
color13 = "#df9fdf"
color14 = "#87dfdf"
color15 = "#efefef"
"##;

    fn hex(text: &str) -> Color {
        Color::from_hex(text).unwrap()
    }

    fn solid(fill: &Fill) -> Color {
        match fill {
            Fill::Solid(c) => *c,
            Fill::Gradient(g) => g.from,
        }
    }

    #[test]
    fn the_embedded_fixtures_parse_and_keep_their_own_values() {
        let tokyo = OmarchyPalette::parse(TOKYO_NIGHT).unwrap();
        assert_eq!(tokyo.mode, Appearance::Dark);
        assert_eq!(tokyo.accent, hex("#7aa2f7"));
        assert_eq!(tokyo.brown, hex("#75493d"), "a key the file names is never re-derived");
        assert_eq!(tokyo.bright_magenta, hex("#bb9af7"));

        let latte = OmarchyPalette::parse(CATPPUCCIN_LATTE).unwrap();
        assert_eq!(latte.mode, Appearance::Light);
        assert_eq!(latte.lighter_background, hex("#dce0e8"));

        let solitude = OmarchyPalette::parse(SOLITUDE).unwrap();
        assert_eq!(solitude.orange, solitude.yellow, "orange falls back to yellow, as Omarchy's resolver does");
        assert_eq!(solitude.brown, solitude.orange.mix(Color::rgb(0, 0, 0), 0.5));
    }

    /// The alacritty-importer form: every semantic key derives from
    /// the ANSI table by `omarchy-theme-color`'s cascade.
    #[test]
    fn the_minimal_alacritty_form_derives_the_semantic_palette() {
        let p = OmarchyPalette::parse(MINIMAL_FROM_ALACRITTY).unwrap();
        assert_eq!(p.mode, Appearance::Dark, "no mode key: judged from the background");
        assert_eq!(p.red, hex("#bf4040"));
        assert_eq!(p.bright_red, hex("#df6a6a"));
        assert_eq!(p.magenta, hex("#af6faf"));
        assert_eq!(p.muted, hex("#5a5a5a"), "muted is color8");
        assert_eq!(p.dark_foreground, hex("#5a5a5a"), "so is dark_foreground");
        assert_eq!(p.bright_foreground, hex("#efefef"), "color15");
        assert_eq!(p.light_foreground, hex("#b8b8b8"), "color7");
        assert_eq!(p.lighter_background, hex("#101010"), "color0");
        assert_eq!(p.selection, hex("#5a5a5a"), "named in the file");
        assert_eq!(p.dark_background, hex("#0c0c0c"), "background 25% toward black");
        assert_eq!(p.darker_background, hex("#080808"), "background 50% toward black");
        assert_eq!(p.orange, p.yellow);
        assert_eq!(p.brown, hex("#685830"), "orange 50% toward black, rounded half-up like Omarchy's awk");
    }

    #[test]
    fn a_three_key_palette_still_yields_a_usable_monochrome_theme() {
        let p = OmarchyPalette::parse("background = \"#202020\"\nforeground = \"#d0d0d0\"\naccent = \"#ffaa00\"\n").unwrap();
        assert_eq!(p.red, p.foreground, "a hue nobody named prints as text");
        assert_eq!(p.bright_red, p.foreground.mix(Color::rgb(0xFF, 0xFF, 0xFF), 0.2));
        assert_eq!(p.muted, p.foreground);
        let theme = theme_from_palette(&p, "Plain");
        assert_eq!(theme.id, ID);
        assert_eq!(theme.name, "Omarchy (Plain)");
    }

    #[test]
    fn legacy_short_names_and_theme_type_are_accepted() {
        let p = OmarchyPalette::parse(
            "theme_type = \"light\"\nbg = \"#ffffff\"\nfg = \"#000000\"\naccent = \"#0000ff\"\ndark_bg = \"#eeeeee\"\npurple = \"#800080\"\n",
        )
        .unwrap();
        assert_eq!(p.mode, Appearance::Light);
        assert_eq!(p.background, hex("#ffffff"));
        assert_eq!(p.dark_background, hex("#eeeeee"));
        assert_eq!(p.magenta, hex("#800080"));
    }

    #[test]
    fn missing_required_keys_and_broken_toml_are_errors_not_panics() {
        assert!(OmarchyPalette::parse("foreground = \"#000000\"\naccent = \"#0000ff\"\n").unwrap_err().contains("background"));
        assert!(OmarchyPalette::parse("background = \"#000000\"\nforeground = \"#ffffff\"\n").unwrap_err().contains("accent"));
        assert!(OmarchyPalette::parse("background = \"#000000\"\nforeground = \"#ffffff\"\naccent = ").is_err());
        // A malformed colour on a required key is a missing key: it is
        // skipped like Omarchy skips it, and nothing else names it.
        assert!(OmarchyPalette::parse("background = \"black\"\nforeground = \"#ffffff\"\naccent = \"#0000ff\"\n").is_err());
    }

    #[test]
    fn light_mode_gives_a_light_appearance_and_dark_gives_dark() {
        let latte = theme_from_palette(&OmarchyPalette::parse(CATPPUCCIN_LATTE).unwrap(), "Catppuccin Latte");
        assert_eq!(latte.appearance, Appearance::Light);
        let tokyo = theme_from_palette(&OmarchyPalette::parse(TOKYO_NIGHT).unwrap(), "Tokyo Night");
        assert_eq!(tokyo.appearance, Appearance::Dark);
        // With no mode key, a pale background is judged light.
        let pale = OmarchyPalette::parse("background = \"#fafafa\"\nforeground = \"#111111\"\naccent = \"#0000ff\"\n").unwrap();
        assert_eq!(pale.mode, Appearance::Light);
    }

    /// The derived theme wears chonkstep's chrome, not Omarchy's: the
    /// flagship's geometry to the pixel.
    #[test]
    fn a_derived_theme_keeps_the_flagship_chrome_geometry() {
        let flagship = crate::default_theme::nextstep_classic();
        for (text, name) in [(TOKYO_NIGHT, "Tokyo Night"), (CATPPUCCIN_LATTE, "Catppuccin Latte"), (MINIMAL_FROM_ALACRITTY, "Minimal")] {
            let theme = theme_from_palette(&OmarchyPalette::parse(text).unwrap(), name);
            assert_eq!(theme.titlebar.height, flagship.titlebar.height, "{name}");
            assert_eq!(theme.titlebar.button_margin, flagship.titlebar.button_margin, "{name}");
            assert_eq!(theme.titlebar.buttons.len(), flagship.titlebar.buttons.len(), "{name}");
            assert_eq!(theme.resize_bar.height, flagship.resize_bar.height, "{name}");
            assert_eq!(theme.resize_bar.corner_width, flagship.resize_bar.corner_width, "{name}");
            assert_eq!(theme.border.width, flagship.border.width, "{name}");
            assert_eq!(theme.menu.item_height, flagship.menu.item_height, "{name}");
            assert_eq!(theme.titlebar.bevel.style, BevelStyle::Raised, "{name}");
        }
    }

    /// The focused bar stays ink in both moods — the rule every
    /// built-in rendition obeys — and the title on it is readable.
    #[test]
    fn the_focused_bar_is_ink_in_both_moods_and_its_title_reads() {
        let tokyo = theme_from_palette(&OmarchyPalette::parse(TOKYO_NIGHT).unwrap(), "");
        let latte = theme_from_palette(&OmarchyPalette::parse(CATPPUCCIN_LATTE).unwrap(), "");
        for theme in [&tokyo, &latte] {
            let bar = solid(&theme.titlebar.active);
            assert!(bar.relative_luminance() < 0.1, "{}: focused bar is ink ({})", theme.name, bar.hex());
            assert!(theme.titlebar.text_color_active.contrast_ratio(bar) > 4.5, "{}: title readable", theme.name);
            assert!(theme.titlebar.text_color_inactive.contrast_ratio(solid(&theme.titlebar.inactive)) > 3.0, "{}: unfocused title readable", theme.name);
            assert!(theme.menu.text_color.contrast_ratio(solid(&theme.menu.background)) > 4.5, "{}: menu text readable", theme.name);
        }
        assert_eq!(solid(&tokyo.titlebar.active), hex("#0e0e14"), "dark: darker_background");
        assert_eq!(solid(&latte.titlebar.active), hex("#4c4f69"), "light: the foreground ink");
        assert_eq!(solid(&latte.titlebar.inactive), hex("#ccd0da"), "light: selection is the pale bar");
        assert_eq!(solid(&tokyo.titlebar.inactive), hex("#414868"), "dark: muted is the dim bar");
    }

    /// The menu highlight is the accent, and its text is whichever of
    /// background/foreground actually reads on it.
    #[test]
    fn the_menu_highlight_is_the_accent_with_text_chosen_by_contrast() {
        let tokyo = theme_from_palette(&OmarchyPalette::parse(TOKYO_NIGHT).unwrap(), "");
        assert_eq!(solid(&tokyo.menu.highlight_background), hex("#7aa2f7"));
        // A pale periwinkle accent on a dark palette: the pale
        // foreground would vanish on it, so the background ink wins.
        assert_eq!(tokyo.menu.highlight_text_color, hex("#1a1b26"));

        // A deep accent on a light palette: the pale background is the
        // readable ink.
        let latte = theme_from_palette(&OmarchyPalette::parse(CATPPUCCIN_LATTE).unwrap(), "");
        assert_eq!(latte.menu.highlight_text_color, hex("#eff1f5"));

        // A mid-grey accent on a light palette: the intended ink (the
        // pale background) would sit at 3.5:1, black sits at 6:1, so the
        // rule overrides the intent.
        let grey = OmarchyPalette::parse("mode = \"light\"\nbackground = \"#ffffff\"\nforeground = \"#000000\"\naccent = \"#8a8a8a\"\n").unwrap();
        let theme = theme_from_palette(&grey, "");
        assert_eq!(theme.menu.highlight_text_color, hex("#000000"));
        for theme in [&tokyo, &latte, &theme] {
            assert!(theme.menu.highlight_text_color.contrast_ratio(solid(&theme.menu.highlight_background)) > 3.0);
        }
    }

    /// The terminal is Omarchy's alacritty template, slot for slot.
    #[test]
    fn the_terminal_mirrors_omarchys_alacritty_slot_mapping() {
        let p = OmarchyPalette::parse(TOKYO_NIGHT).unwrap();
        let t = theme_from_palette(&p, "").terminal;
        assert_eq!(t.bg, p.background);
        assert_eq!(t.fg, p.foreground);
        assert_eq!(t.cursor, p.bright_foreground);
        assert_eq!(t.ansi[0], p.background);
        assert_eq!(t.ansi[1..7], [p.red, p.green, p.yellow, p.blue, p.magenta, p.cyan]);
        assert_eq!(t.ansi[7], p.foreground);
        assert_eq!(t.ansi[8], p.muted);
        assert_eq!(t.ansi[9..15], [p.bright_red, p.bright_green, p.bright_yellow, p.bright_blue, p.bright_magenta, p.bright_cyan]);
        assert_eq!(t.ansi[15], p.bright_foreground);
        assert_eq!(t.opacity, Some(98));
    }

    /// Same pin as `default_theme`'s: the derived theme is what dockapps
    /// are fed as `theme_toml`, so it must survive TOML.
    #[test]
    fn a_derived_theme_round_trips_through_toml_like_the_built_ins() {
        let theme = theme_from_palette(&OmarchyPalette::parse(CATPPUCCIN_LATTE).unwrap(), "Catppuccin Latte");
        let text = toml::to_string(&theme).unwrap();
        let back: Theme = toml::from_str(&text).unwrap();
        assert_eq!(back, theme);
        assert_eq!(back.appearance, Appearance::Light);
        assert_eq!(back.name, "Omarchy (Catppuccin Latte)");
    }

    #[test]
    fn names_are_spelled_the_way_omarchy_spells_them() {
        assert_eq!(title_case("tokyo-night"), "Tokyo Night");
        assert_eq!(title_case("catppuccin-latte\n"), "Catppuccin Latte");
        assert_eq!(title_case("retro-82"), "Retro 82");
        assert_eq!(display_name("Tokyo Night"), "Omarchy (Tokyo Night)");
        assert_eq!(display_name("  "), "Omarchy");
    }

    /// The reverse mapping: every built-in becomes a palette that
    /// parses back (through the same strict path a user file takes) and
    /// whose terminal is the built-in's, slot for slot.
    #[test]
    fn every_built_in_exports_to_a_palette_that_parses_back_with_its_terminal_intact() {
        for theme in crate::default_theme::all_themes() {
            let palette = palette_from_theme(&theme);
            let text = palette.to_toml();
            let back = OmarchyPalette::parse(&text).unwrap_or_else(|e| panic!("{}: {e}\n{text}", theme.id));
            assert_eq!(back, palette, "{}: the full key set round-trips without derivation", theme.id);
            assert_eq!(back.mode, theme.appearance, "{}", theme.id);
            // Omarchy's template pins three things a chonkstep palette
            // can set freely: slot 0 is `background`, slot 7 is
            // `foreground`, the cursor is `bright_foreground` (and
            // opacity is a window rule). Those follow the template;
            // everything else round-trips exactly — text colour, ground,
            // and the fourteen hues.
            let terminal = back.terminal();
            let want = &theme.terminal;
            assert_eq!((terminal.fg, terminal.bg), (want.fg, want.bg), "{}", theme.id);
            assert_eq!(terminal.ansi[1..7], want.ansi[1..7], "{}: normal hues", theme.id);
            assert_eq!(terminal.ansi[8..16], want.ansi[8..16], "{}: bright slots", theme.id);
            assert_eq!(terminal.ansi[0], want.bg, "{}", theme.id);
            assert_eq!(terminal.ansi[7], want.fg, "{}", theme.id);
            assert_eq!(back.accent, solid(&theme.menu.highlight_background), "{}", theme.id);
            // Omarchy's 25 canonical colour keys, plus mode, and nothing else.
            assert_eq!(text.lines().filter(|l| l.contains('=')).count(), 26, "{}", theme.id);
        }
    }

    /// Every theme Omarchy ships on this machine parses and dresses —
    /// skipped, loudly, where Omarchy is not installed. The embedded
    /// fixtures above cover the shapes; this covers the population.
    #[test]
    #[ignore = "reads the Omarchy themes installed on this machine; run by hand or from scripts/e2e.sh"]
    fn every_installed_omarchy_theme_parses_and_dresses() {
        let root = std::env::var_os("OMARCHY_PATH")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/omarchy")))
            .map(|root| root.join("themes"));
        let Some(entries) = root.as_deref().and_then(|dir| std::fs::read_dir(dir).ok()) else {
            eprintln!("skipping: no Omarchy themes directory under $OMARCHY_PATH or ~/.local/share/omarchy");
            return;
        };
        let mut seen = 0;
        for entry in entries.flatten() {
            let path = entry.path().join("colors.toml");
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let palette = OmarchyPalette::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let name = title_case(&entry.file_name().to_string_lossy());
            let theme = theme_from_palette(&palette, &name);
            assert_eq!(theme.id, ID);
            assert!(theme.titlebar.text_color_active.contrast_ratio(solid(&theme.titlebar.active)) > 3.0, "{name}: focused title readable");
            assert!(theme.menu.highlight_text_color.contrast_ratio(solid(&theme.menu.highlight_background)) > 2.5, "{name}: highlight text readable");
            let text = toml::to_string(&theme).unwrap();
            assert_eq!(toml::from_str::<Theme>(&text).unwrap(), theme, "{name}");
            seen += 1;
        }
        assert!(seen > 0, "an Omarchy install with no themes?");
        eprintln!("{seen} installed Omarchy themes parsed and dressed");
    }

    #[test]
    fn the_current_dir_is_omarchys_own_unless_an_isolated_state_tree_exists() {
        let home = PathBuf::from("/home/u");
        let scratch = std::env::temp_dir().join(format!("chonk-omarchy-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        // XDG_STATE_HOME set but holding no omarchy tree: Omarchy's own
        // path, because that is where omarchy-theme-set writes.
        std::fs::create_dir_all(&scratch).unwrap();
        assert_eq!(
            current_dir_in(Some(scratch.clone()), Some(home.clone())),
            Some(PathBuf::from("/home/u/.local/state/omarchy/current"))
        );
        assert_eq!(current_dir_in(None, Some(home.clone())), Some(PathBuf::from("/home/u/.local/state/omarchy/current")));
        assert_eq!(current_dir_in(None, None), None);
        // With one, it is the isolated tree.
        std::fs::create_dir_all(scratch.join("omarchy")).unwrap();
        assert_eq!(current_dir_in(Some(scratch.clone()), Some(home)), Some(scratch.join("omarchy/current")));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn loading_from_a_current_dir_names_the_theme_and_reports_absence() {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_from_dir(&dir).unwrap_err().contains("colors.toml"));
        std::fs::create_dir_all(dir.join("theme")).unwrap();
        std::fs::write(dir.join("theme/colors.toml"), TOKYO_NIGHT).unwrap();
        std::fs::write(dir.join("theme.name"), "tokyo-night\n").unwrap();
        let theme = load_from_dir(&dir).unwrap();
        assert_eq!(theme.name, "Omarchy (Tokyo Night)");
        std::fs::write(dir.join("theme/colors.toml"), "not = [toml").unwrap();
        assert!(load_from_dir(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
