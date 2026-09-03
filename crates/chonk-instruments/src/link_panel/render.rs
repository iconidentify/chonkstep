//! The link panel's face: every network the machine could be on, as
//! one chiseled instrument. Pure — a [`PanelView`] in, pixels out —
//! and byte-stable: the same view, theme and size always produce the
//! same buffer, which is what the tests pin.
//!
//! The look extends the LNK tile's grammar upward: a tile-face frame
//! (the [`wm_theme::tile`] fill and relief) around one large glass
//! well, everything on the glass drawn in [`wm_theme::panel`]'s
//! theme-derived LED palette. Top to bottom:
//!
//! - **header** — the current link, two lines: its name in full ink,
//!   its nature (`WIFI · 87%`, `ETHERNET · 1000M`, `DOWN`) in dim.
//! - **CONNECTIONS** — one row per NetworkManager profile worth a
//!   toggle (ethernet, wifi, WireGuard, VPN): an LED lamp for active,
//!   a kind tag (`E`/`W`/`WG`/`VPN`), the profile's name, and `BUSY`
//!   while an optimistic toggle waits for the sample that confirms it.
//! - **WI-FI NETWORKS** — the scan list: a five-bar signal staircase,
//!   a lock glyph on secured networks, and the row's one-word verdict
//!   (`LINKED`, `SAVED`, `JOIN…`, `OPEN`). No wifi hardware is a
//!   designed state — the section says so in dim ink rather than
//!   vanishing.
//! - **TAILSCALE** — the tailnet row, plus at most two dim note lines
//!   (offline, health warnings, the active exit node) and — when a
//!   toggle came back `Access denied` — the CLI's own remedy line,
//!   because the honest answer to a click that cannot work is the
//!   command that would make it work.
//! - **RESCAN** — the one row that drives the radio, dimmed to
//!   `RESCAN · WAIT` while the post-scan cooldown runs.
//!
//! # Geometry is the layout's, and only the layout's
//!
//! [`panel_layout`] is the single authority on where every band sits;
//! the renderer draws inside its bands and the state machine hit-tests
//! against them, so what the pointer feels is exactly what the eye
//! sees. Like `wm_theme::soundctl`'s zone map, the layout assumes the
//! built-in themes' 1px tile bevel rather than taking a theme — input
//! handlers have no theme to offer.

use cosmic_text::{FontSystem, SwashCache};
use wm_theme::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use wm_theme::panel::{panel_palette, PanelPalette};
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
    Label(&'static str),
    Row(RowKey),
    /// A dim informational line (health, exit node, no-hardware).
    Note(String),
    /// The NeedsOperator remedy, verbatim from the CLI.
    Hint(String),
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

/// Fixed 1px bevel assumption, per the soundctl zone-map precedent:
/// every built-in theme's tile bevel is 1, and hit tests have no
/// theme in hand.
const BEVEL: i32 = 1;

fn row_h(tile: u32) -> u32 {
    ((tile as f32) * 0.30).round().max(13.0) as u32
}

fn outer_margin(tile: u32) -> i32 {
    BEVEL + (tile as i32 / 28).max(1)
}

/// Places every band of `view` at `tile` scale. The one geometry
/// authority — see the module doc.
pub fn panel_layout(view: &PanelView, tile: u32) -> PanelLayout {
    let tile = tile.max(8);
    let width = tile * TILES_WIDE;
    let rh = row_h(tile);
    let outer = outer_margin(tile);
    // draw_panel_glass insets its interior by bevel+1 from the well.
    let glass_x = outer + BEVEL + 1;
    let glass_w = (width as i32 - glass_x * 2).max(0) as u32;
    let pad = (rh / 4).max(2) as i32;
    let label_h = ((rh as f32) * 0.85).round() as u32;

    let mut bands = Vec::new();
    let mut y = glass_x + pad;

    bands.push((Band::Header, y, rh * 2));
    y += (rh * 2) as i32 + pad;

    if !view.connections.is_empty() {
        bands.push((Band::Label("CONNECTIONS"), y, label_h));
        y += label_h as i32;
        for conn in &view.connections {
            bands.push((Band::Row(RowKey::Conn(conn.uuid.clone())), y, rh));
            y += rh as i32;
        }
        y += pad;
    }

    match &view.wifi {
        WifiSection::NoHardware => {
            bands.push((Band::Label("WI-FI"), y, label_h));
            y += label_h as i32;
            bands.push((Band::Note("NO WI-FI HARDWARE".to_string()), y, rh));
            y += rh as i32 + pad;
        }
        WifiSection::Networks(nets) => {
            bands.push((Band::Label("WI-FI NETWORKS"), y, label_h));
            y += label_h as i32;
            if nets.is_empty() {
                bands.push((Band::Note("NO NETWORKS IN RANGE".to_string()), y, rh));
                y += rh as i32;
            }
            for net in nets.iter().take(MAX_WIFI_ROWS) {
                bands.push((Band::Row(RowKey::Net(net.ssid.clone())), y, rh));
                y += rh as i32;
            }
            y += pad;
        }
    }

    if let Some(ts) = &view.tailscale {
        bands.push((Band::Label("TAILSCALE"), y, label_h));
        y += label_h as i32;
        bands.push((Band::Row(RowKey::Tailscale), y, rh));
        y += rh as i32;
        if let OperatorState::NeedsOperator { hint } = &ts.operator {
            bands.push((Band::Hint(hint.clone()), y, rh));
            y += rh as i32;
        }
        for note in tailscale_notes(ts) {
            bands.push((Band::Note(note), y, rh));
            y += rh as i32;
        }
        y += pad;
    }

    if matches!(view.wifi, WifiSection::Networks(_)) {
        bands.push((Band::Row(RowKey::Rescan), y, rh));
        y += rh as i32;
    }

    let height = (y + pad + glass_x) as u32;
    PanelLayout { width, height, bands, glass_x, glass_w }
}

/// The dim informational lines under the tailnet row, most urgent
/// first, capped at two so the panel stays an instrument rather than
/// a log viewer.
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
        if let Some(exit) = &status.exit_node {
            notes.push(format!("EXIT NODE: {}", exit.to_uppercase()));
        }
    }
    notes.truncate(2);
    notes
}

