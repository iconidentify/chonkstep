//! The link panel's face: every network the machine could be on, as
//! one chiseled instrument. Pure — a [`PanelView`] in, pixels out —
//! and byte-stable: the same view, theme and size always produce the
//! same buffer, which is what the tests pin.
//!
//! # Drawn in the shared vocabulary, not in its own
//!
//! Every mark here comes out of [`wm_theme::instrument_panel`], the
//! design system the fold-out panels share: [`ip::draw_panel_ground`]
//! for the glass, [`ip::PanelStyle`]'s three-step type ramp for every
//! word, [`ip::draw_engraved_rule`] and [`ip::draw_section_header`]
//! for the milling, [`ip::draw_lamp`] for state, [`ip::draw_meter`]
//! for level, [`ip::draw_row_ground`] and [`ip::draw_key_cell`] for
//! what the pointer is doing. The panel invents no colour and no
//! groove of its own, which is the whole point: opened beside the
//! dock, it has to look like the LNK tile unfolded rather than like a
//! dialog that happened to appear there.
//!
//! Signal strength in particular is [`ip::draw_meter`] — the same
//! `wm_theme::panel::draw_led_bar` the VOL tile's stacked bars are
//! made of — so a level read here and a level read on a tile are one
//! instrument at two sizes. The first cut drew its own five-bar
//! staircase; a bespoke meter in a panel full of shared ones is
//! exactly the kind of near-miss that made the set look unrelated.
//!
//! # Hierarchy, which is what the first cut lacked
//!
//! Judged at real scale beside the dock, the first cut set section
//! labels, row names and readouts at nearly the same size in nearly
//! the same red: the eye found no structure. The ramp now does the
//! work — [`TypeRole::Section`] for the band labels (smallest, widely
//! tracked, dim), [`TypeRole::Row`] for names, [`TypeRole::Readout`]
//! for the verdict a row is *for* (largest, hottest) — and the header
//! sets its link name in the readout step at half again a row's
//! height, so it outranks the whole stack.
//!
//! # Top to bottom
//!
//! - **header** — the current link as a title block: its name in the
//!   readout ink, its nature (`WIFI · 87%`, `ETHERNET · 1000M`,
//!   `DOWN`) tracked below it, a signal meter at the right on wifi,
//!   and an engraved rule closing the block.
//! - **CONNECTIONS** — one row per NetworkManager profile worth a
//!   toggle (ethernet, wifi, WireGuard, VPN): a lamp and a kind tag
//!   (`E`/`W`/`WG`/`VPN`) in the fixed left cell, the profile's name
//!   on the shared name column, and `BUSY` while an optimistic toggle
//!   waits for the sample that confirms it.
//! - **WI-FI NETWORKS** — the scan list: a signal meter in the left
//!   cell, a lock glyph beside the verdict on secured networks, and
//!   the row's one-word verdict (`LINKED`, `SAVED`, `JOIN…`, `OPEN`).
//!   No radio, or nothing in range, is a *designed* empty state — a
//!   dead meter in a socket milled into the glass (`Band::Empty`),
//!   the panel's version of the SDK's dead tile — not a line of body
//!   text where a row should be.
//! - **TAILSCALE** — the tailnet row, the exit-node field (which is
//!   where the first cut's unexplained `…` went: a bare ellipsis
//!   hanging off the row's right edge told nobody what it was or that
//!   it could be pressed, so the tailnet's state now says a word and
//!   the exit node is a labelled reading of its own), at most two note
//!   lines, and — when a toggle came back `Access denied` — the CLI's
//!   own remedy line, because the honest answer to a click that cannot
//!   work is the command that would make it work.
//! - **RESCAN** — the one row that drives the radio, under its own
//!   engraved rule as the panel's footer, drawn as an
//!   [`ip::draw_key_cell`] soft key and disabled to `RESCAN · WAIT`
//!   while the post-scan cooldown runs.
//!
//! # Geometry is the layout's, and only the layout's
//!
//! [`panel_layout`] is the single authority on where every band sits;
//! the renderer draws inside its bands and the state machine hit-tests
//! against them, so what the pointer feels is exactly what the eye
//! sees. Both sides take their numbers from one `Metrics`, so a
//! spacing change cannot desync the two. Like `wm_theme::soundctl`'s
//! zone map, the layout assumes the built-in themes' 1px tile bevel
//! rather than taking a theme — input handlers have no theme to offer.

use cosmic_text::{FontSystem, SwashCache};
use wm_theme::instrument_panel as ip;
use wm_theme::instrument_panel::{LampState, MeterGlow, PanelFont, PanelStyle, RowState, TypeRole};
use wm_theme::model::{Color, TextAlign};
use wm_theme::{paint, Theme};
use wm_theme_api::DecorationBuffer;

use super::data::ConnKind;
use super::tailscale::{BackendState, OperatorState, TailscaleStatus};

/// The current link, as the header draws it. The LNK tile's sampler
/// already holds this (see `WifiWidget::link_header`); a panel opened
/// before the tile has settled shows `Unknown` honestly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkHeader {
    Unknown,
    Wifi { ssid: String, signal: u8 },
    Wired { interface: String, speed_mbps: Option<u32> },
    Down { interface: String },
}

/// A row's LED lamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lamp {
    Off,
    On,
    /// An optimistic in-flight toggle: lit at the dim level, the LED
    /// equivalent of "connecting…". The next sample resolves it.
    Pending,
}

impl Lamp {
    /// The shared system's lamp reading. One mapping, so a lamp in
    /// this panel and a lamp in the audio or bluetooth panel are the
    /// same component.
    fn state(self) -> LampState {
        match self {
            Lamp::Off => LampState::Off,
            Lamp::On => LampState::On,
            Lamp::Pending => LampState::Pending,
        }
    }
}

/// One CONNECTIONS row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnRow {
    pub uuid: String,
    pub name: String,
    pub kind: ConnKind,
    pub lamp: Lamp,
    /// Activated outside NetworkManager (`connected (externally)`); a
    /// `connection down` may not keep such a link down, so the row
    /// says `EXT` about itself.
    pub external: bool,
}

/// One WI-FI NETWORKS row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetRow {
    pub ssid: String,
    pub signal: u8,
    pub secured: bool,
    /// A saved profile exists — one click connects.
    pub known: bool,
    pub in_use: bool,
    pub pending: bool,
}

/// The TAILSCALE section's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailscaleRow {
    /// `None` until the first sample lands (the binary exists, or the
    /// row would be absent entirely).
    pub status: Option<TailscaleStatus>,
    pub operator: OperatorState,
    pub pending: bool,
}

/// The WI-FI NETWORKS section: either rows, or the honest note that
/// this machine has no radio to scan with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiSection {
    NoHardware,
    Networks(Vec<NetRow>),
}

/// Everything the renderer draws — plain values, no reference to the
/// state machine that folded them, so a test can hand-build any face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelView {
    pub header: LinkHeader,
    pub connections: Vec<ConnRow>,
    pub wifi: WifiSection,
    /// `None` when the `tailscale` binary is absent: no row at all.
    pub tailscale: Option<TailscaleRow>,
    pub rescan_cooling: bool,
    pub hover: Option<RowKey>,
    pub pressed: Option<RowKey>,
}

/// The identity of an interactive row, shared by the layout (which
/// places it), the renderer (which highlights it) and the state
/// machine (which acts on it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    /// A connection profile, by UUID — the one name every action argv
    /// uses.
    Conn(String),
    /// A scanned network, by SSID.
    Net(String),
    Tailscale,
    Rescan,
}

