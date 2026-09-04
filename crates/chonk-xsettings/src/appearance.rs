//! The typed layer: what this desktop actually wants to say, and the
//! unit conversions that say it.
//!
//! [`crate::format`] will publish any name and any value; this module
//! decides *which* names, and turns the things the desktop knows — a UI
//! scale of `2.0`, a theme called `NeXT` — into the integers the
//! XSETTINGS registry expects. It exists so that the arithmetic lives in
//! exactly one place with its factors written down, rather than at every
//! call site as a literal `1024` nobody can justify six months later.
//!
//! # The unit conversions, and why each factor is what it is
//!
//! **`Xft/DPI` is a DPI in 1024ths of a point.** XSETTINGS has no float
//! type — [`crate::format::SettingValue`] is integer, string or colour —
//! so Xft's own convention is to carry the DPI as a 16.16-style fixed
//! point value scaled by 1024. `96 * 1024 = 98304` is the ordinary
//! 96 DPI every toolkit assumes by default; a UI scale of 2.0 is
//! `96 * 2 * 1024 = 196608`. Publishing a bare `96` here is the classic
//! mistake, and it does not fail loudly: the client reads 96/1024 ≈ 0.09
//! DPI and renders text that is not so much small as absent.
//!
//! **`Gdk/WindowScalingFactor` is a whole number of backing-store
//! pixels per logical pixel.** Not a DPI and not a ratio — GDK
//! literally allocates a surface `n` times larger and draws into it, so
//! the value has to be an integer ≥ 1. This is the same quantity
//! `chonk-shell`'s `spawn::gtk_qt_scale_env` puts in `GDK_SCALE`, for
//! the same reason and with the same rounding.
//!
//! **`Gdk/UnscaledDPI` is the DPI to use for text *before* window
//! scaling is applied**, in the same 1024ths as `Xft/DPI`, and it is the
//! setting that keeps the two mechanisms above from multiplying
//! together. A GTK client that is handed both `Xft/DPI = 196608` and
//! `Gdk/WindowScalingFactor = 2` would otherwise draw 192 DPI text into
//! a surface that is itself doubled — text at four times size in a
//! window at two. Publishing `Gdk/UnscaledDPI = 98304` alongside says
//! explicitly "lay text out at 96 DPI and let the window scale carry the
//! factor of two", which is the arrangement the widely used `xsettingsd`
//! HiDPI recipe prescribes and the one GTK is written to expect. At a
//! fractional 1.5x, however, the integer window factor is 2 and the
//! pre-scale DPI must be 72: `72 * 2 = 144`, the requested effective
//! DPI. [`unscaled_xft_dpi_for_scale`] performs that division;
//! [`UNSCALED_XFT_DPI`] remains its 96-DPI answer at integral scales.
//!
//! **`Gtk/CursorThemeSize` is a size in pixels**, and it is deliberately
//! the same number this session already puts in `XCURSOR_SIZE` — 24 px
//! at 1x, Xcursor's own conventional base size, times the scale. See
//! `chonk-shell`'s `startup::xcursor_size_for`, which computes it
//! identically. GTK hands this value directly to Xcursor; unlike text
//! DPI it is not subsequently multiplied by `Gdk/WindowScalingFactor`.
//! Two mechanisms disagreeing about the pointer size is a visible glitch
//! every time the pointer crosses a window border, so the base is stated
//! in both places and [`DesktopAppearance::cursor_size`] exists for a
//! caller who needs to override it anyway.
//!
//! **`Net/ThemeName` and `Gtk/ThemeName` are the same string.**
//! `Net/ThemeName` is the name in the XSETTINGS registry; `Gtk/ThemeName`
//! is the one GTK actually reads. Publishing only the registry name is a
//! very common way to end up with a desktop that changes nothing, so
//! this module always publishes both. The same is true of the icon
//! theme pair.

use crate::format::Settings;

