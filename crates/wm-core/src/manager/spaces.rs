//! Display-owned Spaces. Client/layout indices remain the backend-neutral
//! numeric projection; stable IDs and display keys survive compaction and
//! output reorder. Disconnected displays lend their Spaces to a live output
//! and reclaim them on reconnect, without discarding window membership.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Space {
    pub id: u64,
    pub home_display: String,
    pub output_display: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySpace {
    pub key: String,
    pub name: String,
    pub geometry: Rect,
    pub active: u64,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySpacesSnapshot {
    pub spaces: Vec<Space>,
    pub displays: Vec<DisplaySpace>,
    pub selected: String,
    pub next_id: u64,
}

impl DisplaySpacesSnapshot {
    pub fn valid(&self) -> bool {
        let key = |s: &str| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control);
        if self.spaces.is_empty()
            || self.spaces.len() > MAX_WORKSPACES
            || self.displays.is_empty()
            || self.displays.len() > MAX_WORKSPACES
            || self.next_id > u64::MAX - MAX_WORKSPACES as u64
        {
            return false;
        }
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        if !self.displays.iter().all(|d| {
            key(&d.key)
                && key(&d.name)
                && keys.insert(d.key.as_str())
                && d.geometry.size.w > 0
                && d.geometry.size.h > 0
                && d.geometry.size.w <= 32768
                && d.geometry.size.h <= 32768
                && d.geometry.pos.x.unsigned_abs() <= 1_000_000
                && d.geometry.pos.y.unsigned_abs() <= 1_000_000
        }) {
            return false;
        }
        keys.contains(self.selected.as_str())
            && self.spaces.iter().all(|s| {
                s.id > 0
                    && s.id <= self.next_id
                    && ids.insert(s.id)
                    && keys.contains(s.home_display.as_str())
                    && keys.contains(s.output_display.as_str())
            })
            && self.displays.iter().all(|d| d.active == 0 || ids.contains(&d.active))
    }
}

pub(super) struct DisplaySpaces {
    pub snapshot: DisplaySpacesSnapshot,
    pub reconciling: bool,
}

impl<B: Backend> WindowManager<B> {
    pub fn separate_spaces(&self) -> bool {
        self.mac_mode() && self.interaction.separate_spaces && self.display_spaces.is_some()
    }

    pub fn display_spaces_snapshot(&self) -> Option<&DisplaySpacesSnapshot> {
        self.separate_spaces()
            .then(|| &self.display_spaces.as_ref().unwrap().snapshot)
    }

    /// Restore only before clients map. Reject corrupt topology as a whole,
    /// keeping the working live model and bounded allocation/ID arithmetic.
    pub fn restore_display_spaces(&mut self, snapshot: DisplaySpacesSnapshot) -> bool {
        if !self.mac_mode() || !self.interaction.separate_spaces || !self.clients.is_empty() || !snapshot.valid() {
            return false;
        }
        self.workspace_count = snapshot.spaces.len();
        self.layouts
            .resize_with(self.workspace_count, crate::spatial::WorkspaceLayout::default);
        self.display_spaces = Some(DisplaySpaces {
            snapshot,
            reconciling: false,
        });
        self.reconcile_display_spaces();
        true
    }

    pub fn mac_fullscreen_origin(&self, id: ClientId) -> Option<usize> {
        self.mac_fullscreen.get(&id).map(|(origin, _)| *origin)
    }
    pub fn fullscreen_restore_geometry(&self, id: ClientId) -> Option<Rect> {
        self.fullscreen_restore.get(&id).copied()
    }

    /// Reattach a restored fullscreen window to its saved exclusive Space,
    /// avoiding a duplicate Space during opt-in session restore.
    pub fn restore_mac_fullscreen(&mut self, id: ClientId, origin: usize) -> bool {
        let Some(client) = self.clients.get(id) else {
            return false;
        };
        let space = client.workspace;
        if !self.separate_spaces()
            || origin >= self.workspace_count
            || origin == space
            || self.workspace_output_index(origin) != self.workspace_output_index(space)
            || self.mac_fullscreen.values().any(|(_, full)| *full == space)
        {
            return false;
        }
        self.mac_fullscreen.insert(id, (origin, space));
        self.fullscreen(id);
        true
    }

