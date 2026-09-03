//! The instrument panel design system: the vocabulary every fold-out
//! panel behind a dock tile is drawn with, so a panel reads as *the
//! instrument opened up* rather than as a dialog that happened to
//! appear beside it.
//!
//! [`crate::panel`] is the same idea at tile scale — a 56px LED screen
//! with seven-segment digits and dot matrices, sized for a glance.
//! This module is that instrument at *reading* scale: the same glass,
//! the same theme-derived LED palette ([`crate::panel::panel_palette`]),
//! the same hard-edged unantialiased marks, but with the pieces a list
//! of controls needs — a type ramp, engraved rules, row states, meters
//! and lamps big enough to label and click.
//!
//! Nothing here invents a color. Every ink, every groove and every lift
//! derives from the theme's own panel accent, so all eight built-in
//! themes and both appearances get their own panel with zero per-panel
//! work — an Amber Phosphor panel glows amber, a Teal Blueprint one
//! teal, and neither needed a literal.
//!
//! # The vocabulary
//!
//! **Ground.** [`draw_panel_ground`] lays the whole face: a course of
//! tile fill (the gasket), the sunken well, and the glass inside it —
//! [`crate::panel::draw_panel_glass`]'s recipe at panel size, so the
//! panel looks milled from the same block as the tiles beside it. It
//! returns the glass interior; everything else in this module draws
//! inside that.
//!
//! **Type ramp.** [`TypeRole`] has three steps and they are three
//! genuinely different readings, not three shades of one:
//!
//! | role | size | tracking | weight | ink |
//! |---|---|---|---|---|
//! | [`TypeRole::Section`] | smallest | wide | bold | dim ([`PanelPalette::ink_dim`]) |
//! | [`TypeRole::Row`] | middle | none | bold | lit ([`PanelPalette::ink`]) |
//! | [`TypeRole::Readout`] | largest | slight | bold | hot (the accent lifted toward white) |
//!
//! Ask [`PanelStyle::typeface`] for one; it hands back a [`PanelFont`]
//! carrying spec, tracking and ink together, which
//! [`draw_type`]/[`type_width`]/[`fit_type`] consume.
//! [`PanelFont::receded`] takes any of them one step back for a
//! reading that is stale, muted or unavailable.
//!
//! **Engraved dividers.** [`draw_engraved_rule`] (horizontal) and
//! [`draw_engraved_seam`] (vertical) are a dark line plus a light one
//! — a groove chiseled into the glass, the way the resize bar's notch
//! pair is chiseled into the frame. A hairline in ink would read as a
//! reading; this reads as milling.
//!
//! **Section headers.** [`draw_section_header`] is a tracked dim label
//! with an engraved rule running from it to the right edge, so a panel
//! always says what it is showing.
//!
//! **Meters.** [`draw_meter`] is the panel's only way to show a level:
//! a recessed track of LED segments, drawn by the very function the
//! VOL tile's stacked bars use ([`crate::panel::draw_led_bar`]), so a
//! level in a panel and a level on a tile are the same instrument at
//! two sizes. A bare percentage is not a level — put the number beside
//! the meter as a [`TypeRole::Readout`], never instead of it.
//!
//! **Lamps.** [`draw_lamp`] is the LINK tile's port lamp: a square LED
//! set into the glass behind a dark bezel, with a halo when lit. Three
//! states ([`LampState`]) — lit, an optimistic `Pending` at the dim
//! level, and dark. A lamp is how a panel says "this one"; a colored
//! block with no bezel reads as an error light, which is what this
//! replaced.
//!
//! **Rows and keys.** [`draw_row_ground`] gives a list row its
//! [`RowState`] treatment (hover lifts the glass, press sinks it,
//! disabled leaves it flat while its inks recede), and
//! [`draw_key_cell`] is the same states for a discrete control — a key
//! milled into the glass, for a control that is not the whole row.
//!
//! **Hit targets.** [`MIN_HIT`] is the floor, in device pixels, that
//! any control's smaller edge must reach. [`hit_size`] applies it.
//! Every control here scales with its row, and a row compressed past
//! this stops being clickable rather than becoming a smear — see the
//! audio panel's row clamping for the pattern.
//!
//! # Using it
//!
//! ```no_run
//! # use wm_theme::instrument_panel as ip;
//! # use wm_theme::model::TextAlign;
//! # fn draw(pixmap: &mut tiny_skia::Pixmap, theme: &wm_theme::Theme,
//! #        fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) {
//! let style = ip::PanelStyle::new(theme);
//! let (gx, gy, gw, _gh) = ip::draw_panel_ground(pixmap, 0, 0, 300, 120, theme);
//! ip::draw_section_header(pixmap, fonts, swash, &style, "OUTPUTS", gx, gy, gw, 14);
//! let label = style.typeface(ip::TypeRole::Row, 24);
//! ip::draw_type(pixmap, fonts, swash, &label, "Speakers", gx, gy + 16, gw, 24, TextAlign::Left);
//! ip::draw_meter(pixmap, gx, gy + 44, 80, 8, &style, 0.62, ip::MeterGlow::Active);
//! # }
//! ```

use tiny_skia::Pixmap;

use crate::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use crate::paint;
use crate::panel::{self, PanelPalette};
use crate::tile;
use crate::Theme;

/// The smallest edge, in device pixels, a clickable control in a panel
/// may have. Below this a lamp, a glyph and a label stop being
/// separable marks and a press becomes a guess. A panel whose grant
/// cannot give a control this much should drop the control (and its
/// hit target with it) rather than draw one nobody can hit.
pub const MIN_HIT: u32 = 14;

