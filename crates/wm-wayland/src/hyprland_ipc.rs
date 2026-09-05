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

use smithay::input::keyboard::{xkb, Keysym};

use chonk_hyprland_ipc::dispatch::{Action, Fullscreen};
use chonk_hyprland_ipc::state::{Binding, Devices, Keyboard, Monitor, PointerDevice, Snapshot, Window, Workspace};
use chonk_hyprland_ipc::Server;
use wm_core::{Backend, BackendEvent, Lifecycle, WindowManager};
use wm_theme_api::Point;

use crate::state::{Compositor, ManagedSurface, WaylandBackend};

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
pub(crate) fn snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
) -> Snapshot {
    build_snapshot(wm, locked, session, true)
}

/// Snapshot retained only by the event differ. Bindings are request-only:
/// no Hyprland event compares them, so allocating hundreds of strings here
/// on every genuine state publication cannot affect one wire byte.
pub(crate) fn event_snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
) -> Snapshot {
    build_snapshot(wm, locked, session, false)
}

fn build_snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
    include_bindings: bool,
) -> Snapshot {
    tracing::trace!("constructing Hyprland IPC snapshot");
    let monitors_info = wm.monitors();
    let current = wm.current_workspace();

    let monitors: Vec<Monitor> = monitors_info
        .iter()
        .enumerate()
        .map(|(index, info)| Monitor {
            id: i32::try_from(index).unwrap_or(i32::MAX),
            name: info.name.clone(),
            description: info.identity.clone().unwrap_or_else(|| info.name.clone()),
            x: info.geometry.pos.x,
            y: info.geometry.pos.y,
            width: i32::try_from(info.geometry.size.w).unwrap_or(i32::MAX),
            height: i32::try_from(info.geometry.size.h).unwrap_or(i32::MAX),
            scale: wm.backend().monitor_scales.get(index).copied().unwrap_or(1.0),
            // chonkstep has a single global current workspace rather
            // than one per output, so exactly one monitor is focused
            // and every monitor shows the same workspace. Saying
            // otherwise would put a workspace indicator on a bar
            // instance that no key can change.
            focused: index == focused_monitor_index(wm, &monitors_info),
            active_workspace: current,
            // The connector's own account of itself, mirrored onto the
            // backend from the same `Output` that answers `wl_output`
            // and `zwlr_output_management` (see
            // `Compositor::sync_monitor_outputs`). An index with no
            // mirror yet reports nothing rather than a placeholder.
            make: hardware(wm, index).map(|out| out.make.clone()).unwrap_or_default(),
            model: hardware(wm, index).map(|out| out.model.clone()).unwrap_or_default(),
            serial: hardware(wm, index).map(|out| out.serial.clone()).unwrap_or_default(),
            refresh_millihertz: hardware(wm, index).map_or(0, |out| out.refresh_millihertz),
            transform: hardware(wm, index).map_or(0, |out| out.transform),
            modes: hardware(wm, index).map(|out| out.modes.clone()).unwrap_or_default(),
        })
        .collect();

    // Count windows per workspace by the rule the control socket
    // already established: miniaturised windows count, withdrawn ones
    // do not, and dock and shell surfaces are not clients at all.
    let workspace_count = wm.workspace_count().max(1);
    let mut counts: Vec<u32> = vec![0; workspace_count];
    let mut workspace_monitors: Vec<Option<i32>> = vec![None; workspace_count];
    let mut workspace_fullscreen = vec![false; workspace_count];
    let mut windows = Vec::new();
    let focused = wm.focused_client();
    // The real focus history: `wm.focus_history()` is oldest-first, so
    // reversing it numbers the focused client 0, the one before it 1,
    // and so on — which is what the field is documented to mean
    // ("Position in the focus history, 0 = focused").
    //
    // This used to be the window manager's iteration order, which for a
    // `SlotMap` is creation order, so every client but the focused one
    // carried a plausible-looking fabricated number and a consumer
    // asking "what was the previously focused window" got an arbitrary
    // answer. Built once into a map rather than searched per client:
    // that keeps snapshot construction linear, which is what the
    // previous comment here was protecting.
    let history: std::collections::HashMap<wm_core::ClientId, i32> = wm
        .focus_history()
        .iter()
        .rev()
        .enumerate()
        .map(|(position, &id)| (id, i32::try_from(position).unwrap_or(i32::MAX)))
        .collect();
    // Clients that have never held focus have no position in it, and
    // are numbered after everything that has.
    let mut next_unfocused_history_id =
        i32::try_from(history.len()).unwrap_or(i32::MAX);
    for (id, client) in wm.iter_clients() {
        let id: wm_core::ClientId = id;
        let focus_history_id = match history.get(&id) {
            Some(&position) => position,
            None => {
                let current = next_unfocused_history_id;
                next_unfocused_history_id = next_unfocused_history_id.saturating_add(1);
                current
            }
        };
        if client.lifecycle == Lifecycle::Withdrawn {
            continue;
        }
        // A client can sit at an index past the current count for the
        // instant it takes the core to grow the list around a move. A
        // snapshot taken then must not drop the window on the floor.
        while counts.len() <= client.workspace {
            counts.push(0);
            workspace_monitors.push(None);
            workspace_fullscreen.push(false);
        }
        counts[client.workspace] += 1;

        let geometry = client.geometry;
        let centre = Point {
            x: geometry.pos.x + i32::try_from(geometry.size.w / 2).unwrap_or(0),
            y: geometry.pos.y + i32::try_from(geometry.size.h / 2).unwrap_or(0),
        };
        let monitor = i32::try_from(wm.monitor_index_at(centre)).unwrap_or(0);
        workspace_monitors[client.workspace].get_or_insert(monitor);
        workspace_fullscreen[client.workspace] |=
            client.flags.contains(wm_core::ClientFlags::FULLSCREEN);
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
            monitor,
            // `omarchy-debug-idle` reads `.pid`, and Hyprland's own
            // `pid` is the client's. Not every client sets it, and 0 is
            // Hyprland's own "unknown" — a number invented to fill the
            // gap would let a script signal the wrong process.
            pid: wm.backend().window_pid(client.window).and_then(|pid| i32::try_from(pid).ok()).unwrap_or(0),
            xwayland: wm.backend().windows.get(&client.window)
                .is_some_and(|record| matches!(record.surface, ManagedSurface::X11(_))),
            fullscreen: client.flags.contains(wm_core::ClientFlags::FULLSCREEN),
            hidden: client.lifecycle == Lifecycle::Miniaturized,
            urgent: client.flags.contains(wm_core::ClientFlags::URGENT),
            pinned: client.flags.contains(wm_core::ClientFlags::STICKY),
            inhibiting_idle: client.flags.contains(wm_core::ClientFlags::IDLE_INHIBIT),
            tags: client.tags.clone(),
            xdg_tag: String::new(),
            xdg_description: String::new(),
            focus_history_id,
        });
    }

    let workspaces: Vec<Workspace> = counts
        .iter()
        .enumerate()
        .map(|(index, count)| {
            let monitor_id = workspace_monitors[index]
                .or_else(|| monitors.iter().find(|monitor| monitor.active_workspace == index).map(|monitor| monitor.id))
                .unwrap_or(0);
            Workspace {
                index,
                monitor: monitors.iter().find(|monitor| monitor.id == monitor_id)
                    .map(|monitor| monitor.name.clone()).unwrap_or_default(),
                monitor_id,
                windows: *count,
                has_fullscreen: workspace_fullscreen[index],
            }
        })
        .collect();
    let bindings = if include_bindings {
        session.bindings.iter().map(|binding| ipc_binding(binding, session)).collect()
    } else {
        Vec::new()
    };
    // The keymap the seat is actually running, not the one the config
    // asked for: `XKB_DEFAULT_LAYOUT` overrides the file, and a reload
    // whose layout libxkbcommon rejected keeps the previous one. The
    // config value remains the fallback for a backend that installs no
    // keymap of its own.
    let layout = {
        let installed = wm.backend().keyboard_layout.clone();
        if installed.is_empty() {
            session.input.layout.clone().unwrap_or_else(|| "us".to_string())
        } else {
            installed
        }
    };
    let mut devices = Devices::default();
    for device in &wm.backend().input_devices {
        if device.keyboard {
            devices.keyboards.push(Keyboard {
                name: device.name.clone(), layout: layout.clone(), active_keymap: layout.clone(), active_layout_index: 0,
            });
        }
        let entry = PointerDevice { name: device.name.clone() };
        if device.pointer { devices.mice.push(entry.clone()); }
        if device.touch { devices.touch.push(entry.clone()); }
        if device.tablet { devices.tablets.push(entry.clone()); }
        if device.switch { devices.switches.push(entry); }
    }
    // The nested backend supplies one logical keyboard and pointer
    // through winit, but has no libinput hotplug event from which to
    // build an InputDeviceRecord. They are still real seat devices:
    // clients type and point through them, and reporting an empty
    // keyboard list makes Omarchy's layout widget poll forever.
    if devices.keyboards.is_empty() {
        devices.keyboards.push(Keyboard {
            name: "chonkstep-keyboard".into(),
            layout: layout.clone(),
            active_keymap: layout,
            active_layout_index: 0,
        });
    }
    if devices.mice.is_empty() {
        devices.mice.push(PointerDevice { name: "chonkstep-pointer".into() });
    }
    Snapshot {
        monitors,
        workspaces,
        windows,
        focused: focused.map(wm_core::ClientId::as_u64),
        locked,
        cursor_position: wm
            .backend()
            .pointer_position()
            .map(|point| (point.x, point.y)),
        bindings,
        config_errors: session.config_diagnostics.clone(),
        devices,
        system_info: if include_bindings {
            format!(
                "ChonkStep {}\nsource: {}\nconfig: {}\nworkspace: {}\noutputs: {}\n{}",
                env!("CARGO_PKG_VERSION"),
                chonk_build_info::SOURCE_ID,
                wm_config::config_path()
                    .map_or_else(|| "defaults (HOME unavailable)".to_string(), |path| path.display().to_string()),
                wm.current_workspace() + 1,
                monitors_info.len(),
                wm.backend().system_snapshot(),
            )
        } else {
            // Event clients never receive this request-only field.
            // Leaving it empty keeps /proc and protocol-object walks
            // off ordinary state publication.
            String::new()
        },
    }
}

