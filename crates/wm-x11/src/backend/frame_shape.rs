//! Native Shape windows keep transparent input margins usable without requiring
//! an X compositing manager. The ring is InputOnly and immediately below its
//! frame, so higher windows occlude both input and pixels consistently.
use super::*;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};

pub(super) struct FrameShape {
    pub geometry: Rect,
    pub margin: u32,
    pub corner: u32,
    pub mapped: bool,
    pub ring: Option<Window>,
    applied: Option<(Size, u32, u32)>,
    ring_applied: Option<(Rect, u32, bool)>,
}

impl FrameShape {
    pub fn new(layout: &DecorationLayout) -> Self {
        Self { geometry: Rect::new(Point::new(0, 0), layout.frame_size), margin: layout.input_margin,
            corner: 0, mapped: false, ring: None, applied: None, ring_applied: None }
    }
}

impl X11Backend {
    /// Shell panels with the flat style's offset shadow have two transparent
    /// corners. Native X11 needs an explicit bounding shape; without it the
    /// server paints those RGBA-zero pixels black. Ordinary opaque dock paints
    /// take a constant-time corner check and allocate nothing here.
    pub(super) fn blit_shell(&mut self, window: Window, buffer: &DecorationBuffer) {
        if let Some(key @ (size, shadow)) = shell_shadow(buffer) {
            if self.shell_shapes.get(&window) != Some(&key) {
                let rectangles = [rectangle(0, 0, size.w - shadow, size.h - shadow),
                    rectangle(shadow, shadow, size.w - shadow, size.h - shadow)];
                let _ = self.conn.shape_rectangles(SO::SET, SK::BOUNDING, ClipOrdering::UNSORTED,
                    window, 0, 0, &rectangles);
                self.shell_shapes.insert(window, key);
            }
        } else if self.shell_shapes.remove(&window).is_some() {
            let _ = self.conn.shape_mask(SO::SET, SK::BOUNDING, window, 0, 0, NONE);
        }
        self.blit(window, buffer);
    }

    pub(super) fn event_frame(&self, window: Window) -> Window {
        self.ring_to_frame.get(&window).copied().unwrap_or(window)
    }

    pub(super) fn remove_frame_shape(&mut self, frame: Window) {
        if let Some(ring) = self.frame_shapes.remove(&frame).and_then(|shape| shape.ring) {
            self.ring_to_frame.remove(&ring);
            let _ = self.conn.destroy_window(ring);
        }
    }

    pub(super) fn stack_frame_ring(&self, frame: Window) {
        if let Some(ring) = self.frame_shapes.get(&frame).and_then(|shape| shape.ring) {
            let _ = self.conn.configure_window(ring, &ConfigureWindowAux::new().sibling(frame).stack_mode(StackMode::BELOW));
        }
    }

    pub(super) fn update_frame_shape_pixels(&mut self, frame: Window, surface: &wm_theme_api::DecorationSurface) {
        if let Some(shape) = self.frame_shapes.get_mut(&frame) {
            // The two transparent outside corners are replicated square
            // blocks. Measure the top band's trailing transparent run once
            // per paint; all other bands and the client remain within shape.
            shape.corner = surface.parts.first().map_or(0, |part| {
                part.buffer.pixels.get(..part.buffer.width as usize * 4).unwrap_or_default().as_chunks::<4>().0.iter()
                    .rev().take_while(|pixel| pixel[3] == 0).count() as u32
            });
        }
        self.sync_frame_shape(frame);
    }

