//! The `.socket2.sock` event stream: `EVENT>>DATA\n`.
//!
//! This is the half that makes Omarchy's bar *live* rather than merely
//! rendered. Quickshell queries `j/monitors`, `j/workspaces` and
//! `j/clients` exactly once at startup and then never again unless an
//! event tells it to; a compositor that answers every query perfectly
//! and emits no events produces a bar that is correct for one instant
//! and frozen thereafter.
//!
//! # Why a differ rather than call sites
//!
//! Hyprland emits these events from inside the operations that cause
//! them. This module instead compares consecutive [`Snapshot`]s and
//! derives the events from what changed.
//!
//! That is a deliberate choice and it is the one that upholds "never a
//! cache that can drift". Emitting from call sites means every future
//! change to `wm-core` — every new way a window can move, be retitled,
//! or change workspace — is a place someone must remember to add an
//! emit, and the failure mode of forgetting is a bar that is silently
//! stale. Deriving from state means the events are a *function of* the
//! truth: a change that no call site knows to announce still produces
//! the right event, because the snapshot it is computed from already
//! reflects it.
//!
//! The retained previous snapshot is not a cache. Nothing is ever
//! *served* from it — every query in [`crate::server`] reads a fresh
//! snapshot — and it is used only to answer "what is different now?".
//! This is exactly the role `ControlSocket::note` plays for the
//! chonkstep control socket, and it is deliberately the same shape.
//!
//! # Ordering
//!
//! Order matters to Quickshell, which builds an object graph as events
//! arrive and warns (and drops the event) when one references something
//! it has not been told about yet — "Got openwindow for workspace N
//! which was not previously tracked." So the emission order below is
//! creations, then moves, then destructions:
//!
//! 1. `monitoraddedv2`, `createworkspacev2` — things others will refer to
//! 2. `openwindow` — needs its workspace to exist
//! 3. `movewindowv2`, `windowtitlev2`, `urgent` — need their window to exist
//! 4. `workspacev2`, `focusedmon`, `activewindowv2`, `fullscreen` — focus
//! 5. `closewindow`, `destroyworkspacev2`, `monitorremoved` — removals last
//!
//! # Address formatting is not cosmetic
//!
//! Hyprland writes addresses in event payloads as bare lowercase hex
//! **without** the `0x` that appears in `j/clients` JSON. Quickshell
//! parses both with base-16 `toULongLong`, which accepts either, and
//! then matches them *numerically* — so the two forms agreeing in value
//! is what matters, and they do. This module writes the bare form to
//! match Hyprland byte for byte, because `socat` transcripts get
//! compared against real ones by people debugging bars.

use crate::state::{Snapshot, Window, Workspace};

/// One line of the event stream, already formatted.
///
/// A newtype rather than a `String` so that the newline discipline
/// lives in one place: [`Event::line`] appends exactly one `\n` and the
/// payload is validated to contain none, because a payload with an
/// embedded newline would split into two frames and desynchronise every
/// reader on the socket. Window titles are attacker-controlled — a web
/// page picks its own `<title>` — so this is a real input, not a
/// theoretical one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    name: &'static str,
    data: String,
}

impl Event {
    fn new(name: &'static str, data: impl Into<String>) -> Event {
        let data: String = data.into();
        // Strip rather than reject. A title containing a newline is a
        // perfectly legal window title; dropping the event would lose a
        // real state change, and passing it through would corrupt the
        // stream for every client. Replacing the byte keeps the frame
        // intact and the meaning close enough for a bar label.
        let data = data.replace(['\n', '\r'], " ");
        Event { name, data }
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn data(&self) -> &str {
        &self.data
    }

    /// The wire line, newline included.
    pub fn line(&self) -> String {
        format!("{}>>{}\n", self.name, self.data)
    }
}

/// Hyprland's bare-hex address form, as used in event payloads.
fn address(window: &Window) -> String {
    format!("{:x}", window.id)
}

/// Derives the event stream from successive snapshots.
///
/// Hold one of these per compositor, not per client: the events are a
/// property of the desktop, and every connected client sees the same
/// sequence.
#[derive(Debug, Default)]
pub struct Differ {
    previous: Option<Snapshot>,
}

impl Differ {
    pub fn new() -> Differ {
        Differ::default()
    }