/// One horizontal band of the panel. Only `Row` bands are
/// interactive; the rest are furniture.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Band {
    Header,
    /// A full-width engraved rule on its own — under the header, and
    /// above the rescan footer. Section rules belong to `Label`.
    Rule,
    Label(&'static str),
    Row(RowKey),
    /// A labelled reading that is not a control: `EXIT NODE  GATEWAY`.
    /// The panel's answer to "state the fact, don't hint at a menu
    /// that isn't there".
    Field(&'static str, String),
    /// A dim informational line (health, offline).
    Note(String),
    /// The NeedsOperator remedy, verbatim from the CLI.
    Hint(String),
    /// A designed empty state: a dead meter and a legend in a socket
    /// milled into the glass.
    Empty(&'static str),
}

/// Where everything sits. Built by [`panel_layout`], consumed by both
/// the renderer and the hit test.
pub struct PanelLayout {
    pub width: u32,
    pub height: u32,
    bands: Vec<(Band, i32, u32)>,
    /// Left/right interior edge of the glass, for hit-testing x.
    glass_x: i32,
    glass_w: u32,
}

impl PanelLayout {
    /// The interactive row under a panel-local point, if any.
    ///
    /// Bounded by the layout's own height, which after [`fitted`] is
    /// the *granted* height: a row the glass cut off is not clickable,
    /// because a click has to land on something the person can see.
    ///
    /// [`fitted`]: PanelLayout::fitted
    pub fn row_at(&self, x: i32, y: i32) -> Option<RowKey> {
        if x < self.glass_x || x >= self.glass_x + self.glass_w as i32 || y < 0 || y >= self.height as i32 {
            return None;
        }
        self.bands.iter().find_map(|(band, band_y, band_h)| match band {
            Band::Row(key) if y >= *band_y && y < *band_y + *band_h as i32 => Some(key.clone()),
            _ => None,
        })
    }

    /// Re-anchors a natural layout onto the frame size the shell
    /// actually granted.
    ///
    /// Band *heights and stacking order* are the content's and do not
    /// move; only the glass's horizontal extent and the frame's bounds
    /// follow the grant. That is the whole clip-don't-rescale rule
    /// applied to geometry: a grant taller than the content gets glass
    /// all the way down (the panel fills its frame rather than
    /// floating in it), a grant shorter than the content shows as much
    /// as fits and hit-tests exactly that much, and nothing is ever
    /// squeezed to a size the text was not laid out for.
    pub fn fitted(mut self, width: u32, height: u32) -> PanelLayout {
        self.width = width;
        self.height = height;
        self.glass_w = (width as i32 - self.glass_x * 2).max(0) as u32;
        self
    }
}

/// How many scan-list rows the panel shows. It cannot scroll (a dock
/// panel has no scroll input yet), so it shows the strongest; a dense
/// apartment block's tail is noise anyway.
pub const MAX_WIFI_ROWS: usize = 8;

/// The panel's width in tile edges.
const TILES_WIDE: u32 = 4;

/// Fixed 1px bevel assumption, per the `soundctl` zone-map precedent:
/// every built-in theme's tile bevel is 1, and a hit test has no theme
/// in hand. This is [`ip::ground_inset`] with that bevel substituted.
const GLASS_INSET: i32 = 3;

/// One row's height. Shared with the bluetooth panel so the two
/// radios' stacks keep one rhythm.
fn row_h(tile: u32) -> u32 {
    ((tile.max(8) as f32) * 0.32).round().max(14.0) as u32
}

/// Every number the layout and the renderer both need, derived once
/// from the tile scale. Two copies of this arithmetic is exactly how a
/// hit test drifts a few pixels off what the eye sees, so there is
/// one.
#[derive(Clone, Copy)]
struct Metrics {
    /// An interactive row's height.
    rh: u32,
    /// The spacing unit. Every gap in the panel is a whole number of
    /// these, which is what gives the stack a rhythm instead of a
    /// scatter of ad-hoc paddings.
    step: i32,
    /// Horizontal inset from the glass edge to text. Wider than the
    /// vertical step: a row needs more room from a vertical edge than
    /// from its neighbours, or the first letter sits on the bezel.
    gutter: i32,
    /// The fixed left cell every row's lamp or meter lives in, so
    /// names line up down the whole panel regardless of section.
    cell: u32,
    /// How much of that cell a mark may actually occupy: the cell less
    /// one step of air before the name column. Both things that live
    /// in the cell measure against this — the wifi meter's width and
    /// the right edge the connection tags align to — so the left cell
    /// has one content edge rather than two that nearly agree. It did
    /// not, before: the meter stopped a gutter short of the cell while
    /// the tags stopped a step short, and a design review at real
    /// scale is where an eight-pixel disagreement like that shows up.
    cell_inner: u32,
    /// The header's two-line title block.
    title_h: u32,
    /// Section-label band — [`ip::section_h`].
    label_h: u32,
    /// Note / field / hint band.
    note_h: u32,
    /// The designed empty state's plate.
    empty_h: u32,
    /// An engraved rule: groove plus highlight, one bevel each.
    rule_h: u32,
    /// Left interior edge of the glass.
    glass_x: i32,
    width: u32,
}

impl Metrics {
    fn new(tile: u32) -> Metrics {
        let tile = tile.max(8);
        let rh = row_h(tile);
        let step = ((rh as f32) * 0.22).round().max(3.0) as i32;
        Metrics {
            rh,
            step,
            gutter: step * 2,
            cell: ((rh as f32) * 1.60).round() as u32,
            cell_inner: (((rh as f32) * 1.60).round() as u32).saturating_sub(step as u32),
            title_h: ((rh as f32) * 1.85).round() as u32,
            label_h: ip::section_h(rh),
            note_h: ((rh as f32) * 0.80).round() as u32,
            empty_h: ((rh as f32) * 1.90).round() as u32,
            rule_h: 2,
            glass_x: GLASS_INSET,
            width: tile * TILES_WIDE,
        }
    }
}

/// The resolved type ramp for one render. Held together so no call
/// site can quietly set a readout in row ink and flatten the
/// hierarchy again.
struct Ramp {
    /// The header's link name: the readout step at half again a row's
    /// height — the one thing that outranks the stack.
    title: PanelFont,
    /// Row names: the panel's body.
    row: PanelFont,
    /// The verdict a row is for: `LINKED`, `UP`, `BUSY`.
    readout: PanelFont,
    /// The tracked, dim step: field labels, the header's nature line,
    /// kind tags.
    micro: PanelFont,
    /// A reading in the note bands.
    note: PanelFont,
    /// The connection kind, in the left cell. The section step's size
    /// and ink, but untracked: tracking is the cue that says "this
    /// names a band", and on a three-letter datum it only costs the
    /// `N` of `VPN`.
    tag: PanelFont,
}

impl Ramp {
    fn new(style: &PanelStyle, m: &Metrics) -> Ramp {
        Ramp {
            title: style.typeface(TypeRole::Readout, m.rh * 3 / 2),
            row: style.typeface(TypeRole::Row, m.rh),
            readout: style.typeface(TypeRole::Readout, m.rh),
            micro: style.typeface(TypeRole::Section, m.label_h * 2),
            note: style.typeface(TypeRole::Row, m.rh),
            tag: PanelFont { tracking: 0.0, ..style.typeface(TypeRole::Section, m.label_h * 2) },
        }
    }
}

/// Places every band of `view` at `tile` scale. The one geometry
/// authority — see the module doc.
pub fn panel_layout(view: &PanelView, tile: u32) -> PanelLayout {
    let m = Metrics::new(tile);
    let glass_w = (m.width as i32 - m.glass_x * 2).max(0) as u32;

    let mut bands = Vec::new();
    let mut y = m.step * 2;

    // The title block, closed by its own rule: the header outranks
    // every section, so it gets a divider of its own rather than
    // sharing one with the first label.
    bands.push((Band::Header, y, m.title_h));
    y += m.title_h as i32 + m.step;
    bands.push((Band::Rule, y, m.rule_h));
    y += m.rule_h as i32 + m.step * 2;

    let section = |bands: &mut Vec<(Band, i32, u32)>, y: &mut i32, label: &'static str| {
        bands.push((Band::Label(label), *y, m.label_h));
        *y += m.label_h as i32 + m.step;
    };

    if !view.connections.is_empty() {
        section(&mut bands, &mut y, "CONNECTIONS");
        for conn in &view.connections {
            bands.push((Band::Row(RowKey::Conn(conn.uuid.clone())), y, m.rh));
            y += m.rh as i32;
        }
        y += m.step * 3;
    }

    section(&mut bands, &mut y, "WI-FI NETWORKS");
    match &view.wifi {
        WifiSection::NoHardware => {
            bands.push((Band::Empty("NO WI-FI HARDWARE"), y, m.empty_h));
            y += m.empty_h as i32;
        }
        WifiSection::Networks(nets) => {
            if nets.is_empty() {
                bands.push((Band::Empty("NO NETWORKS IN RANGE"), y, m.empty_h));
                y += m.empty_h as i32;
            }
            for net in nets.iter().take(MAX_WIFI_ROWS) {
                bands.push((Band::Row(RowKey::Net(net.ssid.clone())), y, m.rh));
                y += m.rh as i32;
            }
        }
    }
    y += m.step * 3;

    if let Some(ts) = &view.tailscale {
        section(&mut bands, &mut y, "TAILSCALE");
        bands.push((Band::Row(RowKey::Tailscale), y, m.rh));
        y += m.rh as i32;
        if let OperatorState::NeedsOperator { hint } = &ts.operator {
            bands.push((Band::Hint(hint.clone()), y, m.note_h));
            y += m.note_h as i32;
        }
        if let Some(exit) = exit_node_field(ts) {
            bands.push((Band::Field("EXIT NODE", exit), y, m.note_h));
            y += m.note_h as i32;
        }
        for note in tailscale_notes(ts) {
            bands.push((Band::Note(note), y, m.note_h));
            y += m.note_h as i32;
        }
        y += m.step * 3;
    }

    if matches!(view.wifi, WifiSection::Networks(_)) {
        // The footer: the panel's one command, under a rule of its
        // own so it reads as an action rather than one more reading.
        bands.push((Band::Rule, y, m.rule_h));
        y += m.rule_h as i32 + m.step * 2;
        bands.push((Band::Row(RowKey::Rescan), y, m.rh));
    }

    // The bottom margin matches the top one, measured from the last
    // band that actually drew something — sections carry a trailing gap
    // for whatever follows them, and when nothing does, that gap must
    // not become a hole at the foot of the panel.
    let content_bottom = bands.iter().map(|(_, y, h)| y + *h as i32).max().unwrap_or(0);
    let height = (content_bottom + m.step * 2) as u32;
    PanelLayout { width: m.width, height, bands, glass_x: m.glass_x, glass_w }
}

/// The exit node as a labelled reading, or `None` when traffic is not
/// leaving through one — which is the ordinary case, and a field that
/// says "NONE" every day is furniture, not a reading.
///
/// This is where the unexplained `…` used to hang off the tailnet
/// row's right edge. An ellipsis is a promise of a menu; the panel has
/// no menu to open, so what it owes the reader is the fact — and the
/// fact worth a line is not *that* exit nodes exist but that one is
/// carrying your traffic, plus whether it is still answering. An exit
/// node that has gone offline is the state that actually breaks
/// browsing, so it is the one the field spells out.
fn exit_node_field(ts: &TailscaleRow) -> Option<String> {
    let status = ts.status.as_ref()?;
    if status.backend != BackendState::Running {
        return None;
    }
    let node = status.exit_node.as_ref()?.to_uppercase();
    Some(if status.exit_node_online { node } else { format!("{node} · OFFLINE") })
}

/// The dim informational lines under the tailnet row, most urgent
/// first, capped at two so the panel stays an instrument rather than
/// a log viewer. The exit node is not among them — it is a field of
/// its own, see [`exit_node_field`].
fn tailscale_notes(ts: &TailscaleRow) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(status) = &ts.status {
        if status.backend == BackendState::Running && !status.self_online {
            notes.push("OFFLINE".to_string());
        }
        if !status.health.is_empty() {
            let mut warning = status.health[0].to_uppercase();
            if status.health.len() > 1 {
                warning = format!("{} (+{})", warning, status.health.len() - 1);
            }
            notes.push(warning);
        }
    }
    notes.truncate(2);
    notes
}

