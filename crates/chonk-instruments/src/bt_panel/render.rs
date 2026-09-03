//! The BT panel's face: what Bluetooth this machine has, drawn as one
//! chiseled instrument. Pure — a [`BtView`] in, pixels out — and
//! byte-stable, which is what the tests pin and what lets every state
//! be rasterized headless on a desk with no radio in it.
//!
//! # Why the renderer lives here and not in `wm-theme`
//!
//! It used to be `wm_theme::bluetooth::render_bt_panel`, and the panel
//! it drew was one flat list of `row_h` rows on bare glass. Its input
//! is this crate's own panel state, and so is the vocabulary it needs
//! (device classes, pending toggles, an armed confirm); the two
//! sibling fold-outs — [`crate::link_panel::render`] and
//! [`crate::audio_panel::render`] — already draw from here for exactly
//! that reason. `wm-theme` keeps what is genuinely shared: the tile
//! face, the glass, the LED palette, the meters, and the rune. This
//! module is the arrangement.
//!
//! # The grammar
//!
//! A tile-face frame around one glass well — the LNK panel's shape, at
//! the same four-tile width, because the two radios are meant to read
//! as one family on the dock. Top to bottom:
//!
//! - **header** — the instrument's own mark (the Bluetooth rune,
//!   lit or ghosted by the radio's state), `BLUETOOTH`, a one-line
//!   status, and the connected count as LED digits.
//! - then either an **absence plate** — a large ghosted rune, a
//!   headline, and the notes that make the absence specific — or the
//!   **instrument furniture**: an `ADAPTER` section with the power
//!   row, a `DEVICES` section, and the `PAIR NEW DEVICE` action.
//!
//! # Three absences, three faces
//!
//! This panel's most-seen state on most desks is one of the three ways
//! there is nothing to show, and the reason this module was rewritten
//! is that they used to render alike — a 600x50 sliver reading
//! `NO ADAPTER`, which is what a machine with no radio showed every
//! time its tile was opened. They are three different truths and they
//! now look it:
//!
//! | Truth | Face |
//! |---|---|
//! | [`BtStatus::NoRadio`] — `/sys/class/bluetooth` is empty | ghosted rune at plate size, `NO BLUETOOTH RADIO`, and **no controls at all** — nothing here is actionable, so nothing looks it |
//! | [`BtStatus::NoDaemon`] — a controller exists, BlueZ does not answer | the controller named in lit ink (the hardware is real), `BLUETOOTH SERVICE IS DOWN`, and the literal shell remedy under a `REMEDY` rule |
//! | [`BtStatus::Off`] — adapter answering, radio down | the full instrument: a lit `ADAPTER` section whose power row is a button offering `TURN ON`, the block explained when rfkill set one, and the known devices below it in the disabled treatment |
//!
//! The fourth state, [`BtStatus::On`], is the one nobody on this
//! machine can reach: it is drawn from canned readings and reviewed
//! headless.

use cosmic_text::{FontSystem, SwashCache};
use wm_theme::instrument_panel as ip;
use wm_theme::instrument_panel::{LampState, MeterGlow, PanelStyle, RowState, TypeRole};
use wm_theme::model::{Color, TextAlign};
use wm_theme::panel::draw_led_digits;
use wm_theme::{paint, Theme};
use wm_theme_api::DecorationBuffer;

use super::bluez::Device;

// ---------------------------------------------------------------------
// The view: everything the renderer draws, as plain values.

/// Which of the four truths this panel is showing. The three absences
/// are separate variants rather than one `Off` with a reason, because
/// they get three different *layouts*, not three different labels —
/// see the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtStatus {
    /// `/sys/class/bluetooth` holds no controller: this machine owns
    /// no Bluetooth radio at all.
    NoRadio,
    /// A controller exists in sysfs but BlueZ is not answering on the
    /// bus — a stopped `bluetooth.service`, most often. Deliberately
    /// not folded into [`Off`](BtStatus::Off): "your radio is switched
    /// off" and "the software that drives your radio is not running"
    /// have different remedies, and only one of them is a button.
    NoDaemon,
    /// BlueZ is answering and no adapter is powered.
    Off { block: Block },
    /// BlueZ is answering and an adapter is powered.
    On { connected: u8 },
}

/// What rfkill says about the radio, which is what a power click has
/// to move first. See [`super`]'s module doc for why the block, and
/// not BlueZ's `Powered`, is the state that matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    None,
    /// `rfkill block bluetooth` — set by software, survives a reboot.
    Soft,
    /// A physical kill switch. Nothing this desktop can run clears it,
    /// so the power row is drawn as a fact rather than a control.
    Hard,
}

/// The device classes this panel has a glyph for. BlueZ's `Icon`
/// property is a freeform freedesktop icon name with a long tail;
/// [`DeviceClass::from_icon`] folds that tail into the shapes a
/// 7x7 LED grid can actually say something with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Headset,
    Speaker,
    Keyboard,
    Mouse,
    Phone,
    Gamepad,
    Computer,
    Watch,
    /// Everything else, including a device that published no `Icon` —
    /// drawn as a question mark, which is the honest glyph for "BlueZ
    /// did not say".
    Unknown,
}

impl DeviceClass {
    /// Folds BlueZ's `Icon` into a drawable class. Unknown names fall
    /// to [`DeviceClass::Unknown`] rather than to a plausible guess: a
    /// printer drawn as a headset is worse than a printer drawn as a
    /// question mark.
    pub fn from_icon(icon: Option<&str>) -> DeviceClass {
        match icon.unwrap_or("") {
            "audio-headset" | "audio-headphones" => DeviceClass::Headset,
            "audio-card" | "multimedia-player" | "video-display" => DeviceClass::Speaker,
            "input-keyboard" => DeviceClass::Keyboard,
            "input-mouse" | "input-tablet" => DeviceClass::Mouse,
            "input-gaming" => DeviceClass::Gamepad,
            "phone" => DeviceClass::Phone,
            "computer" => DeviceClass::Computer,
            "watch" => DeviceClass::Watch,
            _ => DeviceClass::Unknown,
        }
    }

    fn grid(self) -> &'static [&'static str; 7] {
        match self {
            DeviceClass::Headset => &GLYPH_HEADSET,
            DeviceClass::Speaker => &GLYPH_SPEAKER,
            DeviceClass::Keyboard => &GLYPH_KEYBOARD,
            DeviceClass::Mouse => &GLYPH_MOUSE,
            DeviceClass::Phone => &GLYPH_PHONE,
            DeviceClass::Gamepad => &GLYPH_GAMEPAD,
            DeviceClass::Computer => &GLYPH_COMPUTER,
            DeviceClass::Watch => &GLYPH_WATCH,
            DeviceClass::Unknown => &GLYPH_UNKNOWN,
        }
    }
}

