//! Opt-in real xdg move/resize requests using observed or deliberately invalid
//! input serials. F1: last pointer press; F2: zero; F3: wrong pointer serial;
//! F4: last touch down; F5: serial supplied in a private fixture file.

use std::path::PathBuf;
use std::time::Duration;

use wayland_client::protocol::{wl_callback, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{ResizeEdge, XdgToplevel};

use super::{say, Probe};

#[derive(Default)]
pub(super) struct State {
    operation: Option<Operation>,
    pointer: Option<u32>,
    touch: Option<u32>,
    serial_file: Option<PathBuf>,
    auto_serial_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Move,
    Resize,
}

impl State {
    pub(super) fn from_args() -> Self {
        let mut state = Self::default();
        for argument in std::env::args() {
            match argument.as_str() {
                "--interactive=move" => state.operation = Some(Operation::Move),
                "--interactive=resize" => state.operation = Some(Operation::Resize),
                _ => {
                    if let Some(path) = argument.strip_prefix("--serial-file=") {
                        state.serial_file = Some(path.into());
                    } else if let Some(path) = argument.strip_prefix("--auto-serial-file=") {
                        state.auto_serial_file = Some(path.into());
                    }
                }
            }
        }
        state
    }

    pub(super) fn enabled(&self) -> bool {
        self.operation.is_some()
    }

    pub(super) fn pointer_down(&mut self, serial: u32) {
        self.pointer = Some(serial);
        if self.enabled() {
            say(&format!("interactive pointer serial {serial}"));
        }
    }

    pub(super) fn touch_down(&mut self, serial: u32) {
        self.touch = Some(serial);
        if self.enabled() {
            say(&format!("interactive touch serial {serial}"));
        }
    }

    /// Sends one request from a fixture-controlled background trigger. This is
    /// solely for the two-client authorization test: the pointer grab keeps
    /// keyboard focus on its owner, so another client cannot honestly be
    /// prompted through a key event while that grab is active.
    pub(super) fn spawn_auto_request(
        &self,
        connection: Connection,
        toplevel: XdgToplevel,
        seat: wl_seat::WlSeat,
    ) {
        let (Some(operation), Some(path)) = (self.operation, self.auto_serial_file.clone()) else {
            return;
        };
        std::thread::spawn(move || {
            for _ in 0..1_000 {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(serial) = text.trim().parse::<u32>() {
                        match operation {
                            Operation::Move => toplevel._move(&seat, serial),
                            Operation::Resize => {
                                toplevel.resize(&seat, serial, ResizeEdge::BottomRight)
                            }
                        }
                        connection
                            .flush()
                            .expect("flush automatic interactive request");
                        say(&format!("interactive auto request {operation:?} {serial}"));
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            say("interactive auto request timed out");
        });
    }

    pub(super) fn key(
        &self,
        key: u32,
        toplevel: &XdgToplevel,
        seat: &wl_seat::WlSeat,
    ) -> Option<String> {
        let operation = self.operation?;
        let serial = match key {
            59 => self.pointer?,
            60 => 0,
            61 => self.pointer?.wrapping_add(1),
            62 => self.touch?,
            63 => std::fs::read_to_string(self.serial_file.as_ref()?)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
            _ => return None,
        };
        match operation {
            Operation::Move => toplevel._move(seat, serial),
            Operation::Resize => toplevel.resize(seat, serial, ResizeEdge::BottomRight),
        }
        say(&format!("interactive request {operation:?} {serial}"));
        Some(format!("interactive fence {key} {serial}"))
    }
}

impl Dispatch<wl_callback::WlCallback, String> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        marker: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        say(marker);
    }
}
