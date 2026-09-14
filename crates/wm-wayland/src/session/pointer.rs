//! The pointer and touchpad settings that came after the first set:
//! tap-and-drag, drag lock, the tap button map, multi-finger drag,
//! middle-button emulation, and the scroll method and button.
//!
//! Each follows the device-class split in [`super::scroll`]. The tapping
//! settings reach touchpads only, which are the only devices that tap, so
//! a touchpad key can never change a mouse. The scroll and middle-button
//! settings come from the class the device belongs to. And unlike the
//! older setters in `configure_libinput_device`, which leave a device on
//! whatever they last wrote, removing any of these keys on reload
//! restores the device's own default.

use std::ffi::{c_int, c_uint, c_void, CStr};
use std::fmt::Debug;
use std::sync::OnceLock;

use smithay::reexports::input::{self as libinput_crate, AsRaw, DeviceConfigError};
use wm_core::{MultiFingerDrag, PointerConfig, ScrollMethod, TapButtonMap};

use super::scroll::{self, Outcome, ScrollDevice};

/// libinput's drag-lock states, kept raw: the binding panics on the
/// sticky state (2), which it predates, and a newer libinput could
/// report sticky as a device default that a removed key must restore.
const DRAG_LOCK_DISABLED: u32 = 0;
const DRAG_LOCK_TIMEOUT: u32 = 1;

pub(super) trait PointerDevice: ScrollDevice {
    fn tap_drag(&self) -> bool;
    fn default_tap_drag(&self) -> bool;
    fn set_tap_drag(&mut self, enabled: bool) -> Result<(), Self::Error>;
    fn drag_lock(&self) -> u32;
    fn default_drag_lock(&self) -> u32;
    fn set_drag_lock(&mut self, state: u32) -> Result<(), Self::Error>;
    fn tap_button_map(&self) -> Option<TapButtonMap>;
    fn default_tap_button_map(&self) -> Option<TapButtonMap>;
    fn set_tap_button_map(&mut self, map: TapButtonMap) -> Result<(), Self::Error>;
    fn has_middle_emulation(&self) -> bool;
    fn middle_emulation(&self) -> bool;
    fn default_middle_emulation(&self) -> bool;
    fn set_middle_emulation(&mut self, enabled: bool) -> Result<(), Self::Error>;
    /// The methods the device offers. libinput never lists `NoScroll`,
    /// and accepts it from any device that scrolls at all.
    fn scroll_methods(&self) -> Vec<ScrollMethod>;
    fn scroll_method(&self) -> Option<ScrollMethod>;
    fn default_scroll_method(&self) -> Option<ScrollMethod>;
    fn set_scroll_method(&mut self, method: ScrollMethod) -> Result<(), Self::Error>;
    fn scroll_button(&self) -> u32;
    fn default_scroll_button(&self) -> u32;
    fn set_scroll_button(&mut self, button: u32) -> Result<(), Self::Error>;
    /// The most fingers multi-finger drag can use here, or `None` when
    /// the running libinput has no multi-finger drag at all.
    fn drag_fingers(&self) -> Option<u32>;
    fn multi_finger_drag(&self) -> MultiFingerDrag;
    fn default_multi_finger_drag(&self) -> MultiFingerDrag;
    fn set_multi_finger_drag(&mut self, drag: MultiFingerDrag) -> Result<(), Self::Error>;
}

/// Brings one setting to `requested`, or to the device default when the
/// configuration has none, writing only on a difference. A device without
/// the capability is never read or written, and is reported only when the
/// configuration asked for the setting.
fn settle<D: ?Sized, T: PartialEq, E>(
    device: &mut D,
    requested: Option<T>,
    supported: bool,
    default: fn(&D) -> T,
    current: fn(&D) -> T,
    set: fn(&mut D, T) -> Result<(), E>,
) -> Result<Outcome, E> {
    if !supported {
        return Ok(if requested.is_some() { Outcome::Unsupported } else { Outcome::Unchanged });
    }
    let wanted = requested.unwrap_or_else(|| default(device));
    if wanted == current(device) {
        return Ok(Outcome::Unchanged);
    }
    set(device, wanted)?;
    Ok(Outcome::Changed)
}