/// One device row, already reduced to what the face shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    /// BlueZ's object path — the row's identity for hit-testing and
    /// the runtime argument of every per-device action.
    pub path: String,
    pub name: String,
    pub class: DeviceClass,
    pub connected: bool,
    /// A connect or disconnect asked for and not yet confirmed by a
    /// reading: the name dims and gains an ellipsis, the lamp drops to
    /// the dim level. A request, not a fact.
    pub pending: bool,
    /// `org.bluez.Battery1`'s `Percentage`, for the devices that
    /// publish one. Absent is absent — no row invents a full battery.
    pub battery: Option<u8>,
    /// This row's forget cell has had its first click and is waiting
    /// for the second. The row draws the question, not just the cell.
    pub armed: bool,
}

impl DeviceRow {
    /// The row as the panel takes it from a BlueZ reading.
    pub fn from_device(device: &Device, pending: bool, armed: bool) -> DeviceRow {
        DeviceRow {
            path: device.path.clone(),
            name: device.name.clone(),
            class: DeviceClass::from_icon(device.icon.as_deref()),
            connected: device.connected,
            pending,
            battery: device.battery,
            armed,
        }
    }
}

/// The identity of an interactive row, shared by the layout (which
/// places it), the renderer (which highlights it) and the panel state
/// machine (which acts on it) — the [`crate::link_panel`] `RowKey`
/// pattern, so the two radios answer a pointer the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtRowKey {
    /// The adapter power line.
    Power,
    /// A device row's body: connect or disconnect.
    Device(String),
    /// A device row's forget cell — a different action in the same
    /// band, which is why it is a key of its own rather than a flag on
    /// [`BtRowKey::Device`].
    Forget(String),
    /// The action that spawns the pairing dialog.
    PairNew,
}

/// Everything the renderer draws. Plain values with no reference to
/// the state machine that folded them, so a test — or a headless
/// design review on a machine with no radio — can hand-build any face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtView {
    pub status: BtStatus,
    /// The controller sysfs named, e.g. `hci0`. Shown by the
    /// [`BtStatus::NoDaemon`] plate, which exists to say the hardware
    /// is real even though BlueZ is silent.
    pub controller: Option<String>,
    /// Connected first, then paired-and-idle — the panel's grammar.
    /// Empty in every absence state that has no BlueZ reading to fill
    /// it from.
    pub devices: Vec<DeviceRow>,
    pub hover: Option<BtRowKey>,
    pub pressed: Option<BtRowKey>,
}

impl BtView {
    /// Whether device rows accept a click. They do not while the radio
    /// is down: connecting needs a powered adapter, and a row that
    /// looks live and does nothing is worse than one that says it is
    /// asleep.
    pub fn devices_live(&self) -> bool {
        matches!(self.status, BtStatus::On { .. })
    }
}

// ---------------------------------------------------------------------
// Glyphs.

/// The device-class marks, authored on a 7x7 cell grid so they stay
/// the same angular shapes at every tile size instead of picking up
/// antialiased curves the LED idiom does not have — the same argument,
/// and the same dot fraction, as `wm_theme::bluetooth`'s rune.
///
/// Seven cells square is the budget: at the stock 112px tile a row is
/// 34px and the glyph cell is about 20px of it, so a 7-row grid gets
/// ~2.9px per cell. An eighth row would put it under the speckle
/// threshold the rune's own design note measured.
const GLYPH_HEADSET: [&str; 7] = [
    "..###..",
    ".#...#.",
    "#.....#",
    "#.....#",
    "##...##",
    "##...##",
    "##...##",
];

const GLYPH_SPEAKER: [&str; 7] = [
    "..##...",
    ".###...",
    "####.#.",
    "####..#",
    "####.#.",
    ".###...",
    "..##...",
];

const GLYPH_KEYBOARD: [&str; 7] = [
    "#######",
    "#.....#",
    "#.#.#.#",
    "#.....#",
    "#.###.#",
    "#.....#",
    "#######",
];

const GLYPH_MOUSE: [&str; 7] = [
    "..###..",
    ".##.##.",
    ".##.##.",
    ".#####.",
    ".#####.",
    ".#####.",
    "..###..",
];

const GLYPH_PHONE: [&str; 7] = [
    ".#####.",
    ".#...#.",
    ".#...#.",
    ".#...#.",
    ".#...#.",
    ".#.#.#.",
    ".#####.",
];

const GLYPH_GAMEPAD: [&str; 7] = [
    ".#...#.",
    ".#####.",
    "#######",
    "#.#.#.#",
    "#######",
    "##...##",
    ".......",
];

const GLYPH_COMPUTER: [&str; 7] = [
    ".#####.",
    ".#...#.",
    ".#...#.",
    ".#####.",
    ".......",
    "#######",
    ".......",
];

const GLYPH_WATCH: [&str; 7] = [
    "..###..",
    "..###..",
    ".#####.",
    ".#...#.",
    ".#####.",
    "..###..",
    "..###..",
];

const GLYPH_UNKNOWN: [&str; 7] = [
    "..###..",
    ".#...#.",
    ".....#.",
    "...##..",
    "...#...",
    ".......",
    "...#...",
];

/// The forget mark: a hard-edged X on the same LED grid the class
/// glyphs use, at five cells rather than seven. The forget cell is the
/// smallest drawn thing on this glass — about 27px at the stock tile —
/// and a 7-cell diagonal there is a 2px stroke that reads as an
/// hourglass, which is the rune's own 9-row lesson at a smaller scale.
const GLYPH_CROSS: [&str; 5] = [
    "#...#",
    ".#.#.",
    "..#..",
    ".#.#.",
    "#...#",
];

