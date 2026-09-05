//! The fullscreen e2e's scripted client: an xdg toplevel with a
//! fullscreen control, and a browser's memory of what it asked for.
//!
//! # Why this is not `foot`
//!
//! The regression this exists for is not "does fullscreen work" — the
//! compositor's own `alt+shift+f` always did. It is the *client-driven*
//! exchange: `xdg_toplevel.set_fullscreen`, the configure that answers
//! it, and what a client concludes from that answer. A terminal has no
//! scripted way to send those requests and no way to report what it was
//! told, so the test would have to infer both from geometry.
//!
//! # The client this pretends to be
//!
//! Chromium, exactly. A page's `requestFullscreen` opens a *fullscreen
//! session* in the browser and sends `set_fullscreen`; the next
//! configure is that request's answer. If the answer says the window is
//! not fullscreen, the request was refused and the session is dropped —
//! and a dropped session cannot be resurrected by a later configure,
//! because there is no longer a page asking for one. The page's exit
//! control then acts on the session: with no session there is nothing
//! to exit, so no `unset_fullscreen` is ever sent.
//!
//! That is not a caricature invented to fail a test. It is what a real
//! Microsoft Edge did on 2026-09-03, live, and what a `WAYLAND_DEBUG`
//! capture of Chromium against this compositor reproduced exactly: one
//! click on a page's fullscreen control fired `fullscreenchange` twice
//! (session opened, session dropped) while the compositor went and
//! stayed fullscreen, so the *second* click was the one that appeared
//! to work — and the click after that exited the page's fullscreen
//! without sending `unset_fullscreen`, leaving the desktop under an
//! invisible full-screen sheet that swallowed every click. See
//! `WaylandBackend::flush_configures` in `wm-wayland` for the trace and
//! the compositor-side bug behind it.
//!
//! # Usage
//!
//! `chonk-fullscreen-probe [title] [app-id] [animation-mode]` — then drive it
//! with injected keys through the test door, on the window once it has
//! keyboard focus. `animation-mode` is `animate` for a self-timed producer,
//! `animate-frame` for a conventional `wl_surface.frame`-paced one, or
//! `animate-inhibit-idle` for a self-timed producer which also binds the idle
//! inhibitor and notification protocols. `animate-duplicate-inhibit-idle`
//! creates two inhibitors on that surface and destroys exactly one, exercising
//! per-object removal. `absurd-geometry` instead declares
//! a hostile 600-million-pixel-wide xdg window geometry while committing the
//! ordinary 400x300 buffer; the compositor-survival E2E uses that mode:
//!
//! | key | evdev | meaning |
//! |---|---|---|
//! | `f` | 33 | the fullscreen control: enter if no session is open, exit if one is |
//! | `m` | 50 | the same control for maximize, whose request pair has the identical shape |
//!
//! Everything it is told and everything it concludes goes to stdout,
//! one line per event, flushed — the harness reads that log back as the
//! client's half of the exchange:
//!
//! ```text
//! configure 1280x800 states=[fullscreen, activated]
//! answer granted: asked fullscreen=true, told fullscreen=true
//! ```
//!
//! The optional `animate` mode independently reattaches, damages and commits
//! its retained buffer at about 60 Hz, without requesting a frame callback. It
//! exists to test the other half of compositor pacing: a client on a parked
//! workspace may continue on its own timer, but its invisible commits must not
//! make the compositor draw.
//! Every thirtieth commit is reported as `animation frame=N`, giving the
//! harness an observable clock that does not depend on compositor telemetry.
//! The frame-driven mode reports every callback as `frame callback=N`, which
//! lets the visibility test distinguish a parked client that truly sleeps from
//! one that keeps drawing invisible buffers behind an active workspace.

use std::io::Write;
use std::os::fd::AsFd;
use std::time::Duration;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback, wl_compositor::WlCompositor, wl_keyboard, wl_registry,
    wl_seat, wl_shm, wl_shm_pool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols::{
    ext::idle_notify::v1::client::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::ExtIdleNotifierV1,
    },
    wp::idle_inhibit::zv1::client::{
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    },
    wp::text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
};

