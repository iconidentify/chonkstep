//! Hyprland's IPC protocol, served from chonkstep's own state.
//!
//! chonkstep is being made a drop-in replacement for Hyprland
//! underneath Omarchy. Fifty-three of Omarchy's 431 scripts ask the
//! compositor a question through `hyprctl`, and its Quickshell bar
//! reads Hyprland's two IPC sockets directly. Under chonkstep every one
//! of those takes a wrong branch. Patching them one at a time does not
//! scale and asks upstream to carry our differences forever; so instead
//! chonkstep speaks the protocol, well enough that Omarchy's
//! *unmodified* shell and the real `hyprctl` binary work against it.
//!
//! `docs/hyprland-ipc.md` is the companion document: the inventory this
//! was scoped from, what is served, what is refused, and how to turn it
//! off. Where that document and this crate disagree, **this crate is
//! right and the document has a bug worth reporting**.
//!
//! # The load-bearing design rule
//!
//! **A verb we cannot honour must fail, loudly, rather than succeed
//! plausibly.**
//!
//! Everything else here is detail; this is the rule the module is built
//! around, and it is worth stating first because it is counter-intuitive.
//! Hyprland is a tiling compositor and chonkstep is a floating one, so
//! a real part of Hyprland's vocabulary — `layoutmsg`, `togglesplit`,
//! `swapwindow`, `pseudo` — is meaningless here. Returning `ok` to
//! those would be easy and nothing would visibly break.
//!
//! It is the worst available option. A script that gets a confident
//! wrong answer takes an unexpected branch and carries the mistake
//! forward silently, surfacing later as behaviour nobody can explain. A
//! script that gets a clean error takes its error branch — which its
//! author wrote, and tested, and which usually falls back to something
//! that works. `docs/omarchy-integration.md` already catalogues four
//! Omarchy commands that fail under chonkstep by *doing nothing* rather
//! than by reporting anything, and records that the silence is the
//! expensive part, not the failure.
//!
//! Omarchy's own source settles the argument.
//! `omarchy-launch-or-focus` sends the Lua dispatch form and falls back
//! to the classic one when it fails; a server that refuses cleanly gets
//! the form it can serve, on the next line, for free.
//!
//! So: [`dispatch::Outcome::Unsupported`] is a first-class result, and
//! every refusal says what chonkstep *is* rather than what it lacks.
//!
//! # Shape
//!
//! - [`request`] — the wire grammar, measured from the real `hyprctl`
//!   and from Quickshell's source rather than taken from documentation.
//! - [`state`] — chonkstep's desktop in Hyprland's vocabulary, and the
//!   JSON field names Omarchy's `jq` filters are written against.
//! - [`event`] — the `.socket2.sock` stream, derived by diffing
//!   successive snapshots so that no future change to `wm-core` can
//!   forget to announce itself.
//! - [`dispatch`] — verbs in, [`dispatch::Action`]s or honest refusals out.
//! - [`server`] — the two sockets, the request table, and the
//!   never-block-on-a-client discipline this codebase has already
//!   shipped one bug for the want of.
//!
//! Nothing here depends on `wm-core`: the caller builds a
//! [`state::Snapshot`] and applies the [`dispatch::Action`]s. That keeps
//! every promise this crate makes to somebody else's binary testable
//! without booting a window manager.

pub mod dispatch;
pub mod event;
pub mod request;
pub mod server;
pub mod state;

pub use dispatch::{Action, Outcome};
pub use event::{Differ, Event};
pub use request::Request;
pub use server::Server;
pub use state::{Monitor, Snapshot, Window, Workspace};
