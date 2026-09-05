use crate::{DecorationBuffer, Rect};

/// Opaque token from `PopupHost::grab_pointer`, passed back to
/// `ungrab_pointer` — implementations never need to inspect it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupGrab(pub u64);

/// What a reusable, stateful popup UI (a cascading menu today; a tooltip
/// or combo-box dropdown could reuse the same shape later) needs from
/// whatever hosts it: create/paint/destroy an unmanaged overlay window,
/// and hold the pointer grab for the popup's lifetime so a
/// press-drag-release gesture reliably lands on the popup rather than
/// wherever the press originally started (X11's implicit-grab quirk).
///
/// Implemented by `wm-x11`'s `X11Backend` for the desktop shell's own
/// popups. This is deliberately *not* `wm_core::Backend` — that trait is
/// about managed client windows, and popups (menus, tooltips) aren't
/// clients. Any other host, such as a `chonk-ui` app building its own
/// dropdown menu over its own X11 connection, can implement this one
/// small trait to reuse the exact same popup *behavior* from
/// `wm-theme`'s cascade controller without depending on `wm-x11` or
/// `wm-core` at all.
pub trait PopupHost {
    type PopupId: Copy + Eq + std::fmt::Debug;

    /// Creates, maps, and raises a fresh overlay window in one step —
    /// every popup this SDK builds is shown immediately on creation.
    /// `None` on failure (the caller has nothing useful to do but skip
    /// showing the popup).
    fn create_popup(&mut self, geometry: Rect, background: (u8, u8, u8)) -> Option<Self::PopupId>;
    fn destroy_popup(&mut self, popup: Self::PopupId);
    fn paint_popup(&mut self, popup: Self::PopupId, buffer: &DecorationBuffer);

    fn grab_pointer(&mut self) -> PopupGrab;
    fn ungrab_pointer(&mut self, grab: PopupGrab);

    /// Gives a modal popup the keyboard for its lifetime. Defaults are
    /// deliberately inert for lightweight SDK hosts; desktop backends
    /// override both halves so menu navigation cannot leak into the
    /// focused client.
    fn grab_keyboard(&mut self) {}
    fn ungrab_keyboard(&mut self) {}
}