/// Applies every setting this module owns, adding what the device refused
/// to `rejected` in the per-device log line's `key: reason` form.
pub(super) fn configure<D: PointerDevice>(device: &mut D, config: &PointerConfig, rejected: &mut Vec<String>)
where
    D::Error: Debug,
{
    let class = scroll::class(device, config);
    let mut note = |key: &str, result: Result<Outcome, D::Error>| match result {
        Ok(Outcome::Unsupported) => rejected.push(format!("{key}: unsupported")),
        Ok(Outcome::Unchanged | Outcome::Changed) => {}
        Err(error) => rejected.push(format!("{key}: {error:?}")),
    };

    let supported = device.has_middle_emulation();
    note(
        "middle_button_emulation",
        settle(device, class.middle_button_emulation, supported, D::default_middle_emulation, D::middle_emulation, D::set_middle_emulation),
    );

    let methods = device.scroll_methods();
    let supported = match class.scroll_method {
        Some(ScrollMethod::NoScroll) | None => !methods.is_empty(),
        Some(method) => methods.contains(&method),
    };
    note(
        "scroll_method",
        settle(
            device,
            class.scroll_method.map(Some),
            supported,
            D::default_scroll_method,
            D::scroll_method,
            |device, method| method.map_or(Ok(()), |method| device.set_scroll_method(method)),
        ),
    );
    // Zero is Hyprland's spelling of "the device's own button".
    let supported = methods.contains(&ScrollMethod::OnButtonDown);
    note(
        "scroll_button",
        settle(
            device,
            class.scroll_button.filter(|button| *button != 0),
            supported,
            D::default_scroll_button,
            D::scroll_button,
            D::set_scroll_button,
        ),
    );

    if !device.is_touchpad() {
        return;
    }
    note("tap_and_drag", settle(device, config.tap_and_drag, true, D::default_tap_drag, D::tap_drag, D::set_tap_drag));
    let lock = config.drag_lock.map(|enabled| if enabled { DRAG_LOCK_TIMEOUT } else { DRAG_LOCK_DISABLED });
    note("drag_lock", settle(device, lock, true, D::default_drag_lock, D::drag_lock, D::set_drag_lock));
    let supported = device.default_tap_button_map().is_some();
    note(
        "tap_button_map",
        settle(
            device,
            config.tap_button_map.map(Some),
            supported,
            D::default_tap_button_map,
            D::tap_button_map,
            |device, map| map.map_or(Ok(()), |map| device.set_tap_button_map(map)),
        ),
    );
    match device.drag_fingers() {
        // Once per touchpad that asks, which is the whole cost of running
        // on a libinput older than the setting.
        None if config.drag_3fg.is_some() => rejected.push("drag_3fg: requires a newer libinput".to_string()),
        None => {}
        Some(fingers) => {
            let supported = fingers >= 3 && (fingers >= 4 || config.drag_3fg != Some(MultiFingerDrag::FourFingers));
            note(
                "drag_3fg",
                settle(device, config.drag_3fg, supported, D::default_multi_finger_drag, D::multi_finger_drag, D::set_multi_finger_drag),
            );
        }
    }
}

// ---- libinput -----------------------------------------------------------

type DeviceHandle = *mut libinput_crate::ffi::libinput_device;

/// libinput's multi-finger drag calls. libinput 1.27 added them, after
/// the binding this build links through, and naming them in the binary
/// would stop the compositor loading at all on an older libinput. So they
/// are looked up by name, once, and an older libinput costs only the
/// setting.
struct DragCalls {
    finger_count: unsafe extern "C" fn(DeviceHandle) -> c_int,
    enabled: unsafe extern "C" fn(DeviceHandle) -> c_uint,
    default_enabled: unsafe extern "C" fn(DeviceHandle) -> c_uint,
    set_enabled: unsafe extern "C" fn(DeviceHandle, c_uint) -> c_uint,
}