/// How many segments a full-width panel meter shows — the VOL tile's
/// own count ([`crate::soundctl::SOUND_BAR_SEGMENTS`]), so a level
/// reads the same on the tile and in the panel. Narrow meters degrade
/// to fewer, wider segments; see [`meter_segments`].
pub const METER_SEGMENTS: u32 = 8;

/// The three steps of the panel type ramp. See the module doc's table:
/// they differ in size, tracking *and* brightness at once, because
/// three shades of one red is not a hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeRole {
    /// The band label — "OUTPUTS", "CONNECTIONS". Small, widely
    /// tracked, dim: furniture that names what follows.
    Section,
    /// A row's own name — the device, the network, the peripheral.
    /// The panel's body text.
    Row,
    /// The number or verdict a row is *for*: a level, a percentage, a
    /// state word. The brightest thing in the panel.
    Readout,
}

/// What a row (or a key) is doing under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Idle,
    /// The pointer is over it: the glass lifts.
    Hover,
    /// Armed — the button contract's pressed half: the glass sinks and
    /// takes a sunken chisel.
    Pressed,
    /// Present but inert. The ground stays flat and every ink recedes
    /// (see [`PanelStyle::ink_for`]), so a row that refuses clicks
    /// looks like one.
    Disabled,
}

/// A lamp's three readings — the LINK tile's port lamp, restated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LampState {
    /// Dark: a ghost LED, visible as hardware, saying nothing.
    Off,
    /// An optimistic in-flight change, lit at the dim level: the LED
    /// equivalent of "asking".
    Pending,
    /// Lit.
    On,
}

/// How brightly a meter's lit segments glow — which is how a panel
/// says whether the thing it is metering is *doing* anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterGlow {
    /// Live: full LED ink.
    Active,
    /// Idle but real: the dim level. The level is still readable, the
    /// instrument is just not driving anything.
    Idle,
    /// Silenced: no segment lights at all, the whole track in ghosts —
    /// the muted tile's all-ghost glass, meter-shaped. The level the
    /// mixer still remembers is not a level anything is playing at.
    Silent,
}

/// One step of the type ramp, resolved: the shaped font, the letter
/// tracking that goes with it, and the ink it is set in. Built by
/// [`PanelStyle::typeface`] and consumed by [`draw_type`],
/// [`type_width`] and [`fit_type`] — three values that must travel
/// together, since a section label drawn without its tracking, or a
/// readout in row ink, silently flattens the ramp.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelFont {
    pub spec: FontSpec,
    /// Extra device pixels between characters. Zero for body text.
    pub tracking: f32,
    pub color: Color,
}

impl PanelFont {
    /// The same type, one step back down the ramp — for a reading that
    /// is stale, muted, or on a row that refuses clicks.
    pub fn receded(mut self, style: &PanelStyle) -> PanelFont {
        self.color = style.recede(self.color);
        self
    }

    /// The same type in an ink the caller picked off the palette. For
    /// the cases the ramp does not name; reach for [`PanelFont::receded`]
    /// first.
    pub fn colored(mut self, color: Color) -> PanelFont {
        self.color = color;
        self
    }
}

/// Everything a panel needs to draw itself in a theme's own voice: the
/// LED palette, the one font family the theme is known to have
/// registered, and the chrome thickness every groove scales with.
///
/// Build one per render ([`PanelStyle::new`]) and pass it around; it is
/// the single place a panel's colors come from.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelStyle {
    /// The theme's glass/ghost/ink/dim, from
    /// [`crate::panel::panel_palette`].
    pub pal: PanelPalette,
    /// The theme's registered menu family. A generic name like
    /// "sans-serif" is not a real face to cosmic-text and would render
    /// nothing at all, so panels use the family the theme itself
    /// declares.
    pub family: String,
    /// Chrome thickness in device pixels — the theme's tile bevel
    /// width, at least 1. Grooves, bezels and chisels are this thick,
    /// so a panel scales with the desktop like every other surface.
    pub bevel: u32,
}

impl PanelStyle {
    pub fn new(theme: &Theme) -> PanelStyle {
        PanelStyle {
            pal: panel::panel_palette(theme),
            family: theme.menu.item_font.family.clone(),
            bevel: theme.tile.bevel.width.max(1) as u32,
        }
    }

    /// The ink for a ramp step on an ordinary row.
    pub fn ink(&self, role: TypeRole) -> Color {
        match role {
            TypeRole::Section => self.pal.ink_dim,
            TypeRole::Row => self.pal.ink,
            // The lit core of an LED: the accent pulled toward white,
            // which is what makes a readout brighter than the label
            // beside it in *every* theme rather than only the dark ones.
            TypeRole::Readout => mix(self.pal.ink, Color::rgb(0xFF, 0xFF, 0xFF), 0.42),
        }
    }

    /// The ink for a ramp step in a given row state — the one call
    /// that knows a disabled row's type recedes.
    pub fn ink_for(&self, role: TypeRole, state: RowState) -> Color {
        let ink = self.ink(role);
        match state {
            RowState::Disabled => self.recede(ink),
            _ => ink,
        }
    }

    /// Any ink, one step back toward the glass. Not a grey wash: a
    /// receding LED is the same LED with less current through it, and
    /// that is the only "disabled" a lit instrument has.
    pub fn recede(&self, color: Color) -> Color {
        mix(color, self.pal.glass, 0.58)
    }