/// Every setting name this crate knows how to publish.
///
/// Constants rather than string literals at the call sites, because
/// [`Settings::set`](crate::format::Settings::set) refuses — and only
/// logs — a name it cannot carry, so a typo would be a setting that
/// silently never appears. A `const` typo is a compile error.
pub mod keys {
    /// Font DPI in 1024ths of a point. Read by essentially everything
    /// that draws text through Xft, which on X11 means GTK, Qt,
    /// Firefox, and the toolkits that copied them.
    pub const XFT_DPI: &str = "Xft/DPI";
    /// Font DPI before window scaling, in the same 1024ths. See the
    /// module documentation for why publishing this matters.
    pub const GDK_UNSCALED_DPI: &str = "Gdk/UnscaledDPI";
    /// Whole-number backing-store scale factor for GDK surfaces.
    pub const GDK_WINDOW_SCALING_FACTOR: &str = "Gdk/WindowScalingFactor";
    /// Pointer cursor size in pixels.
    pub const GTK_CURSOR_THEME_SIZE: &str = "Gtk/CursorThemeSize";
    /// Pointer cursor theme name, as Xcursor would resolve it.
    pub const GTK_CURSOR_THEME_NAME: &str = "Gtk/CursorThemeName";
    /// Widget theme name, XSETTINGS registry spelling.
    pub const NET_THEME_NAME: &str = "Net/ThemeName";
    /// Widget theme name, the spelling GTK reads.
    pub const GTK_THEME_NAME: &str = "Gtk/ThemeName";
    /// Icon theme name, XSETTINGS registry spelling.
    pub const NET_ICON_THEME_NAME: &str = "Net/IconThemeName";
    /// Icon theme name, the spelling GTK reads.
    pub const GTK_ICON_THEME_NAME: &str = "Gtk/IconThemeName";
    /// Default UI font, as a Pango font description ("Sans 10").
    pub const GTK_FONT_NAME: &str = "Gtk/FontName";
}

/// The DPI every toolkit assumes when nobody tells it otherwise, and
/// therefore the DPI a UI scale of 1.0 has to reproduce exactly.
pub const BASE_DPI: f32 = 96.0;

/// Fixed-point denominator for `Xft/DPI` and `Gdk/UnscaledDPI`: the
/// value on the wire is a DPI multiplied by this. See the module
/// documentation.
pub const XFT_DPI_UNITS_PER_POINT: i32 = 1024;

/// 96 DPI in 1024ths: the `Gdk/UnscaledDPI` value at every integral
/// scale, and the harmless fallback for an unusable scale.
pub const UNSCALED_XFT_DPI: i32 = 98_304;

/// Xcursor's conventional 1x pointer size in pixels — the same base
/// `chonk-shell`'s `startup::xcursor_size_for` uses.
pub const BASE_CURSOR_SIZE_PX: f32 = 24.0;

/// The smallest UI scale [`sanitize_ui_scale`] will pass through.
pub const MIN_UI_SCALE: f32 = 0.25;

/// The largest UI scale [`sanitize_ui_scale`] will pass through.
pub const MAX_UI_SCALE: f32 = 8.0;

/// Coerces a scale that came from a configuration file into one the
/// conversions below can be trusted with.
///
/// This desktop reads its scale from a user-editable config, so `0`,
/// `-1` and `NaN` are all things that can reach this crate, and each of
/// them poisons a different conversion: a scale of zero publishes
/// `Xft/DPI = 0` and every application on the display renders nothing
/// legible, and a NaN makes the `as i32` cast produce zero by the same
/// route without even looking wrong in the source. Clamping here means
/// there is one place to reason about it instead of three, and means the
/// published property is always something a client can act on.
///
/// A non-finite or non-positive scale becomes `1.0` rather than
/// [`MIN_UI_SCALE`]: those inputs are not "the user asked for something
/// small", they are "there is no usable answer", and 1.0 is the only
/// value that is certainly harmless.
pub fn sanitize_ui_scale(scale: f32) -> f32 {
    if !scale.is_finite() || scale <= 0.0 {
        return 1.0;
    }
    scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
}

