//! Per-device rules, matched against libinput's device name exactly.
//!
//! A rule's settings are laid over the class and desktop-wide ones for the
//! one device it names, and its `enabled` decides libinput's send-events
//! mode. Both are applied as each device appears, so a rule holds across
//! hotplug, docking and resume without anything re-sending it. See
//! `input::devices` for how a live request joins the configured rules.

use std::borrow::Cow;
use std::fmt::Debug;

use smithay::reexports::input::{self as libinput_crate, DeviceCapability, DeviceConfigError, SendEventsMode};
use wm_core::PointerConfig;

pub(super) trait RuledDevice {
    type Error;
    fn has_keys(&self) -> bool;
    fn can_disable(&self) -> bool;
    fn disabled(&self) -> bool;
    fn set_disabled(&mut self, disabled: bool) -> Result<(), Self::Error>;
}

impl RuledDevice for libinput_crate::Device {
    type Error = DeviceConfigError;
    fn has_keys(&self) -> bool {
        self.has_capability(DeviceCapability::Keyboard)
    }
    fn can_disable(&self) -> bool {
        self.config_send_events_modes().contains(SendEventsMode::DISABLED)
    }
    fn disabled(&self) -> bool {
        self.config_send_events_mode().contains(SendEventsMode::DISABLED)
    }
    fn set_disabled(&mut self, disabled: bool) -> Result<(), DeviceConfigError> {
        self.config_send_events_set_mode(if disabled { SendEventsMode::DISABLED } else { SendEventsMode::ENABLED })
    }
}

/// The configuration as it applies to the device called `name`: its rule's
/// settings over everything else, or the configuration untouched when no
/// rule names it.
pub(super) fn for_device<'a>(config: &'a PointerConfig, name: &str) -> Cow<'a, PointerConfig> {
    let Some(rule) = config.devices.iter().find(|rule| rule.name == name) else {
        return Cow::Borrowed(config);
    };
    let mut applied = config.clone();
    applied.sensitivity = rule.sensitivity.or(config.sensitivity);
    applied.accel_profile = rule.accel_profile.clone().or_else(|| config.accel_profile.clone());
    applied.left_handed = rule.left_handed.or(config.left_handed);
    applied.tap_to_click = rule.tap_to_click.or(config.tap_to_click);
    // A device is in one class or the other, so setting both reaches it
    // whichever class that is.
    if let Some(natural) = rule.natural_scroll {
        applied.pointer.natural_scroll = Some(natural);
        applied.touchpad.natural_scroll = Some(natural);
    }
    Cow::Owned(applied)
}

/// Sends or stops the device's events by its rule, adding a refusal to
/// `rejected`. A device with keys is never stopped; a device whose rule is
/// removed, or says `enabled = true`, sends events again.
pub(super) fn configure_send_events<D: RuledDevice>(device: &mut D, name: &str, config: &PointerConfig, rejected: &mut Vec<String>)
where
    D::Error: Debug,
{
    let mut disable = config.devices.iter().any(|rule| rule.name == name && rule.enabled == Some(false));
    if disable && device.has_keys() {
        rejected.push("enabled: a device with keys is never disabled".to_string());
        disable = false;
    } else if disable && !device.can_disable() {
        rejected.push("enabled: unsupported".to_string());
        return;
    }
    if device.disabled() != disable {
        if let Err(error) = device.set_disabled(disable) {
            rejected.push(format!("enabled: {error:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::DeviceRule;

    struct Device {
        keys: bool,
        can_disable: bool,
        disabled: bool,
        writes: usize,
    }

    impl Device {
        fn pointer() -> Self {
            Device { keys: false, can_disable: true, disabled: false, writes: 0 }
        }
    }

    impl RuledDevice for Device {
        type Error = ();
        fn has_keys(&self) -> bool {
            self.keys
        }
        fn can_disable(&self) -> bool {
            self.can_disable
        }
        fn disabled(&self) -> bool {
            self.disabled
        }
        fn set_disabled(&mut self, disabled: bool) -> Result<(), ()> {
            self.writes += 1;
            self.disabled = disabled;
            Ok(())
        }
    }

    const TOUCHPAD: &str = "SynPS/2 Synaptics TouchPad";
    const TRACKBALL: &str = "Kensington Expert Mouse";

    fn config(rules: Vec<DeviceRule>) -> PointerConfig {
        PointerConfig { sensitivity: Some(0.2), tap_to_click: Some(false), devices: rules, ..PointerConfig::default() }
    }

    fn send_events(device: &mut Device, name: &str, config: &PointerConfig) -> Vec<String> {
        let mut rejected = Vec::new();
        configure_send_events(device, name, config, &mut rejected);
        rejected
    }

    #[test]
    fn a_rule_reaches_only_the_device_it_names() {
        let rules = config(vec![DeviceRule {
            name: TOUCHPAD.into(),
            enabled: Some(false),
            sensitivity: Some(-0.5),
            natural_scroll: Some(true),
            ..DeviceRule::default()
        }]);
        let touchpad = for_device(&rules, TOUCHPAD);
        assert_eq!(touchpad.sensitivity, Some(-0.5));
        assert_eq!(touchpad.touchpad.natural_scroll, Some(true));
        assert_eq!(touchpad.tap_to_click, Some(false), "what the rule leaves unset still comes from the configuration");
        assert!(matches!(for_device(&rules, TRACKBALL), Cow::Borrowed(_)), "no rule, no copy");
        // Exact names only: a prefix of a real name is some other device.
        assert!(matches!(for_device(&rules, "SynPS/2 Synaptics"), Cow::Borrowed(_)));

        let mut touchpad = Device::pointer();
        let mut trackball = Device::pointer();
        assert!(send_events(&mut touchpad, TOUCHPAD, &rules).is_empty());
        assert!(send_events(&mut trackball, TRACKBALL, &rules).is_empty());
        assert!(touchpad.disabled);
        assert!(!trackball.disabled);
        assert_eq!(trackball.writes, 0);
    }

    #[test]
    fn removing_the_rule_sends_events_again_and_an_unchanged_one_writes_nothing() {
        let off = config(vec![DeviceRule { name: TOUCHPAD.into(), enabled: Some(false), ..DeviceRule::default() }]);
        let mut touchpad = Device::pointer();
        send_events(&mut touchpad, TOUCHPAD, &off);
        send_events(&mut touchpad, TOUCHPAD, &off);
        assert_eq!(touchpad.writes, 1);
        send_events(&mut touchpad, TOUCHPAD, &config(Vec::new()));
        assert!(!touchpad.disabled);
        assert_eq!(touchpad.writes, 2);
    }

    #[test]
    fn a_device_with_keys_is_never_disabled_and_an_unsupported_one_is_named() {
        let off = |name: &str| config(vec![DeviceRule { name: name.into(), enabled: Some(false), ..DeviceRule::default() }]);
        let mut combined = Device { keys: true, ..Device::pointer() };
        assert_eq!(send_events(&mut combined, "Logitech USB Receiver", &off("Logitech USB Receiver")), [
            "enabled: a device with keys is never disabled"
        ]);
        assert!(!combined.disabled);
        let mut fixed = Device { can_disable: false, ..Device::pointer() };
        assert_eq!(send_events(&mut fixed, TRACKBALL, &off(TRACKBALL)), ["enabled: unsupported"]);
        assert_eq!(fixed.writes, 0);
    }
}
