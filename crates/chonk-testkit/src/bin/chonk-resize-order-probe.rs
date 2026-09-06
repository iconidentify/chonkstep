//! A client whose old rendered commit crosses a compositor-initiated resize.
//! The private Hyprland IPC reply orders the request before that commit; the
//! next xdg configure must still describe the requested size. No synthetic
//! compositor state, sleeps in the server, or test-only dispatch hooks.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Probe {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    requested: bool,
}

fn say(message: &str) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
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
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => probe.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_shm" => probe.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => probe.wm = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Probe {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            say(&format!(
                "configure {width} {height} requested={}",
                probe.requested
            ));
        }
    }
}

macro_rules! ignore {
    ($($ty:ty),* $(,)?) => { $(
        impl Dispatch<$ty, ()> for Probe {
            fn event(_: &mut Self, _: &$ty, _: <$ty as wayland_client::Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )* };
}
ignore!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface
);

fn main() {
    let act = PathBuf::from(std::env::args().nth(1).expect("private act marker"));
    let connection = Connection::connect_to_env().unwrap();
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).unwrap();
    let surface = probe.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = probe
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("resize-order-probe".into());
    toplevel.set_title("resize-order-probe".into());
    xdg.set_window_geometry(0, 0, 400, 300);
    surface.commit();
    queue.roundtrip(&mut probe).unwrap();

    let path = act.with_extension("shm");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file.write_all(&vec![0x90; 400 * 300 * 4]).unwrap();
    let pool = probe
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 400 * 300 * 4, &qh, ());
    let buffer = pool.create_buffer(0, 400, 300, 400 * 4, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 400, 300);
    surface.commit();
    queue.roundtrip(&mut probe).unwrap();
    say("mapped resize-order-probe");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !act.exists() {
        assert!(
            Instant::now() < deadline,
            "waiting for private resize marker"
        );
        queue.roundtrip(&mut probe).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    // Drain initial map/activation configures before classifying the answer.
    queue.roundtrip(&mut probe).unwrap();
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").unwrap();
    let socket = runtime.join("hypr").join(signature).join(".socket.sock");
    let mut ipc = UnixStream::connect(socket).unwrap();
    ipc.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    ipc.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    ipc.write_all(b"/dispatch resizeactive 120 80").unwrap();
    let mut response = String::new();
    ipc.read_to_string(&mut response).unwrap();
    assert_eq!(response, "ok");
    // This is deliberately the previously rendered content, committed before
    // the client has read/acked any configure answering that resize. The
    // compositor may not interpret it as a new request to undo the resize.
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 400, 300);
    surface.commit();
    connection.flush().unwrap();
    probe.requested = true;
    say("old rendered commit crossed resize request");
    loop {
        queue.blocking_dispatch(&mut probe).unwrap();
    }
}
