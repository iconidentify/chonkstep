//! The real `wm_theme_api::ThemeEngine` implementation: `RasterThemeEngine`
//! rasterizes window decorations with `tiny-skia` (fills, gradients,
//! bevels, button glyphs) and `cosmic-text` (title text, with font
//! fallback). Pure Rust, no X11/Wayland dependency — see the crate-level
//! doc comment in `lib.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use tiny_skia::Pixmap;
use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationRequest, Point, Rect, ResizeEdge,
    Size, ThemeEngine,
};

use crate::model::Theme;
use crate::paint;

/// The font machinery decoration text is shaped and rasterized with,
/// in a handle that is cheap to clone.
///
/// It exists as a separate type for one reason:
/// `cosmic_text::FontSystem::new()` scans the system's fonts through
/// fontconfig, which costs hundreds of milliseconds and must happen
/// exactly once per session. Restyling is the most routine thing a
/// user does to this desktop — every theme pick is one, and every one
/// of them used to re-exec the process — so a live retheme builds a
/// *new* [`RasterThemeEngine`] around the *same* font state
/// ([`RasterThemeEngine::with_fonts`]) rather than a new engine that
/// re-scans. That is the same argument the dockapp protocol makes for
/// `ThemeChanged` being a message rather than a relaunch, applied to
/// the window manager's own engine.
///
/// `Rc`, not `Arc`, and `RefCell`, not a lock: `ThemeEngine`'s methods
/// take `&self` while shaping needs `&mut`, which already pins an
/// engine to one thread — the window manager's, single-threaded by
/// design. Sharing the state does not widen that; it only lets two
/// engines that never coexist on separate threads hand it over.
#[derive(Clone)]
pub struct FontState {
    font_system: Rc<RefCell<cosmic_text::FontSystem>>,
    swash_cache: Rc<RefCell<cosmic_text::SwashCache>>,
}

impl FontState {
    /// Loads the system font database. Expensive — call it once per
    /// session and clone the handle thereafter.
    pub fn new() -> Self {
        Self {
            font_system: Rc::new(RefCell::new(cosmic_text::FontSystem::new())),
            swash_cache: Rc::new(RefCell::new(cosmic_text::SwashCache::new())),
        }
    }

    /// The shared `FontSystem`, mutably. Shaping and measuring need
    /// `&mut`; the `RefMut` is the loan. Callers keep the loan short
    /// and never hold it across a call that could re-enter this state
    /// (the single-threaded discipline the type's doc describes) —
    /// in practice each borrow lives for exactly one render call.
    ///
    /// Public so the *shell's* text (dock tiles, icon labels, menus,
    /// the switcher) rasterizes out of the same one-per-session
    /// database as the decoration engine. Before this existed, the
    /// desktop and the launcher strip each ran their own
    /// `FontSystem::new()` scan — three font databases in one process
    /// for a type whose whole reason to exist is "exactly once per
    /// session".
    pub fn system(&self) -> std::cell::RefMut<'_, cosmic_text::FontSystem> {
        self.font_system.borrow_mut()
    }

    /// The shared glyph raster cache, mutably — [`FontState::system`]'s
    /// companion, under the same short-loan discipline.
    pub fn swash(&self) -> std::cell::RefMut<'_, cosmic_text::SwashCache> {
        self.swash_cache.borrow_mut()
    }

    /// Whether the database holds a face for `family`. Used to warn
    /// once per engine build that a theme names a font this machine
    /// does not have.
    fn has_family(&self, family: &str) -> bool {
        self.font_system
            .borrow()
            .db()
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name == family))
    }
}

impl Default for FontState {
    fn default() -> Self {
        Self::new()
    }
}

/// Implements `ThemeEngine` for a single `Theme`. Holds the font state
/// `cosmic-text` needs behind [`FontState`]'s `RefCell`s, because
/// `ThemeEngine`'s methods take `&self` (a theme can be shared/boxed as
/// `Box<dyn ThemeEngine>`) while shaping/rasterizing glyphs needs `&mut`
/// access — safe because the whole window manager is single-threaded.
///
/// One engine draws one theme, for the life of that theme: a restyle
/// replaces the engine (see [`RasterThemeEngine::with_fonts`] and
/// `wm_core::WindowManager::set_theme_engine`) rather than mutating it
/// underneath the layouts already derived from it. That keeps
/// "which theme is this layout from" answerable by identity — a
/// half-restyled engine, handing out old metrics and new colors in the
/// same pass, is the state this deliberately cannot reach.
pub struct RasterThemeEngine {
    theme: Theme,
    fonts: FontState,
}

