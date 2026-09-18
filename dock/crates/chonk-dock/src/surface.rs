//! Client surface operations, deliberately separate from wm_core::Backend.
pub use wm_core::DragHandle;
use wm_theme_api::{DecorationBuffer, Rect};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    Dock,
    Clip,
    Launcher,
    Icon,
    Backdrop,
    #[default]
    Panel,
}

pub trait Backend {
    type ShellId: Copy + Eq + std::fmt::Debug;
    type WindowId: Copy + Eq;
    fn display_name(&self) -> String {
        crate::dockapp::current_display()
    }
    fn create_shell_surface(
        &mut self,
        geometry: Rect,
        background: (u8, u8, u8),
        input: bool,
    ) -> Option<Self::ShellId>;
    fn set_role(&mut self, id: Self::ShellId, role: Role);
    fn map_shell_surface(&mut self, id: Self::ShellId);
    fn unmap_shell_surface(&mut self, id: Self::ShellId);
    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: Rect);
    fn paint_shell_surface(&mut self, id: Self::ShellId, pixels: &DecorationBuffer);
    fn release_shell_buffer(&mut self, id: Self::ShellId);
    fn destroy_shell_surface(&mut self, id: Self::ShellId);
    fn raise_shell_surface(&mut self, id: Self::ShellId);
    fn grab_pointer_for_drag(&mut self) -> DragHandle;
    fn ungrab_pointer(&mut self, grab: DragHandle);
    fn panel_keyboard(&mut self, enabled: bool);
}

#[cfg(test)]
pub use wm_core::fake_backend::FakeBackend;

#[cfg(test)]
impl Backend for FakeBackend {
    type ShellId = <Self as wm_core::Backend>::ShellId;
    type WindowId = <Self as wm_core::Backend>::WindowId;
    fn create_shell_surface(
        &mut self,
        geometry: Rect,
        bg: (u8, u8, u8),
        input: bool,
    ) -> Option<Self::ShellId> {
        wm_core::Backend::create_shell_surface(self, geometry, bg, input)
    }
    fn set_role(&mut self, _: Self::ShellId, _: Role) {}
    fn map_shell_surface(&mut self, id: Self::ShellId) {
        wm_core::Backend::map_shell_surface(self, id);
    }
    fn unmap_shell_surface(&mut self, id: Self::ShellId) {
        wm_core::Backend::unmap_shell_surface(self, id);
    }
    fn configure_shell_surface(&mut self, id: Self::ShellId, rect: Rect) {
        wm_core::Backend::configure_shell_surface(self, id, rect);
    }
    fn paint_shell_surface(&mut self, id: Self::ShellId, pixels: &DecorationBuffer) {
        wm_core::Backend::paint_shell_surface(self, id, pixels);
    }
    fn release_shell_buffer(&mut self, id: Self::ShellId) {
        wm_core::Backend::release_shell_buffer(self, id);
    }
    fn destroy_shell_surface(&mut self, id: Self::ShellId) {
        wm_core::Backend::destroy_shell_surface(self, id);
    }
    fn raise_shell_surface(&mut self, id: Self::ShellId) {
        wm_core::Backend::raise_shell_surface(self, id);
    }
    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        wm_core::Backend::grab_pointer_for_drag(self)
    }
    fn ungrab_pointer(&mut self, grab: DragHandle) {
        wm_core::Backend::ungrab_pointer(self, grab);
    }
    fn panel_keyboard(&mut self, _: bool) {}
}
