//! `ext_workspace_v1`: the workspace row, told to native Wayland
//! clients.
//!
//! Until this existed a Wayland client could not see or change the
//! workspace at all. The compositor published the row to X11 through
//! EWMH's `_NET_CURRENT_DESKTOP` and to Omarchy's bar through the
//! Hyprland IPC, and a native panel, pager or switcher — anything not
//! written against either of those — got nothing. The information was
//! already there; only the protocol carrying it was missing.
//!
//! # Shape
//!
//! One group covering every output, and one handle per workspace.
//!
//! That is not a simplification, it is what this desktop *is*: there is
//! a single global current workspace (`WindowManager::current_workspace`)
//! and every monitor shows it, which is the same fact the Hyprland IPC
//! reports by marking exactly one monitor focused and giving them all
//! the same active workspace. A group per output would advertise
//! per-output workspaces that no key could ever change independently.
//!
//! # Publication
//!
//! Demand-driven, like the foreign-toplevel list beside it: [`refresh`]
//! returns immediately unless the row actually moved or a manager bound
//! since the last pass. The protocol is transactional — every batch of
//! events ends with `done` — so a pass that changes nothing sends
//! nothing rather than an empty transaction.

use smithay::reexports::wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, State as WorkspaceStateFlags},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use wm_core::BackendEvent;

use crate::state::{Compositor, WlFrameId, WlWindowId};

type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// Highest `ext_workspace_manager_v1` implemented. Version 1 is the
/// whole protocol as it stands.
const WORKSPACE_VERSION: u32 = 1;

/// One bound `ext_workspace_manager_v1`, and everything minted for it.
///
/// Handles are per-manager: the protocol has the compositor create a
/// group and a workspace object *for each manager instance*, so two
/// panels each get their own objects naming the same workspaces.
struct ManagerInstance {
    resource: ExtWorkspaceManagerV1,
    group: Option<ExtWorkspaceGroupHandleV1>,
    /// Index-aligned with the workspace row: entry `i` is workspace `i`.
    workspaces: Vec<ExtWorkspaceHandleV1>,
}

pub(crate) struct WorkspaceState {
    managers: Vec<ManagerInstance>,
    /// The row as last published: how many workspaces there were and
    /// which one was active. `None` until the first publish.
    published: Option<(usize, usize)>,
    /// Set when the row moves. The compositor's workspace state is
    /// re-derived on paths that run every pass, so "dirty means
    /// changed, not mentioned" — the same discipline the EWMH ledger
    /// and the stacking sync keep.
    dirty: bool,
}

pub(crate) fn init(display: &DisplayHandle) -> WorkspaceState {
    display.create_global::<Compositor, ExtWorkspaceManagerV1, ()>(WORKSPACE_VERSION, ());
    tracing::info!(version = WORKSPACE_VERSION, "ext-workspace advertised");
    WorkspaceState { managers: Vec::new(), published: None, dirty: true }
}

impl WorkspaceState {
    /// Marks the row as needing a publish. Called from the one place
    /// the compositor learns the workspace state moved.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// The workspace name a client sees. Matches what the Hyprland IPC and
/// EWMH both publish: this desktop's workspaces are numbered from 1 for
/// a human, and 0-based only internally.
fn workspace_name(index: usize) -> String {
    (index + 1).to_string()
}

/// A stable identifier for a workspace, in the protocol's sense: it must
/// survive a client reconnecting and name the same workspace. The index
/// is exactly that here — workspaces are a fixed row, not a set that can
/// be reordered.
fn workspace_id(index: usize) -> String {
    format!("chonkstep-workspace-{index}")
}

/// Publishes the row to every bound manager, if anything moved.
pub(crate) fn refresh(comp: &mut Compositor) {
    let count = comp.wm.workspace_count().max(1);
    let current = comp.wm.current_workspace();
    let row = (count, current);
    let bound_since_last = comp.workspaces.managers.iter().any(|manager| manager.group.is_none());
    if !comp.workspaces.dirty && !bound_since_last && comp.workspaces.published == Some(row) {
        return;
    }
    comp.workspaces.dirty = false;
    comp.workspaces.published = Some(row);

    // Dead managers first: a client that disconnected leaves resources
    // that answer `is_alive` false, and sending into them is wasted
    // work on every later pass.
    comp.workspaces.managers.retain(|manager| manager.resource.is_alive());

    let display_handle = comp.display_handle.clone();
    let mut state = std::mem::take(&mut comp.workspaces.managers);
    for manager in &mut state {
        publish_to(&display_handle, manager, count, current);
    }
    comp.workspaces.managers = state;
}

fn publish_to(display_handle: &DisplayHandle, manager: &mut ManagerInstance, count: usize, current: usize) {
    if manager.group.is_none() {
        let Some(client) = manager.resource.client() else { return };
        let Ok(group) = client.create_resource::<ExtWorkspaceGroupHandleV1, (), Compositor>(
            display_handle,
            manager.resource.version(),
            (),
        ) else {
            return;
        };
        manager.resource.workspace_group(&group);
        // One group over every output, because this desktop has one
        // global current workspace and every monitor shows it. The
        // group is told about no outputs on purpose: `output_enter`
        // per monitor would suggest the row is per-output, and it is
        // not.
        group.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::empty());
        manager.group = Some(group);
    }
    // Grow the workspace row to match. A row only ever grows on this
    // desktop (`MAX_WORKSPACES` is a ceiling, and nothing destroys a
    // workspace once created), so this is an append rather than a diff.
    while manager.workspaces.len() < count {
        let index = manager.workspaces.len();
        let Some(handle) = mint_workspace(display_handle, manager, index) else { return };
        if let Some(group) = manager.group.as_ref() {
            group.workspace_enter(&handle);
        }
        manager.workspaces.push(handle);
    }
    for (index, handle) in manager.workspaces.iter().enumerate() {
        let flags = if index == current { WorkspaceStateFlags::Active } else { WorkspaceStateFlags::empty() };
        handle.state(flags);
    }
    manager.resource.done();
}