fn ipc_binding(binding: &wm_config::Binding, session: &chonk_shell::startup::SessionState) -> Binding {
    let (dispatcher, argument) = match &binding.action {
        wm_config::Action::Run(name) => ("exec".to_string(), session.commands.get(name).map(|argv| argv.join(" ")).unwrap_or_default()),
        wm_config::Action::SpawnTerminal => ("exec".to_string(), session.terminal.as_ref().map(|argv| argv.join(" ")).unwrap_or_else(|| "foot".to_string())),
        wm_config::Action::Close => ("killactive".to_string(), String::new()),
        wm_config::Action::ToggleFullscreen => ("fullscreen".to_string(), "0".to_string()),
        wm_config::Action::Focus(direction) => ("movefocus".to_string(), match direction {
            wm_core::FocusDirection::Left => "l",
            wm_core::FocusDirection::Right => "r",
            wm_core::FocusDirection::Up => "u",
            wm_core::FocusDirection::Down => "d",
        }.to_string()),
        wm_config::Action::Workspace(index) => ("workspace".to_string(), (index + 1).to_string()),
        wm_config::Action::WorkspaceSend(index) => ("movetoworkspacesilent".to_string(), (index + 1).to_string()),
        wm_config::Action::WorkspaceCarry(index) => ("movetoworkspace".to_string(), (index + 1).to_string()),
        other => ("chonkstep".to_string(), format!("{other:?}")),
    };
    Binding {
        modifiers: hypr_modmask(binding.combo.modifiers), key: keysym_name(binding.combo.keysym),
        description: binding.description.clone().unwrap_or_default(), dispatcher, argument,
        locked: binding.locked, repeating: binding.repeating, release: binding.release,
    }
}

