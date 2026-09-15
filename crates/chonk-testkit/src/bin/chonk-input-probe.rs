//! Minimal native client reporting the coordinates delivered by the seat.
//!
//! A real committed buffer scale / viewport is essential: a compositor-only
//! hit-test assertion cannot see a click grab retaining an obsolete origin.
//!
//! `cursor-shape <name>` names a `wp_cursor_shape_v1` shape on every pointer
//! enter, the way GTK 4, Qt 6 and Chromium set their cursors, and reports
//! `cursor-shape applied <serial>` once the compositor has processed it.
//! `resizable` drops the fixed maximum size so the frame offers resize edges.
//! `dnd` starts an internal drag on a right press; with `icon` the drag
//! carries a solid orange icon surface, committed with an `attach` offset
//! after `start_drag`, and reports `icon frame done` when the compositor
//! answers its frame callback.
//! `--kde-bind-only` binds `org_kde_kwin_server_decoration_manager` and
//! creates no decoration object, which is how a GTK4 header-bar window says
//! it draws its own titlebar.
//! `lagged-fullscreen` requests fullscreen and answers each configure with its
//! old 400x300 buffer. `stale-geometry-fullscreen` pins a 400x300 window
//! geometry, answers the fullscreen configure with a buffer of the full size,
//! then asks to be maximized and leaves every later configure unacknowledged.

