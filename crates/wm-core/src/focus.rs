/// How input focus follows the pointer/clicks. The classic NeXTSTEP
/// desktop's default is click-to-focus; focus-follows-mouse is an
/// available option, not the default. Dispatch logic for either policy is wired up
/// once a real `Backend` drives it (milestone step 8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusPolicy {
    #[default]
    ClickToFocus,
    FocusFollowsMouse,
}

/// A direction in the desktop's root-coordinate space.
///
/// Kept in `wm-core` because deciding which floating window is "left"
/// of another is window-manager policy, not a property of whichever
/// configuration syntax requested it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}
