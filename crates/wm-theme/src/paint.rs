//! Reusable low-level `tiny-skia` drawing primitives: flat/gradient
//! fills, the NeXTSTEP chisel bevel, glyph-mask text blending, and
//! themed button rendering with visual press feedback. Used internally
//! by `raster.rs` (window decorations) and `menu.rs` (popup menus), and
//! public so a third-party `chonk-ui` app can draw its own widgets in
//! the exact same visual language instead of re-deriving it.

use tiny_skia::{
    Color as SkColor, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap,
    Point as SkPoint, PremultipliedColorU8, Rect as SkRect, SpreadMode, Transform,
};

use crate::model::{Bevel, BevelStyle, Color, Fill, FontSpec, FontStyle, FontWeight, GradientDirection, TextAlign};

pub fn sk_color(c: Color) -> SkColor {
    SkColor::from_rgba8(c.r, c.g, c.b, c.a)
}

/// Flat-fills a rect with a single opaque color. Non-anti-aliased on
/// purpose — the whole NeXTSTEP look depends on hard pixel edges.
pub fn fill_rect(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, color: Color) {
    if w == 0 || h == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color(sk_color(color));
    paint.anti_alias = false;
    if let Some(rect) = SkRect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

/// Fills a rect with a `Fill` — solid color or a linear gradient in one
/// of WindowMaker's three directions.
pub fn fill_area(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, fill: &Fill) {
    if w == 0 || h == 0 {
        return;
    }
    let Some(rect) = SkRect::from_xywh(x as f32, y as f32, w as f32, h as f32) else {
        return;
    };
    let mut paint = Paint { anti_alias: false, ..Default::default() };
    match fill {
        Fill::Solid(c) => paint.set_color(sk_color(*c)),
        Fill::Gradient(g) => {
            let (start, end) = match g.direction {
                GradientDirection::Vertical => (
                    SkPoint::from_xy(x as f32, y as f32),
                    SkPoint::from_xy(x as f32, (y + h as i32) as f32),
                ),
                GradientDirection::Horizontal => (
                    SkPoint::from_xy(x as f32, y as f32),
                    SkPoint::from_xy((x + w as i32) as f32, y as f32),
                ),
                GradientDirection::Diagonal => (
                    SkPoint::from_xy(x as f32, y as f32),
                    SkPoint::from_xy((x + w as i32) as f32, (y + h as i32) as f32),
                ),
            };
            let stops = vec![
                GradientStop::new(0.0, sk_color(g.from)),
                GradientStop::new(1.0, sk_color(g.to)),
            ];
            match LinearGradient::new(start, end, stops, SpreadMode::Pad, Transform::identity()) {
                Some(shader) => paint.shader = shader,
                None => paint.set_color(sk_color(g.from)),
            }
        }
    }
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

/// The classic NeXTSTEP chiseled border: hard 1-2px edges. Confirmed
/// against real WindowMaker's own `wDrawBevel` (`src/texture.c`, the
/// `TS_NEXT`/"next" style branch, the one that actually reproduces
/// NeXTSTEP rather than WindowMaker's own separate default look):
/// NeXTSTEP's implied light source reads as coming from the
/// *bottom-right*, not the top-left the way most other 90s toolkits
/// (Windows, Motif, and WindowMaker's own non-NeXT styles) do it — a
/// `Raised` bevel is dark on the top/left sides and light on the
/// bottom/right, swapped for `Sunken`. Getting this backwards (light
/// top-left, like this used to before being confirmed against the real
/// source) is subtle on any one edge but compounds across every button,
/// titlebar, and menu item in the app into a systemically "off" look.
/// `width > 1` nests the same pass shrinking the rect each iteration.
pub fn draw_bevel(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, bevel: &Bevel) {
    let (top_left, bottom_right) = match bevel.style {
        BevelStyle::Raised => (bevel.dark, bevel.light),
        BevelStyle::Sunken => (bevel.light, bevel.dark),
        BevelStyle::Flat => return,
    };
    for i in 0..bevel.width.max(1) as i32 {
        let rx = x + i;
        let ry = y + i;
        let rw = w as i32 - 2 * i;
        let rh = h as i32 - 2 * i;
        if rw <= 0 || rh <= 0 {
            break;
        }
        fill_rect(pixmap, rx, ry, rw as u32, 1, top_left);
        fill_rect(pixmap, rx, ry, 1, rh as u32, top_left);
        fill_rect(pixmap, rx, ry + rh - 1, rw as u32, 1, bottom_right);
        fill_rect(pixmap, rx + rw - 1, ry, 1, rh as u32, bottom_right);
    }
}

/// Clamped per-pixel brighten/shade over an opaque rect — the exact
/// primitive real WindowMaker's relief drawing is built from
/// (`ROperateLine` with `RAddOperation`/`RSubtractOperation` in
/// `wrlib/`): it operates on *whatever fill is already there* rather
/// than painting an absolute color, which is what lets one relief
/// recipe read correctly on both a black and a light-gray titlebar.
/// Assumes the destination is already fully opaque, same as
/// `draw_text` (see its doc comment).
pub fn op_rect(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, delta: i16) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w as i32).min(pw);
    let y1 = (y + h as i32).min(ph);
    let pixels = pixmap.pixels_mut();
    for py in y0..y1 {
        for px in x0..x1 {
            let idx = (py * pw + px) as usize;
            let e = pixels[idx];
            let op = |c: u8| (c as i16 + delta).clamp(0, 255) as u8;
            if let Some(p) = PremultipliedColorU8::from_rgba(op(e.red()), op(e.green()), op(e.blue()), 255) {
                pixels[idx] = p;
            }
        }
    }
}