static DRAG_CALLS: OnceLock<Option<DragCalls>> = OnceLock::new();

/// Looks the calls up, as the session opens libinput, and says whether
/// they exist. Every later caller reuses the answer.
pub(super) fn resolve_drag_calls() -> bool {
    drag_calls().is_some()
}

fn drag_calls() -> Option<&'static DragCalls> {
    DRAG_CALLS.get_or_init(look_up_drag_calls).as_ref()
}

fn look_up_drag_calls() -> Option<DragCalls> {
    fn symbol(name: &CStr) -> Option<*mut c_void> {
        // SAFETY: `name` is NUL-terminated, and `RTLD_DEFAULT` searches
        // the objects already loaded into this process without loading
        // anything new.
        let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!address.is_null()).then_some(address)
    }
    let finger_count = symbol(c"libinput_device_config_3fg_drag_get_finger_count")?;
    let enabled = symbol(c"libinput_device_config_3fg_drag_get_enabled")?;
    let default_enabled = symbol(c"libinput_device_config_3fg_drag_get_default_enabled")?;
    let set_enabled = symbol(c"libinput_device_config_3fg_drag_set_enabled")?;
    // SAFETY: each address is the libinput function of that name, and the
    // pointer type it becomes is that function's C signature in
    // libinput.h (1.27 and later): the device handle in, and an `int`
    // count or an unsigned enumeration value out.
    unsafe {
        Some(DragCalls {
            finger_count: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(DeviceHandle) -> c_int>(finger_count),
            enabled: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(DeviceHandle) -> c_uint>(enabled),
            default_enabled: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(DeviceHandle) -> c_uint>(
                default_enabled,
            ),
            set_enabled: std::mem::transmute::<*mut c_void, unsafe extern "C" fn(DeviceHandle, c_uint) -> c_uint>(
                set_enabled,
            ),
        })
    }
}

fn config_status(status: c_uint) -> Result<(), DeviceConfigError> {
    match status {
        libinput_crate::ffi::libinput_config_status_LIBINPUT_CONFIG_STATUS_SUCCESS => Ok(()),
        libinput_crate::ffi::libinput_config_status_LIBINPUT_CONFIG_STATUS_UNSUPPORTED => {
            Err(DeviceConfigError::Unsupported)
        }
        _ => Err(DeviceConfigError::Invalid),
    }
}

fn multi_finger_drag(state: c_uint) -> MultiFingerDrag {
    // libinput defines no fourth state; anything else reads as off.
    MultiFingerDrag::from_number(i64::from(state)).unwrap_or(MultiFingerDrag::Disabled)
}

fn scroll_method(method: libinput_crate::ScrollMethod) -> Option<ScrollMethod> {
    match method {
        libinput_crate::ScrollMethod::TwoFinger => Some(ScrollMethod::TwoFinger),
        libinput_crate::ScrollMethod::Edge => Some(ScrollMethod::Edge),
        libinput_crate::ScrollMethod::OnButtonDown => Some(ScrollMethod::OnButtonDown),
        libinput_crate::ScrollMethod::NoScroll => Some(ScrollMethod::NoScroll),
        _ => None,
    }
}

fn tap_button_map(map: libinput_crate::TapButtonMap) -> Option<TapButtonMap> {
    match map {
        libinput_crate::TapButtonMap::LeftRightMiddle => Some(TapButtonMap::LeftRightMiddle),
        libinput_crate::TapButtonMap::LeftMiddleRight => Some(TapButtonMap::LeftMiddleRight),
        _ => None,
    }
}