/// A hard-edged padlock: shackle over body, LED-sized.
fn draw_lock(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, size: u32, color: Color) {
    let s = size.max(5);
    let body_h = s * 3 / 5;
    let body_y = y + (s - body_h) as i32;
    let shackle_w = (s * 3 / 5).max(3);
    let shackle_x = x + ((s - shackle_w) / 2) as i32;
    let t = (s / 5).max(1);
    // Shackle: two posts and a lintel.
    paint::fill_rect(pixmap, shackle_x, y, t, s - body_h, color);
    paint::fill_rect(pixmap, shackle_x + (shackle_w - t) as i32, y, t, s - body_h, color);
    paint::fill_rect(pixmap, shackle_x, y, shackle_w, t.min(s - body_h), color);
    paint::fill_rect(pixmap, x, body_y, s, body_h, color);
}

fn kind_tag(kind: ConnKind) -> &'static str {
    match kind {
        ConnKind::Ethernet => "E",
        ConnKind::Wifi => "W",
        ConnKind::WireGuard => "WG",
        ConnKind::Vpn => "VPN",
    }
}

/// Whether a row would actually do something if clicked. The renderer
/// needs this and only this from the action table: an inert row keeps
/// [`RowState::Disabled`]'s flat ground even under the pointer, so the
/// panel never offers a press it will swallow. (The table itself lives
/// in `LinkPanel::activate`; this mirrors its refusals, and a test
/// pins the two together.)
fn row_enabled(view: &PanelView, key: &RowKey) -> bool {
    match key {
        RowKey::Conn(uuid) => view.connections.iter().any(|c| &c.uuid == uuid && c.lamp != Lamp::Pending),
        RowKey::Net(ssid) => match &view.wifi {
            WifiSection::Networks(nets) => nets.iter().any(|n| &n.ssid == ssid && !n.in_use && !n.pending),
            WifiSection::NoHardware => false,
        },
        RowKey::Tailscale => match &view.tailscale {
            Some(ts) => {
                !ts.pending
                    && !matches!(ts.operator, OperatorState::NeedsOperator { .. })
                    && matches!(ts.status.as_ref().map(|s| s.backend), Some(BackendState::Running) | Some(BackendState::Stopped))
            }
            None => false,
        },
        RowKey::Rescan => !view.rescan_cooling,
    }
}

/// What the ground under a row should do. `Disabled` wins over hover
/// on purpose: a row that cannot act must not lift when the pointer
/// crosses it, because a lift is a promise.
fn row_state(view: &PanelView, key: &RowKey) -> RowState {
    if view.pressed.as_ref() == Some(key) && row_enabled(view, key) {
        return RowState::Pressed;
    }
    if !row_enabled(view, key) {
        return RowState::Disabled;
    }
    if view.hover.as_ref() == Some(key) {
        return RowState::Hover;
    }
    RowState::Idle
}

/// Renders the whole panel at its natural size. The returned buffer's
/// size is the layout's — the size [`LinkPanel::spec`] asks the shell
/// for, and what the byte-stability tests render.
///
/// [`LinkPanel::spec`]: super::LinkPanel::spec
pub fn render_link_panel(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile_size: u32,
    view: &PanelView,
) -> DecorationBuffer {
    let layout = panel_layout(view, tile_size);
    let (w, h) = (layout.width, layout.height);
    render_layout(theme, fonts, swash, tile_size, view, layout, w, h)
}

/// Renders into the frame size the shell granted — the entry point the
/// `DockWidget::render_panel` path uses, because the grant, not the
/// request, is what [`PanelFrame::adopt`] will accept.
///
/// The grant is usually the request (the panel is small and the
/// workarea beside the dock is not), so this is usually
/// [`render_link_panel`] exactly. When it is not — a short monitor, a
/// stale grant mid-restyle — the content keeps its laid-out metrics
/// and the frame clips it, per [`PanelLayout::fitted`].
///
/// [`PanelFrame::adopt`]: chonk_dock_widget::PanelFrame::adopt
#[allow(clippy::too_many_arguments)]
pub fn render_link_panel_into(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile_size: u32,
    view: &PanelView,
    width: u32,
    height: u32,
) -> DecorationBuffer {
    let layout = panel_layout(view, tile_size).fitted(width, height);
    render_layout(theme, fonts, swash, tile_size, view, layout, width, height)
}

#[allow(clippy::too_many_arguments)]
fn render_layout(
    theme: &Theme,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    tile_size: u32,
    view: &PanelView,
    layout: PanelLayout,
    width: u32,
    height: u32,
) -> DecorationBuffer {
    let mut pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1)).expect("nonzero panel size");
    let style = PanelStyle::new(theme);
    let m = Metrics::new(tile_size);
    let ramp = Ramp::new(&style, &m);

    // The shared ground: gasket, well and glass, exactly as the audio
    // and bluetooth panels lay theirs.
    let (gx, gy, gw, gh) = ip::draw_panel_ground(&mut pixmap, 0, 0, layout.width, layout.height, theme);
    let _ = (gy, gh);

    // The text column: inset from the glass on both sides, while row
    // highlights still span the glass edge to edge. Content breathes;
    // the pointer target does not shrink.
    let tx = gx + m.gutter;
    let tw = gw.saturating_sub((m.gutter * 2) as u32);

    for (band, y, h) in &layout.bands {
        let (y, h) = (*y, *h);
        // A grant shorter than the content shows what fits. Drawing
        // the rest would only scribble under the bevel, and `row_at`
        // has already stopped offering these rows to the pointer.
        if y >= layout.height as i32 {
            continue;
        }
        match band {
            Band::Header => draw_header(&mut pixmap, fonts, swash, &style, &ramp, &view.header, tx, y, tw, &m),
            Band::Rule => ip::draw_engraved_rule(&mut pixmap, tx, y, tw, &style),
            Band::Label(text) => ip::draw_section_header(&mut pixmap, fonts, swash, &style, text, tx, y, tw, h),
            Band::Row(key) => {
                let state = row_state(view, key);
                ip::draw_row_ground(&mut pixmap, gx, y, gw, h, &style, state);
                // The classic press: content sinks one pixel inside
                // the chisel the ground already took.
                let dy = if state == RowState::Pressed { 1 } else { 0 };
                draw_row(&mut pixmap, fonts, swash, &style, &ramp, view, key, tx, y + dy, tw, h, state, &m);
            }
            Band::Field(name, value) => {
                let x = tx + m.cell as i32;
                let name_w = ip::type_width(fonts, &ramp.micro, name);
                ip::draw_type(&mut pixmap, fonts, swash, &ramp.micro, name, x, y, name_w, h, TextAlign::Left);
                let value_x = x + name_w as i32 + m.gutter;
                let value_w = ((tx + tw as i32) - value_x).max(0) as u32;
                let value = ip::fit_type(fonts, &ramp.readout, value, value_w);
                ip::draw_type(&mut pixmap, fonts, swash, &ramp.readout, &value, value_x, y, value_w, h, TextAlign::Left);
            }
            Band::Note(note) => {
                // Notes hang off the same left cell the rows use, with
                // a dim tick where a lamp would be: they belong to the
                // row above, and the indent says so.
                let tick = (m.step / 2).max(2) as u32;
                paint::fill_rect(&mut pixmap, tx + (m.cell / 2) as i32, y + (h / 2) as i32, tick, 1, style.pal.ink_dim);
                let x = tx + m.cell as i32;
                let w = (tw as i32 - m.cell as i32).max(0) as u32;
                let face = ramp.note.clone().receded(&style);
                let text = ip::fit_type(fonts, &face, note, w);
                ip::draw_type(&mut pixmap, fonts, swash, &face, &text, x, y, w, h, TextAlign::Left);
            }
            Band::Hint(hint) => {
                let x = tx + m.cell as i32;
                let w = (tw as i32 - m.cell as i32).max(0) as u32;
                let text = ip::fit_type(fonts, &ramp.note, hint, w);
                ip::draw_type(&mut pixmap, fonts, swash, &ramp.note, &text, x, y, w, h, TextAlign::Left);
            }
            Band::Empty(legend) => draw_empty_state(&mut pixmap, fonts, swash, &style, &ramp, legend, tx, y, tw, h, &m),
        }
    }

    // The pixmap's own size, not the layout's: a zero-sized grant is
    // clamped to 1px above, and a buffer whose header disagreed with
    // its payload is exactly what `PanelFrame::adopt` refuses.
    DecorationBuffer { width: pixmap.width(), height: pixmap.height(), pixels: pixmap.data().to_vec() }
}

