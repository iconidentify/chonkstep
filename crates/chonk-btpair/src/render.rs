//! The dialog's face: the chiseled chrome, the sunken well, and the
//! rows and buttons the pointer can land on.
//!
//! Layout and drawing live in one place on purpose: [`layout`] returns
//! the rectangles, [`draw`] paints them, and the click handler
//! hit-tests against the *same* rectangles, so the pixels and the
//! hitboxes cannot drift apart.
//!
//! # Chrome, not glass
//!
//! This window used to be drawn on the instruments' LED glass — a
//! recessed screen with `panel_palette` ink on it — because it is
//! spawned by a Bluetooth panel and wears the Bluetooth rune. That was
//! the wrong family, and its sibling says why. `chonk-netjoin` is the
//! other dialog a dock panel spawns for a job a panel may not do
//! itself, and its module doc puts it plainly: *the panel next door
//! draws on an LED screen because it is an instrument; this is a
//! window, decorated by the same window manager that decorates a
//! terminal, so it wears the app vocabulary instead.* Two dialogs from
//! one desktop that did not agree about that read as two desktops.
//!
//! So this window now speaks the same vocabulary as the join dialog,
//! element for element:
//!
//! | Element | Recipe |
//! |---|---|
//! | surface | `theme.menu.background` + `theme.menu.bevel` |
//! | title | `theme.menu.title_font` in `menu.text_color` |
//! | caption | the item font, one step dim |
//! | the well | a sunken bevel over `field_tone` — the join dialog's passphrase field, grown to hold a list |
//! | a list row | the menu's own highlight bar under the pointer, in `menu.highlight_text_color` |
//! | a button | [`paint::draw_button`] on the menu fill and bevel, sinking one pixel when pressed |
//! | a complaint | the highlight bar behind the reason, exactly as a failed join reports one |
//!
//! Its geometry constants are the join dialog's too (`MARGIN`,
//! `TITLE_H`, `BUTTON_W`, `BUTTON_H`, `BUTTON_GAP`), so the two
//! windows share a rhythm and not just a palette.
//!
//! The one deliberate difference is the mark: the join dialog's subject
//! is a network name, which is its title, while this dialog's subject
//! is Bluetooth itself — so the instrument's own rune sits beside the
//! title, drawn by the very function the dock tile draws it with.

use chonk_ui::model::{Color, Fill, FontSpec, TextAlign, Theme};
use chonk_ui::paint;
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

// The join dialog's own logical metrics, so the two windows a dock
// panel can spawn are laid out on one grid.
const MARGIN: i32 = 12;
const TITLE_H: u32 = 20;
const CAPTION_H: u32 = 13;
const ROW_H: u32 = 22;
const BUTTON_W: u32 = 88;
const BUTTON_H: u32 = 26;
const BUTTON_GAP: i32 = 8;

/// A placed box in device pixels — the join dialog's own `Rect`, so
/// the two windows' geometry is written the same way as well as laid
/// out on the same grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w as i32 && y >= self.y && y < self.y + self.h as i32
    }

    /// The rectangle as the tuple [`Hit`] carries. `Hit::rect` is a
    /// tuple because the click handler in `main.rs` destructures it,
    /// and changing that is a wider edit than this file.
    fn tuple(self) -> (i32, i32, u32, u32) {
        (self.x, self.y, self.w, self.h)
    }
}

/// The footer's two button slots: the action on the right, the way out
/// to its left. Named rather than returned as a pair of tuples —
/// which one is which is the whole content of this value, and a
/// `((i32, i32, u32, u32), (i32, i32, u32, u32))` says none of it.
#[derive(Clone, Copy, Debug)]
struct ButtonSlots {
    /// Where the join dialog puts `CANCEL`.
    left: Rect,
    /// Where the join dialog puts `JOIN`.
    right: Rect,
}

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
                Rect { x: hx, y: hy, w: hw, h: hh }.contains(x, y)
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
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
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

