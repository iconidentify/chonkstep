//! Warp 4 Presentation Manager frame. Measurements: docs/decoration-styles/os2warp.md.
use std::collections::VecDeque;
mod buttons;
pub(crate) mod text;
pub(crate) mod ui;
use crate::{
    model::Color,
    os2warp::{INK, PANEL, SHADE, TITLE, WHITE},
    FontState,
};
use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationPart, DecorationRequest,
    DecorationSurface, Point, Rect, ResizeEdge, Size,
};

#[derive(Clone, Copy)]
pub(crate) struct Metrics {
    pub(super) scale: f32,
}
impl Metrics {
    pub(crate) fn new(scale: f32) -> Self {
        Self {
            scale: crate::raster::normalized_scale(scale).clamp(0.5, 8.0),
        }
    }
    pub(super) fn px(self, n: u32) -> u32 {
        (n as f32 * self.scale + 0.5).floor() as u32
    }
}

pub(crate) fn layout(request: &DecorationRequest, scale: f32, edges: bool) -> DecorationLayout {
    let m = Metrics::new(scale);
    let client = wm_theme_api::clamp_client_size(
        request.content_size,
        Size::new(
            wm_theme_api::MAX_CLIENT_WINDOW_DIMENSION,
            wm_theme_api::MAX_CLIENT_WINDOW_DIMENSION,
        ),
    );
    let b = m.px(4);
    let top = if edges { b } else { m.px(24) };
    let size = Size::new(client.w + b * 2, client.h + top + b);
    let mut buttons = Vec::with_capacity(if edges { 0 } else { 4 });
    if !edges {
        // Reserve one 18px cell per control; discard the least essential
        // controls first, without ever overlapping the left icon or resize ring.
        let count = if request.resizable { 3 } else { 2 };
        for (i, kind) in [
            ButtonKind::Close,
            ButtonKind::Miniaturize,
            ButtonKind::Maximize,
        ]
        .into_iter()
        .take(count)
        .enumerate()
        {
            let from_right = m.px(20 + 18 * (count - 1 - i) as u32);
            if size.w >= from_right + m.px(22) {
                buttons.push((
                    kind,
                    Rect::new(
                        Point::new((size.w - from_right) as i32, m.px(7) as i32),
                        Size::new(m.px(14), m.px(14)),
                    ),
                ));
            }
        }
        // A very narrow window still exposes Close whenever one cell fits.
        if !buttons.iter().any(|(kind, _)| *kind == ButtonKind::Close) {
            buttons.clear();
            if size.w >= m.px(22) {
                buttons.push((
                    ButtonKind::Close,
                    Rect::new(
                        Point::new((size.w - m.px(20)) as i32, m.px(7) as i32),
                        Size::new(m.px(14), m.px(14)),
                    ),
                ));
            }
        }
    }
    if !edges && buttons.iter().all(|(_, r)| r.pos.x >= m.px(22) as i32) && size.w >= m.px(42) {
        buttons.push((
            ButtonKind::Menu,
            Rect::new(
                Point::new(m.px(4) as i32, m.px(5) as i32),
                Size::new(m.px(18), m.px(18)),
            ),
        ));
    }
    DecorationLayout {
        frame_size: size,
        input_margin: 0,
        input_exclusion: None,
        client_offset: Point::new(b as i32, top as i32),
        titlebar_height: if edges { 0 } else { top },
        button_hitboxes: buttons,
        resize_hitboxes: if request.resizable {
            resize_ring(size, b)
        } else {
            Vec::new()
        },
        shaded_frame_height: if edges { size.h } else { top + b },
    }
}

fn resize_ring(size: Size, b: u32) -> Vec<(ResizeEdge, Rect)> {
    let (w, h) = (size.w, size.h);
    let mut result = Vec::with_capacity(8);
    for (edge, x, y, width, height) in [
        (ResizeEdge::NorthWest, 0, 0, b, b),
        (ResizeEdge::NorthEast, w - b, 0, b, b),
        (ResizeEdge::SouthWest, 0, h - b, b, b),
        (ResizeEdge::SouthEast, w - b, h - b, b, b),
        (ResizeEdge::North, b, 0, w - b * 2, b),
        (ResizeEdge::South, b, h - b, w - b * 2, b),
        (ResizeEdge::West, 0, b, b, h - b * 2),
        (ResizeEdge::East, w - b, b, b, h - b * 2),
    ] {
        if width > 0 && height > 0 {
            result.push((
                edge,
                Rect::new(Point::new(x as i32, y as i32), Size::new(width, height)),
            ));
        }
    }
    result
}

