//! The X11 half: one window, one event loop, and the keyboard.
//!
//! Structurally this is `chonk_ui::App` with two things added — a
//! `KEY_PRESS` mask with a keysym table behind it, and press/release
//! rather than press alone — because a passphrase field needs the
//! first and a button that sinks under the pointer needs the second.
//! Everything else (the `PutImage` row-chunking, the byte-order swap,
//! the `WM_DELETE_WINDOW` handshake) is the SDK's solution to the SDK's
//! problem and is reproduced rather than reinvented; see the note in
//! this crate's `Cargo.toml` about folding it back once `App` grows a
//! key callback.

use tiny_skia::Pixmap;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use chonk_netjoin::keys::{key_from, level, Key};

/// What the event loop hands back to its caller.
pub enum Input {
    Key(Key),
    Press { x: i32, y: i32 },
    Release { x: i32, y: i32 },
    /// The window was closed by the desktop — the titlebar button,
    /// `WM_DELETE_WINDOW`, or the connection dropping.
    Closed,
    /// Something happened that only needs a repaint.
    Redraw,
}

pub struct Window {
    conn: RustConnection,
    window: x11rb::protocol::xproto::Window,
    gc: Gcontext,
    depth: u8,
    width: u32,
    height: u32,
    wm_delete_window: Atom,
    /// Keycode → keysyms, snapshotted at map time. A layout switch
    /// mid-dialog is not handled: this window lives for as long as it
    /// takes to type one passphrase, and re-reading the map on every
    /// `MappingNotify` is machinery for a case that does not happen.
    keymap: Vec<(u8, Vec<u32>)>,
}

impl Window {
    pub fn open(title: &str, width: u32, height: u32) -> Option<Window> {
        let (conn, screen_num) = RustConnection::connect(None).ok()?;
        let screen = conn.setup().roots.get(screen_num)?.clone();
        let window = conn.generate_id().ok()?;

        let aux = CreateWindowAux::new()
            .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::KEY_PRESS)
            .background_pixel(screen.white_pixel);
        conn.create_window(COPY_DEPTH_FROM_PARENT, window, screen.root, 0, 0, width as u16, height as u16, 0, WindowClass::INPUT_OUTPUT, 0, &aux)
            .ok()?;

        let _ = conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes());
        // Ask to be treated as a dialog. chonkstep's X11 backend reads
        // `_NET_WM_WINDOW_TYPE`, and a dialog is the honest declaration
        // for a transient window with two buttons on it.
        if let (Ok(kind), Ok(dialog)) = (conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE"), conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DIALOG")) {
            if let (Ok(kind), Ok(dialog)) = (kind.reply(), dialog.reply()) {
                let _ = conn.change_property32(PropMode::REPLACE, window, kind.atom, AtomEnum::ATOM, &[dialog.atom]);
            }
        }

        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS").ok()?.reply().ok()?.atom;
        let wm_delete_window = conn.intern_atom(false, b"WM_DELETE_WINDOW").ok()?.reply().ok()?.atom;
        let _ = conn.change_property32(PropMode::REPLACE, window, wm_protocols, AtomEnum::ATOM, &[wm_delete_window]);

        let gc = conn.generate_id().ok()?;
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0)).ok()?;

        let keymap = fetch_keymap(&conn);

        conn.map_window(window).ok()?;
        conn.flush().ok()?;
        Some(Window { conn, window, gc, depth: screen.root_depth, width, height, wm_delete_window, keymap })
    }

    /// Blocks for the next input. Returns [`Input::Closed`] once, after
    /// which the caller should stop asking.
    pub fn next(&self) -> Input {
        loop {
            let Ok(event) = self.conn.wait_for_event() else { return Input::Closed };
            match event {
                Event::Expose(_) => return Input::Redraw,
                Event::ButtonPress(e) if e.detail == 1 => return Input::Press { x: e.event_x as i32, y: e.event_y as i32 },
                Event::ButtonRelease(e) if e.detail == 1 => return Input::Release { x: e.event_x as i32, y: e.event_y as i32 },
                Event::KeyPress(e) => {
                    let state = u16::from(e.state);
                    let shift = state & u16::from(ModMask::SHIFT) != 0;
                    let ctrl = state & u16::from(ModMask::CONTROL) != 0;
                    // Ignore keys with no text meaning rather than
                    // returning a Redraw for each: an arrow key held
                    // down should cost nothing.
                    if let Some(key) = self.keysym(e.detail, shift).and_then(|k| key_from(k, shift, ctrl)) {
                        return Input::Key(key);
                    }
                }
                Event::ClientMessage(e) => {
                    if e.format == 32 && e.data.as_data32()[0] == self.wm_delete_window {
                        return Input::Closed;
                    }
                }
                Event::DestroyNotify(_) => return Input::Closed,
                Event::Error(_) => return Input::Closed,
                _ => {}
            }
        }
    }

    fn keysym(&self, keycode: u8, shift: bool) -> Option<u32> {
        let syms = self.keymap.iter().find(|(code, _)| *code == keycode).map(|(_, syms)| syms)?;
        level(syms, shift)
    }

    pub fn present(&self, pixmap: &Pixmap) {
        let data = to_server_bytes(pixmap, self.conn.setup().image_byte_order);
        self.put_image_rows(self.width as u16, self.height as u16, &data);
        let _ = self.conn.flush();
    }

    /// One `PutImage` per row band that fits the connection's actual
    /// negotiated request limit — a whole scaled window's RGBA8 can
    /// exceed the protocol's hard cap even with BIG-REQUESTS, and
    /// x11rb refuses the oversized request client-side rather than
    /// truncating it, so an unchunked blit simply never paints.
    fn put_image_rows(&self, w: u16, h: u16, data: &[u8]) {
        let stride = w as usize * 4;
        if stride == 0 || h == 0 {
            return;
        }
        const REQUEST_OVERHEAD_BYTES: usize = 64;
        let budget = self.conn.maximum_request_bytes().saturating_sub(REQUEST_OVERHEAD_BYTES);
        let rows_per_chunk = (budget / stride).max(1);
        for (index, chunk) in data.chunks(rows_per_chunk * stride).enumerate() {
            let y = (index * rows_per_chunk) as i16;
            let chunk_h = (chunk.len() / stride) as u16;
            let _ = self.conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, w, chunk_h, 0, y, 0, self.depth, chunk);
        }
    }
}

fn fetch_keymap(conn: &RustConnection) -> Vec<(u8, Vec<u32>)> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(min).saturating_add(1);
    let Ok(reply) = conn.get_keyboard_mapping(min, count).map_err(|_| ()).and_then(|c| c.reply().map_err(|_| ())) else {
        // No keymap means no typing, which is a dialog that cannot do
        // its job — but it is still better to show it and let the
        // person close it than to exit with no explanation.
        return Vec::new();
    };
    let per = (reply.keysyms_per_keycode as usize).max(1);
    reply.keysyms.chunks(per).enumerate().map(|(i, syms)| (min.wrapping_add(i as u8), syms.to_vec())).collect()
}

fn to_server_bytes(pixmap: &Pixmap, order: ImageOrder) -> Vec<u8> {
    let msb_first = order == ImageOrder::MSB_FIRST;
    let mut out = Vec::with_capacity(pixmap.data().len());
    for px in pixmap.data().chunks_exact(4) {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        if msb_first {
            out.extend_from_slice(&[a, r, g, b]);
        } else {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out
}