/// The well: the sunken field everything below the caption is drawn
/// in, stopping one button-row short of the bottom margin.
fn well_rect(m: &Metrics) -> (i32, i32, u32, u32) {
    let margin = m.si(MARGIN as u32);
    let top = margin + m.si(TITLE_H) + m.si(6) + m.si(CAPTION_H) + m.si(4);
    let bottom = m.height as i32 - margin - m.si(BUTTON_H) - m.si(BUTTON_GAP as u32);
    let w = (m.width as i32 - margin * 2).max(1) as u32;
    let h = (bottom - top).max(1) as u32;
    (margin, top, w, h)
}

/// One list row's height.
fn row_height(m: &Metrics) -> u32 {
    m.s(ROW_H)
}

/// The footer's two button rectangles, anchored to the bottom-right
/// corner the way the join dialog anchors its own — to the margins
/// rather than to a running cursor, so a scale that rounds the stack a
/// pixel short still leaves the buttons where the frame expects them.
fn button_slots(m: &Metrics) -> ButtonSlots {
    let margin = m.si(MARGIN as u32);
    let bw = m.s(BUTTON_W);
    let bh = m.s(BUTTON_H);
    let by = m.height as i32 - margin - bh as i32;
    let right = Rect { x: m.width as i32 - margin - bw as i32, y: by, w: bw, h: bh };
    let left = Rect { x: right.x - m.si(BUTTON_GAP as u32) - bw as i32, y: by, w: bw, h: bh };
    ButtonSlots { left, right }
}

/// Computes the hitboxes for a phase. Called by both [`draw`] and the
/// click handler, which is what keeps them honest.
pub fn layout(m: &Metrics, phase: &Phase, devices: &[&Found]) -> Layout {
    let (wx, wy, ww, _) = well_rect(m);
    let slots = button_slots(m);
    let mut hits = Vec::new();
    match phase {
        Phase::Scanning | Phase::Starting => {
            let row_h = row_height(m);
            let t = m.si(1);
            for (index, device) in devices.iter().take(MAX_ROWS).enumerate() {
                if device.paired {
                    // Already paired: shown so the list is not
                    // confusing, but not offered again.
                    continue;
                }
                hits.push(Hit {
                    target: Target::Device(device.address.clone()),
                    rect: (wx + t, wy + t + (index as u32 * row_h) as i32, ww.saturating_sub(t as u32 * 2), row_h),
                });
            }
        }
        Phase::Confirm { .. } => {
            hits.push(Hit { target: Target::No, rect: slots.left.tuple() });
            hits.push(Hit { target: Target::Yes, rect: slots.right.tuple() });
        }
        Phase::Paired { .. } | Phase::Failed { .. } | Phase::NeedsKeyboard { .. } => {
            hits.push(Hit { target: Target::Rescan, rect: slots.right.tuple() });
        }
        Phase::Pairing { .. } | Phase::DisplayPasskey { .. } | Phase::Unavailable { .. } => {}
    }
    Layout { hits }
}

/// The caption under the title: what this window is doing right now.
/// The join dialog's caption names its one field; this one names the
/// phase, because this window's content changes and that one's does
/// not.
fn caption(phase: &Phase) -> &'static str {
    match phase {
        Phase::Starting => "STARTING THE PAIRING AGENT",
        Phase::Scanning => "DEVICES IN RANGE",
        Phase::Pairing { .. } => "PAIRING",
        Phase::Confirm { .. } => "CONFIRM THE PASSKEY",
        Phase::DisplayPasskey { .. } => "TYPE THIS ON THE DEVICE",
        Phase::NeedsKeyboard { .. } => "THIS DEVICE WANTS A PIN",
        Phase::Paired { .. } => "DONE",
        Phase::Failed { .. } => "NOT PAIRED",
        Phase::Unavailable { .. } => "UNAVAILABLE",
    }
}