    /// EDID identities are useful only if unique. Identical/virtual heads must
    /// not accidentally share a row; connector names disambiguate those cases.
    fn space_display_key(&self, monitors: &[MonitorInfo], index: usize) -> String {
        let monitor = &monitors[index];
        // A duplicate EDID appearing/disappearing must not rename a live row.
        if let Some(display) = self.display_spaces.as_ref().and_then(|s| {
            s.snapshot.displays.iter().find(|d| {
                d.name == monitor.name
                    && (d.key == format!("connector:{}", monitor.name)
                        || monitor
                            .identity
                            .as_ref()
                            .is_some_and(|id| d.key == format!("display:{id}")))
            })
        }) {
            return display.key.clone();
        }
        if let Some(identity) = monitor.identity.as_deref().filter(|key| !key.is_empty()) {
            if monitors
                .iter()
                .filter(|m| m.identity.as_deref() == Some(identity))
                .count()
                == 1
            {
                return format!("display:{identity}");
            }
        }
        format!("connector:{}", monitor.name)
    }

    pub fn workspace_output_index(&self, workspace: usize) -> Option<usize> {
        let snapshot = self.display_spaces_snapshot()?;
        let owner = &snapshot.spaces.get(workspace)?.output_display;
        let display = snapshot.displays.iter().find(|d| d.connected && &d.key == owner)?;
        self.monitors_ref().iter().position(|m| m.name == display.name)
    }

    pub fn workspace_id(&self, workspace: usize) -> String {
        self.display_spaces_snapshot()
            .and_then(|s| s.spaces.get(workspace))
            .map_or_else(
                || format!("chonkstep-workspace-{workspace}"),
                |s| format!("chonkstep-space-{}", s.id),
            )
    }

    pub fn active_output_index(&self) -> usize {
        self.workspace_output_index(self.current_workspace).unwrap_or_else(|| {
            self.focused
                .map(|id| self.client_output_index(id))
                .or_else(|| self.last_pointer.map(|point| self.monitor_index_at(point)))
                .unwrap_or_else(|| self.primary_monitor_index())
        })
    }

    pub fn active_workspace_on_output(&self, index: usize) -> usize {
        let Some(snapshot) = self.display_spaces_snapshot() else {
            return self.current_workspace;
        };
        let Some(monitor) = self.monitors_ref().get(index) else {
            return self.current_workspace;
        };
        snapshot
            .displays
            .iter()
            .find(|d| d.connected && d.name == monitor.name)
            .and_then(|d| {
                snapshot
                    .spaces
                    .iter()
                    .position(|s| s.id == d.active && s.output_display == d.key)
            })
            .unwrap_or(self.current_workspace)
    }

    pub fn workspace_visible(&self, workspace: usize) -> bool {
        let Some(snapshot) = self.display_spaces_snapshot() else {
            return workspace == self.current_workspace;
        };
        snapshot.spaces.get(workspace).is_some_and(|space| {
            snapshot
                .displays
                .iter()
                .any(|display| display.connected && display.key == space.output_display && display.active == space.id)
        })
    }

    /// The active display's local strip, expressed as global numeric slots.
    pub fn workspace_row(&self) -> Vec<usize> {
        self.workspace_row_on_output(self.active_output_index())
    }

    pub fn workspace_row_on_output(&self, output: usize) -> Vec<usize> {
        if !self.separate_spaces() {
            return (0..self.workspace_count).collect();
        }
        (0..self.workspace_count)
            .filter(|&space| self.workspace_output_index(space) == Some(output))
            .collect()
    }

