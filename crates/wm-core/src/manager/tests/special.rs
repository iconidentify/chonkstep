//! Special workspaces: named per-output overlays with their own layout.
use super::*;

fn desk() -> WindowManager<FakeBackend> {
    wm(FakeBackend::new())
}

fn map(wm: &mut WindowManager<FakeBackend>) -> ClientId {
    let window = wm.backend_mut().create_window();
    wm.dispatch(BackendEvent::MapRequest(window));
    wm.client_for_window(window).unwrap()
}

/// Where the frame's most recent raise sits in the backend's raise
/// log; a later position is higher in the stack.
fn last_raise(wm: &WindowManager<FakeBackend>, id: ClientId) -> Option<usize> {
    let frame = wm.client(id)?.frame?;
    wm.backend().raised_frames.iter().rposition(|raised| *raised == frame)
}

#[test]
fn toggling_a_special_shows_raises_and_focuses_its_members_then_hides_them() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    let c = map(&mut wm);
    assert_eq!(wm.focused_client(), Some(c));

    assert!(wm.move_client_to_special(b, "scratchpad", false));
    assert!(wm.move_client_to_special(c, "scratchpad", false));
    assert!(!frame_mapped(&wm, b) && !frame_mapped(&wm, c), "silently moved members leave the screen");
    assert_eq!(wm.focused_client(), Some(a), "focus moves on when the focused member leaves");
    assert_eq!(wm.special_workspaces().collect::<Vec<_>>(), vec![(0, "scratchpad")]);
    assert_eq!(wm.special_layout_order(0), &[b, c]);
    assert_eq!(wm.special_shown_on_output(0), None);

    assert!(wm.toggle_special("scratchpad"));
    assert_eq!(wm.special_shown_on_output(0), Some(0));
    assert!(wm.special_visible(0));
    assert!(frame_mapped(&wm, b) && frame_mapped(&wm, c), "shown members are mapped");
    assert!(frame_mapped(&wm, a), "the workspace underneath stays where it is");
    assert_eq!(wm.focused_client(), Some(c), "the most recently focused member takes the keyboard");
    assert!(last_raise(&wm, c) > last_raise(&wm, a), "members are raised over the workspace");
    assert!(last_raise(&wm, b) > last_raise(&wm, a));
    assert!(last_raise(&wm, c) > last_raise(&wm, b), "the focused member is on top");
    assert_focus_is_on_screen(&wm);

    assert!(wm.toggle_special("scratchpad"));
    assert_eq!(wm.special_shown_on_output(0), None);
    assert!(!frame_mapped(&wm, b) && !frame_mapped(&wm, c), "a second toggle hides the members");
    assert!(frame_mapped(&wm, a));
    assert_eq!(wm.focused_client(), Some(a), "focus returns to the workspace's window");
    assert_focus_is_on_screen(&wm);
}

#[test]
fn moving_the_focused_window_silently_hides_it_and_keeps_the_workspace() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    assert_eq!(wm.focused_client(), Some(b));

    assert!(wm.move_client_to_special(b, "special:scratchpad", false));
    assert_eq!(wm.current_workspace(), 0);
    assert!(!frame_mapped(&wm, b));
    assert!(frame_mapped(&wm, a));
    assert_eq!(wm.focused_client(), Some(a));
    assert_eq!(wm.client(b).unwrap().special, Some(0));
    assert_eq!(wm.client(b).unwrap().workspace, 0, "the numbered home is kept");
    assert!(!wm.layout_order(0).contains(&b), "a member leaves its workspace's layout order");
    assert!(!wm.client_visible(b));
    // A hidden member is not somewhere Alt-Tab can land.
    assert!(wm.focus_adjacent_client(true));
    assert_eq!(wm.focused_client(), Some(a));
    assert!(wm.focus_client_without_raising(b));
    assert_eq!(wm.focused_client(), Some(a), "focus is refused for a window nobody can see");
}

#[test]
fn following_a_window_to_a_special_shows_it_and_focuses_the_window() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    wm.focus_client(a);
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    assert_eq!(wm.special_shown_on_output(0), Some(0));
    assert!(frame_mapped(&wm, b));
    assert_eq!(wm.focused_client(), Some(b));
    assert!(last_raise(&wm, b) > last_raise(&wm, a));
}