/// The label on the right-hand button, and `None` when the phase has
/// nothing for it to do.
fn action_label(phase: &Phase) -> Option<&'static str> {
    match phase {
        Phase::Confirm { .. } => Some("YES"),
        Phase::Paired { .. } => Some("PAIR ANOTHER"),
        Phase::Failed { .. } => Some("TRY AGAIN"),
        Phase::NeedsKeyboard { .. } => Some("BACK"),
        _ => None,
    }
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
    // The surface: the menu's own fill and chisel, so the dialog's
    // interior matches the desktop's other panels — and its sibling
    // dialog — exactly.
    paint::fill_area(pixmap, 0, 0, m.width, m.height, &theme.menu.background);
    paint::draw_bevel(pixmap, 0, 0, m.width, m.height, &theme.menu.bevel);

    let ink = theme.menu.text_color;
    let dim = mix(ink, fill_tone(&theme.menu.background), 0.58);
    // The theme's fonts, at the size the theme hands them over. A
    // chonkstep app is given an already-scaled theme
    // (`chonk_ui::scaled_theme` is `active_theme().scaled(scale)`), so
    // multiplying by the scale here again would set this window's type
    // at four times the desk's on a 2x display — which is exactly what
    // the first pass at this did, and what the side-by-side against
    // the join dialog caught.
    let body = theme.menu.item_font.clone();
    let title_font = theme.menu.title_font.clone();

    let margin = m.si(MARGIN as u32);
    let inner_w = (m.width as i32 - margin * 2).max(1) as u32;

    // The header: the instrument's own mark, then the window's subject.
    let rune = m.s(TITLE_H);
    draw_bt_rune(pixmap, margin, margin, rune, rune, ink);
    let title_x = margin + rune as i32 + m.si(8);
    let title_w = (m.width as i32 - margin - title_x).max(0) as u32;
    paint::draw_text(pixmap, fonts, swash, "BLUETOOTH", &title_font, ink, title_x, margin, title_w, m.s(TITLE_H), TextAlign::Left);

    let caption_y = margin + m.si(TITLE_H) + m.si(6);
    paint::draw_text(pixmap, fonts, swash, caption(phase), &body, dim, margin, caption_y, inner_w, m.s(CAPTION_H), TextAlign::Left);

    // The well: the join dialog's passphrase field, grown to hold
    // whatever this phase has to show.
    let (wx, wy, ww, wh) = well_rect(m);
    let t = theme.menu.bevel.width.max(1) as u32;
    paint::fill_rect(pixmap, wx, wy, ww, wh, field_tone(theme));
    paint::draw_sunken_bevel(pixmap, wx, wy, ww, wh, t);
    let (ix, iy, iw, ih) = (wx + t as i32, wy + t as i32, ww.saturating_sub(t * 2), wh.saturating_sub(t * 2));

    let hits = layout(m, phase, devices);
    // The well's ink is the surface's ink, exactly as it is next door:
    // `field_tone` pushes the paper *away* from the ink rather than
    // toward it (that is the whole argument in its doc), so the
    // theme's own colour always reads on its own well — and a
    // Bluetooth dialog whose list was white text while the join
    // dialog's field was amber would be two desktops again.
    let well_ink = ink;
    let well_dim = mix(well_ink, field_tone(theme), 0.55);

    match phase {
        Phase::Starting => centered(pixmap, fonts, swash, &body, well_dim, ix, iy, iw, ih, "STARTING…"),
        Phase::Scanning => {
            draw_list(pixmap, fonts, swash, theme, m, &body, ix, iy, iw, ih, devices, hover, well_ink, well_dim)
        }
        Phase::Pairing { address } => {
            centered(pixmap, fonts, swash, &body, well_ink, ix, iy, iw, ih / 2, "PAIRING…");
            centered(pixmap, fonts, swash, &body, well_dim, ix, iy + (ih / 2) as i32, iw, ih / 2, address);
        }
        Phase::Confirm { passkey, .. } => {
            passkey_plate(pixmap, fonts, swash, m, &body, well_ink, well_dim, ix, iy, iw, ih, "SAME DIGITS ON THE DEVICE?", passkey)
        }
        Phase::DisplayPasskey { passkey, .. } => {
            passkey_plate(pixmap, fonts, swash, m, &body, well_ink, well_dim, ix, iy, iw, ih, "ENTER THESE DIGITS THERE", passkey)
        }
        Phase::NeedsKeyboard { .. } => wrapped(
            pixmap,
            fonts,
            swash,
            m,
            &body,
            well_dim,
            ix,
            iy,
            iw,
            &["THIS DEVICE WANTS A PIN TYPED", "HERE, AND THIS WINDOW HAS NO", "KEYBOARD. USE bluetoothctl."],
        ),
        Phase::Paired { address } => {
            centered(pixmap, fonts, swash, &body, well_ink, ix, iy + m.si(16), iw, m.s(20), "PAIRED");
            centered(pixmap, fonts, swash, &body, well_dim, ix, iy + m.si(40), iw, m.s(20), address);
        }
        Phase::Failed { address, reason } => {
            // The complaint sits flush against the top of the well.
            // Floated a margin down it leaves a bare strip of paper
            // above it that reads as a rendering artifact rather than
            // as space.
            complaint(pixmap, fonts, swash, theme, m, &body, ix, iy, iw, &reason.to_uppercase());
            // Which device refused, under the complaint: a failure
            // with no subject is a failure someone has to guess about.
            centered(pixmap, fonts, swash, &body, well_dim, ix, iy + m.si(26), iw, m.s(20), address);
        }
        Phase::Unavailable { reason } => {
            // The face this machine actually shows, and every machine
            // without a controller.
            centered(pixmap, fonts, swash, &body, well_ink, ix, iy + m.si(16), iw, m.s(20), "NO BLUETOOTH");
            centered(pixmap, fonts, swash, &body, well_dim, ix, iy + m.si(40), iw, m.s(20), &reason.to_uppercase());
        }
    }

    // The footer. `NO` sits where the join dialog's `CANCEL` does and
    // the phase's action sits where its `JOIN` does, so a hand that has
    // used one window already knows where the buttons are in the other.
    if matches!(phase, Phase::Confirm { .. }) {
        button(pixmap, theme, fonts, swash, &body, &hits, Target::No, "NO", hover, ink, dim);
    }
    if let Some(label) = action_label(phase) {
        let target = if matches!(phase, Phase::Confirm { .. }) { Target::Yes } else { Target::Rescan };
        button(pixmap, theme, fonts, swash, &body, &hits, target, label, hover, ink, dim);
    }
}

