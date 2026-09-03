//! Hosting the Hyprland IPC server inside the compositor.
//!
//! The protocol itself lives in `chonk-hyprland-ipc`, which knows
//! nothing about `wm-core` and can therefore be tested without booting
//! a window manager. This module is the other half: it reads the live
//! [`WindowManager`] into a `Snapshot`, applies the actions the
//! protocol decoded, and owns the sockets' place in the event loop.
//!
//! # Why the state is read fresh every time
//!
//! Everything served comes from `wm.iter_clients()` and
//! `wm.monitors()` at the moment of the request. There is no cache and
//! no shadow copy, because a cache is a thing that can be wrong and the
//! entire value of this server is that its answers are true — a bar
//! drawn from a stale window list is worse than no bar, since it is
//! confidently wrong rather than obviously absent.
//!
//! The one piece of retained state is the event differ's previous
//! snapshot, and that is a change *detector*, not a cache: nothing is
//! ever served from it. It is the same role `ControlSocket::note` plays
//! for chonkstep's own control socket.
//!
//! # Why this lives on the compositor's thread
//!
//! `sync_hyprland_sources` registers the sockets' file descriptors with
//! calloop using an empty callback, exactly as `sync_dock_sources` does
//! for the dockapp and control sockets. The callback's only job is to
//! end the `dispatch` wait; the servicing then happens in the ordinary
//! tick, where a `&mut Compositor` is already in hand.
//!
//! No thread, no channel, no mutex — and therefore no way for a client
//! to hold a lock the repaint thread wants. Every read and write on the
//! server's side is non-blocking by construction, so a wedged client
//! costs a bounded number of bytes per tick and nothing else. This is
//! the pattern `docs/control-socket.md` established and the one
//! `clippy.toml`'s incident report exists to protect.

use chonk_hyprland_ipc::dispatch::{Action, Fullscreen};
use chonk_hyprland_ipc::state::{Monitor, Snapshot, Window, Workspace};
use chonk_hyprland_ipc::Server;
use wm_core::{Backend, BackendEvent, Lifecycle, WindowManager};
use wm_theme_api::Point;

use crate::state::WaylandBackend;

/// Bring the server up, if the session asked for it.
///
/// Returns `None` when the feature is off or the sockets cannot be
/// bound. A bind failure is a warning and nothing more: impersonating
/// Hyprland is a convenience for other people's tooling, and it is not
/// worth failing a login over. This is the same posture
/// `ControlSocket::new` takes.
///
/// Must be called while the process is still single-threaded, because
/// it sets an environment variable — the same constraint that puts
/// `WAYLAND_DISPLAY`'s export where it is.
pub(crate) fn init() -> Option<Server> {
    if !Server::enabled() {
        return None;
    }

    let signature = Server::signature();
    match Server::bind(&signature) {
        Ok(server) => {
            // Both clients find the sockets through this variable and
            // nothing else: `hyprctl` exits with "HYPRLAND_INSTANCE_SIGNATURE
            // not set! (is hyprland running?)" without it, and
            // Quickshell's IPC singleton warns and gives up. It must be
            // set before `Shell::new`, which may autostart the bar.
            std::env::set_var(chonk_hyprland_ipc::server::SIGNATURE_ENV, &signature);
            // Logged in the shape `scripts/wayland-session.sh` greps
            // for, so the session can republish it into the systemd and
            // D-Bus activation environment the way it does
            // `WAYLAND_DISPLAY` — the portals and any D-Bus-activated
            // shell inherit nothing from this process.
            tracing::info!(
                signature = ?signature,
                directory = ?server.directory(),
                "hyprland ipc listening"
            );
            Some(server)
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "could not bind the hyprland ipc sockets; Omarchy's hyprctl-based \
                 tooling will fall back to its no-compositor branch"
            );
            None
        }
    }
}

