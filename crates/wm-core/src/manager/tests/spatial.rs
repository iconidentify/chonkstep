//! Behavior of the three workspace styles through the real WindowManager.
use super::*;

#[test]
fn spatial_extreme_restored_positions_are_rescued_when_leaving_a_layout() {
    for pos in [
        Point::new(i32::MIN, i32::MIN),
        Point::new(i32::MAX, i32::MAX),
    ] {
        for float_first in [false, true] {
            let (mut wm, ids) = spatial_desktop(1);
            let id = ids[0];
            wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
            let mut placement = wm.clients[id].placement.clone();
            placement.freeform = Some(Rect::new(pos, Size::new(500, 400)));
            wm.restore_window_placement(id, placement, 0);
            if float_first {
                wm.toggle_floating(id);
            }
            wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
            let frame = client_frame_rect(&wm.clients[id]);
            assert!(wm
                .backend
                .monitors_ref()
                .iter()
                .any(|m| m.geometry.contains(frame.pos)));
            assert_eq!(wm.clients[id].geometry.size, Size::new(500, 400));
            assert_eq!(wm.focused_client(), Some(id));
        }
    }
}

#[test]
fn spatial_late_size_constraints_reflow_and_recover_without_losing_placement() {
    let (mut wm, ids) = spatial_desktop(3);
    let id = ids[1];
    let window = wm.clients[id].window;
    let base = wm.clients[id].geometry;
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let order = wm.layout_order(0).to_vec();
    wm.backend_mut().set_size_hints(
        window,
        SizeHints {
            min_size: Some(Size::new(1200, 1000)),
            ..Default::default()
        },
    );
    wm.dispatch(BackendEvent::SizeHintsChanged(window));
    assert!(wm.is_layout_managed(id));
    assert!(wm.clients[id].geometry.size.w >= 1200 && wm.clients[id].geometry.size.h >= 1000);
    wm.backend_mut().set_size_hints(
        window,
        SizeHints {
            min_size: Some(Size::new(10_000, 10_000)),
            ..Default::default()
        },
    );
    wm.dispatch(BackendEvent::SizeHintsChanged(window));
    assert!(!wm.is_layout_managed(id));
    assert_eq!(wm.clients[id].geometry, base);
    wm.backend_mut()
        .set_size_hints(window, SizeHints::default());
    wm.dispatch(BackendEvent::SizeHintsChanged(window));
    assert!(wm.is_layout_managed(id));
    assert_eq!(wm.layout_order(0), order);
    wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
    assert_eq!(wm.clients[id].geometry, base);
}

#[test]
fn spatial_a_quick_second_drag_never_shades_or_floats_the_window() {
    for mode in [
        crate::LayoutMode::Freeform,
        crate::LayoutMode::Mosaic,
        crate::LayoutMode::Flow,
    ] {
        let (mut wm, ids) = spatial_desktop(3);
        let id = ids[0];
        wm.set_workspace_layout(0, mode);
        let frame = wm.clients[id].frame.unwrap();
        let local = Point::new(30, 2);
        wm.dispatch(titlebar_press(frame, local, 0, Modifiers::empty()));
        let geometry = client_frame_rect(&wm.clients[id]);
        wm.handle_pointer_motion(Point::new(geometry.pos.x + 200, geometry.pos.y + 100));
        wm.dispatch(BackendEvent::DragCancelled);
        wm.dispatch(titlebar_press(frame, local, 150, Modifiers::empty()));
        assert!(wm.interactive_drag_active());
        assert!(!wm.clients[id].flags.contains(ClientFlags::SHADED));
        assert!(!wm.clients[id].placement.floating);
    }
}