#[allow(clippy::too_many_arguments)]
fn draw_header(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    ramp: &Ramp,
    header: &LinkHeader,
    x: i32,
    y: i32,
    w: u32,
    m: &Metrics,
) {
    let (name, detail) = match header {
        LinkHeader::Unknown => ("NO LINK".to_string(), "SENSING".to_string()),
        LinkHeader::Wifi { ssid, signal } => (ssid.to_uppercase(), format!("WIFI · {}%", signal.min(&100))),
        LinkHeader::Wired { interface, speed_mbps } => (
            interface.to_uppercase(),
            match speed_mbps {
                Some(speed) if *speed >= 10_000 => format!("ETHERNET · {}G", speed / 1000),
                Some(speed) => format!("ETHERNET · {speed}M"),
                None => "ETHERNET".to_string(),
            },
        ),
        LinkHeader::Down { interface } => (interface.to_uppercase(), "DOWN".to_string()),
    };
    let name_h = ((m.rh as f32) * 1.05).round() as u32;
    let detail_h = m.title_h.saturating_sub(name_h);

    // Wifi gets the signal meter at the right of the title line — the
    // shared meter, so the header's level and a row's level are the
    // same instrument.
    let mut name_w = w;
    if let LinkHeader::Wifi { signal, .. } = header {
        let meter_w = ((m.rh as f32) * 2.2).round() as u32;
        let meter_h = ip::meter_h(m.rh * 3 / 2);
        let meter_y = y + (name_h as i32 - meter_h as i32) / 2;
        ip::draw_meter(
            pixmap,
            x + (w - meter_w) as i32,
            meter_y,
            meter_w,
            meter_h,
            style,
            *signal as f32 / 100.0,
            MeterGlow::Active,
        );
        name_w = w.saturating_sub(meter_w + m.gutter as u32);
    }
    let title = match header {
        LinkHeader::Down { .. } | LinkHeader::Unknown => ramp.title.clone().receded(style),
        _ => ramp.title.clone(),
    };
    let name = ip::fit_type(fonts, &title, &name, name_w);
    ip::draw_type(pixmap, fonts, swash, &title, &name, x, y, name_w, name_h, TextAlign::Left);
    ip::draw_type(pixmap, fonts, swash, &ramp.micro, &detail, x, y + name_h as i32, w, detail_h, TextAlign::Left);
}

/// The designed empty state: a socket milled into the glass with a
/// dead meter in it (every segment a ghost — the instrument is there,
/// it just has nothing to show) and a tracked legend beside it. The
/// SDK's `render_dead_tile` says the same thing at tile size; this is
/// that idea at row size, and it is what a section says instead of
/// vanishing or dropping a sentence of body text where a row belongs.
#[allow(clippy::too_many_arguments)]
fn draw_empty_state(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    ramp: &Ramp,
    legend: &str,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    m: &Metrics,
) {
    let plate_h = h.saturating_sub(m.step as u32).max(1);
    let plate_y = y + (h as i32 - plate_h as i32) / 2;
    // The socket: the glass cut down and chiseled, the same sunken
    // face a pressed row takes — a recess with nothing seated in it.
    paint::op_rect(pixmap, x, plate_y, w, plate_h, -16);
    paint::draw_sunken_bevel(pixmap, x, plate_y, w, plate_h, style.bevel);

    let meter_w = ((m.rh as f32) * 2.2).round() as u32;
    let meter_h = ip::meter_h(m.rh * 3 / 2);
    let face = ramp.micro.clone().colored(style.pal.ink_dim);
    let text_w = ip::type_width(fonts, &face, legend);
    let group = meter_w + m.gutter as u32 + text_w;
    let group_x = x + ((w.saturating_sub(group)) / 2) as i32;
    ip::draw_meter(
        pixmap,
        group_x,
        plate_y + (plate_h as i32 - meter_h as i32) / 2,
        meter_w,
        meter_h,
        style,
        0.0,
        MeterGlow::Silent,
    );
    ip::draw_type(
        pixmap,
        fonts,
        swash,
        &face,
        legend,
        group_x + (meter_w + m.gutter as u32) as i32,
        plate_y,
        text_w,
        plate_h,
        TextAlign::Left,
    );
}

/// The tailnet row's one-word state. Never an ellipsis: a row that
/// says `…` is either a broken reading or an unlabelled button, and
/// the reader cannot tell which.
fn tailnet_verdict(ts: &TailscaleRow) -> &'static str {
    if matches!(ts.operator, OperatorState::NeedsOperator { .. }) {
        return "LOCKED";
    }
    if ts.pending {
        return "BUSY";
    }
    match ts.status.as_ref().map(|s| s.backend) {
        Some(BackendState::Running) => "UP",
        Some(BackendState::Stopped) => "DOWN",
        Some(BackendState::Starting) => "BUSY",
        Some(BackendState::NeedsLogin) => "LOGIN",
        Some(BackendState::NeedsMachineAuth) => "AUTH",
        Some(BackendState::NoState) | Some(BackendState::Other) => "UNKNOWN",
        // The status sample has not landed yet (or `tailscale status`
        // is taking its time). "Waiting on a reading" is a fact, and
        // the row says it.
        None => "SENSING",
    }
}