fn hypr_modmask(modifiers: wm_core::Modifiers) -> u32 {
    u32::from(modifiers.contains(wm_core::Modifiers::SHIFT))
        | (u32::from(modifiers.contains(wm_core::Modifiers::CONTROL)) << 2)
        | (u32::from(modifiers.contains(wm_core::Modifiers::ALT)) << 3)
        | (u32::from(modifiers.contains(wm_core::Modifiers::SUPER)) << 6)
}

/// The name `hyprctl binds` reports for a bound key, in both the plain
/// and the JSON encoding.
///
/// The consumer prints this string verbatim — `omarchy-menu-keybindings`,
/// the script behind `SUPER + K`, awk-splits the plain bind block and
/// puts the `key` field straight into a menu row — so a name that is
/// not a key name is a cheat sheet that has failed at the one thing it
/// exists for.
///
/// This used to be a seven-entry table with a `format!("0x{keysym:x}")`
/// catch-all, which took the entire `XF86` block, `F1`-`F12`,
/// `BackSpace`, `Delete`, `Insert`, `Home`, `End`, `Page_Up`,
/// `Page_Down` and `Print` — about a quarter of Omarchy's shipped
/// keymap, rendered as hexadecimal. `space` was worse: it fell inside
/// the printable-ASCII arm at `0x20` and came out as a literal space,
/// so `SUPER + SPACE`, the chord that opens Omarchy's own menu, listed
/// itself as blank.
///
/// libxkbcommon's registry is the authority instead. It has a name for
/// every keysym this compositor can bind, and the name it gives is the
/// spelling Hyprland's config syntax uses for that key — so the round
/// trip out through here and back through `wm_config`'s `keysym_for`
/// lands on the same key.
fn keysym_name(keysym: u32) -> String {
    // Printable ASCII above space keeps the existing behaviour: its own
    // character, uppercased. That half already agreed with Hyprland,
    // which prints `W` rather than the registry's `w`, and a working
    // case is not worth moving. `0x20` is deliberately NOT in this
    // range any more — it is `space`, and it has a name.
    if (0x21..=0x7e).contains(&keysym) {
        return char::from_u32(keysym).unwrap_or('?').to_ascii_uppercase().to_string();
    }
    // `keysym_get_name` has its own zero-padded hex fallback for a
    // keysym it does not know (`0x0ffffffe`), so this rarely fires —
    // but its documented failure answer is an empty string, and a
    // blank `key` field reads as "no key", which is exactly the
    // failure `space` used to have. The hex is ugly on purpose: an
    // unmappable value should stay visibly unmappable.
    let name = xkb::keysym_get_name(Keysym::new(keysym));
    if name.is_empty() {
        format!("0x{keysym:x}")
    } else {
        name
    }
}

