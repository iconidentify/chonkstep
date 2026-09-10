use super::*;

fn dual_mac() -> WindowManager<FakeBackend> {
    let mut backend = FakeBackend::new();
    backend.set_monitors(dual_monitors());
    let mut wm = wm(backend);
    wm.set_interaction_config(crate::InteractionConfig {
        mode: crate::InteractionMode::Mac,
        ..Default::default()
    });
    wm
}

fn window_on(wm: &mut WindowManager<FakeBackend>, output: usize) -> ClientId {
    let rect = wm.monitors_ref()[output].geometry;
    wm.dispatch(BackendEvent::PointerMotion {
        root: Point::new(rect.pos.x + 300, rect.pos.y + 250),
        surface_local: None,
    });
    let window = wm.backend_mut().create_window();
    wm.dispatch(BackendEvent::MapRequest(window));
    wm.client_for_window(window).unwrap()
}

fn visible(wm: &WindowManager<FakeBackend>, id: ClientId) -> bool {
    let client = wm.client(id).unwrap();
    client.frame.map_or_else(
        || wm.backend().mapped_frameless.contains(&client.window),
        |frame| wm.backend().mapped_frames.contains(&frame),
    )
}

#[test]
fn independent_rows_preserve_other_display_pixels_and_restore_local_focus() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    let right_space = wm.client(right).unwrap().workspace;
    assert_ne!(wm.client(left).unwrap().workspace, right_space);
    assert!(visible(&wm, left) && visible(&wm, right));
    wm.select_output(0);
    let second = wm.create_workspace().unwrap();
    wm.switch_workspace(second);
    assert!(!visible(&wm, left));
    assert!(visible(&wm, right), "switching left must not park right");
    assert_eq!(wm.active_workspace_on_output(1), right_space);
    assert_eq!(
        wm.focused_client(),
        None,
        "empty left Space must not steal a right-hand window"
    );
    assert_eq!(wm.neighboring_workspace(second, 1), None);
    let previous = wm.neighboring_workspace(second, -1).unwrap();
    wm.switch_workspace(previous);
    assert_eq!(wm.focused_client(), Some(left));
    assert!(visible(&wm, left) && visible(&wm, right));
    wm.select_output(1);
    assert_eq!(wm.current_workspace(), right_space);
    assert_eq!(
        wm.focused_client(),
        Some(left),
        "pointer display selection alone does not take keyboard focus"
    );
}

#[test]
fn fullscreen_on_each_display_has_its_own_return_space_and_geometry() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    let left_geometry = wm.client(left).unwrap().geometry;
    let right_geometry = wm.client(right).unwrap().geometry;
    wm.fullscreen(left);
    let full_left = wm.client(left).unwrap().workspace;
    assert_eq!(wm.client(left).unwrap().geometry, LEFT_HEAD);
    assert!(visible(&wm, right));
    wm.fullscreen(right);
    assert_ne!(wm.client(right).unwrap().workspace, full_left);
    assert_eq!(wm.active_workspace_on_output(0), full_left);
    assert_eq!(wm.client(right).unwrap().geometry, RIGHT_HEAD);
    wm.unfullscreen(right);
    assert!(wm.client(left).unwrap().flags.contains(ClientFlags::FULLSCREEN));
    assert_eq!(wm.client(right).unwrap().geometry, right_geometry);
    wm.unfullscreen(left);
    assert_eq!(wm.client(left).unwrap().geometry, left_geometry);
    assert_eq!(wm.workspace_count(), 2);
}

#[test]
fn deleting_a_space_uses_its_own_display_and_keeps_surviving_ids() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    let left_id = wm.workspace_id(wm.client(left).unwrap().workspace);
    let right_id = wm.workspace_id(wm.client(right).unwrap().workspace);
    let added = wm.create_workspace().unwrap();
    wm.move_client_to_workspace(right, added);
    wm.switch_workspace(added);
    assert!(wm.remove_workspace(added));
    assert_eq!(wm.workspace_id(wm.client(right).unwrap().workspace), right_id);
    assert_eq!(wm.workspace_id(wm.client(left).unwrap().workspace), left_id);
    assert!(
        !wm.remove_workspace(wm.client(left).unwrap().workspace),
        "every display retains a desktop"
    );
    assert!(!wm.remove_workspace(wm.client(right).unwrap().workspace));
    assert!(visible(&wm, left) && visible(&wm, right));
}

