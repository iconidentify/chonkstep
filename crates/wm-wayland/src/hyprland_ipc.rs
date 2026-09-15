
use smithay::input::keyboard::{xkb, Keysym, Layout};

use chonk_hyprland_ipc::dispatch::{Action, Fullscreen, LayoutTarget, MonitorTarget};
use chonk_hyprland_ipc::state::{
    Binding, Devices, Keyboard, Monitor, PointerDevice, Snapshot, SpecialWorkspace, Window, Workspace,
};

// The wire crate restates the core's special-workspace bounds by value
// so it stays free of `wm-core`; this is where the two are held equal.
const _: () = assert!(chonk_hyprland_ipc::state::MAX_SPECIAL_WORKSPACES == wm_core::MAX_SPECIAL_WORKSPACES);
const _: () = assert!(chonk_hyprland_ipc::state::MAX_SPECIAL_NAME == wm_core::MAX_SPECIAL_NAME);
use chonk_hyprland_ipc::Server;
use wm_core::{Backend, BackendEvent, Lifecycle, WindowManager};
use wm_theme_api::{Point, Rect, Size};

use crate::state::{Compositor, ManagedSurface, WaylandBackend};

/// xdg-output advertises the configured output origin unchanged, and divides
/// its transformed extent by the output scale. Apply that same affine mapping
/// to IPC coordinates, scaling offsets from the owning output, never the global
/// origin or the whole desktop by one scale.
#[derive(Clone, Copy)]
struct OutputCoordinates {
    origin: Point,
    scale: f64,
}

impl OutputCoordinates {
    fn for_output(backend: &WaylandBackend, index: usize) -> Self {
        let origin = backend.monitors.get(index).map_or(Point::new(0, 0), |m| m.geometry.pos);
        let scale = backend.monitor_scales.get(index).copied()
            .filter(|scale| scale.is_finite() && *scale > 0.0).unwrap_or(1.0);
        Self { origin, scale }
    }

    fn logical_position(self, point: Point) -> Point {
        Point::new(
            (self.origin.x as f64 + (point.x as f64 - self.origin.x as f64) / self.scale).floor() as i32,
            (self.origin.y as f64 + (point.y as f64 - self.origin.y as f64) / self.scale).floor() as i32,
        )
    }

    fn physical_position(self, point: Point) -> Point {
        Point::new(
            (self.origin.x as f64 + (point.x as f64 - self.origin.x as f64) * self.scale).round() as i32,
            (self.origin.y as f64 + (point.y as f64 - self.origin.y as f64) * self.scale).round() as i32,
        )
    }

    fn logical_size(self, size: Size) -> Size {
        Size::new((size.w as f64 / self.scale) as u32, (size.h as f64 / self.scale) as u32)
    }

    fn physical_length(self, length: i64) -> i64 {
        (length as f64 * self.scale).round() as i64
    }
}

fn logical_monitor_index(backend: &WaylandBackend, point: Point) -> usize {
    backend.monitors.iter().enumerate().map(|(index, monitor)| {
        let coordinates = OutputCoordinates::for_output(backend, index);
        let origin = coordinates.origin;
        // Match xdg-output's rounded logical extent, including fractional scale.
        let width = (monitor.geometry.size.w as f64 / coordinates.scale).round();
        let height = (monitor.geometry.size.h as f64 / coordinates.scale).round();
        let dx = point.x as f64 - (point.x as f64).clamp(origin.x as f64, origin.x as f64 + width.max(1.0) - 1.0);
        let dy = point.y as f64 - (point.y as f64).clamp(origin.y as f64, origin.y as f64 + height.max(1.0) - 1.0);
        (index, dx * dx + dy * dy)
    }).min_by(|a, b| a.1.total_cmp(&b.1)).map_or(0, |(index, _)| index)
}

fn monitor_mode_size(size: Size, transform: i32) -> Size {
    if transform.rem_euclid(2) == 1 { Size::new(size.h, size.w) } else { size }
}

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
/// `shortcut_inhibit` is `Compositor::shortcut_inhibit_report`, which
/// lives beside the grants for the same reason and is served only by
/// `systeminfo`. `autoreload_paused` is `Shell::autoreload_paused`, the
/// live word over the session state's own line.
pub(crate) fn snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
    shortcut_inhibit: &str,
    autoreload_paused: bool,
) -> Snapshot {
    build_snapshot(wm, locked, session, Some(shortcut_inhibit), autoreload_paused)
}

/// Snapshot retained only by the event differ. Bindings are request-only:
/// no Hyprland event compares them, so allocating hundreds of strings here
/// on every genuine state publication cannot affect one wire byte.
pub(crate) fn event_snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
    autoreload_paused: bool,
) -> Snapshot {
    build_snapshot(wm, locked, session, None, autoreload_paused)
}