#[test]
fn spatial_maximize_can_change_under_fullscreen_without_losing_the_base_geometry() {
    for mode in [
        crate::LayoutMode::Freeform,
        crate::LayoutMode::Mosaic,
        crate::LayoutMode::Flow,
    ] {
        let (mut wm, ids) = spatial_desktop(3);
        let id = ids[1];
        let base = wm.clients[id].geometry;
        wm.set_workspace_layout(0, mode);
        wm.maximize(id, MaximizeDirections::FULL);
        wm.fullscreen(id);
        let full = wm.clients[id].geometry;
        wm.unmaximize(id);
        assert_eq!(wm.clients[id].geometry, full);
        wm.maximize(id, MaximizeDirections::FULL);
        assert_eq!(wm.clients[id].geometry, full);
        wm.unfullscreen(id);
        assert!(wm.clients[id]
            .flags
            .contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        wm.unmaximize(id);
        wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
        assert_eq!(wm.clients[id].geometry, base);
    }
}

#[test]
fn spatial_generated_lifecycle_sequences_preserve_membership_focus_and_freeform_intent() {
    use crate::LayoutMode::*;
    for seed in 1..=32u64 {
        let (mut wm, ids) = spatial_desktop(4);
        let originals: Vec<_> = ids.iter().map(|&id| wm.clients[id].geometry).collect();
        let mut random = seed;
        for step in 0..120 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let id = ids[(random >> 32) as usize % ids.len()];
            let workspace = wm.clients[id].workspace;
            match (random >> 40) % 12 {
                0 => wm.set_workspace_layout(workspace, Mosaic),
                1 => wm.set_workspace_layout(workspace, Flow),
                2 => wm.set_workspace_layout(workspace, Freeform),
                3 => wm.toggle_floating(id),
                4 => wm.toggle_fullscreen(id),
                5 => wm.toggle_maximize(id, MaximizeDirections::FULL),
                6 => wm.miniaturize(id),
                7 => wm.deminiaturize(id),
                8 => {
                    wm.set_client_pinned(id, !wm.clients[id].flags.contains(ClientFlags::STICKY));
                }
                9 => wm.move_client_to_workspace(id, 1 - workspace),
                10 => {
                    wm.resize_layout_window(id, Point::new((random % 201) as i32 - 100, 0));
                }
                _ => wm.switch_workspace(1 - wm.current_workspace),
            }
            let ordered: Vec<_> = wm
                .layouts
                .iter()
                .flat_map(|l| l.order.iter().copied())
                .collect();
            assert_eq!(ordered.len(), ids.len(), "seed {seed}, step {step}");
            for &id in &ids {
                assert_eq!(ordered.iter().filter(|&&other| other == id).count(), 1);
                assert!(wm.layout_order(wm.clients[id].workspace).contains(&id));
                if let Some(saved) = wm.clients[id].placement.freeform {
                    assert_eq!(
                        saved,
                        originals[ids.iter().position(|&other| other == id).unwrap()],
                        "seed {seed}, step {step}"
                    );
                }
            }
            let flagged: Vec<_> = wm
                .clients
                .iter()
                .filter(|(_, c)| c.flags.contains(ClientFlags::FOCUSED))
                .map(|(id, _)| id)
                .collect();
            assert_eq!(
                flagged,
                wm.focused_client().into_iter().collect::<Vec<_>>(),
                "seed {seed}, step {step}"
            );
            if let Some(focused) = wm.focused_client() {
                assert!(wm.is_focusable(focused), "seed {seed}, step {step}");
            }
        }
        for &id in &ids {
            wm.unfullscreen(id);
            wm.unmaximize(id);
            wm.deminiaturize(id);
            wm.set_client_pinned(id, false);
        }
        for workspace in 0..wm.workspace_count() {
            wm.set_workspace_layout(workspace, Freeform);
        }
        for (&id, original) in ids.iter().zip(originals) {
            assert_eq!(wm.clients[id].geometry, original, "seed {seed}");
        }
    }
}

#[test]
fn spatial_pinning_an_offscreen_flow_window_makes_it_reachable_and_rejoining_is_reversible() {
    let (mut wm, ids) = spatial_desktop(8);
    let id = ids[0];
    let original = wm.clients[id].geometry;
    let focus = wm.focused_client();
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    assert!(wm.clients[id].geometry.pos.x < 0);
    let order = wm.layout_order(0).to_vec();
    assert!(wm.set_client_pinned(id, true));
    assert_eq!(wm.clients[id].geometry, original);
    assert_eq!(wm.focused_client(), focus);
    assert!(wm.set_client_pinned(id, false));
    assert!(wm.is_layout_managed(id));
    assert_eq!(wm.layout_order(0), order);
    wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
    assert_eq!(wm.clients[id].geometry, original);
}

