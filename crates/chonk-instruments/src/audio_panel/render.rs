//! The audio panel's face: a recessed column of chiseled device rows,
//! rendered pure — theme plus [`AudioPanel`] state in, premultiplied
//! RGBA out — so every state can be pinned byte-for-byte in tests and
//! rasterized headless for design review.
//!
//! The look extends the instrument family's vocabulary sideways: the
//! shell's panel chrome already wraps this content in tile face and a
//! sunken well, so the content ground here is the tile fill shaded
//! down (reading as the well's floor), and each device is a raised
//! chiseled row on it — the tile's own fill and bevel, the exact
//! `draw_button` recipe titlebar buttons use, because the rows *are*
//! buttons. On each row, left to right:
//!
//! * the default lamp — a small sunken LED cell, lit in the theme's
//!   [`panel::panel_accent`] on the (shown) default sink, dark glass
//!   on the rest;
//! * the device description in tile ink;
//! * a three-bar activity mark, lit only while the sink is RUNNING —
//!   "this one is playing right now";
//! * the volume readout;
//! * past a chiseled notch pair: the mute square, a speaker mark that
//!   picks up the accent strike when the sink is muted — the same
//!   crossed-out-speaker language the sound tile speaks.
//!
//! States: the hovered control (row body or mute square, from panel
//! `Motion`) brightens; the pressed one sinks with the standard
//! pressed-button feedback; an unavailable sink's row is shaded down
//! whole — greyed scenery, matching the hit-test that refuses it. A
//! panel with no sinks at all shows one dim "NO OUTPUTS" row.

use tiny_skia::Pixmap;
use wm_theme::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use wm_theme::{paint, panel, tile, Theme};
use wm_theme_api::DecorationBuffer;

use super::{AudioPanel, PanelMetrics, PanelTarget, PanelZone};

/// How much the hovered control brightens. The menu highlight is a
/// theme fill; rows here are tile-fill buttons, so hover is a relative
/// lift the way every relief in the theme kit is relative. Sized by
/// looking at it: at half this it was invisible on a dark palette and
/// nearly so on a light one, which is a hover highlight that does not
/// do its one job.
const HOVER_LIFT: i16 = 26;

/// The wash an unavailable row is flooded with, over the whole row
/// *after* its contents, so face, bevel, lamp and ink all grey out
/// together.
///
/// A neutral half-tone at partial alpha rather than a darkening pass:
/// darkening reads as "disabled" on a dark palette and as "emphasised"
/// on a light one, whereas pulling everything toward mid grey drops the
/// row's internal contrast — and its colour — in both appearances at
/// once. The bevel washes out with the rest, so the row stops looking
/// like a button, which is exactly what the hit-test says it is.
const UNAVAILABLE_WASH: Color = Color { r: 0x80, g: 0x80, b: 0x80, a: 0x74 };

/// The recess of the content ground below the tile face, matching
/// `draw_tile_well`'s shading so the rows read as sitting in the
/// shell-drawn well around them.
const GROUND_SHADE: i16 = -24;

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

    // The well floor: tile fill, shaded down, over the whole grant. No
    // bevel of its own — the shell's chrome provides the sunken lip
    // around this buffer.
    paint::fill_area(&mut pixmap, 0, 0, w, h, &theme.tile.fill);
    paint::op_rect(&mut pixmap, 0, 0, w, h, GROUND_SHADE);

    // "No outputs" is a statement about the mixer, not about the grant:
    // a panel with devices it had no room for shows bare well floor
    // under the ones it drew, never a label claiming there are none.
    if audio.sinks().is_empty() {
        draw_empty_face(&mut pixmap, theme, fonts, swash, m);
        return DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() };
    }

    let visible = m.visible_rows(audio.sinks().len());
    for (i, sink) in audio.sinks().iter().take(visible).enumerate() {
        draw_row(&mut pixmap, theme, fonts, swash, m, m.row_top(i), sink, audio);
    }

    DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() }
}