/// A row's readout type: full ink when the row is *the* one (linked,
/// up, in flight), receded when it is one of the alternatives.
fn readout_face(ramp: &Ramp, style: &PanelStyle, lit: bool) -> PanelFont {
    if lit {
        ramp.readout.clone()
    } else {
        ramp.readout.clone().receded(style)
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    style: &PanelStyle,
    ramp: &Ramp,
    view: &PanelView,
    key: &RowKey,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    state: RowState,
    m: &Metrics,
) {
    // The shared grid: every row's name starts here, whatever sits in
    // the left cell.
    let name_x = x + m.cell as i32;
    let name_limit = (w as i32 - m.cell as i32).max(0) as u32;
    let lamp = ip::lamp_size(m.rh);
    match key {
        RowKey::Conn(uuid) => {
            let Some(conn) = view.connections.iter().find(|c| &c.uuid == uuid) else { return };
            ip::draw_lamp(pixmap, x + style.bevel as i32, y + ((h - lamp) / 2) as i32, lamp, style, conn.lamp.state());
            // The kind tag closes the left cell, right-aligned against
            // the name column so `VPN` and `E` end on the same pixel.
            // Measured, not boxed: a tag given a box narrower than its
            // own letters does not clip, it *wraps* — which at 1x put
            // the `N` of `VPN` on a second line under the row. Asking
            // for exactly the width the type shapes to is the only
            // width that cannot wrap.
            let tag = kind_tag(conn.kind);
            let tag_w = ip::type_width(fonts, &ramp.tag, tag);
            let tag_x = (x + m.cell_inner as i32 - tag_w as i32).max(x + (lamp + style.bevel * 2) as i32);
            ip::draw_type(pixmap, fonts, swash, &ramp.tag, tag, tag_x, y, tag_w, h, TextAlign::Left);

            let right = match (conn.lamp, conn.external) {
                (Lamp::Pending, _) => "BUSY",
                (_, true) => "EXT",
                _ => "",
            };
            let mut right_w = 0;
            if !right.is_empty() {
                let face = readout_face(ramp, style, conn.lamp != Lamp::Off);
                right_w = ip::type_width(fonts, &face, right) + m.gutter as u32;
                ip::draw_type(pixmap, fonts, swash, &face, right, x, y, w, h, TextAlign::Right);
            }
            let name_w = name_limit.saturating_sub(right_w);
            let face = ramp.row.clone();
            let name = ip::fit_type(fonts, &face, &conn.name.to_uppercase(), name_w);
            ip::draw_type(pixmap, fonts, swash, &face, &name, name_x, y, name_w, h, TextAlign::Left);
        }
        RowKey::Net(ssid) => {
            let nets = match &view.wifi {
                WifiSection::Networks(nets) => nets,
                WifiSection::NoHardware => return,
            };
            let Some(net) = nets.iter().find(|n| &n.ssid == ssid) else { return };
            // The whole left cell, and a track sized to be *counted*
            // rather than glanced at. The first cut gave this 42x9 at
            // tile scale — eight segments in 40 usable pixels, which
            // beside the dock turned five different signal strengths
            // into five identical smears. The header's own meter is
            // half again as tall for the same reason; a level nobody
            // can read is not a reading, and a bare percentage in its
            // place would have left the family.
            let meter_w = m.cell_inner.max(8);
            let meter_h = ip::meter_h(m.rh * 5 / 4);
            // The associated network's level burns at full current;
            // the alternatives are readable but idle. Same meter, two
            // currents — the system's own way of saying "this one".
            let glow = if net.in_use { MeterGlow::Active } else { MeterGlow::Idle };
            ip::draw_meter(pixmap, x, y + ((h - meter_h) / 2) as i32, meter_w, meter_h, style, net.signal as f32 / 100.0, glow);

            let right = if net.pending {
                "BUSY"
            } else if net.in_use {
                "LINKED"
            } else if net.known {
                "SAVED"
            } else if net.secured {
                "JOIN…"
            } else {
                "OPEN"
            };
            let face = readout_face(ramp, style, net.in_use || net.pending);
            ip::draw_type(pixmap, fonts, swash, &face, right, x, y, w, h, TextAlign::Right);
            let mut right_w = ip::type_width(fonts, &face, right) + m.gutter as u32;
            // The padlock sits beside the verdict, not beside the
            // meter: "secured" belongs with what clicking would cost,
            // and keeping it out of the left cell is what lets every
            // name in the panel start on the same column.
            if net.secured {
                let lock = ((m.rh as f32) * 0.46).round().max(5.0) as u32;
                let lock_x = x + w as i32 - right_w as i32 - lock as i32;
                let lit = net.in_use || matches!(state, RowState::Hover | RowState::Pressed);
                let ink = if lit { style.pal.ink_dim } else { style.recede(style.pal.ink_dim) };
                draw_lock(pixmap, lock_x, y + ((h - lock) / 2) as i32, lock, ink);
                right_w += lock + m.step as u32;
            }
            let name_w = name_limit.saturating_sub(right_w);
            let face = ramp.row.clone();
            let name = ip::fit_type(fonts, &face, &net.ssid.to_uppercase(), name_w);
            ip::draw_type(pixmap, fonts, swash, &face, &name, name_x, y, name_w, h, TextAlign::Left);
        }
        RowKey::Tailscale => {
            let Some(ts) = &view.tailscale else { return };
            let lamp_state = if ts.pending {
                Lamp::Pending
            } else {
                match ts.status.as_ref().map(|s| s.backend) {
                    Some(BackendState::Running) => Lamp::On,
                    Some(BackendState::Starting) => Lamp::Pending,
                    _ => Lamp::Off,
                }
            };
            ip::draw_lamp(pixmap, x + style.bevel as i32, y + ((h - lamp) / 2) as i32, lamp, style, lamp_state.state());

            let right = tailnet_verdict(ts);
            let face = readout_face(ramp, style, lamp_state == Lamp::On);
            ip::draw_type(pixmap, fonts, swash, &face, right, x, y, w, h, TextAlign::Right);
            let right_w = ip::type_width(fonts, &face, right) + m.gutter as u32;
            let name_face = ramp.row.clone();
            // "TAILNET", not "TAILSCALE": the section label directly
            // above already says Tailscale, and a row that repeats its
            // own heading spends a line saying nothing. This one names
            // the thing the lamp is about.
            ip::draw_type(
                pixmap,
                fonts,
                swash,
                &name_face,
                "TAILNET",
                name_x,
                y,
                name_limit.saturating_sub(right_w),
                h,
                TextAlign::Left,
            );
        }
        RowKey::Rescan => {
            // The one row that is a *command* rather than a reading,
            // so it is drawn as a key milled into the glass — the
            // shared `draw_key_cell`, which is the same control the
            // audio panel's mute wears.
            let (text, lit) = if view.rescan_cooling { ("RESCAN · WAIT", false) } else { ("RESCAN", true) };
            let face = readout_face(ramp, style, lit);
            let label_w = ip::type_width(fonts, &face, text);
            let key_w = (label_w + m.rh * 2).min(w);
            let key_h = ip::hit_size(h.saturating_sub(m.step as u32 / 2));
            let key_x = x + ((w.saturating_sub(key_w)) / 2) as i32;
            let key_y = y + (h as i32 - key_h as i32) / 2;
            ip::draw_key_cell(pixmap, key_x, key_y, key_w, key_h, style, state);
            ip::draw_type(pixmap, fonts, swash, &face, text, key_x, key_y, key_w, key_h, TextAlign::Center);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme::default_theme::{all_themes, nextstep_classic};
    use wm_theme::model::Color;
    use wm_theme::panel::panel_palette;

    fn ctx() -> (FontSystem, SwashCache) {
        (FontSystem::new(), SwashCache::new())
    }

    fn conn(uuid: &str, name: &str, kind: ConnKind, lamp: Lamp) -> ConnRow {
        ConnRow { uuid: uuid.into(), name: name.into(), kind, lamp, external: false }
    }

    fn net(ssid: &str, signal: u8, secured: bool, known: bool, in_use: bool) -> NetRow {
        NetRow { ssid: ssid.into(), signal, secured, known, in_use, pending: false }
    }

    fn full_view() -> PanelView {
        PanelView {
            header: LinkHeader::Wifi { ssid: "HomeBase".into(), signal: 87 },
            connections: vec![
                conn("uuid-eth", "Wired connection 1", ConnKind::Ethernet, Lamp::On),
                conn("uuid-wg", "wg-home", ConnKind::WireGuard, Lamp::Off),
                conn("uuid-vpn", "office-vpn", ConnKind::Vpn, Lamp::Pending),
            ],
            wifi: WifiSection::Networks(vec![
                net("HomeBase", 87, true, true, true),
                net("Cafe", 61, true, false, false),
                net("OpenMesh", 52, false, false, false),
            ]),
            tailscale: Some(TailscaleRow {
                status: Some(TailscaleStatus {
                    backend: BackendState::Running,
                    self_online: true,
                    exit_node: None,
                    exit_node_online: false,
                    health: vec![],
                }),
                operator: OperatorState::Unknown,
                pending: false,
            }),
            rescan_cooling: false,
            hover: None,
            pressed: None,
        }
    }

    #[test]
    fn rendering_is_byte_stable() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let view = full_view();
        let a = render_link_panel(&theme, &mut fs, &mut sc, 56, &view);
        let b = render_link_panel(&theme, &mut fs, &mut sc, 56, &view);
        assert_eq!(a.pixels, b.pixels, "the same view must always produce the same bytes");
        assert_eq!((a.width, a.height), (224, a.height));
    }

    #[test]
    fn layout_and_buffer_agree_on_size() {
        let (mut fs, mut sc) = ctx();
        // Every theme, because bevel width and item font come from the
        // theme and both feed the layout's metrics — a palette whose
        // chisel is a pixel wider must not desync the buffer from the
        // geometry the hit test reads.
        for tile in [40u32, 56, 112] {
            for theme in all_themes() {
                let view = full_view();
                let layout = panel_layout(&view, tile);
                let buffer = render_link_panel(&theme, &mut fs, &mut sc, tile, &view);
                assert_eq!((buffer.width, buffer.height), (layout.width, layout.height), "theme {} at tile {tile}", theme.id);
                assert_eq!(buffer.pixels.len(), (layout.width * layout.height * 4) as usize);
            }
        }
    }

    #[test]
    fn every_interactive_row_is_hit_testable_and_disjoint() {
        let view = full_view();
        let layout = panel_layout(&view, 56);
        let expect = [
            RowKey::Conn("uuid-eth".into()),
            RowKey::Conn("uuid-wg".into()),
            RowKey::Conn("uuid-vpn".into()),
            RowKey::Net("HomeBase".into()),
            RowKey::Net("Cafe".into()),
            RowKey::Net("OpenMesh".into()),
            RowKey::Tailscale,
            RowKey::Rescan,
        ];
        let mut seen = Vec::new();
        for y in 0..layout.height as i32 {
            if let Some(key) = layout.row_at(layout.width as i32 / 2, y) {
                if seen.last() != Some(&key) {
                    seen.push(key);
                }
            }
        }
        assert_eq!(seen, expect, "rows appear once each, in visual order");
        assert_eq!(layout.row_at(0, layout.height as i32 / 2), None, "the frame is not a row");
        assert_eq!(layout.row_at(layout.width as i32 / 2, -5), None);
    }

    #[test]
    fn hover_and_press_change_the_bytes_of_that_row_only_when_present() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let plain = render_link_panel(&theme, &mut fs, &mut sc, 56, &full_view());
        let mut hovered = full_view();
        hovered.hover = Some(RowKey::Tailscale);
        let hover_face = render_link_panel(&theme, &mut fs, &mut sc, 56, &hovered);
        assert_ne!(plain.pixels, hover_face.pixels, "hover must be visible");
        let mut pressed = hovered.clone();
        pressed.pressed = Some(RowKey::Tailscale);
        let pressed_face = render_link_panel(&theme, &mut fs, &mut sc, 56, &pressed);
        assert_ne!(hover_face.pixels, pressed_face.pixels, "press must be visible over hover");
    }

    #[test]
    fn the_designed_states_are_pairwise_distinct() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let mut variants: Vec<PanelView> = Vec::new();
        variants.push(full_view());
        let mut v = full_view();
        v.header = LinkHeader::Wired { interface: "eno1".into(), speed_mbps: Some(1000) };
        variants.push(v);
        let mut v = full_view();
        v.wifi = WifiSection::NoHardware;
        variants.push(v);
        let mut v = full_view();
        v.tailscale = None;
        variants.push(v);
        let mut v = full_view();
        if let Some(ts) = &mut v.tailscale {
            ts.operator = OperatorState::NeedsOperator { hint: "Use 'sudo tailscale set --operator=chris'".into() };
        }
        variants.push(v);
        let mut v = full_view();
        v.rescan_cooling = true;
        variants.push(v);
        let faces: Vec<Vec<u8>> = variants.iter().map(|v| render_link_panel(&theme, &mut fs, &mut sc, 56, v).pixels).collect();
        for a in 0..faces.len() {
            for b in (a + 1)..faces.len() {
                assert_ne!(faces[a], faces[b], "views {a} and {b} rendered identically");
            }
        }
    }

    #[test]
    fn needs_operator_swaps_the_toggle_face_for_the_remedy() {
        let mut view = full_view();
        if let Some(ts) = &mut view.tailscale {
            ts.operator = OperatorState::NeedsOperator { hint: "Use 'sudo tailscale set --operator=chris'".into() };
        }
        let layout = panel_layout(&view, 56);
        let hint_bands = layout.bands.iter().filter(|(band, ..)| matches!(band, Band::Hint(_))).count();
        assert_eq!(hint_bands, 1, "the remedy line must be laid out");
    }

    #[test]
    fn the_wifi_row_cap_holds() {
        let mut view = full_view();
        let many: Vec<NetRow> = (0..20).map(|i| net(&format!("NET{i:02}"), 90 - i, true, false, false)).collect();
        view.wifi = WifiSection::Networks(many);
        let layout = panel_layout(&view, 56);
        let net_rows = layout.bands.iter().filter(|(band, ..)| matches!(band, Band::Row(RowKey::Net(_)))).count();
        assert_eq!(net_rows, MAX_WIFI_ROWS);
    }

    #[test]
    fn every_theme_renders_a_substantial_glass_in_both_appearances() {
        let (mut fs, mut sc) = ctx();
        let view = full_view();
        for theme in all_themes() {
            let pal = panel_palette(&theme);
            let buffer = render_link_panel(&theme, &mut fs, &mut sc, 56, &view);
            let glass = buffer
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| (p[0], p[1], p[2]) == (pal.glass.r, pal.glass.g, pal.glass.b))
                .count();
            assert!(glass > 2000, "theme {}: expected a substantial glass area, found {glass}", theme.id);
        }
    }

    #[test]
    fn a_grant_that_matches_the_request_renders_exactly_the_natural_face() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let view = full_view();
        let natural = render_link_panel(&theme, &mut fs, &mut sc, 56, &view);
        let granted = render_link_panel_into(&theme, &mut fs, &mut sc, 56, &view, natural.width, natural.height);
        assert_eq!(natural.pixels, granted.pixels, "the usual case must not be a second code path");
    }

    #[test]
    fn a_grant_is_obeyed_to_the_byte_in_either_direction() {
        let theme = nextstep_classic();
        let (mut fs, mut sc) = ctx();
        let view = full_view();
        let natural = panel_layout(&view, 56);
        for (w, h) in [(natural.width, natural.height / 2), (natural.width + 60, natural.height + 90), (natural.width, natural.height)] {
            let buffer = render_link_panel_into(&theme, &mut fs, &mut sc, 56, &view, w, h);
            assert_eq!((buffer.width, buffer.height), (w, h), "the grant, not the request, sizes the frame");
            assert_eq!(buffer.pixels.len(), (w * h * 4) as usize, "a buffer whose header lies is refused by adopt");
        }
    }

    #[test]
    fn a_short_grant_clips_the_tail_and_stops_offering_it_to_the_pointer() {
        let view = full_view();
        let natural = panel_layout(&view, 56);
        // Tall enough for the header and the first connections, short
        // enough to lose the tailnet row and the rescan button.
        let short = panel_layout(&view, 56).fitted(natural.width, natural.height / 2);
        let mut reachable = Vec::new();
        for y in 0..natural.height as i32 {
            if let Some(key) = short.row_at(short.width as i32 / 2, y) {
                if reachable.last() != Some(&key) {
                    reachable.push(key);
                }
            }
        }
        assert!(!reachable.is_empty(), "a short grant still shows something");
        assert!(!reachable.contains(&RowKey::Rescan), "a row past the glass must not answer clicks");
        assert!(!reachable.contains(&RowKey::Tailscale));
        assert!(reachable.contains(&RowKey::Conn("uuid-eth".into())), "and the rows that did fit still work");
    }

    #[test]
    fn a_wide_grant_widens_the_hit_zone_with_the_glass() {
        let view = full_view();
        let natural = panel_layout(&view, 56);
        let target = natural.bands.iter().find_map(|(b, y, _)| match b {
            Band::Row(RowKey::Tailscale) => Some(*y + 1),
            _ => None,
        });
        let y = target.expect("the tailnet row is laid out");
        let wide = panel_layout(&view, 56).fitted(natural.width + 80, natural.height);
        assert_eq!(wide.row_at(natural.width as i32 + 20, y), Some(RowKey::Tailscale), "the row spans the granted glass");
        assert_eq!(natural.row_at(natural.width as i32 + 20, y), None, "which the natural layout would not have reached");
    }

    /// Both empty cases are the *designed* plate, not a line of body
    /// text: the section keeps its label and shows a dead meter with a
    /// legend, so an absent radio and a quiet one look like the same
    /// instrument saying two different things.
    #[test]
    fn an_empty_scan_list_says_so_rather_than_showing_nothing() {
        let mut view = full_view();
        view.wifi = WifiSection::Networks(vec![]);
        let layout = panel_layout(&view, 56);
        assert!(
            layout.bands.iter().any(|(band, ..)| matches!(band, Band::Empty("NO NETWORKS IN RANGE"))),
            "an empty list is a designed state"
        );
        let mut view = full_view();
        view.wifi = WifiSection::NoHardware;
        let layout = panel_layout(&view, 56);
        assert!(
            layout.bands.iter().any(|(band, ..)| matches!(band, Band::Empty("NO WI-FI HARDWARE"))),
            "a missing radio is a designed state too, not a sentence where a row goes"
        );
        assert!(
            layout.bands.iter().any(|(band, ..)| matches!(band, Band::Label("WI-FI NETWORKS"))),
            "and the section keeps its heading either way"
        );
    }

    /// The bare `…` that used to hang off the tailnet row's right edge
    /// promised a menu the panel does not have. Every state the row can
    /// be in now says a word, and the exit node — the only choice that
    /// ellipsis could plausibly have meant — is a labelled field.
    #[test]
    fn the_tailnet_row_never_says_ellipsis() {
        let states = [
            (None, OperatorState::Unknown, false),
            (Some(BackendState::Running), OperatorState::Unknown, false),
            (Some(BackendState::Stopped), OperatorState::Unknown, false),
            (Some(BackendState::Starting), OperatorState::Unknown, false),
            (Some(BackendState::NeedsLogin), OperatorState::Unknown, false),
            (Some(BackendState::NeedsMachineAuth), OperatorState::Unknown, false),
            (Some(BackendState::NoState), OperatorState::Unknown, false),
            (Some(BackendState::Other), OperatorState::Unknown, false),
            (Some(BackendState::Running), OperatorState::NeedsOperator { hint: "x".into() }, false),
            (Some(BackendState::Stopped), OperatorState::Unknown, true),
        ];
        for (backend, operator, pending) in states {
            let row = TailscaleRow {
                status: backend.map(|backend| TailscaleStatus {
                    backend,
                    self_online: true,
                    exit_node: None,
                    exit_node_online: false,
                    health: vec![],
                }),
                operator,
                pending,
            };
            let verdict = tailnet_verdict(&row);
            assert!(!verdict.contains('\u{2026}'), "{backend:?} rendered as an ellipsis");
            assert!(verdict.chars().all(|c| c.is_ascii_uppercase()), "{backend:?} said {verdict:?}");
        }
    }

    /// The exit node reads as a fact with a name on it, and says
    /// nothing at all when traffic is not leaving through one.
    #[test]
    fn the_exit_node_is_a_labelled_field_or_absent() {
        let with = |exit: Option<&str>, online: bool, backend: BackendState| TailscaleRow {
            status: Some(TailscaleStatus {
                backend,
                self_online: true,
                exit_node: exit.map(str::to_string),
                exit_node_online: online,
                health: vec![],
            }),
            operator: OperatorState::Unknown,
            pending: false,
        };
        assert_eq!(exit_node_field(&with(Some("100.64.0.5"), true, BackendState::Running)).as_deref(), Some("100.64.0.5"));
        assert_eq!(
            exit_node_field(&with(Some("gateway"), false, BackendState::Running)).as_deref(),
            Some("GATEWAY \u{b7} OFFLINE"),
            "an exit node that stopped answering is the state worth spelling out"
        );
        assert_eq!(exit_node_field(&with(None, false, BackendState::Running)), None, "no exit node, no field");
        assert_eq!(
            exit_node_field(&with(Some("gateway"), true, BackendState::Stopped)),
            None,
            "a stopped tailnet routes nothing"
        );

        let mut view = full_view();
        if let Some(ts) = &mut view.tailscale {
            ts.status = with(Some("gateway"), true, BackendState::Running).status;
        }
        let layout = panel_layout(&view, 56);
        assert!(
            layout.bands.iter().any(|(band, ..)| matches!(band, Band::Field("EXIT NODE", value) if value == "GATEWAY")),
            "the active exit node gets a field of its own"
        );
    }

    /// The renderer's idea of "this row would do something" has to
    /// match the action table's, or a hovered row promises a press that
    /// gets swallowed. Every row the full view offers is live; the
    /// states the table refuses take the disabled ground.
    #[test]
    fn the_disabled_ground_tracks_the_action_table() {
        let view = full_view();
        for key in [RowKey::Conn("uuid-eth".into()), RowKey::Net("Cafe".into()), RowKey::Tailscale, RowKey::Rescan] {
            assert!(row_enabled(&view, &key), "{key:?} should be actionable in the full view");
        }
        assert!(!row_enabled(&view, &RowKey::Net("HomeBase".into())), "the associated network is a fact, not a control");
        assert!(!row_enabled(&view, &RowKey::Conn("uuid-vpn".into())), "a pending toggle takes no second click");
        let mut cooling = full_view();
        cooling.rescan_cooling = true;
        assert!(!row_enabled(&cooling, &RowKey::Rescan));
        let mut locked = full_view();
        if let Some(ts) = &mut locked.tailscale {
            ts.operator = OperatorState::NeedsOperator { hint: "x".into() };
        }
        assert!(!row_enabled(&locked, &RowKey::Tailscale));

        // And a hover over an inert row must not lift its ground: a
        // lift is a promise.
        let mut hovered = full_view();
        hovered.hover = Some(RowKey::Net("HomeBase".into()));
        assert_eq!(row_state(&hovered, &RowKey::Net("HomeBase".into())), RowState::Disabled);
        hovered.hover = Some(RowKey::Net("Cafe".into()));
        assert_eq!(row_state(&hovered, &RowKey::Net("Cafe".into())), RowState::Hover);
    }

    /// Hierarchy, as a number: the three type steps this panel asks the
    /// shared system for really are distinct sizes in that order, and
    /// the ramp's inks are distinct brightnesses. The first cut failed
    /// exactly here — everything at one size in one red — so the ramp
    /// is pinned rather than eyeballed.
    #[test]
    fn the_type_and_ink_ramps_are_ramps() {
        let style = PanelStyle::new(&nextstep_classic());
        for tile in [40u32, 56, 112] {
            let m = Metrics::new(tile);
            let ramp = Ramp::new(&style, &m);
            assert!(ramp.title.spec.size > ramp.readout.spec.size, "tile {tile}: the header must outrank a readout");
            assert!(ramp.readout.spec.size > ramp.row.spec.size, "tile {tile}: a verdict outranks a name");
            assert!(ramp.row.spec.size > ramp.micro.spec.size, "tile {tile}: a name outranks a label");
            assert!(ramp.micro.tracking > 0.0, "tile {tile}: the label step is the tracked one");
            assert_eq!(ramp.row.tracking, 0.0, "body text is not tracked");
        }
        for theme in all_themes() {
            let style = PanelStyle::new(&theme);
            let lum = |c: Color| c.r as i32 + c.g as i32 + c.b as i32;
            let readout = style.ink(TypeRole::Readout);
            let row = style.ink(TypeRole::Row);
            let section = style.ink(TypeRole::Section);
            assert!(lum(readout) > lum(row), "theme {}: a readout burns hotter than a name", theme.id);
            assert!(lum(row) > lum(section), "theme {}: a name burns hotter than a label", theme.id);
            assert!(lum(section) > lum(style.recede(section)), "theme {}: receding costs current", theme.id);
        }
    }

    /// Every divider comes from the shared engraver — a dark line plus
    /// a light one — rather than being a hairline this panel drew
    /// itself. Pinned on the band list too, because "which function
    /// drew it" is exactly what a screenshot cannot tell you.
    #[test]
    fn dividers_are_engraved_rather_than_hairlines() {
        let theme = nextstep_classic();
        let style = PanelStyle::new(&theme);
        let pal = panel_palette(&theme);
        let mut pixmap = tiny_skia::Pixmap::new(20, 6).expect("nonzero");
        for y in 0..6 {
            paint::fill_rect(&mut pixmap, 0, y, 20, 1, pal.glass);
        }
        ip::draw_engraved_rule(&mut pixmap, 0, 2, 20, &style);
        let at = |y: u32| {
            let i = ((y * 20 + 10) * 4) as usize;
            let p = pixmap.data();
            (p[i], p[i + 1], p[i + 2])
        };
        assert_ne!(at(2), at(3), "the score and the highlight are different colours");
        assert_eq!(at(1), at(4), "and the rule is exactly two pixels tall");

        // The header and the rescan footer each get one of their own,
        // and every section label carries its rule with it.
        let view = full_view();
        let layout = panel_layout(&view, 56);
        assert_eq!(layout.bands.iter().filter(|(b, ..)| matches!(b, Band::Rule)).count(), 2);
        assert_eq!(layout.bands.iter().filter(|(b, ..)| matches!(b, Band::Label(_))).count(), 3);
    }

    /// The whole panel is drawn on the shared ground, so the glass a
    /// row highlights against starts where `ip::ground_inset` says it
    /// does — the one number the hit test hard-codes.
    #[test]
    fn the_glass_starts_where_the_shared_ground_puts_it() {
        for theme in all_themes() {
            assert_eq!(ip::ground_inset(&theme), GLASS_INSET, "theme {} moved the glass out from under the hit test", theme.id);
        }
    }
}