#[test]
fn spatial_moving_a_special_window_to_an_output_refits_its_presentation() {
    for fullscreen in [false, true] {
        let (mut wm, ids) = spatial_desktop(2);
        wm.backend_mut().set_monitors(dual_monitors());
        wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
        let id = ids[0];
        if fullscreen {
            wm.fullscreen(id);
        } else {
            wm.maximize(id, MaximizeDirections::FULL);
        }
        wm.move_client_to_output(id, 1);
        assert_eq!(wm.client_output_index(id), 1);
        assert_eq!(client_frame_rect(&wm.clients[id]), RIGHT_HEAD);
    }
}

#[test]
fn spatial_floating_a_special_window_preserves_its_presentation_and_restore_chain() {
    for fullscreen in [false, true] {
        let (mut wm, ids) = spatial_desktop(3);
        let id = ids[1];
        let original = wm.clients[id].geometry;
        wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
        if fullscreen {
            wm.fullscreen(id);
        } else {
            wm.maximize(id, MaximizeDirections::FULL);
        }
        let special = wm.clients[id].geometry;
        wm.toggle_floating(id);
        assert_eq!(
            wm.clients[id].geometry, special,
            "membership must not undo presentation"
        );
        if fullscreen {
            wm.unfullscreen(id);
        } else {
            wm.unmaximize(id);
        }
        assert_eq!(
            wm.clients[id].geometry, original,
            "floating restores the freeform view"
        );
        wm.toggle_floating(id);
        assert!(wm.is_layout_managed(id));
    }
}

#[test]
fn spatial_lifecycle_changes_cancel_a_drag_before_mutating_membership() {
    for operation in 0..5 {
        let (mut wm, ids) = spatial_desktop(3);
        let id = ids[0];
        wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
        wm.focus_client(id);
        wm.handle_pointer_motion(wm.clients[id].geometry.pos);
        wm.handle_move_request(wm.clients[id].window);
        let target = client_frame_rect(&wm.clients[ids[1]]);
        wm.preview_managed_move(id, target.pos);
        assert!(wm.interactive_drag_active());
        match operation {
            0 => wm.move_client_to_workspace(id, 1),
            1 => wm.miniaturize(id),
            2 => wm.fullscreen(id),
            3 => wm.maximize(id, MaximizeDirections::FULL),
            _ => wm.shade(id),
        }
        assert!(
            !wm.interactive_drag_active(),
            "operation {operation} left a grab"
        );
        assert!(wm.layout_drop.is_none());
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0);
    }
}

fn spatial_desktop(n: usize) -> (WindowManager<FakeBackend>, Vec<ClientId>) {
    let mut wm = wm(FakeBackend::new());
    let mut ids = Vec::new();
    for i in 0..n {
        let window = wm.backend_mut().create_window();
        wm.backend_mut().set_geometry(
            window,
            Rect::new(
                Point::new(70 + i as i32 * 50, 90 + i as i32 * 30),
                Size::new(300 + i as u32 * 20, 250),
            ),
        );
        wm.dispatch(BackendEvent::MapRequest(window));
        ids.push(wm.client_for_window(window).unwrap());
    }
    (wm, ids)
}

