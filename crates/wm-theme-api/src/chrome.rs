//! Geometry shared by modern frame and shell renderers. Values are device
//! pixels after scaling. Painting and input must consume the same metrics.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameMetrics {
    pub title_height: u16,
    pub border: u16,
    pub radius: u16,
    pub input_margin: u16,
    pub padding: u16,
    pub button_size: u16,
    pub button_gap: u16,
    pub focus_edge: u16,
    /// Shape of the focused title marker, independent of the frame radius.
    pub round_focus_mark: bool,
}

impl Default for FrameMetrics {
    fn default() -> Self {
        Self {
            title_height: 32,
            border: 1,
            radius: 3,
            input_margin: 6,
            padding: 9,
            button_size: 20,
            button_gap: 8,
            focus_edge: 2,
            round_focus_mark: false,
        }
    }
}

pub(crate) fn scaled(value: u16, scale: f32) -> u16 {
    if value == 0 {
        return 0;
    }
    // This can be an output/base ratio, not an absolute UI scale. Two
    // supported output scales can require a ratio outside 0.125..=8.
    let scale = if scale.is_finite() && scale > 0.0 {
        f64::from(scale)
    } else {
        1.0
    };
    (f64::from(value) * scale)
        .round()
        .clamp(1.0, f64::from(u16::MAX)) as u16
}

impl FrameMetrics {
    /// Device-pixel limits bound every perimeter allocation, including metrics
    /// supplied by a serialized theme. They accommodate the built-ins at 8x.
    pub fn normalized(mut self) -> Self {
        self.border = self.border.clamp(1, 16);
        self.title_height = self.title_height.clamp(self.border + 1, 512);
        self.radius = self.radius.min(256);
        self.input_margin = self.input_margin.min(128);
        self.padding = self.padding.min(256);
        self.button_size = self.button_size.min(256).min(self.title_height);
        self.button_gap = self.button_gap.min(128);
        self.focus_edge = self.focus_edge.min(64).min(self.title_height);
        self
    }

    pub fn scaled(self, scale: f32) -> Self {
        Self {
            title_height: scaled(self.title_height, scale),
            border: scaled(self.border, scale),
            radius: scaled(self.radius, scale),
            input_margin: scaled(self.input_margin, scale),
            padding: scaled(self.padding, scale),
            button_size: scaled(self.button_size, scale),
            button_gap: scaled(self.button_gap, scale),
            focus_edge: scaled(self.focus_edge, scale),
            round_focus_mark: self.round_focus_mark,
        }
        .normalized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_scale_ratios_are_not_clamped_as_absolute_scales() {
        let base = FrameMetrics::default();
        assert_eq!(
            base.scaled(0.5).scaled(16.0).title_height,
            base.scaled(8.0).title_height
        );
        assert_eq!(
            base.scaled(8.0).scaled(0.0625).title_height,
            base.scaled(0.5).title_height
        );
        for invalid in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(base.scaled(invalid), base);
        }
    }

    #[test]
    fn geometry_bounds_survive_extreme_scaling_and_keep_absent_details_absent() {
        let frame = FrameMetrics::default().scaled(f32::MAX);
        assert_eq!(frame.title_height, 512);
        assert_eq!(frame.border, 16);
        assert_eq!(frame.input_margin, 128);
        assert_eq!(frame.radius, 256);
        let flat = FrameMetrics {
            radius: 0,
            input_margin: 0,
            padding: 0,
            button_size: 0,
            button_gap: 0,
            focus_edge: 0,
            ..FrameMetrics::default()
        }
        .scaled(2.0);
        assert_eq!(
            (
                flat.radius,
                flat.input_margin,
                flat.padding,
                flat.button_size,
                flat.button_gap,
                flat.focus_edge
            ),
            (0, 0, 0, 0, 0, 0)
        );
    }

}
