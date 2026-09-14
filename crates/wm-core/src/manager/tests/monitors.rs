//! Monitor focus and the workspace selectors Omarchy binds beside it:
//! `focusmonitor`, `workspace previous` and the `e+1` / `e-1` stepping
//! that only visits workspaces with windows on them.
use super::*;

fn dual_desktop() -> WindowManager<FakeBackend> {
    let mut backend = FakeBackend::new();
    backend.set_monitors(dual_monitors());
    wm(backend)
}

/// Maps a window with the pointer on `output`, which is where the
/// shared desktop places it.
fn map_on(wm: &mut WindowManager<FakeBackend>, output: usize) -> ClientId {
    let rect = wm.monitors_ref()[output].geometry;
    wm.dispatch(BackendEvent::PointerMotion {
        root: Point::new(rect.pos.x + 300, rect.pos.y + 250),
        surface_local: None,
    });
    let window = wm.backend_mut().create_window();
    wm.dispatch(BackendEvent::MapRequest(window));
    wm.client_for_window(window).unwrap()
}

fn center(rect: Rect) -> Point {
    Point::new(rect.pos.x + (rect.size.w / 2) as i32, rect.pos.y + (rect.size.h / 2) as i32)
}

#[test]
fn focus_output_warps_to_the_target_workarea_and_focuses_its_most_recent_client() {
    let mut wm = dual_desktop();
    // A dock strip carved off the left head: the warp lands in the
    // workarea, not behind the dock.
    wm.set_workareas(vec![LEFT_WORKAREA]);
    let left = map_on(&mut wm, 0);
    let right_old = map_on(&mut wm, 1);
    let right = map_on(&mut wm, 1);
    wm.focus_client_without_raising(right_old);
    wm.focus_client_without_raising(left);
    assert_eq!(wm.focused_client(), Some(left));

    assert!(wm.focus_output(1));
    assert_eq!(wm.backend().warped_pointers.last().copied(), Some(center(RIGHT_HEAD)));
    assert_eq!(wm.focused_client(), Some(right_old), "the most recently focused window on that output");

    assert!(wm.focus_output(0));
    assert_eq!(wm.backend().warped_pointers.last().copied(), Some(center(LEFT_WORKAREA)));
    assert_eq!(wm.focused_client(), Some(left));
    let _ = right;

    // An output that has gone since the verb was read is refused, and
    // nothing moves.
    let warps = wm.backend().warped_pointers.len();
    assert!(!wm.focus_output(2));
    assert_eq!(wm.backend().warped_pointers.len(), warps);
}

#[test]
fn focus_output_leaves_the_keyboard_alone_when_the_output_has_no_window() {
    let mut wm = dual_desktop();
    let left = map_on(&mut wm, 0);
    assert_eq!(wm.focused_client(), Some(left));
    assert!(wm.focus_output(1));
    assert_eq!(wm.focused_client(), Some(left));
    assert_eq!(wm.backend().warped_pointers.last().copied(), Some(center(RIGHT_HEAD)));
}

/// The screensaver's whole method: focus a monitor, open a window, and
/// expect it there. On the shared desktop placement follows the pointer,
/// so the warp is what moves it.
#[test]
fn a_window_mapped_after_focus_output_is_placed_on_that_output() {
    let mut wm = dual_desktop();
    let first = map_on(&mut wm, 0);
    assert!(LEFT_HEAD.contains(wm.client(first).unwrap().geometry.pos));

    assert!(wm.focus_output(1));
    let window = wm.backend_mut().create_window();
    wm.dispatch(BackendEvent::MapRequest(window));
    let second = wm.client_for_window(window).unwrap();
    let placed = wm.client(second).unwrap().geometry;
    assert!(RIGHT_HEAD.contains(placed.pos), "placed at {placed:?}, not on the right head");
}

