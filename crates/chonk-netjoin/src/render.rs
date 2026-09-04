//! Where everything sits, and how it is drawn. Both pure: [`layout`]
//! is a function of the scale factor alone, and
//! [`render_join_dialog`] is a function of a [`DialogView`] and a
//! theme. No X connection reaches this module, which is what lets the
//! click tests drive synthetic presses through the very same [`Layout`]
//! the pixels came from.
//!
//! # Chrome, not glass
//!
//! The link panel next door draws on an LED screen, because it is an
//! instrument. This is a *window*: a person types into it, and it is
//! decorated by the same window manager that decorates a terminal. So
//! it wears the app vocabulary instead — the menu surface's fill and
//! bevel, a sunken well for the field, raised buttons that sink when
//! pressed — and comes out looking like the rest of the desktop rather
//! than like a dock tile that escaped.

use chonk_ui::model::{Color, FontSpec, TextAlign, Theme};
use chonk_ui::paint;
use cosmic_text::{FontSystem, SwashCache};
use tiny_skia::Pixmap;

use crate::dialog::{DialogView, Focus, Phase, Target};

/// The dialog's logical (1x) size. Wide enough for a 40-character
/// error line at the item font, tall enough that nothing has to share
/// a row.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 174;

/// Logical metrics. Named because every one of them appears twice —
/// once in [`layout`] and once in a test's expectation of it.
const MARGIN: i32 = 12;
const TITLE_H: u32 = 20;
const CAPTION_H: u32 = 13;
const FIELD_H: u32 = 24;
const REVEAL_H: u32 = 16;
const STATUS_H: u32 = 18;
const BUTTON_W: u32 = 88;
const BUTTON_H: u32 = 26;
const BUTTON_GAP: i32 = 8;

/// A placed box in device pixels.
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

    fn center(&self) -> (i32, i32) {
        (self.x + self.w as i32 / 2, self.y + self.h as i32 / 2)
    }
}

/// Every box the dialog draws or clicks, at one scale.
#[derive(Clone, Debug)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub title: Rect,
    pub caption: Rect,
    pub field: Rect,
    /// The whole clickable reveal row — box plus its label, because a
    /// 12-pixel checkbox is a cruel target and the label has always
    /// been part of the control.
    pub reveal_row: Rect,
    pub reveal_box: Rect,
    pub status: Rect,
    pub join: Rect,
    pub cancel: Rect,
}

impl Layout {
    /// The control under a window-local point, if any.
    pub fn hit(&self, x: i32, y: i32) -> Option<Target> {
        // Buttons first: they are disjoint from everything, and
        // checking them first keeps this readable as a priority list.
        if self.join.contains(x, y) {
            return Some(Target::Join);
        }
        if self.cancel.contains(x, y) {
            return Some(Target::Cancel);
        }
        if self.reveal_row.contains(x, y) {
            return Some(Target::Reveal);
        }
        if self.field.contains(x, y) {
            return Some(Target::Field);
        }
        None
    }

    /// The middle of a control, for a synthetic click.
    pub fn center(&self, target: Target) -> Option<(i32, i32)> {
        Some(match target {
            Target::Field => self.field.center(),
            Target::Reveal => self.reveal_row.center(),
            Target::Join => self.join.center(),
            Target::Cancel => self.cancel.center(),
        })
    }
}