impl RasterThemeEngine {
    /// Builds an engine with its own freshly scanned font database.
    /// The session's *first* engine; every later one should come from
    /// [`Self::with_fonts`] so the scan is not repeated.
    pub fn new(theme: Theme) -> Self {
        Self::with_fonts(theme, FontState::new())
    }

    /// The same engine dressed in a different theme, reusing font state
    /// that is already loaded — how a live retheme or rescale builds
    /// the engine it swaps in. See [`FontState`] for why this is not
    /// simply another [`Self::new`].
    pub fn with_fonts(theme: Theme, fonts: FontState) -> Self {
        if !fonts.has_family(&theme.titlebar.font.family) {
            tracing::warn!(
                family = %theme.titlebar.font.family,
                "configured theme font not found on the system; text will render with whatever fallback sans font fontdb picks"
            );
        }
        Self { theme, fonts }
    }

    /// Convenience constructor wrapping the flagship built-in theme.
    pub fn nextstep_classic() -> Self {
        Self::new(crate::default_theme::nextstep_classic())
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// A handle to this engine's font state, for building the engine
    /// that replaces it. Cheap: two `Rc` bumps.
    pub fn fonts(&self) -> FontState {
        self.fonts.clone()
    }
}

impl ThemeEngine for RasterThemeEngine {
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout {
        layout_decoration(&self.theme, request)
    }

    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer {
        render_decoration(
            &self.theme,
            &mut self.fonts.font_system.borrow_mut(),
            &mut self.fonts.swash_cache.borrow_mut(),
            request,
            layout,
        )
    }
}

/// Pure arithmetic — no rasterization. Miniaturize sits at the
/// titlebar's top-left corner; Close (and Maximize, for a theme that
/// opts it back in) cluster at the top-right — the classic button
/// sides, confirmed by reading actual screenshots, not the reverse
/// this used to be. Each side's buttons claim their slot in the
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

    // `theme.titlebar.button_margin` — the NeXTSTEP inset (see the
    // theme's own doc comment on `buttons`), not a flush `0`: NeXTSTEP's
    // buttons sit inset from the titlebar's corner with visible titlebar
    // fill showing on every side, not stretched flush to the edge the
    // way the later, flatter chrome styles do it.
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
        // The bottom grips follow the classic recipe: each corner owns
        // `corner_width` of the resizebar, delimited on screen by the
        // notch lines `render_decoration` draws at these exact x
        // positions; the middle of the bar resizes straight down. All
        // three regions extend through the bottom border so the frame's
        // outermost pixels still grab.
        let cw = (theme.resize_bar.corner_width as u32).min(frame_size.w / 3).max(1);
        let grip_h = bar_h + border;
        let grip_y = frame_size.h as i32 - grip_h as i32;
        resize_hitboxes.push((
            ResizeEdge::SouthEast,
            Rect::new(Point::new(frame_size.w as i32 - cw as i32, grip_y), Size::new(cw, grip_h)),
        ));
        resize_hitboxes.push((
            ResizeEdge::SouthWest,
            Rect::new(Point::new(0, grip_y), Size::new(cw, grip_h)),
        ));
        let middle_w = frame_size.w.saturating_sub(cw * 2);
        if middle_w > 0 {
            resize_hitboxes.push((
                ResizeEdge::South,
                Rect::new(Point::new(cw as i32, grip_y), Size::new(middle_w, grip_h)),
            ));
        }