    pub(super) fn sync_frame_shape(&mut self, frame: Window) {
        let Some(shape) = self.frame_shapes.get_mut(&frame) else { return; };
        let geometry = shape.geometry;
        let key = (geometry.size, shape.margin, shape.corner);
        if shape.applied != Some(key) {
            // Ordinary WindowMaker frames keep the server's default shape.
            if shape.margin == 0 && shape.corner == 0 {
                if shape.applied.is_some_and(|(_, margin, corner)| margin != 0 || corner != 0) {
                    let _ = self.conn.shape_mask(SO::SET, SK::BOUNDING, frame, 0, 0, NONE);
                }
            } else {
                let rectangles = visible_shape(geometry.size, shape.margin, shape.corner);
                let _ = self.conn.shape_rectangles(SO::SET, SK::BOUNDING, ClipOrdering::UNSORTED, frame, 0, 0, &rectangles);
            }
            shape.applied = Some(key);
        }
        if shape.margin == 0 {
            if let Some(ring) = shape.ring.take() {
                self.ring_to_frame.remove(&ring);
                let _ = self.conn.destroy_window(ring);
                shape.ring_applied = None;
            }
            return;
        }
        if shape.ring.is_none() {
            let Ok(ring) = self.conn.generate_id() else { return; };
            let cursor = self.cursors.for_edge(self.frame_cursor.get(&frame).copied().flatten());
            let aux = CreateWindowAux::new().override_redirect(1).cursor(cursor).event_mask(
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION | EventMask::ENTER_WINDOW);
            if self.conn.create_window(0, ring, self.root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &aux).is_err() { return; }
            shape.ring = Some(ring);
            self.ring_to_frame.insert(ring, frame);
        }
        if let Some(ring) = shape.ring {
            let ring_key = (geometry, shape.margin, shape.mapped);
            if shape.ring_applied == Some(ring_key) { return; }
            let _ = self.conn.configure_window(ring, &ConfigureWindowAux::new()
                .x(geometry.pos.x).y(geometry.pos.y).width(geometry.size.w).height(geometry.size.h)
                .sibling(frame).stack_mode(StackMode::BELOW));
            // InputOnly windows use their bounding shape for delivery. The
            // visible band receives events from the real frame, exclusively.
            let rectangles = input_ring(geometry.size, shape.margin);
            let _ = self.conn.shape_rectangles(SO::SET, SK::BOUNDING, ClipOrdering::UNSORTED, ring, 0, 0, &rectangles);
            if shape.mapped { let _ = self.conn.map_window(ring); } else { let _ = self.conn.unmap_window(ring); }
            shape.ring_applied = Some(ring_key);
        }
    }
}

fn shell_shadow(buffer: &DecorationBuffer) -> Option<(Size, u32)> {
    let (w, h) = (buffer.width as usize, buffer.height as usize);
    if w < 2 || h < 2 || w.checked_mul(h)?.checked_mul(4)? != buffer.pixels.len() { return None; }
    let alpha = |x, y| buffer.pixels[(y * w + x) * 4 + 3];
    if alpha(w - 1, 0) != 0 || alpha(0, h - 1) != 0 || alpha(0, 0) != 255 || alpha(w - 1, h - 1) != 255 {
        return None;
    }
    let shadow = (0..w).rev().take_while(|&x| alpha(x, 0) == 0).count();
    if shadow == 0 || shadow >= w || shadow >= h { return None; }
    // Accept only the exact binary silhouette. Arbitrary translucent shell
    // widgets keep their existing X11 behavior; no bounding-box approximation.
    for y in 0..h {
        for x in 0..w {
            let opaque = (x < w - shadow && y < h - shadow) || (x >= shadow && y >= shadow);
            if alpha(x, y) != if opaque { 255 } else { 0 } { return None; }
        }
    }
    Some((Size::new(buffer.width, buffer.height), shadow as u32))
}

fn rectangle(x: u32, y: u32, w: u32, h: u32) -> Rectangle {
    Rectangle { x: x.min(i16::MAX as u32) as i16, y: y.min(i16::MAX as u32) as i16,
        width: w.min(u16::MAX as u32) as u16, height: h.min(u16::MAX as u32) as u16 }
}

fn visible_shape(size: Size, margin: u32, corner: u32) -> [Rectangle; 2] {
    let margin = margin.min(size.w / 2).min(size.h / 2);
    let w = size.w - margin * 2;
    let h = size.h - margin * 2;
    let corner = corner.min(w).min(h);
    [rectangle(margin, margin, w - corner, h - corner),
        rectangle(margin + corner, margin + corner, w - corner, h - corner)]
}

