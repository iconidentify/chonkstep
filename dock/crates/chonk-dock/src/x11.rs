//! Ordinary X11 dock client: EWMH windows/struts, RandR outputs, and X11 input.
//! This connection never selects SubstructureRedirect or acts as a WM.
use crate::{
    client::Event,
    surface::{Backend, DragHandle, Role},
};
use std::{collections::BTreeMap, os::fd::AsRawFd};
use wm_theme_api::{DecorationBuffer, Point, PopupGrab, PopupHost, Rect, Size};
use x11rb::{
    connection::{Connection, RequestConnection},
    protocol::{
        randr::{self, ConnectionExt as _},
        xproto::*,
        Event as XEvent,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING, WM_STATE, WM_PROTOCOLS, WM_DELETE_WINDOW,
        _NET_WM_NAME, _NET_WM_PID, _NET_WM_WINDOW_TYPE, _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_POPUP_MENU, _NET_WM_STATE, _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_STICKY, _NET_WM_STATE_SKIP_TASKBAR, _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_HIDDEN, _NET_WM_STRUT, _NET_WM_STRUT_PARTIAL, _NET_WM_DESKTOP,
        _NET_CLIENT_LIST, _NET_ACTIVE_WINDOW, _NET_CURRENT_DESKTOP, _NET_NUMBER_OF_DESKTOPS,
        _NET_FRAME_EXTENTS, RESOURCE_MANAGER,
    }
}

pub struct WindowInfo {
    pub app_id: String,
    pub title: String,
    pub minimized: bool,
    pub ready: bool,
}
struct Surface {
    geometry: Rect,
    role: Role,
    mapped: bool,
    background: (u8, u8, u8),
    pixmap: Option<(Pixmap, Size)>,
}

pub struct X11 {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
    gc: Gcontext,
    depth: u8,
    format: Format,
    visual: Visualtype,
    root_size: Size,
    output: Rect,
    output_name: Option<String>,
    selected_output: Option<String>,
    randr: bool,
    scale: f32,
    surfaces: BTreeMap<u32, Surface>,
    pointer_grab: bool,
    dragging: bool,
    popup_pointer: bool,
    keyboard_grab: bool,
    last_time: Timestamp,
    escape: Vec<u8>,
    desktop: usize,
    desktop_count: usize,
    pub windows: BTreeMap<u32, WindowInfo>,
    pub events: Vec<Event>,
    pub closed: bool,
    pub changed: bool,
    pub windows_changed: bool,
}