#[test]
fn a_workspace_switch_hides_the_shown_special_when_configured_to() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    wm.set_hide_special_on_workspace_change(true);
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    assert_eq!(wm.focused_client(), Some(b));

    wm.switch_workspace(1);
    assert_eq!(wm.special_shown_on_output(0), None, "the switch takes the overlay down");
    assert!(!frame_mapped(&wm, b));
    assert!(!frame_mapped(&wm, a), "workspace 0's window left with its workspace");
    assert_eq!(wm.focused_client(), None, "workspace 1 is empty");
    assert_focus_is_on_screen(&wm);

    wm.switch_workspace(0);
    assert_eq!(wm.special_shown_on_output(0), None, "it stays down on the way back");
    assert!(!frame_mapped(&wm, b));
    assert!(frame_mapped(&wm, a));
    assert_eq!(wm.focused_client(), Some(a));
}

#[test]
fn a_workspace_switch_keeps_the_special_shown_and_on_top_by_default() {
    let mut wm = desk();
    let _a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    wm.switch_workspace(1);
    let c = map(&mut wm);
    wm.switch_workspace(0);
    assert_eq!(wm.special_shown_on_output(0), Some(0));
    assert!(frame_mapped(&wm, b), "the overlay survives the switch");
    assert!(!frame_mapped(&wm, c));
    wm.switch_workspace(1);
    assert!(frame_mapped(&wm, b) && frame_mapped(&wm, c));
    assert!(last_raise(&wm, b) > last_raise(&wm, c), "the overlay is reasserted above what the switch remapped");
}

#[test]
fn destroying_a_member_leaves_no_stale_index_and_the_overlay_stays_shown() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    let c = map(&mut wm);
    assert!(wm.move_client_to_special(b, "scratchpad", false));
    assert!(wm.move_client_to_special(c, "scratchpad", false));
    assert!(wm.toggle_special("scratchpad"));
    assert_eq!(wm.focused_client(), Some(c));

    let window = wm.client(c).unwrap().window;
    wm.dispatch(BackendEvent::Destroyed(window));
    assert!(wm.client(c).is_none());
    assert_eq!(wm.special_layout_order(0), &[b], "the destroyed member is out of the special's order");
    assert_eq!(wm.focused_client(), Some(b), "focus stays inside the shown overlay");
    assert_eq!(wm.special_shown_on_output(0), Some(0));

    let window = wm.client(b).unwrap().window;
    wm.dispatch(BackendEvent::Unmapped(window));
    assert!(wm.special_layout_order(0).is_empty());
    assert_eq!(wm.special_shown_on_output(0), Some(0), "an emptied overlay stays shown, as the console expects");
    assert_eq!(wm.focused_client(), Some(a));
    assert!(!wm.focus_history().contains(&b) && !wm.focus_history().contains(&c));
}

#[test]
fn shown_special_members_sit_above_pinned_windows() {
    let mut wm = desk();
    let pinned = map(&mut wm);
    assert!(wm.set_client_pinned(pinned, true));
    let member = map(&mut wm);
    assert!(wm.move_client_to_special(member, "scratchpad", false));
    assert!(last_raise(&wm, pinned) > last_raise(&wm, member), "pinned is above an ordinary window");

    assert!(wm.toggle_special("scratchpad"));
    assert!(last_raise(&wm, member) > last_raise(&wm, pinned), "the overlay is above the pinned window");

    // Focusing (and so raising) the pinned window must not put it over
    // the overlay: the raise path reasserts the tiers in order.
    wm.focus_client(pinned);
    assert_eq!(wm.focused_client(), Some(pinned));
    assert!(last_raise(&wm, member) > last_raise(&wm, pinned));

    assert!(wm.toggle_special("scratchpad"));
    assert!(!frame_mapped(&wm, member));
    assert!(frame_mapped(&wm, pinned));
}

#[derive(Debug)]
struct SilentSpecialRule;

impl FloatPolicy for SilentSpecialRule {
    fn decision_for(&self, _class: &str, _title: &str) -> Option<crate::placement::FloatDecision> {
        None
    }

