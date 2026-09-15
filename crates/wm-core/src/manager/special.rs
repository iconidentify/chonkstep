//! Special workspaces: named overlays shown per output, each with its
//! own layout.
//!
//! Omarchy's scratchpad is a hidden workspace that drops down over
//! whatever the user is doing and goes away again with the same chord.
//! It is not an extra numbered workspace — a client keeps its numbered
//! home in [`Client::workspace`] and *also* carries a special
//! membership in [`Client::special`], and while it is a member the
//! special decides whether it is on screen, not the numbered row.
//!
//! One visibility predicate serves every caller. Focus, Alt-Tab, idle
//! rules and the workspace switch all ask [`WindowManager::client_visible`]
//! (through `client_on_screen`) rather than testing the numbered row
//! themselves, so a special member is hidden, unfocusable and ignored
//! by idle exactly as a window parked on another workspace is, and is
//! shown, raised and focusable exactly as one on the current workspace
//! is, with no second copy of the rule to drift.
//!
//! Shown members are raised through the ordinary stacking path and
//! hidden ones are unmapped like parked windows, so rendered order and
//! hit-test order stay one list. Shown members sit *above* pinned
//! windows: the overlay is the thing the user just asked for, and a
//! picture-in-picture window pinned to a corner must not hide the
//! console that dropped over it. [`WindowManager::raise_client`]
//! reasserts that after every raise, as it already does for pinning.

use super::*;
use crate::spatial::WorkspaceLayout;

/// The most special workspaces one session may create. IPC and window
/// rules name them freely, so the count is bounded before anything
/// allocates.
pub const MAX_SPECIAL_WORKSPACES: usize = 16;

/// The longest name a special workspace may carry, in bytes.
pub const MAX_SPECIAL_NAME: usize = 64;

/// The name Hyprland gives the special workspace named by nothing but
/// `special` — a bare `workspace = "special silent"` rule, or
/// `togglespecialworkspace` with no argument.
pub const DEFAULT_SPECIAL_NAME: &str = "special";

/// One special workspace: its name and the layout its members share.
pub(crate) struct SpecialWorkspace {
    pub name: String,
    pub layout: WorkspaceLayout,
}

/// The name a special workspace selector means, or `None` when it is
/// not a name this desktop will create.
///
/// Accepts `special`, `special:NAME` and a bare `NAME`, trimmed; an
/// empty selector is the default special workspace. Refused: a name
/// over [`MAX_SPECIAL_NAME`] bytes, and one carrying control
/// characters — both arrive from an unauthenticated socket, and a
/// name is published back out through it, so it is bounded and
/// printable before it exists.
pub fn normalize_special_name(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let name = raw.strip_prefix("special:").map_or(raw, str::trim);
    let name = if name.is_empty() || name == "special" { DEFAULT_SPECIAL_NAME } else { name };
    if name.len() > MAX_SPECIAL_NAME || name.chars().any(char::is_control) {
        return None;
    }
    Some(name.to_string())
}

impl<B: Backend> WindowManager<B> {
    /// Omarchy's `binds.hide_special_on_workspace_change`: whether a
    /// workspace switch takes the output's shown special down with it.
    pub fn set_hide_special_on_workspace_change(&mut self, hide: bool) {
        self.hide_special_on_workspace_change = hide;
    }

    /// Hyprland's `misc:focus_on_activate`: whether an application's
    /// own `xdg_activation_v1` request is honoured without the user's
    /// input behind it. Off, such a request marks the window urgent.
    pub fn set_focus_on_activate(&mut self, focus: bool) {
        self.focus_on_activate = focus;
    }

    /// The live `misc:focus_on_activate` value; see
    /// [`Self::set_focus_on_activate`].
    pub fn focus_on_activate(&self) -> bool {
        self.focus_on_activate
    }

