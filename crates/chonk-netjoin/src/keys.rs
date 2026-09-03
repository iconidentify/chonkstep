//! X11 keysyms reduced to the six things a passphrase field cares
//! about.
//!
//! `wm-x11` already turns keycodes into keysyms, but it takes shift
//! level 0 only — correct for a window manager matching `Super+Return`
//! against a binding table, useless for text entry, where the whole
//! point is that `shift`+`2` is `@` and not `2`. So this module exists,
//! and it is pure: keycode lookup and modifier state come from the X
//! connection in [`crate::window`], and everything from there down is a
//! function of two integers that a test can call.
//!
//! # What is deliberately not here
//!
//! Input methods, dead keys, compose sequences and AltGr levels beyond
//! shift. A WPA passphrase is 8–63 characters of ASCII in
//! overwhelming practice, and the failure mode of the missing cases is
//! a character that does not appear — visible, immediate, and
//! recoverable by pasting into a terminal — rather than a passphrase
//! that is silently wrong. Pretending to full i18n text input with 40
//! lines of keysym arithmetic would be the worse lie.

/// One keystroke, as the dialog's state machine understands it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A character to insert. Already shift-resolved.
    Char(char),
    Backspace,
    /// Return or the keypad's Enter — "do the default thing".
    Enter,
    Escape,
    /// Move focus forward, or backward when shift was held.
    Tab { back: bool },
    /// Toggle passphrase visibility without reaching for the mouse.
    ToggleReveal,
}

// The keysyms this dialog names, from <X11/keysymdef.h>.
const XK_BACKSPACE: u32 = 0xff08;
const XK_TAB: u32 = 0xff09;
const XK_RETURN: u32 = 0xff0d;
const XK_ESCAPE: u32 = 0xff1b;
const XK_KP_ENTER: u32 = 0xff8d;
const XK_ISO_LEFT_TAB: u32 = 0xfe20;

/// The keysym a keycode's mapping produces at the current shift level.
///
/// X hands back a list per keycode; index 0 is unshifted and index 1 is
/// shifted. A one-entry list (some keypad and media keys) has no
/// shifted form, so shift is ignored rather than turned into a missing
/// key — the same fallback X's own `XLookupKeysym` makes.
pub fn level(keysyms: &[u32], shift: bool) -> Option<u32> {
    if shift {
        keysyms.get(1).copied().filter(|&k| k != 0).or_else(|| keysyms.first().copied())
    } else {
        keysyms.first().copied()
    }
    .filter(|&k| k != 0)
}

/// Reduces one keysym to a [`Key`], or `None` for the many keys a
/// passphrase field has no opinion about (function keys, arrows, bare
/// modifiers).
///
/// `ctrl` is taken so that a control chord can be *rejected* rather
/// than silently typed: X reports `Ctrl+U` as the `u` keysym with the
/// control bit in the modifier state, so a field that ignored the bit
/// would put a `u` in the passphrase when the person meant "clear the
/// line". The one chord this dialog claims is `Ctrl+R`, for revealing
/// the passphrase without the mouse.
pub fn key_from(keysym: u32, shift: bool, ctrl: bool) -> Option<Key> {
    match keysym {
        XK_BACKSPACE => return Some(Key::Backspace),
        XK_RETURN | XK_KP_ENTER => return Some(Key::Enter),
        XK_ESCAPE => return Some(Key::Escape),
        XK_TAB => return Some(Key::Tab { back: shift }),
        XK_ISO_LEFT_TAB => return Some(Key::Tab { back: true }),
        _ => {}
    }
    let ch = keysym_char(keysym)?;
    if ctrl {
        return (ch.eq_ignore_ascii_case(&'r')).then_some(Key::ToggleReveal);
    }
    Some(Key::Char(ch))
}

/// The character a text keysym stands for, if it stands for one.
///
/// Two ranges cover everything this needs: Latin-1 keysyms are their
/// own codepoint by definition, and every other Unicode character
/// arrives as `0x01000000 + codepoint`. C0/C1 control characters are
/// excluded — a keysym in those ranges is a key that happens to sit at
/// a control codepoint, not a character anyone meant to type.
fn keysym_char(keysym: u32) -> Option<char> {
    let codepoint = match keysym {
        0x20..=0x7E | 0xA0..=0xFF => keysym,
        0x0100_0000..=0x0110_FFFF => keysym - 0x0100_0000,
        _ => return None,
    };
    char::from_u32(codepoint).filter(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_selects_the_second_level_and_falls_back_when_there_is_none() {
        // `2` / `@` on a US layout.
        assert_eq!(level(&[0x32, 0x40], false), Some(0x32));
        assert_eq!(level(&[0x32, 0x40], true), Some(0x40));
        // A key with only one level stays itself under shift.
        assert_eq!(level(&[0xff0d], true), Some(0xff0d));
        // A hole in the mapping is not a keystroke.
        assert_eq!(level(&[0x61, 0], true), Some(0x61), "an empty shift slot falls back, it does not vanish");
        assert_eq!(level(&[0], false), None);
        assert_eq!(level(&[], false), None);
    }

    #[test]
    fn latin1_and_unicode_keysyms_become_their_characters() {
        assert_eq!(key_from(0x61, false, false), Some(Key::Char('a')));
        assert_eq!(key_from(0x41, true, false), Some(Key::Char('A')));
        assert_eq!(key_from(0x20, false, false), Some(Key::Char(' ')), "a space is a legal passphrase character");
        assert_eq!(key_from(0xe9, false, false), Some(Key::Char('é')), "latin-1 keysyms are their own codepoint");
        assert_eq!(key_from(0x0100_20AC, false, false), Some(Key::Char('€')), "unicode keysyms are offset by 0x01000000");
    }

    #[test]
    fn the_named_keys_are_named() {
        assert_eq!(key_from(XK_BACKSPACE, false, false), Some(Key::Backspace));
        assert_eq!(key_from(XK_RETURN, false, false), Some(Key::Enter));
        assert_eq!(key_from(XK_KP_ENTER, false, false), Some(Key::Enter), "the keypad's enter is an enter");
        assert_eq!(key_from(XK_ESCAPE, false, false), Some(Key::Escape));
        assert_eq!(key_from(XK_TAB, false, false), Some(Key::Tab { back: false }));
        assert_eq!(key_from(XK_TAB, true, false), Some(Key::Tab { back: true }));
        assert_eq!(key_from(XK_ISO_LEFT_TAB, false, false), Some(Key::Tab { back: true }), "shift+tab arrives as its own keysym");
    }

    #[test]
    fn a_control_chord_is_never_typed_into_the_passphrase() {
        assert_eq!(key_from(0x75, false, true), None, "Ctrl+U must not put a 'u' in the field");
        assert_eq!(key_from(0x63, false, true), None, "nor Ctrl+C a 'c'");
        assert_eq!(key_from(0x72, false, true), Some(Key::ToggleReveal), "Ctrl+R is the one chord this dialog claims");
        assert_eq!(key_from(0x52, true, true), Some(Key::ToggleReveal), "and it is case-insensitive");
    }

    #[test]
    fn keys_with_no_text_meaning_are_ignored_rather_than_guessed() {
        for keysym in [0xffbe /* F1 */, 0xff51 /* Left */, 0xffe1 /* Shift_L */, 0xffe9 /* Alt_L */, 0xff7f /* Num_Lock */] {
            assert_eq!(key_from(keysym, false, false), None, "keysym {keysym:#x} must not type anything");
        }
        assert_eq!(key_from(0x0100_0007, false, false), None, "a control codepoint is not a character");
    }
}