/// Paints a dot-grid glyph into `(x, y, w, h)`, centered, every lit
/// cell a square dot in `color`.
///
/// This is `wm_theme::bluetooth::draw_bt_rune`'s loop generalized over
/// the grid. It is duplicated here rather than shared because the
/// shared side exposes only the one hard-coded rune; a
/// `draw_dot_glyph(grid, ...)` in the theme kit's vocabulary would
/// retire both copies, and that is a vocabulary gap this instrument is
/// reporting rather than reaching across a lane to fix.
fn draw_glyph(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, w: u32, h: u32, grid: &[&str], color: Color) {
    if w == 0 || h == 0 || grid.is_empty() {
        return;
    }
    let rows = grid.len() as f32;
    let cols = grid[0].len().max(1) as f32;
    let cell = (w as f32 / cols).min(h as f32 / rows);
    // The rune's 0.92 fill, for the rune's reason: these are single
    // continuous marks whose cells are mostly diagonal neighbours, and
    // at the bar meters' 0.7 the strokes come apart into speckles.
    let dot = (cell * 0.92).max(1.0);
    let x0 = x as f32 + (w as f32 - cell * cols) / 2.0;
    let y0 = y as f32 + (h as f32 - cell * rows) / 2.0;
    let size = dot.round().max(1.0) as u32;
    for (row, line) in grid.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if ch != b'#' {
                continue;
            }
            let cx = x0 + col as f32 * cell + (cell - dot) / 2.0;
            let cy = y0 + row as f32 * cell + (cell - dot) / 2.0;
            paint::fill_rect(pixmap, cx.round() as i32, cy.round() as i32, size, size, color);
        }
    }
}
// ---------------------------------------------------------------------
// Layout.

/// One horizontal band. Only [`Band::Row`] is interactive; everything
/// else is furniture, which is the whole point of the absence plates —
/// they are made entirely of furniture.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Band {
    Header,
    /// The absence plate's large mark.
    Plate,
    /// The absence plate's statement, in the readout step.
    Headline(String),
    /// A dim explanatory line.
    Note(String),
    /// A literal shell command, in a recessed field and in the
    /// command's own lower case — the answer to a state no button can
    /// fix is the command that fixes it.
    Hint(String),
    /// A section caption with an engraved rule out to the glass edge.
    Label(&'static str),
    Row(BtRowKey),
    /// An engraved divider across the glass.
    Rule,
}

/// Where everything sits. Built by [`bt_layout`], consumed by both the
/// renderer and the hit test, so what the pointer feels is exactly
/// what the eye sees.
#[derive(Clone, Debug)]
pub struct BtLayout {
    pub width: u32,
    pub height: u32,
    bands: Vec<(Band, i32, u32)>,
    glass_x: i32,
    glass_w: u32,
    /// Width of a device row's forget key, at this tile size.
    forget_w: u32,
    /// Whether device rows are live at all — a layout built from a
    /// view whose radio is off hit-tests its device bands as scenery.
    devices_live: bool,
}

impl BtLayout {
    /// The interactive row under a panel-local point, if any. Bounded
    /// by the layout's own height, which after [`fitted`] is the
    /// *granted* height: a row the glass cut off is not clickable,
    /// because a click has to land on something a person can see.
    ///
    /// [`fitted`]: BtLayout::fitted
    pub fn row_at(&self, x: i32, y: i32) -> Option<BtRowKey> {
        if x < self.glass_x || x >= self.glass_x + self.glass_w as i32 || y < 0 || y >= self.height as i32 {
            return None;
        }
        let band = self.bands.iter().find_map(|(band, band_y, band_h)| match band {
            Band::Row(key) if y >= *band_y && y < *band_y + *band_h as i32 => Some(key.clone()),
            _ => None,
        })?;
        // A device row is two controls in one band, split at the
        // engraved seam the renderer draws the forget key past.
        match band {
            BtRowKey::Device(path) => {
                if !self.devices_live {
                    return None;
                }
                if x >= self.forget_seam() {
                    Some(BtRowKey::Forget(path))
                } else {
                    Some(BtRowKey::Device(path))
                }
            }
            other => Some(other),
        }
    }

    /// The x of the seam a device row's forget key sits past — the one
    /// number the hit test and the renderer must agree on.
    fn forget_seam(&self) -> i32 {
        self.glass_x + self.glass_w as i32 - self.forget_w as i32
    }

    /// Where an interactive row is, in panel-local pixels:
    /// `(x, y, w, h)`. The hit test's inverse — [`BtLayout::row_at`]
    /// answers "what is under this point", this answers "where is this
    /// control", which is what a test (and anything that ever wants to
    /// draw a focus ring) needs. A [`BtRowKey::Forget`] gets the key's
    /// own band past the seam, not the whole row.
    pub fn row_rect(&self, key: &BtRowKey) -> Option<(i32, i32, u32, u32)> {
        let want = match key {
            BtRowKey::Forget(path) => BtRowKey::Device(path.clone()),
            other => other.clone(),
        };
        let (_, y, h) = self.bands.iter().find(|(band, _, _)| matches!(band, Band::Row(k) if *k == want))?;
        match key {
            BtRowKey::Forget(_) => {
                if !self.devices_live {
                    return None;
                }
                Some((self.forget_seam(), *y, self.forget_w, *h))
            }
            BtRowKey::Device(_) => {
                let seam = self.forget_seam();
                let w = if self.devices_live { (seam - self.glass_x).max(0) as u32 } else { self.glass_w };
                Some((self.glass_x, *y, w, *h))
            }
            _ => Some((self.glass_x, *y, self.glass_w, *h)),
        }
    }

    /// Re-anchors the layout onto the glass a *theme* actually
    /// produces: [`ip::draw_panel_ground`] insets its glass by
    /// [`ip::ground_inset`], which is derived from that theme's tile
    /// bevel, while [`bt_layout`] had to assume one — a hit test has
    /// no theme in hand (the `soundctl` zone-map precedent every panel
    /// in this crate follows). For all eight built-in themes the two
    /// numbers are the same 3, so nothing moves; on a plugin theme
    /// with a wider bevel this keeps the *drawing* off the well's lip,
    /// and the hit test is then off by the bevel difference rather
    /// than the content being drawn over the chrome. Called only on
    /// the render path, which is the only path that has a theme.
    pub fn anchored(mut self, glass_inset: i32) -> BtLayout {
        self.glass_x = glass_inset;
        self.glass_w = (self.width as i32 - glass_inset * 2).max(0) as u32;
        self
    }

    /// Re-anchors a natural layout onto the frame size the shell
    /// actually granted. Band heights and stacking order are the
    /// content's and do not move; only the glass's horizontal extent
    /// and the frame's bounds follow the grant — the clip-don't-
    /// rescale rule, applied to geometry.
    pub fn fitted(mut self, width: u32, height: u32) -> BtLayout {
        self.width = width;
        self.height = height;
        self.glass_w = (width as i32 - self.glass_x * 2).max(0) as u32;
        self
    }
}

/// The panel's width in tile edges — four, the same as the LNK panel's,
/// because the two radios' fold-outs should be the same object seen
/// twice rather than two sizes of list.
pub const TILES_WIDE: u32 = 4;

