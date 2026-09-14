//! Sparse modern frames: cached title bands, one-pixel sides, and independent
//! input margins. The client rectangle is never allocated or rasterized here.
use crate::{model::Theme, modern::Chrome, paint};
use std::collections::VecDeque;
use tiny_skia::Pixmap;
use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationPart, DecorationRequest,
    DecorationSolid, DecorationSurface, Point, Rect, ResizeEdge, Size, MAX_CLIENT_WINDOW_DIMENSION,
};

pub(crate) fn layout(theme: &Theme, request: &DecorationRequest) -> DecorationLayout {
    let m = Chrome::from_theme(theme).frame.normalized();
    let border = u32::from(m.border.max(1));
    let margin = u32::from(m.input_margin);
    let title = u32::from(m.title_height).max(border + 1);
    let w = request
        .content_size
        .w
        .min(MAX_CLIENT_WINDOW_DIMENSION)
        .saturating_add((margin + border) * 2);
    let h = request
        .content_size
        .h
        .min(MAX_CLIENT_WINDOW_DIMENSION)
        .saturating_add(margin * 2 + title + border);
    let frame_size = Size::new(w, h);
    let mut buttons = Vec::with_capacity(theme.titlebar.buttons.len());
    let size = u32::from(m.button_size).min(title.saturating_sub(border * 2));
    let mut right = w.saturating_sub(margin + border + u32::from(m.padding));
    let left = margin + border + u32::from(m.padding);
    for button in theme.titlebar.buttons.iter().rev() {
        if size == 0 || right < left.saturating_add(size) {
            break;
        }
        if button.kind == ButtonKind::Maximize && !request.resizable {
            continue;
        }
        right -= size;
        buttons.push((
            button.kind,
            Rect::new(
                Point::new(right as i32, (margin + (title - size) / 2) as i32),
                Size::new(size, size),
            ),
        ));
        right = right.saturating_sub(u32::from(m.button_gap));
    }
    let mut resize = Vec::with_capacity(if request.resizable { 8 } else { 0 });
    if request.resizable {
        let edge = margin + border;
        let corner = (title / 2).max(edge).min(w / 2).min(h / 2);
        for (kind, x, y, rw, rh) in [
            (ResizeEdge::NorthWest, 0, 0, corner, edge),
            (ResizeEdge::NorthEast, w - corner, 0, corner, edge),
            (ResizeEdge::SouthWest, 0, h - corner, corner, corner),
            (
                ResizeEdge::SouthEast,
                w - corner,
                h - corner,
                corner,
                corner,
            ),
            (ResizeEdge::North, corner, 0, w - corner * 2, edge),
            (ResizeEdge::South, corner, h - edge, w - corner * 2, edge),
            (ResizeEdge::West, 0, edge, edge, h - edge - corner),
            (ResizeEdge::East, w - edge, edge, edge, h - edge - corner),
        ] {
            if rw > 0 && rh > 0 {
                resize.push((
                    kind,
                    Rect::new(Point::new(x as i32, y as i32), Size::new(rw, rh)),
                ));
            }
        }
    }
    DecorationLayout {
        frame_size,
        input_margin: margin,
        client_offset: Point::new((margin + border) as i32, (margin + title) as i32),
        titlebar_height: title,
        button_hitboxes: buttons,
        resize_hitboxes: resize,
        shaded_frame_height: margin * 2 + title + border,
    }
}

/// The edge frame for a client that draws its own titlebar: the outline,
/// input margin, shadow and corner shape of [`layout`], with the title band
/// reduced to a border like the other three sides.
pub(crate) fn layout_edges(theme: &Theme, request: &DecorationRequest) -> DecorationLayout {
    let m = Chrome::from_theme(theme).frame.normalized();
    let border = u32::from(m.border.max(1));
    let margin = u32::from(m.input_margin);
    let title = u32::from(m.title_height).max(border + 1);
    let inset = margin + border;
    let w = request.content_size.w.min(MAX_CLIENT_WINDOW_DIMENSION).saturating_add(inset * 2);
    let h = request.content_size.h.min(MAX_CLIENT_WINDOW_DIMENSION).saturating_add(inset * 2);
    let frame_size = Size::new(w, h);
    let resize = if request.resizable {
        // The full frame's corner reach, so a corner grip is where the
        // hand expects it with or without a title band above.
        super::edge_ring(frame_size, inset, inset, inset, inset, (title / 2).max(inset))
    } else {
        Vec::new()
    };
    DecorationLayout {
        frame_size,
        input_margin: margin,
        client_offset: Point::new(inset as i32, inset as i32),
        titlebar_height: 0,
        button_hitboxes: Vec::new(),
        resize_hitboxes: resize,
        shaded_frame_height: h,
    }
}