/// `system_info` is the request-only half: `Some` carries the
/// shortcut-inhibit report into it and asks for the bindings too.
fn build_snapshot(
    wm: &WindowManager<WaylandBackend>,
    locked: bool,
    session: &chonk_shell::startup::SessionState,
    system_info: Option<&str>,
    autoreload_paused: bool,
) -> Snapshot {
    tracing::trace!("constructing Hyprland IPC snapshot");
    let include_bindings = system_info.is_some();
    let monitors_info = wm.monitors();

    let monitors: Vec<Monitor> = monitors_info
        .iter()
        .enumerate()
        .map(|(index, info)| Monitor {
            id: i32::try_from(index).unwrap_or(i32::MAX),
            name: info.name.clone(),
            description: info.identity.clone().unwrap_or_else(|| info.name.clone()),
            x: info.geometry.pos.x,
            y: info.geometry.pos.y,
            // Hyprland reports untransformed physical mode dimensions; consumers
            // apply transform and scale to obtain xdg-output's logical extent.
            width: i32::try_from(monitor_mode_size(info.geometry.size,
                hardware(wm, index).map_or(0, |out| out.transform)).w).unwrap_or(i32::MAX),
            height: i32::try_from(monitor_mode_size(info.geometry.size,
                hardware(wm, index).map_or(0, |out| out.transform)).h).unwrap_or(i32::MAX),
            scale: wm.backend().monitor_scales.get(index).copied().unwrap_or(1.0),
            powered: hardware(wm, index).is_none_or(|out| out.powered),
            vrr_supported: hardware(wm, index).is_some_and(|out| out.vrr_supported),
            vrr_enabled: hardware(wm, index).is_some_and(|out| out.vrr_enabled),
            // Mac Spaces expose independently active display rows.
            focused: index == if wm.separate_spaces() { wm.active_output_index() } else { focused_monitor_index(wm, &monitors_info) },
            active_workspace: wm.active_workspace_on_output(index),
            special_workspace: wm.special_shown_on_output(index).and_then(|special| wm.special_name(special)).map(str::to_string),
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
    let mut workspace_monitors: Vec<Option<i32>> = (0..workspace_count)
        .map(|index| wm.workspace_output_index(index).map(|i| i as i32)).collect();
    let mut workspace_fullscreen = vec![false; workspace_count];
    let special_count = wm.special_workspaces().count();
    let mut special_counts = vec![0u32; special_count];
    let mut special_fullscreen = vec![false; special_count];
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
        let fullscreen = client.flags.contains(wm_core::ClientFlags::FULLSCREEN);
        // A special member is counted on its special workspace, not on
        // the numbered home it keeps for the way back.
        let special = client.special.filter(|&special| special < special_count);
        match special {
            Some(special) => {
                special_counts[special] += 1;
                special_fullscreen[special] |= fullscreen;
            }
            None => {
                counts[client.workspace] += 1;
                workspace_fullscreen[client.workspace] |= fullscreen;
            }
        }

        let output_index = wm.client_output_index(id);
        let record = wm.backend().windows.get(&client.window);
        let coordinates = OutputCoordinates::for_output(wm.backend(), output_index);
        let geometry = Rect::new(coordinates.logical_position(client.geometry.pos), coordinates.logical_size(client.geometry.size));
        let monitor = i32::try_from(output_index).unwrap_or(0);
        if special.is_none() {
            workspace_monitors[client.workspace].get_or_insert(monitor);
        }
        windows.push(Window {
            id: id.as_u64(),
            title: client.title.clone(),
            class: client.class.clone(),
            x: geometry.pos.x,
            y: geometry.pos.y,
            width: i32::try_from(geometry.size.w).unwrap_or(0),
            height: i32::try_from(geometry.size.h).unwrap_or(0),
            workspace: client.workspace,
            special: special.and_then(|special| wm.special_name(special)).map(str::to_string),
            // `Client::monitor` is an unset slotmap key: multi-monitor
            // policy resolves a window's output geometrically. Reading
            // the field would report every window on monitor zero.
            monitor,
            // `omarchy-debug-idle` reads `.pid`, and Hyprland's own
            // `pid` is the client's. Not every client sets it, and 0 is
            // Hyprland's own "unknown" — a number invented to fill the
            // gap would let a script signal the wrong process.
            pid: wm.backend().window_pid(client.window).and_then(|pid| i32::try_from(pid).ok()).unwrap_or(0),
            floating: !wm.is_layout_managed(id),
            xwayland: record.is_some_and(|record| matches!(record.surface, ManagedSurface::X11(_))),
            fullscreen,
            maximized: client
                .flags
                .contains(wm_core::ClientFlags::MAXIMIZED_H | wm_core::ClientFlags::MAXIMIZED_V),
            client_fullscreen: client.flags.contains(wm_core::ClientFlags::CLIENT_FULLSCREEN),
            hidden: client.lifecycle == Lifecycle::Miniaturized,
            urgent: client.flags.contains(wm_core::ClientFlags::URGENT),
            pinned: client.flags.contains(wm_core::ClientFlags::STICKY),
            // The rule's evaluated answer, not whether one matched:
            // `omarchy-debug-idle` reads this to explain why a machine
            // stays awake, and a windowed Steam library does not.
            inhibiting_idle: wm.client_inhibits_idle(id),
            tags: client.tags.clone(),
            // `xdg_toplevel_tag_v1`, as the client set it and the
            // backend bounded it; empty for a window that never set
            // one, which is every XWayland window.
            xdg_tag: record.and_then(|record| record.xdg_tag.clone()).unwrap_or_default(),
            xdg_description: record.and_then(|record| record.xdg_description.clone()).unwrap_or_default(),
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
                layout: wm.workspace_layout(index).compatible_name().into(),
                monitor: monitors.iter().find(|monitor| monitor.id == monitor_id)
                    .map(|monitor| monitor.name.clone()).unwrap_or_default(),
                monitor_id,
                windows: *count,
                has_fullscreen: workspace_fullscreen[index],
            }
        })
        .collect();
    // Special workspaces, with the output each is shown on. One not
    // shown anywhere belongs, for the wire, to the output a toggle would
    // show it on.
    let specials: Vec<SpecialWorkspace> = wm
        .special_workspaces()
        .map(|(index, name)| {
            let shown_on = (0..monitors.len()).find(|&output| wm.special_shown_on_output(output) == Some(index));
            let output = shown_on.unwrap_or_else(|| wm.active_output_index());
            SpecialWorkspace {
                index,
                name: name.to_string(),
                layout: wm.special_layout_mode(index).compatible_name().into(),
                monitor: shown_on.and_then(|output| monitors.get(output)).map(|monitor| monitor.name.clone()),
                monitor_id: monitors.get(output).map_or(0, |monitor| monitor.id),
                windows: special_counts.get(index).copied().unwrap_or(0),
                has_fullscreen: special_fullscreen.get(index).copied().unwrap_or(false),
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
    let configured_layout = {
        let installed = wm.backend().keyboard_layout.clone();
        if installed.is_empty() {
            session.input.layout.clone().unwrap_or_else(|| "us".to_string())
        } else {
            installed
        }
    };
    let active_layout_index = wm.backend().active_keyboard_layout;
    let active_keymap = wm
        .backend()
        .keyboard_layouts
        .get(active_layout_index as usize)
        .cloned()
        .unwrap_or_else(|| configured_layout.clone());
    // `layout` and `active_keymap` answer different questions. `layout` is
    // the list the keymap was built from ("us,de"), which is how a shell
    // tells whether there is anything to switch between; `active_keymap`
    // names the group in force now, which is what a label shows.
    let mut devices = Devices::default();
    for device in &wm.backend().input_devices {
        if device.keyboard {
            devices.keyboards.push(Keyboard {
                name: device.name.clone(),
                layout: configured_layout.clone(),
                active_keymap: active_keymap.clone(),
                active_layout_index,
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
            name: chonk_hyprland_ipc::state::NESTED_KEYBOARD.into(),
            layout: configured_layout.clone(),
            active_keymap,
            active_layout_index,
        });
    }
    if devices.mice.is_empty() {
        devices.mice.push(PointerDevice { name: chonk_hyprland_ipc::state::NESTED_POINTER.into() });
    }
    // Parked outputs: listed by `monitors all` alone, with `disabled:
    // true`, at no layout position and outside the id space the layout
    // monitors occupy.
    let disabled_monitors: Vec<Monitor> = wm
        .backend()
        .parked_monitors
        .iter()
        .map(|parked| Monitor {
            id: -1,
            name: parked.name.clone(),
            description: parked.identity.clone().unwrap_or_else(|| parked.name.clone()),
            x: 0,
            y: 0,
            width: parked.hardware.modes.first().map_or(0, |mode| mode.width),
            height: parked.hardware.modes.first().map_or(0, |mode| mode.height),
            scale: 1.0,
            powered: false,
            vrr_supported: parked.hardware.vrr_supported,
            vrr_enabled: false,
            focused: false,
            active_workspace: 0,
            // A parked output shows nothing, a special included.
            special_workspace: None,
            make: parked.hardware.make.clone(),
            model: parked.hardware.model.clone(),
            serial: parked.hardware.serial.clone(),
            refresh_millihertz: parked.hardware.refresh_millihertz,
            transform: parked.hardware.transform,
            modes: parked.hardware.modes.clone(),
        })
        .collect();
    Snapshot {
        monitors,
        disabled_monitors,
        workspaces,
        specials,
        windows,
        focused: focused.map(wm_core::ClientId::as_u64),
        locked,
        cursor_position: wm
            .backend()
            .pointer_position()
            .map(|point| {
                let point = OutputCoordinates::for_output(wm.backend(), wm.monitor_index_at(point)).logical_position(point);
                (point.x, point.y)
            }),
        bindings,
        config_errors: session.config_diagnostics.clone(),
        devices,
        system_info: if let Some(shortcut_inhibit) = system_info {
            // The inhibitor line is the first clue for "my shortcuts
            // stopped working": which client holds them, or that the
            // user suspended a grant, or that grants are off. The
            // autoreload line is the same clue for "my edits stopped
            // landing": the pause is session-local and written
            // nowhere else.
            format!(
                "ChonkStep {}\nsource: {}\nconfig: {}\nautoreload: {}\nworkspace: {}\noutputs: {}\nshortcut_inhibitor: {}\n{}",
                env!("CARGO_PKG_VERSION"),
                chonk_build_info::SOURCE_ID,
                wm_config::config_path()
                    .map_or_else(|| "defaults (HOME unavailable)".to_string(), |path| path.display().to_string()),
                if autoreload_paused { "paused (misc:disable_autoreload; edits wait for an explicit reload)" } else { "on" },
                wm.current_workspace() + 1,
                monitors_info.len(),
                shortcut_inhibit,
                wm.backend().system_snapshot(),
            )
        } else {
            // Event clients never receive this request-only field.
            // Leaving it empty keeps /proc and protocol-object walks
            // off ordinary state publication.
            String::new()
        },
        separate_spaces: wm.separate_spaces(),
        previous_workspace: wm.previous_workspace(),
        autoreload_paused,
    }
}

/// One binding as `hyprctl binds` reports it.
///
/// Omarchy's SUPER+K menu is a launcher as well as a cheat sheet: picking
/// a row hands its dispatcher and argument straight back to `dispatch`.
/// So every action is reported as a verb `dispatch::parse` lowers back to
/// the same action, `exec` rows as shell source that rebuilds the argv,
/// and an action with no Hyprland verb as `chonkstep <name>`, which
/// replays the binding itself. Never a Rust `Debug` rendering.
fn ipc_binding(binding: &wm_config::Binding, session: &chonk_shell::startup::SessionState) -> Binding {
    use wm_config::Action as A;
    let letter = |direction: &wm_core::FocusDirection| {
        match direction {
            wm_core::FocusDirection::Left => "l",
            wm_core::FocusDirection::Right => "r",
            wm_core::FocusDirection::Up => "u",
            wm_core::FocusDirection::Down => "d",
        }
        .to_string()
    };
    let verb = |dispatcher: &str, argument: &str| (dispatcher.to_string(), argument.to_string());
    let (dispatcher, argument) = match &binding.action {
        A::Run(name) => ("exec".to_string(), session.commands.get(name).map(|argv| shell_join(argv)).unwrap_or_default()),
        A::SpawnTerminal => ("exec".to_string(), session.terminal.as_deref().map(shell_join).unwrap_or_else(|| "foot".to_string())),
        A::Close => verb("killactive", ""),
        A::ToggleFullscreen => verb("fullscreen", "0"),
        A::ToggleMaximize => verb("fullscreen", "1"),
        A::FullscreenState { internal, client } => {
            ("fullscreenstate".to_string(), format!("{} {}", internal.level(), client.level()))
        }
        A::Focus(direction) => ("movefocus".to_string(), letter(direction)),
        A::Move(direction) => ("movewindow".to_string(), letter(direction)),
        A::Floating(None) => verb("togglefloating", ""),
        A::Floating(Some(true)) => verb("setfloating", ""),
        A::Floating(Some(false)) => verb("settiled", ""),
        A::ToggleLayout => verb("togglelayout", ""),
        A::Layout(mode) => verb("layout", mode.compatible_name()),
        A::LayoutNoop => verb("layoutmsg", ""),
        A::WorkspaceNext => verb("workspace", "+1"),
        A::WorkspacePrev => verb("workspace", "-1"),
        A::WorkspaceNextOccupied => verb("workspace", "e+1"),
        A::WorkspacePrevOccupied => verb("workspace", "e-1"),
        A::WorkspacePrevious => verb("workspace", "previous"),
        A::FocusMonitor(target) => ("focusmonitor".to_string(), output_target_argument(target)),
        A::MoveWorkspaceToMonitor(target) => ("movecurrentworkspacetomonitor".to_string(), output_target_argument(target)),
        A::WorkspaceCarryNext => verb("movetoworkspace", "+1"),
        A::WorkspaceCarryPrev => verb("movetoworkspace", "-1"),
        A::Workspace(index) => ("workspace".to_string(), (index + 1).to_string()),
        A::WorkspaceSend(index) => ("movetoworkspacesilent".to_string(), (index + 1).to_string()),
        A::WorkspaceCarry(index) => ("movetoworkspace".to_string(), (index + 1).to_string()),
        A::ToggleSpecial(name) => ("togglespecialworkspace".to_string(), name.clone()),
        A::SendToSpecial { name, follow: false } => ("movetoworkspacesilent".to_string(), format!("special:{name}")),
        A::SendToSpecial { name, follow: true } => ("movetoworkspace".to_string(), format!("special:{name}")),
        other => ("chonkstep".to_string(), chonkstep_label(other)),
    };
    Binding {
        modifiers: hypr_modmask(binding.combo.modifiers), key: keysym_name(binding.combo.keysym),
        description: binding.description.clone().unwrap_or_default(), dispatcher, argument,
        locked: binding.locked, repeating: binding.repeating, release: binding.release,
    }
}

/// A monitor verb's argument as Hyprland spells it, which `dispatch`
/// reads back to the same target.
fn output_target_argument(target: &wm_core::OutputTarget) -> String {
    match target {
        wm_core::OutputTarget::Relative(step) => format!("{step:+}"),
        wm_core::OutputTarget::Direction(wm_core::FocusDirection::Left) => "l".into(),
        wm_core::OutputTarget::Direction(wm_core::FocusDirection::Right) => "r".into(),
        wm_core::OutputTarget::Direction(wm_core::FocusDirection::Up) => "u".into(),
        wm_core::OutputTarget::Direction(wm_core::FocusDirection::Down) => "d".into(),
        wm_core::OutputTarget::Name(name) => name.clone(),
    }
}

/// The window manager's spelling of a monitor selector the socket read.
fn output_target(target: MonitorTarget) -> wm_core::OutputTarget {
    use chonk_hyprland_ipc::dispatch::Direction;
    match target {
        MonitorTarget::Relative(step) => wm_core::OutputTarget::Relative(step),
        MonitorTarget::Direction(Direction::Left) => wm_core::OutputTarget::Direction(wm_core::FocusDirection::Left),
        MonitorTarget::Direction(Direction::Right) => wm_core::OutputTarget::Direction(wm_core::FocusDirection::Right),
        MonitorTarget::Direction(Direction::Up) => wm_core::OutputTarget::Direction(wm_core::FocusDirection::Up),
        MonitorTarget::Direction(Direction::Down) => wm_core::OutputTarget::Direction(wm_core::FocusDirection::Down),
        MonitorTarget::Name(name) => wm_core::OutputTarget::Name(name),
    }
}

/// The argument `chonkstep` reports for a binding with no Hyprland verb:
/// the action's `[keybindings]` name where it has one, and a stable
/// spelling for the few values no name produces. `dispatch` replays it by
/// finding the binding that reports the same label.
fn chonkstep_label(action: &wm_config::Action) -> String {
    action.config_name().unwrap_or_else(|| match action {
        wm_config::Action::Resize(delta) => format!("resize {} {}", delta.x, delta.y),
        wm_config::Action::CycleApplications(step) => format!("application-cycle {step}"),
        wm_config::Action::CycleAppWindows(step) => format!("application-window-cycle {step}"),
        _ => "unnamed".to_string(),
    })
}

/// An argv as POSIX shell source that rebuilds the same argv. The menu
/// runs a reported `exec` row through `exec_cmd`, which is shell source,
/// so `bash -lc "a || b"` joined with plain spaces would run `bash -lc a`.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&byte));
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        Action::ToggleSpecialWorkspace(name) => wm.toggle_special(&name),
        Action::MoveToSpecial { window, name, follow } => {
            let client = match window {
                Some(id) => client_of(wm, id),
                None => wm.focused_client(),
            };
            client.is_some_and(|client| wm.move_client_to_special(client, &name, follow))
        }
        Action::ToggleMaximize => {
            if let Some(id) = wm.focused_client() {
                wm.toggle_maximize(id, wm_core::MaximizeDirections::FULL);
            }
            true
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
        Action::FullscreenState { window, internal, client } => {
            let target = match window {
                Some(id) => client_of(wm, id),
                None => wm.focused_client(),
            };
            // Parsing already refused anything outside 0..=2; a value
            // that still fails here is answered as not applied rather
            // than rounded.
            match (target, wm_core::FullscreenMode::from_level(internal), wm_core::FullscreenMode::from_level(client)) {
                (Some(target), Some(internal), Some(client)) => {
                    wm.toggle_fullscreen_state(target, internal, client);
                    true
                }
                _ => false,
            }
        }
        Action::CycleFocus { forward } => wm.focus_adjacent_client(forward),
        Action::FocusDirection(direction) => {
            wm.focus_direction(match direction {
                chonk_hyprland_ipc::dispatch::Direction::Left => wm_core::FocusDirection::Left,
                chonk_hyprland_ipc::dispatch::Direction::Right => wm_core::FocusDirection::Right,
                chonk_hyprland_ipc::dispatch::Direction::Up => wm_core::FocusDirection::Up,
                chonk_hyprland_ipc::dispatch::Direction::Down => wm_core::FocusDirection::Down,
            });
            true
        }
        Action::MoveWindow {
            window,
            x,
            y,
            relative,
        } => match client_of(wm, window) {
            Some(id) => {
                let Some(client) = wm.client(id) else { return false };
                let mut geometry = client.geometry;
                let source = OutputCoordinates::for_output(wm.backend(), wm.client_output_index(id));
                let position = if relative {
                    let current = source.logical_position(geometry.pos);
                    Point::new(current.x.saturating_add(x), current.y.saturating_add(y))
                } else { Point::new(x, y) };
                let target = OutputCoordinates::for_output(wm.backend(), logical_monitor_index(wm.backend(), position));
                geometry.pos = if relative && source.origin == target.origin && source.scale == target.scale {
                    // Preserve fractional logical offsets on a relative move;
                    // moving by zero must not round an odd physical pixel away.
                    Point::new(
                        (geometry.pos.x as i64 + source.physical_length(x as i64)).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                        (geometry.pos.y as i64 + source.physical_length(y as i64)).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                    )
                } else { target.physical_position(position) };
                if target.scale != source.scale {
                    geometry.size = Size::new(
                        (geometry.size.w as f64 / source.scale * target.scale).round() as u32,
                        (geometry.size.h as f64 / source.scale * target.scale).round() as u32,
                    );
                }
                wm.set_client_content_geometry(id, geometry);
                true
            }
            None => false,
        },
        Action::ResizeWindow { window, width, height, relative } => match client_of(wm, window) {
            Some(id) => {
                let Some(client) = wm.client(id) else { return false };
                let coordinates = OutputCoordinates::for_output(wm.backend(), wm.client_output_index(id));
                let width = coordinates.physical_length(i64::from(width));
                let height = coordinates.physical_length(i64::from(height));
                let width = if relative { i64::from(client.geometry.size.w) + width } else { width };
                let height = if relative { i64::from(client.geometry.size.h) + height } else { height };
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
        Action::SetOpaque { window, opaque } => client_of(wm, window).is_some_and(|id| wm.set_client_opaque(id, opaque)),
        Action::SetFloating { window, floating } => {
            if let Some(id) = client_of(wm, window) {
                if let Some(value) = floating {
                    wm.set_floating(id, value);
                } else {
                    wm.toggle_floating(id);
                }
                true
            } else {
                false
            }
        }
        Action::SetWorkspaceLayout { workspace, mode } => {
            if let Some(mode) = wm_core::LayoutMode::parse(&mode) {
                wm.set_workspace_layout(workspace, mode);
                true
            } else {
                false
            }
        }
        Action::ToggleLayout => {
            wm.toggle_workspace_layout();
            true
        }
        Action::LayoutNoop => true,
        Action::Binding(label) => {
            // The label was validated against the snapshot the reply was
            // formed from; resolve it against the session again, with the
            // same lock rule, so a reload in between cannot run anything
            // the report did not name.
            let locked = comp.wm.backend().locked;
            let session = comp.shell.session_state();
            let action = session
                .bindings
                .iter()
                .find(|binding| {
                    (binding.locked || !locked) && {
                        let reported = ipc_binding(binding, session);
                        reported.dispatcher == "chonkstep" && reported.argument == label
                    }
                })
                .map(|binding| binding.action.clone());
            let Some(action) = action else {
                return false;
            };
            if let wm_config::Action::GlobalShortcut(target) = &action {
                let now = comp.start_time.elapsed();
                comp.global_shortcuts.trigger(target, true, now);
                comp.global_shortcuts.trigger(target, false, now);
                return true;
            }
            let outcome = comp.shell.run_action(&mut comp.wm, &action);
            comp.note_outcome(outcome);
            true
        }
        Action::MoveDirection(direction) => {
            let direction = match direction {
                chonk_hyprland_ipc::dispatch::Direction::Left => wm_core::FocusDirection::Left,
                chonk_hyprland_ipc::dispatch::Direction::Right => wm_core::FocusDirection::Right,
                chonk_hyprland_ipc::dispatch::Direction::Up => wm_core::FocusDirection::Up,
                chonk_hyprland_ipc::dispatch::Direction::Down => wm_core::FocusDirection::Down,
            };
            if let Some(id) = wm.focused_client() {
                wm.move_layout_window(id, direction);
            }
            true
        }
        Action::SetMonitorScale { output, scale_120 } => {
            comp.set_output_scale(&output, scale_120 as f64 / 120.0)
        }
        Action::ConfigureMonitor { output, scale_120, mode, position } => {
            let scale = scale_120.map(|scale| f64::from(scale) / 120.0);
            match crate::output_mgmt::configure_output(comp, &output, mode.as_deref(), position.as_deref(), scale) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%output, %error, "hl.monitor refused before changing the output");
                    false
                }
            }
        }
        Action::SetDpms { output, powered } => {
            crate::output_power::set_from_ipc(comp, output.as_deref(), powered)
        }
        // The Display panel's row toggle and Omarchy's clamshell and
        // laptop-display toggles. Disabling the last output in the
        // layout is refused by `park_output`, with a log line; an output
        // already in the asked-for state is `ok`, as it is under Hyprland.
        Action::SetMonitorEnabled { output, enabled } => {
            let index = comp.outputs.iter().position(|entry| entry.output.name() == output);
            let parked = comp.parked_outputs.iter().any(|parked| parked.setup.output.name() == output);
            let result = match (enabled, index, parked) {
                (false, Some(index), _) => crate::state::park_output(comp, index),
                (false, None, true) | (true, Some(_), _) => Ok(()),
                (true, None, true) => crate::state::unpark_output(comp, &output),
                (_, None, false) => Err(format!("no output named {output}")),
            };
            match result {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%output, enabled, %error, "monitor enable request refused");
                    false
                }
            }
        }
        Action::SwitchKeyboardLayout { device, target } => {
            switch_keyboard_layout(comp, &device, target)
        }
        Action::SetInputDeviceEnabled { name, enabled } => crate::input::devices::set_enabled(comp, &name, enabled),
        Action::SetCursorHidden(hidden) => {
            let owner = hidden.then(|| {
                comp.seat
                    .get_keyboard()
                    .and_then(|keyboard| keyboard.current_focus())
                    .map(|focus| focus.surface().clone())
                    .or_else(|| comp.seat.get_pointer().and_then(|pointer| pointer.current_focus())
                        .map(|target| target.surface().clone()))
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
        Action::WarpPointer { x, y } => {
            // Logical layout units in, ledger pixels out, through the scale
            // of the output that owns the point, so a mixed-DPI desk lands
            // on the requested spot.
            let logical = Point::new(x, y);
            let index = logical_monitor_index(comp.wm.backend(), logical);
            let physical = OutputCoordinates::for_output(comp.wm.backend(), index).physical_position(logical);
            crate::input::warp_pointer(comp, physical)
        }
        Action::FocusMonitor(target) => {
            // Parsing refused this against the snapshot's lock; the lock
            // can have landed since, and a script must never move focus
            // or the pointer behind the lock surface.
            if comp.wm.backend().locked {
                return false;
            }
            let Some(index) = comp.wm.resolve_output_target(&output_target(target)) else {
                return false;
            };
            let focused = comp.wm.focus_output(index);
            // The warp the window manager asked for, applied now rather
            // than at the next drain, so a script that reads `cursorpos`
            // straight after sees the pointer where it asked for it.
            crate::input::flush_pointer_warp(comp);
            focused
        }
        Action::MoveWorkspaceToMonitor(target) => {
            if comp.wm.backend().locked {
                return false;
            }
            let Some(index) = comp.wm.resolve_output_target(&output_target(target)) else {
                return false;
            };
            match comp.wm.move_workspace_to_output(index) {
                Ok(()) => true,
                Err(why) => {
                    tracing::warn!(why, "movecurrentworkspacetomonitor refused at apply");
                    false
                }
            }
        }
        Action::ReloadConfig => {
            comp.shell.reload_config(&mut comp.wm);
            true
        }
        Action::SetAutoreload { paused } => {
            comp.shell.set_autoreload_paused(paused);
            true
        }
        // Already the case, and applied as nothing: see the variant.
        Action::SuppressConfigErrors => true,
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
        // `dispatch exec` is how Omarchy's launch-or-focus scripts start
        // an application that is not yet running, so the launch carries
        // an activation token the same way an `exec` bind's does: the
        // token is what lets a single-instance application raise the
        // window it already has when the script's guess was wrong.
        Action::ExecShell(command) => {
            let env = chonk_shell::spawn::activation_env(wm.backend_mut().create_activation_token());
            chonk_shell::spawn::spawn_detached_with_env("sh", &["-c", &command], &env, &[]).is_some()
        }
        Action::ExecArgv(argv) => {
            let Some((program, args)) = argv.split_first() else {
                return false;
            };
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let env = chonk_shell::spawn::activation_env(wm.backend_mut().create_activation_token());
            chonk_shell::spawn::spawn_detached_with_env(program, &args, &env, &[]).is_some()
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

pub(crate) fn refresh_keyboard_layout(comp: &mut Compositor) {
    let Some(keyboard) = comp.seat.get_keyboard() else {
        return;
    };
    let (layouts, active) = keyboard.with_xkb_state(comp, |context| {
        let xkb = context.xkb().lock().expect("keyboard XKB mutex poisoned");
        let active = xkb.active_layout().0;
        let layouts = xkb.layouts().map(|layout| xkb.layout_name(layout).to_string()).collect();
        (layouts, active)
    });
    let backend = comp.wm.backend_mut();
    backend.keyboard_layouts = layouts;
    backend.active_keyboard_layout = active;
}

fn switch_keyboard_layout(comp: &mut Compositor, device: &str, target: LayoutTarget) -> bool {
    if device != "all"
        && !comp
            .wm
            .backend()
            .input_devices
            .iter()
            .any(|input| input.keyboard && input.name == device)
        && device != chonk_hyprland_ipc::state::NESTED_KEYBOARD
    {
        return false;
    }
    let Some(keyboard) = comp.seat.get_keyboard() else {
        return false;
    };
    let changed = keyboard.with_xkb_state(comp, |mut context| {
        let (current, count) = {
            let xkb = context.xkb().lock().expect("keyboard XKB mutex poisoned");
            (xkb.active_layout().0, xkb.layouts().count() as u32)
        };
        if count == 0 {
            return false;
        }
        let next = match target {
            LayoutTarget::Next => (current + 1) % count,
            LayoutTarget::Previous => (count + current - 1) % count,
            LayoutTarget::Index(index) if index < count => index,
            LayoutTarget::Index(_) => return false,
        };
        context.set_layout(Layout(next));
        true
    });
    if changed {
        refresh_keyboard_layout(comp);
        comp.mark_hyprland_state_dirty();
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{keysym_name, monitor_mode_size, OutputCoordinates};
    use wm_theme_api::{Point, Size};

    #[test]
    fn coordinate_conversion_preserves_output_origins_and_floors_edge_pixels() {
        let left = OutputCoordinates { origin: Point::new(-640, -120), scale: 2.0 };
        assert_eq!(left.logical_position(Point::new(-639, -119)), Point::new(-640, -120));
        assert_eq!(left.logical_position(Point::new(-1, 679)), Point::new(-321, 279));
        assert_eq!(left.physical_position(Point::new(-600, -60)), Point::new(-560, 0));
        let right = OutputCoordinates { origin: Point::new(1280, 100), scale: 1.5 };
        assert_eq!(right.logical_position(Point::new(1283, 103)), Point::new(1282, 102));
        assert_eq!(right.physical_position(Point::new(1282, 102)), Point::new(1283, 103));
        assert_eq!(right.logical_size(Size::new(450, 300)), Size::new(300, 200));
        assert_eq!(right.physical_length(-20), -30);
        assert_eq!(monitor_mode_size(Size::new(1080, 1920), 1), Size::new(1920, 1080));
        assert_eq!(monitor_mode_size(Size::new(1080, 1920), 3), Size::new(1920, 1080));
        assert_eq!(monitor_mode_size(Size::new(1920, 1080), 2), Size::new(1920, 1080));
    }

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

#[cfg(test)]
mod binding_replay_tests {
    use super::{ipc_binding, shell_join};
    use chonk_hyprland_ipc::dispatch::{self, Action, Direction};
    use chonk_hyprland_ipc::state::{Monitor, MonitorMode, Snapshot, Window, Workspace};
    use chonk_hyprland_ipc::Outcome;

    const FOCUSED: u64 = 7;

    fn desk(bindings: Vec<chonk_hyprland_ipc::state::Binding>) -> Snapshot {
        Snapshot {
            monitors: vec![Monitor {
                id: 0,
                name: "eDP-1".into(),
                description: "a panel".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1600,
                scale: 2.0,
                powered: true,
                vrr_supported: false,
                vrr_enabled: false,
                focused: true,
                active_workspace: 1,
                special_workspace: None,
                make: String::new(),
                model: String::new(),
                serial: String::new(),
                refresh_millihertz: 60_000,
                transform: 0,
                modes: vec![MonitorMode { width: 2560, height: 1600, refresh_millihertz: 60_000 }],
            }],
            workspaces: (0..10)
                .map(|index| Workspace {
                    layout: "freeform".into(),
                    index,
                    monitor: "eDP-1".into(),
                    monitor_id: 0,
                    windows: u32::from(index == 1),
                    has_fullscreen: false,
                })
                .collect(),
            windows: vec![Window {
                floating: true,
                id: FOCUSED,
                title: "~ — foot".into(),
                class: "foot".into(),
                x: 10,
                y: 20,
                width: 800,
                height: 600,
                workspace: 1,
                special: None,
                monitor: 0,
                pid: 4242,
                xwayland: false,
                fullscreen: false,
                maximized: false,
                client_fullscreen: false,
                hidden: false,
                urgent: false,
                pinned: false,
                inhibiting_idle: false,
                tags: Vec::new(),
                xdg_tag: String::new(),
                xdg_description: String::new(),
                focus_history_id: 0,
            }],
            focused: Some(FOCUSED),
            bindings,
            // Omarchy binds `workspace previous` and the workspace-to-
            // monitor moves; both need a desk where they mean something.
            separate_spaces: true,
            previous_workspace: Some(0),
            ..Snapshot::default()
        }
    }

    /// A Lua string literal, as the menu JSON-encodes a command for
    /// `hl.dsp.exec_cmd`.
    fn lua_string(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }

    fn sessions() -> Vec<(&'static str, chonk_shell::startup::SessionState)> {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../wm-config/tests/fixtures/hyprland/machine");
        let omarchy = wm_config::parse_with("desktop = \"omarchy\"", &|| {
            Some(wm_config::hyprland::read(&wm_config::hyprland::Roots::under(&fixture)))
        })
        .expect("the captured Omarchy machine parses");
        let built_in = wm_config::Config::default_config();
        vec![
            ("the captured Omarchy machine", chonk_shell::startup::SessionState::resolve(&omarchy)),
            ("the built-in keymap", chonk_shell::startup::SessionState::resolve(&built_in)),
        ]
    }

    /// Omarchy's SUPER+K menu hands every `binds` row back to `dispatch`:
    /// `exec` rows through `hl.dsp.exec_cmd(<string>)`, the rest as
    /// `<dispatcher> <arg>`. Every row must replay, none may be a `Debug`
    /// rendering, and the families with a Hyprland verb must lower back to
    /// the bound action.
    #[test]
    fn every_reported_binding_replays_through_dispatch() {
        for (source, session) in sessions() {
            assert!(!session.bindings.is_empty(), "{source} has bindings");
            let reported: Vec<_> = session.bindings.iter().map(|binding| ipc_binding(binding, &session)).collect();
            let snapshot = desk(reported.clone());
            for (binding, row) in session.bindings.iter().zip(&reported) {
                assert!(
                    row.dispatcher != "chonkstep" || !row.argument.contains(['(', ')', '{', '"']),
                    "{source}: {row:?} is a Debug rendering"
                );
                let wire = if row.dispatcher == "exec" {
                    format!("hl.dsp.exec_cmd({})", lua_string(&row.argument))
                } else {
                    format!("{} {}", row.dispatcher, row.argument)
                };
                let outcome = dispatch::parse(&wire, &snapshot);
                let Outcome::Run(lowered) = outcome else {
                    panic!("{source}: {wire:?} for {:?} was refused: {outcome:?}", binding.action);
                };
                let expected = match &binding.action {
                    wm_config::Action::ToggleLayout => Some(Action::ToggleLayout),
                    wm_config::Action::ToggleMaximize => Some(Action::ToggleMaximize),
                    wm_config::Action::Floating(floating) => Some(Action::SetFloating { window: FOCUSED, floating: *floating }),
                    wm_config::Action::Move(wm_core::FocusDirection::Left) => Some(Action::MoveDirection(Direction::Left)),
                    wm_config::Action::WorkspaceNext => Some(Action::FocusWorkspace(2)),
                    wm_config::Action::WorkspacePrev => Some(Action::FocusWorkspace(0)),
                    // Only workspace 1 (index 1) has a window: `e+1` and
                    // `e-1` both stay on it rather than growing the row.
                    wm_config::Action::WorkspaceNextOccupied | wm_config::Action::WorkspacePrevOccupied => {
                        Some(Action::FocusWorkspace(1))
                    }
                    wm_config::Action::WorkspacePrevious => Some(Action::FocusWorkspace(0)),
                    wm_config::Action::FocusMonitor(wm_core::OutputTarget::Relative(step)) => {
                        Some(Action::FocusMonitor(chonk_hyprland_ipc::dispatch::MonitorTarget::Relative(*step)))
                    }
                    wm_config::Action::MoveWorkspaceToMonitor(wm_core::OutputTarget::Direction(wm_core::FocusDirection::Left)) => {
                        Some(Action::MoveWorkspaceToMonitor(chonk_hyprland_ipc::dispatch::MonitorTarget::Direction(Direction::Left)))
                    }
                    wm_config::Action::Workspace(index) => Some(Action::FocusWorkspace(*index)),
                    wm_config::Action::ToggleSpecial(name) => Some(Action::ToggleSpecialWorkspace(name.clone())),
                    wm_config::Action::SendToSpecial { name, follow } => {
                        Some(Action::MoveToSpecial { window: None, name: name.clone(), follow: *follow })
                    }
                    wm_config::Action::Run(_) | wm_config::Action::SpawnTerminal => Some(Action::ExecShell(row.argument.clone())),
                    _ if row.dispatcher == "chonkstep" => Some(Action::Binding(row.argument.clone())),
                    _ => None,
                };
                if let Some(expected) = expected {
                    assert_eq!(lowered, expected, "{source}: {wire:?} for {:?}", binding.action);
                }
            }
        }
    }

    #[test]
    fn shell_join_quotes_only_what_the_shell_would_split_or_expand() {
        let argv = |words: &[&str]| words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        assert_eq!(shell_join(&argv(&["omarchy-launch-browser"])), "omarchy-launch-browser");
        assert_eq!(
            shell_join(&argv(&["bash", "-lc", "pkill hyprpicker || hyprpicker -a"])),
            "bash -lc 'pkill hyprpicker || hyprpicker -a'"
        );
        assert_eq!(shell_join(&argv(&["echo", "it's", "", "$HOME"])), "echo 'it'\\''s' '' '$HOME'");
    }
}