/// Fixed 1px bevel assumption, per the `soundctl` zone-map precedent:
/// every built-in theme's tile bevel is 1, and a hit test has no theme
/// in hand. This is [`ip::ground_inset`] with that bevel substituted —
/// and the render path replaces it with the theme's own answer through
/// [`BtLayout::anchored`], so the assumption only ever governs where a
/// *click* lands, never where a pixel goes.
const GLASS_INSET: i32 = 3;

/// How many device rows the panel will show. It cannot scroll — a dock
/// panel has no scroll input — so a desk with a drawer full of paired
/// junk shows what fits and says how much it did not.
pub const MAX_DEVICE_ROWS: usize = 8;

/// One row's height, identical to the LNK panel's so the two radios'
/// stacks share a rhythm.
pub fn row_h(tile: u32) -> u32 {
    ((tile.max(8) as f32) * 0.32).round().max(14.0) as u32
}

/// The panel's content width at `tile`.
pub fn panel_width(tile: u32) -> u32 {
    tile.max(8) * TILES_WIDE
}

/// Every number the layout and the renderer both need, derived once
/// from the tile scale. Two copies of this arithmetic is exactly how a
/// hit test drifts a few pixels off what the eye sees, so there is one.
#[derive(Clone, Copy)]
struct Metrics {
    /// An interactive row's height.
    rh: u32,
    /// The spacing unit. Every gap in this panel is a whole number of
    /// these, which is what gives the stack a rhythm instead of a
    /// scatter of ad-hoc paddings.
    step: i32,
    /// Horizontal inset from the glass edge to text. Wider than the
    /// vertical step: a row needs more room from a vertical edge than
    /// from its neighbours, or the first letter sits on the bezel.
    gutter: i32,
    /// The fixed left cell every row's lamp and class glyph live in, so
    /// names line up down the whole panel regardless of section.
    cell: u32,
    /// The header's two-line title block.
    title_h: u32,
    /// Section-label band — [`ip::section_h`].
    label_h: u32,
    /// Note / hint band.
    note_h: u32,
    /// The absence plate's headline.
    headline_h: u32,
    /// An engraved rule: groove plus highlight.
    rule_h: u32,
    /// The forget key's cell, seam included.
    forget_w: u32,
    width: u32,
}

impl Metrics {
    fn new(tile: u32) -> Metrics {
        let rh = row_h(tile);
        let step = ((rh as f32) * 0.22).round().max(3.0) as i32;
        Metrics {
            rh,
            step,
            gutter: step * 2,
            cell: ((rh as f32) * 1.15).round() as u32,
            title_h: ((rh as f32) * 1.85).round() as u32,
            label_h: ip::section_h(rh),
            note_h: ((rh as f32) * 0.80).round() as u32,
            headline_h: ((rh as f32) * 1.30).round() as u32,
            rule_h: 2,
            forget_w: ip::hit_size(((rh as f32) * 1.20).round() as u32),
            width: panel_width(tile),
        }
    }
}

/// Width of a device row's forget key, seam included, at a given row
/// height. Public because the panel's tests hit-test against it.
pub fn forget_cell_width(rh: u32) -> u32 {
    ip::hit_size(((rh as f32) * 1.20).round() as u32)
}

/// Places every band of `view` at `tile` scale. The one geometry
/// authority: the renderer draws inside these bands and the hit test
/// asks these bands, so the cells sit under their own clicks.
pub fn bt_layout(view: &BtView, tile: u32) -> BtLayout {
    let m = Metrics::new(tile.max(8));
    let glass_x = GLASS_INSET;
    let glass_w = (m.width as i32 - glass_x * 2).max(0) as u32;

    let mut bands: Vec<(Band, i32, u32)> = Vec::new();
    let mut y = glass_x + m.step * 2;
    let push = |bands: &mut Vec<(Band, i32, u32)>, y: &mut i32, band: Band, h: u32| {
        bands.push((band, *y, h));
        *y += h as i32;
    };

    push(&mut bands, &mut y, Band::Header, m.title_h);
    y += m.step;
    push(&mut bands, &mut y, Band::Rule, m.rule_h);
    y += m.step * 2;

    match &view.status {
        BtStatus::NoRadio => {
            // Sized against its neighbour: at four rows of plate and
            // five note lines this panel stood a quarter taller than
            // the LNK fold-out beside it on the dock, which made a
            // statement about *nothing being here* the largest object
            // on the desk. The mark and the headline carry the state;
            // the prose only has to be readable.
            push(&mut bands, &mut y, Band::Plate, (m.rh as f32 * 3.4) as u32);
            y += m.step;
            push(&mut bands, &mut y, Band::Headline("NO BLUETOOTH RADIO".to_string()), m.headline_h);
            y += m.step;
            push(&mut bands, &mut y, Band::Note("THIS MACHINE HAS NO CONTROLLER.".to_string()), m.note_h);
            push(&mut bands, &mut y, Band::Note("NOTHING TO SWITCH ON OR PAIR WITH.".to_string()), m.note_h);
            y += m.step * 2;
            push(&mut bands, &mut y, Band::Note("PLUG IN A USB ADAPTER AND IT APPEARS".to_string()), m.note_h);
            push(&mut bands, &mut y, Band::Note("HERE ON THE NEXT SAMPLE.".to_string()), m.note_h);
        }
        BtStatus::NoDaemon => {
            push(&mut bands, &mut y, Band::Plate, m.rh * 3);
            y += m.step;
            push(&mut bands, &mut y, Band::Headline("BLUETOOTH SERVICE DOWN".to_string()), m.headline_h);
            y += m.step;
            let controller = view.controller.clone().unwrap_or_else(|| "A CONTROLLER".to_string()).to_uppercase();
            push(&mut bands, &mut y, Band::Note(format!("{controller} IS PRESENT, BUT BLUEZ IS NOT")), m.note_h);
            push(&mut bands, &mut y, Band::Note("ANSWERING ON THE SYSTEM BUS.".to_string()), m.note_h);
            y += m.step * 2;
            push(&mut bands, &mut y, Band::Label("REMEDY"), m.label_h);
            y += m.step;
            push(&mut bands, &mut y, Band::Hint("systemctl start bluetooth".to_string()), m.rh);
        }
        BtStatus::Off { block } => {
            push(&mut bands, &mut y, Band::Label("ADAPTER"), m.label_h);
            y += m.step;
            push(&mut bands, &mut y, Band::Row(BtRowKey::Power), m.rh);
            y += m.step;
            match block {
                Block::Soft => {
                    push(&mut bands, &mut y, Band::Note("RFKILL HAS A SOFT BLOCK ON THIS RADIO.".to_string()), m.note_h);
                    push(&mut bands, &mut y, Band::Note("THE BLOCK SURVIVES A REBOOT.".to_string()), m.note_h);
                }
                Block::Hard => {
                    push(&mut bands, &mut y, Band::Note("A HARDWARE KILL SWITCH IS SET.".to_string()), m.note_h);
                    push(&mut bands, &mut y, Band::Note("NOTHING HERE CAN CLEAR IT.".to_string()), m.note_h);
                }
                Block::None => {
                    push(&mut bands, &mut y, Band::Note("THE ADAPTER IS POWERED DOWN.".to_string()), m.note_h);
                }
            }
            if !view.devices.is_empty() {
                y += m.step * 2;
                push(&mut bands, &mut y, Band::Label("DEVICES"), m.label_h);
                y += m.step;
                for device in view.devices.iter().take(MAX_DEVICE_ROWS) {
                    push(&mut bands, &mut y, Band::Row(BtRowKey::Device(device.path.clone())), m.rh);
                }
                y += m.step;
                push(&mut bands, &mut y, Band::Note("TURN THE RADIO ON TO CONNECT THESE.".to_string()), m.note_h);
            }
        }
        BtStatus::On { .. } => {
            push(&mut bands, &mut y, Band::Label("ADAPTER"), m.label_h);
            y += m.step;
            push(&mut bands, &mut y, Band::Row(BtRowKey::Power), m.rh);
            y += m.step * 2;
            push(&mut bands, &mut y, Band::Label("DEVICES"), m.label_h);
            y += m.step;
            if view.devices.is_empty() {
                push(&mut bands, &mut y, Band::Note("NO DEVICES PAIRED YET.".to_string()), m.note_h);
            }
            for device in view.devices.iter().take(MAX_DEVICE_ROWS) {
                push(&mut bands, &mut y, Band::Row(BtRowKey::Device(device.path.clone())), m.rh);
            }
            if view.devices.len() > MAX_DEVICE_ROWS {
                let hidden = view.devices.len() - MAX_DEVICE_ROWS;
                push(&mut bands, &mut y, Band::Note(format!("+{hidden} MORE PAIRED, NOT SHOWN")), m.note_h);
            }
            y += m.step * 2;
            push(&mut bands, &mut y, Band::Row(BtRowKey::PairNew), m.rh);
        }
    }

    let height = (y + m.step * 2 + glass_x) as u32;
    BtLayout { width: m.width, height, bands, glass_x, glass_w, forget_w: m.forget_w, devices_live: view.devices_live() }
}

