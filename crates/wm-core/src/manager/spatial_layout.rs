//! Workspace policy lives alongside the existing geometry/lifecycle owner.
use super::*;
use crate::spatial::{self, Item, WorkspaceLayout};
use crate::{LayoutMode, WindowPlacement};

impl<B: Backend> WindowManager<B> {
    /// A membership/presentation change invalidates a managed drag's targets
    /// and resize rollback. Finish cancellation before changing that model.
    pub(super) fn cancel_client_layout_interaction(&mut self, id: ClientId) {
        let Some(active) = self.interactive_drag_client() else {
            return;
        };
        if active == id
            || self
                .clients
                .get(id)
                .zip(self.clients.get(active))
                .is_some_and(|(c, a)| {
                    c.workspace == a.workspace
                        && self.workspace_layout(c.workspace) != LayoutMode::Freeform
                })
        {
            self.end_active_drag();
        }
    }

    pub fn layout_statistics(&self) -> crate::LayoutStatistics {
        self.layout_statistics
    }

    pub fn workspace_layout(&self, workspace: usize) -> LayoutMode {
        self.layouts
            .get(workspace)
            .map_or(LayoutMode::Freeform, |l| l.mode)
    }

    pub fn layout_order(&self, workspace: usize) -> &[ClientId] {
        self.layouts
            .get(workspace)
            .map_or(&[], |l| l.order.as_slice())
    }

    pub fn set_workspace_layout(&mut self, workspace: usize, mode: LayoutMode) {
        if workspace >= MAX_WORKSPACES
            || (workspace < self.workspace_count && self.workspace_layout(workspace) == mode)
        {
            return;
        }
        self.end_active_drag();
        if !self.ensure_display_space_slots(workspace + 1) { return; }
        self.workspace_count = self.workspace_count.max(workspace + 1);
        self.layouts
            .resize_with(self.workspace_count, WorkspaceLayout::default);
        self.layouts[workspace].mode = mode;
        if mode == LayoutMode::Freeform {
            let order = self.layouts[workspace].order.clone();
            for id in order {
                self.restore_freeform_view(id);
            }
        } else {
            self.reflow_workspace(workspace);
        }
        self.bump_protocol_state_revision();
        self.backend
            .publish_workspaces(self.workspace_count, self.current_workspace);
        if workspace == self.current_workspace {
            self.notifications
                .push_back(Notification::LayoutChanged(mode));
        }
    }

    pub(super) fn restore_freeform_view(&mut self, id: ClientId) {
        let Some(c) = self.clients.get_mut(id) else {
            return;
        };
        c.layout_excluded = false;
        c.placement.output = None;
        if let Some(saved) = c.placement.freeform.take() {
            self.restore_freeform_geometry(id, saved);
        }
        self.clear_layout_clip(id);
    }

