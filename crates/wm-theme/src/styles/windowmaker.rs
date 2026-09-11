//! Original WindowMaker recipe, moved intact from raster.rs. The baseline
//! geometry/pixel oracle protects this implementation from accidental restyling.
use std::collections::VecDeque;
use tiny_skia::Pixmap;
use wm_theme_api::{ButtonKind, DecorationBuffer, DecorationLayout, DecorationPart,
    DecorationRequest, DecorationSurface, Point, Rect, ResizeEdge, Size};
use crate::{model::Theme, paint};

/// Pure arithmetic — no rasterization. Miniaturize sits at the
/// titlebar's top-left corner; Close (and Maximize, for a theme that
/// opts it back in) cluster at the top-right — the classic button
/// sides, confirmed by reading actual screenshots, not the reverse
/// this used to be. Each side's buttons claim their slot in the
/// order they appear in `theme.titlebar.buttons` — the first one
/// encountered on a given side lands outermost (closest to the corner),
/// later ones on that same side stack inward from there.
pub(crate) fn layout_decoration(theme: &Theme, request: &DecorationRequest) -> DecorationLayout {
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

/// Paints only the four visible chrome bands. The frame interior is filled by
/// each backend with a cheap solid element/window background, preserving the
/// old mid-resize gap behavior without retaining a client-sized RGBA image.
pub(crate) fn render_sparse_decoration(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    title_cache: &mut VecDeque<(u32, DecorationRequest, DecorationBuffer)>,
    scale_key: u32,
    request: &DecorationRequest,
    layout: &DecorationLayout,
) -> DecorationSurface {
    let frame = layout.frame_size;
    let border = theme.border.width as u32;
    let top_h = layout.client_offset.y.max(0) as u32;
    let bottom_h = frame
        .h
        .saturating_sub(top_h)
        .saturating_sub(request.content_size.h);
    let mut parts = Vec::with_capacity(4);

    if frame.w > 0 && top_h > 0 {
        // Height does not affect title shaping. Normalizing it gives repeated
        // focus/title repaints and constrained resize passes a small bounded
        // cache keyed by the visible inputs: title/font scale/frame width.
        let mut title_key = request.clone();
        title_key.content_size.h = 0;
        let cached = title_cache
            .iter()
            .find(|(cached_scale, cached_request, _)| {
                *cached_scale == scale_key && cached_request == &title_key
            })
            .map(|(_, _, buffer)| buffer.clone());
        let was_cached = cached.is_some();
        let top = cached.unwrap_or_else(|| {
            // Give the existing whole-frame painter one disposable bottom
            // border row-band and crop it away. This keeps its exact classic
            // bevel/button output while allocating only titlebar-sized memory.
            let mut top_layout = layout.clone();
            top_layout.frame_size = Size::new(frame.w, top_h.saturating_add(border));
            let mut top_request = request.clone();
            top_request.content_size.h = 0;
            top_request.resizable = false;
            let rendered = render_decoration(theme, font_system, swash_cache, &top_request, &top_layout);
            crop_rows(&rendered, 0, top_h)
        });
        if !was_cached {
            title_cache.push_back((scale_key, title_key, top.clone()));
            while title_cache.len() > 16 {
                title_cache.pop_front();
            }
        }
        parts.push(DecorationPart { offset: Point::new(0, 0), buffer: top });
    }

    if frame.w > 0 && bottom_h > 0 {
        // The full painter puts the resize bar immediately above its bottom
        // border. Add a disposable top border and crop it off.
        let mut bottom_layout = DecorationLayout {
            frame_size: Size::new(frame.w, bottom_h.saturating_add(border)),
            client_offset: Point::new(border as i32, border as i32),
            titlebar_height: 0,
            button_hitboxes: Vec::new(),
            resize_hitboxes: Vec::new(),
            shaded_frame_height: 0,
        };
        // `render_decoration` only consults this field for geometry already
        // represented above; keep it explicit for future theme additions.
        bottom_layout.client_offset.y = border as i32;
        let mut bottom_request = request.clone();
        bottom_request.content_size = Size::new(frame.w.saturating_sub(border * 2), 0);
        bottom_request.title.clear();
        bottom_request.buttons.clear();
        let rendered = render_decoration(theme, font_system, swash_cache, &bottom_request, &bottom_layout);
        let bottom = crop_rows(&rendered, border, bottom_h);
        parts.push(DecorationPart {
            offset: Point::new(0, frame.h.saturating_sub(bottom_h) as i32),
            buffer: bottom,
        });
    }

    if border > 0 && request.content_size.h > 0 {
        let color = if request.focused { theme.border.color_active } else { theme.border.color_inactive };
        let strip = solid_buffer(border, request.content_size.h, color);
        parts.push(DecorationPart { offset: Point::new(0, top_h as i32), buffer: strip.clone() });
        parts.push(DecorationPart {
            offset: Point::new(frame.w.saturating_sub(border) as i32, top_h as i32),
            buffer: strip,
        });
    }

    DecorationSurface { frame_size: frame, parts }
}

fn crop_rows(buffer: &DecorationBuffer, first: u32, height: u32) -> DecorationBuffer {
    let first = first.min(buffer.height);
    let height = height.min(buffer.height.saturating_sub(first));
    let stride = buffer.width as usize * 4;
    let start = first as usize * stride;
    let end = start + height as usize * stride;
    DecorationBuffer {
        width: buffer.width,
        height,
        pixels: buffer.pixels.get(start..end).unwrap_or_default().to_vec(),
    }
}

fn solid_buffer(width: u32, height: u32, color: crate::model::Color) -> DecorationBuffer {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for rgba in pixels.as_chunks_mut::<4>().0 {
        rgba.copy_from_slice(&[color.r, color.g, color.b, 0xff]);
    }
    DecorationBuffer { width, height, pixels }
}

pub(crate) fn render_decoration(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    request: &DecorationRequest,
    layout: &DecorationLayout,
) -> DecorationBuffer {
    let (w, h) = (layout.frame_size.w.max(1), layout.frame_size.h.max(1));
    let Some(mut pixmap) = Pixmap::new(w, h) else {
        // Client geometry is bounded before reaching the theme, but
        // ThemeEngine is a public contract and future callers must not
        // be able to turn an allocation refusal into a compositor-main-
        // thread panic. A defined opaque pixel keeps every backend's
        // buffer import path total while the log preserves the bad
        // dimensions for diagnosis.
        tracing::error!(width = w, height = h, "could not allocate decoration raster; using a 1x1 fallback");
        return DecorationBuffer {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 0xff],
        };
    };
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
    let title = paint::elide(&request.title, text_w, theme.titlebar.font.size);
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &title,
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

    DecorationBuffer { width: w, height: h, pixels: pixmap.take() }
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
