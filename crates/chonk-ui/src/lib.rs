//! Minimal reusable GUI toolkit for chonkstep apps.
//!
//! An app built with `chonk_ui` is a regular, independent X11 client —
//! chonkstep decorates its window automatically, the same as it would
//! any other window — but it draws its *content* with the same
//! `wm-theme` paint primitives (flat/gradient fills, the chisel bevel,
//! themed text) the desktop shell itself uses for the dock and root
//! menu, via the re-exported [`paint`] module and [`nextstep_theme`].
//! That's the whole point of the SDK: app content and window chrome
//! come from the same visual vocabulary instead of an app inventing its
//! own.
//!
//! This is a deliberately small first cut: one fixed-size window, a
//! single redraw callback, and click notification — no layout engine or
//! widget tree yet. Enough to prove real apps can inherit the look and
//! feel; a fuller widget toolkit is future work.

use tiny_skia::Pixmap;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

pub use wm_theme::{default_theme::nextstep_classic as nextstep_theme, model, paint};

/// Reads the same `CHONKSTEP_SCALE` env var chonkstep itself reads (see
/// `chonkstep::read_scale_factor`) — every window chonkstep manages sits
/// in the same session and should agree on one scale, so an SDK app has
/// no reason to invent its own convention. Deliberately duplicated
/// rather than shared via a common crate: `chonk-ui` apps are meant to
/// be buildable as fully independent X11 clients, with zero dependency
/// on chonkstep's own crates (`wm-core`, `wm-x11`, ...) — the four lines
/// this saves aren't worth coupling the SDK to the WM binary.
pub fn scale_factor() -> f32 {
    std::env::var("CHONKSTEP_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// `nextstep_theme()` scaled by [`scale_factor`] — the theme an app
/// should actually draw with. Every font size, so text an app draws
/// stays crisp (re-shaped at the target size) rather than looking like
/// `nextstep_theme()`'s output blown up and blurry.
pub fn scaled_theme() -> model::Theme {
    nextstep_theme().scaled(scale_factor())
}

/// A single top-level application window.
pub struct App {
    conn: RustConnection,
    window: Window,
    gc: Gcontext,
    depth: u8,
    width: u32,
    height: u32,
    scale: f32,
    wm_delete_window: Atom,
}

impl App {
    /// `logical_width`/`logical_height` are unscaled, 1x-density units —
    /// same convention as this theme's own base pixel sizes (a 20px
    /// titlebar, 14px buttons) before `Theme::scaled` multiplies them up.
    /// The real on-screen window is `logical_size * scale_factor()`, and
    /// [`App::scale`] hands that same factor back so the app's own
    /// drawing code (positions, sizes it computes itself — there's no
    /// layout engine yet to do this automatically) can scale in lockstep
    /// instead of drawing a small pixmap into a big window.
    pub fn new(title: &str, logical_width: u32, logical_height: u32) -> Self {
        let scale = scale_factor();
        let width = ((logical_width as f32) * scale).round().max(1.0) as u32;
        let height = ((logical_height as f32) * scale).round().max(1.0) as u32;

        let (conn, screen_num) = RustConnection::connect(None).expect("connect to X server");
        let screen = conn.setup().roots[screen_num].clone();
        let window = conn.generate_id().expect("generate_id");

        let aux = CreateWindowAux::new()
            .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS)
            .background_pixel(screen.white_pixel);
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            0,
            0,
            width as u16,
            height as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &aux,
        )
        .expect("create_window");

        let _ = conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes());

        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS").expect("intern WM_PROTOCOLS").reply().expect("reply").atom;
        let wm_delete_window = conn
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .expect("intern WM_DELETE_WINDOW")
            .reply()
            .expect("reply")
            .atom;
        let _ = conn.change_property32(PropMode::REPLACE, window, wm_protocols, AtomEnum::ATOM, &[wm_delete_window]);

        let gc = conn.generate_id().expect("generate_id");
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0)).expect("create_gc");

        conn.map_window(window).expect("map_window");
        conn.flush().expect("flush");

        Self { conn, window, gc, depth: screen.root_depth, width, height, scale, wm_delete_window }
    }

    /// The active `CHONKSTEP_SCALE` — multiply any pixel position/size
    /// your own drawing code computes by this before passing it to
    /// `paint::*`, same as [`App::new`] did for the window itself.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Blocking event loop. `draw` paints a fresh `width x height`
    /// pixmap on every redraw (initial map, `Expose`); `on_click` fires
    /// for each button press with the window-local click position.
    /// Returns once the window is closed (via its titlebar close button
    /// or `WM_DELETE_WINDOW`).
    pub fn run(self, mut draw: impl FnMut(&mut Pixmap), mut on_click: impl FnMut(i16, i16)) {
        loop {
            let Some(mut pixmap) = Pixmap::new(self.width, self.height) else { return };
            draw(&mut pixmap);
            self.blit(&pixmap);

            let event = match self.conn.wait_for_event() {
                Ok(e) => e,
                Err(_) => return,
            };
            match event {
                Event::ButtonPress(e) => on_click(e.event_x, e.event_y),
                Event::ClientMessage(e) => {
                    if e.format == 32 && e.data.as_data32()[0] == self.wm_delete_window {
                        return;
                    }
                }
                Event::DestroyNotify(_) => return,
                _ => {}
            }
        }
    }

    fn blit(&self, pixmap: &Pixmap) {
        let data = to_server_bytes(pixmap, self.conn.setup().image_byte_order);
        self.put_image_rows(self.width as u16, self.height as u16, &data);
        let _ = self.conn.flush();
    }

    /// Sends `data` (a `w`x`h` `ZPixmap` buffer, top row first) to the
    /// window via one or more `PutImage` requests, splitting it into
    /// horizontal row bands that each fit under the connection's actual
    /// negotiated request-size limit — a single `PutImage` for a whole
    /// scaled-up window's worth of RGBA8 can exceed that limit (the X11
    /// protocol's own hard cap even with `BIG-REQUESTS` enabled), and
    /// x11rb rejects the oversized request client-side rather than
    /// truncating it, so without chunking a big enough window would
    /// never actually paint anything.
    fn put_image_rows(&self, w: u16, h: u16, data: &[u8]) {
        let stride = w as usize * 4;
        if stride == 0 || h == 0 {
            return;
        }
        const REQUEST_OVERHEAD_BYTES: usize = 64;
        let budget = self.conn.maximum_request_bytes().saturating_sub(REQUEST_OVERHEAD_BYTES);
        let rows_per_chunk = (budget / stride).max(1);
        for (chunk_index, chunk) in data.chunks(rows_per_chunk * stride).enumerate() {
            let y = (chunk_index * rows_per_chunk) as i16;
            let chunk_h = (chunk.len() / stride) as u16;
            let _ = self.conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, w, chunk_h, 0, y, 0, self.depth, chunk);
        }
    }
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
