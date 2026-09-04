//! `chonk-toplevel` — the window list and the focus request, over
//! `zwlr_foreign_toplevel_management_v1`.
//!
//! # Why this exists
//!
//! Omarchy's `omarchy-launch-or-focus` is the script behind every
//! "open it, or raise it if it is already open" keybinding in the
//! distribution — the browser, the terminal, 1Password, Signal, every
//! `omarchy-launch-or-focus-tui` and `-webapp` wrapper. It finds the
//! window with
//!
//! ```text
//! hyprctl clients -j | jq -r '.[]|select((.class|test("\\b"+$p+"\\b";"i")) or …)|.address'
//! hyprctl dispatch 'hl.dsp.focus({ window = "address:…" })'
//! ```
//!
//! Under any compositor that is not Hyprland, `hyprctl` prints
//! `HYPRLAND_INSTANCE_SIGNATURE not set!` and exits 1, the address
//! comes back empty, and the script takes its `else` branch — so
//! "focus it if it is already open" reliably opens a second copy
//! instead. That is the failure this binary fixes, and it fixes it in
//! the compositor-agnostic way: `wlr-foreign-toplevel-management` is
//! the protocol whose entire purpose is letting an outside process
//! enumerate windows and ask for one to be activated, and chonkstep
//! advertises it at version 3 (see
//! `crates/wm-wayland/src/protocols.rs`). So does sway, so does
//! Wayfire, so does river, so does Hyprland itself.
//!
//! This is deliberately a *tool*, not a library and not part of the
//! desktop: it is a plain Wayland client with no privileges, and the
//! shim script in `omarchy/shims/` is its only intended caller. The
//! matching rule is a faithful copy of Omarchy's, so a shim built on
//! it picks the same window Omarchy's script would have picked on
//! Hyprland (see [`matches_pattern`]).
//!
//! # The commands
//!
//! ```text
//! chonk-toplevel list                # one line per window, tab-separated
//! chonk-toplevel activate <pattern>  # raise+focus the first match
//! chonk-toplevel close <pattern>     # politely close the first match
//! chonk-toplevel close-all           # politely close every window
//! ```
//!
//! Exit codes are the interface a shell script actually uses:
//!
//! | code | meaning |
//! |------|---------|
//! | 0 | did the thing (or, for `list`, printed the list — possibly empty) |
//! | 1 | no window matched; the caller should launch instead |
//! | 2 | no Wayland display, or the compositor does not advertise the protocol |
//! | 3 | usage error |
//!
//! `1` versus `2` is the distinction the shim needs and the reason
//! this does not simply print nothing on failure: "there is no such
//! window, launch one" and "I could not look" call for opposite
//! behaviour, and Omarchy's hyprctl pipeline conflates them.
//!
//! # Getting the whole list before answering
//!
//! A `zwlr_foreign_toplevel_manager_v1` does not answer a query; it
//! streams. Binding it makes the compositor announce every window it
//! already has, and then keep going forever. The protocol's own
//! end-of-burst marker is `stop` → `finished`, which is what this uses
//! — but the order matters against how chonkstep implements the
//! server side: handles are minted on the compositor's *next* refresh
//! pass, not inside `bind`, and `stop` drops the manager before that
//! pass runs. Sending `stop` in the same round trip as the bind
//! therefore yields `finished` and an empty list. Hence the shape of
//! [`collect`]: bind, round-trip until a `done` arrives (or a bounded
//! number of tries, since an empty desktop is a legitimate answer),
//! *then* `stop`, then drain to `finished`. Round trips rather than
//! timeouts, because a compositor always answers `wl_display.sync`
//! and a tool on a keybinding must not sit on a sleep.

use std::collections::HashMap;
use std::process::ExitCode;

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, State, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

/// The protocol version this client asks for, capped against whatever
/// the compositor advertises. 3 is what chonkstep offers and what adds
/// `parent`; nothing here needs `parent`, so a v1 compositor is served
/// just as well and the `min` below is the whole compatibility story.
const WANTED_VERSION: u32 = 3;

/// One window, as the protocol describes it.
#[derive(Clone, Debug, Default)]
struct Toplevel {
    app_id: String,
    title: String,
    activated: bool,
    minimized: bool,
    /// Set by the first `done` after this handle appeared. A handle
    /// that has been announced but not yet described carries an empty
    /// app id and title, and matching against those would be matching
    /// against nothing.
    described: bool,
}

impl Toplevel {
    /// The `list` line: tab-separated so `cut -f2` works and a title
    /// with spaces in it stays one field.
    fn line(&self, key: u32) -> String {
        let mut states = Vec::new();
        if self.activated {
            states.push("activated");
        }
        if self.minimized {
            states.push("minimized");
        }
        format!("{key}\t{}\t{}\t{}", self.app_id, self.title, states.join(","))
    }
}

