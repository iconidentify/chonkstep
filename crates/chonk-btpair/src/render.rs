//! The dialog's face: the chiseled chrome, the LED glass, and the rows
//! and buttons the pointer can land on.
//!
//! Layout and drawing live in one place on purpose, and it is the same
//! discipline `wm_theme::bluetooth` uses for the dock panel: [`layout`]
//! returns the rectangles, [`draw`] paints them, and the click handler
//! hit-tests against the *same* rectangles, so the pixels and the
//! hitboxes cannot drift apart.

use chonk_ui::model::{Color, Fill, FontSpec, FontStyle, FontWeight, TextAlign, Theme};
use chonk_ui::{paint, panel};
use tiny_skia::Pixmap;
use wm_theme::bluetooth::draw_bt_rune;

use crate::pair::{Found, Phase};

/// Logical (1x) window size. Tall enough for a header, six device rows
/// and a footer without scrolling — a discovery list longer than that
/// is a crowded room, and the answer there is to move closer to the
/// device rather than to scroll.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 260;

/// The most device rows the list draws. Beyond this the list says how
/// many more there are rather than growing a scrollbar the SDK has no
/// widget for.
pub const MAX_ROWS: usize = 6;

/// Something the pointer can hit, with the rectangle it occupies in
/// device pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Pair with this device.
    Device(String),
    /// The confirm dialog's two buttons.
    Yes,
    No,
    /// Start discovery over, from a finished or failed state.
    Rescan,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub target: Target,
    pub rect: (i32, i32, u32, u32),
}

/// Everything the pointer can land on, in the order it is drawn.
pub struct Layout {
    pub hits: Vec<Hit>,
}

impl Layout {
    /// The target at a point, or `None` for the chrome.
    pub fn at(&self, x: i32, y: i32) -> Option<&Target> {
        self.hits
            .iter()
            .find(|hit| {
                let (hx, hy, hw, hh) = hit.rect;
                x >= hx && y >= hy && x < hx + hw as i32 && y < hy + hh as i32
            })
            .map(|hit| &hit.target)
    }
}

/// The one scale-aware metric helper, so every literal below is a
/// logical pixel and the window agrees with the rest of the session
/// about how big a pixel is.
pub struct Metrics {
    pub scale: f32,
    pub width: u32,
    pub height: u32,
}

impl Metrics {
    pub fn new(scale: f32) -> Self {
        let s = |v: u32| ((v as f32) * scale).round().max(1.0) as u32;
        Self { scale, width: s(WIDTH), height: s(HEIGHT) }
    }

    pub fn s(&self, v: u32) -> u32 {
        ((v as f32) * self.scale).round().max(1.0) as u32
    }

    pub fn si(&self, v: u32) -> i32 {
        self.s(v) as i32
    }
}

/// The glass rectangle: the recessed screen everything but the header
/// is drawn on.
fn glass_rect(m: &Metrics) -> (i32, i32, u32, u32) {
    (m.si(12), m.si(44), m.width - m.s(24), m.height - m.s(56))
}

/// One list row's height.
fn row_height(m: &Metrics) -> u32 {
    m.s(24)
}

/// Computes the hitboxes for a phase. Called by both [`draw`] and the
/// click handler, which is what keeps them honest.
pub fn layout(m: &Metrics, phase: &Phase, devices: &[&Found]) -> Layout {
    let (gx, gy, gw, gh) = glass_rect(m);
    let mut hits = Vec::new();
    match phase {
        Phase::Scanning | Phase::Starting => {
            let row_h = row_height(m);
            for (index, device) in devices.iter().take(MAX_ROWS).enumerate() {
                if device.paired {
                    // Already paired: shown so the list is not
                    // confusing, but not offered again.
                    continue;
                }
                hits.push(Hit {
                    target: Target::Device(device.address.clone()),
                    rect: (gx, gy + (index as u32 * row_h) as i32, gw, row_h),
                });
            }
        }
        Phase::Confirm { .. } => {
            let bw = gw / 2 - m.s(12);
            let bh = m.s(30);
            let by = gy + gh as i32 - bh as i32 - m.si(10);
            hits.push(Hit { target: Target::Yes, rect: (gx + m.si(8), by, bw, bh) });
            hits.push(Hit { target: Target::No, rect: (gx + gw as i32 - bw as i32 - m.si(8), by, bw, bh) });
        }
        Phase::Paired { .. } | Phase::Failed { .. } | Phase::NeedsKeyboard { .. } => {
            let bw = gw - m.s(16);
            let bh = m.s(30);
            hits.push(Hit { target: Target::Rescan, rect: (gx + m.si(8), gy + gh as i32 - bh as i32 - m.si(10), bw, bh) });
        }
        Phase::Pairing { .. } | Phase::DisplayPasskey { .. } | Phase::Unavailable { .. } => {}
    }
    Layout { hits }
}