#[test]
fn spatial_modes_preserve_freeform_and_focus_through_floating_edits() {
    use crate::LayoutMode::*;
    for intermediate in [Mosaic, Flow] {
        let (mut wm, ids) = spatial_desktop(3);
        let saved: Vec<_> = ids.iter().map(|id| wm.clients[*id].geometry).collect();
        let focus = wm.focused_client();
        let order = wm.layout_order(0).to_vec();
        wm.set_workspace_layout(0, Mosaic);
        assert!(ids.iter().all(|&id| wm.is_layout_managed(id)));
        wm.toggle_floating(ids[1]);
        assert_eq!(wm.clients[ids[1]].geometry, saved[1]);
        wm.set_client_content_geometry(
            ids[1],
            Rect::new(Point::new(350, 200), Size::new(400, 350)),
        );
        wm.toggle_floating(ids[1]);
        assert_eq!(wm.layout_order(0), order);
        wm.set_workspace_layout(0, intermediate);
        wm.set_workspace_layout(0, Freeform);
        for (&id, geometry) in ids.iter().zip(saved) {
            assert_eq!(wm.clients[id].geometry, geometry);
        }
        assert_eq!(wm.focused_client(), focus);
    }
}

#[test]
fn spatial_minimize_restore_removal_and_reordering_keep_membership_honest() {
    use crate::LayoutMode::*;
    let (mut wm, ids) = spatial_desktop(4);
    wm.set_workspace_layout(0, Mosaic);
    let order = wm.layout_order(0).to_vec();
    wm.miniaturize(ids[1]);
    assert_eq!(wm.layout_order(0), order);
    assert!(!wm.is_layout_managed(ids[1]));
    wm.deminiaturize(ids[1]);
    assert_eq!(wm.layout_order(0), order);
    assert_eq!(wm.focused_client(), Some(ids[1]));
    wm.set_workspace_layout(0, Flow);
    assert!(wm.move_layout_window(ids[1], FocusDirection::Right));
    assert_eq!(wm.focused_client(), Some(ids[1]));
    wm.resize_layout_window(ids[1], Point::new(100, 0));
    assert_eq!(wm.focused_client(), Some(ids[1]));
    let window = wm.clients[ids[2]].window;
    wm.dispatch(BackendEvent::Destroyed(window));
    assert!(!wm.layout_order(0).contains(&ids[2]));
    assert_eq!(wm.layout_order(0).len(), 3);
    wm.set_workspace_layout(0, Freeform);
}

#[test]
fn spatial_focus_is_visual_and_flow_vertical_navigation_is_quiet() {
    use crate::LayoutMode::*;
    let (mut wm, ids) = spatial_desktop(3);
    wm.set_workspace_layout(0, Mosaic);
    wm.focus_client(ids[0]);
    assert!(wm.focus_direction(FocusDirection::Right));
    let right = wm.focused_client().unwrap();
    assert!(wm.clients[right].geometry.pos.x > wm.clients[ids[0]].geometry.pos.x);
    wm.set_workspace_layout(0, Flow);
    wm.focus_client(ids[1]);
    assert!(!wm.focus_direction(FocusDirection::Down));
    assert_eq!(wm.focused_client(), Some(ids[1]));
    assert!(wm.focus_direction(FocusDirection::Right));
    assert_eq!(wm.focused_client(), Some(ids[2]));
    let c = &wm.clients[ids[2]];
    assert!(
        c.geometry.pos.x >= 0
            && c.geometry.pos.x as u32 + c.geometry.size.w <= wm.backend().screen_size().w
    );
}

#[test]
fn spatial_flow_resize_reverses_immediately_at_both_size_limits() {
    let (mut wm, ids) = spatial_desktop(2);
    let id = ids[1];
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    wm.resize_layout_window(id, Point::new(100_000, 0));
    let widest = wm.clients[id].geometry.size.w;
    wm.resize_layout_window(id, Point::new(-50, 0));
    assert!(wm.clients[id].geometry.size.w < widest);
    wm.resize_layout_window(id, Point::new(-100_000, 0));
    let narrowest = wm.clients[id].geometry.size.w;
    wm.resize_layout_window(id, Point::new(50, 0));
    assert!(wm.clients[id].geometry.size.w > narrowest);
    assert_eq!(wm.focused_client(), Some(id));
}

