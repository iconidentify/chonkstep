//! Apply typing suppression per device, retaining libinput's other palm
//! rejection mechanisms. Pointer capture temporarily suspends DWT for games
//! and other clients that need keyboard and touchpad motion together.

pub(super) trait TypingDevice {
    type Error;
    fn supported(&self) -> bool;
    fn default_enabled(&self) -> bool;
    fn enabled(&self) -> bool;
    fn set_enabled(&self, enabled: bool) -> Result<(), Self::Error>;
}

impl TypingDevice for smithay::reexports::input::Device {
    type Error = smithay::reexports::input::DeviceConfigError;
    fn supported(&self) -> bool {
        self.config_dwt_is_available()
    }
    fn default_enabled(&self) -> bool {
        self.config_dwt_default_enabled()
    }
    fn enabled(&self) -> bool {
        self.config_dwt_enabled()
    }
    fn set_enabled(&self, enabled: bool) -> Result<(), Self::Error> {
        self.config_dwt_set_enabled(enabled)
    }
}

pub(super) fn configure<D: TypingDevice>(
    device: &D,
    requested: Option<bool>,
    captured: bool,
) -> Result<(), D::Error> {
    if device.supported() {
        let enabled = !captured && requested.unwrap_or_else(|| device.default_enabled());
        if enabled != device.enabled() {
            device.set_enabled(enabled)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Touchpad {
        supported: bool,
        default: bool,
        enabled: Cell<bool>,
        changes: RefCell<Vec<bool>>,
    }

    impl Touchpad {
        fn new(default: bool) -> Self {
            Self {
                supported: true,
                default,
                enabled: Cell::new(default),
                changes: RefCell::default(),
            }
        }
    }

    impl TypingDevice for Touchpad {
        type Error = ();
        fn supported(&self) -> bool {
            self.supported
        }
        fn default_enabled(&self) -> bool {
            self.default
        }
        fn enabled(&self) -> bool {
            self.enabled.get()
        }
        fn set_enabled(&self, enabled: bool) -> Result<(), ()> {
            self.changes.borrow_mut().push(enabled);
            self.enabled.set(enabled);
            Ok(())
        }
    }

    #[test]
    fn capture_hotplug_and_reload_restore_each_devices_own_typing_policy() {
        for default in [false, true] {
            let device = Touchpad::new(default);
            // A newly connected/resumed touchpad must already allow motion
            // during a capture; no later mouse report can recover lost input.
            configure(&device, None, true).unwrap();
            assert!(!device.enabled.get());
            // Reloading an explicit typing preference cannot interrupt a game.
            configure(&device, Some(true), true).unwrap();
            assert!(!device.enabled.get());
            configure(&device, Some(true), false).unwrap();
            assert!(device.enabled.get());
            configure(&device, Some(false), false).unwrap();
            assert!(!device.enabled.get());
            // Removing the override restores the device default, not the
            // temporarily disabled value left over from gameplay.
            configure(&device, None, false).unwrap();
            assert_eq!(device.enabled.get(), default);
            let transitions = device.changes.borrow().len();
            for _ in 0..100 {
                configure(&device, None, false).unwrap();
            }
            assert_eq!(device.changes.borrow().len(), transitions);
        }
    }

    #[test]
    fn keyboards_and_mice_without_dwt_are_untouched() {
        let mut device = Touchpad::new(false);
        device.supported = false;
        for captured in [true, false] {
            configure(&device, Some(true), captured).unwrap();
        }
        assert!(device.changes.borrow().is_empty());
    }
}