fn font(family: &str, size: f32) -> FontSpec {
    FontSpec { family: family.to_string(), size: size.max(6.0), weight: FontWeight::Bold, style: FontStyle::Normal }
}

/// Measured truncation, the label-strip recipe from the LNK tile: a
/// string too wide for its box loses characters, never its box.
fn fit(fonts: &mut FontSystem, spec: &FontSpec, text: &str, max_w: u32) -> String {
    let mut out = text.to_string();
    while !out.is_empty() && paint::text_width(fonts, spec, &out) > max_w {
        out.pop();
    }
    out
}

/// The five-bar ascending staircase at row scale — the LNK tile's
/// silhouette, small enough to be a glyph.
fn draw_mini_stairs(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, w: u32, h: u32, pal: &PanelPalette, signal: u8) {
    let lit = wm_theme::wifi::signal_bars(signal);
    let bars = wm_theme::wifi::SIGNAL_BARS;
    let cell = (w as f32 / bars as f32).max(1.0);
    let gap = (cell * 0.30).clamp(1.0, cell * 0.5);
    let base = y + h as i32;
    for i in 0..bars {
        let bar_h = ((h as f32) * (i + 1) as f32 / bars as f32).round().max(1.0) as u32;
        let bx = x + (i as f32 * cell + gap / 2.0).round() as i32;
        let bw = (cell - gap).max(1.0).round() as u32;
        let color = if i < lit { pal.ink } else { pal.ghost };
        paint::fill_rect(pixmap, bx, base - bar_h as i32, bw, bar_h, color);
    }
}

/// A hard-edged padlock: shackle over body, LED-sized.
fn draw_lock(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, size: u32, pal: &PanelPalette) {
    let s = size.max(5);
    let body_h = s * 3 / 5;
    let body_y = y + (s - body_h) as i32;
    let shackle_w = (s * 3 / 5).max(3);
    let shackle_x = x + ((s - shackle_w) / 2) as i32;
    let t = (s / 5).max(1);
    // Shackle: two posts and a lintel.
    paint::fill_rect(pixmap, shackle_x, y, t, s - body_h, pal.ink_dim);
    paint::fill_rect(pixmap, shackle_x + (shackle_w - t) as i32, y, t, s - body_h, pal.ink_dim);
    paint::fill_rect(pixmap, shackle_x, y, shackle_w, t.min(s - body_h), pal.ink_dim);
    paint::fill_rect(pixmap, x, body_y, s, body_h, pal.ink_dim);
}

