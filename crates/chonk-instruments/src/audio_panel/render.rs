//! The audio panel's face: the VOL tile opened up — a sheet of glass
//! with the machine's outputs on it as LED rows, rendered pure (theme
//! plus [`AudioPanel`] state in, premultiplied RGBA out) so every state
//! can be pinned byte-for-byte in tests and rasterized headless for
//! design review.
//!
//! Every mark here comes out of [`wm_theme::instrument_panel`], the
//! shared panel vocabulary, so this panel and its siblings read as one
//! machine. Nothing in this module picks a color.
//!
//! Top to bottom: an `OUTPUTS` section header (tracked dim caps under
//! an engraved rule, so the panel says what it is), then one row per
//! sink, separated by grooves chiseled into the glass. Across a row,
//! left to right:
//!
//! * the **default lamp** — the LINK tile's port lamp, lit on the
//!   (shown) default sink, dark on the rest. A lamp, seated behind a
//!   bezel with a halo when lit: this is the panel saying "this one",
//!   and it is the mark the eye finds first;
//! * the **device name**, in the ramp's row ink;
//! * the **level meter** — the VOL tile's own stacked bars laid on
//!   their side, drawn by the same [`wm_theme::panel::draw_led_bar`]
//!   the tile calls, in a groove milled into the glass. A level in this
//!   desktop is always a meter; the percent beside it is the fine
//!   reading, never the reading itself;
//! * the **percent readout** at the top of the type ramp — the
//!   brightest thing in the row;
//! * past an **engraved seam**, the **mute key**: a control milled into
//!   the glass carrying the same blocky speaker the VOL tile wears,
//!   struck through in the accent when the sink is muted.
//!
//! The states are the vocabulary's: hover lifts a row's glass, a press
//! sinks it under a sunken chisel, and an unavailable sink's whole row
//! recedes — every ink one step back toward the glass, the lamp dark,
//! no lift under the pointer — which is exactly what the hit-test says
//! about it.
//!
//! Muted is a designed state, the way it is on the tile: the meter goes
//! all-ghost (the level the mixer still remembers is not a level
//! anything is playing at), the readout recedes, and the one bright
//! thing on the row is the struck speaker. A sink that is *playing*
//! (`RUNNING`) burns its meter at full ink; an idle one meters at the
//! dim level — same reading, less current.
//!
//! A panel with no sinks at all keeps its header and says `NO OUTPUTS`
//! under it: a mixer with nothing in it is a reading, not a hole.

use tiny_skia::Pixmap;
use wm_theme::instrument_panel as ip;
use wm_theme::instrument_panel::{LampState, MeterGlow, PanelStyle, RowState, TypeRole};
use wm_theme::model::TextAlign;
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use super::{AudioPanel, PanelMetrics, PanelTarget, PanelZone};

/// Renders the panel content at exactly the granted size the metrics
/// carry — [`PanelMetrics::granted`] from the [`PanelFrame`]'s own
/// width and height, never a size this module worked out for itself.
/// A grant the shell clamped short is filled floor-to-floor with the
/// devices that fit; the ones that do not are drawn nowhere and, by
/// [`PanelMetrics::visible_rows`], clickable nowhere either.
///
/// [`PanelFrame`]: chonk_dock_widget::PanelFrame
pub fn render_audio_panel(
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &PanelMetrics,
    audio: &AudioPanel,
) -> DecorationBuffer {
    let (w, h) = (m.width.max(1), m.height.max(1));
    let mut pixmap = Pixmap::new(w, h).expect("nonzero audio panel grant");
    let style = PanelStyle::new(theme);

    // The face: tile gasket, sunken well, glass — the tile's own screen
    // recipe at panel size. The shell's chrome wraps this buffer in the
    // outer relief, so what lands on screen is one milled block.
    ip::draw_panel_ground(&mut pixmap, 0, 0, w, h, theme);

    // Where the glass *actually* starts, which is a function of the
    // theme's bevel. [`PanelMetrics`] assumes the built-in themes' 1px
    // one because a hit test has no theme in scope (the soundctl
    // zone-map precedent); the drawing has one, and uses it, so a
    // wide-bevel theme's rows sit inside their glass rather than
    // crossing the well's lip. The two agree exactly at 1px and differ
    // by a pixel or two above that — well inside a row's slack.
    let edge = ip::ground_inset(theme).max(m.glass_x());
    let band = (w as i32 - edge * 2).max(0) as u32;

    let gx = edge + m.pad as i32;
    let gw = band.saturating_sub(m.pad * 2);
    ip::draw_section_header(&mut pixmap, fonts, swash, &style, "OUTPUTS", gx, edge + m.pad as i32, gw, m.header_h);

    // "No outputs" is a statement about the mixer, not about the grant:
    // a panel with devices it had no room for shows bare glass under
    // the ones it drew, never a label claiming there are none.
    if audio.sinks().is_empty() {
        let font = style.typeface(TypeRole::Row, m.row_h).receded(&style);
        ip::draw_type(&mut pixmap, fonts, swash, &font, "NO OUTPUTS", gx, m.rows_top(), gw, m.row_h, TextAlign::Center);
        return DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() };
    }

    let visible = m.visible_rows(audio.sinks().len());
    for (i, sink) in audio.sinks().iter().take(visible).enumerate() {
        let y = m.row_top(i);
        // The groove between rows: the gap is exactly one engraved
        // rule, so the stack reads as milled slots rather than as
        // floating list items.
        if i > 0 {
            ip::draw_engraved_rule(&mut pixmap, edge, y - m.gap as i32, band, &style);
        }
        draw_row(&mut pixmap, &style, fonts, swash, m, edge, band, y, sink, audio);
    }

    DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() }
}