        // OS X-style activation zones on the remaining edges and top
        // corners — invisible on purpose: the cursor change is the whole
        // affordance, exactly as on a Mac, while the bottom keeps its
        // visible chiseled resizebar above. Each top corner is an
        // L-shaped pair of arms (`handle` long, `band` thick) hugging
        // the frame's outermost pixels, so the extreme corner reads as a
        // diagonal resize but the titlebar between the arms still
        // drags. The east/west bands cover only the frame's own border
        // strip at client height (the client window swallows pointer
        // events further in), plus the titlebar's outer edge — also how
        // a Mac titlebar behaves at its left/right extremes.
        let band = (titlebar_height / 5).max(border).max(3);
        let w = frame_size.w as i32;
        resize_hitboxes.push((ResizeEdge::NorthWest, Rect::new(Point::new(0, 0), Size::new(handle, band))));
        resize_hitboxes.push((ResizeEdge::NorthWest, Rect::new(Point::new(0, 0), Size::new(band, handle))));
        resize_hitboxes.push((ResizeEdge::NorthEast, Rect::new(Point::new(w - handle as i32, 0), Size::new(handle, band))));
        resize_hitboxes.push((ResizeEdge::NorthEast, Rect::new(Point::new(w - band as i32, 0), Size::new(band, handle))));
        let top_middle_w = frame_size.w.saturating_sub(handle * 2);
        if top_middle_w > 0 {
            resize_hitboxes.push((ResizeEdge::North, Rect::new(Point::new(handle as i32, 0), Size::new(top_middle_w, band))));
        }
        let side_h = frame_size.h.saturating_sub(handle * 2);
        if side_h > 0 {
            resize_hitboxes.push((ResizeEdge::West, Rect::new(Point::new(0, handle as i32), Size::new(band, side_h))));
            resize_hitboxes.push((ResizeEdge::East, Rect::new(Point::new(w - band as i32, handle as i32), Size::new(band, side_h))));
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
    // Defined, opaque pixels over the whole frame first — including the
    // client-area interior the client normally covers. Frames are
    // 32-bit ARGB (see wm-x11's `Argb`), and a fresh pixmap's
    // transparent pixels composite as holes: any gap the client leaves
    // (mid-resize, a client painting late after unshade) showed raw
    // wallpaper punched through the frame, reading as corruption.
    paint::fill_rect(&mut pixmap, 0, 0, w, h, crate::model::Color::rgb(0, 0, 0));
    let border = theme.border.width as u32;
    let inner_w = w.saturating_sub(border * 2);

    let titlebar_fill = if request.focused { &theme.titlebar.active } else { &theme.titlebar.inactive };
    paint::fill_area(&mut pixmap, border as i32, border as i32, inner_w, layout.titlebar_height, titlebar_fill);

    // The free space for the title sits between whichever button is
    // positioned furthest left (its right edge) and whichever is
    // furthest right (its left edge) — found by position, not by a
    // blind min/max over every button's edges: since Close sits left of
    // Miniaturize, a naive `max()` of right edges would actually pick
    // Miniaturize's (furthest-right) edge, and `min()` of left edges
    // would pick Close's (furthest-left) edge — collapsing the text
    // region to zero width instead of bounding it correctly. These same
    // edges are also the relief-segment boundaries below.
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

    // The classic chrome reliefs the titlebar as *segments* — left
    // button square, middle bar, right button square, each getting its
    // own independent double raised relief (the buttons were separate
    // X windows over slices of one shared texture, which is where the
    // split comes from). The visible seams where the segments meet are
    // part of the stock look. The middle segment is drawn here; each
    // button's own relief is drawn with the button below.
    let bevel_t = theme.titlebar.bevel.width.max(1) as u32;
    let mid_w = (rightmost_button - leftmost_button).max(0) as u32;
    paint::draw_raised2_bevel(&mut pixmap, leftmost_button, border as i32, mid_w, layout.titlebar_height, bevel_t);

    if request.resizable {
        let bar_h = (theme.resize_bar.height as u32).min(h);
        let bar_y = h.saturating_sub(border).saturating_sub(bar_h);
        paint::fill_area(&mut pixmap, border as i32, bar_y as i32, inner_w, bar_h, &theme.resize_bar.fill);
        // Notch lines at the same `corner_width` the SouthEast/
        // SouthWest hitboxes use, so the visible grip delimiters and
        // the diagonal-resize zones always agree exactly.
        let cw = (theme.resize_bar.corner_width as u32).min(w / 3).max(1);
        let bar_t = theme.resize_bar.bevel.width.max(1) as u32;
        paint::draw_resizebar_relief(&mut pixmap, border as i32, bar_y as i32, inner_w, bar_h, cw, bar_t);
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
            // The stock chiseled chrome's buttons aren't a
            // separately-colored control — each is the titlebar's own
            // current fill (already painted above) showing straight
            // through, with its own independent double raised relief:
            // one segment of the same three-way split the middle bar
            // got. Pressed feedback is a relative luminance shift with
            // the relief inverted to sunken and the glyph nudged — see
            // `paint::draw_button_pressed` for why not the classic
            // white-flash pushed state.
            let t = style.bevel.width.max(1) as u32;
            if pressed {
                paint::draw_button_pressed(&mut pixmap, rect.pos.x, rect.pos.y, rect.size.w, rect.size.h, paint::pressed_delta(titlebar_fill), t);
                draw_button_glyph(&mut pixmap, *kind, *rect, text_color, true);
            } else {
                paint::draw_raised2_bevel(&mut pixmap, rect.pos.x, rect.pos.y, rect.size.w, rect.size.h, t);
                // `text_color` — the classic stamp color for button
                // glyphs: the mask is filled through in the title
                // *text* color, so it is white on the focused black
                // bar and black on the unfocused gray.
                draw_button_glyph(&mut pixmap, *kind, *rect, text_color, false);
            }
        }
    }