#[test]
fn moving_across_displays_joins_the_destination_active_space() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    wm.select_output(1);
    let second = wm.create_workspace().unwrap();
    wm.switch_workspace(second);
    let right = window_on(&mut wm, 1);
    let mut geometry = wm.client(left).unwrap().geometry;
    geometry.pos.x += 800;
    wm.set_client_content_geometry(left, geometry);
    assert_eq!(wm.client(left).unwrap().workspace, second);
    assert!(visible(&wm, left) && visible(&wm, right));
    wm.switch_workspace(wm.neighboring_workspace(second, -1).unwrap());
    assert!(!visible(&wm, left) && !visible(&wm, right));
}

#[test]
fn disconnect_reconnect_and_output_reordering_preserve_spaces_and_windows() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    let right_geometry = wm.client(right).unwrap().geometry;
    let right_id = wm.workspace_id(wm.client(right).unwrap().workspace);
    let left_space = wm.client(left).unwrap().workspace;
    wm.backend_mut().set_monitors(vec![dual_monitors()[0].clone()]);
    wm.reconcile_display_spaces();
    assert_eq!(wm.workspace_output_index(wm.client(right).unwrap().workspace), Some(0));
    assert_eq!(wm.workspace_id(wm.client(right).unwrap().workspace), right_id);
    assert_eq!(wm.active_workspace_on_output(0), left_space);
    wm.switch_workspace(wm.client(right).unwrap().workspace);
    assert!(visible(&wm, right));
    assert!(LEFT_HEAD.contains(wm.client(right).unwrap().geometry.pos));
    let mut reordered = dual_monitors();
    reordered.reverse();
    wm.backend_mut().set_monitors(reordered);
    wm.reconcile_display_spaces();
    assert_eq!(wm.workspace_output_index(wm.client(right).unwrap().workspace), Some(0));
    assert_eq!(wm.workspace_output_index(left_space), Some(1));
    assert_eq!(wm.client(right).unwrap().geometry, right_geometry);
    assert_eq!(wm.workspace_id(wm.client(right).unwrap().workspace), right_id);
    assert!(visible(&wm, left) && visible(&wm, right));
}

#[test]
fn linked_display_setting_preserves_the_existing_global_policy() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    let mut config = wm.interaction_config().clone();
    config.separate_spaces = false;
    wm.set_interaction_config(config);
    assert!(!wm.separate_spaces());
    let target = wm.client(right).unwrap().workspace;
    wm.switch_workspace(target);
    assert!(visible(&wm, right));
    assert!(!visible(&wm, left));
    assert_eq!(wm.active_workspace_on_output(0), wm.active_workspace_on_output(1));
}

#[test]
fn losing_the_focused_display_repairs_focus_and_all_disconnected_parks_everything() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let right = window_on(&mut wm, 1);
    wm.backend_mut().set_monitors(vec![dual_monitors()[0].clone()]);
    wm.reconcile_display_spaces();
    assert_eq!(wm.focused_client(), Some(left));
    assert!(!visible(&wm, right));
    wm.backend_mut().set_monitors(Vec::new());
    wm.reconcile_display_spaces();
    assert_eq!(wm.focused_client(), None);
    assert!(!visible(&wm, left) && !visible(&wm, right));
    wm.backend_mut().set_monitors(dual_monitors());
    wm.reconcile_display_spaces();
    assert!(visible(&wm, left) && visible(&wm, right));
}

#[test]
fn moving_a_window_carries_and_repositions_its_dialog_family() {
    let mut wm = dual_mac();
    let parent = window_on(&mut wm, 0);
    let child = window_on(&mut wm, 0);
    wm.clients[child].parent = Some(parent);
    let child_before = wm.clients[child].geometry;
    assert!(wm.move_client_to_output(parent, 1));
    assert_eq!(wm.clients[parent].workspace, wm.clients[child].workspace);
    assert_eq!(wm.clients[child].geometry.pos.x, child_before.pos.x + 800);
    assert!(visible(&wm, parent) && visible(&wm, child));
}

#[test]
fn topology_restore_preserves_empty_rows_and_validates_untrusted_metadata() {
    let mut old = dual_mac();
    old.select_output(1);
    let extra = old.create_workspace().unwrap();
    old.switch_workspace(extra);
    let snapshot = old.display_spaces_snapshot().unwrap().clone();
    let mut fresh = dual_mac();
    assert!(fresh.restore_display_spaces(snapshot.clone()));
    assert_eq!(fresh.display_spaces_snapshot().unwrap(), &snapshot);
    let mut corrupt = snapshot.clone();
    corrupt.spaces[1].id = corrupt.spaces[0].id;
    assert!(!fresh.restore_display_spaces(corrupt));
    let mut corrupt = snapshot.clone();
    corrupt.next_id = u64::MAX;
    assert!(!fresh.restore_display_spaces(corrupt));
    assert_eq!(fresh.display_spaces_snapshot().unwrap(), &snapshot);
    let window = window_on(&mut fresh, 1);
    assert_eq!(fresh.clients[window].workspace, extra);
    assert!(!fresh.restore_display_spaces(snapshot));
}