/// Whether `target` is this sink's control in the given zone.
fn is_on(target: Option<&PanelTarget>, sink: &str, zone: PanelZone) -> bool {
    target.is_some_and(|t| t.sink == sink && t.zone == zone)
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &PanelMetrics,
    y: i32,
    sink: &super::AudioSink,
    audio: &AudioPanel,
) {
    let t = theme.tile.bevel.width.max(1) as u32;
    let ti = t as i32;
    let x = m.pad as i32;
    let row_w = m.width - m.pad * 2;
    let h = m.row_h;
    let hi = h as i32;
    let pal = panel::panel_palette(theme);
    let ink = tile::tile_ink(theme);
    let dim = tile::tile_ink_dim(theme);

    let row_pressed = is_on(audio.pressed(), &sink.name, PanelZone::Row);
    let mute_pressed = is_on(audio.pressed(), &sink.name, PanelZone::Mute);
    let row_hovered = is_on(audio.hover(), &sink.name, PanelZone::Row);
    let mute_hovered = is_on(audio.hover(), &sink.name, PanelZone::Mute);

    // The row is a button, drawn with the button recipe — raised
    // chiseled face, or the standard pressed feedback when its body is
    // the armed control.
    paint::draw_button(pixmap, x, y, row_w, h, &theme.tile.fill, &theme.tile.bevel, row_pressed);
    if row_hovered && !row_pressed {
        paint::op_rect(pixmap, x + ti, y + ti, row_w - t * 2, h - t * 2, HOVER_LIFT);
    }

    // Inner padding scales with the row.
    let p2 = (hi / 6).max(2);

    // Default lamp: a sunken LED cell. Lit = the panel accent (the
    // same LED ink the tile's glass uses), unlit = dark glass.
    let lamp_s = (hi / 3).max(5);
    let lamp_x = x + p2;
    let lamp_y = y + (hi - lamp_s) / 2;
    let lit = audio.shown_default() == Some(sink.name.as_str());
    paint::fill_rect(pixmap, lamp_x, lamp_y, lamp_s as u32, lamp_s as u32, if lit { pal.ink } else { pal.glass });
    paint::draw_sunken_bevel(pixmap, lamp_x - 1, lamp_y - 1, lamp_s as u32 + 2, lamp_s as u32 + 2, 1);

    // The mute square sits at the row's right edge, past a chiseled
    // notch pair — the resizebar's shade+light seam, marking where the
    // row's action changes.
    let mx = m.mute_zone_left();
    paint::op_rect(pixmap, mx - 1, y + ti, 1, h - t * 2, -40);
    paint::op_rect(pixmap, mx, y + ti, 1, h - t * 2, 80);
    let mute_inner_x = mx + 1;
    let mute_inner_w = (x + row_w as i32 - ti - mute_inner_x).max(0) as u32;
    if mute_pressed {
        paint::draw_button_pressed(pixmap, mute_inner_x, y + ti, mute_inner_w, h - t * 2, paint::pressed_delta(&theme.tile.fill), 1);
    } else if mute_hovered {
        paint::op_rect(pixmap, mute_inner_x, y + ti, mute_inner_w, h - t * 2, HOVER_LIFT);
    }
    let spk = (hi * 3 / 5).max(8);
    let spk_x = mute_inner_x + (mute_inner_w as i32 - spk) / 2;
    let spk_y = y + (hi - spk) / 2;
    if sink.muted {
        draw_speaker_mark(pixmap, spk_x, spk_y, spk as u32, ink, Some(pal.ink));
    } else {
        draw_speaker_mark(pixmap, spk_x, spk_y, spk as u32, dim, None);
    }

    // Right cluster, laid right-to-left from the notch: the volume
    // readout, then the activity bars' fixed slot.
    let font_px = (h as f32 * 0.36).max(7.0);
    let vol_font = FontSpec { family: theme.menu.item_font.family.clone(), size: font_px, weight: FontWeight::Bold, style: FontStyle::Normal };
    let vol_w = paint::text_width(fonts, &vol_font, "888%") + 2;
    let vol_x = mx - 1 - p2 - vol_w as i32;
    let vol_text = match sink.volume_percent {
        Some(percent) => format!("{percent}%"),
        None => "--".to_string(),
    };
    // Muted keeps its level on record but reads dim — the speaker
    // strike is the loud part, exactly like the tile.
    let vol_color = if sink.muted { dim } else { ink };
    paint::draw_text(pixmap, fonts, swash, &vol_text, &vol_font, vol_color, vol_x, y, vol_w, h, TextAlign::Right);

    let bars_w = (hi / 3).max(5);
    let bars_x = vol_x - p2 - bars_w;
    if sink.running {
        draw_activity_bars(pixmap, bars_x, y, bars_w, hi, pal.ink);
    }

    // The description gets whatever is left, ellipsized to fit — and
    // the default sink's is set bold. The lamp is the *statement* of
    // which device the desktop plays through; the weight is what makes
    // that readable at a glance, from across a list, without a second
    // coloured mark competing with the activity bars.
    let desc_weight = if lit { FontWeight::Bold } else { FontWeight::Normal };
    let desc_font = FontSpec { family: theme.menu.item_font.family.clone(), size: font_px, weight: desc_weight, style: FontStyle::Normal };
    let desc_x = lamp_x + lamp_s + p2;
    let desc_w = (bars_x - p2 - desc_x).max(0) as u32;
    let desc = fit_text(fonts, &desc_font, &sink.description, desc_w);
    let desc_color = if sink.available { ink } else { dim };
    paint::draw_text(pixmap, fonts, swash, &desc, &desc_font, desc_color, desc_x, y, desc_w, h, TextAlign::Left);

    // Unavailable: the whole row — face, bevel, lamp, ink — washes out
    // as one, greyed scenery matching the hit-test that refuses it.
    if !sink.available {
        paint::fill_rect(pixmap, x, y, row_w, h, UNAVAILABLE_WASH);
    }
}