    DecorationBuffer { width: w, height: h, pixels: pixmap.data().to_vec() }
}

/// Close and Miniaturize are pixel-for-pixel recreations of the classic
/// button artwork, reproduced as smooth anti-aliased vector shapes at
/// the *same proportions and composition* the 10x10 originals have (a
/// bold diagonal X; a solid title bar over a hollow bordered body), not
/// as a literal nearest-neighbor bitmap stamp. A direct pixel stamp of
/// a 10px source glyph reads fine at the original ~15px button size,
/// where each source pixel is close to one real screen pixel — but this
/// theme scales with `CHONKSTEP_SCALE` (a real 5K-display target), and
/// at 3x a 10x10 grid blown up with nearest-neighbor scaling turns
/// every diagonal into a visibly jagged staircase instead of a clean
/// line — confirmed live, exactly the "jagged edges, low quality"
/// symptom. Anti-aliased vector strokes matching the same shape stay
/// crisp at any scale instead. Maximize has no classic original to copy
/// — the classic chrome has no maximize button at all — so it keeps
/// this theme's own vector glyph.
///
/// The grids below transcribe those stock glyphs cell for cell. Every
/// non-transparent source pixel is part of the *mask*: the whole mask
/// is stamped in one flat color (the title text color), so the
/// original's own two ink shades never reach the screen and a plain
/// `#` = ink / `.` = transparent grid captures it exactly.
const CLOSE_GLYPH: [&str; 10] = [
    "##......##",
    "###....###",
    ".###..###.",
    "..######..",
    "...####...",
    "...####...",
    "..######..",
    ".###..###.",
    "###....###",
    "##......##",
];

const ICONIFY_GLYPH: [&str; 10] = [
    "##########",
    "##########",
    "##########",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "##########",
];

/// No stock counterpart (the classic chrome has no maximize button) —
/// a plain box outline drawn in the same 10x10 bitmap language.
const MAXIMIZE_GLYPH: [&str; 10] = [
    "##########",
    "##########",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "#........#",
    "##########",
];