/// The display-hardware mirror for the monitor at `index`, if the
/// output layout has been synced since that monitor appeared.
fn hardware(wm: &WindowManager<WaylandBackend>, index: usize) -> Option<&crate::state::MonitorOutput> {
    wm.backend().monitor_outputs.get(index)
}

fn focused_monitor_index(wm: &WindowManager<WaylandBackend>, monitors: &[wm_core::MonitorInfo]) -> usize {
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
/// Returns `true` when the request was valid and could be honoured. State
/// publication is deliberately independent: wm-core's semantic revision
/// says whether an accepted request actually changed the desktop.
pub(crate) fn apply(comp: &mut Compositor, action: Action) -> bool {
    let wm = &mut comp.wm;
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
        Action::MoveToWorkspace { window, workspace, follow } => {
            let client = match window {
                Some(id) => client_of(wm, id),
                None => wm.focused_client(),
            };
            match (client, window, follow) {
                // `carry_focused_to_workspace` is exactly move + switch
                // + activate, and it grows the row on demand.
                (Some(_), None, true) => {
                    wm.carry_focused_to_workspace(workspace);
                    true
                }
                // A named target is moved without stealing attention;
                // the silent verb does the same for the active target.
                (Some(client), _, _) => {
                    wm.move_client_to_workspace(client, workspace);
                    true
                }
                (None, _, _) => false,
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
        Action::CycleFocus { forward } => wm.focus_adjacent_client(forward),
        Action::FocusDirection(direction) => wm.focus_direction(match direction {
            chonk_hyprland_ipc::dispatch::Direction::Left => wm_core::FocusDirection::Left,
            chonk_hyprland_ipc::dispatch::Direction::Right => wm_core::FocusDirection::Right,
            chonk_hyprland_ipc::dispatch::Direction::Up => wm_core::FocusDirection::Up,
            chonk_hyprland_ipc::dispatch::Direction::Down => wm_core::FocusDirection::Down,
        }),
        Action::MoveWindow { window, x, y, relative } => match client_of(wm, window) {
            Some(id) => {
                let Some(client) = wm.client(id) else { return false };
                let mut geometry = client.geometry;
                geometry.pos = if relative {
                    Point::new(geometry.pos.x.saturating_add(x), geometry.pos.y.saturating_add(y))
                } else { Point::new(x, y) };
                wm.set_client_content_geometry(id, geometry);
                true
            }
            None => false,
        },
        Action::ResizeWindow { window, width, height, relative } => match client_of(wm, window) {
            Some(id) => {
                let Some(client) = wm.client(id) else { return false };
                let width = if relative { i64::from(client.geometry.size.w) + i64::from(width) } else { i64::from(width) };
                let height = if relative { i64::from(client.geometry.size.h) + i64::from(height) } else { i64::from(height) };
                if width <= 0 || height <= 0 { return false; }
                wm.resize_client_content(id, wm_theme_api::Size::new(width.min(u32::MAX as i64) as u32, height.min(u32::MAX as i64) as u32));
                true
            }
            None => false,
        },
        Action::CenterWindow(window) => client_of(wm, window).is_some_and(|id| wm.center_client(id)),
        Action::RaiseWindow(window) => client_of(wm, window).is_some_and(|id| wm.raise_client_to_top(id)),
        Action::SetPinned { window, pinned } => match client_of(wm, window) {
            Some(id) => {
                let current = wm.client(id).is_some_and(|client| client.flags.contains(wm_core::ClientFlags::STICKY));
                wm.set_client_pinned(id, pinned.unwrap_or(!current))
            }
            None => false,
        },
        Action::SetTag { window, tag, present } => client_of(wm, window).is_some_and(|id| wm.set_client_tag(id, &tag, present)),
        Action::ConfirmFloating(window) => client_of(wm, window).is_some(),
        Action::SetMonitorScale { output, scale_120 } => comp.set_output_scale(&output, scale_120 as f64 / 120.0),
        Action::SetCursorHidden(hidden) => {
            let owner = hidden.then(|| {
                comp.seat
                    .get_keyboard()
                    .and_then(|keyboard| keyboard.current_focus())
                    .or_else(|| comp.seat.get_pointer().and_then(|pointer| pointer.current_focus()))
            });
            let owner = owner.flatten();
            let backend = comp.wm.backend_mut();
            let changed = backend.cursor_hidden != hidden;
            backend.cursor_hidden = hidden;
            backend.cursor_hidden_owner = if hidden {
                owner.or_else(|| backend.cursor_hidden_owner.clone())
            } else {
                None
            };
            if changed {
                backend.mark_damaged();
            }
            true
        }
        Action::ReloadConfig => {
            comp.shell.reload_config(&mut comp.wm);
            true
        }
        Action::SetDiagnostic { name, enabled } => match wm.backend_mut().set_diagnostic(&name, enabled) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, diagnostic = name, "live diagnostic request refused");
                false
            }
        },
        Action::SetLogFilter(directive) => match wm.backend_mut().set_log_filter(&directive) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, filter = directive, "live log-filter request refused");
                false
            }
        },
        Action::ExecShell(command) => chonk_shell::spawn::spawn_detached("sh", &["-c", &command]).is_some(),
        Action::ExecArgv(argv) => {
            let Some((program, args)) = argv.split_first() else {
                return false;
            };
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            chonk_shell::spawn::spawn_detached(program, &args).is_some()
        }
    }
}

