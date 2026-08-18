//! Reusable low-level `tiny-skia` drawing primitives: flat/gradient
//! fills, the NeXTSTEP chisel bevel, glyph-mask text blending, and
//! themed button rendering with visual press feedback. Used internally
//! by `raster.rs` (window decorations) and `menu.rs` (popup menus), and
//! public so a third-party `chonk-ui` app can draw its own widgets in
//! the exact same visual language instead of re-deriving it.

use tiny_skia::{
    Color as SkColor, FillRule, GradientStop, LineCap, LinearGradient, Paint, PathBuilder, Pixmap,
    Point as SkPoint, PremultipliedColorU8, Rect as SkRect, SpreadMode, Stroke, Transform,
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

/// Solid black fill, no bevel: real WindowMaker's own pressed-button
/// rendering (`paintButton`'s `pushed` branch, `TS_NEXT` arm, in
/// `src/framewin.c`) — not a mirrored/`Sunken` bevel. Mirroring made
/// sense before `draw_bevel` was confirmed against that same source:
/// back when `Raised` put *light* on the top-left, `Sunken` (its exact
/// opposite) read as "dark top-left" — sunken/pushed-in, correctly. But
/// authentic NeXTSTEP's `Raised` already puts *dark* on the top-left
/// (see `draw_bevel`'s own doc comment on the inverted light source),
/// so mirroring it into `Sunken` puts *light* there instead — which
/// most eyes read as "raised" regardless of what the code calls it. The
/// result was a button that visually looked like it popped further
/// *out* the instant you pressed it — confirmed live as exactly
/// backwards-feeling, "opposite day." WindowMaker itself sidesteps the
/// whole ambiguity by not bevelling the pressed state at all.
pub fn draw_button_pressed(pixmap: &mut Pixmap, x: i32, y: i32, w: u32, h: u32) {
    fill_rect(pixmap, x, y, w, h, Color::rgb(0, 0, 0));
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
    if pressed {
        draw_button_pressed(pixmap, x, y, w, h);
        return;
    }
    fill_area(pixmap, x, y, w, h, fill);
    draw_bevel(pixmap, x, y, w, h, bevel);
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

/// The resize-corner affordance: three short parallel diagonal grooves
/// (each a light+dark pair, one pixel apart, echoing this theme's own
/// chisel bevel language rather than inventing a new visual motif) in
/// the corner of the resize bar, angled toward the direction dragging
/// that corner actually resizes in. `(x, y)` is the corner of the
/// `size`x`size` box the grip is drawn within — for the bottom-right
/// (SouthEast) corner that's its top-left; `mirrored` flips the angle
/// for the bottom-left (SouthWest) corner, whose box is anchored at its
/// top-right instead.
#[allow(clippy::too_many_arguments)]
pub fn draw_resize_grip(pixmap: &mut Pixmap, x: i32, y: i32, size: u32, light: Color, dark: Color, mirrored: bool) {
    let s = size as f32;
    let stroke = Stroke { width: 1.0, line_cap: LineCap::Round, ..Default::default() };
    let mut light_paint = Paint::default();
    light_paint.set_color(sk_color(light));
    light_paint.anti_alias = false;
    let mut dark_paint = Paint::default();
    dark_paint.set_color(sk_color(dark));
    dark_paint.anti_alias = false;

    // Three evenly-spaced, increasingly long diagonal grooves fanning
    // out toward the true corner — reads as a textured, grippable
    // patch rather than a single arrow-like line.
    for i in 0..3 {
        let t = (i + 1) as f32 / 4.0;
        let len = s * (0.35 + 0.25 * i as f32);
        let (cx, cy) = (x as f32 + s * t, y as f32 + s * t);
        let (dx, dy) = (len / 2.0, len / 2.0);
        let (x0, y0, x1, y1) = if mirrored {
            (cx + dx, cy - dy, cx - dx, cy + dy)
        } else {
            (cx - dx, cy - dy, cx + dx, cy + dy)
        };
        for (paint, ox, oy) in [(&light_paint, 0.0, 0.0), (&dark_paint, 1.0, 1.0)] {
            let mut pb = PathBuilder::new();
            pb.move_to(x0 + ox, y0 + oy);
            pb.line_to(x1 + ox, y1 + oy);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, paint, &stroke, Transform::identity(), None);
            }
        }
    }
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

    /// Regression test for the "pops out instead of pushing in" bug: a
    /// pressed button must be a flat fill with *no* light/dark corner
    /// distinction at all (real WindowMaker's own pressed-button
    /// rendering — see `draw_button_pressed`'s doc comment), not a
    /// bevel of either direction. A mirrored `Sunken` bevel would put
    /// *light* on the top-left corner — indistinguishable from what a
    /// plain `Raised` bevel looked like before the direction fix — so
    /// asserting the two corners are equal (not just "different from
    /// idle") is what actually catches that regression.
    #[test]
    fn pressed_button_has_no_bevel_corner_distinction() {
        let (w, h) = (20u32, 20u32);
        let mut pixmap = Pixmap::new(w, h).unwrap();
        fill_rect(&mut pixmap, 0, 0, w, h, Color::rgb(128, 128, 128));
        draw_button_pressed(&mut pixmap, 0, 0, w, h);

        let top_left = pixmap.pixels()[0];
        let bottom_right = pixmap.pixels()[((h - 1) * w + (w - 1)) as usize];
        assert_eq!(
            (top_left.red(), top_left.green(), top_left.blue()),
            (bottom_right.red(), bottom_right.green(), bottom_right.blue()),
            "pressed state must not bevel — both corners should be the same flat dark fill"
        );
        assert_eq!((top_left.red(), top_left.green(), top_left.blue()), (0, 0, 0), "pressed state should be a flat black fill");
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