// ---------------------------------------------------------------------
// Drawing.

/// The battery meter's level, `0.0..=1.0`, for [`ip::draw_meter`].
pub fn battery_level(percent: u8) -> f32 {
    percent.min(100) as f32 / 100.0
}

/// Renders the whole panel at its natural size — what the preview
/// harness and the byte-stability tests draw.
pub fn render_bt_panel(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile: u32,
    view: &BtView,
) -> DecorationBuffer {
    let layout = bt_layout(view, tile).anchored(ip::ground_inset(theme));
    let (w, h) = (layout.width, layout.height);
    render_layout(theme, fonts, swash, tile, view, layout, w, h)
}

/// Renders into the frame size the shell granted — the entry point the
/// `DockWidget::render_panel` path uses, because the grant, not the
/// request, is what `PanelFrame::adopt` will accept.
#[allow(clippy::too_many_arguments)]
pub fn render_bt_panel_into(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile: u32,
    view: &BtView,
    width: u32,
    height: u32,
) -> DecorationBuffer {
    let layout = bt_layout(view, tile).anchored(ip::ground_inset(theme)).fitted(width, height);
    render_layout(theme, fonts, swash, tile, view, layout, width, height)
}

#[allow(clippy::too_many_arguments)]
fn render_layout(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile: u32,
    view: &BtView,
    layout: BtLayout,
    width: u32,
    height: u32,
) -> DecorationBuffer {
    let mut pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1)).expect("nonzero bt panel size");
    let style = PanelStyle::new(theme);
    let m = Metrics::new(tile.max(8));

    // The face: tile gasket, sunken well, glass — the tile's own screen
    // recipe at panel size. The shell's chrome wraps this buffer in the
    // outer relief, so what lands on screen is one milled block.
    ip::draw_panel_ground(&mut pixmap, 0, 0, width, height, theme);

    let gx = layout.glass_x;
    let gw = layout.glass_w;
    // The text column is inset from the glass on both sides while row
    // grounds still span it edge to edge: content breathes, the
    // pointer target does not shrink.
    let tx = gx + m.gutter;
    let tw = gw.saturating_sub((m.gutter * 2) as u32);

    for (band, y, h) in &layout.bands {
        let (y, h) = (*y, *h);
        // A grant shorter than the content shows what fits. Drawing the
        // rest would only scribble under the bevel, and `row_at` has
        // already stopped offering those rows to the pointer.
        if y >= layout.height as i32 {
            continue;
        }
        match band {
            Band::Header => draw_header(&mut pixmap, fonts, swash, &style, &m, view, tx, y, tw, h),
            Band::Rule => ip::draw_engraved_rule(&mut pixmap, tx, y, tw, &style),
            Band::Plate => draw_plate(&mut pixmap, &style, view, tx, y, tw, h),
            Band::Headline(text) => {
                // The readout step, sized against the *glass* rather
                // than against a row: this is one line of type with
                // the whole panel to itself. Measured — at the
                // header's own size a 22-character headline ran off
                // the glass on the widest built-in face.
                let font = style.typeface(TypeRole::Readout, m.rh * 5 / 4);
                let text = ip::fit_type(fonts, &font, text, tw);
                ip::draw_type(&mut pixmap, fonts, swash, &font, &text, tx, y, tw, h, TextAlign::Center);
            }
            Band::Note(note) => {
                let font = style.typeface(TypeRole::Row, m.rh).receded(&style);
                let text = ip::fit_type(fonts, &font, note, tw);
                // The absence plates are prose about the whole machine
                // and are centred under their mark; a note attached to
                // a control hangs off the same left cell its rows do.
                let centered = matches!(view.status, BtStatus::NoRadio | BtStatus::NoDaemon);
                let (x, w, align) =
                    if centered { (tx, tw, TextAlign::Center) } else { (tx + m.cell as i32, tw.saturating_sub(m.cell), TextAlign::Left) };
                ip::draw_type(&mut pixmap, fonts, swash, &font, &text, x, y, w, h, align);
            }
            Band::Hint(hint) => draw_hint(&mut pixmap, fonts, swash, &style, &m, hint, tx, y, tw, h),
            Band::Label(label) => ip::draw_section_header(&mut pixmap, fonts, swash, &style, label, tx, y, tw, h),
            Band::Row(key) => draw_row(&mut pixmap, fonts, swash, &style, &m, view, &layout, key, gx, y, gw, h),
        }
    }

    DecorationBuffer { width: pixmap.width(), height: pixmap.height(), pixels: pixmap.data().to_vec() }
}