    /// Local indicator without allocating a row on each shell tick.
    pub fn workspace_position_count(&self, output: usize) -> (usize, usize) {
        let active = self.active_workspace_on_output(output);
        let (mut current, mut count) = (0, 0);
        for space in 0..self.workspace_count {
            if !self.separate_spaces() || self.workspace_output_index(space) == Some(output) {
                if space == active {
                    current = count;
                }
                count += 1;
            }
        }
        (current, count)
    }

    pub fn neighboring_workspace(&self, workspace: usize, direction: i32) -> Option<usize> {
        let row = self.workspace_row_on_output(
            self.workspace_output_index(workspace)
                .unwrap_or(self.active_output_index()),
        );
        let index = row.iter().position(|&space| space == workspace)?;
        let target = index.checked_add_signed(direction.signum() as isize)?;
        row.get(target).copied()
    }

    /// Pointer movement chooses the target display without stealing keyboard
    /// focus. Explicit application activation can select another display until
    /// the pointer next moves. Overview/gesture owners can keep their own target.
    pub fn select_output(&mut self, index: usize) {
        if !self.separate_spaces() {
            return;
        }
        let workspace = self.active_workspace_on_output(index);
        self.select_space_output(workspace);
    }

    pub(super) fn select_space_output(&mut self, workspace: usize) {
        let Some(state) = self
            .display_spaces
            .as_mut()
            .filter(|_| self.interaction.separate_spaces)
        else {
            return;
        };
        let Some(space) = state.snapshot.spaces.get(workspace) else {
            return;
        };
        let owner = &space.output_display;
        let active = state
            .snapshot
            .displays
            .iter()
            .find(|d| d.connected && &d.key == owner)
            .map(|d| d.active);
        let Some(current) = state.snapshot.spaces.iter().position(|s| Some(s.id) == active) else {
            return;
        };
        if &state.snapshot.selected == owner && self.current_workspace == current {
            return;
        }
        state.snapshot.selected = owner.clone();
        self.current_workspace = current;
        self.bump_protocol_state_revision();
        self.backend.publish_workspaces(self.workspace_count, current);
    }

    fn append_display_space(&mut self, key: &str) -> Option<usize> {
        if self.workspace_count >= MAX_WORKSPACES {
            return None;
        }
        let state = self.display_spaces.as_mut()?;
        let id = state.snapshot.next_id.checked_add(1)?;
        state.snapshot.next_id = id;
        let index = self.workspace_count;
        state.snapshot.spaces.push(Space {
            id,
            home_display: key.into(),
            output_display: key.into(),
        });
        self.workspace_count += 1;
        self.layouts
            .resize_with(self.workspace_count, crate::spatial::WorkspaceLayout::default);
        Some(index)
    }

    pub fn create_workspace(&mut self) -> Option<usize> {
        if self.workspace_count >= MAX_WORKSPACES {
            return None;
        }
        let space = if let Some(snapshot) = self.display_spaces_snapshot() {
            let key = snapshot.selected.clone();
            self.append_display_space(&key)?
        } else {
            let space = self.workspace_count;
            self.workspace_count += 1;
            self.layouts
                .resize_with(self.workspace_count, crate::spatial::WorkspaceLayout::default);
            space
        };
        self.bump_protocol_state_revision();
        self.backend
            .publish_workspaces(self.workspace_count, self.current_workspace);
        Some(space)
    }

    pub(super) fn ensure_display_space_slots(&mut self, count: usize) -> bool {
        if !self.separate_spaces() {
            return true;
        }
        let state = self.display_spaces.as_mut().unwrap();
        let additional = count.saturating_sub(state.snapshot.spaces.len()) as u64;
        if state.snapshot.next_id.checked_add(additional).is_none() { return false; }
        while state.snapshot.spaces.len() < count {
            state.snapshot.next_id += 1;
            state.snapshot.spaces.push(Space {
                id: state.snapshot.next_id,
                home_display: state.snapshot.selected.clone(),
                output_display: state.snapshot.selected.clone(),
            });
        }
        true
    }

