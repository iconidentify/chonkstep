//! The session-lock e2e's scripted locker — a client shaped like
//! Quickshell, which is the shape that found the bug this probe
//! exists to pin: ONE connection that both holds a wlr-layer-shell
//! surface (Omarchy's bar, its OSDs) and takes the ext-session-lock.
//! A protocol error posted on any object — including a defunct lock
//! surface — kills the whole connection, bar and all, which is
//! exactly what happened to `omarchy-shell` on the live desktop the
//! moment a lock→PAM→unlock cycle completed (Sep 2 13:14:20: `The
//! Wayland connection broke. Did the Wayland compositor die?`).
//!
//! The teardown it performs is not invented here: it is the byte
//! sequence the real Quickshell emits, captured from the installed
//! `/usr/bin/qs` running Omarchy's own `plugins/lock/Service.qml`
//! (`WlSessionLock` + `WlSessionLockSurface`) against a nested
//! chonkstep, `WAYLAND_DEBUG=1`:
//!
//! ```text
//!  -> ext_session_lock_v1#25.unlock_and_destroy()
//!  -> ext_session_lock_surface_v1#36.destroy()
//!  -> wl_surface#34.attach(nil, 0, 0)
//!  -> wl_surface#34.commit()          <- the commit that killed the shell
//!  -> wl_surface#34.destroy()
//! ```
//!
//! That third and fourth line are Qt unmapping a window it is about to
//! drop — the same destroy-the-role-then-commit pattern `layers.rs`
//! already guards for layer surfaces. smithay 0.7.0's session-lock
//! pre-commit hook outlives the destroyed role exactly as its
//! layer-shell one does, and answers that unmap commit with a fatal
//! `null_buffer` error on the object the client already destroyed —
//! which kills the connection, bar and all.
//!
//! The same capture settles what the second lock of a session looks
//! like: Quickshell destroys the lock surface's `wl_surface` with the
//! role and creates a **fresh** one for the next lock (`wl_surface#34`
//! for the first, a newly created surface for the second). That is
//! also the only legal shape — ext-session-lock-v1: "Providing a
//! wl_surface which already has a role or already has a buffer
//! attached or committed is a protocol error". So cycles one and two
//! here are the real client's shape, and the third cycle is the
//! hostile one: a re-lock on a `wl_surface` that has already worn the
//! role, which must not be allowed to blank the session with a locker
//! that can never draw (see `lock::prime_reused_lock_surface`).
//!
//! The script, each step reported on stdout for the test to poll —
//! the checkpoint each one prints in `**bold**`:
//!
//! 1. Map a layer bar that holds the keyboard; Omarchy's popouts and
//!    its own lock preview take exclusive interactivity the same way.
//!    **`layer mapped WxH`**, **`bar holds the keyboard`**
//! 2. Lock, draw, await `locked`. **`locked WxH`**
//! 3. `unlock_and_destroy`, destroy the lock surface role, commit the
//!    kept `wl_surface` with a nil buffer — the teardown captured
//!    above, minus its final `destroy` so that step 7 can re-lock on
//!    this surface. **`survived the unlock teardown`**
//! 4. New frame and frame callback on the bar: the connection is not
//!    merely unkilled but serviced.
//!    **`layer surface serviced after unlock`**
//! 5. The keyboard comes home to the bar.
//!    **`bar has the keyboard back`**
//! 6. Lock again on a FRESH `wl_surface`, Quickshell's own shape, and
//!    tear it down in full, `wl_surface.destroy()` included.
//!    **`relocked WxH`**, **`survived the second unlock teardown`**
//! 7. Lock a third time on the surface step 3 kept, which already wore
//!    the lock-surface role: the mandatory first configure must still
//!    arrive. **`relocked on a reused surface WxH`**,
//!    **`survived the third unlock teardown`**
//!
//! Run with `--hold` the script stops after step 2 and keeps the lock
//! (**`holding the lock`**) instead of ever unlocking: that is the
//! standing lock `chonk-lock-thief` is pointed at, and a bypass can
//! only be attempted against a lock that is actually in force.
//! `--recovery-hold` skips the pre-lock layer-surface exercise and
//! requests the lock immediately, as a locker launched into the
//! compositor's already-blank crash-recovery domain must.
//!
//! Then it sits in its dispatch loop like any shell process would. A
//! broken connection at any step prints `connection broke: ...` (with
//! the protocol error, if one was posted) and exits 2.

