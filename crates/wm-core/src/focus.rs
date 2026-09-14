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

/// Which output a monitor-targeted verb names: the argument of
/// Hyprland's `focusmonitor` and `movecurrentworkspacetomonitor`.
///
/// Kept symbolic rather than resolved to an index at parse time, because
/// hotplug can remove an output between a binding being read and a key
/// being pressed; [`crate::WindowManager::resolve_output_target`] answers
/// against the live monitor list at the moment the verb applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    /// `+N` / `-N`: N outputs along the monitor list from the focused
    /// one, wrapping at either end.
    Relative(i32),
    /// `l` / `r` / `u` / `d`: the nearest output in that direction.
    Direction(FocusDirection),
    /// A connector name, as `Backend::monitors` reports it.
    Name(String),
}
