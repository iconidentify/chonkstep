//! User-facing configuration: keybindings, workspace count, default
//! focus policy.
//!
//! Kept as its own crate from the start (even though milestone 1's needs
//! are small) because config parsing is genuinely orthogonal to
//! windowing/rendering — a future control tool could reuse it without
//! pulling in either.
//!
//! Not yet implemented: milestone step 1 is workspace scaffolding only.

pub use wm_core::FocusPolicy;