/// Paints the whole window.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &Metrics,
    phase: &Phase,
    devices: &[&Found],
    hover: Option<&Target>,
) {
    let family = theme.menu.item_font.family.clone();
    // The chiseled face and its raised bevel: the same chrome every
    // chonkstep window wears, from the live theme.
    paint::fill_area(pixmap, 0, 0, m.width, m.height, &Fill::Solid(face_color(theme)));
    paint::draw_bevel(pixmap, 0, 0, m.width, m.height, &theme.titlebar.bevel);

    draw_header(pixmap, theme, fonts, swash, m, &family);

    let (gx, gy, gw, gh) = glass_rect(m);
    let (gx, gy, gw, gh) = panel::draw_panel_glass(pixmap, gx, gy, gw, gh, theme);
    let pal = panel::panel_palette(theme);
    let hits = layout(m, phase, devices);

    match phase {
        Phase::Starting => {
            centered(pixmap, fonts, swash, &family, m, pal.ink_dim, gx, gy, gw, gh, "STARTING...");
        }
        Phase::Scanning => draw_list(pixmap, fonts, swash, &family, m, &pal, gx, gy, gw, gh, devices, &hits, hover),
        Phase::Pairing { address } => {
            centered(pixmap, fonts, swash, &family, m, pal.ink, gx, gy, gw, gh / 2, "PAIRING...");
            centered(pixmap, fonts, swash, &family, m, pal.ink_dim, gx, gy + (gh / 2) as i32, gw, gh / 2, address);
        }
        Phase::Confirm { passkey, .. } => {
            prompt(pixmap, fonts, swash, &family, m, &pal, gx, gy, gw, "SAME DIGITS ON THE DEVICE?", passkey);
            button(pixmap, theme, fonts, swash, &family, m, &hits, Target::Yes, "YES", hover);
            button(pixmap, theme, fonts, swash, &family, m, &hits, Target::No, "NO", hover);
        }
        Phase::DisplayPasskey { passkey, .. } => {
            prompt(pixmap, fonts, swash, &family, m, &pal, gx, gy, gw, "TYPE THIS ON THE DEVICE", passkey);
        }
        Phase::NeedsKeyboard { .. } => {
            wrapped(
                pixmap,
                fonts,
                swash,
                &family,
                m,
                &pal,
                gx,
                gy,
                gw,
                &["THIS DEVICE WANTS A PIN TYPED", "HERE, AND THIS WINDOW HAS NO", "KEYBOARD. USE bluetoothctl."],
            );
            button(pixmap, theme, fonts, swash, &family, m, &hits, Target::Rescan, "BACK TO THE LIST", hover);
        }
        Phase::Paired { address } => {
            centered(pixmap, fonts, swash, &family, m, pal.ink, gx, gy + m.si(16), gw, m.s(24), "PAIRED");
            centered(pixmap, fonts, swash, &family, m, pal.ink_dim, gx, gy + m.si(44), gw, m.s(20), address);
            button(pixmap, theme, fonts, swash, &family, m, &hits, Target::Rescan, "PAIR ANOTHER", hover);
        }
        Phase::Failed { reason, .. } => {
            centered(pixmap, fonts, swash, &family, m, pal.ink, gx, gy + m.si(16), gw, m.s(24), "NOT PAIRED");
            centered(pixmap, fonts, swash, &family, m, pal.ink_dim, gx, gy + m.si(44), gw, m.s(20), &reason.to_uppercase());
            button(pixmap, theme, fonts, swash, &family, m, &hits, Target::Rescan, "TRY AGAIN", hover);
        }
        Phase::Unavailable { reason } => {
            // The face this machine actually shows, and every machine
            // without a controller: the dead screen's message, on the
            // dialog's own glass.
            centered(pixmap, fonts, swash, &family, m, pal.ink_dim, gx, gy, gw, gh / 2, "NO BLUETOOTH");
            centered(pixmap, fonts, swash, &family, m, pal.ghost, gx, gy + (gh / 2) as i32, gw, gh / 2, &reason.to_uppercase());
        }
    }
}

