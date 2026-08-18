//! The real `wm_theme_api::ThemeEngine` implementation: `RasterThemeEngine`
//! rasterizes window decorations with `tiny-skia` (fills, gradients,
//! bevels, button glyphs) and `cosmic-text` (title text, with font
//! fallback). Pure Rust, no X11/Wayland dependency — see the crate-level
//! doc comment in `lib.rs`.

use std::cell::RefCell;

use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationRequest, Point, Rect, ResizeEdge,
    Size, ThemeEngine,
};

use crate::model::Theme;
use crate::paint;

/// Implements `ThemeEngine` for a single `Theme`. Owns the font state
/// `cosmic-text` needs; wrapped in `RefCell` because `ThemeEngine`'s
/// methods take `&self` (a theme can be shared/boxed as
/// `Box<dyn ThemeEngine>`) while shaping/rasterizing glyphs needs `&mut`
/// access — safe because the whole window manager is single-threaded.
pub struct RasterThemeEngine {
    theme: Theme,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl RasterThemeEngine {
    pub fn new(theme: Theme) -> Self {
        let font_system = cosmic_text::FontSystem::new();
        let family = &theme.titlebar.font.family;
        let has_family = font_system
            .db()
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name == family));
        if !has_family {
            tracing::warn!(
                family = %family,
                "configured theme font not found on the system; text will render with whatever fallback sans font fontdb picks"
            );
        }
        Self {
            theme,
            font_system: RefCell::new(font_system),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }

    /// Convenience constructor wrapping the flagship built-in theme.
    pub fn nextstep_classic() -> Self {
        Self::new(crate::default_theme::nextstep_classic())
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }
}

impl ThemeEngine for RasterThemeEngine {
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout {
        layout_decoration(&self.theme, request)
    }

    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer {
        render_decoration(
            &self.theme,
            &mut self.font_system.borrow_mut(),
            &mut self.swash_cache.borrow_mut(),
            request,
            layout,
        )
    }
}

/// Pure arithmetic — no rasterization. Miniaturize sits at the
/// titlebar's top-left corner; Close (and Maximize, for a theme that
/// opts it back in) cluster at the top-right — real WindowMaker's own
/// button sides, confirmed by reading actual screenshots, not the
/// reverse this used to be. Each side's buttons claim their slot in the
/// order they appear in `theme.titlebar.buttons` — the first one
/// encountered on a given side lands outermost (closest to the corner),
/// later ones on that same side stack inward from there.
fn layout_decoration(theme: &Theme, request: &DecorationRequest) -> DecorationLayout {
    let titlebar_height = theme.titlebar.height as u32;
    let border = theme.border.width as u32;
    let resize_bar_height = if request.resizable { theme.resize_bar.height as u32 } else { 0 };

    let frame_size = Size::new(
        request.content_size.w + border * 2,
        request.content_size.h + titlebar_height + border * 2 + resize_bar_height,
    );

    // `theme.titlebar.button_margin` — real WindowMaker's own `TS_NEXT`
    // inset (see the theme's own doc comment on `buttons`), not a flush
    // `0`: NeXTSTEP's buttons sit inset from the titlebar's corner with
    // visible titlebar fill showing on every side, not stretched flush
    // to the edge the way WindowMaker's own newer `TS_NEW` style does it.
    let button_margin = theme.titlebar.button_margin as i32;
    let mut left_x = border as i32 + button_margin;
    let mut right_x = frame_size.w as i32 - border as i32;
    let mut button_hitboxes = Vec::with_capacity(theme.titlebar.buttons.len());
    for style in &theme.titlebar.buttons {
        let size = style.size as u32;
        let y = border as i32 + ((titlebar_height as i32 - size as i32) / 2).max(0);
        let rect = match style.kind {
            ButtonKind::Miniaturize => {
                let r = Rect::new(Point::new(left_x, y), Size::new(size, size));
                left_x += size as i32 + button_margin;
                r
            }
            ButtonKind::Close | ButtonKind::Maximize => {
                right_x -= size as i32 + button_margin;
                Rect::new(Point::new(right_x, y), Size::new(size, size))
            }
        };
        button_hitboxes.push((style.kind, rect));
    }

    let mut resize_hitboxes = Vec::new();
    if request.resizable {
        // Proportional to the titlebar's own (already-scaled) height,
        // not a flat 10px literal — the flat version never grew with
        // `CHONKSTEP_SCALE` while every other piece of chrome around it
        // did, so at higher scales the corner grip you could *see* was
        // several times bigger than the tiny hitbox you actually had to
        // land the cursor on to trigger it — confirmed live as "have to
        // be extremely precise with the mouse." `* 0.5` at
        // `titlebar_height: 20` reproduces the original 10px exactly at
        // scale 1, so unscaled behavior is unchanged.
        let handle = ((titlebar_height as f32 * 0.5) as u32).min(frame_size.w / 2).min(frame_size.h / 2).max(10);
        let bar_h = resize_bar_height.max(4).min(frame_size.h);
        resize_hitboxes.push((
            ResizeEdge::SouthEast,
            Rect::new(
                Point::new(frame_size.w as i32 - handle as i32, frame_size.h as i32 - handle as i32),
                Size::new(handle, handle),
            ),
        ));
        resize_hitboxes.push((
            ResizeEdge::SouthWest,
            Rect::new(Point::new(0, frame_size.h as i32 - handle as i32), Size::new(handle, handle)),
        ));
        let middle_w = frame_size.w.saturating_sub(handle * 2);
        if middle_w > 0 {
            resize_hitboxes.push((
                ResizeEdge::South,
                Rect::new(
                    Point::new(handle as i32, frame_size.h as i32 - bar_h as i32),
                    Size::new(middle_w, bar_h),
                ),
            ));
        }
    }

    DecorationLayout {
        frame_size,
        client_offset: Point::new(border as i32, (border + titlebar_height) as i32),
        titlebar_height,
        button_hitboxes,
        resize_hitboxes,
        shaded_frame_height: titlebar_height + border * 2,
    }
}