    fn window_decision_for(&self, _class: &str, title: &str) -> crate::placement::WindowRuleDecision {
        let workspace = title.contains("is sharing").then(|| crate::placement::RuleWorkspace {
            target: crate::placement::RuleWorkspaceTarget::Special("special".into()),
            silent: true,
        });
        crate::placement::WindowRuleDecision { workspace, ..Default::default() }
    }
}

#[test]
fn a_silent_special_rule_maps_the_window_hidden_and_unfocused() {
    let mut wm = desk();
    wm.set_float_policy(Some(std::sync::Arc::new(SilentSpecialRule)));
    let a = map(&mut wm);
    let bar = wm.backend_mut().create_window();
    wm.backend_mut().set_title(bar, "example.com is sharing your screen.");
    wm.dispatch(BackendEvent::MapRequest(bar));
    let bar = wm.client_for_window(bar).unwrap();

    assert!(!frame_mapped(&wm, bar), "the rule parks the window on the hidden special");
    assert_eq!(wm.focused_client(), Some(a), "silent: no initial focus");
    assert_eq!(wm.client(bar).unwrap().special, Some(0));
    assert_eq!(wm.special_name(0), Some("special"));
    assert_eq!(wm.special_shown_on_output(0), None);
    assert!(!wm.layout_order(0).contains(&bar), "never entered the workspace's layout");

    // The default special is what a bare `togglespecialworkspace` shows.
    assert!(wm.toggle_special(""));
    assert!(frame_mapped(&wm, bar));
    assert_eq!(wm.focused_client(), Some(bar));
}

#[test]
fn a_numbered_workspace_rule_moves_the_window_before_it_is_seen() {
    #[derive(Debug)]
    struct ToWorkspaceThree(bool);
    impl FloatPolicy for ToWorkspaceThree {
        fn decision_for(&self, _class: &str, _title: &str) -> Option<crate::placement::FloatDecision> {
            None
        }
        fn window_decision_for(&self, _class: &str, _title: &str) -> crate::placement::WindowRuleDecision {
            crate::placement::WindowRuleDecision {
                workspace: Some(crate::placement::RuleWorkspace {
                    target: crate::placement::RuleWorkspaceTarget::Numbered(2),
                    silent: self.0,
                }),
                ..Default::default()
            }
        }
    }
    for silent in [true, false] {
        let mut wm = desk();
        let a = map(&mut wm);
        wm.set_float_policy(Some(std::sync::Arc::new(ToWorkspaceThree(silent))));
        let b = map(&mut wm);
        assert_eq!(wm.client(b).unwrap().workspace, 2, "silent={silent}");
        assert!(wm.layout_order(2).contains(&b) && !wm.layout_order(0).contains(&b));
        if silent {
            assert_eq!(wm.current_workspace(), 0);
            assert!(!frame_mapped(&wm, b) && frame_mapped(&wm, a));
            assert_eq!(wm.focused_client(), Some(a));
        } else {
            assert_eq!(wm.current_workspace(), 2, "a non-silent rule follows the window");
            assert!(frame_mapped(&wm, b) && !frame_mapped(&wm, a));
            assert_eq!(wm.focused_client(), Some(b));
        }
        assert_focus_is_on_screen(&wm);
    }
}

#[test]
fn moving_a_member_to_a_numbered_workspace_ends_its_membership() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.move_client_to_special(b, "scratchpad", false));
    assert!(!frame_mapped(&wm, b));

    // Back to its own home, which is the current workspace: it has to
    // rejoin that layout and come back on screen.
    wm.move_client_to_workspace(b, 0);
    assert_eq!(wm.client(b).unwrap().special, None);
    assert!(wm.special_layout_order(0).is_empty());
    assert!(wm.layout_order(0).contains(&b));
    assert!(frame_mapped(&wm, b));
    assert!(wm.client_visible(b));

    // And to a parked workspace: out of the special, and hidden by the
    // ordinary rule.
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    assert_eq!(wm.focused_client(), Some(b));
    wm.move_client_to_workspace(b, 1);
    assert_eq!(wm.client(b).unwrap().special, None);
    assert!(!frame_mapped(&wm, b));
    assert_eq!(wm.focused_client(), Some(a));
    assert_eq!(wm.special_shown_on_output(0), Some(0), "the overlay itself is untouched");
}