/// The window's face color. The theme's menu background is the closest
/// thing to "a panel of chrome" the palette names, so the dialog wears
/// it rather than a literal grey — a restyle takes this window with it.
fn face_color(theme: &Theme) -> Color {
    match &theme.menu.background {
        Fill::Solid(color) => *color,
        Fill::Gradient(gradient) => gradient.from,
    }
}

/// The header strip: the instrument's own rune, and the window's name.
fn draw_header(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &Metrics,
    family: &str,
) {
    let rune = m.s(22);
    // The same mark the dock tile wears, from the same function.
    draw_bt_rune(pixmap, m.si(14), m.si(12), rune, rune, theme.titlebar.text_color_active);
    let font = FontSpec {
        family: family.to_string(),
        size: m.s(13) as f32,
        weight: FontWeight::Bold,
        style: FontStyle::Normal,
    };
    paint::draw_text(
        pixmap,
        fonts,
        swash,
        "PAIR A BLUETOOTH DEVICE",
        &font,
        theme.titlebar.text_color_active,
        m.si(14) + rune as i32 + m.si(10),
        m.si(12),
        m.width,
        rune,
        TextAlign::Left,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_list(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    family: &str,
    m: &Metrics,
    pal: &panel::PanelPalette,
    gx: i32,
    gy: i32,
    gw: u32,
    gh: u32,
    devices: &[&Found],
    hits: &Layout,
    hover: Option<&Target>,
) {
    if devices.is_empty() {
        centered(pixmap, fonts, swash, family, m, pal.ink_dim, gx, gy, gw, gh, "SCANNING...");
        return;
    }
    let row_h = row_height(m);
    let font = FontSpec { family: family.to_string(), size: (row_h as f32 * 0.46).max(7.0), weight: FontWeight::Bold, style: FontStyle::Normal };
    for (index, device) in devices.iter().take(MAX_ROWS).enumerate() {
        let y = gy + (index as u32 * row_h) as i32;
        let hovered = matches!(hover, Some(Target::Device(address)) if *address == device.address);
        if hovered {
            paint::fill_rect(pixmap, gx, y, gw, row_h, pal.ink_dim);
        }
        let lamp = m.s(6);
        paint::fill_rect(pixmap, gx + m.si(6), y + ((row_h - lamp) / 2) as i32, lamp, lamp, if device.paired { pal.ink } else { pal.ghost });
        let color = if hovered {
            pal.glass
        } else if device.paired {
            pal.ghost
        } else {
            pal.ink
        };
        let text_x = gx + m.si(6) + lamp as i32 + m.si(6);
        let label = if device.paired { format!("{} (PAIRED)", device.name.to_uppercase()) } else { device.name.to_uppercase() };
        paint::draw_text(pixmap, fonts, swash, &label, &font, color, text_x, y, gw.saturating_sub(m.s(20)), row_h, TextAlign::Left);
        // A hairline between rows, shaded rather than inked, so the
        // list reads as one instrument's face and not a list box.
        paint::op_rect(pixmap, gx, y + row_h as i32 - 1, gw, 1, -14);
    }
    if devices.len() > MAX_ROWS {
        let more = format!("+{} MORE", devices.len() - MAX_ROWS);
        let y = gy + (MAX_ROWS as u32 * row_h) as i32;
        paint::draw_text(pixmap, fonts, swash, &more, &font, pal.ghost, gx + m.si(6), y, gw, row_h, TextAlign::Left);
    }
    let _ = hits;
}

/// A prompt with a big passkey under it — the confirm and display
/// screens share this, because they show the same six digits and
/// differ only in what is being asked.
#[allow(clippy::too_many_arguments)]
fn prompt(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    family: &str,
    m: &Metrics,
    pal: &panel::PanelPalette,
    gx: i32,
    gy: i32,
    gw: u32,
    question: &str,
    passkey: &str,
) {
    centered(pixmap, fonts, swash, family, m, pal.ink_dim, gx, gy + m.si(10), gw, m.s(18), question);
    let font = FontSpec { family: family.to_string(), size: m.s(34) as f32, weight: FontWeight::Bold, style: FontStyle::Normal };
    paint::draw_text(pixmap, fonts, swash, passkey, &font, pal.ink, gx, gy + m.si(32), gw, m.s(44), TextAlign::Center);
}

#[allow(clippy::too_many_arguments)]
fn wrapped(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    family: &str,
    m: &Metrics,
    pal: &panel::PanelPalette,
    gx: i32,
    gy: i32,
    gw: u32,
    lines: &[&str],
) {
    // Pre-split rather than measured-wrapped: these are three fixed
    // sentences, and a wrapping engine for them would be ceremony.
    for (index, line) in lines.iter().enumerate() {
        centered(pixmap, fonts, swash, family, m, pal.ink_dim, gx, gy + m.si(12) + (index as u32 * m.s(16)) as i32, gw, m.s(16), line);
    }
}

#[allow(clippy::too_many_arguments)]
fn centered(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    family: &str,
    m: &Metrics,
    color: Color,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    text: &str,
) {
    let font = FontSpec { family: family.to_string(), size: m.s(12) as f32, weight: FontWeight::Bold, style: FontStyle::Normal };
    paint::draw_text(pixmap, fonts, swash, text, &font, color, x, y, w, h, TextAlign::Center);
}

/// One chiseled button, drawn at the rectangle [`layout`] assigned it —
/// so the thing that lights under the pointer is the thing the click
/// resolves to.
#[allow(clippy::too_many_arguments)]
fn button(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    family: &str,
    m: &Metrics,
    hits: &Layout,
    target: Target,
    label: &str,
    hover: Option<&Target>,
) {
    let Some(hit) = hits.hits.iter().find(|hit| hit.target == target) else { return };
    let (x, y, w, h) = hit.rect;
    let hovered = hover == Some(&target);
    paint::fill_area(pixmap, x, y, w, h, &Fill::Solid(face_color(theme)));
    let mut bevel = theme.titlebar.bevel;
    if hovered {
        bevel.style = chonk_ui::model::BevelStyle::Sunken;
    }
    paint::draw_bevel(pixmap, x, y, w, h, &bevel);
    let font = FontSpec { family: family.to_string(), size: m.s(12) as f32, weight: FontWeight::Bold, style: FontStyle::Normal };
    paint::draw_text(pixmap, fonts, swash, label, &font, theme.titlebar.text_color_active, x, y, w, h, TextAlign::Center);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pair::Found;

    fn found(address: &str, paired: bool) -> Found {
        Found { address: address.to_string(), name: address.to_string(), paired }
    }

    fn theme() -> Theme {
        chonk_ui::nextstep_theme()
    }

    /// Every phase must produce a paintable window at every scale this
    /// session can be in — including the one this machine reaches,
    /// which is `Unavailable`.
    #[test]
    fn every_phase_paints_at_every_scale() {
        let theme = theme();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let devices = [found("AA:BB:CC:DD:EE:FF", false), found("F8:4E:17:00:11:22", true)];
        let refs: Vec<&Found> = devices.iter().collect();
        let phases = [
            Phase::Starting,
            Phase::Scanning,
            Phase::Pairing { address: "AA:BB:CC:DD:EE:FF".into() },
            Phase::Confirm { address: "AA:BB:CC:DD:EE:FF".into(), passkey: "123456".into() },
            Phase::DisplayPasskey { address: "AA:BB:CC:DD:EE:FF".into(), passkey: "042318".into() },
            Phase::NeedsKeyboard { address: "AA:BB:CC:DD:EE:FF".into() },
            Phase::Paired { address: "AA:BB:CC:DD:EE:FF".into() },
            Phase::Failed { address: "AA:BB:CC:DD:EE:FF".into(), reason: "AuthenticationFailed".into() },
            Phase::Unavailable { reason: "no Bluetooth controller".into() },
        ];
        for scale in [1.0f32, 2.0] {
            let m = Metrics::new(scale);
            for phase in &phases {
                let mut pixmap = Pixmap::new(m.width, m.height).expect("nonzero window");
                draw(&mut pixmap, &theme, &mut fonts, &mut swash, &m, phase, &refs, None);
                assert_eq!((pixmap.width(), pixmap.height()), (m.width, m.height), "{phase:?} at {scale}x");
            }
        }
    }

    /// A click resolves to the row it was drawn on, and an
    /// already-paired device is not offered again.
    #[test]
    fn the_list_hit_test_matches_the_rows_it_draws() {
        let m = Metrics::new(1.0);
        let devices = [found("AA:BB:CC:DD:EE:FF", false), found("F8:4E:17:00:11:22", true)];
        let refs: Vec<&Found> = devices.iter().collect();
        let hits = layout(&m, &Phase::Scanning, &refs);
        assert_eq!(hits.hits.len(), 1, "a paired device stays visible but is not a target");

        let (x, y, w, h) = hits.hits[0].rect;
        assert_eq!(hits.at(x + w as i32 / 2, y + h as i32 / 2), Some(&Target::Device("AA:BB:CC:DD:EE:FF".to_string())));
        assert_eq!(hits.at(x - 1, y + 1), None, "outside the row is chrome");
        assert_eq!(hits.at(x + 1, y - 1), None);
    }

    #[test]
    fn the_confirm_phase_offers_exactly_two_buttons_that_do_not_overlap() {
        let m = Metrics::new(1.0);
        let hits = layout(&m, &Phase::Confirm { address: "A".into(), passkey: "123456".into() }, &[]);
        assert_eq!(hits.hits.len(), 2);
        let yes = hits.hits.iter().find(|h| h.target == Target::Yes).expect("yes");
        let no = hits.hits.iter().find(|h| h.target == Target::No).expect("no");
        assert!(yes.rect.0 + yes.rect.2 as i32 <= no.rect.0, "the two answers must not overlap");
        // And each resolves to itself.
        assert_eq!(hits.at(yes.rect.0 + 2, yes.rect.1 + 2), Some(&Target::Yes));
        assert_eq!(hits.at(no.rect.0 + 2, no.rect.1 + 2), Some(&Target::No));
    }

    /// The phases with nothing to answer must offer nothing to click —
    /// a button that cannot work is worse than no button.
    #[test]
    fn the_waiting_phases_offer_no_targets() {
        let m = Metrics::new(1.0);
        for phase in [
            Phase::Pairing { address: "A".into() },
            Phase::DisplayPasskey { address: "A".into(), passkey: "1".into() },
            Phase::Unavailable { reason: "no Bluetooth controller".into() },
        ] {
            assert!(layout(&m, &phase, &[]).hits.is_empty(), "{phase:?} must offer nothing");
        }
    }

    #[test]
    fn the_finished_phases_all_offer_the_way_back() {
        let m = Metrics::new(1.0);
        for phase in [
            Phase::Paired { address: "A".into() },
            Phase::Failed { address: "A".into(), reason: "x".into() },
            Phase::NeedsKeyboard { address: "A".into() },
        ] {
            let hits = layout(&m, &phase, &[]);
            assert_eq!(hits.hits.len(), 1);
            assert_eq!(hits.hits[0].target, Target::Rescan, "{phase:?}");
        }
    }

    /// A long list is capped rather than drawn off the glass.
    #[test]
    fn a_crowded_room_caps_the_list() {
        let m = Metrics::new(1.0);
        let devices: Vec<Found> = (0..20).map(|i| found(&format!("AA:BB:CC:DD:EE:{i:02X}"), false)).collect();
        let refs: Vec<&Found> = devices.iter().collect();
        assert_eq!(layout(&m, &Phase::Scanning, &refs).hits.len(), MAX_ROWS);
    }
}