/// Stamps a 10x10 glyph mask centered in `rect` in one flat color (see
/// the mask constants above). The masks are authored for the stock 23px
/// button; each mask cell becomes a `round(button/23 * 10)/10`-sized
/// square so the glyph keeps its stock proportion at any
/// `CHONKSTEP_SCALE`, with hard nearest-neighbor edges — scaling a 10px
/// bitmap, not redrawing it. `pressed` nudges the stamp one cell
/// down-right, the classic one-pixel pressed-state offset.
pub(crate) fn draw_button_glyph(pixmap: &mut Pixmap, kind: ButtonKind, rect: Rect, color: crate::model::Color, pressed: bool) {
    let mask: &[&str; 10] = match kind {
        ButtonKind::Close => &CLOSE_GLYPH,
        ButtonKind::Miniaturize => &ICONIFY_GLYPH,
        ButtonKind::Maximize => &MAXIMIZE_GLYPH,
    };
    let cell = ((rect.size.w.min(rect.size.h) as f32) / 23.0).round().max(1.0) as i32;
    let glyph_span = cell * 10;
    let nudge = if pressed { cell } else { 0 };
    let x0 = rect.pos.x + (rect.size.w as i32 - glyph_span) / 2 + nudge;
    let y0 = rect.pos.y + (rect.size.h as i32 - glyph_span) / 2 + nudge;
    // Magnified diagonals are the one place a scaled 1-bit stamp reads
    // as jagged rather than crisp — see `draw_close_glyph_smooth`.
    if kind == ButtonKind::Close && cell > 1 {
        draw_close_glyph_smooth(pixmap, x0, y0, glyph_span, color);
        return;
    }
    for (row, line) in mask.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if ch == b'#' {
                paint::fill_rect(pixmap, x0 + col as i32 * cell, y0 + row as i32 * cell, cell as u32, cell as u32, color);
            }
        }
    }
}

