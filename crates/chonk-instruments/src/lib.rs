//! The six instruments that ship in chonkstep's dock: the analog clock,
//! network traffic, system load, sound, link (wifi), and power.
//!
//! Every one of them is a pure fold. `update(&Samples)` reads data the
//! dock's sampler threads already collected, `render` turns the folded
//! state into pixels through `wm-theme`'s pure renderers, and
//! `on_input` returns an [`Effect`](chonk_dock_widget::Effect) instead
//! of performing one. There is no entry point here from which a syscall
//! is reachable.
//!
//! # Why this is a crate and not a module
//!
//! It was a module — `chonk-shell`'s `widgets` — and the rule above was
//! written in its module doc for the whole life of the code that broke
//! it. On 2026-08-29 four of these six were still doing blocking file
//! I/O on the compositor's repaint thread and a fifth was shelling out
//! to `nmcli dev wifi`, whose default `--rescan auto` blocks for a real
//! hardware scan: ~3.6s of frozen desktop once every ~34s, reported by
//! the compositor's own stall watchdog as a display-driver fault.
//!
//! Moving them into a crate of their own buys exactly one thing, and it
//! is the thing that was missing: `clippy.toml` resolves per crate, so
//! this directory carries a lint that makes `std::fs::File`,
//! `std::process::Command`, `std::fs::{read, read_to_string, read_dir}`,
//! `std::thread::spawn` and `TcpStream::connect` build errors *here*
//! while leaving them available to the dock, which legitimately needs
//! all of them. "A widget must not do I/O" stopped being a convention
//! and became a compile error. Read that file; it is the argument.
//!
//! The `Cargo.toml` beside it is the other half: this crate can only
//! see the SDK, the theme, a pixel buffer and a text shaper. There is
//! nothing here to reach for even if the lint were removed.
//!
//! # What is not here
//!
//! * The sampler runtime — threads, `read_dir`, `Command` — is
//!   `chonk-shell`'s `widgets::sampling`. An instrument declares a
//!   [`Source`](chonk_dock_widget::Source); the dock executes it.
//! * Supervision (`SupervisedWidget`) and dock layout are
//!   `chonk-shell`'s. An instrument does not know where in the column
//!   it sits, or that it can be evicted.
//! * The renderers are `wm-theme`'s (`panel`, `clock`, `power`,
//!   `sysload`, `nettraffic`, `wifi`), which is what lets them be
//!   tested pixel-for-pixel with no live system behind them.

mod clock;
mod net;
mod power;
mod sound;
mod sysload;
mod wifi;

pub use clock::ClockWidget;
pub use net::NetTrafficWidget;
pub use power::PowerWidget;
pub use sound::SoundWidget;
pub use sysload::SysLoadWidget;
pub use wifi::WifiWidget;