/// Read the live window manager into the protocol's vocabulary.
/// `locked` is the compositor's session-lock state, which the window
/// manager does not carry — it lives on the `Compositor` — so it is
/// passed in. It reaches clients as `LOCK` in every monitor's
/// `solitaryBlockedBy`, the one field Hyprland's IPC exposes lock
/// state through and the one Omarchy's tooling reads.
pub(crate) fn snapshot(wm: &WindowManager<WaylandBackend>, locked: bool) -> Snapshot {
    let monitors_info = wm.monitors();
    let current = wm.current_workspace();

    let monitors: Vec<Monitor> = monitors_info
        .iter()
        .enumerate()
        .map(|(index, info)| Monitor {
            id: i32::try_from(index).unwrap_or(i32::MAX),
            name: info.name.clone(),
            description: info.name.clone(),
            x: info.geometry.pos.x,
            y: info.geometry.pos.y,
            width: i32::try_from(info.geometry.size.w).unwrap_or(i32::MAX),
            height: i32::try_from(info.geometry.size.h).unwrap_or(i32::MAX),
            // chonkstep has one UI scale, not a per-output one.
            scale: 1.0,
            // chonkstep has a single global current workspace rather
            // than one per output, so exactly one monitor is focused
            // and every monitor shows the same workspace. Saying
            // otherwise would put a workspace indicator on a bar
            // instance that no key can change.
            focused: index == focused_monitor_index(wm, &monitors_info),
            active_workspace: current,
        })
        .collect();

    // Count windows per workspace by the rule the control socket
    // already established: miniaturised windows count, withdrawn ones
    // do not, and dock and shell surfaces are not clients at all.
    let mut counts: Vec<u32> = vec![0; wm.workspace_count().max(1)];
    let mut windows = Vec::new();
    let focused = wm.focused_client();

    for (id, client) in wm.iter_clients() {
        let id: wm_core::ClientId = id;
        if client.lifecycle == Lifecycle::Withdrawn {
            continue;
        }
        // A client can sit at an index past the current count for the
        // instant it takes the core to grow the list around a move. A
        // snapshot taken then must not drop the window on the floor.
        while counts.len() <= client.workspace {
            counts.push(0);
        }
        counts[client.workspace] += 1;

        let geometry = client.geometry;
        let centre = Point {
            x: geometry.pos.x + i32::try_from(geometry.size.w / 2).unwrap_or(0),
            y: geometry.pos.y + i32::try_from(geometry.size.h / 2).unwrap_or(0),
        };

        windows.push(Window {
            id: id.as_u64(),
            title: client.title.clone(),
            class: client.class.clone(),
            x: geometry.pos.x,
            y: geometry.pos.y,
            width: i32::try_from(geometry.size.w).unwrap_or(0),
            height: i32::try_from(geometry.size.h).unwrap_or(0),
            workspace: client.workspace,
            // `Client::monitor` is an unset slotmap key: multi-monitor
            // policy resolves a window's output geometrically. Reading
            // the field would report every window on monitor zero.
            monitor: i32::try_from(wm.monitor_index_at(centre)).unwrap_or(0),
            // `omarchy-debug-idle` reads `.pid`, and Hyprland's own
            // `pid` is the client's. Not every client sets it, and 0 is
            // Hyprland's own "unknown" — a number invented to fill the
            // gap would let a script signal the wrong process.
            pid: wm
                .backend()
                .window_pid(client.window)
                .and_then(|pid| i32::try_from(pid).ok())
                .unwrap_or(0),
            xwayland: false,
            fullscreen: client.flags.contains(wm_core::ClientFlags::FULLSCREEN),
            hidden: client.lifecycle == Lifecycle::Miniaturized,
            urgent: client.flags.contains(wm_core::ClientFlags::URGENT),
            focus_history_id: i32::from(Some(id) != focused),
        });
    }

    let workspaces: Vec<Workspace> = counts
        .iter()
        .enumerate()
        .map(|(index, windows)| Workspace {
            index,
            monitor: monitors.first().map(|m| m.name.clone()).unwrap_or_default(),
            monitor_id: 0,
            windows: *windows,
            has_fullscreen: false,
        })
        .collect();

    Snapshot { monitors, workspaces, windows, focused: focused.map(wm_core::ClientId::as_u64), locked }
}

fn focused_monitor_index(
    wm: &WindowManager<WaylandBackend>,
    monitors: &[wm_core::MonitorInfo],
) -> usize {
    if monitors.is_empty() {
        return 0;
    }
    // The output the pointer is on, which is what the control socket
    // reports as `outputs.focused` and what a keyboard-summoned panel
    // belongs on.
    match wm.backend().pointer_position() {
        Some(point) => wm.monitor_index_at(point),
        None => monitors.iter().position(|monitor| monitor.primary).unwrap_or(0),
    }
}