impl X11 {
    pub fn connect(output_name: Option<&str>) -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None)?;
        let root = conn.setup().roots[screen_num].clone();
        let visual = root
            .allowed_depths
            .iter()
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == root.root_visual)
            .copied()
            .ok_or("missing X11 root visual")?;
        if visual.class != VisualClass::TRUE_COLOR {
            return Err("Chonk Dock needs an X11 TrueColor visual".into());
        }
        let format = conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|f| f.depth == root.root_depth)
            .copied()
            .ok_or("missing X11 pixel format")?;
        if !matches!(format.bits_per_pixel, 16 | 24 | 32) {
            return Err("unsupported X11 pixel format".into());
        }
        let atoms = Atoms::new(&conn)?.reply()?;
        conn.change_window_attributes(
            root.root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
        )?
        .check()?;
        let randr = conn
            .randr_query_version(1, 3)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 3));
        if randr {
            conn.randr_select_input(
                root.root,
                randr::NotifyMask::SCREEN_CHANGE
                    | randr::NotifyMask::CRTC_CHANGE
                    | randr::NotifyMask::OUTPUT_CHANGE,
            )?;
        }
        let gc = conn.generate_id()?;
        conn.create_gc(gc, root.root, &CreateGCAux::new().graphics_exposures(0))?;
        let root_size = Size::new(root.width_in_pixels.into(), root.height_in_pixels.into());
        let mut state = Self {
            conn,
            root: root.root,
            atoms,
            gc,
            depth: root.root_depth,
            format,
            visual,
            root_size,
            output: Rect {
                pos: Point::new(0, 0),
                size: root_size,
            },
            output_name: output_name.map(str::to_owned),
            selected_output: None,
            randr,
            scale: 1.0,
            surfaces: BTreeMap::new(),
            pointer_grab: false,
            dragging: false,
            popup_pointer: false,
            keyboard_grab: false,
            last_time: CURRENT_TIME,
            escape: Vec::new(),
            desktop: 0,
            desktop_count: 1,
            windows: BTreeMap::new(),
            events: Vec::new(),
            closed: false,
            changed: false,
            windows_changed: true,
        };
        state.refresh_output()?;
        state.refresh_scale();
        state.refresh_keys()?;
        state.refresh_workspaces();
        state.refresh_windows();
        state.conn.flush()?;
        Ok(state)
    }
    pub fn fd(&self) -> std::os::fd::RawFd {
        self.conn.stream().as_raw_fd()
    }
    pub fn screen(&self) -> Rect {
        Rect {
            pos: Point::new(0, 0),
            size: self.output.size,
        }
    }
    pub fn scale(&self) -> f32 {
        self.scale
    }
    pub fn geometry(&self, id: u32) -> Option<Rect> {
        if id == 0 {
            Some(self.screen())
        } else {
            self.surfaces.get(&id).map(|s| s.geometry)
        }
    }
    pub fn running(&self) -> Vec<(String, u32)> {
        self.windows
            .iter()
            .map(|(id, w)| (w.app_id.clone(), *id))
            .collect()
    }
    pub fn workspace(&self) -> (usize, usize) {
        (self.desktop, self.desktop_count)
    }
    fn send_root(&self, window: Window, atom: Atom, data: [u32; 5]) {
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ClientMessageEvent::new(32, window, atom, data),
        );
        let _ = self.conn.flush();
    }
    pub fn activate(&self, id: u32) {
        if self.windows.get(&id).is_some_and(|w| w.minimized) {
            let _ = self.conn.map_window(id);
        }
        self.send_root(
            id,
            self.atoms._NET_ACTIVE_WINDOW,
            [2, self.last_time, 0, 0, 0],
        );
    }
    pub fn focus_workspace(&self, index: usize) {
        if index < self.desktop_count {
            self.send_root(
                self.root,
                self.atoms._NET_CURRENT_DESKTOP,
                [index as u32, self.last_time, 0, 0, 0],
            );
        }
    }
    fn words(&self, window: Window, atom: Atom, kind: Atom, max: u32) -> Vec<u32> {
        self.conn
            .get_property(false, window, atom, kind, 0, max)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }
    fn bytes(&self, window: Window, atom: Atom, kind: Atom) -> Vec<u8> {
        self.conn
            .get_property(false, window, atom, kind, 0, 4096)
            .ok()
            .and_then(|c| c.reply().ok())
            .filter(|r| r.format == 8)
            .map(|r| r.value)
            .unwrap_or_default()
    }
    fn refresh_windows(&mut self) {
        let ids = self.words(
            self.root,
            self.atoms._NET_CLIENT_LIST,
            AtomEnum::WINDOW.into(),
            4096,
        );
        self.windows.retain(|id, _| ids.contains(id));
        for id in ids {
            if self.surfaces.contains_key(&id) {
                continue;
            }
            let _ = self.conn.change_window_attributes(
                id,
                &ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
            );
            self.refresh_window(id);
        }
        self.windows_changed = true;
    }
    fn refresh_window(&mut self, id: Window) {
        let class = self.bytes(id, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
        let mut parts = class.split(|c| *c == 0);
        let instance = parts.next().unwrap_or_default();
        let class = parts.next().filter(|s| !s.is_empty()).unwrap_or(instance);
        let mut title = self.bytes(id, self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING);
        if title.is_empty() {
            title = self.bytes(id, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into());
        }
        let hidden = self
            .words(id, self.atoms._NET_WM_STATE, AtomEnum::ATOM.into(), 64)
            .contains(&self.atoms._NET_WM_STATE_HIDDEN)
            || self
                .words(id, self.atoms.WM_STATE, self.atoms.WM_STATE, 2)
                .first()
                == Some(&3);
        self.windows.insert(
            id,
            WindowInfo {
                app_id: String::from_utf8_lossy(class).into_owned(),
                title: String::from_utf8_lossy(&title).into_owned(),
                minimized: hidden,
                ready: true,
            },
        );
        self.windows_changed = true;
    }
    fn refresh_workspaces(&mut self) {
        self.desktop_count = self
            .words(
                self.root,
                self.atoms._NET_NUMBER_OF_DESKTOPS,
                AtomEnum::CARDINAL.into(),
                1,
            )
            .first()
            .copied()
            .unwrap_or(1)
            .clamp(1, 4096) as usize;
        self.desktop = (self
            .words(
                self.root,
                self.atoms._NET_CURRENT_DESKTOP,
                AtomEnum::CARDINAL.into(),
                1,
            )
            .first()
            .copied()
            .unwrap_or(0) as usize)
            .min(self.desktop_count - 1);
    }
    fn refresh_scale(&mut self) {
        let resources = self.bytes(
            self.root,
            self.atoms.RESOURCE_MANAGER,
            AtomEnum::STRING.into(),
        );
        let scale = xft_scale(&String::from_utf8_lossy(&resources));
        if self.scale != scale {
            self.scale = scale;
            self.changed = true;
        }
    }
    fn refresh_keys(&mut self) -> Result<()> {
        let setup = self.conn.setup();
        let reply = self
            .conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        self.escape = reply
            .keysyms
            .chunks(reply.keysyms_per_keycode.max(1) as usize)
            .enumerate()
            .filter(|(_, symbols)| symbols.contains(&0xff1b))
            .map(|(i, _)| setup.min_keycode + i as u8)
            .collect();
        Ok(())
    }
    fn refresh_output(&mut self) -> Result<()> {
        let geometry = self.conn.get_geometry(self.root)?.reply()?;
        self.root_size = Size::new(geometry.width.into(), geometry.height.into());
        let primary = if self.randr {
            self.conn
                .randr_get_output_primary(self.root)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.output)
        } else {
            None
        };
        let mut outputs = Vec::new();
        if self.randr {
            let resources = self
                .conn
                .randr_get_screen_resources_current(self.root)?
                .reply()?;
            for id in resources.outputs {
                let info = self
                    .conn
                    .randr_get_output_info(id, resources.config_timestamp)?
                    .reply()?;
                if info.connection != randr::Connection::CONNECTED || info.crtc == NONE {
                    continue;
                }
                let crtc = self
                    .conn
                    .randr_get_crtc_info(info.crtc, resources.config_timestamp)?
                    .reply()?;
                if crtc.width == 0 || crtc.height == 0 {
                    continue;
                }
                outputs.push((
                    id,
                    String::from_utf8_lossy(&info.name).into_owned(),
                    Rect {
                        pos: Point::new(crtc.x.into(), crtc.y.into()),
                        size: Size::new(crtc.width.into(), crtc.height.into()),
                    },
                ));
            }
        }
        let requested = self.output_name.as_ref().or(self.selected_output.as_ref());
        let selected = if let Some(name) = requested {
            outputs.iter().find(|(_, n, _)| n == name)
        } else {
            outputs
                .iter()
                .find(|(id, _, _)| Some(*id) == primary)
                .or(outputs.first())
        };
        let rect = if let Some((_, name, rect)) = selected {
            self.selected_output = Some(name.clone());
            *rect
        } else if requested.is_some() {
            return Err("requested X11 output was not found or was disconnected".into());
        } else {
            Rect {
                pos: Point::new(0, 0),
                size: self.root_size,
            }
        };
        if self.output != rect {
            self.output = rect;
            self.changed = true;
        }
        // Positions are relative to the selected output throughout shared UI code.
        for (&id, surface) in &self.surfaces {
            self.conn.configure_window(
                id,
                &ConfigureWindowAux::new()
                    .x(rect.pos.x + surface.geometry.pos.x)
                    .y(rect.pos.y + surface.geometry.pos.y),
            )?;
            if surface.role == Role::Dock {
                self.reserve(id)?;
            }
        }
        Ok(())
    }
    fn reserve(&self, id: Window) -> Result<()> {
        let surface = &self.surfaces[&id];
        // EWMH struts describe the root's outer edges. Do not reserve a whole
        // neighboring output when the dock is on an internal monitor edge.
        let right = if self.output.pos.x + self.output.size.w as i32 == self.root_size.w as i32 {
            surface.geometry.size.w.min(self.root_size.w)
        } else {
            0
        };
        let values = [
            0,
            right,
            0,
            0,
            0,
            0,
            self.output.pos.y.max(0) as u32,
            (self.output.pos.y as i64 + self.output.size.h as i64 - 1).max(0) as u32,
            0,
            0,
            0,
            0,
        ];
        self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.atoms._NET_WM_STRUT_PARTIAL,
            AtomEnum::CARDINAL,
            &values,
        )?;
        self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.atoms._NET_WM_STRUT,
            AtomEnum::CARDINAL,
            &values[..4],
        )?;
        Ok(())
    }
    pub fn dispatch(&mut self) -> Result<bool> {
        let mut received = false;
        for _ in 0..1024 {
            let Some(event) = self.conn.poll_for_event()? else {
                break;
            };
            received = true;
            match event {
                XEvent::ButtonPress(e) | XEvent::ButtonRelease(e) => {
                    self.last_time = e.time;
                    let pressed = e.response_type & 0x7f == BUTTON_PRESS_EVENT;
                    let (id, local) = self.pointer_target(e.event, e.root_x, e.root_y);
                    if pressed && matches!(e.detail, 4 | 5) {
                        self.events.push(Event::Scroll(
                            id,
                            local,
                            if e.detail == 4 { 1 } else { -1 },
                        ));
                    } else if let Some(button) = match e.detail {
                        1 => Some(0x110),
                        2 => Some(0x112),
                        3 => Some(0x111),
                        _ => None,
                    } {
                        self.events.push(Event::Button(id, local, button, pressed));
                    }
                }
                XEvent::MotionNotify(e) => {
                    let (id, local) = self.pointer_target(e.event, e.root_x, e.root_y);
                    self.events.push(Event::Motion(id, local));
                }
                XEvent::LeaveNotify(_) => self.events.push(Event::Leave),
                XEvent::KeyPress(e) if self.escape.contains(&e.detail) => {
                    self.events.push(Event::Escape)
                }
                XEvent::MappingNotify(_) => self.refresh_keys()?,
                XEvent::ClientMessage(e)
                    if e.type_ == self.atoms.WM_PROTOCOLS
                        && e.data.as_data32()[0] == self.atoms.WM_DELETE_WINDOW =>
                {
                    self.closed = true
                }
                XEvent::DestroyNotify(e) => {
                    if self
                        .surfaces
                        .get(&e.window)
                        .is_some_and(|s| s.role == Role::Dock)
                    {
                        self.closed = true;
                    }
                    if self.windows.remove(&e.window).is_some() {
                        self.windows_changed = true;
                    }
                }
                XEvent::PropertyNotify(e) if e.window == self.root => {
                    if e.atom == self.atoms._NET_CLIENT_LIST {
                        self.refresh_windows();
                    } else if [
                        self.atoms._NET_CURRENT_DESKTOP,
                        self.atoms._NET_NUMBER_OF_DESKTOPS,
                    ]
                    .contains(&e.atom)
                    {
                        self.refresh_workspaces();
                    } else if e.atom == self.atoms.RESOURCE_MANAGER {
                        self.refresh_scale();
                    }
                }
                XEvent::PropertyNotify(e)
                    if self.windows.contains_key(&e.window)
                        && [
                            AtomEnum::WM_CLASS.into(),
                            AtomEnum::WM_NAME.into(),
                            self.atoms._NET_WM_NAME,
                            self.atoms._NET_WM_STATE,
                            self.atoms.WM_STATE,
                        ]
                        .contains(&e.atom) =>
                {
                    self.refresh_window(e.window)
                }
                XEvent::RandrScreenChangeNotify(_) | XEvent::RandrNotify(_) => {
                    self.refresh_output()?
                }
                XEvent::ConfigureNotify(e) if e.window == self.root => self.refresh_output()?,
                XEvent::Error(e) => {
                    tracing::debug!(?e, "X11 client window changed during a request")
                }
                _ => {}
            }
        }
        Ok(received)
    }
    fn pointer_target(&self, event: Window, root_x: i16, root_y: i16) -> (u32, Point) {
        let root = Point::new(
            root_x as i32 - self.output.pos.x,
            root_y as i32 - self.output.pos.y,
        );
        let hit = self
            .surfaces
            .iter()
            .rev()
            .find(|(_, s)| s.mapped && s.geometry.contains(root));
        // During an implicit press grab, event names the original window even
        // while the pointer is over another tile. Root coordinates are authoritative.
        let target = hit.or_else(|| {
            self.surfaces
                .get_key_value(&event)
                .filter(|(_, s)| s.mapped)
        });
        target.map_or((0, root), |(&id, s)| {
            (
                id,
                Point::new(root.x - s.geometry.pos.x, root.y - s.geometry.pos.y),
            )
        })
    }
    fn update_pointer_grab(&mut self) {
        let wanted = self.dragging || self.popup_pointer;
        if wanted && !self.pointer_grab {
            self.pointer_grab = self
                .conn
                .grab_pointer(
                    true,
                    self.root,
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    NONE,
                    NONE,
                    CURRENT_TIME,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|r| r.status == GrabStatus::SUCCESS);
        } else if !wanted && self.pointer_grab {
            let _ = self.conn.ungrab_pointer(CURRENT_TIME);
            self.pointer_grab = false;
        }
    }
    pub fn is_backdrop(&self, id: u32) -> bool {
        id == 0
    }
    pub fn sync_backdrop(&mut self) {
        self.popup_pointer = self
            .surfaces
            .values()
            .any(|s| s.mapped && s.role == Role::Panel);
        self.update_pointer_grab();
        self.panel_keyboard(self.popup_pointer);
    }
    pub fn present(&mut self) -> Result<()> {
        self.conn.flush()?;
        Ok(())
    }
    fn upload(&mut self, id: u32, frame: &DecorationBuffer) -> Result<()> {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return Ok(());
        };
        let size = Size::new(frame.width, frame.height);
        if frame.width == 0
            || frame.height == 0
            || frame.width > u16::MAX.into()
            || frame.height > i16::MAX as u32
        {
            return Ok(());
        }
        if surface.pixmap.is_some_and(|(_, old)| old != size) {
            if let Some((pixmap, _)) = surface.pixmap.take() {
                self.conn.free_pixmap(pixmap)?;
            }
        }
        let pixmap = if let Some((pixmap, _)) = surface.pixmap {
            pixmap
        } else {
            let pixmap = self.conn.generate_id()?;
            self.conn.create_pixmap(
                self.depth,
                pixmap,
                id,
                frame.width as u16,
                frame.height as u16,
            )?;
            surface.pixmap = Some((pixmap, size));
            pixmap
        };
        let (data, stride) = server_pixels(
            frame,
            self.visual,
            self.format,
            self.conn.setup().image_byte_order,
            surface.background,
        );
        let rows = (self.conn.maximum_request_bytes().saturating_sub(64) / stride).max(1);
        for (index, chunk) in data.chunks(rows * stride).enumerate() {
            self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                pixmap,
                self.gc,
                frame.width as u16,
                (chunk.len() / stride) as u16,
                0,
                (index * rows) as i16,
                0,
                self.depth,
                chunk,
            )?;
        }
        self.conn.change_window_attributes(
            id,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?;
        self.conn.clear_area(false, id, 0, 0, 0, 0)?;
        Ok(())
    }
}

