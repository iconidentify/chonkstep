//! Changes a mapped surface density through real buffer-scale/viewport commits.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Probe {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    size: Option<(i32, i32)>,
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
                "wp_viewporter" => probe.viewporter = Some(registry.bind(name, 1, qh, ())),
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
            if width > 0 && height > 0 {
                probe.size = Some((width, height));
            }
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
    wl_surface::WlSurface,
    wp_viewporter::WpViewporter,
    wp_viewport::WpViewport
);

fn main() {
    let act = PathBuf::from(std::env::args().nth(1).expect("private act marker"));
    let mode = std::env::args().nth(2).expect("buffer or viewport");
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
    toplevel.set_app_id("scale-change-probe".into());
    toplevel.set_title("scale-change-probe".into());
    xdg.set_window_geometry(0, 0, 400, 200);
    surface.commit();
    queue.roundtrip(&mut probe).unwrap();

    let viewport = probe
        .viewporter
        .as_ref()
        .unwrap()
        .get_viewport(&surface, &qh, ());
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut logical = (400, 200);
    let mut density = 1;
    let mut stage = String::new();
    let mut dirty = true;
    loop {
        assert!(Instant::now() < deadline, "probe timed out");
        queue.roundtrip(&mut probe).unwrap();
        if let Some(size) = probe.size.take() {
            if size != logical {
                logical = size;
                dirty = true;
            }
        }
        let next = std::fs::read_to_string(&act).unwrap_or_default();
        if next != stage {
            stage = next;
            let next_density = if stage.trim() == "2" { 2 } else { 1 };
            if mode == "viewport-destination" {
                // Change only destination/window coordinates, retaining the
                // raw buffer dimensions. This is distinct from increasing
                // density by submitting a larger buffer at the same destination.
                logical = (
                    logical.0 * density / next_density,
                    logical.1 * density / next_density,
                );
            }
            density = next_density;
            dirty = true;
        }
        if dirty {
            let (w, h) = (logical.0 * density, logical.1 * density);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&[0x31, 0xe7, 0x17, 0xff].repeat((w * h) as usize))
                .unwrap();
            let pool = probe
                .shm
                .as_ref()
                .unwrap()
                .create_pool(file.as_fd(), w * h * 4, &qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
            if mode.starts_with("viewport") {
                viewport.set_destination(logical.0, logical.1);
            } else {
                surface.set_buffer_scale(density);
            }
            xdg.set_window_geometry(0, 0, logical.0, logical.1);
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, w, h);
            surface.commit();
            queue.roundtrip(&mut probe).unwrap();
            buffer.destroy();
            pool.destroy();
            say(&format!(
                "committed {} {} density={density}",
                logical.0, logical.1
            ));
            dirty = false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