    /// Called at topology/configuration boundaries, never at frame cadence.
    pub fn reconcile_display_spaces(&mut self) {
        if !self.mac_mode() || !self.interaction.separate_spaces {
            return;
        }
        self.end_active_drag();
        let monitors = self.monitors();
        if monitors.is_empty() {
            if let Some(state) = self.display_spaces.as_mut() {
                for display in &mut state.snapshot.displays {
                    display.connected = false;
                }
                self.refresh_space_visibility();
                self.repair_space_focus();
                self.bump_protocol_state_revision();
                self.backend
                    .publish_workspaces(self.workspace_count, self.current_workspace);
            }
            return;
        }
        let keys: Vec<_> = (0..monitors.len())
            .map(|i| self.space_display_key(&monitors, i))
            .collect();
        let primary = self.primary_monitor_index().min(monitors.len() - 1);
        let initializing = self.display_spaces.is_none();
        if initializing {
            let key = keys[primary].clone();
            self.display_spaces = Some(DisplaySpaces {
                reconciling: true,
                snapshot: DisplaySpacesSnapshot {
                    spaces: (0..self.workspace_count)
                        .map(|i| Space {
                            id: i as u64 + 1,
                            home_display: key.clone(),
                            output_display: key.clone(),
                        })
                        .collect(),
                    displays: Vec::new(),
                    selected: key,
                    next_id: self.workspace_count as u64,
                },
            });
        }
        let state = self.display_spaces.as_mut().unwrap();
        state.reconciling = true;
        let old_displays = state.snapshot.displays.clone();
        let previous_owner: Vec<_> = state.snapshot.spaces.iter().map(|s| s.output_display.clone()).collect();
        for display in &mut state.snapshot.displays {
            display.connected = false;
        }
        for (monitor, key) in monitors.iter().zip(&keys) {
            if let Some(display) = state.snapshot.displays.iter_mut().find(|d| &d.key == key) {
                display.name.clone_from(&monitor.name);
                display.geometry = monitor.geometry;
                display.connected = true;
            } else if state.snapshot.displays.len() < MAX_WORKSPACES {
                state.snapshot.displays.push(DisplaySpace {
                    key: key.clone(),
                    name: monitor.name.clone(),
                    geometry: monitor.geometry,
                    active: 0,
                    connected: true,
                });
            }
        }
        for space in &mut state.snapshot.spaces {
            if keys.contains(&space.home_display) {
                space.output_display.clone_from(&space.home_display);
            } else if !keys.contains(&space.output_display) {
                space.output_display.clone_from(&keys[primary]);
            }
        }
        for key in &keys {
            let has_regular = self
                .display_spaces
                .as_ref()
                .unwrap()
                .snapshot
                .spaces
                .iter()
                .enumerate()
                .any(|(index, s)| {
                    &s.output_display == key && !self.mac_fullscreen.values().any(|(_, full)| *full == index)
                });
            if !has_regular && self.append_display_space(key).is_none() {
                // At the global safety cap, move an existing desktop instead of
                // advertising a display with no reachable active Space. Prefer
                // empty desktops and preserve stable IDs/window membership.
                let snapshot = &self.display_spaces.as_ref().unwrap().snapshot;
                let candidate = snapshot
                    .spaces
                    .iter()
                    .enumerate()
                    .filter(|(index, space)| {
                        !self.mac_fullscreen.values().any(|(_, full)| full == index)
                            && snapshot
                                .spaces
                                .iter()
                                .enumerate()
                                .filter(|(other, s)| {
                                    s.output_display == space.output_display
                                        && !self.mac_fullscreen.values().any(|(_, full)| full == other)
                                })
                                .count()
                                > 1
                    })
                    .min_by_key(|(index, _)| self.workspace_has_windows(*index))
                    .map(|(index, _)| index);
                if let Some(index) = candidate {
                    let space = &mut self.display_spaces.as_mut().unwrap().snapshot.spaces[index];
                    space.home_display.clone_from(key);
                    space.output_display.clone_from(key);
                }
            }
        }
        let state = self.display_spaces.as_mut().unwrap();
        for display in &mut state.snapshot.displays {
            if !display.connected {
                continue;
            }
            if !state
                .snapshot
                .spaces
                .iter()
                .any(|s| s.id == display.active && s.output_display == display.key)
            {
                display.active = state
                    .snapshot
                    .spaces
                    .iter()
                    .find(|s| s.output_display == display.key)
                    .map_or(0, |s| s.id);
            }
        }
        if !keys.contains(&state.snapshot.selected) {
            state.snapshot.selected.clone_from(&keys[primary]);
        }
        let active = state
            .snapshot
            .displays
            .iter()
            .find(|d| d.key == state.snapshot.selected)
            .map(|d| d.active);
        self.current_workspace = state
            .snapshot
            .spaces
            .iter()
            .position(|s| Some(s.id) == active)
            .unwrap_or(0);

        if initializing {
            // Split a running linked desktop without merging windows from
            // distinct old desktops. Empty matching slots are created lazily.
            let mut assignments = HashMap::new();
            let clients: Vec<_> = self.clients.keys().collect();
            for id in clients {
                let output =
                    self.monitor_index_at(self.client_frame_center(id).unwrap_or(self.clients[id].geometry.pos));
                if output == primary {
                    continue;
                }
                let old = self.clients[id].workspace;
                let target = if let Some(&target) = assignments.get(&(output, old)) {
                    target
                } else {
                    let current = self.active_workspace_on_output(output);
                    let target = if assignments.keys().any(|(monitor, _)| *monitor == output) {
                        self.append_display_space(&keys[output]).unwrap_or(current)
                    } else {
                        current
                    };
                    assignments.insert((output, old), target);
                    target
                };
                self.assign_space_membership(id, target);
            }
        }
        let clients: Vec<_> = self.clients.keys().collect();
        for id in clients {
            let workspace = self.clients[id].workspace;
            let Some(output) = self.workspace_output_index(workspace) else {
                continue;
            };
            let target = monitors[output].geometry;
            let old = previous_owner
                .get(workspace)
                .and_then(|key| old_displays.iter().find(|d| &d.key == key))
                .map(|d| d.geometry);
            if let Some(old) = old.filter(|old| *old != target) {
                self.translate_client_between_displays(id, old, target);
            }
            self.publish_space_output(id);
            self.reflow_frame(id);
        }
        self.display_spaces.as_mut().unwrap().reconciling = false;
        self.refresh_space_visibility();
        self.repair_space_focus();
        self.reflow_layouts();
        self.bump_protocol_state_revision();
        self.backend
            .publish_workspaces(self.workspace_count, self.current_workspace);
        self.publish_workarea_union();
    }

