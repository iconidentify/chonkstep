//! Native Wayland client transport. At most two submitted buffers per surface;
//! a busy compositor coalesces paints into one pending frame.
use crate::surface::{Backend, DragHandle, Role};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::AsFd;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat,
    wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_popup, xdg_positioner, xdg_surface, xdg_wm_base};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1 as top, zwlr_foreign_toplevel_manager_v1 as manager,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as ls, zwlr_layer_surface_v1 as layer,
};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::client::Event;

pub struct Output {
    pub handle: wl_output::WlOutput,
    pub name: String,
    width: u32,
    height: u32,
    scale: i32,
    rotated: bool,
    logical: Option<Size>,
}

pub struct Window {
    pub handle: top::ZwlrForeignToplevelHandleV1,
    pub app_id: String,
    pub title: String,
    pub minimized: bool,
    pub ready: bool,
}

struct Surface {
    handle: wl_surface::WlSurface,
    layer: Option<layer::ZwlrLayerSurfaceV1>,
    popup: Option<(xdg_surface::XdgSurface, xdg_popup::XdgPopup, Point)>,
    viewport: Option<wp_viewport::WpViewport>,
    role: Role,
    geometry: Rect,
    mapped: bool,
    configured: bool,
    height: u32,
    pending: Option<DecorationBuffer>,
    // Files are retained until release; submitted bytes are never overwritten.
    buffers: BTreeMap<u32, (wl_buffer::WlBuffer, std::fs::File)>,
}

pub struct Wayland {
    qh: QueueHandle<Self>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<ls::ZwlrLayerShellV1>,
    output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    backdrop: Option<u32>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    last_press: Option<(u32, std::time::Instant)>,
    pub outputs: BTreeMap<u32, Output>,
    output: Option<u32>,
    pub seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    surfaces: BTreeMap<u32, Surface>,
    next_surface: u32,
    next_buffer: u32,
    pointer_at: Option<(u32, Point)>,
    pub windows: BTreeMap<u32, Window>,
    pub events: Vec<Event>,
    pub closed: bool,
    pub changed: bool,
    pub windows_changed: bool,
    keyboard_panels: bool,
}