/// The design-review rig: the panel as it actually appears, at real
/// scale, with the dock column beside it — the only view in which
/// "does this belong to the same machine as the tiles" is answerable.
///
/// Ignored, because it writes files and answers a question only a
/// person can settle. Run it with
/// `LINK_PANEL_PREVIEW=/some/dir cargo test -p chonk-instruments --lib
/// preview -- --ignored --nocapture`.
///
/// Every pixel here comes from production code: the tiles are
/// `wm_theme`'s own tile renderers (the ones the dock blits), the
/// chrome around the panel is `chonk_shell::dockapp::panel`'s recipe
/// restated (tile face, raised relief, sunken well), and the panel is
/// [`render_link_panel`]. The compositor only composites these
/// buffers, so this is what the screen shows.
#[cfg(test)]
mod preview {
    use super::*;
    use wm_theme::default_theme::theme_variant;
    use wm_theme::model::Appearance;
    use wm_theme::{bluetooth as bt, clock, nettraffic, power, soundctl, sysload, wifi, workspace};

    /// The dock's tile edge at the desktop's usual 2x scale.
    /// `LINK_PANEL_TILE` overrides it, to check the 1x desk.
    fn tile() -> u32 {
        std::env::var("LINK_PANEL_TILE").ok().and_then(|v| v.parse().ok()).unwrap_or(112)
    }
    /// The LNK tile's slot down the column: clip, NET, LOAD, SND, LNK.
    const LINK_SLOT: i32 = 4;