/// The `Xft/DPI` value for a UI scale: `96 * scale`, in 1024ths of a
/// point.
pub fn xft_dpi_for_scale(scale: f32) -> i32 {
    let dpi = BASE_DPI * sanitize_ui_scale(scale);
    (dpi * XFT_DPI_UNITS_PER_POINT as f32).round() as i32
}

/// The `Gdk/WindowScalingFactor` for a UI scale.
///
/// Rounded to a whole number and floored at 1, because GDK multiplies a
/// surface's pixel dimensions by it — there is no such thing as a 1.5x
/// backing store, and a factor of 0 is a zero-sized surface. A
/// fractional scale therefore loses its remainder here; the remainder is
/// carried by the DPI settings instead, exactly as `chonk-shell`'s
/// `spawn::gtk_qt_scale_env` splits `GDK_SCALE` from `GDK_DPI_SCALE`.
pub fn window_scaling_factor_for_scale(scale: f32) -> i32 {
    (sanitize_ui_scale(scale).round() as i32).max(1)
}

/// The `Gdk/UnscaledDPI` value for a UI scale.
///
/// GDK uses this DPI for text and independently multiplies surfaces by
/// [`window_scaling_factor_for_scale`]. Dividing the requested
/// [`xft_dpi_for_scale`] by that integer factor preserves a fractional
/// desktop scale instead of rounding it away. Both wire values are
/// integers; rounding chooses the closest representable effective DPI.
pub fn unscaled_xft_dpi_for_scale(scale: f32) -> i32 {
    let factor = window_scaling_factor_for_scale(scale);
    (xft_dpi_for_scale(scale) as f32 / factor as f32).round() as i32
}

/// The pointer cursor size in pixels for a UI scale: 24 px at 1x.
///
/// Floored at 1 for the same reason `chonk-shell` floors its own: a
/// hand-edited scale must not produce a zero-pixel cursor, which is not
/// a small pointer but no pointer at all.
pub fn cursor_size_for_scale(scale: f32) -> i32 {
    ((BASE_CURSOR_SIZE_PX * sanitize_ui_scale(scale)).round() as i32).max(1)
}

/// Everything this desktop publishes about its own appearance, in the
/// terms the desktop thinks in.
///
/// The caller says "the UI scale is 2.0 and the theme is NeXT"; this
/// type turns that into the eight or so XSETTINGS keys that add up to
/// the same statement. It is a plain struct with public fields rather
/// than a builder with private ones because it is a *description*, not a
/// protocol: the shell will construct one from its config every time
/// something changes and hand the whole thing over, and
/// [`apply_to`](DesktopAppearance::apply_to) is idempotent so that doing
/// so costs nothing when nothing moved.
///
/// The `Option` fields mean "this desktop has no opinion". They are
/// removed from the map rather than published as an empty string —
/// an empty `Gtk/ThemeName` is not "use your default", it is a theme
/// named "" that GTK will look for and fail to find. Note the caveat on
/// [`Settings::remove`](crate::format::Settings::remove) about already
/// running clients: dropping an opinion reaches new processes reliably
/// and old ones patchily, which is a reason to prefer stating a value.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopAppearance {
    /// The desktop's UI scale factor — the same number the shell scales
    /// its own chrome by. `1.0` is 96 DPI and unscaled surfaces.
    /// Sanitised by [`sanitize_ui_scale`] on the way out, so a value
    /// from a hand-edited config is safe to put here directly.
    pub ui_scale: f32,
    /// Widget theme name, published as both `Net/ThemeName` and
    /// `Gtk/ThemeName`.
    pub theme_name: String,
    /// Icon theme name, published as both `Net/IconThemeName` and
    /// `Gtk/IconThemeName`.
    pub icon_theme_name: Option<String>,
    /// Xcursor theme name for `Gtk/CursorThemeName`.
    pub cursor_theme_name: Option<String>,
    /// Pointer size in pixels. `None` derives it from
    /// [`ui_scale`](Self::ui_scale) via [`cursor_size_for_scale`], which
    /// is what keeps it consistent with the `XCURSOR_SIZE` the session
    /// hands to the processes it launches. GTK passes the XSETTING
    /// straight to Xcursor, so the integer window scale is deliberately
    /// not divided out here. Set it explicitly if a particular cursor
    /// theme needs a different pixel size.
    pub cursor_size: Option<u32>,
    /// Default UI font as a Pango description, e.g. `"Sans 10"`, for
    /// `Gtk/FontName`. Left `None` by default: this desktop's own
    /// chrome does not use a Pango font stack, so imposing one on every
    /// GTK application would be an opinion it has not earned.
    pub font_name: Option<String>,
}