#[test]
fn fullscreen_restore_reuses_the_saved_space_and_keeps_other_output_active() {
    let mut old = dual_mac();
    let window = window_on(&mut old, 1);
    let geometry = old.clients[window].geometry;
    old.fullscreen(window);
    let origin = old.mac_fullscreen_origin(window).unwrap();
    let full = old.clients[window].workspace;
    let mut fresh = dual_mac();
    assert!(fresh.restore_display_spaces(old.display_spaces_snapshot().unwrap().clone()));
    let window = window_on(&mut fresh, 1);
    fresh.move_client_to_workspace(window, full);
    fresh.set_client_content_geometry(window, geometry);
    assert!(fresh.restore_mac_fullscreen(window, origin));
    assert_eq!(fresh.workspace_count(), old.workspace_count());
    assert_eq!(fresh.active_workspace_on_output(0), 0);
    fresh.unfullscreen(window);
    assert_eq!(fresh.clients[window].workspace, origin);
    assert_eq!(fresh.clients[window].geometry, geometry);
}

#[test]
fn duplicate_edid_changes_and_a_full_row_do_not_leave_a_display_without_a_space() {
    let mut wm = dual_mac();
    let mut monitors = dual_monitors();
    monitors[0].identity = Some("identical-panel".into());
    monitors[1].identity = Some("identical-panel".into());
    wm.backend_mut().set_monitors(monitors.clone());
    wm.reconcile_display_spaces();
    let left_id = wm.workspace_id(wm.active_workspace_on_output(0));
    wm.backend_mut().set_monitors(vec![monitors[0].clone()]);
    wm.reconcile_display_spaces();
    assert_eq!(wm.workspace_id(wm.active_workspace_on_output(0)), left_id);
    wm.backend_mut().set_monitors(monitors);
    wm.reconcile_display_spaces();
    wm.select_output(0);
    while wm.create_workspace().is_some() {}
    let mut monitors = wm.monitors();
    let mut third = monitors[1].clone();
    third.name = "THIRD".into();
    third.geometry.pos.x += 800;
    monitors.push(third);
    wm.backend_mut().set_monitors(monitors);
    wm.reconcile_display_spaces();
    assert_eq!(wm.workspace_count(), MAX_WORKSPACES);
    for output in 0..3 {
        let row = wm.workspace_row_on_output(output);
        assert!(!row.is_empty());
        assert!(row.contains(&wm.active_workspace_on_output(output)));
    }
}

#[test]
fn moving_between_display_layouts_reflows_both_rows_and_preserves_freeform_restore() {
    let mut wm = dual_mac();
    let left = window_on(&mut wm, 0);
    let moving = window_on(&mut wm, 0);
    let original = wm.clients[moving].geometry;
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    wm.set_workspace_layout(1, crate::LayoutMode::Flow);
    let shared = wm.clients[left].geometry;
    assert!(wm.move_client_to_output(moving, 1));
    assert_eq!(wm.clients[moving].workspace, 1);
    assert_ne!(wm.clients[left].geometry, shared, "the old layout fills the vacated cell");
    assert_eq!(wm.client_output_index(moving), 1);
    wm.set_workspace_layout(1, crate::LayoutMode::Freeform);
    assert_eq!(wm.clients[moving].workspace, 1);
    assert_eq!(wm.clients[moving].geometry.pos.x, original.pos.x + 800);
}

#[test]
fn first_displays_after_a_headless_start_initialize_independent_rows() {
    let mut backend = FakeBackend::new();
    backend.set_monitors(Vec::new());
    let mut wm = wm(backend);
    wm.set_interaction_config(crate::InteractionConfig { mode: crate::InteractionMode::Mac, ..Default::default() });
    assert!(!wm.separate_spaces());
    wm.backend_mut().set_monitors(dual_monitors());
    wm.reconcile_display_spaces();
    assert!(wm.separate_spaces());
    assert_eq!(wm.workspace_row_on_output(0), vec![0]);
    assert_eq!(wm.workspace_row_on_output(1), vec![1]);
    assert!(wm.workspace_visible(0) && wm.workspace_visible(1));
}