/// The discovery list: the desktop's own menu-row idiom, inside the
/// well. A hovered row takes the menu highlight bar, which is what
/// "the pointer is on this one" looks like everywhere else on this
/// desk; a device already paired is shown and receded rather than
/// offered again.
#[allow(clippy::too_many_arguments)]
fn draw_list(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    theme: &Theme,
    m: &Metrics,
    body: &FontSpec,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    devices: &[&Found],
    hover: Option<&Target>,
    ink: Color,
    dim: Color,
) {
    if devices.is_empty() {
        centered(pixmap, fonts, swash, body, dim, x, y, w, h, "SCANNING…");
        return;
    }
    let row_h = row_height(m);
    let pad = m.si(6);
    // The address is set one step smaller than the name: it is a
    // disambiguator, not a label, and at the name's size a MAC takes
    // half the row and gets cut in the middle — which is the one part
    // of it that distinguishes two of the same headphones. The column
    // is measured against the widest address actually in the list, not
    // against a template, because a template that shapes one pixel
    // narrower than the real string clips exactly one character off
    // every row.
    let addr_font = FontSpec { size: (body.size * 0.84).max(6.0), ..body.clone() };
    let addr_w = devices
        .iter()
        .take(MAX_ROWS)
        .map(|device| paint::text_width(fonts, &addr_font, &device.address))
        .max()
        .unwrap_or(0)
        + m.s(2);
    for (index, device) in devices.iter().take(MAX_ROWS).enumerate() {
        let ry = y + (index as u32 * row_h) as i32;
        let hovered = matches!(hover, Some(Target::Device(address)) if *address == device.address) && !device.paired;
        let color = if hovered {
            paint::fill_area(pixmap, x, ry, w, row_h, &theme.menu.highlight_background);
            theme.menu.highlight_text_color
        } else if device.paired {
            dim
        } else {
            ink
        };
        let label = if device.paired { format!("{} — PAIRED", device.name.to_uppercase()) } else { device.name.to_uppercase() };
        let name_w = w.saturating_sub((pad * 2) as u32 + addr_w + m.s(8));
        let label = fit(fonts, body, &label, name_w);
        paint::draw_text(pixmap, fonts, swash, &label, body, color, x + pad, ry, name_w, row_h, TextAlign::Left);
        let addr_color = if hovered { theme.menu.highlight_text_color } else { dim };
        paint::draw_text(
            pixmap,
            fonts,
            swash,
            &device.address,
            &addr_font,
            addr_color,
            x + w as i32 - addr_w as i32 - pad,
            ry,
            addr_w,
            row_h,
            TextAlign::Right,
        );
    }
    if devices.len() > MAX_ROWS {
        let more = format!("+{} MORE IN RANGE", devices.len() - MAX_ROWS);
        let ry = y + (MAX_ROWS as u32 * row_h) as i32;
        paint::draw_text(pixmap, fonts, swash, &more, body, dim, x + pad, ry, w, row_h, TextAlign::Left);
    }
}

