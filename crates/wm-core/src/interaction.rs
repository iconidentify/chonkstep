//! Desktop interaction policy and application-aware Mac shortcut translation.
//!
//! Physical modifiers are never globally swapped. A translation is selected
//! for a complete chord and kept by the backend until that press is released.

use crate::{KeyCombo, Modifiers};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InteractionMode {
    #[default]
    Desktop,
    Mac,
}

impl InteractionMode {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "desktop" => Some(Self::Desktop),
            "mac" => Some(Self::Mac),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mac => "mac",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppProfile {
    #[default]
    Gui,
    Terminal,
    /// Single-window terminals close through the compositor (no Ctrl-W in PTY).
    TerminalWindow,
    Browser,
    FileManager,
    /// The application already has native Command bindings, or needs raw input.
    Native,
    /// Remote sessions and games own even desktop shortcuts.
    Passthrough,
}

impl AppProfile {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "gui" => Some(Self::Gui),
            "terminal" => Some(Self::Terminal),
            "terminal-window" => Some(Self::TerminalWindow),
            "browser" => Some(Self::Browser),
            "files" => Some(Self::FileManager),
            "native" => Some(Self::Native),
            "passthrough" => Some(Self::Passthrough),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionConfig {
    pub mode: InteractionMode,
    pub clipboard_persistence: bool,
    /// Exact, case-insensitive application identities; user entries win.
    pub applications: Vec<(String, AppProfile)>,
}

impl Default for InteractionConfig {
    fn default() -> Self {
        Self {
            mode: InteractionMode::Desktop,
            clipboard_persistence: true,
            applications: Vec::new(),
        }
    }
}

impl InteractionConfig {
    pub fn profile(&self, identity: &str) -> AppProfile {
        if let Some((_, profile)) = self
            .applications
            .iter()
            .rev()
            .find(|(name, _)| name.eq_ignore_ascii_case(identity))
        {
            return *profile;
        }
        // Match complete IDs or their final desktop-ID component. Never match a
        // window title (which can contain arbitrary application-controlled text).
        let short = identity.rsplit('.').next().unwrap_or(identity);
        let is = |names: &[&str]| names.iter().any(|name| short.eq_ignore_ascii_case(name));
        if is(&["Alacritty", "foot", "footclient", "xterm", "uxterm", "st"]) {
            AppProfile::TerminalWindow
        } else if is(&[
            "kitty",
            "ghostty",
            "org.gnome.Terminal",
            "terminal",
            "gnome-terminal-server",
            "konsole",
            "wezterm",
            "xterm",
            "uxterm",
            "st",
        ]) {
            AppProfile::Terminal
        } else if is(&[
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
            "chrome",
            "firefox",
            "zen",
            "brave-browser",
            "vivaldi-stable",
            "microsoft-edge",
        ]) {
            AppProfile::Browser
        } else if is(&["nautilus", "thunar", "dolphin", "nemo", "pcmanfm", "pcmanfm-qt"]) {
            AppProfile::FileManager
        } else if is(&[
            "virt-manager",
            "remote-viewer",
            "virt-viewer",
            "remmina",
            "xfreerdp",
            "wlfreerdp",
            "VirtualBox Machine",
            "looking-glass-client",
        ]) {
            AppProfile::Passthrough
        } else {
            AppProfile::Gui
        }
    }
}

/// Return a replacement chord, or None to preserve the original input. Desktop
/// bindings and shortcut inhibitors must be resolved before calling this.
pub fn mac_chord(profile: AppProfile, key: KeyCombo) -> Option<KeyCombo> {
    use Modifiers as M;
    if matches!(profile, AppProfile::Native | AppProfile::Passthrough) {
        return None;
    }
    let m = key.modifiers;
    let shift = m & M::SHIFT;
    let base = m & !M::SHIFT;
    let to = |keysym, modifiers| Some(KeyCombo { keysym, modifiers });
    let sym = key.keysym;
    // Option editing must not interfere with Option text composition/AltGr.
    if base == M::ALT && !matches!(profile, AppProfile::Terminal | AppProfile::TerminalWindow) {
        return match sym {
            0xff51 | 0xff53 | 0xff52 | 0xff54 | 0xff08 | 0xffff => to(sym, M::CONTROL | shift),
            _ => None,
        };
    }
    if profile == AppProfile::Browser {
        if base == (M::SUPER | M::ALT) && shift.is_empty() {
            return match sym {
                0x69 | 0x6a => to(sym, M::CONTROL | M::SHIFT), // developer tools/console
                0x75 => to(sym, M::CONTROL),                   // page source
                0xff51 => to(0xff55, M::CONTROL),
                0xff53 => to(0xff56, M::CONTROL),
                _ => None,
            };
        }
        if base == M::SUPER && matches!(sym, 0x5b | 0x5d) {
            return if shift.is_empty() {
                to(if sym == 0x5b { 0xff51 } else { 0xff53 }, M::ALT)
            } else {
                to(if sym == 0x5b { 0xff55 } else { 0xff56 }, M::CONTROL)
            };
        }
    }
    if profile == AppProfile::FileManager && base == M::SUPER {
        return match (sym, shift.is_empty()) {
            (0xff52, true) => to(sym, M::ALT),        // enclosing folder
            (0xff54, true) => to(0xff0d, M::empty()), // open selection
            (0xff08, true) => to(0xffff, M::empty()), // trash through file manager
            (0x69, true) => to(0xff0d, M::ALT),       // properties
            (0x68, false) => to(0xff50, M::ALT),      // home
            (0x67, false) => to(0x6c, M::CONTROL),    // go to folder
            // Ctrl+D means bookmark in Linux file managers, not Duplicate.
            (0x64, _) => None,
            (0x61 | 0x63 | 0x76 | 0x78 | 0x7a | 0x6e | 0x66 | 0x6c | 0x77, _) => to(sym, M::CONTROL | shift),
            _ => None,
        };
    }
    if base == M::SUPER | M::ALT && shift == M::SHIFT && sym == b'v' as u32 {
        return to(sym, M::CONTROL | M::SHIFT);
    }
    if base != M::SUPER {
        return None;
    }
    if matches!(profile, AppProfile::Terminal | AppProfile::TerminalWindow) {
        return match sym {
            // These act on terminal selection/paste, never on the PTY.
            0x63 | 0x76 => to(sym, M::CONTROL | M::SHIFT),
            0x6e | 0x77 => to(sym, M::CONTROL | M::SHIFT),
            0x2b | 0x3d | 0x2d | 0x30 => to(sym, M::CONTROL | shift),
            _ => None,
        };
    }
    match sym {
        // Text navigation, separate from Ctrl+arrows (Spaces).
        0xff51 => to(0xff50, shift), // Home
        0xff53 => to(0xff57, shift), // End
        0xff52 => to(0xff50, M::CONTROL | shift),
        0xff54 => to(0xff57, M::CONTROL | shift),
        0x2e => to(0xff1b, M::empty()), // cancel
        // Finite common-command vocabulary; extra modifiers are never dropped.
        0x61..=0x7a | 0x30..=0x39 | 0x2c | 0x2b | 0x3d | 0x2d | 0x5b | 0x5d => to(sym, M::CONTROL | shift),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(c: u32, modifiers: Modifiers) -> KeyCombo {
        KeyCombo { keysym: c, modifiers }
    }

    #[test]
    fn copy_does_not_become_a_terminal_interrupt_and_control_is_untouched() {
        let c = b'c' as u32;
        assert_eq!(
            mac_chord(AppProfile::Gui, key(c, Modifiers::SUPER)),
            Some(key(c, Modifiers::CONTROL))
        );
        assert_eq!(
            mac_chord(AppProfile::Terminal, key(c, Modifiers::SUPER)),
            Some(key(c, Modifiers::CONTROL | Modifiers::SHIFT))
        );
        assert_eq!(mac_chord(AppProfile::Terminal, key(c, Modifiers::CONTROL)), None);
        assert_eq!(mac_chord(AppProfile::Native, key(c, Modifiers::SUPER)), None);
    }

    #[test]
    fn text_navigation_preserves_selection_and_does_not_steal_spaces() {
        assert_eq!(
            mac_chord(AppProfile::Gui, key(0xff51, Modifiers::SUPER | Modifiers::SHIFT)),
            Some(key(0xff50, Modifiers::SHIFT))
        );
        assert_eq!(mac_chord(AppProfile::Gui, key(0xff51, Modifiers::CONTROL)), None);
        assert_eq!(mac_chord(AppProfile::Gui, key(b'e' as u32, Modifiers::ALT)), None);
    }

    #[test]
    fn application_rules_are_exact_and_take_precedence_over_builtins() {
        let mut config = InteractionConfig::default();
        assert_eq!(config.profile("com.mitchellh.ghostty"), AppProfile::Terminal);
        assert_eq!(config.profile("not-a-terminal-foot"), AppProfile::Gui);
        config.applications.push(("foot".into(), AppProfile::Native));
        assert_eq!(config.profile("Foot"), AppProfile::Native);
    }
}