/// A one-pixel hollow rectangle — the soft-key outline. Four fills
/// rather than a stroked path, so it lands on exact pixel boundaries
/// like every other hard edge in this instrument.
fn draw_outline(pixmap: &mut tiny_skia::Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color) {
    if w == 0 || h == 0 {
        return;
    }
    paint::fill_rect(pixmap, x, y, w, 1, color);
    paint::fill_rect(pixmap, x, y + h as i32 - 1, w, 1, color);
    paint::fill_rect(pixmap, x, y, 1, h, color);
    paint::fill_rect(pixmap, x + w as i32 - 1, y, 1, h, color);
}

fn lamp_color(lamp: Lamp, pal: &PanelPalette) -> Color {
    match lamp {
        Lamp::On => pal.ink,
        Lamp::Pending => pal.ink_dim,
        Lamp::Off => pal.ghost,
    }
}

fn kind_tag(kind: ConnKind) -> &'static str {
    match kind {
        ConnKind::Ethernet => "E",
        ConnKind::Wifi => "W",
        ConnKind::WireGuard => "WG",
        ConnKind::Vpn => "VPN",
    }
}

/// Renders the whole panel at its natural size. The returned buffer's
/// size is the layout's — the size [`LinkPanel::spec`] asks the shell
/// for, and what the preview example and the byte-stability tests
/// render.
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
    let pal = panel_palette(theme);
    let family = theme.menu.item_font.family.clone();
    let rh = row_h(tile_size.max(8));
    let pad = (rh / 4).max(2) as i32;

    // Frame and glass: the tile recipe at panel proportions.
    paint::fill_area(&mut pixmap, 0, 0, layout.width, layout.height, &theme.tile.fill);
    let t = theme.tile.bevel.width.max(1) as u32;
    paint::draw_raised2_bevel(&mut pixmap, 0, 0, layout.width, layout.height, t);
    let outer = outer_margin(tile_size.max(8));
    wm_theme::panel::draw_panel_glass(
        &mut pixmap,
        outer,
        outer,
        (layout.width as i32 - outer * 2).max(0) as u32,
        (layout.height as i32 - outer * 2).max(0) as u32,
        theme,
    );

    let gx = layout.glass_x;
    let gw = layout.glass_w;
    let text_pad = pad;

    let body = font(&family, rh as f32 * 0.62);
    let small = font(&family, rh as f32 * 0.52);

    for (band, y, h) in &layout.bands {
        let (y, h) = (*y, *h);
        // A grant shorter than the content shows what fits. Drawing
        // the rest would only scribble under the bevel, and `row_at`
        // has already stopped offering these rows to the pointer.
        if y >= layout.height as i32 {
            continue;
        }
        match band {
            Band::Header => draw_header(&mut pixmap, fonts, swash, &family, &pal, &view.header, gx + text_pad, y, gw.saturating_sub((text_pad * 2) as u32), rh),
            Band::Label(label) => {
                let w = gw.saturating_sub((text_pad * 2) as u32);
                paint::draw_text(&mut pixmap, fonts, swash, label, &small, pal.ink_dim, gx + text_pad, y, w, h, TextAlign::Left);
                // A rule from the label's end to the glass edge.
                let label_w = paint::text_width(fonts, &small, label) + (text_pad as u32) * 2;
                if label_w < w {
                    paint::fill_rect(&mut pixmap, gx + text_pad + label_w as i32, y + h as i32 / 2, w - label_w, 1, pal.ghost);
                }
            }
            Band::Row(key) => {
                let hovered = view.hover.as_ref() == Some(key);
                let pressed = view.pressed.as_ref() == Some(key);
                if hovered || pressed {
                    paint::fill_rect(&mut pixmap, gx, y, gw, h, pal.ghost);
                }
                // The classic press: content sinks one pixel.
                let dy = if pressed { 1 } else { 0 };
                draw_row(&mut pixmap, fonts, swash, &body, &small, &pal, view, key, gx + text_pad, y + dy, gw.saturating_sub((text_pad * 2) as u32), h, hovered || pressed);
            }
            Band::Note(note) => {
                let w = gw.saturating_sub((text_pad * 2) as u32);
                let text = fit(fonts, &small, note, w);
                paint::draw_text(&mut pixmap, fonts, swash, &text, &small, pal.ink_dim, gx + text_pad, y, w, h, TextAlign::Left);
            }
            Band::Hint(hint) => {
                let w = gw.saturating_sub((text_pad * 2) as u32);
                let text = fit(fonts, &small, hint, w);
                paint::draw_text(&mut pixmap, fonts, swash, &text, &small, pal.ink, gx + text_pad, y, w, h, TextAlign::Left);
            }
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
    family: &str,
    pal: &PanelPalette,
    header: &LinkHeader,
    x: i32,
    y: i32,
    w: u32,
    rh: u32,
) {
    let name_font = font(family, rh as f32 * 0.78);
    let detail_font = font(family, rh as f32 * 0.55);
    let (name, detail) = match header {
        LinkHeader::Unknown => ("…".to_string(), String::new()),
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
    // The wifi header gets the staircase at the right edge, the one
    // glyph the whole desktop already reads as signal.
    let mut name_w = w;
    if let LinkHeader::Wifi { signal, .. } = header {
        let stairs_w = rh;
        let stairs_h = (rh * 3 / 5).max(4);
        draw_mini_stairs(pixmap, x + (w - stairs_w) as i32, y + (rh - stairs_h) as i32 - 1, stairs_w, stairs_h, pal, *signal);
        name_w = w.saturating_sub(stairs_w + 4);
    }
    let name_color = if matches!(header, LinkHeader::Down { .. } | LinkHeader::Unknown) { pal.ink_dim } else { pal.ink };
    let name = fit(fonts, &name_font, &name, name_w);
    paint::draw_text(pixmap, fonts, swash, &name, &name_font, name_color, x, y, name_w, rh, TextAlign::Left);
    let detail = fit(fonts, &detail_font, &detail, w);
    paint::draw_text(pixmap, fonts, swash, &detail, &detail_font, pal.ink_dim, x, y + rh as i32, w, rh, TextAlign::Left);
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    pixmap: &mut tiny_skia::Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    body: &FontSpec,
    small: &FontSpec,
    pal: &PanelPalette,
    view: &PanelView,
    key: &RowKey,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    highlighted: bool,
) {
    // On a ghost-filled hover band the ghost-colored elements would
    // vanish; dim ink is the unlit tone that stays visible there.
    let unlit = if highlighted { pal.ink_dim } else { pal.ghost };
    match key {
        RowKey::Conn(uuid) => {
            let Some(conn) = view.connections.iter().find(|c| &c.uuid == uuid) else { return };
            let lamp = (h * 2 / 5).max(3);
            let lamp_y = y + ((h - lamp) / 2) as i32;
            let color = match conn.lamp {
                Lamp::Off => unlit,
                lit => lamp_color(lit, pal),
            };
            paint::fill_rect(pixmap, x, lamp_y, lamp, lamp, color);
            let tag_w = (h * 6 / 5).max(14);
            let tag_x = x + lamp as i32 + (lamp as i32 / 2);
            paint::draw_text(pixmap, fonts, swash, kind_tag(conn.kind), small, pal.ink_dim, tag_x, y, tag_w, h, TextAlign::Left);
            let right = match (conn.lamp, conn.external) {
                (Lamp::Pending, _) => "BUSY",
                (_, true) => "EXT",
                _ => "",
            };
            let right_w = if right.is_empty() { 0 } else { paint::text_width(fonts, small, right) + 2 };
            if !right.is_empty() {
                paint::draw_text(pixmap, fonts, swash, right, small, pal.ink_dim, x, y, w, h, TextAlign::Right);
            }
            let name_x = tag_x + tag_w as i32;
            let name_w = (w as i32 - (name_x - x) - right_w as i32).max(0) as u32;
            let name_color = if conn.lamp == Lamp::Off { pal.ink_dim } else { pal.ink };
            let name = fit(fonts, body, &conn.name.to_uppercase(), name_w);
            paint::draw_text(pixmap, fonts, swash, &name, body, name_color, name_x, y, name_w, h, TextAlign::Left);
        }
        RowKey::Net(ssid) => {
            let nets = match &view.wifi {
                WifiSection::Networks(nets) => nets,
                WifiSection::NoHardware => return,
            };
            let Some(net) = nets.iter().find(|n| &n.ssid == ssid) else { return };
            let stairs_w = (h * 4 / 5).max(8);
            let stairs_h = (h * 3 / 5).max(4);
            draw_mini_stairs(pixmap, x, y + (h - stairs_h) as i32 - 2, stairs_w, stairs_h, pal, net.signal);
            let mut cursor = x + stairs_w as i32 + (h as i32 / 3);
            if net.secured {
                let lock = (h * 3 / 5).max(5);
                draw_lock(pixmap, cursor, y + ((h - lock) / 2) as i32, lock, pal);
                cursor += lock as i32 + (h as i32 / 3);
            }
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
            let right_color = if net.in_use || net.pending { pal.ink } else { pal.ink_dim };
            paint::draw_text(pixmap, fonts, swash, right, small, right_color, x, y, w, h, TextAlign::Right);
            let right_w = paint::text_width(fonts, small, right) + 4;
            let name_w = (w as i32 - (cursor - x) - right_w as i32).max(0) as u32;
            let name_color = if net.in_use { pal.ink } else { pal.ink_dim };
            let name = fit(fonts, body, &net.ssid.to_uppercase(), name_w);
            paint::draw_text(pixmap, fonts, swash, &name, body, name_color, cursor, y, name_w, h, TextAlign::Left);
        }
        RowKey::Tailscale => {
            let Some(ts) = &view.tailscale else { return };
            let lamp = (h * 2 / 5).max(3);
            let lamp_y = y + ((h - lamp) / 2) as i32;
            let lamp_state = if ts.pending {
                Lamp::Pending
            } else {
                match ts.status.as_ref().map(|s| s.backend) {
                    Some(BackendState::Running) => Lamp::On,
                    Some(BackendState::Starting) => Lamp::Pending,
                    _ => Lamp::Off,
                }
            };
            let color = match lamp_state {
                Lamp::Off => unlit,
                lit => lamp_color(lit, pal),
            };
            paint::fill_rect(pixmap, x, lamp_y, lamp, lamp, color);
            let right = if matches!(ts.operator, OperatorState::NeedsOperator { .. }) {
                "LOCKED"
            } else if ts.pending {
                "BUSY"
            } else {
                match ts.status.as_ref().map(|s| s.backend) {
                    Some(BackendState::Running) => "UP",
                    Some(BackendState::Stopped) => "DOWN",
                    Some(BackendState::Starting) => "BUSY",
                    Some(BackendState::NeedsLogin) => "LOGIN",
                    Some(BackendState::NeedsMachineAuth) => "AUTH",
                    Some(BackendState::NoState) | Some(BackendState::Other) => "?",
                    None => "…",
                }
            };
            paint::draw_text(pixmap, fonts, swash, right, small, pal.ink_dim, x, y, w, h, TextAlign::Right);
            let name_x = x + lamp as i32 + (lamp as i32 / 2);
            let name_color = if lamp_state == Lamp::On { pal.ink } else { pal.ink_dim };
            // "TAILNET", not "TAILSCALE": the section label directly
            // above already says Tailscale, and a row that repeats its
            // own heading spends a line saying nothing. This one names
            // the thing the lamp is about.
            paint::draw_text(pixmap, fonts, swash, "TAILNET", body, name_color, name_x, y, w.saturating_sub((name_x - x) as u32), h, TextAlign::Left);
        }
        RowKey::Rescan => {
            // The one row that is a *command* rather than a reading,
            // so it needs to look pressable — and on a glass screen
            // that is an outlined soft key, not a chrome button: a
            // raised bevel floating on the LED would read as a chip
            // stuck to the display.
            let (label, color) = if view.rescan_cooling { ("RESCAN · WAIT", pal.ink_dim) } else { ("RESCAN", pal.ink) };
            let label_w = paint::text_width(fonts, body, label);
            let key_w = (label_w + h * 2).min(w);
            let key_x = x + ((w.saturating_sub(key_w)) / 2) as i32;
            let key_h = h.saturating_sub(2).max(1);
            draw_outline(pixmap, key_x, y + 1, key_w, key_h, if highlighted { pal.ink } else { color });
            paint::draw_text(pixmap, fonts, swash, label, body, color, key_x, y + 1, key_w, key_h, TextAlign::Center);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme::default_theme::{all_themes, nextstep_classic};

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
                    exit_node_choices: 1,
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
                .chunks_exact(4)
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

    #[test]
    fn an_empty_scan_list_says_so_rather_than_showing_nothing() {
        let mut view = full_view();
        view.wifi = WifiSection::Networks(vec![]);
        let layout = panel_layout(&view, 56);
        assert!(
            layout.bands.iter().any(|(band, ..)| matches!(band, Band::Note(note) if note == "NO NETWORKS IN RANGE")),
            "an empty list is a designed state"
        );
    }
}
