//! The layer-shell e2e's scripted bar.
//!
//! The smallest client that can claim an edge: one `wl_surface` with
//! the `zwlr_layer_surface_v1` role, anchored TOP|LEFT|RIGHT like
//! every bar, `height` pixels tall, with an exclusive zone of the same
//! height, filled a solid colour so a screenshot can find it. It then
//! sits in its dispatch loop until it is killed or the compositor
//! goes away — a bar's whole life, minus the clock.
//!
//! What the tests measure through it is the compositor's side of the
//! exclusive-zone contract: where the Dock hangs itself while the bar
//! is up, where a maximized window stops, and that both go back when
//! the bar exits. Omarchy's real bar is a Quickshell process a test
//! cannot reasonably boot; this one speaks the same four requests.
//!
//! Usage: `chonk-fake-bar <height> [top|right] [namespace]` — `right`
//! anchors the bar as a right-edge panel `height` pixels wide instead;
//! the namespace defaults to `chonk-fake-bar`, and is how a test stands
//! in for Omarchy's bar (`omarchy-bar`), whose visibility the desk
//! decides by that name.
//!
//! `chonk-fake-bar background <namespace>` is the other shape a layer
//! client takes: a surface on the `background` layer anchored to all
//! four edges with no exclusive zone, the way a wallpaper daemon — or
//! Omarchy's Background plugin, namespace `omarchy-background` — covers
//! the desk. The compositor declines exactly that namespace on that
//! layer, and the test that pins it needs both a surface it must show
//! and one it must not, differing only in the name.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

/// Premultiplied opaque ARGB, little-endian per pixel: a strong
/// orange nothing in the shell's palettes comes near.
const BAR_ORANGE: [u8; 4] = [0x10, 0x70, 0xE0, 0xFF];

fn fatal(message: &str) -> ! {
    eprintln!("chonk-fake-bar: {message}");
    std::process::exit(1);
}

#[derive(Default)]
struct Bar {
    compositor: Option<WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    /// The size the compositor's latest configure asked for; `None`
    /// until the first one lands.
    configured: Option<(u32, u32)>,
    closed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Bar {
    fn event(
        bar: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => bar.compositor = Some(registry.bind(name, version.min(4), qh, ())),
                "wl_shm" => bar.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => bar.layer_shell = Some(registry.bind(name, version.min(4), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Bar {
    fn event(
        bar: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                bar.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => bar.closed = true,
            _ => {}
        }
    }
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Bar {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                     _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlCompositor, wl_shm::WlShm, wl_shm_pool::WlShmPool, WlBuffer, WlSurface, ZwlrLayerShellV1);

/// A sealed-off scratch file holding one solid-colour frame, for the
/// `wl_shm` pool. Unlinked at once: the fd is the only handle.
fn frame_file(width: u32, height: u32) -> std::fs::File {
    let path = format!(
        "{}/chonk-fake-bar-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id()
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|e| fatal(&format!("cannot create the frame file: {e}")));
    let _ = std::fs::remove_file(&path);
    let pixels = BAR_ORANGE.repeat((width * height) as usize);
    file.write_all(&pixels).unwrap_or_else(|e| fatal(&format!("cannot fill the frame file: {e}")));
    file
}

/// The three shapes the client takes, from its arguments.
enum Shape {
    /// A top bar `thickness` tall, or a right panel `thickness` wide,
    /// under the given namespace.
    Edge { thickness: u32, right_panel: bool, namespace: String },
    /// A wallpaper: the whole output, on the background layer, under
    /// the given namespace.
    Background { namespace: String },
}

fn parse_args() -> Shape {
    let mut args = std::env::args().skip(1);
    let usage = "usage: chonk-fake-bar <height> [top|right] [namespace] | chonk-fake-bar background <namespace>";
    match args.next().as_deref() {
        Some("background") => Shape::Background { namespace: args.next().unwrap_or_else(|| fatal(usage)) },
        Some(thickness) => {
            let thickness = thickness.parse().unwrap_or_else(|_| fatal(usage));
            let right_panel = match args.next().as_deref() {
                None | Some("top") => false,
                Some("right") => true,
                Some(_) => fatal(usage),
            };
            let namespace = args.next().unwrap_or_else(|| "chonk-fake-bar".to_string());
            Shape::Edge { thickness, right_panel, namespace }
        }
        None => fatal(usage),
    }
}

fn main() {
    let shape = parse_args();

    let conn = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("no compositor: {e}")));
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut bar = Bar::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut bar).unwrap_or_else(|e| fatal(&format!("registry roundtrip failed: {e}")));
    let (Some(compositor), Some(shm), Some(layer_shell)) = (&bar.compositor, &bar.shm, &bar.layer_shell) else {
        fatal("the compositor lacks wl_compositor, wl_shm or zwlr_layer_shell_v1");
    };
    let (compositor, shm, layer_shell) = (compositor.clone(), shm.clone(), layer_shell.clone());

    let surface = compositor.create_surface(&qh, ());
    let (layer_kind, namespace) = match &shape {
        Shape::Edge { namespace, .. } => (zwlr_layer_shell_v1::Layer::Top, namespace.clone()),
        Shape::Background { namespace } => (zwlr_layer_shell_v1::Layer::Background, namespace.clone()),
    };
    let layer = layer_shell.get_layer_surface(&surface, None, layer_kind, namespace, &qh, ());
    match shape {
        Shape::Edge { thickness, right_panel: true, .. } => {
            layer.set_anchor(Anchor::Right | Anchor::Top | Anchor::Bottom);
            layer.set_size(thickness, 0);
            layer.set_exclusive_zone(thickness as i32);
        }
        Shape::Edge { thickness, right_panel: false, .. } => {
            layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
            layer.set_size(0, thickness);
            layer.set_exclusive_zone(thickness as i32);
        }
        Shape::Background { .. } => {
            // Every edge, no size of its own, and -1: "do not move me
            // for anyone's exclusive zone" — Quickshell's
            // `exclusionMode: Ignore`, which is what Omarchy's plugin sets.
            layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_size(0, 0);
            layer.set_exclusive_zone(-1);
        }
    }
    surface.commit();

    // The first configure carries the stretched dimension; the client
    // blocks on it by protocol, so waiting here is not optional.
    while bar.configured.is_none() && !bar.closed {
        queue.blocking_dispatch(&mut bar).unwrap_or_else(|e| fatal(&format!("waiting for configure: {e}")));
    }
    let Some((width, height)) = bar.configured else { fatal("closed before the first configure") };
    let (width, height) = (width.max(1), height.max(1));

    let file = frame_file(width, height);
    let pool = shm.create_pool(file.as_fd(), (width * height * 4) as i32, &qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, (width * 4) as i32, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
    queue.roundtrip(&mut bar).unwrap_or_else(|e| fatal(&format!("mapping the bar: {e}")));
    // The test reads this line to know the bar is up.
    println!("mapped {width}x{height}");

    // A bar's life: dispatch until told otherwise.
    while !bar.closed {
        if queue.blocking_dispatch(&mut bar).is_err() {
            break;
        }
    }
}