    /// The resolved type for a ramp step at a given row height —
    /// everything a panel needs to set one string. Sizes are a
    /// fraction of the row so the ramp survives every scale, with a
    /// pixel floor so a compressed row still shapes real glyphs.
    pub fn typeface(&self, role: TypeRole, row_h: u32) -> PanelFont {
        let h = row_h.max(MIN_HIT) as f32;
        let (size, tracking) = match role {
            TypeRole::Section => ((h * 0.30).max(7.0), (h * 0.30).max(7.0) * 0.22),
            TypeRole::Row => ((h * 0.40).max(8.5), 0.0),
            TypeRole::Readout => ((h * 0.46).max(9.5), (h * 0.46).max(9.5) * 0.04),
        };
        PanelFont {
            spec: FontSpec {
                family: self.family.clone(),
                size,
                weight: FontWeight::Bold,
                style: FontStyle::Normal,
            },
            tracking,
            color: self.ink(role),
        }
    }

    /// The dark half of an engraved groove: the glass, cut into.
    fn engrave_dark(&self) -> Color {
        mix(self.pal.glass, Color::rgb(0, 0, 0), 0.65)
    }

    /// The light half: the groove's far wall catching the panel's own
    /// glow. Derived from the accent, so the milling belongs to the
    /// theme like everything else.
    fn engrave_light(&self) -> Color {
        mix(self.pal.glass, self.pal.ink, 0.28)
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color::rgb(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
}

/// The floor [`MIN_HIT`] states, applied: any control's smaller edge,
/// clamped up. Use it when sizing a control from a row height, so a
/// compressed row's controls stay pressable.
pub fn hit_size(desired: u32) -> u32 {
    desired.max(MIN_HIT)
}

// ---------------------------------------------------------------------
// Ground
// ---------------------------------------------------------------------

/// How far the glass sits inside a panel's content rect: one course of
/// tile face (the gasket) plus the well's sunken lip. Layout code that
/// must know where the glass starts before it draws — a hit test, a
/// size request — asks this rather than re-deriving it.
pub fn ground_inset(theme: &Theme) -> i32 {
    let t = theme.tile.bevel.width.max(1) as i32;
    t * 2 + 1
}

/// The panel's face: tile fill over the whole content rect, a sunken
/// well inset one bevel into it, and the glass inside that — the tile's
/// own screen recipe ([`crate::panel::draw_panel_glass`]) restated at
/// panel size, so the fold-out reads as milled from the same block as
/// the tile it came from.
///
/// Returns the glass interior `(x, y, w, h)`. Draw readouts inside
/// that and nothing outside it.
pub fn draw_panel_ground(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, theme: &Theme) -> (i32, i32, u32, u32) {
    let t = theme.tile.bevel.width.max(1) as i32;
    paint::fill_area(pixmap, x, y, w, h, &theme.tile.fill);
    let well_w = (w as i32 - t * 2).max(0) as u32;
    let well_h = (h as i32 - t * 2).max(0) as u32;
    panel::draw_panel_glass(pixmap, x + t, y + t, well_w, well_h, theme)
}

// ---------------------------------------------------------------------
// Engraving
// ---------------------------------------------------------------------

/// A groove chiseled across the glass: a dark line with a light one
/// under it, `style.bevel` thick each. The panel's divider — between
/// sections, under a header, between a list and what follows it.
pub fn draw_engraved_rule(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, style: &PanelStyle) {
    if w == 0 {
        return;
    }
    let t = style.bevel;
    paint::fill_rect(pixmap, x, y, w, t, style.engrave_dark());
    paint::fill_rect(pixmap, x, y + t as i32, w, t, style.engrave_light());
}

/// The same groove standing up: a dark column and a light one, marking
/// where a row's meaning changes — the seam a mute key or a secondary
/// control sits past.
pub fn draw_engraved_seam(pixmap: &mut Pixmap, x: i32, y: i32, h: u32, style: &PanelStyle) {
    if h == 0 {
        return;
    }
    let t = style.bevel;
    paint::fill_rect(pixmap, x, y, t, h, style.engrave_dark());
    paint::fill_rect(pixmap, x + t as i32, y, t, h, style.engrave_light());
}

/// The natural height of a section header band for a given row height.
/// Shorter than a row: a header names rows, it is not one of them.
pub fn section_h(row_h: u32) -> u32 {
    ((row_h as f32) * 0.62).round().max(11.0) as u32
}

/// A band label with its rule: the tracked dim caps at the left, an
/// engraved groove from the end of the lettering to the right edge,
/// centred on the lettering's own baseline band. This is how a panel
/// says what it is showing.
#[allow(clippy::too_many_arguments)]
pub fn draw_section_header(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    style: &PanelStyle,
    label: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let font = style.typeface(TypeRole::Section, h * 2);
    let text_w = type_width(fonts, &font, label).min(w);
    draw_type(pixmap, fonts, swash, &font, label, x, y, text_w, h, TextAlign::Left);
    // The rule starts a gap past the lettering and runs to the edge,
    // vertically centred on the band so it reads as one line of
    // furniture rather than as an underline.
    let gap = (h as i32 / 3).max(3);
    let rule_x = x + text_w as i32 + gap;
    let rule_w = (x + w as i32 - rule_x).max(0) as u32;
    let rule_y = y + (h as i32 - style.bevel as i32 * 2) / 2;
    draw_engraved_rule(pixmap, rule_x, rule_y, rule_w, style);
}

// ---------------------------------------------------------------------
// Rows and keys
// ---------------------------------------------------------------------

/// How much a hovered surface lifts, and a pressed one sinks. Relative
/// (see [`crate::paint::op_rect`]) rather than an absolute fill, so one
/// recipe reads correctly on every theme's glass.
const HOVER_LIFT: i16 = 24;
const PRESS_SINK: i16 = -18;

/// A list row's ground in its current state: [`RowState::Idle`] leaves
/// the glass alone (a row is a region of the screen, not a widget
/// stuck on it), hover lifts it, press sinks it under a sunken chisel,
/// and disabled leaves it flat — its inks do the talking, via
/// [`PanelStyle::ink_for`].
pub fn draw_row_ground(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, style: &PanelStyle, state: RowState) {
    if w == 0 || h == 0 {
        return;
    }
    match state {
        RowState::Idle | RowState::Disabled => {}
        RowState::Hover => paint::op_rect(pixmap, x, y, w, h, HOVER_LIFT),
        RowState::Pressed => {
            paint::op_rect(pixmap, x, y, w, h, PRESS_SINK);
            paint::draw_sunken_bevel(pixmap, x, y, w, h, style.bevel);
        }
    }
}

/// A discrete control on the glass — a key milled into the panel, for
/// an action that is not the whole row (the mute key beside a device,
/// a soft key under a reading). Idle it is a shallow raised cell;
/// hovered it lifts; pressed it takes the same sunken chisel a row
/// does; disabled it stays flat and its glyph recedes.
///
/// Callers must give it at least [`MIN_HIT`] on its smaller edge —
/// [`hit_size`] — or leave it out entirely.
pub fn draw_key_cell(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, style: &PanelStyle, state: RowState) {
    if w == 0 || h == 0 {
        return;
    }
    let t = style.bevel;
    match state {
        RowState::Pressed => {
            paint::op_rect(pixmap, x, y, w, h, PRESS_SINK);
            paint::draw_sunken_bevel(pixmap, x, y, w, h, t);
        }
        RowState::Disabled => {
            paint::op_rect(pixmap, x, y, w, h, 6);
        }
        state => {
            let lift = if state == RowState::Hover { HOVER_LIFT } else { 14 };
            paint::op_rect(pixmap, x, y, w, h, lift);
            // A key stands a hair proud of the glass: the raised
            // chisel, one course thick, in the tile family's own light
            // direction.
            paint::op_rect(pixmap, x, y, w, t, 55);
            paint::op_rect(pixmap, x, y, t, h, 55);
            paint::op_rect(pixmap, x, y + h as i32 - t as i32, w, t, -35);
            paint::op_rect(pixmap, x + w as i32 - t as i32, y, t, h, -35);
        }
    }
}

// ---------------------------------------------------------------------
// Lamps and meters
// ---------------------------------------------------------------------

/// A lamp's natural edge for a given row height: big enough to be a
/// mark, small enough to stay a lamp.
pub fn lamp_size(row_h: u32) -> u32 {
    ((row_h as f32) * 0.26).round().max(5.0) as u32
}

/// The LINK tile's port lamp at panel scale: a square LED set into the
/// glass behind a dark bezel — lit, it burns white-hot in the middle of
/// its accent-colored body and spills a halo onto the panel around it.
/// That layering is the whole difference between a lamp and a colored
/// square: a flat block of accent reads as an error light (which is
/// exactly what this replaced), while a core inside a body inside a
/// bezel reads as a component that is *on*.
pub fn draw_lamp(pixmap: &mut Pixmap, x: i32, y: i32, size: u32, style: &PanelStyle, state: LampState) {
    let s = size.max(3);
    let (fill, halo) = match state {
        LampState::On => (style.pal.ink, Some(mix(style.pal.glass, style.pal.ink, 0.45))),
        LampState::Pending => (style.pal.ink_dim, None),
        // Darker than a ghost segment: an unlit lamp is a component
        // that is off, not a reading at the bottom of its scale.
        LampState::Off => (mix(style.pal.ghost, style.pal.glass, 0.45), None),
    };
    // The bezel: one dark course all round, so the lamp reads as a
    // component seated in the panel rather than a painted square.
    let t = style.bevel as i32;
    paint::fill_rect(
        pixmap,
        x - t,
        y - t,
        s + t as u32 * 2,
        s + t as u32 * 2,
        style.engrave_dark(),
    );
    paint::fill_rect(pixmap, x, y, s, s, fill);
    if state == LampState::On {
        // The filament: the lit core, one course in from the body.
        let core = s as i32 - t * 2;
        if core > 0 {
            paint::fill_rect(pixmap, x + t, y + t, core as u32, core as u32, mix(fill, Color::rgb(0xFF, 0xFF, 0xFF), 0.5));
        }
    }
    if let Some(halo) = halo {
        // A one-course glow just outside the bezel — the light a real
        // lamp spills onto the panel around it.
        let (hx, hy) = (x - t * 2, y - t * 2);
        let hw = s + t as u32 * 4;
        paint::fill_rect(pixmap, hx, hy, hw, t as u32, halo);
        paint::fill_rect(pixmap, hx, hy + hw as i32 - t, hw, t as u32, halo);
        paint::fill_rect(pixmap, hx, hy, t as u32, hw, halo);
        paint::fill_rect(pixmap, hx + hw as i32 - t, hy, t as u32, hw, halo);
    }
}

/// A meter's natural height for a given row height — a strip, not a
/// bar chart.
pub fn meter_h(row_h: u32) -> u32 {
    ((row_h as f32) * 0.26).round().max(4.0) as u32
}

/// How many segments a meter of this width shows: [`METER_SEGMENTS`]
/// where there is room for each to be a readable cell, fewer and wider
/// where there is not. Never zero, so a meter is always a meter.
pub fn meter_segments(w: u32) -> u32 {
    METER_SEGMENTS.min(w / 4).max(3)
}

/// Lit segments for a level: ceiling, so any nonzero level lights at
/// least one cell — a 3% reading showing a dark meter would read as
/// silence, which it is not. The VOL tile's own rule
/// ([`crate::soundctl::lit_segments`]), restated here so the panel does
/// not have to reach into the tile.
pub fn lit_segments(level: f32, segments: u32) -> u32 {
    if level <= 0.0 || segments == 0 {
        return 0;
    }
    ((level * segments as f32).ceil() as u32).clamp(1, segments)
}

/// A level, as a meter: a groove chiseled into the glass with LED
/// segments in it, filling left to right. `level` is `0.0..=1.0`
/// (values above clamp to a full meter); [`MeterGlow`] says how
/// brightly the lit ones burn.
///
/// The segments are drawn by [`crate::panel::draw_led_bar`] — the same
/// call the VOL tile's stacked bars make — so a panel meter and a tile
/// meter are one instrument at two sizes. A panel that prints a
/// percentage where a meter belongs has left the family.
#[allow(clippy::too_many_arguments)]
pub fn draw_meter(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, style: &PanelStyle, level: f32, glow: MeterGlow) {
    if w == 0 || h == 0 {
        return;
    }
    let t = style.bevel as i32;
    // The recessed track: the glass cut down, with the groove's dark
    // wall along the top and its lit wall along the bottom.
    paint::fill_rect(pixmap, x, y, w, h, style.engrave_dark());
    paint::fill_rect(pixmap, x, y + h as i32 - t, w, t as u32, style.engrave_light());

    let inner_x = x + t;
    let inner_y = y + t;
    let inner_w = (w as i32 - t * 2).max(0) as u32;
    let inner_h = (h as i32 - t * 2).max(1) as u32;
    if inner_w == 0 {
        return;
    }
    let segments = meter_segments(inner_w);
    let lit = match glow {
        MeterGlow::Silent => 0,
        _ => lit_segments(level.clamp(0.0, 1.0), segments),
    };
    // draw_led_bar takes its colors from a palette; handing it one
    // whose ink is this meter's glow is what makes "playing" and
    // "idle" the same meter at two currents.
    //
    // An idle meter is toned, not dimmed to the label level: the level
    // is the reading, and a reading nobody can count is not one.
    // (Design review caught exactly that — idle meters drawn in
    // `ink_dim` were indistinguishable from their own ghosts at row
    // scale.) The unlit cells go the other way, darker than a ghost
    // segment on the tile, because a panel meter is read across a list
    // rather than glanced at alone.
    let pal = PanelPalette {
        ink: match glow {
            MeterGlow::Active => style.pal.ink,
            _ => mix(style.pal.ink, style.pal.glass, 0.28),
        },
        ghost: mix(style.pal.glass, style.pal.ghost, 0.45),
        ..style.pal
    };
    panel::draw_led_bar(pixmap, inner_x, inner_y, inner_w, inner_h, &pal, segments, lit, false);
}

/// The blocky speaker mark the VOL tile wears, at panel scale: a driver
/// box plus a three-step cone, all hard-edged rects in the fractional
/// grid of its `s`-sided cell so it survives 11px and 40px alike.
/// `strike` lays the rising slash over it — the crossed-out speaker
/// that means muted, in the panel's own accent.
///
/// Here rather than in an instrument because the mute control is a
/// panel idiom: whatever a panel mutes, this is the mark for it.
pub fn draw_speaker(pixmap: &mut Pixmap, x: i32, y: i32, s: u32, body: Color, strike: Option<Color>) {
    let f = s as f32;
    for (fx, fy, fw, fh) in [
        (0.08, 0.36, 0.22, 0.28),
        (0.30, 0.28, 0.16, 0.44),
        (0.46, 0.18, 0.16, 0.64),
        (0.62, 0.06, 0.16, 0.88),
    ] {
        paint::fill_rect(
            pixmap,
            x + (fx * f).round() as i32,
            y + (fy * f).round() as i32,
            ((fw * f).round() as u32).max(1),
            ((fh * f).round() as u32).max(1),
            body,
        );
    }
    let Some(strike) = strike else { return };
    let thick = ((f / 8.0).round() as i32).max(2);
    let (x0, y0) = (x + (0.06 * f).round() as i32, y + (0.88 * f).round() as i32);
    let (x1, y1) = (x + (0.78 * f).round() as i32, y + (0.10 * f).round() as i32);
    for i in 0..thick {
        tile::draw_line(pixmap, x0 + i, y0, x1 + i, y1, strike);
    }
}

// ---------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------

/// The shaped width of `text` in this type, tracking included — what a
/// layout measures a label against before deciding it fits.
pub fn type_width(fonts: &mut cosmic_text::FontSystem, font: &PanelFont, text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    if font.tracking <= 0.0 {
        return paint::text_width(fonts, &font.spec, text);
    }
    let count = text.chars().count() as f32;
    let mut width = 0.0f32;
    for ch in text.chars() {
        width += paint::text_width(fonts, &font.spec, &ch.to_string()) as f32;
    }
    (width + font.tracking * (count - 1.0).max(0.0)).ceil() as u32
}

/// Sets `text` in this type inside `(x, y, w, h)`. Tracked type is laid
/// out a character at a time (cosmic-text has no letter-spacing of its
/// own), which is exactly what tracking means anyway; untracked type
/// goes through [`crate::paint::draw_text`] in one shaped run.
#[allow(clippy::too_many_arguments)]
pub fn draw_type(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    font: &PanelFont,
    text: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    align: TextAlign,
) {
    if text.is_empty() || w == 0 || h == 0 {
        return;
    }
    if font.tracking <= 0.0 {
        paint::draw_text(pixmap, fonts, swash, text, &font.spec, font.color, x, y, w, h, align);
        return;
    }
    let total = type_width(fonts, font, text);
    let offset = match align {
        TextAlign::Left => 0,
        TextAlign::Center => ((w as i32 - total as i32) / 2).max(0),
        TextAlign::Right => (w as i32 - total as i32).max(0),
    };
    let mut cursor = x + offset;
    let right = x + w as i32;
    for ch in text.chars() {
        let glyph = ch.to_string();
        let cw = paint::text_width(fonts, &font.spec, &glyph);
        if cursor >= right {
            return;
        }
        // Each character gets its own box, so the run cannot re-shape
        // itself narrower than the width the tracking was measured at.
        let box_w = (cw + 1).min((right - cursor) as u32);
        paint::draw_text(pixmap, fonts, swash, &glyph, &font.spec, font.color, cursor, y, box_w, h, TextAlign::Left);
        cursor += cw as i32 + font.tracking.round() as i32;
    }
}

/// Ellipsizes `text` to fit `max_w` at the width this type actually
/// shapes to — trimming characters until the trimmed form plus `…`
/// fits. A label too long for its box loses characters, never its box.
pub fn fit_type(fonts: &mut cosmic_text::FontSystem, font: &PanelFont, text: &str, max_w: u32) -> String {
    if type_width(fonts, font, text) <= max_w {
        return text.to_string();
    }
    let mut kept: Vec<char> = text.chars().collect();
    while kept.len() > 1 {
        kept.pop();
        let candidate: String = kept.iter().collect::<String>().trim_end().to_string() + "…";
        if type_width(fonts, font, &candidate) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::{all_themes_in, nextstep_classic};
    use crate::model::Appearance;

    fn lum(c: Color) -> i32 {
        c.r as i32 + c.g as i32 + c.b as i32
    }

    fn every_theme() -> Vec<Theme> {
        let mut themes = all_themes_in(Appearance::Dark);
        themes.extend(all_themes_in(Appearance::Light));
        themes
    }

    fn ctx() -> (cosmic_text::FontSystem, cosmic_text::SwashCache) {
        (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new())
    }

    fn pixmap(w: u32, h: u32, theme: &Theme) -> Pixmap {
        let mut pixmap = Pixmap::new(w, h).unwrap();
        draw_panel_ground(&mut pixmap, 0, 0, w, h, theme);
        pixmap
    }

    fn count_of(pixmap: &Pixmap, color: Color) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|p| (p.red(), p.green(), p.blue()) == (color.r, color.g, color.b))
            .count()
    }

    /// The ramp's whole point: three steps that are genuinely
    /// different readings, in every theme and both appearances. Size
    /// ascends, tracking distinguishes the section label, and the inks
    /// keep their order with real distance between them.
    #[test]
    fn the_type_ramp_keeps_its_contrast_ordering_in_every_theme() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            let section = style.typeface(TypeRole::Section, 32);
            let row = style.typeface(TypeRole::Row, 32);
            let readout = style.typeface(TypeRole::Readout, 32);

            assert!(
                section.spec.size < row.spec.size && row.spec.size < readout.spec.size,
                "theme {}: the ramp must ascend in size",
                theme.id
            );
            assert!(section.tracking > 0.0, "theme {}: the section label is tracked", theme.id);
            assert_eq!(row.tracking, 0.0, "theme {}: body type is not tracked", theme.id);

            let (s, r, o) = (lum(section.color), lum(row.color), lum(readout.color));
            assert!(s + 90 < r, "theme {}: section ({s}) must be clearly dimmer than a row label ({r})", theme.id);
            assert!(r + 40 < o, "theme {}: a readout ({o}) must clearly out-glow a row label ({r})", theme.id);
            assert!(
                lum(style.recede(row.color)) + 60 < r,
                "theme {}: a receded reading must fall well behind a live one",
                theme.id
            );
        }
    }

    /// Every ink the ramp hands out has to be readable on the glass it
    /// is set on — the dimmest step included.
    #[test]
    fn every_ramp_step_carries_the_glass_in_every_theme() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            let glass = lum(style.pal.glass);
            for role in [TypeRole::Section, TypeRole::Row, TypeRole::Readout] {
                let ink = lum(style.ink(role));
                assert!(ink - glass > 150, "theme {}: {role:?} ink {ink} is lost on glass {glass}", theme.id);
            }
        }
    }

    /// A meter is a meter at every level: nothing at zero, a single
    /// cell for a whisper, everything at full — and never the same
    /// picture twice.
    #[test]
    fn a_meter_draws_zero_one_percent_and_full_distinctly() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let draw = |level: f32, glow: MeterGlow| {
            let mut p = pixmap(120, 24, &theme);
            draw_meter(&mut p, 8, 8, 100, 8, &style, level, glow);
            p.data().to_vec()
        };
        let zero = draw(0.0, MeterGlow::Active);
        let whisper = draw(0.01, MeterGlow::Active);
        let half = draw(0.5, MeterGlow::Active);
        let full = draw(1.0, MeterGlow::Active);
        let silent = draw(1.0, MeterGlow::Silent);
        let idle = draw(1.0, MeterGlow::Idle);
        assert_ne!(zero, whisper, "1% must light a cell zero does not");
        assert_ne!(whisper, half, "a whisper and a half must differ");
        assert_ne!(half, full, "half and full must differ");
        assert_ne!(full, idle, "a playing meter and an idle one burn at different currents");
        assert_ne!(idle, silent, "a silenced meter lights nothing at all");
        assert_eq!(zero, draw(0.0, MeterGlow::Active), "and the drawing is a pure function of its inputs");

        // Lit ink grows monotonically with the level.
        let ink_at = |level: f32| {
            let mut p = pixmap(120, 24, &theme);
            draw_meter(&mut p, 8, 8, 100, 8, &style, level, MeterGlow::Active);
            count_of(&p, style.pal.ink)
        };
        let (low, mid, high) = (ink_at(0.2), ink_at(0.6), ink_at(1.0));
        assert!(low < mid && mid < high, "lit area must rise with the level: {low} < {mid} < {high}");
        assert_eq!(ink_at(0.0), 0, "a zero meter lights nothing");
    }

    /// The design-review regression: an idle meter's lit cells must
    /// still be countable against its unlit ones. Drawn in the ramp's
    /// dim ink they were not — at row scale a 40% meter and a 100% one
    /// were the same picture — so both glows are pinned here, in every
    /// theme and both appearances.
    #[test]
    fn lit_and_unlit_meter_cells_are_countable_in_every_theme() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            for glow in [MeterGlow::Active, MeterGlow::Idle] {
                let mut p = pixmap(120, 24, &theme);
                // Half a meter: the first four cells lit, the last four not.
                draw_meter(&mut p, 8, 8, 96, 8, &style, 0.5, glow);
                let at = |x: u32| {
                    let px = p.pixels()[(12 * 120 + x) as usize];
                    px.red() as i32 + px.green() as i32 + px.blue() as i32
                };
                // Cell centres: 8 cells across 94 interior pixels.
                let lit = at(9 + 94 / 16);
                let unlit = at(9 + 94 * 15 / 16);
                assert!(
                    lit - unlit > 90,
                    "theme {} ({glow:?}): a lit cell ({lit}) must be countable against an unlit one ({unlit})",
                    theme.id
                );
            }
        }
    }

    #[test]
    fn lit_segments_ceil_so_a_whisper_still_shows() {
        assert_eq!(lit_segments(0.0, 8), 0);
        assert_eq!(lit_segments(0.001, 8), 1);
        assert_eq!(lit_segments(0.55, 8), 5);
        assert_eq!(lit_segments(1.0, 8), 8);
        assert_eq!(lit_segments(2.0, 8), 8, "over-full clamps to a full meter");
        assert_eq!(lit_segments(0.5, 0), 0);
    }

    /// A narrow meter keeps being a meter: fewer, wider cells rather
    /// than a smear or nothing at all.
    #[test]
    fn meters_degrade_to_fewer_cells_rather_than_vanish() {
        assert_eq!(meter_segments(100), METER_SEGMENTS);
        assert_eq!(meter_segments(32), METER_SEGMENTS);
        assert_eq!(meter_segments(20), 5);
        assert_eq!(meter_segments(4), 3, "even a sliver shows three cells");
        assert_eq!(meter_segments(0), 3);
    }

    /// The three lamp states are three different lamps, in every
    /// theme — the panel's way of saying "this one".
    #[test]
    fn lamp_states_are_distinguishable_in_every_theme() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            let draw = |state: LampState| {
                let mut p = pixmap(40, 40, &theme);
                draw_lamp(&mut p, 12, 12, 12, &style, state);
                p.data().to_vec()
            };
            let on = draw(LampState::On);
            let pending = draw(LampState::Pending);
            let off = draw(LampState::Off);
            assert_ne!(on, pending, "theme {}: lit and pending differ", theme.id);
            assert_ne!(pending, off, "theme {}: pending and dark differ", theme.id);
            assert_ne!(on, off, "theme {}: lit and dark differ", theme.id);
        }
    }

    /// Hover, press and disabled all have to look like something —
    /// and disabled must not look like hover.
    #[test]
    fn row_and_key_states_all_read_differently() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            let row = |state: RowState| {
                let mut p = pixmap(120, 40, &theme);
                draw_row_ground(&mut p, 4, 4, 112, 32, &style, state);
                p.data().to_vec()
            };
            let key = |state: RowState| {
                let mut p = pixmap(120, 40, &theme);
                draw_key_cell(&mut p, 80, 8, 24, 24, &style, state);
                p.data().to_vec()
            };
            for draw in [&row as &dyn Fn(RowState) -> Vec<u8>, &key] {
                let idle = draw(RowState::Idle);
                let hover = draw(RowState::Hover);
                let pressed = draw(RowState::Pressed);
                let disabled = draw(RowState::Disabled);
                assert_ne!(idle, hover, "theme {}: hover must show", theme.id);
                assert_ne!(hover, pressed, "theme {}: press must not look like hover", theme.id);
                assert_ne!(idle, pressed, "theme {}: press must show", theme.id);
                assert_ne!(hover, disabled, "theme {}: a disabled control must not look hovered", theme.id);
            }
        }
    }

    /// The ground is the tile's own screen recipe: the glass sits
    /// [`ground_inset`] inside the content rect, and it is glass —
    /// which is what makes a panel read as the instrument opened up.
    #[test]
    fn the_ground_is_glass_inside_a_gasket_in_every_theme() {
        for theme in every_theme() {
            let style = PanelStyle::new(&theme);
            let mut p = Pixmap::new(200, 100).unwrap();
            let (gx, gy, gw, gh) = draw_panel_ground(&mut p, 0, 0, 200, 100, &theme);
            let inset = ground_inset(&theme);
            assert_eq!((gx, gy), (inset, inset), "theme {}: the glass sits one gasket in", theme.id);
            assert_eq!((gw, gh), ((200 - inset * 2) as u32, (100 - inset * 2) as u32), "theme {}", theme.id);
            let glass = count_of(&p, style.pal.glass);
            assert!(
                glass > (gw * gh) as usize * 9 / 10,
                "theme {}: the interior should be glass, found {glass} of {}",
                theme.id,
                gw * gh
            );
            // Every pixel opaque: a panel is a face, never a hole.
            assert!(p.pixels().iter().all(|px| px.alpha() == 255), "theme {}: the ground is opaque", theme.id);
        }
    }

    /// An engraved rule is two lines, not one: a dark wall and a lit
    /// one. A hairline would read as a reading.
    #[test]
    fn an_engraved_rule_is_a_groove_not_a_hairline() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let mut p = pixmap(60, 20, &theme);
        draw_engraved_rule(&mut p, 6, 10, 48, &style);
        let dark = count_of(&p, style.engrave_dark());
        let light = count_of(&p, style.engrave_light());
        assert_eq!(dark, 48, "the dark wall runs the rule's whole width");
        assert_eq!(light, 48, "and the lit wall under it");
        assert_ne!(style.engrave_dark(), style.engrave_light(), "the two walls are different colors");

        let mut seam = pixmap(20, 60, &theme);
        draw_engraved_seam(&mut seam, 10, 6, 48, &style);
        assert_eq!(count_of(&seam, style.engrave_dark()), 48, "the standing groove has both walls too");
        assert_eq!(count_of(&seam, style.engrave_light()), 48);
    }

    /// Tracking is real: a tracked string measures wider than the same
    /// string set solid, and the header lays its rule past the
    /// lettering rather than under it.
    #[test]
    fn tracked_type_is_wider_and_headers_carry_their_rule() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let (mut fonts, mut swash) = ctx();
        let section = style.typeface(TypeRole::Section, 32);
        let solid = PanelFont { tracking: 0.0, ..section.clone() };
        let tracked_w = type_width(&mut fonts, &section, "OUTPUTS");
        let solid_w = type_width(&mut fonts, &solid, "OUTPUTS");
        assert!(tracked_w > solid_w, "tracking widens the label: {tracked_w} vs {solid_w}");

        let mut p = pixmap(200, 30, &theme);
        let before = p.data().to_vec();
        draw_section_header(&mut p, &mut fonts, &mut swash, &style, "OUTPUTS", 6, 6, 188, 14);
        assert_ne!(before, p.data().to_vec(), "the header draws something");
        assert!(count_of(&p, style.engrave_dark()) > 40, "the header's rule runs to the right edge");
    }

    #[test]
    fn fitting_trims_characters_not_the_box() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let (mut fonts, _) = ctx();
        let row = style.typeface(TypeRole::Row, 32);
        let long = "Family 17h/19h HD Audio Controller Digital Stereo";
        let fitted = fit_type(&mut fonts, &row, long, 90);
        assert!(fitted.len() < long.len(), "a long label loses characters");
        assert!(fitted.ends_with('…'), "and says so");
        assert!(type_width(&mut fonts, &row, &fitted) <= 90, "the result fits");
        assert_eq!(fit_type(&mut fonts, &row, "OK", 400), "OK", "what fits is left alone");
    }

    #[test]
    fn the_hit_floor_is_stated_and_applied() {
        assert_eq!(hit_size(4), MIN_HIT, "a tiny control is floored");
        assert_eq!(hit_size(40), 40, "a big one is left alone");
        // The floor is a real target, not a formality: whatever a
        // control asks for, what it gets is at least a pressable edge.
        assert!(hit_size(1) >= 14);
        assert!(hit_size(0) >= 14);
    }

    /// Nothing in the vocabulary panics or draws outside its pixmap at
    /// the sizes a clamped grant can produce.
    #[test]
    fn every_primitive_survives_a_squeezed_panel() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let (mut fonts, mut swash) = ctx();
        for (w, h) in [(1u32, 1u32), (8, 4), (40, 14), (400, 300)] {
            let mut p = Pixmap::new(w, h).unwrap();
            let (gx, gy, gw, gh) = draw_panel_ground(&mut p, 0, 0, w, h, &theme);
            draw_section_header(&mut p, &mut fonts, &mut swash, &style, "OUTPUTS", gx, gy, gw, section_h(gh));
            draw_row_ground(&mut p, gx, gy, gw, gh, &style, RowState::Hover);
            draw_key_cell(&mut p, gx, gy, gw.min(20), gh.min(20), &style, RowState::Pressed);
            draw_lamp(&mut p, gx, gy, lamp_size(gh), &style, LampState::On);
            draw_meter(&mut p, gx, gy, gw, meter_h(gh), &style, 0.5, MeterGlow::Active);
            draw_engraved_rule(&mut p, gx, gy, gw, &style);
            draw_engraved_seam(&mut p, gx, gy, gh, &style);
            draw_speaker(&mut p, gx, gy, 9, style.pal.ink, Some(style.pal.ink));
            let row = style.typeface(TypeRole::Row, 20);
            draw_type(&mut p, &mut fonts, &mut swash, &row, "Speakers", gx, gy, gw, gh, TextAlign::Left);
        }
    }
}
