//! X resource-database publication for X11 clients that do not consume
//! XSETTINGS.
//!
//! The root window's `RESOURCE_MANAGER` property is one shared,
//! newline-delimited database. Unlike `_XSETTINGS_SETTINGS`, it does not
//! belong to one selection owner: a user may have merged their own
//! `~/.Xresources` into it before this desktop publishes its scale. The
//! merge here therefore removes and replaces only the resource names this
//! desktop currently states. Every other byte is copied verbatim, including
//! comments and values that are not UTF-8.
//!
//! Xft's resource spelling and unit differ from its XSETTINGS counterpart:
//! `Xft.dpi` is an ordinary integer DPI, while `Xft/DPI` is fixed-point in
//! 1024ths. Xcursor uses pixels for `Xcursor.size` in both cases.

use crate::{DesktopAppearance, XFT_DPI_UNITS_PER_POINT, xft_dpi_for_scale};

const XFT_DPI: &[u8] = b"Xft.dpi";
const XCURSOR_SIZE: &[u8] = b"Xcursor.size";
const XCURSOR_THEME: &[u8] = b"Xcursor.theme";

/// The resource values derived from one desktop appearance.
///
/// Kept separately from the rendered database so the live manager can prove
/// an unchanged reload is a no-op without touching the X server. The cursor
/// theme is optional because an absent opinion must leave a user's own
/// `Xcursor.theme` resource alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceValues {
    dpi: i32,
    cursor_size: i32,
    cursor_theme: Option<String>,
}

impl ResourceValues {
    pub(crate) fn from_appearance(appearance: &DesktopAppearance) -> Self {
        let fixed_dpi = xft_dpi_for_scale(appearance.ui_scale);
        let dpi = ((fixed_dpi as f64) / f64::from(XFT_DPI_UNITS_PER_POINT)).round() as i32;
        let cursor_theme = appearance
            .cursor_theme_name
            .as_deref()
            .filter(|name| !name.is_empty())
            // A resource value is one physical line. Refuse to turn a
            // malformed theme name into another resource declaration.
            .filter(|name| !name.bytes().any(|byte| matches!(byte, b'\n' | b'\r' | b'\0')))
            .map(ToOwned::to_owned);
        Self { dpi, cursor_size: appearance.effective_cursor_size(), cursor_theme }
    }
}

/// Merges this desktop's resource values into an X resource database.
///
/// Existing `Xft.dpi` and `Xcursor.size` declarations are replaced. An
/// existing `Xcursor.theme` is replaced only when `appearance` actually
/// names a cursor theme; otherwise it belongs to the user and is preserved.
/// Unrelated lines are retained byte-for-byte and in their original order.
/// A missing final newline is supplied only when needed to keep the first
/// appended resource from joining the user's last line.
pub fn merge_resource_manager(existing: &[u8], appearance: &DesktopAppearance) -> Vec<u8> {
    let values = ResourceValues::from_appearance(appearance);
    merge_transition(existing, None, &values)
}

/// The live-manager form of [`merge_resource_manager`]. `previous` lets a
/// cursor-theme opinion be withdrawn: the desktop declaration is removed even
/// when the new appearance deliberately states no theme.
pub(crate) fn merge_transition(
    existing: &[u8],
    previous: Option<&ResourceValues>,
    current: &ResourceValues,
) -> Vec<u8> {
    let remove_theme = current.cursor_theme.is_some()
        || previous.is_some_and(|values| values.cursor_theme.is_some());
    let mut merged = Vec::with_capacity(existing.len() + 64);

    for line in existing.split_inclusive(|byte| *byte == b'\n') {
        if !is_owned_line(line, remove_theme) {
            merged.extend_from_slice(line);
        }
    }
    if !merged.is_empty() && !merged.ends_with(b"\n") {
        merged.push(b'\n');
    }

    append_resource(&mut merged, XFT_DPI, current.dpi.to_string().as_bytes());
    append_resource(
        &mut merged,
        XCURSOR_SIZE,
        current.cursor_size.to_string().as_bytes(),
    );
    if let Some(theme) = &current.cursor_theme {
        append_resource(&mut merged, XCURSOR_THEME, theme.as_bytes());
    }
    merged
}

fn append_resource(database: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    database.extend_from_slice(name);
    database.extend_from_slice(b":\t");
    database.extend_from_slice(value);
    database.push(b'\n');
}

fn is_owned_line(line: &[u8], remove_theme: bool) -> bool {
    let Some(colon) = line.iter().position(|byte| *byte == b':') else {
        return false;
    };
    let name = trim_ascii_space(&line[..colon]);
    name == XFT_DPI || name == XCURSOR_SIZE || (remove_theme && name == XCURSOR_THEME)
}

fn trim_ascii_space(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_values_use_resource_database_units() {
        assert_eq!(
            merge_resource_manager(b"", &DesktopAppearance::new(1.0, "")),
            b"Xft.dpi:\t96\nXcursor.size:\t24\n"
        );
        assert_eq!(
            merge_resource_manager(b"", &DesktopAppearance::new(1.5, "")),
            b"Xft.dpi:\t144\nXcursor.size:\t36\n"
        );
        assert_eq!(
            merge_resource_manager(b"", &DesktopAppearance::new(2.0, "")),
            b"Xft.dpi:\t192\nXcursor.size:\t48\n"
        );
    }

    #[test]
    fn merge_replaces_only_owned_lines_and_preserves_arbitrary_bytes() {
        let existing = b"! user comment\nEmacs.font:\tIosevka\n Xft.dpi : 72\r\nraw:\t\xff\xfe\nXcursor.size:\t16";
        let merged = merge_resource_manager(existing, &DesktopAppearance::new(2.0, ""));
        assert_eq!(
            merged,
            b"! user comment\nEmacs.font:\tIosevka\nraw:\t\xff\xfe\nXft.dpi:\t192\nXcursor.size:\t48\n"
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let appearance = DesktopAppearance::new(1.25, "");
        let once = merge_resource_manager(b"XTerm*faceName: monospace\n", &appearance);
        assert_eq!(merge_resource_manager(&once, &appearance), once);
    }

    #[test]
    fn cursor_theme_is_left_to_the_user_until_the_desktop_has_an_opinion() {
        let user = b"Xcursor.theme:\tUserTheme\nOther:\tkept\n";
        let no_opinion = DesktopAppearance::new(1.0, "");
        let preserved = merge_resource_manager(user, &no_opinion);
        assert!(preserved.starts_with(user));

        let themed = no_opinion.with_cursor_theme("DesktopTheme");
        let replaced = merge_resource_manager(&preserved, &themed);
        assert!(!replaced.windows(b"UserTheme".len()).any(|part| part == b"UserTheme"));
        assert!(replaced.windows(b"Xcursor.theme:\tDesktopTheme".len()).any(|part| {
            part == b"Xcursor.theme:\tDesktopTheme"
        }));

        let previous = ResourceValues::from_appearance(&themed);
        let current = ResourceValues::from_appearance(&DesktopAppearance::new(1.0, ""));
        let withdrawn = merge_transition(&replaced, Some(&previous), &current);
        assert!(!withdrawn.windows(XCURSOR_THEME.len()).any(|part| part == XCURSOR_THEME));
        assert!(withdrawn.windows(b"Other:\tkept".len()).any(|part| part == b"Other:\tkept"));
    }
}