pub(crate) fn render(
    request: &DecorationRequest,
    layout: &DecorationLayout,
    scale: f32,
    fonts: &FontState,
    cache: &mut VecDeque<(u32, DecorationRequest, DecorationBuffer)>,
) -> DecorationSurface {
    let m = Metrics::new(scale);
    let (w, h) = (layout.frame_size.w, layout.frame_size.h);
    let b = m.px(4).min(w / 2).min(h / 2);
    let top_h = if layout.titlebar_height == 0 {
        b
    } else {
        layout.titlebar_height.min(h - b)
    };
    let mut key = request.clone();
    // Height does not change title pixels. The visible control set does:
    // narrow resizable frames can have fewer controls than fixed-size ones.
    key.content_size.h = layout.button_hitboxes.iter().fold(0, |mask, (kind, _)| {
        mask | match kind {
            ButtonKind::Close => 1,
            ButtonKind::Miniaturize => 2,
            ButtonKind::Maximize => 4,
            ButtonKind::Menu => 8,
        }
    });
    key.resizable = layout
        .button_hitboxes
        .iter()
        .any(|(kind, _)| *kind == ButtonKind::Maximize);
    for button in &mut key.buttons {
        button.hovered = false;
    }
    let top = if let Some((_, _, buffer)) = cache.iter().find(|(s, cached, buffer)| {
        *s == m.scale.to_bits() && cached == &key && buffer.height == top_h
    }) {
        buffer.clone()
    } else {
        let mut top = empty(w, top_h);
        fill(&mut top, 0, 0, w, top_h, PANEL);
        fill(&mut top, 0, 0, w, m.px(1), SHADE);
        fill(
            &mut top,
            m.px(1),
            m.px(1),
            w.saturating_sub(m.px(3)),
            m.px(1),
            WHITE,
        );
        if layout.titlebar_height > 0 && !request.focused {
            dither(
                &mut top,
                Rect::new(
                    Point::new(m.px(2) as i32, m.px(2) as i32),
                    Size::new(w.saturating_sub(m.px(4)), top_h.saturating_sub(m.px(2))),
                ),
                m,
                0,
            );
        }
        if layout.titlebar_height > 0 {
            fill(
                &mut top,
                m.px(4),
                top_h.saturating_sub(m.px(1)),
                w.saturating_sub(m.px(8)),
                m.px(1),
                PANEL,
            );
        }
        side_columns(&mut top, m, w, top_h, true);
        if layout.titlebar_height > 0 {
            title(&mut top, request, layout, fonts, m);
        }
        cache.push_back((m.scale.to_bits(), key, top.clone()));
        while cache.len() > 16 {
            cache.pop_front();
        }
        top
    };
    let mut parts = Vec::with_capacity(4);
    parts.push(DecorationPart {
        offset: Point::new(0, 0),
        buffer: top,
    });
    let left = [SHADE, WHITE, PANEL, PANEL];
    let right = [PANEL, PANEL, SHADE, INK];
    for (x, colors) in [(0, left), (w - b, right)] {
        let mut side = empty(b, h.saturating_sub(top_h + b));
        for dx in 0..b {
            fill(
                &mut side,
                dx,
                0,
                1,
                h,
                colors[((dx as f32 / m.scale) as usize).min(3)],
            );
        }
        if side.height > 0 {
            parts.push(DecorationPart {
                offset: Point::new(x as i32, top_h as i32),
                buffer: side,
            });
        }
    }
    let mut bottom = empty(w, b);
    for dy in 0..b {
        fill(
            &mut bottom,
            0,
            dy,
            w,
            1,
            right[((dy as f32 / m.scale) as usize).min(3)],
        );
    }
    fill(&mut bottom, 0, 0, m.px(1), b, SHADE);
    fill(
        &mut bottom,
        m.px(1),
        0,
        m.px(1),
        b.saturating_sub(m.px(1)),
        WHITE,
    );
    fill(
        &mut bottom,
        w.saturating_sub(m.px(2)),
        0,
        m.px(1),
        b.saturating_sub(m.px(1)),
        SHADE,
    );
    fill(&mut bottom, w.saturating_sub(m.px(1)), 0, m.px(1), b, INK);
    parts.push(DecorationPart {
        offset: Point::new(0, (h - b) as i32),
        buffer: bottom,
    });
    DecorationSurface {
        frame_size: layout.frame_size,
        parts,
        solids: Vec::new(),
        shadow: None,
        shape: None,
    }
}
fn side_columns(image: &mut DecorationBuffer, m: Metrics, w: u32, h: u32, top: bool) {
    let line = m.px(1);
    fill(image, 0, 0, line, h, SHADE);
    fill(image, line, if top { line } else { 0 }, line, h, WHITE);
    fill(image, w.saturating_sub(line * 2), line, line, h, SHADE);
    fill(image, w.saturating_sub(line), 0, line, h, INK);
}
fn title(
    image: &mut DecorationBuffer,
    request: &DecorationRequest,
    layout: &DecorationLayout,
    fonts: &FontState,
    m: Metrics,
) {
    let w = image.width;
    let right = layout
        .button_hitboxes
        .iter()
        .filter(|(kind, _)| *kind != ButtonKind::Menu)
        .map(|(_, r)| r.pos.x.max(0) as u32)
        .min()
        .unwrap_or(w.saturating_sub(m.px(4)));
    let x = m.px(22);
    if right > x + m.px(2) {
        let rect = Rect::new(
            Point::new(x as i32, m.px(5) as i32),
            Size::new(right - x - m.px(2), m.px(18)),
        );
        if request.focused {
            caption(image, rect, true, m);
        } else {
            dither(image, rect, m, 0);
        }
        text::draw(
            image,
            fonts,
            &request.title,
            Rect::new(
                Point::new(m.px(32) as i32, m.px(5) as i32),
                Size::new(right.saturating_sub(m.px(38)), m.px(16)),
            ),
            if request.focused { WHITE } else { PANEL },
            true,
            m,
        );
    }
    for &(kind, rect) in &layout.button_hitboxes {
        if kind != ButtonKind::Menu {
            let inset = m.px(2);
            fill(
                image,
                (rect.pos.x.max(0) as u32).saturating_sub(inset),
                (rect.pos.y.max(0) as u32).saturating_sub(inset),
                m.px(18),
                m.px(18),
                PANEL,
            );
        }
        control(
            image,
            rect,
            kind,
            request.buttons.iter().any(|b| b.kind == kind && b.pressed),
            m,
        );
    }
}