use std::io::Write;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback, wl_compositor::WlCompositor, wl_keyboard::{self, WlKeyboard},
    wl_output::WlOutput, wl_registry, wl_seat::{self, WlSeat}, wl_shm, wl_shm_pool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

/// The bar's fill — the testkit's fixture orange, so a screenshot
/// assertion could find it, though the lock test's assertions are all
/// about liveness. Premultiplied opaque ARGB, little-endian: B, G, R, A.
const BAR_ORANGE: [u8; 4] = [0x10, 0x70, 0xE0, 0xFF];
/// The lock screen's fill: a navy no wallpaper in the harness wears.
const LOCK_NAVY: [u8; 4] = [0x40, 0x18, 0x08, 0xFF];

/// The bar's thickness, matching nothing else on the desk.
const BAR_HEIGHT: u32 = 24;

fn fatal(message: &str) -> ! {
    eprintln!("chonk-lock-probe: {message}");
    std::process::exit(1);
}

#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    output: Option<WlOutput>,
    seat: Option<WlSeat>,
    layer_shell: Option<ZwlrLayerShellV1>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    /// The `wl_surface` the keyboard last entered, and whether it has
    /// left since — the client-side view of "who holds the keyboard",
    /// which is how this probe sees the lock take the seat away from
    /// its bar and (the assertion that matters) give it back.
    keyboard_on: Option<u32>,
    /// The size the bar's latest layer-shell configure asked for.
    layer_configured: Option<(u32, u32)>,
    layer_closed: bool,
    /// The size the latest lock-surface configure asked for (already
    /// acked by the handler — the buffer committed must match it).
    lock_configured: Option<(u32, u32)>,
    /// `locked` received for the current lock.
    locked: bool,
    /// `finished` received — the compositor refused or revoked a lock.
    lock_finished: bool,
    /// A frame callback on the bar came back.
    frame_done: bool,
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
                // The first output is the nested session's only one.
                "wl_output" if probe.output.is_none() => {
                    probe.output = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_seat" if probe.seat.is_none() => {
                    probe.seat = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    probe.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "ext_session_lock_manager_v1" => {
                    probe.lock_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                probe.layer_configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => probe.layer_closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => probe.locked = true,
            ext_session_lock_v1::Event::Finished => probe.lock_finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            // Ack, then leave the commit to the script: the protocol
            // wants ack before commit, and the buffer must match.
            surface.ack_configure(serial);
            probe.lock_configured = Some((width, height));
        }
    }
}