impl Wayland {
    pub fn connect(
        output_name: Option<&str>,
    ) -> Result<(Connection, wayland_client::EventQueue<Self>, Self), Box<dyn std::error::Error>>
    {
        let connection = Connection::connect_to_env()?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let mut app = Self {
            qh: qh.clone(),
            compositor: None,
            shm: None,
            shell: None,
            output_manager: None,
            viewporter: None,
            backdrop: None,
            wm_base: None,
            last_press: None,
            outputs: BTreeMap::new(),
            output: None,
            seat: None,
            pointer: None,
            keyboard: None,
            surfaces: BTreeMap::new(),
            next_surface: 1,
            next_buffer: 1,
            pointer_at: None,
            windows: BTreeMap::new(),
            events: Vec::new(),
            closed: false,
            changed: false,
            windows_changed: false,
            keyboard_panels: false,
        };
        connection.display().get_registry(&qh, ());
        queue.roundtrip(&mut app)?;
        if let Some(manager) = &app.output_manager {
            for (&id, output) in &app.outputs {
                manager.get_xdg_output(&output.handle, &qh, id);
            }
        }
        queue.roundtrip(&mut app)?;
        if app.compositor.is_none()
            || app.shm.is_none()
            || app.shell.is_none()
            || app.wm_base.is_none()
        {
            return Err(
                "the compositor must support wl_compositor, wl_shm and wlr-layer-shell".into(),
            );
        }
        app.output = app
            .outputs
            .iter()
            .find(|(_, o)| output_name.is_none_or(|name| o.name == name))
            .map(|(id, _)| *id);
        if app.output.is_none() {
            return Err("requested output was not found".into());
        }
        Ok((connection, queue, app))
    }
    pub fn scale(&self) -> i32 {
        self.output
            .and_then(|id| self.outputs.get(&id))
            .map_or(1, |o| o.scale.max(1))
    }
    pub fn screen(&self) -> Rect {
        let size = self
            .output
            .and_then(|id| self.outputs.get(&id))
            .map(|o| {
                if let Some(logical) = o.logical {
                    Size::new(logical.w * o.scale as u32, logical.h * o.scale as u32)
                } else if o.rotated {
                    Size::new(o.height, o.width)
                } else {
                    Size::new(o.width, o.height)
                }
            })
            .unwrap_or(Size::new(1280, 720));
        Rect {
            pos: Point::new(0, 0),
            size,
        }
    }
    pub fn geometry(&self, id: u32) -> Option<Rect> {
        self.surfaces.get(&id).map(|s| s.geometry)
    }
    pub fn running(&self) -> Vec<(String, u32)> {
        self.windows
            .iter()
            .filter(|(_, w)| w.ready)
            .map(|(id, w)| (w.app_id.clone(), *id))
            .collect()
    }
    pub fn activate(&self, id: u32) {
        if let (Some(window), Some(seat)) = (self.windows.get(&id), self.seat.as_ref()) {
            window.handle.unset_minimized();
            window.handle.activate(seat);
        }
    }
    fn configure(&self, id: u32) {
        let scale = self.scale();
        let Some(surface) = self.surfaces.get(&id) else {
            return;
        };
        let Some(layer) = &surface.layer else { return };
        let rect = surface.geometry;
        if surface.role == Role::Backdrop {
            surface.handle.set_buffer_scale(1);
            layer.set_anchor(
                layer::Anchor::Top
                    | layer::Anchor::Right
                    | layer::Anchor::Bottom
                    | layer::Anchor::Left,
            );
            layer.set_size(0, 0);
            layer.set_exclusive_zone(-1);
        } else if surface.role == Role::Dock {
            layer.set_anchor(layer::Anchor::Top | layer::Anchor::Right | layer::Anchor::Bottom);
            layer.set_size(rect.size.w.div_ceil(scale as u32), 0);
            layer.set_exclusive_zone(rect.size.w.div_ceil(scale as u32) as i32);
            layer.set_margin(0, 0, 0, 0);
        } else {
            layer.set_anchor(layer::Anchor::Top | layer::Anchor::Left);
            layer.set_size(
                rect.size.w.div_ceil(scale as u32),
                rect.size.h.div_ceil(scale as u32),
            );
            layer.set_exclusive_zone(-1);
            layer.set_margin(rect.pos.y / scale, 0, 0, rect.pos.x / scale);
        }
        layer.set_keyboard_interactivity(if surface.role == Role::Dock && self.keyboard_panels {
            layer::KeyboardInteractivity::Exclusive
        } else {
            layer::KeyboardInteractivity::None
        });
    }
    pub fn present(&mut self) -> std::io::Result<()> {
        let scale = self.scale() as u32;
        let shm = self.shm.as_ref().expect("bound shm");
        for (&id, surface) in &mut self.surfaces {
            if !surface.mapped || !surface.configured || surface.buffers.len() >= 2 {
                continue;
            }
            let Some(frame) = surface.pending.take() else {
                continue;
            };
            let width = if surface.role == Role::Backdrop {
                1
            } else {
                frame.width.div_ceil(scale) * scale
            };
            let height = if surface.role == Role::Backdrop {
                1
            } else if surface.role == Role::Dock {
                surface.height.max(scale)
            } else {
                frame.height.div_ceil(scale) * scale
            };
            if width == 0 || width > 8192 || height > 16384 {
                continue;
            }
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for y in 0..frame.height.min(height) {
                for x in 0..frame.width.min(width) {
                    let src = ((y * frame.width + x) * 4) as usize;
                    let dst = ((y * width + x) * 4) as usize;
                    // DecorationBuffer is premultiplied RGBA; wl_shm uses native ARGB.
                    let argb = u32::from_be_bytes([
                        frame.pixels[src + 3],
                        frame.pixels[src],
                        frame.pixels[src + 1],
                        frame.pixels[src + 2],
                    ]);
                    pixels[dst..dst + 4].copy_from_slice(&argb.to_ne_bytes());
                }
            }
            let mut file = tempfile::tempfile()?;
            file.write_all(&pixels)?;
            let pool = shm.create_pool(file.as_fd(), pixels.len() as i32, &self.qh, ());
            let generation = self.next_buffer;
            self.next_buffer = self.next_buffer.wrapping_add(1);
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                (width * 4) as i32,
                wl_shm::Format::Argb8888,
                &self.qh,
                (id, generation),
            );
            pool.destroy();
            if surface.role != Role::Backdrop {
                let input = self
                    .compositor
                    .as_ref()
                    .unwrap()
                    .create_region(&self.qh, ());
                input.add(
                    0,
                    0,
                    frame.width.div_ceil(scale) as i32,
                    frame.height.min(height).div_ceil(scale) as i32,
                );
                surface.handle.set_input_region(Some(&input));
                input.destroy();
            }
            surface
                .handle
                .set_buffer_scale(if surface.role == Role::Backdrop {
                    1
                } else {
                    scale as i32
                });
            surface.handle.attach(Some(&buffer), 0, 0);
            surface
                .handle
                .damage_buffer(0, 0, width as i32, height as i32);
            surface.handle.commit();
            surface.buffers.insert(generation, (buffer, file));
        }
        Ok(())
    }
}

