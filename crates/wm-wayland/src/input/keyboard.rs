//! Keyboard configuration ownership: resolve once by the same rules at startup
//! and reload, compile only changed maps, and keep repeat timing independent.
//!
//! Physical key delivery remains in the seat input path. In particular, an
//! unchanged configuration must not reset a client's held-key repeat state, and
//! skipping a reload must not bypass Smithay's virtual-keyboard restoration.

use smithay::input::keyboard::XkbConfig;

use crate::state::Compositor;

mod focus;
pub(crate) mod mac;
pub(super) mod repeat;
pub(crate) use focus::KeyboardFocus;

/// Use the same ownership order for key routing and focus restoration. A
/// modal UI may remain open behind an exclusive launcher or shell focus grab,
/// but must not swallow the keys destined for that higher-priority owner.
pub(crate) fn modal_owns_keyboard(comp: &Compositor) -> bool {
    let backend = comp.wm.backend();
    backend.keyboard_grabbed
        && !backend.locked
        && comp.layer_shell.exclusive_focus.is_none()
        && !comp.focus_grab.is_active()
}

/// Backend grab verbs cannot access the seat. Reconcile their transition after
/// layer/focus-grab ownership settles, including cancel-to-the-same-window:
/// that path deliberately does not produce a new window-manager focus intent.
pub(crate) fn sync_modal_focus(comp: &mut Compositor) {
    if !std::mem::take(&mut comp.wm.backend_mut().keyboard_grab_changed) {
        return;
    }
    repeat::modal_changed(comp);
    if comp.wm.backend().locked
        || comp.layer_shell.exclusive_focus.is_some()
        || comp.focus_grab.is_active()
    {
        // These owners already settled above us. Their release paths consult
        // the shared target resolver, including any still-open modal UI.
        return;
    }
    let target = crate::layers::keyboard_target(comp);
    let target = target.map(|surface| KeyboardFocus::new(comp, surface));
    if let Some(keyboard) = comp.seat.get_keyboard() {
        keyboard.set_focus(comp, target, smithay::utils::SERIAL_COUNTER.next_serial());
    }
    // A modal scene takes pointer ownership too. Games must hear unlock/leave
    // on opening Overview, and regain persistent constraints on cancellation,
    // even if the physical mouse has not moved in between.
    super::sync_pointer_focus(comp);
}

/// Settings successfully installed on the seat, after environment precedence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedKeyboard {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}

impl ResolvedKeyboard {
    pub(crate) fn xkb_config(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: &self.rules,
            model: &self.model,
            layout: &self.layout,
            variant: &self.variant,
            options: self.options.clone(),
        }
    }

    fn same_keymap(&self, other: &Self) -> bool {
        self.rules == other.rules
            && self.model == other.model
            && self.layout == other.layout
            && self.variant == other.variant
            && self.options == other.options
    }

    fn same_repeat(&self, other: &Self) -> bool {
        self.repeat_rate == other.repeat_rate && self.repeat_delay == other.repeat_delay
    }
}

/// Nonempty XKB_DEFAULT_* values win over the file, matching login-session
/// convention. Injecting the reader privately lets tests exercise precedence
/// without mutating process-global environment alongside parallel tests.
pub(crate) fn resolve_keyboard_config(config: &wm_core::KeyboardConfig) -> ResolvedKeyboard {
    resolve_with_env(config, |name| std::env::var(name).ok())
}

fn resolve_with_env(
    config: &wm_core::KeyboardConfig,
    env: impl Fn(&str) -> Option<String>,
) -> ResolvedKeyboard {
    let value = |name| env(name).filter(|value| !value.is_empty());
    ResolvedKeyboard {
        rules: value("XKB_DEFAULT_RULES")
            .or_else(|| config.rules.clone())
            .unwrap_or_default(),
        model: value("XKB_DEFAULT_MODEL")
            .or_else(|| config.model.clone())
            .unwrap_or_default(),
        layout: value("XKB_DEFAULT_LAYOUT")
            .or_else(|| config.layout.clone())
            .unwrap_or_default(),
        variant: value("XKB_DEFAULT_VARIANT")
            .or_else(|| config.variant.clone())
            .unwrap_or_default(),
        options: value("XKB_DEFAULT_OPTIONS").or_else(|| config.options.clone()),
        // Core Wayland requires nonnegative values. Keep direct TOML callers
        // inside the same bounds as Hyprland configuration, including zero.
        repeat_delay: config.repeat_delay.unwrap_or(200).clamp(0, 5000),
        repeat_rate: config.repeat_rate.unwrap_or(25).clamp(0, 1000),
    }
}