fn client_of(wm: &WindowManager<WaylandBackend>, id: u64) -> Option<wm_core::ClientId> {
    wm.iter_clients()
        .find(|(candidate, _): &(wm_core::ClientId, _)| candidate.as_u64() == id)
        .map(|(candidate, _)| candidate)
}

fn window_of(wm: &WindowManager<WaylandBackend>, id: u64) -> Option<<WaylandBackend as wm_core::Backend>::WindowId> {
    wm.iter_clients()
        .find(|(candidate, _): &(wm_core::ClientId, _)| candidate.as_u64() == id)
        .map(|(_, client)| client.window)
}

#[cfg(test)]
mod tests {
    use super::keysym_name;

    /// Key names the config format documents whose keysym is above
    /// printable ASCII — precisely the set the old `format!("0x{:x}")`
    /// catch-all swallowed, plus `space`, which the printable-ASCII arm
    /// rendered as a literal space. Listed rather than derived so that
    /// a key added to `wm-config` without a thought for the cheat sheet
    /// is caught here rather than shipped as hex.
    const NAMED_KEYS: &[&str] = &[
        // Navigation and editing: the old table had seven of these and
        // the catch-all took the rest.
        "space", "return", "tab", "escape", "left", "up", "right", "down", "home", "end",
        "pageup", "pagedown", "backspace", "delete", "insert", "print",
        // The function keys.
        "f1", "f9", "f12", "f23",
        // The XF86 block, which the catch-all took whole.
        "volumeup", "volumedown", "volumemute", "micmute", "playpause", "audiopause",
        "audiostop", "audionext", "audioprev", "brightnessup", "brightnessdown",
        "kbdbrightnessup", "kbdbrightnessdown", "kbdlightonoff", "poweroff", "search",
        "touchpadtoggle", "touchpadon", "touchpadoff", "calculator", "eject",
    ];