impl Backend for X11 {
    type ShellId = u32;
    type WindowId = u32;
    fn create_shell_surface(&mut self, geometry: Rect, bg: (u8, u8, u8), _: bool) -> Option<u32> {
        let create = || -> Result<u32> {
            let id = self.conn.generate_id()?;
            self.conn
                .create_window(
                    self.depth,
                    id,
                    self.root,
                    (self.output.pos.x + geometry.pos.x) as i16,
                    (self.output.pos.y + geometry.pos.y) as i16,
                    geometry.size.w.max(1) as u16,
                    geometry.size.h.max(1) as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new().override_redirect(1).event_mask(
                        EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::POINTER_MOTION
                            | EventMask::LEAVE_WINDOW
                            | EventMask::STRUCTURE_NOTIFY,
                    ),
                )?
                .check()?;
            self.conn.change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"chonk-dock\0ChonkDock\0",
            )?;
            self.conn.change_property8(
                PropMode::REPLACE,
                id,
                self.atoms._NET_WM_NAME,
                self.atoms.UTF8_STRING,
                b"Chonk Dock",
            )?;
            self.conn.change_property32(
                PropMode::REPLACE,
                id,
                self.atoms._NET_WM_PID,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )?;
            self.conn.change_property32(
                PropMode::REPLACE,
                id,
                self.atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                &[self.atoms.WM_DELETE_WINDOW],
            )?;
            self.conn.change_property32(
                PropMode::REPLACE,
                id,
                self.atoms._NET_FRAME_EXTENTS,
                AtomEnum::CARDINAL,
                &[0; 4],
            )?;
            Ok(id)
        };
        match create() {
            Ok(id) => {
                self.surfaces.insert(
                    id,
                    Surface {
                        geometry,
                        role: Role::Panel,
                        mapped: false,
                        background: bg,
                        pixmap: None,
                    },
                );
                self.set_role(id, Role::Panel);
                Some(id)
            }
            Err(error) => {
                tracing::error!(%error,"creating X11 dock surface");
                None
            }
        }
    }
    fn set_role(&mut self, id: u32, role: Role) {
        let Some(s) = self.surfaces.get_mut(&id) else {
            return;
        };
        s.role = role;
        let _ = self.conn.change_window_attributes(
            id,
            &ChangeWindowAttributesAux::new().override_redirect(u32::from(role != Role::Dock)),
        );
        let kind = if role == Role::Panel {
            self.atoms._NET_WM_WINDOW_TYPE_POPUP_MENU
        } else {
            self.atoms._NET_WM_WINDOW_TYPE_DOCK
        };
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[kind],
        );
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.atoms._NET_WM_DESKTOP,
            AtomEnum::CARDINAL,
            &[u32::MAX],
        );
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            id,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &[
                self.atoms._NET_WM_STATE_ABOVE,
                self.atoms._NET_WM_STATE_STICKY,
                self.atoms._NET_WM_STATE_SKIP_TASKBAR,
                self.atoms._NET_WM_STATE_SKIP_PAGER,
            ],
        );
        if role == Role::Dock {
            if let Err(error) = self.reserve(id) {
                tracing::warn!(%error,"publishing X11 dock strut");
            }
        }
    }
    fn map_shell_surface(&mut self, id: u32) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.mapped = true;
        }
        let _ = self.conn.map_window(id);
        self.raise_shell_surface(id);
    }
    fn unmap_shell_surface(&mut self, id: u32) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.mapped = false;
        }
        let _ = self.conn.unmap_window(id);
    }
    fn configure_shell_surface(&mut self, id: u32, rect: Rect) {
        let Some(s) = self.surfaces.get_mut(&id) else {
            return;
        };
        if s.geometry == rect {
            return;
        }
        s.geometry = rect;
        let dock = s.role == Role::Dock;
        let _ = self.conn.configure_window(
            id,
            &ConfigureWindowAux::new()
                .x(self.output.pos.x + rect.pos.x)
                .y(self.output.pos.y + rect.pos.y)
                .width(rect.size.w.max(1))
                .height(rect.size.h.max(1)),
        );
        if dock {
            let _ = self.reserve(id);
        }
    }
    fn paint_shell_surface(&mut self, id: u32, pixels: &DecorationBuffer) {
        if let Err(error) = self.upload(id, pixels) {
            tracing::warn!(%error,"painting X11 dock surface");
        }
    }
    fn release_shell_buffer(&mut self, id: u32) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            if let Some((pixmap, _)) = s.pixmap.take() {
                let _ = self.conn.free_pixmap(pixmap);
            }
        }
    }
    fn destroy_shell_surface(&mut self, id: u32) {
        self.release_shell_buffer(id);
        self.surfaces.remove(&id);
        let _ = self.conn.destroy_window(id);
    }
    fn raise_shell_surface(&mut self, id: u32) {
        let _ = self
            .conn
            .configure_window(id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
    }
    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        self.dragging = true;
        self.update_pointer_grab();
        DragHandle(u64::from(self.pointer_grab))
    }
    fn ungrab_pointer(&mut self, _: DragHandle) {
        self.dragging = false;
        self.update_pointer_grab();
    }
    fn panel_keyboard(&mut self, enabled: bool) {
        if enabled && !self.keyboard_grab {
            self.keyboard_grab = self
                .conn
                .grab_keyboard(
                    false,
                    self.root,
                    CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|r| r.status == GrabStatus::SUCCESS);
        } else if !enabled && self.keyboard_grab {
            let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
            self.keyboard_grab = false;
        }
    }
}
impl PopupHost for X11 {
    type PopupId = u32;
    fn create_popup(&mut self, rect: Rect, bg: (u8, u8, u8)) -> Option<u32> {
        let id = self.create_shell_surface(rect, bg, true)?;
        self.set_role(id, Role::Panel);
        self.map_shell_surface(id);
        Some(id)
    }
    fn destroy_popup(&mut self, id: u32) {
        self.destroy_shell_surface(id);
    }
    fn paint_popup(&mut self, id: u32, pixels: &DecorationBuffer) {
        self.paint_shell_surface(id, pixels);
    }
    fn grab_pointer(&mut self) -> PopupGrab {
        self.popup_pointer = true;
        self.update_pointer_grab();
        PopupGrab(u64::from(self.pointer_grab))
    }
    fn ungrab_pointer(&mut self, _: PopupGrab) {
        self.popup_pointer = false;
        self.update_pointer_grab();
    }
    fn grab_keyboard(&mut self) {
        self.panel_keyboard(true);
    }
    fn ungrab_keyboard(&mut self) {
        self.panel_keyboard(false);
    }
}