impl Compositor {
    /// Apply a staged edit atomically. A rejected keymap keeps both the running
    /// map and timing. Unchanged maps are never compiled or rebroadcast; timing
    /// updates alone use repeat_info and preserve the active XKB group/state.
    pub(crate) fn apply_pending_keyboard(&mut self) {
        let Some(requested) = self.wm.backend_mut().pending_keyboard.take() else {
            return;
        };
        let resolved = resolve_keyboard_config(&requested);
        if self.keyboard_config.as_ref() == Some(&resolved) {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let keymap_changed = self
            .keyboard_config
            .as_ref()
            .is_none_or(|old| !old.same_keymap(&resolved));
        let repeat_changed = self
            .keyboard_config
            .as_ref()
            .is_none_or(|old| !old.same_repeat(&resolved));
        if keymap_changed {
            if let Err(error) = keyboard.set_xkb_config(self, resolved.xkb_config()) {
                tracing::warn!(
                    %error,
                    layout = %resolved.layout,
                    "reload's keyboard configuration was rejected; keeping the running keymap"
                );
                return;
            }
        }
        if repeat_changed {
            keyboard.change_repeat_info(resolved.repeat_rate, resolved.repeat_delay);
        }
        let backend = self.wm.backend_mut();
        backend.repeat_rate = resolved.repeat_rate as u32;
        backend.repeat_delay = std::time::Duration::from_millis(resolved.repeat_delay as u64);
        backend.keyboard_layout.clone_from(&resolved.layout);
        repeat::reconfigure(
            &self.seat,
            backend.repeat_rate,
            backend.repeat_delay,
            keymap_changed,
        );
        tracing::info!(
            layout = %resolved.layout,
            variant = %resolved.variant,
            options = ?resolved.options,
            repeat_rate = resolved.repeat_rate,
            repeat_delay = resolved.repeat_delay,
            keymap_changed,
            "reload applied a new keyboard configuration"
        );
        self.keyboard_config = Some(resolved);
        if keymap_changed {
            self.hyprland_state_dirty = true;
            crate::hyprland_ipc::refresh_keyboard_layout(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_values_and_defaults_do_not_depend_on_the_test_process_environment() {
        let config = wm_core::KeyboardConfig {
            layout: Some("fr".into()),
            variant: Some("azerty".into()),
            repeat_rate: Some(40),
            repeat_delay: Some(300),
            ..Default::default()
        };
        let from_file = resolve_with_env(&config, |_| None);
        assert_eq!((&*from_file.layout, &*from_file.variant), ("fr", "azerty"));
        assert_eq!((from_file.repeat_rate, from_file.repeat_delay), (40, 300));
        let bare = resolve_with_env(&wm_core::KeyboardConfig::default(), |_| None);
        assert_eq!((bare.repeat_rate, bare.repeat_delay), (25, 200));
        assert!(bare.layout.is_empty());
        assert_eq!(bare.options, None);
    }

    #[test]
    fn nonempty_environment_values_win_and_empty_values_fall_back() {
        let config = wm_core::KeyboardConfig {
            layout: Some("fr".into()),
            variant: Some("azerty".into()),
            options: Some("compose:caps".into()),
            ..Default::default()
        };
        let resolved = resolve_with_env(&config, |name| match name {
            "XKB_DEFAULT_LAYOUT" => Some("us,de".into()),
            "XKB_DEFAULT_VARIANT" => Some(String::new()),
            "XKB_DEFAULT_OPTIONS" => Some("grp:alt_shift_toggle".into()),
            _ => None,
        });
        assert_eq!(resolved.layout, "us,de");
        assert_eq!(resolved.variant, "azerty");
        assert_eq!(resolved.options.as_deref(), Some("grp:alt_shift_toggle"));
    }

    #[test]
    fn zero_repeat_is_preserved_and_direct_config_values_are_bounded() {
        for (rate, delay, expected) in [
            (0, 0, (0, 0)),
            (-1, -1, (0, 0)),
            (i32::MAX, i32::MAX, (1000, 5000)),
        ] {
            let config = wm_core::KeyboardConfig {
                repeat_rate: Some(rate),
                repeat_delay: Some(delay),
                ..Default::default()
            };
            let resolved = resolve_with_env(&config, |_| None);
            assert_eq!((resolved.repeat_rate, resolved.repeat_delay), expected);
        }
    }

    #[test]
    fn keymap_and_repeat_changes_are_independent() {
        let initial = resolve_with_env(&wm_core::KeyboardConfig::default(), |_| None);
        let mut next = initial.clone();
        next.repeat_rate = 0;
        assert!(initial.same_keymap(&next));
        assert!(!initial.same_repeat(&next));
        next = initial.clone();
        next.layout = "de".into();
        assert!(!initial.same_keymap(&next));
        assert!(initial.same_repeat(&next));
        next = initial.clone();
        next.options = Some("compose:caps".into());
        assert!(!initial.same_keymap(&next));
    }
}
