//! Bounded capture chrome: one toolbar and one status strip, reused until their
//! visible content changes. Selection translation does not shape text, and
//! dimension changes never redraw the controls above the status strip.

use super::{Mode, Overlay};
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wm_theme::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use wm_theme::FontState;
use wm_theme_api::{DecorationBuffer, Size};

#[derive(Clone, Copy, PartialEq, Eq)]
struct ControlsKey {
    size: Size,
    mode: Mode,
    badge: bool,
    enabled: bool,
    hovered: Option<usize>,
    armed: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct StatusKey {
    size: Size,
    mode: Mode,
    badge: bool,
    selection: Option<Size>,
    seconds: u64,
    finishing: bool,
}

#[derive(Default)]
pub(super) struct Cache {
    controls: Option<(ControlsKey, MemoryRenderBuffer)>,
    status: Option<(StatusKey, MemoryRenderBuffer)>,
    camera: Option<(u32, MemoryRenderBuffer)>,
}

pub(super) fn hint_y(ui: &Overlay) -> i32 {
    if ui.badge {
        0
    } else {
        (ui.toolbar.size.h * 62 / 86) as i32
    }
}

impl Cache {
    pub(super) fn paint(
        &mut self,
        ui: &mut Overlay,
        fonts: &FontState,
        seconds: u64,
        finishing: bool,
    ) -> bool {
        let controls = ControlsKey {
            size: ui.toolbar.size,
            mode: ui.mode,
            badge: ui.badge,
            enabled: ui.selection.is_some_and(|r| r.size.w > 0 && r.size.h > 0),
            hovered: if ui.badge { None } else { ui.hovered },
            armed: if ui.badge { None } else { ui.armed },
        };
        let status = StatusKey {
            size: ui.toolbar.size,
            mode: ui.mode,
            badge: ui.badge,
            selection: ui.selection.map(|r| r.size),
            seconds: if ui.badge && !finishing { seconds } else { 0 },
            finishing,
        };
        let mut changed = ui.label.is_none() || ui.hint.is_none();
        if !self
            .controls
            .as_ref()
            .is_some_and(|(key, _)| *key == controls)
        {
            if let Some(pixels) = paint_controls(controls, fonts).and_then(import) {
                self.controls = Some((controls, pixels));
                changed = true;
            }
        }
        if !self.status.as_ref().is_some_and(|(key, _)| *key == status) {
            if let Some(pixels) = paint_status(status, fonts).and_then(import) {
                self.status = Some((status, pixels));
                changed = true;
            }
        }
        // Cloning these handles shares the backing pixels and uploaded texture.
        ui.label = self.controls.as_ref().map(|(_, buffer)| buffer.clone());
        ui.hint = self.status.as_ref().map(|(_, buffer)| buffer.clone());
        if ui.mode == Mode::Window && !ui.badge {
            if !self
                .camera
                .as_ref()
                .is_some_and(|(height, _)| *height == ui.toolbar.size.h)
            {
                self.camera = paint_camera(ui.toolbar.size.h as f32 / 86.0)
                    .and_then(import)
                    .map(|buffer| (ui.toolbar.size.h, buffer));
            }
            ui.camera = self.camera.as_ref().map(|(_, buffer)| buffer.clone());
        }
        changed
    }
}

// A small cached sprite, with the lens centered on the selection hotspot.
// Drawing the glyph ourselves keeps it legible without a particular icon font
// or cursor theme, and pointer motion only moves its uploaded buffer.
fn paint_camera(scale: f32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new((32.0 * scale).ceil() as u32, (30.0 * scale).ceil() as u32)?;
    let mut path = PathBuilder::new();
    path.move_to(4.0, 9.0);
    path.line_to(10.0, 9.0);
    path.line_to(12.0, 5.0);
    path.line_to(20.0, 5.0);
    path.line_to(22.0, 9.0);
    path.line_to(28.0, 9.0);
    path.line_to(28.0, 25.0);
    path.line_to(4.0, 25.0);
    path.close();
    path.push_circle(16.0, 16.0, 5.0);
    let path = path.finish()?;
    for (width, color) in [(4.5, [12, 16, 23, 255]), (2.0, [255, 255, 255, 255])] {
        let mut paint = Paint::default();
        paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            },
            Transform::from_scale(scale, scale),
            None,
        );
    }
    Some(pixmap)
}