/// Whether `target` is this sink's control in the given zone.
fn is_on(target: Option<&PanelTarget>, sink: &str, zone: PanelZone) -> bool {
    target.is_some_and(|t| t.sink == sink && t.zone == zone)
}

/// The state one control of one row is in. Unavailable wins over
/// everything: a sink the hit-test refuses can be neither hovered nor
/// pressed, so it must not be drawn as either.
fn state_of(audio: &AudioPanel, sink: &super::AudioSink, zone: PanelZone) -> RowState {
    if !sink.available {
        RowState::Disabled
    } else if is_on(audio.pressed(), &sink.name, zone) {
        RowState::Pressed
    } else if is_on(audio.hover(), &sink.name, zone) {
        RowState::Hover
    } else {
        RowState::Idle
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    pixmap: &mut Pixmap,
    style: &PanelStyle,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &PanelMetrics,
    x: i32,
    row_w: u32,
    y: i32,
    sink: &super::AudioSink,
    audio: &AudioPanel,
) {
    let h = m.row_h;
    let hi = h as i32;
    let pad = m.pad as i32;

    let row_state = state_of(audio, sink, PanelZone::Row);
    let mute_state = state_of(audio, sink, PanelZone::Mute);
    let disabled = !sink.available;

    // The seam, and the mute key past it — placed first, because
    // everything else in the row lays itself out against the seam.
    let seam = m.mute_zone_left();
    let has_key = seam + (m.mute_key_w() as i32) <= x + row_w as i32;
    let cluster_right = if has_key { seam } else { x + row_w as i32 - pad };

    // The row's own ground covers the body only: the mute key is its
    // own control and takes its own state, so a hover on one must not
    // light the other.
    ip::draw_row_ground(pixmap, x, y, (cluster_right - x).max(0) as u32, h, style, row_state);

    if has_key {
        ip::draw_engraved_seam(pixmap, seam, y, h, style);
        let key_w = m.mute_key_w();
        let key_x = seam + pad;
        let key_h = ip::hit_size((h * 4 / 5).max(1));
        let key_y = y + (hi - key_h as i32) / 2;
        ip::draw_key_cell(pixmap, key_x, key_y, key_w, key_h, style, mute_state);
        // The speaker: dim as an affordance, full ink under an accent
        // strike when muted — the crossed-out speaker the VOL tile's
        // mute zone wears, so the two read as one control.
        let mark = (key_h * 3 / 5).max(6);
        let mx = key_x + (key_w as i32 - mark as i32) / 2;
        let my = key_y + (key_h as i32 - mark as i32) / 2;
        let (body, strike) = match (sink.muted, disabled) {
            (_, true) => (style.recede(style.pal.ink_dim), None),
            (true, false) => (style.ink(TypeRole::Readout), Some(style.pal.ink)),
            (false, false) => (style.pal.ink_dim, None),
        };
        ip::draw_speaker(pixmap, mx, my, mark, body, strike);
    }

    // The right cluster, laid right-to-left from the seam: the percent
    // readout, then the meter it is the fine reading of.
    let mut readout = style.typeface(TypeRole::Readout, h);
    if sink.muted || disabled || sink.volume_percent.is_none() {
        readout = readout.receded(style);
    }
    let readout_text = match sink.volume_percent {
        Some(percent) => format!("{percent}%"),
        None => "--".to_string(),
    };
    let readout_w = ip::type_width(fonts, &readout, "100%").max(ip::type_width(fonts, &readout, &readout_text));
    let readout_x = cluster_right - pad - readout_w as i32;
    ip::draw_type(pixmap, fonts, swash, &readout, &readout_text, readout_x, y, readout_w, h, TextAlign::Right);

    // The meter: the VOL tile's ladder on its side. Its glow is the
    // row's "is this one playing" reading — full ink while the sink
    // renders audio, the dim level while it idles, and nothing at all
    // while it is muted.
    let meter_h = ip::meter_h(h);
    let meter_w = ((row_w as f32) * 0.18).round().clamp(16.0, 140.0) as u32;
    let meter_x = readout_x - pad - meter_w as i32;
    let meter_y = y + (hi - meter_h as i32) / 2;
    let level = sink.volume_percent.unwrap_or(0) as f32 / 100.0;
    let glow = match (sink.muted, disabled, sink.running) {
        (true, _, _) => MeterGlow::Silent,
        (_, true, _) => MeterGlow::Silent,
        (_, _, true) => MeterGlow::Active,
        _ => MeterGlow::Idle,
    };
    ip::draw_meter(pixmap, meter_x, meter_y, meter_w, meter_h, style, level, glow);

    // The lamp: which device the desktop plays through.
    let lamp = ip::lamp_size(h);
    let lamp_x = x + pad + style.bevel as i32;
    let lamp_y = y + (hi - lamp as i32) / 2;
    let lit = audio.shown_default() == Some(sink.name.as_str());
    let lamp_state = match (lit, disabled) {
        (true, false) => LampState::On,
        (true, true) => LampState::Pending,
        (false, _) => LampState::Off,
    };
    ip::draw_lamp(pixmap, lamp_x, lamp_y, lamp, style, lamp_state);

    // The name gets what is left between the lamp and the meter,
    // ellipsized to fit by measurement so it can never collide with the
    // level beside it.
    let name = style.typeface(TypeRole::Row, h).colored(style.ink_for(TypeRole::Row, row_state));
    let name_x = lamp_x + lamp as i32 + pad * 2;
    let name_w = (meter_x - pad - name_x).max(0) as u32;
    let fitted = elide_middle(fonts, &name, &sink.description, name_w);
    ip::draw_type(pixmap, fonts, swash, &name, &fitted, name_x, y, name_w, h, TextAlign::Left);
}

/// Shortens a device description to `max_w`, cutting from the middle.
///
/// PulseAudio names are front-loaded — "Built-in Audio Digital Stereo
/// (HDMI)", "Built-in Audio Analog Stereo" — so the words that say
/// *which* device this is sit at the end, and a tail trim throws away
/// exactly the part a user is reading the row to find: two built-in
/// outputs both render as "Built-in Audio Digit…". Cutting from the
/// middle keeps both ends, so the head still identifies the family and
/// the tail still distinguishes the device.
///
/// Falls back to the shared tail trim when the string is too short for
/// a middle cut to buy anything.
fn elide_middle(fonts: &mut cosmic_text::FontSystem, font: &ip::PanelFont, text: &str, max_w: u32) -> String {
    if ip::type_width(fonts, font, text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Below this there is no middle worth cutting: the ellipsis would
    // cost more than the characters it replaces.
    if chars.len() < 12 {
        return ip::fit_type(fonts, font, text, max_w);
    }
    // Bias the kept text toward the tail, which is where the
    // distinguishing words live.
    for keep in (6..chars.len()).rev() {
        let head = keep / 3;
        let tail = keep - head;
        // Snap both cuts to word boundaries where one is close, so the
        // result reads as an abbreviation rather than as damage:
        // "Built-in…Stereo (HDMI)", not "Built-in…ital Stereo (HDMI)".
        let head = snap_back(&chars, head);
        let tail_start = snap_forward(&chars, chars.len() - tail);
        let candidate: String = chars[..head]
            .iter()
            .chain("…".chars().collect::<Vec<_>>().iter())
            .chain(chars[tail_start..].iter())
            .collect();
        if ip::type_width(fonts, font, &candidate) <= max_w {
            return candidate;
        }
    }
    ip::fit_type(fonts, font, text, max_w)
}

/// Pulls a cut point back to just after the previous space, when one is
/// near enough that the trimmed word was not worth keeping.
fn snap_back(chars: &[char], at: usize) -> usize {
    let floor = at.saturating_sub(6);
    (floor..at).rev().find(|&i| chars[i] == ' ').map_or(at, |i| i)
}

/// Pushes a cut point forward to the start of the next word, on the
/// same reasoning as [`snap_back`].
fn snap_forward(chars: &[char], at: usize) -> usize {
    let ceiling = (at + 6).min(chars.len());
    (at..ceiling).find(|&i| chars[i] == ' ').map_or(at, |i| i + 1)
}