    pub(super) fn translate_space_move(&mut self, id: ClientId, workspace: usize) {
        let Some(old) = self
            .clients
            .get(id)
            .and_then(|c| self.workspace_output_index(c.workspace))
        else {
            return;
        };
        let Some(new) = self.workspace_output_index(workspace) else {
            return;
        };
        if old != new {
            self.translate_client_between_displays(
                id,
                self.monitors_ref()[old].geometry,
                self.monitors_ref()[new].geometry,
            );
        }
    }

    fn translate_client_between_displays(&mut self, id: ClientId, from: Rect, to: Rect) {
        let translate = |rect: &mut Rect| {
            rect.pos.x = rect.pos.x.saturating_sub(from.pos.x).saturating_add(to.pos.x);
            rect.pos.y = rect.pos.y.saturating_sub(from.pos.y).saturating_add(to.pos.y);
            rect.pos.x = rect.pos.x.clamp(
                to.pos.x,
                to.pos.x.saturating_add(to.size.w.saturating_sub(rect.size.w) as i32),
            );
            rect.pos.y = rect.pos.y.clamp(
                to.pos.y,
                to.pos.y.saturating_add(to.size.h.saturating_sub(rect.size.h) as i32),
            );
        };
        if let Some(client) = self.clients.get_mut(id) {
            translate(&mut client.geometry);
            if let Some(rect) = &mut client.restore_geometry {
                translate(rect);
            }
            if let Some(rect) = &mut client.placement.freeform {
                translate(rect);
            }
        }
        if let Some(rect) = self.fullscreen_restore.get_mut(&id) {
            translate(rect);
        }
    }