/// Paints [`layout_edges`]: four solid borders, with the shadow and corner
/// shape a full frame of this theme wears.
pub(crate) fn render_edges(theme: &Theme, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationSurface {
    let chrome = Chrome::from_theme(theme);
    let visual = layout.visual_bounds();
    let (w, h) = (visual.size.w, visual.size.h);
    let border = u32::from(chrome.frame.border.max(1)).min(w / 2).min(h / 2);
    let color = if request.focused {
        theme.border.color_active
    } else {
        theme.border.color_inactive
    };
    let rgb = [color.r, color.g, color.b];
    let mut solids = Vec::with_capacity(4);
    let inner_h = h.saturating_sub(border * 2);
    for (x, y, rw, rh) in [
        (0, 0, w, border),
        (0, h - border, w, border),
        (0, border, border, inner_h),
        (w - border, border, border, inner_h),
    ] {
        if rw > 0 && rh > 0 {
            solids.push(DecorationSolid {
                rect: Rect::new(
                    Point::new(visual.pos.x + x as i32, visual.pos.y + y as i32),
                    Size::new(rw, rh),
                ),
                rgb,
            });
        }
    }
    let (shadow, shape) = frame_effects(&chrome, visual, rgb);
    DecorationSurface {
        frame_size: layout.frame_size,
        parts: Vec::new(),
        solids,
        shadow,
        shape,
    }
}

/// Cold-cache output retains only the compressed representation. The full title
/// pixmap is discarded after splitting; it is never kept as a second cache.
pub(crate) struct CachedTitle {
    scale: u32,
    request: DecorationRequest,
    size: Size,
    buttons: Vec<(ButtonKind, Rect)>,
    parts: Vec<DecorationPart>,
    solids: Vec<DecorationSolid>,
}

pub(crate) fn render(
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    cache: &mut cosmic_text::SwashCache,
    titles: &mut VecDeque<CachedTitle>,
    scale: u32,
    request: &DecorationRequest,
    layout: &DecorationLayout,
) -> DecorationSurface {
    // The public offline API can receive a forged layout. Check dimensions and
    // offsets before any multiplication/allocation, independently of layout().
    let valid = layout.frame_size.w <= MAX_CLIENT_WINDOW_DIMENSION + 288
        && layout.frame_size.h <= MAX_CLIENT_WINDOW_DIMENSION + 784
        && layout.client_offset.x >= 0
        && layout.client_offset.y >= 0
        && layout.client_offset.x as u32 <= layout.frame_size.w
        && layout.client_offset.y as u32 <= layout.frame_size.h;
    if !valid {
        return DecorationSurface {
            frame_size: Size::default(),
            parts: Vec::new(),
            solids: Vec::new(),
            shadow: None,
            shape: None,
        };
    }
    let chrome = Chrome::from_theme(theme);
    let visual = layout.visual_bounds();
    let w = visual.size.w;
    let top_h = (layout.client_offset.y - visual.pos.y).max(0) as u32;
    let top_h = top_h.min(visual.size.h).min(512);
    let border = u32::from(chrome.frame.border.max(1)).min(w / 2);
    let color = if request.focused {
        theme.border.color_active
    } else {
        theme.border.color_inactive
    };
    let rgb = [color.r, color.g, color.b];
    let size = Size::new(w, top_h);
    let cached = titles.iter().position(|entry| {
        entry.scale == scale
            && entry.size == size
            && entry.request.title == request.title
            && entry.request.focused == request.focused
            && entry.request.buttons == request.buttons
            && entry.buttons == layout.button_hitboxes
    });
    let index = cached.unwrap_or_else(|| {
        let buffer = title(theme, fonts, cache, request, layout, w, top_h);
        let (parts, solids) = split_title(&buffer, border);
        let mut key = request.clone();
        key.content_size.h = 0;
        if titles.len() == 16 {
            titles.pop_front();
        }
        titles.push_back(CachedTitle {
            scale,
            request: key,
            size,
            buttons: layout.button_hitboxes.clone(),
            parts,
            solids,
        });
        titles.len() - 1
    });
    let cached = &titles[index];
    // Exactly two metadata vectors plus at most four pixel vectors. Reserving
    // perimeter slots here prevents a clone-then-append reallocation on hits.
    let mut parts = Vec::with_capacity(cached.parts.len());
    for part in &cached.parts {
        parts.push(DecorationPart {
            offset: Point::new(visual.pos.x + part.offset.x, visual.pos.y + part.offset.y),
            buffer: part.buffer.clone(),
        });
    }
    let mut solids = Vec::with_capacity(cached.solids.len() + 3);
    for solid in &cached.solids {
        let mut solid = *solid;
        solid.rect.pos.x += visual.pos.x;
        solid.rect.pos.y += visual.pos.y;
        solids.push(solid);
    }
    let remaining = visual.size.h.saturating_sub(top_h);
    let bottom_h = border.min(remaining);
    if bottom_h > 0 {
        solids.push(DecorationSolid {
            rect: Rect::new(
                Point::new(
                    visual.pos.x,
                    visual.pos.y + (visual.size.h - bottom_h) as i32,
                ),
                Size::new(w, bottom_h),
            ),
            rgb,
        });
    }
    let side_h = remaining.saturating_sub(bottom_h);
    if border > 0 && side_h > 0 {
        for x in [visual.pos.x, visual.pos.x + (w - border) as i32] {
            solids.push(DecorationSolid {
                rect: Rect::new(
                    Point::new(x, visual.pos.y + top_h as i32),
                    Size::new(border, side_h),
                ),
                rgb,
            });
        }
    }
    let (shadow, shape) = frame_effects(&chrome, visual, rgb);
    DecorationSurface {
        frame_size: layout.frame_size,
        parts,
        solids,
        shadow,
        shape,
    }
}

/// The shadow and corner shape a modern frame wears around `visual`,
/// whether or not it has a title band.
fn frame_effects(
    chrome: &Chrome,
    visual: Rect,
    rgb: [u8; 3],
) -> (Option<wm_theme_api::DecorationShadow>, Option<wm_theme_api::DecorationShape>) {
    let shadow = &chrome.shadow;
    let shadow = (shadow.color.a > 0).then_some(
        wm_theme_api::DecorationShadow {
            rect: visual,
            radius: chrome.frame.radius.min(chrome.frame.title_height),
            offset: Point::new(i32::from(shadow.x), i32::from(shadow.y)),
            blur: shadow.blur,
            rgba: [
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a,
            ],
        }
        .normalized(),
    );
    let shape = (chrome.frame.radius > 0).then_some(
        wm_theme_api::DecorationShape {
            rect: visual,
            radius: chrome.frame.radius.min(chrome.frame.title_height),
            border: chrome.frame.border,
            border_rgb: rgb,
        }
        .normalized(),
    );
    (shadow, shape)
}

/// A row is flat when its interior and each border run are uniform and opaque.
/// Testing the entire row as one color misses almost every blank title row,
/// because the border and the interior deliberately have different colors.
fn flat_row(row: &[u8], width: u32, border: u32) -> Option<[[u8; 3]; 3]> {
    let mut colors = [[0; 3]; 3];
    for (index, (x, w)) in [
        (0, border),
        (border, width - border * 2),
        (width - border, border),
    ]
    .into_iter()
    .enumerate()
    {
        if w == 0 {
            continue;
        }
        let run = &row[x as usize * 4..(x + w) as usize * 4];
        let pixel = &run[..4];
        if pixel[3] != 255 || !run.as_chunks::<4>().0.iter().all(|p| p.as_slice() == pixel) {
            return None;
        }
        colors[index].copy_from_slice(&pixel[..3]);
    }
    Some(colors)
}

fn split_title(
    buffer: &DecorationBuffer,
    border: u32,
) -> (Vec<DecorationPart>, Vec<DecorationSolid>) {
    let (w, h) = (buffer.width, buffer.height);
    if w == 0 || h == 0 {
        return (Vec::new(), Vec::new());
    }
    let stride = w as usize * 4;
    let mut rows: Vec<_> = buffer
        .pixels
        .chunks_exact(stride)
        .map(|row| flat_row(row, w, border))
        .collect();
    // Exotic text can produce many alternating ink/blank rows. Keep warm
    // allocation count bounded by merging the smallest intervening gaps.
    loop {
        let mut runs = 0;
        let mut inside = false;
        let mut gap_start = None;
        let mut best: Option<(usize, usize)> = None;
        for (y, row) in rows.iter().enumerate() {
            if row.is_none() {
                if !inside {
                    runs += 1;
                    if let Some(start) = gap_start.take() {
                        if best.is_none_or(|(a, b)| y - start < b - a) {
                            best = Some((start, y));
                        }
                    }
                }
                inside = true;
            } else if inside {
                inside = false;
                gap_start = Some(y);
            }
        }
        if runs <= 4 {
            break;
        }
        let Some((start, end)) = best else {
            break;
        };
        rows[start..end].fill(None);
    }
    let mut parts = Vec::with_capacity(4);
    let mut solids: Vec<DecorationSolid> = Vec::new();
    let mut y = 0;
    while y < rows.len() {
        if let Some(colors) = rows[y] {
            let end = (y + 1..rows.len())
                .find(|&next| rows[next] != Some(colors))
                .unwrap_or(rows.len());
            for (index, (x, width)) in [(0, border), (border, w - border * 2), (w - border, border)]
                .into_iter()
                .enumerate()
            {
                if width > 0 {
                    solids.push(DecorationSolid {
                        rect: Rect::new(
                            Point::new(x as i32, y as i32),
                            Size::new(width, (end - y) as u32),
                        ),
                        rgb: colors[index],
                    });
                }
            }
            y = end;
        } else {
            let end = (y + 1..rows.len())
                .find(|&next| rows[next].is_some())
                .unwrap_or(rows.len());
            parts.push(DecorationPart {
                offset: Point::new(0, y as i32),
                buffer: DecorationBuffer {
                    width: w,
                    height: (end - y) as u32,
                    pixels: buffer.pixels[y * stride..end * stride].to_vec(),
                },
            });
            y = end;
        }
    }
    (parts, solids)
}

fn solid(w: u32, h: u32, color: crate::model::Color) -> DecorationBuffer {
    let mut pixels = vec![0; w as usize * h as usize * 4];
    for p in pixels.as_chunks_mut::<4>().0 {
        p.copy_from_slice(&[color.r, color.g, color.b, 255]);
    }
    DecorationBuffer {
        width: w,
        height: h,
        pixels,
    }
}

fn title(
    theme: &Theme,
    fonts: &mut cosmic_text::FontSystem,
    cache: &mut cosmic_text::SwashCache,
    request: &DecorationRequest,
    layout: &DecorationLayout,
    w: u32,
    h: u32,
) -> DecorationBuffer {
    let Some(mut image) = Pixmap::new(w.max(1), h.max(1)) else {
        return solid(1, 1, theme.terminal.bg);
    };
    let chrome = Chrome::from_theme(theme);
    let border = u32::from(chrome.frame.border.max(1));
    let line = if request.focused {
        theme.border.color_active
    } else {
        theme.border.color_inactive
    };
    let fill = if request.focused {
        &theme.titlebar.active
    } else {
        &theme.titlebar.inactive
    };
    paint::fill_rect(&mut image, 0, 0, w, h, line);
    paint::fill_area(
        &mut image,
        border as i32,
        border as i32,
        w.saturating_sub(border * 2),
        h.saturating_sub(border * 2),
        fill,
    );
    if request.focused && chrome.frame.focus_edge > 0 {
        paint::fill_rect(
            &mut image,
            border as i32,
            border as i32,
            w.saturating_sub(border * 2),
            u32::from(chrome.frame.focus_edge),
            chrome.accent,
        );
    }
    let ink = if request.focused {
        theme.titlebar.text_color_active
    } else {
        theme.titlebar.text_color_inactive
    };
    let margin = layout.input_margin as i32;
    let pad = i32::from(chrome.frame.padding);
    let mark = (theme.titlebar.font.size * 0.64).round().max(1.0) as u32;
    let mark_x = border as i32 + pad;
    if request.focused {
        let mark_y = (h.saturating_sub(mark) / 2) as i32;
        if chrome.frame.round_focus_mark {
            let radius = mark as f32 / 2.0;
            if let Some(path) = tiny_skia::PathBuilder::from_circle(
                mark_x as f32 + radius,
                mark_y as f32 + radius,
                radius,
            ) {
                let mut ink = tiny_skia::Paint::default();
                let color = chrome.accent;
                ink.set_color_rgba8(color.r, color.g, color.b, 255);
                image.fill_path(
                    &path,
                    &ink,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        } else {
            paint::fill_rect(&mut image, mark_x, mark_y, mark, mark, chrome.accent);
        }
    }
    let text_x = mark_x + mark as i32 + pad;
    let right = layout
        .button_hitboxes
        .iter()
        .map(|(_, r)| r.pos.x - margin)
        .min()
        .unwrap_or(w as i32 - pad);
    let text_w = (right - pad - text_x).max(0) as u32;
    let text = paint::elide(&request.title, text_w, theme.titlebar.font.size);
    paint::draw_text(
        &mut image,
        fonts,
        cache,
        &text,
        &theme.titlebar.font,
        ink,
        text_x,
        0,
        text_w,
        h,
        crate::model::TextAlign::Left,
    );
    for (kind, hit) in &layout.button_hitboxes {
        let (x, y) = (hit.pos.x - margin, hit.pos.y - margin);
        let runtime = request.buttons.iter().find(|b| b.kind == *kind);
        if runtime.is_some_and(|b| b.hovered || b.pressed) {
            paint::fill_rect(
                &mut image,
                x,
                y,
                hit.size.w,
                hit.size.h,
                if runtime.is_some_and(|b| b.pressed) {
                    chrome.selection
                } else {
                    chrome.line
                },
            );
        }
        let s = (hit.size.w * 2 / 5).max(3).min(hit.size.w);
        let (gx, gy) = (
            x + (hit.size.w - s) as i32 / 2,
            y + (hit.size.h - s) as i32 / 2,
        );
        let t = border.min(s).max(1);
        match kind {
            ButtonKind::Miniaturize => {
                paint::fill_rect(&mut image, gx, gy + s as i32 / 2, s, t, ink)
            }
            ButtonKind::Maximize => {
                paint::fill_rect(&mut image, gx, gy, s, t, ink);
                paint::fill_rect(&mut image, gx, gy + (s - t) as i32, s, t, ink);
                paint::fill_rect(&mut image, gx, gy, t, s, ink);
                paint::fill_rect(&mut image, gx + (s - t) as i32, gy, t, s, ink);
            }
            ButtonKind::Close => {
                for step in 0..s {
                    paint::fill_rect(&mut image, gx + step as i32, gy + step as i32, t, t, ink);
                    paint::fill_rect(
                        &mut image,
                        gx + (s - 1 - step) as i32,
                        gy + step as i32,
                        t,
                        t,
                        ink,
                    );
                }
            }
        }
    }
    // Clip only the top corners. A taller logical rect places bottom corners
    // below this sparse band, so no client pixel is touched or copied.
    let radius = u32::from(chrome.frame.radius.min(chrome.frame.title_height));
    crate::modern::round_corners(
        &mut image,
        Rect::new(Point::new(0, 0), Size::new(w, h + radius * 2)),
        radius,
    );
    DecorationBuffer {
        width: w,
        height: h,
        pixels: image.take(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_title_reconstructs_every_original_pixel_including_corner_alpha() {
        let fonts = crate::FontState::new();
        for (name, _) in crate::modern::CHOICES {
            for appearance in [crate::Appearance::Light, crate::Appearance::Dark] {
                for scale in [1.0, 1.5, 2.0] {
                    let theme = crate::modern::theme(name, appearance)
                        .unwrap()
                        .scaled(scale);
                    for text in [
                        "Terminal",
                        "Живет 中文 العربية E\u{301}ditor 👩\u{200d}💻",
                        "",
                        "a\nb\nc",
                    ] {
                        for focused in [false, true] {
                            let request = DecorationRequest {
                                content_size: Size::new(400, 240),
                                title: text.into(),
                                focused,
                                resizable: true,
                                buttons: vec![wm_theme_api::ButtonRuntimeState {
                                    kind: ButtonKind::Close,
                                    hovered: true,
                                    pressed: true,
                                }],
                            };
                            let layout = layout(&theme, &request);
                            let visual = layout.visual_bounds();
                            let h = (layout.client_offset.y - visual.pos.y) as u32;
                            let original = title(
                                &theme,
                                &mut fonts.system(),
                                &mut fonts.swash(),
                                &request,
                                &layout,
                                visual.size.w,
                                h,
                            );
                            let (parts, solids) = split_title(
                                &original,
                                u32::from(Chrome::from_theme(&theme).frame.border),
                            );
                            assert!(parts.len() <= 4);
                            let surface = DecorationSurface {
                                frame_size: Size::new(original.width, original.height),
                                parts,
                                solids,
                                shadow: None,
                                shape: None,
                            };
                            assert_eq!(
                                crate::styles::system7::flatten(surface),
                                original,
                                "{name}/{scale}: {text}"
                            );
                        }
                    }
                }
            }
        }
    }
}