impl Default for DesktopAppearance {
    /// An unscaled desktop with no theme name at all.
    ///
    /// The empty `theme_name` is meaningful:
    /// [`apply_to`](DesktopAppearance::apply_to) treats it exactly like
    /// a `None` option and publishes no theme keys, so the default is
    /// "say the true things about DPI and say nothing about taste".
    fn default() -> Self {
        Self {
            ui_scale: 1.0,
            theme_name: String::new(),
            icon_theme_name: None,
            cursor_theme_name: None,
            cursor_size: None,
            font_name: None,
        }
    }
}

impl DesktopAppearance {
    /// A scale and a theme name, the two things this desktop always
    /// knows.
    pub fn new(ui_scale: f32, theme_name: impl Into<String>) -> Self {
        Self {
            ui_scale,
            theme_name: theme_name.into(),
            ..Self::default()
        }
    }

    /// Sets the icon theme published as `Net/IconThemeName` and
    /// `Gtk/IconThemeName`.
    #[must_use]
    pub fn with_icon_theme(mut self, name: impl Into<String>) -> Self {
        self.icon_theme_name = Some(name.into());
        self
    }

    /// Sets the Xcursor theme published as `Gtk/CursorThemeName`.
    #[must_use]
    pub fn with_cursor_theme(mut self, name: impl Into<String>) -> Self {
        self.cursor_theme_name = Some(name.into());
        self
    }

    /// Overrides the derived pointer size. See
    /// [`cursor_size`](Self::cursor_size).
    #[must_use]
    pub fn with_cursor_size(mut self, pixels: u32) -> Self {
        self.cursor_size = Some(pixels);
        self
    }

    /// Sets the default UI font published as `Gtk/FontName`.
    #[must_use]
    pub fn with_font_name(mut self, description: impl Into<String>) -> Self {
        self.font_name = Some(description.into());
        self
    }

    /// The pointer size this appearance implies, override or derived.
    pub fn effective_cursor_size(&self) -> i32 {
        match self.cursor_size {
            // Clamped into `i32` rather than cast: the field is a `u32`
            // so a caller can hand over a number no cursor could ever
            // be, and a wrapping cast would turn it negative — which
            // the format would happily publish.
            Some(pixels) => pixels.min(i32::MAX as u32).max(1) as i32,
            None => cursor_size_for_scale(self.ui_scale),
        }
    }