pub(super) fn caption(image: &mut DecorationBuffer, rect: Rect, active: bool, m: Metrics) {
    let (x, y, w, h) = (
        rect.pos.x.max(0) as u32,
        rect.pos.y.max(0) as u32,
        rect.size.w,
        rect.size.h,
    );
    let line = m.px(1).min(w / 2).min(h / 2);
    if line == 0 {
        return;
    }
    fill(image, x, y, w, h, SHADE);
    fill(image, x + line, y + line, w - line, h - line, WHITE);
    fill(image, x + w - line, y, line, h, WHITE);
    fill(
        image,
        x + line,
        y + line,
        w - line * 2,
        h - line * 2,
        if active { TITLE } else { SHADE },
    );
    if !active {
        dither(
            image,
            Rect::new(
                Point::new((x + line) as i32, (y + line) as i32),
                Size::new(w - line * 2, h - line * 2),
            ),
            m,
            0,
        );
    }
}

fn dither(image: &mut DecorationBuffer, rect: Rect, m: Metrics, phase: u32) {
    for dy in 0..rect.size.h {
        for dx in 0..rect.size.w {
            let color = if (((dx as f32 / m.scale) as u32) + ((dy as f32 / m.scale) as u32) + phase)
                .is_multiple_of(2)
            {
                Color::rgb(125, 125, 125)
            } else {
                SHADE
            };
            fill(
                image,
                rect.pos.x.max(0) as u32 + dx,
                rect.pos.y.max(0) as u32 + dy,
                1,
                1,
                color,
            );
        }
    }
}
pub(super) fn control(
    image: &mut DecorationBuffer,
    rect: Rect,
    kind: ButtonKind,
    pressed: bool,
    m: Metrics,
) {
    if kind == ButtonKind::Menu {
        fill(
            image,
            rect.pos.x.max(0) as u32,
            rect.pos.y.max(0) as u32,
            rect.size.w,
            rect.size.h,
            PANEL,
        );
        document(
            image,
            rect.pos.x.max(0) as u32 + m.px(1),
            rect.pos.y.max(0) as u32 + m.px(1),
            m,
        );
        if pressed {
            fill(
                image,
                rect.pos.x.max(0) as u32,
                rect.pos.y.max(0) as u32,
                rect.size.w,
                m.px(1),
                SHADE,
            );
            fill(
                image,
                rect.pos.x.max(0) as u32,
                rect.pos.y.max(0) as u32,
                m.px(1),
                rect.size.h,
                SHADE,
            );
        }
        return;
    }
    let pixels = match kind {
        ButtonKind::Menu => unreachable!(),
        ButtonKind::Close => &buttons::CLOSE,
        ButtonKind::Miniaturize => &buttons::MINIMIZE,
        ButtonKind::Maximize => &buttons::MAXIMIZE,
    };
    for y in 0..rect.size.h {
        for x in 0..rect.size.w {
            let (sx, sy) = (
                ((x as f32 / m.scale) as usize).min(13),
                ((y as f32 / m.scale) as usize).min(13),
            );
            let mut index = pixels[sy * 14 + sx] as usize;
            if pressed {
                index = match index {
                    1 => 2,
                    2 => 1,
                    other => other,
                };
            }
            fill(
                image,
                rect.pos.x.max(0) as u32 + x,
                rect.pos.y.max(0) as u32 + y,
                1,
                1,
                buttons::COLORS[index],
            );
        }
    }
}