#[test]
fn spatial_special_presentations_restore_layout_then_original_freeform() {
    use crate::LayoutMode::*;
    let (mut wm, ids) = spatial_desktop(3);
    let original = wm.clients[ids[1]].geometry;
    wm.set_workspace_layout(0, Mosaic);
    let tiled = wm.clients[ids[1]].geometry;
    wm.maximize(ids[1], MaximizeDirections::FULL);
    assert!(!wm.is_layout_managed(ids[1]));
    wm.unmaximize(ids[1]);
    assert_eq!(wm.clients[ids[1]].geometry, tiled);
    wm.fullscreen(ids[1]);
    assert!(!wm.is_layout_managed(ids[1]));
    wm.unfullscreen(ids[1]);
    assert_eq!(wm.clients[ids[1]].geometry, tiled);
    wm.shade(ids[1]);
    assert!(wm.clients[ids[1]].placement.floating);
    wm.toggle_floating(ids[1]);
    assert!(!wm.clients[ids[1]].flags.contains(ClientFlags::SHADED));
    wm.set_workspace_layout(0, Freeform);
    assert_eq!(wm.clients[ids[1]].geometry, original);
}

#[test]
fn spatial_workspace_deletion_merges_order_without_losing_freeform() {
    use crate::LayoutMode::*;
    let (mut wm, ids) = spatial_desktop(3);
    let original = wm.clients[ids[1]].geometry;
    wm.set_workspace_layout(0, Mosaic);
    wm.move_client_to_workspace(ids[1], 1);
    assert_eq!(wm.clients[ids[1]].geometry, original);
    wm.set_workspace_layout(1, Flow);
    wm.switch_workspace(1);
    wm.remove_workspace(0);
    assert_eq!(wm.workspace_layout(0), Flow);
    assert_eq!(wm.layout_order(0).len(), 3);
    wm.set_workspace_layout(0, Freeform);
    assert_eq!(wm.clients[ids[1]].geometry, original);
}

#[test]
fn spatial_client_resize_requests_cannot_destroy_managed_or_saved_geometry() {
    let (mut wm, ids) = spatial_desktop(3);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let id = ids[0];
    let before = wm.clients[id].geometry;
    let saved = wm.clients[id].placement.freeform;
    wm.handle_configure_request(
        wm.clients[id].window,
        Rect::new(Point::new(0, 0), Size::new(10, 10)),
    );
    assert_eq!(wm.clients[id].geometry, before);
    assert_eq!(wm.clients[id].placement.freeform, saved);
}

#[test]
fn spatial_restore_order_does_not_depend_on_application_startup_order() {
    let (mut wm, ids) = spatial_desktop(4);
    for (id, rank) in ids.iter().zip([3, 1, 2, 0]) {
        wm.restore_window_placement(*id, crate::WindowPlacement::default(), rank);
    }
    assert_eq!(wm.layout_order(0), &[ids[3], ids[1], ids[2], ids[0]]);
}

