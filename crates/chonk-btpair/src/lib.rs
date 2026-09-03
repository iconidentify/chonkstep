//! The Bluetooth pairing dialog, minus its window.
//!
//! The split every dialog in this desktop uses, and for the same
//! reason its sibling `chonk-netjoin` uses it: [`pair`] folds
//! `bluetoothctl`'s output into state and returns an *intent*,
//! [`render`] turns that state into pixels, and both are pure.
//! `main.rs` owns the X connection and the one `Command`, and is the
//! only file here that can block or spawn.
//!
//! That is what makes this window reviewable at all on the machine it
//! was written on, which has no Bluetooth controller: every phase —
//! the discovery list, the passkey confirmation, the failures — can be
//! rasterized from canned state with no X server, no BlueZ and no
//! radio. `cargo run -p chonk-btpair --example preview -- /tmp/sheet`
//! is that review, and the `render` tests are it as assertions.
//!
//! The window belongs to the binary, not the library: it is the one
//! part of this crate that cannot be exercised headless, and a library
//! that exported an X connection would invite someone to link it from
//! a process that promised not to hold one.

pub mod pair;
pub mod render;