impl Dispatch<WlSeat, ()> for Probe {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // The keyboard is taken as soon as the seat advertises one and
        // then kept: this probe never releases it, so every enter and
        // leave the compositor sends across a whole lock cycle lands in
        // the handler below.
        if let wl_seat::Event::Capabilities { capabilities: wayland_client::WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => probe.keyboard_on = Some(surface.id().protocol_id()),
            wl_keyboard::Event::Leave { surface, .. }
                if probe.keyboard_on == Some(surface.id().protocol_id()) =>
            {
                probe.keyboard_on = None;
            }
            // The keymap arrives as a file descriptor this probe has no
            // use for; letting it drop closes it.
            _ => {}
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
        if let wl_callback::Event::Done { .. } = event {
            probe.frame_done = true;
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
    WlOutput,
    ZwlrLayerShellV1,
    ExtSessionLockManagerV1
);

/// A sealed-off scratch file holding one solid-colour frame.
fn frame_file(width: u32, height: u32, pixel: [u8; 4]) -> std::fs::File {
    let path = format!(
        "{}/chonk-lock-probe-{}-{}",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/dev/shm".into()),
        std::process::id(),
        // One file per (size, colour) is overkill; per call is simpler
        // and the file is unlinked at once either way.
        fastrand()
    );
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap_or_else(|e| fatal(&format!("cannot create the frame file: {e}")));
    let _ = std::fs::remove_file(&path);
    let pixels = pixel.repeat((width * height) as usize);
    file.write_all(&pixels).unwrap_or_else(|e| fatal(&format!("cannot fill the frame file: {e}")));
    file
}

/// Enough uniqueness for scratch-file names without a dependency.
fn fastrand() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Creates and attaches a solid-colour `width`x`height` buffer to
/// `surface`, damages it whole, and commits.
fn commit_frame(
    probe_shm: &wl_shm::WlShm,
    qh: &QueueHandle<Probe>,
    surface: &WlSurface,
    width: u32,
    height: u32,
    pixel: [u8; 4],
) {
    let file = frame_file(width, height, pixel);
    let pool = probe_shm.create_pool(file.as_fd(), (width * height * 4) as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        (width * 4) as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
}

/// Reports a broken connection — with the protocol error the
/// compositor posted, when there is one — and exits 2, the exit code
/// the e2e reads as "the compositor killed this client".
fn broken(conn: &Connection, when: &str, error: &dyn std::fmt::Display) -> ! {
    println!("connection broke: {when}: {error}");
    if let Some(protocol_error) = conn.protocol_error() {
        println!(
            "protocol error: object {}@{} code {}: {}",
            protocol_error.object_interface,
            protocol_error.object_id,
            protocol_error.code,
            protocol_error.message
        );
    }
    // The report must reach the log before the exit.
    let _ = std::io::stdout().flush();
    std::process::exit(2);
}

/// Dispatches until `done` answers true, dying loudly if the
/// connection breaks or the compositor closes/refuses a surface.
fn dispatch_until(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<Probe>,
    probe: &mut Probe,
    what: &str,
    done: impl Fn(&Probe) -> bool,
) {
    while !done(probe) {
        if probe.layer_closed {
            fatal(&format!("the bar was closed while waiting for {what}"));
        }
        if probe.lock_finished {
            fatal(&format!("the lock was refused (finished) while waiting for {what}"));
        }
        if let Err(error) = queue.blocking_dispatch(probe) {
            broken(conn, what, &error);
        }
    }
}

/// A lock in force: the lock object and the role object of the surface
/// showing it, both live, as [`lock_and_draw`] leaves them and as
/// [`unlock_and_teardown`] consumes them.
struct Held {
    lock: ExtSessionLockV1,
    role: ExtSessionLockSurfaceV1,
}

/// One full lock: take the lock, give `wl_surface` the lock-surface
/// role on `output`, draw it at the configured size, and wait for
/// `locked`. Returns the live lock and the size it was configured at.
fn lock_and_draw(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<Probe>,
    probe: &mut Probe,
    qh: &QueueHandle<Probe>,
    surface: &WlSurface,
) -> (Held, (u32, u32)) {
    let manager = probe.lock_manager.clone().unwrap();
    let output = probe.output.clone().unwrap();
    let shm = probe.shm.clone().unwrap();
    probe.lock_configured = None;
    probe.locked = false;
    let lock = manager.lock(qh, ());
    let role = lock.get_lock_surface(surface, &output, qh, ());
    dispatch_until(conn, queue, probe, "the lock surface's first configure", |p| {
        p.lock_configured.is_some()
    });
    let (width, height) = probe.lock_configured.unwrap();
    let (width, height) = (width.max(1), height.max(1));
    // The handler acked; the buffer must now match exactly, or the
    // compositor is entitled to a dimensions_mismatch error.
    commit_frame(&shm, qh, surface, width, height, LOCK_NAVY);
    dispatch_until(conn, queue, probe, "the locked event", |p| p.locked);
    (Held { lock, role }, (width, height))
}

/// The teardown under test, spelled exactly as the captured Quickshell
/// trace spells it: unlock, destroy the role object, unmap the kept
/// `wl_surface` with a nil buffer, optionally destroy that surface too,
/// and sync — the roundtrip is where a compositor that answered the
/// unmap with a protocol error kills us.
///
/// `keep_surface` is the one deviation the script allows itself: the
/// real client always destroys the surface here, and cycles that mean
/// to re-lock on it later simply skip that last request. The commit
/// that does the damage has already happened either way.
fn unlock_and_teardown(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<Probe>,
    probe: &mut Probe,
    held: Held,
    surface: &WlSurface,
    keep_surface: bool,
    when: &str,
) {
    held.lock.unlock_and_destroy();
    held.role.destroy();
    surface.attach(None, 0, 0);
    surface.commit();
    if !keep_surface {
        surface.destroy();
    }
    if let Err(error) = queue.roundtrip(probe) {
        broken(conn, when, &error);
    }
}

fn main() {
    // `--hold` stops the script after the first lock and keeps it, for
    // the bypass e2e; without it the full seven-step teardown script
    // below runs. `Session::launch` passes argv but not environment, so
    // the switch is a flag.
    let hold = std::env::args().any(|arg| arg == "--hold");
    let recovery_hold = std::env::args().any(|arg| arg == "--recovery-hold");
    let conn = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("no compositor: {e}")));
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut probe = Probe::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut probe).unwrap_or_else(|e| fatal(&format!("registry roundtrip failed: {e}")));
    if probe.compositor.is_none()
        || probe.shm.is_none()
        || probe.output.is_none()
        || probe.seat.is_none()
        || probe.layer_shell.is_none()
        || probe.lock_manager.is_none()
    {
        fatal("the compositor lacks wl_compositor, wl_shm, wl_output, wl_seat, zwlr_layer_shell_v1 or ext_session_lock_manager_v1");
    }
    let compositor = probe.compositor.clone().unwrap();
    let shm = probe.shm.clone().unwrap();
    let layer_shell = probe.layer_shell.clone().unwrap();