/// Places every box at `scale`. The one geometry authority: the
/// renderer draws from this and the hit test reads it, so a control
/// cannot be drawn somewhere it cannot be clicked.
pub fn layout(scale: f32) -> Layout {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let s = |v: i32| ((v as f32) * scale).round() as i32;
    let su = |v: u32| ((v as f32) * scale).round().max(1.0) as u32;

    let width = su(WIDTH);
    let height = su(HEIGHT);
    let margin = s(MARGIN);
    let inner_w = (width as i32 - margin * 2).max(1) as u32;

    let mut y = margin;
    let title = Rect { x: margin, y, w: inner_w, h: su(TITLE_H) };
    y += su(TITLE_H) as i32 + s(6);
    let caption = Rect { x: margin, y, w: inner_w, h: su(CAPTION_H) };
    y += su(CAPTION_H) as i32 + s(2);
    let field = Rect { x: margin, y, w: inner_w, h: su(FIELD_H) };
    y += su(FIELD_H) as i32 + s(8);
    let reveal_h = su(REVEAL_H);
    let reveal_row = Rect { x: margin, y, w: inner_w, h: reveal_h };
    let box_side = su(12);
    let reveal_box = Rect { x: margin, y: y + ((reveal_h.saturating_sub(box_side)) / 2) as i32, w: box_side, h: box_side };
    y += reveal_h as i32 + s(6);
    let status = Rect { x: margin, y, w: inner_w, h: su(STATUS_H) };

    // The button row is anchored to the bottom margin rather than to
    // the running cursor, so a scale that rounds the stack a pixel
    // short still leaves the buttons where the frame expects them.
    let button_w = su(BUTTON_W);
    let button_h = su(BUTTON_H);
    let button_y = height as i32 - margin - button_h as i32;
    let join = Rect { x: width as i32 - margin - button_w as i32, y: button_y, w: button_w, h: button_h };
    let cancel = Rect { x: join.x - s(BUTTON_GAP) - button_w as i32, y: button_y, w: button_w, h: button_h };

    Layout { width, height, title, caption, field, reveal_row, reveal_box, status, join, cancel }
}

/// The label on the action button. It stops saying "Join" the moment
/// joining is no longer what it does.
fn join_label(phase: &Phase) -> &'static str {
    match phase {
        Phase::Joining => "JOINING…",
        Phase::Joined => "CLOSE",
        _ => "JOIN",
    }
}

/// The status line, and whether it is a complaint. `None` while there
/// is nothing to report — an empty row rather than reassuring filler.
fn status_line(view: &DialogView) -> Option<(String, bool)> {
    match &view.phase {
        Phase::Editing => None,
        Phase::Joining => Some(("ASKING NETWORKMANAGER TO CONNECT…".to_string(), false)),
        Phase::Joined => Some((format!("JOINED {}", view.ssid.to_uppercase()), false)),
        Phase::Failed(reason) => Some((reason.clone(), true)),
    }
}

/// Measured truncation with an ellipsis: a string too wide for its box
/// loses characters, never its box.
fn fit(fonts: &mut FontSystem, spec: &FontSpec, text: &str, max_w: u32) -> String {
    if paint::text_width(fonts, spec, text) <= max_w {
        return text.to_string();
    }
    let mut out = text.to_string();
    while !out.is_empty() && paint::text_width(fonts, spec, &format!("{out}…")) > max_w {
        out.pop();
    }
    format!("{out}…")
}