fn import(pixmap: Pixmap) -> Option<MemoryRenderBuffer> {
    crate::backend_impl::import_buffer(
        &DecorationBuffer {
            width: pixmap.width(),
            height: pixmap.height(),
            pixels: pixmap.take(),
        },
        false,
    )
}

fn font(size: f32) -> FontSpec {
    FontSpec {
        family: "sans-serif".into(),
        size,
        weight: FontWeight::Normal,
        style: FontStyle::Normal,
    }
}

fn rounded(pixmap: &mut Pixmap, rect: [f32; 4], radius: f32, rgba: [u8; 4]) {
    let [x, y, w, h] = rect;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = radius.min(w / 2.0).min(h / 2.0);
    let mut path = PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.quad_to(x + w, y, x + w, y + r);
    path.line_to(x + w, y + h - r);
    path.quad_to(x + w, y + h, x + w - r, y + h);
    path.line_to(x + r, y + h);
    path.quad_to(x, y + h, x, y + h - r);
    path.line_to(x, y + r);
    path.quad_to(x, y, x + r, y);
    path.close();
    let Some(path) = path.finish() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn paint_controls(key: ControlsKey, fonts: &FontState) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(key.size.w, key.size.h)?;
    let scale = key.size.h as f32 / if key.badge { 42.0 } else { 86.0 };
    let w = key.size.w as f32;
    let h = key.size.h as f32;
    // An opaque inset and a one-pixel rim keep contrast over any wallpaper;
    // there is no backdrop blur, full-display readback or shadow texture.
    rounded(
        &mut pixmap,
        [0.0, 0.0, w, h],
        13.0 * scale,
        [79, 86, 98, 255],
    );
    rounded(
        &mut pixmap,
        [scale, scale, w - 2.0 * scale, h - 2.0 * scale],
        12.0 * scale,
        [27, 30, 36, 255],
    );
    if key.badge {
        return Some(pixmap);
    }

    let labels = [
        "Cancel",
        "Screen",
        "Window",
        "Area",
        "Record screen",
        "Record area",
        if key.mode.recording() {
            "Record"
        } else {
            "Capture"
        },
    ];
    let selected = match key.mode {
        Mode::Screen => 1,
        Mode::Window => 2,
        Mode::Area => 3,
        Mode::RecordScreen => 4,
        Mode::RecordArea => 5,
    };
    let cell = w / 7.0;
    // Compact displays reduce type/icon size with the control width, preserving
    // the output's device-pixel grid and every hit target.
    let control_scale = scale.min(cell / 94.0);
    let mut system = fonts.system();
    let mut swash = fonts.swash();
    let label_font = font(11.0 * control_scale);
    for (index, label) in labels.iter().enumerate() {
        let x = index as f32 * cell;
        let hovered = key.hovered == Some(index);
        let pressed = hovered && key.armed == Some(index);
        let color = if index == 6 && key.enabled {
            if key.mode.recording() {
                [178, 56, 68, 255]
            } else {
                [55, 111, 196, 255]
            }
        } else if index == selected {
            [53, 68, 88, 255]
        } else if pressed {
            [68, 74, 86, 255]
        } else if hovered {
            [46, 51, 61, 255]
        } else if index == 6 {
            [40, 45, 54, 255]
        } else {
            [0, 0, 0, 0]
        };
        let gutter = 4.0 * control_scale;
        if color[3] != 0 {
            rounded(
                &mut pixmap,
                [x + gutter, 7.0 * scale, cell - 2.0 * gutter, 50.0 * scale],
                8.0 * control_scale,
                color,
            );
        }
        let ink = if index == 6 && !key.enabled {
            Color::rgb(133, 141, 155)
        } else {
            Color::rgb(236, 240, 246)
        };
        if index == 6 {
            wm_theme::paint::draw_text(
                &mut pixmap,
                &mut system,
                &mut swash,
                label,
                &FontSpec {
                    size: 12.0 * control_scale,
                    weight: FontWeight::Bold,
                    ..label_font.clone()
                },
                ink,
                x as i32,
                (7.0 * scale) as i32,
                cell as u32,
                (50.0 * scale) as u32,
                TextAlign::Center,
            );
        } else {
            icon(
                &mut pixmap,
                index,
                x + cell / 2.0,
                23.0 * scale,
                control_scale,
            );
            wm_theme::paint::draw_text(
                &mut pixmap,
                &mut system,
                &mut swash,
                label,
                &label_font,
                ink,
                x as i32,
                (39.0 * scale) as i32,
                cell as u32,
                (13.0 * scale) as u32,
                TextAlign::Center,
            );
        }
    }
    Some(pixmap)
}

