//! Text fitting retained for the dock instrument renderers.
use crate::model::FontSpec;
pub use chonk_theme::paint::*;
pub(crate) fn fit_text(
    font_system: &mut cosmic_text::FontSystem,
    font: &FontSpec,
    text: &str,
    width: u32,
) -> String {
    fit_measured_prefix(text, width, "", |candidate| {
        text_width(font_system, font, candidate)
    })
}
pub(crate) fn fit_measured_prefix(
    text: &str,
    max_width: u32,
    suffix: &str,
    mut measure: impl FnMut(&str) -> u32,
) -> String {
    if measure(text) <= max_width {
        return text.to_string();
    }
    let boundaries: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    let candidate = |count: usize| {
        let prefix = &text[..boundaries.get(count).copied().unwrap_or(text.len())];
        let prefix = if suffix.is_empty() {
            prefix
        } else {
            prefix.trim_end()
        };
        let mut result = String::with_capacity(prefix.len() + suffix.len());
        result.push_str(prefix);
        result.push_str(suffix);
        result
    };
    let mut low = 0;
    let mut high = boundaries.len().saturating_sub(1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if measure(&candidate(middle)) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    candidate(low)
}