/// Draws the whole dialog. Byte-stable for a given view, theme and
/// scale — which is what the design tests assert.
pub fn render_join_dialog(theme: &Theme, fonts: &mut FontSystem, swash: &mut SwashCache, scale: f32, view: &DialogView) -> Pixmap {
    let l = layout(scale);
    let mut pixmap = Pixmap::new(l.width.max(1), l.height.max(1)).expect("nonzero dialog size");
    let su = |v: u32| ((v as f32) * scale).round().max(1.0) as u32;

    // The surface: the menu's own fill and chisel, so the dialog's
    // interior matches the desktop's other panels exactly.
    paint::fill_area(&mut pixmap, 0, 0, l.width, l.height, &theme.menu.background);
    paint::draw_bevel(&mut pixmap, 0, 0, l.width, l.height, &theme.menu.bevel);

    let title_font = theme.menu.title_font.clone();
    let body = theme.menu.item_font.clone();
    let ink = theme.menu.text_color;
    let dim = mix(ink, fill_tone(&theme.menu.background), 0.58);

    // The network being joined, which is the dialog's whole subject.
    let title = fit(fonts, &title_font, &view.ssid.to_uppercase(), l.title.w);
    paint::draw_text(&mut pixmap, fonts, swash, &title, &title_font, ink, l.title.x, l.title.y, l.title.w, l.title.h, TextAlign::Left);
    paint::draw_text(&mut pixmap, fonts, swash, "PASSPHRASE", &body, dim, l.caption.x, l.caption.y, l.caption.w, l.caption.h, TextAlign::Left);

    draw_field(&mut pixmap, fonts, swash, theme, &l, view, &body, ink, scale);
    draw_reveal(&mut pixmap, fonts, swash, theme, &l, view, &body, ink, dim);

    if let Some((text, bad)) = status_line(view) {
        let color = if bad { theme.menu.highlight_text_color } else { dim };
        if bad {
            // A complaint gets the highlight bar behind it, which is
            // the one treatment this theme vocabulary has for "read
            // this" — and the reason the text switches to the
            // highlight's own ink.
            paint::fill_area(&mut pixmap, l.status.x, l.status.y, l.status.w, su(16), &theme.menu.highlight_background);
        }
        let text = fit(fonts, &body, &text, l.status.w.saturating_sub(su(6)));
        let x = l.status.x + if bad { su(3) as i32 } else { 0 };
        paint::draw_text(&mut pixmap, fonts, swash, &text, &body, color, x, l.status.y, l.status.w, su(16), TextAlign::Left);
    }

    let joinable = view.can_join || view.phase == Phase::Joined;
    draw_button(&mut pixmap, fonts, swash, theme, l.cancel, "CANCEL", &body, true, view.pressed == Some(Target::Cancel), view.focus == Focus::Cancel, ink, dim);
    draw_button(
        &mut pixmap,
        fonts,
        swash,
        theme,
        l.join,
        join_label(&view.phase),
        &body,
        joinable,
        view.pressed == Some(Target::Join),
        view.focus == Focus::Join,
        ink,
        dim,
    );
    pixmap
}