fn icon(pixmap: &mut Pixmap, index: usize, cx: f32, cy: f32, scale: f32) {
    let mut p = PathBuilder::new();
    if index == 0 {
        p.move_to(-5.0, -5.0);
        p.line_to(5.0, 5.0);
        p.move_to(5.0, -5.0);
        p.line_to(-5.0, 5.0);
    } else if index == 3 || index == 5 {
        for (x, y, dx, dy) in [
            (-10.0, -7.0, 1.0, 1.0),
            (10.0, -7.0, -1.0, 1.0),
            (-10.0, 7.0, 1.0, -1.0),
            (10.0, 7.0, -1.0, -1.0),
        ] {
            p.move_to(x + dx * 5.0, y);
            p.line_to(x, y);
            p.line_to(x, y + dy * 5.0);
        }
    } else {
        p.move_to(-10.0, -7.0);
        p.line_to(10.0, -7.0);
        p.line_to(10.0, 7.0);
        p.line_to(-10.0, 7.0);
        p.close();
        if index == 2 {
            p.move_to(-10.0, -2.0);
            p.line_to(10.0, -2.0);
        } else {
            p.move_to(0.0, 7.0);
            p.line_to(0.0, 10.0);
            p.move_to(-5.0, 10.0);
            p.line_to(5.0, 10.0);
        }
    }
    if let Some(path) = p.finish() {
        let mut paint = Paint::default();
        paint.set_color_rgba8(221, 229, 240, 255);
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width: 1.5,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            },
            Transform::from_scale(scale, scale).post_translate(cx, cy),
            None,
        );
    }
    if index == 4 || index == 5 {
        rounded(
            pixmap,
            [cx - 3.0 * scale, cy - 3.0 * scale, 6.0 * scale, 6.0 * scale],
            3.0 * scale,
            [241, 109, 116, 255],
        );
    }
}