/// The header: the instrument's own mark, its name, one line of status,
/// and — when there is an instrument to take a reading — the connected
/// count as LED digits. Constant furniture: it identifies the
/// instrument in every state, and the *specific* truth is the plate's
/// or the rows' job.
#[allow(clippy::too_many_arguments)]
fn draw_header(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    view: &BtView,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    // The rune, lit exactly as the tile lights it: full ink for a
    // powered radio, ghost for everything else. The panel's mark and
    // the dock's mark must never disagree.
    let mark = (h * 3 / 4).max(8);
    let rune_color = match view.status {
        BtStatus::On { .. } => style.pal.ink,
        _ => style.pal.ghost,
    };
    wm_theme::bluetooth::draw_bt_rune(pixmap, x, y + (h.saturating_sub(mark) / 2) as i32, mark, mark, rune_color);

    let text_x = x + mark as i32 + m.gutter;

    // The count readout: lit while something is connected, the bare
    // ghost pattern while the adapter is merely off — a powered
    // instrument with nothing to say, which is exactly what the tile
    // shows.
    //
    // The two absent states get *no* readout. A ghosted pair of digits
    // beside `NO CONTROLLER` is furniture pretending to be an
    // instrument: there is no reading here that is failing to be
    // taken, there is no instrument. The plate takes the width back.
    let has_readout = matches!(view.status, BtStatus::On { .. } | BtStatus::Off { .. });
    let digits_w = if has_readout { m.rh * 3 / 2 } else { 0 };
    let digits_x = x + w as i32 - digits_w as i32;
    if has_readout {
        let connected = match view.status {
            BtStatus::On { connected } => connected,
            _ => 0,
        };
        let cells = if connected > 0 { wm_theme::bluetooth::count_digits(connected) } else { [None, None] };
        draw_led_digits(pixmap, digits_x, y + (h.saturating_sub(m.rh) / 2) as i32, digits_w, m.rh, &style.pal, &cells);
    }

    let label_w = (digits_x - text_x - m.gutter).max(0) as u32;
    let title = style.typeface(TypeRole::Readout, m.rh * 3 / 2);
    let title = match view.status {
        // No radio, no daemon: the instrument's own name recedes with
        // everything else it could have said.
        BtStatus::NoRadio | BtStatus::NoDaemon => title.receded(style),
        _ => title,
    };
    let line_h = h / 2;
    ip::draw_type(pixmap, fonts, swash, &title, "BLUETOOTH", text_x, y, label_w, line_h, TextAlign::Left);

    let detail = match &view.status {
        BtStatus::NoRadio => "NO CONTROLLER".to_string(),
        BtStatus::NoDaemon => "DAEMON SILENT".to_string(),
        BtStatus::Off { block: Block::Soft } => "OFF · SOFT BLOCK".to_string(),
        BtStatus::Off { block: Block::Hard } => "OFF · HARD BLOCK".to_string(),
        BtStatus::Off { block: Block::None } => "OFF".to_string(),
        BtStatus::On { connected: 0 } => "READY".to_string(),
        BtStatus::On { connected: 1 } => "1 DEVICE CONNECTED".to_string(),
        BtStatus::On { connected } => format!("{connected} DEVICES CONNECTED"),
    };
    let sub = style.typeface(TypeRole::Section, m.rh);
    let detail = ip::fit_type(fonts, &sub, &detail, label_w);
    ip::draw_type(pixmap, fonts, swash, &sub, &detail, text_x, y + line_h as i32, label_w, line_h, TextAlign::Left);
}

/// The absence plate's mark: the rune at plate size, unlit. A big
/// ghosted glyph is the LED idiom's way of saying "this readout exists
/// and has nothing to light" — the same sentence the dead tile says on
/// the dock, said at panel scale.
fn draw_plate(pixmap: &mut tiny_skia::Pixmap, style: &PanelStyle, view: &BtView, x: i32, y: i32, w: u32, h: u32) {
    let size = h;
    let cx = x + (w.saturating_sub(size) / 2) as i32;
    wm_theme::bluetooth::draw_bt_rune(pixmap, cx, y, size, size, style.pal.ghost);
    if !matches!(view.status, BtStatus::NoRadio) {
        return;
    }
    // No radio at all: strike the mark through. The rune ghosted means
    // "off"; the rune ghosted *and struck* means "there is no such
    // instrument in this machine", which is the difference the whole
    // plate exists to draw. The strike is engraved rather than inked —
    // a groove cut across a dead readout, not a reading of its own.
    let thickness = (size / 13).max(3);
    let bar_y = y + (h.saturating_sub(thickness) / 2) as i32;
    let bar_x = cx - (size / 8) as i32;
    let bar_w = size + size / 4;
    // A groove around a lit bar: the mark is cut through, and the cut
    // itself is the only thing on this plate carrying any current.
    paint::op_rect(pixmap, bar_x, bar_y - 1, bar_w, thickness + 2, -45);
    paint::fill_rect(pixmap, bar_x, bar_y, bar_w, thickness, style.pal.ink_dim);
}

/// A literal shell command, in the command's own lower case, in a
/// recessed field. Every other word on this glass is LED lettering in
/// tracked caps; a line that is not shouting is a line meant to be
/// typed, and the milled recess says where it ends.
#[allow(clippy::too_many_arguments)]
fn draw_hint(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    hint: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let field_h = (h * 4 / 5).max(8);
    let field_y = y + (h.saturating_sub(field_h) / 2) as i32;
    // A read-only recess: the glass cut down and chiseled. The design
    // system has a key cell (a control) and a meter track (a reading)
    // but no plain recessed field, so this is spelled out here — see
    // the module doc's note on what the vocabulary still lacks.
    paint::op_rect(pixmap, x, field_y, w, field_h, -16);
    paint::draw_sunken_bevel(pixmap, x, field_y, w, field_h, style.bevel);
    let font = style.typeface(TypeRole::Row, m.rh);
    let pad = m.gutter;
    let inner = w.saturating_sub(pad as u32 * 2);
    let text = ip::fit_type(fonts, &font, hint, inner);
    ip::draw_type(pixmap, fonts, swash, &font, &text, x + pad, field_y, inner, field_h, TextAlign::Left);
}

