//! The instrument-panel e2e's scripted dockapp.
//!
//! A deliberately tiny conformant client, speaking the wire protocol
//! directly through `chonk-dock-proto` (no SDK — the fewer layers
//! between the test and the bytes, the more the test says). Behavior,
//! each step observable from outside as pixels or ledger state:
//!
//! - draws a solid **blue** tile;
//! - on a tile click, asks for a 600x400 panel — deliberately larger
//!   than one datagram can carry (600 * 400 * 4 = 937.5 KiB), so the
//!   grant can only be satisfied by the banded frame path;
//! - streams the granted panel solid **green**, in bands;
//! - on a click inside the panel, restreams it solid **red** — the
//!   input round trip, visible as a color change in a screenshot;
//! - answers pings, honors `PanelClosed`, exits on `Goodbye`/EOF.
//!
//! Band sends follow the protocol document's bounded-wait rule: a
//! band that hits `EAGAIN` is retried for up to a second, and a
//! repaint that cannot complete is abandoned whole.

use std::time::{Duration, Instant};

use chonk_dock_proto::handshake::hello;
use chonk_dock_proto::transport::{token_from_hex, Seqpacket, ENV_SOCKET, ENV_TOKEN};
use chonk_dock_proto::wire::{InputKind, InputMask};
use chonk_dock_proto::{ClientMessage, ServerMessage, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES};

const ID: &str = "panel-probe";
/// Premultiplied opaque RGBA. Distinctive, screenshot-assertable hues.
const TILE_BLUE: [u8; 4] = [0x20, 0x50, 0xB0, 0xFF];
const PANEL_GREEN: [u8; 4] = [0x10, 0xC8, 0x20, 0xFF];
const PANEL_RED: [u8; 4] = [0xC8, 0x10, 0x10, 0xFF];

fn fatal(message: &str) -> ! {
    eprintln!("chonk-panel-probe: {message}");
    std::process::exit(1);
}

/// One message, sent whole, with the bounded wait the protocol
/// document prescribes for panel bands (and which is harmless for
/// everything smaller).
fn send_bounded(socket: &Seqpacket, bytes: &[u8]) -> bool {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match socket.send(bytes) {
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return false,
        }
    }
}

/// A full top-to-bottom repaint of the granted panel in one color, as
/// bands sharing one generation.
fn stream_panel(socket: &Seqpacket, granted: (u32, u32), generation: u32, color: [u8; 4]) {
    let (width, height) = granted;
    let max_rows = ((MAX_FRAME_BYTES / 4) as u32 / width.max(1)).max(1);
    let mut y = 0;
    while y < height {
        let band_height = max_rows.min(height - y);
        let pixels: Vec<u8> = color.repeat((width * band_height) as usize);
        let band = ClientMessage::PanelFrame { generation, y, band_height, width, pixels };
        let bytes = band.encode().unwrap_or_else(|e| fatal(&format!("band encode failed: {e}")));
        if !send_bounded(socket, &bytes) {
            // The bounded wait expired: abandon the repaint whole; the
            // next trigger re-sends it from the top.
            return;
        }
        y += band_height;
    }
}

fn send_tile(socket: &Seqpacket, tile_px: u32, generation: u32) {
    let pixels: Vec<u8> = TILE_BLUE.repeat((tile_px * tile_px) as usize);
    if let Ok(bytes) = (ClientMessage::Frame { generation, width: tile_px, height: tile_px, pixels }).encode() {
        let _ = socket.send(&bytes);
    }
}

fn main() {
    let socket_path = std::env::var(ENV_SOCKET)
        .unwrap_or_else(|_| fatal("CHONKSTEP_DOCK_SOCKET is not set; this program is launched by the dock, not a terminal"));
    let token = std::env::var(ENV_TOKEN)
        .ok()
        .as_deref()
        .and_then(token_from_hex)
        .unwrap_or_else(|| fatal("CHONKSTEP_DOCK_TOKEN missing or malformed"));

    let socket = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Seqpacket::connect(std::path::Path::new(&socket_path)) {
                Ok(socket) => break socket,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => fatal(&format!("cannot connect to {socket_path}: {e}")),
            }
        }
    };

    let hello = hello(ID, 1, token, InputMask::new(InputMask::PRESS).unwrap_or_else(InputMask::all));
    if !send_bounded(&socket, &hello.encode().unwrap_or_else(|e| fatal(&format!("hello encode: {e}")))) {
        fatal("could not send Hello");
    }

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let mut tile_px;
    match socket.recv_until(&mut buffer, Instant::now() + Duration::from_secs(2)) {
        Ok(Some(n)) if n > 0 => match ServerMessage::decode(&buffer[..n]) {
            Ok(ServerMessage::Welcome(state)) => {
                if !state.panels_supported() {
                    fatal(&format!("shell speaks protocol {}, panels need >= 2", state.proto));
                }
                tile_px = state.tile_px;
            }
            Ok(ServerMessage::Goodbye { reason }) => fatal(&format!("refused: {reason:?}")),
            other => fatal(&format!("expected Welcome, got {other:?}")),
        },
        _ => fatal("no Welcome within the handshake window"),
    }

    let mut frame_generation: u32 = 0;
    let mut panel_generation: u32 = 0;
    let mut granted: Option<(u32, u32)> = None;
    frame_generation += 1;
    send_tile(&socket, tile_px, frame_generation);

    loop {
        let n = match socket.recv_until(&mut buffer, Instant::now() + Duration::from_secs(1)) {
            Ok(Some(0)) => return, // EOF: the test session is over.
            Ok(Some(n)) => n,
            Ok(None) => continue,
            Err(_) => return,
        };
        match ServerMessage::decode(&buffer[..n]) {
            Ok(ServerMessage::Ping { seq }) => {
                let _ = socket.send(&ClientMessage::Pong { seq }.encode().unwrap());
            }
            Ok(ServerMessage::Welcome(state)) | Ok(ServerMessage::ThemeChanged(state)) => {
                tile_px = state.tile_px;
                frame_generation += 1;
                send_tile(&socket, tile_px, frame_generation);
            }
            Ok(ServerMessage::Visibility { visible }) => {
                if visible {
                    frame_generation += 1;
                    send_tile(&socket, tile_px, frame_generation);
                }
            }
            Ok(ServerMessage::Input(event)) => {
                // The open gesture: a click on the tile asks for the
                // detail panel.
                if event.kind == InputKind::Press {
                    let open = ClientMessage::OpenPanel { width: 600, height: 400 };
                    let _ = socket.send(&open.encode().unwrap());
                }
            }
            Ok(ServerMessage::PanelOpened { width, height }) => {
                granted = Some((width, height));
                panel_generation += 1;
                stream_panel(&socket, (width, height), panel_generation, PANEL_GREEN);
            }
            Ok(ServerMessage::PanelInput(event)) => {
                // The input round trip, made visible: a press inside
                // the panel turns it red.
                if event.kind == InputKind::Press {
                    if let Some(granted) = granted {
                        panel_generation += 1;
                        stream_panel(&socket, granted, panel_generation, PANEL_RED);
                    }
                }
            }
            Ok(ServerMessage::PanelClosed { .. }) => {
                granted = None;
            }
            Ok(ServerMessage::Goodbye { .. }) => return,
            Err(_) => return, // Protocol disagreement: stop, as the SDK would.
        }
    }
}