fn input_ring(size: Size, margin: u32) -> [Rectangle; 4] {
    let margin = margin.min(size.w / 2).min(size.h / 2);
    [rectangle(0, 0, size.w, margin), rectangle(0, size.h - margin, size.w, margin),
        rectangle(0, margin, margin, size.h - margin * 2),
        rectangle(size.w - margin, margin, margin, size.h - margin * 2)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};
    use wm_theme_api::{DecorationRequest, ThemeEngine};

    #[test]
    #[ignore = "private X server only: xvfb-run -a cargo test -p wm-x11 native_system7 -- --ignored --test-threads=1"]
    fn native_system7_menu_shadow_shape_input_and_toggle_cleanup() {
        let mut backend = X11Backend::connect_and_become_wm(None, 1.0).unwrap();
        let fonts = FontState::new();
        let base = wm_theme::default_theme::nextstep_classic();
        let items = [wm_theme::menu::MenuItem::Action { label: "Terminal".into(), action: 7 }];
        for scale in [1.0, 2.0] {
            let theme = base.scaled(scale);
            let chrome = wm_theme::UiChrome::new(&theme, fonts.clone(), DecorationStyle::System7, scale);
            let menu = chrome.menu(&theme, &mut fonts.system(), "Applications", &items, Some(0), true);
            let size = Size::new(menu.buffer.width, menu.buffer.height);
            let popup = backend.create_shell_surface(Rect::new(Point::new(100, 100), size), (25, 70, 90), true).unwrap();
            backend.paint_shell_surface(popup, &menu.buffer);
            backend.map_shell_surface(popup);
            let actual = backend.conn.shape_get_rectangles(popup, SK::BOUNDING).unwrap().reply().unwrap();
            for y in 0..size.h {
                for x in 0..size.w {
                    let inside = actual.rectangles.iter().any(|rect| Rect::new(Point::new(rect.x as i32, rect.y as i32),
                        Size::new(rect.width as u32, rect.height as u32)).contains(Point::new(x as i32, y as i32)));
                    assert_eq!(inside, menu.buffer.pixels[((y * size.w + x) * 4 + 3) as usize] != 0);
                }
            }
            backend.conn.warp_pointer(NONE, backend.root, 0, 0, 0, 0, (100 + size.w - 1) as i16, 100).unwrap().check().unwrap();
            assert_eq!(backend.conn.query_pointer(backend.root).unwrap().reply().unwrap().child, NONE,
                "transparent shadow corner must expose the underlying desktop to input too");
            assert_eq!(backend.shell_shapes.len(), 1);
            // Exercise the PopupHost paint path independently of Backend.
            let wm = wm_theme::menu::render_menu(&theme, &mut fonts.system(), "Applications", &items, None, true);
            backend.configure_shell_surface(popup, Rect::new(Point::new(100, 100), Size::new(wm.buffer.width, wm.buffer.height)));
            wm_theme_api::PopupHost::paint_popup(&mut backend, popup, &wm.buffer);
            assert!(!backend.conn.shape_query_extents(popup).unwrap().reply().unwrap().bounding_shaped);
            assert!(backend.shell_shapes.is_empty());
            wm_theme_api::PopupHost::paint_popup(&mut backend, popup, &menu.buffer);
            assert_eq!(backend.shell_shapes.len(), 1);
            backend.destroy_shell_surface(popup);
            assert!(backend.shell_shapes.is_empty());
            assert!(!backend.painted.contains_key(&popup));
        }
    }

    #[test]
    #[ignore = "private X server only: xvfb-run -a cargo test -p wm-x11 native_system7 -- --ignored"]
    fn native_system7_shape_input_stacking_and_toggle_cleanup() {
        let mut backend = X11Backend::connect_and_become_wm(None, 1.0).unwrap();
        let fonts = FontState::new();
        let engine = RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), fonts.clone())
            .with_style(DecorationStyle::System7).unwrap();
        let wm = RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), fonts);
        let new_client = |backend: &X11Backend| {
            let id = backend.conn.generate_id().unwrap();
            backend.conn.create_window(COPY_DEPTH_FROM_PARENT, id, backend.root, 0, 0, 240, 120, 0,
                WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap().check().unwrap();
            XWindow(id)
        };
        let pointer_child = |backend: &X11Backend, x: i16, y: i16| {
            backend.conn.warp_pointer(NONE, backend.root, 0, 0, 0, 0, x, y).unwrap().check().unwrap();
            backend.conn.query_pointer(backend.root).unwrap().reply().unwrap().child
        };
        for scale in [1.0, 2.0] {
            let request = DecorationRequest { content_size: Size::new(240, 120), title: "Terminal".into(),
                focused: true, resizable: true, buttons: Vec::new() };
            let layout = engine.layout_at(&request, scale);
            let client = new_client(&backend);
            let frame = backend.create_decoration(client, &layout);
            backend.set_frame_geometry(frame, Rect::new(Point::new(100, 100), layout.frame_size));
            backend.paint_decoration(frame, &engine.render_surface_at(&request, &layout, scale));
            backend.map_frame(frame);
            let ring = backend.frame_shapes[&frame.0].ring.unwrap();
            let actual = backend.conn.shape_get_rectangles(frame.0, SK::BOUNDING).unwrap().reply().unwrap();
            let contains = |x, y| actual.rectangles.iter().any(|r| Rect::new(Point::new(r.x as i32, r.y as i32),
                Size::new(r.width as u32, r.height as u32)).contains(Point::new(x, y)));
            let visible = layout.visual_bounds();
            assert!(contains(visible.pos.x, visible.pos.y));
            assert!(!contains(visible.pos.x + visible.size.w as i32 - 1, visible.pos.y));
            assert!(!contains(visible.pos.x, visible.pos.y + visible.size.h as i32 - 1));
            assert!(!contains(0, 50));
            assert_eq!(pointer_child(&backend, 101, 170), ring, "real server input must reach the external ring");
            assert!(matches!(backend.translate_button(ring, NONE, 1, 70, 1, true, 100, 0),
                Some(BackendEvent::PointerButton { surface: SurfaceRef::Frame(id), local: Point { x: 1, y: 70 }, .. }) if id == frame));

            let higher_client = new_client(&backend);
            let higher_layout = wm.layout(&request);
            let higher = backend.create_decoration(higher_client, &higher_layout);
            backend.set_frame_geometry(higher, Rect::new(Point::new(90, 160), higher_layout.frame_size));
            backend.map_frame(higher);
            backend.raise(higher);
            assert_eq!(pointer_child(&backend, 101, 170), higher.0, "lower ring cannot steal a higher titlebar click");
            backend.raise(frame);
            assert_eq!(pointer_child(&backend, 101, 170), ring);
            backend.restack(&[frame, higher]);
            assert_eq!(pointer_child(&backend, 101, 170), higher.0);
            backend.unmap_frame(higher);
            backend.unmap_frame(frame);
            assert_eq!(pointer_child(&backend, 101, 170), NONE, "unmapped windows leave no input ghost");
            backend.map_frame(frame);
            assert_eq!(pointer_child(&backend, 101, 170), ring);

            let mut fullscreen = layout.clone();
            fullscreen.frame_size = Size::new(640, 480);
            fullscreen.client_offset = Point::new(0, 0);
            fullscreen.input_margin = 0;
            fullscreen.titlebar_height = 0;
            fullscreen.button_hitboxes.clear();
            fullscreen.resize_hitboxes.clear();
            backend.set_decoration_layout(frame, &fullscreen);
            backend.set_frame_geometry(frame, Rect::new(Point::new(0, 0), fullscreen.frame_size));
            assert!(!backend.conn.shape_query_extents(frame.0).unwrap().reply().unwrap().bounding_shaped,
                "fullscreen clears both transparent corners and external input");
            assert!(backend.conn.get_geometry(ring).unwrap().reply().is_err());
            backend.set_decoration_layout(frame, &layout);
            backend.set_frame_geometry(frame, Rect::new(Point::new(100, 100), layout.frame_size));
            backend.paint_decoration(frame, &engine.render_surface_at(&request, &layout, scale));
            let ring = backend.frame_shapes[&frame.0].ring.unwrap();
            assert_eq!(pointer_child(&backend, 101, 170), ring, "restoring System 7 recreates its input ring");

            backend.set_decoration_layout(frame, &higher_layout);
            backend.set_frame_geometry(frame, Rect::new(Point::new(100, 100), higher_layout.frame_size));
            backend.position_client(client, higher_layout.client_offset);
            backend.paint_decoration(frame, &wm.render_surface(&request, &higher_layout));
            assert!(!backend.ring_to_frame.contains_key(&ring));
            assert!(backend.conn.get_geometry(ring).unwrap().reply().is_err());
            let bounds = backend.conn.shape_query_extents(frame.0).unwrap().reply().unwrap();
            assert!(!bounds.bounding_shaped, "WindowMaker restores the default native shape");
            backend.destroy_decoration(frame);
            backend.destroy_decoration(higher);
            assert!(backend.frame_shapes.is_empty());
            assert!(backend.ring_to_frame.is_empty());
        }
    }
}