#[test]
fn spatial_pointer_reorder_commits_only_on_release_and_resize_cancels_exactly() {
    for mode in [crate::LayoutMode::Mosaic, crate::LayoutMode::Flow] {
        let (mut wm, ids) = spatial_desktop(3);
        wm.set_workspace_layout(0, mode);
        wm.focus_client(ids[0]);
        let original = wm.layout_order(0).to_vec();
        let window = wm.clients[ids[0]].window;
        let target = client_frame_rect(&wm.clients[ids[1]]);
        let center = Point::new(target.pos.x + target.size.w as i32 / 2, target.pos.y + 20);
        wm.handle_pointer_motion(wm.clients[ids[0]].geometry.pos);
        wm.handle_move_request(window);
        assert!(wm.interactive_drag_active());
        wm.preview_managed_move(ids[0], center);
        assert_eq!(wm.layout_order(0), original);
        wm.dispatch(BackendEvent::DragCancelled);
        assert_eq!(wm.layout_order(0), original);
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0);
        wm.handle_pointer_motion(wm.clients[ids[0]].geometry.pos);
        wm.handle_move_request(window);
        assert!(wm.interactive_drag_active());
        wm.preview_managed_move(ids[0], center);
        wm.dispatch(BackendEvent::DragEnded);
        assert_ne!(wm.layout_order(0), original);
        assert_eq!(wm.focused_client(), Some(ids[0]));
        let before: Vec<_> = ids.iter().map(|&id| wm.clients[id].geometry).collect();
        let frame = client_frame_rect(&wm.clients[ids[0]]);
        let west = mode == crate::LayoutMode::Mosaic && frame.pos.x > 0;
        wm.handle_resize_request(
            window,
            if west {
                ResizeEdge::West
            } else {
                ResizeEdge::East
            },
        );
        wm.handle_resize_motion(Point::new(
            if west {
                frame.pos.x - 80
            } else {
                frame.pos.x + frame.size.w as i32 + 80
            },
            frame.pos.y + 50,
        ));
        assert_ne!(wm.clients[ids[0]].geometry, before[0]);
        wm.dispatch(BackendEvent::DragCancelled);
        wm.dispatch(BackendEvent::DragCancelled);
        for (&id, rect) in ids.iter().zip(&before) {
            assert_eq!(&wm.clients[id].geometry, rect);
        }
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0);
        assert_eq!(wm.focused_client(), Some(ids[0]));
        let order = wm.layout_order(0).to_vec();
        wm.handle_pointer_motion(wm.clients[ids[0]].geometry.pos);
        wm.handle_move_request(window);
        assert!(wm.interactive_drag_active());
        wm.preview_managed_move(ids[0], center);
        wm.toggle_floating(ids[0]);
        wm.dispatch(BackendEvent::DragEnded);
        assert_eq!(wm.layout_order(0), order);
        assert!(!wm.interactive_drag_active());
        assert!(wm.clients[ids[0]].placement.floating);
    }
}

#[test]
fn spatial_shared_boundaries_preserve_workarea_and_proportions() {
    let (mut wm, ids) = spatial_desktop(3);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let original = client_frame_rect(&wm.clients[ids[0]]);
    wm.resize_layout_window(ids[0], Point::new(90, 0));
    let left = client_frame_rect(&wm.clients[ids[0]]);
    let right = client_frame_rect(&wm.clients[ids[1]]);
    let bottom = client_frame_rect(&wm.clients[ids[2]]);
    assert!(left.size.w > original.size.w);
    assert_eq!(right.pos.x, bottom.pos.x);
    assert_eq!(right.size.w, bottom.size.w);
    assert!(left.pos.x + left.size.w as i32 <= right.pos.x);
    let adjusted: Vec<_> = ids.iter().map(|&id| wm.clients[id].geometry).collect();
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    for (&id, rect) in ids.iter().zip(&adjusted) {
        assert_eq!(&wm.clients[id].geometry, rect);
    }
    wm.resize_layout_window(ids[0], Point::new(i32::MAX, 0));
    for id in ids {
        assert!(wm.clients[id].geometry.size.w > 0);
    }
}

#[test]
fn spatial_output_affinity_survives_offscreen_flow_hotplug_and_reconnect() {
    let (mut wm, ids) = spatial_desktop(4);
    wm.backend_mut().set_monitors(dual_monitors());
    wm.set_workareas(vec![LEFT_WORKAREA, RIGHT_HEAD]);
    let original = wm.clients[ids[0]].geometry;
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    for &id in &ids {
        assert!(wm.move_client_to_output(id, 1));
    }
    wm.focus_client(ids[3]);
    for &id in &ids {
        assert_eq!(wm.client_output_index(id), 1);
    }
    wm.fullscreen(ids[0]); // named offscreen target still belongs to right
    assert_eq!(wm.clients[ids[0]].geometry, RIGHT_HEAD);
    wm.unfullscreen(ids[0]);
    wm.maximize(ids[0], MaximizeDirections::FULL);
    assert_eq!(client_frame_rect(&wm.clients[ids[0]]), RIGHT_HEAD);
    wm.unmaximize(ids[0]);
    wm.backend_mut()
        .set_monitors(vec![dual_monitors().remove(0)]);
    wm.rescue_clients_from_removed_monitor(RIGHT_HEAD);
    for &id in &ids {
        assert_eq!(wm.client_output_index(id), 0);
    }
    let focused = client_frame_rect(&wm.clients[ids[3]]);
    assert!(focused.pos.x >= 0 && focused.pos.x + focused.size.w as i32 <= 800);
    wm.backend_mut().set_monitors(dual_monitors());
    wm.relayout_all_clients();
    for &id in &ids {
        assert_eq!(wm.client_output_index(id), 1);
    }
    wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
    assert_eq!(wm.clients[ids[0]].geometry, original);
}

