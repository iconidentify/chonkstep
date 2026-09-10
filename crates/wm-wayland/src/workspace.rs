//! Native workspace groups mirror the core's display policy. Stable Space IDs
//! survive row compaction and output reconnect; numeric coordinates remain local
//! to a group. Activation requests take effect only on the manager's commit.
use crate::state::{Compositor, WlFrameId, WlWindowId};
use smithay::reexports::wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, State as Flags},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use std::collections::HashMap;
type WmEvent = wm_core::BackendEvent<WlWindowId, WlFrameId>;
const WORKSPACE_VERSION: u32 = 1;

struct Group {
    handle: ExtWorkspaceGroupHandleV1,
    outputs: Vec<WlOutput>,
}
struct Workspace {
    handle: ExtWorkspaceHandleV1,
    group: String,
}
struct ManagerInstance {
    resource: ExtWorkspaceManagerV1,
    groups: HashMap<String, Group>,
    workspaces: HashMap<String, Workspace>,
    pending: Vec<ExtWorkspaceHandleV1>,
    initialized: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct Row {
    id: String,
    group: String,
    index: usize,
    active: bool,
}
pub(crate) struct WorkspaceState {
    managers: Vec<ManagerInstance>,
    published: Vec<Row>,
    revision: Option<u64>,
    dirty: bool,
}
pub(crate) fn init(display: &DisplayHandle) -> WorkspaceState {
    display.create_global::<Compositor, ExtWorkspaceManagerV1, ()>(WORKSPACE_VERSION, ());
    WorkspaceState {
        managers: Vec::new(),
        published: Vec::new(),
        revision: None,
        dirty: true,
    }
}
impl WorkspaceState {
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

pub(crate) fn refresh(comp: &mut Compositor) {
    let revision = comp.wm.protocol_state_revision();
    if !comp.workspaces.dirty
        && comp.workspaces.revision == Some(revision)
        && comp.workspaces.managers.iter().all(|m| m.initialized)
    {
        return;
    }
    comp.workspaces.revision = Some(revision);
    let forced = std::mem::take(&mut comp.workspaces.dirty);
    let mut rows = Vec::new();
    let mut outputs: HashMap<String, Vec<smithay::output::Output>> = HashMap::new();
    if comp.wm.separate_spaces() {
        for (index, output) in comp.outputs.iter().enumerate() {
            let group = output.output.name();
            outputs.insert(group.clone(), vec![output.output.clone()]);
            for (local, workspace) in comp.wm.workspace_row_on_output(index).into_iter().enumerate() {
                rows.push(Row {
                    id: comp.wm.workspace_id(workspace),
                    group: group.clone(),
                    index: local,
                    active: comp.wm.workspace_visible(workspace),
                });
            }
        }
    } else {
        outputs.insert(
            "desktop".into(),
            comp.outputs.iter().map(|o| o.output.clone()).collect(),
        );
        for index in 0..comp.wm.workspace_count() {
            rows.push(Row {
                id: comp.wm.workspace_id(index),
                group: "desktop".into(),
                index,
                active: comp.wm.workspace_visible(index),
            });
        }
    }
    let changed = comp.workspaces.published != rows;
    comp.workspaces.published = rows;
    comp.workspaces.managers.retain(|m| m.resource.is_alive());
    for manager in &mut comp.workspaces.managers {
        if forced || changed || !manager.initialized {
            publish_to(&comp.display_handle, manager, &comp.workspaces.published, &outputs);
        }
    }
}

fn publish_to(
    dh: &DisplayHandle,
    manager: &mut ManagerInstance,
    rows: &[Row],
    outputs: &HashMap<String, Vec<smithay::output::Output>>,
) {
    let Some(client) = manager.resource.client() else {
        return;
    };
    for (name, members) in outputs {
        if !manager.groups.contains_key(name) {
            let Ok(handle) =
                client.create_resource::<ExtWorkspaceGroupHandleV1, (), Compositor>(dh, manager.resource.version(), ())
            else {
                return;
            };
            manager.resource.workspace_group(&handle);
            handle.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::empty());
            manager.groups.insert(
                name.clone(),
                Group {
                    handle,
                    outputs: Vec::new(),
                },
            );
        }
        let group = manager.groups.get_mut(name).unwrap();
        let resources: Vec<_> = members.iter().flat_map(|o| o.client_outputs(&client)).collect();
        for output in group.outputs.iter().filter(|o| !resources.contains(o)) {
            if output.is_alive() { group.handle.output_leave(output); }
        }
        for output in resources.iter().filter(|o| !group.outputs.contains(o)) {
            group.handle.output_enter(output);
        }
        group.outputs = resources;
    }
    manager.workspaces.retain(|id, workspace| {
        if rows.iter().any(|r| &r.id == id) {
            return true;
        }
        if let Some(group) = manager.groups.get(&workspace.group) {
            group.handle.workspace_leave(&workspace.handle);
        }
        workspace.handle.removed();
        false
    });
    for row in rows {
        if !manager.workspaces.contains_key(&row.id) {
            let Ok(handle) = client.create_resource::<ExtWorkspaceHandleV1, String, Compositor>(
                dh,
                manager.resource.version(),
                row.id.clone(),
            ) else {
                return;
            };
            manager.resource.workspace(&handle);
            handle.id(row.id.clone());
            handle.capabilities(ext_workspace_handle_v1::WorkspaceCapabilities::Activate);
            manager.workspaces.insert(
                row.id.clone(),
                Workspace {
                    handle,
                    group: String::new(),
                },
            );
        }
        let workspace = manager.workspaces.get_mut(&row.id).unwrap();
        if workspace.group != row.group {
            if let Some(group) = manager.groups.get(&workspace.group) {
                group.handle.workspace_leave(&workspace.handle);
            }
            manager.groups[&row.group].handle.workspace_enter(&workspace.handle);
            workspace.group.clone_from(&row.group);
        }
        workspace.handle.name((row.index + 1).to_string());
        workspace.handle.coordinates((row.index as u32).to_ne_bytes().to_vec());
        workspace
            .handle
            .state(if row.active { Flags::Active } else { Flags::empty() });
    }
    manager.groups.retain(|name, group| {
        if outputs.contains_key(name) {
            return true;
        }
        for output in &group.outputs {
            if output.is_alive() { group.handle.output_leave(output); }
        }
        group.handle.removed();
        false
    });
    manager.initialized = true;
    manager.resource.done();
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Compositor {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let resource = init.init(resource, ());
        state.workspaces.managers.push(ManagerInstance {
            resource,
            groups: HashMap::new(),
            workspaces: HashMap::new(),
            pending: Vec::new(),
            initialized: false,
        });
    }
}
impl Dispatch<ExtWorkspaceManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                let Some(manager) = state.workspaces.managers.iter_mut().find(|m| &m.resource == resource) else {
                    return;
                };
                for handle in std::mem::take(&mut manager.pending) {
                    let Some(id) = handle.data::<String>() else { continue; };
                    if manager.workspaces.get(id).is_none_or(|w| w.handle != handle) { continue; }
                    if let Some(index) = (0..state.wm.workspace_count()).find(|&i| &state.wm.workspace_id(i) == id) {
                        state.wm.backend_mut().queue(WmEvent::DesktopSwitchRequested(index));
                    }
                }
            }
            ext_workspace_manager_v1::Request::Stop => {
                resource.finished();
                state.workspaces.managers.retain(|m| &m.resource != resource);
            }
            _ => {}
        }
    }
    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ExtWorkspaceManagerV1,
        _data: &(),
    ) {
        state.workspaces.managers.retain(|m| &m.resource != resource);
    }
}
impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        _request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}
impl Dispatch<ExtWorkspaceHandleV1, String> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        id: &String,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, ext_workspace_handle_v1::Request::Activate) {
            // Membership validates stale handles even when a legacy numeric slot
            // has been recreated. Retain a bounded transaction, coalescing repeats.
            if let Some(manager) = state
                .workspaces
                .managers
                .iter_mut()
                .find(|m| m.workspaces.get(id).is_some_and(|w| &w.handle == resource))
            {
                if !manager.pending.contains(resource) && manager.pending.len() < wm_core::MAX_WORKSPACES {
                    manager.pending.push(resource.clone());
                }
            }
        }
    }
}
