//! Application grouping and reversible desktop visibility for Mac interaction.

use super::*;

impl<B: Backend> WindowManager<B> {
    pub fn cancel_keyboard_cycle(&mut self) {
        self.cycle_end(false);
    }

    pub fn mac_mode(&self) -> bool {
        self.interaction.mode == crate::InteractionMode::Mac
    }
    pub fn interaction_config(&self) -> &crate::InteractionConfig {
        &self.interaction
    }

    pub fn set_interaction_config(&mut self, config: crate::InteractionConfig) {
        if self.interaction == config {
            return;
        }
        self.cycle_end(false);
        let was_mac = self.mac_mode();
        let spaces_changed = config.separate_spaces != self.interaction.separate_spaces;
        if config.mode != crate::InteractionMode::Mac || spaces_changed {
            let fullscreen: Vec<_> = self.mac_fullscreen.keys().copied().collect();
            for id in fullscreen {
                self.unfullscreen(id);
            }
        }
        self.interaction = config;
        if self.mac_mode() && self.interaction.separate_spaces {
            self.reconcile_display_spaces();
        } else if self.display_spaces.take().is_some() {
            let ids: Vec<_> = self.clients.keys().collect();
            for id in ids { self.publish_space_output(id); }
            self.refresh_space_visibility();
            self.repair_space_focus();
            self.bump_protocol_state_revision();
            self.backend.publish_workspaces(self.workspace_count, self.current_workspace);
        }

        if was_mac != self.mac_mode() {
            for modifiers in [Modifiers::ALT, Modifiers::ALT | Modifiers::SHIFT] {
                let key = KeyCombo {
                    keysym: XK_TAB,
                    modifiers,
                };
                if self.mac_mode() {
                    self.backend.ungrab_key(key);
                } else {
                    self.backend.grab_key(key);
                }
            }
            if !self.mac_mode() {
                self.mac_hidden.clear();
                self.desktop_reveal = None;
                let ids: Vec<_> = self.clients.keys().filter(|&id| self.is_focusable(id)).collect();
                for id in ids {
                    self.show_client_surface(id);
                }
            }
        }
    }

    pub fn mac_client_hidden(&self, id: ClientId) -> bool {
        self.mac_hidden.contains(&id) || self.desktop_reveal.as_ref().is_some_and(|(ids, _)| ids.contains(&id))
    }