/// Three rising bars in the activity slot — the "this sink is playing"
/// mark. Static on purpose: the panel repaints on state changes, not
/// on a clock, and a meter that only pretended to meter would be worse
/// than a lamp.
fn draw_activity_bars(pixmap: &mut Pixmap, x: i32, y: i32, w: i32, h: i32, color: Color) {
    let bar_w = (w / 4).max(1);
    let gap = ((w - bar_w * 3) / 2).max(1);
    let base = y + h * 2 / 3;
    for (i, rise) in [(0, h / 6), (1, h / 3), (2, h / 4)] {
        let bx = x + i * (bar_w + gap);
        paint::fill_rect(pixmap, bx, base - rise, bar_w as u32, rise.max(1) as u32, color);
    }
}

/// The blocky speaker, restated small: a driver box and a three-step
/// cone in the fractional grid of its square, plus the rising accent
/// strike when muted — the same silhouette the sound tile's mute zone
/// wears, so the two read as one control.
fn draw_speaker_mark(pixmap: &mut Pixmap, x: i32, y: i32, s: u32, body: Color, strike: Option<Color>) {
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

/// Ellipsizes `text` to fit `max_w` at the shaped width cosmic-text
/// will actually lay out — trimming by characters until the trimmed
/// form plus `…` fits. Descriptions are a few dozen characters, and
/// the panel repaints on state changes rather than frames, so the
/// re-measures stay cheap where they are paid.
fn fit_text(fonts: &mut cosmic_text::FontSystem, font: &FontSpec, text: &str, max_w: u32) -> String {
    if paint::text_width(fonts, font, text) <= max_w {
        return text.to_string();
    }
    let mut kept: Vec<char> = text.chars().collect();
    while kept.len() > 1 {
        kept.pop();
        let candidate: String = kept.iter().collect::<String>().trim_end().to_string() + "…";
        if paint::text_width(fonts, font, &candidate) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

/// The no-devices face: one dim label on the well floor. Dead-screen
/// grammar, panel-shaped.
fn draw_empty_face(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &PanelMetrics,
) {
    let font = FontSpec {
        family: theme.menu.item_font.family.clone(),
        size: (m.row_h as f32 * 0.4).max(8.0),
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    let dim = tile::tile_ink_dim(theme);
    paint::draw_text(pixmap, fonts, swash, "NO OUTPUTS", &font, dim, 0, m.pad as i32, m.width, m.row_h, TextAlign::Center);
}