pub(super) fn document(image: &mut DecorationBuffer, x: u32, y: u32, m: Metrics) {
    // A neutral Workplace Shell document for clients without a supplied icon.
    // Its isometric fold is an original adaptation, not an application logo.
    let rows = [
        "....kkkkkkkk....",
        "....kwwwwwwkk...",
        "....kwggggwkwk..",
        "....kwwwwwwkkkk.",
        "....kwggggwwwwk.",
        "....kwwwwwwwwwk.",
        "....kwggggggwwk.",
        "....kwwwwwwwwwk.",
        "....kwggggggwwk.",
        "....kwwwwwwwwwk.",
        "....kwggggggwwk.",
        "....kwwwwwwwwwk.",
        "....kwggggwwwwk.",
        "....kwwwwwwwwwk.",
        "....kkkkkkkkkkk.",
        "................",
    ];
    sprite(image, x, y, &rows, m);
}
pub(super) fn sprite(image: &mut DecorationBuffer, x: u32, y: u32, rows: &[&str], m: Metrics) {
    for (dy, row) in rows.iter().enumerate() {
        for (dx, c) in row.bytes().enumerate() {
            let color = match c {
                b'k' => INK,
                b'w' => WHITE,
                b'g' => SHADE,
                b'p' => PANEL,
                b'y' => Color::rgb(255, 255, 0),
                b'd' => Color::rgb(130, 130, 0),
                _ => continue,
            };
            fill(
                image,
                x + m.px(dx as u32),
                y + m.px(dy as u32),
                m.px(dx as u32 + 1) - m.px(dx as u32),
                m.px(dy as u32 + 1) - m.px(dy as u32),
                color,
            );
        }
    }
}
pub(super) fn empty(width: u32, height: u32) -> DecorationBuffer {
    DecorationBuffer {
        width,
        height,
        pixels: vec![0; width as usize * height as usize * 4],
    }
}
pub(super) fn fill(
    image: &mut DecorationBuffer,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    c: Color,
) {
    let right = x.saturating_add(width).min(image.width);
    if x >= right {
        return;
    }
    for y in y..y.saturating_add(height).min(image.height) {
        let start = ((y * image.width + x) * 4) as usize;
        let end = ((y * image.width + right) * 4) as usize;
        for pixel in image.pixels[start..end].as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[c.r, c.g, c.b, c.a]);
        }
    }
}