impl Backend for Wayland {
    type ShellId = u32;
    type WindowId = u32;
    fn create_shell_surface(&mut self, geometry: Rect, _: (u8, u8, u8), _: bool) -> Option<u32> {
        let id = self.next_surface;
        self.next_surface += 1;
        let handle = self.compositor.as_ref()?.create_surface(&self.qh, id);
        self.surfaces.insert(
            id,
            Surface {
                handle,
                layer: None,
                popup: None,
                viewport: None,
                role: Role::Panel,
                geometry,
                mapped: false,
                configured: false,
                height: geometry.size.h,
                pending: None,
                buffers: BTreeMap::new(),
            },
        );
        Some(id)
    }
    fn set_role(&mut self, id: u32, role: Role) {
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.role = role;
        }
    }
    fn map_shell_surface(&mut self, id: u32) {
        if self.surfaces.get(&id).is_none_or(|s| s.mapped) {
            return;
        }
        if self.surfaces[&id].role == Role::Panel {
            let parent = self
                .surfaces
                .iter()
                .rev()
                .find(|(_, s)| s.mapped && s.popup.is_some())
                .or_else(|| {
                    self.surfaces
                        .iter()
                        .find(|(_, s)| s.role == Role::Dock && s.mapped)
                });
            let Some((&parent_id, parent)) = parent else {
                return;
            };
            let parent_xdg = parent.popup.as_ref().map(|(xdg, _, _)| xdg.clone());
            let parent_origin = parent.geometry.pos;
            let base = self.wm_base.as_ref().unwrap();
            let rect = self.surfaces[&id].geometry;
            let positioner = self.positioner(rect, parent_origin);
            let xdg = base.get_xdg_surface(&self.surfaces[&id].handle, &self.qh, id);
            let popup = xdg.get_popup(parent_xdg.as_ref(), &positioner, &self.qh, id);
            if parent_xdg.is_none() {
                self.surfaces[&parent_id]
                    .layer
                    .as_ref()
                    .unwrap()
                    .get_popup(&popup);
            }
            if let (Some(seat), Some((serial, at))) = (&self.seat, self.last_press) {
                if at.elapsed() < std::time::Duration::from_secs(1) {
                    popup.grab(seat, serial);
                }
            }
            positioner.destroy();
            let surface = self.surfaces.get_mut(&id).unwrap();
            surface.popup = Some((xdg, popup, parent_origin));
            surface.mapped = true;
            surface.configured = false;
            let scale = self.scale();
            self.surfaces[&id].handle.set_buffer_scale(scale);
            self.surfaces[&id].handle.commit();
            return;
        }
        let surface = self.surfaces.get_mut(&id).unwrap();
        let output = self
            .output
            .and_then(|id| self.outputs.get(&id))
            .map(|o| &o.handle);
        let namespace = match surface.role {
            Role::Dock => "chonk-dock",
            Role::Clip => "chonk-dock-clip",
            Role::Launcher => "chonk-dock-launcher",
            Role::Icon => "chonk-dock-icon",
            Role::Backdrop => "chonk-dock-dismiss",
            Role::Panel => unreachable!(),
        };
        surface.layer = Some(self.shell.as_ref().unwrap().get_layer_surface(
            &surface.handle,
            output,
            ls::Layer::Top,
            namespace.into(),
            &self.qh,
            id,
        ));
        if surface.role == Role::Backdrop {
            surface.viewport = self
                .viewporter
                .as_ref()
                .map(|vp| vp.get_viewport(&surface.handle, &self.qh, ()));
        }
        surface.mapped = true;
        surface.configured = false;
        self.configure(id);
        self.surfaces[&id].handle.commit();
    }
    fn unmap_shell_surface(&mut self, id: u32) {
        if let Some(surface) = self.surfaces.get_mut(&id) {
            if let Some((xdg, popup, _)) = surface.popup.take() {
                popup.destroy();
                xdg.destroy();
            }
            if let Some(layer) = surface.layer.take() {
                layer.destroy();
            }
            if let Some(viewport) = surface.viewport.take() {
                viewport.destroy();
            }
            surface.handle.destroy();
            // A wl_surface role lasts for that wl_surface's lifetime.
            // Reopening therefore gets a fresh surface, never a reused role.
            surface.handle = self
                .compositor
                .as_ref()
                .unwrap()
                .create_surface(&self.qh, id);
            surface.mapped = false;
            surface.configured = false;
        }
    }
    fn configure_shell_surface(&mut self, id: u32, geometry: Rect) {
        if let Some(surface) = self.surfaces.get_mut(&id) {
            if surface.geometry == geometry {
                return;
            }
            surface.geometry = geometry;
        }
        if let Some((_, popup, origin)) = self.surfaces.get(&id).and_then(|s| s.popup.as_ref()) {
            let positioner = self.positioner(geometry, *origin);
            if popup.version() >= 3 {
                popup.reposition(&positioner, 0);
            }
            positioner.destroy();
        } else {
            self.configure(id);
        }
        if let Some(surface) = self.surfaces.get(&id) {
            surface.handle.commit();
        }
    }
    fn paint_shell_surface(&mut self, id: u32, pixels: &DecorationBuffer) {
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.pending = Some(pixels.clone());
        }
    }
    fn release_shell_buffer(&mut self, id: u32) {
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.pending = None;
        }
    }
    fn destroy_shell_surface(&mut self, id: u32) {
        if let Some(surface) = self.surfaces.remove(&id) {
            if let Some((xdg, popup, _)) = surface.popup {
                popup.destroy();
                xdg.destroy();
            }
            if let Some(layer) = surface.layer {
                layer.destroy();
            }
            if let Some(viewport) = surface.viewport {
                viewport.destroy();
            }
            surface.handle.destroy();
            for (_, (buffer, _)) in surface.buffers {
                buffer.destroy();
            }
        }
    }
    // Layer ordering belongs to the compositor; implicit pointer grabs cover
    // every press-through-release drag without a client-side global grab.
    fn raise_shell_surface(&mut self, _: u32) {}
    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        DragHandle(0)
    }
    fn ungrab_pointer(&mut self, _: DragHandle) {}
    fn panel_keyboard(&mut self, enabled: bool) {
        self.keyboard_panels = enabled;
        for &id in self.surfaces.keys() {
            self.configure(id);
            self.surfaces[&id].handle.commit();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Wayland {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "zxdg_output_manager_v1" => {
                    app.output_manager = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "wp_viewporter" => app.viewporter = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    app.shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_output" => {
                    app.outputs.insert(
                        name,
                        Output {
                            handle: registry.bind(name, version.min(4), qh, name),
                            name: String::new(),
                            width: 1280,
                            height: 720,
                            scale: 1,
                            rotated: false,
                            logical: None,
                        },
                    );
                }
                "wl_seat" if app.seat.is_none() => {
                    app.seat = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "zwlr_foreign_toplevel_manager_v1" => {
                    let _: manager::ZwlrForeignToplevelManagerV1 =
                        registry.bind(name, version.min(3), qh, ());
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(output) = app.outputs.remove(&name) {
                    if output.handle.version() >= 3 {
                        output.handle.release();
                    }
                }
                if app.output == Some(name) {
                    app.closed = true;
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_output::WlOutput, u32> for Wayland {
    fn event(
        app: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = app.outputs.get_mut(id) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                output.width = (width as u32).clamp(1, 16384);
                output.height = (height as u32).clamp(1, 16384);
            }
            wl_output::Event::Scale { factor } => output.scale = factor.clamp(1, 8),
            wl_output::Event::Geometry {
                transform: WEnum::Value(transform),
                ..
            } => {
                output.rotated = matches!(
                    transform,
                    wl_output::Transform::_90
                        | wl_output::Transform::_270
                        | wl_output::Transform::Flipped90
                        | wl_output::Transform::Flipped270
                );
            }
            wl_output::Event::Done => app.changed = true,
            _ => {}
        }
    }
}
impl Dispatch<layer::ZwlrLayerSurfaceV1, u32> for Wayland {
    fn event(
        app: &mut Self,
        layer: &layer::ZwlrLayerSurfaceV1,
        event: layer::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let scale = app.scale() as u32;
        let Some(surface) = app.surfaces.get_mut(id) else {
            return;
        };
        // A configure queued before an unmap must not configure the replacement.
        if surface.layer.as_ref() != Some(layer) {
            return;
        }
        match event {
            layer::Event::Configure { serial, height, .. } => {
                layer.ack_configure(serial);
                surface.configured = true;
                surface.height = height.saturating_mul(scale).min(16384);
            }
            layer::Event::Closed => app.closed = true,
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, (u32, u32)> for Wayland {
    fn event(
        app: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        &(id, generation): &(u32, u32),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(surface) = app.surfaces.get_mut(&id) {
            surface.buffers.remove(&generation);
        }
        buffer.destroy();
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Wayland {
    fn event(
        app: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && app.pointer.is_none() {
                app.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && app.keyboard.is_none() {
                app.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if !caps.contains(wl_seat::Capability::Pointer) {
                if let Some(pointer) = app.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release();
                    }
                }
                app.pointer_at = None;
            }
            if !caps.contains(wl_seat::Capability::Keyboard) {
                if let Some(keyboard) = app.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release();
                    }
                }
            }
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Wayland {
    fn event(
        app: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let scale = app.scale() as f64;
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                if let Some((&id, _)) = app.surfaces.iter().find(|(_, s)| s.handle == surface) {
                    let point = Point::new((surface_x * scale) as i32, (surface_y * scale) as i32);
                    app.pointer_at = Some((id, point));
                    app.events.push(Event::Motion(id, point));
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if let Some((id, point)) = &mut app.pointer_at {
                    *point = Point::new((surface_x * scale) as i32, (surface_y * scale) as i32);
                    app.events.push(Event::Motion(*id, *point));
                }
            }
            wl_pointer::Event::Leave { .. } => {
                app.pointer_at = None;
                app.events.push(Event::Leave);
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(state),
                ..
            } => {
                if state == wl_pointer::ButtonState::Pressed {
                    app.last_press = Some((serial, std::time::Instant::now()));
                }
                if let Some((id, point)) = app.pointer_at {
                    app.events.push(Event::Button(
                        id,
                        point,
                        button,
                        state == wl_pointer::ButtonState::Pressed,
                    ));
                }
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
                value,
                ..
            } => {
                if let Some((id, point)) = app.pointer_at {
                    app.events
                        .push(Event::Scroll(id, point, if value < 0.0 { 1 } else { -1 }));
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Wayland {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Escape's evdev key code is independent of the textual keymap.
        if matches!(
            event,
            wl_keyboard::Event::Key {
                key: 1,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            }
        ) {
            app.events.push(Event::Escape);
        }
    }
}
impl Dispatch<manager::ZwlrForeignToplevelManagerV1, ()> for Wayland {
    fn event(
        app: &mut Self,
        _: &manager::ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let manager::Event::Toplevel { toplevel } = event {
            app.windows.insert(
                toplevel.id().protocol_id(),
                Window {
                    handle: toplevel,
                    app_id: String::new(),
                    title: String::new(),
                    minimized: false,
                    ready: false,
                },
            );
        }
    }
    wayland_client::event_created_child!(Wayland, manager::ZwlrForeignToplevelManagerV1, [manager::EVT_TOPLEVEL_OPCODE => (top::ZwlrForeignToplevelHandleV1, ())]);
}
impl Dispatch<top::ZwlrForeignToplevelHandleV1, ()> for Wayland {
    fn event(
        app: &mut Self,
        handle: &top::ZwlrForeignToplevelHandleV1,
        event: top::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id().protocol_id();
        let Some(window) = app.windows.get_mut(&id) else {
            return;
        };
        match event {
            top::Event::AppId { app_id } => window.app_id = app_id.chars().take(4096).collect(),
            top::Event::Title { title } => window.title = title.chars().take(4096).collect(),
            top::Event::State { state } => {
                window.minimized = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|bytes| u32::from_ne_bytes(*bytes) == top::State::Minimized as u32)
            }
            top::Event::Done => {
                window.ready = true;
                app.windows_changed = true;
            }
            top::Event::Closed => {
                app.windows.remove(&id);
                handle.destroy();
                app.windows_changed = true;
            }
            _ => {}
        }
    }
}
macro_rules! ignore {
    ($($ty:ty => $data:ty),* $(,)?) => {$(impl Dispatch<$ty,$data> for Wayland {
        fn event(_: &mut Self, _: &$ty, _: <$ty as Proxy>::Event, _: &$data, _: &Connection, _: &QueueHandle<Self>) {}
    })*};
}
ignore!(wl_compositor::WlCompositor => (), wl_shm::WlShm => (), wl_shm_pool::WlShmPool => (), wl_region::WlRegion => (), wl_surface::WlSurface => u32, ls::ZwlrLayerShellV1 => ());

impl wm_theme_api::PopupHost for Wayland {
    type PopupId = u32;
    fn create_popup(&mut self, rect: Rect, bg: (u8, u8, u8)) -> Option<u32> {
        let id = self.create_shell_surface(rect, bg, true)?;
        self.map_shell_surface(id);
        Some(id)
    }
    fn destroy_popup(&mut self, id: u32) {
        self.destroy_shell_surface(id);
    }
    fn paint_popup(&mut self, id: u32, buffer: &DecorationBuffer) {
        self.paint_shell_surface(id, buffer);
    }
    fn grab_pointer(&mut self) -> wm_theme_api::PopupGrab {
        wm_theme_api::PopupGrab(0)
    }
    fn ungrab_pointer(&mut self, _: wm_theme_api::PopupGrab) {}
    fn grab_keyboard(&mut self) {
        self.panel_keyboard(true);
    }
    fn ungrab_keyboard(&mut self) {
        self.panel_keyboard(false);
    }
}

impl Wayland {
    fn positioner(&self, rect: Rect, origin: Point) -> xdg_positioner::XdgPositioner {
        let scale = self.scale();
        let positioner = self
            .wm_base
            .as_ref()
            .unwrap()
            .create_positioner(&self.qh, ());
        positioner.set_size(
            rect.size.w.div_ceil(scale as u32) as i32,
            rect.size.h.div_ceil(scale as u32) as i32,
        );
        positioner.set_anchor_rect(
            (rect.pos.x - origin.x) / scale,
            (rect.pos.y - origin.y) / scale,
            1,
            1,
        );
        positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            xdg_positioner::ConstraintAdjustment::SlideX
                | xdg_positioner::ConstraintAdjustment::SlideY,
        );
        positioner
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Wayland {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, u32> for Wayland {
    fn event(
        app: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            if let Some(surface) = app.surfaces.get_mut(id).filter(|s| {
                s.popup
                    .as_ref()
                    .is_some_and(|(current, _, _)| current == xdg)
            }) {
                xdg.ack_configure(serial);
                surface.configured = true;
            }
        }
    }
}
impl Dispatch<xdg_popup::XdgPopup, u32> for Wayland {
    fn event(
        app: &mut Self,
        popup: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let scale = app.scale();
        let Some(surface) = app.surfaces.get_mut(id).filter(|s| {
            s.popup
                .as_ref()
                .is_some_and(|(_, current, _)| current == popup)
        }) else {
            return;
        };
        match event {
            xdg_popup::Event::PopupDone => app.events.push(Event::Dismiss(*id)),
            xdg_popup::Event::Configure { x, y, .. } => {
                let origin = surface.popup.as_ref().unwrap().2;
                surface.geometry.pos = Point::new(origin.x + x * scale, origin.y + y * scale);
            }
            _ => {}
        }
    }
}
ignore!(xdg_positioner::XdgPositioner => ());

impl Wayland {
    pub fn is_backdrop(&self, id: u32) -> bool {
        self.backdrop == Some(id)
    }
    /// A one-pixel transparent outside-click surface, stretched by the
    /// viewporter. ChonkStep currently declines xdg_popup.grab, so the client
    /// provides dismissal without a screen-sized allocation or compositor code.
    pub fn sync_backdrop(&mut self) {
        let visible = self
            .surfaces
            .values()
            .any(|s| s.mapped && s.popup.is_some());
        if !visible {
            if let Some(id) = self.backdrop.take() {
                self.destroy_shell_surface(id);
            }
            return;
        }
        if self.viewporter.is_none() {
            return;
        } // Other hosts can grant the popup grab.
        let screen = self.screen();
        let scale = self.scale();
        let id = match self.backdrop {
            Some(id) => id,
            None => {
                let Some(id) = self.create_shell_surface(screen, (0, 0, 0), true) else {
                    return;
                };
                self.set_role(id, Role::Backdrop);
                self.map_shell_surface(id);
                self.paint_shell_surface(
                    id,
                    &DecorationBuffer {
                        width: 1,
                        height: 1,
                        pixels: vec![0; 4],
                    },
                );
                self.backdrop = Some(id);
                id
            }
        };
        let surface = &self.surfaces[&id];
        if let Some(viewport) = &surface.viewport {
            viewport.set_destination(
                screen.size.w.div_ceil(scale as u32) as i32,
                screen.size.h.div_ceil(scale as u32) as i32,
            );
        }
        let region = self
            .compositor
            .as_ref()
            .unwrap()
            .create_region(&self.qh, ());
        region.add(
            0,
            0,
            screen.size.w.div_ceil(scale as u32) as i32,
            screen.size.h.div_ceil(scale as u32) as i32,
        );
        for (&other, s) in &self.surfaces {
            if other != id && s.mapped {
                region.subtract(
                    s.geometry.pos.x / scale,
                    s.geometry.pos.y / scale,
                    s.geometry.size.w.div_ceil(scale as u32) as i32,
                    s.geometry.size.h.div_ceil(scale as u32) as i32,
                );
            }
        }
        surface.handle.set_input_region(Some(&region));
        region.destroy();
        surface.handle.commit();
    }
}
ignore!(wp_viewport::WpViewport => (), wp_viewporter::WpViewporter => ());

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, u32> for Wayland {
    fn event(
        app: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = app.outputs.get_mut(id) else {
            return;
        };
        if let zxdg_output_v1::Event::LogicalSize { width, height } = event {
            output.logical = Some(Size::new(
                width.clamp(1, 16384) as u32,
                height.clamp(1, 16384) as u32,
            ));
            app.changed = true;
        }
    }
}
ignore!(zxdg_output_manager_v1::ZxdgOutputManagerV1 => ());