impl PointerDevice for libinput_crate::Device {
    fn tap_drag(&self) -> bool {
        self.config_tap_drag_enabled()
    }
    fn default_tap_drag(&self) -> bool {
        self.config_tap_default_drag_enabled()
    }
    fn set_tap_drag(&mut self, enabled: bool) -> Result<(), DeviceConfigError> {
        self.config_tap_set_drag_enabled(enabled)
    }
    fn drag_lock(&self) -> u32 {
        // SAFETY: the handle is this `Device`'s own, which holds a
        // libinput reference for as long as `self` is borrowed, and the
        // call only reads configuration.
        unsafe { libinput_crate::ffi::libinput_device_config_tap_get_drag_lock_enabled(self.as_raw_mut()) }
    }
    fn default_drag_lock(&self) -> u32 {
        // SAFETY: as in `drag_lock`.
        unsafe { libinput_crate::ffi::libinput_device_config_tap_get_default_drag_lock_enabled(self.as_raw_mut()) }
    }
    fn set_drag_lock(&mut self, state: u32) -> Result<(), DeviceConfigError> {
        // SAFETY: as in `drag_lock`; libinput validates `state` and
        // answers an unknown one with a status rather than acting on it.
        config_status(unsafe {
            libinput_crate::ffi::libinput_device_config_tap_set_drag_lock_enabled(self.as_raw_mut(), state)
        })
    }
    fn tap_button_map(&self) -> Option<TapButtonMap> {
        self.config_tap_button_map().and_then(tap_button_map)
    }
    fn default_tap_button_map(&self) -> Option<TapButtonMap> {
        self.config_tap_default_button_map().and_then(tap_button_map)
    }
    fn set_tap_button_map(&mut self, map: TapButtonMap) -> Result<(), DeviceConfigError> {
        self.config_tap_set_button_map(match map {
            TapButtonMap::LeftRightMiddle => libinput_crate::TapButtonMap::LeftRightMiddle,
            TapButtonMap::LeftMiddleRight => libinput_crate::TapButtonMap::LeftMiddleRight,
        })
    }
    fn has_middle_emulation(&self) -> bool {
        self.config_middle_emulation_is_available()
    }
    fn middle_emulation(&self) -> bool {
        self.config_middle_emulation_enabled()
    }
    fn default_middle_emulation(&self) -> bool {
        self.config_middle_emulation_default_enabled()
    }
    fn set_middle_emulation(&mut self, enabled: bool) -> Result<(), DeviceConfigError> {
        self.config_middle_emulation_set_enabled(enabled)
    }
    fn scroll_methods(&self) -> Vec<ScrollMethod> {
        self.config_scroll_methods().into_iter().filter_map(scroll_method).collect()
    }
    fn scroll_method(&self) -> Option<ScrollMethod> {
        self.config_scroll_method().and_then(scroll_method)
    }
    fn default_scroll_method(&self) -> Option<ScrollMethod> {
        self.config_scroll_default_method().and_then(scroll_method)
    }
    fn set_scroll_method(&mut self, method: ScrollMethod) -> Result<(), DeviceConfigError> {
        self.config_scroll_set_method(match method {
            ScrollMethod::TwoFinger => libinput_crate::ScrollMethod::TwoFinger,
            ScrollMethod::Edge => libinput_crate::ScrollMethod::Edge,
            ScrollMethod::OnButtonDown => libinput_crate::ScrollMethod::OnButtonDown,
            ScrollMethod::NoScroll => libinput_crate::ScrollMethod::NoScroll,
        })
    }
    fn scroll_button(&self) -> u32 {
        self.config_scroll_button()
    }
    fn default_scroll_button(&self) -> u32 {
        self.config_scroll_default_button()
    }
    fn set_scroll_button(&mut self, button: u32) -> Result<(), DeviceConfigError> {
        self.config_scroll_set_button(button)
    }
    fn drag_fingers(&self) -> Option<u32> {
        let calls = drag_calls()?;
        // SAFETY: the handle is this `Device`'s own, alive while `self` is
        // borrowed, and the function has exactly this signature.
        let fingers = unsafe { (calls.finger_count)(self.as_raw_mut()) };
        Some(u32::try_from(fingers).unwrap_or(0))
    }
    fn multi_finger_drag(&self) -> MultiFingerDrag {
        // Only reached after `drag_fingers` found the calls.
        drag_calls().map_or(MultiFingerDrag::Disabled, |calls| {
            // SAFETY: as in `drag_fingers`.
            multi_finger_drag(unsafe { (calls.enabled)(self.as_raw_mut()) })
        })
    }
    fn default_multi_finger_drag(&self) -> MultiFingerDrag {
        drag_calls().map_or(MultiFingerDrag::Disabled, |calls| {
            // SAFETY: as in `drag_fingers`.
            multi_finger_drag(unsafe { (calls.default_enabled)(self.as_raw_mut()) })
        })
    }
    fn set_multi_finger_drag(&mut self, drag: MultiFingerDrag) -> Result<(), DeviceConfigError> {
        let calls = drag_calls().ok_or(DeviceConfigError::Unsupported)?;
        let state = match drag {
            MultiFingerDrag::Disabled => 0,
            MultiFingerDrag::ThreeFingers => 1,
            MultiFingerDrag::FourFingers => 2,
        };
        // SAFETY: as in `drag_fingers`; `state` is one of libinput's three
        // defined drag states.
        config_status(unsafe { (calls.set_enabled)(self.as_raw_mut(), state) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_core::ScrollClass;

    /// A device with every capability switched on or off by field, whose
    /// defaults differ from libinput's usual ones so that "restored the
    /// default" and "never touched it" cannot be confused.
    struct Device {
        touchpad: bool,
        middle: Option<(bool, bool)>,
        methods: Vec<ScrollMethod>,
        method: (ScrollMethod, ScrollMethod),
        button: (u32, u32),
        tap_drag: (bool, bool),
        drag_lock: (u32, u32),
        tap_map: (TapButtonMap, TapButtonMap),
        drag_fingers: Option<u32>,
        drag: (MultiFingerDrag, MultiFingerDrag),
        writes: Vec<&'static str>,
    }

    impl Device {
        fn touchpad() -> Self {
            Device {
                touchpad: true,
                middle: Some((true, true)),
                methods: vec![ScrollMethod::TwoFinger, ScrollMethod::Edge],
                method: (ScrollMethod::TwoFinger, ScrollMethod::TwoFinger),
                button: (0, 0),
                tap_drag: (true, true),
                drag_lock: (DRAG_LOCK_DISABLED, DRAG_LOCK_DISABLED),
                tap_map: (TapButtonMap::LeftRightMiddle, TapButtonMap::LeftRightMiddle),
                drag_fingers: Some(4),
                drag: (MultiFingerDrag::ThreeFingers, MultiFingerDrag::ThreeFingers),
                writes: Vec::new(),
            }
        }

        fn trackball() -> Self {
            Device {
                touchpad: false,
                middle: Some((false, false)),
                methods: vec![ScrollMethod::OnButtonDown],
                method: (ScrollMethod::OnButtonDown, ScrollMethod::OnButtonDown),
                button: (274, 274),
                drag_fingers: None,
                ..Device::touchpad()
            }
        }

        fn keyboard() -> Self {
            Device { middle: None, methods: Vec::new(), ..Device::trackball() }
        }
    }

    impl ScrollDevice for Device {
        type Error = ();
        fn is_touchpad(&self) -> bool {
            self.touchpad
        }
        fn has_natural_scroll(&self) -> bool {
            false
        }
        fn default_natural_scroll(&self) -> bool {
            false
        }
        fn natural_scroll(&self) -> bool {
            false
        }
        fn set_natural_scroll(&mut self, _: bool) -> Result<(), ()> {
            unreachable!("natural scrolling belongs to `scroll`")
        }
    }

    impl PointerDevice for Device {
        fn tap_drag(&self) -> bool {
            self.tap_drag.0
        }
        fn default_tap_drag(&self) -> bool {
            self.tap_drag.1
        }
        fn set_tap_drag(&mut self, enabled: bool) -> Result<(), ()> {
            self.writes.push("tap_and_drag");
            self.tap_drag.0 = enabled;
            Ok(())
        }
        fn drag_lock(&self) -> u32 {
            self.drag_lock.0
        }
        fn default_drag_lock(&self) -> u32 {
            self.drag_lock.1
        }
        fn set_drag_lock(&mut self, state: u32) -> Result<(), ()> {
            self.writes.push("drag_lock");
            self.drag_lock.0 = state;
            Ok(())
        }
        fn tap_button_map(&self) -> Option<TapButtonMap> {
            self.touchpad.then_some(self.tap_map.0)
        }
        fn default_tap_button_map(&self) -> Option<TapButtonMap> {
            self.touchpad.then_some(self.tap_map.1)
        }
        fn set_tap_button_map(&mut self, map: TapButtonMap) -> Result<(), ()> {
            self.writes.push("tap_button_map");
            self.tap_map.0 = map;
            Ok(())
        }
        fn has_middle_emulation(&self) -> bool {
            self.middle.is_some()
        }
        fn middle_emulation(&self) -> bool {
            self.middle.expect("read only when available").0
        }
        fn default_middle_emulation(&self) -> bool {
            self.middle.expect("read only when available").1
        }
        fn set_middle_emulation(&mut self, enabled: bool) -> Result<(), ()> {
            self.writes.push("middle_button_emulation");
            self.middle.as_mut().expect("written only when available").0 = enabled;
            Ok(())
        }
        fn scroll_methods(&self) -> Vec<ScrollMethod> {
            self.methods.clone()
        }
        fn scroll_method(&self) -> Option<ScrollMethod> {
            Some(self.method.0)
        }
        fn default_scroll_method(&self) -> Option<ScrollMethod> {
            Some(self.method.1)
        }
        fn set_scroll_method(&mut self, method: ScrollMethod) -> Result<(), ()> {
            self.writes.push("scroll_method");
            self.method.0 = method;
            Ok(())
        }
        fn scroll_button(&self) -> u32 {
            self.button.0
        }
        fn default_scroll_button(&self) -> u32 {
            self.button.1
        }
        fn set_scroll_button(&mut self, button: u32) -> Result<(), ()> {
            self.writes.push("scroll_button");
            self.button.0 = button;
            Ok(())
        }
        fn drag_fingers(&self) -> Option<u32> {
            self.drag_fingers
        }
        fn multi_finger_drag(&self) -> MultiFingerDrag {
            self.drag.0
        }
        fn default_multi_finger_drag(&self) -> MultiFingerDrag {
            self.drag.1
        }
        fn set_multi_finger_drag(&mut self, drag: MultiFingerDrag) -> Result<(), ()> {
            self.writes.push("drag_3fg");
            self.drag.0 = drag;
            Ok(())
        }
    }

    fn apply(device: &mut Device, config: &PointerConfig) -> Vec<String> {
        let mut rejected = Vec::new();
        configure(device, config, &mut rejected);
        rejected
    }

    /// Every setting this module owns, set away from the fake's defaults.
    fn everything() -> PointerConfig {
        let class = |method| ScrollClass {
            scroll_method: Some(method),
            scroll_button: Some(275),
            middle_button_emulation: Some(true),
            ..ScrollClass::default()
        };
        PointerConfig {
            pointer: class(ScrollMethod::OnButtonDown),
            touchpad: ScrollClass { middle_button_emulation: Some(false), ..class(ScrollMethod::Edge) },
            tap_and_drag: Some(false),
            drag_lock: Some(true),
            tap_button_map: Some(TapButtonMap::LeftMiddleRight),
            drag_3fg: Some(MultiFingerDrag::FourFingers),
            ..PointerConfig::default()
        }
    }

    #[test]
    fn each_setter_runs_only_where_the_device_advertises_the_capability() {
        let mut touchpad = Device::touchpad();
        assert_eq!(apply(&mut touchpad, &everything()), ["scroll_button: unsupported"]);
        assert_eq!(touchpad.middle, Some((false, true)));
        assert_eq!(touchpad.method.0, ScrollMethod::Edge);
        assert!(!touchpad.tap_drag.0);
        assert_eq!(touchpad.drag_lock.0, DRAG_LOCK_TIMEOUT);
        assert_eq!(touchpad.tap_map.0, TapButtonMap::LeftMiddleRight);
        assert_eq!(touchpad.drag.0, MultiFingerDrag::FourFingers);
        assert!(!touchpad.writes.contains(&"scroll_button"), "a touchpad without on-button-down scrolling has no button");

        let mut trackball = Device::trackball();
        assert!(apply(&mut trackball, &everything()).is_empty());
        assert_eq!(trackball.middle, Some((true, false)));
        assert_eq!(trackball.button.0, 275);
        assert_eq!(trackball.writes, ["middle_button_emulation", "scroll_button"], "tapping settings never reach a mouse");

        let mut keyboard = Device::keyboard();
        assert_eq!(
            apply(&mut keyboard, &everything()),
            ["middle_button_emulation: unsupported", "scroll_method: unsupported", "scroll_button: unsupported"]
        );
        assert!(keyboard.writes.is_empty());
        assert!(apply(&mut keyboard, &PointerConfig::default()).is_empty(), "an unset key is never reported");
    }

    #[test]
    fn removing_a_key_restores_each_devices_default_and_then_writes_nothing() {
        for mut device in [Device::touchpad(), Device::trackball()] {
            apply(&mut device, &everything());
            let writes = device.writes.len();
            apply(&mut device, &everything());
            assert_eq!(device.writes.len(), writes, "an unchanged reload writes nothing");

            assert!(apply(&mut device, &PointerConfig::default()).is_empty());
            assert_eq!(device.middle.map(|(current, default)| current == default), Some(true));
            assert_eq!(device.method.0, device.method.1);
            assert_eq!(device.button.0, device.button.1);
            assert_eq!(device.tap_drag.0, device.tap_drag.1);
            assert_eq!(device.drag_lock.0, device.drag_lock.1);
            assert_eq!(device.tap_map.0, device.tap_map.1);
            assert_eq!(device.drag.0, device.drag.1);
            let writes = device.writes.len();
            apply(&mut device, &PointerConfig::default());
            assert_eq!(device.writes.len(), writes);
        }
    }

    #[test]
    fn a_zero_scroll_button_is_the_devices_own_and_no_scroll_needs_no_listing() {
        let mut trackball = Device::trackball();
        let config = |button, method| PointerConfig {
            pointer: ScrollClass { scroll_button: Some(button), scroll_method: Some(method), ..ScrollClass::default() },
            ..PointerConfig::default()
        };
        apply(&mut trackball, &config(275, ScrollMethod::OnButtonDown));
        assert!(apply(&mut trackball, &config(0, ScrollMethod::NoScroll)).is_empty());
        assert_eq!(trackball.button.0, 274);
        assert_eq!(trackball.method.0, ScrollMethod::NoScroll);
    }

    #[test]
    fn multi_finger_drag_on_an_older_libinput_is_one_named_rejection() {
        let mut touchpad = Device { drag_fingers: None, ..Device::touchpad() };
        let rejected = apply(&mut touchpad, &everything());
        assert_eq!(
            rejected.iter().filter(|line| line.starts_with("drag_3fg")).collect::<Vec<_>>(),
            ["drag_3fg: requires a newer libinput"]
        );
        assert!(!touchpad.writes.contains(&"drag_3fg"));
        assert!(apply(&mut touchpad, &PointerConfig::default()).is_empty(), "silent unless the setting was asked for");

        let mut three = Device { drag_fingers: Some(3), ..Device::touchpad() };
        assert!(apply(&mut three, &everything()).contains(&"drag_3fg: unsupported".to_string()));
        assert_eq!(three.drag.0, MultiFingerDrag::ThreeFingers, "four fingers on a three-finger pad is refused");
    }
}