    /// Every special workspace this session has created, by index and
    /// name. Indices are stable for the session: a special is never
    /// destroyed, so the wire can derive a lasting id from one.
    pub fn special_workspaces(&self) -> impl Iterator<Item = (usize, &str)> + '_ {
        self.specials.iter().enumerate().map(|(index, special)| (index, special.name.as_str()))
    }

    pub fn special_name(&self, index: usize) -> Option<&str> {
        self.specials.get(index).map(|special| special.name.as_str())
    }

    /// The index of the special workspace `name` selects, if it exists.
    pub fn special_index(&self, name: &str) -> Option<usize> {
        let name = normalize_special_name(name)?;
        self.specials.iter().position(|special| special.name == name)
    }

    /// The members of a special workspace in layout order, live or not.
    pub fn special_layout_order(&self, index: usize) -> &[ClientId] {
        self.specials.get(index).map_or(&[], |special| special.layout.order.as_slice())
    }

    pub fn special_layout_mode(&self, index: usize) -> crate::LayoutMode {
        self.specials.get(index).map_or(crate::LayoutMode::Freeform, |special| special.layout.mode)
    }

    /// The special workspace shown on the output at `output`, if any.
    pub fn special_shown_on_output(&self, output: usize) -> Option<usize> {
        self.special_shown.get(&self.output_key(output)).copied()
    }

    /// Whether the special at `index` is shown on a connected output.
    ///
    /// Only a *connected* output counts. An entry for an output that has
    /// gone is pruned when the outputs change, and until then it must
    /// not make a member "visible": a window nobody can see would take
    /// focus.
    pub fn special_visible(&self, index: usize) -> bool {
        self.connected_output_keys().iter().any(|key| self.special_shown.get(key) == Some(&index))
    }

    pub(super) fn special_output_index(&self, index: usize) -> Option<usize> {
        self.connected_output_keys().iter().position(|key| self.special_shown.get(key) == Some(&index))
    }

    /// Whether the user can see this window: mapped, and either its
    /// special workspace is shown, or it sits on a visible numbered
    /// workspace or is pinned to all of them. The one predicate the
    /// focus, cycling, idle and workspace-switch paths all read.
    pub fn client_visible(&self, id: ClientId) -> bool {
        self.clients.get(id).is_some_and(|client| self.client_on_screen(client))
    }

    /// The placement half of [`Self::client_visible`]: where this
    /// client lives, is that place on screen? Lifecycle is the caller's.
    pub(super) fn client_placed_on_screen(&self, client: &Client<B>) -> bool {
        match client.special {
            Some(index) => self.special_visible(index),
            None => self.workspace_visible(client.workspace) || client.flags.contains(ClientFlags::STICKY),
        }
    }

    /// Shows the named special on the active output, or hides it if it
    /// is the one shown there. Creates the special on first use, within
    /// [`MAX_SPECIAL_WORKSPACES`]; `false` when the name is refused or
    /// the count is spent.
    pub fn toggle_special(&mut self, name: &str) -> bool {
        let Some(index) = self.find_or_create_special(name) else {
            return false;
        };
        let output = self.active_output_index();
        if self.special_shown_on_output(output) == Some(index) {
            self.hide_special_on_output(output);
        } else {
            self.show_special_on_output(index, output);
        }
        true
    }

    /// Moves `id` and its transient family onto the named special. With
    /// `follow` the special is shown on the active output and the window
    /// focused; without it the window leaves the screen unless that
    /// special is already shown, and focus moves on. `false` when the
    /// client is unknown, the name is refused or the count is spent.
    pub fn move_client_to_special(&mut self, id: ClientId, name: &str, follow: bool) -> bool {
        if !self.clients.contains_key(id) {
            return false;
        }
        let Some(index) = self.find_or_create_special(name) else {
            return false;
        };
        self.cancel_client_layout_interaction(id);
        // Where the user is, read before the move hands focus elsewhere.
        let output = self.active_output_index();
        for member in self.transient_family(id) {
            self.move_one_client_to_special(member, index);
        }
        if follow {
            self.show_special_on_output(index, output);
            self.focus_client(id);
        }
        tracing::info!(?id, special = %self.specials[index].name, follow, "moved window to a special workspace");
        true
    }

    fn find_or_create_special(&mut self, name: &str) -> Option<usize> {
        let name = normalize_special_name(name)?;
        if let Some(index) = self.specials.iter().position(|special| special.name == name) {
            return Some(index);
        }
        if self.specials.len() >= MAX_SPECIAL_WORKSPACES {
            tracing::warn!(name = %name, limit = MAX_SPECIAL_WORKSPACES, "refusing to create another special workspace");
            return None;
        }
        self.specials.push(SpecialWorkspace { name, layout: WorkspaceLayout::default() });
        self.bump_protocol_state_revision();
        Some(self.specials.len() - 1)
    }

    fn output_key(&self, output: usize) -> String {
        self.monitors_ref().get(output).map(|monitor| Self::layout_output_key(monitor).to_string()).unwrap_or_default()
    }

    /// The keys of every connected output. With no outputs reported at
    /// all the single unnamed key stands for the one screen there is,
    /// so a headless session still has somewhere to show a special.
    fn connected_output_keys(&self) -> Vec<String> {
        let monitors = self.monitors_ref();
        if monitors.is_empty() {
            return vec![String::new()];
        }
        monitors.iter().map(|monitor| Self::layout_output_key(monitor).to_string()).collect()
    }

    fn output_workarea(&self, output: usize) -> Option<Rect> {
        let monitor = self.monitors_ref().get(output)?;
        Some(self.workareas.get(output).copied().unwrap_or(monitor.geometry))
    }

    /// The live members of a special, in layout order.
    fn special_members(&self, index: usize) -> Vec<ClientId> {
        self.special_layout_order(index)
            .iter()
            .copied()
            .filter(|&id| self.clients.get(id).is_some_and(|client| client.lifecycle == Lifecycle::Normal))
            .collect()
    }

    /// The most recently focused focusable member of a shown special
    /// other than `excluding`, for a member that is closing.
    pub(super) fn special_focus_successor(&self, index: usize, excluding: ClientId) -> Option<ClientId> {
        if !self.special_visible(index) {
            return None;
        }
        let members = self.special_members(index);
        self.focus_history
            .iter()
            .rev()
            .copied()
            .chain(members.iter().copied())
            .find(|&id| id != excluding && members.contains(&id) && self.is_focusable(id))
    }

    /// The live members of every special shown on a connected output,
    /// in layout order — the windows that form the overlay tier.
    pub(super) fn shown_special_members(&self) -> Vec<ClientId> {
        let mut shown: Vec<usize> = self
            .connected_output_keys()
            .iter()
            .filter_map(|key| self.special_shown.get(key).copied())
            .collect();
        shown.sort_unstable();
        shown.dedup();
        shown.into_iter().flat_map(|index| self.special_members(index)).collect()
    }

    /// Puts the overlay tier back on top after something remapped
    /// windows underneath it without going through a raise.
    pub(super) fn raise_shown_specials(&mut self) {
        for member in self.shown_special_members() {
            self.raise_transient_family(member);
        }
    }

    pub(super) fn show_special_on_output(&mut self, index: usize, output: usize) {
        let key = self.output_key(output);
        // One place at a time: a special shown on another output moves
        // here, and the special this output was showing goes away first.
        let elsewhere: Vec<String> =
            self.special_shown.iter().filter(|(k, &v)| v == index && **k != key).map(|(k, _)| k.clone()).collect();
        for other in elsewhere {
            self.special_shown.remove(&other);
        }
        let displaced = self.special_shown.insert(key, index).filter(|&other| other != index);
        self.bump_protocol_state_revision();
        if let Some(other) = displaced {
            self.hide_special_members(other);
        }
        let members = self.special_members(index);
        for &id in &members {
            self.carry_frame_to_output(id, output);
            self.publish_space_output(id);
            self.show_client_surface(id);
            // A remapped frame is not guaranteed to still hold its
            // pixels; repaint rather than wait for an expose.
            self.repaint_decoration(id);
        }
        self.reflow_special(index);
        for &id in &members {
            self.raise_client(id);
        }
        // The member the user was last in comes back with the keyboard,
        // and its raise puts it on top of the others.
        let recent = self
            .focus_history
            .iter()
            .rev()
            .copied()
            .chain(members.iter().copied())
            .find(|id| members.contains(id) && self.is_focusable(*id));
        if let Some(next) = recent {
            self.focus_client(next);
        }
        tracing::info!(special = %self.specials[index].name, output, members = members.len(), "showed special workspace");
    }

    fn hide_special_on_output(&mut self, output: usize) {
        let key = self.output_key(output);
        let Some(index) = self.special_shown.remove(&key) else {
            return;
        };
        self.bump_protocol_state_revision();
        self.hide_special_members(index);
        tracing::info!(special = %self.specials[index].name, output, "hid special workspace");
    }

    /// Takes a special's members off screen. The `special_shown` entry
    /// must already be gone, so `is_focusable` rejects every member and
    /// the successor search cannot hand focus back to one of them.
    fn hide_special_members(&mut self, index: usize) {
        let members = self.special_members(index);
        for &id in &members {
            self.hide_client_surface(id);
        }
        if let Some(losing) = self.focused.filter(|focused| members.contains(focused)) {
            self.focus_successor_of(losing);
        }
    }

    pub(super) fn move_one_client_to_special(&mut self, id: ClientId, index: usize) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        if client.special == Some(index) {
            return;
        }
        let old_workspace = client.workspace;
        let old_special = client.special;
        let left_layout = old_special.is_none() && self.workspace_layout(old_workspace) != crate::LayoutMode::Freeform;
        if let Some(layout) = self.layouts.get_mut(old_workspace) {
            layout.order.retain(|&other| other != id);
        }
        if let Some(old) = old_special {
            self.specials[old].layout.order.retain(|&other| other != id);
        }
        self.clients[id].special = Some(index);
        self.specials[index].layout.order.push(id);
        if left_layout {
            self.restore_freeform_view(id);
        }
        self.reflow_workspace(old_workspace);
        if let Some(old) = old_special {
            self.reflow_special(old);
        }
        if self.special_visible(index) {
            if self.clients[id].lifecycle == Lifecycle::Normal {
                if let Some(output) = self.connected_output_keys().iter().position(|key| self.special_shown.get(key) == Some(&index)) {
                    self.carry_frame_to_output(id, output);
                }
                self.show_client_surface(id);
                self.repaint_decoration(id);
                self.raise_client(id);
            }
        } else {
            // `special` is already set and the special is not shown, so
            // `is_focusable` rejects this client and the successor
            // search cannot pick it back up.
            self.focus_successor_of(id);
            self.hide_client_surface(id);
        }
        self.reflow_special(index);
        self.publish_space_output(id);
        self.bump_protocol_state_revision();
    }

    /// Takes `id` out of its special workspace, if it is in one. The
    /// caller puts it somewhere else — a numbered workspace — and shows
    /// or hides it accordingly. Returns whether there was anything to
    /// leave.
    pub(super) fn leave_special(&mut self, id: ClientId) -> bool {
        let Some(index) = self.clients.get(id).and_then(|client| client.special) else {
            return false;
        };
        self.clients[id].special = None;
        self.specials[index].layout.order.retain(|&other| other != id);
        self.reflow_special(index);
        self.bump_protocol_state_revision();
        true
    }

    /// The removal half for a window that is going away: out of every
    /// special's order. A shown special whose last member closes stays
    /// shown and empty, which is what Omarchy's console expects.
    pub(super) fn forget_special_member(&mut self, id: ClientId) {
        let mut reflow = Vec::new();
        for (index, special) in self.specials.iter_mut().enumerate() {
            let before = special.layout.order.len();
            special.layout.order.retain(|&other| other != id);
            if special.layout.order.len() != before {
                reflow.push(index);
            }
        }
        for index in reflow {
            self.reflow_special(index);
        }
    }

    pub(super) fn reflow_special(&mut self, index: usize) {
        if index < self.specials.len() {
            self.reflow_slot_with_force(spatial_layout::LayoutSlot::Special(index), false);
        }
    }

    /// Drops the shown-special entry of every output that is no longer
    /// connected, hiding the members the entry was keeping on screen.
    pub(super) fn prune_special_shown(&mut self) {
        let connected = self.connected_output_keys();
        let dropped: Vec<usize> = self
            .special_shown
            .iter()
            .filter(|(key, _)| !connected.contains(key))
            .map(|(_, &index)| index)
            .collect();
        if dropped.is_empty() {
            return;
        }
        self.special_shown.retain(|key, _| connected.contains(key));
        self.bump_protocol_state_revision();
        for index in dropped {
            if !self.special_visible(index) {
                self.hide_special_members(index);
            }
        }
    }

    /// The `hide_special_on_workspace_change` half of a switch to
    /// `workspace`: the special shown on the output that switch lands
    /// on goes down with it.
    pub(super) fn hide_special_for_workspace_switch(&mut self, workspace: usize) {
        if !self.hide_special_on_workspace_change {
            return;
        }
        let output = self.workspace_output_index(workspace).unwrap_or_else(|| self.active_output_index());
        self.hide_special_on_output(output);
    }

    /// Brings a member's frame onto `output` when it sits on another,
    /// keeping its offset inside the workarea and clamping it in — the
    /// overlay is shown *here*, and a member left on the other head
    /// would be an invisible window holding focus.
    fn carry_frame_to_output(&mut self, id: ClientId, output: usize) {
        // The shown-output entry already names the destination. Read
        // the frame's actual location to work out the translation.
        let Some(center) = self.client_frame_center(id) else { return };
        let from = self.monitor_index_at(center);
        if from == output {
            return;
        }
        let (Some(from_area), Some(to_area)) = (self.output_workarea(from), self.output_workarea(output)) else {
            return;
        };
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let frame = client_frame_rect(client);
        let desired = Point::new(
            to_area.pos.x.saturating_add(frame.pos.x.saturating_sub(from_area.pos.x)),
            to_area.pos.y.saturating_add(frame.pos.y.saturating_sub(from_area.pos.y)),
        );
        let pos = placement::clamp_to(to_area, frame.size, desired);
        let offset = Point::new(client.geometry.pos.x - frame.pos.x, client.geometry.pos.y - frame.pos.y);
        let key = self.output_key(output);
        let client = &mut self.clients[id];
        client.geometry.pos = Point::new(pos.x + offset.x, pos.y + offset.y);
        client.placement.output = Some(key);
        self.reflow_frame(id);
    }
}
