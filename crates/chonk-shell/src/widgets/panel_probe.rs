//! The built-in panel conformance probe: a deliberately trivial
//! panel-capable widget, compiled in but constructed only when
//! `CHONKSTEP_TEST_PANEL_TILE` is set in the shell's environment —
//! the same gating idea as the injection door
//! (`CHONKSTEP_TEST_SOCKET`), and for the same reason: the end-to-end
//! suite boots the real shell and needs a known instrument to poke,
//! while a production session must never show one.
//!
//! It is the in-process twin of `chonk-testkit`'s `chonk-panel-probe`
//! dockapp, and asserts the same conversation from the other side of
//! the process boundary: a solid-color tile, a panel that opens solid
//! green, turns red when clicked (the input round trip made visible in
//! a screenshot), and toggles back per click. Colors are written
//! as plain premultiplied RGBA literals rather than through the theme
//! so a screenshot assertion has exact channels to look for.

use chonk_dock_widget::{DockWidget, PanelCtx, PanelEvent, PanelFrame, PanelReaction, PanelSpec, Samples};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

/// The panel content size the probe asks for — small enough to be
/// granted verbatim on any real workarea, so the e2e can assert the
/// surface's size against it.
const PANEL: (u32, u32) = (300, 200);

/// Opaque premultiplied pure green: the freshly opened panel.
const GREEN: [u8; 4] = [0x00, 0xFF, 0x00, 0xFF];
/// Opaque premultiplied pure red: the panel after a click reached it.
const RED: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF];
/// The tile's own face — an arbitrary opaque blue-gray, distinct from
/// both panel colors.
const FACE: [u8; 4] = [0x30, 0x60, 0x90, 0xFF];

fn fill(pixels: &mut [u8], rgba: [u8; 4]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&rgba);
    }
}

pub(crate) struct PanelProbeWidget {
    /// Flipped by every left press inside the panel; decides the
    /// panel's color, so input reaching the widget is visible from
    /// outside the process as a repaint.
    clicked: bool,
}

impl PanelProbeWidget {
    pub(crate) fn new() -> Self {
        Self { clicked: false }
    }
}

impl DockWidget for PanelProbeWidget {
    fn name(&self) -> &str {
        "BIP"
    }

    fn update(&mut self, _samples: &Samples) -> bool {
        false
    }

    fn render(&self, _theme: &Theme, tile: u32, _fonts: &mut cosmic_text::FontSystem, _swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let mut pixels = vec![0u8; (tile as usize) * (tile as usize) * 4];
        fill(&mut pixels, FACE);
        DecorationBuffer { width: tile, height: tile, pixels }
    }

    fn panel_spec(&self, _tile: u32) -> Option<PanelSpec> {
        // Deliberately a fixed size rather than one derived from
        // `tile`: the e2e asserts the granted surface against these
        // exact numbers, and a probe whose geometry moved with the
        // desk's scale would be asserting the shell's arithmetic
        // twice instead of the panel's plumbing once.
        Some(PanelSpec::new(PANEL.0, PANEL.1))
    }

    fn render_panel(&mut self, frame: &mut PanelFrame, _ctx: &mut PanelCtx<'_>) {
        let color = if self.clicked { RED } else { GREEN };
        fill(frame.pixels_mut(), color);
    }

    fn panel_input(&mut self, event: PanelEvent, _tile: u32) -> PanelReaction {
        match event {
            PanelEvent::LeftPress { .. } => {
                self.clicked = !self.clicked;
                PanelReaction::Repaint
            }
            _ => PanelReaction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's whole contract, held in-process so the e2e's colors
    /// cannot drift away from what it asserts on screen.
    #[test]
    fn the_probe_paints_green_until_clicked_and_red_after() {
        let mut probe = PanelProbeWidget::new();
        let spec = probe.panel_spec(56).expect("the probe is panel-capable");
        let mut frame = PanelFrame::new(spec.width, spec.height);
        let theme = wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty");
        let (mut fonts, mut swash) = (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new());
        let mut ctx = PanelCtx { theme: &theme, tile: 56, fonts: &mut fonts, swash: &mut swash };

        probe.render_panel(&mut frame, &mut ctx);
        assert_eq!(&frame.buffer().pixels[0..4], &GREEN);

        let reaction = probe.panel_input(PanelEvent::LeftPress { local: wm_theme_api::Point::new(1, 1) }, 56);
        assert!(matches!(reaction, PanelReaction::Repaint), "a click must ask for a repaint");
        probe.render_panel(&mut frame, &mut ctx);
        assert_eq!(&frame.buffer().pixels[0..4], &RED);

        assert!(matches!(probe.panel_input(PanelEvent::Enter, 56), PanelReaction::None), "crossings mean nothing to the probe");
    }
}
