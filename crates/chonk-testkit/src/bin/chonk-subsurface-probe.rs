//! A `wl_subcompositor` client, in two halves.
//!
//! The honest half (`stack`) maps a toplevel with one subsurface under
//! it and one over it, in colours a screenshot can tell apart. That is
//! the shape the compositor's own scene walk has to keep in order: a
//! surface's z-position among its children is a real protocol fact —
//! `wl_subsurface.place_below` puts a video under its parent's chrome —
//! and it is the fact an iterative rewrite of a recursive tree walk is
//! most likely to lose.
//!
//! The hostile half (`deep-chain`, `deep-chain-root`) builds a
//! subsurface chain past the compositor's depth ceiling, in both of the
//! orders a client can build one. `deep-chain` is the cheap one:
//! attaching leaf-first — S1 under S2, then S2 under S3 — presents a
//! parent that has no parent of its own on every single call, so a
//! compositor that bounds only the distance from the new child to its
//! root reads zero every time and never fires. On an unbounded
//! compositor the chain simply grows until a commit walks it off the
//! end of the stack; here the link past the ceiling must be refused
//! with a protocol error, and the chain exactly at the ceiling must
//! still be accepted.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_buffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm_pool,
    wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-subsurface-probe: {message}");
    std::process::exit(1);
}

/// Says a line and makes sure it is on disk: the harness reads this log
/// while the process is still alive, so a buffered line is a line the
/// test cannot see.
fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<XdgWmBase>,
    /// The size the compositor last configured, when it named one.
    size: (i32, i32),
    configured: bool,
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
                "wl_subcompositor" => {
                    probe.subcompositor = Some(registry.bind(name, version.min(1), qh, ()))
                }
                "wl_shm" => probe.shm = Some(registry.bind(name, version.min(1), qh, ())),
                "xdg_wm_base" => probe.wm_base = Some(registry.bind(name, version.min(3), qh, ())),
                _ => {}
            }
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
        // An unanswered ping is a kill, and this probe is meant to be
        // killed for exactly one reason.
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
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            probe.configured = true;
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
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            if width > 0 && height > 0 {
                probe.size = (width, height);
            }
        }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Probe {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    WlCompositor,
    WlSubcompositor,
    WlSurface,
    WlSubsurface,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
);

/// One opaque single-colour `wl_buffer`, in premultiplied
/// little-endian ARGB — the order `wl_shm` means by `Argb8888`.
fn color_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<Probe>,
    serial: u32,
    width: i32,
    height: i32,
    bgra: [u8; 4],
) -> wl_buffer::WlBuffer {
    let path = format!(
        "{}/chonk-subsurface-probe-{}-{serial}",
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
    let (width, height) = (width.max(1), height.max(1));
    let bytes: Vec<u8> = std::iter::repeat_n(bgra, (width * height) as usize).flatten().collect();
    let mut writer = &file;
    writer
        .write_all(&bytes)
        .unwrap_or_else(|error| fatal(&format!("filling the buffer: {error}")));
    writer
        .flush()
        .unwrap_or_else(|error| fatal(&format!("flushing the buffer: {error}")));
    let pool = shm.create_pool(file.as_fd(), width * height * 4, qh, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    buffer
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| fatal("usage: chonk-subsurface-probe <mode> [...]"));

    let connection = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("connect: {e}")));
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|error| fatal(&format!("registry roundtrip: {error}")));

    let compositor = probe.compositor.clone().unwrap_or_else(|| fatal("no wl_compositor"));
    let subcompositor = probe
        .subcompositor
        .clone()
        .unwrap_or_else(|| fatal("no wl_subcompositor"));

    match mode.as_str() {
        "stack" => {
            let title = args.next().unwrap_or_else(|| "subsurface-stack".to_string());
            stack(&mut queue, &mut probe, &compositor, &subcompositor, title);
        }
        "deep-chain" | "deep-chain-root" => {
            let limit: usize = args
                .next()
                .unwrap_or_else(|| fatal("the depth limit is the second argument"))
                .parse()
                .unwrap_or_else(|error| fatal(&format!("depth limit: {error}")));
            deep_chain(
                &mut queue,
                &mut probe,
                &compositor,
                &subcompositor,
                limit,
                mode == "deep-chain",
            );
        }
        other => fatal(&format!("unknown mode {other:?}")),
    }
}

