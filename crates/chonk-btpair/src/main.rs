//! The Bluetooth pairing dialog: a small chiseled window that scans
//! for devices, pairs the one someone clicks, and answers BlueZ's
//! passkey question with two buttons.
//!
//! Spawned detached by the dock's BT panel (`+ PAIR NEW…`), and
//! runnable on its own. It inherits the session's look through the same
//! environment every chonkstep app reads — `CHONKSTEP_THEME`,
//! `CHONKSTEP_APPEARANCE`, `CHONKSTEP_SCALE` — so a restyled desktop
//! spawns a restyled dialog.
//!
//! # Why this exists as a window and not as a panel
//!
//! The dock panel it is launched from has no keyboard, by design, and
//! more importantly no *lifetime*: a panel is a popover that any click
//! elsewhere dismisses. Pairing takes tens of seconds, involves a
//! question that must stay on screen until someone answers it, and
//! must survive the user looking at something else. That is a window.
//!
//! # Why this window is not `chonk_ui::App`
//!
//! `chonk_ui::App` is the house window and this crate deliberately
//! does not use it, for one reason that is not a matter of taste:
//! **`App::run` blocks in `wait_for_event()` with no timeout, no timer
//! and no wakeup channel**, repainting only when an X event arrives.
//! That is exactly right for `chonk-about`, whose content never
//! changes. It cannot drive this window, whose entire middle state is
//! a discovery list filling in from a child process over several
//! seconds with no pointer input at all — under `App` the list would
//! sit empty until someone happened to move the mouse.
//!
//! Nor can it be worked around from outside: waking that loop needs the
//! window id and the connection, and `App` keeps both private, so a
//! background thread has nothing to send a synthetic event *to*.
//!
//! So this file carries its own loop, built from the same pieces in the
//! same order as `App`'s — the window creation, the `WM_DELETE_WINDOW`
//! protocol and the pixmap blit are that code's shape on purpose, so
//! the two can be reconciled later. **The fix belongs in the SDK**, not
//! here: `App` wants a `run_with(Options { redraw_interval })`, the
//! same affordance `chonk_ui::dockapp` already has on the socket side.
//! When it grows one, this file should lose its loop. Recorded as
//! friction in `docs/bluetooth.md` rather than left for the next reader
//! to rediscover.

// The pure halves live in the library beside this file
// (`lib.rs`), so every phase of this dialog can be rasterized and
// tested with no X server and no radio — the same split
// `chonk-netjoin` uses.
use chonk_btpair::{pair, render};

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use tiny_skia::Pixmap;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, ImageOrder, PropMode,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use pair::{Pairing, Step};
use render::{Metrics, Target};

/// How often the loop wakes to fold whatever the child has said.
///
/// A discovery list is the thing this paces: BlueZ announces devices
/// whenever it finds them, and 40ms is well inside the interval at
/// which a list feels live while costing a dialog that exists for a
/// minute nothing worth measuring.
const POLL: Duration = Duration::from_millis(40);

/// The control program this dialog drives. See [`pair`]'s module doc
/// for why this process may run it when the dock never may, and why
/// pairing cannot be done with `busctl` alone.
const BLUETOOTHCTL: &str = "bluetoothctl";