    fn blit(dst: &mut tiny_skia::Pixmap, x: i32, y: i32, src: &DecorationBuffer) {
        let Some(src) = tiny_skia::PixmapRef::from_bytes(&src.pixels, src.width, src.height) else { return };
        dst.draw_pixmap(x, y, src, &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    }

    /// The dock column the desktop actually shows, top to bottom.
    fn dock_column(theme: &Theme, fonts: &mut FontSystem, swash: &mut SwashCache) -> Vec<DecorationBuffer> {
        let history: Vec<u32> = (0..nettraffic::NET_TRAFFIC_COLUMNS as u32).map(|i| (i * 7) % 5).collect();
        let up_history: Vec<u32> = (0..nettraffic::NET_TRAFFIC_COLUMNS as u32).map(|i| (i * 3) % 4).collect();
        vec![
            workspace::render_clip_tile(theme, fonts, swash, tile(), 0, 3),
            nettraffic::render_nettraffic_tile(
                theme,
                fonts,
                swash,
                tile(),
                "eno1",
                &nettraffic::TrafficLane {
                    readout: nettraffic::RateReadout { digits: [Some(8), Some(1), Some(5)], unit: nettraffic::RateUnit::Kilo },
                    now: 3,
                    history: &history,
                },
                &nettraffic::TrafficLane {
                    readout: nettraffic::RateReadout { digits: [None, Some(8), Some(4)], unit: nettraffic::RateUnit::Mega },
                    now: 2,
                    history: &up_history,
                },
            ),
            sysload::render_sysload_tile(theme, fonts, swash, tile(), &[2, 3, 5, 4, 6, 3, 2, 4, 7, 5, 3, 2], 6, false),
            soundctl::render_soundctl_tile(theme, fonts, swash, tile(), 0.62, false),
            wifi::render_wifi_tile(theme, fonts, swash, tile(), &wifi::LinkReading::Wifi { ssid: "HomeBase", signal_pct: 87 }),
            bt::render_bluetooth_tile(theme, fonts, swash, tile(), &bt::BtReading::Idle),
            power::render_power_tile(
                theme,
                fonts,
                swash,
                tile(),
                power::PowerFace::Battery { capacity: Some(82), state: power::ChargeState::Discharging },
            ),
            clock::render_clock_tile(theme, tile(), 10, 9, 30),
        ]
    }

    /// The shell's chrome around a panel's content, verbatim from
    /// `chonk_shell::dockapp::panel::render`: tile face under a raised
    /// relief, the content set into a sunken well.
    fn framed(theme: &Theme, content: &DecorationBuffer) -> DecorationBuffer {
        let t = theme.tile.bevel.width.max(1) as u32;
        let inset = t * 3;
        let (w, h) = (content.width + inset * 2, content.height + inset * 2);
        let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("nonzero frame");
        paint::fill_area(&mut pixmap, 0, 0, w, h, &theme.tile.fill);
        paint::draw_raised2_bevel(&mut pixmap, 0, 0, w, h, t);
        let (wx, wy) = ((inset - t) as i32, (inset - t) as i32);
        let (ww, wh) = (content.width + t * 2, content.height + t * 2);
        paint::op_rect(&mut pixmap, wx, wy, ww, wh, -24);
        paint::draw_sunken_bevel(&mut pixmap, wx, wy, ww, wh, t);
        blit(&mut pixmap, inset as i32, inset as i32, content);
        DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() }
    }