#[test]
fn spatial_return_to_freeform_while_fullscreen_over_maximize_keeps_both_restores() {
    let (mut wm, ids) = spatial_desktop(2);
    let id = ids[0];
    let original = wm.clients[id].geometry;
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    wm.maximize(id, MaximizeDirections::FULL);
    let maximized = wm.clients[id].geometry;
    wm.fullscreen(id);
    wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
    wm.unfullscreen(id);
    assert_eq!(wm.clients[id].geometry, maximized);
    wm.unmaximize(id);
    assert_eq!(wm.clients[id].geometry, original);
}

#[test]
fn spatial_late_metadata_reflows_without_overwriting_freeform_intent() {
    for mode in [crate::LayoutMode::Mosaic, crate::LayoutMode::Flow] {
        let (mut wm, ids) = spatial_desktop(3);
        let id = ids[0];
        let window = wm.clients[id].window;
        let original = wm.clients[id].geometry;
        wm.set_workspace_layout(0, mode);
        let frame = client_frame_rect(&wm.clients[id]);
        let focus = wm.focused_client();
        for own_chrome in [true, false] {
            wm.backend_mut()
                .set_client_draws_own_chrome(window, own_chrome);
            wm.refresh_client_chrome(id);
            assert_eq!(client_frame_rect(&wm.clients[id]), frame);
            assert_eq!(wm.focused_client(), focus);
        }
        wm.dispatch(BackendEvent::ModalChanged {
            window,
            modal: true,
        });
        assert!(!wm.is_layout_managed(id));
        assert_eq!(wm.clients[id].geometry, original);
        assert_eq!(wm.layout_statistics().managed_windows, 2);
        let child = wm.clients[ids[1]].window;
        let parent = wm.clients[ids[2]].window;
        let saved = wm.clients[ids[1]].placement.freeform.unwrap();
        wm.backend_mut().set_window_parent(child, parent);
        wm.dispatch(BackendEvent::ParentChanged(child));
        assert!(!wm.is_layout_managed(ids[1]));
        assert_eq!(wm.clients[ids[1]].geometry.size, saved.size);
        assert_eq!(wm.layout_statistics().managed_windows, 1);
        wm.set_workspace_layout(0, crate::LayoutMode::Freeform);
        assert_eq!(wm.clients[id].geometry, original);
        assert_eq!(wm.clients[ids[1]].geometry, saved);
    }
}

#[test]
fn spatial_dialog_fixed_size_and_impossible_minimum_are_floating_exceptions() {
    let (mut wm, ids) = spatial_desktop(2);
    let window = wm.clients[ids[0]].window;
    wm.backend_mut().set_size_hints(
        window,
        SizeHints {
            min_size: Some(Size::new(10000, 10000)),
            ..Default::default()
        },
    );
    for kind in [WindowType::Dialog, WindowType::Normal] {
        let window = wm.backend_mut().create_window();
        wm.backend_mut().set_window_type(window, kind);
        wm.backend_mut().set_size_hints(
            window,
            SizeHints {
                min_size: Some(Size::new(120, 80)),
                max_size: Some(Size::new(120, 80)),
                ..Default::default()
            },
        );
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        assert!(wm.clients[id].placement.floating);
    }
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    assert!(!wm.is_layout_managed(ids[0]));
    assert!(wm.is_layout_managed(ids[1]));
    assert_eq!(wm.layout_statistics().managed_windows, 1);
}