    pub(super) fn publish_space_output(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let name = self
            .workspace_output_index(client.workspace)
            .and_then(|i| self.monitors_ref().get(i))
            .map(|m| m.name.clone());
        self.backend.set_window_space_output(client.window, name.as_deref());
    }

    fn assign_space_membership(&mut self, id: ClientId, workspace: usize) {
        let old = self.clients[id].workspace;
        if old == workspace {
            return;
        }
        self.clients[id].workspace = workspace;
        self.backend.publish_window_desktop(self.clients[id].window, workspace);
        if let Some(layout) = self.layouts.get_mut(old) {
            layout.order.retain(|&other| other != id);
        }
        self.register_layout_client(id);
        self.publish_space_output(id);
        if self.workspace_layout(workspace) == crate::LayoutMode::Freeform { self.restore_freeform_view(id); }
        self.bump_protocol_state_revision();
    }

    pub(super) fn track_window_display(&mut self, id: ClientId) {
        if !self.separate_spaces() || self.display_spaces.as_ref().is_some_and(|s| s.reconciling) {
            return;
        }
        let Some(client) = self.clients.get(id) else {
            return;
        };
        if client.flags.contains(ClientFlags::FULLSCREEN)
            || (self.workspace_layout(client.workspace) != crate::LayoutMode::Freeform && !client.placement.floating)
        {
            return;
        }
        let output = self.monitor_index_at(self.client_frame_center(id).unwrap_or(client.geometry.pos));
        if self.workspace_output_index(client.workspace) == Some(output) {
            return;
        }
        self.move_family_to_display(id, output);
    }

    pub(super) fn move_family_to_display(&mut self, id: ClientId, output: usize) {
        if !self.separate_spaces() || !self.clients.contains_key(id) {
            return;
        }
        let mut root = id;
        for _ in 0..8 {
            match self.clients.get(root).and_then(|c| c.parent) {
                Some(parent) if parent != id && self.clients.contains_key(parent) => root = parent,
                _ => break,
            }
        }
        let target = self.regular_workspace_on_output(output);
        let family = self.transient_family(root);
        let old_workspaces: HashSet<_> = family.iter().map(|&member| self.clients[member].workspace).collect();
        let owns_focus = self.focused.is_some_and(|focused| family.contains(&focused));
        for &member in &family {
            if member != id {
                self.translate_space_move(member, target);
            }
            self.assign_space_membership(member, target);
        }
        // Arriving at a fullscreen display reveals its regular desktop; an
        // unrelated window must not silently join another app's fullscreen Space.
        if !self.workspace_visible(target) {
            self.switch_workspace(target);
        }
        for old in old_workspaces { if old != target { self.reflow_workspace(old); } }
        self.reflow_workspace(target);
        for member in family {
            if member != id {
                self.reflow_frame(member);
            }
        }
        if owns_focus {
            self.select_output(output);
        }
    }

    fn repair_space_focus(&mut self) {
        if self.focused.is_none_or(|id| self.is_focusable(id)) {
            return;
        }
        if let Some(previous) = self.focused.take() {
            if let Some(client) = self.clients.get_mut(previous) {
                client.flags.remove(ClientFlags::FOCUSED);
            }
            self.repaint_decoration(previous);
        }
        let next = self
            .focus_history
            .iter()
            .rev()
            .copied()
            .chain(self.clients.keys())
            .find(|&id| self.clients[id].workspace == self.current_workspace && self.is_focusable(id));
        if let Some(next) = next {
            self.focus_client(next);
        } else {
            self.backend.publish_active_window(None);
        }
    }

    pub(super) fn regular_workspace_on_output(&self, output: usize) -> usize {
        let current = self.active_workspace_on_output(output);
        self.mac_fullscreen
            .values()
            .find(|(_, full)| *full == current)
            .map_or(current, |(origin, _)| *origin)
    }