#[test]
fn activating_a_hidden_member_shows_its_special_unless_the_session_is_locked() {
    let mut wm = desk();
    let a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.move_client_to_special(b, "scratchpad", false));
    let window = wm.client(b).unwrap().window;

    // Behind the lock an activation may only ask for attention: showing
    // the overlay would put a window on a desk the user did not leave.
    wm.backend_mut().session_locked = true;
    wm.dispatch(BackendEvent::ActivateRequested(window));
    assert!(!frame_mapped(&wm, b), "nothing is shown behind the lock");
    assert_eq!(wm.special_shown_on_output(0), None);
    assert!(wm.client(b).unwrap().flags.contains(ClientFlags::URGENT));
    assert_eq!(wm.focused_client(), Some(a));

    // Unlocking re-shows nothing on its own.
    wm.backend_mut().session_locked = false;
    assert!(!frame_mapped(&wm, b));
    assert_eq!(wm.special_shown_on_output(0), None);

    wm.dispatch(BackendEvent::ActivateRequested(window));
    assert_eq!(wm.special_shown_on_output(0), Some(0), "an unlocked activation drops the overlay down");
    assert!(frame_mapped(&wm, b));
    assert_eq!(wm.focused_client(), Some(b));
    assert!(!wm.client(b).unwrap().flags.contains(ClientFlags::URGENT));
}

#[test]
fn a_special_hidden_before_the_lock_stays_hidden_after_it() {
    let mut wm = desk();
    let _a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    assert!(wm.toggle_special("scratchpad"));
    assert!(!frame_mapped(&wm, b));
    wm.backend_mut().session_locked = true;
    wm.switch_workspace(1);
    wm.switch_workspace(0);
    wm.backend_mut().session_locked = false;
    assert!(!frame_mapped(&wm, b));
    assert_eq!(wm.special_shown_on_output(0), None);
}

#[test]
fn removing_an_output_drops_its_shown_entry_and_hides_the_members() {
    let mut backend = FakeBackend::new();
    backend.set_monitors(dual_monitors());
    let mut wm = wm(backend);
    let left = map(&mut wm);
    // A window on the right head makes that the active output.
    let window = wm.backend_mut().create_window();
    wm.backend_mut().set_geometry(window, Rect { pos: Point::new(1000, 100), size: Size::new(240, 180) });
    wm.dispatch(BackendEvent::MapRequest(window));
    let right = wm.client_for_window(window).unwrap();
    assert_eq!(wm.client_output_index(right), 1);
    assert_eq!(wm.focused_client(), Some(right));

    assert!(wm.move_client_to_special(right, "scratchpad", true));
    assert_eq!(wm.special_shown_on_output(1), Some(0));
    assert_eq!(wm.special_shown_on_output(0), None, "the overlay is per output");
    assert!(frame_mapped(&wm, right));

    wm.backend_mut().set_monitors(vec![MonitorInfo {
        geometry: LEFT_HEAD,
        name: "left".to_string(),
        identity: None,
        primary: true,
    }]);
    wm.rescue_clients_from_removed_monitor(RIGHT_HEAD);
    assert_eq!(wm.special_shown_on_output(0), None);
    assert!(!wm.special_visible(0), "an entry for a departed output is gone");
    assert!(!frame_mapped(&wm, right), "its members are off screen, not stranded visible");
    assert_eq!(wm.focused_client(), Some(left));
    assert_focus_is_on_screen(&wm);

    // Showing it again brings the member onto the output that is left.
    assert!(wm.toggle_special("scratchpad"));
    assert!(frame_mapped(&wm, right));
    let frame = client_frame_rect(wm.client(right).unwrap());
    assert!(LEFT_HEAD.contains(frame.pos), "carried into the shown output's workarea: {frame:?}");
}

