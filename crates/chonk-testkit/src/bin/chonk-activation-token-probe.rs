//! Scripted `xdg_activation_v1` clients for the activation E2E tests.
//!
//! With no arguments this is the adversarial abandoned-token client: a
//! normal launcher asks for one token and passes it to the program it
//! starts, this one asks for many and activates nothing, the exact
//! client behavior that used to grow the compositor's token map for the
//! life of the session.
//!
//! The other modes each map one window and play one side of an
//! activation hand-off, so a test can watch where the keyboard goes:
//!
//! - `self-activate --on-blur` / `self-activate --on-file PATH`: mint a
//!   token with no serial and activate the probe's own window when the
//!   keyboard leaves it, or when `PATH` appears — the background client
//!   that wants attention it was not asked for.
//! - `mint-on-key PATH`: on the first key press, mint a token naming
//!   that press's serial and this window, and write it to `PATH` — the
//!   focused client a link was clicked in.
//! - `activate-from-file PATH`: activate this window with whatever
//!   token turns up in `PATH` — the client a token was handed to.
//! - `single-instance PATH`: the first instance maps a window and
//!   creates `PATH`; a second instance, finding `PATH`, writes its
//!   `XDG_ACTIVATION_TOKEN` (or `absent`) there and exits, and the first
//!   activates its window with it — how a single-instance application
//!   raises itself when launched again.
//! - `taskbar-activate NEEDLE`: a bar's click — foreign-toplevel
//!   `activate` on the window whose title contains `NEEDLE`, no token.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

const REQUESTS: usize = 512;
/// The window every mapping mode asks for; the compositor may answer
/// with another size, which the probe then draws instead.
const WINDOWED: (i32, i32) = (320, 240);
const APP_ID: &str = "chonk-activation-token-probe";
/// How often a mode waiting on a file or on its own keyboard state
/// looks again.
const TICK: Duration = Duration::from_millis(30);

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

fn fatal(message: &str) -> ! {
    eprintln!("chonk-activation-token-probe: {message}");
    std::process::exit(1);
}

enum Trigger {
    Blur,
    File(PathBuf),
}

enum Mode {
    Hoard,
    SelfActivate(Trigger),
    MintOnKey(PathBuf),
    ActivateFromFile(PathBuf),
    SingleInstance(PathBuf),
    TaskbarActivate(String),
}

fn parse_args() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |index: usize| args.get(index).map(String::as_str);
    match arg(0) {
        None => Mode::Hoard,
        Some("self-activate") => match (arg(1), arg(2)) {
            (Some("--on-blur"), None) => Mode::SelfActivate(Trigger::Blur),
            (Some("--on-file"), Some(path)) => Mode::SelfActivate(Trigger::File(path.into())),
            _ => fatal("self-activate wants --on-blur or --on-file PATH"),
        },
        Some("mint-on-key") => Mode::MintOnKey(arg(1).unwrap_or_else(|| fatal("mint-on-key wants PATH")).into()),
        Some("activate-from-file") => {
            Mode::ActivateFromFile(arg(1).unwrap_or_else(|| fatal("activate-from-file wants PATH")).into())
        }
        Some("single-instance") => {
            Mode::SingleInstance(arg(1).unwrap_or_else(|| fatal("single-instance wants PATH")).into())
        }
        Some("taskbar-activate") => {
            Mode::TaskbarActivate(arg(1).unwrap_or_else(|| fatal("taskbar-activate wants NEEDLE")).to_string())
        }
        Some(other) => fatal(&format!("unknown mode {other}")),
    }
}