    /// Writes this appearance into a settings map, returning whether
    /// anything actually changed.
    ///
    /// Idempotent, and that is load-bearing rather than a nicety: the
    /// return value is what
    /// [`XSettingsManager::publish_appearance`](crate::manager::XSettingsManager::publish_appearance)
    /// uses to decide whether to touch the X server at all, so re-applying
    /// an unchanged appearance on every config reload costs one map walk
    /// and no round trip, no serial bump, and no `PropertyNotify` waking
    /// every client on the display.
    ///
    /// Only the keys this type describes are touched. Anything else the
    /// caller put in the map is left alone, so a shell that wants to
    /// publish an extra setting of its own can, and this method will not
    /// clobber it.
    pub fn apply_to(&self, settings: &mut Settings) -> bool {
        let mut changed = false;

        changed |= settings.set(keys::XFT_DPI, xft_dpi_for_scale(self.ui_scale));
        changed |= settings.set(keys::GDK_UNSCALED_DPI, unscaled_xft_dpi_for_scale(self.ui_scale));
        changed |= settings.set(
            keys::GDK_WINDOW_SCALING_FACTOR,
            window_scaling_factor_for_scale(self.ui_scale),
        );
        changed |= settings.set(keys::GTK_CURSOR_THEME_SIZE, self.effective_cursor_size());

        changed |= set_or_remove(settings, keys::NET_THEME_NAME, non_empty(&self.theme_name));
        changed |= set_or_remove(settings, keys::GTK_THEME_NAME, non_empty(&self.theme_name));
        changed |= set_or_remove(
            settings,
            keys::NET_ICON_THEME_NAME,
            self.icon_theme_name.as_deref().and_then(non_empty),
        );
        changed |= set_or_remove(
            settings,
            keys::GTK_ICON_THEME_NAME,
            self.icon_theme_name.as_deref().and_then(non_empty),
        );
        changed |= set_or_remove(
            settings,
            keys::GTK_CURSOR_THEME_NAME,
            self.cursor_theme_name.as_deref().and_then(non_empty),
        );
        changed |= set_or_remove(
            settings,
            keys::GTK_FONT_NAME,
            self.font_name.as_deref().and_then(non_empty),
        );

        changed
    }

    /// A fresh settings map containing exactly this appearance.
    ///
    /// The serial starts from zero and climbs once per key written, so
    /// this is for constructing a map, not for updating a live one —
    /// publishing a map built this way over one a client has already
    /// read would move the serial *backwards*. Use
    /// [`apply_to`](Self::apply_to) against the manager's own map for
    /// that; [`XSettingsManager`](crate::manager::XSettingsManager) does.
    pub fn to_settings(&self) -> Settings {
        let mut settings = Settings::new();
        self.apply_to(&mut settings);
        settings
    }
}

/// `Some(s)` for a non-empty string, `None` otherwise — the conversion
/// that makes `""` and "no opinion" the same thing everywhere in
/// [`DesktopAppearance::apply_to`].
fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