    /// One design-review plate: the desk, the dock down its right
    /// edge, and the open panel flush against it.
    fn plate(theme: &Theme, view: &PanelView, fonts: &mut FontSystem, swash: &mut SwashCache) -> tiny_skia::Pixmap {
        let content = render_link_panel(theme, fonts, swash, tile(), view);
        let panel = framed(theme, &content);
        let tiles = dock_column(theme, fonts, swash);

        let dock_h = tile() * tiles.len() as u32;
        let w = panel.width + tile() + tile() / 2;
        let h = dock_h.max(panel.height + tile()).max(tile() * 6);
        let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("nonzero plate");

        // The desk: the theme's own menu ground, taken down a couple of
        // steps. Not the real wallpaper (the shell owns those images),
        // but the right *value* to judge a lit panel against.
        paint::fill_area(&mut pixmap, 0, 0, w, h, &theme.menu.background);
        paint::op_rect(&mut pixmap, 0, 0, w, h, -18);

        let dock_x = (w - tile()) as i32;
        for (i, buffer) in tiles.iter().enumerate() {
            blit(&mut pixmap, dock_x, (i as u32 * tile()) as i32, buffer);
        }
        // Flush against the dock, top-aligned with the LNK slot and
        // clamped into the plate — `dockapp::panel::place`'s rule.
        let x = dock_x - panel.width as i32;
        let y = (LINK_SLOT * tile() as i32).min(h as i32 - panel.height as i32).max(0);
        blit(&mut pixmap, x, y, &panel);
        pixmap
    }

    fn wifi_view() -> PanelView {
        PanelView {
            header: LinkHeader::Wifi { ssid: "HomeBase".into(), signal: 87 },
            connections: vec![
                ConnRow {
                    uuid: "u1".into(),
                    name: "Wired connection 1".into(),
                    kind: ConnKind::Ethernet,
                    lamp: Lamp::On,
                    external: false,
                },
                ConnRow { uuid: "u2".into(), name: "HomeBase".into(), kind: ConnKind::Wifi, lamp: Lamp::On, external: false },
                ConnRow { uuid: "u3".into(), name: "wg-home".into(), kind: ConnKind::WireGuard, lamp: Lamp::Off, external: false },
                ConnRow { uuid: "u4".into(), name: "office-vpn".into(), kind: ConnKind::Vpn, lamp: Lamp::Pending, external: false },
            ],
            wifi: WifiSection::Networks(vec![
                NetRow { ssid: "HomeBase".into(), signal: 87, secured: true, known: true, in_use: true, pending: false },
                NetRow { ssid: "Cafe Wifi".into(), signal: 61, secured: true, known: false, in_use: false, pending: false },
                NetRow { ssid: "OpenMesh".into(), signal: 52, secured: false, known: false, in_use: false, pending: false },
                NetRow { ssid: "Neighbour 5G".into(), signal: 38, secured: true, known: false, in_use: false, pending: false },
                NetRow { ssid: "Printer".into(), signal: 21, secured: false, known: true, in_use: false, pending: false },
            ]),
            tailscale: Some(TailscaleRow {
                status: Some(TailscaleStatus {
                    backend: BackendState::Running,
                    self_online: true,
                    exit_node: Some("gateway".into()),
                    exit_node_online: true,
                    health: vec![],
                }),
                operator: OperatorState::Unknown,
                pending: false,
            }),
            rescan_cooling: false,
            hover: Some(RowKey::Net("Cafe Wifi".into())),
            pressed: None,
        }
    }

    /// This machine's own face: a wired desk with no radio at all, so
    /// the WI-FI section is the designed empty state and there is no
    /// rescan footer.
    fn this_machine_view() -> PanelView {
        PanelView {
            header: LinkHeader::Wired { interface: "cni0".into(), speed_mbps: Some(10_000) },
            connections: vec![ConnRow {
                uuid: "u1".into(),
                name: "Wired connection 1".into(),
                kind: ConnKind::Ethernet,
                lamp: Lamp::On,
                external: false,
            }],
            wifi: WifiSection::NoHardware,
            tailscale: Some(TailscaleRow { status: None, operator: OperatorState::Unknown, pending: false }),
            rescan_cooling: false,
            hover: None,
            pressed: None,
        }
    }

    /// The locked tailnet: the remedy line, and a row that refuses the
    /// click it is being offered.
    fn locked_view() -> PanelView {
        let mut view = wifi_view();
        view.hover = Some(RowKey::Tailscale);
        if let Some(ts) = &mut view.tailscale {
            ts.operator = OperatorState::NeedsOperator { hint: "sudo tailscale set --operator=chris".into() };
            ts.status = Some(TailscaleStatus {
                backend: BackendState::Running,
                self_online: false,
                exit_node: None,
                exit_node_online: false,
                health: vec!["Tailscale is having trouble reaching the network".into()],
            });
        }
        view
    }

    #[test]
    #[ignore = "writes review plates; LINK_PANEL_PREVIEW=<dir> cargo test -p chonk-instruments --lib preview -- --ignored"]
    fn preview_plates() {
        let dir = std::env::var("LINK_PANEL_PREVIEW").unwrap_or_else(|_| "/tmp".to_string());
        let (mut fonts, mut swash) = (FontSystem::new(), SwashCache::new());
        let ids = ["nextstep-classic", "amber-phosphor", "teal-blueprint", "ivory-halftone", "indigo-filament"];
        for id in ids {
            for appearance in [Appearance::Dark, Appearance::Light] {
                let Some(theme) = theme_variant(id, appearance) else { continue };
                for (name, view) in
                    [("wifi", wifi_view()), ("thismachine", this_machine_view()), ("locked", locked_view())]
                {
                    let plate = plate(&theme, &view, &mut fonts, &mut swash);
                    let mood = match appearance {
                        Appearance::Dark => "dark",
                        Appearance::Light => "light",
                    };
                    let path = format!("{dir}/link-{id}-{mood}-{name}.png");
                    plate.save_png(&path).expect("write the review plate");
                    println!("{path}");
                }
            }
        }
    }
}
