//! Minimal native client reporting the coordinates delivered by the seat.
//!
//! A real committed buffer scale / viewport is essential: a compositor-only
//! hit-test assertion cannot see a click grab retaining an obsolete origin.

#[path = "chonk-input-probe/constraints.rs"]
mod constraints;
#[path = "chonk-input-probe/interactive.rs"]
mod interactive;

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use wayland_client::protocol::{
    wl_buffer,
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
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
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
    position: (f64, f64),
    sequence: u64,
}

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
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                say(if Some(&surface) == probe.surface.as_ref() {
                    "entered root"
                } else {
                    "entered subsurface"
                });
                probe.pointer_surface = Some(surface);
                probe.position = (surface_x, surface_y);
                probe.report("enter");
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
                    probe.data_device.as_ref().expect("data device").start_drag(
                        Some(&source),
                        probe.pointer_surface.as_ref().expect("pointer surface"),
                        None,
                        serial,
                    );
                    say("started internal drag");
                }
            }
            wl_pointer::Event::Leave { .. } => probe.report("leave"),
            _ => {}
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
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
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
        _: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            say(&format!("surface configure {serial}"));
            surface.ack_configure(serial);
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
    XdgToplevel,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    WpViewporter,
    WpViewport
);

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
    toplevel.set_min_size(content_w, content_h);
    if !probe.interactive.enabled() {
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
    let bytes: Vec<u8> = std::iter::repeat_n([0x40u8, 0x40, 0xC0, 0xFF], (width * height) as usize)
        .flatten()
        .collect();
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