/// evdev `KEY_F`: the page's fullscreen control.
const KEY_F: u32 = 33;
/// evdev `KEY_M`: the same gesture for maximize.
const KEY_M: u32 = 50;

/// The size the probe asks to be before anything has resized it. Small
/// enough to leave a visibly different rect to come back to when a
/// fullscreen is undone.
const WINDOWED: (i32, i32) = (400, 300);

fn fatal(message: &str) -> ! {
    eprintln!("chonk-fullscreen-probe: {message}");
    std::process::exit(1);
}

/// Says a line and makes sure it is on disk: the harness reads this log
/// while the process is still alive, so a buffered line is a line the
/// test cannot see.
fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

/// One window state this probe has an opinion about. Both members are
/// requested by the same request/answer pair shape, which is the whole
/// point of testing them together.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Fullscreen,
    Maximized,
}

impl Want {
    fn name(self) -> &'static str {
        match self {
            Want::Fullscreen => "fullscreen",
            Want::Maximized => "maximized",
        }
    }
}

#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    idle_notifier: Option<ExtIdleNotifierV1>,
    idle_inhibit_manager: Option<ZwpIdleInhibitManagerV1>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    surface: Option<WlSurface>,
    toplevel: Option<XdgToplevel>,
    /// The size the compositor's latest configure asked for, or
    /// [`WINDOWED`] until one names a size.
    size: (i32, i32),
    /// What the latest configure said the window is.
    told: Vec<Want>,
    /// The request whose answer has not arrived yet: the next configure
    /// is that answer, per the protocol.
    awaiting: Option<(Want, bool)>,
    /// The states this client currently has a *session* open for — the
    /// browser-shaped bit. See the module doc.
    sessions: Vec<Want>,
    /// Refusals seen, so the harness can assert on the count rather
    /// than parsing prose.
    refusals: u32,
    closed: bool,
    dirty: bool,
    /// A requested `wl_surface.frame` has not answered yet.
    frame_pending: bool,
    /// The most recent frame callback permits one new animation frame.
    frame_ready: bool,
    /// Exact callback count, reported for pacing regressions.
    frame_callbacks: u64,
    /// Held for the lifetime of `inhibit-idle` mode.
    idle_inhibitor: Option<ZwpIdleInhibitorV1>,
    /// Reports the compositor's real idle decision to the harness.
    idle_notification: Option<ExtIdleNotificationV1>,
    text_input: Option<ZwpTextInputV3>,
    input_method: Option<ZwpInputMethodV2>,
    text_input_entered: bool,
    text_input_enabled: bool,
    input_method_active: bool,
    ime_popups_requested: bool,
    /// Keep both the surfaces and their role objects alive so only the
    /// compositor's cap, not client-side destruction, bounds the test.
    ime_popups: Vec<(WlSurface, ZwpInputPopupSurfaceV2)>,
}

impl Probe {
    fn holds_session(&self, want: Want) -> bool {
        self.sessions.contains(&want)
    }

    /// The control a user clicks: enter the state if this client has no
    /// session for it, leave it if it does.
    fn control(&mut self, want: Want, qh: &QueueHandle<Self>) {
        let _ = qh;
        let Some(toplevel) = self.toplevel.clone() else {
            return;
        };
        if self.holds_session(want) {
            self.sessions.retain(|open| *open != want);
            self.awaiting = Some((want, false));
            match want {
                Want::Fullscreen => toplevel.unset_fullscreen(),
                Want::Maximized => toplevel.unset_maximized(),
            }
            say(&format!("control exit {}: sent unset", want.name()));
        } else {
            self.sessions.push(want);
            self.awaiting = Some((want, true));
            match want {
                Want::Fullscreen => toplevel.set_fullscreen(None),
                Want::Maximized => toplevel.set_maximized(),
            }
            say(&format!("control enter {}: sent set", want.name()));
        }
    }