/// wrlib's `RBEV_RAISED2` relief (`RBevelImage`, `wrlib/misc.c`) — the
/// one real WindowMaker applies to titlebars, titlebar buttons, and
/// (partially) resizebars: +80 light lines along top/left, a -40 shade
/// line plus a hard black outer line along bottom/right. Generalized
/// from the original's hard-coded 1px lines to `t`-thick ones so the
/// relief scales with `CHONKSTEP_SCALE` like every other piece of
/// chrome.
pub fn draw_raised2_bevel(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, t: u32) {
    let t = t.max(1);
    if w < 3 * t || h < 3 * t {
        return;
    }
    let (wi, hi, ti) = (w as i32, h as i32, t as i32);
    op_rect(pixmap, x, y, w, t, 80);
    op_rect(pixmap, x, y + ti, t, h - t, 80);
    op_rect(pixmap, x, y + hi - 2 * ti, w - 2 * t, t, -40);
    fill_rect(pixmap, x, y + hi - ti, w, t, Color::rgb(0, 0, 0));
    op_rect(pixmap, x + wi - 2 * ti, y, t, h - 2 * t, -40);
    fill_rect(pixmap, x + wi - ti, y, t, h - t, Color::rgb(0, 0, 0));
}

/// The stock resizebar relief, line for line from real WindowMaker's
/// `renderResizebarTexture` (`src/framewin.c`, `SHADOW_RESIZEBAR`
/// undefined, as shipped): a shade+light line pair across the top, and
/// a vertical shade+light notch pair `corner_w` in from each end
/// delimiting the corner grips. No outer side/bottom shading — the
/// bar's silhouette comes from the frame border below it.
#[allow(clippy::too_many_arguments)]
pub fn draw_resizebar_relief(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, corner_w: u32, t: u32) {
    let t = t.max(1);
    let (ti, cwi) = (t as i32, corner_w as i32);
    let notch_h = h.saturating_sub(2 * t);
    op_rect(pixmap, x, y, w, t, -40);
    op_rect(pixmap, x, y + ti, w, t, 80);
    op_rect(pixmap, x + cwi, y + 2 * ti, t, notch_h, -40);
    op_rect(pixmap, x + cwi + ti, y + 2 * ti, t, notch_h, 80);
    op_rect(pixmap, x + w as i32 - cwi - 2 * ti, y + 2 * ti, t, notch_h, -40);
    op_rect(pixmap, x + w as i32 - cwi - ti, y + 2 * ti, t, notch_h, 80);
}

/// wrlib's sunken relief (`RBevelImage`'s `else` branch,
/// `wrlib/misc.c`): -40 shade lines along top/left, +80 light lines
/// along bottom/right — the exact mirror of `draw_raised2_bevel`'s
/// light direction, minus that recipe's hard black outer lines (the
/// sunken branch has none). Thickness generalized to `t` the same way.
pub fn draw_sunken_bevel(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, t: u32) {
    let t = t.max(1);
    if w < 3 * t || h < 3 * t {
        return;
    }
    let (wi, hi, ti) = (w as i32, h as i32, t as i32);
    op_rect(pixmap, x, y, w, t, -40);
    op_rect(pixmap, x, y + ti, t, h - t, -40);
    op_rect(pixmap, x, y + hi - ti, w, t, 80);
    op_rect(pixmap, x + wi - ti, y, t, h - 2 * t, 80);
}

