//! The wifi join dialog — the keyboard the LNK panel is not allowed to
//! have.
//!
//! A dock panel takes no keyboard focus, by protocol design and not by
//! omission: `chonk_dock_widget::panel`'s vocabulary is pointer events
//! and nothing else, and the only key a panel ever "sees" is the
//! Escape the shell grabs to dismiss it. That is the right rule — a
//! popover that could grab the keyboard is a popover that can phish —
//! and it leaves exactly one hole: joining a *new* secured network
//! needs a passphrase, and a passphrase needs a keyboard.
//!
//! So the link panel does not try. Clicking an unknown secured network
//! spawns this, a real window with a real focus, and the panel learns
//! what happened the way it learns everything else — from the next
//! `nmcli dev wifi list` sample.
//!
//! # The command, and where the secret is not
//!
//! | Step | Argv | Secret |
//! |---|---|---|
//! | join | `nmcli --ask dev wifi connect <ssid>` | on stdin |
//!
//! The obvious spelling — `nmcli dev wifi connect <ssid> password <pass>`
//! — puts the passphrase in the process's argv, and argv is world
//! readable through `/proc/<pid>/cmdline` for the whole life of the
//! process. Any user on the machine, and anything that samples the
//! process table, can read it. That is precisely why the panel refuses
//! to handle the secret itself and hands the job to a separate window,
//! and it would be a poor joke to then leak it here.
//!
//! `--ask` makes nmcli prompt for the secret instead, and a prompt
//! reads standard input. Verified against nmcli 1.58.1: with stdin a
//! pipe rather than a terminal, `nmcli --ask` consumes its prompts from
//! the pipe (`printf 'X\n' | nmcli --ask dev wifi connect` reads `X` as
//! the SSID it was not given). So the passphrase goes down a pipe to a
//! child this process spawned, and appears in no argument list.
//!
//! One consequence, and it is the reason `main` sends the child's
//! stdout to `/dev/null`: a prompt read from a pipe has no terminal on
//! which to disable echo, so nmcli's own prompt handling is the single
//! place the passphrase could come back out. Only stderr — where the
//! errors are — is captured, and [`dialog::clean_reason`] caps even
//! that before a pixel is drawn.
//!
//! # Shape
//!
//! The same split every instrument in this desktop uses, for the same
//! reason: [`dialog`] folds input into state and returns an *intent*,
//! [`render`] turns state into pixels, [`keys`] turns keysyms into
//! keystrokes, and all three are pure. `main.rs` owns the X
//! connection and the one `Command`, and is the only file here that
//! could block or spawn. Everything interesting is therefore testable
//! on a machine with no X server, no NetworkManager and no radio —
//! which is the machine this was written on.

pub mod dialog;
pub mod keys;
pub mod render;