fn paint_status(key: StatusKey, fonts: &FontState) -> Option<Pixmap> {
    let scale = key.size.h as f32 / if key.badge { 42.0 } else { 86.0 };
    let height = if key.badge {
        key.size.h
    } else {
        key.size.h - key.size.h * 62 / 86
    };
    let mut pixmap = Pixmap::new(key.size.w, height)?;
    let text = if key.badge {
        if key.finishing {
            "Finishing recording…".into()
        } else {
            format!(
                "{:02}:{:02}     Stop recording",
                key.seconds / 60,
                key.seconds % 60
            )
        }
    } else {
        let action = match key.mode {
            Mode::Screen => "Screen · Enter to capture",
            Mode::Window => "Click a window to capture · Enter also captures",
            Mode::Area => "Drag an area · Enter to capture",
            Mode::RecordScreen => "Record screen · Enter to start",
            Mode::RecordArea => "Drag an area · Enter to record",
        };
        match key.selection {
            Some(size) => format!(
                "{} × {} px   ·   {action}   ·   Esc to cancel",
                size.w, size.h
            ),
            None => format!("{action}   ·   Esc to cancel"),
        }
    };
    if key.badge && !key.finishing {
        rounded(
            &mut pixmap,
            [16.0 * scale, 17.0 * scale, 8.0 * scale, 8.0 * scale],
            2.0 * scale,
            [244, 112, 119, 255],
        );
    }
    let text_scale = scale.min(key.size.w as f32 / if key.badge { 280.0 } else { 640.0 });
    wm_theme::paint::draw_text(
        &mut pixmap,
        &mut fonts.system(),
        &mut fonts.swash(),
        &text,
        &font(if key.badge {
            12.0 * text_scale
        } else {
            10.5 * text_scale
        }),
        if key.badge {
            Color::rgb(250, 218, 220)
        } else {
            Color::rgb(163, 174, 191)
        },
        (8.0 * scale) as i32,
        0,
        key.size.w.saturating_sub((16.0 * scale) as u32),
        height.saturating_sub(if key.badge { 0 } else { (4.0 * scale) as u32 }),
        TextAlign::Center,
    );
    Some(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::Id;
    use wm_theme_api::{Point, Rect};

    fn overlay() -> Overlay {
        Overlay {
            mode: Mode::Area,
            quick: false,
            badge: false,
            monitor: Rect::new(Point::new(0, 0), Size::new(1280, 800)),
            toolbar: Rect::new(Point::new(280, 690), Size::new(720, 86)),
            selection: Some(Rect::new(Point::new(100, 100), Size::new(320, 240))),
            window: None,
            camera: None,
            armed_window: None,
            drag: None,
            move_selection: false,
            label: None,
            hint: None,
            dimming: super::super::dimming::Dimming::default(),
            ids: std::array::from_fn(|_| Id::new()),
            armed: None,
            hovered: None,
        }
    }

    #[test]
    fn warm_chrome_and_translated_selection_do_not_allocate_or_rasterize() {
        let fonts = FontState::new();
        let mut cache = Cache::default();
        let mut ui = overlay();
        assert!(cache.paint(&mut ui, &fonts, 0, false));
        let (_, allocations) = chonk_test_support::measure(|| {
            for x in 0..10_000 {
                ui.selection.as_mut().unwrap().pos.x = x;
                assert!(!cache.paint(&mut ui, &fonts, 0, false));
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
        // Reopening reuses both textures; only their compositor handles return.
        ui.label = None;
        ui.hint = None;
        let (changed, allocations) =
            chonk_test_support::measure(|| cache.paint(&mut ui, &fonts, 0, false));
        assert!(changed);
        assert_eq!(allocations.calls, 0);
    }

    #[test]
    fn status_follows_dimensions_and_recording_while_controls_remain_cached() {
        let fonts = FontState::new();
        let mut cache = Cache::default();
        let mut ui = overlay();
        cache.paint(&mut ui, &fonts, 0, false);
        let controls = cache.controls.as_ref().unwrap().0;
        ui.selection.as_mut().unwrap().size.w += 1;
        assert!(cache.paint(&mut ui, &fonts, 0, false));
        assert!(cache.controls.as_ref().unwrap().0 == controls);
        assert_eq!(
            cache.status.as_ref().unwrap().0.selection,
            Some(Size::new(321, 240))
        );
        ui.badge = true;
        ui.toolbar.size = Size::new(280, 42);
        assert!(cache.paint(&mut ui, &fonts, 0, false));
        assert!(cache.paint(&mut ui, &fonts, 1, false));
        assert!(cache.paint(&mut ui, &fonts, 1, true));
        assert!(!cache.paint(&mut ui, &fonts, 2, true));
    }

    #[test]
    fn toolbar_status_is_not_an_action_or_selection_target() {
        let ui = overlay();
        assert_eq!(
            super::super::toolbar_hit(&ui, Point::new(950, 716)),
            Some(6)
        );
        assert_eq!(super::super::toolbar_hit(&ui, Point::new(950, 755)), None);
        assert_eq!(super::super::toolbar_hit(&ui, Point::new(950, 692)), None);
    }
}