#[derive(Default)]
struct Probe {
    activation: Option<XdgActivationV1>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    wm_base: Option<XdgWmBase>,
    foreign: Option<ZwlrForeignToplevelManagerV1>,
    /// Every foreign toplevel announced, with the title last reported.
    toplevels: Vec<(ZwlrForeignToplevelHandleV1, String)>,
    /// `done` events seen on token objects.
    completed: usize,
    /// The token string of the latest `done`.
    minted: Option<String>,
    /// The size the latest toplevel configure asked for.
    size: (i32, i32),
    /// A surface configure landed since the last buffer was attached.
    dirty: bool,
    /// The keyboard entered this window at some point.
    entered: bool,
    /// ...and left it again afterwards.
    left: bool,
    /// The serial of the first key press delivered to this window.
    key_serial: Option<u32>,
    closed: bool,
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
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "xdg_activation_v1" => probe.activation = Some(registry.bind(name, version.min(1), qh, ())),
            "wl_compositor" => probe.compositor = Some(registry.bind(name, version.min(4), qh, ())),
            "wl_shm" => probe.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" => probe.seat = Some(registry.bind(name, version.min(5), qh, ())),
            "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(2), qh, ())),
            "zwlr_foreign_toplevel_manager_v1" => {
                probe.foreign = Some(registry.bind(name, version.min(3), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            probe.completed += 1;
            probe.minted = Some(token);
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
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(capabilities) } = event {
            if capabilities.contains(wl_seat::Capability::Keyboard) && probe.keyboard.is_none() {
                probe.keyboard = Some(seat.get_keyboard(qh, ()));
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
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => {
                probe.entered = true;
                say("keyboard entered");
            }
            wl_keyboard::Event::Leave { .. } => {
                if probe.entered {
                    probe.left = true;
                    say("keyboard left");
                }
            }
            wl_keyboard::Event::Key { serial, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. }
                if probe.key_serial.is_none() =>
            {
                probe.key_serial = Some(serial);
                say(&format!("key pressed serial={serial}"));
            }
            _ => {}
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
            surface.ack_configure(serial);
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
            xdg_toplevel::Event::Configure { width, height, .. } => {
                probe.size = if width > 0 && height > 0 { (width, height) } else { WINDOWED };
            }
            xdg_toplevel::Event::Close => probe.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            probe.toplevels.push((toplevel, String::new()));
        }
    }

    wayland_client::event_created_child!(Probe, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(entry) = probe.toplevels.iter_mut().find(|(known, _)| known == handle) {
                    entry.1 = title;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                probe.toplevels.retain(|(known, _)| known != handle);
                handle.destroy();
            }
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),* $(,)?) => {$(
        impl Dispatch<$interface, ()> for Probe {
            fn event(_: &mut Self, _: &$interface, _: <$interface as Proxy>::Event, _: &(),
                     _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    XdgActivationV1,
    wl_compositor::WlCompositor,
    WlSurface,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer
);

/// A sealed-off scratch file holding one solid frame for the `wl_shm`
/// pool. Unlinked at once: the fd is the only handle.
fn frame_file(width: i32, height: i32) -> std::fs::File {
    let path = format!(
        "{}/chonk-activation-token-probe-{}",
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

/// One mapped window and the pieces needed to keep it drawn.
struct Window {
    surface: WlSurface,
    _xdg_surface: XdgSurface,
    _toplevel: XdgToplevel,
    shm: wl_shm::WlShm,
    attached: Option<wl_buffer::WlBuffer>,
}

impl Window {
    /// Maps a window titled `title`: commit the role, wait to be told a
    /// size, attach the first buffer — anything else is a protocol
    /// error.
    fn map(connection: &Connection, queue: &mut wayland_client::EventQueue<Probe>, probe: &mut Probe, title: &str) -> Window {
        let qh = queue.handle();
        let compositor = probe.compositor.clone().unwrap_or_else(|| fatal("no wl_compositor"));
        let wm_base = probe.wm_base.clone().unwrap_or_else(|| fatal("no xdg_wm_base"));
        let shm = probe.shm.clone().unwrap_or_else(|| fatal("no wl_shm"));
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.to_string());
        toplevel.set_app_id(APP_ID.to_string());
        surface.commit();
        probe.size = WINDOWED;
        let mut window = Window { surface, _xdg_surface: xdg_surface, _toplevel: toplevel, shm, attached: None };
        while !probe.dirty {
            queue.roundtrip(probe).unwrap_or_else(|error| fatal(&format!("initial configure: {error}")));
        }
        window.draw(probe, &qh);
        let _ = connection.flush();
        say(&format!("**mapped title={title:?}**"));
        window
    }

    /// Answers a configure with a buffer of the size it asked for.
    fn draw(&mut self, probe: &mut Probe, qh: &QueueHandle<Probe>) {
        probe.dirty = false;
        let (width, height) = probe.size;
        let file = frame_file(width, height);
        let stride = width.max(1) * 4;
        let pool = self.shm.create_pool(file.as_fd(), stride * height.max(1), qh, ());
        let buffer = pool.create_buffer(0, width.max(1), height.max(1), stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage(0, 0, width.max(1), height.max(1));
        self.surface.commit();
        if let Some(previous) = self.attached.replace(buffer) {
            previous.destroy();
        }
    }
}

/// One turn of a waiting mode: service the wire, redraw if asked, and
/// pause so the loop does not spin.
fn tick(queue: &mut wayland_client::EventQueue<Probe>, probe: &mut Probe, window: &mut Window) {
    queue.roundtrip(probe).unwrap_or_else(|error| fatal(&format!("roundtrip: {error}")));
    if probe.dirty {
        let qh = queue.handle();
        window.draw(probe, &qh);
    }
    std::thread::sleep(TICK);
}

/// Waits for a token object's `done`, which is where the string is.
fn await_token(queue: &mut wayland_client::EventQueue<Probe>, probe: &mut Probe) -> String {
    let before = probe.completed;
    while probe.completed == before {
        queue.roundtrip(probe).unwrap_or_else(|error| fatal(&format!("token roundtrip: {error}")));
    }
    probe.minted.clone().unwrap_or_else(|| fatal("done without a token"))
}

/// Writes `text` so a reader polling the path never sees half of it.
fn write_whole(path: &Path, text: &str) {
    let staging = path.with_extension("partial");
    std::fs::write(&staging, text).unwrap_or_else(|error| fatal(&format!("writing {}: {error}", staging.display())));
    std::fs::rename(&staging, path).unwrap_or_else(|error| fatal(&format!("renaming to {}: {error}", path.display())));
}

/// The token in `path`, once something has been written there.
fn handed_token(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

/// Sits on the window until the compositor closes it, so a test can
/// keep looking at where the keyboard went.
fn linger(queue: &mut wayland_client::EventQueue<Probe>, probe: &mut Probe, window: &mut Window) -> ! {
    while !probe.closed {
        tick(queue, probe, window);
    }
    std::process::exit(0);
}

fn main() {
    let mode = parse_args();
    let connection = Connection::connect_to_env().expect("connect to compositor");
    let display = connection.display();
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).expect("registry roundtrip");
    // A second roundtrip for the seat's capabilities, which arrive
    // after the bind.
    queue.roundtrip(&mut probe).expect("seat roundtrip");
    let activation = probe.activation.clone().expect("xdg-activation global");

    match mode {
        Mode::Hoard => {
            let mut tokens = Vec::with_capacity(REQUESTS);
            for _ in 0..REQUESTS {
                let token = activation.get_activation_token(&qh, ());
                token.commit();
                tokens.push(token);
            }
            queue.roundtrip(&mut probe).expect("token roundtrip");
            say(&format!("**requested {REQUESTS}; completed {}**", probe.completed));
            for token in tokens {
                token.destroy();
            }
        }
        Mode::SelfActivate(trigger) => {
            let mut window = Window::map(&connection, &mut queue, &mut probe, "activation-self");
            loop {
                let fired = match &trigger {
                    Trigger::Blur => probe.left,
                    Trigger::File(path) => path.exists(),
                };
                if fired {
                    break;
                }
                tick(&mut queue, &mut probe, &mut window);
            }
            // No serial, no surface: a token made out of nothing but the
            // wish to be looked at.
            let token = activation.get_activation_token(&qh, ());
            token.commit();
            let minted = await_token(&mut queue, &mut probe);
            token.destroy();
            activation.activate(minted, &window.surface);
            queue.roundtrip(&mut probe).expect("activate roundtrip");
            say("**activated own window with a serial-less token**");
            linger(&mut queue, &mut probe, &mut window);
        }
        Mode::MintOnKey(path) => {
            let seat = probe.seat.clone().expect("wl_seat global");
            let mut window = Window::map(&connection, &mut queue, &mut probe, "activation-mint");
            let serial = loop {
                if let Some(serial) = probe.key_serial {
                    break serial;
                }
                tick(&mut queue, &mut probe, &mut window);
            };
            let token = activation.get_activation_token(&qh, ());
            token.set_serial(serial, &seat);
            token.set_surface(&window.surface);
            token.commit();
            let minted = await_token(&mut queue, &mut probe);
            token.destroy();
            write_whole(&path, &minted);
            say(&format!("**minted a token for key serial {serial}**"));
            linger(&mut queue, &mut probe, &mut window);
        }
        Mode::ActivateFromFile(path) => {
            let mut window = Window::map(&connection, &mut queue, &mut probe, "activation-handed");
            let token = loop {
                if let Some(token) = handed_token(&path) {
                    break token;
                }
                tick(&mut queue, &mut probe, &mut window);
            };
            activation.activate(token, &window.surface);
            queue.roundtrip(&mut probe).expect("activate roundtrip");
            say("**activated with a handed token**");
            linger(&mut queue, &mut probe, &mut window);
        }
        Mode::SingleInstance(path) => {
            if path.exists() {
                // The second instance: hand over what the launcher gave
                // us and get out of the way, as `--new-window` on a
                // running browser does.
                let token = std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|token| !token.is_empty());
                write_whole(&path, token.as_deref().unwrap_or("absent"));
                say(&format!("**second instance token={}**", if token.is_some() { "present" } else { "absent" }));
                return;
            }
            write_whole(&path, "");
            let mut window = Window::map(&connection, &mut queue, &mut probe, "activation-single");
            let token = loop {
                if let Some(token) = handed_token(&path) {
                    break token;
                }
                tick(&mut queue, &mut probe, &mut window);
            };
            if token == "absent" {
                say("**second instance had no token**");
            } else {
                activation.activate(token, &window.surface);
                queue.roundtrip(&mut probe).expect("activate roundtrip");
                say("**activated with a handed token**");
            }
            linger(&mut queue, &mut probe, &mut window);
        }
        Mode::TaskbarActivate(needle) => {
            let seat = probe.seat.clone().expect("wl_seat global");
            if probe.foreign.is_none() {
                fatal("no zwlr_foreign_toplevel_manager_v1");
            }
            // The list arrives after the bind, its titles a roundtrip
            // later still.
            queue.roundtrip(&mut probe).expect("toplevel roundtrip");
            queue.roundtrip(&mut probe).expect("title roundtrip");
            let Some((handle, title)) = probe.toplevels.iter().find(|(_, title)| title.contains(&needle)) else {
                let titles: Vec<&str> = probe.toplevels.iter().map(|(_, title)| title.as_str()).collect();
                fatal(&format!("no toplevel titled like {needle:?}; saw {titles:?}"));
            };
            handle.activate(&seat);
            let title = title.clone();
            queue.roundtrip(&mut probe).expect("activate roundtrip");
            say(&format!("**taskbar activated {title:?}**"));
        }
    }
}
