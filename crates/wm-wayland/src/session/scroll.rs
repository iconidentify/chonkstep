//! Natural scrolling per device class. libinput offers natural scrolling on
//! wheel mice as well as touchpads, so capability alone cannot decide which
//! setting a device gets: a touchpad preference that reached a mouse would
//! invert its wheel. The class decides, and removing a key on reload
//! restores the device's own default rather than keeping the last value.

use wm_core::{PointerConfig, ScrollClass};

pub(super) trait ScrollDevice {
    type Error;
    /// libinput offers tap configuration only on touchpads, which is the
    /// same test the tap-to-click setter already relies on.
    fn is_touchpad(&self) -> bool;
    fn has_natural_scroll(&self) -> bool;
    fn default_natural_scroll(&self) -> bool;
    fn natural_scroll(&self) -> bool;
    fn set_natural_scroll(&mut self, enabled: bool) -> Result<(), Self::Error>;
}

impl ScrollDevice for smithay::reexports::input::Device {
    type Error = smithay::reexports::input::DeviceConfigError;
    fn is_touchpad(&self) -> bool {
        self.config_tap_finger_count() > 0
    }
    fn has_natural_scroll(&self) -> bool {
        self.config_scroll_has_natural_scroll()
    }
    fn default_natural_scroll(&self) -> bool {
        self.config_scroll_default_natural_scroll_enabled()
    }
    fn natural_scroll(&self) -> bool {
        self.config_scroll_natural_scroll_enabled()
    }
    fn set_natural_scroll(&mut self, enabled: bool) -> Result<(), Self::Error> {
        self.config_scroll_set_natural_scroll_enabled(enabled)
    }
}

/// The scroll settings that apply to `device`.
pub(super) fn class<'a, D: ScrollDevice>(device: &D, config: &'a PointerConfig) -> &'a ScrollClass {
    if device.is_touchpad() {
        &config.touchpad
    } else {
        &config.pointer
    }
}

/// What applying the class's setting did, for the per-device log line.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Unchanged,
    Changed,
    /// The class asks for natural scrolling and the device has none.
    Unsupported,
}

pub(super) fn configure<D: ScrollDevice>(device: &mut D, config: &PointerConfig) -> Result<Outcome, D::Error> {
    let requested = class(device, config).natural_scroll;
    if !device.has_natural_scroll() {
        return Ok(if requested.is_some() { Outcome::Unsupported } else { Outcome::Unchanged });
    }
    let enabled = requested.unwrap_or_else(|| device.default_natural_scroll());
    if enabled == device.natural_scroll() {
        return Ok(Outcome::Unchanged);
    }
    device.set_natural_scroll(enabled)?;
    Ok(Outcome::Changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Device {
        touchpad: bool,
        supported: bool,
        default: bool,
        enabled: bool,
        writes: usize,
    }

    impl Device {
        fn touchpad() -> Self {
            Device { touchpad: true, supported: true, default: false, enabled: false, writes: 0 }
        }

        fn mouse() -> Self {
            Device { touchpad: false, ..Device::touchpad() }
        }
    }

    impl ScrollDevice for Device {
        type Error = ();
        fn is_touchpad(&self) -> bool {
            self.touchpad
        }
        fn has_natural_scroll(&self) -> bool {
            self.supported
        }
        fn default_natural_scroll(&self) -> bool {
            self.default
        }
        fn natural_scroll(&self) -> bool {
            self.enabled
        }
        fn set_natural_scroll(&mut self, enabled: bool) -> Result<(), ()> {
            self.writes += 1;
            self.enabled = enabled;
            Ok(())
        }
    }

    fn natural(class: Option<bool>) -> ScrollClass {
        ScrollClass { natural_scroll: class, scroll_factor: None }
    }

    /// Omarchy's `input.lua` sets natural scrolling in its touchpad table
    /// only.
    fn omarchy() -> PointerConfig {
        PointerConfig {
            touchpad: ScrollClass { natural_scroll: Some(true), scroll_factor: Some(0.4) },
            ..PointerConfig::default()
        }
    }

    #[test]
    fn a_touchpad_setting_reaches_touchpads_and_never_a_wheel() {
        let mut touchpad = Device::touchpad();
        let mut mouse = Device::mouse();
        assert_eq!(configure(&mut touchpad, &omarchy()), Ok(Outcome::Changed));
        assert_eq!(configure(&mut mouse, &omarchy()), Ok(Outcome::Unchanged));
        assert!(touchpad.enabled);
        assert!(!mouse.enabled, "Omarchy's touchpad table must not invert a mouse wheel");
        assert_eq!(mouse.writes, 0);
    }

    #[test]
    fn a_mouse_setting_reaches_wheels_and_never_a_touchpad() {
        let config = PointerConfig { pointer: natural(Some(true)), ..PointerConfig::default() };
        let mut touchpad = Device::touchpad();
        let mut mouse = Device::mouse();
        configure(&mut touchpad, &config).unwrap();
        configure(&mut mouse, &config).unwrap();
        assert!(mouse.enabled);
        assert!(!touchpad.enabled);
    }

    #[test]
    fn removing_the_setting_on_reload_restores_the_device_default() {
        for default in [false, true] {
            let mut touchpad = Device { default, enabled: default, ..Device::touchpad() };
            let explicit = PointerConfig { touchpad: natural(Some(!default)), ..PointerConfig::default() };
            assert_eq!(configure(&mut touchpad, &explicit), Ok(Outcome::Changed));
            assert_eq!(touchpad.enabled, !default);
            assert_eq!(configure(&mut touchpad, &PointerConfig::default()), Ok(Outcome::Changed));
            assert_eq!(touchpad.enabled, default);
            assert_eq!(
                configure(&mut touchpad, &PointerConfig::default()),
                Ok(Outcome::Unchanged),
                "an unchanged reload writes nothing"
            );
        }
    }

    #[test]
    fn a_device_without_natural_scrolling_is_reported_only_when_its_class_asks() {
        let mut keyboard = Device { supported: false, ..Device::mouse() };
        assert_eq!(configure(&mut keyboard, &omarchy()), Ok(Outcome::Unchanged));
        let mut trackball = Device { supported: false, ..Device::mouse() };
        let config = PointerConfig { pointer: natural(Some(true)), ..PointerConfig::default() };
        assert_eq!(configure(&mut trackball, &config), Ok(Outcome::Unsupported));
        assert_eq!(trackball.writes, 0);
    }
}