/// The honest half: a toplevel with a subsurface below it and another
/// above it, then hold the window open for the test to photograph.
fn stack(
    queue: &mut wayland_client::EventQueue<Probe>,
    probe: &mut Probe,
    compositor: &WlCompositor,
    subcompositor: &WlSubcompositor,
    title: String,
) {
    let qh = queue.handle();
    let shm = probe.shm.clone().unwrap_or_else(|| fatal("no wl_shm"));
    let wm_base = probe.wm_base.clone().unwrap_or_else(|| fatal("no xdg_wm_base"));

    let parent = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&parent, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(title.clone());
    toplevel.set_app_id("chonk-subsurface-probe".to_string());
    parent.commit();
    queue
        .roundtrip(probe)
        .unwrap_or_else(|error| fatal(&format!("initial configure: {error}")));
    if !probe.configured {
        fatal("the compositor never configured the toplevel");
    }
    let (width, height) = if probe.size.0 > 0 { probe.size } else { (300, 300) };

    // Red is the parent's own pixels. The green sheet under it is the
    // same size, so any pixel of the window that comes back green means
    // a below-subsurface was drawn above its parent. The blue patch over
    // it is small and in the corner, so a red corner means an
    // above-subsurface was drawn below its parent. One screenshot
    // decides both.
    let parent_buffer = color_buffer(&shm, &qh, 0, width, height, [0x20, 0x20, 0xE0, 0xFF]);
    let under_buffer = color_buffer(&shm, &qh, 1, width, height, [0x20, 0xE0, 0x20, 0xFF]);
    let over_buffer = color_buffer(&shm, &qh, 2, 80, 80, [0xE0, 0x20, 0x20, 0xFF]);

    let under = compositor.create_surface(&qh, ());
    let under_role = subcompositor.get_subsurface(&under, &parent, &qh, ());
    under_role.set_position(0, 0);
    under_role.place_below(&parent);
    under.attach(Some(&under_buffer), 0, 0);
    under.damage(0, 0, width, height);
    under.commit();

    let over = compositor.create_surface(&qh, ());
    let over_role = subcompositor.get_subsurface(&over, &parent, &qh, ());
    over_role.set_position(0, 0);
    over_role.place_above(&parent);
    over.attach(Some(&over_buffer), 0, 0);
    over.damage(0, 0, 80, 80);
    over.commit();

    // Both subsurfaces are synchronized, which is the default and the
    // whole point: their state lands when the parent commits, through
    // the same `commit_sync_surface_tree` walk the depth bound exists to
    // protect.
    parent.attach(Some(&parent_buffer), 0, 0);
    parent.damage(0, 0, width, height);
    parent.commit();
    queue
        .roundtrip(probe)
        .unwrap_or_else(|error| fatal(&format!("mapping roundtrip: {error}")));
    say(&format!("mapped title={title:?} {width}x{height} with 2 subsurfaces"));

    loop {
        if queue.blocking_dispatch(probe).is_err() {
            return;
        }
    }
}

/// The hostile half. `leaf_first` picks the construction order: leaf
/// first grows the chain upward from a leaf (every parent is a fresh
/// root), root first grows it downward from a root (every child is a
/// fresh leaf). Both must be accepted up to `limit` links and refused
/// at `limit + 1`.
fn deep_chain(
    queue: &mut wayland_client::EventQueue<Probe>,
    probe: &mut Probe,
    compositor: &WlCompositor,
    subcompositor: &WlSubcompositor,
    limit: usize,
    leaf_first: bool,
) {
    let qh = queue.handle();
    let order = if leaf_first { "leaf-first" } else { "root-first" };
    let chain: Vec<WlSurface> = (0..=limit + 1).map(|_| compositor.create_surface(&qh, ())).collect();

    // `chain[i]` sits `i` links above the bottom either way; only the
    // order the links are requested in differs.
    let link = |child: usize, parent: usize| {
        subcompositor.get_subsurface(&chain[child], &chain[parent], &qh, ());
    };
    for step in 0..limit {
        if leaf_first {
            link(step, step + 1);
        } else {
            link(limit - 1 - step, limit - step);
        }
    }
    if let Err(error) = queue.roundtrip(probe) {
        fatal(&format!("a chain of {limit} links ({order}) was refused: {error}"));
    }
    say(&format!("**{order} chain of {limit} links accepted**"));

    // One link too many. The compositor must answer this with a
    // protocol error and nothing else.
    link(limit, limit + 1);
    match queue.roundtrip(probe) {
        Ok(_) => say(&format!("**{order} chain of {} links accepted**", limit + 1)),
        Err(error) => say(&format!("**{order} refused at {} links: {error}**", limit + 1)),
    }
    // Whatever happened, report what the connection does next: a
    // refusal that does not actually disconnect would leave the chain
    // in the tree and the compositor still walking it.
    match queue.roundtrip(probe) {
        Ok(_) => say("**connection still alive**"),
        Err(error) => say(&format!("**connection closed: {error}**")),
    }
}