    /// The answer arrived. Grant or refusal is decided by one question,
    /// and it is the question the protocol's own wording implies: does
    /// the configure answering my request describe the state I asked
    /// for?
    fn judge(&mut self) {
        let Some((want, asked)) = self.awaiting.take() else {
            return;
        };
        let told = self.told.contains(&want);
        if told == asked {
            say(&format!("answer granted: asked {}={asked}, told {}={told}", want.name(), want.name()));
            return;
        }
        self.refusals += 1;
        say(&format!("answer REFUSED: asked {}={asked}, told {}={told}", want.name(), want.name()));
        if asked && self.holds_session(want) {
            // A refused request ends the session it was made for.
            // Nothing later can reopen it: the page that asked has
            // already been told no.
            self.sessions.retain(|open| *open != want);
            say(&format!("{} session dropped after the refusal", want.name()));
        }
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
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => probe.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => probe.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_seat" => probe.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "ext_idle_notifier_v1" => {
                    probe.idle_notifier = Some(registry.bind(name, version.min(2), qh, ()))
                }
                "zwp_idle_inhibit_manager_v1" => {
                    probe.idle_inhibit_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_text_input_manager_v3" => {
                    probe.text_input_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_input_method_manager_v2" => {
                    probe.input_method_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // A client that does not pong is killed for being unresponsive,
        // which in a test reads as a mysterious disappearance.
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Probe {
    fn event(
        probe: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            // The toplevel configure that preceded this one carries the
            // content; the surface configure is where it becomes real,
            // so this is where a pending request has been answered.
            surface.ack_configure(serial);
            probe.judge();
            probe.dirty = true;
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
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                if width > 0 && height > 0 {
                    probe.size = (width, height);
                } else {
                    probe.size = WINDOWED;
                }
                // The states arrive as a packed array of little-endian
                // u32 enum values.
                let mut told = Vec::new();
                let mut names = Vec::new();
                for chunk in states.as_chunks::<4>().0 {
                    let value = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    match xdg_toplevel::State::try_from(value) {
                        Ok(xdg_toplevel::State::Fullscreen) => {
                            told.push(Want::Fullscreen);
                            names.push("fullscreen");
                        }
                        Ok(xdg_toplevel::State::Maximized) => {
                            told.push(Want::Maximized);
                            names.push("maximized");
                        }
                        Ok(xdg_toplevel::State::Activated) => names.push("activated"),
                        Ok(xdg_toplevel::State::Resizing) => names.push("resizing"),
                        Ok(xdg_toplevel::State::Suspended) => names.push("suspended"),
                        _ => names.push("other"),
                    }
                }
                probe.told = told;
                say(&format!(
                    "configure {}x{} states=[{}]",
                    probe.size.0,
                    probe.size.1,
                    names.join(", ")
                ));
            }
            xdg_toplevel::Event::Close => probe.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Probe {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(capabilities) } = event {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
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
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Keys arrive as evdev codes, which is what the test door
        // injects, so the two ends need no translation table between
        // them.
        if let wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } = event
        {
            match key {
                KEY_F => probe.control(Want::Fullscreen, qh),
                KEY_M => probe.control(Want::Maximized, qh),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            probe.frame_pending = false;
            probe.frame_ready = true;
            probe.frame_callbacks += 1;
            say(&format!("frame callback={}", probe.frame_callbacks));
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => say("idle state=idled"),
            ext_idle_notification_v1::Event::Resumed => say("idle state=resumed"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => probe.text_input_entered = true,
            zwp_text_input_v3::Event::Leave { .. } => probe.text_input_entered = false,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => probe.input_method_active = true,
            zwp_input_method_v2::Event::Deactivate => probe.input_method_active = false,
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Probe {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                     _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    WlBuffer,
    WlSurface,
    ExtIdleNotifierV1,
    ZwpIdleInhibitManagerV1,
    ZwpIdleInhibitorV1,
    ZwpTextInputManagerV3,
    ZwpInputMethodManagerV2,
    ZwpInputPopupSurfaceV2
);

/// A sealed-off scratch file holding one solid frame for the `wl_shm`
/// pool. Unlinked at once: the fd is the only handle.
fn frame_file(width: i32, height: i32) -> std::fs::File {
    let path = format!(
        "{}/chonk-fullscreen-probe-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|error| fatal(&format!("scratch file {path}: {error}")));
    let _ = std::fs::remove_file(&path);
    let pixels = (width.max(1) as usize) * (height.max(1) as usize);
    // Premultiplied opaque ARGB little-endian: B, G, R, A.
    let mut bytes = Vec::with_capacity(pixels * 4);
    for _ in 0..pixels {
        bytes.extend_from_slice(&[0x80, 0x40, 0x20, 0xFF]);
    }
    let mut writer = &file;
    writer.write_all(&bytes).unwrap_or_else(|error| fatal(&format!("filling the frame: {error}")));
    writer.flush().unwrap_or_else(|error| fatal(&format!("flushing the frame: {error}")));
    file
}

fn main() {
    let mut args = std::env::args().skip(1);
    let title = args.next().unwrap_or_else(|| "chonk-fullscreen-probe".to_string());
    let app_id = args.next().unwrap_or_else(|| "chonk-fullscreen-probe".to_string());
    let animation = args.next();
    let duplicate_inhibit = animation.as_deref() == Some("animate-duplicate-inhibit-idle");
    let ime_popup_flood = animation.as_deref() == Some("ime-popup-flood");
    let self_timed = matches!(
        animation.as_deref(),
        Some("animate" | "animate-inhibit-idle" | "animate-duplicate-inhibit-idle")
    );
    let frame_driven = animation.as_deref() == Some("animate-frame");
    let inhibit_idle = matches!(
        animation.as_deref(),
        Some("inhibit-idle" | "animate-inhibit-idle" | "animate-duplicate-inhibit-idle")
    );
    let absurd_geometry = animation.as_deref() == Some("absurd-geometry");

    let connection = Connection::connect_to_env()
        .unwrap_or_else(|error| fatal(&format!("no wayland display: {error}")));
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let _registry = connection.display().get_registry(&qh, ());

    let mut probe = Probe { size: WINDOWED, ..Probe::default() };
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|error| fatal(&format!("registry roundtrip: {error}")));

    if ime_popup_flood {
        let seat = probe.seat.as_ref().unwrap_or_else(|| fatal("no wl_seat"));
        let input_method_manager = probe
            .input_method_manager
            .as_ref()
            .unwrap_or_else(|| fatal("no zwp_input_method_manager_v2"));
        let text_input_manager = probe
            .text_input_manager
            .as_ref()
            .unwrap_or_else(|| fatal("no zwp_text_input_manager_v3"));
        // Create both before the window maps. Its ensuing keyboard-focus
        // edge then gives text-input the parent surface every popup needs.
        probe.input_method = Some(input_method_manager.get_input_method(seat, &qh, ()));
        probe.text_input = Some(text_input_manager.get_text_input(seat, &qh, ()));
    }

    let compositor = probe.compositor.clone().unwrap_or_else(|| fatal("no wl_compositor"));
    let wm_base = probe.wm_base.clone().unwrap_or_else(|| fatal("no xdg_wm_base"));
    let shm = probe.shm.clone().unwrap_or_else(|| fatal("no wl_shm"));
    // A second roundtrip for the seat's capabilities, which arrive
    // after the bind: without a keyboard there is no control to click.
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|error| fatal(&format!("seat roundtrip: {error}")));

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(title.clone());
    toplevel.set_app_id(app_id);
    if absurd_geometry {
        // The buffer below remains WINDOWED. This declaration is the
        // three-message session-kill shape from issue #36: older
        // ChonkStep releases trusted it as a decoration width and
        // panicked before this process could ever map.
        xdg_surface.set_window_geometry(0, 0, 600_000_000, 10);
        say("declared absurd window geometry 600000000x10");
    }
    surface.commit();
    probe.surface = Some(surface.clone());
    probe.toplevel = Some(toplevel);

    // The opening move: commit the role, wait to be told a size, then
    // attach the first buffer. Anything else is a protocol error.
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|error| fatal(&format!("initial configure: {error}")));

    let mut attached = (0, 0);
    let mut attached_buffer = None;
    let mut animation_frame = 0_u64;
    say(&format!("mapped title={title:?}"));
    while !probe.closed {
        if probe.dirty || attached != probe.size {
            let (width, height) = probe.size;
            let file = frame_file(width, height);
            let stride = width.max(1) * 4;
            let pool = shm.create_pool(file.as_fd(), stride * height.max(1), &qh, ());
            let buffer = pool.create_buffer(
                0,
                width.max(1),
                height.max(1),
                stride,
                wl_shm::Format::Argb8888,
                &qh,
                (),
            );
            surface.attach(Some(&buffer), 0, 0);
            surface.damage(0, 0, width.max(1), height.max(1));
            if frame_driven && !probe.frame_pending {
                surface.frame(&qh, ());
                probe.frame_pending = true;
            }
            surface.commit();
            attached_buffer = Some(buffer);
            if inhibit_idle && probe.idle_inhibitor.is_none() {
                let manager = probe
                    .idle_inhibit_manager
                    .as_ref()
                    .unwrap_or_else(|| fatal("no zwp_idle_inhibit_manager_v1"));
                let notifier = probe.idle_notifier.as_ref().unwrap_or_else(|| fatal("no ext_idle_notifier_v1"));
                let seat = probe.seat.as_ref().unwrap_or_else(|| fatal("no wl_seat"));
                let first = manager.create_inhibitor(&surface, &qh, ());
                if duplicate_inhibit {
                    probe.idle_inhibitor = Some(manager.create_inhibitor(&surface, &qh, ()));
                    first.destroy();
                    say("one duplicate idle inhibitor destroyed");
                } else {
                    probe.idle_inhibitor = Some(first);
                }
                probe.idle_notification = Some(notifier.get_idle_notification(250, seat, &qh, ()));
                say("idle inhibition armed");
            }
            pool.destroy();
            attached = probe.size;
            probe.dirty = false;
        }

        if ime_popup_flood && probe.text_input_entered && !probe.text_input_enabled {
            let text_input = probe.text_input.as_ref().unwrap();
            text_input.enable();
            text_input.commit();
            probe.text_input_enabled = true;
            connection.flush().unwrap_or_else(|error| fatal(&format!("enabling text input: {error}")));
        }
        if ime_popup_flood && probe.input_method_active && !probe.ime_popups_requested {
            let input_method = probe.input_method.as_ref().unwrap();
            for _ in 0..32 {
                let popup_surface = compositor.create_surface(&qh, ());
                let role = input_method.get_input_popup_surface(&popup_surface, &qh, ());
                probe.ime_popups.push((popup_surface, role));
            }
            probe.ime_popups_requested = true;
            queue
                .roundtrip(&mut probe)
                .unwrap_or_else(|error| fatal(&format!("creating IME popups: {error}")));
            say("32 input-method popups requested");
        }

        if self_timed {
            // A deliberately self-timed client: reattach and damage
            // its retained buffer, then commit without a frame
            // callback. Reattaching is important here: a commit that
            // carries only a frame request or surface damage but no
            // new buffer may correctly produce no repaint, which is
            // the empty-frame case this compositor must not turn into
            // a presentation boundary. A roundtrip makes each real
            // buffer commit's server-side delivery observable before
            // the next one, while the sleep is the producer's cadence
            // (not a test wait).
            let (width, height) = probe.size;
            surface.attach(attached_buffer.as_ref(), 0, 0);
            surface.damage(0, 0, width.max(1), height.max(1));
            surface.commit();
            animation_frame += 1;
            if animation_frame.is_multiple_of(30) {
                say(&format!("animation frame={animation_frame}"));
            }
            if queue.roundtrip(&mut probe).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        } else if frame_driven && probe.frame_ready {
            // A conventional animation loop: one newly attached frame
            // only when the compositor says the previous frame was
            // presented, and one callback booked atomically with that
            // commit.
            probe.frame_ready = false;
            let (width, height) = probe.size;
            surface.attach(attached_buffer.as_ref(), 0, 0);
            surface.damage(0, 0, width.max(1), height.max(1));
            surface.frame(&qh, ());
            probe.frame_pending = true;
            surface.commit();
            animation_frame += 1;
            if animation_frame.is_multiple_of(30) {
                say(&format!("animation frame={animation_frame}"));
            }
            connection.flush().unwrap_or_else(|error| fatal(&format!("flushing animation frame: {error}")));
        } else if queue.blocking_dispatch(&mut probe).is_err() {
            break;
        }
    }
    say("closed");
}