    pub(super) fn application_key(&self, id: ClientId) -> String {
        let Some(client) = self.clients.get(id) else {
            return String::new();
        };
        // Query the backend so a late app_id/WM_CLASS update is reflected.
        let class = self
            .backend
            .window_class(client.window)
            .map(|c| c.class)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| client.class.clone());
        if class.is_empty() {
            format!("anonymous:{id:?}")
        } else {
            class.to_ascii_lowercase()
        }
    }

    fn application_members(&self, id: ClientId) -> Vec<ClientId> {
        let key = self.application_key(id);
        let mut members: HashSet<_> = self
            .clients
            .keys()
            .filter(|&other| self.application_key(other) == key)
            .collect();
        for other in members.clone() {
            members.extend(self.transient_family(other));
        }
        members.into_iter().collect()
    }

    pub fn hide_application(&mut self, others: bool) {
        if self.cycle.as_ref().is_some_and(|c| c.applications) {
            self.cycle_end(true);
        }
        let Some(focused) = self.focused else {
            return;
        };
        let app = self.application_members(focused);
        let ids: Vec<_> = self.clients.keys().filter(|id| app.contains(id) != others).collect();
        self.mac_hidden.extend(ids.iter().copied());
        for id in ids {
            self.hide_client_surface(id);
        }
        if !others {
            self.focus_successor_of(focused);
        }
        self.bump_protocol_state_revision();
    }

    /// Ask every application window to close. Never escalate an unsupported
    /// close protocol into termination; applications retain their save prompts.
    pub fn quit_application(&mut self) {
        if self.cycle.as_ref().is_some_and(|c| c.applications) {
            self.cycle_end(true);
        }
        let Some(id) = self.focused else {
            return;
        };
        self.reveal_application(id);
        let roots: Vec<_> = self
            .application_members(id)
            .into_iter()
            .filter(|&member| self.clients.get(member).is_some_and(|c| c.parent.is_none()))
            .collect();
        for id in roots {
            let window = self.clients[id].window;
            if self.backend.supports_protocol(window, crate::WmProtocol::DeleteWindow) {
                let target = self.modal_blocker(id).unwrap_or(id);
                self.backend.send_quit(self.clients[target].window);
            }
        }
    }

    pub fn same_application(&self, first: ClientId, second: ClientId) -> bool {
        self.application_key(first) == self.application_key(second)
    }

    /// A stable snapshot for the application chooser, including hidden apps.
    pub fn running_applications(&self) -> Vec<(ClientId, String)> {
        let mut seen = HashSet::new();
        let mut apps: Vec<_> = self
            .clients
            .keys()
            .filter_map(|id| {
                let key = self.application_key(id);
                seen.insert(key.clone()).then_some((id, key))
            })
            .collect();
        apps.sort_by(|a, b| a.1.cmp(&b.1));
        apps
    }

    /// Explicitly confirmed force quit. A stale chooser target is a no-op;
    /// client handles carry generations, so it cannot target a replacement app.
    pub fn force_quit_application(&mut self, id: ClientId) {
        if self.clients.get(id).is_none() {
            return;
        }
        for member in self.application_members(id) {
            self.kill_client(member);
        }
    }

    pub(super) fn mac_enter_fullscreen(&mut self, id: ClientId) {
        if !self.mac_mode() || self.mac_fullscreen.contains_key(&id) || self.workspace_count >= MAX_WORKSPACES {
            return;
        }
        let origin = self.clients[id].workspace;
        if self.separate_spaces() { self.select_output(self.client_output_index(id)); }
        let Some(space) = self.create_workspace() else { return; };
        // A borrowed desktop's fullscreen child belongs to the same home
        // display, even though it is temporarily shown on another output.
        if let Some(state) = self.display_spaces.as_mut().filter(|_| self.interaction.separate_spaces) {
            state.snapshot.spaces[space].home_display = state.snapshot.spaces[origin].home_display.clone();
        }
        self.mac_fullscreen.insert(id, (origin, space));
        for member in self.transient_family(id) {
            self.move_one_client_to_workspace(member, space);
        }
        self.switch_workspace(space);
        self.focus_client(id);
    }

    pub(super) fn mac_leave_fullscreen(&mut self, id: ClientId) {
        let Some((origin, space)) = self.mac_fullscreen.remove(&id) else {
            return;
        };
        let was_selected = self.current_workspace == space;
        let selected_output = self.active_output_index();
        let previous_focus = self.focused;
        let was_current = self.workspace_visible(space);
        for member in self.transient_family(id) {
            self.move_one_client_to_workspace(member, origin);
        }
        if was_current {
            self.switch_workspace(origin);
        }
        // Other windows explicitly moved into a fullscreen Space are retained.
        if !self.workspace_has_windows(space) {
            self.remove_workspace(space);
        }
        if was_current && self.is_focusable(id) && was_selected { self.focus_client(id); }
        if !was_selected && self.separate_spaces() {
            self.select_output(selected_output);
            if let Some(previous) = previous_focus.filter(|&other| other != id && self.is_focusable(other)) { self.focus_client(previous); }
        }
    }

    pub fn miniaturize_application(&mut self) {
        let Some(id) = self.focused else {
            return;
        };
        for member in self.application_members(id) {
            self.miniaturize(member);
        }
    }

    pub fn toggle_show_desktop(&mut self) {
        if let Some((ids, focus)) = self.desktop_reveal.take() {
            for id in ids {
                if self.is_focusable(id) {
                    self.show_client_surface(id);
                }
            }
            if let Some(id) = focus.filter(|&id| self.is_focusable(id)) {
                self.focus_client(id);
            }
        } else {
            let ids: HashSet<_> = self.clients.keys().filter(|&id| self.is_focusable(id)).collect();
            let focus = self.focused;
            self.desktop_reveal = Some((ids.clone(), focus));
            for id in ids {
                self.hide_client_surface(id);
            }
            if let Some(id) = focus {
                self.focus_successor_of(id);
            }
        }
        self.bump_protocol_state_revision();
    }

    pub(super) fn reveal_application(&mut self, id: ClientId) {
        if !self.mac_mode() {
            return;
        }
        if let Some((ids, _)) = self.desktop_reveal.as_mut() {
            ids.remove(&id);
        }
        let members = self.application_members(id);
        for member in &members {
            self.mac_hidden.remove(member);
        }
        if let Some(client) = self.clients.get(id) {
            self.switch_workspace(client.workspace);
        }
        for member in members {
            if self.is_focusable(member) {
                self.show_client_surface(member);
            }
        }
        // A selected minimized window needs a live surface before focus.
        if self
            .clients
            .get(id)
            .is_some_and(|c| c.lifecycle == Lifecycle::Miniaturized)
        {
            self.deminiaturize(id);
        }
    }

    pub fn cycle_applications(&mut self, direction: i32) {
        if self.cycle.as_ref().is_some_and(|cycle| !cycle.applications) {
            self.cycle_end(false);
        }
        if self.cycle.is_none() {
            let mut seen = HashSet::new();
            let order: Vec<_> = self
                .focus_history
                .iter()
                .rev()
                .copied()
                .chain(self.clients.keys())
                .filter(|&id| {
                    self.clients.get(id).is_some_and(|c| {
                        c.lifecycle != Lifecycle::Withdrawn && !c.flags.contains(ClientFlags::NO_FOCUS)
                    })
                })
                .filter(|&id| seen.insert(self.application_key(id)))
                .collect();
            if order.is_empty() {
                return;
            }
            self.backend.grab_keyboard();
            self.cycle = Some(CycleSession {
                order,
                selected: 0,
                modifier: Modifiers::SUPER,
                applications: true,
            });
        }
        let cycle = self.cycle.as_mut().unwrap();
        cycle.selected = (cycle.selected as i32 + direction).rem_euclid(cycle.order.len() as i32) as usize;
        self.notifications.push_back(Notification::CycleUpdated);
    }

    pub fn cycle_application_windows(&mut self, direction: i32) {
        let Some(focused) = self.focused else {
            return;
        };
        let app = self.application_members(focused);
        // Stable creation order prevents repeated cycling from toggling only
        // the two latest windows as MRU would do after every activation.
        let order: Vec<_> = self
            .clients
            .keys()
            .filter(|id| app.contains(id) && self.is_focusable(*id))
            .collect();
        let Some(at) = order.iter().position(|id| *id == focused) else {
            return;
        };
        let next = (at as i32 + direction).rem_euclid(order.len() as i32) as usize;
        self.focus_client(order[next]);
    }
}
