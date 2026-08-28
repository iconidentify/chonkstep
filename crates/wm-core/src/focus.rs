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