/// A question with the six digits under it. The confirm and display
/// screens share this, because they show the same passkey and differ
/// only in what is being asked about it.
#[allow(clippy::too_many_arguments)]
fn passkey_plate(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &Metrics,
    body: &FontSpec,
    ink: Color,
    dim: Color,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    question: &str,
    passkey: &str,
) {
    centered(pixmap, fonts, swash, body, dim, x, y + m.si(12), w, m.s(18), question);
    // The passkey is the one thing on this screen a person compares
    // against another screen across the room, so it is the largest
    // type this window sets, by a long way.
    let big = FontSpec { size: m.s(34) as f32, ..body.clone() };
    paint::draw_text(pixmap, fonts, swash, passkey, &big, ink, x, y + (h / 2) as i32 - m.si(20), w, m.s(46), TextAlign::Center);
}

/// A failure, in the one treatment this vocabulary has for "read this":
/// the menu highlight bar behind the reason, with the highlight's own
/// ink on it — exactly how a refused join reports itself next door.
#[allow(clippy::too_many_arguments)]
fn complaint(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    theme: &Theme,
    m: &Metrics,
    body: &FontSpec,
    x: i32,
    y: i32,
    w: u32,
    reason: &str,
) {
    let bar_h = m.s(18);
    paint::fill_area(pixmap, x, y, w, bar_h, &theme.menu.highlight_background);
    let pad = m.si(4);
    let text = fit(fonts, body, reason, w.saturating_sub(pad as u32 * 2));
    paint::draw_text(
        pixmap,
        fonts,
        swash,
        &text,
        body,
        theme.menu.highlight_text_color,
        x + pad,
        y,
        w.saturating_sub(pad as u32 * 2),
        bar_h,
        TextAlign::Left,
    );
}

#[allow(clippy::too_many_arguments)]
fn wrapped(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    m: &Metrics,
    body: &FontSpec,
    color: Color,
    x: i32,
    y: i32,
    w: u32,
    lines: &[&str],
) {
    // Pre-split rather than measured-wrapped: these are three fixed
    // sentences, and a wrapping engine for them would be ceremony.
    for (index, line) in lines.iter().enumerate() {
        centered(pixmap, fonts, swash, body, color, x, y + m.si(14) + (index as u32 * m.s(18)) as i32, w, m.s(18), line);
    }
}

