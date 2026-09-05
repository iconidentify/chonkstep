//! An `ext_workspace_v1` client: reports the row it is told about, and
//! can ask for a workspace to be activated.
//!
//! The protocol this probe speaks is the one a native Wayland panel,
//! pager or switcher uses. Before it was served, such a client saw no
//! workspaces at all on this desktop — the information reached X11
//! through EWMH and Omarchy's bar through the Hyprland IPC, and nothing
//! else.

use std::io::Write;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::stdout().flush();
}

#[derive(Default)]
struct Probe {
    manager: Option<ExtWorkspaceManagerV1>,
    /// Every workspace handle, in announcement order, with the name and
    /// active bit last reported for it.
    workspaces: Vec<(ExtWorkspaceHandleV1, String, bool)>,
    groups: usize,
    /// Set once a `done` has been seen, so the report is only printed
    /// for a settled transaction.
    settled: bool,
}

impl Probe {
    fn report(&self) {
        let names: Vec<String> = self.workspaces.iter().map(|(_, name, _)| name.clone()).collect();
        let active: Vec<String> = self
            .workspaces
            .iter()
            .filter(|(_, _, active)| *active)
            .map(|(_, name, _)| name.clone())
            .collect();
        say(&format!(
            "**row groups={} count={} names={} active={}**",
            self.groups,
            self.workspaces.len(),
            names.join(","),
            active.join(",")
        ));
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == "ext_workspace_manager_v1" {
                probe.manager = Some(registry.bind(name, version.min(1), qh, ()));
            }
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { .. } => probe.groups += 1,
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                probe.workspaces.push((workspace, String::new(), false));
            }
            ext_workspace_manager_v1::Event::Done => {
                probe.settled = true;
                probe.report();
            }
            ext_workspace_manager_v1::Event::Finished => say("**finished**"),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Probe, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        resource: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(entry) = probe.workspaces.iter_mut().find(|(handle, _, _)| handle == resource) else {
            return;
        };
        match event {
            ext_workspace_handle_v1::Event::Name { name } => entry.1 = name,
            ext_workspace_handle_v1::Event::State { state } => {
                entry.2 = state
                    .into_result()
                    .map(|state| state.contains(ext_workspace_handle_v1::State::Active))
                    .unwrap_or(false);
            }
            _ => {}
        }
    }
}

fn main() {
    // `activate <n>` asks for the 1-based workspace `n`; with no
    // argument the probe only watches.
    let activate: Option<usize> = std::env::args().nth(1).and_then(|arg| arg.parse().ok());

    let connection = Connection::connect_to_env().expect("connect to compositor");
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).expect("registry roundtrip");
    if probe.manager.is_none() {
        say("**ext_workspace_manager_v1 missing**");
        return;
    }
    say("**workspace manager bound**");
    // A second roundtrip for the row itself, which the compositor
    // publishes on its next pass rather than from the bind.
    queue.roundtrip(&mut probe).expect("workspace roundtrip");

    if let Some(target) = activate {
        // 1-based on the wire, matching the names the compositor sends.
        if let Some((handle, name, _)) = probe.workspaces.iter().find(|(_, name, _)| name == &target.to_string()) {
            handle.activate();
            if let Some(manager) = &probe.manager {
                manager.commit();
            }
            say(&format!("**requested {name}**"));
            let _ = connection.flush();
        } else {
            say(&format!("**no workspace named {target}**"));
        }
    }

    loop {
        if queue.blocking_dispatch(&mut probe).is_err() {
            return;
        }
    }
}