fn render_decoration(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    request: &DecorationRequest,
    layout: &DecorationLayout,
) -> DecorationBuffer {
    let (w, h) = (layout.frame_size.w.max(1), layout.frame_size.h.max(1));
    let mut pixmap = Pixmap::new(w, h).expect("decoration size is nonzero");
    let border = theme.border.width as u32;
    let inner_w = w.saturating_sub(border * 2);

    let titlebar_fill = if request.focused { &theme.titlebar.active } else { &theme.titlebar.inactive };
    paint::fill_area(&mut pixmap, border as i32, border as i32, inner_w, layout.titlebar_height, titlebar_fill);
    // A full 4-sided chiseled bevel around the whole bar — matching
    // real WindowMaker's own `wDrawBevel(fwin->titlebar->window, ...)`
    // call (`src/framewin.c`), not a lone top-edge highlight. That
    // used to be trimmed down to just the top edge to avoid a button's
    // own left bevel edge visually doubling up with the titlebar's —
    // but that doubling was only ugly because it was two *light* edges
    // stacking into a thick white smear. Now that `draw_bevel` puts
    // the authentic NeXTSTEP-direction *dark* tone on top/left (see
    // its doc comment), the titlebar's and each button's top edges
    // coincide on the same unobtrusive dark line instead — exactly
    // the redundant-but-harmless overlap real WindowMaker gets for
    // free from titlebar and buttons being separate, independently
    // beveled windows.
    paint::draw_bevel(&mut pixmap, border as i32, border as i32, inner_w, layout.titlebar_height, &theme.titlebar.bevel);

    if request.resizable {
        let bar_h = theme.resize_bar.height as u32;
        let bar_y = h.saturating_sub(border).saturating_sub(bar_h);
        paint::fill_area(&mut pixmap, border as i32, bar_y as i32, inner_w, bar_h, &theme.resize_bar.fill);
        paint::draw_bevel(&mut pixmap, border as i32, bar_y as i32, inner_w, bar_h, &theme.resize_bar.bevel);

        // The visual half of the resize-corner affordance —
        // `set_frame_cursor`'s hover-driven cursor change is the other
        // half. Drawn directly within the real hitboxes (not a
        // separately-guessed position), so the grip marks and the area
        // that's actually draggable always agree exactly.
        let grip_color = (theme.resize_bar.bevel.light, theme.resize_bar.bevel.dark);
        for (edge, rect) in &layout.resize_hitboxes {
            let inset = (rect.size.w.min(rect.size.h) / 6).max(1) as i32;
            let size = rect.size.w.min(rect.size.h).saturating_sub(inset as u32 * 2);
            match edge {
                ResizeEdge::SouthEast => {
                    paint::draw_resize_grip(&mut pixmap, rect.pos.x + inset, rect.pos.y + inset, size, grip_color.0, grip_color.1, false);
                }
                ResizeEdge::SouthWest => {
                    paint::draw_resize_grip(&mut pixmap, rect.pos.x + inset, rect.pos.y + inset, size, grip_color.0, grip_color.1, true);
                }
                _ => {}
            }
        }
    }

    let border_color = if request.focused { theme.border.color_active } else { theme.border.color_inactive };
    if border > 0 {
        paint::fill_rect(&mut pixmap, 0, 0, w, border, border_color);
        paint::fill_rect(&mut pixmap, 0, h as i32 - border as i32, w, border, border_color);
        paint::fill_rect(&mut pixmap, 0, 0, border, h, border_color);
        paint::fill_rect(&mut pixmap, w as i32 - border as i32, 0, border, h, border_color);
    }

    let text_color = if request.focused { theme.titlebar.text_color_active } else { theme.titlebar.text_color_inactive };
    let text_inset = 6i32;
    // The free space for the title sits between whichever button is
    // positioned furthest left (its right edge) and whichever is
    // furthest right (its left edge) — found by position, not by a
    // blind min/max over every button's edges: since Close sits left of
    // Miniaturize, a naive `max()` of right edges would actually pick
    // Miniaturize's (furthest-right) edge, and `min()` of left edges
    // would pick Close's (furthest-left) edge — collapsing the text
    // region to zero width instead of bounding it correctly.
    let leftmost_button = layout
        .button_hitboxes
        .iter()
        .min_by_key(|(_, r)| r.pos.x)
        .map(|(_, r)| r.pos.x + r.size.w as i32)
        .unwrap_or(border as i32);
    let rightmost_button = layout
        .button_hitboxes
        .iter()
        .max_by_key(|(_, r)| r.pos.x)
        .map(|(_, r)| r.pos.x)
        .unwrap_or(w as i32 - border as i32);
    let text_x = (leftmost_button + text_inset).min(w as i32);
    let text_w = (rightmost_button - text_inset - text_x).max(0) as u32;
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &request.title,
        &theme.titlebar.font,
        text_color,
        text_x,
        border as i32,
        text_w,
        layout.titlebar_height,
        theme.titlebar.text_align,
    );

    for (kind, rect) in &layout.button_hitboxes {
        if let Some(style) = theme.titlebar.buttons.iter().find(|b| b.kind == *kind) {
            let pressed = request.buttons.iter().any(|b| b.kind == *kind && b.pressed);
            // Real WindowMaker's buttons aren't a separately-colored
            // control — they're the titlebar's own current fill showing
            // straight through, framed only by a bevel; confirmed by
            // reading actual screenshots (a black active titlebar's
            // Close button is black too, not some other gray). That
            // fill is *already* painted — the titlebar fill above
            // covers the button's area too — so unlike `paint::
            // draw_button`, this deliberately does not re-fill the
            // button with its own copy of `titlebar_fill`: re-filling a
            // *sub-rect* with what's actually a diagonal gradient
            // recomputes the gradient relative to that small rect
            // instead of the full titlebar span, producing a visibly
            // different (steeper) gradient than the titlebar around it
            // — a mismatched seam right at the button's edges, confirmed
            // live. When idle, only the bevel needs drawing on top of
            // what's already there; when pressed, `draw_button_pressed`
            // (see its own doc comment for why a mirrored bevel is the
            // wrong tool here) replaces it with a flat black fill.
            if pressed {
                paint::draw_button_pressed(&mut pixmap, rect.pos.x, rect.pos.y, rect.size.w, rect.size.h);
            } else {
                paint::draw_bevel(&mut pixmap, rect.pos.x, rect.pos.y, rect.size.w, rect.size.h, &style.bevel);
            }
            // `text_color` — already the correct contrast choice for
            // text sitting on that exact fill — is equally correct for
            // a glyph sitting on the exact same fill, no separate
            // contrast logic needed.
            draw_button_glyph(&mut pixmap, *kind, *rect, text_color);
        }
    }

    DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() }
}