    /// Compare `now` against the last snapshot and return what changed.
    ///
    /// The first call establishes a baseline and emits nothing: a client
    /// that connects gets its state from the `j/` queries it makes on
    /// connect, and replaying the whole desktop as a burst of
    /// `openwindow` events would tell it things it already knows.
    pub fn diff(&mut self, now: &Snapshot) -> Vec<Event> {
        self.diff_owned(now.clone())
    }

    /// Owned form of [`Self::diff`] for producers which just built the
    /// snapshot. Moving it into the baseline avoids cloning every
    /// monitor, workspace, window, binding and device merely so the
    /// differ can remember the same value for its next comparison.
    pub fn diff_owned(&mut self, now: Snapshot) -> Vec<Event> {
        let Some(previous) = self.previous.replace(now) else {
            return Vec::new();
        };
        let now = self.previous.as_ref().expect("the snapshot was installed above");
        let mut events = Vec::new();

        // --- 1. additions others will refer to -------------------------
        for monitor in &now.monitors {
            if !previous.monitors.iter().any(|old| old.id == monitor.id) {
                events.push(Event::new(
                    "monitoraddedv2",
                    format!("{},{},{}", monitor.id, monitor.name, monitor.description),
                ));
            }
        }
        for workspace in &now.workspaces {
            if !previous.workspaces.iter().any(|old| old.index == workspace.index) {
                events.push(Event::new(
                    "createworkspacev2",
                    format!("{},{}", workspace.hypr_id(), workspace.hypr_name()),
                ));
            }
        }

        // --- 2. windows that appeared ---------------------------------
        for window in &now.windows {
            if previous.windows.iter().any(|old| old.id == window.id) {
                continue;
            }
            let workspace = workspace_name(now, window.workspace);
            events.push(Event::new(
                "openwindow",
                format!("{},{},{},{}", address(window), workspace, window.class, window.title),
            ));
        }
        for workspace in &now.workspaces {
            let Some(old) = previous.workspaces.iter().find(|old| old.index == workspace.index) else { continue };
            if old.monitor_id != workspace.monitor_id || old.monitor != workspace.monitor {
                events.push(Event::new(
                    "moveworkspacev2",
                    format!("{},{},{}", workspace.hypr_id(), workspace.hypr_name(), workspace.monitor),
                ));
            }
        }

        for keyboard in &now.devices.keyboards {
            let old = previous.devices.keyboards.iter().find(|old| old.name == keyboard.name);
            if old.is_some_and(|old| old.active_keymap != keyboard.active_keymap
                || old.active_layout_index != keyboard.active_layout_index)
            {
                events.push(Event::new("activelayout", format!("{},{}", keyboard.name, keyboard.active_keymap)));
            }
        }

        // --- 3. per-window changes ------------------------------------
        for window in &now.windows {
            let Some(old) = previous.windows.iter().find(|old| old.id == window.id) else {
                continue;
            };
            if old.workspace != window.workspace {
                let workspace = now.workspaces.iter().find(|w| w.index == window.workspace);
                events.push(Event::new(
                    "movewindowv2",
                    format!(
                        "{},{},{}",
                        address(window),
                        workspace.map_or(1, Workspace::hypr_id),
                        workspace.map_or_else(|| "1".to_string(), Workspace::hypr_name),
                    ),
                ));
            }
            if old.title != window.title {
                events.push(Event::new("windowtitlev2", format!("{},{}", address(window), window.title)));
                // Hyprland emits the v1 form too, and it carries the
                // title only. Kept because scripts that grep the raw
                // stream were written against it.
                events.push(Event::new("windowtitle", address(window)));
            }
            if !old.urgent && window.urgent {
                events.push(Event::new("urgent", address(window)));
            }
        }

        // --- 4. focus -------------------------------------------------
        let old_active = previous.active_workspace().map(Workspace::hypr_id);
        let new_active = now.active_workspace().map(Workspace::hypr_id);
        if old_active != new_active {
            if let Some(workspace) = now.active_workspace() {
                events.push(Event::new("workspacev2", format!("{},{}", workspace.hypr_id(), workspace.hypr_name())));
                events.push(Event::new("workspace", workspace.hypr_name()));
            }
        }

        let old_monitor = previous.focused_monitor().map(|monitor| monitor.name.clone());
        let new_monitor = now.focused_monitor().map(|monitor| monitor.name.clone());
        if old_monitor != new_monitor {
            if let Some(monitor) = now.focused_monitor() {
                // Hyprland writes a literal "?" when the monitor has no
                // workspace, and Quickshell special-cases that exact
                // string ("what the fuck", says its source). Matching it
                // costs nothing and diverging would send Quickshell
                // looking up a workspace named "".
                let workspace = now
                    .workspaces
                    .iter()
                    .find(|w| w.index == monitor.active_workspace)
                    .map_or_else(|| "?".to_string(), Workspace::hypr_name);
                events.push(Event::new("focusedmon", format!("{},{}", monitor.name, workspace)));
            }
        }

        if previous.focused != now.focused {
            match now.focused_window() {
                Some(window) => {
                    events.push(Event::new("activewindowv2", address(window)));
                    events.push(Event::new("activewindow", format!("{},{}", window.class, window.title)));
                }
                None => {
                    // Hyprland announces "nothing is focused" as an
                    // activewindowv2 with an empty payload and an
                    // activewindow with a bare comma. Quickshell's
                    // parse of the empty address fails its `ok` check
                    // and it correctly leaves the active toplevel alone;
                    // scripts watching the raw stream rely on seeing the
                    // transition at all.
                    events.push(Event::new("activewindowv2", ""));
                    events.push(Event::new("activewindow", ","));
                }
            }
        }

        let old_fullscreen = previous.active_workspace().is_some_and(|w| w.has_fullscreen);
        let new_fullscreen = now.active_workspace().is_some_and(|w| w.has_fullscreen);
        if old_fullscreen != new_fullscreen {
            events.push(Event::new("fullscreen", if new_fullscreen { "1" } else { "0" }));
        }

        // --- 5. removals ----------------------------------------------
        for window in &previous.windows {
            if !now.windows.iter().any(|new| new.id == window.id) {
                events.push(Event::new("closewindow", address(window)));
            }
        }
        for workspace in &previous.workspaces {
            if !now.workspaces.iter().any(|new| new.index == workspace.index) {
                events.push(Event::new(
                    "destroyworkspacev2",
                    format!("{},{}", workspace.hypr_id(), workspace.hypr_name()),
                ));
            }
        }
        for monitor in &previous.monitors {
            if !now.monitors.iter().any(|new| new.id == monitor.id) {
                events.push(Event::new("monitorremoved", monitor.name.clone()));
            }
        }

        events
    }