/// Pressed-button feedback: shift the button's *existing* fill toward
/// "pushed" — `delta` positive to lighten (a dark bar), negative to
/// darken (a light one) — and *invert* the relief to wrlib's sunken
/// direction rather than removing it: the edge accents stay put and
/// flip, reading as the surface tilting inward (relief simply
/// vanishing on press read as the button losing its edge — confirmed
/// live). Real WindowMaker's own default pressed state is a full white
/// fill with a black outline (`paintButton`'s `pushed`/`TS_NEW` arm),
/// which at native 23px reads as a blink but at CHONKSTEP_SCALE button
/// sizes reads as a glaring white flash — also confirmed live. `t` is
/// the relief thickness (the theme bevel width, so it scales).
pub fn draw_button_pressed(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, delta: i16, t: u32) {
    op_rect(pixmap, x, y, w, h, delta);
    draw_sunken_bevel(pixmap, x, y, w, h, t);
}

/// Draws a themed, pressable surface: `fill` then the chiseled bevel
/// when idle; a flat black fill with no bevel at all when `pressed` —
/// see [`draw_button_pressed`]'s doc comment for why a bevel (even a
/// mirrored one) is the wrong tool for "pushed in" feedback here. This
/// is the one rule every pressable surface in this theme follows —
/// titlebar buttons, menu close boxes, and any `chonk-ui` app widget
/// that wants the same low-level primitive instead of re-deriving press
/// feedback itself.
#[allow(clippy::too_many_arguments)]
pub fn draw_button(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32, fill: &Fill, bevel: &Bevel, pressed: bool) {
    fill_area(pixmap, x, y, w, h, fill);
    if pressed {
        draw_button_pressed(pixmap, x, y, w, h, pressed_delta(fill), bevel.width.max(1) as u32);
        return;
    }
    draw_bevel(pixmap, x, y, w, h, bevel);
}

/// The luminance shift `draw_button_pressed` should apply for a given
/// fill: lighten a dark fill, darken a light one, either way by the
/// same magnitude so the press feedback is equally visible on any bar.
pub fn pressed_delta(fill: &Fill) -> i16 {
    let c = match fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => Color::rgb(
            ((g.from.r as u16 + g.to.r as u16) / 2) as u8,
            ((g.from.g as u16 + g.to.g as u16) / 2) as u8,
            ((g.from.b as u16 + g.to.b as u16) / 2) as u8,
        ),
    };
    let luminance = (c.r as u16 + c.g as u16 + c.b as u16) / 3;
    if luminance < 128 { 56 } else { -56 }
}

/// A small filled right-pointing triangle — the cascade indicator every
/// classic NeXTSTEP/WindowMaker-style menu draws beside an item that
/// opens a nested submenu (`menu.c`'s "draw the cascade indicator" in
/// real WindowMaker). `(x, y)` is the top-left of the triangle's
/// bounding box, `size` its height; width is `size * 0.8`.
pub fn draw_cascade_arrow(pixmap: &mut Pixmap, x: i32, y: i32, size: u32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(sk_color(color));
    paint.anti_alias = true;

    let (fx, fy, fs) = (x as f32, y as f32, size as f32);
    let mut pb = PathBuilder::new();
    pb.move_to(fx, fy);
    pb.line_to(fx, fy + fs);
    pb.line_to(fx + fs * 0.8, fy + fs / 2.0);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Measures the shaped width of `text` in `font` — the real layout
/// width cosmic-text will produce, fallback glyphs included, not an
/// average-advance estimate. This is what lets menus (and any
/// `chonk-ui` popup) size themselves to their content the way real
/// WindowMaker's `wMenuRealize` does with `WMWidthOfString`.
pub fn text_width(font_system: &mut cosmic_text::FontSystem, font: &FontSpec, text: &str) -> u32 {
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Style, Weight};

    if text.is_empty() {
        return 0;
    }
    let metrics = Metrics::new(font.size, font.size * 1.25);
    let mut buffer = Buffer::new(font_system, metrics);
    // No wrap box: measurement wants the single-line natural width.
    buffer.set_size(font_system, None, None);
    let weight = match font.weight {
        FontWeight::Bold => Weight::BOLD,
        FontWeight::Normal => Weight::NORMAL,
    };
    let style = match font.style {
        FontStyle::Italic => Style::Italic,
        FontStyle::Normal => Style::Normal,
    };
    let attrs = Attrs::new().family(Family::Name(&font.family)).weight(weight).style(style);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);
    let mut width = 0f32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
    }
    width.ceil() as u32
}