/// Close and Miniaturize are pixel-for-pixel recreations of real
/// WindowMaker's own default button artwork — `PRED_CLOSE_XPM` and
/// `PRED_ICONIFY_XPM` in its `src/def_pixmaps.h` — reproduced as smooth
/// anti-aliased vector shapes at the *same proportions and composition*
/// the real 10x10 bitmap has (a bold diagonal X; a solid title bar over
/// a hollow bordered body), not as a literal nearest-neighbor bitmap
/// stamp. A direct pixel stamp of a 10px source glyph reads fine at
/// WindowMaker's own native ~15px button size, where each source pixel
/// is close to one real screen pixel — but this theme scales with
/// `CHONKSTEP_SCALE` (a real 5K-display target), and at 3x a 10x10 grid
/// blown up with nearest-neighbor scaling turns every diagonal into a
/// visibly jagged staircase instead of a clean line — confirmed live,
/// exactly the "jagged edges, low quality" symptom. Anti-aliased vector
/// strokes matching the same shape stay crisp at any scale instead.
/// Maximize has no WindowMaker original to copy — real WindowMaker has
/// no maximize button at all — so it keeps this theme's own vector glyph.
fn draw_wm_close_glyph(pixmap: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, color: crate::model::Color) {
    let mut paint = Paint::default();
    paint.set_color(paint::sk_color(color));
    paint.anti_alias = true;
    let stroke_width = ((x1 - x0).min(y1 - y0) * 0.12).max(1.4);
    let stroke = Stroke { width: stroke_width, line_cap: LineCap::Round, ..Default::default() };
    for (ax, ay, bx, by) in [(x0, y0, x1, y1), (x1, y0, x0, y1)] {
        let mut pb = PathBuilder::new();
        pb.move_to(ax, ay);
        pb.line_to(bx, by);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

/// The real miniaturize glyph: a tiny window icon (solid title bar over
/// a hollow bordered body), not a plain dash.
fn draw_wm_iconify_glyph(pixmap: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, color: crate::model::Color) {
    let mut fill_paint = Paint::default();
    fill_paint.set_color(paint::sk_color(color));
    fill_paint.anti_alias = true;

    let stroke_width = ((x1 - x0).min(y1 - y0) * 0.09).max(1.2);
    let bar_h = (y1 - y0) * 0.28;
    if let Some(r) = tiny_skia::Rect::from_xywh(x0, y0, (x1 - x0).max(1.0), bar_h.max(1.0)) {
        pixmap.fill_rect(r, &fill_paint, Transform::identity(), None);
    }

    let mut stroke_paint = Paint::default();
    stroke_paint.set_color(paint::sk_color(color));
    stroke_paint.anti_alias = true;
    let stroke = Stroke { width: stroke_width, ..Default::default() };
    let body_y = y0 + bar_h + stroke_width * 0.5;
    if let Some(r) = tiny_skia::Rect::from_ltrb(x0, body_y, x1, y1) {
        let path = PathBuilder::from_rect(r);
        pixmap.stroke_path(&path, &stroke_paint, &stroke, Transform::identity(), None);
    }
}

pub(crate) fn draw_button_glyph(pixmap: &mut Pixmap, kind: ButtonKind, rect: Rect, color: crate::model::Color) {
    let mut paint = Paint::default();
    paint.set_color(paint::sk_color(color));
    paint.anti_alias = true;

    let short_side = rect.size.w.min(rect.size.h) as f32;
    let inset = (short_side * 0.26).max(2.0);
    let stroke_width = (short_side * 0.13).max(1.6);
    let x0 = rect.pos.x as f32 + inset;
    let y0 = rect.pos.y as f32 + inset;
    let x1 = rect.pos.x as f32 + rect.size.w as f32 - inset;
    let y1 = rect.pos.y as f32 + rect.size.h as f32 - inset;

    // Real WindowMaker's own bitmap glyph technically covers most of
    // the button, but reproducing that same *proportion* with a clean
    // vector line at this desktop's larger button sizes read as
    // oppressively thick and cramped rather than crisp — confirmed
    // live. A more moderate margin/weight here reads as the standard,
    // refined close/miniaturize icon this is aiming for.
    let wm_inset = (short_side * 0.24).max(2.0);
    let wx0 = rect.pos.x as f32 + wm_inset;
    let wy0 = rect.pos.y as f32 + wm_inset;
    let wx1 = rect.pos.x as f32 + rect.size.w as f32 - wm_inset;
    let wy1 = rect.pos.y as f32 + rect.size.h as f32 - wm_inset;

    match kind {
        ButtonKind::Close => draw_wm_close_glyph(pixmap, wx0, wy0, wx1, wy1, color),
        ButtonKind::Miniaturize => draw_wm_iconify_glyph(pixmap, wx0, wy0, wx1, wy1, color),
        ButtonKind::Maximize => {
            let stroke = Stroke { width: stroke_width, line_cap: LineCap::Round, ..Default::default() };
            if let Some(r) = tiny_skia::Rect::from_ltrb(x0, y0, x1, y1) {
                let path = PathBuilder::from_rect(r);
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::ButtonRuntimeState;

    fn sample_request(title: &str, focused: bool) -> DecorationRequest {
        DecorationRequest {
            content_size: Size::new(300, 200),
            title: title.to_string(),
            focused,
            resizable: true,
            buttons: vec![
                ButtonRuntimeState { kind: ButtonKind::Close, hovered: false, pressed: false },
                ButtonRuntimeState { kind: ButtonKind::Maximize, hovered: false, pressed: false },
                ButtonRuntimeState { kind: ButtonKind::Miniaturize, hovered: false, pressed: false },
            ],
        }
    }

    #[test]
    fn layout_places_miniaturize_top_left_and_close_top_right() {
        // Real WindowMaker's own default sides, confirmed by reading
        // actual screenshots — not the reverse this used to assert.
        let theme = crate::default_theme::nextstep_classic();
        let layout = layout_decoration(&theme, &sample_request("xterm", true));

        let mini = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Miniaturize).unwrap().1;
        let close = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Close).unwrap().1;
        assert!(mini.pos.x < close.pos.x, "miniaturize should sit left of close");
        assert!(mini.pos.x < layout.frame_size.w as i32 / 2);
        assert!(close.pos.x > layout.frame_size.w as i32 / 2);
    }

    /// Regression test: the gap between a button and the titlebar's own
    /// corner bevel used to be a flat 3px constant that didn't grow with
    /// `Theme::scaled()` — at higher scales the (correctly scaled)
    /// titlebar bevel and the (correctly scaled) button bevel ended up
    /// touching with no visible gap of plain fill between them, reading
    /// as one thick undifferentiated light smear in that corner instead
    /// of two distinct chiseled elements.
    #[test]
    fn button_margin_leaves_a_real_gap_from_the_titlebar_bevel_at_any_scale() {
        for scale in [1.0, 2.0, 3.0] {
            let theme = crate::default_theme::nextstep_classic().scaled(scale);
            let layout = layout_decoration(&theme, &sample_request("xterm", true));
            let close = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Close).unwrap().1;

            let border = theme.border.width as i32;
            let bevel_inner_edge = border + theme.titlebar.bevel.width as i32;
            let gap = close.pos.x - bevel_inner_edge;
            assert!(
                gap >= theme.titlebar.bevel.width as i32,
                "scale {scale}: only {gap}px between the titlebar's own bevel and the close button — bevels touch/overlap instead of leaving a visible gap"
            );
        }
    }

    #[test]
    fn layout_frame_size_accounts_for_chrome() {
        let theme = crate::default_theme::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = layout_decoration(&theme, &request);

        assert!(layout.frame_size.w >= request.content_size.w);
        assert!(layout.frame_size.h > request.content_size.h, "titlebar/border/resize-bar must add height");
    }

    #[test]
    fn render_produces_a_correctly_sized_nonempty_buffer() {
        let engine = RasterThemeEngine::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);

        let buffer = engine.render(&request, &layout);

        assert_eq!(buffer.width, layout.frame_size.w);
        assert_eq!(buffer.height, layout.frame_size.h);
        assert_eq!(buffer.pixels.len(), (buffer.width * buffer.height * 4) as usize);
        // Not every pixel the same color — proves the gradient/bevel/text
        // pipeline actually drew something rather than leaving a flat or
        // empty buffer.
        let first = &buffer.pixels[0..4];
        assert!(buffer.pixels.chunks_exact(4).any(|px| px != first), "decoration should not be a single flat color");
    }

    /// Regression test: the resize-corner grip marks (the visual half
    /// of the resize affordance — `set_frame_cursor`'s hover-driven
    /// cursor change is the other half, in `wm-x11`) must actually
    /// render something distinguishable from the plain resize bar fill,
    /// and only when the window is resizable at all.
    #[test]
    fn resizable_windows_render_a_visible_grip_in_the_resize_corner() {
        let engine = RasterThemeEngine::nextstep_classic();
        let resizable = sample_request("xterm", true);
        let layout = engine.layout(&resizable);
        let buffer = engine.render(&resizable, &layout);

        let se = layout.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        let mut region_pixels = Vec::new();
        for y in se.pos.y..(se.pos.y + se.size.h as i32) {
            for x in se.pos.x..(se.pos.x + se.size.w as i32) {
                let idx = ((y as u32 * buffer.width + x as u32) * 4) as usize;
                region_pixels.push(&buffer.pixels[idx..idx + 4]);
            }
        }
        let first = region_pixels[0];
        assert!(region_pixels.iter().any(|px| *px != first), "the SE resize corner should show grip marks, not a flat fill");

        // A non-resizable window has no resize bar at all, so no
        // SE/SW hitboxes to draw a grip into in the first place.
        let mut non_resizable = sample_request("xterm", true);
        non_resizable.resizable = false;
        let non_resizable_layout = engine.layout(&non_resizable);
        assert!(non_resizable_layout.resize_hitboxes.is_empty());
    }

    /// Regression test: the resize-corner hitbox used to be a flat 10px
    /// literal that never grew with `Theme::scaled()` — at higher
    /// `CHONKSTEP_SCALE` values the visible grip mark (drawn from this
    /// same hitbox) still scaled up correctly, but the actual clickable/
    /// hoverable area you had to land the cursor on stayed a tiny,
    /// proportionally shrinking target, which is exactly what "have to
    /// be extremely precise with the mouse to get the resize cursor"
    /// felt like in practice.
    #[test]
    fn resize_corner_hitbox_grows_with_scale() {
        let theme = crate::default_theme::nextstep_classic();
        let scaled = theme.scaled(3.0);
        let layout_1x = layout_decoration(&theme, &sample_request("xterm", true));
        let layout_3x = layout_decoration(&scaled, &sample_request("xterm", true));

        let se_1x = layout_1x.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        let se_3x = layout_3x.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        assert!(se_3x.size.w > se_1x.size.w * 2, "the corner hitbox should scale up with the rest of the chrome, not stay fixed");
    }

    #[test]
    fn focused_and_unfocused_render_differently() {
        let engine = RasterThemeEngine::nextstep_classic();
        let focused = sample_request("xterm", true);
        let unfocused = sample_request("xterm", false);
        let layout = engine.layout(&focused);

        let a = engine.render(&focused, &layout);
        let b = engine.render(&unfocused, &layout);

        assert_ne!(a.pixels, b.pixels, "focused/inactive titlebar fills differ in the flagship theme");
    }

    #[test]
    fn title_text_actually_renders_between_the_buttons() {
        // Regression test: `leftmost_button`/`rightmost_button` used to
        // take a blind max()/min() over every button's edges combined,
        // which — since Close sits left of Miniaturize — actually picked
        // Miniaturize's (rightmost) right edge as "leftmost" and Close's
        // (leftmost) left edge as "rightmost", collapsing the title's
        // available width to zero. A titlebar with a real title rendered
        // byte-for-byte identical to one with an empty title (proven by
        // a from-scratch diff test, not a "some pixel differs somewhere"
        // check, since bevel/button highlight pixels alone are enough to
        // make a weaker check pass without any text ever drawing).
        let engine = RasterThemeEngine::nextstep_classic();
        let empty = sample_request("", true);
        let titled = sample_request("HELLOWORLD", true);
        let layout = engine.layout(&empty);
        assert_eq!(layout, engine.layout(&titled), "title text must never affect layout/hit-test geometry");

        let empty_buffer = engine.render(&empty, &layout);
        let titled_buffer = engine.render(&titled, &layout);

        assert_ne!(empty_buffer.pixels, titled_buffer.pixels, "a non-empty title must actually paint glyph pixels between the titlebar buttons");
    }

    #[test]
    fn pressed_button_renders_differently_from_unpressed() {
        let engine = RasterThemeEngine::nextstep_classic();
        let mut request = sample_request("xterm", true);
        let layout = engine.layout(&request);
        let unpressed = engine.render(&request, &layout);

        request.buttons[0].pressed = true; // Close
        let pressed = engine.render(&request, &layout);

        assert_ne!(unpressed.pixels, pressed.pixels, "a pressed button must render its sunken bevel, not the same pixels");
    }
}