    /// Forget the baseline, so the next [`Differ::diff`] emits nothing.
    ///
    /// Used when the desktop has been reconfigured wholesale and clients
    /// have been told `configreloaded` — they re-query, so deriving a
    /// pile of deltas against a stale baseline would be noise.
    pub fn reset(&mut self) {
        self.previous = None;
    }
}

fn workspace_name(snapshot: &Snapshot, index: usize) -> String {
    snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.index == index)
        .map_or_else(|| "1".to_string(), Workspace::hypr_name)
}

/// The `configreloaded` event, which asks every client to re-query.
pub fn config_reloaded() -> Event {
    Event::new("configreloaded", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owned_diff_keeps_the_producers_snapshot_allocation() {
        let mut snapshot = Snapshot::default();
        snapshot.workspaces.push(Workspace {
            index: 0,
            monitor: "eDP-1".into(),
            monitor_id: 0,
            windows: 0,
            has_fullscreen: false,
        });
        let allocation = snapshot.workspaces.as_ptr();
        let mut differ = Differ::new();

        assert!(differ.diff_owned(snapshot).is_empty());
        assert_eq!(
            differ
                .previous
                .as_ref()
                .expect("owned snapshot becomes the baseline")
                .workspaces
                .as_ptr(),
            allocation,
            "the event baseline must take ownership rather than clone the snapshot"
        );
    }

    #[test]
    fn bindings_are_request_data_not_event_state() {
        let mut differ = Differ::new();
        assert!(differ.diff_owned(Snapshot::default()).is_empty());

        let mut snapshot = Snapshot::default();
        snapshot.bindings.push(crate::state::Binding {
            modifiers: 64,
            key: "Return".into(),
            description: "Terminal".into(),
            dispatcher: "exec".into(),
            argument: "alacritty".into(),
            locked: false,
            repeating: false,
            release: false,
        });
        assert!(
            differ.diff_owned(snapshot).is_empty(),
            "changing the binding table must never create a Hyprland event"
        );
    }
}