#[allow(clippy::too_many_arguments)]
fn draw_field(
    pixmap: &mut Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    theme: &Theme,
    l: &Layout,
    view: &DialogView,
    body: &FontSpec,
    ink: Color,
    scale: f32,
) {
    let t = theme.menu.bevel.width.max(1) as u32;
    let f = l.field;
    // A sunken well is what "you may type here" looks like in this
    // vocabulary; the interior is the menu's highlight-free flat tone
    // so the text sits on something certainly opaque (draw_text
    // requires it).
    paint::fill_rect(pixmap, f.x, f.y, f.w, f.h, field_tone(theme));
    paint::draw_sunken_bevel(pixmap, f.x, f.y, f.w, f.h, t);

    let pad = ((4.0 * scale).round().max(2.0)) as i32;
    let inner_x = f.x + t as i32 + pad;
    let inner_w = (f.w as i32 - (t as i32 + pad) * 2).max(0) as u32;
    // The masked string is measured and drawn; the passphrase itself
    // never reaches this function — see `DialogView::shown`.
    let shown = fit(fonts, body, &view.shown, inner_w);
    paint::draw_text(pixmap, fonts, swash, &shown, body, ink, inner_x, f.y, inner_w, f.h, TextAlign::Left);

    if view.caret.is_some() {
        let text_w = paint::text_width(fonts, body, &shown).min(inner_w.saturating_sub(1));
        let caret_h = f.h.saturating_sub((t + 1) * 2).max(1);
        let caret_w = ((scale).round().max(1.0)) as u32;
        paint::fill_rect(pixmap, inner_x + text_w as i32, f.y + (t + 1) as i32, caret_w, caret_h, ink);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_reveal(
    pixmap: &mut Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    theme: &Theme,
    l: &Layout,
    view: &DialogView,
    body: &FontSpec,
    ink: Color,
    dim: Color,
) {
    let t = theme.menu.bevel.width.max(1) as u32;
    let b = l.reveal_box;
    paint::fill_rect(pixmap, b.x, b.y, b.w, b.h, field_tone(theme));
    paint::draw_sunken_bevel(pixmap, b.x, b.y, b.w, b.h, t);
    if view.revealed {
        let inset = (t + 1) as i32;
        let side = b.w.saturating_sub(((t + 1) * 2).min(b.w)).max(1);
        paint::fill_rect(pixmap, b.x + inset, b.y + inset, side, side, ink);
    }
    let label_x = b.x + b.w as i32 + (b.w as i32 / 2);
    let label_w = (l.reveal_row.x + l.reveal_row.w as i32 - label_x).max(0) as u32;
    let color = if view.focus == Focus::Reveal { ink } else { dim };
    let label = fit(fonts, body, "SHOW PASSPHRASE  (CTRL-R)", label_w);
    paint::draw_text(pixmap, fonts, swash, &label, body, color, label_x, l.reveal_row.y, label_w, l.reveal_row.h, TextAlign::Left);
    if view.focus == Focus::Reveal {
        focus_ring(pixmap, l.reveal_row, ink);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_button(
    pixmap: &mut Pixmap,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    theme: &Theme,
    r: Rect,
    label: &str,
    body: &FontSpec,
    enabled: bool,
    pressed: bool,
    focused: bool,
    ink: Color,
    dim: Color,
) {
    paint::draw_button(pixmap, r.x, r.y, r.w, r.h, &theme.menu.background, &theme.menu.bevel, pressed && enabled);
    // The classic press: the label sinks with the face.
    let dy = if pressed && enabled { 1 } else { 0 };
    let color = if enabled { ink } else { dim };
    let label = fit(fonts, body, label, r.w);
    paint::draw_text(pixmap, fonts, swash, &label, body, color, r.x, r.y + dy, r.w, r.h, TextAlign::Center);
    if focused {
        focus_ring(pixmap, r, ink);
    }
}

/// The keyboard-focus indicator: a one-pixel dotted rectangle just
/// inside the control. Dotted rather than solid so it reads as focus
/// and never as a border the control grew.
fn focus_ring(pixmap: &mut Pixmap, r: Rect, color: Color) {
    if r.w < 4 || r.h < 4 {
        return;
    }
    let (x0, y0) = (r.x + 1, r.y + 1);
    let (x1, y1) = (r.x + r.w as i32 - 2, r.y + r.h as i32 - 2);
    for x in (x0..=x1).step_by(2) {
        paint::fill_rect(pixmap, x, y0, 1, 1, color);
        paint::fill_rect(pixmap, x, y1, 1, 1, color);
    }
    for y in (y0..=y1).step_by(2) {
        paint::fill_rect(pixmap, x0, y, 1, 1, color);
        paint::fill_rect(pixmap, x1, y, 1, 1, color);
    }
}

/// A flat representative color for a `Fill`, so tones can be derived
/// from it. A gradient's midpoint is what the eye reads as "the
/// surface color" over a control this small, and averaging beats
/// picking an end that a Diagonal direction does not really have.
fn fill_tone(fill: &chonk_ui::model::Fill) -> Color {
    match fill {
        chonk_ui::model::Fill::Solid(c) => *c,
        chonk_ui::model::Fill::Gradient(g) => mix(g.from, g.to, 0.5),
    }
}

/// The inside of a sunken well.
///
/// A text field has to read as "paper", and the only way to say that
/// across eight palettes without naming a color is to move the surface
/// *away* from its own ink: a dark-inked theme gets a well pushed
/// toward white, a light-inked one gets a well pushed toward black.
/// Mixing the surface toward the ink — the obvious first spelling —
/// produces a well that differs from the panel by a few percent and
/// vanishes, which is what the first design pass showed.
fn field_tone(theme: &Theme) -> Color {
    let surface = fill_tone(&theme.menu.background);
    let ink = theme.menu.text_color;
    let paper = if luminance(ink) < 128.0 { Color::rgb(0xFF, 0xFF, 0xFF) } else { Color::rgb(0, 0, 0) };
    mix(paper, surface, 0.72)
}

/// Perceived brightness, the usual weighting. Only ever compared
/// against a midpoint, so the exact coefficients matter less than
/// their being the ones everything else in this tree uses.
fn luminance(c: Color) -> f32 {
    0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32
}

/// `t` of `a`, the rest of `b`.
fn mix(a: Color, b: Color, t: f32) -> Color {
    let f = |x: u8, y: u8| ((x as f32) * t + (y as f32) * (1.0 - t)).round().clamp(0.0, 255.0) as u8;
    Color::rgb(f(a.r, b.r), f(a.g, b.g), f(a.b, b.b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::{JoinDialog, Phase};
    use chonk_ui::nextstep_theme;

    fn ctx() -> (FontSystem, SwashCache) {
        (FontSystem::new(), SwashCache::new())
    }

    fn view_with(f: impl FnOnce(&mut JoinDialog)) -> DialogView {
        let mut d = JoinDialog::new("Cafe Wifi");
        f(&mut d);
        d.view()
    }

    #[test]
    fn every_control_is_hit_testable_and_the_boxes_are_disjoint() {
        for scale in [1.0f32, 1.5, 2.0] {
            let l = layout(scale);
            for target in [Target::Field, Target::Reveal, Target::Join, Target::Cancel] {
                let (x, y) = l.center(target).expect("placed");
                assert_eq!(l.hit(x, y), Some(target), "scale {scale}: {target:?} must answer at its own center");
            }
            // Every box inside the frame, and the frame itself inert.
            for r in [l.field, l.reveal_row, l.join, l.cancel, l.title, l.status] {
                assert!(r.x >= 0 && r.y >= 0, "scale {scale}: {r:?} starts inside the frame");
                assert!(r.x + r.w as i32 <= l.width as i32, "scale {scale}: {r:?} fits the width");
                assert!(r.y + r.h as i32 <= l.height as i32, "scale {scale}: {r:?} fits the height");
            }
            assert_eq!(l.hit(0, 0), None, "the frame is not a control");
            assert_eq!(l.hit(l.width as i32 - 1, 1), None);
            assert!(l.join.x >= l.cancel.x + l.cancel.w as i32, "scale {scale}: the buttons must not overlap");
        }
    }

    #[test]
    fn a_nonsense_scale_falls_back_rather_than_producing_a_zero_window() {
        for scale in [0.0f32, -3.0, f32::NAN, f32::INFINITY] {
            let l = layout(scale);
            assert_eq!((l.width, l.height), (WIDTH, HEIGHT), "scale {scale} must fall back to 1x");
        }
    }

    #[test]
    fn rendering_is_byte_stable_and_exactly_the_laid_out_size() {
        let theme = nextstep_theme();
        let (mut fs, mut sc) = ctx();
        let view = view_with(|d| {
            for c in "hunter2".chars() {
                d.on_key(crate::keys::Key::Char(c));
            }
        });
        let a = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &view);
        let b = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &view);
        assert_eq!(a.data(), b.data(), "the same view must always produce the same bytes");
        let l = layout(1.0);
        assert_eq!((a.width(), a.height()), (l.width, l.height));
    }

    #[test]
    fn the_designed_states_are_pairwise_distinct() {
        let theme = nextstep_theme();
        let (mut fs, mut sc) = ctx();
        let typed = |d: &mut JoinDialog| {
            for c in "hunter2".chars() {
                d.on_key(crate::keys::Key::Char(c));
            }
        };
        let views = [
            view_with(|_| {}),
            view_with(typed),
            view_with(|d| {
                typed(d);
                d.on_key(crate::keys::Key::ToggleReveal);
            }),
            view_with(|d| {
                typed(d);
                d.on_key(crate::keys::Key::Tab { back: false });
            }),
            view_with(|d| {
                typed(d);
                d.finished(false, "Secrets were required.");
            }),
            view_with(|d| {
                typed(d);
                d.finished(true, "");
            }),
        ];
        let faces: Vec<Vec<u8>> = views.iter().map(|v| render_join_dialog(&theme, &mut fs, &mut sc, 1.0, v).data().to_vec()).collect();
        for a in 0..faces.len() {
            for b in (a + 1)..faces.len() {
                assert_ne!(faces[a], faces[b], "views {a} and {b} rendered identically");
            }
        }
    }

    #[test]
    fn a_masked_field_never_draws_the_passphrase() {
        // The strongest form this can take without OCR: render the
        // same dialog with the same-length passphrase twice, differing
        // only in content. A masked field must be byte-identical.
        let theme = nextstep_theme();
        let (mut fs, mut sc) = ctx();
        let of = |text: &str, reveal: bool| {
            view_with(|d| {
                for c in text.chars() {
                    d.on_key(crate::keys::Key::Char(c));
                }
                if reveal {
                    d.on_key(crate::keys::Key::ToggleReveal);
                }
            })
        };
        let a = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &of("hunter2", false));
        let b = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &of("swordfi", false));
        assert_eq!(a.data(), b.data(), "two different 7-character passphrases must mask identically");
        let revealed = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &of("hunter2", true));
        assert_ne!(a.data(), revealed.data(), "and revealing must actually show something else");
    }

    #[test]
    fn the_action_button_says_what_it_currently_does() {
        assert_eq!(join_label(&Phase::Editing), "JOIN");
        assert_eq!(join_label(&Phase::Failed("x".into())), "JOIN", "after a failure it is a retry, still a join");
        assert_eq!(join_label(&Phase::Joining), "JOINING…");
        assert_eq!(join_label(&Phase::Joined), "CLOSE", "when there is nothing left to join it stops offering to");
    }

    #[test]
    fn an_idle_dialog_reports_nothing_and_a_failed_one_complains() {
        assert_eq!(status_line(&view_with(|_| {})), None, "an empty status row beats reassuring filler");
        let failed = view_with(|d| d.finished(false, "Secrets were required."));
        let (text, bad) = status_line(&failed).expect("a failure is reported");
        assert!(bad, "a failure must be marked as one");
        assert_eq!(text, "SECRETS WERE REQUIRED.");
        let joined = view_with(|d| d.finished(true, ""));
        let (text, bad) = status_line(&joined).expect("a success is reported too");
        assert!(!bad);
        assert!(text.contains("CAFE WIFI"), "and it names the network: {text}");
    }

    #[test]
    fn a_long_ssid_loses_characters_rather_than_its_box() {
        let theme = nextstep_theme();
        let (mut fs, mut sc) = ctx();
        let mut d = JoinDialog::new("A Very Long Network Name That Nobody Should Have Chosen But Someone Did");
        d.on_key(crate::keys::Key::Char('x'));
        let pixmap = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &d.view());
        let l = layout(1.0);
        assert_eq!((pixmap.width(), pixmap.height()), (l.width, l.height), "an overlong title must not resize the dialog");
        let title = fit(&mut fs, &theme.menu.title_font, &d.view().ssid.to_uppercase(), l.title.w);
        assert!(title.ends_with('…'), "it is elided: {title}");
        assert!(paint::text_width(&mut fs, &theme.menu.title_font, &title) <= l.title.w);
    }

    #[test]
    fn it_renders_in_every_theme_and_both_appearances() {
        let (mut fs, mut sc) = ctx();
        let view = view_with(|d| {
            d.on_key(crate::keys::Key::Char('x'));
        });
        for theme in wm_theme::default_theme::all_themes() {
            let pixmap = render_join_dialog(&theme, &mut fs, &mut sc, 1.0, &view);
            let opaque = pixmap.data().as_chunks::<4>().0.iter().filter(|p| p[3] == 0xFF).count();
            assert_eq!(opaque, (pixmap.width() * pixmap.height()) as usize, "theme {}: a dialog is an opaque window", theme.id);
        }
    }
}