#[test]
fn spatial_resize_only_changes_the_boundary_the_pointer_grabbed() {
    let (mut wm, _) = spatial_desktop(8);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let order = wm.layout_order(0).to_vec();
    let middle = order
        .iter()
        .copied()
        .find(|&id| {
            let rect = client_frame_rect(&wm.clients[id]);
            rect.pos.x > 0 && rect.pos.x < 800
        })
        .unwrap();
    let frame = client_frame_rect(&wm.clients[middle]);
    let far_right = *order.last().unwrap();
    let unchanged = wm.clients[far_right].geometry;
    wm.resize_managed(middle, Point::new(50, 0), Some(ResizeEdge::West));
    let resized = client_frame_rect(&wm.clients[middle]);
    assert!(resized.pos.x < frame.pos.x);
    assert_eq!(
        resized.pos.x + resized.size.w as i32,
        frame.pos.x + frame.size.w as i32
    );
    assert_eq!(wm.clients[far_right].geometry, unchanged);
    let top_left = order[0];
    let before: Vec<_> = order.iter().map(|&id| wm.clients[id].geometry).collect();
    wm.resize_managed(top_left, Point::new(0, 50), Some(ResizeEdge::North));
    for (&id, rect) in order.iter().zip(before) {
        assert_eq!(wm.clients[id].geometry, rect);
    }
}

#[test]
fn spatial_mixed_scale_output_moves_preserve_logical_flow_width_and_gaps() {
    let (mut wm, ids) = spatial_desktop(2);
    let mut monitors = dual_monitors();
    monitors[1].geometry.size = Size::new(1600, 1200);
    wm.backend_mut().set_monitors(monitors);
    wm.backend_mut().set_monitor_scales(vec![1.0, 2.0]);
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    let width = client_frame_rect(&wm.clients[ids[0]]).size.w;
    let logical_width = wm.clients[ids[0]].placement.flow_width;
    wm.move_client_to_output(ids[0], 1);
    wm.move_client_to_output(ids[1], 1);
    assert_eq!(client_frame_rect(&wm.clients[ids[0]]).size.w, width * 2);
    assert_eq!(wm.clients[ids[0]].placement.flow_width, logical_width);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let a = client_frame_rect(&wm.clients[ids[0]]);
    let b = client_frame_rect(&wm.clients[ids[1]]);
    assert_eq!(b.pos.x - a.pos.x - a.size.w as i32, 12);
    assert_eq!(b.pos.x + b.size.w as i32, 2400);
    assert_eq!(a.size.h, 1200);
}

#[test]
fn spatial_deleting_into_freeform_restores_geometry_and_empty_desktops_restore() {
    let (mut wm, ids) = spatial_desktop(3);
    let original: Vec<_> = ids.iter().map(|&id| wm.clients[id].geometry).collect();
    wm.set_workspace_layout(2, crate::LayoutMode::Freeform);
    assert_eq!(wm.workspace_count(), 3);
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    assert!(wm.remove_workspace(0));
    assert_eq!(wm.workspace_layout(0), crate::LayoutMode::Freeform);
    for (&id, rect) in ids.iter().zip(original) {
        assert_eq!(wm.clients[id].geometry, rect);
        assert!(wm.clients[id].placement.freeform.is_none());
    }
    assert_eq!(wm.layout_order(0).len(), 3);
}

#[test]
fn spatial_keyboard_moves_to_an_empty_neighbor_output_and_focus_returns_spatially() {
    let (mut wm, ids) = spatial_desktop(2);
    wm.backend_mut().set_monitors(dual_monitors());
    wm.toggle_floating(ids[0]);
    assert!(
        !wm.clients[ids[0]].placement.floating,
        "Freeform toggle must not change future membership invisibly"
    );
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    wm.focus_client(ids[1]);
    assert!(wm.move_layout_window(ids[1], FocusDirection::Right));
    assert_eq!(wm.client_output_index(ids[1]), 1);
    assert_eq!(wm.focused_client(), Some(ids[1]));
    assert!(wm.focus_direction(FocusDirection::Left));
    assert_eq!(wm.focused_client(), Some(ids[0]));
    wm.set_workspace_layout(0, crate::LayoutMode::Flow);
    assert!(!wm.move_layout_window(ids[0], FocusDirection::Up));
    assert_eq!(wm.client_output_index(ids[0]), 0);
}
