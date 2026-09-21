//! BeOS R5's short title tab and five-pixel frame, measured from OS captures.
//! Source geometry/colors and compatible typography are documented separately.
use std::collections::VecDeque;

mod buttons;
pub(crate) mod text;
pub(crate) mod ui;
use crate::{
    beos::{INK, PANEL, WHITE, YELLOW},
    model::Color,
    FontState,
};
use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationPart, DecorationRequest,
    DecorationSurface, Point, Rect, Size,
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
    let border = m.px(5);
    let title = if edges { 0 } else { m.px(19) };
    let size = Size::new(client.w + border * 2, client.h + title + border * 2);
    let tab = m
        .px(text::width(&request.title, true) + if request.resizable { 72 } else { 54 })
        .min(size.w);
    let mut result = DecorationLayout {
        frame_size: size,
        input_margin: 0,
        input_exclusion: (title > 0 && tab < size.w)
            .then(|| Rect::new(Point::new(tab as i32, 0), Size::new(size.w - tab, title))),
        client_offset: Point::new(border as i32, (title + border) as i32),
        titlebar_height: title,
        button_hitboxes: Vec::with_capacity(if title > 0 { 2 } else { 0 }),
        resize_hitboxes: Vec::new(),
        shaded_frame_height: if edges { size.h } else { title + border * 2 },
    };
    if title > 0 && tab >= m.px(22) {
        result.button_hitboxes.push((
            ButtonKind::Close,
            Rect::new(
                Point::new(m.px(4) as i32, m.px(4) as i32),
                Size::new(m.px(14), m.px(14)),
            ),
        ));
        if request.resizable && tab >= m.px(42) {
            result.button_hitboxes.push((
                ButtonKind::Maximize,
                Rect::new(
                    Point::new((tab - m.px(18)) as i32, m.px(4) as i32),
                    Size::new(m.px(14), m.px(14)),
                ),
            ));
        }
    }
    if request.resizable {
        // The upper resize edge belongs to the body, below the tab. It must
        // never steal controls or turn the empty desktop into a resize handle.
        let body = Size::new(size.w, size.h - title);
        result.resize_hitboxes = super::edge_ring(body, border, border, border, border, m.px(22))
            .into_iter()
            .map(|(edge, mut rect)| {
                rect.pos.y += title as i32;
                (edge, rect)
            })
            .collect();
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
    let title = layout.titlebar_height.min(h);
    let border = m.px(5).min(w / 2).min(h.saturating_sub(title) / 2);
    let body = if request.focused {
        PANEL
    } else {
        Color::rgb(232, 232, 232)
    };
    let dark = if request.focused {
        Color::rgb(136, 136, 136)
    } else {
        Color::rgb(152, 152, 152)
    };
    let tones = [
        Color::rgb(152, 152, 152),
        WHITE,
        body,
        dark,
        Color::rgb(152, 152, 152),
    ];
    let mut top = empty(w, border);
    let tab = layout
        .input_exclusion
        .map_or(w, |rect| rect.pos.x.max(0) as u32);
    let mut parts = Vec::with_capacity(5);
    if title > 0 {
        let mut key = request.clone();
        key.content_size.h = 0;
        key.resizable = layout
            .button_hitboxes
            .iter()
            .any(|(kind, _)| *kind == ButtonKind::Maximize);
        for button in &mut key.buttons {
            button.hovered = false;
        }
        let buffer = if let Some((_, _, buffer)) = cache
            .iter()
            .find(|(scale, cached, _)| *scale == m.scale.to_bits() && cached == &key)
        {
            buffer.clone()
        } else {
            let buffer = paint_tab(request, layout, fonts, m, tab, title, body);
            cache.push_back((m.scale.to_bits(), key, buffer.clone()));
            while cache.len() > 16 {
                cache.pop_front();
            }
            buffer
        };
        parts.push(DecorationPart {
            offset: Point::new(0, 0),
            buffer,
        });
    }
    for dy in 0..border {
        let src = ((dy as f32 / m.scale) as usize).min(4);
        fill(&mut top, 0, dy, w, 1, tones[src]);
        if title > 0 && src <= 1 {
            fill(
                &mut top,
                m.px(2),
                dy,
                tab.saturating_sub(m.px(3)),
                1,
                if src == 0 {
                    if request.focused {
                        YELLOW
                    } else {
                        body
                    }
                } else {
                    body
                },
            );
        }
    }
    let mut bottom = empty(w, border);
    for dy in 0..border {
        let src = ((dy as f32 / m.scale) as usize).min(4);
        fill(
            &mut bottom,
            0,
            dy,
            w,
            1,
            if src == 4 {
                Color::rgb(96, 96, 96)
            } else {
                tones[src]
            },
        );
    }
    // Side columns carry through the top and bottom joins, like the original.
    for (right, x) in [(false, 0), (true, w.saturating_sub(border))] {
        let mut side = empty(border, h.saturating_sub(title + border * 2));
        for dx in 0..border {
            let src = ((dx as f32 / m.scale) as usize).min(4);
            let color = if right && src == 4 {
                Color::rgb(96, 96, 96)
            } else {
                tones[src]
            };
            fill(&mut side, dx, 0, 1, h, color);
            fill(&mut top, x + dx, 0, 1, border, color);
            fill(&mut bottom, x + dx, 0, 1, border, color);
        }
        if side.height > 0 {
            parts.push(DecorationPart {
                offset: Point::new(x as i32, (title + border) as i32),
                buffer: side,
            });
        }
    }
    parts.insert(
        usize::from(title > 0),
        DecorationPart {
            offset: Point::new(0, title as i32),
            buffer: top,
        },
    );
    if border > 0 {
        parts.push(DecorationPart {
            offset: Point::new(0, (h - border) as i32),
            buffer: bottom,
        });
    }
    DecorationSurface {
        frame_size: layout.frame_size,
        parts,
        solids: Vec::new(),
        shadow: None,
        shape: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_tab(
    request: &DecorationRequest,
    layout: &DecorationLayout,
    fonts: &FontState,
    m: Metrics,
    tab: u32,
    title: u32,
    body: Color,
) -> DecorationBuffer {
    let mut top = empty(tab, title);
    fill(
        &mut top,
        0,
        0,
        tab,
        title,
        if request.focused { YELLOW } else { body },
    );
    fill(&mut top, 0, 0, tab, m.px(1), Color::rgb(152, 152, 152));
    fill(&mut top, 0, 0, m.px(1), title, Color::rgb(152, 152, 152));
    let light = if request.focused {
        Color::rgb(252, 252, 100)
    } else {
        WHITE
    };
    fill(
        &mut top,
        m.px(1),
        m.px(1),
        tab.saturating_sub(m.px(2)),
        m.px(1),
        light,
    );
    fill(
        &mut top,
        m.px(1),
        m.px(1),
        m.px(1),
        title.saturating_sub(m.px(1)),
        light,
    );
    fill(
        &mut top,
        tab.saturating_sub(m.px(2)),
        m.px(2),
        m.px(1),
        title.saturating_sub(m.px(2)),
        if request.focused {
            Color::rgb(200, 152, 0)
        } else {
            Color::rgb(152, 152, 152)
        },
    );
    fill(
        &mut top,
        tab.saturating_sub(m.px(1)),
        m.px(1),
        m.px(1),
        title.saturating_sub(m.px(1)),
        Color::rgb(96, 96, 96),
    );
    for &(kind, rect) in &layout.button_hitboxes {
        let pressed = request.buttons.iter().any(|b| b.kind == kind && b.pressed);
        control(
            &mut top,
            rect,
            kind == ButtonKind::Maximize,
            request.focused,
            pressed,
            m,
        );
    }
    let x = m.px(36);
    let right = layout
        .button_hitboxes
        .iter()
        .find(|(kind, _)| *kind == ButtonKind::Maximize)
        .map_or(tab.saturating_sub(m.px(10)), |(_, r)| {
            (r.pos.x as u32).saturating_sub(m.px(8))
        });
    text::draw(
        &mut top,
        fonts,
        &request.title,
        Rect::new(
            Point::new(x as i32, 0),
            Size::new(right.saturating_sub(x), title),
        ),
        if request.focused {
            INK
        } else {
            Color::rgb(80, 80, 80)
        },
        true,
        m,
    );
    top
}

pub(super) fn control(
    image: &mut DecorationBuffer,
    rect: Rect,
    zoom: bool,
    active: bool,
    pressed: bool,
    m: Metrics,
) {
    use buttons::*;
    let (pixels, colors): (&[u8], &[Color]) = match (active, zoom) {
        (true, false) => (&CLOSE, &CLOSE_COLORS),
        (true, true) => (&ZOOM, &ZOOM_COLORS),
        (false, false) => (&INACTIVE_CLOSE, &INACTIVE_CLOSE_COLORS),
        (false, true) => (&INACTIVE_ZOOM, &INACTIVE_ZOOM_COLORS),
    };
    for dy in 0..rect.size.h {
        for dx in 0..rect.size.w {
            let sx = ((dx as f32 / m.scale) as usize).min(13);
            let sy = ((dy as f32 / m.scale) as usize).min(13);
            let index = pixels[sy * 14 + sx] as usize;
            // A depressed control reverses the bevel, keeping the two zoom
            // rectangles in their original orientation. Pressed-state relief
            // is an adaptation; the unpressed bitmap is capture-verified.
            let color = if pressed && index == 0 {
                colors[colors.len() - 1]
            } else if pressed && index == colors.len() - 1 {
                colors[0]
            } else {
                colors[index]
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