/// Renders `text` with `font`, blending glyph coverage onto `pixmap`
/// within the `(x, y, w, h)` box per `align`. Assumes the destination
/// pixels in that box are already fully opaque (alpha 255) — true for
/// every caller here, since text is always drawn over an
/// already-filled titlebar/menu/item background — which lets this treat
/// tiny-skia's premultiplied storage as if it were straight RGBA (at
/// alpha 255 the two are identical) instead of unpremultiplying on read.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    pixmap: &mut Pixmap,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    text: &str,
    font: &FontSpec,
    color: Color,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    align: TextAlign,
) {
    use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Style, Weight};

    if text.is_empty() || w == 0 || h == 0 {
        return;
    }

    let metrics = Metrics::new(font.size, font.size * 1.25);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, Some(w as f32), Some(h as f32));

    let weight = match font.weight {
        FontWeight::Bold => Weight::BOLD,
        FontWeight::Normal => Weight::NORMAL,
    };
    let style = match font.style {
        FontStyle::Italic => Style::Italic,
        FontStyle::Normal => Style::Normal,
    };
    let attrs = Attrs::new().family(Family::Name(&font.family)).weight(weight).style(style);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    let mut text_width = 0f32;
    for run in buffer.layout_runs() {
        text_width = text_width.max(run.line_w);
    }
    let offset_x = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => ((w as f32 - text_width) / 2.0).max(0.0),
        TextAlign::Right => (w as f32 - text_width).max(0.0),
    };

    // First pass: measure the *actual* rendered ink extent (every
    // glyph's real rasterized top/bottom, at a y=0 reference) rather
    // than assuming it from `font.size` alone. `font.size` is a decent
    // stand-in for plain text in the configured font — see the
    // regression test below for that case — but breaks down the moment
    // a glyph the configured font doesn't cover gets shaped with a
    // *fallback* font instead (a title bar showing a status-indicator
    // icon character in front of otherwise plain text, say): that
    // glyph's natural vertical proportions can differ substantially
    // from the main font's, and since centering was computed for the
    // whole line as one uniform block, the outlier glyph silently
    // dragged the *entire* line's apparent center off with it —
    // visibly more empty space above the text than below.
    let mut min_ink_y: Option<i32> = None;
    let mut max_ink_y: Option<i32> = None;
    for run in buffer.layout_runs() {
        for glyph in run.glyphs.iter() {
            let physical = glyph.physical((x as f32 + offset_x, 0.0), 1.0);
            let Some(image) = swash_cache.get_image(font_system, physical.cache_key) else { continue };
            if image.placement.width == 0 || image.placement.height == 0 {
                continue;
            }
            let top = physical.y - image.placement.top + run.line_y as i32;
            let bottom = top + image.placement.height as i32;
            min_ink_y = Some(min_ink_y.map_or(top, |m: i32| m.min(top)));
            max_ink_y = Some(max_ink_y.map_or(bottom, |m: i32| m.max(bottom)));
        }
    }
    let offset_y = match (min_ink_y, max_ink_y) {
        // Shift so the measured ink block — currently sitting at
        // [top, bottom] — lands centered in `h` instead: the same
        // shift applies to every glyph, so relative alignment within
        // the line (baseline, etc.) is untouched, only the whole
        // line's vertical position moves.
        (Some(top), Some(bottom)) => ((h as i32 - (bottom - top)) / 2 - top).max(0),
        // No ink measured at all (e.g. an all-whitespace string) —
        // `font.size` is a reasonable enough stand-in when there's
        // nothing real to measure.
        _ => ((h as f32 - font.size) / 2.0).max(0.0) as i32,
    };

    for run in buffer.layout_runs() {
        for glyph in run.glyphs.iter() {
            let physical = glyph.physical((x as f32 + offset_x, y as f32 + offset_y as f32), 1.0);
            if let Some(image) = swash_cache.get_image(font_system, physical.cache_key) {
                let img_x = physical.x + image.placement.left;
                let img_y = physical.y - image.placement.top + run.line_y as i32;
                blend_glyph_image(pixmap, img_x, img_y, image, color);
            }
        }
    }
}