/// The close glyph at `CHONKSTEP_SCALE` > 1. At native size the
/// `CLOSE_GLYPH` bitmap stamp is pixel-identical to the original, and
/// that path still runs at `cell == 1`. But the classic chrome never
/// draws magnified — it predates HiDPI scaling entirely, so there is no
/// authentic "scaled-up" reference to copy — and nearest-neighbor
/// magnification of a 10px 1-bit staircase reads as jagged, not crisp
/// (confirmed live at scale 2). Scaled, the X is instead redrawn as
/// what the bitmap *depicts*: two corner-to-corner diagonal bars,
/// anti-aliased, proportioned to the bitmap footprint (each arm spans
/// ~2.2 of the 10 bitmap cells perpendicular to its axis, tips filling
/// the glyph box's corners via square caps). The iconify/maximize boxes
/// stay on the stamp path at every scale: their edges are axis-aligned,
/// where hard magnified pixels are exactly what a scaled bitmap should
/// look like.
fn draw_close_glyph_smooth(pixmap: &mut Pixmap, x0: i32, y0: i32, span: i32, color: crate::model::Color) {
    use tiny_skia::{LineCap, Paint, PathBuilder, Stroke, Transform};

    let mut paint = Paint::default();
    paint.set_color(paint::sk_color(color));
    paint.anti_alias = true;

    let s = span as f32;
    let width = s * 0.22;
    let stroke = Stroke { width, line_cap: LineCap::Square, ..Default::default() };
    // Square caps extend half the stroke width past each endpoint, so
    // insetting the endpoints by that much lands the flattened tips
    // exactly in the glyph box's corners, like the bitmap's.
    let m = width * 0.5;
    let (lo_x, lo_y) = (x0 as f32 + m, y0 as f32 + m);
    let (hi_x, hi_y) = (x0 as f32 + s - m, y0 as f32 + s - m);
    for (ax, ay, bx, by) in [(lo_x, lo_y, hi_x, hi_y), (hi_x, lo_y, lo_x, hi_y)] {
        let mut pb = PathBuilder::new();
        pb.move_to(ax, ay);
        pb.line_to(bx, by);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
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
        // The classic sides, confirmed by reading actual screenshots —
        // not the reverse this used to assert.
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
        assert!(
            buffer.pixels.as_chunks::<4>().0.iter().any(|px| px.as_slice() != first),
            "decoration should not be a single flat color"
        );
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

    /// The shade (windowshade / roll-up) render. `wm-core`'s
    /// `shaded_paint_inputs` asks for exactly this shape — the theme's
    /// own `shaded_frame_height` with `resizable` cleared — and the
    /// result has to be a *whole* decoration at that height, not the top
    /// slice of a taller one: the Wayland compositor draws the
    /// decoration buffer at the buffer's own size with nothing to clip
    /// it, so anything the theme paints outside those rows is what the
    /// user sees on screen.
    ///
    /// The `resizable` half is the part that isn't obvious. At shaded
    /// height there is no room below the titlebar for a resize bar, so a
    /// theme still told to draw one puts it straight over the titlebar's
    /// bottom edge — which is why the bottom border strip is checked
    /// against the top one rather than merely checking the height.
    #[test]
    fn a_shaded_frame_renders_as_a_complete_titlebar_only_decoration() {
        let engine = RasterThemeEngine::nextstep_classic();
        let border = engine.theme().border.width as u32;
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);
        assert_eq!(
            layout.shaded_frame_height,
            layout.titlebar_height + border * 2,
            "the shade keeps the titlebar and its own top/bottom border, nothing else"
        );

        let full = engine.render(&request, &layout);

        let mut shaded_request = request.clone();
        shaded_request.resizable = false;
        let mut shaded_layout = layout.clone();
        shaded_layout.frame_size.h = layout.shaded_frame_height;
        shaded_layout.resize_hitboxes.clear();
        let shaded = engine.render(&shaded_request, &shaded_layout);

        assert_eq!(shaded.width, layout.frame_size.w);
        assert_eq!(shaded.height, layout.shaded_frame_height);
        assert_eq!(shaded.pixels.len(), (shaded.width * shaded.height * 4) as usize);

        let row = |buffer: &DecorationBuffer, y: u32| {
            let stride = (buffer.width * 4) as usize;
            buffer.pixels[y as usize * stride..(y as usize + 1) * stride].to_vec()
        };

        // Everything above the bottom border — titlebar fill, bevel
        // segments, title text, button glyphs — is pixel-for-pixel the
        // decoration the unshaded frame draws in the same rows. A shade
        // is the same titlebar, not a redrawn one.
        for y in 0..(layout.shaded_frame_height - border) {
            assert_eq!(row(&shaded, y), row(&full, y), "shaded row {y} differs from the unshaded frame's titlebar");
        }
        // And the rows the resize bar would have landed in are plain
        // border, identical to the top border strip.
        for y in 0..border {
            assert_eq!(
                row(&shaded, layout.shaded_frame_height - border + y),
                row(&shaded, y),
                "the shade's bottom border strip is not plain border — something (the resize bar) painted into it"
            );
        }
    }

    /// The OS X-style zones: every edge and corner of a resizable frame
    /// must be reachable, with the extreme corner pixels resolving to a
    /// *corner* (diagonal) resize rather than the adjacent edge band —
    /// the corner arms are pushed ahead of the edge bands and hit-tests
    /// take the first match, which is what this pins down.
    #[test]
    fn all_eight_resize_zones_are_exposed_and_corners_win_at_the_extremes() {
        let engine = RasterThemeEngine::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);

        for edge in [
            ResizeEdge::North,
            ResizeEdge::South,
            ResizeEdge::East,
            ResizeEdge::West,
            ResizeEdge::NorthEast,
            ResizeEdge::NorthWest,
            ResizeEdge::SouthEast,
            ResizeEdge::SouthWest,
        ] {
            assert!(
                layout.resize_hitboxes.iter().any(|(e, _)| *e == edge),
                "resizable frame should expose a {edge:?} zone"
            );
        }

        // Same first-match rule `wm-core`'s hit_test applies.
        let first_edge_at = |p: Point| layout.resize_hitboxes.iter().find(|(_, r)| r.contains(p)).map(|(e, _)| *e);
        let w = layout.frame_size.w as i32;
        let h = layout.frame_size.h as i32;
        assert_eq!(first_edge_at(Point::new(0, 0)), Some(ResizeEdge::NorthWest));
        assert_eq!(first_edge_at(Point::new(w - 1, 0)), Some(ResizeEdge::NorthEast));
        assert_eq!(first_edge_at(Point::new(w / 2, 0)), Some(ResizeEdge::North));
        assert_eq!(first_edge_at(Point::new(0, h / 2)), Some(ResizeEdge::West));
        assert_eq!(first_edge_at(Point::new(w - 1, h / 2)), Some(ResizeEdge::East));
        // The titlebar's center must stay a drag region, not a resize.
        assert_eq!(first_edge_at(Point::new(w / 2, layout.titlebar_height as i32 / 2 + 4)), None);
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