#[test]
fn output_targets_resolve_by_step_direction_and_name_against_the_live_list() {
    let mut wm = dual_desktop();
    wm.dispatch(BackendEvent::PointerMotion { root: Point::new(100, 100), surface_local: None });
    assert_eq!(wm.focused_output_index(), 0);
    assert_eq!(wm.resolve_output_target(&OutputTarget::Relative(1)), Some(1));
    assert_eq!(wm.resolve_output_target(&OutputTarget::Relative(-1)), Some(1), "wraps");
    assert_eq!(wm.resolve_output_target(&OutputTarget::Relative(2)), Some(0));
    assert_eq!(wm.resolve_output_target(&OutputTarget::Direction(FocusDirection::Right)), Some(1));
    assert_eq!(wm.resolve_output_target(&OutputTarget::Direction(FocusDirection::Left)), None);
    assert_eq!(wm.resolve_output_target(&OutputTarget::Direction(FocusDirection::Up)), None);
    assert_eq!(wm.resolve_output_target(&OutputTarget::Name("right".into())), Some(1));
    assert_eq!(wm.resolve_output_target(&OutputTarget::Name("DP-9".into())), None);
    assert!(wm.focus_output(1));
    assert_eq!(wm.focused_output_index(), 1);
    assert_eq!(wm.resolve_output_target(&OutputTarget::Direction(FocusDirection::Left)), Some(0));
}

#[test]
fn workspace_previous_flips_between_the_last_two_workspaces() {
    let mut wm = wm(FakeBackend::new());
    assert_eq!(wm.previous_workspace(), None);
    assert!(!wm.switch_to_previous_workspace());
    assert_eq!(wm.current_workspace(), 0);

    wm.switch_workspace(2);
    assert_eq!(wm.previous_workspace(), Some(0));
    assert!(wm.switch_to_previous_workspace());
    assert_eq!(wm.current_workspace(), 0);
    assert!(wm.switch_to_previous_workspace());
    assert_eq!(wm.current_workspace(), 2);
    assert_eq!(wm.previous_workspace(), Some(0));

    // Removal renumbers it like every other index, and forgets it
    // when it names the removed workspace itself.
    wm.switch_workspace(1);
    wm.switch_workspace(2);
    assert_eq!(wm.previous_workspace(), Some(1));
    assert!(wm.remove_workspace(0));
    assert_eq!((wm.current_workspace(), wm.previous_workspace()), (1, Some(0)));
    assert!(wm.remove_workspace(0));
    assert_eq!((wm.current_workspace(), wm.previous_workspace()), (0, None));
}

/// Omarchy's SUPER+TAB is `e+1`, "the next workspace that exists": with
/// windows on workspaces 1 and 3, it goes 1 → 3 → 1 and never creates a
/// fourth.
#[test]
fn occupied_stepping_skips_empty_workspaces_wraps_and_never_grows_the_row() {
    let mut wm = wm(FakeBackend::new());
    let first = map_on(&mut wm, 0);
    let third = map_on(&mut wm, 0);
    wm.move_client_to_workspace(third, 2);
    assert_eq!(wm.workspace_count(), 3);
    assert_eq!(wm.current_workspace(), 0);

    assert_eq!(wm.occupied_workspace_step(1), Some(2));
    wm.switch_workspace(2);
    assert_eq!(wm.occupied_workspace_step(1), Some(0), "wraps past the last occupied workspace");
    wm.switch_workspace(0);
    assert_eq!(wm.occupied_workspace_step(-1), Some(2));
    assert_eq!(wm.workspace_count(), 3, "stepping never grows the row");

    // An empty current workspace counts as a stop on the way round.
    wm.switch_workspace(1);
    assert_eq!(wm.occupied_workspace_step(1), Some(2));
    assert_eq!(wm.occupied_workspace_step(-1), Some(0));

    // Nothing else occupied: stay put rather than create a workspace.
    wm.move_client_to_workspace(third, 0);
    wm.switch_workspace(0);
    assert_eq!(wm.occupied_workspace_step(1), None);
    assert_eq!(wm.occupied_workspace_step(-1), None);
    let _ = first;
}