/// Which state a row's ground is in. Disabled wins over everything: a
/// row the hit test refuses can be neither hovered nor pressed, so it
/// must not be drawn as either.
fn state_of(view: &BtView, key: &BtRowKey, inert: bool) -> RowState {
    if inert {
        RowState::Disabled
    } else if view.pressed.as_ref() == Some(key) {
        RowState::Pressed
    } else if view.hover.as_ref() == Some(key) {
        RowState::Hover
    } else {
        RowState::Idle
    }
}

/// One interactive row: its ground in its state, then its contents.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    view: &BtView,
    layout: &BtLayout,
    key: &BtRowKey,
    gx: i32,
    y: i32,
    gw: u32,
    h: u32,
) {
    match key {
        BtRowKey::Power => {
            let inert = matches!(view.status, BtStatus::Off { block: Block::Hard });
            let state = state_of(view, key, inert);
            ip::draw_row_ground(pixmap, gx, y, gw, h, style, state);
            draw_power_row(pixmap, fonts, swash, style, m, view, state, gx + m.gutter, y, gw.saturating_sub((m.gutter * 2) as u32), h);
        }
        BtRowKey::PairNew => {
            let state = state_of(view, key, false);
            draw_pair_row(pixmap, fonts, swash, style, m, state, gx + m.gutter, y, gw.saturating_sub((m.gutter * 2) as u32), h);
        }
        BtRowKey::Device(path) | BtRowKey::Forget(path) => {
            let Some(device) = view.devices.iter().find(|d| &d.path == path) else { return };
            let live = view.devices_live();
            let body_key = BtRowKey::Device(path.clone());
            let forget_key = BtRowKey::Forget(path.clone());
            let mut body_state = state_of(view, &body_key, !live);
            let forget_state = state_of(view, &forget_key, !live);
            // An armed row is not a list entry any more: it is a
            // question waiting for an answer, and it comes forward off
            // the glass so the question is asked at row scale rather
            // than in the one cell that was clicked. The first click
            // has to *look* like a confirm being requested, or the
            // second one is a surprise.
            if device.armed && body_state == RowState::Idle {
                body_state = RowState::Hover;
            }
            let seam = layout.forget_seam();
            // The row's ground covers the body only: the forget key is
            // its own control with its own state, so a hover on one
            // must not light the other.
            ip::draw_row_ground(pixmap, gx, y, (seam - gx).max(0) as u32, h, style, body_state);
            draw_device_row(pixmap, fonts, swash, style, m, device, body_state, gx + m.gutter, y, (seam - gx - m.gutter).max(0) as u32, h);
            if live {
                ip::draw_engraved_seam(pixmap, seam, y, h, style);
                draw_forget_key(pixmap, style, device.armed, forget_state, seam, y, layout.forget_w, h);
            }
        }
    }
}

/// The adapter power line: lamp, name, and — at the right end — the
/// soft key that would change it, or the fact that nothing can.
#[allow(clippy::too_many_arguments)]
fn draw_power_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    view: &BtView,
    state: RowState,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let on = matches!(view.status, BtStatus::On { .. });
    let hard = matches!(view.status, BtStatus::Off { block: Block::Hard });
    let lamp = ip::lamp_size(h);
    ip::draw_lamp(
        pixmap,
        x + (m.cell.saturating_sub(lamp) / 2) as i32,
        y + (h.saturating_sub(lamp) / 2) as i32,
        lamp,
        style,
        if on { LampState::On } else { LampState::Off },
    );

    let text_x = x + m.cell as i32;
    let name = style.typeface(TypeRole::Row, h).colored(style.ink_for(TypeRole::Row, state));
    ip::draw_type(pixmap, fonts, swash, &name, "ADAPTER POWER", text_x, y, w.saturating_sub(m.cell), h, TextAlign::Left);

    // The right end is the control: the verb a click would perform, in
    // a key milled into the glass — or, under a hardware kill switch,
    // the flat receded word for a decision this desktop does not own.
    let legend = if hard {
        "LOCKED"
    } else if on {
        "TURN OFF"
    } else {
        "TURN ON"
    };
    let font = style.typeface(TypeRole::Row, h).colored(style.ink_for(TypeRole::Row, state));
    let key_w = (ip::type_width(fonts, &font, legend) + m.cell).min(w);
    let key_h = ip::hit_size(h * 4 / 5);
    let key_x = x + w as i32 - key_w as i32;
    let key_y = y + (h.saturating_sub(key_h) / 2) as i32;
    ip::draw_key_cell(pixmap, key_x, key_y, key_w, key_h, style, if hard { RowState::Disabled } else { state });
    ip::draw_type(pixmap, fonts, swash, &font, legend, key_x, key_y, key_w, key_h, TextAlign::Center);
}

/// The action that opens the pairing dialog: the whole row is one wide
/// soft key. It is the only control on this glass that spawns a
/// window, and it earns a key the device rows do not have.
#[allow(clippy::too_many_arguments)]
fn draw_pair_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    state: RowState,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let key_h = ip::hit_size(h * 4 / 5);
    let key_y = y + (h.saturating_sub(key_h) / 2) as i32;
    ip::draw_key_cell(pixmap, x, key_y, w, key_h, style, state);
    let font = style.typeface(TypeRole::Row, h).colored(style.ink_for(TypeRole::Row, state));
    ip::draw_type(pixmap, fonts, swash, &font, "+  PAIR A NEW DEVICE", x, key_y, w, key_h, TextAlign::Center);
    let _ = m;
}