#[path = "chonk-input-probe/constraints.rs"]
mod constraints;
#[path = "chonk-input-probe/interactive.rs"]
mod interactive;

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use wayland_client::protocol::{
    wl_buffer, wl_callback,
    wl_compositor::WlCompositor,
    wl_data_device,
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
    wl_touch,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
    zwp_pointer_gesture_swipe_v1::{self, ZwpPointerGestureSwipeV1},
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager;
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

fn say(line: &str) {
    println!("{line}");
    std::io::stdout().flush().expect("flush probe event");
}

#[derive(Default)]
struct Probe {
    constraints: constraints::State,
    interactive: interactive::State,
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<XdgWmBase>,
    viewporter: Option<WpViewporter>,
    seat: Option<wl_seat::WlSeat>,
    seat_version: u32,
    pointer: Option<wl_pointer::WlPointer>,
    gestures: Option<ZwpPointerGesturesV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    shortcuts_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    toplevel: Option<XdgToplevel>,
    inhibit: bool,
    keymap_count: u64,
    touch: Option<wl_touch::WlTouch>,
    touches: HashMap<i32, (f64, f64)>,
    data_manager: Option<WlDataDeviceManager>,
    data_device: Option<wl_data_device::WlDataDevice>,
    drag_offer: Option<WlDataOffer>,
    surface: Option<WlSurface>,
    pointer_surface: Option<WlSurface>,
    drag_mode: bool,
    /// `dnd icon`: the drag also carries a solid orange icon surface,
    /// committed with [`ICON_OFFSET`] after `start_drag`, and asks it
    /// for one frame callback. Kept alive here for the drag's life.
    icon_mode: bool,
    icon: Option<(WlSurface, Option<WpViewport>, std::fs::File, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer)>,
    /// The scale the probe draws at, for buffers created after map.
    scale: f64,
    cursor_shape: Option<Shape>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    cursor_shape_device: Option<WpCursorShapeDeviceV1>,
    /// Held for the client's lifetime and never used: binding it is the
    /// whole of what `--kde-bind-only` says.
    kde_decoration_manager: Option<OrgKdeKwinServerDecorationManager>,
    position: (f64, f64),
    sequence: u64,
    answer_with_old_buffer: bool,
    /// `stale-geometry-fullscreen` state: the last toplevel configure's size
    /// and fullscreen flag, whether later configures go unacknowledged, and
    /// the full-size buffer kept alive for as long as it is attached.
    stale_geometry: bool,
    toplevel_size: (i32, i32),
    toplevel_fullscreen: bool,
    withhold_acks: bool,
    full_buffer: Option<(std::fs::File, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer)>,
}

/// The enter serial a `set_shape` used. The sync sent after it is
/// answered only once the compositor has processed that request.
struct CursorShapeApplied(u32);

/// The drag icon's frame callback: answered only if the compositor
/// treats the icon as visible content.
struct IconFrame;

/// The drag icon's logical size and its ARGB8888 little-endian fill,
/// orange (`R=0xF0 G=0xA0 B=0x20`): unlike the probe's red content, its
/// green stale-geometry buffer, and the wallpaper.
const ICON_SIZE: (i32, i32) = (40, 30);
const ICON_PIXEL: [u8; 4] = [0x20, 0xA0, 0xF0, 0xFF];
/// Where the icon's corner sits relative to the pointer, in its own
/// logical units, given as the `attach` dx/dy of its first commit.
const ICON_OFFSET: (i32, i32) = (6, 10);

/// A configure serial answered by committing the old buffer. The sync sent
/// after that commit returns once the compositor has processed it.
struct ConfigureAnswered(u32);

impl Probe {
    fn report(&mut self, kind: &str) {
        self.report_at(kind, self.position);
    }

    fn report_at(&mut self, kind: &str, position: (f64, f64)) {
        self.sequence += 1;
        say(&format!(
            "input {} {kind} {:.4} {:.4}",
            self.sequence, position.0, position.1
        ));
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_pointer_constraints_v1" | "zwp_relative_pointer_manager_v1" => {
                    probe.constraints.bind(registry, name, &interface, qh);
                }
                "wl_compositor" => {
                    probe.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_subcompositor" => probe.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "zwp_pointer_gestures_v1" => probe.gestures = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_shm" => probe.shm = Some(registry.bind(name, 1, qh, ())),
                "org_kde_kwin_server_decoration_manager"
                    if std::env::args().any(|arg| arg == "--kde-bind-only") =>
                {
                    probe.kde_decoration_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_seat" => {
                    probe.seat = Some(registry.bind(name, version.min(probe.seat_version), qh, ()))
                }
                "wp_viewporter" => probe.viewporter = Some(registry.bind(name, 1, qh, ())),
                "wl_data_device_manager" => {
                    probe.data_manager = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "zwp_text_input_manager_v3" => {
                    probe.text_input_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_input_method_manager_v2" => {
                    probe.input_method_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    probe.shortcuts_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "wp_cursor_shape_manager_v1" => {
                    probe.cursor_shape_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Probe {
    fn event(
        probe: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            let capabilities = capabilities.into_result().expect("known seat capabilities");
            if capabilities.contains(wl_seat::Capability::Pointer) && probe.pointer.is_none() {
                probe.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && probe.keyboard.is_none() {
                probe.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Touch) && probe.touch.is_none() {
                probe.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { fd, size, format } => {
                assert_eq!(
                    format.into_result().expect("known keymap format"),
                    wl_keyboard::KeymapFormat::XkbV1
                );
                assert!(size <= 8 * 1024 * 1024, "bounded test keymap");
                // The keymap is an mmap-style fd; its shared seek offset is
                // unspecified. Read from zero without modifying that offset.
                let mut bytes = vec![0; size as usize];
                std::fs::File::from(fd)
                    .read_exact_at(&mut bytes, 0)
                    .expect("complete keymap fd");
                say(&format!("keyboard keymap-nul {}", bytes.last() == Some(&0)));
                probe.keymap_count += 1;
                say(&format!("keyboard keymap {} {size}", probe.keymap_count));
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                say(&format!("keyboard repeat {rate} {delay}"))
            }
            wl_keyboard::Event::Enter { .. } => say("keyboard enter"),
            wl_keyboard::Event::Leave { .. } => say("keyboard leave"),
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                say(&format!(
                    "keyboard modifiers {mods_depressed} {mods_latched} {mods_locked} {group}"
                ));
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed =
                    state.into_result().expect("known key state") == wl_keyboard::KeyState::Pressed;
                say(&format!(
                    "keyboard key {key} {}",
                    if pressed { "down" } else { "up" }
                ));
                if pressed && probe.interactive.enabled() {
                    if let Some(marker) = probe.interactive.key(
                        key,
                        probe.toplevel.as_ref().expect("toplevel"),
                        probe.seat.as_ref().expect("seat"),
                    ) {
                        connection.display().sync(qh, marker);
                    }
                }
                if pressed && probe.constraints.enabled() {
                    let acted = probe.constraints.key(
                        key,
                        probe.compositor.as_ref().expect("compositor"),
                        probe.pointer.as_ref().expect("pointer"),
                        probe.pointer_surface.as_ref().expect("pointer entered"),
                        qh,
                    );
                    if acted {
                        connection.display().sync(qh, key);
                    }
                }
                if probe.inhibit && pressed && key == 63 {
                    probe
                        .toplevel
                        .as_ref()
                        .expect("mapped toplevel")
                        .set_minimized();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down {
                serial, id, x, y, ..
            } => {
                probe.interactive.touch_down(serial);
                assert!(
                    probe.touches.insert(id, (x, y)).is_none(),
                    "touch ID reused while held"
                );
                probe.report_at(&format!("touch-down-{id}"), (x, y));
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                *probe.touches.get_mut(&id).expect("motion after touch-down") = (x, y);
                probe.report_at(&format!("touch-motion-{id}"), (x, y));
            }
            wl_touch::Event::Up { id, .. } => {
                let position = probe.touches.remove(&id).expect("up after touch-down");
                probe.report_at(&format!("touch-up-{id}"), position);
            }
            wl_touch::Event::Cancel => {
                probe.touches.clear();
                probe.report_at("touch-cancel", (0.0, 0.0));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                say(if Some(&surface) == probe.surface.as_ref() {
                    "entered root"
                } else {
                    "entered subsurface"
                });
                probe.pointer_surface = Some(surface);
                probe.position = (surface_x, surface_y);
                probe.report("enter");
                if let Some(shape) = probe.cursor_shape {
                    let device = probe.cursor_shape_device.get_or_insert_with(|| {
                        probe
                            .cursor_shape_manager
                            .as_ref()
                            .expect("wp_cursor_shape_manager_v1")
                            .get_pointer(probe.pointer.as_ref().expect("pointer"), qh, ())
                    });
                    device.set_shape(serial, shape);
                    connection.display().sync(qh, CursorShapeApplied(serial));
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                probe.position = (surface_x, surface_y);
                probe.report("motion");
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state,
                ..
            } => {
                if matches!(state.into_result(), Ok(wl_pointer::ButtonState::Pressed)) {
                    probe.interactive.pointer_down(serial);
                }
                probe.report(
                    if matches!(state.into_result(), Ok(wl_pointer::ButtonState::Pressed)) {
                        "press"
                    } else {
                        "release"
                    },
                );
                if probe.drag_mode
                    && button == 0x111
                    && matches!(state.into_result(), Ok(wl_pointer::ButtonState::Pressed))
                {
                    let source = probe
                        .data_manager
                        .as_ref()
                        .expect("data manager")
                        .create_data_source(qh, ());
                    source.offer("text/plain;charset=utf-8".into());
                    source.set_actions(DndAction::Copy);
                    let icon = probe
                        .icon_mode
                        .then(|| probe.compositor.as_ref().expect("compositor").create_surface(qh, ()));
                    probe.data_device.as_ref().expect("data device").start_drag(
                        Some(&source),
                        probe.pointer_surface.as_ref().expect("pointer surface"),
                        icon.as_ref(),
                        serial,
                    );
                    say("started internal drag");
                    if let Some(surface) = icon {
                        // Committed after `start_drag`, as a toolkit does once
                        // the surface wears the icon role, at the probe's own
                        // density: an integer buffer scale, or a viewport
                        // destination for the fractional case.
                        let scale = probe.scale;
                        let (width, height) = ((ICON_SIZE.0 as f64 * scale) as i32, (ICON_SIZE.1 as f64 * scale) as i32);
                        let shm = probe.shm.clone().expect("wl_shm");
                        let (file, pool, buffer) = solid_buffer(&shm, qh, width, height, ICON_PIXEL);
                        let viewport = if scale.fract() != 0.0 {
                            let viewport =
                                probe.viewporter.as_ref().expect("wp_viewporter").get_viewport(&surface, qh, ());
                            viewport.set_destination(ICON_SIZE.0, ICON_SIZE.1);
                            Some(viewport)
                        } else {
                            surface.set_buffer_scale(scale as i32);
                            None
                        };
                        surface.attach(Some(&buffer), ICON_OFFSET.0, ICON_OFFSET.1);
                        surface.damage_buffer(0, 0, width, height);
                        surface.frame(qh, IconFrame);
                        surface.commit();
                        say("icon committed");
                        probe.icon = Some((surface, viewport, file, pool, buffer));
                    }
                }
            }
            wl_pointer::Event::Leave { .. } => probe.report("leave"),
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, CursorShapeApplied> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        applied: &CursorShapeApplied,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            say(&format!("cursor-shape applied {}", applied.0));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, IconFrame> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &IconFrame,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            say("icon frame done");
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ConfigureAnswered> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        answered: &ConfigureAnswered,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            say(&format!("configure answered {}", answered.0));
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Enter {
                serial, x, y, id, ..
            } => {
                if let Some(offer) = &id {
                    offer.accept(serial, Some("text/plain;charset=utf-8".into()));
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                probe.drag_offer = id;
                probe.position = (x, y);
                probe.report("dnd-enter");
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                probe.position = (x, y);
                probe.report("dnd-motion");
            }
            wl_data_device::Event::Drop => {
                if let Some(offer) = probe.drag_offer.take() {
                    offer.finish();
                    offer.destroy();
                }
                probe.report("dnd-drop");
            }
            wl_data_device::Event::Leave => probe.report("dnd-leave"),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Probe, wl_data_device::WlDataDevice, [
        0 => (WlDataOffer, ())
    ]);
}

impl Dispatch<WlDataOffer, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Action { dnd_action } = event {
            if matches!(dnd_action.into_result(), Ok(DndAction::Copy)) {
                say("drag copy accepted");
            }
        }
    }
}

impl Dispatch<WlDataSource, ()> for Probe {
    fn event(
        _: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let _ = std::fs::File::from(fd).write_all(b"input probe\n");
            }
            wl_data_source::Event::Cancelled => {
                say("drag cancelled");
                source.destroy()
            }
            wl_data_source::Event::DndFinished => {
                say("drag finished");
                source.destroy()
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => say("shortcut-inhibitor active"),
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                say("shortcut-inhibitor inactive")
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        assert!(
            !matches!(event, zwp_input_method_v2::Event::Unavailable),
            "the private seat must accept its only test input method"
        );
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, u32> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        generation: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_keyboard_grab_v2::Event::Key { key, state, .. } = event {
            say(&format!("ime grab {generation} key {key} {state:?}"));
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => say("text-input enter"),
            zwp_text_input_v3::Event::Leave { .. } => say("text-input leave"),
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
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

impl Dispatch<XdgSurface, ()> for Probe {
    fn event(
        probe: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        connection: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            say(&format!("surface configure {serial}"));
            if probe.withhold_acks {
                say(&format!("left configure {serial} unanswered"));
                return;
            }
            surface.ack_configure(serial);
            // `lagged-fullscreen`: answer at once with the old buffer, as a
            // client whose redraw lags its acknowledgement does.
            if probe.answer_with_old_buffer {
                if let Some(root) = &probe.surface {
                    root.commit();
                    connection.display().sync(qh, ConfigureAnswered(serial));
                }
            } else if probe.stale_geometry && probe.toplevel_fullscreen && probe.toplevel_size.0 > 0 && probe.toplevel_size.1 > 0 {
                // `stale-geometry-fullscreen`: pixels that fill the fullscreen
                // size under the pinned 400x300 geometry, then a maximize
                // request whose configure is never acknowledged, so a reply
                // stays pending while the buffer already answers the resize.
                let (width, height) = probe.toplevel_size;
                let shm = probe.shm.clone().expect("wl_shm");
                let full = solid_buffer(&shm, qh, width, height, [0x40, 0xC0, 0x40, 0xFF]);
                if let Some(root) = &probe.surface {
                    root.attach(Some(&full.2), 0, 0);
                    root.damage_buffer(0, 0, width, height);
                    root.commit();
                }
                probe.full_buffer = Some(full);
                probe.withhold_acks = true;
                if let Some(toplevel) = &probe.toplevel {
                    toplevel.set_maximized();
                }
                connection.display().sync(qh, ConfigureAnswered(serial));
            }
        }
    }
}

impl Dispatch<ZwpPointerGestureSwipeV1, ()> for Probe {
    fn event(_: &mut Self, _: &ZwpPointerGestureSwipeV1, event: zwp_pointer_gesture_swipe_v1::Event,
        _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_pointer_gesture_swipe_v1::Event::Begin { fingers, .. } => say(&format!("swipe begin {fingers}")),
            zwp_pointer_gesture_swipe_v1::Event::Update { dx, dy, .. } => say(&format!("swipe update {dx} {dy}")),
            zwp_pointer_gesture_swipe_v1::Event::End { cancelled, .. } => say(&format!("swipe end {cancelled}")),
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, states } = event {
            probe.toplevel_size = (width, height);
            probe.toplevel_fullscreen = states
                .as_chunks::<4>()
                .0
                .iter()
                .any(|state| u32::from_ne_bytes(*state) == xdg_toplevel::State::Fullscreen as u32);
        }
    }
}

/// An opaque shm buffer of `width` by `height` pixels. The file behind the
/// pool is returned with it and must outlive the buffer.
/// A `width`x`height` ARGB8888 buffer filled with `pixel`'s little-endian bytes.
fn solid_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<Probe>,
    width: i32,
    height: i32,
    pixel: [u8; 4],
) -> (std::fs::File, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer) {
    let path = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("private runtime"))
        .join(format!("chonk-input-probe-{}-{width}x{height}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("exclusive scratch buffer");
    std::fs::remove_file(&path).expect("unlink scratch buffer");
    let bytes: Vec<u8> = std::iter::repeat_n(pixel, (width * height) as usize)
        .flatten()
        .collect();
    file.write_all(&bytes).expect("fill buffer");
    let pool = shm.create_pool(file.as_fd(), width * height * 4, qh, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, qh, ());
    (file, pool, buffer)
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Probe {
            fn event(
                _: &mut Self, _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>,
            ) {}
        }
    )*};
}

ignore_events!(
    ZwpPointerGesturesV1,
    ZwpKeyboardShortcutsInhibitManagerV1,
    ZwpInputMethodManagerV2,
    ZwpTextInputManagerV3,
    WlDataDeviceManager,
    WlCompositor,
    WlSubcompositor,
    WlSubsurface,
    WlSurface,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    WpViewporter,
    WpViewport,
    WpCursorShapeManagerV1,
    WpCursorShapeDeviceV1,
    OrgKdeKwinServerDecorationManager
);

/// The `--csd-input-region` buffer, drawn the way a toolkit with client-side
/// shadows draws one: an opaque grey band around the declared window
/// geometry (25, 30, 340x230 logical), and inside it content whose every
/// pixel names its own buffer position (red the column, green the row, both
/// modulo 256). A capture anchored anywhere but the geometry origin shows
/// the band, or the right colours in the wrong places. Opaque throughout, so
/// a capture over a transparent clear and the composited output agree.
fn csd_shadow_buffer(width: i32, height: i32, scale: f64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (logical_x, logical_y) = (f64::from(x) / scale, f64::from(y) / scale);
            let content = (25.0..365.0).contains(&logical_x) && (30.0..260.0).contains(&logical_y);
            // Premultiplied ARGB8888, little-endian: B, G, R, A.
            bytes.extend_from_slice(&if content {
                [0xC0, (y & 0xFF) as u8, (x & 0xFF) as u8, 0xFF]
            } else {
                [0x30, 0x30, 0x30, 0xFF]
            });
        }
    }
    bytes
}

/// The shape named by `cursor-shape <name>`, in the protocol's spelling.
fn cursor_shape_arg() -> Option<Shape> {
    let name = std::env::args().skip_while(|arg| arg != "cursor-shape").nth(1)?;
    Some(match name.as_str() {
        "default" => Shape::Default,
        "text" => Shape::Text,
        "pointer" => Shape::Pointer,
        "crosshair" => Shape::Crosshair,
        "wait" => Shape::Wait,
        "ew_resize" => Shape::EwResize,
        other => panic!("unsupported test cursor shape {other}"),
    })
}

fn main() {
    let scale: f64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1".into())
        .parse()
        .expect("scale");
    assert!(
        [1.0, 1.5, 2.0].contains(&scale),
        "supported test scales: 1, 1.5, 2"
    );
    let connection = Connection::connect_to_env().expect("private Wayland connection");
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe {
        constraints: constraints::State::from_args(),
        interactive: interactive::State::from_args(),
        inhibit: std::env::args().any(|arg| arg == "inhibit"),
        drag_mode: std::env::args().any(|arg| arg == "dnd"),
        icon_mode: std::env::args().any(|arg| arg == "icon"),
        scale,
        cursor_shape: cursor_shape_arg(),
        seat_version: if std::env::args().any(|arg| arg == "legacy-keyboard") {
            5
        } else {
            7
        },
        ..Probe::default()
    };
    queue.roundtrip(&mut probe).expect("registry");
    queue.roundtrip(&mut probe).expect("seat capabilities");
    let _swipe = probe.gestures.as_ref().map(|manager| manager.get_swipe_gesture(probe.pointer.as_ref().expect("pointer"), &qh, ()));
    let replace_ime_grab = std::env::args().any(|arg| arg == "ime-replace-grab");
    let _input_method = (replace_ime_grab || std::env::args().any(|arg| arg == "ime")).then(|| {
        probe
            .input_method_manager
            .as_ref()
            .expect("input-method-v2")
            .get_input_method(probe.seat.as_ref().expect("seat"), &qh, ())
    });
    let _text_input = probe
        .text_input_manager
        .as_ref()
        .expect("text-input-v3")
        .get_text_input(probe.seat.as_ref().expect("seat"), &qh, ());
    probe.data_device = Some(
        probe
            .data_manager
            .as_ref()
            .expect("wl_data_device_manager")
            .get_data_device(probe.seat.as_ref().expect("seat"), &qh, ()),
    );

    let surface = probe
        .compositor
        .as_ref()
        .expect("wl_compositor")
        .create_surface(&qh, ());
    probe.surface = Some(surface.clone());
    let xdg = probe
        .wm_base
        .as_ref()
        .expect("xdg_wm_base")
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    probe.toplevel = Some(toplevel.clone());
    let _inhibitor = probe.inhibit.then(|| {
        probe
            .shortcuts_manager
            .as_ref()
            .expect("shortcuts-inhibit")
            .inhibit_shortcuts(&surface, probe.seat.as_ref().expect("seat"), &qh, ())
    });
    let name = std::env::args()
        .find_map(|argument| argument.strip_prefix("--app-id=").map(str::to_owned))
        .unwrap_or_else(|| "input-probe".into());
    toplevel.set_title(name.clone());
    toplevel.set_app_id(name);
    // GTK-style shadow buffer with an explicit resize band and a hole in
    // the interior. Deliberately asymmetric: window geometry is not an
    // input region, and its top-left offset cannot predict the other edges.
    let csd_region = std::env::args().any(|arg| arg == "--csd-input-region");
    let (content_w, content_h) = if csd_region { (340, 230) } else { (400, 300) };
    if csd_region {
        xdg.set_window_geometry(25, 30, content_w, content_h);
        let region = probe.compositor.as_ref().unwrap().create_region(&qh, ());
        region.add(13, 18, 364, 254);
        region.subtract(180, 130, 20, 20);
        surface.set_input_region(Some(&region));
        region.destroy();
    }
    if std::env::args().any(|arg| arg == "stale-geometry-fullscreen") {
        // Set once and never restated, so it stays 400x300 whatever buffer
        // later answers a configure.
        xdg.set_window_geometry(0, 0, content_w, content_h);
    }
    toplevel.set_min_size(content_w, content_h);
    if !probe.interactive.enabled() && !std::env::args().any(|arg| arg == "resizable") {
        toplevel.set_max_size(content_w, content_h);
    }
    surface.commit();
    queue.roundtrip(&mut probe).expect("initial configure");

    let (width, height) = ((400.0 * scale) as i32, (300.0 * scale) as i32);
    let _viewport = if scale.fract() != 0.0 {
        let viewport = probe
            .viewporter
            .as_ref()
            .expect("wp_viewporter")
            .get_viewport(&surface, &qh, ());
        viewport.set_destination(400, 300);
        Some(viewport)
    } else {
        surface.set_buffer_scale(scale as i32);
        None
    };
    let path =
        std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("private runtime"))
            .join(format!("chonk-input-probe-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("exclusive scratch buffer");
    std::fs::remove_file(&path).expect("unlink scratch buffer");
    let bytes: Vec<u8> = if csd_region {
        csd_shadow_buffer(width, height, scale)
    } else {
        std::iter::repeat_n([0x40u8, 0x40, 0xC0, 0xFF], (width * height) as usize)
            .flatten()
            .collect()
    };
    file.write_all(&bytes).expect("fill buffer");
    let pool =
        probe
            .shm
            .as_ref()
            .expect("wl_shm")
            .create_pool(file.as_fd(), width * height * 4, &qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        width * 4,
        wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    let _child = if std::env::args().any(|arg| arg == "subsurface") {
        let child = probe
            .compositor
            .as_ref()
            .expect("compositor")
            .create_surface(&qh, ());
        let subsurface = probe
            .subcompositor
            .as_ref()
            .expect("subcompositor")
            .get_subsurface(&child, &surface, &qh, ());
        subsurface.set_position(40, 30);
        let viewport = if scale.fract() != 0.0 {
            let viewport =
                probe
                    .viewporter
                    .as_ref()
                    .expect("viewporter")
                    .get_viewport(&child, &qh, ());
            viewport.set_destination(200, 150);
            Some(viewport)
        } else {
            child.set_buffer_scale(scale as i32);
            None
        };
        let child_buffer = pool.create_buffer(
            0,
            width / 2,
            height / 2,
            width * 4,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        child.attach(Some(&child_buffer), 0, 0);
        child.damage_buffer(0, 0, width / 2, height / 2);
        child.commit();
        Some((child, subsurface, viewport, child_buffer))
    } else {
        None
    };
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
    queue.roundtrip(&mut probe).expect("map");
    say("mapped input-probe");
    if std::env::args().any(|arg| arg == "lagged-fullscreen") {
        probe.answer_with_old_buffer = true;
        toplevel.set_fullscreen(None);
        say("requested fullscreen");
    }
    if std::env::args().any(|arg| arg == "stale-geometry-fullscreen") {
        probe.stale_geometry = true;
        toplevel.set_fullscreen(None);
        say("requested fullscreen");
    }
    let _replacement_grab = replace_ime_grab.then(|| {
        let method = _input_method.as_ref().expect("input method");
        let first = method.grab_keyboard(&qh, 0);
        queue.roundtrip(&mut probe).expect("first IME grab");
        let second = method.grab_keyboard(&qh, 1);
        queue.roundtrip(&mut probe).expect("replacement IME grab");
        first.release();
        queue.roundtrip(&mut probe).expect("retired IME grab");
        say("ime replacement ready");
        second
    });
    probe.interactive.spawn_auto_request(
        connection.clone(),
        toplevel.clone(),
        probe.seat.as_ref().expect("seat").clone(),
    );
    while queue.blocking_dispatch(&mut probe).is_ok() {}
}