fn mint_workspace(
    display_handle: &DisplayHandle,
    manager: &ManagerInstance,
    index: usize,
) -> Option<ExtWorkspaceHandleV1> {
    let client = manager.resource.client()?;
    let handle = client
        .create_resource::<ExtWorkspaceHandleV1, usize, Compositor>(
            display_handle,
            manager.resource.version(),
            index,
        )
        .ok()?;
    manager.resource.workspace(&handle);
    handle.id(workspace_id(index));
    handle.name(workspace_name(index));
    // Coordinates are a single axis on a flat row.
    handle.coordinates(
        u32::try_from(index).unwrap_or(u32::MAX).to_ne_bytes().to_vec(),
    );
    // The only thing a client may ask of a workspace here is to make it
    // the current one. This desktop creates workspaces on demand and
    // never destroys them, so `remove` and `assign` are not offered
    // rather than offered and refused.
    handle.capabilities(ext_workspace_handle_v1::WorkspaceCapabilities::Activate);
    Some(handle)
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Compositor {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let resource = data_init.init(resource, ());
        // Nothing is sent here. The bind callback runs mid-dispatch,
        // before the pass that would reconcile the row; the next
        // `refresh` catches this manager up, which is also the only
        // place objects are minted, so a workspace can never be
        // announced twice to one manager.
        state.workspaces.managers.push(ManagerInstance { resource, group: None, workspaces: Vec::new() });
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
            // The protocol batches requests and applies them on commit.
            // Every request this compositor accepts is applied when it
            // arrives (activation is a single verb with no partner), so
            // there is nothing pending for a commit to flush.
            ext_workspace_manager_v1::Request::Commit => {}
            ext_workspace_manager_v1::Request::Stop => {
                resource.finished();
                state.workspaces.managers.retain(|manager| manager.resource != *resource);
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: smithay::reexports::wayland_server::backend::ClientId, resource: &ExtWorkspaceManagerV1, _data: &()) {
        state.workspaces.managers.retain(|manager| manager.resource != *resource);
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        request: ext_workspace_group_handle_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        // `create_workspace` is not advertised in the group's
        // capabilities, so a compliant client never sends it; an
        // incompliant one is ignored rather than errored, because a row
        // this desktop grows on demand has nothing to refuse.
        let _ = request;
    }
}

/// The workspace index a handle names, carried as the resource's own
/// user data so a request resolves without a scan.
impl Dispatch<ExtWorkspaceHandleV1, usize> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        index: &usize,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // The one verb offered, and the same event an EWMH pager's
            // `_NET_CURRENT_DESKTOP` produces — so both protocols reach
            // `switch_workspace` by one path and cannot disagree.
            ext_workspace_handle_v1::Request::Activate => {
                state.wm.backend_mut().queue(WmEvent::DesktopSwitchRequested(*index));
            }
            // Deactivating *the* current workspace has no meaning on a
            // desktop where exactly one is always current.
            ext_workspace_handle_v1::Request::Deactivate => {}
            _ => {}
        }
    }
}