fn main() {
    let theme = chonk_ui::scaled_theme();
    let metrics = Metrics::new(chonk_ui::scale_factor());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();

    let mut pairing = Pairing::new();
    let mut session = Session::start();
    if let Some(session) = session.as_mut() {
        session.issue(&pairing.opening());
    } else {
        // No `bluetoothctl` on this machine at all. The dialog still
        // opens and still says why, rather than flashing and vanishing.
        pairing.on_line("No default controller available");
    }

    let Some(window) = Window::open(&metrics) else {
        eprintln!("chonk-btpair: could not open a window on this display");
        return;
    };

    let mut hover: Option<Target> = None;
    let mut dirty = true;
    loop {
        // The child's transcript, folded as it arrives — this is the
        // half `chonk_ui::App`'s blocking loop cannot serve.
        if let Some(active) = session.as_mut() {
            match active.poll() {
                Some(lines) => {
                    for line in lines {
                        let steps = pairing.on_line(&line);
                        dirty |= steps.iter().any(|step| matches!(step, Step::Repaint));
                        active.issue(&steps);
                    }
                }
                None => {
                    // The child exited. Whatever it last said stands.
                    session = None;
                }
            }
        }

        match window.pump() {
            Pump::Closed => break,
            Pump::Exposed => dirty = true,
            Pump::Click { x, y } => {
                let devices = pairing.devices();
                let layout = render::layout(&metrics, pairing.phase(), &devices);
                if let Some(target) = layout.at(x as i32, y as i32).cloned() {
                    drop(devices);
                    let steps = match target {
                        Target::Device(address) => pairing.pair_with(&address),
                        Target::Yes => pairing.answer(true),
                        Target::No => pairing.answer(false),
                        Target::Rescan => pairing.rescan(),
                    };
                    dirty |= steps.iter().any(|step| matches!(step, Step::Repaint));
                    if let Some(active) = session.as_mut() {
                        active.issue(&steps);
                    }
                }
            }
            Pump::Motion { x, y } => {
                let devices = pairing.devices();
                let layout = render::layout(&metrics, pairing.phase(), &devices);
                let now = layout.at(x as i32, y as i32).cloned();
                drop(devices);
                if now != hover {
                    hover = now;
                    dirty = true;
                }
            }
            Pump::Idle => {}
        }

        if dirty {
            let Some(mut pixmap) = Pixmap::new(metrics.width, metrics.height) else { break };
            let devices = pairing.devices();
            render::draw(&mut pixmap, &theme, &mut fonts, &mut swash, &metrics, pairing.phase(), &devices, hover.as_ref());
            drop(devices);
            window.blit(&pixmap);
            dirty = false;
        }

        std::thread::sleep(POLL);
    }

    // Leave the adapter as it was found: not discovering.
    if let Some(active) = session.as_mut() {
        active.issue(&pairing.closing());
    }
    drop(session);
}

/// The `bluetoothctl` child and the thread reading its transcript.
///
/// The reader is a thread rather than a non-blocking read because a
/// pipe read blocks and this process must stay responsive to the
/// pointer — the same "put the blocking thing on its own thread"
/// discipline the dock's sampler follows, for the same reason.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Session {
    fn start() -> Option<Self> {
        let mut child = Command::new(BLUETOOTHCTL)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // BlueZ's agent prompts and its errors are not consistently
            // on one stream; folding stderr in means the dialog sees a
            // failure it would otherwise wait forever for.
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;

        let (sender, lines) = mpsc::channel();
        for stream in [child.stdout.take().map(Readable::Out), child.stderr.take().map(Readable::Err)].into_iter().flatten() {
            let sender = sender.clone();
            std::thread::Builder::new()
                .name("chonk-btpair-read".to_string())
                .spawn(move || {
                    let reader: Box<dyn BufRead> = match stream {
                        Readable::Out(out) => Box::new(BufReader::new(out)),
                        Readable::Err(err) => Box::new(BufReader::new(err)),
                    };
                    // `bluetoothctl` writes prompts without a trailing
                    // newline, so splitting on newlines alone would hold
                    // an agent question hostage until the next line.
                    // Splitting on both newline and the prompt's `#`
                    // would be worse (it appears mid-message), so this
                    // reads lines and the parser tolerates a prompt
                    // sharing a line with content.
                    for line in reader.lines().map_while(Result::ok) {
                        if sender.send(line).is_err() {
                            return;
                        }
                    }
                })
                .ok();
        }

        let stdin = child.stdin.take();
        Some(Self { child, stdin, lines })
    }

    /// Whatever the child has said since the last poll, or `None` once
    /// it has exited and said everything.
    fn poll(&mut self) -> Option<Vec<String>> {
        let mut out = Vec::new();
        loop {
            match self.lines.try_recv() {
                Ok(line) => out.push(line),
                Err(TryRecvError::Empty) => return Some(out),
                Err(TryRecvError::Disconnected) => {
                    return (!out.is_empty()).then_some(out);
                }
            }
        }
    }

    /// Writes a fold's commands to the child.
    fn issue(&mut self, steps: &[Step]) {
        let Some(stdin) = self.stdin.as_mut() else { return };
        for step in steps {
            let Step::Send(line) = step else { continue };
            if writeln!(stdin, "{line}").is_err() || stdin.flush().is_err() {
                // The child is gone; nothing left to say to it.
                self.stdin = None;
                return;
            }
        }
    }
}