#[allow(clippy::too_many_arguments)]
fn centered(
    pixmap: &mut Pixmap,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    body: &FontSpec,
    color: Color,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    text: &str,
) {
    paint::draw_text(pixmap, fonts, swash, text, body, color, x, y, w, h, TextAlign::Center);
}

/// One chiseled button, drawn at the rectangle [`layout`] assigned it —
/// so the thing that lights under the pointer is the thing the click
/// resolves to. The join dialog's recipe: the menu's fill and bevel
/// through [`paint::draw_button`], with the label sinking one pixel
/// when the face does.
///
/// This window has no keyboard (see the crate doc's `DisplayYesNo`
/// argument), so there is no focus ring to draw and hover is the only
/// state a button has before it is pressed.
#[allow(clippy::too_many_arguments)]
fn button(
    pixmap: &mut Pixmap,
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    swash: &mut cosmic_text::SwashCache,
    body: &FontSpec,
    hits: &Layout,
    target: Target,
    label: &str,
    hover: Option<&Target>,
    ink: Color,
    _dim: Color,
) {
    let Some(hit) = hits.hits.iter().find(|hit| hit.target == target) else { return };
    let (x, y, w, h) = hit.rect;
    let hovered = hover == Some(&target);
    paint::draw_button(pixmap, x, y, w, h, &theme.menu.background, &theme.menu.bevel, false);
    if hovered {
        // The face lifts under the pointer. A hovered button must not
        // borrow the *pressed* look, or a person cannot tell what the
        // mouse is about to do from what it is already doing.
        let t = theme.menu.bevel.width.max(1) as u32;
        paint::op_rect(pixmap, x + t as i32, y + t as i32, w.saturating_sub(t * 2), h.saturating_sub(t * 2), 22);
    }
    let label = fit(fonts, body, label, w);
    paint::draw_text(pixmap, fonts, swash, &label, body, ink, x, y, w, h, TextAlign::Center);
}

/// Measured truncation with an ellipsis: a string too wide for its box
/// loses characters, never its box.
fn fit(fonts: &mut cosmic_text::FontSystem, spec: &FontSpec, text: &str, max_w: u32) -> String {
    if paint::text_width(fonts, spec, text) <= max_w {
        return text.to_string();
    }
    let mut out = text.to_string();
    while !out.is_empty() && paint::text_width(fonts, spec, &format!("{out}…")) > max_w {
        out.pop();
    }
    format!("{out}…")
}

/// A flat representative color for a `Fill`, so tones can be derived
/// from it. A gradient's midpoint is what the eye reads as "the
/// surface color" over a control this small.
fn fill_tone(fill: &Fill) -> Color {
    match fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => mix(g.from, g.to, 0.5),
    }
}

/// The inside of a sunken well — the join dialog's [`field_tone`],
/// character for character, because two windows whose wells were
/// different shades of paper would be exactly the mismatch this
/// re-skin is fixing.
fn field_tone(theme: &Theme) -> Color {
    let surface = fill_tone(&theme.menu.background);
    let ink = theme.menu.text_color;
    let paper = if luminance(ink) < 128.0 { Color::rgb(0xFF, 0xFF, 0xFF) } else { Color::rgb(0, 0, 0) };
    mix(paper, surface, 0.72)
}

/// Perceived brightness, the usual weighting.
fn luminance(c: Color) -> f32 {
    0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32
}

/// `t` of `a`, the rest of `b`.
fn mix(a: Color, b: Color, t: f32) -> Color {
    let f = |x: u8, y: u8| ((x as f32) * t + (y as f32) * (1.0 - t)).round().clamp(0.0, 255.0) as u8;
    Color::rgb(f(a.r, b.r), f(a.g, b.g), f(a.b, b.b))
}

#[cfg(test)]
mod tests;