fn xft_scale(resources: &str) -> f32 {
    resources
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "Xft.dpi")
                .then(|| value.trim().parse::<f32>().ok())
                .flatten()
        })
        .filter(|dpi| dpi.is_finite() && *dpi > 0.0)
        .map_or(1.0, |dpi| (dpi / 96.0).clamp(0.5, 4.0))
}

fn server_pixels(
    frame: &DecorationBuffer,
    visual: Visualtype,
    format: Format,
    order: ImageOrder,
    bg: (u8, u8, u8),
) -> (Vec<u8>, usize) {
    let bytes = format.bits_per_pixel as usize / 8;
    let pad = (format.scanline_pad as usize / 8).max(1);
    let stride = (frame.width as usize * bytes).div_ceil(pad) * pad;
    let mut result = vec![0; stride * frame.height as usize];
    let channel = |value: u8, mask: u32| -> u32 {
        if mask == 0 {
            return 0;
        }
        let shift = mask.trailing_zeros();
        (((value as u64 * (mask >> shift) as u64 + 127) / 255) as u32) << shift
    };
    for (src, dst) in frame
        .pixels
        .chunks_exact(frame.width as usize * 4)
        .zip(result.chunks_exact_mut(stride))
    {
        for (rgba, target) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.chunks_exact_mut(bytes))
        {
            let blend =
                |c: u8, b: u8| (c as u32 + b as u32 * (255 - rgba[3] as u32) / 255).min(255) as u8;
            let pixel = channel(blend(rgba[0], bg.0), visual.red_mask)
                | channel(blend(rgba[1], bg.1), visual.green_mask)
                | channel(blend(rgba[2], bg.2), visual.blue_mask);
            if order == ImageOrder::LSB_FIRST {
                target.copy_from_slice(&pixel.to_le_bytes()[..bytes]);
            } else {
                target.copy_from_slice(&pixel.to_be_bytes()[4 - bytes..]);
            }
        }
    }
    (result, stride)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dpi_supports_fractional_scale_and_rejects_invalid_values() {
        assert_eq!(xft_scale("Xft.dpi: 144\n"), 1.5);
        assert_eq!(xft_scale("Xft.dpi: NaN"), 1.0);
        assert_eq!(xft_scale("Xft.dpi: -4"), 1.0);
    }
    #[test]
    fn pixels_follow_server_masks_order_and_scanline_padding() {
        let visual = Visualtype {
            red_mask: 0xff0000,
            green_mask: 0xff00,
            blue_mask: 0xff,
            ..Default::default()
        };
        let frame = DecorationBuffer {
            width: 1,
            height: 1,
            pixels: vec![10, 20, 30, 255],
        };
        let format = Format {
            depth: 24,
            bits_per_pixel: 24,
            scanline_pad: 32,
        };
        assert_eq!(
            server_pixels(&frame, visual, format, ImageOrder::LSB_FIRST, (0, 0, 0)),
            (vec![30, 20, 10, 0], 4)
        );
        assert_eq!(
            server_pixels(&frame, visual, format, ImageOrder::MSB_FIRST, (0, 0, 0)),
            (vec![10, 20, 30, 0], 4)
        );
    }
}