#[test]
fn only_one_special_is_shown_per_output_and_it_moves_with_the_toggle() {
    let mut backend = FakeBackend::new();
    backend.set_monitors(dual_monitors());
    let mut wm = wm(backend);
    let a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.move_client_to_special(a, "one", false));
    assert!(wm.move_client_to_special(b, "two", false));
    assert!(wm.toggle_special("one"));
    assert!(frame_mapped(&wm, a));
    assert!(wm.toggle_special("two"));
    assert_eq!(wm.special_shown_on_output(0), Some(1), "showing another special replaces the shown one");
    assert!(!frame_mapped(&wm, a) && frame_mapped(&wm, b));
    assert_eq!(wm.focused_client(), Some(b));

    // Working on the right head makes it the active output: the toggle
    // there moves the overlay rather than showing it twice.
    let window = wm.backend_mut().create_window();
    wm.backend_mut().set_geometry(window, Rect { pos: Point::new(1000, 100), size: Size::new(240, 180) });
    wm.dispatch(BackendEvent::MapRequest(window));
    let right = wm.client_for_window(window).unwrap();
    assert_eq!(wm.focused_client(), Some(right));
    assert_eq!(wm.active_output_index(), 1);
    assert!(wm.toggle_special("two"));
    assert_eq!(wm.special_shown_on_output(0), None);
    assert_eq!(wm.special_shown_on_output(1), Some(1));
    assert!(frame_mapped(&wm, b));
    assert_eq!(wm.client_output_index(b), 1, "the member followed the overlay to the right head");
}

#[test]
fn names_and_the_special_count_are_bounded() {
    let mut wm = desk();
    assert_eq!(normalize_special_name(" special:scratchpad ").as_deref(), Some("scratchpad"));
    assert_eq!(normalize_special_name("").as_deref(), Some(DEFAULT_SPECIAL_NAME));
    assert_eq!(normalize_special_name("special").as_deref(), Some(DEFAULT_SPECIAL_NAME));
    assert_eq!(normalize_special_name("special:").as_deref(), Some(DEFAULT_SPECIAL_NAME));
    assert_eq!(normalize_special_name(&"n".repeat(MAX_SPECIAL_NAME)).map(|n| n.len()), Some(MAX_SPECIAL_NAME));
    assert_eq!(normalize_special_name(&"n".repeat(MAX_SPECIAL_NAME + 1)), None);
    assert_eq!(normalize_special_name("bad\nname"), None);

    assert!(!wm.toggle_special(&"n".repeat(MAX_SPECIAL_NAME + 1)), "an overlong name creates nothing");
    assert_eq!(wm.special_workspaces().count(), 0);
    for n in 0..MAX_SPECIAL_WORKSPACES {
        assert!(wm.toggle_special(&format!("s{n}")));
    }
    assert_eq!(wm.special_workspaces().count(), MAX_SPECIAL_WORKSPACES);
    assert!(!wm.toggle_special("one-too-many"), "the count is spent");
    assert_eq!(wm.special_workspaces().count(), MAX_SPECIAL_WORKSPACES);
    let id = map(&mut wm);
    assert!(!wm.move_client_to_special(id, "one-too-many", false));
    assert_eq!(wm.client(id).unwrap().special, None);
    assert!(wm.toggle_special("s3"), "an existing name still toggles");
    assert_eq!(wm.special_index("special:s3"), Some(3));
}

#[test]
fn a_hidden_member_does_not_inhibit_idle_and_a_shown_one_does() {
    let mut wm = desk();
    wm.set_float_policy(Some(std::sync::Arc::new(InhibitsIdle)));
    let id = map(&mut wm);
    assert!(wm.rule_idle_inhibited());
    assert!(wm.move_client_to_special(id, "scratchpad", false));
    assert!(!wm.rule_idle_inhibited(), "off screen means not inhibiting");
    assert!(wm.toggle_special("scratchpad"));
    assert!(wm.rule_idle_inhibited());
}

#[test]
fn a_member_of_a_mosaic_workspace_is_not_layout_managed_while_it_is_special() {
    let mut wm = desk();
    wm.set_workspace_layout(0, crate::LayoutMode::Mosaic);
    let a = map(&mut wm);
    let b = map(&mut wm);
    assert!(wm.is_layout_managed(a) && wm.is_layout_managed(b));
    assert!(wm.move_client_to_special(b, "scratchpad", true));
    assert!(!wm.is_layout_managed(b), "the special's own layout governs its members");
    assert!(wm.is_layout_managed(a));
    assert_eq!(wm.layout_order(0), &[a]);
    wm.move_client_to_workspace(b, 0);
    assert!(wm.is_layout_managed(b));
    assert_eq!(wm.layout_order(0), &[a, b]);
}