/// One known device: lamp, class glyph, name, battery.
///
/// While the row's forget key is armed the row stops being a list entry
/// and becomes the question itself — the name is kept, so the question
/// names its device; the battery gives up its space; and `FORGET?`
/// stands in the readout step where the reading was.
#[allow(clippy::too_many_arguments)]
fn draw_device_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    m: &Metrics,
    device: &DeviceRow,
    state: RowState,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let disabled = state == RowState::Disabled;
    let lamp = ip::lamp_size(h);
    let lamp_state = if disabled {
        LampState::Off
    } else if device.pending {
        LampState::Pending
    } else if device.connected {
        LampState::On
    } else {
        LampState::Off
    };
    ip::draw_lamp(
        pixmap,
        x + (m.step / 2),
        y + (h.saturating_sub(lamp) / 2) as i32,
        lamp,
        style,
        lamp_state,
    );

    // The class glyph: what kind of thing this is, before its name.
    // Dim on a live row, receded on a dead one — a mark, never a
    // reading, so it never reaches full ink.
    let glyph = (h as f32 * 0.55).round().max(7.0) as u32;
    let glyph_x = x + (m.step / 2) + lamp as i32 + m.gutter;
    let glyph_color = if disabled { style.recede(style.pal.ink_dim) } else { style.pal.ink_dim };
    draw_glyph(pixmap, glyph_x, y + (h.saturating_sub(glyph) / 2) as i32, glyph, glyph, device.class.grid(), glyph_color);

    let name_x = glyph_x + glyph as i32 + m.gutter;
    let right = x + w as i32;

    // A connected device's name burns at the row step; one merely
    // known is a row that is present rather than active, so it recedes
    // — the same distinction the tile draws between lit and ghost.
    let name_font = {
        let font = style.typeface(TypeRole::Row, h);
        if disabled || device.pending || !device.connected {
            font.receded(style)
        } else {
            font
        }
    };
    let mut label = device.name.to_uppercase();
    if device.pending {
        label.push('…');
    }

    if device.armed {
        let ask = style.typeface(TypeRole::Readout, h);
        let ask_w = ip::type_width(fonts, &ask, "FORGET?");
        let ask_x = right - ask_w as i32;
        let name_w = (ask_x - name_x - m.gutter).max(0) as u32;
        let label = ip::fit_type(fonts, &name_font, &label, name_w);
        ip::draw_type(pixmap, fonts, swash, &name_font, &label, name_x, y, name_w, h, TextAlign::Left);
        ip::draw_type(pixmap, fonts, swash, &ask, "FORGET?", ask_x, y, ask_w, h, TextAlign::Right);
        return;
    }

    let battery_w = match device.battery {
        Some(_) if !disabled => (m.rh as f32 * 2.6) as u32,
        _ => 0,
    };
    let name_w = (right - name_x - battery_w as i32 - if battery_w > 0 { m.gutter } else { 0 }).max(0) as u32;
    let label = ip::fit_type(fonts, &name_font, &label, name_w);
    ip::draw_type(pixmap, fonts, swash, &name_font, &label, name_x, y, name_w, h, TextAlign::Left);
    if let (Some(percent), false) = (device.battery, disabled) {
        draw_battery(pixmap, fonts, swash, style, percent, device.connected, right - battery_w as i32, y, battery_w, h);
    }
}

/// A device's battery: a meter with the reading beside it, in the
/// design system's own two pieces — the level is a meter, never a bare
/// percentage, and the number is the readout step beside it.
///
/// The meter glows at full current only for a device that is actually
/// connected; a paired-and-idle headset's last known charge is a
/// reading the panel is *remembering*, not one it is taking.
#[allow(clippy::too_many_arguments)]
fn draw_battery(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    percent: u8,
    connected: bool,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let font = style.typeface(TypeRole::Readout, h);
    let text = format!("{}%", percent.min(100));
    let text_w = ip::type_width(fonts, &font, &text);
    let gap = (h / 5).max(2);
    let meter_w = w.saturating_sub(text_w + gap);
    let meter_h = ip::meter_h(h);
    let meter_y = y + (h.saturating_sub(meter_h) / 2) as i32;
    ip::draw_meter(
        pixmap,
        x,
        meter_y,
        meter_w,
        meter_h,
        style,
        battery_level(percent),
        if connected { MeterGlow::Active } else { MeterGlow::Idle },
    );
    // A device down to its last few per cent is the one battery
    // reading anybody is actually looking for. The LED palette has one
    // hue, so brightness is the whole alarm available: a low reading
    // keeps the hot readout ink, a healthy one steps back to the row
    // level so it stops competing with the device's own name.
    let font = if percent < 20 { font } else { font.receded(style) };
    ip::draw_type(pixmap, fonts, swash, &font, &text, x + (w - text_w) as i32, y, text_w, h, TextAlign::Right);
}

/// The forget key. Calm it is a milled key with a receded X — there,
/// findable, and not inviting. Hovered the key lifts and the X comes to
/// full ink: the one control on the row that lights all the way,
/// because it is the one that destroys something. Armed — first click
/// landed, confirm pending — the key is *filled* with ink and the X is
/// knocked out of it in glass, which is the loudest thing this palette
/// can say and the visible half of the two-click confirm.
#[allow(clippy::too_many_arguments)]
fn draw_forget_key(
    pixmap: &mut tiny_skia::Pixmap,
    style: &PanelStyle,
    armed: bool,
    state: RowState,
    seam_x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    let key_h = ip::hit_size(h * 3 / 4);
    let key_w = ip::hit_size(w * 3 / 4);
    let key_x = seam_x + ((w.saturating_sub(key_w)) / 2) as i32;
    let key_y = y + (h.saturating_sub(key_h) / 2) as i32;
    let mark = (key_w.min(key_h) * 5 / 8).max(5);
    let mx = key_x + (key_w.saturating_sub(mark) / 2) as i32;
    let my = key_y + (key_h.saturating_sub(mark) / 2) as i32;

    if armed {
        paint::fill_rect(pixmap, key_x, key_y, key_w, key_h, style.pal.ink);
        let big = (key_w.min(key_h) * 3 / 4).max(5);
        draw_glyph(
            pixmap,
            key_x + (key_w.saturating_sub(big) / 2) as i32,
            key_y + (key_h.saturating_sub(big) / 2) as i32,
            big,
            big,
            &GLYPH_CROSS,
            style.pal.glass,
        );
        return;
    }
    // Idle, the key has no relief: eight milled keys marching down the
    // right edge is more chrome than a list of headphones needs, and a
    // control that looks pressable on every row invites the accident
    // the two-click confirm exists to catch. The mark alone is enough
    // to find; pointing at it raises the key under it.
    if state != RowState::Idle {
        ip::draw_key_cell(pixmap, key_x, key_y, key_w, key_h, style, state);
    }
    let color = match state {
        RowState::Disabled => style.recede(style.pal.ink_dim),
        RowState::Idle => style.recede(style.pal.ink_dim),
        _ => style.ink(TypeRole::Readout),
    };
    draw_glyph(pixmap, mx, my, mark, mark, &GLYPH_CROSS, color);
}