    /// Membership changes update the base of a special presentation's restore
    /// chain. Fullscreen over maximize still restores through both states.
    pub(super) fn restore_freeform_geometry(&mut self, id: ClientId, saved: Rect) {
        let c = &mut self.clients[id];
        if c.restore_geometry.is_some() {
            c.restore_geometry = Some(saved);
        }
        let maximized = c
            .flags
            .intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V);
        if !maximized {
            if let Some(restore) = self.fullscreen_restore.get_mut(&id) {
                *restore = saved;
            }
        }
        if !maximized && !c.flags.contains(ClientFlags::FULLSCREEN) {
            self.apply_layout_geometry(id, saved, None, false);
        }
    }

    pub fn toggle_workspace_layout(&mut self) {
        let mode = match self.workspace_layout(self.current_workspace) {
            LayoutMode::Mosaic => LayoutMode::Flow,
            LayoutMode::Freeform | LayoutMode::Flow => LayoutMode::Mosaic,
        };
        self.set_workspace_layout(self.current_workspace, mode);
    }

    pub(super) fn layout_candidate(&self, id: ClientId) -> bool {
        self.clients.get(id).is_some_and(|c| {
            self.workspace_layout(c.workspace) != LayoutMode::Freeform
                && !c.placement.floating
                && c.parent.is_none()
                && c.lifecycle == Lifecycle::Normal
                && !c.flags.intersects(
                    ClientFlags::STICKY
                        | ClientFlags::MODAL
                        | ClientFlags::NO_FOCUS
                        | ClientFlags::SHADED
                        | ClientFlags::FULLSCREEN
                        | ClientFlags::MAXIMIZED_H
                        | ClientFlags::MAXIMIZED_V,
                )
        })
    }

    pub fn is_layout_managed(&self, id: ClientId) -> bool {
        self.layout_candidate(id) && !self.clients[id].layout_excluded
    }

    pub fn toggle_floating(&mut self, id: ClientId) {
        let Some(c) = self.clients.get(id) else {
            return;
        };
        // Every window already floats in Freeform. Do not invisibly change
        // its membership in a future style when this shortcut has no effect.
        if self.workspace_layout(c.workspace) == LayoutMode::Freeform {
            return;
        }
        self.set_floating(id, !c.placement.floating);
    }

    pub fn set_floating(&mut self, id: ClientId, floating: bool) {
        let Some(c) = self.clients.get(id) else {
            return;
        };
        if c.placement.floating == floating {
            return;
        }
        if self.interactive_drag_active() {
            self.end_active_drag();
        }
        let c = &mut self.clients[id];
        c.placement.floating = floating;
        let workspace = c.workspace;
        let saved = c.placement.freeform;
        if floating {
            if let Some(saved) = saved {
                self.restore_freeform_geometry(id, saved);
            }
            self.clear_layout_clip(id);
        } else {
            self.unshade(id);
        }
        self.reflow_workspace(workspace);
        self.bump_protocol_state_revision();
    }

    /// Restored indices may arrive out of order. The shell supplies the saved
    /// ordering after it matches an application; no stale runtime IDs persist.
    pub fn restore_window_placement(
        &mut self,
        id: ClientId,
        placement: WindowPlacement,
        index: usize,
    ) {
        let Some(c) = self.clients.get_mut(id) else {
            return;
        };
        c.placement = placement;
        c.layout_restore_order = Some(index);
        let workspace = c.workspace;
        self.layouts
            .resize_with(self.workspace_count, WorkspaceLayout::default);
        let order = &mut self.layouts[workspace].order;
        order.retain(|&other| other != id);
        order.push(id);
        order.sort_by_key(|&id| self.clients[id].layout_restore_order.unwrap_or(usize::MAX));
        self.reflow_workspace(workspace);
        self.bump_protocol_state_revision();
    }

    pub(super) fn register_layout_client(&mut self, id: ClientId) {
        let workspace = self.clients[id].workspace;
        self.layouts
            .resize_with(self.workspace_count, WorkspaceLayout::default);
        let order = &mut self.layouts[workspace].order;
        if order.contains(&id) {
            return;
        }
        let index = self
            .focused
            .and_then(|focused| order.iter().position(|&c| c == focused))
            .map_or(order.len(), |i| i + 1);
        order.insert(index, id);
    }

    pub(super) fn layout_output_key(monitor: &MonitorInfo) -> &str {
        monitor.identity.as_deref().unwrap_or(&monitor.name)
    }

    /// Flow positions may be outside every output: never infer ownership from
    /// its current frame center once an output affinity has been recorded.
    pub fn client_output_index(&self, id: ClientId) -> usize {
        if let Some(output) = self.clients.get(id).and_then(|c| self.workspace_output_index(c.workspace)) { return output; }
        let Some(c) = self.clients.get(id) else {
            return self.primary_monitor_index();
        };
        if self.workspace_layout(c.workspace) != LayoutMode::Freeform
            && !c.placement.floating
            && !c.flags.contains(ClientFlags::STICKY)
        {
            if let Some(key) = c.placement.output.as_deref() {
                return self
                    .backend
                    .monitors_ref()
                    .iter()
                    .position(|m| Self::layout_output_key(m) == key)
                    .unwrap_or_else(|| self.primary_monitor_index());
            }
        }
        self.monitor_index_at(self.client_frame_center(id).unwrap_or(c.geometry.pos))
    }

    pub(super) fn client_decoration_scale(&self, id: ClientId) -> f32 {
        let c = &self.clients[id];
        let rect = if self.workspace_layout(c.workspace) != LayoutMode::Freeform
            && !c.placement.floating
        {
            self.backend
                .monitors_ref()
                .get(self.client_output_index(id))
                .map_or(client_frame_rect(c), |m| m.geometry)
        } else {
            client_frame_rect(c)
        };
        self.backend.decoration_scale(rect)
    }

    pub fn move_client_to_output(&mut self, id: ClientId, index: usize) -> bool {
        if !self.clients.contains_key(id) { return false; }
        self.cancel_client_layout_interaction(id);
        if self.monitors_ref().get(index).is_none() { return false; }
        if self.separate_spaces() && self.clients.get(id).is_some_and(|c| c.flags.contains(ClientFlags::FULLSCREEN)) { self.unfullscreen(id); }
        if self.separate_spaces() { self.translate_space_move(id, self.regular_workspace_on_output(index)); }
        self.move_family_to_display(id, index);
        self.place_client_on_output(id, index)
    }

    fn place_client_on_output(&mut self, id: ClientId, index: usize) -> bool {
        let Some(monitor) = self.backend.monitors_ref().get(index) else {
            return false;
        };
        let key = Self::layout_output_key(monitor).to_string();
        let area = self
            .workareas
            .get(index)
            .copied()
            .unwrap_or(monitor.geometry);
        let Some(c) = self.clients.get_mut(id) else {
            return false;
        };
        c.placement.output = Some(key);
        let workspace = c.workspace;
        let fullscreen = c.flags.contains(ClientFlags::FULLSCREEN);
        let mut maximized = MaximizeDirections::empty();
        maximized.set(
            MaximizeDirections::HORIZONTAL,
            c.flags.contains(ClientFlags::MAXIMIZED_H),
        );
        maximized.set(
            MaximizeDirections::VERTICAL,
            c.flags.contains(ClientFlags::MAXIMIZED_V),
        );
        if self.workspace_layout(workspace) == LayoutMode::Freeform
            || self.clients[id].placement.floating
        {
            let c = &self.clients[id];
            let mut rect = c.geometry;
            rect.pos = Point::new(
                area.pos.x + c.layout.client_offset.x - c.layout.input_margin as i32,
                area.pos.y + c.layout.client_offset.y - c.layout.input_margin as i32,
            );
            self.set_client_content_geometry(id, rect);
        } else {
            if fullscreen {
                self.reflow_frame(id);
            } else if !maximized.is_empty() {
                self.fit_maximized(id, maximized);
            }
            self.reflow_workspace(workspace);
        }
        self.bump_protocol_state_revision();
        true
    }

    pub(super) fn remember_freeform(&mut self, id: ClientId) {
        let index = self.client_output_index(id);
        let output = self
            .backend
            .monitors_ref()
            .get(index)
            .map(|m| Self::layout_output_key(m).to_string());
        let c = &mut self.clients[id];
        if c.placement.freeform.is_none() {
            c.placement.freeform = Some(
                c.restore_geometry
                    .or_else(|| self.fullscreen_restore.get(&id).copied())
                    .unwrap_or(c.geometry),
            );
        }
        if c.placement.output.is_none() {
            c.placement.output = output;
        }
    }

    pub(super) fn clear_layout_clip(&mut self, id: ClientId) {
        if let Some(c) = self.clients.get(id) {
            let rect = client_frame_rect(c);
            self.backend
                .present_layout(c.window, c.frame, rect, rect, None, false);
        }
    }

    fn reachable_freeform(&self, id: ClientId, mut geometry: Rect) -> Rect {
        let Some(c) = self.clients.get(id) else {
            return geometry;
        };
        let pos = Point::new(
            geometry.pos.x.saturating_sub(c.layout.client_offset.x).saturating_add(c.layout.input_margin as i32),
            geometry.pos.y.saturating_sub(c.layout.client_offset.y).saturating_add(c.layout.input_margin as i32),
        );
        let size = Size::new(
            geometry
                .size
                .w
                .saturating_add(c.layout.visual_bounds().size.w.saturating_sub(c.geometry.size.w)),
            geometry
                .size
                .h
                .saturating_add(c.layout.visual_bounds().size.h.saturating_sub(c.geometry.size.h)),
        );
        let reachable = self.backend.monitors_ref().iter().any(|m| {
            pos.x as i64 + size.w as i64 > m.geometry.pos.x as i64 + 32
                && (pos.x as i64) < m.geometry.pos.x as i64 + m.geometry.size.w as i64 - 32
                && pos.y >= m.geometry.pos.y
                && (pos.y as i64) < m.geometry.pos.y as i64 + m.geometry.size.h as i64 - 16
        });
        if !reachable {
            let target = self.usable_area_at(pos);
            let pos = placement::clamp_to(target, size, pos);
            geometry.pos = Point::new(
                pos.x.saturating_add(c.layout.client_offset.x).saturating_sub(c.layout.input_margin as i32),
                pos.y.saturating_add(c.layout.client_offset.y).saturating_sub(c.layout.input_margin as i32),
            );
        }
        geometry
    }

    pub(super) fn apply_layout_geometry(
        &mut self,
        id: ClientId,
        geometry: Rect,
        clip: Option<Rect>,
        force_reflow: bool,
    ) {
        let geometry = if clip.is_none() {
            self.reachable_freeform(id, geometry)
        } else {
            geometry
        };
        let Some(c) = self.clients.get_mut(id) else {
            return;
        };
        let source = client_frame_rect(c);
        // Establish output presentation before staging the configure: Flow
        // may put the final frame outside the owning monitor entirely.
        self.backend
            .present_layout(c.window, c.frame, source, source, clip, false);
        if c.geometry != geometry {
            self.layout_statistics.geometry_changes += 1;
            c.geometry = geometry;
            self.reflow_frame(id);
        } else if force_reflow {
            self.reflow_frame(id);
        }
        let c = &self.clients[id];
        self.backend.present_layout(
            c.window,
            c.frame,
            source,
            client_frame_rect(c),
            clip,
            self.active_resize.is_none(),
        );
    }

    pub(super) fn reflow_client_workspace(&mut self, id: ClientId) {
        if let Some(workspace) = self.clients.get(id).map(|c| c.workspace) {
            self.reflow_workspace(workspace);
        }
    }

    pub(super) fn reflow_layouts(&mut self) {
        for workspace in 0..self.workspace_count {
            self.reflow_workspace(workspace);
        }
    }

    pub(super) fn reflow_workspace(&mut self, workspace: usize) {
        self.reflow_workspace_with_force(workspace, false);
    }

    pub(super) fn reflow_workspace_with_force(&mut self, workspace: usize, force_reflow: bool) {
        let mode = self.workspace_layout(workspace);
        if mode == LayoutMode::Freeform {
            return;
        }
        let started = std::time::Instant::now();
        let order = self.layouts[workspace].order.clone();
        for &id in &order {
            if self.layout_candidate(id) {
                self.remember_freeform(id);
            } else {
                self.clear_layout_clip(id);
            }
        }
        let monitors = self.backend.monitors();
        for (index, monitor) in monitors.iter().enumerate() {
            let area = self
                .workareas
                .get(index)
                .copied()
                .unwrap_or(monitor.geometry);
            let scale = (self.backend.decoration_scale(monitor.geometry) as f64).clamp(0.25, 8.0);
            let logical = Size::new(
                (area.size.w as f64 / scale).floor() as u32,
                (area.size.h as f64 / scale).floor() as u32,
            );
            let ids: Vec<_> = order
                .iter()
                .copied()
                .filter(|&id| self.layout_candidate(id) && self.client_output_index(id) == index)
                .collect();
            let decorations: Vec<_> = ids
                .iter()
                .map(|&id| {
                    let c = &self.clients[id];
                    if c.chrome == ClientChrome::ClientDrawn {
                        frameless_layout(c.geometry.size)
                    } else {
                        self.theme
                            .layout_at(&Self::decoration_request(c, None), scale as f32)
                    }
                })
                .collect();
            let items: Vec<_> = ids
                .iter()
                .zip(&decorations)
                .map(|(&id, layout)| {
                    let c = &self.clients[id];
                    let min = self
                        .backend
                        .size_hints(c.window)
                        .min_size
                        .unwrap_or(Size::new(1, 1));
                    Item {
                        min: Size::new(
                            ((min.w.saturating_add(
                                layout.visual_bounds().size.w.saturating_sub(c.geometry.size.w),
                            )) as f64
                                / scale)
                                .ceil() as u32,
                            ((min.h.saturating_add(
                                layout.visual_bounds().size.h.saturating_sub(c.geometry.size.h),
                            )) as f64
                                / scale)
                                .ceil() as u32,
                        ),
                        weight: c.placement.mosaic_weight,
                        width: c.placement.flow_width,
                    }
                })
                .collect();
            let focused = self
                .focused
                .and_then(|id| ids.iter().position(|&i| i == id));
            let rects = match mode {
                LayoutMode::Mosaic => spatial::mosaic(logical, &items),
                LayoutMode::Flow => {
                    let viewport = self.layouts[workspace]
                        .viewports
                        .entry(Self::layout_output_key(monitor).to_string())
                        .or_default();
                    spatial::flow(logical, &items, focused, viewport)
                }
                LayoutMode::Freeform => unreachable!(),
            };
            for ((id, rect), layout) in ids.into_iter().zip(rects).zip(decorations) {
                self.clients[id].layout_excluded = rect.is_none();
                let Some(rect) = rect else {
                    if let Some(saved) = self.clients[id].placement.freeform {
                        self.apply_layout_geometry(id, saved, None, force_reflow);
                    }
                    continue;
                };
                let c = &self.clients[id];
                // Convert both edges, so fractional-scale rounding cannot
                // create overlapping neighboring frames.
                let edge = |v: i64| (v as f64 * scale).round() as i32;
                let frame = Rect {
                    pos: Point::new(
                        area.pos.x.saturating_add(edge(rect.pos.x as i64)),
                        area.pos.y.saturating_add(edge(rect.pos.y as i64)),
                    ),
                    size: Size::new(
                        (edge(rect.pos.x as i64 + rect.size.w as i64) - edge(rect.pos.x as i64))
                            .max(1) as u32,
                        (edge(rect.pos.y as i64 + rect.size.h as i64) - edge(rect.pos.y as i64))
                            .max(1) as u32,
                    ),
                };
                let candidate = Size::new(
                    frame
                        .size
                        .w
                        .saturating_sub(layout.visual_bounds().size.w.saturating_sub(c.geometry.size.w))
                        .max(1),
                    frame
                        .size
                        .h
                        .saturating_sub(layout.visual_bounds().size.h.saturating_sub(c.geometry.size.h))
                        .max(1),
                );
                let constrained =
                    resize::constrain_size(candidate, self.backend.size_hints(c.window));
                let size = Size::new(
                    constrained.w.min(candidate.w),
                    constrained.h.min(candidate.h),
                );
                let geometry = Rect {
                    pos: Point::new(
                        frame.pos.x + layout.client_offset.x - layout.input_margin as i32,
                        frame.pos.y + layout.client_offset.y - layout.input_margin as i32,
                    ),
                    size,
                };
                if mode == LayoutMode::Flow && c.placement.flow_width == 0 {
                    self.clients[id].placement.flow_width = rect.size.w;
                }
                self.apply_layout_geometry(id, geometry, Some(area), force_reflow);
            }
        }
        self.bump_protocol_state_revision();
        self.layout_statistics.calculations += 1;
        self.layout_statistics.calculation_us += started.elapsed().as_micros();
        self.layout_statistics.managed_windows = order
            .iter()
            .filter(|&&id| self.is_layout_managed(id))
            .count();
    }

    pub(super) fn layout_neighbor(
        &self,
        id: ClientId,
        direction: FocusDirection,
    ) -> Option<ClientId> {
        let c = self.clients.get(id)?;
        if self.workspace_layout(c.workspace) == LayoutMode::Flow
            && matches!(direction, FocusDirection::Up | FocusDirection::Down)
        {
            return None;
        }
        let source = client_frame_rect(c);
        self.layout_order(c.workspace)
            .iter()
            .copied()
            .filter(|&other| {
                other != id
                    && self.is_layout_managed(other)
                    && self.client_output_index(other) == self.client_output_index(id)
            })
            .filter_map(|other| {
                directional_score(source, client_frame_rect(&self.clients[other]), direction)
                    .map(|score| (other, score))
            })
            .min_by_key(|(_, score)| *score)
            .map(|(id, _)| id)
    }

    pub(super) fn layout_neighbor_across_outputs(
        &self,
        id: ClientId,
        direction: FocusDirection,
    ) -> Option<ClientId> {
        let c = &self.clients[id];
        if self.workspace_layout(c.workspace) == LayoutMode::Flow
            && matches!(direction, FocusDirection::Up | FocusDirection::Down)
        {
            return None;
        }
        let output = self.client_output_index(id);
        self.layout_order(c.workspace)
            .iter()
            .copied()
            .filter(|&other| {
                self.is_layout_managed(other) && self.client_output_index(other) != output
            })
            .filter_map(|other| {
                let rect = client_frame_rect(&self.clients[other]);
                let monitor = self
                    .backend
                    .monitors_ref()
                    .get(self.client_output_index(other))?;
                let center = Point::new(
                    rect.pos.x + rect.size.w as i32 / 2,
                    rect.pos.y + rect.size.h as i32 / 2,
                );
                if !monitor.geometry.contains(center) {
                    return None;
                }
                directional_score(client_frame_rect(c), rect, direction).map(|score| (other, score))
            })
            .min_by_key(|(_, score)| *score)
            .map(|(id, _)| id)
    }

    pub fn move_layout_window(&mut self, id: ClientId, direction: FocusDirection) -> bool {
        if !self.is_layout_managed(id) {
            return false;
        }
        if self.interactive_drag_active() {
            self.end_active_drag();
        }
        if self.workspace_layout(self.clients[id].workspace) == LayoutMode::Flow
            && matches!(direction, FocusDirection::Up | FocusDirection::Down)
        {
            return false;
        }
        if let Some(target) = self.layout_neighbor(id, direction) {
            self.reorder_layout_window(id, target);
            return true;
        }
        let output = self.client_output_index(id);
        let Some(source) = self.backend.monitors_ref().get(output).map(|m| m.geometry) else {
            return false;
        };
        let next = self
            .backend
            .monitors_ref()
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != output)
            .filter_map(|(index, m)| {
                directional_score(source, m.geometry, direction).map(|score| (index, score))
            })
            .min_by_key(|(_, score)| *score)
            .map(|(index, _)| index);
        next.is_some_and(|index| self.move_client_to_output(id, index))
    }

    pub(super) fn reorder_layout_window(&mut self, id: ClientId, target: ClientId) {
        let Some(c) = self.clients.get(id) else {
            return;
        };
        let workspace = c.workspace;
        let order = &mut self.layouts[workspace].order;
        let (Some(from), Some(to)) = (
            order.iter().position(|&i| i == id),
            order.iter().position(|&i| i == target),
        ) else {
            return;
        };
        order.remove(from);
        order.insert(to, id);
        self.reflow_workspace(workspace);
    }

    /// Keyboard and pointer resizing share this boundary policy. Mosaic
    /// changes the nearest shared boundary; Flow changes only this width.
    pub fn resize_layout_window(&mut self, id: ClientId, delta: Point) -> bool {
        if self.interactive_drag_active() && self.is_layout_managed(id) {
            self.end_active_drag();
        }
        self.resize_managed(id, delta, None)
    }

    pub(super) fn resize_managed(
        &mut self,
        id: ClientId,
        delta: Point,
        edge: Option<ResizeEdge>,
    ) -> bool {
        if !self.is_layout_managed(id) {
            return false;
        }
        let c = &self.clients[id];
        let workspace = c.workspace;
        let mode = self.workspace_layout(workspace);
        let source = client_frame_rect(c);
        let scale = self
            .backend
            .monitors_ref()
            .get(self.client_output_index(id))
            .map_or(1.0, |m| self.backend.decoration_scale(m.geometry) as f64);
        if mode == LayoutMode::Flow {
            let output = self.client_output_index(id);
            let area = self
                .workareas
                .get(output)
                .copied()
                .or_else(|| self.backend.monitors_ref().get(output).map(|m| m.geometry))
                .unwrap_or(NO_MONITOR_FALLBACK);
            // Start from the visible width, so reversing at a size limit
            // responds immediately instead of undoing invisible overshoot.
            let width = ((source.size.w as i64 + delta.x as i64) as f64 / scale).round();
            let limit = (area.size.w as f64 / scale).floor().clamp(1.0, 65536.0);
            let c = &mut self.clients[id];
            c.placement.flow_width = width.clamp(1.0, limit) as u32;
        } else {
            for (axis, amount, forward, backward) in [
                (0, delta.x, FocusDirection::Right, FocusDirection::Left),
                (1, delta.y, FocusDirection::Down, FocusDirection::Up),
            ] {
                if amount == 0 {
                    continue;
                }
                let output = self.client_output_index(id);
                let area = self
                    .workareas
                    .get(output)
                    .copied()
                    .or_else(|| self.backend.monitors_ref().get(output).map(|m| m.geometry))
                    .unwrap_or(NO_MONITOR_FALLBACK);
                let portrait = area.size.h > area.size.w;
                let neighbor = |direction| {
                    self.layout_order(workspace)
                        .iter()
                        .copied()
                        .filter(|&other| {
                            other != id
                                && self.is_layout_managed(other)
                                && self.client_output_index(other) == output
                        })
                        .filter_map(|other| {
                            let rect = client_frame_rect(&self.clients[other]);
                            // Inside a column (or portrait row), only its own
                            // shared boundary can resize these two cells.
                            if axis == 1 && !portrait && rect.pos.x != source.pos.x {
                                return None;
                            }
                            if axis == 0 && portrait && rect.pos.y != source.pos.y {
                                return None;
                            }
                            directional_score(source, rect, direction).map(|score| (other, score))
                        })
                        .min_by_key(|(_, score)| *score)
                        .map(|(id, _)| id)
                };
                let other = match edge {
                    Some(edge) => {
                        let reverse = if axis == 0 {
                            matches!(
                                edge,
                                ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest
                            )
                        } else {
                            matches!(
                                edge,
                                ResizeEdge::North | ResizeEdge::NorthEast | ResizeEdge::NorthWest
                            )
                        };
                        neighbor(if reverse { backward } else { forward })
                    }
                    None => neighbor(forward).or_else(|| neighbor(backward)),
                };
                let Some(other) = other else {
                    continue;
                };
                let neighbor = client_frame_rect(&self.clients[other]);
                let (a, b) = if axis == 0 {
                    (source.size.w, neighbor.size.w)
                } else {
                    (source.size.h, neighbor.size.h)
                };
                let changed = (a as i64 + amount as i64).clamp(1, a as i64 + b as i64 - 1) as u32;
                // All cells sharing this boundary carry the same width
                // proportion, so resizing a column never tears its edges.
                let ids = self.layouts[workspace].order.clone();
                for &candidate in &ids {
                    if self.is_layout_managed(candidate) {
                        let frame = client_frame_rect(&self.clients[candidate]);
                        self.clients[candidate].placement.mosaic_weight[axis] = if axis == 0 {
                            frame.size.w
                        } else {
                            frame.size.h
                        };
                    }
                }
                for candidate in ids {
                    if !self.is_layout_managed(candidate)
                        || self.client_output_index(candidate) != self.client_output_index(id)
                    {
                        continue;
                    }
                    let rect = client_frame_rect(&self.clients[candidate]);
                    let aligned = if axis == 0 && !portrait {
                        (rect.pos.x == source.pos.x, rect.pos.x == neighbor.pos.x)
                    } else if axis == 1 && portrait {
                        (rect.pos.y == source.pos.y, rect.pos.y == neighbor.pos.y)
                    } else {
                        (candidate == id, candidate == other)
                    };
                    let value = if aligned.0 {
                        Some(changed)
                    } else if aligned.1 {
                        Some(a + b - changed)
                    } else {
                        None
                    };
                    if let Some(value) = value {
                        self.clients[candidate].placement.mosaic_weight[axis] = value;
                    }
                }
            }
        }
        self.reflow_workspace(workspace);
        true
    }

    pub(super) fn preview_managed_move(&mut self, id: ClientId, root: Point) {
        let workspace = self.clients[id].workspace;
        let output = self.monitor_index_at(root);
        let target = self.clients.iter().filter(|(other, c)| *other != id && self.is_layout_managed(*other)
            && (c.workspace == workspace || (self.separate_spaces() && self.workspace_visible(c.workspace)))
            && self.client_output_index(*other) == output)
            .find(|(_, c)| client_frame_rect(c).contains(root)).map(|(id, _)| id);
        let next = target.map(|target| (id, target));
        self.layout_drop = next;
        let c = &self.clients[id];
        let source = client_frame_rect(c);
        let offset = self
            .active_move
            .as_ref()
            .map_or(Point::new(0, 0), |m| m.grab_offset);
        let destination = Rect::new(
            Point::new(root.x - offset.x, root.y - offset.y),
            source.size,
        );
        self.backend.preview_layout_drop(
            Some(crate::LayoutDrag {
                window: c.window,
                frame: c.frame,
                source,
                destination,
            }),
            target.map(|target| client_frame_rect(&self.clients[target])),
        );
    }

    pub(super) fn commit_layout_drop(&mut self) {
        if let Some((id, target)) = self.layout_drop.take() {
            if self.is_layout_managed(id) && self.is_layout_managed(target) {
                let output = self.client_output_index(target);
                if self.client_output_index(id) != output {
                    if self.separate_spaces() { self.translate_space_move(id, self.regular_workspace_on_output(output)); }
                    self.move_family_to_display(id, output);
                    self.place_client_on_output(id, output);
                }
                self.reorder_layout_window(id, target);
            }
        } else if self.separate_spaces() {
            if let Some(id) = self.active_move.as_ref().map(|m| m.client).filter(|&id| self.is_layout_managed(id)) {
                if let Some(at) = self.last_pointer {
                    let output = self.monitor_index_at(at);
                    if self.client_output_index(id) != output {
                        self.translate_space_move(id, self.regular_workspace_on_output(output));
                        self.move_family_to_display(id, output);
                        self.place_client_on_output(id, output);
                    }
                }
            }
        }
        self.backend.preview_layout_drop(None, None);
    }
}