/// Apply one decoded action to the window manager.
///
/// Returns `true` when something changed, so the caller knows to
/// publish a fresh snapshot in the same tick.
pub(crate) fn apply(wm: &mut WindowManager<WaylandBackend>, action: Action) -> bool {
    match action {
        Action::FocusWorkspace(index) => {
            // `dispatch::workspace_target` has already refused any
            // workspace that did not exist in the snapshot the response
            // was computed from, so reaching this branch means the
            // workspace count shrank between the answer and the apply.
            // It cannot today — chonkstep never destroys a workspace —
            // and the guard stays anyway, because the alternative to a
            // stale index here is a panic in `switch_workspace`.
            // No existence check here, deliberately, and the reason is
            // worth recording because the obvious guard is wrong and
            // was written first. `switch_workspace` grows the workspace
            // row on demand, so every non-negative index is reachable;
            // an `index >= workspace_count()` guard therefore refuses
            // switches that would have worked — and refuses them
            // *after* the protocol has already answered `ok`, because
            // the response is formed from the parse and the action is
            // applied afterwards. That is precisely the confident wrong
            // answer this module exists to prevent, produced by the
            // module itself: `hyprctl dispatch workspace 3` printed
            // `ok` and stayed on workspace 1 until this was removed.
            //
            // Anything genuinely unrepresentable — Hyprland's workspace
            // 0, its negative special-workspace ids — is rejected in
            // parsing by `workspace_index_from_hypr_id`, which happens
            // before the response is formed and so reports honestly.
            wm.switch_workspace(index);
            true
        }
        Action::FocusWindow(id) => match window_of(wm, id) {
            Some(window) => {
                wm.dispatch(BackendEvent::ActivateRequested(window));
                true
            }
            None => false,
        },
        Action::CloseWindow(id) => match client_of(wm, id) {
            Some(client) => {
                wm.close_client(client);
                true
            }
            None => false,
        },
        Action::KillActive => match wm.focused_client() {
            Some(client) => {
                wm.close_client(client);
                true
            }
            None => false,
        },
        Action::MoveToWorkspace { window, workspace } => {
            let client = match window {
                Some(id) => client_of(wm, id),
                None => wm.focused_client(),
            };
            match (client, window) {
                // Hyprland's `movetoworkspace` moves the window *and
                // follows it*; `movetoworkspacesilent` is the variant
                // that does not, and this module refuses that one
                // because chonkstep cannot move without following.
                // `carry_focused_to_workspace` is exactly move + switch
                // + activate, and it grows the row on demand.
                (Some(_), None) => {
                    wm.carry_focused_to_workspace(workspace);
                    true
                }
                // A named window that is not the focused one can only
                // be moved; there is nothing to carry.
                (Some(client), Some(_)) => {
                    wm.move_client_to_workspace(client, workspace);
                    true
                }
                (None, _) => false,
            }
        }
        Action::Fullscreen(which) => match wm.focused_client() {
            Some(client) => {
                match which {
                    Fullscreen::Toggle => wm.toggle_fullscreen(client),
                    Fullscreen::On => wm.fullscreen(client),
                    Fullscreen::Off => wm.unfullscreen(client),
                }
                true
            }
            None => false,
        },
        Action::CycleFocus { .. } => {
            // Alt-Tab is a held-modifier interaction in chonkstep, not a
            // single verb, so there is nothing honest to do here yet.
            // Reported as a no-op rather than faked.
            false
        }
        Action::Exec(command) => {
            // Hyprland's `exec` takes a shell command line, not an
            // argv, and Omarchy sends one (`exec -- bash -lc '...'`).
            // `spawn_detached` never waits — the workspace's
            // `clippy.toml` bans the three calls that would, and this
            // runs on the compositor's repaint thread.
            chonk_shell::spawn::spawn_detached("sh", &["-c", &command]).is_some()
        }
    }
}

fn client_of(wm: &WindowManager<WaylandBackend>, id: u64) -> Option<wm_core::ClientId> {
    wm.iter_clients()
        .find(|(candidate, _): &(wm_core::ClientId, _)| candidate.as_u64() == id)
        .map(|(candidate, _)| candidate)
}

fn window_of(
    wm: &WindowManager<WaylandBackend>,
    id: u64,
) -> Option<<WaylandBackend as wm_core::Backend>::WindowId> {
    wm.iter_clients()
        .find(|(candidate, _): &(wm_core::ClientId, _)| candidate.as_u64() == id)
        .map(|(_, client)| client.window)
}