/// Publishes a value or withdraws the key, returning whether the map
/// changed either way.
fn set_or_remove(settings: &mut Settings, name: &str, value: Option<&str>) -> bool {
    match value {
        Some(value) => settings.set(name, value),
        None => settings.remove(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SettingValue;

    fn integer(settings: &Settings, name: &str) -> Option<i32> {
        match settings.get(name) {
            Some(SettingValue::Integer(value)) => Some(*value),
            _ => None,
        }
    }

    fn string<'a>(settings: &'a Settings, name: &str) -> Option<&'a str> {
        match settings.get(name) {
            Some(SettingValue::String(value)) => Some(value.as_str()),
            _ => None,
        }
    }

    #[test]
    fn xft_dpi_is_the_dpi_in_1024ths_of_a_point() {
        assert_eq!(xft_dpi_for_scale(1.0), 96 * 1024);
        assert_eq!(xft_dpi_for_scale(1.0), UNSCALED_XFT_DPI);
        assert_eq!(xft_dpi_for_scale(2.0), 96 * 2 * 1024);
        assert_eq!(xft_dpi_for_scale(2.0), 196_608);
        assert_eq!(xft_dpi_for_scale(1.5), 147_456);
        // 96 * 1.25 = 120 DPI, which is a whole number of points and so
        // needs no rounding to survive the trip.
        assert_eq!(xft_dpi_for_scale(1.25), 120 * 1024);
    }

    #[test]
    fn the_window_scaling_factor_is_a_whole_number_of_at_least_one() {
        assert_eq!(window_scaling_factor_for_scale(1.0), 1);
        assert_eq!(window_scaling_factor_for_scale(1.4), 1);
        assert_eq!(window_scaling_factor_for_scale(1.5), 2);
        assert_eq!(window_scaling_factor_for_scale(2.0), 2);
        assert_eq!(window_scaling_factor_for_scale(3.0), 3);
        // Below 0.5 the rounding would reach zero, which GDK cannot use.
        assert_eq!(window_scaling_factor_for_scale(0.25), 1);
    }

    #[test]
    fn the_cursor_size_matches_the_sessions_own_xcursor_size_convention() {
        // Deliberately the same numbers as `chonk-shell`'s
        // `startup::xcursor_size_for`; the two mechanisms describe the
        // same pointer.
        assert_eq!(cursor_size_for_scale(1.0), 24);
        assert_eq!(cursor_size_for_scale(2.0), 48);
        assert_eq!(cursor_size_for_scale(1.5), 36);
        assert_eq!(cursor_size_for_scale(0.25), 6);
    }

    #[test]
    fn a_nonsensical_scale_cannot_reach_the_conversions() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(sanitize_ui_scale(scale), 1.0, "scale {scale} should fall back to 1.0");
            assert_eq!(xft_dpi_for_scale(scale), UNSCALED_XFT_DPI);
            assert_eq!(window_scaling_factor_for_scale(scale), 1);
            assert_eq!(unscaled_xft_dpi_for_scale(scale), UNSCALED_XFT_DPI);
            assert_eq!(cursor_size_for_scale(scale), 24);
        }
        // Absurd but positive values clamp rather than fall back.
        assert_eq!(sanitize_ui_scale(1_000.0), MAX_UI_SCALE);
        assert_eq!(sanitize_ui_scale(0.001), MIN_UI_SCALE);
    }

    #[test]
    fn unscaled_dpi_times_window_scale_reproduces_the_requested_dpi() {
        // GDK substitutes this value for Xft/DPI before independently
        // applying its integer window scale. The two settings must put
        // the fractional remainder back together rather than round the
        // requested desktop scale away.
        for scale in [1.0f32, 1.25, 1.5, 2.0, 2.5, 3.0] {
            let settings = DesktopAppearance::new(scale, "NeXT").to_settings();
            let unscaled = integer(&settings, keys::GDK_UNSCALED_DPI).unwrap();
            let factor = integer(&settings, keys::GDK_WINDOW_SCALING_FACTOR).unwrap();
            let requested = integer(&settings, keys::XFT_DPI).unwrap();
            assert!((unscaled * factor - requested).abs() <= 1, "scale {scale}");
        }
    }

    #[test]
    fn a_theme_name_is_published_under_both_spellings() {
        let settings = DesktopAppearance::new(1.0, "NeXT").to_settings();
        assert_eq!(string(&settings, keys::NET_THEME_NAME), Some("NeXT"));
        assert_eq!(
            string(&settings, keys::GTK_THEME_NAME),
            Some("NeXT"),
            "GTK reads Gtk/ThemeName, not the registry's Net/ThemeName"
        );
    }

    #[test]
    fn an_appearance_with_no_opinions_publishes_only_the_numbers() {
        let settings = DesktopAppearance::default().to_settings();
        assert_eq!(
            settings.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            [
                keys::GDK_UNSCALED_DPI,
                keys::GDK_WINDOW_SCALING_FACTOR,
                keys::GTK_CURSOR_THEME_SIZE,
                keys::XFT_DPI,
            ]
        );
    }

    #[test]
    fn every_optional_key_appears_when_it_is_set() {
        let settings = DesktopAppearance::new(2.0, "NeXT")
            .with_icon_theme("NeXT-icons")
            .with_cursor_theme("Adwaita")
            .with_font_name("Sans 10")
            .to_settings();

        assert_eq!(string(&settings, keys::NET_ICON_THEME_NAME), Some("NeXT-icons"));
        assert_eq!(string(&settings, keys::GTK_ICON_THEME_NAME), Some("NeXT-icons"));
        assert_eq!(string(&settings, keys::GTK_CURSOR_THEME_NAME), Some("Adwaita"));
        assert_eq!(string(&settings, keys::GTK_FONT_NAME), Some("Sans 10"));
        assert_eq!(integer(&settings, keys::GTK_CURSOR_THEME_SIZE), Some(48));
        assert_eq!(settings.len(), 10);
    }

    #[test]
    fn an_empty_string_is_no_opinion_not_a_theme_called_nothing() {
        let mut settings = DesktopAppearance::new(1.0, "NeXT").to_settings();
        assert!(settings.get(keys::GTK_THEME_NAME).is_some());

        let bare = DesktopAppearance {
            theme_name: String::new(),
            icon_theme_name: Some(String::new()),
            ..DesktopAppearance::default()
        };
        assert!(bare.apply_to(&mut settings), "withdrawing the theme is a change");
        assert_eq!(settings.get(keys::NET_THEME_NAME), None);
        assert_eq!(settings.get(keys::GTK_THEME_NAME), None);
        assert_eq!(settings.get(keys::NET_ICON_THEME_NAME), None);
    }

    #[test]
    fn applying_the_same_appearance_twice_changes_nothing_the_second_time() {
        let appearance = DesktopAppearance::new(2.0, "NeXT").with_cursor_theme("Adwaita");
        let mut settings = Settings::new();

        assert!(appearance.apply_to(&mut settings));
        let serial = settings.serial();
        let bytes = settings.serialize();

        assert!(
            !appearance.apply_to(&mut settings),
            "re-applying an unchanged appearance must not report a change"
        );
        assert_eq!(settings.serial(), serial, "and must not bump the serial");
        assert_eq!(settings.serialize(), bytes, "and must not alter the property");
    }

    #[test]
    fn a_scale_change_moves_exactly_the_settings_that_depend_on_the_scale() {
        let mut settings = DesktopAppearance::new(1.0, "NeXT").to_settings();
        let theme_serial = settings.last_change_serial(keys::GTK_THEME_NAME);

        assert!(DesktopAppearance::new(2.0, "NeXT").apply_to(&mut settings));
        assert_eq!(integer(&settings, keys::XFT_DPI), Some(196_608));
        assert_eq!(integer(&settings, keys::GDK_WINDOW_SCALING_FACTOR), Some(2));
        assert_eq!(integer(&settings, keys::GTK_CURSOR_THEME_SIZE), Some(48));
        assert_eq!(
            settings.last_change_serial(keys::GTK_THEME_NAME),
            theme_serial,
            "the theme did not change, so no client should be told it did"
        );
        assert_eq!(
            settings.last_change_serial(keys::GDK_UNSCALED_DPI),
            Some(2),
            "integral scales all reduce to the same 96-DPI pre-scale value"
        );
    }

    #[test]
    fn an_explicit_cursor_size_overrides_the_derived_one() {
        let appearance = DesktopAppearance::new(2.0, "NeXT").with_cursor_size(64);
        assert_eq!(appearance.effective_cursor_size(), 64);
        let settings = appearance.to_settings();
        assert_eq!(integer(&settings, keys::GTK_CURSOR_THEME_SIZE), Some(64));
    }

    #[test]
    fn an_absurd_explicit_cursor_size_stays_positive() {
        // A `u32` can hold values `i32` cannot, and a wrapping cast
        // would publish a negative pixel count.
        let appearance = DesktopAppearance::new(1.0, "NeXT").with_cursor_size(u32::MAX);
        assert_eq!(appearance.effective_cursor_size(), i32::MAX);
        let appearance = DesktopAppearance::new(1.0, "NeXT").with_cursor_size(0);
        assert_eq!(appearance.effective_cursor_size(), 1);
    }

    #[test]
    fn settings_the_caller_added_are_left_alone() {
        let mut settings = Settings::new();
        settings.set("Net/CursorBlink", 1);
        DesktopAppearance::new(1.0, "NeXT").apply_to(&mut settings);
        assert_eq!(integer(&settings, "Net/CursorBlink"), Some(1));
    }
}