    #[test]
    fn no_named_key_is_reported_as_a_hex_number() {
        // The defect: everything above 0x7e came out as hexadecimal,
        // which is about a quarter of Omarchy's shipped keymap — every
        // XF86 chord, both F9 chords and SUPER+SHIFT+BACKSPACE.
        for spec in NAMED_KEYS {
            let combo = wm_config::parse_key(spec).unwrap_or_else(|| panic!("{spec} must parse"));
            let name = keysym_name(combo.keysym);
            assert!(
                !name.starts_with("0x"),
                "{spec} (keysym {:#x}) is reported to hyprctl as {name:?}",
                combo.keysym
            );
            assert!(!name.trim().is_empty(), "{spec} is reported as blank");
        }
    }

    #[test]
    fn space_is_named_rather_than_printed_as_a_space() {
        // `space` is keysym 0x20, inside the old printable-ASCII arm,
        // so it was rendered as a literal space character. `SUPER +
        // SPACE` opens Omarchy's menu and is the most-pressed chord on
        // the desktop; the cheat sheet listed it as blank.
        let space = wm_config::parse_key("space").expect("space parses");
        assert_eq!(space.keysym, 0x20);
        assert_eq!(keysym_name(space.keysym), "space");
    }

    #[test]
    fn every_named_key_is_reported_as_a_name_the_config_reader_can_read_back() {
        // What makes these names right rather than merely non-hex:
        // libxkbcommon's registry spelling is the one Hyprland's config
        // syntax uses, so a chord this compositor prints into a cheat
        // sheet can be pasted back into a Hyprland config and bind the
        // same key. That closes the loop, and it is the property that
        // would break first if the lookup were swapped for a hand table
        // again.
        for spec in NAMED_KEYS {
            let combo = wm_config::parse_key(spec).expect("documented key parses");
            let reported = keysym_name(combo.keysym);
            let round_trip = wm_config::hyprland::keys::spec_for(&format!("SUPER, {reported}"))
                .unwrap_or_else(|trouble| {
                    panic!("hyprctl reports {spec} as {reported:?}, which reads back as {trouble:?}")
                });
            let parsed = wm_config::parse_key(&round_trip)
                .unwrap_or_else(|| panic!("{round_trip:?} must parse"));
            assert_eq!(
                parsed.keysym, combo.keysym,
                "{spec} is reported as {reported:?}, which reads back as a different key"
            );
        }
    }

    #[test]
    fn printable_ascii_keeps_hyprlands_own_spelling() {
        // The half that already worked and is deliberately left alone:
        // Hyprland's `binds` prints `W`, not the registry's `w`.
        //
        // The punctuation here is the documented exception to the round
        // trip above. `keysym_for` insists on the word `minus` in a
        // spec — a literal `-` beside the `+` separator reads like a
        // typo — so `-` does not read back, and that is the right
        // trade: a cheat sheet row saying `SUPER + -` tells a user
        // which key to press, and `SUPER + minus` makes them think.
        for (spec, reported) in
            [("w", "W"), ("7", "7"), ("slash", "/"), ("minus", "-"), ("period", ".")]
        {
            let combo = wm_config::parse_key(spec).expect("parses");
            assert_eq!(keysym_name(combo.keysym), reported, "{spec}");
        }
    }

    #[test]
    fn a_keysym_with_no_name_stays_visibly_unmappable() {
        // libxkbcommon answers an unknown keysym with its own
        // zero-padded hex rather than the empty string its docs allow,
        // so this is what actually reaches the field. Either way the
        // requirement is the same and is what is asserted: never blank,
        // and obviously not a key name.
        for unknown in [0x0fff_fffe_u32, 0xdead_beef, 0xffff_ffff] {
            let name = keysym_name(unknown);
            assert!(name.starts_with("0x"), "{unknown:#x} -> {name:?}");
            assert!(!name.trim().is_empty(), "{unknown:#x} -> blank");
        }
    }
}