    pub(super) fn refresh_space_visibility(&mut self) {
        let ids: Vec<_> = self.clients.keys().collect();
        for id in ids {
            let client = &self.clients[id];
            if client.lifecycle != Lifecycle::Normal {
                continue;
            }
            if !self.mac_client_hidden(id)
                && (self.workspace_visible(client.workspace) || client.flags.contains(ClientFlags::STICKY))
            {
                self.show_client_surface(id);
                self.repaint_decoration(id);
            } else {
                self.hide_client_surface(id);
            }
        }
    }

    pub(super) fn switch_display_workspace(&mut self, workspace: usize) {
        if !self.ensure_display_space_slots(workspace + 1) { return; }
        self.workspace_count = self.workspace_count.max(workspace + 1);
        self.layouts
            .resize_with(self.workspace_count, crate::spatial::WorkspaceLayout::default);
        let state = self.display_spaces.as_mut().unwrap();
        let space = &state.snapshot.spaces[workspace];
        let Some(display) = state
            .snapshot
            .displays
            .iter_mut()
            .find(|d| d.connected && d.key == space.output_display)
        else {
            return;
        };
        if display.active == space.id && self.current_workspace == workspace {
            return;
        }
        display.active = space.id;
        state.snapshot.selected.clone_from(&display.key);
        self.current_workspace = workspace;
        self.refresh_space_visibility();
        let next = self
            .focus_history
            .iter()
            .rev()
            .copied()
            .chain(self.clients.keys())
            .find(|&id| self.clients[id].workspace == workspace && self.is_focusable(id));
        if let Some(next) = next {
            self.focus_client(next);
        } else if let Some(previous) = self.focused.take() {
            if let Some(client) = self.clients.get_mut(previous) {
                client.flags.remove(ClientFlags::FOCUSED);
            }
            self.repaint_decoration(previous);
            self.backend.publish_active_window(None);
        }
        self.bump_protocol_state_revision();
        self.backend
            .publish_workspaces(self.workspace_count, self.current_workspace);
    }

    pub(super) fn remove_display_workspace(&mut self, workspace: usize) -> bool {
        if workspace >= self.workspace_count || self.mac_fullscreen.values().any(|(_, full)| *full == workspace) {
            return false;
        }
        let Some(output) = self.workspace_output_index(workspace) else {
            return false;
        };
        let row: Vec<_> = self
            .workspace_row_on_output(output)
            .into_iter()
            .filter(|index| !self.mac_fullscreen.values().any(|(_, full)| full == index))
            .collect();
        if row.len() <= 1 {
            return false;
        }
        let at = row.iter().position(|&index| index == workspace).unwrap();
        let target = row[if at == 0 { 1 } else { at - 1 }];
        self.end_active_drag();
        let removed = self.layouts.remove(workspace);
        let remap = |index: usize| {
            let index = if index == workspace { target } else { index };
            index - usize::from(index > workspace)
        };
        let destination = remap(target);
        self.layouts[destination].order.extend(removed.order);
        let state = self.display_spaces.as_mut().unwrap();
        let target_id = state.snapshot.spaces[target].id;
        let removed_id = state.snapshot.spaces.remove(workspace).id;
        for display in &mut state.snapshot.displays {
            if display.active == removed_id {
                display.active = target_id;
            }
        }
        for (origin, full) in self.mac_fullscreen.values_mut() {
            *origin = remap(*origin);
            *full = remap(*full);
        }
        for client in self.clients.values_mut() {
            let old = client.workspace;
            client.workspace = remap(old);
            if old != client.workspace {
                self.backend.publish_window_desktop(client.window, client.workspace);
            }
        }
        self.workspace_count -= 1;
        self.current_workspace = remap(self.current_workspace);
        self.refresh_space_visibility();
        self.repair_space_focus();
        self.reflow_layouts();
        self.bump_protocol_state_revision();
        self.backend
            .publish_workspaces(self.workspace_count, self.current_workspace);
        self.publish_workarea_union();
        true
    }
}