/// The client's whole state: the two globals it needs and the windows
/// it has been told about.
#[derive(Default)]
struct App {
    manager: Option<ZwlrForeignToplevelManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    /// Keyed by the handle's protocol object id, which is stable for
    /// the handle's life and is what `list` prints — so a caller can
    /// correlate two invocations without this tool inventing an id of
    /// its own.
    toplevels: HashMap<u32, Toplevel>,
    /// Announcement order, so "the first match" means the same thing
    /// it means in Omarchy's `head -n1` over `hyprctl clients`.
    order: Vec<u32>,
    handles: HashMap<u32, ZwlrForeignToplevelHandleV1>,
    saw_done: bool,
    finished: bool,
}

impl App {
    /// The windows, in announcement order, skipping any handle that
    /// has not been described yet.
    fn described(&self) -> impl Iterator<Item = (u32, &Toplevel)> {
        self.order
            .iter()
            .filter_map(|key| self.toplevels.get(key).map(|top| (*key, top)))
            .filter(|(_, top)| top.described)
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "zwlr_foreign_toplevel_manager_v1" => {
                state.manager = Some(registry.bind(name, version.min(WANTED_VERSION), qh, ()));
            }
            // Only ever handed straight back to `activate`, which is
            // the one request in this protocol that takes a seat.
            // chonkstep ignores the argument (it has one seat), but the
            // protocol requires it and a compositor that does look at
            // it must be given something real.
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, version.min(1), qh, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(
        state: &mut Self,
        _manager: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                let key = toplevel.id().protocol_id();
                state.toplevels.insert(key, Toplevel::default());
                state.order.push(key);
                state.handles.insert(key, toplevel);
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    // The handle events arrive on the manager's queue but are
    // dispatched to the handle's own impl; this tells wayland-rs how
    // to give a freshly announced handle its user data.
    wayland_client::event_created_child!(App, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let key = handle.id().protocol_id();
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(top) = state.toplevels.get_mut(&key) {
                    top.title = title;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(top) = state.toplevels.get_mut(&key) {
                    top.app_id = app_id;
                }
            }
            // Native-endian u32 array, per the protocol.
            zwlr_foreign_toplevel_handle_v1::Event::State { state: bytes } => {
                let Some(top) = state.toplevels.get_mut(&key) else {
                    return;
                };
                top.activated = false;
                top.minimized = false;
                for chunk in bytes.as_chunks::<4>().0 {
                    let value = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    match State::try_from(value) {
                        Ok(State::Activated) => top.activated = true,
                        Ok(State::Minimized) => top.minimized = true,
                        _ => {}
                    }
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(top) = state.toplevels.get_mut(&key) {
                    top.described = true;
                }
                state.saw_done = true;
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.toplevels.remove(&key);
                state.order.retain(|entry| *entry != key);
                if let Some(handle) = state.handles.remove(&key) {
                    handle.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Omarchy's window-pattern rule, reimplemented literally.
///
/// The original is jq's `test("\\b" + $p + "\\b"; "i")` against a
/// window's class and title, so: case-insensitive, anywhere in the
/// string, but delimited by word boundaries. Three details are worth
/// stating because they are the difference between "the same window"
/// and "nearly the same window":
///
/// - The pattern is matched **literally**, not as a regex. Every
///   pattern Omarchy itself passes is a literal (`org.omarchy.btop`,
///   `Signal`, `1Password`), and in a regex the dots in those would
///   match any character — so literal matching is both what the
///   callers mean and the safer reading of a user-supplied string.
/// - A boundary is required at each end of the pattern whose own
///   character is a word character, and at a punctuation end nothing
///   is asserted. That is `\b` for every pattern that begins and ends
///   in a word character — which is every pattern Omarchy passes —
///   and a deliberate simplification for one that does not: a strict
///   `\b` beside a `.` asserts that the character on the *other* side
///   is a word character, where this simply lets the match through.
///   The simplification can only ever match more, never less, and it
///   keeps the rule explainable in one sentence.
/// - "Word character" is `[A-Za-z0-9_]`, as in the original regex.
///   Deliberately ASCII: `\b` in jq's Oniguruma is Unicode-aware, but
///   an app id or a window class is ASCII in practice and a
///   `char::is_alphanumeric` here would make a CJK title's boundaries
///   disagree with the distribution's.
fn matches_pattern(haystack: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    let hay = haystack.to_ascii_lowercase();
    let needle = pattern.to_ascii_lowercase();
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > hay.len() {
        return false;
    }
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let open = word(needle[0]);
    let close = word(needle[needle.len() - 1]);
    for start in 0..=(hay.len() - needle.len()) {
        if &hay[start..start + needle.len()] != needle {
            continue;
        }
        if open && start > 0 && word(hay[start - 1]) {
            continue;
        }
        let end = start + needle.len();
        if close && end < hay.len() && word(hay[end]) {
            continue;
        }
        return true;
    }
    false
}

/// Connects, binds, and gathers the window list — the whole
/// stream-to-snapshot dance the module header explains.
fn collect() -> Result<(wayland_client::EventQueue<App>, App), ExitCode> {
    let Ok(conn) = Connection::connect_to_env() else {
        eprintln!("chonk-toplevel: no Wayland display (is WAYLAND_DISPLAY set?)");
        return Err(ExitCode::from(2));
    };
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut app = App::default();
    conn.display().get_registry(&qh, ());
    if queue.roundtrip(&mut app).is_err() {
        eprintln!("chonk-toplevel: the compositor hung up during registry enumeration");
        return Err(ExitCode::from(2));
    }
    let Some(manager) = app.manager.clone() else {
        eprintln!(
            "chonk-toplevel: this compositor does not advertise \
             zwlr_foreign_toplevel_manager_v1, so there is no portable way to \
             enumerate its windows"
        );
        return Err(ExitCode::from(2));
    };

    // Round-trip until the announcement burst shows up. Bounded rather
    // than open-ended because an empty desktop never sends a `done`,
    // and that is a legitimate answer this must return promptly. Ten
    // is far more passes than the one it takes in practice; each costs
    // a `wl_display.sync` the compositor answers immediately.
    for _ in 0..10 {
        if queue.roundtrip(&mut app).is_err() {
            eprintln!("chonk-toplevel: the compositor hung up while listing windows");
            return Err(ExitCode::from(2));
        }
        if app.saw_done {
            break;
        }
    }

    // `stop` is the protocol's "that is the whole list" marker: the
    // compositor answers `finished` after everything it had already
    // queued. Handles stay live, so `activate`/`close` below still
    // work on what we collected.
    manager.stop();
    for _ in 0..10 {
        if queue.roundtrip(&mut app).is_err() {
            eprintln!("chonk-toplevel: the compositor hung up while closing the window list");
            return Err(ExitCode::from(2));
        }
        if app.finished {
            break;
        }
    }
    Ok((queue, app))
}

/// Flushes the requests just queued and waits for the compositor to
/// have seen them. Without this the process can exit — closing the
/// connection — before `activate` or `close` has left the socket.
fn commit(queue: &mut wayland_client::EventQueue<App>, app: &mut App) -> ExitCode {
    if queue.roundtrip(app).is_err() {
        eprintln!("chonk-toplevel: the compositor hung up before the request was acknowledged");
        return ExitCode::from(2);
    }
    let _ = queue.dispatch_pending(app);
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: chonk-toplevel list\n\
         \x20      chonk-toplevel activate <pattern>\n\
         \x20      chonk-toplevel close <pattern>\n\
         \x20      chonk-toplevel close-all\n\
         \n\
         <pattern> is matched case-insensitively, at word boundaries, against\n\
         each window's app id and title — the same rule omarchy-launch-or-focus\n\
         applies to a Hyprland window's class and title.\n\
         \n\
         exit: 0 done · 1 no window matched · 2 cannot look · 3 usage"
    );
    ExitCode::from(3)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, pattern) = match args.as_slice() {
        [command] => (command.as_str(), None),
        [command, pattern] => (command.as_str(), Some(pattern.as_str())),
        _ => return usage(),
    };

    match (command, pattern) {
        ("list", None) => {
            let (_queue, app) = match collect() {
                Ok(parts) => parts,
                Err(code) => return code,
            };
            for (key, top) in app.described() {
                println!("{}", top.line(key));
            }
            ExitCode::SUCCESS
        }
        ("activate", Some(pattern)) => {
            let (mut queue, mut app) = match collect() {
                Ok(parts) => parts,
                Err(code) => return code,
            };
            let Some(seat) = app.seat.clone() else {
                eprintln!("chonk-toplevel: the compositor advertises no wl_seat to activate through");
                return ExitCode::from(2);
            };
            let hit = app
                .described()
                .find(|(_, top)| {
                    matches_pattern(&top.app_id, pattern) || matches_pattern(&top.title, pattern)
                })
                .map(|(key, _)| key);
            let Some(key) = hit else {
                return ExitCode::from(1);
            };
            let Some(handle) = app.handles.get(&key).cloned() else {
                return ExitCode::from(1);
            };
            // Unminimize first: `activate` on chonkstep does
            // de-miniaturize (see `handle_activate_request` in
            // wm-core), but that is this compositor's courtesy rather
            // than the protocol's promise, and a shim that works
            // everywhere should ask for what it wants.
            handle.unset_minimized();
            handle.activate(&seat);
            let code = commit(&mut queue, &mut app);
            if code != ExitCode::SUCCESS {
                return code;
            }
            ExitCode::SUCCESS
        }
        ("close", Some(pattern)) => {
            let (mut queue, mut app) = match collect() {
                Ok(parts) => parts,
                Err(code) => return code,
            };
            let hit = app
                .described()
                .find(|(_, top)| {
                    matches_pattern(&top.app_id, pattern) || matches_pattern(&top.title, pattern)
                })
                .map(|(key, _)| key);
            let Some(key) = hit else {
                return ExitCode::from(1);
            };
            let Some(handle) = app.handles.get(&key).cloned() else {
                return ExitCode::from(1);
            };
            handle.close();
            commit(&mut queue, &mut app)
        }
        ("close-all", None) => {
            let (mut queue, mut app) = match collect() {
                Ok(parts) => parts,
                Err(code) => return code,
            };
            let keys: Vec<u32> = app.described().map(|(key, _)| key).collect();
            for key in &keys {
                if let Some(handle) = app.handles.get(key) {
                    handle.close();
                }
            }
            let code = commit(&mut queue, &mut app);
            if code != ExitCode::SUCCESS {
                return code;
            }
            // Nothing to close is not a failure — a session with no
            // windows is already in the state the caller asked for —
            // but say so, because a logout script that reports "closed
            // 0 windows" is easier to read than silence.
            println!("{}", keys.len());
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries_bracket_the_pattern() {
        // The whole string, and a whole word inside it.
        assert!(matches_pattern("Signal", "Signal"));
        assert!(matches_pattern("Signal — Chris", "Signal"));
        assert!(matches_pattern("chat via Signal", "Signal"));
        // Case folds, as jq's "i" flag does.
        assert!(matches_pattern("SIGNAL", "signal"));
        assert!(matches_pattern("signal", "SIGNAL"));
    }

    #[test]
    fn a_word_character_on_either_side_defeats_the_match() {
        // `\bSignal\b` must not fire inside a longer word.
        assert!(!matches_pattern("Signalling", "Signal"));
        assert!(!matches_pattern("resignal", "signal"));
        assert!(!matches_pattern("resignalling", "signal"));
        // But a non-word character is a boundary.
        assert!(matches_pattern("re-signal", "signal"));
        assert!(matches_pattern("signal.desktop", "signal"));
    }

    #[test]
    fn a_punctuation_edge_asserts_no_boundary_on_that_side() {
        // Trailing `.` is not a word character: nothing is asserted
        // after it, so the match may run straight into `btop`.
        assert!(matches_pattern("org.omarchy.btop", "omarchy."));
        // Leading `.` likewise asserts nothing before it.
        assert!(matches_pattern("~/.config", ".config"));
        // The other end is still a word character, and still asserts —
        // which is why a prefix does not match a longer word.
        assert!(!matches_pattern("~/.configuration", ".config"));
        assert!(!matches_pattern("~/.configuration", "config"));
    }

    #[test]
    fn the_omarchy_app_ids_match_themselves() {
        // The shape `omarchy-launch-or-focus-tui` builds:
        // `org.omarchy.<basename>`.
        assert!(matches_pattern("org.omarchy.btop", "org.omarchy.btop"));
        assert!(matches_pattern("org.omarchy.btop", "btop"));
        // And not a sibling that merely shares a prefix.
        assert!(!matches_pattern("org.omarchy.btop2", "btop"));
        // Dots are literal, not regex "any character".
        assert!(!matches_pattern("orgXomarchyXbtop", "org.omarchy.btop"));
    }

    #[test]
    fn nothing_matches_an_empty_pattern_or_an_oversized_one() {
        assert!(!matches_pattern("Signal", ""));
        assert!(!matches_pattern("", "Signal"));
        assert!(!matches_pattern("sig", "Signal"));
    }

    #[test]
    fn the_list_line_is_tab_separated_with_the_states_last() {
        let top = Toplevel {
            app_id: "foot".into(),
            title: "~ — foot".into(),
            activated: true,
            minimized: false,
            described: true,
        };
        assert_eq!(top.line(7), "7\tfoot\t~ — foot\tactivated");
        let plain = Toplevel { app_id: "foot".into(), described: true, ..Toplevel::default() };
        assert_eq!(plain.line(7), "7\tfoot\t\t");
    }
}