enum Readable {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Drop for Session {
    fn drop(&mut self) {
        // Closing stdin is `quit` for a readline client; the kill is
        // the backstop for one that is wedged waiting on BlueZ, which
        // on an adapter-less machine is exactly what happens. A dialog
        // that left a hung `bluetoothctl` behind every time it closed
        // would be a worse citizen than the one it replaced.
        self.stdin = None;
        let _ = self.child.kill();
        // Audited exception to the workspace ban on `Child::wait`. The
        // ban is about blocking a thread something else is waiting on;
        // this is the last statement of a process that is exiting, and
        // the child was killed on the line above, so the wait is a
        // reap of an already-dead process rather than a wait on a live
        // one. Skipping it instead would leave a zombie for however
        // long the shell keeps this dialog's pid — which, since the
        // panel spawns it detached, is the session.
        #[allow(clippy::disallowed_methods)]
        let _ = self.child.wait();
    }
}

/// What one pass of the event pump found.
enum Pump {
    Idle,
    Exposed,
    Click { x: i16, y: i16 },
    Motion { x: i16, y: i16 },
    Closed,
}

/// The X11 window. Created exactly as `chonk_ui::App` creates its own —
/// see this file's module doc for why the loop beside it is not
/// `App`'s.
struct Window {
    conn: RustConnection,
    window: u32,
    gc: u32,
    depth: u8,
    width: u32,
    height: u32,
    wm_delete_window: u32,
}

impl Window {
    fn open(metrics: &Metrics) -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None).ok()?;
        let screen = conn.setup().roots[screen_num].clone();
        let window = conn.generate_id().ok()?;

        let aux = CreateWindowAux::new()
            // Motion on top of `App`'s mask: the buttons and rows light
            // under the pointer, which needs to know where it is.
            .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::POINTER_MOTION)
            .background_pixel(screen.white_pixel);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            0,
            0,
            metrics.width as u16,
            metrics.height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &aux,
        )
        .ok()?;

        let _ = conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, b"Pair a Bluetooth Device");
        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS").ok()?.reply().ok()?.atom;
        let wm_delete_window = conn.intern_atom(false, b"WM_DELETE_WINDOW").ok()?.reply().ok()?.atom;
        let _ = conn.change_property32(PropMode::REPLACE, window, wm_protocols, AtomEnum::ATOM, &[wm_delete_window]);

        let gc = conn.generate_id().ok()?;
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0)).ok()?;
        conn.map_window(window).ok()?;
        conn.flush().ok()?;

        let depth = screen.root_depth;
        Some(Self { conn, window, gc, depth, width: metrics.width, height: metrics.height, wm_delete_window })
    }

    /// Drains the X queue without blocking — the difference from
    /// `App::run`, and the whole reason this type exists.
    fn pump(&self) -> Pump {
        let mut result = Pump::Idle;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::Expose(_) => result = Pump::Exposed,
                Event::ButtonPress(press) => return Pump::Click { x: press.event_x, y: press.event_y },
                Event::MotionNotify(motion) => result = Pump::Motion { x: motion.event_x, y: motion.event_y },
                Event::ClientMessage(message) => {
                    if message.format == 32 && message.data.as_data32()[0] == self.wm_delete_window {
                        return Pump::Closed;
                    }
                }
                Event::DestroyNotify(_) => return Pump::Closed,
                _ => {}
            }
        }
        result
    }

    /// Pushes a pixmap to the window, in the server's byte order and in
    /// row bands under the connection's request limit — `App::blit`'s
    /// job, kept identical so the two can be reconciled.
    fn blit(&self, pixmap: &Pixmap) {
        let bytes = self.to_server_bytes(pixmap);
        let stride = (self.width * 4) as usize;
        let max = self.conn.maximum_request_bytes().saturating_sub(64);
        let rows_per_band = (max / stride).max(1);
        let mut y = 0usize;
        while y < self.height as usize {
            let rows = rows_per_band.min(self.height as usize - y);
            let start = y * stride;
            let _ = self.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.gc,
                self.width as u16,
                rows as u16,
                0,
                y as i16,
                0,
                self.depth,
                &bytes[start..start + rows * stride],
            );
            y += rows;
        }
        let _ = self.conn.flush();
    }

    /// tiny-skia's premultiplied RGBA to the server's pixel order.
    fn to_server_bytes(&self, pixmap: &Pixmap) -> Vec<u8> {
        let msb_first = self.conn.setup().image_byte_order == ImageOrder::MSB_FIRST;
        pixmap
            .data()
            .chunks_exact(4)
            .flat_map(|px| {
                let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
                if msb_first {
                    [a, r, g, b]
                } else {
                    [b, g, r, a]
                }
            })
            .collect()
    }
}