fn blend_glyph_image(pixmap: &mut Pixmap, x: i32, y: i32, image: &cosmic_text::SwashImage, color: Color) {
    let (pw, ph) = (image.placement.width, image.placement.height);
    match image.content {
        cosmic_text::SwashContent::Mask => {
            for row in 0..ph {
                for col in 0..pw {
                    let coverage = image.data[(row * pw + col) as usize];
                    if coverage == 0 {
                        continue;
                    }
                    let a = ((color.a as u32 * coverage as u32) / 255) as u8;
                    blend_pixel(pixmap, x + col as i32, y + row as i32, color.r, color.g, color.b, a);
                }
            }
        }
        cosmic_text::SwashContent::Color => {
            for row in 0..ph {
                for col in 0..pw {
                    let idx = ((row * pw + col) * 4) as usize;
                    let a = image.data[idx + 3];
                    if a == 0 {
                        continue;
                    }
                    blend_pixel(pixmap, x + col as i32, y + row as i32, image.data[idx], image.data[idx + 1], image.data[idx + 2], a);
                }
            }
        }
        cosmic_text::SwashContent::SubpixelMask => {}
    }
}

/// Alpha-blends one `(r,g,b,a)` source pixel onto `pixmap` at `(x, y)`.
/// See `draw_text`'s doc comment for why treating the destination as
/// straight (non-premultiplied) RGBA is valid here.
fn blend_pixel(pixmap: &mut Pixmap, x: i32, y: i32, r: u8, g: u8, b: u8, a: u8) {
    if a == 0 || x < 0 || y < 0 {
        return;
    }
    let (xu, yu) = (x as u32, y as u32);
    if xu >= pixmap.width() || yu >= pixmap.height() {
        return;
    }
    let idx = (yu * pixmap.width() + xu) as usize;
    let pixels = pixmap.pixels_mut();
    let existing = pixels[idx];
    let a = a as u32;
    let inv_a = 255 - a;
    let blend = |c: u32, ec: u32| -> u8 { ((c * a + ec * inv_a) / 255).min(255) as u8 };
    let nr = blend(r as u32, existing.red() as u32);
    let ng = blend(g as u32, existing.green() as u32);
    let nb = blend(b as u32, existing.blue() as u32);
    if let Some(px) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
        pixels[idx] = px;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FontStyle, FontWeight};

    /// Regression test for the light-source direction confirmed against
    /// real WindowMaker's `wDrawBevel` (`TS_NEXT` branch of
    /// `src/texture.c`): a `Raised` bevel's outer top-left corner must
    /// be the *dark* tone and its outer bottom-right corner the *light*
    /// one — backwards from most other 90s toolkits' top-left highlight
    /// convention, which this used to (incorrectly) follow.
    #[test]
    fn raised_bevel_is_dark_on_top_left_and_light_on_bottom_right() {
        let bevel = Bevel { style: BevelStyle::Raised, width: 1, light: Color::rgb(255, 255, 255), dark: Color::rgb(0, 0, 0) };
        let (w, h) = (20u32, 20u32);
        let mut pixmap = Pixmap::new(w, h).unwrap();
        fill_rect(&mut pixmap, 0, 0, w, h, Color::rgb(128, 128, 128));
        draw_bevel(&mut pixmap, 0, 0, w, h, &bevel);

        let top_left = pixmap.pixels()[0];
        let bottom_right = pixmap.pixels()[((h - 1) * w + (w - 1)) as usize];
        assert_eq!((top_left.red(), top_left.green(), top_left.blue()), (0, 0, 0), "top-left should be the dark tone");
        assert_eq!((bottom_right.red(), bottom_right.green(), bottom_right.blue()), (255, 255, 255), "bottom-right should be the light tone");

        // Sunken must be the exact reverse.
        let mut sunken_pixmap = Pixmap::new(w, h).unwrap();
        fill_rect(&mut sunken_pixmap, 0, 0, w, h, Color::rgb(128, 128, 128));
        draw_bevel(&mut sunken_pixmap, 0, 0, w, h, &Bevel { style: BevelStyle::Sunken, ..bevel });
        let sunken_top_left = sunken_pixmap.pixels()[0];
        assert_eq!((sunken_top_left.red(), sunken_top_left.green(), sunken_top_left.blue()), (255, 255, 255), "sunken should invert: light on top-left");
    }

    /// Pressed feedback is a relative shift (lighten a dark fill,
    /// darken a light one) plus the relief *inverted* to wrlib's
    /// sunken direction — dark top-left, light bottom-right — never
    /// removed: the edge accents flipping rather than vanishing is
    /// what reads as "pushed in" (see `draw_button_pressed`'s doc
    /// comment for why not WindowMaker's own white-flash pushed state).
    #[test]
    fn pressed_button_shifts_fill_and_inverts_relief_to_sunken() {
        let (w, h) = (20u32, 20u32);

        let dark_fill = Fill::Solid(Color::rgb(0, 0, 0));
        let mut dark = Pixmap::new(w, h).unwrap();
        fill_rect(&mut dark, 0, 0, w, h, Color::rgb(0, 0, 0));
        draw_button_pressed(&mut dark, 0, 0, w, h, pressed_delta(&dark_fill), 1);
        let top_left = dark.pixels()[0].red();
        let center = dark.pixels()[((h / 2) * w + w / 2) as usize].red();
        let bottom_right = dark.pixels()[((h - 1) * w + (w - 1)) as usize].red();
        assert!(center > 0, "a dark fill should lighten when pressed");
        assert!(top_left < center, "sunken: top-left shade must sit below the shifted interior");
        assert!(bottom_right > center, "sunken: bottom-right light must sit above the shifted interior");

        let light_fill = Fill::Solid(Color::rgb(0xAA, 0xAA, 0xAA));
        let mut light = Pixmap::new(w, h).unwrap();
        fill_rect(&mut light, 0, 0, w, h, Color::rgb(0xAA, 0xAA, 0xAA));
        draw_button_pressed(&mut light, 0, 0, w, h, pressed_delta(&light_fill), 1);
        assert!(light.pixels()[((h / 2) * w + w / 2) as usize].red() < 0xAA, "a light fill should darken when pressed");
    }

    /// `RBEV_RAISED2` pinned against `wrlib/misc.c`: on a mid-gray
    /// fill, the top edge gains +80, the outer bottom/right lines are
    /// hard black, and the inner bottom shade line loses 40.
    #[test]
    fn raised2_bevel_matches_wrlib_recipe() {
        let (w, h) = (20u32, 20u32);
        let mut pixmap = Pixmap::new(w, h).unwrap();
        fill_rect(&mut pixmap, 0, 0, w, h, Color::rgb(128, 128, 128));
        draw_raised2_bevel(&mut pixmap, 0, 0, w, h, 1);

        let px = |x: u32, y: u32| {
            let p = pixmap.pixels()[(y * w + x) as usize];
            (p.red(), p.green(), p.blue())
        };
        assert_eq!(px(w / 2, 0), (208, 208, 208), "top line brightens by 80");
        assert_eq!(px(0, h / 2), (208, 208, 208), "left line brightens by 80");
        assert_eq!(px(w / 2, h - 2), (88, 88, 88), "inner bottom line shades by 40");
        assert_eq!(px(w / 2, h - 1), (0, 0, 0), "outer bottom line is hard black");
        assert_eq!(px(w - 1, h / 2), (0, 0, 0), "outer right line is hard black");
        assert_eq!(px(w / 2, h / 2), (128, 128, 128), "interior untouched");
    }

    /// The topmost/bottommost row (inclusive) containing an ink pixel
    /// brighter than `threshold`, or `None` if nothing was drawn.
    fn ink_row_bounds(pixmap: &Pixmap, threshold: u8) -> Option<(u32, u32)> {
        let (w, h) = (pixmap.width(), pixmap.height());
        let mut min_y = None;
        let mut max_y = None;
        for y in 0..h {
            let row_has_ink = (0..w).any(|x| pixmap.pixels()[(y * w + x) as usize].red() > threshold);
            if row_has_ink {
                min_y.get_or_insert(y);
                max_y = Some(y);
            }
        }
        Some((min_y?, max_y?))
    }

    /// Regression test: `offset_y` used to center against `font.size *
    /// 1.25` (cosmic-text's line-height, meant for spacing *multiple*
    /// stacked lines evenly) instead of the font's actual single-line
    /// glyph extent. The extra phantom leading isn't split evenly
    /// around a line's real ink, so every title bar and menu item
    /// rendered with text sitting visibly high — noticeably more empty
    /// space below the glyphs than above. Checked across a spread of
    /// strings with different ascender/descender profiles, since a
    /// single string's own ink bias could mask a systematic offset bug.
    #[test]
    fn text_is_vertically_centered_within_its_container() {
        let h = 40u32;
        let w = 300u32;
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let font = FontSpec { family: "Nimbus Sans".to_string(), size: 12.0, weight: FontWeight::Bold, style: FontStyle::Normal };

        for text in ["chonkstep", "Hxo8", "gjpqy", "MW"] {
            let mut pixmap = Pixmap::new(w, h).unwrap();
            fill_rect(&mut pixmap, 0, 0, w, h, Color::rgb(10, 10, 12));
            draw_text(&mut pixmap, &mut font_system, &mut swash_cache, text, &font, Color::rgb(255, 255, 255), 0, 0, w, h, TextAlign::Center);

            let (min_y, max_y) = ink_row_bounds(&pixmap, 100).unwrap_or_else(|| panic!("{text:?} drew nothing"));
            let top_gap = min_y as i32;
            let bottom_gap = (h - 1 - max_y) as i32;
            assert!(
                (top_gap - bottom_gap).abs() <= 3,
                "{text:?} not vertically centered: top_gap={top_gap} bottom_gap={bottom_gap} (container h={h})"
            );
        }
    }

    /// Regression test: `offset_y` used to be computed once from
    /// `font.size` alone, assuming every glyph in the line shared the
    /// configured font's proportions. A titlebar showing a status
    /// indicator character in front of otherwise plain text (a running
    /// terminal updating its own title, say) breaks that assumption —
    /// the indicator isn't covered by the configured font, gets shaped
    /// with a *fallback* font instead, and that fallback glyph's real
    /// vertical extent can differ substantially from what `font.size`
    /// predicted. The whole line was centered as one block, so that one
    /// outlier glyph dragged the entire line's apparent center off with
    /// it — visibly more empty space above the text than below, while a
    /// plain-text title right next to it looked correctly centered.
    /// Reproduces the exact titles observed live: a running terminal's
    /// window title gaining a "◐"/"✳" busy-indicator prefix.
    #[test]
    fn text_stays_centered_when_a_fallback_font_glyph_is_mixed_in() {
        let h = 63u32; // a titlebar at CHONKSTEP_SCALE=3 (21px * 3)
        let w = 1200u32;
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        let font = FontSpec { family: "Nimbus Sans".to_string(), size: 36.0, weight: FontWeight::Bold, style: FontStyle::Normal };

        let plain = "chonkstep-rust-wm-milestone-1";
        let mut plain_pixmap = Pixmap::new(w, h).unwrap();
        fill_rect(&mut plain_pixmap, 0, 0, w, h, Color::rgb(10, 10, 12));
        draw_text(&mut plain_pixmap, &mut font_system, &mut swash_cache, plain, &font, Color::rgb(255, 255, 255), 0, 0, w, h, TextAlign::Center);
        let (plain_min, plain_max) = ink_row_bounds(&plain_pixmap, 100).unwrap();
        let baseline_center = (plain_min + plain_max) as f32 / 2.0;

        for text in ["\u{25D0} chonkstep-rust-wm-milestone-1", "\u{2733} Claude Code"] {
            let mut pixmap = Pixmap::new(w, h).unwrap();
            fill_rect(&mut pixmap, 0, 0, w, h, Color::rgb(10, 10, 12));
            draw_text(&mut pixmap, &mut font_system, &mut swash_cache, text, &font, Color::rgb(255, 255, 255), 0, 0, w, h, TextAlign::Center);

            let (min_y, max_y) = ink_row_bounds(&pixmap, 100).unwrap_or_else(|| panic!("{text:?} drew nothing"));
            let top_gap = min_y as i32;
            let bottom_gap = (h - 1 - max_y) as i32;
            assert!(
                (top_gap - bottom_gap).abs() <= 4,
                "{text:?} not vertically centered: top_gap={top_gap} bottom_gap={bottom_gap} (container h={h})"
            );
            // Also check against the plain-text baseline directly — the
            // titlebar shouldn't visibly jump up/down just because its
            // title picked up an icon prefix.
            let this_center = (min_y + max_y) as f32 / 2.0;
            assert!(
                (this_center - baseline_center).abs() <= 4.0,
                "{text:?} centered noticeably differently from plain text: this={this_center} plain={baseline_center}"
            );
        }
    }
}