    // A crash-recovery locker starts behind an already-enforced lock
    // boundary, so it cannot first map an ordinary layer surface and
    // wait for keyboard focus as the lifecycle probe below does. Take
    // over the holderless lock directly, draw, and remain its holder.
    if recovery_hold {
        let surface = compositor.create_surface(&qh, ());
        let (_held, (w, h)) =
            lock_and_draw(&conn, &mut queue, &mut probe, &qh, &surface);
        println!("locked {w}x{h}");
        println!("holding the recovery lock");
        let _ = std::io::stdout().flush();
        loop {
            if let Err(error) = queue.blocking_dispatch(&mut probe) {
                broken(&conn, "the recovery hold loop", &error);
            }
        }
    }

    // -- 1: the bar — the surface that must outlive the lock ------------
    let bar_surface = compositor.create_surface(&qh, ());
    let layer = layer_shell.get_layer_surface(
        &bar_surface,
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "chonk-lock-probe".into(),
        &qh,
        (),
    );
    layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_exclusive_zone(0);
    // Exclusive interactivity: Omarchy's popouts and its lock preview
    // ask for the keyboard this way, and a layer surface that holds the
    // keyboard when the lock engages is the one that can be left
    // keyboard-dead by an unlock that hands the seat somewhere else.
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive);
    bar_surface.commit();
    dispatch_until(&conn, &mut queue, &mut probe, "the bar's first configure", |p| {
        p.layer_configured.is_some()
    });
    let (bar_w, bar_h) = probe.layer_configured.unwrap();
    let (bar_w, bar_h) = (bar_w.max(1), bar_h.max(1));
    commit_frame(&shm, &qh, &bar_surface, bar_w, bar_h, BAR_ORANGE);
    if let Err(error) = queue.roundtrip(&mut probe) {
        broken(&conn, "mapping the bar", &error);
    }
    println!("layer mapped {bar_w}x{bar_h}");
    let bar_id = bar_surface.id().protocol_id();
    dispatch_until(&conn, &mut queue, &mut probe, "the keyboard entering the bar", |p| {
        p.keyboard_on == Some(bar_id)
    });
    println!("bar holds the keyboard");

    // -- 2: lock, on a wl_surface this script keeps for step 7 ----------
    let kept_wl_surface = compositor.create_surface(&qh, ());
    let (held, (w, h)) = lock_and_draw(&conn, &mut queue, &mut probe, &qh, &kept_wl_surface);
    println!("locked {w}x{h}");

    // -- `--hold`: stop here, still locked ------------------------------
    // The holder half of the bypass e2e. Everything below this point
    // unlocks, and the bypass can only be attempted against a lock that
    // is actually in force, so that test needs a locker that takes the
    // lock and then does nothing but stay alive — the state a real
    // locker sits in for as long as the user is away.
    if hold {
        println!("holding the lock");
        let _ = std::io::stdout().flush();
        loop {
            if let Err(error) = queue.blocking_dispatch(&mut probe) {
                broken(&conn, "the hold loop", &error);
            }
        }
    }

    // -- 3: the teardown that killed omarchy-shell ----------------------
    unlock_and_teardown(
        &conn, &mut queue, &mut probe, held, &kept_wl_surface, true, "the unlock teardown",
    );
    println!("survived the unlock teardown");

    // -- 4: the bar is not merely unkilled but still serviced -----------
    probe.frame_done = false;
    bar_surface.frame(&qh, ());
    commit_frame(&shm, &qh, &bar_surface, bar_w, bar_h, BAR_ORANGE);
    dispatch_until(&conn, &mut queue, &mut probe, "a frame callback on the bar", |p| p.frame_done);
    println!("layer surface serviced after unlock");

    // -- 5: and it has the keyboard back --------------------------------
    // The lock took the seat from the bar (`leave`) when it engaged; an
    // unlock that restores focus to a *window* and stops there leaves
    // the bar on screen and deaf, with nothing left to re-assert it.
    dispatch_until(&conn, &mut queue, &mut probe, "the keyboard returning to the bar", |p| {
        p.keyboard_on == Some(bar_id)
    });
    println!("bar has the keyboard back");

    // -- 6: the second cycle, on a fresh wl_surface — the shape the
    // real Quickshell takes, torn down in full this time ----------------
    let fresh_wl_surface = compositor.create_surface(&qh, ());
    let (held, (w, h)) = lock_and_draw(&conn, &mut queue, &mut probe, &qh, &fresh_wl_surface);
    println!("relocked {w}x{h}");
    unlock_and_teardown(
        &conn,
        &mut queue,
        &mut probe,
        held,
        &fresh_wl_surface,
        false,
        "the second unlock teardown",
    );
    println!("survived the second unlock teardown");
    dispatch_until(&conn, &mut queue, &mut probe, "the keyboard returning to the bar again", |p| {
        p.keyboard_on == Some(bar_id)
    });
    println!("bar has the keyboard back again");

    // -- 7: the hostile third cycle: re-lock on the surface from step 3,
    // which already wore the lock-surface role and whose unmap the
    // compositor neutralized. The spec calls that a client error, and
    // this probe is the client making it deliberately: what must NOT
    // happen is the session blanking behind a locker that never gets
    // its mandatory first configure and so can never draw ------------
    let (held, (w, h)) = lock_and_draw(&conn, &mut queue, &mut probe, &qh, &kept_wl_surface);
    println!("relocked on a reused surface {w}x{h}");
    unlock_and_teardown(
        &conn,
        &mut queue,
        &mut probe,
        held,
        &kept_wl_surface,
        false,
        "the third unlock teardown",
    );
    println!("survived the third unlock teardown");
    let _ = std::io::stdout().flush();

    // A shell process's life: dispatch until killed. The test asserts
    // on the lines above and then kills us; dying HERE of a broken
    // connection is still a failure worth reporting.
    loop {
        if let Err(error) = queue.blocking_dispatch(&mut probe) {
            broken(&conn, "the idle loop", &error);
        }
    }
}
